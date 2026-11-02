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

/// A class feature reaching beyond the map's latitude limit, and features lacking values in two sum
/// fields: one notice for each kind.
#[test]
fn one_notice_per_kind_for_missing_values_and_the_latitude_limit() {
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
            "1 zone feature and 1 class feature reach beyond 85.05° latitude, where the map ends: parts there are taken as lying along it, with no area",
            "1 class feature lies partly or wholly outside the zones: its value outside isn't shared out",
        ]
    );
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
    let result = rustgis_analysis::run(
        "tabulateIntersection",
        &layers(z, c),
        &json!({"zones": "zones", "zoneFields": "zone", "classes": "classes"}),
    )
    .unwrap();
    assert_eq!(
        result.messages,
        [
            "Tabulate Intersection: 1 row",
            "1 zone feature crosses the antimeridian without being split there: the map draws them the long way round, and their percentages and shares follow the map, while area_km2 follows the ground",
        ]
    );
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
    assert_eq!(run(json!({"zones": "zones", "zoneFields": [], "classes": "classes"})), "'Zone Fields' is required");
    assert_eq!(
        run(json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "sumFields": "kind"})),
        "the sum field 'kind' is not numeric"
    );
    assert_eq!(
        run(json!({"zones": "zones", "zoneFields": "zone", "classes": "classes", "classFields": "pop", "sumFields": "pop"})),
        "the sum field 'pop' can't also be a class field"
    );
}
