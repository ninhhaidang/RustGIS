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
//! file and bounded, and nesting is limited.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::Value;
use web_time::{Duration, Instant};

use crate::cache::{Done, MAX_BYTES};

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
            Compression::Gzip => gunzip(&b),
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
    /// The JSON metadata (`vector_layers`, `attribution`, `name`…).
    pub metadata: Value,
}

/// Writes a PMTiles (v3) archive of `tiles` (zoom, x, y, bytes): clustered, identical tiles
/// stored once, its directories uncompressed, in leaf directories when the root would pass the
/// first 16 KB. Tiles given twice keep the last.
pub fn write(tiles: Vec<(u8, u32, u32, Vec<u8>)>, a: &NewArchive) -> Result<Vec<u8>, String> {
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
    let meta = serde_json::to_vec(&a.metadata).map_err(|e| e.to_string())?;
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

/// An archive's header and metadata: `Ok(None)` while they are read (the map draws again once
/// they arrive).
pub fn info(archive: &str) -> Result<Option<Arc<Info>>, String> {
    crate::logins::check(archive).map_err(str::to_string)?;
    let mut all = archives().lock().unwrap_or_else(|e| e.into_inner());
    match all.get(archive) {
        Some(State::Ready(i)) => return Ok(Some(i.clone())),
        Some(State::Loading(_)) => return Ok(None),
        Some(State::Failed(e, at)) if at.elapsed() < RETRY => return Err(e.clone()),
        _ => {}
    }
    all.insert(archive.to_string(), State::Loading(Vec::new()));
    drop(all);
    load(archive.to_string());
    Ok(None)
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
    if let Err(e) = crate::logins::check(archive) {
        f(Err(e.to_string()));
        return;
    }
    let mut all = archives().lock().unwrap_or_else(|e| e.into_inner());
    match all.get_mut(archive) {
        Some(State::Ready(i)) => {
            let i = i.clone();
            drop(all);
            f(Ok(i));
        }
        Some(State::Loading(waiting)) => waiting.push(f),
        Some(State::Failed(e, at)) if at.elapsed() < RETRY => {
            let e = e.clone();
            drop(all);
            f(Err(e));
        }
        _ => {
            all.insert(archive.to_string(), State::Loading(vec![f]));
            drop(all);
            load(archive.to_string());
        }
    }
}

/// Reads an archive's header, root directory and metadata, then answers whoever waits.
fn load(archive: String) {
    let a = archive.clone();
    read(
        &archive,
        0,
        HEAD,
        Box::new(move |head| {
            let parsed = head.and_then(|b| parse_head(&b));
            match parsed {
                Err(e) => finish(&a, Err(e)),
                Ok((mut info, meta_at, meta_len)) => {
                    if meta_len == 0 {
                        finish(&a, Ok(info));
                        return;
                    }
                    let a2 = a.clone();
                    read(
                        &a,
                        meta_at,
                        meta_len,
                        Box::new(move |m| {
                            // Unreadable metadata only loses the layer names and credits.
                            if let Ok(m) = m {
                                let m = info.internal.decode(m).unwrap_or_default();
                                info.set_metadata(serde_json::from_slice(&m).unwrap_or(Value::Null));
                            }
                            finish(&a2, Ok(info));
                        }),
                    );
                }
            }
        }),
    );
}

fn finish(archive: &str, r: Result<Info, String>) {
    let r = crate::logins::check(archive)
        .map_err(str::to_string)
        .and(r)
        .map(Arc::new)
        .map_err(|e| if rustgis_data::login_address::is_wrapper(archive) { crate::logins::scrub(&e) } else { e });
    let waiting = {
        let mut all = archives().lock().unwrap_or_else(|e| e.into_inner());
        let old = match &r {
            Ok(i) => all.insert(archive.to_string(), State::Ready(i.clone())),
            // A read the busy queue dropped isn't the archive's fault: forgotten, so the next
            // draw reads it again, rather than kept as a failure.
            Err(e) if e == crate::cache::DROPPED => all.remove(archive),
            Err(e) => all.insert(archive.to_string(), State::Failed(e.clone(), Instant::now())),
        };
        match old {
            Some(State::Loading(w)) => w,
            _ => Vec::new(),
        }
    };
    for w in waiting {
        w(r.clone());
    }
    crate::cache::notify();
}

/// Fetches a tile address (`pmtiles:z/x/y@archive`): its bytes, or none for a tile the archive
/// doesn't have.
pub(crate) fn fetch(url: &str, done: Done) {
    let Some((z, x, y, archive)) = parse_url(url) else {
        done(Err(format!("{url}: not a PMTiles tile address")));
        return;
    };
    let a = archive.clone();
    with_info(
        &archive,
        Box::new(move |info| match info {
            Err(e) => done(Err(e)),
            Ok(info) => {
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
    // One read per leaf: tiles needing a leaf already on its way wait for it.
    {
        let mut l = leaves().lock().unwrap_or_else(|e| e.into_inner());
        match l.get_mut(&key) {
            Some(Leaf::Ready(leaf)) => {
                let leaf = leaf.clone();
                drop(l);
                next(Ok(leaf));
                return;
            }
            Some(Leaf::Loading(waiting)) => {
                waiting.push(next);
                return;
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
    }
    read(
        &archive,
        at,
        e.length,
        Box::new(move |b| {
            let leaf = b.and_then(|b| info.internal.decode(b)).and_then(|b| parse_dir(&b)).map(Arc::new);
            let waiting = {
                let mut l = leaves().lock().unwrap_or_else(|e| e.into_inner());
                let w = match &leaf {
                    Ok(leaf) => l.insert(key.clone(), Leaf::Ready(leaf.clone())),
                    // A failed leaf is read again by the next tile that needs it.
                    Err(_) => l.remove(&key),
                };
                match w {
                    Some(Leaf::Loading(w)) => w,
                    _ => Vec::new(),
                }
            };
            for w in waiting {
                w(leaf.clone());
            }
        }),
    );
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

fn u64_at(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

fn i32_at(b: &[u8], i: usize) -> Option<f64> {
    Some(f64::from(i32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?)) / 1e7)
}

/// The header and root directory, and where the metadata is.
fn parse_head(b: &[u8]) -> Result<(Info, u64, u64), String> {
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
    };
    Ok((info, meta_at, meta_len.min(MAX_BYTES as u64)))
}

/// A directory: delta-coded tile ids, then run lengths, lengths and offsets, all varints.
fn parse_dir(b: &[u8]) -> Result<Vec<Entry>, String> {
    let mut r = Varints { b, at: 0 };
    let n = usize::try_from(r.next()?).map_err(|_| "a directory too large")?;
    // Each entry takes at least four bytes.
    if n > MAX_ENTRIES || n.saturating_mul(4) > b.len() {
        return Err("a directory with more entries than it can hold".into());
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
            (0, Some(p)) => p.offset.saturating_add(p.length),
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
        let dir = std::env::temp_dir().join(format!("rustgis-pmtiles-write-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let new = |tile_type| NewArchive {
            tile_type,
            gzipped_tiles: false,
            bounds: [-10.0, -5.0, 10.0, 5.0],
            metadata: serde_json::json!({"vector_layers": [{"id": "roads"}]}),
        };
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
        let dir = std::env::temp_dir().join(format!("rustgis-pmtiles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tiles = vec![(0u8, 0u32, 0u32, b"zero".to_vec()), (1, 1, 0, b"one-one-zero".to_vec()), (2, 3, 3, b"two".to_vec())];
        // Uncompressed, gzip, Brotli and Zstandard archives.
        for (leaf, code) in [false, true].into_iter().flat_map(|l| (1..=4).map(move |c| (l, c))) {
            let p = dir.join(format!("t{leaf}{code}.pmtiles"));
            std::fs::write(&p, archive_with(&tiles, 1, leaf, r#"{"vector_layers":[{"id":"roads"}],"attribution":"Test"}"#, code)).unwrap();
            let a = p.display().to_string();
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
