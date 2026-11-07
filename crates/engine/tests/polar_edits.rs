//! Moving, rotating or scaling a polygon poleward of 85.05°S keeps it there, rigidly (#1033).
use rustgis_engine::{
    Project, Session,
    data::{Dataset, Feature, Field, FieldType, Geometry, GeometryType, Value},
    geom::{Aeqd, Coord, haversine_m, spherical_centre, wgs84_distance_m},
};
use serde_json::json;

/// A session whose one layer holds `geometries`, all selected, ready to edit.
fn layer(kind: GeometryType, geometries: Vec<Geometry>) -> (Session, u32) {
    let mut data = Dataset::new("Polar", kind, vec![Field::new("n", FieldType::Integer)]);
    let n = geometries.len();
    data.features = geometries
        .into_iter()
        .zip(1..)
        .map(|(g, id)| Feature { id, geometry: Some(g), attrs: vec![Value::Int(0)] })
        .collect::<Vec<_>>()
        .into();
    let mut s = Session::new(Project::new("Polar edits"));
    let id = s.map_mut().add_dataset(data);
    s.active_layer = Some(id);
    s.map_mut().editing.settings.enabled = true;
    s.map_mut().layer_mut(id).unwrap().selection = (0..n).collect();
    (s, id)
}

/// A session whose one layer holds `ring` as a selected polygon, ready to edit.
fn session(ring: Vec<Coord>) -> (Session, u32) {
    layer(GeometryType::Polygon, vec![Geometry::Polygon(vec![vec![ring]])])
}

/// The polygon from 87°S to 89°S of the issue.
fn polar_ring() -> Vec<Coord> {
    let c = Coord::new;
    vec![c(0.0, -89.0), c(1.0, -89.0), c(1.0, -87.0), c(0.0, -87.0), c(0.0, -89.0)]
}

fn ring_of(s: &Session, id: u32, row: usize) -> Vec<Coord> {
    match &s.map().layer(id).unwrap().dataset.features[row].geometry {
        Some(Geometry::Polygon(p)) => p[0][0].clone(),
        g => panic!("{g:?}"),
    }
}

fn stored(s: &Session, id: u32) -> Vec<Coord> {
    ring_of(s, id, 0)
}

#[test]
fn small_edits_keep_a_polar_polygon_where_it_is() {
    let mut wrong = Vec::new();
    for (cmd, params) in
        [("edit.move", json!({"dx": 1000.0, "dy": 0.0})), ("edit.rotate", json!({"angle": 10.0})), ("edit.scale", json!({"factor": 1.01}))]
    {
        let (mut s, id) = session(polar_ring());
        s.run(cmd, &params).unwrap();
        let lats: Vec<f64> = stored(&s, id).iter().map(|c| c.y).collect();
        // The polygon spans 87°S to 89°S, about 222 km; none of these edits moves it 50 km north.
        if lats.iter().any(|y| *y > -86.5) {
            wrong.push(format!("{cmd}: latitudes now {lats:.4?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn polar_edits_are_rigid_on_the_ground() {
    let before = polar_ring();
    let center = spherical_centre(&before[..4]).unwrap();
    let plane = Aeqd::new(center);
    let sides = |r: &[Coord]| r.windows(2).map(|w| wgs84_distance_m(w[0], w[1])).collect::<Vec<_>>();
    let close = |a: &[f64], b: &[f64], k: f64| a.iter().zip(b).all(|(x, y)| ((x * k) / y - 1.0).abs() < 1e-3);

    // 10 km east: the center goes 10 km, east, and the sides keep their lengths (to 0.1 %).
    let (mut s, id) = session(before.clone());
    s.run("edit.move", &json!({"dx": 10.0, "dy": 0.0, "units": "Kilometers"})).unwrap();
    let moved = stored(&s, id);
    let to = plane.forward(spherical_centre(&moved[..4]).unwrap());
    assert!((to.x - 10_000.0).abs() < 10.0 && to.y.abs() < 10.0 && close(&sides(&before), &sides(&moved), 1.0), "{to:?} {moved:?}");

    // Rotated 30° about its center, the box turns rigidly: its sides and diagonals keep their
    // lengths, and each diagonal turns 30° clockwise; scaled, they all grow by half.
    let lengths = |r: &[Coord]| [(0, 1), (1, 2), (2, 3), (3, 0), (0, 2), (1, 3)].map(|(i, j)| wgs84_distance_m(r[i], r[j]));
    let heading = |r: &[Coord], i: usize, j: usize| {
        let (a, b) = (plane.forward(r[i]), plane.forward(r[j]));
        (b.x - a.x).atan2(b.y - a.y).to_degrees()
    };
    let (mut s, id) = session(before.clone());
    s.run("edit.rotate", &json!({"angle": 30.0})).unwrap();
    let after = stored(&s, id);
    let turned = [(0, 2), (1, 3)]
        .iter()
        .all(|&(i, j)| ((heading(&after, i, j) - heading(&before, i, j) + 540.0).rem_euclid(360.0) - 180.0 - 30.0).abs() < 0.1);
    assert!(turned && close(&lengths(&before), &lengths(&after), 1.0), "{after:?}");
    let (mut s, id) = session(before.clone());
    s.run("edit.scale", &json!({"factor": 1.5})).unwrap();
    let after = stored(&s, id);
    assert!(close(&lengths(&before), &lengths(&after), 1.5), "{after:?}");
}

#[test]
fn polar_neighbors_moved_together_still_share_their_corners() {
    // Side by side from 87°S to 89°S, sharing the edge along 10°E.
    let c = Coord::new;
    let ring = |x0: f64, x1: f64| Geometry::Polygon(vec![vec![vec![c(x0, -89.0), c(x1, -89.0), c(x1, -87.0), c(x0, -87.0), c(x0, -89.0)]]]);
    let (mut s, id) = layer(GeometryType::Polygon, vec![ring(0.0, 10.0), ring(10.0, 20.0)]);
    s.run("edit.move", &json!({"dx": 1.0, "dy": 0.5, "units": "Kilometers"})).unwrap();
    let (a, b) = (ring_of(&s, id, 0), ring_of(&s, id, 1));
    // The first's east side is the second's west side, vertex for vertex.
    assert!(a[1] == b[0] && a[2] == b[3], "{a:?} {b:?}");
    // And after a move clear over the pole, to the other side of it.
    s.run("edit.move", &json!({"dx": 0.0, "dy": -500.0, "units": "Kilometers"})).unwrap();
    let (a, b) = (ring_of(&s, id, 0), ring_of(&s, id, 1));
    assert!(a[1] == b[0] && a[2] == b[3], "{a:?} {b:?}");
}

#[test]
fn a_polygon_moved_over_a_pole_keeps_its_hole_inside() {
    let c = Coord::new;
    // Around 0°E from 87°S to 89°S, the shell starting west of 0°E and the hole east of it: over
    // the pole they land around 180°, where a ring left to itself could take either side.
    let shell = vec![c(-5.0, -89.0), c(5.0, -89.0), c(5.0, -87.0), c(-5.0, -87.0), c(-5.0, -89.0)];
    let hole = vec![c(1.0, -88.5), c(1.0, -87.5), c(-1.0, -87.5), c(-1.0, -88.5), c(1.0, -88.5)];
    let (mut s, id) = layer(GeometryType::Polygon, vec![Geometry::Polygon(vec![vec![shell, hole]])]);
    s.run("edit.move", &json!({"dx": 0.0, "dy": -500.0, "units": "Kilometers"})).unwrap();
    let Some(Geometry::Polygon(p)) = &s.map().layer(id).unwrap().dataset.features[0].geometry else { panic!() };
    let span = |r: &[Coord]| r.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), c| (lo.min(c.x), hi.max(c.x)));
    let ((slo, shi), (hlo, hhi)) = (span(&p[0][0]), span(&p[0][1]));
    assert!(slo < hlo && hhi < shi && shi - slo < 20.0, "{p:?}");
}

#[test]
fn a_point_moved_from_84_degrees_over_the_pole_lands_beyond_it() {
    // 1,000 km north of 84°N, on the ground: over the pole (670 km) and 330 km down 180°.
    let start = Coord::new(0.0, 84.0);
    let (mut s, id) = layer(GeometryType::Point, vec![Geometry::Point(start)]);
    s.run("edit.move", &json!({"dx": 0.0, "dy": 1000.0, "units": "Kilometers"})).unwrap();
    let Some(Geometry::Point(p)) = &s.map().layer(id).unwrap().dataset.features[0].geometry else { panic!() };
    assert!(
        (wgs84_distance_m(start, *p) - 1_000_000.0).abs() < 1.0 && (p.x.abs() - 180.0).abs() < 1e-6 && (p.y - 87.0).abs() < 0.05,
        "{p:?}"
    );
}

#[test]
fn a_polygon_across_85_degrees_moves_without_being_clamped() {
    let c = Coord::new;
    let (mut s, id) = session(vec![c(0.0, -86.0), c(1.0, -86.0), c(1.0, -84.0), c(0.0, -84.0), c(0.0, -86.0)]);
    s.run("edit.move", &json!({"dx": 10.0, "units": "Kilometers"})).unwrap();
    let lats: Vec<f64> = stored(&s, id).iter().map(|c| c.y).collect();
    assert!(lats.iter().any(|y| *y < -85.9) && lats.iter().all(|y| (-86.1..-83.9).contains(y)), "{lats:?}");
}

#[test]
fn an_edit_that_would_carry_a_polygon_over_a_pole_is_refused() {
    // 300 km south takes the polygon across the South Pole: refused, and left as it was.
    let (mut s, id) = session(polar_ring());
    let e = s.run("edit.move", &json!({"dx": 0.0, "dy": -300.0, "units": "Kilometers"})).unwrap_err();
    assert!(e.contains("onto or around a pole"), "{e}");
    assert_eq!(stored(&s, id), polar_ring());
}

#[test]
fn a_point_at_a_pole_can_be_moved_off_it() {
    let (mut s, id) = layer(GeometryType::Point, vec![Geometry::Point(Coord::new(0.0, -90.0))]);
    s.run("edit.move", &json!({"dx": 0.0, "dy": 10.0, "units": "Kilometers"})).unwrap();
    let Some(Geometry::Point(p)) = &s.map().layer(id).unwrap().dataset.features[0].geometry else { panic!() };
    assert!((wgs84_distance_m(Coord::new(0.0, -90.0), *p) - 10_000.0).abs() < 1.0, "{p:?}");
}

#[test]
fn a_line_beyond_85_degrees_splits_where_it_is_asked() {
    let c = Coord::new;
    let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(vec![vec![c(0.0, -86.0), c(0.0, -89.0)]])]);
    s.run("edit.split", &json!({"lon": 0.0, "lat": -87.5})).unwrap();
    let mut lats = Vec::new();
    for f in s.map().layer(id).unwrap().dataset.features.iter() {
        if let Some(g) = &f.geometry {
            g.for_each_coord(|c| lats.push(c.y));
        }
    }
    // Split at 87.5°S, not where Web Mercator stops (85.05°S).
    assert!(lats.iter().any(|y| (y + 87.5).abs() < 1e-9) && lats.iter().all(|y| *y < -85.9), "{lats:?}");
}

#[test]
fn split_reports_segments_it_cannot_measure() {
    let c = Coord::new;
    for lat in [45.0, -88.0] {
        for bad in [false, true] {
            let mut parts = vec![vec![c(0.0, lat - 0.1), c(0.0, lat + 0.1)]];
            if bad {
                parts.push(vec![c(f64::NAN, lat), c(1.0, lat), c(f64::INFINITY, lat)]);
            }
            let (mut s, _) = layer(GeometryType::Polyline, vec![Geometry::Line(parts)]);
            let mark = s.notice_mark();
            s.run("edit.split", &json!({"lon": 0.0, "lat": lat})).unwrap();
            let notices: Vec<&str> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
            if bad {
                assert_eq!(notices.len(), 1, "{notices:?}");
                assert!(notices[0].contains("2 segment(s)"), "{notices:?}");
            } else {
                assert!(notices.is_empty(), "{notices:?}");
            }
        }
    }
}

#[test]
fn a_polygon_with_an_edge_across_half_the_world_is_refused() {
    // Along 87°N and 88°N from 0° to 180°: which way around such an edge goes would be unclear once
    // moved, so the edit is refused, and the polygon stays as it was.
    let c = Coord::new;
    let ring = vec![c(0.0, 87.0), c(180.0, 87.0), c(180.0, 88.0), c(0.0, 88.0), c(0.0, 87.0)];
    let (mut s, id) = session(ring.clone());
    let e = s.run("edit.move", &json!({"dx": 1000.0})).unwrap_err();
    assert!(e.contains("180°"), "{e}");
    assert_eq!(bits(&stored(&s, id)), bits(&ring));
}

#[test]
fn a_split_beyond_85_degrees_takes_the_part_nearest_on_the_ground() {
    // At 89°N, one part runs north–south 1 km east of the point asked and another east–west 2 km
    // north of it. The plane, stretched east–west far more, would take the second; on the ground
    // the first is nearer, and it's the one split (into the new feature, on its own).
    let c = Coord::new;
    let east = 1_000.0 / (111_320.0 * 89f64.to_radians().cos());
    let parts = vec![vec![c(-0.5, 89.018), c(0.5, 89.018)], vec![c(east, 88.99), c(east, 89.01)]];
    let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(parts)]);
    s.run("edit.split", &json!({"lon": 0.0, "lat": 89.0})).unwrap();
    let added = match &s.map().layer(id).unwrap().dataset.features[1].geometry {
        Some(Geometry::Line(p)) => p.clone(),
        g => panic!("{g:?}"),
    };
    assert!(added.len() == 1 && added[0].iter().all(|p| (p.x - east).abs() < 1e-9), "{added:?}");
}

#[test]
fn a_move_of_no_distance_leaves_polar_features_as_they_are() {
    // Nothing to carry on the ground: a polar box and a cap reaching the pole stay as they are, to
    // the bit (the cap isn't refused), while a box within ±85.05° moves in Web Mercator as before.
    let c = Coord::new;
    let cap = vec![c(-180.0, -89.0), c(180.0, -89.0), c(180.0, -90.0), c(-180.0, -90.0), c(-180.0, -89.0)];
    let geoms = [polar_ring(), cap.clone(), boxed(0.0, 10.0, 1.0, 11.0)].map(|r| Geometry::Polygon(vec![vec![r]])).to_vec();
    let (mut s, id) = layer(GeometryType::Polygon, geoms);
    s.run("edit.move", &json!({"dx": 0.0, "dy": 0.0})).unwrap();
    assert_eq!((bits(&ring_of(&s, id, 0)), bits(&ring_of(&s, id, 1))), (bits(&polar_ring()), bits(&cap)));
    // With only polar features selected, nothing changes, and the move says so.
    let (mut s, id) = session(polar_ring());
    let e = s.run("edit.move", &json!({"dy": 0.0})).unwrap_err();
    assert!(e.contains("no distance"), "{e}");
    assert_eq!(bits(&stored(&s, id)), bits(&polar_ring()));
}

#[test]
fn a_split_passes_over_a_segment_missing_a_coordinate() {
    // Within ±85.05° too: the first segment, missing a longitude, is passed over, and the
    // second is cut at 15°E (on `main` the missing one took the cut).
    let c = Coord::new;
    let line = vec![c(f64::NAN, 10.0), c(10.0, 10.0), c(20.0, 10.0)];
    let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(vec![line])]);
    s.run("edit.split", &json!({"lon": 15.0, "lat": 10.1})).unwrap();
    let added = match &s.map().layer(id).unwrap().dataset.features[1].geometry {
        Some(Geometry::Line(p)) => p.clone(),
        g => panic!("{g:?}"),
    };
    assert!(added.len() == 1 && (added[0][0].x - 15.0).abs() < 1e-6, "{added:?}");
}

#[test]
fn a_split_passes_over_a_segment_to_an_infinite_latitude() {
    // Along 88°N, then on to a vertex whose latitude is infinite (no place, not the pole): a
    // click at 89.5°N above the corner cuts the line at its corner on 88°N, not the segment
    // "to the pole" at 89.5°N.
    let c = Coord::new;
    let line = vec![c(0.0, 88.0), c(10.0, 88.0), c(10.0, f64::INFINITY)];
    let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(vec![line])]);
    s.run("edit.split", &json!({"lon": 10.0, "lat": 89.5})).unwrap();
    let added = match &s.map().layer(id).unwrap().dataset.features[1].geometry {
        Some(Geometry::Line(p)) => p.clone(),
        g => panic!("{g:?}"),
    };
    assert!(added.len() == 1 && (added[0][0].y - 88.0).abs() < 1e-6, "{added:?}");
}

#[test]
fn points_around_a_pole_moved_keep_their_longitudes_as_written() {
    // Moved a kilometer, each point at 87°N stays written near where it was, not a turn away
    // (90° as −270°, say), wherever their center is.
    let pts = [-170.0, -90.0, 0.0, 90.0, 170.0].map(|x| Coord::new(x, 87.0)).to_vec();
    let (mut s, id) = layer(GeometryType::Point, vec![Geometry::MultiPoint(pts.clone())]);
    s.run("edit.move", &json!({"dx": 1000.0})).unwrap();
    let moved = match &s.map().layer(id).unwrap().dataset.features[0].geometry {
        Some(Geometry::MultiPoint(v)) => v.clone(),
        g => panic!("{g:?}"),
    };
    assert!(moved.iter().zip(&pts).all(|(p, q)| (p.x - q.x).abs() < 1.0), "{moved:?}");
}

#[test]
fn a_split_beyond_85_degrees_cuts_at_the_point_nearest_on_the_ground() {
    // From 86°N on 0° to 89°N on 60°E, straight in longitude and latitude. Its nearest point to
    // 88.5°N on 0° is near 47°E on the ground, while the plane's would be near 13°E.
    let c = Coord::new;
    let (a, b, at) = (c(0.0, 86.0), c(60.0, 89.0), c(0.0, 88.5));
    let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(vec![vec![a, b]])]);
    s.run("edit.split", &json!({"lon": at.x, "lat": at.y})).unwrap();
    let cut = match &s.map().layer(id).unwrap().dataset.features[1].geometry {
        Some(Geometry::Line(p)) => p[0][0],
        g => panic!("{g:?}"),
    };
    let nearest = (0..=10_000)
        .map(|i| haversine_m(at, c(a.x + (b.x - a.x) * f64::from(i) / 1e4, a.y + (b.y - a.y) * f64::from(i) / 1e4)))
        .fold(f64::INFINITY, f64::min);
    assert!(cut.x > 40.0 && haversine_m(at, cut) < nearest + 0.01, "{cut:?}: {} m, nearest {nearest} m", haversine_m(at, cut));
}

#[test]
fn a_split_beyond_85_degrees_looks_past_parts_it_cant_cut() {
    // A part of one point right where it's asked has no segment to cut, and a part missing a
    // coordinate none to measure there: the line's next part, along 87°N, is cut at 15°E.
    let c = Coord::new;
    for first in [vec![c(15.0, 88.0)], vec![c(f64::NAN, 88.0), c(15.0, 88.0)]] {
        let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(vec![first.clone(), vec![c(10.0, 87.0), c(20.0, 87.0)]])]);
        s.run("edit.split", &json!({"lon": 15.0, "lat": 88.0})).unwrap();
        let added = match &s.map().layer(id).unwrap().dataset.features[1].geometry {
            Some(Geometry::Line(p)) => p.clone(),
            g => panic!("{g:?}"),
        };
        assert!(added.len() == 1 && (added[0][0].x - 15.0).abs() < 1e-6 && (added[0][0].y - 87.0).abs() < 1e-3, "{first:?}: {added:?}");
    }
}

#[test]
fn a_split_at_a_vertex_leaves_it_once_in_each_part() {
    // Within ±85.05° and beyond it: the parts meet at the vertex, neither repeating it.
    let c = Coord::new;
    for y in [10.0, 88.0] {
        let line = vec![c(0.0, y), c(10.0, y), c(10.0, y + 1.0)];
        let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(vec![line.clone()])]);
        s.run("edit.split", &json!({"lon": 10.0, "lat": y})).unwrap();
        let parts: Vec<Vec<Coord>> = s
            .map()
            .layer(id)
            .unwrap()
            .dataset
            .features
            .iter()
            .map(|f| match &f.geometry {
                Some(Geometry::Line(p)) => p[0].clone(),
                g => panic!("{g:?}"),
            })
            .collect();
        assert_eq!(parts, [line[..2].to_vec(), line[1..].to_vec()], "{y}");
    }
}

#[test]
fn a_split_too_long_to_search_in_full_says_so() {
    // 70,000 edges along 88°N, each nearly a whole turn: more distances than one split works out,
    // so the last segments count as near as their nearer ends, and the split says so (it leaves
    // fewer distances than one of its edges takes, 16); a line selected after it, one edge going
    // 380° around, which takes 17, finds only its end, and is left whole, and the split says that
    // too. Alone, that line is searched in full and says nothing. A place that's no place is
    // refused.
    let c = Coord::new;
    let zigzag: Vec<Coord> = (0..=70_000).map(|i| c(if i % 2 == 0 { -179.5 } else { 179.5 }, 88.0)).collect();
    let around = vec![c(-190.0, 88.0), c(190.0, 88.0)];
    let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(vec![zigzag]), Geometry::Line(vec![around.clone()])]);
    let mark = s.notice_mark();
    s.run("edit.split", &json!({"lon": 0.0, "lat": 88.5})).unwrap();
    let said: Vec<&str> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
    assert!(
        said.iter().any(|t| t.starts_with("1 line(s) near a pole couldn't be searched in full") && t.contains("was split")),
        "{said:?}"
    );
    assert!(
        said.iter().any(|t| t.starts_with("1 line(s) near a pole couldn't be searched in full") && t.contains("left whole")),
        "{said:?}"
    );
    assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 3);
    let (mut s, _) = layer(GeometryType::Polyline, vec![Geometry::Line(vec![around])]);
    let mark = s.notice_mark();
    s.run("edit.split", &json!({"lon": 0.0, "lat": 88.5})).unwrap();
    assert!(s.notices_since(mark).iter().all(|n| !n.text.contains("searched in full")), "{:?}", s.notices_since(mark));
    for lat in [95.0, -90.5] {
        assert_eq!(s.run("edit.split", &json!({"lon": 0.0, "lat": lat})).unwrap_err(), "lon and lat are degrees", "{lat}");
    }
}

#[test]
fn a_split_along_an_edge_going_around_a_pole_finds_its_nearest_turn() {
    // One edge from 87°N to 89°N, straight in longitude and latitude, going twice around the pole:
    // asked at 87.56°N on 144°W, it's cut where it comes nearest, as a fine sampling finds.
    let c = Coord::new;
    let (a, b, at) = (c(0.0, 87.0), c(700.0, 89.0), c(-144.0, 87.56));
    let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(vec![vec![a, b]])]);
    s.run("edit.split", &json!({"lon": at.x, "lat": at.y})).unwrap();
    let cut = match &s.map().layer(id).unwrap().dataset.features[1].geometry {
        Some(Geometry::Line(p)) => p[0][0],
        g => panic!("{g:?}"),
    };
    let nearest = (0..=200_000)
        .map(|i| haversine_m(at, c(a.x + (b.x - a.x) * f64::from(i) / 2e5, a.y + (b.y - a.y) * f64::from(i) / 2e5)))
        .fold(f64::INFINITY, f64::min);
    assert!(haversine_m(at, cut) < nearest + 1.0, "{cut:?}: {} m, nearest {nearest} m", haversine_m(at, cut));
}

#[test]
fn a_split_near_a_line_reaching_beyond_85_degrees_compares_on_the_ground() {
    // At 84°N, so within 85.05°, but the line reaches beyond. One part runs north–south 25° east,
    // 282 km away on the ground; another along 86.3°N, 256 km north. The plane, stretched
    // east–west far more, would take the first; the second is split.
    let c = Coord::new;
    let parts = vec![vec![c(25.0, 83.5), c(25.0, 84.5)], vec![c(-10.0, 86.3), c(10.0, 86.3)]];
    let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(parts)]);
    s.run("edit.split", &json!({"lon": 0.0, "lat": 84.0})).unwrap();
    let added = match &s.map().layer(id).unwrap().dataset.features[1].geometry {
        Some(Geometry::Line(p)) => p.clone(),
        g => panic!("{g:?}"),
    };
    assert!(added.len() == 1 && added[0][0].x.abs() < 1e-6 && added[0].iter().all(|p| (p.y - 86.3).abs() < 1e-9), "{added:?}");
}

#[test]
fn a_feature_reaching_a_pole_is_refused_and_left_alone() {
    let c = Coord::new;
    // A cap around the South Pole, as files cut it: along 89°S, down to the pole, and back.
    let cap = vec![c(-180.0, -89.0), c(180.0, -89.0), c(180.0, -90.0), c(-180.0, -90.0), c(-180.0, -89.0)];
    for (cmd, params) in
        [("edit.move", json!({"dx": 1000.0})), ("edit.rotate", json!({"angle": 10.0})), ("edit.scale", json!({"factor": 1.1}))]
    {
        let (mut s, id) = session(cap.clone());
        let e = s.run(cmd, &params).unwrap_err();
        assert!(e.contains("reaches a pole"), "{cmd}: {e}");
        assert_eq!(stored(&s, id), cap, "{cmd}");
    }
    // Refused with another feature selected, the edit leaves that one as it was too.
    for (cmd, params) in
        [("edit.move", json!({"dx": 1000.0})), ("edit.rotate", json!({"angle": 10.0})), ("edit.scale", json!({"factor": 1.1}))]
    {
        let (mut s, id) =
            layer(GeometryType::Polygon, vec![Geometry::Polygon(vec![vec![polar_ring()]]), Geometry::Polygon(vec![vec![cap.clone()]])]);
        let e = s.run(cmd, &params).unwrap_err();
        assert!(e.contains("reaches a pole"), "{cmd}: {e}");
        assert_eq!(bits(&ring_of(&s, id, 0)), bits(&polar_ring()), "{cmd}");
    }
}

#[test]
fn small_edits_keep_a_polygon_across_the_antimeridian_narrow() {
    // A polygon 0.6° wide whose longitudes run on from 179.6° to 180.2°. At 10°N, well within Web
    // Mercator's square, the edits work as before and it stays under 1° wide. At 86°S they go on
    // the ground, and it stays narrow there too (a 10° turn makes its 44 km north–south side reach
    // about 1.5° east–west), not stretched around the world.
    let c = Coord::new;
    let mut wrong = Vec::new();
    for (lat, limit) in [(10.0, 1.0), (-86.0, 5.0)] {
        let ring = vec![c(179.6, lat), c(180.2, lat), c(180.2, lat + 0.4), c(179.6, lat + 0.4), c(179.6, lat)];
        for (cmd, params) in [
            ("edit.move", json!({"dx": 1000.0, "dy": 0.0})),
            ("edit.rotate", json!({"angle": 10.0})),
            ("edit.scale", json!({"factor": 1.01})),
        ] {
            let (mut s, id) = session(ring.clone());
            s.run(cmd, &params).unwrap();
            let xs: Vec<f64> = stored(&s, id).iter().map(|c| c.x).collect();
            let width = xs.iter().copied().fold(f64::MIN, f64::max) - xs.iter().copied().fold(f64::MAX, f64::min);
            if width.is_nan() || width >= limit {
                wrong.push(format!("{cmd} at {lat}°: {width}° wide"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Each vertex's bits, to compare results exactly.
fn bits(r: &[Coord]) -> Vec<(u64, u64)> {
    r.iter().map(|c| (c.x.to_bits(), c.y.to_bits())).collect()
}

/// A box polygon's ring from (x0, y0) to (x1, y1).
fn boxed(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Coord> {
    let c = Coord::new;
    vec![c(x0, y0), c(x1, y0), c(x1, y1), c(x0, y1), c(x0, y0)]
}

#[test]
fn edits_within_web_mercator_are_as_before() {
    // A box from 10°N to 20°N, and its results, stay within Web Mercator's square: moved, turned
    // and scaled there exactly as before, to the bit.
    use rustgis_engine::geom::{lonlat_to_merc, merc_to_lonlat};
    let ring = boxed(10.0, 10.0, 20.0, 20.0);
    let (mut s, id) = session(ring.clone());
    s.run("edit.move", &json!({"dx": 1000.0, "dy": 2000.0})).unwrap();
    let moved: Vec<Coord> = ring
        .iter()
        .map(|p| {
            let (k, m) = (1.0 / p.y.to_radians().cos().max(0.01), lonlat_to_merc(*p));
            merc_to_lonlat(Coord::new(m.x + 1000.0 * k, m.y + 2000.0 * k))
        })
        .collect();
    assert_eq!(bits(&stored(&s, id)), bits(&moved));
    let center = lonlat_to_merc(Coord::new(15.0, 15.0));
    let (sin, cos) = (-10f64.to_radians()).sin_cos();
    let (mut s, id) = session(ring.clone());
    s.run("edit.rotate", &json!({"angle": 10.0})).unwrap();
    let turned: Vec<Coord> = ring
        .iter()
        .map(|p| {
            let m = lonlat_to_merc(*p);
            let (x, y) = (m.x - center.x, m.y - center.y);
            merc_to_lonlat(Coord::new(center.x + x * cos - y * sin, center.y + x * sin + y * cos))
        })
        .collect();
    assert_eq!(bits(&stored(&s, id)), bits(&turned));
    let (mut s, id) = session(ring.clone());
    s.run("edit.scale", &json!({"factor": 1.5})).unwrap();
    let scaled: Vec<Coord> = ring
        .iter()
        .map(|p| {
            let m = lonlat_to_merc(*p);
            merc_to_lonlat(Coord::new(center.x + (m.x - center.x) * 1.5, center.y + (m.y - center.y) * 1.5))
        })
        .collect();
    assert_eq!(bits(&stored(&s, id)), bits(&scaled));
}

#[test]
fn a_feature_far_from_a_pole_moves_as_before_alongside_one_near_it() {
    // An equator box selected with a box from 86°S to 88°S: moved 1 km east, it lands exactly where
    // it does moved alone, while the polar one moves 1 km on the ground.
    let (equator, polar) = (boxed(100.0, 0.0, 101.0, 1.0), boxed(0.0, -88.0, 20.0, -86.0));
    let poly = |r: &[Coord]| Geometry::Polygon(vec![vec![r.to_vec()]]);
    let (mut both, id) = layer(GeometryType::Polygon, vec![poly(&equator), poly(&polar)]);
    both.run("edit.move", &json!({"dx": 1000.0})).unwrap();
    let (mut alone, alone_id) = session(equator);
    alone.run("edit.move", &json!({"dx": 1000.0})).unwrap();
    assert_eq!(bits(&ring_of(&both, id, 0)), bits(&stored(&alone, alone_id)));
    let moved = ring_of(&both, id, 1);
    for (before, after) in polar.iter().zip(&moved) {
        assert!((wgs84_distance_m(*before, *after) - 1000.0).abs() < 10.0, "{before:?} → {after:?}");
    }
}

#[test]
fn features_near_both_poles_move_together() {
    // A box near the South Pole and one near the North Pole on the far meridian, selected together:
    // each moves 1 km on the ground about its own center.
    let (south, north) = (boxed(0.0, -89.0, 1.0, -87.0), boxed(180.0, 87.0, 181.0, 89.0));
    let poly = |r: &[Coord]| Geometry::Polygon(vec![vec![r.to_vec()]]);
    let (mut s, id) = layer(GeometryType::Polygon, vec![poly(&south), poly(&north)]);
    s.run("edit.move", &json!({"dx": 1000.0})).unwrap();
    for (row, ring) in [(0, &south), (1, &north)] {
        for (before, after) in ring.iter().zip(ring_of(&s, id, row)) {
            assert!((wgs84_distance_m(*before, after) - 1000.0).abs() < 10.0, "{row}: {before:?} → {after:?}");
        }
    }
}

#[test]
fn a_polygon_across_85_degrees_turns_and_scales_on_the_ground() {
    // A box from 84°S to 86°S, across Web Mercator's edge: turned 10°, its sides keep their length;
    // scaled 1.5, they grow by half (each to 0.5%).
    let ring = boxed(0.0, -86.0, 1.0, -84.0);
    let sides = |r: &[Coord]| r.windows(2).map(|w| wgs84_distance_m(w[0], w[1])).collect::<Vec<_>>();
    for (cmd, params, k) in [("edit.rotate", json!({"angle": 10.0}), 1.0), ("edit.scale", json!({"factor": 1.5}), 1.5)] {
        let (mut s, id) = session(ring.clone());
        s.run(cmd, &params).unwrap();
        for (before, after) in sides(&ring).iter().zip(sides(&stored(&s, id))) {
            assert!((after / (before * k) - 1.0).abs() < 0.005, "{cmd}: {before} → {after}");
        }
    }
}

#[test]
fn a_polar_edit_undoes() {
    let (mut s, id) = session(polar_ring());
    s.run("edit.move", &json!({"dx": 5000.0, "dy": 3000.0})).unwrap();
    assert_ne!(stored(&s, id), polar_ring());
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(bits(&stored(&s, id)), bits(&polar_ring()));
}

#[test]
fn a_polar_line_moves_rigidly() {
    // A line at 88°S, moved 10 km east, keeps its segments' lengths (to 0.1%).
    let c = Coord::new;
    let line = vec![c(0.0, -88.0), c(10.0, -88.0), c(10.0, -87.0)];
    let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(vec![line.clone()])]);
    s.run("edit.move", &json!({"dx": 10_000.0})).unwrap();
    let moved = match &s.map().layer(id).unwrap().dataset.features[0].geometry {
        Some(Geometry::Line(parts)) => parts[0].clone(),
        g => panic!("{g:?}"),
    };
    for (a, b) in line.windows(2).zip(moved.windows(2)) {
        let (before, after) = (wgs84_distance_m(a[0], a[1]), wgs84_distance_m(b[0], b[1]));
        assert!((after / before - 1.0).abs() < 1e-3, "{before} → {after}");
    }
}

#[test]
fn a_missing_coordinate_stays_the_only_one() {
    // A polar ring with one vertex's longitude missing: moved, that vertex stays as it was (its
    // latitude kept, its longitude missing) and every other comes through.
    let mut ring = polar_ring();
    ring[2].x = f64::NAN;
    let (mut s, id) = session(ring);
    s.run("edit.move", &json!({"dx": 1000.0})).unwrap();
    let after = stored(&s, id);
    let bad: Vec<usize> = after.iter().enumerate().filter(|(_, c)| !c.is_finite()).map(|(i, _)| i).collect();
    assert!(bad == [2] && after[2].y == -87.0, "{after:?}");
}

#[test]
fn a_vertex_missing_a_coordinate_left_where_it_was_is_counted() {
    // A move, turn or scale says how many features it changed have a vertex missing a coordinate,
    // which stays where it was, to the bit: on the ground and within ±85.05°; with every vertex
    // there, it says nothing.
    let said = |s: &mut Session, id: &str, p: &serde_json::Value| {
        let mark = s.notice_mark();
        let ran = s.run(id, p);
        let said =
            s.notices_since(mark).iter().filter(|n| n.text.contains("missing a coordinate")).map(|n| n.text.clone()).collect::<Vec<_>>();
        (ran.is_ok(), said)
    };
    let one = ["1 feature(s) have a vertex missing a coordinate, which was left where it was"];
    let edits = [("edit.move", json!({"dx": 1000.0})), ("edit.rotate", json!({"angle": 10.0})), ("edit.scale", json!({"factor": 1.5}))];
    let near_equator = boxed(10.0, 10.0, 11.0, 11.0);
    for ring in [polar_ring(), near_equator] {
        for (id, p) in &edits {
            // Three vertices of one feature, a latitude infinite, a longitude missing and a
            // longitude infinite: one feature.
            let mut lossy = ring.clone();
            lossy[1].y = f64::INFINITY;
            lossy[2].x = f64::NAN;
            lossy[3].x = f64::INFINITY;
            let (mut s, layer_id) = session(lossy.clone());
            assert_eq!(said(&mut s, id, p), (true, one.map(String::from).to_vec()), "{id} {ring:?}");
            let after = stored(&s, layer_id);
            let same = |a: Coord, b: Coord| a.x.to_bits() == b.x.to_bits() && a.y.to_bits() == b.y.to_bits();
            assert!((1..=3).all(|i| same(after[i], lossy[i])), "{id} {after:?}");
            assert!(after.iter().enumerate().all(|(i, c)| (1..=3).contains(&i) || c.is_finite()), "{id} {after:?}");
            let (mut s, _) = session(ring.clone());
            assert_eq!(said(&mut s, id, p), (true, vec![]), "{id} {ring:?}");
        }
    }
    // Several features: each changed one with a vertex missing a coordinate counts once; a point,
    // which a turn leaves as it is, and a feature with every vertex don't.
    let mut two = boxed(10.0, 10.0, 11.0, 11.0);
    two[1].x = f64::NAN;
    two[2].x = f64::NAN;
    let mut once = boxed(20.0, 10.0, 21.0, 11.0);
    once[3].y = f64::NAN;
    let polygon = |ring: Vec<Coord>| Geometry::Polygon(vec![vec![ring]]);
    let features = vec![polygon(two), polygon(once), polygon(boxed(30.0, 10.0, 31.0, 11.0)), Geometry::Point(Coord::new(f64::NAN, 10.0))];
    let (mut s, _) = layer(GeometryType::Polygon, features);
    let (ran, text) = said(&mut s, "edit.rotate", &json!({"angle": 10.0}));
    assert!(ran && text == ["2 feature(s) have a vertex missing a coordinate, which was left where it was"], "{text:?}");
    // A refused edit (a polygon reaching a pole can't be turned yet) says nothing of the kind.
    let mut cap = polar_ring();
    cap[0].y = -90.0;
    cap[4].y = -90.0;
    cap[2].x = f64::NAN;
    let (mut s, _) = session(cap);
    assert_eq!(said(&mut s, "edit.rotate", &json!({"angle": 10.0})), (false, vec![]));
}

#[test]
fn a_polar_move_turns_the_feature_with_north() {
    // 300 km east of 88°S, the box's center has gone about 53° around the South Pole, so north
    // where it lands is turned that much from north where it was. Moved rigidly, the box keeps
    // pointing the same way on the ground (as a shift in a plane about the pole shows): its side
    // that ran north, seen from its center, now runs as many degrees west of north.
    let before = polar_ring();
    let (mut s, id) = session(before.clone());
    s.run("edit.move", &json!({"dx": 300.0, "units": "Kilometers"})).unwrap();
    let after = stored(&s, id);
    // The side from the 89°S corner to the 87°S corner, as a heading at the box's center, and the
    // center's longitude.
    let heading = |r: &[Coord]| {
        let center = spherical_centre(&r[..4]).unwrap();
        let plane = Aeqd::new(center);
        let (a, b) = (plane.forward(r[0]), plane.forward(r[3]));
        ((b.x - a.x).atan2(b.y - a.y).to_degrees(), center.x)
    };
    let ((was, from), (now, to)) = (heading(&before), heading(&after));
    let round = to - from;
    assert!(round > 45.0 && (now - was + round).abs() < 1.0, "{round}° around the pole; heading {was}° → {now}°");
}

#[test]
fn a_polar_feature_turns_about_the_same_center_however_many_vertices_its_edges_have() {
    // A box from 86°S to 88°S turned 10°, alone and with 53 more vertices along its 86°S edge
    // (0.37° apart, off the half-degree stretches its center is weighed in): its corner at
    // (0°, 88°S) lands in the same place (within 50 m).
    let plain = boxed(0.0, -88.0, 20.0, -86.0);
    let mut dense = plain[..3].to_vec();
    dense.extend((1..54).map(|i| Coord::new(20.0 - 0.37 * f64::from(i), -86.0)));
    dense.extend_from_slice(&plain[3..]);
    let turned = |ring: Vec<Coord>| {
        let (mut s, id) = session(ring);
        s.run("edit.rotate", &json!({"angle": 10.0})).unwrap();
        stored(&s, id)[0]
    };
    let (a, b) = (turned(plain), turned(dense));
    assert!(wgs84_distance_m(a, b) < 50.0, "{a:?} {b:?}");
}

#[test]
fn a_long_polar_move_is_rigid_on_the_ground() {
    // 3,000 km north from 88°S: the box's center goes 3,000 km, and its sides and diagonals keep
    // their lengths (to 0.1%; a plain shift in the plane at its old center would shrink its
    // east–west sides by 3.4–3.9%).
    let before = polar_ring();
    let lengths = |r: &[Coord]| [(0, 1), (1, 2), (2, 3), (3, 0), (0, 2), (1, 3)].map(|(i, j)| wgs84_distance_m(r[i], r[j]));
    let (mut s, id) = session(before.clone());
    s.run("edit.move", &json!({"dx": 0.0, "dy": 3000.0, "units": "Kilometers"})).unwrap();
    let after = stored(&s, id);
    let gone = wgs84_distance_m(spherical_centre(&before[..4]).unwrap(), spherical_centre(&after[..4]).unwrap());
    let kept = lengths(&before).iter().zip(lengths(&after)).all(|(b, a)| (a / b - 1.0).abs() < 1e-3);
    assert!((gone - 3_000_000.0).abs() < 3_000.0 && kept, "{gone} {after:?}");
}

#[test]
fn a_polar_edit_reaching_too_far_is_refused() {
    // Moved 19,000 km, or scaled 100,000 times, the polygon would reach toward the far side of the
    // world: refused, and it stays as it was.
    for (cmd, params) in [("edit.move", json!({"dy": -19_000.0, "units": "Kilometers"})), ("edit.scale", json!({"factor": 1e5}))] {
        let (mut s, id) = session(polar_ring());
        let e = s.run(cmd, &params).unwrap_err();
        assert!(e.contains("10,000 km"), "{cmd}: {e}");
        assert_eq!(bits(&stored(&s, id)), bits(&polar_ring()), "{cmd}");
    }
}

#[test]
fn a_tiny_polar_move_moves_a_feature_as_little() {
    // A box from 25°E to 35°E and 87°N to 89°N, moved a trillionth of a meter, or a billionth each
    // way: no corner goes more than a micrometer (the turn the move makes comes from the geodesic,
    // not from a direction measured back over a distance that small).
    let before = boxed(25.0, 87.0, 35.0, 89.0);
    for (dx, dy) in [(1e-12, 0.0), (1e-9, 1e-9), (1e-3, 0.0)] {
        let (mut s, id) = session(before.clone());
        s.run("edit.move", &json!({"dx": dx, "dy": dy})).unwrap();
        let moved = before.iter().zip(stored(&s, id)).map(|(a, b)| wgs84_distance_m(*a, b)).fold(0.0, f64::max);
        assert!(moved < f64::hypot(dx, dy) + 1e-6, "{dx} {dy}: {moved} m");
    }
}

#[test]
fn a_point_moves_onto_a_pole_and_off_it_again() {
    // From 89°N up its meridian as far as the North Pole: it lands there. Moved from the pole, it
    // goes the distance asked.
    let start = Coord::new(10.0, 89.0);
    let (mut s, id) = layer(GeometryType::Point, vec![Geometry::Point(start)]);
    let up = wgs84_distance_m(start, Coord::new(10.0, 90.0));
    s.run("edit.move", &json!({"dy": up})).unwrap();
    let point = |s: &Session| match &s.map().layer(id).unwrap().dataset.features[0].geometry {
        Some(Geometry::Point(c)) => *c,
        g => panic!("{g:?}"),
    };
    let at = point(&s);
    assert!((at.y - 90.0).abs() < 1e-9, "{at:?}");
    s.run("edit.move", &json!({"dx": 1000.0})).unwrap();
    let off = point(&s);
    assert!((wgs84_distance_m(at, off) - 1000.0).abs() < 1e-3, "{off:?}");
}

#[test]
fn polar_neighbors_across_180_degrees_share_their_corners() {
    // From 87°S to 88°S, one from 179°E to 180° and the other from −180° to 179°W: their shared
    // side is written 180° in one and −180° in the other, one place. Moved together, they keep it
    // shared, vertex for vertex.
    let (mut s, id) = layer(
        GeometryType::Polygon,
        vec![
            Geometry::Polygon(vec![vec![boxed(179.0, -88.0, 180.0, -87.0)]]),
            Geometry::Polygon(vec![vec![boxed(-180.0, -88.0, -179.0, -87.0)]]),
        ],
    );
    s.run("edit.move", &json!({"dx": 1000.0})).unwrap();
    let (a, b) = (ring_of(&s, id, 0), ring_of(&s, id, 1));
    assert!(a[1] == b[0] && a[2] == b[3], "{a:?} {b:?}");
}

#[test]
fn features_joined_in_a_chain_move_together() {
    // Three boxes at 86°N to 87°N in a row, each sharing a side with the next only: all three move
    // as one, so both shared sides stay shared.
    let polygon = |x0: f64| Geometry::Polygon(vec![vec![boxed(x0, 86.0, x0 + 5.0, 87.0)]]);
    let (mut s, id) = layer(GeometryType::Polygon, vec![polygon(0.0), polygon(5.0), polygon(10.0)]);
    s.run("edit.move", &json!({"dx": 2000.0, "dy": -1000.0})).unwrap();
    let (a, b, c) = (ring_of(&s, id, 0), ring_of(&s, id, 1), ring_of(&s, id, 2));
    assert!(a[1] == b[0] && a[2] == b[3] && b[1] == c[0] && b[2] == c[3], "{a:?} {b:?} {c:?}");
}

#[test]
fn points_around_a_pole_move_the_same_however_their_longitudes_are_written() {
    // Four points at 88°N around the pole, their center on it: a move east takes its direction from
    // the prime meridian there (east is down 90°E), so writing 270° as −90° changes nothing.
    let moved = |west: f64| {
        let pts = [0.0, 90.0, 180.0, west].map(|x| Coord::new(x, 88.0)).to_vec();
        let (mut s, id) = layer(GeometryType::Point, vec![Geometry::MultiPoint(pts)]);
        s.run("edit.move", &json!({"dx": 1000.0})).unwrap();
        match &s.map().layer(id).unwrap().dataset.features[0].geometry {
            Some(Geometry::MultiPoint(v)) => v.clone(),
            g => panic!("{g:?}"),
        }
    };
    let (a, b) = (moved(270.0), moved(-90.0));
    assert!(a.iter().zip(&b).all(|(p, q)| wgs84_distance_m(*p, *q) < 1e-6), "{a:?} {b:?}");
    let center = spherical_centre(&a).unwrap();
    let from_pole = wgs84_distance_m(center, Coord::new(0.0, 90.0));
    assert!((center.x - 90.0).abs() < 1.0 && (from_pole - 1000.0).abs() < 1.0, "{center:?} {from_pole}");
}

#[test]
fn an_edit_whose_web_mercator_result_leaves_85_degrees_is_made_on_the_ground() {
    // A box from 80°N to 85°N, within Web Mercator's square, scaled twice over: in Web Mercator its
    // top would pass 85.05°, so it's scaled on the ground instead, where each side doubles (scaled
    // in Web Mercator, its bottom side would grow about 2.6 times).
    let before = boxed(0.0, 80.0, 20.0, 85.0);
    let (mut s, id) = session(before.clone());
    s.run("edit.scale", &json!({"factor": 2.0})).unwrap();
    let after = stored(&s, id);
    let ratios: Vec<f64> = (0..4).map(|i| wgs84_distance_m(after[i], after[i + 1]) / wgs84_distance_m(before[i], before[i + 1])).collect();
    assert!(ratios.iter().all(|r| (r / 2.0 - 1.0).abs() < 5e-3), "{ratios:?}");
}

#[test]
fn a_vertex_shared_by_lines_moved_together_stays_shared() {
    // A line spiraling once around the North Pole, its longitudes running on from 10° to 370°, and
    // a second line starting at its end: wherever they land, the end is the start.
    let c = Coord::new;
    let spiral: Vec<Coord> = (0..7).map(|i| c(10.0 + 60.0 * f64::from(i), 87.0 - f64::from(i) / 12.0)).collect();
    let next = vec![spiral[6], c(375.0, 86.0)];
    let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(vec![spiral]), Geometry::Line(vec![next])]);
    s.run("edit.move", &json!({"dx": 1000.0, "dy": 500.0})).unwrap();
    let line = |row: usize| match &s.map().layer(id).unwrap().dataset.features[row].geometry {
        Some(Geometry::Line(parts)) => parts[0].clone(),
        g => panic!("{g:?}"),
    };
    let (a, b) = (line(0), line(1));
    assert_eq!(bits(&a[6..]), bits(&b[..1]), "{a:?} {b:?}");
}

#[test]
fn polar_neighbors_share_a_corner_written_as_zero_or_minus_zero() {
    // Side by side from 86°N to 87°N, one writing their shared meridian as 0.0, the other as -0.0:
    // one place, so they move together and keep the corners shared.
    let c = Coord::new;
    let west = vec![c(-5.0, 86.0), c(0.0, 86.0), c(0.0, 87.0), c(-5.0, 87.0), c(-5.0, 86.0)];
    let east = vec![c(-0.0, 86.0), c(5.0, 86.0), c(5.0, 87.0), c(-0.0, 87.0), c(-0.0, 86.0)];
    let (mut s, id) = layer(GeometryType::Polygon, vec![Geometry::Polygon(vec![vec![west]]), Geometry::Polygon(vec![vec![east]])]);
    s.run("edit.move", &json!({"dx": 1000.0, "dy": 500.0})).unwrap();
    let (a, b) = (ring_of(&s, id, 0), ring_of(&s, id, 1));
    assert!(a[1] == b[0] && a[2] == b[3], "{a:?} {b:?}");
}

#[test]
fn edits_refuse_points_past_the_poles() {
    for (cmd, params) in
        [("edit.move", json!({"dx": 100, "dy": 0})), ("edit.rotate", json!({"angle": 20})), ("edit.scale", json!({"factor": 2}))]
    {
        let g = Geometry::MultiPoint(vec![Coord::new(0.0, 95.0), Coord::new(1.0, 89.0)]);
        let (mut s, id) = layer(GeometryType::Point, vec![g.clone()]);
        let err = s.run(cmd, &params).unwrap_err();
        assert!(err.contains("latitude past ±90°"), "{cmd}: {err}");
        assert_eq!(s.map().layer(id).unwrap().dataset.features[0].geometry, Some(g));
        let (mut clean, _) = layer(GeometryType::Point, vec![Geometry::MultiPoint(vec![Coord::new(0.0, 88.0), Coord::new(1.0, 89.0)])]);
        clean.run(cmd, &params).unwrap();
    }
}

#[test]
fn split_searches_every_turn_of_a_long_polar_line() {
    let (mut s, id) = layer(GeometryType::Polyline, vec![Geometry::Line(vec![vec![Coord::new(0.0, 89.0), Coord::new(368_640.0, 89.0)]])]);
    s.run("edit.split", &json!({"lon": 0.014, "lat": 89.0})).unwrap();
    assert_eq!(s.map().layer(id).unwrap().dataset.features.len(), 2);
    let Some(Geometry::Line(parts)) = &s.map().layer(id).unwrap().dataset.features[0].geometry else { panic!("expected a line") };
    let cut = *parts[0].last().unwrap();
    assert!(wgs84_distance_m(cut, Coord::new(0.014, 89.0)) < 0.01, "{cut:?}");
}
