//! A rectangle, circle, ellipse or curve dragged on the map is added as the drag previewed it
//! (#2032 C2f-6, `"frame": "view"`): built in the map's own coordinates as the view faces, on a
//! map in another projection and on one turned. A command that names no frame builds them as
//! every release did.

use crate::{Project, Session};
use rustgis_data::Geometry;
use rustgis_geom::Coord;
use serde_json::{Value, json};

/// A session on a map in Swiss LV95 (EPSG:2056), whose grid is turned from the meridians away
/// from its centre, looking at the corners used below.
fn on_lv95() -> Session {
    let mut s = Session::new(Project::default());
    s.run("map.projection", &json!({"type": "EPSG:2056"})).unwrap();
    s.run("map.goToXY", &json!({"lon": 9.0, "lat": 46.5, "scale": 2_000_000})).unwrap();
    s
}

/// The outer ring of the graphic `r` added, in longitude/latitude.
fn ring_of(s: &Session, r: &Value) -> Vec<Coord> {
    let (id, row) = (r["layer"].as_u64().unwrap() as u32, r["row"].as_u64().unwrap() as usize);
    match s.map().layer(id).unwrap().dataset.features[row].geometry.clone() {
        Some(Geometry::Polygon(p)) => p[0][0].clone(),
        other => panic!("{other:?}"),
    }
}

/// `c` as the camera places it, in map units from the view's centre: x right, y up on screen.
/// Read through the camera's own turn ([`rustgis_map::camera::Camera::ground_px`]), not the
/// graphics plane's, so a turn the wrong way in either shows.
fn in_view(s: &Session, c: Coord) -> Coord {
    let cam = &s.map().camera;
    let (u, v) = cam.ground_px(s.map().to_map(c).unwrap());
    Coord::new(u * cam.res, -v * cam.res)
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol * b.abs().max(1.0)
}

#[test]
fn a_dragged_rectangle_has_the_views_corners_on_any_map() {
    for bearing in [0.0, 30.0] {
        let mut s = on_lv95();
        s.run("map.setBearing", &json!({"bearing": bearing})).unwrap();
        let r = s.run("graphics.add", &json!({"shape": "rectangle", "points": [[8.0, 46.0], [10.0, 47.0]], "frame": "view"})).unwrap();
        let ring: Vec<Coord> = ring_of(&s, &r).into_iter().map(|c| in_view(&s, c)).collect();
        assert_eq!(ring.len(), 5);
        // Its sides run along the view's x and y.
        for w in ring.windows(2) {
            let (dx, dy) = (w[1].x - w[0].x, w[1].y - w[0].y);
            assert!(dx.abs() < 1e-4 || dy.abs() < 1e-4, "bearing {bearing}: side {dx} {dy}");
        }
        // And the corners dragged between are two of its corners.
        for c in [Coord::new(8.0, 46.0), Coord::new(10.0, 47.0)] {
            let v = in_view(&s, c);
            assert!(ring.iter().any(|r| close(r.x, v.x, 1e-7) && close(r.y, v.y, 1e-7)), "bearing {bearing}: {c:?}");
        }
    }
    // Named by no frame, it is a box of longitudes and latitudes, as before: on LV95 its sides
    // don't run along the map's grid.
    let mut s = on_lv95();
    let r = s.run("graphics.add", &json!({"shape": "rectangle", "points": [[8.0, 46.0], [10.0, 47.0]]})).unwrap();
    let ring = ring_of(&s, &r);
    assert_eq!(ring[..4], [Coord::new(8.0, 46.0), Coord::new(10.0, 46.0), Coord::new(10.0, 47.0), Coord::new(8.0, 47.0)]);
    let (a, b) = (in_view(&s, ring[0]), in_view(&s, ring[1]));
    assert!((b.y - a.y).abs() > 1_000.0, "a parallel isn't a grid line on LV95: {}", b.y - a.y);
}

#[test]
fn a_dragged_circle_and_ellipse_are_round_in_the_view() {
    let mut s = on_lv95();
    s.run("map.setBearing", &json!({"bearing": -20.0})).unwrap();
    let r = s.run("graphics.add", &json!({"shape": "circle", "points": [[9.0, 46.5], [9.5, 46.5]], "frame": "view"})).unwrap();
    let (c, e) = (in_view(&s, Coord::new(9.0, 46.5)), in_view(&s, Coord::new(9.5, 46.5)));
    let radius = ((e.x - c.x).powi(2) + (e.y - c.y).powi(2)).sqrt();
    for p in ring_of(&s, &r) {
        let v = in_view(&s, p);
        let d = ((v.x - c.x).powi(2) + (v.y - c.y).powi(2)).sqrt();
        // Within what LV95's round trip through longitude/latitude moves a place (a millimetre).
        assert!(close(d, radius, 1e-7), "{d} vs {radius}");
    }
    // An ellipse fills the view's box between its corners: its widest points touch the box.
    let r = s.run("graphics.add", &json!({"shape": "ellipse", "points": [[8.0, 46.0], [10.0, 47.0]], "frame": "view"})).unwrap();
    let (a, b) = (in_view(&s, Coord::new(8.0, 46.0)), in_view(&s, Coord::new(10.0, 47.0)));
    let ring: Vec<Coord> = ring_of(&s, &r).into_iter().map(|p| in_view(&s, p)).collect();
    let max_x = ring.iter().map(|p| p.x).fold(f64::MIN, f64::max);
    let min_y = ring.iter().map(|p| p.y).fold(f64::MAX, f64::min);
    assert!(close(max_x, a.x.max(b.x), 1e-7) && close(min_y, a.y.min(b.y), 1e-7), "{max_x} {min_y}");
    // The preview is the outline added.
    let preview = crate::cmd::graphics::dragged_outline(&s, "ellipse", &[Coord::new(8.0, 46.0), Coord::new(10.0, 47.0)]).unwrap();
    assert_eq!(preview, ring_of(&s, &r));
}

#[test]
fn a_frame_other_than_view_or_mercator_is_refused() {
    let mut s = on_lv95();
    let e = s.run("graphics.add", &json!({"shape": "rectangle", "points": [[8.0, 46.0], [10.0, 47.0]], "frame": "map"})).unwrap_err();
    assert!(e.contains("frame \"map\""), "{e}");
    let e = s.run("graphics.add", &json!({"shape": "rectangle", "points": [[8.0, 46.0], [10.0, 47.0]], "frame": true})).unwrap_err();
    assert!(e.contains("frame true"), "{e}");
    // A shape that leaves the map's projection is refused, not drawn folded.
    let e = s.run("graphics.add", &json!({"shape": "rectangle", "points": [[-170.0, -80.0], [170.0, 80.0]], "frame": "view"})).unwrap_err();
    assert!(e.contains("isn't on the map") || e.contains("past the map's projection"), "{e}");
}

/// On a map that shows a pole, a corner dragged past 85° is added where it was dragged, as the
/// preview shows it: only Web Mercator keeps points within 85° of the equator.
#[test]
fn a_dragged_corner_near_a_pole_stays_where_it_was_dragged() {
    let mut s = Session::new(Project::default());
    s.run("map.projection", &json!({"type": "EPSG:3413"})).unwrap();
    s.run("map.goToXY", &json!({"lon": -45.0, "lat": 88.0, "scale": 20_000_000})).unwrap();
    let corners = [Coord::new(-60.0, 87.0), Coord::new(-30.0, 88.5)];
    let r = s.run("graphics.add", &json!({"shape": "rectangle", "points": [[-60.0, 87.0], [-30.0, 88.5]], "frame": "view"})).unwrap();
    let ring = ring_of(&s, &r);
    for c in corners {
        assert!(ring.iter().any(|p| (p.x - c.x).abs() < 1e-6 && (p.y - c.y).abs() < 1e-6), "{c:?} in {ring:?}");
    }
    assert_eq!(crate::cmd::graphics::dragged_outline(&s, "rectangle", &corners).unwrap(), ring);
}

/// The point a graphic `r` added is at, in longitude/latitude.
fn point_of(s: &Session, layer: u64, row: u64) -> Coord {
    match s.map().layer(layer as u32).unwrap().dataset.features[row as usize].geometry.clone() {
        Some(Geometry::Point(c)) => c,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_point_keeps_within_85_degrees_on_a_web_mercator_map_and_not_on_a_polar_one() {
    // Web Mercator, where a pole is infinitely far: clicked at 88°, placed at 85° as the map
    // draws it, by `"frame": "view"` as by the default.
    let mut s = Session::new(Project::default());
    for frame in [json!("view"), Value::Null] {
        let r = s.run("graphics.add", &json!({"shape": "point", "points": [[10.0, 88.0]], "frame": frame})).unwrap();
        assert_eq!(point_of(&s, r["layer"].as_u64().unwrap(), r["row"].as_u64().unwrap()), Coord::new(10.0, 85.0), "{frame}");
    }
    // A polar map shows the pole: a point at 88° stays there, added and placed again.
    let mut s = Session::new(Project::default());
    s.run("map.projection", &json!({"type": "EPSG:3413"})).unwrap();
    let r = s.run("graphics.add", &json!({"shape": "point", "points": [[0.0, 88.0]], "frame": "view"})).unwrap();
    let (layer, row) = (r["layer"].as_u64().unwrap(), r["row"].as_u64().unwrap());
    assert_eq!(point_of(&s, layer, row), Coord::new(0.0, 88.0));
    s.run("graphics.edit", &json!({"layer": layer, "row": row, "points": [[5.0, 88.5]]})).unwrap();
    assert_eq!(point_of(&s, layer, row), Coord::new(5.0, 88.5));
    // Below 85° nothing is clamped on either map.
    s.run("graphics.edit", &json!({"layer": layer, "row": row, "points": [[5.0, 60.0]]})).unwrap();
    assert_eq!(point_of(&s, layer, row), Coord::new(5.0, 60.0));
}

#[test]
fn the_view_turns_as_the_screen_does() {
    // Turned 90°, east is up on screen: a rectangle dragged from west to east is tall on screen
    // and its east side is at the top.
    let mut s = on_lv95();
    s.run("map.setBearing", &json!({"bearing": 90.0})).unwrap();
    let (w, e) = (in_view(&s, Coord::new(8.0, 46.5)), in_view(&s, Coord::new(10.0, 46.5)));
    assert!(e.y - w.y > 100_000.0 && (e.x - w.x).abs() < 20_000.0, "{w:?} {e:?}");
    let r = s.run("graphics.add", &json!({"shape": "rectangle", "points": [[8.0, 46.4], [10.0, 46.6]], "frame": "view"})).unwrap();
    let ring: Vec<Coord> = ring_of(&s, &r).into_iter().map(|c| in_view(&s, c)).collect();
    for w in ring.windows(2) {
        let (dx, dy) = (w[1].x - w[0].x, w[1].y - w[0].y);
        assert!(dx.abs() < 1e-4 || dy.abs() < 1e-4, "side {dx} {dy}");
    }
}

#[test]
fn a_ring_point_past_the_maps_projection_is_refused() {
    // A plane point far past LV95's area reads back to no place the map shows there.
    let s = on_lv95();
    let plane = crate::cmd::graphics::Plane::view(&s);
    assert!(plane.back_for_tests(Coord::new(2_600_000.0, 1_200_000.0)).is_some());
    assert!(plane.back_for_tests(Coord::new(9e7, 9e7)).is_none());
}

#[test]
fn a_circle_near_85_degrees_on_a_web_mercator_map_is_drawn_and_one_round_the_pole_on_a_polar_map() {
    // The default map: a circle near 85° is added whole, within the 85° its points are kept in.
    let mut s = Session::new(Project::default());
    let r = s.run("graphics.add", &json!({"shape": "circle", "points": [[0.0, 84.0], [0.0, 84.9]], "frame": "view"})).unwrap();
    let ring = ring_of(&s, &r);
    assert_eq!(ring.len(), 73, "{ring:?}");
    assert!(ring.iter().all(|c| c.is_finite() && c.y > 82.0 && c.y <= 85.0), "{ring:?}");
    // A polar map: a circle round the pole, its ring through 90° itself.
    let mut s = Session::new(Project::default());
    s.run("map.projection", &json!({"type": "EPSG:3413"})).unwrap();
    let r = s.run("graphics.add", &json!({"shape": "circle", "points": [[-45.0, 89.0], [-45.0, 90.0]], "frame": "view"})).unwrap();
    assert!(ring_of(&s, &r).iter().all(|c| c.y >= 87.9), "{:?}", ring_of(&s, &r));
}

#[test]
fn a_drag_previews_and_adds_within_85_degrees_on_a_web_mercator_map_and_says_so() {
    // A corner dragged to 85.04° previews, and is added, at 85°, and the command says so; one
    // dragged below 85° moves nowhere and says nothing.
    let mut s = Session::new(Project::default());
    let corners = [Coord::new(0.0, 84.9), Coord::new(1.0, 85.04)];
    let outline = crate::cmd::graphics::dragged_outline(&s, "rectangle", &corners).unwrap();
    let mark = s.notice_mark();
    let r = s.run("graphics.add", &json!({"shape": "rectangle", "points": [[0.0, 84.9], [1.0, 85.04]], "frame": "view"})).unwrap();
    assert_eq!(outline, ring_of(&s, &r));
    assert!(outline.iter().all(|c| c.y <= 85.0) && outline.iter().any(|c| c.y == 85.0), "{outline:?}");
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(said.iter().any(|t| t.contains("placed at 85°")), "{said:?}");
    let mark = s.notice_mark();
    let below = [Coord::new(0.0, 80.0), Coord::new(1.0, 84.5)];
    let outline = crate::cmd::graphics::dragged_outline(&s, "rectangle", &below).unwrap();
    let r = s.run("graphics.add", &json!({"shape": "rectangle", "points": [[0.0, 80.0], [1.0, 84.5]], "frame": "view"})).unwrap();
    assert_eq!(outline, ring_of(&s, &r));
    assert!(outline.iter().any(|c| (c.y - 84.5).abs() < 1e-9), "{outline:?}");
    assert!(s.notices_since(mark).iter().all(|n| !n.text.contains("85°")));
}
