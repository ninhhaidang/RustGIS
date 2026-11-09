//! Data providers: vector sources read in place rather than loaded whole.
//!
//! A [`FeatureSource`] answers "the features in this extent, at most this many" from the file's own
//! spatial index (the GeoPackage R-tree, the FlatGeobuf packed Hilbert R-tree, GeoParquet row-group
//! bounding boxes), so a layer can draw a dataset far larger than memory a view at a time.
//! Coordinates come out as WGS 84 longitude/latitude whatever the source's coordinate system.
//!
//! Sources are hostile input: malformed rows are skipped and counted, never a panic.

pub mod credentials;
#[cfg(not(target_arch = "wasm32"))]
pub mod duckdb;
pub mod edits;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod external;
pub mod fgb;
#[cfg(not(target_arch = "wasm32"))]
pub mod gdal;
#[cfg(not(target_arch = "wasm32"))]
pub mod geojson;
#[cfg(not(target_arch = "wasm32"))]
pub mod gpkg;
mod gpkg_blob;
pub mod gpkg_mem;
pub mod lidar;
pub mod memory;
pub mod msconn;
pub mod myconn;
#[cfg(not(target_arch = "wasm32"))]
pub mod mysql;
pub mod parquet;
pub mod pgconn;
#[cfg(not(target_arch = "wasm32"))]
pub mod postgis;
pub mod remote;
#[cfg(not(target_arch = "wasm32"))]
pub mod shp;
#[cfg(not(target_arch = "wasm32"))]
pub mod sqlserver;

use crate::raster::{Crs, Transformer};
use crate::{DataError, Dataset, Feature, Field, Geometry, GeometryType, ReadReport, Result};
use rustgis_geom::{Coord, Extent};
use std::path::Path;

/// One kind of geometry, for a layer that mixes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Family {
    Points,
    Lines,
    Polygons,
}

impl Family {
    pub const ALL: [Family; 3] = [Family::Points, Family::Lines, Family::Polygons];

    pub fn label(self) -> &'static str {
        match self {
            Family::Points => "points",
            Family::Lines => "lines",
            Family::Polygons => "polygons",
        }
    }

    /// An OGR SQL filter on the geometry type. Curves and surfaces count as lines and polygons:
    /// an Esri multipatch, read as a TIN, a polyhedral surface or (from a Shapefile) a geometry
    /// collection of them, is polygons. A collection of anything else fails the conversion to
    /// polygons and is left out, as before.
    pub fn filter(self) -> &'static str {
        match self {
            Family::Points => "OGR_GEOMETRY IN ('POINT', 'MULTIPOINT')",
            Family::Lines => "OGR_GEOMETRY IN ('LINESTRING', 'MULTILINESTRING', 'CIRCULARSTRING', 'COMPOUNDCURVE', 'MULTICURVE')",
            Family::Polygons => {
                "OGR_GEOMETRY IN ('POLYGON', 'MULTIPOLYGON', 'CURVEPOLYGON', 'MULTISURFACE', 'TRIANGLE', 'TIN', 'POLYHEDRALSURFACE', 'GEOMETRYCOLLECTION')"
            }
        }
    }

    /// The OGR geometry type its features are converted to: curves become straight segments, and
    /// a surface's faces the parts of a multipolygon.
    pub fn ogr_type(self) -> &'static str {
        match self {
            Family::Points => "MULTIPOINT",
            Family::Lines => "MULTILINESTRING",
            Family::Polygons => "MULTIPOLYGON",
        }
    }

    /// The family of an OGR layer geometry type as `ogrinfo -json` spells it (`MultiPolygonZ`,
    /// `TIN`, `CompoundCurve`); `None` for one that mixes them (`GeometryZ`).
    pub fn of_ogr_type(ty: &str) -> Option<Family> {
        let ty = ty.to_ascii_lowercase();
        if ty.contains("point") {
            Some(Family::Points)
        } else if ["polygon", "surface", "tin", "triangle"].iter().any(|k| ty.contains(k)) {
            Some(Family::Polygons)
        } else if ["linestring", "circularstring", "curve"].iter().any(|k| ty.contains(k)) {
            Some(Family::Lines)
        } else {
            None
        }
    }
}

/// File types read through a provider.
pub const EXTENSIONS: &[&str] = &["gpkg", "parquet", "geoparquet", "fgb", "las", "laz"];

/// The most features one read returns, whatever the request asks for. A whole read of a source
/// read a chunk at a time is bounded by [`MAX_READ_BYTES`] instead.
pub const MAX_LIMIT: usize = if cfg!(target_arch = "wasm32") { 500_000 } else { 2_000_000 };

/// The most memory a whole read may hold of a source that can be read a chunk at a time
/// ([`FeatureSource::visits_chunks`]): its features' storage, as [`chunk_memory::features_bytes`]
/// counts it.
pub const MAX_READ_BYTES: u64 = if cfg!(target_arch = "wasm32") { 1 << 30 } else { 4 << 30 };

/// The chunk size a whole read asks for; each chunk is moved into the dataset as it comes.
const READ_CHUNK_BYTES: usize = 8 << 20;

/// Whether a file name is read through a provider.
pub fn is_source_name(name: &str) -> bool {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    EXTENSIONS.contains(&ext.as_str())
}

/// Whether a folder is an Esri File Geodatabase (`roads.gdb`): added as data, not browsed.
pub fn is_gdb_name(name: &str) -> bool {
    ext(Path::new(name.trim_end_matches(['/', '\\']))) == "gdb"
}

/// The File Geodatabase one of its own files belongs to (`roads.gdb/a00000001.gdbtable`, or the
/// `gdb` file every one holds): file dialogs pick files, not the folder. Another file kept in the
/// folder is left as it is.
pub fn gdb_of(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    let name = path.file_name()?.to_str()?;
    (is_gdb_part(name) && is_gdb_name(&parent.to_string_lossy())).then_some(parent)
}

/// Whether a file name is one a File Geodatabase is made of: the `gdb` and `timestamps` files, and
/// its tables and their indexes, `a` and eight hex digits (`a00000004.gdbtablx`,
/// `a00000004.CatItemsByType.atx`).
fn is_gdb_part(name: &str) -> bool {
    if matches!(name, "gdb" | "timestamps") {
        return true;
    }
    let (Some((stem, _)), Some((_, ext))) = (name.split_once('.'), name.rsplit_once('.')) else { return false };
    stem.len() == 9
        && stem.starts_with('a')
        && stem.bytes().skip(1).all(|b| b.is_ascii_hexdigit())
        && matches!(ext.to_ascii_lowercase().as_str(), "gdbtable" | "gdbtablx" | "gdbindexes" | "atx" | "spx" | "freelist" | "horizon")
}

/// What a source holds.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceInfo {
    /// Layer name (the table, or the file name).
    pub name: String,
    /// "GeoPackage", "GeoParquet" or "FlatGeobuf".
    pub format: &'static str,
    pub geometry_type: GeometryType,
    pub fields: Vec<Field>,
    /// Longitude/latitude extent, empty when the file doesn't say.
    pub extent: Extent,
    /// Number of features, when known without reading them.
    pub count: Option<u64>,
    /// The source's coordinate system, as `EPSG:…`.
    pub crs: String,
    /// Whether extent queries use a spatial index rather than a scan.
    pub indexed: bool,
    /// Things worth telling whoever adds the source (an unknown coordinate system read as
    /// longitude/latitude, tables left out).
    pub notes: Vec<String>,
    /// The source has Z or M values, which reading drops (see [`Dataset::zm_dropped`]).
    pub zm_dropped: bool,
}

impl SourceInfo {
    /// Marks the source as having Z or M values, which reading drops, and notes it once: in
    /// `features` features when known (every one, when the source declares them). A count of 0 (an
    /// empty source) drops nothing and says nothing.
    pub(crate) fn drop_zm(&mut self, features: Option<u64>) {
        if !self.zm_dropped && features != Some(0) {
            self.zm_dropped = true;
            self.notes.push(crate::zm_note(features));
        }
    }

    /// Notes what happened to some columns in one note, counting them and naming the first 12
    /// with their kind ("price (decimal)"): `what` says it for one column, then for several.
    /// Nothing when there are none.
    pub(crate) fn note_columns(&mut self, cols: &[(String, String)], what: [&str; 2]) {
        const NAMED: usize = 12;
        let [one, several] = what;
        let n = cols.len();
        let what = match n {
            0 => return,
            1 => one,
            _ => several,
        };
        let named: Vec<String> = cols.iter().take(NAMED).map(|(name, kind)| format!("{name} ({kind})")).collect();
        let more = if n > NAMED { format!(" and {} more", n - NAMED) } else { String::new() };
        self.notes.push(format!("{n} column{} {what}: {}{more}", if n == 1 { "" } else { "s" }, named.join(", ")));
    }

    /// The source's coordinate system, when it isn't longitude/latitude: one known by its EPSG
    /// code or a custom one without. `None` also when `crs` isn't one RustGIS can parse.
    pub fn coordinate_system(&self) -> Option<Crs> {
        Crs::parse(&self.crs).ok().filter(|c| *c != Crs::LonLat)
    }
}

/// A read: the features intersecting `extent` (longitude/latitude; `None` = all), at most `limit`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Request {
    pub extent: Option<Extent>,
    pub limit: usize,
}

impl Request {
    /// Every feature, up to [`MAX_LIMIT`].
    pub fn all() -> Self {
        Request { extent: None, limit: MAX_LIMIT }
    }
}

/// The answer to a [`Request`].
#[derive(Clone, Debug, Default)]
pub struct Batch {
    pub features: Vec<Feature>,
    /// How many features the request matched, before the limit (an estimate where the index only
    /// narrows to blocks of rows).
    pub matched: u64,
    /// Whether every matching feature is in `features`. When the limit is hit, providers return an
    /// evenly spread sample of the matches rather than the first `limit`, so the map stays covered.
    pub complete: bool,
    /// Rows that couldn't be read.
    pub skipped: usize,
}

/// A vector data source read in place.
pub trait FeatureSource: Send + Sync {
    /// Whether the map draws the source by complete traversal ([`FeatureSource::visit_chunks`])
    /// on every render rather than from a sampled view window. Implies
    /// [`FeatureSource::visits_chunks`].
    fn streams_chunks(&self) -> bool {
        false
    }
    /// Whether [`FeatureSource::visit_chunks`] is available, for a pass over the whole source
    /// (an export) that holds one chunk at a time. Unlike [`FeatureSource::streams_chunks`], it
    /// leaves drawing alone.
    fn visits_chunks(&self) -> bool {
        self.streams_chunks()
    }
    /// Visits every matching feature in source painter order: the features and ids a complete
    /// [`FeatureSource::read`] of the same extent gives, in the same order and with the same
    /// skipped count when there is no extent (an extent read through an index may order and skip
    /// differently). Release each chunk before the next callback. `bytes` targets owned feature
    /// storage; an indivisible larger row is emitted alone, and a provider may hold its own unit
    /// of the file (a GeoParquet row group) or an index's list of matches (FlatGeobuf, for an
    /// extent) besides. Callback errors stop traversal. Success returns an empty, complete
    /// summary batch. Its `matched` is the provider's count of matches: the features visited for
    /// GeoPackage, FlatGeobuf and GeoParquet, the records the index matched for Shapefile (which
    /// may include rows skipped or deleted).
    fn visit_chunks(&self, _extent: Option<Extent>, _bytes: usize, _visit: &mut dyn FnMut(Vec<Feature>) -> Result<()>) -> Result<Batch> {
        Err(DataError::Unsupported("a source that can't be read a chunk at a time".into()))
    }
    fn info(&self) -> &SourceInfo;
    /// Whether features were left out of every read because the coordinate system the source
    /// declares can't place them, so that defining its projection again can't bring them back.
    fn lost_to_crs(&self) -> bool {
        false
    }
    /// How many features the source left out when it was opened (and from every read), for a
    /// whole read's `ReadReport::skipped` to count with what reads skip.
    fn skipped_at_open(&self) -> usize {
        0
    }
    /// Reads the features a request asks for. A source read by its web address fails with
    /// [`remote::NEED`] until the bytes the read needs are fetched: run it with
    /// [`remote::drive`] (see [`FeatureSource::remote`]).
    fn read(&self, req: &Request) -> Result<Batch>;
    /// A disposable view window bounded by allocated row/geometry/attribute bytes. Providers
    /// can enforce this during decoding; the fallback probes a small uniform sample before
    /// sizing the actual read. Full materialization deliberately continues to use `read`.
    fn read_budgeted(&self, req: &Request, bytes: usize) -> Result<Batch> {
        let mut request = *req;
        request.limit = clamp_limit(req.limit).min(128);
        let mut batch = self.read(&request)?;
        if req.limit == 0 {
            batch.features.clear();
            batch.features.shrink_to_fit();
            batch.complete = batch.matched == 0;
            return Ok(batch);
        }
        let used = memory::features_bytes(&batch.features);
        if !batch.complete && used < bytes {
            // A small probe can hit only unreadable/deleted candidates. An incomplete
            // empty answer is not proof that the requested extent contains no features.
            let count = if batch.features.is_empty() {
                bytes / (std::mem::size_of::<Feature>() * 4)
            } else {
                batch.features.len().saturating_mul(bytes / 2) / used.max(1)
            };
            let next = clamp_limit(req.limit).min(count).max(1);
            if next > request.limit {
                request.limit = next;
                drop(batch);
                batch = self.read(&request)?;
            }
        }
        batch.features.shrink_to_fit();
        while memory::features_bytes(&batch.features) > bytes {
            if batch.features.len() <= 1 {
                return Err(DataError::Format("one feature exceeds the view memory budget; load it explicitly for full processing".into()));
            }
            memory::halve(&mut batch.features);
            batch.complete = false;
        }
        Ok(batch)
    }
    /// The file a source reads by its web address, a range at a time.
    fn remote(&self) -> Option<std::sync::Arc<remote::Remote>> {
        None
    }
}

/// Opens a file through its provider. `layer` names a table in a GeoPackage (default: the first).
pub fn open_path(path: &Path, layer: Option<&str>) -> Result<Box<dyn FeatureSource>> {
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("layer").to_string();
    match ext(path).as_str() {
        "parquet" | "geoparquet" => Ok(Box::new(parquet::ParquetSource::open_path(path, &name)?)),
        "fgb" => Ok(Box::new(fgb::FgbSource::open_path(path, &name)?)),
        "las" | "laz" => Ok(Box::new(lidar::LidarSource::open_path(path, &name)?)),
        #[cfg(not(target_arch = "wasm32"))]
        "gpkg" => Ok(Box::new(gpkg::GpkgSource::open(path, layer)?)),
        // Only large GeoJSON files are read in place (see `geojson`); `is_source_name` leaves
        // them out.
        #[cfg(not(target_arch = "wasm32"))]
        "geojson" | "json" => Ok(Box::new(geojson::GeoJsonSource::open_path(path, &name)?)),
        // Only large Shapefiles are read in place (see `shp`); `is_source_name` leaves them out.
        #[cfg(not(target_arch = "wasm32"))]
        "shp" => Ok(Box::new(shp::ShpSource::open_path(path, &name)?)),
        other => {
            let _ = layer;
            Err(DataError::Unsupported(other.to_string()))
        }
    }
}

/// Opens a file held in memory (a browser upload).
pub fn open_bytes(file_name: &str, bytes: Vec<u8>, layer: Option<&str>) -> Result<Box<dyn FeatureSource>> {
    open_shared(file_name, std::sync::Arc::new(bytes), layer)
}

/// File bytes shared without copying (a layer keeps them for reopening the source).
#[derive(Clone, Debug)]
pub struct Shared(pub std::sync::Arc<Vec<u8>>);

impl AsRef<[u8]> for Shared {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

/// Opens a file held in memory, sharing its bytes rather than copying them.
pub fn open_shared(file_name: &str, bytes: std::sync::Arc<Vec<u8>>, layer: Option<&str>) -> Result<Box<dyn FeatureSource>> {
    let path = Path::new(file_name);
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("layer").to_string();
    let bytes = Shared(bytes);
    match ext(path).as_str() {
        "parquet" | "geoparquet" => Ok(Box::new(parquet::ParquetSource::open_shared(bytes, &name)?)),
        "fgb" => Ok(Box::new(fgb::FgbSource::open_shared(bytes, &name)?)),
        "las" | "laz" => Ok(Box::new(lidar::LidarSource::open_bytes(bytes.as_ref(), &name)?)),
        // The desktop reads GeoPackages with SQLite (and its R-tree); the browser, which has no
        // SQLite, with the pure-Rust reader.
        #[cfg(not(target_arch = "wasm32"))]
        "gpkg" => Ok(Box::new(gpkg::GpkgSource::open_bytes(bytes.as_ref(), layer)?)),
        #[cfg(target_arch = "wasm32")]
        "gpkg" => Ok(Box::new(gpkg_mem::GpkgMemSource::open_shared(bytes, layer)?)),
        other => {
            let _ = layer;
            Err(DataError::Unsupported(other.to_string()))
        }
    }
}

/// Whether a web address names a file read in place by range: a FlatGeobuf or a GeoParquet.
/// Saved S3 connection and runtime service-login addresses are judged by the URL they wrap;
/// the original address remains the range reader's identity.
pub fn is_remote_name(url: &str) -> bool {
    let lower = crate::login_address::inner(crate::s3_address::inner(url)).to_ascii_lowercase();
    let path = lower.split(['?', '#']).next().unwrap_or(&lower);
    // `s3://` too: the app's fetchers check it and read it from its bucket's web address (signed
    // only for a connection that serves the bucket, see rustgis_tiles::s3).
    (lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("s3://"))
        && [".fgb", ".parquet", ".geoparquet"].iter().any(|e| path.ends_with(e))
}

/// A layer name from a web address: its file name without the extension.
fn url_name(url: &str) -> String {
    let url = crate::login_address::inner(crate::s3_address::inner(url));
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let file = path.rsplit('/').next().unwrap_or(path);
    let stem = file.rsplit_once('.').map(|(a, _)| a).unwrap_or(file);
    if stem.is_empty() { "layer".to_string() } else { stem.to_string() }
}

/// One attempt at opening a file by its web address from what is fetched of it so far: fails
/// with [`remote::NEED`] until its header (or footer) is in. See [`open_url`].
pub fn open_url_now(url: &str) -> Result<Box<dyn FeatureSource>> {
    open_file_now(remote::file(url))
}

/// [`open_url_now`] from `file`: a drive's reads note what they miss on the file it fetches for,
/// even after other files pushed it out of those kept.
fn open_file_now(file: std::sync::Arc<remote::Remote>) -> Result<Box<dyn FeatureSource>> {
    let url = crate::login_address::inner(crate::s3_address::inner(file.url()));
    let name = url_name(url);
    let lower = url.to_ascii_lowercase();
    let path = lower.split(['?', '#']).next().unwrap_or(&lower);
    if path.ends_with(".fgb") {
        Ok(Box::new(fgb::FgbSource::open_remote(file, &name)?))
    } else if path.ends_with(".parquet") || path.ends_with(".geoparquet") {
        Ok(Box::new(parquet::ParquetSource::open_remote(file, &name)?))
    } else {
        Err(DataError::Unsupported(format!("{url}: only FlatGeobuf and GeoParquet are read in place by address")))
    }
}

/// Opens a file by its web address, fetching what it needs (off the caller's thread).
pub fn open_url(url: &str, done: Box<dyn FnOnce(Result<Box<dyn FeatureSource>>) + Send>) {
    let file = remote::file(url);
    let f = file.clone();
    remote::drive(file, std::sync::Arc::new(move || open_file_now(f.clone())), done);
}

/// [`open_url`], waiting for it (the command line and tools).
#[cfg(not(target_arch = "wasm32"))]
pub fn open_url_blocking(url: &str) -> Result<Box<dyn FeatureSource>> {
    let file = remote::file(url);
    let f = file.clone();
    remote::drive_blocking(file, std::sync::Arc::new(move || open_file_now(f.clone())))
}

/// What a [`query_url`] read: the fields (the file's, then the nested columns asked for) and the
/// rows.
pub type Queried = (Vec<crate::Field>, Batch);

fn query_now(file: &std::sync::Arc<remote::Remote>, columns: &[String], req: &Request) -> Result<Queried> {
    let cols: Vec<&str> = columns.iter().map(String::as_str).collect();
    let src = parquet::ParquetSource::open_remote(file.clone(), &url_name(file.url()))?.with_columns(&cols).exact();
    let batch = src.read(req)?;
    Ok((src.info().fields.clone(), batch))
}

/// Reads the rows of a GeoParquet by its web address that a request matches, with nested
/// columns (`assets.data.href`) as fields too: a catalog's index read in place, fetching what it
/// needs (off the caller's thread).
pub fn query_url(url: &str, columns: Vec<String>, req: Request, done: Box<dyn FnOnce(Result<Queried>) + Send>) {
    let file = remote::file(url);
    let f = file.clone();
    remote::drive(file, std::sync::Arc::new(move || query_now(&f, &columns, &req)), done);
}

/// [`query_url`], waiting for it (the command line and tools).
#[cfg(not(target_arch = "wasm32"))]
pub fn query_url_blocking(url: &str, columns: Vec<String>, req: Request) -> Result<Queried> {
    let file = remote::file(url);
    let f = file.clone();
    remote::drive_blocking(file, std::sync::Arc::new(move || query_now(&f, &columns, &req)))
}

/// Reads the points of a GeoParquet by its web address in an extent, with a list column's
/// vectors (see [`parquet::read_vectors`]), fetching what it needs (off the caller's thread).
pub fn vectors_url(
    url: &str,
    extent: Extent,
    column: String,
    limits: (usize, usize),
    done: Box<dyn FnOnce(Result<parquet::Vectors>) + Send>,
) {
    let file = remote::file(url);
    let f = file.clone();
    remote::drive(file, std::sync::Arc::new(move || parquet::read_vectors(&f, &extent, &column, limits.0, limits.1)), done);
}

/// [`vectors_url`], waiting for it (the command line and tools).
#[cfg(not(target_arch = "wasm32"))]
pub fn vectors_url_blocking(url: &str, extent: Extent, column: String, limits: (usize, usize)) -> Result<parquet::Vectors> {
    let file = remote::file(url);
    let f = file.clone();
    remote::drive_blocking(file, std::sync::Arc::new(move || parquet::read_vectors(&f, &extent, &column, limits.0, limits.1)))
}

/// Reads a request, fetching what a source read by its web address needs, and waiting for it.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_blocking(src: &std::sync::Arc<dyn FeatureSource>, req: &Request) -> Result<Batch> {
    match src.remote() {
        Some(file) => {
            let (s, r) = (src.clone(), *req);
            remote::drive_blocking(file, std::sync::Arc::new(move || s.read(&r)))
        }
        None => src.read(req),
    }
}

/// A source whose reads wait for the bytes a source by its web address needs (tools and Load
/// All Features on the desktop).
#[cfg(not(target_arch = "wasm32"))]
pub struct Fetching(pub std::sync::Arc<dyn FeatureSource>);

#[cfg(not(target_arch = "wasm32"))]
impl FeatureSource for Fetching {
    fn info(&self) -> &SourceInfo {
        self.0.info()
    }

    fn lost_to_crs(&self) -> bool {
        self.0.lost_to_crs()
    }

    fn skipped_at_open(&self) -> usize {
        self.0.skipped_at_open()
    }

    fn read(&self, req: &Request) -> Result<Batch> {
        read_blocking(&self.0, req)
    }
}

/// The vector layers in a file: the feature tables of a GeoPackage, or the one layer of others.
pub fn layers(path: &Path) -> Result<Vec<String>> {
    match ext(path).as_str() {
        #[cfg(not(target_arch = "wasm32"))]
        "gpkg" => gpkg::layers(path),
        _ => Ok(vec![open_path(path, None)?.info().name.clone()]),
    }
}

/// Reads a whole source into a dataset: a chunk at a time up to [`MAX_READ_BYTES`] where the
/// source can, otherwise in one read up to [`MAX_LIMIT`]. A larger source, or an incomplete read,
/// is refused.
pub fn materialize(src: &dyn FeatureSource) -> Result<(Dataset, ReadReport)> {
    materialize_within(src, MAX_READ_BYTES)
}

/// [`materialize`], with `budget` bytes for a source read a chunk at a time.
fn materialize_within(src: &dyn FeatureSource, budget: u64) -> Result<(Dataset, ReadReport)> {
    let info = src.info();
    let (features, skipped) = if src.visits_chunks() { read_chunks(src, budget)? } else { read_at_once(src)? };
    let mut ds = Dataset::new(info.name.clone(), info.geometry_type, info.fields.clone());
    ds.crs = info.coordinate_system();
    // A streamed Shapefile skips unconvertible shapes rather than retaining empty geometry.
    // Once materialized, it must still direct CRS repairs back to the original file.
    ds.crs_incomplete = (info.format == "Shapefile" && skipped > 0) || src.lost_to_crs();
    ds.zm_dropped = info.zm_dropped;
    ds.features = features.into();
    let report = ReadReport {
        skipped: skipped + src.skipped_at_open(),
        messages: info.notes.iter().map(|n| format!("{}: {n}", info.name)).collect(),
    };
    if ds.geometry_type == GeometryType::None
        && let Some(g) = ds.features.iter().find_map(|f| f.geometry.as_ref())
    {
        ds.geometry_type = g.kind();
    }
    Ok((ds, report))
}

/// Every feature of a source in one read, and the rows it skipped, refusing a source over
/// [`MAX_LIMIT`] and an incomplete read.
fn read_at_once(src: &dyn FeatureSource) -> Result<(Vec<Feature>, usize)> {
    let info = src.info();
    let too_many = |n| DataError::Format(format!("{}: the layer has {n} features, more than the {MAX_LIMIT} read at once", info.name));
    // Refuse before reading or allocating a sample when the source already knows its size.
    if let Some(n) = info.count.filter(|n| *n > MAX_LIMIT as u64) {
        return Err(too_many(n));
    }
    let batch = src.read(&Request::all())?;
    if !batch.complete {
        if batch.matched > MAX_LIMIT as u64 {
            return Err(too_many(batch.matched));
        }
        return Err(DataError::Format(format!(
            "{}: could not read the whole source ({} of {} features; read limit {MAX_LIMIT})",
            info.name,
            batch.features.len(),
            batch.matched
        )));
    }
    // Preserve Load All Features' conservative refusal when the source cannot give a count.
    if info.count.is_none() && batch.features.len() >= MAX_LIMIT {
        return Err(DataError::Format(format!("{}: the layer has more features than the {MAX_LIMIT} read at once", info.name)));
    }
    Ok((batch.features, batch.skipped))
}

/// Every feature of a source that can be read a chunk at a time, and the rows it skipped,
/// refusing the source once its features would hold more than `budget` bytes.
fn read_chunks(src: &dyn FeatureSource, budget: u64) -> Result<(Vec<Feature>, usize)> {
    let info = src.info();
    let row_bytes = std::mem::size_of::<Feature>() as u64;
    let too_large = || {
        DataError::Format(format!(
            "{}: the layer needs more than the {} MiB a whole read may hold: clip or filter it first",
            info.name,
            budget >> 20
        ))
    };
    let no_memory = || DataError::Format(format!("{}: not enough memory to read the whole layer", info.name));
    // A count that alone is too many rows is refused before reading. It isn't trusted to size the
    // rows: a file's header can claim any count.
    if info.count.is_some_and(|n| n.saturating_mul(row_bytes) > budget) {
        return Err(too_large());
    }
    let mut features: Vec<Feature> = Vec::new();
    let mut held = 0u64;
    let batch = src.visit_chunks(None, READ_CHUNK_BYTES, &mut |chunk| {
        held = chunk.iter().fold(held, |n, f| n.saturating_add(chunk_memory::feature_bytes(f) as u64));
        let needed = features.len().saturating_add(chunk.len());
        if needed > features.capacity() {
            // Grow by doubling, but never past what the budget leaves for the rows.
            let room = usize::try_from(budget.saturating_sub(held) / row_bytes).unwrap_or(usize::MAX);
            let grown = needed.max(features.capacity().saturating_mul(2)).min(room);
            if grown < needed {
                return Err(too_large());
            }
            features.try_reserve_exact(grown - features.len()).map_err(|_| no_memory())?;
        } else if held.saturating_add((features.capacity() as u64).saturating_mul(row_bytes)) > budget {
            return Err(too_large());
        }
        features.extend(chunk);
        Ok(())
    })?;
    Ok((features, batch.skipped))
}

/// The geometry type of a source that doesn't declare one, from its first feature.
pub(crate) fn peek_kind(src: &dyn FeatureSource) -> GeometryType {
    src.read(&Request { extent: None, limit: 1 })
        .ok()
        .and_then(|b| b.features.into_iter().find_map(|f| f.geometry))
        .map(|g| g.kind())
        .unwrap_or(GeometryType::None)
}

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// A source's coordinate system: converts its geometry to longitude/latitude and a request's
/// extent to its own coordinates.
#[derive(Clone, Debug)]
pub(crate) struct Reproject {
    crs: Crs,
    /// `None` for longitude/latitude.
    t: Option<Transformer>,
}

impl Reproject {
    /// For a reader that converts only EPSG codes: a coordinate system declared without one, by
    /// `authority` and `code` or by a `wkt` definition, when it is another body's
    /// ([`crate::raster::planetary::declared`]). `None` when it isn't.
    /// - Its longitude/latitude are read as they are, with a note. The layer keeps the system when
    ///   the definition gives it.
    /// - Other coordinates are an error, because they would land in the wrong place.
    ///
    /// `what` starts the note or the error (`its coordinate system (srs_id 49900)`).
    pub(crate) fn planetary(
        what: &str,
        authority: Option<&str>,
        code: Option<i64>,
        wkt: Option<&str>,
        notes: &mut Vec<String>,
    ) -> Option<Result<Reproject>> {
        use crate::raster::planetary::{Declared, declared};
        Some(match declared(authority, code, wkt)? {
            Declared::Refused(e) => Err(DataError::Format(format!("{what}: {e}"))),
            Declared::LonLat { note, crs } => {
                notes.push(format!("{what}: {note}"));
                Ok(Reproject::from_crs(crs.unwrap_or(Crs::LonLat), notes))
            }
        })
    }

    /// From an EPSG code; an unknown or missing code is taken as longitude/latitude, with a note.
    pub(crate) fn from_epsg(code: Option<u32>, notes: &mut Vec<String>) -> Reproject {
        let crs = match code {
            None | Some(0) => Crs::LonLat,
            Some(c) => match Crs::from_epsg(c) {
                Some(crs) => crs,
                None => {
                    // Unknown, or one only the desktop app can use (an azimuthal equidistant one in
                    // the web app), which it says.
                    notes.push(format!("{}; read as longitude/latitude", crate::raster::crs::unusable_epsg(c)));
                    Crs::LonLat
                }
            },
        };
        Reproject::from_crs(crs, notes)
    }

    /// From a coordinate system; one that can't be converted is taken as longitude/latitude, with
    /// a note.
    pub(crate) fn from_crs(crs: Crs, notes: &mut Vec<String>) -> Reproject {
        let t = match crs {
            Crs::LonLat => None,
            other => match other.transformer() {
                Ok(t) => Some(t),
                Err(e) => {
                    notes.push(format!("{e}; read as longitude/latitude"));
                    None
                }
            },
        };
        Reproject { crs: if t.is_some() { crs } else { Crs::LonLat }, t }
    }

    /// From a coordinate system already known to convert (`None` for longitude/latitude).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn known(found: Option<(Crs, Transformer)>) -> Reproject {
        match found {
            Some((crs, t)) => Reproject { crs, t: Some(t) },
            None => Reproject { crs: Crs::LonLat, t: None },
        }
    }

    /// What converts to longitude/latitude (`None` when coordinates are taken as they are).
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn transformer(&self) -> Option<&Transformer> {
        self.t.as_ref()
    }

    /// Whether coordinates are taken as they are.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn is_lonlat(&self) -> bool {
        self.t.is_none()
    }

    pub(crate) fn label(&self) -> String {
        self.crs.label()
    }

    /// The system as [`SourceInfo::crs`] gives it: a custom one (another body's, declared by a
    /// definition) by its PROJ definition, which [`SourceInfo::coordinate_system`] reads back as
    /// that very system, as the Shapefile source does; else its name.
    pub(crate) fn info_crs(&self) -> String {
        self.crs.custom_def().map(|d| d.proj4.clone()).unwrap_or_else(|| self.label())
    }

    /// The geometry in longitude/latitude; `None` when a coordinate can't be converted.
    pub(crate) fn geometry(&self, g: Geometry) -> Option<Geometry> {
        let Some(t) = &self.t else { return Some(g) };
        let mut ok = true;
        let out = g.map_coords(&|c| t.to_lonlat(c).unwrap_or(Coord::new(f64::NAN, f64::NAN)));
        out.for_each_coord(|c| ok &= c.is_finite());
        ok.then_some(out)
    }

    /// A longitude/latitude geometry in the source's coordinates.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn to_native(&self, g: &Geometry) -> Option<Geometry> {
        let out = match &self.t {
            Some(t) => g.map_coords(&|c| t.from_lonlat(c).unwrap_or(Coord::new(f64::NAN, f64::NAN))),
            None => g.clone(),
        };
        let mut ok = true;
        out.for_each_coord(|c| ok &= c.is_finite());
        ok.then_some(out)
    }

    /// A native extent in longitude/latitude.
    pub(crate) fn extent_to_lonlat(&self, e: &Extent) -> Extent {
        match &self.t {
            None => *e,
            Some(t) => around(e, |c| t.to_lonlat(c)),
        }
    }

    /// A longitude/latitude extent in native coordinates.
    pub(crate) fn extent_from_lonlat(&self, e: &Extent) -> Extent {
        match &self.t {
            None => *e,
            Some(t) => {
                // Clamp to the valid range first: projections fail near the poles.
                let c = Extent::new(e.min_x.max(-180.0), e.min_y.max(-89.9), e.max_x.min(180.0), e.max_y.min(89.9));
                around(&c, |p| t.from_lonlat(p))
            }
        }
    }
}

/// A GeoPackage table's `gpkg_spatial_ref_sys` row: organization, its code, and the definition.
pub(crate) struct GpkgSrs {
    pub(crate) organization: Option<String>,
    pub(crate) code: Option<i64>,
    pub(crate) definition: Option<String>,
}

/// A GeoPackage table's coordinate system, decided the same way on the desktop and in the browser
/// (#1476): by its EPSG code when its organization is EPSG and the code is known, else by its WKT
/// definition, else longitude/latitude, with a note. With no row for `srs_id`, it is taken as an
/// EPSG code only for WGS 84 and Web Mercator, the ids writers commonly use without defining.
/// Another body's system ([`Reproject::planetary`]) is said, or an error when its coordinates
/// would be in the wrong place as longitude/latitude.
pub(crate) fn gpkg_reproject(srs_id: i64, row: Option<GpkgSrs>, notes: &mut Vec<String>) -> Result<Reproject> {
    // The WKT the row defines the system with, if any. ("undefined", or a writer's "undefined
    // geographic …", defines nothing.)
    let definition = row
        .as_ref()
        .and_then(|r| r.definition.as_deref())
        .map(str::trim)
        .filter(|d| !d.is_empty() && !d.get(..9).is_some_and(|p| p.eq_ignore_ascii_case("undefined")));
    // An EPSG code: the row's when its organization is EPSG (a table-local id such as 100001
    // isn't one, nor is 0); with no row, or a row naming neither organization nor code nor a
    // definition, the id itself for WGS 84 and Web Mercator only.
    let epsg = match &row {
        Some(GpkgSrs { organization: Some(org), code: Some(code), .. }) if org.eq_ignore_ascii_case("epsg") => {
            u32::try_from(*code).ok().filter(|c| *c != 0)
        }
        Some(GpkgSrs { organization: None, code: None, .. }) if definition.is_none() => {
            u32::try_from(srs_id).ok().filter(|c| matches!(c, 4326 | 3857))
        }
        None => u32::try_from(srs_id).ok().filter(|c| matches!(c, 4326 | 3857)),
        Some(_) => None,
    };
    if let Some(crs) = epsg.and_then(Crs::from_epsg) {
        if let Some(note) = definition.and_then(|d| crate::raster::wkt::projection_compatibility_note(d, crs)) {
            notes.push(format!(
                "its coordinate system (srs_id {srs_id}): using EPSG:{} parameters in preference to the WKT definition; {note}",
                crs.epsg()
            ));
        }
        return Ok(Reproject::from_crs(crs, notes));
    }
    let what = format!("its coordinate system (srs_id {srs_id})");
    // Another body's system that can't be converted is told by its definition, or without one by
    // its organization and code (an IAU one). Its longitude/latitude are read with a notice. Other
    // coordinates are refused, because they would land in the wrong place. A planetary definition
    // that `read_wkt` reads is converted like any other, with its own notice.
    let other_body = |definition: Option<&str>, notes: &mut Vec<String>| {
        row.as_ref().and_then(|r| Reproject::planetary(&what, r.organization.as_deref(), r.code, definition, notes))
    };
    // Else the WKT the row defines it with: a custom system, or a code this build doesn't know.
    if let Some(definition) = definition {
        let crs = match crate::raster::wkt::read_wkt(definition) {
            Ok((crs, note)) => {
                notes.extend(note.map(|n| format!("{what}: {n}")));
                crs
            }
            Err(e) => {
                if let Some(reproject) = other_body(Some(definition), notes) {
                    return reproject;
                }
                notes.push(format!("{what} can't be read ({e}); read as longitude/latitude"));
                Crs::LonLat
            }
        };
        return Ok(Reproject::from_crs(crs, notes));
    }
    if let Some(reproject) = other_body(None, notes) {
        return reproject;
    }
    if epsg.is_some() {
        // A code this build doesn't know, and no definition to fall back on: said by its code.
        return Ok(Reproject::from_epsg(epsg, notes));
    }
    // Longitude/latitude is what WGS 84 and the spec's "undefined geographic" id 0 mean; any other
    // id, the "undefined cartesian" -1 too, is assumed to be it.
    if srs_id != 0 && srs_id != 4326 {
        notes.push(format!("its coordinate system (srs_id {srs_id}) has no EPSG code; read as longitude/latitude"));
    }
    Ok(Reproject::from_crs(Crs::LonLat, notes))
}

/// The extent of an extent's edges (sampled, since edges curve when reprojected) through `f`.
fn around(e: &Extent, f: impl Fn(Coord) -> Option<Coord>) -> Extent {
    let mut out = Extent::EMPTY;
    if e.is_empty() {
        return out;
    }
    const N: usize = 16;
    for i in 0..=N {
        let t = i as f64 / N as f64;
        let x = e.min_x + t * e.width();
        let y = e.min_y + t * e.height();
        for c in [Coord::new(x, e.min_y), Coord::new(x, e.max_y), Coord::new(e.min_x, y), Coord::new(e.max_x, y)] {
            if let Some(p) = f(c) {
                out.expand(p);
            }
        }
    }
    out
}

/// The step that keeps at most `limit` of `matched` items, evenly spread (1 when they all fit,
/// including when `matched` is 0, which `Sampler::new` relies on).
pub(crate) fn stride(matched: u64, limit: usize) -> u64 {
    if limit == 0 {
        return u64::MAX;
    }
    matched.div_ceil(limit as u64).max(1)
}

/// Keeps an evenly spread sample of at most `limit` items from a stream of unknown length: every
/// `step`-th item, doubling the step (and dropping every other kept item) whenever the sample
/// overflows.
pub(crate) struct Sampler<T> {
    limit: usize,
    step: u64,
    seen: u64,
    pub(crate) items: Vec<T>,
}

impl<T> Sampler<T> {
    pub(crate) fn new(limit: usize) -> Self {
        Self::with_total(limit, 0)
    }

    /// A sampler for a stream of about `total` items: it starts at the step that fits them.
    pub(crate) fn with_total(limit: usize, total: u64) -> Self {
        let limit = limit.max(1);
        Sampler { limit, step: stride(total, limit), seen: 0, items: Vec::new() }
    }

    /// Counts a match; whether it would be kept (so the caller decodes only those).
    pub(crate) fn wants(&mut self) -> bool {
        self.seen += 1;
        (self.seen - 1).is_multiple_of(self.step)
    }

    /// Keeps an item `wants` asked for.
    pub(crate) fn keep(&mut self, item: T) {
        self.items.push(item);
        if self.items.len() > self.limit {
            let mut i = 0;
            self.items.retain(|_| {
                i += 1;
                (i - 1) % 2 == 0
            });
            self.step = self.step.saturating_mul(2);
        }
    }

    /// Matches counted so far.
    pub(crate) fn seen(&self) -> u64 {
        self.seen
    }

    /// Whether every match was kept.
    pub(crate) fn complete(&self) -> bool {
        self.step == 1
    }
}

/// The limit a request may use.
pub(crate) fn clamp_limit(limit: usize) -> usize {
    limit.min(MAX_LIMIT)
}

/// Whether a feature's geometry falls in a request's extent (always true without one).
pub(crate) fn in_request(g: &Geometry, req: &Request) -> bool {
    match &req.extent {
        Some(e) => g.extent().intersects(e),
        None => true,
    }
}

/// Makes ring rustls's process-wide crypto provider. SQL Server's client also builds aws-lc-rs
/// into rustls, and with two providers built in rustls picks neither by itself (the MySQL
/// client's `ClientConfig::builder()` would panic).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn tls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

#[cfg(test)]
mod tests;

pub mod chunk_memory;
#[cfg(not(target_arch = "wasm32"))]
pub mod spill;
