//! Cloud-optimised GeoTIFFs by their web address, read in place: the file's header (its IFDs)
//! once, then only the tiles of the overview a map tile needs, by HTTP range requests. Each map
//! tile (`cog:{z}/{x}/{y}@<address>`) is drawn from them in Web Mercator, as a picture, so a COG is
//! a tile layer like a PMTiles archive: drawn as its tiles land, on the desktop and in the browser.
//!
//! A file's own look is what its PhotometricInterpretation says: RGB images in colour, palette
//! images by their colour table, grey (MinIsBlack) ones grey, their first band, but for three bands
//! (taken as red, green and blue), each with its alpha band. Other bands of an image with more are
//! for the user to pick. Grey and colour images are stretched between their 2nd and 98th
//! percentiles (from the coarsest overview); 8-bit colour images are shown as they are. An address
//! may end with a [`Look`] (`#bands=2,17,10&range=-0.3,0.3`): the bands shown as red, green and
//! blue (or one, grey) and their stretch. Bands stored apart (planar files, such as AlphaEarth's 64
//! embedding bands) are read a band at a time, only those shown. Files are hostile input: header
//! reads, tile counts and sizes are bounded (see [`rustgis_data::raster::cog`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rustgis_data::diagnostics::message_with_address;
use rustgis_data::raster::ColorHint;
use rustgis_data::raster::cog::{self, Layout, Level, Parse, Sparse};
use rustgis_data::raster::crs::Transformer;
use rustgis_geom::Coord;
use web_time::{Duration, Instant};

use crate::cache::{Done, MAX_BYTES, RANGE, caller_callback};

pub mod raster;

/// The scheme of a map tile drawn from a COG.
pub const SCHEME: &str = "cog:";

/// The first read of a file: enough for a COG's IFDs, as GDAL writes them, in one request.
const HEAD: u64 = 64 << 10;
/// More of the header, when it's longer, is read in steps this large at first, doubling each
/// round ([`header_read`]).
const STEP: u64 = 256 << 10;
/// The most header reads before giving up.
const MAX_ROUNDS: usize = 12;
/// The last byte position a header read may start at: far past any real file, and far enough from
/// `u64::MAX` that a position plus a read's length never overflows.
const MAX_AT: u64 = u64::MAX >> 1;
/// The most COG tiles one map tile may need (past that, a coarser overview is read).
const MAX_PER_TILE: usize = 16;
/// The most tiles of the coarsest overview read for the stretch.
const MAX_STATS_TILES: usize = 16;
/// Decoded tiles kept, in samples.
const BUDGET: usize = if cfg!(target_arch = "wasm32") { 24 << 20 } else { 64 << 20 };
/// A failed file is tried again after this long.
const RETRY: Duration = Duration::from_secs(60);
/// A map tile's side, in pixels.
const SIZE: u32 = 256;
/// Web Mercator's half extent.
const HALF: f64 = 20_037_508.342_789_244;

/// The template of a COG's map tiles (its address's [`Look`] kept).
pub fn template(url: &str) -> String {
    format!("{SCHEME}{{z}}/{{x}}/{{y}}@{url}")
}

/// The most looks' stretches a file keeps.
const MAX_LOOKS: usize = 64;
/// The most bands a [`Look`] names.
const MAX_LOOK_BANDS: usize = 3;
/// The highest band number a [`Look`] may name.
const MAX_BAND: usize = 65_535;

/// How a COG layer shows its bands, from the end of its address (`#bands=2,17,10&range=-0.3,0.3`,
/// bands counted from 1, `&dequantize=aef` for AlphaEarth's quantised embeddings).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Look {
    /// The bands shown (0-based): three as red, green and blue, or one, grey; empty for the file's
    /// own (its colour bands, else its first).
    pub bands: Vec<usize>,
    /// The values shown from black to full colour, for every band; `None` for each band's 2nd to
    /// 98th percentile.
    pub range: Option<(f32, f32)>,
    /// Values are AlphaEarth's quantised embeddings, shown as `sign(v)·(v/127.5)²`.
    pub aef: bool,
    /// A data cube's file (`stac.addCube`): a tile layer that follows the time slider, kept one
    /// when its project opens again.
    pub cube: bool,
}

impl Look {
    /// The address part this look adds (empty for the file's own look).
    pub fn suffix(&self) -> String {
        let mut parts = Vec::new();
        if !self.bands.is_empty() {
            parts.push(format!("bands={}", self.bands.iter().map(|b| b.saturating_add(1).to_string()).collect::<Vec<_>>().join(",")));
        }
        if let Some((lo, hi)) = self.range {
            parts.push(format!("range={lo},{hi}"));
        }
        if self.aef {
            parts.push("dequantize=aef".into());
        }
        if self.cube {
            parts.push("cube=1".into());
        }
        if parts.is_empty() { String::new() } else { format!("#{}", parts.join("&")) }
    }

    /// The look an address part says (`bands=…&range=…&dequantize=aef`); `None` when it isn't one.
    fn parse(part: &str) -> Option<Look> {
        let mut look = Look::default();
        for kv in part.split('&') {
            let (k, v) = kv.split_once('=')?;
            match k {
                "bands" => {
                    let bands: Vec<usize> = v.split(',').map(|b| b.trim().parse::<usize>().ok()).collect::<Option<_>>()?;
                    if bands.len() > MAX_LOOK_BANDS || bands.iter().any(|b| *b == 0 || *b > MAX_BAND) {
                        return None;
                    }
                    look.bands = bands.into_iter().map(|b| b - 1).collect();
                }
                "range" => {
                    let (lo, hi) = v.split_once(',')?;
                    let (lo, hi) = (lo.trim().parse::<f32>().ok()?, hi.trim().parse::<f32>().ok()?);
                    if !(lo.is_finite() && hi.is_finite() && hi > lo) {
                        return None;
                    }
                    look.range = Some((lo, hi));
                }
                "dequantize" => look.aef = v == "aef",
                "cube" => look.cube = v == "1",
                _ => return None,
            }
        }
        Some(look)
    }
}

/// A COG's address without its [`Look`], and the look (the file's own when it names none).
pub fn split(url: &str) -> (&str, Look) {
    match url.rsplit_once('#') {
        Some((file, part)) => match Look::parse(part) {
            Some(look) => (file, look),
            None => (url, Look::default()),
        },
        None => (url, Look::default()),
    }
}

/// A COG's address showing `look` (any look it had replaced).
pub fn with_look(url: &str, look: &Look) -> String {
    format!("{}{}", split(url).0, look.suffix())
}

/// A value as a look shows it.
fn shown_value(v: f32, aef: bool) -> f32 {
    if aef {
        let q = v / 127.5;
        q * q * v.signum()
    } else {
        v
    }
}

/// Whether a tile address is a COG's.
pub fn is_tile(url: &str) -> bool {
    url.starts_with(SCHEME)
}

/// A policy change retires a read without classifying the resource as failed.
pub(super) const REACH_CHANGED: &str = "the file's network reach changed while it was loading; retry the read";

/// What a COG holds, ready to draw from.
pub struct Info {
    /// A descendant read never exceeds the ceiling under which its header was requested.
    reach: crate::address::Reach,
    pub layout: Layout,
    header: Sparse,
    transformer: Transformer,
    /// Longitude/latitude bounds: west, south, east, north.
    pub bounds: [f64; 4],
    /// Its Web Mercator bounds: min x, min y, max x, max y.
    merc: [f64; 4],
    pub min_zoom: u8,
    /// The zoom whose pixels are about the size of the full image's.
    pub max_zoom: u8,
    /// Each shown band's (low, high) values, mapped to 0 and 255, by the bands shown and whether
    /// they are dequantised (the file's own bands' set once the coarsest overview is read, before
    /// the file is ready; others' as a look first needs them).
    stretch: Mutex<HashMap<(Vec<usize>, bool), Vec<(f32, f32)>>>,
    /// The stretches being read, with who waits for each: read once, however many map tiles
    /// ask for it first.
    stretching: Mutex<HashMap<(Vec<usize>, bool), Vec<StretchThen>>>,
    /// Stretches whose read failed, and when: drawn with the type's full range meanwhile (not
    /// kept), read again after [`RETRY`].
    stretch_failed: Mutex<HashMap<(Vec<usize>, bool), Instant>>,
    /// The file's alpha band, shown with any look.
    alpha: Option<usize>,
    /// A GeoTIFF on this computer: the revision of the file its header was read from, taken
    /// before it was read. The header is kept only while the file is that revision, and what is
    /// read with it is checked against it.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) revision: OnceLock<rustgis_data::raster::FileRevision>,
}

impl Info {
    /// The bands a look colours the map with (0-based): three as red, green and blue, or one,
    /// grey. Its own: an RGB image's colour bands, or a grey image's three bands (most remote
    /// sensing images are grey, whatever their bands: three are taken as red, green and blue);
    /// else the first band, the others for the user to pick.
    pub fn colour(&self, look: &Look) -> Vec<usize> {
        let n = self.layout.bands.max(1);
        // Bands the file has, as many as asked (one or three); otherwise the file's own look.
        let picked: Vec<usize> = look.bands.iter().copied().filter(|b| *b < n).collect();
        match picked.len() {
            1 | 3 if picked.len() == look.bands.len() => picked,
            _ if matches!(self.layout.color, ColorHint::Rgb { .. }) && n >= 3 => vec![0, 1, 2],
            _ if matches!(self.layout.color, ColorHint::Gray) && n == 3 => vec![0, 1, 2],
            _ => vec![0],
        }
    }

    /// The bands a look reads: its colour bands, then the file's alpha band, if it has one.
    pub fn shown(&self, look: &Look) -> Vec<usize> {
        self.shown_from(self.colour(look))
    }

    /// [`Info::shown`], from the look's colours already found.
    fn shown_from(&self, mut shown: Vec<usize>) -> Vec<usize> {
        if let Some(a) = self.alpha.filter(|a| !shown.contains(a)) {
            shown.push(a);
        }
        shown
    }

    /// The colours of a palette image's values, when its own look shows them.
    fn palette(&self, look: &Look) -> Option<&[[u8; 4]]> {
        self.layout.palette.as_deref().filter(|_| look.bands.is_empty() && look.range.is_none() && !look.aef)
    }

    /// The bands' (low, high) values for a look, when known.
    fn stretch_for(&self, look: &Look) -> Option<Vec<(f32, f32)>> {
        let colour = self.colour(look);
        if let Some(r) = look.range {
            return Some(vec![r; colour.len()]);
        }
        self.stretch.lock().unwrap_or_else(|e| e.into_inner()).get(&(colour, look.aef)).cloned()
    }
}

enum State {
    Ready(Arc<Info>, crate::address::Reach),
    Loading(Vec<Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>>, crate::address::Reach),
    Failed(String, Instant, crate::address::Reach),
}

fn infos() -> &'static Mutex<HashMap<String, State>> {
    static I: OnceLock<Mutex<HashMap<String, State>>> = OnceLock::new();
    I.get_or_init(Mutex::default)
}

type Bands = Arc<cog::Tile>;

enum TileState {
    /// Decoded: its bands, how many samples they hold, and when it was last used (see
    /// [`Tiles::clock`]).
    Ready(Bands, usize, u64, crate::address::Reach),
    /// Being read: who waits for it, the reach it is read under, and the header (`Info`, by its
    /// address, alive while the read is) it is decoded with.
    Loading(Vec<Box<dyn FnOnce(Result<Bands, String>) + Send>>, crate::address::Reach, usize),
}

#[derive(Default)]
struct Tiles {
    map: HashMap<(String, usize, u32), TileState>,
    samples: usize,
    /// Goes up with each use: past [`BUDGET`], the tile used longest ago makes room, never one
    /// the map in view has just used (which would be read again, and make room for itself by
    /// throwing out another in view: the view flickering between levels).
    clock: u64,
    /// The tiles ready, by when each was last used (the clock then, unique): the first is the
    /// one to make room, found without a scan of them all.
    order: BTreeMap<u64, (String, usize, u32)>,
}

impl Tiles {
    /// A tile ready, marked as just used.
    fn used(&mut self, key: &(String, usize, u32), ceiling: crate::address::Reach) -> Option<Bands> {
        self.clock += 1;
        let now = self.clock;
        match self.map.get_mut(key) {
            Some(TileState::Ready(b, _, used, reach)) if *reach <= ceiling.min(crate::cache::ceiling_for(&key.0)) => {
                self.order.remove(used);
                *used = now;
                self.order.insert(now, key.clone());
                Some(b.clone())
            }
            _ => None,
        }
    }

    /// Keeps a tile ready, making room for it past [`BUDGET`] (the tiles used longest ago go);
    /// returns what was kept under its key before.
    fn keep(&mut self, key: (String, usize, u32), bands: Bands, samples: usize, reach: crate::address::Reach) -> Option<TileState> {
        // Kept under this key already: counted once.
        let before = self.map.remove(&key);
        if let Some(TileState::Ready(_, n, used, _)) = &before {
            self.samples = self.samples.saturating_sub(*n);
            self.order.remove(used);
        }
        while self.samples + samples > BUDGET {
            let Some((_, k)) = self.order.pop_first() else { break };
            if let Some(TileState::Ready(_, n, _, _)) = self.map.remove(&k) {
                self.samples = self.samples.saturating_sub(n);
            }
        }
        self.clock += 1;
        self.samples += samples;
        self.order.insert(self.clock, key.clone());
        self.map.insert(key, TileState::Ready(bands, samples, self.clock, reach));
        before
    }
}

fn tiles() -> &'static Mutex<Tiles> {
    static T: OnceLock<Mutex<Tiles>> = OnceLock::new();
    T.get_or_init(Mutex::default)
}

/// Forgets every file and tile (those that failed too), so they are read again (with
/// [`crate::cache::clear`]).
pub(crate) fn forget() {
    infos().lock().unwrap_or_else(|e| e.into_inner()).retain(|_, s| matches!(s, State::Loading(..)));
    raster::forget_failed(None);
    let mut t = tiles().lock().unwrap_or_else(|e| e.into_inner());
    t.map.retain(|_, s| matches!(s, TileState::Loading(..)));
    t.order.clear();
    t.samples = 0;
}

/// Forgets what was read of one file (its info and its tiles, those that failed too), but not a
/// read under way: a failed read is made again with whatever login now signs the file's address.
pub(crate) fn forget_file(url: &str) {
    let mut all = infos().lock().unwrap_or_else(|e| e.into_inner());
    if all.get(url).is_some_and(|s| !matches!(s, State::Loading(..))) {
        all.remove(url);
    }
    drop(all);
    raster::forget_failed(Some(url));
    forget_file_in(&mut tiles().lock().unwrap_or_else(|e| e.into_inner()), url);
}

/// Forgets what was read of a GeoTIFF on this computer ([`is_local_file`]), so its header and
/// tiles are read again from the file as it is now: it is added or reloaded after it may have
/// been written again. Nothing for a web address, whose reads follow the tile cache's.
pub fn forget_local_file(url: &str) {
    if is_local_file(url) {
        forget_file(url);
    }
}

/// Forgets a file's tiles kept (not those loading) from `t`.
fn forget_file_in(t: &mut Tiles, url: &str) {
    let mut freed = 0;
    t.map.retain(|(u, _, _), s| match s {
        TileState::Ready(_, n, _, _) if u == url => {
            freed += *n;
            false
        }
        _ => true,
    });
    t.samples = t.samples.saturating_sub(freed);
    t.order.retain(|_, (u, _, _)| u != url);
}

/// A file's info: `Ok(None)` while it is being read (asking for it starts the read).
pub fn info(url: &str) -> Result<Option<Arc<Info>>, String> {
    crate::cache::check_still(&crate::cache::current_still())?;
    crate::logins::check(url).map_err(str::to_string)?;
    let url = split(url).0;
    // A local file written again since its header was read is read again (as with `with_info`).
    #[cfg(not(target_arch = "wasm32"))]
    forget_if_rewritten(url);
    {
        let i = infos().lock().unwrap_or_else(|e| e.into_inner());
        match i.get(url) {
            Some(State::Ready(info, reach)) if *reach <= crate::cache::ceiling_for(url) => return Ok(Some(info.clone())),
            Some(State::Loading(..)) => return Ok(None),
            Some(State::Failed(e, at, reach)) if *reach == crate::cache::ceiling_for(url) => {
                // Not read again until its wait is over: the map draws again then.
                let wait = crate::cache::retry_wait(e, RETRY);
                if at.elapsed() < wait {
                    crate::cache::notify_at(*at + wait);
                    return Err(e.clone());
                }
            }
            _ => {}
        }
    }
    with_info(url, Box::new(|_| crate::cache::notify()));
    Ok(None)
}

/// A file's info if its header is already in, without asking for it (a layer the map doesn't
/// draw starts no read): what has landed, for what is said once it has. `None` while it is being
/// read, after a failed read, or where the address may not be read now (a login that ended).
pub fn info_landed(url: &str) -> Option<Arc<Info>> {
    crate::cache::check_still(&crate::cache::current_still()).ok()?;
    crate::logins::check(url).ok()?;
    let url = split(url).0;
    match infos().lock().unwrap_or_else(|e| e.into_inner()).get(url) {
        Some(State::Ready(info, reach)) if *reach <= crate::cache::ceiling_for(url) => Some(info.clone()),
        _ => None,
    }
}

/// Whether a file's header is being read now (asked for, not in yet), without asking for it.
pub fn info_loading(url: &str) -> bool {
    matches!(infos().lock().unwrap_or_else(|e| e.into_inner()).get(split(url).0), Some(State::Loading(..)))
}

/// How many headers have landed in this run: what looks for headers landed since it last looked
/// (the files a dated series showed before the slider moved on) looks again only when it changes.
static LANDED: AtomicU64 = AtomicU64::new(0);

/// The count of headers landed so far ([`LANDED`]).
pub fn landed_count() -> u64 {
    LANDED.load(Ordering::Relaxed)
}

/// Forgets what was read of a GeoTIFF on this computer whose header was read from another
/// revision of the file than it is now: a file written again while it is open is read again, not
/// drawn from its old header and tiles.
#[cfg(not(target_arch = "wasm32"))]
fn forget_if_rewritten(url: &str) {
    if !is_local_file(url) {
        return;
    }
    let kept = match infos().lock().unwrap_or_else(|e| e.into_inner()).get(url) {
        Some(State::Ready(info, _)) => info.revision.get().cloned(),
        _ => return,
    };
    if let Some(kept) = kept
        && Some(kept) != rustgis_data::raster::FileRevision::of(std::path::Path::new(url))
    {
        forget_file(url);
    }
}

/// Shares parsed headers and loading waiters, retaining retry delays only for resource failures.
fn with_info(url: &str, then: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>) {
    with_info_at(url, crate::cache::ceiling_for(url), then);
}

/// Retains a waiter's original ceiling when another reader's in-flight load must finish first.
fn with_info_at(url: &str, ceiling: crate::address::Reach, then: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>) {
    let then = caller_callback(then);
    let started_reach = ceiling.min(crate::cache::ceiling_for(url));
    if let Err(e) = crate::cache::check_still(&crate::cache::current_still()) {
        then(Err(e));
        return;
    }
    if let Err(e) = crate::logins::check(url) {
        then(Err(e.to_string()));
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    forget_if_rewritten(url);
    {
        let mut i = infos().lock().unwrap_or_else(|e| e.into_inner());
        match i.get_mut(url) {
            Some(State::Ready(info, reach)) if *reach <= started_reach.min(crate::cache::ceiling_for(url)) => {
                let info = info.clone();
                drop(i);
                then(Ok(info));
                return;
            }
            Some(State::Loading(waiting, reach)) => {
                if *reach == started_reach {
                    waiting.push(then);
                } else {
                    let url = url.to_string();
                    waiting.push(crate::cache::caller_retry(Box::new(move |_| with_info_at(&url, started_reach, then))));
                }
                return;
            }
            Some(State::Failed(e, at, reach)) if *reach == started_reach && at.elapsed() < crate::cache::retry_wait(e, RETRY) => {
                crate::cache::notify_at(*at + crate::cache::retry_wait(e, RETRY));
                let e = e.clone();
                drop(i);
                then(Err(e));
                return;
            }
            _ => {
                i.insert(url.to_string(), State::Loading(vec![then], started_reach));
            }
        }
    }
    let key = url.to_string();
    load(
        url.to_string(),
        started_reach,
        Box::new(move |r| {
            let r = crate::cache::check_still(&crate::cache::current_still()).and(r);
            let r = crate::logins::check(&key)
                .map_err(str::to_string)
                .and(r)
                .map_err(|e| if rustgis_data::login_address::is_wrapper(&key) { crate::logins::scrub_error(&e) } else { e });
            let mut r = r;
            let mut transient = r.as_ref().err().is_some_and(|e| !crate::cache::keep_failure(e, &crate::cache::current_still()));
            let waiting = {
                let mut i = infos().lock().unwrap_or_else(|e| e.into_inner());
                if started_reach > crate::cache::ceiling_for(&key) {
                    r = Err(REACH_CHANGED.into());
                    transient = true;
                }
                let old = match &r {
                    Ok(info) => {
                        LANDED.fetch_add(1, Ordering::Relaxed);
                        i.insert(key, State::Ready(info.clone(), started_reach))
                    }
                    Err(_) if transient => i.remove(&key),
                    Err(e) => {
                        // Read again once its wait is over: the map draws again then (a 429
                        // or 503 says when).
                        crate::cache::notify_at(Instant::now() + crate::cache::retry_wait(e, RETRY));
                        i.insert(key, State::Failed(e.clone(), Instant::now(), started_reach))
                    }
                };
                match old {
                    Some(State::Loading(w, _)) => w,
                    _ => Vec::new(),
                }
            };
            for w in waiting {
                w(r.clone());
            }
            crate::cache::notify();
        }),
    );
}

/// Reads `len` bytes at `at` of the file at `url`: by range from its web address, or from the
/// file on this computer that a local path names ([`is_local_file`]).
fn read(url: &str, at: u64, len: u64, ceiling: crate::address::Reach, done: Done) {
    if len == 0 {
        done(Ok(Vec::new()));
        return;
    }
    if len > MAX_BYTES as u64 {
        done(Err(format!("{len} bytes is too large to read from a GeoTIFF")));
        return;
    }
    let end = at.saturating_add(len) - 1;
    if is_local_file(url) {
        if let Err(e) = local_read_allowed(url, ceiling) {
            done(Err(e));
            return;
        }
        // Read on the fetch workers, as a web address's ranges are, never on the frame; not kept
        // in the tile cache, as the file is at hand.
        crate::cache::fetch_local(format!("{url}{RANGE}{at}-{end}"), done);
        return;
    }
    crate::cache::fetch_counted_with_ceiling(format!("{url}{RANGE}{at}-{end}"), ceiling, done);
}

/// Whether `url` names a GeoTIFF on this computer, by its full path: the desktop reads a large
/// local tiled GeoTIFF in place, as it reads a web address's (#2761).
pub fn is_local_file(url: &str) -> bool {
    // A plain path, as a layer's file is given: a `file:` address isn't read in place.
    !url.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("file:"))
        && crate::pmtiles::is_local(url)
        && crate::pmtiles::check_local_path(url, "a GeoTIFF read in place", &["tif", "tiff"]).is_ok()
}

/// Whether a local GeoTIFF may be read in place: a full path to a `.tif` or `.tiff` file, asked
/// for where this computer may be reached (never by a document limited to public addresses), and
/// not in the browser, which has no files to read by path.
fn local_read_allowed(url: &str, ceiling: crate::address::Reach) -> Result<(), String> {
    if cfg!(target_arch = "wasm32") {
        return Err(message_with_address(url, "the web app reads GeoTIFFs in place from web addresses only"));
    }
    if ceiling != crate::address::Reach::Local {
        return Err(message_with_address(url, "a file on this computer can't be read from here"));
    }
    crate::pmtiles::check_local_path(url, "a GeoTIFF read in place", &["tif", "tiff"])
}

/// Reads a file's header (then its stretch): the start, then whatever more the IFDs need.
///
/// A file an API makes on request (an address not named `.tif`, such as Vantor's
/// `…/geotiff?…`) is asked for its headers first: one that says how long its TIFF header is
/// (`X-TIFF-Header-Length`) has it read in one request. One that says it is JSON, text or a page
/// and doesn't say it answers ranges isn't read as a GeoTIFF at all (a server ignoring the range
/// would send its whole answer); a GeoTIFF mislabelled so on a server that answers ranges is still
/// read. Without an answer, the first [`HEAD`] bytes are read as for any other file.
fn load(url: String, ceiling: crate::address::Reach, done: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>) {
    // A file on this computer: its header read from the file, as any other part of it is.
    if is_local_file(&url) {
        if let Err(e) = local_read_allowed(&url, ceiling) {
            done(Err(e));
            return;
        }
        // The revision the header is read at, taken before it is read, kept with the header.
        #[cfg(not(target_arch = "wasm32"))]
        let done: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send> = {
            let revision = rustgis_data::raster::FileRevision::of(std::path::Path::new(&url));
            Box::new(move |r: Result<Arc<Info>, String>| {
                if let (Ok(info), Some(revision)) = (&r, revision) {
                    let _ = info.revision.set(revision);
                }
                done(r);
            })
        };
        return read_header(url, HEAD, ceiling, done);
    }
    if let Err(e) = crate::address::check_reach(&url, ceiling) {
        done(Err(e));
        return;
    }
    if named_tiff(&url) {
        return read_header(url, HEAD, ceiling, done);
    }
    let u = url.clone();
    crate::cache::head_with_ceiling(
        &url,
        ceiling,
        Box::new(move |h| {
            let h = h.unwrap_or_default();
            if h.content_type.as_deref().is_some_and(not_tiff_type) && !h.accepts_ranges && h.tiff_header_length.is_none() {
                return done(Err(message_with_address(&u, "not a GeoTIFF")));
            }
            read_header(u, header_length(h.tiff_header_length), ceiling, done);
        }),
    );
}

/// Whether an address is named as a TIFF (its path's extension).
fn named_tiff(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url).to_ascii_lowercase();
    path.ends_with(".tif") || path.ends_with(".tiff")
}

/// Whether a `Content-Type` is plainly not a TIFF: JSON, text, a page or XML (a service's error,
/// a feature collection). Anything else, `application/octet-stream` included, is read to see.
fn not_tiff_type(content_type: &str) -> bool {
    let t = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    t.starts_with("text/") || t.ends_with("json") || t.ends_with("xml") || t.ends_with("+json")
}

/// The first read of a file whose server announced its TIFF header's length: that much (one
/// request), bounded as any header read; [`HEAD`] without an announcement.
fn header_length(announced: Option<u64>) -> u64 {
    announced.filter(|n| *n >= 8).map_or(HEAD, |n| n.min(cog::MAX_HEADER).min(MAX_BYTES as u64))
}

/// Reads a file's first `len` bytes, then whatever more the IFDs need.
fn read_header(url: String, len: u64, ceiling: crate::address::Reach, done: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>) {
    let u = url.clone();
    read(
        &url,
        0,
        len,
        ceiling,
        Box::new(move |r| match r {
            Ok(b) if cog::is_tiff(&b) => step(u, Sparse::default().with(0, b), 0, ceiling, done),
            Ok(_) => done(Err(message_with_address(&u, "not a GeoTIFF"))),
            Err(e) => done(Err(message_with_address(&u, &e))),
        }),
    );
}

fn step(url: String, have: Sparse, rounds: usize, ceiling: crate::address::Reach, done: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>) {
    match cog::layout(&have) {
        Ok(Parse::Done(layout)) => stats(url, have, *layout, ceiling, done),
        Ok(Parse::NotTiled(why)) => done(Err(format!("{NOT_COG}: {why}"))),
        Ok(Parse::Need(a, b)) => {
            let Some(len) = header_read(&have, a, b, rounds) else {
                done(Err(message_with_address(&url, "the GeoTIFF's header is too large or too scattered to read in place")));
                return;
            };
            let u = url.clone();
            read(
                &url,
                a,
                len,
                ceiling,
                Box::new(move |r| match r {
                    // Nothing more to read: the file ends at or before `a`. Read again knowing so, a
                    // tag that points past the end (an optional one, GDAL's metadata or a colour
                    // table) is left out and said, as the whole-file read does; one the image needs
                    // fails to decode. Once only: a file whose length is known isn't asked past it.
                    Ok(more) if more.is_empty() && !have.len_known() => step(u, have.with_len(a), rounds + 1, ceiling, done),
                    Ok(more) if more.is_empty() => done(Err(message_with_address(&u, "the GeoTIFF is cut short"))),
                    Ok(more) => step(u, have.with(a, more), rounds + 1, ceiling, done),
                    Err(e) => done(Err(message_with_address(&u, &e))),
                }),
            );
        }
        Err(e) => done(Err(message_with_address(&url, &e.to_string()))),
    }
}

/// How many bytes of the header to read next, from `a`, when the layout stopped for `a..b`
/// after `rounds` reads; `None` past [`MAX_ROUNDS`] or [`cog::MAX_HEADER`], or for a position no
/// range request can name ([`MAX_AT`]). Where `a` is doesn't matter: a part far from the rest (an
/// IFD written after the image data) is a part of its own, one read. The TIFF decoder
/// reads a tag's values 512 bytes at a time, so `a..b` is no longer than that even inside a long
/// array (a large image's tile index, 4 MB for MapBiomas's 346,092 tiles): the step doubles each
/// round, from [`STEP`] up to [`MAX_BYTES`], so such an index takes a few rounds, not one per
/// 256 KB. A read never takes what is held past [`cog::MAX_HEADER`], and stops where a part
/// already read starts, so no byte is held twice.
fn header_read(have: &Sparse, a: u64, b: u64, rounds: usize) -> Option<u64> {
    if rounds >= MAX_ROUNDS || have.held() >= cog::MAX_HEADER || a > MAX_AT {
        return None;
    }
    let room = cog::MAX_HEADER - have.held();
    // What the layout stopped for has to fit in what is left, or the read would be wasted.
    if b.saturating_sub(a) > room {
        return None;
    }
    // A part read after `a` (a round that went past it, or the tags at the header's end) isn't
    // read again.
    let gap = have.next_start(a).map_or(u64::MAX, |next| next - a);
    // `rounds` is below MAX_ROUNDS here, so the shift stays far from overflowing.
    Some(b.saturating_sub(a).max(STEP << rounds).min(MAX_BYTES as u64).min(room).min(gap))
}

/// What a file that isn't a cloud-optimised GeoTIFF says (it is read whole instead).
pub const NOT_COG: &str = "not a cloud-optimised GeoTIFF";

/// Reads the coarsest overview (a few tiles) for the stretch, then the file is ready.
fn stats(
    url: String,
    header: Sparse,
    layout: Layout,
    ceiling: crate::address::Reach,
    done: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>,
) {
    let transformer = match layout.crs.transformer() {
        Ok(t) => t,
        Err(e) => {
            done(Err(message_with_address(&url, &e)));
            return;
        }
    };
    let Some(full) = layout.levels.first().cloned() else {
        done(Err(message_with_address(&url, "no image")));
        return;
    };
    // Bounds: the full image's edges, sampled (a projected square isn't square in degrees).
    let t = layout.transform;
    let (w, h) = (f64::from(full.width), f64::from(full.height));
    let mut lon = (f64::MAX, f64::MIN);
    let mut lat = (f64::MAX, f64::MIN);
    for i in 0..=8 {
        let f = f64::from(i) / 8.0;
        for (px, py) in [(f * w, 0.0), (f * w, h), (0.0, f * h), (w, f * h)] {
            let c = Coord::new(t.origin_x + px * t.pixel_width, t.origin_y + py * t.pixel_height);
            if let Some(ll) = transformer.to_lonlat(c) {
                lon = (lon.0.min(ll.x), lon.1.max(ll.x));
                lat = (lat.0.min(ll.y), lat.1.max(ll.y));
            }
        }
    }
    if lon.0 > lon.1 || lat.0 > lat.1 {
        done(Err(message_with_address(&url, "the GeoTIFF's corners aren't on the globe")));
        return;
    }
    let bounds = [lon.0.max(-180.0), lat.0.max(-85.05), lon.1.min(180.0), lat.1.min(85.05)];
    let merc = {
        let a = rustgis_geom::lonlat_to_merc(Coord::new(bounds[0], bounds[1]));
        let b = rustgis_geom::lonlat_to_merc(Coord::new(bounds[2], bounds[3]));
        [a.x, a.y, b.x, b.y]
    };
    // The zoom whose pixels match the full image's.
    let res = (merc[2] - merc[0]).abs() / w.max(1.0);
    let max_zoom = if res > 0.0 { (2.0 * HALF / f64::from(SIZE) / res).log2().ceil().clamp(0.0, 24.0) as u8 } else { 18 };
    let alpha = match layout.color {
        ColorHint::Rgb { alpha: true } if layout.bands >= 4 => Some(3),
        ColorHint::GrayAlpha if layout.bands >= 2 => Some(1),
        _ => None,
    };
    let rgb = match layout.color {
        ColorHint::Rgb { .. } => layout.bands >= 3,
        ColorHint::Gray => layout.bands == 3,
        _ => false,
    };
    let bytes = layout.data_type == "UInt8";
    let info = Arc::new(Info {
        reach: ceiling,
        layout,
        header,
        transformer,
        bounds,
        merc,
        min_zoom: 0,
        max_zoom,
        stretch: Mutex::new(HashMap::new()),
        stretching: Mutex::new(HashMap::new()),
        stretch_failed: Mutex::new(HashMap::new()),
        alpha,
        #[cfg(not(target_arch = "wasm32"))]
        revision: OnceLock::new(),
    });
    // 8-bit colour images are shown as they are; others between the coarsest overview's
    // percentiles (a palette image's own look needs none: its table colours its values, see
    // `with_stretch`).
    let own = Look::default();
    if bytes && rgb {
        let colour = info.colour(&own);
        let n = colour.len();
        info.stretch.lock().unwrap_or_else(|e| e.into_inner()).insert((colour, false), vec![(0.0, 255.0); n]);
        done(Ok(info));
        return;
    }
    // Any other look's stretch is read when a tile first needs it (`with_stretch`): a header read
    // for a window or an in-place overview reads nothing more.
    done(Ok(info));
}

/// The image the stretch is read from: of those few enough tiles to read for it, the coarsest of
/// at least 256 × 256 pixels (GDAL writes overviews down to a pixel, too few to take percentiles
/// of), else the finest. A long strip's overviews are narrow: one 256 pixels wide is too many tiles
/// long, a narrower one plenty of pixels. None small enough: the coarsest (the type's range is
/// shown).
fn stats_level(levels: &[Level]) -> usize {
    let few = |l: &Level| (l.tiles() as usize) <= MAX_STATS_TILES;
    let pixels = |l: &Level| u64::from(l.width) * u64::from(l.height);
    levels
        .iter()
        .rposition(|l| few(l) && pixels(l) >= 256 * 256)
        .or_else(|| levels.iter().position(few))
        .unwrap_or(levels.len().saturating_sub(1))
}

/// The full range of a file's type of values, as a look shows them (floats have none: 0–255 is a
/// rough default for them).
fn type_range(layout: &Layout, aef: bool) -> (f32, f32) {
    let r = match layout.data_type {
        "Int8" => (-128.0, 127.0),
        "Int16" => (-32768.0, 32767.0),
        "UInt16" => (0.0, 65535.0),
        "Int32" => (-2_147_483_648.0, 2_147_483_647.0),
        "UInt32" => (0.0, 4_294_967_295.0),
        _ => (0.0, 255.0),
    };
    (shown_value(r.0, aef), shown_value(r.1, aef))
}

/// Keeps a look's stretch: a few looks a file (a script trying many is hostile input); past them,
/// the others are read again when asked for.
fn keep_stretch(info: &Info, key: (Vec<usize>, bool), s: Vec<(f32, f32)>) {
    let own = (info.colour(&Look::default()), false);
    let mut kept = info.stretch.lock().unwrap_or_else(|e| e.into_inner());
    // Past the cap one other look goes, never the file's own (an 8-bit colour image's is "as it
    // is", not read).
    if kept.len() >= MAX_LOOKS
        && let Some(k) = kept.keys().find(|k| **k != own).cloned()
    {
        kept.remove(&k);
    }
    kept.insert(key, s);
}

/// The (low, high) values of a band's 2nd and 98th percentiles.
fn percentiles(mut v: Vec<f32>) -> (f32, f32) {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return (0.0, 1.0);
    }
    v.sort_by(f32::total_cmp);
    let at = |q: f64| v.get(((v.len() - 1) as f64 * q) as usize).copied().unwrap_or(0.0);
    let (lo, hi) = (at(0.02), at(0.98));
    if hi > lo { (lo, hi) } else { (lo, lo + 1.0) }
}

/// Calls `then` with a look's stretch: given, known, or read from the coarsest overview (a few
/// tiles of the bands shown) and kept.
type StretchThen = Box<dyn FnOnce(Result<Vec<(f32, f32)>, String>) + Send>;

/// Shares computed stretches; a retired read cannot leave fallback values or a retry delay.
fn with_stretch(info: Arc<Info>, file: String, look: Look, then: StretchThen) {
    let then = caller_callback(then);
    if let Err(e) = crate::cache::check_still(&crate::cache::current_still()) {
        then(Err(e));
        return;
    }
    if let Some(s) = info.stretch_for(&look) {
        then(Ok(s));
        return;
    }
    // A palette image's own look colours its values by its table (see `paint`): no stretch to
    // read, and none kept, which a look of its bands would take for its own.
    if info.palette(&look).is_some() {
        then(Ok(vec![(0.0, 255.0)]));
        return;
    }
    let colour = info.colour(&look);
    let coarsest = stats_level(&info.layout.levels);
    let n = info.layout.levels.get(coarsest).map(|l| l.tiles() as usize).unwrap_or(0);
    // Too many tiles to read for it: the values' full range for the type.
    if n == 0 || n > MAX_STATS_TILES {
        let s = vec![type_range(&info.layout, look.aef); colour.len()];
        keep_stretch(&info, (colour, look.aef), s.clone());
        then(Ok(s));
        return;
    }
    // Read once: later asks wait for the read under way.
    let key = (colour.clone(), look.aef);
    // A read that failed lately: the type's full range for now.
    if info.stretch_failed.lock().unwrap_or_else(|e| e.into_inner()).get(&key).is_some_and(|at| at.elapsed() < RETRY) {
        then(Ok(vec![type_range(&info.layout, look.aef); colour.len()]));
        return;
    }
    {
        let mut reading = info.stretching.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(waiting) = reading.get_mut(&key) {
            waiting.push(then);
            return;
        }
        reading.insert(key.clone(), vec![then]);
    }
    let tiles: Vec<u32> = (0..n as u32).collect();
    let i = info.clone();
    let shown_bands = colour.len();
    let key_aef = look.aef;
    let done = move |r: Result<Vec<(f32, f32)>, String>| {
        let r = crate::cache::check_still(&crate::cache::current_still()).and(r);
        let waiting = i.stretching.lock().unwrap_or_else(|e| e.into_inner()).remove(&key).unwrap_or_default();
        // A server error uses the type's full range until RETRY passes. Dropped or retired
        // reads propagate their refusal without leaving a shared stretch or retry delay.
        let s = match r {
            Ok(s) => {
                keep_stretch(&i, key, s.clone());
                Ok(s)
            }
            Err(e) if !crate::cache::keep_failure(&e, &crate::cache::current_still()) => Err(e),
            Err(_) => {
                let mut failed = i.stretch_failed.lock().unwrap_or_else(|e| e.into_inner());
                if failed.len() >= MAX_LOOKS {
                    failed.clear();
                }
                failed.insert(key, Instant::now());
                Ok(vec![type_range(&i.layout, key_aef); shown_bands])
            }
        };
        for w in waiting {
            w(s.clone());
        }
    };
    // The colour bands, and the alpha band where the file has one: a cell it hides is left out,
    // as the map leaves it unpainted (#1810), so a value a see-through cell holds (−3.4e38 in a
    // Float32 image) doesn't set the stretch.
    let read = info.shown_from(colour.clone());
    let alpha = info.alpha.and_then(|a| read.iter().rposition(|b| *b == a));
    let opaque = rustgis_data::raster::alpha_max(info.layout.data_type);
    fetch_bands(
        info,
        file,
        coarsest,
        tiles,
        read,
        Box::new(move |r| match r {
            Ok(picked) => {
                let s: Vec<(f32, f32)> = (0..colour.len())
                    .map(|j| {
                        // The alpha band shown as a colour isn't hidden by itself.
                        let alpha = alpha.filter(|a| *a != j);
                        percentiles(
                            picked
                                .iter()
                                .filter_map(|t| {
                                    let band = t.get(j).and_then(|(chunk, b)| chunk.get(*b))?;
                                    Some((band, alpha.and_then(|a| t.get(a)).and_then(|(chunk, b)| chunk.get(*b))))
                                })
                                .flat_map(|(band, hides)| {
                                    band.iter()
                                        .enumerate()
                                        .filter(move |(i, _)| {
                                            hides.is_none_or(|al| {
                                                al.get(*i).is_some_and(|&a| !rustgis_data::raster::see_through(a, !a.is_nan(), opaque))
                                            })
                                        })
                                        .map(|(_, x)| shown_value(*x, look.aef))
                                })
                                .collect(),
                        )
                    })
                    .collect();
                done(Ok(s))
            }
            Err(e) => done(Err(e)),
        }),
    );
}

/// The bands of a tile a look shows: for each, the decoded chunk holding it and its place there.
type Picked = Vec<(Bands, usize)>;

/// Reads the bands `bands` (0-based) of some tiles of a level, then calls `then` with each tile's
/// (in the order asked). Bands stored apart are read alone; together, a tile's chunk once.
fn fetch_bands(
    info: Arc<Info>,
    file: String,
    level: usize,
    tiles: Vec<u32>,
    bands: Vec<usize>,
    then: Box<dyn FnOnce(Result<Vec<Picked>, String>) + Send>,
) {
    let Some(l) = info.layout.levels.get(level) else {
        then(Err("no such overview".into()));
        return;
    };
    let mut places = Vec::with_capacity(tiles.len() * bands.len());
    for &t in &tiles {
        for &b in &bands {
            match info.layout.chunk(l, t, b) {
                Some(p) => places.push(p),
                None => {
                    then(Err(format!("no band {} in tile {t}", b + 1)));
                    return;
                }
            }
        }
    }
    let mut chunks: Vec<u32> = places.iter().map(|(c, _)| *c).collect();
    chunks.sort_unstable();
    chunks.dedup();
    let want = chunks.clone();
    let nb = bands.len();
    fetch_all(
        info,
        file,
        level,
        want,
        Box::new(move |r| {
            then(r.and_then(|got| {
                if got.len() != chunks.len() {
                    return Err("a GeoTIFF tile went missing".into());
                }
                Ok(places
                    .chunks(nb.max(1))
                    .map(|tile| {
                        tile.iter()
                            .filter_map(|(c, at)| chunks.binary_search(c).ok().and_then(|i| got.get(i)).map(|b| (b.clone(), *at)))
                            .collect()
                    })
                    .collect())
            }))
        }),
    );
}

type Gathered = Box<dyn FnOnce(Result<Vec<Bands>, String>) + Send>;

/// How many times the fetch queue may drop the tiles of a joint read of `tiles` before it fails:
/// eight a tile, 64 at least, so a busy queue delays a read but one that never gets its turn ends
/// (the browser has no other end to it).
fn drops_allowed(tiles: usize) -> usize {
    tiles.saturating_mul(8).max(64)
}

/// Tiles a joint read ([`fetch_all`]) has under way at once: half the fetch queue, so a read of
/// many (the overview of a file with few overviews) doesn't push its own requests out of it.
pub(super) const AT_ONCE: usize = 256;

/// Reads tiles of a level (each once, shared by the map tiles that need it), then calls `then`
/// with their bands in the order asked. [`AT_ONCE`] are under way at a time, and a tile the fetch
/// queue dropped (other reads filled it: see [`crate::cache::DROPPED`]) is asked for again, so a
/// drop is a delay, not a failure, until the drops pass [`drops_allowed`]: then the read fails,
/// saying so, rather than asking without end (the layer reads again later).
fn fetch_all(info: Arc<Info>, url: String, level: usize, wanted: Vec<u32>, then: Gathered) {
    fetch_all_with(Arc::new(get_tile), info, url, level, wanted, then)
}

/// How [`fetch_all_with`] gets a tile: [`get_tile`], or a test's stand-in.
type GetTile = Arc<dyn Fn(Arc<Info>, String, usize, u32, Box<dyn FnOnce(Result<Bands, String>) + Send>) + Send + Sync>;

/// [`fetch_all`] through `get`.
fn fetch_all_with(get: GetTile, info: Arc<Info>, url: String, level: usize, wanted: Vec<u32>, then: Gathered) {
    if wanted.is_empty() {
        then(Ok(Vec::new()));
        return;
    }
    struct Gather {
        get: GetTile,
        info: Arc<Info>,
        url: String,
        level: usize,
        wanted: Vec<u32>,
        got: Vec<Option<Bands>>,
        /// Places in `wanted` not asked for yet (or dropped, to ask for again), the next last.
        to_ask: Vec<usize>,
        in_flight: usize,
        /// Someone is asking for the next tiles, and asks again after: a tile answered at once
        /// leaves it to them (asking from the answer would nest as deep as the tiles kept).
        asking: bool,
        /// Tiles the fetch queue dropped so far, bounded by [`drops_allowed`].
        dropped: usize,
        failed: Option<String>,
        then: Option<Gathered>,
    }
    let n = wanted.len();
    let g = Arc::new(Mutex::new(Gather {
        get,
        info,
        url,
        level,
        wanted,
        got: vec![None; n],
        to_ask: (0..n).rev().collect(),
        in_flight: 0,
        asking: false,
        dropped: 0,
        failed: None,
        then: Some(then),
    }));
    ask(&g);

    /// Asks for the next tiles, [`AT_ONCE`] under way at most, until all are here or one failed
    /// (then once the rest have answered), and answers `then` once.
    fn ask(g: &Arc<Mutex<Gather>>) {
        loop {
            let (get, info, url, level, batch) = {
                let mut s = g.lock().unwrap_or_else(|e| e.into_inner());
                if s.asking {
                    return;
                }
                if s.in_flight == 0 && (s.to_ask.is_empty() || s.failed.is_some()) {
                    if let Some(then) = s.then.take() {
                        let r = match s.failed.take() {
                            Some(e) => Err(e),
                            None => Ok(s.got.iter_mut().filter_map(Option::take).collect()),
                        };
                        drop(s);
                        then(r);
                    }
                    return;
                }
                let mut batch = Vec::new();
                while s.in_flight < AT_ONCE && s.failed.is_none() {
                    let Some(k) = s.to_ask.pop() else { break };
                    let Some(&idx) = s.wanted.get(k) else { continue };
                    s.in_flight += 1;
                    batch.push((k, idx));
                }
                if batch.is_empty() {
                    return;
                }
                s.asking = true;
                (s.get.clone(), s.info.clone(), s.url.clone(), s.level, batch)
            };
            for (k, idx) in batch {
                let g = g.clone();
                get(
                    info.clone(),
                    url.clone(),
                    level,
                    idx,
                    Box::new(move |r| {
                        {
                            let mut s = g.lock().unwrap_or_else(|e| e.into_inner());
                            s.in_flight = s.in_flight.saturating_sub(1);
                            match r {
                                Ok(b) => {
                                    if let Some(slot) = s.got.get_mut(k) {
                                        *slot = Some(b);
                                    }
                                }
                                // Dropped: asked for again, within the drops allowed (a batch
                                // already asked for answers after the read failed: not counted).
                                Err(e) if e == crate::cache::DROPPED => {
                                    if s.failed.is_none() {
                                        s.dropped += 1;
                                        if s.dropped > drops_allowed(s.wanted.len()) {
                                            s.failed = Some(format!(
                                                "the fetch queue dropped its tiles {} times (other reads filled it): busy",
                                                s.dropped
                                            ));
                                        } else {
                                            s.to_ask.push(k);
                                        }
                                    }
                                }
                                Err(e) => s.failed = Some(e),
                            }
                        }
                        ask(&g);
                    }),
                );
            }
            g.lock().unwrap_or_else(|e| e.into_inner()).asking = false;
        }
    }
}

/// One tile of a level, decoded: from the tiles kept, or read.
fn get_tile(info: Arc<Info>, url: String, level: usize, idx: u32, then: Box<dyn FnOnce(Result<Bands, String>) + Send>) {
    let ceiling = info.reach.min(crate::cache::ceiling_for(&url));
    get_tile_at(info, url, level, idx, ceiling, then);
}

/// A retry can narrow its reader's original ceiling, never reconstruct a wider one.
fn get_tile_at(
    info: Arc<Info>,
    url: String,
    level: usize,
    idx: u32,
    ceiling: crate::address::Reach,
    then: Box<dyn FnOnce(Result<Bands, String>) + Send>,
) {
    let then = caller_callback(then);
    if let Err(e) = crate::cache::check_still(&crate::cache::current_still()) {
        then(Err(e));
        return;
    }
    if let Err(e) = crate::logins::check(&url) {
        then(Err(e.to_string()));
        return;
    }
    let Some(l) = info.layout.levels.get(level) else {
        then(Err("no such overview".into()));
        return;
    };
    let (Some(&at), Some(&len)) = (l.offsets.get(idx as usize), l.counts.get(idx as usize)) else {
        then(Err("no such tile".into()));
        return;
    };
    let nb = info.layout.chunk_bands();
    let per_band = (l.tile_width as usize) * (l.tile_height as usize);
    let samples = per_band * nb;
    let started_reach = ceiling.min(info.reach).min(crate::cache::ceiling_for(&url));
    // The header this read decodes with. A local file's read in flight is joined only by a
    // reader with the same header: one holding the header of a file written again since waits for
    // it to land, then reads the tile again (#2814), so no old tile reaches its overview.
    let header = Arc::as_ptr(&info) as usize;
    let joins = |reach: crate::address::Reach, from: usize| reach == started_reach && (from == header || !is_local_file(&url));
    // A tile the file leaves out (GDAL's sparse files: no bytes) is empty: kept as any other, so
    // a window over it is whole at once rather than asking for it at every draw.
    if len == 0 {
        let key = (url.clone(), level, idx);
        let mut t = tiles().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(TileState::Loading(waiting, reach, from)) = t.map.get_mut(&key) {
            if joins(*reach, *from) {
                waiting.push(then);
            } else {
                waiting.push(crate::cache::caller_retry(Box::new(move |_| get_tile_at(info, url, level, idx, started_reach, then))));
            }
            return;
        }
        let empty = match t.used(&key, started_reach) {
            Some(b) => b,
            None => {
                let b: Bands = Arc::new(vec![vec![f32::NAN; per_band]; nb].into());
                // Kept only under the file's current header, as a decoded tile is (a local file
                // written again since is read again).
                let current = !is_local_file(&url)
                    || matches!(infos().lock().unwrap_or_else(|e| e.into_inner()).get(&url), Some(State::Ready(i, _)) if Arc::ptr_eq(i, &info));
                if current {
                    t.keep(key, b.clone(), samples, started_reach);
                }
                b
            }
        };
        drop(t);
        then(Ok(empty));
        return;
    }
    // Bytes at offset 0 would be the file's header: a broken index, said rather than drawn blank.
    if at == 0 {
        then(Err(format!("tile {idx} of level {level} has bytes but no offset")));
        return;
    }
    let key = (url.clone(), level, idx);
    {
        let mut t = tiles().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(b) = t.used(&key, started_reach) {
            drop(t);
            then(Ok(b));
            return;
        }
        match t.map.get_mut(&key) {
            Some(TileState::Ready(..)) => {
                if let Some(TileState::Ready(_, n, used, _)) = t.map.remove(&key) {
                    t.samples = t.samples.saturating_sub(n);
                    t.order.remove(&used);
                }
                t.map.insert(key.clone(), TileState::Loading(vec![then], started_reach, header));
            }
            Some(TileState::Loading(w, reach, from)) => {
                if joins(*reach, *from) {
                    w.push(then);
                } else {
                    w.push(crate::cache::caller_retry(Box::new(move |_| get_tile_at(info, url, level, idx, started_reach, then))));
                }
                return;
            }
            None => {
                t.map.insert(key.clone(), TileState::Loading(vec![then], started_reach, header));
            }
        }
    }
    read(
        &url,
        at,
        len,
        started_reach,
        Box::new(move |r| {
            let r = r
                .and_then(|b| cog::decode_tile_counted(&info.header, &info.layout, level, idx, b).map_err(|e| e.to_string()))
                .map(Arc::new);
            let r = crate::cache::check_still(&crate::cache::current_still()).and(r);
            let r = crate::logins::check(&key.0).map_err(str::to_string).and(r);
            let mut r = r;
            let waiting = {
                let mut t = tiles().lock().unwrap_or_else(|e| e.into_inner());
                // A local file's tile decoded with a header since forgotten (the file written
                // again while it was read) is handed on, not kept: the next draw reads it with the
                // new one. Looked at under the tiles' lock: a header is forgotten before its tiles
                // are, under this lock, so a tile kept here goes with them.
                let superseded = is_local_file(&key.0)
                    && !matches!(infos().lock().unwrap_or_else(|e| e.into_inner()).get(&key.0), Some(State::Ready(i, _)) if Arc::ptr_eq(i, &info));
                if started_reach > crate::cache::ceiling_for(&key.0) {
                    r = Err(REACH_CHANGED.into());
                }
                let old = match &r {
                    Ok(_) if superseded => t.map.remove(&key),
                    // Over the budget: the tiles used longest ago make room.
                    Ok(b) => t.keep(key, b.clone(), samples, started_reach),
                    // A failed tile is read again by the next map tile that needs it.
                    Err(_) => t.map.remove(&key),
                };
                match old {
                    Some(TileState::Loading(w, ..)) => w,
                    _ => Vec::new(),
                }
            };
            for w in waiting {
                w(r.clone());
            }
        }),
    );
}

/// `cog:z/x/y@address` → its parts.
fn parse_url(url: &str) -> Option<(u8, u32, u32, String)> {
    let rest = url.strip_prefix(SCHEME)?;
    let (zxy, archive) = rest.split_once('@')?;
    let mut p = zxy.split('/');
    let z: u8 = p.next()?.parse().ok()?;
    let x: u32 = p.next()?.parse().ok()?;
    let y: u32 = p.next()?.parse().ok()?;
    (z <= 30 && !archive.is_empty()).then(|| (z, x, y, archive.to_string()))
}

/// Draws a map tile (`cog:z/x/y@address`): empty where the file has nothing.
pub(crate) fn fetch(url: &str, done: Done) {
    let Some((z, x, y, archive)) = parse_url(url) else {
        done(Err(message_with_address(url, "not a COG tile address")));
        return;
    };
    let (file, look) = split(&archive);
    let file = file.to_string();
    let f = file.clone();
    with_info(
        &file,
        Box::new(move |info| match info {
            Ok(info) => {
                let i = info.clone();
                let f2 = f.clone();
                with_stretch(
                    info,
                    f,
                    look.clone(),
                    Box::new(move |s| match s {
                        Ok(s) => draw(i, f2, &look, &s, z, x, y, done),
                        Err(e) => done(Err(e)),
                    }),
                );
            }
            Err(e) => done(Err(e)),
        }),
    );
}

/// Where each pixel of a map tile falls in the file, at a level: (tile, sample) or none.
fn locate(info: &Info, level: usize, native: &[Option<Coord>]) -> Vec<Option<(u32, usize)>> {
    let Some(l) = info.layout.levels.get(level) else { return vec![None; native.len()] };
    let t = info.layout.transform_of(l);
    let (tw, th) = (l.tile_width.max(1), l.tile_height.max(1));
    native
        .iter()
        .map(|c| {
            let c = (*c)?;
            let px = (c.x - t.origin_x) / t.pixel_width;
            let py = (c.y - t.origin_y) / t.pixel_height;
            if !(px >= 0.0 && py >= 0.0 && px < f64::from(l.width) && py < f64::from(l.height)) {
                return None;
            }
            let (px, py) = (px as u32, py as u32);
            let idx = (py / th) * l.across() + px / tw;
            Some((idx, ((py % th) * tw + px % tw) as usize))
        })
        .collect()
}

fn draw(info: Arc<Info>, url: String, look: &Look, stretch: &[(f32, f32)], z: u8, x: u32, y: u32, done: Done) {
    let n = (1u64 << z) as f64;
    let size = 2.0 * HALF / n;
    let (x0, y1) = (-HALF + f64::from(x) * size, HALF - f64::from(y) * size);
    let [mx0, my0, mx1, my1] = info.merc;
    if x0 > mx1 || x0 + size < mx0 || y1 < my0 || y1 - size > my1 {
        done(Ok(Vec::new()));
        return;
    }
    // Native coordinates of every pixel's centre: exact on a grid every 16 pixels, between
    // them interpolated (projections are smooth at a tile's scale).
    const STEP_PX: usize = 16;
    let px = size / f64::from(SIZE);
    let s = SIZE as usize;
    let g = s / STEP_PX + 1;
    let grid: Vec<Option<Coord>> = (0..g * g)
        .map(|k| {
            let (i, j) = ((k % g) * STEP_PX, (k / g) * STEP_PX);
            info.transformer.from_merc(Coord::new(x0 + i as f64 * px, y1 - j as f64 * px))
        })
        .collect();
    let native: Vec<Option<Coord>> = (0..s * s)
        .map(|k| {
            let (i, j) = (k % s, k / s);
            let (fx, fy) = ((i as f64 + 0.5) / STEP_PX as f64, (j as f64 + 0.5) / STEP_PX as f64);
            let (gi, gj) = ((fx as usize).min(g - 2), (fy as usize).min(g - 2));
            let (tx, ty) = (fx - gi as f64, fy - gj as f64);
            let at = |a: usize, b: usize| grid.get(b * g + a).copied().flatten();
            match (at(gi, gj), at(gi + 1, gj), at(gi, gj + 1), at(gi + 1, gj + 1)) {
                (Some(a), Some(b), Some(c), Some(d)) => {
                    let top = (a.x + (b.x - a.x) * tx, a.y + (b.y - a.y) * tx);
                    let bottom = (c.x + (d.x - c.x) * tx, c.y + (d.y - c.y) * tx);
                    Some(Coord::new(top.0 + (bottom.0 - top.0) * ty, top.1 + (bottom.1 - top.1) * ty))
                }
                // Near a projection's edge: exactly.
                _ => info.transformer.from_merc(Coord::new(x0 + (i as f64 + 0.5) * px, y1 - (j as f64 + 0.5) * px)),
            }
        })
        .collect();
    // The output pixel's size in the file's units, from the grid across the tile.
    let target = match (grid.first().copied().flatten(), grid.get(g - 1).copied().flatten()) {
        (Some(a), Some(b)) => ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt() / s as f64,
        _ => f64::MAX,
    };
    // The coarsest overview fine enough for the tile; a coarser one when too many tiles of it
    // are needed.
    let levels = &info.layout.levels;
    let mut level = levels
        .iter()
        .enumerate()
        .rev()
        .find(|(_, l)| info.layout.transform_of(l).pixel_width.abs() <= target * 1.25)
        .map(|(i, _)| i)
        .unwrap_or(0);
    let (spots, wanted) = loop {
        let spots = locate(&info, level, &native);
        let mut wanted: Vec<u32> = spots.iter().flatten().map(|(t, _)| *t).collect();
        wanted.sort_unstable();
        wanted.dedup();
        if wanted.len() <= MAX_PER_TILE || level + 1 >= levels.len() {
            break (spots, wanted);
        }
        level += 1;
    };
    if wanted.is_empty() {
        done(Ok(Vec::new()));
        return;
    }
    if wanted.len() > MAX_PER_TILE * 4 {
        done(Err("too many GeoTIFF tiles for one map tile".into()));
        return;
    }
    let colour = info.colour(look);
    let colours = colour.len();
    let shown = info.shown_from(colour);
    let pen = Pen {
        rgb: colours >= 3,
        // The alpha band where it is among those shown: after the colours, or one of them.
        alpha: info.alpha.and_then(|a| shown.iter().rposition(|b| *b == a)),
        alpha_max: rustgis_data::raster::alpha_max(info.layout.data_type),
        palette: info.palette(look).map(<[[u8; 4]]>::to_vec),
        aef: look.aef,
        stretch: stretch.to_vec(),
    };
    let w2 = wanted.clone();
    fetch_bands(
        info,
        url,
        level,
        wanted,
        shown,
        Box::new(move |r| match r {
            Ok(bands) => done(paint(&pen, &spots, &w2, &bands)),
            Err(e) => done(Err(e)),
        }),
    );
}

/// How a map tile's pixels are coloured from the bands shown.
struct Pen {
    /// Three bands as red, green and blue (else one, grey).
    rgb: bool,
    /// Where the alpha band is among those shown, after the colour ones, and its opaque value.
    alpha: Option<usize>,
    alpha_max: f32,
    /// A palette image's colours, by value (no stretch).
    palette: Option<Vec<[u8; 4]>>,
    aef: bool,
    stretch: Vec<(f32, f32)>,
}

/// The map tile's pixels, from the tiles read, as a raw picture (none when nothing is there).
fn paint(pen: &Pen, spots: &[Option<(u32, usize)>], wanted: &[u32], tiles: &[Picked]) -> Result<Vec<u8>, String> {
    let scale = |b: usize, v: f32| -> u8 {
        let (lo, hi) = pen.stretch.get(b).copied().unwrap_or((0.0, 255.0));
        let v = shown_value(v, pen.aef);
        (((v - lo) / (hi - lo)).clamp(0.0, 1.0) * 255.0).round() as u8
    };
    let mut rgba = vec![0u8; spots.len() * 4];
    let mut any = false;
    for (k, spot) in spots.iter().enumerate() {
        let Some((t, at)) = spot else { continue };
        let Some(bands) = wanted.binary_search(t).ok().and_then(|i| tiles.get(i)) else { continue };
        let v = |b: usize| bands.get(b).and_then(|(chunk, i)| chunk.get(*i)).and_then(|band| band.get(*at)).copied().unwrap_or(f32::NAN);
        // The alpha band's value, 0 to 255 (opaque without one); none where it hides the pixel.
        let a = match pen.alpha {
            Some(i) => {
                let a = v(i);
                if a.is_nan() || a <= 0.0 {
                    continue;
                }
                ((a / pen.alpha_max).clamp(0.0, 1.0) * 255.0).round() as u8
            }
            None => 255,
        };
        let px = if pen.rgb {
            let (r, g, b) = (v(0), v(1), v(2));
            if !(r.is_finite() && g.is_finite() && b.is_finite()) {
                continue;
            }
            [scale(0, r), scale(1, g), scale(2, b), a]
        } else if let Some(table) = &pen.palette {
            let g = v(0);
            // A value past the table: nothing.
            let Some(c) = (g.is_finite() && g >= 0.0).then(|| table.get(g as usize)).flatten() else { continue };
            if c[3] == 0 {
                continue;
            }
            [c[0], c[1], c[2], ((u16::from(c[3]) * u16::from(a)) / 255) as u8]
        } else {
            let g = v(0);
            if !g.is_finite() {
                continue;
            }
            let g = scale(0, g);
            [g, g, g, a]
        };
        if let Some(out) = rgba.get_mut(k * 4..k * 4 + 4) {
            out.copy_from_slice(&px);
            any = true;
        }
    }
    if !any {
        return Ok(Vec::new());
    }
    bleed(&mut rgba, spots);
    let mut out = Vec::with_capacity(crate::cache::RAW.len() + 8 + rgba.len());
    out.extend_from_slice(crate::cache::RAW);
    out.extend_from_slice(&SIZE.to_le_bytes());
    out.extend_from_slice(&SIZE.to_le_bytes());
    out.extend_from_slice(&rgba);
    Ok(out)
}

/// Carries the file's edge one pixel out: each pixel outside the file (`spots` none) beside one
/// painted, or touching one at a corner, takes its colour. A map tile is drawn scaled with
/// bilinear filtering, which turns the step from painted to empty into a half-transparent pixel;
/// at the edge between two files laid side by side (a data cube's tiles) the two half-transparent
/// edges let the map show through as a thin line. Carried out, the step falls outside the file and
/// its own pixels stay opaque. Pixels with no data inside the file are left empty. Where no file
/// lies beside, at the data's outer edge, the picture grows by that pixel (1/256 of a map tile).
/// The colour is copied whole, alpha included, so a half-transparent edge is carried out as it is.
/// A file's edge on a map tile's own edge needs none: a tile is drawn clamped at its edges, and the
/// pixels past them are the next tile's.
fn bleed(rgba: &mut [u8], spots: &[Option<(u32, usize)>]) {
    let s = SIZE as usize;
    let painted = |k: usize| rgba.get(k * 4 + 3).is_some_and(|a| *a > 0);
    // A pixel of the file with a colour: what the edge is carried out from.
    let source = |m: usize| spots.get(m).is_some_and(Option::is_some) && painted(m);
    let mut fill: Vec<(usize, usize)> = Vec::new();
    for (k, spot) in spots.iter().enumerate() {
        if spot.is_some() || painted(k) {
            continue;
        }
        let (i, j) = (k % s, k / s);
        let (west, east, north, south) = (i > 0, i + 1 < s, j > 0, j + 1 < s);
        // Beside first, then at a corner, where filtering at the file's corner reaches too.
        let near = [
            west.then(|| k - 1),
            east.then(|| k + 1),
            north.then(|| k - s),
            south.then(|| k + s),
            (north && west).then(|| k - s - 1),
            (north && east).then(|| k - s + 1),
            (south && west).then(|| k + s - 1),
            (south && east).then(|| k + s + 1),
        ];
        if let Some(from) = near.into_iter().flatten().find(|m| source(*m)) {
            fill.push((k, from));
        }
    }
    for (k, from) in fill {
        rgba.copy_within(from * 4..from * 4 + 4, k * 4);
    }
}

/// Whether a failure says the file is a GeoTIFF to read whole rather than in place.
fn read_whole(e: &str) -> bool {
    e.contains(NOT_COG) || e.contains("not a GeoTIFF")
}

/// Whether the file at `url` is a cloud-optimised GeoTIFF, waiting for the answer (the command
/// line): `Ok(false)` for any other file, read whole instead.
#[cfg(not(target_arch = "wasm32"))]
pub fn probe_blocking(url: &str) -> Result<bool, String> {
    let url = split(url).0;
    let (tx, rx) = std::sync::mpsc::channel();
    with_info(url, Box::new(move |r| tx.send(r.map(|_| ())).unwrap_or_default()));
    match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(Ok(())) => Ok(true),
        Ok(Err(e)) if read_whole(&e) => Ok(false),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(message_with_address(url, "no answer in a minute")),
    }
}

/// Whether the file at `url` is a cloud-optimised GeoTIFF, off the frame (the app): `then` gets
/// `Ok(false)` for any other file, read whole instead.
pub fn probe(url: &str, then: Box<dyn FnOnce(Result<bool, String>) + Send>) {
    let url = split(url).0;
    with_info(
        url,
        Box::new(move |r| {
            then(match r {
                Ok(_) => Ok(true),
                Err(e) if read_whole(&e) => Ok(false),
                Err(e) => Err(e),
            })
        }),
    );
}

/// The most tiles of a file one read of values takes.
#[cfg(not(target_arch = "wasm32"))]
pub const MAX_VALUE_TILES: usize = 4096;

/// The most values one read returns: 64 million, 256 MB, refused before anything is allocated.
#[cfg(not(target_arch = "wasm32"))]
pub const MAX_VALUES_READ: u64 = 64_000_000;

/// A file's header, waiting for it: for an analysis that reads the values (opengeos/RustGIS#579).
#[cfg(not(target_arch = "wasm32"))]
pub fn info_blocking(url: &str) -> Result<Arc<Info>, String> {
    info_until(url, &std::sync::atomic::AtomicBool::new(false))
}

/// [`info_blocking`], giving up within 100 ms of `stop` being set (a download's Cancel), with
/// [`STOPPED`].
#[cfg(not(target_arch = "wasm32"))]
pub fn info_until(url: &str, stop: &std::sync::atomic::AtomicBool) -> Result<Arc<Info>, String> {
    let url = split(url).0;
    let (tx, rx) = std::sync::mpsc::channel();
    with_info(url, Box::new(move |r| tx.send(r).unwrap_or_default()));
    wait_until(&rx, Duration::from_secs(120), stop).map_err(|stopped| stopped_or(stopped, url, "no answer in two minutes"))?
}

/// What a read given up for its `stop` says.
#[cfg(not(target_arch = "wasm32"))]
pub const STOPPED: &str = "stopped before it was read";

/// How often a blocking read looks at its `stop` while it waits.
#[cfg(not(target_arch = "wasm32"))]
const STOP_STEP: Duration = Duration::from_millis(100);

/// What `rx` sends within `limit`, waited for in steps of [`STOP_STEP`]: `Err(true)` once `stop`
/// is set, `Err(false)` when nothing came in time (or the sender is gone).
#[cfg(not(target_arch = "wasm32"))]
fn wait_until<T>(rx: &std::sync::mpsc::Receiver<T>, limit: Duration, stop: &std::sync::atomic::AtomicBool) -> Result<T, bool> {
    let end = Instant::now() + limit;
    loop {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(true);
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(false);
        }
        match rx.recv_timeout(left.min(STOP_STEP)) {
            Ok(v) => return Ok(v),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Err(false),
        }
    }
}

/// [`STOPPED`] for a wait given up for its `stop`, else `late`; both name the address.
#[cfg(not(target_arch = "wasm32"))]
fn stopped_or(stopped: bool, url: &str, late: &str) -> String {
    message_with_address(url, if stopped { STOPPED } else { late })
}

/// Band `band`'s values (0-based) in a window of a level: columns `cols.0..cols.1` and rows
/// `rows.0..rows.1` of that level's grid, row by row, waiting for the tiles. No-data, and the
/// window's pixels outside the file, are NaN. The values are as stored (no scale or offset).
/// A read returns up to [`MAX_VALUES_READ`] values (256 MB): a caller reading on several threads
/// bounds its windows itself (a STAC trend's slices are a million pixels at most). It waits up to
/// two minutes for the tiles, as [`info_blocking`] does for the header.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_values_blocking(
    info: &Arc<Info>,
    url: &str,
    level: usize,
    band: usize,
    cols: (u32, u32),
    rows: (u32, u32),
) -> Result<Vec<f32>, String> {
    let never = std::sync::atomic::AtomicBool::new(false);
    read_values_counted_blocking(info, url, level, band, cols, rows, &never).map(|(values, _, _)| values)
}

/// [`read_values_blocking`], with how many of the window's values `f32` rounded (32- and 64-bit
/// whole numbers and 64-bit floats it doesn't hold exactly; no-data isn't counted), and how many
/// it can't hold at all (past its range: left out, NaN). It gives up within 100 ms of `stop`
/// being set (a download's Cancel), with [`STOPPED`]; the tiles already asked for are
/// still fetched, for the cache.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_values_counted_blocking(
    info: &Arc<Info>,
    url: &str,
    level: usize,
    band: usize,
    cols: (u32, u32),
    rows: (u32, u32),
    stop: &std::sync::atomic::AtomicBool,
) -> Result<(Vec<f32>, u64, u64), String> {
    let mut read = read_bands_counted_blocking(info, url, level, &[band], cols, rows, Duration::from_secs(120), stop)?;
    read.pop().ok_or_else(|| "no band was read".to_string())
}

/// [`read_values_counted_blocking`] for several bands of the window at once (in their order): their
/// tiles are asked for together, so the reads of a file whose bands are stored apart (a chunk a
/// band, as AlphaEarth's 64) overlap instead of following one another. It waits up to `wait` for
/// them all.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_bands_counted_blocking(
    info: &Arc<Info>,
    url: &str,
    level: usize,
    bands: &[usize],
    cols: (u32, u32),
    rows: (u32, u32),
    wait: Duration,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<Vec<(Vec<f32>, u64, u64)>, String> {
    let url = split(url).0.to_string();
    let l = info.layout.levels.get(level).ok_or("no such overview")?;
    if bands.iter().any(|b| *b >= info.layout.bands.max(1)) {
        return Err(format!("the file has {} band(s)", info.layout.bands));
    }
    // The window's size checked before its values are allocated: a caller's window, not the file.
    let (w, h) = (cols.1.saturating_sub(cols.0) as usize, rows.1.saturating_sub(rows.0) as usize);
    let n = (w as u64).saturating_mul(h as u64);
    if n > MAX_VALUES_READ {
        return Err(format!("a window of {w} × {h} values; one read takes {MAX_VALUES_READ} at most"));
    }
    // The part of the window inside the file.
    let (x0, x1) = (cols.0.min(l.width), cols.1.min(l.width));
    let (y0, y1) = (rows.0.min(l.height), rows.1.min(l.height));
    // Reserved first: a window the machine can't hold is an error, not a crash (#1291).
    let mut outs = bands
        .iter()
        .map(|_| rustgis_data::raster::filled(w, h, f32::NAN).map(|v| (v, 0u64, 0u64)).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, String>>()?;
    if x1 <= x0 || y1 <= y0 {
        return Ok(outs);
    }
    let (tw, th, across) = (l.tile_width.max(1), l.tile_height.max(1), l.across().max(1));
    // The tiles the window takes, counted before they are listed, in checked arithmetic: a
    // hostile header can't make a long list or a wrapped index.
    let (c0, c1, r0, r1) = (x0 / tw, (x1 - 1) / tw, y0 / th, (y1 - 1) / th);
    let count = (u64::from(c1 - c0) + 1) * (u64::from(r1 - r0) + 1);
    if count > MAX_VALUE_TILES as u64 {
        return Err(format!("{count} tiles of the file for one read; at most {MAX_VALUE_TILES}: take a coarser level or a smaller area"));
    }
    let tiles: Vec<u32> =
        (r0..=r1).flat_map(|ty| (c0..=c1).filter_map(move |tx| ty.checked_mul(across).and_then(|t| t.checked_add(tx)))).collect();
    if stop.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(message_with_address(&url, STOPPED));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    fetch_bands(info.clone(), url.clone(), level, tiles.clone(), bands.to_vec(), Box::new(move |r| tx.send(r).unwrap_or_default()));
    let late = if wait == Duration::from_secs(120) {
        "tiles not read in two minutes".to_string()
    } else {
        format!("tiles not read in {} s", wait.as_secs())
    };
    let picked = wait_until(&rx, wait, stop).map_err(|stopped| stopped_or(stopped, &url, &late))??;
    let nodata = info.layout.nodata.map(|v| v as f32);
    for (&t, p) in tiles.iter().zip(&picked) {
        let (tx0, ty0) = ((t % across).saturating_mul(tw), (t / across).saturating_mul(th));
        for ((chunk, b), (out, rounded, overflowed)) in p.iter().zip(outs.iter_mut()) {
            let Some(values) = chunk.get(*b) else { continue };
            for py in ty0.max(y0)..ty0.saturating_add(th).min(y1) {
                for px in tx0.max(x0)..tx0.saturating_add(tw).min(x1) {
                    let i = (py - ty0) as usize * tw as usize + (px - tx0) as usize;
                    let Some(&v) = values.get(i) else { continue };
                    // A marked value past `f32`'s range is infinite: left out as no value, and
                    // counted apart from those rounded. No-data isn't marked.
                    if chunk.rounded_at(*b, i) {
                        if v.is_infinite() { *overflowed += 1 } else { *rounded += 1 }
                    }
                    if v.is_finite()
                        && Some(v) != nodata
                        && let Some(slot) = out.get_mut((py - rows.0) as usize * w + (px - cols.0) as usize)
                    {
                        *slot = v;
                    }
                }
            }
        }
    }
    Ok(outs)
}

#[cfg(test)]
mod tests {
    /// Two callers share cached bytes, but a joining callback never borrows the loader's
    /// lifetime. Both metadata-to-overview and tile-to-next-tile run the real range reader;
    /// a retired loader also leaves no successful header or raw tile for later callers.
    #[test]
    fn permission_policy_shared_cog_waiters_keep_their_own_lifetime() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let _turn = crate::cache::turn();
        let _alarm = crate::cache::alarm_turn();
        let file = Arc::new(cog::example(512, 128));
        let held = Arc::new(Mutex::new(Vec::<(String, Done)>::new()));
        let hold = held.clone();
        crate::cache::set_fetcher(move |url, done| hold.lock().unwrap().push((url, done)));
        let drain = || {
            let mut sent = 0;
            loop {
                let pending = std::mem::take(&mut *held.lock().unwrap());
                if pending.is_empty() {
                    return sent;
                }
                for (url, done) in pending {
                    sent += 1;
                    assert!(sent < 100, "bounded overview fixture");
                    let (_, range) = crate::cache::split_range(&url);
                    let (a, b) = range.unwrap();
                    done(Ok(file[a as usize..((b + 1) as usize).min(file.len())].to_vec()));
                }
            }
        };
        let mut failures = Vec::new();
        for revoke in [false, true] {
            let url = format!("https://shared-cog.invalid/{revoke}.tif");
            let (loaded, info_result) = std::sync::mpsc::channel();
            with_info(&url, Box::new(move |r| loaded.send(r).unwrap()));
            assert_eq!(held.lock().unwrap().len(), 1, "first caller holds the header");
            let live = Arc::new(AtomicBool::new(true));
            let flag = live.clone();
            let still: crate::cache::Still = Arc::new(move || flag.load(SeqCst));
            let (done, result) = std::sync::mpsc::channel();
            crate::cache::with_still(still.clone(), || raster::overview(&url, Box::new(move |r| done.send(r).unwrap())));
            live.store(!revoke, SeqCst);
            let sent = drain();
            let info = info_result.recv().unwrap().unwrap();
            let r = result.recv().unwrap();
            if revoke {
                if r.is_ok() || sent != 1 {
                    failures.push(format!("revoked metadata waiter succeeded or fetched {sent} ranges"));
                }
            } else {
                assert!(r.is_ok() && sent > 1, "live overview needs real tile ranges: {sent}");
            }
            // A different tile starts under User; the bounded joiner's successful callback
            // would ask for a second tile. It must receive refusal before doing so.
            let (first, first_result) = std::sync::mpsc::channel();
            get_tile(info.clone(), url.clone(), 0, 0, Box::new(move |r| first.send(r).unwrap()));
            live.store(true, SeqCst);
            let (done, result) = std::sync::mpsc::channel();
            let (i, u) = (info.clone(), url.clone());
            crate::cache::with_still(still.clone(), || {
                get_tile(
                    i.clone(),
                    u.clone(),
                    0,
                    0,
                    Box::new(move |r| match r {
                        Ok(_) => get_tile(i, u, 0, 1, Box::new(move |r| done.send(r).unwrap())),
                        Err(e) => done.send(Err(e)).unwrap(),
                    }),
                );
            });
            live.store(!revoke, SeqCst);
            drain();
            assert!(first_result.recv().unwrap().is_ok());
            if result.recv().unwrap().is_ok() == revoke {
                failures.push(format!("tile joiner did not respect revoke={revoke}"));
            }
            // Ready hits are an entry point too, even without another network request.
            live.store(false, SeqCst);
            let (done, result) = std::sync::mpsc::channel();
            crate::cache::with_still(still.clone(), || get_tile(info.clone(), url.clone(), 0, 0, Box::new(move |r| done.send(r).unwrap())));
            if result.recv().unwrap().is_ok() {
                failures.push("revoked ready tile was delivered".into());
            }
            // A retired statistics read must not delay a later caller's stretch for a minute.
            forget_file(&url);
            let look = Look { aef: true, ..Default::default() };
            let (done, result) = std::sync::mpsc::channel();
            live.store(true, SeqCst);
            crate::cache::with_still(still, || {
                with_stretch(info.clone(), url.clone(), look.clone(), Box::new(move |r| done.send(r).unwrap()))
            });
            assert!(!held.lock().unwrap().is_empty(), "the stretch starts a real range read");
            live.store(false, SeqCst);
            drain();
            assert!(result.recv().unwrap().is_err());
            if !info.stretch_failed.lock().unwrap().is_empty() {
                failures.push("a revoked stretch left a shared retry delay".into());
            }
            let (done, result) = std::sync::mpsc::channel();
            let key = (info.colour(&look), look.aef);
            with_stretch(info.clone(), url, look, Box::new(move |r| done.send(r).unwrap()));
            drain();
            assert!(result.recv().unwrap().is_ok());
            // Valid raw samples may already be shared; a computed stretch is kept, whereas
            // a recent failure's placeholder would return without storing this look's key.
            if !info.stretch.lock().unwrap().contains_key(&key) {
                failures.push("a live stretch caller received the cancelled read's fallback".into());
            }
        }
        let url = "https://shared-cog.invalid/retired-loader.tif";
        let live = Arc::new(AtomicBool::new(true));
        let flag = live.clone();
        let still: crate::cache::Still = Arc::new(move || flag.load(SeqCst));
        let (done, result) = std::sync::mpsc::channel();
        crate::cache::with_still(still.clone(), || with_info(url, Box::new(move |r| done.send(r).unwrap())));
        live.store(false, SeqCst);
        drain();
        assert!(result.recv().unwrap().is_err());
        if infos().lock().unwrap().contains_key(url) {
            failures.push("a retired loader cached a successful header".into());
            forget_file(url);
        }
        let (done, result) = std::sync::mpsc::channel();
        with_info(url, Box::new(move |r| done.send(r).unwrap()));
        drain();
        let info = result.recv().unwrap().unwrap();
        live.store(true, SeqCst);
        let (done, result) = std::sync::mpsc::channel();
        crate::cache::with_still(still, || get_tile(info.clone(), url.into(), 0, 0, Box::new(move |r| done.send(r).unwrap())));
        live.store(false, SeqCst);
        drain();
        assert!(result.recv().unwrap().is_err());
        if tiles().lock().unwrap().map.contains_key(&(url.into(), 0, 0)) {
            failures.push("a retired loader cached successful raw samples".into());
            forget_file(url);
        }
        let (done, result) = std::sync::mpsc::channel();
        get_tile(info, url.into(), 0, 0, Box::new(move |r| done.send(r).unwrap()));
        assert!(drain() > 0, "the live caller reads fresh raw samples");
        assert!(result.recv().unwrap().is_ok());
        assert!(failures.is_empty(), "{failures:?}");
    }

    /// A palette image's own look colours its values by the table the header keeps apart from its
    /// colour hint (`Layout::palette`), with no stretch read or kept; a look of its bands doesn't.
    #[test]
    fn a_palette_images_own_look_has_its_table() {
        use rustgis_data::raster::cog::{Parse, Sparse, example, layout};
        use std::sync::{Arc, Mutex};
        let Ok(Parse::Done(mut layout)) = layout(&Sparse::default().with(0, example(64, 16))) else { panic!("not a COG") };
        layout.palette = Some(vec![[1, 2, 3, 255]; 256]);
        let transformer = layout.crs.transformer().unwrap();
        let info = Arc::new(super::Info {
            layout: *layout,
            header: Sparse::default(),
            reach: crate::address::Reach::Local,
            transformer,
            bounds: [0.0; 4],
            merc: [0.0; 4],
            min_zoom: 0,
            max_zoom: 0,
            stretch: Mutex::default(),
            stretching: Mutex::default(),
            stretch_failed: Mutex::default(),
            alpha: None,
            #[cfg(not(target_arch = "wasm32"))]
            revision: OnceLock::new(),
        });
        let own = super::Look::default();
        assert_eq!(info.palette(&own).map(<[[u8; 4]]>::len), Some(256));
        assert!(info.palette(&super::Look { bands: vec![0], ..super::Look::default() }).is_none());
        let (tx, rx) = std::sync::mpsc::channel();
        super::with_stretch(info.clone(), "test://palette.tif".into(), own.clone(), Box::new(move |r| tx.send(r).unwrap_or_default()));
        let stretch = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("answered at once").unwrap();
        assert_eq!(stretch, vec![(0.0, 255.0)]);
        assert!(info.stretch_for(&own).is_none(), "kept, which a look of its bands would take for its own");
    }

    /// A colour image's stretch leaves out the cells its alpha band hides (#1810): an RGBA
    /// Float32 COG whose see-through half holds −3.4e38, as an export from before #1759 wrote it,
    /// is stretched over its opaque half, black cells and all.
    #[test]
    fn a_stretch_leaves_out_what_the_alpha_band_hides() {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let _turn = crate::cache::turn();
        let _alarm = crate::cache::alarm_turn();
        let (w, h) = (512usize, 512usize);
        let n = w * h;
        // The top half opaque, 10 to 209 and black in its first column; the bottom see-through.
        let colour: Vec<f32> = (0..n)
            .map(|i| {
                if i >= n / 2 {
                    -3.4e38
                } else if i % w == 0 {
                    0.0
                } else {
                    (10 + i % 200) as f32
                }
            })
            .collect();
        let alpha: Vec<f32> = (0..n).map(|i| if i >= n / 2 { 0.0 } else { 1.0 }).collect();
        let gt = GeoTransform { origin_x: 500_000.0, origin_y: 4_800_000.0, pixel_width: 10.0, pixel_height: -10.0 };
        let utm = Crs::from_epsg(32611).unwrap();
        let bands = vec![colour.clone(), colour.clone(), colour, alpha];
        let r = Raster::new("rgba", w as u32, h as u32, bands, gt, utm, None, ColorHint::Rgb { alpha: true }, "Float32").unwrap();
        let file = Arc::new(rustgis_data::raster::cogwrite::write(&r, 0).unwrap());
        let held = Arc::new(Mutex::new(Vec::<(String, Done)>::new()));
        let hold = held.clone();
        crate::cache::set_fetcher(move |url, done| hold.lock().unwrap().push((url, done)));
        let drain = || {
            for _ in 0..100 {
                let pending = std::mem::take(&mut *held.lock().unwrap());
                if pending.is_empty() {
                    return;
                }
                for (url, done) in pending {
                    let (a, b) = crate::cache::split_range(&url).1.unwrap();
                    done(Ok(file[a as usize..((b + 1) as usize).min(file.len())].to_vec()));
                }
            }
            panic!("bounded reads");
        };
        let url = "https://see-through.invalid/rgba.tif".to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        with_info(&url, Box::new(move |r| tx.send(r).unwrap()));
        drain();
        let info = rx.recv().unwrap().unwrap();
        assert_eq!(info.alpha, Some(3));
        let (tx, rx) = std::sync::mpsc::channel();
        with_stretch(info, url.clone(), Look::default(), Box::new(move |r| tx.send(r).unwrap()));
        drain();
        let stretch = rx.recv().unwrap().unwrap();
        assert_eq!(stretch.len(), 3);
        assert!(stretch.iter().all(|(lo, hi)| (0.0..=209.0).contains(lo) && (0.0..=209.0).contains(hi) && lo < hi), "{stretch:?}");
        forget_file(&url);
    }

    /// A joint read whose tiles the fetch queue keeps dropping ends, saying so, rather than
    /// asking without end: within eight drops a tile (64 at least).
    #[test]
    fn a_read_the_queue_keeps_dropping_ends() {
        use rustgis_data::raster::cog::{Parse, Sparse, example, layout};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};
        let Ok(Parse::Done(layout)) = layout(&Sparse::default().with(0, example(64, 16))) else { panic!("not a COG") };
        let transformer = layout.crs.transformer().unwrap();
        let info = Arc::new(super::Info {
            layout: *layout,
            header: Sparse::default(),
            reach: crate::address::Reach::Local,
            transformer,
            bounds: [0.0; 4],
            merc: [0.0; 4],
            min_zoom: 0,
            max_zoom: 0,
            stretch: Mutex::default(),
            stretching: Mutex::default(),
            stretch_failed: Mutex::default(),
            alpha: None,
            #[cfg(not(target_arch = "wasm32"))]
            revision: OnceLock::new(),
        });
        let asked = Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        let get: super::GetTile = Arc::new(
            move |_: Arc<super::Info>, _: String, _: usize, _: u32, done: Box<dyn FnOnce(Result<super::Bands, String>) + Send>| {
                a.fetch_add(1, Ordering::Relaxed);
                done(Err(crate::cache::DROPPED.into()));
            },
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let wanted: Vec<u32> = (0..16).collect();
        super::fetch_all_with(get, info, "test://dropping.tif".into(), 0, wanted, Box::new(move |r| tx.send(r).unwrap_or_default()));
        let r = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("the read ended");
        let e = r.map(|_| ()).unwrap_err();
        // Eight drops a tile, then the read fails on the next; the rest of that batch isn't counted.
        assert!(e.contains("dropped its tiles 129 times"), "{e}");
        let asked = asked.load(Ordering::Relaxed);
        assert!((129..=129 + 15).contains(&asked), "{asked}");
        assert_eq!((super::drops_allowed(1), super::drops_allowed(16), super::drops_allowed(4096)), (64, 128, 32768));
    }

    #[test]
    fn announced_header_lengths_and_types() {
        use super::{HEAD, header_length, named_tiff, not_tiff_type};
        assert_eq!(header_length(Some(8192)), 8192);
        assert_eq!(header_length(None), HEAD);
        assert_eq!(header_length(Some(4)), HEAD, "too short for a TIFF header");
        assert_eq!(header_length(Some(u64::MAX)), crate::cache::MAX_BYTES as u64, "bounded as any read");
        for t in ["application/json", "application/geo+json; charset=utf-8", "text/html", "application/xml", "TEXT/PLAIN"] {
            assert!(not_tiff_type(t), "{t}");
        }
        for t in ["image/tiff", "image/tiff; application=geotiff", "application/octet-stream", ""] {
            assert!(!not_tiff_type(t), "{t}");
        }
        assert!(named_tiff("https://x.org/a/scene.TIF?sig=1") && !named_tiff("https://x.org/a/geotiff?f=tif"));
    }

    use super::*;

    #[test]
    fn tile_addresses() {
        assert_eq!(template("https://x.org/a.tif"), "cog:{z}/{x}/{y}@https://x.org/a.tif");
        assert_eq!(parse_url("cog:3/2/1@https://x.org/a.tif"), Some((3, 2, 1, "https://x.org/a.tif".into())));
        assert!(parse_url("cog:99/0/0@x").is_none() && parse_url("cog:1/0/0@").is_none() && !is_tile("pmtiles:1/0/0@x"));
    }

    #[test]
    fn stretch_read_from_an_overview_large_enough() {
        let file = rustgis_data::raster::cog::example(1024, 64);
        let rustgis_data::raster::cog::Parse::Done(l) = cog::layout(&Sparse::default().with(0, file)).unwrap() else { panic!() };
        // Overviews down to 64 pixels: the 256-pixel one is read.
        assert_eq!(l.levels.last().map(|l| l.width), Some(64));
        assert_eq!(l.levels[stats_level(&l.levels)].width, 256);
        // A long strip (a WorldView image 44 626 × 353 888 pixels, in tiles of 256): its overview
        // 349 pixels wide is 22 tiles, too many; the one 175 wide, 6 tiles, still a quarter of a
        // million pixels, is read.
        let level = |width: u32, height: u32| Level {
            ifd: 0,
            width,
            height,
            tile_width: 256,
            tile_height: 256,
            offsets: Vec::new(),
            counts: Vec::new(),
        };
        let strip: Vec<Level> = [(44_626, 353_888), (5579, 44_236), (698, 5530), (349, 2765), (175, 1383), (88, 692)]
            .into_iter()
            .map(|(w, h)| level(w, h))
            .collect();
        assert_eq!(strip[stats_level(&strip)].width, 175);
        // Only small ones: the finest of them; none: the coarsest.
        assert_eq!(stats_level(&[level(4096, 4096), level(200, 200), level(100, 100)]), 1);
        assert_eq!(stats_level(&[level(8192, 8192), level(4096, 4096)]), 1);
        assert_eq!(stats_level(&[]), 0);
    }

    /// Reads `file`'s layout as [`load`] and [`step`] do, a range at a time: the layout, how many
    /// reads past the first it took and the bytes held, or the reads made when it gave up.
    fn layout_by_ranges(file: &[u8]) -> Result<(Box<Layout>, usize, u64), usize> {
        layout_by_parts(&[(0, file)])
    }

    /// The same for a file that is only these parts, `(where it starts, its bytes)`: the rest is
    /// a hole no read comes back from (a server asked past the file's end), without writing it.
    /// Positions stay `u64`, as they are on wasm32.
    fn layout_by_parts(parts: &[(u64, &[u8])]) -> Result<(Box<Layout>, usize, u64), usize> {
        let part = |a: u64, len: u64| -> Vec<u8> {
            let Some((at, bytes)) = parts.iter().find(|(at, bytes)| *at <= a && a.saturating_sub(*at) < bytes.len() as u64) else {
                return Vec::new();
            };
            let from = usize::try_from(a.saturating_sub(*at)).unwrap_or(usize::MAX);
            let to = usize::try_from(a.saturating_sub(*at).saturating_add(len)).unwrap_or(usize::MAX).min(bytes.len());
            bytes.get(from..to).unwrap_or_default().to_vec()
        };
        let mut have = Sparse::default().with(0, part(0, HEAD));
        let mut rounds = 0;
        loop {
            match cog::layout(&have) {
                Ok(Parse::Done(layout)) => return Ok((layout, rounds, have.held())),
                Ok(Parse::Need(a, b)) => {
                    let len = header_read(&have, a, b, rounds).ok_or(rounds)?;
                    let more = part(a, len);
                    // An empty read past the end of a file whose length isn't known: read again
                    // knowing it, as `step` does (once).
                    have = if more.is_empty() && !have.len_known() { have.with_len(a) } else { have.with(a, more) };
                    rounds += 1;
                }
                other => panic!("{other:?}"),
            }
        }
    }

    /// GDAL's metadata tag pointing past the end of a file whose length isn't known: the empty
    /// read that comes back gives the length, as in [`step`], and the layout is read with the tag
    /// left out and said.
    #[test]
    fn a_tag_past_the_end_of_a_file_of_unknown_length_is_left_out() {
        use rustgis_data::raster::cog::{Example, example_with};
        let mut bad = example_with(&Example { size: 512, tile: 256, classes: true, ..Example::default() });
        let n = usize::from(u16::from_le_bytes([bad[8], bad[9]]));
        let len = u32::try_from(bad.len()).unwrap();
        let entry = (0..n).map(|k| 10 + k * 12).find(|&e| bad[e..e + 2] == [0x80, 0xA4]).unwrap();
        bad[entry + 8..entry + 12].copy_from_slice(&(len + 1000).to_le_bytes());
        let (l, ..) = layout_by_ranges(&bad).expect("read");
        let said: Vec<&String> = l.lost.iter().chain(l.described.iter().flat_map(|d| d.lost.iter())).collect();
        assert!(said.iter().any(|x| x.contains("band and class names and wavelengths are left out")), "{said:?}");
    }

    /// The header alone of a tiled GeoTIFF, `side` pixels a side in 256-pixel tiles, laid out as
    /// MapBiomas's: the tiles' byte counts and offsets right after the IFD, its georeferencing
    /// after them. The tiles themselves aren't there.
    fn big_index(side: u32) -> Vec<u8> {
        index_at(side, 8)
    }

    /// [`big_index`] with its IFD, and what follows it, meant to sit from byte `ifd_at` of the
    /// file: the vector is the 8-byte header, then those bytes, which a test places at `ifd_at`.
    fn index_at(side: u32, ifd_at: u32) -> Vec<u8> {
        let n = side.div_ceil(256).pow(2);
        let counts_at = ifd_at + 2 + 12 * 14 + 4;
        let offsets_at = counts_at + 4 * n;
        let scale_at = offsets_at + 4 * n;
        let (tie_at, keys_at) = (scale_at + 24, scale_at + 72);
        // (tag, type, count, value or where the values are); 3 short, 4 long, 12 double.
        let entries: [(u16, u16, u32, u32); 14] = [
            (256, 4, 1, side),
            (257, 4, 1, side),
            (258, 3, 1, 8),
            (259, 3, 1, 1),
            (262, 3, 1, 1),
            (277, 3, 1, 1),
            (284, 3, 1, 1),
            (322, 3, 1, 256),
            (323, 3, 1, 256),
            (324, 4, n, offsets_at),
            (325, 4, n, counts_at),
            (33550, 12, 3, scale_at),
            (33922, 12, 6, tie_at),
            (34735, 3, 12, keys_at),
        ];
        let mut out = b"II*\0".to_vec();
        out.extend(ifd_at.to_le_bytes());
        out.extend(14u16.to_le_bytes());
        for (tag, kind, count, value) in entries {
            out.extend(tag.to_le_bytes());
            out.extend(kind.to_le_bytes());
            out.extend(count.to_le_bytes());
            out.extend(value.to_le_bytes());
        }
        out.extend(0u32.to_le_bytes());
        let data_at = keys_at + 24;
        out.extend((0..n).flat_map(|_| 1u32.to_le_bytes()));
        out.extend((0..n).flat_map(|i| (data_at + i).to_le_bytes()));
        let px = 1.0 / 3600.0;
        out.extend([px, px, 0.0, 0.0, 0.0, 0.0, -74.0, 6.0, 0.0].iter().flat_map(|v: &f64| v.to_le_bytes()));
        out.extend([1u16, 1, 0, 2, 1024, 0, 1, 2, 2048, 0, 1, 4326].iter().flat_map(|v| v.to_le_bytes()));
        out
    }

    /// A tile index of 3.9 MB (490,000 tiles) is read within the rounds allowed: the decoder asks
    /// for 512 bytes at a time, and steps of a fixed 256 KB stopped at about 3 MB.
    #[test]
    fn a_large_tile_index_is_read_in_a_few_rounds() {
        let file = big_index(700 * 256);
        let (layout, rounds, held) = layout_by_ranges(&file).unwrap_or_else(|r| panic!("gave up after {r} reads"));
        assert_eq!(layout.levels.len(), 1);
        assert_eq!(layout.levels[0].offsets.len(), 490_000);
        assert_eq!(layout.levels[0].width, 179_200);
        assert_eq!((layout.transform.origin_x, layout.transform.origin_y), (-74., 6.));
        assert!(rounds <= 6, "{rounds} reads");
        // No byte is read twice: a read stops where a part already read starts.
        assert!(held <= file.len() as u64, "{held} bytes held of a {}-byte header", file.len());
        // A cloud-optimised file's header is in its first read, as before.
        assert_eq!(layout_by_ranges(&rustgis_data::raster::cog::example(1024, 64)).map(|(_, r, _)| r), Ok(0));
        // A read stops at the header's cap, however large the step has grown.
        let near = Sparse::default().with(0, vec![0; cog::MAX_HEADER as usize - 1000]);
        assert_eq!(header_read(&near, cog::MAX_HEADER, cog::MAX_HEADER + 512, 8), Some(1000));
        assert_eq!(header_read(&near, cog::MAX_HEADER, cog::MAX_HEADER + 1001, 8), None, "more than is left");
        assert_eq!(header_read(&near.with(cog::MAX_HEADER, vec![0; 1000]), cog::MAX_HEADER + 1000, cog::MAX_HEADER + 1512, 9), None);
    }

    /// A tiled GeoTIFF written as OpenGeoHub's GEDTM30 layers are: the IFD and its tile index after
    /// the image data, past 2 GiB. Only its header and that part exist here, the rest is a hole.
    #[test]
    fn a_header_past_2_gib_is_read_as_a_part_of_its_own() {
        let at = 3u32 << 30;
        let file = index_at(400 * 256, at);
        let (layout, rounds, held) =
            layout_by_parts(&[(0, &file[..8]), (u64::from(at), &file[8..])]).unwrap_or_else(|r| panic!("gave up after {r} reads"));
        assert_eq!(layout.levels.len(), 1);
        assert_eq!(layout.levels[0].offsets.len(), 160_000);
        assert_eq!(layout.levels[0].width, 102_400);
        // The IFD and its tile index, then nothing more: 4 reads after the first measured, well under the 12 allowed.
        assert!(rounds <= 6, "{rounds} reads");
        // Only what the header holds is read, not the 3 GiB before it.
        assert!(held <= file.len() as u64, "{held} bytes held of a {}-byte header", file.len());
        // The same layout where the IFD is at the start, for the same file.
        let near = layout_by_ranges(&big_index(400 * 256)).expect("read");
        assert_eq!(near.0.levels[0].offsets.len(), layout.levels[0].offsets.len());
    }

    /// Where a read starts doesn't limit it (a BigTIFF's offset, 340 GB), the bytes and the rounds
    /// do; a position no range request can name is refused, not overflowed.
    #[test]
    fn a_far_part_is_bounded_and_never_overflows() {
        let have = Sparse::default().with(0, vec![0; 64]);
        let far = 340_229_032_000;
        assert_eq!(header_read(&have, far, far + 512, 0), Some(STEP));
        assert_eq!(header_read(&have, MAX_AT, MAX_AT + 512, 0), Some(STEP));
        assert_eq!(header_read(&have, MAX_AT + 1, MAX_AT + 513, 0), None);
        assert_eq!(header_read(&have, u64::MAX - 8, u64::MAX, 0), None);
        assert_eq!(header_read(&have, far, far + 512, MAX_ROUNDS), None, "the rounds are still counted");
        let full = Sparse::default().with(0, vec![0; usize::try_from(cog::MAX_HEADER).unwrap_or_default()]);
        assert_eq!(header_read(&full, far, far + 512, 0), None, "so are the bytes held");
        // A read stops where a part already read starts.
        assert_eq!(header_read(&have.with(far + 1000, vec![0; 8]), far, far + 512, 0), Some(1000));
    }

    /// An IFD past the end of a file whose length is known is an error after one more read, not a
    /// loop or a panic.
    #[test]
    fn an_offset_past_the_end_is_an_error() {
        let file = index_at(256, 3 << 30);
        let have = Sparse::default().with_len(8).with(0, file[..8].to_vec());
        assert!(cog::layout(&have).is_err());
        // A file whose length isn't known: the first read past it finds the end, then the same.
        let have = Sparse::default().with(0, file[..8].to_vec());
        let Ok(Parse::Need(a, _)) = cog::layout(&have) else { panic!("asked for the IFD") };
        assert_eq!(a, 3 << 30);
        assert!(cog::layout(&have.with_len(a)).is_err());
    }

    #[test]
    fn looks_in_addresses() {
        let u = "https://x.org/a.tiff";
        let look = Look { bands: vec![1, 16, 9], range: Some((-0.3, 0.3)), aef: true, cube: false };
        let shown = with_look(u, &look);
        assert_eq!(shown, "https://x.org/a.tiff#bands=2,17,10&range=-0.3,0.3&dequantize=aef");
        assert_eq!(split(&shown), (u, look));
        // Another look replaces it; the file's own adds nothing.
        assert_eq!(with_look(&shown, &Look::default()), u);
        // A template keeps the look, so each look's map tiles are their own.
        let (_, _, _, archive) = parse_url(&template(&shown).replace("{z}/{x}/{y}", "1/0/0")).unwrap();
        assert_eq!(split(&archive).0, u);
        // Anything else after `#` is part of the address; bad looks are no looks.
        for bad in ["#page=2", "#bands=0,1,2", "#bands=1,2,3,4", "#range=1,1", "#range=x,1", "#bands=99999999999", "#bands="] {
            let a = format!("{u}{bad}");
            assert_eq!(split(&a), (a.as_str(), Look::default()), "{bad}");
        }
        assert!((shown_value(127.5, true) - 1.0).abs() < 1e-6 && (shown_value(-63.75, true) + 0.25).abs() < 1e-6);
        assert_eq!(shown_value(-5.0, false), -5.0);
    }

    #[test]
    fn paints_the_bands_shown() {
        let chunk = |v: f32| -> Bands { Arc::new(vec![vec![v; 4]].into()) };
        // One tile, its three bands from three chunks (stored apart); pixel 1 has no data.
        let mut red = vec![10.0; 4];
        red[1] = f32::NAN;
        let tile: Picked = vec![(Arc::new(vec![red].into()), 0), (chunk(20.0), 0), (chunk(30.0), 0)];
        let spots: Vec<Option<(u32, usize)>> = (0..(SIZE * SIZE) as usize).map(|k| if k < 2 { Some((7, k)) } else { None }).collect();
        let pen = Pen { rgb: true, alpha: None, alpha_max: 255.0, palette: None, aef: false, stretch: vec![(0.0, 40.0); 3] };
        let out = paint(&pen, &spots, &[7], &[tile]).unwrap();
        let px = &out[crate::cache::RAW.len() + 8..];
        assert_eq!(&px[..4], &[64, 128, 191, 255]);
        assert_eq!(&px[4..8], &[0, 0, 0, 0]);
        // Outside the file, beside a painted pixel: its colour, carried out (pixel 0's below);
        // beside only empty ones, nothing.
        let below = SIZE as usize * 4;
        assert_eq!(&px[below..below + 4], &[64, 128, 191, 255]);
        assert_eq!(&px[below + 4..below + 8], &[64, 128, 191, 255], "touching pixel 0 only at a corner");
        assert_eq!(&px[8..12], &[0, 0, 0, 0], "beside pixel 1, which has no data");
        assert_eq!(&px[below + 8..below + 12], &[0, 0, 0, 0]);
        // At the tile's own border: a file pixel in the top row's last column is carried left, down
        // and down-left, not on into the next row's first pixel.
        let s = SIZE as usize;
        let last: Vec<Option<(u32, usize)>> = (0..s * s).map(|k| (k == s - 1).then_some((7, 0))).collect();
        let tile: Picked = vec![(chunk(10.0), 0), (chunk(20.0), 0), (chunk(30.0), 0)];
        let out = paint(&pen, &last, &[7], &[tile]).unwrap();
        let px = &out[crate::cache::RAW.len() + 8..];
        let at = |k: usize| &px[k * 4..k * 4 + 4];
        for k in [s - 2, 2 * s - 2, 2 * s - 1] {
            assert_eq!(at(k), &[64, 128, 191, 255], "pixel {k}");
        }
        assert_eq!(at(s), &[0, 0, 0, 0], "the next row's first pixel");
        // Nothing there: no picture.
        assert!(paint(&pen, &vec![None; spots.len()], &[], &[]).unwrap().is_empty());
    }

    #[test]
    fn paints_grey_with_alpha_and_palettes() {
        let chunk = |v: Vec<f32>| -> Bands { Arc::new(vec![v].into()) };
        let spots: Vec<Option<(u32, usize)>> = (0..(SIZE * SIZE) as usize).map(|k| (k < 3).then_some((7, k))).collect();
        let px = |out: Vec<u8>| out[crate::cache::RAW.len() + 8..crate::cache::RAW.len() + 8 + 12].to_vec();
        // A 16-bit pan band and its 16-bit alpha band: opaque, half, hidden.
        let tile: Picked = vec![(chunk(vec![500.0, 1000.0, 1000.0, 0.0]), 0), (chunk(vec![65535.0, 32768.0, 0.0, 0.0]), 0)];
        let pen = Pen { rgb: false, alpha: Some(1), alpha_max: 65535.0, palette: None, aef: false, stretch: vec![(0.0, 1000.0)] };
        assert_eq!(px(paint(&pen, &spots, &[7], &[tile]).unwrap()), [128, 128, 128, 255, 255, 255, 255, 128, 0, 0, 0, 0]);
        // A palette image: each value its colour, a transparent one or one past the table nothing.
        let tile: Picked = vec![(chunk(vec![1.0, 2.0, 9.0, 0.0]), 0)];
        let table = vec![[0, 0, 0, 255], [200, 10, 20, 255], [0, 0, 0, 0]];
        let pen = Pen { rgb: false, alpha: None, alpha_max: 255.0, palette: Some(table), aef: false, stretch: vec![(0.0, 255.0)] };
        let out = px(paint(&pen, &spots, &[7], &[tile]).unwrap());
        assert_eq!(&out[..4], &[200, 10, 20, 255]);
        assert_eq!(&out[4..8], &[0, 0, 0, 0], "a transparent colour");
        assert_eq!(&out[8..12], &[0, 0, 0, 0], "a value past the table");
    }

    #[test]
    fn the_tiles_used_longest_ago_make_room() {
        let _turn = crate::cache::turn();
        let mut t = Tiles::default();
        let key = |i: u32| ("https://x.org/a.tif".to_string(), 0usize, i);
        let bands: Bands = Arc::new(cog::Tile::default());
        let half = BUDGET / 2;
        t.keep(key(1), bands.clone(), half, crate::address::Reach::Local);
        t.keep(key(2), bands.clone(), half, crate::address::Reach::Local);
        // Tile 1 is used again (the view still shows it): tile 2 makes room for tile 3.
        assert!(t.used(&key(1), crate::address::Reach::Local).is_some());
        t.keep(key(3), bands.clone(), half, crate::address::Reach::Local);
        assert!(t.used(&key(1), crate::address::Reach::Local).is_some() && t.used(&key(3), crate::address::Reach::Local).is_some());
        assert!(t.used(&key(2), crate::address::Reach::Local).is_none());
        assert_eq!(t.samples, 2 * half);
        // Kept again under its key: counted once, nothing else making room for it.
        t.keep(key(3), bands.clone(), half, crate::address::Reach::Local);
        assert!(t.used(&key(1), crate::address::Reach::Local).is_some() && t.used(&key(3), crate::address::Reach::Local).is_some());
        assert_eq!((t.samples, t.order.len()), (2 * half, 2));
        forget_file_in(&mut t, "https://x.org/a.tif");
        assert_eq!((t.samples, t.order.len(), t.map.len()), (0, 0, 0));
    }

    /// A file's header refused with a `Retry-After` (429 or 503) isn't asked for again before
    /// then (the failure stands, no read starts), and the map is asked to draw again by then, on
    /// each draw while it waits (as a tile's is), so a layer whose header failed doesn't stay
    /// failed until something else draws the map. (That the file is read again then: the
    /// engine's `a_header_refused_with_retry_after_is_read_again_when_it_says`.)
    #[test]
    fn a_header_refused_with_retry_after_has_the_map_drawn_again_then() {
        // Its turn at the cache (another test's clearing would forget the failure) and the alarms.
        let _turn = crate::cache::turn();
        let _alarm = crate::cache::alarm_turn();
        let file = "test://cog/busy.tif";
        let refused = || super::State::Failed("http status: 503 (retry after 2 s)".into(), Instant::now(), crate::cache::ceiling_for(file));
        super::infos().lock().unwrap().insert(file.to_string(), refused());
        crate::cache::clear_alarms();
        let within = || crate::cache::next_alarm().is_some_and(|d| d <= Duration::from_secs(2));
        let waiting = || matches!(super::infos().lock().unwrap().get(file), Some(super::State::Failed(..)));
        let Err(e) = super::info(file) else { panic!("read while waiting") };
        assert!(e.contains("retry after 2 s"), "{e}");
        assert!(waiting(), "asked for again before its wait was over");
        assert!(within(), "no alarm on a draw while waiting: {:?}", crate::cache::next_alarm());
        // Again on the next draw: the alarm fired (or was consumed) before the wait was over.
        crate::cache::clear_alarms();
        assert!(super::info(file).is_err() && waiting());
        assert!(within(), "not armed again: {:?}", crate::cache::next_alarm());
        super::infos().lock().unwrap().remove(file);
    }
    #[test]
    fn public_restriction_rejects_an_in_flight_local_header_and_allows_retry() {
        let _turn = crate::cache::turn();
        let file = Arc::new(cog::example(32, 16));
        let held = Arc::new(Mutex::new(Vec::<(String, Done)>::new()));
        let hold = held.clone();
        crate::cache::set_fetcher(move |url, done| hold.lock().unwrap().push((url, done)));
        let url = "https://cog-reach-change.example/header.tif";
        let (tx, rx) = std::sync::mpsc::channel();
        with_info(url, Box::new(move |r| tx.send(r).unwrap()));
        assert!(!held.lock().unwrap().is_empty());
        crate::cache::restrict_to_public(url);
        let drain = || {
            for _ in 0..100 {
                let pending = std::mem::take(&mut *held.lock().unwrap());
                if pending.is_empty() {
                    return;
                }
                for (url, done) in pending {
                    let (a, b) = crate::cache::split_range(&url).1.unwrap();
                    done(Ok(file[a as usize..((b + 1) as usize).min(file.len())].to_vec()));
                }
            }
            panic!("bounded COG fixture");
        };
        drain();
        assert!(rx.recv().unwrap().is_err());
        assert!(info_landed(url).is_none());
        let (tx, rx) = std::sync::mpsc::channel();
        with_info(url, Box::new(move |r| tx.send(r).unwrap()));
        drain();
        assert!(rx.recv().unwrap().is_ok());
        assert!(info_landed(url).is_some());
        let tile_url = "https://cog-reach-change.example/tile.tif";
        let (tx, rx) = std::sync::mpsc::channel();
        with_info(tile_url, Box::new(move |r| tx.send(r).unwrap()));
        drain();
        let info = rx.recv().unwrap().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        get_tile(info.clone(), tile_url.into(), 0, 0, Box::new(move |r| tx.send(r).unwrap()));
        assert!(!held.lock().unwrap().is_empty());
        crate::cache::restrict_to_public(tile_url);
        drain();
        assert!(rx.recv().unwrap().is_err());
        let (tx, rx) = std::sync::mpsc::channel();
        get_tile(info, tile_url.into(), 0, 0, Box::new(move |r| tx.send(r).unwrap()));
        drain();
        assert!(rx.recv().unwrap().is_ok());
        // A ready anonymous header has no outer tile-cache slot to invalidate by alias.
        let plain = "https://cog-reach-change.example/alias.tif";
        let anonymous = crate::s3::anonymous(plain);
        let (tx, rx) = std::sync::mpsc::channel();
        with_info(&anonymous, Box::new(move |r| tx.send(r).unwrap()));
        drain();
        assert!(rx.recv().unwrap().is_ok());
        assert!(info_landed(&anonymous).is_some());
        crate::cache::restrict_to_public(plain);
        assert!(info_landed(&anonymous).is_none());
        forget_file(url);
        forget_file(tile_url);
        forget_file(&anonymous);
    }
    #[test]
    fn every_header_range_retains_its_starting_public_ceiling() {
        use crate::address::{Origin, Reach};
        use crate::cache::{Fetched, FetchedDone};
        let _turn = crate::cache::turn();
        let file = cog::example(32, 16);
        let held = Arc::new(Mutex::new(Vec::<(String, Reach, FetchedDone)>::new()));
        let hold = held.clone();
        crate::cache::set_fetcher_with_origin(move |url, ceiling, done, _| hold.lock().unwrap().push((url, ceiling, done)));
        let url = "https://cog-header-ceiling.example/header.tif";
        crate::cache::restrict_to_public(url);
        let (tx, rx) = std::sync::mpsc::channel();
        with_info(url, Box::new(move |r| tx.send(r).unwrap()));
        let (first, reach, done) = held.lock().unwrap().pop().unwrap();
        assert_eq!(reach, Reach::Public);
        crate::cache::allow_named(url);
        // A short first answer forces a second range while the global setting allows Local.
        done(Ok(Fetched { bytes: file[..8].to_vec(), origin: Origin::named(&first) }));
        assert!(!held.lock().unwrap().is_empty());
        crate::cache::restrict_to_public(url);
        for _ in 0..100 {
            let pending = std::mem::take(&mut *held.lock().unwrap());
            if pending.is_empty() {
                break;
            }
            for (address, reach, done) in pending {
                assert_eq!(reach, Reach::Public, "{address}");
                let (a, b) = crate::cache::split_range(&address).1.unwrap();
                done(Ok(Fetched { bytes: file[a as usize..((b + 1) as usize).min(file.len())].to_vec(), origin: Origin::named(&address) }));
            }
        }
        let info = rx.recv().unwrap().unwrap();
        assert_eq!(info.reach, Reach::Public);
        assert!(info_landed(url).is_some());
        forget_file(url);
    }

    #[test]
    fn ready_tiles_keep_the_loading_ceiling_when_a_template_changes() {
        let _turn = crate::cache::turn();
        let url = "https://cog-ready-ceiling.example/a.tif";
        crate::cache::restrict_to_public("https://cog-ready-ceiling.example/{s}.tif");
        let mut t = Tiles::default();
        let key = (url.to_string(), 0, 0);
        t.keep(key.clone(), Arc::new(cog::Tile::default()), 0, crate::address::Reach::Local);
        assert!(t.used(&key, crate::address::Reach::Local).is_none(), "a late Local completion cannot be relabelled Public");
        crate::cache::allow_named(url);
        assert!(t.used(&key, crate::address::Reach::Public).is_none(), "a Public reader never reuses a Local tile");
        assert!(t.used(&key, crate::address::Reach::Local).is_some(), "a Local reader can still reuse it");
    }
    #[test]
    fn sparse_tiles_preserve_pending_callbacks_and_retry_under_their_own_reach() {
        use crate::address::Reach;
        let _turn = crate::cache::turn();
        for reader in [Reach::Local, Reach::Public] {
            let cog::Parse::Done(mut layout) = cog::layout(&Sparse::default().with(0, cog::example(32, 16))).unwrap() else {
                panic!("fixture")
            };
            layout.levels[0].counts[0] = 0;
            let transformer = layout.crs.transformer().unwrap();
            let info = Arc::new(Info {
                layout: *layout,
                header: Sparse::default(),
                reach: reader,
                transformer,
                bounds: [0.0; 4],
                merc: [0.0; 4],
                min_zoom: 0,
                max_zoom: 0,
                stretch: Mutex::default(),
                stretching: Mutex::default(),
                stretch_failed: Mutex::default(),
                alpha: None,
                #[cfg(not(target_arch = "wasm32"))]
                revision: OnceLock::new(),
            });
            let url = format!("https://sparse-pending.example/{reader:?}.tif");
            let key = (url.clone(), 0, 0);
            let (old_tx, old_rx) = std::sync::mpsc::channel();
            tiles()
                .lock()
                .unwrap()
                .map
                .insert(key.clone(), TileState::Loading(vec![Box::new(move |r| old_tx.send(r).unwrap())], Reach::Local, 0));
            let (new_tx, new_rx) = std::sync::mpsc::channel();
            get_tile(info, url.clone(), 0, 0, Box::new(move |r| new_tx.send(r).unwrap()));
            let Some(TileState::Loading(waiting, ..)) = tiles().lock().unwrap().map.remove(&key) else {
                panic!("the old loader must remain")
            };
            assert_eq!(waiting.len(), 2);
            for then in waiting {
                then(Err("old read stopped".into()));
            }
            assert!(old_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap().is_err());
            let new = new_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            if reader == Reach::Public {
                assert!(new.unwrap().iter().flatten().all(|v| v.is_nan()));
            } else {
                assert!(new.is_err());
            }
            forget_file(&url);
        }
    }

    #[test]
    fn readers_with_different_ceilings_retry_headers_and_tiles_under_their_own_scope() {
        use crate::address::{Origin, Reach};
        use crate::cache::{Fetched, FetchedDone};
        use std::sync::atomic::{AtomicBool, Ordering};
        let _turn = crate::cache::turn();
        let file = cog::example(32, 16);
        for (case, fail_first, retire_first) in [(0, false, false), (1, true, false), (2, false, true)] {
            let held = Arc::new(Mutex::new(Vec::<(String, Reach, FetchedDone)>::new()));
            let seen = Arc::new(Mutex::new(Vec::new()));
            let (hold, record) = (held.clone(), seen.clone());
            crate::cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                record.lock().unwrap().push(ceiling);
                hold.lock().unwrap().push((url, ceiling, done));
            });
            let url = format!("https://cog-pending-ceilings.example/{case}.tif");
            let live = Arc::new(AtomicBool::new(true));
            let first_live = live.clone();
            let (local_tx, local_rx) = std::sync::mpsc::channel();
            crate::cache::with_still(Arc::new(move || first_live.load(Ordering::Relaxed)), || {
                with_info(&url, Box::new(move |r| local_tx.send(r).unwrap()));
            });
            crate::cache::restrict_to_public(&url);
            let (public_tx, public_rx) = std::sync::mpsc::channel();
            with_info(&url, Box::new(move |r| public_tx.send(r).unwrap()));
            crate::cache::allow_named(&url);
            live.store(!retire_first, Ordering::Relaxed);
            let (first, reach, done) = held.lock().unwrap().pop().unwrap();
            assert_eq!(reach, Reach::Local);
            done(if fail_first { Err("HTTP 503".into()) } else { Ok(Fetched { bytes: file.clone(), origin: Origin::named(&first) }) });
            let drain = || {
                for _ in 0..100 {
                    let pending = std::mem::take(&mut *held.lock().unwrap());
                    if pending.is_empty() {
                        return;
                    }
                    for (address, _, done) in pending {
                        let (a, b) = crate::cache::split_range(&address).1.unwrap();
                        done(Ok(Fetched {
                            bytes: file[a as usize..((b + 1) as usize).min(file.len())].to_vec(),
                            origin: Origin::named(&address),
                        }));
                    }
                }
                panic!("bounded COG fixture");
            };
            drain();
            let local = local_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            assert_eq!(local.is_err(), fail_first || retire_first);
            let public = public_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap().unwrap();
            assert_eq!(public.reach, Reach::Public);
            assert_eq!(*seen.lock().unwrap(), vec![Reach::Local, Reach::Public]);
            if let Ok(local) = local {
                for narrowed_local_header in [false, true] {
                    forget_file(&url);
                    let mark = seen.lock().unwrap().len();
                    let (local_tx, local_rx) = std::sync::mpsc::channel();
                    get_tile(local.clone(), url.clone(), 0, 0, Box::new(move |r| local_tx.send(r).unwrap()));
                    if narrowed_local_header {
                        crate::cache::restrict_to_public(&url);
                    }
                    let (public_tx, public_rx) = std::sync::mpsc::channel();
                    get_tile(
                        if narrowed_local_header { local.clone() } else { public.clone() },
                        url.clone(),
                        0,
                        0,
                        Box::new(move |r| public_tx.send(r).unwrap()),
                    );
                    crate::cache::allow_named(&url);
                    assert_eq!(held.lock().unwrap().len(), 1, "the incompatible tile waiter waits for its own retry");
                    drain();
                    assert!(local_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap().is_ok());
                    assert!(public_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap().is_ok());
                    assert_eq!(seen.lock().unwrap().get(mark..).unwrap(), &[Reach::Local, Reach::Public]);
                }
            }
            forget_file(&url);
        }
    }

    /// A GeoTIFF on this computer is read in place from its file, by its full path (#2761): its
    /// header and overview, as a web address's are by range. Refused for a document that may reach
    /// only public addresses; a path that isn't a full one to a `.tif` or `.tiff` isn't taken for
    /// one, nor is a web address.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_local_geotiff_is_read_in_place_from_its_file() {
        let _turn = crate::cache::turn();
        let dir = std::env::temp_dir().join(format!("rustgis-cog-local-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("local.tif");
        std::fs::write(&path, cog::example(512, 128)).unwrap();
        let url = path.display().to_string();
        assert!(is_local_file(&url));
        let info = info_blocking(&url).unwrap();
        let sizes: Vec<u32> = info.layout.levels.iter().map(|l| l.width).collect();
        assert_eq!(sizes, vec![512, 256, 128]);
        let overview = raster::overview_blocking(&url).unwrap();
        assert_eq!((overview.width, overview.overview), (512, None), "small enough to hold its full image as the overview");
        assert!(overview.rounded.is_empty(), "bytes round nothing");
        // Int32 values past 16,777,216: the odd ones `f32` rounds are counted, as the whole-file
        // reader counts them.
        let wide = dir.join("wide.tif");
        // 200 pixels a side in tiles of 128: the edge tiles' padding isn't counted.
        std::fs::write(&wide, cog::example_with(&cog::Example { size: 200, tile: 128, wide: true, ..cog::Example::default() })).unwrap();
        let read = raster::overview_blocking(&wide.display().to_string()).unwrap();
        assert_eq!(read.rounded, vec![200 * 200 / 2]);
        let (tx, rx) = std::sync::mpsc::channel();
        load(url.clone(), crate::address::Reach::Public, Box::new(move |r| tx.send(r.err()).unwrap()));
        let refused = rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap_or_default();
        assert!(refused.contains("a file on this computer can't be read from here"), "{refused}");
        for not in ["local.tif", "./local.tif", "//server/share/local.tif", "https://example.com/local.tif", "file:///tmp/local.tif"] {
            assert!(!is_local_file(not), "{not}");
        }
        assert!(!is_local_file(&dir.join("local.png").display().to_string()));
        // Written again while open (another image, another file): its header is read again, not
        // the one kept.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, cog::example(256, 128)).unwrap();
        let info = info_blocking(&url).unwrap();
        let sizes: Vec<u32> = info.layout.levels.iter().map(|l| l.width).collect();
        assert_eq!(sizes, vec![256, 128]);
        // A read of a tile still in flight from before the rewrite (another header) isn't joined:
        // given the old read's stale tile, the new reader reads the tile again from the file.
        let key = (url.clone(), 0usize, 0u32);
        let (old_tx, old_rx) = std::sync::mpsc::channel();
        tiles()
            .lock()
            .unwrap()
            .map
            .insert(key.clone(), TileState::Loading(vec![Box::new(move |r| old_tx.send(r).unwrap())], crate::address::Reach::Local, 0));
        let (new_tx, new_rx) = std::sync::mpsc::channel();
        get_tile(info.clone(), url.clone(), 0, 0, Box::new(move |r| new_tx.send(r).unwrap()));
        let Some(TileState::Loading(waiting, ..)) = tiles().lock().unwrap().map.remove(&key) else { panic!("the old read must remain") };
        let stale: Bands = Arc::new(vec![vec![999.0; 128 * 128]].into());
        for then in waiting {
            then(Ok(stale.clone()));
        }
        assert_eq!(old_rx.recv().unwrap().unwrap().bands[0][0], 999.0);
        let fresh = new_rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        assert_eq!(fresh.bands[0][1], 1.0, "pixel (1, 0) of the file, not the stale tile");
        forget_local_file(&url);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
