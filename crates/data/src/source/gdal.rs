//! GDAL, optionally: formats the built-in readers don't know (File Geodatabase, DWG, MapInfo TAB,
//! OpenStreetMap XML…) read through GDAL's `ogr2ogr` program when it is installed.
//!
//! Each layer is converted once into a FlatGeobuf with a spatial index (in WGS 84, reprojected by
//! GDAL) in the cache folder, then read like any FlatGeobuf, so large files stream a view at a
//! time too. Nothing links to GDAL: without the program, these formats report how to get it.
//! Layers mixing geometry types (KML often does) become one converted layer per type.

pub use super::external::ReloadWorkspace;
use super::external::{self, TempFile, fnv1a, output, stamp, touch};
use crate::{DataError, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// File types read through GDAL. A File Geodatabase is a folder ending in `.gdb`.
/// (Not `.vrt`, which can point GDAL at any file on the machine, nor `.sqlite`, more often a plain
/// database than a spatial one.)
// KML, KMZ and GPX have built-in readers (`kml`, `gpx`), in the browser too.
// GML, MapInfo .mif, OSM PBF, TopoJSON and DXF have built-in readers too (`gml`, `mapinfo`, `osm`,
// `topojson`, `dxf`). DWG, AutoCAD's own format, is read by GDAL only (its CAD driver, when built
// in).
pub const EXTENSIONS: &[&str] = &["dwg", "gdb", "tab", "osm", "geojsonl", "geojsons", "gmt"];

/// Most layers converted from one file.
const MAX_LAYERS: usize = 64;

/// Whether a file name is read through GDAL.
pub fn is_gdal_name(name: &str) -> bool {
    EXTENSIONS.contains(&extension(name).as_str())
}

fn extension(name: &str) -> String {
    let n = name.trim_end_matches(['/', '\\']);
    Path::new(n).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// The GDAL drivers that may open a file, by its type (`-if`). GDAL picks a driver from a file's
/// contents, so without them a VRT renamed `.tab` would open whatever files or addresses it names.
/// A driver GDAL wasn't built with is only warned about. Other types are refused (a project's
/// saved GDAL source could name any file).
fn drivers(src: &Path) -> Result<&'static [&'static str]> {
    Ok(match extension(&src.to_string_lossy()).as_str() {
        "dwg" => &["CAD", "DWG"],
        "gdb" => &["OpenFileGDB", "FileGDB"],
        "tab" | "mif" => &["MapInfo File"],
        // A large OpenStreetMap extract is read through GDAL too (`.osm.pbf`).
        "osm" | "pbf" => &["OSM"],
        "geojsonl" | "geojsons" => &["GeoJSONSeq"],
        "gmt" => &["OGR_GMT"],
        // Read natively now, but a project saved when GDAL read them converts them again.
        "kml" | "kmz" => &["LIBKML", "KML"],
        "gpx" => &["GPX"],
        "dxf" => &["DXF"],
        // GDAL's GML driver fetches the schemas a file names; the built-in reader doesn't (a
        // project saved when GDAL read GML adds it again instead).
        "gml" => {
            return Err(DataError::Unsupported(format!("{}: GML is read without GDAL now: add the file again", src.display())));
        }
        // A test fixture: CSV can mix surfaces, curves and points in one layer, which none of the
        // types above can.
        #[cfg(test)]
        "csv" => &["CSV"],
        _ => return Err(DataError::Unsupported(format!("{}: not a file type read through GDAL", src.display()))),
    })
}

/// A GDAL program that may open its input with `drivers` only.
pub(crate) fn command(program: PathBuf, drivers: &[&str]) -> Command {
    let mut c = Command::new(program);
    for d in drivers {
        c.arg("-if").arg(d);
    }
    c
}

pub use super::Family;

/// A layer of a GDAL dataset, converted.
#[derive(Clone, Debug, PartialEq)]
pub struct Converted {
    /// The FlatGeobuf in the cache.
    pub path: PathBuf,
    /// The layer's name (with its geometry kind, when the layer mixed them).
    pub name: String,
    /// The layer in the source.
    pub layer: String,
    pub family: Option<Family>,
    /// GDAL couldn't reproject it (the file doesn't say its coordinate system): its coordinates
    /// are the file's own.
    pub unprojected: bool,
}

/// A GDAL program (`ogr2ogr`, `ogrinfo`): from `RUSTGIS_GDAL_BIN` (a folder), else on the `PATH`.
pub fn program(name: &str) -> Option<PathBuf> {
    external::program("RUSTGIS_GDAL_BIN", name)
}

/// Whether GDAL's programs are installed.
pub fn available() -> bool {
    program("ogr2ogr").is_some() && program("ogrinfo").is_some()
}

/// Whether GDAL is installed and new enough (3.8 or later) to list and convert: an older one is
/// refused by every conversion.
pub fn usable() -> bool {
    available() && program("ogrinfo").is_some_and(|p| check_version(&p).is_ok())
}

fn missing() -> DataError {
    DataError::Unsupported(
        "this format needs GDAL: install it (ogr2ogr and ogrinfo on the PATH, or RUSTGIS_GDAL_BIN naming their folder) and add the file again"
            .into(),
    )
}

/// Where converted layers are kept: the platform's cache folder, or `RUSTGIS_GDAL_CACHE`.
pub fn cache_dir() -> Option<PathBuf> {
    external::cache_dir("RUSTGIS_GDAL_CACHE", "gdal")
}

/// A layer: its name, the one kind of geometry it holds (`None`: it mixes them), and its
/// feature count when GDAL knows it.
#[derive(Clone, Debug, PartialEq)]
struct LayerInfo {
    name: String,
    /// SQLite's geometry column name; unnamed OGR geometry fields are exposed as `geometry`.
    geometry_column: String,
    family: Option<Family>,
    count: Option<i64>,
    /// Whether the layer says its coordinate system.
    has_srs: bool,
    has_zm: bool,
}

/// GDAL before 3.8 ignores `ogrinfo -if` (and `ogr2ogr` refuses it), so it could open a file as
/// another format than its type's: it is refused.
fn check_version(ogrinfo: &Path) -> Result<()> {
    let mut c = Command::new(ogrinfo);
    c.arg("--version");
    let out = output(c, LIST_TIMEOUT, "GDAL")?;
    let text = String::from_utf8_lossy(&out.stdout);
    match version(&text) {
        Some(v) if v >= (3, 8) => Ok(()),
        _ => Err(DataError::Unsupported(format!(
            "GDAL 3.8 or later is needed (found: {})",
            text.lines().next().unwrap_or("no version").trim()
        ))),
    }
}

/// GDAL's major and minor version, from `--version` (`GDAL 3.8.5, released 2024/04/02`).
fn version(text: &str) -> Option<(u32, u32)> {
    let v = text.trim().strip_prefix("GDAL ")?.split([',', ' ']).next()?;
    let mut parts = v.split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// The vector layers of a dataset, from `ogrinfo -json`.
fn list(src: &Path) -> Result<Vec<LayerInfo>> {
    let src = std::path::absolute(src).unwrap_or_else(|_| src.to_path_buf());
    // A file or folder on this computer: GDAL would fetch a `/vsicurl/` address.
    std::fs::metadata(&src)?;
    // Its type first, so a type that isn't read through GDAL says so whether GDAL is installed or not.
    let drivers = drivers(&src)?;
    let ogrinfo = program("ogrinfo").ok_or_else(missing)?;
    check_version(&ogrinfo)?;
    let mut c = command(ogrinfo, drivers);
    c.arg("-json").arg("-ro").arg("-so").arg("-nomd").arg(&src);
    let out = output(c, LIST_TIMEOUT, "GDAL")?;
    if !out.status.success() {
        return Err(DataError::Format(format!("GDAL: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| DataError::Format(format!("GDAL: unreadable ogrinfo output ({e}); GDAL 3.8 or later is needed")))?;
    let layers = json.get("layers").and_then(|l| l.as_array());
    Ok(layers
        .into_iter()
        .flatten()
        .filter_map(|l| {
            let name = l.get("name")?.as_str()?.to_string();
            let geom = l.get("geometryFields").and_then(|g| g.as_array()).and_then(|g| g.first())?;
            let geometry = geom.get("type").and_then(|t| t.as_str()).unwrap_or("Geometry");
            let family = Family::of_ogr_type(geometry);
            let has_zm = geometry.ends_with(['Z', 'M']) || geometry.starts_with("3D ");
            let has_srs = geom.get("coordinateSystem").is_some_and(|c| !c.is_null());
            let geometry_column = geom.get("name").and_then(|n| n.as_str()).filter(|n| !n.is_empty()).unwrap_or("geometry").to_string();
            Some(LayerInfo { name, geometry_column, family, count: l.get("featureCount").and_then(|c| c.as_i64()), has_srs, has_zm })
        })
        .collect())
}

/// The vector layers of a dataset.
pub fn layers(src: &Path) -> Result<Vec<String>> {
    Ok(list(src)?.into_iter().take(MAX_LAYERS).map(|l| l.name).collect())
}

/// Converts the dataset's layers (or just `layer`) to FlatGeobufs, reusing earlier conversions of
/// the same file.
/// `crs` gives the coordinate system of a file that doesn't say (`EPSG:2263`; a DWG often doesn't).
/// GDAL overrides accept `AUTH:CODE`: at least two ASCII letters for the authority and an ASCII
/// alphanumeric code (also `.` and `_`), at most 64 bytes total; or the exact well-known geographic
/// names `WGS84`, `WGS72`, `NAD27`, `NAD83` (in any case, as GDAL reads them). Files, addresses, WKT and other aliases are refused.
/// Returns the converted layers, and the layers that couldn't be converted (with why), so a
/// partial result is never mistaken for the whole file.
pub fn convert(src: &Path, layer: Option<&str>, crs: Option<&str>) -> Result<(Vec<Converted>, Vec<String>)> {
    check_crs(crs)?;
    let dir = cache_dir().ok_or_else(|| DataError::Format("no cache folder for GDAL conversions".into()))?;
    convert_in(&dir, src, layer, crs)
}

fn check_crs(crs: Option<&str>) -> Result<()> {
    if let Some(c) = crs {
        let valid = ["WGS84", "WGS72", "NAD27", "NAD83"].iter().any(|name| c.eq_ignore_ascii_case(name))
            || (c.len() <= 64
                && c.split_once(':').is_some_and(|(authority, code)| {
                    authority.len() >= 2
                        && authority.bytes().all(|b| b.is_ascii_alphabetic())
                        && !code.is_empty()
                        && code.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_'))
                }));
        if !valid {
            return Err(DataError::Format(format!("crs '{c}': give a code such as EPSG:2263")));
        }
    }
    Ok(())
}

fn convert_in(dir: &Path, src: &Path, layer: Option<&str>, crs: Option<&str>) -> Result<(Vec<Converted>, Vec<String>)> {
    check_crs(crs)?;
    if !available() {
        return Err(missing());
    }
    let all = list(src)?;
    let chosen: Vec<LayerInfo> = match layer {
        Some(l) => {
            all.iter().find(|x| x.name == l).or_else(|| all.iter().find(|x| x.name.eq_ignore_ascii_case(l))).cloned().into_iter().collect()
        }
        None => all.into_iter().take(MAX_LAYERS).filter(|x| x.count != Some(0)).collect(),
    };
    if chosen.is_empty() {
        return Err(DataError::Format(match layer {
            Some(l) => format!("no layer '{l}'"),
            None => "no layers with features".into(),
        }));
    }
    let mut out = Vec::new();
    let mut failed = Vec::new();
    let mut last = None;
    for l in chosen {
        let before = out.len();
        let mut errors = Vec::new();
        let families: Vec<Option<Family>> = if l.family.is_some() { vec![None] } else { Family::ALL.iter().copied().map(Some).collect() };
        for family in families {
            match convert_one_in(dir, src, &l.name, ConversionRequest { family, kind: l.family, crs, has_srs: l.has_srs, fresh: false }) {
                Ok(Some(converted)) => out.push(Converted {
                    path: converted.path,
                    unprojected: converted.unprojected,
                    name: match family {
                        Some(f) => format!("{} ({})", l.name, f.label()),
                        None => l.name.clone(),
                    },
                    layer: l.name.clone(),
                    family,
                }),
                // A family the layer doesn't have.
                Ok(None) => {}
                // A layer (or a family filter the driver can't apply) that fails doesn't stop the
                // others; the file fails only if nothing converts.
                Err(e) => errors.push(e),
            }
        }
        // A layer none of whose parts converted is reported, with the first reason.
        if out.len() == before
            && let Some(e) = errors.into_iter().next()
        {
            failed.push(format!("{}: {e}", l.name));
            last = Some(e);
        }
    }
    if out.is_empty() {
        return Err(last.unwrap_or_else(|| DataError::Format("GDAL found no features to read".into())));
    }
    Ok((out, failed))
}

/// Converts one layer (or one geometry family of it) into the cache; `None` when it has no
/// features. The cache key covers the file's path, size and modification time.
/// CRS overrides follow the authority-code-only contract of [`convert`].
pub fn convert_one(src: &Path, layer: &str, family: Option<Family>, crs: Option<&str>) -> Result<Option<PathBuf>> {
    check_crs(crs)?;
    let dir = cache_dir().ok_or_else(|| DataError::Format("no cache folder for GDAL conversions".into()))?;
    // The layer's own kind of geometry, and whether it says its coordinate system, as a fresh
    // listing tells. Its name is matched as `convert` matches it, ignoring case (the exact
    // spelling first), and converted under the listed spelling, so the two share a conversion.
    let all = list(src)?;
    let info = all
        .iter()
        .find(|l| l.name == layer)
        .or_else(|| all.iter().find(|l| l.name.eq_ignore_ascii_case(layer)))
        .ok_or_else(|| DataError::Format(format!("no layer '{layer}'")))?;
    let has_srs = info.has_srs;
    let kind = info.family;
    let layer = info.name.as_str();
    Ok(convert_one_in(&dir, src, layer, ConversionRequest { family, kind, crs, has_srs, fresh: false })?.map(|c| c.path))
}

/// A freshly converted candidate and diagnostics from reading its original source.
#[derive(Debug)]
pub struct FreshConverted {
    pub path: PathBuf,
    pub notes: Vec<String>,
    /// A successful read confirmed that this declared layer contains no features.
    pub empty: bool,
    pub zm_dropped: bool,
    /// Private conversion files, retained by the concrete reader after opening.
    pub workspace: std::sync::Arc<ReloadWorkspace>,
}

#[derive(Debug)]
struct Conversion {
    path: PathBuf,
    unprojected: bool,
    notes: Vec<String>,
    empty: bool,
}

/// Converts only the selected layer afresh, without reusing or replacing a retained conversion.
/// A declared empty layer is valid; a missing layer or incompatible declared family is not.
/// Successful GDAL diagnostics are returned too, including features skipped by the driver.
/// CRS overrides follow the authority-code-only contract of [`convert`].
pub fn convert_one_fresh(src: &Path, layer: &str, family: Option<Family>, crs: Option<&str>) -> Result<FreshConverted> {
    check_crs(crs)?;
    let dir = cache_dir().ok_or_else(|| DataError::Format("no cache folder for GDAL conversions".into()))?;
    convert_one_fresh_in(&dir, src, layer, family, crs)
}

fn convert_one_fresh_in(dir: &Path, src: &Path, layer: &str, family: Option<Family>, crs: Option<&str>) -> Result<FreshConverted> {
    check_crs(crs)?;
    let workspace = external::reload_workspace(dir)?;
    let all = list(src)?;
    let info = all
        .iter()
        .find(|l| l.name == layer)
        .or_else(|| all.iter().find(|l| l.name.eq_ignore_ascii_case(layer)))
        .ok_or_else(|| DataError::Format(format!("no layer '{layer}'")))?;
    if let (Some(wanted), Some(actual)) = (family, info.family)
        && wanted != actual
    {
        return Err(DataError::Format(format!("layer '{layer}' declares {}, not {}", actual.label(), wanted.label())));
    }
    let mut converted = convert_one_in(
        workspace.path(),
        src,
        &info.name,
        ConversionRequest { family, kind: info.family, crs, has_srs: info.has_srs, fresh: true },
    )?
    .ok_or_else(|| DataError::Format(format!("layer '{layer}' has no compatible declared geometry")))?;
    if converted.unprojected {
        converted.notes.push(format!("GDAL: '{layer}' has no declared coordinate system; its coordinates were left unprojected"));
    }
    let mut zm_dropped = info.has_zm && !converted.empty;
    if zm_dropped {
        let count = info.count.filter(|n| *n >= 0).map_or_else(|| "an unknown number of".into(), |n| n.to_string());
        converted.notes.push(format!(
            "GDAL: '{layer}' declares Z or M coordinates on {count} source feature(s); conversion keeps only XY coordinates"
        ));
    }
    if !info.has_zm && info.family.is_none() && !converted.empty {
        match count_zm_features(src, info, family) {
            Ok(0) => {}
            Ok(count) => {
                zm_dropped = true;
                converted.notes.push(format!(
                    "GDAL: '{layer}' contains Z or M coordinates on {count} source feature(s); conversion keeps only XY coordinates"
                ));
            }
            Err(_) => converted.notes.push(format!(
                "GDAL: '{layer}': an unknown number of source feature(s) may have had Z or M coordinates dropped; conversion keeps only XY coordinates"
            )),
        }
    }
    Ok(FreshConverted { path: converted.path, notes: converted.notes, empty: converted.empty, zm_dropped, workspace })
}

struct ConversionRequest<'a> {
    family: Option<Family>,
    kind: Option<Family>,
    crs: Option<&'a str>,
    has_srs: bool,
    fresh: bool,
}

/// The conversion and whether it is unprojected (see [`Converted::unprojected`]).
///
/// - `family` picks one kind of geometry out of a layer that mixes them; `kind` is the layer's
///   own, when it holds one. Either converts curves and surfaces (an Esri multipatch is a TIN or
///   a polyhedral surface) to plain lines and polygons, which the FlatGeobuf reader takes.
/// - `has_srs` says whether the layer gives its coordinate system: one that doesn't (and has no
///   `crs` given) is taken as it is, unprojected.
fn convert_one_in(dir: &Path, src: &Path, layer: &str, request: ConversionRequest<'_>) -> Result<Option<Conversion>> {
    let ConversionRequest { family, kind, crs, has_srs, fresh } = request;
    check_crs(crs)?;
    // A name GDAL would take for an option (a file can name its own layers).
    if layer.starts_with('-') {
        return Err(DataError::Format(format!("layer '{layer}': a name starting with '-' isn't read")));
    }
    let ogr2ogr = program("ogr2ogr").ok_or_else(missing)?;
    std::fs::create_dir_all(dir)?;
    if !fresh {
        external::prune(dir, ours, KEEP);
    }
    let full = std::path::absolute(src).unwrap_or_else(|_| src.to_path_buf());
    let (len, modified) = stamp(&full)?;
    // The cache key, spelled out: kept beside the conversion and checked, so a hash collision
    // can't hand back another file's layer.
    let nlt = family.or(kind).map_or("PROMOTE_TO_MULTI", Family::ogr_type);
    // The type is part of the key: conversions made before surfaces were kept are made again.
    let key = format!("{}\n{layer}\n{:?}\n{:?}\n{nlt}\n{len}\n{modified}", full.display(), family.map(Family::label), crs);
    // Fresh replacements have their own private directory, so retained readers never share a
    // file with a later conversion, even when its source's signature hasn't changed.
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tag = format!("{:016x}", fnv1a(key.as_bytes()));
    let out = dir.join(format!("{tag}.fgb"));
    let empty = dir.join(format!("{tag}.empty"));
    let keyfile = dir.join(format!("{tag}.key"));
    // The key file holds the key, then whether the conversion is unprojected: one file, so the two
    // can't disagree.
    const UNPROJECTED: &str = "\nunprojected";
    let stored = std::fs::read_to_string(&keyfile).unwrap_or_default();
    let same = stored == key || stored.strip_suffix(UNPROJECTED) == Some(key.as_str());
    let was_unprojected = stored.ends_with(UNPROJECTED);
    // A hit is touched, so pruning removes what isn't used rather than what is old.
    if !fresh && same && empty.exists() {
        touch(&empty);
        touch(&keyfile);
        return Ok(None);
    }
    if !fresh && same && out.metadata().is_ok_and(|m| m.len() > 0) {
        touch(&out);
        touch(&keyfile);
        return Ok(Some(Conversion { path: out, unprojected: was_unprojected, notes: Vec::new(), empty: false }));
    }
    // Unique to this conversion, even with others of the same layer running in this process.
    let tmp = dir.join(format!("{tag}.{}{n:04}.tmp.fgb", std::process::id()));
    // Reprojected to WGS 84 from its own coordinate system, or from the one given; a layer with
    // neither is taken as it is.
    let unprojected = crs.is_none() && !has_srs;
    let mut c = command(ogr2ogr, drivers(&full)?);
    c.args(["-f", "FlatGeobuf", "-nlt", nlt, "-dim", "XY", "-lco", "SPATIAL_INDEX=YES"]);
    // Fresh replacements must fail rather than install a partial conversion.
    if !fresh {
        c.arg("-skipfailures");
    }
    if let Some(crs) = crs {
        c.args(["-s_srs", crs]);
    }
    if !unprojected {
        c.args(["-t_srs", "EPSG:4326"]);
    }
    if let Some(f) = family {
        c.args(["-where", f.filter()]);
    }
    c.arg(&tmp).arg(&full).arg(layer);
    // The temp file goes whatever happens, unless it becomes the conversion.
    let mut tmp_guard = TempFile(Some(tmp.clone()));
    let res = output(c, CONVERT_TIMEOUT, "GDAL")?;
    if !res.status.success() {
        let text = String::from_utf8_lossy(&res.stderr);
        // The ERROR lines say what went wrong (the rest is the usage text); else the first line.
        let mut msg: Vec<&str> = text.lines().filter(|l| l.contains("ERROR")).take(3).collect();
        if msg.is_empty() {
            msg = text.lines().filter(|l| !l.trim().is_empty()).take(1).collect();
        }
        let msg = if msg.is_empty() { format!("ogr2ogr failed ({})", res.status) } else { msg.join(" ") };
        return Err(DataError::Format(format!("GDAL: {msg}")));
    }
    // Whether it holds anything: a feature read, not the header's count (which FlatGeobuf may leave
    // out, as 0). A filter matching nothing leaves no file at all.
    let any = if fresh {
        // A read failure is not an empty dataset. Never install a corrupt or partially readable
        // conversion as a successful replacement.
        let source = super::open_path(&tmp, None)?;
        !source.read(&super::Request { extent: None, limit: 1 })?.features.is_empty()
    } else {
        tmp.exists()
            && super::open_path(&tmp, None)
                .ok()
                .is_some_and(|s| s.read(&super::Request { extent: None, limit: 1 }).is_ok_and(|b| !b.features.is_empty()))
    };
    // The entry is replaced in an order that never leaves it naming a missing file: the new state
    // first, then the key that vouches for it.
    let _ = std::fs::remove_file(&keyfile);
    if !any && !(fresh && family.or(kind).is_some()) {
        // An empty family is remembered, so the file isn't converted again for it.
        std::fs::write(&empty, b"")?;
        let _ = std::fs::remove_file(&out);
        std::fs::write(&keyfile, &key)?;
        return Ok(None);
    }
    // A rename replaces the old conversion in one step (Windows needs it gone first).
    #[cfg(windows)]
    let _ = std::fs::remove_file(&out);
    std::fs::rename(&tmp, &out)?;
    tmp_guard.0 = None;
    let _ = std::fs::remove_file(&empty);
    std::fs::write(&keyfile, if unprojected { format!("{key}{UNPROJECTED}") } else { key.clone() })?;
    let notes = if fresh {
        let mut counts = std::collections::BTreeMap::<String, u64>::new();
        for line in String::from_utf8_lossy(&res.stderr).lines().map(str::trim).filter(|line| !line.is_empty()) {
            let count = counts.entry(line.to_string()).or_default();
            *count = count.saturating_add(1);
        }
        let mut notes: Vec<String> = counts.into_iter().map(|(line, count)| format!("GDAL: {line} ({count} occurrence(s))")).collect();
        // A layer declaring one geometry type holds no other; only a mixed layer is counted. A
        // count that fails doesn't undo a good conversion: the possible loss is said instead.
        if let Some(family) = family.filter(|_| kind.is_none()) {
            match count_other_families(&full, layer, family) {
                Ok(0) => {}
                Ok(other) => notes.push(format!("GDAL: {other} features of another geometry type were left out")),
                Err(_) => notes.push("GDAL: an unknown number of features of another geometry type may have been left out".into()),
            }
        }
        notes
    } else {
        Vec::new()
    };
    Ok(Some(Conversion { path: out, unprojected, notes, empty: !any }))
}

/// Count only the other supported geometry families, not geometry-less rows. Reuse exactly the
/// filters used for conversion, so curves, surfaces and collections have the same classification.
fn count_other_families(src: &Path, layer: &str, selected: Family) -> Result<u64> {
    let (first, second) = match selected {
        Family::Points => (Family::Lines, Family::Polygons),
        Family::Lines => (Family::Points, Family::Polygons),
        Family::Polygons => (Family::Points, Family::Lines),
    };
    let filter = format!("({}) OR ({})", first.filter(), second.filter());
    let mut c = command(program("ogrinfo").ok_or_else(missing)?, drivers(src)?);
    c.args(["-json", "-ro", "-so", "-nomd", "-where", &filter]).arg(src).arg(layer);
    let out = output(c, CONVERT_TIMEOUT, "GDAL")?;
    if !out.status.success() {
        return Err(DataError::Format(format!("GDAL: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| DataError::Format(format!("GDAL: unreadable geometry count ({e})")))?;
    json.get("layers")
        .and_then(|layers| layers.as_array())
        .and_then(|layers| layers.first())
        .and_then(|layer| layer.get("featureCount"))
        .and_then(|count| count.as_u64())
        .ok_or_else(|| DataError::Format("GDAL: could not count features of other geometry types".into()))
}

/// Generic layers don't declare the dimensions of their features. Count the selected family's
/// actual Z/M geometries that conversion flattens; SQLite's spatial functions aren't present in every
/// GDAL build, so the caller reports possible loss if this diagnostic query fails.
fn count_zm_features(src: &Path, layer: &LayerInfo, family: Option<Family>) -> Result<u64> {
    let src = std::path::absolute(src).unwrap_or_else(|_| src.to_path_buf());
    let geometry = format!("\"{}\"", layer.geometry_column.replace('"', "\"\""));
    let name = format!("\"{}\"", layer.name.replace('"', "\"\""));
    // ST_GeometryType includes a trailing ` Z`, ` M` or ` ZM`, unlike OGR_GEOMETRY.
    // None of the supported base geometry names ends in a space, Z or M.
    let filter = family.map_or_else(String::new, |f| {
        format!(" AND ({})", f.filter().replace("OGR_GEOMETRY", &format!("rtrim(ST_GeometryType({geometry}), ' ZM')")))
    });
    let sql = format!("SELECT COUNT(*) AS zm_count FROM {name} WHERE (ST_Is3D({geometry}) = 1 OR ST_IsMeasured({geometry}) = 1){filter}");
    let mut c = command(program("ogrinfo").ok_or_else(missing)?, drivers(&src)?);
    c.args(["-json", "-ro", "-features", "-nomd", "-dialect", "SQLite", "-sql", &sql]).arg(&src);
    let out = output(c, CONVERT_TIMEOUT, "GDAL")?;
    if !out.status.success() {
        return Err(DataError::Format(format!("GDAL: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| DataError::Format(format!("GDAL: unreadable dimension count ({e})")))?;
    json.get("layers")
        .and_then(|layers| layers.as_array())
        .and_then(|layers| layers.first())
        .and_then(|layer| layer.get("features"))
        .and_then(|features| features.as_array())
        .and_then(|features| features.first())
        .and_then(|feature| feature.get("properties"))
        .and_then(|properties| properties.get("zm_count"))
        .and_then(|count| count.as_u64())
        .ok_or_else(|| DataError::Format("GDAL: could not count features with Z or M coordinates".into()))
}

/// The longest a conversion may run, and the longest a layer listing may.
const CONVERT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Conversions not used for this long are removed from the cache.
const KEEP: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 3600);

/// Whether a file name is one this module writes: `<16 hex>.fgb|.key|.empty`, or a
/// `<16 hex>.<pid>.tmp.fgb` left by a conversion that didn't finish.
fn ours(name: &str) -> bool {
    let Some((tag, rest)) = name.split_once('.') else { return false };
    let tmp = rest.strip_suffix(".tmp.fgb").is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()));
    tag.len() == 16 && tag.bytes().all(|b| b.is_ascii_hexdigit()) && (matches!(rest, "fgb" | "key" | "empty") || tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GeometryType;

    #[test]
    fn names() {
        assert!(
            is_gdal_name("a.DWG")
                && !is_gdal_name("a.dxf")
                && is_gdal_name("/x/roads.gdb/")
                && !is_gdal_name("t.gpx")
                && !is_gdal_name("a.gml")
        );
        assert!(!is_gdal_name("a.geojson") && !is_gdal_name("a.fgb") && !is_gdal_name("noext"));
        // A layer named like an option never reaches GDAL.
        let e = convert_one_in(
            &std::env::temp_dir(),
            Path::new("x.kml"),
            "-overwrite",
            ConversionRequest { family: None, kind: None, crs: None, has_srs: true, fresh: false },
        )
        .unwrap_err();
        assert!(e.to_string().contains("starting with '-'"), "{e}");
        // Every type read through GDAL has its drivers; a reference file (a project's saved source
        // could name one) has none.
        assert!(EXTENSIONS.iter().all(|e| drivers(Path::new(&format!("a.{e}"))).is_ok()) && drivers(Path::new("/x/a.gdb/")).is_ok());
        assert!(drivers(Path::new("a.vrt")).is_err() && drivers(Path::new("noext")).is_err());
        // A project saved when GDAL read GML is told to add it again, GDAL installed or not.
        let dir = tempfile::tempdir().unwrap();
        let gml = dir.path().join("old.gml");
        std::fs::write(&gml, "<x/>").unwrap();
        assert!(layers(&gml).is_err_and(|e| e.to_string().contains("add the file again")));
        // An address is never handed to GDAL in place of a file, installed or not.
        assert!(matches!(layers(Path::new("/vsicurl/http://127.0.0.1:9/a.osm")), Err(DataError::Io(_))));
        // GDAL before 3.8 doesn't hold `ogrinfo` to the drivers given.
        assert_eq!(version("GDAL 3.8.5, released 2024/04/02\n"), Some((3, 8)));
        assert_eq!(version("GDAL 3.13.3 \"Iowa City\", released 2026/08/13"), Some((3, 13)));
        assert!(version("GDAL 3.7.3, released 2023/10/30").is_some_and(|v| v < (3, 8)));
        assert_eq!(version("ogrinfo: command not found"), None);
        // Pruning touches only the cache's own files.
        assert!(ours("0123456789abcdef.fgb") && ours("0123456789abcdef.key") && ours("0123456789abcdef.empty"));
        assert!(ours("0123456789abcdef.4242.tmp.fgb"));
        assert!(!ours("notes.fgb") && !ours("0123456789abcdef.txt") && !ours("photo.jpg") && !ours("0123456789abcdef.x.tmp.fgb"));
        // The cache key's hash is stable.
        assert_eq!(fnv1a(b"rustgis"), fnv1a(b"rustgis"));
        assert_ne!(fnv1a(b"a"), fnv1a(b"b"));
    }

    /// A VRT renamed `.tab` isn't opened as a VRT, so the file it names isn't read.
    #[test]
    fn a_renamed_vrt_isnt_read_when_gdal_is_installed() {
        if !available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("private.csv");
        std::fs::write(&csv, "WKT,name\n\"POINT (1 2)\",secret\n").unwrap();
        let tab = dir.path().join("audit.tab");
        let vrt = format!(
            "<OGRVRTDataSource><OGRVRTLayer name=\"private\"><SrcDataSource>{}</SrcDataSource></OGRVRTLayer></OGRVRTDataSource>",
            csv.display()
        );
        std::fs::write(&tab, vrt).unwrap();
        // Without its type's drivers, GDAL reads it.
        let mut c = Command::new(program("ogrinfo").unwrap());
        let listed = c.args(["-ro", "-so"]).arg(&tab).output().unwrap();
        assert!(String::from_utf8_lossy(&listed.stdout).contains("private"));
        // With them, it doesn't take it for a MapInfo table (or a GDAL that ignores them is refused).
        let e = layers(&tab).unwrap_err().to_string();
        assert!(e.contains("not recognized") || e.contains("3.8 or later"), "{e}");
        let request = ConversionRequest { family: None, kind: None, crs: None, has_srs: true, fresh: false };
        assert!(convert_one_in(&dir.path().join("cache"), &tab, "private", request).is_err());
    }

    #[test]
    fn layer_types_name_their_family() {
        for (ty, family) in [
            ("MultiPolygonZ", Some(Family::Polygons)),
            ("TIN", Some(Family::Polygons)),
            ("PolyhedralSurfaceZ", Some(Family::Polygons)),
            ("MultiSurface", Some(Family::Polygons)),
            ("CurvePolygon", Some(Family::Polygons)),
            ("CompoundCurve", Some(Family::Lines)),
            ("CircularStringZ", Some(Family::Lines)),
            ("MultiLineString", Some(Family::Lines)),
            ("PointZ", Some(Family::Points)),
            ("GeometryZ", None),
            ("Geometry", None),
            ("GeometryCollection", None),
        ] {
            assert_eq!(Family::of_ogr_type(ty), family, "{ty}");
        }
    }

    /// A layer mixing a point, a TIN (how GDAL reads an Esri multipatch), a curved polygon, a
    /// compound curve and a collection of a TIN and a polygon (a multipatch from a Shapefile)
    /// keeps all five: the surfaces, the curves and the collection were left out before.
    #[test]
    fn keeps_surfaces_curves_and_collections_when_gdal_is_installed() {
        if !usable() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rustgis-gdal-surfaces-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let csv = dir.join("mixed.csv");
        std::fs::write(
            &csv,
            "WKT,name\n\
             \"POINT (10 20)\",p\n\
             \"TIN Z (((0 0 1,1 0 1,0 1 2,0 0 1)),((1 0 1,1 1 2,0 1 2,1 0 1)))\",tin\n\
             \"CURVEPOLYGON (CIRCULARSTRING (0 0,1 1,2 0,1 -1,0 0))\",curved\n\
             \"COMPOUNDCURVE ((0 0,1 0),CIRCULARSTRING (1 0,2 1,3 0))\",curve\n\
             \"GEOMETRYCOLLECTION Z (TIN Z (((5 5 1,6 5 1,5 6 2,5 5 1))),POLYGON Z ((7 7 0,8 7 0,8 8 0,7 7 0)))\",patch\n",
        )
        .unwrap();
        let (out, failed) = convert_in(&dir.join("cache"), &csv, None, Some("EPSG:4326")).unwrap();
        assert!(failed.is_empty(), "{failed:?}");
        let kinds: Vec<(String, GeometryType, u64)> = out
            .iter()
            .map(|c| {
                let s = crate::source::open_path(&c.path, None).unwrap();
                (c.name.clone(), s.info().geometry_type, s.info().count.unwrap_or(0))
            })
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("mixed (points)".to_string(), GeometryType::Point, 1),
                ("mixed (lines)".to_string(), GeometryType::Polyline, 1),
                ("mixed (polygons)".to_string(), GeometryType::Polygon, 3),
            ],
            "{kinds:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A KML mixing points and polygons, through GDAL when it is installed (skipped otherwise).
    #[test]
    fn converts_a_mixed_kml_when_gdal_is_installed() {
        if !usable() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rustgis-gdal-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let kml = dir.join("mixed.kml");
        std::fs::write(
            &kml,
            r#"<?xml version="1.0" encoding="UTF-8"?><kml xmlns="http://www.opengis.net/kml/2.2"><Document><name>mixed</name>
<Placemark><name>a</name><Point><coordinates>10,20</coordinates></Point></Placemark>
<Placemark><name>b</name><Point><coordinates>11,21</coordinates></Point></Placemark>
<Placemark><name>c</name><Polygon><outerBoundaryIs><LinearRing><coordinates>0,0 1,0 1,1 0,0</coordinates></LinearRing></outerBoundaryIs></Polygon></Placemark>
</Document></kml>"#,
        )
        .unwrap();
        let cache = dir.join("cache");
        let (out, failed) = convert_in(&cache, &kml, None, None).unwrap();
        assert!(failed.is_empty(), "{failed:?}");
        let kinds: Vec<(String, u64)> = out
            .iter()
            .map(|c| {
                let s = crate::source::open_path(&c.path, None).unwrap();
                (c.name.clone(), s.info().count.unwrap_or(0))
            })
            .collect();
        assert_eq!(kinds, vec![("mixed (points)".to_string(), 2), ("mixed (polygons)".to_string(), 1)], "{kinds:?}");
        // Converted once: the second call reuses the cache.
        let (again, _) = convert_in(&cache, &kml, Some("mixed"), None).unwrap();
        assert_eq!(again.iter().map(|c| &c.path).collect::<Vec<_>>(), out.iter().map(|c| &c.path).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn fresh_conversion_reports_dropped_dimensions_but_not_for_clean_xy_data() {
        if !usable() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("rustgis-gdal-fresh-dimensions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // GeoJSON Lines: a file type GDAL reads (plain GeoJSON is read without it).
        let path = dir.join("points.geojsonl");
        for (coordinates, lossy) in [("[2,48,100]", true), ("[8,48]", false)] {
            let input =
                format!(r#"{{"type":"Feature","properties":{{"rank":1}},"geometry":{{"type":"Point","coordinates":{coordinates}}}}}"#);
            std::fs::write(&path, input).unwrap();
            let fresh = convert_one_fresh_in(&dir.join("cache"), &path, "points", None, None).unwrap();
            assert_eq!(fresh.notes.iter().any(|n| n.contains("Z or M coordinates on 1 source feature")), lossy);
            assert_eq!(fresh.zm_dropped, lossy);
            let source = crate::source::open_path(&fresh.path, None).unwrap();
            let batch = source.read(&crate::source::Request { extent: None, limit: 10 }).unwrap();
            assert_eq!(batch.features[0].geometry.as_ref().unwrap().extent().center().x, if lossy { 2.0 } else { 8.0 });
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fresh_csv_diagnostics_count_repeated_overflows_without_claiming_feature_loss() {
        if !usable() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("points.csv");
        let cache = dir.path().join("cache");
        std::fs::write(path.with_extension("csvt"), "\"WKT\",\"Integer\"\n").unwrap();
        for (value, lossy) in [("2147483648", true), ("7", false)] {
            std::fs::write(&path, format!("WKT,rank\n\"POINT(2 48)\",{value}\n\"POINT(3 48)\",{value}\n\"POINT(4 48)\",{value}\n"))
                .unwrap();
            let fresh = convert_one_fresh_in(&cache, &path, "points", Some(Family::Points), Some("EPSG:4326")).unwrap();
            let overflow: Vec<_> = fresh.notes.iter().filter(|n| n.to_ascii_lowercase().contains("integer overflow")).collect();
            if lossy {
                assert_eq!(overflow.len(), 1, "{:?}", fresh.notes);
                assert!(overflow[0].ends_with("(3 occurrence(s))"), "{overflow:?}");
            } else {
                assert!(fresh.notes.is_empty(), "{:?}", fresh.notes);
            }
            let source = crate::source::open_path(&fresh.path, None).unwrap();
            let batch = source.read(&crate::source::Request { extent: None, limit: 10 }).unwrap();
            assert_eq!(batch.features.len(), 3);
            assert_eq!(batch.skipped, 0, "diagnostic occurrences are not skipped-feature counts");
            assert!(batch.features.iter().all(|f| f.geometry.as_ref().unwrap().kind() == GeometryType::Point));
            let rank = source.info().fields.iter().position(|f| f.name == "rank").unwrap();
            assert!(batch.features.iter().all(|f| f.attrs[rank] == crate::Value::Int(if lossy { i64::from(i32::MAX) } else { 7 })));
            drop(source);
            let workspace = fresh.workspace.path().to_path_buf();
            drop(fresh);
            assert!(!workspace.exists(), "unretained conversions must be removed");
        }
        assert!(convert_one_fresh_in(&cache, &path, "absent", None, Some("EPSG:4326")).is_err());
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0, "failed conversions must not retain workspaces");
    }

    #[test]
    fn fresh_selected_family_reports_other_geometries_but_not_missing_or_matching_geometries() {
        if !usable() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("survey.geojsonl");
        let point = r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[2,48]}}"#;
        let line = r#"{"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[2,48],[3,48]]}}"#;
        let missing = r#"{"type":"Feature","properties":{},"geometry":null}"#;
        for mixed in [true, false] {
            let mut input = format!("{point}\n{missing}\n");
            if mixed {
                input.push_str(&format!("{line}\n{line}\n"));
            }
            std::fs::write(&path, input).unwrap();
            let fresh = convert_one_fresh_in(&dir.path().join("cache"), &path, "survey", Some(Family::Points), None).unwrap();
            let omitted = fresh.notes.iter().filter(|note| note.contains("another geometry type")).collect::<Vec<_>>();
            if mixed {
                assert_eq!(omitted, vec!["GDAL: 2 features of another geometry type were left out"]);
            } else {
                assert!(omitted.is_empty(), "{:?}", fresh.notes);
                assert!(fresh.notes.is_empty(), "single-family input has no conversion losses: {:?}", fresh.notes);
            }
            let source = crate::source::open_path(&fresh.path, None).unwrap();
            let batch = source.read(&crate::source::Request { extent: None, limit: 10 }).unwrap();
            assert_eq!(batch.features.len(), 1);
            assert_eq!(batch.features[0].geometry.as_ref().unwrap().kind(), GeometryType::Point);
        }
    }

    #[test]
    fn fresh_and_shared_conversion_refuse_crs_addresses_before_reading_or_converting() {
        // No GDAL gate: all these inputs must fail before looking for its programs or source data.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.geojsonl");
        let cache = dir.path().join("cache");
        for crs in [
            "local.prj",
            "./x.prj",
            "C:x.prj",
            "c:/x.prj",
            "https://example.org/x",
            "WGS84.prj",
            "wgs84x",
            "-EPSG:4326",
            "EPSG:",
            ":4326",
            "EPSG:4:326",
            "EPSG:4\\326",
        ] {
            let expected = format!("crs '{crs}': give a code");
            let error = convert_one_fresh_in(&cache, &path, "survey", Some(Family::Points), Some(crs)).unwrap_err();
            assert!(error.to_string().contains(&expected), "{error}");
            assert!(!cache.exists(), "invalid CRS must be refused before allocating or running a conversion");
            let request = ConversionRequest { family: Some(Family::Points), kind: None, crs: Some(crs), has_srs: true, fresh: false };
            let error = convert_one_in(&cache, &path, "survey", request).unwrap_err();
            assert!(error.to_string().contains(&expected), "{error}");
            let error = convert_in(&cache, &path, Some("survey"), Some(crs)).unwrap_err();
            assert!(error.to_string().contains(&expected), "{error}");
            let error = convert_one(&path, "survey", Some(Family::Points), Some(crs)).unwrap_err();
            assert!(error.to_string().contains(&expected), "{error}");
            assert!(!cache.exists(), "the shared converters must reject invalid CRS before touching their output");
        }
    }

    #[test]
    fn crs_overrides_require_authority_codes_and_accept_existing_epsg_codes() {
        for crs in
            ["EPSG:2263", "EPSG:4326", "OGC:CRS84", "ESRI:102100", "AUTH:Code_1.2", "WGS84", "WGS72", "NAD27", "NAD83", "wgs84", "Nad83"]
        {
            assert!(check_crs(Some(crs)).is_ok(), "{crs}");
        }
        assert!(check_crs(None).is_ok());
        assert!(check_crs(Some(&format!("AA:{}", "1".repeat(61)))).is_ok());
        assert!(check_crs(Some(&format!("AA:{}", "1".repeat(62)))).is_err());
        for crs in [
            "",
            "C:x.prj",
            "c:/x.prj",
            "WGS84.prj",
            "wgs84x",
            "CRS84",
            "EPSG1:4326",
            "ÉPSG:4326",
            "EPSG:٤٣٢٦",
            "EPSG:4326/other",
            "EPSG:4326 other",
        ] {
            assert!(check_crs(Some(crs)).is_err(), "{crs}");
        }
    }

    #[test]
    fn fresh_generic_layer_reports_only_selected_features_with_dropped_dimensions() {
        if !usable() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("survey.geojsonl");
        let point = r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[2,48]}}"#;
        for (point_coordinates, line_coordinates, lossy) in
            [("[3,48,100]", "[[2,48],[3,48]]", true), ("[3,48]", "[[2,48],[3,48]]", false), ("[3,48]", "[[2,48,100],[3,48,100]]", false)]
        {
            let input = format!(
                "{point}\n\
                 {{\"type\":\"Feature\",\"properties\":{{}},\"geometry\":{{\"type\":\"Point\",\"coordinates\":{point_coordinates}}}}}\n\
                 {{\"type\":\"Feature\",\"properties\":{{}},\"geometry\":{{\"type\":\"LineString\",\"coordinates\":{line_coordinates}}}}}\n"
            );
            std::fs::write(&path, input).unwrap();
            let info = list(&path).unwrap().remove(0);
            assert!(info.family.is_none() && !info.has_zm, "the fixture must declare generic Geometry: {info:?}");
            let fresh = convert_one_fresh_in(&dir.path().join("cache"), &path, "survey", Some(Family::Points), None).unwrap();
            let notes = fresh.notes.iter().filter(|n| n.contains("Z or M coordinates")).collect::<Vec<_>>();
            if lossy {
                assert_eq!(notes.len(), 1, "{:?}", fresh.notes);
                assert!(notes[0].contains("Z or M coordinates on 1 source feature(s)"), "{notes:?}");
            } else {
                assert!(notes.is_empty(), "{:?}", fresh.notes);
            }
            assert_eq!(fresh.zm_dropped, lossy);
            let source = crate::source::open_path(&fresh.path, None).unwrap();
            let batch = source.read(&crate::source::Request { extent: None, limit: 10 }).unwrap();
            assert_eq!(batch.features.len(), 2);
            assert!(batch.features.iter().all(|f| f.geometry.as_ref().unwrap().kind() == GeometryType::Point));
        }
    }

    #[test]
    fn generic_dimension_count_includes_measures_and_quotes_layer_names() {
        if !usable() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("surv'ey.csv");
        for (point, expected) in [("POINT M (2 48 100)", 1), ("POINT ZM (2 48 100 200)", 1), ("POINT (2 48)", 0)] {
            std::fs::write(&path, format!("WKT,name\n\"{point}\",point\n\"LINESTRING Z (2 48 100,3 48 100)\",line\n")).unwrap();
            let info = list(&path).unwrap().remove(0);
            assert_eq!(count_zm_features(&path, &info, Some(Family::Points)).unwrap(), expected);
        }
    }
}
