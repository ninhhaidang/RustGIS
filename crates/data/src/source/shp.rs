//! Shapefiles read in place: a large `.shp` with its `.dbf`, `.prj` and `.cpg`, a view at a time.
//!
//! Opening builds or reuses a disk-backed R-tree and record directory. Queries use a bounded
//! SQLite page cache, then read matching shapes and `.dbf` rows by their offsets.
//! Smaller Shapefiles are still read whole by `io::read_path`,
//! so they can be edited.
//!
//! The files are hostile input: record lengths, part and point counts are checked against the
//! bytes there, and a record that doesn't parse is skipped and counted.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

mod cache;
mod index;

#[doc(hidden)]
pub use cache::use_temporary_index_dir;
pub use cache::{Emptied, empty_index_cache};

use super::{Batch, FeatureSource, Reproject, Request, SourceInfo, clamp_limit, in_request};
use crate::io::group_rings;
use crate::{DataError, Feature, Field, FieldType, Geometry, GeometryType, Result, Value};
use rustgis_geom::{Coord, Extent};

fn bad(msg: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("Shapefile: {msg}"))
}

/// The number of records a Shapefile holds, from the size of its `.shx` (without reading it).
pub fn record_count(path: &Path) -> Option<u64> {
    let len = std::fs::metadata(path.with_extension("shx")).ok()?.len();
    Some(len.checked_sub(100)? / 8)
}

/// Where a record's content is in the `.shp`, and how long it is.
#[derive(Clone, Copy)]
struct Place {
    offset: u32,
    len: u32,
}

/// A `.dbf` column.
struct Column {
    ty: u8,
    /// Byte offset in the row (after the deletion flag) and width.
    offset: usize,
    len: usize,
    out: FieldType,
}

/// A `.dbf` table's layout: rows of fixed width after the header.
struct Table {
    header_len: u64,
    row_len: usize,
    rows: u64,
    columns: Vec<Column>,
    ldid: u8,
}

/// A Shapefile read in place.
pub struct ShpSource {
    info: SourceInfo,
    shp: PathBuf,
    dbf: PathBuf,
    index: index::Index,
    table: Table,
    encoding: crate::dbf::DbfEncoding,
    reproject: Reproject,
}

/// Bytes read ahead from the files: shapes and rows near each other in a read share a fill.
const BUFFER: usize = 1 << 20;

/// The largest record read: one past it is skipped rather than given a buffer that size, since a
/// record's length is only checked against the file's, which can be gigabytes (#1055).
const MAX_RECORD: u64 = crate::io::MAX_BYTES;

impl ShpSource {
    /// Opens `path` (the `.shp`) with the `.dbf` and optional `.prj` and `.cpg` beside it.
    pub fn open_path(path: &Path, name: &str) -> Result<ShpSource> {
        Self::open_path_in(path, name, None)
    }

    /// Opens a Shapefile with an optional source CRS replacing its `.prj`. The spatial index
    /// stays in native coordinates; requests and results use the chosen CRS for conversion.
    pub fn open_path_in(path: &Path, name: &str, crs: Option<crate::raster::Crs>) -> Result<ShpSource> {
        if let Some(c) = crs {
            c.transformer().map_err(DataError::Format)?;
        }
        let dbf = crate::io::dbf_sidecar(path);
        if !dbf.is_file() {
            return Err(bad(format!("{} is missing", dbf.display())));
        }
        let mut notes = Vec::new();
        let table = read_table(&dbf)?;
        let cpg = crate::dbf::read_cpg(path, &mut notes);
        let encoding = crate::dbf::encoding(cpg.as_deref(), table.ldid, &mut notes);
        let shp_len = std::fs::metadata(path)?.len();
        if shp_len > u64::from(u32::MAX) {
            return Err(bad("the .shp is larger than the format's 4 GB"));
        }
        let mut f = BufReader::with_capacity(BUFFER, crate::provenance::open(path)?);
        let mut header = [0u8; 100];
        f.read_exact(&mut header).map_err(|_| bad("the .shp header is cut short"))?;
        if be_i32(&header, 0) != Some(9994) {
            return Err(bad("not a .shp file"));
        }
        let shape_type = le_i32(&header, 32).unwrap_or(0);
        let geometry_type = match shape_type {
            1 | 11 | 21 | 8 | 18 | 28 => GeometryType::Point,
            3 | 13 | 23 => GeometryType::Polyline,
            5 | 15 | 25 => GeometryType::Polygon,
            _ => GeometryType::None,
        };
        let native = Extent::new(
            le_f64(&header, 36).unwrap_or(0.0),
            le_f64(&header, 44).unwrap_or(0.0),
            le_f64(&header, 52).unwrap_or(0.0),
            le_f64(&header, 60).unwrap_or(0.0),
        );
        drop(f);
        let index = index::Index::open(path, shp_len, &mut notes)?;
        if table.rows != index.count {
            notes.push(format!("the .shp holds {} shapes and the .dbf {} rows; rows are matched by position", index.count, table.rows));
        }
        let reproject = match crs {
            Some(c) => Reproject::from_crs(c, &mut notes),
            None => shp_reproject(crate::raster::read_prj(path).as_deref(), &mut notes)?,
        };
        // The records' own boxes frame the layer, as the index holds them (rounded outward to f32);
        // with none (no records, or every shape null or not a number) it has no extent, never the
        // 0, 0, 0, 0 a header often holds then (Null Island). The header's box, exact, stands for
        // them when it frames them all; one that doesn't (a corner that isn't a number, #1985, or
        // stale after records were added) is set aside, and said.
        let native = index.bounds.map(|[x0, y0, x1, y1]| {
            // The header, a box itself, rounded outward as the records' boxes are, holds them all:
            // so a header written exactly always passes, as does one short of a shape by so little
            // that both round to the same f32 (under a meter in UTM).
            let frames = [native.min_x, native.min_y, native.max_x, native.max_y].iter().all(|v| v.is_finite())
                && !native.is_empty()
                && f32_down(native.min_x) <= x0
                && f32_down(native.min_y) <= y0
                && f32_up(native.max_x) >= x1
                && f32_up(native.max_y) >= y1;
            if frames {
                native
            } else {
                notes.push("the .shp header's bounding box doesn't frame its shapes: the layer's extent is taken from the shapes".into());
                Extent::new(f64::from(x0), f64::from(y0), f64::from(x1), f64::from(y1))
            }
        });
        let extent = native.map_or(Extent::EMPTY, |n| reproject.extent_to_lonlat(&n));
        if reproject.is_lonlat()
            && !extent.is_empty()
            && (extent.min_x < -180.5 || extent.max_x > 180.5 || extent.min_y < -90.5 || extent.max_y > 90.5)
        {
            notes.push("coordinates are not longitude/latitude; RustGIS reads them as WGS 84 and they may draw out of place".into());
        }
        let fields = table
            .columns
            .iter()
            .zip(read_names(&dbf, table.columns.len(), &encoding.decoder())?)
            .map(|(c, n)| Field::new(n, c.out))
            .collect();
        let mut info = SourceInfo {
            name: name.to_string(),
            format: "Shapefile",
            geometry_type,
            fields,
            extent,
            count: Some(index.count),
            // A system the .prj defines, as one given, by its PROJ definition, so the layer keeps it
            // (`SourceInfo::coordinate_system`) as a whole read does.
            crs: reproject.crs.definition_text(),
            indexed: index.indexed(),
            notes,
            zm_dropped: false,
        };
        // PointZ … MultiPointM: the header's type says every shape carries Z or M values.
        if matches!(shape_type, 11 | 13 | 15 | 18 | 21 | 23 | 25 | 28) {
            info.drop_zm(Some(index.count));
        }
        Ok(ShpSource { info, shp: path.to_path_buf(), dbf, index, table, encoding, reproject })
    }
}

impl FeatureSource for ShpSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    /// Samples indexed records, falling back to geometry matches when the native view is refused.
    /// The fallback counts records and checks Cancel before reading each one.
    fn read(&self, req: &Request) -> Result<Batch> {
        let encoding = self.encoding.decoder();
        let limit = clamp_limit(req.limit);
        let extent = req.extent.and_then(|e| self.reproject.extent_from_lonlat(&e));
        // Keep indexed zero-limit counts; a refused view has no cheap candidate count.
        if limit == 0 && req.extent.is_some() && extent.is_none() {
            return Err(super::unbounded_count());
        }
        if extent.is_some_and(|e| e.is_empty()) {
            return Ok(Batch { complete: super::stride(0, limit) == 1, ..Batch::default() });
        }
        let bounds = extent.map(|e| [e.min_x as f32, e.min_y as f32, e.max_x as f32, e.max_y as f32]);
        let mut shp = Cursor::open(&self.shp)?;
        let mut dbf = Cursor::open(&self.dbf)?;
        let mut buf = Vec::new();
        let mut row = vec![0u8; self.table.row_len];
        let mut features = Vec::new();
        let (mut skipped, mut unread_dates, mut null_text) = (0, Vec::new(), Vec::new());
        // An extent the file's system couldn't bound (#1925) has no boxes for the index to sample
        // by: every record is visited, tested, and the matches sampled as they come.
        let late = req.extent.is_some() && bounds.is_none();
        let mut sample = super::Sampler::new(limit);
        if late {
            crate::progress::begin(self.info.count.and_then(|n| usize::try_from(n).ok()).unwrap_or(0));
        }
        let mut each = |i: u32, place: Place| {
            if late {
                crate::progress::step(1)?;
            }
            if u64::from(place.len) > MAX_RECORD {
                skipped += 1;
                return Ok(());
            }
            buf.resize(place.len as usize, 0);
            if shp.read_at(u64::from(place.offset), &mut buf).is_err() {
                skipped += 1;
                return Ok(());
            }
            let geometry = match parse_shape(&buf) {
                // A coordinate that isn't a number (NaN or infinite) has no place: the record is
                // left out and counted, as one that can't be converted (#1985).
                Ok(Some(g)) if !g.is_finite() => {
                    skipped += 1;
                    return Ok(());
                }
                Ok(Some(g)) => match self.reproject.geometry(g) {
                    Some(g) => Some(g),
                    None => {
                        skipped += 1;
                        return Ok(());
                    }
                },
                Ok(None) => None,
                Err(()) => {
                    skipped += 1;
                    return Ok(());
                }
            };
            // The index's boxes are rounded outward to f32, so it can match a shape just outside. A
            // shape without geometry doesn't match an extent, bounded natively or not.
            if geometry.as_ref().map_or(req.extent.is_some(), |g| !in_request(g, req)) {
                return Ok(());
            }
            let attrs = if u64::from(i) < self.table.rows {
                let at = self.table.header_len + u64::from(i) * self.table.row_len as u64;
                if dbf.read_at(at, &mut row).is_err() {
                    skipped += 1;
                    return Ok(());
                }
                // A deleted row: its shape is left out too.
                if row.first() == Some(&b'*') {
                    return Ok(());
                }
                if late && !sample.wants() {
                    return Ok(());
                }
                self.table.values(&row, &encoding, &mut unread_dates, &mut null_text)
            } else {
                if late && !sample.wants() {
                    return Ok(());
                }
                vec![Value::Null; self.table.columns.len()]
            };
            let f = Feature { id: u64::from(i) + 1, geometry, attrs };
            if late {
                sample.keep(f)
            } else {
                features.push(f)
            }
            Ok(())
        };
        let (matched, complete) = if late {
            self.index.visit_complete(None, &mut each)?;
            features = std::mem::take(&mut sample.items);
            (sample.seen(), sample.complete())
        } else {
            self.index.visit(bounds, limit, &mut each)?
        };
        Ok(Batch { features, matched, complete, skipped, unread_dates, rounded_dates: Vec::new(), null_text })
    }
    fn streams_chunks(&self) -> bool {
        true
    }
    /// Visits located shapes in a spatial request; geometry-less rows belong only to whole reads.
    /// Counts scanned records and checks Cancel before filtering them.
    fn visit_chunks(&self, extent: Option<Extent>, bytes: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        let encoding = self.encoding.decoder();
        let mut chunks = super::chunk_memory::Chunker::new(bytes, visit)?;
        let req = Request { extent, limit: usize::MAX };
        let extent = extent.and_then(|e| self.reproject.extent_from_lonlat(&e));
        if extent.is_some_and(|e| e.is_empty()) {
            return Ok(Batch { complete: true, ..Batch::default() });
        }
        let bounds = extent.map(|e| [e.min_x as f32, e.min_y as f32, e.max_x as f32, e.max_y as f32]);
        let mut shp = Cursor::open(&self.shp)?;
        let mut dbf = Cursor::open(&self.dbf)?;
        let mut buf = Vec::new();
        let mut row = vec![0u8; self.table.row_len];
        let (mut skipped, mut unread_dates, mut null_text) = (0, Vec::new(), Vec::new());
        // The same directory, ordering, extent rules and read-only scan fallback as sampled
        // reads, with no interaction row cap. The index visits records without materializing
        // their directory or the complete feature collection in memory.
        // Every record id fits u32. This format bound visits them all while keeping SQLite's
        // LIMIT within its signed integer range on 64-bit hosts.
        crate::progress::begin(self.info.count.and_then(|n| usize::try_from(n).ok()).unwrap_or(0));
        let (matched, complete) = self.index.visit_complete(bounds, |i, place| {
            crate::progress::step(1)?;
            if u64::from(place.len) > MAX_RECORD {
                skipped += 1;
                return Ok(());
            }
            buf.resize(place.len as usize, 0);
            if shp.read_at(u64::from(place.offset), &mut buf).is_err() {
                skipped += 1;
                return Ok(());
            }
            let geometry = match parse_shape(&buf) {
                // A coordinate that isn't a number (NaN or infinite) has no place: the record is
                // left out and counted, as one that can't be converted (#1985).
                Ok(Some(g)) if !g.is_finite() => {
                    skipped += 1;
                    return Ok(());
                }
                Ok(Some(g)) => match self.reproject.geometry(g) {
                    Some(g) => Some(g),
                    None => {
                        skipped += 1;
                        return Ok(());
                    }
                },
                Ok(None) => None,
                Err(()) => {
                    skipped += 1;
                    return Ok(());
                }
            };
            // As in `read`, the shape itself is checked.
            if geometry.as_ref().map_or(req.extent.is_some(), |g| !in_request(g, &req)) {
                return Ok(());
            }
            let attrs = if u64::from(i) < self.table.rows {
                let at = self.table.header_len + u64::from(i) * self.table.row_len as u64;
                if dbf.read_at(at, &mut row).is_err() {
                    skipped += 1;
                    return Ok(());
                }
                // A deleted row: its shape is left out too.
                if row.first() == Some(&b'*') {
                    return Ok(());
                }
                self.table.values(&row, &encoding, &mut unread_dates, &mut null_text)
            } else {
                vec![Value::Null; self.table.columns.len()]
            };
            chunks.push(Feature { id: u64::from(i) + 1, geometry, attrs })
        })?;
        chunks.flush()?;
        Ok(Batch { features: Vec::new(), matched, complete, skipped, unread_dates, rounded_dates: Vec::new(), null_text })
    }
}

/// A file read at offsets, through a buffer that a nearby offset reuses.
struct Cursor {
    f: BufReader<File>,
    pos: u64,
}

impl Cursor {
    fn open(path: &Path) -> Result<Cursor> {
        Ok(Cursor { f: BufReader::with_capacity(BUFFER, crate::provenance::open(path)?), pos: 0 })
    }

    fn read_at(&mut self, at: u64, buf: &mut [u8]) -> std::io::Result<()> {
        if at != self.pos {
            // Ahead and near: within the buffer, or worth reading through.
            match i64::try_from(at.wrapping_sub(self.pos)) {
                Ok(d) if at > self.pos && d < BUFFER as i64 => self.f.seek_relative(d)?,
                _ => {
                    self.f.seek(SeekFrom::Start(at))?;
                }
            }
        }
        let r = self.f.read_exact(buf);
        // After a failure, where the file is is unknown: seek next time.
        self.pos = if r.is_ok() { at + buf.len() as u64 } else { u64::MAX };
        r
    }
}

fn be_i32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le_i32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le_f64(b: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// A record's box from the start of its content (shape type, then a point or a box), rounded
/// outward to `f32`; NaN for a null or unreadable shape, so no extent meets it.
fn record_box(c: &[u8]) -> [f32; 4] {
    const NONE: [f32; 4] = [f32::NAN; 4];
    let b = match le_i32(c, 0) {
        Some(1 | 11 | 21) => match (le_f64(c, 4), le_f64(c, 12)) {
            (Some(x), Some(y)) => [x, y, x, y],
            _ => return NONE,
        },
        Some(t @ (3 | 5 | 8 | 13 | 15 | 18 | 23 | 25 | 28 | 31)) => {
            // A shape of no points, or no parts (an empty geometry some writers give a 0, 0, 0, 0
            // box), has nothing to frame. The counts follow the box: a multipoint's points at 36,
            // else the parts at 36 and the points at 40.
            let empty = if matches!(t, 8 | 18 | 28) {
                le_i32(c, 36).is_some_and(|n| n <= 0)
            } else {
                le_i32(c, 36).is_some_and(|n| n <= 0) || le_i32(c, 40).is_some_and(|n| n <= 0)
            };
            if empty {
                return NONE;
            }
            match (le_f64(c, 4), le_f64(c, 12), le_f64(c, 20), le_f64(c, 28)) {
                (Some(a), Some(b), Some(x), Some(y)) => [a, b, x, y],
                _ => return NONE,
            }
        }
        _ => return NONE,
    };
    if b.iter().any(|v| !v.is_finite()) {
        return NONE;
    }
    [f32_down(b[0]), f32_down(b[1]), f32_up(b[2]), f32_up(b[3])]
}

/// The nearest f32 at or below `v`, as the index rounds a box's low corner.
fn f32_down(v: f64) -> f32 {
    let f = v as f32;
    if f64::from(f) > v { f.next_down() } else { f }
}

/// The nearest f32 at or above `v`, as the index rounds a box's high corner.
fn f32_up(v: f64) -> f32 {
    let f = v as f32;
    if f64::from(f) < v { f.next_up() } else { f }
}

/// Most parts and points one shape may have (a record's own length bounds them first).
const MAX_PARTS: usize = 1 << 24;

/// A record's shape: `Ok(None)` for a null shape, `Err` for one that doesn't parse or isn't
/// supported (a multipatch). Z and M values are dropped (opening notes it, from the header).
fn parse_shape(c: &[u8]) -> std::result::Result<Option<Geometry>, ()> {
    let pt = |at: usize| -> std::result::Result<Coord, ()> { Ok(Coord::new(le_f64(c, at).ok_or(())?, le_f64(c, at + 8).ok_or(())?)) };
    match le_i32(c, 0).ok_or(())? {
        0 => Ok(None),
        1 | 11 | 21 => Ok(Some(Geometry::Point(pt(4)?))),
        8 | 18 | 28 => {
            let n = usize::try_from(le_i32(c, 36).ok_or(())?).map_err(|_| ())?;
            if n > MAX_PARTS || 40 + n * 16 > c.len() {
                return Err(());
            }
            Ok(Some(Geometry::MultiPoint((0..n).map(|i| pt(40 + i * 16)).collect::<std::result::Result<_, _>>()?)))
        }
        t @ (3 | 5 | 13 | 15 | 23 | 25) => {
            let parts = usize::try_from(le_i32(c, 36).ok_or(())?).map_err(|_| ())?;
            let points = usize::try_from(le_i32(c, 40).ok_or(())?).map_err(|_| ())?;
            if parts > MAX_PARTS || points > MAX_PARTS || 44 + parts * 4 + points * 16 > c.len() {
                return Err(());
            }
            let at = 44 + parts * 4;
            let mut lines: Vec<Vec<Coord>> = Vec::with_capacity(parts);
            for p in 0..parts {
                let start = usize::try_from(le_i32(c, 44 + p * 4).ok_or(())?).map_err(|_| ())?;
                let end = if p + 1 < parts { usize::try_from(le_i32(c, 44 + (p + 1) * 4).ok_or(())?).map_err(|_| ())? } else { points };
                if start > end || end > points {
                    return Err(());
                }
                lines.push((start..end).map(|i| pt(at + i * 16)).collect::<std::result::Result<_, _>>()?);
            }
            if matches!(t, 3 | 13 | 23) {
                return Ok(Some(Geometry::Line(lines)));
            }
            // A ring wound clockwise is an outer one (the shapefile crate's rule, for the same
            // shapes as a Shapefile read whole).
            let outer = |r: &[Coord]| r.windows(2).map(|w| (w[1].x - w[0].x) * (w[1].y + w[0].y)).sum::<f64>() >= 0.0;
            Ok(Some(group_rings(
                lines
                    .into_iter()
                    .map(|r| {
                        let o = outer(&r);
                        (r, o)
                    })
                    .collect(),
            )))
        }
        _ => Err(()),
    }
}

/// Reads a `.dbf` header: its row layout and columns.
fn read_table(path: &Path) -> Result<Table> {
    let mut f = BufReader::new(crate::provenance::open(path)?);
    let mut head = [0u8; 32];
    f.read_exact(&mut head).map_err(|_| bad("the .dbf header is cut short"))?;
    let rows = u64::from(u32::from_le_bytes(head.get(4..8).and_then(|b| b.try_into().ok()).unwrap_or([0; 4])));
    let header_len = u64::from(u16::from_le_bytes([head[8], head[9]]));
    let row_len = usize::from(u16::from_le_bytes([head[10], head[11]]));
    if header_len < 33 || row_len == 0 {
        return Err(bad("the .dbf header is damaged"));
    }
    let mut columns = Vec::new();
    let mut offset = 0usize;
    // Field descriptors, 32 bytes each, until the 0x0D terminator.
    for _ in 0..((header_len - 32) / 32).min(2048) {
        let mut d = [0u8; 32];
        if f.read_exact(&mut d[..1]).is_err() || d[0] == 0x0D {
            break;
        }
        f.read_exact(&mut d[1..]).map_err(|_| bad("the .dbf header is cut short"))?;
        let (ty, len, decimals) = (d[11].to_ascii_uppercase(), usize::from(d[16]), d[17]);
        let out = match ty {
            b'N' if decimals == 0 && len <= 18 => FieldType::Integer,
            b'I' => FieldType::Integer,
            b'N' | b'F' | b'O' | b'Y' => FieldType::Double,
            b'L' => FieldType::Boolean,
            b'D' => FieldType::Date,
            _ => FieldType::Text,
        };
        columns.push(Column { ty, offset, len, out });
        offset += len;
    }
    if offset + 1 > row_len {
        return Err(bad("the .dbf's fields are wider than its rows"));
    }
    Ok(Table { header_len, row_len, rows, columns, ldid: head.get(29).copied().unwrap_or(0) })
}

impl Table {
    /// A row's values (after its deletion flag), counting in `unread_dates` the dates that
    /// aren't ones ([`Batch::unread_dates`]) and in `null_text` the text `<Null>` read as null
    /// ([`Batch::null_text`]).
    fn values(
        &self,
        row: &[u8],
        encoding: &dyn shapefile::dbase::Encoding,
        unread_dates: &mut Vec<usize>,
        null_text: &mut Vec<usize>,
    ) -> Vec<Value> {
        let mut out = Vec::with_capacity(self.columns.len());
        for (k, c) in self.columns.iter().enumerate() {
            let b = row.get(1 + c.offset..1 + c.offset + c.len).unwrap_or(&[]);
            out.push(if c.ty == b'D' && std::str::from_utf8(padded(b)).is_ok_and(Value::is_null_marker) {
                // `<Null>` is a missing value's marker, counted as one, not as a date that isn't.
                super::null_text_at(null_text, k);
                Value::Null
            } else if c.ty == b'D' {
                crate::dbf::date(b)
                    .unwrap_or_else(|()| {
                        super::unread_date(unread_dates, k);
                        None
                    })
                    .map_or(Value::Null, Value::Date)
            } else {
                let v = value(c, b, encoding);
                if c.ty == b'C' && matches!(&v, Value::Text(t) if Value::is_null_marker(t)) {
                    super::null_text_at(null_text, k);
                    Value::Null
                } else {
                    v
                }
            });
        }
        out
    }
}

/// The `.dbf`'s column names, in order (read again: the layout above keeps only offsets).
fn read_names(path: &Path, n: usize, encoding: &dyn shapefile::dbase::Encoding) -> Result<Vec<String>> {
    let mut f = BufReader::new(crate::provenance::open(path)?);
    f.seek(SeekFrom::Start(32))?;
    let mut out = Vec::with_capacity(n);
    let mut d = [0u8; 32];
    for _ in 0..n {
        f.read_exact(&mut d)?;
        let name = d.get(..11).unwrap_or(&[]);
        let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
        let name = name.get(..end).unwrap_or(&[]);
        out.push(encoding.decode(name).unwrap_or_else(|_| String::from_utf8_lossy(name)).trim().to_string());
    }
    Ok(out)
}

/// A text field's bytes up to the first NUL, without the padding after them: spaces before the
/// text are the text's, as a Shapefile read whole keeps them.
fn padded(b: &[u8]) -> &[u8] {
    let b = b.get(..b.iter().position(|c| *c == 0).unwrap_or(b.len())).unwrap_or(&[]);
    b.get(..b.iter().rposition(|c| *c != b' ').map_or(0, |e| e + 1)).unwrap_or(&[])
}

/// A field's bytes up to the first NUL, without the spaces around them.
fn trimmed(b: &[u8]) -> &[u8] {
    let b = b.get(..b.iter().position(|c| *c == 0).unwrap_or(b.len())).unwrap_or(&[]);
    let start = b.iter().position(|c| *c != b' ').unwrap_or(b.len());
    let end = b.iter().rposition(|c| *c != b' ').map_or(start, |e| e + 1);
    b.get(start..end).unwrap_or(&[])
}

/// One `.dbf` value, as a Shapefile read whole gives it (blank and unreadable values are null;
/// [`Table::values`] counts the dates).
fn value(c: &Column, b: &[u8], encoding: &dyn shapefile::dbase::Encoding) -> Value {
    let text = || encoding.decode(trimmed(b)).unwrap_or_else(|_| String::from_utf8_lossy(trimmed(b))).into_owned();
    let number = || {
        let t = trimmed(b);
        if t.is_empty() || t.iter().all(|c| *c == b'*') {
            return None;
        }
        std::str::from_utf8(t).ok()?.parse::<f64>().ok().filter(|v| v.is_finite())
    };
    match c.ty {
        b'C' => {
            let t = padded(b);
            if t.is_empty() {
                Value::Null
            } else {
                Value::Text(encoding.decode(t).unwrap_or_else(|_| String::from_utf8_lossy(t)).into_owned())
            }
        }
        // A whole-number column's values as integers, exactly (its at most 18 digits fit an i64);
        // one written with a point or exponent (`12.0`, `1e3`) too, and one that isn't whole is
        // kept as a number rather than lost.
        b'N' | b'F' if c.out == FieldType::Integer => match std::str::from_utf8(trimmed(b)).ok().and_then(|t| t.parse::<i64>().ok()) {
            Some(i) => Value::Int(i),
            None => {
                number().map_or(Value::Null, |v| if v.fract() == 0.0 && v.abs() < 9.0e15 { Value::Int(v as i64) } else { Value::Float(v) })
            }
        },
        b'N' | b'F' => number().map_or(Value::Null, Value::Float),
        b'I' => b.get(..4).and_then(|v| v.try_into().ok()).map_or(Value::Null, |v| Value::Int(i64::from(i32::from_le_bytes(v)))),
        // Currency as the `dbase` crate reads it (a Shapefile read whole): a little-endian f64.
        b'O' | b'Y' => {
            b.get(..8).and_then(|v| v.try_into().ok()).map(f64::from_le_bytes).filter(|v| v.is_finite()).map_or(Value::Null, Value::Float)
        }
        b'L' => match trimmed(b).first() {
            Some(b'1' | b'T' | b't' | b'Y' | b'y') => Value::Bool(true),
            Some(b'0' | b'F' | b'f' | b'N' | b'n') => Value::Bool(false),
            _ => Value::Null,
        },
        b'D' => crate::dbf::date(b).ok().flatten().map_or(Value::Null, Value::Date),
        // Memos (in a `.dbt` beside the file), date-times and binary fields aren't read.
        b'M' | b'T' | b'@' | b'B' | b'G' | b'P' => Value::Null,
        _ => {
            let t = text();
            if t.is_empty() { Value::Null } else { Value::Text(t) }
        }
    }
}

/// The coordinate system in a `.prj`, with what couldn't be understood said in the notes. Another
/// body's system that can't be read is an error, as when the file is read whole.
fn shp_reproject(prj: Option<&str>, notes: &mut Vec<String>) -> Result<Reproject> {
    use crate::raster::planetary;
    let crs = match prj.map(|p| (p, crate::raster::wkt::read_wkt(p))) {
        None => crate::raster::Crs::LonLat,
        Some((_, Ok((c, note)))) => {
            if let Some(n) = note {
                notes.push(format!("the .prj file: {n}"));
            }
            c
        }
        Some((p, Err(e))) => {
            if let Some(refused) = planetary::refused_wkt(p, &e) {
                return Err(DataError::Format(format!("the .prj file: {refused}")));
            }
            notes.push(format!("the .prj file: {e}; coordinates are read as longitude/latitude"));
            crate::raster::Crs::LonLat
        }
    };
    Ok(Reproject::from_crs(crs, notes))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) fn with_test_cache<T>(root: PathBuf, run: impl FnOnce() -> T) -> T {
    cache::with_index_dir(root.clone(), || {
        let result = run();
        if root.exists() {
            assert_eq!(cache::maintain(&root, 0).unwrap().remaining_bytes, 0);
        }
        result
    })
}
