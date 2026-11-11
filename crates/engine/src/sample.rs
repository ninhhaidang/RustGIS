//! The built-in sample project: a world map from Natural Earth (public domain) data.

use rustgis_data::io::read_geojson;
use rustgis_geom::{Coord, lonlat_to_merc};
use rustgis_map::{Bookmark, Camera, ClassMethod, Marker, Ramp, Renderer, Rgba, Symbol};

use crate::Project;

pub use rustgis_render::sample_data::{
    CITIES, COUNTRIES, Detail, LAKES, RIVERS, View, Worldview, countries, dataset, install, set_view, text, view, worldview,
};

/// The worldview this system calls for: `RUSTGIS_WORLDVIEW` (a worldview's code, such as `ind`)
/// when set, else China when the system's time zone or first locale is mainland China's. Nothing
/// is looked up over the network. Test builds of the engine don't look at the system.
pub fn system_worldview() -> Worldview {
    if let Some(w) = std::env::var("RUSTGIS_WORLDVIEW").ok().and_then(|v| Worldview::from_code(&v)) {
        return w;
    }
    if cfg!(test) {
        return Worldview::DEFAULT;
    }
    let tz = iana_time_zone::get_timezone().ok();
    let locales: Vec<String> = sys_locale::get_locales().take(4).collect();
    Worldview::detect(tz.as_deref(), locales.iter().map(String::as_str))
}

/// The worldview a preference asks for: a worldview's code, or the system's for `auto` (or
/// anything unknown).
pub fn wanted_worldview(pref: &str) -> Worldview {
    Worldview::from_code(pref).unwrap_or_else(system_worldview)
}

/// The view the preferences ask for: the worldview's (see [`wanted_worldview`]) at the detail
/// `detail` names (`original` or `simplified`; anything else is the default, simplified).
pub fn wanted_view(pref: &str, detail: &str) -> View {
    View::new(wanted_worldview(pref), Detail::from_code(detail).unwrap_or_default())
}

/// Applies the worldview preferences (see [`wanted_view`]) and returns the view asked for. It
/// shows only once its countries are in memory; until then the current one stays, and the caller
/// gets them ([`fetch`], [`download_blocking`]). Nothing is read from disk here.
pub fn apply_view(pref: &str, detail: &str) -> View {
    let v = wanted_view(pref, detail);
    if text(v).is_some() {
        set_view(v);
    }
    v
}

/// Whether downloaded worldviews are kept in the cache folder and read back from it (not in the
/// engine's own tests, which mustn't touch the computer's cache).
static KEEP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(!cfg!(test));

/// Turns keeping downloaded worldviews in the cache folder on or off (off for tests).
pub fn keep_downloads(on: bool) {
    KEEP.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Where a downloaded view is kept: `worldviews/countries_<code>.geojson` (simplified) or
/// `worldviews/original/countries_<code>.geojson` in the cache folder (none in the browser).
fn kept_path(v: View) -> Option<std::path::PathBuf> {
    if v.built_in() || !KEEP.load(std::sync::atomic::Ordering::Relaxed) {
        return None;
    }
    rustgis_data::dirs::cache_dir().map(|d| d.join("worldviews").join(v.file()))
}

/// Reads a view kept from an earlier download into memory. A kept file that isn't usable (too
/// big, not text, not countries) is deleted, so it's downloaded again.
fn load_kept(v: View) -> bool {
    let Some(path) = kept_path(v) else { return false };
    let Ok(meta) = std::fs::metadata(&path) else { return false };
    let usable = meta.len() <= v.max_bytes() as u64
        && std::fs::read(&path).ok().and_then(|b| String::from_utf8(b).ok()).is_some_and(|t| install(v, t).is_ok());
    if !usable {
        log::warn!("{}: not a usable worldview, removed", path.display());
        let _ = std::fs::remove_file(&path);
    }
    usable
}

/// Whether a view's countries are at hand: built in, in memory, or kept from an earlier download
/// (read now, so callers on the UI thread use [`fetch`] instead).
pub fn ready(v: View) -> bool {
    text(v).is_some() || load_kept(v)
}

/// Takes a view's downloaded countries: checks and keeps them for this run, and in the cache
/// folder for the next. Not keeping the file only costs a download next time, so it is logged.
pub fn accept(v: View, bytes: Vec<u8>) -> Result<(), String> {
    let w = v.worldview;
    // The original details are published gzipped; inflating stops at the size allowed.
    let bytes = if bytes.starts_with(&[0x1f, 0x8b]) {
        crate::render::rustgis_tiles::mvt::gunzip_within(&bytes, v.max_bytes()).map_err(|e| {
            format!("the {} worldview's countries can't be unpacked ({e}): more than {} bytes, or damaged", w.name(), v.max_bytes())
        })?
    } else {
        bytes
    };
    if bytes.len() > v.max_bytes() {
        return Err(format!("the {} worldview's countries are {} bytes, more than the {} allowed", w.name(), bytes.len(), v.max_bytes()));
    }
    let text = String::from_utf8(bytes).map_err(|_| format!("the {} worldview's countries aren't UTF-8 text", w.name()))?;
    install(v, text)?;
    // Kept from the copy `install` keeps, not a second one.
    if let (Some(path), Some(text)) = (kept_path(v), self::text(v)) {
        let saved =
            path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| rustgis_data::io::write_atomic(&path, text.as_bytes()));
        if let Err(e) = saved {
            log::warn!("couldn't keep the {} worldview in {}: {e}", w.name(), path.display());
        }
    }
    Ok(())
}

/// How long a worldview's download may take (an original one is about 10 MB).
#[cfg(not(target_arch = "wasm32"))]
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Gets a view's countries into memory and calls `done` with the outcome: natively on a thread of
/// its own, which reads the kept copy or downloads, checks the file and builds its world outline,
/// so an original (1:10m) view never holds the window; in the browser with its fetch, checking
/// the file when it lands. A download superseded by another choice isn't stopped: it is one
/// request of a fixed size, and what it brings is kept for the run, so choosing that worldview
/// again costs nothing. `done`'s error doesn't name the worldview; the caller's message does.
pub fn fetch(v: View, done: Box<dyn FnOnce(Result<(), String>) + Send>) {
    #[cfg(not(target_arch = "wasm32"))]
    fetch_with(v, done, |url, max| crate::render::rustgis_tiles::cache::download_blocking_within(url, max, DOWNLOAD_TIMEOUT));
    #[cfg(target_arch = "wasm32")]
    {
        let Some(url) = v.url() else {
            return done(Ok(()));
        };
        crate::render::rustgis_tiles::cache::download_limited(
            &url,
            v.max_bytes(),
            Box::new(move |r| done(r.and_then(|bytes| accept(v, bytes)))),
        );
    }
}

/// [`fetch`] natively, downloading with `download` (the address and the most bytes allowed).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn fetch_with(
    v: View,
    done: Box<dyn FnOnce(Result<(), String>) + Send>,
    download: impl FnOnce(&str, usize) -> Result<Vec<u8>, String> + Send + 'static,
) {
    let Some(url) = v.url() else {
        return done(Ok(()));
    };
    {
        let work = move || {
            let r = if load_kept(v) {
                Ok(())
            } else {
                // The error alone: the app's notice names the worldview.
                download(&url, v.max_bytes()).and_then(|bytes| accept(v, bytes))
            };
            if r.is_ok() {
                rustgis_render::prepare_world(v);
            }
            r
        };
        // A thread the system refuses costs the worldview, never the window's time.
        let shared = std::sync::Arc::new(std::sync::Mutex::new(Some((work, done))));
        let mine = shared.clone();
        let spawned = std::thread::Builder::new().name("worldview".into()).spawn(move || {
            if let Some((work, done)) = mine.lock().ok().and_then(|mut g| g.take()) {
                // A panic (a parser on hostile bytes) is the download's error, not a silent stall.
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                    .unwrap_or_else(|_| Err("the download stopped unexpectedly".to_string()));
                done(r);
            }
        });
        if spawned.is_err()
            && let Some((_, done)) = shared.lock().ok().and_then(|mut g| g.take())
        {
            done(Err("couldn't start a thread".to_string()));
        }
    }
}

/// Makes a view's countries ready, downloading them and waiting if they aren't at hand (the
/// command line's `--worldview`).
#[cfg(not(target_arch = "wasm32"))]
pub fn download_blocking(v: View) -> Result<(), String> {
    if ready(v) {
        return Ok(());
    }
    let url = v.url().ok_or("the default worldview is built in")?;
    let bytes = crate::render::rustgis_tiles::cache::download_blocking_within(&url, v.max_bytes(), DOWNLOAD_TIMEOUT)
        .map_err(|e| format!("couldn't download the {} worldview: {e}", v.worldview.name()))?;
    accept(v, bytes)
}

fn cam(lon: f64, lat: f64, res: f64) -> Camera {
    Camera { center: lonlat_to_merc(Coord::new(lon, lat)), res, bearing: 0.0, pitch: 0.0 }
}

/// Whether the project has a layer named `Countries` holding exactly the built-in countries of
/// view `v` (the sample's, whether or not it may be swapped).
pub fn holds_countries(s: &crate::Session, v: View) -> bool {
    dataset(v).is_some_and(|d| s.map().layers.iter().any(|l| l.name == "Countries" && l.dataset.features == d.features))
}

/// Swaps an untouched sample project's countries from view `from` to `to`: only in a project
/// that is unsaved, unchanged and has nothing to undo, and only a layer named `Countries` holding
/// exactly the built-in countries of `from`. Returns whether a layer was swapped.
///
/// It runs on the UI thread, where the app applies a chosen worldview, and copies the view's
/// countries read ahead of time. The copy shares the features (`Features` is copy on write):
/// measured at 15 µs for an original (1:10m) view.
pub fn swap_countries(s: &mut crate::Session, from: View, to: View) -> bool {
    if from == to || s.dirty() || s.path.is_some() || s.can_undo() {
        return false;
    }
    // Read once per view (ahead of time on the desktop, see `fetch`), so a swap only copies.
    let (Some(old), Some(new)) = (dataset(from), dataset(to)) else {
        return false;
    };
    let Some(l) = s.map_mut().layers.iter_mut().find(|l| l.name == "Countries" && l.dataset.features == old.features) else {
        return false;
    };
    l.dataset = new.clone();
    l.selection.clear();
    l.touch_geometry();
    true
}

/// Slate gray country borders: visible against both the near-white lowest population class and the ocean.
const COUNTRY_BORDER: Rgba = Rgba::rgb(0x7D, 0x8C, 0x96);

/// "World Atlas": countries by population, cities sized by population, rivers and lakes.
pub fn world_project() -> Project {
    let mut p = Project::new("World Atlas");
    let map = p.map_mut();
    map.name = "World".into();
    map.basemap = rustgis_map::Basemap::Ocean;
    let mut add = |name: &str, text: &str| read_geojson(text, name).ok().map(|(ds, _)| map.add_dataset(ds));
    let countries = add("Countries", countries());
    let lakes = add("Lakes", LAKES);
    let rivers = add("Rivers", RIVERS);
    let cities = add("Cities", CITIES);
    if let Some(l) = countries.and_then(|id| map.layer_mut(id)) {
        let base = Symbol { fill: Rgba::rgb(200, 200, 200), stroke: COUNTRY_BORDER, stroke_width: 0.6, size: 0.0, marker: Marker::Circle };
        if let Ok(r) = Renderer::graduated(&l.dataset, &[], "pop_est", 5, ClassMethod::NaturalBreaks, Ramp::Lagoon, base) {
            l.renderer = r;
            l.renderer.map_symbols(|s| {
                s.stroke = COUNTRY_BORDER;
                s.stroke_width = 0.6;
            });
        }
        l.labels.enabled = true;
        l.labels.default.field = "name".into();
        l.labels.default.size = 8.5;
        l.labels.default.color = Rgba::rgb(0x33, 0x3A, 0x44);
    }
    if let Some(l) = lakes.and_then(|id| map.layer_mut(id)) {
        l.renderer = Renderer::Single(Symbol {
            fill: Rgba::rgb(0x9C, 0xC6, 0xE0),
            stroke: Rgba::rgb(0x6E, 0xA4, 0xC8),
            stroke_width: 0.5,
            size: 0.0,
            marker: Marker::Circle,
        });
    }
    if let Some(l) = rivers.and_then(|id| map.layer_mut(id)) {
        l.renderer = Renderer::Single(Symbol {
            fill: Rgba::rgb(0x4F, 0x8F, 0xC6),
            stroke: Rgba::rgb(0x4F, 0x8F, 0xC6),
            stroke_width: 1.2,
            size: 0.0,
            marker: Marker::Circle,
        });
    }
    if let Some(l) = cities.and_then(|id| map.layer_mut(id)) {
        let base = Symbol {
            fill: Rgba::new(0xE0, 0x6A, 0x3B, 220),
            stroke: Rgba::rgb(255, 255, 255),
            stroke_width: 0.8,
            size: 6.0,
            marker: Marker::Circle,
        };
        if let Ok(r) = Renderer::proportional(&l.dataset, &[], "pop_max", base) {
            l.renderer = r;
            if let Renderer::Proportional { min_size, max_size, .. } = &mut l.renderer {
                *min_size = 3.5;
                *max_size = 18.0;
            }
        }
        l.labels.default.field = "name".into();
        l.labels.default.size = 8.0;
        l.labels.default.bold = true;
        l.labels.default.color = Rgba::rgb(0x5A, 0x2A, 0x14);
        l.max_scale = None;
    }
    map.bookmarks = vec![
        Bookmark::new("World", cam(10.0, 15.0, 26_000.0)),
        Bookmark::new("Europe", cam(12.0, 50.0, 5_200.0)),
        Bookmark::new("Africa", cam(19.0, 2.0, 8_500.0)),
        Bookmark::new("Americas", cam(-80.0, 12.0, 15_000.0)),
        Bookmark::new("East Asia", cam(118.0, 32.0, 6_500.0)),
    ];
    map.camera = cam(10.0, 18.0, 26_000.0);
    p
}
