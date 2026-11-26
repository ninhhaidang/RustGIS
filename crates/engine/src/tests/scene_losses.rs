use super::{mesh_feature, mesh_session, square};
use crate::{Project, Session};
use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value as Attr};
use serde_json::{Value, json};

/// Export through the command, removing the temporary file before checking its contents.
fn scene(s: &mut Session, tag: &str) -> Value {
    let path = std::env::temp_dir().join(format!("rustgis-scene-losses-{tag}-{}.gltf", std::process::id()));
    let result = s.run("share.webScene", &json!({"path":path}));
    let bytes = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    result.unwrap();
    serde_json::from_slice(&bytes.unwrap()).unwrap()
}

/// Count exported positions through the primitives, independently of accessor ordering.
fn positions(doc: &Value) -> u64 {
    doc["meshes"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["primitives"].as_array().unwrap())
        .map(|p| {
            let i = p["attributes"]["POSITION"].as_u64().unwrap() as usize;
            doc["accessors"][i]["count"].as_u64().unwrap()
        })
        .sum()
}

#[test]
fn web_scene_reports_degenerate_parts_and_preserves_clean_geometry() {
    let short = json!({"type":"Polygon", "coordinates":[[[0.0,0.0],[0.01,0.01],[0.0,0.0]]]});
    let hole = json!({"type":"Polygon", "coordinates":[
        [[0.0,0.0],[0.01,0.0],[0.01,0.01],[0.0,0.01],[0.0,0.0]],
        [[0.002,0.002],[0.003,0.003],[0.002,0.002]]
    ]});
    let line = json!({"type":"LineString", "coordinates":[[0.0,0.0]]});
    let mut s = mesh_session(json!([
        mesh_feature(short.clone(), 30.0),
        mesh_feature(hole, 30.0),
        mesh_feature(line, 30.0),
        mesh_feature(square(0.02, false), 30.0)
    ]));
    let doc = scene(&mut s, "parts");
    for expected in [
        "Skipped 1 polygon(s) whose outer ring has fewer than 3 vertices",
        "Dropped 1 hole(s) with fewer than 3 vertices",
        "Skipped 1 line part(s) with a single vertex",
    ] {
        assert_eq!(s.notices.iter().filter(|n| n.text == expected).count(), 1, "{expected}: {:?}", s.notices);
    }
    assert_eq!(positions(&doc), 8, "the two valid squares, with the short hole omitted");
    let mut clean = mesh_session(json!([mesh_feature(square(0.0, false), 30.0)]));
    assert_eq!(positions(&scene(&mut clean, "clean-parts")), 4);
    assert!(!clean.notices.iter().any(|n| n.text.contains("Skipped") || n.text.contains("Dropped")), "{:?}", clean.notices);
    // An unusable result is an error that also explains what made the scene empty.
    let mut empty = mesh_session(json!([mesh_feature(short, 30.0)]));
    let error = crate::cmd::sharing::share_bytes(&mut empty, "webScene", &json!({})).unwrap_err();
    assert!(error.contains("Skipped 1 polygon(s) whose outer ring has fewer than 3 vertices"), "{error}");
}

#[test]
fn web_scene_reports_the_feature_limit_and_does_not_count_filtered_rows() {
    // The documented scene limit is 200,000 visible features with geometry.
    let mut ds = Dataset::new("Points", GeometryType::Point, vec![Field::new("i", FieldType::Integer)]);
    for i in 0..=200_000 {
        ds.push(Some(Geometry::Point(Coord::new(f64::from(i) * 1e-7, 0.0))), vec![Attr::Int(i64::from(i))]);
    }
    let mut s = Session::new(Project::default());
    s.map_mut().add_dataset(ds);
    assert_eq!(positions(&scene(&mut s, "limit")), 400_000, "two positions per exported point");
    let expected = "1 feature(s) left out of the scene: the 200000 feature limit was reached";
    assert_eq!(
        s.notices.iter().filter(|n| n.text == expected && n.loss() == Some(rustgis_data::loss::LossCode::DroppedOverLimit)).count(),
        1,
        "{:?}",
        s.notices
    );
    s.run("layer.definitionQuery", &json!({"layer":"Points", "where":"i < 200000"})).unwrap();
    s.notices.clear();
    assert_eq!(positions(&scene(&mut s, "filtered-limit")), 400_000);
    assert!(!s.notices.iter().any(|n| n.text.contains("feature limit")), "hidden rows are a choice: {:?}", s.notices);
    let mut clean = mesh_session(json!([mesh_feature(json!({"type":"Point", "coordinates":[0.0,0.0]}), 30.0)]));
    assert_eq!(positions(&scene(&mut clean, "clean-limit")), 2);
    assert!(!clean.notices.iter().any(|n| n.text.contains("feature limit")), "{:?}", clean.notices);
}

#[test]
fn web_scene_says_per_layer_how_many_extrusions_it_holds_to_its_limit() {
    let extrude = |s: &mut Session, layer: &str, base: f64| {
        if let Some(l) = s.map_mut().layers.iter_mut().find(|l| l.name == layer) {
            l.extrusion = Some(rustgis_map::effects::Extrusion { field: Some("h".into()), base, ..Default::default() });
        }
    };
    let held = |s: &Session| s.notices.iter().filter(|n| n.text.contains("held to the web scene's limit")).count();
    // Two polygons and a point (its stick) past the scene's top, one polygon within it, and a
    // polygon past it that draws nothing (skipped, so not held); a second layer whose base alone
    // is past the top.
    let short = json!({"type":"Polygon", "coordinates":[[[0.0,0.0],[0.01,0.01],[0.0,0.0]]]});
    let mut s = mesh_session(json!([
        mesh_feature(square(0.0, false), 3e6),
        mesh_feature(square(0.02, false), 2e6),
        mesh_feature(json!({"type":"Point", "coordinates":[0.03,0.0]}), 4e6),
        mesh_feature(square(0.04, false), 30.0),
        mesh_feature(short, 5e6)
    ]));
    let text = json!({"type": "FeatureCollection", "features": [mesh_feature(square(0.06, false), 30.0)]}).to_string();
    let (raised, _) = rustgis_data::io::read_geojson(&text, "Raised").unwrap();
    s.map_mut().add_dataset(raised);
    extrude(&mut s, "Mesh", 0.0);
    extrude(&mut s, "Raised", 2e6);
    scene(&mut s, "held");
    for expected in [
        "3 feature(s) of 'Mesh' had their extrusion base or height held to the web scene's limit: base plus height stays within 1,000 km",
        "1 feature(s) of 'Raised' had their extrusion base or height held to the web scene's limit: base plus height stays within 1,000 km",
    ] {
        assert_eq!(s.notices.iter().filter(|n| n.text == expected).count(), 1, "{expected}: {:?}", s.notices);
    }
    assert_eq!(held(&s), 2, "{:?}", s.notices);
    // A base plus height that reaches the top exactly is drawn whole: nothing said.
    let mut clean = mesh_session(json!([mesh_feature(square(0.0, false), 30.0), mesh_feature(square(0.02, false), 1e6 - 20.0)]));
    extrude(&mut clean, "Mesh", 20.0);
    scene(&mut clean, "held-clean");
    assert_eq!(held(&clean), 0, "{:?}", clean.notices);
    // So does one whose fractional base and height sum to the top, though the top less the base
    // rounds just below the height.
    let mut fractional = mesh_session(json!([mesh_feature(square(0.0, false), 0.1)]));
    extrude(&mut fractional, "Mesh", 999_999.9);
    scene(&mut fractional, "held-fractional");
    assert_eq!(held(&fractional), 0, "{:?}", fractional.notices);
}
