//! Cut Polygon, Reshape, Split, Merge and Trace on a layer held in a projected system work on its
//! own segments (#2032 C2f-7): where they run straight in its own coordinates, not in
//! longitude/latitude or Web Mercator. Every place the edit doesn't make stays as stored, and the
//! places it makes lie on the layer's segments there.

use crate::{Project, Session};
use rustgis_data::raster::Crs;
use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value};
use serde_json::json;

/// UTM zone 32N, in metres.
const UTM: Crs = Crs::Epsg(32632);

/// A dataset `name` of `kind` with `geoms` given in UTM 32N, held there.
fn held(name: &str, kind: GeometryType, geoms: Vec<Geometry>) -> Dataset {
    held_in(name, kind, geoms, UTM)
}

/// [`held`] in `crs`: the coordinates given are stored as they are, in it.
fn held_in(name: &str, kind: GeometryType, geoms: Vec<Geometry>, crs: Crs) -> Dataset {
    let mut ds = Dataset::new(name, kind, vec![Field::new("name", FieldType::Text)]);
    for (i, g) in geoms.into_iter().enumerate() {
        ds.push(Some(g), vec![Value::Text(format!("{name} {i}"))]);
    }
    ds.held_in = crs;
    ds.crs = Some(crs);
    ds
}

/// A square of `side` metres with its south-west corner at (`x`, `y`) in UTM 32N, anticlockwise.
fn square(x: f64, y: f64, side: f64) -> Geometry {
    let c = |dx: f64, dy: f64| Coord::new(x + dx, y + dy);
    Geometry::Polygon(vec![vec![vec![c(0.0, 0.0), c(side, 0.0), c(side, side), c(0.0, side), c(0.0, 0.0)]]])
}

fn session(ds: Dataset) -> Session {
    let mut s = Session::new(Project::default());
    crate::cmd::layer::add_dataset(&mut s, ds);
    s.map_mut().editing.settings.enabled = true;
    let l = &mut s.map_mut().layers[0];
    l.selection = (0..l.dataset.features.len()).collect();
    s
}

/// Every vertex of the layer, as it holds them.
fn stored(s: &Session) -> Vec<Coord> {
    let mut out = Vec::new();
    for f in &s.map().layers[0].dataset.features {
        if let Some(g) = &f.geometry {
            g.for_each_coord(|c| out.push(c));
        }
    }
    out
}

/// A UTM 32N place in longitude/latitude, as a line drawn on the map gives it.
fn lonlat(c: Coord) -> [f64; 2] {
    let c = UTM.to(Crs::LonLat).unwrap().point(c).unwrap();
    [c.x, c.y]
}

/// How far `c` is from the nearest of `segments`' straight segments, in metres of the grid.
fn off(c: Coord, segments: &[(Coord, Coord)]) -> f64 {
    segments
        .iter()
        .map(|(a, b)| {
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let t = (((c.x - a.x) * dx + (c.y - a.y) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            (a.x + t * dx - c.x).hypot(a.y + t * dy - c.y)
        })
        .fold(f64::INFINITY, f64::min)
}

/// The square's sides.
fn sides(x: f64, y: f64, side: f64) -> Vec<(Coord, Coord)> {
    let c = |dx: f64, dy: f64| Coord::new(x + dx, y + dy);
    vec![(c(0.0, 0.0), c(side, 0.0)), (c(side, 0.0), c(side, side)), (c(side, side), c(0.0, side)), (c(0.0, side), c(0.0, 0.0))]
}

/// A 20 km square well east of UTM 32N's central meridian, where its grid lines curve in
/// longitude/latitude by metres over that length.
const X: f64 = 700_000.0;
const Y: f64 = 5_300_000.0;
const SIDE: f64 = 20_000.0;

#[test]
fn a_cut_meets_the_layers_own_sides() {
    let mut s = session(held("Parcels", GeometryType::Polygon, vec![square(X, Y, SIDE)]));
    let before = stored(&s);
    // A line drawn north–south across the square, a quarter of the way in.
    let line = [lonlat(Coord::new(X + 5_000.0, Y - 1_000.0)), lonlat(Coord::new(X + 5_000.0, Y + SIDE + 1_000.0))];
    let r = s.run("edit.cutPolygon", &json!({"line": {"type": "LineString", "coordinates": line}})).unwrap();
    assert_eq!(r["pieces"], 2);
    let after = stored(&s);
    let made: Vec<Coord> = after.iter().copied().filter(|c| !before.contains(c)).collect();
    // The square's corners stay as stored; the cut makes its two points on the square's sides,
    // where they run straight in UTM, and on the line converted into it.
    assert!(before.iter().all(|c| after.contains(c)));
    assert!(!made.is_empty());
    for c in &made {
        assert!(off(*c, &sides(X, Y, SIDE)) < 1e-6, "{c:?} is {} m off the square's sides", off(*c, &sides(X, Y, SIDE)));
        assert!((c.x - (X + 5_000.0)).abs() < 1e-3, "{c:?}");
    }
}

#[test]
fn a_reshape_meets_the_layers_own_sides() {
    let mut s = session(held("Parcels", GeometryType::Polygon, vec![square(X, Y, SIDE)]));
    let before = stored(&s);
    // A bump out of the south side, drawn from inside to inside.
    let line = [
        lonlat(Coord::new(X + 5_000.0, Y + 2_000.0)),
        lonlat(Coord::new(X + 5_000.0, Y - 3_000.0)),
        lonlat(Coord::new(X + 15_000.0, Y - 3_000.0)),
        lonlat(Coord::new(X + 15_000.0, Y + 2_000.0)),
    ];
    s.run("edit.reshape", &json!({"line": {"type": "LineString", "coordinates": line}})).unwrap();
    let after = stored(&s);
    // Each new place once: the ring's first is also its last.
    let mut crossings: Vec<Coord> = Vec::new();
    for c in after.iter().copied().filter(|c| !before.contains(c) && (c.y - Y).abs() < 1.0) {
        if !crossings.contains(&c) {
            crossings.push(c);
        }
    }
    assert_eq!(crossings.len(), 2, "{after:?}");
    for c in crossings {
        assert!(off(c, &sides(X, Y, SIDE)) < 1e-6, "{c:?}");
    }
    // The north corners, which the reshape didn't reach, stay as stored.
    assert!(after.contains(&Coord::new(X + SIDE, Y + SIDE)) && after.contains(&Coord::new(X, Y + SIDE)));
}

#[test]
fn a_split_is_on_the_layers_own_segment() {
    let road = Geometry::Line(vec![vec![Coord::new(X, Y), Coord::new(X + SIDE, Y + SIDE), Coord::new(X + 2.0 * SIDE, Y + SIDE)]]);
    let mut s = session(held("Roads", GeometryType::Polyline, vec![road]));
    let before = stored(&s);
    let at = lonlat(Coord::new(X + 8_000.0, Y + 8_000.0));
    s.run("edit.split", &json!({"lon": at[0], "lat": at[1]})).unwrap();
    let after = stored(&s);
    assert_eq!(s.map().layers[0].dataset.features.len(), 2);
    assert!(before.iter().all(|c| after.contains(c)));
    let made: Vec<Coord> = after.iter().copied().filter(|c| !before.contains(c)).collect();
    assert!(!made.is_empty());
    for c in made {
        // On the diagonal as it runs straight in UTM, at the point asked for.
        assert!((c.x - c.y + (Y - X)).abs() / 2f64.sqrt() < 1e-6, "{c:?}");
        assert!((c.x - (X + 8_000.0)).abs() < 1e-3, "{c:?}");
    }
}

#[test]
fn a_merge_unions_the_layers_own_shapes_as_stored() {
    // Two squares sharing a side, as stored in UTM: merged, they are one square with no place
    // converted.
    let mut s = session(held("Parcels", GeometryType::Polygon, vec![square(X, Y, SIDE), square(X + SIDE, Y, SIDE)]));
    let before = stored(&s);
    s.run("edit.merge", &json!({})).unwrap();
    let l = &s.map().layers[0];
    assert_eq!(l.dataset.features.len(), 1);
    let Some(Geometry::Polygon(p)) = &l.dataset.features[0].geometry else { panic!() };
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(stored(&s).iter().all(|c| before.contains(c)), "{:?}", stored(&s));
}

#[test]
fn a_merge_meets_the_layers_own_sides_where_the_shapes_cross() {
    // A triangle reaching out of the square's east side: merged, the two points where its sides
    // cross the square's east side are new, on that side as it runs straight in UTM.
    let triangle = Geometry::Polygon(vec![vec![vec![
        Coord::new(X + 10_000.0, Y + 10_000.0),
        Coord::new(X + 30_000.0, Y + 4_000.0),
        Coord::new(X + 30_000.0, Y + 16_000.0),
        Coord::new(X + 10_000.0, Y + 10_000.0),
    ]]]);
    let mut s = session(held("Parcels", GeometryType::Polygon, vec![square(X, Y, SIDE), triangle]));
    let before = stored(&s);
    s.run("edit.merge", &json!({})).unwrap();
    let made: Vec<Coord> = stored(&s).into_iter().filter(|c| !before.contains(c)).collect();
    assert!(!made.is_empty());
    for c in made {
        assert!((c.x - (X + SIDE)).abs() < 1e-6, "{c:?} is off the square's east side");
    }
}

#[test]
fn a_split_in_a_layer_in_feet_is_on_its_own_segment() {
    // New York Long Island (US survey feet): a line split a few miles along, at the point asked.
    const NY: Crs = Crs::Epsg(2263);
    let (x, y) = (1_000_000.0, 200_000.0);
    let road = Geometry::Line(vec![vec![Coord::new(x, y), Coord::new(x + 60_000.0, y + 30_000.0)]]);
    let mut s = session(held_in("Roads", GeometryType::Polyline, vec![road], NY));
    let at = NY.to(Crs::LonLat).unwrap().point(Coord::new(x + 20_000.0, y + 10_000.0)).unwrap();
    s.run("edit.split", &json!({"lon": at.x, "lat": at.y})).unwrap();
    assert_eq!(s.map().layers[0].dataset.features.len(), 2);
    let made: Vec<Coord> =
        stored(&s).into_iter().filter(|c| *c != Coord::new(x, y) && *c != Coord::new(x + 60_000.0, y + 30_000.0)).collect();
    assert!(!made.is_empty());
    for c in made {
        // On the line (y = x / 2 from its start), at the point asked for.
        assert!(((c.y - y) - (c.x - x) / 2.0).abs() < 1e-6, "{c:?}");
        assert!((c.x - (x + 20_000.0)).abs() < 1e-3, "{c:?}");
    }
}

#[test]
fn a_trace_ends_on_the_layers_own_side() {
    let mut s = session(held("Parcels", GeometryType::Polygon, vec![square(X, Y, SIDE)]));
    let (from, to) = (lonlat(Coord::new(X + 3_000.0, Y + 1.0)), lonlat(Coord::new(X + SIDE - 1.0, Y + 12_000.0)));
    let r = s.run("edit.trace", &json!({"from": from, "to": to, "layer": 1, "row": 0})).unwrap();
    let to_utm = Crs::LonLat.to(UTM).unwrap();
    let path: Vec<Coord> = r["path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| to_utm.point(Coord::new(c[0].as_f64().unwrap(), c[1].as_f64().unwrap())).unwrap())
        .collect();
    // Its ends on the square's sides as they run straight in UTM (to the round trip's millimetre),
    // a metre from the points asked for, by the grid's metres.
    for end in [path[0], path[path.len() - 1]] {
        assert!(off(end, &sides(X, Y, SIDE)) < 1e-3, "{end:?}");
    }
    assert!((r["fromOffM"].as_f64().unwrap() - 1.0).abs() < 1e-3, "{r}");
}

#[test]
fn a_split_says_how_many_segments_it_couldnt_measure_and_a_clean_one_says_nothing() {
    let split = |road: Geometry| {
        let mut s = session(held("Roads", GeometryType::Polyline, vec![road]));
        let at = lonlat(Coord::new(X + 8_000.0, Y + 8_000.0));
        let mark = s.notice_mark();
        s.run("edit.split", &json!({"lon": at[0], "lat": at[1]})).unwrap();
        s.notices_since(mark).iter().map(|n| n.text.clone()).collect::<Vec<_>>()
    };
    // A segment to a vertex missing a coordinate is passed over, and said.
    let said = split(Geometry::Line(vec![vec![
        Coord::new(X, Y),
        Coord::new(X + SIDE, Y + SIDE),
        Coord::new(f64::NAN, Y + SIDE),
        Coord::new(X + 2.0 * SIDE, Y + SIDE),
    ]]));
    assert!(said.iter().any(|t| t.contains("2 segment(s)") && t.contains("could not be measured")), "{said:?}");
    let said = split(Geometry::Line(vec![vec![Coord::new(X, Y), Coord::new(X + SIDE, Y + SIDE)]]));
    assert!(said.iter().all(|t| !t.contains("could not be measured")), "{said:?}");
}
