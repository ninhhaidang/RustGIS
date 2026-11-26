//! GeoPackage: an SQLite database of feature tables, read with SQLite (desktop only).
//!
//! Extent queries use the table's R-tree spatial index (`rtree_<table>_<column>`) when it has one,
//! and otherwise scan, testing the envelope in each geometry's header before decoding it.

use super::gpkg_blob::{field_type, is_empty_blob, is_whole_empty_blob, split_blob};
use super::{Batch, FeatureSource, Reproject, Request, Sampler, SourceInfo, clamp_limit, stride};
use crate::raster::Crs;
use crate::{DataError, Feature, Field, FieldType, GeometryType, Result, Value, wkb};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params_from_iter};
use rustgis_geom::Extent;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

mod add;
pub mod edit;

pub use crate::io::{GpkgMode, GpkgTable, GpkgWritten};

/// Rows fetched by id per query.
const CHUNK: usize = 500;

/// What a view read through the R-tree fetches a table's rows by, so they come in the order a
/// scan of the table gives them.
#[derive(Clone, Copy)]
enum RowOrder {
    /// The key, which is the rowid (a GeoPackage's `INTEGER PRIMARY KEY`).
    Key,
    /// The rowid, by this name, where the key is apart from it (declared `INT`, say).
    Rowid(&'static str),
    /// Neither: no rowid, or columns have taken all its names. A view is read by a scan.
    Scan,
}

/// A feature table in a GeoPackage.
pub struct GpkgSource {
    info: SourceInfo,
    /// `None` only while the source is dropped.
    conn: Mutex<Option<Connection>>,
    table: String,
    geom: String,
    pk: Option<String>,
    row_order: RowOrder,
    rtree: Option<String>,
    reproject: Reproject,
    /// A copy written for a file opened from memory, removed when the source is dropped.
    temp: Option<tempfile::NamedTempFile>,
    /// The database file and its identity when opened ([`file_identity`]), for a traversal on a
    /// connection of its own to check it reads the same file.
    file: Option<(PathBuf, Option<(u64, u64)>)>,
    /// Whether `info.count` is the count GDAL keeps ([`kept_count`]) rather than the rows counted:
    /// it may be past them (an `INSERT OR REPLACE` leaves it high, a file may say any number), so
    /// a read doesn't start its sample's step from it.
    count_kept: bool,
}

impl Drop for GpkgSource {
    fn drop(&mut self) {
        // Closed first: Windows can't remove a file SQLite still holds open.
        if let Ok(c) = self.conn.get_mut() {
            drop(c.take());
        }
    }
}

/// The feature tables of a GeoPackage.
pub fn layers(path: &Path) -> Result<Vec<String>> {
    let conn = open_db(path)?;
    feature_tables(&conn)
}

/// Unsupported contents omitted when the GeoPackage is added as feature layers, by type.
pub fn unsupported_table_counts(path: &Path) -> Result<Vec<(String, u64)>> {
    unsupported_counts(&open_db(path)?)
}

/// Counts on the same connection used to open an uploaded feature table.
fn unsupported_counts(conn: &Connection) -> Result<Vec<(String, u64)>> {
    let invalid: i64 = conn
        .query_row("SELECT count(*) FROM gpkg_contents WHERE typeof(table_name) <> 'text' OR typeof(data_type) <> 'text'", [], |row| {
            row.get(0)
        })
        .map_err(not_gpkg)?;
    if invalid > 0 {
        return Err(DataError::Format(format!(
            "GeoPackage: {invalid} invalid gpkg_contents entry/entries: table_name and data_type must be text"
        )));
    }
    let registered_filter = if has_geometry_columns(conn)? {
        "WHERE lower(table_name) NOT IN (SELECT lower(table_name) FROM gpkg_geometry_columns WHERE table_name IS NOT NULL)"
    } else {
        ""
    };
    let query = format!(
        "SELECT CASE WHEN data_type COLLATE NOCASE IN ('attributes','features','tiles','2d-gridded-coverage') THEN lower(data_type) ELSE 'other' END AS kind, count(*) FROM gpkg_contents {registered_filter} GROUP BY 1 ORDER BY 1"
    );
    let mut st = conn.prepare(&query).map_err(not_gpkg)?;
    st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
        .map_err(err)?
        .map(|r| {
            let (kind, count) = r.map_err(err)?;
            Ok((kind, u64::try_from(count).map_err(|_| DataError::Format("GeoPackage: invalid table count".into()))?))
        })
        .collect()
}

/// Count a desktop upload with SQLite, matching the reader used to open its feature table.
pub fn unsupported_table_counts_bytes(bytes: &[u8]) -> Result<Vec<(String, u64)>> {
    GpkgUpload::new(bytes)?.unsupported_table_counts()
}

/// One private desktop copy and connection, shared by upload inspection and opening.
/// The connection closes before the temporary file is removed, including on Windows.
pub struct GpkgUpload {
    conn: Connection,
    copy: tempfile::NamedTempFile,
}

impl GpkgUpload {
    /// Writes the upload once; inspection and the eventual source use this same database.
    pub fn new(bytes: &[u8]) -> Result<Self> {
        use std::io::Write;
        let mut copy = tempfile::Builder::new().prefix("rustgis-").suffix(".gpkg").tempfile()?;
        copy.write_all(bytes)?;
        copy.flush()?;
        let conn = crate::provenance::without(|| open_db(copy.path()))?;
        Ok(Self { conn, copy })
    }

    pub fn unsupported_table_counts(&self) -> Result<Vec<(String, u64)>> {
        unsupported_counts(&self.conn)
    }

    /// Transfers the connection and its private copy to the selected feature source.
    pub fn open(self, layer: Option<&str>) -> Result<GpkgSource> {
        let Self { conn, copy } = self;
        let path = copy.path().to_path_buf();
        let mut source = GpkgSource::open_conn(conn, &path, layer, file_identity(&path))?;
        source.temp = Some(copy);
        Ok(source)
    }
}

fn open_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).map_err(err)?;
    crate::provenance::sqlite(path);
    // A cap on SQLite's page cache, so a huge database can't take all the memory; and the file is
    // hostile: nothing it defines (views, triggers, generated columns) may call functions, and
    // nothing writes.
    let _ = conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false);
    let _ = conn.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true);
    let _ = conn.execute_batch("PRAGMA cache_size = -65536; PRAGMA query_only = ON;");
    Ok(conn)
}

/// Attribute-only and tile-only packages need not declare any geometry columns.
fn has_geometry_columns(conn: &Connection) -> Result<bool> {
    has_table(conn, "gpkg_geometry_columns")
}

fn has_table(conn: &Connection, name: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = ?1 COLLATE NOCASE AND type IN ('table','view'))",
        [name],
        |row| row.get(0),
    )
    .map_err(err)
}

/// The feature count GDAL keeps for `table` in `gpkg_ogr_contents`, used as the table's count
/// when the file also has the two triggers that keep it up to date
/// (`trigger_insert_feature_count_<table>` and `trigger_delete_feature_count_<table>`, which add or
/// take one per insert and delete); GDAL answers a table's count with it too, and it is as right
/// as GDAL's: a row an `INSERT OR REPLACE` puts out fires no delete trigger (SQLite's
/// `recursive_triggers` is off by default), so such writes leave it high. A read doesn't start
/// its sample's step from it (`GpkgSource::count_kept`), so a preview still gets the rows there
/// are. GDAL may drop the triggers while it writes and create them again when it saves the file,
/// so a file without them may hold an out-of-date count. `None` then, and when there is no such
/// table or row (a file GDAL didn't write), or when the count isn't a whole number from zero up
/// (NULL, text, a negative): the table is counted instead.
fn kept_count(conn: &Connection, table: &str) -> Option<i64> {
    conn.query_row(
        // The table's row (its own name first, then one differing in case), then whether its count holds.
        "SELECT feature_count FROM (SELECT feature_count FROM gpkg_ogr_contents WHERE lower(table_name) = lower(?1)
                                    ORDER BY table_name = ?1 DESC LIMIT 1)
         WHERE typeof(feature_count) = 'integer' AND feature_count >= 0
           AND (SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND lower(tbl_name) = lower(?1) AND lower(name) IN
                (lower('trigger_insert_feature_count_' || ?1), lower('trigger_delete_feature_count_' || ?1))) = 2",
        [table],
        |r| r.get(0),
    )
    .ok()
}

fn feature_tables(conn: &Connection) -> Result<Vec<String>> {
    if !has_geometry_columns(conn)? {
        if !has_table(conn, "gpkg_contents")? {
            return Err(DataError::Format("not a GeoPackage: no gpkg_contents table".into()));
        }
        return Ok(Vec::new());
    }
    let mut st = conn.prepare("SELECT table_name FROM gpkg_geometry_columns ORDER BY table_name").map_err(not_gpkg)?;
    let rows = st.query_map([], |r| r.get::<_, String>(0)).map_err(err)?;
    Ok(rows.filter_map(std::result::Result::ok).collect())
}

impl GpkgSource {
    /// The total a scan of the table counts its progress against: none (unknown) for a kept count
    /// (`count_kept`), which may be past the rows.
    fn progress_total(&self) -> usize {
        self.info.count.filter(|_| !self.count_kept).and_then(|n| usize::try_from(n).ok()).unwrap_or(0)
    }

    pub fn open(path: &Path, layer: Option<&str>) -> Result<Self> {
        // The identity is taken before the database is opened: a file put in its place after
        // this leaves it describing the file opened, which a traversal then refuses to swap.
        // Made absolute first, so the file opened and the path kept for a traversal are one even
        // if the working directory changes in between.
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let id = file_identity(&path);
        Self::open_conn(open_db(&path)?, &path, layer, id)
    }

    /// Opens a GeoPackage held in memory, through a temporary copy (SQLite reads files). The copy
    /// is a new file with a name no one can guess, so another user can't plant one in its place.
    pub fn open_bytes(bytes: &[u8], layer: Option<&str>) -> Result<Self> {
        GpkgUpload::new(bytes)?.open(layer)
    }

    /// Opens a feature table (`layer`, else the first) on an open database from its metadata,
    /// noting what reading changes: an unknown coordinate system, other tables, Z or M values
    /// dropped. `id` identifies the file for later traversals. On-disk sources capture it before
    /// opening; an upload identifies its private copy.
    /// `path` is the file as the caller named it, made absolute. SQLite's own name for it resolves symbolic links
    /// (macOS's temporary folder `/var` is `/private/var`), so it isn't the path to record.
    fn open_conn(conn: Connection, path: &Path, layer: Option<&str>, id: Option<(u64, u64)>) -> Result<Self> {
        let tables = feature_tables(&conn)?;
        let table = match layer {
            Some(l) => tables
                .iter()
                .find(|t| t.eq_ignore_ascii_case(l))
                .cloned()
                .ok_or_else(|| DataError::Format(format!("no feature table '{l}' (tables: {})", tables.join(", "))))?,
            None => tables.first().cloned().ok_or_else(|| DataError::Format("the GeoPackage has no feature tables".into()))?,
        };
        let mut notes = crate::loss::Notes::new();
        if layer.is_none() && tables.len() > 1 {
            notes.push(format!("the first of {} feature tables ({})", tables.len(), tables.join(", ")));
        }
        let (geom, geom_type, srs_id): (String, String, i64) = conn
            .query_row("SELECT column_name, geometry_type_name, srs_id FROM gpkg_geometry_columns WHERE table_name = ?1", [&table], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(err)?;
        // Its row, read cell by cell as the browser reader reads it: a code stored as a whole REAL
        // (9377.0) is that code, and a column a writer left out (the definition) or a value of
        // another type is that cell missing, never the whole row.
        let row = conn
            .query_row("SELECT * FROM gpkg_spatial_ref_sys WHERE srs_id = ?1", [srs_id], |r| {
                let text = |col: &str| match r.get_ref(col) {
                    Ok(ValueRef::Text(t)) => Some(String::from_utf8_lossy(t).into_owned()),
                    _ => None,
                };
                let code = match r.get_ref("organization_coordsys_id") {
                    Ok(ValueRef::Integer(i)) => Some(i),
                    Ok(ValueRef::Real(x)) if x.fract() == 0.0 && x.abs() < 9e15 => Some(x as i64),
                    _ => None,
                };
                Ok(super::GpkgSrs {
                    organization: text("organization"),
                    code,
                    definition: text("definition"),
                    definition_12_063: text("definition_12_063"),
                })
            })
            .ok();
        let reproject = super::gpkg_reproject(srs_id, row, &mut notes)?;

        // Columns: the primary key, the geometry and the attribute fields.
        let mut pk = None;
        let mut fields = Vec::new();
        {
            let mut st = conn.prepare(&format!("PRAGMA table_info({})", quote(&table))).map_err(err)?;
            let cols = st
                .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?.unwrap_or_default(), r.get::<_, i64>(5)?)))
                .map_err(err)?;
            for c in cols.filter_map(std::result::Result::ok) {
                let (name, decl, is_pk) = c;
                if name.eq_ignore_ascii_case(&geom) {
                    continue;
                }
                if is_pk == 1 && pk.is_none() && decl.to_ascii_uppercase().contains("INT") {
                    pk = Some(name);
                    continue;
                }
                if let Some(ty) = field_type(&decl) {
                    fields.push(Field::new(name, ty));
                }
            }
        }
        let row_order = row_order(&conn, &table).map_err(err)?;
        // Looked up without regard to case, as SQLite names are; the real name is kept.
        let rtree_name = format!("rtree_{table}_{geom}");
        let rtree: Option<String> = conn
            .query_row("SELECT name FROM sqlite_master WHERE lower(name) = lower(?1) AND type = 'table'", [&rtree_name], |r| r.get(0))
            .ok()
            .filter(|_| pk.is_some());
        let declared: Option<Extent> = conn
            .query_row("SELECT min_x, min_y, max_x, max_y FROM gpkg_contents WHERE table_name = ?1", [&table], |r| {
                Ok((r.get::<_, Option<f64>>(0)?, r.get::<_, Option<f64>>(1)?, r.get::<_, Option<f64>>(2)?, r.get::<_, Option<f64>>(3)?))
            })
            .ok()
            .and_then(|e| match e {
                (Some(a), Some(b), Some(c), Some(d)) => Some(Extent::new(a, b, c, d)),
                _ => None,
            });
        let native = declared.filter(|e| !e.is_empty()).or_else(|| {
            let rt = rtree.as_ref()?;
            conn.query_row(&format!("SELECT min(minx), min(miny), max(maxx), max(maxy) FROM {}", quote(rt)), [], |r| {
                Ok(Extent::new(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .ok()
        });
        let extent = native.map(|e| reproject.extent_to_lonlat(&e)).unwrap_or(Extent::EMPTY);
        // Z and M are 0 (none), 1 (every geometry) or 2 (optional: looked for in the first rows).
        let zm: Option<(i64, i64)> = conn
            .query_row("SELECT z, m FROM gpkg_geometry_columns WHERE table_name = ?1 AND column_name = ?2", [&table, &geom], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .ok();
        // Rows: the count GDAL keeps ([`kept_count`]), so a large table isn't scanned to count it
        // (#2849), else counted. With Z or M declared for every geometry, also the rows with one
        // (what the note on them counts), in the same scan. Counting geometries reads every blob,
        // so only then.
        let mandatory = zm.is_some_and(|(z, m)| z == 1 || m == 1);
        let counted = if mandatory { format!("COUNT({})", quote(&geom)) } else { "0".into() };
        let kept = if mandatory { None } else { kept_count(&conn, &table) };
        // The rows, and the geometries when `mandatory`.
        let counts: Option<(i64, Option<i64>)> = match kept {
            Some(n) => Some((n, None)),
            None => conn
                .query_row(&format!("SELECT COUNT(*), {counted} FROM {}", quote(&table)), [], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
                })
                .ok()
                .map(|(n, g)| (n, mandatory.then_some(g))),
        };
        let count = counts.and_then(|(n, _)| u64::try_from(n).ok());
        let geometries = counts.and_then(|(_, g)| g).and_then(|g| u64::try_from(g).ok());
        let geometry_type = match geom_type.to_ascii_uppercase().as_str() {
            "POINT" | "MULTIPOINT" => GeometryType::Point,
            "LINESTRING" | "MULTILINESTRING" | "CURVE" | "MULTICURVE" => GeometryType::Polyline,
            "POLYGON" | "MULTIPOLYGON" | "SURFACE" | "MULTISURFACE" => GeometryType::Polygon,
            _ => GeometryType::None,
        };
        let info = SourceInfo {
            name: table.clone(),
            format: "GeoPackage",
            geometry_type,
            fields,
            extent,
            count,
            // A system the definition gives, by its PROJ definition, so the layer keeps it
            // (`SourceInfo::coordinate_system`), as a Shapefile's .prj does (#1476).
            crs: reproject.info_crs(),
            indexed: rtree.is_some() && !matches!(row_order, RowOrder::Scan),
            notes,
            zm_dropped: false,
        };
        let file = Some((path.to_path_buf(), id));
        let mut src = GpkgSource {
            info,
            conn: Mutex::new(Some(conn)),
            table,
            geom,
            pk,
            row_order,
            rtree,
            reproject,
            temp: None,
            file,
            count_kept: kept.is_some(),
        };
        if src.info.geometry_type == GeometryType::None {
            src.info.geometry_type = src.peek_kind();
        }
        match zm {
            // Every geometry has them.
            Some((z, m)) if z == 1 || m == 1 => src.info.drop_zm(geometries),
            Some((z, m)) if z == 2 || m == 2 => match src.peek_zm() {
                Some(true) => src.info.drop_zm(None),
                Some(false) => {}
                None => src.info.notes.push("couldn't tell whether it has Z or M values, which RustGIS doesn't keep"),
            },
            _ => {}
        }
        Ok(src)
    }

    /// Whether any of the first few geometries has Z or M values (a query that stops there);
    /// `None` when they can't be read.
    fn peek_zm(&self) -> Option<bool> {
        let guard = self.conn.lock().ok()?;
        let conn = guard.as_ref()?;
        let sql = format!("SELECT {g} FROM {t} WHERE {g} IS NOT NULL LIMIT 16", g = quote(&self.geom), t = quote(&self.table));
        let mut st = conn.prepare(&sql).ok()?;
        let mut rows = st.query([]).ok()?;
        while let Some(r) = rows.next().ok()? {
            if let Ok(ValueRef::Blob(b)) = r.get_ref(0)
                && split_blob(b).is_some_and(|(_, body)| wkb::has_zm(body))
            {
                return Some(true);
            }
        }
        Some(false)
    }

    /// The geometry type of a table declared as plain `GEOMETRY`, from its first few rows (a
    /// query that stops there, rather than a read of the table).
    fn peek_kind(&self) -> GeometryType {
        let Ok(guard) = self.conn.lock() else { return GeometryType::None };
        let Some(conn) = guard.as_ref() else { return GeometryType::None };
        let sql = format!("SELECT {g} FROM {t} WHERE {g} IS NOT NULL LIMIT 16", g = quote(&self.geom), t = quote(&self.table));
        let Ok(mut st) = conn.prepare(&sql) else { return GeometryType::None };
        let Ok(mut rows) = st.query([]) else { return GeometryType::None };
        while let Ok(Some(r)) = rows.next() {
            if let Ok(ValueRef::Blob(b)) = r.get_ref(0)
                && let Some((_, body)) = split_blob(b)
                && let Some(g) = wkb::decode(body)
            {
                return g.kind();
            }
        }
        GeometryType::None
    }

    fn select_list(&self) -> String {
        let mut cols = vec![self.pk.as_deref().map(quote).unwrap_or_else(|| "rowid".into()), quote(&self.geom)];
        cols.extend(self.info.fields.iter().map(|f| quote(&f.name)));
        cols.join(", ")
    }

    /// The order a scan of the table gives, written out (` ORDER BY …`) so that SQLite can't read
    /// it another way, such as down a covering index: the order a view read through the R-tree
    /// fetches in.
    fn order_by(&self) -> String {
        match (self.row_order, &self.pk) {
            // The rowid itself, or a name of it no column has taken: no sort.
            (RowOrder::Key, Some(pk)) => format!(" ORDER BY {}", quote(pk)),
            (RowOrder::Rowid(rowid), _) => format!(" ORDER BY {rowid}"),
            // Without a key no view is read through the R-tree, so there's no order to match.
            (RowOrder::Key, None) | (RowOrder::Scan, _) => String::new(),
        }
    }

    /// Turns a row (id, geometry, fields…) into a feature; `None` for an unreadable geometry.
    /// Dates that aren't ones are counted in `unread_dates` ([`Batch::unread_dates`]), date-times
    /// rounded to the microsecond in `rounded_dates`.
    fn feature(
        &self,
        r: &rusqlite::Row<'_>,
        native: Option<&Extent>,
        req: &Request,
        unread_dates: &mut Vec<usize>,
        rounded_dates: &mut Vec<usize>,
    ) -> rusqlite::Result<Row> {
        // Integer keys, negative ones too, map to distinct ids.
        let Some(id) = r.get::<_, Option<i64>>(0)?.map(|i| i as u64) else { return Ok(Row::Bad) };
        let mut attrs = || {
            let mut out = Vec::with_capacity(self.info.fields.len());
            for (k, f) in self.info.fields.iter().enumerate() {
                out.push(match r.get_ref(k + 2).map(|v| value(v, f.ty, unread_dates, Some(&mut *rounded_dates), k)) {
                    Ok(v) => v,
                    // The select list holds a column for every field, so there is no column to miss.
                    Err(_) => Value::Null,
                });
            }
            out
        };
        let blob = match r.get_ref(1)? {
            ValueRef::Blob(b) if !is_whole_empty_blob(b) => b,
            // A row without geometry (NULL, or an empty one) is kept in a whole read; it has no
            // place to match an extent.
            ValueRef::Null | ValueRef::Blob(_) if req.extent.is_none() => {
                return Ok(Row::Feature(Feature { id, geometry: None, attrs: attrs() }));
            }
            ValueRef::Null | ValueRef::Blob(_) => return Ok(Row::Skip),
            _ => return Ok(Row::Bad),
        };
        let Some((envelope, body)) = split_blob(blob) else { return Ok(Row::Bad) };
        if let (Some(q), Some(e)) = (native, envelope)
            && !e.intersects(q)
        {
            return Ok(Row::Skip);
        }
        let Some(placed) = wkb::decode(body).and_then(|g| self.reproject.place(g, req)) else { return Ok(Row::Bad) };
        if !placed.inside {
            return Ok(Row::Skip);
        }
        Ok(Row::Feature(Feature { id, geometry: Some(placed.geometry), attrs: attrs() }))
    }
}

enum Row {
    Feature(Feature),
    /// Outside the request (a row without geometry is, when an extent is asked for).
    Skip,
    /// Unreadable.
    Bad,
}

impl FeatureSource for GpkgSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn reads_own_box(&self) -> bool {
        true
    }

    /// Its own system, in which it gives its features as the file holds them.
    fn coord_crs(&self) -> crate::raster::Crs {
        self.reproject.crs()
    }

    /// Samples native-envelope matches, or decoded view matches when the native box is refused.
    /// Scans count records and check Cancel between them.
    fn read(&self, req: &Request) -> Result<Batch> {
        if self.temp.is_none()
            && let Some((path, id)) = &self.file
            && file_identity(path) == *id
        {
            crate::provenance::sqlite(path);
        }
        let limit = clamp_limit(req.limit);
        let guard = self.conn.lock().map_err(|_| DataError::Format("GeoPackage connection poisoned".into()))?;
        let conn = guard.as_ref().ok_or_else(|| DataError::Format("GeoPackage connection closed".into()))?;
        let native = self.reproject.request_box(req);
        // Preserve ordinary candidate counts; a refused view needs a geometry scan to count.
        if limit == 0 && req.extent.is_some() && native.is_none() {
            return Err(super::unbounded_count());
        }
        let mut out = Batch::default();
        let push = |row: Row, out: &mut Batch| match row {
            Row::Feature(f) => out.features.push(f),
            Row::Skip => {}
            Row::Bad => out.skipped += 1,
        };
        match (native, &self.rtree, &self.pk) {
            // A count alone (`limit` 0) fetches no rows, so it needs no order: the index counts it.
            (Some(e), Some(rt), Some(pk)) if limit == 0 || !matches!(self.row_order, RowOrder::Scan) => {
                // One read, so a write between picking the ids and fetching them (and, for a key
                // apart from the rowid, between looking up their rowids and fetching by them)
                // can't hand back a row that wasn't picked. A read rolls nothing back.
                let _read = conn.unchecked_transaction().map_err(err)?;
                // The index gives the matching ids; an evenly spread sample of them is fetched.
                // Counted first, then streamed keeping every `step`-th, so a huge match never sits
                // in memory as a list of ids.
                let filter = format!("FROM {} WHERE maxx >= ?1 AND minx <= ?2 AND maxy >= ?3 AND miny <= ?4", quote(rt));
                let args = [e.min_x, e.max_x, e.min_y, e.max_y];
                let n: i64 = conn.query_row(&format!("SELECT COUNT(*) {filter}"), args, |r| r.get(0)).map_err(err)?;
                out.matched = u64::try_from(n).unwrap_or(0);
                let step = usize::try_from(stride(out.matched, limit)).unwrap_or(usize::MAX);
                let mut st = conn.prepare(&format!("SELECT id {filter}")).map_err(err)?;
                // A row error (a corrupt page) fails the read rather than truncating it quietly.
                let mut picked: Vec<i64> = st
                    .query_map(args, |r| r.get(0))
                    .map_err(err)?
                    .step_by(step.max(1))
                    .take(limit)
                    .collect::<rusqlite::Result<_>>()
                    .map_err(err)?;
                out.complete = step == 1;
                let table = quote(&self.table);
                // The index lists them in its own order. They are fetched in the table's, as a
                // whole read gives them: the order the map draws them in.
                let by = match self.row_order {
                    RowOrder::Rowid(rowid) => {
                        picked = rowids(conn, &table, rowid, &quote(pk), &picked).map_err(err)?;
                        rowid.to_string()
                    }
                    _ => quote(pk),
                };
                picked.sort_unstable();
                for chunk in picked.chunks(CHUNK) {
                    let marks = vec!["?"; chunk.len()].join(",");
                    let sql = format!("SELECT {} FROM {table} WHERE {by} IN ({marks}) ORDER BY {by}", self.select_list());
                    let mut st = conn.prepare(&sql).map_err(err)?;
                    let mut rows = st.query(params_from_iter(chunk.iter())).map_err(err)?;
                    while let Some(r) = rows.next().map_err(err)? {
                        let row = self.feature(r, native.as_ref(), req, &mut out.unread_dates, &mut out.rounded_dates).map_err(err)?;
                        push(row, &mut out);
                    }
                    if out.features.len() >= limit {
                        out.features.truncate(limit);
                        out.complete = false;
                        break;
                    }
                }
            }
            _ => {
                // A scan: the envelope in each geometry's header tests the extent before decoding.
                let sql = format!("SELECT {} FROM {}{}", self.select_list(), quote(&self.table), self.order_by());
                let mut st = conn.prepare(&sql).map_err(err)?;
                let mut rows = st.query([]).map_err(err)?;
                // A whole read knows its count, unless the file's own; an extent, bounded natively
                // or not, is matched as it goes.
                let mut sample = if req.extent.is_none() && !self.count_kept {
                    Sampler::with_total(limit, self.info.count.unwrap_or(0))
                } else {
                    Sampler::new(limit)
                };
                crate::progress::begin(self.progress_total());
                while let Some(r) = rows.next().map_err(err)? {
                    crate::progress::step(1)?;
                    // With an extent, the envelope in the geometry's header says whether the row
                    // matches before anything is decoded; without an envelope, only the decoded
                    // geometry can, so the match is counted after it.
                    let envelope = match (&native, r.get_ref(1)) {
                        (Some(_), Ok(ValueRef::Blob(b))) => split_blob(b).and_then(|(e, _)| e),
                        _ => None,
                    };
                    if let (Some(q), Some(e)) = (&native, envelope)
                        && !e.intersects(q)
                    {
                        continue;
                    }
                    let known = req.extent.is_none() || (native.is_some() && envelope.is_some());
                    if known && !sample.wants() {
                        continue;
                    }
                    match self.feature(r, native.as_ref(), req, &mut out.unread_dates, &mut out.rounded_dates).map_err(err)? {
                        Row::Feature(f) if known || sample.wants() => sample.keep(f),
                        Row::Feature(_) | Row::Skip => {}
                        Row::Bad => out.skipped += 1,
                    }
                }
                out.matched = sample.seen();
                out.complete = sample.complete();
                out.features = sample.items;
            }
        }
        Ok(out)
    }

    /// Every table in a file is scanned in full, whatever its index.
    fn visits_chunks(&self) -> bool {
        self.file.is_some()
    }

    /// A scan in table order: the envelope in each geometry's header tests the extent before
    /// decoding, as a read without the spatial index does, with the same statement. It runs on a
    /// read-only connection of its own, one statement and so one snapshot of the database. The
    /// map's reads of the layer go on, and the source's connection isn't held while the visitor
    /// runs; another program writing to the file waits for the scan to end, as for any reader
    /// (outside WAL mode). A file put in place of the one opened is refused (where the system
    /// says files' identities); a file removed fails as missing. Counts scanned records and checks
    /// Cancel between them.
    fn visit_chunks(&self, extent: Option<Extent>, bytes: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        let mut chunks = super::chunk_memory::Chunker::new(bytes, visit)?;
        let Some((path, id)) = &self.file else { return Err(DataError::Format("GeoPackage: no file to read".into())) };
        let same_file = || {
            // A missing file is an error of its own, not a replacement.
            std::fs::metadata(path)?;
            if id.is_some() && file_identity(path) != *id {
                return Err(DataError::Format(format!("{}: the file was replaced since it was added; add it again", path.display())));
            }
            Ok(())
        };
        same_file()?;
        // An upload's SQLite copy is generated; its bytes were supplied by the caller.
        let conn = if self.temp.is_some() { crate::provenance::without(|| open_db(path)) } else { open_db(path) }?;
        same_file()?;
        let req = Request { extent, limit: usize::MAX, own: None };
        let native = extent.and_then(|e| self.reproject.extent_from_lonlat(&e));
        let sql = format!("SELECT {} FROM {}{}", self.select_list(), quote(&self.table), self.order_by());
        let mut st = conn.prepare(&sql).map_err(err)?;
        let mut rows = st.query([]).map_err(err)?;
        let mut out = Batch { complete: true, ..Batch::default() };
        crate::progress::begin(self.progress_total());
        while let Some(r) = rows.next().map_err(err)? {
            crate::progress::step(1)?;
            match self.feature(r, native.as_ref(), &req, &mut out.unread_dates, &mut out.rounded_dates).map_err(err)? {
                Row::Feature(f) => {
                    out.matched += 1;
                    chunks.push(f)?;
                }
                Row::Skip => {}
                Row::Bad => out.skipped += 1,
            }
        }
        chunks.flush()?;
        Ok(out)
    }
}

/// How to fetch rows of `table` in a scan's order ([`RowOrder`]). Only "no such row" answers a
/// question here; a statement that fails fails the open, rather than picking an order unseen.
fn row_order(conn: &Connection, table: &str) -> rusqlite::Result<RowOrder> {
    // SQLite indexes a primary key that isn't the rowid itself.
    if conn.query_row("SELECT 1 FROM pragma_index_list(?1) WHERE origin = 'pk'", [table], |_| Ok(())).optional()?.is_none() {
        return Ok(RowOrder::Key);
    }
    // A column takes a rowid's name from it; a table without rowids has none of them, and a query
    // naming one doesn't prepare (where it fails otherwise, the table is read by a scan: slower,
    // never out of order).
    for name in ["rowid", "_rowid_", "oid"] {
        let taken = "SELECT 1 FROM pragma_table_xinfo(?1) WHERE lower(name) = ?2";
        if conn.query_row(taken, [table, name], |_| Ok(())).optional()?.is_none()
            && conn.prepare(&format!("SELECT {name} FROM {}", quote(table))).is_ok()
        {
            return Ok(RowOrder::Rowid(name));
        }
    }
    Ok(RowOrder::Scan)
}

/// The rowids (named `rowid`) of the rows of `table` whose `key` is among `keys`.
fn rowids(conn: &Connection, table: &str, rowid: &str, key: &str, keys: &[i64]) -> rusqlite::Result<Vec<i64>> {
    let mut found = Vec::with_capacity(keys.len());
    for chunk in keys.chunks(CHUNK) {
        let marks = vec!["?"; chunk.len()].join(",");
        let mut st = conn.prepare(&format!("SELECT {rowid} FROM {table} WHERE {key} IN ({marks})"))?;
        for id in st.query_map(params_from_iter(chunk.iter()), |r| r.get(0))? {
            found.push(id?);
        }
    }
    Ok(found)
}

/// A file's device and inode, which tell it from another file put at its path since; `None`
/// where the system doesn't say them.
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// WGS 84 as OGC WKT, the coordinate system GeoPackages are written in.
const WGS84_WKT: &str = "GEOGCS[\"WGS 84\",DATUM[\"WGS_1984\",SPHEROID[\"WGS 84\",6378137,298.257223563,AUTHORITY[\"EPSG\",\"7030\"]],AUTHORITY[\"EPSG\",\"6326\"]],PRIMEM[\"Greenwich\",0,AUTHORITY[\"EPSG\",\"8901\"]],UNIT[\"degree\",0.0174532925199433,AUTHORITY[\"EPSG\",\"9122\"]],AXIS[\"Latitude\",NORTH],AXIS[\"Longitude\",EAST],AUTHORITY[\"EPSG\",\"4326\"]]";

/// A geometry as a GeoPackage blob: the `GP` header (little-endian, WGS 84, an XY envelope) and
/// the WKB. Points of a multipoint layer are written as one-point multipoints. `extent` is the
/// geometry's box.
fn blob(g: &crate::Geometry, extent: &Extent, multi_points: bool, srs_id: i32) -> Vec<u8> {
    let g = match g {
        crate::Geometry::Point(c) if multi_points => &crate::Geometry::MultiPoint(vec![*c]),
        g => g,
    };
    header_blob(&wkb::encode(g), extent, srs_id)
}

fn header_blob(wkb: &[u8], e: &Extent, srs_id: i32) -> Vec<u8> {
    let mut out = Vec::with_capacity(40 + wkb.len());
    out.extend_from_slice(b"GP");
    out.push(0);
    if e.is_empty() {
        // No coordinates: flagged empty, without an envelope.
        out.push(0b1_0001);
        out.extend_from_slice(&srs_id.to_le_bytes());
    } else {
        out.push(0b0000_0011);
        out.extend_from_slice(&srs_id.to_le_bytes());
        for v in [e.min_x, e.max_x, e.min_y, e.max_y] {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    out.extend_from_slice(wkb);
    out
}

/// Writes a dataset as a GeoPackage with one table (WGS 84, with an R-tree spatial index), or an
/// attributes table for a dataset without geometry. The destination must be new: an existing
/// GeoPackage may contain other tables. The file is built beside `path` and installed without
/// replacing anything, so a failed write leaves existing data untouched.
pub fn write(ds: &crate::Dataset, path: &Path) -> Result<()> {
    write_in(ds, path, Crs::LonLat)
}

/// Writes a dataset whose geometry is in `crs`'s coordinates as a new GeoPackage in that system.
/// Existing destinations are refused, as with [`write`].
pub fn write_in(ds: &crate::Dataset, path: &Path, crs: Crs) -> Result<()> {
    write_table_in(ds, path, crs, &GpkgTable::default()).map(|_| ())
}

/// As [`write_in`], the table placed as `target` says: in a new file, or added to (or replacing
/// a table of) the GeoPackage at `path` when it exists, its other tables left as they were.
pub fn write_table_in(ds: &crate::Dataset, path: &Path, crs: Crs, target: &GpkgTable) -> Result<GpkgWritten> {
    let name = target.name.as_deref().unwrap_or(&ds.name);
    let pinned = pinned_time()?;
    if let Some(replace) = into_existing(path, target)? {
        return add::write_features(path, name, table_kind(ds), &ds.fields, crs, replace, pinned, &mut |put| put(&ds.features));
    }
    check_new_destination(path)?;
    let tmp = temp_database_beside(path)?;
    let conn = Connection::open(&tmp).map_err(err)?;
    let table = build(&conn, ds, name, crs, pinned)?;
    install(conn, tmp, path)?;
    Ok(GpkgWritten { table, replaced: false, dependents: Vec::new() })
}

/// The kind of geometry a GeoPackage table of `ds` is made for: its type or, for a layer that
/// declares none but carries geometry, its first geometry's (as a streamed export takes it), so
/// that geometry isn't dropped for want of a column. A layer holding other kinds as well gets a
/// GEOMETRY column, which holds any kind ([`Redo::Mixed`]).
fn table_kind(ds: &crate::Dataset) -> GeometryType {
    match ds.geometry_type {
        GeometryType::None => ds.features.iter().find_map(|f| f.geometry.as_ref()).map_or(GeometryType::None, crate::Geometry::kind),
        t => t,
    }
}

/// Whether a table made for geometry of `kind` can't hold `f`'s in a column of that type, so it
/// needs a GEOMETRY column: a feature of another kind (a mixed layer's, as a GeoJSON of points
/// and polygons is). One without geometry fits any table.
fn other_kind(f: &crate::Feature, kind: GeometryType) -> bool {
    kind != GeometryType::None && f.geometry.as_ref().is_some_and(|g| g.kind() != kind)
}

/// Whether an export goes into the existing GeoPackage at `path` (anything there, which opening
/// it as one then checks) and, if so, whether it replaces a table: `None` for a new file. A
/// table is added to a new file when there is none; one can't be replaced in a file not there.
fn into_existing(path: &Path, target: &GpkgTable) -> Result<Option<bool>> {
    let exists = std::fs::symlink_metadata(path).is_ok();
    match target.mode {
        GpkgMode::NewFile => Ok(None),
        GpkgMode::AddTable => Ok(exists.then_some(false)),
        GpkgMode::ReplaceTable if exists => Ok(Some(true)),
        GpkgMode::ReplaceTable => Err(DataError::Format(format!("{}: no GeoPackage there to replace a table in", path.display()))),
    }
}

/// As [`write_table_in`], for a layer `name` of `geometry_type` with `fields`: the features `feed`
/// hands to the function it is given, a run at a time, already in `crs`'s coordinates, so a
/// whole layer needn't be held. A point layer's table type, whether the column holds any kind
/// (a feature of another kind than `geometry_type`) and whether ids are kept are chosen from the
/// features so far; should a later one contradict that choice, the export starts again with it
/// settled (feeding the features again), giving the file [`write_in`] would.
pub(crate) fn write_features_in(
    name: &str,
    geometry_type: GeometryType,
    fields: &[Field],
    path: &Path,
    crs: Crs,
    target: &GpkgTable,
    feed: &mut dyn FnMut(&mut dyn FnMut(&[crate::Feature]) -> Result<()>) -> Result<()>,
) -> Result<GpkgWritten> {
    let name = target.name.as_deref().unwrap_or(name);
    let pinned = pinned_time()?;
    if let Some(replace) = into_existing(path, target)? {
        return add::write_features(path, name, geometry_type, fields, crs, replace, pinned, feed);
    }
    check_new_destination(path)?;
    let (mut multi_points, mut mixed, mut keep_ids) = (None, false, true);
    // Each choice changes at most once.
    for _ in 0..4 {
        let tmp = temp_database_beside(path)?;
        let conn = Connection::open(&tmp).map_err(err)?;
        let mut make = |multi, mixed, first: &[crate::Feature]| create(&conn, name, geometry_type, fields, crs, multi, mixed, first);
        match fill(&conn, multi_points, mixed, keep_ids, &mut make, feed)? {
            Err(Redo::MultiPoints) => multi_points = Some(true),
            Err(Redo::Mixed) => mixed = true,
            Err(Redo::Renumber) => keep_ids = false,
            Ok((layout, extent)) => {
                finish(&conn, &layout, extent, pinned)?;
                install(conn, tmp, path)?;
                return Ok(GpkgWritten { table: layout.table, replaced: false, dependents: Vec::new() });
            }
        }
    }
    Err(DataError::Format("GeoPackage: the export didn't settle on a table".into()))
}

/// Feeds the features into the table `make` creates on `conn`, made once the first run shows
/// whether a point layer holds multipoints (unless `multi_points` already says), its column one
/// for any kind of geometry when `mixed`: the table and the extent of what went in, or what the
/// export must start again for. With `keep_ids` each feature keeps its id.
fn fill(
    conn: &Connection,
    multi_points: Option<bool>,
    mixed: bool,
    keep_ids: bool,
    make: &mut dyn FnMut(bool, bool, &[crate::Feature]) -> Result<Layout>,
    feed: &mut dyn FnMut(&mut dyn FnMut(&[crate::Feature]) -> Result<()>) -> Result<()>,
) -> Result<std::result::Result<(Layout, Extent), Redo>> {
    let (mut layout, mut extent, mut redo) = (None, Extent::EMPTY, None);
    let mut boxes = Vec::new();
    let fed = feed(&mut |features| {
        // An empty run (a chunk a definition query emptied) says nothing about the rows to come:
        // the table, its page size and whether it holds multipoints wait for the first run with
        // rows. Without any, the end of the feed makes it.
        if layout.is_none() && features.is_empty() {
            return Ok(());
        }
        if layout.is_none() {
            let multi = multi_points.unwrap_or_else(|| features.iter().any(|f| matches!(f.geometry, Some(crate::Geometry::MultiPoint(_)))));
            layout = Some(make(multi, mixed, features)?);
        }
        let Some(l) = layout.as_ref() else { return Err(DataError::Format("GeoPackage: no table to write".into())) };
        redo = insert(conn, l, features, keep_ids, &mut boxes, &mut extent)?;
        match redo {
            // Stops the traversal; the export starts again.
            Some(_) => Err(DataError::Format("GeoPackage: starting again".into())),
            None => Ok(()),
        }
    });
    match (fed, redo) {
        (_, Some(r)) => Ok(Err(r)),
        (Err(e), None) => Err(e),
        (Ok(()), None) => {
            let layout = match layout {
                Some(l) => l,
                None => make(multi_points.unwrap_or(false), mixed, &[])?,
            };
            fill_index(conn, &layout, &mut boxes)?;
            Ok(Ok((layout, extent)))
        }
    }
}

/// A new temporary file beside `path`, removed unless [`install`]ed.
fn temp_database_beside(path: &Path) -> Result<tempfile::TempPath> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    // Drop the file handle before SQLite opens it, including on Windows. TempPath cleans up on
    // every error, and persist_noclobber refuses files created after the check above as well.
    let mut builder = tempfile::Builder::new();
    builder.prefix(".rustgis-gpkg-").suffix(".tmp");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Match ordinary exports: request 0666 and let the user's umask restrict it.
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    Ok(builder.tempfile_in(parent)?.into_temp_path())
}

/// Closes a built database and moves it to `path`, which must still be new.
fn install(conn: Connection, tmp: tempfile::TempPath, path: &Path) -> Result<()> {
    conn.close().map_err(|(_, e)| err(e))?;
    // Built without syncing (fast); synced once before installation. Windows needs a write handle.
    std::fs::OpenOptions::new().write(true).open(&tmp)?.sync_all()?;
    // A cancel that came while it was built leaves no file (the temporary one is removed). This
    // is the last look: persisting commits the file, and a Cancel after it finds it written.
    crate::progress::check()?;
    // Refuse journals that appeared during the build; persistence also closes the file race.
    check_new_destination(path)?;
    tmp.persist_noclobber(path).map_err(|e| destination_error(path, e.error))?;
    Ok(())
}

/// Refuses existing files, including dangling symlinks and journals left without their database.
fn check_new_destination(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut entry = path.as_os_str().to_owned();
        entry.push(suffix);
        match std::fs::symlink_metadata(Path::new(&entry)) {
            Ok(_) => return Err(destination_error(path, std::io::ErrorKind::AlreadyExists.into())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

fn destination_error(path: &Path, error: std::io::Error) -> DataError {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "GeoPackage export cannot replace {} or its SQLite journals. Choose a new filename to preserve existing data.",
                path.display()
            ),
        )
        .into()
    } else {
        error.into()
    }
}

/// Builds a whole layer's GeoPackage on `conn`, its table `name` chosen from all its features;
/// returns the table's name as written.
fn build(conn: &Connection, ds: &crate::Dataset, name: &str, crs: Crs, pinned: Option<i64>) -> Result<String> {
    let multi_points = ds.features.iter().any(|f| matches!(f.geometry, Some(crate::Geometry::MultiPoint(_))));
    let kind = table_kind(ds);
    let mixed = ds.features.iter().any(|f| other_kind(f, kind));
    let layout = create(conn, name, kind, &ds.fields, crs, multi_points, mixed, &ds.features)?;
    // Feature ids are kept when they are unique and fit; otherwise SQLite numbers the rows.
    let mut seen = std::collections::HashSet::new();
    let keep_ids = ds.features.iter().all(|f| i64::try_from(f.id).is_ok() && seen.insert(f.id));
    // The table is made for the whole layer, so nothing should need doing again; should a
    // feature not fit after all, the export fails rather than leave it out.
    let (mut boxes, mut extent) = (Vec::new(), Extent::EMPTY);
    if insert(conn, &layout, &ds.features, keep_ids, &mut boxes, &mut extent)?.is_some() {
        return Err(DataError::Format("GeoPackage: a feature doesn't fit the table chosen for the layer".into()));
    }
    fill_index(conn, &layout, &mut boxes)?;
    finish(conn, &layout, extent, pinned)?;
    Ok(layout.table)
}

/// A feature table being written: its names, and what its rows hold.
struct Layout {
    table: String,
    geom: String,
    columns: Vec<String>,
    fields: Vec<Field>,
    has_geom: bool,
    multi_points: bool,
    /// The kind of geometry the table is made for, and whether its column holds any kind.
    kind: GeometryType,
    mixed: bool,
    geom_type: &'static str,
    srs_id: i32,
    /// The table, geometry and id column, and spatial index, quoted for SQL.
    table_q: String,
    geom_q: String,
    fid_q: String,
    rtree_q: String,
}

/// The GeoPackage's own tables, as a new file has them.
const METADATA: &str = "CREATE TABLE gpkg_spatial_ref_sys (srs_name TEXT NOT NULL, srs_id INTEGER PRIMARY KEY, organization TEXT NOT NULL,
        organization_coordsys_id INTEGER NOT NULL, definition TEXT NOT NULL, description TEXT);
    CREATE TABLE gpkg_contents (table_name TEXT NOT NULL PRIMARY KEY, data_type TEXT NOT NULL, identifier TEXT UNIQUE,
        description TEXT DEFAULT '', last_change DATETIME NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
        min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE, srs_id INTEGER,
        CONSTRAINT fk_gc_r_srs_id FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys(srs_id));";

/// The tables a GeoPackage holding a feature table with a spatial index needs beyond
/// [`METADATA`], made where missing (an existing GeoPackage may have only attribute tables).
const FEATURE_METADATA: &str = "CREATE TABLE IF NOT EXISTS gpkg_geometry_columns (table_name TEXT NOT NULL, column_name TEXT NOT NULL,
        geometry_type_name TEXT NOT NULL, srs_id INTEGER NOT NULL, z TINYINT NOT NULL, m TINYINT NOT NULL,
        CONSTRAINT pk_geom_cols PRIMARY KEY (table_name, column_name),
        CONSTRAINT uk_gc_table_name UNIQUE (table_name),
        CONSTRAINT fk_gc_tn FOREIGN KEY (table_name) REFERENCES gpkg_contents(table_name),
        CONSTRAINT fk_gc_srs FOREIGN KEY (srs_id) REFERENCES gpkg_spatial_ref_sys (srs_id));
    CREATE TABLE IF NOT EXISTS gpkg_extensions (table_name TEXT, column_name TEXT, extension_name TEXT NOT NULL,
        definition TEXT NOT NULL, scope TEXT NOT NULL, CONSTRAINT ge_tce UNIQUE (table_name, column_name, extension_name));";

/// SQLite's own page size.
const PAGE: usize = 4096;
/// The page size a new file gets instead when its rows fill such pages much better.
const WIDE_PAGE: usize = 16 * 1024;
/// How many of a layer's first features [`page_size`] looks at.
const PAGE_SAMPLE: usize = 1000;
/// About how many pages a new file holds besides its feature table's, nearly empty at any size.
const OTHER_PAGES: usize = 16;

/// The page size for a new file whose first features are `first`, `None` for SQLite's own
/// [`PAGE`]: [`WIDE_PAGE`] when such pages would hold the rows of the first [`PAGE_SAMPLE`]
/// features, counting what the other tables' pages grow by, in less than nine tenths of the
/// space. A row of about 2 KiB (a polygon of 130 places) takes a 4 KiB page to itself and leaves
/// half of it empty. A streamed export looks at its first run, so it chooses as a whole one does
/// unless that run holds only some of those features.
fn page_size(fields: &[Field], first: &[crate::Feature]) -> Option<usize> {
    let rows: Vec<usize> = first
        .iter()
        .take(PAGE_SAMPLE)
        .map(|f| {
            // The geometry with its blob header, and about what each value takes.
            let geometry = f.geometry.as_ref().map_or(0, |g| 40 + wkb::wkb_size(g));
            let values = f.attrs.iter().take(fields.len()).map(|v| match v {
                Value::Null => 1,
                Value::Text(s) => 2 + s.len(),
                _ => 9,
            });
            geometry.saturating_add(values.sum())
        })
        .collect();
    let wide = table_bytes(&rows, WIDE_PAGE).saturating_add(OTHER_PAGES * (WIDE_PAGE - PAGE));
    (wide.saturating_mul(10) < table_bytes(&rows, PAGE).saturating_mul(9)).then_some(WIDE_PAGE)
}

/// The bytes a table takes in pages of `page` bytes for rows of these sizes, added in order, as
/// SQLite's file format lays them out: a row goes on the last leaf page or starts the next, and
/// one too big for a page keeps its start there and the rest on overflow pages of its own.
fn table_bytes(rows: &[usize], page: usize) -> usize {
    // The most of a row a leaf page holds, and the least it keeps of one that overflows.
    let (most, least) = (page - 35, (page - 12) * 32 / 255 - 23);
    let (mut pages, mut used) = (1usize, 0usize);
    for &row in rows {
        let mut here = row;
        if row > most {
            let rest = least + (row - least) % (page - 4);
            here = if rest <= most { rest } else { least };
            pages = pages.saturating_add((row - here).div_ceil(page - 4));
        }
        // With it, its place in the page's list, its length, its id and where it overflows to.
        let cell = here + 12;
        if used + cell > page - 8 {
            pages = pages.saturating_add(1);
            used = 0;
        }
        used += cell;
    }
    pages.saturating_mul(page)
}

/// Creates the GeoPackage's tables and the empty feature table (and its spatial index) for a
/// layer, in a transaction [`finish`] commits, in pages sized for its `first` features
/// ([`page_size`]). A point layer holding any multipoint is a MULTIPOINT table, whose single
/// points [`insert`] writes as one-point multipoints; a `mixed` layer's is a GEOMETRY table.
fn create(
    conn: &Connection,
    name: &str,
    geometry_type: GeometryType,
    fields: &[Field],
    crs: Crs,
    multi_points: bool,
    mixed: bool,
    first: &[crate::Feature],
) -> Result<Layout> {
    // An EPSG system by its code; a custom one numbered as GDAL numbers them.
    let srs_id = match crs.epsg() {
        0 => 100_000,
        c => i32::from(c),
    };
    let layout = plan(name, geometry_type, fields, multi_points, mixed, srs_id);
    // The page size goes first: it can't change once anything is written.
    let page = page_size(fields, first).map_or_else(String::new, |p| format!("PRAGMA page_size = {p};"));
    conn.execute_batch(&format!(
        "{page} PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;
        PRAGMA application_id = 1196444487; PRAGMA user_version = 10400;
        {METADATA} {FEATURE_METADATA} BEGIN;"
    ))
    .map_err(err)?;
    create_table(conn, &layout)?;
    conn.execute(
        "INSERT INTO gpkg_spatial_ref_sys VALUES
            ('Undefined cartesian SRS', -1, 'NONE', -1, 'undefined', 'undefined cartesian coordinate reference system'),
            ('Undefined geographic SRS', 0, 'NONE', 0, 'undefined', 'undefined geographic coordinate reference system'),
            ('WGS 84 geodetic', 4326, 'EPSG', 4326, ?1, 'longitude/latitude coordinates in decimal degrees on the WGS 84 spheroid')",
        [WGS84_WKT],
    )
    .map_err(err)?;
    if srs_id != 4326 {
        // GeoPackage readers take an EPSG system by its code; a custom one has only its WKT
        // (`undefined` when it has none).
        let org = if crs.epsg() > 0 { "EPSG" } else { "NONE" };
        let wkt = crate::raster::wkt::to_wkt(crs);
        conn.execute(
            "INSERT INTO gpkg_spatial_ref_sys VALUES (?1, ?2, ?3, ?2, ?4, '')",
            rusqlite::params![crs.label(), srs_id, org, wkt.as_deref().unwrap_or("undefined")],
        )
        .map_err(err)?;
    }
    Ok(layout)
}

/// The names a layer's feature table is written with, in spatial reference system `srs_id`: the
/// table named for the layer (not a reserved name, never empty), its id and geometry columns
/// clear of its fields, and its fields' columns unique without regard to case. A `mixed` layer's
/// geometry column is GEOMETRY, which holds every kind, its features written as they are.
fn plan(name: &str, geometry_type: GeometryType, fields: &[Field], multi_points: bool, mixed: bool, srs_id: i32) -> Layout {
    let mut table: String = name.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string();
    if table.is_empty() {
        table = "layer".into();
    }
    if table.to_ascii_lowercase().starts_with("gpkg_")
        || table.to_ascii_lowercase().starts_with("sqlite_")
        || table.to_ascii_lowercase().starts_with("rtree_")
    {
        table = format!("layer_{table}");
    }
    let has_geom = geometry_type != GeometryType::None;
    let (fid, geom) = (crate::io::free_column_name(fields, "fid"), crate::io::free_column_name(fields, "geom"));
    let columns = crate::io::geopackage_columns(fields);
    let mixed = mixed && has_geom;
    // A GEOMETRY column holds single points as they are.
    let multi_points = multi_points && !mixed;
    let geom_type = match geometry_type {
        _ if mixed => "GEOMETRY",
        GeometryType::Point if multi_points => "MULTIPOINT",
        GeometryType::Point => "POINT",
        GeometryType::Polyline => "MULTILINESTRING",
        GeometryType::Polygon => "MULTIPOLYGON",
        GeometryType::None => "",
    };
    let (t, g, i) = (quote(&table), quote(&geom), quote(&fid));
    let rt = quote(&format!("rtree_{table}_{geom}"));
    Layout {
        table,
        geom,
        columns,
        fields: fields.to_vec(),
        has_geom,
        multi_points,
        kind: geometry_type,
        mixed,
        geom_type,
        srs_id,
        table_q: t,
        geom_q: g,
        fid_q: i,
        rtree_q: rt,
    }
}

/// Creates the empty feature table [`plan`] named, and its spatial index.
fn create_table(conn: &Connection, layout: &Layout) -> Result<()> {
    let Layout { columns, fields, has_geom, geom_type, table_q: t, geom_q: g, fid_q: i, rtree_q: rt, .. } = layout;
    let mut cols = vec![format!("{i} INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL")];
    if *has_geom {
        cols.push(format!("{g} {geom_type}"));
    }
    for (f, col) in fields.iter().zip(columns) {
        let ty = match f.ty {
            FieldType::Integer => "INTEGER",
            FieldType::Double => "REAL",
            FieldType::Boolean => "BOOLEAN",
            FieldType::Text => "TEXT",
            FieldType::Date => "DATE",
            FieldType::DateTime => "DATETIME",
        };
        cols.push(format!("{} {ty}", quote(col)));
    }
    let mut sql = format!("CREATE TABLE {t} ({});", cols.join(", "));
    if *has_geom {
        sql.push_str(&format!("CREATE VIRTUAL TABLE {rt} USING rtree(id, minx, maxx, miny, maxy);"));
    }
    conn.execute_batch(&sql).map_err(err)
}

/// What a streamed export found it must start again for, having chosen from the features so far.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Redo {
    /// A multipoint in a POINT table: the table is MULTIPOINT.
    MultiPoints,
    /// Geometry of another kind than the table's: its column is GEOMETRY, holding every kind.
    Mixed,
    /// An id that doesn't fit or is taken: SQLite numbers every row.
    Renumber,
}

/// Inserts features into the table [`create`] made, grows `extent` to hold their geometry, and
/// puts the box of each one with a place in `boxes` for [`fill_index`] to load, loading them
/// whenever [`INDEX_BATCH`] are waiting; the caller loads the rest at the end. With `keep_ids`
/// each keeps its id; otherwise SQLite numbers it. Stops at the first feature the table as made
/// can't hold as the whole layer would have it (a multipoint in a POINT table, geometry of
/// another kind, an id that doesn't fit or is taken), saying what to [`Redo`].
fn insert(
    conn: &Connection,
    layout: &Layout,
    features: &[crate::Feature],
    keep_ids: bool,
    boxes: &mut Vec<IndexBox>,
    extent: &mut Extent,
) -> Result<Option<Redo>> {
    use rusqlite::types::Value as Sql;
    let Layout { columns, fields, has_geom, multi_points, srs_id, table_q: t, geom_q: g, fid_q: i, .. } = layout;
    let (has_geom, multi_points, srs_id) = (*has_geom, *multi_points, *srs_id);
    let names: Vec<String> =
        std::iter::once(i.clone()).chain(has_geom.then(|| g.clone())).chain(columns.iter().map(|c| quote(c))).collect();
    let marks = vec!["?"; names.len()].join(", ");
    let mut st = conn.prepare_cached(&format!("INSERT INTO {t} ({}) VALUES ({marks})", names.join(", "))).map_err(err)?;
    // The spatial index is filled after the rows ([`fill_index`]); its triggers (which call
    // functions only GeoPackage software defines) are created after that.
    for f in features {
        crate::progress::step(1)?;
        if has_geom && !layout.mixed && other_kind(f, layout.kind) {
            return Ok(Some(Redo::Mixed));
        }
        if has_geom && !multi_points && matches!(f.geometry, Some(crate::Geometry::MultiPoint(_))) && layout.geom_type == "POINT" {
            return Ok(Some(Redo::MultiPoints));
        }
        let id = if keep_ids {
            match i64::try_from(f.id) {
                Ok(id) => Sql::Integer(id),
                Err(_) => return Ok(Some(Redo::Renumber)),
            }
        } else {
            Sql::Null
        };
        let mut row: Vec<Sql> = Vec::with_capacity(names.len());
        row.push(id);
        // The geometry's box, for its blob, the table's extent and the index.
        let e = f.geometry.as_ref().map_or(Extent::EMPTY, crate::Geometry::extent);
        *extent = extent.union(&e);
        if has_geom {
            row.push(f.geometry.as_ref().map(|x| Sql::Blob(blob(x, &e, multi_points, srs_id))).unwrap_or(Sql::Null));
        }
        for (k, fd) in fields.iter().enumerate() {
            row.push(sql_value(f.attrs.get(k).unwrap_or(&Value::Null), fd.ty));
        }
        match st.execute(params_from_iter(row)) {
            Ok(_) => {}
            // The id is taken: the table's only key.
            Err(rusqlite::Error::SqliteFailure(e, _)) if keep_ids && e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY => {
                return Ok(Some(Redo::Renumber));
            }
            Err(e) => return Err(err(e)),
        }
        if has_geom && !e.is_empty() {
            boxes.push(IndexBox { key: 0, id: conn.last_insert_rowid(), bounds: [e.min_x, e.max_x, e.min_y, e.max_y] });
            // Loaded the moment a batch fills, so `boxes` never grows past one batch and a
            // streamed export batches exactly as a whole-layer one does.
            if boxes.len() == INDEX_BATCH {
                fill_index(conn, layout, boxes)?;
            }
        }
    }
    Ok(None)
}

/// A feature's box waiting to go into the spatial index, with its place along the Hilbert curve
/// (`key`, 0 until [`fill_index`] works it out from the whole batch's extent).
/// It holds 48 bytes per feature that has a place, until [`fill_index`] loads it.
struct IndexBox {
    key: u64,
    id: i64,
    /// `[min_x, max_x, min_y, max_y]`, the order of the index's columns.
    bounds: [f64; 4],
}

/// How many boxes [`fill_index`] loads between looks at Cancel.
const INDEX_CHECK_EVERY: usize = 8192;

/// How many boxes an export keeps before it loads them (96 MB at 48 bytes each), so a layer of
/// any size needs no more; a layer of fewer features is loaded in one go at the end.
/// Each load is in Hilbert order within itself.
/// (1,000 in this crate's tests, so they reach a second load.)
const INDEX_BATCH: usize = if cfg!(test) { 1000 } else { 1 << 21 };

/// The distance along a Hilbert curve of 2^16 by 2^16 cells at which the cell `(x, y)` lies.
fn hilbert(mut x: u32, mut y: u32) -> u64 {
    const SIDE: u32 = 1 << 16;
    let mut d = 0u64;
    let mut s = SIDE >> 1;
    while s > 0 {
        let (rx, ry) = (u32::from(x & s != 0), u32::from(y & s != 0));
        d += u64::from(s) * u64::from(s) * u64::from((3 * rx) ^ ry);
        if ry == 0 {
            if rx == 1 {
                x = SIDE - 1 - x;
                y = SIDE - 1 - y;
            }
            std::mem::swap(&mut x, &mut y);
        }
        s >>= 1;
    }
    d
}

/// Fills the table's spatial index with `boxes`, one entry for each, in Hilbert order of their
/// centres. SQLite's R-tree picks a leaf for each entry by walking down from the root, which costs
/// far more when consecutive entries lie all over the map than when neighbours arrive together.
/// Ties fall back to the row id, so the same layer gives the same file each time. Empties `boxes`.
/// Looks at Cancel as it goes, but not within one SQLite insert, nor while it works out the
/// Hilbert places and sorts them (one pass and one sort over at most [`INDEX_BATCH`] boxes).
fn fill_index(conn: &Connection, layout: &Layout, boxes: &mut Vec<IndexBox>) -> Result<()> {
    if !layout.has_geom || boxes.is_empty() {
        return Ok(());
    }
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for b in boxes.iter() {
        for (axis, (a, z)) in [(0, 1), (2, 3)].into_iter().enumerate() {
            let centre = (b.bounds[a] + b.bounds[z]) / 2.0;
            if centre.is_finite() {
                lo[axis] = lo[axis].min(centre);
                hi[axis] = hi[axis].max(centre);
            }
        }
    }
    let cell = |v: f64, axis: usize| {
        let span = hi[axis] - lo[axis];
        // A NaN or an empty span lands in the first cell.
        if span > 0.0 { (((v - lo[axis]) / span) * 65535.0).clamp(0.0, 65535.0) as u32 } else { 0 }
    };
    for b in boxes.iter_mut() {
        let [x0, x1, y0, y1] = b.bounds;
        b.key = hilbert(cell((x0 + x1) / 2.0, 0), cell((y0 + y1) / 2.0, 1));
    }
    crate::progress::check()?;
    boxes.sort_unstable_by_key(|b| (b.key, b.id));
    let mut index = conn.prepare_cached(&format!("INSERT INTO {} VALUES (?1, ?2, ?3, ?4, ?5)", layout.rtree_q)).map_err(err)?;
    for (n, b) in boxes.iter().enumerate() {
        if n % INDEX_CHECK_EVERY == 0 {
            crate::progress::check()?;
        }
        let [min_x, max_x, min_y, max_y] = b.bounds;
        index.execute(rusqlite::params![b.id, min_x, max_x, min_y, max_y]).map_err(err)?;
    }
    boxes.clear();
    Ok(())
}

/// [`register`]s the table, in a new file, and commits.
fn finish(conn: &Connection, layout: &Layout, extent: Extent, pinned: Option<i64>) -> Result<()> {
    register(conn, layout, extent, pinned, None)?;
    conn.execute_batch("COMMIT;").map_err(err)
}

/// Records the table in the GeoPackage's contents with its `extent` (identified by its table
/// name, numbered if another table already has that identifier) and adds the triggers that keep its
/// spatial index in step with edits. Its `last_change` is the `pinned` time ([`pinned_time`]) or
/// now, and past the `last_change` of the table it `replaced`, if any.
fn register(conn: &Connection, layout: &Layout, extent: Extent, pinned: Option<i64>, replaced: Option<&str>) -> Result<()> {
    let Layout { table, geom, has_geom, geom_type, srs_id, table_q: t, geom_q: g, fid_q: i, rtree_q: rt, .. } = layout;
    let (has_geom, srs_id) = (*has_geom, *srs_id);
    let e = extent;
    let bounds: [Option<f64>; 4] = if e.is_empty() { [None; 4] } else { [Some(e.min_x), Some(e.min_y), Some(e.max_x), Some(e.max_y)] };
    let srs: Option<i64> = has_geom.then_some(i64::from(srs_id));
    // The table's name, numbered when another table of an existing GeoPackage has it as its
    // (unique) identifier.
    let mut identifier = table.clone();
    for i in 2.. {
        let taken: bool = conn
            .query_row("SELECT EXISTS (SELECT 1 FROM gpkg_contents WHERE identifier = ?1)", [&identifier], |r| r.get(0))
            .map_err(err)?;
        if !taken {
            break;
        }
        identifier = format!("{table}_{i}");
    }
    // The time of the export, or the pinned one; a replaced table's always moves past the one it
    // replaces, since saving edits takes an unchanged `last_change` for an unchanged table.
    let last_change: String = conn
        .query_row(
            "SELECT CASE WHEN julianday(?2) >= julianday(t) THEN strftime('%Y-%m-%dT%H:%M:%fZ', ?2, '+0.001 seconds') ELSE t END
             FROM (SELECT COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', ?1, 'unixepoch'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) AS t)",
            rusqlite::params![pinned, replaced],
            |r| r.get(0),
        )
        .map_err(err)?;
    conn.execute(
        "INSERT INTO gpkg_contents (table_name, data_type, identifier, last_change, min_x, min_y, max_x, max_y, srs_id)
            VALUES (?1, ?2, ?3, ?9, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            table,
            if has_geom { "features" } else { "attributes" },
            identifier,
            bounds[0],
            bounds[1],
            bounds[2],
            bounds[3],
            srs,
            last_change
        ],
    )
    .map_err(err)?;
    if has_geom {
        conn.execute(
            "INSERT INTO gpkg_geometry_columns (table_name, column_name, geometry_type_name, srs_id, z, m) VALUES (?1, ?2, ?3, ?4, 0, 0)",
            rusqlite::params![table, geom, geom_type, srs_id],
        )
        .map_err(err)?;
        // The GeoPackage 1.4 triggers that keep the index in step with edits.
        let trig = |s: &str| quote(&format!("rtree_{table}_{geom}_{s}"));
        conn.execute_batch(&format!(
            "CREATE TRIGGER {ins} AFTER INSERT ON {t} WHEN (new.{g} NOT NULL AND NOT ST_IsEmpty(NEW.{g}))
            BEGIN INSERT OR REPLACE INTO {rt} VALUES (NEW.{i}, ST_MinX(NEW.{g}), ST_MaxX(NEW.{g}), ST_MinY(NEW.{g}), ST_MaxY(NEW.{g})); END;
            CREATE TRIGGER {u6} AFTER UPDATE OF {g} ON {t} WHEN OLD.{i} = NEW.{i}
                AND (NEW.{g} NOTNULL AND NOT ST_IsEmpty(NEW.{g})) AND (OLD.{g} NOTNULL AND NOT ST_IsEmpty(OLD.{g}))
            BEGIN UPDATE {rt} SET minx = ST_MinX(NEW.{g}), maxx = ST_MaxX(NEW.{g}), miny = ST_MinY(NEW.{g}), maxy = ST_MaxY(NEW.{g})
                WHERE id = NEW.{i}; END;
            CREATE TRIGGER {u7} AFTER UPDATE OF {g} ON {t} WHEN OLD.{i} = NEW.{i}
                AND (NEW.{g} NOTNULL AND NOT ST_IsEmpty(NEW.{g})) AND (OLD.{g} ISNULL OR ST_IsEmpty(OLD.{g}))
            BEGIN INSERT INTO {rt} VALUES (NEW.{i}, ST_MinX(NEW.{g}), ST_MaxX(NEW.{g}), ST_MinY(NEW.{g}), ST_MaxY(NEW.{g})); END;
            CREATE TRIGGER {u2} AFTER UPDATE OF {g} ON {t} WHEN OLD.{i} = NEW.{i} AND (NEW.{g} ISNULL OR ST_IsEmpty(NEW.{g}))
            BEGIN DELETE FROM {rt} WHERE id = OLD.{i}; END;
            CREATE TRIGGER {u5} AFTER UPDATE ON {t} WHEN OLD.{i} != NEW.{i} AND (NEW.{g} NOTNULL AND NOT ST_IsEmpty(NEW.{g}))
            BEGIN DELETE FROM {rt} WHERE id = OLD.{i};
                INSERT INTO {rt} VALUES (NEW.{i}, ST_MinX(NEW.{g}), ST_MaxX(NEW.{g}), ST_MinY(NEW.{g}), ST_MaxY(NEW.{g})); END;
            CREATE TRIGGER {u4} AFTER UPDATE ON {t} WHEN OLD.{i} != NEW.{i} AND (NEW.{g} ISNULL OR ST_IsEmpty(NEW.{g}))
            BEGIN DELETE FROM {rt} WHERE id IN (OLD.{i}, NEW.{i}); END;
            CREATE TRIGGER {del} AFTER DELETE ON {t} WHEN old.{g} NOT NULL
            BEGIN DELETE FROM {rt} WHERE id = OLD.{i}; END;",
            ins = trig("insert"),
            u2 = trig("update2"),
            u4 = trig("update4"),
            u5 = trig("update5"),
            u6 = trig("update6"),
            u7 = trig("update7"),
            del = trig("delete"),
        ))
        .map_err(err)?;
        conn.execute(
            "INSERT INTO gpkg_extensions (table_name, column_name, extension_name, definition, scope)
                VALUES (?1, ?2, 'gpkg_rtree_index', 'http://www.geopackage.org/spec120/#extension_rtree', 'write-only')",
            rusqlite::params![table, geom],
        )
        .map_err(err)?;
    }
    Ok(())
}

/// The SQLite value written for `v` in a column of a `ty` field: numbers and yes/no as numbers,
/// dates as GeoPackage's ISO 8601 text ([`gpkg_datetime`]), text kept as it is.
fn sql_value(v: &Value, ty: FieldType) -> rusqlite::types::Value {
    use rusqlite::types::Value as Sql;
    match (v, ty) {
        (Value::Null, _) => Sql::Null,
        (Value::Int(x), _) => Sql::Integer(*x),
        (Value::Float(x), _) if x.is_finite() => Sql::Real(*x),
        (Value::Float(_), _) => Sql::Null,
        (Value::Bool(b), _) => Sql::Integer(i64::from(*b)),
        (Value::Text(s), FieldType::Text) => Sql::Text(s.clone()),
        (Value::Date(d), FieldType::DateTime) => Sql::Text(gpkg_datetime(crate::date::DateTime::from_date(*d))),
        (Value::Date(d), _) => Sql::Text(d.to_string()),
        (Value::DateTime(t), FieldType::Date) if t.is_date() => Sql::Text(t.date().to_string()),
        // A moment in a date field isn't a day: written as it is, not rounded into a midnight a
        // read would take as one; it reads as null, as `io::text_in_dates` counts it.
        (Value::DateTime(t), FieldType::Date) => Sql::Text(t.to_string()),
        (Value::DateTime(t), _) => Sql::Text(gpkg_datetime(*t)),
        // Text that reads as a date is written as that date, as a read takes it (digits past the
        // microsecond rounded); other text is kept as it is, though a read finds it isn't a date
        // and gives null.
        (Value::Text(s), FieldType::Date | FieldType::DateTime) => match Value::parse_stored_counted(s, ty).0 {
            Value::Null => Sql::Text(s.clone()),
            date => sql_value(&date, ty),
        },
        (v, ty) => match Value::parse_source(&v.to_string(), ty) {
            Value::Int(x) => Sql::Integer(x),
            Value::Float(x) if x.is_finite() => Sql::Real(x),
            Value::Bool(b) => Sql::Integer(i64::from(b)),
            _ => Sql::Null,
        },
    }
}

/// A date-time as a GeoPackage DATETIME holds it, `YYYY-MM-DDTHH:MM:SS.SSSZ`: one with a zone in
/// UTC, so another offset isn't kept ([`crate::io::offset_dates`] counts them), and one without a
/// zone without the `Z`, rather than relabelled as UTC. Milliseconds are always written, as the
/// specification has them, rounded to the nearest (cut at the end of year 9999);
/// [`crate::io::fine_dates`] counts the values that lose digits.
fn gpkg_datetime(t: crate::date::DateTime) -> String {
    let t = to_millisecond(t);
    let t = match t.offset() {
        Some(o) if o != 0 => crate::date::DateTime::with_offset(t.micros(), 0).unwrap_or(t),
        _ => t,
    };
    let mut text = t.to_string();
    // `YYYY-MM-DDTHH:MM:SS` is 19 bytes, then the fraction if any, then the zone.
    if text.get(19..20) != Some(".") && text.len() >= 19 {
        text.insert_str(19, ".000");
    }
    text
}

/// A date-time at the nearest millisecond (halves up), cut instead where that would leave years 1
/// to 9999.
fn to_millisecond(t: crate::date::DateTime) -> crate::date::DateTime {
    use crate::date::DateTime;
    let rest = t.micros().rem_euclid(1000);
    if rest == 0 {
        return t;
    }
    let cut = t.micros() - rest;
    let at = |micros: i64| match t.offset() {
        Some(o) => DateTime::with_offset(micros, o),
        None => DateTime::local(micros),
    };
    let up = (rest >= 500).then(|| at(cut + 1000)).flatten();
    up.or_else(|| at(cut)).unwrap_or(t)
}

/// An attribute value of a field's type, for the field `k`. One a date field can't hold is null,
/// and counted in `unread` ([`Batch::unread_dates`]); with `rounded`, a date-time with digits past
/// the microsecond is rounded and counted there ([`Batch::rounded_dates`]), else it isn't a date.
fn value(v: ValueRef<'_>, ty: FieldType, unread: &mut Vec<usize>, rounded: Option<&mut Vec<usize>>, k: usize) -> Value {
    match (v, ty) {
        (ValueRef::Null, _) => Value::Null,
        (v, FieldType::Date | FieldType::DateTime) => {
            let text = if let ValueRef::Text(t) = v { Some(String::from_utf8_lossy(t)) } else { None };
            super::gpkg_blob::temporal(text.as_deref(), ty, unread, rounded, k)
        }
        (ValueRef::Integer(i), FieldType::Boolean) => Value::Bool(i != 0),
        (ValueRef::Integer(i), FieldType::Double) => Value::Float(i as f64),
        (ValueRef::Integer(i), _) => Value::Int(i),
        (ValueRef::Real(f), FieldType::Integer) if f.fract() == 0.0 && f.abs() < 9e15 => Value::Int(f as i64),
        (ValueRef::Real(f), _) => Value::Float(f),
        (ValueRef::Text(t), FieldType::Text) => Value::Text(String::from_utf8_lossy(t).into_owned()),
        (ValueRef::Text(t), ty) => Value::parse_source(&String::from_utf8_lossy(t), ty),
        (ValueRef::Blob(_), _) => Value::Null,
    }
}

/// The time a new table's `last_change` records, when `SOURCE_DATE_EPOCH` sets one
/// ([`source_date_epoch`]). Read before an export writes anything, so a bad value refuses it at once.
fn pinned_time() -> Result<Option<i64>> {
    source_date_epoch(std::env::var_os("SOURCE_DATE_EPOCH").as_deref())
}

/// `SOURCE_DATE_EPOCH` read: seconds since 1970 as a whole number, as reproducible builds use it,
/// so the same export run twice writes the same bytes. Unset or empty, the table records the time
/// it is written. A value that isn't such a number (text that isn't Unicode included), or lies
/// past year 9999, refuses the export rather than being ignored.
fn source_date_epoch(raw: Option<&std::ffi::OsStr>) -> Result<Option<i64>> {
    let raw = raw.map(|r| r.to_string_lossy());
    let Some(raw) = raw.as_deref().map(str::trim).filter(|r| !r.is_empty()) else { return Ok(None) };
    match raw.parse::<i64>() {
        Ok(secs) if (0..=253_402_300_799).contains(&secs) => Ok(Some(secs)),
        _ => Err(DataError::Format(format!(
            "SOURCE_DATE_EPOCH is {raw:?}: it must be a whole number of seconds since 1970, up to the end of year 9999"
        ))),
    }
}

/// An SQL identifier, quoted.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn err(e: rusqlite::Error) -> DataError {
    DataError::Format(format!("GeoPackage: {e}"))
}

fn not_gpkg(e: rusqlite::Error) -> DataError {
    DataError::Format(format!("not a GeoPackage: {e}"))
}

#[cfg(test)]
mod tests {
    use super::source_date_epoch;
    use std::ffi::OsStr;

    /// Inspection, opening and later traversal all retain the one uploaded database.
    #[test]
    fn upload_reuses_one_copy_and_removes_it_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("input.gpkg");
        super::super::tests::write_gpkg(&original, true);
        let upload = super::GpkgUpload::new(&std::fs::read(original).unwrap()).unwrap();
        let path = upload.copy.path().to_path_buf();
        assert!(upload.unsupported_table_counts().unwrap().is_empty());
        let source = upload.open(None).unwrap();
        assert_eq!(source.temp.as_ref().unwrap().path(), path);
        assert!(path.is_file());
        drop(source);
        assert!(!path.exists());
    }

    /// Whole seconds from 1970 to the end of year 9999 pin the time; unset or blank leaves it the
    /// export's; anything else, text that isn't Unicode included, refuses the export.
    #[test]
    fn source_date_epoch_takes_whole_seconds_and_refuses_the_rest() {
        let read = |raw: Option<&OsStr>| source_date_epoch(raw).map_err(|e| e.to_string());
        assert_eq!(read(None), Ok(None));
        for (raw, secs) in
            [("", None), ("  ", None), ("0", Some(0)), (" 1000000000 ", Some(1_000_000_000)), ("253402300799", Some(253_402_300_799))]
        {
            assert_eq!(read(Some(OsStr::new(raw))), Ok(secs), "{raw:?}");
        }
        for raw in ["-1", "253402300800", "1.5", "yesterday"] {
            assert!(read(Some(OsStr::new(raw))).is_err_and(|e| e.contains("SOURCE_DATE_EPOCH")), "{raw:?}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            assert!(read(Some(OsStr::from_bytes(b"10\xff"))).is_err(), "not Unicode");
        }
    }
}
