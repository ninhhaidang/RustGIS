//! Summary fields on Dissolve and Summarize Within: the statistics of Summary Statistics, computed
//! over each dissolved group's records and over the points in each polygon, through the tools on
//! layers read by the real GeoJSON reader.
use rustgis_analysis::{Inputs, Output, ToolResult};
use rustgis_data::io::{read_csv, read_geojson};
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

fn field_ty(ds: &Dataset, name: &str) -> FieldType {
    ds.fields.iter().find(|f| f.name == name).map(|f| f.ty).unwrap_or_else(|| panic!("no field {name}"))
}
fn column<'a>(ds: &'a Dataset, name: &str) -> Vec<&'a Value> {
    let i = ds.fields.iter().position(|f| f.name == name).unwrap_or_else(|| panic!("no field {name}"));
    ds.features.iter().map(|f| &f.attrs[i]).collect()
}
fn names(ds: &Dataset) -> Vec<&str> {
    ds.fields.iter().map(|f| f.name.as_str()).collect()
}
fn layer(r: ToolResult) -> Dataset {
    match r.output {
        Output::Layer(d) => d,
        _ => panic!("a layer"),
    }
}
fn run(tool: &str, layers: &Layers, p: Json) -> Dataset {
    layer(rustgis_analysis::run(tool, layers, &p).unwrap_or_else(|e| panic!("{tool} {p}: {e}")))
}
fn error(tool: &str, layers: &Layers, p: Json) -> String {
    match rustgis_analysis::run(tool, layers, &p) {
        Ok(_) => panic!("{tool} {p} should fail"),
        Err(e) => e,
    }
}
fn int(i: i64) -> Value {
    Value::Int(i)
}
fn float(x: f64) -> Value {
    Value::Float(x)
}
fn text(t: &str) -> Value {
    Value::Text(t.into())
}

/// A square of side `s` with its south-west corner at (`x`, `y`), as GeoJSON.
fn square(x: f64, y: f64, s: f64) -> Json {
    json!({"type": "Polygon", "coordinates": [[[x, y], [x + s, y], [x + s, y + s], [x, y + s], [x, y]]]})
}
fn point(x: f64, y: f64) -> Json {
    json!({"type": "Point", "coordinates": [x, y]})
}
fn collection(features: Vec<(Json, Json)>) -> String {
    let features: Vec<Json> = features
        .into_iter()
        .map(|(geometry, properties)| json!({"type": "Feature", "geometry": geometry, "properties": properties}))
        .collect();
    json!({"type": "FeatureCollection", "features": features}).to_string()
}

/// Parcels in record order. Zone A: records 0, 2, 7, and 4, which has no geometry; zone B: 1 and
/// 5; no zone: 3 and 6. `pop` and `owner` have nulls.
fn parcels() -> Dataset {
    let text = collection(vec![
        (square(0.0, 0.0, 1.0), json!({"zone": "A", "pop": 10, "owner": "ann"})),
        (square(5.0, 0.0, 1.0), json!({"zone": "B", "pop": null, "owner": "bob"})),
        (square(1.0, 0.0, 1.0), json!({"zone": "A", "pop": 30, "owner": null})),
        (square(10.0, 0.0, 1.0), json!({"zone": null, "pop": 7, "owner": "cy"})),
        (Json::Null, json!({"zone": "A", "pop": 20, "owner": "dee"})),
        (square(6.0, 0.0, 1.0), json!({"zone": "B", "pop": 5, "owner": "eve"})),
        (square(12.0, 0.0, 1.0), json!({"zone": null, "pop": 3, "owner": "fay"})),
        (square(2.0, 0.0, 1.0), json!({"zone": "A", "pop": 60, "owner": "gus"})),
    ]);
    let (ds, _) = read_geojson(&text, "parcels").unwrap();
    assert_eq!(names(&ds), ["owner", "pop", "zone"], "premise: the fields in the text's order, which `json!` sorts by name");
    assert_eq!(field_ty(&ds, "zone"), FieldType::Text);
    assert_eq!(field_ty(&ds, "pop"), FieldType::Integer, "premise: whole numbers read as Integer");
    assert_eq!(field_ty(&ds, "owner"), FieldType::Text);
    assert_eq!(ds.features.len(), 8, "premise: a feature without geometry is kept");
    assert!(ds.features[4].geometry.is_none());
    assert_eq!(ds.value(1, 1), &Value::Null);
    assert_eq!(ds.value(3, 2), &Value::Null);
    ds
}

/// Statistics of numbers and text, nulls included.
fn parcel_statistics() -> Json {
    json!([
        {"field": "pop", "statistic": "SUM"},
        {"field": "pop", "statistic": "MEAN"},
        {"field": "pop", "statistic": "MIN"},
        {"field": "pop", "statistic": "MAX"},
        {"field": "pop", "statistic": "MEDIAN"},
        {"field": "pop", "statistic": "COUNT"},
        {"field": "pop", "statistic": "NULLS"},
        {"field": "pop", "statistic": "FIRST"},
        {"field": "owner", "statistic": "FIRST"},
        {"field": "owner", "statistic": "LAST"},
        {"field": "owner", "statistic": "CONCATENATE"},
        {"field": "owner", "statistic": "COUNT"},
        {"field": "owner", "statistic": "NULLS"},
    ])
}

const PARCEL_FIELDS: [&str; 13] = [
    "SUM_pop",
    "MEAN_pop",
    "MIN_pop",
    "MAX_pop",
    "MEDIAN_pop",
    "COUNT_pop",
    "NULLS_pop",
    "FIRST_pop",
    "FIRST_owner",
    "LAST_owner",
    "CONCATENATE_owner",
    "COUNT_owner",
    "NULLS_owner",
];

/// Each group's statistics row, in [`PARCEL_FIELDS`] order.
fn stats_row(ds: &Dataset, row: usize) -> Vec<Value> {
    PARCEL_FIELDS.iter().map(|n| column(ds, n)[row].clone()).collect()
}

#[test]
fn dissolve_adds_statistics_over_each_groups_merged_records() {
    let layers = Layers::default().with(parcels());
    let out = run("dissolve", &layers, json!({"input": "parcels", "field": "zone", "statistics": parcel_statistics()}));
    let mut expected = vec!["zone", "count"];
    expected.extend(PARCEL_FIELDS);
    assert_eq!(names(&out), expected);
    let types: Vec<FieldType> = PARCEL_FIELDS.iter().map(|n| field_ty(&out, n)).collect();
    use FieldType::{Double as D, Integer as I, Text as T};
    assert_eq!(types, [D, D, D, D, D, I, I, D, T, T, T, I, I]);
    // Groups in the order they first appear, as without statistics; the null zone is a group.
    assert_eq!(column(&out, "zone"), [&text("A"), &text("B"), &Value::Null]);
    assert_eq!(column(&out, "count"), [&int(3), &int(2), &int(2)]);
    // Zone A is records 0, 2 and 7: record 4 has no polygon, so it isn't merged or counted.
    assert_eq!(
        stats_row(&out, 0),
        [
            float(100.0),
            float(100.0 / 3.0),
            float(10.0),
            float(60.0),
            float(30.0),
            int(3),
            int(0),
            float(10.0),
            text("ann"),
            text("gus"),
            text("ann, gus"),
            int(2),
            int(1)
        ]
    );
    assert_eq!(
        stats_row(&out, 1),
        [
            float(5.0),
            float(5.0),
            float(5.0),
            float(5.0),
            float(5.0),
            int(1),
            int(1),
            float(5.0),
            text("bob"),
            text("eve"),
            text("bob, eve"),
            int(2),
            int(0)
        ]
    );
    assert_eq!(
        stats_row(&out, 2),
        [
            float(10.0),
            float(5.0),
            float(3.0),
            float(7.0),
            float(5.0),
            int(2),
            int(0),
            float(7.0),
            text("cy"),
            text("fay"),
            text("cy, fay"),
            int(2),
            int(0)
        ]
    );
}

#[test]
fn dissolve_without_a_field_summarises_every_merged_record() {
    let layers = Layers::default().with(parcels());
    let p = json!({"input": "parcels", "statistics": parcel_statistics(), "separator": " | "});
    let out = run("dissolve", &layers, p);
    let mut expected = vec!["count"];
    expected.extend(PARCEL_FIELDS);
    assert_eq!(names(&out), expected);
    assert_eq!(column(&out, "count"), [&int(7)]);
    // Every record but 4 (no polygon): pop 10, null, 30, 7, 5, 3, 60.
    assert_eq!(
        stats_row(&out, 0),
        [
            float(115.0),
            float(115.0 / 6.0),
            float(3.0),
            float(60.0),
            float(8.5),
            int(6),
            int(1),
            float(10.0),
            text("ann"),
            text("gus"),
            text("ann | bob | cy | eve | fay | gus"),
            int(6),
            int(1)
        ]
    );
}

#[test]
fn dissolve_statistics_follow_record_order_and_the_selection() {
    // Out of order: groups still appear in the selection's order, as without statistics, but
    // FIRST, LAST and CONCATENATE follow record order. Zone A's record 4 has no polygon.
    let layers = Layers::default().with(parcels()).select("parcels", &[7, 5, 4, 2, 0, 1]);
    let out = run("dissolve", &layers, json!({"input": "parcels", "field": "zone", "statistics": parcel_statistics()}));
    let plain = run("dissolve", &layers, json!({"input": "parcels", "field": "zone"}));
    assert_eq!(column(&out, "zone"), column(&plain, "zone"));
    assert_eq!(column(&out, "zone"), [&text("A"), &text("B")], "the unselected null zone is left out");
    assert_eq!(column(&out, "count"), [&int(3), &int(2)]);
    assert_eq!(column(&out, "FIRST_owner"), [&text("ann"), &text("bob")]);
    assert_eq!(column(&out, "LAST_owner"), [&text("gus"), &text("eve")]);
    assert_eq!(column(&out, "FIRST_pop"), [&float(10.0), &float(5.0)]);
    assert_eq!(column(&out, "CONCATENATE_owner"), [&text("ann, gus"), &text("bob, eve")]);
    // Only the selected records of a group count.
    let layers = Layers::default().with(parcels()).select("parcels", &[7, 0]);
    let out = run("dissolve", &layers, json!({"input": "parcels", "field": "zone", "statistics": parcel_statistics()}));
    assert_eq!(column(&out, "count"), [&int(2)]);
    assert_eq!(column(&out, "SUM_pop"), [&float(70.0)]);
    assert_eq!(column(&out, "MEDIAN_pop"), [&float(35.0)]);
    assert_eq!(column(&out, "CONCATENATE_owner"), [&text("ann, gus")]);
    assert_eq!(column(&out, "NULLS_owner"), [&int(0)]);
}

#[test]
fn dissolve_without_statistics_is_unchanged() {
    let layers = Layers::default().with(parcels());
    for field in [None, Some("zone")] {
        let mut base = json!({"input": "parcels"});
        if let Some(f) = field {
            base["field"] = json!(f);
        }
        let res = rustgis_analysis::run("dissolve", &layers, &base).unwrap();
        let messages = res.messages.clone();
        let plain = layer(res);
        let expected: &[&str] = if field.is_some() { &["zone", "count"] } else { &["count"] };
        assert_eq!(names(&plain), expected);
        for empty in [json!([]), json!(""), json!("  "), Json::Null] {
            let mut p = base.clone();
            p["statistics"] = empty.clone();
            // A separator isn't read without statistics.
            p["separator"] = json!(5);
            let res = rustgis_analysis::run("dissolve", &layers, &p).unwrap();
            assert_eq!(res.messages, messages, "{empty}");
            assert_eq!(layer(res), plain, "{empty}");
        }
        // Statistics add fields after the others and leave them as they were.
        let mut p = base.clone();
        p["statistics"] = json!([{"field": "pop", "statistic": "SUM"}]);
        let res = rustgis_analysis::run("dissolve", &layers, &p).unwrap();
        assert_eq!(res.messages, messages);
        let with = layer(res);
        assert_eq!(with.fields[..plain.fields.len()], plain.fields[..]);
        for (a, b) in with.features.iter().zip(plain.features.iter()) {
            assert_eq!(a.geometry, b.geometry);
            assert_eq!(a.attrs[..b.attrs.len()], b.attrs[..]);
            assert_eq!(a.attrs.len(), b.attrs.len() + 1);
        }
    }
}

#[test]
fn statistics_read_from_json_text_and_lists_alike() {
    let layers = Layers::default().with(parcels());
    let list = parcel_statistics();
    let from_list = run("dissolve", &layers, json!({"input": "parcels", "field": "zone", "statistics": list}));
    let from_text = run("dissolve", &layers, json!({"input": "parcels", "field": "zone", "statistics": list.to_string()}));
    assert_eq!(from_list, from_text);
    // Field and statistic names in any case; the output field is named by the field's own name.
    let loose = json!([{"field": "POP", "statistic": " sum "}]);
    let out = run("dissolve", &layers, json!({"input": "parcels", "field": "zone", "statistics": loose}));
    assert_eq!(names(&out), ["zone", "count", "SUM_pop"]);
}

#[test]
fn dissolve_refuses_bad_statistics() {
    let layers = Layers::default().with(parcels());
    let bad = |statistics: Json| error("dissolve", &layers, json!({"input": "parcels", "field": "zone", "statistics": statistics}));
    assert_eq!(bad(json!([{"field": "owner", "statistic": "SUM"}])), "SUM needs a numeric field ('owner' is not)");
    assert_eq!(bad(json!([{"field": "owner", "statistic": "MEDIAN"}])), "MEDIAN needs a numeric field ('owner' is not)");
    assert_eq!(bad(json!([{"field": "nope", "statistic": "SUM"}])), "no field named 'nope'");
    assert!(bad(json!([{"field": "pop", "statistic": "TOTAL"}])).starts_with("the statistic for 'pop' must be one of COUNT, SUM"));
    assert_eq!(bad(json!([{"field": "pop", "statistic": "SUM"}, {"field": "POP", "statistic": "sum"}])), "SUM of 'POP' is listed twice");
    assert!(bad(json!({"field": "pop", "statistic": "SUM"})).starts_with("Statistics must be a list"));
    assert!(bad(json!("[{\"field\": ")).starts_with("Statistics: "));
    let long = "x".repeat(257);
    let p = json!({"input": "parcels", "statistics": [{"field": "owner", "statistic": "CONCATENATE"}], "separator": long});
    assert_eq!(error("dissolve", &layers, p), "Separator can be at most 256 bytes");
}

#[test]
fn dissolve_refuses_summary_field_names_that_clash() {
    // A dissolve field named as a statistic's field would be, in another case.
    let text = collection(vec![(square(0.0, 0.0, 1.0), json!({"sum_POP": "x", "pop": 1, "POP": 2.5}))]);
    let (ds, _) = read_geojson(&text, "clash").unwrap();
    assert_eq!(names(&ds), ["POP", "pop", "sum_POP"], "premise: the reader keeps names differing only in case");
    let layers = Layers::default().with(ds);
    let p = |field: &str, statistics: Json| json!({"input": "clash", "field": field, "statistics": statistics});
    let err = error("dissolve", &layers, p("sum_POP", json!([{"field": "pop", "statistic": "SUM"}])));
    assert_eq!(err, "the output would have two fields named 'SUM_pop'");
    // Two statistics whose names differ only in case.
    let err = error("dissolve", &layers, p("", json!([{"field": "pop", "statistic": "MAX"}, {"field": "POP", "statistic": "MAX"}])));
    assert_eq!(err, "the output would have two fields named 'MAX_POP'");
    // Without the clashing dissolve field, the same statistic is fine.
    let out = run("dissolve", &layers, p("", json!([{"field": "pop", "statistic": "SUM"}, {"field": "POP", "statistic": "MIN"}])));
    assert_eq!(names(&out), ["count", "SUM_pop", "MIN_POP"]);
    assert_eq!(column(&out, "MIN_POP"), [&float(2.5)]);
}

/// Zones in record order: `west` and `east` overlap; `empty` has no points; `nowhere` has no
/// geometry.
fn zones() -> Dataset {
    let text = collection(vec![
        (square(0.0, 0.0, 10.0), json!({"name": "west"})),
        (square(5.0, 0.0, 10.0), json!({"name": "east"})),
        (square(100.0, 0.0, 1.0), json!({"name": "empty"})),
        (Json::Null, json!({"name": "nowhere"})),
    ]);
    let (ds, _) = read_geojson(&text, "zones").unwrap();
    assert_eq!(names(&ds), ["name"]);
    assert_eq!(ds.features.len(), 4);
    assert!(ds.features[3].geometry.is_none());
    ds
}

/// Sites in record order. 1 and 6 lie where `west` and `east` overlap, 2 in `east` only, 3 on
/// `west`'s east edge inside `east`, 4 in neither, and 5 has no geometry.
fn sites() -> Dataset {
    const MAX: i64 = i64::MAX;
    let text = collection(vec![
        (point(1.0, 1.0), json!({"val": 1, "label": "p0", "big": MAX})),
        (point(6.0, 1.0), json!({"val": 2, "label": "p1", "big": MAX - 1})),
        (point(12.0, 1.0), json!({"val": null, "label": "p2", "big": -5})),
        (point(10.0, 5.0), json!({"val": 4, "label": "p3", "big": null})),
        (point(50.0, 50.0), json!({"val": 100, "label": "p4", "big": 1})),
        (Json::Null, json!({"val": 1000, "label": "p5", "big": 1})),
        (point(7.0, 7.0), json!({"val": 8, "label": null, "big": null})),
    ]);
    let (ds, _) = read_geojson(&text, "sites").unwrap();
    assert_eq!(names(&ds), ["big", "label", "val"]);
    assert_eq!(field_ty(&ds, "val"), FieldType::Integer);
    assert_eq!(field_ty(&ds, "label"), FieldType::Text);
    assert_eq!(field_ty(&ds, "big"), FieldType::Integer, "premise: whole numbers up to i64::MAX read as Integer");
    assert_eq!(ds.value(0, 0), &int(MAX), "premise: i64::MAX is read exactly");
    assert_eq!(ds.features.len(), 7);
    ds
}

fn site_statistics() -> Json {
    json!([
        {"field": "val", "statistic": "SUM"},
        {"field": "val", "statistic": "MEAN"},
        {"field": "val", "statistic": "MEDIAN"},
        {"field": "val", "statistic": "COUNT"},
        {"field": "val", "statistic": "NULLS"},
        {"field": "val", "statistic": "FIRST"},
        {"field": "val", "statistic": "LAST"},
        {"field": "label", "statistic": "CONCATENATE"},
        {"field": "label", "statistic": "FIRST"},
        {"field": "label", "statistic": "LAST"},
        {"field": "label", "statistic": "NULLS"},
        {"field": "label", "statistic": "MIN"},
        {"field": "big", "statistic": "SUM"},
        {"field": "big", "statistic": "MAX"},
        {"field": "big", "statistic": "CONCATENATE"},
    ])
}

#[test]
fn summarize_within_adds_statistics_of_the_points_in_each_polygon() {
    let layers = Layers::default().with(zones()).with(sites());
    let res =
        rustgis_analysis::run("countPoints", &layers, &json!({"polygons": "zones", "points": "sites", "statistics": site_statistics()}))
            .unwrap();
    // SUM and MAX of `big` near i64::MAX are rounded in west and east, and the tool says so.
    assert_eq!(
        res.messages,
        [
            "Summarize Within: 8 points counted",
            "4 results rounded in SUM_big, MAX_big: a decimal field holds whole numbers exactly only up to 2^53 (9007199254740992)"
        ]
    );
    let out = layer(res);
    assert_eq!(
        names(&out),
        [
            "name",
            "point_count",
            "SUM_val",
            "MEAN_val",
            "MEDIAN_val",
            "COUNT_val",
            "NULLS_val",
            "FIRST_val",
            "LAST_val",
            "CONCATENATE_label",
            "FIRST_label",
            "LAST_label",
            "NULLS_label",
            "MIN_label",
            "SUM_big",
            "MAX_big",
            "CONCATENATE_big"
        ]
    );
    use FieldType::{Double as D, Integer as I, Text as T};
    let types: Vec<FieldType> = out.fields.iter().map(|f| f.ty).collect();
    assert_eq!(types, [T, I, D, D, D, I, I, D, D, T, T, T, I, T, D, D, T]);
    assert_eq!(column(&out, "name"), [&text("west"), &text("east"), &text("empty"), &text("nowhere")]);
    // Site 3, on west's edge, meets west as point_count counts it: the statistics take it too. A
    // point where the polygons overlap counts in both.
    assert_eq!(column(&out, "point_count"), [&int(4), &int(4), &int(0), &int(0)]);
    assert_eq!(column(&out, "COUNT_val"), [&int(4), &int(3), &int(0), &int(0)]);
    assert_eq!(column(&out, "NULLS_val"), [&int(0), &int(1), &int(0), &int(0)]);
    assert_eq!(column(&out, "SUM_val"), [&float(15.0), &float(14.0), &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "MEAN_val"), [&float(3.75), &float(14.0 / 3.0), &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "MEDIAN_val"), [&float(3.0), &float(4.0), &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "FIRST_val"), [&float(1.0), &float(2.0), &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "LAST_val"), [&float(8.0), &float(8.0), &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "CONCATENATE_label"), [&text("p0, p1, p3"), &text("p1, p2, p3"), &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "FIRST_label"), [&text("p0"), &text("p1"), &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "LAST_label"), [&text("p3"), &text("p3"), &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "NULLS_label"), [&int(1), &int(1), &int(0), &int(0)]);
    assert_eq!(column(&out, "MIN_label"), [&text("p0"), &text("p1"), &Value::Null, &Value::Null]);
    // Whole numbers near i64::MAX sum as decimals without overflowing, and CONCATENATE writes
    // them exactly.
    let two_63 = 9_223_372_036_854_775_808.0;
    // In east, i64::MAX − 1 − 5 rounds to 2^63 as a decimal.
    assert_eq!(column(&out, "SUM_big"), [&float(2.0 * two_63), &float(two_63), &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "MAX_big"), [&float(two_63), &float(two_63), &Value::Null, &Value::Null]);
    assert_eq!(
        column(&out, "CONCATENATE_big"),
        [&text("9223372036854775807, 9223372036854775806"), &text("9223372036854775806, -5"), &Value::Null, &Value::Null]
    );
    // The polygons' own geometry and attributes are kept.
    let input = zones();
    assert!(out.features.iter().zip(input.features.iter()).all(|(a, b)| a.geometry == b.geometry && a.id == b.id));
}

#[test]
fn summarize_within_statistics_take_only_selected_points_in_record_order() {
    // Out of order, with a point outside every polygon.
    let layers = Layers::default().with(zones()).with(sites()).select("sites", &[6, 4, 1, 0]).select("zones", &[2]);
    let statistics = json!([
        {"field": "val", "statistic": "SUM"},
        {"field": "val", "statistic": "FIRST"},
        {"field": "val", "statistic": "LAST"},
        {"field": "label", "statistic": "CONCATENATE"},
    ]);
    let out = run("countPoints", &layers, json!({"polygons": "zones", "points": "sites", "statistics": statistics, "separator": ";"}));
    // Every polygon, whatever the polygons' selection, as without statistics.
    assert_eq!(column(&out, "name").len(), 4);
    assert_eq!(column(&out, "point_count"), [&int(3), &int(2), &int(0), &int(0)]);
    assert_eq!(column(&out, "SUM_val"), [&float(11.0), &float(10.0), &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "FIRST_val"), [&float(1.0), &float(2.0), &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "LAST_val"), [&float(8.0), &float(8.0), &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "CONCATENATE_label"), [&text("p0;p1"), &text("p1"), &Value::Null, &Value::Null]);
}

#[test]
fn summarize_within_without_statistics_is_unchanged() {
    let layers = Layers::default().with(zones()).with(sites());
    let res = rustgis_analysis::run("countPoints", &layers, &json!({"polygons": "zones", "points": "sites"})).unwrap();
    let messages = res.messages.clone();
    let plain = layer(res);
    assert_eq!(names(&plain), ["name", "point_count"]);
    assert_eq!(column(&plain, "point_count"), [&int(4), &int(4), &int(0), &int(0)]);
    for empty in [json!([]), json!(""), Json::Null] {
        let p = json!({"polygons": "zones", "points": "sites", "statistics": empty, "separator": []});
        let res = rustgis_analysis::run("countPoints", &layers, &p).unwrap();
        assert_eq!(res.messages, messages, "{empty}");
        assert_eq!(layer(res), plain, "{empty}");
    }
    // From JSON text, the same as from a list.
    let list = site_statistics();
    let from_list = run("countPoints", &layers, json!({"polygons": "zones", "points": "sites", "statistics": list}));
    let from_text = run("countPoints", &layers, json!({"polygons": "zones", "points": "sites", "statistics": list.to_string()}));
    assert_eq!(from_list, from_text);
    assert_eq!(from_list.fields[..2], plain.fields[..]);
    assert!(from_list.features.iter().zip(plain.features.iter()).all(|(a, b)| a.attrs[..2] == b.attrs[..]));
}

#[test]
fn summarize_within_refuses_bad_statistics_and_clashing_names() {
    // Polygons with a field named as a statistic of the points would be, in another case, and
    // a point_count of their own.
    let text = collection(vec![(square(0.0, 0.0, 10.0), json!({"sum_VAL": 1, "point_count": 9}))]);
    let (polys, _) = read_geojson(&text, "counted").unwrap();
    assert_eq!(names(&polys), ["point_count", "sum_VAL"]);
    let layers = Layers::default().with(zones()).with(sites()).with(polys);
    let run_with = |polygons: &str, statistics: Json| {
        rustgis_analysis::run("countPoints", &layers, &json!({"polygons": polygons, "points": "sites", "statistics": statistics}))
    };
    let err = |polygons: &str, statistics: Json| run_with(polygons, statistics).err().unwrap_or_else(|| panic!("should fail"));
    assert_eq!(err("counted", json!([{"field": "val", "statistic": "SUM"}])), "the output would have two fields named 'SUM_val'");
    // A statistic is named for the points' field, never for the polygons' fields: the zones have
    // no `val`, and their `name` isn't a field of the points.
    assert_eq!(err("zones", json!([{"field": "name", "statistic": "COUNT"}])), "no field named 'name'");
    assert_eq!(err("zones", json!([{"field": "label", "statistic": "MEAN"}])), "MEAN needs a numeric field ('label' is not)");
    assert!(err("zones", json!([{"field": "val", "statistic": "P100"}])).starts_with("the statistic for 'val' must be one of"));
    assert_eq!(
        err("zones", json!([{"field": "val", "statistic": "LAST"}, {"field": "val", "statistic": "last"}])),
        "LAST of 'val' is listed twice"
    );
    // The polygons' existing point_count is reused, as without statistics, and other names pass.
    let out = layer(run_with("counted", json!([{"field": "val", "statistic": "MAX"}])).unwrap());
    assert_eq!(names(&out), ["point_count", "sum_VAL", "MAX_val"]);
    assert_eq!(column(&out, "point_count"), [&int(4)]);
    assert_eq!(column(&out, "MAX_val"), [&float(8.0)]);
}

#[test]
fn summarize_within_reads_statistics_of_csv_points() {
    // Points from a CSV table, the reader typing each column.
    let csv = "lon,lat,kind,amount\n1,1,shop,2.5\n2,2,home,\n3,3,shop,4\n20,20,shop,100\n";
    let (table, _) = read_csv(csv, "places").unwrap();
    assert_eq!(field_ty(&table, "amount"), FieldType::Double);
    assert_eq!(field_ty(&table, "kind"), FieldType::Text);
    let layers = Layers::default().with(zones()).with(table);
    let points = run("xyTableToPoint", &layers, json!({"input": "places", "x": "lon", "y": "lat", "output": "places_xy"}));
    let layers = layers.with(points);
    let statistics = json!([
        {"field": "amount", "statistic": "SUM"},
        {"field": "amount", "statistic": "NULLS"},
        {"field": "kind", "statistic": "MODE"},
        {"field": "kind", "statistic": "UNIQUE"},
    ]);
    let out = run("countPoints", &layers, json!({"polygons": "zones", "points": "places_xy", "statistics": statistics}));
    assert_eq!(column(&out, "point_count"), [&int(3), &int(0), &int(0), &int(0)]);
    assert_eq!(column(&out, "SUM_amount"), [&float(6.5), &Value::Null, &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "NULLS_amount"), [&int(1), &int(0), &int(0), &int(0)]);
    assert_eq!(column(&out, "MODE_kind"), [&text("shop"), &Value::Null, &Value::Null, &Value::Null]);
    assert_eq!(column(&out, "UNIQUE_kind"), [&int(2), &int(0), &int(0), &int(0)]);
}

#[test]
fn a_record_selected_twice_counts_once() {
    // Dissolve: zone A's records 0, 2 and 7, with 0 and 7 listed twice.
    let layers = Layers::default().with(parcels()).select("parcels", &[0, 7, 0, 2, 7]);
    let statistics = json!([{"field": "pop", "statistic": "SUM"}, {"field": "pop", "statistic": "COUNT"}]);
    let out = run("dissolve", &layers, json!({"input": "parcels", "field": "zone", "statistics": statistics}));
    assert_eq!(column(&out, "zone"), [&text("A")]);
    assert_eq!(column(&out, "count"), [&int(3)]);
    assert_eq!(column(&out, "COUNT_pop"), [&int(3)]);
    assert_eq!(column(&out, "SUM_pop"), [&float(100.0)]);
    let plain = run("dissolve", &layers, json!({"input": "parcels", "field": "zone"}));
    assert_eq!(column(&plain, "count"), [&int(3)], "without statistics too");
    // The same records in order, 0 and 7 still listed twice.
    let sorted = Layers::default().with(parcels()).select("parcels", &[0, 0, 2, 7, 7]);
    let out = run("dissolve", &sorted, json!({"input": "parcels", "field": "zone", "statistics": statistics}));
    assert_eq!(column(&out, "count"), [&int(3)]);
    assert_eq!(column(&out, "COUNT_pop"), [&int(3)]);
    // Summarize Within: sites 0 and 1, both in west, 1 also in east, each listed twice.
    let layers = Layers::default().with(zones()).with(sites()).select("sites", &[1, 0, 1, 0]);
    let statistics = json!([{"field": "val", "statistic": "COUNT"}, {"field": "val", "statistic": "SUM"}]);
    let res =
        rustgis_analysis::run("countPoints", &layers, &json!({"polygons": "zones", "points": "sites", "statistics": statistics})).unwrap();
    assert_eq!(res.messages, ["Summarize Within: 3 points counted"]);
    let out = layer(res);
    assert_eq!(column(&out, "point_count"), [&int(2), &int(1), &int(0), &int(0)]);
    assert_eq!(column(&out, "COUNT_val"), [&int(2), &int(1), &int(0), &int(0)]);
    assert_eq!(column(&out, "SUM_val"), [&float(3.0), &float(2.0), &Value::Null, &Value::Null]);
}

/// Whole numbers in a GeoJSON file, read as Integer: `n` past 2^53 in zone A, `m` within it but
/// summing past it in zone B, and `small` within it everywhere.
fn large_numbers() -> Dataset {
    const BIG: i64 = (1 << 53) + 1;
    let text = collection(vec![
        (square(0.0, 0.0, 1.0), json!({"zone": "A", "n": BIG, "m": 1, "small": 1})),
        (square(1.0, 0.0, 1.0), json!({"zone": "A", "n": 2, "m": 2, "small": 2})),
        (square(5.0, 0.0, 1.0), json!({"zone": "B", "n": 3, "m": 1i64 << 53, "small": 3})),
        (square(6.0, 0.0, 1.0), json!({"zone": "B", "n": 4, "m": 1, "small": 4})),
    ]);
    let (ds, _) = read_geojson(&text, "large").unwrap();
    for f in ["n", "m", "small"] {
        assert_eq!(field_ty(&ds, f), FieldType::Integer, "premise: {f} reads as Integer");
    }
    assert_eq!(ds.value(0, ds.fields.iter().position(|f| f.name == "n").unwrap()), &int(BIG), "premise: read exactly");
    ds
}

#[test]
fn rounded_statistics_are_reported_and_exact_ones_are_not() {
    let layers = Layers::default().with(large_numbers());
    let statistics = json!([
        {"field": "n", "statistic": "SUM"},
        {"field": "n", "statistic": "MAX"},
        {"field": "n", "statistic": "MIN"},
        {"field": "n", "statistic": "CONCATENATE"},
        {"field": "m", "statistic": "SUM"},
        {"field": "m", "statistic": "MAX"},
        {"field": "small", "statistic": "SUM"},
        {"field": "small", "statistic": "MEAN"},
    ]);
    let note = |n: &str, fields: &str| {
        format!("{n} rounded in {fields}: a decimal field holds whole numbers exactly only up to 2^53 (9007199254740992)")
    };
    // Dissolve: zone A rounds SUM_n and MAX_n (its MIN is 2), zone B rounds SUM_m (2^53 + 1);
    // MAX_m, 2^53, fits. CONCATENATE writes the whole numbers as they are.
    let res = rustgis_analysis::run("dissolve", &layers, &json!({"input": "large", "field": "zone", "statistics": statistics})).unwrap();
    assert_eq!(res.messages, ["Dissolve: 2 features".to_string(), note("3 results", "SUM_n, MAX_n, SUM_m")]);
    let out = layer(res);
    assert_eq!(column(&out, "CONCATENATE_n"), [&text("9007199254740993, 2"), &text("3, 4")]);
    assert_eq!(column(&out, "MIN_n"), [&float(2.0), &float(3.0)]);
    // One group: SUM_m of 1, 2, 2^53 and 1 in record order passes 2^53 but comes out exact,
    // 2^53 + 4, so only SUM_n and MAX_n are rounded.
    let res = rustgis_analysis::run("dissolve", &layers, &json!({"input": "large", "statistics": statistics})).unwrap();
    assert_eq!(res.messages[1], note("2 results", "SUM_n, MAX_n"));
    assert_eq!(column(&layer(res), "SUM_m"), [&float(9_007_199_254_740_996.0)]);
    // Only zone B: SUM_m alone, one value.
    let only_b = Layers::default().with(large_numbers()).select("large", &[2, 3]);
    let res = rustgis_analysis::run("dissolve", &only_b, &json!({"input": "large", "statistics": statistics})).unwrap();
    assert_eq!(res.messages[1], note("1 result", "SUM_m"));
    // Fields within 2^53: no notice.
    let clean = json!([{"field": "small", "statistic": "SUM"}, {"field": "n", "statistic": "MIN"}, {"field": "n", "statistic": "COUNT"}]);
    let res = rustgis_analysis::run("dissolve", &layers, &json!({"input": "large", "field": "zone", "statistics": clean})).unwrap();
    assert_eq!(res.messages, ["Dissolve: 2 features"]);

    // Summarize Within over the same records as points: a zone holding all of them.
    let points = run("featureToPoint", &layers, json!({"input": "large", "output": "large_points"}));
    let area = collection(vec![(square(-1.0, -1.0, 20.0), json!({"name": "all"}))]);
    let (area, _) = read_geojson(&area, "area").unwrap();
    let layers = Layers::default().with(points).with(area);
    let res =
        rustgis_analysis::run("countPoints", &layers, &json!({"polygons": "area", "points": "large_points", "statistics": statistics}))
            .unwrap();
    assert_eq!(res.messages, ["Summarize Within: 4 points counted".to_string(), note("2 results", "SUM_n, MAX_n")]);
    let res =
        rustgis_analysis::run("countPoints", &layers, &json!({"polygons": "area", "points": "large_points", "statistics": clean})).unwrap();
    assert_eq!(res.messages, ["Summarize Within: 4 points counted"]);

    // Summary Statistics, by zone and from its single-field shortcut.
    let layers = Layers::default().with(large_numbers());
    let res =
        rustgis_analysis::run("summaryStatistics", &layers, &json!({"input": "large", "case": "zone", "statistics": statistics})).unwrap();
    assert_eq!(res.messages, ["Summary Statistics: 2 rows".to_string(), note("3 results", "SUM_n, MAX_n, SUM_m")]);
    let res = rustgis_analysis::run("summaryStatistics", &layers, &json!({"input": "large", "field": "n"})).unwrap();
    assert_eq!(res.messages, ["Summary Statistics: 1 rows".to_string(), note("2 results", "SUM_n, MAX_n")]);
    let res = rustgis_analysis::run("summaryStatistics", &layers, &json!({"input": "large", "field": "small"})).unwrap();
    assert_eq!(res.messages, ["Summary Statistics: 1 rows"]);
    // The shortcut reads a text field's values as numbers: whole numbers written as text past 2^53
    // are rounded and reported too. The MEAN, (2^53 + 1 + 1) / 2, is whole and rounded as well.
    let csv = "code\n9007199254740993\nn/a\n1\n";
    let (codes, _) = read_csv(csv, "codes").unwrap();
    assert_eq!(field_ty(&codes, "code"), FieldType::Text, "premise: a column with a word in it is text");
    let layers = Layers::default().with(codes);
    let res = rustgis_analysis::run("summaryStatistics", &layers, &json!({"input": "codes", "field": "code"})).unwrap();
    assert_eq!(res.messages, ["Summary Statistics: 1 rows".to_string(), note("3 results", "SUM_code, MEAN_code, MAX_code")]);
}

#[test]
fn rounding_check_survives_whole_numbers_past_the_i64_range() {
    // Text read as numbers: whole decimals near 1e38, whose sum would pass the exact check's range.
    let csv = "code\n1\n9e37\n9e37\n-9e37\nn/a\n";
    let (codes, _) = read_csv(csv, "codes").unwrap();
    assert_eq!(field_ty(&codes, "code"), FieldType::Text, "premise: a column with a word in it is text");
    let layers = Layers::default().with(codes);
    let res = rustgis_analysis::run("summaryStatistics", &layers, &json!({"input": "codes", "field": "code"})).unwrap();
    assert_eq!(res.messages, ["Summary Statistics: 1 rows"], "decimals past the i64 range are decimals, not whole numbers");
    // A whole number written as text in digits past the i64 range is read as a decimal too, past
    // what the checks take exactly: the tool says so once for the field, and its statistics aren't
    // checked one by one. One the decimal holds (10^20) is counted as well, as its statistics
    // aren't checked either; one written with an exponent (above) isn't.
    let csv = "code\n123456789012345678901\n100000000000000000000\n1\nn/a\n-9223372036854775809\n";
    let (codes, _) = read_csv(csv, "codes").unwrap();
    assert_eq!(field_ty(&codes, "code"), FieldType::Text, "premise");
    let layers = Layers::default().with(codes);
    let res = rustgis_analysis::run("summaryStatistics", &layers, &json!({"input": "codes", "field": "code"})).unwrap();
    assert_eq!(
        res.messages,
        [
            "Summary Statistics: 1 rows".to_string(),
            "3 whole numbers written as text in code are past the 64-bit integer range and were read as decimals, so statistics of them may be rounded"
                .to_string()
        ]
    );
    let Output::Table(out) = res.output else { panic!("a table") };
    assert_eq!(column(&out, "MAX_code"), [&float(123_456_789_012_345_678_901.0)], "the decimal, not the number written");
    // 2^63 is a decimal, but the SUM of it and 1 is 2^63 + 1, which isn't: the statistic is rounded
    // without the per-statistic check seeing it, and the count is the notice.
    let csv = "code\n9223372036854775808\n1\nn/a\n";
    let (codes, _) = read_csv(csv, "codes").unwrap();
    assert_eq!(field_ty(&codes, "code"), FieldType::Text, "premise");
    let layers = Layers::default().with(codes);
    let res = rustgis_analysis::run("summaryStatistics", &layers, &json!({"input": "codes", "field": "code"})).unwrap();
    assert_eq!(
        res.messages,
        [
            "Summary Statistics: 1 rows".to_string(),
            "1 whole number written as text in code is past the 64-bit integer range and was read as a decimal, so statistics of it may be rounded"
                .to_string()
        ]
    );
    let Output::Table(out) = res.output else { panic!("a table") };
    assert_eq!(column(&out, "SUM_code"), [&float(9_223_372_036_854_775_808.0)], "2^63, not 2^63 + 1");
    // By case, the field is counted once over all its groups, and the results' notice comes first.
    let csv = "k,code\na,123456789012345678901\nb,9007199254740993\nb,9223372036854775809\nc,n/a\n";
    let (codes, _) = read_csv(csv, "codes").unwrap();
    assert_eq!(field_ty(&codes, "code"), FieldType::Text, "premise");
    let layers = Layers::default().with(codes);
    let statistics = json!([{"field": "code", "statistic": "MAX"}, {"field": "code", "statistic": "FIRST"}]);
    let p = json!({"input": "codes", "case": "k", "statistics": statistics});
    assert_eq!(
        rustgis_analysis::run("summaryStatistics", &layers, &p).unwrap().messages,
        ["Summary Statistics: 3 rows"],
        "a text field's statistics are text, read as they are"
    );
    let p = json!({"input": "codes", "case": "k", "field": "code"});
    let res = rustgis_analysis::run("summaryStatistics", &layers, &p).unwrap();
    assert_eq!(
        res.messages,
        [
            "Summary Statistics: 3 rows".to_string(),
            "1 result rounded in MIN_code: a decimal field holds whole numbers exactly only up to 2^53 (9007199254740992)".to_string(),
            "2 whole numbers written as text in code are past the 64-bit integer range and were read as decimals, so statistics of them may be rounded"
                .to_string()
        ]
    );
    // One such value.
    let (codes, _) = read_csv("code\n123456789012345678901\nx\n", "codes").unwrap();
    let res =
        rustgis_analysis::run("summaryStatistics", &Layers::default().with(codes), &json!({"input": "codes", "field": "code"})).unwrap();
    assert_eq!(
        res.messages[1],
        "1 whole number written as text in code is past the 64-bit integer range and was read as a decimal, so statistics of it may be rounded"
    );
    // An Integer field holding such decimals beside a whole number, through every statistic.
    let text = collection(vec![(square(0.0, 0.0, 1.0), json!({"n": 1})), (square(1.0, 0.0, 1.0), json!({"n": 2}))]);
    let (mut ds, _) = read_geojson(&text, "huge").unwrap();
    assert_eq!(field_ty(&ds, "n"), FieldType::Integer);
    for x in [9e37, 9e37, -9e37, 1.7e38] {
        ds.push(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.5, 0.5))), vec![Value::Float(x)]);
    }
    let all: Vec<Json> = ["SUM", "MEAN", "MIN", "MAX", "RANGE", "MEDIAN", "P10", "P90", "IQR", "MODE", "FIRST", "LAST"]
        .iter()
        .map(|s| json!({"field": "n", "statistic": s}))
        .collect();
    let layers = Layers::default().with(ds);
    let res = rustgis_analysis::run("summaryStatistics", &layers, &json!({"input": "huge", "statistics": all})).unwrap();
    assert_eq!(res.messages, ["Summary Statistics: 1 rows"]);
}

/// Whole numbers written as text past the i64 range are counted per record: a point in two
/// overlapping polygons is read for both but is one value, and so is a record in a dissolved group.
#[test]
fn a_rounded_text_in_overlapping_polygons_is_counted_once() {
    const HUGE: &str = "123456789012345678901";
    const LOW: &str = "-9223372036854775809";
    let one = |n: usize| {
        format!(
            "{n} whole number written as text in n is past the 64-bit integer range and was read as a decimal, so statistics of it may be rounded"
        )
    };
    let many = |n: usize| {
        format!(
            "{n} whole numbers written as text in n are past the 64-bit integer range and were read as decimals, so statistics of them may be rounded"
        )
    };
    // Sites 0 and 1 lie where `west` and `east` overlap, 2 in `west` only, 3 in neither. The
    // Integer field `n` then gets text values: HUGE at 0 and 2, LOW at 3, and 2^53 + 1 at 1.
    let geojson = collection(vec![
        (point(6.0, 1.0), json!({"n": 1})),
        (point(6.0, 2.0), json!({"n": (1i64 << 53) + 1})),
        (point(1.0, 1.0), json!({"n": 1})),
        (point(50.0, 50.0), json!({"n": 1})),
    ]);
    let (mut pts, _) = read_geojson(&geojson, "sites").unwrap();
    assert_eq!(field_ty(&pts, "n"), FieldType::Integer, "premise");
    pts.features[0].attrs[0] = text(HUGE);
    pts.features[2].attrs[0] = text(HUGE);
    pts.features[3].attrs[0] = text(LOW);
    let statistics = json!([{"field": "n", "statistic": "MIN"}, {"field": "n", "statistic": "MAX"}, {"field": "n", "statistic": "COUNT"}]);
    let p = |rows: &str| json!({"polygons": "zones", "points": rows, "statistics": statistics});
    // Site 0 alone: one value, in two polygons. Site 1's 2^53 + 1 is the MIN picked in both, a
    // result rounded in each, so that notice counts two.
    let layers = Layers::default().with(zones()).with(pts.clone()).select("sites", &[1, 0]);
    let res = rustgis_analysis::run("countPoints", &layers, &p("sites")).unwrap();
    assert_eq!(
        res.messages,
        [
            "Summarize Within: 4 points counted".to_string(),
            "2 results rounded in MIN_n: a decimal field holds whole numbers exactly only up to 2^53 (9007199254740992)".to_string(),
            one(1)
        ]
    );
    let out = layer(res);
    assert_eq!(column(&out, "COUNT_n"), [&int(2), &int(2), &int(0), &int(0)]);
    assert_eq!(
        column(&out, "MAX_n"),
        [&float(123_456_789_012_345_678_901.0), &float(123_456_789_012_345_678_901.0), &Value::Null, &Value::Null]
    );
    // All sites: site 2's HUGE is a second value, and site 3's LOW lies in no polygon and isn't read.
    let layers = Layers::default().with(zones()).with(pts.clone());
    let res = rustgis_analysis::run("countPoints", &layers, &p("sites")).unwrap();
    assert_eq!(res.messages[0], "Summarize Within: 5 points counted");
    assert_eq!(res.messages[2], many(2));
    assert_eq!(column(&layer(res), "COUNT_n"), [&int(3), &int(2), &int(0), &int(0)]);
    // Without statistics nothing is read, and nothing said.
    let res = rustgis_analysis::run("countPoints", &layers, &json!({"polygons": "zones", "points": "sites"})).unwrap();
    assert_eq!(res.messages, ["Summarize Within: 5 points counted"]);

    // Dissolve: records 0 and 2 in zone A, 1 and 3 in zone B, with the same values; each is in one
    // group, and the count is the same by zone and as one group. Zone B's MAX, 2^53 + 1, is the one
    // result rounded: zone A's and the whole layer's MIN and MAX are the decimals read from text.
    let geojson = collection(vec![
        (square(0.0, 0.0, 1.0), json!({"zone": "A", "n": 1})),
        (square(5.0, 0.0, 1.0), json!({"zone": "B", "n": (1i64 << 53) + 1})),
        (square(1.0, 0.0, 1.0), json!({"zone": "A", "n": 1})),
        (square(6.0, 0.0, 1.0), json!({"zone": "B", "n": 1})),
    ]);
    let (mut ds, _) = read_geojson(&geojson, "parcels").unwrap();
    assert_eq!(names(&ds), ["n", "zone"], "premise");
    assert_eq!(field_ty(&ds, "n"), FieldType::Integer, "premise");
    ds.features[0].attrs[0] = text(HUGE);
    ds.features[2].attrs[0] = text(HUGE);
    ds.features[3].attrs[0] = text(LOW);
    let layers = Layers::default().with(ds.clone());
    let res = rustgis_analysis::run("dissolve", &layers, &json!({"input": "parcels", "field": "zone", "statistics": statistics})).unwrap();
    assert_eq!(
        res.messages,
        [
            "Dissolve: 2 features".to_string(),
            "1 result rounded in MAX_n: a decimal field holds whole numbers exactly only up to 2^53 (9007199254740992)".to_string(),
            many(3)
        ]
    );
    let res = rustgis_analysis::run("dissolve", &layers, &json!({"input": "parcels", "statistics": statistics})).unwrap();
    assert_eq!(res.messages, ["Dissolve: 1 features".to_string(), many(3)]);
    // Only zone A, selected with a repeat: its two values.
    let layers = Layers::default().with(ds).select("parcels", &[2, 0, 2]);
    let res = rustgis_analysis::run("dissolve", &layers, &json!({"input": "parcels", "field": "zone", "statistics": statistics})).unwrap();
    assert_eq!(res.messages, ["Dissolve: 1 features".to_string(), many(2)]);
}

/// A Double field's values are decimals already: statistics of them round as decimal arithmetic
/// rounds, and the tools say nothing, however large the values. The readers store a Double field's
/// whole numbers as decimals too.
#[test]
fn decimal_fields_get_no_rounding_notice() {
    const BIG: f64 = 9_007_199_254_740_994.0;
    // GeoJSON: a decimal among whole numbers makes the field Double, and 2^53 + 2 and 1 sum to
    // 2^53 + 3, which a decimal can't hold.
    let text = collection(vec![
        (square(0.0, 0.0, 1.0), json!({"zone": "A", "d": 9_007_199_254_740_994i64})),
        (square(1.0, 0.0, 1.0), json!({"zone": "A", "d": 1})),
        (square(5.0, 0.0, 1.0), json!({"zone": "B", "d": 0.5})),
    ]);
    let (ds, _) = read_geojson(&text, "decimals").unwrap();
    assert_eq!(field_ty(&ds, "d"), FieldType::Double, "premise");
    assert_eq!(column(&ds, "d"), [&float(BIG), &float(1.0), &float(0.5)], "premise: whole numbers stored as decimals");
    let statistics: Vec<Json> = ["SUM", "RANGE", "MEAN", "MEDIAN", "P75", "IQR", "MIN", "MAX", "FIRST", "MODE"]
        .iter()
        .map(|s| json!({"field": "d", "statistic": s}))
        .collect();
    let layers = Layers::default().with(ds);
    let res = rustgis_analysis::run("dissolve", &layers, &json!({"input": "decimals", "field": "zone", "statistics": statistics})).unwrap();
    assert_eq!(res.messages, ["Dissolve: 2 features"]);
    let out = layer(res);
    assert_eq!(column(&out, "SUM_d"), [&float(9_007_199_254_740_996.0), &float(0.5)], "rounded, as decimals are");
    assert_eq!(column(&out, "MAX_d"), [&float(BIG), &float(0.5)]);
    // Summarize Within over the same records as points.
    let points = run("featureToPoint", &layers, json!({"input": "decimals", "output": "decimal_points"}));
    let area = collection(vec![(square(-1.0, -1.0, 20.0), json!({"name": "all"}))]);
    let (area, _) = read_geojson(&area, "area").unwrap();
    let layers = Layers::default().with(points).with(area);
    let p = json!({"polygons": "area", "points": "decimal_points", "statistics": statistics});
    let res = rustgis_analysis::run("countPoints", &layers, &p).unwrap();
    assert_eq!(res.messages, ["Summarize Within: 3 points counted"]);
    assert_eq!(column(&layer(res), "SUM_d"), [&float(9_007_199_254_740_996.0)]);
    // CSV: a column of whole numbers with a decimal among them reads as Double as well.
    let csv = "d\n9007199254740994\n1\n0.5\n";
    let (ds, _) = read_csv(csv, "decimals").unwrap();
    assert_eq!(field_ty(&ds, "d"), FieldType::Double, "premise");
    assert_eq!(column(&ds, "d"), [&float(BIG), &float(1.0), &float(0.5)], "premise");
    let layers = Layers::default().with(ds);
    let res = rustgis_analysis::run("summaryStatistics", &layers, &json!({"input": "decimals", "statistics": statistics})).unwrap();
    assert_eq!(res.messages, ["Summary Statistics: 1 rows"]);
    let res = rustgis_analysis::run("summaryStatistics", &layers, &json!({"input": "decimals", "field": "d"})).unwrap();
    assert_eq!(res.messages, ["Summary Statistics: 1 rows"], "through the shortcut too");
}

#[test]
fn whole_numbers_written_as_text_are_counted_and_joined_exactly() {
    // An Integer field holding whole numbers past 2^53 as text, which a decimal can't tell apart:
    // UNIQUE, MODE, MINORITY and CONCATENATE take them exactly, as they do Integer values.
    let geojson = collection(vec![(square(0.0, 0.0, 1.0), json!({"n": 1}))]);
    let (mut ds, _) = read_geojson(&geojson, "codes").unwrap();
    assert_eq!(field_ty(&ds, "n"), FieldType::Integer);
    ds.features.clear();
    for t in ["9007199254740993", "9.007199254740992e15", "9007199254740993", "9007199254740995.0"] {
        ds.push(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.5, 0.5))), vec![text(t)]);
    }
    let statistics = json!([
        {"field": "n", "statistic": "UNIQUE"},
        {"field": "n", "statistic": "MODE"},
        {"field": "n", "statistic": "MINORITY"},
        {"field": "n", "statistic": "CONCATENATE"},
    ]);
    let layers = Layers::default().with(ds);
    let res = rustgis_analysis::run("summaryStatistics", &layers, &json!({"input": "codes", "statistics": statistics})).unwrap();
    let Output::Table(out) = res.output else { panic!("a table") };
    assert_eq!(column(&out, "UNIQUE_n"), [&int(3)]);
    // 2^53 + 1 twice is the mode; as decimals all four would be 2^53 or 2^53 + 4.
    assert_eq!(column(&out, "MODE_n"), [&float(9_007_199_254_740_992.0)], "2^53 + 1, written as a decimal");
    assert_eq!(column(&out, "MINORITY_n"), [&float(9_007_199_254_740_992.0)], "2^53, the lowest of the rarest");
    assert_eq!(column(&out, "CONCATENATE_n"), [&text("9007199254740993, 9007199254740992, 9007199254740993, 9007199254740995")]);
    // MODE, 2^53 + 1, comes out rounded, and the tool says so; MINORITY, 2^53, fits.
    assert_eq!(res.messages.len(), 2, "{:?}", res.messages);
    assert!(res.messages[1].starts_with("1 result rounded in MODE_n:"), "{:?}", res.messages);
}
