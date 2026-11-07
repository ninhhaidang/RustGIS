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
}

impl FgbSource {
    pub fn open_path(path: &Path, name: &str) -> Result<Self> {
        let f = BufReader::new(std::fs::File::open(path)?);
        Self::new(f, Input::Path(path.to_path_buf()), name)
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
        let reader = open_reader(r)?;
        let h = reader.header();
        let mut notes = Vec::new();
        // The code is an EPSG one only when the organization is EPSG (or not given, which means EPSG).
        let epsg_org = h.crs().is_none_or(|c| c.org().is_none_or(|o| o.eq_ignore_ascii_case("epsg")));
        let code = h.crs().map(|c| c.code()).and_then(|c| u32::try_from(c).ok()).filter(|c| *c > 0 && epsg_org);
        // Another body's system (an IAU organization, or WKT that says so) is said, or refused when
        // its coordinates aren't longitude/latitude. It is checked whenever there's no EPSG code
        // RustGIS converts, because an unknown code is read as longitude/latitude too.
        let other_body = match h.crs() {
            Some(c) if code.and_then(Crs::from_epsg).is_none() => {
                Reproject::planetary("its coordinate system", c.org(), Some(i64::from(c.code())), c.wkt(), &mut notes)
            }
            _ => None,
        };
        let reproject = match other_body {
            Some(r) => r?,
            None => {
                if !epsg_org && let Some(org) = h.crs().and_then(|c| c.org()) {
                    notes.push(format!("its coordinate system is defined by {org}, not EPSG; read as longitude/latitude"));
                }
                // A coordinate system described (by WKT or name) but without an EPSG code can't be converted.
                if code.is_none() && h.crs().is_some_and(|c| c.wkt().is_some_and(|w| !w.contains("WGS 84") && !w.contains("WGS84"))) {
                    notes.push("its coordinate system has no EPSG code; read as longitude/latitude".into());
                }
                Reproject::from_epsg(code, &mut notes)
            }
        };
        let mut fields = Vec::new();
        let mut slots = Vec::new();
        let mut left_out = Vec::new();
        for c in h.columns().iter().flat_map(|cols| cols.iter()) {
            match field_type(c.type_()) {
                Some(ty) => {
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
        let mut src = FgbSource { info, input, reproject, slots };
        if src.info.geometry_type == GeometryType::None {
            src.info.geometry_type = src.peek_kind();
        }
        Ok(src)
    }

    /// Reads a request; `scan` ignores the index.
    fn read_with<R: Read + Seek>(&self, open: &impl Fn() -> Result<R>, req: &Request, scan: bool) -> Result<Batch> {
        let limit = clamp_limit(req.limit);
        let reader = open_reader(open()?)?;
        let native = req.extent.map(|e| self.reproject.extent_from_lonlat(&e));
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
        let late = native.is_some() && indexed.is_none();
        let matched = iter.features_count().map(|n| n as u64).or(self.info.count).unwrap_or(0);
        let mut sample = if late || matched == 0 { Sampler::new(limit) } else { Sampler::with_total(limit, matched) };
        let mut out = Batch::default();
        let mut n = 0u64;
        while let Some(f) = iter.next().map_err(err)? {
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
            let mut props = Props { fields: &self.info.fields, slots: &self.slots, values: vec![Value::Null; self.info.fields.len()] };
            if f.process_properties(&mut props).is_err() {
                out.skipped += 1;
                continue;
            }
            sample.keep(Feature { id, geometry: Some(geometry), attrs: props.values });
        }
        out.matched = if late || matched == 0 { sample.seen() } else { matched };
        out.complete = sample.complete();
        out.features = sample.items;
        Ok(out)
    }

    /// Visits the features a complete [`FgbSource::read_with`] of `extent` gives, in the same
    /// order and with the same ids, a chunk at a time. An extent read through the index holds
    /// the feature numbers it finds (8 bytes each) for the ids, as a read does.
    fn visit_with<R: Read + Seek>(
        &self,
        open: &impl Fn() -> Result<R>,
        extent: Option<Extent>,
        chunks: &mut super::chunk_memory::Chunker<'_>,
    ) -> Result<Batch> {
        let req = Request { extent, limit: usize::MAX };
        let reader = open_reader(open()?)?;
        let native = extent.map(|e| self.reproject.extent_from_lonlat(&e));
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
        while let Some(f) = iter.next().map_err(err)? {
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
            let mut props = Props { fields: &self.info.fields, slots: &self.slots, values: vec![Value::Null; self.info.fields.len()] };
            if f.process_properties(&mut props).is_err() {
                out.skipped += 1;
                continue;
            }
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
    /// takes, fetched together before any is read; the others are passed over unread.
    fn read_remote(&self, f: &Arc<Remote>, req: &Request) -> Result<Batch> {
        let limit = clamp_limit(req.limit);
        let native = match req.extent {
            Some(e) => self.reproject.extent_from_lonlat(&e),
            None => Extent::new(f64::MIN, f64::MIN, f64::MAX, f64::MAX),
        };
        let (index, features) = self.sections(f)?;
        let hits = self.remote_search(f, index, &native)?;
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
        let mut step = usize::try_from(stride(n as u64, limit)).unwrap_or(usize::MAX).max(1);
        // Runs short enough for the sample to be spread over many places.
        let run = (limit / 256).clamp(1, RUN);
        let taken = |k: usize, step: usize| step == 1 || (k / run).is_multiple_of(step);
        loop {
            let bytes: u64 = (0..n).filter(|&k| taken(k, step)).map(size).sum();
            if bytes <= FEATURE_BUDGET || step >= n.max(1) {
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
        let mut k = 0usize;
        while let Some(feat) = iter.next().map_err(err)? {
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
            if !in_request(&geometry, req) {
                continue;
            }
            let mut props = Props { fields: &self.info.fields, slots: &self.slots, values: vec![Value::Null; self.info.fields.len()] };
            if feat.process_properties(&mut props).is_err() {
                out.skipped += 1;
                continue;
            }
            out.features.push(Feature { id, geometry: Some(geometry), attrs: props.values });
            if out.features.len() >= limit {
                break;
            }
        }
        out.matched = (n as u64).saturating_mul(hits.thin);
        out.complete = step == 1 && hits.thin == 1;
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

/// Collects a feature's properties into values in field order.
struct Props<'a> {
    fields: &'a [Field],
    /// The field of each header column (see [`FgbSource::slots`]).
    slots: &'a [Option<usize>],
    values: Vec<Value>,
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
pub(crate) fn write_features(
    name: &str,
    geometry_type: GeometryType,
    fields: &[Field],
    crs: Crs,
    out: &mut dyn std::io::Write,
    feed: &mut dyn FnMut(&mut dyn FnMut(&[Feature]) -> Result<()>) -> Result<()>,
) -> Result<usize> {
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
        ..Default::default()
    };
    let mut w = FgbWriter::create_with_options(name, ty, options).map_err(err)?;
    for f in fields {
        let t = match f.ty {
            FieldType::Integer => ColumnType::Long,
            FieldType::Double => ColumnType::Double,
            FieldType::Boolean => ColumnType::Bool,
            FieldType::Text => ColumnType::String,
        };
        w.add_column(&f.name, t, |_, c| c.nullable = true);
    }
    let mut left_out = 0;
    feed(&mut |features| {
        for feat in features {
            crate::progress::step(1)?;
            let Some(g) = &feat.geometry else {
                left_out += 1;
                continue;
            };
            let mut failed = None;
            w.add_feature_geom(Wkb(wkb::encode(g)), |out| {
                use flatgeobuf::geozero::PropertyProcessor;
                for (i, (f, v)) in fields.iter().zip(&feat.attrs).enumerate() {
                    let r = match v {
                        Value::Null => continue,
                        Value::Int(x) => out.property(i, &f.name, &ColumnValue::Long(*x)),
                        Value::Float(x) => out.property(i, &f.name, &ColumnValue::Double(*x)),
                        Value::Bool(x) => out.property(i, &f.name, &ColumnValue::Bool(*x)),
                        Value::Text(x) => out.property(i, &f.name, &ColumnValue::String(x)),
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
    })?;
    w.write(out).map_err(err)?;
    Ok(left_out)
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
}
