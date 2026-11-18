//! Datum grids downloaded when the user turns that on (#2032 A4): a read on a thread of its own
//! waits for the grids its system needs, keeps them in RustGIS's grid folder, and transforms with
//! them, as PROJ does with them; what was downloaded is said. Its own test binary: grids are
//! process-wide, and the other tests' reference values are those without grids. No network: the
//! "server" hands out the PROJ-data grids in `rustgis-data`'s test data.
//!
//! References: PROJ 9.8.1 (pyproj 3.8.0), network off, EPSG:32040 (Texas South Central, NAD27,
//! US feet) to EPSG:4326 at (2000000, 500000): without grids (-99.00033704176887,
//! 29.208750090065326); with `us_noaa_conus.tif` and `us_noaa_ethpgn.tif`
//! (-99.00031395482138, 29.208749002954207).

use rustgis_data::Coord;
use rustgis_data::raster::{Crs, crs};
use rustgis_engine::grids::{self, News};
use rustgis_engine::{Project, Session};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

static ASKED: AtomicUsize = AtomicUsize::new(0);

/// How the server answers: with the grid (or 404), with an error page, or slowly.
static MODE: AtomicU8 = AtomicU8::new(GRIDS);
const GRIDS: u8 = 0;
const PAGE: u8 = 1;
const SLOW: u8 = 2;

/// PROJ's grid server, as far as these grids go.
fn server(url: &str, _max: usize, _timeout: std::time::Duration) -> Result<Vec<u8>, String> {
    ASKED.fetch_add(1, Ordering::SeqCst);
    match MODE.load(Ordering::SeqCst) {
        // A captive portal's answer: a page, not a grid.
        PAGE => return Ok(b"<html><body>Sign in to the network</body></html>".to_vec()),
        SLOW => std::thread::sleep(std::time::Duration::from_secs(3)), // test-wait: a download slower than the read is willing to wait while cancelled.
        _ => {}
    }
    let name = url.strip_prefix("https://cdn.proj.org/").ok_or_else(|| format!("not PROJ's grid server: {url}"))?;
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../data/tests/data/grids/");
    std::fs::read(format!("{dir}{name}")).map_err(|_| "HTTP 404".to_string())
}

/// Removes a test's folder when dropped, a failed assertion included.
struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_read_waits_for_the_grids_its_system_needs() {
    let root = std::env::temp_dir().join(format!("rustgis-grid-downloads-{}", std::process::id()));
    // What a run with this process id that panicked may have left.
    let _ = std::fs::remove_dir_all(&root);
    // And gone however the test ends.
    let _scratch = Scratch(root.clone());
    // Only a folder of the test's own: none from this computer (PROJ's).
    crs::set_grid_folders(vec![root.join("datum-grids")]);
    grids::set_download(server);
    let at = Coord::new(2_000_000.0, 500_000.0);
    let texas = Crs::Epsg(32040);
    let close = |c: Coord, (x, y): (f64, f64)| (c.x - x).abs() < 1e-11 && (c.y - y).abs() < 1e-11;

    // Off (as it starts): nothing is downloaded, and a read goes on with the next transformation.
    let t = crs::waiting_for_grids(|| texas.transformer()).unwrap();
    assert!(close(t.to_lonlat(at).unwrap(), (-99.00033704176887, 29.208750090065326)));
    assert_eq!(ASKED.load(Ordering::SeqCst), 0);

    // On: the read gets both grids first, keeps them, and its features land where they put them.
    grids::set_enabled(true);
    let t = crs::waiting_for_grids(|| texas.transformer()).unwrap();
    assert!(close(t.to_lonlat(at).unwrap(), (-99.00031395482138, 29.208749002954207)), "{:?}", t.to_lonlat(at));
    assert_eq!(ASKED.load(Ordering::SeqCst), 2);
    let dir = crs::grid_dir().unwrap();
    assert!(dir.starts_with(&root));
    for name in ["us_noaa_conus.tif", "us_noaa_ethpgn.tif"] {
        assert!(dir.join(name).is_file(), "{name}");
    }
    let mut news = grids::take_news();
    news.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    assert_eq!(
        news,
        [News::Got { name: "us_noaa_conus.tif".into(), bytes: 173_029 }, News::Got { name: "us_noaa_ethpgn.tif".into(), bytes: 13_442 }]
    );
    assert!(crs::missing_grids(texas).is_empty());
    // Read again, nothing more is asked for.
    let _ = crs::waiting_for_grids(|| texas.transformer()).unwrap();
    assert_eq!(ASKED.load(Ordering::SeqCst), 2);

    // Off the read's thread (the window's), a system's grids aren't waited for.
    // EPSG:4267 is asked about in the middle of its area, in Oregon, where PROJ's best operation
    // with `us_noaa_conus.tif` there (NAD27 to WGS 84 (68)) needs the HARN grid `us_noaa_WO.tif`.
    assert!(crs::missing_grids(Crs::Epsg(4267)).iter().any(|g| g.name == "us_noaa_WO.tif"));
    let _ = Crs::Epsg(4267).transformer().unwrap();
    assert_eq!(ASKED.load(Ordering::SeqCst), 2);

    // A grid the server doesn't have is said, and not asked for again this run.
    let _ = crs::waiting_for_grids(|| Crs::Epsg(4267).transformer()).unwrap();
    let asked = ASKED.load(Ordering::SeqCst);
    assert!(asked > 2);
    let failed: Vec<News> = grids::take_news();
    assert!(
        failed.iter().any(|n| matches!(n, News::Failed { name, error } if name == "us_noaa_WO.tif" && error == "HTTP 404")),
        "{failed:?}"
    );
    let said = grids::news_texts(&failed);
    assert!(said.iter().any(|(text, error)| *error && text.contains("couldn't be downloaded")), "{said:?}");
    let _ = crs::waiting_for_grids(|| Crs::Epsg(4267).transformer()).unwrap();
    assert_eq!(ASKED.load(Ordering::SeqCst), asked);

    // An answer that isn't a grid file (an error page) isn't kept: it is said, and the grid stays
    // missing. Turning the setting on again tries this run's failures again.
    MODE.store(PAGE, Ordering::SeqCst);
    grids::set_enabled(true);
    let _ = crs::waiting_for_grids(|| Crs::Epsg(4267).transformer()).unwrap();
    let page = grids::take_news();
    assert!(
        page.iter().any(|n| matches!(n, News::Failed { name, error } if name == "us_noaa_WO.tif" && error.contains("isn't a grid file"))),
        "{page:?}"
    );
    assert!(!dir.join("us_noaa_WO.tif").exists());
    assert!(crs::missing_grids(Crs::Epsg(4267)).iter().any(|g| g.name == "us_noaa_WO.tif"));

    // A read that is cancelled stops waiting at once; the download goes on, and the session says
    // what it did once it ends.
    MODE.store(SLOW, Ordering::SeqCst);
    grids::set_enabled(true);
    let control = rustgis_analysis::control::Control::stoppable();
    let stop = control.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300)); // test-wait: cancel while the read waits for the slow download.
        stop.cancel();
    });
    let started = std::time::Instant::now();
    let read = rustgis_analysis::run_under("Add Data", &control, || Ok(crs::waiting_for_grids(|| Crs::Epsg(4267).transformer())));
    canceller.join().unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
    assert!(read.is_err(), "a cancelled read says so");
    // Every session of the process says it (two engines in one Python process, say), each once.
    let mut session = Session::new(Project::default());
    session.offers_grid_downloads = true;
    let mut other = Session::new(Project::default());
    let said = |s: &Session| {
        s.notices.iter().filter(|n| n.error && n.text.contains("us_noaa_WO.tif") && n.text.contains("couldn't be downloaded")).count()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while said(&session) == 0 && std::time::Instant::now() < deadline {
        session.notice_landed_rasters();
        std::thread::sleep(std::time::Duration::from_millis(50)); // test-wait: the slow download ends on its own thread.
    }
    assert_eq!(said(&session), 1, "{:?}", session.notices.iter().map(|n| &n.text).collect::<Vec<_>>());
    other.notice_landed_rasters();
    other.notice_landed_rasters();
    assert_eq!(said(&other), 1, "{:?}", other.notices.iter().map(|n| &n.text).collect::<Vec<_>>());

    // Off again: the grids already downloaded stay in use, and nothing more is asked for.
    grids::set_enabled(false);
    MODE.store(GRIDS, Ordering::SeqCst);
    let asked = ASKED.load(Ordering::SeqCst);
    let t = crs::waiting_for_grids(|| texas.transformer()).unwrap();
    assert!(close(t.to_lonlat(at).unwrap(), (-99.00031395482138, 29.208749002954207)));
    let _ = crs::waiting_for_grids(|| Crs::Epsg(4267).transformer()).unwrap();
    assert_eq!(ASKED.load(Ordering::SeqCst), asked);

    // The note about a missing grid points to the setting where the front end has it (the desktop
    // and web apps); the command line and Python, which have none, don't name it.
    for offers in [true, false] {
        let mut s = Session::new(Project::default());
        s.offers_grid_downloads = offers;
        let mut ds = rustgis_data::Dataset::new("nad27", rustgis_data::GeometryType::Point, Vec::new());
        ds.crs = Some(Crs::Epsg(4267));
        rustgis_engine::cmd::layer::add_dataset(&mut s, ds);
        s.notice_landed_rasters();
        let note = s.notices.iter().find(|n| n.text.contains("EPSG:4267")).map(|n| n.text.clone()).unwrap_or_default();
        assert!(note.contains("needs a grid"), "{note}");
        assert_eq!(note.contains("Settings › Datum Grids › Download Datum Grids"), offers, "{note}");
    }

    // And the other way: a system whose grids are all there, the setting on, says nothing of them.
    grids::set_enabled(true);
    let mut s = Session::new(Project::default());
    s.offers_grid_downloads = true;
    let mut ds = rustgis_data::Dataset::new("texas", rustgis_data::GeometryType::Point, Vec::new());
    ds.crs = Some(texas);
    rustgis_engine::cmd::layer::add_dataset(&mut s, ds);
    s.notice_landed_rasters();
    assert!(!s.notices.iter().any(|n| n.text.contains("grid")), "{:?}", s.notices.iter().map(|n| &n.text).collect::<Vec<_>>());
    grids::set_enabled(false);

    // A download is said once, however often the session asks while it is under way.
    MODE.store(SLOW, Ordering::SeqCst);
    grids::set_enabled(true);
    let mut s = Session::new(Project::default());
    let mut ds = rustgis_data::Dataset::new("nad27", rustgis_data::GeometryType::Point, Vec::new());
    ds.crs = Some(Crs::Epsg(4267));
    rustgis_engine::cmd::layer::add_dataset(&mut s, ds);
    assert!(s.want_grids_in_use().contains(&"us_noaa_WO.tif".to_string()));
    assert!(s.want_grids_in_use().contains(&"us_noaa_WO.tif".to_string()), "still coming");
    let downloading = s.notices.iter().filter(|n| n.text.starts_with("Downloading the datum grids")).count();
    assert_eq!(downloading, 1, "{:?}", s.notices.iter().map(|n| &n.text).collect::<Vec<_>>());
    // A session started with the setting already on (as after a restart): a download its
    // missing-grid note names isn't announced again when it asks for the grids itself.
    let mut fresh = Session::new(Project::default());
    fresh.offers_grid_downloads = true;
    let mut ds = rustgis_data::Dataset::new("nad27", rustgis_data::GeometryType::Point, Vec::new());
    ds.crs = Some(Crs::Epsg(4267));
    rustgis_engine::cmd::layer::add_dataset(&mut fresh, ds);
    fresh.notice_landed_rasters();
    assert!(
        fresh.notices.iter().any(|n| n.text.contains("RustGIS is downloading") && n.text.contains("us_noaa_WO.tif")),
        "{:?}",
        fresh.notices.iter().map(|n| &n.text).collect::<Vec<_>>()
    );
    let _ = fresh.want_grids_in_use();
    assert!(
        !fresh.notices.iter().any(|n| n.text.starts_with("Downloading the datum grids")),
        "{:?}",
        fresh.notices.iter().map(|n| &n.text).collect::<Vec<_>>()
    );
    // Its end, before the folder goes.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !grids::take_news().iter().any(|n| matches!(n, News::Failed { name, .. } if name == "us_noaa_WO.tif"))
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50)); // test-wait: the slow download ends on its own thread.
    }
    grids::set_enabled(false);
    MODE.store(GRIDS, Ordering::SeqCst);
}
