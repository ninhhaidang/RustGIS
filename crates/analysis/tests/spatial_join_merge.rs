//! Spatial Join's merge rules on whole numbers past 2^53 and near the 64-bit limits, through the
//! tool, on layers read by the real GeoJSON reader: `min` and `max` pick exactly whatever the
//! matches' order, an integer `sum` past the 64-bit range is empty and said so, and a whole `mean`
//! a decimal can't hold is said to be rounded.
use rustgis_analysis::{Inputs, Output, ToolResult};
use rustgis_data::io::read_geojson;
use rustgis_data::{Dataset, FieldType, Value};
use serde_json::{Value as Json, json};
use std::collections::HashMap;

#[derive(Default)]
struct Layers {
    data: HashMap<String, Dataset>,
    selected: HashMap<String, Vec<usize>>,
}
impl Inputs for Layers {
    fn dataset(&self, name: &str) -> Option<&Dataset> {
        self.data.get(name)
    }
    fn selection(&self, name: &str) -> &[usize] {
        self.selected.get(name).map(Vec::as_slice).unwrap_or(&[])
    }
}
impl Layers {
    fn with(mut self, ds: Dataset) -> Self {
        self.data.insert(ds.name.clone(), ds);
        self
    }
    fn select(mut self, layer: &str, rows: &[usize]) -> Self {
        self.selected.insert(layer.to_string(), rows.to_vec());
        self
    }
}

const TWO_53: i64 = 1 << 53;

fn field(ds: &Dataset, name: &str) -> usize {
    ds.fields.iter().position(|f| f.name == name).unwrap_or_else(|| panic!("no field {name}"))
}
fn column(ds: &Dataset, name: &str) -> Vec<Value> {
    let i = field(ds, name);
    ds.features.iter().map(|f| f.attrs[i].clone()).collect()
}
fn layer(r: ToolResult) -> Dataset {
    match r.output {
        Output::Layer(d) => d,
        _ => panic!("a layer"),
    }
}
fn square(x: f64) -> Json {
    json!({"type": "Polygon", "coordinates": [[[x, 0.0], [x + 1.0, 0.0], [x + 1.0, 1.0], [x, 1.0], [x, 0.0]]]})
}
fn collection(features: Vec<(Json, Json)>) -> String {
    let features: Vec<Json> = features
        .into_iter()
        .map(|(geometry, properties)| json!({"type": "Feature", "geometry": geometry, "properties": properties}))
        .collect();
    json!({"type": "FeatureCollection", "features": features}).to_string()
}

/// Four zones, A to D, side by side.
fn zones() -> Dataset {
    let text =
        collection(["A", "B", "C", "D"].iter().enumerate().map(|(i, name)| (square(i as f64 * 2.0), json!({"name": name}))).collect());
    read_geojson(&text, "zones").unwrap().0
}

/// Two points in each zone, in this order. `n`: 2^53 and 2^53 + 1 in A, the other way round in B,
/// 2^53 + 1 twice in C, 1 and 3 in D. `m`: i64::MAX and 1 in A (a sum past the range, a mean of
/// 2^62), 5 and 6 in B, i64::MIN and -1 in C (a sum past the range), 2 and 4 in D. `d`, a decimal
/// field: 2^53 + 2 twice in A, 2^53 + 2 and 2^53 + 4 in C (a whole mean, 2^53 + 3, a decimal can't
/// hold), halves elsewhere.
fn points() -> Dataset {
    let rows: [(f64, i64, i64, f64); 8] = [
        (0.2, TWO_53, i64::MAX, 9_007_199_254_740_994.0),
        (0.6, TWO_53 + 1, 1, 9_007_199_254_740_994.0),
        (2.2, TWO_53 + 1, 5, 0.5),
        (2.6, TWO_53, 6, 1.5),
        (4.2, TWO_53 + 1, i64::MIN, 9_007_199_254_740_994.0),
        (4.6, TWO_53 + 1, -1, 9_007_199_254_740_996.0),
        (6.2, 1, 2, 4.5),
        (6.6, 3, 4, 5.5),
    ];
    let text = collection(
        rows.iter().map(|&(x, n, m, d)| (json!({"type": "Point", "coordinates": [x, 0.5]}), json!({"n": n, "m": m, "d": d}))).collect(),
    );
    let (ds, _) = read_geojson(&text, "points").unwrap();
    for (f, ty) in [("n", FieldType::Integer), ("m", FieldType::Integer), ("d", FieldType::Double)] {
        assert_eq!(ds.fields[field(&ds, f)].ty, ty, "premise: {f} reads as {ty:?}");
    }
    assert_eq!(column(&ds, "n")[1], Value::Int(TWO_53 + 1), "premise: read exactly");
    assert_eq!(column(&ds, "m")[0], Value::Int(i64::MAX), "premise: read exactly");
    ds
}

fn join(layers: &Layers, merge: Json) -> ToolResult {
    let p = json!({"target": "zones", "join": "points", "merge": merge});
    rustgis_analysis::run("spatialJoin", layers, &p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

const JOINED: &str = "Spatial Join: 4 features joined (8 matches)";

fn rounded(n: &str, fields: &str) -> String {
    format!("{n} rounded in {fields}: a decimal field holds whole numbers exactly only up to 2^53 (9007199254740992)")
}

#[test]
fn min_and_max_pick_whole_numbers_exactly_in_any_order() {
    let layers = Layers::default().with(zones()).with(points());
    let res = join(&layers, json!([{"field": "n", "rule": "max"}, {"field": "m", "rule": "max"}]));
    assert_eq!(res.messages, [JOINED]);
    let out = layer(res);
    let big = Value::Int(TWO_53 + 1);
    assert_eq!(column(&out, "join_n"), [big.clone(), big.clone(), big.clone(), Value::Int(3)]);
    assert_eq!(column(&out, "join_m"), [Value::Int(i64::MAX), Value::Int(6), Value::Int(-1), Value::Int(4)]);
    let res = join(&layers, json!([{"field": "n", "rule": "min"}, {"field": "m", "rule": "min"}]));
    assert_eq!(res.messages, [JOINED]);
    let out = layer(res);
    let low = Value::Int(TWO_53);
    assert_eq!(column(&out, "join_n"), [low.clone(), low, big, Value::Int(1)]);
    assert_eq!(column(&out, "join_m"), [Value::Int(1), Value::Int(5), Value::Int(i64::MIN), Value::Int(2)]);

    // Whole numbers written as text in the integer field are compared as the numbers they are, and
    // kept as written; a decimal among them is compared exactly too.
    let mut pts = points();
    let n = field(&pts, "n");
    pts.features[2].attrs[n] = Value::Text("9007199254740993".into());
    pts.features[3].attrs[n] = Value::Text(" 9007199254740992 ".into());
    pts.features[6].attrs[n] = Value::Float(2.5);
    let layers = Layers::default().with(zones()).with(pts);
    let out = layer(join(&layers, json!([{"field": "n", "rule": "max"}])));
    assert_eq!(column(&out, "join_n")[1..], [Value::Text("9007199254740993".into()), Value::Int(TWO_53 + 1), Value::Int(3)]);
    let out = layer(join(&layers, json!([{"field": "n", "rule": "min"}])));
    assert_eq!(column(&out, "join_n")[1..], [Value::Text(" 9007199254740992 ".into()), Value::Int(TWO_53 + 1), Value::Float(2.5)]);
    // Text written with a decimal point or an exponent is read from its digits too: 2^53 + 1 as
    // "9007199254740993.0", after 2^53, is the max, though their decimals are equal.
    let mut pts = points();
    let n = field(&pts, "n");
    pts.features[1].attrs[n] = Value::Text("9007199254740993.0".into());
    pts.features[2].attrs[n] = Value::Text("90071992547409930e-1".into());
    pts.features[3].attrs[n] = Value::Int(TWO_53 + 2);
    let layers = Layers::default().with(zones()).with(pts);
    let out = layer(join(&layers, json!([{"field": "n", "rule": "max"}])));
    assert_eq!(column(&out, "join_n")[..2], [Value::Text("9007199254740993.0".into()), Value::Int(TWO_53 + 2)]);
    let out = layer(join(&layers, json!([{"field": "n", "rule": "min"}])));
    assert_eq!(column(&out, "join_n")[..2], [Value::Int(TWO_53), Value::Text("90071992547409930e-1".into())]);
}

#[test]
fn an_integer_sum_past_the_64_bit_range_is_empty_and_said_so() {
    let layers = Layers::default().with(zones()).with(points());
    let res = join(&layers, json!([{"field": "n", "rule": "sum"}, {"field": "m", "rule": "sum"}]));
    assert_eq!(res.messages, [JOINED, "2 sums in join_m are past the 64-bit integer range and were left empty"]);
    let out = layer(res);
    assert_eq!(column(&out, "join_m"), [Value::Null, Value::Int(11), Value::Null, Value::Int(6)]);
    // Sums past 2^53 that fit 64 bits are exact.
    assert_eq!(column(&out, "join_n")[..3], [Value::Int(2 * TWO_53 + 1), Value::Int(2 * TWO_53 + 1), Value::Int(2 * TWO_53 + 2)]);
    // One: zone A alone.
    let only_a = Layers::default().with(zones()).with(points()).select("points", &[0, 1]);
    let p = json!({"target": "zones", "join": "points", "merge": [{"field": "m", "rule": "sum"}]});
    let res = rustgis_analysis::run("spatialJoin", &only_a, &p).unwrap();
    assert_eq!(
        res.messages,
        ["Spatial Join: 4 features joined (2 matches)", "1 sum in join_m is past the 64-bit integer range and was left empty"]
    );
    // Within the range, and the other rules: no notice.
    let clean = Layers::default().with(zones()).with(points()).select("points", &[2, 3, 6, 7]);
    let p = json!({"target": "zones", "join": "points", "merge": [{"field": "m", "rule": "sum"}, {"field": "n", "rule": "sum"}]});
    assert_eq!(rustgis_analysis::run("spatialJoin", &clean, &p).unwrap().messages, ["Spatial Join: 4 features joined (4 matches)"]);
    // Whole numbers written as text in the integer field are added exactly, not as decimals.
    let mut pts = points();
    let m = field(&pts, "m");
    pts.features[6].attrs[m] = Value::Text("9007199254740993".into());
    let layers = Layers::default().with(zones()).with(pts);
    let out = layer(join(&layers, json!([{"field": "m", "rule": "sum"}])));
    assert_eq!(column(&out, "join_m")[3], Value::Int(TWO_53 + 5));
    // In any form a number is written: "9007199254740993.0" is 2^53 + 1, not its decimal, 2^53.
    let mut pts = points();
    pts.features[6].attrs[m] = Value::Text("9007199254740993.0".into());
    let layers = Layers::default().with(zones()).with(pts);
    let out = layer(join(&layers, json!([{"field": "m", "rule": "sum"}])));
    assert_eq!(column(&out, "join_m")[3], Value::Int(TWO_53 + 5));
}

#[test]
fn an_integer_sum_is_checked_once_and_says_what_it_leaves_out() {
    let mut pts = points();
    let m = field(&pts, "m");
    // A: i64::MAX, 1 and -1. The total, i64::MAX, fits, though i64::MAX + 1 doesn't.
    let mut third = pts.features[1].clone();
    third.attrs[m] = Value::Int(-1);
    pts.push(third.geometry, third.attrs);
    // B: a decimal with a fraction and a whole number past the range written as text, which the
    // field's whole numbers can't hold: both are left out, and the sum is empty. 2^63 as a decimal
    // is past it too (the unit test in `join.rs`).
    pts.features[2].attrs[m] = Value::Float(2.5);
    pts.features[3].attrs[m] = Value::Text("9223372036854775808".into());
    // D: "1e309", a number past the range written as text, too large for a decimal, is left out of
    // 4. Text that isn't a number is no value, as for every numeric rule, and isn't counted.
    pts.features[6].attrs[m] = Value::Text("1e309".into());
    let mut fourth = pts.features[7].clone();
    fourth.attrs[m] = Value::Text("n/a".into());
    pts.push(fourth.geometry, fourth.attrs);
    let layers = Layers::default().with(zones()).with(pts);
    let res = join(&layers, json!([{"field": "n", "rule": "first"}, {"field": "m", "rule": "sum"}]));
    assert_eq!(
        res.messages,
        [
            "Spatial Join: 4 features joined (10 matches)",
            "1 sum in join_m is past the 64-bit integer range and was left empty",
            "3 matched numbers in join_m were left out: a whole-number sum takes whole numbers in the 64-bit range, the other rules numbers a decimal holds"
        ]
    );
    assert_eq!(column(&layer(res), "join_m"), [Value::Int(i64::MAX), Value::Null, Value::Null, Value::Int(4)]);
    // One left out: D alone.
    let layers = layers.select("points", &[6, 7]);
    let p = json!({"target": "zones", "join": "points", "merge": [{"field": "m", "rule": "sum"}]});
    assert_eq!(
        rustgis_analysis::run("spatialJoin", &layers, &p).unwrap().messages,
        [
            "Spatial Join: 4 features joined (2 matches)",
            "1 matched number in join_m was left out: a whole-number sum takes whole numbers in the 64-bit range, the other rules numbers a decimal holds"
        ]
    );
}

#[test]
fn a_whole_mean_a_decimal_cant_hold_is_said_to_be_rounded() {
    let layers = Layers::default().with(zones()).with(points());
    // C's mean of n, 2^53 + 1, is whole and rounded; A's and B's, 2^53 + 0.5, are decimals anyway.
    let res = join(&layers, json!([{"field": "d", "rule": "mean"}, {"field": "m", "rule": "mean"}, {"field": "n", "rule": "mean"}]));
    assert_eq!(res.messages, [JOINED.to_string(), rounded("1 result", "join_n")]);
    let out = layer(res);
    let two_53 = Value::Float(9_007_199_254_740_992.0);
    assert_eq!(column(&out, "join_n"), [two_53.clone(), two_53.clone(), two_53, Value::Float(2.0)]);
    // m's means come from the exact sum: A's, (2^63 - 1 + 1) / 2, is 2^62, which a decimal holds;
    // C's, (-2^63 - 1) / 2, is a decimal.
    assert_eq!(
        column(&out, "join_m"),
        [Value::Float(4_611_686_018_427_387_904.0), Value::Float(5.5), Value::Float(-4_611_686_018_427_387_904.0), Value::Float(3.0)]
    );
    // A decimal field's mean is a decimal, worked out as decimals are, however large: no notice.
    assert_eq!(column(&out, "join_d")[0], Value::Float(9_007_199_254_740_994.0));
    assert_eq!(column(&out, "join_d")[2], Value::Float(9_007_199_254_740_996.0));
    // Two rounded means: A's, with 2^53 + 3 twice, and C's.
    let mut pts = points();
    let n = field(&pts, "n");
    for f in &mut pts.features[..2] {
        f.attrs[n] = Value::Int(TWO_53 + 3);
    }
    let layers = Layers::default().with(zones()).with(pts);
    let res = join(&layers, json!([{"field": "n", "rule": "mean"}]));
    assert_eq!(res.messages, [JOINED.to_string(), rounded("2 results", "join_n")]);
    // B's mean of 2^53 + 1 and 2^53 + 2, 2^53 + 1.5, isn't whole: no notice, though its decimal is
    // a whole number. C's of 2^53 + 1 three times is rounded once, to 2^53, not from a rounded sum
    // (3 * 2^53 + 4, over 3, would give 2^53 + 2).
    let mut pts = points();
    let n = field(&pts, "n");
    pts.features[2].attrs[n] = Value::Int(TWO_53 + 1);
    pts.features[3].attrs[n] = Value::Int(TWO_53 + 2);
    let third = pts.features[4].clone();
    pts.push(third.geometry.clone(), third.attrs.clone());
    // Text that isn't a number is no value, and leaves C's mean exact.
    let mut fourth = third;
    fourth.attrs[n] = Value::Text("n/a".into());
    pts.push(fourth.geometry, fourth.attrs);
    let layers = Layers::default().with(zones()).with(pts);
    let res = join(&layers, json!([{"field": "n", "rule": "mean"}]));
    assert_eq!(res.messages, ["Spatial Join: 4 features joined (10 matches)".to_string(), rounded("1 result", "join_n")]);
    let out = layer(res);
    assert_eq!(column(&out, "join_n")[1..3], [Value::Float(9_007_199_254_740_994.0), Value::Float(9_007_199_254_740_992.0)]);
    // Means within 2^53: no notice.
    let clean = Layers::default().with(zones()).with(points()).select("points", &[6, 7]);
    let p = json!({"target": "zones", "join": "points", "merge": [{"field": "n", "rule": "mean"}, {"field": "m", "rule": "mean"}]});
    assert_eq!(rustgis_analysis::run("spatialJoin", &clean, &p).unwrap().messages, ["Spatial Join: 4 features joined (2 matches)"]);
}

/// A number too large for a decimal, written as text ("1e309"), is a number past every range: `mean`,
/// a decimal field's `sum`, `min` and `max` leave it out and say so, as an integer `sum` does.
#[test]
fn a_number_too_large_for_a_decimal_is_left_out_and_counted() {
    let mut pts = points();
    let (n, d) = (field(&pts, "n"), field(&pts, "d"));
    pts.features[6].attrs[n] = Value::Text("1e309".into());
    pts.features[7].attrs[d] = Value::Text("-1E400".into());
    let layers = Layers::default().with(zones()).with(pts);
    let notice = |n: &str, fields: &str| {
        format!(
            "{n} in {fields} were left out: a whole-number sum takes whole numbers in the 64-bit range, the other rules numbers a decimal holds"
        )
    };
    let res = join(&layers, json!([{"field": "n", "rule": "mean"}, {"field": "d", "rule": "sum"}]));
    // D's mean of n leaves out "1e309", and its sum of d "-1E400"; C's mean of n is still rounded.
    assert_eq!(res.messages, [JOINED.to_string(), notice("2 matched numbers", "join_n, join_d"), rounded("1 result", "join_n")]);
    let out = layer(res);
    assert_eq!((column(&out, "join_n")[3].clone(), column(&out, "join_d")[3].clone()), (Value::Float(3.0), Value::Float(4.5)));
    for (n_rule, d_rule) in [("max", "min"), ("min", "max")] {
        let res = join(&layers, json!([{"field": "n", "rule": n_rule}, {"field": "d", "rule": d_rule}]));
        assert_eq!(res.messages, [JOINED.to_string(), notice("2 matched numbers", "join_n, join_d")], "{n_rule}");
        let out = layer(res);
        assert_eq!((column(&out, "join_n")[3].clone(), column(&out, "join_d")[3].clone()), (Value::Int(3), Value::Float(4.5)));
    }
}
