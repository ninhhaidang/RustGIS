//! Reprojection round trips, as a property: a longitude/latitude taken into a coordinate system
//! with `Transformer::from_lonlat` and back with `to_lonlat` is the place it was, within a
//! centimetre on the ground. Inside a system's area of use a place must go there and back; outside
//! it the system may refuse it (`None`), but a place it does take must come back, never somewhere
//! else (`from_lonlat` refuses a place that doesn't, #1925). What a system does instead is listed
//! in [`LIMITS`] with the reason. Places lean on the areas' edges and corners, the poles and the
//! antimeridian.
//!
//! Pull requests run a fixed seed; the nightly a new one with more cases. A failure gives its seed
//! and the place found that doesn't come back. To run a seed again, or deeper:
//! `PROPERTY_SEED=7 PROPERTY_CASES=200000 cargo test -p rustgis-data --test reprojection_round_trip`.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use rustgis_data::raster::{Crs, Transformer};
use rustgis_geom::{Coord, wgs84_distance_m};
use std::sync::atomic::{AtomicUsize, Ordering};

/// How far (metres on the ground) a place may come back from where it was.
const TOLERANCE_M: f64 = 0.01;

/// Coordinate systems, each with its area of use (west, east, south, north; degrees, east of
/// 180° for one across the antimeridian), roughly as EPSG gives it.
const SYSTEMS: &[(&str, (f64, f64, f64, f64))] = &[
    ("EPSG:3857", (-180.0, 180.0, -85.051_128, 85.051_128)),
    ("EPSG:32617", (-84.0, -78.0, 0.0, 84.0)),
    ("EPSG:32760", (174.0, 180.0, -80.0, 0.0)),
    ("EPSG:32601", (-180.0, -174.0, 0.0, 84.0)),
    ("EPSG:32661", (-180.0, 180.0, 60.0, 90.0)),
    ("EPSG:32761", (-180.0, 180.0, -90.0, -60.0)),
    ("EPSG:3413", (-180.0, 180.0, 30.0, 90.0)),
    ("EPSG:3031", (-180.0, 180.0, -90.0, -60.0)),
    ("EPSG:2154", (-9.86, 10.38, 41.15, 51.56)),
    ("EPSG:3347", (-141.0, -47.0, 38.0, 86.0)),
    ("EPSG:5070", (-124.79, -66.91, 24.41, 49.38)),
    ("EPSG:3577", (112.0, 154.0, -44.0, -10.0)),
    ("EPSG:3035", (-35.58, 44.83, 24.6, 84.73)),
    ("EPSG:3338", (-172.54, -129.99, 51.3, 71.4)),
    ("EPSG:27700", (-9.0, 2.0, 49.75, 61.0)),
    ("EPSG:2056", (5.96, 10.49, 45.82, 47.81)),
    ("EPSG:2193", (166.37, 178.63, -47.33, -34.1)),
    ("EPSG:3832", (98.0, 300.0, -60.0, 66.67)),
    ("EPSG:4087", (-180.0, 180.0, -90.0, 90.0)),
    ("EPSG:6933", (-180.0, 180.0, -86.0, 86.0)),
    ("+proj=sinu +lon_0=0 +x_0=0 +y_0=0 +R=6371007.181 +units=m +no_defs", (-180.0, 180.0, -90.0, 90.0)),
    ("+proj=robin +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs", (-180.0, 180.0, -90.0, 90.0)),
    ("+proj=natearth +lon_0=0 +datum=WGS84 +units=m +no_defs", (-180.0, 180.0, -90.0, 90.0)),
    ("+proj=wintri +lon_0=0 +datum=WGS84 +units=m +no_defs", (-180.0, 180.0, -90.0, 90.0)),
    ("+proj=eck4 +lon_0=0 +datum=WGS84 +units=m +no_defs", (-180.0, 180.0, -90.0, 90.0)),
];

/// Where a system doesn't bring a place back to within [`TOLERANCE_M`], as (system, how far in
/// metres a place may come back from where it was, whether inside its area of use too or only
/// outside it, why). Each is PROJ's behaviour too, checked with PROJ 9.8.1.
const LIMITS: &[(&str, f64, Where, &str)] = &[
    ("EPSG:3857", f64::INFINITY, Where::Outside, "a latitude past 85.05° is taken as 85.05°, where Web Mercator ends; an export says so"),
    // The authalic latitude's sine is nearly 1 near a pole, and its arcsine loses half its digits
    // there: about R·√ε, a decimetre.
    ("EPSG:3035", 0.2, Where::Outside, "the authalic latitude rounds within a ten-thousandth of a degree of a pole"),
    ("EPSG:6933", 0.2, Where::Outside, "the authalic latitude rounds within a ten-thousandth of a degree of a pole"),
    // Eckert IV: near a pole its inverse solves for the latitude only to about half a kilometre
    // (503 m at 89.9955°, 30°E; PROJ 9.8.1 the same).
    ("+proj=eck4 +lon_0=0 +datum=WGS84 +units=m +no_defs", 600.0, Where::NearPole(0.01), "the inverse's latitude is coarse near a pole"),
    // Robinson's curve between its table rows (every 5°) is in pieces whose single-precision
    // coefficients don't quite meet: at 10°, 35°, 45°, 65° and 90° a band of y up to 2.13 m wide is
    // reached from both sides of the row, and the inverse takes the side above. As PROJ's.
    (
        "+proj=robin +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs",
        2.5,
        Where::Anywhere,
        "Robinson's pieces overlap at some table rows",
    ),
    // Transverse and oblique Mercator take places up to 150° from their central line (as PROJ
    // does); far from it they bring them back less closely. `from_lonlat` refuses a place that
    // doesn't come back within 1e-4° on each axis (#1925), at most 16 m on the ground, such as
    // those near their singular points 90° off on the equator or on the far side of the globe.
    ("EPSG:32617", 16.0, Where::Outside, "transverse Mercator far from its meridian, within 1e-4°"),
    ("EPSG:32760", 16.0, Where::Outside, "transverse Mercator far from its meridian, within 1e-4°"),
    ("EPSG:32601", 16.0, Where::Outside, "transverse Mercator far from its meridian, within 1e-4°"),
    ("EPSG:2193", 16.0, Where::Outside, "transverse Mercator far from its meridian, within 1e-4°"),
    ("EPSG:27700", 16.0, Where::Outside, "transverse Mercator far from its meridian, within 1e-4°"),
    // The datum transformation is chosen for each place by its area of use, as PROJ chooses it:
    // there and back may take different ones at the area's edge (OSGB 1936's at 9°W, 45.6 m
    // apart; at 0°E 61°N 135 m, at 1.72°E 49.75°N 150 m: PROJ 9.8.1 the same to the digit).
    // Just outside the area, in none of the transformations' areas, PROJ falls back to a
    // different one each way: up to 154 m, and as far as 2.02° out (84 m at 9.94°W 60.55°N).
    ("EPSG:27700", 200.0, Where::NearEdge(0.5), "the datum transformation differs there and back at the area's edge"),
    ("EPSG:27700", 200.0, Where::Beyond(3.0), "the datum transformation differs there and back just outside the area"),
    ("EPSG:2056", 16.0, Where::Outside, "oblique Mercator far from its centre line, within 1e-4°"),
    // Its datum, NAD83, has transformations to WGS 84 for Alaska and for Hawaii (up to 0.97 m)
    // as well as none; outside the area of use there and back may take one and not the other
    // (0.60 m at 156.6°W 26.8°N, 0.75 m at 180°W 53.8°N: PROJ 9.8.1 the same to the digit).
    ("EPSG:3347", 1.0, Where::Outside, "NAD83's transformation for Alaska or Hawaii is taken one way only"),
    // Albers' inverse (PROJ's) snaps a latitude within about 0.02° of a pole onto
    // it. Those `from_lonlat` takes are the ones within 1e-4° of the pole.
    ("EPSG:5070", 16.0, Where::Outside, "Albers takes a place near a pole as the pole, within 1e-4°"),
    ("EPSG:3577", 16.0, Where::Outside, "Albers takes a place near a pole as the pole, within 1e-4°"),
    ("EPSG:3338", 16.0, Where::Outside, "Albers takes a place near a pole as the pole, within 1e-4°"),
];

/// Where a limit in [`LIMITS`] holds.
#[derive(Clone, Copy)]
enum Where {
    /// Outside the system's area of use only.
    Outside,
    /// Anywhere.
    Anywhere,
    /// Within this many degrees of a pole.
    NearPole(f64),
    /// Within this many degrees of the area of use's edge, on either side.
    NearEdge(f64),
    /// Outside the area of use, within this many degrees of its edge.
    Beyond(f64),
}

impl Where {
    fn holds(self, (w, e, s, n): (f64, f64, f64, f64), p: Coord, inside: bool) -> bool {
        match self {
            Where::Outside => !inside,
            Where::Anywhere => true,
            Where::NearPole(d) => p.y.abs() >= 90.0 - d,
            // Within `d` of the box's outline, inside (`NearEdge` only) or out.
            Where::NearEdge(d) | Where::Beyond(d) => {
                // The longitude as it lies nearest the box, a turn either way (boxes run east of
                // 180° across the antimeridian).
                let off = |lon: f64| (w - lon).max(lon - e).max(0.0);
                let lon = [p.x, p.x + 360.0, p.x - 360.0].into_iter().min_by(|a, b| off(*a).total_cmp(&off(*b))).unwrap_or(p.x);
                let outside_by = off(lon).hypot((s - p.y).max(p.y - n).max(0.0));
                let inside_by = (lon - w).min(e - lon).min(p.y - s).min(n - p.y);
                if inside { matches!(self, Where::NearEdge(_)) && inside_by < d } else { outside_by < d }
            }
        }
    }
}

/// A number from the environment, else `default`.
fn env(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// A longitude folded into −180..180.
fn fold(lon: f64) -> f64 {
    if lon > 180.0 { lon - 360.0 } else { lon }
}

/// A place in an area of use, often on its edges or corners: (place, whether it is inside).
fn inside((w, e, s, n): (f64, f64, f64, f64)) -> impl Strategy<Value = (Coord, bool)> {
    let lon = prop_oneof![3 => w..=e, 1 => prop::sample::select(vec![w, e, (w + e) / 2.0])];
    let lat = prop_oneof![3 => s..=n, 1 => prop::sample::select(vec![s, n, (s + n) / 2.0])];
    (lon, lat).prop_map(|(x, y)| (Coord::new(fold(x), y), true))
}

/// A place anywhere, often on an edge of the globe: ±180°, the poles, Web Mercator's limit.
fn anywhere() -> impl Strategy<Value = (Coord, bool)> {
    let lon = prop_oneof![3 => -180.0..=180.0f64, 1 => prop::sample::select(vec![-180.0, 180.0, 0.0, 179.999_999, -90.0, 90.0])];
    let lat = prop_oneof![3 => -90.0..=90.0f64, 1 => prop::sample::select(vec![-90.0, 90.0, 0.0, 85.06, -85.06, 89.999_999])];
    (lon, lat).prop_map(|(x, y)| (Coord::new(x, y), false))
}

/// A system (its index in [`SYSTEMS`]) and a place, inside its area of use or anywhere.
fn case() -> impl Strategy<Value = (usize, Coord, bool)> {
    (0..SYSTEMS.len()).prop_flat_map(|i| {
        let area = SYSTEMS.get(i).map_or((-180.0, 180.0, -90.0, 90.0), |s| s.1);
        prop_oneof![3 => inside(area), 1 => anywhere()].prop_map(move |(c, ins)| (i, c, ins))
    })
}

/// Whether `p` is in the area of use `(w, e, s, n)`, whose east edge may be past 180°.
fn in_area((w, e, s, n): (f64, f64, f64, f64), p: Coord) -> bool {
    (s..=n).contains(&p.y) && [p.x - 360.0, p.x, p.x + 360.0].iter().any(|x| (w..=e).contains(x))
}

#[test]
fn a_place_on_the_antimeridian_is_in_an_area_on_either_side() {
    let zone_1 = (-180.0, -174.0, 0.0, 84.0);
    assert!(in_area(zone_1, Coord::new(180.0, 45.0)) && in_area(zone_1, Coord::new(-180.0, 45.0)));
    assert!(in_area((174.0, 180.0, -80.0, 0.0), Coord::new(-180.0, -45.0)));
    assert!(!in_area(zone_1, Coord::new(179.0, 45.0)));
}

/// Cases that went there and back, and places that were refused.
static TRIPS: AtomicUsize = AtomicUsize::new(0);
static REFUSED: AtomicUsize = AtomicUsize::new(0);

/// Takes a place there and back: how far (metres) it came back from where it was, or `None` when
/// it was refused.
fn trip(t: &Transformer, p: Coord) -> Result<Option<f64>, String> {
    let Some(q) = t.from_lonlat(p) else { return Ok(None) };
    let Some(back) = t.to_lonlat(q) else { return Err(format!("{p:?} went to {q:?}, which didn't come back")) };
    let off = wgs84_distance_m(p, back);
    if off.is_nan() {
        return Err(format!("{p:?} went to {q:?} and came back at {back:?}"));
    }
    Ok(Some(off))
}

#[test]
fn a_place_taken_into_a_coordinate_system_comes_back() {
    let transformers: Vec<Transformer> = SYSTEMS
        .iter()
        .map(|(text, _)| Crs::parse(text).and_then(|c| c.transformer()).map_err(|e| format!("{text}: {e}")).unwrap())
        .collect();
    let seed = env("PROPERTY_SEED", 1);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    let cases = u32::try_from(env("PROPERTY_CASES", 20_000)).unwrap_or(20_000);
    let config = Config { cases, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes));
    let result = runner.run(&case(), |(i, p, _)| {
        let (Some((name, area)), Some(t)) = (SYSTEMS.get(i), transformers.get(i)) else { return Ok(()) };
        // Inside the area of use by where the place is, whichever way it was made.
        let inside = in_area(*area, p);
        let limits = LIMITS.iter().filter(|(s, _, at, _)| s == name && at.holds(*area, p, inside));
        let allowed = limits.fold(TOLERANCE_M, |most, l| most.max(l.1));
        match trip(t, p).map_err(|e| TestCaseError::fail(format!("{name}: {e}")))? {
            None if inside => Err(TestCaseError::fail(format!("{name}: {p:?} is in the area of use but was refused"))),
            None => {
                REFUSED.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Some(off) if off > allowed => Err(TestCaseError::fail(format!("{name}: {p:?} came back {off:.3e} m away"))),
            Some(_) => {
                TRIPS.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
        }
    });
    eprintln!("{} places there and back, {} refused", TRIPS.load(Ordering::Relaxed), REFUSED.load(Ordering::Relaxed));
    if let Err(e) = result {
        panic!("PROPERTY_SEED={seed}: {e}");
    }
}
