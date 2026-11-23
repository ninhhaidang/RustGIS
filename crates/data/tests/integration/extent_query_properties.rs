//! A source asked for the features in an extent gives the features a whole read gives whose
//! geometry's box meets the extent (`Request`), as a property: through each provider's spatial
//! index (GeoPackage and Shapefile R-trees, FlatGeobuf's packed R-tree, GeoJSON's boxes) or its
//! scan, by `read` and, where the source has it (not GeoJSON), by `visit_chunks`. What a provider
//! may do differently is listed in [`LIMITS`], with the reason. Layers and extents lean on edges:
//! boxes that touch, degenerate boxes, empty and inverted boxes, infinite boxes, coordinates that
//! `f32` can't hold, empty layers.
//!
//! Pull requests run a fixed seed; the nightly a new one with more cases. A failure gives its seed
//! and the smallest layer and extent found. To run a seed again, or deeper:
//! `PROPERTY_SEED=7 PROPERTY_CASES=2000 cargo test -p rustgis-data --test integration extent_query_properties::`.

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use rustgis_data::source::{FeatureSource, MAX_LIMIT, Request, open_path};
use rustgis_data::{Coord, Dataset, Extent, Geometry, GeometryType};
use std::sync::atomic::{AtomicU64, Ordering};

const FORMATS: [&str; 5] = ["gpkg", "fgb", "shp", "parquet", "geojson"];

/// Differences a provider may have from the whole read filtered in memory, as (format, the start
/// of a difference from [`differences`], why).
const LIMITS: &[(&str, &str, &str)] = &[];

/// Whether `difference` is a limit of `ext` listed above.
fn declared(ext: &str, difference: &str) -> bool {
    LIMITS.iter().any(|(f, start, _)| *f == ext && difference.starts_with(start))
}

/// A number from the environment, else `default`; one set that isn't a whole number (or isn't
/// text) fails, so a typo doesn't run the defaults without a word (#2639).
fn env(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(v) => v.trim().parse().unwrap_or_else(|_| panic!("{name}={v:?} isn't a whole number")),
        Err(std::env::VarError::NotPresent) => default,
        Err(e) => panic!("{name}: {e}"),
    }
}

/// Values that extents and geometries share, so boxes touch: whole degrees, the edges of the
/// world, and ones `f32` can't hold (0.1, a hair either side of 1).
const EDGES: [f64; 12] = [-180.0, -90.0, -1.0, 0.0, 0.1, 0.3, 1.0 - 1e-9, 1.0, 1.0 + 1e-9, 45.123_456_789, 90.0, 180.0];

/// A coordinate value, often one of [`EDGES`].
fn number(range: f64) -> impl Strategy<Value = f64> {
    prop_oneof![2 => -range..range, 3 => prop::sample::select(EDGES.to_vec()).prop_map(move |v| v.clamp(-range, range))]
}

fn coord() -> impl Strategy<Value = Coord> {
    (number(180.0), number(90.0)).prop_map(|(x, y)| Coord::new(x, y))
}

/// A coordinate and another up to `size` from it, often on a whole degree, within the world.
fn pair(size: f64) -> impl Strategy<Value = (Coord, Coord)> {
    let step = prop_oneof![2 => -size..size, 1 => (-3i32..4).prop_map(f64::from)];
    (coord(), step.clone(), step).prop_map(|(a, dx, dy)| (a, Coord::new((a.x + dx).clamp(-180.0, 180.0), (a.y + dy).clamp(-90.0, 90.0))))
}

/// A geometry of a layer of `kind`: points and multipoints, lines, rectangles.
fn geometry(kind: GeometryType) -> BoxedStrategy<Geometry> {
    match kind {
        GeometryType::Point => prop_oneof![
            3 => coord().prop_map(Geometry::Point),
            1 => prop::collection::vec(coord(), 1..4).prop_map(Geometry::MultiPoint),
        ]
        .boxed(),
        GeometryType::Polyline => prop::collection::vec(pair(10.0), 1..3)
            .prop_map(|l| Geometry::Line(vec![l.into_iter().flat_map(|(a, b)| [a, b]).collect()]))
            .boxed(),
        _ => pair(20.0)
            .prop_filter("a rectangle with an area", |(a, b)| a.x != b.x && a.y != b.y)
            .prop_map(|(a, b)| {
                let e = Extent::from_points(a, b);
                let ring = [(e.min_x, e.min_y), (e.max_x, e.min_y), (e.max_x, e.max_y), (e.min_x, e.max_y), (e.min_x, e.min_y)];
                Geometry::Polygon(vec![vec![ring.iter().map(|&(x, y)| Coord::new(x, y)).collect()]])
            })
            .boxed(),
    }
}

/// A layer of up to 60 features, sometimes none, a few without geometry: enough for more than
/// one node of a packed R-tree.
fn layer() -> impl Strategy<Value = Dataset> {
    let kind = prop::sample::select(vec![GeometryType::Point, GeometryType::Polyline, GeometryType::Polygon]);
    let size = prop_oneof![1 => Just(0usize), 6 => 1usize..61];
    (kind, size).prop_flat_map(|(kind, n)| {
        prop::collection::vec(prop::option::weighted(0.95, geometry(kind)), n).prop_map(move |gs| {
            let mut ds = Dataset::new("layer", kind, vec![]);
            for g in gs {
                ds.push(g, vec![]);
            }
            ds
        })
    })
}

/// An extent: one from two coordinates (touching the layer's on [`EDGES`]), a line or a point,
/// an inverted (empty) one, the world, or an infinite one.
fn extent() -> impl Strategy<Value = Extent> {
    prop_oneof![
        4 => pair(40.0).prop_map(|(a, b)| Extent::from_points(a, b)),
        2 => (coord(), coord()).prop_map(|(a, b)| Extent::from_points(a, b)),
        1 => coord().prop_map(|c| Extent::new(c.x, c.y, c.x, c.y)),
        1 => (coord(), number(90.0)).prop_map(|(c, y)| Extent::new(c.x, c.y.min(y), c.x, c.y.max(y))),
        1 => (coord(), coord()).prop_map(|(a, b)| Extent::new(a.x.max(b.x) + 1.0, a.y, a.x.min(b.x), b.y)),
        1 => Just(Extent::new(-180.0, -90.0, 180.0, 90.0)),
        1 => Just(Extent::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::INFINITY)),
    ]
}

/// The ids of a read's features, sorted.
fn ids(fs: impl IntoIterator<Item = u64>) -> Vec<u64> {
    let mut v: Vec<u64> = fs.into_iter().collect();
    v.sort_unstable();
    v
}

/// `q` grown by what rounding each side to the nearest `f32` may move it.
fn widened(q: &Extent) -> Extent {
    let pad = |v: f64| v.abs() * f64::from(f32::EPSILON) + f64::from(f32::MIN_POSITIVE);
    Extent::new(q.min_x - pad(q.min_x), q.min_y - pad(q.min_y), q.max_x + pad(q.max_x), q.max_y + pad(q.max_y))
}

/// How the features a source gave for `q` (`got`) differ from what the whole read filtered in
/// memory gives: features given whose box doesn't meet `q`, and features missed whose box does.
fn differences(all: &[rustgis_data::Feature], got: &[u64], q: &Extent, how: &str) -> Vec<String> {
    let want = ids(all.iter().filter(|f| f.geometry.as_ref().is_some_and(|g| g.extent().intersects(q))).map(|f| f.id));
    let mut out = Vec::new();
    for w in got.windows(2).filter(|w| w[0] == w[1]) {
        out.push(format!("{how} gave a feature twice (id {})", w[0]));
    }
    for id in got.iter().filter(|id| !want.contains(id)) {
        let g = all.iter().find(|f| f.id == *id).and_then(|f| f.geometry.as_ref());
        let near = g.is_some_and(|g| g.extent().intersects(&widened(q)));
        let outside = if near { "just outside the extent, as an `f32`" } else { "outside the extent" };
        out.push(format!("{how} gave a feature {outside} (id {id}, box {:?})", g.map(|g| g.extent())));
    }
    for id in want.iter().filter(|id| !got.contains(id)) {
        let g = all.iter().find(|f| f.id == *id).and_then(|f| f.geometry.as_ref());
        out.push(format!("{how} missed a feature in the extent (id {id}, box {:?})", g.map(|g| g.extent())));
    }
    out
}

/// What `src` does differently for `q`, besides the limits.
fn check(src: &dyn FeatureSource, q: &Extent) -> Result<(Vec<String>, usize, usize), String> {
    let all = src.read(&Request::all()).map_err(|e| format!("whole read: {e}"))?;
    let read = src.read(&Request { extent: Some(*q), limit: MAX_LIMIT, own: None }).map_err(|e| format!("read: {e}"))?;
    let mut found = differences(&all.features, &ids(read.features.iter().map(|f| f.id)), q, "read");
    if !read.complete {
        found.push("read was incomplete".into());
    }
    if src.visits_chunks() {
        let (mut visited, mut chunks) = (Vec::new(), 0);
        // A budget no feature fits in, so each comes in a chunk of its own.
        src.visit_chunks(Some(*q), 1, &mut |fs| {
            chunks += 1;
            visited.extend(fs.into_iter().map(|f| f.id));
            Ok(())
        })
        .map_err(|e| format!("visit: {e}"))?;
        if visited.len() >= 2 && chunks < 2 {
            found.push(format!("visit gave {} features in one chunk of a byte", visited.len()));
        }
        found.extend(differences(&all.features, &ids(visited), q, "visit"));
    }
    Ok((found, read.features.len(), all.features.len()))
}

#[test]
fn an_extent_read_gives_the_features_a_whole_read_filters_to() {
    rustgis_data::source::shp::use_temporary_index_dir();
    let dir = tempfile::tempdir().unwrap();
    let case = AtomicU64::new(0);
    // Cases where some formats' answer was neither all of the layer nor none of it.
    let partial = AtomicU64::new(0);
    let seed = env("PROPERTY_SEED", 1);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    // About 20 ms a case.
    let cases = env("PROPERTY_CASES", 128);
    let cases = u32::try_from(cases).unwrap_or_else(|_| panic!("PROPERTY_CASES={cases} is past {}", u32::MAX));
    let config = Config { cases, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes));
    // Layers each format wrote, by kind (points, lines, polygons): one that refuses every layer
    // of a kind checks nothing for it.
    let written = std::cell::RefCell::new([[0u32; 3]; FORMATS.len()]);
    // Kinds of layer made: a small run may not make all three.
    let made = std::cell::RefCell::new([false; 3]);
    let result = runner.run(&(layer(), extent()), |(ds, q)| {
        // A folder of its own for each case: a Shapefile's index is reused by path and stamp.
        let sub = dir.path().join(case.fetch_add(1, Ordering::Relaxed).to_string());
        std::fs::create_dir_all(&sub).unwrap();
        let mut some = false;
        let kind = match ds.geometry_type {
            GeometryType::Point => 0,
            GeometryType::Polyline => 1,
            _ => 2,
        };
        // A format may refuse a layer with nothing to draw (a Shapefile does).
        if ds.features.iter().any(|f| f.geometry.is_some()) {
            made.borrow_mut()[kind] = true;
        }
        for (k, ext) in FORMATS.into_iter().enumerate() {
            let path = sub.join(format!("layer.{ext}"));
            if rustgis_data::io::write_path(&ds, &path).is_err() {
                continue;
            }
            written.borrow_mut()[k][kind] += 1;
            let src = open_path(&path, None).map_err(|e| TestCaseError::fail(format!("{ext}: open: {e}")))?;
            let (found, n, total) = check(src.as_ref(), &q).map_err(|e| TestCaseError::fail(format!("{ext}: {e}")))?;
            some |= n > 0 && n < total;
            let wrong: Vec<String> = found.into_iter().filter(|d| !declared(ext, d)).collect();
            if !wrong.is_empty() {
                return Err(TestCaseError::fail(format!("{ext}: {wrong:?}")));
            }
        }
        partial.fetch_add(u64::from(some), Ordering::Relaxed);
        Ok(())
    });
    eprintln!("{} of {} cases matched part of a layer", partial.load(Ordering::Relaxed), case.load(Ordering::Relaxed));
    if let Err(e) = result {
        panic!("PROPERTY_SEED={seed}: {e}");
    }
    let made = made.into_inner();
    for (ext, n) in FORMATS.iter().zip(written.into_inner()) {
        let missing = n.iter().zip(made).any(|(n, made)| made && *n == 0);
        assert!(!missing, "PROPERTY_SEED={seed}: {ext} wrote no layer of a kind made (points, lines, polygons): {n:?}");
    }
}
