//! Geoprocessing over a layer held in its own system with a place that can't be given in
//! longitude/latitude (#2032 C2f-4b): tools, the geometry calculator, annotation, routes,
//! simulation, profiles and a search area read such a feature without its geometry (or leave it
//! out) and say so once, rather than failing; a layer whose places all convert says nothing of it.
//! The layers are held in Robinson, a world projection, so every run converts them to
//! longitude/latitude.

use crate::Session;
use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value, raster::Crs};
use rustgis_geom::Coord;
use serde_json::json;

fn robinson() -> Crs {
    Crs::parse("+proj=robin +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs").unwrap()
}

/// A place in Robinson with none in longitude/latitude: far past the projection's edge.
const FAR: Coord = Coord { x: 1e12, y: 1e12 };

/// A dataset held in Robinson, made from `lonlat` geometries in longitude/latitude, then with
/// `far`, one more feature (named "far") reaching past the projection's edge.
fn held(name: &str, kind: GeometryType, lonlat: Vec<Geometry>, far: Option<Geometry>) -> Dataset {
    let mut ds = Dataset::new(name, kind, vec![Field::new("name", FieldType::Text)]);
    for (i, g) in lonlat.into_iter().enumerate() {
        ds.push(Some(g), vec![Value::Text(format!("{name} {i}"))]);
    }
    ds.hold_in(robinson()).unwrap();
    if let Some(g) = far {
        ds.push(Some(g), vec![Value::Text("far".into())]);
    }
    ds
}

/// A session with the DEM, the line layer "lines" (one line over the hill) and the polygon layer
/// "areas" (one square on it), both held in Robinson; with `far`, each has one more feature with a
/// place that can't be given in longitude/latitude.
fn session(far: bool) -> Session {
    let mut s = Session::new(crate::Project::default());
    s.run("layer.addDataBytes", &json!({"fileName": "dem.tif", "data": super::dem_b64()})).unwrap();
    let line = Geometry::Line(vec![vec![Coord::new(10.002, 45.002), Coord::new(10.038, 45.038)]]);
    let far_line = Geometry::Line(vec![vec![Coord::new(1_000_000.0, 5_000_000.0), FAR]]);
    let square = Geometry::Polygon(vec![vec![vec![
        Coord::new(10.0, 45.0),
        Coord::new(10.03, 45.0),
        Coord::new(10.03, 45.03),
        Coord::new(10.0, 45.03),
        Coord::new(10.0, 45.0),
    ]]]);
    let far_square = Geometry::Polygon(vec![vec![vec![
        Coord::new(1_000_000.0, 5_000_000.0),
        Coord::new(1_000_100.0, 5_000_000.0),
        FAR,
        Coord::new(1_000_000.0, 5_000_000.0),
    ]]]);
    let lines = held("lines", GeometryType::Polyline, vec![line], far.then_some(far_line));
    let areas = held("areas", GeometryType::Polygon, vec![square], far.then_some(far_square));
    // The far places truly have none in longitude/latitude.
    let to = lines.lonlat_conversion().unwrap();
    assert_eq!(to.point(FAR), None);
    crate::cmd::layer::add_dataset(&mut s, lines);
    crate::cmd::layer::add_dataset(&mut s, areas);
    s
}

/// What `cmd` with `p` answers, and its notices that speak of a place that can't be given in
/// longitude/latitude.
fn run(s: &mut Session, cmd: &str, p: serde_json::Value) -> (serde_json::Value, Vec<String>) {
    let mark = s.notice_mark();
    let out = s.run(cmd, &p).unwrap_or_else(|e| panic!("{cmd}: {e}"));
    let said = s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("can't be given in")).collect();
    (out, said)
}

/// The messages of a tool's run (`gp.run`) that speak of such a place.
fn tool_said(out: &serde_json::Value, notes: Vec<String>) -> Vec<String> {
    let messages = out["messages"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(String::from));
    notes
        .into_iter()
        .chain(messages.filter(|t| t.contains("can't be given in")))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn geometry_of(s: &Session, layer: &str, row: usize) -> Option<Geometry> {
    s.map().layer_by_name(layer).unwrap().dataset.features.row(row).and_then(|r| r.geometry().cloned())
}

#[test]
fn a_tool_reads_a_feature_it_cant_place_without_its_geometry_and_says_so() {
    for far in [false, true] {
        let mut s = session(far);
        // A tool that makes a layer: the far line gives no points.
        let (out, notes) = run(&mut s, "gp.run", json!({"tool": "featureVerticesToPoints", "params": {"input": "lines"}}));
        let said = tool_said(&out, notes);
        if far {
            assert_eq!(said.len(), 1, "{said:?}");
            assert!(said[0].contains("1 feature(s) of 'lines' (1)") && said[0].contains("without their geometry"), "{}", said[0]);
        } else {
            assert!(said.is_empty(), "{said:?}");
        }
        let points = s.map().layers.iter().find(|l| l.name.contains("lines") && l.name != "lines").expect("the tool's output");
        assert_eq!(points.dataset.features.rows().filter(|r| r.geometry().is_some()).count(), 2, "the line's two vertices");
        // A tool that updates the layer in place: the far line keeps its stored geometry, bit for
        // bit, rather than losing it, and gets no length worked out from a geometry it wasn't
        // given (left empty, not 0).
        let before = geometry_of(&s, "lines", 1);
        let (out, notes) = run(&mut s, "gp.run", json!({"tool": "calculateGeometry", "params": {"input": "lines"}}));
        let said = tool_said(&out, notes);
        assert_eq!(said.len(), usize::from(far), "{said:?}");
        assert_eq!(geometry_of(&s, "lines", 1), before);
        assert!(geometry_of(&s, "lines", 0).is_some());
        let ds = &s.map().layer_by_name("lines").unwrap().dataset;
        let fi = ds.field_index("length_km").unwrap();
        assert!((4.0..6.0).contains(&ds.value(0, fi).as_f64().unwrap()), "{:?}", ds.value(0, fi));
        if far {
            assert!(ds.value(1, fi).is_null(), "{:?}", ds.value(1, fi));
            assert_eq!(ds.value(1, ds.field_index("name").unwrap()), &Value::Text("far".into()));
        }
        assert_eq!(s.map().layer_by_name("lines").unwrap().dataset.coord_crs(), robinson());
    }
}

#[test]
fn the_geometry_calculator_leaves_a_feature_it_cant_place_empty_and_says_so() {
    for far in [false, true] {
        let mut s = session(far);
        let (out, said) = run(
            &mut s,
            "edit.calculateGeometry",
            json!({"layer": "lines", "property": "length", "unit": "m", "field": "len", "create": true, "scope": "all"}),
        );
        assert_eq!(out["unplaced"], json!(usize::from(far)));
        assert_eq!(said.len(), usize::from(far), "{said:?}");
        let ds = &s.map().layer_by_name("lines").unwrap().dataset;
        let fi = ds.field_index("len").unwrap();
        let length = ds.value(0, fi).as_f64().unwrap();
        assert!((4000.0..6000.0).contains(&length), "{length}");
        if far {
            assert!(ds.value(1, fi).is_null());
            assert!(said[0].contains("1 feature(s)") && said[0].contains("length is left empty in len"), "{}", said[0]);
        }
        // Areas too: no ring through both poles to refuse, the far one left empty.
        let (out, said) = run(
            &mut s,
            "edit.calculateGeometry",
            json!({"layer": "areas", "property": "area", "unit": "m2", "field": "a", "create": true, "scope": "all"}),
        );
        assert_eq!((out["unplaced"].clone(), said.len()), (json!(usize::from(far)), usize::from(far)));
    }
}

#[test]
fn annotation_leaves_out_a_feature_it_cant_place_and_says_so() {
    for far in [false, true] {
        let mut s = session(far);
        let l = s.map_mut().layers.iter_mut().find(|l| l.name == "lines").unwrap();
        l.labels.enabled = true;
        l.labels.default.field = "name".into();
        let (out, said) = run(&mut s, "labels.convert", json!({"layer": "lines"}));
        assert_eq!(out["annotations"], json!(1));
        assert_eq!(said.len(), usize::from(far), "{said:?}");
        if far {
            assert!(said[0].contains("1 feature(s) of lines") && said[0].contains("labels aren't converted"), "{}", said[0]);
        }
    }
    // A far feature with no label text (its field null) would make no label anyway: not counted.
    let mut s = session(true);
    let l = s.map_mut().layers.iter_mut().find(|l| l.name == "lines").unwrap();
    let fi = l.dataset.field_index("name").unwrap();
    l.dataset.features.set_value(1, fi, Value::Null);
    l.labels.enabled = true;
    l.labels.default.field = "name".into();
    let (out, said) = run(&mut s, "labels.convert", json!({"layer": "lines"}));
    assert_eq!(out["annotations"], json!(1));
    assert!(said.is_empty(), "{said:?}");
}

#[test]
fn a_route_and_a_simulation_leave_out_what_they_cant_place() {
    for far in [false, true] {
        let mut s = session(far);
        // The route runs over the line it can place, rather than being refused.
        let mark = s.notice_mark();
        let r = s.run("analysis.route", &json!({"layer": "lines", "from": [10.002, 45.002], "to": [10.038, 45.038]})).unwrap();
        assert!(r["lengthKm"].as_f64().unwrap() > 4.0, "{r}");
        let omitted: Vec<_> =
            s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains("segments with invalid coordinates")).collect();
        assert_eq!(omitted.len(), usize::from(far), "{omitted:?}");
        // Points in the square only.
        let (out, said) = run(&mut s, "analysis.simulate", json!({"layer": "areas", "count": 5}));
        assert_eq!(out["points"], json!(5));
        assert_eq!(said.len(), usize::from(far), "{said:?}");
        if far {
            assert!(said[0].contains("1 polygon(s) of areas"), "{}", said[0]);
        }
    }
}

#[test]
fn profiles_and_a_search_area_leave_out_what_they_cant_place() {
    for far in [false, true] {
        let mut s = session(far);
        let dem = s.map().layers.iter().find(|l| l.is_raster()).unwrap().name.clone();
        s.run("select.rows", &json!({"layer": "lines", "rows": if far { json!([0, 1]) } else { json!([0]) }})).unwrap();
        let (out, said) = run(&mut s, "profile.alongSelected", json!({"layer": dem, "lines": "lines"}));
        assert_eq!(out.as_array().unwrap().len(), 1);
        assert_eq!(said.len(), usize::from(far), "{said:?}");
        let (geometries, notes) = crate::cmd::stac_area::selected_geometries(&s, &json!({"layer": "lines"})).unwrap();
        assert_eq!(geometries.len(), 1);
        let unplaced: Vec<_> = notes.iter().filter(|n| n.contains("can't be given in")).collect();
        assert_eq!(unplaced.len(), usize::from(far), "{notes:?}");
        assert!(notes.iter().all(|n| !n.contains("have no geometry")), "{notes:?}");
        if far {
            // The far line alone: no line to profile, and why.
            s.run("select.rows", &json!({"layer": "lines", "rows": [1]})).unwrap();
            let e = s.run("profile.alongSelected", &json!({"layer": dem, "lines": "lines"})).unwrap_err();
            assert!(e.contains("can't be given in longitude/latitude"), "{e}");
        }
    }
}

/// A search area read from a file in its own system leaves out a feature it can't place and says
/// so; a file whose places all convert says nothing of it.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_search_area_file_leaves_out_what_it_cant_place() {
    let dir = std::env::temp_dir().join(format!("rustgis-unplaced-area-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let square = Geometry::Polygon(vec![vec![vec![
        Coord::new(10.0, 45.0),
        Coord::new(10.03, 45.0),
        Coord::new(10.03, 45.03),
        Coord::new(10.0, 45.03),
        Coord::new(10.0, 45.0),
    ]]]);
    let far_square = Geometry::Polygon(vec![vec![vec![
        Coord::new(1_000_000.0, 5_000_000.0),
        Coord::new(1_000_100.0, 5_000_000.0),
        FAR,
        Coord::new(1_000_000.0, 5_000_000.0),
    ]]]);
    for far in [false, true] {
        // Robinson's numbers written as they are, then labelled Robinson by the .prj, as a file
        // made elsewhere would be: a writer converting to longitude/latitude refuses the far one.
        let mut ds = held("area", GeometryType::Polygon, vec![square.clone()], far.then(|| far_square.clone()));
        ds.held_in = Crs::LonLat;
        ds.crs = None;
        let path = dir.join(format!("area-{far}.shp"));
        rustgis_data::io::write_path(&ds, &path).unwrap();
        std::fs::write(path.with_extension("prj"), rustgis_data::raster::wkt::to_wkt(robinson()).unwrap()).unwrap();
        let loaded = crate::cmd::stac_area::load_file(path.to_str().unwrap()).unwrap();
        // The reader keeps the far square as the file holds it and says so (#2032 C2f-4b), and the
        // search area leaves it out; a clean file says nothing of it.
        let said = loaded.notices.iter().any(|n| n.contains("longitude/latitude"));
        assert_eq!(said, far, "{:?}", loaded.notices);
        if far {
            assert!(loaded.notices.iter().any(|n| n.contains("kept as the file holds it")), "{:?}", loaded.notices);
        }
    }
    std::fs::remove_dir_all(&dir).ok();
}
