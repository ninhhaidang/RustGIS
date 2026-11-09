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
    assert_eq!(s.notices.iter().filter(|n| n.text == expected).count(), 1, "{:?}", s.notices);
    s.run("layer.definitionQuery", &json!({"layer":"Points", "where":"i < 200000"})).unwrap();
    s.notices.clear();
    assert_eq!(positions(&scene(&mut s, "filtered-limit")), 400_000);
    assert!(!s.notices.iter().any(|n| n.text.contains("feature limit")), "hidden rows are a choice: {:?}", s.notices);
    let mut clean = mesh_session(json!([mesh_feature(json!({"type":"Point", "coordinates":[0.0,0.0]}), 30.0)]));
    assert_eq!(positions(&scene(&mut clean, "clean-limit")), 2);
    assert!(!clean.notices.iter().any(|n| n.text.contains("feature limit")), "{:?}", clean.notices);
}
