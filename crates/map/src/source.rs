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
use std::sync::{Arc, Mutex, OnceLock, Weak};

use rustgis_data::source::{Batch, FeatureSource, Request, SourceInfo};
use rustgis_geom::Extent;
use serde::{Deserialize, Serialize};

mod render_lod;
mod window;
pub(crate) use window::Query;

/// Retained streamed rows and geometry caches per map; excludes scratch and render/export copies.
pub const VIEW_BYTES: usize = if cfg!(target_arch = "wasm32") { 48 * 1024 * 1024 } else { 128 * 1024 * 1024 };

/// Sources with more features than this are streamed when added; smaller ones are loaded whole.
pub const STREAM_ABOVE: u64 = 100_000;

/// The most features a view reads.
pub const VIEW_LIMIT: usize = if cfg!(target_arch = "wasm32") { 100_000 } else { 250_000 };

/// The most features a view reads from a file by its web address (each one is fetched).
pub const REMOTE_VIEW_LIMIT: usize = if cfg!(target_arch = "wasm32") { 30_000 } else { 60_000 };

/// Repair advice shared by render errors and the session's translated read-failure notice.
pub const REPAIR_SOURCE: &str = "if the file was moved or deleted, restore it or use Repair Data Source to point to its new location";

const RECONNECT_REQUIRED: &str = "this database source came from a peer; reconnect explicitly before reading it";

/// The kinds of read note one opening of a source says at most ([`SourceLayer::saw_read_notes`]).
const READ_NOTE_KINDS: usize = 32;

/// A read note's kind: its text with each count as `#`, so the same note with another count (a view
/// read of another sample) is the same kind. A count is a number that starts the note or a clause
/// (after `: `, `, `, `; ` or `(`) and is followed by a space, as notes put their counts ("3 of the
/// shapes read …", "so far, 2 values …"). Any other number (an EPSG code, a column's place, a
/// count written mid-clause) is kept, so notes that differ in it are kinds of their own.
fn note_kind(note: &str) -> String {
    let mut kind = String::with_capacity(note.len());
    let mut rest = note;
    while let Some(start) = rest.find(|c: char| c.is_ascii_digit()) {
        let (before, from) = rest.split_at(start);
        let len = from.find(|c: char| !c.is_ascii_digit()).unwrap_or(from.len());
        let (digits, after) = from.split_at(len);
        // A run of digits is whole, so only the note's first can have nothing before it.
        let opens = (kind.is_empty() && before.is_empty()) || [": ", ", ", "; ", "("].iter().any(|p| before.ends_with(p));
        kind.push_str(before);
        kind.push_str(if opens && after.starts_with(' ') { "#" } else { digits });
        rest = after;
    }
    kind.push_str(rest);
    kind
}

/// Ambiguous polygon areas among decoded features, including rows an area query could not keep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AmbiguousAreas {
    pub rings: usize,
    pub features: usize,
}

impl AmbiguousAreas {
    /// Count each affected feature once and all of its ambiguous rings.
    pub fn from_features(features: &[rustgis_data::Feature]) -> Self {
        Self::from_geometries(features.iter().filter_map(|feature| feature.geometry.as_ref()))
    }

    /// [`AmbiguousAreas::from_features`] of features given in `crs`, read in longitude/latitude
    /// (a place that can't be given there is left out of its ring).
    pub fn from_features_in(features: &[rustgis_data::Feature], crs: rustgis_data::raster::Crs) -> Self {
        if crs == rustgis_data::raster::Crs::LonLat {
            return Self::from_features(features);
        }
        let Ok(to) = crs.to(rustgis_data::raster::Crs::LonLat) else { return Self::default() };
        // One converted geometry at a time, not a copy of the layer.
        let mut counts = Self::default();
        for g in features.iter().filter_map(|f| f.geometry.as_ref()) {
            counts.include(Self::from_geometries(std::iter::once(&to.geometry(g))));
        }
        counts
    }

    fn from_geometries<'a>(geometries: impl Iterator<Item = &'a rustgis_data::Geometry>) -> Self {
        let mut counts = Self::default();
        for geometry in geometries {
            let rings = geometry.ambiguous_area_rings();
            if rings > 0 {
                counts.rings = counts.rings.saturating_add(rings);
                counts.features = counts.features.saturating_add(1);
            }
        }
        counts
    }

    pub(crate) fn include(&mut self, other: Self) {
        self.rings = self.rings.saturating_add(other.rings);
        self.features = self.features.saturating_add(other.features);
    }
}

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
    /// Runtime metadata from a fresh provenance conversion, including its loader diagnostics.
    #[serde(skip)]
    conversion_info: Option<SourceInfo>,
    /// A successful saved-conversion fallback, consumed once when the project opens.
    #[serde(skip)]
    conversion_fallback: Option<String>,
    /// Losses from a conversion actually performed while opening, consumed once by its caller.
    #[serde(skip)]
    conversion_notes_unsaid: rustgis_data::loss::Notes,
    /// Values in date fields a view read found weren't dates ([`Batch::unread_dates`]), until
    /// said by [`SourceLayer::take_unread_dates`]; once said, later reads aren't counted again.
    /// Kept for each window read ([`Tally`]): a window read again replaces its counts.
    #[serde(skip)]
    unread_dates: Tally,
    /// Date-times a view read rounded to the microsecond ([`Batch::rounded_dates`]), likewise.
    #[serde(skip)]
    rounded_dates: Tally,
    #[serde(skip)]
    unread_dates_said: bool,
    #[serde(skip)]
    rounded_dates_said: bool,
    /// Text values `<Null>` a view read read as null ([`Batch::null_text`]), likewise.
    #[serde(skip)]
    null_text: Tally,
    #[serde(skip)]
    null_text_said: bool,
    /// Text values a view read found weren't UTF-8 ([`Batch::not_utf8`]), likewise.
    #[serde(skip)]
    not_utf8: Tally,
    /// The encoding `not_utf8` counts were read in, taken when they were kept: the source may be
    /// opened again before they are said.
    #[serde(skip)]
    not_utf8_in: Option<&'static str>,
    #[serde(skip)]
    not_utf8_said: bool,
    /// What else view reads lost or assumed ([`Batch::notes`]), until said by
    /// [`SourceLayer::take_read_notes`].
    #[serde(skip)]
    read_notes: rustgis_data::loss::Notes,
    /// The kinds of note view reads of this opening have given ([`note_kind`]): each is said once,
    /// with the count of the read that first gave it, not again on each redraw.
    #[serde(skip)]
    read_note_kinds: Vec<String>,
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
    /// The datum grids this window was read with ([`rustgis_data::raster::crs::grid_generation`]):
    /// a window read before a grid arrived is read again, as after a change of projection.
    #[serde(skip)]
    pub(crate) grids: Option<u64>,
    #[serde(skip)]
    pub(crate) window_fields: Option<usize>,
    /// The layer's definition query, applied while a view is read rather than to the rows read.
    #[serde(skip)]
    pub(crate) query: Option<Query>,
    /// A source by its web address being opened (its header or footer being fetched).
    #[serde(skip)]
    opening: Option<Opening>,
    /// Why the source couldn't be opened or read.
    #[serde(skip)]
    pub error: Option<String>,
    /// Last read failure reported by the session, independent of disposable render copies.
    #[serde(skip)]
    error_said: Option<String>,
    /// The last open failed because the server refused the login (no password given this session, or a wrong one).
    #[serde(skip)]
    pub login: bool,
    /// Runtime-only: imported source selection awaits an explicit reconnect or exact peer retention.
    #[serde(skip)]
    reconnect_required: bool,
    /// The most features a view reads (0 = [`VIEW_LIMIT`]).
    #[serde(skip)]
    pub limit: usize,
    /// Reads installed so far: the features changing with it came from the source, not an edit.
    #[serde(skip)]
    pub reads: u64,
    /// Last installed window examined by the session's display diagnostics.
    #[serde(skip)]
    display_read_said: u64,
    /// Counts computed on the read worker, including unavailable rows removed by its query.
    #[serde(skip)]
    pub(crate) area_counts: AmbiguousAreas,
    /// A public dataset mutation invalidates these counts just as it invalidates view caches.
    #[serde(skip)]
    pub(crate) area_features: Option<Weak<Vec<rustgis_data::Feature>>>,
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

/// FNV-1a's starting value.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a over `bytes`, from `h`.
fn fnv1a(h: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(h, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// How a loaded row is told apart from the same row edited: a hash of its geometry and
/// attributes (FNV-1a over their JSON, the same in every build). Non-finite attributes also
/// contribute their position and native bits, since output JSON substitutes Null for them.
pub fn row_hash(f: &rustgis_data::Feature) -> u64 {
    /// FNV-1a over the bytes written to it, kept as they come (no buffer per row).
    struct Fnv(u64);
    impl Fnv {
        /// Feeds bytes without buffering; also used for values JSON cannot distinguish.
        fn bytes(&mut self, buf: &[u8]) {
            self.0 = fnv1a(self.0, buf);
        }
    }
    impl std::io::Write for Fnv {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.bytes(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut h = Fnv(FNV_OFFSET);
    // As before, a row that can't be written hashes as no bytes.
    if serde_json::to_writer(&mut h, &(&f.geometry, &f.attrs)).is_err() {
        return FNV_OFFSET;
    }
    for (index, value) in f.attrs.iter().enumerate() {
        if let rustgis_data::Value::Float(number) = value
            && !number.is_finite()
        {
            h.bytes(b"\xffnon-finite:");
            h.bytes(&(index as u64).to_le_bytes());
            h.bytes(&number.to_bits().to_le_bytes());
        }
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

/// Ordinary conversions live directly in the provider's reusable cache. Other linked copies
/// (including reader-owned workspaces) must not borrow another source's artifact lifetime.
#[cfg(not(target_arch = "wasm32"))]
fn persistent_conversion(path: &std::path::Path, cache: Option<std::path::PathBuf>) -> bool {
    if rustgis_data::source::gdal::ReloadWorkspace::contains_file(path) {
        return false;
    }
    path.parent()
        .and_then(|p| p.canonicalize().ok())
        .zip(cache.and_then(|p| p.canonicalize().ok()))
        .is_some_and(|(parent, cache)| parent == cache)
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
            return Err(rustgis_data::DataError::Format(rustgis_data::diagnostics::message_with_address(
                &pg.database,
                "not a PostgreSQL connection",
            )));
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
            return Err(rustgis_data::DataError::Format(rustgis_data::diagnostics::message_with_address(
                &my.database,
                "not a MySQL or MariaDB connection",
            )));
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
            return Err(rustgis_data::DataError::Format(rustgis_data::diagnostics::message_with_address(
                &ms.database,
                "not a SQL Server connection",
            )));
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
    /// The read kept the definition query's matches: `matched` counts them.
    pub filtered: bool,
}

/// A read in flight.
struct Pending {
    extent: Extent,
    result: Mutex<Option<Result<(Batch, window::Cache), String>>>,
    still: Option<rustgis_data::source::remote::Still>,
}

/// A source being opened off the frame.
type Opening = Arc<OpeningState>;
struct OpeningState {
    result: Mutex<Option<Result<Arc<dyn FeatureSource>, String>>>,
    still: Option<rustgis_data::source::remote::Still>,
    #[cfg(not(target_arch = "wasm32"))]
    progress: Option<Arc<OpeningProgress>>,
}
impl OpeningState {
    /// Owns the initiating carrier before an asynchronous source open can leave this thread.
    fn new() -> Opening {
        Arc::new(Self {
            result: Mutex::new(None),
            still: rustgis_data::source::remote::current_still(),
            #[cfg(not(target_arch = "wasm32"))]
            progress: None,
        })
    }
    /// Retains the existing completion-slot locking behavior; authority stays outside its payload.
    fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, Option<Result<Arc<dyn FeatureSource>, String>>>> {
        self.result.lock()
    }
}

/// Progress and cancellation for a local schema scan. A worker never keeps its owner alive:
/// dropping the last source copy stops the scan at its next progress check.
#[cfg(not(target_arch = "wasm32"))]
struct OpeningProgress {
    owner: Weak<OpeningState>,
    cancelled: std::sync::atomic::AtomicBool,
    done: AtomicU64,
    total: AtomicU64,
}

#[cfg(not(target_arch = "wasm32"))]
impl rustgis_data::progress::Probe for OpeningProgress {
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
            || self.owner.upgrade().is_none_or(|owner| rustgis_data::source::remote::check_still(&owner.still).is_err())
    }
    fn begin(&self, total: u64) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }
    fn advance(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }
}

/// Asks the app for a frame (the waker) with nothing new to draw: what landed while the map drew
/// is said a frame after (see [`wake`] for something new to draw).
pub(crate) fn another_frame() {
    if let Some(w) = waker().lock().ok().and_then(|w| w.clone()) {
        w();
    }
}

/// How many windows a [`Tally`] keeps apart.
const WINDOWS_COUNTED: usize = 64;

/// What view reads found, counted for each field by its position, kept for each window read: a
/// window read again replaces its counts instead of adding to them.
#[derive(Clone, Default)]
struct Tally {
    windows: Vec<(Extent, Vec<usize>)>,
    /// Counts of windows past [`WINDOWS_COUNTED`], added up as they come.
    // ponytail: past 64 windows between two notice passes a window read again adds up as before;
    // keep rows' identity in `Batch` to count exactly.
    rest: Vec<usize>,
}

impl Tally {
    fn saw(&mut self, window: &Extent, counts: &[usize]) {
        let at = self.windows.iter().position(|(w, _)| w == window);
        if counts.iter().all(|n| *n == 0) {
            if let Some(at) = at {
                self.windows.remove(at);
            }
        } else if let Some((_, kept)) = at.and_then(|at| self.windows.get_mut(at)) {
            *kept = counts.to_vec();
        } else if self.windows.len() < WINDOWS_COUNTED {
            self.windows.push((*window, counts.to_vec()));
        } else {
            rustgis_data::source::add_unread_dates(&mut self.rest, counts);
        }
    }

    /// Nothing is pending.
    fn is_empty(&self) -> bool {
        self.windows.is_empty() && self.rest.iter().all(|n| *n == 0)
    }

    /// The counts of every window added up, or `None` when nothing is pending.
    fn total(&self) -> Option<Vec<usize>> {
        if self.is_empty() {
            return None;
        }
        let mut all = self.rest.clone();
        for (_, counts) in &self.windows {
            rustgis_data::source::add_unread_dates(&mut all, counts);
        }
        Some(all)
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
    /// Counts for the installed read, or `None` if its features have since been changed.
    pub fn unavailable_areas(&self, features: &rustgis_data::Features) -> Option<AmbiguousAreas> {
        self.area_features.as_ref().filter(|key| Weak::ptr_eq(key, &features.allocation_key())).map(|_| self.area_counts)
    }
    /// Keeps what view reads found of values in date fields that weren't dates, until
    /// [`SourceLayer::take_unread_dates`] says it. Once said, later reads aren't counted: they
    /// read the same features again, and the notice says Load All Features counts them all.
    /// Each kind is counted until its own notice is said, so saying one doesn't silence the other.
    /// `window` is the extent the batch was read for: reading it again replaces its counts, so the
    /// same rows aren't counted twice. Two different windows that overlap still count the rows
    /// they share twice; the notice says "so far" and Load All Features counts them all.
    pub fn saw_unread_dates(&mut self, window: &Extent, unread: &[usize], rounded: &[usize]) {
        if !self.unread_dates_said {
            self.unread_dates.saw(window, unread);
        }
        if !self.rounded_dates_said {
            self.rounded_dates.saw(window, rounded);
        }
    }

    /// What view reads found of values in date fields that weren't dates
    /// ([`rustgis_data::source::unread_dates_note`]) and of date-times rounded to the microsecond
    /// ([`rustgis_data::source::rounded_dates_note`]), once for the source.
    pub fn take_unread_dates(&mut self) -> (Option<String>, Option<String>) {
        let Some(fields) = self.info().map(|i| i.fields.clone()) else { return (None, None) };
        let (unread, rounded) = (std::mem::take(&mut self.unread_dates).total(), std::mem::take(&mut self.rounded_dates).total());
        if unread.is_none() && rounded.is_none() {
            return (None, None);
        }
        self.unread_dates_said |= unread.is_some();
        self.rounded_dates_said |= rounded.is_some();
        (
            unread.and_then(|c| rustgis_data::source::unread_dates_note(&fields, &c)),
            rounded.and_then(|c| rustgis_data::source::rounded_dates_note(&fields, &c)),
        )
    }

    /// Keeps what view reads found of text values `<Null>` read as null, until
    /// [`SourceLayer::take_null_text`] says it, as [`SourceLayer::saw_unread_dates`] does for dates.
    pub fn saw_null_text(&mut self, window: &Extent, counts: &[usize]) {
        if !self.null_text_said {
            self.null_text.saw(window, counts);
        }
    }

    /// What view reads found of text values `<Null>` read as null
    /// ([`rustgis_data::source::null_text_note`]), once for the source.
    pub fn take_null_text(&mut self) -> Option<String> {
        let fields = self.info().map(|i| i.fields.clone())?;
        let counts = std::mem::take(&mut self.null_text).total()?;
        self.null_text_said = true;
        rustgis_data::source::null_text_note(&fields, &counts)
    }

    /// Keeps what view reads found of text values that aren't UTF-8, until
    /// [`SourceLayer::take_not_utf8`] says it, as [`SourceLayer::saw_unread_dates`] does for dates.
    pub fn saw_not_utf8(&mut self, window: &Extent, counts: &[usize]) {
        if !self.not_utf8_said {
            let encoding = self.handle.as_ref().map_or("UTF-8", |h| h.text_encoding());
            // Counts of another encoding are of another opening's text: they aren't added to these.
            if self.not_utf8_in != Some(encoding) {
                self.not_utf8 = Tally::default();
            }
            self.not_utf8_in = Some(encoding);
            self.not_utf8.saw(window, counts);
        }
    }

    /// What view reads found of text values that aren't UTF-8
    /// ([`rustgis_data::source::not_utf8_note`]), once for the source.
    pub fn take_not_utf8(&mut self) -> Option<String> {
        // Checked first: this runs every frame for each layer with a source, and most have nothing pending.
        if self.not_utf8.is_empty() {
            return None;
        }
        let fields = self.info().map(|i| i.fields.clone())?;
        let counts = std::mem::take(&mut self.not_utf8).total()?;
        self.not_utf8_said = true;
        rustgis_data::source::not_utf8_note(&fields, &counts, self.not_utf8_in.take().unwrap_or("UTF-8"))
    }

    /// Keeps what view reads said they lost or assumed ([`Batch::notes`]) until
    /// [`SourceLayer::take_read_notes`] says it: each kind of note once for the opening, as the
    /// next redraw reads the same features again. Two notes are one kind when they differ only in
    /// their counts ([`note_kind`]: a number starting the note or a clause, before a space); notes
    /// that differ in any other number or word are kinds of their own. Past [`READ_NOTE_KINDS`]
    /// kinds, one last note says there are more.
    pub fn saw_read_notes(&mut self, notes: &[rustgis_data::loss::Note]) {
        for note in notes.iter().filter(|n| !n.text.trim().is_empty()) {
            // A kind is its wording without its counts, and its loss code: a coded note isn't
            // folded into a plain one that reads the same.
            let kind = match note.loss {
                Some(code) => format!("{}\u{1f}{}", note_kind(&note.text), code.spec().id),
                None => note_kind(&note.text),
            };
            if self.read_note_kinds.contains(&kind) {
                continue;
            }
            if self.read_note_kinds.len() >= READ_NOTE_KINDS {
                if !self.read_note_kinds.iter().any(String::is_empty) {
                    // The empty kind marks the overflow note said: no note kept has an empty kind.
                    self.read_note_kinds.push(String::new());
                    self.read_notes.push(format!(
                        "reading it for the map gave more than {READ_NOTE_KINDS} kinds of note; Load All Features lists them all"
                    ));
                }
                return;
            }
            self.read_note_kinds.push(kind);
            self.read_notes.push(note.clone());
        }
    }

    /// What view reads said they lost or assumed ([`Batch::notes`]) and haven't said yet, each
    /// kind once for the opening ([`SourceLayer::saw_read_notes`]).
    pub fn take_read_notes(&mut self) -> rustgis_data::loss::Notes {
        std::mem::take(&mut self.read_notes)
    }

    /// Forgets which read notes were said, so the next view read says them again: the source was
    /// opened anew or its file written.
    pub fn forget_read_notes(&mut self) {
        self.read_notes.clear();
        self.read_note_kinds.clear();
    }

    /// For an undo snapshot: drops the notes not yet said, which the live layer says, and keeps
    /// the kinds, so a snapshot of another opening restored later doesn't say them again.
    pub fn hold_read_notes(&mut self) {
        self.read_notes.clear();
    }

    /// After undo, redo or a rollback restores this layer: the same opening as `live` (the layer
    /// it replaces) keeps what `live` has said and has still to say, as restoring the project
    /// doesn't read the source again. Another opening keeps its own.
    pub fn keep_read_notes(&mut self, live: &SourceLayer) {
        if self.same_opening(live) {
            self.read_notes.clone_from(&live.read_notes);
            self.read_note_kinds.clone_from(&live.read_note_kinds);
        }
    }

    /// Whether another installed window needs its retained display expressions checked.
    pub fn take_display_window(&mut self) -> bool {
        if self.reads == self.display_read_said {
            return false;
        }
        self.display_read_said = self.reads;
        true
    }
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

    /// A read failure with the linked file and the repair action, when it has a local source.
    /// The caller supplies the layer's current name, which can differ from the file's name.
    pub fn read_error(&self) -> Option<String> {
        let error = self.error.as_ref()?;
        Some(match self.linked_file() {
            Some(path) => format!("could not read {path}: {error}; {REPAIR_SOURCE}"),
            None => error.clone(),
        })
    }

    /// Clears a recovered failure and its notice marker, even between notice passes.
    pub fn clear_error(&mut self) {
        self.error = None;
        self.error_said = None;
    }

    /// Takes each persistent read failure once, so redraws do not repeat its notice. A successful
    /// read, or an explicit reopen/repair, allows a later failure to be reported.
    pub fn error_unsaid(&mut self) -> Option<String> {
        if self.error == self.error_said {
            return None;
        }
        let error = self.error.clone();
        self.error_said.clone_from(&error);
        error
    }

    /// Opens a replacement with fresh handles, windows, caches and selection.
    pub fn relinked(&self, path: &str, table: Option<&str>) -> Result<SourceLayer, String> {
        if self.embedded.is_some() || self.postgis.is_some() || self.mysql.is_some() || self.sqlserver.is_some() {
            return Err("the layer reads a database or data kept in the project: there's no file to repair".into());
        }
        let mut new =
            SourceLayer { shapefile_crs: self.shapefile_crs, limit: self.limit, byte_budget: self.byte_budget, ..Default::default() };
        let mut conversion_notes = rustgis_data::loss::Notes::new();
        let declared_empty;
        let conversion_zm_dropped;
        if let Some(g) = &self.gdal {
            #[cfg(target_arch = "wasm32")]
            {
                let _ = g;
                return Err("GDAL files are repaired in the desktop app".into());
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let layer = table.unwrap_or(&g.layer);
                let fresh = rustgis_data::source::gdal::convert_one_fresh(std::path::Path::new(path), layer, g.family, g.crs.as_deref())
                    .map_err(|e| format!("{path}: {e}"))?;
                conversion_notes = fresh.notes;
                declared_empty = fresh.empty;
                conversion_zm_dropped = fresh.zm_dropped;
                new.path = Some(fresh.path.display().to_string());
                let handle = rustgis_data::source::fgb::FgbSource::open_converted_path(
                    &fresh.path,
                    layer,
                    std::path::Path::new(path),
                    Some(fresh.workspace),
                )
                .map_err(|e| format!("{path}: {e}"))?;
                new.handle = Some(Arc::new(handle));
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
                use rustgis_data::source::duckdb;
                let database = duckdb::Database::File(path.into());
                let name = d.table.as_deref().ok_or_else(|| format!("{path}: the DuckDB layer has no table"))?;
                let table = duckdb::find_table_of(&database, name).map_err(|e| format!("{path}: {e}"))?;
                let fresh = duckdb::convert_table_fresh(std::path::Path::new(path), &table, d.family, d.crs.as_deref())
                    .map_err(|e| format!("{path}: {e}"))?;
                let out = fresh.output;
                if out.skipped > 0 {
                    conversion_notes.push(format!("DuckDB: {} rows without a geometry were left out", out.skipped));
                }
                if out.other_family > 0 {
                    conversion_notes.push(format!("DuckDB: {} rows of another geometry type were left out", out.other_family));
                }
                let part = out.parts.first().ok_or_else(|| format!("{path}: the table returns no {}", d.family.label()))?;
                if part.zm_dropped > 0 {
                    conversion_notes.push(rustgis_data::zm_note(Some(part.zm_dropped)));
                }
                declared_empty = part.count == 0;
                new.path = Some(part.path.display().to_string());
                let handle = rustgis_data::source::parquet::ParquetSource::open_converted_path(
                    &part.path,
                    name,
                    Some(std::path::Path::new(path)),
                    Some(fresh.workspace),
                )
                .map_err(|e| format!("{path}: {e}"))?;
                new.handle = Some(Arc::new(handle));
                conversion_zm_dropped = part.zm_dropped > 0;
                new.duckdb = Some(d);
            }
        } else {
            declared_empty = false;
            conversion_zm_dropped = false;
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
        let named = |e: &str| rustgis_data::diagnostics::message_with_address(path, e);
        if new.handle.is_none() {
            if path.to_ascii_lowercase().ends_with(".fgb") {
                // Repair/reload prepare and probe a complete candidate on the caller's worker.
                // A project-restored source instead uses open()'s asynchronous classification.
                new.handle = Some(Arc::from(
                    rustgis_data::source::open_path(std::path::Path::new(path), new.table.as_deref()).map_err(|e| named(&e.to_string()))?,
                ));
            } else {
                new.open().map_err(|e| named(&e))?;
            }
        }
        // Opening a header alone doesn't establish that its payload is readable. Probe the
        // candidate, never the live source, before the caller commits the replacement.
        let handle = new.handle.as_deref().ok_or_else(|| named("the replacement source didn't open"))?;
        let probe = handle.read(&Request { extent: None, limit: 1 }).map_err(|e| named(&e.to_string()))?;
        if probe.features.is_empty() && probe.skipped > 0 {
            return Err(named(&format!(
                "no readable features found while checking the replacement ({} rows could not be read)",
                probe.skipped
            )));
        }
        if probe.skipped > 0 {
            conversion_notes.push(format!("{} rows could not be read while checking the replacement", probe.skipped));
        }
        if let Some(info) = new.info() {
            let mut info = info.clone();
            info.notes.extend(conversion_notes);
            info.zm_dropped |= conversion_zm_dropped;
            if declared_empty && probe.complete && probe.features.is_empty() && probe.skipped == 0 {
                info.count = Some(0);
            }
            // Empty GeoParquet has no observed geometry_types. Only an actual, complete zero-row
            // conversion may inherit its selected family's declaration.
            if info.count == Some(0)
                && info.geometry_type == rustgis_data::GeometryType::None
                && let Some(d) = &new.duckdb
            {
                info.geometry_type = match d.family {
                    rustgis_data::source::Family::Points => rustgis_data::GeometryType::Point,
                    rustgis_data::source::Family::Lines => rustgis_data::GeometryType::Polyline,
                    rustgis_data::source::Family::Polygons => rustgis_data::GeometryType::Polygon,
                };
            }
            new.conversion_info = Some(info);
        }
        Ok(new)
    }

    /// The system its features are given in ([`FeatureSource::coord_crs`]): the file's own for a
    /// file read in place; longitude/latitude until it is open.
    pub fn coord_crs(&self) -> rustgis_data::raster::Crs {
        self.handle.as_ref().map_or(rustgis_data::raster::Crs::LonLat, |h| h.coord_crs())
    }

    /// A source that can be read whole a chunk at a time, for an export.
    pub fn visit_source(&self) -> Option<Arc<dyn FeatureSource>> {
        self.handle.as_ref().filter(|h| h.visits_chunks()).cloned()
    }

    /// A source that can provide all features in an extent a chunk at a time.
    pub(super) fn visit_extent_source(&self) -> Option<Arc<dyn FeatureSource>> {
        self.handle.as_ref().filter(|h| h.visits_extent_chunks()).cloned()
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

    /// Installs only a successfully opened provenance conversion, preserving this layer's
    /// settings and read generation while discarding state belonging to its previous reader.
    #[cfg(not(target_arch = "wasm32"))]
    fn restored_conversion(&mut self, candidate: Result<SourceLayer, String>) -> Result<(), String> {
        match candidate {
            Ok(new) => {
                self.path = new.path;
                self.handle = new.handle;
                self.conversion_info = new.conversion_info;
                self.conversion_notes_unsaid = self.conversion_info.as_ref().map(|info| info.notes.clone()).unwrap_or_default();
                self.opening = None;
                self.reset();
                self.clear_error();
                self.login = false;
                Ok(())
            }
            Err(e) => {
                self.error = Some(e.clone());
                self.login = false;
                Err(e)
            }
        }
    }

    /// Opens the source again (after a project loads). Web addresses and native FlatGeobuf date
    /// scans open off the frame: `Ok` while pending, open once a later call installs the result.
    pub fn open(&mut self) -> Result<(), String> {
        rustgis_data::source::remote::check_still(&rustgis_data::source::remote::current_still())?;
        if self.reconnect_required {
            self.error = Some(RECONNECT_REQUIRED.into());
            return Err(RECONNECT_REQUIRED.into());
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(slot) = self.opening.clone().filter(|slot| slot.progress.is_some()) {
            let cancelled = slot.progress.as_ref().is_some_and(|p| rustgis_data::progress::Probe::cancelled(p.as_ref()));
            let done =
                if cancelled { Some(Err("opening the layer was cancelled".into())) } else { slot.lock().ok().and_then(|r| r.clone()) };
            return match done {
                Some(result) => {
                    self.opening = None;
                    self.opened(result)
                }
                None => Ok(()),
            };
        }
        self.conversion_fallback = None;
        self.conversion_notes_unsaid.clear();
        if let Some(url) = self.path.clone().filter(|p| rustgis_data::source::is_remote_name(p)) {
            return self.open_remote(&url);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let mut saved_conversion_note = None;
        let src = if let Some(pg) = &self.postgis {
            open_postgis(pg)
        } else if let Some(my) = &self.mysql {
            open_mysql(my)
        } else if let Some(ms) = &self.sqlserver {
            open_sqlserver(ms)
        } else if let Some(bytes) = &self.embedded {
            let name = self.file_name.as_deref().unwrap_or("layer.parquet");
            #[cfg(not(target_arch = "wasm32"))]
            if name.to_ascii_lowercase().ends_with(".fgb") {
                let name = std::path::Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or("layer");
                let prepared = rustgis_data::source::fgb::FgbSource::prepare_shared(rustgis_data::source::Shared(bytes.clone()), name);
                return self.open_fgb(prepared);
            }
            rustgis_data::source::open_shared(name, bytes.clone(), self.table.as_deref())
        } else if let Some(p) = &self.path {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let path = std::path::Path::new(p);
                if let Some(g) = &self.gdal
                    && (!path.exists() || !persistent_conversion(path, rustgis_data::source::gdal::cache_dir()))
                {
                    let candidate = self.relinked(&g.path, None);
                    if candidate.is_ok() || !path.exists() || rustgis_data::source::gdal::ReloadWorkspace::contains_file(path) {
                        return self.restored_conversion(candidate);
                    }
                    if let Err(error) = candidate {
                        saved_conversion_note = Some(format!("re-reading {} failed ({error}); showing the saved conversion", g.path));
                    }
                }
                if let Some(d) = &self.duckdb
                    && d.sql.is_none()
                    && d.table.is_some()
                    && let Some(database) = &d.database
                    && matches!(rustgis_data::source::duckdb::Database::parse(database), rustgis_data::source::duckdb::Database::File(_))
                    && (!path.exists() || !persistent_conversion(path, rustgis_data::source::duckdb::cache_dir()))
                {
                    let candidate = self.relinked(database, None);
                    if candidate.is_ok() || !path.exists() || rustgis_data::source::duckdb::ReloadWorkspace::contains_file(path) {
                        return self.restored_conversion(candidate);
                    }
                    if let Err(error) = candidate {
                        saved_conversion_note = Some(format!("re-reading {database} failed ({error}); showing the saved conversion"));
                    }
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            let overridden = self.shapefile_crs.filter(|_| p.to_ascii_lowercase().ends_with(".shp"));
            #[cfg(not(target_arch = "wasm32"))]
            if p.to_ascii_lowercase().ends_with(".fgb") {
                let path = std::path::Path::new(p);
                let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("layer");
                let prepared = match &self.gdal {
                    Some(g) => rustgis_data::source::fgb::FgbSource::prepare_converted_path(path, name, std::path::Path::new(&g.path)),
                    None => rustgis_data::source::fgb::FgbSource::prepare_path(path, name),
                };
                self.conversion_fallback = saved_conversion_note;
                return self.open_fgb(prepared);
            }
            let opened = {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(d) = &self.duckdb {
                    use rustgis_data::source::{duckdb::Database, parquet::ParquetSource};
                    let database = d.database.as_deref().map(Database::parse);
                    let original = match &database {
                        Some(Database::File(path)) => Some(path.as_path()),
                        _ => None,
                    };
                    let path = std::path::Path::new(p);
                    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("layer");
                    ParquetSource::open_converted_path(path, name, original, None).map(|src| Box::new(src) as Box<dyn FeatureSource>)
                } else if let Some(crs) = overridden {
                    let name = std::path::Path::new(p).file_stem().and_then(|s| s.to_str()).unwrap_or("layer");
                    rustgis_data::source::shp::ShpSource::open_path_in(std::path::Path::new(p), name, Some(crs))
                        .map(|src| Box::new(src) as Box<dyn FeatureSource>)
                } else {
                    rustgis_data::source::open_path(std::path::Path::new(p), self.table.as_deref())
                }
                #[cfg(target_arch = "wasm32")]
                rustgis_data::source::open_path(std::path::Path::new(p), self.table.as_deref())
            };
            match (opened, &self.duckdb) {
                // Queries and server origins retain their existing restoration restrictions.
                #[cfg(not(target_arch = "wasm32"))]
                (Err(_), Some(d)) if !std::path::Path::new(p).exists() => match rerun(d) {
                    Ok(fresh) => {
                        use rustgis_data::source::{duckdb::Database, parquet::ParquetSource};
                        let database = d.database.as_deref().map(Database::parse);
                        let original = match &database {
                            Some(Database::File(path)) => Some(path.as_path()),
                            _ => None,
                        };
                        let name = fresh.file_stem().and_then(|s| s.to_str()).unwrap_or("layer");
                        let r = ParquetSource::open_converted_path(&fresh, name, original, None)
                            .map(|src| Box::new(src) as Box<dyn FeatureSource>);
                        if r.is_ok() {
                            self.path = Some(fresh.display().to_string());
                        }
                        r
                    }
                    Err(e) => Err(e),
                },
                (r, _) => r,
            }
        } else {
            return Err("the layer has no source".into());
        };
        match src {
            Ok(s) => {
                self.conversion_info = None;
                self.handle = Some(Arc::from(s));
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.conversion_fallback = saved_conversion_note;
                }
                self.window = None;
                self.clear_error();
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

    /// Keeps header-only opens synchronous and sends only full native date scans to a worker.
    #[cfg(not(target_arch = "wasm32"))]
    fn open_fgb(&mut self, prepared: rustgis_data::Result<rustgis_data::source::fgb::FgbOpen>) -> Result<(), String> {
        self.conversion_info = None;
        self.login = false;
        let prepared = match prepared {
            Ok(p) => p,
            Err(e) => return self.opened(Err(e.to_string())),
        };
        if prepared.needs_scan() {
            self.open_local(move || prepared.finish().map(|source| Box::new(source) as Box<dyn FeatureSource>))
        } else {
            self.opened(prepared.finish().map(|source| Arc::new(source) as Arc<dyn FeatureSource>).map_err(|e| e.to_string()))
        }
    }

    /// Opens a local FlatGeobuf off the frame: date classification scans the complete file.
    /// Copies share its result and Cancel; the initiating authority is checked during the scan.
    #[cfg(not(target_arch = "wasm32"))]
    fn open_local(&mut self, work: impl FnOnce() -> rustgis_data::Result<Box<dyn FeatureSource>> + Send + 'static) -> Result<(), String> {
        let slot = Arc::new_cyclic(|owner| OpeningState {
            result: Mutex::new(None),
            still: rustgis_data::source::remote::current_still(),
            progress: Some(Arc::new(OpeningProgress {
                owner: owner.clone(),
                cancelled: Default::default(),
                done: AtomicU64::new(0),
                total: AtomicU64::new(0),
            })),
        });
        let land = Arc::downgrade(&slot);
        let progress = slot.progress.clone();
        let still = slot.still.clone();
        let capture = rustgis_data::diagnostics::Capture::current();
        let job = move || {
            let _diagnostics = capture.enter();
            let _progress = rustgis_data::progress::install(progress.map(|p| p as Arc<dyn rustgis_data::progress::Probe>));
            let result = rustgis_data::source::remote::with_optional_still(still, || {
                rustgis_data::progress::check()?;
                let source = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                    .unwrap_or_else(|_| Err(rustgis_data::DataError::Format("opening the layer failed unexpectedly".into())))?;
                rustgis_data::progress::check()?;
                Ok(Arc::from(source))
            });
            if let Some(slot) = land.upgrade()
                && let Ok(mut out) = slot.lock()
            {
                *out = Some(result.map_err(|e: rustgis_data::DataError| e.to_string()));
            }
            wake();
        };
        self.handle = None;
        self.clear_error();
        self.reset();
        if let Err(e) = std::thread::Builder::new().name("rustgis-source-open".into()).spawn(job) {
            return self.opened(Err(format!("couldn't start opening the layer: {e}")));
        }
        self.conversion_info = None;
        self.login = false;
        self.opening = Some(slot);
        Ok(())
    }

    /// A local date-classification scan still running: rows read, known total and Cancel state.
    /// Completed hidden layers need no polling or status row until they are displayed.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn opening_progress(&self) -> Option<(u64, u64, bool)> {
        let slot = self.opening.as_ref()?;
        let progress = slot.progress.as_ref()?;
        if slot.lock().ok()?.is_some() {
            return None;
        }
        Some((progress.done.load(Ordering::Relaxed), progress.total.load(Ordering::Relaxed), progress.cancelled.load(Ordering::Relaxed)))
    }

    /// Cancels a local source's schema scan, including source copies sharing that open.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn cancel_opening(&self) {
        if let Some(progress) = self.opening.as_ref().and_then(|slot| slot.progress.as_ref()) {
            progress.cancelled.store(true, Ordering::Relaxed);
            wake();
        }
    }

    /// Opens a source by its web address: from what is fetched of it, else fetching its header
    /// (or footer) off the frame, taken by a later call when it has landed.
    fn open_remote(&mut self, url: &str) -> Result<(), String> {
        if let Some(slot) = self.opening.clone() {
            if let Err(e) = rustgis_data::source::remote::check_still(&slot.still) {
                self.opening = None;
                return self.opened(Err(e));
            }
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
            Err(e) if e.to_string().contains(rustgis_data::source::remote::READ_WHOLE) => self.open_whole(url, OpeningState::new()),
            Err(e) if e.to_string().contains(rustgis_data::source::remote::NEED) => {
                let slot = OpeningState::new();
                let land = slot.clone();
                self.opening = Some(slot);
                rustgis_data::source::open_url(
                    url,
                    Box::new(move |r| {
                        if let Ok(mut s) = land.lock() {
                            *s = Some(r.map(Arc::from).map_err(|e| e.to_string()));
                        }
                        wake();
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
        let still = land.still.clone();
        rustgis_data::source::remote::with_optional_still(still, || {
            rustgis_data::source::remote::fetch_whole(
                &url.clone(),
                Box::new(move |r| {
                    let r = match r {
                        Ok(bytes) => rustgis_data::source::open_shared(&name, Arc::new(bytes), table.as_deref())
                            .map(Arc::from)
                            .map_err(|e| rustgis_data::diagnostics::message_with_address(&url, &e.to_string())),
                        Err(rustgis_data::source::remote::Failed::Other(e)) => {
                            Err(rustgis_data::diagnostics::message_with_address(&url, &e))
                        }
                        Err(e) => Err(rustgis_data::diagnostics::message_with_address(&url, &format!("couldn't be downloaded ({e:?})"))),
                    };
                    if let Ok(mut s) = land.lock() {
                        *s = Some(r);
                    }
                    wake();
                }),
            );
        });
        Ok(())
    }

    fn opened(&mut self, r: Result<Arc<dyn FeatureSource>, String>) -> Result<(), String> {
        match r {
            Ok(s) => {
                self.handle = Some(s);
                self.window = None;
                self.clear_error();
                Ok(())
            }
            Err(e) => {
                self.error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Waits (up to `timeout`) for an asynchronous source open: exports, which draw
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

    /// When `query` (on a layer of `fields`) can filter only a sample of a view, the source's
    /// feature count (`None` when it doesn't say) and the most a view reads. That is when the view
    /// isn't walked with it (a database, GeoJSON or LiDAR read in place, a file by its web address,
    /// or a query using a row number), and the source holds more features than a view reads,
    /// doesn't say how many, or the view read now is already a sample (of more bytes than a view
    /// holds). `None` until open.
    pub fn query_sample(&self, query: &str, fields: &[rustgis_data::Field]) -> Option<(Option<u64>, usize)> {
        let h = self.handle.as_ref()?;
        let most = match self.limit {
            0 if h.remote().is_some() => REMOTE_VIEW_LIMIT,
            0 => VIEW_LIMIT,
            n => n,
        };
        let count = h.info().count;
        let query = Query { text: query.to_string(), fields: fields.to_vec(), env: Default::default() };
        let crowded = count.is_none_or(|c| c > most as u64) || self.sampled().is_some();
        (window::walked(h.as_ref(), &query).is_none() && crowded).then_some((count, most))
    }

    pub fn info(&self) -> Option<&SourceInfo> {
        self.conversion_info.as_ref().or_else(|| self.handle.as_deref().map(|h| h.info()))
    }

    /// Takes the original-read failure once, only when a saved conversion opened instead.
    pub fn take_conversion_fallback(&mut self) -> Option<String> {
        self.conversion_fallback.take()
    }

    /// Takes diagnostics only from a fresh conversion performed by [`Self::open`].
    pub fn take_conversion_notes(&mut self) -> rustgis_data::loss::Notes {
        std::mem::take(&mut self.conversion_notes_unsaid)
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

    /// Suspends automatic reads without changing the serialized source definition or any password.
    pub fn require_reconnect(&mut self) {
        self.reconnect_required = true;
        self.handle = None;
        self.opening = None;
        self.pending = None;
        self.error = Some(RECONNECT_REQUIRED.into());
    }

    /// Whether the source still needs its owner to authorize a reconnect.
    pub fn reconnect_required(&self) -> bool {
        self.reconnect_required
    }

    /// Releases a source after its owner explicitly reconnects or validates exact peer retention.
    pub fn allow_reconnect(&mut self) {
        self.reconnect_required = false;
    }

    /// Opens the source afresh, forgetting a failed or never-started open (and the window read
    /// from the old one): a source by its web address read again once what signs it has changed
    /// (its address, or the login behind it). Its symbology and place in the project stay.
    pub fn reopen(&mut self) -> Result<(), String> {
        self.handle = None;
        self.opening = None;
        self.pending = None;
        self.clear_error();
        self.login = false;
        // A new opening may hold other values: what was said of the last one doesn't hold.
        self.unread_dates = Tally::default();
        self.rounded_dates = Tally::default();
        self.unread_dates_said = false;
        self.rounded_dates_said = false;
        self.null_text = Tally::default();
        self.null_text_said = false;
        self.not_utf8 = Tally::default();
        self.not_utf8_in = None;
        self.not_utf8_said = false;
        self.forget_read_notes();
        self.reset();
        self.open()
    }

    /// Leaves a read under way to the layer this one was copied from, which takes it when it
    /// lands: a copy drawn elsewhere (an export on a thread of its own) reads what it needs for
    /// itself. The two would otherwise share the read, and the one that didn't take it would wait
    /// for it forever. The copy decides its thinning by itself too, so the map's redraws never
    /// wait for the export's.
    pub fn detach_read(&mut self) {
        self.pending = None;
        self.render_lod = Default::default();
    }

    /// Whether `other` (a copy of this one) reads the same source the same way: the same file and
    /// table, opened by the same open. One opened afresh since (`reopen`, a table replaced) is
    /// another, whatever its path.
    pub fn same_opening(&self, other: &SourceLayer) -> bool {
        let handles = match (&self.handle, &other.handle) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        // A source by its web address still opening: the same open shares its slot.
        let openings = match (&self.opening, &other.opening) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        handles && openings && self.path == other.path && self.table == other.table
    }

    /// Forgets the window, so the next view reads again (after the source changes).
    pub fn reset(&mut self) {
        self.render_lod = Default::default();
        self.reset_window();
    }

    /// Reads the view again, keeping the selection parked for the next read.
    pub(crate) fn reread(&mut self) {
        let selected = std::mem::take(&mut self.saved_selection);
        self.reset_window();
        self.saved_selection = selected;
    }

    /// Applies the layer's definition query (`fields` are the layer's) as views are read; a
    /// different one reads the view again.
    pub(crate) fn set_query(&mut self, text: Option<&str>, fields: &[rustgis_data::Field], env: &rustgis_data::query::Env) {
        let text = text.map(str::trim).filter(|t| !t.is_empty());
        let page = env.page.as_ref().filter(|_| text.is_some_and(rustgis_data::query::reads_series_page));
        if self.query.as_ref().map(|q| (q.text.as_str(), q.fields.as_slice(), q.env.page.as_ref())) != text.map(|t| (t, fields, page)) {
            self.query = text.map(|t| Query {
                text: t.to_string(),
                fields: fields.to_vec(),
                env: rustgis_data::query::Env { page: page.cloned(), ..Default::default() },
            });
            // A source that can't be walked reads the same rows whatever the query.
            if self.handle.as_ref().is_some_and(|h| h.visits_chunks()) {
                self.reread();
            }
        }
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
        let query = self.query.clone();
        let still = rustgis_data::source::remote::current_still();
        let pending = Arc::new(Pending { extent, result: Mutex::new(None), still: still.clone() });
        let p = pending.clone();
        // A source by its web address: the read runs as its bytes land (on the desktop and in
        // the browser alike), fetching what it needs in rounds.
        if let Some(file) = h.remote() {
            let (h2, p2) = (h.clone(), p.clone());
            let work = Arc::new(move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    window::read_mode(h2.as_ref(), &req, budget, &projection, estimate, full_read, fields, query.as_ref())
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
                    wake();
                }),
            );
            self.pending = Some(pending);
            let _ = block;
            return;
        }
        let job = move || {
            rustgis_data::source::remote::with_optional_still(still.clone(), || {
                let r = rustgis_data::source::remote::check_still(&still)
                    .and_then(|()| {
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            window::read_mode(h.as_ref(), &req, budget, &projection, estimate, full_read, fields, query.as_ref())
                        }))
                        .unwrap_or_else(|_| Err(rustgis_data::DataError::Format("the read failed unexpectedly".into())))
                        .map_err(|e| e.to_string())
                    })
                    .and_then(|r| rustgis_data::source::remote::check_still(&still).map(|()| r));
                if let Ok(mut slot) = p.result.lock() {
                    *slot = Some(r);
                }
            })
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
                    self.window = Some(Window { extent, complete: true, matched: 0, kept: 0, filtered: false });
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
            let done = p.result.lock().ok().and_then(|mut r| r.take()).map(|r| rustgis_data::source::remote::check_still(&p.still).and(r));
            match done {
                Some(Ok((b, cache))) => {
                    self.pending = None;
                    self.estimated_row_bytes = cache.mean_bytes;
                    let filtered = cache.filtered;
                    self.ready_cache = Some(cache);
                    let w = Window { extent: p.extent, complete: b.complete, matched: b.matched, kept: b.features.len(), filtered };
                    self.window = Some(w);
                    self.clear_error();
                    return Polled::Done(w, b);
                }
                Some(Err(e)) => {
                    self.pending = None;
                    self.ready_cache = None;
                    self.saved_selection.clear();
                    self.error = Some(e.clone());
                    // Not read again until the view moves past it.
                    self.window = Some(Window { extent: p.extent, complete: true, matched: 0, kept: 0, filtered: false });
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

    /// Whether the view read kept the definition query's matches, so [`SourceLayer::sampled`]
    /// counts matches rather than every feature in view.
    pub fn sampled_matches(&self) -> bool {
        self.window.as_ref().is_some_and(|w| w.filtered)
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

/// Something read in the background (a feature read, a source opened by its address, a raster's
/// pixels read in place; outside this module, a Zarr step) has landed: the map redraws
/// ([`generation`]) and the UI is woken.
pub fn wake() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
    if let Some(w) = waker().lock().ok().and_then(|w| w.clone()) {
        w();
    }
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

impl SourceLayer {
    /// Persisted source address and local driver paths; handles and embedded data are opaque.
    pub(crate) fn output_slots(&self) -> Vec<(crate::LayerRole, &str, bool, bool)> {
        let SourceLayer {
            path,
            file_name: _,
            table: _,
            shapefile_crs: _,
            embedded,
            handle: _,
            window: _,
            render_lod: _,
            pending: _,
            projection: _,
            ready_cache: _,
            byte_budget: _,
            estimated_row_bytes: _,
            full_read: _,
            saved_selection: _,
            grids: _,
            window_fields: _,
            query: _,
            opening: _,
            error: _,
            error_said: _,
            login: _,
            reconnect_required: _,
            limit: _,
            reads: _,
            display_read_said: _,
            area_counts: _,
            area_features: _,
            conversion_info: _,
            conversion_fallback: _,
            conversion_notes_unsaid: _,
            unread_dates: _,
            rounded_dates: _,
            unread_dates_said: _,
            rounded_dates_said: _,
            null_text: _,
            null_text_said: _,
            not_utf8: _,
            not_utf8_in: _,
            not_utf8_said: _,
            read_notes: _,
            read_note_kinds: _,
            gdal,
            duckdb,
            postgis,
            mysql,
            sqlserver,
        } = self;
        use crate::{LayerRole as R, paths};
        let mut out = Vec::new();
        if let Some(p) = path {
            out.push((
                R::Streamed,
                p.as_str(),
                true,
                embedded.is_none()
                    && gdal.is_none()
                    && duckdb.is_none()
                    && postgis.is_none()
                    && mysql.is_none()
                    && sqlserver.is_none()
                    && paths::is_local(p),
            ));
        }
        if let Some(g) = gdal {
            out.push((R::GdalPath, g.path.as_str(), false, paths::is_local(&g.path)));
        }
        if let Some(d) = duckdb {
            if let Some(p) = &d.database {
                out.push((R::DuckDbDatabase, p.as_str(), true, paths::is_local(p)));
            }
            if let Some(sql) = &d.sql {
                out.push((R::DuckDbSql, sql.as_str(), true, false));
            }
        }
        if let Some(pg) = postgis {
            out.push((R::PostgisDatabase, pg.database.as_str(), true, false));
        }
        if let Some(my) = mysql {
            out.push((R::MysqlDatabase, my.database.as_str(), true, false));
        }
        if let Some(ms) = sqlserver {
            out.push((R::SqlServerDatabase, ms.database.as_str(), true, false));
        }
        out
    }

    /// Resolves a prepared field without examining its written contents.
    pub(crate) fn output_slot_mut(&mut self, role: crate::LayerRole) -> Option<&mut String> {
        match role {
            crate::LayerRole::Streamed => self.path.as_mut(),
            crate::LayerRole::GdalPath => Some(&mut self.gdal.as_mut()?.path),
            crate::LayerRole::DuckDbDatabase => self.duckdb.as_mut()?.database.as_mut(),
            crate::LayerRole::DuckDbSql => self.duckdb.as_mut()?.sql.as_mut(),
            crate::LayerRole::PostgisDatabase => Some(&mut self.postgis.as_mut()?.database),
            crate::LayerRole::MysqlDatabase => Some(&mut self.mysql.as_mut()?.database),
            crate::LayerRole::SqlServerDatabase => Some(&mut self.sqlserver.as_mut()?.database),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn whole_layer_report_keeps_effective_conversion_notes_and_dimensions() {
        use rustgis_data::{Dataset, GeometryType};
        let mut original = Dataset::new("converted", GeometryType::Point, Vec::new());
        original.push(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(1.0, 2.0))), Vec::new());
        let bytes = std::sync::Arc::new(rustgis_data::source::fgb::write(&original).unwrap());
        let handle = rustgis_data::source::open_shared("converted.fgb", bytes.clone(), None).unwrap();
        let mut source = super::SourceLayer::from_bytes("converted.fgb", bytes, None, handle);
        let mut info = source.info().unwrap().clone();
        info.notes.push("conversion left out 2 source features");
        info.zm_dropped = true;
        source.conversion_info = Some(info);
        let mut layer = crate::Layer::new(1, Dataset::new("layer", GeometryType::Point, Vec::new()));
        layer.source = Some(source);
        let (dataset, report) = layer.load_all_reported().unwrap();
        assert!(dataset.zm_dropped);
        assert_eq!(report.messages.iter().filter(|n| n.contains("conversion left out 2 source features")).count(), 1);
        assert_eq!(dataset.features.len(), 1);
        let (_, counted) = layer.load_all_counted().unwrap();
        assert_eq!(counted.messages.iter().filter(|n| n.contains("conversion left out 2 source features")).count(), 1);
        assert!(!counted.zm_noted, "a flag with no Z/M diagnostic cannot suppress the export warning");
    }

    /// Project-restored disk and embedded date sources schedule their classification, share
    /// the pending open across copies, and expose typed dates only after it lands.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn restored_flatgeobuf_dates_open_off_the_frame_and_share_the_result() {
        use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
        let mut ds = Dataset::new("dates", GeometryType::Point, vec![Field::new("issued", FieldType::Date)]);
        ds.push(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.0, 0.0))), vec![Value::parse("2026-03-05", FieldType::Date)]);
        let bytes = std::sync::Arc::new(rustgis_data::source::fgb::write(&ds).unwrap());
        let path = std::env::temp_dir().join(format!("rustgis-date-open-{}.fgb", std::process::id()));
        std::fs::write(&path, bytes.as_slice()).unwrap();
        for mut source in [
            super::SourceLayer { path: Some(path.display().to_string()), ..Default::default() },
            super::SourceLayer { file_name: Some("dates.fgb".into()), embedded: Some(bytes), ..Default::default() },
        ] {
            source.open().unwrap();
            assert!(!source.is_open() && source.is_pending());
            let mut copy = source.clone();
            source.wait_open(std::time::Duration::from_secs(5));
            copy.wait_open(std::time::Duration::from_secs(5));
            assert!(source.is_open(), "{:?}", source.error);
            assert_eq!(source.info().unwrap().fields[0].ty, FieldType::Date);
            assert!(source.same_opening(&copy));
            // A direct reopen also discards the previous handle while its new scan runs.
            source.open().unwrap();
            assert!(!source.is_open() && source.is_pending());
            source.wait_open(std::time::Duration::from_secs(5));
            assert!(source.is_open(), "{:?}", source.error);
            assert!(!source.same_opening(&copy));
        }
        // Repair and Reload need a finished, probed candidate before changing the layer.
        let old = super::SourceLayer { path: Some("missing.fgb".into()), ..Default::default() };
        let repaired = old.relinked(path.to_str().unwrap(), None).unwrap();
        assert!(repaired.is_open() && !repaired.is_pending());
        assert_eq!(repaired.info().unwrap().fields[0].ty, FieldType::Date);
        std::fs::remove_file(path).unwrap();
    }

    /// Unread dates and rounded date-times are each said once, and saying one doesn't silence the
    /// other, whichever a view read finds first.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn unread_and_rounded_date_notices_are_said_independently() {
        use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
        let mut ds = Dataset::new("dates", GeometryType::Point, vec![Field::new("at", FieldType::DateTime)]);
        ds.push(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.0, 0.0))), vec![Value::Null]);
        let bytes = std::sync::Arc::new(rustgis_data::source::fgb::write(&ds).unwrap());
        for unread_first in [true, false] {
            let mut source =
                super::SourceLayer { file_name: Some("dates.fgb".into()), embedded: Some(bytes.clone()), ..Default::default() };
            source.open().unwrap();
            source.wait_open(std::time::Duration::from_secs(5));
            assert!(source.is_open(), "{:?}", source.error);
            let (first, second): (&[usize], &[usize]) = if unread_first { (&[1], &[]) } else { (&[], &[1]) };
            let window = rustgis_geom::Extent::new(0.0, 0.0, 1.0, 1.0);
            source.saw_unread_dates(&window, first, second);
            let said = source.take_unread_dates();
            assert_eq!((said.0.is_some(), said.1.is_some()), (unread_first, !unread_first));
            // The other kind, found by a later read, is still said; the said kind isn't again.
            source.saw_unread_dates(&window, &[2], &[2]);
            let said = source.take_unread_dates();
            assert_eq!((said.0.is_some(), said.1.is_some()), (!unread_first, unread_first), "unread first: {unread_first}");
            assert_eq!(source.take_unread_dates(), (None, None));
        }
    }

    /// A window read again replaces its counts; another window adds to them; a clean re-read
    /// clears its own; past the windows kept apart, counts add up and nothing panics.
    #[test]
    fn a_window_read_again_replaces_its_counts() {
        use super::{Tally, WINDOWS_COUNTED};
        use rustgis_geom::Extent;
        let (a, b) = (Extent::new(0.0, 0.0, 1.0, 1.0), Extent::new(2.0, 0.0, 3.0, 1.0));
        let mut tally = Tally::default();
        assert_eq!(tally.total(), None);
        tally.saw(&a, &[0, 3]);
        tally.saw(&a, &[0, 3]);
        assert_eq!(tally.total(), Some(vec![0, 3]));
        tally.saw(&b, &[1]);
        assert_eq!(tally.total(), Some(vec![1, 3]));
        tally.saw(&a, &[]);
        assert_eq!(tally.total(), Some(vec![1]));
        tally.saw(&b, &[0]);
        assert_eq!(tally.total(), None);
        let nan = Extent::new(f64::NAN, 0.0, 1.0, 1.0);
        for _ in 0..2 * WINDOWS_COUNTED {
            tally.saw(&nan, &[1]);
        }
        assert_eq!(tally.total(), Some(vec![2 * WINDOWS_COUNTED]));
        assert!(tally.windows.len() <= WINDOWS_COUNTED);
        // The same through the layer: a text kind said once, a re-read of its window not counted again.
        let mut source = super::SourceLayer::default();
        source.saw_not_utf8(&a, &[1]);
        source.saw_not_utf8(&a, &[1]);
        source.saw_null_text(&a, &[2]);
        source.saw_null_text(&a, &[2]);
        assert_eq!(std::mem::take(&mut source.not_utf8).total(), Some(vec![1]));
        assert_eq!(std::mem::take(&mut source.null_text).total(), Some(vec![2]));
        source.saw_unread_dates(&a, &[1], &[1]);
        source.saw_unread_dates(&a, &[1], &[1]);
        assert_eq!(std::mem::take(&mut source.unread_dates).total(), Some(vec![1]));
        assert_eq!(std::mem::take(&mut source.rounded_dates).total(), Some(vec![1]));
    }

    /// A view read's notes are said once for each kind, whatever count a later read of another
    /// sample gives; a new kind is still said; too many kinds end in one note saying so; a source
    /// opened anew says them again.
    #[test]
    fn view_read_notes_are_said_once_for_each_kind() {
        let mut source = super::SourceLayer::default();
        let note = |n: usize| {
            rustgis_data::loss::Note::plain(format!("{n} of the shapes read (a sample of the layer, which may hold more) have a NaN"))
        };
        source.saw_read_notes(&[note(3), "".into(), " ".into()]);
        source.saw_read_notes(&[note(5)]);
        assert_eq!(source.take_read_notes(), [note(3)]);
        source.saw_read_notes(&[note(12), "7 values cut".into()]);
        assert_eq!(source.take_read_notes(), ["7 values cut"]);
        assert!(source.take_read_notes().is_empty());
        // A source with endless kinds of note says the first ones, then one note that there are more.
        let many: Vec<rustgis_data::loss::Note> = (0..2 * super::READ_NOTE_KINDS)
            .map(|k| format!("kind {}: 1", char::from(b'a' + (k % 26) as u8).to_string().repeat(k / 26 + 1)).into())
            .collect();
        source.saw_read_notes(&many);
        source.saw_read_notes(&many);
        let said = source.take_read_notes();
        assert_eq!(said.len(), super::READ_NOTE_KINDS - 1, "{said:?}");
        assert!(said.last().is_some_and(|n| n.contains("Load All Features lists them all")), "{said:?}");
        source.forget_read_notes();
        source.saw_read_notes(&[note(4)]);
        assert_eq!(source.take_read_notes(), [note(4)]);
    }

    /// A note that reports a loss isn't folded into a plain one that reads the same: its code
    /// is said too, once.
    #[test]
    fn a_coded_read_note_is_its_own_kind() {
        use rustgis_data::loss::{LossCode, Note};
        let mut source = super::SourceLayer::default();
        let plain = Note::plain("2 rings left out");
        let coded = Note::loss(LossCode::DroppedRing, "3 rings left out");
        source.saw_read_notes(&[plain.clone(), coded.clone(), coded.clone()]);
        assert_eq!(source.take_read_notes(), [plain, coded]);
    }

    /// Only counts make two notes one kind: notes differing in another number stay apart.
    #[test]
    fn a_read_note_kind_folds_counts_only() {
        let mut source = super::SourceLayer::default();
        let epsg = |code: u32| format!("rows in EPSG {code} are read as longitude/latitude");
        let column = |at: u32| format!("values cut in column {at}");
        let notes = [
            "3 of the shapes read have a NaN".to_string(),
            "5 of the shapes read have a NaN".into(),
            "roads: so far, 2 values cut (4 fields)".into(),
            "roads: so far, 7 values cut (1 fields)".into(),
            epsg(4326),
            epsg(3857),
            column(2),
            column(3),
            "EPSG:4326: 3 shape(s) left out".into(),
            "EPSG:3857: 3 shape(s) left out".into(),
        ];
        let notes = notes.map(rustgis_data::loss::Note::plain);
        source.saw_read_notes(&notes);
        let said = source.take_read_notes();
        let expected = [&notes[0], &notes[2], &notes[4], &notes[5], &notes[6], &notes[7], &notes[8], &notes[9]];
        assert_eq!(said.iter().collect::<Vec<_>>(), expected);
        assert_eq!(super::note_kind("12 of 40 read; 3 left"), "# of 40 read; # left");
    }

    /// A layer brought back by undo keeps what its live opening said; another opening keeps its own.
    #[test]
    fn a_restored_layer_keeps_the_live_openings_read_notes() {
        let mut live = super::SourceLayer::default();
        live.saw_read_notes(&["2 shape(s) left out".into()]);
        let mut snapshot = live.clone();
        snapshot.hold_read_notes();
        assert!(snapshot.take_read_notes().is_empty(), "the snapshot leaves the unsaid note to the live layer");
        assert_eq!(live.take_read_notes(), ["2 shape(s) left out"]);
        let mut restored = live.clone();
        restored.forget_read_notes();
        restored.keep_read_notes(&live);
        restored.saw_read_notes(&["4 shape(s) left out".into()]);
        assert!(restored.take_read_notes().is_empty(), "said by the live layer");
        let other = super::SourceLayer { path: Some("other.shp".into()), ..Default::default() };
        let mut kept = other.clone();
        kept.keep_read_notes(&live);
        kept.saw_read_notes(&["4 shape(s) left out".into()]);
        assert_eq!(kept.take_read_notes(), ["4 shape(s) left out"], "another opening");
    }

    /// A paused schema worker reports real progress; Cancel wins for every source copy and
    /// prevents publishing its handle. Channels keep the cancellation boundary deterministic.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn local_schema_open_progress_and_cancel_are_shared() {
        let (started, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let mut source = super::SourceLayer::default();
        source
            .open_local(move || {
                rustgis_data::progress::begin(2);
                rustgis_data::progress::step(1)?;
                started.send(()).map_err(|_| rustgis_data::DataError::Cancelled)?;
                wait.recv().map_err(|_| rustgis_data::DataError::Cancelled)?;
                rustgis_data::progress::step(1)?;
                Err(rustgis_data::DataError::Format("cancel must stop before this result".into()))
            })
            .unwrap();
        ready.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        let mut copy = source.clone();
        assert_eq!(source.opening_progress(), Some((1, 2, false)));
        copy.cancel_opening();
        release.send(()).unwrap();
        source.wait_open(std::time::Duration::from_secs(5));
        copy.wait_open(std::time::Duration::from_secs(5));
        for source in [&source, &copy] {
            assert!(!source.is_open() && !source.is_pending());
            assert!(source.error.as_deref().unwrap().contains("cancelled"));
        }
    }

    #[test]
    fn conversion_fallback_is_runtime_only_and_taken_once() {
        let note = "re-reading original failed (missing); showing the saved conversion";
        let mut source = super::SourceLayer { conversion_fallback: Some(note.into()), ..Default::default() };
        let saved = serde_json::to_value(&source).unwrap();
        assert!(saved.get("conversion_fallback").is_none());
        let mut restored: super::SourceLayer = serde_json::from_value(saved).unwrap();
        assert!(restored.take_conversion_fallback().is_none());
        assert_eq!(source.take_conversion_fallback().as_deref(), Some(note));
        assert!(source.take_conversion_fallback().is_none());
    }

    #[test]
    fn opening_conversion_notes_are_runtime_only_and_taken_once() {
        let note = "GDAL: 2 features of another geometry type were left out";
        let mut source = super::SourceLayer { conversion_notes_unsaid: [note].into_iter().collect(), ..Default::default() };
        let saved = serde_json::to_value(&source).unwrap();
        assert!(saved.get("conversion_notes_unsaid").is_none());
        let mut restored: super::SourceLayer = serde_json::from_value(saved).unwrap();
        assert!(restored.take_conversion_notes().is_empty());
        assert_eq!(source.take_conversion_notes(), vec![note]);
        assert!(source.take_conversion_notes().is_empty());
    }

    /// A later frame's whole-file fallback owns the opening's carrier through send and landing.
    #[test]
    fn permission_policy_opening_fallback_retains_owner_outside_ambient_context() {
        use rustgis_data::source::remote;
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        for (owned, revoke) in [(true, false), (true, true), (false, false)] {
            let live = Arc::new(AtomicBool::new(true));
            let check = live.clone();
            let still = owned.then(|| Arc::new(move || check.load(Ordering::SeqCst)) as remote::Still);
            let held = Arc::new(Mutex::new(None));
            let hold = held.clone();
            let sends = Arc::new(AtomicUsize::new(0));
            let count = sends.clone();
            let ranges = Arc::new(AtomicUsize::new(0));
            let range_count = ranges.clone();
            let url = format!("https://opening-fallback.invalid/{owned}-{revoke}.fgb");
            remote::forget_file(&url);
            remote::set_fetcher(move |_, want, done| match want {
                remote::Want::Whole => {
                    let context = remote::current_still();
                    assert_eq!(context.is_some(), owned, "the actual later-frame fallback must re-enter its saved carrier");
                    *hold.lock().unwrap() = Some((context, done));
                }
                _ => {
                    range_count.fetch_add(1, Ordering::SeqCst);
                    done(Err(remote::Failed::NoRanges));
                }
            });
            let mut source = super::SourceLayer { path: Some(url.clone()), ..Default::default() };
            remote::with_optional_still(still, || source.open()).unwrap();
            assert_eq!(ranges.load(Ordering::SeqCst), 1, "actual opening tried a range");
            assert!(source.opening.is_some());
            assert!(remote::current_still().is_none());
            // The next frame starts outside the original synchronous scope.
            source.open().unwrap();
            let (context, done) = held.lock().unwrap().take().unwrap();
            live.store(!revoke, Ordering::SeqCst);
            let mut ds = rustgis_data::Dataset::new("fallback", rustgis_data::GeometryType::Point, vec![]);
            ds.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0., 0.))), vec![]);
            let result = remote::check_still(&context).map_err(remote::Failed::Other).map(|()| {
                count.fetch_add(1, Ordering::SeqCst);
                rustgis_data::source::fgb::write(&ds).unwrap()
            });
            done(result);
            assert_eq!(source.open().is_err(), revoke);
            assert_eq!(source.is_open(), !revoke, "a revoked landing must never install the source");
            assert_eq!(sends.load(Ordering::SeqCst), usize::from(!revoke));
            assert!(remote::current_still().is_none());
            remote::forget_file(&url);
        }
    }

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

    /// Clearing a non-finite source cell is a write-back update even though output JSON uses Null.
    #[test]
    fn clearing_nonfinite_source_values_is_a_row_update() {
        use rustgis_data::{Feature, Value};
        for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let loaded = Feature { id: 1, geometry: None, attrs: vec![Value::Float(number)] };
            let versions: super::Versions = [(1, super::RowVersion { version: 7, hash: super::row_hash(&loaded) })].into_iter().collect();
            assert!(versions.changes(std::slice::from_ref(&loaded)).is_empty());
            let cleared = Feature { attrs: vec![Value::Null], ..loaded };
            let changes = versions.changes(std::slice::from_ref(&cleared));
            assert_eq!(changes.updates.iter().map(|(feature, version)| (feature.id, *version)).collect::<Vec<_>>(), [(1, 7)]);
            assert_eq!(changes.updates[0].0.attrs, [Value::Null]);
        }
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
