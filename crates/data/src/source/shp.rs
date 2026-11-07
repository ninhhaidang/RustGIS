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

use super::{Batch, FeatureSource, Reproject, Request, SourceInfo, clamp_limit};
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
        let dbf = path.with_extension("dbf");
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
        let mut f = BufReader::with_capacity(BUFFER, File::open(path)?);
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
        let extent = if native.is_empty() || !native.min_x.is_finite() { Extent::EMPTY } else { reproject.extent_to_lonlat(&native) };
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
            crs: reproject.crs.custom_def().map(|d| d.proj4.clone()).unwrap_or_else(|| reproject.label()),
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

    fn read(&self, req: &Request) -> Result<Batch> {
        let encoding = self.encoding.decoder();
        let limit = clamp_limit(req.limit);
        let extent = req.extent.map(|e| self.reproject.extent_from_lonlat(&e));
        if extent.is_some_and(|e| e.is_empty()) {
            return Ok(Batch { complete: super::stride(0, limit) == 1, ..Batch::default() });
        }
        let bounds = extent.map(|e| [e.min_x as f32, e.min_y as f32, e.max_x as f32, e.max_y as f32]);
        let mut shp = Cursor::open(&self.shp)?;
        let mut dbf = Cursor::open(&self.dbf)?;
        let mut buf = Vec::new();
        let mut row = vec![0u8; self.table.row_len];
        let mut features = Vec::new();
        let mut skipped = 0;
        let (matched, complete) = self.index.visit(bounds, limit, |i, place| {
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
                self.table.columns.iter().map(|c| value(c, row.get(1 + c.offset..1 + c.offset + c.len).unwrap_or(&[]), &encoding)).collect()
            } else {
                vec![Value::Null; self.table.columns.len()]
            };
            features.push(Feature { id: u64::from(i) + 1, geometry, attrs });
            Ok(())
        })?;
        Ok(Batch { features, matched, complete, skipped })
    }
    fn streams_chunks(&self) -> bool {
        true
    }
    fn visit_chunks(&self, extent: Option<Extent>, bytes: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        let encoding = self.encoding.decoder();
        let mut chunks = super::chunk_memory::Chunker::new(bytes, visit)?;
        let extent = extent.map(|e| self.reproject.extent_from_lonlat(&e));
        if extent.is_some_and(|e| e.is_empty()) {
            return Ok(Batch { complete: true, ..Batch::default() });
        }
        let bounds = extent.map(|e| [e.min_x as f32, e.min_y as f32, e.max_x as f32, e.max_y as f32]);
        let mut shp = Cursor::open(&self.shp)?;
        let mut dbf = Cursor::open(&self.dbf)?;
        let mut buf = Vec::new();
        let mut row = vec![0u8; self.table.row_len];
        let mut skipped = 0;
        // The same directory, ordering, extent rules and read-only scan fallback as sampled
        // reads, with no interaction row cap. The index visits records without materializing
        // their directory or the complete feature collection in memory.
        // Every record id fits u32. This format bound visits them all while keeping SQLite's
        // LIMIT within its signed integer range on 64-bit hosts.
        let (matched, complete) = self.index.visit_complete(bounds, |i, place| {
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
                self.table.columns.iter().map(|c| value(c, row.get(1 + c.offset..1 + c.offset + c.len).unwrap_or(&[]), &encoding)).collect()
            } else {
                vec![Value::Null; self.table.columns.len()]
            };
            chunks.push(Feature { id: u64::from(i) + 1, geometry, attrs })
        })?;
        chunks.flush()?;
        Ok(Batch { features: Vec::new(), matched, complete, skipped })
    }
}

/// A file read at offsets, through a buffer that a nearby offset reuses.
struct Cursor {
    f: BufReader<File>,
    pos: u64,
}

impl Cursor {
    fn open(path: &Path) -> Result<Cursor> {
        Ok(Cursor { f: BufReader::with_capacity(BUFFER, File::open(path)?), pos: 0 })
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
        Some(3 | 5 | 8 | 13 | 15 | 18 | 23 | 25 | 28 | 31) => match (le_f64(c, 4), le_f64(c, 12), le_f64(c, 20), le_f64(c, 28)) {
            (Some(a), Some(b), Some(x), Some(y)) => [a, b, x, y],
            _ => return NONE,
        },
        _ => return NONE,
    };
    if b.iter().any(|v| !v.is_finite()) {
        return NONE;
    }
    let down = |v: f64| {
        let f = v as f32;
        if f64::from(f) > v { f.next_down() } else { f }
    };
    let up = |v: f64| {
        let f = v as f32;
        if f64::from(f) < v { f.next_up() } else { f }
    };
    [down(b[0]), down(b[1]), up(b[2]), up(b[3])]
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
    let mut f = BufReader::new(File::open(path)?);
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

/// The `.dbf`'s column names, in order (read again: the layout above keeps only offsets).
fn read_names(path: &Path, n: usize, encoding: &dyn shapefile::dbase::Encoding) -> Result<Vec<String>> {
    let mut f = BufReader::new(File::open(path)?);
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

/// A field's bytes up to the first NUL, without the spaces around them.
fn trimmed(b: &[u8]) -> &[u8] {
    let b = b.get(..b.iter().position(|c| *c == 0).unwrap_or(b.len())).unwrap_or(&[]);
    let start = b.iter().position(|c| *c != b' ').unwrap_or(b.len());
    let end = b.iter().rposition(|c| *c != b' ').map_or(start, |e| e + 1);
    b.get(start..end).unwrap_or(&[])
}

/// One `.dbf` value, as a Shapefile read whole gives it (blank and unreadable values are null).
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
            let t = text();
            if t.is_empty() { Value::Null } else { Value::Text(t) }
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
        b'D' => {
            let t = trimmed(b);
            match t.get(..8) {
                Some(d) if d.iter().all(u8::is_ascii_digit) => {
                    let part = |r: std::ops::Range<usize>| String::from_utf8_lossy(d.get(r).unwrap_or(&[])).into_owned();
                    Value::Text(format!("{}-{}-{}", part(0..4), part(4..6), part(6..8)))
                }
                _ => Value::Null,
            }
        }
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
