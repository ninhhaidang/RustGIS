//! Edits of a layer held in its own system with a place that can't be given in
//! longitude/latitude (#2032 C2f-4b-1c): an edit of the selection leaves such a feature as it is
//! and says how many, rather than failing for all; an edit of that one feature is refused naming
//! it; a paste leaves out what the layer can't hold; a topology check leaves it out and says so.
//! The layers are held in Robinson, a world projection, so every edit converts them to
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
/// `far`, one more feature reaching past the projection's edge (its last row).
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

fn square() -> Geometry {
    Geometry::Polygon(vec![vec![vec![
        Coord::new(10.0, 45.0),
        Coord::new(10.03, 45.0),
        Coord::new(10.03, 45.03),
        Coord::new(10.0, 45.03),
        Coord::new(10.0, 45.0),
    ]]])
}

/// A session with the line layer "lines" (one line) and the polygon layer "areas" (one square),
/// both held in Robinson; with `far`, each has one more feature with a place that can't be given
/// in longitude/latitude. Every feature of both is selected.
fn session(far: bool) -> Session {
    let mut s = Session::new(crate::Project::default());
    let line = Geometry::Line(vec![vec![Coord::new(10.002, 45.002), Coord::new(10.038, 45.038)]]);
    let far_line = Geometry::Line(vec![vec![Coord::new(1_000_000.0, 5_000_000.0), FAR]]);
    let far_square = Geometry::Polygon(vec![vec![vec![
        Coord::new(1_000_000.0, 5_000_000.0),
        Coord::new(1_000_100.0, 5_000_000.0),
        FAR,
        Coord::new(1_000_000.0, 5_000_000.0),
    ]]]);
    let lines = held("lines", GeometryType::Polyline, vec![line], far.then_some(far_line));
    let areas = held("areas", GeometryType::Polygon, vec![square()], far.then_some(far_square));
    assert_eq!(lines.lonlat_conversion().unwrap().point(FAR), None, "the far place truly has none in longitude/latitude");
    crate::cmd::layer::add_dataset(&mut s, lines);
    crate::cmd::layer::add_dataset(&mut s, areas);
    for l in &mut s.map_mut().layers {
        l.selection = (0..l.dataset.features.len()).collect();
    }
    s
}

fn select(s: &mut Session, layer: &str, rows: &[usize]) {
    for l in &mut s.map_mut().layers {
        l.selection = if l.name == layer { rows.to_vec() } else { Vec::new() };
    }
}

fn geometry_of(s: &Session, layer: &str, row: usize) -> Option<Geometry> {
    s.map().layer_by_name(layer).unwrap().dataset.features.row(row).and_then(|r| r.geometry().cloned())
}

/// What `cmd` with `p` answers, and its notices that speak of a place that can't be given in
/// longitude/latitude (or held).
fn run(s: &mut Session, cmd: &str, p: serde_json::Value) -> (serde_json::Value, Vec<String>) {
    let mark = s.notice_mark();
    let out = s.run(cmd, &p).unwrap_or_else(|e| panic!("{cmd}: {e}"));
    let said = s
        .notices_since(mark)
        .iter()
        .map(|n| n.text.clone())
        .filter(|t| t.contains("can't be given in") || t.contains("can't hold"))
        .collect();
    (out, said)
}

/// Move, rotate and scale leave a feature they can't place as it is, and say how many per layer;
/// the rest change. Flip, which only reorders vertices, flips it in its own coordinates. A
/// selection of such features alone is refused, saying why. A layer whose places all convert
/// says nothing new.
#[test]
fn a_selection_edit_leaves_a_feature_it_cant_place_as_it_is() {
    for far in [false, true] {
        for (cmd, p) in [
            ("edit.move", json!({"dx": 100.0, "dy": 50.0})),
            ("edit.rotate", json!({"angle": 30.0})),
            ("edit.scale", json!({"factor": 1.5})),
        ] {
            let mut s = session(far);
            let (near, far_before) = (geometry_of(&s, "lines", 0), geometry_of(&s, "lines", 1));
            let (out, said) = run(&mut s, cmd, p);
            assert_ne!(geometry_of(&s, "lines", 0), near, "{cmd} changed the line it can place");
            if far {
                assert_eq!(geometry_of(&s, "lines", 1), far_before, "{cmd} left the far line as stored, to the bit");
                assert_eq!(said.len(), 1, "{cmd}: {said:?}");
                assert!(
                    said[0].contains("2 feature(s) of 'areas' (1), 'lines' (1)") && said[0].contains("left as they were"),
                    "{}",
                    said[0]
                );
                assert_eq!(out["modified"], 2, "{cmd}");
            } else {
                assert!(said.is_empty(), "{cmd}: {said:?}");
            }
        }
    }
    // Only the far line selected: refused, saying why.
    let mut s = session(true);
    select(&mut s, "lines", &[1]);
    let e = s.run("edit.move", &json!({"dx": 100.0, "dy": 0.0})).unwrap_err();
    assert!(
        e.contains("1 feature(s) of 'lines' (1) have a place that can't be given in longitude/latitude") && e.contains("nothing selected"),
        "{e}"
    );
    // Flip reverses it in its own coordinates.
    let (_, said) = run(&mut s, "edit.flip", json!({}));
    assert!(said.is_empty(), "{said:?}");
    let Some(Geometry::Line(parts)) = geometry_of(&s, "lines", 1) else { panic!("a line") };
    assert_eq!(parts[0], vec![FAR, Coord::new(1_000_000.0, 5_000_000.0)]);
}

/// Split and Cut Polygon leave a line or polygon they can't place whole, say so, and split or cut
/// the rest; with only such features selected they are refused, saying why.
#[test]
fn split_and_cut_leave_out_what_they_cant_place() {
    for far in [false, true] {
        let mut s = session(far);
        select(&mut s, "lines", if far { &[0, 1] } else { &[0] });
        let (_, said) = run(&mut s, "edit.split", json!({"lon": 10.02, "lat": 45.02}));
        assert_eq!(s.map().layer_by_name("lines").unwrap().dataset.features.len(), 2 + usize::from(far), "the line was split");
        if far {
            assert_eq!(said.len(), 1, "{said:?}");
            assert!(said[0].contains("1 feature(s) of 'lines' (1)") && said[0].contains("weren't split"), "{}", said[0]);
        } else {
            assert!(said.is_empty(), "{said:?}");
        }
        select(&mut s, "areas", if far { &[0, 1] } else { &[0] });
        let across = json!({"type": "LineString", "coordinates": [[10.015, 44.99], [10.015, 45.04]]});
        let (out, said) = run(&mut s, "edit.cutPolygon", json!({"line": across}));
        assert_eq!(out["cut"], 1);
        if far {
            assert_eq!(said.len(), 1, "{said:?}");
            assert!(said[0].contains("Cut Polygon: 1 feature(s) of 'areas' (1)") && said[0].contains("weren't cut"), "{}", said[0]);
        } else {
            assert!(said.is_empty(), "{said:?}");
        }
    }
    let mut s = session(true);
    select(&mut s, "lines", &[1]);
    let e = s.run("edit.split", &json!({"lon": 10.02, "lat": 45.02})).unwrap_err();
    assert!(e.contains("can't be given in longitude/latitude, so they can't be split"), "{e}");
    select(&mut s, "areas", &[1]);
    let across = json!({"type": "LineString", "coordinates": [[10.015, 44.99], [10.015, 45.04]]});
    let e = s.run("edit.cutPolygon", &json!({"line": across})).unwrap_err();
    assert!(e.contains("can't be given in longitude/latitude, so they can't be cut"), "{e}");
}

/// An edit of the one feature that can't be placed (a vertex, a reshape, a merge with it) is
/// refused naming the layer and the feature.
#[test]
fn an_edit_of_a_feature_it_cant_place_names_it() {
    let mut s = session(true);
    let id = s.map().layer_by_name("lines").unwrap().dataset.features.row(1).unwrap().id();
    let named = format!("lines: feature {id} has a place in");
    let e = s.run("edit.vertex", &json!({"layer": "lines", "row": 1, "action": "move", "index": 0, "lon": 10.0, "lat": 45.0})).unwrap_err();
    assert!(e.contains(&named) && e.contains("so it can't be edited there"), "{e}");
    select(&mut s, "lines", &[1]);
    let across = json!({"type": "LineString", "coordinates": [[10.0, 45.0], [10.01, 45.01]]});
    let e = s.run("edit.reshape", &json!({"line": across})).unwrap_err();
    assert!(e.contains(&named), "{e}");
    select(&mut s, "lines", &[0, 1]);
    let e = s.run("edit.merge", &json!({})).unwrap_err();
    assert!(e.contains(&named), "{e}");
}

/// Paste into a layer of another system leaves out a feature with a place that layer can't
/// hold, and says so; the rest are pasted.
#[test]
fn a_paste_leaves_out_what_the_layer_cant_hold() {
    let mut s = session(true);
    let mut target = Dataset::new("plain", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
    target.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]])), vec![Value::Null]);
    crate::cmd::layer::add_dataset(&mut s, target);
    select(&mut s, "lines", &[0, 1]);
    s.run("edit.copy", &json!(null)).unwrap();
    let (out, said) = run(&mut s, "edit.paste", json!({"layer": "plain"}));
    assert_eq!(out["pasted"], 1, "the line it can place");
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("Paste: 1 feature has a place that layer plain") && said[0].contains("so it wasn't pasted"), "{}", said[0]);
    assert_eq!(s.map().layer_by_name("plain").unwrap().dataset.features.len(), 2);
}

/// A topology check in longitude/latitude (a layer in a world projection) leaves out a feature
/// with a place that can't be given there and says so, rather than failing every rule.
#[test]
fn a_topology_check_leaves_out_what_it_cant_place() {
    for far in [false, true] {
        let mut s = session(far);
        s.run("topology.rules", &json!({"add": {"rule": "mustNotSelfIntersect", "layer": "areas"}})).unwrap();
        s.run("topology.rules", &json!({"add": {"rule": "mustNotHaveDangles", "layer": "lines"}})).unwrap();
        let mark = s.notice_mark();
        let (out, said) = run(&mut s, "topology.validate", json!({"toleranceM": 1.0}));
        // The near line's two ends dangle.
        assert_eq!(s.topology_errors.len(), 2, "{:?}", s.topology_errors);
        assert!(out["first"].as_array().unwrap().iter().all(|e| e["located"] == true), "{out}");
        // A feature left out may make its neighbours' issues: its rules are said to be incomplete.
        let incomplete = s.notices_since(mark).iter().any(|n| n.text.contains("weren't checked completely"));
        assert_eq!(incomplete, far);
        if far {
            assert_eq!(said.len(), 1, "{said:?}");
            assert!(
                said[0].contains("left out of the check")
                    && said[0].contains("1 feature(s) of 'areas'")
                    && said[0].contains("1 feature(s) of 'lines'"),
                "{}",
                said[0]
            );
        } else {
            assert!(said.is_empty(), "{said:?}");
        }
    }
}

/// A check in a layer's own projected system (UTM) that finds an issue at a place that can't be
/// given in longitude/latitude keeps it, with no place to zoom to, and says so, rather than
/// failing the check.
#[test]
fn a_topology_issue_it_cant_place_is_kept_without_a_place() {
    let mut s = Session::new(crate::Project::default());
    let mut lines = Dataset::new("paths", GeometryType::Polyline, vec![]);
    lines.push(Some(Geometry::Line(vec![vec![Coord::new(10.0, 45.0), Coord::new(10.01, 45.01)]])), vec![]);
    lines.hold_in(Crs::Epsg(32632)).unwrap();
    lines.push(Some(Geometry::Line(vec![vec![Coord::new(600_000.0, 5_000_000.0), FAR]])), vec![]);
    crate::cmd::layer::add_dataset(&mut s, lines);
    s.run("topology.rules", &json!({"add": {"rule": "mustNotHaveDangles", "layer": "paths"}})).unwrap();
    let (out, said) = run(&mut s, "topology.validate", json!({"toleranceM": 1.0}));
    assert_eq!(s.topology_errors.len(), 4, "every end dangles: {:?}", s.topology_errors);
    let errors = out["first"].as_array().unwrap();
    assert_eq!(errors.iter().filter(|e| e["located"] == false && e["lon"].is_null()).count(), 1, "{out}");
    let unlocated = s.topology_errors.iter().filter(|e| !e.lon.is_finite()).count();
    assert_eq!(unlocated, 1, "{:?}", s.topology_errors);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("1 topology issue(s) are at a place") && said[0].contains("can't zoom"), "{}", said[0]);
}
