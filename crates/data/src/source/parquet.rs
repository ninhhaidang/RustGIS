//! GeoParquet: columnar files with WKB geometry, read with the pure-Rust `parquet` crate's column
//! readers (no Arrow, which would make the browser build far larger).
//!
//! Extent queries prune whole row groups by the bounding-box statistics of the bbox covering
//! column (GeoParquet 1.1, `covering.bbox`), then filter rows by those bounding boxes before any
//! geometry is decoded. Only the columns needed are read. Files without a covering still work: their
//! geometry is decoded and tested row by row.
//!
//! A file by its web address is read by range: its footer (from the end), then for a read the
//! covering's column chunks of the row groups that meet the extent, then the geometry and field
//! chunks of the rows the read takes, each set fetched in one round. Files without a covering are
//! downloaded whole instead.

use super::remote::{self, Remote};
use super::{Batch, FeatureSource, Reproject, Request, Sampler, SourceInfo, clamp_limit, in_request};
use crate::{DataError, Feature, Field, FieldType, GeometryType, Result, Value, wkb};
use bytes::Bytes;
use parquet::basic::{ConvertedType, LogicalType, Type as Physical};
use parquet::column::reader::{ColumnReader, ColumnReaderImpl};
use parquet::data_type::DataType as PqType;
use parquet::file::metadata::ParquetMetaData;
use parquet::file::reader::{FileReader, RowGroupReader, SerializedFileReader};
use parquet::file::statistics::Statistics;
use parquet::schema::types::ColumnDescPtr;
use rustgis_geom::Extent;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

/// Values read per call.
const BATCH: usize = 8192;

/// Most rows read from one row group (fewer in the browser, whose memory is smaller).
const MAX_GROUP_ROWS: usize = if cfg!(target_arch = "wasm32") { 1 << 21 } else { 1 << 24 };

/// A GeoParquet file.
pub struct ParquetSource {
    info: SourceInfo,
    reader: Arc<dyn FileReader>,
    /// Leaf column index of the geometry.
    geom_col: usize,
    /// Leaf column index of each field, in `info.fields` order.
    field_cols: Vec<usize>,
    /// Leaf column indices of the bbox covering's xmin, ymin, xmax, ymax.
    covering: Option<[usize; 4]>,
    /// The metadata declares the covering, whose null boxes then mean no geometry or an empty
    /// one, neither of which meets an extent.
    declared_covering: bool,
    reproject: Reproject,
    /// The file, when it is read by its web address.
    remote: Option<Arc<Remote>>,
    /// Every row group meeting a read's extent is read (a catalog's index), never a sample of them.
    exact: bool,
    /// The most rows read from one row group: `MAX_GROUP_ROWS`, lowered by tests.
    group_rows: usize,
}

impl ParquetSource {
    pub fn open_path(path: &Path, name: &str) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        Self::new(Arc::new(SerializedFileReader::new(file).map_err(err)?), name, None)
    }

    pub fn open_bytes(bytes: Vec<u8>, name: &str) -> Result<Self> {
        Self::open_shared(super::Shared(Arc::new(bytes)), name)
    }

    /// From bytes shared with the caller, without a copy.
    pub fn open_shared(bytes: super::Shared, name: &str) -> Result<Self> {
        Self::new(Arc::new(SerializedFileReader::new(Bytes::from_owner(bytes)).map_err(err)?), name, None)
    }

    /// A file by its web address, read by range. Fails with [`remote::NEED`] until its footer is
    /// fetched (run it with [`remote::drive`]); a file without a bbox covering can't be read in
    /// place ([`remote::READ_WHOLE`]).
    pub fn open_remote(file: Arc<Remote>, name: &str) -> Result<Self> {
        let reader = remote_reader(&file)?;
        let src = Self::new(reader, name, Some(file))?;
        if src.covering.is_none() {
            return Err(DataError::Unsupported(format!("a GeoParquet without a bbox covering {}", remote::READ_WHOLE)));
        }
        Ok(src)
    }

    /// Adds nested columns (a struct's fields, by dotted path such as `assets.data.href`) to the
    /// fields read, as top-level columns are read. Paths the file hasn't, repeated columns and types
    /// not read are left out.
    pub fn with_columns(mut self, paths: &[&str]) -> Self {
        let reader = self.reader.clone();
        let schema = reader.metadata().file_metadata().schema_descr();
        for p in paths.iter().take(64) {
            if self.info.fields.iter().any(|f| f.name == *p) {
                continue;
            }
            let Some(i) = (0..schema.num_columns()).find(|&i| schema.column(i).path().string() == *p) else { continue };
            let c = schema.column(i);
            if i == self.geom_col || c.max_rep_level() > 0 || self.covering.is_some_and(|cv| cv.contains(&i)) {
                continue;
            }
            if let Some(ty) = field_type(&c) {
                self.info.fields.push(Field::new(p.to_string(), ty));
                self.field_cols.push(i);
            }
        }
        self
    }

    /// Reads take every row group an extent meets (a catalog's index, whose matches are all
    /// wanted), not a sample spread over the file; an extent needing more than one read may fetch
    /// is an error.
    pub fn exact(mut self) -> Self {
        self.exact = true;
        self
    }
}

/// The metadata of a file by its web address, from its footer: fails with [`remote::NEED`] until
/// the footer is fetched.
fn remote_reader(file: &Arc<Remote>) -> Result<Arc<RemoteFile>> {
    let (mut tail, whole) = file.tail(FOOTER).ok_or_else(remote::need)?;
    let meta_len = footer_length(&tail)?;
    let need = meta_len.saturating_add(8);
    if need > tail.len() as u64 {
        if whole || need > MAX_FOOTER {
            return Err(DataError::Format(format!("GeoParquet: a footer of {need} bytes, longer than the file or too long to read")));
        }
        tail = file.tail(need).ok_or_else(remote::need)?.0;
    }
    let end = tail.len().saturating_sub(8);
    let start = end.saturating_sub(usize::try_from(meta_len).unwrap_or(usize::MAX));
    let md = parquet::file::metadata::ParquetMetaDataReader::decode_metadata(tail.get(start..end).unwrap_or_default()).map_err(err)?;
    validate_chunks(&md)?;
    let props = Arc::new(parquet::file::properties::ReaderProperties::builder().build());
    Ok(Arc::new(RemoteFile { chunks: Arc::new(Chunks(file.clone())), metadata: Arc::new(md), props }))
}

impl ParquetSource {
    /// Opens a GeoParquet from its `geo` metadata: geometry column, types, bbox and coordinate
    /// system, noting Z or M values (which reading drops).
    fn new(reader: Arc<dyn FileReader>, name: &str, remote: Option<Arc<Remote>>) -> Result<Self> {
        let md = reader.metadata();
        validate_chunks(md)?;
        let schema = md.file_metadata().schema_descr();
        let kv = md.file_metadata().key_value_metadata();
        let geo: Option<serde_json::Value> = kv
            .and_then(|kv| kv.iter().find(|k| k.key == "geo"))
            .and_then(|k| k.value.as_deref())
            .and_then(|v| serde_json::from_str(v).ok());
        // Top-level leaf columns by name (nested ones are left out of the fields).
        let top = |i: usize| -> Option<String> {
            let c = schema.column(i);
            (c.path().parts().len() == 1 && c.max_rep_level() == 0).then(|| c.name().to_string())
        };
        let leaf_named = |n: &str| (0..schema.num_columns()).find(|&i| top(i).as_deref() == Some(n));
        // The geometry column: the metadata's primary column, else a binary column with a usual name.
        let primary = geo.as_ref().and_then(|g| g.get("primary_column")).and_then(|v| v.as_str()).map(str::to_string).or_else(|| {
            ["geometry", "geom", "wkb_geometry", "the_geom", "wkb"]
                .into_iter()
                .find(|n| leaf_named(n).is_some_and(|i| schema.column(i).physical_type() == Physical::BYTE_ARRAY))
                .map(str::to_string)
        });
        let Some(primary) = primary else {
            return Err(DataError::Format("not a GeoParquet file: no geometry column".into()));
        };
        let col_meta = geo.as_ref().and_then(|g| g.get("columns")).and_then(|c| c.get(&primary));
        let encoding = col_meta.and_then(|c| c.get("encoding")).and_then(|v| v.as_str()).unwrap_or("WKB");
        if !encoding.eq_ignore_ascii_case("wkb") {
            return Err(DataError::Unsupported(format!("GeoParquet geometry encoding '{encoding}' (only WKB is read)")));
        }
        let geom_col = leaf_named(&primary).ok_or_else(|| DataError::Format(format!("no column '{primary}'")))?;
        if schema.column(geom_col).physical_type() != Physical::BYTE_ARRAY {
            return Err(DataError::Format(format!("geometry column '{primary}' is not binary")));
        }
        let mut notes = Vec::new();
        let crs = col_meta.and_then(|c| c.get("crs"));
        let code = crs_code(crs);
        // Another body's system is said, or refused when its coordinates aren't longitude/latitude.
        let other_body = crs.filter(|_| code.is_none()).and_then(crate::raster::planetary::projjson);
        if let Some(crate::raster::planetary::Declared::Refused(e)) = &other_body {
            return Err(DataError::Format(format!("its coordinate system: {e}")));
        }
        if let Some(crate::raster::planetary::Declared::LonLat { note, .. }) = &other_body {
            notes.push(format!("its coordinate system: {note}"));
        } else if code.is_none() && crs.is_some_and(|c| !c.is_null() && !is_crs84(c)) {
            // A coordinate system given without an EPSG code (and not OGC:CRS84) can't be converted.
            notes.push("its coordinate system has no EPSG code; read as longitude/latitude".into());
        }
        let reproject = Reproject::from_epsg(code, &mut notes);
        let covering = find_covering(md, col_meta, &primary);
        let declared_covering = col_meta.and_then(|c| c.get("covering")).and_then(|c| c.get("bbox")).is_some();

        let mut fields = Vec::new();
        let mut field_cols = Vec::new();
        // Dates and times, read as the whole numbers they're stored as.
        let mut as_numbers = Vec::new();
        for i in 0..schema.num_columns() {
            if i == geom_col || covering.is_some_and(|c| c.contains(&i)) {
                continue;
            }
            let Some(name) = top(i) else { continue };
            let c = schema.column(i);
            if let Some(ty) = field_type(&c) {
                if let Some(unit) = time_unit(&c) {
                    as_numbers.push((name.clone(), unit));
                }
                fields.push(Field::new(name, ty));
                field_cols.push(i);
            }
        }
        // Top-level columns not read: those holding a leaf that is neither a field, the geometry
        // nor its covering's box (whose struct may also hold zmin and zmax, bounds of the geometry).
        let read: HashSet<usize> = field_cols.iter().chain([&geom_col]).chain(covering.iter().flatten()).copied().collect();
        let box_root = covering.and_then(|[x, ..]| schema.column(x).path().parts().first().cloned());
        let unread: HashSet<String> = (0..schema.num_columns())
            .filter(|i| !read.contains(i))
            .filter_map(|i| match schema.column(i).path().parts() {
                [r, bound] if Some(r) == box_root.as_ref() && matches!(bound.as_str(), "zmin" | "zmax") => None,
                parts => parts.first().cloned(),
            })
            .collect();
        let geometries = geo.as_ref().and_then(|g| g.get("columns")).and_then(|c| c.as_object());
        let left_out: Vec<(String, String)> = schema
            .root_schema()
            .get_fields()
            .iter()
            .filter(|t| unread.contains(t.name()))
            .map(|t| {
                let kind = if geometries.is_some_and(|g| g.contains_key(t.name())) { "geometry" } else { column_kind(t) };
                (t.name().to_string(), kind.to_string())
            })
            .collect();
        let geometry_type = col_meta
            .and_then(|c| c.get("geometry_types"))
            .and_then(|t| t.as_array())
            .and_then(|t| t.iter().filter_map(|v| v.as_str()).find_map(geometry_type_name))
            .unwrap_or(GeometryType::None);
        let bbox = col_meta.and_then(|c| c.get("bbox")).and_then(|b| b.as_array()).and_then(|b| {
            let v: Vec<f64> = b.iter().filter_map(|x| x.as_f64()).collect();
            match v.as_slice() {
                [a, b, c, d] => Some(Extent::new(*a, *b, *c, *d)),
                [a, b, _, c, d, _] => Some(Extent::new(*a, *b, *c, *d)),
                _ => None,
            }
        });
        let extent = match bbox {
            Some(e) => reproject.extent_to_lonlat(&e),
            None => covering.map(|c| reproject.extent_to_lonlat(&groups_extent(md, c))).unwrap_or(Extent::EMPTY),
        };
        // Types such as "Point Z" (every geometry has Z or M values when every type does), or a bbox
        // with Z bounds. Without types (any geometry), the first rows are looked at below.
        let types: Vec<&str> = col_meta
            .and_then(|c| c.get("geometry_types"))
            .and_then(|t| t.as_array())
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .collect();
        let has_zm = |v: &&str| v.ends_with(" Z") || v.ends_with(" M") || v.ends_with(" ZM");
        let all_zm = !types.is_empty() && types.iter().all(has_zm);
        let zm = types.iter().any(has_zm) || col_meta.and_then(|c| c.get("bbox")).and_then(|b| b.as_array()).is_some_and(|b| b.len() == 6);
        let count = u64::try_from(md.file_metadata().num_rows()).ok();
        let mut info = SourceInfo {
            name: name.to_string(),
            format: "GeoParquet",
            geometry_type,
            fields,
            extent,
            count,
            crs: reproject.label(),
            indexed: covering.is_some(),
            notes,
            zm_dropped: false,
        };
        info.note_columns(
            &left_out,
            ["of a type RustGIS has no field for was left out", "of types RustGIS has no field for were left out"],
        );
        info.note_columns(&as_numbers, ["of dates or times was read as whole numbers", "of dates or times were read as whole numbers"]);
        // An empty file loses nothing.
        if zm && count != Some(0) {
            info.drop_zm(if all_zm { count } else { None });
        }
        let untyped = types.is_empty() && !zm;
        let mut src = ParquetSource {
            info,
            reader,
            geom_col,
            field_cols,
            covering,
            declared_covering,
            reproject,
            remote,
            exact: false,
            group_rows: MAX_GROUP_ROWS,
        };
        if untyped {
            match src.peek_zm() {
                Some(true) => src.info.drop_zm(None),
                Some(false) => {}
                None => src.info.notes.push("couldn't tell whether it has Z or M values, which RustGIS doesn't keep".into()),
            }
        }
        if src.info.geometry_type == GeometryType::None {
            src.info.geometry_type = super::peek_kind(&src);
        }
        Ok(src)
    }

    /// Whether any of the first 16 geometries has Z or M values; `None` when they can't be read
    /// here (a remote file's first row group not fetched yet).
    fn peek_zm(&self) -> Option<bool> {
        let md = self.reader.metadata();
        if md.num_row_groups() == 0 {
            return Some(false);
        }
        if let Some(file) = &self.remote
            && !want_chunks(file, md, &[0], &[self.geom_col]).ok()?
        {
            return None;
        }
        let rows = usize::try_from(md.row_group(0).num_rows()).unwrap_or(0).min(16);
        let rg = self.reader.get_row_group(0).ok()?;
        let geoms = read_bytes(rg.as_ref(), self.geom_col, &vec![true; rows]).ok()?;
        Some(geoms.iter().filter_map(|b| b.as_deref()).any(wkb::has_zm))
    }
}

impl ParquetSource {
    /// A source that reads at most `rows` of a row group, for tests of the cap.
    #[cfg(test)]
    pub(crate) fn with_group_rows(mut self, rows: usize) -> Self {
        self.group_rows = rows;
        self
    }

    /// The rows of row group `g` read. Row counts come from the file: bounded by what the
    /// geometry column chunk holds, and by a cap, so a crafted count can't size a huge allocation.
    fn rows_in(&self, md: &ParquetMetaData, g: usize) -> usize {
        let rg = md.row_group(g);
        let rows = usize::try_from(rg.num_rows()).unwrap_or(0);
        let values = rg.columns().get(self.geom_col).and_then(|c| usize::try_from(c.num_values()).ok()).unwrap_or(0);
        rows.min(values).min(self.group_rows)
    }

    /// Whether inferred bounds may omit or count rows that only their geometry can test.
    /// Unknown null counts require the same fallback as known nulls; declared coverings promise
    /// that a box is present if and only if a non-empty geometry is present.
    fn needs_geometry_filter(&self, md: &ParquetMetaData, g: usize) -> bool {
        let Some(c) = self.covering else { return true };
        !self.declared_covering
            && c.into_iter()
                .chain([self.geom_col])
                .any(|i| md.row_group(g).columns().get(i).and_then(|col| col.statistics()).and_then(|s| s.null_count_opt()) != Some(0))
    }

    /// Which rows of row group `g` have a bounding box meeting the native extent `e`, read from
    /// the covering columns `c` alone. Null boxes cannot match a declared covering; inferred
    /// bounds leave them for the geometry to decide.
    fn covered_rows(&self, md: &ParquetMetaData, g: usize, e: &Extent, c: [usize; 4]) -> Result<Vec<bool>> {
        let rg = self.reader.get_row_group(g).map_err(err)?;
        let n = self.rows_in(md, g);
        let cols: Vec<Vec<Option<f64>>> = c.iter().map(|&i| read_f64(rg.as_ref(), i, n)).collect::<Result<_>>()?;
        if cols.iter().any(|col| col.len() != n) {
            return Err(DataError::Format(format!("GeoParquet: row group {g} has covering columns shorter than its rows")));
        }
        let v = |k: usize, r: usize| cols.get(k).and_then(|c| c.get(r).copied().flatten());
        Ok((0..n)
            .map(|r| {
                let [Some(min_x), Some(min_y), Some(max_x), Some(max_y)] = [v(0, r), v(1, r), v(2, r), v(3, r)] else {
                    return !self.declared_covering;
                };
                let be = Extent::new(min_x, min_y, max_x, max_y);
                be.is_empty() || be.intersects(e)
            })
            .collect())
    }
}

impl FeatureSource for ParquetSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    /// Keeps rows with null geometry in whole reads; they have no location to match an extent.
    fn read(&self, req: &Request) -> Result<Batch> {
        let limit = clamp_limit(req.limit);
        let md = self.reader.metadata();
        let native = req.extent.map(|e| self.reproject.extent_from_lonlat(&e));
        let rows_in = |g: usize| self.rows_in(md, g);
        // Row groups whose bounding box meets the extent.
        let mut groups: Vec<usize> = (0..md.num_row_groups())
            .filter(|&g| match (native, self.covering) {
                (Some(e), Some(c)) => self.needs_geometry_filter(md, g) || group_extent(md, g, c).is_none_or(|ge| ge.intersects(&e)),
                _ => true,
            })
            .collect();
        // Read by address: no more row groups than a read may fetch (an evenly spread part of them,
        // a sample), and the covering's chunks of those asked for together.
        // A row group wholly inside the extent with no null boxes: every row matches,
        // its covering unread. Unknown null counts must be checked in the covering.
        let inside = |g: usize| match (native, self.covering) {
            (Some(e), Some(c)) => {
                let all_boxes = c.iter().all(|&i| {
                    md.row_group(g).columns().get(i).and_then(|col| col.statistics()).and_then(|s| s.null_count_opt()) == Some(0)
                });
                all_boxes && group_extent(md, g, c).is_some_and(|ge| contains(&e, &ge))
            }
            _ => true,
        };
        let mut thinned = 1usize;
        if let Some(file) = &self.remote {
            // Far more rows than the read takes: a sample of row groups evenly spread over them
            // (the read samples their rows), so what is fetched stays near what is shown. No more
            // bytes than a read may fetch.
            let rows: u64 = groups.iter().map(|&g| rows_in(g) as u64).sum();
            if rows > (limit as u64).saturating_mul(8) && !groups.is_empty() && !self.exact {
                thinned = usize::try_from(rows.div_ceil((limit as u64).saturating_mul(4).max(1))).unwrap_or(usize::MAX).max(2);
            }
            let cols: Vec<usize> = self.covering.map(|c| c.to_vec()).unwrap_or_default();
            let all: Vec<usize> = cols.iter().copied().chain([self.geom_col]).chain(self.field_cols.iter().copied()).collect();
            let cost: u64 = groups.iter().step_by(thinned).map(|&g| chunks_size(md, g, &all)).sum();
            if cost > remote::READ_BUDGET && self.exact {
                return Err(DataError::Format(format!(
                    "GeoParquet: the extent needs {} MB of the file, more than one read takes",
                    cost >> 20
                )));
            }
            if cost > remote::READ_BUDGET {
                thinned = thinned.saturating_mul(usize::try_from(cost.div_ceil(remote::READ_BUDGET)).unwrap_or(usize::MAX).max(2));
            }
            if thinned > 1 {
                groups = groups.into_iter().step_by(thinned).collect();
            }
            let edge: Vec<usize> = groups.iter().copied().filter(|&g| !inside(g)).collect();
            if native.is_some() && !want_chunks(file, md, &edge, &cols)? {
                return Err(remote::need());
            }
        }

        // Which rows of those groups match: with a covering and an extent, exactly the rows whose
        // bounding box meets it (read from the covering alone); otherwise all of them.
        let mut keep: Vec<Option<Vec<bool>>> = Vec::with_capacity(groups.len());
        for &g in &groups {
            keep.push(match (native, self.covering) {
                (Some(_), Some(_)) if inside(g) => None,
                (Some(e), Some(c)) => Some(self.covered_rows(md, g, &e, c)?),
                _ => None,
            });
        }
        let matched: u64 =
            groups.iter().zip(&keep).map(|(&g, k)| k.as_ref().map(|k| k.iter().filter(|&&b| b).count()).unwrap_or(rows_in(g)) as u64).sum();
        // Without a covering, or with nullable inferred bounds, test geometry before sampling
        // for the whole request. Otherwise the match count is known, and every `step`-th is read.
        let late = native.is_some() && groups.iter().any(|&g| self.needs_geometry_filter(md, g));
        let mut sample = if late { Sampler::new(limit) } else { Sampler::with_total(limit, matched) };
        // The rows of each group the sample takes, decided before anything else is read.
        let takes: Vec<Vec<bool>> = groups
            .iter()
            .zip(&keep)
            .map(|(&g, k)| {
                (0..rows_in(g)).map(|r| k.as_ref().is_none_or(|k| k.get(r).copied().unwrap_or(false)) && (late || sample.wants())).collect()
            })
            .collect();
        // Read by address: the geometry and field chunks of the groups any row is taken from.
        if let Some(file) = &self.remote {
            let cols: Vec<usize> = [self.geom_col].into_iter().chain(self.field_cols.iter().copied()).collect();
            let used: Vec<usize> = groups.iter().zip(&takes).filter(|(_, t)| t.iter().any(|&t| t)).map(|(&g, _)| g).collect();
            if !want_chunks(file, md, &used, &cols)? {
                return Err(remote::need());
            }
        }
        let mut skipped = 0;
        let mut starts = Vec::with_capacity(md.num_row_groups());
        let mut first_row = 0u64;
        // From the declared row counts, not the capped ones, so ids stay put whatever is read.
        for g in 0..md.num_row_groups() {
            starts.push(first_row);
            first_row = first_row.saturating_add(u64::try_from(md.row_group(g).num_rows()).unwrap_or(0));
        }
        for (&g, take) in groups.iter().zip(&takes) {
            if !take.iter().any(|&t| t) {
                continue;
            }
            let rg = self.reader.get_row_group(g).map_err(err)?;
            let geoms = read_bytes(rg.as_ref(), self.geom_col, take)?;
            let attrs: Vec<Vec<Value>> = self.field_cols.iter().map(|&c| read_values(rg.as_ref(), c, take)).collect::<Result<_>>()?;
            // A column shorter than its row group (a truncated or corrupt file) would pair values
            // with the wrong rows: refuse it.
            let wanted = take.iter().filter(|&&t| t).count();
            if geoms.len() != wanted || attrs.iter().any(|a| a.len() != wanted) {
                return Err(DataError::Format(format!("GeoParquet: row group {g} has columns shorter than its rows")));
            }
            let base = starts.get(g).copied().unwrap_or(0);
            // The columns hold the taken rows only, in order: the k-th is row `r`.
            for (k, (r, _)) in take.iter().enumerate().filter(|(_, t)| **t).enumerate() {
                // Null geometry is valid; malformed non-null WKB is skipped and counted.
                let geometry = match geoms.get(k).and_then(|b| b.as_deref()) {
                    Some(bytes) => {
                        let Some(geometry) = wkb::decode(bytes).and_then(|g| self.reproject.geometry(g)) else {
                            skipped += 1;
                            continue;
                        };
                        Some(geometry)
                    }
                    None => None,
                };
                let matches_extent = geometry.as_ref().map_or(req.extent.is_none(), |g| in_request(g, req));
                if !matches_extent || (late && !sample.wants()) {
                    continue;
                }
                let values = attrs.iter().map(|col| col.get(k).cloned().unwrap_or(Value::Null)).collect();
                sample.keep(Feature { id: base.saturating_add(r as u64 + 1), geometry, attrs: values });
            }
        }
        let matched = (if late { sample.seen() } else { matched }).saturating_mul(thinned as u64);
        Ok(Batch { matched, complete: sample.complete() && thinned == 1, skipped, features: sample.items })
    }

    fn remote(&self) -> Option<Arc<Remote>> {
        self.remote.clone()
    }

    /// A file or an upload; a file by its web address is read a view at a time, not in full.
    fn visits_chunks(&self) -> bool {
        self.remote.is_none()
    }

    /// A row group at a time, in file order: the features, ids and skipped rows of a complete
    /// read of the same extent. The taken rows of one row group are decoded together before
    /// they are chunked, so the peak is a row group's columns plus a chunk. Null geometry is
    /// retained without an extent filter, keeping its row and attributes in whole exports.
    fn visit_chunks(&self, extent: Option<Extent>, bytes: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        let mut chunks = super::chunk_memory::Chunker::new(bytes, visit)?;
        if self.remote.is_some() {
            return Err(DataError::Unsupported("a whole GeoParquet by its web address read a chunk at a time".into()));
        }
        let req = Request { extent, limit: usize::MAX };
        let md = self.reader.metadata();
        let native = extent.map(|e| self.reproject.extent_from_lonlat(&e));
        let mut out = Batch { complete: true, ..Batch::default() };
        let mut base = 0u64;
        for g in 0..md.num_row_groups() {
            // From the declared row counts, as a read numbers them.
            let start = base;
            let declared = u64::try_from(md.row_group(g).num_rows()).unwrap_or(0);
            base = base.saturating_add(declared);
            let area = match (native, self.covering) {
                (Some(e), Some(c)) => Some((e, c, group_extent(md, g, c))),
                _ => None,
            };
            if let Some((e, _, Some(ge))) = area
                && !self.needs_geometry_filter(md, g)
                && !ge.intersects(&e)
            {
                continue;
            }
            // A read takes at most `MAX_GROUP_ROWS` of a row group, and no more than its geometry
            // column holds. A traversal promises every row, so a group it can't read whole is an
            // error, not a quietly shorter export; it is refused before this group is read.
            let readable = self.rows_in(md, g);
            if declared > self.group_rows as u64 {
                return Err(DataError::Format(format!(
                    "GeoParquet: row group {g} has {declared} rows, more than the {} read from one",
                    self.group_rows
                )));
            }
            if declared > readable as u64 {
                return Err(DataError::Format(format!("GeoParquet: row group {g} has columns shorter than its rows")));
            }
            let take = match area {
                Some((e, c, ge)) if !ge.is_some_and(|ge| contains(&e, &ge)) => self.covered_rows(md, g, &e, c)?,
                _ => vec![true; readable],
            };
            if !take.iter().any(|&t| t) {
                continue;
            }
            let rg = self.reader.get_row_group(g).map_err(err)?;
            let geoms = read_bytes(rg.as_ref(), self.geom_col, &take)?;
            let attrs: Vec<Vec<Value>> = self.field_cols.iter().map(|&c| read_values(rg.as_ref(), c, &take)).collect::<Result<_>>()?;
            let wanted = take.iter().filter(|&&t| t).count();
            if geoms.len() != wanted || attrs.iter().any(|a| a.len() != wanted) {
                return Err(DataError::Format(format!("GeoParquet: row group {g} has columns shorter than its rows")));
            }
            for (k, (r, _)) in take.iter().enumerate().filter(|(_, t)| **t).enumerate() {
                let geometry = match geoms.get(k).and_then(|b| b.as_deref()) {
                    Some(bytes) => {
                        let Some(geometry) = wkb::decode(bytes).and_then(|geom| self.reproject.geometry(geom)) else {
                            out.skipped += 1;
                            continue;
                        };
                        Some(geometry)
                    }
                    None => None,
                };
                if !geometry.as_ref().map_or(req.extent.is_none(), |g| in_request(g, &req)) {
                    continue;
                }
                let values = attrs.iter().map(|col| col.get(k).cloned().unwrap_or(Value::Null)).collect();
                out.matched += 1;
                chunks.push(Feature { id: start.saturating_add(r as u64 + 1), geometry, attrs: values })?;
            }
        }
        chunks.flush()?;
        Ok(out)
    }
}

/// The footer read first from a file by its address: enough for most files' metadata.
const FOOTER: u64 = 256 << 10;
/// The longest footer read.
const MAX_FOOTER: u64 = 64 << 20;

/// The metadata's length, from a file's last 8 bytes.
fn footer_length(tail: &[u8]) -> Result<u64> {
    let n = tail.len();
    if n < 12 || tail.get(n - 4..) != Some(b"PAR1".as_slice()) {
        return Err(DataError::Format("not a Parquet file".into()));
    }
    let len = tail.get(n - 8..n - 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes).unwrap_or(0);
    Ok(u64::from(len))
}

/// Rejects invalid ranges before any column reader can reach `ColumnChunkMetaData::byte_range`,
/// which asserts on negative offsets and lengths. Also covers geometry probing and vector reads.
fn validate_chunks(md: &ParquetMetaData) -> Result<()> {
    for (g, group) in md.row_groups().iter().enumerate() {
        for (col, chunk) in group.columns().iter().enumerate() {
            let data = chunk.data_page_offset();
            let start = chunk.dictionary_page_offset().unwrap_or(data);
            let size = chunk.compressed_size();
            // A dictionary page precedes the data pages; reading from a later one would skip data.
            if data < 0 || start < 0 || start > data || size < 0 || start.checked_add(size).is_none() {
                return Err(DataError::Format(format!("GeoParquet: row group {g}, column {col} has an invalid column chunk range")));
            }
        }
    }
    Ok(())
}

/// A column chunk's bytes: (start, length), from its offsets (checked, never negative).
fn chunk_range(md: &ParquetMetaData, g: usize, col: usize) -> Option<(u64, u64)> {
    let c = md.row_groups().get(g)?.columns().get(col)?;
    let start = c.dictionary_page_offset().unwrap_or(c.data_page_offset());
    Some((u64::try_from(start).ok()?, u64::try_from(c.compressed_size()).ok()?))
}

/// The bytes of some columns' chunks of a row group.
fn chunks_size(md: &ParquetMetaData, g: usize, cols: &[usize]) -> u64 {
    cols.iter().filter_map(|&c| chunk_range(md, g, c)).map(|(_, n)| n).sum()
}

/// Asks for some columns' chunks of some row groups: `true` when all are held.
fn want_chunks(file: &Remote, md: &ParquetMetaData, groups: &[usize], cols: &[usize]) -> Result<bool> {
    let mut held = true;
    for &g in groups {
        for &c in cols {
            let (at, len) =
                chunk_range(md, g, c).ok_or_else(|| DataError::Format(format!("GeoParquet: row group {g} has no column {c}")))?;
            if len > remote::READ_BUDGET {
                return Err(DataError::Format(format!("GeoParquet: a column chunk of {len} bytes is too large to read in place")));
            }
            held &= file.want(at, len);
        }
    }
    Ok(held)
}

/// A file by its address, as the `parquet` crate reads it: from the parts held (a read of one not
/// held notes it missing and fails).
struct Chunks(Arc<Remote>);

impl parquet::file::reader::Length for Chunks {
    fn len(&self) -> u64 {
        // Not known (the footer is read from the end): reads are bounded by the parts held.
        u64::MAX
    }
}

impl parquet::file::reader::ChunkReader for Chunks {
    type T = bytes::buf::Reader<Bytes>;

    fn get_read(&self, start: u64) -> parquet::errors::Result<Self::T> {
        use bytes::Buf;
        self.0.from(start, remote::MIN_FETCH).map(Buf::reader).ok_or_else(|| parquet::errors::ParquetError::General(remote::NEED.into()))
    }

    fn get_bytes(&self, start: u64, length: usize) -> parquet::errors::Result<Bytes> {
        match self.0.bytes(start, length as u64) {
            Some(b) if b.len() == length => Ok(b),
            Some(_) => Err(parquet::errors::ParquetError::EOF("the file ends early".into())),
            None => Err(parquet::errors::ParquetError::General(remote::NEED.into())),
        }
    }
}

/// A file by its address, with its metadata read from the footer.
struct RemoteFile {
    chunks: Arc<Chunks>,
    metadata: Arc<ParquetMetaData>,
    props: parquet::file::properties::ReaderPropertiesPtr,
}

impl FileReader for RemoteFile {
    fn metadata(&self) -> &ParquetMetaData {
        &self.metadata
    }

    fn num_row_groups(&self) -> usize {
        self.metadata.num_row_groups()
    }

    fn get_row_group(&self, i: usize) -> parquet::errors::Result<Box<dyn RowGroupReader + '_>> {
        let rg = self.metadata.row_groups().get(i).ok_or_else(|| parquet::errors::ParquetError::General(format!("no row group {i}")))?;
        Ok(Box::new(parquet::file::serialized_reader::SerializedRowGroupReader::new(
            self.chunks.clone(),
            rg,
            self.metadata.page_index_for_row_group(i),
            self.props.clone(),
        )?))
    }

    fn get_row_iter(
        &self,
        projection: Option<parquet::schema::types::Type>,
    ) -> parquet::errors::Result<parquet::record::reader::RowIter<'_>> {
        parquet::record::reader::RowIter::from_file(projection, self)
    }
}

fn err(e: parquet::errors::ParquetError) -> DataError {
    DataError::Format(format!("GeoParquet: {e}"))
}

/// The reader of a row group's column, its pages checked by [`CheckedPages`].
fn column_reader(rg: &dyn RowGroupReader, col: usize) -> Result<ColumnReader> {
    let desc = rg.metadata().column(col).column_descr_ptr();
    let bits = match desc.physical_type() {
        Physical::BOOLEAN => 1,
        Physical::INT32 | Physical::FLOAT | Physical::BYTE_ARRAY => 32,
        Physical::INT64 | Physical::DOUBLE => 64,
        Physical::INT96 => 96,
        Physical::FIXED_LEN_BYTE_ARRAY => u64::try_from(desc.type_length()).unwrap_or(0).max(1) * 8,
    };
    let pages = CheckedPages { pages: rg.get_column_page_reader(col).map_err(err)?, bits };
    Ok(parquet::column::reader::get_column_reader(desc, Box::new(pages)))
}

/// A column's pages, a dictionary page refused when it says it holds more values than its bytes
/// can: the `parquet` crate makes room for the count it is told before decoding any (a few bytes
/// of header can ask for tens of gigabytes).
struct CheckedPages {
    pages: Box<dyn parquet::column::page::PageReader>,
    /// The fewest bits one value of the column takes in a dictionary page (plain encoded).
    bits: u64,
}

impl Iterator for CheckedPages {
    type Item = parquet::errors::Result<parquet::column::page::Page>;

    /// The next page, checked as [`PageReader::get_next_page`](parquet::column::page::PageReader::get_next_page) checks it.
    fn next(&mut self) -> Option<Self::Item> {
        use parquet::column::page::PageReader;
        self.get_next_page().transpose()
    }
}

impl parquet::column::page::PageReader for CheckedPages {
    /// The next page, or an error for a dictionary page whose count its bytes can't hold.
    fn get_next_page(&mut self) -> parquet::errors::Result<Option<parquet::column::page::Page>> {
        let page = self.pages.get_next_page()?;
        if let Some(parquet::column::page::Page::DictionaryPage { buf, num_values, .. }) = &page
            && u64::from(*num_values).saturating_mul(self.bits) > (buf.len() as u64).saturating_mul(8)
        {
            return Err(parquet::errors::ParquetError::General(format!(
                "a dictionary page says it holds {num_values} values, more than its {} bytes can",
                buf.len()
            )));
        }
        Ok(page)
    }

    /// The next page's header, unchecked: a peek decodes no values.
    fn peek_next_page(&mut self) -> parquet::errors::Result<Option<parquet::column::page::PageMetadata>> {
        self.pages.peek_next_page()
    }

    /// Skips the next page unchecked: a skipped page is never decoded.
    fn skip_next_page(&mut self) -> parquet::errors::Result<()> {
        self.pages.skip_next_page()
    }

    /// Whether the next page starts a record, as the column's own pages say.
    fn at_record_boundary(&mut self) -> parquet::errors::Result<bool> {
        self.pages.at_record_boundary()
    }
}

fn geometry_type_name(t: &str) -> Option<GeometryType> {
    let t = t.trim_end_matches(" Z").trim_end_matches(" M").trim_end_matches(" ZM");
    match t {
        "Point" | "MultiPoint" => Some(GeometryType::Point),
        "LineString" | "MultiLineString" => Some(GeometryType::Polyline),
        "Polygon" | "MultiPolygon" => Some(GeometryType::Polygon),
        _ => None,
    }
}

/// Whether a PROJJSON `crs` is OGC:CRS84 (or EPSG:4326's longitude/latitude twin).
fn is_crs84(crs: &serde_json::Value) -> bool {
    let id = crs.get("id");
    let authority = id.and_then(|i| i.get("authority")).and_then(|a| a.as_str()).unwrap_or("");
    let code = id.and_then(|i| i.get("code")).map(|c| c.to_string().trim_matches('"').to_string()).unwrap_or_default();
    (authority.eq_ignore_ascii_case("OGC") && code.eq_ignore_ascii_case("CRS84"))
        || crs.get("name").and_then(|n| n.as_str()).is_some_and(|n| n.contains("CRS84"))
}

/// The EPSG code of a GeoParquet `crs` (PROJJSON); absent means OGC:CRS84 (longitude/latitude).
fn crs_code(crs: Option<&serde_json::Value>) -> Option<u32> {
    let crs = crs?;
    if crs.is_null() {
        return None;
    }
    let id = crs.get("id")?;
    let authority = id.get("authority").and_then(|a| a.as_str()).unwrap_or("");
    let code = id.get("code").and_then(|c| c.as_u64().or_else(|| c.as_str().and_then(|s| s.parse().ok())))?;
    match authority.to_ascii_uppercase().as_str() {
        "EPSG" => u32::try_from(code).ok(),
        // OGC:CRS84 is longitude/latitude.
        _ => None,
    }
}

/// The bbox covering's leaf columns: the declared `covering.bbox`, else a `<geometry>_bbox` or
/// `bbox` struct.
fn find_covering(md: &ParquetMetaData, col_meta: Option<&serde_json::Value>, primary: &str) -> Option<[usize; 4]> {
    let schema = md.file_metadata().schema_descr();
    let leaf = |path: &[&str]| -> Option<usize> {
        let want = path.join(".");
        (0..schema.num_columns()).find(|&i| schema.column(i).path().string() == want)
    };
    let declared = col_meta.and_then(|c| c.get("covering")).and_then(|c| c.get("bbox"));
    let paths: Vec<Vec<String>> = match declared {
        Some(b) => ["xmin", "ymin", "xmax", "ymax"]
            .iter()
            .map(|k| b.get(*k).and_then(|p| p.as_array()).map(|p| p.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()))
            .collect::<Option<Vec<Vec<String>>>>()?,
        None => {
            let base = [format!("{primary}_bbox"), "bbox".to_string()].into_iter().find(|b| leaf(&[b.as_str(), "xmin"]).is_some())?;
            ["xmin", "ymin", "xmax", "ymax"].iter().map(|k| vec![base.clone(), (*k).to_string()]).collect()
        }
    };
    let mut leaves = [0usize; 4];
    for (i, p) in paths.iter().enumerate() {
        let refs: Vec<&str> = p.iter().map(String::as_str).collect();
        let l = leaf(&refs)?;
        if !matches!(schema.column(l).physical_type(), Physical::FLOAT | Physical::DOUBLE) || schema.column(l).max_rep_level() > 0 {
            return None;
        }
        *leaves.get_mut(i)? = l;
    }
    Some(leaves)
}

fn stat(s: Option<&Statistics>, max: bool) -> Option<f64> {
    match s? {
        Statistics::Float(v) => (if max { v.max_opt() } else { v.min_opt() }).map(|x| f64::from(*x)),
        Statistics::Double(v) => (if max { v.max_opt() } else { v.min_opt() }).copied(),
        _ => None,
    }
}

/// A row group's bounding box from its covering statistics.
fn group_extent(md: &ParquetMetaData, g: usize, c: [usize; 4]) -> Option<Extent> {
    let rg = md.row_groups().get(g)?;
    let s = |i: usize, max: bool| stat(rg.columns().get(*c.get(i)?)?.statistics(), max);
    let e = Extent::new(s(0, false)?, s(1, false)?, s(2, true)?, s(3, true)?);
    (!e.is_empty()).then_some(e)
}

/// Whether `outer` holds all of `inner`.
fn contains(outer: &Extent, inner: &Extent) -> bool {
    outer.min_x <= inner.min_x && outer.min_y <= inner.min_y && outer.max_x >= inner.max_x && outer.max_y >= inner.max_y
}

fn groups_extent(md: &ParquetMetaData, c: [usize; 4]) -> Extent {
    (0..md.num_row_groups()).filter_map(|g| group_extent(md, g, c)).fold(Extent::EMPTY, |a, e| a.union(&e))
}

fn is_text(c: &ColumnDescPtr) -> bool {
    matches!(c.logical_type_ref(), Some(LogicalType::String | LogicalType::Enum | LogicalType::Json))
        || matches!(c.converted_type(), ConvertedType::UTF8 | ConvertedType::ENUM | ConvertedType::JSON)
}

/// The scale of a decimal column (its values are integers × 10^-scale).
fn decimal_scale(c: &ColumnDescPtr) -> Option<i32> {
    match c.logical_type_ref() {
        Some(LogicalType::Decimal(d)) => Some(d.scale),
        _ if c.converted_type() == ConvertedType::DECIMAL => Some(c.type_scale()),
        _ => None,
    }
}

/// What a top-level column not read as a field holds, for the note saying it was left out: a
/// list, map or struct, or a decimal, geometry, INT96 timestamp or binary value.
fn column_kind(t: &parquet::schema::types::Type) -> &'static str {
    let info = t.get_basic_info();
    let logical = info.logical_type_ref();
    if info.has_repetition() && info.repetition() == parquet::basic::Repetition::REPEATED {
        return "list";
    }
    if t.is_group() {
        return match (logical, info.converted_type()) {
            (Some(LogicalType::List), _) | (_, ConvertedType::LIST) => "list",
            (Some(LogicalType::Map), _) | (_, ConvertedType::MAP | ConvertedType::MAP_KEY_VALUE) => "map",
            _ => "struct",
        };
    }
    match (t.get_physical_type(), logical, info.converted_type()) {
        (_, Some(LogicalType::Decimal(_)), _) | (_, _, ConvertedType::DECIMAL) => "decimal",
        (_, Some(LogicalType::Geometry(_) | LogicalType::Geography(_)), _) => "geometry",
        (Physical::INT96, ..) => "INT96 timestamp",
        _ => "binary",
    }
}

/// What a date or time column's whole numbers count ("days since 1970-01-01"); `None` for other
/// columns.
fn time_unit(c: &ColumnDescPtr) -> Option<String> {
    use parquet::basic::TimeUnit;
    let unit = |u: &TimeUnit| match u {
        TimeUnit::MILLIS => "milliseconds",
        TimeUnit::MICROS => "microseconds",
        TimeUnit::NANOS => "nanoseconds",
    };
    let (count, since) = match (c.logical_type_ref(), c.converted_type()) {
        (Some(LogicalType::Timestamp(t)), _) => (unit(&t.unit), "1970-01-01"),
        (Some(LogicalType::Time(t)), _) => (unit(&t.unit), "midnight"),
        (Some(LogicalType::Date), _) | (None, ConvertedType::DATE) => ("days", "1970-01-01"),
        (None, ConvertedType::TIMESTAMP_MILLIS) => ("milliseconds", "1970-01-01"),
        (None, ConvertedType::TIMESTAMP_MICROS) => ("microseconds", "1970-01-01"),
        (None, ConvertedType::TIME_MILLIS) => ("milliseconds", "midnight"),
        (None, ConvertedType::TIME_MICROS) => ("microseconds", "midnight"),
        _ => return None,
    };
    Some(format!("{count} since {since}"))
}

/// A field type for a top-level column; `None` for types not read (binary, INT96, fixed-length).
fn field_type(c: &ColumnDescPtr) -> Option<FieldType> {
    Some(match c.physical_type() {
        Physical::BOOLEAN => FieldType::Boolean,
        Physical::INT32 | Physical::INT64 if decimal_scale(c).is_some() => FieldType::Double,
        Physical::INT32 | Physical::INT64 => FieldType::Integer,
        Physical::FLOAT | Physical::DOUBLE => FieldType::Double,
        Physical::BYTE_ARRAY if is_text(c) => FieldType::Text,
        _ => return None,
    })
}

/// Reads the rows `take` marks of a flat column, in order (`None` where a row is null). Runs of
/// rows not taken are skipped without being decoded into values, so a sample of a large row
/// group doesn't hold all of it.
fn read_column<T: PqType>(r: &mut ColumnReaderImpl<T>, take: &[bool], max_def: i16) -> Result<Vec<Option<T::T>>> {
    let wanted = take.iter().filter(|&&t| t).count();
    let mut out: Vec<Option<T::T>> = Vec::with_capacity(wanted.min(1 << 24));
    let mut defs = Vec::new();
    let mut values = Vec::new();
    let mut i = 0;
    while i < take.len() {
        let on = take.get(i).copied().unwrap_or(false);
        let mut j = i;
        while j < take.len() && take.get(j).copied().unwrap_or(false) == on {
            j += 1;
        }
        let mut left = j - i;
        i = j;
        if !on {
            if r.skip_records(left).map_err(err)? < left {
                break;
            }
            continue;
        }
        while left > 0 {
            defs.clear();
            values.clear();
            let (records, _, levels) = r.read_records(left.min(BATCH), Some(&mut defs), None, &mut values).map_err(err)?;
            if records == 0 {
                return Ok(out);
            }
            left = left.saturating_sub(records);
            if max_def == 0 {
                out.extend(values.drain(..).map(Some));
            } else {
                let mut it = values.drain(..);
                for d in defs.iter().take(levels) {
                    out.push(if *d == max_def { it.next() } else { None });
                }
            }
        }
    }
    Ok(out)
}

/// Reads a field column as attribute values, for the rows `take` marks, in order.
fn read_values(rg: &dyn RowGroupReader, col: usize, take: &[bool]) -> Result<Vec<Value>> {
    let desc = rg.metadata().column(col).column_descr_ptr();
    let max_def = desc.max_def_level();
    let scale = decimal_scale(&desc).map(|s| 10f64.powi(-s));
    let int = |v: Option<i64>| match (v, scale) {
        (None, _) => Value::Null,
        (Some(i), Some(f)) => Value::Float(i as f64 * f),
        (Some(i), None) => Value::Int(i),
    };
    Ok(match column_reader(rg, col)? {
        ColumnReader::BoolColumnReader(mut r) => {
            read_column(&mut r, take, max_def)?.into_iter().map(|v| v.map(Value::Bool).unwrap_or_default()).collect()
        }
        ColumnReader::Int32ColumnReader(mut r) => read_column(&mut r, take, max_def)?.into_iter().map(|v| int(v.map(i64::from))).collect(),
        ColumnReader::Int64ColumnReader(mut r) => read_column(&mut r, take, max_def)?.into_iter().map(int).collect(),
        ColumnReader::FloatColumnReader(mut r) => {
            read_column(&mut r, take, max_def)?.into_iter().map(|v| v.map(|f| Value::Float(f64::from(f))).unwrap_or_default()).collect()
        }
        ColumnReader::DoubleColumnReader(mut r) => {
            read_column(&mut r, take, max_def)?.into_iter().map(|v| v.map(Value::Float).unwrap_or_default()).collect()
        }
        ColumnReader::ByteArrayColumnReader(mut r) => read_column(&mut r, take, max_def)?
            .into_iter()
            .map(|v| v.map(|b| Value::Text(String::from_utf8_lossy(b.data()).into_owned())).unwrap_or_default())
            .collect(),
        _ => Vec::new(),
    })
}

/// Reads a binary column (the WKB geometry), for the rows `take` marks, in order.
fn read_bytes(rg: &dyn RowGroupReader, col: usize, take: &[bool]) -> Result<Vec<Option<Vec<u8>>>> {
    let max_def = rg.metadata().column(col).column_descr().max_def_level();
    match column_reader(rg, col)? {
        ColumnReader::ByteArrayColumnReader(mut r) => {
            Ok(read_column(&mut r, take, max_def)?.into_iter().map(|v| v.map(|b| b.data().to_vec())).collect())
        }
        _ => Err(DataError::Format("GeoParquet: the geometry column is not binary".into())),
    }
}

/// Reads every one of `n` rows of a float or double column as doubles (the covering).
fn read_f64(rg: &dyn RowGroupReader, col: usize, n: usize) -> Result<Vec<Option<f64>>> {
    let max_def = rg.metadata().column(col).column_descr().max_def_level();
    let all = vec![true; n];
    match column_reader(rg, col)? {
        ColumnReader::FloatColumnReader(mut r) => Ok(read_column(&mut r, &all, max_def)?.into_iter().map(|v| v.map(f64::from)).collect()),
        ColumnReader::DoubleColumnReader(mut r) => read_column(&mut r, &all, max_def),
        _ => Ok(Vec::new()),
    }
}

/// Points with a vector each (patch embeddings, such as Earth Index's), read from a GeoParquet by
/// its web address: the points in an extent, and their vectors.
#[derive(Clone, Debug, Default)]
pub struct Vectors {
    /// Each point's longitude and latitude.
    pub points: Vec<(f64, f64)>,
    /// Values a vector.
    pub dims: usize,
    /// The vectors, one after another (`points.len() × dims` values).
    pub values: Vec<f32>,
}

/// The most values one vector may have.
const MAX_DIMS: usize = 8192;
/// The most vector values one read keeps (in all; 4 bytes each).
const MAX_VECTOR_VALUES: usize = if cfg!(target_arch = "wasm32") { 8 << 20 } else { 32 << 20 };

/// Reads the points of a GeoParquet in `extent` (longitude/latitude) with the vectors of a list
/// column (`column`, such as `embedding`), from a file by its web address. The geometry of every
/// row group is read (it has no bbox covering), then the vectors of those holding points in the
/// extent: at most `max_groups` of them and `max_points` points, or an error saying how many the
/// extent needs. Fails with [`remote::NEED`] until the bytes it needs are fetched (run it with
/// [`remote::drive`]).
pub fn read_vectors(file: &Arc<Remote>, extent: &Extent, column: &str, max_groups: usize, max_points: usize) -> Result<Vectors> {
    let reader = remote_reader(file)?;
    let md = reader.metadata();
    let schema = md.file_metadata().schema_descr();
    let leaf = |pred: &dyn Fn(&ColumnDescPtr) -> bool| (0..schema.num_columns()).find(|&i| pred(&schema.column(i)));
    let geom = leaf(&|c| c.path().string() == "geometry" && c.physical_type() == Physical::BYTE_ARRAY)
        .ok_or_else(|| DataError::Format("GeoParquet: no geometry column".into()))?;
    let prefix = format!("{column}.");
    let vec_col = leaf(&|c| {
        c.path().string().starts_with(&prefix) && c.max_rep_level() == 1 && matches!(c.physical_type(), Physical::FLOAT | Physical::DOUBLE)
    })
    .ok_or_else(|| DataError::Format(format!("GeoParquet: no list column '{column}' of numbers")))?;
    let groups: Vec<usize> = (0..md.num_row_groups()).collect();
    if groups.iter().map(|&g| chunks_size(md, g, &[geom])).sum::<u64>() > remote::READ_BUDGET {
        return Err(DataError::Format("GeoParquet: its geometry is too large to read in place".into()));
    }
    if !want_chunks(file, md, &groups, &[geom])? {
        return Err(remote::need());
    }
    // The points in the extent, by row group.
    let mut found: Vec<(usize, Vec<(usize, (f64, f64))>)> = Vec::new();
    let mut total = 0usize;
    let mut hit = 0usize;
    for &g in &groups {
        let rg = reader.get_row_group(g).map_err(err)?;
        let rows = usize::try_from(rg.metadata().num_rows()).unwrap_or(0).min(MAX_GROUP_ROWS);
        let geoms = read_bytes(rg.as_ref(), geom, &vec![true; rows])?;
        let inside: Vec<(usize, (f64, f64))> = geoms
            .iter()
            .enumerate()
            .filter_map(|(r, b)| match b.as_deref().and_then(wkb::decode) {
                Some(crate::Geometry::Point(c)) if extent.contains(c) => Some((r, (c.x, c.y))),
                _ => None,
            })
            .collect();
        if !inside.is_empty() {
            total = total.saturating_add(inside.len());
            hit += 1;
            // Past the limits the points are only counted, for the error.
            if total <= max_points && hit <= max_groups {
                found.push((g, inside));
            }
        }
    }
    if hit > max_groups || total > max_points {
        return Err(DataError::Format(format!("the area holds {total} points in {hit} row groups, more than are read at once: zoom in")));
    }
    let used: Vec<usize> = found.iter().map(|(g, _)| *g).collect();
    if !want_chunks(file, md, &used, &[vec_col])? {
        return Err(remote::need());
    }
    let mut out = Vectors::default();
    for (g, inside) in &found {
        let rg = reader.get_row_group(*g).map_err(err)?;
        let rows = usize::try_from(rg.metadata().num_rows()).unwrap_or(0).min(MAX_GROUP_ROWS);
        let mut take = vec![false; rows];
        for (r, _) in inside {
            if let Some(t) = take.get_mut(*r) {
                *t = true;
            }
        }
        let lists = read_lists(rg.as_ref(), vec_col, &take)?;
        for (r, p) in inside {
            let Some(v) = lists.get(*r) else { continue };
            if out.dims == 0 && !v.is_empty() && v.len() <= MAX_DIMS {
                out.dims = v.len();
            }
            if v.len() == out.dims && out.dims > 0 {
                if out.values.len().saturating_add(v.len()) > MAX_VECTOR_VALUES {
                    return Err(DataError::Format(format!(
                        "the area's vectors hold more than {MAX_VECTOR_VALUES} values, more than are read at once: zoom in"
                    )));
                }
                out.points.push(*p);
                out.values.extend_from_slice(v);
            }
        }
    }
    Ok(out)
}

/// Reads a list column of numbers over the rows of a row group (`take.len()` of them): each row's
/// values for the rows `take` marks, empty for the others (whose values are passed over, not
/// kept) and for a null or empty list.
fn read_lists(rg: &dyn RowGroupReader, col: usize, take: &[bool]) -> Result<Vec<Vec<f32>>> {
    let n = take.len();
    let desc = rg.metadata().column(col).column_descr_ptr();
    let max_def = desc.max_def_level();
    let mut out: Vec<Vec<f32>> = Vec::with_capacity(n.min(1 << 20));
    let (mut defs, mut reps) = (Vec::new(), Vec::new());
    // Values kept over the row group: bounded, so a crafted file can't fill memory.
    let mut budget = 1usize << 24;
    macro_rules! lists {
        ($r:expr, $conv:expr) => {{
            let mut values = Vec::new();
            let mut left = n;
            while left > 0 {
                defs.clear();
                reps.clear();
                values.clear();
                let (records, _, levels) = $r.read_records(left.min(BATCH), Some(&mut defs), Some(&mut reps), &mut values).map_err(err)?;
                if records == 0 {
                    break;
                }
                left = left.saturating_sub(records);
                let mut it = values.drain(..);
                let mut kept = false;
                for (d, r) in defs.iter().zip(&reps).take(levels) {
                    if *r == 0 {
                        kept = take.get(out.len()).copied().unwrap_or(false);
                        out.push(Vec::new());
                    }
                    if *d == max_def
                        && let Some(v) = it.next()
                        && kept
                        && let Some(last) = out.last_mut()
                    {
                        budget = budget.checked_sub(1).ok_or_else(|| DataError::Format("GeoParquet: lists too long to read".into()))?;
                        last.push($conv(v));
                    }
                }
            }
        }};
    }
    match column_reader(rg, col)? {
        ColumnReader::FloatColumnReader(mut r) => lists!(r, |v: f32| v),
        ColumnReader::DoubleColumnReader(mut r) => lists!(r, |v: f64| v as f32),
        _ => return Err(DataError::Format("GeoParquet: the list column holds no numbers".into())),
    }
    Ok(out)
}

/// Rows per row group written.
const WRITE_GROUP_ROWS: usize = 65_536;

/// A column name not taken by a field (`base`, else `base_1`, `base_2`, …).
fn free_name(fields: &[Field], base: &str) -> String {
    let taken = |n: &str| fields.iter().any(|f| f.name.eq_ignore_ascii_case(n));
    if !taken(base) {
        return base.to_string();
    }
    (1..).map(|i| format!("{base}_{i}")).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
}

/// The GeoParquet name of a geometry's type, as WKB encodes it.
fn wkb_type_name(g: &crate::Geometry) -> &'static str {
    match g {
        crate::Geometry::Point(_) => "Point",
        crate::Geometry::MultiPoint(_) => "MultiPoint",
        crate::Geometry::Line(_) => "MultiLineString",
        crate::Geometry::Polygon(_) => "MultiPolygon",
    }
}

/// Writes a dataset as GeoParquet 1.1 (WGS 84 longitude/latitude, WKB geometry, Zstd level 9), with a
/// `bbox` covering column so readers can skip rows and row groups by extent. A table without
/// geometry is written as plain Parquet.
pub fn write(ds: &crate::Dataset) -> Result<Vec<u8>> {
    write_features(ds.geometry_type, &ds.fields, Vec::new(), &mut |add| add(&ds.features)).map(|(bytes, _)| bytes)
}

/// An Integer column's value, `None` for one it can't hold: a whole float fits; a fraction, a
/// float out of range or text that isn't a whole number would lose its value.
fn int_cell(value: &Value) -> Option<i64> {
    match value {
        Value::Int(x) => Some(*x),
        Value::Bool(b) => Some(i64::from(*b)),
        Value::Float(x) if x.fract() == 0.0 && x.abs() < 9.0e18 => Some(*x as i64),
        Value::Text(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// A Boolean column's value, `None` for one it can't hold (text, a float).
fn bool_cell(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Int(x) => Some(*x != 0),
        _ => None,
    }
}

/// Whether the GeoParquet writer writes `value` as null in a column of type `ty` although it
/// isn't null (in a Double column: text that isn't a number, NaN or an infinity).
fn unfit(ty: FieldType, value: &Value) -> bool {
    !value.is_null()
        && match ty {
            FieldType::Integer => int_cell(value).is_none(),
            FieldType::Double => value.as_f64().is_none(),
            FieldType::Boolean => bool_cell(value).is_none(),
            FieldType::Text => false,
        }
}

/// The notice for values a GeoParquet export wrote as null because their column's type couldn't
/// hold them, given the count per field (as `write_features` returns); `None` when there are
/// none. Names the first five fields.
pub fn unfit_note(fields: &[Field], counts: &[usize], target: &str) -> Option<String> {
    let total = counts.iter().fold(0usize, |a, n| a.saturating_add(*n));
    if total == 0 {
        return None;
    }
    let named: Vec<&str> = fields.iter().zip(counts).filter(|(_, n)| **n > 0).map(|(f, _)| f.name.as_str()).collect();
    // Names and the target flattened and bounded as the Shapefile notices' are, on one line.
    let list: Vec<String> = named.iter().take(5).map(|n| format!("'{}'", crate::io::dbf_notice_name(n))).collect();
    let more = if named.len() > 5 { format!(" and {} more", named.len() - 5) } else { String::new() };
    let what = if total == 1 {
        "1 value couldn't be written as its column's type and was".to_string()
    } else {
        format!("{total} values couldn't be written as their columns' types and were")
    };
    let target: String = target.chars().map(crate::io::dbf_notice_char).collect();
    Some(format!("{target}: {what} written as null, in {}{more}", list.join(", ")))
}

/// [`unfit_note`] for writing the whole of `ds` as GeoParquet.
pub fn values_note(ds: &crate::Dataset, target: &str) -> Option<String> {
    let mut counts = vec![0usize; ds.fields.len()];
    for row in &ds.features {
        for ((field, value), n) in ds.fields.iter().zip(&row.attrs).zip(counts.iter_mut()) {
            if unfit(field.ty, value) {
                *n = n.saturating_add(1);
            }
        }
    }
    unfit_note(&ds.fields, &counts, target)
}

/// Writes GeoParquet to `out`: the features `feed` hands to the function it is given, a run at
/// a time, their attributes named by `fields`. Each run is written as row groups of at most
/// `WRITE_GROUP_ROWS` rows, so only one run is held (a run's last group can be smaller, and a
/// streamed export's groups end with its chunks); the geometry types and bounding box the `geo`
/// metadata gives are gathered as the rows go and written with the footer.
pub(crate) fn write_features<W: std::io::Write + Send>(
    geometry_type: GeometryType,
    fields: &[Field],
    out: W,
    feed: &mut dyn FnMut(&mut dyn FnMut(&[Feature]) -> Result<()>) -> Result<()>,
) -> Result<(W, Vec<usize>)> {
    use parquet::basic::{Compression, Encoding, Repetition, ZstdLevel};
    use parquet::data_type::{BoolType, ByteArray, ByteArrayType, DoubleType, Int64Type};
    use parquet::file::metadata::KeyValue;
    use parquet::file::properties::WriterProperties;
    use parquet::file::writer::SerializedFileWriter;
    use parquet::schema::types::{ColumnPath, Type};

    let has_geom = geometry_type != GeometryType::None;
    let geom_name = free_name(fields, "geometry");
    let bbox_name = free_name(fields, "bbox");
    let prim = |name: &str, t: Physical, rep: Repetition, logical: Option<LogicalType>| {
        Type::primitive_type_builder(name, t).with_repetition(rep).with_logical_type(logical).build().map(Arc::new).map_err(err)
    };
    let mut cols = Vec::new();
    if has_geom {
        cols.push(prim(&geom_name, Physical::BYTE_ARRAY, Repetition::OPTIONAL, None)?);
        let corners = ["xmin", "ymin", "xmax", "ymax"]
            .iter()
            .map(|k| prim(k, Physical::DOUBLE, Repetition::REQUIRED, None))
            .collect::<Result<Vec<_>>>()?;
        cols.push(Arc::new(
            Type::group_type_builder(&bbox_name).with_repetition(Repetition::OPTIONAL).with_fields(corners).build().map_err(err)?,
        ));
    }
    for f in fields {
        cols.push(match f.ty {
            FieldType::Integer => prim(&f.name, Physical::INT64, Repetition::OPTIONAL, None)?,
            FieldType::Double => prim(&f.name, Physical::DOUBLE, Repetition::OPTIONAL, None)?,
            FieldType::Boolean => prim(&f.name, Physical::BOOLEAN, Repetition::OPTIONAL, None)?,
            FieldType::Text => prim(&f.name, Physical::BYTE_ARRAY, Repetition::OPTIONAL, Some(LogicalType::String))?,
        });
    }
    let schema = Arc::new(Type::group_type_builder("schema").with_fields(cols).build().map_err(err)?);

    let mut props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(9).map_err(err)?))
        .set_created_by("RustGIS".to_string());
    if has_geom {
        // Keep the covering for range reads; split its doubles into byte streams so Zstd can
        // compress their shared exponent bytes without rounding any coordinate values.
        for corner in ["xmin", "ymin", "xmax", "ymax"] {
            let path = ColumnPath::new(vec![bbox_name.clone(), corner.to_string()]);
            props = props.set_column_dictionary_enabled(path.clone(), false).set_column_encoding(path, Encoding::BYTE_STREAM_SPLIT);
        }
    }
    let mut w = SerializedFileWriter::new(out, schema, Arc::new(props.build())).map_err(err)?;

    /// The row group's next column writer; an error should the schema end first.
    fn next_column<'a, W: std::io::Write + Send>(
        rg: &'a mut parquet::file::writer::SerializedRowGroupWriter<'_, W>,
    ) -> Result<parquet::file::writer::SerializedColumnWriter<'a>> {
        rg.next_column().map_err(err)?.ok_or_else(|| DataError::Format("GeoParquet: the schema ended early".into()))
    }
    /// The present values of an optional column and its definition levels.
    fn levels<T>(values: impl Iterator<Item = Option<T>>) -> (Vec<T>, Vec<i16>) {
        let (mut vals, mut defs) = (Vec::new(), Vec::new());
        for v in values {
            defs.push(i16::from(v.is_some()));
            vals.extend(v);
        }
        (vals, defs)
    }
    let mut types: Vec<&'static str> = Vec::new();
    let mut extent = Extent::EMPTY;
    // Per field, the values written as null because its type couldn't hold them.
    let mut unfit_counts = vec![0usize; fields.len()];
    feed(&mut |features| {
        for rows in features.chunks(WRITE_GROUP_ROWS) {
            crate::progress::step(rows.len())?;
            for g in rows.iter().filter_map(|f| f.geometry.as_ref()) {
                let t = wkb_type_name(g);
                if !types.contains(&t) {
                    types.push(t);
                }
                extent = extent.union(&g.extent());
            }
            let mut rg = w.next_row_group().map_err(err)?;
            if has_geom {
                let (vals, defs) = levels(rows.iter().map(|f| f.geometry.as_ref().map(|g| ByteArray::from(wkb::encode(g)))));
                let mut c = next_column(&mut rg)?;
                c.typed::<ByteArrayType>().write_batch(&vals, Some(&defs), None).map_err(err)?;
                c.close().map_err(err)?;
                // A feature's bounding box, `None` without geometry (or with an empty one).
                let boxes: Vec<Option<Extent>> =
                    rows.iter().map(|f| f.geometry.as_ref().map(|g| g.extent()).filter(|e| !e.is_empty())).collect();
                for corner in 0..4 {
                    let (vals, defs) = levels(boxes.iter().map(|b| {
                        b.map(|e| match corner {
                            0 => e.min_x,
                            1 => e.min_y,
                            2 => e.max_x,
                            _ => e.max_y,
                        })
                    }));
                    let mut c = next_column(&mut rg)?;
                    c.typed::<DoubleType>().write_batch(&vals, Some(&defs), None).map_err(err)?;
                    c.close().map_err(err)?;
                }
            }
            for (i, f) in fields.iter().enumerate() {
                let attr = |r: &Feature| r.attrs.get(i).cloned().unwrap_or(Value::Null);
                // A value the column's type can't hold is written as null, and counted.
                if let Some(n) = unfit_counts.get_mut(i) {
                    *n = n.saturating_add(rows.iter().filter(|r| r.attrs.get(i).is_some_and(|v| unfit(f.ty, v))).count());
                }
                let mut c = next_column(&mut rg)?;
                match f.ty {
                    FieldType::Integer => {
                        let (v, d) = levels(rows.iter().map(|r| int_cell(&attr(r))));
                        c.typed::<Int64Type>().write_batch(&v, Some(&d), None).map_err(err)?;
                    }
                    FieldType::Double => {
                        let (v, d) = levels(rows.iter().map(|r| attr(r).as_f64()));
                        c.typed::<DoubleType>().write_batch(&v, Some(&d), None).map_err(err)?;
                    }
                    FieldType::Boolean => {
                        let (v, d) = levels(rows.iter().map(|r| bool_cell(&attr(r))));
                        c.typed::<BoolType>().write_batch(&v, Some(&d), None).map_err(err)?;
                    }
                    FieldType::Text => {
                        let (v, d) = levels(rows.iter().map(|r| match attr(r) {
                            Value::Null => None,
                            v => Some(ByteArray::from(v.to_string().into_bytes())),
                        }));
                        c.typed::<ByteArrayType>().write_batch(&v, Some(&d), None).map_err(err)?;
                    }
                }
                c.close().map_err(err)?;
            }
            rg.close().map_err(err)?;
        }
        Ok(())
    })?;
    // A run cancelled during the last row group gets no footer either.
    crate::progress::check()?;
    if has_geom {
        types.sort_unstable();
        let e = extent;
        let mut col = serde_json::json!({
            "encoding": "WKB",
            "geometry_types": types,
            "covering": {"bbox": {
                "xmin": [bbox_name, "xmin"], "ymin": [bbox_name, "ymin"],
                "xmax": [bbox_name, "xmax"], "ymax": [bbox_name, "ymax"],
            }},
        });
        if !e.is_empty() {
            col["bbox"] = serde_json::json!([e.min_x, e.min_y, e.max_x, e.max_y]);
        }
        let geo = serde_json::json!({"version": "1.1.0", "primary_column": geom_name, "columns": {geom_name.clone(): col}});
        w.append_key_value_metadata(KeyValue::new("geo".to_string(), geo.to_string()));
    }
    Ok((w.into_inner().map_err(err)?, unfit_counts))
}
