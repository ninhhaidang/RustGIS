use super::*;
use crate::source::{Batch, Request, SourceInfo, open_path};
use crate::{Coord, Field, FieldType, Geometry, GeometryType, Value};
use std::path::PathBuf;

/// A new, empty folder for one test's files.
fn folder(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rustgis-export-tests-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Squares over Europe with every kind of attribute, and a row without geometry.
fn squares() -> Dataset {
    let fields = vec![
        Field::new("name", FieldType::Text),
        Field::new("count", FieldType::Integer),
        Field::new("share", FieldType::Double),
        Field::new("open", FieldType::Boolean),
    ];
    let mut ds = Dataset::new("squares", GeometryType::Polygon, fields);
    for i in 0..300 {
        let (x, y) = (-10.0 + (i % 30) as f64, 40.0 + (i / 30) as f64);
        let ring = vec![Coord::new(x, y), Coord::new(x + 0.5, y), Coord::new(x + 0.5, y + 0.5), Coord::new(x, y + 0.5), Coord::new(x, y)];
        let name = if i % 7 == 0 { Value::Null } else { Value::Text(format!("square {i}")) };
        let attrs = vec![name, Value::Int(i), Value::Float(i as f64 / 3.0), Value::Bool(i % 2 == 0)];
        ds.push_next(Some(Geometry::Polygon(vec![vec![ring]])), attrs);
    }
    ds.push_next(None, vec![Value::Text("no geometry".into()), Value::Int(-1), Value::Null, Value::Null]);
    ds
}

/// What a file holds, read back whole.
fn read_back(path: &Path) -> Dataset {
    crate::io::read_path(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())).0
}

/// A layer's features handed over `per` at a time, whatever the byte budget: for exports whose
/// later features contradict what the first ones suggest.
struct Runs {
    info: SourceInfo,
    features: Vec<Feature>,
    per: usize,
    /// Fails the traversal on its run numbered `fail_after` (counted from 0), on the traversal
    /// numbered `fail_pass` if set, or on every traversal.
    fail_after: Option<usize>,
    fail_pass: Option<usize>,
    passes: std::sync::atomic::AtomicUsize,
    /// Whether it can be read a chunk at a time.
    chunks: bool,
    /// Rows it reports as unreadable, as a GeoPackage's bad rows are.
    unreadable: usize,
}

impl Runs {
    /// A source of `ds`'s features, `per` a run, that can be read a chunk at a time.
    fn new(ds: &Dataset, per: usize) -> Self {
        let info = SourceInfo {
            name: ds.name.clone(),
            format: "test",
            geometry_type: ds.geometry_type,
            fields: ds.fields.clone(),
            extent: ds.extent(),
            count: Some(ds.features.len() as u64),
            crs: "EPSG:4326".into(),
            indexed: false,
            notes: Vec::new(),
            zm_dropped: false,
        };
        Runs {
            info,
            features: ds.features.to_vec(),
            per,
            fail_after: None,
            fail_pass: None,
            passes: Default::default(),
            chunks: true,
            unreadable: 0,
        }
    }
}

impl FeatureSource for Runs {
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn read(&self, _: &Request) -> Result<Batch> {
        Ok(Batch { features: self.features.clone(), matched: self.features.len() as u64, complete: true, skipped: 0 })
    }
    fn visits_chunks(&self) -> bool {
        self.chunks
    }
    fn visit_chunks(&self, _: Option<rustgis_geom::Extent>, _: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        let pass = self.passes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        for (n, run) in self.features.chunks(self.per).enumerate() {
            if self.fail_after == Some(n) && self.fail_pass.is_none_or(|p| p == pass) {
                return Err(DataError::Format("the source failed".into()));
            }
            visit(run.to_vec())?;
        }
        Ok(Batch { complete: true, matched: self.features.len() as u64, skipped: self.unreadable, ..Batch::default() })
    }
}

/// Every streamed format, from a source read in place a few features a chunk, holds what the
/// same export of the whole layer holds, in longitude/latitude and in a projected system.
#[test]
fn a_streamed_export_writes_what_a_whole_export_writes() {
    let d = folder("parity");
    let ds = squares();
    let mut sources: Vec<Box<dyn FeatureSource>> = Vec::new();
    for ext in ["gpkg", "fgb", "parquet", "shp"] {
        let p = d.join(format!("source.{ext}"));
        crate::io::write_path(&ds, &p).unwrap();
        sources.push(open_path(&p, None).unwrap());
    }
    for src in &sources {
        let (whole, _) = crate::source::materialize(src.as_ref()).unwrap();
        let mut layer = Dataset::new(whole.name.clone(), whole.geometry_type, whole.fields.clone());
        layer.crs = whole.crs;
        let format = src.info().format;
        for ext in STREAMED {
            let systems: &[Crs] = if PROJECTED_FORMATS.contains(&ext) { &[Crs::LonLat, Crs::WebMercator] } else { &[Crs::LonLat] };
            for &crs in systems {
                let (a, b) =
                    (d.join(format!("{format}-streamed-{}.{ext}", crs.epsg())), d.join(format!("{format}-whole-{}.{ext}", crs.epsg())));
                let streamed = write_chunks_in(src.as_ref(), &layer, &a, crs, 2048).unwrap();
                let left_out = crate::io::write_path_in(&whole, &b, crs).unwrap();
                let what = format!("{format} to .{ext} in {}", crs.label());
                assert_eq!(streamed, Exported { written: whole.features.len() - left_out, left_out, skipped: 0 }, "{what}");
                let (a, b) = (read_back(&a), read_back(&b));
                assert_eq!((&a.fields, a.geometry_type, a.crs), (&b.fields, b.geometry_type, b.crs), "{what}");
                assert_eq!(a.features, b.features, "{what}");
                assert!(!a.features.is_empty(), "{what}");
            }
        }
    }
}

/// A GeoPackage export decides its table from the features so far and starts again when a later
/// one contradicts it, giving the file a whole export gives.
#[test]
fn a_streamed_geopackage_starts_again_when_later_features_need_another_table() {
    let d = folder("redo");
    let fields = vec![Field::new("n", FieldType::Integer)];
    // Points, then a multipoint: a MULTIPOINT table.
    let mut points = Dataset::new("points", GeometryType::Point, fields.clone());
    for i in 0..20 {
        let c = Coord::new(i as f64, 1.0);
        let g = if i == 15 { Geometry::MultiPoint(vec![c, Coord::new(i as f64, 2.0)]) } else { Geometry::Point(c) };
        points.push_next(Some(g), vec![Value::Int(i)]);
    }
    // An id used twice: SQLite numbers every row, as for the whole layer.
    let mut repeated = Dataset::new("repeated", GeometryType::Point, fields);
    for i in 0..20 {
        repeated.push_next(Some(Geometry::Point(Coord::new(i as f64, 1.0))), vec![Value::Int(i)]);
    }
    let first = repeated.features.first().map(|f| f.id).unwrap();
    if let Some(f) = repeated.features.get_mut(12) {
        f.id = first;
    }
    // Both: a multipoint, then a repeated id, so the export is written three times.
    let mut both = points.clone();
    both.name = "both".into();
    let first = both.features.first().map(|f| f.id).unwrap();
    if let Some(f) = both.features.get_mut(18) {
        f.id = first;
    }
    for ds in [points, repeated, both] {
        let (a, b) = (d.join(format!("{}-streamed.gpkg", ds.name)), d.join(format!("{}-whole.gpkg", ds.name)));
        let layer = Dataset::new(ds.name.clone(), ds.geometry_type, ds.fields.clone());
        write_chunks_in(&Runs::new(&ds, 4), &layer, &a, Crs::LonLat, 1).unwrap();
        crate::io::write_path(&ds, &b).unwrap();
        let types = |p: &Path| {
            let c = rusqlite::Connection::open(p).unwrap();
            c.query_row("SELECT geometry_type_name FROM gpkg_geometry_columns", [], |r| r.get::<_, String>(0)).unwrap()
        };
        assert_eq!(types(&a), types(&b), "{}", ds.name);
        assert_eq!(read_back(&a).features, read_back(&b).features, "{}", ds.name);
    }
}

/// A GeoPackage export whose source fails as it starts again leaves no file behind, from either
/// pass.
#[test]
fn a_geopackage_export_failing_as_it_starts_again_leaves_nothing_behind() {
    let d = folder("redo-failed");
    let mut points = Dataset::new("points", GeometryType::Point, vec![]);
    for i in 0..20 {
        let c = Coord::new(i as f64, 1.0);
        points.push_next(Some(if i == 15 { Geometry::MultiPoint(vec![c]) } else { Geometry::Point(c) }), vec![]);
    }
    let layer = Dataset::new("points", GeometryType::Point, vec![]);
    let mut src = Runs::new(&points, 4);
    (src.fail_after, src.fail_pass) = (Some(2), Some(1));
    assert!(write_chunks_in(&src, &layer, &d.join("points.gpkg"), Crs::LonLat, 1).is_err());
    assert_eq!(src.passes.load(std::sync::atomic::Ordering::Relaxed), 2, "the multipoint starts the export again, which then fails");
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
}

/// A layer that declares no geometry type is written with its first geometry's, as a whole read
/// gives it: a point layer's CSV has coordinate columns.
#[test]
fn a_layer_without_a_declared_type_takes_its_first_geometrys() {
    let d = folder("untyped");
    let mut points = Dataset::new("points", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    points.push_next(None, vec![Value::Int(0)]);
    for i in 1..5 {
        points.push_next(Some(Geometry::Point(Coord::new(i as f64, 2.0))), vec![Value::Int(i)]);
    }
    let layer = Dataset::new("points", GeometryType::None, points.fields.clone());
    write_chunks_in(&Runs::new(&points, 2), &layer, &d.join("points.csv"), Crs::LonLat, 1).unwrap();
    let text = std::fs::read_to_string(d.join("points.csv")).unwrap();
    assert!(text.starts_with("n,longitude,latitude"), "{text}");
}

/// Rows a source couldn't read are counted, so an export can say how many it left out; a clean
/// source counts none.
#[test]
fn rows_a_source_couldnt_read_are_counted() {
    let d = folder("unreadable");
    let ds = squares();
    let layer = Dataset::new("squares", ds.geometry_type, ds.fields.clone());
    let mut src = Runs::new(&ds, 40);
    let clean = write_chunks_in(&src, &layer, &d.join("clean.csv"), Crs::LonLat, 2048).unwrap();
    assert_eq!(clean.skipped, 0);
    src.unreadable = 3;
    let lossy = write_chunks_in(&src, &layer, &d.join("lossy.csv"), Crs::LonLat, 2048).unwrap();
    assert_eq!(lossy.skipped, 3);
    assert_eq!(lossy.written, clean.written);
}

/// A failed export leaves no file, and no temporary one, behind; a file it would have replaced
/// stays as it was.
#[test]
fn a_failed_streamed_export_leaves_nothing_behind() {
    let d = folder("failed");
    let ds = squares();
    let layer = Dataset::new(ds.name.clone(), ds.geometry_type, ds.fields.clone());
    let mut src = Runs::new(&ds, 10);
    src.fail_after = Some(3);
    std::fs::write(d.join("kept.geojson"), "{}").unwrap();
    for name in ["new.gpkg", "new.fgb", "new.parquet", "new.csv", "kept.geojson"] {
        let path = d.join(name);
        assert!(write_chunks_in(&src, &layer, &path, Crs::LonLat, 1).is_err(), "{name}");
    }
    let mut left: Vec<String> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, vec!["kept.geojson".to_string()]);
    assert_eq!(std::fs::read_to_string(d.join("kept.geojson")).unwrap(), "{}");
}

/// Formats written from a whole layer, systems a format can't say, and sources that can't be
/// read a chunk at a time are refused before anything is written.
#[test]
fn streamed_exports_refuse_what_they_cant_write() {
    let d = folder("refused");
    let ds = squares();
    let layer = Dataset::new(ds.name.clone(), ds.geometry_type, ds.fields.clone());
    let src = Runs::new(&ds, 10);
    for (name, crs) in
        [("out.shp", Crs::LonLat), ("out.kml", Crs::LonLat), ("out.geojson", Crs::WebMercator), ("out.parquet", Crs::WebMercator)]
    {
        assert!(write_source_in(&src, &layer, &d.join(name), crs).is_err(), "{name}");
    }
    let whole_only = Runs { chunks: false, ..Runs::new(&ds, 10) };
    assert!(write_source_in(&whole_only, &layer, &d.join("out.csv"), Crs::LonLat).is_err());
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
}

/// A writer that panics leaves no temporary file behind either.
#[test]
fn a_panicking_writer_leaves_nothing_behind() {
    let d = folder("panic");
    let path = d.join("out.csv");
    let r = std::panic::catch_unwind(|| crate::io::write_atomic_with(&path, |_| -> Result<()> { panic!("the writer failed") }));
    assert!(r.is_err());
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
}

/// Streamed into an existing GeoPackage, an export that starts again (a multipoint, then a
/// repeated id) rolls back what it wrote and writes the table once, as a whole export would; one
/// whose source fails leaves the file's bytes as they were.
#[test]
fn a_streamed_export_into_an_existing_geopackage_starts_again_inside_its_transaction() {
    let d = folder("redo-existing");
    let county = d.join("county.gpkg");
    let mut parcels = Dataset::new("parcels", GeometryType::Point, vec![]);
    parcels.push_next(Some(Geometry::Point(Coord::new(5.0, 5.0))), vec![]);
    crate::io::write_path(&parcels, &county).unwrap();
    let fields = vec![Field::new("n", FieldType::Integer)];
    let mut both = Dataset::new("both", GeometryType::Point, fields.clone());
    for i in 0..20 {
        let c = Coord::new(i as f64, 1.0);
        let g = if i == 15 { Geometry::MultiPoint(vec![c, Coord::new(i as f64, 2.0)]) } else { Geometry::Point(c) };
        both.push_next(Some(g), vec![Value::Int(i)]);
    }
    let first = both.features.first().map(|f| f.id).unwrap();
    if let Some(f) = both.features.get_mut(18) {
        f.id = first;
    }
    let layer = Dataset::new("both", GeometryType::Point, fields);
    let add = GpkgTable { mode: crate::io::GpkgMode::AddTable, name: None };
    let src = Runs::new(&both, 4);
    let (e, table) = write_chunks_to(&src, &layer, &county, Crs::LonLat, &add, 1).unwrap();
    assert_eq!(src.passes.load(std::sync::atomic::Ordering::Relaxed), 3, "written three times");
    assert_eq!((e.written, table), (20, Some(GpkgWritten { table: "both".into(), replaced: false, dependents: vec![] })));
    let whole = d.join("whole.gpkg");
    crate::io::write_path(&both, &whole).unwrap();
    let read =
        |p: &Path, t: &str| crate::source::open_path(p, Some(t)).unwrap().read(&Request { extent: None, limit: 100 }).unwrap().features;
    assert_eq!(read(&county, "both"), read(&whole, "both"));
    assert_eq!(read(&county, "parcels").len(), 1);
    let c = rusqlite::Connection::open(&county).unwrap();
    let kind: String =
        c.query_row("SELECT geometry_type_name FROM gpkg_geometry_columns WHERE table_name = 'both'", [], |r| r.get(0)).unwrap();
    assert_eq!(kind, "MULTIPOINT");
    drop(c);
    // A source failing on the second pass: nothing of the table is left.
    let before = std::fs::read(&county).unwrap();
    let mut layer2 = layer.clone();
    layer2.name = "again".into();
    let mut failing = Runs::new(&both, 4);
    (failing.fail_after, failing.fail_pass) = (Some(2), Some(1));
    assert!(write_chunks_to(&failing, &layer2, &county, Crs::LonLat, &add, 1).is_err());
    assert_eq!(std::fs::read(&county).unwrap(), before);
}
