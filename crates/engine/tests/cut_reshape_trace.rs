//! Cut Polygon, Reshape and Trace through their commands: attributes carried to the pieces, one
//! undo step, and refusals that leave the layer as it was.
use rustgis_engine::{
    Project, Session,
    data::{Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value as Cell},
    geom::Coord,
};
use serde_json::{Value, json};

/// A closed rectangle.
fn rectangle(left: f64, bottom: f64, right: f64, top: f64) -> Geometry {
    Geometry::Polygon(vec![vec![vec![
        Coord::new(left, bottom),
        Coord::new(right, bottom),
        Coord::new(right, top),
        Coord::new(left, top),
        Coord::new(left, bottom),
    ]]])
}

/// One layer of the given shapes, each with a name, editing on.
fn fixture(kind: GeometryType, shapes: Vec<Geometry>) -> (Session, u32) {
    let mut s = Session::new(Project::new("Sketch edits"));
    let mut data = Dataset::new("Parcels", kind, vec![Field::new("name", FieldType::Text)]);
    data.features = shapes
        .into_iter()
        .enumerate()
        .map(|(i, g)| Feature { id: i as u64, geometry: Some(g), attrs: vec![Cell::Text(format!("p{i}"))] })
        .collect::<Vec<_>>()
        .into();
    let id = s.map_mut().add_dataset(data);
    s.active_layer = Some(id);
    s.run("edit.enable", &json!({"on": true})).unwrap();
    s.clear_history();
    (s, id)
}

/// The layer's whole features, so tests see attribute, id and untouched-row changes too.
fn features(s: &Session, id: u32) -> Vec<Feature> {
    s.map().layer(id).unwrap().dataset.features.iter().cloned().collect()
}

/// A GeoJSON LineString, the `line` parameter of Cut Polygon and Reshape.
fn line(coords: &[[f64; 2]]) -> Value {
    json!({"type": "LineString", "coordinates": coords})
}

#[test]
fn cut_polygon_splits_the_selected_parcel_keeping_its_attributes() {
    let (mut s, id) = fixture(GeometryType::Polygon, vec![rectangle(0., 0., 1., 1.), rectangle(2., 0., 3., 1.)]);
    let before = features(&s, id);
    s.run("select.rows", &json!({"layer": id, "rows": [0]})).unwrap();
    // New features get an elevation; the pieces of a cut keep the feature's own values.
    s.run("edit.elevation", &json!({"mode": "constant", "z": 5})).unwrap();
    let mark = s.notice_mark();
    let r = s.run("edit.cutPolygon", &json!({"line": line(&[[0.3, -1.], [0.3, 2.]])})).unwrap();
    assert!(s.notices_since(mark).is_empty(), "everything selected was cut: no notice");
    assert_eq!(r, json!({"cut": 1, "pieces": 2, "missed": 0}));
    let after = features(&s, id);
    assert_eq!(after.len(), 3);
    // The new piece copies the parcel's attributes; the other parcel is untouched.
    assert_eq!(after[2].attrs, before[0].attrs);
    assert!(s.map().layer(id).unwrap().dataset.field_index("z").is_none(), "no elevation field added for the pieces");
    assert_eq!(after[1], before[1]);
    assert_eq!(s.map().layer(id).unwrap().selection, vec![0, 2]);
    // One undo step puts it back.
    s.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(features(&s, id), before);
}

#[test]
fn a_cut_that_misses_or_bad_lines_change_nothing() {
    let (mut s, id) = fixture(GeometryType::Polygon, vec![rectangle(0., 0., 1., 1.)]);
    let before = features(&s, id);
    s.run("select.rows", &json!({"layer": id, "rows": [0]})).unwrap();
    for p in [
        json!({"line": line(&[[5., -1.], [5., 2.]])}),
        json!({"line": line(&[[0.5, -1.], [0.5, 0.5]])}),
        json!({"line": {"type": "Point", "coordinates": [0, 0]}}),
        json!({"line": line(&[[0.5, -1.]])}),
        json!({"line": line(&[[500., -1.], [500., 2.]])}),
        json!({}),
    ] {
        assert!(s.run("edit.cutPolygon", &p).is_err(), "{p}");
        assert!(s.run("edit.reshape", &p).is_err(), "{p}");
    }
    assert_eq!(features(&s, id), before);
}

#[test]
fn a_refused_cut_of_one_selected_parcel_cuts_none_of_them() {
    // The second parcel has a corrupt vertex (as from a damaged file): its cut is refused.
    let mut corrupt = rectangle(0., 1., 1., 2.);
    if let Geometry::Polygon(p) = &mut corrupt {
        p[0][0][2] = Coord::new(f64::NAN, 2.);
    }
    let (mut s, id) = fixture(GeometryType::Polygon, vec![rectangle(0., 0., 1., 1.), corrupt]);
    let before = features(&s, id);
    s.run("select.rows", &json!({"layer": id, "rows": [0, 1]})).unwrap();
    assert!(s.run("edit.cutPolygon", &json!({"line": line(&[[0.5, -1.], [0.5, 2.]])})).is_err());
    // (NaN never equals itself, so the healthy parcel and the row count are compared.)
    let after = features(&s, id);
    assert_eq!((after.len(), &after[0]), (before.len(), &before[0]));
    // A selected parcel the line misses is counted, not cut.
    let (mut s, id) = fixture(GeometryType::Polygon, vec![rectangle(0., 0., 1., 1.), rectangle(5., 0., 6., 1.)]);
    s.run("select.rows", &json!({"layer": id, "rows": [0, 1]})).unwrap();
    let mark = s.notice_mark();
    let r = s.run("edit.cutPolygon", &json!({"line": line(&[[0.5, -1.], [0.5, 2.]])})).unwrap();
    assert_eq!(r, json!({"cut": 1, "pieces": 2, "missed": 1}));
    let notices = s.notices_since(mark);
    assert_eq!(notices.len(), 1, "the parcel left uncut is said");
    assert!(notices[0].text.contains("Cut Polygon left 1 selected feature(s)"), "{notices:?}");
}

/// A parcel unwrapped past the antimeridian (longitudes past 180°, within the ±360° taken) cuts.
#[test]
fn cut_polygon_accepts_longitudes_past_180() {
    let (mut s, id) = fixture(GeometryType::Polygon, vec![rectangle(265., 0., 275., 1.)]);
    s.run("select.rows", &json!({"layer": id, "rows": [0]})).unwrap();
    let r = s.run("edit.cutPolygon", &json!({"line": line(&[[270., -1.], [270., 2.]])})).unwrap();
    assert_eq!(r, json!({"cut": 1, "pieces": 2, "missed": 0}));
    let after = features(&s, id);
    assert_eq!(after.len(), 2);
    // One piece on each side of 270° (sorted by west edge: they're the same size).
    let mut spans: Vec<(f64, f64)> = after
        .iter()
        .map(|f| {
            let Some(Geometry::Polygon(p)) = &f.geometry else { panic!("not a polygon") };
            p[0][0].iter().fold((f64::MAX, f64::MIN), |(lo, hi), c| (lo.min(c.x), hi.max(c.x)))
        })
        .collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
    assert!(near(spans[0].0, 265.) && near(spans[0].1, 270.) && near(spans[1].0, 270.) && near(spans[1].1, 275.), "{spans:?}");
}

#[test]
fn reshape_moves_a_boundary_and_needs_one_feature() {
    let (mut s, id) = fixture(GeometryType::Polygon, vec![rectangle(0., 0., 1., 1.), rectangle(2., 0., 3., 1.)]);
    let bump = json!({"line": line(&[[0.8, 0.2], [1.5, 0.2], [1.5, 0.8], [0.8, 0.8]])});
    s.run("select.rows", &json!({"layer": id, "rows": [0, 1]})).unwrap();
    assert!(s.run("edit.reshape", &bump).is_err(), "two features selected");
    s.run("select.rows", &json!({"layer": id, "rows": [0]})).unwrap();
    let mark = s.notice_mark();
    s.run("edit.reshape", &bump).unwrap();
    let Some(Geometry::Polygon(p)) = &features(&s, id)[0].geometry else { panic!("not a polygon") };
    assert!(p[0][0].iter().any(|c| (c.x - 1.5).abs() < 1e-9), "{p:?}");
    assert_eq!(s.notices_since(mark).len(), 0, "no hole removed, no notice");
    // A reshape that would make the ring cross itself is refused.
    let cross = json!({"line": line(&[[0.5, -0.5], [0.5, 1.5], [0.7, 1.5], [0.7, -0.5], [0.6, -0.5], [0.6, 2.0]])});
    let before = features(&s, id);
    assert!(s.run("edit.reshape", &cross).is_err());
    assert_eq!(features(&s, id), before);
}

#[test]
fn reshaping_away_a_hole_says_so() {
    let mut g = rectangle(0., 0., 1., 1.);
    if let Geometry::Polygon(p) = &mut g {
        let hole = [(0.1, 0.4), (0.1, 0.6), (0.2, 0.6), (0.2, 0.4), (0.1, 0.4)];
        p[0].push(hole.iter().map(|(x, y)| Coord::new(*x, *y)).collect());
    }
    let (mut s, id) = fixture(GeometryType::Polygon, vec![g]);
    s.run("select.rows", &json!({"layer": id, "rows": [0]})).unwrap();
    let mark = s.notice_mark();
    let r = s.run("edit.reshape", &json!({"line": line(&[[0.3, -1.], [0.3, 2.]])})).unwrap();
    assert_eq!(r["holesRemoved"], 1);
    let notices = s.notices_since(mark);
    assert_eq!(notices.len(), 1, "one notice, with the count");
    assert!(notices[0].text.contains("Reshape removed 1 hole"), "{notices:?}");
}

#[test]
fn trace_finds_the_edge_near_both_points() {
    let (mut s, id) = fixture(GeometryType::Polygon, vec![rectangle(0., 0., 1., 1.)]);
    let r = s.run("edit.trace", &json!({"layer": id, "row": 0, "from": [0.5, 0.], "to": [1., 0.5]})).unwrap();
    assert_eq!(r["path"], json!([[0.5, 0.0], [1.0, 0.0], [1.0, 0.5]]));
    assert!(r["fromOffM"].as_f64().is_some_and(|d| d < 1e-6) && r["toOffM"].as_f64().is_some_and(|d| d < 1e-6), "{r}");
    // A point off the edge is moved onto it, and how far is said.
    let r = s.run("edit.trace", &json!({"layer": id, "row": 0, "from": [0.5, -0.1], "to": [1., 0.5]})).unwrap();
    assert!(r["fromOffM"].as_f64().is_some_and(|d| d > 10_000.0), "{r}");
    // Without a row, the edge is found near both points (a generous tolerance at this zoom).
    s.zoom_to(&rustgis_engine::geom::Extent::new(-1., -1., 2., 2.));
    let r = s.run("edit.trace", &json!({"from": [0.5, 0.], "to": [1., 0.5], "tolerancePx": 20})).unwrap();
    assert_eq!(r["row"], 0);
    assert!(s.run("edit.trace", &json!({"from": [0.5, 0.5], "to": [0.6, 0.6], "tolerancePx": 1})).is_err());
    // A given layer limits the search: another layer's edge isn't taken.
    let other = s.map_mut().add_dataset(Dataset::new("Empty", GeometryType::Polygon, vec![]));
    let p = json!({"layer": other, "from": [0.5, 0.], "to": [1., 0.5], "tolerancePx": 20});
    assert!(s.run("edit.trace", &p).is_err());
}
