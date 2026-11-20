//! Datum grids RustGIS downloads itself (#2032 A4), when its user turns that on ([`set_enabled`]):
//! the grids a coordinate system's most accurate datum transformation needs, from PROJ's grid
//! server (`https://cdn.proj.org/`), as `projsync` would get them. On a computer they are kept in
//! RustGIS's grid folder ([`crs::grid_dir`]) and read from there on later runs; in the browser they
//! are registered for the run ([`crs::register_grid`]). terrio-crs's own network stays off: a grid
//! is only ever asked for here. The frame path may start a download (the session asks for the
//! grids of the systems in use), but it runs on a thread of its own: a frame never waits for one.
//!
//! A file read on a thread of its own waits for its system's grids (and those of a conversion
//! between two systems it makes) before its features are converted ([`crate::cmd::reads::ReadAhead::run_waiting_for_grids`]), so they land where the grid puts
//! them. Elsewhere a download starts in the background, and the session says what it brought:
//! rasters, streamed layers and the map follow at once, while a layer read whole before keeps the places
//! it was given until it is opened again (it is held in longitude/latitude).

use rustgis_data::raster::Crs;
use rustgis_data::raster::crs::{self, MissingGrid};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

/// The largest grid downloaded: PROJ's largest are a few hundred megabytes (geoid models).
const MAX_GRID: usize = 512 << 20;

/// The most grids, and bytes, downloaded each time the setting is turned on: a file names the
/// systems whose grids are fetched, so one can't make RustGIS download without end.
const MAX_GRIDS: usize = 16;
const MAX_BYTES: usize = 2 << 30;

/// How many grids are downloaded at once: a few, so one slow (a large grid, or a fetch that
/// doesn't answer) doesn't hold up the others.
const AT_ONCE: usize = 3;

/// How long a download may take before it is given up.
#[cfg(not(target_arch = "wasm32"))]
const DOWNLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// How long a file's read waits for its system's grids before it goes on without them (and says so).
#[cfg(not(target_arch = "wasm32"))]
const WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Whether the user downloads datum grids (off unless turned on: it reaches a server).
static ENABLED: AtomicBool = AtomicBool::new(false);

/// What a download did, for the session to say ([`take_news`]).
#[derive(Debug, Clone, PartialEq)]
pub enum News {
    /// A grid arrived, with its size in bytes.
    Got { name: String, bytes: usize },
    /// A grid couldn't be had, and why.
    Failed { name: String, error: String },
    /// News a session asked for too late to hear, counted ([`news_since`]).
    Missed(u64),
}

#[derive(Default)]
struct State {
    /// Grids being downloaded now.
    busy: HashSet<String>,
    /// Grids whose download failed this run: not tried again until the setting is turned on again.
    failed: HashSet<String>,
    /// Grids downloaded this run: never downloaded again, even when still listed as missing.
    got: HashSet<String>,
    /// Downloads started, and bytes they brought, since the setting was turned on.
    started: usize,
    bytes: usize,
    /// Grids claimed and not yet started: [`AT_ONCE`] are downloaded at a time, so at most that
    /// many grids' bytes ([`MAX_GRID`]) are held at once, and the 2 GB counts what arrived.
    queue: std::collections::VecDeque<MissingGrid>,
    /// Downloads under way.
    running: usize,
    /// Times the setting was turned on ([`turns`]).
    turns: u64,
    /// What the downloads did, numbered, the last [`NEWS_KEPT`]: each session says what came
    /// after the last it said ([`news_since`]), so every session of the process hears each. One
    /// that asks too late to hear some is told how many it missed.
    news: std::collections::VecDeque<(u64, News)>,
    /// The number of the last news.
    told: u64,
}

/// How much news is kept for sessions that haven't asked yet.
const NEWS_KEPT: usize = 256;

impl State {
    /// Records what a download did, for every session to say.
    fn tell(&mut self, news: News) {
        self.told += 1;
        self.news.push_back((self.told, news));
        while self.news.len() > NEWS_KEPT {
            self.news.pop_front();
        }
    }

    /// [`news_since`], on this state.
    fn news_since(&self, seen: &mut u64) -> Vec<News> {
        let mut out = Vec::new();
        // Those dropped before this session heard them: how many.
        let first = self.news.front().map_or(self.told + 1, |(n, _)| *n);
        if first > *seen + 1 {
            out.push(News::Missed(first - *seen - 1));
        }
        out.extend(self.news.iter().filter(|(n, _)| *n > *seen).map(|(_, news)| news.clone()));
        *seen = self.told;
        out
    }

    /// The setting turned on: this run's failures are tried again, and the limits start over.
    fn turned_on(&mut self) {
        self.turns += 1;
        self.failed.clear();
        self.started = 0;
        self.bytes = 0;
    }
}

fn state() -> &'static (Mutex<State>, Condvar) {
    static STATE: OnceLock<(Mutex<State>, Condvar)> = OnceLock::new();
    STATE.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, State> {
    state().0.lock().unwrap_or_else(|e| e.into_inner())
}

/// How a grid is downloaded natively: its address, the most bytes, how long it may take.
#[cfg(not(target_arch = "wasm32"))]
type Download = fn(&str, usize, std::time::Duration) -> Result<Vec<u8>, String>;

#[cfg(not(target_arch = "wasm32"))]
fn download() -> &'static std::sync::RwLock<Download> {
    static DOWNLOAD: std::sync::RwLock<Download> = std::sync::RwLock::new(crate::render::rustgis_tiles::cache::download_blocking_within);
    &DOWNLOAD
}

/// Downloads grids with `f` instead of from the server: for tests, which reach no network.
#[doc(hidden)]
#[cfg(not(target_arch = "wasm32"))]
pub fn set_download(f: Download) {
    *download().write().unwrap_or_else(|e| e.into_inner()) = f;
}

/// Turns downloading datum grids on or off (the "Download Datum Grids" setting). Turning it on
/// from off forgets this run's failures, so they are tried again, and starts the limits over;
/// on again while on changes nothing.
pub fn set_enabled(on: bool) {
    // Only turning it on (from off) starts the limits over: on again while on changes nothing.
    let was = ENABLED.swap(on, Ordering::SeqCst);
    if on && !was {
        lock().turned_on();
    }
    #[cfg(not(target_arch = "wasm32"))]
    crs::set_grid_waiter(if on { Some(wait_for) } else { None });
    // Off: those not started yet aren't (they are asked for again when it is turned on).
    next();
}

/// Whether datum grids are downloaded.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// How many times downloads were turned on this run: a download said before is said again after.
pub fn turns() -> u64 {
    lock().turns
}

/// What the downloads did after the news numbered `seen` (a session's own count, from
/// [`news_told`] when it started), and moves `seen` to the last.
pub fn news_since(seen: &mut u64) -> Vec<News> {
    lock().news_since(seen)
}

/// Records news as a download would: for tests of the sessions' notices.
#[doc(hidden)]
pub fn tell(news: News) {
    lock().tell(news);
}

/// The number of the last news: a session starting now says what comes after it.
pub fn news_told() -> u64 {
    lock().told
}

/// What the downloads did since this was last asked here (one reader's count shared by every
/// caller: for tests; a session reads with [`news_since`]).
#[doc(hidden)]
pub fn take_news() -> Vec<News> {
    static SEEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut seen = SEEN.load(Ordering::SeqCst);
    let out = news_since(&mut seen);
    SEEN.store(seen, Ordering::SeqCst);
    out
}

/// Starts downloading, in the background, the grids `crs` needs that RustGIS doesn't have, and
/// gives the names of those it started or that were already on their way: none when downloads are
/// off, and none for a grid that failed or arrived this run.
pub fn want(crs: Crs) -> Vec<String> {
    want_in(crs, false)
}

/// [`want`] for the direct operation from `source` to `target`
/// ([`crs::missing_grids_between`]): the grids a conversion between them needs beyond each one's own.
pub fn want_between(source: Crs, target: Crs) -> Vec<String> {
    want_between_in(source, target, false)
}

/// [`want_between`]; `first`: ahead of the grids queued before, for a read waiting for them.
fn want_between_in(source: Crs, target: Crs, first: bool) -> Vec<String> {
    if !enabled() {
        return Vec::new();
    }
    want_grids(crs::missing_grids_between(source, target), first)
}

/// [`want`]; `first`: ahead of the grids queued before, for a read waiting for them.
fn want_in(crs: Crs, first: bool) -> Vec<String> {
    if !enabled() {
        return Vec::new();
    }
    want_grids(crs::missing_grids(crs), first)
}

/// Queues `grids` that aren't on their way or settled, and gives the names of those started or
/// already on their way; `first`: ahead of the grids queued before.
fn want_grids(grids: Vec<MissingGrid>, first: bool) -> Vec<String> {
    let mut coming = Vec::new();
    for grid in grids {
        match claim(&grid.name) {
            Claim::Mine => {
                coming.push(grid.name.clone());
                let mut s = lock();
                if first {
                    s.queue.push_front(grid);
                } else {
                    s.queue.push_back(grid);
                }
            }
            Claim::Coming => {
                // A read waiting for it: moved ahead, if not started yet.
                if first {
                    let mut s = lock();
                    if let Some(g) = s.queue.iter().position(|g| g.name == grid.name).and_then(|i| s.queue.remove(i)) {
                        s.queue.push_front(g);
                    }
                }
                coming.push(grid.name);
            }
            Claim::Settled => {}
        }
    }
    next();
    coming
}

/// Starts the grids queued, as many as may run at once.
fn next() {
    loop {
        let grid = lock().take_next(enabled());
        let Some(grid) = grid else { break };
        start(grid);
    }
    // Those dropped or refused on the way: their waiters see it.
    state().1.notify_all();
}

/// What [`claim`] found of a grid.
enum Claim {
    /// It is this caller's to download.
    Mine,
    /// Another is downloading it.
    Coming,
    /// It arrived or failed this run.
    Settled,
}

/// Marks `name` as being downloaded by the caller, unless it is already, or it arrived or failed
/// this run.
fn claim(name: &str) -> Claim {
    // On a computer, a grid is downloaded only if there is a folder to keep it in.
    #[cfg(not(target_arch = "wasm32"))]
    let can_keep = crs::grid_dir().is_some();
    #[cfg(target_arch = "wasm32")]
    let can_keep = true;
    lock().claim(name, can_keep)
}

impl State {
    /// [`claim`], on this state.
    fn claim(&mut self, name: &str, can_keep: bool) -> Claim {
        if self.busy.contains(name) {
            return Claim::Coming;
        }
        if self.failed.contains(name) || self.got.contains(name) {
            return Claim::Settled;
        }
        // Checked before anything is asked of the server: only PROJ's plain names are.
        let refused = if !plain(name) {
            Some("its name isn't a plain file name".to_string())
        } else if !name.ends_with(".tif") {
            // The server's other files (deformation models, point lists) aren't grids RustGIS reads.
            Some("RustGIS downloads PROJ's GeoTIFF grids only".to_string())
        } else if !can_keep {
            Some("RustGIS has no folder to keep it in".to_string())
        } else if self.started >= MAX_GRIDS || self.bytes >= MAX_BYTES {
            Some(limit_text())
        } else {
            None
        };
        if let Some(error) = refused {
            self.failed.insert(name.to_string());
            self.tell(News::Failed { name: name.to_string(), error });
            return Claim::Settled;
        }
        self.started += 1;
        self.busy.insert(name.to_string());
        Claim::Mine
    }

    /// The next grid queued to start, if fewer than [`AT_ONCE`] are under way. With downloads
    /// `on` no longer, the queue is dropped (each asked for again when they are turned on);
    /// once 2 GB have arrived, those queued are refused.
    fn take_next(&mut self, on: bool) -> Option<MissingGrid> {
        while !on || self.running < AT_ONCE {
            let Some(grid) = self.queue.pop_front() else { break };
            if !on {
                self.busy.remove(&grid.name);
                self.started = self.started.saturating_sub(1);
            } else if self.bytes >= MAX_BYTES {
                self.busy.remove(&grid.name);
                self.failed.insert(grid.name.clone());
                self.tell(News::Failed { name: grid.name, error: limit_text() });
            } else {
                self.running += 1;
                return Some(grid);
            }
        }
        None
    }
}

/// What the session says of `missed` download news it asked for too late to hear: what it was
/// isn't known (arrivals or failures), so it is said as information.
pub(crate) fn missed_text(missed: u64) -> crate::notice_text::NoticeText {
    use crate::notice_text::NoticeText;
    let key = if missed == 1 {
        "{count} notice about datum grid downloads came before it could be said, and is left out"
    } else {
        "{count} notices about datum grid downloads came before they could be said, and are left out"
    };
    NoticeText::format(key, [("count", missed.to_string().into())])
}

/// Why a grid isn't downloaded that trying again doesn't change.
const FINAL: [&str; 3] = ["its name isn't a plain file name", "RustGIS has no folder", "RustGIS downloads PROJ's GeoTIFF grids only"];

/// Why a grid past the limits isn't downloaded.
fn limit_text() -> String {
    format!(
        "RustGIS downloads at most {MAX_GRIDS} grids each time the setting is turned on, and starts none once {} GB of grids have been kept",
        MAX_BYTES >> 30
    )
}

/// Records how `name`'s download went, and wakes those waiting for it.
fn finish(name: &str, outcome: Result<usize, String>) {
    let mut s = lock();
    s.busy.remove(name);
    match outcome {
        Ok(bytes) => {
            s.bytes += bytes;
            s.got.insert(name.to_string());
            s.tell(News::Got { name: name.to_string(), bytes });
        }
        Err(error) => {
            s.failed.insert(name.to_string());
            s.tell(News::Failed { name: name.to_string(), error });
        }
    }
    s.running = s.running.saturating_sub(1);
    drop(s);
    state().1.notify_all();
    rustgis_map::source::wake();
    next();
}

/// Whether a grid's name can be a file name in the grid folder: PROJ's grid names are plain
/// (`us_noaa_conus.tif`), and nothing else is written there.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Whether `bytes` are a datum grid file, as PROJ's grid server gives them: a GeoTIFF (classic or
/// BigTIFF, either byte order; [`claim`] asks only for `.tif` names). Anything else (an
/// error page a network answered with) isn't kept, or it would count as the grid from then on.
/// Every part of it is checked to lie within the bytes, so a cut-off answer isn't kept either.
fn a_grid(bytes: &[u8]) -> bool {
    let n = bytes.len() as u64;
    let at = |i: usize, w: usize| i.checked_add(w).and_then(|end| bytes.get(i..end));
    let int = |i: usize, w: usize, big: bool| {
        at(i, w).map(|b| {
            let mut v = [0u8; 8];
            if big {
                v[8 - w..].copy_from_slice(b);
                u64::from_be_bytes(v)
            } else {
                v[..w].copy_from_slice(b);
                u64::from_le_bytes(v)
            }
        })
    };
    match bytes.get(..4) {
        // TIFF and BigTIFF: every directory, and the strips or tiles each points to, within the
        // file, so one cut off anywhere is refused.
        Some(b"II*\0") | Some(b"MM\0*") => tiff_whole(&int, n, bytes.first() == Some(&b'M'), false),
        Some(b"II+\0") | Some(b"MM\0+") => tiff_whole(&int, n, bytes.first() == Some(&b'M'), true),
        _ => false,
    }
}

/// Whether a TIFF of `n` bytes (`int` reads its integers: at, width, big-endian) holds every
/// directory it chains and every strip or tile they point to. The work is bounded by the file's
/// size: each directory is visited once, each part tag counted once a directory, and no more
/// values are read in all than the file has bytes.
fn tiff_whole(int: &dyn Fn(usize, usize, bool) -> Option<u64>, n: u64, big: bool, bigtiff: bool) -> bool {
    // Classic: 2-byte entry counts, 12-byte entries, 4-byte offsets; BigTIFF: 8, 20, 8.
    let (count_w, entry_w, off_w, header) = if bigtiff { (8, 20, 8, 16) } else { (2, 12, 4, 8) };
    let Some(mut ifd) = int(if bigtiff { 8 } else { 4 }, off_w, big) else { return false };
    let mut seen = HashSet::new();
    let mut budget = n;
    while ifd != 0 {
        // PROJ's grids have a directory per subgrid: a few hundred at most, each once.
        if seen.len() >= 4096 || ifd < header || !seen.insert(ifd) {
            return false;
        }
        let Ok(at) = usize::try_from(ifd) else { return false };
        let Some(entries) = int(at, count_w, big) else { return false };
        let Some(entries_at) = usize::try_from(entries).ok().and_then(|e| e.checked_mul(entry_w)) else { return false };
        let Some(next_at) = at.checked_add(count_w).and_then(|a| a.checked_add(entries_at)) else { return false };
        let Some(following) = int(next_at, off_w, big) else { return false };
        // Entries walked count too (directories may overlap).
        let Some(left) = budget.checked_sub(entries) else { return false };
        budget = left;
        if entries == 0 {
            return false;
        }
        // Where strip (273, 279) or tile (324, 325) offsets and byte counts are: values at, how
        // many, their width.
        let mut parts: [Option<(usize, usize, usize)>; 4] = [None; 4];
        for e in 0..entries as usize {
            let entry = at + count_w + e * entry_w;
            let (Some(tag), Some(ty), Some(count)) = (int(entry, 2, big), int(entry + 2, 2, big), int(entry + 4, off_w, big)) else {
                return false;
            };
            let slot = match tag {
                273 => 0,
                279 => 1,
                324 => 2,
                325 => 3,
                _ => continue,
            };
            let width = match ty {
                3 => 2,
                4 => 4,
                16 => 8,
                _ => return false,
            };
            let Some((count, size)) = usize::try_from(count).ok().and_then(|c| Some((c, c.checked_mul(width)?))) else { return false };
            let value_at = entry + 4 + off_w;
            let values_at = if size <= off_w {
                value_at
            } else {
                match int(value_at, off_w, big).and_then(|o| usize::try_from(o).ok()) {
                    Some(o) => o,
                    None => return false,
                }
            };
            // Within the file, and each tag once.
            if values_at.checked_add(size).is_none_or(|end| end as u64 > n) || parts[slot].is_some() {
                return false;
            }
            parts[slot] = Some((values_at, count, width));
        }
        let mut any = false;
        for (offsets, counts) in [(parts[0], parts[1]), (parts[2], parts[3])] {
            match (offsets, counts) {
                (None, None) => {}
                (Some((oa, oc, ow)), Some((ca, cc, cw))) if oc == cc => {
                    any |= oc > 0;
                    // No more values read in all than the file has bytes.
                    let Some(left) = budget.checked_sub(2 * oc as u64) else { return false };
                    budget = left;
                    for i in 0..oc {
                        let (Some(o), Some(c)) = (int(oa + i * ow, ow, big), int(ca + i * cw, cw, big)) else { return false };
                        if o.checked_add(c).is_none_or(|end| end > n) {
                            return false;
                        }
                    }
                }
                _ => return false,
            }
        }
        if !any {
            return false;
        }
        ifd = following;
    }
    !seen.is_empty()
}

/// Makes the downloaded bytes of `name` available: kept in the grid folder on a computer, then
/// the transformers are built again with it.
#[cfg(not(target_arch = "wasm32"))]
fn keep(name: &str, bytes: &[u8]) -> Result<(), String> {
    if !plain(name) {
        return Err("its name isn't a plain file name".into());
    }
    if !a_grid(bytes) {
        return Err("the server's answer isn't a grid file".into());
    }
    let dir = crs::grid_dir().ok_or("RustGIS has no folder to keep it in")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("the folder {} can't be made: {e}", dir.display()))?;
    rustgis_data::io::write_atomic(&dir.join(name), bytes).map_err(|e| format!("it can't be written to {}: {e}", dir.display()))?;
    crs::grids_changed();
    Ok(())
}

/// Downloads `grid` on a thread of its own. The download has no Cancel: it is no read's step (a
/// read stops waiting for it when cancelled), and ends within [`DOWNLOAD_TIMEOUT`] and
/// [`MAX_GRID`] bytes; what it brought, or why not, is said.
#[cfg(not(target_arch = "wasm32"))]
fn start(grid: MissingGrid) {
    let name = grid.name.clone();
    let spawned = std::thread::Builder::new().name("datum grid".into()).spawn(move || fetch_now(&grid));
    if let Err(e) = spawned {
        finish(&name, Err(format!("couldn't start a thread: {e}")));
    }
}

/// Downloads `grid` here, and keeps it.
#[cfg(not(target_arch = "wasm32"))]
fn fetch_now(grid: &MissingGrid) {
    let got = std::panic::catch_unwind(|| {
        let download = *download().read().unwrap_or_else(|e| e.into_inner());
        let bytes = download(&grid.url, MAX_GRID, DOWNLOAD_TIMEOUT)?;
        keep(&grid.name, &bytes).map(|()| bytes.len())
    })
    .unwrap_or_else(|_| Err("the download stopped unexpectedly".into()));
    finish(&grid.name, got);
}

/// Downloads `grid` with the browser's fetch, and registers it for the run.
#[cfg(target_arch = "wasm32")]
fn start(grid: MissingGrid) {
    let name = grid.name.clone();
    crate::render::rustgis_tiles::cache::download_limited(
        &grid.url,
        MAX_GRID,
        Box::new(move |r| {
            let got = r.and_then(|bytes| {
                if !plain(&name) {
                    return Err("its name isn't a plain file name".to_string());
                }
                if !a_grid(&bytes) {
                    return Err("the server's answer isn't a grid file".to_string());
                }
                let n = bytes.len();
                crs::register_grid(&name, bytes);
                Ok(n)
            });
            finish(&name, got);
        }),
    );
}

/// Gets the grids a system needs before a file in it is read, or a conversion between two systems
/// needs before it is made ([`crs::set_grid_waiter`]): each one missing is downloaded on a thread
/// of its own, or was already on its way, and the read waits for them, then asks again, as the
/// next operation's grids come up where the first's don't reach. It waits at most [`WAIT`] in
/// all, for all the systems and pairs of one read, and stops waiting when its read is cancelled;
/// the downloads go on, and what they bring is said and used from then on.
#[cfg(not(target_arch = "wasm32"))]
fn wait_for(of: crs::GridsOf, read: u64) {
    thread_local! {
        /// The read waited for on this thread, and when its wait ends: one for all its systems.
        static DEADLINE: std::cell::Cell<Option<(u64, web_time::Instant)>> = const { std::cell::Cell::new(None) };
    }
    let deadline = DEADLINE
        .try_with(|d| match d.get() {
            Some((r, at)) if r == read => at,
            _ => {
                let at = web_time::Instant::now() + WAIT;
                d.set(Some((read, at)));
                at
            }
        })
        .unwrap_or_else(|_| web_time::Instant::now() + WAIT);
    let control = rustgis_analysis::control::current();
    // A few rounds at most: each brings the grids the operation chosen after the last needs.
    for _ in 0..4 {
        let coming = match of {
            crs::GridsOf::System(crs) => want_in(crs, true),
            crs::GridsOf::Pair(a, b) => want_between_in(a, b, true),
        };
        if coming.is_empty() {
            return;
        }
        let (lock, ready) = (&state().0, &state().1);
        let mut s = lock.lock().unwrap_or_else(|e| e.into_inner());
        while coming.iter().any(|name| s.busy.contains(name)) {
            let left = deadline.saturating_duration_since(web_time::Instant::now());
            if left.is_zero() || control.as_ref().is_some_and(|c| c.is_cancelled()) {
                return;
            }
            // In slices, to see Cancel.
            s = ready.wait_timeout(s, left.min(std::time::Duration::from_millis(100))).unwrap_or_else(|e| e.into_inner()).0;
        }
        if coming.iter().all(|name| s.failed.contains(name)) {
            return;
        }
    }
}

/// What the session says of downloads: one notice for the grids that arrived (each with its size)
/// and what they change, and one for those that couldn't be had and why, however many.
pub fn news_texts(news: &[News]) -> Vec<(String, bool)> {
    let mut got = Vec::new();
    // By reason, in order: a reason given once for all its grids (the limit, for one).
    let mut why: Vec<(&str, Vec<&str>)> = Vec::new();
    // (Missed news is the session's to say: [`missed_text`].)
    let mut count = 0;
    for n in news {
        match n {
            News::Missed(_) => {}
            News::Got { name, bytes } => got.push(format!("{name} ({})", size_text(*bytes))),
            News::Failed { name, error } => {
                count += 1;
                match why.iter_mut().find(|(e, _)| e == error) {
                    Some((_, names)) => names.push(name),
                    None => why.push((error, vec![name])),
                }
            }
        }
    }
    // A few named per reason, the rest counted: the notice stays short however many.
    let failed: Vec<String> = why
        .iter()
        .map(|(error, names)| {
            let named = names.iter().take(3).copied().collect::<Vec<_>>().join(", ");
            let more = if names.len() > 3 { format!(" and {} more", names.len() - 3) } else { String::new() };
            format!("{named}{more} ({error})")
        })
        .collect();
    let mut out = Vec::new();
    if !got.is_empty() {
        let (grids, it) = if got.len() == 1 { ("grid", "it") } else { ("grids", "them") };
        out.push((
            format!(
                "Downloaded the datum {grids} {} from PROJ's grid server: streamed layers, rasters and the map now place features with {it}, as PROJ does; layers read whole before keep their places until they are opened again.",
                got.join(", ")
            ),
            false,
        ));
    }
    if let Some(first) = failed.first() {
        // Trying again helps a download that failed, not one RustGIS won't make (a name it
        // doesn't download, no folder to keep it in).
        let again = why.iter().any(|(e, _)| !FINAL.iter().any(|f| e.starts_with(f)));
        let again = if again { "; turn Download Datum Grids off and on again to try again" } else { "" };
        let text = if count == 1 {
            format!("The datum grid {first} couldn't be downloaded, so the next transformation is used without it{again}.")
        } else {
            format!(
                "{count} datum grids couldn't be downloaded: {}, so the next transformations are used without them{again}.",
                failed.join("; ")
            )
        };
        out.push((text, true));
    }
    out
}

fn size_text(bytes: usize) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1.0 { format!("{mb:.1} MB") } else { format!("{} KB", bytes.div_ceil(1000)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_names_are_kept() {
        assert!(plain("us_noaa_conus.tif"));
        assert!(plain("ch_swisstopo_CHENyx06_ETRS.tif"));
        for bad in ["", "../a.tif", "a/b.tif", "a\\b.tif", ".hidden", "c:a.tif", "a b.tif"] {
            assert!(!plain(bad), "{bad}");
        }
    }

    #[test]
    fn a_grid_file_is_whole_and_anything_else_is_refused() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../data/tests/data/grids/");
        for name in ["us_noaa_conus.tif", "us_noaa_ethpgn.tif"] {
            let bytes = std::fs::read(format!("{dir}{name}")).unwrap();
            assert!(a_grid(&bytes), "{name}");
            // Cut off before its first directory ends, or in its last tile: not kept.
            assert!(!a_grid(&bytes[..16]), "{name}");
            assert!(!a_grid(&bytes[..bytes.len() - 1]), "{name}");
        }
        // Only the leading bytes of one, or of another kind: not grids.
        for bad in [&b"II*\0"[..], b"II*\0\xff\xff\xff\x7f", b"II+\0", b"NUM_OREC", b"CTABLE V2", b"<html>"] {
            assert!(!a_grid(bad), "{bad:?}");
        }
        // A whole NTv2 or CTable2 file isn't one: PROJ's grid server has GeoTIFFs only, and the
        // grids RustGIS reads from it are those.
        let mut ct = vec![0u8; 160 + 2 * 3 * 8];
        ct[..9].copy_from_slice(b"CTABLE V2");
        ct[128..132].copy_from_slice(&2u32.to_le_bytes());
        ct[132..136].copy_from_slice(&3u32.to_le_bytes());
        assert!(!a_grid(&ct));
    }

    #[test]
    fn downloads_stop_at_the_limit_until_the_setting_is_turned_on_again() {
        // A state of the test's own: the process's is the other tests' in this binary.
        let mut s = State { started: MAX_GRIDS, ..Default::default() };
        assert!(matches!(s.claim("a.tif", true), Claim::Settled));
        let news = s.news_since(&mut 0);
        assert!(matches!(&news[..], [News::Failed { name, error }] if name == "a.tif" && error.contains("at most 16 grids")), "{news:?}");
        assert!(matches!(s.claim("a.tif", true), Claim::Settled), "not asked again");
        s.turned_on();
        assert!(matches!(s.claim("a.tif", true), Claim::Mine));
        assert!(matches!(s.claim("a.tif", true), Claim::Coming));
        // Once 2 GB have arrived, none is started.
        let mut s = State { bytes: MAX_BYTES, ..Default::default() };
        assert!(matches!(s.claim("b.tif", true), Claim::Settled));
        // Queued grids start a few at a time; with 2 GB arrived, those left are refused; with
        // downloads off, those left are dropped, to be asked for again.
        let grid = |name: &str| MissingGrid { name: name.into(), url: format!("https://cdn.proj.org/{name}") };
        let mut s = State::default();
        for name in ["q1.tif", "q2.tif", "q3.tif", "q4.tif", "q5.tif"] {
            assert!(matches!(s.claim(name, true), Claim::Mine));
            s.queue.push_back(grid(name));
        }
        for _ in 0..AT_ONCE {
            assert!(s.take_next(true).is_some());
        }
        assert!(s.take_next(true).is_none(), "no more than {AT_ONCE} at once");
        s.running -= 1;
        s.bytes = MAX_BYTES;
        assert!(s.take_next(true).is_none());
        assert!(s.queue.is_empty() && s.failed.contains("q4.tif") && s.failed.contains("q5.tif"));
        let mut s = State::default();
        assert!(matches!(s.claim("q6.tif", true), Claim::Mine));
        s.queue.push_back(grid("q6.tif"));
        assert!(s.take_next(false).is_none());
        assert!(s.queue.is_empty() && !s.busy.contains("q6.tif") && !s.failed.contains("q6.tif"));
        assert!(matches!(s.claim("q6.tif", true), Claim::Mine), "asked for again");
        // A name that isn't a plain file name: refused before the server is asked.
        let mut s = State::default();
        assert!(matches!(s.claim("a b.tif", true), Claim::Settled));
        assert!(
            matches!(&s.news.iter().map(|(_, n)| n.clone()).collect::<Vec<_>>()[..], [News::Failed { error, .. }] if error.contains("plain file name"))
        );
        // Nowhere to keep it: not downloaded, and said.
        let mut s = State::default();
        assert!(matches!(s.claim("c.tif", false), Claim::Settled));
        assert!(
            matches!(&s.news.iter().map(|(_, n)| n.clone()).collect::<Vec<_>>()[..], [News::Failed { error, .. }] if error.contains("no folder"))
        );
        // A name not a GeoTIFF's (a deformation model): not asked for.
        let mut s = State::default();
        assert!(matches!(s.claim("nz_linz_nzgd2000-20180701.json", true), Claim::Settled));
        assert!(
            matches!(&s.news.iter().map(|(_, n)| n.clone()).collect::<Vec<_>>()[..], [News::Failed { error, .. }] if error.contains("GeoTIFF grids only"))
        );
        // Hostile sizes don't overflow.
        let mut big = b"II+\0\x08\0\0\0".to_vec();
        big.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(!a_grid(&big));
    }

    #[test]
    fn news_says_the_grid_its_size_and_what_it_changes() {
        let said = news_texts(&[News::Got { name: "us_noaa_conus.tif".into(), bytes: 173_029 }]);
        assert!(
            matches!(&said[..], [(text, false)] if text.contains("grid us_noaa_conus.tif (174 KB)") && text.contains("opened again")),
            "{said:?}"
        );
        let said = news_texts(&[News::Failed { name: "a.tif".into(), error: "HTTP 404".into() }]);
        assert!(matches!(&said[..], [(text, true)] if text.contains("a.tif (HTTP 404)")), "{said:?}");
        // One notice of each kind, however many grids.
        let said = news_texts(&[
            News::Got { name: "a.tif".into(), bytes: 1000 },
            News::Failed { name: "b.tif".into(), error: "HTTP 404".into() },
            News::Got { name: "c.tif".into(), bytes: 2000 },
            News::Failed { name: "d.tif".into(), error: "HTTP 500".into() },
        ]);
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[0].0.contains("grids a.tif (1 KB), c.tif (2 KB)") && !said[0].1, "{said:?}");
        assert!(said[1].0.starts_with("2 datum grids couldn't be downloaded: b.tif (HTTP 404); d.tif (HTTP 500)") && said[1].1, "{said:?}");
        assert!(said[1].0.contains("off and on again"), "{said:?}");
        // Where trying again doesn't help, it isn't offered.
        let said = news_texts(&[News::Failed { name: "c.tif".into(), error: "RustGIS has no folder to keep it in".into() }]);
        assert!(!said[0].0.contains("off and on again"), "{said:?}");
        // News a session asked for too late to hear: counted, and said.
        let mut log = State::default();
        for i in 0..NEWS_KEPT + 10 {
            log.tell(News::Got { name: format!("g{i}.tif"), bytes: 1 });
        }
        let mut seen = 0;
        let heard = log.news_since(&mut seen);
        assert_eq!((heard.first(), heard.len(), seen), (Some(&News::Missed(10)), NEWS_KEPT + 1, (NEWS_KEPT + 10) as u64));
        assert!(log.news_since(&mut seen).is_empty(), "each once");
        assert!(news_texts(&[News::Missed(10)]).is_empty(), "the session's to say");
        assert!(missed_text(10).english().starts_with("10 notices about datum grid downloads came before they could be said"));
        assert!(missed_text(1).english().starts_with("1 notice about datum grid downloads came before it could be said, and is left out"));
        // A reason given once for its grids, a few named.
        let limit: Vec<News> = (0..5).map(|i| News::Failed { name: format!("g{i}.tif"), error: limit_text() }).collect();
        let said = news_texts(&limit);
        assert!(
            said[0].0.starts_with("5 datum grids couldn't be downloaded: g0.tif, g1.tif, g2.tif and 2 more (RustGIS downloads at most"),
            "{said:?}"
        );
        assert_eq!(said[0].0.matches("at most 16").count(), 1, "{said:?}");
        assert_eq!(size_text(12_345_678), "12.3 MB");
    }
}
