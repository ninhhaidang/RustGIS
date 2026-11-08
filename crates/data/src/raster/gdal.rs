//! GDAL, optionally, for rasters: formats the built-in readers don't know (JPEG 2000, ERDAS
//! Imagine, SRTM HGT, DTED, NetCDF, HDF, ENVI, SAGA…) read through GDAL's `gdal_translate` when it
//! is installed.
//!
//! A file (or one of its subdatasets, such as a NetCDF variable) is converted once into a
//! cloud-optimised GeoTIFF with overviews in the cache folder, then read as a GeoTIFF, so large
//! rasters open from their overviews. Nothing links to GDAL: without the program these formats
//! say how to get it.

use std::path::{Path, PathBuf};

use crate::source::external::{self, TempFile, fnv1a, output_with_budget, stamp, touch};
use crate::source::gdal::command;
use crate::{DataError, Result};

/// Raster file types read through GDAL. (Not `.vrt`, which can point GDAL at any file on the
/// machine, nor `.xyz`, more often a point cloud than a grid.)
pub const EXTENSIONS: &[&str] = &[
    "jp2", "j2k", "img", "hgt", "dem", "dt0", "dt1", "dt2", "nc", "nc4", "hdf", "hdf5", "h5", "he5", "bil", "bsq", "bip", "envi", "grd",
    "rst", "sdat", "kea", "ers", "ntf", "nitf", "ecw", "sid", "gsb", "bt", "rsw",
];

/// Most subdatasets listed from one file.
const MAX_SUBDATASETS: usize = 256;

/// Whether a file name is a raster read through GDAL.
pub fn is_gdal_raster_name(name: &str) -> bool {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    EXTENSIONS.contains(&ext.as_str())
}

/// The GDAL drivers that may open a raster, by its type (`-if`). GDAL picks a driver from a file's
/// contents, so without them a VRT renamed `.img` would read whatever files or addresses it names.
/// A driver GDAL wasn't built with is only warned about; other types are refused. A subdataset
/// opens through its file's drivers (HDF's through its `Image` driver).
fn drivers(src: &Path) -> Result<&'static [&'static str]> {
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    Ok(match ext.as_str() {
        "jp2" | "j2k" => &["JP2OpenJPEG", "JP2KAK", "JP2ECW", "JP2MrSID"],
        // ERDAS Imagine, or raw cells with an ENVI or EHdr `.hdr` header beside them.
        "img" => &["HFA", "ENVI", "EHdr"],
        "hgt" => &["SRTMHGT"],
        // A USGS DEM, or a GTOPO30 tile.
        "dem" => &["USGSDEM", "EHdr"],
        "dt0" | "dt1" | "dt2" => &["DTED"],
        // NetCDF-4 is HDF5, and either driver may be the one built.
        "nc" | "nc4" | "hdf5" | "h5" | "he5" => &["netCDF", "HDF5", "HDF5Image"],
        "hdf" => &["HDF4", "HDF4Image", "HDF5", "HDF5Image"],
        "bil" | "bsq" | "bip" => &["EHdr", "ENVI", "GenBin"],
        "envi" => &["ENVI"],
        // Surfer grids, Northwood grids, and GMT's (NetCDF).
        "grd" => &["GSAG", "GSBG", "GS7BG", "NWT_GRD", "NWT_GRC", "netCDF"],
        "rst" => &["RST"],
        "sdat" => &["SAGA"],
        "kea" => &["KEA"],
        "ers" => &["ERS"],
        "ntf" | "nitf" => &["NITF"],
        "ecw" => &["ECW"],
        "sid" => &["MrSID"],
        "gsb" => &["NTv2"],
        "bt" => &["BT"],
        "rsw" => &["RMF"],
        _ => return Err(DataError::Unsupported(format!("{}: not a raster type read through GDAL", src.display()))),
    })
}

fn program(name: &str) -> Option<PathBuf> {
    external::program("RUSTGIS_GDAL_BIN", name)
}

/// Whether GDAL's raster programs are installed.
pub fn available() -> bool {
    program("gdal_translate").is_some() && program("gdalinfo").is_some()
}

/// Whether a file's contents are JPEG 2000 (see [`super::jpeg2000_extension`]).
pub fn is_jpeg2000(bytes: &[u8]) -> bool {
    super::jpeg2000_extension(bytes).is_some()
}

/// What converting a file's contents made: the GeoTIFF's bytes, and whether the raster was taken
/// as WGS 84 (see [`Converted::assumed_lonlat`]).
pub struct ConvertedBytes {
    pub tif: Vec<u8>,
    pub assumed_lonlat: bool,
}

/// Converts a JPEG 2000 file's contents (an upload or download) to a cloud-optimised GeoTIFF's
/// bytes, of at most `max` bytes; `crs` as for [`convert`]. Only JPEG 2000 is taken, by its
/// signature: other contents could be a format that opens files on this computer (a VRT).
/// Source, output and scratch files stay in a private temporary folder. Its monitored budget
/// allows the source plus three `max`-sized working copies; writes may overshoot between checks.
/// Takes ownership so the input buffer is released after staging, before conversion/readback.
pub fn convert_jpeg2000(bytes: Vec<u8>, crs: Option<&str>, max: u64) -> Result<ConvertedBytes> {
    let Some(ext) = super::jpeg2000_extension(&bytes) else {
        return Err(DataError::Format("not a JPEG 2000 file (its contents don't start as one)".into()));
    };
    let mut workspace = tempfile::Builder::new();
    workspace.prefix("rustgis-gdal-");
    #[cfg(unix)]
    workspace.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700));
    let dir = workspace.tempdir()?;
    let src = dir.path().join(format!("download.{ext}"));
    {
        use std::io::Write;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let mut f = o.open(&src)?;
        f.write_all(&bytes)?;
    }
    // COG generation can hold an intermediate image and overviews alongside the final output.
    // Keep that working-space budget separate from the final GeoTIFF's size limit.
    let budget = (bytes.len() as u64).saturating_add(max.saturating_mul(3));
    drop(bytes);
    let c = convert_in(dir.path(), &src, None, crs, Some(budget))?;
    // A GeoTIFF is larger than the JPEG 2000 it came from (Sentinel-2's about twice).
    if std::fs::metadata(&c.path)?.len() > max {
        return Err(DataError::Format(format!(
            "converted to a GeoTIFF larger than {} MB, too large to open: save the file and add it with Add Data",
            max >> 20
        )));
    }
    Ok(ConvertedBytes { tif: std::fs::read(&c.path)?, assumed_lonlat: c.assumed_lonlat })
}

fn missing() -> DataError {
    DataError::Unsupported(
        "this raster format needs GDAL: install it (gdal_translate and gdalinfo on the PATH, or RUSTGIS_GDAL_BIN naming their folder) and add the file again"
            .into(),
    )
}

/// Refuses an ER Mapper header that would open more than its own data: any but an `ERStorage` one
/// (a `Translated` one's data file GDAL opens as any format, so a VRT beside it would read whatever
/// it names), or one whose data file isn't a file beside it (GDAL before 3.12 follows `../`).
/// Other files pass.
fn plain_ers(src: &Path) -> Result<()> {
    if !src.extension().is_some_and(|e| e.eq_ignore_ascii_case("ers")) {
        return Ok(());
    }
    // A header is a few kilobytes of text.
    const MAX_HEADER: u64 = 1 << 20;
    let meta = std::fs::metadata(src)?;
    // A pipe or device named `.ers` would never finish being read.
    if !meta.is_file() {
        return Err(DataError::Format("not a file: an ER Mapper header is one".into()));
    }
    if meta.len() > MAX_HEADER {
        return Err(DataError::Format("too large for an ER Mapper header".into()));
    }
    let text = String::from_utf8_lossy(&std::fs::read(src)?).into_owned();
    // Every line naming a key, so a second one can't hide behind the first.
    let values = |key: &str| {
        // GDAL ends a line at a carriage return too.
        text.split(['\r', '\n'])
            .filter_map(|l| l.split_once('='))
            .filter(|(k, _)| k.trim().eq_ignore_ascii_case(key))
            .map(|(_, v)| v.trim().trim_matches('"').to_string())
            .collect::<Vec<_>>()
    };
    // Only raw data of its own: each type's first word, so trailing text can't change it.
    let own = |t: &String| t.split_whitespace().next().is_some_and(|w| w.trim_matches('"').eq_ignore_ascii_case("ERStorage"));
    if !values("DataSetType").iter().all(own) {
        return Err(DataError::Unsupported(
            "an ER Mapper header that isn't raw data of its own (DataSetType = ERStorage), such as one linking to another file, isn't read"
                .into(),
        ));
    }
    if values("DataFile").iter().any(|f| f.contains(['/', '\\', ':']) || f == "..") {
        return Err(DataError::Unsupported("an ER Mapper header whose data file isn't beside it isn't read".into()));
    }
    Ok(())
}

/// The subdatasets of a file (NetCDF variables, HDF datasets): GDAL's name for each, and its
/// description. Empty for a file that is one raster.
pub fn subdatasets(src: &Path) -> Result<Vec<(String, String)>> {
    subdatasets_with_budget(src, None)
}

/// Lists subdatasets with the downloaded raster's optional workspace budget.
fn subdatasets_with_budget(src: &Path, workspace: Option<(&Path, u64)>) -> Result<Vec<(String, String)>> {
    let full = std::path::absolute(src).unwrap_or_else(|_| src.to_path_buf());
    // Its type and header first, so a file that isn't read says so whether GDAL is installed or not.
    let drivers = drivers(&full)?;
    plain_ers(&full)?;
    let gdalinfo = program("gdalinfo").ok_or_else(missing)?;
    let mut c = command(gdalinfo, drivers);
    // With the metadata (`-nomd` would drop the SUBDATASETS domain too, `-mdd` or not).
    c.arg("-json").arg("-nogcp").arg("-norat").arg("-noct").arg(&full);
    let out = output_with_budget(c, LIST_TIMEOUT, "GDAL", workspace)?;
    if !out.status.success() {
        return Err(DataError::Format(format!("GDAL: {}", first_error(&out.stderr))));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| DataError::Format(format!("GDAL: unreadable gdalinfo output ({e})")))?;
    let Some(md) = json.get("metadata").and_then(|m| m.get("SUBDATASETS")).and_then(|m| m.as_object()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for i in 1..=MAX_SUBDATASETS {
        let Some(name) = md.get(&format!("SUBDATASET_{i}_NAME")).and_then(|v| v.as_str()) else { break };
        let desc = md.get(&format!("SUBDATASET_{i}_DESC")).and_then(|v| v.as_str()).unwrap_or(name);
        out.push((name.to_string(), desc.to_string()));
    }
    Ok(out)
}

/// What GDAL says of a raster (a file or a subdataset).
struct Described {
    /// It says its coordinate system.
    has_srs: bool,
    /// That system is geographic (longitude/latitude).
    geographic: bool,
    /// Its corner coordinates (west, south, east, north) in its own units, when it is
    /// georeferenced at all.
    extent: Option<[f64; 4]>,
    /// Its cell width in its own units.
    cell: Option<f64>,
}

/// What GDAL says of a raster (a file or subdataset, as GDAL names it).
fn describe(input: &std::ffi::OsStr, drivers: &[&str], workspace: Option<(&Path, u64)>) -> Result<Described> {
    let gdalinfo = program("gdalinfo").ok_or_else(missing)?;
    let mut c = command(gdalinfo, drivers);
    c.args(["-json", "-nogcp", "-nomd", "-norat", "-noct"]).arg(input);
    let out = output_with_budget(c, LIST_TIMEOUT, "GDAL", workspace)?;
    if !out.status.success() {
        return Err(DataError::Format(format!("GDAL: {}", first_error(&out.stderr))));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| DataError::Format(format!("GDAL: unreadable gdalinfo output ({e})")))?;
    let wkt = json.get("coordinateSystem").and_then(|c| c.get("wkt")).and_then(|w| w.as_str()).unwrap_or("").trim();
    let has_srs = !wkt.is_empty();
    let geographic = wkt.starts_with("GEOGCRS") || wkt.starts_with("GEOGCS");
    // Without a geotransform GDAL gives pixel corners, not coordinates.
    let georeferenced = json.get("geoTransform").is_some_and(|g| g.is_array());
    let cell = json.get("geoTransform").and_then(|g| g.get(1)).and_then(|w| w.as_f64()).map(f64::abs).filter(|w| w.is_finite());
    let corner = |k: &str| -> Option<(f64, f64)> {
        let v = json.get("cornerCoordinates")?.get(k)?.as_array()?;
        Some((v.first()?.as_f64()?, v.get(1)?.as_f64()?))
    };
    let extent = match (corner("upperLeft"), corner("lowerRight")) {
        (Some((w, n)), Some((e, s))) if georeferenced => Some([w.min(e), s.min(n), w.max(e), s.max(n)]),
        _ => None,
    };
    Ok(Described { has_srs, geographic, extent, cell })
}

/// Whether a longitude/latitude grid runs from 0 to 360 round the world, so it is moved to
/// -180–180 (or half wouldn't draw). Its west edge is half a cell before 0 when its cell centres
/// start there (a 2.5° grid's at -1.25). A regional grid past 180° isn't: it would become a
/// mostly empty global raster at its own cell size.
fn wraps(extent: [f64; 4], cell: Option<f64>) -> bool {
    let slack = cell.unwrap_or(0.0).clamp(1.0, 10.0);
    extent[2] > 180.5 && extent[0] >= -slack && extent[2] - extent[0] >= 300.0
}

/// Whether corner coordinates look like longitude/latitude (0–360 longitudes included), with half
/// a cell of slack at the poles.
fn looks_geographic(e: [f64; 4]) -> bool {
    let [w, s, east, n] = e;
    w >= -181.0 && east <= 361.0 && s >= -92.0 && n <= 92.0 && w < east && s < n
}

/// The short name of a subdataset: the part after its last `:` (a NetCDF variable's name).
pub fn short_name(subdataset: &str) -> &str {
    subdataset.rsplit(':').next().unwrap_or(subdataset).trim_matches('"')
}

/// What a conversion made: the GeoTIFF, the subdataset it came from, and the file's other
/// subdatasets (by short name).
#[derive(Clone, Debug, PartialEq)]
pub struct Converted {
    pub path: PathBuf,
    pub subdataset: Option<String>,
    pub others: Vec<String>,
    /// The file didn't say its coordinate system and its coordinates looked like
    /// longitude/latitude, so it was taken as WGS 84.
    pub assumed_lonlat: bool,
}

/// Converts a raster file, or its subdataset `layer` (by short or full name; the first when
/// none is given), to a GeoTIFF in the cache, reusing an earlier conversion of the same file.
/// `crs` gives the coordinate system of a file that doesn't say.
pub fn convert(src: &Path, layer: Option<&str>, crs: Option<&str>) -> Result<Converted> {
    let dir = external::cache_dir("RUSTGIS_GDAL_CACHE", "gdal")
        .ok_or_else(|| DataError::Format("no cache folder for GDAL conversions".into()))?;
    convert_in(&dir, src, layer, crs, None)
}

/// Whether a coordinate system given for GDAL is one: an authority code (`EPSG:32633`), an OGC
/// URN, a PROJ string or WKT — never a file path or address, which GDAL would open.
fn valid_crs(c: &str) -> bool {
    let c = c.trim();
    let up = c.to_ascii_uppercase();
    let code = |s: &str| {
        s.split_once(':').is_some_and(|(auth, id)| {
            matches!(auth, "EPSG" | "ESRI" | "OGC" | "IGNF")
                && !id.is_empty()
                && id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '.')
        })
    };
    let wkt = ["PROJCS", "GEOGCS", "PROJCRS", "GEOGCRS", "GEODCRS", "COMPD_CS", "COMPOUNDCRS", "LOCAL_CS", "ENGCRS", "BOUNDCRS"]
        .iter()
        .any(|k| up.starts_with(k))
        && c.contains('[');
    // PROJ parameters that name grid or definition files PROJ would look up, or build a
    // pipeline, are refused (the same check as for typed definitions).
    let proj = c.starts_with("+proj=")
        && c.chars().all(|ch| ch.is_ascii_alphanumeric() || " +=_.,-".contains(ch))
        && !c.split('+').any(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            super::crs::proj_key_refused(k, v)
        });
    let urn = up.starts_with("URN:OGC:DEF:CRS:") && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ":._-".contains(ch));
    !c.chars().any(char::is_control) && (code(&up) || wkt || proj || urn)
}

/// The file a GDAL subdataset name points into: its quoted path (`NETCDF:"/d/x.nc":air`,
/// `HDF4_SDS:UNKNOWN:"/d/x.hdf":0`), else what lies between the driver and the last colon.
fn subdataset_file(name: &str) -> Option<&str> {
    if let Some((_, rest)) = name.split_once('"') {
        return rest.split_once('"').map(|(p, _)| p);
    }
    let (_, rest) = name.split_once(':')?;
    rest.rsplit_once(':').map(|(p, _)| p)
}

/// Separates a cache key file's key from what the conversion found (JSON).
const FOUND: &str = "\n\u{1}found\n";

/// The conversion's version, in its cache key: raised when what a conversion makes changes (its
/// options, the wrap, the drivers its input may open as), so older results are made again.
const CONVERSION: u32 = 3;

/// Whether two paths name the same file: canonical paths when both resolve, else spellings
/// compared with one separator (and, on Windows, without regard to case).
fn same_file(a: &Path, b: &Path) -> bool {
    if let (Ok(x), Ok(y)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        return x == y;
    }
    let norm = |p: &Path| {
        let s = p.display().to_string().replace('\\', "/");
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    norm(a) == norm(b)
}

fn convert_in(dir: &Path, src: &Path, layer: Option<&str>, crs: Option<&str>, budget: Option<u64>) -> Result<Converted> {
    let workspace = budget.map(|max| (dir, max));
    if let Some(c) = crs
        && !valid_crs(c)
    {
        return Err(DataError::Format(format!("crs '{c}': give an EPSG code (EPSG:32633), a PROJ string or WKT")));
    }
    let full = std::path::absolute(src).unwrap_or_else(|_| src.to_path_buf());
    let layer = layer.map(str::trim).filter(|l| !l.is_empty());
    std::fs::create_dir_all(dir)?;
    external::prune(dir, ours, KEEP);
    let (len, modified) = stamp(&full)?;
    // The cache key, from what was asked, spelled out and checked, so a hash collision can't hand
    // back another raster; the key file also keeps what the conversion found. A hit needs no GDAL.
    let key = format!("raster {CONVERSION}\n{}\n{:?}\n{:?}\n{len}\n{modified}", full.display(), layer, crs);
    let tag = format!("{:016x}", fnv1a(key.as_bytes()));
    let out = dir.join(format!("{tag}.tif"));
    let keyfile = dir.join(format!("{tag}.rkey"));
    let stored = std::fs::read_to_string(&keyfile).unwrap_or_default();
    if let Some((k, found)) = stored.split_once(FOUND)
        && k == key
        && out.metadata().is_ok_and(|m| m.len() > 0)
        && let Ok(found) = serde_json::from_str::<serde_json::Value>(found)
    {
        touch(&out);
        touch(&keyfile);
        let others = found.get("others").and_then(|o| o.as_array()).into_iter().flatten().filter_map(|o| o.as_str().map(str::to_string));
        return Ok(Converted {
            path: out,
            subdataset: found.get("subdataset").and_then(|s| s.as_str()).map(str::to_string),
            others: others.collect(),
            assumed_lonlat: found.get("assumedLonLat").and_then(|a| a.as_bool()).unwrap_or(false),
        });
    }
    let translate = program("gdal_translate").ok_or_else(missing)?;
    let drivers = drivers(&full)?;
    let subs = subdatasets_with_budget(&full, workspace)?;
    let (input, picked) = if subs.is_empty() {
        if let Some(l) = layer {
            return Err(DataError::Format(format!("no subdataset '{l}': the file is one raster")));
        }
        (full.as_os_str().to_owned(), None)
    } else {
        let pick = match layer {
            Some(l) => subs.iter().find(|(n, _)| n == l || short_name(n).eq_ignore_ascii_case(l)).ok_or_else(|| {
                DataError::Format(format!(
                    "no subdataset '{l}' (it has: {})",
                    subs.iter().map(|(n, _)| short_name(n)).collect::<Vec<_>>().join(", ")
                ))
            })?,
            None => subs.first().ok_or_else(|| DataError::Format("no subdatasets".into()))?,
        };
        // GDAL's own name for a subdataset names this very file: one naming another is refused.
        if !subdataset_file(&pick.0).is_some_and(|f| same_file(Path::new(f), &full)) {
            return Err(DataError::Format(format!("subdataset '{}' isn't in this file", pick.0)));
        }
        (std::ffi::OsString::from(&pick.0), Some(pick.0.clone()))
    };
    let others: Vec<String> = subs.iter().filter(|(n, _)| Some(n) != picked.as_ref()).map(|(n, _)| short_name(n).to_string()).collect();
    // What a project keeps: the short name, which still finds the subdataset if the file moves.
    let subdataset = picked.as_deref().map(|n| short_name(n).to_string());
    // A raster that doesn't say its coordinate system: WGS 84 when its coordinates look like
    // longitude/latitude (as NetCDF climate grids often are), else the user must say.
    let mut assumed_lonlat = false;
    let d = describe(&input, drivers, workspace)?;
    let Some(extent) = d.extent else {
        return Err(DataError::Format("the raster isn't georeferenced: GDAL finds no coordinates for its cells".into()));
    };
    let crs = match crs {
        Some(c) => Some(c),
        None if d.has_srs => None,
        None if looks_geographic(extent) => {
            assumed_lonlat = true;
            Some("EPSG:4326")
        }
        None => {
            return Err(DataError::Format("the raster doesn't say its coordinate system: give it (\"crs\", such as EPSG:32633)".into()));
        }
    };
    let geographic = d.geographic || assumed_lonlat || crs.is_some_and(|c| c.eq_ignore_ascii_case("EPSG:4326"));
    let wrap = geographic && wraps(extent, d.cell);
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!("{tag}.{}{n:04}.tmp.tif", std::process::id()));
    // A cloud-optimised GeoTIFF: tiled, compressed, with overviews, which the reader opens from.
    let cog = ["-q", "-of", "COG", "-co", "COMPRESS=DEFLATE", "-co", "BIGTIFF=IF_SAFER", "-co", "OVERVIEWS=AUTO"];
    let mut c = if wrap {
        let warp = program("gdalwarp").ok_or_else(missing)?;
        let mut c = command(warp, drivers);
        c.args(cog).args(["-t_srs", "EPSG:4326", "-te", "-180", "-90", "180", "90", "--config", "CENTER_LONG", "0"]);
        if let Some(crs) = crs {
            c.args(["-s_srs", crs]);
        }
        c
    } else {
        let mut c = command(translate, drivers);
        c.args(cog);
        if let Some(crs) = crs {
            c.args(["-a_srs", crs]);
        }
        c
    };
    c.arg(&input).arg(&tmp);
    let mut tmp_guard = TempFile(Some(tmp.clone()));
    let res = output_with_budget(c, CONVERT_TIMEOUT, "GDAL", workspace)?;
    if !res.status.success() || !tmp.exists() {
        return Err(DataError::Format(format!("GDAL: {}", first_error(&res.stderr))));
    }
    let _ = std::fs::remove_file(&keyfile);
    #[cfg(windows)]
    let _ = std::fs::remove_file(&out);
    std::fs::rename(&tmp, &out)?;
    tmp_guard.0 = None;
    let found = serde_json::json!({"subdataset": subdataset, "others": others, "assumedLonLat": assumed_lonlat});
    std::fs::write(&keyfile, format!("{key}{FOUND}{found}"))?;
    Ok(Converted { path: out, subdataset, others, assumed_lonlat })
}

/// GDAL's ERROR lines (the rest is noise), else its first line.
fn first_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let mut msg: Vec<&str> = text.lines().filter(|l| l.contains("ERROR")).take(3).collect();
    if msg.is_empty() {
        msg = text.lines().filter(|l| !l.trim().is_empty()).take(1).collect();
    }
    if msg.is_empty() { "GDAL failed".into() } else { msg.join(" ") }
}

const CONVERT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);
/// Conversions not used for this long are removed from the cache.
const KEEP: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 3600);

/// Whether a file name is one this module writes: `<16 hex>.tif|.rkey`, or a temporary file.
fn ours(name: &str) -> bool {
    let hex = |s: &str| s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit());
    match name.split_once('.') {
        Some((tag, rest)) => hex(tag) && (rest == "tif" || rest == "rkey" || rest.ends_with(".tmp.tif")),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn names() {
        assert!(is_gdal_raster_name("/d/N45E006.HGT"));
        assert!(is_gdal_raster_name("air.nc"));
        assert!(!is_gdal_raster_name("a.tif") && !is_gdal_raster_name("a.vrt"));
        assert_eq!(short_name("NETCDF:\"/d/air.nc\":air"), "air");
        assert!(looks_geographic([-1.25, -91.25, 358.75, 91.25]));
        assert!(!looks_geographic([500000.0, 4e6, 600000.0, 5e6]));
        // 0–360 grids wrap, their west edge half a cell before 0; regional ones don't.
        assert!(wraps([-1.25, -90.0, 358.75, 90.0], Some(2.5)));
        assert!(wraps([-0.5, -90.0, 359.5, 90.0], Some(1.0)));
        assert!(wraps([0.0, -90.0, 360.0, 90.0], None));
        assert!(!wraps([-1.25, -90.0, 358.75, 90.0], Some(0.25)), "west of half a fine cell");
        assert!(!wraps([170.0, -10.0, 250.0, 10.0], Some(0.5)));
        assert!(!wraps([-180.0, -90.0, 180.0, 90.0], Some(1.0)));
        assert!(ours("0123456789abcdef.tif") && ours("0123456789abcdef.rkey") && ours("0123456789abcdef.12340001.tmp.tif"));
        assert!(!ours("0123456789abcdef.fgb") && !ours("notes.tif"));
        for good in [
            "EPSG:32633",
            "epsg:4326",
            "ESRI:102003",
            "+proj=utm +zone=33 +datum=WGS84",
            "urn:ogc:def:crs:EPSG::2263",
            "PROJCS[\"x\",GEOGCS[\"y\"]]",
        ] {
            assert!(valid_crs(good), "{good}");
        }
        for bad in [
            "/etc/passwd",
            "x.prj",
            "https://example.com/crs",
            "-of",
            "EPSG:",
            "+proj=x;rm",
            "WGS84",
            "+proj=longlat +nadgrids=@x.gsb",
            "+proj=longlat +nadgrids=conus",
            "+proj=utm +zone=33 +geoidgrids=egm96_15.gtx",
            "+proj=longlat +init=epsg",
            "+proj=pipeline +step +proj=hgridshift +grids=ntv2.gsb",
            "+proj=utm +zone=33 +vgrids=egm96_15.gtx",
            "+proj=deformation +xy_grids=a.tif",
        ] {
            assert!(!valid_crs(bad), "{bad}");
        }
        assert_eq!(subdataset_file("NETCDF:\"/d/air.nc\":air"), Some("/d/air.nc"));
        assert_eq!(subdataset_file("HDF4_SDS:UNKNOWN:\"/d/x.hdf\":0"), Some("/d/x.hdf"));
        assert_eq!(subdataset_file("NETCDF:/d/air.nc:air"), Some("/d/air.nc"));
        assert_ne!(subdataset_file("NETCDF:\"/d/air.nc.evil\":air"), Some("/d/air.nc"), "a longer name isn't the file");
        // The same file however it is spelled; another isn't.
        let dir = std::env::temp_dir().join(format!("rustgis-same-file-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let f = dir.join("a.nc");
        std::fs::write(&f, b"x").unwrap();
        assert!(same_file(&dir.join("sub").join("..").join("a.nc"), &f));
        assert!(same_file(&dir.join(".").join("a.nc"), &f));
        assert!(!same_file(&dir.join("b.nc"), &f));
        assert!(!same_file(Path::new("/nowhere/x.nc"), Path::new("/nowhere/y.nc")));
        assert!(same_file(Path::new("/nowhere/x.nc"), Path::new("/nowhere/x.nc")));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Every type read through GDAL has its drivers; a reference file has none.
    #[test]
    fn every_type_has_its_drivers() {
        assert!(EXTENSIONS.iter().all(|e| drivers(Path::new(&format!("a.{e}"))).is_ok()));
        assert!(drivers(Path::new("a.vrt")).is_err() && drivers(Path::new("a.tif")).is_err());
    }

    /// An ER Mapper header opens only its own data beside it: one linking to another file, or
    /// naming data elsewhere, is refused before GDAL runs (GDAL installed or not).
    #[test]
    fn er_mapper_headers_open_only_their_own_data() {
        let dir = tempfile::tempdir().unwrap();
        let header = |name: &str, lines: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, format!("DatasetHeader Begin\n{lines}\tDataType\t= Raster\nDatasetHeader End\n")).unwrap();
            p
        };
        let plain = header("plain.ers", "\tDataSetType\t= ERStorage\n\tDataFile\t= \"plain.dat\"\n");
        assert!(plain_ers(&plain).is_ok());
        let linked = header("linked.ers", "\tDataSetType\t= ERStorage\n\tdatasettype = Translated\n");
        assert!(plain_ers(&linked).is_err_and(|e| e.to_string().contains("ERStorage")));
        // Only raw data passes, whatever follows the type's word.
        for kind in ["Translated ;", "Translated", "Other", "\"Translated\""] {
            let p = header("kind.ers", &format!("\tDataSetType\t= {kind}\n"));
            assert!(plain_ers(&p).is_err(), "{kind}");
        }
        let trailing = header("trailing.ers", "\tDataSetType\t= \"ERStorage\" ; raw\n");
        assert!(plain_ers(&trailing).is_ok());
        // Lines ended by carriage returns alone are lines to GDAL too.
        let cr = header("cr.ers", "\tVersion\t= \"6.0\"\r\tDataSetType\t= Translated\r");
        assert!(plain_ers(&cr).is_err());
        for far in ["../far.dat", "/tmp/far.dat", "C:\\far.dat", ".."] {
            let p = header("far.ers", &format!("\tDataFile\t= \"{far}\"\n"));
            assert!(plain_ers(&p).is_err_and(|e| e.to_string().contains("isn't beside it")), "{far}");
        }
        assert!(subdatasets(&linked).is_err_and(|e| e.to_string().contains("ERStorage")));
        // No type at all is raw data to GDAL (only `Translated` links), so it opens.
        let untyped = header("untyped.ers", "\tDataFile\t= \"untyped.dat\"\n");
        assert!(plain_ers(&untyped).is_ok());
        // A folder named like one isn't read as a header, nor a pipe (which would never finish).
        let folder = dir.path().join("folder.ers");
        std::fs::create_dir(&folder).unwrap();
        assert!(plain_ers(&folder).is_err());
        #[cfg(unix)]
        {
            let pipe = dir.path().join("pipe.ers");
            assert!(Command::new("mkfifo").arg(&pipe).status().is_ok_and(|s| s.success()));
            assert!(plain_ers(&pipe).is_err_and(|e| e.to_string().contains("not a file")));
        }
        // Not an ER Mapper header: nothing to check.
        assert!(plain_ers(Path::new("/nowhere/a.img")).is_ok());
    }

    /// A VRT renamed `.img` isn't opened as a VRT, so the raster it names isn't read.
    #[test]
    fn a_renamed_vrt_isnt_read_when_gdal_is_installed() {
        let Some(translate) = program("gdal_translate") else { return };
        if program("gdalinfo").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let asc = dir.path().join("private.asc");
        std::fs::write(&asc, "ncols 1\nnrows 1\nxllcorner 0\nyllcorner 0\ncellsize 1\n7\n").unwrap();
        let tif = dir.path().join("private.tif");
        let made = Command::new(&translate).args(["-q", "-a_srs", "EPSG:4326"]).arg(&asc).arg(&tif).status();
        assert!(made.is_ok_and(|s| s.success()));
        let img = dir.path().join("audit.img");
        let vrt = format!(
            "<VRTDataset rasterXSize=\"1\" rasterYSize=\"1\"><SRS>EPSG:4326</SRS><GeoTransform>0,1,0,1,0,-1</GeoTransform>\
             <VRTRasterBand dataType=\"Byte\" band=\"1\"><SimpleSource><SourceFilename relativeToVRT=\"0\">{}</SourceFilename>\
             <SourceBand>1</SourceBand></SimpleSource></VRTRasterBand></VRTDataset>",
            tif.display()
        );
        std::fs::write(&img, vrt).unwrap();
        // Without its type's drivers, GDAL reads it.
        assert!(Command::new(program("gdalinfo").unwrap()).arg(&img).status().is_ok_and(|s| s.success()));
        // With them, it doesn't take it for an ERDAS Imagine file.
        let e = convert_in(&dir.path().join("cache"), &img, None, None, None).unwrap_err().to_string();
        assert!(e.contains("not recognized"), "{e}");
    }

    /// Runs only where GDAL is installed: a NetCDF file's variables, an HGT tile.
    #[test]
    fn converts_when_gdal_is_installed() {
        let Some(translate) = program("gdal_translate") else { return };
        if program("gdalinfo").is_none() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rustgis-gdal-raster-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // An ERDAS Imagine file made from an ASCII grid.
        let asc = dir.join("g.asc");
        std::fs::write(
            &asc,
            "ncols 4\nnrows 3\nxllcorner 10\nyllcorner 40\ncellsize 0.5\nNODATA_value -9999\n1 2 3 4\n5 6 7 8\n9 10 11 12\n",
        )
        .unwrap();
        let img = dir.join("g.img");
        let ok = Command::new(&translate)
            .args(["-q", "-of", "HFA", "-a_srs", "EPSG:4326"])
            .arg(&asc)
            .arg(&img)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let cache = dir.join("cache");
        let c = convert_in(&cache, &img, None, None, None).unwrap();
        assert!(c.subdataset.is_none() && c.others.is_empty());
        let r = crate::raster::read_path(&c.path).unwrap();
        assert_eq!((r.width, r.height), (4, 3));
        // The second time comes from the cache.
        let again = convert_in(&cache, &img, None, None, None).unwrap();
        assert_eq!(again.path, c.path);
        assert!(convert_in(&cache, &img, Some("x"), None, None).is_err(), "no subdatasets");
        assert!(convert_in(&cache, &img, None, Some("-of"), None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
