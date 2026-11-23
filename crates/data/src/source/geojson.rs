//! Large GeoJSON FeatureCollections read in place, a view at a time.
//!
//! Opening reads the file once, a feature at a time, keeping each feature's place in the file and
//! bounding box (28 bytes a feature) and working out the schema; a read picks the features whose
//! boxes meet the request and parses just those from the file again. A file up to
//! [`io::BYTES_FLOOR`](crate::io::BYTES_FLOOR) is read into memory for that one pass (the quickest
//! way); a larger one is read through a buffer, so it's never held whole. Smaller files are loaded
//! whole by `io::read_path`.

use std::fs::{File, Metadata};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::{Batch, FeatureSource, Reproject, Request, Sampler, SourceInfo, clamp_limit, stride};
use crate::io::{
    FeatureSink, Schema, declared_crs, geojson_attrs, geojson_geometry, geojson_has_z, geometry_to_lonlat, latitude_past_poles, lonlat_box,
    swapped_latitude_note,
};
use crate::{DataError, Feature, GeometryType, Result};
use rustgis_geom::Extent;
use serde_json::Value as Json;

fn bad(msg: impl std::fmt::Display) -> DataError {
    DataError::Format(format!("GeoJSON: {msg}"))
}

/// GeoJSON files larger than this are read in place (smaller ones load whole faster).
pub const IN_PLACE_ABOVE: u64 = 32 << 20;

/// Where a feature's text is in the file.
#[derive(Clone, Copy)]
struct Place {
    offset: u64,
    len: u32,
}

/// Metadata of the opened file, cheap to compare without reading the whole source again.
#[derive(PartialEq, Eq)]
struct FileVersion {
    len: u64,
    modified: Option<std::time::SystemTime>,
    // On Unix, also catch replacements and edits that preserve the modification time.
    #[cfg(unix)]
    identity_and_change: (u64, u64, i64, i64),
}

impl FileVersion {
    fn of(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity_and_change: (metadata.dev(), metadata.ino(), metadata.ctime(), metadata.ctime_nsec()),
        }
    }

    /// Checks the same handle used for the scan or visit, including changes during the read.
    fn check(&self, file: &File) -> Result<()> {
        if *self != Self::of(&file.metadata()?) {
            return Err(bad("the file changed on disk since it was opened. Reopen the layer and try again"));
        }
        Ok(())
    }
}

/// A GeoJSON FeatureCollection read in place.
pub struct GeoJsonSource {
    info: SourceInfo,
    path: PathBuf,
    version: FileVersion,
    places: Vec<Place>,
    /// Each feature's box in the file's own coordinates, rounded outward (NaN for a feature
    /// without geometry).
    boxes: Vec<[f32; 4]>,
    /// The coordinate system the file declares (its `crs`), converting what's read to
    /// longitude/latitude.
    reproject: Reproject,
    /// The features (by place) the declared system can't convert: in no read, and not counted.
    /// Sorted.
    failed: Vec<u32>,
    /// Features left out when it was opened for geometry that couldn't be read.
    unreadable: usize,
}

/// The features of a file as it's scanned.
#[derive(Default)]
struct Scan {
    schema: Schema,
    places: Vec<Place>,
    boxes: Vec<[f32; 4]>,
    extent: Extent,
    /// Points, lines and polygons seen.
    kinds: [usize; 3],
    skipped: usize,
    /// Features, as the file gives them, with a latitude past the poles.
    swapped: usize,
    /// The places of features whose heights reading drops.
    zm: Vec<u32>,
    /// Features [`paced`](Scan::pace) so far, and the byte the progress was counted to.
    paced: usize,
    paced_to: u64,
    /// Cancel was pressed: the scan stops.
    stopped: bool,
}

/// Features between looks at Cancel (and progress counts) while a file is scanned or converted.
const PACE: usize = 4096;

impl Scan {
    /// Says the scan has come `at` bytes into the file, and looks at Cancel: at the first feature
    /// and every [`PACE`] after it (#2446).
    fn pace(&mut self, at: u64) -> Result<()> {
        let n = self.paced;
        self.paced += 1;
        if !n.is_multiple_of(PACE) {
            return Ok(());
        }
        let done = at.saturating_sub(self.paced_to);
        self.paced_to = self.paced_to.max(at);
        crate::progress::step(usize::try_from(done).unwrap_or(usize::MAX))
    }

    /// One element of `"features"`, read from `len` bytes at `offset` (with the comma and
    /// whitespace before it).
    fn feature(&mut self, f: &Json, offset: u64, len: u64) {
        // Other values than objects are kept as features without geometry or values, as when
        // read whole, so features have the same ids both ways.
        // Every feature's properties count in the schema, those with unreadable geometry too.
        if let Some(props) = f.get("properties").and_then(Json::as_object) {
            for (k, v) in props {
                self.schema.see(k, v);
            }
        }
        let Ok(geom) = geojson_geometry(f) else {
            self.skipped += 1;
            return;
        };
        let Ok(len) = u32::try_from(len) else {
            self.skipped += 1;
            return;
        };
        let b = match &geom {
            Some(g) => {
                // Only a geometry kept counts.
                if geojson_has_z(f) {
                    self.zm.push(u32::try_from(self.places.len()).unwrap_or(u32::MAX));
                }
                let e = g.extent();
                self.swapped += usize::from(latitude_past_poles(g));
                self.extent = self.extent.union(&e);
                if let Some(slot) = self.kinds.get_mut(kind_slot(g.kind())) {
                    *slot += 1;
                }
                outward(&e)
            }
            None => [f32::NAN; 4],
        };
        self.places.push(Place { offset, len });
        self.boxes.push(b);
    }
}

/// Where in [`Scan::kinds`] a kind of geometry is counted.
fn kind_slot(kind: GeometryType) -> usize {
    match kind {
        GeometryType::Point => 0,
        GeometryType::Polyline => 1,
        _ => 2,
    }
}

/// An extent as `f32`s, rounded outward (NaN when it isn't finite, so no request meets it).
fn outward(e: &Extent) -> [f32; 4] {
    if e.is_empty() || ![e.min_x, e.min_y, e.max_x, e.max_y].iter().all(|v| v.is_finite()) {
        return [f32::NAN; 4];
    }
    let down = |v: f64| {
        let f = v as f32;
        if f64::from(f) > v { f.next_down() } else { f }
    };
    let up = |v: f64| {
        let f = v as f32;
        if f64::from(f) < v { f.next_up() } else { f }
    };
    [down(e.min_x), down(e.min_y), up(e.max_x), up(e.max_y)]
}

impl GeoJsonSource {
    /// Opens a GeoJSON FeatureCollection. Other GeoJSON documents (a single feature, a bare
    /// geometry) and TopoJSON aren't read in place: they fail here, to be read whole.
    pub fn open_path(path: &Path, name: &str) -> Result<GeoJsonSource> {
        Self::open_with(path, name, crate::io::BYTES_FLOOR)
    }

    /// [`open_path`](Self::open_path), reading a file of at most `in_memory` bytes into memory to
    /// scan it, and a larger one through a buffer.
    pub(crate) fn open_with(path: &Path, name: &str, in_memory: u64) -> Result<GeoJsonSource> {
        use serde::de::DeserializeSeed;
        let mut file = crate::provenance::open(path)?;
        let version = FileVersion::of(&file.metadata()?);
        let mut scan = Scan::default();
        // How far the scan has come, in bytes of the file; Cancel stops it, said as such rather than
        // as the JSON error it ends with.
        crate::progress::begin(usize::try_from(version.len).unwrap_or(usize::MAX));
        let failed = |e: serde_json::Error| if crate::progress::check().is_err() { DataError::Cancelled } else { bad(e) };
        let rest = if version.len <= in_memory {
            let mut text = String::new();
            file.read_to_string(&mut text)?;
            crate::provenance::buffer(path, &file, text.as_bytes());
            // Its objects and vertices would all become a JSON tree, only to be refused below.
            if crate::cityjson::looks_like_cityjson(&text) {
                return Err(DataError::Unsupported("only GeoJSON FeatureCollections are read in place".into()));
            }
            crate::io::stream_features(&text, &mut InText { scan: &mut scan, start: text.as_ptr() as usize }).map_err(failed)?
        } else {
            let read = std::rc::Rc::new(std::cell::Cell::new(0u64));
            let input = Counting { inner: BufReader::with_capacity(BUFFER, &mut file), read: read.clone() };
            let mut de = serde_json::Deserializer::from_reader(input);
            let rest = Top { scan: &mut scan, read }.deserialize(&mut de).map_err(failed)?;
            // Only whitespace may follow, as when read whole.
            de.end().map_err(failed)?;
            rest
        };
        if rest.get("type").and_then(Json::as_str) != Some("FeatureCollection") {
            return Err(DataError::Unsupported("only GeoJSON FeatureCollections are read in place".into()));
        }
        let mut notes = Vec::new();
        if scan.skipped > 0 {
            notes.push(format!("{} features had unreadable geometry", scan.skipped));
        }
        let reproject = Reproject::known(declared_crs(&rest, &mut notes)?);
        // The declaration may come after the features, so only now can each be tried: those the
        // declared system can't convert are found here (and left out of every read, the count,
        // the extent and the geometry type), as they are when the file is read whole.
        let mut failed = Vec::new();
        let mut extent = scan.extent;
        if let Some(t) = reproject.transformer() {
            file.rewind()?;
            let pass = convert_pass(&mut file, &scan.places, t)?;
            extent = pass.extent;
            for (kept, lost) in scan.kinds.iter_mut().zip(pass.dropped) {
                *kept = kept.saturating_sub(lost);
            }
            if !pass.failed.is_empty() {
                notes.push(format!(
                    "{} features couldn't be converted from {} to longitude/latitude and were skipped",
                    pass.failed.len(),
                    reproject.label()
                ));
            }
            failed = pass.failed;
        } else {
            // Only coordinates read as they are can be longitude/latitude with the two swapped;
            // a declared system's features were converted above, and those that don't fit left out.
            notes.extend(swapped_latitude_note(scan.swapped));
        }
        // The kind most features have, as a layer read whole takes.
        let geometry_type = match scan.kinds.iter().enumerate().max_by_key(|(_, n)| **n) {
            Some((_, 0)) | None => GeometryType::None,
            Some((0, _)) => GeometryType::Point,
            Some((1, _)) => GeometryType::Polyline,
            Some(_) => GeometryType::Polygon,
        };
        let mut info = SourceInfo {
            name: name.to_string(),
            format: "GeoJSON",
            geometry_type,
            fields: scan.schema.fields(),
            extent,
            count: Some((scan.places.len() - failed.len()) as u64),
            crs: reproject.label(),
            indexed: false,
            notes: notes.into(),
            zm_dropped: false,
        };
        // Features the declared system can't convert are left out, and so are their heights.
        let zm = scan.zm.iter().filter(|i| failed.binary_search(i).is_err()).count() as u64;
        if zm > 0 {
            info.drop_zm(Some(zm));
        }
        let unreadable = scan.skipped;
        version.check(&file)?;
        Ok(GeoJsonSource { info, path: path.to_path_buf(), version, places: scan.places, boxes: scan.boxes, reproject, failed, unreadable })
    }

    /// The features a request matches, in file order: every one without an extent; otherwise
    /// those whose boxes meet it (features without geometry never do). The extent is in the
    /// file's own coordinates.
    fn matches(&self, extent: Option<Extent>) -> impl Iterator<Item = u32> + '_ {
        self.candidates(extent).filter_map(|(i, matches)| matches.then_some(i))
    }

    /// Walks every index entry, exposing rejected entries too so a full visit can count
    /// progress and check Cancel even across a long stretch outside the requested extent.
    fn candidates(&self, extent: Option<Extent>) -> impl Iterator<Item = (u32, bool)> + '_ {
        // Rounded to the nearest f32: never past a box rounded outward that meets the extent.
        let e = extent.map(|e| (e.is_empty(), [e.min_x as f32, e.min_y as f32, e.max_x as f32, e.max_y as f32]));
        self.boxes.iter().enumerate().map(move |(i, b)| {
            let in_box = match e {
                None => true,
                Some((true, _)) => false,
                Some((false, [x0, y0, x1, y1])) => b[0] <= x1 && b[2] >= x0 && b[1] <= y1 && b[3] >= y0,
            };
            (i as u32, in_box && self.failed.binary_search(&(i as u32)).is_err())
        })
    }
}

/// Bytes read ahead from the file: features near each other in a read share a fill.
const BUFFER: usize = 1 << 20;

impl FeatureSource for GeoJsonSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    /// Its own system, in which it gives its features as the file holds them.
    fn coord_crs(&self) -> crate::raster::Crs {
        self.reproject.crs()
    }

    fn lost_to_crs(&self) -> bool {
        !self.failed.is_empty()
    }

    fn skipped_at_open(&self) -> usize {
        self.unreadable + self.failed.len()
    }

    fn visits_extent_chunks(&self) -> bool {
        true
    }

    /// Visits the matching file records in order and retains only one batch of decoded rows.
    /// A changed file invalidates the saved bounds and offsets, even for records the bounds
    /// would skip. A successful visit uses the same file version as the index.
    fn visit_chunks(&self, extent: Option<Extent>, bytes: usize, visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        let req = Request { extent, limit: usize::MAX };
        let native = extent.and_then(|e| self.reproject.extent_from_lonlat(&e));
        let mut chunks = super::chunk_memory::Chunker::new(bytes, visit)?;
        let mut f = BufReader::with_capacity(BUFFER, crate::provenance::open(&self.path)?);
        self.version.check(f.get_ref())?;
        let mut pos = 0;
        let mut buf = Vec::new();
        let mut batch = Batch { complete: true, ..Batch::default() };
        crate::progress::begin(self.places.len());
        for (i, matches) in self.candidates(native) {
            crate::progress::step(1)?;
            if !matches {
                continue;
            }
            match self.read_feature((&mut f, &mut pos, &mut buf), i, &req, || true) {
                Ok(Some(feature)) => {
                    batch.matched += 1;
                    chunks.push(feature)?;
                }
                Err(()) => batch.skipped += 1,
                Ok(None) => {}
            }
        }
        chunks.flush()?;
        self.version.check(f.get_ref())?;
        Ok(batch)
    }

    /// Reads a bounded sample; when native bounds are refused, filters geometry before sampling,
    /// counting records and checking Cancel between them. A whole read (no extent, every match:
    /// Add Data loading the file) counts and looks at Cancel likewise; other reads look at Cancel
    /// every [`PACE`] features.
    fn read(&self, req: &Request) -> Result<Batch> {
        let limit = clamp_limit(req.limit);
        // Counted first, then sampled, unless the native box is refused: no list of every match.
        let extent = req.extent.and_then(|e| self.reproject.extent_from_lonlat(&e));
        // Preserve ordinary candidate counts; a refused view needs a geometry scan to count.
        if limit == 0 && req.extent.is_some() && extent.is_none() {
            return Err(super::unbounded_count());
        }
        let matched = self.matches(extent).count();
        // A refused native box needs geometry filtering before sampling, just as an unindexed source does.
        let late = req.extent.is_some() && extent.is_none();
        let step = if late { 1 } else { stride(matched as u64, limit).max(1) as usize };
        let mut sample = Sampler::new(limit);
        let mut f = BufReader::with_capacity(BUFFER, crate::provenance::open(&self.path)?);
        let mut pos = 0u64;
        let mut buf = Vec::new();
        let mut features = Vec::with_capacity(matched.div_ceil(step).min(limit));
        let mut skipped = 0;
        // A whole read counts its features as a refused box does; a view's sample only looks.
        let counted = late || (req.extent.is_none() && step == 1);
        if counted {
            crate::progress::begin(if late { matched } else { matched.min(limit) });
        }
        for (n, i) in self.matches(extent).step_by(step).take(if late { usize::MAX } else { limit }).enumerate() {
            if counted {
                crate::progress::step(1)?;
            } else if n.is_multiple_of(PACE) {
                crate::progress::check()?;
            }
            let feature = match self.read_feature((&mut f, &mut pos, &mut buf), i, req, || !late || sample.wants()) {
                Ok(Some(feature)) => feature,
                Err(()) => {
                    skipped += 1;
                    continue;
                }
                Ok(None) => continue,
            };
            if late {
                sample.keep(feature);
            } else {
                features.push(feature);
            }
        }
        let (matched, complete) = if late {
            features = std::mem::take(&mut sample.items);
            (sample.seen(), sample.complete())
        } else {
            (matched as u64, step == 1)
        };
        Ok(Batch {
            features,
            matched,
            complete,
            skipped,
            unread_dates: Vec::new(),
            rounded_dates: Vec::new(),
            null_text: Vec::new(),
            notes: Default::default(),
            not_utf8: Vec::new(),
        })
    }
}

impl GeoJsonSource {
    /// Decodes one indexed record. A missing or malformed record is unreadable, while a
    /// record past the request's bounds (including rounding in the index) is excluded.
    fn read_feature(
        &self,
        (f, pos, buf): (&mut BufReader<File>, &mut u64, &mut Vec<u8>),
        i: u32,
        req: &Request,
        wanted: impl FnOnce() -> bool,
    ) -> std::result::Result<Option<Feature>, ()> {
        let place = self.places.get(i as usize).ok_or(())?;
        let json = read_place(f, pos, buf, place).ok_or(())?;
        let geometry = geojson_geometry(&json).map_err(|_| ())?;
        // Kept as the file holds it, in its own system; its box in longitude/latitude answers the
        // request.
        let lonlat = match (self.reproject.transformer(), &geometry) {
            (Some(t), Some(g)) => Some(lonlat_box(g, t).ok_or(())?),
            (_, g) => g.as_ref().map(crate::Geometry::extent),
        };
        // One without geometry doesn't match an extent.
        let outside = match (&lonlat, &req.extent) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(e), Some(r)) => !e.intersects(r),
        };
        if outside || !wanted() {
            return Ok(None);
        }
        Ok(Some(Feature { id: u64::from(i) + 1, geometry, attrs: geojson_attrs(&json, &self.info.fields) }))
    }
}

/// The feature at `place`, read through `f` (at `pos`, which is kept up to date): `None` when the
/// file has changed since it was opened.
fn read_place(f: &mut BufReader<impl Read + Seek>, pos: &mut u64, buf: &mut Vec<u8>, place: &Place) -> Option<Json> {
    // Ahead and near: through the buffer; otherwise a seek.
    let read = (|| -> std::io::Result<()> {
        match i64::try_from(place.offset.wrapping_sub(*pos)) {
            Ok(0) => {}
            Ok(d) if place.offset > *pos && d < BUFFER as i64 => f.seek_relative(d)?,
            _ => {
                f.seek(SeekFrom::Start(place.offset))?;
            }
        }
        buf.resize(place.len as usize, 0);
        f.read_exact(buf)
    })();
    if read.is_err() {
        // Where the file is now is unknown.
        *pos = u64::MAX;
        return None;
    }
    *pos = place.offset + u64::from(place.len);
    // Past the comma and whitespace before the feature.
    let text = buf.iter().position(|b| !b.is_ascii_whitespace() && *b != b',').and_then(|at| buf.get(at..)).unwrap_or_default();
    serde_json::from_slice::<Json>(text).ok()
}

/// What a pass over a file's features converts to longitude/latitude finds.
struct Pass {
    /// Places of the features the transformer can't convert (see [`geometry_to_lonlat`]), sorted.
    failed: Vec<u32>,
    /// Points, lines and polygons among them (as [`Scan::kinds`]).
    dropped: [usize; 3],
    /// The extent, in longitude/latitude, of those it can.
    extent: Extent,
}

/// A second pass over the file, made only for a declared coordinate system.
fn convert_pass(file: &mut File, places: &[Place], t: &crate::raster::Transformer) -> Result<Pass> {
    let mut f = BufReader::with_capacity(BUFFER, file);
    let (mut pos, mut buf) = (0u64, Vec::new());
    let mut pass = Pass { failed: Vec::new(), dropped: [0; 3], extent: Extent::EMPTY };
    // Counted as a step of its own, a feature at a time every so many.
    crate::progress::begin(places.len());
    for (i, place) in places.iter().enumerate() {
        if i.is_multiple_of(PACE) {
            crate::progress::step(if i == 0 { 0 } else { PACE })?;
        }
        // A feature that can't be read now is left to the reads, which skip it.
        if let Some(json) = read_place(&mut f, &mut pos, &mut buf, place)
            && let Ok(Some(g)) = geojson_geometry(&json)
        {
            match geometry_to_lonlat(&g, t) {
                Some(g) => pass.extent = pass.extent.union(&g.extent()),
                None => {
                    pass.failed.push(i as u32);
                    if let Some(slot) = pass.dropped.get_mut(kind_slot(g.kind())) {
                        *slot += 1;
                    }
                }
            }
        }
    }
    Ok(pass)
}

/// A reader counting the bytes read through it, for features' places in the file. The JSON reader
/// takes a byte at a time and looks at most one ahead, which it doesn't after a feature's last
/// byte, so the count after one is where it ends.
struct Counting<R> {
    inner: R,
    read: std::rc::Rc<std::cell::Cell<u64>>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.set(self.read.get() + n as u64);
        Ok(n)
    }
}

/// The top level of the file: its members, but for `"features"`, which are scanned as they're
/// read.
struct Top<'a> {
    scan: &'a mut Scan,
    read: std::rc::Rc<std::cell::Cell<u64>>,
}

impl<'de> serde::de::DeserializeSeed<'de> for Top<'_> {
    type Value = serde_json::Map<String, Json>;

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Top<'_> {
    type Value = serde_json::Map<String, Json>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a GeoJSON FeatureCollection")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> std::result::Result<Self::Value, A::Error> {
        let mut rest = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if key == "features" {
                // The last of repeated members wins, as when read whole.
                *self.scan = Scan::default();
                map.next_value_seed(Elements { scan: &mut *self.scan, read: self.read.clone() })?;
            } else if key == "type" || key == "crs" {
                rest.insert(key, map.next_value()?);
            } else {
                // All that's looked at above: another document's members, such as a CityJSON
                // model's objects and vertices, mustn't become a JSON tree only to be refused.
                map.next_value::<serde::de::IgnoredAny>()?;
            }
        }
        Ok(rest)
    }
}

/// The `"features"` array, a feature at a time.
struct Elements<'a> {
    scan: &'a mut Scan,
    read: std::rc::Rc<std::cell::Cell<u64>>,
}

impl<'de> serde::de::DeserializeSeed<'de> for Elements<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Elements<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an array of features")
    }

    /// Scans each feature with its place in the file, ending with an error once Cancel is pressed.
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
        loop {
            let start = self.read.get();
            let Some(f) = seq.next_element::<Json>()? else { break };
            self.scan.feature(&f, start, self.read.get().saturating_sub(start));
            self.scan.pace(self.read.get()).map_err(|_| serde::de::Error::custom("cancelled"))?;
        }
        Ok(())
    }
}

/// The scan of a file read into memory: each feature's place is where its text is in the file's.
struct InText<'a> {
    scan: &'a mut Scan,
    /// Where the file's text starts in memory.
    start: usize,
}

impl FeatureSink for InText<'_> {
    fn restart(&mut self) {
        *self.scan = Scan::default();
    }

    /// Scans one feature, its place where its text is in the file's; Cancel pressed stops the read.
    fn feature(&mut self, raw: &str) {
        let offset = (raw.as_ptr() as usize).saturating_sub(self.start) as u64;
        match serde_json::from_str::<Json>(raw) {
            Ok(f) => self.scan.feature(&f, offset, raw.len() as u64),
            // Valid JSON, but with a number that doesn't fit.
            Err(_) => self.scan.skipped += 1,
        }
        if self.scan.pace(offset + raw.len() as u64).is_err() {
            self.scan.stopped = true;
        }
    }

    fn stopped(&self) -> bool {
        self.scan.stopped
    }
}
