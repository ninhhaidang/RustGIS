//! Runs the analysis tools on the layers in `reference/layers` and compares what they give with the
//! answers in `reference/cases.json`, worked out by GEOS (the geometry engine of QGIS and PostGIS)
//! with `reference/generate.py`. Every case runs and every difference is reported together.
//!
//! A case marked `known` fails for a reason with an open issue. It must still agree within the
//! mark's looser tolerance, so it fails the test only if it gets worse; its differences are printed
//! (`cargo test --test reference -- --nocapture` shows them). Once it agrees within the case's own
//! tolerance, the test fails until the mark is removed from `generate.py`.
//!
//! When a case fails, fix the tool with RustGIS's own code. Don't translate GEOS's source to make
//! it pass: GEOS is LGPL-2.1, and RustGIS is Apache-2.0 with no third-party code.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use geo::{Area, BooleanOps, Euclidean, HausdorffDistance, Length, MultiLineString, MultiPolygon};
use rustgis_analysis::convert::{to_multiline, to_multipolygon};
use rustgis_analysis::{Inputs, Output};
use rustgis_data::{Dataset, Geometry, Value};
use serde_json::Value as Json;

/// Polygons may differ by this fraction of their area (0.1 ppm: 160 m² of a 40 km square).
const AREA_TOLERANCE: f64 = 1e-7;
/// Lines may differ in length, and sit, this far (Web Mercator metres) from the reference.
const POSITION_TOLERANCE_M: f64 = 1e-3;
/// Distances may differ by this fraction, plus a millimetre. The reference measures to the WGS 84
/// geodesic between an edge's vertices; the join finds the nearest points in an azimuthal
/// equidistant plane (see `join.rs`), whose straight edges are within this of the geodesics.
const DISTANCE_TOLERANCE: f64 = 1e-4;

/// The reference corpus: the layers, the expected results and `cases.json`.
fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/reference")
}

/// Reads a GeoJSON file as a layer named `name`, the name the cases' parameters refer to it by.
fn read_layer(path: &Path, name: &str) -> Dataset {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    rustgis_data::io::read_geojson(&text, name).unwrap_or_else(|e| panic!("{}: {e}", path.display())).0
}

/// The reference layers by name, none with a selection: every tool works on all their features.
struct Layers(HashMap<String, Dataset>);

impl Inputs for Layers {
    fn dataset(&self, layer: &str) -> Option<&Dataset> {
        self.0.get(layer)
    }
    fn selection(&self, _: &str) -> &[usize] {
        &[]
    }
}

/// A field's value as text, for matching features by key (`None` for a missing field or null).
fn text(ds: &Dataset, row: usize, field: &str) -> Option<String> {
    let fi = ds.field_index(field)?;
    match ds.value(row, fi) {
        Value::Null => None,
        Value::Text(t) => Some(t.clone()),
        v => Some(v.to_string()),
    }
}

/// A key from `cases.json` as text, so it compares with [`text`] whether written as a string or a number.
fn json_text(v: &Json) -> Option<String> {
    match v {
        Json::Null => None,
        Json::String(s) => Some(s.clone()),
        v => Some(v.to_string()),
    }
}

/// A feature's key: its key fields' values as text, joined by `|` as `generate.py` writes a key of
/// several fields ("P1|Z1"); `"all"` when there are no key fields.
fn key_text(ds: &Dataset, row: usize, fields: Option<&[String]>) -> String {
    match fields {
        Some(fields) => fields.iter().map(|f| text(ds, row, f).unwrap_or_default()).collect::<Vec<_>>().join("|"),
        None => "all".into(),
    }
}

/// The fields a case's output is keyed by: its `key`, one field or a list; `id` without one; none
/// when it is null.
fn key_fields(expect: &Json) -> Option<Vec<String>> {
    match expect.get("key") {
        Some(Json::Null) => None,
        Some(Json::Array(fields)) => Some(fields.iter().filter_map(|f| f.as_str().map(String::from)).collect()),
        Some(Json::String(field)) => Some(vec![field.clone()]),
        _ => Some(vec!["id".into()]),
    }
}

/// The features of a layer by key (see [`key_text`]).
fn by_key(ds: &Dataset, key: Option<&[String]>) -> BTreeMap<String, Vec<Geometry>> {
    let mut out: BTreeMap<String, Vec<Geometry>> = BTreeMap::new();
    for (row, f) in ds.features.iter().enumerate() {
        out.entry(key_text(ds, row, key)).or_default().extend(f.geometry.clone());
    }
    out
}

/// The polygons among `gs`, together, in Web Mercator metres.
fn polygons(gs: &[Geometry]) -> MultiPolygon<f64> {
    MultiPolygon::new(gs.iter().filter_map(to_multipolygon).flat_map(|m| m.0).collect())
}

/// The lines among `gs`, together, in the analysis plane.
fn lines(gs: &[Geometry]) -> MultiLineString<f64> {
    MultiLineString::new(gs.iter().filter_map(to_multiline).flat_map(|m| m.0).collect())
}

/// The kinds of geometry among `gs`, by name, in order and without repeats.
fn kinds(gs: &[Geometry]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = gs
        .iter()
        .map(|g| match g {
            Geometry::Point(_) | Geometry::MultiPoint(_) => "point",
            Geometry::Line(_) => "line",
            Geometry::Polygon(_) => "polygon",
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// How one geometry differs from the reference, if it does beyond the tolerances; `area_tolerance`
/// is the fraction of a polygon's area the two may not share.
fn compare_geometry(got: &[Geometry], want: &[Geometry], area_tolerance: f64) -> Option<String> {
    // The comparisons below look only at the reference's kind of geometry, so another kind in the
    // output would go unseen.
    if kinds(got) != kinds(want) {
        return Some(format!("{:?} geometry, the reference {:?}", kinds(got), kinds(want)));
    }
    if want.iter().any(|g| matches!(g, Geometry::Polygon(_))) {
        let (g, w) = (polygons(got), polygons(want));
        // The area the two don't share, not vertex positions: GEOS keeps vertices partway along a
        // straight edge that other engines drop, with the same shape.
        let (area, off) = (w.unsigned_area(), g.xor(&w).unsigned_area());
        (off > area_tolerance * area)
            .then(|| format!("area {:.1} m² vs {area:.1} m²; {off:.1} m² ({:.2e} of it) not shared", g.unsigned_area(), off / area))
    } else {
        let (g, w) = (lines(got), lines(want));
        let (gl, wl) = (Euclidean.length(&g), Euclidean.length(&w));
        let far = if g.0.is_empty() { f64::INFINITY } else { g.hausdorff_distance(&w) };
        ((gl - wl).abs() > POSITION_TOLERANCE_M || far > POSITION_TOLERANCE_M)
            .then(|| format!("length {gl:.3} m vs {wl:.3} m; up to {far:.3} m apart"))
    }
}

/// The dataset a tool's output carries, whether a new layer, an updated one or a table.
fn layer_of(output: Output) -> Option<Dataset> {
    match output {
        Output::Layer(ds) | Output::Update { dataset: ds, .. } | Output::Table(ds) => Some(ds),
        _ => None,
    }
}

/// Checks one case; the differences found, empty when it matches.
fn check(case: &Json, layers: &Layers) -> Vec<String> {
    let tool = case["tool"].as_str().unwrap_or_default();
    let expect = &case["expect"];
    let result = match rustgis_analysis::run(tool, layers, &case["params"]) {
        Ok(r) => r,
        Err(e) => return vec![format!("the tool failed: {e}")],
    };
    let mut problems = Vec::new();

    if let Some(path) = expect["features"].as_str() {
        let Some(ds) = layer_of(result.output) else { return vec!["no layer came out".into()] };
        let key = key_fields(expect);
        // The reference writes every key, of however many fields, as its `id`.
        let id = ["id".to_string()];
        let want = by_key(&read_layer(&dir().join(path), "expected"), key.as_ref().map(|_| id.as_slice()));
        let got = by_key(&ds, key.as_deref());
        // A case may loosen the area tolerance, for a tool that approximates curves (Buffer).
        let tolerance = expect["tolerance"].as_f64().unwrap_or(AREA_TOLERANCE);
        for (k, w) in &want {
            match got.get(k) {
                None => problems.push(format!("{k}: missing (the reference has it)")),
                Some(g) => problems.extend(compare_geometry(g, w, tolerance).map(|d| format!("{k}: {d}"))),
            }
        }
        for k in got.keys().filter(|k| !want.contains_key(*k)) {
            problems.push(format!("{k}: present (the reference has nothing)"));
        }
        if let Some(counts) = expect["counts"].as_object() {
            for (row, _) in ds.features.iter().enumerate() {
                // The same key as `by_key` gives the feature.
                let k = key_text(&ds, row, key.as_deref());
                let (n, want) = (text(&ds, row, "count"), counts.get(&k).and_then(json_text));
                if n != want {
                    problems.push(format!("{k}: count {n:?}, the reference {want:?}"));
                }
            }
        }
    } else if let Some(ids) = expect["ids"].as_array() {
        let want: Vec<String> = ids.iter().filter_map(json_text).collect();
        let got: Vec<String> = match result.output {
            Output::Selection { layer, rows } => match layers.0.get(&layer) {
                Some(ds) => {
                    let key = key_fields(expect);
                    rows.iter().map(|r| key_text(ds, *r, key.as_deref())).collect()
                }
                None => return vec![format!("a selection in '{layer}', which isn't a reference layer")],
            },
            other => match layer_of(other) {
                Some(ds) => {
                    let key = key_fields(expect);
                    (0..ds.features.len()).map(|r| key_text(&ds, r, key.as_deref())).collect()
                }
                None => return vec!["neither a selection nor a layer came out".into()],
            },
        };
        if got != want {
            problems.push(format!("gave {got:?}, the reference {want:?}"));
        }
    } else if let Some(values) = expect["values"].as_object() {
        let Some(ds) = layer_of(result.output) else { return vec!["no layer came out".into()] };
        let field = expect["field"].as_str().unwrap_or_default();
        // Every feature once: a missing, extra or repeated one fails even when the counts agree.
        let mut ids: Vec<String> = (0..ds.features.len()).map(|row| text(&ds, row, "id").unwrap_or_default()).collect();
        let mut want_ids: Vec<String> = values.keys().cloned().collect();
        ids.sort();
        want_ids.sort();
        if ids != want_ids {
            problems.push(format!("features {ids:?}, the reference {want_ids:?}"));
        }
        for row in 0..ds.features.len() {
            let id = text(&ds, row, "id").unwrap_or_default();
            let (got, want) = (text(&ds, row, field), values.get(&id).and_then(json_text));
            if got != want {
                problems.push(format!("{id}: {field} {got:?}, the reference {want:?}"));
            }
        }
    } else if let Some(numbers) = expect["numbers"].as_object() {
        // Measurements per feature by id, each within a fraction `tolerance` of the reference.
        let Some(ds) = layer_of(result.output) else { return vec!["no layer came out".into()] };
        let tolerance = expect["tolerance"].as_f64().unwrap_or(0.0);
        let mut ids: Vec<String> = (0..ds.features.len()).map(|row| text(&ds, row, "id").unwrap_or_default()).collect();
        let mut want_ids: Vec<String> = numbers.keys().cloned().collect();
        ids.sort();
        want_ids.sort();
        if ids != want_ids {
            problems.push(format!("features {ids:?}, the reference {want_ids:?}"));
        }
        for row in 0..ds.features.len() {
            let id = text(&ds, row, "id").unwrap_or_default();
            let Some(fields) = numbers.get(&id).and_then(Json::as_object) else {
                problems.push(format!("{id}: no measurements in the reference"));
                continue;
            };
            for (field, want) in fields {
                let want = want.as_f64().unwrap_or(f64::NAN);
                let got = ds.field_index(field).and_then(|fi| ds.value(row, fi).as_f64());
                match got {
                    Some(got) if (got - want).abs() <= tolerance * want.abs() => {}
                    got => problems.push(format!("{id}: {field} {got:?}, the reference {want} (±{:.1e})", tolerance * want.abs())),
                }
            }
        }
    } else if let Some(pairs) = expect["pairs"].as_array() {
        let Some(ds) = layer_of(result.output) else { return vec!["no layer came out".into()] };
        // Per target, its joined features in order, each with its distance when one is written.
        type Joined = BTreeMap<String, Vec<(Option<String>, Option<f64>)>>;
        let mut want = Joined::new();
        for p in pairs {
            let d = p.get(2).and_then(Json::as_f64);
            want.entry(json_text(&p[0]).unwrap_or_default()).or_default().push((json_text(&p[1]), d));
        }
        let mut got = Joined::new();
        let dist = ds.field_index("dist");
        for row in 0..ds.features.len() {
            let d = dist.and_then(|fi| ds.value(row, fi).as_f64());
            got.entry(text(&ds, row, "id").unwrap_or_default()).or_default().push((text(&ds, row, "join_id"), d));
        }
        // Order matters only for Within a distance (nearest first).
        let ordered = case["params"]["match"] == "Within a distance";
        for (t, w) in &want {
            let mut g = got.get(t).cloned().unwrap_or_default();
            let mut w = w.clone();
            if !ordered {
                g.sort_by(|a, b| a.0.cmp(&b.0));
                w.sort_by(|a, b| a.0.cmp(&b.0));
            }
            let ids = |v: &[(Option<String>, Option<f64>)]| v.iter().map(|p| p.0.clone()).collect::<Vec<_>>();
            if ids(&g) != ids(&w) {
                problems.push(format!("{t}: joined {:?}, the reference {:?}", ids(&g), ids(&w)));
                continue;
            }
            for ((j, gd), (_, wd)) in g.iter().zip(&w) {
                let j = j.clone().unwrap_or_default();
                match (gd, wd) {
                    (Some(gd), Some(wd)) if (gd - wd).abs() > DISTANCE_TOLERANCE * wd + 1e-3 => {
                        problems.push(format!("{t}→{j}: {gd:.4} m, the reference {wd:.4} m"));
                    }
                    // A distance where the reference has none, or none where it has one.
                    (Some(_), None) | (None, Some(_)) => {
                        problems.push(format!("{t}→{j}: distance {gd:?}, the reference {wd:?}"));
                    }
                    _ => {}
                }
            }
        }
        for t in got.keys().filter(|t| !want.contains_key(*t)) {
            problems.push(format!("{t}: present (the reference has no such target)"));
        }
    } else {
        problems.push("the case says nothing to expect".into());
    }
    problems
}

#[test]
fn tools_agree_with_the_reference() {
    let path = dir().join("cases.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let cases: Json = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let layers = Layers(
        ["parcels", "zones", "points", "roads", "stations", "long"]
            .into_iter()
            .map(|n| (n.to_string(), read_layer(&dir().join(format!("layers/{n}.geojson")), n)))
            .collect(),
    );
    let mut report = Vec::new();
    for case in cases["cases"].as_array().expect("cases.json has a \"cases\" list") {
        let name = case["name"].as_str().unwrap_or("?");
        let problems = check(case, &layers);
        let known = &case["known"];
        match (known["issue"].as_str(), problems.is_empty()) {
            (Some(issue), false) => {
                // Within the known bound: the case as written, with the mark's tolerance.
                let mut loose = case.clone();
                loose["expect"]["tolerance"] = known["tolerance"].clone();
                let worse = check(&loose, &layers);
                if worse.is_empty() {
                    eprintln!("{name}: still differs ({issue}):\n    {}", problems.join("\n    "));
                } else {
                    report.push(format!("{name}: differs beyond its known bound ({issue}):\n    {}", worse.join("\n    ")));
                }
            }
            (Some(issue), true) => report.push(format!("{name}: agrees now; remove its `known` mark ({issue}) from generate.py")),
            (None, false) => report.push(format!("{name}:\n    {}", problems.join("\n    "))),
            (None, true) => {}
        }
    }
    let n = cases["cases"].as_array().map_or(0, Vec::len);
    assert!(report.is_empty(), "{} of {n} cases differ from the reference:\n{}", report.len(), report.join("\n"));
}
