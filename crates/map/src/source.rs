//! Streamed layers: a vector source (GeoPackage, GeoParquet, FlatGeobuf) read in place, a view's
//! worth at a time.
//!
//! The layer's `dataset.features` hold a window onto the source: the features in the last view
//! read, padded so small pans don't read again, at most [`VIEW_LIMIT`] of them (an evenly spread
//! sample when more match). Reads run on a background thread on the desktop, so the map stays
//! responsive; in the browser they run in place. The window isn't saved with the project: the
//! source is opened and read again.
//!
//! A FlatGeobuf or GeoParquet by its web address is read in place too, by range: opening it and
//! each read fetch what they need off the frame (desktop and browser alike), the read landing
//! when its bytes have.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rustgis_data::source::{Batch, FeatureSource, Request, SourceInfo};
use rustgis_geom::Extent;
use serde::{Deserialize, Serialize};

mod render_lod;
mod window;

/// Retained streamed rows and geometry caches per map; excludes scratch and render/export copies.
pub const VIEW_BYTES: usize = if cfg!(target_arch = "wasm32") { 48 * 1024 * 1024 } else { 128 * 1024 * 1024 };

/// Sources with more features than this are streamed when added; smaller ones are loaded whole.
pub const STREAM_ABOVE: u64 = 100_000;

/// The most features a view reads.
pub const VIEW_LIMIT: usize = if cfg!(target_arch = "wasm32") { 100_000 } else { 250_000 };

/// The most features a view reads from a file by its web address (each one is fetched).
pub const REMOTE_VIEW_LIMIT: usize = if cfg!(target_arch = "wasm32") { 30_000 } else { 60_000 };

/// A streamed layer's source.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SourceLayer {
    /// The file it reads; reopened when the project loads.
    #[serde(default)]
    pub path: Option<String>,
    /// The file's own name (for its format) when it is embedded.
    #[serde(default)]
    pub file_name: Option<String>,
    /// The GeoPackage table.
    #[serde(default)]
    pub table: Option<String>,
    /// A Shapefile's source CRS override, reapplied when its path is reopened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shapefile_crs: Option<rustgis_data::raster::Crs>,
    /// The original file, kept in the project when there is no path (a browser upload).
    #[serde(default, skip_serializing_if = "Option::is_none", with = "crate::raster::b64")]
    pub embedded: Option<Arc<Vec<u8>>>,
    #[serde(skip)]
    handle: Option<Arc<dyn FeatureSource>>,
    #[serde(skip)]
    window: Option<Window>,
    /// Whole-source thinning decisions shared by render snapshots, independent of view rows.
    #[serde(skip)]
    render_lod: Arc<Mutex<render_lod::Decisions>>,
    #[serde(skip)]
    pending: Option<Arc<Pending>>,
    /// Projection used for this disposable window's geometry cache.
    #[serde(skip)]
    pub(crate) projection: Option<crate::Projection>,
    #[serde(skip)]
    pub(crate) ready_cache: Option<window::Cache>,
    /// Allocated row/projection/display bytes; zero selects the standalone default.
    #[serde(skip)]
    pub byte_budget: usize,
    #[serde(skip)]
    estimated_row_bytes: usize,
    /// Blocking render/export reads preserve the original row limit rather than the interaction budget.
    #[serde(skip)]
    pub(crate) full_read: bool,
    /// IDs parked only while budget/projection eviction leaves this same source without rows.
    /// Hidden layers retain them until the next read; success consumes them, failure or an
    /// explicit source reset discards them. Display-only resets preserve them in `sync_sources`.
    #[serde(skip)]
    pub(crate) saved_selection: Vec<u64>,
    #[serde(skip)]
    pub(crate) window_fields: Option<usize>,
    /// A source by its web address being opened (its header or footer being fetched).
    #[serde(skip)]
    opening: Option<Opening>,
    /// Why the source couldn't be opened or read.
    #[serde(skip)]
    pub error: Option<String>,
    /// The last open failed because the server refused the login (no password given this session, or a wrong one).
    #[serde(skip)]
    pub login: bool,
    /// The most features a view reads (0 = [`VIEW_LIMIT`]).
    #[serde(skip)]
    pub limit: usize,
    /// Reads installed so far: the features changing with it came from the source, not an edit.
    #[serde(skip)]
    pub reads: u64,
    /// For a layer read through GDAL: the original file, converted again into the cache when the
    /// converted copy is gone (desktop only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gdal: Option<GdalOrigin>,
    /// For a layer from a DuckDB query or database table: a table is converted again when the
    /// result in the cache is gone (desktop only); a query is not run again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duckdb: Option<DuckdbOrigin>,
    /// For a layer read live from a PostGIS table (desktop only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postgis: Option<PostgisOrigin>,
    /// For a layer read live from a MySQL or MariaDB table (desktop only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mysql: Option<MysqlOrigin>,
    /// For a layer read live from a SQL Server table (desktop only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sqlserver: Option<SqlServerOrigin>,
}

/// A PostGIS table a layer reads live.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PostgisOrigin {
    /// The connection (`postgres:host=… dbname=…` or a `postgresql://` address), never with its
    /// password.
    pub database: String,
    /// The table, as `table`, `schema.table`, with `:column` when it has several.
    pub table: String,
    /// The geometry family read, for a column of mixed types.
    #[serde(default)]
    pub family: Option<rustgis_data::source::Family>,
    /// The coordinate system given for a geometry that doesn't say.
    #[serde(default)]
    pub crs: Option<String>,
}

/// A PostGIS table a layer was loaded from whole to be edited: what saving its edits writes to
/// (desktop only). The layer's feature ids are the table's key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PostgisEdit {
    /// The connection (never with its password), the table and the family loaded.
    pub origin: PostgisOrigin,
    /// The table's integer primary key column.
    pub key: String,
    /// The SRID of the table's geometry.
    pub srid: i32,
    /// Whether the user it was loaded as may insert, update and delete rows.
    pub writable: bool,
    /// The SQL condition the rows were loaded with, if any (loaded again with it when edits
    /// are discarded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// The most rows loaded.
    #[serde(default)]
    pub limit: usize,
    /// Each loaded row's version and what it held, by its key, as read: saving checks the
    /// database against them, so a row changed or deleted by someone else since is a conflict,
    /// not overwritten.
    #[serde(default)]
    pub versions: Versions,
}

/// A GeoPackage feature table loaded whole for desktop editing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GpkgEdit {
    /// Absolute when loaded; relative to a project saved beside it.
    pub path: String,
    pub table: String,
    /// Its INTEGER PRIMARY KEY column.
    pub key: String,
    pub geometry: String,
    /// The source SRS when these rows were loaded.
    pub srs_id: i64,
    /// The declared geometry type (uppercase) and Z/M flags when these rows were loaded.
    /// Missing in a project means unknown: saving refuses until Discard Edits reloads them.
    #[serde(default)]
    pub geometry_type: String,
    #[serde(default)]
    pub z: i64,
    #[serde(default)]
    pub m: i64,
    /// The table's durable revision, checked before saving pending inserts.
    pub revision: String,
    pub writable: bool,
    #[serde(default)]
    pub limit: usize,
    /// Every column's stored fingerprint and the loaded feature's hash, by key.
    pub versions: Versions,
}

/// How a loaded row is told apart from the same row edited: a hash of its geometry and
/// attributes (FNV-1a over their JSON, the same in every build).
pub fn row_hash(f: &rustgis_data::Feature) -> u64 {
    /// FNV-1a over the bytes written to it, kept as they come (no buffer per row).
    struct Fnv(u64);
    impl std::io::Write for Fnv {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 = buf.iter().fold(self.0, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3));
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    // As before, a row that can't be written hashes as no bytes.
    if serde_json::to_writer(&mut h, &(&f.geometry, &f.attrs)).is_err() {
        return 0xcbf2_9ce4_8422_2325;
    }
    h.0
}

/// A loaded row: its source version or fingerprint and [`row_hash`] as read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowVersion {
    pub version: i64,
    pub hash: u64,
}

/// Loaded rows by key, shared until changed like a dataset's features: an undo snapshot copies
/// a pointer, not the map.
#[derive(Clone, Default, PartialEq)]
pub struct Versions(Arc<std::collections::BTreeMap<u64, RowVersion>>);

impl Versions {
    /// The row with key `id`, if it was loaded.
    pub fn get(&self, id: u64) -> Option<RowVersion> {
        self.0.get(&id).copied()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether two are the same rows in memory (one cloned from the other).
    pub fn shares(&self, other: &Versions) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Whether anything but this holds the same rows (a layer, an undo step).
    pub fn held_elsewhere(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }

    /// The keys and rows, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (u64, RowVersion)> + '_ {
        self.0.iter().map(|(k, v)| (*k, *v))
    }
}

impl FromIterator<(u64, RowVersion)> for Versions {
    fn from_iter<I: IntoIterator<Item = (u64, RowVersion)>>(it: I) -> Versions {
        Versions(Arc::new(it.into_iter().collect()))
    }
}

impl std::fmt::Debug for Versions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Versions({} rows)", self.0.len())
    }
}

/// Saved as `[[key, version, hash], …]` (JSON object keys would be text).
impl Serialize for Versions {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(|(k, r)| (k, r.version, r.hash)))
    }
}

impl<'de> Deserialize<'de> for Versions {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Versions, D::Error> {
        Vec::<(u64, i64, u64)>::deserialize(d).map(|v| v.into_iter().map(|(k, version, hash)| (k, RowVersion { version, hash })).collect())
    }
}

/// What changed in a source layer since it was loaded (or last saved), by
/// comparing its features with the rows as read.
#[derive(Debug, Default)]
pub struct RowChanges {
    /// Features of loaded rows that differ from them, with the rows' versions.
    pub updates: Vec<(rustgis_data::Feature, i64)>,
    /// Loaded rows no feature holds any more: their keys and versions.
    pub deletes: Vec<(u64, i64)>,
    /// Features of no loaded row.
    pub inserts: Vec<rustgis_data::Feature>,
    /// Where each of `inserts` is among the features.
    pub insert_rows: Vec<usize>,
}

impl RowChanges {
    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.deletes.is_empty() && self.inserts.is_empty()
    }
}

impl Versions {
    /// What changed in `features` since the rows were read. A feature whose id is a loaded row's
    /// is that row (once: a copy with the same id is new).
    pub fn changes(&self, features: &[rustgis_data::Feature]) -> RowChanges {
        let mut out = RowChanges::default();
        let mut seen = std::collections::HashSet::new();
        for (i, f) in features.iter().enumerate() {
            match self.get(f.id) {
                Some(row) if seen.insert(f.id) => {
                    if row_hash(f) != row.hash {
                        out.updates.push((f.clone(), row.version));
                    }
                }
                _ => {
                    out.inserts.push(f.clone());
                    out.insert_rows.push(i);
                }
            }
        }
        out.deletes = self.iter().filter(|(k, _)| !seen.contains(k)).map(|(k, r)| (k, r.version)).collect();
        out
    }
}

/// The loaded rows' new baseline after a save or load.
pub fn versions_of(features: &[rustgis_data::Feature], old: &Versions, saved: &std::collections::HashMap<u64, i64>) -> Versions {
    let mut seen = std::collections::HashSet::new();
    features
        .iter()
        .filter_map(|f| {
            let version = saved.get(&f.id).copied().or_else(|| old.get(f.id).map(|r| r.version))?;
            seen.insert(f.id).then(|| (f.id, RowVersion { version, hash: row_hash(f) }))
        })
        .collect()
}

/// A MySQL or MariaDB table a layer reads live.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MysqlOrigin {
    /// The connection (a `mysql://` or `mariadb://` address), never with its password.
    pub database: String,
    /// The table, as `table`, `database.table`, with `:column` when it has several.
    pub table: String,
    /// The geometry family read, for a column of mixed types.
    #[serde(default)]
    pub family: Option<rustgis_data::source::Family>,
    /// The coordinate system given for a geometry whose SRID is 0.
    #[serde(default)]
    pub crs: Option<String>,
}

/// A SQL Server table a layer reads live.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SqlServerOrigin {
    /// The connection (an `mssql://` address), never with its password.
    pub database: String,
    /// The table, as `table`, `schema.table`, with `:column` when it has several.
    pub table: String,
    /// The geometry family read, for a column of mixed types.
    #[serde(default)]
    pub family: Option<rustgis_data::source::Family>,
    /// The coordinate system given for a geometry whose SRID is 0.
    #[serde(default)]
    pub crs: Option<String>,
}

/// Where a layer from DuckDB came from: a query, or a table of a database.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DuckdbOrigin {
    /// The query (for a table, none).
    #[serde(default)]
    pub sql: Option<String>,
    /// The database opened for the query, or holding the table.
    #[serde(default)]
    pub database: Option<String>,
    /// The table, as `table` or `schema.table`.
    #[serde(default)]
    pub table: Option<String>,
    /// The coordinate system given for a geometry that doesn't say.
    #[serde(default)]
    pub crs: Option<String>,
    /// The geometry family of the result this layer shows.
    pub family: rustgis_data::source::Family,
}

/// Where a layer read through GDAL came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GdalOrigin {
    pub path: String,
    pub layer: String,
    #[serde(default)]
    pub family: Option<rustgis_data::source::Family>,
    /// The coordinate system given for a file that doesn't say.
    #[serde(default)]
    pub crs: Option<String>,
}

impl std::fmt::Debug for SourceLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceLayer")
            .field("path", &self.path)
            .field("file_name", &self.file_name)
            .field("table", &self.table)
            .field("window", &self.window)
            .field("error", &self.error)
            .field("login", &self.login)
            .finish_non_exhaustive()
    }
}

/// Converts a DuckDB layer's table again: the result's part of the layer's geometry family.
/// A query is never run again here: a project file is hostile input, and a query can read any file
/// and send it to any address. Its user runs it again.
#[cfg(not(target_arch = "wasm32"))]
fn rerun(d: &DuckdbOrigin) -> rustgis_data::Result<std::path::PathBuf> {
    use rustgis_data::source::duckdb;
    let out = match (&d.table, d.database.as_deref().map(duckdb::Database::parse)) {
        // A project names its server, so it never connects on its own: it would send the user's
        // own PostgreSQL credentials (PGPASSWORD, ~/.pgpass) to whatever host the file says.
        (Some(_), Some(duckdb::Database::Postgres(_))) => {
            return Err(rustgis_data::DataError::Format(
                "the table's result is no longer in the cache: add it again (Home › Add › Add Data › Database…)".into(),
            ));
        }
        (Some(t), Some(db)) => duckdb::convert_table_of(&db, &duckdb::find_table_of(&db, t)?, d.crs.as_deref())?,
        _ => {
            return Err(rustgis_data::DataError::Format(
                "the query's result is no longer in the cache: run the query again (Add Data From SQL)".into(),
            ));
        }
    };
    out.parts
        .into_iter()
        .find(|p| p.family == d.family)
        .map(|p| p.path)
        .ok_or_else(|| rustgis_data::DataError::Format(format!("the query returns no {} any more", d.family.label())))
}

/// Adds the manual reconnect hint without losing a refused-password classification.
#[cfg(not(target_arch = "wasm32"))]
fn database_open_error(e: rustgis_data::DataError) -> rustgis_data::DataError {
    match e {
        rustgis_data::DataError::Login(message) => {
            rustgis_data::DataError::Login(format!("{message} (connect again with Home › Add › Add Data › Database…, giving its password)"))
        }
        e => rustgis_data::DataError::Format(format!("{e} (connect again with Home › Add › Add Data › Database…, giving its password)")),
    }
}

/// Connects to a PostGIS layer's table again (a project opening). Only a password given this
/// session is sent: a project file names its server, which must never get the user's own
/// credentials (`PGPASSWORD`, the password file) just for being opened.
fn open_postgis(pg: &PostgisOrigin) -> rustgis_data::Result<Box<dyn FeatureSource>> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = pg;
        Err(rustgis_data::DataError::Format("PostGIS databases are read in the desktop app".into()))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::{duckdb::Database, postgis};
        let Database::Postgres(dsn) = Database::parse(&pg.database) else {
            return Err(rustgis_data::DataError::Format(format!("{}: not a PostgreSQL connection", pg.database)));
        };
        let conn = postgis::connect(&dsn, false).map_err(database_open_error)?;
        let table = postgis::find_table(&conn, &pg.table)?;
        Ok(Box::new(postgis::PostgisSource::open(conn, table, pg.family, pg.crs.as_deref())?))
    }
}

/// Connects to a MySQL or MariaDB layer's table again (a project opening). Only a password given
/// this session is sent, never `MYSQL_PWD`, as for [`open_postgis`].
fn open_mysql(my: &MysqlOrigin) -> rustgis_data::Result<Box<dyn FeatureSource>> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = my;
        Err(rustgis_data::DataError::Format("MySQL and MariaDB databases are read in the desktop app".into()))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::{duckdb::Database, mysql};
        let Database::Mysql(address) = Database::parse(&my.database) else {
            return Err(rustgis_data::DataError::Format(format!("{}: not a MySQL or MariaDB connection", my.database)));
        };
        let conn = mysql::connect(&address, false).map_err(database_open_error)?;
        let table = mysql::find_table(&conn, &my.table)?;
        Ok(Box::new(mysql::MysqlSource::open(conn, table, my.family, my.crs.as_deref())?))
    }
}

/// Connects to a SQL Server layer's table again (a project opening). Only a password given
/// this session is sent, never `SQLCMDPASSWORD`, as for [`open_postgis`].
fn open_sqlserver(ms: &SqlServerOrigin) -> rustgis_data::Result<Box<dyn FeatureSource>> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = ms;
        Err(rustgis_data::DataError::Format("SQL Server databases are read in the desktop app".into()))
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use rustgis_data::source::{duckdb::Database, sqlserver};
        let Database::Sqlserver(address) = Database::parse(&ms.database) else {
            return Err(rustgis_data::DataError::Format(format!("{}: not a SQL Server connection", ms.database)));
        };
        let conn = sqlserver::connect(&address, false).map_err(database_open_error)?;
        let table = sqlserver::find_table(&conn, &ms.table)?;
        Ok(Box::new(sqlserver::SqlServerSource::open(conn, table, ms.family, ms.crs.as_deref())?))
    }
}

/// What the layer's features hold.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Window {
    /// The extent read (longitude/latitude).
    pub extent: Extent,
    /// Whether every feature in it was read.
    pub complete: bool,
    /// How many features matched it.
    pub matched: u64,
    /// How many were read.
    pub kept: usize,
}

/// A read in flight.
struct Pending {
    extent: Extent,
    result: Mutex<Option<Result<(Batch, window::Cache), String>>>,
}

/// A source being opened off the frame.
type Opening = Arc<Mutex<Option<Result<Arc<dyn FeatureSource>, String>>>>;

/// Tells the app something landed (a read, or a source opened by its address).
pub(crate) fn landed() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
    if let Some(w) = waker().lock().ok().and_then(|w| w.clone()) {
        w();
    }
}

/// What [`SourceLayer::poll`] found.
pub enum Polled {
    /// Nothing new.
    Idle,
    /// A read is still running.
    Waiting,
    /// A read finished: install its features.
    Done(Window, Batch),
    /// A read failed.
    Failed(String),
}

impl SourceLayer {
    /// The original local file, excluding embedded data, databases and query results.
    pub fn linked_file(&self) -> Option<&str> {
        if self.embedded.is_some() || self.postgis.is_some() || self.mysql.is_some() || self.sqlserver.is_some() {
            return None;
        }
        let p = if let Some(g) = &self.gdal {
            Some(g.path.as_str())
        } else if let Some(d) = &self.duckdb {
            if d.sql.is_some() || d.table.is_none() { None } else { d.database.as_deref() }
        } else {
            self.path.as_deref()
        };
        p.filter(|p| crate::paths::is_local(p))
    }

    /// Opens a replacement with fresh handles, windows, caches and selection.
    pub fn relinked(&self, path: &str, table: Option<&str>) -> Result<SourceLayer, String> {
        if self.embedded.is_some() || self.postgis.is_some() || self.mysql.is_some() || self.sqlserver.is_some() {
            return Err("the layer reads a database or data kept in the project: there's no file to repair".into());
        }
        let mut new =
            SourceLayer { shapefile_crs: self.shapefile_crs, limit: self.limit, byte_budget: self.byte_budget, ..Default::default() };
        if let Some(g) = &self.gdal {
            #[cfg(target_arch = "wasm32")]
            {
                let _ = g;
                return Err("GDAL files are repaired in the desktop app".into());
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let layer = table.unwrap_or(&g.layer);
                let fresh = rustgis_data::source::gdal::convert_one(std::path::Path::new(path), layer, g.family, g.crs.as_deref())
                    .map_err(|e| format!("{path}: {e}"))?
                    .ok_or_else(|| format!("{path}: {layer} has no features"))?;
                new.path = Some(fresh.display().to_string());
                new.gdal = Some(GdalOrigin { path: path.into(), layer: layer.into(), ..g.clone() });
            }
        } else if let Some(d) = &self.duckdb {
            if d.sql.is_some() || d.table.is_none() {
                return Err("a layer from a DuckDB query is read again by running the query (Add Data From SQL), not repaired".into());
            }
            #[cfg(target_arch = "wasm32")]
            return Err("DuckDB files are repaired in the desktop app".into());
            #[cfg(not(target_arch = "wasm32"))]
            {
                let d =
                    DuckdbOrigin { database: Some(path.into()), table: table.map(String::from).or_else(|| d.table.clone()), ..d.clone() };
                new.path = Some(rerun(&d).map_err(|e| format!("{path}: {e}"))?.display().to_string());
                new.duckdb = Some(d);
            }
        } else {
            new.path = Some(path.into());
            new.table = table.map(String::from).or_else(|| self.table.clone());
            if path.to_ascii_lowercase().ends_with(".gpkg")
                && let Ok(layers) = rustgis_data::source::layers(std::path::Path::new(path))
            {
                let list = layers.join(", ");
                if new.table.is_none() && layers.len() > 1 {
                    return Err(format!("{path} holds several tables ({list}): choose one (\"table\")"));
                }
                if let Some(t) = &new.table
                    && !layers.contains(t)
                {
                    return Err(format!("{path} has no table '{t}' (it has {list}): choose one (\"table\")"));
                }
            }
        }
        new.open().map_err(|e| format!("{path}: {e}"))?;
        Ok(new)
    }

    /// A source that can be read whole a chunk at a time, for an export.
    pub fn visit_source(&self) -> Option<Arc<dyn FeatureSource>> {
        self.handle.as_ref().filter(|h| h.visits_chunks()).cloned()
    }

    /// A source capable of a complete, ordered render traversal.
    pub fn chunk_source(&self) -> Option<Arc<dyn FeatureSource>> {
        self.handle.as_ref().filter(|h| h.streams_chunks()).cloned()
    }

    /// Reuses a complete source's thinning decision for a projection and LOD level. Both
    /// outcomes are retained across render snapshots; failed calculations are retried. The
    /// bounded cache follows the open handle, and explicit source resets discard it.
    pub fn cached_thinning(
        &self,
        projection: &str,
        level: i32,
        calculate: impl FnOnce() -> rustgis_data::Result<bool>,
    ) -> rustgis_data::Result<bool> {
        let Some(handle) = &self.handle else { return calculate() };
        // Calculations insert only on success, so an escaped source panic leaves a valid
        // cache. Recover the guard so later frames can cache successful decisions again.
        let mut cache = self.render_lod.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.get_or_try_insert(handle, projection, level, calculate)
    }

    /// Retained interaction bytes assigned by the map, or the standalone default.
    pub fn budget(&self) -> usize {
        if self.byte_budget == 0 { VIEW_BYTES } else { self.byte_budget }
    }

    /// A source read from a file on disk.
    pub fn from_path(path: impl Into<String>, table: Option<String>, handle: Box<dyn FeatureSource>) -> Self {
        SourceLayer { path: Some(path.into()), table, handle: Some(Arc::from(handle)), ..SourceLayer::default() }
    }

    /// A PostGIS table read live.
    pub fn from_postgis(origin: PostgisOrigin, handle: Box<dyn FeatureSource>) -> Self {
        SourceLayer { postgis: Some(origin), handle: Some(Arc::from(handle)), ..SourceLayer::default() }
    }

    /// A MySQL or MariaDB table read live.
    pub fn from_mysql(origin: MysqlOrigin, handle: Box<dyn FeatureSource>) -> Self {
        SourceLayer { mysql: Some(origin), handle: Some(Arc::from(handle)), ..SourceLayer::default() }
    }

    /// A SQL Server table read live.
    pub fn from_sqlserver(origin: SqlServerOrigin, handle: Box<dyn FeatureSource>) -> Self {
        SourceLayer { sqlserver: Some(origin), handle: Some(Arc::from(handle)), ..SourceLayer::default() }
    }

    /// A source read from a file's bytes, kept in the project.
    pub fn from_bytes(file_name: impl Into<String>, bytes: Arc<Vec<u8>>, table: Option<String>, handle: Box<dyn FeatureSource>) -> Self {
        SourceLayer {
            file_name: Some(file_name.into()),
            embedded: Some(bytes),
            table,
            handle: Some(Arc::from(handle)),
            ..SourceLayer::default()
        }
    }

    /// Opens the source again (after a project loads). A source by its web address is opened off
    /// the frame: `Ok` while it is (see [`SourceLayer::is_pending`]), open once it has landed.
    pub fn open(&mut self) -> Result<(), String> {
        if let Some(url) = self.path.clone().filter(|p| rustgis_data::source::is_remote_name(p)) {
            return self.open_remote(&url);
        }
        let src = if let Some(pg) = &self.postgis {
            open_postgis(pg)
        } else if let Some(my) = &self.mysql {
            open_mysql(my)
        } else if let Some(ms) = &self.sqlserver {
            open_sqlserver(ms)
        } else if let Some(bytes) = &self.embedded {
            let name = self.file_name.as_deref().unwrap_or("layer.parquet");
            rustgis_data::source::open_shared(name, bytes.clone(), self.table.as_deref())
        } else if let Some(p) = &self.path {
            #[cfg(not(target_arch = "wasm32"))]
            let overridden = self.shapefile_crs.filter(|_| p.to_ascii_lowercase().ends_with(".shp"));
            let opened = {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(crs) = overridden {
                    let name = std::path::Path::new(p).file_stem().and_then(|s| s.to_str()).unwrap_or("layer");
                    rustgis_data::source::shp::ShpSource::open_path_in(std::path::Path::new(p), name, Some(crs))
                        .map(|src| Box::new(src) as Box<dyn FeatureSource>)
                } else {
                    rustgis_data::source::open_path(std::path::Path::new(p), self.table.as_deref())
                }
                #[cfg(target_arch = "wasm32")]
                rustgis_data::source::open_path(std::path::Path::new(p), self.table.as_deref())
            };
            match (opened, &self.gdal) {
                // The converted copy is gone (a cleared cache, another machine): convert again.
                #[cfg(not(target_arch = "wasm32"))]
                // (Only when it is missing: a copy that's there but unreadable is reported as is.)
                (Err(_), Some(g)) if !std::path::Path::new(p).exists() => {
                    match rustgis_data::source::gdal::convert_one(std::path::Path::new(&g.path), &g.layer, g.family, g.crs.as_deref()) {
                        Ok(Some(fresh)) => {
                            let r = rustgis_data::source::open_path(&fresh, None);
                            if r.is_ok() {
                                self.path = Some(fresh.display().to_string());
                            }
                            r
                        }
                        Ok(None) => Err(rustgis_data::DataError::Format(format!("{}: the layer has no features", g.path))),
                        Err(e) => Err(e),
                    }
                }
                (r, _) => match (r, &self.duckdb) {
                    #[cfg(not(target_arch = "wasm32"))]
                    (Err(_), Some(d)) if !std::path::Path::new(p).exists() => match rerun(d) {
                        Ok(fresh) => {
                            let r = rustgis_data::source::open_path(&fresh, None);
                            if r.is_ok() {
                                self.path = Some(fresh.display().to_string());
                            }
                            r
                        }
                        Err(e) => Err(e),
                    },
                    (r, _) => r,
                },
            }
        } else {
            return Err("the layer has no source".into());
        };
        match src {
            Ok(s) => {
                self.handle = Some(Arc::from(s));
                self.window = None;
                self.error = None;
                self.login = false;
                Ok(())
            }
            Err(e) => {
                self.login = matches!(e, rustgis_data::DataError::Login(_));
                let e = e.to_string();
                self.error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Opens a source by its web address: from what is fetched of it, else fetching its header
    /// (or footer) off the frame, taken by a later call when it has landed.
    fn open_remote(&mut self, url: &str) -> Result<(), String> {
        if let Some(slot) = self.opening.clone() {
            // A file that can't be read in place is downloaded whole into the same slot, by the
            // first copy to see it; the others wait on that download rather than start their own.
            let claimed = slot.lock().ok().is_some_and(|mut s| {
                let whole = matches!(&*s, Some(Err(e)) if e.contains(rustgis_data::source::remote::READ_WHOLE));
                if whole {
                    *s = None;
                }
                whole
            });
            if claimed {
                return self.open_whole(url, slot);
            }
            // Read, not taken: copies of the layer (undo, Load All Features) share the slot.
            let done = slot.lock().ok().and_then(|s| s.clone());
            return match done {
                None => Ok(()),
                Some(r) => {
                    self.opening = None;
                    self.opened(r)
                }
            };
        }
        match rustgis_data::source::open_url_now(url) {
            Ok(s) => self.opened(Ok(Arc::from(s))),
            Err(e) if e.to_string().contains(rustgis_data::source::remote::READ_WHOLE) => self.open_whole(url, Arc::new(Mutex::new(None))),
            Err(e) if e.to_string().contains(rustgis_data::source::remote::NEED) => {
                let slot: Opening = Arc::new(Mutex::new(None));
                let land = slot.clone();
                self.opening = Some(slot);
                rustgis_data::source::open_url(
                    url,
                    Box::new(move |r| {
                        if let Ok(mut s) = land.lock() {
                            *s = Some(r.map(Arc::from).map_err(|e| e.to_string()));
                        }
                        landed();
                    }),
                );
                Ok(())
            }
            Err(e) => self.opened(Err(e.to_string())),
        }
    }

    /// A file by its web address that can't be read in place (a GeoParquet without a bounding-box
    /// covering, a FlatGeobuf without a spatial index, a server without byte ranges): downloaded
    /// whole off the frame and read from memory, as Add Data From URL does, so a project's layer
    /// keeps its symbology instead of staying empty. The project keeps the address, not the bytes.
    /// The download lands in `slot`, which copies of the layer may share.
    fn open_whole(&mut self, url: &str, slot: Opening) -> Result<(), String> {
        let land = slot.clone();
        self.opening = Some(slot);
        let name = url.split(['?', '#']).next().unwrap_or(url).rsplit('/').next().unwrap_or("layer").to_string();
        let table = self.table.clone();
        let url = url.to_string();
        rustgis_data::source::remote::fetch_whole(
            &url.clone(),
            Box::new(move |r| {
                let r = match r {
                    Ok(bytes) => rustgis_data::source::open_shared(&name, Arc::new(bytes), table.as_deref())
                        .map(Arc::from)
                        .map_err(|e| format!("{url}: {e}")),
                    Err(rustgis_data::source::remote::Failed::Other(e)) => Err(format!("{url}: {e}")),
                    Err(e) => Err(format!("{url}: couldn't be downloaded ({e:?})")),
                };
                if let Ok(mut s) = land.lock() {
                    *s = Some(r);
                }
                landed();
            }),
        );
        Ok(())
    }

    fn opened(&mut self, r: Result<Arc<dyn FeatureSource>, String>) -> Result<(), String> {
        match r {
            Ok(s) => {
                self.handle = Some(s);
                self.window = None;
                self.error = None;
                Ok(())
            }
            Err(e) => {
                self.error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Waits (up to `timeout`) for a source by its web address to open: exports, which draw
    /// what is there. Native only (in the browser nothing may wait).
    pub fn wait_open(&mut self, timeout: std::time::Duration) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let start = web_time::Instant::now();
            while self.opening.is_some() && !self.is_open() && self.error.is_none() && start.elapsed() < timeout {
                std::thread::sleep(std::time::Duration::from_millis(5));
                let _ = self.open();
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = timeout;
    }

    pub fn is_open(&self) -> bool {
        self.handle.is_some()
    }

    pub fn info(&self) -> Option<&SourceInfo> {
        self.handle.as_deref().map(|h| h.info())
    }

    /// Every feature of the source, up to the read limit (a source by its web address fetching
    /// what that needs, on the desktop).
    pub fn read_all(&self) -> Result<(rustgis_data::Dataset, rustgis_data::ReadReport), String> {
        let h = self.handle.clone().ok_or("the layer's source isn't open")?;
        #[cfg(not(target_arch = "wasm32"))]
        if h.remote().is_some() {
            return rustgis_data::source::materialize(&rustgis_data::source::Fetching(h)).map_err(|e| e.to_string());
        }
        let r = rustgis_data::source::materialize(h.as_ref()).map_err(|e| e.to_string());
        match r {
            Err(e) if e.contains(rustgis_data::source::remote::NEED) => {
                Err("the web app reads a file by its address a view at a time: download it to load it whole".into())
            }
            r => r,
        }
    }

    pub fn window(&self) -> Option<&Window> {
        self.window.as_ref()
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some() || self.opening.is_some()
    }

    /// Opens the source afresh, forgetting a failed or never-started open (and the window read
    /// from the old one): a source by its web address read again once what signs it has changed
    /// (its address, or the login behind it). Its symbology and place in the project stay.
    pub fn reopen(&mut self) -> Result<(), String> {
        self.handle = None;
        self.opening = None;
        self.pending = None;
        self.error = None;
        self.login = false;
        self.reset();
        self.open()
    }

    /// Forgets the window, so the next view reads again (after the source changes).
    pub fn reset(&mut self) {
        self.render_lod = Default::default();
        self.reset_window();
    }

    /// Drops disposable view rows while retaining decisions about the unchanged full source.
    pub(crate) fn reset_window(&mut self) {
        self.window = None;
        self.pending = None;
        self.ready_cache = None;
        self.estimated_row_bytes = 0;
        self.saved_selection.clear();
    }

    /// The extent to read for a view (longitude/latitude), or `None` when the window serves it.
    pub fn wanted(&self, view: &Extent) -> Option<Extent> {
        let v = clamp_world(view);
        if v.is_empty() {
            return None;
        }
        if let Some(w) = &self.window {
            let all = self.info().map(|i| i.extent).filter(|e| !e.is_empty());
            // Everything has been read: nothing more to fetch, wherever the view goes.
            if w.complete && all.is_some_and(|a| contains(&w.extent, &a)) {
                return None;
            }
            // The window covers the view, and is complete or not much larger (a view zoomed well
            // inside a sampled window reads again, for the detail the sample left out).
            if contains(&w.extent, &v) && (w.complete || area(&v) * 6.0 > area(&w.extent)) {
                return None;
            }
        }
        // One read at a time per layer: while one runs, the view waits for it, and the next
        // frame after it lands asks again for wherever the view has gone (so fast panning
        // doesn't pile up reads).
        if self.pending.is_some() {
            return None;
        }
        Some(clamp_world(&v.padded(0.5, 0.0)))
    }

    /// Starts reading an extent: on a background thread unless `block` (or in the browser).
    pub fn start(&mut self, extent: Extent, block: bool) {
        self.ready_cache = None;
        let Some(h) = self.handle.clone() else { return };
        let most = if h.remote().is_some() { REMOTE_VIEW_LIMIT } else { VIEW_LIMIT };
        let req = Request { extent: Some(extent), limit: if self.limit == 0 { most } else { self.limit } };
        let budget = self.budget();
        let projection = self.projection.clone().unwrap_or(crate::Projection::WebMercator);
        let estimate = self.estimated_row_bytes;
        let full_read = self.full_read;
        let fields = self.window_fields;
        let pending = Arc::new(Pending { extent, result: Mutex::new(None) });
        let p = pending.clone();
        // A source by its web address: the read runs as its bytes land (on the desktop and in
        // the browser alike), fetching what it needs in rounds.
        if let Some(file) = h.remote() {
            let (h2, p2) = (h.clone(), p.clone());
            let work = Arc::new(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    window::read_mode(h2.as_ref(), &req, budget, &projection, estimate, full_read, fields)
                }))
                .unwrap_or_else(|_| Err(rustgis_data::DataError::Format("the read failed unexpectedly".into())))
            });
            rustgis_data::source::remote::drive(
                file,
                work,
                Box::new(move |r| {
                    if let Ok(mut slot) = p2.result.lock() {
                        *slot = Some(r.map_err(|e| e.to_string()));
                    }
                    landed();
                }),
            );
            self.pending = Some(pending);
            let _ = block;
            return;
        }
        let job = move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                window::read_mode(h.as_ref(), &req, budget, &projection, estimate, full_read, fields)
            }))
            .unwrap_or_else(|_| Err(rustgis_data::DataError::Format("the read failed unexpectedly".into())))
            .map_err(|e| e.to_string());
            if let Ok(mut slot) = p.result.lock() {
                *slot = Some(r);
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        if !block {
            // The read's requests (a remote FlatGeobuf's or GeoParquet's ranges) belong to the
            // session that asked for it: its diagnostics capture goes with the thread.
            let capture = rustgis_data::diagnostics::Capture::current();
            let spawned = std::thread::Builder::new().name("rustgis-source".into()).spawn(move || {
                let _diagnostics = capture.enter();
                job();
                GENERATION.fetch_add(1, Ordering::Relaxed);
                if let Some(w) = waker().lock().ok().and_then(|w| w.clone()) {
                    w();
                }
            });
            match spawned {
                Ok(_) => self.pending = Some(pending),
                // No thread to read on: say so, and don't ask again until the view moves on.
                Err(e) => {
                    self.saved_selection.clear();
                    self.error = Some(format!("couldn't start reading the layer: {e}"));
                    self.window = Some(Window { extent, complete: true, matched: 0, kept: 0 });
                }
            }
            return;
        }
        let _ = block;
        job();
        self.pending = Some(pending);
    }

    /// Takes a finished read, if any. With `block`, waits for one in flight (up to a minute).
    pub fn poll(&mut self, block: bool) -> Polled {
        let Some(p) = self.pending.clone() else { return Polled::Idle };
        let start = web_time::Instant::now();
        loop {
            let done = p.result.lock().ok().and_then(|mut r| r.take());
            match done {
                Some(Ok((b, cache))) => {
                    self.pending = None;
                    self.estimated_row_bytes = cache.mean_bytes;
                    self.ready_cache = Some(cache);
                    let w = Window { extent: p.extent, complete: b.complete, matched: b.matched, kept: b.features.len() };
                    self.window = Some(w);
                    self.error = None;
                    return Polled::Done(w, b);
                }
                Some(Err(e)) => {
                    self.pending = None;
                    self.ready_cache = None;
                    self.saved_selection.clear();
                    self.error = Some(e.clone());
                    // Not read again until the view moves past it.
                    self.window = Some(Window { extent: p.extent, complete: true, matched: 0, kept: 0 });
                    return Polled::Failed(e);
                }
                None if block && cfg!(not(target_arch = "wasm32")) && start.elapsed() < web_time::Duration::from_secs(60) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                // An export that waited a minute for the read: say so rather than draw what's
                // there as if it were the view. (The read carries on; the next view takes it.)
                None if block && cfg!(not(target_arch = "wasm32")) => {
                    let e = "reading the layer took more than a minute".to_string();
                    self.saved_selection.clear();
                    self.error = Some(e.clone());
                    return Polled::Failed(e);
                }
                None => return Polled::Waiting,
            }
        }
    }

    /// (features read, features matching) when the window is a sample of the view.
    pub fn sampled(&self) -> Option<(usize, u64)> {
        self.window.as_ref().filter(|w| !w.complete).map(|w| (w.kept, w.matched))
    }
}

fn area(e: &Extent) -> f64 {
    e.width() * e.height()
}

fn contains(outer: &Extent, inner: &Extent) -> bool {
    !outer.is_empty()
        && !inner.is_empty()
        && outer.min_x <= inner.min_x
        && outer.min_y <= inner.min_y
        && outer.max_x >= inner.max_x
        && outer.max_y >= inner.max_y
}

fn clamp_world(e: &Extent) -> Extent {
    if e.is_empty() {
        return *e;
    }
    let c = Extent::new(e.min_x.max(-180.0), e.min_y.max(-90.0), e.max_x.min(180.0), e.max_y.min(90.0));
    if c.is_empty() { Extent::EMPTY } else { c }
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

fn waker() -> &'static Mutex<Option<Arc<dyn Fn() + Send + Sync>>> {
    static W: OnceLock<Mutex<Option<Arc<dyn Fn() + Send + Sync>>>> = OnceLock::new();
    W.get_or_init(|| Mutex::new(None))
}

/// Called whenever a background read finishes (the UI asks for a repaint).
pub fn set_waker(f: impl Fn() + Send + Sync + 'static) {
    if let Ok(mut w) = waker().lock() {
        *w = Some(Arc::new(f));
    }
}

/// Bumped whenever a background read finishes, so a cached map image knows to redraw.
pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    /// Reopening this same layer must invalidate populated decisions even without `reset`.
    #[test]
    fn reopening_a_populated_thinning_cache_uses_the_new_handle() {
        let mut ds = rustgis_data::Dataset::new("source", rustgis_data::GeometryType::Point, vec![]);
        ds.push(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.0, 0.0))), vec![]);
        let bytes = std::sync::Arc::new(rustgis_data::source::fgb::write(&ds).unwrap());
        let handle = rustgis_data::source::open_shared("source.fgb", bytes.clone(), None).unwrap();
        let mut source = super::SourceLayer::from_bytes("source.fgb", bytes, None, handle);
        assert!(!source.cached_thinning("mercator", 0, || Ok(false)).unwrap());
        let old_handle = source.handle.clone().unwrap();
        source.open().unwrap();
        assert!(!std::sync::Arc::ptr_eq(&old_handle, source.handle.as_ref().unwrap()));
        assert!(source.cached_thinning("mercator", 0, || Ok(true)).unwrap());
        assert!(source.cached_thinning("mercator", 0, || panic!("new handle's decision must be cached")).unwrap());
    }

    #[test]
    fn postgis_edits_keep_their_row_versions() {
        let edit = super::PostgisEdit {
            origin: super::PostgisOrigin { database: "postgres:host=h dbname=g".into(), table: "roads".into(), family: None, crs: None },
            key: "gid".into(),
            srid: 3857,
            writable: true,
            condition: Some("lanes > 2".into()),
            limit: 100,
            versions: [(1, 700), (2, 701), (u64::MAX, 9)]
                .into_iter()
                .map(|(k, version)| (k, super::RowVersion { version, hash: k ^ 5 }))
                .collect(),
        };
        let text = serde_json::to_string(&edit).unwrap_or_default();
        assert!(text.contains("\"versions\":[[1,700,4],[2,701,7],"), "{text}");
        let back: super::PostgisEdit = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, edit);
        assert_eq!((back.versions.get(2).map(|r| r.version), back.versions.get(3), back.versions.len()), (Some(701), None, 3));
        // A copy (an undo snapshot) shares the versions.
        let copy = edit.clone();
        assert!(std::sync::Arc::ptr_eq(&copy.versions.0, &edit.versions.0));
    }

    /// The hash is FNV-1a over the row's JSON, the same as the projects saved with it hold.
    #[test]
    fn row_hashes_are_fnv_over_the_rows_json() {
        use rustgis_data::{Coord, Feature, Geometry, Value};
        let f = Feature {
            id: 3,
            geometry: Some(Geometry::Point(Coord { x: 1.5, y: -2.0 })),
            attrs: vec![Value::Text("Main St".into()), Value::Int(4), Value::Null],
        };
        let bytes = serde_json::to_vec(&(&f.geometry, &f.attrs)).unwrap();
        let fnv = bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3));
        assert_eq!(super::row_hash(&f), fnv);
        // And the value itself: a change in how rows are written would show every saved row edited.
        assert_eq!(super::row_hash(&f), 8_547_063_913_914_077_080);
    }

    #[test]
    fn changes_are_what_differs_from_the_rows_as_read() {
        use rustgis_data::{Coord, Feature, Geometry, Value};
        let row = |id: u64, name: &str| Feature {
            id,
            geometry: Some(Geometry::Point(Coord { x: id as f64, y: 1.0 })),
            attrs: vec![Value::Text(name.into())],
        };
        let loaded = [row(1, "a"), row(2, "b"), row(3, "c")];
        let edit = super::PostgisEdit {
            origin: super::PostgisOrigin { database: "postgres:dbname=g".into(), table: "t".into(), family: None, crs: None },
            key: "id".into(),
            srid: 4326,
            writable: true,
            condition: None,
            limit: 0,
            versions: loaded.iter().map(|f| (f.id, super::RowVersion { version: f.id as i64 * 10, hash: super::row_hash(f) })).collect(),
        };
        assert!(edit.versions.changes(&loaded).is_empty());
        // Row 1 as it was, row 2 renamed, row 3 deleted, a new feature and a copy of row 1.
        let now = [row(1, "a"), row(2, "bee"), row(9, "new"), row(1, "copy")];
        let c = edit.versions.changes(&now);
        assert_eq!(c.updates.iter().map(|(f, v)| (f.id, *v)).collect::<Vec<_>>(), vec![(2, 20)]);
        assert_eq!(c.deletes, vec![(3, 30)]);
        assert_eq!(c.inserts.iter().map(|f| f.id).collect::<Vec<_>>(), vec![9, 1]);
        assert_eq!(c.insert_rows, vec![2, 3]);
        // The hash is the same however often it is taken.
        assert_eq!(super::row_hash(&now[1]), super::row_hash(&row(2, "bee")));
    }
}
