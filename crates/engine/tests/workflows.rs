//! Everyday jobs run from start to finish the way a user runs them: by command id, as the ribbon,
//! the CLI, Python and the AI Assistant do. Each checks its result against an answer worked out
//! independently, then saves, reopens and hands the result on, checking nothing changed on the way.
//!
//! The data is made here, small enough that the whole file runs in well under a second.

use rustgis_engine::data::{Dataset, Value as Field, io};
use rustgis_engine::{Project, Session};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Declared versions are checked before payloads, and unreadable nodes cannot disappear on import.
#[test]
fn model_import_refuses_unsupported_files_without_changing_session() {
    use base64::Engine;
    let scratch = Scratch::new("model-compatibility");
    let mut s = Session::new(Project::new("Original"));
    s.run("model.new", &json!({"name": "Keep", "steps": []})).unwrap();
    s.run("project.rename", &json!({"name": "Redo"})).unwrap();
    s.run("edit.undo", &Value::Null).unwrap();
    s.path = Some(scratch.path("original.rgis"));
    s.mark_saved();
    let before = serde_json::to_value(&s.project).unwrap();
    let history = (s.undo_depth(), s.undo_label(), s.redo_label());
    let graph = json!({"nodes": [
        {"id": "i", "kind": "input", "layer": "Points"},
        {"id": "future", "kind": "branch"},
        {"id": "o", "kind": "output", "name": "Result"}
    ], "edges": [
        {"from": "i", "fromPort": "out", "to": "future", "toPort": "input"},
        {"from": "future", "fromPort": "out", "to": "o", "toPort": "in"}
    ]});
    for (file, message) in [
        (json!({"version": "2.0.0", "graph": "unreadable future graph"}), "unsupported version"),
        (json!({"version": 2, "steps": "unreadable future steps"}), "unsupported version"),
        (json!({"version": null, "graph": {"nodes": []}}), "unsupported version"),
        (json!({"version": "1", "steps": []}), "unsupported version"),
        (json!({"graph": {"nodes": []}, "description": "Keep this"}), "file has unsupported fields"),
        (json!({"steps": [], "future": true}), "file has unsupported fields"),
        (json!({"graph": {"nodes": []}, "steps": [{"tool": "buffer", "params": {}}]}), "both a graph and steps"),
        (json!({"steps": [{"tool": "buffer", "params": {}, "when": {"enabled": true}}]}), "step 1 has unsupported fields"),
        (json!({"steps": [{"tool": "buffer", "params": ["input"]}]}), "step 1 needs an object"),
        (json!({"steps": vec![json!({"tool": "buffer", "params": {}}); 2001]}), "at most 2000 steps"),
        (json!({"graph": {"nodes": [], "edges": [], "future": {"enabled": true}}}), "graph has unsupported fields"),
        (json!({"version": "1.0.0", "graph": graph}), "node 2"),
        (json!({"graph": {"nodes": [{"id": "t", "kind": "tool", "tool": {}}]}}), "node 1"),
        (json!({"graph": {"nodes": [{"id": "", "kind": "input"}]}}), "discard 1 node(s)"),
        (json!({"graph": {"nodes": [{"id": " padded ", "kind": "input"}]}}), "would be trimmed or truncated"),
        (json!({"graph": {"nodes": [{"id": "a".repeat(101), "kind": "input"}]}}), "would be trimmed or truncated"),
        (json!({"graph": {"nodes": [{"id": "a", "kind": "input", "x": 1000001}]}}), "outside the supported canvas bounds"),
        (json!({"graph": {"nodes": [{"id": "a", "kind": "input", "y": 1e300}]}}), "outside the supported canvas bounds"),
        (json!({"graph": {"nodes": [{"id": "t", "kind": "tool"}, {"id": "t", "kind": "output"}]}}), "discard 1 node(s)"),
        (json!({"graph": {"nodes": [{"id": "t", "kind": "tool", "parms": {"distance": 37}}]}}), "unsupported fields"),
        (json!({"graph": {"nodes": [{"id": "t", "kind": "tool", "params": [37]}]}}), "needs an object"),
        (json!({"graph": {"nodes": [], "edges": {}}}), "connections must be an array"),
        (json!({"graph": {"nodes": [], "edges": ["bad edge"]}}), "connection 1 is malformed"),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input"}, {"id": "b", "kind": "output"}],
            "edges": [{"from": "a", "to": "b"}]}}),
            "1 connection(s)",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input"}],
            "edges": [{"from": "a", "to": "missing", "toPort": "input"}]}}),
            "1 connection(s)",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input"}, {"id": "b", "kind": "output"}],
            "edges": [{"from": "a", "to": "b", "toPort": "in"}, {"from": "a", "to": "b", "toPort": "in"}]}}),
            "1 connection(s)",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input"}, {"id": "b", "kind": "output"}],
            "edges": [{"from": "a", "to": "b", "toPort": "in", "future": true}]}}),
            "unsupported fields",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input", "layer": "Points"},
            {"id": "b", "kind": "tool", "tool": "buffer"}],
            "edges": [{"from": "a", "fromPort": "out", "to": "b", "toPort": "future"}]}}),
            "connection 1 refers to an unsupported port",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input"}, {"id": "b", "kind": "output"}],
            "edges": [{"from": "a", "fromPort": "out", "to": "b", "toPort": "future"}]}}),
            "connection 1 refers to an unsupported port",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input"}, {"id": "b", "kind": "output"}],
            "edges": [{"from": "a", "fromPort": "future", "to": "b", "toPort": "in"}]}}),
            "connection 1 refers to an unsupported port",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input", "layer": "A"},
            {"id": "b", "kind": "input", "layer": "B"}, {"id": "t", "kind": "tool", "tool": "buffer"}], "edges": [
                {"from": "a", "fromPort": "out", "to": "t", "toPort": "input"},
                {"from": "b", "fromPort": "out", "to": "t", "toPort": "input"}
            ]}}),
            "connection 2 duplicates a single-valued input port",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input"}, {"id": "t", "kind": "tool", "tool": "buffer"}],
            "edges": [{"from": "a", "fromPort": "out", "to": "t", "toPort": "input"}]}}),
            "connection 1 has an input without a layer",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input", "layer": ""}, {"id": "t", "kind": "tool", "tool": "buffer"}],
            "edges": [{"from": "a", "fromPort": "out", "to": "t", "toPort": "input"}]}}),
            "connection 1 has an input without a layer",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input", "layer": "East;West"}, {"id": "t", "kind": "tool", "tool": "merge"}],
            "edges": [{"from": "a", "fromPort": "out", "to": "t", "toPort": "inputs"}]}}),
            "connection 1 has an input layer name that cannot be represented in steps",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input", "layer": "%1"}, {"id": "t", "kind": "tool", "tool": "buffer"}],
            "edges": [{"from": "a", "fromPort": "out", "to": "t", "toPort": "input"}]}}),
            "connection 1 has an input layer name that cannot be represented in steps",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "input", "layer": " Points "}, {"id": "t", "kind": "tool", "tool": "buffer"}],
            "edges": [{"from": "a", "fromPort": "out", "to": "t", "toPort": "input"}]}}),
            "connection 1 has an input layer name that cannot be represented in steps",
        ),
        (
            json!({"graph": {"nodes": [{"id": "a", "kind": "tool", "tool": "buffer"},
            {"id": "b", "kind": "tool", "tool": "buffer"}], "edges": [
                {"from": "a", "fromPort": "out", "to": "b", "toPort": "input"},
                {"from": "b", "fromPort": "out", "to": "a", "toPort": "input"}
            ]}}),
            "contains a loop",
        ),
    ] {
        let text = file.to_string();
        let path = scratch.path("unsupported.model.json");
        std::fs::write(&path, &text).unwrap();
        for params in
            [json!({"text": text}), json!({"data": base64::engine::general_purpose::STANDARD.encode(&text)}), json!({"path": path})]
        {
            let error = s.run("model.import", &params).unwrap_err();
            assert!(error.contains(message), "{error}");
            if message == "unsupported version" || message.starts_with("node ") || message.contains("unsupported fields") {
                assert!(error.contains("Update RustGIS"), "{error}");
            }
            assert_eq!(serde_json::to_value(&s.project).unwrap(), before);
            assert_eq!((s.undo_depth(), s.undo_label(), s.redo_label()), history);
            assert!(s.can_undo() && s.can_redo());
            assert_eq!(s.path.as_ref(), Some(&scratch.path("original.rgis")));
            assert!(!s.dirty());
        }
    }
}

/// Actual graph and web-tool writers still round-trip, with or without their legacy version marker.
#[test]
fn model_import_preserves_supported_and_legacy_files() {
    use base64::Engine;
    let scratch = Scratch::new("model-roundtrip");
    let mut original = Session::new(Project::new("Original"));
    original
        .run(
            "model.new",
            &json!({"name": "Graph", "graph": {"nodes": [
                {"id": "i", "kind": "input", "layer": "Points"},
                {"id": "t", "kind": "tool", "tool": "buffer", "params": {"distance": 37}},
                {"id": "o", "kind": "output", "name": "Near points"}
            ], "edges": [
                {"from": "i", "fromPort": "out", "to": "t", "toPort": "input"},
                {"from": "t", "fromPort": "out", "to": "o", "toPort": "in"}
            ]}}),
        )
        .unwrap();
    original
        .run(
            "model.new",
            &json!({"name": "Steps", "steps": [
                {"tool": "buffer", "params": {"input": "Points", "distance": 37}},
                {"tool": "buffer", "params": {"input": "%1", "distance": 74}}
            ]}),
        )
        .unwrap();
    let graph = original.run("model.export", &json!({"name": "Graph"})).unwrap();
    let (_, web, _) = rustgis_engine::cmd::sharing::share_bytes(&mut original, "webTool", &json!({"model": "Steps"})).unwrap();
    original.run("model.new", &json!({"name": "Long Steps", "steps": vec![json!({"tool": "buffer", "params": {}}); 1001]})).unwrap();
    let (_, long, _) = rustgis_engine::cmd::sharing::share_bytes(&mut original, "webTool", &json!({"model": "Long Steps"})).unwrap();
    for (index, text) in [graph["text"].as_str().unwrap().to_string(), String::from_utf8(web).unwrap(), String::from_utf8(long).unwrap()]
        .into_iter()
        .enumerate()
    {
        let file: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(file["version"], if index == 0 { json!("1.0.0") } else { json!(1) });
        for legacy in [false, true] {
            let mut file = file.clone();
            if legacy {
                file.as_object_mut().unwrap().remove("version");
            }
            let text = file.to_string();
            let path = scratch.path(if index == 0 { "graph.model.json" } else { "steps.rgistool" });
            std::fs::write(&path, &text).unwrap();
            for params in
                [json!({"text": text}), json!({"data": base64::engine::general_purpose::STANDARD.encode(&text)}), json!({"path": path})]
            {
                let mut loaded = Session::new(Project::new("Loaded"));
                loaded.run("model.import", &params).unwrap();
                assert_eq!(loaded.project.models[0], original.project.models[index]);
                assert!(loaded.notices.is_empty(), "supported files need no warning");
                if index == 1 {
                    let graph = loaded.run("model.export", &json!({})).unwrap();
                    let mut reopened = Session::new(Project::new("Graph"));
                    reopened.run("model.import", &json!({"text": graph["text"]})).unwrap();
                    assert_eq!(reopened.project.models[0].steps, original.project.models[index].steps);
                    assert!(reopened.notices.is_empty(), "supported references need no warning");
                }
                if index == 2 {
                    let output = scratch.path("keep.model.json");
                    std::fs::write(&output, "keep").unwrap();
                    let error = loaded.run("model.export", &json!({"path": output})).unwrap_err();
                    assert!(error.contains("1001 steps") && error.contains("at most 1000"), "{error}");
                    assert_eq!(std::fs::read_to_string(output).unwrap(), "keep");
                    assert_eq!(loaded.project.models[0], original.project.models[index]);
                }
            }
        }
    }
    // Step syntax stays literal for semicolons on single-input ports and nonnumeric percent names.
    for (layer, tool, port) in [("East;West", "buffer", "input"), ("%north", "merge", "inputs")] {
        let graph = json!({"nodes": [{"id": "i", "kind": "input", "layer": layer}, {"id": "t", "kind": "tool", "tool": tool}],
            "edges": [{"from": "i", "fromPort": "out", "to": "t", "toPort": port}]});
        let mut loaded = Session::new(Project::default());
        loaded.run("model.import", &json!({"text": json!({"graph": graph}).to_string()})).unwrap();
        assert_eq!(loaded.project.models[0].steps[0].params[port], layer);
        let (_, web, _) = rustgis_engine::cmd::sharing::share_bytes(&mut loaded, "webTool", &json!({})).unwrap();
        let mut reopened = Session::new(Project::default());
        reopened.run("model.import", &json!({"text": String::from_utf8(web).unwrap()})).unwrap();
        let exported = reopened.run("model.export", &json!({})).unwrap();
        let file: Value = serde_json::from_str(exported["text"].as_str().unwrap()).unwrap();
        assert!(file["graph"]["nodes"].as_array().unwrap().iter().any(|node| node["layer"] == layer));
        assert!(loaded.notices.is_empty() && reopened.notices.is_empty());
    }
    // Graph capacity is checked before building or laying out a large multi-input step.
    for (name, inputs) in
        [("Connections", "A;".repeat(4001)), ("Nodes", (0..2001).map(|i| format!("Layer{i}")).collect::<Vec<_>>().join(";"))]
    {
        original.run("model.new", &json!({"name": name, "steps": [{"tool": "merge", "params": {"inputs": inputs}}]})).unwrap();
        let before = original.project.models.clone();
        let error = original.run("model.export", &json!({"name": name})).unwrap_err();
        assert!(error.contains("graph limits"), "{error}");
        assert_eq!(original.project.models, before);
    }
    for (steps, reference, message) in [
        (json!([{"tool": "buffer", "params": {"input": "%0"}}]), "%0", "not an earlier step"),
        (json!([{"tool": "buffer", "params": {"input": "%2"}}]), "%2", "not an earlier step"),
        (
            json!([{"tool": "vectorToPmtiles", "params": {"input": "Points"}}, {"tool": "buffer", "params": {"input": "%1"}}]),
            "%1",
            "has no layer output",
        ),
    ] {
        let mut loaded = Session::new(Project::new("Invalid reference"));
        let file = json!({"steps": steps});
        loaded.run("model.import", &json!({"text": file.to_string()})).unwrap();
        let before = loaded.project.models.clone();
        let output = scratch.path("keep-reference.model.json");
        std::fs::write(&output, "keep").unwrap();
        let error = loaded.run("model.export", &json!({"path": output})).unwrap_err();
        assert!(error.contains(reference) && error.contains(message), "{error}");
        assert_eq!(std::fs::read_to_string(output).unwrap(), "keep");
        assert_eq!(loaded.project.models, before);
    }
}

/// Even a name emitted by the writer can exceed the import naming limit after disambiguation.
#[test]
fn model_import_reports_changed_names() {
    let mut original = Session::new(Project::new("Original"));
    for _ in 0..2 {
        original.run("model.new", &json!({"name": "N".repeat(200), "steps": [{"tool": "buffer", "params": {"input": "Cities"}}]})).unwrap();
    }
    let name = original.project.models[1].name.clone();
    assert!(name.chars().count() > 200);
    let exported = original.run("model.export", &json!({"name": name})).unwrap();
    let mut file: Value = serde_json::from_str(exported["text"].as_str().unwrap()).unwrap();
    for name in [file["name"].clone(), json!(" Padded "), json!(17)] {
        file["name"] = name.clone();
        let mut loaded = Session::new(Project::new("Loaded"));
        loaded.run("model.import", &json!({"text": file.to_string()})).unwrap();
        assert_ne!(Some(loaded.project.models[0].name.as_str()), name.as_str());
        assert_eq!(loaded.notices.len(), 1);
        assert!(!loaded.notices[0].error && loaded.notices[0].text.contains("1 model name"));
        assert_eq!(serde_json::to_value(loaded.project.models[0].graph.as_ref().unwrap()).unwrap(), file["graph"]);
    }
    file.as_object_mut().unwrap().remove("name");
    let mut loaded = Session::new(Project::new("Loaded"));
    loaded.run("model.import", &json!({"text": file.to_string()})).unwrap();
    assert_eq!(loaded.project.models[0].name, "Tool");
    assert!(loaded.notices.is_empty(), "a missing name alone needs no warning");
    loaded.run("model.import", &json!({"text": file.to_string()})).unwrap();
    assert_eq!(loaded.project.models[1].name, "Tool 2");
    assert_eq!(loaded.notices.len(), 1);
    assert!(!loaded.notices[0].error && loaded.notices[0].text.contains("1 model name"));
    assert_eq!(serde_json::to_value(loaded.project.models[1].graph.as_ref().unwrap()).unwrap(), file["graph"]);
}

/// Repeated merge inputs survive import, saving under a new name and re-export.
#[test]
fn model_import_round_trips_repeated_step_inputs() {
    let mut original = Session::new(Project::new("Original"));
    let steps = json!([{"tool": "merge", "params": {"inputs": "A;A"}}]);
    original.run("model.new", &json!({"name": "Repeated", "steps": steps})).unwrap();
    let exported = original.run("model.export", &json!({"name": "Repeated"})).unwrap();
    let mut file: Value = serde_json::from_str(exported["text"].as_str().unwrap()).unwrap();
    assert_eq!(file["graph"]["edges"].as_array().unwrap().len(), 2);
    for legacy in [false, true] {
        if legacy {
            file.as_object_mut().unwrap().remove("version");
        }
        let mut loaded = Session::new(Project::new("Loaded"));
        loaded.run("model.import", &json!({"text": file.to_string()})).unwrap();
        let model = &loaded.project.models[0];
        assert_eq!(serde_json::to_value(&model.steps).unwrap(), steps);
        assert_eq!(serde_json::to_value(model.graph.as_ref().unwrap()).unwrap(), file["graph"]);
        loaded.run("model.save", &json!({"name": "Repeated", "rename": "Resaved", "graph": file["graph"]})).unwrap();
        assert_eq!(serde_json::to_value(&loaded.project.models[0].steps).unwrap(), steps);
        assert_eq!(serde_json::to_value(loaded.project.models[0].graph.as_ref().unwrap()).unwrap(), file["graph"]);
        assert!(loaded.notices.is_empty(), "supported files need no warning");
        for params in [json!({"name": "Resaved"}), json!({"name": "Resaved", "graph": file["graph"]})] {
            let reexported = loaded.run("model.export", &params).unwrap();
            let reexported: Value = serde_json::from_str(reexported["text"].as_str().unwrap()).unwrap();
            assert_eq!(reexported["graph"], file["graph"]);
        }
    }
}

/// The parcels' south-west corner, in Memphis, and their size in degrees: 4 columns by 3 rows.
const WEST: f64 = -90.05;
const SOUTH: f64 = 35.10;
const STEP: f64 = 0.005;
/// Zone R1 runs from the parcels' west edge to this longitude, through every row; C2 from here to
/// their east edge, through the two southern rows. Both split a column of parcels down the middle.
const SPLIT: f64 = WEST + 2.5 * STEP;
/// Square kilometres to acres (an acre is 4046.8564224 m²).
const ACRES_PER_KM2: f64 = 1e6 / 4_046.856_422_4;

/// A scratch folder for one test, removed when it is dropped.
struct Scratch(PathBuf);

impl Scratch {
    /// A new, empty folder under the system's temporary folder, unique to this process and moment.
    fn new(label: &str) -> Self {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("rustgis-workflow-{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// A file in the folder.
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    /// Removes the folder and everything in it, ignoring a failure.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A lon/lat rectangle as a GeoJSON polygon.
fn rect(w: f64, s: f64, e: f64, n: f64) -> Value {
    json!({"type": "Polygon", "coordinates": [[[w, s], [e, s], [e, n], [w, n], [w, s]]]})
}

/// The id of the parcel in column `col` and row `row`: text with leading zeros, "00101" to "00112".
fn parcel_id(col: usize, row: usize) -> String {
    format!("{:05}", 101 + row * 4 + col)
}

/// The parcels and zoning layers, and an assessment table to join to the parcels.
///
/// The table keys parcels by the same text ids. It has no rows for "00103", which straddles the
/// zones' boundary, or for the last parcel, and has a row for "101": the first parcel's id read as
/// a number, which must not match it.
fn job_data() -> (String, String, String) {
    let mut parcels = Vec::new();
    for row in 0..3 {
        for col in 0..4 {
            let (w, s) = (WEST + col as f64 * STEP, SOUTH + row as f64 * STEP);
            parcels.push(
                json!({"type": "Feature", "properties": {"parcel_id": parcel_id(col, row)}, "geometry": rect(w, s, w + STEP, s + STEP)}),
            );
        }
    }
    let zoning = [
        json!({"type": "Feature", "properties": {"zone": "R1"}, "geometry": rect(WEST, SOUTH, SPLIT, SOUTH + 3.0 * STEP)}),
        json!({"type": "Feature", "properties": {"zone": "C2"}, "geometry": rect(SPLIT, SOUTH, WEST + 4.0 * STEP, SOUTH + 2.0 * STEP)}),
    ];
    let mut csv = String::from("parcel_id,value_usd\n");
    for k in (0..12).filter(|&k| !unassessed(k)) {
        csv += &format!("{},{}\n", parcel_id(k % 4, k / 4), 100_000 + k * 1_000);
    }
    csv += "101,999999\n";
    let fc = |f: &[Value]| json!({"type": "FeatureCollection", "features": f}).to_string();
    (fc(&parcels), fc(&zoning), csv)
}

/// A session with the job's layers and table loaded, named `parcels`, `zoning` and `assessments`.
fn loaded() -> Session {
    let (parcels, zoning, csv) = job_data();
    let mut s = Session::new(Project::new("workflow"));
    s.run("layer.addDataText", &json!({"fileName": "parcels.geojson", "text": parcels})).unwrap();
    s.run("layer.addDataText", &json!({"fileName": "zoning.geojson", "text": zoning})).unwrap();
    s.run("layer.addDataText", &json!({"fileName": "assessments.csv", "text": csv})).unwrap();
    s
}

/// The job's steps: join the assessments to the parcels, split them by zone, measure the pieces,
/// and total their area by zone, into the table `zoned_Statistics`.
fn job_steps() -> Value {
    json!([
        {"tool": "joinField", "params": {"target": "parcels", "targetField": "parcel_id", "joinTable": "assessments", "output": "assessed"}},
        {"tool": "intersect", "params": {"input": "%1", "overlay": "zoning", "output": "zoned"}},
        {"tool": "calculateGeometry", "params": {"input": "%2"}},
        {"tool": "summaryStatistics", "params": {"input": "%2", "case": "zone", "statistics": [{"field": "area_km2", "statistic": "SUM"}]}},
    ])
}

/// The area of a lon/lat rectangle on WGS 84, in km², from the closed form for the area between two
/// parallels (Snyder, Map Projections: A Working Manual, eq. 3-12 with 3-11). The tool's edges are
/// geodesics, which bow from the parallels by about 2 cm over these 1 km edges: under 1e-6 of the area.
fn quadrangle_km2(w: f64, s: f64, e: f64, n: f64) -> f64 {
    let (a, f): (f64, f64) = (6_378_137.0, 1.0 / 298.257_223_563);
    let e2 = f * (2.0 - f);
    let ecc = e2.sqrt();
    let q = |lat: f64| {
        let x = lat.to_radians().sin();
        (1.0 - e2) * (x / (1.0 - e2 * x * x) - (1.0 / (2.0 * ecc)) * ((1.0 - ecc * x) / (1.0 + ecc * x)).ln())
    };
    a * a / 2.0 * (e - w).to_radians() * (q(n) - q(s)) / 1e6
}

/// A dataset's rows as `field → value` maps, keyed by one field's text, for comparing two copies.
fn rows(ds: &Dataset, key: &str) -> std::collections::BTreeMap<String, Vec<(String, Field)>> {
    let k = ds.field_index(key).unwrap_or_else(|| panic!("{}: no field '{key}'", ds.name));
    (0..ds.features.len())
        .map(|r| {
            let fields = ds.fields.iter().enumerate().map(|(i, f)| (f.name.clone(), ds.value(r, i).clone())).collect();
            (ds.value(r, k).to_string(), fields)
        })
        .collect()
}

/// The project's table called `name`.
fn table<'a>(s: &'a Session, name: &str) -> &'a Dataset {
    s.project.tables.iter().map(|t| &t.dataset).find(|d| d.name == name).unwrap_or_else(|| panic!("no table '{name}'"))
}

/// The map's layer called `name`.
fn layer<'a>(s: &'a Session, name: &str) -> &'a Dataset {
    &s.map().layer_by_name(name).unwrap_or_else(|| panic!("no layer '{name}'")).dataset
}

/// Checks the zone totals against the closed-form areas: R1 is three rows of 2.5 parcels, C2 two
/// rows of 1.5 (the north-east parcel and the east half of its neighbour are in no zone).
fn check_totals(stats: &Dataset) {
    let want = [
        ("R1", 9, quadrangle_km2(WEST, SOUTH, SPLIT, SOUTH + 3.0 * STEP)),
        ("C2", 4, quadrangle_km2(SPLIT, SOUTH, WEST + 4.0 * STEP, SOUTH + 2.0 * STEP)),
    ];
    let got = rows(stats, "zone");
    assert_eq!(stats.features.len(), 2, "one row per zone");
    assert_eq!(got.len(), 2, "one row per zone: {got:?}");
    for (zone, pieces, km2) in want {
        let row: std::collections::HashMap<_, _> = got[zone].iter().cloned().collect();
        assert_eq!(row["FREQUENCY"], Field::Int(pieces), "{zone}: pieces");
        let sum = row["SUM_area_km2"].as_f64().unwrap();
        assert!((sum - km2).abs() < 1e-6 * km2, "{zone}: {sum} km², the closed form {km2} km²");
        // The figure an analyst reports, to the acre.
        assert_eq!((sum * ACRES_PER_KM2).round(), (km2 * ACRES_PER_KM2).round(), "{zone}: acres");
    }
}

/// Whether the assessment table has no row for the parcel numbered `k` from 0: "00103", split
/// into two pieces by the overlay, and the last, which is in no zone.
fn unassessed(k: usize) -> bool {
    k == 2 || k == 11
}

/// The value the assessment table gives a parcel: none for those with no row.
fn assessment(parcel: &str) -> Field {
    let k = parcel.parse::<usize>().unwrap_or_else(|_| panic!("{parcel}")) - 101;
    if unassessed(k) { Field::Null } else { Field::Int(100_000 + k as i64 * 1_000) }
}

/// Checks that every feature carries its parcel's assessed value; the number checked.
fn check_values(ds: &Dataset) -> usize {
    let (id, value) = (ds.field_index("parcel_id").unwrap(), ds.field_index("value_usd").unwrap());
    for r in 0..ds.features.len() {
        let parcel = ds.value(r, id).to_string();
        assert_eq!(ds.value(r, value), &assessment(&parcel), "{}: {parcel}", ds.name);
    }
    ds.features.len()
}

/// Join a spreadsheet to parcels, keeping leading zeros in their ids; overlay zoning; total the
/// area by zone; make a map. Save, reopen and hand the pieces and totals to a colleague as files,
/// with every count and total still matching.
#[test]
fn join_overlay_summarise_map_save_reopen_and_share() {
    let dir = Scratch::new("w1");
    let mut s = loaded();
    // One tool at a time, as from the Tools pane: each step names the layer the one before made.
    let steps = job_steps().to_string().replace("\"%1\"", "\"assessed\"").replace("\"%2\"", "\"zoned\"");
    for step in serde_json::from_str::<Value>(&steps).unwrap().as_array().unwrap() {
        s.run("gp.run", step).unwrap_or_else(|e| panic!("{}: {e}", step["tool"]));
    }

    // The join matched every parcel by its text id; "101" matched none, and the parcels with no
    // assessment kept their rows with no value.
    assert_eq!(check_values(layer(&s, "assessed")), 12);
    check_totals(table(&s, "zoned_Statistics"));
    let zoned = layer(&s, "zoned").clone();
    // Every piece keeps its parcel's value through the overlay, "00103"'s two pieces none.
    assert_eq!(check_values(&zoned), 13);
    let (id, value) = (zoned.field_index("parcel_id").unwrap(), zoned.field_index("value_usd").unwrap());
    let split = (0..zoned.features.len()).filter(|&r| zoned.value(r, id).to_string() == "00103").collect::<Vec<_>>();
    assert_eq!(split.len(), 2, "the overlay splits 00103");
    assert!(split.iter().all(|&r| zoned.value(r, value) == &Field::Null), "00103 has no assessment");

    let png = dir.path("map.png");
    s.run("share.exportMap", &json!({"path": png, "width": 320, "height": 200})).unwrap();
    let bytes = std::fs::read(&png).unwrap();
    let map = rustgis_engine::render::decode_png(&bytes).expect("a PNG came out");
    let colours: std::collections::HashSet<_> = map.pixels().iter().map(|p| (p.red(), p.green(), p.blue())).collect();
    assert!(colours.len() > 2, "the map draws its layers, not only a background: {} colours", colours.len());

    // Reopened, the project has the same layers, pieces and totals.
    let project = dir.path("zoning.rgis");
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    let mut reopened = Session::new(Project::default());
    reopened.run("project.open", &json!({"path": project})).unwrap();
    let names = |s: &Session| s.map().layers.iter().map(|l| l.dataset.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&reopened), names(&s));
    assert_eq!(layer(&reopened, "zoned").features, zoned.features);
    check_totals(table(&reopened, "zoned_Statistics"));

    // Handed on as a GeoPackage and a CSV, read back as a colleague's GIS would read them.
    let gpkg = dir.path("zoned.gpkg");
    reopened.run("share.exportLayer", &json!({"layer": "zoned", "path": gpkg})).unwrap();
    let (back, _) = io::read_path(&gpkg).unwrap();
    assert_eq!(check_values(&back), 13, "values survive the GeoPackage");
    let ids = |ds: &Dataset| (0..ds.features.len()).map(|r| ds.value(r, ds.field_index("parcel_id").unwrap()).clone()).collect::<Vec<_>>();
    assert_eq!(ids(&back), ids(&zoned), "parcel ids, leading zeros included");
    let area =
        |ds: &Dataset| -> f64 { (0..ds.features.len()).filter_map(|r| ds.value(r, ds.field_index("area_km2").unwrap()).as_f64()).sum() };
    assert!((area(&back) - area(&zoned)).abs() < 1e-12, "areas survive the GeoPackage");
    let csv = dir.path("totals.csv");
    reopened.run("share.exportTable", &json!({"table": "zoned_Statistics", "path": csv})).unwrap();
    let (totals, _) = io::read_path(&csv).unwrap();
    check_totals(&totals);
}

/// Exports a session's job results as files and returns their bytes: the pieces as GeoJSON and
/// the totals as CSV.
fn outputs(s: &mut Session, dir: &Path, tag: &str) -> (Vec<u8>, Vec<u8>) {
    let (pieces, totals) = (dir.join(format!("{tag}.geojson")), dir.join(format!("{tag}.csv")));
    s.run("share.exportLayer", &json!({"layer": "zoned", "path": pieces})).unwrap();
    s.run("share.exportTable", &json!({"table": "zoned_Statistics", "path": totals})).unwrap();
    (std::fs::read(pieces).unwrap(), std::fs::read(totals).unwrap())
}

/// A saved job, run again on the same data, gives byte-identical files: in another session, and
/// after the project is saved and reopened.
#[test]
fn a_saved_job_gives_identical_output_every_time() {
    let dir = Scratch::new("w3");
    let mut first = loaded();
    first.run("model.new", &json!({"name": "zoning job", "steps": job_steps()})).unwrap();
    first.run("model.run", &json!({"name": "zoning job"})).unwrap();
    let want = outputs(&mut first, &dir.0, "first");
    check_totals(table(&first, "zoned_Statistics"));

    let mut second = loaded();
    second.run("model.new", &json!({"name": "zoning job", "steps": job_steps()})).unwrap();
    second.run("model.run", &json!({"name": "zoning job"})).unwrap();
    assert!(outputs(&mut second, &dir.0, "second") == want, "a second session's run differs");

    // The model travels with the project: reopened, the job runs on a fresh copy of the data.
    let project = dir.path("job.rgis");
    let mut fresh = loaded();
    let saved = first.project.models.clone();
    fresh.project.models = saved;
    fresh.run("project.saveAs", &json!({"path": project})).unwrap();
    let mut reopened = Session::new(Project::default());
    reopened.run("project.open", &json!({"path": project})).unwrap();
    reopened.run("model.run", &json!({"name": "zoning job"})).unwrap();
    assert!(outputs(&mut reopened, &dir.0, "reopened") == want, "the reopened project's run differs");
}

/// A job given bad input stops with a message that names the problem, and leaves the project as
/// it was: the data, the layers and the tables.
#[test]
fn a_bad_input_stops_the_job_and_leaves_the_data_untouched() {
    let mut s = loaded();
    let before = serde_json::to_value(&s.project).unwrap();
    let mut steps = job_steps();
    steps[1]["params"]["overlay"] = json!("zonning");
    s.run("model.new", &json!({"name": "typo", "steps": steps})).unwrap();
    let with_model = serde_json::to_value(&s.project).unwrap();
    let err = s.run("model.run", &json!({"name": "typo"})).unwrap_err();
    assert!(err.contains("zonning"), "the message names the missing layer: {err}");
    let after = serde_json::to_value(&s.project).unwrap();
    // Everything but the run's history record is as it was.
    let strip = |mut v: Value| {
        v.as_object_mut().map(|o| o.remove("gp_history"));
        v
    };
    assert_eq!(strip(after), strip(with_model.clone()), "the failed run changed the project");
    assert_ne!(before, with_model, "the model was saved");

    // A file that isn't text is refused, again without touching the project.
    let err = s.run("layer.addDataBytes", &json!({"fileName": "broken.csv", "data": "//79/A=="})).unwrap_err();
    assert!(err.contains("broken.csv"), "the message names the file: {err}");
    assert_eq!(strip(serde_json::to_value(&s.project).unwrap()), strip(with_model));
}

/// The Census files the nightly build downloads (see `.github/workflows/nightly.yml`), in the folder
/// `RUSTGIS_WORKFLOW_DATA` names.
fn census(file: &str) -> PathBuf {
    let dir = std::env::var_os("RUSTGIS_WORKFLOW_DATA").expect("RUSTGIS_WORKFLOW_DATA names the folder of Census files");
    Path::new(&dir).join(file)
}

/// The Census Bureau's tracts for Tennessee (TIGER/Line), cut to Shelby County, joined to the
/// Gazetteer's published areas and measured: every tract's geodesic area matches its published
/// land and water area. Saved, reopened and exported, the 249 tracts and their areas stay the same.
///
/// The published areas and the TIGER outlines agree to 4e-6 (worked out separately with
/// GeographicLib); the test allows 1e-5.
#[test]
#[ignore = "reads Census files the nightly build downloads (RUSTGIS_WORKFLOW_DATA)"]
fn shelby_county_tract_areas_match_the_census() {
    let dir = Scratch::new("shelby");
    let mut s = Session::new(Project::new("shelby"));
    s.run("layer.addData", &json!({"path": census("tl_2024_47_tract.zip")})).unwrap();
    s.run("layer.addData", &json!({"path": census("2024_gaz_tracts_47.txt")})).unwrap();
    let tracts = "tl_2024_47_tract";
    s.run("gp.run", &json!({"tool": "selectByAttributes", "params": {"input": tracts, "where": "COUNTYFP = '157'"}})).unwrap();
    // The Gazetteer's GEOID reads as a number and the tracts' as text, and Join Field matches only
    // keys of one type, so the key is copied into a number field first, as a user would.
    s.run("edit.addField", &json!({"layer": tracts, "name": "GEOID_N", "type": "long"})).unwrap();
    s.run("edit.calculateField", &json!({"layer": tracts, "field": "GEOID_N", "expression": "GEOID"})).unwrap();
    let join = json!({"target": tracts, "targetField": "GEOID_N", "joinTable": "2024_gaz_tracts_47", "joinField": "GEOID",
                      "fields": "ALAND,AWATER", "prefix": "gaz_", "output": "shelby"});
    s.run("gp.run", &json!({"tool": "joinField", "params": join})).unwrap();
    s.run("gp.run", &json!({"tool": "calculateGeometry", "params": {"input": "shelby"}})).unwrap();

    let check = |ds: &Dataset| {
        assert_eq!(ds.features.len(), 249, "Shelby County's tracts");
        let f = |name: &str| ds.field_index(name).unwrap_or_else(|| panic!("no field '{name}'"));
        let (geoid, land, water, area) = (f("GEOID"), f("gaz_ALAND"), f("gaz_AWATER"), f("area_km2"));
        for r in 0..ds.features.len() {
            let id = ds.value(r, geoid).to_string();
            assert!(id.starts_with("47157") && id.len() == 11, "{id}");
            let published =
                ds.value(r, land).as_f64().unwrap_or_else(|| panic!("{id}: not joined")) + ds.value(r, water).as_f64().unwrap_or(0.0);
            let got = ds.value(r, area).as_f64().unwrap() * 1e6;
            // The published areas are whole square metres: a metre either way is rounding.
            assert!((got - published).abs() <= 1e-5 * published + 1.0, "{id}: {got:.0} m², the Census Bureau {published:.0} m²");
        }
    };
    let shelby = layer(&s, "shelby").clone();
    check(&shelby);

    let project = dir.path("shelby.rgis");
    s.run("project.saveAs", &json!({"path": project})).unwrap();
    let mut reopened = Session::new(Project::default());
    reopened.run("project.open", &json!({"path": project})).unwrap();
    assert_eq!(layer(&reopened, "shelby").features, shelby.features);
    let gpkg = dir.path("shelby.gpkg");
    reopened.run("share.exportLayer", &json!({"layer": "shelby", "path": gpkg})).unwrap();
    check(&io::read_path(&gpkg).unwrap().0);
}

#[test]
fn network_routes_report_invalid_segments_but_clean_networks_are_quiet() {
    use rustgis_engine::data::{Geometry, GeometryType};
    use rustgis_engine::geom::Coord;
    for invalid in [false, true] {
        let mut s = Session::new(Project::default());
        let mut ds = Dataset::new("roads", GeometryType::Polyline, vec![]);
        ds.push(
            Some(Geometry::Line(vec![
                vec![Coord::new(0.0, 0.0), Coord::new(0.01, 0.0)],
                vec![Coord::new(0.02, 0.0), Coord::new(0.03, 0.0)],
            ])),
            vec![],
        );
        if invalid {
            ds.push(Some(Geometry::Line(vec![vec![Coord::new(f64::NAN, 0.0), Coord::new(0.02, 0.0)]])), vec![]);
            ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 95.0), Coord::new(0.02, 95.0)]])), vec![]);
        }
        let id = s.map_mut().add_dataset(ds);
        let mark = s.notice_mark();
        s.run("analysis.route", &json!({"layer":id, "from":[0.0,0.0], "to":[0.01,0.0], "snapDistance":1})).unwrap();
        let notices = s.notices_since(mark);
        if invalid {
            assert_eq!(notices.len(), 1, "{notices:?}");
            assert!(notices[0].text.contains("2 segments") && notices[0].text.contains("roads"), "{notices:?}");
            assert!(!notices[0].error);
        } else {
            assert!(notices.is_empty(), "{notices:?}");
        }
        for (to, expected) in
            [(0.03, "the two points aren't connected by the network"), (0.1, "a route end is more than 1 m from the network")]
        {
            let error = s.run("analysis.route", &json!({"layer":id, "from":[0.0,0.0], "to":[to,0.0], "snapDistance":1})).unwrap_err();
            if invalid {
                assert!(error.contains("2 segments") && error.contains("roads"), "{error}");
            } else {
                assert_eq!(error, expected);
            }
        }
    }
}
