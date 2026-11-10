//! PMTiles (v3): a whole tile set in one file, read a tile at a time with byte-range requests
//! (or from disk), so a vector or raster tile set needs no tile server.
//!
//! A tile's address is `pmtiles:{z}/{x}/{y}@<archive>`, the archive being an `http(s)` address
//! or, on the desktop, a file path. Fetching it reads the archive's header and root directory
//! (the first 16 KB, once), the leaf directory holding the tile if there is one (cached), then
//! the tile's bytes. Everything goes through the cache's fetcher, so it works in the browser
//! too, where the server must allow range requests (and CORS).
//!
//! Archives are hostile input: offsets, lengths and directory sizes are checked against the
//! file and bounded, and nesting is limited. Metadata must be readable, valid JSON objects.
//! Shared reads retain their initiating request lifetime; retired contexts cannot publish
//! archive information or directories, and reload epochs reject pre-reload completions.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;
use web_time::{Duration, Instant};

use crate::cache::{Done, MAX_BYTES, caller_callback};

/// The scheme of a tile address read from a PMTiles archive.
pub const SCHEME: &str = "pmtiles:";

/// The header and root directory are within the first 16 KB of an archive.
const HEAD: u64 = 16_384;
/// The most entries a directory may hold.
const MAX_ENTRIES: usize = 1 << 20;
/// Leaf directories go no deeper than this.
const MAX_DEPTH: usize = 4;
/// Leaf directories kept.
const MAX_LEAVES: usize = 256;
/// Fresh validation bounds the entire reachable index, including the root.
#[cfg(not(target_arch = "wasm32"))]
const INDEX_BUDGET: IndexBudget =
    IndexBudget { stored: (MAX_BYTES * 4) as u64, decoded: (MAX_BYTES * 4) as u64, entries: MAX_ENTRIES, directories: MAX_LEAVES * 16 };
/// A failed archive is tried again after this long.
const RETRY: Duration = Duration::from_secs(60);

/// What the tiles are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileType {
    Unknown,
    Mvt,
    Png,
    Jpeg,
    Webp,
    Avif,
}

impl TileType {
    pub fn is_vector(self) -> bool {
        self == TileType::Mvt
    }
}

/// An archive's header and metadata.
#[derive(Clone, Debug)]
pub struct Info {
    pub tile_type: TileType,
    pub min_zoom: u8,
    pub max_zoom: u8,
    /// Longitude/latitude bounds: west, south, east, north.
    pub bounds: [f64; 4],
    /// The JSON metadata (vector layers, attribution…), `Null` when there is none.
    pub metadata: Value,
    /// Its vector layers' names, as [`source_layers::of_metadata`](rustgis_data::source_layers::of_metadata) keeps them.
    vector_layers: Vec<String>,
    root: Arc<Vec<Entry>>,
    leaf_offset: u64,
    leaf_length: u64,
    data_offset: u64,
    data_length: u64,
    internal: Compression,
    tile: Compression,
    epoch: u64,
}

impl Info {
    /// The names of the metadata's vector layers, as
    /// [`source_layers::of_metadata`](rustgis_data::source_layers::of_metadata) keeps them.
    pub fn vector_layers(&self) -> &[String] {
        &self.vector_layers
    }

    /// Takes the archive's metadata, and the vector layers' names from it.
    fn set_metadata(&mut self, metadata: Value) {
        self.vector_layers = rustgis_data::source_layers::of_metadata(&metadata);
        self.metadata = metadata;
    }

    /// The metadata's attribution, if any.
    pub fn attribution(&self) -> String {
        self.metadata.get("attribution").and_then(Value::as_str).unwrap_or_default().to_string()
    }
}

/// How directories, metadata or tiles are compressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Compression {
    None,
    Gzip,
    Brotli,
    Zstd,
}

impl Compression {
    /// Decompresses bytes (bounded, as gzip is).
    fn decode(self, b: Vec<u8>) -> Result<Vec<u8>, String> {
        use std::io::Read;
        let limit = MAX_BYTES as u64 * 4;
        let bounded = |r: &mut dyn Read, what: &str| {
            let mut out = Vec::new();
            r.take(limit + 1).read_to_end(&mut out).map_err(|e| format!("{what}: {e}"))?;
            if out.len() as u64 > limit {
                return Err(format!("{what}: too large once decompressed"));
            }
            Ok(out)
        };
        match self {
            Compression::None => Ok(b),
            Compression::Gzip => {
                if !b.starts_with(&[0x1f, 0x8b]) {
                    return Err("gzip: invalid header".into());
                }
                gunzip(&b)
            }
            Compression::Brotli => bounded(&mut brotli_decompressor::Decompressor::new(b.as_slice(), 4096), "Brotli"),
            Compression::Zstd => unzstd(&b, limit),
        }
    }
}

/// Decompresses Zstandard bytes: every frame of them, in turn (skippable frames skipped), at
/// most `limit` bytes in all.
fn unzstd(b: &[u8], limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut src = b;
    let mut out = Vec::new();
    while !src.is_empty() {
        // A skippable frame: its magic (0x184D2A50 to 0x184D2A5F), its length, its bytes.
        if let Some(magic) = src.get(..4).and_then(|m| <[u8; 4]>::try_from(m).ok()).map(u32::from_le_bytes)
            && magic & 0xFFFF_FFF0 == 0x184D_2A50
        {
            let len = src.get(4..8).and_then(|l| <[u8; 4]>::try_from(l).ok()).map(u32::from_le_bytes).ok_or("Zstandard: a cut frame")?;
            let next = usize::try_from(len).ok().and_then(|l| l.checked_add(8)).ok_or("Zstandard: a cut frame")?;
            src = src.get(next..).ok_or("Zstandard: a cut frame")?;
            continue;
        }
        let room = (limit + 1).saturating_sub(out.len() as u64);
        // A frame declaring a window larger than an archive section needs is refused before its
        // buffer is allocated.
        let window = limit.min(32 << 20);
        let d = ruzstd::decoding::StreamingDecoder::new_with_max_window_size(&mut src, window).map_err(|e| format!("Zstandard: {e}"))?;
        d.take(room).read_to_end(&mut out).map_err(|e| format!("Zstandard: {e}"))?;
        if out.len() as u64 > limit {
            return Err("Zstandard: too large once decompressed".into());
        }
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Entry {
    tile_id: u64,
    offset: u64,
    length: u64,
    run_length: u64,
}

/// The template for an archive's tiles.
pub fn template(archive: &str) -> String {
    format!("{SCHEME}{{z}}/{{x}}/{{y}}@{archive}")
}

/// Whether an address is a tile read from a PMTiles archive.
pub fn is_tile(url: &str) -> bool {
    url.starts_with(SCHEME)
}

/// Whether an archive address is a file on this computer rather than a web address.
/// (As `rustgis_map::tiles::is_local_archive` decides it, which a layer's address went through.)
/// An address read through a saved S3 connection is never a file, well formed or not.
pub fn is_local(archive: &str) -> bool {
    if rustgis_data::login_address::is_wrapper(archive) {
        return false;
    }
    if rustgis_data::s3_address::is_wrapper(archive) {
        return false;
    }
    let lower = archive.get(..8).unwrap_or(archive).to_ascii_lowercase();
    !(lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("s3://"))
}

/// Whether a local archive may be read: an absolute path to a `.pmtiles` file (checked again
/// here, whatever route the address came by).
pub fn check_local(archive: &str) -> Result<(), String> {
    let p = archive.strip_prefix("file://").unwrap_or(archive);
    let drive = p.as_bytes().get(1) == Some(&b':') && p.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    // A path on this computer: not a network share (\\server\share, or file://server/…), which a
    // project could use to make Windows connect to another machine.
    let share = p.starts_with("\\\\") || p.starts_with("//") || (archive.starts_with("file://") && !p.starts_with('/'));
    if share || !(p.starts_with('/') || drive) || p.chars().any(char::is_control) || p.len() > 4096 {
        return Err(format!("'{p}': a PMTiles file is given by its full path on this computer"));
    }
    if !p.to_ascii_lowercase().ends_with(".pmtiles") {
        return Err(format!("'{p}' isn't a .pmtiles file"));
    }
    Ok(())
}

/// The tile id of z/x/y: tiles of lower zooms first, then along a Hilbert curve.
pub fn tile_id(z: u8, x: u32, y: u32) -> u64 {
    let z = u32::from(z.min(31));
    let base = ((1u64 << (2 * z)) - 1) / 3;
    let n = 1u64 << z;
    let (mut x, mut y) = (u64::from(x), u64::from(y));
    let mut d = 0u64;
    let mut s = n / 2;
    while s > 0 {
        let rx = u64::from(x & s > 0);
        let ry = u64::from(y & s > 0);
        d += s * s * ((3 * rx) ^ ry);
        // Only the bits below `s` matter from here; rotate the quadrant they're in.
        x &= s - 1;
        y &= s - 1;
        if ry == 0 {
            if rx == 1 {
                x = s - 1 - x;
                y = s - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    base + d
}

/// What a new archive says of itself (see [`write`]).
#[derive(Clone, Debug)]
pub struct NewArchive {
    pub tile_type: TileType,
    /// The tiles given are gzipped (as vector tiles usually are).
    pub gzipped_tiles: bool,
    /// Longitude/latitude bounds: west, south, east, north.
    pub bounds: [f64; 4],
    /// The JSON metadata object (`vector_layers`, `attribution`, `name`…); use `{}` when absent.
    /// Its serialized JSON must not exceed 16 MiB.
    pub metadata: Value,
}

/// Writes a PMTiles (v3) archive of `tiles` (zoom, x, y, bytes): clustered, identical tiles
/// stored once, its directories uncompressed, in leaf directories when the root would pass the
/// first 16 KB. Tiles given twice keep the last.
pub fn write(tiles: Vec<(u8, u32, u32, Vec<u8>)>, a: &NewArchive) -> Result<Vec<u8>, String> {
    if !a.metadata.is_object() {
        return Err("PMTiles metadata must be a JSON object".into());
    }
    let meta = serde_json::to_vec(&a.metadata).map_err(|e| e.to_string())?;
    if meta.len() > MAX_BYTES {
        return Err("PMTiles metadata exceeds the 16 MiB limit".into());
    }
    // A tile outside its zoom would take the id of another zoom's; readers stop at zoom 30.
    if let Some((z, x, y, _)) = tiles.iter().find(|(z, x, y, _)| *z > 30 || u64::from(*x) >= 1u64 << z || u64::from(*y) >= 1u64 << z) {
        return Err(format!("{z}/{x}/{y} isn't a tile (zoom 0–30, x and y below 2^zoom)"));
    }
    let mut tiles: Vec<(u64, u8, Vec<u8>)> = tiles.into_iter().map(|(z, x, y, b)| (tile_id(z, x, y), z, b)).collect();
    tiles.sort_by_key(|t| t.0);
    // The last of each id (a stable sort keeps them in the order given).
    let mut unique: Vec<(u64, u8, Vec<u8>)> = Vec::with_capacity(tiles.len());
    for t in tiles {
        match unique.last_mut() {
            Some(last) if last.0 == t.0 => *last = t,
            _ => unique.push(t),
        }
    }
    let (Some(first), Some(last)) = (unique.first(), unique.last()) else { return Err("an archive needs at least one tile".into()) };
    let (min_zoom, max_zoom) = (first.1, last.1);
    let mut data: Vec<u8> = Vec::new();
    let mut stored: HashMap<&[u8], (u64, u64)> = HashMap::new();
    let mut entries: Vec<Entry> = Vec::new();
    for (id, _, b) in &unique {
        let (offset, length) = *stored.entry(b.as_slice()).or_insert_with(|| {
            let at = data.len() as u64;
            data.extend_from_slice(b);
            (at, b.len() as u64)
        });
        // The same tile right after the previous one: a longer run.
        if let Some(e) = entries.last_mut()
            && e.offset == offset
            && e.length == length
            && e.tile_id + e.run_length == *id
        {
            e.run_length += 1;
            continue;
        }
        entries.push(Entry { tile_id: *id, offset, length, run_length: 1 });
    }
    let contents = stored.len() as u64;
    let addressed: u64 = entries.iter().map(|e| e.run_length).sum();
    // The root directory, or leaves of more entries each until the root fits.
    let mut root = write_dir(&entries);
    let mut leaves = Vec::new();
    let mut per_leaf = 4096usize;
    while 127 + root.len() > HEAD as usize {
        leaves.clear();
        let mut index = Vec::new();
        for chunk in entries.chunks(per_leaf) {
            let Some(first) = chunk.first() else { continue };
            let leaf = write_dir(chunk);
            index.push(Entry { tile_id: first.tile_id, offset: leaves.len() as u64, length: leaf.len() as u64, run_length: 0 });
            leaves.extend(leaf);
        }
        root = write_dir(&index);
        per_leaf = per_leaf.checked_mul(2).ok_or("too many tiles")?;
    }
    let root_at = 127u64;
    let meta_at = root_at + root.len() as u64;
    let leaf_at = meta_at + meta.len() as u64;
    let data_at = leaf_at + leaves.len() as u64;
    let mut h = b"PMTiles".to_vec();
    h.push(3);
    for v in [
        root_at,
        root.len() as u64,
        meta_at,
        meta.len() as u64,
        leaf_at,
        leaves.len() as u64,
        data_at,
        data.len() as u64,
        addressed,
        entries.len() as u64,
        contents,
    ] {
        h.extend_from_slice(&v.to_le_bytes());
    }
    let tile_type = match a.tile_type {
        TileType::Unknown => 0,
        TileType::Mvt => 1,
        TileType::Png => 2,
        TileType::Jpeg => 3,
        TileType::Webp => 4,
        TileType::Avif => 5,
    };
    // Clustered; directories uncompressed; tiles as given.
    h.extend_from_slice(&[1, 1, if a.gzipped_tiles { 2 } else { 1 }, tile_type, min_zoom, max_zoom]);
    let e7 = |v: f64, max: f64| ((v.clamp(-max, max) * 1e7).round() as i32).to_le_bytes();
    let [w, so, e, n] = a.bounds;
    for (v, max) in [(w, 180.0), (so, 90.0), (e, 180.0), (n, 90.0)] {
        h.extend_from_slice(&e7(v, max));
    }
    h.push(min_zoom);
    h.extend_from_slice(&e7((w + e) / 2.0, 180.0));
    h.extend_from_slice(&e7((so + n) / 2.0, 90.0));
    let mut out = h;
    out.reserve_exact(root.len() + meta.len() + leaves.len() + data.len());
    for part in [root, meta, leaves] {
        out.extend_from_slice(&part);
    }
    out.extend_from_slice(&data);
    Ok(out)
}

/// A directory: delta-coded tile ids, then run lengths, lengths and offsets (0 for "right after
/// the previous entry"), all varints.
fn write_dir(entries: &[Entry]) -> Vec<u8> {
    use crate::mvt::put_varint as varint;
    let mut b = Vec::new();
    varint(&mut b, entries.len() as u64);
    let mut last = 0;
    for e in entries {
        varint(&mut b, e.tile_id - last);
        last = e.tile_id;
    }
    for e in entries {
        varint(&mut b, e.run_length);
    }
    for e in entries {
        varint(&mut b, e.length);
    }
    let mut prev: Option<&Entry> = None;
    for e in entries {
        let next = prev.is_some_and(|p| p.offset + p.length == e.offset);
        varint(&mut b, if next { 0 } else { e.offset + 1 });
        prev = Some(e);
    }
    b
}

enum State {
    Loading(Vec<Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>>),
    Ready(Arc<Info>),
    Failed(String, Instant),
}

fn archives() -> &'static Mutex<HashMap<String, State>> {
    static A: OnceLock<Mutex<HashMap<String, State>>> = OnceLock::new();
    A.get_or_init(Mutex::default)
}

/// A leaf directory: read, or being read with tiles waiting for it.
enum Leaf {
    Ready(Arc<Vec<Entry>>),
    Loading(Vec<Box<dyn FnOnce(Result<Arc<Vec<Entry>>, String>) + Send>>),
}

fn leaves() -> &'static Mutex<HashMap<(String, u64), Leaf>> {
    static L: OnceLock<Mutex<HashMap<(String, u64), Leaf>>> = OnceLock::new();
    L.get_or_init(Mutex::default)
}

/// Forgets the archives read (with the tile cache).
pub(crate) fn forget() {
    archives().lock().unwrap_or_else(|e| e.into_inner()).retain(|_, s| matches!(s, State::Loading(_)));
    // Reads under way are kept: their waiting tiles are answered when they land.
    leaves().lock().unwrap_or_else(|e| e.into_inner()).retain(|_, l| matches!(l, Leaf::Loading(_)));
}

/// Forgets what was read of one archive (its header, and its directories), but not a read under
/// way: a failed read is made again with whatever login now signs the archive's address.
pub(crate) fn forget_archive(archive: &str) {
    let mut all = archives().lock().unwrap_or_else(|e| e.into_inner());
    if all.get(archive).is_some_and(|s| !matches!(s, State::Loading(_))) {
        all.remove(archive);
    }
    drop(all);
    leaves().lock().unwrap_or_else(|e| e.into_inner()).retain(|(a, _), l| a != archive || matches!(l, Leaf::Loading(_)));
}

/// Pending callbacks are returned for disposal outside the revision lock: dropping a tile's
/// answer may immediately launch another read.
pub(crate) fn reload_archive(archive: &str) -> impl Sized {
    let state = archives().lock().unwrap_or_else(|e| e.into_inner()).remove(archive);
    let mut all = leaves().lock().unwrap_or_else(|e| e.into_inner());
    let keys: Vec<_> = all.keys().filter(|(a, _)| a == archive).cloned().collect();
    let removed: Vec<_> = keys.into_iter().filter_map(|key| all.remove(&key)).collect();
    (state, removed)
}

/// An archive's header and metadata: `Ok(None)` while they are read (the map draws again once
/// they arrive).
pub fn info(archive: &str) -> Result<Option<Arc<Info>>, String> {
    crate::cache::check_still(&crate::cache::current_still())?;
    crate::logins::check(archive).map_err(str::to_string)?;
    let epoch = crate::cache::archive_epoch(archive);
    let result = crate::cache::if_archive_current(archive, epoch, || {
        let mut all = archives().lock().unwrap_or_else(|e| e.into_inner());
        match all.get(archive) {
            Some(State::Ready(i)) => return (Ok(Some(i.clone())), false, None),
            Some(State::Loading(_)) => return (Ok(None), false, None),
            Some(State::Failed(e, at)) => {
                let wait = crate::cache::retry_wait(e, RETRY);
                if at.elapsed() < wait {
                    return (Err(e.clone()), false, Some(*at + wait));
                }
            }
            _ => {}
        }
        all.insert(archive.to_string(), State::Loading(Vec::new()));
        (Ok(None), true, None)
    });
    match result {
        Some((result, start, retry_at)) => {
            if let Some(at) = retry_at {
                crate::cache::notify_at(at);
            }
            if start {
                load(archive.to_string(), epoch);
            }
            result
        }
        None => Ok(None),
    }
}

/// The archive's info, read now if it hasn't been, waiting up to `timeout` (natively: the
/// browser can't wait).
#[cfg(not(target_arch = "wasm32"))]
pub fn info_blocking(archive: &str, timeout: std::time::Duration) -> Result<Arc<Info>, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    with_info(archive, Box::new(move |r| tx.send(r).unwrap_or_default()));
    rx.recv_timeout(timeout).map_err(|_| format!("{archive}: no answer in {} s", timeout.as_secs()))?
}

/// Calls `f` with the archive's info, once it is read.
fn with_info(archive: &str, f: Box<dyn FnOnce(Result<Arc<Info>, String>) + Send>) {
    let f = caller_callback(f);
    if let Err(e) = crate::cache::check_still(&crate::cache::current_still()) {
        f(Err(e));
        return;
    }
    if let Err(e) = crate::logins::check(archive) {
        f(Err(e.to_string()));
        return;
    }
    let epoch = crate::cache::archive_epoch(archive);
    let result = crate::cache::if_archive_current(archive, epoch, || {
        let mut all = archives().lock().unwrap_or_else(|e| e.into_inner());
        match all.get_mut(archive) {
            Some(State::Ready(i)) => (Some((f, Ok(i.clone()))), false, None),
            Some(State::Loading(waiting)) => {
                waiting.push(f);
                (None, false, None)
            }
            Some(State::Failed(e, at)) if at.elapsed() < crate::cache::retry_wait(e, RETRY) => {
                (Some((f, Err(e.clone()))), false, Some(*at + crate::cache::retry_wait(e, RETRY)))
            }
            _ => {
                all.insert(archive.to_string(), State::Loading(vec![f]));
                (None, true, None)
            }
        }
    });
    if let Some((answer, start, retry_at)) = result {
        if let Some(at) = retry_at {
            crate::cache::notify_at(at);
        }
        if let Some((f, result)) = answer {
            f(result);
        }
        if start {
            load(archive.to_string(), epoch);
        }
    }
}

/// Reads an archive's header, root directory and metadata, then answers whoever waits.
fn load(archive: String, epoch: u64) {
    let a = archive.clone();
    read(
        &archive,
        0,
        HEAD,
        Box::new(move |head| {
            let parsed = head.and_then(|b| parse_head(&b));
            match parsed {
                Err(e) => finish(&a, Err(e), epoch),
                Ok((mut info, meta_at, meta_len)) => {
                    info.epoch = epoch;
                    if meta_len == 0 {
                        finish(&a, Ok(info), epoch);
                        return;
                    }
                    let a2 = a.clone();
                    read(
                        &a,
                        meta_at,
                        meta_len,
                        Box::new(move |m| {
                            let result = m.and_then(|m| metadata(&mut info, m, meta_len)).map(|()| info);
                            finish(&a2, result, epoch);
                        }),
                    );
                }
            }
        }),
    );
}

/// Settles archive waiters with live results, forgetting dropped or retired reads.
fn finish(archive: &str, r: Result<Info, String>, epoch: u64) {
    let still = crate::cache::current_still();
    let r = crate::cache::check_still(&still).and(r);
    let r = crate::logins::check(archive)
        .map_err(str::to_string)
        .and(r)
        .map(Arc::new)
        .map_err(|e| if rustgis_data::login_address::is_wrapper(archive) { crate::logins::scrub_error(&e) } else { e });
    let transient = r.as_ref().err().is_some_and(|e| !crate::cache::keep_failure(e, &still));
    let (waiting, retry_at) = crate::cache::if_archive_current(archive, epoch, || {
        let mut all = archives().lock().unwrap_or_else(|e| e.into_inner());
        let mut retry_at = None;
        let old = match &r {
            Ok(i) => all.insert(archive.to_string(), State::Ready(i.clone())),
            // A dropped or retired owner's read isn't the archive's fault: the next caller
            // starts its own read instead of inheriting a shared retry delay.
            Err(_) if transient => all.remove(archive),
            Err(e) => {
                // Read again once its wait is over: the map draws again then (a 429 or 503 says
                // when).
                let at = Instant::now();
                retry_at = Some(at + crate::cache::retry_wait(e, RETRY));
                all.insert(archive.to_string(), State::Failed(e.clone(), at))
            }
        };
        let waiting = match old {
            Some(State::Loading(w)) => w,
            _ => Vec::new(),
        };
        (waiting, retry_at)
    })
    .unwrap_or_default();
    if let Some(at) = retry_at {
        crate::cache::notify_at(at);
    }
    for w in waiting {
        w(r.clone());
    }
    crate::cache::notify();
}

/// Fetches a tile address (`pmtiles:z/x/y@archive`): its bytes, or none for a tile the archive
/// doesn't have.
pub(crate) fn fetch(url: &str, done: Done) {
    if let Err(e) = crate::cache::check_still(&crate::cache::current_still()) {
        done(Err(e));
        return;
    }
    let Some((z, x, y, archive)) = parse_url(url) else {
        done(Err(rustgis_data::diagnostics::message_with_address(url, "not a PMTiles tile address")));
        return;
    };
    let a = archive.clone();
    let epoch = crate::cache::archive_epoch(&archive);
    with_info(
        &archive,
        Box::new(move |info| match info {
            Err(e) => done(Err(e)),
            Ok(info) => {
                if crate::cache::archive_epoch(&a) != epoch {
                    done(Err(crate::cache::DROPPED.into()));
                    return;
                }
                // Outside the archive's zooms, or not a tile of its zoom: none.
                let n = 1u64 << z;
                if z < info.min_zoom || z > info.max_zoom || u64::from(x) >= n || u64::from(y) >= n {
                    done(Ok(Vec::new()));
                    return;
                }
                let id = tile_id(z, x, y);
                let root = info.root.clone();
                find(a, info, id, root, 0, done);
            }
        }),
    );
}

/// Looks `id` up in a directory, reading leaf directories as needed, then reads the tile.
fn find(archive: String, info: Arc<Info>, id: u64, dir: Arc<Vec<Entry>>, depth: usize, done: Done) {
    if let Err(e) = crate::cache::check_still(&crate::cache::current_still()) {
        done(Err(e));
        return;
    }
    let epoch = info.epoch;
    if crate::cache::archive_epoch(&archive) != epoch {
        done(Err(crate::cache::DROPPED.into()));
        return;
    }
    let Some(e) = lookup(&dir, id) else {
        done(Ok(Vec::new()));
        return;
    };
    if e.run_length > 0 {
        let tile = info.tile;
        // Within the tile data section (when the header gives its length).
        let inside = info.data_length == 0 || e.offset.saturating_add(e.length) <= info.data_length;
        let Some(at) = info.data_offset.checked_add(e.offset).filter(|_| inside) else {
            done(Err("a tile outside the archive".into()));
            return;
        };
        read(&archive, at, e.length, Box::new(move |b| done(b.and_then(|b| tile.decode(b)))));
        return;
    }
    // A leaf directory.
    if depth >= MAX_DEPTH {
        done(Err("PMTiles directories nested too deep".into()));
        return;
    }
    if e.offset.saturating_add(e.length) > info.leaf_length {
        done(Err("a leaf directory outside the archive".into()));
        return;
    }
    let at = info.leaf_offset.saturating_add(e.offset);
    let key = (archive.clone(), at);
    // Continues the lookup once the leaf is there.
    let next: Box<dyn FnOnce(Result<Arc<Vec<Entry>>, String>) + Send> = {
        let (archive, info) = (archive.clone(), info.clone());
        Box::new(move |leaf| match leaf {
            Ok(leaf) => find(archive, info, id, leaf, depth + 1, done),
            Err(e) => done(Err(e)),
        })
    };
    let next = caller_callback(next);
    // One read per leaf: tiles needing a leaf already on its way wait for it.
    let admitted = crate::cache::if_archive_current(&archive, epoch, || {
        let mut l = leaves().lock().unwrap_or_else(|e| e.into_inner());
        match l.get_mut(&key) {
            Some(Leaf::Ready(leaf)) => {
                let leaf = leaf.clone();
                drop(l);
                return (Some((next, leaf)), false);
            }
            Some(Leaf::Loading(waiting)) => {
                waiting.push(next);
                return (None, false);
            }
            None => {
                // Full: one ready leaf makes room (the cache is a working set, not a history).
                if l.len() >= MAX_LEAVES
                    && let Some(old) = l.iter().find(|(_, v)| matches!(v, Leaf::Ready(_))).map(|(k, _)| k.clone())
                {
                    l.remove(&old);
                }
                l.insert(key.clone(), Leaf::Loading(vec![next]));
            }
        }
        (None, true)
    });
    match admitted {
        None => return,
        Some((Some((next, leaf)), _)) => {
            next(Ok(leaf));
            return;
        }
        Some((None, false)) => return,
        Some((None, true)) => {}
    }
    read(
        &archive,
        at,
        e.length,
        Box::new(move |b| {
            let leaf = b.and_then(|b| info.internal.decode(b)).and_then(|b| parse_dir(&b)).map(Arc::new);
            finish_leaf(key, leaf, epoch);
        }),
    );
}

fn finish_leaf(key: (String, u64), leaf: Result<Arc<Vec<Entry>>, String>, epoch: u64) {
    let leaf = crate::cache::check_still(&crate::cache::current_still()).and(leaf);
    let waiting = crate::cache::if_archive_current(&key.0, epoch, || {
        let mut all = leaves().lock().unwrap_or_else(|e| e.into_inner());
        let old = match &leaf {
            Ok(leaf) => all.insert(key.clone(), Leaf::Ready(leaf.clone())),
            Err(_) => all.remove(&key),
        };
        match old {
            Some(Leaf::Loading(waiting)) => waiting,
            _ => Vec::new(),
        }
    })
    .unwrap_or_default();
    for callback in waiting {
        callback(leaf.clone());
    }
}

/// The entry holding `id`: the last at or before it, when it is a leaf directory or its run
/// covers `id`.
fn lookup(dir: &[Entry], id: u64) -> Option<Entry> {
    let i = dir.partition_point(|e| e.tile_id <= id);
    let e = *dir.get(i.checked_sub(1)?)?;
    if e.run_length == 0 || id < e.tile_id.saturating_add(e.run_length) { Some(e) } else { None }
}

/// `pmtiles:z/x/y@archive` → its parts.
fn parse_url(url: &str) -> Option<(u8, u32, u32, String)> {
    let rest = url.strip_prefix(SCHEME)?;
    let (zxy, archive) = rest.split_once('@')?;
    let mut p = zxy.split('/');
    let z: u8 = p.next()?.parse().ok()?;
    let x: u32 = p.next()?.parse().ok()?;
    let y: u32 = p.next()?.parse().ok()?;
    (z <= 30 && !archive.is_empty()).then(|| (z, x, y, archive.to_string()))
}

/// Reads `len` bytes at `at` of an archive.
fn read(archive: &str, at: u64, len: u64, done: Done) {
    if let Err(e) = crate::cache::check_still(&crate::cache::current_still()) {
        done(Err(e));
        return;
    }
    if len == 0 {
        done(Ok(Vec::new()));
        return;
    }
    if len > MAX_BYTES as u64 {
        done(Err(format!("{len} bytes is too large to read from an archive")));
        return;
    }
    let end = at.saturating_add(len) - 1;
    let url = format!("{archive}{}{at}-{end}", crate::cache::RANGE);
    if is_local(archive) {
        if let Err(e) = check_local(archive) {
            done(Err(e));
            return;
        }
        // Read on the fetch workers, as web archives are, never on the frame.
        crate::cache::fetch_local(url, done);
        return;
    }
    crate::cache::fetch_counted(url, done);
}

/// Reads a range of a local archive (on a fetch worker).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_file(archive: &str, at: u64, len: u64) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let path = archive.strip_prefix("file://").unwrap_or(archive);
    let mut f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    f.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
    let mut buf = Vec::with_capacity(usize::try_from(len).unwrap_or(0).min(MAX_BYTES));
    f.take(len).read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

fn metadata(info: &mut Info, bytes: Vec<u8>, expected: u64) -> Result<(), String> {
    if bytes.len() as u64 != expected {
        return Err("truncated PMTiles metadata".into());
    }
    let bytes = info.internal.decode(bytes)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|e| format!("PMTiles metadata: {e}"))?;
    if !value.is_object() {
        return Err("PMTiles metadata must be a JSON object".into());
    }
    info.set_metadata(value);
    Ok(())
}

/// Reads the current local file without consulting or changing retained caches. All reachable
/// directories and nonoverlapping tile runs are validated within fixed aggregate budgets;
/// cancellation is checked between entries, while tile payloads remain lazy.
/// Ranges are bounded and freshly opened; failure leaves the displayed archive intact.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_info_local(archive: &str) -> Result<Arc<Info>, String> {
    read_info_local_bounded(archive, INDEX_BUDGET)
}

#[cfg(not(target_arch = "wasm32"))]
fn read_info_local_bounded(archive: &str, mut budget: IndexBudget) -> Result<Arc<Info>, String> {
    check_local(archive)?;
    let head = read_file(archive, 0, HEAD)?;
    let path = archive.strip_prefix("file://").unwrap_or(archive);
    let file_len = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?.len();
    if head.len() < 127 {
        return Err("a truncated PMTiles header".into());
    }
    let (mut info, at, len, root_decoded) = parse_head_detail(&head)?;
    for (offset, length) in [(8, 16), (24, 32), (40, 48), (56, 64)] {
        let start = u64_at(&head, offset).ok_or("a truncated PMTiles header")?;
        let size = u64_at(&head, length).ok_or("a truncated PMTiles header")?;
        if start.checked_add(size).is_none_or(|end| end > file_len) {
            return Err("a PMTiles section outside the local file".into());
        }
    }
    if len != 0 {
        metadata(&mut info, read_file(archive, at, len)?, len)?;
    }
    budget.take(u64_at(&head, 16).ok_or("a truncated PMTiles header")?, root_decoded, info.root.len())?;
    validate_directories(archive, &info, &info.root, 0, &mut budget, &mut HashMap::new())?;
    Ok(Arc::new(info))
}

#[cfg(target_arch = "wasm32")]
pub fn read_info_local(archive: &str) -> Result<Arc<Info>, String> {
    Err(format!("{archive}: the web app cannot reload local PMTiles paths"))
}

/// Remaining aggregate work. The decoder also independently caps any one directory at 64 MiB;
/// an over-budget decode is discarded before parsing or following its references.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy)]
struct IndexBudget {
    stored: u64,
    decoded: u64,
    entries: usize,
    directories: usize,
}

#[cfg(not(target_arch = "wasm32"))]
impl IndexBudget {
    fn take(&mut self, stored: u64, decoded: u64, entries: usize) -> Result<(), String> {
        let error = "PMTiles index exceeds validation budget";
        self.stored = self.stored.checked_sub(stored).ok_or(error)?;
        self.decoded = self.decoded.checked_sub(decoded).ok_or(error)?;
        self.entries = self.entries.checked_sub(entries).ok_or(error)?;
        self.directories = self.directories.checked_sub(1).ok_or(error)?;
        Ok(())
    }
}

/// A completed directory's height and descendant tile bounds; empty subtrees have no minimum.
/// Recheck these on every shared reference: its parent may impose a tighter depth or tile boundary.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug, Default)]
struct DirectorySummary {
    height: usize,
    tile_start: Option<u64>,
    tile_end: u64,
}

/// An unfinished range encountered again is a cycle; completed ranges cost no further I/O or budget.
#[cfg(not(target_arch = "wasm32"))]
fn validate_directories(
    archive: &str,
    info: &Info,
    dir: &[Entry],
    depth: usize,
    budget: &mut IndexBudget,
    seen: &mut HashMap<(u64, u64), Option<DirectorySummary>>,
) -> Result<DirectorySummary, String> {
    let mut summary = DirectorySummary::default();
    for (i, e) in dir.iter().enumerate() {
        rustgis_data::progress::check().map_err(|e| e.to_string())?;
        let end = e.offset.checked_add(e.length).ok_or("a PMTiles entry range overflows")?;
        if e.run_length > 0 {
            let tile_end = e.tile_id.checked_add(e.run_length).ok_or("tile ids overflow")?;
            if dir.get(i + 1).is_some_and(|next| tile_end > next.tile_id) {
                return Err("overlapping PMTiles tile runs".into());
            }
            if end > info.data_length || info.data_offset.checked_add(end).is_none() {
                return Err("a tile outside the archive".into());
            }
            if e.length > MAX_BYTES as u64 {
                return Err(format!("{} bytes is too large to read from an archive", e.length));
            }
            summary.tile_start = Some(summary.tile_start.map_or(e.tile_id, |start| start.min(e.tile_id)));
            summary.tile_end = summary.tile_end.max(tile_end);
            continue;
        }
        if depth >= MAX_DEPTH {
            return Err("PMTiles directories nested too deep".into());
        }
        if end > info.leaf_length {
            return Err("a leaf directory outside the archive".into());
        }
        let at = info.leaf_offset.checked_add(e.offset).ok_or("a leaf directory range overflows")?;
        info.leaf_offset.checked_add(end).ok_or("a leaf directory range overflows")?;
        if e.length > MAX_BYTES as u64 {
            return Err(format!("{} bytes is too large to read from an archive", e.length));
        }
        let key = (at, e.length);
        let child_summary = if let Some(done) = seen.get(&key) {
            done.ok_or("cyclic PMTiles directories")?
        } else {
            // Refuse oversized ranges and too many directories before issuing any I/O.
            if budget.directories == 0 || e.length > budget.stored {
                return Err("PMTiles index exceeds validation budget".into());
            }
            seen.insert(key, None);
            let bytes = read_file(archive, at, e.length)?;
            if bytes.len() as u64 != e.length {
                return Err("a truncated PMTiles leaf directory".into());
            }
            let bytes = info.internal.decode(bytes)?;
            if bytes.len() as u64 > budget.decoded {
                return Err("PMTiles index exceeds validation budget".into());
            }
            let child = parse_dir_bounded(&bytes, budget.entries)?;
            budget.take(e.length, bytes.len() as u64, child.len())?;
            drop(bytes);
            let child_summary = validate_directories(archive, info, &child, depth + 1, budget, seen)?;
            seen.insert(key, Some(child_summary));
            child_summary
        };
        if child_summary.tile_start.is_some_and(|start| start < e.tile_id) {
            return Err("a PMTiles leaf directory contains tiles below its pointer".into());
        }
        if dir.get(i + 1).is_some_and(|next| child_summary.tile_end > next.tile_id) {
            return Err("overlapping PMTiles tile runs".into());
        }
        let subtree = child_summary.height + 1;
        if depth + subtree > MAX_DEPTH {
            return Err("PMTiles directories nested too deep".into());
        }
        summary.height = summary.height.max(subtree);
        if let Some(start) = child_summary.tile_start {
            summary.tile_start = Some(summary.tile_start.map_or(start, |current| current.min(start)));
        }
        summary.tile_end = summary.tile_end.max(child_summary.tile_end);
    }
    Ok(summary)
}

fn u64_at(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

fn i32_at(b: &[u8], i: usize) -> Option<f64> {
    Some(f64::from(i32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?)) / 1e7)
}

/// The header and root directory, and where the metadata is.
fn parse_head(b: &[u8]) -> Result<(Info, u64, u64), String> {
    parse_head_detail(b).map(|(info, at, len, _)| (info, at, len))
}

fn parse_head_detail(b: &[u8]) -> Result<(Info, u64, u64, u64), String> {
    if b.get(..7) != Some(b"PMTiles".as_slice()) {
        return Err("not a PMTiles archive".into());
    }
    if b.get(7) != Some(&3) {
        return Err(format!("PMTiles version {} isn't read (only 3)", b.get(7).copied().unwrap_or(0)));
    }
    let g = |i| u64_at(b, i).ok_or("a truncated PMTiles header");
    let (root_at, root_len, meta_at, meta_len) = (g(8)?, g(16)?, g(24)?, g(32)?);
    let (leaf_offset, leaf_length, data_offset, data_length) = (g(40)?, g(48)?, g(56)?, g(64)?);
    let byte = |i: usize| b.get(i).copied().ok_or("a truncated PMTiles header");
    let compression = |c: u8, what: &str| match c {
        0 | 1 => Ok(Compression::None),
        2 => Ok(Compression::Gzip),
        3 => Ok(Compression::Brotli),
        4 => Ok(Compression::Zstd),
        c => Err(format!("unknown {what} compression {c}")),
    };
    let internal = compression(byte(97)?, "directories")?;
    let tile = compression(byte(98)?, "tiles")?;
    let tile_type = match byte(99)? {
        1 => TileType::Mvt,
        2 => TileType::Png,
        3 => TileType::Jpeg,
        4 => TileType::Webp,
        5 => TileType::Avif,
        _ => TileType::Unknown,
    };
    let (min_zoom, max_zoom) = (byte(100)?.min(30), byte(101)?.min(30));
    let c = |i| i32_at(b, i).ok_or("a truncated PMTiles header");
    let bounds = [c(102)?, c(106)?, c(110)?, c(114)?];
    // The root directory is within the first 16 KB.
    let root_end = root_at.checked_add(root_len).filter(|e| *e <= b.len() as u64).ok_or("the root directory isn't where it should be")?;
    let raw = b
        .get(usize::try_from(root_at).map_err(|_| "bad offset")?..usize::try_from(root_end).map_err(|_| "bad offset")?)
        .ok_or("bad offset")?;
    let raw = internal.decode(raw.to_vec())?;
    let root = parse_dir(&raw)?;
    let info = Info {
        tile_type,
        min_zoom: min_zoom.min(max_zoom),
        max_zoom,
        bounds,
        metadata: Value::Null,
        vector_layers: Vec::new(),
        root: Arc::new(root),
        leaf_offset,
        leaf_length,
        data_offset,
        data_length,
        internal,
        tile,
        epoch: 0,
    };
    if meta_len > MAX_BYTES as u64 || meta_at.checked_add(meta_len).is_none() {
        return Err("PMTiles metadata is too large or has an invalid range".into());
    }
    Ok((info, meta_at, meta_len, raw.len() as u64))
}

/// A directory: delta-coded tile ids, then run lengths, lengths and offsets, all varints.
fn parse_dir(b: &[u8]) -> Result<Vec<Entry>, String> {
    parse_dir_bounded(b, MAX_ENTRIES)
}

fn parse_dir_bounded(b: &[u8], entry_budget: usize) -> Result<Vec<Entry>, String> {
    let mut r = Varints { b, at: 0 };
    let n = usize::try_from(r.next()?).map_err(|_| "a directory too large")?;
    // Each entry takes at least four bytes.
    if n > MAX_ENTRIES || n.saturating_mul(4) > b.len() {
        return Err("a directory with more entries than it can hold".into());
    }
    if n > entry_budget {
        return Err("PMTiles index exceeds validation budget".into());
    }
    let mut entries = vec![Entry { tile_id: 0, offset: 0, length: 0, run_length: 0 }; n];
    let mut last = 0u64;
    for e in entries.iter_mut() {
        last = last.checked_add(r.next()?).ok_or("tile ids overflow")?;
        e.tile_id = last;
    }
    for e in entries.iter_mut() {
        e.run_length = r.next()?;
    }
    for e in entries.iter_mut() {
        e.length = r.next()?;
    }
    for i in 0..n {
        let v = r.next()?;
        let prev = if i > 0 { entries.get(i - 1).copied() } else { None };
        let e = entries.get_mut(i).ok_or("a short directory")?;
        e.offset = match (v, prev) {
            // Right after the previous entry.
            (0, Some(p)) => p.offset.checked_add(p.length).ok_or("a directory offset overflows")?,
            (v, _) => v.saturating_sub(1),
        };
    }
    Ok(entries)
}

struct Varints<'a> {
    b: &'a [u8],
    at: usize,
}

impl Varints<'_> {
    fn next(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.b.get(self.at).ok_or("a truncated directory")?;
            self.at += 1;
            if shift == 63 && byte > 1 {
                return Err("a varint too long".into());
            }
            v |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("a varint too long".into())
    }
}

/// Inflates gzip (or zlib) bytes, bounded.
fn gunzip(b: &[u8]) -> Result<Vec<u8>, String> {
    crate::mvt::inflate(b)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn retired_context_cannot_start_or_join_archive_reads() {
        let _turn = crate::cache::turn();
        let url = "https://retired-pmtiles-initiation.invalid/archive.pmtiles";
        let bytes = archive(&[(0, 0, 0, b"tile".to_vec())], 1, false, "{}");
        let (ready, _, _) = parse_head(&bytes).unwrap();
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sent = asked.clone();
        crate::cache::set_fetcher(move |_, done| {
            sent.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            done(Err("unexpected range".into()));
        });
        for state in [None, Some(State::Loading(Vec::new())), Some(State::Ready(Arc::new(ready)))] {
            if let Some(state) = state {
                archives().lock().unwrap().insert(url.into(), state);
            }
            crate::cache::with_still(Arc::new(|| false), || {
                assert!(info(url).is_err());
                let (tx, rx) = std::sync::mpsc::channel();
                with_info(url, Box::new(move |result| tx.send(result).unwrap()));
                assert!(rx.recv().unwrap().is_err());
                let (tx, rx) = std::sync::mpsc::channel();
                fetch(&format!("pmtiles:0/0/0@{url}"), Box::new(move |result| tx.send(result).unwrap()));
                assert!(rx.recv().unwrap().is_err());
            });
        }
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
        forget_archive(url);
    }

    /// Revoked metadata/directory reads and dropped metadata cannot become cached successes.
    /// Ordinary metadata failures remain failures; a fresh live read restores the real style.
    #[test]
    fn retired_metadata_does_not_become_ready_archive_info() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let _turn = crate::cache::turn();
        let held = Arc::new(Mutex::new(Vec::<(String, Done)>::new()));
        let hold = held.clone();
        crate::cache::set_fetcher(move |url, done| hold.lock().unwrap().push((url, done)));
        let bytes = archive(&[(0, 0, 0, Vec::new())], 1, false, r#"{"vector_layers":[{"id":"roads"}],"attribution":"Test"}"#);
        let answer = |url: String, done: Done| {
            let (_, range) = crate::cache::split_range(&url);
            let (a, b) = range.unwrap();
            done(Ok(bytes[a as usize..((b + 1) as usize).min(bytes.len())].to_vec()));
        };
        let mut failures = Vec::new();
        for revoke in [false, true] {
            for (case, error) in [None, Some("HTTP 503"), Some(crate::cache::DROPPED)].into_iter().enumerate() {
                let url = format!("https://retired-metadata.invalid/{revoke}-{case}.pmtiles");
                let live = Arc::new(AtomicBool::new(true));
                let flag = live.clone();
                assert!(crate::cache::with_still(Arc::new(move || flag.load(SeqCst)), || info(&url)).unwrap().is_none());
                let (asked, done) = held.lock().unwrap().pop().unwrap();
                answer(asked, done);
                assert_eq!(held.lock().unwrap().len(), 1, "metadata is a separate real range read");
                live.store(!revoke, SeqCst);
                let (asked, done) = held.lock().unwrap().pop().unwrap();
                if let Some(error) = error {
                    done(Err(error.into()))
                } else {
                    answer(asked, done)
                }
                let retry = revoke || error == Some(crate::cache::DROPPED);
                if retry {
                    if archives().lock().unwrap().contains_key(&url) {
                        failures.push(format!("retired/dropped metadata cached with revoke={revoke}, error={error:?}"));
                        archives().lock().unwrap().remove(&url);
                    }
                    assert!(info(&url).unwrap().is_none());
                    for _ in 0..2 {
                        let (asked, done) = held.lock().unwrap().pop().unwrap();
                        answer(asked, done);
                    }
                }
                if let (false, Some(error)) = (retry, error) {
                    assert_eq!(info(&url).unwrap_err(), error, "a live metadata error cannot become ready information");
                } else {
                    let got = info(&url).unwrap().unwrap();
                    assert_eq!(got.vector_layers(), vec!["roads"]);
                    assert_eq!(got.attribution(), "Test");
                }
                assert!(held.lock().unwrap().is_empty());
            }
        }
        // A ready header can start a separate directory read. Revocation during that read
        // must not leave old directory bytes for another caller, even though its tile fails.
        let bytes = archive(&[(0, 0, 0, b"tile".to_vec())], 1, true, "{}");
        let answer = |url: String, done: Done| {
            let (_, range) = crate::cache::split_range(&url);
            let (a, b) = range.unwrap();
            done(Ok(bytes[a as usize..((b + 1) as usize).min(bytes.len())].to_vec()));
        };
        let url = "https://retired-metadata.invalid/directory.pmtiles";
        assert!(info(url).unwrap().is_none());
        for _ in 0..2 {
            let (asked, done) = held.lock().unwrap().pop().unwrap();
            answer(asked, done);
        }
        let live = Arc::new(AtomicBool::new(true));
        let flag = live.clone();
        let (done, result) = std::sync::mpsc::channel();
        let tile = format!("pmtiles:0/0/0@{url}");
        crate::cache::with_still(Arc::new(move || flag.load(SeqCst)), || fetch(&tile, Box::new(move |r| done.send(r).unwrap())));
        assert_eq!(held.lock().unwrap().len(), 1);
        live.store(false, SeqCst);
        let (asked, done) = held.lock().unwrap().pop().unwrap();
        answer(asked, done);
        assert!(result.recv().unwrap().is_err());
        if leaves().lock().unwrap().keys().any(|(a, _)| a == url) {
            failures.push("a retired reader cached a successful leaf directory".into());
            forget_archive(url);
        }
        let (done, result) = std::sync::mpsc::channel();
        fetch(&tile, Box::new(move |r| done.send(r).unwrap()));
        for _ in 0..4 {
            let next = held.lock().unwrap().pop();
            let Some((asked, done)) = next else { break };
            answer(asked, done);
        }
        assert_eq!(result.recv().unwrap().unwrap(), b"tile");
        assert!(failures.is_empty(), "{failures:?}");
    }

    /// A metadata or directory joiner retains its own lifetime before starting another range;
    /// revoking it does not stop the independent live loader or its tile.
    #[test]
    fn shared_archive_waiters_keep_their_own_lifetime() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let _turn = crate::cache::turn();
        let bytes = archive(&[(0, 0, 0, b"tile".to_vec())], 1, true, "{}");
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
                    assert!(sent <= 8, "bounded directory fixture");
                    let (_, range) = crate::cache::split_range(&url);
                    let (a, b) = range.unwrap();
                    done(Ok(bytes[a as usize..((b + 1) as usize).min(bytes.len())].to_vec()));
                }
            }
        };
        let mut failures = Vec::new();
        for join_leaf in [false, true] {
            let url = format!("https://shared-pmtiles.invalid/{join_leaf}.pmtiles");
            if join_leaf {
                assert!(info(&url).unwrap().is_none());
                assert_eq!(drain(), 2);
            }
            let tile = format!("pmtiles:0/0/0@{url}");
            let (first, first_result) = std::sync::mpsc::channel();
            fetch(&tile, Box::new(move |r| first.send(r).unwrap()));
            let live = Arc::new(AtomicBool::new(true));
            let flag = live.clone();
            let (done, result) = std::sync::mpsc::channel();
            crate::cache::with_still(Arc::new(move || flag.load(SeqCst)), || fetch(&tile, Box::new(move |r| done.send(r).unwrap())));
            assert_eq!(held.lock().unwrap().len(), 1, "the joiner shares the pending read");
            live.store(false, SeqCst);
            let sent = drain();
            assert_eq!(first_result.recv().unwrap().unwrap(), b"tile");
            if result.recv().unwrap().is_ok() || sent != if join_leaf { 2 } else { 4 } {
                failures.push(format!("join_leaf={join_leaf}: revoked joiner succeeded or sent extra ranges ({sent})"));
            }
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    #[test]
    fn reloaded_archive_rejects_old_metadata_and_leaf_answers() {
        let _turn = crate::cache::turn();
        let source = "/pmtiles-stale-completion-regression.pmtiles";
        let old_epoch = crate::cache::archive_epoch(source);
        let bytes = archive(&[(0, 0, 0, vec![1])], 1, true, "{}");
        let (old_info, _, _) = parse_head(&bytes).unwrap();
        crate::cache::reload_archive(source);
        let (tx, rx) = std::sync::mpsc::channel();
        archives()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(source.into(), State::Loading(vec![Box::new(move |result| tx.send(result.map(|info| info.attribution())).unwrap())]));
        let key = (source.to_string(), HEAD);
        let (leaf_tx, leaf_rx) = std::sync::mpsc::channel();
        leaves()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), Leaf::Loading(vec![Box::new(move |result| leaf_tx.send(result.map(|leaf| leaf.len())).unwrap())]));
        finish(source, Ok(old_info), old_epoch);
        finish_leaf(key.clone(), Ok(Arc::new(Vec::new())), old_epoch);
        assert!(matches!(rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
        assert!(matches!(leaf_rx.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
        // Global clears retain pending reads, so this regression is independent of other tests.
        assert!(matches!(archives().lock().unwrap_or_else(|e| e.into_inner()).get(source), Some(State::Loading(_))));
        assert!(matches!(leaves().lock().unwrap_or_else(|e| e.into_inner()).get(&key), Some(Leaf::Loading(_))));
        let (mut current, _, _) = parse_head(&bytes).unwrap();
        current.set_metadata(serde_json::json!({"attribution": "fresh credits"}));
        let epoch = crate::cache::archive_epoch(source);
        finish(source, Ok(current), epoch);
        finish_leaf(key, Ok(Arc::new(vec![Entry { tile_id: 0, offset: 0, length: 1, run_length: 1 }])), epoch);
        assert_eq!(rx.recv().unwrap().unwrap(), "fresh credits");
        assert_eq!(leaf_rx.recv().unwrap().unwrap(), 1);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn fresh_local_validation_rejects_bad_metadata_and_truncated_sections() {
        let path = std::env::temp_dir().join(format!("rustgis-pmtiles-validation-{}.pmtiles", std::process::id()));
        let source = path.to_str().unwrap();
        let valid = archive(&[(0, 0, 0, vec![1, 2, 3])], 1, true, r#"{"attribution":"fresh"}"#);
        std::fs::write(&path, &valid).unwrap();
        assert_eq!(read_info_local(source).unwrap().attribution(), "fresh");
        for metadata in ["{", "null", "[]"] {
            std::fs::write(&path, archive(&[(0, 0, 0, vec![1])], 1, false, metadata)).unwrap();
            assert!(read_info_local(source).is_err(), "{metadata}");
        }
        std::fs::write(&path, &valid[..valid.len() - 1]).unwrap();
        assert!(read_info_local(source).is_err());
        let mut truncated_metadata = valid.clone();
        truncated_metadata[24..32].copy_from_slice(&(valid.len() as u64 - 1).to_le_bytes());
        std::fs::write(&path, truncated_metadata).unwrap();
        assert!(read_info_local(source).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn indexed_archive(root: &[Entry], leaves: Vec<u8>) -> Vec<u8> {
        let mut head = archive(&[(0, 0, 0, vec![1, 2, 3])], 1, false, "{}")[..127].to_vec();
        let root = write_dir(root);
        let meta_at = 127 + root.len() as u64;
        let leaf_at = meta_at + 2;
        let data_at = leaf_at + leaves.len() as u64;
        for (at, value) in [(16, root.len() as u64), (24, meta_at), (40, leaf_at), (48, leaves.len() as u64), (56, data_at)] {
            head[at..at + 8].copy_from_slice(&value.to_le_bytes());
        }
        [head, root, b"{}".to_vec(), leaves, vec![1, 2, 3]].concat()
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn fresh_local_validation_checks_every_leaf_and_deduplicates_shared_ranges() {
        let path = std::env::temp_dir().join(format!("rustgis-pmtiles-leaves-{}.pmtiles", std::process::id()));
        let source = path.to_str().unwrap();
        let mut bad = archive(&[(0, 0, 0, vec![1, 2, 3])], 1, true, "{}");
        bad[138] = 0x80;
        std::fs::write(&path, bad).unwrap();
        assert_eq!(read_info_local(source).unwrap_err(), "a truncated directory");

        let first = write_dir(&[]);
        let second = write_dir(&[Entry { tile_id: 1, offset: 0, length: 3, run_length: 1 }]);
        let root = [
            Entry { tile_id: 0, offset: 0, length: first.len() as u64, run_length: 0 },
            Entry { tile_id: 1, offset: first.len() as u64, length: second.len() as u64, run_length: 0 },
            Entry { tile_id: 2, offset: 0, length: first.len() as u64, run_length: 0 },
        ];
        let leaves = [first.clone(), second].concat();
        let valid = indexed_archive(&root, leaves.clone());
        std::fs::write(&path, &valid).unwrap();
        assert!(read_info_local(source).is_ok());
        // Exactly enough for the root and two unique leaves; the repeated range costs no work.
        let bytes = (write_dir(&root).len() + leaves.len()) as u64;
        let exact = IndexBudget { stored: bytes, decoded: bytes, entries: 4, directories: 3 };
        assert!(read_info_local_bounded(source, exact).is_ok());
        for budget in [
            IndexBudget { stored: bytes - 1, ..exact },
            IndexBudget { decoded: bytes - 1, ..exact },
            IndexBudget { entries: 3, ..exact },
            IndexBudget { directories: 2, ..exact },
        ] {
            assert_eq!(read_info_local_bounded(source, budget).unwrap_err(), "PMTiles index exceeds validation budget");
        }

        let mut later_bad = valid;
        let leaf_at = u64_at(&later_bad, 40).unwrap() as usize;
        later_bad[leaf_at + leaves.len() - 1] = 0x80;
        std::fs::write(&path, later_bad).unwrap();
        assert_eq!(read_info_local(source).unwrap_err(), "a truncated directory");
        for entry in [
            Entry { tile_id: 0, offset: u64::MAX - 1, length: 2, run_length: 0 },
            Entry { tile_id: 0, offset: 0, length: leaves.len() as u64 + 1, run_length: 0 },
        ] {
            std::fs::write(&path, indexed_archive(&[entry], leaves.clone())).unwrap();
            let error = read_info_local(source).unwrap_err();
            assert!(error.contains("overflows") || error == "a leaf directory outside the archive", "{error}");
        }
        // Oversized leaf reads fail before opening a file, without a huge fixture.
        let (mut info, _, _) = parse_head(&archive(&[(0, 0, 0, vec![1])], 1, false, "{}")).unwrap();
        info.leaf_length = MAX_BYTES as u64 + 1;
        let entry = Entry { tile_id: 0, offset: 0, length: info.leaf_length, run_length: 0 };
        let mut budget = INDEX_BUDGET;
        assert_eq!(
            validate_directories("/nonexistent.pmtiles", &info, &[entry], 0, &mut budget, &mut HashMap::new()).unwrap_err(),
            format!("{} bytes is too large to read from an archive", entry.length)
        );
        // A declared compression on lazy tile payloads is not decoded during validation.
        let mut lazy = archive(&[(0, 0, 0, vec![1, 2, 3])], 1, true, "{}");
        lazy[98] = 2;
        std::fs::write(&path, lazy).unwrap();
        assert!(read_info_local(source).is_ok());
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn fresh_local_validation_checks_run_boundaries_in_roots_and_reachable_leaves() {
        let path = std::env::temp_dir().join(format!("rustgis-pmtiles-run-boundaries-{}.pmtiles", std::process::id()));
        let source = path.to_str().unwrap();
        for nested in [false, true] {
            for next_is_leaf in [false, true] {
                // [0, 2) overlaps id 1, is adjacent to id 2, and leaves a gap before id 4.
                for next_id in [1, 2, 4] {
                    let tile = Entry { tile_id: next_id, offset: 0, length: 3, run_length: 1 };
                    let mut leaves = if next_is_leaf { write_dir(&[tile]) } else { Vec::new() };
                    let next = if next_is_leaf { Entry { length: leaves.len() as u64, run_length: 0, ..tile } } else { tile };
                    let entries = [Entry { tile_id: 0, offset: 0, length: 3, run_length: 2 }, next];
                    let bytes = if nested {
                        let directory = write_dir(&entries);
                        let root = Entry { tile_id: 0, offset: leaves.len() as u64, length: directory.len() as u64, run_length: 0 };
                        leaves.extend(directory);
                        indexed_archive(&[root], leaves)
                    } else {
                        indexed_archive(&entries, leaves)
                    };
                    std::fs::write(&path, bytes).unwrap();
                    let result = read_info_local(source);
                    if next_id == 1 {
                        assert_eq!(result.unwrap_err(), "overlapping PMTiles tile runs", "nested={nested}, next_is_leaf={next_is_leaf}");
                    } else {
                        assert!(result.is_ok(), "nested={nested}, next_is_leaf={next_is_leaf}, next_id={next_id}: {result:?}");
                    }
                }
            }
        }
        // The final run has no following entry, but its exclusive end must still not overflow.
        let entries = [Entry { tile_id: u64::MAX, offset: 0, length: 3, run_length: 1 }];
        std::fs::write(&path, indexed_archive(&entries, Vec::new())).unwrap();
        assert_eq!(read_info_local(source).unwrap_err(), "tile ids overflow");
        std::fs::remove_file(path).unwrap();
    }

    /// Two root pointers; the first reaches [0, 3), optionally through an intermediate directory,
    /// while the second reaches an empty directory. `next_id = 2` hides tile 2 from lookup.
    #[cfg(not(target_arch = "wasm32"))]
    fn ancestor_boundary_archive(nested: bool, next_id: u64) -> Vec<u8> {
        let mut leaves = write_dir(&[Entry { tile_id: 0, offset: 0, length: 3, run_length: 3 }]);
        let mut first = Entry { tile_id: 0, offset: 0, length: leaves.len() as u64, run_length: 0 };
        if nested {
            let directory = write_dir(&[first]);
            first.offset = leaves.len() as u64;
            first.length = directory.len() as u64;
            leaves.extend(directory);
        }
        let empty = write_dir(&[]);
        let next = Entry { tile_id: next_id, offset: leaves.len() as u64, length: empty.len() as u64, run_length: 0 };
        leaves.extend(empty);
        indexed_archive(&[first, next], leaves)
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn fresh_local_validation_checks_descendant_runs_against_ancestor_boundaries() {
        let path = std::env::temp_dir().join(format!("rustgis-pmtiles-ancestor-boundaries-{}.pmtiles", std::process::id()));
        let source = path.to_str().unwrap();
        for nested in [false, true] {
            // [0, 3) crosses a boundary at 2, ends exactly at 3, and leaves a gap before 5.
            for next_id in [2, 3, 5] {
                std::fs::write(&path, ancestor_boundary_archive(nested, next_id)).unwrap();
                let result = read_info_local(source);
                if next_id == 2 {
                    assert_eq!(result.unwrap_err(), "overlapping PMTiles tile runs", "nested={nested}");
                } else {
                    assert!(result.is_ok(), "nested={nested}, next_id={next_id}: {result:?}");
                }
            }
        }
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn fresh_local_validation_checks_descendant_tiles_against_pointer_lower_bounds() {
        let path = std::env::temp_dir().join(format!("rustgis-pmtiles-pointer-lower-bounds-{}.pmtiles", std::process::id()));
        let source = path.to_str().unwrap();
        for nested in [false, true] {
            for tile_id in [0, 5] {
                for pointer_id in [0, 5, 6] {
                    let mut leaves = write_dir(&[Entry { tile_id, offset: 0, length: 3, run_length: 1 }]);
                    let mut pointer = Entry { tile_id: 0, offset: 0, length: leaves.len() as u64, run_length: 0 };
                    if nested {
                        let directory = write_dir(&[pointer]);
                        pointer.offset = leaves.len() as u64;
                        pointer.length = directory.len() as u64;
                        leaves.extend(directory);
                    }
                    pointer.tile_id = pointer_id;
                    std::fs::write(&path, indexed_archive(&[pointer], leaves)).unwrap();
                    let result = read_info_local(source);
                    if pointer_id > tile_id {
                        assert_eq!(
                            result.unwrap_err(),
                            "a PMTiles leaf directory contains tiles below its pointer",
                            "nested={nested}, pointer_id={pointer_id}, tile_id={tile_id}"
                        );
                    } else {
                        assert!(result.is_ok(), "nested={nested}, pointer_id={pointer_id}, tile_id={tile_id}: {result:?}");
                    }
                }
            }
        }
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn memoized_directory_rechecks_pointer_lower_bounds_without_io_or_budget() {
        let path = std::env::temp_dir().join(format!("rustgis-pmtiles-memo-lower-bounds-{}.pmtiles", std::process::id()));
        let source = path.to_str().unwrap();
        let leaf = write_dir(&[Entry { tile_id: 5, offset: 0, length: 3, run_length: 1 }]);
        let pointer = Entry { tile_id: 0, offset: 0, length: leaf.len() as u64, run_length: 0 };
        let bytes = indexed_archive(&[pointer], leaf.clone());
        let (info, _, _) = parse_head(&bytes).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let mut budget = IndexBudget { stored: leaf.len() as u64, decoded: leaf.len() as u64, entries: 1, directories: 1 };
        let mut seen = HashMap::new();
        validate_directories(source, &info, &[pointer], 0, &mut budget, &mut seen).unwrap();
        assert_eq!((budget.stored, budget.decoded, budget.entries, budget.directories), (0, 0, 0, 0));
        std::fs::remove_file(&path).unwrap();
        // The same completed subtree is legal below its first tile and at equality, but not above.
        for tile_id in [0, 5, 6] {
            let result = validate_directories(source, &info, &[Entry { tile_id, ..pointer }], 0, &mut budget, &mut seen);
            if tile_id > 5 {
                assert_eq!(result.unwrap_err(), "a PMTiles leaf directory contains tiles below its pointer");
            } else {
                assert!(result.is_ok(), "pointer_id={tile_id}: {result:?}");
            }
            assert_eq!((budget.stored, budget.decoded, budget.entries, budget.directories), (0, 0, 0, 0));
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn memoized_directory_rechecks_each_parent_boundary_without_io_or_budget() {
        let path = std::env::temp_dir().join(format!("rustgis-pmtiles-memo-boundaries-{}.pmtiles", std::process::id()));
        let source = path.to_str().unwrap();
        let leaf = write_dir(&[Entry { tile_id: 0, offset: 0, length: 3, run_length: 3 }]);
        let pointer = Entry { tile_id: 0, offset: 0, length: leaf.len() as u64, run_length: 0 };
        let bytes = indexed_archive(&[pointer], leaf.clone());
        let (info, _, _) = parse_head(&bytes).unwrap();
        std::fs::write(&path, bytes).unwrap();
        // Exactly one leaf's work is available; the initial visit consumes all of it.
        let mut budget = IndexBudget { stored: leaf.len() as u64, decoded: leaf.len() as u64, entries: 1, directories: 1 };
        let mut seen = HashMap::new();
        let summary = validate_directories(source, &info, &[pointer], 0, &mut budget, &mut seen).unwrap();
        assert_eq!((summary.height, summary.tile_end), (1, 3));
        assert_eq!((budget.stored, budget.decoded, budget.entries, budget.directories), (0, 0, 0, 0));
        // Reused references must neither reopen the removed file nor charge the exhausted budget.
        std::fs::remove_file(&path).unwrap();
        for next_id in [3, 5, 2] {
            let parent = [pointer, Entry { tile_id: next_id, offset: 0, length: 3, run_length: 1 }];
            let result = validate_directories(source, &info, &parent, 0, &mut budget, &mut seen);
            if next_id == 2 {
                assert_eq!(result.unwrap_err(), "overlapping PMTiles tile runs");
            } else {
                let summary = result.unwrap();
                assert_eq!((summary.height, summary.tile_end), (1, next_id + 1));
            }
            assert_eq!((budget.stored, budget.decoded, budget.entries, budget.directories), (0, 0, 0, 0));
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn fresh_local_validation_cancels_before_reading_a_later_malformed_leaf() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CancelBeforeThirdEntry(AtomicUsize);
        impl rustgis_data::progress::Probe for CancelBeforeThirdEntry {
            fn cancelled(&self) -> bool {
                self.0.fetch_add(1, Ordering::Relaxed) >= 2
            }
            fn begin(&self, _: u64) {}
            fn advance(&self, _: u64) {}
        }

        let path = std::env::temp_dir().join(format!("rustgis-pmtiles-cancel-traversal-{}.pmtiles", std::process::id()));
        let source = path.to_str().unwrap();
        let first = write_dir(&[Entry { tile_id: 0, offset: 0, length: 3, run_length: 1 }]);
        let root = [
            Entry { tile_id: 0, offset: 0, length: first.len() as u64, run_length: 0 },
            Entry { tile_id: 1, offset: first.len() as u64, length: 1, run_length: 0 },
        ];
        let mut leaves = first;
        leaves.push(0x80);
        std::fs::write(&path, indexed_archive(&root, leaves)).unwrap();
        assert_eq!(read_info_local(source).unwrap_err(), "a truncated directory");
        let probe = Arc::new(CancelBeforeThirdEntry(AtomicUsize::new(0)));
        {
            let _installed = rustgis_data::progress::install(Some(probe.clone()));
            assert_eq!(read_info_local(source).unwrap_err(), rustgis_data::DataError::Cancelled.to_string());
        }
        // Root pointer, its reachable tile, then the next root pointer: stop before its I/O.
        assert_eq!(probe.0.load(Ordering::Relaxed), 3);
        assert_eq!(read_info_local(source).unwrap_err(), "a truncated directory");
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn fresh_local_validation_rejects_cycles_and_excess_depth_even_when_shared() {
        let path = std::env::temp_dir().join(format!("rustgis-pmtiles-depth-{}.pmtiles", std::process::id()));
        let source = path.to_str().unwrap();
        let pointer = |offset| Entry { tile_id: 0, offset, length: 5, run_length: 0 };
        let tile = Entry { tile_id: 0, offset: 0, length: 3, run_length: 1 };
        std::fs::write(&path, indexed_archive(&[pointer(0)], write_dir(&[pointer(0)]))).unwrap();
        assert_eq!(read_info_local(source).unwrap_err(), "cyclic PMTiles directories");
        let mutual: Vec<u8> = [pointer(5), pointer(0)].iter().flat_map(|entry| write_dir(&[*entry])).collect();
        std::fs::write(&path, indexed_archive(&[pointer(0)], mutual)).unwrap();
        assert_eq!(read_info_local(source).unwrap_err(), "cyclic PMTiles directories");
        for count in [MAX_DEPTH, MAX_DEPTH + 1] {
            let leaves: Vec<u8> =
                (0..count).flat_map(|i| write_dir(&[if i + 1 == count { tile } else { pointer((i as u64 + 1) * 5) }])).collect();
            std::fs::write(&path, indexed_archive(&[pointer(0)], leaves)).unwrap();
            if count == MAX_DEPTH {
                assert!(read_info_local(source).is_ok());
            } else {
                assert_eq!(read_info_local(source).unwrap_err(), "PMTiles directories nested too deep");
            }
        }
        // Validate a shared subtree shallowly first, then reach it by an over-deep path.
        let leaves: Vec<u8> =
            [pointer(5), tile, pointer(15), pointer(20), pointer(0)].iter().flat_map(|entry| write_dir(&[*entry])).collect();
        let root = [pointer(0), Entry { tile_id: 1, ..pointer(10) }];
        std::fs::write(&path, indexed_archive(&root, leaves)).unwrap();
        assert_eq!(read_info_local(source).unwrap_err(), "PMTiles directories nested too deep");
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn ordinary_loaders_reject_present_invalid_metadata_with_specific_errors() {
        let _turn = crate::cache::turn();
        let dir = std::env::temp_dir().join(format!("rustgis-pmtiles-metadata-errors-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cases = Vec::new();
        for (text, error) in [
            ("{", "PMTiles metadata:"),
            ("null", "PMTiles metadata must be a JSON object"),
            ("[]", "PMTiles metadata must be a JSON object"),
            ("42", "PMTiles metadata must be a JSON object"),
        ] {
            cases.push((archive(&[(0, 0, 0, vec![1])], 1, false, text), error));
        }
        let mut truncated = archive(&[(0, 0, 0, vec![1])], 1, false, "{}");
        truncated[32..40].copy_from_slice(&100u64.to_le_bytes());
        cases.push((truncated, "truncated PMTiles metadata"));
        for (code, error) in [(2, "gzip: invalid header"), (3, "Brotli:"), (4, "Zstandard:")] {
            let mut invalid = archive_with(&[(0, 0, 0, vec![1])], 1, false, "{}", code);
            let at = u64_at(&invalid, 24).unwrap() as usize;
            invalid[at] = 0xff;
            invalid[32..40].copy_from_slice(&1u64.to_le_bytes());
            cases.push((invalid, error));
        }
        let mut oversized = archive(&[(0, 0, 0, vec![1])], 1, false, "{}");
        oversized[32..40].copy_from_slice(&(MAX_BYTES as u64 + 1).to_le_bytes());
        // Store the whole oversized (otherwise valid JSON) section, not merely a header
        // claiming an invalid range. The ordinary loader must reject it before reading it.
        let metadata_at = u64_at(&oversized, 24).unwrap() as usize;
        let metadata_end = metadata_at + MAX_BYTES + 1;
        oversized.truncate(metadata_at + 2);
        oversized.resize(metadata_end, b' ');
        oversized.push(1);
        oversized[56..64].copy_from_slice(&(metadata_end as u64).to_le_bytes());
        cases.push((oversized, "PMTiles metadata is too large or has an invalid range"));
        for (i, (bytes, expected)) in cases.into_iter().enumerate() {
            // Separate paths force both entry points through their ordinary loader, not a
            // failure memoized by the preceding assertion.
            for loader in ["info", "get"] {
                let path = dir.join(format!("{i}-{loader}.pmtiles"));
                std::fs::write(&path, &bytes).unwrap();
                let source = path.to_str().unwrap();
                let error = if loader == "info" {
                    info_blocking(source, std::time::Duration::from_secs(10)).unwrap_err()
                } else {
                    get(&format!("pmtiles:0/0/0@{source}")).unwrap_err()
                };
                assert!(error.starts_with(expected), "case {i}, {loader}: {error}");
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tile_ids_follow_the_spec() {
        assert_eq!(tile_id(0, 0, 0), 0);
        // Zoom 1: (0,0), (0,1), (1,1), (1,0).
        assert_eq!([tile_id(1, 0, 0), tile_id(1, 0, 1), tile_id(1, 1, 1), tile_id(1, 1, 0)], [1, 2, 3, 4]);
        assert_eq!(tile_id(2, 0, 0), 5);
        // Every id of zoom 3 once.
        let mut ids: Vec<u64> = (0..8).flat_map(|x| (0..8).map(move |y| tile_id(3, x, y))).collect();
        ids.sort_unstable();
        assert_eq!(ids, (21..85).collect::<Vec<_>>());
    }

    #[test]
    fn directories_round_trip_and_hostile_ones_fail() {
        // Three entries: ids 1, 2, 5; runs 1, 1, 0 (a leaf); lengths 10, 20, 30; offsets 0, next, 100.
        let mut b = vec![3u8];
        b.extend([1, 1, 3]);
        b.extend([1, 1, 0]);
        b.extend([10, 20, 30]);
        b.extend([1, 0, 101]);
        let d = parse_dir(&b).unwrap();
        assert_eq!(
            d.iter().map(|e| (e.tile_id, e.offset, e.length, e.run_length)).collect::<Vec<_>>(),
            vec![(1, 0, 10, 1), (2, 10, 20, 1), (5, 100, 30, 0)]
        );
        assert_eq!(lookup(&d, 2).map(|e| e.offset), Some(10));
        assert_eq!(lookup(&d, 3), None);
        assert_eq!(lookup(&d, 9).map(|e| e.run_length), Some(0));
        assert!(parse_dir(&[0xff, 0xff, 0xff, 0xff, 0x7f]).is_err());
        assert!(parse_dir(&[]).is_err());
        for n in 0..b.len() {
            let _ = parse_dir(&b[..n]);
        }
        // Zstandard in several frames, a skippable one among them: all of it.
        let zstd = |b: &[u8]| ruzstd::encoding::compress_to_vec(b, ruzstd::encoding::CompressionLevel::Fastest);
        let mut frames = zstd(b"first ");
        frames.extend([0x5a, 0x2a, 0x4d, 0x18, 2, 0, 0, 0, 9, 9]);
        frames.extend(zstd(b"second"));
        assert_eq!(Compression::Zstd.decode(frames.clone()).unwrap(), b"first second");
        assert!(Compression::Zstd.decode(frames[..frames.len() - 3].to_vec()).is_err());
        assert!(unzstd(&frames, 8).is_err());
        // A frame header declaring a 1 GB window.
        assert!(unzstd(&[0x28, 0xb5, 0x2f, 0xfd, 0x00, 0xa0, 1, 0, 0], 1 << 20).is_err());
        // A skippable frame claiming more than there is.
        assert!(unzstd(&[0x50, 0x2a, 0x4d, 0x18, 0xff, 0xff, 0xff, 0xff], 64).is_err());
        // Junk that claims to be compressed fails rather than panics.
        for c in [Compression::Gzip, Compression::Brotli, Compression::Zstd] {
            for junk in [&b""[..], b"\x1f\x8b", b"\xff\xff\xff\xff", b"\x28\xb5\x2f\xfd\x00"] {
                let _ = c.decode(junk.to_vec());
            }
        }
        assert!(parse_head(b"PMTiles\x02").is_err());
        assert!(parse_head(b"nope").is_err());
        assert!(parse_url("pmtiles:3/1/2@https://x/a.pmtiles").is_some());
        assert!(parse_url("pmtiles:99/1/2@x").is_none() && parse_url("pmtiles:1/2@x").is_none());
    }

    /// A PMTiles archive of `tiles` (uncompressed), its directory in the root or in a leaf.
    pub(crate) fn archive(tiles: &[(u8, u32, u32, Vec<u8>)], tile_type: u8, leaf: bool, metadata: &str) -> Vec<u8> {
        archive_with(tiles, tile_type, leaf, metadata, 1)
    }

    /// Compresses bytes as the header's compression code says (1 none, 2 gzip, 3 Brotli, 4 Zstandard).
    fn compress(b: &[u8], code: u8) -> Vec<u8> {
        use std::io::Write;
        match code {
            2 => {
                let mut gz = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
                gz.extend(miniz_oxide::deflate::compress_to_vec(b, 6));
                gz.extend([0u8; 8]);
                gz
            }
            3 => {
                let mut out = Vec::new();
                let mut w = brotli::CompressorWriter::new(&mut out, 4096, 5, 22);
                w.write_all(b).unwrap();
                drop(w);
                out
            }
            4 => ruzstd::encoding::compress_to_vec(b, ruzstd::encoding::CompressionLevel::Fastest),
            _ => b.to_vec(),
        }
    }

    /// An archive whose directories, metadata and tiles are compressed with `code`.
    fn archive_with(tiles: &[(u8, u32, u32, Vec<u8>)], tile_type: u8, leaf: bool, metadata: &str, code: u8) -> Vec<u8> {
        let tiles: Vec<(u8, u32, u32, Vec<u8>)> = tiles.iter().map(|(z, x, y, b)| (*z, *x, *y, compress(b, code))).collect();
        let dir = |e: &[Entry]| compress(&write_dir(e), code);
        let mut tiles: Vec<(u64, &Vec<u8>)> = tiles.iter().map(|(z, x, y, b)| (tile_id(*z, *x, *y), b)).collect();
        tiles.sort_by_key(|t| t.0);
        let mut data = Vec::new();
        let mut entries = Vec::new();
        for (id, b) in &tiles {
            entries.push(Entry { tile_id: *id, offset: data.len() as u64, length: b.len() as u64, run_length: 1 });
            data.extend_from_slice(b);
        }
        let (root, leaves) = if leaf {
            let l = dir(&entries);
            (dir(&[Entry { tile_id: entries[0].tile_id, offset: 0, length: l.len() as u64, run_length: 0 }]), l)
        } else {
            (dir(&entries), Vec::new())
        };
        let meta = compress(metadata.as_bytes(), code);
        let meta = meta.as_slice();
        let root_at = 127u64;
        let meta_at = root_at + root.len() as u64;
        let leaf_at = meta_at + meta.len() as u64;
        let data_at = leaf_at + leaves.len() as u64;
        let mut h = b"PMTiles".to_vec();
        h.push(3);
        for v in [root_at, root.len() as u64, meta_at, meta.len() as u64, leaf_at, leaves.len() as u64, data_at, data.len() as u64] {
            h.extend_from_slice(&v.to_le_bytes());
        }
        h.extend_from_slice(&[0u8; 24]);
        h.push(1); // clustered
        h.extend_from_slice(&[code, code, tile_type, 0, 2]); // compression; type; zooms 0–2
        for v in [-1_800_000_000i32, -850_000_000, 1_800_000_000, 850_000_000] {
            h.extend_from_slice(&v.to_le_bytes());
        }
        h.push(0);
        h.extend_from_slice(&[0u8; 8]);
        assert_eq!(h.len(), 127);
        [h, root, meta.to_vec(), leaves, data].concat()
    }

    fn get(url: &str) -> Result<Vec<u8>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        fetch(url, Box::new(move |r| tx.send(r).unwrap_or_default()));
        rx.recv_timeout(std::time::Duration::from_secs(10)).map_err(|e| e.to_string())?
    }

    #[test]
    fn written_archives_read_back() {
        let _turn = crate::cache::turn();
        let dir = std::env::temp_dir().join(format!("rustgis-pmtiles-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let new = |tile_type| NewArchive {
            tile_type,
            gzipped_tiles: false,
            bounds: [-10.0, -5.0, 10.0, 5.0],
            metadata: serde_json::json!({"vector_layers": [{"id": "roads"}], "attribution": "Written credits"}),
        };
        for metadata in [Value::Null, serde_json::json!([]), serde_json::json!("credits"), serde_json::json!(1), serde_json::json!(true)] {
            let invalid = NewArchive { metadata, ..new(TileType::Mvt) };
            assert_eq!(write(vec![(0, 0, 0, b"tile".to_vec())], &invalid).unwrap_err(), "PMTiles metadata must be a JSON object");
        }
        {
            let mut bounded = NewArchive { metadata: serde_json::json!({"attribution": ""}), ..new(TileType::Mvt) };
            let overhead = serde_json::to_vec(&bounded.metadata).unwrap().len();
            for len in [MAX_BYTES, MAX_BYTES + 1] {
                bounded.metadata["attribution"] = Value::String("x".repeat(len - overhead));
                assert_eq!(serde_json::to_vec(&bounded.metadata).unwrap().len(), len);
                let written = write(vec![(0, 0, 0, b"tile".to_vec())], &bounded);
                if len == MAX_BYTES {
                    let bytes = written.unwrap();
                    let (_, _, meta_len) = parse_head(&bytes).unwrap();
                    assert_eq!(meta_len, MAX_BYTES as u64);
                    let p = dir.join("metadata-limit.pmtiles");
                    std::fs::write(&p, &bytes).unwrap();
                    #[cfg(not(target_arch = "wasm32"))]
                    assert_eq!(read_info_local(p.to_str().unwrap()).unwrap().metadata, bounded.metadata);
                } else {
                    assert_eq!(written.unwrap_err(), "PMTiles metadata exceeds the 16 MiB limit");
                }
            }
        }
        // A few tiles, one given twice and two alike (stored once).
        let small =
            vec![(0u8, 0u32, 0u32, b"old".to_vec()), (1, 0, 0, b"same".to_vec()), (1, 0, 1, b"same".to_vec()), (0, 0, 0, b"zero".to_vec())];
        let bytes = write(small, &new(TileType::Mvt)).unwrap();
        let p = dir.join("small.pmtiles");
        std::fs::write(&p, &bytes).unwrap();
        let (info, _, _) = parse_head(&bytes).unwrap();
        assert_eq!((info.tile_type, info.min_zoom, info.max_zoom), (TileType::Mvt, 0, 1));
        assert_eq!(info.bounds, [-10.0, -5.0, 10.0, 5.0]);
        // Latitudes past the poles are clamped to them.
        let far = NewArchive { bounds: [-200.0, -95.0, 200.0, 95.0], ..new(TileType::Mvt) };
        let (info, _, _) = parse_head(&write(vec![(0, 0, 0, b"t".to_vec())], &far).unwrap()).unwrap();
        assert_eq!(info.bounds, [-180.0, -90.0, 180.0, 90.0]);
        let a = p.display().to_string();
        assert_eq!(get(&format!("pmtiles:0/0/0@{a}")).unwrap(), b"zero");
        assert_eq!(get(&format!("pmtiles:1/0/0@{a}")).unwrap(), b"same");
        assert_eq!(get(&format!("pmtiles:1/0/1@{a}")).unwrap(), b"same");
        assert!(get(&format!("pmtiles:1/1/1@{a}")).unwrap().is_empty());
        #[cfg(not(target_arch = "wasm32"))]
        assert_eq!(info_blocking(&a, std::time::Duration::from_secs(10)).unwrap().attribution(), "Written credits");
        #[cfg(not(target_arch = "wasm32"))]
        assert_eq!(read_info_local(&a).unwrap().attribution(), "Written credits");
        // Every tile to zoom 7, each different: too many for the root, so in leaves.
        let many: Vec<(u8, u32, u32, Vec<u8>)> = (0..=7u8)
            .flat_map(|z| (0..1u32 << z).flat_map(move |x| (0..1u32 << z).map(move |y| (z, x, y, format!("{z}/{x}/{y}").into_bytes()))))
            .collect();
        let bytes = write(many, &new(TileType::Png)).unwrap();
        let (info, _, _) = parse_head(&bytes).unwrap();
        assert!(info.leaf_length > 0);
        let p = dir.join("many.pmtiles");
        std::fs::write(&p, &bytes).unwrap();
        let a = p.display().to_string();
        for (z, x, y) in [(0, 0, 0), (5, 17, 3), (7, 127, 127), (7, 0, 99)] {
            assert_eq!(get(&format!("pmtiles:{z}/{x}/{y}@{a}")).unwrap(), format!("{z}/{x}/{y}").into_bytes());
        }
        assert!(write(Vec::new(), &new(TileType::Mvt)).is_err());
        for (z, x, y) in [(0, 1, 0), (2, 0, 4), (31, 0, 0)] {
            assert!(write(vec![(0, 0, 0, b"ok".to_vec()), (z, x, y, b"bad".to_vec())], &new(TileType::Mvt)).is_err(), "{z}/{x}/{y}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_tiles_from_root_and_leaf_directories() {
        let _turn = crate::cache::turn();
        let dir = std::env::temp_dir().join(format!("rustgis-pmtiles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tiles = vec![(0u8, 0u32, 0u32, b"zero".to_vec()), (1, 1, 0, b"one-one-zero".to_vec()), (2, 3, 3, b"two".to_vec())];
        // Uncompressed, gzip, Brotli and Zstandard archives.
        for (leaf, code) in [false, true].into_iter().flat_map(|l| (1..=4).map(move |c| (l, c))) {
            let p = dir.join(format!("t{leaf}{code}.pmtiles"));
            std::fs::write(&p, archive_with(&tiles, 1, leaf, r#"{"vector_layers":[{"id":"roads"}],"attribution":"Test"}"#, code)).unwrap();
            let a = p.display().to_string();
            #[cfg(not(target_arch = "wasm32"))]
            assert_eq!(read_info_local(&a).unwrap().attribution(), "Test");
            let mut i = info(&a).unwrap();
            for _ in 0..200 {
                if i.is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
                i = info(&a).unwrap();
            }
            let i = i.unwrap();
            assert_eq!(i.tile_type, TileType::Mvt);
            assert_eq!((i.min_zoom, i.max_zoom), (0, 2));
            assert_eq!(i.vector_layers(), vec!["roads".to_string()]);
            assert_eq!(i.attribution(), "Test");
            for (z, x, y, b) in &tiles {
                assert_eq!(&get(&format!("pmtiles:{z}/{x}/{y}@{a}")).unwrap(), b, "{z}/{x}/{y} leaf={leaf} compression={code}");
            }
            // Not in the archive, outside its zooms, or not a tile of its zoom: no tile.
            assert!(get(&format!("pmtiles:1/0/0@{a}")).unwrap().is_empty());
            assert!(get(&format!("pmtiles:1/2/0@{a}")).unwrap().is_empty());
            assert!(get(&format!("pmtiles:9/0/0@{a}")).unwrap().is_empty());
        }
        // Not an archive.
        let bad = dir.join("bad.pmtiles");
        std::fs::write(&bad, b"not one").unwrap();
        assert!(get(&format!("pmtiles:0/0/0@{}", bad.display())).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_raster_archive_resolves_and_decodes_and_truncations_fail_cleanly() {
        let _turn = crate::cache::turn();
        let dir = std::env::temp_dir().join(format!("rustgis-pmtiles-raster-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(256, 256, image::Rgba([200, 30, 30, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let bytes = archive(&[(0, 0, 0, png)], 2, false, "{}");
        let p = dir.join("red.pmtiles");
        std::fs::write(&p, &bytes).unwrap();
        let a = p.display().to_string();
        let spec = crate::source::Spec {
            kind: crate::source::Kind::PmTiles,
            url: a.clone(),
            subdomains: Vec::new(),
            min_zoom: 0,
            max_zoom: 19,
            tile_size: 256,
            tms: false,
            attribution: String::new(),
            layers: Vec::new(),
            style_layers: String::new(),
            hidden_layers: Vec::new(),
        };
        let mut resolved = crate::source::resolve(&spec);
        for _ in 0..200 {
            if !matches!(resolved, crate::source::Resolve::Pending) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            resolved = crate::source::resolve(&spec);
        }
        let crate::source::Resolve::Ready(r) = resolved else { panic!("not resolved") };
        assert_eq!(r.style.sources.len(), 1);
        // Through the cache, decoded as a picture.
        let url = format!("pmtiles:0/0/0@{a}");
        let mut state = crate::cache::get(&url, crate::cache::Want::Image);
        for _ in 0..100 {
            if !matches!(state, crate::cache::State::Pending) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            state = crate::cache::get(&url, crate::cache::Want::Image);
        }
        match state {
            crate::cache::State::Ready(p) => match &*p {
                crate::cache::Payload::Image(pm) => assert_eq!(pm.pixel(10, 10).map(|c| c.red()), Some(200)),
                _ => panic!("not a picture"),
            },
            _ => panic!("tile not read"),
        }
        // Every truncation of an archive is an error, never a panic.
        for n in (0..bytes.len()).step_by(7) {
            let t = dir.join(format!("cut{n}.pmtiles"));
            std::fs::write(&t, &bytes[..n]).unwrap();
            let _ = get(&format!("pmtiles:0/0/0@{}", t.display()));
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
