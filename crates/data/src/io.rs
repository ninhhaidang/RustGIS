//! Vector readers and writers: GeoJSON, CSV (with longitude/latitude columns) and Shapefile.

use std::path::Path;

use serde_json::{Map, Value as Json, json};

use crate::raster::{Crs, planetary};
use crate::{Coord, DataError, Dataset, Field, FieldType, Geometry, GeometryType, ReadReport, Result, Value};

/// Largest file read into memory in the browser, on a 32-bit build, or on a computer whose memory
/// isn't known (256 MiB): the floor of [`max_bytes`]. Also where a file stops being read whole and
/// is read in place instead (a GeoJSON file, a Shapefile), or handed to GDAL (an OpenStreetMap
/// extract), whatever the memory: those readers don't need it in memory.
pub const BYTES_FLOOR: u64 = 256 << 20;

/// The most [`max_bytes`] grows to, however much memory the computer has (4 GiB).
pub const BYTES_CEILING: u64 = 4 << 30;

/// The memory a file read whole takes for each byte of it, at its peak, as measured adding files
/// of about 200 MiB from the command line (#2408):
/// - CityGML, LoD 2 buildings of seven surfaces: about 1.7;
/// - GeoJSON polygons of 40 vertices and 8 attributes, read whole: about 2.8;
/// - KML polygons of 40 vertices and 2 attributes: about 5.7;
/// - GeoJSON points with 30 one-letter attributes, read whole, about the most a byte costs: about
///   7.75.
///
/// Planned at 8: at the limit, even the costliest measured takes about a quarter of the memory.
const MEMORY_PER_BYTE: u64 = 8;

/// Largest file read into memory on this computer (#2408): on the desktop as much as a quarter of
/// its memory holds at [`MEMORY_PER_BYTE`] a byte, from [`BYTES_FLOOR`] up to [`BYTES_CEILING`],
/// as the other memory limits follow it (#1291 step 2); [`BYTES_FLOOR`] in the browser. Less once
/// RustGIS holds more than half the memory, or the computer has less free ([`crate::memory::share`],
/// #2431), never below the floor.
pub fn max_bytes() -> u64 {
    if let Some(max) = TEST_MAX.with(std::cell::Cell::get) {
        return max;
    }
    max_bytes_within(crate::memory::share(crate::memory::physical(), crate::memory::held(), crate::memory::available()))
}

/// [`max_bytes`] on a computer with `memory` bytes (`None`: not known) with nothing held. The
/// formula is [`crate::memory::scaled`]'s, worked out here in `u64`, as that one counts in `usize`,
/// which doesn't hold 4 GiB on a 32-bit build.
fn max_bytes_for(memory: Option<u64>) -> u64 {
    max_bytes_within(memory.map(|m| m / crate::memory::SHARE))
}

/// [`max_bytes`] when a job plans for `share` bytes ([`crate::memory::share`]; `None`: not known):
/// less once RustGIS holds more than half the memory (#2431), never below [`BYTES_FLOOR`].
fn max_bytes_within(share: Option<u64>) -> u64 {
    share.map_or(BYTES_FLOOR, |share| (share / MEMORY_PER_BYTE).clamp(BYTES_FLOOR, bytes_ceiling()))
}

/// The most [`max_bytes`] grows to in this build: [`BYTES_CEILING`] on a 64-bit desktop build. A
/// 32-bit build stays at [`BYTES_FLOOR`] whatever the memory: a file read whole takes several
/// times its size in address space, which 4 GiB doesn't hold for a larger one. So does a phone:
/// it reports all its memory, but an app may use much less of it before the system stops it.
fn bytes_ceiling() -> u64 {
    if usize::BITS >= 64 && !cfg!(any(target_os = "android", target_os = "ios")) { BYTES_CEILING } else { BYTES_FLOOR }
}

/// The refusal of a file of `len` bytes, past [`max_bytes`]: its size, and the limit, saying
/// where it follows the computer's memory, so a file refused here and opened elsewhere is
/// understood.
pub fn too_large(len: u64) -> String {
    too_large_said(len, max_bytes())
}

/// [`too_large`] against the limit `max` taken when the read started. What is held and free is
/// read again here: it may have moved since `max` was taken, so the figures said are the ones now,
/// beside the limit the read was held to.
fn too_large_said(len: u64, max: u64) -> String {
    let memory = crate::memory::physical().filter(|_| bytes_ceiling() > BYTES_FLOOR);
    // A test's own limit isn't lowered by what is held or free.
    let (held, free) =
        if TEST_MAX.with(std::cell::Cell::get).is_some() { (0, None) } else { (crate::memory::held(), crate::memory::available()) };
    too_large_held(len, max, memory, held, free)
}

/// [`too_large_said`] on a computer with `memory` bytes (`None`: the limit doesn't follow it)
/// while RustGIS holds `held` and `free` is free (`None`: not known): when the limit is below the
/// one with nothing else in use, it says what is held and free, that limit, and what to do (#2431).
fn too_large_held(len: u64, max: u64, memory: Option<u64>, held: u64, free: Option<u64>) -> String {
    let file = size_said(len, true);
    let Some(memory) = memory else {
        return format!("file is too large ({file}; the most read into memory is {})", size_said(max, false));
    };
    let full = max_bytes_for(Some(memory));
    if max >= full || (held == 0 && free.is_none()) {
        return format!(
            "file is too large ({file}; the most read into memory is {} on this computer, a limit that follows its memory)",
            size_said(max, false)
        );
    }
    let free = free.map(|free| format!(" and {} free", size_said(free, false))).unwrap_or_default();
    format!(
        "file is too large ({file}; the most read into memory is {} on this computer now, with RustGIS holding {}{free}, {} otherwise): free memory by removing layers or tables you no longer need (save and reopen the project to let go of what Undo keeps of them) or closing other programs, then try again",
        size_said(max, false),
        size_said(held, true),
        size_said(full, false)
    )
}

/// A size in MiB, rounded `up` (a file's, so it never looks within the limit) or down (the
/// limit's), or in bytes below one.
pub(crate) fn size_said(bytes: u64, up: bool) -> String {
    let mib = if up { bytes.div_ceil(1 << 20) } else { bytes >> 20 };
    if bytes < 1 << 20 { format!("{bytes} bytes") } else { format!("{mib} MiB") }
}

thread_local! {
    /// A smaller [`max_bytes`] for the tests on this thread ([`with_max_bytes`]), so a test of the
    /// limit doesn't build a file of gigabytes.
    static TEST_MAX: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Runs `f` with [`max_bytes`] at `max` on the calling thread only (work `f` hands to another
/// thread sees the real limit); the limit is restored even if `f` panics. For tests, here and in
/// the crates above.
#[doc(hidden)]
pub fn with_max_bytes<T>(max: u64, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_MAX.with(|m| m.set(self.0));
        }
    }
    let _restore = Restore(TEST_MAX.with(|m| m.replace(Some(max))));
    f()
}

const FILE_SIZE_LIMIT_START: &str = " is too large to open (limit ";
const VECTOR_SIZE_LIMIT_END: &str =
    " MiB). Split it into smaller files, or convert vector data to FlatGeobuf or GeoPackage and open it on the desktop.";
const SHAPEFILE_SIZE_LIMIT_END: &str =
    " MiB). Split the whole Shapefile, or convert it to FlatGeobuf or GeoPackage and open it on the desktop.";
const SPLIT_SIZE_LIMIT_END: &str = " MiB). Split the data into smaller files and import each one separately.";

/// The whole-file refusal, using the reader's limit and recovery advice for the file type.
pub fn file_size_error(name: &str, limit: u64) -> String {
    let ext = Path::new(name).extension();
    if ext.is_some_and(|ext| ext.eq_ignore_ascii_case("shp") || ext.eq_ignore_ascii_case("shx")) {
        return shapefile_size_error(name, limit);
    }
    let dbf = ext.is_some_and(|ext| ext.eq_ignore_ascii_case("dbf"));
    let table = is_delimited_name(name) || crate::sheets::is_sheet_name(name) || dbf;
    let raster = crate::raster::is_raster_name(name) || crate::raster::is_jpeg2000_name(name) || crate::mbtiles::is_mbtiles_name(name);
    let end = if table || raster || crate::source::is_source_name(name) { SPLIT_SIZE_LIMIT_END } else { VECTOR_SIZE_LIMIT_END };
    format!("{name}{FILE_SIZE_LIMIT_START}{}{end}", limit >> 20)
}

/// Names the refused part and gives recovery advice for the whole Shapefile.
pub(crate) fn shapefile_size_error(name: &str, limit: u64) -> String {
    format!("{name}{FILE_SIZE_LIMIT_START}{}{SHAPEFILE_SIZE_LIMIT_END}", limit >> 20)
}

/// Recovery instructions for a refused whole-file read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileSizeAdvice {
    Split,
    Vector,
    Shapefile,
}

/// Recovers a whole-file refusal's file name, size limit and recovery instructions.
pub fn file_size_error_args(error: &str) -> Option<(&str, &str, FileSizeAdvice)> {
    for (end, advice) in [
        (SPLIT_SIZE_LIMIT_END, FileSizeAdvice::Split),
        (VECTOR_SIZE_LIMIT_END, FileSizeAdvice::Vector),
        (SHAPEFILE_SIZE_LIMIT_END, FileSizeAdvice::Shapefile),
    ] {
        if let Some(body) = error.strip_suffix(end) {
            let (name, limit) = body.rsplit_once(FILE_SIZE_LIMIT_START)?;
            limit.parse::<u64>().ok()?;
            return Some((name, limit, advice));
        }
    }
    None
}

/// File types the readers understand, for open dialogs.
pub const READ_EXTENSIONS: &[&str] = &[
    "geojson",
    "json",
    "csv",
    "tsv",
    "txt",
    "dat",
    "gz",
    "shp",
    "dbf",
    "zip",
    "kml",
    "kmz",
    "gpx",
    "gpkg",
    "parquet",
    "geoparquet",
    "fgb",
    "xlsx",
    "xlsm",
    "xlsb",
    "xls",
    "ods",
    "rss",
    "atom",
    "georss",
    "landxml",
    "xml",
    "gml",
    "dxf",
    "mif",
    "pbf",
    "topojson",
    "las",
    "laz",
];

/// Most XML nodes parsed from one document (KML, GPX): each costs tens of bytes, and the
/// browser's memory is smaller (a 1M-point GPX track is about 8M nodes).
pub(crate) const XML_NODES: u32 = if cfg!(target_arch = "wasm32") { 8_000_000 } else { 25_000_000 };

/// A layer read from a file that holds several, with the styles its features' `style` field
/// names (KML).
pub struct LayerRead {
    pub dataset: Dataset,
    pub report: ReadReport,
    pub styles: Vec<(String, crate::kml::Style)>,
}

/// Whether a file holds several layers ([`read_layers`] reads them all): a zip, KML, KMZ, GPX or
/// `.city.json`, among others.
pub fn is_multi_layer_name(name: &str) -> bool {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    crate::cityjson::is_cityjson_name(name)
        || matches!(
            ext.as_str(),
            "zip" | "kml" | "kmz" | "gpx" | "rss" | "atom" | "georss" | "landxml" | "xml" | "gml" | "dxf" | "pbf" | "topojson"
        )
}

/// Whether a `.json` file is CityJSON, by its name or its contents.
pub fn is_cityjson(file_name: &str, bytes: &[u8]) -> bool {
    file_name.to_ascii_lowercase().ends_with(".json")
        && (crate::cityjson::is_cityjson_name(file_name) || std::str::from_utf8(bytes).is_ok_and(crate::cityjson::looks_like_cityjson))
}

/// Whether a `.gml` or `.xml` file is a CityGML city model, by its root.
pub fn is_citygml(file_name: &str, bytes: &[u8]) -> bool {
    let ext = Path::new(file_name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "gml" | "xml") && crate::citygml::looks_like_citygml(bytes)
}

/// Whether a file is a city model, CityJSON or CityGML: its layers all come from one reading, which
/// takes the coordinate system a caller gives ([`read_city_model`]).
pub fn is_city_model(file_name: &str, bytes: &[u8]) -> bool {
    is_cityjson(file_name, bytes) || is_citygml(file_name, bytes)
}

/// Reads a city model's layers (see [`is_city_model`]); `crs`, when given, goes over the system it
/// names.
pub fn read_city_model(file_name: &str, bytes: &[u8], crs: Option<Crs>) -> Result<Vec<(Dataset, ReadReport)>> {
    if is_citygml(file_name, bytes) { crate::citygml::read(file_name, bytes, crs) } else { crate::cityjson::read(file_name, bytes, crs) }
}

/// As [`read_city_model`], for a member of an archive: a model that names a system RustGIS knows
/// keeps it over `crs`.
fn read_city_member(file_name: &str, bytes: &[u8], crs: Option<Crs>) -> Result<Vec<(Dataset, ReadReport)>> {
    if is_citygml(file_name, bytes) {
        crate::citygml::read_member(file_name, bytes, crs)
    } else {
        crate::cityjson::read_member(file_name, bytes, crs)
    }
}

/// Reads every layer of a file held in memory: the Shapefiles and other files of a zip, a KML or
/// KMZ's points, lines and polygons (with their styles), or a GPX's waypoints, routes, tracks and
/// track points. Other files give their one layer. A file read as it is added (KML and KMZ,
/// LandXML, RSS, Atom, GeoRSS, XML that isn't a city model) stays within [`BYTES_FLOOR`], as
/// before #2408; the others take [`max_bytes`].
pub fn read_layers(file_name: &str, bytes: &[u8]) -> Result<Vec<LayerRead>> {
    let ext = Path::new(file_name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    // KML and KMZ, LandXML, RSS, Atom, GeoRSS and other XML that isn't a city model are read as
    // they are added, on the desktop's UI thread, never ahead: within [`BYTES_FLOOR`] whatever the
    // memory (#2408), wherever they come from.
    let as_added = matches!(ext.as_str(), "kml" | "kmz" | "rss" | "atom" | "georss" | "landxml")
        || (ext == "xml" && !crate::citygml::looks_like_citygml(bytes));
    let max = if as_added { BYTES_FLOOR } else { max_bytes() };
    if bytes.len() as u64 > max {
        return Err(DataError::Format(file_size_error(file_name, max)));
    }
    let plain =
        |v: Vec<(Dataset, ReadReport)>| v.into_iter().map(|(dataset, report)| LayerRead { dataset, report, styles: Vec::new() }).collect();
    let mut out: Vec<LayerRead> = match ext.as_str() {
        "zip" => plain(read_zip(file_name, bytes)?),
        "kml" | "kmz" => crate::kml::read(file_name, bytes)?
            .into_iter()
            .map(|l| LayerRead { dataset: l.dataset, report: l.report, styles: l.styles })
            .collect(),
        "gpx" => plain(crate::gpx::read(file_name, bytes)?),
        "rss" | "atom" | "georss" => plain(crate::georss::read(file_name, bytes)?),
        "landxml" => plain(crate::landxml::read(file_name, bytes, None)?),
        // Plain XML: whichever of the XML formats its contents say.
        "xml" => {
            let text = String::from_utf8_lossy(bytes.get(..bytes.len().min(8192)).unwrap_or(bytes));
            if crate::citygml::looks_like_citygml(bytes) {
                plain(crate::citygml::read(file_name, bytes, None)?)
            } else if crate::landxml::looks_like_landxml(&text) {
                plain(crate::landxml::read(file_name, bytes, None)?)
            } else if crate::georss::looks_like_georss(&text) {
                plain(crate::georss::read(file_name, bytes)?)
            } else if text.contains("<gpx") {
                plain(crate::gpx::read(file_name, bytes)?)
            } else if text.contains("<kml") {
                crate::kml::read(file_name, bytes)?
                    .into_iter()
                    .map(|l| LayerRead { dataset: l.dataset, report: l.report, styles: l.styles })
                    .collect()
            } else {
                return Err(DataError::Format(format!(
                    "{file_name}: an XML file RustGIS doesn't read (it reads GeoRSS, LandXML, CityGML, KML and GPX)"
                )));
            }
        }
        // A CityGML city model, or the features of a collection.
        "gml" if crate::citygml::looks_like_citygml(bytes) => plain(crate::citygml::read(file_name, bytes, None)?),
        "gml" => plain(crate::gml::read(file_name, bytes)?),
        "dxf" => plain(crate::dxf::read(file_name, bytes, &crate::dxf::Options::default())?),
        // OpenStreetMap PBF, not a vector tile (`.pbf` too).
        "pbf" if crate::osm::looks_like_osm_pbf(bytes) => plain(crate::osm::read(file_name, bytes)?),
        // An ArcGIS feature service's answer to a query (`f=pbf`), fetched or saved: told by its
        // content, as `.pbf` names OpenStreetMap too.
        "pbf" if crate::esripbf::is_esripbf(bytes) => {
            let name = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("layer");
            plain(vec![crate::esripbf::read(bytes, name)?])
        }
        "pbf" => {
            return Err(DataError::Unsupported(format!(
                "{file_name}: a .pbf that isn't OpenStreetMap data or an ArcGIS feature service's answer (vector tiles are added with Add Tile Layer)"
            )));
        }
        // A CityJSON model, named .city.json or .json.
        "json" if is_cityjson(file_name, bytes) => plain(crate::cityjson::read(file_name, bytes, None)?),
        // A TopoJSON topology, named .topojson or .json.
        "topojson" | "json" if ext == "topojson" || std::str::from_utf8(bytes).is_ok_and(crate::topojson::looks_like_topojson) => {
            let text = std::str::from_utf8(bytes).map_err(|_| DataError::Format(format!("{file_name} is not UTF-8 text")))?;
            plain(crate::topojson::read(file_name, text)?)
        }
        _ => plain(vec![read_bytes(file_name, bytes)?]),
    };
    if out.is_empty() {
        return Err(DataError::Format(format!("{file_name}: nothing to draw")));
    }
    for l in &mut out {
        l.dataset.source.get_or_insert_with(|| file_name.to_string());
    }
    Ok(out)
}

/// The extension of a file from its first bytes, for a download whose address doesn't say:
/// GeoParquet, FlatGeobuf, GeoTIFF, a zip, a GeoPackage, an ArcGIS feature service's protocol
/// buffer answer (`pbf`), KML, GPX, GeoJSON, delimited text, JPEG 2000, a PDF or GRIB (`None` for
/// anything else, such as a web page).
pub fn sniff_extension(bytes: &[u8]) -> Option<&'static str> {
    if let Some(ext) = crate::raster::jpeg2000_extension(bytes) {
        return Some(ext);
    }
    let head = bytes.get(..bytes.len().min(4096)).unwrap_or(bytes);
    let text = String::from_utf8_lossy(head);
    let start = text.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    let mut lines = start.lines().filter(|l| !l.trim().is_empty());
    let (first_line, second_line) = (lines.next().unwrap_or(""), lines.next());
    // A separator splitting the first two lines alike (a header and a row): not a sentence with a
    // comma, such as a plain-text error.
    let split_alike = second_line.is_some_and(|second| {
        [',', ';', '\t', '|'].iter().any(|&c| {
            let n = first_line.matches(c).count();
            n > 0 && second.matches(c).count() == n
        })
    });
    Some(match bytes {
        [b'P', b'A', b'R', b'1', ..] => "parquet",
        [b'f', b'g', b'b', ..] => "fgb",
        [b'I', b'I', 42 | 43, 0, ..] | [b'M', b'M', 0, 42 | 43, ..] => "tif",
        [b'P', b'K', 3, 4, ..] => "zip",
        _ if bytes.starts_with(b"SQLite format 3\0") => "gpkg",
        _ if bytes.starts_with(b"%PDF-") => "pdf",
        // A GRIB message of edition 1 or 2 (a byte range of a model's file starts with one).
        [b'G', b'R', b'I', b'B', _, _, _, 1 | 2, ..] => "grb",
        // LiDAR: LAZ marks its point format's top bits.
        _ if crate::lidar::looks_like_las(bytes) => {
            if bytes.get(104).is_some_and(|f| f & 0xC0 != 0) {
                "laz"
            } else {
                "las"
            }
        }
        // An ArcGIS feature service's answer to a query (`…/query?…&f=pbf`, whose address has no
        // extension), read by content under `.pbf`.
        _ if crate::esripbf::is_esripbf(bytes) => "pbf",
        _ if start.starts_with('{') || start.starts_with('[') => "geojson",
        // A CityGML city model, by its root.
        _ if start.starts_with('<') && crate::citygml::looks_like_citygml(bytes) => "gml",
        _ if start.starts_with('<') && text.contains("<kml") => "kml",
        _ if start.starts_with('<') && text.contains("<gpx") => "gpx",
        // Text whose first two lines a separator splits alike, and no control characters.
        _ if !start.starts_with('<') && split_alike && !head.iter().any(|&b| b < 9 || (b > 13 && b < 32)) => "csv",
        _ => return None,
    })
}

/// The first layer of a file holding several ([`read_layers`]), saying what else it holds.
fn first_layer(file_name: &str, bytes: &[u8]) -> Result<(Dataset, ReadReport)> {
    let layers = read_layers(file_name, bytes)?;
    let rest: Vec<String> = layers.iter().skip(1).map(|l| l.dataset.name.clone()).collect();
    let first = layers.into_iter().next().ok_or_else(|| DataError::Format(format!("{file_name}: empty")))?;
    let mut first = (first.dataset, first.report);
    if !rest.is_empty() {
        let leaf = Path::new(file_name).file_name().and_then(|n| n.to_str()).unwrap_or(file_name);
        first.1.messages.push(format!("{leaf}: read its first layer; it also holds {}", rest.join(", ")));
    }
    Ok(first)
}

/// Reads vector data held in memory (a browser upload or a drop), choosing the reader by the
/// file name's extension. A Shapefile needs its sidecar files, so it comes zipped (`.zip`, whose
/// first layer is read).
pub fn read_bytes(file_name: &str, bytes: &[u8]) -> Result<(Dataset, ReadReport)> {
    let path = Path::new(file_name);
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("layer").to_string();
    let max = max_bytes();
    if bytes.len() as u64 > max {
        return Err(DataError::Format(file_size_error(file_name, max)));
    }
    let text = || std::str::from_utf8(bytes).map_err(|_| DataError::Format(format!("{file_name} is not UTF-8 text")));
    let mut out = match ext.as_str() {
        // A CityJSON model or a TopoJSON topology named .json: its first layer, saying what else
        // it holds.
        "json" if is_cityjson(file_name, bytes) => return first_layer(file_name, bytes),
        "json" if crate::topojson::looks_like_topojson(text()?) => return first_layer(file_name, bytes),
        "geojson" | "json" => read_geojson(text()?, &name)?,
        "csv" | "tsv" | "txt" | "dat" => read_delimited_bytes(file_name, bytes, &CsvOptions::default())?,
        "gz" => read_gzip_text(bytes, file_name, &CsvOptions::default())?,
        // A MIF without its .mid (an upload of the one file): no attributes.
        "mif" => crate::mapinfo::read_bytes(bytes, None, &name)?,
        // Picked or dropped with its .dbf, a Shapefile comes zipped (`gather_shapefiles`).
        "shp" => {
            return Err(DataError::Unsupported(
                "shp on its own: add it together with its .dbf, which it needs, and its .shx and .prj if it has them (pick or drop them at once), or add them zipped".into(),
            ));
        }
        "dbf" => read_dbf(bytes.to_vec(), None, &name, &mut csv_memory_budget())?,
        "shx" | "prj" | "cpg" => {
            return Err(DataError::Unsupported(format!("{ext} on its own: it is part of a Shapefile; add it together with its .shp")));
        }
        // The first layer (`read_layers` reads them all), saying what else the file holds.
        "xml" if is_citygml(file_name, bytes) => return first_layer(file_name, bytes),
        "zip" | "kml" | "kmz" | "gpx" | "gml" | "dxf" | "pbf" | "topojson" => return first_layer(file_name, bytes),
        "las" | "laz" => crate::lidar::read_bytes(bytes, &name, None)?,
        // A copy, since only borrowed bytes are given; `read_owned` takes them without one.
        e if crate::source::EXTENSIONS.contains(&e) => return read_owned(file_name, bytes.to_vec()),
        other => return Err(DataError::Unsupported(other.to_string())),
    };
    out.0.source = Some(file_name.to_string());
    Ok(out)
}

/// Reads a GeoPackage, GeoParquet, FlatGeobuf or LAS/LAZ held in memory without copying it
/// (other types go to [`read_bytes`]).
pub fn read_owned(file_name: &str, bytes: Vec<u8>) -> Result<(Dataset, ReadReport)> {
    if !crate::source::is_source_name(file_name) {
        return read_bytes(file_name, &bytes);
    }
    let max = max_bytes();
    if bytes.len() as u64 > max {
        return Err(DataError::Format(file_size_error(file_name, max)));
    }
    let mut out = if crate::lidar::is_lidar_name(file_name) {
        let name = Path::new(file_name).file_stem().and_then(|s| s.to_str()).unwrap_or("layer");
        crate::lidar::read_bytes(&bytes, name, None)?
    } else {
        crate::source::materialize(crate::source::open_bytes(file_name, bytes, None)?.as_ref())?
    };
    out.0.source = Some(file_name.to_string());
    Ok(out)
}

/// Reads a vector file by extension.
pub fn read_path(path: &Path) -> Result<(Dataset, ReadReport)> {
    if let Some(shp) = shapefile_for_dbf(path) {
        return read_path(&shp);
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("layer").to_string();
    let mut out = match ext.as_str() {
        "geojson" | "json" => {
            check_size(path)?;
            let text = crate::provenance::read_to_string(path)?;
            // A CityJSON model or a TopoJSON topology named .json, as read_bytes reads it.
            let leaf = path.file_name().and_then(|n| n.to_str()).unwrap_or("layer.json");
            if ext == "json" && (is_cityjson(leaf, text.as_bytes()) || crate::topojson::looks_like_topojson(&text)) {
                first_layer(leaf, text.as_bytes())?
            } else {
                read_geojson(&text, &name)?
            }
        }
        "csv" | "tsv" | "txt" | "dat" => {
            check_size(path)?;
            let leaf = path.file_name().and_then(|n| n.to_str()).unwrap_or("layer.csv");
            read_delimited_bytes(leaf, &crate::provenance::read(path)?, &CsvOptions::default())?
        }
        "gz" => {
            check_size(path)?;
            read_gzip_text(std::io::BufReader::new(crate::provenance::open(path)?), &path.display().to_string(), &CsvOptions::default())?
        }
        "dbf" => {
            check_size(path)?;
            let mut notes = crate::loss::Notes::new();
            let cpg = crate::dbf::read_cpg(path, &mut notes);
            let (ds, mut report) = read_dbf(std::fs::read(path)?, cpg.as_deref(), &name, &mut csv_memory_budget())?;
            report.messages.extend(notes.prefixed(&name));
            (ds, report)
        }
        "shp" => read_shapefile_in(path, &name, None)?,
        "zip" | "kml" | "kmz" | "gpx" | "gml" | "dxf" | "pbf" | "topojson" => {
            check_size(path)?;
            read_bytes(&path.display().to_string(), &crate::provenance::read(path)?)?
        }
        "xml" => {
            check_size(path)?;
            let bytes = crate::provenance::read(path)?;
            if !is_citygml(&path.display().to_string(), &bytes) {
                return Err(DataError::Unsupported("xml".into()));
            }
            read_bytes(&path.display().to_string(), &bytes)?
        }
        // With its .mid beside it (spelt mid, MID or Mid).
        "mif" => {
            check_size(path)?;
            let mid = ["mid", "MID", "Mid"].iter().map(|e| path.with_extension(e)).find(|p| p.is_file());
            let mid = match mid {
                Some(m) => {
                    check_size(&m)?;
                    Some(crate::provenance::read(m)?)
                }
                None => None,
            };
            crate::mapinfo::read_bytes(&crate::provenance::read(path)?, mid.as_deref(), &name)?
        }
        "las" | "laz" => crate::lidar::read(std::io::BufReader::new(crate::provenance::open(path)?), &name, None)?,
        // Read in place by a provider, so there's no size cap on the file, only on the features.
        e if crate::source::EXTENSIONS.contains(&e) => crate::source::materialize(crate::source::open_path(path, None)?.as_ref())?,
        other => return Err(DataError::Unsupported(other.to_string())),
    };
    out.0.source = Some(path.display().to_string());
    Ok(out)
}

/// Refuses an oversized file before any attempt to read it into memory.
fn check_size(path: &Path) -> Result<()> {
    check_size_with(path, file_size_error)
}

/// Refuses an oversized file with advice supplied by its reader.
fn check_size_with(path: &Path, message: impl FnOnce(&str, u64) -> String) -> Result<()> {
    let len = std::fs::metadata(path)?.len();
    let max = max_bytes();
    if len > max {
        return Err(DataError::Format(message(&path.to_string_lossy(), max)));
    }
    Ok(())
}

/// The field KML and GPX name placemarks by: one called `name`, if any.
pub fn name_field(ds: &Dataset) -> Option<usize> {
    ds.fields.iter().position(|f| f.name.eq_ignore_ascii_case("name"))
}

/// Features a format that holds only features with geometry leaves out.
fn without_geometry(ds: &Dataset) -> usize {
    ds.features.iter().filter(|f| f.geometry.is_none()).count()
}

/// A dataset encoded in the format of extension `ext`, as one file, and the number of features the
/// format couldn't hold. A Shapefile (`shp` or `zip`) comes as a zip of its files, named after the
/// dataset. GeoPackage needs a file ([`write_path`]).
pub fn write_bytes(ds: &Dataset, ext: &str) -> Result<(Vec<u8>, usize)> {
    write_bytes_in(ds, ext, Crs::LonLat)
}

/// The formats that can be written in a coordinate system other than WGS 84 longitude/latitude
/// (GeoJSON, KML and GPX are longitude/latitude by definition). GeoParquet takes only a system with
/// an EPSG code ([`check_projected`]).
pub const PROJECTED_FORMATS: [&str; 6] = ["gpkg", "shp", "zip", "fgb", "parquet", "geoparquet"];

/// Whether a format of extension `ext` can be written in a coordinate system other than WGS 84
/// longitude/latitude when one is asked for: those of [`PROJECTED_FORMATS`], and CSV, whose x and
/// y columns are then in it, though the file can't say which system that is
/// ([`csv_system_note`]); CSV is never written in another by default.
pub fn holds_a_system(ext: &str) -> bool {
    let ext = ext.to_ascii_lowercase();
    PROJECTED_FORMATS.contains(&ext.as_str()) || ext == "csv"
}

/// What is said of a CSV export of a point layer written in `crs` other than longitude/latitude:
/// the file can't say its system, so adding it back needs it given. `None` otherwise.
pub fn csv_system_note(path: &str, geometry_type: GeometryType, crs: Crs) -> Option<String> {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    (ext == "csv" && geometry_type == GeometryType::Point && crs != Crs::LonLat).then(|| {
        format!("{path}: its coordinates are in {}, which a CSV file can't say; give that system when adding it back", crs.label())
    })
}

/// The coordinate system an export to extension `ext` is written in when none is asked for: the
/// one the dataset was read in, for a format that can say it, else WGS 84. A system without an
/// EPSG code goes into a GeoPackage or FlatGeobuf by its WKT definition, as into a Shapefile's
/// `.prj`; one WKT can't describe goes into a Shapefile (without a `.prj`, said) and not into the
/// others ([`default_export_note`] says which system is written instead).
pub fn default_export_crs(ds: &Dataset, ext: &str) -> Crs {
    let ext = ext.to_ascii_lowercase();
    ds.crs.filter(|&c| PROJECTED_FORMATS.contains(&ext.as_str()) && check_projected(&ext, c).is_ok()).unwrap_or(Crs::LonLat)
}

/// What is said when an export to `path` is written in WGS 84 where the layer has a system of
/// its own that the format can't hold ([`default_export_crs`]); `None` when it keeps its system
/// or has none to keep.
pub fn default_export_note(ds: &Dataset, path: &str) -> Option<String> {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let own = ds.crs.filter(|&c| c != Crs::LonLat && PROJECTED_FORMATS.contains(&ext.as_str()))?;
    (default_export_crs(ds, &ext) == Crs::LonLat).then(|| {
        format!(
            "{path}: the coordinate system of {} ({}) {}, so it is written in WGS 84 (EPSG:4326); give a \"crs\" to write another",
            ds.name,
            own.label(),
            unwritable(&ext, own)
        )
    })
}

/// Why a system can't be written to a file with extension `ext` ([`check_projected`]).
fn unwritable(ext: &str, crs: Crs) -> &'static str {
    if !matches!(ext, "parquet" | "geoparquet") {
        "has no EPSG code and no WKT definition RustGIS can write"
    } else if crs.epsg() == 0 {
        "has no EPSG code"
    } else {
        "has no PROJJSON definition RustGIS can write"
    }
}

/// The system an export to `path` is written in when none is asked for, and what is said of it,
/// in a project whose coordinate system is `project` (#2032 C3a): as [`default_export_crs`] and
/// [`default_export_note`] for a dataset with a system of its own, or in a project without one. A
/// dataset with none of its own, in longitude/latitude, is written in the project's where the
/// format can say it, and that is said; in WGS 84, said, when the format can't hold that system.
/// Places that system can't hold are written as any export into it writes them (at 85.05° in Web
/// Mercator, said).
pub fn default_export_in(ds: &Dataset, path: &str, project: Option<Crs>) -> (Crs, Option<String>) {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let project = project.filter(|p| {
        ds.crs.is_none()
            && ds.coord_crs() == Crs::LonLat
            && *p != Crs::LonLat
            && !p.is_geocentric()
            && PROJECTED_FORMATS.contains(&ext.as_str())
    });
    let Some(p) = project else { return (default_export_crs(ds, &ext), default_export_note(ds, path)) };
    let in_wgs84 = |why: String| {
        (
            Crs::LonLat,
            Some(format!(
                "{path}: written in WGS 84 (EPSG:4326), not the project's coordinate system ({}), as {why}; give a \"crs\" to write another",
                p.label()
            )),
        )
    };
    if check_projected(&ext, p).is_err() {
        return in_wgs84(format!("that {}", unwritable(&ext, p)));
    }
    (p, Some(format!("{path}: written in {}, the project's coordinate system, as {} has none of its own", p.label(), ds.name)))
}

/// The dataset in `crs`'s coordinates, from the system it holds them in ([`Dataset::coord_crs`]),
/// by the direct operation between the two ([`Crs::to`]). A copy as it is when it holds them in
/// `crs`. Fails on a coordinate the system can't hold, naming its feature.
pub fn project(ds: &Dataset, crs: Crs) -> Result<Dataset> {
    let from = ds.coord_crs();
    if crs == from {
        return Ok(ds.clone());
    }
    let mut out = ds.clone();
    let to = from.to(crs).map_err(DataError::Format)?;
    project_features(&mut out.features, &to)?;
    out.held_in = crs;
    Ok(out)
}

/// Converts features' geometry in place from `to`'s source system to its target, as a step of the
/// progress. Fails on a coordinate the target can't hold, naming its feature.
pub(crate) fn project_features(features: &mut [crate::Feature], to: &crate::raster::Reprojection) -> Result<()> {
    crate::progress::begin(features.len());
    project_each(features, to, true)
}

/// [`project`] for an export: a feature with a place `crs` can't hold keeps its attributes and is
/// written without its geometry, rather than failing the export; returns how many were
/// ([`unplaced_export_note`]).
pub fn project_leaving_out(ds: &Dataset, crs: Crs) -> Result<(Dataset, usize)> {
    let from = ds.coord_crs();
    if crs == from {
        return Ok((ds.clone(), 0));
    }
    let mut out = ds.clone();
    let to = from.to(crs).map_err(DataError::Format)?;
    crate::progress::begin(out.features.len());
    let unplaced = project_each_leaving_out(&mut out.features, &to, true)?;
    out.held_in = crs;
    Ok((out, unplaced))
}

/// [`project_each`] for an export: a feature with a place the target can't hold loses its
/// geometry rather than failing; returns how many did. A coordinate that is missing already (NaN,
/// infinite) stays missing, as [`crate::Dataset::in_lonlat`] keeps it, and loses nothing.
pub(crate) fn project_each_leaving_out(features: &mut [crate::Feature], to: &crate::raster::Reprojection, count: bool) -> Result<usize> {
    let mut unplaced = 0usize;
    for f in features {
        if count {
            crate::progress::step(1)?;
        } else {
            crate::progress::check()?;
        }
        if let Some(g) = f.geometry.take() {
            // A coordinate that isn't a number is kept as it is: converting it could give it a
            // place (Web Mercator takes an infinite latitude to 85.05°).
            let out = if to.is_identity() {
                g.clone()
            } else {
                g.map_coords(&|c| if c.is_finite() { to.point(c).unwrap_or(Coord::new(f64::NAN, f64::NAN)) } else { c })
            };
            if lost_a_place(&g, &out) {
                unplaced += 1;
            } else {
                f.geometry = Some(out);
            }
        }
    }
    Ok(unplaced)
}

/// Whether `out`, `g` converted place for place, lost a place: one that was a number in `g` and
/// isn't in `out`.
fn lost_a_place(g: &Geometry, out: &Geometry) -> bool {
    let mut was = Vec::new();
    g.for_each_coord(|c| was.push(c.is_finite()));
    let mut k = 0usize;
    let mut lost = false;
    out.for_each_coord(|c| {
        lost |= was.get(k).copied().unwrap_or(true) && !c.is_finite();
        k += 1;
    });
    lost
}

/// What an export to `target` says of the `n` features of `layer` with a place `crs` can't hold
/// ([`project_leaving_out`]): their geometry is left out, and in a format that holds only
/// features with geometry the feature too. `None` for none.
pub fn unplaced_export_note(target: &str, layer: &str, n: usize, crs: Crs) -> Option<String> {
    let system = if crs == Crs::LonLat { "WGS 84 (EPSG:4326)".to_string() } else { crs.label() };
    (n > 0).then(|| {
        format!("{target}: {n} feature(s) of {layer} have a place that can't be given in {system}, so their geometry is left out (and the feature, in a format that holds only features with geometry)")
    })
}

/// [`project_features`] without a step of its own: it counts each feature in the step under way
/// when `count` is set, and otherwise only stops once the run is cancelled.
pub(crate) fn project_each(features: &mut [crate::Feature], to: &crate::raster::Reprojection, count: bool) -> Result<()> {
    for f in features {
        if count {
            crate::progress::step(1)?;
        } else {
            crate::progress::check()?;
        }
        if let Some(g) = f.geometry.take() {
            let Some(p) = to.whole_geometry(&g) else {
                return Err(DataError::Format(format!("feature {} can't be placed in {}", f.id, to.target().label())));
            };
            f.geometry = Some(p);
        }
    }
    Ok(())
}

/// Defines the CRS of the dataset's native coordinates without changing those numeric values: a
/// relabel. Its coordinates as the file held them (in the system it is held in, else the one it was
/// read in, [`Dataset::crs`], or longitude/latitude without one) are held in `crs` as they are; a
/// dataset held in longitude/latitude while it says another system (a project saved by an older
/// release) has its coordinates brought back to that system first. Refused when a place can't be
/// given in longitude/latitude from `crs`. Builds a copy so a refusal leaves the input intact.
/// Source geometry discarded during import cannot be recovered this way.
pub fn define_projection(ds: &Dataset, crs: Crs) -> Result<Dataset> {
    if ds.crs_incomplete {
        return Err(DataError::Format("geometry was lost during import; import the original file again with a crs override".into()));
    }
    // The system its numbers are in: the one it is held in, or, for a layer held in
    // longitude/latitude, the one it was read in (a project saved by an older release).
    let from = match ds.coord_crs() {
        Crs::LonLat => ds.crs.unwrap_or(Crs::LonLat),
        held => held,
    };
    // As they are while held in the system it was read in, as a file read now is.
    let mut out = project(ds, from)?;
    let to = crs.to(Crs::LonLat).map_err(DataError::Format)?;
    for f in &out.features {
        if let Some(g) = &f.geometry {
            let lonlat = to.geometry(g);
            let mut valid = true;
            lonlat.for_each_coord(|c| valid &= c.is_finite() && c.x.abs() <= 180.0 && c.y.abs() <= 90.0);
            if !valid {
                return Err(DataError::Format(format!(
                    "feature {} can't be interpreted in {}; import the original file with a crs override",
                    f.id,
                    crs.label()
                )));
            }
        }
    }
    out.held_in = crs;
    out.crs = Some(crs);
    Ok(out)
}

/// Refuses a system for a GeoParquet that has no EPSG code ([`crate::source::parquet::projjson`]),
/// and for a GeoPackage or FlatGeobuf that the file couldn't declare: one without an
/// EPSG code is declared by its WKT definition, and without one (a projection WKT1 has no name
/// for) the file would read as longitude/latitude.
pub(crate) fn check_projected(ext: &str, crs: Crs) -> Result<()> {
    // GeoParquet says a system by PROJJSON with its EPSG code, which every release reads.
    if matches!(ext, "parquet" | "geoparquet") && crs != Crs::LonLat && crate::source::parquet::projjson(crs).is_none() {
        return Err(DataError::Unsupported(format!(".{ext} in {}: {}", crs.label(), crate::source::parquet::projjson_refusal(crs))));
    }
    if crs.epsg() == 0 && matches!(ext, "gpkg" | "fgb") && crate::raster::wkt::to_wkt(crs).is_none() {
        return Err(DataError::Unsupported(format!(
            ".{ext} in {}: it has no EPSG code or WKT definition; give the system as an EPSG code",
            crs.label()
        )));
    }
    Ok(())
}

/// As [`write_bytes`], in the coordinate system `crs`: a GeoPackage, Shapefile or FlatGeobuf (see
/// [`PROJECTED_FORMATS`]) says it; any other format is written only in longitude/latitude.
/// Refused on a place `crs` can't hold, naming its feature; [`write_bytes_noted`] writes the
/// feature without its geometry instead, and says so.
pub fn write_bytes_in(ds: &Dataset, ext: &str, crs: Crs) -> Result<(Vec<u8>, usize)> {
    let ext = ext.to_ascii_lowercase();
    if crs != Crs::LonLat {
        if !holds_a_system(&ext) {
            return Err(DataError::Unsupported(format!(".{ext} in {} (it holds longitude/latitude only)", crs.label())));
        }
        check_projected(&ext, crs)?;
    }
    // From the system the dataset holds its coordinates in: as they are when that is `crs`.
    if crs == ds.coord_crs() {
        return projected_bytes(ds, &ext, crs);
    }
    projected_bytes(&project(ds, crs)?, &ext, crs)
}

/// [`write_bytes_in`] for an export (named `name` in its notes): a feature with a place `crs`
/// can't hold is written without its geometry ([`project_leaving_out`]), and the notes say how
/// many ([`unplaced_export_note`]).
pub fn write_bytes_noted(ds: &Dataset, ext: &str, crs: Crs, name: &str) -> Result<(Vec<u8>, usize, Vec<String>)> {
    let ext = ext.to_ascii_lowercase();
    if crs != Crs::LonLat {
        if !holds_a_system(&ext) {
            return Err(DataError::Unsupported(format!(".{ext} in {} (it holds longitude/latitude only)", crs.label())));
        }
        check_projected(&ext, crs)?;
    }
    if crs == ds.coord_crs() {
        let (bytes, left_out) = projected_bytes(ds, &ext, crs)?;
        return Ok((bytes, left_out, Vec::new()));
    }
    let (projected, unplaced) = project_leaving_out(ds, crs)?;
    let (bytes, left_out) = projected_bytes(&projected, &ext, crs)?;
    Ok((bytes, left_out, unplaced_export_note(name, &ds.name, unplaced, crs).into_iter().collect()))
}

/// [`write_bytes_in`] for a dataset already in `crs`'s coordinates.
fn projected_bytes(ds: &Dataset, ext: &str, crs: Crs) -> Result<(Vec<u8>, usize)> {
    crate::progress::begin(ds.features.len());
    Ok(match ext {
        "geojson" | "json" => {
            let mut w = GeoJsonWriter::new(Vec::new(), &ds.name, &ds.fields)?;
            w.write(&ds.features)?;
            (w.finish()?, 0)
        }
        "csv" => (to_csv_in(ds, crs)?.into_bytes(), 0),
        "fgb" => (crate::source::fgb::write_in(ds, crs)?, without_geometry(ds)),
        "parquet" | "geoparquet" => (crate::source::parquet::write_in(ds, crs)?, 0),
        "kml" => (crate::interop::to_kml(ds, name_field(ds))?.into_bytes(), without_geometry(ds)),
        "kmz" => {
            let kml = crate::interop::to_kml(ds, name_field(ds))?.into_bytes();
            (crate::zip::write(&[("doc.kml".to_string(), kml)]).map_err(DataError::Format)?, without_geometry(ds))
        }
        "gpx" => (crate::interop::to_gpx(ds, name_field(ds))?.into_bytes(), gpx_left_out(ds)),
        "shp" | "zip" => {
            let (parts, left_out) = shapefile_parts_in(ds, crs)?;
            let base: String = ds.name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ') { c } else { '_' }).collect();
            let base = if base.trim().is_empty() { "layer".to_string() } else { base };
            let entries: Vec<(String, Vec<u8>)> = parts.into_iter().map(|(e, b)| (format!("{base}.{e}"), b)).collect();
            (crate::zip::write(&entries).map_err(DataError::Format)?, left_out)
        }
        other => return Err(DataError::Unsupported(other.to_string())),
    })
}

/// Writes a dataset by extension (`.geojson`/`.json`, `.csv`, `.fgb`, `.gpkg` on the desktop,
/// `.parquet`/`.geoparquet`, `.shp`, `.zip`, `.kml`, `.kmz` or `.gpx`); returns the number of features the
/// format couldn't hold (FlatGeobuf, KML, GPX and Shapefile hold only features with geometry). A
/// Shapefile is written as its `.shp`, `.shx`, `.dbf`, `.prj` and `.cpg` files (or zipped, to a
/// `.zip`). GeoPackage exports require a new path and never replace an existing file.
pub fn write_path(ds: &Dataset, path: &Path) -> Result<usize> {
    write_path_in(ds, path, Crs::LonLat)
}

/// How a GeoPackage export treats a GeoPackage already at its path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GpkgMode {
    /// Writes a new file; an existing one is refused, as it may hold other tables.
    #[default]
    NewFile,
    /// Adds the table to the GeoPackage (a new file when there is none); refuses a table of its
    /// name.
    AddTable,
    /// Replaces the GeoPackage's feature or attribute table of its name, which must be there.
    ReplaceTable,
}

/// Where a GeoPackage export puts its table: how, and under what name (default: the layer's).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpkgTable {
    pub mode: GpkgMode,
    pub name: Option<String>,
}

/// The table a GeoPackage export wrote: its name in the file (a reserved name is prefixed),
/// whether it replaced one, and the views and triggers on other tables that name a replaced one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpkgWritten {
    pub table: String,
    pub replaced: bool,
    pub dependents: Vec<String>,
}

/// As [`write_path`], in the coordinate system `crs` (a GeoPackage, Shapefile or FlatGeobuf; see
/// [`write_bytes_in`]). Refused on a place `crs` can't hold, naming its feature, as it gives no
/// notes to say it; [`write_path_to`] writes the feature without its geometry instead, and says so.
pub fn write_path_in(ds: &Dataset, path: &Path, crs: Crs) -> Result<usize> {
    write_path_as(ds, path, crs, &GpkgTable::default(), false).map(|(left_out, _, _)| left_out)
}

/// As [`write_path_in`], a GeoPackage's table placed as `gpkg` says (other formats ignore it):
/// also returns the table written, and notes to give with the export (a Shapefile's old parts it
/// couldn't remove once the new ones landed, [`kept_backups_note`]). A feature with a place `crs`
/// can't hold is written without its geometry, and the notes say how many
/// ([`unplaced_export_note`]).
pub fn write_path_to(ds: &Dataset, path: &Path, crs: Crs, gpkg: &GpkgTable) -> Result<(usize, Option<GpkgWritten>, Vec<String>)> {
    write_path_as(ds, path, crs, gpkg, true)
}

/// [`write_path_to`], leaving out a geometry `crs` can't hold (`leaving_out`, said in the notes)
/// or refusing it ([`write_path_in`]).
fn write_path_as(
    ds: &Dataset,
    path: &Path,
    crs: Crs,
    gpkg: &GpkgTable,
    leaving_out: bool,
) -> Result<(usize, Option<GpkgWritten>, Vec<String>)> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if crs != Crs::LonLat && PROJECTED_FORMATS.contains(&ext.as_str()) {
        check_projected(&ext, crs)?;
    }
    let projected;
    // From the system the dataset holds its coordinates in ([`project_leaving_out`]): as they are
    // when that is `crs`, and when `crs` is one the format can't hold, which is refused below. A
    // feature with a place `crs` can't hold is written without its geometry, and said.
    let mut unplaced = 0;
    let ds = if crs == ds.coord_crs() || (crs != Crs::LonLat && !holds_a_system(&ext)) {
        ds
    } else {
        (projected, unplaced) = if leaving_out { project_leaving_out(ds, crs)? } else { (project(ds, crs)?, 0) };
        &projected
    };
    let unplaced = unplaced_export_note(&path.display().to_string(), &ds.name, unplaced, crs);
    match ext.as_str() {
        "gpkg" => {
            #[cfg(not(target_arch = "wasm32"))]
            {
                crate::progress::begin(ds.features.len());
                // Every feature is written: a layer of several kinds of geometry gets a GEOMETRY
                // column, and one that declares no type takes its geometry's.
                let table = crate::source::gpkg::write_table_in(ds, path, crs, gpkg)?;
                Ok((0, Some(table), unplaced.into_iter().collect()))
            }
            #[cfg(target_arch = "wasm32")]
            {
                let _ = gpkg;
                Err(DataError::Unsupported("GeoPackage in the browser".into()))
            }
        }
        "shp" => {
            crate::progress::begin(ds.features.len());
            let (parts, left_out) = shapefile_parts_in(ds, crs)?;
            let parts: Vec<(std::path::PathBuf, Vec<u8>)> = parts.into_iter().map(|(e, bytes)| (shapefile_part(path, e), bytes)).collect();
            let kept = place_shapefile(path, &stage_shapefile(&parts)?)?;
            Ok((left_out, None, unplaced.into_iter().chain(kept_backups_note(path, &kept)).collect()))
        }
        _ if crs != Crs::LonLat && !holds_a_system(&ext) => {
            Err(DataError::Unsupported(format!(".{ext} in {} (it holds longitude/latitude only)", crs.label())))
        }
        _ => {
            let (bytes, left_out) = projected_bytes(ds, &ext, crs)?;
            write_atomic(path, &bytes)?;
            Ok((left_out, None, unplaced.into_iter().collect()))
        }
    }
}

/// The path of the part with extension `ext` of the Shapefile at `path`: the `.shp` keeps the
/// name given (`Roads.SHP` stays so); the others take its stem.
pub(crate) fn shapefile_part(path: &Path, ext: &str) -> std::path::PathBuf {
    if ext == "shp" { path.to_path_buf() } else { path.with_extension(ext) }
}

/// Writes each of a Shapefile's parts (its path and bytes) to a temporary file beside it, synced
/// to the disk; returns each temporary file with the part it becomes. Nothing is left behind
/// when one fails or the run is cancelled (see [`place_shapefile`]).
pub(crate) fn stage_shapefile(parts: &[(std::path::PathBuf, Vec<u8>)]) -> Result<Vec<(std::path::PathBuf, std::path::PathBuf)>> {
    let mut written: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    let staged = parts.iter().try_for_each(|(target, bytes)| {
        let tmp = temp_beside(target)?;
        let result = write_new_like(&tmp, target, bytes);
        if result.is_ok() {
            written.push((tmp, target.clone()));
        }
        result
    });
    // A cancel before the parts land leaves the old Shapefile as it was.
    if let Err(e) = staged.map_err(DataError::from).and_then(|()| crate::progress::check()) {
        unstage(&written);
        return Err(e);
    }
    Ok(written)
}

/// Removes the temporary files [`stage_shapefile`] wrote.
pub(crate) fn unstage(staged: &[(std::path::PathBuf, std::path::PathBuf)]) {
    for (tmp, _) in staged {
        let _ = std::fs::remove_file(tmp);
    }
}

/// Renames each staged part of the Shapefile at `path` ([`stage_shapefile`]) into place, the
/// `.shp` last, so a failed write leaves the old Shapefile whole: each old part is moved aside
/// first and put back if any new part fails to land, and a Cancel before the last part lands
/// rolls back the same way. Once all have landed, the old parts and the spatial indexes that
/// would no longer match (`.qix`, `.sbn`, `.sbx`) are removed.
///
/// Returns the old parts that landed but couldn't be removed afterwards, each as the hidden copy
/// that holds it ([`kept_backups_note`] says them). When a part fails to land and every old part is
/// put back, the error names `path` and says nothing changed.
pub(crate) fn place_shapefile(path: &Path, staged: &[(std::path::PathBuf, std::path::PathBuf)]) -> Result<Vec<std::path::PathBuf>> {
    let mut written: Vec<&(std::path::PathBuf, std::path::PathBuf)> = staged.iter().collect();
    written.sort_by_key(|(_, target)| target == path);
    let mut backups: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    let mut placed: Vec<std::path::PathBuf> = Vec::new();
    let mut result = Ok(());
    for (tmp, target) in written {
        // A cancel before the last part lands rolls back like a failed rename.
        if crate::progress::check().is_err() {
            result = Err(std::io::Error::other(DataError::Cancelled));
            break;
        }
        if target.exists() {
            // Any failure here rolls back like a failed rename.
            let bak = match temp_beside(target) {
                Ok(b) => b,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            };
            // Reserve this name before moving the old part: a previous crash's backup
            // (or a symlink) must never be replaced when a process id is reused.
            if let Err(e) = std::fs::OpenOptions::new().write(true).create_new(true).open(&bak).map(drop) {
                result = Err(e);
                break;
            }
            if let Err(e) = std::fs::rename(target, &bak) {
                let _ = std::fs::remove_file(&bak);
                result = Err(e);
                break;
            }
            backups.push((bak, target.clone()));
        }
        if let Err(e) = std::fs::rename(tmp, target) {
            result = Err(e);
            break;
        }
        placed.push(target.clone());
    }
    if let Err(e) = result {
        // A new part that can't be removed, where no old one is put back over it, is named.
        let mut kept = Vec::new();
        for p in &placed {
            if std::fs::remove_file(p).is_err() && !backups.iter().any(|(_, target)| target == p) {
                kept.push(format!("the new {} is left", p.display()));
            }
        }
        // An old part that can't be put back is named, so it isn't lost under its temporary name.
        for (bak, target) in &backups {
            if std::fs::rename(bak, target).is_err() {
                kept.push(format!("{} is kept as {}", target.display(), bak.display()));
            }
        }
        unstage(staged);
        // The old parts put back, durably too.
        sync_folder(path);
        let e = DataError::from(e);
        if kept.is_empty() {
            // With no old file, there was nothing to put back.
            let outcome = if backups.is_empty() { "nothing was written" } else { "the old file was put back, nothing changed" };
            // Its kind kept, for a caller that tells failures apart by it.
            return Err(match e {
                DataError::Io(e) => DataError::Io(std::io::Error::new(e.kind(), format!("{}: {e}; {outcome}", path.display()))),
                e => e,
            });
        }
        return Err(DataError::Format(format!("{}: {e}; undoing the write failed: {}", path.display(), kept.join(", "))));
    }
    // A copy that can't be removed is returned, to be said: it holds an old part beside the file.
    let left: Vec<std::path::PathBuf> = backups.into_iter().filter(|(bak, _)| remove_backup(bak).is_err()).map(|(bak, _)| bak).collect();
    // A spatial index left from an older file of the same name would no longer match.
    for stale in ["qix", "sbn", "sbx"] {
        // Sidecars may use any letter case, even beside a lowercase `.shp`.
        for capitals in 0..8 {
            let extension: String = stale
                .bytes()
                .enumerate()
                .map(|(i, b)| char::from(if capitals & (1 << i) == 0 { b } else { b.to_ascii_uppercase() }))
                .collect();
            let _ = std::fs::remove_file(path.with_extension(extension));
        }
    }
    // Every part is renamed within `path`'s folder.
    sync_folder(path);
    Ok(left)
}

#[cfg(test)]
thread_local! {
    /// Makes [`remove_backup`] fail on this thread, for the tests of a copy that can't be removed
    /// (no permission does that alone: the folder must still take the renames).
    static KEEP_BACKUPS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Removes an old part's copy once the new parts have landed ([`place_shapefile`]).
fn remove_backup(bak: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if KEEP_BACKUPS.get() {
        return Err(std::io::Error::other("kept for the test"));
    }
    std::fs::remove_file(bak)
}

/// What is said of the old parts of the Shapefile at `path` that [`place_shapefile`] couldn't
/// remove once the new ones landed; `None` when there are none.
pub(crate) fn kept_backups_note(path: &Path, kept: &[std::path::PathBuf]) -> Option<String> {
    (!kept.is_empty()).then(|| {
        let copies: Vec<String> = kept.iter().map(|k| k.display().to_string()).collect();
        format!(
            "{}: {} old part(s) couldn't be removed after the new ones landed; they are kept as {} and can be deleted",
            path.display(),
            kept.len(),
            copies.join(", ")
        )
    })
}

/// Makes a rename into `path`'s folder survive a power cut: on Linux and macOS a synced file can
/// still come back under its old name until the folder itself is synced. Best effort, after the
/// rename: the file is written either way, so a folder that can't be opened (writable but not
/// readable) or synced (some network and FUSE mounts refuse it) leaves the rename to land when
/// the system writes it, as every rename did before, and the write still succeeds. Windows can't
/// open a folder this way without unsafe code, so there NTFS's own journal decides when it lands;
/// the browser has no folders to sync.
pub(crate) fn sync_folder(path: &Path) {
    #[cfg(unix)]
    {
        let folder = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        let _ = std::fs::File::open(folder).and_then(|f| f.sync_all());
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// A new temporary name beside `path`, unique per call, so concurrent writes to the same target
/// never share one.
pub(crate) fn temp_beside(path: &Path) -> std::io::Result<std::path::PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let Some(name) = path.file_name() else {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{} is not a file path", path.display())));
    };
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}.{}.tmp", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    Ok(path.with_file_name(tmp_name))
}

/// Writes and syncs a new file (`create_new`, so an existing file or symlink is never followed);
/// removes it on failure.
#[cfg(test)]
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_new_with(path, |_, w| w.write_all(bytes))
}

/// [`write_new`], the new file taking the mode of the file at `like` (on Unix, when there is one)
/// before any content is written: a private part (`0600`) stays private once the new file
/// replaces it, and its content is never readable by others meanwhile. A mode that can't be set
/// is an error, rather than a part made readable.
fn write_new_like(path: &Path, like: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    let mode = match std::fs::symlink_metadata(like) {
        Ok(old) if !old.file_type().is_symlink() => Some(old.permissions()),
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    #[cfg(not(unix))]
    let _ = like;
    write_new_with(path, |file, w| {
        // Through the handle opened, not the path, which could name another file by now.
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            if file.metadata()?.permissions().mode() != mode.mode() {
                file.set_permissions(mode)?;
            }
        }
        #[cfg(not(unix))]
        let _ = file;
        w.write_all(bytes)
    })
}

/// [`write_new`] with the content written by `write`, through a buffer, as it goes; `write` is
/// also given the file opened, before anything is written to it.
fn write_new_with(path: &Path, write: impl FnOnce(&std::fs::File, &mut dyn std::io::Write) -> std::io::Result<()>) -> std::io::Result<()> {
    use std::io::Write;
    let f = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    let res = (|| {
        let mut w = std::io::BufWriter::new(&f);
        write(&f, &mut w)?;
        w.flush()?;
        drop(w);
        f.sync_all()
    })();
    // Close before removing a failed write, including on Windows.
    drop(f);
    if res.is_err() {
        let _ = std::fs::remove_file(path);
    }
    res
}

/// Writes through a temporary file that is synced and renamed into place, so an interrupted
/// write never truncates an existing file. The temporary file is removed on failure. A symbolic
/// link is written through, and an existing file keeps its permissions. For app-managed files
/// whose destination link must be replaced instead, use [`replace_atomic`]. On Unix, failure to
/// inspect or preserve an existing file's permissions is an error and leaves that file unchanged.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    write_atomic_with(path, |w| w.write_all(bytes))
}

/// [`write_atomic`] with the content written by `write` as it goes, so a long text or a layer
/// written a chunk at a time needn't be held whole first. A failure in `write` leaves an existing
/// file as it was. Returns what `write` returns.
pub fn write_atomic_with<T, E: From<std::io::Error>>(
    path: &Path,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
    let path = link_target(path)?;
    atomic_write_at(path, write)
}

/// Atomically replaces an app-managed file, replacing a final symbolic link rather than
/// writing through it. Parent directories must be trusted, as for [`write_atomic`]. Existing
/// regular files keep their permissions; a replaced link uses a new file's permissions.
pub fn replace_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    replace_atomic_with(path, |out| out.write_all(bytes))
}

/// [`replace_atomic`] with the content written through a buffer, without concatenating it first.
pub fn replace_atomic_with<T, E: From<std::io::Error>>(
    path: &Path,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
    atomic_write_at(path.to_path_buf(), write)
}

/// Writes and syncs bytes beside the destination, preserving the destination until
/// [`StagedWrite::commit`] installs them. Dropping the result discards the staged file.
pub fn stage_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<StagedWrite> {
    use std::io::Write;
    stage_at(link_target(path)?, 8 << 10, |w| w.write_all(bytes)).map(|(staged, ())| staged)
}

/// [`stage_atomic`] for a write that streams: `write` fills the file beside the destination (a
/// symbolic link's target), which [`StagedWrite::commit`] installs; dropping the result discards it.
pub fn stage_streamed<T, E: From<std::io::Error>>(
    path: &Path,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> std::result::Result<T, E>,
) -> std::result::Result<(StagedWrite, T), E> {
    // A streamed write (a raster's tiles) goes through a buffer of a megabyte.
    stage_at(link_target(path)?, 1 << 20, write)
}

/// [`stage_streamed`] to a destination nothing holds yet: it is reserved first, an empty file
/// made in its place only if nothing is there (an `AlreadyExists` error otherwise, before
/// anything is written), so two writes never pick the same file. [`StagedWrite::commit`] puts
/// the file there; dropping the result, or an error, removes the reservation, and `folder` (one
/// the caller just made for it) when nothing else is in it.
pub fn stage_streamed_new<T, E: From<std::io::Error>>(
    path: &Path,
    folder: Option<&Path>,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> std::result::Result<T, E>,
) -> std::result::Result<(StagedWrite, T), E> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut held = Reserved { path: Some(path.to_path_buf()), folder: folder.map(Path::to_path_buf) };
    let (mut staged, value) = stage_at(path.to_path_buf(), 1 << 20, write)?;
    (staged.reserved, staged.folder) = (held.path.take(), held.folder.take());
    Ok((staged, value))
}

/// A destination reserved for a write ([`stage_streamed_new`]), and a folder made for it, removed
/// while the write isn't installed: the reservation while it is still empty, the folder while
/// nothing else is in it.
struct Reserved {
    path: Option<std::path::PathBuf>,
    folder: Option<std::path::PathBuf>,
}

impl Drop for Reserved {
    fn drop(&mut self) {
        unreserve(self.path.take(), self.folder.take());
    }
}

/// Removes a reserved destination still empty, then `folder` if nothing else is in it.
fn unreserve(path: Option<std::path::PathBuf>, folder: Option<std::path::PathBuf>) {
    if let Some(p) = path
        && std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file() && m.len() == 0)
    {
        let _ = std::fs::remove_file(p);
    }
    if let Some(f) = folder {
        let _ = std::fs::remove_dir(f);
    }
}

/// Stages and installs a write, retaining the old file on error or cancellation.
fn atomic_write_at<T, E: From<std::io::Error>>(
    path: std::path::PathBuf,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> std::result::Result<T, E>,
) -> std::result::Result<T, E> {
    let (staged, value) = stage_at(path, 8 << 10, write)?;
    staged.commit()?;
    Ok(value)
}

/// Syncs the temporary file on the writing thread, so installation only needs a rename
/// and a directory sync.
fn stage_at<T, E: From<std::io::Error>>(
    path: std::path::PathBuf,
    buffer: usize,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> std::result::Result<T, E>,
) -> std::result::Result<(StagedWrite, T), E> {
    use std::io::Write;
    let tmp = temp_beside(&path)?;
    // Opened before the guard, so a name already taken is never removed.
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    let staged = StagedWrite { path, tmp: Some(tmp), reserved: None, folder: None };
    // The new file takes the old one's mode before any content is written, so a private file
    // (0600) never becomes readable by others. On Windows a new file inherits its folder's access
    // rules, and copying a read-only flag would only make the rename fail.
    #[cfg(unix)]
    match std::fs::symlink_metadata(&staged.path) {
        Ok(old) if !old.file_type().is_symlink() => {
            use std::os::unix::fs::PermissionsExt;
            let permissions = old.permissions();
            // Mounts that cannot chmod need no syscall when the mode already matches.
            // If preserving a different mode fails, refuse the save instead of widening it.
            if file.metadata()?.permissions().mode() != permissions.mode() {
                file.set_permissions(permissions)?;
            }
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut out = std::io::BufWriter::with_capacity(buffer, file);
    let value = write(&mut out)?;
    out.flush()?;
    out.get_ref().sync_all()?;
    // Closed before the rename, including on Windows.
    drop(out);
    Ok((staged, value))
}

/// An atomic write awaiting installation. An error or drop removes its temporary file, and what
/// [`stage_streamed_new`] reserved for it.
pub struct StagedWrite {
    path: std::path::PathBuf,
    tmp: Option<std::path::PathBuf>,
    /// The destination reserved, and the folder made for it ([`stage_streamed_new`]).
    reserved: Option<std::path::PathBuf>,
    folder: Option<std::path::PathBuf>,
}

impl StagedWrite {
    /// The destination resolved at staging time, after following any symbolic links.
    pub fn destination(&self) -> &Path {
        &self.path
    }

    /// Installs the staged file, checking Cancel just before the rename. The previous file
    /// remains in place if installation fails.
    pub fn commit(mut self) -> std::io::Result<()> {
        if crate::progress::check().is_err() {
            return Err(std::io::Error::other(DataError::Cancelled));
        }
        if let Some(tmp) = &self.tmp {
            std::fs::rename(tmp, &self.path)?;
            self.tmp = None;
            (self.reserved, self.folder) = (None, None);
            sync_folder(&self.path);
        }
        Ok(())
    }
}

impl Drop for StagedWrite {
    fn drop(&mut self) {
        if let Some(tmp) = &self.tmp {
            let _ = std::fs::remove_file(tmp);
        }
        unreserve(self.reserved.take(), self.folder.take());
    }
}

/// The file a write to `path` replaces: `path` itself, or, when it is a symbolic link, the file
/// its chain of links ends at (which need not exist yet). A link's relative target counts from
/// the link's folder. More than [`MAX_LINKS`] links in a row, as in a loop, is an error.
fn link_target(path: &Path) -> std::io::Result<std::path::PathBuf> {
    let mut at = path.to_path_buf();
    for _ in 0..=MAX_LINKS {
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let next = std::fs::read_link(&at)?;
                // `join` keeps an absolute target as it is.
                at = at.parent().map_or_else(|| next.clone(), |folder| folder.join(&next));
            }
            Ok(_) => return Ok(at),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(at),
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("{} leads through more than {MAX_LINKS} symbolic links", path.display()),
    ))
}

/// How many symbolic links in a row [`link_target`] follows, as many as Linux does.
const MAX_LINKS: usize = 40;

// ---------------------------------------------------------------------------------------------
// GeoJSON (RFC 7946)

fn coord(v: &Json) -> Option<Coord> {
    let a = v.as_array()?;
    let c = Coord::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?);
    c.is_finite().then_some(c)
}

fn coords(v: &Json) -> Option<Vec<Coord>> {
    v.as_array()?.iter().map(coord).collect()
}

fn rings(v: &Json) -> Option<Vec<Vec<Coord>>> {
    v.as_array()?.iter().map(coords).collect()
}

/// Parses a GeoJSON geometry object. Collections must contain exactly one member, since
/// `Geometry` cannot represent a collection. Unwraps nested single-member collections without
/// recursion; rejects empty or multi-member collections rather than retaining partial geometry.
pub fn geometry_from_geojson(mut g: &Json) -> Option<Geometry> {
    while g.get("type")?.as_str()? == "GeometryCollection" {
        let [only] = g.get("geometries")?.as_array()?.as_slice() else {
            return None;
        };
        g = only;
    }
    let c = g.get("coordinates");
    match g.get("type")?.as_str()? {
        "Point" => coord(c?).map(Geometry::Point),
        "MultiPoint" => coords(c?).map(Geometry::MultiPoint),
        "LineString" => coords(c?).map(|l| Geometry::Line(vec![l])),
        "MultiLineString" => rings(c?).map(Geometry::Line),
        "Polygon" => rings(c?).map(|r| Geometry::Polygon(vec![r])),
        "MultiPolygon" => c?.as_array()?.iter().map(rings).collect::<Option<Vec<_>>>().map(Geometry::Polygon),
        _ => None,
    }
}

pub fn geometry_to_geojson(g: &Geometry) -> Json {
    fn c(c: &Coord) -> Json {
        json!([c.x, c.y])
    }
    fn l(v: &[Coord]) -> Json {
        Json::Array(v.iter().map(c).collect())
    }
    match g {
        Geometry::Point(p) => json!({"type": "Point", "coordinates": c(p)}),
        Geometry::MultiPoint(v) => json!({"type": "MultiPoint", "coordinates": l(v)}),
        Geometry::Line(parts) if let [only] = parts.as_slice() => json!({"type": "LineString", "coordinates": l(only)}),
        Geometry::Line(parts) => json!({"type": "MultiLineString", "coordinates": parts.iter().map(|p| l(p)).collect::<Vec<_>>()}),
        Geometry::Polygon(polys) if let [only] = polys.as_slice() => {
            json!({"type": "Polygon", "coordinates": only.iter().map(|r| l(r)).collect::<Vec<_>>()})
        }
        Geometry::Polygon(polys) => json!({"type": "MultiPolygon", "coordinates": polys
            .iter()
            .map(|rings| rings.iter().map(|r| l(r)).collect::<Vec<_>>())
            .collect::<Vec<_>>()}),
    }
}

/// Reads a GeoJSON FeatureCollection, Feature or bare geometry.
///
/// A FeatureCollection is read a feature at a time: each one is parsed, turned into a feature and
/// dropped before the next, so the whole document is never held as JSON values.
pub fn read_geojson(text: &str, name: &str) -> Result<(Dataset, ReadReport)> {
    let mut features = Collector::default();
    match stream_features(text, &mut features) {
        Ok(rest) => match rest.get("type").and_then(Json::as_str) {
            Some("FeatureCollection") => features.finish(name, &rest),
            Some(_) => read_geojson_value(Json::Object(rest), name),
            None => Err(DataError::Format("GeoJSON: missing \"type\"".into())),
        },
        // Not an object, or "features" not an array: read whole, as before.
        Err(_) => read_geojson_value(serde_json::from_str(text).map_err(|e| DataError::Format(format!("GeoJSON: {e}")))?, name),
    }
}

/// The longest `crs` name looked at; a longer one is reported, not parsed.
const CRS_NAME_MAX: usize = 256;

/// At most 80 characters of `s`, for a message.
fn clip(s: &str) -> String {
    let mut out: String = s.chars().take(80).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// The coordinate system a FeatureCollection declares with the legacy (GeoJSON 2008) `crs` member,
/// `{"type": "name", "properties": {"name": "urn:ogc:def:crs:EPSG::28992"}}`, with the transformer
/// to longitude/latitude: `None` for longitude/latitude (no member, `null`, `EPSG:4326`, `CRS84`)
/// and for a declaration that isn't understood, which is said in `messages`: coordinates are then
/// read as longitude/latitude, but never without saying so.
///
/// Only named EPSG codes and CRS84 are honoured (not links, WKT or PROJ strings in the name, nor
/// codes RustGIS doesn't know). The coordinates are x/y (easting/northing, longitude/latitude)
/// whatever the axis order of the EPSG definition, as the legacy GeoJSON text says.
///
/// An IAU code (another body's system, as GDAL writes `urn:ogc:def:crs:IAU_2015::49900`) is read
/// as longitude/latitude with a notice when it is planetocentric. Any other IAU code is an error,
/// because its coordinates would land in the wrong place ([`planetary::declared`]).
pub(crate) fn declared_crs(
    root: &Map<String, Json>,
    messages: &mut crate::loss::Notes,
) -> Result<Option<(Crs, crate::raster::Transformer)>> {
    let Some(decl) = root.get("crs").filter(|v| !v.is_null()) else { return Ok(None) };
    if let Some(name) = decl.get("properties").and_then(|p| p.get("name")).and_then(Json::as_str).filter(|n| n.len() <= CRS_NAME_MAX)
        && let Some((authority, code)) = iau_name(name)
        && let Some(found) = planetary::declared(Some(&authority), Some(code), None)
    {
        return match found {
            planetary::Declared::LonLat { note, .. } => {
                messages.push(format!("GeoJSON crs \"{}\": {note}", clip(name)));
                Ok(None)
            }
            planetary::Declared::Refused(e) => Err(DataError::Format(format!("GeoJSON crs \"{}\": {e}", clip(name)))),
        };
    }
    Ok(match legacy_crs(decl) {
        Ok(found) => found,
        Err(why) => {
            let shown = match decl.get("properties").and_then(|p| p.get("name")).and_then(Json::as_str) {
                Some(name) => format!("\"{}\"", clip(name)),
                None => match decl.get("type").and_then(Json::as_str) {
                    Some(kind) => format!("of type \"{}\"", clip(kind)),
                    None => "of an unknown shape".to_string(),
                },
            };
            messages.push(crate::loss::Note::loss(
                crate::loss::LossCode::AssumedCrs,
                format!("GeoJSON crs {shown}: {why}; coordinates are read as longitude/latitude"),
            ));
            None
        }
    })
}

/// The authority and code of a `crs` name whose authority is the IAU's: `IAU_2015:49900`,
/// `urn:ogc:def:crs:IAU_2015::49900`, `urn:ogc:def:crs:IAU:2015:49900` or
/// `http(s)://www.opengis.net/def/crs/IAU/2015/49900` (any case, whitespace around).
fn iau_name(name: &str) -> Option<(String, i64)> {
    let up = name.trim().to_ascii_uppercase();
    let rest = ["URN:OGC:DEF:CRS:", "HTTP://WWW.OPENGIS.NET/DEF/CRS/", "HTTPS://WWW.OPENGIS.NET/DEF/CRS/"]
        .iter()
        .find_map(|p| up.strip_prefix(p))
        .unwrap_or(&up);
    let authority = rest.split([':', '/']).next().filter(|a| planetary::is_iau(a))?;
    let code = rest.rsplit([':', '/']).next()?;
    if code.is_empty() || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((authority.to_string(), code.parse().ok()?))
}

fn legacy_crs(decl: &Json) -> std::result::Result<Option<(Crs, crate::raster::Transformer)>, String> {
    if decl.get("type").and_then(Json::as_str) != Some("name") {
        return Err("only named coordinate systems are supported".into());
    }
    let name = decl.get("properties").and_then(|p| p.get("name")).and_then(Json::as_str).ok_or("it has no properties.name")?;
    // Only the forms below, anchored (not any text holding "EPSG"), and no other text for
    // `Crs::parse`, which also takes WKT and PROJ strings (and registers a system for them).
    if name.len() > CRS_NAME_MAX || !is_epsg_or_crs84_name(name) {
        return Err("only EPSG codes and CRS84 are supported".into());
    }
    let crs = Crs::parse(name)?;
    if crs == Crs::LonLat {
        return Ok(None);
    }
    let t = crs.transformer()?;
    Ok(Some((crs, t)))
}

/// Whether a `crs` name is `EPSG:N`, `urn:ogc:def:crs:EPSG:<version, maybe empty>:N`,
/// `http(s)://www.opengis.net/def/crs/EPSG/<version>/N`, or CRS84 as `CRS84`, `OGC:CRS84`,
/// `urn:ogc:def:crs:OGC:<version, maybe empty>:CRS84` or
/// `http(s)://www.opengis.net/def/crs/OGC/<version>/CRS84` (any case, whitespace around).
pub(crate) fn is_epsg_or_crs84_name(name: &str) -> bool {
    let up = name.trim().to_ascii_uppercase();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let version = |s: &str| s.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    // `<version><sep><last>`: a version (maybe empty), then what's after the last separator.
    let tail = |rest: &str, sep: char| {
        rest.split_once(sep).filter(|(v, last)| version(v) && !last.contains(sep)).map(|(_, last)| last.to_string())
    };
    fn url(rest: &str) -> Option<&str> {
        rest.strip_prefix("HTTP://").or_else(|| rest.strip_prefix("HTTPS://")).and_then(|r| r.strip_prefix("WWW.OPENGIS.NET/DEF/CRS/"))
    }
    if let Some(code) = up.strip_prefix("EPSG:") {
        return digits(code);
    }
    if let Some(rest) = up.strip_prefix("URN:OGC:DEF:CRS:EPSG:") {
        return tail(rest, ':').is_some_and(|c| digits(&c));
    }
    if let Some(rest) = up.strip_prefix("URN:OGC:DEF:CRS:OGC:") {
        return tail(rest, ':').is_some_and(|c| c == "CRS84");
    }
    if let Some(rest) = url(&up).and_then(|r| r.strip_prefix("EPSG/")) {
        return tail(rest, '/').is_some_and(|c| digits(&c));
    }
    if let Some(rest) = url(&up).and_then(|r| r.strip_prefix("OGC/")) {
        return tail(rest, '/').is_some_and(|c| c == "CRS84");
    }
    // The shorthands: `CRS84`, `OGC:CRS84`, `OGC::CRS84`, `OGC:1.3:CRS84`.
    up == "CRS84" || up == "OGC:CRS84" || up.strip_prefix("OGC:").and_then(|rest| tail(rest, ':')).is_some_and(|c| c == "CRS84")
}

/// The box in longitude/latitude of `g`'s places, in the system `t` converts: `None` when one of
/// them has no place there (none that is finite). A reader keeps the geometry as the file holds
/// it, held in its system ([`Dataset::held_in`]), either way, and counts those with none
/// ([`kept_unplaced_note`]) and those whose box comes out beyond longitude/latitude
/// ([`beyond_lonlat_note`]). Each coordinate is converted to be checked, none is kept.
pub(crate) fn places_lonlat_extent(g: &Geometry, t: &crate::raster::Transformer) -> Option<rustgis_geom::Extent> {
    let mut ok = true;
    let mut e = rustgis_geom::Extent::EMPTY;
    g.for_each_coord(|c: Coord| match t.to_lonlat(c).filter(|l| l.is_finite()) {
        Some(l) => e.expand(l),
        None => ok = false,
    });
    ok.then_some(e)
}

/// What a reader says of the `n` of its `what` (features, shapes, rows) it keeps as the file holds
/// them, held in `crs`, though a place of theirs can't be given in longitude/latitude: they don't
/// draw (the map says so too), and a step that needs longitude/latitude leaves them out and says
/// so, or refuses naming one where leaving it out would lose it (a write-back, a sync of an
/// offline area holding it, a Field Package's list of changes) or a contract says so (the plugin
/// host's layers) (#2032 C2f-4b). `all` when none of the file's has a place there: then its system
/// doesn't seem to match its coordinates, and Define Projection can set the one that does, as
/// nothing was lost. `None` for none.
pub(crate) fn kept_unplaced_note(what: &str, n: usize, all: bool, crs: Crs) -> Option<String> {
    (n > 0).then(|| {
        if all {
            format!(
                "none of the {what} has a place in longitude/latitude from {}: it doesn't seem to be their coordinate system. They are kept as the file holds them, but don't draw; Define Projection can set the right one",
                crs.label()
            )
        } else {
            let system = crs.label();
            if n == 1 {
                format!("one of the {what} has a place in {system} that can't be given in longitude/latitude: it is kept as the file holds it, but doesn't draw")
            } else {
                format!("{n} {what} have a place in {system} that can't be given in longitude/latitude: they are kept as the file holds them, but don't draw")
            }
        }
    })
}

/// What a reader says of the `n` of its `what` whose places convert from `crs` but come out beyond
/// longitude/latitude (their box there is, [`rustgis_geom::Extent::beyond_lonlat`]): the system
/// may not be theirs, and they may draw out of place. `None` for none.
pub(crate) fn beyond_lonlat_note(what: &str, n: usize, crs: Crs) -> Option<String> {
    let system = crs.label();
    (n > 0).then(|| {
        if n == 1 {
            format!("one of the {what} has coordinates that come out beyond longitude/latitude once converted from {system}: it may not be their coordinate system, and it may draw out of place")
        } else {
            format!("{n} {what} have coordinates that come out beyond longitude/latitude once converted from {system}: it may not be their coordinate system, and they may draw out of place")
        }
    })
}

/// What takes a FeatureCollection's features as they're read.
pub(crate) trait FeatureSink {
    /// The `"features"` member starts (again, when a document repeats it: the last one wins).
    fn restart(&mut self);
    /// One element of `"features"`, as its text.
    fn feature(&mut self, raw: &str);
    /// Whether the read is to stop now (Cancel pressed): it then ends with an error.
    fn stopped(&self) -> bool {
        false
    }
}

/// Reads a GeoJSON document's top level, handing each element of `"features"` to `sink` as it is
/// read; returns the other members. Fails for a document that isn't an object, or whose
/// `"features"` isn't an array.
pub(crate) fn stream_features(text: &str, sink: &mut dyn FeatureSink) -> std::result::Result<Map<String, Json>, serde_json::Error> {
    use serde::de::DeserializeSeed;
    let mut de = serde_json::Deserializer::from_str(text);
    let rest = Streamed { sink }.deserialize(&mut de)?;
    // Only whitespace may follow, as when read whole.
    de.end()?;
    Ok(rest)
}

/// The top level of a GeoJSON document: its members, but for `"features"`, which go to a sink.
struct Streamed<'a> {
    sink: &'a mut dyn FeatureSink,
}

impl<'de> serde::de::DeserializeSeed<'de> for Streamed<'_> {
    type Value = Map<String, Json>;

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Streamed<'_> {
    type Value = Map<String, Json>;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a GeoJSON object")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> std::result::Result<Self::Value, A::Error> {
        let mut rest = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if key == "features" {
                self.sink.restart();
                map.next_value_seed(Features { sink: &mut *self.sink })?;
            } else {
                rest.insert(key, map.next_value()?);
            }
        }
        Ok(rest)
    }
}

/// A FeatureCollection's `"features"` array, a feature at a time.
struct Features<'a> {
    sink: &'a mut dyn FeatureSink,
}

impl<'de> serde::de::DeserializeSeed<'de> for Features<'_> {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> std::result::Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Features<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an array of features")
    }

    /// Hands each element to the sink as its text, ending with an error once the sink is stopped.
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
        while let Some(raw) = seq.next_element::<&'de serde_json::value::RawValue>()? {
            self.sink.feature(raw.get());
            if self.sink.stopped() {
                return Err(serde::de::Error::custom("cancelled"));
            }
        }
        Ok(())
    }
}

/// A GeoJSON layer's schema as features are read: the union of property keys in first-seen
/// order. A type stays unknown until a non-null value is seen (so `[null, 1, 2]` is Integer),
/// then widens; all-null columns become Text.
#[derive(Default)]
pub(crate) struct Schema {
    order: Vec<(String, Option<FieldType>)>,
    index: std::collections::HashMap<String, usize>,
}

impl Schema {
    /// Counts a property; returns its field's index.
    pub(crate) fn see(&mut self, k: &str, v: &Json) -> usize {
        let ty = json_type(v);
        match self.index.get(k) {
            Some(&i) => {
                if let Some((_, cur)) = self.order.get_mut(i) {
                    *cur = match *cur {
                        None => ty,
                        Some(c) => Some(widen(c, ty)),
                    };
                }
                i
            }
            None => {
                self.index.insert(k.to_string(), self.order.len());
                self.order.push((k.to_string(), ty));
                self.order.len() - 1
            }
        }
    }

    /// A property's field, once seen.
    pub(crate) fn index(&self, k: &str) -> Option<usize> {
        self.index.get(k).copied()
    }

    /// The fields, typed by every value seen.
    pub(crate) fn fields(&self) -> Vec<Field> {
        self.order.iter().map(|(k, ty)| Field::new(k.clone(), ty.unwrap_or(FieldType::Text))).collect()
    }
}

/// A feature's attributes from its properties, for `fields` (see [`Schema::fields`]).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn geojson_attrs(f: &Json, fields: &[Field]) -> Vec<Value> {
    let props = f.get("properties").and_then(Json::as_object);
    fields
        .iter()
        .map(|field| coerce(props.and_then(|p| p.get(&field.name)).map(Value::from_json).unwrap_or(Value::Null), field.ty))
        .collect()
}

/// Whether a feature's geometry has a third value in any position (a height, which reading drops),
/// a GeometryCollection's members included.
pub(crate) fn geojson_has_z(f: &Json) -> bool {
    /// Any position under `v` (nested arrays, ending in arrays of numbers) has a number beyond its
    /// first two values: a height or measure (a `null` there isn't one).
    fn positions(v: &Json) -> bool {
        match v.as_array() {
            Some(a) if a.first().is_some_and(Json::is_number) => a.iter().skip(2).any(Json::is_number),
            Some(a) => a.iter().any(positions),
            None => false,
        }
    }
    /// A geometry object, or a collection's members, has such a position.
    fn has_z(g: &Json) -> bool {
        match g.get("type").and_then(Json::as_str) {
            Some("GeometryCollection") => g.get("geometries").and_then(Json::as_array).is_some_and(|m| m.iter().any(has_z)),
            _ => g.get("coordinates").is_some_and(positions),
        }
    }
    f.get("geometry").is_some_and(has_z)
}

/// A feature's geometry: `Ok(None)` for none, `Err` for one that can't be read.
pub(crate) fn geojson_geometry(f: &Json) -> std::result::Result<Option<Geometry>, ()> {
    match f.get("geometry").filter(|g| !g.is_null()) {
        None => Ok(None),
        Some(g) => geometry_from_geojson(g).map(Some).ok_or(()),
    }
}

/// Features as they're read, and the schema so far.
#[derive(Default)]
struct Collector {
    schema: Schema,
    /// Each feature's geometry, its values by field (typed once every feature is in) and whether
    /// its geometry had heights, which are dropped.
    features: Vec<(Option<Geometry>, Vec<(usize, Value)>, bool)>,
    skipped: usize,
}

impl FeatureSink for Collector {
    fn restart(&mut self) {
        *self = Collector::default();
    }

    fn feature(&mut self, raw: &str) {
        // Valid JSON, but its numbers may not fit (1e400): such a feature is skipped and counted.
        match serde_json::from_str::<Json>(raw) {
            Ok(f) => self.add(&f),
            Err(_) => self.skipped += 1,
        }
    }
}

impl Collector {
    /// Takes one feature: its properties into the schema, its geometry (or counts it unreadable),
    /// and whether it had heights.
    fn add(&mut self, f: &Json) {
        let props = f.get("properties").and_then(Json::as_object);
        // Every feature's properties count in the schema, those with unreadable geometry too.
        let attrs: Vec<(usize, Value)> = props.into_iter().flatten().map(|(k, v)| (self.schema.see(k, v), Value::from_json(v))).collect();
        match geojson_geometry(f) {
            Ok(geom) => {
                let zm = geom.is_some() && geojson_has_z(f);
                self.features.push((geom, attrs, zm));
            }
            Err(()) => self.skipped += 1,
        }
    }

    /// The dataset; `root` is the document's top level (for its `crs`, see [`declared_crs`]).
    fn finish(self, name: &str, root: &Map<String, Json>) -> Result<(Dataset, ReadReport)> {
        let mut report = ReadReport { skipped: self.skipped, ..ReadReport::default() };
        let fields = self.schema.fields();
        // Every feature gets a cell for every field, those it lacks too: many features with
        // properties spread over many fields would take far more than the file. Refused when the
        // table alone takes more than a file at the limit is planned to ([`MEMORY_PER_BYTE`]). In
        // the browser that plan is 2 GiB, more than its memory may hold beside the document: this
        // guards the desktop, and the browser no less than before #2408.
        let cells = (self.features.len() as u64).saturating_mul(fields.len() as u64);
        let table = cells.saturating_mul(std::mem::size_of::<Value>() as u64);
        let planned = max_bytes().saturating_mul(MEMORY_PER_BYTE);
        if table > planned {
            return Err(DataError::Format(format!(
                "{name}: its attribute table would take {} MiB ({} features × {} fields), more than the {} MiB planned for a file read into memory; a file with fewer fields, or split into parts, would fit",
                table.div_ceil(1 << 20),
                self.features.len(),
                fields.len(),
                planned >> 20
            )));
        }
        let mut ds = Dataset::new(name, GeometryType::None, fields);
        ds.features.reserve(self.features.len());
        if report.skipped > 0 {
            report.messages.push(crate::loss::Note::loss(
                crate::loss::LossCode::DroppedFeature,
                format!("{} features had unreadable geometry", report.skipped),
            ));
        }
        let mut said = crate::loss::Notes::new();
        let declared = declared_crs(root, &mut said)?;
        report.messages.extend(said);
        let (mut unconverted, mut placed) = (0usize, 0usize);
        let mut beyond = 0usize;
        // Only features kept count toward the heights dropped.
        let mut zm = 0usize;
        // Features kept, as longitude/latitude, with a latitude past the poles.
        let mut swapped = 0usize;
        for (n, (geom, values, had_z)) in self.features.into_iter().enumerate() {
            // Kept as the file holds it, in its declared system, and counted when it has no place
            // in longitude/latitude ([`places_lonlat_extent`]), as when the file is read in place.
            if let (Some((_, t)), Some(g)) = (&declared, &geom) {
                match places_lonlat_extent(g, t) {
                    Some(e) => {
                        placed += 1;
                        beyond += usize::from(e.beyond_lonlat());
                    }
                    None => unconverted += 1,
                }
            }
            let mut attrs = vec![Value::Null; ds.fields.len()];
            for (i, v) in values {
                if let (Some(slot), Some(field)) = (attrs.get_mut(i), ds.fields.get(i)) {
                    *slot = coerce(v, field.ty);
                }
            }
            zm += usize::from(had_z);
            // Only what is read as longitude/latitude can be swapped.
            swapped += usize::from(declared.is_none() && geom.as_ref().is_some_and(latitude_past_poles));
            ds.features.push(crate::Feature { id: n as u64 + 1, geometry: geom, attrs });
        }
        ds.geometry_type = dominant_type(&ds);
        if let Some((crs, _)) = &declared {
            ds.crs = Some(*crs);
            ds.held_in = *crs;
            report.messages.extend(kept_unplaced_note("features", unconverted, placed == 0, *crs));
            report.messages.extend(beyond_lonlat_note("features", beyond, *crs));
        }
        report.messages.extend(swapped_latitude_note(swapped));
        // Last, as a source read in place notes it.
        ds.drop_zm(&mut report, zm);
        Ok((ds, report))
    }
}

/// Reads a GeoJSON document already parsed: a FeatureCollection, Feature or bare geometry. Only a
/// FeatureCollection's `crs` is read (a Feature's or a geometry's isn't part of the legacy text).
fn read_geojson_value(root: Json, name: &str) -> Result<(Dataset, ReadReport)> {
    let mut features = Collector::default();
    let mut top = Map::new();
    match root.get("type").and_then(Json::as_str) {
        Some("FeatureCollection") => {
            for f in root.get("features").and_then(Json::as_array).into_iter().flatten() {
                features.add(f);
            }
            if let Some(crs) = root.get("crs") {
                top.insert("crs".to_string(), crs.clone());
            }
        }
        Some("Feature") => features.add(&root),
        Some(_) => features.add(&json!({"type": "Feature", "geometry": &root, "properties": {}})),
        None => return Err(DataError::Format("GeoJSON: missing \"type\"".into())),
    }
    features.finish(name, &top)
}

fn json_type(v: &Json) -> Option<FieldType> {
    match v {
        Json::Bool(_) => Some(FieldType::Boolean),
        Json::Number(n) if n.is_i64() => Some(FieldType::Integer),
        Json::Number(_) => Some(FieldType::Double),
        Json::Null => None,
        _ => Some(FieldType::Text),
    }
}

fn widen(a: FieldType, b: Option<FieldType>) -> FieldType {
    match (a, b) {
        (a, None) => a,
        (a, Some(b)) if a == b => a,
        (FieldType::Integer, Some(FieldType::Double)) | (FieldType::Double, Some(FieldType::Integer)) => FieldType::Double,
        _ => FieldType::Text,
    }
}

/// A value as its field's type holds it: an integer in a decimal field as a decimal, anything in
/// a text field as its text.
pub(crate) fn coerce(v: Value, ty: FieldType) -> Value {
    match (ty, v) {
        (_, Value::Null) => Value::Null,
        (FieldType::Double, Value::Int(i)) => Value::Float(i as f64),
        (FieldType::Text, v @ Value::Text(_)) => v,
        // A number in full, as a file writes it: `Display` rounds to 6 decimals.
        (FieldType::Text, v) => Value::Text(v.to_file_text()),
        (_, v) => v,
    }
}

pub(crate) fn dominant_type(ds: &Dataset) -> GeometryType {
    let mut counts = [0usize; 3];
    for g in ds.features.iter().filter_map(|f| f.geometry.as_ref()) {
        match g.kind() {
            GeometryType::Point => counts[0] += 1,
            GeometryType::Polyline => counts[1] += 1,
            GeometryType::Polygon => counts[2] += 1,
            GeometryType::None => {}
        }
    }
    match counts.iter().enumerate().max_by_key(|(_, n)| **n) {
        Some((_, 0)) | None => GeometryType::None,
        Some((0, _)) => GeometryType::Point,
        Some((1, _)) => GeometryType::Polyline,
        Some(_) => GeometryType::Polygon,
    }
}

/// A dataset as a GeoJSON FeatureCollection value, of its coordinates as they are. GeoJSON is in
/// longitude/latitude, so a dataset held in another system ([`Dataset::coord_crs`]) is converted
/// first, by [`Dataset::in_lonlat`], which refuses, naming the feature, a place that can't be
/// given there; a writer that adds a legacy `crs` member passes its own system's coordinates.
pub fn to_geojson(ds: &Dataset) -> Json {
    let features: Vec<Json> = ds.features.iter().map(|f| feature_to_geojson(&ds.fields, f)).collect();
    json!({"type": "FeatureCollection", "name": ds.name, "features": features})
}

/// A GeoJSON text conversion with per-feature progress and cancellation, of `ds`'s coordinates as
/// they are: the caller converts a dataset held in another system first ([`Dataset::in_lonlat`]).
pub fn to_geojson_text(ds: &Dataset) -> Result<String> {
    let mut writer = GeoJsonWriter::new(Vec::new(), &ds.name, &ds.fields)?;
    writer.write(&ds.features)?;
    String::from_utf8(writer.finish()?).map_err(|e| DataError::Format(e.to_string()))
}

/// A feature as a GeoJSON Feature, its attributes named by `fields`.
fn feature_to_geojson(fields: &[Field], f: &crate::Feature) -> Json {
    let mut props = Map::new();
    for (field, v) in fields.iter().zip(&f.attrs) {
        props.insert(field.name.clone(), v.to_json());
    }
    json!({"type": "Feature", "id": f.id, "properties": props, "geometry": f.geometry.as_ref().map(geometry_to_geojson)})
}

/// Writes a GeoJSON FeatureCollection a run of features at a time, for exports that never hold
/// the whole layer: the same collection as [`to_geojson`] for the same features, its members
/// written in the order `type`, `name`, `features` (a serialized [`to_geojson`] sorts them).
pub(crate) struct GeoJsonWriter<W: std::io::Write> {
    out: W,
    fields: Vec<Field>,
    first: bool,
}

impl<W: std::io::Write> GeoJsonWriter<W> {
    /// Starts the collection named `name`, its features' attributes named by `fields`.
    pub(crate) fn new(mut out: W, name: &str, fields: &[Field]) -> Result<Self> {
        out.write_all(br#"{"type":"FeatureCollection","name":"#)?;
        serde_json::to_writer(&mut out, name).map_err(|e| DataError::Format(e.to_string()))?;
        out.write_all(br#","features":["#)?;
        Ok(GeoJsonWriter { out, fields: fields.to_vec(), first: true })
    }

    /// Writes the next features.
    pub(crate) fn write(&mut self, features: &[crate::Feature]) -> Result<()> {
        for f in features {
            crate::progress::step(1)?;
            if !std::mem::take(&mut self.first) {
                self.out.write_all(b",")?;
            }
            serde_json::to_writer(&mut self.out, &feature_to_geojson(&self.fields, f)).map_err(|e| DataError::Format(e.to_string()))?;
        }
        Ok(())
    }

    /// Closes the collection and gives the writer back.
    pub(crate) fn finish(mut self) -> Result<W> {
        self.out.write_all(b"]}")?;
        Ok(self.out)
    }
}

// ---------------------------------------------------------------------------------------------
// CSV

/// Longitude and latitude columns, recognised in any order; CSV export names its coordinate
/// columns with the first of each that no field has.
const LON_NAMES: &[&str] = &["longitude", "lon", "lng", "long", "lon_dd", "longitude_dd", "stop_lon"];
const LAT_NAMES: &[&str] = &["latitude", "lat", "lat_dd", "latitude_dd", "stop_lat"];
/// Further spellings seen in published files, recognised on reading but never written.
const LON_ALIASES: &[&str] = &["longitude decimal degrees", "longitutde", "longtitude"];
const LAT_ALIASES: &[&str] = &["latitude decimal degrees", "lattitude"];
/// Longitude in other languages and scripts, lower-cased, recognised on reading but never
/// written (#2385). Only words that name the coordinate: one that is also an object's length
/// (German `Länge`, Arabic `الطول`, Greek `μήκος`) isn't taken on its own, so a table of
/// measurements doesn't open as points. Spanish and Catalan `longitud` also means length: it is
/// only taken as written (no note in brackets) and beside `latitud` ([`AMBIGUOUS_LON`]).
const LON_WORLD: &[&str] = &[
    // Chinese (traditional, simplified), Japanese, Korean; east longitude.
    "經度",
    "座標經度",
    "坐標經度",
    "经度",
    "坐标经度",
    "座标经度",
    "経度",
    "경도",
    "東經",
    "东经",
    "東経",
    // Vietnamese, Thai, Indonesian and Malay, Filipino.
    "kinh độ",
    "kinh do",
    "ลองจิจูด",
    "ลองติจูด",
    "bujur",
    "garis bujur",
    "longhitud",
    // Hindi and Marathi, Bengali, Tamil, Urdu, Persian (both yehs), Arabic, Hebrew.
    "देशांतर",
    "देशान्तर",
    "रेखांश",
    "দ্রাঘিমাংশ",
    "தீர்க்கரேகை",
    "طول بلد",
    "طول جغرافیایی",
    "طول جغرافيايي",
    "خط الطول",
    "קו אורך",
    // Russian, Ukrainian, Bulgarian, Serbian, Kazakh, Mongolian, Greek (and as upper case loses
    // its accents).
    "долгота",
    "довгота",
    "географска дължина",
    "географска дужина",
    "бойлық",
    "уртраг",
    "γεωγραφικό μήκος",
    "γεωγραφικο μηκος",
    // Germanic, Romance, Baltic and Slavic languages in Latin script (German also as ASCII).
    "längengrad",
    "laengengrad",
    "geographische länge",
    "geographische laenge",
    "geografische länge",
    "geografische laenge",
    "lengtegraad",
    "longitud",
    "longitudine",
    "lonxitude",
    "longitudea",
    "längdgrad",
    "længdegrad",
    "lengdegrad",
    "lengdargráða",
    "pituusaste",
    "pikkuskraad",
    "ilguma",
    "boylam",
    "długość geograficzna",
    "zeměpisná délka",
    "zemepisná dĺžka",
    "földrajzi hosszúság",
    "geografska dužina",
    "zemljepisna dolžina",
    "ģeogrāfiskais garums",
];
/// Words of [`LON_WORLD`] that also mean length, each with the latitude it is only taken beside.
const AMBIGUOUS_LON: &[(&str, &str)] = &[("longitud", "latitud")];
/// Latitude likewise ([`LON_WORLD`]).
const LAT_WORLD: &[&str] = &[
    "緯度",
    "座標緯度",
    "坐標緯度",
    "纬度",
    "坐标纬度",
    "座标纬度",
    "위도",
    "北緯",
    "北纬",
    "vĩ độ",
    "vi do",
    "ละติจูด",
    "ละติดจูด",
    "lintang",
    "garis lintang",
    "latitud",
    "अक्षांश",
    "অক্ষাংশ",
    "அட்சரேகை",
    "عرض بلد",
    "عرض جغرافیایی",
    "عرض جغرافيايي",
    "خط العرض",
    "קו רוחב",
    "широта",
    "географска ширина",
    "ендік",
    "өргөрөг",
    "γεωγραφικό πλάτος",
    "γεωγραφικο πλατος",
    "breitengrad",
    "geographische breite",
    "geografische breite",
    "breedtegraad",
    "latitudine",
    "latitudea",
    "breddegrad",
    "breiddargráða",
    "leveysaste",
    "laiuskraad",
    "platuma",
    "enlem",
    "szerokość geograficzna",
    "zeměpisná šířka",
    "zemepisná šírka",
    "földrajzi szélesség",
    "geografska širina",
    "zemljepisna širina",
    "ģeogrāfiskais platums",
];

/// Projected coordinate columns, recognised as well as longitude/latitude ones.
const X_NAMES: &[&str] = &["easting", "point_x", "xcoord", "x_coord"];
const Y_NAMES: &[&str] = &["northing", "point_y", "ycoord", "y_coord"];
/// Columns that may hold WKT geometry.
const WKT_NAMES: &[&str] = &["wkt", "geometry", "geom", "the_geom", "wkb_geometry", "shape", "ewkt"];

/// How to read a delimited text file; everything left out is found from the file.
#[derive(Clone, Debug, Default)]
pub struct CsvOptions {
    /// The column of x (longitude, easting) values.
    pub x: Option<String>,
    /// The column of y (latitude, northing) values.
    pub y: Option<String>,
    /// A column of WKT (or EWKT) geometry.
    pub wkt: Option<String>,
    /// The coordinate system of the coordinates (otherwise an EWKT SRID, or longitude/latitude).
    pub crs: Option<crate::raster::Crs>,
    /// The field separator (otherwise the most common of `,`, `;`, tab and `|` in the header).
    pub delimiter: Option<u8>,
}

/// The separator of a delimited text file: the most common of `,`, `;`, tab and `|` in its first
/// line, outside quotes (`,` when none is).
pub fn sniff_delimiter(text: &str) -> u8 {
    let seps = *b",;\t|";
    // Each separator's count on each of the first lines, outside quotes.
    let counts: Vec<[usize; 4]> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(20)
        .map(|line| {
            let mut c = [0usize; 4];
            let mut quoted = false;
            for ch in line.chars() {
                match ch {
                    '"' => quoted = !quoted,
                    ',' if !quoted => c[0] += 1,
                    ';' if !quoted => c[1] += 1,
                    '\t' if !quoted => c[2] += 1,
                    '|' if !quoted => c[3] += 1,
                    _ => {}
                }
            }
            c
        })
        .collect();
    let Some(header) = counts.first() else { return b',' };
    // One that splits every line into the same number of columns (as `Name; Surname` in a
    // one-column file doesn't, once there are rows); the most columns wins, a tie keeps the
    // order above, so the comma.
    let consistent = |i: usize| header[i] > 0 && counts.iter().all(|c| c[i] == header[i]);
    let pick = (0..4).filter(|&i| counts.len() > 1 && consistent(i)).fold(None, |b: Option<usize>, i| match b {
        Some(b) if header[b] >= header[i] => Some(b),
        _ => Some(i),
    });
    // Otherwise (a header alone, or ragged rows): the comma if the header has one, else the most
    // common.
    let pick = pick.unwrap_or_else(|| if header[0] > 0 { 0 } else { (0..4).fold(0, |b, i| if header[i] > header[b] { i } else { b }) });
    seps.get(pick).copied().unwrap_or(b',')
}

/// Finds a complete X/Y pair, falling back to planar aliases.
fn found_non_geographic_xy(headers: &[String]) -> Option<(usize, usize)> {
    let find = |name: &str| headers.iter().position(|h| h.trim().eq_ignore_ascii_case(name));
    let alias = |names: &[&str]| headers.iter().position(|h| names.iter().any(|name| h.trim().eq_ignore_ascii_case(name)));
    find("x").zip(find("y")).or_else(|| alias(X_NAMES).zip(alias(Y_NAMES))).or_else(|| found_planar_xy(headers))
}

/// A projected system prefers explicitly planar names over longitude/latitude, regardless of
/// header order. With no planar pair, retain the ordinary named-pair fallback.
fn found_projected_xy(headers: &[String]) -> Option<(usize, usize)> {
    found_planar_xy(headers).or_else(|| found_geographic_xy(headers))
}

/// Finds both axes among planar aliases and generic X/Y names.
fn found_planar_xy(headers: &[String]) -> Option<(usize, usize)> {
    let find = |names: &[&str], generic: &str| {
        headers.iter().position(|h| {
            let name = h.trim().to_ascii_lowercase();
            name == generic || names.contains(&name.as_str())
        })
    };
    find(X_NAMES, "x").zip(find(Y_NAMES, "y"))
}

/// A header as longitude and latitude names are looked up ([`folded`]), with a note in brackets
/// at its end set aside (`經度(X)`, `Longitude (WGS84)`, `緯度（度）`, `lat [deg]`).
fn geographic_key(header: &str) -> String {
    let folded = folded(header);
    let mut name = folded.as_str();
    for (open, close) in [('(', ')'), ('（', '）'), ('[', ']')] {
        if let Some(rest) = name.strip_suffix(close)
            && let Some(at) = rest.rfind(open)
            && let Some(head) = rest.get(..at).map(str::trim_end)
            && !head.is_empty()
        {
            name = head;
            break;
        }
    }
    name.to_string()
}

/// A header as it is compared with longitude and latitude names: trimmed, in Unicode's composed
/// form (a name whose accents a file saved as separate marks, as some Macs save Vietnamese, reads
/// as one typed with them), lower-cased in any script.
fn folded(header: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    header.trim().nfc().collect::<String>().to_lowercase()
}

/// Whether a header is, as written, a longitude word that also means length ([`AMBIGUOUS_LON`]).
fn ambiguous_lon(header: &str) -> bool {
    let name = folded(header);
    AMBIGUOUS_LON.iter().any(|(word, _)| *word == name)
}

/// How well a header names longitude (`lat`: latitude), best first: an English name as written
/// (0), a published misspelling as written (1), either with a note in brackets set aside (2, 3),
/// a name in another language as written (4), then one with a note set aside (5); its words may
/// be joined by `_`. `None`: not a name of it. So `Longitude` wins over `Longitude (DMS)`, and
/// `經度` over `經度(DMS)`, wherever they stand.
fn geographic_tier(header: &str, lat: bool) -> Option<usize> {
    let exact = folded(header);
    let key = geographic_key(header);
    let (names, aliases, world) = if lat { (LAT_NAMES, LAT_ALIASES, LAT_WORLD) } else { (LON_NAMES, LON_ALIASES, LON_WORLD) };
    let in_world = |k: &str| world.contains(&k) || world.contains(&k.replace('_', " ").as_str());
    // A word that also means length counts only as written ([`AMBIGUOUS_LON`]).
    let ambiguous = AMBIGUOUS_LON.iter().any(|(w, _)| *w == key);
    let hits = [
        names.contains(&exact.as_str()),
        aliases.contains(&exact.as_str()),
        names.contains(&key.as_str()),
        aliases.contains(&key.as_str()),
        in_world(&exact),
        !ambiguous && in_world(&key),
    ];
    hits.iter().position(|hit| *hit)
}

/// The number of [`geographic_tier`]s.
const GEOGRAPHIC_TIERS: usize = 6;

/// A complete longitude/latitude pair: for each, the first column of the best tier of names
/// that has one ([`geographic_tier`]). A longitude that also means length ([`AMBIGUOUS_LON`])
/// is taken only when there is no other, beside its own language's latitude (with or without a
/// note), whatever other latitude the file has.
fn found_geographic_xy(headers: &[String]) -> Option<(usize, usize)> {
    let best = |lat: bool, keep: &dyn Fn(&str) -> bool| {
        let tiers: Vec<Option<usize>> = headers.iter().map(|h| geographic_tier(h, lat)).collect();
        (0..GEOGRAPHIC_TIERS)
            .find_map(|tier| (0..tiers.len()).find(|&i| tiers.get(i) == Some(&Some(tier)) && headers.get(i).is_some_and(|h| keep(h))))
    };
    let lat = best(true, &|_| true);
    if let (Some(lon), Some(lat)) = (best(false, &|h| !ambiguous_lon(h)), lat) {
        return Some((lon, lat));
    }
    AMBIGUOUS_LON.iter().find_map(|(word, partner)| {
        let lon = headers.iter().position(|h| folded(h) == *word)?;
        // Its latitude as written first, then one with a note.
        let lat =
            headers.iter().position(|h| folded(h) == *partner).or_else(|| headers.iter().position(|h| geographic_key(h) == *partner))?;
        Some((lon, lat))
    })
}

/// Names candidate columns when both axes are present without a matching pair.
fn unmatched_coordinates_note(headers: &[String]) -> Option<String> {
    let mut candidates = Vec::new();
    let mut has_x = false;
    let mut has_y = false;
    for header in headers.iter().map(|h| h.trim()) {
        let name = header.to_ascii_lowercase();
        // (A word that also means length isn't named on its own.)
        let x = name == "x" || X_NAMES.contains(&name.as_str()) || (geographic_tier(header, false).is_some() && !ambiguous_lon(header));
        let y = name == "y" || Y_NAMES.contains(&name.as_str()) || geographic_tier(header, true).is_some();
        if x || y {
            candidates.push(header);
            has_x |= x;
            has_y |= y;
        }
    }
    (has_x && has_y).then(|| {
        format!("Opened as a table. No matching coordinate pair among {}. Choose x and y to import points.", candidates.join(", "))
    })
}

/// Whether a position can be counted as evidence for inferring geographic coordinate columns.
fn usable_degree_point(p: Coord) -> bool {
    p.x.abs() <= 180.0 && p.y.abs() <= 90.0 && (p.x != 0.0 || p.y != 0.0)
}

/// Compares complete pairs by usable degree positions regardless of header order, preferring
/// longitude/latitude on a tie. With no usable positions, retains finite X/Y for the import notices.
/// Explicit columns and projected systems bypass this inference.
fn found_xy_in_rows(headers: &[String], rows: &[Vec<String>]) -> Result<Option<(usize, usize)>> {
    let Some(xy) = found_non_geographic_xy(headers) else { return Ok(found_geographic_xy(headers)) };
    let Some(geographic) = found_geographic_xy(headers) else { return Ok(Some(xy)) };
    let (mut xy_usable, mut geographic_usable) = (0usize, 0usize);
    let mut xy_finite = false;
    for row in rows {
        crate::progress::check()?;
        let point = |(x, y): (usize, usize)| {
            let x = row.get(x)?.trim().parse::<f64>().ok()?;
            let y = row.get(y)?.trim().parse::<f64>().ok()?;
            let p = Coord::new(x, y);
            p.is_finite().then_some(p)
        };
        let xy_point = point(xy);
        xy_finite |= xy_point.is_some();
        xy_usable += usize::from(xy_point.is_some_and(usable_degree_point));
        geographic_usable += usize::from(point(geographic).is_some_and(usable_degree_point));
    }
    let prefer_geographic = geographic_usable >= xy_usable && (geographic_usable > 0 || !xy_finite);
    Ok(Some(if prefer_geographic { geographic } else { xy }))
}

/// A WKT column found: one named like geometry whose first value reads as WKT.
fn found_wkt(headers: &[String], rows: &[Vec<String>]) -> Option<usize> {
    // The name first: looking for a column's first value costs a pass over the rows.
    headers.iter().enumerate().filter(|(_, h)| WKT_NAMES.contains(&h.trim().to_ascii_lowercase().as_str())).find_map(|(i, _)| {
        let first = rows.iter().filter_map(|r| r.get(i)).map(|s| s.trim()).find(|s| !s.is_empty())?;
        crate::interop::parse_wkt(first).is_ok().then_some(i)
    })
}

/// The columns of delimited text or a sheet, and those its geometry is read from when none are
/// given (see [`read_csv_with`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Columns {
    pub names: Vec<String>,
    pub x: Option<String>,
    pub y: Option<String>,
    pub wkt: Option<String>,
    /// The field separator (delimited text only).
    pub delimiter: Option<u8>,
}

/// The rows [`csv_columns`] samples for coordinate ranges and a WKT column's first value.
const COLUMN_SAMPLE_ROWS: usize = 1000;

/// The columns of rows under a header row, as [`read_rows`] would find them with `crs`.
pub(crate) fn columns_of(headers: &[String], rows: &[Vec<String>], crs: Option<Crs>) -> Result<Columns> {
    let xy = if crs.is_none_or(Crs::is_geographic) { found_xy_in_rows(headers, rows)? } else { found_projected_xy(headers) };
    let wkt = if xy.is_some() { None } else { found_wkt(headers, rows) };
    let name = |i: usize| headers.get(i).map(|h| h.trim().to_string());
    Ok(Columns {
        names: headers.iter().map(|h| h.trim().to_string()).collect(),
        x: xy.and_then(|(x, _)| name(x)),
        y: xy.and_then(|(_, y)| name(y)),
        wkt: wkt.and_then(name),
        delimiter: None,
    })
}

/// The columns of delimited text (split at `delimiter`, or the one found), without reading all
/// of it: coordinate ranges and a WKT column's first value are checked within
/// the first [`COLUMN_SAMPLE_ROWS`] rows only. A projected `crs` prefers planar x/y or
/// easting/northing names over longitude/latitude, without inspecting coordinate ranges.
pub fn csv_columns(text: &str, delimiter: Option<u8>, crs: Option<Crs>) -> Result<Columns> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let delimiter = delimiter.unwrap_or_else(|| sniff_delimiter(text));
    let mut rdr = csv::ReaderBuilder::new().flexible(true).delimiter(delimiter).from_reader(text.as_bytes());
    let headers: Vec<String> = rdr.headers().map_err(|e| DataError::Format(format!("CSV: {e}")))?.iter().map(|s| s.to_string()).collect();
    let rows: Vec<Vec<String>> =
        rdr.records().flatten().take(COLUMN_SAMPLE_ROWS).map(|r| r.iter().map(|s| s.to_string()).collect()).collect();
    Ok(Columns { delimiter: Some(delimiter), ..columns_of(&headers, &rows, crs)? })
}

/// The text of a delimited file from its bytes, and the note to report when it wasn't UTF-8.
/// UTF-8 is read as it is. A UTF-16 byte-order mark (Excel's "Unicode text") is read as UTF-16.
/// Other bytes are read as Windows-1252, the code page most Latin-1 files use: Latin-1's
/// characters, plus quotes and dashes in 0x80–0x9F. Many public sources still publish CSV in it
/// (Brazil's DATASUS, INEP and TSE among them). Bytes with a NUL in them aren't text.
pub fn delimited_text<'a>(file_name: &str, bytes: &'a [u8]) -> Result<(std::borrow::Cow<'a, str>, Option<String>)> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok((text.into(), None));
    }
    if let Some((enc, _)) = encoding_rs::Encoding::for_bom(bytes).filter(|(enc, _)| *enc != encoding_rs::UTF_8) {
        let (text, malformed) = enc.decode_with_bom_removal(bytes);
        let mut note = format!("{file_name} was read as {} text", enc.name());
        if malformed {
            note.push_str("; bytes that weren't valid in it became the replacement character (U+FFFD)");
        }
        return Ok((text, Some(note)));
    }
    if bytes.contains(&0) {
        return Err(DataError::Format(format!("{file_name} is not text")));
    }
    // A UTF-8 byte-order mark before a stray Latin-1 byte (Excel's "CSV UTF-8" edited elsewhere)
    // would become "ï»¿" at the start of the first header.
    let (text, _) = encoding_rs::WINDOWS_1252.decode_without_bom_handling(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes));
    Ok((text, Some(format!("{file_name} isn't UTF-8 text: it was read as Windows-1252 (Latin-1)"))))
}

/// Whether a name is gzip-compressed delimited text, rather than an arbitrary gzip archive.
pub fn is_gzip_text_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".gz") && is_delimited_name(&lower)
}

/// Whether a name is delimited text, including gzip-compressed CSV, TSV, TXT and DAT.
pub fn is_delimited_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let plain = lower.strip_suffix(".gz").unwrap_or(&lower);
    [".csv", ".tsv", ".txt", ".dat"].iter().any(|ext| plain.ends_with(ext))
}

/// A gzip decoder with a limit on expanded bytes. The limit is checked while reading, rather
/// than trusting a gzip trailer's 32-bit size; concatenated members share the same budget.
struct GzipText<R: std::io::Read> {
    decoder: flate2::read::MultiGzDecoder<R>,
    left: u64,
    /// The limit it started with, for the refusal.
    limit: u64,
}

impl<R: std::io::Read> GzipText<R> {
    /// Starts a streaming decode; callers still check cancellation at record boundaries.
    fn new(reader: R) -> Self {
        let limit = max_bytes();
        Self { decoder: flate2::read::MultiGzDecoder::new(reader), left: limit, limit }
    }
}

impl<R: std::io::Read> std::io::Read for GzipText<R> {
    /// Reads at most the remaining budget plus one byte, refusing expansion past the limit.
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        crate::progress::check().map_err(std::io::Error::other)?;
        let size = buf.len().min(usize::try_from(self.left.saturating_add(1)).unwrap_or(usize::MAX));
        let n = self.decoder.read(buf.get_mut(..size).unwrap_or_default())?;
        if n as u64 > self.left {
            // At least this much expanded: what was read before and this read.
            let expanded = (self.limit - self.left).saturating_add(n as u64);
            return Err(std::io::Error::other(format!(
                "expanded to more than the limit, at least {}: {}",
                size_said(expanded, true),
                too_large_said(expanded, self.limit)
            )));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

/// The first `limit` expanded bytes, for the column picker. It deliberately stops at the
/// prefix; adding the file later validates its complete gzip stream and checksum.
pub fn gzip_text_prefix(reader: impl std::io::Read, limit: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    let result = GzipText::new(reader).take(limit.min(usize::try_from(max_bytes()).unwrap_or(usize::MAX)) as u64).read_to_end(&mut bytes);
    crate::progress::check()?;
    result.map_err(|e| DataError::Format(format!("gzip delimited text: {e}")))?;
    Ok(bytes)
}

/// Reads UTF-8 gzip-compressed delimited text, streaming records without holding the whole
/// expanded file. Other encodings can be decompressed and added as .txt. The existing table
/// builder retains rows for type inference; this isn't a read-in-place provider.
pub fn read_gzip_text(reader: impl std::io::Read, file_name: &str, opts: &CsvOptions) -> Result<(Dataset, ReadReport)> {
    read_gzip_text_budget(reader, file_name, opts, &mut csv_memory_budget())
}

/// Reads a gzip member against the remaining retained-table allowance shared by its archive.
fn read_gzip_text_budget(
    reader: impl std::io::Read,
    file_name: &str,
    opts: &CsvOptions,
    budget: &mut usize,
) -> Result<(Dataset, ReadReport)> {
    use std::io::{BufRead, Read};
    if !is_gzip_text_name(file_name) {
        return Err(DataError::Unsupported(format!("{file_name}: decompress it first, then add the supported file inside")));
    }
    let mut decoder = std::io::BufReader::new(GzipText::new(reader));
    let mut prefix = Vec::new();
    let result = (|| -> std::io::Result<()> {
        decoder.by_ref().take(64 << 10).read_to_end(&mut prefix)?;
        if opts.delimiter.is_none() {
            // The existing sniffer compares physical lines. Finish its sampled line and include
            // a data line even when the header or first row exceeds the usual prefix size.
            if prefix.last().is_some_and(|c| *c != b'\n') {
                decoder.read_until(b'\n', &mut prefix)?;
            }
            let mut lines = prefix.split(|b| *b == b'\n').filter(|line| !line.iter().all(u8::is_ascii_whitespace)).count();
            while lines < 2 {
                let start = prefix.len();
                if decoder.read_until(b'\n', &mut prefix)? == 0 {
                    break;
                }
                if prefix.get(start..).is_some_and(|line| !line.iter().all(u8::is_ascii_whitespace)) {
                    lines += 1;
                }
                crate::progress::check().map_err(std::io::Error::other)?;
            }
        }
        Ok(())
    })();
    crate::progress::check()?;
    result.map_err(|e| DataError::Format(format!("{file_name}: gzip: {e}")))?;
    if prefix.starts_with(b"\xEF\xBB\xBF") {
        prefix.drain(..3);
    }
    let delimiter = opts.delimiter.unwrap_or_else(|| sniff_delimiter(&String::from_utf8_lossy(&prefix)));
    let plain = file_name.get(..file_name.len().saturating_sub(3)).unwrap_or(file_name);
    let name = Path::new(plain).file_stem().and_then(|n| n.to_str()).unwrap_or("table");
    let mut rdr = csv::ReaderBuilder::new().flexible(true).delimiter(delimiter).from_reader(std::io::Cursor::new(prefix).chain(decoder));
    // The sample can end inside a record. Parse the complete header from the stream before
    // deciding whether an ambiguous DAT is delimited text.
    if plain.to_ascii_lowercase().ends_with(".dat") && opts.delimiter.is_none() {
        let headers = rdr.headers();
        crate::progress::check()?;
        let headers = headers.map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
        if headers.len() < 2 || headers.iter().any(|h| h.chars().any(char::is_control)) {
            return Err(DataError::Format(format!(
                "{file_name}: not recognised as delimited text; give a delimiter, or decompress and rename to .txt"
            )));
        }
    }
    // The encoding hint only on text that isn't UTF-8 (told by the reader), not past the size
    // limit or the memory budget.
    let result = read_csv_records(&mut rdr, name, opts, budget, Some("; if not UTF-8, decompress and add as .txt"));
    crate::progress::check()?;
    let (mut dataset, report) = result.map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
    dataset.source = Some(file_name.to_string());
    Ok((dataset, report))
}

/// Estimated memory for retained CSV rows and the dataset built alongside them: a quarter
/// of physical memory, between 256 MiB (also the browser limit) and 4 GiB.
/// Less once RustGIS holds more than half the memory, never below the floor ([`crate::memory::planned`], #2431).
fn csv_memory_budget() -> usize {
    let ceiling = usize::try_from(4u64 << 30).unwrap_or(usize::MAX);
    crate::memory::planned(256 << 20, ceiling, 1)
}

/// Collects records for type inference, charging text and row/cell objects before copying.
/// The estimate includes the dataset built while these rows remain alive and room for vector
/// capacity. Reader failures and a spent memory budget refuse the whole import; text that isn't
/// UTF-8 is refused with `utf8_hint` after the reader's error, where given.
fn read_csv_records(
    reader: &mut csv::Reader<impl std::io::Read>,
    name: &str,
    opts: &CsvOptions,
    budget: &mut usize,
    utf8_hint: Option<&str>,
) -> Result<(Dataset, ReadReport)> {
    let refused = |e: csv::Error| {
        let hint = if matches!(e.kind(), csv::ErrorKind::Utf8 { .. }) { utf8_hint.unwrap_or("") } else { "" };
        DataError::Format(format!("CSV: {e}{hint}"))
    };
    let cost = |r: &csv::StringRecord, columns: usize| {
        r.as_slice()
            .len()
            .saturating_mul(2)
            .saturating_add(r.len().saturating_mul(2 * std::mem::size_of::<String>()))
            .saturating_add(columns.saturating_mul(2 * std::mem::size_of::<Value>()))
            .saturating_add(2 * (std::mem::size_of::<Vec<String>>() + std::mem::size_of::<crate::Feature>()))
    };
    let allowance = *budget;
    let too_large =
        || DataError::Format(format!("CSV retained table exceeds the {allowance}-byte remaining memory budget; import a smaller file"));
    let header = reader.headers().map_err(refused)?;
    let mut left = budget.checked_sub(cost(header, header.len())).ok_or_else(too_large)?;
    let headers: Vec<String> = header.iter().map(str::to_string).collect();
    let mut malformed = 0;
    let mut rows = Vec::new();
    for record in reader.records() {
        crate::progress::check()?;
        match record {
            Ok(r) => {
                left = left.checked_sub(cost(&r, headers.len())).ok_or_else(too_large)?;
                rows.push(r.iter().map(str::to_string).collect());
            }
            Err(e) if e.is_io_error() || matches!(e.kind(), csv::ErrorKind::Utf8 { .. }) => return Err(refused(e)),
            Err(_) => malformed += 1,
        }
    }
    let result = read_rows(&headers, &rows, malformed, name, opts, None)?;
    *budget = left;
    Ok(result)
}

/// Reads delimited text with explicit columns/separator, including gzip. Uncompressed text
/// uses [`delimited_text`]'s encoding detection. A DAT needs a delimiter or recognisable rows.
pub fn read_delimited_bytes(file_name: &str, bytes: &[u8], opts: &CsvOptions) -> Result<(Dataset, ReadReport)> {
    read_delimited_bytes_budget(file_name, bytes, opts, &mut csv_memory_budget())
}

/// Reads an archive member without resetting the retained-table allowance.
fn read_delimited_bytes_budget(file_name: &str, bytes: &[u8], opts: &CsvOptions, budget: &mut usize) -> Result<(Dataset, ReadReport)> {
    let max = max_bytes();
    if bytes.len() as u64 > max {
        return Err(DataError::Format(file_size_error(file_name, max)));
    }
    if is_gzip_text_name(file_name) {
        return read_gzip_text_budget(bytes, file_name, opts, budget);
    }
    let (text, note) = delimited_text(file_name, bytes)?;
    if file_name.to_ascii_lowercase().ends_with(".dat") && opts.delimiter.is_none() && delimited_table_columns(&text) < 2 {
        return Err(DataError::Format(format!(
            "{file_name}: not recognised as delimited text; give a delimiter, or rename to .txt to read it as a table"
        )));
    }
    let name = Path::new(file_name).file_stem().and_then(|n| n.to_str()).unwrap_or("table");
    let (mut ds, mut report) = read_csv_with_budget(&text, name, opts, budget)?;
    ds.source = Some(file_name.to_string());
    report.messages.extend(note);
    Ok((ds, report))
}

/// Reads CSV; columns named like longitude/latitude become point geometry.
pub fn read_csv(text: &str, name: &str) -> Result<(Dataset, ReadReport)> {
    read_csv_with(text, name, &CsvOptions::default())
}

/// Reads delimited text: points from x/y columns (longitude/latitude unless `crs` says
/// otherwise), or geometry from a WKT column; without either it is a table.
pub fn read_csv_with(text: &str, name: &str, opts: &CsvOptions) -> Result<(Dataset, ReadReport)> {
    read_csv_with_budget(text, name, opts, &mut csv_memory_budget())
}

/// Parses a text member using the same retained-table accounting as streamed gzip records.
fn read_csv_with_budget(text: &str, name: &str, opts: &CsvOptions, budget: &mut usize) -> Result<(Dataset, ReadReport)> {
    // Spreadsheet exports often start with a byte-order mark, which would hide the first header.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let delimiter = opts.delimiter.unwrap_or_else(|| sniff_delimiter(text));
    let mut rdr = csv::ReaderBuilder::new().flexible(true).delimiter(delimiter).from_reader(text.as_bytes());
    read_csv_records(&mut rdr, name, opts, budget, None)
}

/// Rows of text cells under a header row (delimited text, a spreadsheet's sheet) as a dataset, as
/// [`read_csv_with`] describes; `malformed` rows were already passed over. A workbook gives its
/// cell types, which may override text inference by source column, and the dates it already read
/// as empty (the cells counted for each source column), before removing the geometry column.
pub(crate) fn read_rows(
    headers: &[String],
    rows: &[Vec<String>],
    malformed: usize,
    name: &str,
    opts: &CsvOptions,
    workbook: Option<(&[Option<FieldType>], &[usize])>,
) -> Result<(Dataset, ReadReport)> {
    let types = workbook.map(|(types, _)| types);
    let named = |n: &Option<String>, what: &str| -> Result<Option<usize>> {
        match n.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => headers
                .iter()
                .position(|h| h.trim().eq_ignore_ascii_case(n))
                .map(Some)
                .ok_or_else(|| DataError::Format(format!("no {what} column '{n}' (columns: {})", headers.join(", ")))),
            None => Ok(None),
        }
    };
    let (x, y) = (named(&opts.x, "x")?, named(&opts.y, "y")?);
    let wkt_given = named(&opts.wkt, "WKT")?;
    if wkt_given.is_some() && (x.is_some() || y.is_some()) {
        return Err(DataError::Format("give x and y columns, or a WKT column, not both".into()));
    }
    let (x, y) = match (x, y) {
        (Some(x), Some(y)) => (Some(x), Some(y)),
        (None, None) if wkt_given.is_none() => {
            if opts.crs.is_none_or(Crs::is_geographic) { found_xy_in_rows(headers, rows)? } else { found_projected_xy(headers) }.unzip()
        }
        (None, None) => (None, None),
        _ => return Err(DataError::Format("give both the x and the y column".into())),
    };
    let xy = x.zip(y);
    // A WKT column: the one named, or (without x/y columns) one found.
    let wkt = wkt_given.or_else(|| if xy.is_some() { None } else { found_wkt(headers, rows) });
    // Infer each column's type from its values; a WKT column is the geometry, not an attribute.
    let columns: Vec<usize> = (0..headers.len()).filter(|&i| Some(i) != wkt).collect();
    let fields: Vec<Field> = columns
        .iter()
        .filter_map(|&i| {
            let ty = types
                .and_then(|t| t.get(i).copied().flatten())
                // `<Null>` is read as empty, so it types a column no more than an empty cell does.
                .unwrap_or_else(|| {
                    text_type(rows.iter().filter_map(|r| r.get(i)).map(|s| s.as_str()).filter(|s| !Value::is_null_marker(s)))
                });
            Some(Field::new(headers.get(i)?.trim(), ty))
        })
        .collect();
    let mut report = ReadReport { skipped: malformed, ..ReadReport::default() };
    if xy.is_none()
        && wkt.is_none()
        && let Some(note) = unmatched_coordinates_note(headers)
    {
        report.messages.push(note);
    }
    if malformed > 0 {
        report
            .messages
            .push(crate::loss::Note::loss(crate::loss::LossCode::DroppedFeature, format!("{malformed} rows were malformed and skipped")));
    }
    let gt = if xy.is_some() { GeometryType::Point } else { GeometryType::None };
    let mut ds = Dataset::new(name, gt, fields);
    let mut srid: Option<u32> = None;
    let (mut unread_dates, mut null_text) = (Vec::new(), Vec::new());
    for (k, &i) in columns.iter().enumerate() {
        crate::source::unread_dates_by(&mut unread_dates, k, workbook.and_then(|(_, empty)| empty.get(i)).copied().unwrap_or(0));
    }
    // The WKT column is the geometry, not a field: a workbook date there that never was is empty,
    // so its row has no geometry, said here.
    if let Some(w) = wkt {
        let column = headers.get(w).map_or("", |h| h.trim());
        // The values read as empty, below the heading: each a value lost, and with it its row's
        // geometry.
        match workbook.and_then(|(_, empty)| empty.get(w)).copied().unwrap_or(0) {
            0 => {}
            1 => report.messages.push(crate::loss::Note::loss(
                crate::loss::LossCode::DroppedValue,
                format!("1 value in '{column}' isn't a date and was read as empty, so its row has no geometry, or is left out if it held nothing else"),
            )),
            n => report.messages.push(crate::loss::Note::loss(
                crate::loss::LossCode::DroppedValue,
                format!("{n} values in '{column}' aren't dates and were read as empty, so their rows have no geometry, or are left out if they held nothing else"),
            )),
        }
    }
    let (mut bad_wkt, mut zm_rows, mut collection_rows) = (0usize, 0usize, 0usize);
    let (mut extra_cells, mut extra_rows) = (0usize, 0usize);
    for r in rows {
        crate::progress::step(1)?;
        // Only header-defined columns are retained below; count actual cells, including empty ones.
        let extra = r.len().saturating_sub(headers.len());
        extra_cells += extra;
        extra_rows += usize::from(extra > 0);
        let cell = |i: usize| r.get(i).map(|s| s.trim()).unwrap_or("");
        let geom = if let Some((x, y)) = xy {
            match cell(x).parse::<f64>().ok().zip(cell(y).parse::<f64>().ok()) {
                Some((x, y)) if x.is_finite() && y.is_finite() => Some(Geometry::Point(Coord::new(x, y))),
                _ => {
                    report.skipped += 1;
                    None
                }
            }
        } else if let Some(w) = wkt {
            match cell(w) {
                "" => None,
                t => match crate::interop::parse_wkt_report(t) {
                    Ok((g, s, zm, members_dropped)) => {
                        srid = srid.or(s);
                        zm_rows += usize::from(zm);
                        collection_rows += usize::from(members_dropped);
                        g
                    }
                    Err(_) => {
                        bad_wkt += 1;
                        None
                    }
                },
            }
        } else {
            None
        };
        let attrs = columns
            .iter()
            .zip(&ds.fields)
            .enumerate()
            .map(|(k, (&i, f))| {
                let text = unguard(r.get(i).map(|s| s.as_str()).unwrap_or(""));
                let marker = Value::is_null_marker(&text);
                if marker {
                    crate::source::null_text_at(&mut null_text, k);
                }
                let value = Value::parse_file(&text, f.ty);
                // The marker is counted once, as one, not also as a date that isn't one.
                if f.ty.is_temporal() && !marker && !text.trim().is_empty() && value.is_null() {
                    crate::source::unread_date(&mut unread_dates, k);
                }
                value
            })
            .collect();
        ds.push_next(geom, attrs);
    }
    if extra_cells > 0 {
        let cells = if extra_cells == 1 { "cell" } else { "cells" };
        let was = if extra_cells == 1 { "was" } else { "were" };
        let rows = if extra_rows == 1 { "row" } else { "rows" };
        let columns = if headers.len() == 1 { "column" } else { "columns" };
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedExtraAttributes,
            format!("{extra_cells} {cells} past the header's {} {columns} {was} omitted in {extra_rows} {rows}", headers.len()),
        ));
    }
    ds.drop_zm(&mut report, zm_rows);
    if collection_rows > 0 {
        let column = wkt.and_then(|i| headers.get(i)).map_or("", |h| h.trim());
        let what = if collection_rows == 1 { "WKT value" } else { "WKT values" };
        report.messages.push(crate::loss::Note::loss(crate::loss::LossCode::DroppedGeometry, format!("{collection_rows} {what} in '{column}' lost later non-empty geometry-collection members; only the first non-empty member is kept")));
    }
    if report.skipped > malformed {
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedGeometry,
            format!("{} rows had no usable coordinates", report.skipped - malformed),
        ));
    }
    if bad_wkt > 0 {
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedGeometry,
            format!("{bad_wkt} rows had geometry that isn't readable WKT"),
        ));
    }
    if wkt.is_some() {
        // The most common kind, as GeoJSON's reader takes it; rows of other kinds are kept (they
        // draw), and said.
        ds.geometry_type = dominant_type(&ds);
        let other = ds.features.iter().filter_map(|f| f.geometry.as_ref()).filter(|g| g.kind() != ds.geometry_type).count();
        if other > 0 {
            report
                .messages
                .push(format!("{other} rows have another kind of geometry than the layer's {}", ds.geometry_type.label().to_lowercase()));
        }
    }
    // Coordinates in another system become longitude/latitude.
    let crs = match (opts.crs, srid) {
        (Some(c), _) => Some(c),
        (None, Some(code)) => match crate::raster::Crs::from_epsg(code) {
            Some(c) => Some(c),
            None => {
                report.messages.push(crate::loss::Note::loss(
                    crate::loss::LossCode::AssumedCrs,
                    format!("unknown coordinate system SRID {code}; coordinates are read as longitude/latitude"),
                ));
                None
            }
        },
        (None, None) => None,
    };
    if let Some(c) = crs.filter(|c| *c != crate::raster::Crs::LonLat) {
        let t = c.transformer().map_err(DataError::Format)?;
        ds.crs = Some(c);
        // Kept as the file holds them, in its system; counted where they have no place in
        // longitude/latitude.
        ds.held_in = c;
        let (mut lost, mut placed, mut beyond) = (0usize, 0usize, 0usize);
        for g in ds.features.iter().filter_map(|f| f.geometry.as_ref()) {
            match places_lonlat_extent(g, &t) {
                Some(e) => {
                    placed += 1;
                    beyond += usize::from(e.beyond_lonlat());
                }
                None => lost += 1,
            }
        }
        report.messages.extend(kept_unplaced_note("rows", lost, placed == 0, c));
        report.messages.extend(beyond_lonlat_note("rows", beyond, c));
    }
    // Read as longitude/latitude as they are: no system given, or longitude/latitude itself. A
    // row a given system can't place is said above, as kept.
    let read_as_degrees = crs.is_none_or(|c| c == Crs::LonLat);
    let (mut outside, mut zero) = (0usize, 0usize);
    for f in &ds.features {
        crate::progress::check()?;
        if let Some(g) = &f.geometry {
            outside += usize::from(read_as_degrees && out_of_lonlat(g));
            zero += usize::from(matches!(g, Geometry::Point(p) if p.x == 0.0 && p.y == 0.0));
        }
    }
    if outside > 0 {
        let rows = if outside == 1 { "1 row has".to_string() } else { format!("{outside} rows have") };
        report.messages.push(format!(
            "{rows} coordinates that aren't longitude/latitude (outside ±180° longitude or ±90° latitude): give the file's coordinate system (\"crs\", such as EPSG:2263) to place them"
        ));
    }
    if zero > 0 {
        let rows = if zero == 1 { "1 row was".to_string() } else { format!("{zero} rows were") };
        report.messages.push(format!("{rows} placed at 0,0; these may be missing coordinates (the points were kept)"));
    }
    report.unread_dates = crate::source::unread_dates_note(&ds.fields, &unread_dates);
    report.messages.extend(report.unread_dates.clone());
    report.messages.extend(crate::source::null_text_note(&ds.fields, &null_text));
    Ok((ds, report))
}

/// Whether a geometry has a position that could be longitude/latitude with the two swapped: its x
/// within ±180.5° and its y past the poles (beyond ±90.5°, as the readers check). Eastings and
/// northings, with x far past 180 too, aren't taken for that. Each position is judged on its own, so
/// another position elsewhere doesn't hide it.
pub(crate) fn latitude_past_poles(g: &Geometry) -> bool {
    let mut past = false;
    g.for_each_coord(|c| past |= c.x.abs() <= 180.5 && c.y.abs() > 90.5);
    past
}

/// The notice for `n` features with a latitude past the poles (none for 0).
pub(crate) fn swapped_latitude_note(n: usize) -> Option<String> {
    let tail = "a latitude past ±90°: longitude and latitude may be swapped";
    match n {
        0 => None,
        1 => Some(format!("1 feature has {tail}")),
        n => Some(format!("{n} features have {tail}")),
    }
}

/// Whether a geometry has coordinates outside longitude/latitude's range.
fn out_of_lonlat(g: &Geometry) -> bool {
    let e = g.extent();
    e.min_x < -180.0 || e.max_x > 180.0 || e.min_y < -90.0 || e.max_y > 90.0
}

/// The type of a column of text values: Integer or Double when every non-empty value is one,
/// otherwise (or with no values) Text. A number whose digits a Double doesn't keep, such as a
/// 20-digit identifier, keeps its column Text, so no digit is rounded away ([`double_holds`]).
pub(crate) fn text_type<'a>(vals: impl Iterator<Item = &'a str>) -> FieldType {
    let mut column = TextColumn::default();
    vals.for_each(|v| column.add(v));
    column.ty()
}

/// [`text_type`] a value at a time, so a column written a chunk at a time is typed as one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TextColumn {
    ty: Option<FieldType>,
    /// A whole number a Double can't hold: fine in an Integer column, not in a Double one.
    whole_past_double: bool,
}

impl TextColumn {
    /// Adds a value's text; one that is empty once trimmed types nothing.
    fn add(&mut self, v: &str) {
        let v = v.trim();
        if v.is_empty() {
            return;
        }
        let t = if has_leading_zero(v) {
            // Identifiers such as ZIP codes keep their leading zeros.
            FieldType::Text
        } else if let Ok(i) = v.parse::<i64>() {
            // A whole number is held when its Double is the same number (in i128: no saturation).
            self.whole_past_double |= (i as f64) as i128 != i128::from(i);
            FieldType::Integer
        } else if double_holds(v) {
            FieldType::Double
        } else {
            FieldType::Text
        };
        self.ty = Some(self.ty.map_or(t, |prev| widen(prev, Some(t))));
        self.settle();
    }

    /// Adds another chunk of the column.
    fn merge(&mut self, other: Self) {
        self.whole_past_double |= other.whole_past_double;
        self.ty = match self.ty {
            None => other.ty,
            Some(prev) => Some(widen(prev, other.ty)),
        };
        self.settle();
    }

    /// Whether the column is Text whatever values follow.
    fn is_text(self) -> bool {
        self.ty == Some(FieldType::Text)
    }

    /// One state for a Text column, however its values came: a column skips the values after it
    /// is Text, and its chunks then compare equal to the whole.
    fn settle(&mut self) {
        if self.is_text() {
            self.whole_past_double = false;
        }
    }

    /// The column's type: Text without values.
    fn ty(self) -> FieldType {
        match self.ty {
            Some(FieldType::Double) if self.whole_past_double => FieldType::Text,
            ty => ty.unwrap_or(FieldType::Text),
        }
    }
}

/// Whether a number's text reads as a finite Double that, written with as many significant digits,
/// gives the same digits back, so no digit is lost: `1.50`, `0.1`, `1e300` and
/// `0.10000000000000001` (0.1 to 17 digits) and `0.00000000023283064365386962890625` (2^-32
/// exactly) are; `12345678901234567891` and `0.12345678901234567891` aren't. No Double's exact
/// value has more than 767 significant digits, so longer text isn't one, and isn't formatted.
fn double_holds(v: &str) -> bool {
    /// The digits of a number's mantissa without leading or trailing zeros.
    fn digits(s: &str) -> String {
        let mantissa = s.split(['e', 'E']).next().unwrap_or_default();
        let all: String = mantissa.chars().filter(char::is_ascii_digit).collect();
        all.trim_start_matches('0').trim_end_matches('0').to_string()
    }
    // A whole number written as one is held only exactly: `12345678901234567000` is a Double's
    // 17 digits and zeros, but that Double is 12345678901234567168.
    let whole = v.strip_prefix(['-', '+']).unwrap_or(v);
    if !whole.is_empty() && whole.bytes().all(|b| b.is_ascii_digit()) {
        // A whole Double's exact digits, written out (no finite Double has more than 309).
        let digits = whole.trim_start_matches('0');
        let digits = if digits.is_empty() { "0" } else { digits };
        return digits.len() <= 309 && v.parse::<f64>().is_ok_and(|x| x.is_finite() && format!("{:.0}", x.abs()) == digits);
    }
    let want = digits(v);
    v.parse::<f64>()
        .is_ok_and(|x| x.is_finite() && (want.is_empty() || want.len() <= 767 && digits(&format!("{x:.*e}", want.len() - 1)) == want))
}

/// Types a dataset's Text columns from their values (see [`text_type`]), for readers whose
/// formats hold every value as text (KML, GPX); the columns named in `keep` stay text.
pub(crate) fn type_text_columns(ds: &mut Dataset, keep: &[&str]) {
    for i in 0..ds.fields.len() {
        if ds.fields.get(i).is_none_or(|f| f.ty != FieldType::Text || keep.iter().any(|k| f.name.eq_ignore_ascii_case(k))) {
            continue;
        }
        let ty = text_type(ds.features.iter().filter_map(|f| match f.attrs.get(i) {
            Some(Value::Text(t)) => Some(t.as_str()),
            _ => None,
        }));
        if ty == FieldType::Text {
            continue;
        }
        if let Some(f) = ds.fields.get_mut(i) {
            f.ty = ty;
        }
        for f in &mut ds.features {
            if let Some(v) = f.attrs.get_mut(i)
                && let Value::Text(t) = v
            {
                *v = if t.trim().is_empty() { Value::Null } else { Value::parse_source(t, ty) };
            }
        }
    }
}

/// `0123` or `-007` (but not `0`, `0.5` or `-0.5`): a code, not a number.
fn has_leading_zero(v: &str) -> bool {
    let d = v.strip_prefix(['-', '+']).unwrap_or(v);
    d.len() > 1 && d.starts_with('0') && d.as_bytes().get(1).is_some_and(|b| b.is_ascii_digit())
}

/// A number with more than 15 significant digits (e.g. an account or card number, or a decimal
/// such as `+1234567890123456.5`); spreadsheets retain only 15, so these must be apostrophe-guarded in CSV export.
/// Redundant fractional trailing zeros (e.g. `+35.13294600000000`) do not count towards precision loss.
fn is_long_number(v: &str) -> bool {
    let mantissa = v.split(['e', 'E']).next().unwrap_or_default();
    let (whole, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let frac = frac.trim_end_matches('0');
    let mut digits = String::new();
    digits.extend(whole.chars().filter(char::is_ascii_digit));
    digits.extend(frac.chars().filter(char::is_ascii_digit));
    digits.trim_start_matches('0').len() > 15
}

/// Undoes the export formula guard: `'=x` reads back as `=x`.
fn unguard(cell: &str) -> std::borrow::Cow<'_, str> {
    match cell.strip_prefix('\'') {
        Some(rest) if rest.starts_with(FORMULA_START) => std::borrow::Cow::Owned(rest.to_string()),
        _ => std::borrow::Cow::Borrowed(cell),
    }
}

const FORMULA_START: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// Text needs an apostrophe if it starts like a spreadsheet formula. The numeric exception
/// keeps finite numbers that fit Double precision and have at most 15 significant digits.
/// A leading-zero code stays guarded unless its mantissa has digits after a decimal point.
fn csv_needs_guard(text: &str) -> bool {
    if !text.starts_with(FORMULA_START) {
        return false;
    }
    let mantissa = text.split(['e', 'E']).next().unwrap_or_default();
    let fraction = mantissa.split_once('.').is_some_and(|(_, frac)| !frac.is_empty());
    !((!has_leading_zero(text) || fraction)
        && text.parse::<f64>().is_ok_and(|f| f.is_finite())
        && !is_long_number(text)
        && double_holds(text))
}

/// A cell's file text, with an apostrophe when the spreadsheet guard applies.
fn csv_cell(v: &Value) -> String {
    match v {
        Value::Text(t) if csv_needs_guard(t) => format!("'{t}"),
        other => other.to_file_text(),
    }
}

/// The names of a point layer's longitude and latitude columns in CSV: the first of the names
/// reading takes for each that no field has, ignoring case and the spaces around a name (reading
/// trims them).
/// A layer with a field of every such name gets `longitude_1` or `latitude_1`, and reading takes
/// that field instead ([`CsvLosses::coordinates_note`]).
fn csv_coordinate_names(fields: &[Field]) -> [String; 2] {
    let taken = |name: &str| fields.iter().any(|f| f.name.trim().eq_ignore_ascii_case(name));
    let pick = |base: &str, known: &[&str]| {
        let name = known.iter().map(|n| n.to_string()).chain((1..).map(|i| format!("{base}_{i}"))).find(|n| !taken(n));
        name.unwrap_or_default()
    };
    [pick("longitude", LON_NAMES), pick("latitude", LAT_NAMES)]
}

/// The names of a point layer's x and y columns in CSV written in a projected system: `x` and
/// `y`, which reading takes first when given a projected system, else `easting` and `northing`,
/// else `x_1` and `y_1` and so on, the first that no field has (ignoring case and spaces).
fn csv_projected_names(fields: &[Field]) -> [String; 2] {
    let taken = |name: &str| fields.iter().any(|f| f.name.trim().eq_ignore_ascii_case(name));
    let pick = |base: &str, other: &str| {
        let name = [base.to_string(), other.to_string()].into_iter().chain((1..).map(|i| format!("{base}_{i}"))).find(|n| !taken(n));
        name.unwrap_or_default()
    };
    [pick("x", "easting"), pick("y", "northing")]
}

/// A point layer's coordinate column names in CSV written in `crs`: longitude and latitude ones
/// for a geographic system, x and y ones for a projected one.
fn csv_names_in(fields: &[Field], crs: Crs) -> [String; 2] {
    if crs.is_geographic() { csv_coordinate_names(fields) } else { csv_projected_names(fields) }
}

/// A dataset as CSV text: its fields, and a point layer's longitude and latitude ([`CsvWriter`]).
pub fn to_csv(ds: &Dataset) -> Result<String> {
    to_csv_in(ds, Crs::LonLat)
}

/// As [`to_csv`], for a dataset whose coordinates are in `crs`: a projected system's go in x and y
/// columns ([`csv_projected_names`]).
pub fn to_csv_in(ds: &Dataset, crs: Crs) -> Result<String> {
    let mut w = CsvWriter::new(Vec::new(), &ds.fields, ds.geometry_type, crs)?;
    w.write(&ds.features)?;
    String::from_utf8(w.finish()?).map_err(|e| DataError::Format(e.to_string()))
}

/// Writes CSV a run of features at a time: a header of the fields, and for a point layer
/// longitude and latitude columns before them.
pub(crate) struct CsvWriter<W: std::io::Write> {
    w: csv::Writer<W>,
    point: bool,
}

impl<W: std::io::Write> CsvWriter<W> {
    /// Writes the header for `fields`, with coordinate columns for a point layer, named for
    /// `crs`, the system its features' coordinates are in ([`csv_names_in`]).
    pub(crate) fn new(out: W, fields: &[Field], geometry_type: GeometryType, crs: Crs) -> Result<Self> {
        let mut w = csv::Writer::from_writer(out);
        let point = geometry_type == GeometryType::Point;
        let mut head = Vec::new();
        if point {
            // The coordinate columns come first, before any attributes using recognised names.
            head.extend(csv_names_in(fields, crs));
        }
        head.extend(fields.iter().map(|f| f.name.clone()));
        w.write_record(&head).map_err(|e| DataError::Format(e.to_string()))?;
        Ok(CsvWriter { w, point })
    }

    /// Writes the next features, a row each.
    pub(crate) fn write(&mut self, features: &[crate::Feature]) -> Result<()> {
        for f in features {
            crate::progress::step(1)?;
            let mut row = Vec::with_capacity(f.attrs.len() + 2);
            if self.point {
                let c = f.geometry.as_ref().and_then(|g| g.label_point());
                row.push(c.map(|c| c.x.to_string()).unwrap_or_default());
                row.push(c.map(|c| c.y.to_string()).unwrap_or_default());
            }
            row.extend(f.attrs.iter().map(csv_cell));
            self.w.write_record(&row).map_err(|e| DataError::Format(e.to_string()))?;
        }
        Ok(())
    }

    /// Flushes the rows and gives the writer back.
    pub(crate) fn finish(self) -> Result<W> {
        self.w.into_inner().map_err(|e| DataError::Format(e.to_string()))
    }
}

// ---------------------------------------------------------------------------------------------
// Shapefile

/// A DBF picked by its path opens the Shapefile beside it when present. Try every ASCII
/// spelling of the extension without scanning a directory; standalone tables return `None`.
pub fn shapefile_for_dbf(path: &Path) -> Option<std::path::PathBuf> {
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dbf")) {
        return None;
    }
    ["shp", "SHP", "Shp", "sHp", "shP", "SHp", "ShP", "sHP"].iter().map(|e| path.with_extension(e)).find(|p| p.is_file())
}

/// Finds the attributes beside a Shapefile, accepting every ASCII case of `.dbf` in whole
/// and streamed reads. A missing sidecar keeps the usual lowercase path for the error.
pub fn dbf_sidecar(path: &Path) -> std::path::PathBuf {
    ["dbf", "DBF", "Dbf", "dBf", "dbF", "DBf", "DbF", "dBF"]
        .iter()
        .map(|e| path.with_extension(e))
        .find(|p| p.is_file())
        .unwrap_or_else(|| path.with_extension("dbf"))
}

/// Reads a Shapefile with an optional source CRS replacing its `.prj`, before any geometry is
/// converted. This changes only interpretation; the files on disk are never modified.
pub fn read_shapefile_in(path: &Path, name: &str, crs: Option<Crs>) -> Result<(Dataset, ReadReport)> {
    check_size_with(path, shapefile_size_error)?;
    let dbf = dbf_sidecar(path);
    if !dbf.is_file() {
        return Err(DataError::Format(format!("Shapefile: {} is missing", dbf.display())));
    }
    check_size_with(&dbf, shapefile_size_error)?;
    let shx = path.with_extension("shx");
    if shx.is_file() {
        check_size_with(&shx, shapefile_size_error)?;
    }
    let mut notes = crate::loss::Notes::new();
    let parts = ShapefileParts {
        shp: crate::provenance::read(path)?,
        shx: if shx.is_file() { Some(crate::provenance::read(shx)?) } else { None },
        dbf: crate::provenance::read(dbf)?,
        prj: crate::raster::read_prj(path),
        cpg: crate::dbf::read_cpg(path, &mut notes),
    };
    let (ds, mut report) = read_shapefile_parts(parts, name, crs)?;
    report.messages.extend(notes.prefixed(name));
    Ok((ds, report))
}

/// The files of a Shapefile besides its `.shp` that [`gather_shapefiles`] zips with it.
const SHAPEFILE_SIDECARS: [&str; 4] = ["dbf", "shx", "prj", "cpg"];

/// Gathers each Shapefile among files given together (several picked at once in the browser, or
/// dropped at once) into a zip that reads as a zipped Shapefile: a `.shp` with the `.dbf` of the
/// same base name, ignoring case, and its `.shx`, `.prj` and `.cpg` when they are there. The zip,
/// named after the `.shp`, takes its place; its parts go. Other files, and a `.shp` without its
/// `.dbf`, pass through as they came. A standalone `.dbf` with a `.cpg` is zipped with it too.
/// Files whose parts are too large to read together (more than [`max_bytes`], or the zip
/// reader's limit) are left out, with an explanation in the messages returned with the files.
pub fn gather_shapefiles(files: Vec<(String, Vec<u8>)>) -> (Vec<(String, Vec<u8>)>, Vec<String>) {
    let ext_of = |n: &str| Path::new(n).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
    let stem = |n: &str| Path::new(n).file_stem().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    let sidecar = |files: &[Option<(String, Vec<u8>)>], base: &str, ext: &str| {
        files.iter().position(|f| f.as_ref().is_some_and(|(n, _)| ext_of(n) == ext && stem(n) == base))
    };
    // Files are taken out as they're zipped, so none is copied twice; each zip goes where its
    // `.shp` was.
    let mut files: Vec<Option<(String, Vec<u8>)>> = files.into_iter().map(Some).collect();
    let mut zips: Vec<Option<Vec<(String, Vec<u8>)>>> = files.iter().map(|_| None).collect();
    for i in 0..files.len() {
        let Some(base) = files.get(i).and_then(Option::as_ref).filter(|(n, _)| ext_of(n) == "shp").map(|(n, _)| stem(n)) else {
            continue;
        };
        if sidecar(&files, &base, "dbf").is_none() {
            continue;
        }
        let mut parts: Vec<(String, Vec<u8>)> = files.get_mut(i).and_then(Option::take).into_iter().collect();
        for ext in SHAPEFILE_SIDECARS {
            if let Some(part) = sidecar(&files, &base, ext).and_then(|j| files.get_mut(j)).and_then(Option::take) {
                parts.push(part);
            }
        }
        if let Some(slot) = zips.get_mut(i) {
            *slot = Some(parts);
        }
    }
    // A standalone DBF's .cpg travels through the browser's Services inbox in the same zip.
    for i in 0..files.len() {
        let Some(base) = files.get(i).and_then(Option::as_ref).filter(|(n, _)| ext_of(n) == "dbf").map(|(n, _)| stem(n)) else {
            continue;
        };
        let Some(j) = sidecar(&files, &base, "cpg") else { continue };
        let mut parts: Vec<_> = files.get_mut(i).and_then(Option::take).into_iter().collect();
        parts.extend(files.get_mut(j).and_then(Option::take));
        if let Some(slot) = zips.get_mut(i) {
            *slot = Some(parts);
        }
    }
    // The zip must open again: within the read size and the zip reader's inflated total.
    // Read once for the whole run: it reads what the computer has free (#2431).
    let max = max_bytes();
    let limit = crate::zip::MAX_TOTAL.min(usize::try_from(max).unwrap_or(usize::MAX));
    let (mut out, mut problems) = (Vec::new(), Vec::new());
    for (file, parts) in files.into_iter().zip(zips) {
        out.extend(file);
        let Some(parts) = parts else {
            continue;
        };
        let shp = parts.first().map(|(n, _)| n.clone()).unwrap_or_else(|| "layer.shp".into());
        let zip_name = format!("{}.zip", Path::new(&shp).file_stem().and_then(|s| s.to_str()).unwrap_or("layer"));
        let total = parts.iter().fold(0usize, |sum, (_, b)| sum.saturating_add(b.len()));
        // Said by the limit it passed: the size read into memory (which follows the computer's
        // memory), or the most a zip expands to.
        if total as u64 > max {
            problems.push(format!("{shp}: its data files together: {}", too_large_said(total as u64, max)));
            continue;
        }
        if total > limit {
            problems.push(format!(
                "{shp}: its data files hold {} MiB in all; at most {} MiB is expanded from a zip",
                total.div_ceil(1 << 20),
                limit >> 20
            ));
            continue;
        }
        match crate::zip::write(&parts) {
            Ok(zip) if zip.len() as u64 <= max => out.push((zip_name, zip)),
            Ok(zip) => problems.push(format!("{shp}: zipped together: {}", too_large_said(zip.len() as u64, max))),
            Err(e) => problems.push(format!("{shp}: its data files couldn't be zipped together: {e}")),
        }
    }
    (out, problems)
}

/// The addresses of a Shapefile's files beside its `.shp`, by the `.shp`'s address (#1238): the
/// address with each of [`SHAPEFILE_SIDECARS`] in place of `.shp`, its extension in upper case
/// when the `.shp`'s is (`.SHP`), its query and fragment kept (a signed address stays signed).
/// `None` for an address whose path doesn't end in a file named `.shp`, for one without a scheme
/// (`rios.shp`, a file on this computer), and for one with no path after its host
/// (`https://x.org.shp`, `s3://bucket.shp`).
pub fn shapefile_part_urls(url: &str) -> Option<Vec<(&'static str, String)>> {
    let (path, rest) = url.split_at(url.find(['?', '#']).unwrap_or(url.len()));
    let stem = path.get(..path.len().checked_sub(4)?)?;
    let ext = path.get(stem.len()..)?;
    // A path after the host: `https://x.org.shp` names a host, whose parts would be other hosts.
    // A saved S3 connection's endpoint has its own `://`, so the check looks inside the wrapper.
    let has_path = crate::s3_address::inner(stem).split_once("://").is_some_and(|(_, rest)| rest.contains('/'));
    if !ext.eq_ignore_ascii_case(".shp") || stem.ends_with('/') || !has_path {
        return None;
    }
    let upper = ext == ".SHP";
    Some(
        SHAPEFILE_SIDECARS
            .iter()
            .map(|&e| (e, format!("{stem}.{}{rest}", if upper { e.to_ascii_uppercase() } else { e.to_string() })))
            .collect(),
    )
}

/// A Shapefile fetched by its files' addresses as one zipped Shapefile ([`gather_shapefiles`]):
/// the `.shp` named `file_name`, and the parts that answered, by extension. An error when the
/// `.dbf` isn't among them or they are too large to read together.
pub fn zip_shapefile_parts(file_name: &str, shp: Vec<u8>, parts: Vec<(&str, Vec<u8>)>) -> std::result::Result<Vec<u8>, String> {
    let stem = Path::new(file_name).file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()).unwrap_or("layer");
    if !parts.iter().any(|(e, _)| e.eq_ignore_ascii_case("dbf")) {
        return Err(format!("{stem}.shp: its .dbf isn't beside it"));
    }
    let mut files = vec![(format!("{stem}.shp"), shp)];
    files.extend(parts.into_iter().map(|(e, b)| (format!("{stem}.{}", e.to_ascii_lowercase()), b)));
    let (out, problems) = gather_shapefiles(files);
    out.into_iter()
        .find(|(n, _)| n.ends_with(".zip"))
        .map(|(_, b)| b)
        .ok_or_else(|| if problems.is_empty() { format!("{stem}.shp: its files couldn't be read together") } else { problems.join("; ") })
}

/// Whether a zip entry is a macOS resource fork (`__MACOSX/`, `._*`), which isn't read.
fn is_resource_fork(name: &str) -> bool {
    name.split('/').any(|part| part == "__MACOSX" || part.starts_with("._"))
}

/// Which of its optional parts (`.shx`, `.prj`, `.cpg`) each Shapefile in a zip lacks, beside it
/// by its name: what a Shapefile added by its `.shp` address (#1238) was read without, to say so.
/// One entry per `.shp` that lacks any, its name as in the zip, in name order. Only the `.shp`s
/// [`read_zip`] reads count: those with their `.dbf`, not a macOS resource fork, the first
/// [`MAX_ZIP_LAYERS`] in name order (the reader counts those it reads, so past a damaged one it
/// reads one more than these). Empty for bytes that aren't a zip.
pub fn shapefile_zip_lacks(bytes: &[u8]) -> Vec<(String, Vec<&'static str>)> {
    // The entries' names only: `want` sees each, and none is inflated.
    let names = std::cell::RefCell::new(Vec::new());
    if crate::zip::read_matching(bytes, |n| {
        if !is_resource_fork(n) {
            names.borrow_mut().push(n.to_string());
        }
        false
    })
    .is_err()
    {
        return Vec::new();
    }
    let names = names.into_inner();
    // A set: a served zip may hold up to 65,535 entries.
    let lower: std::collections::HashSet<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    // The Shapefiles the reader takes: in name order, with their `.dbf`, as many as it reads.
    let mut shps: Vec<(&String, String)> = names
        .iter()
        .filter(|n| n.to_ascii_lowercase().ends_with(".shp"))
        .filter_map(|n| Some((n, n.get(..n.len() - 4)?.to_ascii_lowercase())))
        .filter(|(_, s)| lower.contains(&format!("{s}.dbf")))
        .collect();
    shps.sort();
    shps.into_iter()
        .take(MAX_ZIP_LAYERS)
        .filter_map(|(n, s)| {
            let lacks: Vec<&'static str> = ["shx", "prj", "cpg"].into_iter().filter(|e| !lower.contains(&format!("{s}.{e}"))).collect();
            (!lacks.is_empty()).then(|| (n.clone(), lacks))
        })
        .collect()
}

/// Most layers and standalone tables read together from one zip archive.
pub const MAX_ZIP_LAYERS: usize = 32;

/// A text table has matching CSV records, or an identifier-only header for an empty table.
/// Read records rather than counting separators: quoted commas and embedded newlines belong
/// to cells. A one-line prose readme with punctuation isn't an empty GTFS table.
fn delimited_table_columns(text: &str) -> usize {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.chars().take(4096).any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
        return 0;
    }
    let mut reader = csv::ReaderBuilder::new().delimiter(sniff_delimiter(text)).from_reader(text.as_bytes());
    let Ok(headers) = reader.headers() else { return 0 };
    let columns = headers.len();
    if columns == 0 {
        return 0;
    }
    let identifiers = headers.iter().all(|name| !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'));
    match reader.records().next() {
        Some(Ok(row)) if row.len() == columns && (columns > 1 || identifiers) => columns,
        None if identifiers => columns,
        _ => 0,
    }
}

/// Reads the vector data in a zip archive: each Shapefile in it (its `.shp` with the `.dbf`,
/// `.shx` and `.prj` beside it), then every other file the readers understand (GeoJSON, CSV,
/// GeoParquet, FlatGeobuf…; unpaired `.dbf` files become tables, and `.txt` files that sniff as
/// delimited text become tables or points), at most [`MAX_ZIP_LAYERS`] layers and tables together.
/// Folders inside are fine; macOS
/// resource forks (`__MACOSX/`, `._*`) are ignored.
pub fn read_zip(file_name: &str, bytes: &[u8]) -> Result<Vec<(Dataset, ReadReport)>> {
    read_zip_in(file_name, bytes, None)
}

/// Reads a ZIP, optionally overriding the source CRS of its Shapefiles and delimited text.
/// Other formats retain their own coordinate systems; a CityJSON model takes it only when it names none RustGIS
/// knows. The override is applied before reading any shapes. Cancellation is checked between
/// archive members and records; inflating one member cannot be interrupted mid-decompression.
pub fn read_zip_in(file_name: &str, bytes: &[u8], crs: Option<Crs>) -> Result<Vec<(Dataset, ReadReport)>> {
    read_zip_budget(file_name, bytes, crs, &mut csv_memory_budget())
}

/// Delimited members share one allowance, including gzip members whose text is nested in ZIP.
fn read_zip_budget(file_name: &str, bytes: &[u8], crs: Option<Crs>, budget: &mut usize) -> Result<Vec<(Dataset, ReadReport)>> {
    let max = max_bytes();
    if bytes.len() as u64 > max {
        return Err(DataError::Format(file_size_error(file_name, max)));
    }
    if let Some(c) = crs {
        c.transformer().map_err(DataError::Format)?;
    }
    let lower = |n: &str| n.to_ascii_lowercase();
    let ext_of = |n: &str| Path::new(n).extension().and_then(|e| e.to_str()).map(lower).unwrap_or_default();
    // A `.pbf` is read only as an OpenStreetMap extract (`.osm.pbf`): a vector tile set's
    // `z/x/y.pbf` files aren't extracted at all.
    let geodatabase = std::cell::Cell::new(false);
    let wanted = |n: &str| {
        if crate::progress::check().is_err() {
            return false;
        }
        if !is_resource_fork(n) && n.split('/').any(|part| part.to_ascii_lowercase().ends_with(".gdb")) {
            geodatabase.set(true);
            return false;
        }
        let e = ext_of(n);
        !is_resource_fork(n)
            && (matches!(e.as_str(), "shp" | "shx" | "dbf" | "prj" | "cpg" | "mid")
                || (e == "pbf" && lower(n).ends_with(".osm.pbf"))
                || (!matches!(e.as_str(), "zip" | "pbf") && READ_EXTENSIONS.contains(&e.as_str())))
    };
    let entries = crate::zip::read_matching(bytes, wanted).map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
    crate::progress::check()?;
    // Sidecar files by their path without the extension, compared without regard to case.
    let stem = |n: &str| lower(n.get(..n.len().saturating_sub(ext_of(n).len() + 1)).unwrap_or(n));
    // Entries are taken out as they're used, so no file's bytes are copied.
    let mut entries: Vec<Option<(String, Vec<u8>)>> = entries.into_iter().map(Some).collect();
    let mut shps: Vec<String> = entries.iter().flatten().filter(|(n, _)| ext_of(n) == "shp").map(|(n, _)| n.clone()).collect();
    shps.sort();
    let shp_stems: Vec<_> = shps.iter().map(|n| stem(n)).collect();
    let take = |entries: &mut Vec<Option<(String, Vec<u8>)>>, base: &str, ext: &str| -> Option<(String, Vec<u8>)> {
        entries.iter_mut().find(|e| e.as_ref().is_some_and(|(n, _)| ext_of(n) == ext && stem(n) == base))?.take()
    };
    let mut out = Vec::new();
    // Members that couldn't be read are skipped, and said in the first layer's report.
    let mut skipped: Vec<String> = Vec::new();
    for n in shps {
        if out.len() >= MAX_ZIP_LAYERS {
            skipped.push(format!("{n} (past the {MAX_ZIP_LAYERS}-layer limit)"));
            // Its `.dbf` goes with it, as a read one's does: it isn't a table of its own.
            let _ = take(&mut entries, &stem(&n), "dbf");
            continue;
        }
        let base = stem(&n);
        let name = Path::new(&n).file_stem().and_then(|s| s.to_str()).unwrap_or("layer").to_string();
        let shp = take(&mut entries, &base, "shp").map(|(_, b)| b).unwrap_or_default();
        let shx = take(&mut entries, &base, "shx").map(|(_, b)| b);
        let prj = take(&mut entries, &base, "prj").map(|(_, b)| String::from_utf8_lossy(&b).into_owned());
        let cpg = take(&mut entries, &base, "cpg").map(|(_, b)| b);
        let Some((_, dbf)) = take(&mut entries, &base, "dbf") else {
            skipped.push(format!("{n} (no .dbf beside it)"));
            continue;
        };
        match read_shapefile_parts(ShapefileParts { shp, shx, dbf, prj, cpg }, &name, crs) {
            Ok(mut read) => {
                read.0.source = Some(format!("{file_name}/{n}"));
                out.push(read);
            }
            Err(DataError::Cancelled) => return Err(DataError::Cancelled),
            Err(e) => skipped.push(format!("{n} ({e})")),
        }
    }
    // MapInfo files with their .mid.
    let mut mifs: Vec<String> = entries.iter().flatten().filter(|(n, _)| ext_of(n) == "mif").map(|(n, _)| n.clone()).collect();
    mifs.sort();
    for n in mifs {
        // The `.mif` files not taken stay in `entries`, so the loop below names them as past the limit.
        if out.len() >= MAX_ZIP_LAYERS {
            let _ = take(&mut entries, &stem(&n), "mid");
            continue;
        }
        let base = stem(&n);
        let name = Path::new(&n).file_stem().and_then(|s| s.to_str()).unwrap_or("layer").to_string();
        let mif = take(&mut entries, &base, "mif").map(|(_, b)| b).unwrap_or_default();
        let mid = take(&mut entries, &base, "mid").map(|(_, b)| b);
        match crate::mapinfo::read_bytes(&mif, mid.as_deref(), &name) {
            Ok(mut read) => {
                read.0.source = Some(format!("{file_name}/{n}"));
                out.push(read);
            }
            Err(DataError::Cancelled) => return Err(DataError::Cancelled),
            Err(e) => skipped.push(format!("{n} ({e})")),
        }
    }
    let mut others: Vec<(String, Vec<u8>)> = entries
        .iter_mut()
        .filter(|e| e.as_ref().is_some_and(|(n, _)| !matches!(ext_of(n).as_str(), "shp" | "shx" | "prj" | "cpg")))
        .filter_map(Option::take)
        .collect();
    others.sort_by(|a, b| a.0.cmp(&b.0));
    for (n, b) in others {
        if ext_of(&n) == "mid" {
            skipped.push(format!("{n} (no .mif beside it)"));
            continue;
        }
        if ext_of(&n) == "dbf" && shp_stems.contains(&stem(&n)) {
            skipped.push(format!("{n} (an extra .dbf sidecar of a Shapefile)"));
            continue;
        }
        // Prose readmes aren't tables; optional empty GTFS tables still have headers.
        if ext_of(&n) == "txt" {
            let columns = delimited_text(&n, &b).map_or(0, |(text, _)| delimited_table_columns(&text));
            let readme = n.rsplit('/').next().is_some_and(|leaf| leaf.eq_ignore_ascii_case("readme.txt"));
            if columns == 0 || (columns == 1 && readme) {
                skipped.push(format!("{n} (not recognised as a data table; extract it and open separately to read as text)"));
                continue;
            }
        }
        if out.len() >= MAX_ZIP_LAYERS {
            skipped.push(format!("{n} (past the {MAX_ZIP_LAYERS}-layer limit)"));
            continue;
        }
        let leaf = n.rsplit('/').next().unwrap_or(&n);
        // A city model has many layers.
        let read = if ext_of(&n) == "dbf" {
            let cpg = take(&mut entries, &stem(&n), "cpg").map(|(_, b)| b);
            let name = Path::new(leaf).file_stem().and_then(|s| s.to_str()).unwrap_or("table");
            read_dbf(b, cpg.as_deref(), name, budget).map(|r| vec![r])
        } else if is_city_model(leaf, &b) {
            read_city_member(leaf, &b, crs)
        } else if is_delimited_name(leaf) {
            read_delimited_bytes_budget(leaf, &b, &CsvOptions { crs, ..Default::default() }, budget).map(|r| vec![r])
        } else {
            read_bytes(leaf, &b).map(|r| vec![r])
        };
        match read {
            Ok(layers) => {
                for (mut ds, report) in layers {
                    if out.len() >= MAX_ZIP_LAYERS {
                        skipped.push(format!("{n}: {} (past the {MAX_ZIP_LAYERS}-layer limit)", ds.name));
                        continue;
                    }
                    ds.source = Some(format!("{file_name}/{n}"));
                    out.push((ds, report));
                }
            }
            // A run cancelled while a member was read stops the archive, not just that member.
            Err(DataError::Cancelled) => return Err(DataError::Cancelled),
            Err(e) => skipped.push(format!("{n} ({e})")),
        }
    }
    // At most this many skipped members are named; the rest are counted.
    const NAMED_SKIPPED: usize = 20;
    let mut skipped = if skipped.len() > NAMED_SKIPPED {
        let more = skipped.len() - NAMED_SKIPPED;
        let mut named: Vec<String> = skipped.into_iter().take(NAMED_SKIPPED).collect();
        named.push(format!("and {more} more"));
        named
    } else {
        skipped
    };
    if geodatabase.get() {
        skipped.push("File Geodatabase contents (unzip the archive and add the .gdb folder on the desktop)".into());
    }
    if out.is_empty() {
        let why = if skipped.is_empty() { String::new() } else { format!(": {}", skipped.join("; ")) };
        let hint = if skipped.is_empty() {
            "; unzip it and add a supported file (rename delimited text to .txt if it was not recognised)"
        } else {
            ""
        };
        return Err(DataError::Format(format!("{file_name}: no supported layers or tables in the archive could be read{why}{hint}")));
    }
    if let Some((_, report)) = out.first_mut()
        && !skipped.is_empty()
    {
        report.messages.push(format!("{file_name}: skipped {}", skipped.join("; ")));
    }
    Ok(out)
}

/// The same schema, decoder and numeric declarations for loose tables and Shapefile attributes.
struct DbfRead {
    reader: shapefile::dbase::Reader<std::io::Cursor<Vec<u8>>>,
    fields: Vec<Field>,
    decimals: Vec<u8>,
    report: ReadReport,
    /// For each field, its text values that aren't valid in the table's encoding
    /// ([`crate::dbf::not_utf8`]).
    not_utf8: Vec<usize>,
    /// The records marked deleted, by position, ascending: the reader gives them as any other
    /// ([`crate::dbf::take_deleted`]), and they are left out by position.
    deleted: Vec<usize>,
    /// The name of that encoding ([`crate::dbf::DbfEncoding::name`]), for the notice.
    encoding: &'static str,
}

/// Opens dBASE attributes with the Shapefile reader's code-page and date/number handling.
/// `name` is the layer's, for the error of a `.dbf` it can't open.
fn dbf_reader(mut bytes: Vec<u8>, cpg: Option<&[u8]>, name: &str) -> Result<DbfRead> {
    use std::io::Cursor;
    let mut declared = crate::loss::Notes::new();
    let encoding = crate::dbf::encoding(cpg, bytes.get(29).copied().unwrap_or(0), &mut declared);
    let mut encoding_notes = declared;
    // Counted before a date column is marked a character one below: its values aren't text.
    let (names, not_utf8) = crate::dbf::not_utf8(&bytes, &encoding);
    encoding_notes.extend(crate::dbf::not_utf8_names_note(&names, encoding.name()));
    let deleted = crate::dbf::take_deleted(&mut bytes);
    crate::dbf::readable_header_date(&mut bytes);
    let date_columns = crate::dbf::dates_as_text(&mut bytes);
    crate::dbf::floats_as_numeric(&mut bytes);
    let decimals = crate::dbf::field_decimals(&bytes);
    let dbf = shapefile::dbase::ReaderBuilder::new()
        .with_encoding(encoding.decoder())
        // A date's year may be padded with spaces on the left (`   10101`): keep them.
        .with_options(shapefile::dbase::ReadingOptions::default().character_trim(shapefile::dbase::TrimOption::End))
        .build(Cursor::new(bytes))
        .map_err(|e| DataError::Format(format!("{name}: the .dbf: {e}")))?;
    // Field order comes from the .dbf header (records are unordered maps).
    let order: Vec<(String, FieldType)> = dbf
        .fields()
        .iter()
        .filter(|f| f.name() != "DeletionFlag")
        .enumerate()
        .map(|(k, f)| {
            use shapefile::dbase::FieldType as D;
            let ty = match f.field_type() {
                _ if date_columns.contains(&k) => FieldType::Date,
                D::Numeric | D::Float | D::Double | D::Currency => FieldType::Double,
                D::Integer => FieldType::Integer,
                D::Logical => FieldType::Boolean,
                _ => FieldType::Text,
            };
            (f.name().to_string(), ty)
        })
        .collect();
    let fields = order.into_iter().map(|(n, t)| Field::new(n, t)).collect();
    let report = ReadReport { messages: encoding_notes, ..Default::default() };
    Ok(DbfRead { reader: dbf, fields, decimals, report, not_utf8, deleted, encoding: encoding.name() })
}

/// Reads a geometry-free dBASE table, checking cancellation and counting progress per record.
/// Charges retained rows and cell allocations, with room for vector capacity, against the
/// same budget as delimited tables; archive members share the remaining allowance.
/// The bounded file read and header decoding finish before cancellation can be observed.
fn read_dbf(bytes: Vec<u8>, cpg: Option<&[u8]>, name: &str, budget: &mut usize) -> Result<(Dataset, ReadReport)> {
    let DbfRead { mut reader, fields, decimals, mut report, not_utf8, deleted, encoding } = dbf_reader(bytes, cpg, name)?;
    let mut ds = Dataset::new(name, GeometryType::None, fields);
    let mut counts = DbfCounts::new(ds.fields.len(), not_utf8, encoding);
    let allowance = *budget;
    let mut left = allowance;
    crate::progress::begin(reader.header().num_records as usize);
    let (mut is_deleted, mut left_out) = (deleted_by_position(&deleted), 0);
    for (k, record) in reader.iter_records().enumerate() {
        crate::progress::step(1)?;
        if is_deleted(k) {
            left_out += 1;
            continue;
        }
        match record {
            Ok(record) => {
                let attrs = dbf_attrs(&record, &ds.fields, &mut counts);
                let cost = attrs
                    .iter()
                    .fold(
                        std::mem::size_of::<crate::Feature>().saturating_add(attrs.capacity().saturating_mul(std::mem::size_of::<Value>())),
                        |sum, value| {
                            sum.saturating_add(match value {
                                Value::Text(s) => s.capacity(),
                                _ => 0,
                            })
                        },
                    )
                    .saturating_mul(2);
                left = left.checked_sub(cost).ok_or_else(|| {
                    DataError::Format(format!(
                        "{name}: dBASE retained table exceeds the {allowance}-byte remaining memory budget; import a smaller file"
                    ))
                })?;
                ds.push_next(None, attrs);
            }
            // An I/O failure can leave dbase's iterator at the same record forever.
            Err(e) if matches!(e.kind(), shapefile::dbase::ErrorKind::IoError(_)) => {
                return Err(DataError::Format(format!("{name}: the .dbf: {e}")));
            }
            Err(_) => report.skipped += 1,
        }
    }
    report.messages.extend(deleted_records_note(name, left_out, false));
    dbf_notices(&ds, &mut report, &counts);
    integral_numeric_to_integer(&mut ds, &decimals);
    match report.skipped {
        0 => {}
        1 => report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedFeature,
            format!("{name}: 1 dBASE record was unreadable and left out"),
        )),
        n => report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedFeature,
            format!("{name}: {n} dBASE records were unreadable and left out"),
        )),
    }
    *budget = left;
    Ok((ds, report))
}

/// Whether the record at a position, asked in ascending order, is one of `deleted` (ascending).
fn deleted_by_position(deleted: &[usize]) -> impl FnMut(usize) -> bool + '_ {
    let mut next = deleted.iter().peekable();
    move |k| {
        while next.next_if(|d| **d < k).is_some() {}
        next.next_if(|d| **d == k).is_some()
    }
}

/// The notice for `n` records of `name`'s `.dbf` marked deleted and left out, and for a
/// Shapefile (`with_shapes`), their shapes with them.
fn deleted_records_note(name: &str, n: usize, with_shapes: bool) -> Option<String> {
    deleted_records_text(n, with_shapes).map(|t| format!("{name}: {t}"))
}

/// [`deleted_records_note`] without the layer's name.
pub(crate) fn deleted_records_text(n: usize, with_shapes: bool) -> Option<String> {
    let (what, shapes) = match n {
        0 => return None,
        1 => ("1 dBASE record marked deleted was left out".to_string(), ", with its shape"),
        n => (format!("{n} dBASE records marked deleted were left out"), ", with their shapes"),
    };
    Some(if with_shapes { format!("{what}{shapes}") } else { what })
}

/// Converts one record to typed values, sharing loss counts with the containing dataset.
fn dbf_attrs(record: &shapefile::dbase::Record, fields: &[Field], counts: &mut DbfCounts) -> Vec<Value> {
    use shapefile::dbase::FieldValue;
    let DbfCounts { unread_dates, nonfinite, null_text, .. } = counts;
    fields
        .iter()
        .enumerate()
        .zip(unread_dates.iter_mut().zip(nonfinite.iter_mut()))
        .map(|((k, f), (unread, nonfinite))| match record.get(&f.name) {
            // `<Null>` is a missing value's marker, in a date column too; other text keeps its
            // leading spaces (the reader drops only the padding after it).
            Some(FieldValue::Character(Some(s))) if Value::is_null_marker(s) => {
                crate::source::null_text_at(null_text, k);
                Value::Null
            }
            Some(FieldValue::Character(Some(s))) if f.ty == FieldType::Date => match crate::dbf::date(s.as_bytes()) {
                Ok(d) => d.map_or(Value::Null, Value::Date),
                Err(()) => {
                    *unread += 1;
                    Value::Null
                }
            },
            Some(FieldValue::Character(Some(s))) => Value::Text(s.clone()),
            Some(FieldValue::Numeric(Some(n))) if n.is_finite() => Value::Float(*n),
            Some(FieldValue::Numeric(Some(_))) => {
                *nonfinite += 1;
                Value::Null
            }
            Some(FieldValue::Integer(i)) => Value::Int(i64::from(*i)),
            Some(FieldValue::Double(d)) => Value::Float(*d),
            Some(FieldValue::Currency(d)) => Value::Float(*d),
            Some(FieldValue::Logical(Some(b))) => Value::Bool(*b),
            Some(FieldValue::Memo(s)) => Value::Text(s.clone()),
            _ => Value::Null,
        })
        .collect()
}

/// Values a `.dbf` read changed, counted for each field by its position: dates that weren't
/// ones, numbers that weren't finite, and text `<Null>`, all read as null; and text that wasn't
/// valid in the table's encoding, counted before the read ([`crate::dbf::not_utf8`]).
struct DbfCounts {
    unread_dates: Vec<usize>,
    nonfinite: Vec<usize>,
    null_text: Vec<usize>,
    not_utf8: Vec<usize>,
    /// The name of the encoding `not_utf8` is of.
    encoding: &'static str,
}

impl DbfCounts {
    fn new(fields: usize, not_utf8: Vec<usize>, encoding: &'static str) -> Self {
        DbfCounts { unread_dates: vec![0; fields], nonfinite: vec![0; fields], null_text: Vec::new(), not_utf8, encoding }
    }
}

/// Says the numbers in a `.dbf` that aren't finite (nan or inf), read as empty, with the count for
/// each field in `counts` (by field index).
pub(crate) fn nonfinite_note(fields: &[Field], counts: &[usize]) -> Option<crate::loss::Note> {
    let total: usize = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let what = if total == 1 {
        "1 number isn't finite (nan or inf) and was".to_string()
    } else {
        format!("{total} numbers aren't finite (nan or inf) and were")
    };
    Some(crate::loss::Note::loss(crate::loss::LossCode::DroppedValue, format!("{what} read as empty, in {}", named_fields(fields, counts))))
}

/// Reports date, numeric and `<Null>` cells read as null, and text that isn't UTF-8, identically
/// for tables and Shapefiles.
fn dbf_notices(ds: &Dataset, report: &mut ReadReport, counts: &DbfCounts) {
    let DbfCounts { unread_dates, nonfinite, null_text, not_utf8, encoding } = counts;
    let name = &ds.name;
    report.unread_dates = crate::source::unread_dates_note(&ds.fields, unread_dates);
    report.messages.extend(report.unread_dates.iter().map(|n| n.clone().prefixed(name)));
    report.messages.extend(nonfinite_note(&ds.fields, nonfinite).map(|n| n.prefixed(name)));
    report.messages.extend(crate::source::null_text_note(&ds.fields, null_text).map(|n| n.prefixed(name)));
    report.messages.extend(crate::source::not_utf8_note(&ds.fields, not_utf8, encoding).map(|n| n.prefixed(name)));
}

/// The files of a Shapefile, read into memory.
pub(crate) struct ShapefileParts {
    pub shp: Vec<u8>,
    pub shx: Option<Vec<u8>>,
    pub dbf: Vec<u8>,
    pub prj: Option<String>,
    pub cpg: Option<Vec<u8>>,
}

/// Decodes attributes and shapes together, tracking geometry lost during CRS conversion so
/// a later projection definition cannot silently repair an incomplete layer.
pub(crate) fn read_shapefile_parts(parts: ShapefileParts, name: &str, crs: Option<Crs>) -> Result<(Dataset, ReadReport)> {
    use shapefile::Shape;
    use std::io::Cursor;
    let fmt = |e: shapefile::Error| DataError::Format(format!("Shapefile: {e}"));
    let DbfRead { reader: mut dbf, fields, decimals, mut report, not_utf8, deleted, encoding } =
        dbf_reader(parts.dbf, parts.cpg.as_deref(), name)?;
    let mut shapes = match parts.shx {
        Some(shx) => shapefile::ShapeReader::with_shx(Cursor::new(parts.shp), Cursor::new(shx)),
        None => shapefile::ShapeReader::new(Cursor::new(parts.shp)),
    }
    .map_err(fmt)?;
    let mut ds = Dataset::new(name, GeometryType::None, fields);
    // A system given or in a `.prj` (even one that can't be used, said on its own) isn't one
    // RustGIS assumed.
    let declared = crs.is_some() || parts.prj.is_some();
    let to_lonlat = match crs {
        Some(c) => {
            ds.crs = Some(c);
            Some(c.transformer().map_err(DataError::Format)?)
        }
        None => shapefile_crs(parts.prj.as_deref(), &mut report)?.map(|(crs, t)| {
            ds.crs = Some(crs);
            t
        }),
    };
    let (mut converted, mut unconverted, mut zm_shapes, mut not_numbers) = (0usize, 0usize, 0usize, 0usize);
    // The box of the converted places in longitude/latitude, for the notice of a `.prj` that
    // doesn't match the coordinates.
    let mut beyond = 0usize;
    let mut counts = DbfCounts::new(ds.fields.len(), not_utf8, encoding);
    let mut source_kind = GeometryType::None;
    let pt = |x: f64, y: f64| Coord::new(x, y);
    // Paired by position: a record marked deleted is read as any other, and left out with its shape.
    let (mut is_deleted, mut left_out) = (deleted_by_position(&deleted), 0);
    // Each shape is read with its record, a shape that can't be read too: `shapefile`'s own pairing
    // returns a shape's error before reading its record, which shifts every later record. Shapes
    // past the last record, and records past the last shape, are counted.
    // Without a `.shx`, a shape that can't be read leaves the reader inside it: the shapes after it
    // can't be found, so the pairing stops there, and the records after it are said.
    let records = dbf.header().num_records as usize;
    let indexed = shapes.shape_count().is_ok();
    let mut lost_extra = false;
    let (mut shape_iter, mut record_iter) = (shapes.iter_shapes(), dbf.iter_records());
    let (mut pairs, mut extra_shapes, mut extra_unreadable, mut lost_track) = (0usize, 0usize, 0usize, false);
    while let Some(shape) = shape_iter.next() {
        let Some(record) = record_iter.next() else {
            // Counted, those that can't be read too; without a `.shx` up to the first of those,
            // past which the file can't be followed.
            let mut shape = shape;
            loop {
                crate::progress::check()?;
                extra_shapes += 1;
                if shape.is_err() {
                    extra_unreadable += 1;
                    if !indexed {
                        lost_extra = true;
                        break;
                    }
                }
                match shape_iter.next() {
                    Some(next) => shape = next,
                    None => break,
                }
            }
            break;
        };
        let k = pairs;
        pairs += 1;
        lost_track = shape.is_err() && !indexed;
        if is_deleted(k) {
            left_out += 1;
            if lost_track {
                break;
            }
            continue;
        }
        let (Ok(shape), Ok(record)) = (shape, record) else {
            report.skipped += 1;
            if lost_track {
                break;
            }
            continue;
        };
        zm_shapes += usize::from(!matches!(
            shape,
            Shape::NullShape | Shape::Point(_) | Shape::Multipoint(_) | Shape::Polyline(_) | Shape::Polygon(_) | Shape::Multipatch(_)
        ));
        let geom = match shape {
            Shape::NullShape => None,
            Shape::Point(p) => Some(Geometry::Point(pt(p.x, p.y))),
            Shape::PointM(p) => Some(Geometry::Point(pt(p.x, p.y))),
            Shape::PointZ(p) => Some(Geometry::Point(pt(p.x, p.y))),
            Shape::Multipoint(m) => Some(Geometry::MultiPoint(m.points().iter().map(|p| pt(p.x, p.y)).collect())),
            Shape::MultipointM(m) => Some(Geometry::MultiPoint(m.points().iter().map(|p| pt(p.x, p.y)).collect())),
            Shape::MultipointZ(m) => Some(Geometry::MultiPoint(m.points().iter().map(|p| pt(p.x, p.y)).collect())),
            Shape::Polyline(l) => Some(Geometry::Line(l.parts().iter().map(|p| p.iter().map(|q| pt(q.x, q.y)).collect()).collect())),
            Shape::PolylineM(l) => Some(Geometry::Line(l.parts().iter().map(|p| p.iter().map(|q| pt(q.x, q.y)).collect()).collect())),
            Shape::PolylineZ(l) => Some(Geometry::Line(l.parts().iter().map(|p| p.iter().map(|q| pt(q.x, q.y)).collect()).collect())),
            Shape::Polygon(p) => {
                Some(group_rings(p.rings().iter().map(|r| (r.points().iter().map(|q| pt(q.x, q.y)).collect(), is_outer(r))).collect()))
            }
            Shape::PolygonM(p) => {
                Some(group_rings(p.rings().iter().map(|r| (r.points().iter().map(|q| pt(q.x, q.y)).collect(), is_outer(r))).collect()))
            }
            Shape::PolygonZ(p) => {
                Some(group_rings(p.rings().iter().map(|r| (r.points().iter().map(|q| pt(q.x, q.y)).collect(), is_outer(r))).collect()))
            }
            Shape::Multipatch(_) => {
                report.skipped += 1;
                continue;
            }
        };
        if source_kind == GeometryType::None
            && let Some(g) = &geom
        {
            source_kind = g.kind();
        }
        // A coordinate that isn't a number (NaN, which some writers use for an empty point, or an
        // infinity) has no place: the shape keeps its attributes without its geometry, counted
        // (#1985). Checked before converting, so it isn't taken for a conversion that failed.
        let geom = match geom {
            Some(g) if !g.is_finite() => {
                not_numbers += 1;
                None
            }
            g => g,
        };
        // Kept as the file holds it, in its own system; counted when it has no place in
        // longitude/latitude.
        if let (Some(t), Some(g)) = (&to_lonlat, &geom) {
            let placed = places_lonlat_extent(g, t);
            if let Some(e) = &placed {
                beyond += usize::from(e.beyond_lonlat());
            }
            converted += usize::from(placed.is_some());
            unconverted += usize::from(placed.is_none());
        }
        let attrs = dbf_attrs(&record, &ds.fields, &mut counts);
        ds.push_next(geom, attrs);
    }
    ds.drop_zm(&mut report, zm_shapes);
    report.messages.extend(deleted_records_note(name, left_out, true));
    if extra_shapes > 0 {
        let unreadable = match (extra_unreadable, lost_extra) {
            (0, _) => String::new(),
            (_, true) => " (the last of them can't be read, and without a .shx the .shp isn't followed past it)".to_string(),
            (n, false) => format!(" ({n} of them can't be read)"),
        };
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedFeature,
            format!("{name}: {extra_shapes} shape(s) past the .dbf's last record have no attributes and were left out{unreadable}"),
        ));
    }
    if lost_track && records > pairs {
        report.messages.push(crate::loss::Note::loss(crate::loss::LossCode::DroppedFeature, format!(
            "{name}: shape {pairs} can't be read and the Shapefile has no .shx to find the next one by: the {} record(s) after it were left out",
            records.saturating_sub(pairs)
        )));
    } else if records > pairs && extra_shapes == 0 && !lost_track {
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedFeature,
            format!("{name}: {} dBASE record(s) past the .shp's last shape have no shape and were left out", records - pairs),
        ));
    }
    dbf_notices(&ds, &mut report, &counts);
    ds.geometry_type = dominant_type(&ds);
    // Shapes whose coordinates aren't numbers must not turn a spatial layer into an
    // attribute-only table.
    if ds.geometry_type == GeometryType::None && not_numbers > 0 {
        ds.geometry_type = source_kind;
    }
    integral_numeric_to_integer(&mut ds, &decimals);
    // Held in its own system when it has one RustGIS converts; only coordinates read as
    // longitude/latitude are checked for being so.
    if let Some(c) = ds.crs.filter(|_| to_lonlat.is_some()) {
        ds.held_in = c;
    }
    // Beyond longitude/latitude: as the file's shapes come out there from the `.prj`'s system
    // (one that doesn't match them), counted; or as they are without a system to convert from.
    match ds.crs.filter(|_| to_lonlat.is_some()) {
        Some(c) => report.messages.extend(beyond_lonlat_note("shapes", beyond, c)),
        None if ds.extent().beyond_lonlat() => {
            let text = "coordinates are not longitude/latitude; RustGIS reads them as WGS 84 and they may draw out of place";
            report.messages.push(if declared {
                crate::loss::Note::plain(text)
            } else {
                crate::loss::Note::loss(crate::loss::LossCode::AssumedCrs, text)
            });
        }
        None => {}
    }
    if report.skipped > 0 {
        report.messages.push(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedFeature,
            format!("{} shapes were unreadable or unsupported", report.skipped),
        ));
    }
    if not_numbers > 0 {
        report.messages.push(crate::loss::Note::loss(crate::loss::LossCode::DroppedGeometry, format!(
            "{name}: {not_numbers} shape(s) have a coordinate that isn't a number (NaN or infinite): their features are kept without geometry"
        )));
    }
    if let Some(c) = ds.crs.filter(|_| to_lonlat.is_some()) {
        report.messages.extend(kept_unplaced_note("shapes", unconverted, converted == 0, c));
    }
    Ok((ds, report))
}

/// Converts a Shapefile's coordinates to longitude/latitude, from the coordinate system in its
/// `.prj`: `None` for longitude/latitude, or when there's no `.prj` or it isn't understood (said in
/// the report). Another body's system that can't be read is an error, because its coordinates
/// would land in the wrong place as longitude/latitude.
fn shapefile_crs(prj: Option<&str>, report: &mut ReadReport) -> Result<Option<(Crs, crate::raster::Transformer)>> {
    let Some(prj) = prj else { return Ok(None) };
    let crs = match crate::raster::wkt::read_wkt(prj) {
        Ok((c, note)) => {
            if let Some(n) = note {
                report.messages.push(format!("the .prj file: {n}"));
            }
            c
        }
        Err(e) => {
            if let Some(refused) = planetary::refused_wkt(prj, &e) {
                return Err(DataError::Format(format!("the .prj file: {refused}")));
            }
            report.messages.push(crate::loss::Note::loss(
                crate::loss::LossCode::AssumedCrs,
                format!("the .prj file: {e}; coordinates are read as longitude/latitude"),
            ));
            return Ok(None);
        }
    };
    if crs == crate::raster::Crs::LonLat {
        return Ok(None);
    }
    match crs.transformer() {
        Ok(t) => Ok(Some((crs, t))),
        Err(e) => {
            report.messages.push(crate::loss::Note::loss(
                crate::loss::LossCode::AssumedCrs,
                format!("the .prj file: {e}; coordinates are read as longitude/latitude"),
            ));
            Ok(None)
        }
    }
}

/// A dBASE Numeric column with no decimals declared whose values are all whole numbers
/// within ±9.0e15 is read as Integer. A column with decimals declared (decimals > 0)
/// remains Double.
fn integral_numeric_to_integer(ds: &mut Dataset, decimals: &[u8]) {
    const LIMIT: f64 = 9.0e15;
    for fi in 0..ds.fields.len() {
        if ds.fields.get(fi).map(|f| f.ty) != Some(FieldType::Double) {
            continue;
        }
        // Decimals missing from a malformed header can't be ruled out: the column stays Double.
        if decimals.get(fi).is_none_or(|&d| d > 0) {
            continue;
        }
        let whole = ds.features.iter().all(|f| match f.attrs.get(fi) {
            Some(Value::Float(v)) => v.fract() == 0.0 && v.abs() < LIMIT,
            _ => true,
        });
        let any = ds.features.iter().any(|f| matches!(f.attrs.get(fi), Some(Value::Float(_))));
        if !whole || !any {
            continue;
        }
        if let Some(f) = ds.fields.get_mut(fi) {
            f.ty = FieldType::Integer;
        }
        for f in &mut ds.features {
            if let Some(v) = f.attrs.get_mut(fi)
                && let Value::Float(x) = *v
            {
                *v = Value::Int(x as i64);
            }
        }
    }
}

/// WGS 84 as ESRI WKT, for a Shapefile's `.prj`.
pub(crate) const WGS84_PRJ: &str = "GEOGCS[\"GCS_WGS_1984\",DATUM[\"D_WGS_1984\",SPHEROID[\"WGS_1984\",6378137.0,298.257223563]],PRIMEM[\"Greenwich\",0.0],UNIT[\"Degree\",0.0174532925199433]]";

/// The longest prefix of `s` whose text takes at most `max` bytes in `encoding`, cut at a
/// character boundary: what a `.dbf` in that code page holds of it.
fn cut_encoded<'a>(s: &'a str, max: usize, encoding: &crate::dbf::DbfEncoding) -> &'a str {
    use shapefile::dbase::encoding::Encoding;
    let encoder = encoding.decoder();
    let fits = |t: &str| encoder.encode(t).map_or(t.len(), |b| b.len()) <= max;
    if fits(s) {
        return s;
    }
    // The longest prefix that fits: a character takes at least one byte, so it is within `max`
    // characters, and a shorter prefix never takes more bytes.
    let ends: Vec<usize> = s.char_indices().map(|(i, _)| i).skip(1).take(max).chain([s.len()]).collect();
    let (mut lo, mut hi) = (0usize, ends.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if ends.get(mid - 1).is_some_and(|&e| fits(s.get(..e).unwrap_or(""))) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo.checked_sub(1).and_then(|i| ends.get(i)).and_then(|&e| s.get(..e)).unwrap_or("")
}

/// The longest prefix of `s` that fits in `max` bytes, cut at a character boundary.
pub(crate) fn cut(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

/// What an export to a Shapefile says when field names don't fit dBase's (at most 10 bytes,
/// unique without regard to case), or `None` when every name is kept: one notice, counting the
/// fields renamed and naming the first few as `'old' → 'new'`. `target` names what was written.
/// Line breaks, other control characters and the characters that reorder text (bidirectional
/// marks, embeddings, overrides and isolates) in a name or in `target` show as spaces, and a
/// long name only its first 40 bytes.
pub fn shapefile_names_note(fields: &[Field], target: &str) -> Option<String> {
    names_note(fields, dbf_names(fields), target, "a Shapefile (10 bytes at most, unique ignoring case)")
}

/// The column each field gets in a GeoPackage: its name, unless an earlier field's column or the
/// id or geometry column has it without regard to case, as SQLite compares column names; then
/// the name with `_1`, `_2`… added (a later `a` beside `A` becomes `a_1`).
pub fn geopackage_columns(fields: &[Field]) -> Vec<String> {
    let (fid, geom) = (free_column_name(fields, "fid"), free_column_name(fields, "geom"));
    let mut columns: Vec<String> = Vec::with_capacity(fields.len());
    for f in fields {
        let taken =
            |n: &str| columns.iter().any(|c| c.eq_ignore_ascii_case(n)) || n.eq_ignore_ascii_case(&fid) || n.eq_ignore_ascii_case(&geom);
        let name = (0..).map(|i| if i == 0 { f.name.clone() } else { format!("{}_{i}", f.name) }).find(|n| !taken(n));
        columns.push(name.unwrap_or_else(|| f.name.clone()));
    }
    columns
}

/// A name no field has, ignoring case (`base`, else `base_1`, `base_2`, …): a GeoPackage's id and
/// geometry columns.
pub(crate) fn free_column_name(fields: &[Field], base: &str) -> String {
    let taken = |n: &str| fields.iter().any(|f| f.name.eq_ignore_ascii_case(n));
    (0..).map(|i| if i == 0 { base.to_string() } else { format!("{base}_{i}") }).find(|n| !taken(n)).unwrap_or_else(|| base.to_string())
}

/// What an export to a GeoPackage says when field names differ only in case, which SQLite's
/// columns can't ([`geopackage_columns`]), or `None` when every name is kept; written as
/// [`shapefile_names_note`] is.
pub fn geopackage_names_note(fields: &[Field], target: &str) -> Option<String> {
    names_note(fields, geopackage_columns(fields), target, "a GeoPackage (unique ignoring case)")
}

/// One notice of the fields whose names became `names` in `target`, a file of the kind `what`
/// describes, or `None` when every name is kept.
fn names_note(fields: &[Field], names: Vec<String>, target: &str, what: &str) -> Option<String> {
    let renamed: Vec<(&str, String)> = fields.iter().zip(names).filter(|(f, n)| f.name != *n).map(|(f, n)| (f.name.as_str(), n)).collect();
    if renamed.is_empty() {
        return None;
    }
    // A name read from a file, like a path or layer name, can hold anything, at any length: the
    // notice stays one line, reads in the order it is written, and copies no more of a field name
    // than it shows.
    let target: String = target.chars().map(dbf_notice_char).collect();
    let listed: Vec<String> = renamed
        .iter()
        .take(5)
        .map(|(old, new)| {
            let (o, n) = (dbf_notice_name(old), dbf_notice_name(new));
            // A name made unique keeps its start and gains an ending, which a cut at the end
            // hides: cut in the middle instead.
            let (o, n) = if o == n || n.ends_with('…') { (notice_name_ends(old), notice_name_ends(new)) } else { (o, n) };
            format!("'{o}' → '{n}'")
        })
        .collect();
    let more = if renamed.len() > 5 { format!(" and {} more", renamed.len() - 5) } else { String::new() };
    let count = if renamed.len() == 1 { "1 field name was".to_string() } else { format!("{} field names were", renamed.len()) };
    Some(format!("{target}: {count} changed to fit {what}: {}{more}", listed.join(", ")))
}

/// What an export to `target` says of the date fields it writes as ISO 8601 text, losing the field
/// type: a Shapefile's date-time fields (a `.dbf` has date fields only). `None` for other formats,
/// whose dates are text anyway or are written as dates, or without such fields.
pub fn dates_as_text_note(fields: &[Field], target: &str) -> Option<String> {
    let ext = Path::new(target).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let as_text = |ty: FieldType| match ext.as_str() {
        "shp" | "zip" => ty == FieldType::DateTime,
        _ => false,
    };
    let dates: Vec<String> = fields.iter().filter(|f| as_text(f.ty)).map(|f| format!("'{}'", dbf_notice_name(&f.name))).collect();
    if dates.is_empty() {
        return None;
    }
    let target: String = target.chars().map(dbf_notice_char).collect();
    let listed: Vec<&str> = dates.iter().take(5).map(String::as_str).collect();
    let more = if dates.len() > 5 { format!(" and {} more", dates.len() - 5) } else { String::new() };
    let kind = if matches!(ext.as_str(), "shp" | "zip") { "date-time field" } else { "date field" };
    let count = if dates.len() == 1 { format!("1 {kind} was") } else { format!("{} {kind}s were", dates.len()) };
    Some(format!("{target}: {count} written as text (ISO 8601), not as dates: {}{more}", listed.join(", ")))
}

/// Counts, for each field, the date-times at an offset other than UTC's (`+02:00`), which a
/// GeoPackage holds in UTC: the moment is kept, the offset it was shown at isn't.
pub fn offset_dates<'a>(features: impl IntoIterator<Item = &'a crate::Feature>, fields: &[Field]) -> Vec<usize> {
    let offset = |v: &Value| matches!(v, Value::DateTime(t) if t.offset().is_some_and(|o| o != 0));
    let mut counts = vec![0usize; fields.len()];
    for f in features {
        for ((n, field), v) in counts.iter_mut().zip(fields).zip(&f.attrs) {
            // Text in a date field that reads as a date is written as that date (as a read takes it).
            let text = matches!(v, Value::Text(t) if field.ty.is_temporal() && offset(&Value::parse_stored_counted(t, field.ty).0));
            if offset(v) || text {
                *n += 1;
            }
        }
    }
    counts
}

/// Whether a value written to a date field of type `ty` has digits past the millisecond, which a
/// GeoPackage's DATETIME doesn't keep (text that reads as a date-time counts as that, digits past
/// the microsecond included).
pub(crate) fn past_millisecond(v: &Value, ty: FieldType) -> bool {
    // A moment in a date field is written as it is, and reads as null ([`text_in_dates`] counts
    // it); in any other field a date-time is written to the millisecond.
    if ty == FieldType::Date {
        return false;
    }
    let fine = |v: &Value| matches!(v, Value::DateTime(t) if t.micros().rem_euclid(1000) != 0);
    fine(v)
        || matches!(v, Value::Text(t) if ty.is_temporal() && {
            let (d, cut) = Value::parse_stored_counted(t, ty);
            cut || fine(&d)
        })
}

/// Counts, for each field, the date-times with digits past the millisecond, which a GeoPackage
/// holds rounded to the millisecond.
pub fn fine_dates<'a>(features: impl IntoIterator<Item = &'a crate::Feature>, fields: &[Field]) -> Vec<usize> {
    let mut counts = vec![0usize; fields.len()];
    for f in features {
        for ((n, field), v) in counts.iter_mut().zip(fields).zip(&f.attrs) {
            *n += usize::from(past_millisecond(v, field.ty));
        }
    }
    counts
}

/// What a GeoPackage export to `target` says of the date-times it rounded to the millisecond
/// ([`fine_dates`]). `None` when there were none.
pub fn fine_dates_note(fields: &[Field], counts: &[usize], target: &str) -> Option<String> {
    let total: usize = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let target: String = target.chars().map(dbf_notice_char).collect();
    let what = if total == 1 { "1 date-time was".to_string() } else { format!("{total} date-times were") };
    Some(format!("{target}: {what} rounded to the millisecond, as GeoPackage stores them, in {}", named_fields(fields, counts)))
}

/// What a GeoPackage export to `target` says of the date-times it wrote in UTC ([`offset_dates`]).
/// `None` when there were none.
pub fn utc_dates_note(fields: &[Field], counts: &[usize], target: &str) -> Option<String> {
    let total: usize = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let target: String = target.chars().map(dbf_notice_char).collect();
    let what = if total == 1 { "1 date-time was".to_string() } else { format!("{total} date-times were") };
    Some(format!(
        "{target}: {what} written in UTC, as GeoPackage stores them, without their own offsets, in {}",
        named_fields(fields, counts)
    ))
}

/// Counts, for each field, the values a FlatGeobuf export writes: non-null ones of features that
/// have geometry (the others aren't in the file).
pub fn written_values<'a>(features: impl IntoIterator<Item = &'a crate::Feature>, fields: &[Field]) -> Vec<usize> {
    let mut counts = vec![0usize; fields.len()];
    for f in features.into_iter().filter(|f| f.geometry.is_some()) {
        for (n, v) in counts.iter_mut().zip(&f.attrs) {
            *n += usize::from(!v.is_null());
        }
    }
    counts
}

/// What a FlatGeobuf export to `target` says of its date fields with no values written
/// ([`written_values`]): the format has no date type, so a reader tells dates from date-times by
/// the values, and these come back as date-time fields. `None` when there were none.
pub fn fgb_empty_dates_note(fields: &[Field], written: &[usize], target: &str) -> Option<String> {
    let empty: Vec<usize> =
        fields.iter().enumerate().map(|(i, f)| usize::from(f.ty == FieldType::Date && written.get(i).copied().unwrap_or(0) == 0)).collect();
    let total: usize = empty.iter().sum();
    if total == 0 {
        return None;
    }
    let target: String = target.chars().map(dbf_notice_char).collect();
    let what = if total == 1 { "1 date field has".to_string() } else { format!("{total} date fields have") };
    Some(format!(
        "{target}: {what} no values, and FlatGeobuf can't tell a date from a date-time, so it reads back as a date-time field, in {}",
        named_fields(fields, &empty)
    ))
}

/// Counts, for each date field, the values in it that a GeoPackage export writes in a form a read
/// gives back as null: text that isn't a date, a number or yes/no, and a date-time with a time or
/// zone in a date field.
pub fn text_in_dates<'a>(features: impl IntoIterator<Item = &'a crate::Feature>, fields: &[Field]) -> Vec<usize> {
    let mut counts = vec![0usize; fields.len()];
    for f in features {
        for ((n, field), v) in counts.iter_mut().zip(fields).zip(&f.attrs) {
            let text = matches!(v, Value::Text(t) if !v.asks_null() && Value::parse_stored_counted(t, field.ty).0.is_null());
            let moment = field.ty == FieldType::Date && matches!(v, Value::DateTime(t) if !t.is_date());
            let other = matches!(v, Value::Int(_) | Value::Float(_) | Value::Bool(_));
            if field.ty.is_temporal() && (text || moment || other) {
                *n += 1;
            }
        }
    }
    counts
}

/// What a GeoPackage export to `target` says of the text it wrote in date fields
/// ([`text_in_dates`]). `None` when there was none.
pub fn text_in_dates_note(fields: &[Field], counts: &[usize], target: &str) -> Option<String> {
    let total: usize = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let target: String = target.chars().map(dbf_notice_char).collect();
    let what = if total == 1 {
        "1 value isn't a date and was written as it is, and reads".to_string()
    } else {
        format!("{total} values aren't dates and were written as they are, and read")
    };
    Some(format!("{target}: {what} back empty, in {}", named_fields(fields, counts)))
}

/// The fields with a count, quoted, the first five named: `'a', 'b' and 3 more`.
pub(crate) fn named_fields(fields: &[Field], counts: &[usize]) -> String {
    let named: Vec<String> =
        fields.iter().zip(counts).filter(|(_, n)| **n > 0).map(|(f, _)| format!("'{}'", dbf_notice_name(&f.name))).collect();
    let more = if named.len() > 5 { format!(" and {} more", named.len() - 5) } else { String::new() };
    format!("{}{more}", named.iter().take(5).cloned().collect::<Vec<_>>().join(", "))
}

/// Keeps a diagnostic on one line and prevents field names from reordering its text.
pub fn dbf_notice_char(c: char) -> char {
    let reorders = matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    if c.is_control() || reorders || matches!(c, '\u{2028}' | '\u{2029}') { ' ' } else { c }
}

/// A field name as a notice shows it: cut after 40 bytes, with controls and directional marks
/// flattened. [`notice_name_ends`] cuts in the middle instead.
pub(crate) fn dbf_notice_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().map(dbf_notice_char) {
        if out.len() + c.len_utf8() > 40 {
            out.push('…');
            break;
        }
        out.push(c);
    }
    out
}

/// `name` as a notice shows it, flattened like [`dbf_notice_name`], but cut in the middle: its
/// first and last 20 bytes, so a name made unique by an ending shows it. Reads no more of the
/// name than it keeps.
fn notice_name_ends(name: &str) -> String {
    let len: usize = name.chars().map(|c| dbf_notice_char(c).len_utf8()).sum();
    if len <= 40 {
        return name.chars().map(dbf_notice_char).collect();
    }
    let fit = |chars: &mut dyn Iterator<Item = char>| {
        let mut len = 0;
        chars
            .map(dbf_notice_char)
            .take_while(|c| {
                len += c.len_utf8();
                len <= 20
            })
            .collect::<Vec<char>>()
    };
    let start: String = fit(&mut name.chars()).into_iter().collect();
    let end: String = fit(&mut name.chars().rev()).into_iter().rev().collect();
    format!("{start}…{end}")
}

/// Value losses counted independently for each dBase field, using only rows the writer keeps.
#[derive(Clone, Copy, Default)]
struct DbfLosses {
    text: usize,
    empty: usize,
    rounded: usize,
}

/// Counts text cut to 254 bytes, non-null values left empty, and numbers rounded to the field's
/// precision, in rows whose geometry is written. Each kind has one notice naming the affected
/// source fields (first five, bounded and flattened); a clean dataset gives none.
pub fn shapefile_values_notes(ds: &Dataset, target: &str) -> Vec<String> {
    let decimals: Vec<_> =
        ds.fields.iter().enumerate().map(|(k, f)| if f.ty == FieldType::Double { dbf_decimals(ds, k) } else { 0 }).collect();
    let mut counts = vec![DbfLosses::default(); ds.fields.len()];
    // Geometry is examined once per row, regardless of how many attribute fields it has.
    for row in ds.features.iter().filter(|f| shapefile_keeps(f.geometry.as_ref(), ds.geometry_type)) {
        for (k, (field, value)) in ds.fields.iter().zip(&row.attrs).enumerate() {
            if value.is_null() {
                continue;
            }
            let Some(loss) = counts.get_mut(k) else { continue };
            match field.ty {
                // A date-time field is written as its text too, so text held in one is cut the same way.
                FieldType::Text | FieldType::DateTime if value.to_string().len() > 254 => loss.text += 1,
                FieldType::Date if !value.asks_null() && dbf_date(value).is_none() => loss.empty += 1,
                FieldType::Integer | FieldType::Double => {
                    let d = decimals.get(k).copied().unwrap_or(0);
                    match dbf_number(value, field.ty, d, if field.ty == FieldType::Integer { 18 } else { 24 }) {
                        None => loss.empty += 1,
                        Some(number) => {
                            // The writer uses fixed decimal text, including ties rounded to even.
                            let written = format!("{number:.d$}").parse::<f64>().ok();
                            if written != value.as_f64() {
                                loss.rounded += 1;
                            }
                        }
                    }
                }
                FieldType::Boolean if dbf_boolean(value).is_none() => loss.empty += 1,
                _ => {}
            }
        }
    }
    let selected = |count: fn(&DbfLosses) -> usize| {
        let total: usize = counts.iter().map(count).sum();
        let fields: Vec<_> = ds.fields.iter().zip(&counts).filter(|(_, n)| count(n) > 0).map(|(f, _)| f.name.as_str()).collect();
        let names: Vec<_> = fields.iter().take(5).map(|s| format!("'{}'", dbf_notice_name(s))).collect();
        let more = if fields.len() > 5 { format!(" and {} more", fields.len() - 5) } else { String::new() };
        (total, format!("{}{more}", names.join(", ")))
    };
    let target: String = target.chars().map(dbf_notice_char).collect();
    let mut notes = Vec::new();
    let (cut, fields) = selected(|n| n.text);
    if cut > 0 {
        let what = if cut == 1 { "1 text value was".into() } else { format!("{cut} text values were") };
        notes.push(format!("{target}: {what} cut to at most 254 bytes, in {fields}"));
    }
    let (empty, fields) = selected(|n| n.empty);
    if empty > 0 {
        let what = if empty == 1 {
            "1 value couldn't be written to its dBase field and was".into()
        } else {
            format!("{empty} values couldn't be written to their dBase fields and were")
        };
        notes.push(format!("{target}: {what} left empty, in {fields}"));
    }
    let (rounded, fields) = selected(|n| n.rounded);
    if rounded > 0 {
        let what = if rounded == 1 { "1 number was".into() } else { format!("{rounded} numbers were") };
        notes.push(format!("{target}: {what} rounded to fit dBase field precision, in {fields}"));
    }
    notes
}

/// The names of the `.dbf` columns a Shapefile written from `fields` has, in order
/// ([`dbf_names`]).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn shapefile_column_names(fields: &[Field]) -> Vec<String> {
    dbf_names(fields)
}

/// dBase field names (at most 10 bytes, unique without regard to case) for the fields.
fn dbf_names(fields: &[Field]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(fields.len());
    for f in fields {
        let base = cut(f.name.trim(), 10);
        let base = if base.is_empty() { "FIELD" } else { base };
        let taken = |n: &str, out: &[String]| out.iter().any(|o| o.eq_ignore_ascii_case(n));
        let name = if !taken(base, &out) {
            base.to_string()
        } else {
            (1..100_000)
                .map(|i| {
                    let tag = format!("_{i}");
                    format!("{}{tag}", cut(base, 10 - tag.len()))
                })
                .find(|n| !taken(n, &out))
                .unwrap_or_else(|| base.to_string())
        };
        out.push(name);
    }
    out
}

/// Decimals of the `.dbf` field for Double field `k`: as many as fit dBase's 24 characters beside
/// the field's largest whole part, at most 15.
fn dbf_decimals(ds: &Dataset, k: usize) -> usize {
    let max = ds
        .features
        .iter()
        .filter_map(|r| r.attrs.get(k).and_then(Value::as_f64))
        .filter(|v| v.is_finite())
        .fold(0.0f64, |m, v| m.max(v.abs()));
    // Digits before the point, a sign and the point itself; the rest are decimals.
    let int_digits = if max >= 1.0 { (max.log10().floor() as usize).saturating_add(1) } else { 1 };
    24usize.saturating_sub(int_digits + 2).min(15)
}

/// The number a `.dbf` numeric field of type `ty` with `d` decimals holds for `v`: none for a
/// value that isn't a number, or one rejected by the selected column's `width` and precision.
/// Rejected numbers are left empty rather than cut short.
fn dbf_number(v: &Value, ty: FieldType, d: usize, width: usize) -> Option<f64> {
    let x = v.as_f64().filter(|x| {
        // A whole number past 2^53 would be held only rounded (dBase's decimal text goes through
        // f64), and one too small for the field's decimals would read 0.
        let exact = !(matches!(v, Value::Int(i) if i.unsigned_abs() > 1 << 53)
            || ty == FieldType::Double && *x != 0.0 && x.abs() < 0.5 * 10f64.powi(-(d as i32)));
        exact && format!("{x:.d$}").len() <= width
    })?;
    Some(if ty == FieldType::Integer { x.round() } else { x })
}

/// The day a `.dbf` date field holds for `v`: a date, a date-time at midnight without a zone, or
/// text that is one. Anything else is left empty.
fn dbf_date(v: &Value) -> Option<crate::date::Date> {
    match v {
        Value::Date(d) => Some(*d),
        Value::DateTime(t) if t.is_date() => Some(t.date()),
        Value::Text(t) => match Value::parse_stored(t, FieldType::Date) {
            Value::Date(d) => Some(d),
            _ => None,
        },
        _ => None,
    }
}

/// The Boolean values the dBase writer accepts; an incompatible value is left empty.
fn dbf_boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Int(i) => Some(*i != 0),
        _ => None,
    }
}

/// The notice for the `left_out` features an export to `name` (a file of extension `ext`) left
/// out, as [`write_path_in`] and [`write_bytes_in`] count them; `None` when none were. A
/// Shapefile export (`.shp`, or zipped) writes only drawable geometry of the layer's one kind (it
/// writes no null shapes, though the format has them); FlatGeobuf, KML and GPX exports write only
/// features with geometry; the other formats leave nothing out.
pub fn left_out_note(name: &str, ext: &str, left_out: usize) -> Option<String> {
    (left_out > 0).then(|| match ext {
        "shp" | "zip" => format!(
            "{name}: {left_out} features left out of the Shapefile: without drawable geometry, or with geometry of another kind than the layer's"
        ),
        _ => format!("{name}: {left_out} features without drawable geometry were left out"),
    })
}

/// Changes made by a CSV export to geometry and to text guarded for spreadsheets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CsvLosses {
    /// Features of a point layer with more than one point (a multipoint), or another shape,
    /// written as one point.
    pub as_one_point: usize,
    /// Features whose geometry isn't written: in a line or polygon layer (or one that declares
    /// no type), and in a point layer those with no point to write (an empty multipoint).
    pub unwritten: usize,
    /// Written with coordinate columns, as a point layer: a streamed export knows this from its
    /// chunks when the layer declares no type ([`CsvLosses::coordinates_note`]).
    pub points: bool,
    /// Values prefixed with an apostrophe, counted by field.
    pub guarded: Vec<usize>,
    /// Coordinate rows that can let a competing attribute pair win on reimport.
    unusable_points: usize,
}

impl CsvLosses {
    /// The losses of `features` in a layer of type `gt`; a streamed export adds up its chunks'.
    pub fn of(features: &[crate::Feature], gt: GeometryType, fields: &[Field]) -> Result<Self> {
        Self::of_placed(features, gt, fields, &|c| Some(c))
    }

    /// As [`CsvLosses::of`], for features whose points are written where `place` puts them (in
    /// another system): only the points are converted, not the features.
    fn of_placed(features: &[crate::Feature], gt: GeometryType, fields: &[Field], place: &dyn Fn(Coord) -> Option<Coord>) -> Result<Self> {
        let mut out = Self { points: gt == GeometryType::Point, guarded: vec![0; fields.len()], ..Self::default() };
        for feature in features {
            crate::progress::check()?;
            for (count, value) in out.guarded.iter_mut().zip(&feature.attrs) {
                *count += usize::from(matches!(value, Value::Text(text) if csv_needs_guard(text)));
            }
            let geometry = feature.geometry.as_ref();
            let point = if out.points { geometry.and_then(Geometry::label_point) } else { None };
            if out.points && !point.and_then(place).is_some_and(usable_degree_point) {
                out.unusable_points += 1;
            }
            let Some(g) = geometry else { continue };
            match (gt, g) {
                (GeometryType::Point, Geometry::Point(_)) => {}
                // No point to write: its coordinate cells are left empty.
                (GeometryType::Point, _) if point.is_none() => out.unwritten += 1,
                (GeometryType::Point, Geometry::MultiPoint(v)) if v.len() <= 1 => {}
                (GeometryType::Point, _) => out.as_one_point += 1,
                // A layer of lines or polygons, or one that declares no type: no coordinates.
                _ => out.unwritten += 1,
            }
        }
        Ok(out)
    }

    /// One notice for each loss of a layer written to `target`; none without loss. Geometry
    /// not written is counted by feature, whatever its kind: a layer of one type can hold others.
    pub fn notes(&self, fields: &[Field], target: &str) -> Vec<String> {
        let mut notes = Vec::new();
        if self.as_one_point > 0 {
            let what = if self.as_one_point == 1 {
                "1 feature has more than one point (a multipoint, line or polygon) and was".to_string()
            } else {
                format!("{} features have more than one point (a multipoint, line or polygon) and were", self.as_one_point)
            };
            notes.push(format!("{target}: {what} written as one, as CSV holds a point a row"));
        }
        if self.unwritten > 0 {
            let what = if self.unwritten == 1 { "1 feature".to_string() } else { format!("{} features", self.unwritten) };
            notes.push(format!("{target}: CSV holds a point a row, so the geometry of {what} wasn't written, only the attributes"));
        }
        let total: usize = self.guarded.iter().sum();
        if total > 0 {
            let target: String = target.chars().map(dbf_notice_char).collect();
            let values = if total == 1 { "value" } else { "values" };
            let affected = fields
                .iter()
                .zip(&self.guarded)
                .filter(|(_, n)| **n > 0)
                .map(|(f, n)| format!("'{}' ({n})", dbf_notice_name(&f.name)))
                .collect::<Vec<_>>()
                .join(", ");
            notes.push(format!("{target}: CSV prefixed {total} text {values} with an apostrophe for spreadsheet protection in {affected}"));
        }
        notes
    }

    /// Warns when adding the CSV back may use attributes for geometry. This can happen when the
    /// coordinate names are all taken or when unusable coordinate rows let a competing pair win.
    pub fn coordinates_note(&self, fields: &[Field], target: &str) -> Option<String> {
        self.coordinates_note_in(fields, target, Crs::LonLat)
    }

    /// As [`CsvLosses::coordinates_note`], for a CSV written in `crs`. In a projected system, the
    /// x and y columns come first and reading takes them first when given the system, unless the
    /// fields hold every name they could have.
    pub fn coordinates_note_in(&self, fields: &[Field], target: &str, crs: Crs) -> Option<String> {
        if !self.points {
            return None;
        }
        if !crs.is_geographic() {
            let [x, y] = csv_projected_names(fields);
            let target: String = target.chars().map(dbf_notice_char).collect();
            return (x != "x" && x != "easting" || y != "y" && y != "northing").then(|| {
                // The names that moved the columns: each axis's two, when both are taken.
                let names: Vec<String> = [(x != "x" && x != "easting", ["x", "easting"]), (y != "y" && y != "northing", ["y", "northing"])]
                    .into_iter()
                    .filter(|(moved, _)| *moved)
                    .flat_map(|(_, names)| names.map(|n| format!("'{n}'")))
                    .collect();
                format!(
                    "{target}: the fields have the names {}, so the coordinates are in columns '{}' and '{}'; pick those columns when adding the file back",
                    names.join(" and ").replacen(" and ", ", ", names.len().saturating_sub(2)),
                    dbf_notice_name(&x),
                    dbf_notice_name(&y)
                )
            });
        }
        let header: Vec<String> = csv_coordinate_names(fields).into_iter().chain(fields.iter().map(|f| f.name.clone())).collect();
        let alternative = found_non_geographic_xy(&header);
        let geographic = found_geographic_xy(&header);
        let found = geographic.or(alternative);
        let name = |i: usize| dbf_notice_name(header.get(i).map_or("", String::as_str));
        let target: String = target.chars().map(dbf_notice_char).collect();
        if found == Some((0, 1)) {
            return alternative.filter(|_| self.unusable_points > 0).map(|(x, y)| {
                format!(
                    "{target}: {} of the exported coordinate rows lack a usable longitude/latitude position; adding the file back may read columns '{}' and '{}' as coordinates. To use the exported geometry, pick '{}' and '{}'",
                    self.unusable_points,
                    name(x),
                    name(y),
                    name(0),
                    name(1)
                )
            });
        }
        let read = match (geographic, alternative) {
            (Some((lon, lat)), Some((x, y))) => format!(
                "may read '{}' and '{}' or '{}' and '{}' as the coordinates, which can move the points",
                name(lon),
                name(lat),
                name(x),
                name(y)
            ),
            _ => match found {
                Some((x, y)) => format!("reads columns '{}' and '{}' as the coordinates, which moves the points", name(x), name(y)),
                None => "reads no coordinates".to_string(),
            },
        };
        Some(format!(
            "{target}: the fields have every name read as a longitude or latitude, so the coordinates are in columns '{}' and '{}'; adding the file back {read} unless those columns are picked",
            name(0),
            name(1)
        ))
    }
}

impl std::ops::AddAssign for CsvLosses {
    /// Adds a chunk's losses for the same layer and fields.
    fn add_assign(&mut self, other: Self) {
        self.as_one_point += other.as_one_point;
        self.unwritten += other.unwritten;
        self.points |= other.points;
        if self.guarded.is_empty() {
            self.guarded = other.guarded;
        } else {
            for (total, count) in self.guarded.iter_mut().zip(other.guarded) {
                *total += count;
            }
        }
        self.unusable_points += other.unusable_points;
    }
}

/// What reading a CSV, Shapefile, KML or GPX export back does to its text, counted for each field
/// over the features the writer writes and reading keeps: spaces around text dropped, blank text
/// read as null, and features and vertices reading skips. Readers, as they are:
/// - CSV (and Data Interop's WKT CSV) keeps a cell as it is; an empty one reads as null, and so
///   does one of spaces in a column read as numbers, and text `<Null>` ([`Value::parse_file`]).
/// - dBase drops the spaces after text, and reads text of spaces only, or `<Null>`, as null
///   ([`read_dbf`]).
/// - KML and GPX trim each value; a blank name reads as null, and a blank KML extended data
///   value too in a column read as numbers ([`type_text_columns`], for each kind of geometry's
///   layer). A KML description is kept as it is. KML writes no null value, so a field without one
///   has no column, which [`export_losses_notes`] says; GPX writes only names, which
///   [`gpx_notes`] says.
///
/// A streamed export adds up its chunks' (`+=`): whether reading takes a column as numbers, which
/// decides what its blanks become, is settled over all of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextLosses {
    /// For each layer reading gives (KML: points, lines, polygons; the others: one), each field's,
    /// in the layer's order.
    layers: Vec<Vec<FieldText>>,
    /// Only the spaces after text are dropped (dBase).
    ends_only: bool,
    /// Features written that reading skips (KML, GPX): no coordinate in longitude/latitude, or
    /// too few for their shape.
    unread: usize,
    /// Vertices of the features reading keeps (KML, GPX) that don't read back with them
    /// ([`crate::kml::lost_vertices`], [`crate::gpx::lost_vertices`]).
    vertices: usize,
}

/// One field's [`TextLosses`] in one layer read back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FieldText {
    /// Text with spaces around it that reading drops, unless its column reads as numbers (the
    /// text is then a number, and read as one).
    padded: usize,
    /// Text with spaces around it in a KML or GPX name, which reading trims whatever its
    /// extended data column's type.
    name_padded: usize,
    /// dBase text with a NUL in what the file keeps: reading keeps only the text before it, and
    /// reads it as null when that is blank (#2415).
    nul: usize,
    /// CSV text starting with an apostrophe before a formula's first character (`'=x`), which
    /// reading takes for the formula guard and drops ([`unguard`], #2415).
    apostrophe: usize,
    /// Blank text, read as null.
    blank: usize,
    /// Text `<Null>` ([`Value::is_null_marker`]), which CSV and dBase read as null.
    markers: usize,
    /// Blank text read as null in a column read as numbers, else read as it is.
    blank_in_numbers: usize,
    /// Text of spaces only: read as null in a column read as numbers, else as empty text.
    spaces: usize,
    /// The column's written text, when reading types the column from its values.
    column: Option<TextColumn>,
}

/// How reading treats one copy of a field's text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextRead {
    /// A CSV cell, as it is.
    Cell,
    /// A dBase Character column: the spaces after it dropped.
    Dbf,
    /// A KML or GPX name: trimmed, a blank one null.
    Name,
    /// KML extended data: trimmed, its column typed from its values.
    Data,
}

/// Whether a longitude and latitude are in range, which the KML and GPX readers keep.
pub(crate) fn lon_lat_in_range(x: f64, y: f64) -> bool {
    (-180.0..=180.0).contains(&x) && (-90.0..=90.0).contains(&y)
}

impl TextLosses {
    /// The losses of `features`, with `fields`, in a layer of type `gt` written to a file of
    /// extension `ext` (lowercase), each KML or GPX feature named from field `name_field`; none
    /// for another format.
    ///
    /// One pass over the features without a check for Cancel: the counts of a pass stopped half
    /// way would understate the losses, so it can't return early. It formats each value once,
    /// in memory, which is less than the write it follows (the same formatting, plus the write),
    /// and a streamed export counts a chunk at a time, looking at Cancel between chunks.
    pub fn of(features: &[crate::Feature], fields: &[Field], gt: GeometryType, ext: &str, name_field: Option<usize>) -> Self {
        let (csv, shp, kml, gpx) = (matches!(ext, "csv" | "wkt"), matches!(ext, "shp" | "zip"), matches!(ext, "kml" | "kmz"), ext == "gpx");
        if !(csv || shp || kml || gpx) {
            return Self::default();
        }
        // How reading treats each field's copies: a KML name field is the placemark's name and,
        // unless called `name` (whose copy reading leaves out as the same), extended data too.
        let reads: Vec<Vec<TextRead>> = (fields.iter().enumerate())
            .map(|(k, f)| {
                let named = (kml || gpx) && Some(k) == name_field;
                let mut reads = Vec::new();
                if named {
                    reads.push(TextRead::Name);
                }
                if csv {
                    reads.push(TextRead::Cell);
                } else if shp && matches!(f.ty, FieldType::Text | FieldType::DateTime) {
                    reads.push(TextRead::Dbf);
                } else if kml && f.name != "description" && !(named && f.name.eq_ignore_ascii_case("name")) {
                    // A KML description is text as it is.
                    reads.push(TextRead::Data);
                }
                reads
            })
            .collect();
        let fresh: Vec<FieldText> = reads
            .iter()
            .map(|r| FieldText { column: (csv || r.contains(&TextRead::Data)).then(TextColumn::default), ..FieldText::default() })
            .collect();
        // KML reads a layer for each kind of geometry, and types each one's columns apart.
        let mut layers = vec![fresh; if kml { 3 } else { 1 }];
        let mut unread = 0usize;
        let mut vertices = 0usize;
        let layer_of = |g: &Geometry| match g.kind() {
            _ if !kml => 0,
            GeometryType::Point => 0,
            GeometryType::Polyline => 1,
            _ => 2,
        };
        for feature in features {
            let layer = match &feature.geometry {
                _ if csv => 0,
                g if shp && shapefile_keeps(g.as_ref(), gt) => 0,
                Some(g) if kml && crate::kml::keeps(g) => {
                    vertices = vertices.saturating_add(crate::kml::lost_vertices(g));
                    layer_of(g)
                }
                Some(g) if gpx && crate::gpx::keeps(g) => {
                    vertices = vertices.saturating_add(crate::gpx::lost_vertices(g));
                    layer_of(g)
                }
                // Written, and skipped on reading (a GPX empty multipoint isn't written at all, and
                // is said as left out).
                Some(g) if kml || (gpx && !matches!(g, Geometry::MultiPoint(v) if v.is_empty())) => {
                    unread = unread.saturating_add(1);
                    continue;
                }
                _ => continue,
            };
            let Some(layer) = layers.get_mut(layer) else { continue };
            for ((v, loss), reads) in feature.attrs.iter().zip(layer.iter_mut()).zip(&reads) {
                if v.is_null() {
                    continue;
                }
                // Text absorbs every later value, so a column typed Text needs no more.
                if let Some(column) = &mut loss.column
                    && !column.is_text()
                {
                    let text = match ext {
                        "csv" => csv_cell(v),
                        "wkt" => crate::interop::wkt_csv_text(&v.to_file_text()),
                        _ => v.to_file_text(),
                    };
                    // CSV's `<Null>` reads as null, so it types a column no more than an empty cell.
                    if !(csv && Value::is_null_marker(&text)) {
                        column.add(&text);
                    }
                }
                let Value::Text(s) = v else { continue };
                // A value is counted once, however many copies of it are written (a KML name
                // field's extended data, a GPX multipoint's waypoints): by its first copy, the
                // name, whose loss is never less than the extended data's.
                if let Some(read) = reads.first() {
                    // dBase keeps only the text before a NUL in the 254 bytes a Character column
                    // keeps: counted apart, and what's kept then loses its trailing spaces as
                    // other text does; a blank one, read as null, the NUL's notice says (#2415).
                    let mut s = s.as_str();
                    if *read == TextRead::Dbf
                        && let Some(at) = s.bytes().take(254).position(|b| b == 0)
                    {
                        loss.nul += 1;
                        s = s.get(..at).unwrap_or_default();
                        if s.trim_end_matches(' ').is_empty() {
                            continue;
                        }
                    }
                    let trimmed = s.trim();
                    let count = match read {
                        TextRead::Cell if s.is_empty() => &mut loss.blank,
                        TextRead::Cell | TextRead::Dbf if Value::is_null_marker(s) => &mut loss.markers,
                        TextRead::Cell if s.strip_prefix('\'').is_some_and(|rest| rest.starts_with(FORMULA_START)) => &mut loss.apostrophe,
                        TextRead::Cell if trimmed.is_empty() => &mut loss.blank_in_numbers,
                        // A Character column; text cut to 254 bytes is said as cut.
                        TextRead::Dbf if s.len() <= 254 && s.trim_end_matches(' ').is_empty() => &mut loss.blank,
                        TextRead::Dbf if s.len() <= 254 && s.ends_with(' ') => &mut loss.padded,
                        TextRead::Name if trimmed.is_empty() => &mut loss.blank,
                        TextRead::Name if trimmed.len() < s.len() => &mut loss.name_padded,
                        TextRead::Data if s.is_empty() => &mut loss.blank_in_numbers,
                        TextRead::Data if trimmed.is_empty() => &mut loss.spaces,
                        TextRead::Data if trimmed.len() < s.len() => &mut loss.padded,
                        _ => continue,
                    };
                    *count += 1;
                }
            }
        }
        Self { layers, ends_only: shp, unread, vertices }
    }

    /// One notice for each loss of a layer with `fields` written to `target`, with a count and
    /// the fields; none without loss.
    pub fn notes(&self, fields: &[Field], target: &str) -> Vec<String> {
        let numbers = |t: &FieldText| t.column.is_some_and(|c| c.ty() != FieldType::Text);
        // Each field's count over the layers read back.
        let counts = |count: &dyn Fn(&FieldText) -> usize| -> Vec<usize> {
            (0..fields.len()).map(|k| self.layers.iter().filter_map(|l| l.get(k)).map(count).sum()).collect()
        };
        let trimmed = counts(&|t| t.name_padded + if numbers(t) { 0 } else { t.padded + t.spaces });
        let blank = counts(&|t| t.blank + t.markers + if numbers(t) { t.blank_in_numbers + t.spaces } else { 0 });
        let markers = self.layers.iter().flatten().any(|t| t.markers > 0);
        let target: String = target.chars().map(dbf_notice_char).collect();
        let mut notes = Vec::new();
        let n: usize = trimmed.iter().sum();
        if n > 0 {
            let what = match (n, self.ends_only) {
                (1, true) => "1 text value ends with spaces".to_string(),
                (1, false) => "1 text value starts or ends with spaces, tabs or line breaks".to_string(),
                (n, true) => format!("{n} text values end with spaces"),
                (n, false) => format!("{n} text values start or end with spaces, tabs or line breaks"),
            };
            notes.push(format!("{target}: {what}, which reading the file back drops, in {}", named_fields(fields, &trimmed)));
        }
        let n: usize = blank.iter().sum();
        if n > 0 {
            let what = match (n, markers) {
                (1, false) => "1 blank text value reads".to_string(),
                (n, false) => format!("{n} blank text values read"),
                (1, true) => "1 text value, blank or <Null>, reads".to_string(),
                (n, true) => format!("{n} text values, blank or <Null>, read"),
            };
            notes.push(format!("{target}: {what} back as empty (null), in {}", named_fields(fields, &blank)));
        }
        let nul = counts(&|t| t.nul);
        let n: usize = nul.iter().sum();
        if n > 0 {
            let what = if n == 1 { "1 text value holds".to_string() } else { format!("{n} text values hold") };
            notes.push(format!(
                "{target}: {what} a NUL character, and reading the file back keeps only the text before it, read as empty (null) when that is blank, in {}",
                named_fields(fields, &nul)
            ));
        }
        let apostrophe = counts(&|t| t.apostrophe);
        let n: usize = apostrophe.iter().sum();
        if n > 0 {
            let what = if n == 1 { "1 text value starts".to_string() } else { format!("{n} text values start") };
            notes.push(format!(
                "{target}: {what} with an apostrophe before =, +, -, @, a tab or a line break, which reading the file back takes for the formula guard and drops, in {}",
                named_fields(fields, &apostrophe)
            ));
        }
        if self.unread > 0 {
            let what = if self.unread == 1 { "1 feature is".to_string() } else { format!("{} features are", self.unread) };
            notes.push(format!(
                "{target}: {what} written but won't read back: no coordinate is inside longitude/latitude, or too few for the shape"
            ));
        }
        if self.vertices > 0 {
            let what = if self.vertices == 1 { "1 vertex won't".to_string() } else { format!("{} vertices won't", self.vertices) };
            notes.push(format!(
                "{target}: {what} read back with the features kept: outside longitude/latitude, or in a part left with too few to keep"
            ));
        }
        notes
    }
}

impl std::ops::AddAssign for TextLosses {
    /// Adds a chunk's losses, of the same fields.
    fn add_assign(&mut self, other: Self) {
        if self.layers.is_empty() {
            *self = other;
            return;
        }
        self.unread = self.unread.saturating_add(other.unread);
        self.vertices = self.vertices.saturating_add(other.vertices);
        for (a, b) in self.layers.iter_mut().zip(other.layers).flat_map(|(a, b)| a.iter_mut().zip(b)) {
            a.padded += b.padded;
            a.name_padded += b.name_padded;
            a.nul += b.nul;
            a.apostrophe += b.apostrophe;
            a.blank += b.blank;
            a.markers += b.markers;
            a.blank_in_numbers += b.blank_in_numbers;
            a.spaces += b.spaces;
            if let (Some(a), Some(b)) = (&mut a.column, b.column) {
                a.merge(b);
            }
        }
    }
}

/// What a CSV, Shapefile, KML, KMZ or GPX file (extension `ext`) of the whole dataset `ds`
/// changes when written to `target`: [`CsvLosses`]' notices, KML's fields without a value,
/// [`gpx_notes`] with each feature named from `name_field`, and [`TextLosses`]' (a Shapefile's
/// other values are [`shapefile_values_notes`]). A streamed export counts CSV's changes a chunk
/// at a time instead.
pub fn export_losses_notes(ds: &Dataset, ext: &str, name_field: Option<usize>, target: &str) -> Result<Vec<String>> {
    export_losses_notes_in(ds, ext, name_field, target, Crs::LonLat)
}

/// As [`export_losses_notes`], for an export written in `crs`.
pub fn export_losses_notes_in(ds: &Dataset, ext: &str, name_field: Option<usize>, target: &str, crs: Crs) -> Result<Vec<String>> {
    let mut notes = match ext {
        "csv" => {
            // Of the coordinates the file is written in: longitude/latitude, or another
            // geographic system's, whose points alone are converted to be checked (no copy of the
            // layer); a projected system's aren't degrees, so aren't checked as such.
            let losses = if crs == Crs::LonLat {
                match ds.in_lonlat() {
                    Ok(ds) => CsvLosses::of(&ds.features, ds.geometry_type, &ds.fields)?,
                    // A place that can't be given there (written without its geometry, and said
                    // by [`unplaced_export_note`]): its points converted one by one, those that
                    // can't be counted as unusable.
                    Err(_) => {
                        let to = ds.lonlat_conversion().map_err(DataError::of_conversion)?;
                        CsvLosses::of_placed(&ds.features, ds.geometry_type, &ds.fields, &|c| to.point(c))?
                    }
                }
            } else if crs.is_geographic() && crs != ds.coord_crs() {
                let to = ds.coord_crs().to(crs).map_err(DataError::Format)?;
                CsvLosses::of_placed(&ds.features, ds.geometry_type, &ds.fields, &|c| to.point(c))?
            } else {
                CsvLosses::of(&ds.features, ds.geometry_type, &ds.fields)?
            };
            let mut notes = losses.notes(&ds.fields, target);
            notes.extend(losses.coordinates_note_in(&ds.fields, target, crs));
            notes
        }
        "kml" | "kmz" => kml_empty_fields_note(ds, target)?.into_iter().chain(kml_blank_names_note(ds, target)?).collect(),
        "gpx" => gpx_notes(ds, name_field, target),
        _ => Vec::new(),
    };
    notes.extend(TextLosses::of(&ds.features, &ds.fields, ds.geometry_type, ext, name_field).notes(&ds.fields, target));
    Ok(notes)
}

/// KML fields whose name is blank once trimmed, with values in the placemarks written: each is
/// written as `<Data name="">`, which reading drops, so the column and its values are gone
/// (#2415). A field with no value written is said by [`kml_empty_fields_note`].
fn kml_blank_names_note(ds: &Dataset, target: &str) -> Result<Option<String>> {
    let blank: Vec<usize> = ds.fields.iter().enumerate().filter(|(_, f)| f.name.trim().is_empty()).map(|(k, _)| k).collect();
    if blank.is_empty() {
        return Ok(None);
    }
    // One pass over the placemarks written, looking at Cancel as [`kml_empty_fields_note`] does.
    let mut with_value = vec![false; blank.len()];
    let mut values = 0usize;
    for f in ds.features.iter().filter(|f| f.geometry.is_some()) {
        crate::progress::check()?;
        for (has, k) in with_value.iter_mut().zip(&blank) {
            if f.attrs.get(*k).is_some_and(|v| !v.is_null()) {
                *has = true;
                values += 1;
            }
        }
    }
    let with_values = with_value.iter().filter(|has| **has).count();
    Ok((values > 0).then(|| {
        let target: String = target.chars().map(dbf_notice_char).collect();
        let fields = if with_values == 1 { "1 field".to_string() } else { format!("{with_values} fields") };
        let written = if values == 1 { "1 value".to_string() } else { format!("{values} values") };
        format!(
            "{target}: {fields} with a blank name, and {written} in them, won't read back: reading KML drops extended data without a name"
        )
    }))
}

/// KML writes ExtendedData only for non-null values of placemarks it writes. Report every
/// field absent from that output, even when an unwritten feature has a value for it.
fn kml_empty_fields_note(ds: &Dataset, target: &str) -> Result<Option<String>> {
    let mut written = vec![false; ds.fields.len()];
    for f in &ds.features {
        crate::progress::check()?;
        if f.geometry.is_some() {
            for (written, value) in written.iter_mut().zip(&f.attrs) {
                *written |= !value.is_null();
            }
        }
    }
    let empty: Vec<_> = ds
        .fields
        .iter()
        .zip(written)
        .filter(|(_, written)| !written)
        .map(|(field, _)| format!("'{}'", dbf_notice_name(&field.name)))
        .collect();
    let target: String = target.chars().map(dbf_notice_char).collect();
    Ok((!empty.is_empty()).then(|| {
        let count = empty.len();
        let fields = if count == 1 { "field" } else { "fields" };
        format!("{target}: KML omitted {count} {fields} with no values in the placemarks written ({})", empty.join(", "))
    }))
}

/// Features a GPX file of `ds` leaves out: those without geometry, and multipoints without a
/// point, which give no waypoint.
pub fn gpx_left_out(ds: &Dataset) -> usize {
    let empty = |g: &Option<Geometry>| match g {
        None => true,
        Some(Geometry::MultiPoint(v)) => v.is_empty(),
        Some(_) => false,
    };
    ds.features.iter().filter(|f| empty(&f.geometry)).count()
}

/// What a GPX file of `ds` can't hold, each feature named from field `name_field`: other
/// fields' values, polygons (written as tracks along their outer rings, holes left out) and
/// multipoints (a waypoint per point). One notice per kind; none for a layer GPX holds whole.
pub fn gpx_notes(ds: &Dataset, name_field: Option<usize>, target: &str) -> Vec<String> {
    let drawn = || ds.features.iter().filter(|f| f.geometry.is_some());
    // Every other field, null everywhere or not: it isn't in the file, so it doesn't come back.
    // Fields with values come first, so the names shown before "and N more" are the ones that lost data.
    let mut fields: Vec<(bool, &str)> = (ds.fields.iter().enumerate())
        .filter(|(k, _)| Some(*k) != name_field)
        .map(|(k, f)| (drawn().all(|g| g.attrs.get(k).is_none_or(Value::is_null)), f.name.as_str()))
        .collect();
    fields.sort_by_key(|(empty, _)| *empty);
    let fields: Vec<&str> = fields.into_iter().map(|(_, name)| name).collect();
    let (mut polygons, mut holes, mut multipoints) = (0, 0, 0);
    for g in drawn().filter_map(|f| f.geometry.as_ref()) {
        match g {
            Geometry::Polygon(polys) => {
                polygons += 1;
                holes += polys.iter().map(|rings| rings.len().saturating_sub(1)).sum::<usize>();
            }
            Geometry::MultiPoint(v) if v.len() > 1 => multipoints += 1,
            _ => {}
        }
    }
    let mut notes = Vec::new();
    if !fields.is_empty() {
        let names: Vec<_> = fields.iter().take(5).map(|s| format!("'{}'", dbf_notice_name(s))).collect();
        let more = if fields.len() > 5 { format!(" and {} more", fields.len() - 5) } else { String::new() };
        let what = if fields.len() == 1 { "1 field wasn't" } else { "fields weren't" };
        notes.push(format!("{target}: GPX holds a name for each feature, so {what} written: {}{more}", names.join(", ")));
    }
    if polygons > 0 {
        let what = if polygons == 1 { "1 polygon was".to_string() } else { format!("{polygons} polygons were") };
        let left = match holes {
            0 => String::new(),
            1 => ", and 1 hole left out".to_string(),
            n => format!(", and {n} holes left out"),
        };
        notes.push(format!("{target}: GPX has no areas, so {what} written as tracks along their outer edges{left}"));
    }
    if multipoints > 0 {
        let what = if multipoints == 1 { "1 multipoint was".to_string() } else { format!("{multipoints} multipoints were") };
        notes.push(format!("{target}: {what} written as a waypoint for each of its points"));
    }
    notes
}

/// Whether [`shapefile_parts_in`] writes a feature with geometry `g` to a layer of type `gt`: one
/// of the layer's kind with something drawable (a finite point, a line part of two finite points,
/// a polygon whose outer ring has three).
fn shapefile_keeps(g: Option<&Geometry>, gt: GeometryType) -> bool {
    let finite = |c: &&Coord| c.x.is_finite() && c.y.is_finite();
    match (g, gt) {
        (Some(g @ (Geometry::Point(_) | Geometry::MultiPoint(_))), GeometryType::Point) => {
            let mut any = false;
            g.for_each_coord(|c| any |= finite(&&c));
            any
        }
        (Some(Geometry::Line(parts)), GeometryType::Polyline) => parts.iter().any(|p| p.iter().filter(finite).count() >= 2),
        (Some(Geometry::Polygon(polys)), GeometryType::Polygon) => {
            polys.iter().any(|p| p.first().is_some_and(|r| r.iter().filter(finite).count() >= 3))
        }
        _ => false,
    }
}

/// Calls `f` with each vertex of `g` that [`shapefile_parts_in`] writes, in a layer of type `gt`,
/// once a projected export has made every coordinate finite (`project` refuses one it can't):
/// none of another kind than the layer's, and of a line or a polygon only the parts it can draw,
/// a line part of two vertices, a ring of three (a polygon's holes go with its outer ring).
pub(crate) fn shapefile_vertices(g: &Geometry, gt: GeometryType, f: impl FnMut(Coord)) {
    match (g, gt) {
        (Geometry::Point(_) | Geometry::MultiPoint(_), GeometryType::Point) => g.for_each_coord(f),
        (Geometry::Line(parts), GeometryType::Polyline) => parts.iter().filter(|p| p.len() >= 2).flatten().copied().for_each(f),
        (Geometry::Polygon(polys), GeometryType::Polygon) => polys
            .iter()
            .filter(|p| p.first().is_some_and(|outer| outer.len() >= 3))
            .flatten()
            .filter(|r| r.len() >= 3)
            .flatten()
            .copied()
            .for_each(f),
        _ => {}
    }
}

/// What [`shapefile_parts_in`] leaves out of geometry `g` it writes to a layer of type `gt`, as
/// (vertices, parts), `drawable` telling the vertices it writes: the vertices that aren't, and the
/// line parts and polygon rings with too few drawable vertices (a part needs two, a ring three),
/// whether written so or left so by those vertices, a hole going with its polygon's outer ring.
/// `None` for geometry it leaves out whole, of another kind or with nothing drawable
/// ([`left_out_note`]'s). The same filters as the writer's.
fn shapefile_geometry_losses(g: Option<&Geometry>, gt: GeometryType, drawable: fn(&Coord) -> bool) -> Option<(usize, usize)> {
    let count = |r: &[Coord]| r.iter().filter(|c| drawable(c)).count();
    match (g, gt) {
        (Some(g @ (Geometry::Point(_) | Geometry::MultiPoint(_))), GeometryType::Point) => {
            let (mut all, mut kept) = (0usize, 0usize);
            g.for_each_coord(|c| {
                all += 1;
                kept += usize::from(drawable(&c));
            });
            (kept > 0).then_some((all - kept, 0))
        }
        (Some(Geometry::Line(parts)), GeometryType::Polyline) => {
            let missing = parts.iter().map(|p| p.len() - count(p)).sum();
            let short = parts.iter().filter(|p| count(p) < 2).count();
            (short < parts.len()).then_some((missing, short))
        }
        (Some(Geometry::Polygon(polys)), GeometryType::Polygon) => {
            let outer_kept = |poly: &[Vec<Coord>]| poly.first().is_some_and(|outer| count(outer) >= 3);
            if !polys.iter().any(|poly| outer_kept(poly)) {
                return None;
            }
            let missing = polys.iter().flatten().map(|r| r.len() - count(r)).sum();
            let rings = polys
                .iter()
                .map(|poly| if outer_kept(poly) { poly.iter().skip(1).filter(|r| count(r) < 3).count() } else { poly.len() })
                .sum();
            Some((missing, rings))
        }
        _ => None,
    }
}

/// What an export to a Shapefile in `crs` says it leaves out of the geometry it writes, one
/// notice per kind with a count, or none: vertices with a missing (NaN) or infinite coordinate,
/// and the line parts or polygon rings without enough vertices to draw. Features left out whole
/// are [`left_out_note`]'s. In another system than the one the layer holds its coordinates in
/// ([`Dataset::coord_crs`]), the export writes the projected copy and
/// refuses a coordinate that doesn't project to a finite one ([`project`]), so every vertex it
/// writes is drawable (an infinite latitude in Web Mercator is written at 85.05°), and only parts
/// too short to draw are left out. `target` names what was written; line
/// breaks and other control characters in it show as spaces.
pub fn shapefile_geometry_notes(ds: &Dataset, target: &str, crs: Crs) -> Vec<String> {
    // Written as held when `crs` is the system the layer holds its coordinates in; otherwise a
    // converted copy, which refused every vertex that isn't a place.
    let drawable: fn(&Coord) -> bool = if crs == ds.coord_crs() { |c| c.x.is_finite() && c.y.is_finite() } else { |_| true };
    let (mut vertices, mut parts) = (0usize, 0usize);
    for f in &ds.features {
        let Some((v, p)) = shapefile_geometry_losses(f.geometry.as_ref(), ds.geometry_type, drawable) else { continue };
        vertices += v;
        parts += p;
    }
    let target: String = target.chars().map(dbf_notice_char).collect();
    let mut notes = Vec::new();
    if vertices > 0 {
        let what = if vertices == 1 {
            "1 vertex with a missing or infinite coordinate was".to_string()
        } else {
            format!("{vertices} vertices with a missing or infinite coordinate were")
        };
        notes.push(format!("{target}: {what} left out of the Shapefile"));
    }
    if parts > 0 {
        let (one, many, why) = if ds.geometry_type == GeometryType::Polyline {
            ("line part", "line parts", "with fewer than 2 drawable vertices")
        } else {
            ("polygon ring", "polygon rings", "with fewer than 3 drawable vertices or in a polygon whose outer ring was left out")
        };
        let what = if parts == 1 { format!("1 {one} was") } else { format!("{parts} {many} were") };
        notes.push(format!("{target}: {what} left out of the Shapefile, {why}"));
    }
    notes
}

/// A dataset as the files of a Shapefile: (extension, bytes) for `.shp`, `.shx`, `.dbf`, `.prj`
/// (WGS 84) and `.cpg` (UTF-8). Shapefiles hold one kind of shape and features with geometry:
/// features without it (or with nothing drawable) are left out, and the count of them returned.
/// Field names are cut to dBase's 10 bytes and text to 254.
pub fn shapefile_parts(ds: &Dataset) -> Result<(Vec<(&'static str, Vec<u8>)>, usize)> {
    shapefile_parts_in(ds, Crs::LonLat)
}

/// As [`shapefile_parts`], for a dataset whose geometry is in `crs`'s coordinates: the `.prj`
/// says `crs`, and is left out for a system WKT can't describe.
pub fn shapefile_parts_in(ds: &Dataset, crs: Crs) -> Result<(Vec<(&'static str, Vec<u8>)>, usize)> {
    shapefile_parts_styled(ds, crs, None).map(|(parts, left_out, _)| (parts, left_out))
}

/// What a Shapefile written over the one a layer was read from keeps of that file's `.dbf`
/// (#1385): its code page (the encoding its text is written in, and the language driver byte
/// its header names), and each column's width, decimals and type (`F` stays `F`) where the
/// layer's field of that name still fits it.
pub(crate) struct DbfStyle {
    pub encoding: crate::dbf::DbfEncoding,
    pub ldid: u8,
    pub columns: Vec<crate::dbf::Column>,
}

/// The `.dbf` column a field is written to: its type letter, width and decimals.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Spec {
    ty: u8,
    len: u8,
    decimals: u8,
}

/// The column field `k` of `ds` is written to as a new file has it: numbers in 18 digits
/// (integers) or 24 characters with as many decimals as fit, text as wide as its longest value.
fn new_column(ds: &Dataset, k: usize) -> Spec {
    let Some(f) = ds.fields.get(k) else { return Spec { ty: b'C', len: 1, decimals: 0 } };
    match f.ty {
        // 18 digits: GDAL reads a wider field as a real number.
        FieldType::Integer => Spec { ty: b'N', len: 18, decimals: 0 },
        FieldType::Double => Spec { ty: b'N', len: 24, decimals: u8::try_from(dbf_decimals(ds, k)).unwrap_or(15) },
        FieldType::Boolean => Spec { ty: b'L', len: 1, decimals: 0 },
        FieldType::Date => Spec { ty: b'D', len: 8, decimals: 0 },
        // A `.dbf` has no date-time field that other programs read, so they're ISO text.
        FieldType::Text | FieldType::DateTime => {
            let longest = ds.features.iter().filter_map(|r| r.attrs.get(k)).map(|v| v.to_string().len()).max().unwrap_or(0);
            Spec { ty: b'C', len: u8::try_from(longest.clamp(1, 254)).unwrap_or(254), decimals: 0 }
        }
    }
}

impl std::fmt::Display for Spec {
    /// As a `.dbf` column is described: its type letter, width and, for a number, decimals
    /// (`N(10,2)`, `C(24)`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ty = char::from(self.ty);
        if matches!(self.ty, b'N' | b'F') { write!(f, "{ty}({},{})", self.len, self.decimals) } else { write!(f, "{ty}({})", self.len) }
    }
}

/// The column field `k` (written as `name`) is written to over a file whose `.dbf` `style`
/// describes: the file's column of that name where the field's kind and every value still fit
/// it, so the file keeps its widths and decimals; otherwise a new one ([`new_column`]), at least
/// as wide as the file's, and, when a column of that name changes kind or can't
/// hold the values, what changed (`'ACRES' N(10,2) to N(24,9)`).
fn kept_column(ds: &Dataset, k: usize, name: &str, style: &DbfStyle) -> (Spec, Option<String>) {
    let new = new_column(ds, k);
    let (Some(field), Some(was)) = (ds.fields.get(k), style.columns.iter().find(|c| c.name == name.as_bytes())) else {
        return (new, None);
    };
    let values = || ds.features.iter().filter_map(|r| r.attrs.get(k)).filter(|v| !v.is_null());
    let ty = was.ty.to_ascii_uppercase();
    let kept = Spec { ty, len: was.len, decimals: was.decimals };
    let encoding = style.encoding.decoder();
    // Bytes of a value's text in the file's code page.
    let encoded_len = |v: &Value| {
        use shapefile::dbase::encoding::Encoding;
        let text = v.to_string();
        encoding.encode(&text).map_or(text.len(), |b| b.len())
    };
    let fits = match (field.ty, ty) {
        (FieldType::Integer, b'N' | b'F') if was.decimals > 0 => false,
        (FieldType::Integer | FieldType::Double, b'N' | b'F') => values().all(|v| {
            let d = usize::from(was.decimals);
            dbf_number(v, field.ty, d, usize::from(was.len)).is_some_and(|x| {
                let text = format!("{x:.d$}");
                text.len() <= usize::from(was.len) && text.parse::<f64>().ok() == v.as_f64()
            })
        }),
        (FieldType::Boolean, b'L') | (FieldType::Date, b'D') => true,
        (FieldType::Text | FieldType::DateTime, b'C') => values().all(|v| encoded_len(v) <= usize::from(was.len)),
        // Another kind of field now: say how its physical column changes too.
        _ => return (new, Some(format!("'{}' {kept} to {new}", dbf_notice_name(&field.name)))),
    };
    if fits {
        return (kept, None);
    }
    let spec = if new.ty == b'C' {
        // Text wider than the file's column: as wide as its longest value, in the file's code
        // page, up to the 254 bytes written (a longer value is cut, which publishing refuses).
        let longest = values().map(encoded_len).max().unwrap_or(0).min(254);
        Spec { len: u8::try_from(longest).unwrap_or(254), ..new }
    } else {
        // A number with more digits or decimals: never narrower than the file's column.
        Spec { len: new.len.max(was.len), ..new }
    };
    (spec, Some(format!("'{}' {kept} to {spec}", dbf_notice_name(&field.name))))
}

/// As [`shapefile_parts_in`], the `.dbf` written as `style` says when it's given (in the file's
/// code page, its columns kept where the values fit): also returns, for each field whose column
/// couldn't hold its values, how its column changed.
pub(crate) fn shapefile_parts_styled(
    ds: &Dataset,
    crs: Crs,
    style: Option<&DbfStyle>,
) -> Result<(Vec<(&'static str, Vec<u8>)>, usize, Vec<String>)> {
    use shapefile::dbase::{FieldName, FieldValue, Record, TableWriterBuilder};
    use std::io::Cursor;
    let fmt = |e: shapefile::Error| DataError::Format(format!("Shapefile: {e}"));
    let names = dbf_names(&ds.fields);
    let dbf_name = |n: &str| FieldName::try_from(n).map_err(|e| DataError::Format(format!("Shapefile: field {n}: {e}")));
    let mut table = match style {
        Some(style) => TableWriterBuilder::with_encoding(style.encoding.decoder()),
        None => TableWriterBuilder::new(),
    };
    // Per field: its column (numbers too large for it become null), and how the columns that
    // couldn't hold their values changed.
    let mut specs = Vec::with_capacity(ds.fields.len());
    let mut widened = Vec::new();
    for (k, n) in names.iter().enumerate() {
        let spec = match style {
            Some(style) => {
                let (spec, changed) = kept_column(ds, k, n, style);
                widened.extend(changed);
                spec
            }
            None => new_column(ds, k),
        };
        table = match spec.ty {
            // A float column is written as a numeric one (both hold the number as text) and
            // marked `F` again below: `dbase` writes an `F` column's number as an `f32`.
            b'N' | b'F' => table.add_numeric_field(dbf_name(n)?, spec.len, spec.decimals),
            b'L' => table.add_logical_field(dbf_name(n)?),
            b'D' => table.add_date_field(dbf_name(n)?),
            _ => table.add_character_field(dbf_name(n)?, spec.len),
        };
        specs.push(spec);
    }
    let record = |r: &crate::Feature| -> Record {
        let mut rec = Record::default();
        for (k, (f, n)) in ds.fields.iter().zip(&names).enumerate() {
            let v = r.attrs.get(k).unwrap_or(&Value::Null);
            let decimals = specs.get(k).map_or(0, |s| usize::from(s.decimals));
            let width = specs.get(k).map_or(0, |s| usize::from(s.len));
            let value = match f.ty {
                FieldType::Integer | FieldType::Double => FieldValue::Numeric(dbf_number(v, f.ty, decimals, width)),
                FieldType::Boolean => FieldValue::Logical(dbf_boolean(v)),
                FieldType::Date => FieldValue::Date(dbf_date(v).and_then(|d| {
                    let (y, m, d) = d.ymd();
                    shapefile::dbase::Date::new(u32::try_from(d).ok()?, u32::try_from(m).ok()?, u32::try_from(y).ok()?).ok()
                })),
                FieldType::Text | FieldType::DateTime => FieldValue::Character((!v.is_null()).then(|| match style {
                    Some(style) => cut_encoded(&v.to_string(), 254, &style.encoding).to_string(),
                    None => cut(&v.to_string(), 254).to_string(),
                })),
            };
            rec.insert(n.clone(), value);
        }
        rec
    };

    let pt = |c: &Coord| shapefile::Point::new(c.x, c.y);
    let finite = |c: &&Coord| c.x.is_finite() && c.y.is_finite();
    let (mut shp, mut shx, mut dbf) = (Cursor::new(Vec::new()), Cursor::new(Vec::new()), Cursor::new(Vec::new()));
    let mut left_out = 0;
    let mut written = 0;
    {
        let shapes = shapefile::ShapeWriter::with_shx(&mut shp, &mut shx);
        let mut w = shapefile::Writer::new(shapes, table.build_with_dest(&mut dbf));
        let multi_points = ds.features.iter().any(|f| matches!(f.geometry, Some(Geometry::MultiPoint(_))));
        for f in &ds.features {
            crate::progress::step(1)?;
            let ok = match (&f.geometry, ds.geometry_type) {
                (Some(Geometry::Point(c)), GeometryType::Point) if !multi_points && finite(&c) => {
                    w.write_shape_and_record(&pt(c), &record(f)).map_err(fmt)?;
                    true
                }
                (Some(g @ (Geometry::Point(_) | Geometry::MultiPoint(_))), GeometryType::Point) => {
                    let mut pts = Vec::new();
                    g.for_each_coord(|c| pts.push(c));
                    let pts: Vec<shapefile::Point> = pts.iter().filter(finite).map(pt).collect();
                    if pts.is_empty() {
                        false
                    } else {
                        w.write_shape_and_record(&shapefile::Multipoint::new(pts), &record(f)).map_err(fmt)?;
                        true
                    }
                }
                (Some(Geometry::Line(parts)), GeometryType::Polyline) => {
                    let parts: Vec<Vec<shapefile::Point>> =
                        parts.iter().map(|p| p.iter().filter(finite).map(pt).collect::<Vec<_>>()).filter(|p| p.len() >= 2).collect();
                    if parts.is_empty() {
                        false
                    } else {
                        w.write_shape_and_record(&shapefile::Polyline::with_parts(parts), &record(f)).map_err(fmt)?;
                        true
                    }
                }
                (Some(Geometry::Polygon(polys)), GeometryType::Polygon) => {
                    let mut rings = Vec::new();
                    for poly in polys {
                        let mut it = poly.iter().map(|r| r.iter().filter(finite).map(pt).collect::<Vec<_>>()).filter(|r| r.len() >= 3);
                        // A polygon whose outer ring is gone loses its holes too.
                        let outer_kept = poly.first().is_some_and(|r| r.iter().filter(finite).count() >= 3);
                        if let Some(outer) = it.next().filter(|_| outer_kept) {
                            rings.push(shapefile::PolygonRing::Outer(outer));
                            rings.extend(it.map(shapefile::PolygonRing::Inner));
                        }
                    }
                    if rings.is_empty() {
                        false
                    } else {
                        w.write_shape_and_record(&shapefile::Polygon::with_rings(rings), &record(f)).map_err(fmt)?;
                        true
                    }
                }
                _ => false,
            };
            if ok {
                written += 1;
            } else {
                left_out += 1;
            }
        }
        if written == 0 {
            let what = if ds.geometry_type == GeometryType::None { "a table without geometry" } else { "a layer with no features to draw" };
            return Err(DataError::Unsupported(format!("{what} as a Shapefile")));
        }
    }
    let prj = if crs == Crs::LonLat { Some(WGS84_PRJ.to_string()) } else { crate::raster::wkt::to_wkt(crs) };
    let mut dbf = dbf.into_inner();
    if let Some(style) = style {
        // The file's language driver, whatever `dbase` takes the encoding for, and its float
        // columns marked `F` again.
        if let Some(b) = dbf.get_mut(29) {
            *b = style.ldid;
        }
        for (k, spec) in specs.iter().enumerate() {
            if spec.ty == b'F'
                && let Some(b) = dbf.get_mut(32 + k * 32 + 11)
            {
                *b = b'F';
            }
        }
    }
    let mut parts = vec![("shp", shp.into_inner()), ("shx", shx.into_inner()), ("dbf", dbf)];
    parts.extend(prj.map(|p| ("prj", p.into_bytes())));
    parts.push(("cpg", b"UTF-8".to_vec()));
    Ok((parts, left_out, widened))
}

fn is_outer<P>(r: &shapefile::PolygonRing<P>) -> bool {
    matches!(r, shapefile::PolygonRing::Outer(_))
}

/// Groups a flat list of rings (outer flag) into polygons: each hole joins the preceding outer ring.
pub(crate) fn group_rings(rings: Vec<(Vec<Coord>, bool)>) -> Geometry {
    let mut polys: Vec<Vec<Vec<Coord>>> = Vec::new();
    for (ring, outer) in rings {
        match polys.last_mut() {
            Some(p) if !outer => p.push(ring),
            _ => polys.push(vec![ring]),
        }
    }
    Geometry::Polygon(polys)
}

#[expect(clippy::disallowed_methods, reason = "a thread the system refuses fails the test, as it should")]
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A write to a name nothing holds reserves it first: a second write to it is refused before
    /// anything is written, and dropping the write removes the reservation and the folder made for
    /// it; once installed, both stay (#2865).
    #[test]
    fn a_new_staged_file_reserves_its_name_and_leaves_nothing_when_dropped() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Survey.rgis.data");
        std::fs::create_dir(&folder).unwrap();
        let dest = folder.join("index.tif");
        let (staged, ()) = stage_streamed_new::<_, std::io::Error>(&dest, Some(&folder), |w| w.write_all(b"pixels")).unwrap();
        assert!(dest.is_file(), "reserved");
        let taken = stage_streamed_new::<(), std::io::Error>(&dest, None, |_| panic!("written to a name another holds"));
        assert_eq!(taken.err().map(|e| e.kind()), Some(std::io::ErrorKind::AlreadyExists));
        drop(staged);
        assert!(!dest.exists() && !folder.exists(), "the reservation and its folder go with it");
        std::fs::create_dir(&folder).unwrap();
        let (staged, ()) = stage_streamed_new::<_, std::io::Error>(&dest, Some(&folder), |w| w.write_all(b"pixels")).unwrap();
        staged.commit().unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"pixels");
    }

    /// Each extra cell is counted, including empty ones, and each affected row once, preserving named data.
    #[test]
    fn csv_extra_cells_are_counted() {
        for (text, expected) in [
            ("wkt,id,note\nPOINT(1 2),7,kept\nPOINT(3 4),8,\n", vec![]),
            (
                "wkt,id,note\nPOINT(1 2),7,kept,lost,\nPOINT(3 4),8,,tail\n",
                vec!["3 cells past the header's 3 columns were omitted in 2 rows"],
            ),
            ("wkt,id,note\nPOINT(1 2),7,kept,\nPOINT(3 4),8,\n", vec!["1 cell past the header's 3 columns was omitted in 1 row"]),
        ] {
            let (ds, report) = read_csv(text, "extra").unwrap();
            assert_eq!(ds.fields, [Field::new("id", FieldType::Integer), Field::new("note", FieldType::Text)]);
            assert_eq!(ds.features.len(), 2);
            assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(1.0, 2.0))));
            assert_eq!(ds.features[1].geometry, Some(Geometry::Point(Coord::new(3.0, 4.0))));
            assert_eq!(ds.features[0].attrs, [Value::Int(7), Value::Text("kept".into())]);
            assert_eq!(ds.features[1].attrs, [Value::Int(8), Value::Null]);
            assert_eq!(report.skipped, 0);
            assert_eq!(report.messages.texts().collect::<Vec<_>>(), expected);
            assert!(report.messages.iter().all(|m| m.loss == Some(crate::loss::LossCode::DroppedExtraAttributes)));
        }
    }

    /// A file refused because RustGIS holds much of the memory says so, with the limit when nothing
    /// is held and what to do; refused at the full limit, or where the limit doesn't follow the
    /// memory, it says what it said before (#2431).
    #[test]
    fn a_file_refused_while_memory_is_held_says_so() {
        const GIB: u64 = 1 << 30;
        if bytes_ceiling() == BYTES_FLOOR {
            return; // A phone or a 32-bit build: the limit doesn't follow the memory.
        }
        let lowered = too_large_held(2 * GIB, GIB, Some(64 * GIB), 40 * GIB, None);
        assert_eq!(
            lowered,
            "file is too large (2048 MiB; the most read into memory is 1024 MiB on this computer now, with RustGIS holding 40960 MiB, 2048 MiB otherwise): free memory by removing layers or tables you no longer need (save and reopen the project to let go of what Undo keeps of them) or closing other programs, then try again"
        );
        let busy = too_large_held(GIB, 512 << 20, Some(64 * GIB), GIB, Some(5 * GIB));
        assert!(busy.contains("with RustGIS holding 1024 MiB and 5120 MiB free, 2048 MiB otherwise"), "{busy}");
        let full = too_large_held(3 * GIB, 2 * GIB, Some(64 * GIB), GIB, Some(60 * GIB));
        assert_eq!(
            full,
            "file is too large (3072 MiB; the most read into memory is 2048 MiB on this computer, a limit that follows its memory)"
        );
        assert_eq!(too_large_held(3 * GIB, 2 * GIB, Some(64 * GIB), 0, None), full);
        assert_eq!(
            too_large_held(GIB, 256 << 20, None, 40 * GIB, None),
            "file is too large (1024 MiB; the most read into memory is 256 MiB)"
        );
    }

    /// The size read into memory follows the computer's memory between its floor and ceiling,
    /// and stays at the floor where the memory isn't known (the browser) (#2408).
    #[test]
    fn the_size_read_into_memory_follows_the_memory() {
        const GIB: u64 = 1 << 30;
        assert_eq!(max_bytes_for(None), BYTES_FLOOR);
        assert_eq!(max_bytes_for(Some(4 * GIB)), BYTES_FLOOR, "never below the floor");
        assert_eq!(max_bytes_for(Some(8 * GIB)), BYTES_FLOOR, "8 GiB plans for 2 GiB, 256 MiB of file");
        if bytes_ceiling() > BYTES_FLOOR {
            assert_eq!(max_bytes_for(Some(16 * GIB)), 512 << 20);
            assert_eq!(max_bytes_for(Some(64 * GIB)), 2 * GIB);
            assert_eq!(max_bytes_for(Some(1024 * GIB)), BYTES_CEILING, "never above the ceiling");
        } else {
            // A phone or a 32-bit build: the floor, whatever the memory.
            assert_eq!(max_bytes_for(Some(16 * GIB)), BYTES_FLOOR);
            assert_eq!(max_bytes_for(Some(1024 * GIB)), BYTES_FLOOR);
        }
        // Holding more than half the memory lowers it, never below the floor (#2431).
        if bytes_ceiling() > BYTES_FLOOR {
            assert_eq!(max_bytes_within(crate::memory::share(Some(16 * GIB), 8 * GIB, None)), 512 << 20, "half held: unchanged");
            assert_eq!(max_bytes_within(crate::memory::share(Some(64 * GIB), 40 * GIB, None)), GIB);
            assert_eq!(max_bytes_within(crate::memory::share(Some(16 * GIB), 12 * GIB, None)), BYTES_FLOOR);
        }
        // The test limit is the calling thread's, and comes back after it, nested too. The real one
        // follows what is free now (#2431), so it is only compared with its floor.
        with_max_bytes(10, || {
            assert_eq!(max_bytes(), 10);
            with_max_bytes(20, || assert_eq!(max_bytes(), 20));
            assert_eq!(max_bytes(), 10);
            assert!(std::thread::spawn(max_bytes).join().unwrap() >= BYTES_FLOOR);
        });
        assert!(max_bytes() >= BYTES_FLOOR);
        let said = with_max_bytes(1 << 20, || too_large(3 << 20));
        assert!(said.starts_with("file is too large (3 MiB;") && said.contains("is 1 MiB"), "{said}");
    }

    /// A file past the limit is refused before it is parsed, by each reader that takes it whole,
    /// saying its size and the limit; one within it is read.
    #[test]
    fn a_file_past_the_size_read_into_memory_is_refused_saying_the_limit() {
        let geojson = br#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#;
        let max = geojson.len() as u64 - 1;
        let e = with_max_bytes(max, || read_bytes("a.geojson", geojson)).unwrap_err().to_string();
        assert_eq!(e, file_size_error("a.geojson", max));
        assert!(with_max_bytes(max + 1, || read_bytes("a.geojson", geojson)).is_ok());
        let e = with_max_bytes(max, || read_layers("a.geojson", geojson)).err().map(|e| e.to_string()).unwrap_or_default();
        assert_eq!(e, file_size_error("a.geojson", max));
        // A KML stays within the floor with the limit raised; a GPX, read ahead, doesn't.
        let past_floor = vec![b' '; BYTES_FLOOR as usize + 1];
        let (kml, gpx) =
            with_max_bytes(2 * BYTES_FLOOR, || (read_layers("a.kml", &past_floor).err(), read_layers("a.gpx", &past_floor).err()));
        assert_eq!(kml.map(|e| e.to_string()), Some(file_size_error("a.kml", BYTES_FLOOR)));
        assert!(gpx.is_some_and(|e| file_size_error_args(&e.to_string()).is_none()), "a GPX is parsed, not refused by its size");
        let csv = b"x,y\n1,2\n";
        let e = with_max_bytes(4, || read_bytes("a.csv", csv)).unwrap_err().to_string();
        assert_eq!(e, file_size_error("a.csv", 4));
        // Compressed text within the size but expanding past it: refused by the size, without
        // the hint about its encoding.
        let gz = {
            use std::io::Write;
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
            encoder.write_all(&b"1,2\n".repeat(10_000)).unwrap();
            encoder.finish().unwrap()
        };
        let e = with_max_bytes(1000.max(gz.len() as u64), || read_bytes("a.csv.gz", &gz)).unwrap_err().to_string();
        assert!(e.contains("expanded to more than the limit") && e.contains("is 1000 bytes") && !e.contains("UTF-8"), "{e}");
        // The delimited reader's own guard, for callers that don't come through `read_bytes`.
        let e = with_max_bytes(4, || read_delimited_bytes("a.csv", csv, &CsvOptions::default())).unwrap_err().to_string();
        assert_eq!(e, file_size_error("a.csv", 4));
        // The encoding hint on compressed text that isn't UTF-8 only, not on one past the memory
        // budget.
        let latin1 = gzip_csv(b"name,x,y\nZ\xfcrich,1,2\n");
        let e = read_gzip_text_budget(latin1.as_slice(), "a.csv.gz", &CsvOptions::default(), &mut (1 << 20)).unwrap_err().to_string();
        assert!(e.contains("if not UTF-8, decompress"), "{e}");
        let utf8 = gzip_csv(&b"1,2\n".repeat(10_000));
        let e = read_gzip_text_budget(utf8.as_slice(), "a.csv.gz", &CsvOptions::default(), &mut 4096).unwrap_err().to_string();
        assert!(e.contains("memory budget") && !e.contains("UTF-8"), "{e}");
        // Within the size, but forty features each with a field of its own: a table of 1,600
        // cells, more than a file at the limit is planned to take.
        let sparse: Vec<String> = (0..40).map(|i| format!(r#"{{"type":"Feature","properties":{{"f{i}":1}},"geometry":null}}"#)).collect();
        let sparse = format!(r#"{{"type":"FeatureCollection","features":[{}]}}"#, sparse.join(","));
        let cells = 1600 * std::mem::size_of::<Value>() as u64;
        let max = (cells / MEMORY_PER_BYTE - 1).max(sparse.len() as u64);
        assert!(max * MEMORY_PER_BYTE < cells, "the test's file is too large for its table");
        let e = with_max_bytes(max, || read_bytes("a.geojson", sparse.as_bytes())).unwrap_err().to_string();
        assert!(e.contains("attribute table would take") && e.contains("40 features × 40 fields"), "{e}");
        assert!(with_max_bytes(cells, || read_bytes("a.geojson", sparse.as_bytes())).is_ok());
        let city = br#"<core:CityModel xmlns:core="http://www.opengis.net/citygml/2.0"></core:CityModel>"#;
        let e = with_max_bytes(8, || crate::citygml::read("a.gml", city, None)).unwrap_err().to_string();
        assert_eq!(e, file_size_error("a.gml", 8));
    }

    /// A whole-valued Double is read as Integer only when the header declares no decimals for it;
    /// when the header doesn't give its decimals at all, it stays Double.
    #[test]
    fn a_double_with_undeclared_decimals_stays_double() {
        let layer = || {
            let mut ds = Dataset::new("t", GeometryType::None, vec![Field::new("V", FieldType::Double)]);
            ds.push(None, vec![Value::Float(100.0)]);
            ds
        };
        let mut missing = layer();
        integral_numeric_to_integer(&mut missing, &[]);
        assert_eq!(missing.fields[0].ty, FieldType::Double);
        let mut none_declared = layer();
        integral_numeric_to_integer(&mut none_declared, &[0]);
        assert_eq!(none_declared.fields[0].ty, FieldType::Integer);
    }

    /// Text is cut to what its code page holds in the bytes given, at a character boundary.
    #[test]
    fn text_is_cut_to_its_code_page_bytes() {
        let cp1252 = crate::dbf::DbfEncoding::Standard(encoding_rs::WINDOWS_1252);
        let e = "é".repeat(300);
        assert_eq!(cut_encoded(&e, 254, &cp1252).chars().count(), 254);
        assert_eq!(cut_encoded(&e, 254, &crate::dbf::DbfEncoding::Utf8).chars().count(), 127);
        assert_eq!(cut_encoded("short", 254, &cp1252), "short");
        assert_eq!(cut_encoded("ab", 0, &cp1252), "");
    }

    /// A text column as wide as a `.dbf` allows (255 bytes, a width the writer never uses) with a
    /// longer value gets a column of the 254 bytes written, not a crash.
    #[test]
    fn a_full_width_text_column_with_a_longer_value_is_written_at_254() {
        let mut ds = Dataset::new("a", GeometryType::Point, vec![Field::new("NOTE", FieldType::Text)]);
        ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("x".repeat(300))]);
        let style = DbfStyle {
            encoding: crate::dbf::DbfEncoding::Utf8,
            ldid: 0,
            columns: vec![crate::dbf::Column { name: b"NOTE".to_vec(), ty: b'C', len: 255, decimals: 0 }],
        };
        let (spec, changed) = kept_column(&ds, 0, "NOTE", &style);
        assert_eq!(spec.len, 254);
        assert_eq!(changed.as_deref(), Some("'NOTE' C(255) to C(254)"));
    }

    /// A Shapefile export of a polygon layer holding a point and a feature without geometry writes
    /// the polygon and counts both left out, the point for its kind; a layer of polygons loses none.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_shapefile_counts_geometry_of_another_kind_left_out() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let geojson = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"name":"area"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}},
            {"type":"Feature","properties":{"name":"spot"},"geometry":{"type":"Point","coordinates":[5,5]}},
            {"type":"Feature","properties":{"name":"none"},"geometry":null}]}"#;
        let (mixed, _) = read_geojson(geojson, "mixed")?;
        let dir = tempfile::tempdir()?;
        assert_eq!(write_path(&mixed, &dir.path().join("mixed.shp"))?, 2);
        let (back, _) = read_path(&dir.path().join("mixed.shp"))?;
        assert_eq!(back.features.len(), 1);
        let mut areas = mixed.clone();
        areas.features.truncate(1);
        assert_eq!(write_path(&areas, &dir.path().join("areas.shp"))?, 0);
        Ok(())
    }

    /// Placing a Shapefile's parts: old parts whose copies can't be removed once the new ones land
    /// are returned and said; a clean placement leaves and says nothing; a placement rolled back
    /// whole names the file and says nothing changed.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn placing_a_shapefile_says_old_parts_it_keeps_and_a_rollback() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let shp = dir.path().join("parcels.shp");
        let dbf = dir.path().join("parcels.dbf");
        let entries = || -> std::io::Result<usize> { Ok(std::fs::read_dir(dir.path())?.count()) };
        std::fs::write(&shp, b"old shp")?;
        std::fs::write(&dbf, b"old dbf")?;
        let parts = |tag: &str| vec![(shp.clone(), format!("{tag} shp").into_bytes()), (dbf.clone(), format!("{tag} dbf").into_bytes())];

        KEEP_BACKUPS.set(true);
        let kept = place_shapefile(&shp, &stage_shapefile(&parts("new"))?);
        KEEP_BACKUPS.set(false);
        let kept = kept?;
        assert_eq!(std::fs::read(&shp)?, b"new shp");
        assert_eq!(kept.len(), 2);
        assert!(kept.iter().all(|k| k.exists()));
        let note = kept_backups_note(&shp, &kept).unwrap_or_default();
        assert!(note.contains("2 old part(s) couldn't be removed") && note.contains("can be deleted"), "{note}");
        assert!(kept.iter().all(|k| note.contains(&k.display().to_string())), "{note}");
        for k in &kept {
            std::fs::remove_file(k)?;
        }

        // Clean: nothing kept, nothing said, nothing left beside the file.
        let kept = place_shapefile(&shp, &stage_shapefile(&parts("newer"))?)?;
        assert!(kept.is_empty() && kept_backups_note(&shp, &kept).is_none());
        assert_eq!(entries()?, 2);

        // A part that fails to land (its staged copy gone): every old part is put back.
        let staged = stage_shapefile(&parts("newest"))?;
        let (gone, _) = staged.iter().find(|(_, target)| *target == dbf).ok_or("no staged .dbf")?;
        std::fs::remove_file(gone)?;
        let error = place_shapefile(&shp, &staged).err().ok_or("placed")?.to_string();
        assert!(
            error.contains(&format!("{}: ", shp.display())) && error.ends_with("the old file was put back, nothing changed"),
            "{error}"
        );
        assert_eq!(std::fs::read(&shp)?, b"newer shp");
        assert_eq!(std::fs::read(&dbf)?, b"newer dbf");
        assert_eq!(entries()?, 2);

        // A new file that fails to land had nothing to put back, and says so.
        let fresh = dir.path().join("fresh.shp");
        let parts = vec![(fresh.clone(), b"shp".to_vec()), (fresh.with_extension("dbf"), b"dbf".to_vec())];
        let staged = stage_shapefile(&parts)?;
        let (gone, _) = staged.iter().find(|(_, target)| *target == fresh).ok_or("no staged .shp")?;
        std::fs::remove_file(gone)?;
        let error = place_shapefile(&fresh, &staged).err().ok_or("placed")?.to_string();
        assert!(error.ends_with("nothing was written") && !error.contains("put back"), "{error}");
        assert_eq!(entries()?, 2);

        // An export over the file says the old parts it keeps.
        let mut ds = Dataset::new("parcels", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![]);
        KEEP_BACKUPS.set(true);
        let written = write_path_to(&ds, &shp, Crs::LonLat, &GpkgTable::default());
        KEEP_BACKUPS.set(false);
        let (_, _, notes) = written?;
        assert!(notes.len() == 1 && notes.iter().all(|n| n.contains("couldn't be removed")), "{notes:?}");
        Ok(())
    }

    /// The left-out notice names what each format can't hold: a Shapefile, features without
    /// drawable geometry or of another kind; the others, features without drawable geometry.
    /// Nothing left out, no notice.
    #[test]
    fn left_out_notes_say_what_the_format_cant_hold() {
        for ext in ["shp", "zip"] {
            assert_eq!(
                left_out_note("out", ext, 2).as_deref(),
                Some(
                    "out: 2 features left out of the Shapefile: without drawable geometry, or with geometry of another kind than the layer's"
                )
            );
        }
        for ext in ["fgb", "kml", "gpx"] {
            assert_eq!(left_out_note("out", ext, 1).as_deref(), Some("out: 1 features without drawable geometry were left out"));
        }
        assert_eq!(left_out_note("out", "shp", 0), None);
        assert_eq!(left_out_note("out", "gpkg", 0), None);
    }

    /// A Shapefile export leaves out vertices with a missing or infinite coordinate, and the line
    /// parts and polygon rings that leaves too short to draw, and says how many; the features
    /// themselves are written. A feature with nothing drawable left is the left-out notice's, and
    /// clean geometry gives no notice (#1687).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_shapefile_says_which_vertices_and_parts_it_leaves_out() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let c = Coord::new;
        let nan = f64::NAN;
        let inf = f64::INFINITY;
        let dir = tempfile::tempdir()?;
        let coords = |g: &Option<Geometry>| {
            let mut n = 0;
            if let Some(g) = g {
                g.for_each_coord(|_| n += 1);
            }
            n
        };

        // The issue's case: a multipoint of four, one member's x missing, is written with three.
        let mut points = Dataset::new("mp", GeometryType::Point, vec![]);
        points.push(Some(Geometry::MultiPoint(vec![c(0., 0.), c(nan, 1.), c(2., 2.), c(3., 3.)])), vec![]);
        assert_eq!(write_path(&points, &dir.path().join("mp.shp"))?, 0);
        let (back, _) = read_path(&dir.path().join("mp.shp"))?;
        assert_eq!(coords(&back.features[0].geometry), 3);
        assert_eq!(
            shapefile_geometry_notes(&points, "mp.shp", Crs::LonLat),
            ["mp.shp: 1 vertex with a missing or infinite coordinate was left out of the Shapefile"]
        );

        // Lines: two vertices go, and the part left with one finite vertex goes with them, as does
        // a part written with one vertex.
        let mut lines = Dataset::new("lines", GeometryType::Polyline, vec![]);
        let parts = vec![vec![c(0., 0.), c(nan, 1.), c(2., 2.)], vec![c(5., 5.), c(inf, 6.)], vec![c(9., 9.)]];
        lines.push(Some(Geometry::Line(parts)), vec![]);
        assert_eq!(write_path(&lines, &dir.path().join("lines.shp"))?, 0);
        let (back, _) = read_path(&dir.path().join("lines.shp"))?;
        assert_eq!(coords(&back.features[0].geometry), 2);
        assert_eq!(
            shapefile_geometry_notes(&lines, "lines.shp", Crs::LonLat),
            [
                "lines.shp: 2 vertices with a missing or infinite coordinate were left out of the Shapefile",
                "lines.shp: 2 line parts were left out of the Shapefile, with fewer than 2 drawable vertices",
            ]
        );

        // Polygons: the first keeps its outer ring but loses a hole left with two vertices; the
        // second loses its outer ring, and its whole hole with it.
        let square = |x: f64| vec![c(x, 0.), c(x + 4., 0.), c(x + 4., 4.), c(x, 4.), c(x, 0.)];
        let hole = |x: f64| vec![c(x + 1., 1.), c(x + 2., 1.), c(x + 2., 2.), c(x + 1., 1.)];
        let mut broken_hole = hole(0.);
        broken_hole[1] = c(nan, nan);
        broken_hole[2] = c(nan, 2.);
        let mut broken_outer = square(10.);
        broken_outer[1] = c(inf, 0.);
        broken_outer[2] = c(14., nan);
        broken_outer[3] = c(nan, 4.);
        let mut polys = Dataset::new("polys", GeometryType::Polygon, vec![]);
        polys.push(Some(Geometry::Polygon(vec![vec![square(0.), broken_hole], vec![broken_outer, hole(10.)]])), vec![]);
        assert_eq!(write_path(&polys, &dir.path().join("polys.shp"))?, 0);
        let (back, _) = read_path(&dir.path().join("polys.shp"))?;
        assert_eq!(coords(&back.features[0].geometry), 5);
        assert_eq!(
            shapefile_geometry_notes(&polys, "polys.shp", Crs::LonLat),
            [
                "polys.shp: 5 vertices with a missing or infinite coordinate were left out of the Shapefile",
                "polys.shp: 3 polygon rings were left out of the Shapefile, with fewer than 3 drawable vertices or in a polygon whose outer ring was left out",
            ]
        );

        // A feature with nothing drawable left is counted left out whole, not vertex by vertex.
        let mut gone = Dataset::new("gone", GeometryType::Point, vec![]);
        gone.push(Some(Geometry::MultiPoint(vec![c(nan, 0.), c(0., inf)])), vec![]);
        gone.push(Some(Geometry::Point(c(1., 1.))), vec![]);
        assert_eq!(write_path(&gone, &dir.path().join("gone.shp"))?, 1);
        assert!(shapefile_geometry_notes(&gone, "gone.shp", Crs::LonLat).is_empty());

        // Clean geometry: nothing to say.
        for ds in [&points, &lines, &polys] {
            let mut clean = ds.clone();
            clean.features[0].geometry = match ds.geometry_type {
                GeometryType::Point => Some(Geometry::MultiPoint(vec![c(0., 0.), c(1., 1.)])),
                GeometryType::Polyline => Some(Geometry::Line(vec![vec![c(0., 0.), c(1., 1.)]])),
                _ => Some(Geometry::Polygon(vec![vec![square(0.), hole(0.)]])),
            };
            assert!(shapefile_geometry_notes(&clean, "clean.shp", Crs::LonLat).is_empty(), "{}", ds.name);
        }

        // Projected, the export writes the projected copy. Web Mercator writes an infinite
        // latitude at 85.05°, so that vertex and its part are written and nothing is said of
        // them; a part of one vertex is still left out. A missing coordinate can't be projected,
        // so the export is refused. In longitude/latitude, the infinite latitude goes, and its
        // part with it.
        let mut merc = Dataset::new("merc", GeometryType::Polyline, vec![]);
        merc.push(Some(Geometry::Line(vec![vec![c(0., 0.), c(1., inf)], vec![c(5., 5.)], vec![c(7., 7.), c(8., 8.)]])), vec![]);
        assert_eq!(write_path_in(&merc, &dir.path().join("merc.shp"), Crs::WebMercator)?, 0);
        let (back, _) = read_path(&dir.path().join("merc.shp"))?;
        assert_eq!(coords(&back.features[0].geometry), 4);
        assert_eq!(
            shapefile_geometry_notes(&merc, "merc.shp", Crs::WebMercator),
            ["merc.shp: 1 line part was left out of the Shapefile, with fewer than 2 drawable vertices"]
        );
        assert_eq!(write_path(&merc, &dir.path().join("merc-4326.shp"))?, 0);
        let (back, _) = read_path(&dir.path().join("merc-4326.shp"))?;
        assert_eq!(coords(&back.features[0].geometry), 2);
        assert_eq!(
            shapefile_geometry_notes(&merc, "merc-4326.shp", Crs::LonLat),
            [
                "merc-4326.shp: 1 vertex with a missing or infinite coordinate was left out of the Shapefile",
                "merc-4326.shp: 2 line parts were left out of the Shapefile, with fewer than 2 drawable vertices",
            ]
        );
        assert!(write_path_in(&points, &dir.path().join("mp-3857.shp"), Crs::WebMercator).is_err());
        Ok(())
    }

    /// A GeoPackage export loses no geometry: a layer read from a GeoJSON of a polygon and a point
    /// (declared a polygon layer, by its most common kind) gets a GEOMETRY column holding both, and
    /// a feature without geometry stays, as a table holds it. A layer of one kind keeps its kind's
    /// column, whether it declares its type or not.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_geopackage_of_a_mixed_layer_keeps_every_kind() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let geojson = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"name":"area"},"geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}},
            {"type":"Feature","properties":{"name":"spot"},"geometry":{"type":"Point","coordinates":[5,5]}},
            {"type":"Feature","properties":{"name":"none"},"geometry":null}]}"#;
        let (mixed, _) = read_geojson(geojson, "mixed")?;
        assert_eq!(mixed.geometry_type, GeometryType::Polygon);
        let dir = tempfile::tempdir()?;
        // The column's type, and the rows written with whether each has geometry, read with SQL.
        let table = |path: &Path| -> std::result::Result<(String, Vec<(String, bool)>), Box<dyn std::error::Error>> {
            let conn = rusqlite::Connection::open(path)?;
            let kind: String = conn.query_row("SELECT geometry_type_name FROM gpkg_geometry_columns", [], |r| r.get(0))?;
            let mut rows = conn.prepare(r#"SELECT "name", "geom" IS NOT NULL FROM "mixed" ORDER BY "fid""#)?;
            let written = rows.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<std::result::Result<_, _>>()?;
            Ok((kind, written))
        };
        let all = vec![("area".to_string(), true), ("spot".to_string(), true), ("none".to_string(), false)];
        let path = dir.path().join("mixed.gpkg");
        assert_eq!(write_path(&mixed, &path)?, 0);
        assert_eq!(table(&path)?, ("GEOMETRY".to_string(), all.clone()));
        // Read back, the point and the polygon are both there.
        let (back, _) = read_path(&path)?;
        let mut kinds: Vec<GeometryType> = back.features.iter().filter_map(|f| f.geometry.as_ref().map(Geometry::kind)).collect();
        kinds.sort_by_key(|k| format!("{k:?}"));
        assert_eq!(kinds, [GeometryType::Point, GeometryType::Polygon]);
        // A layer that declares no geometry type does the same.
        let mut typeless = mixed.clone();
        typeless.geometry_type = GeometryType::None;
        let at = dir.path().join("typeless.gpkg");
        assert_eq!(write_path(&typeless, &at)?, 0);
        assert_eq!(table(&at)?, ("GEOMETRY".to_string(), all));
        // Projected, every feature is written too.
        let utm = Crs::from_epsg(32631).ok_or("no UTM zone 31N")?;
        let projected = dir.path().join("projected.gpkg");
        assert_eq!(write_path_in(&mixed, &projected, utm)?, 0);
        assert_eq!(table(&projected)?.0, "GEOMETRY");
        // One kind, declared or not: that kind's column, nothing left out.
        let mut areas = mixed.clone();
        areas.features.retain(|f| !matches!(f.geometry, Some(Geometry::Point(_))));
        let one = vec![("area".to_string(), true), ("none".to_string(), false)];
        let at = dir.path().join("areas.gpkg");
        assert_eq!(write_path(&areas, &at)?, 0);
        assert_eq!(table(&at)?, ("MULTIPOLYGON".to_string(), one.clone()));
        areas.geometry_type = GeometryType::None;
        let at = dir.path().join("typeless-areas.gpkg");
        assert_eq!(write_path(&areas, &at)?, 0);
        assert_eq!(table(&at)?, ("MULTIPOLYGON".to_string(), one));
        Ok(())
    }

    /// Exercises staging and backup cleanup through the public writer, one fresh process per collision.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn shapefile_staging_collisions_preserve_existing_entries() -> std::io::Result<()> {
        let (original, _) =
            read_geojson(r#"{"type":"Feature","properties":{"name":"old"},"geometry":{"type":"Point","coordinates":[1,2]}}"#, "roads")
                .map_err(std::io::Error::other)?;
        let (parts, _) = shapefile_parts(&original).map_err(std::io::Error::other)?;
        const CHILD: &str = "RUSTGIS_SHAPEFILE_STAGING_COLLISION";
        let Ok(case) = std::env::var(CHILD) else {
            let kinds: &[&str] = if cfg!(unix) { &["file", "live", "dangling"] } else { &["file"] };
            for phase in ["stage", "backup"] {
                for index in 0..parts.len() {
                    for kind in kinds {
                        let output = std::process::Command::new(std::env::current_exe()?)
                            .args([
                                "--exact",
                                "io::tests::shapefile_staging_collisions_preserve_existing_entries",
                                "--nocapture",
                                "--test-threads=1",
                            ])
                            .env(CHILD, format!("{phase}:{index}:{kind}"))
                            .output()?;
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        assert!(
                            output.status.success()
                                && stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored;")
                                && stdout.contains(&format!("collision case completed: {phase}:{index}:{kind}")),
                            "phase={phase} part={index} kind={kind}: {}{}",
                            stdout,
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                }
            }
            println!("collision scenarios: {}", 2 * parts.len() * kinds.len());
            return Ok(());
        };
        let (phase, case_parts) = case.split_once(':').ok_or_else(|| std::io::Error::other("missing collision phase"))?;
        let (index, kind) = case_parts.split_once(':').ok_or_else(|| std::io::Error::other("missing collision case"))?;
        let index = index.parse::<usize>().map_err(std::io::Error::other)?;
        if !matches!(kind, "file" | "live" | "dangling") {
            return Err(std::io::Error::other("unknown collision kind"));
        }
        let workspace = tempfile::Builder::new().prefix("rustgis-shp-collision-").tempdir()?;
        let root = workspace.path();
        let path = root.join("roads.SHP");
        let destination = |extension: &str| if extension == "shp" { path.clone() } else { path.with_extension(extension) };
        for (extension, bytes) in &parts {
            std::fs::write(destination(extension), bytes)?;
        }
        let mut ordered: Vec<_> = parts.iter().collect();
        if phase == "backup" {
            ordered.sort_by_key(|(extension, _)| *extension == "shp");
        }
        let (extension, _) = ordered.get(index).ok_or_else(|| std::io::Error::other("collision part out of range"))?;
        let target = destination(extension);
        // This child starts the counter at zero; backups follow all staged parts.
        let mut name = std::ffi::OsString::from(".");
        name.push(target.file_name().ok_or_else(|| std::io::Error::other("missing part name"))?);
        let counter = if phase == "backup" { parts.len() + index } else { index };
        name.push(format!(".{}.{counter}.tmp", std::process::id()));
        let collision = target.with_file_name(name);
        #[cfg(unix)]
        let link_target = root.join("link-target");
        if kind == "file" {
            std::fs::write(&collision, b"unrelated entry")?;
        }
        #[cfg(unix)]
        if kind != "file" {
            if kind == "live" {
                std::fs::write(&link_target, b"unrelated target")?;
            }
            std::os::unix::fs::symlink(&link_target, &collision)?;
        }
        let (replacement, _) =
            read_geojson(r#"{"type":"Feature","properties":{"name":"new"},"geometry":{"type":"Point","coordinates":[3,4]}}"#, "roads")
                .map_err(std::io::Error::other)?;
        let result = write_path(&replacement, &path);
        assert!(matches!(&result, Err(DataError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists), "{result:?}");
        if kind == "file" {
            assert_eq!(std::fs::read(&collision)?, b"unrelated entry");
        }
        #[cfg(unix)]
        if kind != "file" {
            assert_eq!(std::fs::read_link(&collision)?, link_target);
            if kind == "live" {
                assert_eq!(std::fs::read(&link_target)?, b"unrelated target");
            } else {
                assert!(!link_target.exists());
            }
        }
        for (extension, bytes) in &parts {
            assert_eq!(&std::fs::read(destination(extension))?, bytes, "original {extension} changed");
        }
        assert_eq!(std::fs::read_dir(root)?.count(), parts.len() + 1 + usize::from(kind == "live"), "owned staging files were not cleaned");
        std::fs::remove_file(collision)?;
        assert_eq!(write_path(&replacement, &path).map_err(std::io::Error::other)?, 0);
        for (extension, bytes) in shapefile_parts(&replacement).map_err(std::io::Error::other)?.0 {
            assert_eq!(std::fs::read(destination(extension))?, bytes, "retry changed {extension}");
        }
        workspace.close()?;
        println!("collision case completed: {case}");
        Ok(())
    }

    #[test]
    fn staging_collision_preserves_the_existing_file() -> std::io::Result<()> {
        let path = temp_beside(&std::env::temp_dir().join("rustgis-staging-collision"))?;
        write_new(&path, b"unrelated existing file")?;
        let error = write_new(&path, b"must not replace").err().ok_or_else(|| std::io::Error::other("collision succeeded"))?;
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path)?, b"unrelated existing file");
        std::fs::remove_file(path)
    }

    #[cfg(unix)]
    #[test]
    fn staging_collision_preserves_symlinks_and_their_targets() -> std::io::Result<()> {
        let target = temp_beside(&std::env::temp_dir().join("rustgis-staging-target"))?;
        let link = temp_beside(&target)?;
        write_new(&target, b"unrelated target")?;
        std::os::unix::fs::symlink(&target, &link)?;
        assert!(write_new(&link, b"must not follow").is_err());
        assert_eq!(std::fs::read_link(&link)?, target);
        assert_eq!(std::fs::read(&target)?, b"unrelated target");
        std::fs::remove_file(&target)?;
        assert!(write_new(&link, b"must not replace dangling link").is_err());
        assert_eq!(std::fs::read_link(&link)?, target);
        assert!(!target.exists());
        std::fs::remove_file(link)
    }

    #[test]
    fn write_atomic_failed_install_preserves_destination_and_removes_staging() -> std::io::Result<()> {
        let root = temp_beside(&std::env::temp_dir().join("rustgis-atomic-failed-install"))?;
        std::fs::create_dir(&root)?;
        let destination = root.join("occupied");
        std::fs::create_dir(&destination)?;
        let marker = destination.join("keep.txt");
        std::fs::write(&marker, b"keep destination contents")?;
        assert!(write_atomic(&destination, b"must not replace directory").is_err());
        assert_eq!(std::fs::read(marker)?, b"keep destination contents");
        assert_eq!(std::fs::read_dir(&root)?.count(), 1, "failed install left a staging file");
        std::fs::remove_dir_all(root)
    }

    /// Saving through a link (a project kept in a synced folder, linked from elsewhere) updates
    /// the file it leads to, relative or dangling, and leaves the link in place; a loop is an
    /// error that changes nothing.
    #[cfg(unix)]
    #[test]
    fn write_atomic_writes_through_symbolic_links() -> std::io::Result<()> {
        let root = temp_beside(&std::env::temp_dir().join("rustgis-atomic-link"))?;
        std::fs::create_dir(&root)?;
        let real_dir = root.join("real");
        std::fs::create_dir(&real_dir)?;
        let real = real_dir.join("project.rgis");
        std::fs::write(&real, b"old")?;
        let link = root.join("project.rgis");
        std::os::unix::fs::symlink("real/project.rgis", &link)?;
        let chained = root.join("chained.rgis");
        std::os::unix::fs::symlink(&link, &chained)?;
        write_atomic(&chained, b"new")?;
        assert_eq!(std::fs::read(&real)?, b"new");
        assert!(std::fs::symlink_metadata(&link)?.file_type().is_symlink());
        assert!(std::fs::symlink_metadata(&chained)?.file_type().is_symlink());
        assert_eq!(std::fs::read_dir(&real_dir)?.count(), 1, "a temp file was left behind");
        // A dangling link gets its file created.
        let missing = real_dir.join("missing.rgis");
        let dangling = root.join("dangling.rgis");
        std::os::unix::fs::symlink(&missing, &dangling)?;
        write_atomic(&dangling, b"made")?;
        assert_eq!(std::fs::read(&missing)?, b"made");
        assert!(std::fs::symlink_metadata(&dangling)?.file_type().is_symlink());
        // A loop is refused without touching anything.
        let (a, b) = (root.join("a.rgis"), root.join("b.rgis"));
        std::os::unix::fs::symlink(&b, &a)?;
        std::os::unix::fs::symlink(&a, &b)?;
        assert!(write_atomic(&a, b"loop").is_err());
        assert!(std::fs::symlink_metadata(&a)?.file_type().is_symlink());
        assert_eq!(std::fs::read_dir(&root)?.count(), 6, "a temp file was left behind");
        let mut target = real.clone();
        for i in 0..MAX_LINKS {
            let next = root.join(format!("chain-{i}"));
            std::os::unix::fs::symlink(&target, &next)?;
            target = next;
        }
        write_atomic(&target, b"forty links")?;
        assert_eq!(std::fs::read(&real)?, b"forty links");
        let too_many = root.join("chain-over-limit");
        std::os::unix::fs::symlink(&target, &too_many)?;
        assert!(write_atomic(&too_many, b"too many").is_err());
        assert_eq!(std::fs::read(&real)?, b"forty links");
        // Managed files replace the link itself, retaining the target's original contents.
        replace_atomic(&link, b"managed file")?;
        assert!(!std::fs::symlink_metadata(&link)?.file_type().is_symlink());
        assert_eq!(std::fs::read(&link)?, b"managed file");
        assert_eq!(std::fs::read(&real)?, b"forty links");
        std::fs::remove_dir_all(root)
    }

    /// A private file stays private when saved over, and a new file still gets the usual mode.
    #[cfg(unix)]
    #[test]
    fn write_atomic_keeps_permissions() -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = temp_beside(&std::env::temp_dir().join("rustgis-atomic-mode"))?;
        std::fs::create_dir(&root)?;
        let private = root.join("private.rgis");
        std::fs::write(&private, b"old")?;
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o600))?;
        write_atomic(&private, b"new")?;
        assert_eq!(std::fs::read(&private)?, b"new");
        assert_eq!(std::fs::metadata(&private)?.permissions().mode() & 0o7777, 0o600);
        // A read-only file is still replaced, and stays read-only.
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o400))?;
        write_atomic(&private, b"newer")?;
        assert_eq!(std::fs::read(&private)?, b"newer");
        assert_eq!(std::fs::metadata(&private)?.permissions().mode() & 0o7777, 0o400);
        // A new file is created as before: owner read-write, the rest by the umask.
        let fresh = root.join("fresh.rgis");
        write_atomic(&fresh, b"x")?;
        assert_eq!(std::fs::metadata(&fresh)?.permissions().mode() & 0o600, 0o600);
        std::fs::remove_dir_all(root)
    }

    #[test]
    fn topojson_named_json_from_disk() {
        let topo = r#"{"type":"Topology","objects":{"pts":{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[5,6]}]},"more":{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":[1,2]}]}},"arcs":[]}"#;
        let path = std::env::temp_dir().join(format!("rustgis-topo-{}.json", std::process::id()));
        std::fs::write(&path, topo).unwrap();
        let (ds, report) = read_path(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(ds.features.len(), 1);
        assert!(report.messages.iter().any(|m| m.contains("also holds")), "{:?}", report.messages);
    }

    #[test]
    fn formats_by_contents() {
        let cases: [(&[u8], Option<&str>); 11] = [
            (b"PAR1\0\0", Some("parquet")),
            (b"fgb\x03fgb\0", Some("fgb")),
            (b"II*\0\x08\0", Some("tif")),
            (b"PK\x03\x04", Some("zip")),
            (b"SQLite format 3\0", Some("gpkg")),
            (b"\xef\xbb\xbf  {\"type\":\"FeatureCollection\"}", Some("geojson")),
            (b"<?xml version=\"1.0\"?><kml xmlns=\"\">", Some("kml")),
            (b"name;lon;lat\r\nA;7;45\r\n", Some("csv")),
            (b"name\tlon\tlat\n\nA\t7\t45", Some("csv")),
            // A sentence with a comma isn't a table, nor a header alone.
            (b"Not found, try again later.\n", None),
            (b"name,lon,lat", None),
        ];
        for (bytes, want) in cases {
            assert_eq!(sniff_extension(bytes), want, "{:?}", String::from_utf8_lossy(bytes));
        }
        assert_eq!(sniff_extension(b"a,b\n1,2,3\n"), None);
        assert_eq!(sniff_extension(b"a,b\n1,\x01\n"), None);
    }

    #[test]
    fn geojson_roundtrip() {
        let text = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"name":"A","pop":5},"geometry":{"type":"Point","coordinates":[1,2]}},
            {"type":"Feature","properties":{"name":"B","pop":2.5},"geometry":{"type":"Point","coordinates":[3,4]}},
            {"type":"Feature","properties":{"name":"C"},"geometry":{"type":"Point","coordinates":["x",4]}}]}"#;
        let (ds, rep) = read_geojson(text, "t").unwrap();
        assert_eq!(ds.features.len(), 2);
        assert_eq!(rep.skipped, 1);
        assert_eq!(ds.fields[1].ty, FieldType::Double);
        assert_eq!(ds.geometry_type, GeometryType::Point);
        let back = read_geojson(&to_geojson(&ds).to_string(), "t").unwrap().0;
        assert_eq!(back.features.len(), 2);
        assert_eq!(back.value(1, 0), &Value::Text("B".into()));
    }

    #[test]
    fn geojson_collection_never_exports_only_a_member() {
        let point = json!({"type":"Point", "coordinates":[1,2]});
        let polygon = json!({"type":"Polygon", "coordinates":[[[0,0],[4,0],[4,4],[0,0]]]});
        let collection = json!({"type":"GeometryCollection", "geometries":[point, polygon]});
        let nested = json!({"type":"GeometryCollection", "geometries":[collection]});
        for geometry in [collection, nested] {
            let feature = json!({"type":"Feature", "properties":{}, "geometry":geometry});
            let fc = json!({"type":"FeatureCollection", "features":[feature]});
            for root in [&geometry, &feature, &fc] {
                let (ds, report) = read_geojson(&root.to_string(), "collection").unwrap();
                assert!(ds.features.is_empty(), "must not retain a partial collection: {root}");
                assert_eq!(report.skipped, 1);
                assert_eq!(report.messages.texts().collect::<Vec<_>>(), ["1 features had unreadable geometry"]);
                assert_eq!(report.messages[0].loss, Some(crate::loss::LossCode::DroppedFeature));
                assert!(to_geojson(&ds)["features"].as_array().unwrap().is_empty());
            }
        }
    }

    #[test]
    fn geojson_single_member_collection_is_lossless_and_quiet() {
        let geometry = json!({"type":"GeometryCollection", "geometries":[
            {"type":"GeometryCollection", "geometries":[{"type":"Point", "coordinates":[1,2]}]}
        ]});
        let (ds, report) = read_geojson(&geometry.to_string(), "single").unwrap();
        assert_eq!(ds.features.len(), 1);
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(1.0, 2.0))));
        assert_eq!(report.skipped, 0);
        assert!(report.messages.is_empty());
    }

    #[test]
    fn geojson_null_first_keeps_numeric_type() {
        let text = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"v":null,"n":null},"geometry":null},
            {"type":"Feature","properties":{"v":1,"n":null},"geometry":null},
            {"type":"Feature","properties":{"v":2.5},"geometry":null}]}"#;
        let (ds, _) = read_geojson(text, "t").unwrap();
        let (v, n) = (ds.field_index("v").unwrap(), ds.field_index("n").unwrap());
        assert_eq!(ds.fields[v].ty, FieldType::Double);
        assert_eq!(ds.fields[n].ty, FieldType::Text, "all-null columns are Text");
        assert_eq!(ds.value(1, v), &Value::Float(1.0));
    }

    #[test]
    fn write_atomic_replaces_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("rustgis-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("out.txt");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp files left behind");
        assert!(write_atomic(Path::new("/"), b"x").is_err());
        // Written as it goes: a failure part way leaves the file as it was, and no temp file.
        let failed = write_atomic_with(&p, |w| {
            w.write_all(b"half")?;
            Err::<(), _>(std::io::Error::other("stopped"))
        });
        assert!(failed.is_err());
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp files left behind");
        write_atomic_with(&p, |w| w.write_all(b"th").and_then(|()| w.write_all(b"ree"))).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"three");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_bytes_dispatches_by_extension() {
        let (ds, _) = read_bytes("pts.CSV", b"lon,lat\n1,2\n").unwrap();
        assert_eq!(ds.geometry_type, GeometryType::Point);
        assert!(read_bytes("x.shp", b"").is_err());
        assert!(read_bytes("x.geojson", &[0xff, 0xfe]).is_err());
    }

    /// Projected x/y columns with a coordinate system, other separators, WKT and EWKT columns, and
    /// the notes when something can't be placed.
    #[test]
    fn delimited_text_options() {
        use crate::raster::Crs;
        assert_eq!(sniff_delimiter("a;b;c\n1;2;3"), b';');
        assert_eq!(sniff_delimiter("a\tb\n"), b'\t');
        assert_eq!(sniff_delimiter("\"a;b\",c\n"), b',');
        assert_eq!(sniff_delimiter("one\n"), b',');
        // A semicolon in a one-column file's heading doesn't split it once rows show the shape.
        assert_eq!(sniff_delimiter("Name; Surname,id\nAda Lovelace,1\nAlan Turing,2\n"), b',');
        assert_eq!(sniff_delimiter("a;b,c\n1;2,3\n"), b',', "a tie keeps the comma");
        assert_eq!(sniff_delimiter("x;y\n1,5;2\n3,5;4\n"), b';', "decimal commas");
        // Central Park in New York State Plane (US feet), semicolon-separated, columns named.
        let text = "id;E;N\n1;992000;220000\n2;x;1\n";
        let opts =
            CsvOptions { x: Some("e".into()), y: Some("N".into()), crs: Some(Crs::parse("EPSG:2263").unwrap()), ..CsvOptions::default() };
        let (ds, rep) = read_csv_with(text, "t", &opts).unwrap();
        assert_eq!(ds.geometry_type, GeometryType::Point);
        // Held in the file's system; placed in longitude/latitude.
        assert_eq!(ds.held_in, Crs::parse("EPSG:2263").unwrap());
        let ds = ds.in_lonlat().unwrap().into_owned();
        let Some(Geometry::Point(c)) = &ds.features[0].geometry else { panic!() };
        assert!((c.x + 73.96).abs() < 0.05 && (c.y - 40.78).abs() < 0.05, "{c:?}");
        assert_eq!(rep.messages.texts().collect::<Vec<_>>(), ["1 rows had no usable coordinates"]);
        // Without the coordinate system, the points are said to be out of place.
        let (_, rep) = read_csv_with(text, "t", &CsvOptions { crs: None, ..opts.clone() }).unwrap();
        assert!(rep.messages.iter().any(|m| m.contains("\"crs\"")), "{:?}", rep.messages);
        assert!(read_csv_with(text, "t", &CsvOptions { x: Some("nope".into()), ..opts.clone() }).is_err());
        assert!(read_csv_with(text, "t", &CsvOptions { y: None, ..opts.clone() }).is_err(), "x without y");
        assert!(read_csv_with(text, "t", &CsvOptions { wkt: Some("id".into()), ..opts }).is_err(), "x/y and WKT");
        // A longitude without a latitude doesn't pair with a northing.
        let (ds, _) = read_csv("lon,northing\n1,2\n", "t").unwrap();
        assert_eq!(ds.geometry_type, GeometryType::None);
        // Easting/northing found by name in a tab-separated file.
        let (ds, _) = read_csv("name\teasting\tnorthing\nA\t500000\t4649776\n", "t").unwrap();
        assert_eq!(ds.geometry_type, GeometryType::Point);
        // A WKT column found by name: it is the geometry, not a field; EWKT's SRID places it.
        let text = "name,geometry\nA,\"SRID=32633;LINESTRING (500000 4649776, 500100 4649876)\"\nB,\"SRID=32633;LINESTRING Z (500000 4649776 3, 500200 4649776 4)\"\nC,POINT (1 2)\nD,\nE,nonsense(\n";
        let (ds, rep) = read_csv(text, "t").unwrap();
        assert_eq!(ds.geometry_type, GeometryType::Polyline);
        assert_eq!(ds.fields.len(), 1);
        let e = ds.in_lonlat().unwrap().features[0].geometry.as_ref().unwrap().extent();
        assert!((e.min_x - 15.0).abs() < 0.01 && (e.min_y - 42.0).abs() < 0.05, "{e:?}");
        assert!(ds.features[3].geometry.is_none());
        assert_eq!(
            rep.messages,
            [
                crate::loss::Note::loss(crate::loss::LossCode::DroppedZm, crate::zm_note(Some(1))),
                crate::loss::Note::loss(crate::loss::LossCode::DroppedGeometry, "1 rows had geometry that isn't readable WKT"),
                "1 rows have another kind of geometry than the layer's polyline".into()
            ]
        );
        // A named WKT column with a given system.
        let opts = CsvOptions { wkt: Some("g".into()), crs: Some(Crs::WebMercator), ..CsvOptions::default() };
        let (ds, _) =
            read_csv_with("g|v\nPOLYGON ((0 0, 1000 0, 1000 1000, 0 0))|1\n", "t", &CsvOptions { delimiter: Some(b'|'), ..opts }).unwrap();
        assert_eq!(ds.geometry_type, GeometryType::Polygon);
        assert_eq!(ds.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["v"]);
    }

    /// Counts affected input values once, even when loss happens at several nesting levels.
    #[test]
    fn wkt_collection_losses_are_said_without_changing_retained_data() {
        let values = [
            "GEOMETRYCOLLECTION(POINT(1 2),POINT(3 4),POINT(5 6))",
            "GEOMETRYCOLLECTION(GEOMETRYCOLLECTION(POINT(1 2),POINT(3 4)),POINT(5 6))",
            "GEOMETRYCOLLECTION(POINT EMPTY,GEOMETRYCOLLECTION(POINT(1 2),POINT EMPTY))",
            "GEOMETRYCOLLECTION EMPTY",
            "GEOMETRYCOLLECTION(POINT EMPTY,GEOMETRYCOLLECTION EMPTY)",
            "GEOMETRYCOLLECTION(POINT(1 2))",
        ];
        let mut csv = String::from("wkt,id\n");
        for (i, value) in values.iter().enumerate() {
            csv.push_str(&format!("\"SRID=4326;{value}\",{i}\n"));
        }
        let (ds, report) = read_csv(&csv, "collections").unwrap();
        assert_eq!(report.skipped, 0);
        assert_eq!(
            report.messages.texts().collect::<Vec<_>>(),
            ["2 WKT values in 'wkt' lost later non-empty geometry-collection members; only the first non-empty member is kept"]
        );
        assert_eq!(ds.held_in, crate::raster::Crs::LonLat);
        assert!(!ds.zm_dropped);
        for (i, row) in ds.features.iter().enumerate() {
            assert_eq!(row.attrs, [Value::Int(i as i64)]);
            assert_eq!(row.geometry, if matches!(i, 3 | 4) { None } else { Some(Geometry::Point(Coord::new(1.0, 2.0))) });
        }
        for value in &values[2..] {
            let (_, report) = read_csv(&format!("wkt,id\n\"{value}\",7\n"), "clean").unwrap();
            assert!(report.messages.is_empty(), "{value}: {report:?}");
        }
        let (ds, report) = read_csv("wkt,id\n\"SRID=4326;GEOMETRYCOLLECTION(POINT M(1 2 3),POINT Z(3 4 5))\",7\n", "dimensions").unwrap();
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(1.0, 2.0))));
        assert_eq!(ds.features[0].attrs, [Value::Int(7)]);
        assert!(ds.zm_dropped);
        assert_eq!(
            report.messages,
            [
                crate::loss::Note::loss(crate::loss::LossCode::DroppedZm, crate::zm_note(Some(1))),
                crate::loss::Note::loss(
                    crate::loss::LossCode::DroppedGeometry,
                    "1 WKT value in 'wkt' lost later non-empty geometry-collection members; only the first non-empty member is kept"
                )
            ]
        );
    }

    /// Z and M values RustGIS doesn't keep are said when read, whole or in place, and the mark
    /// that exports will lack them survives a project save; 2D data says nothing.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn dropped_z_and_m_values_are_said() {
        use crate::source::FeatureSource;
        use shapefile::dbase::{Record, TableWriterBuilder};
        let said = |r: &ReadReport| r.messages.iter().any(|m| m.contains("Z or M values"));
        let dir = tempfile::tempdir().unwrap();
        let shp = |name: &str, z: bool| {
            let path = dir.path().join(name);
            let mut w = shapefile::Writer::from_path(&path, TableWriterBuilder::new()).unwrap();
            if z {
                w.write_shape_and_record(&shapefile::PointZ::new(1.0, 2.0, 30.0, 0.0), &Record::default()).unwrap();
            } else {
                w.write_shape_and_record(&shapefile::Point::new(1.0, 2.0), &Record::default()).unwrap();
            }
            path
        };
        let (wells, flat) = (shp("wells.shp", true), shp("flat.shp", false));
        let (ds, rep) = read_path(&wells).unwrap();
        assert!(ds.zm_dropped && rep.messages.says(&crate::zm_note(Some(1))), "{:?}", rep.messages);
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(1.0, 2.0))));
        let (ds, rep) = read_path(&flat).unwrap();
        assert!(!ds.zm_dropped && !said(&rep));
        let src = crate::source::shp::ShpSource::open_path(&wells, "wells").unwrap();
        assert!(src.info().zm_dropped);
        let (ds, rep) = crate::source::materialize(&src).unwrap();
        assert!(ds.zm_dropped && rep.messages.iter().any(|m| m.ends_with(&crate::zm_note(Some(1)))), "{:?}", rep.messages);
        assert!(!crate::source::shp::ShpSource::open_path(&flat, "flat").unwrap().info().zm_dropped);
        // A source declaring Z or M but holding no features (an empty PointZ file) loses nothing.
        let mut empty = crate::source::shp::ShpSource::open_path(&flat, "flat").unwrap().info().clone();
        empty.drop_zm(Some(0));
        assert!(!empty.zm_dropped && !empty.notes.iter().any(|n| n.contains("Z or M")));
        // The same through a real zero-record PointZ file, built from the wells' headers (the
        // exporter refuses an empty layer).
        let none = dir.path().join("none.shp");
        for ext in ["shp", "shx"] {
            let mut header = std::fs::read(wells.with_extension(ext)).unwrap();
            header.truncate(100);
            header[24..28].copy_from_slice(&50i32.to_be_bytes());
            std::fs::write(none.with_extension(ext), header).unwrap();
        }
        let mut header = std::fs::read(wells.with_extension("dbf")).unwrap();
        let len = u16::from_le_bytes([header[8], header[9]]) as usize;
        header.truncate(len);
        header[4..8].copy_from_slice(&0u32.to_le_bytes());
        header.push(0x1a);
        std::fs::write(none.with_extension("dbf"), header).unwrap();
        assert!(!crate::source::shp::ShpSource::open_path(&none, "none").unwrap().info().zm_dropped);
        let (ds, rep) = read_path(&none).unwrap();
        assert!(!ds.zm_dropped && !said(&rep), "{:?}", rep.messages);

        // GeoJSON: a third position value, whole or in place, in a collection's member too.
        let doc = |pos: &str| {
            format!(
                r#"{{"type":"FeatureCollection","features":[{{"type":"Feature","properties":{{}},"geometry":{{"type":"GeometryCollection","geometries":[{{"type":"LineString","coordinates":[{pos},[3,4]]}}]}}}}]}}"#
            )
        };
        let (ds, rep) = read_geojson(&doc("[1,2,9]"), "track").unwrap();
        assert!(ds.zm_dropped && said(&rep));
        let (ds, rep) = read_geojson(&doc("[1,2]"), "track").unwrap();
        assert!(!ds.zm_dropped && !said(&rep));
        let path = dir.path().join("track.geojson");
        std::fs::write(&path, doc("[1,2,9]")).unwrap();
        assert!(crate::source::geojson::GeoJsonSource::open_path(&path, "track").unwrap().info().zm_dropped);
        // A height past the first position counts; one only in a geometry that isn't read doesn't.
        let line = r#"{"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[1,2],[3,4,5]]}}"#;
        assert!(read_geojson(line, "line").unwrap().0.zm_dropped);
        // A `null` third value isn't a height, but a measure after it counts.
        let null_z = r#"{"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[1,2,null],[3,4]]}}"#;
        assert!(!read_geojson(null_z, "line").unwrap().0.zm_dropped);
        let null_z_m = r#"{"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[1,2,null,7],[3,4]]}}"#;
        assert!(read_geojson(null_z_m, "line").unwrap().0.zm_dropped);
        // A stray `geometries` on another type isn't read, so its heights don't count.
        let stray = r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2],"geometries":[{"type":"Point","coordinates":[1,2,3]}]}}"#;
        assert!(!read_geojson(stray, "pt").unwrap().0.zm_dropped);
        // A rejected collection loses the whole feature, not just its heights.
        let mixed = r#"{"type":"Feature","properties":{},"geometry":{"type":"GeometryCollection","geometries":[{"type":"LineString","coordinates":[[1,2],[3,4]]},{"type":"Point","coordinates":[5,6,7]}]}}"#;
        let (ds, rep) = read_geojson(mixed, "mixed").unwrap();
        assert!(ds.features.is_empty() && !ds.zm_dropped && !said(&rep));
        assert_eq!(rep.skipped, 1);
        assert_eq!(rep.messages.texts().collect::<Vec<_>>(), ["1 features had unreadable geometry"]);
        let curve = r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Curve","coordinates":[[1,2,3]]}},{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#;
        let (ds, rep) = read_geojson(curve, "curve").unwrap();
        assert!(!ds.zm_dropped && !said(&rep) && rep.skipped == 1, "{rep:?}");
        std::fs::write(&path, curve).unwrap();
        assert!(!crate::source::geojson::GeoJsonSource::open_path(&path, "curve").unwrap().info().zm_dropped);

        // KML marks each kind's layer by its own altitudes: a 3D point beside a flat polygon.
        let kml = r#"<kml><Placemark><MultiGeometry><Point><coordinates>1,2,30</coordinates></Point><Polygon><outerBoundaryIs><LinearRing><coordinates>0,0 1,0 1,1 0,0</coordinates></LinearRing></outerBoundaryIs></Polygon></MultiGeometry></Placemark></kml>"#;
        let layers = crate::kml::read("site.kml", kml.as_bytes()).unwrap();
        let marked: Vec<bool> = layers.iter().map(|l| l.dataset.zm_dropped).collect();
        assert_eq!(marked, [true, false]);
        // A point keeps its first tuple, and only that tuple's altitude counts.
        let kml = r#"<kml><Placemark><Point><coordinates>1,2 3,4,30</coordinates></Point></Placemark></kml>"#;
        assert!(!crate::kml::read("pt.kml", kml.as_bytes()).unwrap()[0].dataset.zm_dropped);
        // A one-point line or track (left out) doesn't mark the line kept beside it.
        let kml = r#"<kml xmlns:gx="http://www.google.com/kml/ext/2.2"><Placemark><MultiGeometry><LineString><coordinates>1,2,30</coordinates></LineString><gx:Track><gx:coord>1 2 30</gx:coord></gx:Track><LineString><coordinates>0,0 1,1</coordinates></LineString></MultiGeometry></Placemark></kml>"#;
        let layers = crate::kml::read("site.kml", kml.as_bytes()).unwrap();
        assert!(!layers[0].dataset.zm_dropped);

        // A WKT column with Z.
        let (ds, rep) = read_csv("wkt,n\n\"POINT Z (1 2 3)\",1\n", "pts").unwrap();
        assert!(ds.zm_dropped && said(&rep));

        // Exporting several marked layers together says it once, counting and naming them.
        let mut other = ds.clone();
        other.name = "wells".into();
        let flat = Dataset::new("roads", GeometryType::Polyline, vec![]);
        assert_eq!(
            crate::zm_export_note([&ds, &flat, &other], "site.zip"),
            Some(crate::loss::Note::loss(
                crate::loss::LossCode::DroppedZm,
                "site.zip: its geometry has no Z or M values for 2 layers (pts, wells), which were dropped when they were read"
            ))
        );
        assert_eq!(crate::zm_export_note([&flat], "roads.zip"), None);

        // Saved and opened again, the mark stays; an unmarked dataset doesn't write it.
        let back: Dataset = serde_json::from_value(serde_json::to_value(&ds).unwrap()).unwrap();
        assert!(back.zm_dropped);
        assert!(serde_json::to_value(Dataset::new("x", GeometryType::Point, vec![])).unwrap().get("zm_dropped").is_none());
    }

    /// An export says when a layer's positions are approximate (its datum had no shift to WGS 84)
    /// unless it is written back in its own system, and when its coordinates aren't longitude/
    /// latitude; once per kind, counting layers. Clean layers give no notice.
    #[test]
    fn exports_say_when_positions_are_approximate_or_not_longitude_latitude() {
        let ext = |ds: &Dataset| ds.extent();
        // A CSV of projected coordinates without its "crs" is read as they are, with a notice.
        let (utm, rep) = read_csv("id,x,y\n1,500000,3800000\n", "utm").unwrap();
        assert!(rep.messages.iter().any(|m| m.contains("aren't longitude/latitude")));
        let (lonlat, rep) = read_csv("id,x,y\n1,-89.9,35.1\n", "lonlat").unwrap();
        assert!(rep.messages.is_empty(), "{:?}", rep.messages);
        assert_eq!(
            crate::export_notes([(&utm, ext(&utm)), (&lonlat, ext(&lonlat))], "out.geojson", Crs::LonLat),
            // Told by the extent alone: said without a code.
            [
                "out.geojson: the coordinates of utm aren't longitude/latitude, so they're out of place: define its projection and export again"
            ]
        );
        assert!(crate::export_notes([(&lonlat, ext(&lonlat))], "out.geojson", Crs::LonLat).is_empty());

        // A datum no one knows a shift of (NAD 27's ellipsoid under a local name; NAD 27 itself is
        // transformed by its name, as PROJ 9.8.1 does), as its WKT is kept with the layer and
        // saved with the project.
        let wkt = r#"PROJCS["NAD_1927_UTM_Zone_16N",GEOGCS["GCS_Local",DATUM["D_Local_Survey",SPHEROID["Clarke_1866",6378206.4,294.9786982]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-87.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
        let (nad27, note) = crate::raster::wkt::read_wkt(wkt).unwrap();
        assert!(note.is_some());
        let mut parcels = lonlat.clone();
        parcels.name = "parcels".into();
        parcels.crs = Some(nad27);
        let saved: Dataset = serde_json::from_value(serde_json::to_value(&parcels).unwrap()).unwrap();
        assert_eq!(
            crate::export_notes([(&saved, ext(&saved))], "parcels.geojson", Crs::LonLat),
            [crate::loss::Note::loss(
                crate::loss::LossCode::ApproximatedDatum,
                format!(
                    "parcels.geojson: the positions of parcels are approximate, possibly a few hundred metres off: its coordinate system ({}) has no shift to WGS 84",
                    nad27.label()
                )
            )]
        );
        // Written back in NAD 27, the positions come back as they were read.
        assert!(crate::export_notes([(&saved, ext(&saved))], "parcels.shp", nad27).is_empty());
        // Several layers: one notice, counting them; a layer with its shift says nothing.
        let mut roads = parcels.clone();
        roads.name = "roads".into();
        let mut shifted = lonlat.clone();
        shifted.crs = Some(Crs::Epsg(26716));
        assert_eq!(
            crate::export_notes([&parcels, &roads, &shifted].map(|ds| (ds, ext(ds))), "map.json", Crs::LonLat),
            [crate::loss::Note::loss(
                crate::loss::LossCode::ApproximatedDatum,
                "map.json: the positions of 2 layers (parcels, roads) are approximate, possibly a few hundred metres off: their coordinate systems have no shift to WGS 84"
            )]
        );
    }

    /// An export in Web Mercator writes a latitude beyond 85.05°, an infinite one too, at 85.05°,
    /// where Web Mercator ends, and says how many vertices it moved; for a streamed layer, its
    /// source's extent tells. Within 85.05°, or in another system, nothing is said of it.
    #[test]
    fn a_web_mercator_export_says_what_it_writes_at_85_degrees() {
        use rustgis_geom::{Extent, MAX_LAT, lonlat_to_merc, merc_to_lonlat};
        let c = Coord::new;
        let ext = |ds: &Dataset| ds.extent();
        let layer = |name: &str, points: &[Coord]| {
            let mut ds = Dataset::new(name, GeometryType::Point, vec![]);
            for p in points {
                ds.push(Some(Geometry::Point(*p)), vec![]);
            }
            ds
        };
        let polar = layer("polar", &[c(10.0, 89.0), c(20.0, -90.0), c(30.0, 60.0), c(40.0, f64::INFINITY)]);
        // What the export writes: each latitude beyond at 85.05°.
        let merc = project(&polar, Crs::WebMercator).unwrap();
        let written: Vec<Coord> = merc
            .features
            .iter()
            .filter_map(|f| match f.geometry {
                Some(Geometry::Point(p)) => Some(merc_to_lonlat(p)),
                _ => None,
            })
            .collect();
        assert_eq!(written.len(), 4);
        for (w, lat) in written.iter().zip([MAX_LAT, -MAX_LAT, 60.0, MAX_LAT]) {
            assert!((w.y - lat).abs() < 1e-9, "{w:?}");
        }
        assert_eq!(merc.features[0].geometry, Some(Geometry::Point(lonlat_to_merc(c(10.0, MAX_LAT)))));
        assert_eq!(
            crate::export_notes([(&polar, ext(&polar))], "polar.shp", Crs::WebMercator),
            [crate::loss::Note::loss(
                crate::loss::LossCode::ChangedPosition,
                "polar.shp: 3 vertices of polar are beyond 85.05° north or south, and written at 85.05°, where Web Mercator ends"
            )]
        );
        // In another system, or within 85.05°: nothing.
        for crs in [Crs::LonLat, Crs::Epsg(32631)] {
            assert!(crate::export_notes([(&polar, ext(&polar))], "polar.shp", crs).is_empty(), "{crs:?}");
        }
        let within = layer("within", &[c(30.0, 60.0), c(0.0, -MAX_LAT)]);
        assert!(crate::export_notes([(&within, ext(&within))], "within.shp", Crs::WebMercator).is_empty());
        // One vertex; and several layers in one notice, counting the vertices of all.
        let one = layer("one", &[c(0.0, 86.0)]);
        assert_eq!(
            crate::export_notes([(&one, ext(&one))], "one.gpkg", Crs::WebMercator),
            [crate::loss::Note::loss(
                crate::loss::LossCode::ChangedPosition,
                "one.gpkg: 1 vertex of one is beyond 85.05° north or south, and written at 85.05°, where Web Mercator ends"
            )]
        );
        assert_eq!(
            crate::export_notes([(&polar, ext(&polar)), (&within, ext(&within)), (&one, ext(&one))], "map.gpkg", Crs::WebMercator),
            [crate::loss::Note::loss(
                crate::loss::LossCode::ChangedPosition,
                "map.gpkg: 4 vertices of 2 layers (polar, one) are beyond 85.05° north or south, and written at 85.05°, where Web Mercator ends"
            )]
        );
        // A streamed layer's features are only those in view: its source's extent tells, without
        // a count, and only that it reaches beyond, as a source can declare a wider one than its
        // data's.
        let mut window = layer("tiles", &[c(0.0, 0.0)]);
        window.windowed = true;
        assert_eq!(
            crate::export_notes([(&window, Extent::new(-180.0, -90.0, 180.0, 84.0))], "tiles.gpkg", Crs::WebMercator),
            [
                "tiles.gpkg: the extent of tiles reaches beyond 85.05° north or south, and what lies beyond is written at 85.05°, where Web Mercator ends"
            ]
        );
        assert!(crate::export_notes([(&window, Extent::new(-180.0, -80.0, 180.0, 84.0))], "tiles.gpkg", Crs::WebMercator).is_empty());
        // A layer counted and a window told by its extent: one notice, saying both.
        assert_eq!(
            crate::export_notes([(&polar, ext(&polar)), (&window, Extent::new(-180.0, -90.0, 180.0, 84.0))], "map.gpkg", Crs::WebMercator),
            [crate::loss::Note::loss(
                crate::loss::LossCode::ChangedPosition,
                "map.gpkg: 3 vertices of polar are beyond 85.05° north or south, and written at 85.05°; the extent of tiles reaches beyond 85.05° north or south, and what lies beyond is written at 85.05°, where Web Mercator ends"
            )]
        );
        // A layer without features that isn't a window (a raster layer's, or a streamed layer's
        // written a chunk at a time, whose export counts as it writes) has no extent tell.
        let empty = Dataset::new("empty", GeometryType::Point, vec![]);
        assert!(crate::export_notes([(&empty, Extent::new(-180.0, -90.0, 180.0, 90.0))], "empty.gpkg", Crs::WebMercator).is_empty());
        // A Shapefile holds only geometry of the layer's kind, and of it only parts it can draw:
        // the vertices it leaves out aren't counted, though a GeoPackage holds them. Of the lines,
        // the 2 vertices of the first are written, not the one-vertex part or the point. Of the
        // polygons, the 4 of the first one's three-vertex hole, not its two-vertex one, nor the
        // second polygon, whose outer ring has 2 vertices, with its hole.
        let mut lines = Dataset::new("lines", GeometryType::Polyline, vec![]);
        lines.push(Some(Geometry::Line(vec![vec![c(0.0, 88.0), c(10.0, 88.0)]])), vec![]);
        lines.push(Some(Geometry::Line(vec![vec![c(0.0, 89.0)], vec![c(0.0, 0.0), c(1.0, 0.0)]])), vec![]);
        lines.push(Some(Geometry::Point(c(5.0, 89.0))), vec![]);
        let mut polys = Dataset::new("polys", GeometryType::Polygon, vec![]);
        let ring = |r: &[(f64, f64)]| r.iter().map(|&(x, y)| c(x, y)).collect::<Vec<_>>();
        polys.push(
            Some(Geometry::Polygon(vec![vec![
                ring(&[(0.0, 0.0), (0.0, 10.0), (10.0, 10.0), (0.0, 0.0)]),
                ring(&[(1.0, 86.0), (2.0, 86.0), (1.5, 87.0), (1.0, 86.0)]),
                ring(&[(3.0, 89.0), (3.0, 89.0)]),
            ]])),
            vec![],
        );
        polys.push(
            Some(Geometry::Polygon(vec![vec![
                ring(&[(0.0, 89.0), (1.0, 89.0)]),
                ring(&[(0.0, 86.0), (1.0, 86.0), (1.0, 87.0), (0.0, 86.0)]),
            ]])),
            vec![],
        );
        for (ds, in_shapefile, in_geopackage) in [(&lines, 2, 4), (&polys, 4, 12)] {
            let name = ds.name.as_str();
            for (target, n) in
                [(format!("{name}.shp"), in_shapefile), (format!("{name}.zip"), in_shapefile), (format!("{name}.gpkg"), in_geopackage)]
            {
                assert_eq!(
                    crate::export_notes([(ds, ext(ds))], &target, Crs::WebMercator),
                    [crate::loss::Note::loss(
                        crate::loss::LossCode::ChangedPosition,
                        format!(
                            "{target}: {n} vertices of {name} are beyond 85.05° north or south, and written at 85.05°, where Web Mercator ends"
                        )
                    )]
                );
            }
            // As many as the Shapefile written holds at 85.05°.
            let (zip, _) = write_bytes_in(ds, "shp", Crs::WebMercator).unwrap();
            let mut at_edge = 0;
            for (back, _) in read_zip(&format!("{name}.zip"), &zip).unwrap() {
                let back = back.in_lonlat().unwrap();
                for g in back.features.iter().filter_map(|f| f.geometry.as_ref()) {
                    g.for_each_coord(|c| at_edge += usize::from((c.y.abs() - MAX_LAT).abs() < 1e-6));
                }
            }
            assert_eq!(at_edge, in_shapefile, "{name}");
        }
    }

    #[test]
    fn wkt_geometries() {
        use crate::interop::parse_wkt;
        let p = |t: &str| parse_wkt(t).unwrap_or_else(|e| panic!("{t}: {e}")).0;
        assert_eq!(p("POINT (1 2)"), Some(Geometry::Point(Coord::new(1.0, 2.0))));
        assert_eq!(p("point z (1 2 3)"), Some(Geometry::Point(Coord::new(1.0, 2.0))));
        assert_eq!(p("POINTZM(1 2 3 4)"), Some(Geometry::Point(Coord::new(1.0, 2.0))));
        assert_eq!(p("MULTIPOINT ((1 2), (3 4))"), p("MULTIPOINT (1 2, 3 4)"));
        assert!(matches!(p("MULTILINESTRING ((0 0, 1 1), (2 2, 3 3))"), Some(Geometry::Line(l)) if l.len() == 2));
        assert!(
            matches!(p("MULTIPOLYGON (((0 0, 1 0, 1 1, 0 0)), ((5 5, 6 5, 6 6, 5 5), (5.1 5.1, 5.2 5.1, 5.2 5.2, 5.1 5.1)))"), Some(Geometry::Polygon(v)) if v.len() == 2 && v[1].len() == 2)
        );
        assert_eq!(p("GEOMETRYCOLLECTION (POINT EMPTY, POINT (7 8))"), Some(Geometry::Point(Coord::new(7.0, 8.0))));
        assert_eq!(p("POLYGON EMPTY"), None);
        assert_eq!(parse_wkt("SRID=4326;POINT (1 2)").unwrap().1, Some(4326));
        for bad in [
            "",
            "POINT",
            "POINT (1)",
            "POINT (1 2 3 4 5)",
            "POINT (1 2",
            "POINT (1 2) x",
            "CIRCLE (1 2)",
            "POINT (nan 2)",
            "SRID=x;POINT (1 2)",
            "POINT (1e999 2)",
        ] {
            assert!(parse_wkt(bad).is_err(), "{bad}");
        }
        let deep = format!("{}POINT (1 2){}", "GEOMETRYCOLLECTION (".repeat(50), ")".repeat(50));
        assert!(parse_wkt(&deep).is_err(), "nesting is bounded");
        assert_eq!(parse_wkt("SRID=4326 POINT (1 2)").unwrap_err(), "an SRID= without the ';' before its geometry");
        assert_eq!(crate::raster::Crs::parse("epsg:2263").unwrap().epsg(), 2263);
        assert_eq!(crate::raster::Crs::parse("http://www.opengis.net/def/crs/EPSG/0/3857").unwrap(), crate::raster::Crs::WebMercator);
        assert_eq!(crate::raster::Crs::parse("OGC:CRS84").unwrap(), crate::raster::Crs::LonLat);
        assert!(crate::raster::Crs::parse("EPSG:999999").is_err());
        assert!(crate::raster::Crs::parse("Mercator").is_err());
    }

    /// The columns Add Delimited Text offers, and the ones found as the reader finds them.
    #[test]
    fn delimited_text_columns() {
        let c = csv_columns("\u{feff}name;Lat;Lon\nA;1;2\n", None, None).unwrap();
        assert_eq!(c.names, ["name", "Lat", "Lon"]);
        assert_eq!((c.x.as_deref(), c.y.as_deref(), c.wkt.as_deref(), c.delimiter), (Some("Lon"), Some("Lat"), None, Some(b';')));
        // Longitude/latitude over easting/northing; easting/northing as a pair only.
        let c = csv_columns("easting,northing,lon,lat\n", None, None).unwrap();
        assert_eq!((c.x.as_deref(), c.y.as_deref()), (Some("lon"), Some("lat")));
        let c = csv_columns("easting,lat\n1,2\n", None, None).unwrap();
        assert_eq!((c.x, c.y), (None, None));
        // A WKT column by its name and first value, not by its name alone.
        let c = csv_columns("id,geometry\n1,\n2,POINT (1 2)\n", None, None).unwrap();
        assert_eq!(c.wkt.as_deref(), Some("geometry"));
        assert_eq!(csv_columns("id,geometry\n1,not wkt\n", None, None).unwrap().wkt, None);
        // The separator given splits the header.
        let c = csv_columns("a;b,c\n", Some(b','), None).unwrap();
        assert_eq!((c.names, c.delimiter), (vec!["a;b".to_string(), "c".to_string()], Some(b',')));
        // Nothing to read is no columns, not a crash.
        assert!(csv_columns("", None, None).unwrap().names.is_empty());
        let _ = csv_columns("\"unclosed\n", None, None);
    }

    /// CSV that isn't UTF-8 (#1303): Latin-1, as many public sources publish it, and UTF-16 with
    /// its byte-order mark are read, with a note; bytes with a NUL aren't text.
    #[test]
    fn csv_in_latin1_or_utf16() {
        let latin1 = b"nome;lat;lon\nS\xe3o Jos\xe9;-5.1;-42.8\n";
        let (ds, rep) = read_bytes("cnes.csv", latin1).unwrap();
        assert_eq!(ds.value(0, 0), &Value::Text("São José".into()));
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(-42.8, -5.1))));
        assert!(rep.messages.iter().any(|m| m.contains("cnes.csv") && m.contains("Windows-1252")), "{:?}", rep.messages);
        // Inside a zip too, as public sources ship it.
        let zipped = crate::zip::write(&[("cnes.csv".to_string(), latin1.to_vec())]).unwrap();
        assert_eq!(read_bytes("cnes.zip", &zipped).unwrap().0.value(0, 0), &Value::Text("São José".into()));
        // UTF-16 little-endian with its mark.
        let utf16: Vec<u8> = [0xff, 0xfe].into_iter().chain("nome,n\nJosé,1\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        let (ds, rep) = read_bytes("t.csv", &utf16).unwrap();
        assert_eq!(ds.value(0, 0), &Value::Text("José".into()));
        assert!(rep.messages.iter().any(|m| m.contains("UTF-16") && !m.contains("replacement")), "{:?}", rep.messages);
        // An odd last byte isn't UTF-16: replaced, and said.
        let mut odd = utf16.clone();
        odd.push(b'x');
        let (_, rep) = read_bytes("t.csv", &odd).unwrap();
        assert!(rep.messages.iter().any(|m| m.contains("UTF-16") && m.contains("replacement")), "{:?}", rep.messages);
        // A UTF-8 byte-order mark before a Latin-1 byte doesn't end up in the first header.
        let (ds, _) = read_bytes("t.csv", b"\xef\xbb\xbfnome;n\nS\xe3o;1\n").unwrap();
        assert_eq!((ds.fields[0].name.as_str(), ds.value(0, 0)), ("nome", &Value::Text("São".into())));
        // UTF-8 says nothing; binary data named .csv is refused.
        assert!(read_bytes("t.csv", "nome,n\nJosé,1\n".as_bytes()).unwrap().1.messages.is_empty());
        assert!(read_bytes("t.csv", b"a,b\n\x00\xff\x01,2\n").is_err());
    }

    /// A GeoJSON column of text and numbers is read as text, its numbers in full rather than
    /// rounded as they're shown.
    #[test]
    fn a_mixed_geojson_column_keeps_its_numbers_in_full() {
        let text = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","properties":{"v":"a"},"geometry":{"type":"Point","coordinates":[1,2]}},
            {"type":"Feature","properties":{"v":0.1234567891},"geometry":{"type":"Point","coordinates":[3,4]}}]}"#;
        let (ds, _) = read_geojson(text, "t").unwrap();
        assert_eq!(ds.fields[0].ty, FieldType::Text);
        assert_eq!(ds.value(1, 0), &Value::Text("0.1234567891".into()));
    }

    #[test]
    fn csv_points() {
        let (ds, rep) = read_csv("name,Lat,Lon,n\nA,10,20,1\nB,bad,1,2\n", "t").unwrap();
        assert_eq!(ds.geometry_type, GeometryType::Point);
        assert_eq!(rep.skipped, 1);
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(20.0, 10.0))));
        assert!(to_csv(&ds).unwrap().contains("longitude"));
    }

    /// Projected X/Y must not hide geographic columns; explicit columns and systems still win.
    #[test]
    fn csv_coordinate_choice() {
        for (lon, lat) in [
            ("Longitude", "Latitude"),
            ("lon_dd", "lat_dd"),
            ("stop_lon", "stop_lat"),
            ("Longitutde", "Lattitude"),
            ("Longitude Decimal Degrees", "Latitude Decimal Degrees"),
        ] {
            let text = format!("X,Y,{lon},{lat}\n500000,3800000,-89.9,35.1\n");
            let (ds, report) = read_csv(&text, "mixed").unwrap();
            assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(-89.9, 35.1))), "{lon}/{lat}");
            assert!(report.messages.is_empty(), "{:?}", report.messages);
            let c = csv_columns(&text, None, None).unwrap();
            assert_eq!((c.x.as_deref(), c.y.as_deref()), (Some(lon), Some(lat)));
        }
        let text = "X,Y,Longitude,Latitude\n1000,2000,-89.9,35.1\n";
        let opts = CsvOptions { crs: Some(Crs::WebMercator), ..Default::default() };
        let (ds, report) = read_csv_with(text, "projected", &opts).unwrap();
        let Some(Geometry::Point(p)) = ds.in_lonlat().unwrap().features[0].geometry else { panic!("point") };
        assert!((p.x - 0.00898315).abs() < 1e-7 && (p.y - 0.0179663).abs() < 1e-7);
        assert!(report.messages.is_empty());
        for crs in [Crs::WebMercator, Crs::parse("EPSG:2263").unwrap(), Crs::parse("+proj=merc +datum=WGS84 +units=m").unwrap()] {
            let c = csv_columns(text, None, Some(crs)).unwrap();
            assert_eq!((c.x.as_deref(), c.y.as_deref()), (Some("X"), Some("Y")));
        }
        for crs in [Crs::LonLat, Crs::parse("+proj=latlong +datum=WGS84 +no_defs").unwrap()] {
            let c = csv_columns(text, None, Some(crs)).unwrap();
            assert_eq!((c.x.as_deref(), c.y.as_deref()), (Some("Longitude"), Some("Latitude")));
            let opts = CsvOptions { crs: Some(crs), ..Default::default() };
            let (ds, report) = read_csv_with(text, "geographic", &opts).unwrap();
            let Some(Geometry::Point(p)) = ds.features[0].geometry else { panic!("point") };
            assert!((p.x + 89.9).abs() < 1e-10 && (p.y - 35.1).abs() < 1e-10);
            assert_eq!(report.skipped, 0);
            assert!(report.messages.is_empty(), "{:?}", report.messages);
        }
        let opts = CsvOptions { x: Some("X".into()), y: Some("Y".into()), ..Default::default() };
        assert_eq!(
            read_csv_with(text, "explicit", &opts).unwrap().0.features[0].geometry,
            Some(Geometry::Point(Coord::new(1000.0, 2000.0)))
        );
        // An unusable geographic alternative does not replace the generic pair.
        for (text, point) in [
            ("X,Y,Longitude,Latitude\n1000,2000,bad,\n", Coord::new(1000.0, 2000.0)),
            ("X,Y,Longitude,Latitude\n1000,2000,0,0\n", Coord::new(1000.0, 2000.0)),
        ] {
            let c = csv_columns(text, None, None).unwrap();
            assert_eq!((c.x.as_deref(), c.y.as_deref()), (Some("X"), Some("Y")));
            let (ds, _) = read_csv(text, "keep xy").unwrap();
            assert_eq!(ds.features[0].geometry, Some(Geometry::Point(point)));
        }
        // A named GTFS pair works alone, but a lone longitude never becomes geometry.
        assert_eq!(read_csv("stop_lat,stop_lon\n35.1,-89.9\n", "stops").unwrap().0.geometry_type, GeometryType::Point);
        assert_eq!(read_csv("stop_lon,stop_name\n-89.9,Main\n", "stops").unwrap().0.geometry_type, GeometryType::None);
    }

    /// An incomplete pair opens as a table, while a complete published pair places the point.
    #[test]
    fn csv_automatic_coordinates_require_a_complete_pair() {
        for text in ["X,Latitude\n570000,44.95\n", "Longitude,Y\n-93.10,160000\n"] {
            let (dataset, report) = read_csv(text, "incomplete").unwrap();
            assert_eq!(dataset.geometry_type, GeometryType::None);
            assert_eq!(dataset.features.len(), 1);
            let (header, _) = text.split_once('\n').unwrap();
            let (x, y) = header.split_once(',').unwrap();
            assert_eq!(report.messages.len(), 1);
            assert!(report.messages[0].contains(&format!("{x}, {y}")));
            assert!(report.messages[0].contains("Choose x and y"));
            let columns = csv_columns(text, None, None).unwrap();
            assert_eq!((columns.x, columns.y), (None, None));
        }

        for text in
            ["id,PROPX,PROPY,Latitude,Longtitude\n1,570000,160000,44.95,-93.10\n", "X,Latitude,Longtitude,Y\n570000,44.95,-93.10,160000\n"]
        {
            let (dataset, report) = read_csv(text, "permits").unwrap();
            assert_eq!(dataset.features[0].geometry, Some(Geometry::Point(Coord::new(-93.10, 44.95))));
            assert!(report.messages.is_empty(), "{:?}", report.messages);
            let columns = csv_columns(text, None, None).unwrap();
            assert_eq!((columns.x.as_deref(), columns.y.as_deref()), (Some("Longtitude"), Some("Latitude")));
        }
    }

    /// Longitude and latitude named in other languages and scripts place the points, as the
    /// English names do, with no notice (#2385): Taiwan's open data as data.gov.tw publishes it,
    /// with a note in brackets after the name, and each language's own pair.
    #[test]
    fn csv_coordinates_named_in_other_languages_place_the_points() {
        let taiwan = [
            ("經度", "緯度"),
            ("座標經度", "座標緯度"),
            ("經度(X)", "緯度(Y)"),
            ("经度", "纬度"),
            ("経度", "緯度"),
            ("경도", "위도"),
            ("Kinh độ", "Vĩ độ"),
            ("ДОЛГОТА", "ШИРОТА"),
            ("Längengrad", "Breitengrad"),
            ("zeměpisná_délka", "zeměpisná_šířka"),
            ("Longitude (WGS84)", "Latitude (WGS84)"),
            ("經度（度）", "緯度（度）"),
            ("lon [deg]", "lat [deg]"),
            ("Longitud", "Latitud"),
            ("東經", "北緯"),
            ("Laengengrad", "Breitengrad"),
        ];
        // Every name of either list, with the other axis in English.
        let each = LON_WORLD
            .iter()
            .filter(|x| !AMBIGUOUS_LON.iter().any(|(w, _)| w == *x))
            .map(|x| (x.to_string(), "latitude".to_string()))
            .chain(LAT_WORLD.iter().map(|y| ("longitude".to_string(), y.to_string())));
        for (x, y) in taiwan.iter().map(|(x, y)| (x.to_string(), y.to_string())).chain(each) {
            // Latitude first, as the Kaohsiung camera list has it, and a column besides.
            let text = format!("名稱,{y},{x}\nA,22.99,120.21\n");
            let (ds, report) = read_csv(&text, "places").unwrap();
            assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(120.21, 22.99))), "{x}, {y}");
            assert!(report.messages.is_empty(), "{x}, {y}: {:?}", report.messages);
            let columns = csv_columns(&text, None, None).unwrap();
            assert_eq!((columns.x.as_deref(), columns.y.as_deref()), (Some(x.as_str()), Some(y.as_str())));
        }
    }

    /// Words that are also an object's length or width aren't coordinates on their own, and an
    /// English pair wins over another language's, so a table of measurements stays a table and
    /// exports read back as written.
    #[test]
    fn csv_lengths_and_widths_stay_a_table() {
        for text in [
            "id,Länge,Breite\n1,3,4\n",
            "id,Longitud,Ancho\n1,3,4\n",
            "id,μήκος,πλάτος\n1,3,4\n",
            "id,الطول,العرض\n1,3,4\n",
            "id,經度\n1,120\n",
        ] {
            let (ds, report) = read_csv(text, "measures").unwrap();
            assert_eq!(ds.geometry_type, GeometryType::None, "{text}");
            assert!(report.messages.is_empty(), "{text}: {:?}", report.messages);
        }
        // English names first: a point layer with fields 經度 and 緯度 reads back from its own
        // longitude and latitude columns.
        let text = "經度,緯度,longitude,latitude\n1,2,120.21,22.99\n";
        let (ds, _) = read_csv(text, "both").unwrap();
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(120.21, 22.99))));
        // Spanish `longitud` (also length) only beside `latitud`, and only as written; on its
        // own it isn't named in a notice either.
        for text in ["Tramo,Lat,Longitud\nA,22.99,3\n", "Tramo,Latitud,Longitud (km)\nA,22.99,3\n"] {
            let (ds, report) = read_csv(text, "tramos").unwrap();
            assert_eq!(ds.geometry_type, GeometryType::None, "{text}");
            assert!(report.messages.is_empty(), "{text}: {:?}", report.messages);
        }
        // Beside `latitud` (with a note, or with another latitude before it), it pairs with it.
        for text in [
            "Longitud,Latitud (WGS84)\n121.5,25\n",
            "Lat,Longitud,Latitud\n1,121.5,25\n",
            "Longitud,Latitud (DMS),Latitud\n121.5,\"25°N\",25\n",
        ] {
            let (ds, report) = read_csv(text, "pares").unwrap();
            assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(121.5, 25.0))), "{text}");
            assert!(report.messages.is_empty(), "{text}: {:?}", report.messages);
        }
        // Another language's name as written wins over one with a note, as English names do.
        let text = "經度(DMS),緯度(DMS),經度,緯度\n\"E121°30'\",\"N25°\",121.5,25.0\n";
        let columns = csv_columns(text, None, None).unwrap();
        assert_eq!((columns.x.as_deref(), columns.y.as_deref()), (Some("經度"), Some("緯度")));
        // A name saved with its accents as separate marks (NFD) reads as one typed with them.
        use unicode_normalization::UnicodeNormalization;
        let (x, y): (String, String) = ("Kinh độ".nfd().collect(), "Vĩ độ".nfd().collect());
        assert_ne!(x, "Kinh độ");
        let (ds, _) = read_csv(&format!("{x},{y}\n105.8,21.0\n"), "hanoi").unwrap();
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(105.8, 21.0))));
        // A name as written wins over one with a note set aside, wherever it stands: decimal
        // degrees, not the DMS text before them.
        let text = "Longitude (DMS),Latitude (DMS),Longitude,Latitude\n\"121°30'E\",\"25°N\",121.5,25.0\n";
        let columns = csv_columns(text, None, None).unwrap();
        assert_eq!((columns.x.as_deref(), columns.y.as_deref()), (Some("Longitude"), Some("Latitude")));
        // Half a pair in another language beside an X names both in the notice.
        let (ds, report) = read_csv("X,緯度\n570000,22.99\n", "half").unwrap();
        assert_eq!(ds.geometry_type, GeometryType::None);
        assert!(report.messages.first().is_some_and(|m| m.contains("X, 緯度")), "{:?}", report.messages);
    }

    /// A plain table with only one candidate axis opens without a coordinate notice.
    #[test]
    fn csv_lone_coordinate_like_headers_do_not_warn() {
        for text in ["id,name\n1,Main\n", "id,Y\n1,2\n", "id,Lat\n1,2\n", "id,X,Longitude\n1,2,3\n"] {
            let (dataset, report) = read_csv(text, "table").unwrap();
            assert_eq!(dataset.geometry_type, GeometryType::None);
            assert!(report.messages.is_empty(), "{text}: {:?}", report.messages);
        }
    }

    /// A projected system must never treat an earlier geographic pair as easting/northing when
    /// planar coordinate columns are also present. Explicit choices retain their authority.
    #[test]
    fn projected_csv_prefers_planar_columns_in_either_header_order() {
        for (x, y) in [("X", "Y"), ("Easting", "Northing"), ("point_x", "point_y"), ("x_coord", "y_coord")] {
            for header_first in [true, false] {
                let text = if header_first {
                    format!("Longitude,Latitude,{x},{y}\n40,50,111319.49079327357,222684.20850554405\n")
                } else {
                    format!("{x},{y},Longitude,Latitude\n111319.49079327357,222684.20850554405,40,50\n")
                };
                let columns = csv_columns(&text, None, Some(Crs::WebMercator)).unwrap();
                assert_eq!((columns.x.as_deref(), columns.y.as_deref()), (Some(x), Some(y)));
                let opts = CsvOptions { crs: Some(Crs::WebMercator), ..Default::default() };
                let (dataset, report) = read_csv_with(&text, "projected", &opts).unwrap();
                let Some(Geometry::Point(point)) = dataset.in_lonlat().unwrap().features[0].geometry else { panic!("point") };
                assert!((point.x - 1.0).abs() < 1e-10 && (point.y - 2.0).abs() < 1e-10, "{point:?}");
                assert_eq!(report.skipped, 0);
                assert!(report.messages.is_empty(), "{:?}", report.messages);
                let opts = CsvOptions { x: Some("Longitude".into()), y: Some("Latitude".into()), ..opts };
                let (explicit, _) = read_csv_with(&text, "explicit", &opts).unwrap();
                let Some(Geometry::Point(point)) = explicit.in_lonlat().unwrap().features[0].geometry else { panic!("point") };
                assert!(point.x.abs() < 0.001 && point.y.abs() < 0.001, "explicit selection must win: {point:?}");
            }
        }
        // A geographic system still chooses the geographic family; a projected file without
        // planar names retains its existing longitude/latitude-name fallback.
        let text = "Longitude,Latitude,Easting,Northing\n40,50,111319.49079327357,222684.20850554405\n";
        let columns = csv_columns(text, None, Some(Crs::LonLat)).unwrap();
        assert_eq!((columns.x.as_deref(), columns.y.as_deref()), (Some("Longitude"), Some("Latitude")));
        let columns = csv_columns("Longitude,Latitude\n1000,2000\n", None, Some(Crs::WebMercator)).unwrap();
        assert_eq!((columns.x.as_deref(), columns.y.as_deref()), (Some("Longitude"), Some("Latitude")));
    }

    #[test]
    fn geographic_csv_prefers_specific_names_with_in_range_generic_xy() {
        for text in ["X,Y,Longitude,Latitude\n1,2,40,50\n", "Longitude,Latitude,X,Y\n40,50,1,2\n"] {
            let columns = csv_columns(text, None, Some(Crs::LonLat)).unwrap();
            assert_eq!((columns.x.as_deref(), columns.y.as_deref()), (Some("Longitude"), Some("Latitude")));
            let (ds, _) = read_csv_with(text, "geographic", &CsvOptions::default()).unwrap();
            assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(40.0, 50.0))));
            let opts = CsvOptions { x: Some("X".into()), y: Some("Y".into()), ..Default::default() };
            let (ds, _) = read_csv_with(text, "explicit", &opts).unwrap();
            assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(1.0, 2.0))));
        }
    }

    /// Both pairs compete by their coverage of usable degree positions.
    #[test]
    fn csv_coordinate_coverage() {
        for (rows, geographic) in [
            ("10,20,3,4\n11,21,,\n", false),
            ("10,20,3,4\n11,21,0,0\n", false),
            ("10,20,3,4\n11,21,181,20\n", false),
            ("10,20,3,4\n11,21,5,6\n", true),
            ("10,20,3,4\n,,5,6\n", true),
        ] {
            let text = format!("X,Y,Longitude,Latitude\n{rows}");
            let c = csv_columns(&text, None, None).unwrap();
            let expected = if geographic { ("Longitude", "Latitude") } else { ("X", "Y") };
            assert_eq!((c.x.as_deref(), c.y.as_deref()), (Some(expected.0), Some(expected.1)), "{rows}");
            let (ds, report) = read_csv(&text, "coverage").unwrap();
            assert_eq!(ds.features.len(), 2);
            assert_eq!(report.skipped, 0);
            assert_eq!(
                ds.features[0].geometry,
                Some(Geometry::Point(if geographic { Coord::new(3.0, 4.0) } else { Coord::new(10.0, 20.0) }))
            );
        }
    }

    /// A sparse geographic pair gives way to complete X/Y columns in either header order.
    #[test]
    fn csv_coordinate_coverage_is_independent_of_header_order() {
        for text in ["Longitude,Latitude,X,Y\n3,4,10,20\n,6,11,21\n", "X,Y,Longitude,Latitude\n10,20,3,4\n11,21,,6\n"] {
            let columns = csv_columns(text, None, None).unwrap();
            assert_eq!((columns.x.as_deref(), columns.y.as_deref()), (Some("X"), Some("Y")), "{text}");
            let (dataset, report) = read_csv(text, "coverage").unwrap();
            let points: Vec<_> = dataset.features.into_iter().map(|feature| feature.geometry).collect();
            assert_eq!(points, [Some(Geometry::Point(Coord::new(10.0, 20.0))), Some(Geometry::Point(Coord::new(11.0, 21.0)))], "{text}");
            assert_eq!(report.skipped, 0);
            assert!(report.messages.is_empty(), "{:?}", report.messages);
        }
    }

    /// A complete named pair beats an unrelated lone X or an earlier misspelled heading.
    #[test]
    fn csv_complete_pairs_prefer_recognized_headers() {
        for (text, x, y, point) in [
            ("X,Northing,Easting\nsite-a,44,20\n", "Easting", "Northing", Coord::new(20.0, 44.0)),
            ("Longtitude,Longitude,Latitude\nbad,-93.1,44.95\n", "Longitude", "Latitude", Coord::new(-93.1, 44.95)),
        ] {
            let columns = csv_columns(text, None, None).unwrap();
            assert_eq!((columns.x.as_deref(), columns.y.as_deref()), (Some(x), Some(y)));
            let (ds, report) = read_csv(text, "points").unwrap();
            assert_eq!(ds.features[0].geometry, Some(Geometry::Point(point)));
            assert!(report.messages.is_empty());
        }
    }

    /// Finite projected values cannot replace usable longitude/latitude without a projected CRS.
    #[test]
    fn csv_coordinate_coverage_ignores_out_of_range_planar_values() {
        let text = "Longitude,Latitude,Easting,Northing\n-93.10,44.95,570000,160000\n,,570010,160010\n";
        for crs in [None, Some(Crs::LonLat)] {
            let columns = csv_columns(text, None, crs).unwrap();
            assert_eq!((columns.x.as_deref(), columns.y.as_deref()), (Some("Longitude"), Some("Latitude")));
        }
        let (dataset, _) = read_csv(text, "coverage").unwrap();
        assert_eq!(dataset.features[0].geometry, Some(Geometry::Point(Coord::new(-93.1, 44.95))));
        assert_eq!(dataset.features[1].geometry, None);
    }

    /// Count suspect degree positions, retaining them; one zero axis or a nearby point is clean.
    #[test]
    fn csv_coordinate_notices() {
        let (ds, report) = read_csv("lon,lat\n0,0\n-0,0\n181,20\n20,-91\n", "suspect").unwrap();
        assert_eq!(ds.features.len(), 4);
        assert_eq!(report.skipped, 0);
        assert!(report.messages.iter().any(|m| m.contains("2 rows") && m.contains("0,0")), "{:?}", report.messages);
        assert!(report.messages.iter().any(|m| m.contains("2 rows") && m.contains("aren't longitude/latitude")), "{:?}", report.messages);
        let (_, report) = read_csv("lon,lat\n0,1\n1,0\n0.000001,0\n180,90\n-180,-90\n", "clean").unwrap();
        assert!(report.messages.is_empty(), "{:?}", report.messages);
    }

    /// Default import, explicit dialog picks, byte uploads, paths and gzip share the same rules.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn csv_coordinate_import_paths() {
        let dir = tempfile::tempdir().unwrap();
        for text in ["X,Y,Longitude,Latitude\n1000,2000,-89.9,35.1\n,,0,0\n", "X,Y,Longitude,Latitude\n1000,2000,-89.9,35.1\n"] {
            let columns = csv_columns(text, None, None).unwrap();
            let dialog = CsvOptions { x: columns.x, y: columns.y, ..Default::default() };
            for name in ["points.csv", "points.txt", "points.dat", "points.csv.gz", "points.dat.gz"] {
                let bytes = if name.ends_with(".gz") { gzip_csv(text.as_bytes()) } else { text.as_bytes().to_vec() };
                let path = dir.path().join(name);
                std::fs::write(&path, &bytes).unwrap();
                for (ds, report) in
                    [read_bytes(name, &bytes).unwrap(), read_path(&path).unwrap(), read_delimited_bytes(name, &bytes, &dialog).unwrap()]
                {
                    assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(-89.9, 35.1))), "{name}");
                    if ds.features.len() == 2 {
                        assert_eq!(report.messages.len(), 1, "{name}: {:?}", report.messages);
                        assert!(report.messages[0].contains("1 row was placed at 0,0"));
                    } else {
                        assert!(report.messages.is_empty(), "{name}: {:?}", report.messages);
                    }
                }
            }
        }
    }

    /// A layer that declares no geometry type gets no coordinate columns, so geometry it holds
    /// is counted as not written, as for a line or polygon layer; a point layer's points aren't.
    #[test]
    fn csv_losses_count_geometry_a_layer_without_a_type_holds() {
        let square =
            Geometry::Polygon(vec![vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(1.0, 1.0), Coord::new(0.0, 0.0)]]]);
        let mut ds = Dataset::new("t", GeometryType::None, vec![]);
        ds.push(Some(square), vec![]);
        ds.push(None, vec![]);
        let losses = CsvLosses::of(&ds.features, ds.geometry_type, &ds.fields).unwrap();
        assert_eq!(losses, CsvLosses { unwritten: 1, ..CsvLosses::default() });
        assert!(losses.notes(&ds.fields, "t.csv")[0].contains("the geometry of 1 feature wasn't written"));
        assert!(!to_csv(&ds).unwrap().contains("longitude"));
        let points = [crate::Feature { id: 0, geometry: Some(Geometry::Point(Coord::new(1.0, 2.0))), attrs: vec![] }];
        assert_eq!(CsvLosses::of(&points, GeometryType::Point, &[]).unwrap(), CsvLosses { points: true, ..CsvLosses::default() });
        // A multipoint without a point in a point layer gets empty coordinate cells.
        let empty = [crate::Feature { id: 1, geometry: Some(Geometry::MultiPoint(vec![])), attrs: vec![] }];
        assert_eq!(
            CsvLosses::of(&empty, GeometryType::Point, &[]).unwrap(),
            CsvLosses { unwritten: 1, points: true, unusable_points: 1, ..CsvLosses::default() }
        );
    }

    /// GPX leaves out a feature without geometry and a multipoint without a point (no waypoint
    /// for it), and counts both; a point is written.
    #[test]
    fn gpx_counts_a_multipoint_without_points_as_left_out() {
        let mut ds = Dataset::new("t", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![]);
        ds.push(Some(Geometry::MultiPoint(vec![])), vec![]);
        ds.push(None, vec![]);
        assert_eq!(gpx_left_out(&ds), 2);
        assert_eq!(write_bytes(&ds, "gpx").unwrap().1, 2);
    }

    /// A text layer for [`TextLosses`]: point features with `rows` of text values, and one without
    /// geometry holding blanks and padded text, which KML, GPX and Shapefile don't write.
    fn text_layer(names: &[&str], rows: &[&[Option<&str>]]) -> Dataset {
        let fields = names.iter().map(|n| Field::new(*n, FieldType::Text)).collect();
        let mut ds = Dataset::new("t", GeometryType::Point, fields);
        for (i, row) in rows.iter().enumerate() {
            let attrs = row.iter().map(|v| v.map_or(Value::Null, |t| Value::Text(t.to_string()))).collect();
            ds.push(Some(Geometry::Point(Coord::new(i as f64, 1.0))), attrs);
        }
        ds.push(None, names.iter().map(|_| Value::Text(" ".into())).collect());
        ds
    }

    fn text_notes(ds: &Dataset, ext: &str) -> Vec<String> {
        TextLosses::of(&ds.features, &ds.fields, ds.geometry_type, ext, name_field(ds)).notes(&ds.fields, "t")
    }

    #[test]
    fn csv_says_blank_text_that_reads_as_null() {
        // 'code' reads as numbers, so its blank of spaces reads as null; 'note' stays text, which
        // keeps spaces.
        let ds = text_layer(&["note", "code"], &[&[Some(""), Some("1")], &[Some("  "), Some(" ")], &[Some(" x "), Some("")]]);
        // The feature without geometry is a CSV row: its blank in 'code' counts too.
        assert_eq!(text_notes(&ds, "csv"), ["t: 4 blank text values read back as empty (null), in 'note', 'code'"]);
        assert_eq!(text_notes(&ds, "wkt"), ["t: 4 blank text values read back as empty (null), in 'note', 'code'"]);
        let mut clean = text_layer(&["note", "code"], &[&[Some(" x "), Some("1")], &[Some("  "), None]]);
        // Without the row of spaces, which 'code' reads as null.
        clean.features.truncate(2);
        assert!(text_notes(&clean, "csv").is_empty());
    }

    #[test]
    fn shapefile_says_trailing_spaces_and_blank_text() {
        let ds = text_layer(&["a", "b"], &[&[Some("lead "), Some(" kept")], &[Some("   "), Some("")], &[None, Some("x  ")]]);
        assert_eq!(
            text_notes(&ds, "shp"),
            [
                "t: 2 text values end with spaces, which reading the file back drops, in 'a', 'b'",
                "t: 2 blank text values read back as empty (null), in 'a', 'b'",
            ]
        );
        assert_eq!(text_notes(&ds, "zip"), text_notes(&ds, "shp"));
        let clean = text_layer(&["a"], &[&[Some(" kept")], &[None]]);
        assert!(text_notes(&clean, "shp").is_empty());
    }

    /// Shapefile text with a NUL in it is said: reading the `.dbf` back keeps only the text before
    /// it, and reads a value starting with one as null (#2415).
    #[test]
    fn shapefile_says_text_cut_at_a_nul() {
        // The text before a NUL loses its trailing spaces as other text does ('ab '), and blank
        // text before one (' ') reads as null, which the NUL's notice says.
        let ds = text_layer(&["a", "b"], &[&[Some("ab\0cd"), Some("kept")], &[Some("\0x"), None], &[Some("ab \0cd"), Some(" \0y")]]);
        assert_eq!(
            text_notes(&ds, "shp"),
            [
                "t: 1 text value ends with spaces, which reading the file back drops, in 'a'",
                "t: 4 text values hold a NUL character, and reading the file back keeps only the text before it, read as empty (null) when that is blank, in 'a', 'b'",
            ]
        );
        // What the note says is what reading the file back does.
        let (bytes, _) = write_bytes(&ds, "zip").unwrap();
        let back = read_bytes("t.zip", &bytes).unwrap().0;
        let column = |k: usize| back.features.iter().take(3).map(|f| f.attrs[k].clone()).collect::<Vec<_>>();
        assert_eq!(column(0), [Value::Text("ab".into()), Value::Null, Value::Text("ab".into())]);
        assert_eq!(column(1), [Value::Text("kept".into()), Value::Null, Value::Null]);
        let clean = text_layer(&["a"], &[&[Some("abcd")], &[None]]);
        assert!(text_notes(&clean, "shp").is_empty());
    }

    /// CSV text starting with an apostrophe before a formula's first character is said: reading
    /// the file back takes the apostrophe for the formula guard and drops it (#2415). Text the
    /// writer guards (`=y`) reads back as it was, and says nothing.
    #[test]
    fn csv_says_an_apostrophe_reading_takes_for_the_guard() {
        let ds = text_layer(&["note", "code"], &[&[Some("'=x"), Some("a")], &[Some("'kept"), Some("'-5")], &[Some("=y"), Some("b")]]);
        let said = "t: 2 text values start with an apostrophe before =, +, -, @, a tab or a line break, which reading the file back takes for the formula guard and drops, in 'note', 'code'";
        assert_eq!(text_notes(&ds, "csv"), [said]);
        assert_eq!(text_notes(&ds, "wkt"), [said]);
        // What the note says is what reading the file back does.
        let (bytes, _) = write_bytes(&ds, "csv").unwrap();
        let back = read_bytes("t.csv", &bytes).unwrap().0;
        let column = |n: &str| {
            let i = back.field_index(n).unwrap();
            back.features.iter().take(3).map(|f| f.attrs[i].to_string()).collect::<Vec<_>>()
        };
        assert_eq!(column("note"), ["=x", "'kept", "=y"]);
        assert_eq!(column("code"), ["a", "-5", "b"]);
        let clean = text_layer(&["note"], &[&[Some("'kept")], &[Some("=y")], &[Some("it's")]]);
        assert!(text_notes(&clean, "csv").is_empty());
    }

    /// A KML field whose name is blank is said with its values: it's written as
    /// `<Data name="">`, which reading drops, column and values (#2415). One without a value is
    /// said by the note on fields with no values, not by this one.
    #[test]
    fn kml_says_fields_with_a_blank_name() {
        let ds = text_layer(&["name", "", " ", "\t"], &[&[Some("a"), Some("x"), Some("y"), None], &[Some("b"), Some("z"), None, None]]);
        let notes = export_losses_notes(&ds, "kml", name_field(&ds), "t").unwrap();
        let blank: Vec<&String> = notes.iter().filter(|n| n.contains("blank name")).collect();
        assert_eq!(
            blank,
            ["t: 2 fields with a blank name, and 3 values in them, won't read back: reading KML drops extended data without a name"],
            "{notes:?}"
        );
        // What the note says is what reading the file back does.
        let (bytes, _) = write_bytes(&ds, "kml").unwrap();
        let back = &crate::kml::read("t.kml", &bytes).unwrap()[0].dataset;
        assert!(back.fields.iter().all(|f| !f.name.trim().is_empty()), "{:?}", back.fields);
        let clean = text_layer(&["name", "note", " "], &[&[Some("a"), Some("x"), None]]);
        let notes = export_losses_notes(&clean, "kml", name_field(&clean), "t").unwrap();
        assert!(!notes.iter().any(|n| n.contains("blank name")), "{notes:?}");
    }

    #[test]
    fn kml_says_trimmed_and_blank_text() {
        let ds = text_layer(
            &["name", "note", "code", "gone", "folder", "description"],
            &[
                &[Some(" a"), Some(" padded "), Some("1"), None, None, Some(" kept ")],
                &[Some("  "), Some(""), Some(""), None, None, Some("")],
                &[Some("b"), Some(" "), Some("2"), None, None, None],
            ],
        );
        assert_eq!(
            text_notes(&ds, "kml"),
            [
                "t: 3 text values start or end with spaces, tabs or line breaks, which reading the file back drops, in 'name', 'note'",
                "t: 2 blank text values read back as empty (null), in 'name', 'code'",
            ]
        );
        assert_eq!(text_notes(&ds, "kmz"), text_notes(&ds, "kml"));
        // What the notes say is what reading the file back does.
        let (bytes, _) = write_bytes(&ds, "kml").unwrap();
        let back = &crate::kml::read("t.kml", &bytes).unwrap()[0].dataset;
        let column = |n: &str| back.field_index(n).map(|i| back.features.iter().map(|f| f.attrs[i].clone()).collect::<Vec<_>>());
        let text = |t: &str| Value::Text(t.into());
        assert_eq!(column("name"), Some(vec![text("a"), Value::Null, text("b")]));
        assert_eq!(column("note"), Some(vec![text("padded"), text(""), text("")]));
        assert_eq!(column("code"), Some(vec![Value::Int(1), Value::Null, Value::Int(2)]));
        assert_eq!(column("gone"), None);
        assert_eq!(column("description"), Some(vec![text(" kept "), text(""), Value::Null]));
        let clean = text_layer(&["name", "note"], &[&[Some("a"), Some("")], &[None, Some("x")]]);
        assert!(text_notes(&clean, "kml").is_empty());
    }

    #[test]
    fn gpx_says_trimmed_and_blank_names() {
        let ds = text_layer(&["name", "note", "gone", "ele"], &[&[Some("a "), Some("x"), None, None], &[Some(""), None, None, None]]);
        assert_eq!(
            text_notes(&ds, "gpx"),
            [
                "t: 1 text value starts or ends with spaces, tabs or line breaks, which reading the file back drops, in 'name'",
                "t: 1 blank text value reads back as empty (null), in 'name'",
            ]
        );
        // GPX writes only names: one notice says the other fields, with a value first.
        assert_eq!(
            gpx_notes(&ds, name_field(&ds), "t"),
            ["t: GPX holds a name for each feature, so fields weren't written: 'note', 'gone', 'ele'"]
        );
        let clean = text_layer(&["name"], &[&[Some("a")], &[None]]);
        assert!(text_notes(&clean, "gpx").is_empty());
        assert!(gpx_notes(&clean, name_field(&clean), "t").is_empty());
        assert!(text_notes(&ds, "geojson").is_empty());
    }

    /// A placemark or waypoint reading skips (no coordinate in range, or a line of one point)
    /// holds nothing that comes back.
    #[test]
    fn kml_and_gpx_count_only_features_reading_keeps() {
        let mut ds = text_layer(&["name", "note"], &[&[Some("a"), None]]);
        ds.push(Some(Geometry::Point(Coord::new(200.0, 0.0))), vec![Value::Text(" off ".into()), Value::Text(" x".into())]);
        ds.push(Some(Geometry::MultiPoint(vec![])), vec![Value::Text("".into()), Value::Text("".into())]);
        ds.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0)]])), vec![Value::Text(" ".into()), Value::Text(" ".into())]);
        // 'note' has values only where reading skips the placemark: its padded text doesn't come
        // back, but its name still gives a column, of nulls. The skipped placemarks are said.
        let unread =
            |n: &str| format!("t: {n} written but won't read back: no coordinate is inside longitude/latitude, or too few for the shape");
        assert_eq!(text_notes(&ds, "kml"), [unread("3 features are")]);
        let (bytes, _) = write_bytes(&ds, "kml").unwrap();
        let layers = crate::kml::read("t.kml", &bytes).unwrap();
        assert_eq!(layers.len(), 1);
        let back = &layers[0].dataset;
        assert_eq!(back.features.len(), 1);
        assert_eq!(back.field_index("note").map(|i| back.value(0, i).clone()), Some(Value::Null));
        // GPX doesn't write the empty multipoint (said as left out); the other two it skips.
        assert_eq!(text_notes(&ds, "gpx"), [unread("2 features are")]);
        let (bytes, _) = write_bytes(&ds, "gpx").unwrap();
        let read = crate::gpx::read("t.gpx", &bytes).unwrap();
        // One waypoint, and no track (its lone point, unnamed, is among the track points).
        let count = |name: &str| read.iter().find(|(d, _)| d.name == name).map(|(d, _)| d.features.len());
        assert_eq!((count("t waypoints"), count("t tracks")), (Some(1), None));
        // The same values in a feature reading keeps are said.
        let kept = text_layer(&["name", "note"], &[&[Some(" off "), Some(" x")]]);
        assert_eq!(text_notes(&kept, "kml").len(), 1);
        assert_eq!(text_notes(&kept, "gpx").len(), 1);
    }

    /// A KML name field not called `name` is the placemark's name and its own extended data
    /// column, read with a type from its values: each value is counted once.
    #[test]
    fn a_kml_name_field_of_another_name_counts_each_value_once() {
        let ds = text_layer(&["title", "other"], &[&[Some(" a"), Some("x")], &[Some(""), Some("y")], &[Some("5"), Some("z")]]);
        let losses = |ds: &Dataset| TextLosses::of(&ds.features, &ds.fields, ds.geometry_type, "kml", Some(0)).notes(&ds.fields, "t");
        // In the name, " a" is trimmed and "" null; in 'title', a text column, " a" is trimmed too,
        // the same value.
        assert_eq!(
            losses(&ds),
            [
                "t: 1 text value starts or ends with spaces, tabs or line breaks, which reading the file back drops, in 'title'",
                "t: 1 blank text value reads back as empty (null), in 'title'",
            ]
        );
        let back = crate::interop::to_kml(&ds, Some(0)).unwrap();
        let read = &crate::kml::read("t.kml", back.as_bytes()).unwrap()[0].dataset;
        let column = |n: &str| read.field_index(n).map(|i| read.features.iter().map(|f| f.attrs[i].clone()).collect::<Vec<_>>());
        let text = |t: &str| Value::Text(t.into());
        assert_eq!(column("name"), Some(vec![text("a"), Value::Null, text("5")]));
        assert_eq!(column("title"), Some(vec![text("a"), text(""), text("5")]));
        let clean = text_layer(&["title"], &[&[Some("a")], &[Some("b")]]);
        assert!(losses(&clean).is_empty());
    }

    /// A KML name field's placemark name is trimmed even when its own extended data column reads
    /// as numbers, which keeps the padded number as a number.
    #[test]
    fn a_kml_name_of_numbers_is_still_trimmed() {
        let ds = text_layer(&["code"], &[&[Some(" 5")], &[Some("6")]]);
        let losses = |ds: &Dataset| TextLosses::of(&ds.features, &ds.fields, ds.geometry_type, "kml", Some(0)).notes(&ds.fields, "t");
        assert_eq!(
            losses(&ds),
            ["t: 1 text value starts or ends with spaces, tabs or line breaks, which reading the file back drops, in 'code'"]
        );
        let back = crate::interop::to_kml(&ds, Some(0)).unwrap();
        let read = &crate::kml::read("t.kml", back.as_bytes()).unwrap()[0].dataset;
        let column = |n: &str| read.field_index(n).map(|i| read.features.iter().map(|f| f.attrs[i].clone()).collect::<Vec<_>>());
        assert_eq!(column("name"), Some(vec![Value::Text("5".into()), Value::Text("6".into())]));
        assert_eq!(column("code"), Some(vec![Value::Int(5), Value::Int(6)]));
        let clean = text_layer(&["code"], &[&[Some("5")], &[Some("6")]]);
        assert!(losses(&clean).is_empty());
    }

    /// CSV and dBase read text `<Null>`, in any case and with spaces around it, as null.
    #[test]
    fn csv_and_shapefile_say_text_null_markers() {
        let mut ds = text_layer(&["note"], &[&[Some("<Null>")], &[Some(" <null> ")], &[Some("x")], &[Some("")]]);
        ds.features.retain(|f| f.geometry.is_some());
        for ext in ["csv", "zip"] {
            assert_eq!(text_notes(&ds, ext), ["t: 3 text values, blank or <Null>, read back as empty (null), in 'note'"], "{ext}");
            let (bytes, _) = write_bytes(&ds, ext).unwrap();
            let (back, _) = read_bytes(&format!("t.{ext}"), &bytes).unwrap();
            let i = back.field_index("note").unwrap();
            let values: Vec<_> = back.features.iter().map(|f| f.attrs[i].clone()).collect();
            assert_eq!(values, [Value::Null, Value::Null, Value::Text("x".into()), Value::Null], "{ext}");
        }
        // Text that only looks like the marker comes back as it is.
        let mut clean = text_layer(&["note"], &[&[Some("Null")], &[Some("<Null>x")]]);
        clean.features.retain(|f| f.geometry.is_some());
        assert!(text_notes(&clean, "csv").is_empty());
        assert!(text_notes(&clean, "zip").is_empty());
    }

    /// GPX writes a waypoint for each point of a multipoint, each with the name: reading trims each
    /// copy, and the value is counted once. The point out of range is a vertex that doesn't read
    /// back.
    #[test]
    fn gpx_counts_a_multipoints_name_once() {
        let mut ds = Dataset::new("t", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        let points = vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0), Coord::new(200.0, 0.0)];
        ds.push(Some(Geometry::MultiPoint(points)), vec![Value::Text(" a".into())]);
        assert_eq!(
            text_notes(&ds, "gpx"),
            [
                "t: 1 text value starts or ends with spaces, tabs or line breaks, which reading the file back drops, in 'name'",
                "t: 1 vertex won't read back with the features kept: outside longitude/latitude, or in a part left with too few to keep",
            ]
        );
        let (bytes, _) = write_bytes(&ds, "gpx").unwrap();
        let read = crate::gpx::read("t.gpx", &bytes).unwrap();
        let (waypoints, _) = read.iter().find(|(d, _)| d.name == "t waypoints").unwrap();
        let names: Vec<_> = waypoints.features.iter().map(|f| f.attrs[0].clone()).collect();
        assert_eq!(names, [Value::Text("a".into()), Value::Text("a".into())]);
        ds.features[0].attrs = vec![Value::Text("a".into())];
        ds.features[0].geometry = Some(Geometry::MultiPoint(vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]));
        assert!(text_notes(&ds, "gpx").is_empty());
    }

    /// KML and GPX readers drop a vertex outside longitude/latitude one at a time, and a part left
    /// with too few: the features kept lose those vertices, which are counted.
    #[test]
    fn kml_and_gpx_count_vertices_that_dont_read_back() {
        let c = Coord::new;
        let geometries = [
            // One vertex out of range; the line keeps two.
            Geometry::Line(vec![vec![c(0.0, 0.0), c(200.0, 0.0), c(1.0, 1.0)]]),
            // A second part left with one vertex is dropped whole: 2 vertices.
            Geometry::Line(vec![vec![c(0.0, 0.0), c(1.0, 1.0)], vec![c(2.0, 2.0), c(0.0, 95.0)]]),
            // Two points of three out of range.
            Geometry::MultiPoint(vec![c(0.0, 0.0), c(181.0, 0.0), c(0.0, -91.0)]),
        ];
        let mut ds = Dataset::new("t", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
        for g in geometries {
            ds.push(Some(g), vec![Value::Text("a".into())]);
        }
        let vertices = |n: usize| {
            format!(
                "t: {n} vertices won't read back with the features kept: outside longitude/latitude, or in a part left with too few to keep"
            )
        };
        assert_eq!(text_notes(&ds, "kml"), [vertices(5)]);
        assert_eq!(text_notes(&ds, "gpx"), [vertices(5)]);
        // Reading the file back keeps the other 5 of the 10 vertices written.
        let (bytes, _) = write_bytes(&ds, "kml").unwrap();
        let read = crate::kml::read("t.kml", &bytes).unwrap();
        let kept: usize =
            read.iter().flat_map(|l| &l.dataset.features).filter_map(|f| f.geometry.as_ref()).map(Geometry::coord_count).sum();
        assert_eq!(kept, 10 - 5);
        // A polygon: an outer ring of four in range keeps them; a hole left with two is dropped
        // (4 vertices), and a part whose outer ring is left with two is dropped with its hole.
        let square = |x: f64| vec![c(x, 0.0), c(x + 1.0, 0.0), c(x + 1.0, 1.0), c(x, 0.0)];
        let polygon = Geometry::Polygon(vec![
            vec![square(0.0), vec![c(0.1, 0.1), c(200.0, 0.0), c(200.0, 1.0), c(0.1, 0.1)]],
            vec![vec![c(2.0, 0.0), c(300.0, 0.0), c(300.0, 1.0), c(2.0, 1.0)], square(2.0)],
        ]);
        let mut ds = Dataset::new("t", GeometryType::Polygon, vec![Field::new("name", FieldType::Text)]);
        ds.push(Some(polygon), vec![Value::Text("a".into())]);
        assert_eq!(text_notes(&ds, "kml"), [vertices(12)]);
        let (bytes, _) = write_bytes(&ds, "kml").unwrap();
        let read = crate::kml::read("t.kml", &bytes).unwrap();
        assert_eq!(read[0].dataset.features[0].geometry, Some(Geometry::Polygon(vec![vec![square(0.0)]])));
        // GPX writes the outer rings as a track: the second is left with two, so isn't dropped.
        assert_eq!(text_notes(&ds, "gpx"), [vertices(2)]);
        // In range, nothing is said.
        let mut clean = Dataset::new("t", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
        clean.push(Some(Geometry::Line(vec![vec![c(0.0, 0.0), c(180.0, 90.0)]])), vec![Value::Text("a".into())]);
        clean.push(Some(Geometry::MultiPoint(vec![c(-180.0, -90.0), c(1.0, 1.0)])), vec![Value::Text("b".into())]);
        clean.push(Some(Geometry::Polygon(vec![vec![square(0.0), square(0.0)]])), vec![Value::Text("c".into())]);
        assert!(text_notes(&clean, "kml").is_empty());
        assert!(text_notes(&clean, "gpx").is_empty());
    }

    /// KML reads a layer for each kind of geometry and types each one's columns apart: a blank in
    /// the points' column of numbers reads as null, one in the lines' column of text doesn't.
    #[test]
    fn kml_types_each_kind_of_geometry_apart() {
        let line = || Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)]]));
        let point = |x: f64| Some(Geometry::Point(Coord::new(x, 1.0)));
        let mut ds = Dataset::new("t", GeometryType::Point, vec![Field::new("code", FieldType::Text)]);
        for (g, v) in [(point(0.0), "1"), (point(1.0), ""), (line(), "x"), (line(), "")] {
            ds.push(g, vec![Value::Text(v.into())]);
        }
        assert_eq!(text_notes(&ds, "kml"), ["t: 1 blank text value reads back as empty (null), in 'code'"]);
        let (bytes, _) = write_bytes(&ds, "kml").unwrap();
        let layers = crate::kml::read("t.kml", &bytes).unwrap();
        let code = |l: usize| {
            let d = &layers[l].dataset;
            let i = d.field_index("code").unwrap();
            d.features.iter().map(|f| f.attrs[i].clone()).collect::<Vec<_>>()
        };
        assert_eq!(code(0), [Value::Int(1), Value::Null]);
        assert_eq!(code(1), [Value::Text("x".into()), Value::Text("".into())]);
        // Lines alone: their blank is kept.
        ds.features.retain(|f| matches!(f.geometry, Some(Geometry::Line(_))));
        assert!(text_notes(&ds, "kml").is_empty());
    }

    /// A streamed CSV export adds up its chunks' text losses to the whole layer's, and types a
    /// column over all of them: 'code' reads as numbers only from the second chunk on.
    #[test]
    fn text_losses_of_chunks_add_up_to_the_whole_layer() {
        let ds = text_layer(
            &["note", "code"],
            &[&[Some(""), Some(" ")], &[Some("  "), Some("")], &[Some("x"), Some("12")], &[None, Some("  ")]],
        );
        let whole = TextLosses::of(&ds.features, &ds.fields, ds.geometry_type, "csv", None);
        for size in 1..=ds.features.len() {
            let mut sum = TextLosses::default();
            for chunk in ds.features.chunks(size) {
                sum += TextLosses::of(chunk, &ds.fields, ds.geometry_type, "csv", None);
            }
            assert_eq!(sum, whole, "chunks of {size}");
        }
        assert_eq!(whole.notes(&ds.fields, "t"), ["t: 5 blank text values read back as empty (null), in 'note', 'code'"]);
        // Alone, the first chunk's 'code' is text, which keeps its spaces.
        let first = TextLosses::of(&ds.features[..1], &ds.fields, ds.geometry_type, "csv", None);
        assert_eq!(first.notes(&ds.fields, "t"), ["t: 1 blank text value reads back as empty (null), in 'note'"]);
        // A column that is Text skips the values after, and chunks still add up to the whole: a
        // whole number past a Double's digits, alone a chunk's Integer, is Text after text.
        let wide = text_layer(&["id"], &[&[Some("abc")], &[Some("9007199254740993")], &[Some(" ")]]);
        let whole = TextLosses::of(&wide.features, &wide.fields, wide.geometry_type, "csv", None);
        for size in 1..=wide.features.len() {
            let mut sum = TextLosses::default();
            for chunk in wide.features.chunks(size) {
                sum += TextLosses::of(chunk, &wide.fields, wide.geometry_type, "csv", None);
            }
            assert_eq!(sum, whole, "chunks of {size}");
        }
        assert_eq!(whole.notes(&wide.fields, "t"), Vec::<String>::new());
        // The NUL and apostrophe counts add up too (#2415).
        for (ext, layer, said) in [
            ("csv", text_layer(&["note"], &[&[Some("'=x")], &[Some("y")], &[Some("'+z")]]), "2 text values start with an apostrophe"),
            ("shp", text_layer(&["note"], &[&[Some("a\0b")], &[Some("y")], &[Some("ab \0c")]]), "2 text values hold a NUL"),
        ] {
            let whole = TextLosses::of(&layer.features, &layer.fields, layer.geometry_type, ext, None);
            for size in 1..=layer.features.len() {
                let mut sum = TextLosses::default();
                for chunk in layer.features.chunks(size) {
                    sum += TextLosses::of(chunk, &layer.fields, layer.geometry_type, ext, None);
                }
                assert_eq!(sum, whole, "{ext}: chunks of {size}");
            }
            let notes = whole.notes(&layer.fields, "t");
            assert!(notes.iter().any(|n| n.contains(said)), "{ext}: {notes:?}");
        }
    }

    /// GPX names fields with values before fields of nulls, so a field that lost data isn't
    /// hidden behind "and N more".
    #[test]
    fn gpx_names_fields_with_values_first() {
        let mut fields: Vec<_> = (1..=5).map(|i| Field::new(format!("empty{i}"), FieldType::Text)).collect();
        fields.push(Field::new("population", FieldType::Integer));
        let mut ds = Dataset::new("t", GeometryType::Point, fields);
        let mut attrs = vec![Value::Null; 5];
        attrs.push(Value::Int(7));
        ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), attrs);
        let notes = gpx_notes(&ds, None, "t.gpx");
        assert!(notes[0].contains("written: 'population', 'empty1'") && notes[0].ends_with("and 1 more"), "{notes:?}");
    }

    #[test]
    fn kml_and_kmz_name_fields_missing_from_exported_placemarks() {
        let mut ds =
            Dataset::new("parcels", GeometryType::Point, vec![Field::new("a", FieldType::Integer), Field::new("empty", FieldType::Text)]);
        ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Int(1), Value::Null]);
        ds.push(None, vec![Value::Int(2), Value::Text("no placemark".into())]);
        for ext in ["kml", "kmz"] {
            let target = format!("parcels.{ext}");
            let (bytes, _) = write_bytes(&ds, ext).unwrap();
            let back = &crate::kml::read(&target, &bytes).unwrap()[0].dataset;
            assert!(back.field_index("a").is_some());
            assert!(back.field_index("empty").is_none());
            assert_eq!(
                export_losses_notes(&ds, ext, None, &target).unwrap(),
                [format!("{target}: KML omitted 1 field with no values in the placemarks written ('empty')")]
            );
        }
        ds.push(Some(Geometry::Point(Coord::new(3.0, 4.0))), vec![Value::Int(3), Value::Text("present".into())]);
        assert!(export_losses_notes(&ds, "kml", None, "parcels.kml").unwrap().is_empty());
    }

    /// A point layer's fields named like coordinates keep their values when its CSV is read back:
    /// the coordinate columns come first, so reading takes them, not the fields. (Reading keeps the
    /// coordinate columns as fields too.)
    #[test]
    fn csv_fields_named_like_coordinates_stay_fields() {
        let names = ["lat", "x", "longitude"];
        let fields: Vec<Field> = names.iter().map(|n| Field::new(*n, FieldType::Double)).collect();
        let mut ds = Dataset::new("t", GeometryType::Point, fields);
        ds.push(Some(Geometry::Point(Coord::new(20.0, 10.0))), vec![Value::Float(1.5), Value::Float(2.5), Value::Float(3.5)]);
        let out = to_csv(&ds).unwrap();
        assert_eq!(out.lines().next(), Some("lon,latitude,lat,x,longitude"), "{out}");
        let (back, _) = read_csv(&out, "t").unwrap();
        assert_eq!(back.features[0].geometry, Some(Geometry::Point(Coord::new(20.0, 10.0))));
        for (name, v) in names.iter().zip([1.5, 2.5, 3.5]) {
            let i = back.field_index(name).unwrap();
            assert_eq!(back.value(0, i), &Value::Float(v), "{name}");
        }
        // A name with spaces around it is the name reading gives it.
        let mut padded = Dataset::new("t", GeometryType::Point, vec![Field::new(" longitude ", FieldType::Double)]);
        padded.push(Some(Geometry::Point(Coord::new(20.0, 10.0))), vec![Value::Float(1.5)]);
        let out = to_csv(&padded).unwrap();
        assert_eq!(out.lines().next(), Some("lon,latitude, longitude "), "{out}");
        let (back, _) = read_csv(&out, "t").unwrap();
        assert_eq!(back.features[0].geometry, Some(Geometry::Point(Coord::new(20.0, 10.0))));
        assert_eq!(back.value(0, back.field_index("longitude").unwrap()), &Value::Float(1.5));
    }

    #[test]
    fn csv_generic_coordinate_name_collisions_keep_geometry_on_reimport() {
        let names = ["longitude", "lon", "lng", "long", "latitude", "lat"];
        let mut ds = Dataset::new("t", GeometryType::Point, names.iter().map(|n| Field::new(*n, FieldType::Double)).collect());
        ds.push(
            Some(Geometry::Point(Coord::new(20.0, 10.0))),
            vec![Value::Float(40.0), Value::Float(40.0), Value::Float(40.0), Value::Float(40.0), Value::Float(50.0), Value::Float(50.0)],
        );
        let text = to_csv(&ds).unwrap();
        assert!(text.starts_with("lon_dd,lat_dd,"), "{text}");
        let (back, _) = read_csv(&text, "back").unwrap();
        assert_eq!(back.features[0].geometry, ds.features[0].geometry);
        assert!(export_losses_notes(&ds, "csv", None, "t.csv").unwrap().is_empty());
    }

    /// A point layer with a field of every longitude name has its longitudes in a column reading
    /// doesn't take, and its export says so; one with a name left says nothing.
    #[test]
    fn csv_coordinates_in_columns_reading_doesnt_take_are_said() {
        let layer = |names: &[&str]| {
            let fields: Vec<Field> = names.iter().map(|n| Field::new(*n, FieldType::Integer)).collect();
            let mut ds = Dataset::new("t", GeometryType::Point, fields);
            ds.push(Some(Geometry::Point(Coord::new(20.0, 10.0))), names.iter().map(|_| Value::Int(1)).collect());
            ds
        };
        let every = layer(LON_NAMES);
        let out = to_csv(&every).unwrap();
        assert!(out.starts_with("longitude_1,latitude,"), "{out}");
        let notes = export_losses_notes(&every, "csv", None, "t.csv").unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(
            notes[0],
            "t.csv: the fields have every name read as a longitude or latitude, so the coordinates are in columns 'longitude_1' and 'latitude'; adding the file back reads columns 'longitude' and 'latitude' as the coordinates, which moves the points unless those columns are picked"
        );
        // As said: reading takes the field `longitude` (1) for the longitude.
        let (back, _) = read_csv(&out, "t").unwrap();
        assert_eq!(back.features[0].geometry, Some(Geometry::Point(Coord::new(1.0, 10.0))));
        // A name left, or a layer that isn't of points: nothing to say.
        assert!(export_losses_notes(&layer(&LON_NAMES[1..]), "csv", None, "t.csv").unwrap().is_empty());
        assert_eq!(CsvLosses::of(&[], GeometryType::Polyline, &[]).unwrap().coordinates_note(&every.fields, "t.csv"), None);
        let names: Vec<_> = LON_NAMES.iter().copied().chain(["X", "Y"]).collect();
        let notes = export_losses_notes(&layer(&names), "csv", None, "t.csv").unwrap();
        assert!(notes[0].contains("may read 'longitude' and 'latitude' or 'X' and 'Y'"), "{notes:?}");
    }

    /// Missing geometry can let exported X/Y attributes win on reimport. Report the ambiguity
    /// before the file is reopened and show which columns restore the exported geometry.
    #[test]
    fn csv_export_warns_when_attributes_can_supply_geometry() {
        let fields = vec![Field::new("X", FieldType::Integer), Field::new("Y", FieldType::Integer)];
        let geometry = Some(Geometry::Point(Coord::new(20.0, 10.0)));
        let mut ds = Dataset::new("t", GeometryType::Point, fields);
        ds.push(geometry.clone(), vec![Value::Int(40), Value::Int(50)]);
        assert!(export_losses_notes(&ds, "csv", None, "t.csv").unwrap().is_empty());
        ds.push(None, vec![Value::Int(41), Value::Int(51)]);
        let notes = export_losses_notes(&ds, "csv", None, "t.csv").unwrap();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("1 of the exported coordinate rows"), "{notes:?}");
        assert!(notes[0].contains("may read columns 'X' and 'Y'"), "{notes:?}");
        let text = to_csv(&ds).unwrap();
        let (reopened, _) = read_csv(&text, "t").unwrap();
        assert_eq!(reopened.features[0].geometry, Some(Geometry::Point(Coord::new(40.0, 50.0))));
        let opts = CsvOptions { x: Some("longitude".into()), y: Some("latitude".into()), ..Default::default() };
        let (reopened, _) = read_csv_with(&text, "t", &opts).unwrap();
        assert_eq!(reopened.features[0].geometry, geometry);
        assert_eq!(reopened.features[1].geometry, None);
    }

    #[test]
    fn csv_leading_zeros_and_guard_roundtrip() {
        let (ds, _) = read_csv("zip,n,note\n02134,0,-abc\n10001,0.5,=x\n", "t").unwrap();
        assert_eq!(ds.fields[0].ty, FieldType::Text);
        assert_eq!(ds.value(0, 0), &Value::Text("02134".into()));
        assert_eq!(ds.fields[1].ty, FieldType::Double);
        let once = to_csv(&ds).unwrap();
        let (back, _) = read_csv(&once, "t").unwrap();
        assert_eq!(back.value(0, 2), &Value::Text("-abc".into()));
        assert_eq!(back.value(1, 2), &Value::Text("=x".into()));
        assert_eq!(to_csv(&back).unwrap(), once, "export/import is stable");
    }

    /// `<Null>` in a `.dbf` date column is counted once, as the marker, not also as a date that
    /// isn't one, whole and streamed.
    #[test]
    fn a_null_marker_in_a_date_column_is_not_an_unread_date() {
        let mut ds = Dataset::new("permits", GeometryType::Point, vec![Field::new("issued", FieldType::Date)]);
        for day in ["2026-01-02", "2026-01-03"] {
            ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::parse(day, FieldType::Date)]);
        }
        let dir = tempfile::tempdir().unwrap();
        let shp = dir.path().join("permits.shp");
        write_path(&ds, &shp).unwrap();
        let dbf = shp.with_extension("dbf");
        let original = std::fs::read(&dbf).unwrap();
        // Padded with spaces, or with NULs as some writers do.
        for marker in [b"<Null>  ", b"<Null>\0\0"] {
            let mut table = original.clone();
            let at = table.windows(8).position(|w| w == b"20260103").unwrap();
            table[at..at + 8].copy_from_slice(marker);
            std::fs::write(&dbf, table).unwrap();
            let whole = read_shapefile_in(&shp, "permits", None).unwrap();
            let streamed = crate::source::materialize(&crate::source::shp::ShpSource::open_path(&shp, "permits").unwrap()).unwrap();
            for (read, (back, report)) in [("whole", whole), ("streamed", streamed)] {
                assert_eq!(back.features[1].attrs[0], Value::Null, "{read}");
                let said = "permits: 1 value written as Null was read as empty, in 'issued'";
                assert!(
                    report.messages.iter().any(|m| m.as_str() == said && m.loss == Some(crate::loss::LossCode::AssumedNullMarker)),
                    "{read}: {:?}",
                    report.messages
                );
                assert!(!report.messages.iter().any(|m| m.contains("date")), "{read}: {:?}", report.messages);
            }
        }
    }

    /// Text cells read as the file has them (#2059): spaces kept, all-space text kept, an empty cell
    /// null, `<Null>` null and counted; a number column still reads past the spaces, and `<Null>`
    /// doesn't make it text.
    #[test]
    fn csv_and_workbook_text_is_read_as_the_file_has_it() {
        // A workbook keeps the spaces of a cell marked so, as spreadsheet programs mark them.
        let kept = |t: &str| format!(r#"<c t="inlineStr"><is><t xml:space="preserve">{t}</t></is></c>"#);
        let (lead, blank, trail) = (kept(" 2535"), kept("   "), kept("x  "));
        let rows: &[&[&str]] = &[
            &["code", "n", "note", "x"],
            &[&lead, " 7 ", "a", "1.5"],
            &[&blank, "8", "&lt;null&gt;", "&lt;Null&gt;"],
            &["&lt;Null&gt;", "", "b", "2.5"],
            &[&trail, "9", "c", "3.5"],
        ];
        let text = "code,n,note,x\n 2535, 7 ,a,1.5\n   ,8,<null>,<Null>\n<Null>,,b,2.5\nx  ,9,c,3.5\n";
        let csv = read_csv(text, "t").unwrap();
        let book = crate::sheets::read(&crate::sheets::xlsx(&[("Sheet1", rows)]), "t", None, &CsvOptions::default()).unwrap();
        for (what, (ds, report)) in [("csv", csv), ("xlsx", book)] {
            let column = |k: usize| ds.features.iter().map(|f| f.attrs[k].clone()).collect::<Vec<_>>();
            let text = |t: &str| Value::Text(t.into());
            assert_eq!(column(0), [text(" 2535"), text("   "), Value::Null, text("x  ")], "{what}");
            assert_eq!(column(1), [Value::Int(7), Value::Int(8), Value::Null, Value::Int(9)], "{what}: numbers read past the spaces");
            assert_eq!(column(2), [text("a"), Value::Null, text("b"), text("c")], "{what}");
            assert_eq!(ds.fields[3].ty, FieldType::Double, "{what}: `<Null>` doesn't type a column");
            assert_eq!(column(3), [Value::Float(1.5), Value::Null, Value::Float(2.5), Value::Float(3.5)], "{what}");
            let said = "3 values written as Null were read as empty, in 'code', 'note', 'x'";
            assert!(
                report.messages.iter().any(|m| m.as_str() == said && m.loss == Some(crate::loss::LossCode::AssumedNullMarker)),
                "{what}: {:?}",
                report.messages
            );
        }
        let (_, report) = read_csv("code\n 2535\n", "t").unwrap();
        assert!(report.messages.iter().all(|m| !m.contains("written as Null")), "{:?}", report.messages);
    }

    #[test]
    fn csv_digits_a_number_cannot_hold_stay_text() {
        let text = "id,share,mixed,n,x\n12345678901234567891,0.12345678901234567891,9007199254740993,7,1.50\n2,0.5,1.5,8,1e300\n";
        let (ds, _) = read_csv(text, "t").unwrap();
        let types: Vec<FieldType> = ds.fields.iter().map(|f| f.ty).collect();
        assert_eq!(types, [FieldType::Text, FieldType::Text, FieldType::Text, FieldType::Integer, FieldType::Double]);
        assert_eq!(ds.value(0, 0), &Value::Text("12345678901234567891".into()));
        assert_eq!(ds.value(0, 1), &Value::Text("0.12345678901234567891".into()));
        assert_eq!(ds.value(0, 2), &Value::Text("9007199254740993".into()), "a whole number past 2^53 in a decimal column");
        assert_eq!(ds.value(1, 4), &Value::Float(1e300));
        // A number written to 17 digits, as `%.17g` writes 0.1, is a Double.
        let (ds, _) = read_csv("v\n0.10000000000000001\n2.5\n", "t").unwrap();
        assert_eq!((ds.fields[0].ty, ds.value(0, 0)), (FieldType::Double, &Value::Float(0.1)));
        // So is one written out exactly, however many digits that takes.
        assert!(double_holds("0.00000000023283064365386962890625") && !double_holds(&"1".repeat(800)));
        // A whole number past i64 with trailing zeros is held only if exactly.
        assert!(!double_holds("12345678901234567000") && double_holds("18446744073709551616"));
        // At any size, with no saturation: i128::MAX isn't one, 2^200 is.
        assert!(!double_holds("170141183460469231731687303715884105727"));
        assert!(double_holds("1606938044258990275541962092341162602522202993782792835301376"));
        // A whole number beside a decimal: 2^60 is a Double exactly, 10^18 + 100 isn't.
        let (ds, _) = read_csv("a,b\n1152921504606846976,1000000000000000100\n0.5,0.5\n", "t").unwrap();
        assert_eq!((ds.fields[0].ty, ds.fields[1].ty), (FieldType::Double, FieldType::Text));
    }

    /// CSV export writes numbers as stored (#513): through `Display` a 0.4 m² area in km² was 0.
    #[test]
    fn csv_floats_keep_full_precision() {
        let (ds, _) = read_csv("v\n0.0000004\n0.002492708\n1\n", "t").unwrap();
        let out = to_csv(&ds).unwrap();
        assert_eq!(out, "v\n0.0000004\n0.002492708\n1\n");
    }

    #[test]
    fn csv_bom_injection_and_column_names() {
        let (ds, _) = read_csv("\u{feff}lon,lat,longitude,note\n1,2,9,=SUM(A1)\n", "t").unwrap();
        assert_eq!(ds.geometry_type, GeometryType::Point, "BOM must not hide the lon header");
        let out = to_csv(&ds).unwrap();
        assert!(out.contains("'=SUM(A1)"), "{out}");
        assert_eq!(out.lines().next(), Some("lng,latitude,lon,lat,longitude,note"), "{out}");
        let (ds, _) = read_csv("a\nNaN\nx\n", "t").unwrap();
        assert_eq!(ds.fields[0].ty, FieldType::Text);
    }

    #[test]
    fn csv_export_does_not_escape_valid_signed_numbers() {
        let mut ds =
            Dataset::new("tiger", GeometryType::None, vec![Field::new("INTPTLAT", FieldType::Text), Field::new("note", FieldType::Text)]);
        ds.push(None, vec![Value::Text("+35.1329460".into()), Value::Text("=SUM(A1)".into())]);
        ds.push(None, vec![Value::Text("-90.1234567".into()), Value::Text("+cmd|' /C calc'!A0".into())]);
        ds.push(None, vec![Value::Text("+447700900123".into()), Value::Text("-12345678901234567890".into())]);
        ds.push(None, vec![Value::Text("-007".into()), Value::Text("-123456789012345678".into())]);
        ds.push(None, vec![Value::Text("-1234567890123456".into()), Value::Text("+1234567890123456".into())]);
        ds.push(None, vec![Value::Text("+1234567890123456.5".into()), Value::Text("+1.234567890123456e5".into())]);
        ds.push(None, vec![Value::Text("-1152921504606846976".into()), Value::Text("-9007199254740992".into())]);
        ds.push(None, vec![Value::Text("+987654321098765".into()), Value::Text("+0.00001234".into())]);
        ds.push(None, vec![Value::Text("+35.13294600000000".into()), Value::Text("-90.12345670000000".into())]);
        let out = to_csv(&ds).unwrap();
        assert!(out.contains("+35.1329460"), "{out}");
        assert!(!out.contains("'+35.1329460"), "{out}");
        assert!(out.contains("+35.13294600000000"), "{out}");
        assert!(!out.contains("'+35.13294600000000"), "{out}");
        assert!(out.contains("-90.12345670000000"), "{out}");
        assert!(!out.contains("'-90.12345670000000"), "{out}");
        assert!(out.contains("-90.1234567"), "{out}");
        assert!(!out.contains("'-90.1234567"), "{out}");
        assert!(out.contains("+447700900123"), "{out}");
        assert!(!out.contains("'+447700900123"), "{out}");
        assert!(out.contains("+987654321098765"), "{out}");
        assert!(!out.contains("'+987654321098765"), "{out}");
        assert!(out.contains("+0.00001234"), "{out}");
        assert!(!out.contains("'+0.00001234"), "{out}");
        assert!(out.contains("'-007,"), "{out}");
        assert!(out.contains("'-1234567890123456,"), "{out}");
        assert!(out.contains(",'+1234567890123456\n"), "{out}");
        assert!(out.contains("'+1234567890123456.5,"), "{out}");
        assert!(out.contains(",'+1.234567890123456e5\n"), "{out}");
        assert!(out.contains("'-1152921504606846976,"), "{out}");
        assert!(out.contains(",'-9007199254740992\n"), "{out}");
        assert!(out.contains(",'-123456789012345678\n"), "{out}");
        assert!(out.contains(",'-12345678901234567890\n"), "{out}");
        assert!(out.contains("'=SUM(A1)"), "{out}");
        assert!(out.contains("'+cmd"), "{out}");
    }

    #[test]
    fn csv_keeps_padded_decimals_and_reports_only_guarded_values() {
        let mut ds =
            Dataset::new("tiger", GeometryType::None, ["INTPTLAT", "INTPTLON"].map(|name| Field::new(name, FieldType::Text)).to_vec());
        ds.push(None, vec![Value::Text("+44.9500000".into()), Value::Text("-093.1445780".into())]);
        assert_eq!(to_csv(&ds).unwrap(), "INTPTLAT,INTPTLON\n+44.9500000,-093.1445780\n");
        assert!(export_losses_notes(&ds, "csv", None, "tiger.csv").unwrap().is_empty());
        ds.push(None, vec![Value::Text("-007".into()), Value::Text("=SUM(A1)".into())]);
        assert!(to_csv(&ds).unwrap().ends_with("'-007,'=SUM(A1)\n"));
        assert_eq!(
            export_losses_notes(&ds, "csv", None, "tiger.csv").unwrap(),
            ["tiger.csv: CSV prefixed 2 text values with an apostrophe for spreadsheet protection in 'INTPTLAT' (1), 'INTPTLON' (1)"]
        );
    }

    #[test]
    fn csv_signed_numbers_and_text_round_trip() {
        let mut ds = Dataset::new(
            "test",
            GeometryType::None,
            vec![
                Field::new("coord", FieldType::Text),
                Field::new("phone", FieldType::Text),
                Field::new("code", FieldType::Text),
                Field::new("long_id", FieldType::Text),
                Field::new("formula", FieldType::Text),
            ],
        );
        ds.push(
            None,
            vec![
                Value::Text("+35.1329460".into()),
                Value::Text("+447700900123".into()),
                Value::Text("-007".into()),
                Value::Text("-123456789012345678".into()),
                Value::Text("=SUM(A1)".into()),
            ],
        );
        let csv_text = to_csv(&ds).unwrap();
        let (reimported, _) = read_csv(&csv_text, "test").unwrap();
        assert_eq!(reimported.fields[0].ty, FieldType::Double);
        assert_eq!(reimported.features[0].attrs[0], Value::Float(35.132946));
        assert_eq!(reimported.fields[1].ty, FieldType::Integer);
        assert_eq!(reimported.features[0].attrs[1], Value::Int(447700900123));
        assert_eq!(reimported.fields[2].ty, FieldType::Text);
        assert_eq!(reimported.features[0].attrs[2], Value::Text("-007".into()));
        assert_eq!(reimported.fields[3].ty, FieldType::Text);
        assert_eq!(reimported.features[0].attrs[3], Value::Text("-123456789012345678".into()));
        assert_eq!(reimported.fields[4].ty, FieldType::Text);
        assert_eq!(reimported.features[0].attrs[4], Value::Text("=SUM(A1)".into()));
    }

    #[test]
    fn hostile_geojson_is_an_error_not_a_panic() {
        for t in ["", "{", "[]", "{\"type\":5}", "{\"type\":\"FeatureCollection\",\"features\":7}"] {
            let _ = read_geojson(t, "x");
        }
        // A `crs` of any shape, anywhere in the document: read, never a panic.
        let deep = "[".repeat(5000) + &"]".repeat(5000);
        for crs in
            ["null", "7", "-1e308", "\"\"", "[]", "{}", "{\"type\":\"name\"}", "{\"type\":\"name\",\"properties\":{\"name\":{}}}", &deep]
        {
            for t in [
                format!(
                    "{{\"crs\":{crs},\"type\":\"FeatureCollection\",\"features\":[{{\"type\":\"Feature\",\"geometry\":{{\"type\":\"Point\",\"coordinates\":[1,2]}}}}]}}"
                ),
                format!("{{\"type\":\"FeatureCollection\",\"features\":7,\"crs\":{crs}}}"),
                format!("{{\"type\":\"Feature\",\"crs\":{crs},\"geometry\":null}}"),
                format!("{{\"type\":\"Point\",\"coordinates\":[1,2],\"crs\":{crs}}}"),
            ] {
                let _ = read_geojson(&t, "x");
            }
        }
    }

    #[test]
    fn read_path_dispatches_by_extension() {
        let dir = std::env::temp_dir().join(format!("rustgis-io-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("square.GeoJSON");
        std::fs::write(&p, r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}"#).unwrap();
        let (ds, _) = read_path(&p).unwrap();
        assert_eq!(ds.geometry_type, GeometryType::Polygon);
        assert!(read_path(&dir.join("x.kml")).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A `.dbf` whose header date is zero, as GeoLibre's runner writes, opens with its rows:
    /// read from a file, from a zip, and streamed (#1272).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_dbf_with_a_zero_header_date_opens() {
        use shapefile::dbase::{FieldValue, Record, TableWriterBuilder};
        let dir = tempfile::tempdir().unwrap();
        let shp = dir.path().join("curvas.shp");
        {
            let table = TableWriterBuilder::new().add_character_field("name".try_into().unwrap(), 10);
            let mut w = shapefile::Writer::from_path(&shp, table).unwrap();
            for name in ["a", "b"] {
                let mut r = Record::default();
                r.insert("name".into(), FieldValue::Character(Some(name.into())));
                w.write_shape_and_record(&shapefile::Point::new(1.0, 2.0), &r).unwrap();
            }
        }
        let dbf = shp.with_extension("dbf");
        let mut bytes = std::fs::read(&dbf).unwrap();
        bytes[1..4].copy_from_slice(&[0, 0, 0]);
        std::fs::write(&dbf, &bytes).unwrap();
        let names = |ds: &Dataset| (0..ds.features.len()).map(|r| ds.value(r, 0).clone()).collect::<Vec<_>>();
        let rows = vec![Value::Text("a".into()), Value::Text("b".into())];
        let (ds, rep) = read_path(&shp).unwrap();
        assert_eq!(names(&ds), rows);
        assert!(rep.messages.is_empty(), "{:?}", rep.messages);
        let entries: Vec<(String, Vec<u8>)> =
            ["shp", "shx", "dbf"].iter().map(|e| (format!("curvas.{e}"), std::fs::read(shp.with_extension(e)).unwrap())).collect();
        let layers = read_zip("curvas.zip", &crate::zip::write(&entries).unwrap()).unwrap();
        assert_eq!(names(&layers[0].0), rows);
        let src = crate::source::shp::ShpSource::open_path(&shp, "curvas").unwrap();
        assert_eq!(names(&crate::source::materialize(&src).unwrap().0), rows);
        // The file itself is left as it was.
        assert_eq!(std::fs::read(&dbf).unwrap(), bytes);
    }

    /// A floating point (`F`) field is read at full `f64` precision, as GDAL reads it, not rounded
    /// through `f32` (`1809878594` as `1809878656`): read from a file, from a zip, and streamed.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn shapefile_float_fields_keep_f64_precision() {
        use shapefile::dbase::{FieldValue, Record, TableWriterBuilder};
        let dir = tempfile::tempdir().unwrap();
        let shp = dir.path().join("parcels.shp");
        {
            let table = TableWriterBuilder::new().add_numeric_field("valuation".try_into().unwrap(), 12, 0).add_numeric_field(
                "lat".try_into().unwrap(),
                19,
                9,
            );
            let mut w = shapefile::Writer::from_path(&shp, table).unwrap();
            let mut r = Record::default();
            r.insert("valuation".into(), FieldValue::Numeric(Some(1_809_878_594.0)));
            r.insert("lat".into(), FieldValue::Numeric(Some(35.373_575_453)));
            w.write_shape_and_record(&shapefile::Point::new(1.0, 2.0), &r).unwrap();
        }
        // The writer has no `f64` floating point field: relabel the two columns `F`, as GDAL writes them.
        let dbf = shp.with_extension("dbf");
        let mut bytes = std::fs::read(&dbf).unwrap();
        for column in 0..2 {
            bytes[32 + column * 32 + 11] = b'F';
        }
        std::fs::write(&dbf, &bytes).unwrap();
        let exact = |ds: &Dataset| {
            assert_eq!(ds.value(0, 0).as_f64(), Some(1_809_878_594.0));
            assert_eq!(ds.value(0, 1).as_f64(), Some(35.373_575_453));
        };
        let (ds, rep) = read_path(&shp).unwrap();
        exact(&ds);
        assert!(rep.messages.is_empty(), "{:?}", rep.messages);
        let entries: Vec<(String, Vec<u8>)> =
            ["shp", "shx", "dbf"].iter().map(|e| (format!("parcels.{e}"), std::fs::read(shp.with_extension(e)).unwrap())).collect();
        exact(&read_zip("parcels.zip", &crate::zip::write(&entries).unwrap()).unwrap()[0].0);
        let src = crate::source::shp::ShpSource::open_path(&shp, "parcels").unwrap();
        exact(&crate::source::materialize(&src).unwrap().0);

        // A value that isn't finite is read as empty, and a notice counts it.
        let odd = dir.path().join("odd.shp");
        {
            let table = TableWriterBuilder::new().add_numeric_field("lat".try_into().unwrap(), 19, 9);
            let mut w = shapefile::Writer::from_path(&odd, table).unwrap();
            for v in [f64::NAN, 1.5] {
                let mut r = Record::default();
                r.insert("lat".into(), FieldValue::Numeric(Some(v)));
                w.write_shape_and_record(&shapefile::Point::new(1.0, 2.0), &r).unwrap();
            }
        }
        let dbf = odd.with_extension("dbf");
        let mut bytes = std::fs::read(&dbf).unwrap();
        bytes[32 + 11] = b'F';
        std::fs::write(&dbf, &bytes).unwrap();
        let (ds, rep) = read_path(&odd).unwrap();
        assert_eq!(ds.value(0, 0), &Value::Null);
        assert_eq!(ds.value(1, 0).as_f64(), Some(1.5));
        assert!(rep.messages.iter().any(|m| m.contains("1 number isn't finite") && m.contains("'lat'")), "{:?}", rep.messages);
    }

    /// A GeoJSON read in the system its legacy `crs` member names is written back in
    /// longitude/latitude, converted through WGS 84 first, as the format is: UTM 32N, Ingolstadt.
    #[test]
    fn a_held_geojson_is_written_in_lonlat() {
        let at = Coord::new(11.5, 48.7);
        let utm = Crs::Epsg(32632).transformer().unwrap().from_lonlat(at).unwrap();
        let text = format!(
            r#"{{"type":"FeatureCollection","crs":{{"type":"name","properties":{{"name":"urn:ogc:def:crs:EPSG::32632"}}}},"features":[{{"type":"Feature","properties":{{}},"geometry":{{"type":"Point","coordinates":[{},{}]}}}}]}}"#,
            utm.x, utm.y
        );
        let (ds, _) = read_geojson(&text, "held").unwrap();
        assert_eq!(ds.coord_crs(), Crs::Epsg(32632));
        let out = to_geojson(&ds.in_lonlat().unwrap());
        let c = &out["features"][0]["geometry"]["coordinates"];
        let (x, y) = (c[0].as_f64().unwrap(), c[1].as_f64().unwrap());
        assert!((x - at.x).abs() < 1e-9 && (y - at.y).abs() < 1e-9, "{c}");
        assert!(out.get("crs").is_none());
    }

    /// A `.prj` that doesn't match the coordinates (a geographic system for numbers past 180°) is
    /// said, though the file is held in its system as it is, as it was when it was converted.
    #[test]
    fn a_shapefile_whose_prj_doesnt_fit_is_said() {
        use shapefile::dbase::{FieldValue, Record, TableWriterBuilder};
        let dir = std::env::temp_dir().join(format!("rustgis-shp-prj-misfit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shp = dir.join("misfit.shp");
        {
            let table = TableWriterBuilder::new().add_character_field("name".try_into().unwrap(), 10);
            let mut w = shapefile::Writer::from_path(&shp, table).unwrap();
            let mut r = Record::default();
            r.insert("name".into(), FieldValue::Character(Some("far".into())));
            w.write_shape_and_record(&shapefile::Point::new(500.0, 10.0), &r).unwrap();
        }
        let prj = r#"GEOGCS["GCS_North_American_1983",DATUM["D_North_American_1983",SPHEROID["GRS_1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
        std::fs::write(dir.join("misfit.prj"), prj).unwrap();
        let (_, rep) = read_path(&shp).unwrap();
        assert!(
            rep.messages.iter().any(|m| m.contains("not longitude/latitude") || m.contains("couldn't be converted")),
            "{:?}",
            rep.messages
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A projected Shapefile is converted to longitude/latitude by its `.prj` (issue #84).
    #[test]
    fn shapefile_prj_reprojects() {
        use shapefile::dbase::{FieldValue, Record, TableWriterBuilder};
        let dir = std::env::temp_dir().join(format!("rustgis-shp-prj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shp = dir.join("paris.shp");
        {
            let table = TableWriterBuilder::new().add_character_field("name".try_into().unwrap(), 10);
            let mut w = shapefile::Writer::from_path(&shp, table).unwrap();
            let mut r = Record::default();
            r.insert("name".into(), FieldValue::Character(Some("paris".into())));
            // Paris in UTM zone 31N.
            w.write_shape_and_record(&shapefile::Point::new(451_219.33, 5_411_550.20), &r).unwrap();
        }
        // In longitude/latitude, from the system the read holds it in.
        let at = |ds: &Dataset| ds.in_lonlat().unwrap().features[0].geometry.as_ref().and_then(Geometry::label_point).unwrap();

        // As ArcGIS and GDAL write it: Esri WKT with no EPSG authority.
        let prj = r#"PROJCS["WGS_1984_UTM_Zone_31N",GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",3.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
        std::fs::write(dir.join("paris.prj"), prj).unwrap();
        let (ds, rep) = read_path(&shp).unwrap();
        let c = at(&ds);
        assert!((c.x - 2.335).abs() < 1e-6 && (c.y - 48.855).abs() < 1e-6, "{c:?}");
        assert!(rep.messages.is_empty(), "{:?}", rep.messages);
        assert_eq!(ds.value(0, 0), &Value::Text("paris".into()));

        // A .prj that isn't understood is said, not passed over.
        std::fs::write(dir.join("paris.prj"), r#"PROJCS["x",PROJECTION["Hotine_Oblique_Mercator"]]"#).unwrap();
        let (ds, rep) = read_path(&shp).unwrap();
        assert_eq!(at(&ds).x, 451_219.33);
        assert!(rep.messages.iter().any(|m| m.contains(".prj") && m.contains("Hotine")), "{:?}", rep.messages);

        // No .prj: read as longitude/latitude, with the out-of-range warning.
        std::fs::remove_file(dir.join("paris.prj")).unwrap();
        let (_, rep) = read_path(&shp).unwrap();
        assert!(rep.messages.iter().any(|m| m.contains("not longitude/latitude")), "{:?}", rep.messages);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A shape with a coordinate that isn't a number (NaN, as some writers mark an empty point, or
    /// an infinity) keeps its attributes without its geometry, counted (#1985), with or without a
    /// `.prj` to convert from, and isn't taken for a failed conversion. A clean file says nothing.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn shapefile_coordinates_that_arent_numbers_are_left_out_and_said() {
        use shapefile::dbase::{FieldValue, Record, TableWriterBuilder};
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let write = |name: &str, points: &[(f64, f64)]| {
            let shp = dir.join(name);
            let table = TableWriterBuilder::new().add_character_field("name".try_into().unwrap(), 10);
            let mut w = shapefile::Writer::from_path(&shp, table).unwrap();
            for (i, &(x, y)) in points.iter().enumerate() {
                let mut r = Record::default();
                r.insert("name".into(), FieldValue::Character(Some(format!("p{i}"))));
                w.write_shape_and_record(&shapefile::Point::new(x, y), &r).unwrap();
            }
            shp
        };
        let said = |m: &[crate::loss::Note]| m.iter().any(|m| m.contains("isn't a number"));
        let bad = write("bad.shp", &[(f64::NAN, 20.0), (11.0, 21.0), (f64::INFINITY, 22.0)]);
        let (ds, rep) = read_path(&bad).unwrap();
        assert_eq!(ds.features.len(), 3, "the features are kept");
        assert!(ds.features[0].geometry.is_none() && ds.features[2].geometry.is_none());
        assert_eq!(ds.features[1].geometry, Some(Geometry::Point(Coord::new(11.0, 21.0))));
        assert_eq!(ds.value(0, 0), &Value::Text("p0".into()), "with their attributes");
        assert_eq!(ds.geometry_type, GeometryType::Point);
        assert_eq!(ds.unsaved_coordinates(), 0);
        assert!(
            rep.messages
                .says("bad: 2 shape(s) have a coordinate that isn't a number (NaN or infinite): their features are kept without geometry"),
            "{:?}",
            rep.messages
        );
        // Every shape so: still a point layer, not a table.
        let all = write("all.shp", &[(f64::NAN, f64::NAN)]);
        let (ds, rep) = read_path(&all).unwrap();
        assert_eq!(ds.geometry_type, GeometryType::Point);
        assert!(said(&rep.messages), "{:?}", rep.messages);
        // With a .prj to convert from, said as not a number, not as a failed conversion.
        let prj = r#"PROJCS["WGS_1984_UTM_Zone_31N",GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",3.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
        let utm = write("utm.shp", &[(f64::NAN, 5_411_550.2), (451_219.33, 5_411_550.2)]);
        std::fs::write(dir.join("utm.prj"), prj).unwrap();
        let (ds, rep) = read_path(&utm).unwrap();
        assert!(ds.features[0].geometry.is_none() && ds.features[1].geometry.is_some());
        assert!(said(&rep.messages), "{:?}", rep.messages);
        assert!(!rep.messages.iter().any(|m| m.contains("converted")), "{:?}", rep.messages);
        assert!(!ds.crs_incomplete);
        // A clean file says nothing of it.
        let clean = write("clean.shp", &[(10.0, 20.0)]);
        let (_, rep) = read_path(&clean).unwrap();
        assert!(!said(&rep.messages), "{:?}", rep.messages);
    }
    /// An ESRI .prj without an authority retains the national datum shift: Monte Mario in Milan
    /// (#807), and PRS92 in Manila, known by its Esri datum name (#2258). Positions are the
    /// corpus's PROJ 9.8.1 references for the row's WKT.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn shapefile_esri_national_datum() {
        use shapefile::dbase::{Record, TableWriterBuilder};
        let dir = tempfile::tempdir().unwrap();
        for code in ["3003", "3123"] {
            let row = include_str!("raster/esri_datum_corpus.tsv").lines().find(|l| l.starts_with(&format!("{code}\t"))).unwrap();
            let f: Vec<&str> = row.split('\t').collect();
            let [x, y, lon, lat] = [1, 2, 3, 4].map(|i| f[i].parse::<f64>().unwrap());
            let shp = dir.path().join(format!("{code}.shp"));
            {
                let mut writer = shapefile::Writer::from_path(&shp, TableWriterBuilder::new()).unwrap();
                writer.write_shape_and_record(&shapefile::Point::new(x, y), &Record::default()).unwrap();
            }
            std::fs::write(shp.with_extension("prj"), f[5]).unwrap();
            let (ds, report) = read_path(&shp).unwrap();
            let c = ds.in_lonlat().unwrap().features[0].geometry.as_ref().and_then(Geometry::label_point).unwrap();
            assert!((c.x - lon).abs() < 1e-7 && (c.y - lat).abs() < 1e-7, "EPSG:{code}: {c:?}");
            assert!(report.messages.is_empty(), "EPSG:{code}: {:?}", report.messages);
        }
    }

    /// Tiny gzip fixtures exercise the public byte/path readers, concatenation and corruption.
    fn gzip_csv(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn published_gzip_text_imports() {
        let bytes = gzip_csv(b"\xEF\xBB\xBFid|count\nA|12\n");
        let (table, report) = read_bytes("SUMMARY.DAT.GZ", &bytes).unwrap();
        assert_eq!(table.name, "SUMMARY");
        assert_eq!(table.geometry_type, GeometryType::None);
        assert_eq!(table.value(0, 1), &Value::Int(12));
        assert!(report.messages.is_empty());
        let long = format!("id|value\n{}|1\n", "A".repeat(70_000));
        let (table, report) = read_bytes("long.dat.gz", &gzip_csv(long.as_bytes())).unwrap();
        assert_eq!(table.value(0, 1), &Value::Int(1));
        assert!(report.messages.is_empty());
        let ambiguous = format!("label, description;value\n{};1\n", "A".repeat(70_000));
        let plain = read_bytes("ambiguous.csv", ambiguous.as_bytes()).unwrap().0;
        let zipped = read_bytes("ambiguous.csv.gz", &gzip_csv(ambiguous.as_bytes())).unwrap().0;
        assert_eq!(zipped.fields, plain.fields);
        assert_eq!(zipped.value(0, 1), &Value::Int(1));
        let mut members = gzip_csv(b"id,count\nA,12\n");
        members.extend(gzip_csv(b"B,13\n"));
        assert_eq!(read_bytes("counts.csv.gz", &members).unwrap().0.features.len(), 2);
        for n in [0, 1, members.len() - 1] {
            assert!(read_bytes("counts.csv.gz", &members[..n]).is_err(), "truncated at {n}");
        }
        let mut corrupt = gzip_csv(b"id,count\nA,12\n");
        let crc = corrupt.len() - 8;
        corrupt[crc] ^= 1;
        assert!(read_bytes("counts.csv.gz", &corrupt).is_err());
        // The checksum is also checked after records have streamed beyond the sniffed prefix.
        let mut late = gzip_csv(format!("id,count\n{}", "A,12\n".repeat(20_000)).as_bytes());
        let crc = late.len() - 8;
        late[crc] ^= 1;
        assert!(read_bytes("counts.csv.gz", &late).is_err());
        assert!(read_bytes("counts.tar.gz", &bytes).unwrap_err().to_string().contains("decompress"));
        let opts = CsvOptions { delimiter: Some(b'|'), ..Default::default() };
        assert_eq!(read_delimited_bytes("one.dat.gz", &gzip_csv(b"id|count\n"), &opts).unwrap().0.fields.len(), 2);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("counts.csv.gz");
        std::fs::write(&path, &members).unwrap();
        assert_eq!(read_path(&path).unwrap().0.features.len(), 2);
    }

    /// Short repeated cells cost more as owned cells than as compressed or expanded text.
    #[test]
    fn gzip_retained_cells_have_a_memory_budget() {
        let bytes = gzip_csv(format!("a,b,c,d\n{}", ",,,\n".repeat(1000)).as_bytes());
        assert!(bytes.len() < 100);
        let mut reader = csv::ReaderBuilder::new().from_reader(GzipText::new(bytes.as_slice()));
        let error = read_csv_records(&mut reader, "small", &CsvOptions::default(), &mut 4096, None).unwrap_err();
        assert!(error.to_string().contains("memory budget"), "{error}");
        let mut reader = csv::ReaderBuilder::new().from_reader(GzipText::new(bytes.as_slice()));
        let (table, report) = read_csv_records(&mut reader, "small", &CsvOptions::default(), &mut (1 << 20), None).unwrap();
        assert_eq!(table.features.len(), 1000);
        assert!(report.messages.is_empty());
        // Missing fields still become dataset attributes, so charge the complete header width.
        let header = (0..100).map(|i| format!("f{i}")).collect::<Vec<_>>().join(",");
        let bytes = gzip_csv(format!("{header}\n{}", "x\n".repeat(100)).as_bytes());
        let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(GzipText::new(bytes.as_slice()));
        let error = read_csv_records(&mut reader, "short", &CsvOptions::default(), &mut (64 << 10), None).unwrap_err();
        assert!(error.to_string().contains("memory budget"), "{error}");
    }

    /// Two nested gzip tables can each fit while their combined retained cells do not.
    #[test]
    fn zip_delimited_members_share_retained_budget() {
        let text = b"id,n\nA,1\n";
        let gzip = gzip_csv(text);
        let mut left = 1 << 20;
        read_delimited_bytes_budget("a.csv.gz", &gzip, &CsvOptions::default(), &mut left).unwrap();
        let cost = (1 << 20) - left;
        for second in ["b.csv.gz", "b.csv"] {
            let bytes = if second.ends_with(".gz") { gzip.clone() } else { text.to_vec() };
            let zip = crate::zip::write(&[("a.csv.gz".into(), gzip.clone()), (second.into(), bytes)]).unwrap();
            let limited = read_zip_budget("tables.zip", &zip, None, &mut { cost }).unwrap();
            assert_eq!(limited.len(), 1);
            assert!(limited[0].1.messages.iter().any(|m| m.contains(second) && m.contains("memory budget")));
            let clean = read_zip_budget("tables.zip", &zip, None, &mut (cost * 2)).unwrap();
            assert_eq!(clean.len(), 2);
            assert!(clean.iter().all(|(_, report)| report.messages.is_empty()));
        }
    }

    /// Cancellation while the CSV parser asks for more of a long DAT header stays cancellation.
    #[test]
    fn gzip_long_dat_header_preserves_cancellation() {
        use std::io::{Read, Write};
        use std::sync::{Arc, atomic::Ordering};
        struct HeaderRead {
            bytes: std::io::Cursor<Vec<u8>>,
            probe: Arc<crate::progress::tests::Counted>,
        }
        impl Read for HeaderRead {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let limit = buf.len().min(4096);
                let n = self.bytes.read(&mut buf[..limit])?;
                if self.bytes.position() >= 96 << 10 {
                    self.probe.cancel.store(true, Ordering::Relaxed);
                }
                Ok(n)
            }
        }
        // Stored blocks ensure decoding the first 64 KiB cannot consume the cancellation point.
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::none());
        encoder.write_all(format!("{},value\nA,1\n", "h".repeat(200_000)).as_bytes()).unwrap();
        let probe = Arc::new(crate::progress::tests::Counted::default());
        let reader = HeaderRead { bytes: std::io::Cursor::new(encoder.finish().unwrap()), probe: probe.clone() };
        let _guard = crate::progress::install(Some(probe.clone()));
        assert!(matches!(read_gzip_text(reader, "long.dat.gz", &CsvOptions::default()), Err(DataError::Cancelled)));
        assert!(probe.cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn gzip_expansion_is_bounded_and_cancellable() {
        use std::io::Read;
        let bytes = gzip_csv(b"id,count\nA,12\n");
        let mut limited = GzipText::new(bytes.as_slice());
        limited.left = 4;
        assert!(limited.read_to_end(&mut Vec::new()).is_err());
        assert_eq!(gzip_text_prefix(bytes.as_slice(), 2).unwrap(), b"id");
        let probe = crate::progress::tests::Counted::cancelling_after(0);
        let _guard = crate::progress::install(Some(probe));
        assert!(matches!(read_bytes("counts.csv.gz", &bytes), Err(DataError::Cancelled)));
    }

    #[test]
    fn published_dat_and_gtfs_imports() {
        let (table, report) = read_bytes("summary.dat", b"id|count\nA|12\n").unwrap();
        assert_eq!(table.geometry_type, GeometryType::None);
        assert_eq!(table.value(0, 1), &Value::Int(12));
        assert!(report.messages.is_empty());
        assert_eq!(read_bytes("quoted.dat", b"id|name\nA|\"Main|First\"\n").unwrap().0.value(0, 1), &Value::Text("Main|First".into()));
        assert!(read_bytes("binary.dat", b"\0\x01\x02").unwrap_err().to_string().contains("rename"));
        let zip = crate::zip::write(&[
            ("feed/stops.txt".into(), b"stop_id,stop_name,stop_lat,stop_lon\ns1,\"Main, First\",35.1,-89.9\n".to_vec()),
            ("feed/routes.txt".into(), b"route_id,route_short_name\nr1,Main\n".to_vec()),
            ("feed/transfers.txt".into(), b"from_stop_id,to_stop_id,transfer_type\n".to_vec()),
            ("README.txt".into(), b"A transit feed.\nRead the documentation.\n".to_vec()),
        ])
        .unwrap();
        let layers = read_zip("transit.zip", &zip).unwrap();
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[2].0.name, "transfers");
        assert!(layers[2].0.features.is_empty());
        assert_eq!(layers[2].0.fields.len(), 3);
        assert_eq!(layers[0].0.name, "routes");
        assert_eq!(layers[0].0.geometry_type, GeometryType::None);
        assert_eq!(layers[1].0.features[0].geometry, Some(Geometry::Point(Coord::new(-89.9, 35.1))));
        assert!(layers[0].1.messages.iter().any(|m| m.contains("README.txt")));
        let readme = crate::zip::write(&[
            ("nested/README.txt".into(), b"README\nA transit feed.\nRead the documentation.\n".to_vec()),
            ("ids.txt".into(), b"id\nA\n".to_vec()),
        ])
        .unwrap();
        let layers = read_zip("readme.zip", &readme).unwrap();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].0.name, "ids");
        assert!(layers[0].1.messages.iter().any(|m| m.contains("nested/README.txt")));
        let clean = crate::zip::write(&[("ids.txt".into(), b"id\nA\n".to_vec())]).unwrap();
        let layers = read_zip("ids.zip", &clean).unwrap();
        assert_eq!(layers[0].0.value(0, 0), &Value::Text("A".into()));
        assert!(layers[0].1.messages.is_empty());
        let ragged =
            crate::zip::write(&[("ids.txt".into(), b"id\nA\n".to_vec()), ("ragged.txt".into(), b"id,n\nA,1,extra\n".to_vec())]).unwrap();
        let layers = read_zip("ragged.zip", &ragged).unwrap();
        assert!(layers[0].1.messages.iter().any(|m| m.contains("ragged.txt") && m.contains("extract")));
    }

    /// A supplied ZIP CRS also places coordinates from its delimited members.
    #[test]
    fn zip_text_tables_use_the_supplied_crs() {
        let bytes = b"id,easting,northing\nA,500000,4649776.22482\n";
        let zip = crate::zip::write(&[("projected.txt".into(), bytes.to_vec()), ("projected.csv.gz".into(), gzip_csv(bytes))]).unwrap();
        let crs = Crs::from_epsg(32633).unwrap();
        let layers = read_zip_in("projected.zip", &zip, Some(crs)).unwrap();
        assert_eq!(layers.len(), 2);
        for (table, report) in layers {
            let point = table.in_lonlat().unwrap().features[0].geometry.as_ref().unwrap().label_point().unwrap();
            assert!((point.x - 15.0).abs() < 1e-6 && (point.y - 42.0).abs() < 1e-6, "{point:?}");
            assert_eq!(table.crs, Some(crs));
            assert!(report.messages.is_empty(), "{:?}", report.messages);
        }
    }

    #[test]
    fn zipped_geodatabase_has_actionable_refusal() {
        let zip = crate::zip::write(&[("County.GDB/a00000001.gdbtable".into(), vec![0])]).unwrap();
        let error = read_zip("county.zip", &zip).err().unwrap().to_string();
        assert!(error.contains("unzip") && error.contains(".gdb folder"), "{error}");
        let mut entries = vec![("county.gdb/a00000001.gdbtable".into(), vec![0]), ("counts.csv".into(), b"id,n\nA,12\n".to_vec())];
        entries.extend((0..21).map(|i| (format!("bad{i}.csv.gz"), vec![0])));
        let mixed = crate::zip::write(&entries).unwrap();
        let layers = read_zip("mixed.zip", &mixed).unwrap();
        assert!(layers[0].1.messages.iter().any(|m| m.contains(".gdb folder")));
    }

    /// A DBF in another folder is a table, even with the Shapefile's base name. Both read;
    /// paired sidecars and resource forks never become extra tables.
    #[test]
    fn zipped_standalone_dbf_opens_as_table() {
        let mut ds = Dataset::new("places", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        ds.push_next(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Text("one".into())]);
        ds.push_next(Some(Geometry::Point(Coord::new(3.0, 4.0))), vec![Value::Text("two".into())]);
        let (parts, lost) = shapefile_parts(&ds).unwrap();
        assert_eq!(lost, 0);
        let dbf = parts.iter().find(|(e, _)| *e == "dbf").unwrap().1.clone();
        let mut entries: Vec<_> = parts.into_iter().map(|(e, b)| (format!("shapes/Places.{}", e.to_ascii_uppercase()), b)).collect();
        // Pairing ignores case, including the folder name.
        entries.iter_mut().find(|(n, _)| n.ends_with(".DBF")).unwrap().0 = "SHAPES/places.dbf".into();
        let clean = read_zip("county.zip", &crate::zip::write(&entries).unwrap()).unwrap();
        assert_eq!(clean.len(), 1);
        assert_eq!(clean[0].0.features.len(), 2);
        assert!(clean[0].1.messages.is_empty(), "{:?}", clean[0].1.messages);

        entries.push(("tables/Places.DBF".into(), dbf.clone()));
        entries.push(("__MACOSX/._table.dbf".into(), dbf.clone()));
        let mixed = read_zip("county.zip", &crate::zip::write(&entries).unwrap()).unwrap();
        assert_eq!(mixed.len(), 2);
        assert_eq!(mixed[0].0.geometry_type, GeometryType::Point);
        assert_eq!(mixed[1].0.geometry_type, GeometryType::None);
        assert_eq!(mixed[1].0.features.len(), 2);
        assert_eq!(mixed[1].0.source.as_deref(), Some("county.zip/tables/Places.DBF"));
        assert_eq!(mixed[1].0.value(1, 0), &Value::Text("two".into()));
        assert_eq!(mixed[0].0.features.len(), 2);
        assert_eq!(mixed[0].0.value(1, 0), &Value::Text("two".into()));
        assert!(mixed.iter().all(|(_, r)| r.messages.is_empty()));
        // A second `.dbf` beside the Shapefile, its name differing only in case, isn't its table.
        entries.push(("shapes/PLACES.dbf".into(), dbf.clone()));
        let twice = read_zip("county.zip", &crate::zip::write(&entries).unwrap()).unwrap();
        assert!(
            twice[0].1.messages.iter().any(|m| m.contains("shapes/PLACES.dbf (an extra .dbf sidecar of a Shapefile)")),
            "{:?}",
            twice[0].1.messages
        );
        // Tables count toward the same 32-item budget as layers.
        let entries: Vec<_> = (0..33).map(|i| (format!("tables/table{i:02}.dbf"), dbf.clone())).collect();
        let limited = read_zip("tables.zip", &crate::zip::write(&entries).unwrap()).unwrap();
        assert_eq!(limited.len(), MAX_ZIP_LAYERS);
        assert_eq!(limited[0].1.messages, ["tables.zip: skipped tables/table32.dbf (past the 32-layer limit)"]);
    }

    /// A small file can expand into many owned rows; both loose and zipped tables must fit
    /// the retained-memory allowance, shared with other archive tables.
    #[test]
    fn standalone_dbf_rows_share_the_zip_memory_budget() {
        let mut ds = Dataset::new("small", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        for _ in 0..32 {
            ds.push_next(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Text("one".into())]);
        }
        let (parts, _) = shapefile_parts(&ds).unwrap();
        let bytes = &parts.iter().find(|(ext, _)| *ext == "dbf").unwrap().1;
        let mut left = 1 << 20;
        let (table, report) = read_dbf(bytes.clone(), None, "small", &mut left).unwrap();
        assert_eq!(table.features.len(), 32);
        assert!(report.messages.is_empty());
        let cost = (1 << 20) - left;
        let error = read_dbf(bytes.clone(), None, "small", &mut (cost - 1)).unwrap_err();
        assert!(matches!(error, DataError::Format(_)) && error.to_string().contains("memory budget"), "{error}");
        let zip = crate::zip::write(&[("a.dbf".into(), bytes.clone()), ("b.dbf".into(), bytes.clone())]).unwrap();
        let limited = read_zip_budget("tables.zip", &zip, None, &mut { cost }).unwrap();
        assert_eq!(limited.len(), 1);
        assert!(limited[0].1.messages.iter().any(|m| m.contains("b.dbf") && m.contains("memory budget")));
        let clean = read_zip_budget("tables.zip", &zip, None, &mut (cost * 2)).unwrap();
        assert_eq!(clean.len(), 2);
        assert!(clean.iter().all(|(_, report)| report.messages.is_empty()));
    }

    /// An oversized Shapefile part names the failing file and advises keeping the parts together.
    #[test]
    fn oversized_shapefile_part_reports_whole_dataset_advice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roads.shp");
        std::fs::File::create(&path).unwrap();
        std::fs::File::create(path.with_extension("dbf")).unwrap();
        let limit = (max_bytes() >> 20).to_string();
        for ext in ["shp", "dbf", "shx"] {
            let sidecar = path.with_extension(ext);
            let file = std::fs::File::create(&sidecar).unwrap();
            file.set_len(max_bytes() + 1).unwrap();
            for result in [read_path(&path), read_shapefile_in(&path, "roads", Some(Crs::LonLat))] {
                let error = result.unwrap_err().to_string();
                assert_eq!(file_size_error_args(&error), Some((sidecar.to_str().unwrap(), limit.as_str(), FileSizeAdvice::Shapefile)));
                assert!(error.contains("Split the whole Shapefile"), "{error}");
            }
            file.set_len(0).unwrap();
        }
    }

    #[test]
    fn oversized_standalone_dbf_reports_limit_and_table_advice() {
        // A sparse file exercises the production cap without allocating or reading its contents.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("communities.DBF");
        std::fs::File::create(&path).unwrap().set_len(max_bytes() + 1).unwrap();

        let error = read_path(&path).unwrap_err().to_string();
        let limit = (max_bytes() >> 20).to_string();
        assert_eq!(file_size_error_args(&error), Some((path.to_str().unwrap(), limit.as_str(), FileSizeAdvice::Split)));
        assert!(error.contains("Split the data into smaller files"), "{error}");
    }

    /// Standalone tables share Shapefile typing and loss notices; .cpg wins over the header,
    /// including when loose files are gathered for a browser upload.
    #[test]
    fn standalone_dbf_keeps_typed_rows_and_cpg() {
        use shapefile::dbase::{FieldValue, Record, TableWriterBuilder};
        use std::io::Cursor;
        let mut bytes = Cursor::new(Vec::new());
        let table = TableWriterBuilder::with_encoding(shapefile::dbase::encoding::EncodingRs::from(encoding_rs::WINDOWS_1252))
            .add_character_field("name".try_into().unwrap(), 12)
            .add_numeric_field("count".try_into().unwrap(), 8, 0)
            .add_numeric_field("amount".try_into().unwrap(), 12, 2)
            .add_logical_field("active".try_into().unwrap())
            .add_date_field("day".try_into().unwrap());
        {
            let mut writer = table.build_with_dest(&mut bytes);
            let mut row = Record::default();
            row.insert("name".into(), FieldValue::Character(Some("Málaga".into())));
            row.insert("count".into(), FieldValue::Numeric(Some(7.0)));
            // A whole value with decimals declared still has Double type.
            row.insert("amount".into(), FieldValue::Numeric(Some(2.0)));
            row.insert("active".into(), FieldValue::Logical(Some(true)));
            row.insert("day".into(), FieldValue::Date(Some(shapefile::dbase::Date::new(14, 11, 2026).unwrap())));
            writer.write_record(&row).unwrap();
            writer.finalize().unwrap();
        }
        let mut bytes = bytes.into_inner();
        bytes[29] = 0; // No language driver: the .cpg must be used.
        let dir = std::env::temp_dir().join(format!("rustgis-lone-dbf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("communities.DBF");
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(path.with_extension("cPg"), b"1252").unwrap();
        let (ds, report) = read_path(&path).unwrap();
        assert_eq!(ds.geometry_type, GeometryType::None);
        assert_eq!(ds.features.len(), 1);
        assert_eq!(
            ds.fields.iter().map(|f| f.ty).collect::<Vec<_>>(),
            [FieldType::Text, FieldType::Integer, FieldType::Double, FieldType::Boolean, FieldType::Date]
        );
        assert_eq!(
            ds.features[0].attrs,
            [
                Value::Text("Málaga".into()),
                Value::Int(7),
                Value::Float(2.0),
                Value::Bool(true),
                Value::Date(crate::date::Date::from_ymd(2026, 11, 14).unwrap())
            ]
        );
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        let (uploads, notes) =
            gather_shapefiles(vec![("communities.DBF".into(), bytes.clone()), ("communities.CPG".into(), b"1252".to_vec())]);
        assert!(notes.is_empty());
        assert_eq!(uploads.len(), 1);
        let read = read_layers(&uploads[0].0, &uploads[0].1).unwrap();
        assert_eq!(read[0].dataset.features, ds.features);
        assert!(read[0].report.messages.is_empty());
        // Unknown metadata and an invalid date are counted just as in Shapefile attributes.
        let offset = u16::from_le_bytes([bytes[8], bytes[9]]) as usize + 1 + 12 + 8 + 12 + 1;
        bytes[offset..offset + 8].copy_from_slice(b"20260230");
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(path.with_extension("cPg"), b"unknown-page").unwrap();
        let (bad, report) = read_path(&path).unwrap();
        assert_eq!(bad.value(0, 4), &Value::Null);
        assert!(report.messages.iter().any(|m| m.contains(".cpg")), "{:?}", report.messages);
        assert!(report.messages.iter().any(|m| m.contains("1") && m.contains("day")), "{:?}", report.messages);
        // A cut-short record must stop with an error; dbase can repeat the same I/O error.
        bytes.truncate(u16::from_le_bytes([bytes[8], bytes[9]]) as usize + 2);
        assert!(read_bytes("short.dbf", &bytes).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn zipped_orphan_mid_is_named_in_notice() {
        let mut entries = vec![("counts.csv".into(), b"id,n\nA,12\n".to_vec())];
        let clean = read_zip("map.zip", &crate::zip::write(&entries).unwrap()).unwrap();
        assert!(clean[0].1.messages.is_empty());
        entries.push(("tables/Addresses.MID".into(), b"1,Main St".to_vec()));
        let mixed = read_zip("map.zip", &crate::zip::write(&entries).unwrap()).unwrap();
        assert_eq!(mixed[0].1.messages, ["map.zip: skipped tables/Addresses.MID (no .mif beside it)"]);
        let error = read_zip("map.zip", &crate::zip::write(&entries[1..]).unwrap()).unwrap_err().to_string();
        assert!(error.contains("tables/Addresses.MID (no .mif beside it)"), "{error}");
    }

    /// Zipped Shapefiles are read from memory (the web app's way to open them), with their `.prj`,
    /// from folders inside the archive, beside other vector files; truncations are errors.
    #[test]
    fn zipped_shapefiles() {
        use shapefile::dbase::{FieldValue, Record, TableWriterBuilder};
        use std::io::Cursor;
        let (mut shp, mut shx, mut dbf) = (Cursor::new(Vec::new()), Cursor::new(Vec::new()), Cursor::new(Vec::new()));
        {
            let table = TableWriterBuilder::new().add_character_field("name".try_into().unwrap(), 10);
            let mut w = shapefile::Writer::new(shapefile::ShapeWriter::with_shx(&mut shp, &mut shx), table.build_with_dest(&mut dbf));
            let mut r = Record::default();
            r.insert("name".into(), FieldValue::Character(Some("paris".into())));
            w.write_shape_and_record(&shapefile::Point::new(451_219.33, 5_411_550.20), &r).unwrap();
        }
        let prj = r#"PROJCS["WGS_1984_UTM_Zone_31N",GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",3.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
        let geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"n":1},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#;
        let entries = vec![
            ("data/Paris.SHP".to_string(), shp.into_inner()),
            ("data/Paris.shx".to_string(), shx.into_inner()),
            ("data/Paris.dbf".to_string(), dbf.into_inner()),
            ("data/Paris.prj".to_string(), prj.as_bytes().to_vec()),
            ("__MACOSX/data/._Paris.shp".to_string(), vec![0; 10]),
            ("extra/points.geojson".to_string(), geojson.as_bytes().to_vec()),
            ("ne_VERSION.txt".to_string(), b"5.1.1".to_vec()),
        ];
        let zip = crate::zip::write(&entries).unwrap();
        let layers = read_zip("upload.zip", &zip).unwrap();
        assert_eq!(layers.len(), 2);
        let (ds, rep) = &layers[0];
        assert_eq!(ds.name, "Paris");
        let c = ds.in_lonlat().unwrap().features[0].geometry.as_ref().and_then(Geometry::label_point).unwrap();
        assert!((c.x - 2.335).abs() < 1e-6 && (c.y - 48.855).abs() < 1e-6, "{c:?}");
        assert!(rep.messages.iter().any(|m| m.contains("ne_VERSION.txt")), "{:?}", rep.messages);
        assert_eq!(ds.value(0, 0), &Value::Text("paris".into()));
        assert_eq!(layers[1].0.name, "points");
        // read_bytes takes the first layer.
        assert_eq!(read_bytes("upload.zip", &zip).unwrap().0.name, "Paris");
        // Without its .dbf the Shapefile is skipped (and said); the rest of the archive is read.
        let no_dbf: Vec<_> = entries.iter().filter(|(n, _)| !n.ends_with(".dbf")).cloned().collect();
        let layers = read_zip("x.zip", &crate::zip::write(&no_dbf).unwrap()).unwrap();
        assert_eq!(layers.len(), 1);
        assert!(layers[0].1.messages.iter().any(|m| m.contains("no .dbf")), "{:?}", layers[0].1.messages);
        // A metadata file that isn't GeoJSON doesn't keep the Shapefile out.
        let mut with_meta = entries.clone();
        with_meta.push(("data/metadata.json".to_string(), br#"{"title": "not GeoJSON"}"#.to_vec()));
        let layers = read_zip("x.zip", &crate::zip::write(&with_meta).unwrap()).unwrap();
        assert_eq!(layers.len(), 2);
        assert!(layers[0].1.messages.iter().any(|m| m.contains("metadata.json")), "{:?}", layers[0].1.messages);
        // A vector tile set's .pbf files aren't read as OpenStreetMap extracts (nor said).
        let mut with_tiles = entries.clone();
        with_tiles.extend((0..3).map(|i| (format!("tiles/3/{i}/1.pbf"), vec![0x1a, 2, 0, 0])));
        let layers = read_zip("x.zip", &crate::zip::write(&with_tiles).unwrap()).unwrap();
        assert_eq!(layers.len(), 2);
        assert!(layers[0].1.messages.iter().all(|m| !m.contains(".pbf")), "{:?}", layers[0].1.messages);
        // Nothing readable at all is an error.
        let only_shp: Vec<_> = entries.iter().filter(|(n, _)| n.ends_with(".SHP")).cloned().collect();
        assert!(read_zip("x.zip", &crate::zip::write(&only_shp).unwrap()).is_err());
        // Nothing to read.
        assert!(read_zip("x.zip", &crate::zip::write(&[("a.txt.bak".to_string(), vec![1])]).unwrap()).is_err());
        for n in (0..zip.len()).step_by(37) {
            let _ = read_zip("x.zip", &zip[..n]);
        }
        // A .shp alone is refused with a hint to zip it.
        assert!(read_bytes("x.shp", &entries[0].1).is_err());
        // Shapefiles past the layer limit are named, not dropped unsaid; at the limit nothing is said.
        let shapefiles = |count: usize| -> Vec<(String, Vec<u8>)> {
            (0..count).flat_map(|i| entries.iter().take(4).map(move |(n, b)| (format!("s{i:02}.{}", ext(n)), b.clone()))).collect()
        };
        fn ext(n: &str) -> String {
            n.rsplit('.').next().unwrap_or_default().to_ascii_lowercase()
        }
        let layers = read_zip("x.zip", &crate::zip::write(&shapefiles(MAX_ZIP_LAYERS + 2)).unwrap()).unwrap();
        assert_eq!(layers.len(), MAX_ZIP_LAYERS);
        assert_eq!(layers[0].1.messages, ["x.zip: skipped s32.shp (past the 32-layer limit); s33.shp (past the 32-layer limit)"]);
        let layers = read_zip("x.zip", &crate::zip::write(&shapefiles(MAX_ZIP_LAYERS)).unwrap()).unwrap();
        assert_eq!(layers.len(), MAX_ZIP_LAYERS);
        assert!(layers.iter().all(|(_, r)| r.messages.is_empty()), "{:?}", layers[0].1.messages);
        // MapInfo files past the limit are named too (by the loop over the other members, which
        // they're left to), once each.
        let mif = b"Version 300\nColumns 1\n  id Integer\nData\nPoint 1 2\n".to_vec();
        let mifs = |count: usize| -> Vec<(String, Vec<u8>)> {
            (0..count).flat_map(|i| [(format!("m{i:02}.mif"), mif.clone()), (format!("m{i:02}.mid"), b"1\n".to_vec())]).collect()
        };
        let layers = read_zip("x.zip", &crate::zip::write(&mifs(MAX_ZIP_LAYERS + 1)).unwrap()).unwrap();
        assert_eq!(layers.len(), MAX_ZIP_LAYERS);
        assert_eq!(layers[0].1.messages, ["x.zip: skipped m32.mif (past the 32-layer limit)"]);
        let layers = read_zip("x.zip", &crate::zip::write(&mifs(MAX_ZIP_LAYERS)).unwrap()).unwrap();
        assert!(layers.iter().all(|(_, r)| r.messages.is_empty()), "{:?}", layers[0].1.messages);
        // A long list of skipped members names the first ones and counts the rest.
        let layers = read_zip("x.zip", &crate::zip::write(&mifs(MAX_ZIP_LAYERS + 25)).unwrap()).unwrap();
        let note = &layers[0].1.messages[0];
        assert!(note.contains("m51.mif") && !note.contains("m52.mif") && note.ends_with("; and 5 more"), "{note:?}");
    }

    /// A Shapefile's loose files picked or dropped together are zipped into one, which reads as a
    /// layer; parts of different Shapefiles stay apart, and other files pass through.
    #[test]
    fn loose_shapefiles_gather_into_zips() {
        let (ds, _) = read_geojson(
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"n":"a"},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
            "rivers",
        )
        .unwrap();
        let loose = |stem: &str, upper: bool| -> Vec<(String, Vec<u8>)> {
            let zip = write_bytes(&ds, "zip").unwrap().0;
            crate::zip::read(&zip)
                .unwrap()
                .into_iter()
                .map(|(n, b)| {
                    let ext = Path::new(&n).extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
                    (if upper && ext == "shp" { format!("{}.SHP", stem.to_uppercase()) } else { format!("{stem}.{ext}") }, b)
                })
                .collect()
        };
        let names = |files: &[(String, Vec<u8>)]| files.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        let rivers = loose("rivers", false);
        assert!(rivers.len() >= 4, "{:?}", names(&rivers));
        // One Shapefile becomes one zip, which reads.
        let out = gather_shapefiles(rivers.clone()).0;
        assert_eq!(names(&out), ["rivers.zip"]);
        let (read, _) = read_bytes(&out[0].0, &out[0].1).unwrap();
        assert_eq!((read.name.as_str(), read.features.len()), ("rivers", 1));
        // Two in one pick, in any order, with other files passed through in place.
        let mut two = vec![("cities.geojson".to_string(), b"{}".to_vec())];
        two.extend(loose("roads", false).into_iter().rev());
        two.extend(rivers.clone());
        let out = gather_shapefiles(two).0;
        assert_eq!(names(&out), ["cities.geojson", "roads.zip", "rivers.zip"]);
        for (n, b) in &out[1..] {
            let layers = read_zip(n, b).unwrap();
            assert_eq!(layers.len(), 1, "{n}");
        }
        // The base names match without regard to case.
        let out = gather_shapefiles(loose("rivers", true)).0;
        assert_eq!(names(&out), ["RIVERS.zip"]);
        assert_eq!(read_bytes(&out[0].0, &out[0].1).unwrap().0.features.len(), 1);
        // A .shp without its .dbf is left as it is, and explained when read.
        let no_dbf: Vec<_> = rivers.iter().filter(|(n, _)| !n.ends_with(".dbf")).cloned().collect();
        let out = gather_shapefiles(no_dbf.clone()).0;
        assert_eq!(names(&out), names(&no_dbf));
        let shp = out.iter().find(|(n, _)| n.ends_with(".shp")).unwrap();
        assert!(read_bytes(&shp.0, &shp.1).unwrap_err().to_string().contains("together with its .dbf"));
        assert!(read_bytes("rivers.prj", b"").unwrap_err().to_string().contains("together with its .shp"));
        // Parts of another Shapefile aren't taken in.
        let mut mixed = rivers.clone();
        mixed.push(("lakes.prj".to_string(), b"x".to_vec()));
        assert_eq!(names(&gather_shapefiles(mixed).0), ["rivers.zip", "lakes.prj"]);
        assert!(gather_shapefiles(Vec::new()).0.is_empty());
        // One too large to read together is left out and said why; the others still come.
        let mut big = rivers.clone();
        big.push(("huge.shp".to_string(), vec![0; 1 << 20]));
        big.push(("huge.dbf".to_string(), vec![0; BYTES_FLOOR as usize]));
        let (out, problems) = with_max_bytes(BYTES_FLOOR, || gather_shapefiles(big));
        assert_eq!(names(&out), ["rivers.zip"]);
        assert_eq!(problems.len(), 1);
        // Its size and the limit it passed, the size read into memory.
        assert!(problems[0].starts_with("huge.shp: its data files together: file is too large (257 MiB;"), "{problems:?}");
        assert!(problems[0].contains("the most read into memory is 256 MiB"), "{problems:?}");
    }

    /// A Shapefile's parts by the address of its `.shp` (#1238): beside it, in its case, the query
    /// kept; fetched, they zip into one that reads, and without the `.dbf` they don't.
    #[test]
    fn shapefile_parts_by_address() {
        let urls = |u: &str| shapefile_part_urls(u).map(|v| v.into_iter().map(|(_, u)| u).collect::<Vec<_>>());
        assert_eq!(
            urls("https://geo.example.org/PI/TERESINA/HIDROGRAFIA/PI_2211001_RIOS_SIMPLES.shp").unwrap(),
            ["dbf", "shx", "prj", "cpg"].map(|e| format!("https://geo.example.org/PI/TERESINA/HIDROGRAFIA/PI_2211001_RIOS_SIMPLES.{e}"))
        );
        assert_eq!(urls("https://x.org/a/RIOS.SHP?token=1#f").unwrap()[0], "https://x.org/a/RIOS.DBF?token=1#f");
        assert_eq!(urls("s3://bucket/rios.shp").unwrap()[2], "s3://bucket/rios.prj");
        let wrapped = "rustgis-s3:0123456789abcdef:18:https://minio.test:s3://bucket/rios.shp";
        assert_eq!(urls(wrapped).unwrap()[0], "rustgis-s3:0123456789abcdef:18:https://minio.test:s3://bucket/rios.dbf");
        for not_shp in [
            "https://x.org/a.zip",
            "https://x.org/a.shp.zip",
            "https://x.org/.shp",
            "https://x.org/a?f=x.shp",
            "https://x.org.shp",
            "s3://bucket.shp",
            "rustgis-s3:0123456789abcdef:18:https://minio.test:s3://bucket.shp",
            "rios.shp",
            "",
        ] {
            assert!(urls(not_shp).is_none(), "{not_shp}");
        }
        let (ds, _) = read_geojson(
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"n":"a"},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
            "rios",
        )
        .unwrap();
        let (parts, _) = shapefile_parts(&ds).unwrap();
        let shp = parts.iter().find(|(e, _)| *e == "shp").unwrap().1.clone();
        let others: Vec<(&str, Vec<u8>)> = parts.iter().filter(|(e, _)| *e != "shp").map(|(e, b)| (*e, b.clone())).collect();
        let zip = zip_shapefile_parts("RIOS_SIMPLES.shp", shp.clone(), others.clone()).unwrap();
        let (read, _) = read_bytes("RIOS_SIMPLES.zip", &zip).unwrap();
        assert_eq!((read.name.as_str(), read.features.len()), ("RIOS_SIMPLES", 1));
        assert!(shapefile_zip_lacks(&zip).is_empty());
        let no_prj: Vec<_> = others.iter().filter(|(e, _)| !matches!(*e, "prj" | "cpg")).cloned().collect();
        let lacks = |z: &[u8]| shapefile_zip_lacks(z).into_iter().map(|(n, e)| format!("{n}: {}", e.join(" "))).collect::<Vec<_>>();
        assert_eq!(lacks(&zip_shapefile_parts("RIOS_SIMPLES.shp", shp.clone(), no_prj).unwrap()), ["RIOS_SIMPLES.shp: prj cpg"]);
        assert!(shapefile_zip_lacks(&shp).is_empty(), "not a zip");
        // Another file's .prj isn't this one's.
        let mixed: Vec<(String, Vec<u8>)> =
            [("roads.shp", &shp), ("roads.dbf", &shp), ("roads.shx", &shp), ("roads.cpg", &shp), ("other.prj", &shp)]
                .map(|(n, b)| (n.to_string(), b.clone()))
                .into();
        assert_eq!(lacks(&crate::zip::write(&mixed).unwrap()), ["roads.shp: prj"]);
        // A stray .shp without its .dbf, and a macOS resource fork, aren't read, so they lack nothing.
        let mut whole: Vec<(String, Vec<u8>)> =
            ["roads.shp", "roads.dbf", "roads.shx", "roads.prj", "roads.cpg", "stray.shp", "__MACOSX/._roads.shp"]
                .map(|n| (n.to_string(), shp.clone()))
                .into();
        assert!(shapefile_zip_lacks(&crate::zip::write(&whole).unwrap()).is_empty());
        whole.retain(|(n, _)| n != "roads.prj");
        assert_eq!(lacks(&crate::zip::write(&whole).unwrap()), ["roads.shp: prj"]);
        // Each Shapefile for itself: one with its .prj, one without.
        let two: Vec<(String, Vec<u8>)> =
            ["a.shp", "a.dbf", "a.shx", "a.prj", "a.cpg", "b.shp", "b.dbf", "b.shx", "b.cpg"].map(|n| (n.to_string(), shp.clone())).into();
        assert_eq!(lacks(&crate::zip::write(&two).unwrap()), ["b.shp: prj"]);
        // As many as the reader reads: of 40 without a .prj, the first 32 by name.
        let many: Vec<(String, Vec<u8>)> =
            (0..40).flat_map(|i| ["shp", "dbf", "shx", "cpg"].map(|e| (format!("s{i:02}.{e}"), shp.clone()))).collect();
        let said = shapefile_zip_lacks(&crate::zip::write(&many).unwrap());
        assert_eq!((said.len(), said[0].0.as_str(), said[31].0.as_str()), (MAX_ZIP_LAYERS, "s00.shp", "s31.shp"));
        let no_dbf: Vec<_> = others.into_iter().filter(|(e, _)| *e != "dbf").collect();
        assert!(zip_shapefile_parts("RIOS_SIMPLES.shp", shp, no_dbf).unwrap_err().contains(".dbf"));
    }

    /// Typed datasets can retain incompatible cells or fractional values from dynamic sources.
    #[test]
    fn shapefile_notes_cover_incompatible_and_rounded_cells() {
        let cases = [
            ("integer text", FieldType::Integer, Value::Text("n/a".into()), false),
            ("double NaN", FieldType::Double, Value::Float(f64::NAN), false),
            ("double infinity", FieldType::Double, Value::Float(f64::INFINITY), false),
            ("boolean text", FieldType::Boolean, Value::Text("abc".into()), false),
            ("integer rounding", FieldType::Integer, Value::Float(2.5), true),
            ("double rounding", FieldType::Double, Value::Float(2.55), true),
            ("positive half", FieldType::Double, Value::Float(0.5), true),
            ("negative half", FieldType::Double, Value::Float(-0.5), true),
        ];
        let mut missing = Vec::new();
        for (name, ty, value, rounded) in cases {
            let mut ds = Dataset::new("cells", GeometryType::Point, vec![Field::new("value", ty)]);
            ds.push(Some(Geometry::Point(Coord::new(1., 2.))), vec![value.clone()]);
            if name == "double rounding" {
                // The largest whole part leaves one decimal place in this column.
                ds.push(Some(Geometry::Point(Coord::new(3., 4.))), vec![Value::Float(1e20)]);
            }
            if name.ends_with("half") {
                // A 22-digit whole part forces zero decimal places; ties round to even.
                ds.push(Some(Geometry::Point(Coord::new(3., 4.))), vec![Value::Float(1e21)]);
            }
            let (bytes, _) = write_bytes(&ds, "zip").unwrap();
            let (back, _) = read_bytes("cells.zip", &bytes).unwrap();
            if rounded {
                assert_ne!(back.features[0].attrs[0].as_f64(), value.as_f64(), "{name}");
            } else {
                assert_eq!(back.features[0].attrs[0], Value::Null, "{name}");
            }
            let notes = shapefile_values_notes(&ds, "cells.zip");
            let kind = if rounded { "rounded" } else { "left empty" };
            if !notes.iter().any(|n| n.contains(kind) && n.contains("1 ") && n.contains("value")) {
                missing.push(name);
            }
            ds.features[0].attrs = vec![Value::Null];
            assert!(shapefile_values_notes(&ds, "cells.zip").is_empty(), "null: {name}");
            ds.features[0].attrs = vec![if ty == FieldType::Boolean { Value::Bool(false) } else { Value::Int(0) }];
            assert!(shapefile_values_notes(&ds, "cells.zip").is_empty(), "clean: {name}");
        }
        assert!(missing.is_empty(), "losses without notices: {missing:?}");
    }

    #[test]
    fn dates_written_as_text_are_said() {
        let fields = [Field::new("id", FieldType::Integer), Field::new("issued", FieldType::Date), Field::new("ts", FieldType::DateTime)];
        // A Shapefile writes dates as dates, and only its date-times as text.
        for target in ["a.shp", "a.zip"] {
            assert_eq!(
                dates_as_text_note(&fields, target).as_deref(),
                Some(format!("{target}: 1 date-time field was written as text (ISO 8601), not as dates: 'ts'").as_str())
            );
        }
        // Formats without date columns, GeoPackage with them, and layers without dates say nothing.
        for target in ["a.geojson", "a.csv", "a.kml", "a.gpkg", "a.parquet", "a.geoparquet", "a.fgb"] {
            assert_eq!(dates_as_text_note(&fields, target), None, "{target}");
        }
        assert_eq!(dates_as_text_note(&fields[..2], "a.shp"), None);
        // Text held in a date-time field is cut to a Shapefile's 254 bytes like any text, and said;
        // text in a date field that isn't a date is left empty, and said.
        let fields = vec![Field::new("ts", FieldType::DateTime), Field::new("issued", FieldType::Date)];
        let mut ds = Dataset::new("d", GeometryType::Point, fields);
        ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("x".repeat(300)), Value::Text("soon".into())]);
        assert_eq!(
            shapefile_values_notes(&ds, "a.shp"),
            [
                "a.shp: 1 text value was cut to at most 254 bytes, in 'ts'",
                "a.shp: 1 value couldn't be written to its dBase field and was left empty, in 'issued'"
            ]
        );
    }

    /// A Shapefile's date fields are written as `.dbf` dates and read back as dates, whole and
    /// streamed; its date-times as text. A date that isn't one (`20260230`) is null and said, and
    /// its shape is kept; a blank or zero date is just null; a year padded with spaces is read.
    #[test]
    fn shapefile_dates_are_written_and_read_as_dates() {
        let fields = vec![Field::new("issued", FieldType::Date), Field::new("ts", FieldType::DateTime)];
        let mut ds = Dataset::new("permits", GeometryType::Point, fields);
        // A midnight without a zone held as text in a date field is written as its day.
        let mut midnight = Dataset::new("m", GeometryType::Point, ds.fields.clone());
        midnight.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text("2026-01-01T00:00:00".into()), Value::Null]);
        let (bytes, _) = write_bytes(&midnight, "zip").unwrap();
        assert_eq!(read_bytes("m.zip", &bytes).unwrap().0.features[0].attrs[0], Value::parse("2026-01-01", FieldType::Date));
        assert!(shapefile_values_notes(&midnight, "m.shp").is_empty());
        for (i, day) in ["2026-03-05", "1999-12-31", "2000-01-01", "0001-01-01", ""].into_iter().enumerate() {
            let at = Value::parse("2026-03-05T14:30:00+02:00", FieldType::DateTime);
            ds.push(Some(Geometry::Point(Coord::new(i as f64, 1.0))), vec![Value::parse(day, FieldType::Date), at]);
        }
        let (bytes, _) = write_bytes(&ds, "zip").unwrap();
        let (back, report) = read_bytes("permits.zip", &bytes).unwrap();
        assert_eq!(back.fields.iter().map(|f| f.ty).collect::<Vec<_>>(), [FieldType::Date, FieldType::Text]);
        let days: Vec<Value> = back.features.iter().map(|f| f.attrs[0].clone()).collect();
        let expected: Vec<Value> = ds.features.iter().map(|f| f.attrs[0].clone()).collect();
        assert_eq!(days, expected);
        assert_eq!(back.features[0].attrs[1], Value::Text("2026-03-05T14:30:00+02:00".into()));
        assert!(!report.messages.iter().any(|m| m.contains("date")), "{report:?}");

        let dir = tempfile::tempdir().unwrap();
        let shp = dir.path().join("permits.shp");
        write_path(&ds, &shp).unwrap();
        let dbf = shp.with_extension("dbf");
        let mut table = std::fs::read(&dbf).unwrap();
        let replace = |table: &mut Vec<u8>, from: &[u8], to: &[u8]| {
            let at = table.windows(from.len()).position(|w| w == from).unwrap();
            table[at..at + to.len()].copy_from_slice(to);
        };
        replace(&mut table, b"19991231", b"20260230");
        replace(&mut table, b"20000101", b"00000000");
        // GDAL pads a year below 1000 with spaces (`%4d%02d%02d`): still year 1.
        replace(&mut table, b"00010101", b"   10101");
        std::fs::write(&dbf, table).unwrap();
        let whole = read_shapefile_in(&shp, "permits", None).unwrap();
        let streamed = crate::source::materialize(&crate::source::shp::ShpSource::open_path(&shp, "permits").unwrap()).unwrap();
        for (read, (back, report)) in [("whole", whole), ("streamed", streamed)] {
            let days: Vec<Value> = back.features.iter().map(|f| f.attrs[0].clone()).collect();
            let at = |d| Value::parse(d, FieldType::Date);
            assert_eq!(days, [at("2026-03-05"), Value::Null, Value::Null, at("0001-01-01"), Value::Null], "{read}");
            assert!(back.features.iter().all(|f| f.geometry.is_some()), "{read}");
            let note = report.messages.iter().find(|m| m.contains("date")).unwrap_or_else(|| panic!("{read}: {report:?}"));
            assert_eq!(note.as_str(), "permits: 1 value isn't a date and was read as empty, in 'issued'", "{read}");
            assert_eq!(note.loss, Some(crate::loss::LossCode::DroppedValue), "{read}");
        }
    }

    /// The notice for field names a Shapefile can't hold names each change as the file has it,
    /// and there is none when every name fits.
    #[test]
    fn shapefile_field_name_changes_are_said() {
        // Each field as a Shapefile written and read back names it.
        let written = |ds: &Dataset| -> Vec<String> {
            let (zip, _) = write_bytes(ds, "zip").unwrap();
            let (back, _) = read_bytes("out.zip", &zip).unwrap();
            back.fields.iter().map(|f| f.name.clone()).collect()
        };
        let layer = |names: &[&str]| {
            let fields = names.iter().map(|n| Field::new(*n, FieldType::Integer)).collect();
            let mut ds = Dataset::new("t", GeometryType::Point, fields);
            ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), names.iter().map(|_| Value::Int(1)).collect());
            ds
        };
        // Names that fit, ten bytes included: no notice, and the file keeps them.
        let fits = layer(&["id", "population", "Ab_9"]);
        assert_eq!(shapefile_names_note(&fits.fields, "out.zip"), None);
        assert_eq!(written(&fits), ["id", "population", "Ab_9"]);
        // Cut to ten bytes, made unique ignoring case, at a character boundary (é is two bytes):
        // each change as the file has it.
        let long = layer(&["population_2020", "POPULATION_2021", "kept", "éééééé"]);
        let back = written(&long);
        assert_eq!(back, ["population", "POPULATI_1", "kept", "ééééé"], "premise: the writer's names");
        assert_eq!(
            shapefile_names_note(&long.fields, "out.zip").as_deref(),
            Some(
                "out.zip: 3 field names were changed to fit a Shapefile (10 bytes at most, unique ignoring case): \
                 'population_2020' → 'population', 'POPULATION_2021' → 'POPULATI_1', 'éééééé' → 'ééééé'"
            )
        );
        // One: singular. A name of spaces is written FIELD, and padding is trimmed.
        assert_eq!(
            shapefile_names_note(&layer(&["  "]).fields, "a.shp").as_deref(),
            Some("a.shp: 1 field name was changed to fit a Shapefile (10 bytes at most, unique ignoring case): '  ' → 'FIELD'")
        );
        assert!(shapefile_names_note(&layer(&[" id"]).fields, "a.shp").is_some_and(|n| n.ends_with("' id' → 'id'")));
        // Past five, the rest are counted; a very long name shows its start.
        let many: Vec<String> = (0..8).map(|i| format!("measurement_{i}")).collect();
        let note = shapefile_names_note(&layer(&many.iter().map(String::as_str).collect::<Vec<_>>()).fields, "m.shp").unwrap();
        assert!(note.starts_with("m.shp: 8 field names were changed"), "{note}");
        assert!(note.ends_with("'measurement_4' → 'measurem_4' and 3 more"), "{note}");
        // Line breaks and other control characters in a name show as spaces: one line.
        let note = shapefile_names_note(&layer(&["a\nb\r\tc\u{2028}d\u{2029}e_long_name"]).fields, "c.shp").unwrap();
        assert!(!note.contains(['\n', '\r', '\t', '\u{2028}', '\u{2029}']), "{note:?}");
        // U+2028 is three bytes: the ten-byte name ends at d.
        assert!(note.ends_with("'a b  c d e_long_name' → 'a b  c d'"), "{note:?}");
        // So does the target, a path or layer name given by the user or read from a file.
        let note = shapefile_names_note(&layer(&["population_2020"]).fields, "out\nput\r\u{2029}.shp").unwrap();
        assert!(note.starts_with("out put  .shp: 1 field name"), "{note:?}");
        // Characters that reorder text (a right-to-left override, isolates, direction marks) show
        // as spaces too, so the notice reads 'old' → 'new' in that order; a zero-width joiner
        // stays, as emoji sequences use it.
        // The override and the isolate are three bytes each: the ten-byte name ends at d.
        let bidi = "a\u{202E}bc\u{2066}d\u{2069}e\u{200E}f\u{200F}g\u{061C}h_longer";
        let note = shapefile_names_note(&layer(&[bidi]).fields, "r\u{202E}tl.shp").unwrap();
        assert!(!note.contains(['\u{202E}', '\u{2066}', '\u{2069}', '\u{200E}', '\u{200F}', '\u{061C}']), "{note:?}");
        assert!(note.starts_with("r tl.shp: ") && note.ends_with("'a bc d e f g h_longer' → 'a bc d'"), "{note:?}");
        let joined = "a\u{200D}b_long_name";
        assert!(shapefile_names_note(&layer(&[joined]).fields, "j.shp").unwrap().contains(&format!("'{joined}' →")));
        // Exactly 40 bytes is shown whole; past that, cut at a character boundary.
        let forty = format!("{}_long", "y".repeat(35));
        assert!(shapefile_names_note(&layer(&[&forty]).fields, "f.shp").unwrap().contains(&format!("'{forty}' →")));
        let wide = format!("{}é_long", "z".repeat(39));
        assert!(shapefile_names_note(&layer(&[&wide]).fields, "w.shp").unwrap().contains(&format!("'{}…' →", "z".repeat(39))));
        let huge = "x".repeat(5000);
        let note = shapefile_names_note(&layer(&[&huge]).fields, "h.shp").unwrap();
        assert!(note.len() < 200 && note.contains(&format!("'{}…' → 'xxxxxxxxxx'", "x".repeat(40))), "{note}");
    }

    /// A cancelled export stops with [`DataError::Cancelled`] and leaves nothing at (or beside)
    /// its destination, in every format; without a probe the same exports are written.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_cancelled_export_leaves_no_file() {
        use crate::progress::tests::Counted;
        let fields = vec![Field::new("name", FieldType::Text), Field::new("n", FieldType::Integer)];
        let mut ds = Dataset::new("points", GeometryType::Point, fields);
        for i in 0..3000u64 {
            let c = Coord::new(i as f64 / 100.0, (i % 80) as f64);
            ds.features.push(crate::Feature {
                id: i + 1,
                geometry: Some(Geometry::Point(c)),
                attrs: vec![Value::Text(format!("p{i}")), Value::Int(i as i64)],
            });
        }
        let dir = tempfile::tempdir().unwrap();
        let exts = ["geojson", "csv", "fgb", "gpkg", "parquet", "shp", "kml", "gpx"];
        for ext in exts {
            let path = dir.path().join(format!("out.{ext}"));
            let probe = Counted::cancelling_after(100);
            let res = {
                let _g = crate::progress::install(Some(probe.clone()));
                write_path_in(&ds, &path, Crs::LonLat)
            };
            assert!(matches!(res, Err(DataError::Cancelled)), "{ext}: {res:?}");
            let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
            assert!(left.is_empty(), "{ext} left {left:?}");
        }
        // Cancelled once every feature is written, before the file is moved into place: a file
        // there before stays as it was (a GeoPackage export never replaces one).
        for ext in exts {
            let path = dir.path().join(format!("old.{ext}"));
            if ext != "gpkg" {
                std::fs::write(&path, b"old").unwrap();
            }
            let probe = Counted::cancelling_after(ds.features.len() as u64);
            let res = {
                let _g = crate::progress::install(Some(probe.clone()));
                write_path_in(&ds, &path, Crs::LonLat)
            };
            assert!(matches!(res, Err(DataError::Cancelled)), "{ext}: {res:?}");
            if ext != "gpkg" {
                assert_eq!(std::fs::read(&path).unwrap(), b"old", "{ext}");
                std::fs::remove_file(&path).unwrap();
            }
            let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
            assert!(left.is_empty(), "{ext} left {left:?}");
        }
        for ext in exts {
            let path = dir.path().join(format!("out.{ext}"));
            write_path_in(&ds, &path, Crs::LonLat).unwrap();
            assert!(path.exists(), "{ext}");
        }
        for e in ["shp", "shx", "dbf"] {
            assert!(dir.path().join(format!("out.{e}")).exists());
        }
    }

    #[test]
    fn shapefile_dbf_double_with_whole_numbers_remains_double() {
        let dir = tempfile::tempdir().unwrap();
        let shp = dir.path().join("values.shp");
        let fields = vec![Field::new("rate", FieldType::Double), Field::new("count", FieldType::Integer)];
        let mut ds = Dataset::new("values", GeometryType::Point, fields);
        ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Float(10.0), Value::Int(1)]);
        ds.push(Some(Geometry::Point(Coord::new(2.0, 3.0))), vec![Value::Float(20.0), Value::Int(2)]);
        write_path(&ds, &shp).unwrap();

        let (read_back, _) = read_shapefile_in(&shp, "values", None).unwrap();
        assert_eq!(read_back.fields[0].ty, FieldType::Double);
        assert_eq!(read_back.features[0].attrs[0], Value::Float(10.0));
        assert_eq!(read_back.fields[1].ty, FieldType::Integer);
        assert_eq!(read_back.features[0].attrs[1], Value::Int(1));
    }

    /// A layer held in its own system (UTM 32N here; every layer's is longitude/latitude until
    /// #2032 C2) is written from it: in longitude/latitude for a format that holds only those, in
    /// another system by the direct operation between the two, and in its own as it is.
    #[test]
    fn a_layer_is_written_from_the_system_it_holds() {
        let (utm, etrs) = (Crs::Epsg(32632), Crs::Epsg(25832));
        let lonlat = Coord::new(9.0, 48.7);
        let at = utm.transformer().unwrap().from_lonlat(lonlat).unwrap();
        let mut ds = Dataset::new("held", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(at)), vec![]);
        ds.held_in = utm;
        assert_eq!(ds.coord_crs(), utm);
        // GeoJSON: longitude/latitude.
        let (bytes, _) = write_bytes_in(&ds, "geojson", Crs::LonLat).unwrap();
        let read = read_bytes("held.geojson", &bytes).unwrap().0;
        let Some(Geometry::Point(p)) = read.features[0].geometry.clone() else { panic!("a point") };
        assert!((p.x - lonlat.x).abs() < 1e-9 && (p.y - lonlat.y).abs() < 1e-9, "{p:?}");
        // Its own system: the coordinates as they are.
        let same = project(&ds, utm).unwrap();
        assert_eq!(same.features[0].geometry, ds.features[0].geometry);
        // Another: by the direct operation from it.
        let other = project(&ds, etrs).unwrap();
        let want = utm.to(etrs).unwrap().point(at).unwrap();
        assert_eq!(other.features[0].geometry, Some(Geometry::Point(want)));
        // A file written whole, in the layer's own system, and in longitude/latitude.
        let d = std::env::temp_dir().join(format!("rustgis-held-writes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        write_path_in(&ds, &d.join("own.fgb"), utm).unwrap();
        write_path_in(&ds, &d.join("lonlat.csv"), Crs::LonLat).unwrap();
        for name in ["own.fgb", "lonlat.csv"] {
            let read = read_path(&d.join(name)).unwrap().0.in_lonlat().unwrap().into_owned();
            let Some(Geometry::Point(p)) = read.features[0].geometry.clone() else { panic!("{name}: a point") };
            assert!((p.x - lonlat.x).abs() < 1e-9 && (p.y - lonlat.y).abs() < 1e-9, "{name}: {p:?}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// The notes of an export of a layer held in its own system (UTM 32N here) are those of what
    /// is written: a CSV's of the longitude/latitude it writes (as for the same layer held in
    /// them), and a Shapefile's vertices left out only when it is written as held.
    #[test]
    fn an_exports_notes_are_of_what_it_writes() {
        let utm = Crs::Epsg(32632);
        // With x and y fields, which a CSV reader could take for the position: a point the file
        // can't give a longitude/latitude would be said.
        let fields = vec![Field::new("x", FieldType::Double), Field::new("y", FieldType::Double)];
        let mut lonlat = Dataset::new("held", GeometryType::Point, fields);
        for (lon, lat) in [(9.0, 48.7), (10.0, 50.0)] {
            lonlat.push(Some(Geometry::Point(Coord::new(lon, lat))), vec![Value::Float(1.0), Value::Float(2.0)]);
        }
        let want = export_losses_notes(&lonlat, "csv", None, "out.csv").unwrap();
        let held = project(&lonlat, utm).unwrap();
        assert_eq!(held.coord_crs(), utm);
        assert_eq!(export_losses_notes(&held, "csv", None, "out.csv").unwrap(), want);
        // A line with a missing coordinate: its vertex is left out of a Shapefile written as
        // held, and noted; in any other system the export's conversion refuses the line instead
        // (`project`), so there is nothing to note.
        let mut line = Dataset::new("line", GeometryType::Polyline, vec![]);
        let parts = vec![vec![Coord::new(500_000.0, 5_400_000.0), Coord::new(f64::NAN, 5_400_100.0), Coord::new(500_100.0, 5_400_200.0)]];
        line.push(Some(Geometry::Line(parts)), vec![]);
        line.held_in = utm;
        let own = shapefile_geometry_notes(&line, "out.shp", utm);
        assert!(own.iter().any(|n| n.contains("1 vertex with a missing or infinite coordinate")), "{own:?}");
        assert!(shapefile_geometry_notes(&line, "out.shp", Crs::Epsg(25832)).is_empty());
    }

    /// The `.dbf` of three points, named `a`, `b` and `c`, with a numeric `n` of 1, 2 and 3.
    fn three_named_points(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("pts.shp");
        let fields = vec![Field::new("name", FieldType::Text), Field::new("n", FieldType::Integer)];
        let mut ds = Dataset::new("pts", GeometryType::Point, fields);
        for (k, name) in ["a", "b", "c"].iter().enumerate() {
            let x = k as f64;
            ds.push(Some(Geometry::Point(Coord::new(x, x))), vec![Value::Text((*name).into()), Value::Int(k as i64 + 1)]);
        }
        write_path(&ds, &path).unwrap();
        path
    }

    /// Where record `k` of a `.dbf` starts (its deletion flag).
    fn dbf_record_at(dbf: &[u8], k: usize) -> usize {
        let header = usize::from(u16::from_le_bytes([dbf[8], dbf[9]]));
        let len = usize::from(u16::from_le_bytes([dbf[10], dbf[11]]));
        header + k * len
    }

    /// A Shapefile whose `.dbf` marks its middle record deleted: that record's shape is left out
    /// with it, each kept shape keeps its own attributes (dBASE's reader passing over the record
    /// once paired every later shape with the next record, and dropped the last shape), and one
    /// notice gives the count. A clean file says nothing of it.
    #[test]
    fn a_deleted_dbf_record_leaves_its_shape_out_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_named_points(dir.path());
        let deleted = |r: &ReadReport| r.messages.iter().filter(|m| m.contains("marked deleted")).count();
        let (clean, report) = read_shapefile_in(&path, "pts", None).unwrap();
        assert_eq!(clean.features.len(), 3);
        assert_eq!(deleted(&report), 0, "{:?}", report.messages);
        let dbf_path = path.with_extension("dbf");
        let mut dbf = std::fs::read(&dbf_path).unwrap();
        let at = dbf_record_at(&dbf, 1);
        dbf[at] = b'*';
        std::fs::write(&dbf_path, &dbf).unwrap();
        let (ds, report) = read_shapefile_in(&path, "pts", None).unwrap();
        let kept: Vec<_> = ds
            .features
            .iter()
            .map(|f| {
                let Some(Geometry::Point(p)) = f.geometry.clone() else { panic!("a point") };
                (p.x, f.attrs[0].clone())
            })
            .collect();
        assert_eq!(kept, [(0.0, Value::Text("a".into())), (2.0, Value::Text("c".into()))]);
        assert_eq!(deleted(&report), 1, "{:?}", report.messages);
        assert!(
            report.messages.iter().any(|m| m == "pts: 1 dBASE record marked deleted was left out, with its shape"),
            "{:?}",
            report.messages
        );
        assert_eq!(report.skipped, 0);
    }

    /// A shape that can't be read is left out with its own record, counted: the later shapes keep
    /// theirs (`shapefile`'s pairing read no record for it, shifting every later one), whether or
    /// not the record is also marked deleted.
    #[test]
    fn an_unreadable_shape_leaves_its_own_record_out() {
        for deleted in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = three_named_points(dir.path());
            // The first record's shape type (after the 100-byte header and 8-byte record header).
            let mut shp = std::fs::read(&path).unwrap();
            shp[108..112].copy_from_slice(&99i32.to_le_bytes());
            std::fs::write(&path, &shp).unwrap();
            if deleted {
                let dbf_path = path.with_extension("dbf");
                let mut dbf = std::fs::read(&dbf_path).unwrap();
                let at = dbf_record_at(&dbf, 0);
                dbf[at] = b'*';
                std::fs::write(&dbf_path, &dbf).unwrap();
            }
            let (ds, report) = read_shapefile_in(&path, "pts", None).unwrap();
            let names: Vec<_> = ds.features.iter().map(|f| f.attrs[0].clone()).collect();
            assert_eq!(names, [Value::Text("b".into()), Value::Text("c".into())], "deleted: {deleted}");
            assert_eq!(report.skipped, usize::from(!deleted), "{:?}", report.messages);
        }
    }

    /// With a `.shx`, the shapes past the `.dbf`'s last record are all counted, those that can't
    /// be read too.
    #[test]
    fn shapes_past_the_last_record_are_all_counted_with_an_shx() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_named_points(dir.path());
        let dbf_path = path.with_extension("dbf");
        let mut dbf = std::fs::read(&dbf_path).unwrap();
        dbf[4..8].copy_from_slice(&1u32.to_le_bytes());
        std::fs::write(&dbf_path, &dbf).unwrap();
        // The second shape's type: each point record is 8 + 20 bytes.
        let mut shp = std::fs::read(&path).unwrap();
        shp[136..140].copy_from_slice(&99i32.to_le_bytes());
        std::fs::write(&path, &shp).unwrap();
        let (ds, report) = read_shapefile_in(&path, "pts", None).unwrap();
        assert_eq!(ds.features.len(), 1);
        let want = "pts: 2 shape(s) past the .dbf's last record have no attributes and were left out (1 of them can't be read)";
        assert!(
            report.messages.iter().any(|m| m.as_str() == want && m.loss == Some(crate::loss::LossCode::DroppedFeature)),
            "{:?}",
            report.messages
        );
    }

    /// Without a `.shx`, a shape that can't be read ends the pairing: the shapes after it can't be
    /// found, so the records after it are left out and said rather than paired with what is read
    /// from inside the broken shape.
    #[test]
    fn without_an_shx_an_unreadable_shape_ends_the_pairing() {
        for deleted in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = three_named_points(dir.path());
            std::fs::remove_file(path.with_extension("shx")).unwrap();
            let mut shp = std::fs::read(&path).unwrap();
            shp[108..112].copy_from_slice(&99i32.to_le_bytes());
            std::fs::write(&path, &shp).unwrap();
            if deleted {
                let dbf_path = path.with_extension("dbf");
                let mut dbf = std::fs::read(&dbf_path).unwrap();
                let at = dbf_record_at(&dbf, 0);
                dbf[at] = b'*';
                std::fs::write(&dbf_path, &dbf).unwrap();
            }
            let (ds, report) = read_shapefile_in(&path, "pts", None).unwrap();
            assert!(ds.features.is_empty(), "deleted: {deleted}");
            let last = {
                let dir = tempfile::tempdir().unwrap();
                let path = three_named_points(dir.path());
                std::fs::remove_file(path.with_extension("shx")).unwrap();
                let mut shp = std::fs::read(&path).unwrap();
                // The third shape's type: each point record is 8 + 20 bytes.
                shp[164..168].copy_from_slice(&99i32.to_le_bytes());
                std::fs::write(&path, &shp).unwrap();
                read_shapefile_in(&path, "pts", None).unwrap()
            };
            assert_eq!((last.0.features.len(), last.1.skipped), (2, 1));
            assert!(last.1.messages.iter().all(|m| !m.contains("no .shx") && !m.contains("past the")), "{:?}", last.1.messages);
            assert!(
                report.messages.iter().any(|m| m.contains("no .shx to find the next one by: the 2 record(s) after it were left out")),
                "{:?}",
                report.messages
            );
        }
    }

    /// A `.dbf` one record short leaves the last shape out, and says so; one with a record more
    /// than the shapes says that. A matching pair says neither.
    #[test]
    fn shapes_and_records_that_dont_pair_are_said() {
        let said = |r: &ReadReport| {
            r.messages
                .iter()
                .filter(|m| m.contains("past the") && m.loss == Some(crate::loss::LossCode::DroppedFeature))
                .map(|m| m.as_str().to_string())
                .collect::<Vec<_>>()
        };
        let dir = tempfile::tempdir().unwrap();
        let path = three_named_points(dir.path());
        let (ds, report) = read_shapefile_in(&path, "pts", None).unwrap();
        assert_eq!(ds.features.len(), 3);
        assert!(said(&report).is_empty(), "{:?}", report.messages);
        let dbf_path = path.with_extension("dbf");
        let clean = std::fs::read(&dbf_path).unwrap();
        for (records, want) in [
            (2u32, "pts: 1 shape(s) past the .dbf's last record have no attributes and were left out"),
            (4, "pts: 1 dBASE record(s) past the .shp's last shape have no shape and were left out"),
        ] {
            let mut dbf = clean.clone();
            dbf[4..8].copy_from_slice(&records.to_le_bytes());
            if records == 4 {
                // A fourth record, a copy of the third, before the end-of-file marker.
                let (at, len) = (dbf_record_at(&dbf, 2), dbf_record_at(&dbf, 1) - dbf_record_at(&dbf, 0));
                let row = dbf[at..at + len].to_vec();
                dbf.splice(at + len..at + len, row);
            }
            std::fs::write(&dbf_path, &dbf).unwrap();
            let (ds, report) = read_shapefile_in(&path, "pts", None).unwrap();
            assert_eq!(ds.features.len(), records.min(3) as usize);
            assert_eq!(said(&report), [want]);
        }
    }

    /// A standalone table: a record marked deleted is left out and counted, and one dBASE can't
    /// read (a number that isn't one) is left out and said, with the rows on both sides kept.
    /// Clean input says neither.
    #[test]
    fn a_dbf_table_counts_deleted_and_unreadable_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = three_named_points(dir.path());
        let clean = std::fs::read(path.with_extension("dbf")).unwrap();
        let notes = |r: &ReadReport| {
            r.messages
                .iter()
                .filter(|m| m.contains("marked deleted") || m.contains("unreadable"))
                .map(|m| m.as_str().to_string())
                .collect::<Vec<_>>()
        };
        let (ds, report) = read_bytes("t.dbf", &clean).unwrap();
        assert_eq!(ds.features.len(), 3);
        assert!(notes(&report).is_empty(), "{:?}", report.messages);
        let mut one_deleted = clean.clone();
        let at = dbf_record_at(&one_deleted, 1);
        one_deleted[at] = b'*';
        let (ds, report) = read_bytes("t.dbf", &one_deleted).unwrap();
        assert_eq!(ds.features.iter().map(|f| f.attrs[0].clone()).collect::<Vec<_>>(), [Value::Text("a".into()), Value::Text("c".into())]);
        assert_eq!(notes(&report), ["t: 1 dBASE record marked deleted was left out"]);
        // The `n` column follows the 1-byte flag and the `name` column.
        let mut unreadable = clean.clone();
        let name_len = usize::from(unreadable[32 + 16]);
        let at = dbf_record_at(&unreadable, 1) + 1 + name_len;
        let n_len = usize::from(unreadable[64 + 16]);
        unreadable[at..at + n_len].fill(b'x');
        let (ds, report) = read_bytes("t.dbf", &unreadable).unwrap();
        assert_eq!(ds.features.iter().map(|f| f.attrs[0].clone()).collect::<Vec<_>>(), [Value::Text("a".into()), Value::Text("c".into())]);
        assert_eq!(notes(&report), ["t: 1 dBASE record was unreadable and left out"]);
    }
}
