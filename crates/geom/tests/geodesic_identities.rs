//! Geodesic measurement, as properties. A distance is the same both ways, zero only between two
//! names of one place (±180°, a pole at any longitude), and never longer than a detour through a
//! third point. A ring's perimeter and area don't depend on the vertex it starts from, its
//! direction, a closing vertex or a vertex written twice. Points lean on the poles and the
//! antimeridian; rings include ones across the antimeridian, around a pole, globe-wide bands, caps
//! closed along the pole and pieces of a polygon split at ±180°.
//!
//! Pull requests run a fixed seed; the nightly a new one with more cases. A failure gives its seed
//! and the smallest input found. To run a seed again, or deeper:
//! `PROPERTY_SEED=7 PROPERTY_CASES=50000 cargo test -p rustgis-geom --test geodesic_identities`.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use rustgis_geom::{Coord, geodesic_ring_area_m2, geodesic_ring_perimeter_m, haversine_m, wgs84_distance_m, wgs84_ring_perimeter_area_m2};

/// A number from the environment, else `default`; one set that isn't a whole number (or isn't
/// text) fails, so a typo doesn't run the defaults without a word (#2639).
fn env(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(v) => v.trim().parse().unwrap_or_else(|_| panic!("{name}={v:?} isn't a whole number")),
        Err(std::env::VarError::NotPresent) => default,
        Err(e) => panic!("{name}: {e}"),
    }
}

/// A runner seeded from `PROPERTY_SEED`, running `PROPERTY_CASES` cases (else `default`).
fn runner(default: u32) -> (u64, TestRunner) {
    let seed = env("PROPERTY_SEED", 1);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    let cases = env("PROPERTY_CASES", default.into());
    let cases = u32::try_from(cases).unwrap_or_else(|_| panic!("PROPERTY_CASES={cases} is past {}", u32::MAX));
    let config = Config { cases, failure_persistence: None, ..Config::default() };
    (seed, TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes)))
}

/// A longitude/latitude, often on an edge: ±180°, the poles, the equator.
fn coord() -> impl Strategy<Value = Coord> {
    let lon = prop_oneof![3 => -180.0..=180.0f64, 1 => prop::sample::select(vec![-180.0, 180.0, 0.0, 90.0, 179.999_999])];
    let lat = prop_oneof![3 => -90.0..=90.0f64, 1 => prop::sample::select(vec![-90.0, 90.0, 0.0, 89.999_999, -45.0])];
    (lon, lat).prop_map(|(x, y)| Coord::new(x, y))
}

/// How far (metres) a distance may be from each identity, as (measure, a part of the distance,
/// why): what its formula can't hold, not a mistake in it.
const ROUNDING: &[(&str, f64, &str)] = &[
    ("WGS 84", 1e-12, "GeographicLib solves to a few nanometres"),
    // h = sin²(Δφ/2) + … is near 1 there, and asin(√h) has lost half of h's digits: about
    // R·√ε, a decimetre in 20,000 km.
    ("sphere", 2e-8, "the haversine formula loses half its digits between points nearly opposite"),
];

/// Whether two coordinates name one place: the same latitude, and the same longitude but for
/// whole turns, or any longitude at a pole.
fn same_place(a: Coord, b: Coord) -> bool {
    a.y == b.y && (a.y.abs() == 90.0 || (a.x - b.x).rem_euclid(360.0) == 0.0)
}

#[test]
fn a_distance_is_symmetric_zero_only_at_one_place_and_no_longer_than_a_detour() {
    let (seed, mut runner) = runner(20_000);
    let result = runner.run(&(coord(), coord(), coord()), |(a, b, c)| {
        for ((name, part, _), d) in ROUNDING.iter().zip([wgs84_distance_m as fn(Coord, Coord) -> f64, haversine_m]) {
            let (ab, ba, ac, bc) = (d(a, b), d(b, a), d(a, c), d(b, c));
            // NaN fails no comparison below, so a distance that isn't a number fails here.
            if ![ab, ba, ac, bc].iter().all(|d| d.is_finite()) {
                return Err(TestCaseError::fail(format!("{name}: {ab}, {ba}, {ac}, {bc} m among {a:?}, {b:?}, {c:?}")));
            }
            let tolerance = 1e-6 + part * ab.max(ac).max(bc);
            if (ab - ba).abs() > tolerance {
                return Err(TestCaseError::fail(format!("{name}: {ab} m from {a:?} to {b:?}, {ba} m back")));
            }
            // cos 90° isn't quite 0 in floating point: two longitudes at a pole are a nanometre apart.
            if d(a, a) != 0.0 || if same_place(a, b) { ab > 1e-6 } else { ab <= 0.0 } {
                return Err(TestCaseError::fail(format!("{name}: {ab} m from {a:?} to {b:?}, the same place: {}", same_place(a, b))));
            }
            if ac > ab + bc + tolerance {
                return Err(TestCaseError::fail(format!("{name}: {ac} m from {a:?} to {c:?}, {} m through {b:?}", ab + bc)));
            }
        }
        Ok(())
    });
    if let Err(e) = result {
        panic!("PROPERTY_SEED={seed}: {e}");
    }
}

/// A longitude folded into −180..180, as a file writes it.
fn fold(lon: f64) -> f64 {
    (lon + 180.0).rem_euclid(360.0) - 180.0
}

/// Three to seven vertices around a centre, in order of angle, folded at the antimeridian: small
/// and large rings, rings across ±180° and, centred near a pole, rings around it.
fn star() -> impl Strategy<Value = Vec<Coord>> {
    let centre = prop_oneof![3 => (-180.0..180.0f64, -80.0..80.0f64), 1 => (prop::sample::select(vec![179.9, -180.0, 0.0]), prop::sample::select(vec![89.0, -89.5, 0.0, 60.0]))];
    let spokes = prop::collection::vec((0.0..1.0f64, 0.2..1.0f64), 3..8);
    (centre, prop_oneof![1e-4..0.01f64, 0.5..20.0f64, 20.0..60.0f64], spokes).prop_map(|((x, y), r, mut spokes)| {
        spokes.sort_by(|a, b| a.0.total_cmp(&b.0));
        spokes
            .iter()
            .map(|(a, k)| {
                let a = a * std::f64::consts::TAU;
                Coord::new(fold(x + r * k * a.cos()), (y + r * k * a.sin()).clamp(-90.0, 90.0))
            })
            .collect()
    })
}

/// A globe-wide band between two parallels: down the −180° meridian, round the parallel and up
/// the 180° meridian, with vertices sometimes part way along the meridians.
fn band() -> impl Strategy<Value = Vec<Coord>> {
    (-89.0..0.0f64, 0.5..89.0f64, any::<bool>()).prop_map(|(s, n, mid)| {
        let m = (s + n) / 2.0;
        let west: Vec<f64> = if mid { vec![n, m, s] } else { vec![n, s] };
        let mut ring: Vec<Coord> = west.iter().map(|&y| Coord::new(-180.0, y)).collect();
        ring.extend(west.iter().rev().map(|&y| Coord::new(180.0, y)));
        ring
    })
}

/// A cap around a pole closed along it: a coastline from −180° to 180°, then the pole.
fn cap() -> impl Strategy<Value = Vec<Coord>> {
    (prop::collection::vec(-80.0..80.0f64, 2..6), any::<bool>()).prop_map(|(lats, north)| {
        let n = lats.len() - 1;
        let mut ring: Vec<Coord> = lats.iter().enumerate().map(|(i, &y)| Coord::new(-180.0 + 360.0 * i as f64 / n as f64, y)).collect();
        let pole = if north { 90.0 } else { -90.0 };
        ring.extend([Coord::new(180.0, pole), Coord::new(-180.0, pole)]);
        if !north {
            ring.reverse();
        }
        ring
    })
}

/// A piece of a polygon split at the antimeridian, along ±180°, or a cell across it written with
/// vertices on both 180° and −180°.
fn seam() -> impl Strategy<Value = Vec<Coord>> {
    (-80.0..0.0f64, 0.1..80.0f64, 0.01..20.0f64, any::<bool>(), any::<bool>()).prop_map(|(s, n, w, east, cell)| {
        if cell {
            let c = |x: f64, y| Coord::new(x, y);
            vec![c(180.0 - w, s), c(180.0, s), c(-180.0, s), c(-180.0 + w, s), c(-180.0 + w, n), c(-180.0, n), c(180.0, n), c(180.0 - w, n)]
        } else {
            let (e, x) = if east { (180.0, 180.0 - w) } else { (-180.0, -180.0 + w) };
            vec![Coord::new(e, s), Coord::new(x, s), Coord::new(x, n), Coord::new(e, n)]
        }
    })
}

/// A ring, without its closing vertex.
fn ring() -> impl Strategy<Value = Vec<Coord>> {
    prop_oneof![4 => star(), 1 => band(), 1 => cap(), 1 => seam()]
}

/// The perimeter and area of a ring on the ellipsoid and on the sphere.
fn measures(ring: &[Coord]) -> [(&'static str, f64); 4] {
    let (p, a) = wgs84_ring_perimeter_area_m2(ring);
    [
        ("WGS 84 perimeter", p),
        ("WGS 84 area", a),
        ("sphere perimeter", geodesic_ring_perimeter_m(ring)),
        ("sphere area", geodesic_ring_area_m2(ring)),
    ]
}

/// Whether two measures agree: finite and equal to a part in 10⁹ and a square metre.
fn agree(a: f64, b: f64) -> bool {
    a.is_finite() && b.is_finite() && (a - b).abs() <= 1e-9 * a.abs().max(b.abs()) + 1.0
}

#[test]
fn a_ring_measures_the_same_from_any_vertex_either_way_round() {
    let (seed, mut runner) = runner(10_000);
    let result = runner.run(&(ring(), any::<prop::sample::Index>()), |(ring, twice)| {
        let want = measures(&ring);
        // The rings made here are never the ambiguous ones (through both poles) measured as NaN.
        if let Some((name, m)) = want.iter().find(|(_, m)| !m.is_finite()) {
            return Err(TestCaseError::fail(format!("{name} of {ring:?} is {m}")));
        }
        let n = ring.len();
        let mut variants: Vec<(String, Vec<Coord>)> =
            (1..n).map(|k| (format!("starting at vertex {k}"), [&ring[k..], &ring[..k]].concat())).collect();
        variants.push(("reversed".into(), ring.iter().rev().copied().collect()));
        variants.push(("closed".into(), [&ring[..], &ring[..1]].concat()));
        let i = twice.index(n);
        variants.push((format!("with vertex {i} twice"), [&ring[..=i], &ring[i..]].concat()));
        for (how, other) in variants {
            for ((name, a), (_, b)) in want.iter().zip(measures(&other)) {
                if !agree(*a, b) {
                    return Err(TestCaseError::fail(format!("{name} of {ring:?}: {a}, {how} {b}")));
                }
            }
        }
        Ok(())
    });
    if let Err(e) = result {
        panic!("PROPERTY_SEED={seed}: {e}");
    }
}
