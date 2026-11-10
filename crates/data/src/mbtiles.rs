//! MBTiles: a tile set (raster pictures or Mapbox Vector Tiles) in an SQLite database, with its
//! metadata in a `metadata` table and its tiles in `tiles` (a table, or a view over deduplicated
//! tables), rows counted from the south (TMS).
//!
//! On the desktop a file is read in place through SQLite, a tile at a time. A file uploaded to the
//! browser is read whole by the pure-Rust reader ([`crate::sqlite`]) into an [`Archive`]: plain
//! `tiles` tables and the deduplicated layouts (`map` + `images`, Planetiler's `tiles_shallow` +
//! `tiles_data`) alike.
//!
//! Files are hostile input: tile counts and sizes are bounded, and metadata is checked before use.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::Arc;

use serde_json::Value;

use crate::sqlite::{Db, Table, Val};

/// The most tiles an archive read whole may hold.
pub const MAX_TILES: usize = 4_000_000;
/// The most bytes a single tile may have.
pub const MAX_TILE_BYTES: usize = 16 << 20;
/// The deepest zoom level read.
const MAX_ZOOM: u8 = 24;

/// What the tiles are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Webp,
    /// Mapbox Vector Tiles (often gzipped).
    Pbf,
    Unknown,
}

impl Format {
    pub fn is_vector(self) -> bool {
        self == Format::Pbf
    }

    fn from_name(s: &str) -> Format {
        match s.trim().to_ascii_lowercase().as_str() {
            "png" => Format::Png,
            "jpg" | "jpeg" => Format::Jpeg,
            "webp" => Format::Webp,
            "pbf" | "mvt" => Format::Pbf,
            _ => Format::Unknown,
        }
    }

    /// The format of a tile, from its first bytes.
    pub fn sniff(tile: &[u8]) -> Format {
        match tile {
            [0x89, b'P', b'N', b'G', ..] => Format::Png,
            [0xff, 0xd8, ..] => Format::Jpeg,
            [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Format::Webp,
            [] => Format::Unknown,
            // Gzipped or plain protocol buffers.
            _ => Format::Pbf,
        }
    }
}

/// An archive's metadata.
#[derive(Clone, Debug)]
pub struct Info {
    pub name: String,
    pub format: Format,
    pub min_zoom: u8,
    pub max_zoom: u8,
    /// Longitude/latitude bounds: west, south, east, north.
    pub bounds: [f64; 4],
    pub attribution: String,
    /// The `json` metadata entry (vector layers…), `Null` when there is none.
    pub json: Value,
    /// Its vector layers' names, as [`source_layers::of_metadata`](crate::source_layers::of_metadata) keeps them.
    vector_layers: Vec<String>,
}

impl Info {
    /// The names of the vector layers the metadata lists, as
    /// [`source_layers::of_metadata`](crate::source_layers::of_metadata) keeps them.
    pub fn vector_layers(&self) -> &[String] {
        &self.vector_layers
    }

    /// Reads the `metadata` table's (name, value) rows; `zooms` are the tiles' own zoom range and
    /// `first` one of the tiles, for what the metadata leaves out.
    fn from_metadata(rows: &[(String, String)], zooms: Option<(u8, u8)>, first: Option<&[u8]>) -> Info {
        let get = |k: &str| rows.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.trim());
        let zoom =
            |k: &str| get(k).and_then(|v| v.parse::<f64>().ok()).filter(|z| z.is_finite()).map(|z| z.clamp(0.0, f64::from(MAX_ZOOM)) as u8);
        let format = match get("format").map(Format::from_name) {
            Some(f) if f != Format::Unknown => f,
            _ => first.map(Format::sniff).unwrap_or(Format::Unknown),
        };
        let (lo, hi) = zooms.unwrap_or((0, MAX_ZOOM));
        let min_zoom = zoom("minzoom").unwrap_or(lo);
        let max_zoom = zoom("maxzoom").unwrap_or(hi).max(min_zoom);
        let bounds = get("bounds")
            .map(|b| b.split(',').map(|v| v.trim().parse::<f64>().ok().filter(|v| v.is_finite())).collect::<Option<Vec<f64>>>())
            .and_then(|v| match v.as_deref() {
                Some(&[w, s, e, n]) if w < e && s < n => Some([w.max(-180.0), s.max(-85.06), e.min(180.0), n.min(85.06)]),
                _ => None,
            })
            .unwrap_or([-180.0, -85.06, 180.0, 85.06]);
        let json = get("json").and_then(|j| serde_json::from_str(j).ok()).unwrap_or(Value::Null);
        Info {
            name: get("name").unwrap_or_default().chars().take(120).collect(),
            format,
            min_zoom,
            max_zoom,
            bounds,
            attribution: get("attribution").unwrap_or_default().chars().take(500).collect(),
            vector_layers: crate::source_layers::of_metadata(&json),
            json,
        }
    }
}

/// The TMS row of an XYZ tile row (and back).
fn flip(z: u8, y: u32) -> Option<u32> {
    let n = 1u64.checked_shl(u32::from(z))?;
    u32::try_from(n.checked_sub(1)?.checked_sub(u64::from(y))?).ok()
}

/// An archive read whole into memory (a browser upload).
pub struct Archive {
    pub info: Info,
    /// Tiles by zoom, column and XYZ row (counted from the north).
    tiles: HashMap<(u8, u32, u32), Arc<[u8]>>,
}

impl Archive {
    /// The tile at z/x/y (`y` from the north), if the archive has it.
    pub fn tile(&self, z: u8, x: u32, y: u32) -> Option<Arc<[u8]>> {
        self.tiles.get(&(z, x, y)).cloned()
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }
}

/// Whether a file name is an MBTiles archive.
pub fn is_mbtiles_name(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".mbtiles")
}

fn int(v: Option<&Val>) -> Option<i64> {
    match v? {
        Val::Int(i) => Some(*i),
        Val::Real(r) if r.fract() == 0.0 && r.is_finite() => Some(*r as i64),
        _ => None,
    }
}

/// A key joining a deduplicated layout's tiles to their data.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    Int(i64),
    Text(String),
    Blob(Vec<u8>),
}

fn key(v: Option<&Val>) -> Option<Key> {
    match v? {
        Val::Int(i) => Some(Key::Int(*i)),
        Val::Text(t) => Some(Key::Text(t.clone())),
        Val::Blob(b) => Some(Key::Blob(b.clone())),
        _ => None,
    }
}

fn col(t: &Table, name: &str) -> Option<usize> {
    t.columns.iter().position(|c| c.name.eq_ignore_ascii_case(name))
}

/// Reads an MBTiles file held in memory.
pub fn read_bytes(bytes: &[u8]) -> Result<Archive, String> {
    let db = Db::open(bytes)?;
    let tables = db.tables()?;
    let find = |n: &str| tables.iter().find(|t| t.name.eq_ignore_ascii_case(n));
    let mut meta = Vec::new();
    if let Some(m) = find("metadata")
        && let (Some(n), Some(v)) = (col(m, "name"), col(m, "value"))
    {
        db.rows(m, |_, r| {
            if let (Some(Val::Text(n)), Some(Val::Text(v))) = (r.get(n), r.get(v)) {
                meta.push((n.clone(), v.clone()));
            }
            if meta.len() >= 1000 { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        })?;
    }
    let mut tiles: HashMap<(u8, u32, u32), Arc<[u8]>> = HashMap::new();
    let mut err = None;
    let mut put = |z: Option<i64>, x: Option<i64>, row: Option<i64>, data: Arc<[u8]>| -> ControlFlow<()> {
        let (Some(z), Some(x), Some(row)) = (z, x, row) else { return ControlFlow::Continue(()) };
        let Ok(z) = u8::try_from(z) else { return ControlFlow::Continue(()) };
        if z > MAX_ZOOM || data.len() > MAX_TILE_BYTES {
            return ControlFlow::Continue(());
        }
        let (Ok(x), Ok(row)) = (u32::try_from(x), u32::try_from(row)) else { return ControlFlow::Continue(()) };
        let Some(y) = flip(z, row) else { return ControlFlow::Continue(()) };
        if u64::from(x) >= 1u64 << z {
            return ControlFlow::Continue(());
        }
        if tiles.len() >= MAX_TILES {
            err = Some(format!("more than {MAX_TILES} tiles"));
            return ControlFlow::Break(());
        }
        tiles.insert((z, x, y), data);
        ControlFlow::Continue(())
    };
    let plain = find("tiles").filter(|t| col(t, "tile_data").is_some());
    if let Some(t) = plain {
        let (cz, cx, cy, cd) = (col(t, "zoom_level"), col(t, "tile_column"), col(t, "tile_row"), col(t, "tile_data"));
        db.rows(t, |_, r| {
            let data: Arc<[u8]> = match cd.and_then(|i| r.get(i)) {
                Some(Val::Blob(b)) => b.as_slice().into(),
                _ => return ControlFlow::Continue(()),
            };
            put(int(cz.and_then(|i| r.get(i))), int(cx.and_then(|i| r.get(i))), int(cy.and_then(|i| r.get(i))), data)
        })?;
    } else {
        // A deduplicated layout: a table of tile positions with an id, and a table of tile data
        // by the same id (`map` + `images`, `tiles_shallow` + `tiles_data`…).
        let ids = ["tile_id", "tile_data_id"];
        let shallow = tables
            .iter()
            .find_map(|t| {
                let id = ids.iter().find_map(|n| col(t, n))?;
                (col(t, "zoom_level").is_some()
                    && col(t, "tile_column").is_some()
                    && col(t, "tile_row").is_some()
                    && col(t, "tile_data").is_none())
                .then_some((t, id))
            })
            .ok_or("no tiles table: not an MBTiles file")?;
        let id_name = shallow.0.columns.get(shallow.1).map(|c| c.name.clone()).unwrap_or_default();
        let data_table = tables
            .iter()
            .find(|t| t.name != shallow.0.name && col(t, "tile_data").is_some() && col(t, &id_name).is_some())
            .ok_or("no table of tile data: not an MBTiles file")?;
        let (di, dd) = (col(data_table, &id_name), col(data_table, "tile_data"));
        // Tiles too large to draw are left out here, not after they're held. (The blobs together
        // are no larger than the file: the reader visits each page once.)
        let mut blobs: HashMap<Key, Arc<[u8]>> = HashMap::new();
        let mut too_many = false;
        db.rows(data_table, |_, r| {
            if let (Some(k), Some(Val::Blob(b))) = (key(di.and_then(|i| r.get(i))), dd.and_then(|i| r.get(i)))
                && b.len() <= MAX_TILE_BYTES
            {
                blobs.insert(k, b.as_slice().into());
            }
            too_many = blobs.len() > MAX_TILES;
            if too_many { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        })?;
        if too_many {
            return Err(format!("more than {MAX_TILES} tiles"));
        }
        let t = shallow.0;
        let (cz, cx, cy, ci) = (col(t, "zoom_level"), col(t, "tile_column"), col(t, "tile_row"), Some(shallow.1));
        db.rows(t, |_, r| {
            let Some(data) = key(ci.and_then(|i| r.get(i))).and_then(|k| blobs.get(&k).cloned()) else { return ControlFlow::Continue(()) };
            put(int(cz.and_then(|i| r.get(i))), int(cx.and_then(|i| r.get(i))), int(cy.and_then(|i| r.get(i))), data)
        })?;
    }
    if let Some(e) = err {
        return Err(e);
    }
    let zooms = tiles.keys().map(|k| k.0).min().zip(tiles.keys().map(|k| k.0).max());
    let first = tiles.values().find(|t| !t.is_empty()).cloned();
    let info = Info::from_metadata(&meta, zooms, first.as_deref());
    Ok(Archive { info, tiles })
}

#[cfg(not(target_arch = "wasm32"))]
mod file {
    //! Files on disk, read through SQLite a tile at a time.

    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

    use super::{Info, MAX_TILE_BYTES, flip};

    /// Open connections kept per file, for the tile readers to share.
    const POOL: usize = 4;
    #[derive(Default)]
    struct Pool {
        connections: HashMap<PathBuf, Vec<Connection>>,
        epochs: HashMap<PathBuf, u64>,
        generation: u64,
    }

    fn pool() -> &'static Mutex<Pool> {
        static P: std::sync::LazyLock<Mutex<Pool>> = std::sync::LazyLock::new(Mutex::default);
        &P
    }

    fn open(path: &Path) -> Result<Connection, String> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        // The file is hostile: nothing it defines may call functions, and nothing writes.
        let _ = conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false);
        let _ = conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true);
        let _ = conn.execute_batch("PRAGMA cache_size = -16384; PRAGMA query_only = ON;");
        Ok(conn)
    }

    /// Runs `f` with a connection to `path`, from the pool or newly opened, and puts it back.
    fn with<T>(path: &Path, f: impl FnOnce(&Connection) -> Result<T, String>) -> Result<T, String> {
        let (generation, epoch, pooled) = {
            let mut all = pool().lock().unwrap_or_else(|e| e.into_inner());
            // Track checked-out files too: close_unless must invalidate a handle even while no
            // idle connection to its file remains. Bound both this record and the idle pool.
            if all.epochs.len() >= 16 && !all.epochs.contains_key(path) {
                all.generation = all.generation.wrapping_add(1);
                all.connections.clear();
                all.epochs.clear();
            }
            let epoch = *all.epochs.entry(path.to_path_buf()).or_default();
            (all.generation, epoch, all.connections.get_mut(path).and_then(Vec::pop))
        };
        let conn = match pooled {
            Some(c) => c,
            None => open(path)?,
        };
        let r = f(&conn);
        let mut all = pool().lock().unwrap_or_else(|e| e.into_inner());
        // Closed while in use (the file changed, or the cache was cleared): not reused.
        if all.generation != generation || all.epochs.get(path).copied().unwrap_or(0) != epoch {
            return r;
        }
        let slot = all.connections.entry(path.to_path_buf()).or_default();
        if slot.len() < POOL {
            slot.push(conn);
        }
        r
    }

    /// Forgets the open connections to `path` (it changed on disk, or was closed).
    pub fn close(path: &Path) {
        let mut all = pool().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(epoch) = all.epochs.get_mut(path) {
            *epoch = epoch.wrapping_add(1);
        }
        all.connections.remove(path);
    }

    /// Forgets every open connection.
    pub fn close_all() {
        let mut all = pool().lock().unwrap_or_else(|e| e.into_inner());
        all.generation = all.generation.wrapping_add(1);
        all.connections.clear();
        all.epochs.clear();
    }

    /// Forgets the open connections to the files `keep` says no to. Windows won't move or delete a
    /// file SQLite has open, so a file nothing reads any more mustn't stay in the pool.
    pub fn close_unless(keep: impl Fn(&Path) -> bool) {
        let mut all = pool().lock().unwrap_or_else(|e| e.into_inner());
        for (path, epoch) in &mut all.epochs {
            if !keep(path) {
                *epoch = epoch.wrapping_add(1);
            }
        }
        all.connections.retain(|path, _| keep(path));
    }

    /// Whether a connection to `path` is kept open.
    pub fn is_open(path: &Path) -> bool {
        pool().lock().unwrap_or_else(|e| e.into_inner()).connections.get(path).is_some_and(|open| !open.is_empty())
    }

    /// The metadata of the MBTiles file at `path`.
    pub fn open_path(path: &Path) -> Result<Info, String> {
        with(path, |c| read_info(c, path))
    }

    /// Reads metadata and one bounded raster sample on a fresh connection, retaining nothing.
    /// Vector archives get only a tile-page readability probe; an empty archive has no sample.
    /// The current progress probe and request predicate cancel SQLite scans on this connection.
    pub fn read_info_path(path: &Path) -> Result<(Info, Option<Vec<u8>>), String> {
        let still = crate::source::remote::current_still();
        let cancelled = move || crate::progress::check().is_err() || still.as_ref().is_some_and(|s| !s());
        if cancelled() {
            return Err(crate::DataError::Cancelled.to_string());
        }
        // Latch callback cancellation: SQLite's "interrupted" error is not archive damage,
        // even if a request predicate has changed again before the query unwinds.
        let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let result = (|| {
            let mut c = open(path)?;
            let poll = cancelled.clone();
            let stopped = interrupted.clone();
            c.progress_handler(
                1000,
                Some(move || {
                    let stop = poll();
                    if stop {
                        stopped.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    stop
                }),
            )
            .map_err(|e| format!("{}: cannot install MBTiles cancellation handler ({e})", path.display()))?;
            let info = read_info(&c, path)?;
            if cancelled() {
                return Err(crate::DataError::Cancelled.to_string());
            }
            let sql = |e: rusqlite::Error| format!("{}: not a readable MBTiles file ({e})", path.display());
            // Even complete metadata must not hide a damaged first tile page. Vector payloads are
            // not decoded here; the tiles layer decodes the raster sample before accepting a reload.
            let sample = if info.format.is_vector() {
                let _: Option<Option<Vec<u8>>> =
                    c.query_row("SELECT substr(tile_data, 1, 16) FROM tiles LIMIT 1", [], |row| row.get(0)).optional().map_err(sql)?;
                None
            } else {
                // Check the size before materializing a blob, as the normal tile reader does.
                let tile: Option<(i64, Option<Vec<u8>>)> = c
                    .query_row(
                        "SELECT length(tile_data), CASE WHEN length(tile_data) <= ?1 THEN tile_data END FROM tiles LIMIT 1",
                        [MAX_TILE_BYTES as i64],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(sql)?;
                match tile {
                    Some((len, _)) if len > MAX_TILE_BYTES as i64 => {
                        return Err(format!(
                            "{}: raster sample of {len} bytes exceeds the {MAX_TILE_BYTES}-byte tile limit",
                            path.display()
                        ));
                    }
                    Some((_, bytes)) => bytes,
                    None => None,
                }
            };
            Ok((info, sample))
        })();
        // Tiny queries may finish before the first SQLite callback; check success and error
        // boundaries too, including a failed open. No fresh handle or hook enters the pool.
        if interrupted.load(std::sync::atomic::Ordering::Relaxed) || cancelled() {
            Err(crate::DataError::Cancelled.to_string())
        } else {
            result
        }
    }

    fn read_info(c: &Connection, path: &Path) -> Result<Info, String> {
        let sql = |e: rusqlite::Error| format!("{}: not a readable MBTiles file ({e})", path.display());
        let mut meta = Vec::new();
        let has_metadata: bool = c
            .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'metadata' COLLATE NOCASE)", [], |r| r.get(0))
            .map_err(sql)?;
        if has_metadata {
            let mut st = c.prepare("SELECT name, value FROM metadata LIMIT 1001").map_err(sql)?;
            let mut rows = st.query([]).map_err(sql)?;
            while let Some(row) = rows.next().map_err(sql)? {
                if meta.len() == 1000 {
                    return Err(format!("{}: MBTiles metadata exceeds the 1000-row limit; refusing to omit metadata", path.display()));
                }
                meta.push((row.get::<_, String>(0).map_err(sql)?, row.get::<_, String>(1).map_err(sql)?));
            }
        }
        // That it is an MBTiles file: the tiles table (or view) has its columns. Nothing is read.
        c.prepare("SELECT zoom_level, tile_column, tile_row, tile_data FROM tiles LIMIT 0").map_err(sql)?;
        // What the metadata says, if it is usable: a zoom that reads as a number, a format
        // RustGIS knows.
        let says =
            |k: &str, ok: &dyn Fn(&str) -> bool| meta.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).is_some_and(|(_, v)| ok(v.trim()));
        let zoom = |v: &str| v.parse::<f64>().is_ok_and(f64::is_finite);
        let format = |v: &str| super::Format::from_name(v) != super::Format::Unknown;
        // The tiles are looked at only for what the metadata leaves out: finding the zooms can
        // scan the whole table (or join a deduplicated layout's view).
        let zooms = if says("minzoom", &zoom) && says("maxzoom", &zoom) {
            None
        } else {
            let z: (Option<i64>, Option<i64>) =
                c.query_row("SELECT min(zoom_level), max(zoom_level) FROM tiles", [], |r| Ok((r.get(0)?, r.get(1)?))).map_err(sql)?;
            match z {
                (Some(a), Some(b)) => Some((a.clamp(0, 24) as u8, b.clamp(0, 24) as u8)),
                _ => None,
            }
        };
        // A tile's first bytes say its format when the metadata doesn't.
        let first: Option<Vec<u8>> = if says("format", &format) {
            None
        } else {
            c.query_row("SELECT substr(tile_data, 1, 16) FROM tiles LIMIT 1", [], |r| r.get(0)).optional().map_err(sql)?
        };
        Ok(Info::from_metadata(&meta, zooms, first.as_deref()))
    }

    /// The tile at z/x/y (`y` from the north) of the file at `path`, `None` when it has none.
    pub fn tile_path(path: &Path, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>, String> {
        let Some(row) = flip(z, y) else { return Ok(None) };
        with(path, |c| {
            // The size first, so a huge blob is never read into memory.
            let t: Option<(i64, Option<Vec<u8>>)> = c
                .query_row(
                    "SELECT length(tile_data), CASE WHEN length(tile_data) <= ?4 THEN tile_data END FROM tiles
                     WHERE zoom_level = ?1 AND tile_column = ?2 AND tile_row = ?3",
                    params![z, x, row, MAX_TILE_BYTES as i64],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            match t {
                Some((len, _)) if len > MAX_TILE_BYTES as i64 => Err(format!("a tile of {len} bytes is too large")),
                Some((_, t)) => Ok(t),
                None => Ok(None),
            }
        })
    }

    #[cfg(test)]
    mod reload_tests {
        use super::*;

        struct CancelAfterPolls {
            polls: std::sync::atomic::AtomicUsize,
            after: usize,
        }

        impl CancelAfterPolls {
            fn new(after: usize) -> std::sync::Arc<Self> {
                std::sync::Arc::new(Self { polls: std::sync::atomic::AtomicUsize::new(0), after })
            }

            fn polls(&self) -> usize {
                self.polls.load(std::sync::atomic::Ordering::Relaxed)
            }
        }

        impl crate::progress::Probe for CancelAfterPolls {
            fn cancelled(&self) -> bool {
                self.polls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1 >= self.after
            }

            fn begin(&self, _: u64) {}

            fn advance(&self, _: u64) {}
        }

        #[test]
        fn candidate_missing_zoom_scan_cancels_inside_sql_and_retries_cleanly() {
            let path = std::env::temp_dir().join(format!("rustgis-mbtiles-cancel-scan-{}.mbtiles", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE metadata(name TEXT,value TEXT);
                 CREATE TABLE tiles(zoom_level INTEGER,tile_column INTEGER,tile_row INTEGER,tile_data BLOB);
                 INSERT INTO metadata VALUES('format','png'),('minzoom','2'),('maxzoom','7');
                 WITH RECURSIVE n(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i<1023)
                 INSERT INTO tiles SELECT 2+i%6,i,0,x'89504e470d0a1a0a' FROM n;",
            )
            .unwrap();
            drop(c);

            // The same small archive with complete zoom metadata needs only the entry and
            // completion checks. Cancelling at poll four therefore requires SQLite's hook.
            let complete = CancelAfterPolls::new(4);
            {
                let _installed = crate::progress::install(Some(complete.clone()));
                let (info, _) = read_info_path(&path).unwrap();
                assert_eq!((info.min_zoom, info.max_zoom), (2, 7));
            }
            assert!(complete.polls() < 4, "complete metadata unexpectedly ran a scan");
            let c = Connection::open(&path).unwrap();
            c.execute("DELETE FROM metadata WHERE name IN ('minzoom','maxzoom')", []).unwrap();
            drop(c);

            let scan = CancelAfterPolls::new(4);
            {
                let _installed = crate::progress::install(Some(scan.clone()));
                assert_eq!(read_info_path(&path).unwrap_err(), crate::DataError::Cancelled.to_string());
            }
            assert_eq!(scan.polls(), 4, "the SQL callback must stop at the first cancelled poll");
            assert!(!is_open(&path), "a cancelled fresh validator must not populate the pool");

            let clean = CancelAfterPolls::new(usize::MAX);
            {
                let _installed = crate::progress::install(Some(clean.clone()));
                let (info, sample) = read_info_path(&path).unwrap();
                assert_eq!((info.min_zoom, info.max_zoom, info.format), (2, 7, super::super::Format::Png));
                assert_eq!(sample.as_deref(), Some(b"\x89PNG\r\n\x1a\n".as_slice()));
            }
            assert!(clean.polls() > complete.polls() + 3, "the fixed scan must cross several SQLite callback intervals");
            assert!(!is_open(&path), "a successful fresh validator must not populate the pool");
            // A request revoked only during SQL must also latch cancellation, not relabel the
            // interruption as a damaged archive when its predicate is live again afterwards.
            let request_polls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let polled = request_polls.clone();
            crate::source::remote::with_still(
                std::sync::Arc::new(move || polled.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1 != 4),
                || assert_eq!(read_info_path(&path).unwrap_err(), crate::DataError::Cancelled.to_string()),
            );
            assert_eq!(request_polls.load(std::sync::atomic::Ordering::Relaxed), 4);
            assert!(!is_open(&path));
            std::fs::remove_file(path).unwrap();
        }

        #[test]
        fn candidate_already_cancelled_stops_before_opening_the_file() {
            let path = std::env::temp_dir().join(format!("rustgis-mbtiles-cancel-missing-{}.mbtiles", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let cancelled = CancelAfterPolls::new(1);
            {
                let _installed = crate::progress::install(Some(cancelled.clone()));
                assert_eq!(read_info_path(&path).unwrap_err(), crate::DataError::Cancelled.to_string());
            }
            assert_eq!(cancelled.polls(), 1);
            assert!(!is_open(&path));
            assert!(
                read_info_path(&path).unwrap_err().contains(&path.display().to_string()),
                "a clean retry must report the real missing-file error"
            );

            let during_open = CancelAfterPolls::new(2);
            {
                let _installed = crate::progress::install(Some(during_open.clone()));
                assert_eq!(read_info_path(&path).unwrap_err(), crate::DataError::Cancelled.to_string());
            }
            assert_eq!(during_open.polls(), 2, "a failed open still checks cancellation at its error boundary");
        }

        #[test]
        fn candidate_small_queries_check_cancellation_at_completion() {
            let path = std::env::temp_dir().join(format!("rustgis-mbtiles-cancel-small-{}.mbtiles", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE metadata(name TEXT,value TEXT);
                 CREATE TABLE tiles(zoom_level INTEGER,tile_column INTEGER,tile_row INTEGER,tile_data BLOB);
                 INSERT INTO metadata VALUES('format','pbf'),('minzoom','2'),('maxzoom','7');",
            )
            .unwrap();
            drop(c);
            // Neither short query reaches 1,000 VM instructions. Cancel after metadata and
            // after the sample query must still refuse to publish a successful candidate.
            for after in [2, 3] {
                let cancelled = CancelAfterPolls::new(after);
                let _installed = crate::progress::install(Some(cancelled));
                assert_eq!(read_info_path(&path).unwrap_err(), crate::DataError::Cancelled.to_string());
                assert!(!is_open(&path));
            }
            let (info, sample) = read_info_path(&path).unwrap();
            assert_eq!((info.min_zoom, info.max_zoom), (2, 7));
            assert!(sample.is_none());
            std::fs::remove_file(path).unwrap();
        }

        #[test]
        fn metadata_row_limit_is_explicit_and_retryable_for_both_readers() {
            let path = std::env::temp_dir().join(format!("rustgis-mbtiles-metadata-limit-{}.mbtiles", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE metadata(name TEXT,value TEXT);
                 CREATE TABLE tiles(zoom_level INTEGER,tile_column INTEGER,tile_row INTEGER,tile_data BLOB);
                 INSERT INTO metadata VALUES('format','png'),('minzoom','0'),('maxzoom','0');",
            )
            .unwrap();
            let tx = c.unchecked_transaction().unwrap();
            {
                let mut insert = tx.prepare("INSERT INTO metadata VALUES(?1,'unused')").unwrap();
                for n in 0..996 {
                    insert.execute([format!("unused-{n}")]).unwrap();
                }
            }
            tx.execute("INSERT INTO metadata VALUES('attribution','last allowed attribution')", []).unwrap();
            tx.commit().unwrap();

            let assert_exact_limit = || {
                let ordinary = open_path(&path).unwrap();
                let (candidate, sample) = read_info_path(&path).unwrap();
                for info in [&ordinary, &candidate] {
                    assert_eq!(info.format, super::super::Format::Png);
                    assert_eq!(info.attribution, "last allowed attribution", "the thousandth metadata row must survive");
                }
                assert!(sample.is_none(), "the fixture has no tiles");
            };
            assert_exact_limit();

            c.execute("INSERT INTO metadata VALUES('json',?1)", [r#"{"vector_layers":[{"id":"overflow"}]}"#]).unwrap();
            for error in [open_path(&path).unwrap_err(), read_info_path(&path).unwrap_err()] {
                assert!(error.contains(&path.display().to_string()), "{error}");
                assert!(error.contains("1000-row limit"), "{error}");
            }
            c.execute("DELETE FROM metadata WHERE name='json'", []).unwrap();
            assert_exact_limit();

            close(&path);
            drop(c);
            std::fs::remove_file(path).unwrap();
        }

        #[test]
        fn ordinary_open_refuses_null_and_non_text_metadata_rows() {
            let path = std::env::temp_dir().join(format!("rustgis-mbtiles-metadata-types-{}.mbtiles", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE metadata(name,value);
                 CREATE TABLE tiles(zoom_level INTEGER,tile_column INTEGER,tile_row INTEGER,tile_data BLOB);
                 INSERT INTO metadata VALUES('format','png'),('minzoom','0'),('maxzoom','0');",
            )
            .unwrap();
            assert_eq!(open_path(&path).unwrap().format, super::super::Format::Png);
            for row in ["(NULL,'unused')", "('unused',NULL)", "(7,'unused')", "('unused',7)", "(x'01','unused')", "('unused',x'01')"] {
                c.execute_batch(&format!("INSERT INTO metadata VALUES {row}")).unwrap();
                let error = open_path(&path).unwrap_err();
                assert!(error.contains(&path.display().to_string()), "{error}");
                assert!(error.contains("not a readable MBTiles file"), "{error}");
                c.execute("DELETE FROM metadata WHERE rowid = (SELECT max(rowid) FROM metadata)", []).unwrap();
                assert!(open_path(&path).is_ok(), "valid text rows must remain usable after {row}");
            }
            close(&path);
            drop(c);
            std::fs::remove_file(path).unwrap();
        }

        #[test]
        fn candidate_raster_sample_is_bounded_and_unpooled() {
            let path = std::env::temp_dir().join(format!("rustgis-mbtiles-raster-sample-{}.mbtiles", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE metadata(name TEXT,value TEXT);
                 CREATE TABLE tiles(zoom_level INTEGER,tile_column INTEGER,tile_row INTEGER,tile_data BLOB);
                 INSERT INTO metadata VALUES('format','png'),('minzoom','0'),('maxzoom','0');",
            )
            .unwrap();
            assert!(read_info_path(&path).unwrap().1.is_none(), "an empty archive remains valid");
            let invalid_png = b"\x89PNG\r\n\x1a\n";
            c.execute("INSERT INTO tiles VALUES(0,0,0,?1)", [invalid_png.as_slice()]).unwrap();
            let (info, sample) = read_info_path(&path).unwrap();
            assert_eq!(info.format, super::super::Format::Png);
            assert_eq!(sample.as_deref(), Some(invalid_png.as_slice()), "the tiles layer must see more than a discarded signature probe");
            c.execute("UPDATE tiles SET tile_data=zeroblob(?1)", [MAX_TILE_BYTES as i64 + 1]).unwrap();
            let error = read_info_path(&path).unwrap_err();
            assert!(error.contains(&path.display().to_string()), "{error}");
            assert!(error.contains("exceeds") && error.contains(&MAX_TILE_BYTES.to_string()), "{error}");
            c.execute_batch("UPDATE metadata SET value='pbf' WHERE name='format'; UPDATE tiles SET tile_data=x'01';").unwrap();
            assert!(read_info_path(&path).unwrap().1.is_none(), "vector payloads get a readability probe, not image validation");
            assert!(!pool().lock().unwrap().connections.contains_key(&path), "candidate validation must not populate the retained pool");
            drop(c);
            std::fs::remove_file(path).unwrap();
        }

        #[test]
        fn candidate_probe_reads_tile_pages_even_with_complete_metadata() {
            let path = std::env::temp_dir().join(format!("rustgis-mbtiles-damaged-tile-page-{}.mbtiles", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE metadata(name TEXT,value TEXT);
                 CREATE TABLE tiles(zoom_level INTEGER,tile_column INTEGER,tile_row INTEGER,tile_data BLOB);
                 INSERT INTO metadata VALUES('format','pbf'),('minzoom','0'),('maxzoom','0');
                 INSERT INTO tiles VALUES(0,0,0,x'01');",
            )
            .unwrap();
            let page = c.query_row("SELECT rootpage FROM sqlite_master WHERE name='tiles'", [], |r| r.get::<_, u32>(0)).unwrap() as usize;
            let size = c.query_row("PRAGMA page_size", [], |r| r.get::<_, u32>(0)).unwrap() as usize;
            drop(c);
            assert!(read_info_path(&path).is_ok());
            let mut bytes = std::fs::read(&path).unwrap();
            bytes[(page - 1) * size] = 0;
            std::fs::write(&path, bytes).unwrap();
            assert!(open_path(&path).is_ok(), "complete metadata alone does not step the damaged tile table");
            let error = read_info_path(&path).unwrap_err();
            assert!(error.contains(&path.display().to_string()), "{error}");
            assert!(error.contains("not a readable MBTiles file"), "{error}");
            close(&path);
            std::fs::remove_file(path).unwrap();
        }

        #[test]
        fn closing_one_file_rejects_its_old_connection_but_keeps_another_file() {
            let root = std::env::temp_dir().join(format!("rustgis-mbtiles-pool-reload-{}", std::process::id()));
            std::fs::create_dir_all(&root).unwrap();
            let path = root.join("reload.mbtiles");
            let other = root.join("other.mbtiles");
            for p in [&path, &other] {
                let c = Connection::open(p).unwrap();
                c.execute_batch(
                    "CREATE TABLE IF NOT EXISTS tiles(zoom_level INTEGER,tile_column INTEGER,tile_row INTEGER,tile_data BLOB);
                    DELETE FROM tiles; INSERT INTO tiles VALUES(0,0,0,x'01');",
                )
                .unwrap();
            }
            // Another test's new path can clear the process-global pool when 16 are tracked: the
            // other file is only expected to survive `close` if no clear happened around it.
            let generation = pool().lock().unwrap().generation;
            assert_eq!(tile_path(&other, 0, 0, 0).unwrap(), Some(vec![1]));
            let (started_tx, started_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let read_path = path.clone();
            let worker = std::thread::spawn(move || {
                with(&read_path, |c| {
                    let tile: Vec<u8> = c.query_row("SELECT tile_data FROM tiles LIMIT 1", [], |r| r.get(0)).map_err(|e| e.to_string())?;
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(tile)
                })
                .unwrap()
            });
            started_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
            close(&path);
            let mut kept = {
                let all = pool().lock().unwrap();
                (all.generation == generation).then(|| all.connections.get(&other).is_some_and(|v| !v.is_empty()))
            };
            for _ in 0..8 {
                if kept.is_some() {
                    break;
                }
                // The pool was cleared by a concurrent test: seed both files again and close
                // `path` once more, so the retry still shows `close` drops only its own file.
                let generation = pool().lock().unwrap().generation;
                assert_eq!(tile_path(&other, 0, 0, 0).unwrap(), Some(vec![1]));
                assert_eq!(tile_path(&path, 0, 0, 0).unwrap(), Some(vec![1]));
                close(&path);
                let all = pool().lock().unwrap();
                kept = (all.generation == generation).then(|| {
                    all.connections.get(&other).is_some_and(|v| !v.is_empty()) && all.connections.get(&path).is_none_or(Vec::is_empty)
                });
            }
            assert_eq!(kept, Some(true), "closing one file must keep another file's connection");
            release_tx.send(()).unwrap();
            assert_eq!(worker.join().unwrap(), vec![1]);
            assert!(
                pool().lock().unwrap().connections.get(&path).is_none_or(Vec::is_empty),
                "a read admitted before close returned its obsolete connection"
            );
            let c = Connection::open(&path).unwrap();
            c.execute_batch("UPDATE tiles SET tile_data=x'02'").unwrap();
            drop(c);
            assert_eq!(tile_path(&path, 0, 0, 0).unwrap(), Some(vec![2]));
            close(&path);
            close(&other);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Only the files `keep` says no to lose their connections, and one left alone stays open.
        #[test]
        fn close_unless_leaves_the_files_it_keeps() {
            struct Cleanup(PathBuf);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    // Also clean up if an assertion fails with the files still open.
                    close_unless(|path| !path.starts_with(&self.0));
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
            let dir = std::env::temp_dir().join(format!("rustgis-mbtiles-close-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let _cleanup = Cleanup(dir.clone());
            let (kept, gone) = (dir.join("kept.mbtiles"), dir.join("gone.mbtiles"));
            for path in [&kept, &gone] {
                std::fs::write(path, b"").unwrap();
                with(path, |_| Ok(())).unwrap();
            }
            with(&kept, |_| {
                with(&gone, |_| {
                    close_unless(|path| path != gone);
                    Ok(())
                })
            })
            .unwrap();
            assert!(is_open(&kept));
            assert!(!is_open(&gone));
            std::fs::remove_file(&gone).unwrap();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use file::{close, close_all, close_unless, is_open, open_path, read_info_path, tile_path};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_flip_and_formats_sniff() {
        assert_eq!(flip(0, 0), Some(0));
        assert_eq!(flip(3, 0), Some(7));
        assert_eq!(flip(3, 8), None);
        assert_eq!(flip(40, 0), None);
        assert_eq!(Format::sniff(b"\x89PNG\r\n"), Format::Png);
        assert_eq!(Format::sniff(&[0xff, 0xd8, 0xff]), Format::Jpeg);
        assert_eq!(Format::sniff(b"RIFF\0\0\0\0WEBPVP8"), Format::Webp);
        assert_eq!(Format::sniff(&[0x1f, 0x8b]), Format::Pbf);
    }

    #[test]
    fn metadata_is_checked() {
        let m = |rows: &[(&str, &str)]| rows.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        let i = Info::from_metadata(
            &m(&[
                ("format", "PBF"),
                ("minzoom", "2"),
                ("maxzoom", "99"),
                ("bounds", "-200,-10,10,95"),
                ("json", "{\"vector_layers\":[{\"id\":\"roads\"}]}"),
            ]),
            None,
            None,
        );
        assert_eq!((i.format, i.min_zoom, i.max_zoom), (Format::Pbf, 2, 24));
        assert_eq!(i.bounds, [-180.0, -10.0, 10.0, 85.06]);
        assert_eq!(i.vector_layers(), ["roads"]);
        // Missing or junk: the tiles' own zooms and format, the whole world.
        let i = Info::from_metadata(&m(&[("bounds", "1,2,x,4"), ("minzoom", "NaN"), ("json", "{")]), Some((3, 5)), Some(b"\x89PNG"));
        assert_eq!((i.format, i.min_zoom, i.max_zoom), (Format::Png, 3, 5));
        assert_eq!(i.bounds[0], -180.0);
        assert!(i.json.is_null());
    }

    /// Metadata that can't be used (zooms that aren't numbers, an unknown format) is made up from
    /// the tiles, as missing metadata is.
    #[test]
    fn junk_metadata_falls_back_to_the_tiles() {
        let path = std::env::temp_dir().join(format!("rustgis-mbtiles-junk-{}.mbtiles", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let c = rusqlite::Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE metadata (name text, value text); CREATE TABLE tiles (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_data BLOB);
                 INSERT INTO metadata VALUES ('minzoom', 'abc'), ('maxzoom', ''), ('format', 'tiff-ish');",
            )
            .unwrap();
            c.execute("INSERT INTO tiles VALUES (2, 0, 0, ?1), (3, 1, 1, ?1)", [b"\x89PNG\r\n\x1a\n".to_vec()]).unwrap();
        }
        let info = open_path(&path).unwrap();
        assert_eq!((info.min_zoom, info.max_zoom, info.format), (2, 3, Format::Png));
        close(&path);
        let _ = std::fs::remove_file(&path);
    }

    /// Plain and deduplicated layouts written by SQLite read the same through both readers.
    #[test]
    fn reads_layouts_both_ways() {
        let dir = std::env::temp_dir().join(format!("rustgis-mbtiles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = |i: u8| -> Vec<u8> { [b"\x89PNG\r\n\x1a\n".as_slice(), &[i; 40]].concat() };
        let layouts = [
            ("plain", "CREATE TABLE tiles (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_data BLOB);"),
            (
                "dedup",
                "CREATE TABLE map (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_id TEXT);
                 CREATE TABLE images (tile_data blob, tile_id text);
                 CREATE VIEW tiles AS SELECT map.zoom_level AS zoom_level, map.tile_column AS tile_column, map.tile_row AS tile_row,
                   images.tile_data AS tile_data FROM map JOIN images ON images.tile_id = map.tile_id;",
            ),
            (
                "shallow",
                "CREATE TABLE tiles_shallow (zoom_level integer, tile_column integer, tile_row integer, tile_data_id integer,
                   primary key(zoom_level, tile_column, tile_row)) without rowid;
                 CREATE TABLE tiles_data (tile_data_id integer primary key, tile_data blob);
                 CREATE VIEW tiles AS SELECT s.zoom_level AS zoom_level, s.tile_column AS tile_column, s.tile_row AS tile_row,
                   d.tile_data AS tile_data FROM tiles_shallow s JOIN tiles_data d ON s.tile_data_id = d.tile_data_id;",
            ),
        ];
        for (name, schema) in layouts {
            let path = dir.join(format!("{name}.mbtiles"));
            let _ = std::fs::remove_file(&path);
            {
                let c = rusqlite::Connection::open(&path).unwrap();
                c.execute_batch(&format!("CREATE TABLE metadata (name text, value text); {schema}")).unwrap();
                c.execute_batch("INSERT INTO metadata VALUES ('name', 'Test'), ('bounds', '-10,-5,10,5'), ('attribution', '© me');")
                    .unwrap();
                let tx = c.unchecked_transaction().unwrap();
                // Zoom 0 to 3, every tile; the picture is shared by tiles with the same (x + y) % 3.
                for z in 0..4u32 {
                    for x in 0..(1u32 << z) {
                        for row in 0..(1u32 << z) {
                            let shared = ((x + row) % 3) as u8;
                            match name {
                                "plain" => {
                                    tx.execute("INSERT INTO tiles VALUES (?1, ?2, ?3, ?4)", rusqlite::params![z, x, row, png(shared)])
                                }
                                "dedup" => tx
                                    .execute("INSERT INTO map VALUES (?1, ?2, ?3, ?4)", rusqlite::params![z, x, row, format!("h{shared}")]),
                                _ => tx.execute("INSERT INTO tiles_shallow VALUES (?1, ?2, ?3, ?4)", rusqlite::params![z, x, row, shared]),
                            }
                            .unwrap();
                        }
                    }
                }
                for k in 0..3u8 {
                    match name {
                        "dedup" => tx.execute("INSERT INTO images VALUES (?1, ?2)", rusqlite::params![png(k), format!("h{k}")]).map(|_| ()),
                        "shallow" => tx.execute("INSERT INTO tiles_data VALUES (?1, ?2)", rusqlite::params![k, png(k)]).map(|_| ()),
                        _ => Ok(()),
                    }
                    .unwrap();
                }
                tx.commit().unwrap();
            }
            let info = open_path(&path).unwrap();
            assert_eq!((info.format, info.min_zoom, info.max_zoom, info.name.as_str()), (Format::Png, 0, 3, "Test"), "{name}");
            assert_eq!(info.bounds, [-10.0, -5.0, 10.0, 5.0]);
            let bytes = std::fs::read(&path).unwrap();
            let a = read_bytes(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(a.len(), 1 + 4 + 16 + 64, "{name}");
            assert_eq!((a.info.format, a.info.min_zoom, a.info.max_zoom), (Format::Png, 0, 3));
            assert_eq!(a.info.attribution, "© me");
            // XYZ row 0 at zoom 2 is TMS row 3.
            for (z, x, y) in [(0u8, 0u32, 0u32), (2, 1, 0), (3, 7, 2)] {
                let row = flip(z, y).unwrap();
                let want = png(((x + row) % 3) as u8);
                assert_eq!(tile_path(&path, z, x, y).unwrap().as_deref(), Some(want.as_slice()), "{name} {z}/{x}/{y}");
                assert_eq!(a.tile(z, x, y).as_deref(), Some(want.as_slice()), "{name} {z}/{x}/{y}");
            }
            assert_eq!(tile_path(&path, 5, 0, 0).unwrap(), None);
            assert!(a.tile(5, 0, 0).is_none());
            for cut in (0..bytes.len()).step_by(97) {
                let _ = read_bytes(&bytes[..cut]);
            }
            close(&path);
            let _ = std::fs::remove_file(&path);
        }
        assert!(read_bytes(b"not sqlite").is_err());
        assert!(open_path(&dir.join("missing.mbtiles")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reads every `.mbtiles` file in `RUSTGIS_SAMPLE_MBTILES` both ways and compares them:
    /// `RUSTGIS_SAMPLE_MBTILES=<dir> cargo test --release -p rustgis-data -- --ignored mbtiles_samples --nocapture`.
    #[test]
    #[ignore = "needs sample files"]
    fn mbtiles_samples() {
        let dir = std::env::var("RUSTGIS_SAMPLE_MBTILES").unwrap_or_else(|_| panic!("set RUSTGIS_SAMPLE_MBTILES"));
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if !is_mbtiles_name(&path.to_string_lossy()) {
                continue;
            }
            let t = std::time::Instant::now();
            let info = open_path(&path).unwrap_or_else(|e| panic!("{e}"));
            let opened = t.elapsed();
            let bytes = std::fs::read(&path).unwrap();
            let t = std::time::Instant::now();
            let a = read_bytes(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let whole = t.elapsed();
            // Every tile the in-memory reader found reads the same through SQLite.
            let t = std::time::Instant::now();
            for (&(z, x, y), data) in &a.tiles {
                assert_eq!(tile_path(&path, z, x, y).unwrap().as_deref(), Some(&data[..]), "{} {z}/{x}/{y}", path.display());
            }
            let per_tile = t.elapsed() / a.len().max(1) as u32;
            assert_eq!((info.format, info.min_zoom, info.max_zoom), (a.info.format, a.info.min_zoom, a.info.max_zoom));
            println!(
                "{}: {} bytes, {:?} z{}–{}, {} tiles; metadata {opened:?}, whole file {whole:?}, {per_tile:?} a tile",
                path.display(),
                bytes.len(),
                info.format,
                info.min_zoom,
                info.max_zoom,
                a.len()
            );
        }
    }
}
