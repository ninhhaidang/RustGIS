//! Zarr stores: what they hold, and a variable's 2D slice added as a raster layer — from a folder
//! (read again when the project opens) or a web address (its chunks fetched; the slice is kept in
//! the project).

#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

use rustgis_data::raster::{self, Crs, zarr};
use rustgis_map::raster::RasterLayer;
#[cfg(not(target_arch = "wasm32"))]
use rustgis_map::raster::ZarrTime;
use serde_json::Value;

use super::{b, ps};
use crate::{CmdResult, CommandSpec as C, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("raster.zarrInfo", "Zarr Variables", "Home › Add › Add Data › Zarr", info)
            .view()
            .params(r#"{"path": "a Zarr store's folder" | "url": "https://… a store with consolidated metadata"}"#),
        C::new("zarr.series", "Time Series at a Point", "View › Time Series", series).view().params(
            r#"{"lon", "lat", "layer"? (a Zarr layer: its store, variable and steps; default the active layer) | "path": folder | "url": "https://…", "variable"?, "index"?: {"level": 0} (the other dimensions' steps), "start"?, "end"?: dates (default: the layer's period, else the whole axis), "crs"?: for a projected store that doesn't say} → dates, values (null for no data) and labels along the variable's time dimension at the cell holding the point (desktop)"#,
        ),
        C::new("zarr.reduce", "Summarise Zarr Over Time", "View › Time Series", reduce).params(
            r#"{"layer"? (a Zarr layer: its store, variable and steps; default the active layer) | "path": folder | "url": "https://…", "variable"?, "index"?, "stat": "mean"|"min"|"max"|"sum"|"std" (the population standard deviation, n)|"count"|"countAbove", "above"?: number (countAbove's threshold), "start"?, "end"?: dates (default: the layer's period, else the whole axis), "months"?: [1–12] (a season across years: [12, 1, 2]), "minValid"?: 0–1 (a cell with a smaller share of values is left empty; default 0), "name"?: text, "crs"?} → a raster layer of the statistic (desktop)"#,
        ),
        C::new("layer.addZarr", "Add Zarr Variable", "Home › Add › Add Data", add).params(
            r#"{"path": folder | "url": "https://…", "variable"?: "its path in the store (default: the first)", "index"?: {"time": 3, "level": 0} (other dimensions' steps, default 0), "crs"?: "EPSG:… for a store that doesn't say", "name"?: text, "time"?: bool (follow the time slider: the layer shows the step the slider is at, and turns the slider on over the variable's dates when it is off; desktop), "start"?, "end"?: dates (with time: the period to take, for an axis longer than the slider's 20,000 steps), "ran"?: (internal: a slice the app has read), "summary"?: bool (internal: with ran, a summary over time rather than a slice)}"#,
        ),
    ]
}

/// Where a store is: a folder, or a web address.
enum Source {
    Dir(String),
    Url(String),
}

fn source(p: &Value) -> Result<Source, String> {
    let s = ps(p, "url").or_else(|| ps(p, "path")).map(str::trim).filter(|s| !s.is_empty()).ok_or("path (a folder) or url is required")?;
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Ok(Source::Url(s.trim_end_matches('/').to_string()))
    } else {
        Ok(Source::Dir(s.to_string()))
    }
}

/// The selection a command names: `variable` and `index` ({dim: step}).
pub fn selection(p: &Value) -> zarr::Selection {
    let index = match p.get("index") {
        Some(Value::Object(m)) => m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_u64()?))).take(32).collect(),
        _ => Vec::new(),
    };
    zarr::Selection { variable: ps(p, "variable").map(str::trim).unwrap_or("").to_string(), index }
}

/// A store's root as a layer's first name: its folder or address's last part, without `.zarr`.
pub fn store_name(root: &str) -> String {
    let last = root.trim_end_matches('/').rsplit(['/', '\\']).next().unwrap_or("zarr");
    last.strip_suffix(".zarr").unwrap_or(last).to_string()
}

/// A slice's layer name: the store's name when it is a `.zarr` folder or address (not a version
/// folder such as CMIP6's `v20190308`), then the variable and its steps (`label` for each, when
/// given, as `tas 1850-01-15`).
pub fn layer_name(root: &str, sel: &zarr::Selection, labels: &[String]) -> String {
    let what = if labels.is_empty() { sel.label() } else { format!("{} {}", sel.variable, labels.join(" ")) };
    if root.trim_end_matches('/').to_ascii_lowercase().ends_with(".zarr") { format!("{} {what}", store_name(root)) } else { what }
}

/// Whether a fetch's error means the file isn't there: a 404, or a 403 as S3 answers for a key
/// that doesn't exist when listing isn't allowed. Decided by the HTTP status, never by digits
/// found in the message.
pub fn is_absent(error: &str) -> bool {
    matches!(crate::render::rustgis_tiles::cache::http_status(error), Some(404 | 403))
}

/// Fetches files of a store by web address, several at a time; a missing one (see [`is_absent`])
/// is left out. Also says how many were refused with 403, so a store that refuses everything
/// (access denied) isn't taken for an empty one.
#[cfg(not(target_arch = "wasm32"))]
fn fetch(root: &str, keys: &[String]) -> Result<(HashMap<String, Vec<u8>>, usize), String> {
    use std::sync::Mutex;
    let out = Mutex::new(HashMap::new());
    let forbidden = std::sync::atomic::AtomicUsize::new(0);
    // All the files fetched together, within what a folder's read takes, so many chunks each
    // within the download limit can't fill the memory.
    let total = std::sync::atomic::AtomicU64::new(0);
    let failure: Mutex<Option<String>> = Mutex::new(None);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let capture = rustgis_data::diagnostics::Capture::current();
    std::thread::scope(|scope| {
        for _ in 0..8.min(keys.len().max(1)) {
            scope.spawn(|| {
                let _diagnostic_scope = capture.enter();
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(k) = keys.get(i) else { break };
                    if failure.lock().is_ok_and(|f| f.is_some()) {
                        break;
                    }
                    // A missing file is an expected answer here (see `is_absent`), not a failure: its 404
                    // or 403 makes no network record of its own (one note per server says the
                    // omission, and a store refusing everything still says "access denied"), while
                    // a real one (a 500, a timeout) does, with its address, status and duration.
                    let got = crate::render::rustgis_tiles::cache::download_blocking_expecting(&format!("{root}/{k}"), &[404, 403]);
                    match got {
                        Ok(b) => {
                            let len = b.len() as u64;
                            if total.fetch_add(len, std::sync::atomic::Ordering::Relaxed).saturating_add(len) > zarr::MAX_READ {
                                if let Ok(mut f) = failure.lock() {
                                    f.get_or_insert(format!("{root}: more than {} GB to read: take a smaller part", zarr::MAX_READ >> 30));
                                }
                                break;
                            }
                            if let Ok(mut o) = out.lock() {
                                o.insert(k.clone(), b);
                            }
                        }
                        Err(e) if is_absent(&e) => {
                            if crate::render::rustgis_tiles::cache::http_status(&e) == Some(403) {
                                forbidden.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        Err(e) => {
                            if let Ok(mut f) = failure.lock() {
                                f.get_or_insert(format!("{root}/{k}: {e}"));
                            }
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = failure.into_inner().ok().flatten() {
        return Err(e);
    }
    let out = out.into_inner().map_err(|_| "a fetch failed".to_string())?;
    Ok((out, forbidden.into_inner()))
}

/// A store's metadata from its web address.
#[cfg(not(target_arch = "wasm32"))]
fn store_at(url: &str) -> Result<zarr::Store, String> {
    let keys: Vec<String> = zarr::METADATA_KEYS.iter().map(|k| k.to_string()).collect();
    let (got, forbidden) = fetch(url, &keys)?;
    // Every metadata file refused: access is denied, not a store without metadata.
    if got.is_empty() && forbidden > 0 {
        return Err(format!("{url}: access denied (HTTP 403)"));
    }
    let docs: Vec<(String, Vec<u8>)> = got.into_iter().collect();
    zarr::parse_store(&docs).map_err(|e| format!("{url}: {e}"))
}

fn info(_s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = p;
        Err("in the web app, open Zarr stores with Home › Add › Add Data › Zarr…".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    match source(p)? {
        Source::Dir(d) => {
            let store = zarr::read_metadata(Path::new(&d)).map_err(|e| format!("{d}: {e}"))?;
            let mut chunks = HashMap::new();
            // The coordinate chunks, read as read_dir reads a slice's: files only, bounded.
            let mut total = 0u64;
            for k in zarr::info_keys(&store) {
                let path = Path::new(&d).join(&k);
                let Ok(meta) = std::fs::metadata(&path) else { continue };
                if !meta.is_file() || meta.len() > zarr::MAX_CHUNK_FILE {
                    continue;
                }
                total = total.saturating_add(meta.len());
                if total > zarr::MAX_READ {
                    return Err(format!("{d}: the coordinates are too large to read"));
                }
                if let Ok(b) = std::fs::read(&path) {
                    chunks.insert(k, b);
                }
            }
            Ok(zarr::info(&store, &chunks))
        }
        Source::Url(u) => {
            let store = store_at(&u)?;
            let (chunks, _) = fetch(&u, &zarr::info_keys(&store))?;
            Ok(zarr::info(&store, &chunks))
        }
    }
}

/// The variable to read: the one named, else the store's first that can be read.
pub fn pick_variable(store: &zarr::Store, sel: &mut zarr::Selection) -> Result<(), String> {
    if sel.variable.is_empty() {
        sel.variable = store
            .variables()
            .iter()
            .find(|v| v.unsupported().is_none())
            .map(|v| v.path.clone())
            .ok_or("the store has no variable that can be read")?;
    }
    Ok(())
}

pub(crate) fn add(s: &mut Session, p: &Value) -> CmdResult {
    if b(p, "time") == Some(true) {
        return add_following(s, p);
    }
    let src = source(p)?;
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    let mut sel = selection(p);
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty());
    let crs_parsed = crs.map(Crs::parse).transpose()?;
    let root = match &src {
        Source::Dir(d) | Source::Url(d) => d.clone(),
    };
    // A slice the app read itself (the web app, or a store by address): its GeoTIFF, handed over.
    if let Some(id) = super::f(p, "ran").filter(|v| *v >= 0.0 && v.fract() == 0.0).map(|v| v as u64) {
        let i = s.downloaded.iter().position(|(d, _)| *d == id).ok_or("no such read (it was dropped when another project opened)")?;
        let (_, tif) = s.downloaded.remove(i);
        let mut r = raster::read_bytes("zarr.tif", &tif).map_err(|e| e.to_string())?;
        r.source = Some(root.clone());
        let name = ps(p, "name").map(str::to_string).unwrap_or_else(|| layer_name(&root, &sel, &[]));
        let mut layer = RasterLayer::from_bytes(r, format!("{}.tif", store_name(&root)), tif);
        // The layer is its GeoTIFF's bytes (`from_bytes`: no path), so it's never read again from
        // the store and `subdataset` only names it: a slice by its variable and steps, a summary
        // (said by the app that worked it out) by nothing, so it isn't taken for a slice.
        layer.subdataset = (super::b(p, "summary") != Some(true)).then(|| sel.label());
        // The coordinate system given for a store that doesn't say, kept for its series and
        // summaries, as a folder's slice keeps it.
        layer.crs = crs.map(str::to_string);
        return super::raster::add_raster(s, &name, layer);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (crs_parsed, src);
        Err("in the web app, add Zarr variables with Home › Add › Add Data › Zarr…".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    match src {
        Source::Dir(d) => {
            let store = zarr::read_metadata(Path::new(&d)).map_err(|e| format!("{d}: {e}"))?;
            pick_variable(&store, &mut sel)?;
            let r = zarr::read_dir(Path::new(&d), &sel.label(), crs).map_err(|e| format!("{d}: {e}"))?;
            let name = ps(p, "name").map(str::to_string).unwrap_or_else(|| r.name.clone());
            // Absolute, so the project finds the store again from anywhere.
            let full = std::path::absolute(&d).map(|p| p.display().to_string()).unwrap_or(d);
            let mut layer = RasterLayer::from_path(r, full);
            layer.subdataset = Some(sel.label());
            layer.crs = crs.map(str::to_string);
            super::raster::add_raster(s, &name, layer)
        }
        Source::Url(u) => {
            let store = store_at(&u)?;
            pick_variable(&store, &mut sel)?;
            let plan = zarr::plan(&store, &sel).map_err(|e| format!("{u}: {e}"))?;
            let (chunks, forbidden) = fetch(&u, &plan.keys)?;
            if forbidden > 0 {
                return Err(format!("{u}: access denied (HTTP 403) to {forbidden} of the slice's chunks"));
            }
            let name = ps(p, "name").map(str::to_string).unwrap_or_else(|| layer_name(&u, &sel, &[]));
            let mut r = zarr::assemble(&store, &plan, &chunks, &name, crs_parsed).map_err(|e| format!("{u}: {e}"))?;
            r.source = Some(u.clone());
            // Kept in the project as a GeoTIFF: a web store isn't fetched again when it opens.
            let tif = raster::geotiff::write(&r, 0).map_err(|e| e.to_string())?;
            let mut layer = RasterLayer::from_bytes(r, format!("{}.tif", store_name(&u)), tif);
            layer.subdataset = Some(sel.label());
            // The coordinate system given for a store that doesn't say, kept as a folder's slice
            // keeps it, so a time series read from the layer finds it.
            layer.crs = crs.map(str::to_string);
            super::raster::add_raster(s, &name, layer)
        }
    }
}

// --- Following the time slider (opengeos/RustGIS#554) --------------------------------------------

/// Slices kept for layers that follow the time slider, by store and selection: the last few, so
/// stepping back and forth or looping doesn't read them again.
#[cfg(not(target_arch = "wasm32"))]
const SLICES_KEPT: usize = 8;

/// The slices kept (see [`SLICES_KEPT`]), newest last.
#[cfg(not(target_arch = "wasm32"))]
fn slices() -> &'static std::sync::Mutex<Vec<(String, std::sync::Arc<raster::Raster>)>> {
    static S: std::sync::OnceLock<std::sync::Mutex<Vec<(String, std::sync::Arc<raster::Raster>)>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// Web stores' metadata, read once per run (the last few stores).
#[cfg(not(target_arch = "wasm32"))]
fn stores() -> &'static std::sync::Mutex<Vec<(String, std::sync::Arc<zarr::Store>)>> {
    static S: std::sync::OnceLock<std::sync::Mutex<Vec<(String, std::sync::Arc<zarr::Store>)>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

#[cfg(not(target_arch = "wasm32"))]
fn is_web(store: &str) -> bool {
    let lower = store.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// A web store's metadata, from the run's cache when it was read before.
#[cfg(not(target_arch = "wasm32"))]
fn cached_store(url: &str) -> Result<std::sync::Arc<zarr::Store>, String> {
    if let Some((_, st)) = stores().lock().ok().and_then(|c| c.iter().find(|(u, _)| u == url).cloned()) {
        return Ok(st);
    }
    let st = std::sync::Arc::new(store_at(url)?);
    if let Ok(mut c) = stores().lock() {
        c.push((url.to_string(), st.clone()));
        let excess = c.len().saturating_sub(4);
        c.drain(..excess);
    }
    Ok(st)
}

/// The key a slice is kept by: its store, selection and coordinate system.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn slice_key(zt: &ZarrTime, i: usize) -> String {
    // A folder store rewritten on disk (a model run adding steps) is read again: its metadata's
    // time of change is part of the key.
    let changed = if is_web(&zt.store) {
        String::new()
    } else {
        [".zmetadata", "zarr.json", ".zgroup"]
            .iter()
            .find_map(|f| std::fs::metadata(Path::new(&zt.store).join(f)).and_then(|m| m.modified()).ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or_else(String::new, |d| d.as_millis().to_string())
    };
    format!("{}\u{0}{}\u{0}{}\u{0}{changed}", zt.store, zt.selection(i), zt.crs.as_deref().unwrap_or(""))
}

#[cfg(not(target_arch = "wasm32"))]
fn is_kept(key: &str) -> bool {
    slices().lock().is_ok_and(|c| c.iter().any(|(k, _)| k == key))
}

/// Slices being read ahead in the background, by key: each is read once, however often the
/// slider moves meanwhile.
#[cfg(not(target_arch = "wasm32"))]
fn reading_ahead() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static R: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

/// The most threads reading ahead at once.
#[cfg(not(target_arch = "wasm32"))]
const MAX_READ_AHEAD: usize = 2;

#[cfg(not(target_arch = "wasm32"))]
static READ_AHEAD_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Reads the two steps after `i` in the background, those neither kept nor already being read,
/// on at most [`MAX_READ_AHEAD`] threads: a thread more is skipped (the next step starts one).
#[cfg(not(target_arch = "wasm32"))]
fn read_ahead(zt: ZarrTime, i: usize) {
    use std::sync::atomic::Ordering;
    let keys: Vec<(usize, String)> = (i + 1..zt.dates.len())
        .take(2)
        .map(|j| (j, slice_key(&zt, j)))
        .filter(|(_, k)| !is_kept(k) && reading_ahead().lock().is_ok_and(|mut r| r.insert(k.clone())))
        .collect();
    if keys.is_empty() {
        return;
    }
    let release = |keys: &[(usize, String)]| {
        if let Ok(mut r) = reading_ahead().lock() {
            for (_, k) in keys {
                r.remove(k);
            }
        }
    };
    if READ_AHEAD_THREADS.fetch_add(1, Ordering::SeqCst) >= MAX_READ_AHEAD {
        READ_AHEAD_THREADS.fetch_sub(1, Ordering::SeqCst);
        release(&keys);
        return;
    }
    // Whatever ends the thread, a panic on a hostile store included, its keys and its place
    // among the threads are given back.
    struct Done(Vec<String>);
    impl Drop for Done {
        fn drop(&mut self) {
            if let Ok(mut r) = reading_ahead().lock() {
                for k in &self.0 {
                    r.remove(k);
                }
            }
            READ_AHEAD_THREADS.fetch_sub(1, Ordering::SeqCst);
        }
    }
    let ids: Vec<String> = keys.iter().map(|(_, k)| k.clone()).collect();
    let capture = rustgis_data::diagnostics::Capture::current();
    let spawned = std::thread::Builder::new().name("zarr-read-ahead".into()).spawn(move || {
        let _diagnostic_scope = capture.enter();
        let _done = Done(keys.iter().map(|(_, k)| k.clone()).collect());
        for (n, (j, k)) in keys.iter().enumerate() {
            let ok = read_step(&zt, *j, k.clone()).is_ok();
            release(&keys[n..=n]);
            if !ok {
                break;
            }
        }
    });
    // No thread to be had: nothing is read ahead, and its keys and place are given back.
    if spawned.is_err() {
        drop(Done(ids));
    }
}

/// Waits (up to 15 s) for a step being read ahead, so it isn't fetched twice; after that it is
/// read here.
#[cfg(not(target_arch = "wasm32"))]
fn wait_for_read_ahead(key: &str) {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(15) && reading_ahead().lock().is_ok_and(|r| r.contains(key)) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Step `i`, once a read ahead of it has finished, so it isn't fetched twice.
#[cfg(not(target_arch = "wasm32"))]
fn read_waiting(zt: &ZarrTime, i: usize) -> Result<std::sync::Arc<raster::Raster>, String> {
    // One key for the wait and the read: a store rewritten between them is read once.
    let key = slice_key(zt, i);
    wait_for_read_ahead(&key);
    read_step(zt, i, key)
}

/// Step `i` of a following layer's variable, kept as `key` (its [`slice_key`]), from the kept
/// slices or read now.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_step(zt: &ZarrTime, i: usize, key: String) -> Result<std::sync::Arc<raster::Raster>, String> {
    if let Some(r) = kept_slice(&key) {
        return Ok(r);
    }
    let r = std::sync::Arc::new(read_slice(zt, i)?.ok_or_else(|| format!("{}: step {i} has no data written", zt.store))?);
    if let Ok(mut c) = slices().lock() {
        c.retain(|(k, _)| *k != key);
        c.push((key, r.clone()));
        let excess = c.len().saturating_sub(SLICES_KEPT);
        c.drain(..excess);
    }
    Ok(r)
}

/// A slice kept by [`read_step`], by its key.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn kept_slice(key: &str) -> Option<std::sync::Arc<raster::Raster>> {
    slices().lock().ok().and_then(|c| c.iter().find(|(k, _)| k == key).map(|(_, r)| r.clone()))
}

/// Step `i` of a following layer's variable, read from its store (no slice kept).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_slice(zt: &ZarrTime, i: usize) -> Result<Option<raster::Raster>, String> {
    if rustgis_map::paths::is_project_relative(&zt.store) {
        return Err(rustgis_map::paths::unknown_folder(&zt.store));
    }
    let sel = zt.selection(i);
    let crs = zt.crs.as_deref().map(str::trim).filter(|c| !c.is_empty());
    let read = if is_web(&zt.store) {
        let st = cached_store(&zt.store)?;
        let plan = zarr::plan(&st, &zarr::Selection::parse(&sel)).map_err(|e| e.to_string())?;
        let (chunks, forbidden) = fetch(&zt.store, &plan.keys)?;
        // A chunk refused (HTTP 403) isn't one never written: it's said, not read as fill.
        if forbidden > 0 {
            return Err(format!("{}: access denied (HTTP 403) to {forbidden} of the slice's chunks", zt.store));
        }
        let crs = crs.map(Crs::parse).transpose()?;
        zarr::assemble(&st, &plan, &chunks, &sel, crs)
    } else {
        zarr::read_dir(Path::new(&zt.store), &sel, crs)
    };
    // A step none of whose chunks is there was never written (Zarr's fill value): `None`, told
    // apart by its error kind, not its message.
    match read {
        Ok(r) => Ok(Some(r)),
        Err(rustgis_data::DataError::NoChunks(_)) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Reopens the same time-following variable from a replacement local store.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn relink_following(s: &Session, rl: &RasterLayer, full: &str) -> Result<RasterLayer, String> {
    let zt = rl.time_zarr.as_ref().ok_or("the raster has no time-following variable")?;
    let store = zarr::read_metadata(Path::new(full)).map_err(|e| format!("{full}: {e}"))?;
    if !store.variables().iter().any(|v| v.path == zt.variable) {
        return Err(format!("{full} has no variable '{}': choose the store the layer was added from", zt.variable));
    }
    let mut zt = ZarrTime { store: full.into(), shown: None, ..zt.clone() };
    zt.repair();
    let window = s.map().time.as_ref().map(rustgis_map::time::TimeSlider::window);
    let want = zt.step_for(window);
    let i = want.unwrap_or(0);
    let r = read_step(&zt, i, slice_key(&zt, i))?;
    zt.shown = want;
    let mut new = rl.clone();
    new.path = Some(full.into());
    new.data = want.map(|_| r);
    new.error = None;
    new.time_zarr = Some(zt);
    Ok(new)
}

/// Layers that follow the time slider read the step it shows, in every map: the last of their
/// steps in the slider's window, or none (the layer draws nothing) when no step falls in it. A
/// web store's next steps are then read ahead in the background, so playing doesn't wait for
/// each. A slice that can't be read leaves its layer empty, and says why.
pub(crate) fn follow_time(s: &mut Session) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut errors = Vec::new();
        let mut ahead: Vec<(ZarrTime, usize)> = Vec::new();
        for m in &mut s.project.maps {
            let window = m.time.as_ref().map(rustgis_map::time::TimeSlider::window);
            for l in &mut m.layers {
                let name = l.name.clone();
                let Some(rl) = l.raster.as_mut() else { continue };
                let Some(zt) = rl.time_zarr.as_mut() else { continue };
                let want = zt.step_for(window);
                // The step shown, or one that couldn't be read: that one isn't read again (nor its
                // error said again) until the slider has moved to another step and back.
                if want == zt.shown {
                    continue;
                }
                match want {
                    None => {
                        rl.data = None;
                        zt.shown = None;
                    }
                    Some(i) => match read_waiting(zt, i) {
                        Ok(r) => {
                            rl.data = Some(r);
                            zt.shown = Some(i);
                            if is_web(&zt.store) {
                                ahead.push((zt.clone(), i));
                            }
                        }
                        Err(e) => {
                            rl.data = None;
                            zt.shown = Some(i);
                            errors.push(format!("{name}: {e}"));
                        }
                    },
                }
            }
        }
        for e in errors {
            s.notify(e, true);
        }
        for (zt, i) in ahead {
            read_ahead(zt, i);
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = s;
}

/// `layer.addZarr` with `"time": true`: the variable's time axis read from its coordinate, a layer
/// that shows the step the slider is at, and the slider turned on over the axis when it is off.
/// The stretch is fixed at the first slice's, so a colour means one value at every step.
fn add_following(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("in the web app, a Zarr variable can't follow the time slider yet".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        if p.get("ran").is_some_and(|r| !r.is_null()) {
            return Err("time and ran don't go together".into());
        }
        let src = source(p)?;
        let mut sel = selection(p);
        let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty()).map(str::to_string);
        if let Some(c) = &crs {
            Crs::parse(c)?;
        }
        let (root, store, chunks) = match src {
            Source::Dir(d) => {
                let store = zarr::read_metadata(Path::new(&d)).map_err(|e| format!("{d}: {e}"))?;
                // The variable first (the store's first when none is given), so its coordinates are read.
                pick_variable(&store, &mut sel)?;
                let mut chunks = HashMap::new();
                let mut total = 0u64;
                for k in zarr::coordinate_keys(&store, &sel.variable) {
                    let path = Path::new(&d).join(&k);
                    let Ok(meta) = std::fs::metadata(&path) else { continue };
                    if !meta.is_file() || meta.len() > zarr::MAX_CHUNK_FILE {
                        continue;
                    }
                    total = total.saturating_add(meta.len());
                    if total > zarr::MAX_READ {
                        return Err(format!("{d}: the coordinates are too large to read"));
                    }
                    if let Ok(b) = std::fs::read(&path) {
                        chunks.insert(k, b);
                    }
                }
                let full = std::path::absolute(&d).map(|p| p.display().to_string()).unwrap_or(d);
                (full, std::sync::Arc::new(store), chunks)
            }
            Source::Url(u) => {
                let store = cached_store(&u)?;
                (u, store, HashMap::new())
            }
        };
        pick_variable(&store, &mut sel)?;
        let chunks =
            if chunks.is_empty() && is_web(&root) { fetch(&root, &zarr::coordinate_keys(&store, &sel.variable))?.0 } else { chunks };
        let (dim, dates) = zarr::time_axis(&store, &sel.variable, &chunks).ok_or_else(|| {
            format!("{} has no time dimension (a coordinate with CF units, such as \"days since 2001-01-01\")", sel.variable)
        })?;
        // Each date is a step: an axis out of order or with a date twice (stores joined end to end)
        // can't be followed step by step.
        if !dates.iter().all(|d| d.is_finite()) || !dates.windows(2).all(|w| w[0] < w[1]) {
            return Err(format!("{}'s time axis isn't in increasing order without repeats, so it can't follow the slider", sel.variable));
        }
        // The period asked for, else the whole axis.
        let date = |k: &str| -> Result<Option<f64>, String> {
            match p.get(k).filter(|v| !v.is_null()) {
                None => Ok(None),
                Some(v) => rustgis_data::time::parse_input(v)
                    .map(Some)
                    .ok_or_else(|| format!("{k} is a date (2015-06-01, a year, or milliseconds since 1970)")),
            }
        };
        let (start, end) = (date("start")?, date("end")?);
        let inside = |d: &f64| start.is_none_or(|a| *d >= a) && end.is_none_or(|z| *d <= z);
        let (cut, dates_len) = (start.is_none() && end.is_none() && dates.len() > rustgis_map::raster::MAX_ZARR_STEPS, dates.len());
        let (first, picked): (usize, Vec<f64>) = if cut {
            // An axis longer than the slider takes, with no period given (the Zarr dialog gives
            // none): from the step picked on it, as many steps as the slider takes.
            let at = sel.index.iter().find(|(d, _)| *d == dim).map_or(0, |(_, i)| usize::try_from(*i).unwrap_or(usize::MAX));
            if at >= dates.len() {
                return Err(format!("the step picked on {dim} is past its {} steps", dates.len()));
            }
            (at, dates[at..].iter().copied().take(rustgis_map::raster::MAX_ZARR_STEPS).collect())
        } else {
            let first = dates.iter().position(inside).ok_or("no step of the variable falls between start and end")?;
            (first, dates[first..].iter().copied().take_while(inside).collect())
        };
        if picked.len() > rustgis_map::raster::MAX_ZARR_STEPS {
            return Err(format!(
                "{} has {} steps there; the time slider takes {} at most: give start and end",
                sel.variable,
                picked.len(),
                rustgis_map::raster::MAX_ZARR_STEPS
            ));
        }
        let mut zt = ZarrTime {
            store: root.clone(),
            variable: sel.variable.clone(),
            index: sel.index.iter().filter(|(d, _)| *d != dim).cloned().collect(),
            dim,
            offset: first as u64,
            dates: picked,
            crs,
            shown: None,
        };
        zt.repair();
        // The first step read before the slider is touched, so a store that can't be read leaves
        // the map as it was.
        let first = read_step(&zt, 0, slice_key(&zt, 0)).map_err(|e| format!("{root}: {e}"))?;
        // The slider, on over the variable's dates when it is off.
        if s.map().time.is_none() {
            let dates: Vec<Value> = zt.dates.iter().map(|d| serde_json::json!(d)).collect();
            super::time::set(s, &serde_json::json!({"dates": dates}))?;
        }
        let window = s.map().time.as_ref().map(rustgis_map::time::TimeSlider::window);
        let want = zt.step_for(window);
        // The first slice the layer shows sets its style; with no step in the window, the first step's.
        let r = match want {
            Some(i) if i != 0 => read_step(&zt, i, slice_key(&zt, i)).map_err(|e| format!("{root}: {e}"))?,
            _ => first,
        };
        let mut layer = RasterLayer::from_path((*r).clone(), root.clone());
        if is_web(&root) {
            layer.path = None;
        }
        layer.data = want.map(|_| r.clone());
        // One stretch for every step: the first slice's range, fixed.
        let band = layer.style.renderer.bands().first().copied().unwrap_or(0);
        if !matches!(layer.style.stretch, rustgis_map::raster::Stretch::Custom { .. })
            && let Some(stats) = r.bands.get(band).map(|b| &b.stats)
        {
            let n = rustgis_map::raster::Normalizer::new(layer.style.stretch, stats, &r.data_type);
            if n.lo.is_finite() && n.hi.is_finite() && n.lo < n.hi {
                layer.style.stretch = rustgis_map::raster::Stretch::custom(n.lo, n.hi);
            }
        }
        zt.shown = want;
        let name = ps(p, "name")
            .map(str::to_string)
            .unwrap_or_else(|| layer_name(&root, &zarr::Selection { variable: zt.variable.clone(), index: zt.index.clone() }, &[]));
        // A long axis taken in part (no period given): said, with the period taken.
        let part = cut.then(|| {
            let day = |d: Option<&f64>| d.map_or_else(String::new, |d| rustgis_data::time::format(*d, "YYYY-MM-DD HH:mm"));
            format!(
                "{} has {dates_len} steps: the layer takes {} of them, {} to {}; give start and end for another period",
                zt.variable,
                zt.dates.len(),
                day(zt.dates.first()),
                day(zt.dates.last())
            )
        });
        layer.time_zarr = Some(zt);
        let mut out = super::raster::add_raster(s, &name, layer)?;
        if let Some(w) = part {
            s.warn(w.clone());
            out["warning"] = serde_json::json!(w);
        }
        Ok(out)
    }
}

// --- A point's time series (opengeos/RustGIS#555) -------------------------------------------------

/// The store a series reads, and what of it: from a Zarr layer (one that follows the time slider,
/// or a slice: its store, variable and steps), or from the parameters.
#[cfg(not(target_arch = "wasm32"))]
struct SeriesSource {
    root: String,
    sel: zarr::Selection,
    /// A layer's period of the axis (a following layer's offset and dates), else the whole axis.
    period: Option<(u64, usize)>,
    crs: Option<String>,
}

#[cfg(not(target_arch = "wasm32"))]
fn series_source(s: &Session, p: &Value) -> Result<SeriesSource, String> {
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    if !given("path") && !given("url") {
        let id = s.layer_id(p)?;
        let l = s.map().layer(id).ok_or("no such layer")?;
        let rl = l.raster.as_ref().ok_or_else(|| format!("{} isn't a Zarr layer", l.name))?;
        if let Some(zt) = &rl.time_zarr {
            return Ok(SeriesSource {
                root: zt.store.clone(),
                sel: zarr::Selection { variable: zt.variable.clone(), index: zt.index.clone() },
                period: Some((zt.offset, zt.dates.len())),
                crs: zt.crs.clone(),
            });
        }
        // A slice: its store is its folder, or the address it was read from.
        let label = rl.subdataset.clone().ok_or_else(|| format!("{} isn't a Zarr layer", l.name))?;
        let root = rl
            .path
            .clone()
            .or_else(|| rl.data.as_ref().and_then(|d| d.source.clone()))
            .ok_or_else(|| format!("{}: its store isn't known", l.name))?;
        return Ok(SeriesSource { root, sel: zarr::Selection::parse(&label), period: None, crs: rl.crs.clone() });
    }
    let root = match source(p)? {
        Source::Dir(d) | Source::Url(d) => d,
    };
    let crs = ps(p, "crs").map(str::trim).filter(|c| !c.is_empty()).map(str::to_string);
    Ok(SeriesSource { root, sel: selection(p), period: None, crs })
}

/// A store's metadata, and its files `keys`: a folder's read (files only, bounded), a web
/// store's fetched.
#[cfg(not(target_arch = "wasm32"))]
fn store_files(root: &str, keys: &[String]) -> Result<HashMap<String, Vec<u8>>, String> {
    if is_web(root) {
        // In batches, within the same total a folder's read takes: a store chunked a map a step
        // (one chunk per step) can't fill the memory.
        let mut out = HashMap::new();
        let mut total = 0u64;
        for batch in keys.chunks(64) {
            let (got, forbidden) = fetch(root, batch)?;
            // A file refused (HTTP 403) isn't one never written: said, not read as missing.
            if forbidden > 0 {
                return Err(format!("{root}: access denied (HTTP 403) to {forbidden} of its files"));
            }
            for (k, b) in got {
                total = total.saturating_add(b.len() as u64);
                if total > zarr::MAX_READ {
                    return Err(format!("{root}: the series is too large to read: take a shorter period"));
                }
                out.insert(k, b);
            }
        }
        return Ok(out);
    }
    let mut out = HashMap::new();
    let mut total = 0u64;
    for k in keys {
        let path = Path::new(root).join(k);
        // A file that isn't there is a chunk never written (Zarr's fill value); one that is but
        // can't be taken is said, so a value isn't silently missing.
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if !meta.is_file() || meta.len() > zarr::MAX_CHUNK_FILE {
            return Err(format!("{root}: {k} isn't a chunk file that can be read (too large, or not a file)"));
        }
        total = total.saturating_add(meta.len());
        if total > zarr::MAX_READ {
            return Err(format!("{root}: the series is too large to read"));
        }
        let b = std::fs::read(&path).map_err(|e| format!("{root}: {k}: {e}"))?;
        out.insert(k.clone(), b);
    }
    Ok(out)
}

/// `zarr.series`: a variable's values along its time dimension at the cell holding a point,
/// reading only the chunks that hold that cell.
fn series(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("in the web app, a Zarr time series can't be read yet".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let num = |k: &str| super::f(p, k).ok_or_else(|| format!("{k} is required (degrees)"));
        let (lon, lat) = (num("lon")?, num("lat")?);
        if !lon.is_finite() || !(-90.0..=90.0).contains(&lat) {
            return Err("lon is a longitude and lat a latitude from -90 to 90 (degrees)".into());
        }
        let mut src = series_source(s, p)?;
        let store = if is_web(&src.root) {
            cached_store(&src.root)?
        } else {
            std::sync::Arc::new(zarr::read_metadata(Path::new(&src.root)).map_err(|e| format!("{}: {e}", src.root))?)
        };
        pick_variable(&store, &mut src.sel)?;
        let var = src.sel.variable.clone();
        let mut keys = zarr::coordinate_keys(&store, &var);
        keys.extend(zarr::map_coordinate_keys(&store, &var));
        let coords = store_files(&src.root, &keys)?;
        let (dim, dates) = zarr::time_axis(&store, &var, &coords)
            .ok_or_else(|| format!("{var} has no time dimension (a coordinate with CF units, such as \"days since 2001-01-01\")"))?;
        // A period is found along the axis in order, as following the slider takes it.
        if !dates.iter().all(|d| d.is_finite()) || !dates.windows(2).all(|w| w[0] < w[1]) {
            return Err(format!("{var}'s time axis isn't in increasing order without repeats, so its series can't be read by date"));
        }
        // The period: the one asked for, else the layer's, else the whole axis.
        let date = |k: &str| -> Result<Option<f64>, String> {
            match p.get(k).filter(|v| !v.is_null()) {
                None => Ok(None),
                Some(v) => rustgis_data::time::parse_input(v)
                    .map(Some)
                    .ok_or_else(|| format!("{k} is a date (2015-06-01, a year, or milliseconds since 1970)")),
            }
        };
        let (start, end) = (date("start")?, date("end")?);
        let (from, count) = if start.is_some() || end.is_some() {
            let inside = |d: &f64| start.is_none_or(|a| *d >= a) && end.is_none_or(|z| *d <= z);
            let first = dates.iter().position(inside).ok_or("no step of the variable falls between start and end")?;
            (first, dates[first..].iter().take_while(|d| inside(d)).count())
        } else {
            match src.period {
                Some((o, n)) => (usize::try_from(o).unwrap_or(usize::MAX).min(dates.len()), n),
                None => (0, dates.len()),
            }
        };
        let count = count.min(dates.len().saturating_sub(from));
        if count == 0 {
            return Err("no step to read".into());
        }
        // The point in the grid's coordinates.
        let (x, y) = if zarr::is_lonlat_grid(&store, &var) {
            (lon, lat)
        } else {
            let crs = match zarr::grid_crs(&store, &var) {
                Some(c) => c,
                None => {
                    let given =
                        src.crs.as_deref().ok_or("the store doesn't say its coordinate system; give \"crs\" (such as EPSG:3035)")?;
                    Crs::parse(given)?
                }
            };
            let m = crs
                .transformer()?
                .from_lonlat(rustgis_geom::Coord::new(lon, lat))
                .ok_or("the point can't be placed in the store's coordinate system")?;
            (m.x, m.y)
        };
        let (row, col) =
            zarr::locate(&store, &var, &coords, x, y).map_err(|e| e.to_string())?.ok_or("the point is outside the variable's grid")?;
        let plan = zarr::plan_series(&store, &src.sel, &dim, from as u64, count as u64, row, col).map_err(|e| e.to_string())?;
        let chunks = store_files(&src.root, &plan.keys)?;
        let values = zarr::assemble_series(&store, &plan, &chunks).map_err(|e| e.to_string())?;
        let picked = &dates[from..from + count];
        let labels: Vec<String> = picked.iter().map(|d| rustgis_data::time::format(*d, "YYYY-MM-DD HH:mm")).collect();
        let values: Vec<Value> = values.iter().map(|v| if v.is_finite() { serde_json::json!(v) } else { Value::Null }).collect();
        let units = store.array(&var).and_then(|a| a.attrs.get("units")).and_then(Value::as_str).map(str::to_string);
        Ok(serde_json::json!({
            "variable": var, "units": units, "dimension": dim, "lon": lon, "lat": lat, "row": row, "col": col,
            "dates": picked, "labels": labels, "values": values,
        }))
    }
}

// --- A summary over time (opengeos/RustGIS#556) ----------------------------------------------------

#[cfg(not(target_arch = "wasm32"))]
pub use super::cube::Stat;

/// A summary over time, prepared on the session (`zarr.reduce`'s parameters checked and the
/// steps chosen) and computed anywhere with [`ReduceRun::run`], which reads one slice at a time:
/// the desktop app runs it on a thread of its own, with progress and Cancel.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct ReduceRun {
    zt: ZarrTime,
    /// The steps to read, as indexes into `zt.dates`.
    steps: Vec<usize>,
    stat: Stat,
    min_valid: f64,
    /// The layer's name, and the store and variable to hand the result over with.
    pub name: String,
    pub root: String,
    pub variable: String,
}

#[cfg(not(target_arch = "wasm32"))]
impl ReduceRun {
    /// How many slices it reads.
    pub fn total(&self) -> usize {
        self.steps.len()
    }

    /// Reads the steps one at a time and folds them into running totals (Welford's method for
    /// the standard deviation), so memory stays one slice and the totals whatever the period.
    /// `done` counts the slices read; `cancel` stops it between slices.
    pub fn run(&self, cancel: &std::sync::atomic::AtomicBool, done: &std::sync::atomic::AtomicUsize) -> Result<raster::Raster, String> {
        use std::sync::atomic::Ordering;
        // The first slice's grid (its size, place and coordinate system), all the run keeps of it.
        let mut first: Option<(u32, u32, raster::GeoTransform, Crs)> = None;
        let mut totals = super::cube::Totals::new(self.stat, 0);
        // Steps never written (no chunk at all), read as having no value.
        let mut unwritten = 0usize;
        for &i in &self.steps {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            // A slice already kept is used; the others are read without being kept, so a long
            // summary doesn't push out the slices a layer following the slider holds.
            let r = match kept_slice(&slice_key(&self.zt, i)) {
                Some(r) => r,
                None => match read_slice(&self.zt, i)? {
                    Some(r) => std::sync::Arc::new(r),
                    // A step never written is its fill value: no value for any cell, so it only
                    // lowers the counts (and the step after it still sets the grid).
                    None => {
                        unwritten += 1;
                        done.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                },
            };
            let vals = &r.bands.first().ok_or("the slice has no band")?.data;
            match &first {
                None => {
                    let len = vals.len();
                    // The running totals take 12 to 20 bytes a cell: bounded before they're made.
                    if len > MAX_REDUCE_CELLS {
                        return Err(format!("{len} cells, more than a summary takes ({MAX_REDUCE_CELLS})"));
                    }
                    totals = super::cube::Totals::new(self.stat, len);
                    first = Some((r.width, r.height, r.transform, r.crs));
                }
                // One grid for every step: the same size and coordinate system, its place within a
                // thousandth of a pixel, as for a trend.
                Some(f)
                    if (f.0, f.1, f.3) != (r.width, r.height, r.crs)
                        || !super::cube::same_grid(&f.2, &r.transform)
                        || vals.len() != totals.cells() =>
                {
                    return Err(format!("step {} is {} × {}, unlike the first ({} × {})", i, r.width, r.height, f.0, f.1));
                }
                Some(_) => {}
            }
            // The totals are the first slice's length, which every slice is checked to have.
            for (c, v) in vals.iter().enumerate().take(totals.cells()) {
                if v.is_finite() {
                    totals.add(c, f64::from(*v));
                }
            }
            done.fetch_add(1, Ordering::Relaxed);
        }
        let first = first.ok_or_else(|| {
            if unwritten > 0 {
                format!("none of the {unwritten} steps in the period has data written (only fill values)")
            } else {
                "no step to read".to_string()
            }
        })?;
        // Too few values: empty, unless the statistic is a count.
        let out = totals.finish(self.min_valid, self.steps.len());
        // One band: a project keeps the result as a GeoTIFF of one band (the count is a statistic of its own).
        let mut r =
            raster::Raster::new(&self.name, first.0, first.1, vec![out], first.2, first.3, None, raster::ColorHint::Gray, "Float32")
                .map_err(|e| e.to_string())?;
        r.source = Some(self.root.clone());
        Ok(r)
    }
}

/// Prepares `zarr.reduce`: the store, the statistic, and the steps of the period (and months)
/// asked for. At most the time slider's 20,000 steps.
#[cfg(not(target_arch = "wasm32"))]
pub fn reduce_start(s: &Session, p: &Value) -> Result<ReduceRun, String> {
    let stat = Stat::parse(p)?;
    // A variable of classes is summarised by cube.reduce, which reads its dates in order.
    if stat.categorical() {
        return Err(format!(
            "zarr.reduce takes mean, min, max, sum, std, count or countAbove: {} is cube.reduce's (give it this layer or store)",
            stat.code()
        ));
    }
    let min_valid = super::cube::min_share(p)?;
    let ZarrCube { zt, period } = zarr_cube(s, p)?;
    let steps = super::cube::steps(&zt.dates, period, p)?;
    let months = super::cube::months(p)?;
    let (var, dates) = (zt.variable.clone(), &zt.dates);
    if steps.len() > rustgis_map::raster::MAX_ZARR_STEPS {
        return Err(format!(
            "{} steps; a summary reads {} at most: take a shorter period",
            steps.len(),
            rustgis_map::raster::MAX_ZARR_STEPS
        ));
    }
    let day = |i: usize| rustgis_data::time::format(dates.get(i).copied().unwrap_or(f64::NAN), "YYYY-MM-DD");
    let (a, z) = (steps.first().copied().unwrap_or(0), steps.last().copied().unwrap_or(0));
    let mut label = format!("{var} {} {}–{}", stat.code(), day(a), day(z));
    if !months.is_empty() {
        let m: Vec<String> = months.iter().map(u32::to_string).collect();
        label = format!("{label} (months {})", m.join(","));
    }
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).map_or(label, |n| n.chars().take(200).collect());
    let root = zt.store.clone();
    Ok(ReduceRun { zt, steps, stat, min_valid, name, root, variable: var })
}

/// A Zarr variable's time axis as a data cube (#579): the store and variable to read, all its
/// dates, and the steps of a following layer's period (else the whole axis).
#[cfg(not(target_arch = "wasm32"))]
pub struct ZarrCube {
    pub zt: ZarrTime,
    pub period: (usize, usize),
}

/// The cube of a Zarr layer, or of the store and variable the parameters name.
#[cfg(not(target_arch = "wasm32"))]
pub fn zarr_cube(s: &Session, p: &Value) -> Result<ZarrCube, String> {
    let mut src = series_source(s, p)?;
    let store = if is_web(&src.root) {
        cached_store(&src.root)?
    } else {
        std::sync::Arc::new(zarr::read_metadata(Path::new(&src.root)).map_err(|e| format!("{}: {e}", src.root))?)
    };
    pick_variable(&store, &mut src.sel)?;
    let var = src.sel.variable.clone();
    let coords = store_files(&src.root, &zarr::coordinate_keys(&store, &var))?;
    let (dim, dates) = zarr::time_axis(&store, &var, &coords)
        .ok_or_else(|| format!("{var} has no time dimension (a coordinate with CF units, such as \"days since 2001-01-01\")"))?;
    // Every step a date, so a period and months pick the steps they say (a summary takes them
    // in any order; a trend asks for time order itself, in `cube_from`).
    if !dates.iter().all(|d| d.is_finite()) {
        return Err(format!("{var}'s time axis has a step that isn't a date"));
    }
    let period = match src.period {
        Some((o, n)) => {
            let o = usize::try_from(o).unwrap_or(usize::MAX).min(dates.len());
            (o, o.saturating_add(n).min(dates.len()))
        }
        None => (0, dates.len()),
    };
    let zt = ZarrTime {
        store: src.root.clone(),
        variable: var,
        index: src.sel.index.iter().filter(|(d, _)| *d != dim).cloned().collect(),
        dim,
        offset: 0,
        dates,
        crs: src.crs.clone(),
        shown: None,
    };
    Ok(ZarrCube { zt, period })
}

/// The most cells a summary keeps running totals for (12 to 20 bytes each): 32 million, at most
/// 640 MB.
#[cfg(not(target_arch = "wasm32"))]
const MAX_REDUCE_CELLS: usize = 32_000_000;

/// `zarr.reduce`: the summary computed here (blocking), and added as a raster layer kept in the
/// project as a GeoTIFF.
fn reduce(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (s, p);
        Err("in the web app, a Zarr variable can't be summarised over time yet".into())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let run = reduce_start(s, p)?;
        let r = run.run(&std::sync::atomic::AtomicBool::new(false), &std::sync::atomic::AtomicUsize::new(0))?;
        let tif = raster::geotiff::write(&r, 0).map_err(|e| e.to_string())?;
        let mut layer = RasterLayer::from_bytes(r, format!("{}.tif", store_name(&run.root)), tif);
        layer.subdataset = None;
        let mut out = super::raster::add_raster(s, &run.name, layer)?;
        out["steps"] = serde_json::json!(run.total());
        Ok(out)
    }
}
