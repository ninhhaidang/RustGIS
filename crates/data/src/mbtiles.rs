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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock};

    use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

    use super::{Info, MAX_TILE_BYTES, flip};

    /// Open connections kept per file, for the tile readers to share.
    const POOL: usize = 4;
    /// Goes up when connections are closed, so one in use then isn't put back for reuse.
    static GENERATION: AtomicU64 = AtomicU64::new(0);

    fn pool() -> &'static Mutex<HashMap<PathBuf, Vec<Connection>>> {
        static P: OnceLock<Mutex<HashMap<PathBuf, Vec<Connection>>>> = OnceLock::new();
        P.get_or_init(Mutex::default)
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
        let generation = GENERATION.load(Ordering::SeqCst);
        let pooled = pool().lock().unwrap_or_else(|e| e.into_inner()).get_mut(path).and_then(Vec::pop);
        let conn = match pooled {
            Some(c) => c,
            None => open(path)?,
        };
        let r = f(&conn);
        let mut all = pool().lock().unwrap_or_else(|e| e.into_inner());
        // Closed while in use (the file changed, or the cache was cleared): not reused.
        if GENERATION.load(Ordering::SeqCst) != generation {
            return r;
        }
        // A few files are open at a time: past that many, start again rather than grow.
        if all.len() >= 16 && !all.contains_key(path) {
            all.clear();
        }
        let slot = all.entry(path.to_path_buf()).or_default();
        if slot.len() < POOL {
            slot.push(conn);
        }
        r
    }

    /// Forgets the open connections to `path` (it changed on disk, or was closed).
    pub fn close(path: &Path) {
        let mut all = pool().lock().unwrap_or_else(|e| e.into_inner());
        GENERATION.fetch_add(1, Ordering::SeqCst);
        all.remove(path);
    }

    /// Forgets every open connection.
    pub fn close_all() {
        let mut all = pool().lock().unwrap_or_else(|e| e.into_inner());
        GENERATION.fetch_add(1, Ordering::SeqCst);
        all.clear();
    }

    /// Forgets the open connections to the files `keep` says no to. Windows won't move or delete a
    /// file SQLite has open, so a file nothing reads any more mustn't stay in the pool.
    pub fn close_unless(keep: impl Fn(&Path) -> bool) {
        let mut all = pool().lock().unwrap_or_else(|e| e.into_inner());
        if !all.keys().all(|path| keep(path)) {
            GENERATION.fetch_add(1, Ordering::SeqCst);
            all.retain(|path, _| keep(path));
        }
    }

    /// Whether a connection to `path` is kept open.
    pub fn is_open(path: &Path) -> bool {
        pool().lock().unwrap_or_else(|e| e.into_inner()).get(path).is_some_and(|open| !open.is_empty())
    }

    /// The metadata of the MBTiles file at `path`.
    pub fn open_path(path: &Path) -> Result<Info, String> {
        with(path, |c| {
            let sql = |e: rusqlite::Error| format!("{}: not a readable MBTiles file ({e})", path.display());
            let mut meta = Vec::new();
            if let Ok(mut st) = c.prepare("SELECT name, value FROM metadata LIMIT 1000") {
                let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))).map_err(sql)?;
                meta.extend(rows.filter_map(Result::ok));
            }
            // That it is an MBTiles file: the tiles table (or view) has its columns. Nothing is read.
            c.prepare("SELECT zoom_level, tile_column, tile_row, tile_data FROM tiles LIMIT 0").map_err(sql)?;
            // What the metadata says, if it is usable: a zoom that reads as a number, a format
            // RustGIS knows.
            let says = |k: &str, ok: &dyn Fn(&str) -> bool| {
                meta.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).is_some_and(|(_, v)| ok(v.trim()))
            };
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
        })
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
                pool().lock().unwrap().entry(path.clone()).or_default().push(open(path).unwrap());
            }
            close_unless(|path| path != gone);
            assert!(is_open(&kept));
            assert!(!is_open(&gone));
            std::fs::remove_file(&gone).unwrap();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use file::{close, close_all, close_unless, is_open, open_path, tile_path};

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
