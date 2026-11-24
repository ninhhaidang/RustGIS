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
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering::Relaxed;

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
    /// Whether every record's stored box holds its shape, as reads by area take the boxes on
    /// trust: [`BOXES_UNKNOWN`] until a whole read has looked, then [`BOXES_HOLD`] or
    /// [`BOXES_DONT`].
    boxes: AtomicU8,
    /// The `.shp`'s and the `.dbf`'s versions as opened
    /// ([`FeatureSource::same_as_opened`]).
    stamps: [Option<Stamp>; 2],
}

/// What a file's metadata says of its version ([`FeatureSource::same_as_opened`]): its size and
/// modification time and, on Unix, its identity and change time, which a write of the same size
/// within the clock's step still moves. `None` without a modification time: then no copy of what
/// was read is trusted.
#[derive(PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    identity_and_change: (u64, u64, i64, i64),
}

impl Stamp {
    fn of(path: &Path) -> Option<Stamp> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).ok()?;
        Some(Stamp {
            len: m.len(),
            modified: m.modified().ok()?,
            #[cfg(unix)]
            identity_and_change: (m.dev(), m.ino(), m.ctime(), m.ctime_nsec()),
        })
    }
}

/// Not yet looked at ([`ShpSource::boxes`]).
const BOXES_UNKNOWN: u8 = 0;
/// Every stored box holds its shape.
const BOXES_HOLD: u8 = 1;
/// Some stored box misses part of its shape: reads by area leave that shape out.
const BOXES_DONT: u8 = 2;

/// Whether `b`, a stored box as [`record_box`] gives it (rounded outward to f32, as the index
/// keeps it), holds `e`: a box that held the shape still does once rounded outward.
fn holds(b: [f32; 4], e: &Extent) -> bool {
    let [x0, y0, x1, y1] = b.map(f64::from);
    x0 <= e.min_x && y0 <= e.min_y && x1 >= e.max_x && y1 >= e.max_y
}

/// Bytes read ahead from the files: shapes and rows near each other in a read share a fill.
const BUFFER: usize = 1 << 20;

/// The largest record read: one past it is skipped rather than given a buffer that size, since a
/// record's length is only checked against the file's, which can be gigabytes (#1055).
const MAX_RECORD: u64 = crate::io::BYTES_FLOOR;

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
        // Taken before anything is read, so a file written while it opens is seen as changed.
        let stamps = [Stamp::of(path), Stamp::of(&dbf)];
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
        // with none (no records, or every shape null, empty or not a number) it has no extent,
        // never the 0, 0, 0, 0 a header often holds then (Null Island). The header's box, exact,
        // stands for them when it is their box (the specification's minimum bounding rectangle);
        // one that isn't is set aside: said when it misses a shape or isn't a box (a corner that
        // isn't a number, #1985, or stale after records were added), not when it only holds more
        // (editors that only grow it, or junk) or is 0, 0, 0, 0 (no box kept).
        let header = native;
        // A box at all: its corners numbers, in order.
        let a_box = [header.min_x, header.min_y, header.max_x, header.max_y].iter().all(|v| v.is_finite()) && !header.is_empty();
        // 0, 0, 0, 0: what writers put when they keep no box, not a box that's wrong; set aside
        // without a word.
        let zero = [header.min_x, header.min_y, header.max_x, header.max_y].iter().all(|v| *v == 0.0);
        let native = index.bounds.map(|[x0, y0, x1, y1]| {
            // Rounded outward as the records' boxes are, the header is theirs on every side: so a
            // header written exactly always passes, whatever its size, as does one off by less
            // than one f32 step (a meter or two at UTM and Web Mercator northings).
            let [h0, h1, h2, h3] = [f32_down(header.min_x), f32_down(header.min_y), f32_up(header.max_x), f32_up(header.max_y)];
            if a_box && h0 == x0 && h1 == y0 && h2 == x1 && h3 == y1 {
                return header;
            }
            // Larger than the shapes but holding them (left so by editors that only grow it, or
            // junk): the shapes' box, with nothing to say. One that misses a shape, or isn't a box,
            // is said.
            if !zero && !(a_box && h0 <= x0 && h1 <= y0 && h2 >= x1 && h3 >= y1) {
                notes.push(
                    "the .shp header's bounding box doesn't hold its shapes, or isn't a box: the layer's extent is taken from the shapes"
                        .into(),
                );
            }
            Extent::new(f64::from(x0), f64::from(y0), f64::from(x1), f64::from(y1))
        });
        // Records, every shape null or empty, under a header that says otherwise (a box, or one
        // that isn't, such as a corner that isn't a number; not the 0, 0, 0, 0 such a file usually
        // holds): said, as the layer has no extent. A file of no records loses nothing by it;
        // shapes there but without a usable box are said below instead.
        if native.is_none() && index.count > 0 && index.unboxed == 0 && !zero {
            notes.push("the .shp has no shapes to frame: its header's box isn't used, and the layer has no extent".into());
        }
        // Records with no usable box, neither stored nor from their own points (a coordinate that
        // isn't a number or past f32's range, or a record that can't be read; a backwards stored
        // box over good points is framed by the points instead) are left out of the extent and of
        // every read by area (the R-tree and the scan alike): said, with how many, and when
        // nothing else frames the layer, so.
        if index.unboxed > 0 {
            let none = if native.is_none() { ", so the layer has no extent" } else { "" };
            notes.push(format!(
                "{} shape(s) have no usable bounding box (a coordinate that isn't a number or is out of range, a record that can't be read, or a shape RustGIS doesn't read, such as a multipatch): they're left out of the layer's extent and of reads by area{none}",
                index.unboxed
            ));
        }
        let extent = native.map_or(Extent::EMPTY, |n| reproject.extent_to_lonlat(&n));
        if reproject.is_lonlat() && extent.beyond_lonlat() {
            notes.push("coordinates are not longitude/latitude; RustGIS reads them as WGS 84 and they may draw out of place".into());
        }
        let (names, not_utf8) = read_names(&dbf, table.columns.len(), &encoding)?;
        let mut notes = crate::loss::Notes::from(notes);
        notes.extend(crate::dbf::not_utf8_names_note(&not_utf8, encoding.name()));
        let fields = table.columns.iter().zip(names).map(|(c, n)| Field::new(n, c.out)).collect();
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
        // Points too: each record's own type is read, whatever the header says.
        let boxes = AtomicU8::new(BOXES_UNKNOWN);
        Ok(ShpSource { info, shp: path.to_path_buf(), dbf, index, table, encoding, reproject, boxes, stamps })
    }
}

impl ShpSource {
    /// A complete visit of a request's records, or of those numbered `numbered` (from 0, in
    /// order), parsed and given a chunk at a time ([`FeatureSource::visit_request_chunks`],
    /// [`FeatureSource::visit_ids_chunks`]). Only a whole read looks at the stored boxes.
    fn visit_with(
        &self,
        req: &Request,
        numbered: Option<&[u32]>,
        bytes: usize,
        visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>,
    ) -> Result<Batch> {
        let encoding = self.encoding.decoder();
        let mut chunks = super::chunk_memory::Chunker::new(bytes, visit)?;
        let req = Request { limit: usize::MAX, ..*req };
        // The request's box in the file's own system, when it is one (a box in another system
        // is read as if absent), and whether it is one over the whole plane, which narrows
        // nothing: every record, those without a box too, as a whole read.
        let own = self.reproject.own_box(&req);
        let everywhere = own == Some(super::whole_plane());
        let extent = self.reproject.request_box(&req).filter(|_| !everywhere);
        if extent.is_some_and(|e| e.is_empty()) {
            return Ok(Batch { complete: true, ..Batch::default() });
        }
        // Rounded outward, as the index keeps the records' boxes, so a box reaching the extent by
        // less than an f32 step is still found.
        let bounds = extent.map(|e| [f32_down(e.min_x), f32_down(e.min_y), f32_up(e.max_x), f32_up(e.max_y)]);
        // As in `read`, an extent the file's system couldn't bound (#1925) visits every record.
        let late = req.extent.is_some() && bounds.is_none();
        // A whole read looks at whether the stored boxes hold their shapes, once.
        let checking =
            numbered.is_none() && req.extent.is_none() && (own.is_none() || everywhere) && self.boxes.load(Relaxed) == BOXES_UNKNOWN;
        let mut missed = false;
        // A record a whole read couldn't read this time had its box left unchecked: the check
        // waits for a read that reads them all.
        let mut unchecked = false;
        let mut shp = Cursor::open(&self.shp)?;
        let mut dbf = Cursor::open(&self.dbf)?;
        let mut buf = Vec::new();
        let mut row = vec![0u8; self.table.row_len];
        let (mut skipped, mut unread_dates, mut null_text, mut boxed_off, mut not_utf8) = (0, Vec::new(), Vec::new(), 0u64, Vec::new());
        let mut deleted = 0usize;
        // The same directory, ordering, extent rules and read-only scan fallback as sampled
        // reads, with no interaction row cap. The index visits records without materializing
        // their directory or the complete feature collection in memory.
        // Every record id fits u32. This format bound visits them all while keeping SQLite's
        // LIMIT within its signed integer range on 64-bit hosts.
        crate::progress::begin(self.info.count.and_then(|n| usize::try_from(n).ok()).unwrap_or(0));
        let mut each = |i: u32, place: Place| -> Result<()> {
            crate::progress::step(1)?;
            if u64::from(place.len) > MAX_RECORD {
                skipped += 1;
                return Ok(());
            }
            buf.resize(place.len as usize, 0);
            if shp.read_at(u64::from(place.offset), &mut buf).is_err() {
                skipped += 1;
                unchecked = true;
                return Ok(());
            }
            let geometry = match parse_shape(&buf) {
                // A coordinate that isn't a number (NaN or infinite) has no place: the record is
                // left out and counted, as one that can't be converted (#1985). One under a stored
                // box the index takes ([`index::usable`]) is counted apart too, as the index framed
                // it by that box; a backwards one it can't frame, and counts as unboxed instead.
                Ok(Some(g)) if !g.is_finite() => {
                    skipped += 1;
                    boxed_off += u64::from(index::usable(record_box(&buf, u64::from(place.len))));
                    return Ok(());
                }
                Ok(Some(g)) => {
                    // A box the index took as stored (one in order) that misses part of the shape.
                    if checking && !missed {
                        let stored = record_box(&buf, u64::from(place.len));
                        missed = index::usable(stored) && !holds(stored, &g.extent());
                    }
                    match self.reproject.place(g, &req) {
                        Some(placed) => Some(placed),
                        None => {
                            skipped += 1;
                            return Ok(());
                        }
                    }
                }
                Ok(None) => None,
                Err(()) => {
                    skipped += 1;
                    return Ok(());
                }
            };
            // As in `read`, the shape itself is checked.
            // As a read by area, its box in the file's own system included, gives no rows
            // without a shape.
            if geometry.as_ref().map_or(if own.is_some() { !everywhere } else { req.extent.is_some() }, |g| !g.inside) {
                return Ok(());
            }
            let geometry = geometry.map(|p| p.geometry);
            let attrs = if u64::from(i) < self.table.rows {
                let at = self.table.header_len + u64::from(i) * self.table.row_len as u64;
                if dbf.read_at(at, &mut row).is_err() {
                    skipped += 1;
                    return Ok(());
                }
                // A deleted row: its shape is left out too, and counted.
                if row.first() == Some(&b'*') {
                    deleted += 1;
                    return Ok(());
                }
                self.table.values(&row, &encoding, &self.encoding, &mut unread_dates, &mut null_text, &mut not_utf8)
            } else {
                vec![Value::Null; self.table.columns.len()]
            };
            chunks.push(Feature { id: u64::from(i) + 1, geometry, attrs })
        };
        let (matched, complete) = match numbered {
            Some(numbers) => self.index.visit_numbered(numbers, &mut each)?,
            None => self.index.visit_complete(bounds, &mut each)?,
        };
        chunks.flush()?;
        if checking && complete && (missed || !unchecked) {
            self.boxes.store(if missed { BOXES_DONT } else { BOXES_HOLD }, Relaxed);
        }
        Ok(Batch {
            features: Vec::new(),
            matched,
            complete,
            skipped,
            unread_dates,
            rounded_dates: Vec::new(),
            null_text,
            notes: boxed_off_note(boxed_off, false, late).into_iter().chain(deleted_note(deleted, false)).collect(),
            not_utf8,
        })
    }
}

impl FeatureSource for ShpSource {
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

    /// The code page its text is read in.
    fn text_encoding(&self) -> &'static str {
        self.encoding.name()
    }

    /// Samples indexed records, falling back to geometry matches when the native view is refused.
    /// The fallback counts records and checks Cancel before reading each one.
    fn read(&self, req: &Request) -> Result<Batch> {
        let encoding = self.encoding.decoder();
        let limit = clamp_limit(req.limit);
        let extent = self.reproject.request_box(req);
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
        let (mut skipped, mut unread_dates, mut null_text, mut boxed_off, mut not_utf8) = (0, Vec::new(), Vec::new(), 0u64, Vec::new());
        let mut deleted = 0usize;
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
                // left out and counted, as one that can't be converted (#1985). One under a stored
                // box the index takes ([`index::usable`]) is counted apart too, as the index framed
                // it by that box; a backwards one it can't frame, and counts as unboxed instead.
                Ok(Some(g)) if !g.is_finite() => {
                    skipped += 1;
                    boxed_off += u64::from(index::usable(record_box(&buf, u64::from(place.len))));
                    return Ok(());
                }
                Ok(Some(g)) => match self.reproject.place(g, req) {
                    Some(placed) => Some(placed),
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
            if geometry.as_ref().map_or(req.extent.is_some(), |g| !g.inside) {
                return Ok(());
            }
            let geometry = geometry.map(|p| p.geometry);
            let attrs = if u64::from(i) < self.table.rows {
                let at = self.table.header_len + u64::from(i) * self.table.row_len as u64;
                if dbf.read_at(at, &mut row).is_err() {
                    skipped += 1;
                    return Ok(());
                }
                // A deleted row: its shape is left out too, and counted.
                if row.first() == Some(&b'*') {
                    deleted += 1;
                    return Ok(());
                }
                if late && !sample.wants() {
                    return Ok(());
                }
                self.table.values(&row, &encoding, &self.encoding, &mut unread_dates, &mut null_text, &mut not_utf8)
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
        Ok(Batch {
            features,
            matched,
            complete,
            skipped,
            unread_dates,
            rounded_dates: Vec::new(),
            null_text,
            // A sampled read visits only some of the matches, so its count is of those; a late one
            // visits the whole layer, so its count is of the layer.
            notes: boxed_off_note(boxed_off, !late && !complete, late)
                .into_iter()
                .chain(deleted_note(deleted, !late && !complete))
                .collect(),
            not_utf8,
        })
    }
    fn streams_chunks(&self) -> bool {
        true
    }
    /// Once a whole read has found every stored box holding its shape, and the index placed every
    /// record (those without a usable box are left out of reads by area, said when the file is
    /// opened). Until then, and for a file whose boxes miss their shapes, reads by area may leave
    /// shapes out.
    fn finds_all_by_area(&self) -> bool {
        self.index.unboxed == 0 && self.boxes.load(Relaxed) == BOXES_HOLD
    }
    /// Both files opened as a read opens them, so one gone gives a read's error, then their
    /// versions ([`Stamp`]) compared with those at opening.
    fn same_as_opened(&self) -> Result<bool> {
        drop(crate::provenance::open(&self.shp)?);
        drop(crate::provenance::open(&self.dbf)?);
        let now = [Stamp::of(&self.shp), Stamp::of(&self.dbf)];
        Ok(self.stamps.iter().all(Option::is_some) && now == self.stamps)
    }
    /// Visits located shapes in a spatial request; geometry-less rows belong only to whole reads.
    /// Counts scanned records and checks Cancel before filtering them.
    fn visit_chunks(&self, extent: Option<Extent>, bytes: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        self.visit_request_chunks(&Request { extent, limit: usize::MAX, own: None }, bytes, visit)
    }
    /// As [`FeatureSource::visit_chunks`], narrowed by the request's box in the file's own system
    /// when it gives one ([`Request::own`]), and each shape's own box tested against it.
    fn visit_request_chunks(&self, req: &Request, bytes: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        self.visit_with(req, None, bytes, visit)
    }
    fn visits_ids(&self) -> bool {
        true
    }
    /// The records whose features have these ids (record numbers from 1), in file order.
    fn visit_ids_chunks(&self, ids: &[u64], bytes: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        let numbers: Vec<u32> = ids.iter().filter_map(|id| id.checked_sub(1).and_then(|n| u32::try_from(n).ok())).collect();
        self.visit_with(&Request { extent: None, limit: usize::MAX, own: None }, Some(&numbers), bytes, visit)
    }
}

/// Says how many shapes a read left out for a coordinate that isn't a number while their stored
/// box is one the index takes (finite and in order, [`index::usable`]): [`record_box`] doesn't read
/// the points, so the layer's extent and its index still take in those boxes. A backwards stored
/// box over such a point isn't counted here: the index can't frame it from the points, so it's
/// left out of the extent and counted with the shapes that have no usable box. A `sampled` read visited only some of the matches, so its count is of
/// the shapes it read, not of the layer. A `late` read (an area the layer's system couldn't bound,
/// #1925) visited every record and can't place a NaN shape's stored box in the area, so its count
/// is of the whole layer, and says so.
fn boxed_off_note(n: u64, sampled: bool, late: bool) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let what = if sampled {
        format!("{n} of the shapes read (a sample of the layer, which may hold more)")
    } else if late {
        format!("{n} shape(s) in the layer (a count of the whole layer, as the area couldn't be placed in its coordinates)")
    } else {
        format!("{n} shape(s)")
    };
    vec![format!(
        "{what} have a coordinate that isn't a number under a bounding box that is one: they're left out of the read, but the layer's extent and its index still take in their stored boxes"
    )]
}

/// The note for `n` records marked deleted in the `.dbf`, left out with their shapes; of those
/// read when the read is `sampled`.
fn deleted_note(n: usize, sampled: bool) -> Option<String> {
    if sampled && n > 0 {
        return Some(format!(
            "{n} of the records read (a sample of the layer, which may hold more) are marked deleted in the .dbf: they're left out, with their shapes"
        ));
    }
    crate::io::deleted_records_text(n, true)
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
/// outward to `f32`. NaN for a null or empty shape, which has nothing to frame, so no extent meets
/// it; infinite for a record whose box isn't one (a coordinate that isn't a number or past `f32`'s
/// range, an unknown shape type, or a record too short for what it says), so it is counted where
/// it's left out ([`index::Index::unboxed`]). A backwards box is given as stored, finite but out of
/// order, for the index to put right from its points where they read ([`index::scan_boxed`]).
/// `len` is the record's content length as its header gives it: one too short for the points its
/// counts say, or a line or polygon whose parts' starts don't fit its points ([`parse_shape`]'s
/// rules, so a record it won't read frames nothing), isn't a box. `c` holds a line's or polygon's
/// whole array of parts' starts where the record has room for it.
///
/// The points themselves aren't read: a line, polygon or multipoint with a stored box in order over
/// a point that isn't a number keeps that box here, though a read leaves the shape out, counts it
/// and says so ([`boxed_off_note`]). A backwards one stays backwards, as the index can't frame it
/// from points that don't read, so it's counted as having no usable box instead. Telling it apart here would mean reading every record's points to
/// build the index, for files (#1985's) with such shapes by the thousand.
fn record_box(c: &[u8], len: u64) -> [f32; 4] {
    const NONE: [f32; 4] = [f32::NAN; 4];
    const NOT_A_BOX: [f32; 4] = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
    let b = match le_i32(c, 0) {
        Some(0) => return NONE,
        Some(1 | 11 | 21) => match (le_f64(c, 4), le_f64(c, 12)) {
            (Some(x), Some(y)) if len >= 20 => [x, y, x, y],
            _ => return NOT_A_BOX,
        },
        // A multipatch, which the reader doesn't read, frames nothing it draws: counted.
        Some(t @ (3 | 5 | 8 | 13 | 15 | 18 | 23 | 25 | 28)) => {
            // The counts follow the box: a multipoint's points at 36, else the parts at 36 and
            // the points at 40. A record too short to hold them, or the parts' starts and points
            // they count (as a read needs), or a count below 0, can't be read; one that reads but
            // has no points, or no parts (an empty geometry some writers give a 0, 0, 0, 0 box),
            // has nothing to frame.
            let (parts, points) = if matches!(t, 8 | 18 | 28) { (Some(1), le_i32(c, 36)) } else { (le_i32(c, 36), le_i32(c, 40)) };
            let (Some(parts), Some(points)) = (parts, points) else { return NOT_A_BOX };
            if parts < 0 || points < 0 {
                return NOT_A_BOX;
            }
            let (parts, points) = (parts.unsigned_abs(), points.unsigned_abs());
            let need =
                if matches!(t, 8 | 18 | 28) { 40 + 16 * u64::from(points) } else { 44 + 4 * u64::from(parts) + 16 * u64::from(points) };
            if parts as usize > MAX_PARTS || points as usize > MAX_PARTS || len < need {
                return NOT_A_BOX;
            }
            // Each part runs from its start to the next one's (the last to the end of the points):
            // a start below 0, past the next, or past the points can't be read.
            if !matches!(t, 8 | 18 | 28) {
                let start = |p: u32| le_i32(c, 44 + 4 * p as usize).and_then(|s| u32::try_from(s).ok());
                for p in 0..parts {
                    let end = if p + 1 < parts { start(p + 1) } else { Some(points) };
                    match (start(p), end) {
                        (Some(s), Some(e)) if s <= e && e <= points => {}
                        _ => return NOT_A_BOX,
                    }
                }
                // Starts in order, the last part running to the end of the points: every part is
                // empty when the first starts there, and the shape reads as empty, framing nothing
                // whatever its stored box.
                if parts > 0 && start(0) == Some(points) {
                    return NONE;
                }
            }
            if parts == 0 || points == 0 {
                return NONE;
            }
            match (le_f64(c, 4), le_f64(c, 12), le_f64(c, 20), le_f64(c, 28)) {
                (Some(x0), Some(y0), Some(x1), Some(y1)) => [x0, y0, x1, y1],
                _ => return NOT_A_BOX,
            }
        }
        _ => return NOT_A_BOX,
    };
    if b.iter().any(|v| !v.is_finite()) {
        return NOT_A_BOX;
    }
    // Past f32's range, a corner rounds to an infinity: not a box either, and counted so.
    // A backwards side is rounded the other way (its min up, its max down), so it stays backwards
    // even when the two are less than an f32 step apart, for the index to frame from the points;
    // rounded outward, it could come out in order and be taken as a box.
    let side = |lo: f64, hi: f64| if lo > hi { (f32_up(lo), f32_down(hi)) } else { (f32_down(lo), f32_up(hi)) };
    let ((x0, x1), (y0, y1)) = (side(b[0], b[2]), side(b[1], b[3]));
    [x0, y0, x1, y1]
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
    /// aren't ones ([`Batch::unread_dates`]), in `null_text` the text `<Null>` read as null
    /// ([`Batch::null_text`]) and in `not_utf8` the text that isn't valid in `declared`, the
    /// encoding `decoder` reads ([`Batch::not_utf8`]).
    fn values(
        &self,
        row: &[u8],
        decoder: &dyn shapefile::dbase::Encoding,
        declared: &crate::dbf::DbfEncoding,
        unread_dates: &mut Vec<usize>,
        null_text: &mut Vec<usize>,
        not_utf8: &mut Vec<usize>,
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
                let v = value(c, b, decoder);
                if c.ty == b'C' && matches!(&v, Value::Text(t) if Value::is_null_marker(t)) {
                    super::null_text_at(null_text, k);
                    Value::Null
                } else {
                    // Counted on the bytes: U+FFFD in the text read can also be what the file holds.
                    if matches!(v, Value::Text(_)) && declared.malformed(padded(b)) {
                        super::not_utf8_at(not_utf8, k);
                    }
                    v
                }
            });
        }
        out
    }
}

/// The `.dbf`'s column names, in order (read again: the layout above keeps only offsets), and the
/// columns (from 1) whose names aren't valid in its `encoding` ([`crate::dbf::not_utf8_names_note`]).
fn read_names(path: &Path, n: usize, encoding: &crate::dbf::DbfEncoding) -> Result<(Vec<String>, Vec<usize>)> {
    let decoder = encoding.decoder();
    let mut f = BufReader::new(crate::provenance::open(path)?);
    f.seek(SeekFrom::Start(32))?;
    let mut out = Vec::with_capacity(n);
    let mut not_utf8 = Vec::new();
    let mut d = [0u8; 32];
    for _ in 0..n {
        f.read_exact(&mut d)?;
        let name = d.get(..11).unwrap_or(&[]);
        let end = name.iter().position(|b| *b == 0).unwrap_or(name.len());
        let name = name.get(..end).unwrap_or(&[]);
        if encoding.malformed(name) {
            not_utf8.push(out.len() + 1);
        }
        out.push(shapefile::dbase::Encoding::decode(&decoder, name).unwrap_or_else(|_| String::from_utf8_lossy(name)).trim().to_string());
    }
    Ok((out, not_utf8))
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
        // Dates are read, and those that aren't ones counted, by `Table::values`, which never
        // passes a date column here.
        b'D' => Value::Null,
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
