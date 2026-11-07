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
