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
    /// Its coordinate system is one RustGIS can't read, so GDAL converted it to longitude/latitude
    /// (as every conversion did before layers kept their own system).
    pub in_lonlat: bool,
    /// Its features whose Z or M values the conversion dropped, as it keeps x and y only.
    pub zm_dropped: u64,
    /// Its features couldn't be checked for Z or M values: any it had were dropped.
    pub zm_unknown: bool,
    /// Its layer declares Z or M values: one whose count failed most likely lost them.
    pub zm_declared: bool,
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

/// Whether a test that needs GDAL runs: skipped without a [`usable`] GDAL, unless
/// `RUSTGIS_REQUIRE_GDAL` is set, when it fails on the `Err` instead.
pub fn test_runs() -> std::result::Result<bool, String> {
    external::test_runs("RUSTGIS_REQUIRE_GDAL", "GDAL 3.8 or later (ogr2ogr and ogrinfo)", usable())
}

fn missing() -> DataError {
    DataError::Unsupported(missing_text(crate::no_programs_here()))
}

/// Why a format read through GDAL can't be read: GDAL isn't installed, or (`here` names it) this
/// build runs no programs, where installing it wouldn't help.
fn missing_text(here: &str) -> String {
    if here.is_empty() {
        "this format needs GDAL: install it (ogr2ogr and ogrinfo on the PATH, or RUSTGIS_GDAL_BIN naming their folder) and add the file again"
            .into()
    } else {
        format!("this format is read through GDAL, a program {here} can't run: convert the file to a GeoPackage on a computer and add that")
    }
}

/// Where converted layers are kept: the platform's cache folder, or `RUSTGIS_GDAL_CACHE`.
pub fn cache_dir() -> Option<PathBuf> {
    external::cache_dir("RUSTGIS_GDAL_CACHE", "gdal")
}

/// A layer: its name, the one kind of geometry it holds (`None`: it mixes them), and its
/// feature count when GDAL knows it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
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

/// The full source signature guards against hash collisions; the version in the key lets a
/// later reader discard metadata whose meaning has changed. This is disposable cache data.
#[derive(serde::Serialize, serde::Deserialize)]
struct Listing {
    key: String,
    layers: Vec<LayerInfo>,
}

/// Reuses layer metadata under the conversion cache's path, size and modification-time rules,
/// including the source count needed to check cached FlatGeobufs. Missing or invalid metadata
/// is listed again. Explicit Reload bypasses this helper, and a cache write failure is harmless.
fn cached_list(dir: &Path, src: &Path) -> Result<Vec<LayerInfo>> {
    use std::io::{BufReader, Read};

    crate::provenance::path(src);
    let full = std::path::absolute(src).unwrap_or_else(|_| src.to_path_buf());
    drivers(&full)?;
    let signature = stamp(&full)?;
    let key = format!("GDAL listing v1\n{}\n{}\n{}", full.display(), signature.0, signature.1);
    let tag = format!("{:016x}", fnv1a(key.as_bytes()));
    let path = dir.join(format!("{tag}.layers"));
    // Bound reads of disposable metadata, including a damaged file that is unexpectedly large.
    if let Ok(file) = std::fs::File::open(&path)
        && let Ok(stored) = serde_json::from_reader::<_, Listing>(BufReader::new(file.take(4 * 1024 * 1024)))
        && stored.key == key
    {
        touch(&path);
        return Ok(stored.layers);
    }
    let layers = list(&full)?;
    // A listing made while the source changed must not vouch for either generation.
    if stamp(&full)? == signature && std::fs::create_dir_all(dir).is_ok() {
        external::prune(dir, ours, KEEP);
        if let Ok(mut tmp) = tempfile::Builder::new().prefix(&format!("{tag}.")).suffix(".tmp.layers").tempfile_in(dir)
            && serde_json::to_writer(tmp.as_file_mut(), &Listing { key, layers: layers.clone() }).is_ok()
        {
            let _ = tmp.persist(&path);
        }
    }
    Ok(layers)
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
    crate::provenance::path(src);
    let src = std::path::absolute(src).unwrap_or_else(|_| src.to_path_buf());
    // A file or folder on this computer: GDAL would fetch a `/vsicurl/` address.
    std::fs::metadata(&src)?;
    // Its type first, so a type that isn't read through GDAL says so whether GDAL is installed or not.
    let drivers = drivers(&src)?;
    let ogrinfo = program("ogrinfo").ok_or_else(missing)?;
    check_version(&ogrinfo).map_err(|e| DataError::Format(format!("{}: {e}", src.display())))?;
    let mut c = command(ogrinfo, drivers);
    c.args(["-json", "-ro", "-so", "-nomd", "-noextent"]).arg(&src);
    let out = output(c, LIST_TIMEOUT, &format!("GDAL listing {}", src.display()))?;
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

/// The vector layers of a dataset, reusing metadata while its conversion signature is unchanged.
pub fn layers(src: &Path) -> Result<Vec<String>> {
    let all = match cache_dir() {
        Some(dir) => cached_list(&dir, src)?,
        None => list(src)?,
    };
    Ok(all.into_iter().take(MAX_LAYERS).map(|l| l.name).collect())
}

/// Converts the dataset's layers (or just `layer`) to FlatGeobufs, reusing earlier conversions of
/// the same file.
/// `crs` gives the coordinate system of a file that doesn't say (`EPSG:2263`; a DWG often doesn't).
/// GDAL overrides accept `AUTH:CODE`: at least two ASCII letters for the authority and an ASCII
/// alphanumeric code (also `.` and `_`), at most 64 bytes total; or the exact well-known geographic
/// names `WGS84`, `WGS72`, `NAD27`, `NAD83` (in any case, as GDAL reads them). Files, addresses, WKT and other aliases are refused.
/// Returns the converted layers and notices about layers or features left out, so a partial
/// result is never mistaken for the whole file. Feature losses compare the known source count
/// with the converted headers, including when reusing cached conversions.
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

/// Converts selected layers, reporting failed layers and known feature-count shortfalls across
/// all their geometry families. Cache hits are checked too, without scanning converted features.
fn convert_in(dir: &Path, src: &Path, layer: Option<&str>, crs: Option<&str>) -> Result<(Vec<Converted>, Vec<String>)> {
    crate::provenance::path(src);
    check_crs(crs)?;
    if !available() {
        return Err(missing());
    }
    let all = cached_list(dir, src)?;
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
                    in_lonlat: converted.in_lonlat,
                    name: match family {
                        Some(f) => format!("{} ({})", l.name, f.label()),
                        None => l.name.clone(),
                    },
                    layer: l.name.clone(),
                    family,
                    zm_dropped: 0,
                    zm_unknown: false,
                    zm_declared: l.has_zm,
                }),
                // A family the layer doesn't have.
                Ok(None) => {}
                // A layer (or a family filter the driver can't apply) that fails doesn't stop the
                // others; the file fails only if nothing converts.
                Err(e) => errors.push(e),
            }
        }
        // The features of each part that had Z or M values, which the conversion drops (`-dim
        // XY`), as a fresh conversion counts them: counted once, with the conversion, and kept
        // beside it, so a cache hit doesn't scan the source again. A count that fails is said
        // without a code (the layer may hold only x and y) and isn't kept: its cause may pass, and
        // a query GDAL refuses fails before scanning anything.
        if l.has_zm || l.family.is_none() {
            for part in out.iter_mut().skip(before) {
                // A count read is touched with its conversion, so pruning keeps it as long.
                let count = kept_zm_count(&part.path).inspect(|_| touch(&part.path.with_extension("zm"))).or_else(|| {
                    let count = count_zm_features(src, &l, part.family).ok().flatten()?;
                    // Kept beside the conversion, so a cache hit reads it instead of scanning the
                    // source again; a write that fails only means it is counted again next time.
                    let _ = std::fs::write(part.path.with_extension("zm"), format!("{count}\n"));
                    Some(count)
                });
                match count {
                    Some(n) => part.zm_dropped = n,
                    None => part.zm_unknown = true,
                }
            }
        }
        // Count all converted families together: splitting a mixed layer isn't a loss. Read
        // only the headers, so checking a cache hit doesn't scan its features or date columns.
        // A layer with nothing converted and no error lost every feature (all empty polygons,
        // say); one that failed is said below, with why.
        if (out.len() > before || errors.is_empty())
            && let Some(expected) = l.count.and_then(|n| u64::try_from(n).ok()).filter(|n| *n > 0)
        {
            // A part whose header can't be read leaves the count unchecked, and says so.
            let count: std::result::Result<u64, DataError> = out
                .iter()
                .skip(before)
                .try_fold(0u64, |total, converted| Ok(total.saturating_add(super::fgb::header_feature_count(&converted.path)?)));
            match count {
                Ok(actual) if actual < expected => {
                    failed.push(format!("{}: GDAL conversion left out {} of {expected} source features", l.name, expected - actual));
                }
                Ok(_) => {}
                Err(e) => failed.push(format!("{}: couldn't check how many features GDAL's conversion kept ({e})", l.name)),
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
        // Nothing converted and no error: what was left out says why (all of it empty polygons).
        return Err(last.unwrap_or_else(|| {
            DataError::Format(if failed.is_empty() { "GDAL found no features to read".into() } else { failed.join("; ") })
        }));
    }
    Ok((out, failed))
}

/// Converts one layer (or one geometry family of it) into the cache; `None` when it has no
/// features. The cache key covers the file's path, size and modification time.
/// CRS overrides follow the authority-code-only contract of [`convert`].
pub fn convert_one(src: &Path, layer: &str, family: Option<Family>, crs: Option<&str>) -> Result<Option<PathBuf>> {
    check_crs(crs)?;
    let dir = cache_dir().ok_or_else(|| DataError::Format("no cache folder for GDAL conversions".into()))?;
    // The layer's geometry kind and coordinate-system declaration, retained while the source
    // signature is unchanged. Match its name as `convert` does (exact spelling first, then
    // ignoring case), and convert under the listed spelling, so the two share a conversion.
    let all = cached_list(&dir, src)?;
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
    pub notes: crate::loss::Notes,
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
    in_lonlat: bool,
    notes: crate::loss::Notes,
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
        converted.notes.push(crate::loss::Note::loss(
            crate::loss::LossCode::AssumedCrs,
            format!("GDAL: '{layer}' has no declared coordinate system; its coordinates were left unprojected"),
        ));
    }
    if converted.in_lonlat {
        converted.notes.push(in_lonlat_note(layer));
    }
    // A repair/reload reports the same conversion loss as Add. For a mixed layer, compare
    // only the selected family; the other families have their own counted notice.
    let expected = match family {
        Some(family) => count_geometry_filter(src, &info.name, family.filter()).map(Some),
        None => Ok(info.count.and_then(|n| u64::try_from(n).ok())),
    };
    match expected.and_then(|expected| super::fgb::header_feature_count(&converted.path).map(|actual| (expected, actual))) {
        Ok((expected, actual)) => {
            if let Some(note) = conversion_count_note(&info.name, expected, actual, !converted.empty) {
                converted.notes.push(note);
            }
        }
        Err(error) => converted.notes.push(format!("{}: couldn't check how many features GDAL's conversion kept ({error})", info.name)),
    }
    let mut zm_dropped = false;
    let mut zm_note = None;
    // A layer declaring Z or M may still hold features with only XY: those that carry them are
    // counted, as for a layer of several types.
    if (info.has_zm || info.family.is_none()) && !converted.empty {
        match count_zm_features(src, info, family).ok().flatten() {
            Some(0) => {}
            Some(count) => {
                zm_dropped = true;
                zm_note = Some(crate::loss::Note::loss(
                    crate::loss::LossCode::DroppedZm,
                    format!(
                        "GDAL: '{layer}' contains Z or M coordinates on {count} source feature(s); conversion keeps only XY coordinates"
                    ),
                ));
            }
            None => {
                // A layer that declares them may still hold only x and y: said without a code, as
                // the count isn't known. Still marked as having lost them when it declares them, so
                // its exports don't pass over what was most likely dropped.
                zm_dropped = info.has_zm;
                zm_note = Some(crate::loss::Note::plain(format!(
                    "GDAL: '{layer}': an unknown number of source feature(s) may have had Z or M coordinates dropped; conversion keeps only XY coordinates"
                )));
            }
        }
    }
    let mut notes: crate::loss::Notes = converted.notes;
    notes.extend(zm_note);
    Ok(FreshConverted { path: converted.path, notes, empty: converted.empty, zm_dropped, workspace })
}

/// A counted loss, or an honest diagnostic when a nonempty replacement's source count is unknown.
fn conversion_count_note(layer: &str, expected: Option<u64>, actual: u64, nonempty: bool) -> Option<crate::loss::Note> {
    match expected {
        Some(expected) if actual < expected => Some(crate::loss::Note::loss(
            crate::loss::LossCode::DroppedFeature,
            format!("{layer}: GDAL conversion left out {} of {expected} source features", expected - actual),
        )),
        // Only a possible loss: said without a code.
        None if nonempty => Some(crate::loss::Note::plain(format!(
            "{layer}: couldn't check how many source features GDAL's conversion kept because the source count is unknown; features may have been left out"
        ))),
        _ => None,
    }
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
    // Kept in its own system since #2032: a conversion made before, in longitude/latitude, isn't
    // reused as one in its own.
    let key = format!("{}\n{layer}\n{:?}\n{:?}\n{nlt}\n{len}\n{modified}\nown system", full.display(), family.map(Family::label), crs);
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
    const IN_LONLAT: &str = "\nin longitude/latitude";
    let stored = std::fs::read_to_string(&keyfile).unwrap_or_default();
    let same = stored == key || [UNPROJECTED, IN_LONLAT].iter().any(|flag| stored.strip_suffix(flag) == Some(key.as_str()));
    let was_unprojected = stored.ends_with(UNPROJECTED);
    let was_in_lonlat = stored.ends_with(IN_LONLAT);
    // A hit is touched, so pruning removes what isn't used rather than what is old.
    if !fresh && same && empty.exists() {
        touch(&empty);
        touch(&keyfile);
        return Ok(None);
    }
    if !fresh && same && out.metadata().is_ok_and(|m| m.len() > 0) {
        touch(&out);
        touch(&keyfile);
        return Ok(Some(Conversion {
            path: out,
            unprojected: was_unprojected,
            in_lonlat: was_in_lonlat,
            notes: crate::loss::Notes::new(),
            empty: false,
        }));
    }
    // A count of Z/M features kept for an earlier conversion with this tag isn't this one's.
    if !fresh {
        let _ = std::fs::remove_file(dir.join(format!("{tag}.zm")));
    }
    // Unique to this conversion, even with others of the same layer running in this process.
    let tmp = dir.join(format!("{tag}.{}{n:04}.tmp.fgb", std::process::id()));
    // Kept in its own coordinate system, or the one given, which the FlatGeobuf says; one RustGIS
    // can't read is converted to WGS 84 instead, as before. A layer with neither is taken as it
    // is.
    let unprojected = crs.is_none() && !has_srs;
    let drivers = drivers(&full)?;
    let run = |to_lonlat: bool| -> std::io::Result<std::process::Output> {
        let mut c = command(ogr2ogr.clone(), drivers);
        c.args(["-f", "FlatGeobuf", "-nlt", nlt, "-dim", "XY", "-lco", "SPATIAL_INDEX=YES"]);
        // Fresh replacements must fail rather than install a partial conversion.
        if !fresh {
            c.arg("-skipfailures");
        }
        // The system given is assigned when kept, and converted from when not.
        match (crs, to_lonlat) {
            (Some(crs), false) => {
                c.args(["-a_srs", crs]);
            }
            (Some(crs), true) => {
                c.args(["-s_srs", crs, "-t_srs", "EPSG:4326"]);
            }
            (None, true) => {
                c.args(["-t_srs", "EPSG:4326"]);
            }
            (None, false) => {}
        }
        if let Some(f) = family {
            c.args(["-where", f.filter()]);
        }
        c.arg(&tmp).arg(&full).arg(layer);
        output(c, CONVERT_TIMEOUT, &format!("GDAL conversion of {}, layer '{layer}'", full.display()))
    };
    // The temp file goes whatever happens, unless it becomes the conversion.
    let mut tmp_guard = TempFile(Some(tmp.clone()));
    let mut res = run(false)?;
    let mut in_lonlat = false;
    if res.status.success() && !unprojected && tmp.exists() {
        let placement = placement(&tmp);
        // Another body's system RustGIS refuses can't go to WGS 84 either (GDAL won't convert
        // between bodies): refused, saying why, rather than left out as empty.
        if let Placement::Refused(why) = &placement {
            return Err(DataError::Format(format!("GDAL: '{layer}': {why}")));
        }
        if placement != Placement::Own {
            crate::progress::check()?;
            // GDAL can't write over a FlatGeobuf: the first conversion goes first.
            std::fs::remove_file(&tmp)?;
            res = run(true)?;
            in_lonlat = placement == Placement::Unreadable;
        }
    }
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
    let any = crate::provenance::without(|| {
        if fresh {
            // A read failure is not an empty dataset. Never install a corrupt or partially readable
            // conversion as a successful replacement.
            let source = super::open_path(&tmp, None)?;
            Ok::<_, DataError>(!source.read(&super::Request { extent: None, limit: 1, own: None })?.features.is_empty())
        } else {
            Ok(tmp.exists()
                && super::open_path(&tmp, None)
                    .ok()
                    .is_some_and(|s| s.read(&super::Request { extent: None, limit: 1, own: None }).is_ok_and(|b| !b.features.is_empty())))
        }
    })?;
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
    let flag = if unprojected {
        UNPROJECTED
    } else if in_lonlat {
        IN_LONLAT
    } else {
        ""
    };
    std::fs::write(&keyfile, format!("{key}{flag}"))?;
    let notes = if fresh {
        let mut counts = std::collections::BTreeMap::<String, u64>::new();
        for line in String::from_utf8_lossy(&res.stderr).lines().map(str::trim).filter(|line| !line.is_empty()) {
            let count = counts.entry(line.to_string()).or_default();
            *count = count.saturating_add(1);
        }
        let mut notes: crate::loss::Notes =
            counts.into_iter().map(|(line, count)| format!("GDAL: {line} ({count} occurrence(s))")).collect();
        // A layer declaring one geometry type holds no other; only a mixed layer is counted. A
        // count that fails doesn't undo a good conversion: the possible loss is said instead.
        if let Some(family) = family.filter(|_| kind.is_none()) {
            match count_other_families(&full, layer, family) {
                Ok(0) => {}
                Ok(other) => notes.push(crate::loss::Note::loss(
                    crate::loss::LossCode::DroppedFeature,
                    format!("GDAL: {other} features of another geometry type were left out"),
                )),
                Err(_) => notes.push("GDAL: an unknown number of features of another geometry type may have been left out"),
            }
        }
        notes
    } else {
        crate::loss::Notes::new()
    };
    Ok(Some(Conversion { path: out, unprojected, in_lonlat, notes, empty: !any }))
}

/// How RustGIS reads a FlatGeobuf GDAL wrote in the source's own system.
#[derive(Clone, Debug, PartialEq)]
enum Placement {
    /// In that system, which it reads and converts itself (WGS 84 longitude/latitude among them).
    Own,
    /// A geographic system that places coordinates as WGS 84 longitude/latitude does under
    /// another name (WGS 84 in 3D, whose heights the conversion drops): written in EPSG:4326, as
    /// before.
    LonLat,
    /// A system it can't read, which it would take for longitude/latitude: converted to it.
    Unreadable,
    /// Another body's system it refuses, and why: not converted at all.
    Refused(String),
}

/// How RustGIS reads `fgb` ([`Placement`]), from its header alone. One it reads as
/// longitude/latitude must say WGS 84 (by code or definition), and is kept as GDAL wrote it; one
/// that doesn't would have its coordinates taken for degrees. One whose header can't be read is
/// converted as before.
fn placement(fgb: &Path) -> Placement {
    use crate::raster::Crs;
    crate::provenance::without(|| {
        // A header that can't be read says nothing of the system: converted as before, unsaid. A
        // system it says that RustGIS refuses can't be read, and is said.
        let crs = match super::fgb::header_coord_crs(fgb) {
            Err(_) => return Placement::LonLat,
            Ok(Err(e)) => return Placement::Refused(e.to_string()),
            Ok(Ok(crs)) => crs,
        };
        if crs == Crs::LonLat {
            return if super::fgb::header_says_wgs84(fgb) { Placement::Own } else { Placement::Unreadable };
        }
        // Longitude/latitude by another name: the same places, to a nanodegree.
        let same = crs.is_geographic()
            && crs.to(Crs::LonLat).is_ok_and(|t| {
                [(12.0, 48.0), (-120.5, -33.25), (179.0, 80.0), (0.0, 0.0)]
                    .into_iter()
                    .all(|(x, y)| t.point(rustgis_geom::Coord::new(x, y)).is_some_and(|c| (c.x - x).abs() < 1e-9 && (c.y - y).abs() < 1e-9))
            });
        if same { Placement::LonLat } else { Placement::Own }
    })
}

/// The notice for a layer converted to longitude/latitude because its system can't be read.
pub fn in_lonlat_note(layer: &str) -> String {
    in_lonlat_notes(&[layer])
}

/// One notice for the layers of a file converted to longitude/latitude because their systems
/// can't be read: their count, and their names (the first ten).
pub fn in_lonlat_notes(layers: &[&str]) -> String {
    match layers {
        [one] => format!("GDAL: '{one}': RustGIS can't read its coordinate system, so GDAL converted it to longitude/latitude (WGS 84)"),
        _ => {
            let mut named: Vec<String> = layers.iter().take(10).map(|l| format!("'{l}'")).collect();
            if layers.len() > 10 {
                named.push(format!("{} more", layers.len() - 10));
            }
            format!(
                "GDAL: {} layers ({}): RustGIS can't read their coordinate systems, so GDAL converted them to longitude/latitude (WGS 84)",
                layers.len(),
                named.join(", ")
            )
        }
    }
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
    count_geometry_filter(src, layer, &filter)
}

/// The count under the exact family filter used by conversion, including empty geometries.
fn count_geometry_filter(src: &Path, layer: &str, filter: &str) -> Result<u64> {
    let mut c = command(program("ogrinfo").ok_or_else(missing)?, drivers(src)?);
    c.args(["-json", "-ro", "-so", "-nomd", "-where", filter]).arg(src).arg(layer);
    let out = output(c, CONVERT_TIMEOUT, &format!("GDAL geometry count for {}, layer '{layer}'", src.display()))?;
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

/// The count of Z/M features kept beside the conversion at `path` ([`convert_in`]), when there is
/// one: written whole, with its line's end, or not taken. Read no further than a count's length,
/// so a damaged file costs nothing.
fn kept_zm_count(path: &Path) -> Option<u64> {
    use std::io::Read;
    let mut text = String::new();
    // The longest a count is (`u64::MAX` and its line's end), and a byte more to tell a longer file.
    const LONGEST: u64 = 21;
    std::fs::File::open(path.with_extension("zm")).ok()?.take(LONGEST + 1).read_to_string(&mut text).ok()?;
    if text.len() as u64 > LONGEST {
        return None;
    }
    text.strip_suffix('\n')?.parse().ok()
}

/// Generic layers don't declare the dimensions of their features. Count the selected family's
/// actual Z/M geometries that conversion flattens; SQLite's spatial functions aren't present in every
/// GDAL build, so the caller reports possible loss if this diagnostic query fails. One `ogrinfo`
/// scan of the source, as long as the conversion's, with no Cancel check inside it. `Ok(None)` when
/// GDAL refuses the query or its answer can't be read, as it will every time; an error when the
/// program couldn't run it (missing, timed out), which may not happen again.
fn count_zm_features(src: &Path, layer: &LayerInfo, family: Option<Family>) -> Result<Option<u64>> {
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
    let out = output(c, CONVERT_TIMEOUT, &format!("GDAL dimension count for {}, layer '{}'", src.display(), layer.name))?;
    if !out.status.success() {
        return Ok(None);
    }
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return Ok(None) };
    Ok(json
        .get("layers")
        .and_then(|layers| layers.as_array())
        .and_then(|layers| layers.first())
        .and_then(|layer| layer.get("features"))
        .and_then(|features| features.as_array())
        .and_then(|features| features.first())
        .and_then(|feature| feature.get("properties"))
        .and_then(|properties| properties.get("zm_count"))
        .and_then(|count| count.as_u64()))
}

/// The longest a conversion may run, and the longest a layer listing may.
const CONVERT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);
const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Conversions not used for this long are removed from the cache.
const KEEP: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 3600);

/// Whether a file name is a conversion, its key or layer metadata, or a temporary file left
/// by an interrupted conversion or metadata write.
fn ours(name: &str) -> bool {
    let Some((tag, rest)) = name.split_once('.') else { return false };
    let tmp = rest.strip_suffix(".tmp.fgb").is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()));
    let listing_tmp =
        rest.strip_suffix(".tmp.layers").is_some_and(|nonce| !nonce.is_empty() && nonce.bytes().all(|b| b.is_ascii_alphanumeric()));
    tag.len() == 16
        && tag.bytes().all(|b| b.is_ascii_hexdigit())
        && (matches!(rest, "fgb" | "key" | "empty" | "layers" | "zm") || tmp || listing_tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GeometryType;

    /// A GDAL program is told the drivers it may open its input with, each as `-if`: without them a
    /// VRT renamed `.tab` would open whatever files it names. Needs no GDAL, so CI runs it.
    #[test]
    fn a_gdal_program_gets_its_drivers_as_if() {
        let c = command(PathBuf::from("ogrinfo"), &["MapInfo File", "CSV"]);
        let args: Vec<&std::ffi::OsStr> = c.get_args().collect();
        assert_eq!(args, ["-if", "MapInfo File", "-if", "CSV"]);
    }

    #[test]
    fn where_programs_cant_run_the_message_says_so_instead_of_asking_for_an_install() {
        assert!(missing_text("").contains("install it"));
        let phone = missing_text("a phone");
        assert!(phone.contains("a phone can't run") && !phone.contains("install"), "{phone}");
        // Where programs run, the build reports none missing for that reason.
        assert_eq!(crate::RUNS_PROGRAMS, crate::no_programs_here().is_empty());
    }

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
        assert!(ours("0123456789abcdef.4242.tmp.fgb") && ours("0123456789abcdef.zm"));
    }

    /// A kept Z/M count is taken only whole: a count with its line's end, or `unknown`.
    #[test]
    fn a_kept_zm_count_is_taken_only_whole() {
        let dir = tempfile::tempdir().unwrap();
        let fgb = dir.path().join("0123456789abcdef.fgb");
        let kept = |text: &str| {
            std::fs::write(fgb.with_extension("zm"), text).unwrap();
            kept_zm_count(&fgb)
        };
        assert_eq!(kept("7\n"), Some(7));
        assert_eq!(kept(&format!("{}\n", u64::MAX)), Some(u64::MAX));
        assert_eq!(kept("unknown\n"), None);
        assert_eq!(kept("7"), None, "cut short");
        assert_eq!(kept(&format!("{}\n", "0".repeat(30))), None, "longer than any count");
        assert_eq!(kept("18446744073709551616\n"), None, "past u64");
        assert!(!ours("notes.fgb") && !ours("0123456789abcdef.txt") && !ours("photo.jpg") && !ours("0123456789abcdef.x.tmp.fgb"));
        // The cache key's hash is stable: a cached file is found again by its name, so a changed
        // hash orphans every cache already on disk. FNV-1a (64-bit) of "rustgis".
        assert_eq!(fnv1a(b"rustgis"), 0xde69_1e62_2dd6_7b88);
        assert_ne!(fnv1a(b"a"), fnv1a(b"b"));
    }

    /// A layer in a system RustGIS reads is converted in it, its coordinates as the file holds
    /// them; one in a system RustGIS can't read is converted to longitude/latitude, as every
    /// layer was before, and said. A conversion made before is not reused for one in its own.
    #[test]
    fn layers_are_converted_in_their_own_system_when_gdal_is_installed() {
        if !test_runs().unwrap() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let made = |name: &str, (driver, ext): (&str, &str), srs: &str| {
            let csv = dir.path().join(format!("{name}.csv"));
            std::fs::write(&csv, "WKT,name\n\"POINT (2600000 1200000)\",Bern\n").unwrap();
            let file = dir.path().join(format!("{name}.{ext}"));
            let out = Command::new(program("ogr2ogr").unwrap()).args(["-f", driver, "-a_srs", srs]).arg(&file).arg(&csv).output().unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            file
        };
        let first = |path: &Path| {
            let source = super::super::open_path(path, None).unwrap();
            let batch = source.read(&super::super::Request { extent: None, limit: 1, own: None }).unwrap();
            let Some(crate::Geometry::Point(c)) = batch.features.into_iter().next().unwrap().geometry else { panic!() };
            (source.coord_crs(), c)
        };
        let cache = dir.path().join("cache");
        let lv95 = made("lv95", ("MapInfo File", "tab"), "EPSG:2056");
        // Where GDAL itself puts the point in longitude/latitude, from the file's definition (which
        // in MapInfo leaves out LV95's datum shift).
        let gdal_lonlat = {
            let json = dir.path().join("lv95.geojson");
            let out = Command::new(program("ogr2ogr").unwrap())
                .args(["-f", "GeoJSON", "-t_srs", "EPSG:4326"])
                .arg(&json)
                .arg(&lv95)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
            let c = &v["features"][0]["geometry"]["coordinates"];
            (c[0].as_f64().unwrap(), c[1].as_f64().unwrap())
        };
        let (converted, failed) = convert_in(&cache, &lv95, None, None).unwrap();
        assert!(failed.is_empty(), "{failed:?}");
        assert!(!converted[0].in_lonlat && !converted[0].unprojected);
        // MapInfo keeps the definition, not the code: held in that system, where LV95 is.
        let (crs, c) = first(&converted[0].path);
        assert_ne!(crs, crate::raster::Crs::LonLat);
        assert!((c.x - 2_600_000.0).abs() < 0.01 && (c.y - 1_200_000.0).abs() < 0.01, "{c:?}");
        let Some(crate::Geometry::Point(ll)) = crs.to(crate::raster::Crs::LonLat).unwrap().whole_geometry(&crate::Geometry::Point(c))
        else {
            panic!()
        };
        assert!((ll.x - gdal_lonlat.0).abs() < 1e-6 && (ll.y - gdal_lonlat.1).abs() < 1e-6, "{ll:?} vs {gdal_lonlat:?}");
        // Taken from the cache the same way.
        let (again, _) = convert_in(&cache, &lv95, None, None).unwrap();
        assert!(!again[0].in_lonlat);
        assert_eq!(first(&again[0].path).0, crs);
        // A system RustGIS can't read (Nell's projection, which a GMT file keeps): converted to
        // longitude/latitude, as before, and said; the same from the cache.
        let nell = made("nell", ("GMT", "gmt"), "+proj=nell +datum=WGS84 +units=m");
        for _ in 0..2 {
            let (converted, failed) = convert_in(&cache, &nell, None, None).unwrap();
            assert!(failed.is_empty() && converted[0].in_lonlat, "{failed:?}");
            let (crs, c) = first(&converted[0].path);
            assert_eq!(crs, crate::raster::Crs::LonLat);
            assert!(c.x.abs() <= 180.0 && c.y.abs() <= 90.0, "{c:?}");
        }
        assert!(in_lonlat_note("nell").contains("converted it to longitude/latitude"));
        // A system RustGIS refuses (another body's, in a projection it doesn't convert) is refused,
        // saying why, not converted; a header that can't be read says nothing, so is converted.
        let mars = dir.path().join("mars.fgb");
        let out = Command::new(program("ogr2ogr").unwrap())
            .args(["-f", "FlatGeobuf", "-a_srs", "+proj=nell +R=3396190 +units=m +no_defs +type=crs"])
            .arg(&mars)
            .arg(dir.path().join("lv95.csv"))
            .output()
            .unwrap();
        if out.status.success() {
            assert!(matches!(placement(&mars), Placement::Refused(why) if why.contains("another body")));
            let gmt = made("mars", ("GMT", "gmt"), "+proj=nell +R=3396190 +units=m +no_defs +type=crs");
            let e = convert_in(&cache, &gmt, None, None).unwrap_err().to_string();
            assert!(e.contains("another body") && e.contains("nell"), "{e}");
        }
        let broken = dir.path().join("broken.fgb");
        std::fs::write(&broken, b"not a flatgeobuf").unwrap();
        assert_eq!(placement(&broken), Placement::LonLat);
        // Several layers of a file: one notice, with their count and names.
        let many: Vec<String> = (1..=12).map(|i| format!("l{i}")).collect();
        let names: Vec<&str> = many.iter().map(String::as_str).collect();
        let note = in_lonlat_notes(&names);
        assert!(
            note.starts_with("GDAL: 12 layers ('l1', 'l2',") && note.contains("'l10', 2 more)") && note.contains("converted them"),
            "{note}"
        );
    }

    /// A VRT renamed `.tab` isn't opened as a VRT, so the file it names isn't read.
    #[test]
    fn a_renamed_vrt_isnt_read_when_gdal_is_installed() {
        if !test_runs().unwrap() {
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
        if !test_runs().unwrap() {
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
    fn converts_a_mixed_kml_with_source_provenance_when_gdal_is_installed() {
        use crate::source::FeatureSource;
        if !test_runs().unwrap() {
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
        let captured = crate::provenance::Capture::start();
        let (out, failed) = convert_in(&cache, &kml, None, None).unwrap();
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(captured.inputs().iter().map(|i| &i.path).collect::<Vec<_>>(), vec![&kml.display().to_string()]);
        drop(captured);
        let kinds: Vec<(String, u64)> = out
            .iter()
            .map(|c| {
                let s = crate::source::open_path(&c.path, None).unwrap();
                (c.name.clone(), s.info().count.unwrap_or(0))
            })
            .collect();
        assert_eq!(kinds, vec![("mixed (points)".to_string(), 2), ("mixed (polygons)".to_string(), 1)], "{kinds:?}");
        // Converted once: the second call reuses the cache.
        let captured = crate::provenance::Capture::start();
        cached_list(&cache, &kml).unwrap();
        let (again, _) = convert_in(&cache, &kml, Some("mixed"), None).unwrap();
        let fresh = convert_one_fresh_in(&cache, &kml, "mixed", Some(Family::Points), None).unwrap();
        assert_eq!(captured.inputs().iter().map(|i| &i.path).collect::<Vec<_>>(), vec![&kml.display().to_string()]);
        drop(captured);
        // Repeated streamed reads attribute both persistent and fresh conversions to the source.
        for (path, workspace) in [(&again[0].path, None), (&fresh.path, Some(fresh.workspace.clone()))] {
            let source = super::super::fgb::FgbSource::open_converted_path(path, "mixed", &kml, workspace).unwrap();
            let captured = crate::provenance::Capture::start();
            source.read(&super::super::Request { extent: None, limit: 1, own: None }).unwrap();
            source.visit_chunks(None, 1024, &mut |_| Ok(())).unwrap();
            assert_eq!(captured.inputs().iter().map(|i| &i.path).collect::<Vec<_>>(), vec![&kml.display().to_string()]);
        }
        assert_eq!(again.iter().map(|c| &c.path).collect::<Vec<_>>(), out.iter().map(|c| &c.path).collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_nonempty_conversion_counts_are_reported_and_known_clean_counts_are_silent() {
        let note = conversion_count_note("source", None, 1, true).unwrap();
        assert!(note.contains("source count is unknown") && note.contains("features may have been left out"));
        assert_eq!(note.loss, None, "only a possible loss");
        assert!(conversion_count_note("source", Some(1), 1, true).is_none());
        assert!(conversion_count_note("source", Some(0), 0, false).is_none());
        assert!(conversion_count_note("source", None, 0, false).is_none());
        let short = conversion_count_note("source", Some(3), 1, true).unwrap();
        assert_eq!(short.as_str(), "source: GDAL conversion left out 2 of 3 source features");
        assert_eq!(short.loss, Some(crate::loss::LossCode::DroppedFeature));
    }

    /// Indexed FlatGeobuf conversion drops empty polygons in GDAL, before RustGIS reads it.
    /// Both a new conversion and a cache hit must say how many source features were lost.
    #[test]
    fn conversion_reports_empty_polygons_left_out_but_not_clean_input() {
        if !test_runs().unwrap() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let polygon = r#"{"type":"Feature","properties":{"id":2},"geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}"#;
        let empty = r#"{"type":"Feature","properties":{"id":1},"geometry":{"type":"Polygon","coordinates":[]}}"#;
        for lossy in [true, false] {
            let path = dir.path().join(if lossy { "lossy.geojsonl" } else { "clean.geojsonl" });
            std::fs::write(&path, if lossy { format!("{empty}\n{polygon}\n") } else { format!("{polygon}\n") }).unwrap();
            for _ in 0..2 {
                let (converted, notes) = convert_in(&dir.path().join("cache"), &path, None, None).unwrap();
                assert_eq!(converted.len(), 1);
                let source = super::super::open_path(&converted[0].path, None).unwrap();
                let batch = source.read(&super::super::Request { extent: None, limit: 10, own: None }).unwrap();
                assert_eq!(batch.features.len(), 1);
                assert_eq!(batch.skipped, 0, "GDAL dropped the feature before RustGIS read it");
                if lossy {
                    assert_eq!(notes, vec!["lossy: GDAL conversion left out 1 of 2 source features"]);
                } else {
                    assert!(notes.is_empty(), "clean input loses no features: {notes:?}");
                }
            }
        }
        let empties = dir.path().join("empties.geojsonl");
        std::fs::write(&empties, format!("{empty}\n{empty}\n")).unwrap();
        // A file of only empty polygons converts to nothing: the error says how many were left out.
        for _ in 0..2 {
            let error = convert_in(&dir.path().join("cache"), &empties, None, None).unwrap_err().to_string();
            assert!(error.contains("empties: GDAL conversion left out 2 of 2 source features"), "{error}");
        }
        // A cached conversion whose header can't be read is said, and the layer still comes back.
        // Repair refuses malformed geometry rather than installing a partial conversion.
        let lossy = dir.path().join("lossy.geojsonl");
        assert!(convert_one_fresh_in(&dir.path().join("fresh"), &lossy, "lossy", None, None).is_err());
        let clean = dir.path().join("clean.geojsonl");
        let converted = convert_one_fresh_in(&dir.path().join("fresh"), &clean, "clean", None, None).unwrap();
        assert!(converted.notes.is_empty(), "{:?}", converted.notes);

        let path = dir.path().join("clean.geojsonl");
        let (converted, _) = convert_in(&dir.path().join("cache"), &path, None, None).unwrap();
        std::fs::write(&converted[0].path, b"not a FlatGeobuf").unwrap();
        let (again, notes) = convert_in(&dir.path().join("cache"), &path, None, None).unwrap();
        assert_eq!(again.len(), 1);
        assert!(notes.len() == 1 && notes[0].starts_with("clean: couldn't check how many features GDAL's conversion kept"), "{notes:?}");
    }

    /// A conversion hit must not open the source in GDAL again. Preserve exactly the signature
    /// the conversion cache trusts while making the source unreadable by its allowed driver.
    #[test]
    fn reused_conversion_skips_listing_and_keeps_the_source_count() {
        if !test_runs().unwrap() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parcels.geojsonl");
        let input = concat!(
            "{\"type\":\"Feature\",\"properties\":{},\"geometry\":{\"type\":\"Polygon\",\"coordinates\":[]}}\n",
            "{\"type\":\"Feature\",\"properties\":{},\"geometry\":{\"type\":\"Polygon\",\"coordinates\":[[[0,0],[1,0],[1,1],[0,0]]]}}\n"
        );
        std::fs::write(&path, input).unwrap();
        let cache = dir.path().join("cache");
        let (first, notes) = convert_in(&cache, &path, None, None).unwrap();
        assert_eq!(notes, vec!["parcels: GDAL conversion left out 1 of 2 source features"]);
        let modified = path.metadata().unwrap().modified().unwrap();
        std::fs::write(&path, vec![b'!'; input.len()]).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(modified).unwrap();
        assert!(list(&path).is_err(), "the source must require the stored listing");
        let (again, reused_notes) = convert_in(&cache, &path, None, None).unwrap();
        assert_eq!(again, first);
        assert_eq!(reused_notes, notes, "the cached count must still detect conversion loss");
        assert_eq!(cached_list(&cache, &path).unwrap()[0].count, Some(2));
        assert!(convert_one_fresh_in(&cache, &path, "parcels", None, None).is_err(), "Reload must always list afresh");

        let metadata =
            std::fs::read_dir(&cache).unwrap().map(|e| e.unwrap().path()).find(|p| p.extension().is_some_and(|e| e == "layers")).unwrap();
        let stored = std::fs::read(&metadata).unwrap();
        // Even at the right hash, a different full key must not be trusted.
        let mut wrong: Listing = serde_json::from_slice(&stored).unwrap();
        wrong.key.push('!');
        std::fs::write(&metadata, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(convert_in(&cache, &path, None, None).is_err());
        std::fs::write(&metadata, b"broken metadata").unwrap();
        assert!(convert_in(&cache, &path, None, None).is_err());
        std::fs::remove_file(&metadata).unwrap();
        assert!(convert_in(&cache, &path, None, None).is_err());
        // Old caches without metadata are repaired by listing once, preserving their conversion.
        std::fs::write(&path, input).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(modified).unwrap();
        assert_eq!(convert_in(&cache, &path, None, None).unwrap(), (first, notes));

        // Each part of the existing source signature invalidates stored metadata independently.
        std::fs::write(&path, vec![b'!'; input.len()]).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(modified + std::time::Duration::from_secs(1)).unwrap();
        assert!(cached_list(&cache, &path).is_err(), "modification time changed");
        std::fs::write(&path, vec![b'!'; input.len() + 1]).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(modified).unwrap();
        assert!(cached_list(&cache, &path).is_err(), "size changed");
    }

    #[test]
    fn fresh_conversion_reports_dropped_dimensions_but_not_for_clean_xy_data() {
        if !test_runs().unwrap() {
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
            assert_eq!(
                fresh
                    .notes
                    .iter()
                    .any(|n| n.loss == Some(crate::loss::LossCode::DroppedZm) && n.contains("Z or M coordinates on 1 source feature")),
                lossy
            );
            assert_eq!(fresh.zm_dropped, lossy);
            // A first conversion, as Add Data makes it, counts them too.
            let (parts, _) = convert_in(&dir.join(format!("added-{lossy}")), &path, None, None).unwrap();
            assert_eq!(parts.iter().map(|p| p.zm_dropped).sum::<u64>(), u64::from(lossy));
            // A cache hit takes the count kept beside the conversion, without scanning the source.
            if lossy {
                for part in &parts {
                    std::fs::write(part.path.with_extension("zm"), "7\n").unwrap();
                }
                let (hit, _) = convert_in(&dir.join(format!("added-{lossy}")), &path, None, None).unwrap();
                assert_eq!(hit.iter().map(|p| p.zm_dropped).collect::<Vec<_>>(), vec![7; parts.len()]);
                // A kept count that can't be read is counted again, and kept.
                for part in &parts {
                    std::fs::write(part.path.with_extension("zm"), "junk").unwrap();
                }
                let (hit, _) = convert_in(&dir.join(format!("added-{lossy}")), &path, None, None).unwrap();
                assert_eq!(hit.iter().map(|p| p.zm_dropped).sum::<u64>(), 1, "{hit:?}");
                assert!(parts.iter().all(|p| std::fs::read_to_string(p.path.with_extension("zm")).is_ok_and(|t| t != "junk")));
            }
            let source = crate::source::open_path(&fresh.path, None).unwrap();
            let batch = source.read(&crate::source::Request { extent: None, limit: 10, own: None }).unwrap();
            assert_eq!(batch.features[0].geometry.as_ref().unwrap().extent().center().x, if lossy { 2.0 } else { 8.0 });
        }
        // A layer of points with Z, two of its three features with only XY: the one with Z is
        // counted, not the layer.
        let mixed = ["[2,48]", "[3,48,100]", "[4,48]"]
            .map(|c| format!(r#"{{"type":"Feature","properties":{{"rank":1}},"geometry":{{"type":"Point","coordinates":{c}}}}}"#));
        std::fs::write(&path, mixed.join("\n")).unwrap();
        let fresh = convert_one_fresh_in(&dir.join("cache"), &path, "points", None, None).unwrap();
        let said: Vec<_> = fresh.notes.iter().filter(|n| n.contains("Z or M")).collect();
        assert_eq!(said.len(), 1, "{:?}", fresh.notes);
        assert!(said[0].contains("Z or M coordinates on 1 source feature(s)"), "{said:?}");
        assert!(fresh.zm_dropped);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn fresh_csv_diagnostics_count_repeated_overflows_without_claiming_feature_loss() {
        if !test_runs().unwrap() {
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
            let overflow: Vec<_> = fresh.notes.iter().filter(|n| n.text.to_ascii_lowercase().contains("integer overflow")).collect();
            if lossy {
                assert_eq!(overflow.len(), 1, "{:?}", fresh.notes);
                assert!(overflow[0].ends_with("(3 occurrence(s))"), "{overflow:?}");
            } else {
                assert!(fresh.notes.is_empty(), "{:?}", fresh.notes);
            }
            let source = crate::source::open_path(&fresh.path, None).unwrap();
            let batch = source.read(&crate::source::Request { extent: None, limit: 10, own: None }).unwrap();
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
        if !test_runs().unwrap() {
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
                assert_eq!(
                    omitted.iter().map(|n| n.as_str()).collect::<Vec<_>>(),
                    vec!["GDAL: 2 features of another geometry type were left out"]
                );
            } else {
                assert!(omitted.is_empty(), "{:?}", fresh.notes);
                assert!(fresh.notes.is_empty(), "single-family input has no conversion losses: {:?}", fresh.notes);
            }
            let source = crate::source::open_path(&fresh.path, None).unwrap();
            let batch = source.read(&crate::source::Request { extent: None, limit: 10, own: None }).unwrap();
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
        if !test_runs().unwrap() {
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
            let batch = source.read(&crate::source::Request { extent: None, limit: 10, own: None }).unwrap();
            assert_eq!(batch.features.len(), 2);
            assert!(batch.features.iter().all(|f| f.geometry.as_ref().unwrap().kind() == GeometryType::Point));
        }
    }

    #[test]
    fn generic_dimension_count_includes_measures_and_quotes_layer_names() {
        if !test_runs().unwrap() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("surv'ey.csv");
        for (point, expected) in [("POINT M (2 48 100)", 1), ("POINT ZM (2 48 100 200)", 1), ("POINT (2 48)", 0)] {
            std::fs::write(&path, format!("WKT,name\n\"{point}\",point\n\"LINESTRING Z (2 48 100,3 48 100)\",line\n")).unwrap();
            let info = list(&path).unwrap().remove(0);
            assert_eq!(count_zm_features(&path, &info, Some(Family::Points)).unwrap(), Some(expected));
        }
    }
}
