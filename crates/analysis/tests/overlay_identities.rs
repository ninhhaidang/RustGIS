//! The eight polygon overlay tools obey set algebra, so they check each other with no outside
//! answer. For generated layers A (input) and B (overlay), feature by feature (`a` in A, `b` in B,
//! areas in the analysis plane):
//!
//! - Clip(a) + Erase(a) = a.
//! - Intersect(a, b) = Intersect(b, a) = Split(a in b) = Identity(a, b) = Union(a, b).
//! - Identity(a outside B) = Union(a outside B) = Symmetrical Difference(a) = Erase(a).
//! - Union(b outside A) = Symmetrical Difference(b) = Erase(b by A), and with Clip(b by A) it
//!   makes b.
//! - Update: A's rows are Erase's, then every b as it is.
//! - When B's polygons don't overlap each other (a tiled B), Σ over b of Intersect(a, b) = Clip(a).
//!
//! Clip, Erase, Update and Split each have their own code; Intersect, Identity, Union and
//! Symmetrical Difference share one. So a fix that reaches one and not its twin breaks a line here.
//! The layers lean on what overlays get wrong: edges on a shared grid or a shared diagonal, slanted
//! edges crossing anywhere, a feature the same as an overlay polygon or just inside it, holes (with
//! a polygon in them, or nothing), multipart features, concave outlines, and features wholly
//! inside a polygon, wholly outside one whose extent they meet, or crossing its edge (Clip's three
//! ways, #2880).
//!
//! Areas may differ by [`AREA_TOLERANCE`] of the case's whole input area: the overlay rounds to a
//! grid, and the rounding depends on the inputs even when a result is tiny.
//!
//! Pull requests run a fixed seed; the nightly a new one with more cases. A failure gives its seed
//! and the smallest layers found. To run a seed again, or deeper:
//! `PROPERTY_SEED=7 PROPERTY_CASES=2000 cargo test -p rustgis-analysis --test overlay_identities`.
use std::collections::BTreeMap;

use geo::Area;
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use rustgis_analysis::convert::to_multipolygon;
use rustgis_analysis::{Inputs, Output, OutputLayer};
use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value};
use serde_json::{Value as Json, json};

/// Areas may differ by this fraction of the case's whole input area (0.1 ppm, as the reference
/// corpus allows).
const AREA_TOLERANCE: f64 = 1e-7;

/// A number from the environment, else `default`; one set that isn't a whole number (or isn't
/// text) fails, so a typo doesn't run the defaults without a word (#2639).
fn env(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(v) => v.trim().parse().unwrap_or_else(|_| panic!("{name}={v:?} isn't a whole number")),
        Err(std::env::VarError::NotPresent) => default,
        Err(e) => panic!("{name}: {e}"),
    }
}

/// A rectangle, `(x0, y0, x1, y1)` in degrees.
type Rect = (f64, f64, f64, f64);
/// A feature's polygons: each its rings, the outer one first.
type Shape = Vec<Vec<Vec<Coord>>>;

/// A ring around a rectangle, anticlockwise, or clockwise for a hole.
fn ring((x0, y0, x1, y1): Rect, hole: bool) -> Vec<Coord> {
    let mut r = vec![Coord::new(x0, y0), Coord::new(x1, y0), Coord::new(x1, y1), Coord::new(x0, y1), Coord::new(x0, y0)];
    if hole {
        r.reverse();
    }
    r
}

/// A rectangle shrunk about its centre to `f` of its size.
fn shrunk((x0, y0, x1, y1): Rect, f: f64) -> Rect {
    let (cx, cy, hw, hh) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, (x1 - x0) / 2.0 * f, (y1 - y0) / 2.0 * f);
    (cx - hw, cy - hh, cx + hw, cy + hh)
}

/// A number on a quarter-degree grid half the time, so edges fall on each other.
fn snapped(lo: f64, hi: f64) -> impl Strategy<Value = f64> {
    prop_oneof![(lo..hi).prop_map(|v| (v * 4.0).round() / 4.0), lo..hi]
}

/// A rectangle in and around the 4° scene, from a sliver to half of it across.
fn rect() -> impl Strategy<Value = Rect> {
    let size = || prop_oneof![3 => 0.05..1.5f64, 1 => prop::sample::select(vec![0.25, 0.5, 1.0, 2.0])];
    (snapped(-0.25, 4.0), snapped(-0.25, 4.0), size(), size()).prop_map(|(x, y, w, h)| (x, y, x + w, y + h))
}

/// A star about a centre: its points at even angles from a turn, each at its own distance, so
/// its edges slant and it is concave where a point comes in.
fn star() -> impl Strategy<Value = Shape> {
    (snapped(0.0, 4.0), snapped(0.0, 4.0), 0.0..std::f64::consts::TAU, prop::collection::vec(0.05..1.2f64, 3..10)).prop_map(
        |(cx, cy, turn, radii)| {
            let n = radii.len() as f64;
            let mut r: Vec<Coord> = radii
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    let at = turn + std::f64::consts::TAU * i as f64 / n;
                    Coord::new(cx + d * at.cos(), cy + d * at.sin())
                })
                .collect();
            r.extend(r.first().copied());
            vec![vec![r]]
        },
    )
}

/// A free shape: a rectangle, an L (concave), a rectangle with a hole, two rectangles apart, or
/// a star.
fn free_shape() -> impl Strategy<Value = Shape> {
    prop_oneof![4 => boxy(), 1 => star()]
}

/// A shape of rectangles: one, an L (concave), one with a hole, or two apart.
fn boxy() -> impl Strategy<Value = Shape> {
    (rect(), 0u8..4, 0.2..0.8f64, 0.05..1.0f64).prop_map(|(r, kind, f, gap)| {
        let (x0, y0, x1, y1) = r;
        match kind {
            1 => {
                // The rectangle without its top right corner.
                let (xm, ym) = (x0 + (x1 - x0) * f, y0 + (y1 - y0) * f);
                let c = Coord::new;
                vec![vec![vec![c(x0, y0), c(x1, y0), c(x1, ym), c(xm, ym), c(xm, y1), c(x0, y1), c(x0, y0)]]]
            }
            2 => vec![vec![ring(r, false), ring(shrunk(r, f), true)]],
            3 => vec![vec![ring(r, false)], vec![ring((x1 + gap, y0, x1 + gap + (x1 - x0) * f, y1), false)]],
            _ => vec![vec![ring(r, false)]],
        }
    })
}

/// Grid lines across the scene: 0 and 4, with up to three between, on the quarter grid half the
/// time.
fn lines() -> impl Strategy<Value = Vec<f64>> {
    prop::collection::vec(snapped(0.3, 3.7), 0..4).prop_map(|mut inner| {
        inner.sort_by(f64::total_cmp);
        inner.dedup_by(|a, b| (*a - *b).abs() < 0.1);
        std::iter::once(0.0).chain(inner).chain(std::iter::once(4.0)).collect()
    })
}

/// What a cell of a tiled B becomes.
#[derive(Debug, Clone, Copy)]
enum Cell {
    /// Its own feature.
    Whole,
    /// Left out: a gap in B.
    Gap,
    /// A feature with a hole of this fraction of its size, the hole another feature.
    Filled(f64),
    /// A feature with a hole of this fraction of its size, nothing in it.
    Empty(f64),
    /// A part of the feature before it (multipart, or one shape where they touch).
    Joined,
    /// Two triangles, cut along a diagonal: an edge they share that slants.
    Halved,
}

fn cell() -> impl Strategy<Value = Cell> {
    prop_oneof![
        6 => Just(Cell::Whole),
        1 => Just(Cell::Gap),
        1 => (0.2..0.8f64).prop_map(Cell::Filled),
        1 => (0.2..0.8f64).prop_map(Cell::Empty),
        1 => Just(Cell::Joined),
        2 => Just(Cell::Halved),
    ]
}

/// B's polygons, and the rectangles they are made of, for features of A to sit on.
#[derive(Debug, Clone)]
struct Overlay {
    shapes: Vec<Shape>,
    rects: Vec<Rect>,
    /// Whether no two polygons overlap.
    tiled: bool,
}

/// A tiled B: grid cells, none overlapping another.
fn tiled() -> impl Strategy<Value = Overlay> {
    (lines(), lines()).prop_flat_map(|(xs, ys)| {
        let n = (xs.len() - 1) * (ys.len() - 1);
        prop::collection::vec(cell(), n).prop_map(move |cells| {
            let mut shapes: Vec<Shape> = Vec::new();
            let mut rects = Vec::new();
            let grid = xs.windows(2).flat_map(|x| ys.windows(2).map(move |y| (x[0], y[0], x[1], y[1])));
            for (r, cell) in grid.zip(cells) {
                rects.push(r);
                match cell {
                    Cell::Whole => shapes.push(vec![vec![ring(r, false)]]),
                    Cell::Gap => {}
                    Cell::Filled(f) | Cell::Empty(f) => {
                        let hole = shrunk(r, f);
                        rects.push(hole);
                        shapes.push(vec![vec![ring(r, false), ring(hole, true)]]);
                        if matches!(cell, Cell::Filled(_)) {
                            shapes.push(vec![vec![ring(hole, false)]]);
                        }
                    }
                    Cell::Halved => {
                        let (x0, y0, x1, y1) = r;
                        let c = Coord::new;
                        shapes.push(vec![vec![vec![c(x0, y0), c(x1, y0), c(x1, y1), c(x0, y0)]]]);
                        shapes.push(vec![vec![vec![c(x0, y0), c(x1, y1), c(x0, y1), c(x0, y0)]]]);
                    }
                    Cell::Joined => match shapes.last_mut() {
                        Some(last) => last.push(vec![ring(r, false)]),
                        None => shapes.push(vec![vec![ring(r, false)]]),
                    },
                }
            }
            Overlay { shapes, rects, tiled: true }
        })
    })
}

/// A free B: shapes that may overlap each other.
fn free() -> impl Strategy<Value = Overlay> {
    prop::collection::vec(free_shape(), 1..8).prop_map(|shapes| Overlay { shapes, rects: Vec::new(), tiled: false })
}

/// A feature of A: a free shape, one of B's features as it is, or one of B's rectangles as it is,
/// just inside it, half its size (wholly inside a cell, or in a hole, where the cell's extent
/// meets it but the cell doesn't), or a fifth larger (into the cells around it).
fn input_shape(b: &Overlay) -> BoxedStrategy<Shape> {
    let same = prop::sample::select(b.shapes.clone());
    if b.rects.is_empty() {
        return prop_oneof![4 => free_shape(), 1 => same].boxed();
    }
    let on = (prop::sample::select(b.rects.clone()), prop::sample::select(vec![1.0, 0.999_999, 0.5, 1.2]))
        .prop_map(|(r, f)| vec![vec![ring(shrunk(r, f), false)]]);
    prop_oneof![4 => free_shape(), 1 => same, 2 => on].boxed()
}

/// A case: B, then A.
fn case() -> impl Strategy<Value = (Overlay, Vec<Shape>)> {
    let some = |b: &Overlay| !b.shapes.is_empty();
    prop_oneof![3 => tiled().prop_filter("B has a polygon", some), 1 => free()].prop_flat_map(|b| {
        let a = prop::collection::vec(input_shape(&b), 1..10);
        (Just(b), a)
    })
}

/// A layer named `name` of `shapes`, each with a key (`name` and its place, as `a3`).
fn layer(name: &str, shapes: &[Shape]) -> Dataset {
    let mut ds = Dataset::new(name, GeometryType::Polygon, vec![Field::new("key", FieldType::Text)]);
    for (i, s) in shapes.iter().enumerate() {
        ds.push(Some(Geometry::Polygon(s.clone())), vec![Value::Text(format!("{}{i}", name.to_lowercase()))]);
    }
    ds
}

struct Layers(Dataset, Dataset);

impl Inputs for Layers {
    fn dataset(&self, name: &str) -> Option<&Dataset> {
        [&self.0, &self.1].into_iter().find(|d| d.name == name)
    }
    fn selection(&self, _: &str) -> &[usize] {
        &[]
    }
}

/// A geometry's area in the analysis plane (none for no polygon).
fn area(g: Option<&Geometry>) -> f64 {
    g.and_then(to_multipolygon).map_or(0.0, |m| m.unsigned_area())
}

/// A field's value as text, `""` for a missing field or null.
fn text(ds: &Dataset, row: usize, field: &str) -> String {
    match ds.field_index(field).map(|fi| ds.value(row, fi)) {
        Some(Value::Text(t)) => t.clone(),
        _ => String::new(),
    }
}

/// Area by key: each output row's key fields' values (`""` for none), its areas summed.
type Areas = BTreeMap<(String, String), f64>;

/// Adds `ds`'s rows to `into` by the values of its fields `first` and `second` (`""` without one).
fn by_key(ds: &Dataset, first: &str, second: Option<&str>, into: &mut Areas) {
    for row in 0..ds.features.len() {
        let key = (text(ds, row, first), second.map_or(String::new(), |f| text(ds, row, f)));
        *into.entry(key).or_default() += area(ds.features.geometry(row));
    }
}

/// A tool's output: its layer's areas by key, or for Split the areas by (piece's key, layer's).
fn run(layers: &Layers, tool: &str, params: Json) -> Result<Areas, TestCaseError> {
    let r = rustgis_analysis::run(tool, layers, &params).map_err(|e| TestCaseError::fail(format!("{tool} {params}: {e}")))?;
    let mut out = Areas::new();
    match r.output {
        Output::Layer(ds) => by_key(&ds, "key", ds.field_index("key_2").is_some().then_some("key_2"), &mut out),
        Output::Layers(layers) => {
            for l in layers {
                let OutputLayer::Vector(ds) = l else { return Err(TestCaseError::fail(format!("{tool}: a raster or table"))) };
                let b = ds.name.rsplit('_').next().unwrap_or_default();
                for row in 0..ds.features.len() {
                    *out.entry((text(&ds, row, "key"), b.to_string())).or_default() += area(ds.features.geometry(row));
                }
            }
        }
        other => return Err(TestCaseError::fail(format!("{tool}: {other:?}"))),
    }
    Ok(out)
}

/// The areas in `areas` whose first key is `first` and whose second `second` accepts, summed.
fn of(areas: &Areas, first: &str, second: impl Fn(&str) -> bool) -> f64 {
    areas.iter().filter(|((a, b), _)| a == first && second(b)).map(|(_, v)| v).sum()
}

/// Every line of the table that fails on one case, each with its two sides, and whether any
/// feature of A overlaps one of B (so the case compares more than nothing with nothing).
fn identities(b: &Overlay, a: &[Shape]) -> Result<(Vec<String>, bool), TestCaseError> {
    let (la, lb) = (layer("A", a), layer("B", &b.shapes));
    let keys = |ds: &Dataset| (0..ds.features.len()).map(|r| (text(ds, r, "key"), area(ds.features.geometry(r)))).collect::<Vec<_>>();
    let (ka, kb) = (keys(&la), keys(&lb));
    let total: f64 = ka.iter().chain(&kb).map(|(_, v)| v).sum();
    let tol = AREA_TOLERANCE * total;
    let layers = Layers(la, lb);
    let ab = |tool: &str, key: &str| run(&layers, tool, json!({"input": "A", key: "B"}));
    let ba = |tool: &str, key: &str| run(&layers, tool, json!({"input": "B", key: "A"}));
    let clip = ab("clip", "clip")?;
    let erase = ab("erase", "erase")?;
    let intersect = ab("intersect", "overlay")?;
    let identity = ab("identity", "overlay")?;
    let union = ab("union", "overlay")?;
    let symdiff = ab("symmetricalDifference", "overlay")?;
    let update = ab("update", "update")?;
    let split = run(&layers, "split", json!({"input": "A", "split": "B", "field": "key"}))?;
    let intersect_ba = ba("intersect", "overlay")?;
    let erase_ba = ba("erase", "erase")?;
    let clip_ba = ba("clip", "clip")?;

    let mut failed = Vec::new();
    let mut check = |what: String, x: f64, y: f64| {
        // A difference that isn't a number fails too.
        let off = (x - y).abs();
        if off.is_nan() || off > tol {
            failed.push(format!("{what}: {x:.1} m² vs {y:.1} m² (off {:.3e} of the inputs)", off / total));
        }
    };
    let none = |k: &str| k.is_empty();
    for (k, whole) in &ka {
        let (c, e) = (of(&clip, k, none), of(&erase, k, none));
        check(format!("Clip({k}) + Erase({k}) = {k}"), c + e, *whole);
        for (kb, _) in &kb {
            let is = |x: &str| x == kb;
            let i = of(&intersect, k, is);
            check(format!("Intersect({k}, {kb}) = Intersect({kb}, {k})"), i, of(&intersect_ba, kb, |x| x == k));
            check(format!("Intersect({k}, {kb}) = Split({k} in {kb})"), i, of(&split, k, is));
            check(format!("Intersect({k}, {kb}) = Identity({k}, {kb})"), i, of(&identity, k, is));
            check(format!("Intersect({k}, {kb}) = Union({k}, {kb})"), i, of(&union, k, is));
        }
        check(format!("Identity({k} outside B) = Erase({k})"), of(&identity, k, none), e);
        check(format!("Union({k} outside B) = Erase({k})"), of(&union, k, none), e);
        check(format!("Symmetrical Difference({k}) = Erase({k})"), of(&symdiff, k, none), e);
        check(format!("Update({k}) = Erase({k})"), of(&update, k, none), e);
        if b.tiled {
            check(format!("Σ Intersect({k}, b) = Clip({k}) (B tiled)"), of(&intersect, k, |x| !x.is_empty()), c);
        }
    }
    for (k, whole) in &kb {
        let e = of(&erase_ba, k, none);
        check(format!("Union({k} outside A) = Erase({k} by A)"), of(&union, "", |x| x == k), e);
        check(format!("Symmetrical Difference({k}) = Erase({k} by A)"), of(&symdiff, "", |x| x == k), e);
        check(format!("Clip({k} by A) + Erase({k} by A) = {k}"), of(&clip_ba, k, none) + e, *whole);
        check(format!("Update adds {k} as it is"), of(&update, k, none), *whole);
    }
    // Nothing else: no row whose keys aren't of the layers, and Symmetrical Difference keeps no
    // part both cover.
    let known = |k: &str| k.is_empty() || ka.iter().chain(&kb).any(|(x, _)| x == k);
    let made = [
        ("Clip", &clip),
        ("Erase", &erase),
        ("Intersect", &intersect),
        ("Identity", &identity),
        ("Union", &union),
        ("Symmetrical Difference", &symdiff),
        ("Update", &update),
        ("Split", &split),
        ("Intersect of B by A", &intersect_ba),
        ("Erase of B by A", &erase_ba),
        ("Clip of B by A", &clip_ba),
    ];
    for (name, areas) in made {
        for ((x, y), v) in areas.iter() {
            if !known(x) || !known(y) {
                failed.push(format!("{name} made a row keyed {x:?}, {y:?} ({v:.1} m²)"));
            }
        }
    }
    for ((x, y), v) in &symdiff {
        if !x.is_empty() && !y.is_empty() {
            failed.push(format!("Symmetrical Difference kept {v:.1} m² of both {x} and {y}"));
        }
    }
    let of_layer = |keys: &[(String, f64)], k: &str| keys.iter().any(|(x, _)| x == k);
    let overlapped = intersect.iter().any(|((x, y), v)| of_layer(&ka, x) && of_layer(&kb, y) && *v > tol);
    Ok((failed, overlapped))
}

#[test]
fn overlay_tools_agree_with_each_other() {
    let seed = env("PROPERTY_SEED", 1);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    // About 3 ms a case.
    let cases = env("PROPERTY_CASES", 512);
    let cases = u32::try_from(cases).unwrap_or_else(|_| panic!("PROPERTY_CASES={cases} is past {}", u32::MAX));
    let config = Config { cases, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes));
    // The cases whose layers overlap: with none, every line would compare nothing with nothing.
    let overlapped = std::sync::atomic::AtomicU32::new(0);
    let result = runner.run(&case(), |(b, a)| {
        let (failed, overlaps) = identities(&b, &a)?;
        overlapped.fetch_add(u32::from(overlaps), std::sync::atomic::Ordering::Relaxed);
        if failed.is_empty() { Ok(()) } else { Err(TestCaseError::fail(failed.join("\n"))) }
    });
    if let Err(e) = result {
        panic!("PROPERTY_SEED={seed}: {e}");
    }
    let overlapped = overlapped.into_inner();
    assert!(overlapped >= cases / 4, "PROPERTY_SEED={seed}: only {overlapped} of {cases} cases have layers that overlap");
}
