//! Vector readers and writers: GeoJSON, CSV (with longitude/latitude columns) and Shapefile.

use std::path::Path;

use serde_json::{Map, Value as Json, json};

use crate::raster::Crs;
use crate::{Coord, DataError, Dataset, Field, FieldType, Geometry, GeometryType, ReadReport, Result, Value};

/// Largest file read into memory (256 MiB). Parsing JSON can take several times the file size.
pub const MAX_BYTES: u64 = 256 << 20;

/// File types the readers understand, for open dialogs.
pub const READ_EXTENSIONS: &[&str] = &[
    "geojson",
    "json",
    "csv",
    "tsv",
    "txt",
    "shp",
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

/// Reads every layer of a file held in memory: the Shapefiles and other files of a zip, a KML or
/// KMZ's points, lines and polygons (with their styles), or a GPX's waypoints, routes, tracks and
/// track points. Other files give their one layer.
pub fn read_layers(file_name: &str, bytes: &[u8]) -> Result<Vec<LayerRead>> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(DataError::Format(format!("file is too large ({} bytes)", bytes.len())));
    }
    let ext = Path::new(file_name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
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
            if crate::landxml::looks_like_landxml(&text) {
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
                    "{file_name}: an XML file RustGIS doesn't read (it reads GeoRSS, LandXML, KML and GPX)"
                )));
            }
        }
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
/// buffer answer (`pbf`), KML, GPX, GeoJSON, delimited text or JPEG 2000 (`None` for anything
/// else, such as a web page).
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
    if bytes.len() as u64 > MAX_BYTES {
        return Err(DataError::Format(format!("file is too large ({} bytes)", bytes.len())));
    }
    let text = || std::str::from_utf8(bytes).map_err(|_| DataError::Format(format!("{file_name} is not UTF-8 text")));
    let mut out = match ext.as_str() {
        // A CityJSON model or a TopoJSON topology named .json: its first layer, saying what else
        // it holds.
        "json" if is_cityjson(file_name, bytes) => return first_layer(file_name, bytes),
        "json" if crate::topojson::looks_like_topojson(text()?) => return first_layer(file_name, bytes),
        "geojson" | "json" => read_geojson(text()?, &name)?,
        "csv" | "tsv" | "txt" => read_csv_bytes(file_name, bytes, &name)?,
        // A MIF without its .mid (an upload of the one file): no attributes.
        "mif" => crate::mapinfo::read_bytes(bytes, None, &name)?,
        // Picked or dropped with its .dbf, a Shapefile comes zipped (`gather_shapefiles`).
        "shp" => {
            return Err(DataError::Unsupported(
                "shp on its own: add it together with its .dbf, which it needs, and its .shx and .prj if it has them (pick or drop them at once), or add them zipped".into(),
            ));
        }
        "dbf" | "shx" | "prj" | "cpg" => {
            return Err(DataError::Unsupported(format!("{ext} on its own: it is part of a Shapefile; add it together with its .shp")));
        }
        // The first layer (`read_layers` reads them all), saying what else the file holds.
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
    if bytes.len() as u64 > MAX_BYTES {
        return Err(DataError::Format(format!("file is too large ({} bytes)", bytes.len())));
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
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("layer").to_string();
    let mut out = match ext.as_str() {
        "geojson" | "json" => {
            check_size(path)?;
            let text = std::fs::read_to_string(path)?;
            // A CityJSON model or a TopoJSON topology named .json, as read_bytes reads it.
            let leaf = path.file_name().and_then(|n| n.to_str()).unwrap_or("layer.json");
            if ext == "json" && (is_cityjson(leaf, text.as_bytes()) || crate::topojson::looks_like_topojson(&text)) {
                first_layer(leaf, text.as_bytes())?
            } else {
                read_geojson(&text, &name)?
            }
        }
        "csv" | "tsv" | "txt" => {
            check_size(path)?;
            let leaf = path.file_name().and_then(|n| n.to_str()).unwrap_or("layer.csv");
            read_csv_bytes(leaf, &std::fs::read(path)?, &name)?
        }
        "shp" => {
            check_size(path)?;
            let dbf = path.with_extension("dbf");
            if dbf.exists() {
                check_size(&dbf)?;
            }
            read_shapefile(path, &name)?
        }
        "zip" | "kml" | "kmz" | "gpx" | "gml" | "dxf" | "pbf" | "topojson" => {
            check_size(path)?;
            read_bytes(&path.display().to_string(), &std::fs::read(path)?)?
        }
        // With its .mid beside it (spelt mid, MID or Mid).
        "mif" => {
            check_size(path)?;
            let mid = ["mid", "MID", "Mid"].iter().map(|e| path.with_extension(e)).find(|p| p.is_file());
            let mid = match mid {
                Some(m) => {
                    check_size(&m)?;
                    Some(std::fs::read(m)?)
                }
                None => None,
            };
            crate::mapinfo::read_bytes(&std::fs::read(path)?, mid.as_deref(), &name)?
        }
        "las" | "laz" => crate::lidar::read(std::io::BufReader::new(std::fs::File::open(path)?), &name, None)?,
        // Read in place by a provider, so there's no size cap on the file, only on the features.
        e if crate::source::EXTENSIONS.contains(&e) => crate::source::materialize(crate::source::open_path(path, None)?.as_ref())?,
        other => return Err(DataError::Unsupported(other.to_string())),
    };
    out.0.source = Some(path.display().to_string());
    Ok(out)
}

fn check_size(path: &Path) -> Result<()> {
    let len = std::fs::metadata(path)?.len();
    if len > MAX_BYTES {
        return Err(DataError::Format(format!("file is too large ({len} bytes)")));
    }
    Ok(())
}

/// The field KML and GPX name placemarks by: one called `name`, if any.
fn name_field(ds: &Dataset) -> Option<usize> {
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
/// (GeoJSON, KML and GPX are longitude/latitude by definition).
pub const PROJECTED_FORMATS: [&str; 4] = ["gpkg", "shp", "zip", "fgb"];

/// The coordinate system an export to extension `ext` is written in when none is asked for: the
/// one the dataset was read in, for a format that can say it, else WGS 84. A system without an
/// EPSG code goes only into a Shapefile, whose `.prj` gives it in full; GeoPackage and FlatGeobuf
/// readers (RustGIS's among them) go by the code.
pub fn default_export_crs(ds: &Dataset, ext: &str) -> Crs {
    let ext = ext.to_ascii_lowercase();
    ds.crs
        .filter(|c| PROJECTED_FORMATS.contains(&ext.as_str()) && (c.epsg() > 0 || matches!(ext.as_str(), "shp" | "zip")))
        .unwrap_or(Crs::LonLat)
}

/// The dataset (in longitude/latitude, as every dataset is) in `crs`'s coordinates. Fails on a
/// coordinate the system can't hold, naming its feature.
pub fn project(ds: &Dataset, crs: Crs) -> Result<Dataset> {
    if crs == Crs::LonLat {
        return Ok(ds.clone());
    }
    let mut out = ds.clone();
    let t = crs.transformer().map_err(DataError::Format)?;
    project_features(&mut out.features, &t, crs)?;
    Ok(out)
}

/// Converts features' longitude/latitude geometry in place to the coordinates of `crs`, through
/// its transformer `t`. Fails on a coordinate the system can't hold, naming its feature.
pub(crate) fn project_features(features: &mut [crate::Feature], t: &crate::raster::Transformer, crs: Crs) -> Result<()> {
    for f in features {
        if let Some(g) = f.geometry.take() {
            let mut ok = true;
            let p = g.map_coords(&|c| t.from_lonlat(c).unwrap_or(Coord::new(f64::NAN, f64::NAN)));
            p.for_each_coord(|c| ok &= c.is_finite());
            if !ok {
                return Err(DataError::Format(format!("feature {} can't be placed in {}", f.id, crs.label())));
            }
            f.geometry = Some(p);
        }
    }
    Ok(())
}

/// Defines the CRS of the dataset's native coordinates without changing those numeric values.
/// Geometry stored in longitude/latitude is converted back through the previous CRS, then
/// interpreted in the new one. Builds a copy so failed transformations leave the input intact.
/// Source geometry discarded during import cannot be recovered this way.
pub fn define_projection(ds: &Dataset, crs: Crs) -> Result<Dataset> {
    if ds.crs_incomplete {
        return Err(DataError::Format("geometry was lost during import; import the original file again with a crs override".into()));
    }
    let from = ds.crs.unwrap_or(Crs::LonLat);
    let native = project(ds, from)?;
    let to = crs.transformer().map_err(DataError::Format)?;
    let mut out = native;
    for f in &mut out.features {
        if let Some(g) = f.geometry.take() {
            let g = g.map_coords(&|c| to.to_lonlat(c).unwrap_or(Coord::new(f64::NAN, f64::NAN)));
            let mut valid = true;
            g.for_each_coord(|c| valid &= c.is_finite() && c.x.abs() <= 180.0 && c.y.abs() <= 90.0);
            if !valid {
                return Err(DataError::Format(format!(
                    "feature {} can't be interpreted in {}; import the original file with a crs override",
                    f.id,
                    crs.label()
                )));
            }
            f.geometry = Some(g);
        }
    }
    out.crs = Some(crs);
    Ok(out)
}

/// Refuses a system without an EPSG code for a GeoPackage or FlatGeobuf: their readers (RustGIS's
/// among them) go by the code, and would read the file as longitude/latitude.
pub(crate) fn check_projected(ext: &str, crs: Crs) -> Result<()> {
    if crs.epsg() == 0 && matches!(ext, "gpkg" | "fgb") {
        return Err(DataError::Unsupported(format!(".{ext} in {}: give the system as an EPSG code", crs.label())));
    }
    Ok(())
}

/// As [`write_bytes`], in the coordinate system `crs`: a GeoPackage, Shapefile or FlatGeobuf (see
/// [`PROJECTED_FORMATS`]) says it; any other format is written only in longitude/latitude.
pub fn write_bytes_in(ds: &Dataset, ext: &str, crs: Crs) -> Result<(Vec<u8>, usize)> {
    let ext = ext.to_ascii_lowercase();
    if crs != Crs::LonLat {
        if !PROJECTED_FORMATS.contains(&ext.as_str()) {
            return Err(DataError::Unsupported(format!(".{ext} in {} (it holds longitude/latitude only)", crs.label())));
        }
        check_projected(&ext, crs)?;
        return projected_bytes(&project(ds, crs)?, &ext, crs);
    }
    projected_bytes(ds, &ext, crs)
}

/// [`write_bytes_in`] for a dataset already in `crs`'s coordinates.
fn projected_bytes(ds: &Dataset, ext: &str, crs: Crs) -> Result<(Vec<u8>, usize)> {
    Ok(match ext {
        "geojson" | "json" => {
            let mut w = GeoJsonWriter::new(Vec::new(), &ds.name, &ds.fields)?;
            w.write(&ds.features)?;
            (w.finish()?, 0)
        }
        "csv" => (to_csv(ds)?.into_bytes(), 0),
        "fgb" => (crate::source::fgb::write_in(ds, crs)?, without_geometry(ds)),
        "parquet" | "geoparquet" => (crate::source::parquet::write(ds)?, 0),
        "kml" => (crate::interop::to_kml(ds, name_field(ds)).into_bytes(), without_geometry(ds)),
        "kmz" => {
            let kml = crate::interop::to_kml(ds, name_field(ds)).into_bytes();
            (crate::zip::write(&[("doc.kml".to_string(), kml)]).map_err(DataError::Format)?, without_geometry(ds))
        }
        "gpx" => (crate::interop::to_gpx(ds, name_field(ds)).into_bytes(), without_geometry(ds)),
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

/// As [`write_path`], in the coordinate system `crs` (a GeoPackage, Shapefile or FlatGeobuf; see
/// [`write_bytes_in`]).
pub fn write_path_in(ds: &Dataset, path: &Path, crs: Crs) -> Result<usize> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let projected;
    let ds = if crs == Crs::LonLat || !PROJECTED_FORMATS.contains(&ext.as_str()) {
        ds
    } else {
        check_projected(&ext, crs)?;
        projected = project(ds, crs)?;
        &projected
    };
    match ext.as_str() {
        "gpkg" => {
            #[cfg(not(target_arch = "wasm32"))]
            {
                crate::source::gpkg::write_in(ds, path, crs)?;
                Ok(0)
            }
            #[cfg(target_arch = "wasm32")]
            Err(DataError::Unsupported("GeoPackage in the browser".into()))
        }
        "shp" => {
            let (parts, left_out) = shapefile_parts_in(ds, crs)?;
            // Every part is written to a temporary file first and renamed into place only once all
            // are written, the .shp last, so a failed write leaves the old Shapefile whole.
            let mut written: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
            let staged = parts.iter().try_for_each(|(e, bytes)| {
                // The .shp keeps the name given (`Roads.SHP` stays so); the others take its stem.
                let target = if *e == "shp" { path.to_path_buf() } else { path.with_extension(e) };
                let tmp = temp_beside(&target)?;
                let result = write_new(&tmp, bytes);
                if result.is_ok() {
                    written.push((tmp, target));
                }
                result
            });
            if let Err(e) = staged {
                for (tmp, _) in &written {
                    let _ = std::fs::remove_file(tmp);
                }
                return Err(e.into());
            }
            written.sort_by_key(|(_, target)| target == path);
            // The old parts are moved aside first and put back if any new part fails to land, so
            // the old Shapefile stays a matching set.
            let mut backups: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
            let mut placed: Vec<std::path::PathBuf> = Vec::new();
            let mut result = Ok(());
            for (tmp, target) in &written {
                if target.exists() {
                    // Any failure here rolls back like a failed rename.
                    let bak = match temp_beside(target) {
                        Ok(b) => b,
                        Err(e) => {
                            result = Err(e);
                            break;
                        }
                    };
                    if let Err(e) = std::fs::rename(target, &bak) {
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
                for p in &placed {
                    let _ = std::fs::remove_file(p);
                }
                for (bak, target) in &backups {
                    let _ = std::fs::rename(bak, target);
                }
                for (tmp, _) in &written {
                    let _ = std::fs::remove_file(tmp);
                }
                return Err(e.into());
            }
            for (bak, _) in &backups {
                let _ = std::fs::remove_file(bak);
            }
            // A spatial index left from an older file of the same name would no longer match.
            for stale in ["qix", "sbn", "sbx"] {
                let _ = std::fs::remove_file(path.with_extension(stale));
            }
            Ok(left_out)
        }
        _ if crs != Crs::LonLat && !PROJECTED_FORMATS.contains(&ext.as_str()) => {
            Err(DataError::Unsupported(format!(".{ext} in {} (it holds longitude/latitude only)", crs.label())))
        }
        _ => {
            let (bytes, left_out) = projected_bytes(ds, &ext, crs)?;
            write_atomic(path, &bytes)?;
            Ok(left_out)
        }
    }
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
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_new_with(path, |w| w.write_all(bytes))
}

/// [`write_new`] with the content written by `write`, through a buffer, as it goes.
fn write_new_with(path: &Path, write: impl FnOnce(&mut dyn std::io::Write) -> std::io::Result<()>) -> std::io::Result<()> {
    use std::io::Write;
    let f = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    let res = (|| {
        let mut w = std::io::BufWriter::new(&f);
        write(&mut w)?;
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
/// write never truncates an existing file. The temporary file is removed on failure.
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
    use std::io::Write;
    let tmp = temp_beside(path)?;
    // Opened before the guard, so a name already taken is never removed.
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    // Removes the file on an error and on a panic in `write` alike, unless it was moved into place.
    let staged = Staged(&tmp);
    let mut out = std::io::BufWriter::new(file);
    let value = write(&mut out)?;
    out.flush()?;
    out.get_ref().sync_all()?;
    // Closed before the rename, including on Windows.
    drop(out);
    std::fs::rename(&tmp, path)?;
    std::mem::forget(staged);
    Ok(value)
}

/// A file being written beside its destination: removed when dropped, as an export that
/// failed or panicked leaves it. A finished export forgets it once the file is renamed.
struct Staged<'a>(&'a Path);

impl Drop for Staged<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

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
            Some("FeatureCollection") => Ok(features.finish(name, &rest)),
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
pub(crate) fn declared_crs(root: &Map<String, Json>, messages: &mut Vec<String>) -> Option<(Crs, crate::raster::Transformer)> {
    let decl = root.get("crs").filter(|v| !v.is_null())?;
    match legacy_crs(decl) {
        Ok(found) => found,
        Err(why) => {
            let shown = match decl.get("properties").and_then(|p| p.get("name")).and_then(Json::as_str) {
                Some(name) => format!("\"{}\"", clip(name)),
                None => match decl.get("type").and_then(Json::as_str) {
                    Some(kind) => format!("of type \"{}\"", clip(kind)),
                    None => "of an unknown shape".to_string(),
                },
            };
            messages.push(format!("GeoJSON crs {shown}: {why}; coordinates are read as longitude/latitude"));
            None
        }
    }
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
fn is_epsg_or_crs84_name(name: &str) -> bool {
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

/// A geometry in longitude/latitude from the coordinates `t` converts: `None` when a coordinate
/// can't be converted or comes out beyond longitude/latitude (a declared system that doesn't match
/// the coordinates).
pub(crate) fn geometry_to_lonlat(g: &Geometry, t: &crate::raster::Transformer) -> Option<Geometry> {
    let mut ok = true;
    let out = g.map_coords(&|c| t.to_lonlat(c).unwrap_or(Coord::new(f64::NAN, f64::NAN)));
    out.for_each_coord(|c: Coord| ok &= c.is_finite() && c.x.abs() <= 180.5 && c.y.abs() <= 90.5);
    ok.then_some(out)
}

/// What takes a FeatureCollection's features as they're read.
pub(crate) trait FeatureSink {
    /// The `"features"` member starts (again, when a document repeats it: the last one wins).
    fn restart(&mut self);
    /// One element of `"features"`, as its text.
    fn feature(&mut self, raw: &str);
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

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
        while let Some(raw) = seq.next_element::<&'de serde_json::value::RawValue>()? {
            self.sink.feature(raw.get());
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
    fn finish(self, name: &str, root: &Map<String, Json>) -> (Dataset, ReadReport) {
        let mut report = ReadReport { skipped: self.skipped, ..ReadReport::default() };
        let mut ds = Dataset::new(name, GeometryType::None, self.schema.fields());
        ds.features.reserve(self.features.len());
        if report.skipped > 0 {
            report.messages.push(format!("{} features had unreadable geometry", report.skipped));
        }
        let declared = declared_crs(root, &mut report.messages);
        let mut unconverted = 0usize;
        // Only features kept count toward the heights dropped.
        let mut zm = 0usize;
        for (n, (geom, values, had_z)) in self.features.into_iter().enumerate() {
            // A feature that can't be converted is dropped, and its id is not reused, as when the
            // file is read in place.
            let geom = match (&declared, geom) {
                (Some((_, t)), Some(g)) => match geometry_to_lonlat(&g, t) {
                    Some(g) => Some(g),
                    None => {
                        unconverted += 1;
                        continue;
                    }
                },
                (_, g) => g,
            };
            let mut attrs = vec![Value::Null; ds.fields.len()];
            for (i, v) in values {
                if let (Some(slot), Some(field)) = (attrs.get_mut(i), ds.fields.get(i)) {
                    *slot = coerce(v, field.ty);
                }
            }
            zm += usize::from(had_z);
            ds.features.push(crate::Feature { id: n as u64 + 1, geometry: geom, attrs });
        }
        ds.geometry_type = dominant_type(&ds);
        if let Some((crs, _)) = &declared {
            ds.crs = Some(*crs);
            if unconverted > 0 {
                report.skipped += unconverted;
                report.messages.push(format!(
                    "{unconverted} features couldn't be converted from {} to longitude/latitude and were skipped",
                    crs.label()
                ));
            }
            // What couldn't be converted can't come back by defining the projection again.
            ds.crs_incomplete = unconverted > 0;
        }
        // Last, as a source read in place notes it.
        ds.drop_zm(&mut report, zm);
        (ds, report)
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
    Ok(features.finish(name, &top))
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

fn coerce(v: Value, ty: FieldType) -> Value {
    match (ty, v) {
        (_, Value::Null) => Value::Null,
        (FieldType::Double, Value::Int(i)) => Value::Float(i as f64),
        (FieldType::Text, v @ Value::Text(_)) => v,
        (FieldType::Text, v) => Value::Text(v.to_string()),
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

/// A dataset as a GeoJSON FeatureCollection value, in longitude/latitude.
pub fn to_geojson(ds: &Dataset) -> Json {
    let features: Vec<Json> = ds.features.iter().map(|f| feature_to_geojson(&ds.fields, f)).collect();
    json!({"type": "FeatureCollection", "name": ds.name, "features": features})
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

const LON_NAMES: &[&str] = &["lon", "lng", "long", "longitude", "x", "lon_dd", "longitude_dd"];
const LAT_NAMES: &[&str] = &["lat", "latitude", "y", "lat_dd", "latitude_dd"];

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

/// The x/y columns found by name, as a pair from one family: longitude/latitude, else
/// easting/northing.
fn found_xy(headers: &[String]) -> Option<(usize, usize)> {
    let find = |names: &[&str]| headers.iter().position(|h| names.contains(&h.trim().to_ascii_lowercase().as_str()));
    find(LON_NAMES).zip(find(LAT_NAMES)).or_else(|| find(X_NAMES).zip(find(Y_NAMES)))
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

/// The rows [`csv_columns`] reads: a WKT column is found by its first value.
const COLUMN_SAMPLE_ROWS: usize = 1000;

/// The columns of rows under a header row, as [`read_rows`] would find them.
pub(crate) fn columns_of(headers: &[String], rows: &[Vec<String>]) -> Columns {
    let xy = found_xy(headers);
    let wkt = if xy.is_some() { None } else { found_wkt(headers, rows) };
    let name = |i: usize| headers.get(i).map(|h| h.trim().to_string());
    Columns {
        names: headers.iter().map(|h| h.trim().to_string()).collect(),
        x: xy.and_then(|(x, _)| name(x)),
        y: xy.and_then(|(_, y)| name(y)),
        wkt: wkt.and_then(name),
        delimiter: None,
    }
}

/// The columns of delimited text (split at `delimiter`, or the one found), without reading all
/// of it: a WKT column whose first value is past the first [`COLUMN_SAMPLE_ROWS`] rows isn't found.
pub fn csv_columns(text: &str, delimiter: Option<u8>) -> Result<Columns> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let delimiter = delimiter.unwrap_or_else(|| sniff_delimiter(text));
    let mut rdr = csv::ReaderBuilder::new().flexible(true).delimiter(delimiter).from_reader(text.as_bytes());
    let headers: Vec<String> = rdr.headers().map_err(|e| DataError::Format(format!("CSV: {e}")))?.iter().map(|s| s.to_string()).collect();
    let rows: Vec<Vec<String>> =
        rdr.records().flatten().take(COLUMN_SAMPLE_ROWS).map(|r| r.iter().map(|s| s.to_string()).collect()).collect();
    Ok(Columns { delimiter: Some(delimiter), ..columns_of(&headers, &rows) })
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

/// Reads delimited text from its bytes, in the encoding [`delimited_text`] finds, saying which
/// when it isn't UTF-8.
fn read_csv_bytes(file_name: &str, bytes: &[u8], name: &str) -> Result<(Dataset, ReadReport)> {
    let (text, note) = delimited_text(file_name, bytes)?;
    let (ds, mut report) = read_csv(&text, name)?;
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
    // Spreadsheet exports often start with a byte-order mark, which would hide the first header.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let delimiter = opts.delimiter.unwrap_or_else(|| sniff_delimiter(text));
    let mut rdr = csv::ReaderBuilder::new().flexible(true).delimiter(delimiter).from_reader(text.as_bytes());
    let headers: Vec<String> = rdr.headers().map_err(|e| DataError::Format(format!("CSV: {e}")))?.iter().map(|s| s.to_string()).collect();
    let mut malformed = 0usize;
    let rows: Vec<Vec<String>> = rdr
        .records()
        .filter_map(|r| match r {
            Ok(r) => Some(r.iter().map(|s| s.to_string()).collect()),
            Err(_) => {
                malformed += 1;
                None
            }
        })
        .collect();
    read_rows(&headers, &rows, malformed, name, opts)
}

/// Rows of text cells under a header row (delimited text, a spreadsheet's sheet) as a dataset, as
/// [`read_csv_with`] describes; `malformed` rows were already passed over.
pub(crate) fn read_rows(
    headers: &[String],
    rows: &[Vec<String>],
    malformed: usize,
    name: &str,
    opts: &CsvOptions,
) -> Result<(Dataset, ReadReport)> {
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
        (None, None) if wkt_given.is_none() => found_xy(headers).unzip(),
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
        .filter_map(|&i| Some(Field::new(headers.get(i)?.trim(), text_type(rows.iter().filter_map(|r| r.get(i)).map(|s| s.as_str())))))
        .collect();
    let mut report = ReadReport { skipped: malformed, ..ReadReport::default() };
    if malformed > 0 {
        report.messages.push(format!("{malformed} rows were malformed and skipped"));
    }
    let gt = if xy.is_some() { GeometryType::Point } else { GeometryType::None };
    let mut ds = Dataset::new(name, gt, fields);
    let mut srid: Option<u32> = None;
    let (mut bad_wkt, mut zm_rows) = (0usize, 0usize);
    for r in rows {
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
                t => match crate::interop::parse_wkt_zm(t) {
                    Ok((g, s, zm)) => {
                        srid = srid.or(s);
                        zm_rows += usize::from(zm);
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
            .map(|(&i, f)| Value::parse(&unguard(r.get(i).map(|s| s.as_str()).unwrap_or("")), f.ty))
            .collect();
        ds.push_next(geom, attrs);
    }
    ds.drop_zm(&mut report, zm_rows);
    if report.skipped > malformed {
        report.messages.push(format!("{} rows had no usable coordinates", report.skipped - malformed));
    }
    if bad_wkt > 0 {
        report.messages.push(format!("{bad_wkt} rows had geometry that isn't readable WKT"));
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
                report.messages.push(format!("unknown coordinate system SRID {code}; coordinates are read as longitude/latitude"));
                None
            }
        },
        (None, None) => None,
    };
    match crs.filter(|c| *c != crate::raster::Crs::LonLat) {
        Some(c) => {
            let t = c.transformer().map_err(DataError::Format)?;
            ds.crs = Some(c);
            let mut lost = 0usize;
            for f in &mut ds.features {
                if let Some(g) = f.geometry.take() {
                    let out = g.map_coords(&|p| t.to_lonlat(p).unwrap_or(Coord::new(f64::NAN, f64::NAN)));
                    let mut ok = true;
                    out.for_each_coord(|c| ok &= c.is_finite());
                    if ok {
                        f.geometry = Some(out);
                    } else {
                        lost += 1;
                    }
                }
            }
            if lost > 0 {
                report.skipped += lost;
                report.messages.push(format!("{lost} rows couldn't be converted from {} to longitude/latitude", c.label()));
            }
        }
        None => {
            let outside = ds.features.iter().filter_map(|f| f.geometry.as_ref()).any(out_of_lonlat);
            if outside {
                report.messages.push(
                    "coordinates aren't longitude/latitude: give the file's coordinate system (\"crs\", such as EPSG:2263) to place them"
                        .into(),
                );
            }
        }
    }
    Ok((ds, report))
}

/// Whether a geometry has coordinates outside longitude/latitude's range.
fn out_of_lonlat(g: &Geometry) -> bool {
    let e = g.extent();
    e.min_x < -180.0 || e.max_x > 180.0 || e.min_y < -90.0 || e.max_y > 90.0
}

/// The type of a column of text values: Integer or Double when every non-empty value is one,
/// otherwise (or with no values) Text.
pub(crate) fn text_type<'a>(vals: impl Iterator<Item = &'a str>) -> FieldType {
    let mut ty: Option<FieldType> = None;
    for v in vals.map(str::trim).filter(|s| !s.is_empty()) {
        let t = if has_leading_zero(v) {
            // Identifiers such as ZIP codes keep their leading zeros.
            FieldType::Text
        } else if v.parse::<i64>().is_ok() {
            FieldType::Integer
        } else if v.parse::<f64>().is_ok_and(f64::is_finite) {
            FieldType::Double
        } else {
            FieldType::Text
        };
        ty = Some(match ty {
            None => t,
            Some(prev) => widen(prev, Some(t)),
        });
    }
    ty.unwrap_or(FieldType::Text)
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
                *v = if t.trim().is_empty() { Value::Null } else { Value::parse(t, ty) };
            }
        }
    }
}

/// `0123` or `-007` (but not `0`, `0.5` or `-0.5`): a code, not a number.
fn has_leading_zero(v: &str) -> bool {
    let d = v.strip_prefix(['-', '+']).unwrap_or(v);
    d.len() > 1 && d.starts_with('0') && d.as_bytes().get(1).is_some_and(|b| b.is_ascii_digit())
}

/// Undoes the export formula guard: `'=x` reads back as `=x`.
fn unguard(cell: &str) -> std::borrow::Cow<'_, str> {
    match cell.strip_prefix('\'') {
        Some(rest) if rest.starts_with(FORMULA_START) => std::borrow::Cow::Owned(rest.to_string()),
        _ => std::borrow::Cow::Borrowed(cell),
    }
}

const FORMULA_START: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// A cell's text; text that a spreadsheet would run as a formula is prefixed with `'`.
fn csv_cell(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Text(t) if t.starts_with(FORMULA_START) => format!("'{t}"),
        // The shortest text that reads back as the same number: `Display` rounds to 6 decimals.
        Value::Float(f) => f.to_string(),
        other => other.to_string(),
    }
}

/// A dataset as CSV text: its fields, and a point layer's longitude and latitude ([`CsvWriter`]).
pub fn to_csv(ds: &Dataset) -> Result<String> {
    let mut w = CsvWriter::new(Vec::new(), &ds.fields, ds.geometry_type)?;
    w.write(&ds.features)?;
    String::from_utf8(w.finish()?).map_err(|e| DataError::Format(e.to_string()))
}

/// Writes CSV a run of features at a time: a header of the fields, and for a point layer
/// longitude and latitude columns after them.
pub(crate) struct CsvWriter<W: std::io::Write> {
    w: csv::Writer<W>,
    point: bool,
}

impl<W: std::io::Write> CsvWriter<W> {
    /// Writes the header for `fields`, with coordinate columns for a point layer.
    pub(crate) fn new(out: W, fields: &[Field], geometry_type: GeometryType) -> Result<Self> {
        let mut w = csv::Writer::from_writer(out);
        let point = geometry_type == GeometryType::Point;
        let mut head: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
        if point {
            // Coordinate columns get names that don't collide with existing fields.
            for base in ["longitude", "latitude"] {
                let mut name = base.to_string();
                let mut i = 1;
                while head.iter().any(|h| h.eq_ignore_ascii_case(&name)) {
                    name = format!("{base}_{i}");
                    i += 1;
                }
                head.push(name);
            }
        }
        w.write_record(&head).map_err(|e| DataError::Format(e.to_string()))?;
        Ok(CsvWriter { w, point })
    }

    /// Writes the next features, a row each.
    pub(crate) fn write(&mut self, features: &[crate::Feature]) -> Result<()> {
        for f in features {
            let mut row: Vec<String> = f.attrs.iter().map(csv_cell).collect();
            if self.point {
                let c = f.geometry.as_ref().and_then(|g| g.label_point());
                row.push(c.map(|c| c.x.to_string()).unwrap_or_default());
                row.push(c.map(|c| c.y.to_string()).unwrap_or_default());
            }
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

/// Reads a loose Shapefile using its sidecar CRS, retaining the default reader's behavior.
fn read_shapefile(path: &Path, name: &str) -> Result<(Dataset, ReadReport)> {
    read_shapefile_in(path, name, None)
}

/// Reads a Shapefile with an optional source CRS replacing its `.prj`, before any geometry is
/// converted. This changes only interpretation; the files on disk are never modified.
pub fn read_shapefile_in(path: &Path, name: &str, crs: Option<Crs>) -> Result<(Dataset, ReadReport)> {
    check_size(path)?;
    let dbf = path.with_extension("dbf");
    if !dbf.is_file() {
        return Err(DataError::Format(format!("Shapefile: {} is missing", dbf.display())));
    }
    check_size(&dbf)?;
    let shx = path.with_extension("shx");
    if shx.is_file() {
        check_size(&shx)?;
    }
    let mut notes = Vec::new();
    let parts = ShapefileParts {
        shp: std::fs::read(path)?,
        shx: if shx.is_file() { Some(std::fs::read(shx)?) } else { None },
        dbf: std::fs::read(dbf)?,
        prj: crate::raster::read_prj(path),
        cpg: crate::dbf::read_cpg(path, &mut notes),
    };
    let (ds, mut report) = read_shapefile_parts(parts, name, crs)?;
    report.messages.extend(notes);
    Ok((ds, report))
}

/// The files of a Shapefile besides its `.shp` that [`gather_shapefiles`] zips with it.
const SHAPEFILE_SIDECARS: [&str; 4] = ["dbf", "shx", "prj", "cpg"];

/// Gathers each Shapefile among files given together (several picked at once in the browser, or
/// dropped at once) into a zip that reads as a zipped Shapefile: a `.shp` with the `.dbf` of the
/// same base name, ignoring case, and its `.shx`, `.prj` and `.cpg` when they are there. The zip,
/// named after the `.shp`, takes its place; its parts go. Other files, and a `.shp` without its
/// `.dbf`, pass through as they came. A Shapefile whose parts are too large to read together (more
/// than [`MAX_BYTES`], or the zip reader's limit) is left out, and said why in the messages
/// returned with the files.
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
    // The zip must open again: within the read size and the zip reader's inflated total.
    let limit = crate::zip::MAX_TOTAL.min(usize::try_from(MAX_BYTES).unwrap_or(usize::MAX));
    let (mut out, mut problems) = (Vec::new(), Vec::new());
    for (file, parts) in files.into_iter().zip(zips) {
        out.extend(file);
        let Some(parts) = parts else {
            continue;
        };
        let shp = parts.first().map(|(n, _)| n.clone()).unwrap_or_else(|| "layer.shp".into());
        let zip_name = format!("{}.zip", Path::new(&shp).file_stem().and_then(|s| s.to_str()).unwrap_or("layer"));
        let total = parts.iter().fold(0usize, |sum, (_, b)| sum.saturating_add(b.len()));
        let too_large =
            || format!("{shp}: its Shapefile's files hold {} MB in all; at most {} MB can be read together", total >> 20, limit >> 20);
        if total > limit {
            problems.push(too_large());
            continue;
        }
        match crate::zip::write(&parts) {
            Ok(zip) if zip.len() as u64 <= MAX_BYTES => out.push((zip_name, zip)),
            _ => problems.push(too_large()),
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

/// Most layers read from one zip archive.
pub const MAX_ZIP_LAYERS: usize = 32;

/// Reads the vector data in a zip archive: each Shapefile in it (its `.shp` with the `.dbf`,
/// `.shx` and `.prj` beside it), then every other file the readers understand (GeoJSON, CSV,
/// GeoParquet, FlatGeobuf…; `.txt` files in archives are readmes, so they're skipped), at most
/// [`MAX_ZIP_LAYERS`]. Folders inside are fine; macOS
/// resource forks (`__MACOSX/`, `._*`) are ignored.
pub fn read_zip(file_name: &str, bytes: &[u8]) -> Result<Vec<(Dataset, ReadReport)>> {
    read_zip_in(file_name, bytes, None)
}

/// Reads a ZIP, optionally overriding the source CRS of every Shapefile in it. Other formats
/// retain their own coordinate systems; a CityJSON model takes it only when it names none RustGIS
/// knows. The override is applied before reading any shapes.
pub fn read_zip_in(file_name: &str, bytes: &[u8], crs: Option<Crs>) -> Result<Vec<(Dataset, ReadReport)>> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(DataError::Format(format!("file is too large ({} bytes)", bytes.len())));
    }
    if let Some(c) = crs {
        c.transformer().map_err(DataError::Format)?;
    }
    let lower = |n: &str| n.to_ascii_lowercase();
    let ext_of = |n: &str| Path::new(n).extension().and_then(|e| e.to_str()).map(lower).unwrap_or_default();
    // A `.pbf` is read only as an OpenStreetMap extract (`.osm.pbf`): a vector tile set's
    // `z/x/y.pbf` files aren't extracted at all.
    let wanted = |n: &str| {
        let e = ext_of(n);
        !is_resource_fork(n)
            && (matches!(e.as_str(), "shp" | "shx" | "dbf" | "prj" | "cpg" | "mid")
                || (e == "pbf" && lower(n).ends_with(".osm.pbf"))
                || (!matches!(e.as_str(), "zip" | "txt" | "pbf") && READ_EXTENSIONS.contains(&e.as_str())))
    };
    let entries = crate::zip::read_matching(bytes, wanted).map_err(|e| DataError::Format(format!("{file_name}: {e}")))?;
    // Sidecar files by their path without the extension, compared without regard to case.
    let stem = |n: &str| lower(n.get(..n.len().saturating_sub(ext_of(n).len() + 1)).unwrap_or(n));
    // Entries are taken out as they're used, so no file's bytes are copied.
    let mut entries: Vec<Option<(String, Vec<u8>)>> = entries.into_iter().map(Some).collect();
    let mut shps: Vec<String> = entries.iter().flatten().filter(|(n, _)| ext_of(n) == "shp").map(|(n, _)| n.clone()).collect();
    shps.sort();
    let take = |entries: &mut Vec<Option<(String, Vec<u8>)>>, base: &str, ext: &str| -> Option<(String, Vec<u8>)> {
        entries.iter_mut().find(|e| e.as_ref().is_some_and(|(n, _)| ext_of(n) == ext && stem(n) == base))?.take()
    };
    let mut out = Vec::new();
    // Members that couldn't be read are skipped, and said in the first layer's report.
    let mut skipped: Vec<String> = Vec::new();
    for n in shps {
        if out.len() >= MAX_ZIP_LAYERS {
            break;
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
            Err(e) => skipped.push(format!("{n} ({e})")),
        }
    }
    // MapInfo files with their .mid.
    let mut mifs: Vec<String> = entries.iter().flatten().filter(|(n, _)| ext_of(n) == "mif").map(|(n, _)| n.clone()).collect();
    mifs.sort();
    for n in mifs {
        if out.len() >= MAX_ZIP_LAYERS {
            break;
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
            Err(e) => skipped.push(format!("{n} ({e})")),
        }
    }
    let mut others: Vec<(String, Vec<u8>)> = entries
        .into_iter()
        .flatten()
        .filter(|(n, _)| !matches!(ext_of(n).as_str(), "shp" | "shx" | "dbf" | "prj" | "cpg" | "mid"))
        .collect();
    others.sort_by(|a, b| a.0.cmp(&b.0));
    for (n, b) in others {
        if out.len() >= MAX_ZIP_LAYERS {
            skipped.push(format!("{n} (past the {MAX_ZIP_LAYERS}-layer limit)"));
            continue;
        }
        let leaf = n.rsplit('/').next().unwrap_or(&n);
        // A city model has many layers.
        let read = if is_cityjson(leaf, &b) { crate::cityjson::read_member(leaf, &b, crs) } else { read_bytes(leaf, &b).map(|r| vec![r]) };
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
            Err(e) => skipped.push(format!("{n} ({e})")),
        }
    }
    if out.is_empty() {
        let why = if skipped.is_empty() { String::new() } else { format!(": {}", skipped.join("; ")) };
        return Err(DataError::Format(format!("{file_name}: no Shapefile or other vector data in the archive could be read{why}")));
    }
    if let Some((_, report)) = out.first_mut()
        && !skipped.is_empty()
    {
        report.messages.push(format!("{file_name}: skipped {}", skipped.join("; ")));
    }
    Ok(out)
}

/// The files of a Shapefile, read into memory.
struct ShapefileParts {
    shp: Vec<u8>,
    shx: Option<Vec<u8>>,
    dbf: Vec<u8>,
    prj: Option<String>,
    cpg: Option<Vec<u8>>,
}

/// Decodes attributes and shapes together, tracking geometry lost during CRS conversion so
/// a later projection definition cannot silently repair an incomplete layer.
fn read_shapefile_parts(mut parts: ShapefileParts, name: &str, crs: Option<Crs>) -> Result<(Dataset, ReadReport)> {
    use shapefile::Shape;
    use shapefile::dbase::FieldValue;
    use std::io::Cursor;
    let fmt = |e: shapefile::Error| DataError::Format(format!("Shapefile: {e}"));
    let mut encoding_notes = Vec::new();
    let encoding = crate::dbf::encoding(parts.cpg.as_deref(), parts.dbf.get(29).copied().unwrap_or(0), &mut encoding_notes);
    crate::dbf::readable_header_date(&mut parts.dbf);
    let dbf = shapefile::dbase::ReaderBuilder::new()
        .with_encoding(encoding.decoder())
        .build(Cursor::new(parts.dbf))
        .map_err(|e| DataError::Format(format!("Shapefile: the .dbf: {e}")))?;
    // Field order comes from the .dbf header (records are unordered maps).
    let order: Vec<(String, FieldType)> = dbf
        .fields()
        .iter()
        .filter(|f| f.name() != "DeletionFlag")
        .map(|f| {
            use shapefile::dbase::FieldType as D;
            let ty = match f.field_type() {
                D::Numeric | D::Float | D::Double | D::Currency => FieldType::Double,
                D::Integer => FieldType::Integer,
                D::Logical => FieldType::Boolean,
                _ => FieldType::Text,
            };
            (f.name().to_string(), ty)
        })
        .collect();
    let shapes = match parts.shx {
        Some(shx) => shapefile::ShapeReader::with_shx(Cursor::new(parts.shp), Cursor::new(shx)),
        None => shapefile::ShapeReader::new(Cursor::new(parts.shp)),
    }
    .map_err(fmt)?;
    let mut reader = shapefile::Reader::new(shapes, dbf);
    let fields: Vec<Field> = order.iter().map(|(n, t)| Field::new(n.clone(), *t)).collect();
    let mut ds = Dataset::new(name, GeometryType::None, fields);
    let mut report = ReadReport::default();
    report.messages.extend(encoding_notes);
    let to_lonlat = match crs {
        Some(c) => {
            ds.crs = Some(c);
            Some(c.transformer().map_err(DataError::Format)?)
        }
        None => shapefile_crs(parts.prj.as_deref(), &mut report).map(|(crs, t)| {
            ds.crs = Some(crs);
            t
        }),
    };
    let (mut converted, mut unconverted, mut zm_shapes) = (0usize, 0usize, 0usize);
    let mut source_kind = GeometryType::None;
    let pt = |x: f64, y: f64| Coord::new(x, y);
    for item in reader.iter_shapes_and_records() {
        let Ok((shape, record)) = item else {
            report.skipped += 1;
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
        let geom = match (&to_lonlat, geom) {
            (Some(t), Some(g)) => {
                let mut ok = true;
                let out = g.map_coords(&|c| t.to_lonlat(c).unwrap_or(Coord::new(f64::NAN, f64::NAN)));
                out.for_each_coord(|c| ok &= c.is_finite());
                converted += usize::from(ok);
                unconverted += usize::from(!ok);
                ok.then_some(out)
            }
            (_, g) => g,
        };
        let attrs = ds
            .fields
            .iter()
            .map(|f| match record.get(&f.name) {
                Some(FieldValue::Character(Some(s))) => Value::Text(s.trim().to_string()),
                Some(FieldValue::Numeric(Some(n))) if n.is_finite() => Value::Float(*n),
                Some(FieldValue::Float(Some(n))) => Value::Float(f64::from(*n)),
                Some(FieldValue::Integer(i)) => Value::Int(i64::from(*i)),
                Some(FieldValue::Double(d)) => Value::Float(*d),
                Some(FieldValue::Currency(d)) => Value::Float(*d),
                Some(FieldValue::Logical(Some(b))) => Value::Bool(*b),
                Some(FieldValue::Memo(s)) => Value::Text(s.clone()),
                Some(FieldValue::Date(Some(d))) => Value::Text(format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())),
                _ => Value::Null,
            })
            .collect();
        ds.push_next(geom, attrs);
    }
    ds.drop_zm(&mut report, zm_shapes);
    ds.geometry_type = dominant_type(&ds);
    // A bad .prj must not turn a spatial layer into an attribute-only table and hide the
    // conversion warning. Its shapes can be recovered by importing with an override.
    if ds.geometry_type == GeometryType::None && unconverted > 0 {
        ds.geometry_type = source_kind;
    }
    integral_numeric_to_integer(&mut ds);
    let e = ds.extent();
    if !e.is_empty() && (e.min_x < -180.5 || e.max_x > 180.5 || e.min_y < -90.5 || e.max_y > 90.5) {
        report.messages.push("coordinates are not longitude/latitude; RustGIS reads them as WGS 84 and they may draw out of place".into());
    }
    if report.skipped > 0 {
        report.messages.push(format!("{} shapes were unreadable or unsupported", report.skipped));
    }
    ds.crs_incomplete = unconverted > 0;
    if unconverted > 0 && converted == 0 {
        report.messages.push("no shape could be converted to longitude/latitude: the .prj doesn't seem to match the coordinates".into());
    } else if unconverted > 0 {
        report.messages.push(format!("{unconverted} shapes couldn't be converted to longitude/latitude and have no geometry"));
    }
    Ok((ds, report))
}

/// Converts a Shapefile's coordinates to longitude/latitude, from the coordinate system in its
/// `.prj`: `None` for longitude/latitude, or when there's no `.prj` or it isn't understood (said in
/// the report).
fn shapefile_crs(prj: Option<&str>, report: &mut ReadReport) -> Option<(Crs, crate::raster::Transformer)> {
    let crs = match crate::raster::wkt::read_wkt(prj?) {
        Ok((c, note)) => {
            if let Some(n) = note {
                report.messages.push(format!("the .prj file: {n}"));
            }
            c
        }
        Err(e) => {
            report.messages.push(format!("the .prj file: {e}; coordinates are read as longitude/latitude"));
            return None;
        }
    };
    if crs == crate::raster::Crs::LonLat {
        return None;
    }
    match crs.transformer() {
        Ok(t) => Some((crs, t)),
        Err(e) => {
            report.messages.push(format!("the .prj file: {e}; coordinates are read as longitude/latitude"));
            None
        }
    }
}

/// The .dbf header doesn't say whether a Numeric column has decimals, so a Double column whose
/// values are all whole numbers within i64 range is read as Integer.
fn integral_numeric_to_integer(ds: &mut Dataset) {
    const LIMIT: f64 = 9.0e15;
    for fi in 0..ds.fields.len() {
        if ds.fields.get(fi).map(|f| f.ty) != Some(FieldType::Double) {
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
const WGS84_PRJ: &str = "GEOGCS[\"GCS_WGS_1984\",DATUM[\"D_WGS_1984\",SPHEROID[\"WGS_1984\",6378137.0,298.257223563]],PRIMEM[\"Greenwich\",0.0],UNIT[\"Degree\",0.0174532925199433]]";

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
    let renamed: Vec<(&str, String)> =
        fields.iter().zip(dbf_names(fields)).filter(|(f, n)| f.name != *n).map(|(f, n)| (f.name.as_str(), n)).collect();
    if renamed.is_empty() {
        return None;
    }
    // A name read from a file, like a path or layer name, can hold anything, at any length: the
    // notice stays one line, reads in the order it is written, and copies no more of a field name
    // than it shows.
    let target: String = target.chars().map(dbf_notice_char).collect();
    let listed: Vec<String> =
        renamed.iter().take(5).map(|(old, new)| format!("'{}' → '{}'", dbf_notice_name(old), dbf_notice_name(new))).collect();
    let more = if renamed.len() > 5 { format!(" and {} more", renamed.len() - 5) } else { String::new() };
    let count = if renamed.len() == 1 { "1 field name was".to_string() } else { format!("{} field names were", renamed.len()) };
    Some(format!("{target}: {count} changed to fit a Shapefile (10 bytes at most, unique ignoring case): {}{more}", listed.join(", ")))
}

/// Keeps a diagnostic on one line and prevents field names from reordering its text.
fn dbf_notice_char(c: char) -> char {
    let reorders = matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}');
    if c.is_control() || reorders || matches!(c, '\u{2028}' | '\u{2029}') { ' ' } else { c }
}

/// A bounded field name for a dBase notice, with controls and directional marks flattened.
fn dbf_notice_name(name: &str) -> String {
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
                FieldType::Text if value.to_string().len() > 254 => loss.text += 1,
                FieldType::Integer | FieldType::Double => {
                    let d = decimals.get(k).copied().unwrap_or(0);
                    match dbf_number(value, field.ty, d) {
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
/// value that isn't a number, or one rejected by the writer's size and precision checks.
/// Rejected numbers are left empty rather than cut short.
fn dbf_number(v: &Value, ty: FieldType, d: usize) -> Option<f64> {
    let x = v.as_f64().filter(|x| {
        // A whole number past 2^53 would be held only rounded (dBase's decimal text goes through
        // f64), and one too small for the field's decimals would read 0.
        let exact = !(matches!(v, Value::Int(i) if i.unsigned_abs() > 1 << 53)
            || ty == FieldType::Double && *x != 0.0 && x.abs() < 0.5 * 10f64.powi(-(d as i32)));
        exact && format!("{x:.d$}").len() <= if ty == FieldType::Integer { 18 } else { 24 }
    })?;
    Some(if ty == FieldType::Integer { x.round() } else { x })
}

/// The Boolean values the dBase writer accepts; an incompatible value is left empty.
fn dbf_boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Int(i) => Some(*i != 0),
        _ => None,
    }
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
    use shapefile::dbase::{FieldName, FieldValue, Record, TableWriterBuilder};
    use std::io::Cursor;
    let fmt = |e: shapefile::Error| DataError::Format(format!("Shapefile: {e}"));
    let names = dbf_names(&ds.fields);
    let dbf_name = |n: &str| FieldName::try_from(n).map_err(|e| DataError::Format(format!("Shapefile: field {n}: {e}")));
    let mut table = TableWriterBuilder::new();
    // Per field: decimals of a Double field (numbers too large for 24 characters become null).
    let mut decimals = vec![0usize; ds.fields.len()];
    for (k, (f, n)) in ds.fields.iter().zip(&names).enumerate() {
        table = match f.ty {
            // 18 digits: GDAL reads a wider field as a real number.
            FieldType::Integer => table.add_numeric_field(dbf_name(n)?, 18, 0),
            FieldType::Double => {
                let d = dbf_decimals(ds, k);
                if let Some(slot) = decimals.get_mut(k) {
                    *slot = d;
                }
                table.add_numeric_field(dbf_name(n)?, 24, u8::try_from(d).unwrap_or(15))
            }
            FieldType::Boolean => table.add_logical_field(dbf_name(n)?),
            FieldType::Text => {
                let longest = ds.features.iter().filter_map(|r| r.attrs.get(k)).map(|v| v.to_string().len()).max().unwrap_or(0);
                table.add_character_field(dbf_name(n)?, u8::try_from(longest.clamp(1, 254)).unwrap_or(254))
            }
        };
    }
    let record = |r: &crate::Feature| -> Record {
        let mut rec = Record::default();
        for (k, (f, n)) in ds.fields.iter().zip(&names).enumerate() {
            let v = r.attrs.get(k).unwrap_or(&Value::Null);
            let value = match f.ty {
                FieldType::Integer | FieldType::Double => FieldValue::Numeric(dbf_number(v, f.ty, decimals.get(k).copied().unwrap_or(0))),
                FieldType::Boolean => FieldValue::Logical(dbf_boolean(v)),
                FieldType::Text => FieldValue::Character((!v.is_null()).then(|| cut(&v.to_string(), 254).to_string())),
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
    let mut parts = vec![("shp", shp.into_inner()), ("shx", shx.into_inner()), ("dbf", dbf.into_inner())];
    parts.extend(prj.map(|p| ("prj", p.into_bytes())));
    parts.push(("cpg", b"UTF-8".to_vec()));
    Ok((parts, left_out))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Exercises caller cleanup through the public Shapefile writer, one fresh process per collision.
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
            for index in 0..parts.len() {
                for kind in kinds {
                    let output = std::process::Command::new(std::env::current_exe()?)
                        .args([
                            "--exact",
                            "io::tests::shapefile_staging_collisions_preserve_existing_entries",
                            "--nocapture",
                            "--test-threads=1",
                        ])
                        .env(CHILD, format!("{index}:{kind}"))
                        .output()?;
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    assert!(
                        output.status.success()
                            && stdout.contains("test result: ok. 1 passed; 0 failed; 0 ignored;")
                            && stdout.contains(&format!("collision case completed: {index}:{kind}")),
                        "part={index} kind={kind}: {}{}",
                        stdout,
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
            println!("collision scenarios: {}", parts.len() * kinds.len());
            return Ok(());
        };
        let (index, kind) = case.split_once(':').ok_or_else(|| std::io::Error::other("missing collision case"))?;
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
        let (extension, _) = parts.get(index).ok_or_else(|| std::io::Error::other("collision part out of range"))?;
        let target = destination(extension);
        // This child runs only this test: write_path's first staging counter is zero.
        let mut name = std::ffi::OsString::from(".");
        name.push(target.file_name().ok_or_else(|| std::io::Error::other("missing part name"))?);
        name.push(format!(".{}.{index}.tmp", std::process::id()));
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
                assert_eq!(report.messages, ["1 features had unreadable geometry"]);
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
        let Some(Geometry::Point(c)) = &ds.features[0].geometry else { panic!() };
        assert!((c.x + 73.96).abs() < 0.05 && (c.y - 40.78).abs() < 0.05, "{c:?}");
        assert_eq!(rep.messages, ["1 rows had no usable coordinates"]);
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
        let e = ds.features[0].geometry.as_ref().unwrap().extent();
        assert!((e.min_x - 15.0).abs() < 0.01 && (e.min_y - 42.0).abs() < 0.05, "{e:?}");
        assert!(ds.features[3].geometry.is_none());
        assert_eq!(
            rep.messages,
            [
                crate::zm_note(Some(1)).as_str(),
                "1 rows had geometry that isn't readable WKT",
                "1 rows have another kind of geometry than the layer's polyline"
            ]
        );
        // A named WKT column with a given system.
        let opts = CsvOptions { wkt: Some("g".into()), crs: Some(Crs::WebMercator), ..CsvOptions::default() };
        let (ds, _) =
            read_csv_with("g|v\nPOLYGON ((0 0, 1000 0, 1000 1000, 0 0))|1\n", "t", &CsvOptions { delimiter: Some(b'|'), ..opts }).unwrap();
        assert_eq!(ds.geometry_type, GeometryType::Polygon);
        assert_eq!(ds.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["v"]);
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
        assert!(ds.zm_dropped && rep.messages.contains(&crate::zm_note(Some(1))), "{:?}", rep.messages);
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
        assert_eq!(rep.messages, ["1 features had unreadable geometry"]);
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
            crate::zm_export_note([&ds, &flat, &other], "site.zip").as_deref(),
            Some("site.zip: its geometry has no Z or M values for 2 layers (pts, wells), which were dropped when they were read")
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
            [
                "out.geojson: the coordinates of utm aren't longitude/latitude, so they're out of place: define its projection and export again"
            ]
        );
        assert!(crate::export_notes([(&lonlat, ext(&lonlat))], "out.geojson", Crs::LonLat).is_empty());

        // NAD 27 without TOWGS84, as its WKT is kept with the layer and saved with the project.
        let wkt = r#"PROJCS["NAD_1927_UTM_Zone_16N",GEOGCS["GCS_North_American_1927",DATUM["D_North_American_1927",SPHEROID["Clarke_1866",6378206.4,294.9786982]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",-87.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
        let (nad27, note) = crate::raster::wkt::read_wkt(wkt).unwrap();
        assert!(note.is_some());
        let mut parcels = lonlat.clone();
        parcels.name = "parcels".into();
        parcels.crs = Some(nad27);
        let saved: Dataset = serde_json::from_value(serde_json::to_value(&parcels).unwrap()).unwrap();
        assert_eq!(
            crate::export_notes([(&saved, ext(&saved))], "parcels.geojson", Crs::LonLat),
            [format!(
                "parcels.geojson: the positions of parcels are approximate, possibly a few hundred metres off: its coordinate system ({}) has no shift to WGS 84",
                nad27.label()
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
            [
                "map.json: the positions of 2 layers (parcels, roads) are approximate, possibly a few hundred metres off: their coordinate systems have no shift to WGS 84"
            ]
        );
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
        let c = csv_columns("\u{feff}name;Lat;Lon\nA;1;2\n", None).unwrap();
        assert_eq!(c.names, ["name", "Lat", "Lon"]);
        assert_eq!((c.x.as_deref(), c.y.as_deref(), c.wkt.as_deref(), c.delimiter), (Some("Lon"), Some("Lat"), None, Some(b';')));
        // Longitude/latitude over easting/northing; easting/northing as a pair only.
        let c = csv_columns("easting,northing,lon,lat\n", None).unwrap();
        assert_eq!((c.x.as_deref(), c.y.as_deref()), (Some("lon"), Some("lat")));
        let c = csv_columns("easting,lat\n1,2\n", None).unwrap();
        assert_eq!((c.x, c.y), (None, None));
        // A WKT column by its name and first value, not by its name alone.
        let c = csv_columns("id,geometry\n1,\n2,POINT (1 2)\n", None).unwrap();
        assert_eq!(c.wkt.as_deref(), Some("geometry"));
        assert_eq!(csv_columns("id,geometry\n1,not wkt\n", None).unwrap().wkt, None);
        // The separator given splits the header.
        let c = csv_columns("a;b,c\n", Some(b',')).unwrap();
        assert_eq!((c.names, c.delimiter), (vec!["a;b".to_string(), "c".to_string()], Some(b',')));
        // Nothing to read is no columns, not a crash.
        assert!(csv_columns("", None).unwrap().names.is_empty());
        let _ = csv_columns("\"unclosed\n", None);
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

    #[test]
    fn csv_points() {
        let (ds, rep) = read_csv("name,Lat,Lon,n\nA,10,20,1\nB,bad,1,2\n", "t").unwrap();
        assert_eq!(ds.geometry_type, GeometryType::Point);
        assert_eq!(rep.skipped, 1);
        assert_eq!(ds.features[0].geometry, Some(Geometry::Point(Coord::new(20.0, 10.0))));
        assert!(to_csv(&ds).unwrap().contains("longitude"));
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
        assert!(out.lines().next().unwrap().contains("longitude_1"), "{out}");
        let (ds, _) = read_csv("a\nNaN\nx\n", "t").unwrap();
        assert_eq!(ds.fields[0].ty, FieldType::Text);
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
        let at = |ds: &Dataset| ds.features[0].geometry.as_ref().and_then(Geometry::label_point).unwrap();

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
    /// An ESRI .prj without an authority retains the national datum shift (issue #807).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn shapefile_esri_national_datum() {
        use shapefile::dbase::{Record, TableWriterBuilder};
        let dir = tempfile::tempdir().unwrap();
        let shp = dir.path().join("milan.shp");
        {
            let mut writer = shapefile::Writer::from_path(&shp, TableWriterBuilder::new()).unwrap();
            writer
                .write_shape_and_record(&shapefile::Point::new(1_514_881.490_594_152, 5_034_556.812_441_661), &Record::default())
                .unwrap();
        }
        let row = include_str!("raster/esri_datum_corpus.tsv").lines().find(|l| l.starts_with("3003\t")).unwrap();
        std::fs::write(shp.with_extension("prj"), row.split('\t').nth(5).unwrap()).unwrap();
        let (ds, report) = read_path(&shp).unwrap();
        let c = ds.features[0].geometry.as_ref().and_then(Geometry::label_point).unwrap();
        assert!((c.x - 9.19).abs() < 1e-7 && (c.y - 45.4642).abs() < 1e-7, "{c:?}");
        assert!(report.messages.is_empty(), "{:?}", report.messages);
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
        let c = ds.features[0].geometry.as_ref().and_then(Geometry::label_point).unwrap();
        assert!((c.x - 2.335).abs() < 1e-6 && (c.y - 48.855).abs() < 1e-6, "{c:?}");
        assert!(rep.messages.is_empty(), "{:?}", rep.messages);
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
        assert!(layers[0].1.messages.is_empty(), "{:?}", layers[0].1.messages);
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
        big.push(("huge.dbf".to_string(), vec![0; MAX_BYTES as usize]));
        let (out, problems) = gather_shapefiles(big);
        assert_eq!(names(&out), ["rivers.zip"]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("huge.shp:") && problems[0].contains("MB"), "{problems:?}");
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
}
