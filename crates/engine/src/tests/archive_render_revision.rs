use crate::{data, map, render};
use data::source::{Batch, FeatureSource, Request, SourceInfo};
use data::{Dataset, Feature, Geometry, GeometryType};
use map::{Camera, Map, TileKind, TileLayer, source::SourceLayer};
use render::rustgis_tiles::{cache, pmtiles};
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "rustgis-render-revision-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("creating {}: {e}", path.display()),
            }
        }
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        data::mbtiles::close_unless(|path| !path.starts_with(&self.0));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A real streamed layer between two drawings of the archive. Complete traversal pauses only
/// after the lower drawing has copied its old pixels and credits; the upper drawing has not run.
struct PauseSource {
    info: SourceInfo,
    ds: Dataset,
    once: AtomicBool,
    paused: mpsc::SyncSender<()>,
    resume: Arc<Barrier>,
}
impl FeatureSource for PauseSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }
    fn read(&self, _: &Request) -> data::Result<Batch> {
        Ok(Batch { features: self.ds.features.to_vec(), matched: 1, complete: true, ..Default::default() })
    }
    fn streams_chunks(&self) -> bool {
        true
    }
    fn visit_chunks(
        &self,
        _: Option<data::Extent>,
        _: usize,
        visit: &mut dyn FnMut(Vec<Feature>) -> data::Result<()>,
    ) -> data::Result<Batch> {
        if !self.once.swap(true, Ordering::SeqCst) {
            self.paused.send(()).unwrap();
            self.resume.wait();
        }
        visit(self.ds.features.to_vec())?;
        Ok(Batch { matched: 1, complete: true, ..Default::default() })
    }
}

fn archive(path: &std::path::Path, kind: TileKind, green: bool) {
    let mut tile = tiny_skia::Pixmap::new(256, 256).unwrap();
    tile.fill(if green { tiny_skia::Color::from_rgba8(0, 220, 0, 255) } else { tiny_skia::Color::from_rgba8(220, 0, 0, 255) });
    let png = tile.encode_png().unwrap();
    let credits = if green { "New archive credits" } else { "Old archive credits" };
    if kind == TileKind::PmTiles {
        let options = pmtiles::NewArchive {
            tile_type: pmtiles::TileType::Png,
            gzipped_tiles: false,
            bounds: [-180.0, -85.0, 180.0, 85.0],
            metadata: serde_json::json!({"attribution": credits}),
        };
        std::fs::write(path, pmtiles::write(vec![(0, 0, 0, png)], &options).unwrap()).unwrap();
    } else {
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch("CREATE TABLE metadata(name TEXT,value TEXT); CREATE TABLE tiles(zoom_level INTEGER,tile_column INTEGER,tile_row INTEGER,tile_data BLOB); INSERT INTO metadata VALUES('format','png'),('minzoom','0'),('maxzoom','0');").unwrap();
        db.execute("INSERT INTO metadata VALUES('attribution',?1)", [credits]).unwrap();
        db.execute("INSERT INTO tiles VALUES(0,0,0,?1)", [png]).unwrap();
    }
}

fn paused_map(path: &std::path::Path, kind: TileKind) -> (Map, render::View, mpsc::Receiver<()>, Arc<Barrier>) {
    let mut map = Map::new("Archive");
    map.graticule = false;
    map.camera = Camera::new(data::Coord::new(0.0, 0.0), 100_000.0);
    map.tiles.push(TileLayer::new("Below", kind, path.to_string_lossy()));
    let view = render::View { width: 128.0, height: 128.0, pixels_per_point: 1.0, camera: map.camera, sky: false };
    let old = render::render_placed(&mut map, &view).unwrap();
    assert_eq!(old.credits, ["Old archive credits"]);
    assert!(old.pixmap.pixel(8, 8).unwrap().red() > 200);
    let mut ds = Dataset::new("Between", GeometryType::Polyline, Vec::new());
    ds.push(Some(Geometry::Line(vec![vec![data::Coord::new(0.0, 0.0), data::Coord::new(0.1, 0.1)]])), Vec::new());
    let info = SourceInfo {
        name: ds.name.clone(),
        format: "test",
        geometry_type: ds.geometry_type,
        fields: Vec::new(),
        extent: ds.extent(),
        count: Some(1),
        crs: "EPSG:4326".into(),
        indexed: true,
        notes: Vec::new(),
        zm_dropped: false,
    };
    let id = map.add_dataset(ds.clone());
    let (paused, receive) = mpsc::sync_channel(1);
    let resume = Arc::new(Barrier::new(2));
    map.layer_mut(id).unwrap().source = Some(SourceLayer::from_path(
        "pause.shp",
        None,
        Box::new(PauseSource { info, ds, once: AtomicBool::new(false), paused, resume: resume.clone() }),
    ));
    let mut above = map.tiles[0].clone();
    above.name = "Above".into();
    above.over = Some(id);
    map.tiles.insert(0, above);
    (map, view, receive, resume)
}

fn page(map: &Map) -> map::layout::Layout {
    use map::layout::{ElementKind, FrameShape, Layout};
    let mut page = Layout::new("Archive", 40.0, 40.0);
    page.add(
        "Map",
        (0.0, 0.0, 40.0, 40.0),
        ElementKind::MapFrame {
            map: map.name.clone(),
            center: data::Coord::new(0.0, 0.0),
            scale: 300_000_000.0,
            shape: FrameShape::Rectangle,
            grid: None,
            extent_of: None,
            extent_color: map::Rgba::rgb(0, 0, 0),
            extent_width: 1.0,
            bearing: 0.0,
            pitch: 0.0,
        },
    );
    page
}

#[test]
fn archive_reload_refuses_spanning_exports_without_replacing_destination() {
    for kind in [TileKind::PmTiles, TileKind::MbTiles] {
        for route in 0..6 {
            let dir = Fixture::new();
            let path = dir.path().join(format!("target.{}", kind.code()));
            let replacement = dir.path().join(format!("replacement.{}", kind.code()));
            archive(&path, kind, false);
            archive(&replacement, kind, true);
            let (mut map, view, paused, resume) = paused_map(&path, kind);
            let mut warm = map.clone();
            warm.layers.clear();
            warm.tiles.truncate(1);
            warm.tiles[0].over = None;
            let destination = dir.path().join("map.png");
            std::fs::write(&destination, b"existing destination").unwrap();
            let output = destination.clone();
            let worker = std::thread::spawn(move || {
                let result = match route {
                    0 => render::render_png(&mut map, &view),
                    1 => render::render_placed(&mut map, &view).and_then(|p| p.pixmap.encode_png().map_err(|e| e.to_string())),
                    2 => render::render_drawing(&mut map, &view)
                        .map(|drawing| render::pdf::write(&[render::pdf::Page::drawing(drawing, 40.0, 40.0)], "Archive")),
                    3 => render::layout::render(&page(&map), &mut [map], 96.0, &Default::default())
                        .and_then(|p| p.encode_png().map_err(|e| e.to_string())),
                    4 => render::layout::render_drawing(&page(&map), &mut [map], 96.0, &Default::default())
                        .map(|(drawing, _)| render::pdf::write(&[render::pdf::Page::drawing(drawing, 40.0, 40.0)], "Archive")),
                    _ => render::svg::layout_svg(&page(&map), &mut [map], 96.0, &Default::default()).map(|(svg, _)| svg.into_bytes()),
                };
                if let Ok(bytes) = &result {
                    data::io::write_atomic(&output, bytes).unwrap();
                }
                result
            });
            paused.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
            data::mbtiles::close(&path);
            std::fs::rename(&replacement, &path).unwrap();
            cache::reload_archive(path.to_str().unwrap());
            let fresh = render::render_placed(&mut warm, &view);
            resume.wait();
            let result = worker.join().unwrap();
            let fresh = fresh.unwrap();
            assert_eq!(fresh.credits, ["New archive credits"]);
            assert!(fresh.pixmap.pixel(8, 8).unwrap().green() > 200);
            let error = result.unwrap_err();
            assert!(error.contains("archive changed during rendering") && error.contains("export again"), "{error}");
            assert_eq!(std::fs::read(&destination).unwrap(), b"existing destination");
            data::mbtiles::close(&path);
        }
    }
}

#[test]
fn archive_revision_ignores_other_sources_and_tile_arrivals() {
    let dir = Fixture::new();
    let path = dir.path().join("target.pmtiles");
    archive(&path, TileKind::PmTiles, false);
    let mut map = Map::new("Revisions");
    map.tiles.push(TileLayer::new("Target", TileKind::PmTiles, path.to_string_lossy()));
    let revision = render::archive_revisions([&map]);
    cache::reload_archive("/render-revision-unrelated.mbtiles");
    assert!(revision.current([&map]));
    let arrived_before = cache::generation();
    let view = render::View { width: 32.0, height: 32.0, pixels_per_point: 1.0, camera: map.camera, sky: false };
    render::render_placed(&mut map, &view).unwrap();
    assert_ne!(cache::generation(), arrived_before);
    assert!(revision.current([&map]));
    cache::reload_archive(&map.tiles[0].url);
    assert!(!revision.current([&map]));
}

#[test]
fn a_spanning_render_keeps_progressive_arrivals_and_unrelated_reloads() {
    for kind in [TileKind::PmTiles, TileKind::MbTiles] {
        let dir = Fixture::new();
        let target = dir.path().join(format!("target.{}", kind.code()));
        let unrelated = dir.path().join(format!("other.{}", kind.code()));
        archive(&target, kind, false);
        archive(&unrelated, kind, true);
        let (mut map, view, paused, resume) = paused_map(&target, kind);
        let worker = std::thread::spawn(move || render::render_placed(&mut map, &view));
        paused.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
        cache::reload_archive(unrelated.to_str().unwrap());
        let mut other = Map::new("Unrelated");
        other.tiles.push(TileLayer::new("Other", kind, unrelated.to_string_lossy()));
        let arrived_before = cache::generation();
        let fresh = render::render_placed(&mut other, &view);
        resume.wait();
        let result = worker.join().unwrap().unwrap();
        fresh.unwrap();
        assert_ne!(cache::generation(), arrived_before);
        assert_eq!(result.credits, ["Old archive credits"]);
        assert!(result.pixmap.pixel(8, 8).unwrap().red() > 200);
        data::mbtiles::close(&target);
        data::mbtiles::close(&unrelated);
    }
}

#[test]
fn archive_reload_discards_spanning_cpu_gpu_and_drape_passes() {
    for kind in [TileKind::PmTiles, TileKind::MbTiles] {
        for route in 0..4 {
            let dir = Fixture::new();
            let path = dir.path().join(format!("target.{}", kind.code()));
            let replacement = dir.path().join(format!("replacement.{}", kind.code()));
            archive(&path, kind, false);
            archive(&replacement, kind, true);
            let (mut map, view, paused, resume) = paused_map(&path, kind);
            let mut warm = map.clone();
            warm.layers.clear();
            warm.tiles.truncate(1);
            warm.tiles[0].over = None;
            let worker = std::thread::spawn(move || match route {
                0 => render::render_backend(&mut map, &view, render::Backend::Cpu).is_none(),
                1 => render::render_backend(&mut map, &view, render::Backend::Gpu).is_none(),
                2 => {
                    let view = render::View { camera: Camera { pitch: 30.0, ..view.camera }, ..view };
                    render::render_tilt_drape(&mut map, &view, 0.0, 128).is_empty()
                }
                _ => render::render_globe_drape(&mut map, &view, &[data::Extent::new(-10.0, -10.0, 10.0, 10.0)], 128).is_empty(),
            });
            paused.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
            data::mbtiles::close(&path);
            std::fs::rename(&replacement, &path).unwrap();
            cache::reload_archive(path.to_str().unwrap());
            let fresh = render::render_placed(&mut warm, &view);
            resume.wait();
            let refused = worker.join().unwrap();
            fresh.unwrap();
            assert!(refused, "mixed archive revision escaped route {route} ({kind:?})");
            data::mbtiles::close(&path);
        }
    }
}

#[test]
fn a_reload_between_series_pages_refuses_the_engine_pdf_without_replacing_destination() {
    struct BetweenPages {
        pages: std::sync::atomic::AtomicU64,
        paused: mpsc::SyncSender<()>,
        resume: Arc<Barrier>,
    }
    impl data::progress::Probe for BetweenPages {
        fn cancelled(&self) -> bool {
            false
        }
        fn begin(&self, _: u64) {}
        fn advance(&self, n: u64) {
            if self.pages.fetch_add(n, Ordering::SeqCst) == 0 {
                self.paused.send(()).unwrap();
                self.resume.wait();
            }
        }
    }

    for kind in [TileKind::PmTiles, TileKind::MbTiles] {
        let dir = Fixture::new();
        let path = dir.path().join(format!("target.{}", kind.code()));
        let replacement = dir.path().join(format!("replacement.{}", kind.code()));
        archive(&path, kind, false);
        archive(&replacement, kind, true);
        let mut map = Map::new("Archive");
        map.graticule = false;
        map.camera = Camera::new(data::Coord::new(0.0, 0.0), 100_000.0);
        map.tiles.push(TileLayer::new("Archive", kind, path.to_string_lossy()));
        let mut index = Dataset::new("Pages", GeometryType::Point, Vec::new());
        for x in [0.0, 1.0] {
            index.push(Some(Geometry::Point(data::Coord::new(x, 0.0))), Vec::new());
        }
        map.add_dataset(index);
        let layout = page(&map);
        let mut warm = map.clone();
        warm.layers.clear();
        let view = render::View { width: 32.0, height: 32.0, pixels_per_point: 1.0, camera: map.camera, sky: false };
        let old = render::render_placed(&mut warm, &view).unwrap();
        assert_eq!(old.credits, ["Old archive credits"], "{:?}", old.gaps);
        let mut session = crate::Session::new(crate::Project::default());
        session.project.maps = vec![map];
        session.project.layouts = vec![layout];
        session.active_layout = Some(0);
        let series = session.run("layout.series", &serde_json::json!({"layer":"Pages"})).unwrap();
        assert_eq!(series["pages"], 2);
        let destination = dir.path().join("series.pdf");
        std::fs::write(&destination, b"existing destination").unwrap();
        // The command's analysis wrapper installs its own progress carrier. Run its actual
        // export work under this probe to pause exactly between complete pages, without a hook.
        let work = crate::cmd::layout::export_work(&session, &serde_json::json!({"path":destination,"dpi":36,"all":true})).unwrap();
        let (paused, receive) = mpsc::sync_channel(1);
        let resume = Arc::new(Barrier::new(2));
        let probe = Arc::new(BetweenPages { pages: std::sync::atomic::AtomicU64::new(0), paused, resume: resume.clone() });
        let source = path.clone();
        let reloaded = std::thread::spawn(move || {
            receive.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
            // The layout's page-progress callback runs after page one is complete and before
            // page two captures its own render revision: only the whole-document guard spans this.
            data::mbtiles::close(&source);
            std::fs::rename(replacement, &source).unwrap();
            cache::reload_archive(source.to_str().unwrap());
            let fresh = render::render_placed(&mut warm, &view);
            resume.wait();
            fresh
        });
        let installed = data::progress::install(Some(probe.clone()));
        let wrote = work().unwrap();
        drop(installed);
        let fresh = reloaded.join().unwrap().unwrap();
        assert_eq!(fresh.credits, ["New archive credits"]);
        assert!(fresh.pixmap.pixel(8, 8).unwrap().green() > 200);
        assert_eq!(probe.pages.load(Ordering::SeqCst), 2, "both individually coherent pages were drawn");
        let error = wrote.then.unwrap()(&mut session, &mut serde_json::Value::Null).unwrap_err();
        assert!(error.contains("archive changed during rendering"), "{error}");
        assert_eq!(std::fs::read(&destination).unwrap(), b"existing destination");
        session.run("layout.export", &serde_json::json!({"path":destination,"dpi":36,"all":true})).unwrap();
        let pdf = std::fs::read(&destination).unwrap();
        assert!(pdf.starts_with(b"%PDF") && pdf.windows(b"/Count 2".len()).any(|part| part == b"/Count 2"));
    }
}
