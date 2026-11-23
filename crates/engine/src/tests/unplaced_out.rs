//! What leaves the session from a layer held in its own system with a place that can't be given in
//! longitude/latitude (#2032 C2f-4b): exports, copies, text conversions, shared items, 3D files and
//! packages take the feature without that geometry and say so once, rather than failing; a layer
//! whose places all convert says nothing of it.

use crate::Session;
use serde_json::json;

const UTM: &str = r#"{"type":"FeatureCollection","crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::32631"}},"features":[{"type":"Feature","properties":{"name":"paris"},"geometry":{"type":"LineString","coordinates":[[451000,5411000],[452000,5412000]]}}]}"#;

/// A folder removed however the test ends.
struct Dir(std::path::PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(name: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("rustgis-unplaced-out-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    Dir(d)
}

/// A session with the layer "lines" held in UTM 31N: a line at Paris and, with `far`, one
/// reaching 1e9 m east (as a reader that keeps it will hold it).
fn session(d: &Dir, far: bool) -> Session {
    let path = d.0.join("lines.geojson");
    std::fs::write(&path, UTM).unwrap();
    let mut s = Session::new(crate::Project::default());
    let id = s.run("layer.addData", &json!({"path": path, "name": "lines"})).unwrap()["layer"].as_u64().unwrap() as u32;
    let l = s.map_mut().layer_mut(id).unwrap();
    assert_eq!(l.dataset.coord_crs(), rustgis_data::raster::Crs::from_epsg(32631).unwrap());
    if far {
        let line = rustgis_data::Geometry::Line(vec![vec![
            rustgis_geom::Coord::new(451_500.0, 5_411_500.0),
            rustgis_geom::Coord::new(1e9, 5_411_500.0),
        ]]);
        l.dataset.push(Some(line), vec![rustgis_data::Value::Text("far east".into())]);
        l.touch_geometry();
    }
    s
}

/// The notices of `cmd` with `p` that speak of a place that can't be given in longitude/latitude.
fn unplaced_said(s: &mut Session, cmd: &str, p: serde_json::Value) -> Vec<String> {
    let mark = s.notice_mark();
    s.run(cmd, &p).unwrap_or_else(|e| panic!("{cmd}: {e}"));
    s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("can't be given in")).collect()
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn what_leaves_the_session_takes_a_feature_it_cant_place_without_that_geometry_and_says_so() {
    for far in [false, true] {
        let d = dir(if far { "far" } else { "clean" });
        let mut s = session(&d, far);
        let file = |name: &str| d.0.join(name).display().to_string();
        s.run("layer.select", &json!({"layer": "lines", "rows": [0, 1]})).ok();
        let cases = [
            ("share.exportLayer", json!({"layer": "lines", "path": file("out.geojson")}), "out.geojson"),
            ("data.convert", json!({"layer": "lines", "format": "geojson"}), "lines"),
            ("share.webLayer", json!({"layer": "lines", "path": file("web.zip")}), "The web layer"),
            ("share.webMap", json!({"path": file("web.html")}), "The web map"),
            ("share.geodataService", json!({"path": file("service.zip")}), "The geodata service"),
            ("share.locator", json!({"layer": "lines", "fields": ["name"], "path": file("places.rgisloc")}), "The locator"),
            ("share.export3d", json!({"layer": "lines", "path": file("lines.obj")}), "The 3D model"),
            ("map.package", json!({"path": file("map.rgismpk"), "mobile": true, "tolerance": 0.0001}), "Mobile map package"),
        ];
        for (cmd, p, said) in cases {
            let notes = unplaced_said(&mut s, cmd, p);
            if far {
                assert_eq!(notes.len(), 1, "{cmd}: {notes:?}");
                assert!(notes[0].contains(said) && notes[0].contains("1 feature(s) of lines"), "{cmd}: {}", notes[0]);
            } else {
                assert!(notes.is_empty(), "{cmd}: {notes:?}");
            }
        }
        // Copied: the text leaves it without its geometry; the clipboard keeps it whole.
        let mark = s.notice_mark();
        let id = s.map().layers.iter().find(|l| l.name == "lines").unwrap().id;
        s.map_mut().layer_mut(id).unwrap().selection = if far { vec![0, 1] } else { vec![0] };
        let out = s.run("edit.copy", &json!(null)).unwrap();
        let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("can't be given in")).collect();
        assert_eq!(said.len(), usize::from(far), "{said:?}");
        let text: serde_json::Value = serde_json::from_str(out["text"].as_str().unwrap()).unwrap();
        let geometries: Vec<bool> = text["features"].as_array().unwrap().iter().map(|f| !f["geometry"].is_null()).collect();
        assert_eq!(geometries, if far { vec![true, false] } else { vec![true] });
        if far {
            let kept = s.clipboard.as_ref().unwrap().data.features.iter().filter(|f| f.geometry.is_some()).count();
            assert_eq!(kept, 2);
        }
    }
}

/// A streamed source held in UTM 31N whose second line reaches 1e9 m east, read a chunk at a time.
struct Far {
    info: rustgis_data::source::SourceInfo,
    features: Vec<rustgis_data::Feature>,
}

impl rustgis_data::source::FeatureSource for Far {
    fn info(&self) -> &rustgis_data::source::SourceInfo {
        &self.info
    }
    fn coord_crs(&self) -> rustgis_data::raster::Crs {
        rustgis_data::raster::Crs::from_epsg(32631).unwrap()
    }
    fn read(&self, _: &rustgis_data::source::Request) -> rustgis_data::Result<rustgis_data::source::Batch> {
        Ok(rustgis_data::source::Batch { features: self.features.clone(), matched: 2, complete: true, ..Default::default() })
    }
    fn visits_chunks(&self) -> bool {
        true
    }
    fn visit_chunks(
        &self,
        _: Option<rustgis_geom::Extent>,
        _: usize,
        visit: &mut dyn FnMut(Vec<rustgis_data::Feature>) -> rustgis_data::Result<()>,
    ) -> rustgis_data::Result<rustgis_data::source::Batch> {
        visit(self.features.clone())?;
        Ok(rustgis_data::source::Batch { matched: 2, complete: true, ..Default::default() })
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_streamed_conversion_writes_a_feature_it_cant_place_without_its_geometry_and_says_so() {
    use rustgis_data::{Field, FieldType, Geometry, GeometryType, Value};
    let d = dir("streamed");
    let line = |a: (f64, f64), b: (f64, f64)| {
        Some(Geometry::Line(vec![vec![rustgis_geom::Coord::new(a.0, a.1), rustgis_geom::Coord::new(b.0, b.1)]]))
    };
    let features = vec![
        rustgis_data::Feature {
            id: 1,
            geometry: line((451_000.0, 5_411_000.0), (452_000.0, 5_412_000.0)),
            attrs: vec![Value::Text("paris".into())],
        },
        rustgis_data::Feature {
            id: 2,
            geometry: line((451_500.0, 5_411_500.0), (1e9, 5_411_500.0)),
            attrs: vec![Value::Text("far".into())],
        },
    ];
    let info = rustgis_data::source::SourceInfo {
        name: "lines".into(),
        format: "test",
        geometry_type: GeometryType::Polyline,
        fields: vec![Field::new("name", FieldType::Text)],
        extent: rustgis_geom::Extent::new(2.3, 48.8, 2.4, 48.9),
        count: Some(2),
        crs: "EPSG:32631".into(),
        indexed: false,
        notes: Default::default(),
        zm_dropped: false,
    };
    let mut s = Session::new(crate::Project::default());
    let src = rustgis_map::source::SourceLayer::from_path("far.test", None, Box::new(Far { info, features }));
    s.map_mut().add_streamed("lines", src);
    for format in ["geojson", "csv"] {
        let path = d.0.join(format!("lines.{format}")).display().to_string();
        let notes = unplaced_said(&mut s, "data.convert", json!({"layer": "lines", "format": format, "path": path}));
        assert_eq!(notes.len(), 1, "{format}: {notes:?}");
        assert!(notes[0].contains("1 feature(s) of lines have a place"), "{format}: {}", notes[0]);
    }
    // Both rows are written, with their values; only the far one without its geometry.
    let text: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.0.join("lines.geojson")).unwrap()).unwrap();
    let rows: Vec<(String, bool)> = text["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["properties"]["name"].as_str().unwrap().to_string(), f["geometry"].is_null()))
        .collect();
    assert_eq!(rows, [("paris".to_string(), false), ("far".to_string(), true)]);
    let csv = std::fs::read_to_string(d.0.join("lines.csv")).unwrap();
    assert!(csv.lines().count() == 3 && csv.contains("paris") && csv.contains("far"), "{csv}");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn kml_counts_a_feature_it_cant_place_as_left_out_and_a_3d_model_is_placed_by_what_it_keeps() {
    let d = dir("kml-3d");
    // KML holds features with geometry only: the far line is left out of it, and counted so.
    let mut s = session(&d, true);
    let mark = s.notice_mark();
    s.run("data.convert", &json!({"layer": "lines", "format": "kml"})).unwrap();
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(said.iter().any(|t| t.contains("1 feature") && t.contains("left out") && !t.contains("can't be given")), "{said:?}");
    // What it writes is checked in longitude/latitude, as written, not in the layer's metres.
    assert!(said.iter().all(|t| !t.contains("won't read back")), "{said:?}");
    // A line left out of a 3D model doesn't move it through a place of it that converts (here
    // 750 km east of Paris): the model of the line at Paris is the one made without it.
    let obj = |far: bool| {
        let mut s = session(&d, false);
        if far {
            let id = s.map().layers.iter().find(|l| l.name == "lines").unwrap().id;
            let l = s.map_mut().layer_mut(id).unwrap();
            let line = rustgis_data::Geometry::Line(vec![vec![
                rustgis_geom::Coord::new(1_200_000.0, 5_411_000.0),
                rustgis_geom::Coord::new(1e9, 5_411_000.0),
            ]]);
            l.dataset.push(Some(line), vec![rustgis_data::Value::Text("far".into())]);
            l.touch_geometry();
        }
        let path = d.0.join(format!("lines-{far}.obj"));
        s.run("share.export3d", &json!({"layer": "lines", "path": path})).unwrap();
        std::fs::read_to_string(path).unwrap().lines().filter(|l| l.starts_with("v ")).map(str::to_string).collect::<Vec<_>>()
    };
    let (alone, beside) = (obj(false), obj(true));
    assert!(!alone.is_empty());
    assert_eq!(alone, beside);
}
