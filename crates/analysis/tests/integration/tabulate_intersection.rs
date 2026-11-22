//! Tabulate Intersection through `rustgis_analysis::run`: class areas and sums shared out to zones.

use rustgis_analysis::{Inputs, Output, ToolResult};
use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value};
use serde_json::json;
use std::collections::HashMap;

/// The tool's inputs: layers by name, and each layer's selected rows.
#[derive(Default)]
struct Layers {
    data: HashMap<String, Dataset>,
    selected: HashMap<String, Vec<usize>>,
}
impl Inputs for Layers {
    /// The layer of this name.
    fn dataset(&self, name: &str) -> Option<&Dataset> {
        self.data.get(name)
    }
    /// The layer's selected rows; none selects nothing, so every row is used.
    fn selection(&self, name: &str) -> &[usize] {
        self.selected.get(name).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// A lon/lat rectangle from `x0` to `x1` and the equator to 1°N.
fn rect(x0: f64, x1: f64) -> Option<Geometry> {
    let ring = [(x0, 0.0), (x1, 0.0), (x1, 1.0), (x0, 1.0), (x0, 0.0)].map(|(x, y)| Coord::new(x, y)).to_vec();
    Some(Geometry::Polygon(vec![vec![ring]]))
}

/// Zones `A` (0°–1°E), `B` (1°–2°E) and `C` (10°–11°E), named by `zone`.
fn zones() -> Dataset {
    let mut ds = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    for (name, x0) in [("A", 0.0), ("B", 1.0), ("C", 10.0)] {
        ds.push_next(rect(x0, x0 + 1.0), vec![Value::Text(name.into())]);
    }
    ds
}

/// Classes by `kind`, with a population `pop`: a forest across A and B, a second forest over the
/// first one's half in A, and water over A's other half with no population.
fn classes() -> Dataset {
    let mut ds = Dataset::new(
        "classes",
        GeometryType::Polygon,
        vec![Field::new("kind", FieldType::Text), Field::new("pop", FieldType::Integer), Field::new("zone", FieldType::Text)],
    );
    let rows = [(0.5, 1.5, "forest", Value::Int(100)), (0.5, 1.0, "forest", Value::Int(10)), (0.0, 0.5, "water", Value::Null)];
    for (x0, x1, kind, pop) in rows {
        ds.push_next(rect(x0, x1), vec![Value::Text(kind.into()), pop, Value::Text("x".into())]);
    }
    ds
}

/// The two layers as the tool's inputs, under their own names, with nothing selected.
fn layers(zones: Dataset, classes: Dataset) -> Layers {
    let mut l = Layers::default();
    l.data.insert(zones.name.clone(), zones);
    l.data.insert(classes.name.clone(), classes);
    l
}

/// The standalone table a run made.
fn table(result: ToolResult) -> Dataset {
    match result.output {
        Output::Table(ds) => ds,
        _ => panic!("expected a table"),
    }
}

/// Each row's text fields, then its numbers rounded to 3 decimals (empty as NaN).
fn rows(ds: &Dataset) -> Vec<(Vec<String>, Vec<f64>)> {
    ds.features
        .iter()
        .map(|f| {
            let text = f.attrs.iter().filter_map(|v| if let Value::Text(t) = v { Some(t.clone()) } else { None }).collect();
            let nums = f
                .attrs
                .iter()
                .filter(|v| !matches!(v, Value::Text(_)))
                .map(|v| v.as_f64().map_or(f64::NAN, |n| (n * 1000.0).round() / 1000.0))
                .collect();
            (text, nums)
        })
        .collect()
}

/// The `area_km2` Calculate Geometry Attributes gives a polygon, rounded as [`rows`] rounds.
fn calculated_area_km2(g: Option<Geometry>) -> f64 {
    let mut ds = Dataset::new("one", GeometryType::Polygon, vec![]);
    ds.push_next(g, vec![]);
    let l = layers(ds, Dataset::new("none", GeometryType::Polygon, vec![]));
    let Output::Update { dataset, .. } = rustgis_analysis::run("calculateGeometry", &l, &json!({"input": "one"})).unwrap().output else {
        panic!("expected an update")
    };
    let area = dataset.field_index("area_km2").map(|i| dataset.value(0, i).clone());
    area.and_then(|v| v.as_f64()).map(|a| (a * 1000.0).round() / 1000.0).unwrap()
}

#[test]
fn shares_class_areas_and_sums_out_to_zones() {
    let l = layers(zones(), classes());
    let params = json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "classFields": "kind", "sumFields": ["pop"]});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    let messages = result.messages.clone();
    let out = table(result);
    let names: Vec<&str> = out.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["zone", "kind", "area_km2", "percentage", "pop"]);
    assert!(out.fields.iter().skip(2).all(|f| f.ty == FieldType::Double));
    let got = rows(&out);
    let text: Vec<Vec<String>> = got.iter().map(|(t, _)| t.clone()).collect();
    assert_eq!(text, [["A", "forest"], ["A", "water"], ["B", "forest"]].map(|r| r.map(String::from).to_vec()));
    // Half of A, measured as Calculate Geometry Attributes measures it: the two forests over that
    // half count once.
    let expected = calculated_area_km2(rect(0.5, 1.0));
    let half = got.first().and_then(|(_, n)| n.first()).copied().unwrap();
    assert!((half - expected).abs() < 0.01 && (6100.0..6200.0).contains(&half), "{half} km², not {expected}");
    for (_, nums) in &got {
        assert!((nums[0] - half).abs() < 0.01 && (nums[1] - 50.0).abs() < 0.001, "{nums:?}");
    }
    // A gets half the first forest's 100 and all the second's 10; B the other half of 100. Water
    // has no population, so its share is 0 and a notice says so.
    let pops: Vec<f64> = got.iter().map(|(_, n)| n[2]).collect();
    assert_eq!(pops, [60.0, 0.0, 50.0]);
    assert_eq!(
        messages,
        [
            "Tabulate Intersection: 3 rows",
            "1 zone overlaps no class feature, so it has no rows",
            "1 class feature has no value in 'pop', left out of its shares",
        ]
    );
}

#[test]
fn clean_input_gives_no_notices() {
    let mut l = layers(zones(), classes());
    // Zones A and B only, and the forests, which all have a population.
    l.selected.insert("zones".into(), vec![1, 0]);
    l.selected.insert("classes".into(), vec![0, 1]);
    let params = json!({"zones": "zones", "zoneFields": ["zone"], "classes": "classes", "sumFields": "pop"});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(result.messages, ["Tabulate Intersection: 2 rows"]);
    let out = table(result);
    // Without class fields every class feature is one class.
    let names: Vec<&str> = out.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["zone", "area_km2", "percentage", "pop"]);
    let got = rows(&out);
    assert_eq!(got.iter().map(|(t, _)| t.join("")).collect::<Vec<_>>(), ["A", "B"]);
    assert_eq!(got.iter().map(|(_, n)| (n[1], n[2])).collect::<Vec<_>>(), [(50.0, 60.0), (50.0, 50.0)]);
}

/// The middle zone only touches a class. Its zero row belongs between the measured rows.
#[test]
fn include_empty_zones_keeps_unmatched_zones_in_order() {
    let mut c =
        Dataset::new("classes", GeometryType::Polygon, vec![Field::new("kind", FieldType::Text), Field::new("pop", FieldType::Integer)]);
    for x in [0.0, 10.0] {
        c.push_next(rect(x, x + 1.0), vec![Value::Text("forest".into()), Value::Int(100)]);
    }
    let l = layers(zones(), c);
    let mut params = json!({"zones":"zones", "zoneFields":"zone", "classes":"classes", "classFields":"kind", "sumFields":"pop"});
    let default = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(default.messages, ["Tabulate Intersection: 2 rows", "1 zone overlaps no class feature, so it has no rows"]);
    params["includeEmptyZones"] = json!(false);
    let off = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(off.messages, default.messages);
    let default = table(default);
    let off = table(off);
    assert_eq!(rows(&off), rows(&default));

    params["includeEmptyZones"] = json!(true);
    let on = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(on.messages, ["Tabulate Intersection: 3 rows"]);
    let on = table(on);
    assert_eq!(on.features[0].attrs, off.features[0].attrs);
    assert_eq!(on.features[1].attrs, [Value::Text("B".into()), Value::Null, Value::Float(0.0), Value::Float(0.0), Value::Float(0.0)]);
    assert_eq!(on.features[2].attrs, off.features[1].attrs);
}

#[test]
fn include_empty_zones_works_with_an_empty_class_layer() {
    let l = layers(zones(), Dataset::new("classes", GeometryType::Polygon, vec![]));
    let mut params = json!({"zones":"zones", "classes":"classes", "areaUnit":"m2", "includeEmptyZones":false});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(result.messages, ["Tabulate Intersection: 0 rows", "3 zones overlap no class feature, so they have no rows"]);
    assert!(table(result).features.is_empty());

    params["includeEmptyZones"] = json!(true);
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(result.messages, ["Tabulate Intersection: 3 rows"]);
    let out = table(result);
    assert_eq!(out.fields[1].name, "area_m2");
    let attrs: Vec<_> = out.features.into_iter().map(|feature| feature.attrs).collect();
    assert_eq!(attrs, [0, 1, 2].map(|row| vec![Value::Int(row), Value::Float(0.0), Value::Float(0.0)]));
}

/// Every shared area unit converts the same ground area; percentages, totals and default output
/// retain their existing values, including where an area field name collides with a zone field.
#[test]
fn area_units_convert_only_the_area_and_name_its_unit() {
    let l = layers(zones(), classes());
    let params = json!({"zones":"zones", "zoneFields":"zone", "classes":"classes", "sumFields":"pop"});
    let default = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    let messages = default.messages.clone();
    let baseline = table(default);
    let spec = rustgis_analysis::TOOLS.iter().find(|t| t.id == "tabulateIntersection").unwrap();
    let unit = spec.params.iter().find(|p| p.name == "areaUnit").unwrap();
    assert!(!unit.required);
    assert_eq!(unit.default, "\"km2\"");
    let rustgis_analysis::ParamKind::Choice(choices) = unit.kind else { panic!("expected unit choices") };
    assert_eq!(*choices, rustgis_data::Measure::Area.units().iter().map(|(key, _)| *key).collect::<Vec<_>>());
    for &(unit, divisor) in rustgis_data::Measure::Area.units() {
        let mut params = params.clone();
        params["areaUnit"] = json!(unit);
        let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
        assert_eq!(result.messages, messages);
        let out = table(result);
        assert_eq!(out.fields[1].name, format!("area_{unit}"));
        assert_eq!(out.features.len(), baseline.features.len());
        for (got, old) in out.features.iter().zip(baseline.features.iter()) {
            assert_eq!(got.attrs[0], old.attrs[0]);
            assert_eq!(got.attrs[2..], old.attrs[2..]);
            let expected = old.attrs[1].as_f64().unwrap() * 1e6 / divisor;
            let actual = got.attrs[1].as_f64().unwrap();
            assert!((actual - expected).abs() <= expected * 1e-12, "{unit}: {actual} != {expected}");
            if unit == "km2" {
                assert_eq!(got.attrs, old.attrs);
            }
        }
    }
    let mut z = zones();
    z.fields[0].name = "area_ac".into();
    let out = table(
        rustgis_analysis::run(
            "tabulateIntersection",
            &layers(z, classes()),
            &json!({"zones":"zones", "zoneFields":"area_ac", "classes":"classes", "areaUnit":"ac"}),
        )
        .unwrap(),
    );
    assert_eq!(out.fields[0].name, "area_ac");
    assert_eq!(out.fields[1].name, "area_ac_2");
}

/// Null optional units match omission exactly, including the table schema and notices.
#[test]
fn null_area_unit_keeps_default_output() {
    let l = layers(zones(), classes());
    let mut params = json!({"zones":"zones", "zoneFields":"zone", "classes":"classes", "sumFields":"pop"});
    let default = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    params["areaUnit"] = json!(null);
    let null = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(null.messages, default.messages);
    assert_eq!(serde_json::to_value(table(null)).unwrap(), serde_json::to_value(table(default)).unwrap());
}

/// Geographic notices name the numeric area column after collisions with both grouping fields.
#[test]
fn area_notices_use_the_actual_column_name() {
    let polygon = |points: &[(f64, f64)]| Some(Geometry::Polygon(vec![vec![points.iter().map(|&(x, y)| Coord::new(x, y)).collect()]]));
    for (zone, class, notice) in [
        (
            [(179.0, 0.0), (-179.0, 0.0), (-179.0, 1.0), (179.0, 1.0), (179.0, 0.0)],
            [(0.0, 0.2), (0.5, 0.2), (0.5, 0.8), (0.0, 0.8), (0.0, 0.2)],
            "follows the ground",
        ),
        (
            [(0.0, 80.0), (10.0, 80.0), (10.0, 88.0), (0.0, 88.0), (0.0, 80.0)],
            [(0.0, 84.0), (10.0, 84.0), (10.0, 89.0), (0.0, 89.0), (0.0, 84.0)],
            "takes the shortest way on the ground",
        ),
    ] {
        let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("area_ac", FieldType::Text)]);
        z.push_next(polygon(&zone), vec![Value::Text("A".into())]);
        let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("area_ac", FieldType::Text)]);
        c.push_next(polygon(&class), vec![Value::Text("forest".into())]);
        let params = json!({"zones":"zones", "zoneFields":"area_ac", "classes":"classes", "classFields":"area_ac", "areaUnit":"ac"});
        let result = rustgis_analysis::run("tabulateIntersection", &layers(z, c), &params).unwrap();
        assert!(result.messages.iter().any(|m| m.contains(&format!("while area_ac_3 {notice}"))), "{:?}", result.messages);
        let out = table(result);
        assert_eq!(out.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["area_ac", "area_ac_2", "area_ac_3", "percentage"]);
        assert_eq!(out.fields[2].ty, FieldType::Double);
    }
}

/// A fieldless dissolved layer works without a synthetic attribute. Empty lists and strings
/// behave like omission; a reordered selection retains original row identities and order.
#[test]
fn missing_zone_fields_use_individual_input_rows() {
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![]);
    for (x0, x1) in [(0.0, 1.0), (0.0, 2.0), (1.0, 2.0)] {
        z.push_next(rect(x0, x1), vec![]);
    }
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![]);
    c.push_next(rect(0.0, 2.0), vec![]);
    let mut l = layers(z, c);
    let params = json!({"zones":"zones", "classes":"classes"});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(result.messages, ["Tabulate Intersection: 3 rows"]);
    let all = table(result);
    assert_eq!(all.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["zone_row", "area_km2", "percentage"]);
    assert_eq!(all.fields[0].ty, FieldType::Integer);
    assert_eq!(all.features.iter().map(|f| f.attrs[0].clone()).collect::<Vec<_>>(), [Value::Int(0), Value::Int(1), Value::Int(2)]);
    for empty in [json!(null), json!(""), json!([])] {
        let mut params = params.clone();
        params["zoneFields"] = empty;
        let out = table(rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap());
        assert_eq!(*out.features, *all.features);
    }
    l.selected.insert("zones".into(), vec![2, 0, 2]);
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(result.messages, ["Tabulate Intersection: 2 rows"]);
    let out = table(result);
    assert_eq!(out.features.iter().map(|f| f.attrs[0].clone()).collect::<Vec<_>>(), [Value::Int(0), Value::Int(2)]);
    assert_eq!(out.features.iter().map(|f| &f.attrs).collect::<Vec<_>>(), [&all.features[0].attrs, &all.features[2].attrs]);
}

#[test]
fn says_what_it_leaves_out() {
    let mut z = zones();
    z.push_next(None, vec![Value::Text("D".into())]);
    // A geometry with no polygon, as GeoJSON's `"coordinates": []` reads.
    z.push_next(Some(Geometry::Polygon(vec![])), vec![Value::Text("E".into())]);
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("pop", FieldType::Integer)]);
    // Half in B, half outside every zone; then one without geometry.
    c.push_next(rect(1.5, 2.5), vec![Value::Int(100)]);
    c.push_next(None, vec![Value::Int(7)]);
    // A line in a polygon layer: it has geometry, but no polygon.
    c.push_next(Some(Geometry::Line(vec![vec![Coord::new(1.2, 0.2), Coord::new(1.8, 0.8)]])), vec![Value::Int(9)]);
    // A ring collapsed onto a parallel inside B: a polygon with no area.
    let collapsed = [(1.2, 0.5), (1.8, 0.5), (1.5, 0.5), (1.2, 0.5)].map(|(x, y)| Coord::new(x, y)).to_vec();
    c.push_next(Some(Geometry::Polygon(vec![vec![collapsed]])), vec![Value::Int(11)]);
    let l = layers(z, c);
    let params = json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "sumFields": "pop"});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(
        result.messages,
        [
            "Tabulate Intersection: 1 row",
            "2 zone features have no polygon and were left out",
            "3 class features have no polygon and were left out",
            "2 zones overlap no class feature, so they have no rows",
            "1 class feature lies partly or wholly outside the zones: its value outside isn't shared out",
        ]
    );
    let got = rows(&table(result));
    assert_eq!(got.iter().map(|(t, n)| (t.join(""), n[1], n[2])).collect::<Vec<_>>(), [("B".into(), 50.0, 50.0)]);
}

/// A class feature with a long slanted edge, cut by a meridian between two zones: the cut point lies
/// partway along the slanted edge, and its value still shares out in full.
#[test]
fn shares_add_up_where_a_cut_crosses_a_slanted_edge() {
    let square = |x0: f64, x1: f64| {
        let ring = [(x0, 40.0), (x1, 40.0), (x1, 60.0), (x0, 60.0), (x0, 40.0)].map(|(x, y)| Coord::new(x, y)).to_vec();
        Some(Geometry::Polygon(vec![vec![ring]]))
    };
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(square(0.0, 10.0), vec![Value::Text("A".into())]);
    z.push_next(square(10.0, 20.0), vec![Value::Text("B".into())]);
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("pop", FieldType::Integer)]);
    let triangle = [(0.0, 40.0), (20.0, 40.0), (0.0, 60.0), (0.0, 40.0)].map(|(x, y)| Coord::new(x, y)).to_vec();
    c.push_next(Some(Geometry::Polygon(vec![vec![triangle]])), vec![Value::Int(1_000_000)]);
    let l = layers(z, c);
    let params = json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "sumFields": "pop"});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(result.messages, ["Tabulate Intersection: 2 rows"]);
    let out = table(result);
    let pops: Vec<f64> = out.features.iter().filter_map(|f| f.attrs.get(3).and_then(Value::as_f64)).collect();
    assert_eq!(pops.len(), 2);
    let total: f64 = pops.iter().sum();
    assert!((total / 1_000_000.0 - 1.0).abs() < 1e-6, "{pops:?} add up to {total}");
    // Each zone's share is its part of the triangle's area, worked out here by Simpson's rule on the
    // sphere: the slanted edge is straight on the map, so its isometric latitude ψ changes linearly
    // with longitude, and the triangle's area at each longitude runs from 40° up to that edge.
    let psi = |lat: f64| lat.to_radians().tan().asinh();
    let above_40 = |lon: f64| (psi(60.0) + (psi(40.0) - psi(60.0)) * lon / 20.0).tanh() - 40f64.to_radians().sin();
    let simpson = |a: f64, b: f64| {
        let n = 2000;
        let h = (b - a) / n as f64;
        (0..=n)
            .map(|i| {
                above_40(a + h * i as f64)
                    * if i == 0 || i == n {
                        1.0
                    } else if i % 2 == 1 {
                        4.0
                    } else {
                        2.0
                    }
            })
            .sum::<f64>()
            * h
            / 3.0
    };
    let (in_a, in_b) = (simpson(0.0, 10.0), simpson(10.0, 20.0));
    let expected = [in_a, in_b].map(|part| 1_000_000.0 * part / (in_a + in_b));
    for (got, want) in pops.iter().zip(expected) {
        assert!((got / want - 1.0).abs() < 1e-6, "{pops:?}, not {expected:?}");
    }
}

/// Zones that overlap each other give a feature shares adding up to its whole value while half of
/// it lies outside them both: the notice still says so.
#[test]
fn overlapping_zones_still_say_what_lies_outside() {
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(rect(0.0, 1.0), vec![Value::Text("A".into())]);
    z.push_next(rect(0.5, 1.5), vec![Value::Text("B".into())]);
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("pop", FieldType::Integer)]);
    c.push_next(rect(0.0, 2.0), vec![Value::Int(100)]);
    let l = layers(z, c);
    let params = json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "sumFields": "pop"});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(
        result.messages,
        ["Tabulate Intersection: 2 rows", "1 class feature lies partly or wholly outside the zones: its value outside isn't shared out"]
    );
    let got = rows(&table(result));
    assert_eq!(got.iter().map(|(_, n)| n[2]).collect::<Vec<_>>(), [50.0, 50.0]);
}

/// Features lacking values in two sum fields: one notice for each field, and none for a zone and a
/// class feature reaching past 85.05°, which are measured there like anywhere else (#1033).
#[test]
fn one_notice_per_field_for_missing_values() {
    let polar = |x0: f64, x1: f64, y0: f64, y1: f64| {
        let ring = [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)].map(|(x, y)| Coord::new(x, y)).to_vec();
        Some(Geometry::Polygon(vec![vec![ring]]))
    };
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(polar(0.0, 10.0, 80.0, 84.0), vec![Value::Text("A".into())]);
    // A zone reaching 87°N, beyond the class features.
    z.push_next(polar(20.0, 30.0, 80.0, 87.0), vec![Value::Text("B".into())]);
    let mut c = Dataset::new(
        "classes",
        GeometryType::Polygon,
        vec![Field::new("pop", FieldType::Integer), Field::new("income", FieldType::Double)],
    );
    c.push_next(polar(0.0, 5.0, 80.0, 84.0), vec![Value::Null, Value::Null]);
    c.push_next(polar(5.0, 10.0, 80.0, 84.0), vec![Value::Null, Value::Float(1.0)]);
    // Reaches 88°N; its part in the zone, below 84°N, is measured as usual.
    c.push_next(polar(2.0, 3.0, 82.0, 88.0), vec![Value::Int(1), Value::Float(2.0)]);
    let l = layers(z, c);
    let params = json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "sumFields": ["pop", "income"]});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(
        result.messages,
        [
            "Tabulate Intersection: 1 row",
            "1 zone overlaps no class feature, so it has no rows",
            "class features with no value in a sum field are left out of its shares: 2 class features in 'pop', 1 class feature in 'income'",
            "1 zone feature and 1 class feature have an edge beyond 85.05° running east or west: the analysis plane takes it straight in longitude and latitude beyond 85.05°, and their percentages and shares follow the plane, while area_km2 takes the shortest way on the ground, which near a pole can differ widely",
            "1 class feature lies partly or wholly outside the zones: its value outside isn't shared out",
        ]
    );
}

/// Polygons reaching past 85.05°, where the overlay goes on to the poles (#1033): a class feature
/// from 84°N to 89°N shares out 91.42% of its value to a zone from 80°N to 88°N (on the sphere,
/// the part from 84° to 88° over the whole), not all of it as when its part beyond 85.05° had no
/// area; and one wholly beyond, from 86°N to 90°N, has its row in a zone from 80°N to the pole,
/// 16.03% of it, rather than none.
#[test]
fn polygons_past_85_degrees_are_measured_to_the_pole() {
    let polar = |x0: f64, x1: f64, y0: f64, y1: f64| {
        let ring = [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)].map(|(x, y)| Coord::new(x, y)).to_vec();
        Some(Geometry::Polygon(vec![vec![ring]]))
    };
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(polar(0.0, 10.0, 80.0, 88.0), vec![Value::Text("A".into())]);
    z.push_next(polar(20.0, 30.0, 80.0, 90.0), vec![Value::Text("B".into())]);
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("pop", FieldType::Double)]);
    c.push_next(polar(0.0, 10.0, 84.0, 89.0), vec![Value::Float(1000.0)]);
    c.push_next(polar(20.0, 30.0, 86.0, 90.0), vec![Value::Float(500.0)]);
    let l = layers(z, c);
    let params = json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "sumFields": ["pop"]});
    let res = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    // Each has an edge beyond 85.05° running east or west, which the map and the ground take
    // differently: the tool says so.
    let said = "2 zone features and 2 class features have an edge beyond 85.05° running east or west";
    assert!(res.messages.iter().any(|m| m.starts_with(said)), "{:?}", res.messages);
    let got = rows(&table(res));
    // zone, then area_km2, percentage, pop.
    assert_eq!(got.len(), 2, "{got:?}");
    let (a, b) = (&got[0], &got[1]);
    assert_eq!((a.0[0].as_str(), b.0[0].as_str()), ("A", "B"));
    // The parts' areas on WGS 84, as polygons of geodesic edges (GeographicLib 2.1 gives 34,625.271
    // and 17,323.559 km²), not 0 for the stretch beyond 85.05°.
    assert!((a.1[0] - 34_625.271).abs() < 0.01 && (b.1[0] - 17_323.559).abs() < 0.01, "{a:?} {b:?}");
    assert!((a.1[1] - 33.388).abs() < 0.01 && (a.1[2] - 914.216).abs() < 0.1, "{a:?}");
    assert!((b.1[1] - 16.034).abs() < 0.01 && (b.1[2] - 500.0).abs() < 1e-6, "{b:?}");
    // Within ±85.05°, or beyond it only along meridians, nothing of the kind.
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(polar(0.0, 10.0, 70.0, 85.0), vec![Value::Text("A".into())]);
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("pop", FieldType::Double)]);
    c.push_next(polar(0.0, 10.0, 75.0, 85.0), vec![Value::Float(1000.0)]);
    let res = rustgis_analysis::run("tabulateIntersection", &layers(z, c), &params).unwrap();
    assert!(res.messages.iter().all(|m| !m.contains("beyond 85.05°")), "{:?}", res.messages);
}

/// Features of about 10 m in zones of half a degree: the overlay rounds their parts to a grid
/// scaled to the zones, so a feature wholly inside a zone still counts whole, and one across the
/// border between two zones shares its value out in full within that rounding, with no notice.
#[test]
fn small_features_in_large_zones_share_out_in_full() {
    let sq = |x0: f64, y0: f64, w: f64, h: f64| {
        let ring = [(x0, y0), (x0 + w, y0), (x0 + w, y0 + h), (x0, y0 + h), (x0, y0)].map(|(x, y)| Coord::new(x, y)).to_vec();
        Some(Geometry::Polygon(vec![vec![ring]]))
    };
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(sq(-90.3, 34.9, 0.5, 1.0), vec![Value::Text("A".into())]);
    z.push_next(sq(-89.8, 34.9, 0.5, 1.0), vec![Value::Text("B".into())]);
    let mut c =
        Dataset::new("classes", GeometryType::Polygon, vec![Field::new("id", FieldType::Integer), Field::new("pop", FieldType::Integer)]);
    for i in 0..40 {
        // Twenty wholly inside A, then twenty across the border at 89.8°W.
        let x = if i < 20 { -90.2 + 0.0173 * f64::from(i) } else { -89.80005 + 0.000001 * f64::from(i - 20) };
        c.push_next(sq(x, 35.0 + 0.0211 * f64::from(i), 0.00009, 0.00009), vec![Value::Int(i64::from(i)), Value::Int(1_000_000)]);
    }
    let l = layers(z, c);
    let params = json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "classFields": "id", "sumFields": "pop"});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(result.messages, ["Tabulate Intersection: 60 rows"]);
    let mut shared: HashMap<i64, f64> = HashMap::new();
    for f in &table(result).features {
        let (Some(Value::Int(id)), Some(pop)) = (f.attrs.get(1), f.attrs.get(4).and_then(Value::as_f64)) else { panic!("{:?}", f.attrs) };
        *shared.entry(*id).or_default() += pop;
    }
    for (id, pop) in shared {
        if id < 20 {
            assert_eq!(pop, 1_000_000.0, "feature {id} wholly inside A");
        } else {
            assert!((pop / 1_000_000.0 - 1.0).abs() < 1e-3, "feature {id} across the border shares {pop}");
        }
    }
}

/// A strip about 1 cm wide and 2° long: the overlay's rounding of so thin a feature is large next to
/// its area, yet half outside a zone it is reported and shares out half, and wholly inside one it
/// counts whole.
#[test]
fn a_thin_strip_shares_out_half_when_half_outside_and_all_when_inside() {
    let rect = |x0: f64, y0: f64, x1: f64, y1: f64| {
        let ring = [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)].map(|(x, y)| Coord::new(x, y)).to_vec();
        Some(Geometry::Polygon(vec![vec![ring]]))
    };
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(rect(-91.0, 34.0, -90.0, 36.0), vec![Value::Text("A".into())]);
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("pop", FieldType::Integer)]);
    c.push_next(rect(-91.0, 35.0, -89.0, 35.0000001), vec![Value::Int(1_000_000)]);
    let l = layers(z, c);
    let params = json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "sumFields": "pop"});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    // Half outside, and so thin that rounding moves its area by more than 1%: both are said.
    assert_eq!(
        result.messages,
        [
            "Tabulate Intersection: 1 row",
            "1 class feature is so narrow next to its zones that the overlay's rounding can move its area by more than 1%: its parts are measured only roughly",
            "1 class feature lies partly or wholly outside the zones: its value outside isn't shared out",
        ]
    );
    let pop = table(result).features.first().and_then(|f| f.attrs.get(3)).and_then(Value::as_f64).unwrap();
    // About half: so thin a strip is rounded by about a percent, but not taken as wholly inside.
    assert!((pop / 500_000.0 - 1.0).abs() < 0.05, "{pop}");

    // The same strip wholly inside one zone, and half inside another overlapping it: whole in the
    // first, but the second's part is cut and rounded, which the notice still says.
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(rect(-91.0, 34.0, -88.0, 36.0), vec![Value::Text("A".into())]);
    z.push_next(rect(-89.5, 34.0, -88.0, 36.0), vec![Value::Text("B".into())]);
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("pop", FieldType::Integer)]);
    c.push_next(rect(-90.5, 35.0, -88.5, 35.0000001), vec![Value::Int(1_000_000)]);
    let result = rustgis_analysis::run("tabulateIntersection", &layers(z, c), &params).unwrap();
    assert!(result.messages.iter().any(|m| m.starts_with("1 class feature is so narrow next to its zones")), "{:?}", result.messages);
    // A holds it whole; B gets the half from 89.5°W, roughly measured.
    let pops: Vec<f64> = table(result).features.iter().filter_map(|f| f.attrs.get(3).and_then(Value::as_f64)).collect();
    assert_eq!(pops.first(), Some(&1_000_000.0), "{pops:?}");
    assert!(pops.get(1).is_some_and(|b| (b / 500_000.0 - 1.0).abs() < 0.05), "{pops:?}");

    // The same strip wholly inside the zone counts whole, unrounded, with no notice.
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(rect(-91.0, 34.0, -88.0, 36.0), vec![Value::Text("A".into())]);
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("pop", FieldType::Integer)]);
    c.push_next(rect(-90.5, 35.0, -88.5, 35.0000001), vec![Value::Int(1_000_000)]);
    let result = rustgis_analysis::run("tabulateIntersection", &layers(z, c), &params).unwrap();
    assert_eq!(result.messages, ["Tabulate Intersection: 1 row"]);
    let pop = table(result).features.first().and_then(|f| f.attrs.get(3)).and_then(Value::as_f64).unwrap();
    assert_eq!(pop, 1_000_000.0);
}

/// A feature of about 10 m in a zone 10° wide, with 0.15% of its area across the zone's edge: more
/// than the overlay's rounding of it, so it is reported as partly outside rather than counted whole.
#[test]
fn a_small_feature_just_across_a_zone_edge_is_reported() {
    let rect = |x0: f64, y0: f64, x1: f64, y1: f64| {
        let ring = [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)].map(|(x, y)| Coord::new(x, y)).to_vec();
        Some(Geometry::Polygon(vec![vec![ring]]))
    };
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(rect(-100.0, 30.0, -90.0, 40.0), vec![Value::Text("A".into())]);
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("pop", FieldType::Integer)]);
    let (side, across) = (0.00009, 0.00009 * 0.0015);
    c.push_next(rect(-90.0 - side + across, 35.0, -90.0 + across, 35.0 + side), vec![Value::Int(1_000_000)]);
    let l = layers(z, c);
    let params = json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "sumFields": "pop"});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(
        result.messages,
        ["Tabulate Intersection: 1 row", "1 class feature lies partly or wholly outside the zones: its value outside isn't shared out"]
    );
    // The 99.85% inside the zone.
    let pop = table(result).features.first().and_then(|f| f.attrs.get(3)).and_then(Value::as_f64).unwrap();
    assert!((pop / 998_500.0 - 1.0).abs() < 1e-4, "{pop}");
}

/// Slivers 1 m long and 0.5 to 5 mm wide across the border of two zones 5° wide, so narrow that
/// the overlay's rounding moves their areas by more than 1%: never taken as whole in both zones,
/// and a notice says they are measured only roughly.
#[test]
fn slivers_too_narrow_to_measure_finely_are_reported_not_counted_twice() {
    let rect = |x0: f64, y0: f64, x1: f64, y1: f64| {
        let ring = [(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)].map(|(x, y)| Coord::new(x, y)).to_vec();
        Some(Geometry::Polygon(vec![vec![ring]]))
    };
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(rect(-100.0, 30.0, -95.0, 40.0), vec![Value::Text("A".into())]);
    z.push_next(rect(-95.0, 30.0, -90.0, 40.0), vec![Value::Text("B".into())]);
    let params = json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "sumFields": "pop"});
    // From 0.5 mm wide, about half a grid step there, to 5 mm.
    for width_m in [0.0005, 0.001, 0.002, 0.003, 0.005] {
        let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("pop", FieldType::Integer)]);
        c.push_next(rect(-95.0000045, 35.0, -94.9999955, 35.0 + width_m / 111_000.0), vec![Value::Int(1_000_000)]);
        let result = rustgis_analysis::run("tabulateIntersection", &layers(z.clone(), c), &params).unwrap();
        let rough = result.messages.iter().any(|m| m.starts_with("1 class feature is so narrow next to its zones"));
        assert!(rough, "{width_m} m: {:?}", result.messages);
        let total: f64 = table(result).features.iter().filter_map(|f| f.attrs.get(3).and_then(Value::as_f64)).sum();
        assert!(total <= 1_000_000.0 * 1.01, "{width_m} m wide shared out {total}");
    }
    // The narrowest wholly inside A: whatever the cut leaves of it, A covers it, so it counts whole.
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("pop", FieldType::Integer)]);
    c.push_next(rect(-96.0000045, 35.0, -95.9999955, 35.0 + 0.0005 / 111_000.0), vec![Value::Int(1_000_000)]);
    let result = rustgis_analysis::run("tabulateIntersection", &layers(z, c), &params).unwrap();
    assert_eq!(result.messages, ["Tabulate Intersection: 1 row", "1 zone overlaps no class feature, so it has no rows"]);
    let pop = table(result).features.first().and_then(|f| f.attrs.get(3)).and_then(Value::as_f64);
    assert_eq!(pop, Some(1_000_000.0));
}

/// A thin feature in the notch of an L-shaped zone: within the zone's extent but touching none of
/// it, so it has no parts to measure, roughly or not, and no sum fields to share out.
#[test]
fn a_thin_feature_near_a_zone_only_by_its_extent_gets_no_notice() {
    let ring = |pts: &[(f64, f64)]| Some(Geometry::Polygon(vec![vec![pts.iter().map(|&(x, y)| Coord::new(x, y)).collect()]]));
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(ring(&[(0.0, 0.0), (2.0, 0.0), (2.0, 1.0), (1.0, 1.0), (1.0, 2.0), (0.0, 2.0), (0.0, 0.0)]), vec![Value::Text("L".into())]);
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("kind", FieldType::Text)]);
    c.push_next(ring(&[(1.5, 1.5), (1.9, 1.5), (1.9, 1.50000001), (1.5, 1.50000001), (1.5, 1.5)]), vec![Value::Text("river".into())]);
    let l = layers(z, c);
    let result =
        rustgis_analysis::run("tabulateIntersection", &l, &json!({"zones": "zones", "zoneFields": "zone", "classes": "classes"})).unwrap();
    assert_eq!(result.messages, ["Tabulate Intersection: 0 rows", "1 zone overlaps no class feature, so it has no rows"]);
}

/// A zone stored with a jump across the antimeridian, unsplit: the map draws it the long way round,
/// so a class feature at 0° falls in it, and a notice says its percentages follow the map.
#[test]
fn a_zone_crossing_the_antimeridian_unsplit_is_reported() {
    let ring = |pts: &[(f64, f64)]| Some(Geometry::Polygon(vec![vec![pts.iter().map(|&(x, y)| Coord::new(x, y)).collect()]]));
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(ring(&[(179.0, 0.0), (-179.0, 0.0), (-179.0, 1.0), (179.0, 1.0), (179.0, 0.0)]), vec![Value::Text("A".into())]);
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![Field::new("kind", FieldType::Text)]);
    c.push_next(ring(&[(0.0, 0.2), (0.5, 0.2), (0.5, 0.8), (0.0, 0.8), (0.0, 0.2)]), vec![Value::Text("x".into())]);
    let l = layers(z, c);
    let params = json!({"zones": "zones", "zoneFields": "zone", "classes": "classes"});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(
        result.messages,
        [
            "Tabulate Intersection: 1 row",
            "1 zone feature crosses the antimeridian without being split there: the map draws them the long way round, and their percentages and shares follow the map, while area_km2 follows the ground",
        ]
    );
    let mut params = params;
    params["areaUnit"] = json!("ac");
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert!(result.messages.iter().any(|m| m.contains("while area_ac follows the ground")));
}

#[test]
fn zones_sharing_a_value_are_one_zone() {
    let mut z = zones();
    for f in &mut z.features {
        f.attrs[0] = Value::Text("all".into());
    }
    let l = layers(z, classes());
    let params = json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "classFields": "kind"});
    let result = rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap();
    assert_eq!(result.messages, ["Tabulate Intersection: 2 rows"]);
    // The forests cover 1° of the zone's 3°, the water half a degree.
    let got = rows(&table(result));
    assert_eq!(
        got.iter().map(|(t, n)| (t.join(" "), n[1])).collect::<Vec<_>>(),
        [("all forest".into(), 33.333), ("all water".into(), 16.667)]
    );
}

#[test]
fn refuses_inputs_it_cannot_tabulate() {
    let mut lines = Dataset::new("lines", GeometryType::Polyline, vec![Field::new("zone", FieldType::Text)]);
    lines.push_next(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]])), vec![Value::Text("A".into())]);
    let mut l = layers(zones(), classes());
    l.data.insert("lines".into(), lines);
    let run = |params: serde_json::Value| rustgis_analysis::run("tabulateIntersection", &l, &params).unwrap_err();
    assert_eq!(run(json!({"zones": "zones", "zoneFields": "zone", "classes": "lines"})), "'lines' is not a polygon layer");
    assert_eq!(
        run(json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "sumFields": "kind"})),
        "the sum field 'kind' is not numeric"
    );
    assert_eq!(
        run(json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "classFields": "pop", "sumFields": "pop"})),
        "the sum field 'pop' can't also be a class field"
    );
}

/// Issue #1966: two touching class boundaries leave tiny interior rings in the combined cut.
/// The area stays near the sum of the individual cuts, without a warning for valid input.
#[test]
fn touching_classes_keep_a_positive_ground_area() {
    let polygon = |points: &[(f64, f64)]| Some(Geometry::Polygon(vec![vec![points.iter().map(|&(x, y)| Coord::new(x, y)).collect()]]));
    let mut z = Dataset::new("zones", GeometryType::Polygon, vec![Field::new("zone", FieldType::Text)]);
    z.push_next(
        polygon(&[(-89.73323, 35.02434), (-89.73186, 35.03686), (-89.72338, 35.02649), (-89.73323, 35.02434)]),
        vec![Value::Text("zone".into())],
    );
    let mut c = Dataset::new("classes", GeometryType::Polygon, vec![]);
    c.push_next(
        polygon(&[
            (-89.72515, 35.02759),
            (-89.72513, 35.02759),
            (-89.72513, 35.0276),
            (-89.72512, 35.0276),
            (-89.72511, 35.0275),
            (-89.72515, 35.02759),
        ]),
        vec![],
    );
    c.push_next(
        polygon(&[
            (-89.69212, 35.03669),
            (-89.72512, 35.0276),
            (-89.72513, 35.0276),
            (-89.72513, 35.02759),
            (-89.72515, 35.02759),
            (-89.73252, 35.02294),
            (-89.73463, 35.02829),
            (-89.69213, 35.03686),
            (-89.69212, 35.03669),
        ]),
        vec![],
    );
    let result = rustgis_analysis::run(
        "tabulateIntersection",
        &layers(z, c),
        &json!({"zones": "zones", "zoneFields": "zone", "classes": "classes"}),
    )
    .unwrap();
    assert_eq!(result.messages, ["Tabulate Intersection: 1 row"]);
    let out = table(result);
    assert_eq!(out.features.len(), 1);
    let area = out.value(0, out.field_index("area_km2").unwrap()).as_f64().unwrap();
    let percentage = out.value(0, out.field_index("percentage").unwrap()).as_f64().unwrap();
    assert!((area - 0.29858434).abs() < 1e-5, "area_km2 = {area}");
    assert!((percentage - 48.99970001).abs() < 1e-5, "percentage = {percentage}");
}
