//! Never hold the window (AGENTS.md), as a property: work split over several threads gives what
//! one thread gives, bit for bit. Each case makes a scene of polygons, points and a raster whose
//! sizes straddle the tools' thresholds (256 features, 200,000 units of raster work), runs every
//! tool on it with the thread setting at one and at several, and compares the outputs exactly:
//! features, their order, values (floats by their bits), messages and raster cells. Nothing may
//! differ: a refusal included, since the work caps that grow with the threads (#1291) are far
//! above these scenes.
//!
//! The thread setting holds for the whole process, so the property is one test, alone in its
//! file. Pull requests run a fixed seed; the nightly a new one with more cases. A failure gives
//! its seed and the smallest scene found. To run a seed again, or deeper:
//! `PROPERTY_SEED=7 PROPERTY_CASES=200 cargo test -p rustgis-analysis --test parallel_serial`.
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use rustgis_analysis::{Inputs, RasterInput, density, kriging, rasterops};
use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
use rustgis_data::{Coord, Dataset, Extent, Field, FieldType, Geometry, GeometryType, Value, threads};
use serde_json::{Value as Json, json};

/// A number from the environment, else `default`.
fn env(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// The layers and the raster a case runs the tools on.
#[derive(Debug, Clone)]
struct Scene {
    polygons: Dataset,
    points: Dataset,
    overlay: Dataset,
    /// Width of a 450-row raster over the scene: 380 to 520 columns, 171,000 to 234,000 cells.
    width: u32,
    /// The raster's cells left without data, by index.
    holes: Vec<usize>,
    /// Cells across the scene for IDW, kriging and kernel density.
    across: u32,
    threads: usize,
}

impl Inputs for Scene {
    fn dataset(&self, name: &str) -> Option<&Dataset> {
        [&self.polygons, &self.points, &self.overlay].into_iter().find(|d| d.name == name)
    }
    fn selection(&self, _: &str) -> &[usize] {
        &[]
    }
}

/// A number on a quarter-degree grid half the time, so shapes share edges and points lie on them.
fn snapped(lo: f64, hi: f64) -> impl Strategy<Value = f64> {
    prop_oneof![(lo..hi).prop_map(|v| (v * 4.0).round() / 4.0), lo..hi]
}

/// A square, a sliver or a point-sized square around a centre in the 4° scene.
fn square() -> impl Strategy<Value = Geometry> {
    let size = prop_oneof![3 => 0.05..0.6f64, 1 => prop::sample::select(vec![0.25, 0.5, 1e-9])];
    (snapped(0.0, 4.0), snapped(0.0, 4.0), size, prop::bool::weighted(0.1)).prop_map(|(x, y, s, thin)| {
        let (w, h) = if thin { (s, 1e-7) } else { (s, s) };
        let c = |x, y| Coord::new(x, y);
        Geometry::Polygon(vec![vec![vec![c(x, y), c(x + w, y), c(x + w, y + h), c(x, y + h), c(x, y)]]])
    })
}

/// A value for a decimal field, leaning on edges.
fn number() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => (-1e3..1e3f64).prop_map(Value::Float),
        1 => prop::sample::select(vec![0.1, -0.0, 1e300, -1e300, 5e-324, f64::NAN, f64::INFINITY, 0.0]).prop_map(Value::Float),
        1 => Just(Value::Null),
    ]
    .boxed()
}

/// Fields every layer has: a class, a decimal and a whole number.
fn fields() -> Vec<Field> {
    vec![Field::new("class", FieldType::Text), Field::new("v", FieldType::Double), Field::new("n", FieldType::Integer)]
}

/// A layer of `count` features of `geometry`, some repeated, a few without geometry.
fn layer(
    name: &'static str,
    kind: GeometryType,
    geometry: BoxedStrategy<Geometry>,
    count: impl Strategy<Value = usize>,
) -> impl Strategy<Value = Dataset> {
    let class = prop::sample::select(vec!["a", "b", "c", ""]).prop_map(|s| Value::Text(s.into()));
    let whole = prop_oneof![4 => (-50i64..50).prop_map(Value::Int), 1 => prop::sample::select(vec![i64::MAX, i64::MIN, 1 << 53]).prop_map(Value::Int)];
    let feature = (prop::option::weighted(0.97, geometry), class, number(), whole, prop::bool::weighted(0.05));
    count.prop_flat_map(move |n| prop::collection::vec(feature.clone(), n)).prop_map(move |features| {
        let mut ds = Dataset::new(name, kind, fields());
        let mut last = None;
        for (g, class, v, n, again) in features {
            // A feature repeating the one before it, as duplicates in real data do.
            let (g, attrs) = match (again, last.clone()) {
                (true, Some(prev)) => prev,
                _ => (g, vec![class, v, n]),
            };
            last = Some((g.clone(), attrs.clone()));
            ds.push(g, attrs);
        }
        ds
    })
}

/// A count around 256, where feature tools go from one thread to several, or a few.
fn around_threshold() -> impl Strategy<Value = usize> {
    prop_oneof![4 => 240usize..290, 1 => 1usize..20]
}

fn scene() -> impl Strategy<Value = Scene> {
    let point = (snapped(-0.2, 4.2), snapped(-0.2, 4.2)).prop_map(|(x, y)| Geometry::Point(Coord::new(x, y))).boxed();
    (
        layer("polygons", GeometryType::Polygon, square().boxed(), around_threshold()),
        layer("points", GeometryType::Point, point, around_threshold()),
        layer("overlay", GeometryType::Polygon, square().boxed(), 1usize..8),
        380u32..520,
        prop::collection::vec(0usize..234_000, 0..40),
        16u32..44,
        prop::sample::select(vec![2usize, 3, 7, 8]),
    )
        .prop_map(|(polygons, points, overlay, width, holes, across, threads)| Scene {
            polygons,
            points,
            overlay,
            width,
            holes,
            across,
            threads,
        })
}

/// The scene's raster: hills over the 4° scene, with holes.
fn raster(s: &Scene) -> Raster {
    let (w, h) = (s.width as usize, 450usize);
    let mut cells: Vec<f32> =
        (0..w * h).map(|i| ((i % w) as f32 * 0.05).sin() * 300.0 + ((i / w) as f32 * 0.07).cos() * 200.0 + (i % 7) as f32).collect();
    for &i in &s.holes {
        if let Some(c) = cells.get_mut(i) {
            *c = -9999.0;
        }
    }
    let gt = GeoTransform { origin_x: 0.0, origin_y: 4.0, pixel_width: 4.0 / w as f64, pixel_height: -4.0 / h as f64 };
    Raster::new("dem", s.width, 450, vec![cells], gt, Crs::LonLat, Some(-9999.0), ColorHint::Gray, "Float32").unwrap()
}

/// The points with a finite value, scaled by `scale` (metres for a projected system).
fn weighted(s: &Scene, scale: f64) -> Vec<(Coord, f64)> {
    s.points
        .features
        .iter()
        .filter_map(|f| match (&f.geometry, f.attrs.get(1)) {
            (Some(Geometry::Point(c)), Some(Value::Float(v))) if v.is_finite() && v.abs() < 1e6 => {
                Some((Coord::new(c.x * scale, c.y * scale), *v))
            }
            _ => None,
        })
        .collect()
}

/// What a tool made: its layer and messages as their debug text (which writes a float exactly),
/// and a raster's cells by their bits.
type Made = Result<(String, Vec<u32>), String>;

/// Every tool's output on the scene, by name, or its error.
fn outputs(s: &Scene) -> Vec<(String, Made)> {
    let tool = |id: &str, p: Json| rustgis_analysis::run(id, s, &p).map(|r| (format!("{:?} {:?}", r.output, r.messages), Vec::new()));
    let stats = json!([{"field": "v", "statistic": "SUM"}, {"field": "v", "statistic": "MEAN"}, {"field": "n", "statistic": "SUM"}]);
    let overlays = ["clip", "erase", "intersect", "identity", "union", "symmetricalDifference", "update", "split"];
    let mut out: Vec<(String, Made)> = overlays
        .iter()
        .map(|id| {
            let key = match *id {
                "clip" | "erase" | "split" | "update" => *id,
                _ => "overlay",
            };
            (id.to_string(), tool(id, json!({"input": "polygons", key: "overlay"})))
        })
        .collect();
    let mut add = |name: &str, r: Made| out.push((name.to_string(), r));
    add("clip points", tool("clip", json!({"input": "points", "clip": "overlay"})));
    add("buffer", tool("buffer", json!({"input": "points", "distance": 5, "units": "Kilometers"})));
    add("buffer dissolved", tool("buffer", json!({"input": "polygons", "distance": 2, "dissolve": "All"})));
    add("dissolve", tool("dissolve", json!({"input": "polygons", "field": "class", "statistics": stats})));
    add("near", tool("near", json!({"input": "points", "near": "polygons"})));
    add("convex hull", tool("convexHull", json!({"input": "polygons", "group": "None"})));
    add("convex hull all", tool("convexHull", json!({"input": "polygons", "group": "All"})));
    add("feature to point", tool("featureToPoint", json!({"input": "polygons"})));
    add("simplify", tool("simplify", json!({"input": "polygons", "tolerance": 5})));
    add("select by location", tool("selectByLocation", json!({"input": "points", "select": "polygons", "distance": 1})));
    add("summarize within", tool("countPoints", json!({"polygons": "polygons", "points": "points", "statistics": stats})));
    add("calculate geometry", tool("calculateGeometry", json!({"input": "polygons"})));
    let merge = json!([{"field": "v", "rule": "sum"}, {"field": "n", "rule": "max"}]);
    add("spatial join", tool("spatialJoin", json!({"target": "polygons", "join": "points", "merge": merge})));
    add("spatial join closest", tool("spatialJoin", json!({"target": "points", "join": "polygons", "match": "Closest"})));
    add("spatial join many", tool("spatialJoin", json!({"target": "polygons", "join": "points", "operation": "Join one to many"})));

    let dem = raster(s);
    let bits = |r: Result<Vec<f32>, String>| r.map(|v| (String::new(), v.iter().map(|c| c.to_bits()).collect()));
    let rasters = |r: Result<Raster, String>| bits(r.map(|r| r.bands.into_iter().flat_map(|b| b.data).collect()));
    add("slope", bits(rasterops::slope(&dem, 0, 1.0)));
    add("aspect", bits(rasterops::aspect(&dem, 0)));
    add("hillshade", bits(rasterops::hillshade(&dem, 0, 315.0, 45.0, 1.0)));
    add("band math", bits(rasterops::band_math(&dem, "(b1 - 100) / (b1 + 0.5) * 3")));
    add("k-means", bits(rasterops::kmeans(&dem, &[0], 6, 4)));
    let viewshed = rasterops::viewshed(&dem, 0, Coord::new(2.0, 2.0), 30.0, 150_000.0, rasterops::Heights::default());
    add("viewshed", viewshed.map(|v| (format!("{} {}", v.through_gaps, v.read_nearest), v.cells.iter().map(|c| c.to_bits()).collect())));
    let with_raster = WithRaster(s, &dem);
    let zonal = rustgis_analysis::run("zonalStats", &with_raster, &json!({"raster": "dem", "polygons": "polygons"}));
    add("zonal statistics", zonal.map(|r| (format!("{:?} {:?}", r.output, r.messages), Vec::new())));

    let extent = Extent::new(-0.2, -0.2, 4.2, 4.2);
    let cell = 4.4 / f64::from(s.across);
    let points = weighted(s, 1.0);
    add("idw", rasters(rasterops::idw(&points, &extent, cell, 2.0, 0, "idw", Crs::LonLat)));
    add("idw power 3", rasters(rasterops::idw(&points, &extent, cell, 3.0, 12, "idw", Crs::LonLat)));
    let params = kriging::auto_variogram(&points, &extent, kriging::VariogramModel::Spherical);
    add("kriging", rasters(kriging::kriging(&points, &extent, cell, &params, 12, "kriging", Crs::LonLat)));
    // Kernel density takes a projected system: the scene in metres, 50 km around each point.
    let metres = weighted(s, 100_000.0);
    let radius = 50_000.0;
    for kernel in [density::KernelType::Quartic, density::KernelType::Gaussian] {
        let made = density::kernel_density(&metres, None, radius / f64::from(2 * s.across), radius, kernel, "kd", Crs::WebMercator);
        add(&format!("kernel density {kernel:?}"), rasters(made));
    }
    out
}

/// The scene with its raster, for Zonal Statistics.
struct WithRaster<'a>(&'a Scene, &'a Raster);

impl Inputs for WithRaster<'_> {
    fn dataset(&self, name: &str) -> Option<&Dataset> {
        self.0.dataset(name)
    }
    fn selection(&self, _: &str) -> &[usize] {
        &[]
    }
    fn raster(&self, name: &str) -> Option<RasterInput<'_>> {
        (name == "dem").then_some(RasterInput { raster: self.1, path: None, embedded: None })
    }
}

/// Where two outputs first differ: the text with some of each around it, or the cell.
fn first_difference((a, cells): &(String, Vec<u32>), (b, others): &(String, Vec<u32>)) -> String {
    if let Some(i) = cells.iter().zip(others).position(|(x, y)| x != y).or((cells.len() != others.len()).then_some(0)) {
        let at = |c: &[u32]| c.get(i).map(|v| f32::from_bits(*v));
        return format!("cell {i}: one thread {:?}, several {:?}", at(cells), at(others));
    }
    let at = a.bytes().zip(b.bytes()).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    let around = |t: &str| t.get(at.saturating_sub(60)..(at + 60).min(t.len())).unwrap_or("").to_string();
    format!("at byte {at}: one thread «{}», several «{}»", around(a), around(b))
}

#[test]
fn several_threads_give_what_one_thread_gives() {
    let seed = env("PROPERTY_SEED", 1);
    let mut bytes = [0u8; 32];
    bytes[..8].copy_from_slice(&seed.to_le_bytes());
    // About two seconds a case.
    let cases = u32::try_from(env("PROPERTY_CASES", 6)).unwrap_or(6);
    let config = Config { cases, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &bytes));
    let result = runner.run(&scene(), |s| {
        threads::set_max(Some(1)).unwrap();
        let one = outputs(&s);
        threads::set_max(Some(s.threads)).unwrap();
        let several = outputs(&s);
        threads::set_max(None).unwrap();
        let short = |r: &Made| match r {
            Ok((t, _)) => format!("made {}…", t.chars().take(120).collect::<String>()),
            Err(e) => format!("refused: {e}"),
        };
        for ((name, a), (_, b)) in one.iter().zip(&several) {
            match (a, b) {
                (Ok(x), Ok(y)) if x != y => {
                    return Err(TestCaseError::fail(format!("{name} on {} threads: {}", s.threads, first_difference(x, y))));
                }
                _ if a != b => {
                    return Err(TestCaseError::fail(format!("{name} on {} threads: one {}; several {}", s.threads, short(a), short(b))));
                }
                _ => {}
            }
        }
        Ok(())
    });
    threads::set_max(None).unwrap();
    if let Err(e) = result {
        panic!("PROPERTY_SEED={seed}: {e}");
    }
}
