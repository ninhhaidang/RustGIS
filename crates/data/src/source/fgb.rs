//! FlatGeobuf: features in a packed Hilbert R-tree, read with the pure-Rust `flatgeobuf` crate.
//!
//! Extent queries walk the file's spatial index and seek to just the matching features. Files
//! written without an index are scanned.
//!
//! A file by its web address is read the same way, by range: the index a level at a time (each
//! level's nodes in one round of requests), then the features a view needs. Features left out of a
//! sample are passed over without being fetched.

use super::remote::{self, Remote};
use super::{Batch, FeatureSource, Reproject, Request, Sampler, SourceInfo, clamp_limit, in_request, stride};
use crate::raster::Crs;
use crate::{DataError, Feature, Field, FieldType, GeometryType, Result, Value, wkb};
use flatgeobuf::geozero::{ColumnValue, CoordDimensions, PropertyProcessor, ToWkb};
use flatgeobuf::{ColumnType, FeatureProperties};
use rustgis_geom::Extent;
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod checked;

/// The header's feature count, without scanning date columns or decoding features. GDAL
/// conversions use it to report features lost before RustGIS reads the converted file; they are
/// indexed, and an indexed FlatGeobuf always gives its count, so 0 is no features.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn header_feature_count(path: &Path) -> Result<u64> {
    let reader = open_reader(BufReader::new(std::fs::File::open(path)?))?;
    Ok(reader.header().features_count())
}

/// A file or an upload, read and seeked.
trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

#[derive(Clone, Debug)]
enum Input {
    Path(PathBuf),
    Bytes(super::Shared),
    /// A file by its web address.
    Remote(Arc<Remote>),
}

/// A FlatGeobuf file.
pub struct FgbSource {
    info: SourceInfo,
    input: Input,
    reproject: Reproject,
    /// The field each of the header's columns is read into, `None` for one left out (binary).
    slots: Vec<Option<usize>>,
    /// The type of each of the header's columns, to check a feature's properties against
    /// ([`properties_fit`]).
    kinds: Vec<ColumnType>,
    /// Kept after the input so all readers close before the private workspace is removed.
    #[cfg(not(target_arch = "wasm32"))]
    _workspace: Option<Arc<super::gdal::ReloadWorkspace>>,
}

/// A header read whose native date schema may still need a complete scan. It cannot read
/// features until finished, so a frame can schedule that work without publishing a text schema.
pub struct FgbOpen {
    source: FgbSource,
    date_columns: Vec<usize>,
}

impl FgbOpen {
    /// Whether completing this local open visits every feature.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn needs_scan(&self) -> bool {
        !self.date_columns.is_empty() && !matches!(self.source.input, Input::Remote(_))
    }

    /// Completes the schema, checking cancellation between features of a local date scan.
    pub fn finish(self) -> Result<FgbSource> {
        let Self { source: mut src, date_columns } = self;
        if !date_columns.is_empty() {
            match &src.input {
                Input::Remote(_) => src.info.notes.push(format!(
                    "{} date/time columns are kept as ISO text to read this remote FlatGeobuf by range without scanning every feature",
                    date_columns.len()
                )),
                Input::Path(path) => src.classify_dates(BufReader::new(std::fs::File::open(path)?), &date_columns)?,
                Input::Bytes(bytes) => src.classify_dates(Cursor::new(bytes.clone()), &date_columns)?,
            }
        }
        if src.info.geometry_type == GeometryType::None {
            src.info.geometry_type = src.peek_kind();
        }
        Ok(src)
    }
}

impl FgbSource {
    /// Reads a local header without classifying date columns; finish it on a worker when needed.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn prepare_path(path: &Path, name: &str) -> Result<FgbOpen> {
        Self::prepare(BufReader::new(std::fs::File::open(path)?), Input::Path(path.to_path_buf()), name)
    }

    /// The same staged open for bytes retained by a project, without copying them.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn prepare_shared(bytes: super::Shared, name: &str) -> Result<FgbOpen> {
        Self::prepare(Cursor::new(bytes.clone()), Input::Bytes(bytes), name)
    }

    pub fn open_path(path: &Path, name: &str) -> Result<Self> {
        let f = BufReader::new(std::fs::File::open(path)?);
        Self::new(f, Input::Path(path.to_path_buf()), name)
    }

    /// Opens a private conversion, keeping its files alive for every shared reader.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn open_owned_path(path: &Path, name: &str, workspace: Arc<super::gdal::ReloadWorkspace>) -> Result<Self> {
        let mut source = Self::open_path(path, name)?;
        source._workspace = Some(workspace);
        Ok(source)
    }

    pub fn open_bytes(bytes: Vec<u8>, name: &str) -> Result<Self> {
        Self::open_shared(super::Shared(Arc::new(bytes)), name)
    }

    /// From bytes shared with the caller, without a copy.
    pub fn open_shared(bytes: super::Shared, name: &str) -> Result<Self> {
        Self::new(Cursor::new(bytes.clone()), Input::Bytes(bytes), name)
    }

    /// A file by its web address, read by range. Fails with [`remote::NEED`] until its header is
    /// fetched (run it with [`remote::drive`]); a file without the usual spatial index can't be
    /// read in place ([`remote::READ_WHOLE`]).
    pub fn open_remote(file: Arc<Remote>, name: &str) -> Result<Self> {
        let src = Self::new(remote::Reader::new(file.clone()), Input::Remote(file), name)?;
        if !src.info.indexed {
            return Err(DataError::Unsupported(format!("a FlatGeobuf without a spatial index {}", remote::READ_WHOLE)));
        }
        Ok(src)
    }

    /// Opens a FlatGeobuf from its header: schema, extent, coordinate system, and notes (including
    /// Z or M values, which reading drops).
    fn new<R: Read + Seek>(r: R, input: Input, name: &str) -> Result<Self> {
        Self::prepare(r, input, name)?.finish()
    }

    /// Reads only metadata; a caller must finish the native date scan before using the source.
    fn prepare<R: Read + Seek>(r: R, input: Input, name: &str) -> Result<FgbOpen> {
        let reader = open_reader(r)?;
        let h = reader.header();
        let mut notes = Vec::new();
        // The header's system is read as a GeoPackage row's is ([`super::declared_reproject`]): by
        // its EPSG code, or by its WKT when the WKT gives another shift to WGS 84 or there is no
        // EPSG code (#1855). The code is an EPSG one only when the organization is EPSG (or not
        // given, which means EPSG); 0 is none.
        let header_crs = h.crs();
        let organization = header_crs.and_then(|c| c.org());
        let epsg_org = organization.is_none_or(|o| o.eq_ignore_ascii_case("epsg"));
        let code = header_crs.map(|c| i64::from(c.code())).filter(|c| *c != 0);
        let declared = super::DeclaredCrs {
            what: "its coordinate system",
            organization,
            code,
            epsg: code.and_then(|c| u32::try_from(c).ok()).filter(|_| epsg_org),
            wkt1: super::defined(header_crs.and_then(|c| c.wkt())),
            wkt2: None,
        };
        let reproject = match super::declared_reproject(declared, &mut notes) {
            Some(reproject) => reproject?,
            // Nothing to read it by: longitude/latitude, said when another organization's code
            // names the system, or a negative code, which can't be an EPSG one.
            None => {
                if !epsg_org && let Some(org) = organization {
                    notes.push(format!("its coordinate system is defined by {org}, not EPSG; read as longitude/latitude"));
                } else if let Some(code) = code {
                    notes.push(format!("its coordinate system's code {code} isn't an EPSG code; read as longitude/latitude"));
                }
                Reproject::from_crs(Crs::LonLat, &mut notes)
            }
        };
        let mut date_columns = Vec::new();
        let mut fields = Vec::new();
        let mut slots = Vec::new();
        let mut kinds = Vec::new();
        let mut left_out = Vec::new();
        for c in h.columns().iter().flat_map(|cols| cols.iter()) {
            kinds.push(c.type_());
            match field_type(c.type_()) {
                Some(ty) => {
                    if c.type_() == ColumnType::DateTime {
                        date_columns.push(fields.len());
                    }
                    slots.push(Some(fields.len()));
                    fields.push(Field::new(c.name().to_string(), ty));
                }
                None => {
                    slots.push(None);
                    left_out.push((c.name().to_string(), "binary".to_string()));
                }
            }
        }
        let geometry_type = {
            use flatgeobuf::GeometryType as G;
            match h.geometry_type() {
                G::Point | G::MultiPoint => GeometryType::Point,
                G::LineString | G::MultiLineString => GeometryType::Polyline,
                G::Polygon | G::MultiPolygon => GeometryType::Polygon,
                _ => GeometryType::None,
            }
        };
        let extent = h
            .envelope()
            .and_then(|e| match (e.iter().next(), e.iter().nth(1), e.iter().nth(2), e.iter().nth(3)) {
                (Some(a), Some(b), Some(c), Some(d)) => Some(reproject.extent_to_lonlat(&Extent::new(a, b, c, d))),
                _ => None,
            })
            .unwrap_or(Extent::EMPTY);
        let count = h.features_count();
        let mut info = SourceInfo {
            name: h.name().filter(|n| !n.trim().is_empty()).unwrap_or(name).to_string(),
            format: "FlatGeobuf",
            geometry_type,
            fields,
            extent,
            count: (count > 0).then_some(count),
            crs: reproject.info_crs(),
            // The crate's index search assumes the default node size; other files are scanned.
            indexed: h.index_node_size() == flatgeobuf::packed_r_tree::PackedRTree::DEFAULT_NODE_SIZE && count > 0,
            notes,
            zm_dropped: false,
        };
        info.note_columns(
            &left_out,
            ["of a type RustGIS has no field for was left out", "of types RustGIS has no field for were left out"],
        );
        if h.has_z() || h.has_m() {
            info.drop_zm((count > 0).then_some(count));
        }
        let src = FgbSource {
            info,
            input,
            reproject,
            slots,
            kinds,
            #[cfg(not(target_arch = "wasm32"))]
            _workspace: None,
        };
        Ok(FgbOpen { source: src, date_columns })
    }

    /// Classifies local DateTime columns by a complete, cancellable pass. Time-only columns
    /// stay text; all-date columns become Date. Remote sources retain text to keep range reads.
    /// Checks Cancel after each decoded feature; reading and verifying one encoded feature
    /// cannot be interrupted midway.
    fn classify_dates<R: Read + Seek>(&mut self, reader: R, columns: &[usize]) -> Result<()> {
        crate::progress::begin(self.info.count.and_then(|n| usize::try_from(n).ok()).unwrap_or(0));
        let mut states = vec![0u8; self.info.fields.len()];
        let mut iter = open_reader(reader)?.select_all().map_err(err)?;
        while let Some(feature) = iter.next().map_err(err)? {
            crate::progress::step(1)?;
            let mut props = Props {
                fields: &self.info.fields,
                slots: &self.slots,
                values: vec![Value::Null; self.info.fields.len()],
                unread_dates: Vec::new(),
                rounded_dates: Vec::new(),
            };
            // Reads skip a feature with unreadable properties and report it in Batch::skipped.
            // Such a feature must not influence the inferred date type either.
            if !properties_fit(feature, &self.kinds) || feature.process_properties(&mut props).is_err() {
                continue;
            }
            for &k in columns {
                if let Some(Value::Text(s)) = props.values.get(k) {
                    let state = if crate::Date::parse(s).is_some() {
                        1
                    } else if crate::DateTime::parse_rounding(s).is_some() {
                        2
                    } else if is_clock(s) {
                        4
                    } else {
                        0
                    };
                    if let Some(v) = states.get_mut(k) {
                        *v |= state;
                    }
                }
            }
        }
        for &k in columns {
            if let Some(field) = self.info.fields.get_mut(k) {
                field.ty = match states.get(k).copied().unwrap_or(0) {
                    1 => FieldType::Date,
                    s if s & 4 != 0 => FieldType::Text,
                    _ => FieldType::DateTime,
                };
            }
        }
        Ok(())
    }

    /// Reads a request; `scan` ignores the index. A full geometry scan for a spatial request
    /// counts features and checks Cancel between them.
    fn read_with<R: Read + Seek>(&self, open: &impl Fn() -> Result<R>, req: &Request, scan: bool) -> Result<Batch> {
        let limit = clamp_limit(req.limit);
        let native = req.extent.and_then(|e| self.reproject.extent_from_lonlat(&e));
        // Preserve ordinary candidate counts; a refused view needs a geometry scan to count.
        if limit == 0 && req.extent.is_some() && native.is_none() {
            return Err(super::unbounded_count());
        }
        let reader = open_reader(open()?)?;
        let indexed = native.filter(|_| self.info.indexed && !scan);
        // Feature numbers in the file, for ids that stay put between reads: the index search's
        // results, in the order the selection visits them.
        let numbers: Option<Vec<u64>> = match indexed {
            Some(e) => Some(self.search(open()?, &e)?),
            None => None,
        };
        let mut iter = match indexed {
            Some(e) => reader.select_bbox(e.min_x, e.min_y, e.max_x, e.max_y).map_err(err)?,
            None => reader.select_all().map_err(err)?,
        };
        // The ids come from a separate walk of the index: should it ever disagree with the
        // selection, scan instead rather than give features the wrong ids.
        if let Some(nums) = &numbers
            && iter.features_count() != Some(nums.len())
        {
            return self.read_with(open, req, true);
        }
        // Without an index, an extent is tested on each feature: sample the matches as they come.
        // That includes one the file's system couldn't bound (`native` is `None`).
        let late = req.extent.is_some() && indexed.is_none();
        let matched = iter.features_count().map(|n| n as u64).or(self.info.count).unwrap_or(0);
        let mut sample = if late || matched == 0 { Sampler::new(limit) } else { Sampler::with_total(limit, matched) };
        let mut out = Batch::default();
        let mut n = 0u64;
        if late {
            crate::progress::begin(usize::try_from(matched).unwrap_or(0));
        }
        while let Some(f) = iter.next().map_err(err)? {
            if late {
                crate::progress::step(1)?;
            }
            n += 1;
            let id = numbers.as_ref().and_then(|v| v.get(usize::try_from(n - 1).unwrap_or(usize::MAX)).copied()).unwrap_or(n);
            // A cheap skip when the sample won't take this feature (the extent isn't tested yet).
            if !late && !sample.wants() {
                continue;
            }
            let geometry = f.to_wkb(CoordDimensions::xy()).ok().as_deref().and_then(wkb::decode).and_then(|g| self.reproject.geometry(g));
            let Some(geometry) = geometry else {
                out.skipped += 1;
                continue;
            };
            if !in_request(&geometry, req) || (late && !sample.wants()) {
                continue;
            }
            let mut props = Props {
                fields: &self.info.fields,
                slots: &self.slots,
                values: vec![Value::Null; self.info.fields.len()],
                unread_dates: Vec::new(),
                rounded_dates: Vec::new(),
            };
            if !properties_fit(f, &self.kinds) || f.process_properties(&mut props).is_err() {
                out.skipped += 1;
                continue;
            }
            super::add_unread_dates(&mut out.unread_dates, &props.unread_dates);
            super::add_unread_dates(&mut out.rounded_dates, &props.rounded_dates);
            sample.keep(Feature { id, geometry: Some(geometry), attrs: props.values });
        }
        out.matched = if late || matched == 0 { sample.seen() } else { matched };
        out.complete = sample.complete();
        out.features = sample.items;
        Ok(out)
    }

    /// Visits the features a complete [`FgbSource::read_with`] of `extent` gives, in the same
    /// order and with the same ids, a chunk at a time. An extent read through the index holds
    /// the feature numbers it finds (8 bytes each) for the ids, as a read does. Counts candidates
    /// and checks Cancel before filtering, including candidates the view does not keep.
    fn visit_with<R: Read + Seek>(
        &self,
        open: &impl Fn() -> Result<R>,
        extent: Option<Extent>,
        chunks: &mut super::chunk_memory::Chunker<'_>,
    ) -> Result<Batch> {
        let req = Request { extent, limit: usize::MAX };
        let reader = open_reader(open()?)?;
        let native = extent.and_then(|e| self.reproject.extent_from_lonlat(&e));
        let indexed = native.filter(|_| self.info.indexed);
        let mut numbers: Option<Vec<u64>> = match indexed {
            Some(e) => Some(self.search(open()?, &e)?),
            None => None,
        };
        let mut iter = match indexed {
            Some(e) => reader.select_bbox(e.min_x, e.min_y, e.max_x, e.max_y).map_err(err)?,
            None => reader.select_all().map_err(err)?,
        };
        // As in a read: an index walk that disagrees with the selection is not trusted for ids.
        if let Some(nums) = &numbers
            && iter.features_count() != Some(nums.len())
        {
            numbers = None;
            iter = open_reader(open()?)?.select_all().map_err(err)?;
        }
        let mut out = Batch { complete: true, ..Batch::default() };
        let mut n = 0u64;
        crate::progress::begin(iter.features_count().unwrap_or(0));
        while let Some(f) = iter.next().map_err(err)? {
            crate::progress::step(1)?;
            n += 1;
            let id = numbers.as_ref().and_then(|v| v.get(usize::try_from(n - 1).unwrap_or(usize::MAX)).copied()).unwrap_or(n);
            let geometry = f.to_wkb(CoordDimensions::xy()).ok().as_deref().and_then(wkb::decode).and_then(|g| self.reproject.geometry(g));
            let Some(geometry) = geometry else {
                out.skipped += 1;
                continue;
            };
            if !in_request(&geometry, &req) {
                continue;
            }
            let mut props = Props {
                fields: &self.info.fields,
                slots: &self.slots,
                values: vec![Value::Null; self.info.fields.len()],
                unread_dates: Vec::new(),
                rounded_dates: Vec::new(),
            };
            if !properties_fit(f, &self.kinds) || f.process_properties(&mut props).is_err() {
                out.skipped += 1;
                continue;
            }
            super::add_unread_dates(&mut out.unread_dates, &props.unread_dates);
            super::add_unread_dates(&mut out.rounded_dates, &props.rounded_dates);
            out.matched += 1;
            chunks.push(Feature { id, geometry: Some(geometry), attrs: props.values })?;
        }
        chunks.flush()?;
        Ok(out)
    }

    /// The geometry type of a file that declares none (mixed output), from its first features: a
    /// read that stops there, not a pass over the file.
    fn peek_kind(&self) -> GeometryType {
        let first = |r: Box<dyn ReadSeek>| -> Option<GeometryType> {
            let mut iter = open_reader(r).ok()?.select_all().ok()?;
            for _ in 0..16 {
                let f = iter.next().ok()??;
                if let Some(g) = f.to_wkb(CoordDimensions::xy()).ok().as_deref().and_then(wkb::decode) {
                    return Some(g.kind());
                }
            }
            None
        };
        let r: Option<Box<dyn ReadSeek>> = match &self.input {
            Input::Path(p) => std::fs::File::open(p).ok().map(|f| Box::new(BufReader::new(f)) as Box<dyn ReadSeek>),
            Input::Bytes(b) => Some(Box::new(Cursor::new(b.clone()))),
            Input::Remote(f) => Some(Box::new(remote::Reader::new(f.clone()))),
        };
        r.and_then(first).unwrap_or(GeometryType::None)
    }

    /// The 1-based numbers of the features the index finds in a native extent, in file order.
    fn search<R: Read + Seek>(&self, mut r: R, e: &Extent) -> Result<Vec<u64>> {
        // The index follows the magic bytes, the header's size and the header.
        let mut head = [0u8; 12];
        r.read_exact(&mut head)?;
        let size = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
        r.seek(std::io::SeekFrom::Start(12 + u64::from(size)))?;
        let count = usize::try_from(self.info.count.unwrap_or(0)).unwrap_or(0);
        let hits = flatgeobuf::packed_r_tree::PackedRTree::stream_search(
            &mut r,
            count,
            flatgeobuf::packed_r_tree::PackedRTree::DEFAULT_NODE_SIZE,
            e.min_x,
            e.min_y,
            e.max_x,
            e.max_y,
        )
        .map_err(err)?;
        Ok(hits.into_iter().map(|h| h.index as u64 + 1).collect())
    }
}

/// A packed R-tree node's size in bytes.
const NODE: u64 = 40;
/// The most index bytes one level of a search fetches (past it, branches are thinned).
const INDEX_BUDGET: u64 = if cfg!(target_arch = "wasm32") { 4 << 20 } else { 8 << 20 };
/// Neighbouring branches followed together when a search thins the tree.
const BRANCH_RUN: usize = 32;
/// The most bytes of features one read fetches (past it, a sample of runs of them).
const FEATURE_BUDGET: u64 = if cfg!(target_arch = "wasm32") { 12 << 20 } else { 24 << 20 };
/// Consecutive features taken together when a read samples (runs of them, evenly spread: their
/// bytes are fetched together, where single features spread out would mean the whole file).
const RUN: usize = 512;
/// A feature's bytes guessed when the index doesn't give its end (more are read if needed).
const FEATURE_GUESS: u64 = 16 << 10;
/// A feature size prefix and an empty flatbuffers table: the stand-in for a feature a sample
/// passes over (the reader needs something to step past; smaller than any real feature).
const STAND_IN: [u8; 16] = [12, 0, 0, 0, 8, 0, 0, 0, 4, 0, 4, 0, 4, 0, 0, 0];
#[cfg(test)]
pub(crate) const STAND_IN_FOR_TESTS: [u8; 16] = STAND_IN;

/// A packed R-tree's levels, leaves first: each one's first node and end (node numbers), as the
/// format lays them out (root first in the file). `None` for a count no index could have.
fn level_bounds(items: u64, node_size: u64) -> Option<Vec<(u64, u64)>> {
    if items == 0 || node_size < 2 || items > 1 << 40 {
        return None;
    }
    let mut counts = vec![items];
    let mut n = items;
    let mut total = items;
    while n > 1 {
        n = n.div_ceil(node_size);
        counts.push(n);
        total = total.checked_add(n)?;
    }
    let mut out = Vec::with_capacity(counts.len());
    let mut end = total;
    for c in counts {
        out.push((end - c, end));
        end -= c;
    }
    Some(out)
}

/// What an index search of a remote file found.
struct Hits {
    /// (offset in the feature section, feature number), in file order.
    items: Vec<(u64, u64)>,
    /// How many matches each hit stands for (more than one when the search had to thin the tree).
    thin: u64,
    /// The node runs the thinning passed over: (where in the file, how many nodes). The crate's
    /// own search reads them, so they are served as nodes that meet nothing.
    passed: Vec<(u64, u64)>,
}

impl FgbSource {
    /// The index's start and the feature section's, for a file whose header is held.
    fn sections(&self, f: &Remote) -> Result<(u64, u64)> {
        let head = f.bytes(8, 4).ok_or_else(remote::need)?;
        let size = head
            .get(..4)
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or_else(|| err(flatgeobuf::Error::MissingMagicBytes))?;
        let index = 12 + u64::from(size);
        let count = self.info.count.unwrap_or(0);
        let levels = level_bounds(count, u64::from(flatgeobuf::packed_r_tree::PackedRTree::DEFAULT_NODE_SIZE))
            .ok_or_else(|| DataError::Format("FlatGeobuf: an index for an impossible number of features".into()))?;
        let nodes = levels.first().map(|l| l.1).unwrap_or(0);
        Ok((index, index.saturating_add(nodes.saturating_mul(NODE))))
    }

    /// The features a native extent meets, by the index, a level at a time: each level's nodes
    /// are asked for together, so a search takes as many rounds of fetching as the tree has
    /// levels. A level too large to fetch for one read is thinned (every n-th branch followed).
    fn remote_search(&self, f: &Remote, index: u64, e: &Extent) -> Result<Hits> {
        let count = self.info.count.unwrap_or(0);
        let node_size = u64::from(flatgeobuf::packed_r_tree::PackedRTree::DEFAULT_NODE_SIZE);
        let levels = level_bounds(count, node_size).ok_or_else(|| DataError::Format("FlatGeobuf: an impossible index".into()))?;
        let (leaf_start, leaf_end) = levels.first().copied().unwrap_or((0, 0));
        let bounds = flatgeobuf::packed_r_tree::NodeItem::bounds(e.min_x, e.min_y, e.max_x, e.max_y);
        let mut level = levels.len().saturating_sub(1);
        let mut queue: Vec<u64> = vec![levels.get(level).map(|l| l.0).unwrap_or(0)];
        let mut items = Vec::new();
        let mut thin = 1u64;
        let mut passed = Vec::new();
        while let Some(&(start, end)) = levels.get(level) {
            // Too many nodes to fetch for one view: follow an evenly spread part of them.
            let budget = usize::try_from(INDEX_BUDGET / (NODE * node_size)).unwrap_or(usize::MAX).max(1);
            if queue.len() > budget {
                // In runs of neighbouring branches: single ones spread out would be fetched as
                // one range (the gaps between them are smaller than a fetch).
                let step = queue.len().div_ceil(budget);
                let (kept, left): (Vec<(usize, u64)>, Vec<(usize, u64)>) =
                    queue.into_iter().enumerate().partition(|(i, _)| (i / BRANCH_RUN).is_multiple_of(step));
                for (_, n) in left {
                    passed.push((index + n * NODE, n.saturating_add(node_size).min(end).saturating_sub(n)));
                }
                queue = kept.into_iter().map(|(_, n)| n).collect();
                thin = thin.saturating_mul(step as u64);
            }
            let spans: Vec<(u64, u64)> =
                queue.iter().map(|&n| (n, n.saturating_add(node_size).min(end))).filter(|(a, b)| a < b && *a >= start).collect();
            // Every node of the level asked for before any is read.
            let mut held = true;
            for &(a, b) in &spans {
                held &= f.want(index + a * NODE, (b - a) * NODE);
            }
            if !held {
                return Err(remote::need());
            }
            let mut next = Vec::new();
            for (a, b) in spans {
                let bytes = f.bytes(index + a * NODE, (b - a) * NODE).ok_or_else(remote::need)?;
                for (k, raw) in bytes.as_chunks::<{ NODE as usize }>().0.iter().enumerate() {
                    let node = flatgeobuf::packed_r_tree::NodeItem::from_reader(raw.as_slice()).map_err(err)?;
                    if !bounds.intersects(&node) {
                        continue;
                    }
                    if level == 0 {
                        items.push((node.offset, a + k as u64 - leaf_start));
                    } else {
                        // A child must be a node of the level below.
                        let below = levels.get(level - 1).copied().unwrap_or((0, 0));
                        if node.offset < below.0 || node.offset >= below.1 {
                            return Err(DataError::Format("FlatGeobuf: an index node points outside its level".into()));
                        }
                        next.push(node.offset);
                    }
                }
            }
            if level == 0 || next.is_empty() {
                break;
            }
            queue = next;
            level -= 1;
        }
        let _ = leaf_end;
        Ok(Hits { items, thin, passed })
    }

    /// A request on a file by its web address: the index search, then the features the sample
    /// takes, fetched together before any is read; the others are passed over unread. An unbounded
    /// spatial read scans every candidate within the budget, counting features and checking Cancel.
    fn read_remote(&self, f: &Arc<Remote>, req: &Request) -> Result<Batch> {
        let limit = clamp_limit(req.limit);
        let native = req.extent.and_then(|e| self.reproject.extent_from_lonlat(&e));
        // Preserve ordinary candidate counts; a refused view needs a geometry scan to count.
        if limit == 0 && req.extent.is_some() && native.is_none() {
            return Err(super::unbounded_count());
        }
        let late = req.extent.is_some() && native.is_none();
        let native = native.unwrap_or(Extent::new(f64::MIN, f64::MIN, f64::MAX, f64::MAX));
        let (index, features) = self.sections(f)?;
        let hits = self.remote_search(f, index, &native)?;
        if late && hits.thin > 1 {
            return Err(remote::view_budget());
        }
        let n = hits.items.len();
        // Each hit's bytes: to the next feature when that follows it, else a guess.
        let size = |k: usize| -> u64 {
            match (hits.items.get(k), hits.items.get(k + 1)) {
                (Some(&(a, i)), Some(&(b, j))) if j == i + 1 && b > a => (b - a).min(remote::READ_BUDGET),
                _ => FEATURE_GUESS,
            }
        };
        // A sample: runs of consecutive features, every `step`-th run (fewer when their bytes
        // would be more than a read may fetch).
        // A refused native view must match geometry before any feature is sampled away.
        let mut step = if late { 1 } else { usize::try_from(stride(n as u64, limit)).unwrap_or(usize::MAX).max(1) };
        // Runs short enough for the sample to be spread over many places.
        let run = (limit / 256).clamp(1, RUN);
        let taken = |k: usize, step: usize| step == 1 || (k / run).is_multiple_of(step);
        loop {
            let bytes: u64 = (0..n).filter(|&k| taken(k, step)).map(size).sum();
            if bytes <= FEATURE_BUDGET {
                break;
            }
            if late {
                return Err(remote::view_budget());
            }
            if step >= n.max(1) {
                break;
            }
            step = step.saturating_mul(2);
        }
        let mut held = true;
        for k in (0..n).filter(|&k| taken(k, step)) {
            if let Some(&(off, _)) = hits.items.get(k) {
                held &= f.want(features + off, size(k));
            }
        }
        if !held {
            return Err(remote::need());
        }
        // The features passed over (and the first feature's size, read before any seek, when
        // the sample doesn't start there) are stand-ins.
        let feature = Arc::new(STAND_IN.to_vec());
        let mut at: std::collections::HashMap<u64, Arc<Vec<u8>>> = hits
            .items
            .iter()
            .enumerate()
            .filter(|(k, _)| !taken(*k, step))
            .map(|(_, (off, _))| (features + off, feature.clone()))
            .collect();
        if hits.items.first().is_none_or(|(off, _)| *off != 0) || !taken(0, step) {
            at.insert(features, feature.clone());
        }
        // Index nodes the search passed over: nodes that meet nothing (inverted bounds).
        let mut nothing = Vec::with_capacity(NODE as usize);
        for v in [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY] {
            nothing.extend_from_slice(&v.to_le_bytes());
        }
        nothing.extend_from_slice(&0u64.to_le_bytes());
        for &(pos, count) in &hits.passed {
            let bytes: Vec<u8> = nothing.iter().copied().cycle().take(usize::try_from(count * NODE).unwrap_or(0)).collect();
            at.insert(pos, Arc::new(bytes));
        }
        let stand_ins = Arc::new(remote::StandIns { at });
        let reader = open_reader(remote::Reader::with_stand_ins(f.clone(), stand_ins))?;
        let mut iter = reader.select_bbox(native.min_x, native.min_y, native.max_x, native.max_y).map_err(err)?;
        // The crate's search must find what this one did (it reads the nodes fetched for it).
        if iter.features_count() != Some(n) {
            return Err(DataError::Format("FlatGeobuf: the index search found a different set of features".into()));
        }
        let mut out = Batch::default();
        let mut sample = Sampler::new(limit);
        let mut k = 0usize;
        if late {
            crate::progress::begin(n);
        }
        while let Some(feat) = iter.next().map_err(err)? {
            if late {
                crate::progress::step(1)?;
            }
            let this = k;
            k += 1;
            if !taken(this, step) {
                continue;
            }
            let id = hits.items.get(this).map(|(_, i)| i + 1).unwrap_or(this as u64 + 1);
            let geometry =
                feat.to_wkb(CoordDimensions::xy()).ok().as_deref().and_then(wkb::decode).and_then(|g| self.reproject.geometry(g));
            let Some(geometry) = geometry else {
                out.skipped += 1;
                continue;
            };
            if !in_request(&geometry, req) || (late && !sample.wants()) {
                continue;
            }
            let mut props = Props {
                fields: &self.info.fields,
                slots: &self.slots,
                values: vec![Value::Null; self.info.fields.len()],
                unread_dates: Vec::new(),
                rounded_dates: Vec::new(),
            };
            if !properties_fit(feat, &self.kinds) || feat.process_properties(&mut props).is_err() {
                out.skipped += 1;
                continue;
            }
            super::add_unread_dates(&mut out.unread_dates, &props.unread_dates);
            super::add_unread_dates(&mut out.rounded_dates, &props.rounded_dates);
            let feature = Feature { id, geometry: Some(geometry), attrs: props.values };
            if late {
                sample.keep(feature);
            } else {
                out.features.push(feature);
                if out.features.len() >= limit {
                    break;
                }
            }
        }
        if late {
            out.matched = sample.seen();
            out.complete = sample.complete();
            out.features = sample.items;
        } else {
            out.matched = (n as u64).saturating_mul(hits.thin);
            out.complete = step == 1 && hits.thin == 1;
        }
        Ok(out)
    }
}

impl FeatureSource for FgbSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    fn read(&self, req: &Request) -> Result<Batch> {
        match &self.input {
            Input::Path(p) => self.read_with(&|| Ok(BufReader::new(std::fs::File::open(p)?)), req, false),
            Input::Bytes(b) => self.read_with(&|| Ok(Cursor::new(b.clone())), req, false),
            Input::Remote(f) => self.read_remote(f, req),
        }
    }

    fn remote(&self) -> Option<Arc<Remote>> {
        match &self.input {
            Input::Remote(f) => Some(f.clone()),
            _ => None,
        }
    }

    /// A file or an upload; a file by its web address is read a view at a time, not in full.
    fn visits_chunks(&self) -> bool {
        !matches!(self.input, Input::Remote(_))
    }

    /// In file order, or the index's order for an extent, as a read; one feature decoded at a
    /// time from the file or upload.
    fn visit_chunks(&self, extent: Option<Extent>, bytes: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        let mut chunks = super::chunk_memory::Chunker::new(bytes, visit)?;
        match &self.input {
            Input::Path(p) => self.visit_with(&|| Ok(BufReader::new(std::fs::File::open(p)?)), extent, &mut chunks),
            Input::Bytes(b) => self.visit_with(&|| Ok(Cursor::new(b.clone())), extent, &mut chunks),
            Input::Remote(_) => Err(DataError::Unsupported("a whole FlatGeobuf by its web address read a chunk at a time".into())),
        }
    }
}

/// Every local and remote selection checks feature prefixes before allocating their bodies.
fn open_reader<R: Read + Seek>(r: R) -> Result<checked::Reader<R>> {
    checked::Reader::open(r)
}

fn err(e: flatgeobuf::Error) -> DataError {
    DataError::Format(format!("FlatGeobuf: {e}"))
}

/// Validates a complete ISO clock by supplying a neutral day. The original stays text,
/// so fractional digits past microseconds are checked as digits but never removed from values.
fn is_clock(s: &str) -> bool {
    let s = s.trim();
    if s.as_bytes().get(2) != Some(&b':') {
        return false;
    }
    let check = if let Some((head, tail)) = s.split_once('.') {
        let digits = tail.bytes().take_while(|c| c.is_ascii_digit()).count();
        format!("{head}.{}{}", tail.get(..digits.min(6)).unwrap_or_default(), tail.get(digits..).unwrap_or_default())
    } else {
        s.to_string()
    };
    crate::DateTime::parse(&format!("2000-01-01T{check}")).is_some()
}

/// The field type a column is read as; `None` for binary, which RustGIS has no field type for.
fn field_type(t: ColumnType) -> Option<FieldType> {
    Some(match t {
        ColumnType::Byte
        | ColumnType::UByte
        | ColumnType::Short
        | ColumnType::UShort
        | ColumnType::Int
        | ColumnType::UInt
        | ColumnType::Long
        | ColumnType::ULong => FieldType::Integer,
        ColumnType::Float | ColumnType::Double => FieldType::Double,
        ColumnType::Bool => FieldType::Boolean,
        ColumnType::Binary => return None,
        _ => FieldType::Text,
    })
}

/// Whether a feature's property bytes hold what they say, laid out as FlatGeobuf has them: each a
/// column index (`u16`), then the value, its width fixed by the column's type, or a `u32` length
/// and that many bytes for text, JSON, date-times and binary. flatgeobuf 6.0.1 slices them as the
/// file says and panics on a length past the end, which the browser can't catch; the fix
/// (flatgeobuf#544) isn't released yet. A feature that fails this is skipped as unreadable, as one
/// whose properties the reader refuses. Remove it once a release with that fix is used.
fn properties_fit(feature: &flatgeobuf::FgbFeature, kinds: &[ColumnType]) -> bool {
    let Some(bytes) = feature.fbs_feature().properties().map(|p| p.bytes()) else { return true };
    let mut at = 0usize;
    // As the reader does, a single byte left at the end is ignored.
    while at.saturating_add(1) < bytes.len() {
        let Some(&[lo, hi]) = bytes.get(at..at + 2) else { return false };
        at += 2;
        // An index past the columns is the reader's own error, not a panic.
        let Some(kind) = kinds.get(usize::from(u16::from_le_bytes([lo, hi]))) else { return true };
        let width = match *kind {
            ColumnType::Byte | ColumnType::UByte | ColumnType::Bool => 1,
            ColumnType::Short | ColumnType::UShort => 2,
            ColumnType::Int | ColumnType::UInt | ColumnType::Float => 4,
            ColumnType::Long | ColumnType::ULong | ColumnType::Double => 8,
            ColumnType::String | ColumnType::Json | ColumnType::DateTime | ColumnType::Binary => {
                let Some(&[a, b, c, d]) = bytes.get(at..at.saturating_add(4)) else { return false };
                at += 4;
                usize::try_from(u32::from_le_bytes([a, b, c, d])).unwrap_or(usize::MAX)
            }
            // A type the reader doesn't know: it reads nothing for it, and neither does this.
            _ => 0,
        };
        match at.checked_add(width) {
            Some(end) if end <= bytes.len() => at = end,
            _ => return false,
        }
    }
    true
}

/// Collects a feature's properties into values in field order.
struct Props<'a> {
    fields: &'a [Field],
    /// The field of each header column (see [`FgbSource::slots`]).
    slots: &'a [Option<usize>],
    values: Vec<Value>,
    unread_dates: Vec<usize>,
    rounded_dates: Vec<usize>,
}

impl PropertyProcessor for Props<'_> {
    fn property(&mut self, idx: usize, _name: &str, v: &ColumnValue) -> flatgeobuf::geozero::error::Result<bool> {
        // A column left out (binary), or past the header's.
        let Some(&Some(idx)) = self.slots.get(idx) else { return Ok(false) };
        let value = match v {
            ColumnValue::Byte(x) => Value::Int(i64::from(*x)),
            ColumnValue::UByte(x) => Value::Int(i64::from(*x)),
            ColumnValue::Bool(x) => Value::Bool(*x),
            ColumnValue::Short(x) => Value::Int(i64::from(*x)),
            ColumnValue::UShort(x) => Value::Int(i64::from(*x)),
            ColumnValue::Int(x) => Value::Int(i64::from(*x)),
            ColumnValue::UInt(x) => Value::Int(i64::from(*x)),
            ColumnValue::Long(x) => Value::Int(*x),
            ColumnValue::ULong(x) => i64::try_from(*x).map(Value::Int).unwrap_or(Value::Float(*x as f64)),
            ColumnValue::Float(x) => Value::Float(f64::from(*x)),
            ColumnValue::Double(x) => Value::Float(*x),
            ColumnValue::String(s) | ColumnValue::Json(s) | ColumnValue::DateTime(s) => Value::Text((*s).to_string()),
            ColumnValue::Binary(_) => Value::Null,
        };
        // Integers in a Double column stay numbers of the column's type.
        let value = match (self.fields.get(idx).map(|f| f.ty), value) {
            (Some(FieldType::Double), Value::Int(i)) => Value::Float(i as f64),
            (Some(ty @ (FieldType::Date | FieldType::DateTime)), Value::Text(s)) => {
                let (value, rounded) = Value::parse_stored_counted(&s, ty);
                if !s.trim().is_empty() && value.is_null() {
                    super::unread_date(&mut self.unread_dates, idx);
                } else if rounded {
                    super::unread_date(&mut self.rounded_dates, idx);
                }
                value
            }
            (_, v) => v,
        };
        if let Some(slot) = self.values.get_mut(idx) {
            *slot = value;
        }
        Ok(false)
    }
}

/// Writes a dataset as FlatGeobuf, with a spatial index (WGS 84 longitude/latitude).
pub fn write(ds: &crate::Dataset) -> Result<Vec<u8>> {
    write_in(ds, Crs::LonLat)
}

/// Writes a dataset whose geometry is in `crs`'s coordinates as FlatGeobuf, with a spatial index,
/// the file saying `crs`.
pub fn write_in(ds: &crate::Dataset, crs: Crs) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_features(&ds.name, ds.geometry_type, &ds.fields, crs, &mut out, &mut |add| add(&ds.features))?;
    Ok(out)
}

/// Refuses to write FlatGeobuf in the browser, before the writer is made: its temporary file
/// would reach `std::env::temp_dir`, which aborts the web app. (A flag rather than `#[cfg]`, so
/// both cases are tested natively.)
fn writable(browser: bool) -> Result<()> {
    if browser {
        return Err(DataError::Unsupported("FlatGeobuf in the browser".into()));
    }
    Ok(())
}

/// Writes FlatGeobuf with a spatial index to `out`, in `crs`'s coordinates: the features `feed`
/// hands to the function it is given, a run at a time, their attributes named by `fields`.
/// Returns how many were left out for having no geometry. The writer keeps each feature in a
/// temporary file and only its index entry in memory, so a whole layer needn't be held. The
/// writer needs that file whether or not it writes an index, and the web app has no files, so
/// there a FlatGeobuf is refused rather than written.
///
/// A point layer's file takes the type of its first point. Should a later one be of the other
/// kind (a multipoint after points, or a point after multipoints), the export starts again
/// (feeding the features again) with each feature's type in the file, which a file of one type
/// can't hold. Nothing reaches `out` before the last feature, so the first run leaves it empty.
pub(crate) fn write_features(
    name: &str,
    geometry_type: GeometryType,
    fields: &[Field],
    crs: Crs,
    out: &mut dyn std::io::Write,
    feed: &mut dyn FnMut(&mut dyn FnMut(&[Feature]) -> Result<()>) -> Result<()>,
) -> Result<usize> {
    for mixed_points in [false, true] {
        if let Some(left_out) = write_run(name, geometry_type, fields, crs, mixed_points, out, feed)? {
            return Ok(left_out);
        }
    }
    Err(DataError::Format("FlatGeobuf: the export didn't settle on a geometry type".into()))
}

/// One run of [`write_features`], each feature's geometry type in the file when `mixed_points`:
/// how many were left out, or `None` when a point layer turned out to mix points and
/// multipoints and the export must start again so. With `mixed_points`, a point layer's
/// geometry of another kind (a line, a polygon) is refused: a file of mixed types is read as
/// the layer of its first feature's kind.
fn write_run(
    name: &str,
    geometry_type: GeometryType,
    fields: &[Field],
    crs: Crs,
    mixed_points: bool,
    out: &mut dyn std::io::Write,
    feed: &mut dyn FnMut(&mut dyn FnMut(&[Feature]) -> Result<()>) -> Result<()>,
) -> Result<Option<usize>> {
    use flatgeobuf::geozero::wkb::Wkb;
    use flatgeobuf::{FgbCrs, FgbWriter, FgbWriterOptions, GeometryType as G};
    writable(cfg!(target_arch = "wasm32"))?;
    let ty = match geometry_type {
        GeometryType::Point => G::Unknown,
        GeometryType::Polyline => G::MultiLineString,
        GeometryType::Polygon => G::MultiPolygon,
        GeometryType::None => return Err(DataError::Unsupported("a table without geometry as FlatGeobuf".into())),
    };
    let wkt = crate::raster::wkt::to_wkt(crs);
    let code = i32::from(crs.epsg());
    let options = FgbWriterOptions {
        crs: FgbCrs { org: (code > 0).then_some("EPSG"), code, wkt: wkt.as_deref(), ..Default::default() },
        detect_type: !mixed_points,
        ..Default::default()
    };
    let mut w = FgbWriter::create_with_options(name, ty, options).map_err(err)?;
    for f in fields {
        let t = match f.ty {
            FieldType::Integer => ColumnType::Long,
            FieldType::Double => ColumnType::Double,
            FieldType::Boolean => ColumnType::Bool,
            FieldType::Text => ColumnType::String,
            FieldType::Date | FieldType::DateTime => ColumnType::DateTime,
        };
        w.add_column(&f.name, t, |_, c| c.nullable = true);
    }
    let (mut left_out, mut first_multi, mut again) = (0, None, false);
    let fed = feed(&mut |features| {
        for feat in features {
            crate::progress::step(1)?;
            let Some(g) = &feat.geometry else {
                left_out += 1;
                continue;
            };
            if geometry_type == GeometryType::Point {
                let multi = match g {
                    crate::Geometry::Point(_) => Some(false),
                    crate::Geometry::MultiPoint(_) => Some(true),
                    _ => None,
                };
                match (multi, mixed_points) {
                    (Some(m), false) if *first_multi.get_or_insert(m) != m => {
                        again = true;
                        // Stops the traversal; the export starts again.
                        return Err(DataError::Format("FlatGeobuf: starting again".into()));
                    }
                    (None, true) => {
                        return Err(DataError::Format(format!(
                            "FlatGeobuf: feature {} of a point layer isn't a point or multipoint",
                            feat.id
                        )));
                    }
                    _ => {}
                }
            }
            // Refuse an invalid date before writing its native column: otherwise another reader
            // could replace it with null, while this writer reported a successful, lossless export.
            // A date is what a read takes as one (digits past the microsecond included: the text
            // is written as it is, and a read rounds it and says so).
            for (field, value) in fields.iter().zip(&feat.attrs) {
                if field.ty.is_temporal() && !value.is_null() && Value::parse_stored_counted(&value.to_string(), field.ty).0.is_null() {
                    return Err(DataError::Format(format!(
                        "FlatGeobuf: feature {} has an invalid date in '{}'",
                        feat.id,
                        crate::io::dbf_notice_name(&field.name)
                    )));
                }
            }
            let mut failed = None;
            w.add_feature_geom(Wkb(wkb::encode(g)), |out| {
                use flatgeobuf::geozero::PropertyProcessor;
                for (i, (f, v)) in fields.iter().zip(&feat.attrs).enumerate() {
                    let r = match v {
                        Value::Null => continue,
                        Value::Int(x) => out.property(i, &f.name, &ColumnValue::Long(*x)),
                        Value::Float(x) => out.property(i, &f.name, &ColumnValue::Double(*x)),
                        Value::Bool(x) => out.property(i, &f.name, &ColumnValue::Bool(*x)),
                        // A date field's text as its day (a midnight `…T00:00:00` too), so a read
                        // still finds a column of dates; a date-time's text as it is.
                        Value::Text(x) if f.ty == FieldType::Date => match Value::parse_stored_counted(x, f.ty).0 {
                            Value::Date(d) => out.property(i, &f.name, &ColumnValue::DateTime(&d.to_string())),
                            _ => out.property(i, &f.name, &ColumnValue::DateTime(x)),
                        },
                        Value::Text(x) if f.ty == FieldType::DateTime => out.property(i, &f.name, &ColumnValue::DateTime(x)),
                        Value::Text(x) => out.property(i, &f.name, &ColumnValue::String(x)),
                        // A midnight in a date field as its day, as text that is one.
                        Value::DateTime(t) if f.ty == FieldType::Date && t.is_date() => {
                            out.property(i, &f.name, &ColumnValue::DateTime(&t.date().to_string()))
                        }
                        Value::Date(_) | Value::DateTime(_) => out.property(i, &f.name, &ColumnValue::DateTime(&v.to_string())),
                    };
                    if let Err(e) = r {
                        failed = Some(e.to_string());
                    }
                }
            })
            .map_err(|e| DataError::Format(format!("FlatGeobuf: {e}")))?;
            if let Some(e) = failed {
                return Err(DataError::Format(format!("FlatGeobuf: {e}")));
            }
        }
        Ok(())
    });
    if again {
        return Ok(None);
    }
    fed?;
    w.write(out).map_err(err)?;
    Ok(Some(left_out))
}

#[cfg(test)]
mod write_tests {
    use super::*;

    /// The browser refuses FlatGeobuf before the writer, whose temporary file aborted the web
    /// app, is made; the desktop writes it.
    #[test]
    fn flatgeobuf_is_refused_in_the_browser_and_written_on_the_desktop() {
        assert!(matches!(writable(true), Err(DataError::Unsupported(m)) if m.contains("browser")));
        assert!(writable(false).is_ok());
    }

    /// A point layer with points and multipoints (as a GeoJSON file gives) is written whole,
    /// each feature its own kind; a layer of points alone keeps its one geometry type.
    #[test]
    fn a_point_layer_mixing_points_and_multipoints_is_written() {
        use crate::{Coord, Dataset, Geometry};
        let mut ds = Dataset::new("m", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
        ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Int(1)]);
        ds.push(Some(Geometry::MultiPoint(vec![Coord::new(3.0, 4.0), Coord::new(5.0, 6.0)])), vec![Value::Int(2)]);
        let bytes = write_in(&ds, Crs::LonLat).unwrap();
        let (back, _) = crate::io::read_bytes("m.fgb", &bytes).unwrap();
        let mut got: Vec<(Value, Option<Geometry>)> = back.features.iter().map(|f| (f.attrs[0].clone(), f.geometry.clone())).collect();
        got.sort_by_key(|(n, _)| n.as_f64().map(|n| n as i64));
        let want: Vec<(Value, Option<Geometry>)> = ds.features.iter().map(|f| (f.attrs[0].clone(), f.geometry.clone())).collect();
        assert_eq!(got, want);
        let mut points = Dataset::new("p", GeometryType::Point, vec![]);
        points.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![]);
        let header = flatgeobuf::FgbReader::open(std::io::Cursor::new(write_in(&points, Crs::LonLat).unwrap())).unwrap();
        assert_eq!(header.header().geometry_type(), flatgeobuf::GeometryType::Point);
    }

    /// Fed a chunk at a time (a layer read in place), the multipoint arrives after the point
    /// has set the file's type: the export starts again and writes both. A line in a point layer
    /// isn't written into a file of mixed types, which would read back as a line layer.
    #[test]
    fn a_streamed_point_layer_mixing_points_and_multipoints_starts_again() {
        use crate::{Coord, Geometry};
        let chunks = [
            vec![Feature { id: 1, geometry: Some(Geometry::Point(Coord::new(1.0, 2.0))), attrs: vec![] }],
            vec![Feature { id: 2, geometry: Some(Geometry::MultiPoint(vec![Coord::new(3.0, 4.0), Coord::new(5.0, 6.0)])), attrs: vec![] }],
        ];
        let mut runs = 0;
        let mut out = Vec::new();
        write_features("m", GeometryType::Point, &[], Crs::LonLat, &mut out, &mut |add| {
            runs += 1;
            chunks.iter().try_for_each(|c| add(c))
        })
        .unwrap();
        assert_eq!(runs, 2);
        let (back, _) = crate::io::read_bytes("m.fgb", &out).unwrap();
        let mut got: Vec<_> = back.features.iter().filter_map(|f| f.geometry.clone()).collect();
        got.sort_by_key(|g| matches!(g, Geometry::MultiPoint(_)));
        assert_eq!(got, chunks.iter().flatten().filter_map(|f| f.geometry.clone()).collect::<Vec<_>>());
        let line = Feature { id: 3, geometry: Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]])), attrs: vec![] };
        let with_line = [chunks[0].clone(), chunks[1].clone(), vec![line]];
        let e = write_features("m", GeometryType::Point, &[], Crs::LonLat, &mut Vec::new(), &mut |add| {
            with_line.iter().try_for_each(|c| add(c))
        });
        assert!(matches!(&e, Err(DataError::Format(m)) if m.contains("feature 3 of a point layer isn't a point")), "{e:?}");
    }
}
