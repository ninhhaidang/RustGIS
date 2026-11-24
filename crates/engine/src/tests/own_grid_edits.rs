//! Move, Rotate, Scale and Traverse on a layer held in a projected system work in its own grid
//! (#2032 C2f-6): distances flat in its unit, directions from its grid north, by its axes. A
//! layer in longitude/latitude, Web Mercator or a world projection is edited as before.

use crate::{Project, Session};
use rustgis_data::raster::Crs;
use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value};
use serde_json::json;

/// UTM zone 32N, in metres, east then north.
const UTM: Crs = Crs::Epsg(32632);

/// A square of 0.005° near Ingolstadt (2.5° east of UTM 32N's central meridian, its grid turned
/// some 1.9° from north there) and a line through it, in longitude/latitude.
fn square() -> Geometry {
    let (x, y) = (11.5, 48.7);
    Geometry::Polygon(vec![vec![vec![
        Coord::new(x, y),
        Coord::new(x + 0.005, y),
        Coord::new(x + 0.005, y + 0.005),
        Coord::new(x, y + 0.005),
        Coord::new(x, y),
    ]]])
}

fn line() -> Geometry {
    Geometry::Line(vec![vec![Coord::new(11.49, 48.69), Coord::new(11.515, 48.705), Coord::new(11.53, 48.73)]])
}

/// A dataset `name` of `kind` with `geoms` (longitude/latitude) held in `crs`.
fn held(name: &str, kind: GeometryType, geoms: Vec<Geometry>, crs: Crs) -> Dataset {
    let mut ds = Dataset::new(name, kind, vec![Field::new("name", FieldType::Text)]);
    for (i, g) in geoms.into_iter().enumerate() {
        ds.push(Some(g), vec![Value::Text(format!("{name} {i}"))]);
    }
    ds.hold_in(crs).unwrap();
    ds
}

/// A session with editing on and `layers` added.
fn session(layers: Vec<Dataset>) -> Session {
    let mut s = Session::new(Project::default());
    for ds in layers {
        crate::cmd::layer::add_dataset(&mut s, ds);
    }
    s.map_mut().editing.settings.enabled = true;
    s
}

fn select_all(s: &mut Session) {
    for l in &mut s.map_mut().layers {
        l.selection = (0..l.dataset.features.len()).collect();
    }
}

/// Every vertex of layer `name`, as it holds them.
fn stored(s: &Session, name: &str) -> Vec<Coord> {
    let mut out = Vec::new();
    for f in &s.map().layer_by_name(name).unwrap().dataset.features {
        if let Some(g) = &f.geometry {
            g.for_each_coord(|c| out.push(c));
        }
    }
    out
}

/// What `cmd` with `p` said.
fn run(s: &mut Session, cmd: &str, p: serde_json::Value) -> Vec<String> {
    let mark = s.notice_mark();
    s.run(cmd, &p).unwrap_or_else(|e| panic!("{cmd}: {e}"));
    s.notices_since(mark).iter().map(|n| n.text.clone()).collect()
}

/// A move adds the distance to every stored coordinate, exactly as a step on the grid, in the
/// layers of one projected system.
#[test]
fn a_move_steps_along_the_layers_own_grid() {
    let mut s = session(vec![
        held("Parcels", GeometryType::Polygon, vec![square()], UTM),
        held("Roads", GeometryType::Polyline, vec![line()], UTM),
    ]);
    let (before, roads) = (stored(&s, "Parcels"), stored(&s, "Roads"));
    select_all(&mut s);
    let said = run(&mut s, "edit.move", json!({"dx": 250.0, "dy": -100.0}));
    assert!(said.iter().all(|t| !t.contains("more than one coordinate system")), "{said:?}");
    let after = stored(&s, "Parcels");
    assert_eq!(after, before.iter().map(|c| Coord::new(c.x + 250.0, c.y - 100.0)).collect::<Vec<_>>());
    assert_eq!(stored(&s, "Roads"), roads.iter().map(|c| Coord::new(c.x + 250.0, c.y - 100.0)).collect::<Vec<_>>());
    for l in &mut s.map_mut().layers {
        l.selection = if l.name == "Parcels" { vec![0] } else { Vec::new() };
    }
    // In feet, the same step in metres.
    run(&mut s, "edit.move", json!({"dx": 1000.0, "dy": 0.0, "units": "Feet"}));
    let feet = stored(&s, "Parcels");
    for (a, b) in feet.iter().zip(&after) {
        assert!((a.x - b.x - 304.8).abs() < 1e-9 && a.y == b.y, "{a:?} vs {b:?}");
    }
    // Ground to grid scales and turns the step before it is taken.
    run(&mut s, "edit.groundToGrid", json!({"scale": 0.9996, "rotation": 90.0}));
    run(&mut s, "edit.move", json!({"dx": 0.0, "dy": 1000.0}));
    for (a, b) in stored(&s, "Parcels").iter().zip(&feet) {
        assert!((a.x - b.x - 999.6).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6, "{a:?} vs {b:?}");
    }
    // Undo, step by step (Ground To Grid is one too), puts it back as stored, to the bit.
    for _ in 0..8 {
        if stored(&s, "Parcels") == before || s.run("edit.undo", &json!(null)).is_err() {
            break;
        }
    }
    assert_eq!(stored(&s, "Parcels"), before, "undo puts it back as stored, to the bit");
    // A move of no distance on that layer alone changes nothing, as before.
    for l in &mut s.map_mut().layers {
        l.selection = if l.name == "Parcels" { vec![0] } else { Vec::new() };
    }
    let e = s.run("edit.move", &json!({"dx": 0.0, "dy": 0.0})).unwrap_err();
    assert!(e.contains("no distance"), "{e}");
}

/// In a system in US survey feet, a move in metres goes as many of its feet; in one whose axes
/// point west and south, east is the way x falls.
#[test]
fn a_move_follows_the_systems_unit_and_axes() {
    // NAD83 / New York Long Island (ftUS), near New York.
    let ny = held("Lots", GeometryType::Polygon, vec![square_at(-73.9, 40.75)], Crs::Epsg(2263));
    // Hartebeesthoek94 / Lo29, x west and y south, near Johannesburg.
    let lo = held("Erven", GeometryType::Polygon, vec![square_at(28.5, -26.2)], Crs::Epsg(2053));
    assert_eq!(lo.coord_crs().axis_directions(), [(-1.0, 0.0), (0.0, -1.0)], "Lo29 points west and south");
    let mut s = session(vec![ny, lo]);
    let (ny0, lo0) = (stored(&s, "Lots"), stored(&s, "Erven"));
    // One layer at a time: two systems at once move together in Web Mercator.
    for name in ["Lots", "Erven"] {
        for l in &mut s.map_mut().layers {
            l.selection = if l.name == name { vec![0] } else { Vec::new() };
        }
        run(&mut s, "edit.move", json!({"dx": 100.0, "dy": 50.0}));
    }
    let ft = 1200.0 / 3937.0;
    for (a, b) in stored(&s, "Lots").iter().zip(&ny0) {
        assert!((a.x - b.x - 100.0 / ft).abs() < 1e-6 && (a.y - b.y - 50.0 / ft).abs() < 1e-6, "{a:?} vs {b:?}");
    }
    for (a, b) in stored(&s, "Erven").iter().zip(&lo0) {
        assert!((a.x - b.x + 100.0).abs() < 1e-9 && (a.y - b.y + 50.0).abs() < 1e-9, "{a:?} vs {b:?}");
    }
}

fn square_at(x: f64, y: f64) -> Geometry {
    Geometry::Polygon(vec![vec![vec![
        Coord::new(x, y),
        Coord::new(x + 0.002, y),
        Coord::new(x + 0.002, y + 0.002),
        Coord::new(x, y + 0.002),
        Coord::new(x, y),
    ]]])
}

/// Rotate turns clockwise with grid north up, and Scale scales, about the middle of the feature's
/// extent in its own coordinates; with the axes swapped (north first), clockwise still turns
/// north to east.
#[test]
fn rotate_and_scale_work_about_the_middle_of_the_own_extent() {
    let mut s = session(vec![held("Roads", GeometryType::Polyline, vec![line()], UTM)]);
    let before = stored(&s, "Roads");
    let mid = extent_middle(&before);
    select_all(&mut s);
    run(&mut s, "edit.rotate", json!({"angle": 90.0}));
    for (a, b) in stored(&s, "Roads").iter().zip(&before) {
        // North of the middle turns east: (e, n) → (n, −e).
        let want = Coord::new(mid.x + (b.y - mid.y), mid.y - (b.x - mid.x));
        assert!((a.x - want.x).abs() < 1e-6 && (a.y - want.y).abs() < 1e-6, "{a:?} vs {want:?}");
    }
    run(&mut s, "edit.undo", json!(null));
    run(&mut s, "edit.scale", json!({"factor": 2.0}));
    for (a, b) in stored(&s, "Roads").iter().zip(&before) {
        let want = Coord::new(mid.x + (b.x - mid.x) * 2.0, mid.y + (b.y - mid.y) * 2.0);
        assert!((a.x - want.x).abs() < 1e-6 && (a.y - want.y).abs() < 1e-6, "{a:?} vs {want:?}");
    }
    // UTM 32N with north first: x north, y east.
    let neu = Crs::parse("+proj=utm +zone=32 +datum=WGS84 +units=m +axis=neu +no_defs").unwrap();
    assert_eq!(neu.axis_directions(), [(0.0, 1.0), (1.0, 0.0)]);
    let mut s = session(vec![held("Roads", GeometryType::Polyline, vec![line()], neu)]);
    let before = stored(&s, "Roads");
    let mid = extent_middle(&before);
    select_all(&mut s);
    run(&mut s, "edit.rotate", json!({"angle": 90.0}));
    for (a, b) in stored(&s, "Roads").iter().zip(&before) {
        // x is north, y east: (n, e) → (−e, n).
        let want = Coord::new(mid.x - (b.y - mid.y), mid.y + (b.x - mid.x));
        assert!((a.x - want.x).abs() < 1e-6 && (a.y - want.y).abs() < 1e-6, "{a:?} vs {want:?}");
    }
}

fn extent_middle(cs: &[Coord]) -> Coord {
    let (mut lo, mut hi) = (Coord::new(f64::INFINITY, f64::INFINITY), Coord::new(f64::NEG_INFINITY, f64::NEG_INFINITY));
    for c in cs {
        lo = Coord::new(lo.x.min(c.x), lo.y.min(c.y));
        hi = Coord::new(hi.x.max(c.x), hi.y.max(c.y));
    }
    Coord::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0)
}

/// A feature of a projected layer with a place that can't be given in longitude/latitude moves
/// with the rest, since nothing is converted; no notice counts it.
#[test]
fn a_feature_with_no_lonlat_place_moves_on_the_grid() {
    let mut ds = held("Roads", GeometryType::Polyline, vec![line()], UTM);
    let far = Coord::new(1e12, 1e12);
    assert_eq!(ds.lonlat_conversion().unwrap().point(far), None, "the far place truly has none in longitude/latitude");
    ds.push(Some(Geometry::Line(vec![vec![Coord::new(700_000.0, 5_400_000.0), far]])), vec![Value::Text("far".into())]);
    let mut s = session(vec![ds]);
    select_all(&mut s);
    let said = run(&mut s, "edit.move", json!({"dx": 10.0, "dy": 0.0}));
    assert!(said.iter().all(|t| !t.contains("can't be given")), "{said:?}");
    assert_eq!(stored(&s, "Roads").last().copied(), Some(Coord::new(1e12 + 10.0, 1e12)));
}

/// A traverse into a projected layer starts at the given longitude/latitude converted into its
/// system, and steps each leg on its grid: a closed polygon of exact metres.
#[test]
fn a_traverse_is_drawn_on_the_layers_grid() {
    let mut s = session(vec![held("Lots", GeometryType::Polygon, vec![square()], UTM)]);
    let start = Crs::LonLat.to(UTM).unwrap().point(Coord::new(11.6, 48.8)).unwrap();
    let r = s
        .run(
            "edit.traverse",
            &json!({"layer": "Lots", "start": [11.6, 48.8], "legs": [{"distance": 100, "bearing": 90}, {"distance": 50, "bearing": 0}, {"distance": 100, "bearing": 270}]}),
        )
        .unwrap();
    let row = r["row"].as_u64().unwrap() as usize;
    let Some(Geometry::Polygon(p)) = s.map().layer_by_name("Lots").unwrap().dataset.features[row].geometry.clone() else {
        panic!("a polygon")
    };
    let want = [(0.0, 0.0), (100.0, 0.0), (100.0, 50.0), (0.0, 50.0), (0.0, 0.0)];
    assert_eq!(p[0][0].len(), want.len());
    for (c, (dx, dy)) in p[0][0].iter().zip(want) {
        assert!((c.x - start.x - dx).abs() < 1e-6 && (c.y - start.y - dy).abs() < 1e-6, "{c:?}");
    }
    assert_eq!(p[0][0].first(), p[0][0].last(), "closed on its start, to the bit");
}

/// A selection spanning a projected layer and one in longitude/latitude moves both in Web
/// Mercator, as before, so a corner they share stays together, and says why.
#[test]
fn a_selection_across_systems_moves_together_as_before() {
    let corner = Geometry::Line(vec![vec![Coord::new(11.5, 48.7), Coord::new(11.52, 48.71)]]);
    let mut s = session(vec![
        held("Parcels", GeometryType::Polygon, vec![square()], UTM),
        held("Roads", GeometryType::Polyline, vec![corner.clone()], Crs::LonLat),
    ]);
    select_all(&mut s);
    let said = run(&mut s, "edit.move", json!({"dx": 250.0, "dy": -100.0}));
    assert!(said.iter().any(|t| t.contains("more than one coordinate system") && t.contains("'Parcels'")), "{said:?}");
    let parcel = s.map().layer_by_name("Parcels").unwrap().dataset.lonlat_conversion().unwrap().point(stored(&s, "Parcels")[0]).unwrap();
    let road = stored(&s, "Roads")[0];
    assert!((parcel.x - road.x).abs() < 1e-8 && (parcel.y - road.y).abs() < 1e-8, "{parcel:?} vs {road:?}");
}

/// A traverse begun at a stored corner, given as that corner reads in longitude/latitude, starts
/// on the corner as stored, to the bit, as an added feature does.
#[test]
fn a_traverse_from_a_corner_starts_on_it_as_stored() {
    let mut s = session(vec![held("Lots", GeometryType::Polygon, vec![square()], UTM)]);
    let corner = stored(&s, "Lots")[1];
    let lonlat = s.map().layer_by_name("Lots").unwrap().dataset.lonlat_conversion().unwrap().point(corner).unwrap();
    let r = s
        .run(
            "edit.traverse",
            &json!({"layer": "Lots", "start": [lonlat.x, lonlat.y], "legs": [{"distance": 100, "bearing": 90}, {"distance": 50, "bearing": 0}]}),
        )
        .unwrap();
    let Some(Geometry::Polygon(p)) =
        s.map().layer_by_name("Lots").unwrap().dataset.features[r["row"].as_u64().unwrap() as usize].geometry.clone()
    else {
        panic!("a polygon")
    };
    assert_eq!(p[0][0][0], corner);
    assert_eq!(p[0][0][1], Coord::new(corner.x + 100.0, corner.y));
}
