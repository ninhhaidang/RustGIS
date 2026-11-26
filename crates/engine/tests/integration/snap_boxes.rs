//! Snapping, which reads only the shapes whose kept box is near, finds what a search of every
//! shape finds, as a property: over two generated layers, with one feature left out, and after
//! shapes are moved, added and removed.
//!
//! Pull requests run a fixed seed; the nightly a new one with more cases. A failure gives its seed
//! and the smallest layers and places found. To run a seed again, or deeper:
//! `PROPERTY_SEED=7 PROPERTY_CASES=2000 cargo test -p rustgis-engine --test integration snap_boxes::`.
use proptest::prelude::*;
use proptest::sample::Index;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use rustgis_engine::{
    Project, Session,
    cmd::design::snap_except,
    data::{Dataset, Geometry, GeometryType},
    geom::{Coord, merc_to_lonlat},
    map::design::Snapping,
};

const TOLERANCE: f64 = 10.0;

/// A number from the environment, else `default`; one set that isn't a whole number fails, so a
/// typo doesn't run the defaults without a word (#2639).
fn env(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(v) => v.trim().parse().unwrap_or_else(|_| panic!("{name}={v:?} isn't a whole number")),
        Err(std::env::VarError::NotPresent) => default,
        Err(e) => panic!("{name}: {e}"),
    }
}

/// Metre-sized fixtures in the longitude/latitude coordinates of feature layers, a few hundred
/// metres from a corner far from the map's origin.
fn c((x, y): (f64, f64)) -> Coord {
    merc_to_lonlat(Coord::new(x - 5e6, y - 2e6))
}

/// A shape of any kind, or none, a few tens of metres across, somewhere in 400 m square.
fn shape() -> BoxedStrategy<Option<Geometry>> {
    let path = |at: (f64, f64), n: std::ops::Range<usize>| {
        proptest::collection::vec((0.0..60.0f64, 0.0..60.0f64), n)
            .prop_map(move |v| v.into_iter().map(|(dx, dy)| c((at.0 + dx, at.1 + dy))).collect::<Vec<_>>())
    };
    let square =
        |(x, y): (f64, f64), r: f64| vec![c((x - r, y - r)), c((x + r, y - r)), c((x + r, y + r)), c((x - r, y + r)), c((x - r, y - r))];
    (0.0..400.0f64, 0.0..400.0f64)
        .prop_flat_map(move |at| {
            prop_oneof![
                Just(Some(Geometry::Point(c(at)))),
                path(at, 1..4).prop_map(|p| Some(Geometry::MultiPoint(p))),
                // A part of one place too, whose end is its vertex.
                (path(at, 2..5), path(at, 1..3)).prop_map(|(a, b)| Some(Geometry::Line(vec![a, b]))),
                (1.0..40.0f64).prop_map(move |r| Some(Geometry::Polygon(vec![vec![square(at, r), square(at, r / 2.0)]]))),
                (path(at, 3..6), path(at, 3..5)).prop_map(|(a, b)| Some(Geometry::Polygon(vec![vec![a], vec![b]]))),
                Just(None),
            ]
        })
        .boxed()
}

fn dataset(name: &str, shapes: Vec<Option<Geometry>>) -> Dataset {
    let mut d = Dataset::new(name, GeometryType::Polygon, vec![]);
    shapes.into_iter().for_each(|g| d.push(g, vec![]));
    d
}

/// The kind and the map place a search of every shape but `skip` finds nearest to `m`: a vertex or
/// a line's end before an edge, the first of those as near.
fn nearest_of_every_shape(s: &mut Session, m: Coord, skip: Option<(u32, usize)>) -> Option<(Coord, &'static str)> {
    let (mut point, mut edge): (Option<(f64, Coord, &str)>, Option<(f64, Coord, &str)>) = (None, None);
    let mut consider = |at: Coord, kind: &'static str| {
        let d = (at.x - m.x).powi(2) + (at.y - m.y).powi(2);
        let slot = if kind == "edge" { &mut edge } else { &mut point };
        if d <= TOLERANCE * TOLERANCE && slot.is_none_or(|b| d < b.0) {
            *slot = Some((d, at, kind));
        }
    };
    for l in &mut s.map_mut().layers {
        let id = l.id;
        for (_, g) in l.projected().iter().enumerate().filter(|(row, _)| skip != Some((id, *row))) {
            let (points, paths): (&[Coord], Vec<&Vec<Coord>>) = match g {
                Some(Geometry::Point(p)) => (std::slice::from_ref(p), vec![]),
                Some(Geometry::MultiPoint(p)) => (p, vec![]),
                Some(Geometry::Line(parts)) => (&[], parts.iter().collect()),
                Some(Geometry::Polygon(polys)) => (&[], polys.iter().flatten().collect()),
                None => continue,
            };
            points.iter().for_each(|p| consider(*p, "vertex"));
            for path in paths {
                if matches!(g, Some(Geometry::Line(_))) {
                    [path.first(), path.last()].into_iter().flatten().for_each(|p| consider(*p, "end"));
                }
                path.iter().for_each(|p| consider(*p, "vertex"));
                for w in path.windows(2) {
                    let (dx, dy) = (w[1].x - w[0].x, w[1].y - w[0].y);
                    let len2 = dx * dx + dy * dy;
                    let t = if len2 > 0.0 { (((m.x - w[0].x) * dx + (m.y - w[0].y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                    consider(Coord::new(w[0].x + t * dx, w[0].y + t * dy), "edge");
                }
            }
        }
    }
    point.or(edge).map(|(_, at, kind)| (at, kind))
}

/// Snapping at each place finds what the search of every shape finds.
fn check(s: &mut Session, places: &[(f64, f64)], skip: Option<(u32, usize)>) -> Result<(), TestCaseError> {
    for at in places.iter().map(|p| c(*p)) {
        let m = s.map().to_map(at).unwrap();
        let expected = nearest_of_every_shape(s, m, skip);
        let got = snap_except(s, at, skip);
        prop_assert_eq!(got.map(|g| g.1), expected.map(|e| e.1), "at {:?}, leaving out {:?}", at, skip);
        if let (Some((got, _)), Some((expected, _))) = (got, expected) {
            let got = s.map().to_map(got).unwrap();
            prop_assert!((got.x - expected.x).abs() < 1e-6 && (got.y - expected.y).abs() < 1e-6, "{:?} != {:?}", got, expected);
        }
    }
    Ok(())
}

/// A line far from the generated shapes, which every case snaps to by its end, its other vertex
/// and its edge, and beside which it finds nothing: each kind is checked in every case.
const FIXED: [(f64, f64); 3] = [(1000.0, 0.0), (1100.0, 0.0), (1100.0, 100.0)];

fn check_fixed(s: &mut Session) -> Result<(), TestCaseError> {
    for (at, expected) in [
        ((1003.0, 2.0), Some(((1000.0, 0.0), "end"))),
        ((1102.0, 3.0), Some(((1100.0, 0.0), "vertex"))),
        ((1050.0, 4.0), Some(((1050.0, 0.0), "edge"))),
        ((1050.0, 50.0), None),
    ] {
        let got = snap_except(s, c(at), None).map(|(g, kind)| (s.map().to_map(g).unwrap(), kind));
        prop_assert_eq!(got.map(|g| g.1), expected.map(|e| e.1), "at {:?}", at);
        if let (Some((got, _)), Some(((x, y), _))) = (got, expected) {
            prop_assert!((got.x - (x - 5e6)).abs() < 1e-6 && (got.y - (y - 2e6)).abs() < 1e-6, "{:?} at {:?}", got, at);
        }
    }
    Ok(())
}

#[test]
fn snapping_finds_what_a_search_of_every_shape_finds() {
    let seed = env("PROPERTY_SEED", 1);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    let cases = env("PROPERTY_CASES", 256);
    let cases = u32::try_from(cases).unwrap_or_else(|_| panic!("PROPERTY_CASES={cases} is past {}", u32::MAX));
    let config = Config { cases, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes));
    let layer = || proptest::collection::vec(shape(), 1..40);
    let places = proptest::collection::vec((-50.0..500.0f64, -50.0..500.0f64), 1..40);
    let skip = proptest::option::of((any::<bool>(), any::<Index>()));
    let edits = proptest::collection::vec((any::<Index>(), shape()), 0..4);
    let result = runner.run(&(layer(), layer(), places, skip, edits), |(a, mut b, places, skip, edits)| {
        let (na, nb) = (a.len(), b.len());
        b.push(Some(Geometry::Line(vec![FIXED.iter().map(|p| c(*p)).collect()])));
        let mut s = Session::new(Project::new("Snap boxes"));
        s.map_mut().layers.clear();
        let ia = s.map_mut().add_dataset(dataset("A", a));
        let ib = s.map_mut().add_dataset(dataset("B", b));
        s.map_mut().camera.res = 1.0;
        s.map_mut().editing.snapping = Snapping { enabled: true, tolerance_px: TOLERANCE, vertex: true, edge: true, end: true };
        check_fixed(&mut s)?;
        check(&mut s, &places, None)?;
        // The fixed line is never the one left out.
        let skip = skip.map(|(first, i)| if first { (ia, i.index(na)) } else { (ib, i.index(nb)) });
        check(&mut s, &places, skip)?;
        // Shapes moved, added and removed: their boxes are those of the new ones.
        let l = s.map_mut().layer_mut(ia).unwrap();
        for (i, g) in edits {
            l.dataset.features.set_geometry(i.index(na), g);
        }
        let far = c((900.0, 0.0));
        l.dataset.features.set_geometry(0, Some(Geometry::Point(far)));
        l.touch_geometry();
        prop_assert_eq!(snap_except(&mut s, c((905.0, 0.0)), None), Some((far, "vertex")));
        check(&mut s, &places, None)
    });
    if let Err(e) = result {
        panic!("PROPERTY_SEED={seed}: {e}");
    }
}
