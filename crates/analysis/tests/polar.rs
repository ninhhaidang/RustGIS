//! Polar features keep their shape and their relations through the analysis tools (#1033):
//! nothing is clamped onto 85.05°, where Web Mercator ends.
use rustgis_analysis::{Inputs, Output, ToolResult};
use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value};
use std::collections::HashMap;

struct Map(HashMap<String, Dataset>);
impl Inputs for Map {
    fn dataset(&self, layer: &str) -> Option<&Dataset> {
        self.0.get(layer)
    }
    fn selection(&self, _: &str) -> &[usize] {
        &[]
    }
}

fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Geometry {
    let c = Coord::new;
    Geometry::Polygon(vec![vec![vec![c(x0, y0), c(x1, y0), c(x1, y1), c(x0, y1), c(x0, y0)]]])
}

fn layer(name: &str, geometries: Vec<Geometry>) -> (String, Dataset) {
    let mut ds = Dataset::new(name, GeometryType::Polygon, vec![Field::new("k", FieldType::Text)]);
    for g in geometries {
        ds.push(Some(g), vec![Value::Text("a".into())]);
    }
    (name.to_string(), ds)
}

/// Two overlapping polygons between 87°S and 89°S, a mask over them, and polygons from 86°S to 87°S
/// and 88°S to 89°S, about 111 km apart.
fn polar() -> Map {
    Map(HashMap::from([
        layer("polar", vec![square(0.0, -89.0, 1.0, -87.0), square(0.5, -89.0, 1.5, -87.0)]),
        layer("mask", vec![square(-10.0, -89.9, 10.0, -86.0)]),
        layer("upper", vec![square(0.0, -87.0, 1.0, -86.0)]),
        layer("lower", vec![square(0.0, -89.0, 1.0, -88.0)]),
    ]))
}

fn dataset(r: ToolResult) -> Dataset {
    match r.output {
        Output::Layer(d) | Output::Table(d) => d,
        _ => panic!("expected a layer"),
    }
}

/// Every latitude of every feature's geometry, with the feature count.
fn latitudes(ds: &Dataset) -> (usize, Vec<f64>) {
    let mut lats = Vec::new();
    for f in ds.features.iter() {
        if let Some(g) = &f.geometry {
            g.for_each_coord(|c| lats.push(c.y));
        }
    }
    (ds.features.len(), lats)
}

fn run(m: &Map, tool: &str, params: serde_json::Value) -> Dataset {
    dataset(rustgis_analysis::run(tool, m, &params).unwrap_or_else(|e| panic!("{tool}: {e}")))
}

#[test]
fn polar_polygons_keep_their_shape_through_the_tools() {
    let m = polar();
    let between = |lats: &[f64]| !lats.is_empty() && lats.iter().all(|y| (-89.0 - 1e-9..=-87.0 + 1e-9).contains(y));
    let reach = |lats: &[f64], y: f64| lats.iter().any(|v| (v - y).abs() < 1e-9);

    // Clip by a mask that covers them: both come through whole.
    let (n, lats) = latitudes(&run(&m, "clip", serde_json::json!({"input": "polar", "clip": "mask"})));
    assert!(n == 2 && between(&lats) && reach(&lats, -89.0) && reach(&lats, -87.0), "clip: {n} {lats:?}");
    // Dissolve unions them into one polygon spanning both (to the overlay's grid, under a
    // millimeter: 1e-7° here).
    let d = run(&m, "dissolve", serde_json::json!({"input": "polar", "field": "k"}));
    let (n, lats) = latitudes(&d);
    let e = d.features.first().and_then(|f| f.geometry.as_ref()).map(|g| g.extent()).unwrap();
    assert!(n == 1 && between(&lats) && e.min_x.abs() < 1e-7 && (e.max_x - 1.5).abs() < 1e-7, "dissolve: {lats:?} {e:?}");
    // Simplify keeps a square's corners, hull and points stay inside.
    let (_, lats) = latitudes(&run(&m, "simplify", serde_json::json!({"input": "polar", "tolerance": 0.001})));
    assert!(between(&lats) && reach(&lats, -89.0) && reach(&lats, -87.0), "simplify: {lats:?}");
    let (_, lats) = latitudes(&run(&m, "convexHull", serde_json::json!({"input": "polar"})));
    assert!(between(&lats) && reach(&lats, -89.0) && reach(&lats, -87.0), "convexHull: {lats:?}");
    let (n, lats) = latitudes(&run(&m, "featureToPoint", serde_json::json!({"input": "polar"})));
    assert!(n == 2 && lats.iter().all(|y| (-89.0..-87.0).contains(y)), "featureToPoint: {lats:?}");
}

#[test]
fn a_cap_around_a_pole_keeps_its_pole_through_the_overlays() {
    let c = Coord::new;
    // As files cut a cap around the South Pole: along 85°S, down to the pole at 180°, and back.
    let cap = Geometry::Polygon(vec![vec![vec![c(-180.0, -85.0), c(180.0, -85.0), c(180.0, -90.0), c(-180.0, -90.0), c(-180.0, -85.0)]]]);
    let m = Map(HashMap::from([layer("cap", vec![cap]), layer("mask", vec![square(-180.0, -90.0, 180.0, -80.0)])]));
    // Within the overlays' grid of the pole and of 85°S, and nowhere in between them.
    let pole = |lats: &[f64]| lats.iter().any(|y| (y + 90.0).abs() <= rustgis_geom::POLE_TOLERANCE);
    let edge = |lats: &[f64]| lats.iter().any(|y| (y + 85.0).abs() < 1e-7);
    for (tool, params) in [
        ("clip", serde_json::json!({"input": "cap", "clip": "mask"})),
        ("dissolve", serde_json::json!({"input": "cap", "field": "k"})),
        ("simplify", serde_json::json!({"input": "cap", "tolerance": 0.001})),
    ] {
        let (n, lats) = latitudes(&run(&m, tool, params));
        assert!(n == 1 && pole(&lats) && edge(&lats) && lats.iter().all(|y| (-90.0 - 1e-9..=-85.0 + 1e-7).contains(y)), "{tool}: {lats:?}");
    }
}

#[test]
fn polar_polygons_apart_are_apart() {
    let m = polar();
    // 86–87°S and 88–89°S don't meet: they did when both were clamped onto 85.05°S.
    let r = rustgis_analysis::run("selectByLocation", &m, &serde_json::json!({"input": "upper", "select": "lower"})).unwrap();
    let Output::Selection { rows, .. } = r.output else { panic!("expected a selection") };
    assert!(rows.is_empty(), "{rows:?}");
    // The closest one is 1° of latitude away near 88°S: 111.69 km on the WGS 84 meridian, not 0.
    let joined =
        run(&m, "spatialJoin", serde_json::json!({"target": "upper", "join": "lower", "match": "Closest", "distanceField": "gap"}));
    let gap = joined.field_index("gap").map(|i| joined.value(0, i).clone());
    let km = match gap {
        Some(Value::Float(v)) => v,
        other => panic!("gap: {other:?}"),
    };
    assert!((km - 111.69).abs() < 0.05, "{km}");
}

#[test]
fn line_ends_near_a_pole_meet_within_the_tolerance() {
    // Two short lines at 89.6°N, 500 m apart in the plane, 3.5 m on the ground: no dangles at 10 m.
    let c = Coord::new;
    let dx = (500.0 / rustgis_geom::MERCATOR_R).to_degrees();
    let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![]);
    for x in [0.0, dx] {
        ds.push(Some(Geometry::Line(vec![vec![c(x, 89.6), c(x, 89.6001)]])), vec![]);
    }
    let checked = rustgis_analysis::topology::dangles(&ds, &[0, 1], 10.0);
    assert!(checked.complete && checked.errors.is_empty(), "{:?}", checked.errors.iter().map(|e| e.at).collect::<Vec<_>>());

    // The same lines 5 km apart in the plane, about 35 m on the ground: all four ends dangle.
    // (Clamped onto 85.05°, each line was a single point, and none was found.)
    let dx = (5_000.0 / rustgis_geom::MERCATOR_R).to_degrees();
    let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![]);
    for x in [0.0, dx] {
        ds.push(Some(Geometry::Line(vec![vec![c(x, 89.6), c(x, 89.6001)]])), vec![]);
    }
    let checked = rustgis_analysis::topology::dangles(&ds, &[0, 1], 10.0);
    assert!(checked.complete && checked.errors.len() == 4, "{:?}", checked.errors.iter().map(|e| e.at).collect::<Vec<_>>());

    // Lines end to end along 89.9°N, where a degree of longitude is 194 m: 5 m apart on the ground
    // they meet, and only their outer ends dangle; 30 m apart, all four do.
    let per_degree = rustgis_geom::MERCATOR_R * 89.9f64.to_radians().cos() * std::f64::consts::PI / 180.0;
    for (gap_m, dangling) in [(5.0, 2), (30.0, 4)] {
        let gap = gap_m / per_degree;
        let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![vec![c(0.0, 89.9), c(1.0, 89.9)]])), vec![]);
        ds.push(Some(Geometry::Line(vec![vec![c(1.0 + gap, 89.9), c(2.0, 89.9)]])), vec![]);
        let checked = rustgis_analysis::topology::dangles(&ds, &[0, 1], 10.0);
        let at: Vec<Coord> = checked.errors.iter().map(|e| e.at).collect();
        assert!(checked.complete && at.len() == dangling, "{gap_m} m: {at:?}");
    }

    // Meridians at 0°E and 90°E from 80°N meet at the North Pole, where every longitude is one
    // place: only their ends at 80°N dangle.
    let mut ds = Dataset::new("meridians", GeometryType::Polyline, vec![]);
    for x in [0.0, 90.0] {
        ds.push(Some(Geometry::Line(vec![vec![c(x, 80.0), c(x, 90.0)]])), vec![]);
    }
    let checked = rustgis_analysis::topology::dangles(&ds, &[0, 1], 10.0);
    let at: Vec<f64> = checked.errors.iter().map(|e| e.at.y).collect();
    assert!(checked.complete && at.len() == 2 && at.iter().all(|y| (y - 80.0).abs() < 1e-9), "{at:?}");
}

#[test]
fn line_ends_near_a_pole_are_looked_up_not_scanned() {
    // 7,000 lines end to end around 88°S, 200 m each: every end meets its neighbor's, the chain's
    // first and last too, across ±180°. Each end is measured only against the lines near it, so the
    // check finishes (against every line, it would pass the work limit).
    let c = Coord::new;
    let n = 7_000;
    let lon = |i: i32| -180.0 + 360.0 * f64::from(i) / f64::from(n);
    let mut ds = Dataset::new("chain", GeometryType::Polyline, vec![]);
    for i in 0..n {
        ds.push(Some(Geometry::Line(vec![vec![c(lon(i), -88.0), c(lon(i + 1), -88.0)]])), vec![]);
    }
    let rows: Vec<usize> = (0..ds.features.len()).collect();
    let checked = rustgis_analysis::topology::dangles(&ds, &rows, 10.0);
    let at: Vec<Coord> = checked.errors.iter().map(|e| e.at).collect();
    assert!(checked.complete && at.is_empty(), "{} found: {:?}", at.len(), at.iter().take(4).collect::<Vec<_>>());

    // 8,000 short lines on top of one another at 89°N: each end meets the first other line it
    // looks at, so the check finishes (measuring every line against every end, it wouldn't).
    let mut ds = Dataset::new("pile", GeometryType::Polyline, vec![]);
    for _ in 0..8_000 {
        ds.push(Some(Geometry::Line(vec![vec![c(0.0, 89.0), c(0.001, 89.0)]])), vec![]);
    }
    let rows: Vec<usize> = (0..ds.features.len()).collect();
    let checked = rustgis_analysis::topology::dangles(&ds, &rows, 10.0);
    assert!(checked.complete && checked.errors.is_empty(), "{} found", checked.errors.len());
}

/// How many line ends dangle at `tol_m` among `lines`, each a list of (longitude, latitude).
fn dangling(lines: &[&[(f64, f64)]], tol_m: f64) -> usize {
    let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![]);
    for line in lines {
        ds.push(Some(Geometry::Line(vec![line.iter().map(|&(x, y)| Coord::new(x, y)).collect()])), vec![]);
    }
    let rows: Vec<usize> = (0..ds.features.len()).collect();
    let checked = rustgis_analysis::topology::dangles(&ds, &rows, tol_m);
    assert!(checked.complete);
    checked.errors.len()
}

#[test]
fn line_ends_close_to_a_pole_are_measured_across_it() {
    // Meridian lines from 89.9°, ending `rho` meters from the pole (the North Pole, or the South
    // Pole when `s` is -1): their ends there meet (only the two at 89.9° dangle) when within the
    // tolerance of the other line, through the pole.
    let from_pole = |rho_m: f64| 90.0 - degrees(rho_m);
    for (lon_a, rho_a, lon_b, rho_b, s, tol, want) in [
        (0.0, 11.0, 40.0, 2.0, 1.0, 10.0, 2),  // a's end 7.07 m from b's line (9.55 m from its end)
        (0.0, 11.0, 60.0, 5.5, 1.0, 10.0, 2),  // 9.53 m from b's line
        (0.0, 2.0, 90.0, 11.0, 1.0, 10.0, 4),  // 11.18 m from end to end
        (0.0, 4.5, 180.0, 4.5, 1.0, 10.0, 2),  // 9.0 m, through the pole
        (0.0, 4.5, 180.0, 4.5, -1.0, 10.0, 2), // the same through the South Pole
        (0.0, 1.0, 180.0, 10.0, 1.0, 10.0, 4), // 11.0 m
        (0.0, 11.1, 180.0, 0.0, 1.0, 15.0, 2), // one at the pole, 11.1 m away
    ] {
        let a = [(lon_a, s * 89.9), (lon_a, s * from_pole(rho_a))];
        let b = [(lon_b, s * 89.9), (lon_b, s * from_pole(rho_b))];
        let got = dangling(&[&a, &b], tol);
        assert_eq!(got, want, "{lon_a}° {rho_a} m and {lon_b}° {rho_b} m at {tol} m ({s})");
    }
    // A lone line reaching the pole still dangles there.
    assert_eq!(dangling(&[&[(0.0, 89.0), (0.0, 90.0)]], 10.0), 2);
}

/// Degrees of latitude in `m` meters near a pole (WGS 84's radius of curvature there, a²/b).
fn degrees(m: f64) -> f64 {
    (m / rustgis_geom::WGS84_POLAR_RADIUS_M).to_degrees()
}

#[test]
fn line_ends_near_a_pole_are_measured_to_the_edge_as_it_runs() {
    // Beyond 85.05° an edge straight in the plane curves in the frame about the pole that ends are
    // measured in: along a parallel it bows away from the pole (by up to 0.2 m over 0.09° at 85°),
    // and running toward the pole it spirals, so it's measured in pieces short enough to follow it
    // to a millimeter. An end 1.1 m poleward of an edge's middle dangles at 1 m (the default
    // tolerance), and an end on an edge along a parallel meets it at 0.1 m. (At 84.9°, within Web
    // Mercator's square, ends are checked as before.)
    for lat in [84.9, 85.06, 87.0, 89.0] {
        let edge = [(0.0, lat), (0.09, lat + degrees(11.0))];
        let near = lat + degrees(5.5 + 1.1);
        assert_eq!(dangling(&[&edge, &[(0.045, near), (0.045, near + degrees(100.0))]], 1.0), 4, "{lat}");
        let parallel = [(0.0, lat), (0.09, lat)];
        assert_eq!(dangling(&[&parallel, &[(0.045, lat), (0.045, lat - degrees(100.0))]], 0.1), 3, "{lat}");
    }
    // An end on the middle (in the plane) of an edge running mostly toward the pole meets it, at
    // each tolerance: only the edge's ends and the other line's far end dangle.
    for ((x0, y0), (x1, y1), tol) in [
        ((0.0, 86.0), (0.0069, 89.9), 1.0),
        ((0.0, 86.0), (0.0069, 89.9), 10.0),
        ((0.0, 85.1), (0.01, 89.99), 10.0),
        ((0.0, 85.1), (0.05, 89.99), 10.0),
        ((0.0, 85.06), (0.02, 89.9999), 0.5),
    ] {
        let (a, b) = (rustgis_geom::lonlat_to_plane(Coord::new(x0, y0)), rustgis_geom::lonlat_to_plane(Coord::new(x1, y1)));
        let mid = rustgis_geom::plane_to_lonlat(Coord::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0));
        let edge = [(x0, y0), (x1, y1)];
        assert_eq!(dangling(&[&edge, &[(mid.x, mid.y), (mid.x + 0.01, mid.y)]], tol), 3, "{edge:?} at {tol} m");
    }
}

#[test]
fn a_graticule_near_a_pole_is_checked_in_full() {
    // Meridians every 0.25° from 80°N up to 86°N, ending on that parallel, drawn as one edge right
    // round: only their 1,440 ends at 80°N dangle (the parallel's own ends meet across ±180°), and
    // the check finishes, since only the stretch of the long edge within each end's reach is
    // measured.
    let mut lines: Vec<Vec<(f64, f64)>> = (0..1_440)
        .map(|i| {
            let x = -180.0 + 0.25 * f64::from(i);
            vec![(x, 80.0), (x, 86.0)]
        })
        .collect();
    lines.push(vec![(-180.0, 86.0), (180.0, 86.0)]);
    let refs: Vec<&[(f64, f64)]> = lines.iter().map(Vec::as_slice).collect();
    assert_eq!(dangling(&refs, 1.0), 1_440);
}

#[test]
fn line_ends_near_a_pole_meet_north_south_and_across_180() {
    // Lines end to end along the meridian at 10°E around 89°N: 5 m apart they meet, 30 m apart they
    // don't (north–south the plane stretches by sec 85.05° beyond it, not by sec(latitude)).
    let gap = degrees;
    for (gap_m, want) in [(5.0, 2), (30.0, 4)] {
        let a = [(10.0, 88.9), (10.0, 89.0)];
        let b = [(10.0, 89.0 + gap(gap_m)), (10.0, 89.1)];
        assert_eq!(dangling(&[&a, &b], 10.0), want, "{gap_m} m");
    }
    // Lines along 89.9°N meeting across ±180°, 0.02° (3.9 m) apart: only their outer ends dangle.
    assert_eq!(dangling(&[&[(170.0, 89.9), (179.99, 89.9)], &[(-179.99, 89.9), (-170.0, 89.9)]], 10.0), 2);
    // One line all the way around 89.9°N ends where it starts: no dangle.
    assert_eq!(dangling(&[&[(-180.0, 89.9), (180.0, 89.9)]], 10.0), 0);
    // A line curling back onto itself at 88°S, like a 6: only its first end dangles.
    assert_eq!(dangling(&[&[(0.0, -88.1), (0.0, -88.0), (1.0, -88.0), (1.0, -88.05), (0.0, -88.05)]], 10.0), 1);
}

#[test]
fn an_edge_crossing_85_degrees_is_straight_in_the_plane() {
    // A mask whose edge runs from (0°, 80°N) to (10°E, 89°N), across 85.05°: the tools follow it
    // straight in the plane, so a line along 84.5°N is cut at 3.99°E. (Web Mercator, clamping the
    // edge's far end onto 85.05°, cut it at 8.50°E.)
    let c = Coord::new;
    let mut line = Dataset::new("line", GeometryType::Polyline, vec![Field::new("k", FieldType::Text)]);
    line.push(Some(Geometry::Line(vec![vec![c(0.0, 84.5), c(10.0, 84.5)]])), vec![Value::Text("a".into())]);
    let mask = Geometry::Polygon(vec![vec![vec![c(0.0, 80.0), c(10.0, 89.0), c(10.0, 80.0), c(0.0, 80.0)]]]);
    let m = Map(HashMap::from([("line".to_string(), line), layer("mask", vec![mask])]));
    let clipped = run(&m, "clip", serde_json::json!({"input": "line", "clip": "mask"}));
    let e = clipped.features.first().and_then(|f| f.geometry.as_ref()).map(|g| g.extent()).unwrap();
    assert!((e.min_x - 3.986).abs() < 0.002 && (e.max_x - 10.0).abs() < 1e-6, "{e:?}");
}

#[test]
fn simplify_near_a_pole_drops_nothing_farther_than_its_tolerance() {
    // Beyond 85.05° the plane stretches east–west by sec(latitude) but north–south by a constant
    // sec(85.05°), so a tolerance in meters can't be right both ways: for a feature wholly beyond
    // 85°, Simplify converts it (at the feature's label point, as everywhere) with a scale no
    // larger than the smaller of the two (sec 85°), and nothing it drops is farther than the
    // tolerance on the ground, either way.
    let c = Coord::new;
    // A box from 0° to 10°E and 89°S to 86°S. Its northern edge zigzags 0.05° (5.6 km) and 0.02°
    // (2.2 km) north and south; its western edge 1° (3.4 to 7.3 km) and 0.3° (0.7 to 1.9 km) east
    // and west.
    let mut ring = Vec::new();
    for i in 0..=20 {
        let bump = match i % 4 {
            1 => 0.05,
            3 => -0.02,
            _ => 0.0,
        };
        ring.push(c(10.0 - 0.5 * f64::from(i), -86.0 + bump));
    }
    for j in 1..12 {
        let bump = match j % 4 {
            1 => 1.0,
            3 => -0.3,
            _ => 0.0,
        };
        ring.push(c(bump, -86.0 - 0.25 * f64::from(j)));
    }
    ring.extend([c(0.0, -89.0), c(10.0, -89.0), c(10.0, -86.0)]);
    let original = ring.clone();
    let m = Map(HashMap::from([layer("box", vec![Geometry::Polygon(vec![vec![ring]])])]));
    let tolerance_km = 3.0;
    let simplified = run(&m, "simplify", serde_json::json!({"input": "box", "tolerance": tolerance_km}));
    let kept: Vec<Coord> = match simplified.features.first().and_then(|f| f.geometry.as_ref()) {
        Some(Geometry::Polygon(parts)) => parts.first().and_then(|rings| rings.first()).cloned().unwrap_or_default(),
        other => panic!("expected a polygon: {other:?}"),
    };
    assert!(kept.len() > 4 && kept.len() < original.len(), "{} of {} kept", kept.len(), original.len());
    // How far a point is from what's kept, on the ground: each kept edge followed as the plane
    // draws it, in 2,000 steps (under 170 m each here: hence 100 m allowed past the tolerance).
    let (to_plane, back) = (rustgis_geom::lonlat_to_plane, rustgis_geom::plane_to_lonlat);
    let from_kept = |v: Coord| {
        kept.windows(2)
            .flat_map(|w| {
                let (a, b) = (to_plane(w[0]), to_plane(w[1]));
                let steps = 2_000;
                (0..=steps).map(move |k| {
                    let t = f64::from(k) / f64::from(steps);
                    back(Coord::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t))
                })
            })
            .map(|q| rustgis_geom::wgs84_distance_m(v, q))
            .fold(f64::INFINITY, f64::min)
    };
    for v in original {
        let d = from_kept(v);
        assert!(d <= tolerance_km * 1000.0 + 100.0, "{v:?} is {d:.0} m from the simplified box");
    }
}

#[test]
fn the_plane_keeps_longitudes_past_180_as_they_are() {
    // A polygon across the antimeridian, its longitudes running on from 179.6° to 180.2° (as moving
    // a feature across it leaves them): its convex hull is 0.6° wide, not folded around the world,
    // at 10°N as at 86°S, beyond 85.05°.
    let c = Coord::new;
    for lat in [10.0, -86.0] {
        let ring = vec![c(179.6, lat), c(180.2, lat), c(180.2, lat + 0.4), c(179.6, lat + 0.4), c(179.6, lat)];
        let m = Map(HashMap::from([layer("across", vec![Geometry::Polygon(vec![vec![ring]])])]));
        let hull = run(&m, "convexHull", serde_json::json!({"input": "across"}));
        let e = hull.features.first().and_then(|f| f.geometry.as_ref()).map(|g| g.extent()).unwrap();
        assert!((e.max_x - e.min_x - 0.6).abs() < 1e-9 && (e.min_x - 179.6).abs() < 1e-9, "{lat}: {e:?}");
        assert!((e.min_y - lat).abs() < 1e-7 && (e.max_y - lat - 0.4).abs() < 1e-7, "{lat}: {e:?}");
    }
}

#[test]
fn a_radius_near_a_pole_reaches_as_far_as_it_says() {
    let c = Coord::new;
    let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
    // 33.5 km and 55.8 km north of 89°N, and 447 km away at 85°N, on the same meridian.
    for y in [89.3, 89.5, 85.0] {
        pts.push(Some(Geometry::Point(c(10.0, y))), vec![]);
    }
    let found = rustgis_analysis::network::within(&pts, &[0, 1, 2], c(10.0, 89.0), 50_000.0);
    assert_eq!(found.iter().map(|(r, _)| *r).collect::<Vec<_>>(), [0], "{found:?}");
    let d = found.first().map(|(_, d)| *d).unwrap_or_default();
    assert!((d - 33_507.0).abs() < 50.0, "{d}");

    // 120 km around the South Pole reaches 89°S (111.7 km) and not 88.9°S (122.9 km).
    let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
    for (x, y) in [(0.0, -89.0), (10.0, -88.9)] {
        pts.push(Some(Geometry::Point(c(x, y))), vec![]);
    }
    let found = rustgis_analysis::network::within(&pts, &[0, 1], c(0.0, -90.0), 120_000.0);
    assert_eq!(found.iter().map(|(r, _)| *r).collect::<Vec<_>>(), [0], "{found:?}");
}

#[test]
fn a_radius_reaches_polar_shapes_along_their_edges() {
    let c = Coord::new;
    let ground = rustgis_geom::wgs84_distance_m;
    let shapes = |g: Geometry| {
        let mut ds = Dataset::new("shapes", GeometryType::Polygon, vec![]);
        ds.push(Some(g), vec![]);
        ds
    };
    let reach = |ds: &Dataset, at: Coord, r: f64| rustgis_analysis::network::within(ds, &[0], at, r).first().map(|(_, d)| *d);
    // Inside a box from 84°N to 89°N: no distance at all. From a center that's no place, nothing.
    assert_eq!(reach(&shapes(square(0.0, 84.0, 90.0, 89.0)), c(45.0, 85.0), 50_000.0), Some(0.0));
    for at in [c(45.0, 1000.0), c(45.0, f64::NAN), c(f64::INFINITY, 85.0)] {
        assert_eq!(reach(&shapes(square(0.0, 84.0, 90.0, 89.0)), at, 50_000_000.0), None, "{at:?}");
    }
    // A cap written from 0° to 360° holds a center at 10°W, inside it on the ground, however its
    // longitude is written. A radius that's no distance reaches nothing.
    let cap360 = shapes(Geometry::Polygon(vec![vec![vec![c(0.0, 85.0), c(360.0, 85.0), c(360.0, 90.0), c(0.0, 90.0), c(0.0, 85.0)]]]));
    for x in [-10.0, 710.0, -370.0] {
        assert_eq!(reach(&cap360, c(x, 88.0), 100_000.0), Some(0.0), "{x}");
    }
    for r in [f64::NAN, -1.0] {
        assert!(rustgis_analysis::network::within(&cap360, &[0], c(-10.0, 88.0), r).is_empty(), "{r}");
    }
    // A polygon whose ring is one point, 10 km off, written once or closed: as far as that point.
    for ring in [vec![c(10.0, 86.0)], vec![c(10.0, 86.0), c(10.0, 86.0)]] {
        let d = reach(&shapes(Geometry::Polygon(vec![vec![ring]])), c(10.0, 86.09), 50_000.0);
        assert!(d.is_some_and(|d| (d - ground(c(10.0, 86.09), c(10.0, 86.0))).abs() < 10.0), "{d:?}");
    }
    // A cap cut at ±180°, its edge along 85.5°N half a degree north: that far, not as far as the
    // pole.
    let cap = Geometry::Polygon(vec![vec![vec![c(-180.0, 85.5), c(180.0, 85.5), c(180.0, 90.0), c(-180.0, 90.0), c(-180.0, 85.5)]]]);
    let d = reach(&shapes(cap), c(0.0, 85.0), 100_000.0);
    assert!(d.is_some_and(|d| (d - ground(c(0.0, 85.0), c(0.0, 85.5))).abs() < 10.0), "{d:?}");
    // A meridian from pole to pole, a degree west at 45°N: about 79 km off, not 0 by a chord.
    let mut line = Dataset::new("meridian", GeometryType::Polyline, vec![]);
    line.push(Some(Geometry::Line(vec![vec![c(0.0, -90.0), c(0.0, 90.0)]])), vec![]);
    let d = reach(&line, c(1.0, 45.0), 100_000.0);
    assert!(d.is_some_and(|d| (d - ground(c(1.0, 45.0), c(0.0, 45.0))).abs() < 100.0), "{d:?}");
    // From 80°N, 1,600 km reaches a point beyond 85.05° on the far side of the pole, 1,563 km away
    // over it. One within ±85.05°, from a center within it, is measured in local Web Mercator as
    // before, which doesn't go over the pole: 2,300 km from 80°N doesn't reach 80°N across it.
    let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
    pts.push(Some(Geometry::Point(c(180.0, 86.0))), vec![]);
    let d = reach(&pts, c(0.0, 80.0), 1_600_000.0);
    assert!(d.is_some_and(|d| (d - ground(c(0.0, 80.0), c(180.0, 86.0))).abs() < 100.0), "{d:?}");
    let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
    pts.push(Some(Geometry::Point(c(180.0, 80.0))), vec![]);
    assert_eq!(reach(&pts, c(0.0, 80.0), 2_300_000.0), None);
    // A line along 180° from 88.1°N to 89.4°S, through the antipode (0°, 89°S) of a center at 89°N,
    // where the plane at the center tears: 400 km reaches it 2.9° away over the pole (324 km), not
    // at a chord across the tear; 200 km doesn't.
    let mut line = Dataset::new("through", GeometryType::Polyline, vec![]);
    line.push(Some(Geometry::Line(vec![vec![c(180.0, 88.1), c(180.0, -89.4)]])), vec![]);
    let d = reach(&line, c(0.0, 89.0), 400_000.0);
    assert!(d.is_some_and(|d| (d - ground(c(0.0, 89.0), c(180.0, 88.1))).abs() < 100.0), "{d:?}");
    assert_eq!(reach(&line, c(0.0, 89.0), 200_000.0), None);
    // A line from near the center (180°, 89°N; 55 km off at its nearest) down to 89°S and along it
    // past the center's antipode (0°, 89°S), where the plane at the center draws neighboring steps
    // on opposite sides of it, their chord 40 km from the center: not within 50 km.
    let mut line = Dataset::new("far", GeometryType::Polyline, vec![]);
    // (Steps from 150.25°E fall either side of 0°, not on it.)
    line.push(Some(Geometry::Line(vec![vec![c(150.25, 89.5), c(150.25, -89.0), c(-170.0, -89.0)]])), vec![]);
    assert_eq!(reach(&line, c(180.0, 89.0), 50_000.0), None);
    let d = reach(&line, c(180.0, 89.0), 60_000.0);
    assert!(d.is_some_and(|d| (52_000.0..58_000.0).contains(&d)), "{d:?}");
}

#[test]
fn a_radius_follows_hostile_lines_near_a_pole_in_bounded_steps() {
    // 200 lines, each going 348 times nearly all the way around along 89.95°N, 5.6 km from a center
    // at 89.9°N: about 50 million half-degree steps, far past the search's budget. The edges
    // followed before it runs out find the first line where it is; every line still counts, the
    // rest at their boxes' distance (5.5 km), never more than they are.
    let c = Coord::new;
    let path: Vec<Coord> = (0..=348).map(|i| c(if i % 2 == 0 { -179.5 } else { 179.5 }, 89.95)).collect();
    let mut lines = Dataset::new("hostile", GeometryType::Polyline, vec![]);
    for _ in 0..200 {
        lines.push(Some(Geometry::Line(vec![path.clone()])), vec![]);
    }
    let rows: Vec<usize> = (0..200).collect();
    let found = rustgis_analysis::network::within(&lines, &rows, c(0.0, 89.9), 10_000.0);
    assert_eq!(found.len(), 200);
    assert!(found.iter().all(|(_, d)| (5_500.0..5_600.0).contains(d)), "{:?}", found.first());
}

#[test]
fn a_larger_radius_never_loses_a_feature() {
    // Each shape is measured one way whatever the radius. Within ±85.05°, in local Web Mercator: a
    // point at 75°N from Paris, whether the radius reaches 85.05° or not, and one at 82.95°N from
    // 84°N the same.
    let c = Coord::new;
    let found = |at: Coord, p: Coord, r: f64| {
        let mut pts = Dataset::new("pts", GeometryType::Point, vec![]);
        pts.push(Some(Geometry::Point(p)), vec![]);
        rustgis_analysis::network::within(&pts, &[0], at, r).first().map(|(_, d)| *d)
    };
    let paris = c(2.35, 48.86);
    assert_eq!(found(paris, c(2.35, 75.0), 4_000_000.0), found(paris, c(2.35, 75.0), 4_100_000.0));
    let (at, p) = (c(0.0, 84.0), c(3.9277, 82.9505));
    let (near, far) = (found(at, p, 117_000.0), found(at, p, 117_020.0));
    assert!(near.is_some() && near == far, "{near:?} {far:?}");
    // On the ground: a line along 89.99°N, 10 km from a center at 89.9°N, found at 20 km, where 400
    // lines around 89.6°N (33 km off) are out of reach, and at 50 km, where following those lines
    // first leaves too few of the search's steps for it, so it counts as near as its box (9.95 km):
    // never farther than it is.
    let at = c(0.0, 89.9);
    let mut ds = Dataset::new("polar", GeometryType::Polyline, vec![]);
    for _ in 0..400 {
        ds.push(Some(Geometry::Line(vec![vec![c(-179.5, 89.6), c(179.5, 89.6)]])), vec![]);
    }
    ds.push(Some(Geometry::Line(vec![vec![c(-150.0, 89.99), c(150.0, 89.99)]])), vec![]);
    let rows: Vec<usize> = (0..=400).collect();
    let point = |r: f64| rustgis_analysis::network::within(&ds, &rows, at, r).into_iter().find(|(row, _)| *row == 400).map(|(_, d)| d);
    let (near, far) = (point(20_000.0), point(50_000.0));
    let fell_back = far.is_some_and(|f| (9_940.0..9_960.0).contains(&f) && near.is_some_and(|n| f < n));
    assert!(near.is_some_and(|d| (9_900.0..10_200.0).contains(&d)) && fell_back, "{near:?} {far:?}");
}

#[test]
fn features_reaching_the_same_pole_meet_there() {
    // Wedges from 89°S to the South Pole at 0°E and 90°E: apart in the plane, where the pole is a
    // line, but one place on the ground. One at 0°E reaching the North Pole meets neither.
    let wedge = |x: f64, s: f64| square(x, s * 90.0, x + 10.0, s * 89.0);
    let m = Map(HashMap::from([
        layer("south0", vec![wedge(0.0, -1.0)]),
        layer("south90", vec![wedge(90.0, -1.0)]),
        layer("north0", vec![wedge(0.0, 1.0)]),
    ]));
    let selected = |input: &str, select: &str| {
        let r = rustgis_analysis::run("selectByLocation", &m, &serde_json::json!({"input": input, "select": select})).unwrap();
        let Output::Selection { rows, .. } = r.output else { panic!("expected a selection") };
        rows
    };
    assert_eq!((selected("south0", "south90"), selected("north0", "south90")), (vec![0], vec![]));
    let r = rustgis_analysis::run("selectByLocation", &m, &serde_json::json!({"input": "south0", "select": "south90", "invert": "Yes"}))
        .unwrap();
    let Output::Selection { rows, .. } = r.output else { panic!("expected a selection") };
    assert!(rows.is_empty(), "{rows:?}");
    let joined = |target: &str, join: &str, how: &str| {
        let mut params = serde_json::json!({"target": target, "join": join, "match": how});
        if how == "Closest" {
            params["distanceField"] = serde_json::json!("gap");
        }
        run(&m, "spatialJoin", params)
    };
    let sj = joined("south0", "south90", "Intersects");
    assert_eq!(sj.value(0, sj.field_index("join_count").unwrap()), &Value::Int(1));
    let sj = joined("north0", "south90", "Intersects");
    assert_eq!(sj.value(0, sj.field_index("join_count").unwrap()), &Value::Int(0));
    let sj = joined("south0", "south90", "Closest");
    assert_eq!(sj.value(0, sj.field_index("gap").unwrap()), &Value::Float(0.0));
}

#[test]
fn a_clipped_wedge_still_meets_others_at_the_pole() {
    // A wedge from 0°E to 90°E, clipped, still reaches the pole, where it meets one at 120°E: the
    // overlay's grid moves its pole vertices, but not out of the pole's tolerance.
    let m = Map(HashMap::from([
        layer("wedge", vec![square(0.0, -90.0, 90.0, -89.0)]),
        layer("mask", vec![square(-180.0, -90.0, 180.0, -80.0)]),
    ]));
    let clipped = run(&m, "clip", serde_json::json!({"input": "wedge", "clip": "mask"}));
    let off = latitudes(&clipped).1.iter().map(|y| (y + 90.0).abs()).fold(f64::INFINITY, f64::min);
    let m = Map(HashMap::from([("clipped".to_string(), clipped), layer("other", vec![square(120.0, -90.0, 130.0, -89.0)])]));
    let r = rustgis_analysis::run("selectByLocation", &m, &serde_json::json!({"input": "clipped", "select": "other"})).unwrap();
    let Output::Selection { rows, .. } = r.output else { panic!("expected a selection") };
    assert_eq!(rows, [0], "pole vertices {off:e}° off");
}

#[test]
fn a_spatial_join_finds_every_feature_at_the_pole_through_its_index() {
    // Twenty wedges reaching the South Pole all around it, and five squares far off: enough join
    // features (over 16) for Spatial Join to look them up in its grid, where a wedge's extent
    // meets only its neighbors'.
    let wedges = (0..20).map(|i| square(-180.0 + 18.0 * f64::from(i), -90.0, -172.0 + 18.0 * f64::from(i), -89.0));
    let far = (0..5).map(|i| square(f64::from(i), 0.0, f64::from(i) + 0.5, 0.5));
    let m = Map(HashMap::from([layer("target", vec![square(0.0, -90.0, 10.0, -89.0)]), layer("join", wedges.chain(far).collect())]));
    let sj = run(&m, "spatialJoin", serde_json::json!({"target": "target", "join": "join", "match": "Intersects"}));
    assert_eq!(sj.value(0, sj.field_index("join_count").unwrap()), &Value::Int(20));
}

#[test]
fn a_point_at_a_pole_is_found_through_each_index() {
    // The South Pole points of the test below, with twenty points and twenty squares far off: more
    // than 16 features on each side, so the tools look them up in their grids, where the pole
    // points' extents don't meet the wedge's.
    let c = Coord::new;
    let mut pts = Dataset::new("pts", GeometryType::Point, vec![Field::new("k", FieldType::Text)]);
    let polar = [c(0.0, -90.0), c(120.0, -90.0), c(0.0, -90.0 + 5e-8), c(45.0, -90.0 + 5e-8)];
    for p in polar.into_iter().chain((0..20).map(|i| c(100.0 + f64::from(i), 10.0))) {
        pts.push(Some(Geometry::Point(p)), vec![Value::Text("a".into())]);
    }
    let far = (0..20).map(|i| square(f64::from(i), 0.0, f64::from(i) + 0.5, 0.5));
    let (_, polys) = layer("polys", std::iter::once(square(0.0, -90.0, 90.0, -89.0)).chain(far).collect());
    let m = Map(HashMap::from([("pts".to_string(), pts), ("polys".to_string(), polys)]));
    for (tool, params, want) in [
        ("clip", serde_json::json!({"input": "pts", "clip": "polys"}), 4),
        ("intersect", serde_json::json!({"input": "pts", "overlay": "polys"}), 4),
        ("erase", serde_json::json!({"input": "pts", "erase": "polys"}), 20),
    ] {
        assert_eq!(run(&m, tool, params).features.len(), want, "{tool}");
    }
    let counted = run(&m, "countPoints", serde_json::json!({"polygons": "polys", "points": "pts"}));
    assert_eq!(counted.value(0, counted.field_index("point_count").unwrap()), &Value::Int(4));
    let split = rustgis_analysis::run("split", &m, &serde_json::json!({"input": "pts", "split": "polys"})).unwrap();
    let Output::Layers(layers) = split.output else { panic!("expected layers") };
    let [rustgis_analysis::OutputLayer::Vector(part)] = layers.as_slice() else { panic!("expected one layer: {layers:?}") };
    assert_eq!(part.features.len(), 4);
    let joined = run(&m, "spatialJoin", serde_json::json!({"target": "pts", "join": "polys", "match": "Have their center in"}));
    let count = joined.field_index("join_count").unwrap();
    let found: Vec<usize> = (0..24).filter(|r| joined.value(*r, count) == &Value::Int(1)).collect();
    assert_eq!(found, [0, 1, 2, 3]);
}

#[test]
fn a_point_at_a_pole_meets_what_reaches_it_and_lies_in_what_surrounds_it() {
    // In the plane a pole is a line, on the ground one place. Points at the South Pole, written at
    // 0°E and 120°E and 5e-8° off it, touch a wedge from 0°E to 90°E reaching the pole: Clip and
    // Intersect keep them and Summarize Within counts them, as they do points on an edge. Only a
    // polygon that surrounds the pole holds them inside, as Must Be Covered By counts; nothing
    // reaches the North Pole point. A point 5e-8° off the pole within the wedge, at 45°E, lies
    // inside it as anywhere else.
    let c = Coord::new;
    let mut pts = Dataset::new("pts", GeometryType::Point, vec![Field::new("k", FieldType::Text)]);
    for p in [c(0.0, -90.0), c(120.0, -90.0), c(0.0, -90.0 + 5e-8), c(0.0, 90.0), c(45.0, -90.0 + 5e-8)] {
        pts.push(Some(Geometry::Point(p)), vec![Value::Text("a".into())]);
    }
    let (_, wedge) = layer("wedge", vec![square(0.0, -90.0, 90.0, -89.0)]);
    let m = Map(HashMap::from([("pts".to_string(), pts.clone()), ("wedge".to_string(), wedge.clone())]));
    for (tool, params) in [
        ("clip", serde_json::json!({"input": "pts", "clip": "wedge"})),
        ("intersect", serde_json::json!({"input": "pts", "overlay": "wedge"})),
    ] {
        assert_eq!(latitudes(&run(&m, tool, params)).0, 4, "{tool}");
    }
    let counted = run(&m, "countPoints", serde_json::json!({"polygons": "wedge", "points": "pts"}));
    assert_eq!(counted.value(0, counted.field_index("point_count").unwrap()), &Value::Int(4));
    // Erase takes them away, leaving the North Pole point; Split puts them in the wedge's layer;
    // Spatial Join finds each in the wedge when it has its center there.
    let erased = run(&m, "erase", serde_json::json!({"input": "pts", "erase": "wedge"}));
    assert_eq!(latitudes(&erased), (1, vec![90.0]));
    let split = rustgis_analysis::run("split", &m, &serde_json::json!({"input": "pts", "split": "wedge"})).unwrap();
    let Output::Layers(layers) = split.output else { panic!("expected layers") };
    let [rustgis_analysis::OutputLayer::Vector(part)] = layers.as_slice() else { panic!("expected one layer: {layers:?}") };
    assert_eq!(part.features.len(), 4);
    let joined = run(&m, "spatialJoin", serde_json::json!({"target": "pts", "join": "wedge", "match": "Have their center in"}));
    let count = joined.field_index("join_count").unwrap();
    let counts: Vec<&Value> = (0..5).map(|r| joined.value(r, count)).collect();
    assert_eq!(counts, [&Value::Int(1), &Value::Int(1), &Value::Int(1), &Value::Int(0), &Value::Int(1)]);
    let uncovered = |by: &Dataset| -> Vec<usize> {
        rustgis_analysis::topology::not_covered(&pts, &[0, 1, 2, 3, 4], by).errors.iter().flat_map(|e| e.rows.clone()).collect()
    };
    assert_eq!(uncovered(&wedge), [0, 1, 2, 3]);
    // Surrounding the pole: a cap written from −180° to 180° or from 0° to 360°, one in two halves
    // (meeting exactly, or 1e-9° apart, as an overlay's rounding can leave them), one in parts
    // written in both ways, one with an island far west; not one leaving a gap of 10° or of 1° (its
    // pole points lie on its edge), nor one with a hole reaching the pole (the point beside the
    // pole, at 0°, lies inside it, as anywhere else), even one narrowing to a corner there, under a
    // meter wide in the plane a meter off the pole, unless another polygon fills it; nor halves
    // with a gap narrowing to the pole between them (along a meridian or slanting, 0.5° wide at
    // 85°S, millimeters a meter off the pole in the plane), or one with a notch narrowing to it;
    // nor one whose longitudes can't be taken (one infinite, or so far around that a meter is lost
    // in rounding). A cap 110 m across holds it, in halves a seam apart too; a hole near the pole
    // that doesn't reach it is reported as if it did, as the check leans to reporting.
    let ring = |w: f64, e: f64| vec![c(w, -85.0), c(e, -85.0), c(e, -90.0), c(w, -90.0), c(w, -85.0)];
    let island = vec![c(-100.0, 0.0), c(-95.0, 0.0), c(-95.0, 5.0), c(-100.0, 5.0), c(-100.0, 0.0)];
    let hole = vec![c(-60.0, -90.0), c(-50.0, -90.0), c(-50.0, -89.0), c(-60.0, -89.0), c(-60.0, -90.0)];
    let narrowing = vec![c(-60.0, -89.0), c(-50.0, -89.0), c(-55.0, -90.0), c(-60.0, -89.0)];
    let tapering = vec![c(0.0, -85.0), c(180.0, -85.0), c(180.0, -90.0), c(1.0, -90.0), c(0.0, -89.0), c(0.0, -85.0)];
    let notched = vec![
        c(-180.0, -85.0),
        c(180.0, -85.0),
        c(180.0, -90.0),
        c(2.0, -90.0),
        c(0.0, -89.5),
        c(-2.0, -90.0),
        c(-180.0, -90.0),
        c(-180.0, -85.0),
    ];
    let infinite = vec![c(-180.0, -85.0), c(f64::INFINITY, -85.0), c(180.0, -90.0), c(-180.0, -90.0), c(-180.0, -85.0)];
    let half_wedge = vec![c(0.5, -85.0), c(180.0, -85.0), c(180.0, -90.0), c(0.0, -90.0), c(0.5, -85.0)];
    let near = vec![c(10.0, -89.999), c(20.0, -89.999), c(20.0, -89.99), c(10.0, -89.99), c(10.0, -89.999)];
    let small = vec![c(-180.0, -89.9995), c(180.0, -89.9995), c(180.0, -90.0), c(-180.0, -90.0), c(-180.0, -89.9995)];
    let slanted_west = vec![c(-180.0, -85.0), c(10.0, -85.0), c(0.0, -90.0), c(-180.0, -90.0), c(-180.0, -85.0)];
    let slanted_east = vec![c(10.5, -85.0), c(180.0, -85.0), c(180.0, -90.0), c(0.0, -90.0), c(10.5, -85.0)];
    let small_west = vec![c(-180.0, -89.9995), c(0.0, -89.9995), c(0.0, -90.0), c(-180.0, -90.0), c(-180.0, -89.9995)];
    let small_east = vec![c(1e-9, -89.9995), c(180.0, -89.9995), c(180.0, -90.0), c(1e-9, -90.0), c(1e-9, -89.9995)];
    let notch_outline = vec![
        c(0.0, -85.0),
        c(360.0, -85.0),
        c(360.0, -90.0),
        c(200.0, -90.0),
        c(200.0, -86.0),
        c(100.0, -86.0),
        c(100.0, -90.0),
        c(0.0, -90.0),
        c(0.0, -85.0),
    ];
    let notch_hole = vec![c(100.0, -90.0), c(200.0, -90.0), c(200.0, -86.5), c(100.0, -86.5), c(100.0, -90.0)];
    let short_of = vec![c(-180.0, -85.0), c(180.0, -85.0), c(180.0, -89.9999995), c(-180.0, -89.9999995), c(-180.0, -85.0)];
    let nearly = vec![
        c(-180.0, -85.0),
        c(180.0, -85.0),
        c(180.0, -90.0),
        c(2.0, -90.0),
        c(0.0, -89.9999996),
        c(-2.0, -90.0),
        c(-180.0, -90.0),
        c(-180.0, -85.0),
    ];
    let one = |rings: Vec<Vec<Vec<Coord>>>| vec![Geometry::Polygon(rings)];
    let each = |rings: Vec<Vec<Vec<Coord>>>| rings.into_iter().map(|r| Geometry::Polygon(vec![r])).collect::<Vec<_>>();
    for (name, features, want) in [
        ("cap", one(vec![vec![ring(-180.0, 180.0)]]), vec![3]),
        ("cap from 0°", one(vec![vec![ring(0.0, 360.0)]]), vec![3]),
        ("halves", one(vec![vec![ring(-180.0, 0.0)], vec![ring(0.0, 180.0)]]), vec![3]),
        ("halves a seam apart", one(vec![vec![ring(-180.0, 0.0)], vec![ring(1e-9, 180.0)]]), vec![3]),
        ("halves overlapping", one(vec![vec![ring(-180.0, 10.0)], vec![ring(-10.0, 180.0)]]), vec![3]),
        ("halves as two features", each(vec![vec![ring(-180.0, 0.0)], vec![ring(0.0, 180.0)]]), vec![3]),
        ("parts both ways", one(vec![vec![ring(-180.0, -10.0)], vec![ring(0.0, 180.0)], vec![ring(350.0, 360.0)]]), vec![3]),
        ("island", one(vec![vec![ring(0.0, 360.0)], vec![island.clone()]]), vec![3]),
        ("gap", one(vec![vec![ring(0.0, 350.0)]]), vec![0, 1, 2, 3]),
        ("halves a degree apart", each(vec![vec![ring(-180.0, 0.0)], vec![ring(1.0, 180.0)]]), vec![0, 1, 2, 3]),
        ("hole", one(vec![vec![ring(-180.0, 180.0), hole.clone()]]), vec![0, 1, 3]),
        ("hole narrowing to the pole", one(vec![vec![ring(-180.0, 180.0), narrowing.clone()]]), vec![0, 1, 3]),
        (
            "that hole filled",
            vec![Geometry::Polygon(vec![vec![ring(-180.0, 180.0), narrowing.clone()]]), Geometry::Polygon(vec![vec![narrowing.clone()]])],
            vec![3],
        ),
        ("halves a gap narrowing to the pole apart", each(vec![vec![ring(-180.0, 0.0)], vec![tapering.clone()]]), vec![0, 1, 2, 3]),
        ("a notch to the pole", one(vec![vec![notched.clone()]]), vec![0, 1, 2, 3]),
        ("halves a 0.5° wedge apart", each(vec![vec![ring(-180.0, 0.0)], vec![half_wedge.clone()]]), vec![0, 1, 2, 3]),
        ("a hole near the pole, not reaching it", one(vec![vec![ring(-180.0, 180.0), near.clone()]]), vec![0, 1, 3]),
        ("halves a slanted 0.5° wedge apart", each(vec![vec![slanted_west.clone()], vec![slanted_east.clone()]]), vec![0, 1, 3]),
        ("a small cap in halves a seam apart", each(vec![vec![small_west.clone()], vec![small_east.clone()]]), vec![3]),
        ("a cap stopping 5e-7° short of the pole", one(vec![vec![short_of.clone()]]), vec![0, 1, 2, 3, 4]),
        ("a notch ending 4e-7° short of the pole", one(vec![vec![nearly.clone()]]), vec![0, 1, 2, 3]),
        ("a hole beside its outline, in its gap", one(vec![vec![ring(0.0, 350.0), ring(350.0, 360.0)]]), vec![0, 1, 2, 3]),
        ("a hole filling a notch in its outline", one(vec![vec![notch_outline.clone(), notch_hole.clone()]]), vec![0, 1, 2, 3]),
        ("a cap 110 m across", one(vec![vec![small.clone()]]), vec![3]),
        ("a longitude infinite", one(vec![vec![infinite.clone()]]), vec![0, 1, 3]),
        ("a turn written 1e15° away", one(vec![vec![ring(1e15, 1e15 + 360.0)]]), vec![0, 1, 2, 3, 4]),
    ] {
        assert_eq!(uncovered(&layer("cap", features).1), want, "{name}");
    }
}

#[test]
fn radius_search_reports_invalid_input_without_clamping() {
    let c = Coord::new;
    let mut ds = Dataset::new("bad", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(c(0.0, 100.0))), vec![]);
    ds.push(Some(Geometry::Line(vec![vec![c(0.0, 89.0), c(0.5, 89.0), c(f64::NAN, 89.0), c(1.0, 89.0)]])), vec![]);
    ds.push(Some(Geometry::Point(c(0.0, 89.0))), vec![]);
    let report = rustgis_analysis::network::within_report(&ds, &[0, 1, 2], c(0.0, 90.0), 200_000.0);
    assert_eq!(report.invalid_latitudes, 1);
    assert_eq!(report.missing_coordinates, 1);
    assert_eq!(report.rows.len(), 2);
    assert!(!report.rows.iter().any(|(r, _)| *r == 0));
    let clean = rustgis_analysis::network::within_report(&ds, &[2], c(0.0, 90.0), 200_000.0);
    assert_eq!((clean.invalid_latitudes, clean.missing_coordinates), (0, 0));
}

#[test]
fn radius_search_measures_incomplete_polygons_only_along_finite_edges() {
    let c = Coord::new;
    for (latitude, radius) in [(45.0, 100_000.0), (89.0, 10_000.0)] {
        let (a, b, d) = (c(-1.0, latitude - 1.0), c(1.0, latitude - 1.0), c(-1.0, latitude + 1.0));
        let at = c(0.0, latitude);
        let mut edges = Dataset::new("edges", GeometryType::Polyline, vec![]);
        edges.push(Some(Geometry::Line(vec![vec![a, b], vec![d, a]])), vec![]);
        let expected = rustgis_analysis::network::within_report(&edges, &[0], at, radius);
        assert_eq!(expected.rows.len(), 1);
        assert!(expected.rows[0].1 > 1_000.0);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let (_, ds) = layer("broken", vec![Geometry::Polygon(vec![vec![vec![a, b, c(1.0, bad), d, a]]])]);
            let report = rustgis_analysis::network::within_report(&ds, &[0], at, radius);
            assert_eq!((report.invalid_latitudes, report.missing_coordinates, report.bounded), (0, 1, 0));
            assert_eq!(report.rows.len(), 1, "{latitude} {bad}");
            assert!((report.rows[0].1 - expected.rows[0].1).abs() < 1e-6, "{latitude} {bad}: {:?}", report.rows);
        }
        let (_, clean) = layer("clean", vec![Geometry::Polygon(vec![vec![vec![a, b, c(1.0, latitude + 1.0), d, a]]])]);
        let report = rustgis_analysis::network::within_report(&clean, &[0], at, radius);
        assert_eq!((report.rows, report.missing_coordinates), (vec![(0, 0.0)], 0));
    }
}

#[test]
fn pole_tools_refuse_latitudes_outside_the_earth() {
    let cases = [
        ("clip", serde_json::json!({"input":"a", "clip":"b"})),
        ("CLIP", serde_json::json!({"input":"a", "clip":"b"})),
        ("erase", serde_json::json!({"input":"a", "erase":"b"})),
        ("intersect", serde_json::json!({"input":"a", "overlay":"b"})),
        ("identity", serde_json::json!({"input":"a", "overlay":"b"})),
        ("union", serde_json::json!({"input":"a", "overlay":"b"})),
        ("symmetricalDifference", serde_json::json!({"input":"a", "overlay":"b"})),
        ("update", serde_json::json!({"input":"a", "update":"b"})),
        ("split", serde_json::json!({"input":"a", "split":"b"})),
        ("countPoints", serde_json::json!({"points":"a", "polygons":"b"})),
        ("spatialJoin", serde_json::json!({"target":"a", "join":"b"})),
        ("selectByLocation", serde_json::json!({"input":"a", "select":"b"})),
        ("Select Layer By Location", serde_json::json!({"input":"a", "select":"b"})),
    ];
    for (id, params) in cases {
        for bad in ["a", "b"] {
            for latitude in [95.0, f64::INFINITY, f64::NEG_INFINITY] {
                let map = Map(HashMap::from([
                    layer("a", vec![square(0.0, 88.0, 1.0, if bad == "a" { latitude } else { 89.0 })]),
                    layer("b", vec![square(0.0, 88.0, 1.0, if bad == "b" { latitude } else { 89.0 })]),
                ]));
                let error = rustgis_analysis::run(id, &map, &params).unwrap_err();
                assert!(error.contains("latitude past ±90°"), "{id} {bad}: {error}");
                let resolved = rustgis_analysis::resolve_published(id).unwrap();
                let error =
                    rustgis_analysis::run_resolved(&resolved, &map, &params, &rustgis_analysis::control::Control::new()).unwrap_err();
                assert!(error.contains("latitude past ±90°"), "resolved {id} {bad}: {error}");
            }
        }
    }
}

#[test]
fn polar_dangles_match_parts_stored_many_turns_apart() {
    let c = Coord::new;
    for turn in [720.0, -1080.0] {
        let mut ds = Dataset::new("turns", GeometryType::Polyline, vec![]);
        ds.push(Some(Geometry::Line(vec![vec![c(0.0, 88.0), c(0.0, 89.0)]])), vec![]);
        ds.push(Some(Geometry::Line(vec![vec![c(turn, 89.0), c(turn, 89.5)]])), vec![]);
        let checked = rustgis_analysis::topology::dangles(&ds, &[0, 1], 1.0);
        assert!(checked.complete);
        assert_eq!(checked.errors.len(), 2, "turn {turn}");
    }
}

#[test]
fn summarize_within_validates_unselected_polygons_it_also_reads() {
    struct Selected<'a>(&'a Map);
    impl Inputs for Selected<'_> {
        fn dataset(&self, name: &str) -> Option<&Dataset> {
            self.0.dataset(name)
        }
        fn selection(&self, name: &str) -> &[usize] {
            if name == "zones" { &[0] } else { &[] }
        }
    }
    for invalid in [false, true] {
        let mut points = Dataset::new("points", GeometryType::Point, vec![]);
        points.push(Some(Geometry::Point(Coord::new(0.5, 89.0))), vec![]);
        let map = Map(HashMap::from([
            ("points".into(), points),
            layer("zones", vec![square(0.0, 88.0, 1.0, 89.0), square(0.0, 89.0, 1.0, if invalid { 95.0 } else { 90.0 })]),
        ]));
        let result = rustgis_analysis::run("countPoints", &Selected(&map), &serde_json::json!({"points":"points", "polygons":"zones"}));
        if invalid {
            let error = result.unwrap_err();
            assert!(error.contains("zones") && error.contains("1 feature(s)") && error.contains("latitude past ±90°"), "{error}");
        } else {
            assert!(result.is_ok(), "{result:?}");
        }
    }
}
