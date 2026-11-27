//! Coordinate reference systems for rasters, by EPSG code or by definition.
//!
//! WGS 84 longitude/latitude and Web Mercator are handled directly; any other EPSG or ESRI code is
//! looked up in terrio-crs's catalog ([`epsg_proj4`], [`esri_proj4`]) and transformed by
//! terrio-crs, a pure-Rust PROJ port, so the browser build works too. A file's own (user-defined) projection,
//! such as the Albers of the NLCD land cover, is a [`Crs::Custom`]: its PROJ definition, kept in a
//! table for the run.

use rustgis_geom::{Coord, lonlat_to_merc, merc_to_lonlat};
use serde::{Deserialize, Serialize};

/// The PROJ definition of an EPSG code, as GDAL 3.13.3 writes it (`OSRExportToProj4`, with its
/// `+towgs84`) from terrio-crs's EPSG catalog (PROJ 9.9.0, EPSG v13.102), or of a code the catalog
/// can't compute that RustGIS read before ([`UNCOMPUTED_EPSG`]). `None` for a code neither has, or
/// one of heights alone. terrio-crs transforms a catalog code from the catalog itself
/// ([`Crs::transformer`]).
pub fn epsg_proj4(code: u16) -> Option<&'static str> {
    catalog_proj4(code).or_else(|| UNCOMPUTED_EPSG.iter().find(|&&(c, _)| c == code).map(|&(_, def)| def))
}

/// EPSG codes RustGIS read before (by PostGIS's `spatial_ref_sys` definitions) whose method PROJ
/// 9.9.0 can't write as a PROJ string, and terrio-crs's catalog doesn't compute: data written with
/// them still opens where it did.
const UNCOMPUTED_EPSG: &[(u16, &str)] = &[
    // WGS 84 / Mercator 41: a Mercator whose latitude of origin isn't the equator.
    (3752, "+proj=merc +lon_0=100 +lat_ts=-41 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"),
    // Deir ez Zor / Levant Zone: Lambert Conic Near-Conformal, written as the conformal conic.
    (
        22700,
        "+proj=lcc +lat_1=34.65 +lat_0=34.65 +lon_0=37.35 +k_0=0.9996256 +x_0=300000 +y_0=300000 +a=6378249.2 +b=6356515 +towgs84=-190.421,8.532,238.69,0,0,0,0 +units=m +no_defs",
    ),
];

/// The PROJ definition of an EPSG code of terrio-crs's catalog, kept for the run (once a code:
/// there are at most as many as the catalog has codes).
fn catalog_proj4(code: u16) -> Option<&'static str> {
    static KEPT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u16, Option<&'static str>>>> = std::sync::OnceLock::new();
    if epsg_catalog_codes().binary_search(&u32::from(code)).is_err() {
        return None;
    }
    let kept = KEPT.get_or_init(Default::default);
    if let Some(&def) = kept.lock().unwrap_or_else(|e| e.into_inner()).get(&code) {
        return def;
    }
    // Worked out without the lock, so other threads' codes aren't held up by this one's first
    // look-up; two threads asking for the same new code at once keep the first's.
    let def = terrio_crs::SpatialRef::from_epsg(u32::from(code))
        .ok()
        .and_then(|sr| sr.to_proj4_gdal())
        // A vertical system alone (`+vunits=m`, heights without a place) isn't a layer's or a
        // raster's.
        .filter(|def| def.starts_with("+proj="))
        .map(|def| with_shift(def, Some(u32::from(code))));
    *kept.lock().unwrap_or_else(|e| e.into_inner()).entry(code).or_insert_with(|| def.map(|d| &*Box::leak(d.into_boxed_str())))
}

/// `def` with the shift to WGS 84 PROJ 9.9.0 takes without grids for the EPSG geographic system
/// `geog` (or a projected one on it), where `def` gives none. GDAL 3.13.3 writes no `+towgs84` for a
/// datum with several transformations to WGS 84 (ED50, OSGB 1936, Pulkovo 1942, …), so a file's
/// WKT with that shift wouldn't be the code's own.
fn with_shift(def: String, geog: Option<u32>) -> String {
    if ["+towgs84=", "+datum=", "+nadgrids="].iter().any(|k| def.contains(k)) {
        return def;
    }
    let Some(shift) = geog.and_then(terrio_crs::helmert_to_wgs84) else { return def };
    let towgs84 = format!("+towgs84={}", shift.iter().map(f64::to_string).collect::<Vec<_>>().join(","));
    // Where GDAL writes it: after the ellipsoid, before the units.
    match def.find(" +units=").or_else(|| def.find(" +no_defs")) {
        Some(at) => {
            def.get(..at).zip(def.get(at..)).map_or_else(|| format!("{def} {towgs84}"), |(head, tail)| format!("{head} {towgs84}{tail}"))
        }
        None => format!("{def} {towgs84}"),
    }
}

/// The EPSG codes of terrio-crs's catalog, deprecated ones included (data written with them holds
/// their coordinates), sorted.
fn epsg_catalog_codes() -> &'static [u32] {
    static CODES: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    CODES.get_or_init(|| {
        let query = terrio_crs::CrsQuery { authorities: vec!["EPSG".into()], include_deprecated: true, ..Default::default() };
        let mut codes: Vec<u32> = terrio_crs::list_crs(&query).into_iter().map(|e| e.code).collect();
        codes.sort_unstable();
        codes.dedup();
        codes
    })
}

/// Whether the EPSG system `code` is written north (or latitude) first, as EPSG defines its axes:
/// WMS 1.3.0 (§6.7.3.3) and WMTS 1.0.0 write a bounding box and a tile matrix's `TopLeftCorner` in
/// that order, where RustGIS's coordinates are east first. `Some(true)` for a system whose first
/// axis is a northing or a latitude (EPSG:3035, 31467, 4326), `Some(false)` for another RustGIS
/// knows, and `None` for one whose order isn't known: a code RustGIS doesn't know, or a first axis
/// pointing south or west that RustGIS doesn't turn so (`+axis=`), whose coordinates swapping
/// can't put in its order. A system whose `+axis=` RustGIS applies (Krovak, EPSG:5513 and 8352;
/// the southern African Lo grids) has its coordinates in its own order already, as are polar
/// stereographic systems' easting and northing pointing along meridians (EPSG:3413).
pub(crate) fn epsg_north_first(code: u16) -> Option<bool> {
    // Asked once per bounding box of a service's layers: kept for the run, at most once per code.
    static KEPT: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u16, Option<bool>>>> = std::sync::OnceLock::new();
    let kept = KEPT.get_or_init(Default::default);
    if let Some(n) = kept.lock().unwrap_or_else(|e| e.into_inner()).get(&code) {
        return *n;
    }
    let n = north_first_of(code);
    kept.lock().unwrap_or_else(|e| e.into_inner()).insert(code, n);
    n
}

/// [`epsg_north_first`], worked out.
fn north_first_of(code: u16) -> Option<bool> {
    let Ok(sr) = terrio_crs::SpatialRef::from_epsg(u32::from(code)) else {
        // A code the catalog doesn't compute, which RustGIS still reads (EPSG:3752, 22700): east
        // first, as EPSG defines both.
        return epsg_proj4(code).map(|_| false);
    };
    if sr.to_proj4().is_some_and(|d| d.split_whitespace().any(|t| t.starts_with("+axis="))) {
        return Some(false);
    }
    match sr.axis_order() {
        terrio_crs::AxisOrder::NorthEast => Some(true),
        terrio_crs::AxisOrder::EastNorth => Some(false),
        terrio_crs::AxisOrder::Other => {
            // The root's first axis: the second-to-last `AXIS[` (the root's two close the WKT),
            // by its name (a polar system's `"Easting",SOUTH`), else its direction (`"Y",NORTH`).
            // Southing or westing first can't be given by swapping.
            let wkt = sr.to_wkt()?;
            let axes: Vec<&str> = wkt.match_indices("AXIS[").filter_map(|(i, _)| wkt.get(i + 5..)?.split(']').next()).collect();
            let first = axes.len().checked_sub(2).and_then(|i| axes.get(i))?.to_ascii_uppercase();
            let name = first.split(',').next().unwrap_or_default().trim().trim_matches('"').trim().to_string();
            let direction = first.split(',').nth(1).unwrap_or_default().trim().to_string();
            if name.starts_with("NORTH") || name.contains("LAT") {
                Some(true)
            } else if name.starts_with("EAST") || name.contains("LON") {
                Some(false)
            } else if direction.starts_with("NORTH") {
                Some(true)
            } else if direction.starts_with("EAST") {
                Some(false)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Why a file's EPSG code can't be used.
pub fn unusable_epsg(code: u32) -> String {
    format!("the coordinate system EPSG:{code} isn't supported")
}

/// Whether a bare number (`102033`) is read as ESRI's code: it isn't one RustGIS reads as EPSG's
/// (`2056`, and Web Mercator's `102100`), and ESRI has it. [`Crs::parse`], the coordinate system
/// boxes and their lists all read a bare number by this.
pub fn bare_number_is_esri(code: u32) -> bool {
    Crs::from_epsg(code).is_none() && esri_proj4(code).is_some()
}

/// The PROJ definition of one of Esri's projected coordinate systems (`ESRI:102033`, South
/// America Albers), as GDAL 3.13.3 writes it from terrio-crs's catalog of them (PROJ 9.9.0's ESRI
/// authority), else of one the catalog lacks that RustGIS read before ([`UNCOMPUTED_ESRI`]).
/// `None` for a code neither has.
pub fn esri_proj4(code: u32) -> Option<&'static str> {
    catalog_esri(code).map(|(def, _)| def).or_else(|| UNCOMPUTED_ESRI.iter().find(|&&(c, _)| c == code).map(|&(_, def)| def))
}

/// ESRI codes RustGIS read before that terrio-crs's catalog lacks: data written with them still
/// opens where it did.
const UNCOMPUTED_ESRI: &[(u32, &str)] = &[
    // WGS_1984_Agriculture_Canada_Albers and WGS_1984_SIRES_TMQ, which PROJ 9.9.0 no longer has,
    // as PROJ 9.7.1 wrote them.
    (10820, "+proj=aea +lat_0=40 +lon_0=-96 +lat_1=44.75 +lat_2=55.75 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"),
    (65162, "+proj=tmerc +lat_0=0 +lon_0=-78.5 +k=1.0004584 +x_0=500000 +y_0=10000000 +datum=WGS84 +units=m +no_defs"),
];

/// An ESRI projected coordinate system of terrio-crs's catalog, kept for the run (at most once a
/// code): its PROJ definition and the WKT GDAL writes for it, which terrio-crs transforms with as
/// PROJ 9.9.0 does. `None` for a code the catalog lacks, isn't projected or isn't computed.
fn catalog_esri(code: u32) -> Option<(&'static str, &'static str)> {
    type Kept = std::collections::HashMap<u32, Option<(&'static str, &'static str)>>;
    static KEPT: std::sync::OnceLock<std::sync::Mutex<Kept>> = std::sync::OnceLock::new();
    if esri_catalog_codes().binary_search(&code).is_err() {
        return None;
    }
    let kept = KEPT.get_or_init(Default::default);
    if let Some(&def) = kept.lock().unwrap_or_else(|e| e.into_inner()).get(&code) {
        return def;
    }
    // Worked out without the lock, as [`catalog_proj4`] does.
    let def = terrio_crs::SpatialRef::from_esri(code)
        .ok()
        .and_then(|sr| Some((with_shift(sr.to_proj4_gdal()?, sr.geographic_code()), sr.to_wkt()?)));
    *kept
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(code)
        .or_insert_with(|| def.map(|(d, w)| (&*Box::leak(d.into_boxed_str()), &*Box::leak(w.into_boxed_str()))))
}

/// The projected ESRI codes of terrio-crs's catalog that it computes, deprecated ones included
/// (ArcGIS still writes many of them), sorted.
fn esri_catalog_codes() -> &'static [u32] {
    static CODES: std::sync::OnceLock<Vec<u32>> = std::sync::OnceLock::new();
    CODES.get_or_init(|| {
        let query = terrio_crs::CrsQuery {
            authorities: vec!["ESRI".into()],
            kinds: vec![terrio_crs::CrsKind::Projected],
            include_deprecated: true,
            ..Default::default()
        };
        let mut codes: Vec<u32> = terrio_crs::list_crs(&query).into_iter().map(|e| e.code).collect();
        codes.sort_unstable();
        codes.dedup();
        codes
    })
}

/// Every ESRI code [`esri_proj4`] knows, in order.
pub fn esri_codes() -> impl Iterator<Item = u32> {
    let mut codes: Vec<u32> = UNCOMPUTED_ESRI
        .iter()
        .map(|&(c, _)| c)
        .chain(esri_catalog_codes().iter().copied().filter(|&c| catalog_esri(c).is_some()))
        .collect();
    codes.sort_unstable();
    codes.dedup();
    codes.into_iter()
}

/// The WKT terrio-crs's catalog gives the ESRI projected system `code`, which places its points as
/// PROJ 9.9.0 does. `None` for a code the catalog doesn't compute.
pub fn esri_wkt(code: u32) -> Option<&'static str> {
    catalog_esri(code).map(|(_, wkt)| wkt)
}

/// One of Esri's projected coordinate systems by its code (`ESRI:102033`): transformed by the WKT
/// terrio-crs's catalog gives it, with the datum transformation PROJ 9.9.0 picks, where the
/// catalog has it; else (or where its WKT can't be transformed) by its PROJ definition. `None`
/// for a code [`esri_proj4`] lacks.
pub fn esri_crs(code: u32) -> Option<Crs> {
    let proj4 = esri_proj4(code)?;
    let custom = |wkt: Option<&str>| {
        Crs::custom(CustomCrs {
            proj4: proj4.to_string(),
            label: format!("ESRI:{code}"),
            wkt: wkt.map(str::to_string),
            ..CustomCrs::default()
        })
        .ok()
    };
    catalog_esri(code).and_then(|(_, wkt)| custom(Some(wkt))).or_else(|| custom(None))
}

/// A raster's coordinate system. Saved as its EPSG code, or a custom one as its definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(into = "SavedCrs", try_from = "SavedCrs")]
pub enum Crs {
    /// WGS 84 longitude/latitude in degrees (EPSG:4326).
    LonLat,
    /// Web Mercator metres (EPSG:3857).
    WebMercator,
    /// Any other EPSG code.
    Epsg(u16),
    /// A projection defined in the file (see [`Crs::custom`]); the number is its place in this
    /// run's table, so it is never saved (rasters are read again from their files).
    Custom(u32),
}

/// How a [`Crs`] is saved: a custom system by its PROJ definition, never by its number in a run's
/// table. One read from WKT keeps that WKT beside it (`wkt`), which this release reads first;
/// older releases ignore the field and read the PROJ definition, as they always have.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
enum SavedCrs {
    LonLat,
    WebMercator,
    Epsg(u16),
    Proj {
        proj4: String,
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wkt: Option<String>,
    },
}

impl From<Crs> for SavedCrs {
    fn from(c: Crs) -> Self {
        match c {
            Crs::LonLat => SavedCrs::LonLat,
            Crs::WebMercator => SavedCrs::WebMercator,
            Crs::Epsg(e) => SavedCrs::Epsg(e),
            Crs::Custom(_) => match c.custom_def() {
                Some(d) => {
                    if d.wkt_needed() {
                        WKT_SAVED.with(|s| s.set(true));
                    }
                    SavedCrs::Proj { proj4: d.proj4.clone(), label: d.label.clone(), wkt: d.wkt.clone() }
                }
                None => SavedCrs::LonLat,
            },
        }
    }
}

thread_local! {
    /// Set when a system is saved whose PROJ definition doesn't stand in for its WKT
    /// ([`CustomCrs::wkt_needed`]), for [`saves_wkt`].
    static WKT_SAVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether saving `value` saves a system whose PROJ definition doesn't place points as its WKT
/// does ([`CustomCrs::wkt_needed`]): an older release reads only the definition, so a saved file
/// with one requires `crs-wkt`. Serializes `value` to `io::sink()` (when this run has such a
/// system); kept as a serialization oracle for tests alongside the direct [`Crs::needs_wkt`]
/// field walks.
pub fn saves_wkt(value: &impl Serialize) -> bool {
    let table: Vec<std::sync::Arc<CustomCrs>> = custom_table().lock().unwrap_or_else(|e| e.into_inner()).clone();
    if !table.iter().any(|d| d.wkt_needed()) {
        return false;
    }
    WKT_SAVED.with(|s| s.set(false));
    let _ = serde_json::to_writer(std::io::sink(), value);
    WKT_SAVED.with(|s| s.replace(false))
}

impl TryFrom<SavedCrs> for Crs {
    type Error = String;
    fn try_from(s: SavedCrs) -> Result<Self, String> {
        Ok(match s {
            SavedCrs::LonLat => Crs::LonLat,
            SavedCrs::WebMercator => Crs::WebMercator,
            // A code another variant stands for (4326, 3857 and its aliases), as a file written
            // by hand or by another program may give it, is that variant, as `from_epsg` gives it.
            SavedCrs::Epsg(e) => Crs::from_epsg(u32::from(e)).unwrap_or(Crs::Epsg(e)),
            SavedCrs::Proj { proj4, label, wkt } => Crs::custom(CustomCrs { proj4, label, wkt, ..CustomCrs::default() })?,
        })
    }
}

/// A coordinate system a file defines itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CustomCrs {
    /// The PROJ definition (`+proj=aea +lat_1=29.5 …`). Empty for a system read from WKT that
    /// PROJ writes no PROJ string for and RustGIS wrote none for before ([`Self::wkt`]).
    pub proj4: String,
    /// What to call it: the file's citation, or the projection's name.
    pub label: String,
    /// The GeoTIFF keys it came from, written back when the raster is saved as a GeoTIFF: the key
    /// directory, its double parameters and its ASCII parameters.
    pub geokeys: Vec<u16>,
    pub doubles: Vec<f64>,
    pub ascii: String,
    /// The WKT it was read from, which terrio-crs transforms with when given: a PROJ definition
    /// can't hold all of it (the order and direction of its axes, its angle unit, a registry
    /// datum's transformation), so `proj4` is then only its closest PROJ string, for what reads one
    /// (notes, older releases).
    pub wkt: Option<String>,
}

/// The most custom systems one run keeps.
const MAX_CUSTOM: usize = 1024;
/// The most of them typed as PROJ strings (`Crs::parse`): text from commands can't take the room
/// files' own systems need.
const MAX_TYPED: usize = 256;
/// The name a system typed as a PROJ string gets.
const TYPED_LABEL: &str = "Custom PROJ";

fn custom_table() -> &'static std::sync::Mutex<Vec<std::sync::Arc<CustomCrs>>> {
    static T: std::sync::OnceLock<std::sync::Mutex<Vec<std::sync::Arc<CustomCrs>>>> = std::sync::OnceLock::new();
    T.get_or_init(Default::default)
}

/// The VN-2000 to WGS 84 Helmert shift as a literal, for `concat!` (rustgis-map's presets):
/// PROJ's position-vector `+towgs84` for EPSG:3405, 3406 and 5897.
#[doc(hidden)]
#[macro_export]
macro_rules! vn2000_towgs84 {
    () => {
        "+towgs84=-191.90441429,-39.30318279,-111.45032835,0.00928836,-0.01975479,0.00427372,0.252906278"
    };
}

/// The 7-parameter Helmert shift from VN-2000 to WGS 84, as PROJ gives it for EPSG:4756.
pub const VN2000_TOWGS84: &str = crate::vn2000_towgs84!();

/// A name's ASCII letters and digits, upper-cased: how VN-2000 names are matched, whatever the
/// spacing, punctuation and case.
fn squash(text: &str) -> String {
    text.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_uppercase()).collect()
}

/// Returns true if `text` is an ambiguous bare reference to VN-2000 without a specified projection.
pub fn is_bare_vn2000(text: &str) -> bool {
    squash(text) == "VN2000"
}

/// The EPSG code an explicit VN-2000 system name stands for, matched on its letters and digits
/// in any case: `VN-2000 / UTM zone 48N`, `vn2000_utm48` or `vn-2000-48` (3405), the same for
/// 49 (3406), `VN-2000 / TM-3` or `vn2000tm3` (5897). The one table for [`Crs::parse`] and
/// rustgis-map's presets, so a name one takes the other takes too.
pub fn vn2000_epsg(text: &str) -> Option<u16> {
    match squash(text).as_str() {
        "VN2000UTMZONE48N" | "VN2000UTM48N" | "VN2000UTM48" | "VN2000UTMZONE48" | "VN200048" | "VN200048N" => Some(3405),
        "VN2000UTMZONE49N" | "VN2000UTM49N" | "VN2000UTM49" | "VN2000UTMZONE49" | "VN200049" | "VN200049N" => Some(3406),
        // EPSG:5897, VN-2000 / TM-3 zone 482: central meridian 105°, k 0.9999.
        "VN2000TM3" | "VN2000TM3VIETNAM" | "VN2000TM3105" | "VN2000TM3LON0105" => Some(5897),
        _ => None,
    }
}

/// The error message returned when an ambiguous bare VN-2000 name is provided.
pub fn bare_vn2000_error(text: &str) -> String {
    format!(
        "coordinate system '{text}' is ambiguous: specify 'VN-2000 / UTM zone 48N' (vn2000Utm48, EPSG:3405), 'VN-2000 / UTM zone 49N' (vn2000Utm49, EPSG:3406), or TM-3 ('vn2000tm3' / 'VN-2000 / TM-3')"
    )
}

/// Checks a PROJ string typed by a user (not one read from a file, whose `+nadgrids=@null` is
/// fine): no keys naming grid or definition files, as for GDAL (`raster::gdal`), no pipelines,
/// and no paths. Every key with `grid` in it is refused (`nadgrids`, `geoidgrids`, `vgrids`,
/// `xy_grids`, `z_grids`, `gridlist`…), so a grid key PROJ adds later is refused too.
fn validate_proj_string(proj4: &str) -> Result<(), String> {
    for token in proj4.split_whitespace() {
        let t = token.trim_start_matches('+');
        let (key, val) = t.split_once('=').unwrap_or((t, ""));
        if proj_key_refused(key, val) {
            return Err(format!("parameter '+{t}' is not allowed in coordinate system definition"));
        }
        if val.contains("..") || val.contains('/') || val.contains('\\') {
            return Err("file path in parameter is not allowed in coordinate system definition".into());
        }
    }
    Ok(())
}

/// Whether a PROJ parameter names files PROJ would read (any key with `grid` in it, `init`,
/// `file`, `catalog`) or builds a pipeline (`step`, `proj=pipeline`): refused in definitions
/// typed by a user and in those handed to GDAL's programs (`raster::gdal`).
pub(crate) fn proj_key_refused(key: &str, val: &str) -> bool {
    let key = key.trim().to_ascii_lowercase();
    key.contains("grid")
        || ["init", "file", "catalog", "step"].contains(&key.as_str())
        || (key == "proj" && val.trim().eq_ignore_ascii_case("pipeline"))
}

impl Crs {
    /// A file's own coordinate system: checked (terrio-crs must compute its PROJ definition) and
    /// added to the run's table, once per definition.
    pub fn custom(def: CustomCrs) -> Result<Crs, String> {
        Crs::register(def, false)
    }

    /// Adds a system to the run's table; one `typed` as a PROJ string counts against
    /// `MAX_TYPED` too, checked under the same lock as the insertion.
    fn register(def: CustomCrs, typed: bool) -> Result<Crs, String> {
        // Room in the table first, so a definition refused for want of it isn't built (and kept).
        {
            let t = custom_table().lock().unwrap_or_else(|e| e.into_inner());
            if !t.iter().any(|d| d.proj4 == def.proj4 && d.wkt == def.wkt) {
                if t.len() >= MAX_CUSTOM {
                    return Err("too many coordinate systems defined by files".into());
                }
                if typed && t.iter().filter(|d| d.label == TYPED_LABEL).count() >= MAX_TYPED {
                    return Err("too many coordinate systems typed as PROJ strings".into());
                }
            }
        }
        def.transformer().map(|_| ()).map_err(|e| format!("{}: {e}", def.proj4))?;
        let mut t = custom_table().lock().unwrap_or_else(|e| e.into_inner());
        let index = |i: usize| u32::try_from(i).map(Crs::Custom).map_err(|_| "too many coordinate systems defined by files".to_string());
        if let Some(i) = t.iter().position(|d| d.proj4 == def.proj4 && d.wkt == def.wkt) {
            // Keys a file brings fill in a system first known without them (from a saved project).
            if let Some(slot) = t.get_mut(i)
                && slot.geokeys.is_empty()
                && !def.geokeys.is_empty()
            {
                *slot = std::sync::Arc::new(def);
            }
            return index(i);
        }
        if t.len() >= MAX_CUSTOM {
            return Err("too many coordinate systems defined by files".into());
        }
        if typed && t.iter().filter(|d| d.label == TYPED_LABEL).count() >= MAX_TYPED {
            return Err("too many coordinate systems typed as PROJ strings".into());
        }
        t.push(std::sync::Arc::new(def));
        index(t.len() - 1)
    }

    /// The definition of a [`Crs::Custom`].
    pub fn custom_def(self) -> Option<std::sync::Arc<CustomCrs>> {
        match self {
            Crs::Custom(i) => custom_table().lock().unwrap_or_else(|e| e.into_inner()).get(i as usize).cloned(),
            _ => None,
        }
    }

    /// Whether saving this coordinate system requires `crs-wkt` because its PROJ definition
    /// doesn't place points as its WKT does ([`CustomCrs::wkt_needed`]).
    pub fn needs_wkt(self) -> bool {
        matches!(self, Crs::Custom(_)) && self.custom_def().is_some_and(|d| d.wkt_needed())
    }

    /// The coordinate system for an EPSG code, or `None` when it isn't known.
    pub fn from_epsg(code: u32) -> Option<Crs> {
        match code {
            4326 => Some(Crs::LonLat),
            3857 | 900_913 | 102_100 | 102_113 | 3785 => Some(Crs::WebMercator),
            c => {
                let c = u16::try_from(c).ok()?;
                epsg_proj4(c).map(|_| Crs::Epsg(c))
            }
        }
    }

    /// A coordinate system given as text: `EPSG:2263`, `2263` (a number EPSG lacks and ESRI has,
    /// such as `102033`, is ESRI's: [`bare_number_is_esri`]), `ESRI:102033`, an OGC URN or URL
    /// ending in the code, WKT, a PROJ string (`+proj=…`, naming no grid or definition files), or an explicit
    /// VN-2000 system: `VN-2000 / UTM zone 48N` (EPSG:3405), `… 49N` (EPSG:3406) or
    /// `VN-2000 / TM-3` (EPSG:5897, central meridian 105°). A bare `VN-2000` is ambiguous.
    pub fn parse(text: &str) -> Result<Crs, String> {
        let t = text.trim();
        if t.contains('[') {
            return super::wkt::crs_from_wkt(t);
        }
        if t.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("+proj=")) {
            if t.len() > 1024 {
                return Err("coordinate system definition is too long".into());
            }
            let proj4 = format!("+proj{}", t.get(5..).unwrap_or(""));
            validate_proj_string(&proj4)?;
            return Crs::register(
                CustomCrs {
                    proj4,
                    label: TYPED_LABEL.to_string(),
                    geokeys: Vec::new(),
                    doubles: Vec::new(),
                    ascii: String::new(),
                    wkt: None,
                },
                true,
            );
        }
        let up = t.to_ascii_uppercase();
        if up.ends_with("CRS84") {
            return Ok(Crs::LonLat);
        }
        if is_bare_vn2000(t) {
            return Err(bare_vn2000_error(t));
        }
        if squash(t).starts_with("VN2000") {
            return vn2000_epsg(t).and_then(|c| Crs::from_epsg(c.into())).ok_or_else(|| format!("unknown coordinate system '{t}'"));
        }
        let code = t.rsplit([':', '/']).next().unwrap_or(t).trim();
        // `ESRI:102033`, as the coordinate system boxes offer it and a `.prj` names it.
        if let Some(rest) = up.strip_prefix("ESRI")
            && let Some(n) = rest.trim_start().strip_prefix(':').map(str::trim)
        {
            let c = Some(n)
                .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|n| n.parse::<u32>().ok())
                .ok_or_else(|| format!("'{t}': an ESRI code is a number, such as ESRI:102033"))?;
            return esri_crs(c).ok_or_else(|| format!("unknown coordinate system ESRI:{c}"));
        }
        let known = t.chars().all(|c| c.is_ascii_digit()) || up.contains("EPSG");
        match code.parse::<u32>() {
            // A bare number EPSG lacks and ESRI lists (`102033`) is ESRI's, as the boxes find it.
            Ok(c) if known => Crs::from_epsg(c)
                .or_else(|| (t.bytes().all(|b| b.is_ascii_digit()) && bare_number_is_esri(c)).then(|| esri_crs(c)).flatten())
                .ok_or_else(|| format!("unknown coordinate system EPSG:{c}")),
            _ => Err(format!("'{t}': give a coordinate system as EPSG:<code>, WKT, or +proj string")),
        }
    }

    /// The EPSG code (0 for a custom system, which has none).
    pub fn epsg(self) -> u16 {
        match self {
            Crs::LonLat => 4326,
            Crs::WebMercator => 3857,
            Crs::Epsg(c) => c,
            Crs::Custom(_) => 0,
        }
    }

    /// Text [`Crs::parse`] reads back as this system: `EPSG:32645`, or a custom one's definition.
    pub fn code(self) -> String {
        match self.custom_def() {
            Some(d) => d.proj4.clone(),
            None => format!("EPSG:{}", self.epsg()),
        }
    }

    /// `EPSG:32645`, or a custom system's name.
    pub fn label(self) -> String {
        match self.custom_def() {
            Some(d) => d.label.clone(),
            None => format!("EPSG:{}", self.epsg()),
        }
    }

    /// Whether coordinates are in degrees.
    pub fn is_geographic(self) -> bool {
        match self {
            Crs::LonLat => true,
            Crs::WebMercator => false,
            Crs::Epsg(c) => epsg_proj4(c).is_some_and(geographic_def),
            Crs::Custom(_) => self.custom_def().is_some_and(|d| geographic_def(&d.proj4)),
        }
    }

    /// Metres per unit of its coordinates: 1 for a system in metres, 0.3048 for one in feet,
    /// 0.304800609601219 for one in US survey feet; 1 when its conversion gives none (longitude/
    /// latitude, whose degrees are no length, and Web Mercator) or can't be made (a layer held in
    /// a system has made it: [`Dataset::hold_in`] converts through it).
    ///
    /// [`Dataset::hold_in`]: crate::Dataset::hold_in
    pub fn metres_per_unit(self) -> f64 {
        self.unit_and_axes().0
    }

    /// [`Crs::metres_per_unit`] and [`Crs::axis_directions`] together, from one conversion.
    pub fn unit_and_axes(self) -> (f64, [(f64, f64); 2]) {
        let t = self.transformer().ok();
        let unit = t.as_ref().and_then(|t| t.params().map(|p| p.to_meter)).filter(|m| m.is_finite() && *m > 0.0).unwrap_or(1.0);
        (unit, t.map_or([(1.0, 0.0), (0.0, 1.0)], |t| t.axis_directions()))
    }

    /// Which way its coordinates point ([`Transformer::axis_directions`]): east, then north, for
    /// one whose conversion can't be made (as [`Crs::metres_per_unit`], not one a layer is held in).
    pub fn axis_directions(self) -> [(f64, f64); 2] {
        self.unit_and_axes().1
    }

    /// Whether it is a projection of the whole world whose plane has the antimeridian as a seam
    /// across it and the poles as lines (cylindrical and pseudocylindrical maps: World Mercator,
    /// Plate Carrée, Robinson, Equal Earth, Mollweide and the like), by its PROJ method. A place
    /// there may be a seam's width from its neighbour on the ground, so it isn't one tools work in
    /// as a plane. Web Mercator is one too.
    pub fn is_world_projection(self) -> bool {
        const WORLD: &[&str] = &[
            "merc",
            "webmerc",
            "eqc",
            "cea",
            "mill",
            "gall",
            "robin",
            "eqearth",
            "moll",
            "eck1",
            "eck2",
            "eck3",
            "eck4",
            "eck5",
            "eck6",
            "wintri",
            "natearth",
            "natearth2",
            "sinu",
            "hammer",
            "aitoff",
            "kav5",
            "kav7",
            "wag1",
            "wag2",
            "wag3",
            "wag4",
            "wag5",
            "wag6",
            "wag7",
            "loxim",
            "mbtfps",
            "mbt_s",
            "goode",
            "igh",
            "vandg",
            "vandg2",
            "vandg3",
            "vandg4",
            "times",
            "patterson",
            "putp1",
            "putp2",
            "putp3",
            "putp4p",
            "putp5",
            "putp6",
            "quau",
            "gins8",
            "collg",
            "crast",
            "denoy",
            "fahey",
            "fouc",
            "gn_sinu",
            "hatano",
            "larr",
            "lask",
            "nell",
            "nell_h",
            "urmfps",
            "weren",
            "boggs",
            "bonne",
            "comill",
            "mbtfpq",
            "fouc_s",
            "wink1",
            "wink2",
        ];
        let proj4 = match self {
            Crs::LonLat => return false,
            Crs::WebMercator => return true,
            Crs::Epsg(c) => epsg_proj4(c).map(|d| d.to_string()),
            Crs::Custom(_) => self.custom_def().map(|d| d.proj4.clone()),
        };
        // (A method may be written quoted, as PROJ reads it.)
        proj4.is_some_and(|d| {
            d.split_whitespace().filter_map(|t| t.strip_prefix("+proj=")).map(|m| m.trim_matches('"')).any(|m| WORLD.contains(&m))
        })
    }

    /// Whether its coordinates are geocentric (X, Y and Z from the Earth's centre), not a map's.
    pub fn is_geocentric(self) -> bool {
        match self {
            Crs::LonLat | Crs::WebMercator => false,
            Crs::Epsg(c) => epsg_proj4(c).is_some_and(|d| d.contains("+proj=geocent")),
            Crs::Custom(_) => self.custom_def().is_some_and(|d| d.proj4.contains("+proj=geocent")),
        }
    }

    /// The unit its heights are in, when that isn't metres (`us-ft`, `0.3048 m`): its vertical unit,
    /// else a projected system's linear unit, the only sign of its heights' unit a definition may
    /// give. A geographic system with no vertical unit says nothing of its heights. A name goes
    /// before a factor, as PROJ reads them.
    pub(crate) fn height_unit_not_metre(self) -> Option<String> {
        let custom;
        let definition = match self {
            Crs::LonLat | Crs::WebMercator => return None,
            Crs::Epsg(c) => epsg_proj4(c)?,
            Crs::Custom(_) => {
                custom = self.custom_def()?;
                custom.proj4.as_str()
            }
        };
        let member = |key: &str| definition.split_whitespace().find_map(|t| t.strip_prefix(key));
        let stated = |units: &str, factor: &str| {
            let named = member(units).map(|u| (u != "m").then(|| u.to_string()));
            named.or_else(|| member(factor).map(|f| (f.parse::<f64>().ok() != Some(1.0)).then(|| format!("{f} m"))))
        };
        let geographic = geographic_def(definition);
        stated("+vunits=", "+vto_meter=").or_else(|| if geographic { None } else { stated("+units=", "+to_meter=") }).flatten()
    }

    /// The system as text that [`Crs::parse`] reads back as this very system: a custom one by the
    /// WKT it was read from (or its PROJ definition, without one), else its name.
    pub fn definition_text(self) -> String {
        match self.custom_def() {
            Some(d) => d.wkt.clone().unwrap_or_else(|| d.proj4.clone()),
            None => self.label(),
        }
    }

    /// Whether converting to WGS 84 leaves out a datum shift, so positions in longitude/latitude
    /// may be off by up to a few hundred metres: a custom system with an ellipsoid (`+ellps`, `+a`
    /// or `+R`) but no `+towgs84`, `+datum` or `+nadgrids`. That is what reading a WKT datum with
    /// no known shift writes, as `+a`/`+rf` even on WGS 84's axes (SVY21's), and the reading says
    /// so. `+ellps=WGS84` and `+ellps=GRS80` alone need no shift (GRS 80 datums the reader knows
    /// get a null one), nor does a Mercator on Web Mercator's sphere of radius 6378137 m. Written
    /// back in the same system, positions come back exact. False for another body's system
    /// ([`Crs::is_planetary`]): it has no shift to WGS 84 at all, and its own notice says so.
    pub fn lacks_datum_shift(self) -> bool {
        let Some(def) = self.custom_def() else { return false };
        // Web Mercator's sphere, of WGS 84's major axis, is a projection's, not a datum's: in a
        // Mercator only, as a datum on that sphere elsewhere is unknown (and reading it says so).
        // Typed or read from WKT, it converts exactly as Web Mercator does.
        let keys: Vec<&str> = def.proj4.split_whitespace().collect();
        let axis = |key: &str| keys.iter().find_map(|k| k.strip_prefix(key)?.parse::<f64>().ok()).is_some_and(|v| v == 6_378_137.0);
        let sphere = keys.contains(&"+proj=merc") && (axis("+R=") || (axis("+a=") && axis("+b=")));
        !sphere && self.datum_unshifted()
    }

    /// Whether the system's datum has no shift to WGS 84, as [`Crs::lacks_datum_shift`] but for
    /// Web Mercator's sphere too: what reading a WKT notes. For a system read from WKT, terrio-crs's
    /// operations are sampled at a spread of places, once a run per WKT ([`Crs::sampled_unshifted`]).
    pub(crate) fn datum_unshifted(self) -> bool {
        let Some(def) = self.custom_def().filter(|_| !self.is_planetary()) else { return false };
        let keys: Vec<&str> = def.proj4.split_whitespace().collect();
        let has = |prefix: &str| keys.iter().any(|k| k.starts_with(prefix));
        // Read from WKT, by the transformations terrio-crs uses (its PROJ string may leave out a
        // shift it applies): only ballparks, around its central meridian. A datum's
        // transformations hold in its area of use only, which isn't known here, so a spread of
        // places stands in for it.
        if let Some(wkt) = def.wkt.as_deref() {
            // Sampled once a run per WKT (thousands of places for a geographic system): reading
            // and every export ask. Bounded as the custom table is.
            static SAID: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> = std::sync::OnceLock::new();
            let said = SAID.get_or_init(Default::default);
            if let Some(unshifted) = said.lock().unwrap_or_else(|e| e.into_inner()).get(wkt) {
                return *unshifted;
            }
            let unshifted = self.sampled_unshifted(wkt);
            let mut said = said.lock().unwrap_or_else(|e| e.into_inner());
            if said.len() < MAX_CUSTOM {
                said.insert(wkt.to_string(), unshifted);
            }
            return unshifted;
        }
        let shifted = has("+towgs84=") || has("+datum=") || has("+nadgrids=");
        let wgs84 = keys.contains(&"+ellps=WGS84") || keys.contains(&"+ellps=GRS80");
        let ellipsoid = has("+ellps=") || has("+a=") || has("+R=");
        ellipsoid && !wgs84 && !shifted
    }

    /// [`Crs::datum_unshifted`] for a system read from `wkt`: no operation but ballparks at a spread
    /// of places.
    fn sampled_unshifted(self, wkt: &str) -> bool {
        // No operation at all is a datum taken for WGS 84 (by its ellipsoid), which only one the
        // file names as WGS 84 (or one read as it) is.
        if super::wkt::names_wgs84_datum(wkt) {
            return false;
        }
        let Ok(t) = self.transformer() else { return false };
        // Around its central meridian, or all round for a system without one (geographic).
        let lons: Vec<f64> = match t.params().filter(|_| !t.is_geographic()) {
            Some(p) => [-10.0, -5.0, 0.0, 5.0, 10.0].map(|d| p.lon_0 + p.prime_meridian + d).to_vec(),
            None => (-180..180).map(f64::from).collect(),
        };
        // Every degree for a geographic system: a datum's one transformation may cover an
        // island only (Monte Mario (Rome)'s, Sardinia, 8-10°E); every 5° of latitude around a
        // central meridian.
        let step = if t.is_geographic() { 1 } else { 5 };
        // And the centre of the datum's area, by its EPSG code or Esri name: a transformation may
        // cover a place smaller than the spacing (Grenada 1953's, an island of 0.3°).
        let places: Vec<Coord> = super::wkt::datum_area_centre(wkt)
            .into_iter()
            .chain((-90 / step..=90 / step).flat_map(|i| lons.iter().map(move |&lon| Coord::new(lon, f64::from(i * step)))))
            .collect();
        places.into_iter().all(|at| !t.shift_at(at))
    }

    /// Whether the system is another body's than the Earth's: a custom one whose ellipsoid's
    /// semi-major axis isn't the Earth's ([`super::planetary`]). Its longitude/latitude are read as
    /// they are, on the Earth.
    pub fn is_planetary(self) -> bool {
        self.planetary_semi_major().is_some()
    }

    /// The semi-major axis, in metres, of another body's system ([`Crs::is_planetary`]), which
    /// tells the bodies apart; `None` for the Earth's.
    pub fn planetary_semi_major(self) -> Option<f64> {
        self.custom_def().and_then(|d| super::planetary::proj4_axes(&d.proj4)).map(|(a, _)| a).filter(|a| super::planetary::off_earth(*a))
    }

    /// A transformer between this system and WGS 84 longitude/latitude, built once a run per
    /// system and shared ([`Transformer`] is cheap to clone).
    pub fn transformer(self) -> Result<Transformer, String> {
        match self {
            Crs::LonLat => Ok(Transformer { kind: Kind::LonLat }),
            Crs::WebMercator => Ok(Transformer { kind: Kind::Merc }),
            // From terrio-crs's EPSG catalog, as a WKT naming the same system is read: both put a
            // place at the same point, where PROJ 9.8.1 does. A code it can't build goes by its
            // bundled PROJ definition.
            Crs::Epsg(c) => match epsg_proj4(c) {
                Some(def) => {
                    wait_for_grids(GridsOf::System(self));
                    note_used(def, || format!("EPSG:{c}"));
                    let t = match epsg_transformer(c) {
                        Some(t) => t,
                        None => transformer_of(def).map_err(|e| format!("EPSG:{c}: {e}"))?,
                    };
                    probe_grid(def, || area_centre(c));
                    Ok(t)
                }
                None => Err(format!("unknown coordinate system EPSG:{c}")),
            },
            _ => {
                let def = self.custom_def().ok_or("an unknown custom coordinate system")?;
                wait_for_grids(GridsOf::System(self));
                note_used(&def.grid_key(), || def.label.clone());
                def.transformer().map_err(|e| format!("{}: {e}", def.label))
            }
        }
    }
}

impl CustomCrs {
    /// What its transformer's notes are kept under ([`needs_a_missing_grid`]): its WKT's key when
    /// it has one ([`transformer_of_wkt`]), else its PROJ definition.
    pub(crate) fn grid_key(&self) -> String {
        match &self.wkt {
            Some(wkt) => format!("WKT {wkt}"),
            None => self.proj4.clone(),
        }
    }

    /// Whether its PROJ definition, which older releases read, doesn't place points as its WKT
    /// does, within a centimetre on the ground, at the centre of its geographic system's area of use
    /// (else its own centre) and a few degrees around it: angles in grads, axes or a datum
    /// transformation the definition can't hold, or no definition. Worked out once a run per
    /// system.
    pub fn wkt_needed(&self) -> bool {
        let Some(wkt) = self.wkt.as_deref() else { return false };
        static NEEDED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<(String, String), bool>>> =
            std::sync::OnceLock::new();
        let needed = NEEDED.get_or_init(Default::default);
        let key = (self.proj4.clone(), wkt.to_string());
        if let Some(n) = needed.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return *n;
        }
        let n = !places_as_wkt(&self.proj4, wkt);
        let mut needed = needed.lock().unwrap_or_else(|e| e.into_inner());
        if needed.len() < MAX_CUSTOM {
            needed.insert(key, n);
        }
        n
    }

    /// Its transformer: by its WKT when it has one, else by its PROJ definition.
    fn transformer(&self) -> Result<Transformer, String> {
        match &self.wkt {
            Some(wkt) => transformer_of_wkt(wkt),
            None => transformer_of(&self.proj4),
        }
    }
}

/// Whether PROJ definition `proj4` places points as `wkt` does ([`CustomCrs::wkt_needed`]).
pub(crate) fn places_as_wkt(proj4: &str, wkt: &str) -> bool {
    if proj4.is_empty() {
        return false;
    }
    let (Ok(by_def), Ok(by_wkt)) = (transformer_of(proj4), transformer_of_wkt(wkt)) else { return false };
    // Where its data are: the centre of its geographic system's area of use (a datum's
    // transformations are chosen by place), else the projection's centre.
    let area = terrio_crs::SpatialRef::from_wkt(wkt).geographic_code().and_then(|c| u16::try_from(c).ok()).and_then(area_centre);
    let centre = area.unwrap_or_else(|| by_wkt.params().map_or(Coord::new(0.0, 0.0), |p| Coord::new(p.lon_0 + p.prime_meridian, p.lat_0)));
    let mut compared = false;
    for (dx, dy) in [(0.0, 0.0), (2.0, 2.0), (-2.0, -2.0), (3.0, -1.0), (-1.0, 3.0)] {
        let at = Coord::new(centre.x + dx, (centre.y + dy).clamp(-85.0, 85.0));
        // The place the WKT's coordinates of `at` are, by the definition: within a centimetre on
        // the ground, whatever the coordinates' unit.
        match (by_def.from_lonlat(at), by_wkt.from_lonlat(at)) {
            (Some(_), Some(native)) => {
                let Some(back) = by_def.to_lonlat(native) else { return false };
                let dlon = (back.x - at.x + 540.0).rem_euclid(360.0) - 180.0;
                let metres = (dlon * at.y.to_radians().cos()).hypot(back.y - at.y) * 111_195.0;
                if metres > 0.01 {
                    return false;
                }
                compared = true;
            }
            (None, None) => {}
            _ => return false,
        }
    }
    compared
}

/// [`transformer_of`] for a system terrio-crs reads from WKT (`SpatialRef::from_wkt`), which
/// applies its axes, angle unit and datum transformation as PROJ 9.8.1 does.
pub fn transformer_of_wkt(wkt: &str) -> Result<Transformer, String> {
    // Kept beside the PROJ definitions, under a key no definition has.
    let key = format!("WKT {wkt}");
    if let Some(t) = built().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return t.clone();
    }
    let flushing = FLUSHING.try_with(|l| l.replace(Some(key.clone()))).ok().flatten();
    let generation = grid_generation();
    let t = keep_built(key, build_wkt(wkt), generation);
    let _ = FLUSHING.try_with(|l| l.replace(flushing));
    t
}

/// [`transformer_of_wkt`]'s transformer, built.
fn build_wkt(wkt: &str) -> Result<Transformer, String> {
    build_sr(&terrio_crs::SpatialRef::from_wkt(wkt))
}

/// The transformer of an EPSG code, from terrio-crs's EPSG catalog, which transforms the code's
/// system as PROJ 9.9.0 does (its datum transformation included); `None` for a code terrio-crs
/// doesn't hold or can't build, which is transformed by its bundled PROJ definition instead.
fn epsg_transformer(code: u16) -> Option<Transformer> {
    let key = format!("EPSG {code}");
    if let Some(t) = built().lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return t.clone().ok();
    }
    let flushing = FLUSHING.try_with(|l| l.replace(Some(key.clone()))).ok().flatten();
    let generation = grid_generation();
    // A code the catalog gives another's definition goes by its bundled one ([`catalog_has_own`]).
    // A compound code (a horizontal system and heights, EPSG:5832 DB_REF / 3-degree Gauss-Krüger
    // zone 3 + DHHN2016) is the catalog's too: its horizontal part, with its datum transformation
    // as PROJ 9.9.0 picks it, where its bundled definition named no shift (175 m off there).
    // No WKT of its own in the catalog: a compound code (or one the catalog doesn't compute, which
    // `from_epsg` refuses below as before).
    let compound = terrio_crs::epsg_wkt(u32::from(code)).is_none();
    let t = if catalog_has_own(code) || compound {
        if let Some(def) = epsg_proj4(code) {
            let mut codes = epsg_built().lock().unwrap_or_else(|e| e.into_inner());
            if codes.len() < MAX_BUILT && !codes.iter().any(|(c, _)| *c == code) {
                codes.push((code, def));
            }
        }
        terrio_crs::SpatialRef::from_epsg(u32::from(code)).map_err(|e| e.to_string()).and_then(|sr| build_sr(&sr))
    } else {
        Err(format!("EPSG:{code} is given another code's definition"))
    };
    let t = keep_built(key, t, generation);
    let _ = FLUSHING.try_with(|l| l.replace(flushing));
    t.ok()
}

/// Whether terrio-crs's EPSG catalog gives `code` its own definition, a WKT whose root names
/// `code`. A deprecated code keeps its own, as PROJ 9.9.0 does, since data written with it holds
/// its coordinates.
pub(crate) fn catalog_has_own(code: u16) -> bool {
    terrio_crs::epsg_wkt(u32::from(code)).is_some_and(|w| root_epsg_code(w) == Some(u32::from(code)))
}

/// The EPSG code a WKT's root names: its last `AUTHORITY["EPSG","…"]` (WKT 1) or `ID["EPSG",…]`
/// (WKT 2), which close the root after its parts' own.
fn root_epsg_code(wkt: &str) -> Option<u32> {
    let at = [r#"AUTHORITY["EPSG",""#, r#"ID["EPSG","#].iter().filter_map(|k| wkt.rfind(k).map(|i| i + k.len())).max()?;
    wkt[at..].trim_start_matches('"').split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// A transformer for a system terrio-crs holds as a `SpatialRef`.
fn build_sr(sr: &terrio_crs::SpatialRef) -> Result<Transformer, String> {
    // Another body's system goes to its own longitude/latitude, read as they are on the Earth
    // ([`Crs::is_planetary`]): by its PROJ string, as before, since terrio-crs finds the
    // geographic system of a WKT 2 one only through a WKT 1 `GEOGCS`.
    if !sr.is_earth() {
        let def = sr.to_proj4().ok_or("another body's coordinate system without a PROJ definition")?;
        return transformer_of(&def.replace(" +type=crs", ""));
    }
    // The order and direction of its axes as terrio-crs applies them, which its PROJ string
    // states (`+axis=wsu`); a geographic system's are longitude then latitude.
    let def = sr.to_proj4().unwrap_or_default();
    let axis = def.split_whitespace().find_map(|t| t.strip_prefix("+axis=")).unwrap_or("enu");
    let geographic = sr.is_geographic();
    let lat_first = !geographic && axis.starts_with(['n', 's']);
    let west = axis.chars().take(2).any(|c| c == 'w');
    let south = !geographic && axis.chars().take(2).any(|c| c == 's');
    let t = Transformer::of(sr, geographic, (lat_first, west, south))?;
    let centre = t.params().map_or(Coord::new(0.0, 0.0), |p| Coord::new(p.lon_0 + p.prime_meridian, p.lat_0.clamp(-89.0, 89.0)));
    if !t.is_geographic() && t.from_lonlat_unchecked(centre).is_none() {
        return Err("it can't place its own centre".into());
    }
    Ok(t)
}

/// The geographic system of `sr` with the grid its PROJ string names (`+nadgrids`, from GDAL's
/// `PROJ4` extension, which PROJ reads in place of the WKT): its WKT's geographic system lacks it.
/// `None` without a grid.
fn own_geographic(sr: &terrio_crs::SpatialRef) -> Option<terrio_crs::SpatialRef> {
    let def = sr.to_proj4()?;
    if !def.split_whitespace().any(|t| t.starts_with("+nadgrids=")) {
        return None;
    }
    const DATUM: [&str; 9] = ["+datum=", "+ellps=", "+a=", "+b=", "+rf=", "+R=", "+towgs84=", "+nadgrids=", "+pm="];
    let datum: Vec<&str> = def.split_whitespace().filter(|t| DATUM.iter().any(|k| t.starts_with(k))).collect();
    terrio_crs::SpatialRef::from_proj4(&format!("+proj=longlat {} +no_defs +type=crs", datum.join(" "))).ok()
}

/// The transformers built this run, by definition, kept for the run: building one reads
/// terrio-crs's tables, and [`grid_note`] asks each what it had to do.
type Built = std::collections::HashMap<String, Result<Transformer, String>>;

fn built() -> &'static std::sync::Mutex<Built> {
    static BUILT: std::sync::OnceLock<std::sync::Mutex<Built>> = std::sync::OnceLock::new();
    BUILT.get_or_init(Default::default)
}

/// The most definitions [`built`] keeps: they come from files and commands, so bounded as the
/// custom table is. One past it is built each time it is asked for.
const MAX_BUILT: usize = 4 * MAX_CUSTOM;

/// The direct operation between two systems terrio-crs computes (`cs2cs A B`, but never a
/// ballpark): a transformation between their datums where the registry has one (DHDN to ETRS89,
/// SAD69 to SIRGAS 2000), none between two systems on the same datum, and through a common datum
/// otherwise. Where its best transformation needs a grid RustGIS doesn't have, terrio-crs takes the
/// next, as PROJ does, and the pair is noted for the missing-grid notice, its grids for download
/// ([`missing_grids_between`]). Where the datums have no transformation between them but a
/// ballpark, it has none, and the conversion goes through WGS 84.
pub(crate) struct DirectOperation {
    op: terrio_crs::Transformer,
    /// A geographic target's turn in its unit (360° in grads is 400) and whether its longitude is
    /// the second coordinate: a longitude is put on the same turn as the one through WGS 84.
    turn: Option<(f64, bool)>,
    /// How far, in the target's units, its coordinates may be from those through WGS 84: 5 km
    /// (0.05°), more than any two transformations between the same datums differ by, and far less
    /// than a world projection's other edge, where a place on one edge may come back.
    slack: f64,
    /// The grids its best transformation needs at the middle of the source's area of use that
    /// RustGIS doesn't have, those it can download.
    missing: Vec<MissingGrid>,
    /// Its pair's key ([`pair_key`]), what its notes are kept under.
    key: String,
}

// What it noted and the session hasn't asked for yet is the pair's as it goes (built again when
// grids arrive), not a system's RustGIS can't name ([`record_warning`]).
impl Drop for DirectOperation {
    fn drop(&mut self) {
        self.flush();
    }
}

impl std::fmt::Debug for DirectOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DirectOperation")
    }
}

impl DirectOperation {
    /// Notes what it had to do (a missing grid) under its pair's key ([`record_warning`]).
    fn flush(&self) {
        let before = FLUSHING.try_with(|l| l.replace(Some(self.key.clone()))).ok().flatten();
        self.op.flush_warnings();
        let _ = FLUSHING.try_with(|l| l.replace(before));
    }

    /// Coordinates `c` of the source system in the target's, `via` being the place through WGS 84
    /// (which judges whether it is one: a place past a pole or a world projection's edge, or one
    /// the target folds, and on which edge or side of the antimeridian it lies). `None` where the
    /// direct operation can't convert it, or puts it elsewhere than a datum shift from `via`.
    fn point(&self, c: Coord, via: Coord) -> Option<Coord> {
        let (x, y) = self.op.transform(c.x, c.y).ok()?;
        let mut out = Coord::new(x, y);
        if !out.is_finite() {
            return None;
        }
        if let Some((turn, lat_first)) = self.turn {
            let (lon, at) = if lat_first { (&mut out.y, via.y) } else { (&mut out.x, via.x) };
            *lon += turn * ((at - *lon) / turn).round();
        }
        ((out.x - via.x).abs() <= self.slack && (out.y - via.y).abs() <= self.slack).then_some(out)
    }
}

/// The direct operation between two systems, for a [`super::Reprojection`] to hold: built again
/// when grids arrive while it is held (a long export), as each system's transformer is.
pub(crate) struct DirectPair {
    source: Crs,
    target: Crs,
    from: Transformer,
    to: Transformer,
    /// The operation as first built, for the grids of generation `built_for`: what every place
    /// takes, without a lock, until grids arrive.
    first: std::sync::Arc<DirectOperation>,
    built_for: u64,
    /// Built again for the grids there are since, with their generation.
    again: std::sync::RwLock<Option<(u64, Option<std::sync::Arc<DirectOperation>>)>>,
}

impl std::fmt::Debug for DirectPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DirectPair({:?} to {:?})", self.source, self.target)
    }
}

impl DirectPair {
    /// The direct operation from `source` to `target`, whose transformers are `from` and `to`;
    /// `None` when either isn't computed by terrio-crs (longitude/latitude, Web Mercator), is
    /// another body's, or terrio-crs has no such operation. Notes the pair for the missing-grid
    /// notice ([`note_used`]) and, on a read that may, waits for its grids first
    /// ([`waiting_for_grids`]), as asking for a system's transformer does for the system.
    pub(crate) fn new(source: Crs, target: Crs, from: &Transformer, to: &Transformer) -> Option<DirectPair> {
        let key = pair_key(from, to)?;
        wait_for_grids(GridsOf::Pair(source, target));
        let built_for = grid_generation();
        let first = direct_operation(source, target, from, to)?;
        note_used(&key, || format!("{} to {}", source.label(), target.label()));
        let again = std::sync::RwLock::new(None);
        Some(DirectPair { source, target, from: from.clone(), to: to.clone(), first, built_for, again })
    }

    /// [`DirectOperation::point`], by the operation built for the grids there are now: as first
    /// built until grids arrive (one atomic load), then as built again (a read lock).
    pub(crate) fn point(&self, c: Coord, via: Coord) -> Option<Coord> {
        let generation = grid_generation();
        if generation == self.built_for {
            return self.first.point(c, via);
        }
        let kept =
            self.again.read().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(g, _)| *g == generation).map(|(_, op)| op.clone());
        let op = match kept {
            Some(op) => op,
            None => {
                let op = direct_operation(self.source, self.target, &self.from, &self.to);
                *self.again.write().unwrap_or_else(|e| e.into_inner()) = Some((generation, op.clone()));
                op
            }
        };
        op?.point(c, via)
    }
}

/// What the direct operation between two systems is noted under for the missing-grid notice
/// ([`needs_a_missing_grid`]): both systems' keys, which no system's key is. `None` for a system
/// terrio-crs doesn't compute.
fn pair_key(from: &Transformer, to: &Transformer) -> Option<String> {
    let (Kind::Terrio(a), Kind::Terrio(b)) = (&from.kind, &to.kind) else { return None };
    let (a, b) = (a.key.as_deref()?, b.key.as_deref()?);
    // The first key's length, so no two pairs' keys run together into the same text (a WKT's key
    // may hold any character).
    Some(format!("PAIR {} {a}{b}", a.len()))
}

/// A direct operation built ([`direct_operation`]).
struct DirectBuilt {
    /// The grid generation it was built for.
    generation: u64,
    /// Its source and target systems.
    systems: (Crs, Crs),
    /// `None` for a pair without one.
    op: Option<std::sync::Arc<DirectOperation>>,
}

/// The direct operations built, by their pair's key ([`pair_key`]).
type Direct = std::collections::HashMap<String, DirectBuilt>;

fn direct() -> &'static std::sync::Mutex<Direct> {
    static DIRECT: std::sync::OnceLock<std::sync::Mutex<Direct>> = std::sync::OnceLock::new();
    DIRECT.get_or_init(Default::default)
}

/// The direct operation from `source` to `target` (their transformers `from` and `to`), built once
/// a run per pair and again when grids arrive. Building it asks which grids its best transformation
/// needs at the middle of the source's area of use (else, where it has none or can't hold it, the
/// target's), as a system's own are asked where its data usually are ([`probe_grid`]): one
/// missing notes the pair for the missing-grid notice. A place elsewhere whose best
/// transformation's grid is missing notes it too, once the session asks
/// ([`needs_a_missing_grid`]).
fn direct_operation(source: Crs, target: Crs, from: &Transformer, to: &Transformer) -> Option<std::sync::Arc<DirectOperation>> {
    let key = pair_key(from, to)?;
    let (Kind::Terrio(a), Kind::Terrio(b)) = (&from.kind, &to.kind) else { return None };
    let generation = grid_generation();
    if let Some(d) = direct().lock().unwrap_or_else(|e| e.into_inner()).get(&key)
        && d.generation == generation
    {
        return d.op.clone();
    }
    let (f, t) = (a.peek(), b.peek());
    let op = if f.sr.is_earth() && t.sr.is_earth() { build_direct(&f.sr, &t, source, target, from, &key) } else { None };
    let mut kept = direct().lock().unwrap_or_else(|e| e.into_inner());
    if kept.len() < MAX_BUILT || kept.contains_key(&key) {
        kept.insert(key, DirectBuilt { generation, systems: (source, target), op: op.clone() });
    }
    op
}

/// [`direct_operation`]'s operation, built: from `from_sr` to the system `to` computes.
fn build_direct(
    from_sr: &terrio_crs::SpatialRef,
    to: &Terrio,
    source: Crs,
    target: Crs,
    from: &Transformer,
    key: &str,
) -> Option<std::sync::Arc<DirectOperation>> {
    let options = terrio_crs::TransformOptions { allow_ballpark: Some(false), ..Default::default() };
    // Its missing grids are noted where its data usually are (below), and at the places it takes
    // as the session asks ([`needs_a_missing_grid`] flushes them under the pair's key).
    let op = terrio_crs::Transformer::with_options(from_sr, &to.sr, &options).ok()?.with_warning_handler(record_warning);
    let turn = to.geographic.then(|| (360.0 / to.unit, to.lat_first));
    let slack = match to.params.as_ref().filter(|_| !to.geographic) {
        Some(p) => 5_000.0 / if p.to_meter > 0.0 { p.to_meter } else { 1.0 },
        None if to.geographic => 0.05 / to.unit,
        None => 5_000.0,
    };
    let at = [source, target].into_iter().find_map(|crs| area_of_use(crs).and_then(|c| from.from_lonlat(c)));
    let grids = at.and_then(|c| op.best_operation_grids_at(c.x, c.y, 0.0).ok()).unwrap_or_default();
    if grids.iter().any(|g| !g.available && !g.optional) {
        let mut needs = needs_grid().lock().unwrap_or_else(|e| e.into_inner());
        if !needs.iter().any(|d| d == key) {
            needs.push(key.to_string());
        }
    }
    let missing = grids.into_iter().filter(downloadable).filter_map(|g| Some(MissingGrid { url: g.url?, name: g.name })).collect();
    Some(std::sync::Arc::new(DirectOperation { op, turn, slack, missing, key: key.to_string() }))
}

/// The datum grids the direct operation from `source` to `target` needs where the source's data
/// usually are that RustGIS doesn't have and can download, as [`missing_grids`] gives a system's:
/// none where either is longitude/latitude or Web Mercator (that is the other's own), or there is
/// no direct operation.
pub fn missing_grids_between(source: Crs, target: Crs) -> Vec<MissingGrid> {
    let (Ok(from), Ok(to)) = (source.transformer(), target.transformer()) else { return Vec::new() };
    direct_operation(source, target, &from, &to).map(|op| op.missing.clone()).unwrap_or_default()
}

/// The two systems whose direct operation is noted under `key` ([`DirectPair::new`] notes it), for the
/// grids it needs; `None` for a key that isn't a pair's, or one not kept.
pub fn pair_systems(key: &str) -> Option<(Crs, Crs)> {
    direct().lock().unwrap_or_else(|e| e.into_inner()).get(key).map(|d| d.systems)
}

/// Transformer `t`, built for `key` while the grids were those of `generation`
/// ([`grid_generation`]), kept in [`built`] and shared: the first one built is kept, as another
/// thread may have built it meanwhile. One built before a grid arrived isn't kept, as it was
/// made without it.
fn keep_built(key: String, t: Result<Transformer, String>, generation: u64) -> Result<Transformer, String> {
    let mut map = built().lock().unwrap_or_else(|e| e.into_inner());
    let t = t.map(|t| t.keyed(&key, generation));
    if grid_generation() != generation {
        return t;
    }
    if map.len() < MAX_BUILT || map.contains_key(&key) { map.entry(key).or_insert(t).clone() } else { t }
}

/// How many times datum grids have become available this run ([`grids_changed`]). A transformer
/// keeps the operation it chose when it was built, as PROJ's does, so whatever keeps one (a
/// map's projection, a tile warp) builds it again when this changes.
pub fn grid_generation() -> u64 {
    GRID_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

static GRID_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The folders RustGIS reads datum grids from, in order ([`set_grid_folders`]): none until the app
/// sets them, so a library caller or a test lands a place where it does on every computer.
fn grid_folders() -> &'static std::sync::RwLock<Vec<std::path::PathBuf>> {
    static FOLDERS: std::sync::RwLock<Vec<std::path::PathBuf>> = std::sync::RwLock::new(Vec::new());
    &FOLDERS
}

/// The folder RustGIS keeps the datum grids it downloads in: the first of its grid folders
/// ([`use_grid_folders`]). `None` before the app sets them, and in the browser, which registers
/// the grids it fetches ([`register_grid`]).
pub fn grid_dir() -> Option<std::path::PathBuf> {
    if !KEEPS_FIRST.load(std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    grid_folders().read().unwrap_or_else(|e| e.into_inner()).first().cloned()
}

/// Whether the first grid folder is RustGIS's own, where downloads are kept: not when there is no
/// app data folder, so a download never lands in PROJ's.
static KEEPS_FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Reads datum grids from RustGIS's own folder (`datum-grids` in its app data), where it keeps the
/// ones it downloads, then from those PROJ and terrio-crs use on this computer (`TERRIO_GRID_DIR`,
/// `PROJ_DATA`, `PROJ_LIB`, PROJ's user folder, terrio's download cache), so grids installed for
/// PROJ count. Called by the desktop app, the command line and Python as they start; nothing in
/// the browser, which has no folders.
pub fn use_grid_folders() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let own = crate::dirs::app_data_dir().map(|d| d.join("datum-grids"));
        let mut dirs: Vec<std::path::PathBuf> = own.iter().cloned().collect();
        for d in terrio_crs::default_grid_search_paths() {
            if !dirs.contains(&d) {
                dirs.push(d);
            }
        }
        set_folders(dirs, own.is_some());
    }
}

/// Reads datum grids from `dirs`, in order, the first being where downloaded ones are kept
/// ([`grid_dir`]), and builds transformers again with them. A grid named by a path in a definition
/// (`+nadgrids=/data/a.gsb`) stays refused, so a file read can't make RustGIS read others.
pub fn set_grid_folders(dirs: Vec<std::path::PathBuf>) {
    set_folders(dirs, true);
}

/// [`set_grid_folders`], the first of `dirs` being where downloads are kept only if `keeps_first`.
fn set_folders(dirs: Vec<std::path::PathBuf>, keeps_first: bool) {
    KEEPS_FIRST.store(keeps_first, std::sync::atomic::Ordering::SeqCst);
    // The first setting up done (on whatever thread started it), so it can't put the folders
    // read before these after them.
    grids_set_up();
    *grid_folders().write().unwrap_or_else(|e| e.into_inner()) = dirs.clone();
    terrio_crs::set_network_enabled(Some(false));
    terrio_crs::set_grid_search_paths(Some(dirs));
    grids_changed();
}

/// Points terrio-crs at the grid folders (none unless the app set some), once a run, with its own
/// network off: RustGIS fetches grids itself.
fn grids_set_up() {
    static SET_UP: std::sync::Once = std::sync::Once::new();
    SET_UP.call_once(|| {
        let dirs = grid_folders().read().unwrap_or_else(|e| e.into_inner()).clone();
        terrio_crs::set_grid_search_paths(Some(dirs));
        terrio_crs::set_network_enabled(Some(false));
    });
}

/// Says that datum grids have become available (a file put in [`grid_dir`], a grid registered):
/// transformers are built again, with them, and the notes that a grid was missing are asked again.
pub fn grids_changed() {
    grids_set_up();
    // terrio-crs first, so whatever is built from here on finds the grids; then the generation and
    // the transformers kept, together, so none built before is kept after ([`keep_built`]) and none
    // kept after was built before (one [`Live::now`] builds again finds only those).
    terrio_crs::reset_grid_cache();
    {
        let mut built = built().lock().unwrap_or_else(|e| e.into_inner());
        GRID_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        built.clear();
    }
    epsg_built().lock().unwrap_or_else(|e| e.into_inner()).clear();
    needs_grid().lock().unwrap_or_else(|e| e.into_inner()).clear();
    probed().lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// A datum grid a system needs that RustGIS doesn't have and can download: its name on PROJ's
/// grid server (`us_noaa_conus.tif`, the name it is kept and registered under) and its address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MissingGrid {
    pub name: String,
    pub url: String,
}

/// The datum grids `crs`'s most accurate transformation needs where its data usually are (the
/// middle of its area of use, [`area_of_use`]) that RustGIS doesn't have and can download (PROJ's
/// grid server gives the file itself), as terrio-crs lists them (PROJ's
/// `proj_coordoperation_get_grid_used`). Once those have arrived, ask again: the next operation's
/// grids come up where the first one doesn't reach. None for a system without a datum
/// transformation, or without an area of use to ask at.
pub fn missing_grids(crs: Crs) -> Vec<MissingGrid> {
    let Some(at) = area_of_use(crs) else { return Vec::new() };
    crs.transformer().map(|t| t.missing_grids_at(at)).unwrap_or_default()
}

/// Whether RustGIS downloads `grid`: one it doesn't have and needs (not optional), the file itself
/// on PROJ's grid server (the only server it contacts for them) under an open licence. A
/// definition read from a file can name any grid PROJ knows: this keeps what it can make RustGIS
/// fetch to PROJ's own, freely licensed grids. Only its GeoTIFF grids: the server's other files
/// (deformation models, point lists) aren't grids RustGIS downloads.
fn downloadable(grid: &terrio_crs::GridUse) -> bool {
    !grid.available
        && grid.name.ends_with(".tif")
        && !grid.optional
        && grid.direct_download
        && grid.open_license
        && grid.url.as_deref().is_some_and(|u| u.strip_prefix("https://cdn.proj.org/").is_some_and(|n| n == grid.name))
}

/// What a read waits for the datum grids of ([`wait_for_grids`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GridsOf {
    /// A system's own, to and from WGS 84 ([`missing_grids`]).
    System(Crs),
    /// The direct operation's from one system to another ([`missing_grids_between`]).
    Pair(Crs, Crs),
}

/// What waits for the datum grids a system or a pair of systems needs ([`set_grid_waiter`]): what
/// it waits for, and the read it waits for (a number of this thread's own), so a read's systems
/// share one wait.
type GridWaiter = fn(GridsOf, u64);

fn grid_waiter() -> &'static std::sync::RwLock<Option<GridWaiter>> {
    static WAITER: std::sync::RwLock<Option<GridWaiter>> = std::sync::RwLock::new(None);
    &WAITER
}

thread_local! {
    /// This thread may wait for datum grids before it transforms ([`waiting_for_grids`]).
    static MAY_WAIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The systems and pairs this read has already waited for: each is asked about once.
    static WAITED: std::cell::RefCell<Vec<GridsOf>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The read on this thread ([`waiting_for_grids`]), counted.
    static READ: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Puts [`MAY_WAIT`] back as it was when dropped, a panic included.
struct MayWait(bool);

impl MayWait {
    fn set(to: bool) -> MayWait {
        MayWait(MAY_WAIT.try_with(|m| m.replace(to)).unwrap_or(false))
    }
}

impl Drop for MayWait {
    fn drop(&mut self) {
        let _ = MAY_WAIT.try_with(|m| m.set(self.0));
    }
}

/// Installs what gets the datum grids a system needs before a file in it is read (or a conversion
/// between two systems before it is made) on a thread that may wait ([`waiting_for_grids`]): the app's download, when its user turned it on. `None` takes
/// it away.
pub fn set_grid_waiter(waiter: Option<GridWaiter>) {
    *grid_waiter().write().unwrap_or_else(|e| e.into_inner()) = waiter;
}

/// Runs `f`, a read on a thread of its own (never the window's), so that a system it transforms,
/// or a pair of systems it converts between, first gets the datum grids it needs
/// ([`set_grid_waiter`]): its features then land where those grids put them, not where the next
/// transformation does.
pub fn waiting_for_grids<R>(f: impl FnOnce() -> R) -> R {
    let outer = !MAY_WAIT.try_with(std::cell::Cell::get).unwrap_or(false);
    if outer {
        let _ = WAITED.try_with(|w| w.borrow_mut().clear());
        let _ = READ.try_with(|r| r.set(r.get().wrapping_add(1)));
    }
    let _may = MayWait::set(true);
    f()
}

/// Runs `f` without waiting for datum grids, on a thread that otherwise may: for a transformer
/// asked for only to compare with, whose grids the read may not use.
pub(crate) fn not_waiting_for_grids<R>(f: impl FnOnce() -> R) -> R {
    let _may = MayWait::set(false);
    f()
}

/// Waits for the grids `of` needs, on a thread that may ([`waiting_for_grids`]), once a read.
fn wait_for_grids(of: GridsOf) {
    if !MAY_WAIT.try_with(std::cell::Cell::get).unwrap_or(false) {
        return;
    }
    let first = WAITED
        .try_with(|w| {
            let mut w = w.borrow_mut();
            let first = !w.contains(&of);
            if first {
                w.push(of);
            }
            first
        })
        .unwrap_or(false);
    if !first {
        return;
    }
    let Some(waiter) = *grid_waiter().read().unwrap_or_else(|e| e.into_inner()) else { return };
    // The waiter asks for transformers itself: not again from inside it.
    let _may = MayWait::set(false);
    waiter(of, READ.try_with(std::cell::Cell::get).unwrap_or(0));
}

/// The middle of the area of use of `crs`: an EPSG code's own, else that of the geographic system
/// a custom one is on (`None` when that has no EPSG code).
pub fn area_of_use(crs: Crs) -> Option<Coord> {
    match crs {
        Crs::Epsg(c) => area_centre(c),
        Crs::Custom(_) => {
            let d = crs.custom_def()?;
            let sr = match &d.wkt {
                Some(w) => terrio_crs::SpatialRef::from_wkt(w),
                None => terrio_crs::SpatialRef::from_proj4(&format!("{} +type=crs", d.proj4)).ok()?,
            };
            sr.geographic_code().and_then(|c| u16::try_from(c).ok()).and_then(area_centre)
        }
        Crs::LonLat | Crs::WebMercator => None,
    }
}

/// Makes a datum grid available from its bytes, under its name (`us_noaa_conus.tif`): how the
/// browser, without files, uses one it fetched.
pub fn register_grid(name: &str, bytes: Vec<u8>) {
    grids_set_up();
    terrio_crs::register_grid(name, bytes);
    grids_changed();
}

/// A transformer between the system a PROJ definition describes and WGS 84 longitude/latitude,
/// computed by terrio-crs. Winkel Tripel's standard parallel (`+lat_1`) must be a latitude short
/// of a pole, where the projection has no width. Natural Earth, Winkel Tripel and Eckert IV have no scale factor:
/// one other than 1, which PROJ ignores, is refused.
pub fn transformer_of(def: &str) -> Result<Transformer, String> {
    if let Some(t) = built().lock().unwrap_or_else(|e| e.into_inner()).get(def) {
        return t.clone();
    }
    // What a transformer built here says as it is dropped (one that fails its checks, or one
    // another thread built first) is this definition's ([`record_warning`]).
    let flushing = FLUSHING.try_with(|l| l.replace(Some(def.to_string()))).ok().flatten();
    let generation = grid_generation();
    let degrees = in_degrees(def);
    let param = |key: &str| degrees.split_whitespace().find_map(|t| t.strip_prefix(key));
    let world = param("+proj=").is_some_and(|m| WORLD_METHODS.contains(&m));
    let scale = param("+k=").or_else(|| param("+k_0="));
    let t = if param("+proj=") == Some("wintri")
        && let Some(v) = param("+lat_1=")
        && !parse_angle(v).is_some_and(|v| v.is_finite() && v.abs() < 90.0)
    {
        Err(format!("+lat_1={v} isn't a latitude short of a pole"))
    } else if world && let Some(k) = scale.filter(|k| k.parse::<f64>().ok() != Some(1.0)) {
        // PROJ ignores it; refused, as it was before, rather than dropped without a word.
        Err(format!("a scale factor (+k={k}) other than 1 isn't supported in this projection"))
    } else {
        misread_parameter(def).and_then(|()| build(&degrees))
    };
    let t = keep_built(def.to_string(), t, generation);
    let _ = FLUSHING.try_with(|l| l.replace(flushing));
    t
}

/// A definition with its angles in radians (`+lat_ts=0.5r`, as PROJ reads them) in degrees,
/// which terrio-crs reads: radians it would take as 0.
fn in_degrees(def: &str) -> String {
    def.split_whitespace()
        .map(|token| {
            let t = token.trim_start_matches('+');
            let angle = t.split_once('=').filter(|(k, v)| angular(k, v));
            let radians = angle.and_then(|(k, v)| {
                let d = v.strip_suffix(['r', 'R'])?.parse::<f64>().ok()?.to_degrees();
                d.is_finite().then(|| format!("+{k}={d}"))
            });
            // The degree marks PROJ reads besides `d`, which terrio-crs doesn't.
            let marked = angle.filter(|(_, v)| v.contains(['D', '°'])).map(|(k, v)| format!("+{k}={}", v.replace(['D', '°'], "d")));
            radians.or(marked).unwrap_or_else(|| token.to_string())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A definition's parameters as terrio-crs reads them: its ellipsoid's axes in metres, the prime
/// meridian (and other angles) in degrees, and metres per unit. Angles given in radians or with
/// the `D`/`°` marks are turned into degrees before terrio-crs reads them.
pub(crate) fn def_params(def: &str) -> Option<terrio_crs::ProjParams> {
    terrio_crs::SpatialRef::from_proj4(&in_degrees(def)).ok()?.projection_params()
}

/// The prime meridians PROJ 9.8.1 knows by name (`proj -lm`), as terrio-crs reads them.
const PRIME_MERIDIANS: [&str; 14] = [
    "greenwich",
    "lisbon",
    "paris",
    "bogota",
    "madrid",
    "rome",
    "bern",
    "jakarta",
    "ferro",
    "brussels",
    "stockholm",
    "athens",
    "oslo",
    "copenhagen",
];

/// The angle parameters of a PROJ definition: every one terrio-crs reads as an angle (Laborde's
/// azimuth `azi` among them), and the longitudes it reads otherwise.
const ANGLES: [&str; 12] = ["lat_0", "lon_0", "lat_1", "lat_2", "lat_ts", "lonc", "alpha", "gamma", "lon_1", "lon_2", "azi", "rot_xy"];

/// Whether `key=value` is an angle ([`ANGLES`]), a prime meridian given as one (`+pm=2D`, not
/// `+pm=paris`) included.
pub(crate) fn angular(key: &str, value: &str) -> bool {
    ANGLES.contains(&key) || (key == "pm" && value.starts_with(|c: char| c.is_ascii_digit() || "+-.".contains(c)))
}

/// An angle parameter's value in degrees, as PROJ reads it: degrees, degrees, minutes and
/// seconds ([`parse_angle`]) or radians (`0.5r`).
pub(crate) fn angle_degrees(value: &str) -> Option<f64> {
    parse_angle(value)
        .or_else(|| value.strip_suffix(['r', 'R']).and_then(|r| r.parse::<f64>().ok()).map(f64::to_degrees))
        .filter(|d| d.is_finite())
}

/// [`transformer_of`]'s transformer, built.
fn build(def: &str) -> Result<Transformer, String> {
    let sr = terrio_crs::SpatialRef::from_proj4(def).map_err(|e| e.to_string())?;
    // The order and direction of its axes, which terrio-crs applies (`+axis=neu`, `wnu`).
    let axis = def.split_whitespace().find_map(|t| t.strip_prefix("+axis=")).unwrap_or("enu");
    let lat_first = axis.starts_with(['n', 's']);
    let west = axis.chars().take(2).any(|c| c == 'w');
    let south = axis.chars().take(2).any(|c| c == 's');
    let t = Transformer::of(&sr, geographic_def(def), (lat_first, west, south))?;
    // A definition that can't place its own centre (a unit of 0 metres) is no system.
    let centre = t.params().map_or(Coord::new(0.0, 0.0), |p| Coord::new(p.lon_0 + p.prime_meridian, p.lat_0.clamp(-89.0, 89.0)));
    if !t.is_geographic() && t.from_lonlat_unchecked(centre).is_none() {
        return Err("it can't place its own centre".into());
    }
    Ok(t)
}

/// The numeric parameters of a PROJ definition: each must be a finite number, or degrees,
/// minutes and seconds (`30d30'N`), as PROJ reads them.
const NUMERIC_PARAMETERS: [&str; 28] = [
    "lat_0", "lon_0", "lat_1", "lat_2", "lat_ts", "lonc", "alpha", "gamma", "k", "k_0", "x_0", "y_0", "to_meter", "a", "b", "R", "rf", "f",
    "h", "zone", "lon_1", "lon_2", "azi", "rot_xy", "e", "es", "n", "h_0",
];

/// Refuses what terrio-crs would read otherwise than PROJ, so a place is never put somewhere
/// else silently, where PROJ refuses the definition: a numeric parameter that isn't a number
/// (`+lat_ts=abc`, `+x_0=500000d`, which terrio-crs would read as 0), an angle that isn't
/// degrees, degrees, minutes and seconds, or radians (turned into degrees, [`in_degrees`]), and
/// a datum shift that isn't 3 or 7 numbers (`+towgs84=100,bad,200,300`, which terrio-crs would
/// read as the 3 that are).
fn misread_parameter(def: &str) -> Result<(), String> {
    for token in def.split_whitespace() {
        let t = token.trim_start_matches('+');
        let Some((key, value)) = t.split_once('=') else { continue };
        if key == "towgs84" {
            let values: Vec<Option<f64>> =
                value.trim_matches('"').split(',').map(|v| v.trim().parse::<f64>().ok().filter(|v| v.is_finite())).collect();
            if !matches!(values.len(), 3 | 7) || values.contains(&None) {
                return Err(format!("+towgs84={value} isn't 3 or 7 numbers"));
            }
            continue;
        }
        let angle = angular(key, value);
        // A prime meridian by name: one PROJ knows, as it spells it (terrio-crs would take
        // another as Greenwich).
        if key == "pm" && !angle && !PRIME_MERIDIANS.contains(&value) {
            return Err(format!("+pm={value} isn't a prime meridian PROJ knows"));
        }
        if !angle && !NUMERIC_PARAMETERS.contains(&key) {
            continue;
        }
        let read = if angle { angle_degrees(value).is_some() } else { value.parse::<f64>().is_ok_and(f64::is_finite) };
        if !read {
            let what = if angle { "an angle" } else { "a number" };
            return Err(format!("+{key}={value} isn't {what}"));
        }
    }
    Ok(())
}

/// An angle as PROJ reads it: decimal degrees, or degrees, minutes and seconds
/// (`30d30'15"N`, the degree mark `d`, `D` or `°`), a hemisphere letter giving the sign over a
/// minus (`-52.5N` is 52.5°), as terrio-crs reads it too once its mark is `d` ([`in_degrees`]).
/// `None` for anything else (`30x`, `d`, `30d30d`).
fn parse_angle(value: &str) -> Option<f64> {
    let s = value.trim().replace(['D', '°'], "d");
    if let Ok(v) = s.parse::<f64>() {
        return Some(v);
    }
    let (body, sign) = match s.chars().last()? {
        'N' | 'n' | 'E' | 'e' => (s.get(..s.len() - 1)?, 1.0),
        'S' | 's' | 'W' | 'w' => (s.get(..s.len() - 1)?, -1.0),
        _ => (s.as_str(), 1.0),
    };
    let negative = body.len() == s.len() && body.starts_with('-');
    let mut rest = body.strip_prefix(['-', '+']).unwrap_or(body);
    let part = |v: &str| v.parse::<f64>().ok().filter(|v| *v >= 0.0);
    let (mut degrees, mut any, mut next) = (0.0, false, 1.0);
    for (mark, div) in [('d', 1.0), ('\'', 60.0), ('"', 3600.0)] {
        if let Some((v, r)) = rest.split_once(mark) {
            degrees += part(v)? / div;
            (any, next, rest) = (true, div * 60.0, r);
        }
    }
    // A last unmarked part is the next unit down (`30d30` is 30.5°), or the degrees themselves.
    if !rest.is_empty() {
        if next > 3600.0 {
            return None;
        }
        degrees += part(rest)? / next;
        any = true;
    }
    any.then_some(sign * if negative { -degrees } else { degrees })
}

/// Whether a PROJ definition is in degrees: `longlat`, or one of the names PROJ takes for it.
fn geographic_def(definition: &str) -> bool {
    definition.split_whitespace().any(|t| matches!(t, "+proj=longlat" | "+proj=latlong" | "+proj=lonlat" | "+proj=latlon"))
}

#[cfg(test)]
mod datum_tests {
    use super::*;

    /// Every name PROJ takes for longitude/latitude is in degrees; a projection is not.
    #[test]
    fn longlat_and_its_aliases_are_geographic() {
        for proj in ["longlat", "latlong", "lonlat", "latlon"] {
            assert!(geographic_def(&format!("+proj={proj} +datum=WGS84 +no_defs")), "{proj}");
        }
        assert!(!geographic_def("+proj=utm +zone=16 +datum=WGS84"));
        assert!(!geographic_def("+proj=longlatx"));
        // Read as degrees too: a corner comes back where it was, not off by the radian factor.
        let to = transformer_of("+proj=latlong +datum=WGS84 +no_defs").unwrap();
        let c = to.from_lonlat(Coord::new(-90.0, 35.0)).unwrap();
        assert!((c.x + 90.0).abs() < 1e-9 && (c.y - 35.0).abs() < 1e-9, "{c:?}");
    }

    /// A corner on a datum shifted from WGS 84 (SAD69) moves by the shift, tens of metres; on one
    /// with none (SIRGAS 2000) it stays (#1191).
    #[test]
    fn a_point_moves_onto_a_shifted_datum_and_not_onto_an_unshifted_one() {
        let c = Coord::new(-45.0, -20.0);
        let sirgas = lonlat_on_datum_of(epsg_proj4(31983).unwrap(), c).unwrap();
        assert!((sirgas.x - c.x).abs() < 1e-9 && (sirgas.y - c.y).abs() < 1e-9, "{sirgas:?}");
        let sad69 = epsg_proj4(29193).unwrap();
        assert!(sad69.contains("+towgs84="), "{sad69}");
        let on = lonlat_on_datum_of(sad69, c).unwrap();
        // About 40 m east and 40 m north at 20° S: well past a centimetre, short of a kilometre.
        let (dx, dy) = ((on.x - c.x) * 111_320.0 * 20f64.to_radians().cos(), (on.y - c.y) * 110_574.0);
        let moved = dx.hypot(dy);
        assert!(moved > 10.0 && moved < 200.0, "{moved} m ({dx} m, {dy} m)");
    }
}

/// A WGS 84 longitude/latitude on the datum of the PROJ definition `def`: where a reader that
/// takes `def`'s own geographic system puts the point (a GeoPDF's corners, #1191). The same
/// point where `def`'s datum has no shift from WGS 84.
pub fn lonlat_on_datum_of(def: &str, c: Coord) -> Option<Coord> {
    transformer_of(&geographic_of(def)).ok()?.from_lonlat(c)
}

/// The WGS 84 longitude/latitude of `c`, a longitude/latitude on the datum of the PROJ definition
/// `def`: [`lonlat_on_datum_of`] the other way, for a GeoPDF's corners read back (#2336).
pub fn lonlat_from_datum_of(def: &str, c: Coord) -> Option<Coord> {
    transformer_of(&geographic_of(def)).ok()?.to_lonlat(c)
}

/// The longitude/latitude system on the datum of the PROJ definition `def`: its datum, ellipsoid,
/// shift and prime meridian, without its projection.
fn geographic_of(def: &str) -> String {
    const DATUM: [&str; 9] = ["datum", "ellps", "towgs84", "a", "b", "rf", "f", "R", "pm"];
    let mut geographic = String::from("+proj=longlat");
    for t in def.split_whitespace() {
        let key = t.trim_start_matches('+').split('=').next().unwrap_or("");
        if DATUM.contains(&key) {
            geographic.push(' ');
            geographic.push_str(t);
        }
    }
    geographic.push_str(" +no_defs");
    geographic
}

/// How far (degrees of latitude, or of longitude scaled to the ground) a place taken into a
/// projection may come back from where it was: about 11 m on each axis, 16 m at most. Inside their
/// areas of use places come back within a centimetre, except within a ten-thousandth of a degree of
/// a pole in the authalic-latitude systems (0.2 m).
const COMES_BACK_DEG: f64 = 1e-4;

/// How far Eckert IV may bring back a place within a hundredth of a degree of a pole: its inverse
/// solves for the latitude to about half a kilometre there (0.0045° at 89.9955°, PROJ 9.8.1 the
/// same), short of a fold.
const ECK4_POLE_DEG: f64 = 0.006;

/// Whether `back`, a place taken into a projection and back, is `c`: within [`COMES_BACK_DEG`],
/// or, for a place within a hundredth of a degree of a pole, within `pole` when that is more.
fn comes_back(c: Coord, back: Coord, pole: f64) -> bool {
    let within = if c.y.abs() > 89.99 { COMES_BACK_DEG.max(pole) } else { COMES_BACK_DEG };
    let dlon = (back.x - c.x + 180.0).rem_euclid(360.0) - 180.0;
    (back.y - c.y).abs() <= within && (dlon * c.y.to_radians().cos()).abs() <= within
}

#[derive(Clone)]
enum Kind {
    LonLat,
    Merc,
    /// Computed by terrio-crs, in both directions.
    Terrio(std::sync::Arc<Live>),
}

/// A system terrio-crs computes, built again with the datum grids that have arrived since
/// ([`grid_generation`]): a transformer keeps the operation it chose when it was built, and what
/// keeps one (a streamed layer, a tile warp, a map's projection) needn't know.
struct Live {
    /// What it was built for, as [`built`] keeps it (a PROJ definition, `WKT …`, `EPSG n`), to
    /// build it again; `None` for one that can't be.
    key: Option<String>,
    /// As first built: its conversion (axes, unit, parameters), which no grid changes.
    first: std::sync::Arc<Terrio>,
    /// The generation of grids it is built for.
    generation: std::sync::atomic::AtomicU64,
    /// Built again, once grids have arrived since it was first: `None` while it is as first built,
    /// which then needs no lock.
    again: std::sync::RwLock<Option<std::sync::Arc<Terrio>>>,
    /// Whether [`Live::again`] holds one.
    rebuilt: std::sync::atomic::AtomicBool,
}

/// [`Live::now`]'s answer: the system as first built, or as built again.
enum Now<'a> {
    First(&'a Terrio),
    Again(std::sync::Arc<Terrio>),
}

impl std::ops::Deref for Now<'_> {
    type Target = Terrio;
    fn deref(&self) -> &Terrio {
        match self {
            Now::First(t) => t,
            Now::Again(t) => t,
        }
    }
}

impl Live {
    fn new(key: Option<String>, generation: u64, t: std::sync::Arc<Terrio>) -> Live {
        Live {
            key,
            first: t,
            generation: std::sync::atomic::AtomicU64::new(generation),
            again: std::sync::RwLock::new(None),
            rebuilt: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// As built for the grids there are now: built again when grids have arrived since. One that
    /// can't be built again goes on as it was. Without grids arriving, two atomic loads.
    fn now(&self) -> Now<'_> {
        use std::sync::atomic::Ordering;
        let generation = grid_generation();
        if self.generation.load(Ordering::Acquire) != generation && self.key.is_some() {
            let again = self.key.as_deref().and_then(rebuilt);
            let mut kept = self.again.write().unwrap_or_else(|e| e.into_inner());
            match again {
                Some(t) => {
                    *kept = Some(t);
                    self.rebuilt.store(true, Ordering::Release);
                }
                // Said once: it goes on with the transformation it had, without the grid.
                None => {
                    if let Some(key) = &self.key {
                        note_rebuild_failure(key);
                    }
                }
            }
            self.generation.store(generation, Ordering::Release);
        }
        if !self.rebuilt.load(Ordering::Acquire) {
            return Now::First(&self.first);
        }
        match &*self.again.read().unwrap_or_else(|e| e.into_inner()) {
            Some(t) => Now::Again(std::sync::Arc::clone(t)),
            None => Now::First(&self.first),
        }
    }

    /// As it is, without building it again.
    fn peek(&self) -> std::sync::Arc<Terrio> {
        self.again.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| std::sync::Arc::clone(&self.first))
    }
}

/// The system [`built`] keeps under `key`, built for the grids there are now.
fn rebuilt(key: &str) -> Option<std::sync::Arc<Terrio>> {
    let t = if let Some(wkt) = key.strip_prefix("WKT ") {
        transformer_of_wkt(wkt).ok()
    } else if let Some(code) = key.strip_prefix("EPSG ").and_then(|c| c.parse::<u16>().ok()) {
        epsg_transformer(code)
    } else {
        transformer_of(key).ok()
    }?;
    match &t.kind {
        Kind::Terrio(live) => Some(live.peek()),
        _ => None,
    }
}

/// A system terrio-crs computes: the transformers to and from longitude/latitude, built once.
struct Terrio {
    /// The system itself, for a direct operation to another ([`direct_operation`]).
    sr: terrio_crs::SpatialRef,
    to_lonlat: terrio_crs::Transformer,
    from_lonlat: terrio_crs::Transformer,
    /// For a projection: WGS 84 longitude/latitude onto its own datum's, and its coordinates back
    /// to that longitude/latitude without a datum shift, so [`Transformer::from_lonlat`] checks
    /// the projection's round trip alone. A datum transformation chosen for each place (by its
    /// area of use) may differ there and back near the edge of that area, by tens of metres, and
    /// that is no fold.
    own: Option<(terrio_crs::Transformer, terrio_crs::Transformer, terrio_crs::Transformer)>,
    /// Its coordinates are angles: longitude and latitude, in [`Terrio::unit`].
    geographic: bool,
    /// Degrees in a unit of a geographic system's coordinates (0.9 in grads; 1 otherwise).
    unit: f64,
    /// Its coordinates are north first (`+axis=neu`).
    lat_first: bool,
    /// Its east-west coordinate points west (`+axis=wnu`, `wsu`): -1, else 1.
    lon_sign: f64,
    /// Its north-south coordinate points south (`+axis=wsu`, `swu`): -1, else 1.
    lat_sign: f64,
    /// The conversion's parameters (centre, offsets, unit, prime meridian), when it has some.
    params: Option<terrio_crs::ProjParams>,
    /// A world projection whose inverse may go on past its edges ([`WRAPPING`]).
    wraps: bool,
    /// Eckert IV, whose inverse is coarse near a pole ([`ECK4_POLE_DEG`]).
    eck4: bool,
}

/// The pseudo-cylindrical world projections whose inverse may go on past their edges (Natural
/// Earth's gives 179.8°W for a place a thousandth past 180°E, and a longitude for a place past a
/// pole), by PROJ's name: what their inverse gives is taken forward again and must land where it
/// was. A cylindrical projection's inverse past its east or west edge is a longitude round the
/// world, as PROJ gives it, and stays.
const WRAPPING: [&str; 9] = ["natearth", "wintri", "eck4", "robin", "moll", "eqearth", "sinu", "wag4", "wag5"];

impl Terrio {
    /// A world projection's coordinates `c` to longitude/latitude through `inverse` (to WGS 84,
    /// or to the projection's own datum), refusing a place past its edges, and taking one a
    /// rounding's worth past its east or west edge as on it (#1925).
    fn unproject(&self, inverse: &terrio_crs::Transformer, forward: &terrio_crs::Transformer, c: Coord) -> Option<Coord> {
        let wraps = self.params.as_ref().filter(|_| self.wraps);
        let back = |c: Coord| -> Option<Coord> {
            let (x, y) = inverse.transform(c.x, c.y).ok()?;
            if let Some(p) = wraps {
                let (fx, fy) = forward.transform(x, y).ok()?;
                // Within 5 m (Robinson's pieces overlap by up to 2.5 m at some table rows), or a
                // ten-millionth of the distance from the centre: a place past an edge lands
                // kilometres or a world away.
                let unit = if p.to_meter > 0.0 { p.to_meter } else { 1.0 };
                let slack = 5.0 / unit + 1e-7 * c.x.abs().max(c.y.abs());
                if (fx - c.x).abs() > slack || (fy - c.y).abs() > slack {
                    return None;
                }
            }
            Some(Coord::new(x, y))
        };
        back(c).or_else(|| self.on_edge(c, &back))
    }

    /// A world projection's coordinates on its east or west edge, as longitude/latitude, when
    /// its inverse (`back`) refused them as past it. Robinson's inverse settles a latitude, then
    /// divides x by the width there, and that latitude's rounding puts the longitude past ±180° by
    /// up to 1.4e-7 of it (PROJ refuses it too), so places on the edge didn't come back. A point no
    /// more than a millionth past it, taken that little towards the centre, is on it (#1925).
    fn on_edge(&self, c: Coord, back: &dyn Fn(Coord) -> Option<Coord>) -> Option<Coord> {
        let p = self.params.as_ref().filter(|_| self.wraps)?;
        let unit = if p.to_meter > 0.0 { p.to_meter } else { 1.0 };
        let x0 = p.x_0 / unit;
        let (f, at) = EDGE_NUDGES.iter().find_map(|f| Some((*f, back(Coord::new(x0 + (c.x - x0) * (1.0 - f), c.y))?)))?;
        // Only a place the nudge brings back by the meridian opposite the centre: one nearer the
        // centre was refused for another reason.
        let lon0 = p.lon_0 + p.prime_meridian;
        let off = (at.x - lon0 + 540.0).rem_euclid(360.0) - 180.0;
        if off.abs() <= 179.0 {
            return None;
        }
        // Back out by the nudge: along a parallel x grows in proportion to the longitude, so the
        // edge is that share further from the central meridian. Within ±180°, as the inverse gives
        // other places; on Greenwich it stays on its own side, at -180° or 180°, which the
        // proportion may overshoot by a hundredth of a second.
        let mut lon = lon0 + off / (1.0 - f);
        if lon.abs() > 180.0 + 1e-3 {
            lon -= 360.0 * (lon / 360.0).round();
        }
        Some(Coord::new(lon.clamp(-180.0, 180.0), at.y))
    }
}

/// What a transformation had to do that changes where places land: a datum transformation that
/// needs a grid RustGIS doesn't have, so the next one was used (NAD27's to WGS 84 then places
/// features up to about 10 m off). The flags terrio-crs keeps for a transformer are turned into notes
/// by [`grid_note`].
fn record_warning(message: String) {
    // terrio-crs's other notes: a datum shift the system doesn't give, which reading it says
    // (`Crs::lacks_datum_shift`), a transformation used outside its area of use, which a map
    // in a regional system drawing the world does all the time, and a geoid, for heights,
    // which aren't transformed yet.
    if !message.starts_with("best coordinate operation needs a missing grid") {
        return;
    }
    // The definition whose transformer [`grid_note_of`] is asking. One built past [`MAX_BUILT`]
    // isn't kept, and flushes as it is dropped, unnamed: an EPSG code's probe ([`probe_grid`])
    // names it, and otherwise the session says that a system it can't name needed one
    // ([`take_unnamed_grid_need`]).
    let def = FLUSHING.try_with(|l| l.borrow().clone()).ok().flatten().unwrap_or_default();
    // An EPSG code's catalog transformer ([`epsg_transformer`]) is noted under the code's PROJ
    // definition, as its probe ([`probe_grid`]) and [`definition_and_label`] name it.
    let def = match def.strip_prefix("EPSG ").and_then(|c| c.parse::<u16>().ok()).and_then(epsg_proj4) {
        Some(proj4) => proj4.to_string(),
        None => def,
    };
    if def.is_empty() {
        UNNAMED_NEEDS_GRID.store(true, std::sync::atomic::Ordering::Relaxed);
        return;
    }
    // Kept without a limit: what is noted is bounded by the definitions built ([`MAX_BUILT`]).
    let mut needs = needs_grid().lock().unwrap_or_else(|e| e.into_inner());
    if !needs.contains(&def) {
        needs.push(def);
    }
}

/// A transformer RustGIS didn't keep ([`MAX_BUILT`]) needed a missing grid: [`record_warning`].
static UNNAMED_NEEDS_GRID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The systems (by the key they are kept under) whose transformer couldn't be built again when
/// grids arrived, each once for the run, in order: they go on with the transformation they had
/// ([`Live::now`]), and every session reads them from its own place ([`rebuild_failures_since`]).
/// The first [`REBUILD_KEPT`] by name, the rest counted.
struct RebuildFailures {
    named: Vec<String>,
    said: std::collections::BTreeSet<String>,
    /// Those past the named, counted once each (by a fingerprint of their key, the first
    /// [`REBUILD_COUNTED`]; past those, each failure of a system not among them).
    more: usize,
    counted: std::collections::BTreeSet<u64>,
}

const REBUILD_COUNTED: usize = 1 << 16;

const REBUILD_KEPT: usize = 256;

impl RebuildFailures {
    fn add(&mut self, key: &str) {
        if self.said.contains(key) {
            return;
        }
        if self.named.len() < REBUILD_KEPT {
            self.said.insert(key.to_string());
            self.named.push(key.to_string());
        } else {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut h);
            // Once each by fingerprint; past those kept, a new one still counts each time.
            let h = h.finish();
            if !self.counted.contains(&h) {
                if self.counted.len() < REBUILD_COUNTED {
                    self.counted.insert(h);
                }
                self.more += 1;
            }
        }
    }
}

static REBUILD_FAILED: std::sync::Mutex<RebuildFailures> = std::sync::Mutex::new(RebuildFailures {
    named: Vec::new(),
    said: std::collections::BTreeSet::new(),
    more: 0,
    counted: std::collections::BTreeSet::new(),
});

/// Records that the system kept under `key` ("EPSG 4267", "WKT …") couldn't be built again with
/// grids that arrived: for the sessions to say ([`rebuild_failures_since`]). Public for tests of
/// the sessions' notice; [`Live::now`] calls it.
#[doc(hidden)]
pub fn note_rebuild_failure(key: &str) {
    REBUILD_FAILED.lock().unwrap_or_else(|e| e.into_inner()).add(key);
}

/// Where a session has read the rebuild failures to ([`rebuild_failures_since`]): now's, for
/// a session starting now.
pub fn rebuild_failures_told() -> (usize, usize) {
    let f = REBUILD_FAILED.lock().unwrap_or_else(|e| e.into_inner());
    (f.named.len(), f.more)
}

/// How many coordinate systems couldn't be built again with grids that arrived since `seen`
/// (from [`rebuild_failures_told`]), and the first of them as said ("EPSG:4267", or a
/// definition's start), and moves `seen` on: for the session to say they go on without them.
pub fn rebuild_failures_since(seen: &mut (usize, usize)) -> (usize, Vec<String>) {
    let f = REBUILD_FAILED.lock().unwrap_or_else(|e| e.into_inner());
    let keys = f.named.get(seen.0..).unwrap_or_default();
    let shown = keys
        .iter()
        .take(3)
        .map(|k| match k.strip_prefix("EPSG ") {
            Some(code) => format!("EPSG:{code}"),
            None => {
                // On one line: a WKT read from a file may hold line breaks.
                let k = k.strip_prefix("WKT ").unwrap_or(k).split_whitespace().collect::<Vec<_>>().join(" ");
                let cut: String = k.chars().take(40).collect();
                if k.chars().count() > 40 { format!("{cut}…") } else { cut }
            }
        })
        .collect();
    let count = keys.len() + f.more.saturating_sub(seen.1);
    *seen = (f.named.len(), f.more);
    (count, shown)
}

/// Whether a coordinate system RustGIS can't name, one of more than [`MAX_BUILT`] used this run,
/// needed a missing datum grid since this was last asked: for the session to say so.
pub fn take_unnamed_grid_need() -> bool {
    UNNAMED_NEEDS_GRID.swap(false, std::sync::atomic::Ordering::Relaxed)
}

thread_local! {
    /// The definition whose transformer [`grid_note_of`] is asking, for [`record_warning`].
    static FLUSHING: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The EPSG codes whose catalog transformer was built for PROJ definition `def` ([`epsg_transformer`]).
fn epsg_codes_of(def: &str) -> Vec<u16> {
    epsg_built().lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, d)| *d == def).map(|(c, _)| *c).collect()
}

/// The EPSG codes built from the catalog, with their PROJ definitions: bounded as the transformers
/// are ([`MAX_BUILT`]).
fn epsg_built() -> &'static std::sync::Mutex<Vec<(u16, &'static str)>> {
    static BUILT: std::sync::OnceLock<std::sync::Mutex<Vec<(u16, &'static str)>>> = std::sync::OnceLock::new();
    BUILT.get_or_init(Default::default)
}

/// The definitions whose most accurate datum transformation needed a missing grid since the app
/// started (a session says each once).
fn needs_grid() -> &'static std::sync::Mutex<Vec<String>> {
    static NEEDS: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> = std::sync::OnceLock::new();
    NEEDS.get_or_init(Default::default)
}

/// The note for `crs`, when its most accurate datum transformation needed a grid RustGIS doesn't
/// have yet, so the next one was used: for the app to say once (`Session`'s notices). A system
/// that hasn't needed one, or hasn't been used, has none.
pub fn grid_note(crs: Crs) -> Option<String> {
    let (def, label) = definition_and_label(crs)?;
    grid_note_of(&def, &label)
}

/// The key of the transformer a system is transformed by (its PROJ definition, or for a system
/// read from WKT, `WKT {text}`), and what a note calls it; `None` for WGS 84 longitude/latitude
/// and Web Mercator, which need no datum transformation.
pub fn definition_and_label(crs: Crs) -> Option<(String, String)> {
    match crs {
        Crs::Epsg(c) => Some((epsg_proj4(c)?.to_string(), format!("EPSG:{c}"))),
        // Under its transformer's key: a WKT's, else its PROJ definition ([`CustomCrs::grid_key`]).
        Crs::Custom(_) => crs.custom_def().map(|d| (d.grid_key(), d.label.clone())),
        Crs::LonLat | Crs::WebMercator => None,
    }
}

/// The note of `count` systems whose most accurate datum transformation needed a grid
/// ([`needs_a_missing_grid`]), `names` naming them (the app lists a few and counts the rest):
/// one notice for all of them, with their count.
pub fn grid_notes_text(count: usize, names: &str) -> String {
    const TAIL: &str = "the most accurate datum transformation needs a grid RustGIS doesn't have yet, so the next one was used; places may be metres off (NAD27, for one, up to about 10 m off without its grid)";
    match count {
        1 => format!("{names}: {TAIL}"),
        _ => format!("{count} coordinate systems ({names}): {TAIL}"),
    }
}

/// The middle of an EPSG system's area of use, as the registry gives it (west past east runs
/// across the antimeridian). Each code's answer, none included, is kept: asking searches
/// terrio-crs's whole catalog, about half a millisecond, after it is read once a run (about
/// 50 ms).
pub fn area_centre(code: u16) -> Option<Coord> {
    type Centres = std::sync::Mutex<std::collections::HashMap<u16, Option<Coord>>>;
    static CENTRES: std::sync::OnceLock<Centres> = std::sync::OnceLock::new();
    let centres = CENTRES.get_or_init(Default::default);
    if let Some(c) = centres.lock().unwrap_or_else(|e| e.into_inner()).get(&code) {
        return *c;
    }
    let centre = area_centre_uncached(code);
    let mut centres = centres.lock().unwrap_or_else(|e| e.into_inner());
    if centres.len() < MAX_BUILT {
        centres.insert(code, centre);
    }
    centre
}

fn area_centre_uncached(code: u16) -> Option<Coord> {
    let query = terrio_crs::CrsQuery {
        authorities: vec!["EPSG".into()],
        text: Some(format!("EPSG:{code}")),
        include_deprecated: true,
        ..Default::default()
    };
    let [w, s, e, n] = terrio_crs::list_crs(&query).into_iter().find(|e| e.code == u32::from(code))?.bounds?;
    let lon = if w > e { ((w + e + 360.0) / 2.0 + 180.0).rem_euclid(360.0) - 180.0 } else { (w + e) / 2.0 };
    Some(Coord::new(lon, (s + n) / 2.0))
}

/// The definitions [`probe_grid`] has asked about, since grids last changed.
fn probed() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static PROBED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    PROBED.get_or_init(Default::default)
}

/// Whether a definition's most accurate datum transformation needs a grid RustGIS doesn't have,
/// asked once per definition at a place where it is used (`at`: the middle of an EPSG code's area
/// of use): terrio-crs notes a missing grid by the first place a transformer takes, which may be
/// one where the grid doesn't reach (a world map's corner), and then never again. A system
/// without a code, which has no area of use to ask at, has only that first place. The first use
/// of a code reads terrio-crs's catalog of areas ([`area_centre`]: about 50 ms, once a run).
pub fn probe_grid(def: &str, at: impl FnOnce() -> Option<Coord>) {
    {
        let probed = probed().lock().unwrap_or_else(|e| e.into_inner());
        // Without a limit: only EPSG codes' definitions are probed, a table's worth at most.
        if probed.contains(def) {
            return;
        }
    }
    let Some(at) = at().filter(|c| c.is_finite()) else { return };
    probed().lock().unwrap_or_else(|e| e.into_inner()).insert(def.to_string());
    grids_set_up();
    let strict = terrio_crs::TransformOptions { only_best: true, ..Default::default() };
    let Ok(sr) = terrio_crs::SpatialRef::from_proj4(&in_degrees(def)) else { return };
    let Ok(wgs84) = terrio_crs::SpatialRef::from_epsg(4326) else { return };
    let Ok(t) = terrio_crs::Transformer::with_options(&wgs84, &sr, &strict) else { return };
    // Its own notes are the probe's answer, not a place's.
    let t = t.with_warning_handler(|_| {});
    if matches!(t.transform(at.x, at.y), Err(terrio_crs::Error::MissingGrid(_))) {
        let mut needs = needs_grid().lock().unwrap_or_else(|e| e.into_inner());
        if !needs.iter().any(|d| d == def) {
            needs.push(def.to_string());
        }
    }
}

thread_local! {
    /// The systems this thread has asked for a transformer for since [`take_used`] last looked
    /// (definition and label), so the app can check those a command used without keeping them
    /// in its project: an export's target system, a grid's, a calculation's.
    static USED: std::cell::RefCell<std::collections::HashMap<String, String>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Notes that this thread asked for a transformer for `def` (called `label`), for
/// [`take_used`]: without a limit, as each system is noted once and a command uses a few. The
/// map's projections note theirs too, for a background export's map.
pub fn note_used(def: &str, label: impl FnOnce() -> String) {
    let _ = USED.try_with(|u| {
        let mut u = u.borrow_mut();
        if !u.contains_key(def) {
            u.insert(def.to_string(), label());
        }
    });
}

/// The systems this thread asked for a transformer for since it last looked, as (definition,
/// label): for a worker thread (a background export) to hand to the session that says them.
pub fn take_used() -> Vec<(String, String)> {
    USED.try_with(|u| u.borrow_mut().drain().collect()).unwrap_or_default()
}

/// [`grid_note`] for a PROJ definition (a map's projection), called `label`.
pub fn grid_note_of(def: &str, label: &str) -> Option<String> {
    needs_a_missing_grid(def).then(|| grid_notes_text(1, label))
}

/// Whether a definition's most accurate datum transformation needed a grid RustGIS doesn't have,
/// as far as it has been used or probed ([`probe_grid`]).
pub fn needs_a_missing_grid(def: &str) -> bool {
    // The definition's transformer, and an EPSG code's from the catalog that it names
    // ([`epsg_transformer`], noted under the definition by [`record_warning`]).
    let codes: Vec<String> = epsg_codes_of(def).into_iter().map(|c| format!("EPSG {c}")).collect();
    let map = built().lock().unwrap_or_else(|e| e.into_inner());
    for key in std::iter::once(def).chain(codes.iter().map(String::as_str)) {
        if let Some(Ok(Transformer { kind: Kind::Terrio(live) })) = map.get(key) {
            let t = live.peek();
            let _ = FLUSHING.try_with(|l| *l.borrow_mut() = Some(key.to_string()));
            t.to_lonlat.flush_warnings();
            t.from_lonlat.flush_warnings();
            if let Some((onto, unproject, _)) = &t.own {
                onto.flush_warnings();
                unproject.flush_warnings();
            }
            let _ = FLUSHING.try_with(|l| *l.borrow_mut() = None);
        }
    }
    drop(map);
    // A pair's direct operation ([`pair_key`]), noted under its key.
    if def.starts_with("PAIR ") {
        let op = direct().lock().unwrap_or_else(|e| e.into_inner()).get(def).and_then(|d| d.op.clone());
        if let Some(op) = op {
            op.flush();
        }
    }
    needs_grid().lock().unwrap_or_else(|e| e.into_inner()).iter().any(|d| d == def)
}

/// How far past a world projection's east or west edge, as shares of the distance from its
/// centre, a place is taken back towards it ([`Terrio::on_edge`]): the least that brings it back
/// first, so it moves by no more than its rounding.
const EDGE_NUDGES: [f64; 4] = [1e-9, 1e-8, 1e-7, 1e-6];

/// Converts between a raster's coordinates and WGS 84 longitude/latitude.
#[derive(Clone)]
pub struct Transformer {
    kind: Kind,
}

impl std::fmt::Debug for Transformer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            Kind::LonLat => write!(f, "Transformer(lonlat)"),
            Kind::Merc => write!(f, "Transformer(mercator)"),
            Kind::Terrio(live) => write!(f, "Transformer({})", live.first.params.as_ref().map_or("?", |p| p.method.as_str())),
        }
    }
}

/// The world projections whose outline's east and west edges are the meridian opposite their
/// central one (#1701), by PROJ's name.
const WORLD_METHODS: [&str; 3] = ["natearth", "wintri", "eck4"];

impl Transformer {
    /// The transformer between `sr` and WGS 84 longitude/latitude; `geographic` when its
    /// coordinates are degrees. A system on another body than the Earth goes to and from its own
    /// longitude/latitude instead, which is read as it is, on the Earth ([`Crs::is_planetary`]).
    fn of(sr: &terrio_crs::SpatialRef, geographic: bool, (lat_first, west, south): (bool, bool, bool)) -> Result<Transformer, String> {
        grids_set_up();
        let wgs84 = terrio_crs::SpatialRef::from_epsg(4326).map_err(|e| e.to_string())?;
        let pair = |to: &terrio_crs::SpatialRef| -> Result<_, String> {
            let forward = terrio_crs::Transformer::new(sr, to).map_err(|e| e.to_string())?.with_warning_handler(record_warning);
            // `inverse` keeps the handler.
            let back = forward.inverse().map_err(|e| e.to_string())?;
            Ok((forward, back))
        };
        let (to_lonlat, from_lonlat) = match pair(&wgs84) {
            Ok(p) => p,
            Err(e) if !sr.is_earth() => {
                let own = sr.geographic_wkt().ok_or(e)?;
                pair(&terrio_crs::SpatialRef::from_wkt(&own))?
            }
            Err(e) => return Err(e),
        };
        let params = sr.projection_params();
        // On WGS 84 itself there is no datum transformation to choose: the round trip to WGS 84 is
        // the projection's own. Nor is there with a shift of the system's own (a `BOUNDCRS`'s, a
        // `TOWGS84`): its geographic system, taken by its EPSG code, would go to WGS 84 by the
        // code's transformation, not the file's, and every point would seem not to come back.
        let own_shift = sr.to_wkt().is_some_and(|w| w.contains("BOUNDCRS[") || w.contains("TOWGS84["));
        let own = (!geographic && !own_shift)
            .then(|| sr.geographic_wkt())
            .flatten()
            .map(|g| own_geographic(sr).unwrap_or_else(|| terrio_crs::SpatialRef::from_wkt(&g)))
            .filter(|g| !g.same_as(&wgs84))
            .and_then(|g| {
                let unproject = terrio_crs::Transformer::new(sr, &g).ok()?.with_warning_handler(record_warning);
                let project = unproject.inverse().ok()?;
                Some((terrio_crs::Transformer::new(&wgs84, &g).ok()?.with_warning_handler(record_warning), unproject, project))
            });
        let method = |names: &[&str]| params.as_ref().is_some_and(|p| !geographic && names.contains(&p.method.as_str()));
        let (wraps, eck4) = (method(&WRAPPING), method(&["eck4"]));
        let unit = if geographic { sr.to_wkt().map_or(1.0, |w| super::wkt::angle_unit_degrees(&w)) } else { 1.0 };
        let t = Terrio {
            sr: sr.clone(),
            to_lonlat,
            from_lonlat,
            own,
            geographic,
            unit,
            lat_first,
            lon_sign: if west { -1.0 } else { 1.0 },
            lat_sign: if south { -1.0 } else { 1.0 },
            params,
            wraps,
            eck4,
        };
        Ok(Transformer { kind: Kind::Terrio(std::sync::Arc::new(Live::new(None, grid_generation(), std::sync::Arc::new(t)))) })
    }

    /// This transformer, built again under `key` ([`built`]'s) when datum grids arrive after
    /// `generation`, the one it was built for.
    fn keyed(self, key: &str, generation: u64) -> Transformer {
        match self.kind {
            Kind::Terrio(live) if live.key.is_none() => {
                Transformer { kind: Kind::Terrio(std::sync::Arc::new(Live::new(Some(key.to_string()), generation, live.peek()))) }
            }
            kind => Transformer { kind },
        }
    }

    /// The grids its most accurate transformation needs at `at` (longitude/latitude) that RustGIS
    /// doesn't have and can download ([`missing_grids`]).
    pub fn missing_grids_at(&self, at: Coord) -> Vec<MissingGrid> {
        let Kind::Terrio(live) = &self.kind else { return Vec::new() };
        let Ok(uses) = live.now().from_lonlat.best_operation_grids_at(at.x, at.y, 0.0) else { return Vec::new() };
        uses.into_iter().filter(downloadable).filter_map(|g| Some(MissingGrid { url: g.url?, name: g.name })).collect()
    }

    /// Native coordinates to longitude/latitude.
    pub fn to_lonlat(&self, c: Coord) -> Option<Coord> {
        let out = match &self.kind {
            Kind::LonLat => c,
            Kind::Merc => merc_to_lonlat(c),
            Kind::Terrio(live) => {
                let t = live.now();
                // A latitude past a pole is no place (terrio-crs would fold it back to one): past
                // 90°, in the system's unit (100 grads).
                let lat = if t.lat_first { c.x } else { c.y };
                if !c.is_finite() || (t.geographic && (lat * t.unit).abs() > 90.0 + 1e-9) {
                    return None;
                }
                let at = t.unproject(&t.to_lonlat, &t.from_lonlat, c)?;
                // On the antimeridian (a rounding past it included), the end on the place's side of
                // the map: a world layer's shapes on its west edge stay there, at -180°.
                if (at.x.abs() - 180.0).abs() < 1e-9 {
                    // Its longitude pointing east, whatever its axes' order and direction.
                    let x = t.lon_sign * if t.lat_first { c.y } else { c.x };
                    let east = t.params.as_ref().is_none_or(|p| x >= p.x_0 / if p.to_meter > 0.0 { p.to_meter } else { 1.0 });
                    Coord::new(if east { 180.0 } else { -180.0 }, at.y)
                } else {
                    at
                }
            }
        };
        out.is_finite().then_some(out)
    }

    /// Extent samples keep a geographic grid's winding without losing its prime meridian or
    /// datum shift. Only whole turns change; other consumers keep `to_lonlat`'s usual branch.
    pub(super) fn to_lonlat_for_extent(&self, c: Coord) -> Option<Coord> {
        let mut out = self.to_lonlat(c)?;
        if let Kind::Terrio(live) = &self.kind
            && live.first.geographic
        {
            let t = &live.first;
            let lon = t.lon_sign * t.unit * if t.lat_first { c.y } else { c.x };
            let greenwich = lon + t.params.as_ref().map_or(0.0, |p| p.prime_meridian);
            out.x += 360.0 * ((greenwich - out.x) / 360.0).round();
        }
        out.is_finite().then_some(out)
    }

    /// Longitude/latitude to native coordinates, or `None` where the system can't hold the place.
    /// A projection takes some places outside its area of use that it can't bring back, as PROJ
    /// does: transverse Mercator up to 150° from its central meridian, where those near 90° off on
    /// the equator come back anywhere; oblique Mercator the far side of the globe, mirrored;
    /// Albers a place near a pole, as the pole. So its result is taken back, and a place that
    /// doesn't come back (within [`COMES_BACK_DEG`]) is refused rather than put somewhere else
    /// (#1925). A place that comes back keeps the projection's coordinates.
    pub fn from_lonlat(&self, c: Coord) -> Option<Coord> {
        let out = self.from_lonlat_unchecked(c)?;
        if let Kind::Terrio(live) = &self.kind
            && !live.first.geographic
        {
            let t = live.now();
            let back = match &t.own {
                // On the projection's own datum: the place there, projected and taken back. Its
                // coordinates from there, not `out`: WGS 84 onto that datum may go by another
                // transformation than the system's own (ESRI:31491 in the North Sea, outside the
                // DHDN Helmert's area, where the system's is a ballpark), and the projection
                // alone is what may fold. `out` itself isn't checked: it is the same projection
                // of a place a datum shift away, so only a strip that wide along a fold (metres,
                // or hundreds of metres for a ballpark) could be judged differently.
                Some((onto, unproject, project)) => onto.transform(c.x, c.y).ok().and_then(|(x, y)| {
                    let (px, py) = project.transform(x, y).ok()?;
                    Some((Coord::new(x, y), t.unproject(unproject, project, Coord::new(px, py))?))
                }),
                None => self.to_lonlat(out).map(|back| (c, back)),
            };
            let pole = if t.eck4 { ECK4_POLE_DEG } else { 0.0 };
            if !back.is_some_and(|(there, back)| comes_back(there, back, pole)) {
                return None;
            }
        }
        Some(out)
    }

    /// Longitude/latitude to native coordinates as the projection gives them, without checking
    /// that they come back (a map's projection checks that itself, where it folds).
    pub fn from_lonlat_unchecked(&self, c: Coord) -> Option<Coord> {
        let out = match &self.kind {
            Kind::LonLat => c,
            Kind::Merc => lonlat_to_merc(c),
            Kind::Terrio(live) => {
                if !c.is_finite() || c.y.abs() > 90.0 + 1e-9 {
                    return None;
                }
                let (x, y) = live.now().from_lonlat.transform(c.x, c.y).ok()?;
                Coord::new(x, y)
            }
        };
        out.is_finite().then_some(out)
    }

    /// Whether the raster's own coordinates are longitude/latitude degrees.
    pub fn is_geographic(&self) -> bool {
        match &self.kind {
            Kind::LonLat => true,
            Kind::Merc => false,
            Kind::Terrio(live) => live.first.geographic,
        }
    }

    /// Whether a datum transformation from WGS 84 is applied at `at` (longitude/latitude): one
    /// other than PROJ's ballpark, which knows no shift for the datum there.
    pub fn shift_at(&self, at: Coord) -> bool {
        match &self.kind {
            Kind::Terrio(live) => live.now().from_lonlat.operation_at(at.x, at.y, 0.0).ok().flatten().is_some_and(|o| !o.ballpark),
            _ => false,
        }
    }

    /// Which way its coordinates point: for x, then y, the (east, north) of a step of one along it,
    /// by the order and direction of its axes (`+axis=wsu`: x west, y south). East, then north,
    /// for a system without its own.
    pub fn axis_directions(&self) -> [(f64, f64); 2] {
        match &self.kind {
            Kind::Terrio(live) => {
                let t = &live.first;
                let (east, north) = ((t.lon_sign, 0.0), (0.0, t.lat_sign));
                if t.lat_first { [north, east] } else { [east, north] }
            }
            _ => [(1.0, 0.0), (0.0, 1.0)],
        }
    }

    /// The conversion's parameters (method, centre, offsets, unit, prime meridian), for a system
    /// terrio-crs computes that has some.
    pub fn params(&self) -> Option<&terrio_crs::ProjParams> {
        match &self.kind {
            Kind::Terrio(live) => live.first.params.as_ref(),
            _ => None,
        }
    }

    /// The WGS 84 longitude of its central meridian, when its projection is a world projection
    /// whose outline's east and west edges are the meridian opposite (Natural Earth, Winkel
    /// Tripel, Eckert IV, #1701).
    pub(super) fn world_central_meridian(&self) -> Option<f64> {
        let p = self.params().filter(|p| WORLD_METHODS.contains(&p.method.as_str()))?;
        let unit = if p.to_meter > 0.0 { p.to_meter } else { 1.0 };
        self.to_lonlat(Coord::new(p.x_0 / unit, p.y_0 / unit)).map(|c| c.x).filter(|lon| lon.is_finite())
    }

    /// Web Mercator to native coordinates (the renderer's direction).
    pub fn from_merc(&self, c: Coord) -> Option<Coord> {
        match &self.kind {
            Kind::Merc => Some(c),
            _ => self.from_lonlat(merc_to_lonlat(c)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The length of a system's unit, as PROJ 9.9.0 gives its axes' `LENGTHUNIT`
    /// (`projinfo -o WKT2_2019 EPSG:2227`: "US survey foot",0.304800609601219; EPSG:3361: "foot",
    /// 0.3048; EPSG:32632: "metre",1).
    #[test]
    fn a_systems_unit_has_its_length() {
        assert!((Crs::Epsg(2227).metres_per_unit() - 0.304_800_609_601_219).abs() < 1e-15);
        assert!((Crs::Epsg(3361).metres_per_unit() - 0.3048).abs() < 1e-15);
        assert_eq!(Crs::Epsg(32632).metres_per_unit(), 1.0);
        assert_eq!(Crs::parse("+proj=utm +zone=32 +datum=WGS84 +units=km").unwrap().metres_per_unit(), 1000.0);
        assert_eq!(Crs::LonLat.metres_per_unit(), 1.0);
    }

    /// Which way a system's x and y point, as PROJ 9.9.0 gives its axes (`projinfo -o PROJ
    /// EPSG:22275`: `+axis=wsu`, x west and y south; EPSG:2065: `+axis=swu`, x south and y west).
    #[test]
    fn a_systems_axes_have_their_directions() {
        assert_eq!(Crs::Epsg(32632).axis_directions(), [(1.0, 0.0), (0.0, 1.0)]);
        assert_eq!(Crs::Epsg(22275).axis_directions(), [(-1.0, 0.0), (0.0, -1.0)]);
        assert_eq!(Crs::Epsg(2065).axis_directions(), [(0.0, -1.0), (-1.0, 0.0)]);
        assert_eq!(Crs::parse("+proj=utm +zone=32 +datum=WGS84 +axis=neu").unwrap().axis_directions(), [(0.0, 1.0), (1.0, 0.0)]);
    }

    /// World projections, whose planes have the antimeridian as a seam, are told apart from regional
    /// ones by their method.
    #[test]
    fn world_projections_are_told_apart() {
        for c in [3395, 4087, 32662, 54030, 8857, 54009] {
            let crs = Crs::parse(&format!("{c}")).or_else(|_| Crs::parse(&format!("ESRI:{c}"))).unwrap();
            assert!(crs.is_world_projection(), "{c}");
        }
        for c in [32632, 2056, 25832, 3035, 31467, 3413] {
            assert!(!Crs::from_epsg(c).unwrap().is_world_projection(), "{c}");
        }
        assert!(Crs::WebMercator.is_world_projection() && !Crs::LonLat.is_world_projection());
        // PROJ's own spelling of Web Mercator, as a file may give it.
        assert!(Crs::parse("+proj=webmerc +datum=WGS84 +units=m +no_defs").unwrap().is_world_projection());
        assert!(Crs::parse("+proj=\"eqc\" +datum=WGS84 +units=m").unwrap().is_world_projection());
    }

    /// A saved code another variant stands for reads as that variant, so Web Mercator saved as
    /// `{"Epsg": 3857}` isn't taken for a projected system of its own.
    #[test]
    fn a_saved_code_another_variant_stands_for_reads_as_it() {
        for (code, crs) in [(3857, Crs::WebMercator), (3785, Crs::WebMercator), (4326, Crs::LonLat), (32632, Crs::Epsg(32632))] {
            assert_eq!(Crs::try_from(SavedCrs::Epsg(code)), Ok(crs), "EPSG:{code}");
        }
    }

    /// An ESRI code reads as the system the picker offers for it (`ESRI:102033`, as a `.prj` and
    /// the coordinate system boxes name it); one the catalog lacks is refused with its name.
    #[test]
    fn an_esri_code_is_read() {
        for text in ["ESRI:102033", "esri:102033", " ESRI : 102033 "] {
            assert_eq!(Crs::parse(text).map(|c| c.label()), Ok("ESRI:102033".to_string()), "{text}");
        }
        assert!(Crs::parse("ESRI:1").is_err_and(|e| e.contains("ESRI:1")));
        assert!(Crs::parse("ESRI:x").is_err() && Crs::parse("ESRI:+102033").is_err());
        // A bare number: EPSG's, else ESRI's.
        assert_eq!(Crs::parse("102033").map(|c| c.label()), Ok("ESRI:102033".to_string()));
        assert_eq!(Crs::parse("2056").map(|c| c.epsg()), Ok(2056));
        assert!(Crs::parse("EPSG:102033").is_err(), "an EPSG code stays EPSG's");
        // Web Mercator's old number is read as before.
        assert_eq!(Crs::parse("102100"), Ok(Crs::WebMercator));
        assert_eq!(Crs::parse("0102033").map(|c| c.label()), Ok("ESRI:102033".to_string()));
    }

    /// A kept transformer that can't be built again when grids arrive goes on as it was, said
    /// once however many grids arrive; one that is built again isn't said.
    #[test]
    fn a_transformer_that_cant_be_built_again_goes_on_and_is_said() {
        let Some(Transformer { kind: Kind::Terrio(live), .. }) = epsg_transformer(4267) else { panic!("EPSG:4267") };
        // Kept under a key that no longer builds, and from another grid generation than now's.
        // (Generations no test reaches, so another test's grids arriving meanwhile can't match them.)
        let stale = Live::new(Some("WKT not a definition".into()), u64::MAX, live.peek());
        let mut seen = rebuild_failures_told();
        let mut other = seen;
        assert!(matches!(stale.now(), Now::First(_)), "goes on with the transformation it had");
        let (failed, shown) = rebuild_failures_since(&mut seen);
        assert!(failed >= 1 && shown.iter().any(|s| s == "not a definition"), "{failed} {shown:?}");
        // Said once: not again in this generation, nor when more grids arrive.
        let _ = stale.now();
        stale.generation.store(u64::MAX - 1, std::sync::atomic::Ordering::SeqCst);
        let _ = stale.now();
        assert!(!rebuild_failures_since(&mut seen).1.iter().any(|s| s == "not a definition"));
        // Another session hears it too, once.
        assert!(rebuild_failures_since(&mut other).1.iter().any(|s| s == "not a definition"));
        assert!(!rebuild_failures_since(&mut other).1.iter().any(|s| s == "not a definition"));
        // Past those kept by name, counted.
        let mut f = RebuildFailures { named: Vec::new(), said: Default::default(), more: 0, counted: Default::default() };
        for i in 0..REBUILD_KEPT + 3 {
            f.add(&format!("EPSG {i}"));
        }
        // Each once, past the named too.
        f.add("EPSG 0");
        f.add(&format!("EPSG {}", REBUILD_KEPT + 1));
        assert_eq!((f.named.len(), f.more), (REBUILD_KEPT, 3));
        // Past the fingerprints kept, one already counted still isn't counted again.
        for i in 0..REBUILD_COUNTED {
            f.add(&format!("WKT {i}"));
        }
        let more = f.more;
        f.add("WKT 0");
        f.add(&format!("EPSG {}", REBUILD_KEPT + 1));
        assert_eq!(f.more, more);
        f.add("WKT new");
        assert_eq!(f.more, more + 1, "a new one past them counts");
        // One that is built again: nothing said.
        let fine = Live::new(Some("EPSG 4267".into()), u64::MAX, live.peek());
        assert!(matches!(fine.now(), Now::Again(_)));
        assert!(!rebuild_failures_since(&mut seen).1.iter().any(|s| s == "EPSG:4267"));
    }

    /// Only PROJ's own grid server, a grid file itself and an open licence: a definition read
    /// from a file names any grid PROJ knows, some of them elsewhere (rdtrans2008.gsb, on
    /// salsa.debian.org, without an open licence), and none of those is fetched.
    #[test]
    fn only_open_grids_on_projs_server_are_downloaded() {
        let grid = |name: &str, url: Option<&str>, direct: bool, open: bool, available: bool, optional: bool| terrio_crs::GridUse {
            name: name.into(),
            url: url.map(str::to_string),
            direct_download: direct,
            open_license: open,
            available,
            optional,
        };
        let cdn = Some("https://cdn.proj.org/us_noaa_conus.tif");
        assert!(downloadable(&grid("us_noaa_conus.tif", cdn, true, true, false, false)));
        for g in [
            grid("us_noaa_conus.tif", cdn, true, true, true, false),
            grid("us_noaa_conus.tif", cdn, true, true, false, true),
            grid("us_noaa_conus.tif", cdn, false, true, false, false),
            grid("us_noaa_conus.tif", cdn, true, false, false, false),
            grid("us_noaa_conus.tif", None, true, true, false, false),
            grid(
                "rdtrans2008.gsb",
                Some("https://salsa.debian.org/debian-gis-team/proj-rdnap/raw/upstream/2008/rdtrans2008.gsb"),
                true,
                true,
                false,
                false,
            ),
            grid("../a.tif", Some("https://cdn.proj.org/us_noaa_conus.tif"), true, true, false, false),
            // On the server, but not a GeoTIFF grid.
            grid("nz_linz_nzgd2000-20180701.json", Some("https://cdn.proj.org/nz_linz_nzgd2000-20180701.json"), true, true, false, false),
            grid("a.tif", Some("http://cdn.proj.org/a.tif"), true, true, false, false),
        ] {
            assert!(!downloadable(&g), "{g:?}");
        }
    }

    /// A system read from WKT is saved with its WKT beside a PROJ string, so a newer release reads
    /// the WKT and an older one, which ignores the field, the PROJ string; a project saved without
    /// the field (by an older release) still opens. Where PROJ writes no PROJ string (South
    /// Orientated with false offsets), the one RustGIS wrote before is saved.
    #[test]
    fn a_saved_system_keeps_its_wkt_and_a_proj_string() {
        let lo19 = r#"PROJCS["Lo19 offset",GEOGCS["Hartebeesthoek94",DATUM["Hartebeesthoek94",SPHEROID["WGS 84",6378137,298.257223563],AUTHORITY["EPSG","6148"]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Transverse_Mercator_South_Orientated"],PARAMETER["latitude_of_origin",0],PARAMETER["central_meridian",19],PARAMETER["scale_factor",1],PARAMETER["false_easting",1000],PARAMETER["false_northing",2000],UNIT["metre",1]]"#;
        for (wkt, at) in [(super::super::wkt::tests::NAD27_UTM17, Coord::new(-81.0, 35.0)), (lo19, Coord::new(19.5, -30.0))] {
            let crs = super::super::wkt::crs_from_wkt(wkt).unwrap();
            let json = serde_json::to_value(crs).unwrap();
            let proj4 = json["Proj"]["proj4"].as_str().unwrap();
            assert!(!proj4.is_empty() && json["Proj"]["wkt"].is_string(), "{json}");
            assert_eq!(serde_json::from_value::<Crs>(json.clone()).unwrap(), crs);
            // As an older release reads it: by its PROJ string alone, placing points alike.
            let mut old = json.clone();
            old["Proj"].as_object_mut().unwrap().remove("wkt");
            let older: Crs = serde_json::from_value(old).unwrap();
            assert!(older.custom_def().unwrap().wkt.is_none());
            let (a, b) = (crs.transformer().unwrap().from_lonlat(at).unwrap(), older.transformer().unwrap().from_lonlat(at).unwrap());
            assert!((a.x - b.x).hypot(a.y - b.y) < 0.01, "{wkt}: {a:?} {b:?}");
        }
    }

    #[test]
    fn utm_round_trip() {
        let crs = Crs::from_epsg(32617).unwrap();
        assert!(!crs.is_geographic());
        let t = crs.transformer().unwrap();
        let ll = t.to_lonlat(Coord::new(500_000.0, 4_649_776.22)).unwrap();
        assert!((ll.x + 81.0).abs() < 1e-6, "{ll:?}");
        assert!((ll.y - 42.0).abs() < 1e-4, "{ll:?}");
        let back = t.from_lonlat(ll).unwrap();
        assert!((back.x - 500_000.0).abs() < 0.01 && (back.y - 4_649_776.22).abs() < 0.01);
    }

    /// #1925: places a projection can't bring back are refused, and Robinson's edge comes back.
    #[test]
    fn places_that_dont_come_back_are_refused() {
        let utm = Crs::from_epsg(32617).unwrap().transformer().unwrap();
        assert_eq!(utm.from_lonlat(Coord::new(10.598, 2.156)), None);
        let swiss = Crs::from_epsg(2056).unwrap().transformer().unwrap();
        assert_eq!(swiss.from_lonlat(Coord::new(-170.0, 0.0)), None);
        assert!(swiss.from_lonlat(Coord::new(7.44, 46.95)).is_some());
        // On WGS 84 the map's west edge comes back at -180°, not 180°, or a world layer's shapes
        // would cross the map; a datum shift can put ±180° a hair round to the other end.
        for (def, same_end) in [
            ("+proj=robin +lon_0=0 +x_0=1000 +y_0=0 +datum=WGS84 +units=m +no_defs", true),
            ("+proj=robin +lon_0=0 +x_0=1000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs", false),
            ("+proj=robin +lon_0=150 +x_0=1000 +y_0=0 +datum=WGS84 +units=m +no_defs", false),
        ] {
            let t = Crs::parse(def).unwrap().transformer().unwrap();
            let centre = t.to_lonlat(Coord::new(1000.0, 0.0)).unwrap().x;
            for lon in [centre - 180.0, centre + 180.0] {
                for lat in [-60.0, 0.0, 5.008_897_586_777_495, 30.0, 89.0] {
                    let at = Coord::new(lon, lat);
                    let p = t.from_lonlat(at).unwrap_or_else(|| panic!("{def}: {at:?} was refused"));
                    // Past the edge by a rounding's worth, from either system's side: still the edge.
                    let past = Coord::new(1000.0 + (p.x - 1000.0) * (1.0 + 1e-8), p.y);
                    for q in [p, past] {
                        let back = t.to_lonlat(q).unwrap_or_else(|| panic!("{def}: {q:?} from {at:?} was refused"));
                        assert!(back.x.abs() <= 180.0, "{def}: {at:?} came back at {back:?}");
                        let dx = if same_end { back.x - at.x } else { (back.x - at.x + 180.0).rem_euclid(360.0) - 180.0 };
                        assert!(dx.abs() < 1e-4 && (back.y - at.y).abs() < 1e-4, "{def}: {at:?} came back at {back:?}");
                    }
                }
            }
            // Well past the edge is still refused.
            let p = t.from_lonlat(Coord::new(centre + 180.0, 30.0)).unwrap();
            assert_eq!(t.to_lonlat(Coord::new(1000.0 + (p.x - 1000.0) * 1.001, p.y)), None, "{def}");
        }
    }

    /// NAD27's most accurate transformation needs a grid RustGIS doesn't have: the next one is
    /// used (about 10 m), and its note names the system. WGS 84's UTM has none.
    #[test]
    fn a_missing_datum_grid_is_noted() {
        let utm = Crs::from_epsg(32617).unwrap();
        assert!(utm.transformer().unwrap().from_lonlat(Coord::new(-81.0, 35.0)).is_some());
        assert_eq!(grid_note(utm), None);
        let nad27 = Crs::from_epsg(26717).unwrap();
        let p = nad27.transformer().unwrap().from_lonlat(Coord::new(-81.0, 35.0)).unwrap();
        // PROJ 9.8.1 without the grid (pyproj 3.8.0, network off): NAD27 to WGS 84 (4).
        assert!((p.x - 499_982.879).abs() < 0.01 && (p.y - 3_872_833.282).abs() < 0.01, "{p:?}");
        let note = grid_note(nad27).unwrap();
        assert!(note.starts_with("EPSG:26717: ") && note.contains("grid") && note.contains("10 m"), "{note}");
        assert_eq!(grid_note(Crs::LonLat), None);
        // Probed where it is used: NAD27 longitude/latitude in North America, before any place.
        let def = epsg_proj4(4267).unwrap();
        let at = area_centre(4267).unwrap();
        assert!(at.x < -60.0 && at.x > -180.0 && at.y > 10.0 && at.y < 85.0, "{at:?}");
        probe_grid(def, || Some(at));
        assert!(needs_grid().lock().unwrap().iter().any(|d| d == def));
        // An area across the antimeridian (EPSG:3832, PDC Mercator, 98°E to 70°W): its middle is
        // in the Pacific, not in Africa.
        let pdc = area_centre(3832).unwrap();
        assert!(pdc.x > 150.0 || pdc.x < -150.0, "{pdc:?}");
        // A deprecated code has its area too (EPSG:29100, SAD69 / Brazil Polyconic).
        let all = terrio_crs::CrsQuery { text: Some("EPSG:29100".into()), include_deprecated: true, ..Default::default() };
        assert!(terrio_crs::list_crs(&all).iter().any(|e| e.code == 29100 && e.deprecated));
        let brazil = area_centre(29100).unwrap();
        assert!(brazil.x < -30.0 && brazil.x > -80.0 && brazil.y < 10.0 && brazil.y > -40.0, "{brazil:?}");
        // A code the catalog has no area for is asked once, and kept as none (EPSG:22300).
        assert_eq!(area_centre(22300), None);
        assert_eq!(area_centre(22300), None);
    }

    /// A missing grid that only a transformer RustGIS doesn't keep met is said unnamed, once; one
    /// met by a definition being built is that definition's.
    #[test]
    fn a_missing_grid_without_a_name_is_flagged_once() {
        let message = || "best coordinate operation needs a missing grid (us_noaa_conus.tif)".to_string();
        let _ = take_unnamed_grid_need();
        record_warning(message());
        assert!(take_unnamed_grid_need());
        assert!(!take_unnamed_grid_need());
        let def = "+proj=longlat +datum=NAD27 +no_defs +test=named";
        let _ = FLUSHING.try_with(|l| l.replace(Some(def.to_string())));
        record_warning(message());
        let _ = FLUSHING.try_with(|l| l.replace(None));
        assert!(!take_unnamed_grid_need());
        assert!(needs_a_missing_grid(def));
    }

    /// A definition's angle in radians, as PROJ reads it (an old project's typed definition), is
    /// read in degrees; one that isn't a number is refused.
    #[test]
    fn radians_are_read_as_degrees() {
        assert_eq!(in_degrees("+proj=eqc +lat_ts=0.5r +x_0=1"), format!("+proj=eqc +lat_ts={} +x_0=1", 0.5f64.to_degrees()));
        let r = Crs::parse("+proj=eqc +ellps=WGS84 +lat_ts=0.5r").and_then(Crs::transformer).unwrap();
        let d = Crs::parse(&format!("+proj=eqc +ellps=WGS84 +lat_ts={}", 0.5f64.to_degrees())).and_then(Crs::transformer).unwrap();
        let c = Coord::new(45.0, 10.0);
        assert_eq!(r.from_lonlat(c), d.from_lonlat(c));
        assert!(Crs::parse("+proj=eqc +ellps=WGS84 +lat_ts=0.5x").is_err());
        // Radians whose degrees aren't a number are refused, as such degrees are.
        assert!(Crs::parse("+proj=eqc +ellps=WGS84 +lat_ts=1e308r").is_err());
        // Winkel Tripel's parallel in radians is checked in degrees.
        assert!(Crs::parse("+proj=wintri +lat_1=0.5r +datum=WGS84").is_ok());
        assert!(Crs::parse("+proj=wintri +lat_1=1.6r +datum=WGS84").is_err());
    }

    /// What PROJ 9.8.1 refuses (pyproj 3.8.0) is refused, not read otherwise by terrio-crs: a
    /// distance in degrees, minutes and seconds, a datum shift of other than 3 or 7 numbers. An
    /// angle in degrees, minutes and seconds is read as PROJ reads it, Winkel Tripel's parallel
    /// included.
    #[test]
    fn misread_numbers_are_refused_and_dms_angles_read() {
        for def in [
            "+proj=tmerc +ellps=WGS84 +x_0=500000d",
            "+proj=tmerc +ellps=WGS84 +k=1d",
            "+proj=longlat +ellps=intl +towgs84=100,bad,200,300",
            "+proj=longlat +ellps=intl +towgs84=1,2",
            "+proj=longlat +ellps=intl +towgs84=1,2,3,4",
            "+proj=tmerc +ellps=WGS84 +lat_0=30x",
            "+proj=tmerc +ellps=WGS84 +lat_0=30d30d",
            "+proj=tmerc +ellps=WGS84 +lat_0=d",
        ] {
            assert!(Crs::parse(def).and_then(Crs::transformer).is_err(), "{def}");
        }
        for def in ["+proj=longlat +ellps=intl +towgs84=1,2,3", "+proj=longlat +ellps=intl +towgs84=1,2,3,4,5,6,7"] {
            assert!(Crs::parse(def).and_then(Crs::transformer).is_ok(), "{def}");
        }
        assert_eq!(parse_angle("30d30"), Some(30.5));
        assert_eq!(parse_angle("30d30'36\"S"), Some(-30.51));
        assert_eq!(parse_angle("-52.5N"), Some(52.5));
        assert_eq!(parse_angle("-5W"), Some(-5.0));
        // The degree marks PROJ reads besides `d`.
        assert_eq!(parse_angle("38D30'"), Some(38.5));
        assert_eq!(parse_angle("38°30'"), Some(38.5));
        assert_eq!(parse_angle("30d30\"\""), None);
        let marked = Crs::parse("+proj=tmerc +ellps=WGS84 +lat_0=30°30' +lon_0=9D").and_then(Crs::transformer).unwrap();
        let plain = Crs::parse("+proj=tmerc +ellps=WGS84 +lat_0=30.5 +lon_0=9").and_then(Crs::transformer).unwrap();
        // A prime meridian given as an angle, in any of its forms, as one in degrees; a named one
        // as it is (PROJ 9.8.1: Paris is 2°20'14.025"E).
        let pm = |v: &str| {
            Crs::parse(&format!("+proj=longlat +datum=WGS84 +pm={v}"))
                .and_then(Crs::transformer)
                .unwrap()
                .to_lonlat(Coord::new(0.0, 0.0))
                .unwrap()
        };
        for v in ["2", "2D", "2°", "2d0'0\"E", &format!("{}r", 2f64.to_radians())] {
            assert!((pm(v).x - 2.0).abs() < 1e-9, "{v}: {:?}", pm(v));
        }
        assert!((pm("paris").x - (2.0 + 20.0 / 60.0 + 14.025 / 3600.0)).abs() < 1e-9);
        assert!(Crs::parse("+proj=longlat +datum=WGS84 +pm=2x").and_then(Crs::transformer).is_err());
        // Laborde's azimuth is an angle too: in radians or degrees and minutes as in degrees, and
        // refused when it isn't one (terrio-crs would take 0).
        let labrd = |azi: &str| {
            Crs::parse(&format!("+proj=labrd +lat_0=-18.9 +lon_0=44.1 +azi={azi} +k_0=0.9995 +x_0=400000 +y_0=800000 +ellps=intl +units=m"))
                .and_then(Crs::transformer)
                .map(|t| t.from_lonlat(Coord::new(47.0, -19.0)).unwrap())
        };
        let degrees = labrd("18.9").unwrap();
        for azi in [format!("{}r", 18.9f64.to_radians()), "18d54'".to_string(), "18°54'".to_string()] {
            let at = labrd(&azi).unwrap();
            assert!((at.x - degrees.x).abs() < 1e-6 && (at.y - degrees.y).abs() < 1e-6, "{azi}: {at:?} {degrees:?}");
        }
        assert!(labrd("abc").is_err());
        // A name PROJ doesn't know, as it spells them, is refused, as PROJ 9.8.1 refuses it.
        for v in ["Paris", "pariss"] {
            assert!(Crs::parse(&format!("+proj=longlat +datum=WGS84 +pm={v}")).and_then(Crs::transformer).is_err(), "{v}");
        }
        assert_eq!(marked.from_lonlat(Coord::new(9.5, 47.0)), plain.from_lonlat(Coord::new(9.5, 47.0)));
        let dms = Crs::parse("+proj=tmerc +ellps=WGS84 +lat_0=30d30 +lon_0=9").and_then(Crs::transformer).unwrap();
        let dec = Crs::parse("+proj=tmerc +ellps=WGS84 +lat_0=30.5 +lon_0=9").and_then(Crs::transformer).unwrap();
        assert_eq!(dms.from_lonlat(Coord::new(9.5, 47.0)), dec.from_lonlat(Coord::new(9.5, 47.0)));
        assert!(Crs::parse("+proj=wintri +lat_1=50d27'35\" +datum=WGS84").and_then(Crs::transformer).is_ok());
        assert!(Crs::parse("+proj=wintri +lat_1=90d +datum=WGS84").and_then(Crs::transformer).is_err());
    }

    /// Latitude first (`+axis=neu`): the latitude is the first coordinate, so a longitude past 90°
    /// is a place, and a first coordinate past 90° isn't.
    #[test]
    fn latitude_first_degrees_are_checked_by_their_latitude() {
        let t = Crs::parse("+proj=longlat +datum=WGS84 +axis=neu").and_then(Crs::transformer).unwrap();
        let native = t.from_lonlat(Coord::new(120.0, 45.0)).unwrap();
        let back = t.to_lonlat(native).unwrap();
        assert!((back.x - 120.0).abs() < 1e-9 && (back.y - 45.0).abs() < 1e-9, "{native:?} {back:?}");
        assert_eq!(t.to_lonlat(Coord::new(95.0, 10.0)), None);
        // An extent's corner keeps its turn of the world, latitude first or longitude west.
        let corner = t.to_lonlat_for_extent(Coord::new(45.0, 120.0)).unwrap();
        assert!((corner.x - 120.0).abs() < 1e-9 && (corner.y - 45.0).abs() < 1e-9, "{corner:?}");
        let west = Crs::parse("+proj=longlat +datum=WGS84 +axis=wnu").and_then(Crs::transformer).unwrap();
        let native = west.from_lonlat(Coord::new(100.0, 10.0)).unwrap();
        assert!((native.x + 100.0).abs() < 1e-9, "{native:?}");
        let corner = west.to_lonlat_for_extent(native).unwrap();
        assert!((corner.x - 100.0).abs() < 1e-9 && (corner.y - 10.0).abs() < 1e-9, "{corner:?}");
        // On the antimeridian, the end on the place's side: 180°W stays west whatever the axes.
        assert_eq!(t.to_lonlat(Coord::new(45.0, -180.0)).map(|c| c.x), Some(-180.0));
        assert_eq!(t.to_lonlat(Coord::new(45.0, 180.0)).map(|c| c.x), Some(180.0));
        assert_eq!(west.to_lonlat(Coord::new(180.0, 10.0)).map(|c| c.x), Some(-180.0));
        assert_eq!(west.to_lonlat(Coord::new(-180.0, 10.0)).map(|c| c.x), Some(180.0));
        // A projection's easting pointing west or north first: the same side.
        let edge = std::f64::consts::PI * 6_378_137.0;
        let wsu = Crs::parse("+proj=eqc +datum=WGS84 +axis=wsu").and_then(Crs::transformer).unwrap();
        assert_eq!(wsu.to_lonlat(Coord::new(-edge, 0.0)).map(|c| c.x), Some(180.0));
        assert_eq!(wsu.to_lonlat(Coord::new(edge, 0.0)).map(|c| c.x), Some(-180.0));
        let neu = Crs::parse("+proj=eqc +datum=WGS84 +axis=neu").and_then(Crs::transformer).unwrap();
        assert_eq!(neu.to_lonlat(Coord::new(0.0, edge)).map(|c| c.x), Some(180.0));
        assert_eq!(neu.to_lonlat(Coord::new(0.0, -edge)).map(|c| c.x), Some(-180.0));
    }

    /// A world projection on a sphere whose radius isn't finite and positive places nothing,
    /// either way (#2038, which checked RustGIS's own world projections before terrio-crs took
    /// them over); one of 1 m or the Earth's still works, with its offsets.
    #[test]
    fn world_projections_with_invalid_radii_place_nothing() {
        for method in ["robin", "eqearth", "natearth", "wintri", "eck4"] {
            for radius in ["0", "-0", "-1", "-6378137", "nan", "inf", "-inf"] {
                let def = format!("+proj={method} +R={radius} +units=m");
                let Ok(t) = Crs::parse(&def).and_then(Crs::transformer) else { continue };
                for c in [Coord::new(0.0, 0.0), Coord::new(10.0, 50.0)] {
                    assert_eq!(t.from_lonlat(c), None, "{def}: forward {c:?}");
                    assert_eq!(t.to_lonlat(c), None, "{def}: inverse {c:?}");
                }
            }
            for radius in ["1", "6378137"] {
                let def = format!("+proj={method} +R={radius} +lon_0=30 +x_0=1000 +y_0=-2000 +units=m");
                let t = Crs::parse(&def).and_then(Crs::transformer).unwrap_or_else(|e| panic!("{def}: {e}"));
                let c = Coord::new(40.0, 50.0);
                let back = t.to_lonlat(t.from_lonlat(c).unwrap()).unwrap();
                assert!((back.x - c.x).abs() < 1e-5 && (back.y - c.y).abs() < 1e-5, "{def}: {back:?}");
            }
        }
    }

    /// Hostile definitions in every method terrio-crs computes, typed or read from a file: refused
    /// or computed, never a panic, and a place either lands somewhere finite or is refused.
    #[test]
    fn hostile_definitions_never_panic() {
        let methods = [
            "tmerc",
            "utm +zone=61",
            "lcc +lat_1=90 +lat_2=-90",
            "aea +lat_1=45 +lat_2=-45",
            "omerc +alpha=90 +lat_0=90 +lonc=0",
            "geos +h=-1e308",
            "stere +lat_ts=1e308",
            "krovak +alpha=1e300",
            "ortho +lat_0=91",
            "aeqd +lat_0=-90",
            "eqdc +lat_1=0 +lat_2=0",
            "laea +lat_0=1e-320",
            "merc +lat_ts=90",
            "cea +lat_ts=89.999999999",
            "poly",
            "cass +lat_0=-90",
            "sinu +R=1e-300",
            "robin +R=0",
            "natearth +x_0=1e308",
            "wintri +lat_1=89.9999999",
            "eck4 +a=1e308 +b=1",
            "somerc +lat_0=90",
            "sterea +lat_0=-90",
            "nzmg",
            "labrd +azi=1e9",
            "isea",
            "healpix",
            "qsc",
            "igh",
            "moll +over",
        ];
        let places = [(0.0, 0.0), (180.0, 90.0), (-180.0, -90.0), (1e-300, -1e-300), (179.9999999, 89.9999999), (f64::MAX, 0.0)];
        for m in methods {
            for datum in ["+datum=WGS84", "+a=6378137 +b=6378137", "+a=1e-3 +rf=1.0000001"] {
                let def = format!("+proj={m} {datum} +no_defs");
                let Ok(t) = Crs::parse(&def).and_then(Crs::transformer) else { continue };
                for (x, y) in places {
                    if let Some(p) = t.from_lonlat(Coord::new(x, y)) {
                        assert!(p.is_finite(), "{def}: {p:?}");
                        let _ = t.to_lonlat(p);
                    }
                    let _ = t.to_lonlat(Coord::new(x * 1e5, y * 1e5));
                }
            }
        }
    }

    #[test]
    fn extent_longitudes_keep_prime_meridian_and_datum_shift() {
        let crs = Crs::parse("+proj=longlat +ellps=WGS84 +towgs84=0,1000,0 +pm=paris +no_defs").unwrap();
        let t = crs.transformer().unwrap();
        let native = Coord::new(190.0, 0.0);
        let greenwich = 192.337_229_166_667_f64;
        // At the equator, the specified geocentric translation adds 1000 m to WGS84's Y.
        let angle = greenwich.to_radians();
        let expected = (6_378_137.0 * angle.sin() + 1000.0).atan2(6_378_137.0 * angle.cos()).to_degrees();
        let usual = t.to_lonlat(native).unwrap();
        let extent = t.to_lonlat_for_extent(native).unwrap();
        assert!((usual.x - expected).abs() < 1e-9, "{usual:?}: {expected}");
        assert!((extent.x - (expected + 360.0)).abs() < 1e-9, "{extent:?}: {expected}");
        assert!((extent.x - greenwich).abs() > 0.001, "the datum shift must not be replaced by native longitude");
        assert_eq!(extent.y, usual.y, "only longitude winding changes");
    }

    #[test]
    fn known_systems() {
        assert_eq!(Crs::from_epsg(4326), Some(Crs::LonLat));
        assert_eq!(Crs::from_epsg(3857), Some(Crs::WebMercator));
        assert_eq!(Crs::from_epsg(1), None);
        assert_eq!(Crs::from_epsg(u32::MAX), None);
        let m = Crs::WebMercator.transformer().unwrap();
        let p = m.from_lonlat(Coord::new(10.0, 20.0)).unwrap();
        let q = m.to_lonlat(p).unwrap();
        assert!((q.x - 10.0).abs() < 1e-9 && (q.y - 20.0).abs() < 1e-9);
    }

    #[test]
    fn eqc_reads_proj_gdal_coordinates_without_moving_latitude() {
        // PROJ 9.8 computes +proj=eqc on an ellipsoid with the ellipsoidal formula (EPSG method
        // 1028, PROJ #4656), EPSG:4087 and IAU planetographic definitions included. pyproj 3.8.0
        // (PROJ 9.8.1): `Transformer.from_crs("EPSG:4326", "EPSG:4087", always_xy=True)
        // .transform(0, 45)`; for Mars from `+proj=longlat +a=3396190 +rf=169.894447223612`.
        for (def, lon, lat, x, y) in [
            ("EPSG:4087", 0.0, 45.0, 0.0, 4_984_944.377_977_743),
            ("+proj=eqc +a=3396190 +rf=169.894447223612", -133.8, 18.65, -7_930_954.528_618_374, 1_093_163.433_158_274),
        ] {
            let t = Crs::parse(def).unwrap().transformer().unwrap();
            let m = t.from_lonlat(Coord::new(lon, lat)).unwrap();
            assert!((m.x - x).abs() < 0.01 && (m.y - y).abs() < 0.01, "{def}: {m:?}");
            let back = t.to_lonlat(Coord::new(x, y)).unwrap();
            assert!((back.x - lon).abs() < 1e-6 && (back.y - lat).abs() < 1e-6, "{def}: {back:?}");
        }
    }

    /// EPSG:4087 and equidistant cylindrical systems on ellipsoidal datums match PROJ 9.8.1, which
    /// computes them with the ellipsoidal formula (EPSG method 1028). pyproj 3.8.0:
    /// `Transformer.from_crs("EPSG:4326", "EPSG:4087", always_xy=True).transform(10, 55)`.
    #[test]
    fn equidistant_cylindrical_on_an_ellipsoid() {
        check(4087, (10.0, 55.0), (1_113_194.907_932_735_7, 6_097_230.313_124_184), 1e-6);
        check(4087, (0.0, 45.0), (0.0, 4_984_944.377_977_743), 1e-6);
        let t = Crs::from_epsg(4087).unwrap().transformer().unwrap();
        assert_eq!(format!("{t:?}"), "Transformer(eqc)");
        // ESRI World Equidistant Cylindrical's true scale at 60° (PROJ 9.8.1 the same way).
        let world = Crs::parse("+proj=eqc +lat_ts=60 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs").unwrap();
        let m = world.transformer().unwrap().from_lonlat(Coord::new(1.0, 45.0)).unwrap();
        assert!((m.x - 55_800.001_572_436_14).abs() < 1e-6 && (m.y - 4_984_944.377_977_743).abs() < 1e-6, "{m:?}");
        // On a sphere it is the spherical formula (IAU_2015:49910, Mars).
        for sphere in ["+proj=eqc +lat_ts=0 +lon_0=0 +R=3396190 +units=m +no_defs", "+proj=eqc +lon_0=0 +a=3396190 +b=3396190 +no_defs"] {
            let t = Crs::parse(sphere).unwrap().transformer().unwrap();
            let m = t.from_lonlat(Coord::new(77.45, 18.44)).unwrap();
            assert!((m.x - 3_396_190.0 * 77.45f64.to_radians()).abs() < 1e-6 && (m.y - 3_396_190.0 * 18.44f64.to_radians()).abs() < 1e-6);
        }
    }

    /// The datum of an ellipsoidal equidistant cylindrical system is kept: its prime meridian, its
    /// shift to WGS 84, its unit and its origin.
    #[test]
    fn equidistant_cylindrical_keeps_its_datum() {
        let base = "+proj=eqc +lat_ts=30 +lat_0=10 +lon_0=10 +x_0=1000 +y_0=2000 +ellps=GRS80";
        let t = |extra: &str| Crs::parse(&format!("{base} {extra} +no_defs")).unwrap().transformer().unwrap();
        let m = Coord::new(800_000.0, 3_000_000.0);
        let plain = t("+units=m").to_lonlat(m).unwrap();
        // Round trips.
        let back = t("+units=m").from_lonlat(plain).unwrap();
        assert!((back.x - m.x).abs() < 1e-6 && (back.y - m.y).abs() < 1e-6, "{back:?}");
        // The origin: lat_0 10°N, lon_0 10°E at the false origin.
        let o = t("+units=m").to_lonlat(Coord::new(1000.0, 2000.0)).unwrap();
        assert!((o.x - 10.0).abs() < 1e-12 && (o.y - 10.0).abs() < 1e-12, "{o:?}");
        // Paris's meridian moves longitudes 2.337 229 167° east, and nothing else.
        let paris = t("+units=m +pm=paris").to_lonlat(m).unwrap();
        assert!((paris.x - plain.x - 2.337_229_166_666_667).abs() < 1e-9 && (paris.y - plain.y).abs() < 1e-12, "{paris:?}");
        // Coordinates in feet are the same place as their metres (the false origin stays metres).
        let feet = t("+units=ft").to_lonlat(Coord::new(m.x / 0.3048, m.y / 0.3048)).unwrap();
        assert!((feet.x - plain.x).abs() < 1e-9 && (feet.y - plain.y).abs() < 1e-9, "{feet:?}");
        let back = t("+units=ft").from_lonlat(feet).unwrap();
        assert!((back.x - m.x / 0.3048).abs() < 1e-5 && (back.y - m.y / 0.3048).abs() < 1e-5, "{back:?}");
        // A shift to WGS 84 is applied as to longitude/latitude on that datum.
        let shifted = t("+units=m +towgs84=100,-50,30").to_lonlat(m).unwrap();
        let geo = Crs::parse("+proj=longlat +ellps=GRS80 +towgs84=100,-50,30 +no_defs").unwrap().transformer().unwrap();
        let p = geo.to_lonlat(plain).unwrap();
        assert!((shifted.x - p.x).abs() < 1e-12 && (shifted.y - p.y).abs() < 1e-12, "{shifted:?} {p:?}");
        assert!((shifted.x - plain.x).abs() + (shifted.y - plain.y).abs() > 1e-4, "no shift: {shifted:?}");
    }

    /// Every parameter of a long definition is read: a latitude of true scale and a datum shift
    /// after 64 others place points as they do at its start.
    #[test]
    fn equidistant_cylindrical_reads_a_long_definition_whole() {
        let padding = "+no_defs ".repeat(70);
        let tail = "+lat_ts=60 +towgs84=100,-50,30";
        let (short, long) = (format!("+proj=eqc +ellps=GRS80 {tail}"), format!("+proj=eqc +ellps=GRS80 {padding}{tail}"));
        let lonlat = Coord::new(10.0, 45.0);
        let at = |def: &str| Crs::parse(def).and_then(Crs::transformer).unwrap_or_else(|e| panic!("{e}")).from_lonlat(lonlat).unwrap();
        let (a, b) = (at(&short), at(&long));
        assert!((a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9, "{a:?} {b:?}");
        // And that is with the scale at 60°: half the equator's easting.
        let at_equator = at("+proj=eqc +ellps=GRS80 +towgs84=100,-50,30");
        assert!((a.x / at_equator.x - 0.5).abs() < 0.01, "{a:?} {at_equator:?}");
    }

    /// Definitions it can't compute are refused, saying why, and hostile ones never panic.
    #[test]
    fn equidistant_cylindrical_hostile_definitions() {
        // What PROJ refuses (radians are read: `radians_are_read_as_degrees`).
        for refused in ["+proj=eqc +ellps=WGS84 +lat_ts=abc", "+proj=eqc +ellps=WGS84 +lat_ts=NaN", "+proj=eqc +ellps=WGS84 +to_meter=0"] {
            assert!(Crs::parse(refused).is_err(), "{refused}");
        }
        // Degrees and minutes, other axes and +over, as PROJ 9.8.1 takes them (pyproj 3.8.0, from
        // `+proj=longlat +R=3396190`: 20°E 10°N is -1185493.95, -592746.98 with west/south axes).
        for (def, lon, x, y) in [
            ("+proj=eqc +ellps=WGS84 +lat_ts=30d30", 45.0, 4_319_951.728_809_517, 1_105_854.833_234_372),
            ("+proj=eqc +R=3396190 +over +no_defs", 200.0, -9_483_951.603_728_997, 592_746.975_233_062_2),
            ("+proj=eqc +R=3396190 +axis=wsu +no_defs", 20.0, -1_185_493.950_466_124_3, -592_746.975_233_062_2),
        ] {
            let t = Crs::parse(def).and_then(Crs::transformer).unwrap_or_else(|e| panic!("{def}: {e}"));
            let at = t.from_lonlat_unchecked(Coord::new(lon, 10.0)).unwrap();
            assert!((at.x - x).abs() < 1e-6 && (at.y - y).abs() < 1e-6, "{def}: {at:?}");
        }
        // On a datum as flat as Jupiter's too (PROJ 9.8.1: 12477707.89, 50867500.11).
        let t = Crs::parse("+proj=eqc +a=71492000 +b=66854000 +no_defs").unwrap().transformer().unwrap();
        let at = t.from_lonlat(Coord::new(10.0, 45.0)).unwrap();
        assert!((at.x - 12_477_707.888_357_861).abs() < 1e-6 && (at.y - 50_867_500.106_014_006).abs() < 1e-6, "{at:?}");
        assert!(Crs::parse("+proj=eqc +a=3396190 +b=3376200 +no_defs").is_ok(), "Mars");
        for odd in [
            "+proj=eqc +a=1e308 +rf=1.0000001",
            "+proj=eqc +a=-1 +b=5",
            "+proj=eqc +a=6378137 +b=1",
            "+proj=eqc +ellps=WGS84 +x_0=1e308 +y_0=-1e308",
            "+proj=eqc +ellps=WGS84 +lat_0=89.999999 +lat_ts=-89.9999999",
            "+proj=eqc +ellps=WGS84 +lon_0=1e300",
        ] {
            if let Ok(t) = Crs::parse(odd).and_then(Crs::transformer) {
                for c in [Coord::new(0.0, 0.0), Coord::new(f64::NAN, 1.0), Coord::new(1e308, -1e308), Coord::new(f64::INFINITY, 0.0)] {
                    let _ = t.to_lonlat(c);
                    let _ = t.from_lonlat(c);
                }
            }
        }
    }

    /// Projects `lonlat` with EPSG `code` and checks it against `want` (metres, or degrees for
    /// a geographic system), then back.
    fn check(code: u32, lonlat: (f64, f64), want: (f64, f64), tolerance: f64) {
        let crs = Crs::from_epsg(code).unwrap_or_else(|| panic!("EPSG:{code} unknown"));
        assert_eq!(crs, Crs::Epsg(code as u16));
        assert_eq!(crs.label(), format!("EPSG:{code}"));
        let t = crs.transformer().unwrap_or_else(|e| panic!("EPSG:{code}: {e}"));
        let p = t.from_lonlat(Coord::new(lonlat.0, lonlat.1)).unwrap();
        assert!((p.x - want.0).abs() < tolerance && (p.y - want.1).abs() < tolerance, "EPSG:{code}: {p:?}, want {want:?}");
        let back = t.to_lonlat(p).unwrap();
        assert!((back.x - lonlat.0).abs() < 1e-7 && (back.y - lonlat.1).abs() < 1e-7, "EPSG:{code}: {back:?}");
    }

    /// An ESRI code is transformed as PROJ 9.9.0 transforms it (network off,
    /// `Transformer.from_crs("EPSG:4326", "ESRI:<code>", always_xy=True)`), its datum
    /// transformation included, by the WKT terrio-crs's catalog gives it: Merchich UTM zone 28N
    /// (ESRI:102144) in the Canary Islands, 246 m from the Helmert the bundled table wrote, and
    /// Germany zone 1 in the North Sea, past the DHDN Helmert's area, where PROJ goes by a
    /// ballpark that the projection's round trip once refused.
    #[test]
    fn esri_codes_place_points_as_proj_9_9_0() {
        for (code, (lon, lat), (x, y)) in
            [(102144u32, (-15.0, 28.34), (500_000.000, 3_134_617.542)), (31491, (3.925, 55.58), (1_558_329.555, 6_161_550.584))]
        {
            let crs = esri_crs(code).unwrap();
            assert_eq!(crs.label(), format!("ESRI:{code}"));
            let at = crs.transformer().unwrap().from_lonlat(Coord::new(lon, lat)).unwrap();
            assert!((at.x - x).abs() < 0.01 && (at.y - y).abs() < 0.01, "ESRI:{code}: {at:?}");
            // A project with it needs its WKT where the PROJ definition older releases read places
            // points elsewhere (Merchich, 246 m; DHDN past its Helmert's area), as for any other
            // system.
            assert!(crs.custom_def().unwrap().wkt_needed(), "ESRI:{code}");
        }
        // One whose PROJ definition places points as its WKT does (World Robinson, on WGS 84)
        // doesn't need it: older releases read it as this one does.
        assert!(!esri_crs(54030).unwrap().custom_def().unwrap().wkt_needed());
        // Projected codes the bundled table lacked are the catalog's (World Miller Cylindrical),
        // and geographic ones aren't ESRI projected systems.
        assert!(esri_proj4(54003).is_some_and(|d| d.contains("+proj=mill")));
        assert!(esri_codes().count() > 2000);
        assert_eq!(esri_proj4(104000), None);
    }

    #[test]
    fn axis_order_as_epsg_defines_it() {
        // Latitude first; northing first (LAEA and LCC Europe, Gauss-Krüger, SWEREF99 TM, Poland's
        // 1992, Austria's Lambert, ETRS89 UTM (N-E), UPS North and South, RSPS2000), and a code newer than
        // the table RustGIS kept before (EPSG:10952).
        for code in [4326, 4258, 4269, 3035, 3034, 31466, 31467, 31468, 31469, 3006, 2180, 31287, 3040, 32661, 32761, 5482, 10952] {
            assert_eq!(epsg_north_first(code), Some(true), "EPSG:{code}");
        }
        // Easting first: Web Mercator, LV95 and LV03, ETRS89 UTM, WGS 84 UTM, a State Plane in feet,
        // Krovak East North; westing first (Lo15, `+axis=wsu`); polar stereographic, north and
        // south; southing first but turned so by its PROJ string (`+axis=swu`: 8352, 5513); Equi7
        // Europe (aeqd); a code PROJ dropped (EPSG:3752).
        for code in [3857, 2056, 21781, 25832, 25833, 32633, 2264, 5514, 2046, 3031, 3413, 8352, 5513, 27704, 3752] {
            assert_eq!(epsg_north_first(code), Some(false), "EPSG:{code}");
        }
        // Southing then westing, which swapping can't give (the Gusterberg and St. Stephen grids);
        // a code that doesn't exist.
        for code in [8044, 8045, 1] {
            assert_eq!(epsg_north_first(code), None, "EPSG:{code}");
        }
    }

    /// EPSG codes PostGIS's table lacked, which RustGIS read from its own gap table before
    /// (opengeos/RustGIS#198), from terrio-crs's catalog; the expected coordinates are pyproj's.
    #[test]
    fn codes_postgis_lacked() {
        // SIRGAS-Chile 2016 / UTM zone 19S: Santiago de Chile.
        check(9155, (-70.65, -33.45), (346_642.695, 6_297_606.833), 0.01);
        assert!(!Crs::from_epsg(9155).unwrap().is_geographic());
        // SIRGAS-Chile 2016 / UTM zone 18S (Valdivia), and the 2013 realisation's zones.
        check(9154, (-73.0, -40.0), (670_725.494, 5_570_327.027), 0.01);
        check(9149, (-73.0, -40.0), (670_725.494, 5_570_327.027), 0.01);
        check(9150, (-70.65, -33.45), (346_642.695, 6_297_606.833), 0.01);
        // SIRGAS-Chile 2013 and 2016 themselves are in degrees.
        for code in [9148, 9153] {
            assert!(Crs::from_epsg(code).unwrap().is_geographic(), "EPSG:{code}");
            check(code, (-70.65, -33.45), (-70.65, -33.45), 1e-9);
        }
        // MAGNA-SIRGAS 2018 / Origen-Nacional (Colombia): Bogotá.
        check(9377, (-74.08, 4.65), (4_880_256.854, 2_071_911.388), 0.01);
        assert_eq!(Crs::parse("EPSG:9377"), Ok(Crs::Epsg(9377)));
        assert_eq!(Crs::parse("urn:ogc:def:crs:EPSG::9377"), Ok(Crs::Epsg(9377)));
        // VN-2000 UTM zones 48N and 49N (opengeos/RustGIS#684)
        assert_eq!(Crs::parse("EPSG:3405"), Ok(Crs::Epsg(3405)));
        assert_eq!(Crs::parse("EPSG:3406"), Ok(Crs::Epsg(3406)));
        for bare in ["VN-2000", "VN2000", "vn-2000", "vn2000", "  VN-2000  ", "VN 2000", "vn_2000"] {
            let err = Crs::parse(bare).unwrap_err();
            assert!(err.contains("ambiguous"), "{bare}: {err}");
        }
        for name in [
            "VN-2000 / UTM zone 48N",
            "VN2000 / UTM zone 48N",
            "VN-2000 / UTM 48N",
            "VN2000 / UTM 48N",
            "VN-2000-48",
            "VN2000-48",
            "vn-2000 / utm zone 48n",
            "vn2000 / utm 48n",
            "  VN-2000 / UTM 48N  ",
        ] {
            let crs = Crs::parse(name).unwrap();
            assert_eq!(crs, Crs::Epsg(3405), "{name}");
            assert_eq!(crs.epsg(), 3405, "{name}");
            assert!(!crs.is_geographic(), "{name}");
        }
        for name in [
            "VN-2000 / UTM zone 49N",
            "VN2000 / UTM zone 49N",
            "VN-2000 / UTM 49N",
            "VN2000 / UTM 49N",
            "VN-2000-49",
            "VN2000-49",
            "vn-2000 / utm zone 49n",
            "vn2000 / utm 49n",
            "  VN-2000 / UTM 49N  ",
        ] {
            let crs = Crs::parse(name).unwrap();
            assert_eq!(crs, Crs::Epsg(3406), "{name}");
            assert_eq!(crs.epsg(), 3406, "{name}");
            assert!(!crs.is_geographic(), "{name}");
        }
        for name in [
            "VN-2000 / TM-3",
            "VN2000 / TM-3",
            "VN-2000 / TM3",
            "VN2000 / TM3",
            "VN-2000-TM3",
            "VN2000-TM3",
            "vn2000tm3",
            "VN-2000 TM-3",
            "vn-2000 / tm-3",
            "  VN-2000 / TM-3  ",
        ] {
            let crs = Crs::parse(name).unwrap();
            assert_eq!(crs, Crs::Epsg(5897), "{name}");
            assert!(!crs.is_geographic(), "{name}");
            let def = epsg_proj4(5897).unwrap();
            assert!(def.contains("+proj=tmerc") && def.contains("+lon_0=105") && def.contains("+k=0.9999"), "{name}");
            assert!(def.contains("+x_0=500000") && def.contains(VN2000_TOWGS84), "{name}");
            let tr = crs.transformer().unwrap();
            let pt = Coord::new(105.8525, 21.0285);
            let proj_pt = tr.from_lonlat(pt).unwrap();
            assert!(proj_pt.x > 500_000.0 && proj_pt.x < 700_000.0);
            assert!(proj_pt.y > 2_000_000.0 && proj_pt.y < 2_500_000.0);
            let unproj_pt = tr.to_lonlat(proj_pt).unwrap();
            assert!((unproj_pt.x - pt.x).abs() < 1e-6 && (unproj_pt.y - pt.y).abs() < 1e-6);
        }
        // Numeric EPSG codes for VN-2000 UTM 48N/49N
        assert_eq!(Crs::parse("3405"), Ok(Crs::Epsg(3405)));
        assert_eq!(Crs::parse("3406"), Ok(Crs::Epsg(3406)));
        assert_eq!(Crs::parse(" 3405 "), Ok(Crs::Epsg(3405)));
        assert_eq!(Crs::parse("epsg:3405"), Ok(Crs::Epsg(3405)));
        assert_eq!(Crs::parse("EPSG:3406"), Ok(Crs::Epsg(3406)));
        assert_eq!(Crs::parse("urn:ogc:def:crs:EPSG::3405"), Ok(Crs::Epsg(3405)));
        assert_eq!(Crs::parse("urn:ogc:def:crs:EPSG::3406"), Ok(Crs::Epsg(3406)));

        // Coordinate transformation for EPSG:3405 and EPSG:3406
        let crs48 = Crs::from_epsg(3405).unwrap();
        let tr48 = crs48.transformer().unwrap();
        let pt48 = Coord::new(105.8525, 21.0285);
        let proj48 = tr48.from_lonlat(pt48).unwrap();
        assert!(proj48.x > 500_000.0 && proj48.x < 700_000.0);
        assert!(proj48.y > 2_000_000.0 && proj48.y < 2_500_000.0);
        let unproj48 = tr48.to_lonlat(proj48).unwrap();
        assert!((unproj48.x - pt48.x).abs() < 1e-6 && (unproj48.y - pt48.y).abs() < 1e-6);

        let crs49 = Crs::from_epsg(3406).unwrap();
        let tr49 = crs49.transformer().unwrap();
        let pt49 = Coord::new(109.1967, 12.2388);
        let proj49 = tr49.from_lonlat(pt49).unwrap();
        assert!(proj49.x > 200_000.0 && proj49.x < 400_000.0);
        assert!(proj49.y > 1_300_000.0 && proj49.y < 1_500_000.0);
        let unproj49 = tr49.to_lonlat(proj49).unwrap();
        assert!((unproj49.x - pt49.x).abs() < 1e-6 && (unproj49.y - pt49.y).abs() < 1e-6);

        // Over-broad UTM matching and unrelated CRS names must NOT match VN-2000
        assert!(Crs::parse("WGS 84 / UTM zone 48N").is_err());
        assert!(Crs::parse("WGS 84 / UTM zone 49N").is_err());
        assert!(Crs::parse("NAD83 / UTM zone 48N").is_err());
        assert!(Crs::parse("NAD83 / UTM zone 49N").is_err());
        assert!(Crs::parse("VN-2000 CUSTOM UTM ZONE 48N").is_err());
        assert!(Crs::parse("VN2000 EXTRA TEXT UTM ZONE 49N").is_err());
        assert!(Crs::parse("VN-2000 UTM 50N").is_err());
        assert!(Crs::parse("VN2000XYZ").is_err());
        assert!(Crs::parse("VN-2000 unknown").is_err());

        // Shared bare VN-2000 helper verification
        assert!(is_bare_vn2000("VN-2000"));
        assert!(is_bare_vn2000("vn2000"));
        assert!(is_bare_vn2000("VN 2000"));
        assert!(is_bare_vn2000("vn_2000"));
        assert!(!is_bare_vn2000("VN-2000 / UTM zone 48N"));
        assert!(!is_bare_vn2000("vn2000tm3"));
        assert!(!is_bare_vn2000("WGS 84"));
        assert!(bare_vn2000_error("VN-2000").contains("ambiguous"));

        // Embedded EPSG definitions verification
        let p3405 = epsg_proj4(3405).expect("EPSG:3405");
        assert!(p3405.contains("+proj=utm") && p3405.contains("+zone=48"), "{p3405}");
        let p3406 = epsg_proj4(3406).expect("EPSG:3406");
        assert!(p3406.contains("+proj=utm") && p3406.contains("+zone=49"), "{p3406}");
        assert_eq!(Crs::from_epsg(65534), None);
        assert!(Crs::parse("EPSG:65534").is_err());

        // PROJ string validation: case normalization, prefix validation, and size limit
        let custom_proj = Crs::parse("+proj=utm +zone=48 +ellps=WGS84").unwrap();
        assert_eq!(custom_proj.label(), "Custom PROJ");
        let custom_proj_upper = Crs::parse("+PROJ=utm +zone=48 +ellps=WGS84").unwrap();
        assert_eq!(custom_proj_upper.label(), "Custom PROJ");

        // Custom CRS deduplication
        let custom_dup = Crs::parse("+proj=utm +zone=48 +ellps=WGS84").unwrap();
        assert_eq!(custom_proj, custom_dup);

        // Security validation: unsafe parameter forms (+init, +nadgrids, path traversal)
        assert!(Crs::parse("+proj=utm +zone=48 +init=epsg:4326").is_err());
        assert!(Crs::parse("+proj=utm +zone=48 +nadgrids=@null").is_err());
        assert!(Crs::parse("+proj=utm +zone=48 +file=../secret").is_err());
        assert!(Crs::parse("+proj=utm +zone=48 +file=C:\\secret").is_err());
        assert!(Crs::parse("+proj=utm +zone=33 +geoidgrids=egm96_15.gtx").is_err());
        // Any key naming grids (legacy `vgrids`, `deformation`'s `xy_grids`), pipelines and steps.
        for bad in [
            "+proj=utm +zone=33 +vgrids=egm96_15.gtx",
            "+proj=deformation +xy_grids=a.tif +z_grids=b.tif",
            "+proj=utm +zone=33 +gridlist=x",
            "+proj=pipeline +step +proj=utm +zone=33",
            "+PROJ=Pipeline +step +proj=utm +zone=33",
        ] {
            assert!(Crs::parse(bad).is_err(), "{bad}");
        }
        // A file's own definition may carry `+nadgrids=@null` (Web Mercator as GDAL writes it).
        let merc = "+proj=merc +a=6378137 +b=6378137 +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +k=1 +units=m +nadgrids=@null +wktext +no_defs";
        let file =
            CustomCrs { proj4: merc.into(), label: "m".into(), geokeys: Vec::new(), doubles: Vec::new(), ascii: String::new(), wkt: None };
        assert!(Crs::custom(file).is_ok());

        let long_proj = format!("+proj={}", "a".repeat(1025));
        let long_utm = format!("+proj=utm {}", "a".repeat(1025));
        let rejected = [
            "+proj",
            "+project",
            "+proj=",
            "+proj=   ",
            "+proj=invalid_projection",
            "+PROJ=invalid_projection",
            long_proj.as_str(),
            long_utm.as_str(),
        ];
        for r in rejected {
            assert!(Crs::parse(r).is_err(), "{r}");
        }
        // None of them was registered (an exact check: other tests add to the table meanwhile).
        let table = custom_table().lock().unwrap_or_else(|e| e.into_inner());
        assert!(!table.iter().any(|d| rejected.iter().any(|r| d.proj4.eq_ignore_ascii_case(r.trim()))));
        drop(table);
        // VN-2000 / TM-3 103-00: its datum shift (+towgs84) is applied.
        check(9205, (103.065, 21.855), (506_521.959, 2_417_609.479), 1.0);
        // NAD27 / US National Atlas Equal Area: Washington, DC, with NAD27's datum shift as PROJ
        // 9.8.1 applies it without grids ("NAD27 to WGS 84 (4)"; pyproj 3.8.0, network off,
        // `Transformer.from_crs("EPSG:4326", "EPSG:9311", always_xy=True)`).
        check(9311, (-77.04, 38.89), (1_958_660.023, -405_359.527), 0.01);
        // Codes the bundled tables lacked, from terrio-crs's catalog, against PROJ 9.9.0 (network
        // off, `Transformer.from_crs("EPSG:4326", "EPSG:<code>", always_xy=True)`): EPSG:10622,
        // an orthographic with an azimuth and a scale (in US survey feet), at San Francisco
        // airport, and EPSG:5225, Modified Krovak East North on Ferro, in Prague.
        check(10622, (-122.39, 37.62), (2_532.630, -2_356.763), 0.01);
        check(5225, (14.42, 50.09), (-5_742_860.106, -6_042_721.337), 0.01);
        // EPSG:6247, MAGNA-SIRGAS / Bogota urban grid (Colombia Urban), in Bogotá.
        check(6247, (-74.08, 4.6), (99_727.065, 100_417.445), 0.01);
        // A vertical system alone isn't a place's (EGM2008 height, NAVD88 height in feet).
        assert_eq!(Crs::from_epsg(3855), None);
        assert_eq!(Crs::from_epsg(6360), None);
        // The Equi7 grids, above the gap (azimuthal equidistant on WGS 84), against PROJ 9's
        // cs2cs: a capital, or McMurdo, in each continent's system.
        for (code, lonlat, xy) in [
            (27701, (36.82, -1.29), (7334614.4467, 4929069.6856)),
            (27702, (166.67, -77.85), (4027107.7883, 2081695.0307)),
            (27703, (116.40, 39.90), (6237986.3076, 4287997.5336)),
            (27704, (16.37, 48.21), (5270556.2083, 1618120.6818)),
            (27705, (-77.04, 38.89), (10035658.3291, 3647233.1)),
            (27706, (151.21, -33.87), (8823663.5437, 5930910.0748)),
            (27707, (-47.88, -15.79), (8608845.8731, 5356292.7056)),
        ] {
            check(code, lonlat, xy, 0.01);
        }
        assert_eq!(Crs::parse("EPSG:27707"), Ok(Crs::Epsg(27707)));
        // Azimuthal equidistant works in every build; a code that doesn't exist is unknown.
        assert_eq!(unusable_epsg(28707), "the coordinate system EPSG:28707 isn't supported");
    }

    /// The ESRI codes (opengeos/RustGIS#213) are in order and only projected systems on Earth.
    #[test]
    fn esri_codes_are_sound() {
        let codes: Vec<u32> = esri_codes().collect();
        for pair in codes.windows(2) {
            assert!(pair[0] < pair[1], "out of order at ESRI:{}", pair[1]);
        }
        let defs: Vec<(u32, &str)> = codes.iter().filter_map(|&c| Some((c, esri_proj4(c)?))).collect();
        assert!(defs.len() > 2000, "{}", defs.len());
        // Every catalog code's WKT, which places its points, is transformed (else [`esri_crs`]
        // falls back to the PROJ definition).
        for &c in &codes {
            if let Some((_, wkt)) = catalog_esri(c) {
                assert!(build_wkt(wkt).is_ok(), "ESRI:{c}");
            }
        }
        for &(code, def) in &defs {
            assert!(def.starts_with("+proj=") && !def.contains("+proj=longlat") && !def.contains("+proj=geocent"), "ESRI:{code}");
            assert!(!def.contains("+type=crs"), "ESRI:{code}");
        }
        // South America Albers Equal Area Conic, which opened the issue.
        assert!(esri_proj4(102033).is_some_and(|d| d.contains("+proj=aea") && d.contains("+lon_0=-60")));
        // A datum PROJ 9.9.0 goes from by a ballpark (Roma 1940, ESRI:102093) gets no shift.
        assert!(esri_proj4(102093).is_some_and(|d| d.contains("+ellps=intl") && !d.contains("+towgs84")));
        // One whose geographic system has a single Helmert gets PROJ's (OSGB 1936, ESRI:102799,
        // as GDAL 3.13.3 and PROJ 9.9.0 write it).
        assert!(esri_proj4(102799).is_some_and(|d| d.contains("+towgs84=446,-99,544,0.945,0.261,0.435,-20.8927 ")));
        // Tokyo UTM zone 51N, which RustGIS kept itself before terrio-crs had it, as PROJ 9.9.0
        // writes it (`projinfo -o PROJ ESRI:102151`).
        assert_eq!(esri_proj4(102151), Some("+proj=utm +zone=51 +ellps=bessel +towgs84=-147.54,507.26,680.47,0,0,0,0 +units=m +no_defs"));
        // Left out: a lunar system, a Hammer–Aitoff PROJ can't write as a string, and codes that
        // don't exist.
        for code in [103877, 54044, 0, 4326, u32::MAX] {
            assert_eq!(esri_proj4(code), None, "ESRI:{code}");
        }
    }

    /// Codes terrio-crs's catalog doesn't compute that RustGIS read before still read, by the
    /// definitions they had, and the catalog doesn't have them now (else they'd go).
    #[test]
    fn uncomputed_codes_still_read() {
        for &(code, def) in UNCOMPUTED_EPSG {
            assert!(catalog_proj4(code).is_none(), "the catalog has EPSG:{code} now: drop it from the list");
            assert_eq!(epsg_proj4(code), Some(def));
            let crs = Crs::from_epsg(u32::from(code)).unwrap_or_else(|| panic!("EPSG:{code}"));
            crs.transformer().unwrap_or_else(|e| panic!("EPSG:{code}: {e}"));
        }
        for &(code, def) in UNCOMPUTED_ESRI {
            assert!(catalog_esri(code).is_none(), "the catalog has ESRI:{code} now: drop it from the list");
            assert_eq!(esri_proj4(code), Some(def));
            assert!(esri_codes().any(|c| c == code), "ESRI:{code}");
            esri_crs(code).unwrap_or_else(|| panic!("ESRI:{code}")).transformer().unwrap_or_else(|e| panic!("ESRI:{code}: {e}"));
        }
    }

    /// EPSG:11476 (SIRGAS-Chile 2025 / UTM zone 19S, EPSG v13.102, opengeos/RustGIS#2335) reads
    /// and places points as PROJ 9.9.0 does. Reference: PROJ 9.9.0 (pyproj 3.8.0 built against it,
    /// PROJ_NETWORK=OFF), `Transformer.from_crs("EPSG:4326", "EPSG:11476", always_xy=True)` at
    /// (-70.6, -33.45): (351290.3712547213, 6297679.510189325). Its only transformation to WGS 84
    /// is time-specific, so PROJ uses a ballpark one, and so does RustGIS.
    #[test]
    fn sirgas_chile_2025_places_points_as_proj_9_9_0() {
        let crs = Crs::from_epsg(11476).expect("EPSG:11476");
        assert_eq!(crs.label(), "EPSG:11476");
        let p = crs.transformer().unwrap().from_lonlat(Coord::new(-70.6, -33.45)).unwrap();
        assert!((p.x - 351290.3712547213).abs() < 1e-6 && (p.y - 6297679.510189325).abs() < 1e-6, "{p:?}");
    }

    /// [`Crs::needs_wkt`] matches [`saves_wkt`] without serializing.
    #[test]
    fn crs_needs_wkt_matches_saves_wkt() {
        let grads = r#"GEOGCS["NTF (Paris)",DATUM["Nouvelle_Triangulation_Francaise_Paris",SPHEROID["Clarke 1880 (IGN)",6378249.2,293.466021293627,AUTHORITY["EPSG","7011"]],AUTHORITY["EPSG","6807"]],PRIMEM["Paris",2.33722917,AUTHORITY["EPSG","8903"]],UNIT["grad",0.0157079632679489,AUTHORITY["EPSG","9105"]]]"#;
        let ntf_grads = crate::raster::wkt::crs_from_wkt(grads).unwrap();
        let merchich = esri_crs(102144).unwrap();
        let robinson = esri_crs(54030).unwrap();
        let custom_proj = Crs::parse("+proj=robin +lon_0=0 +datum=WGS84 +units=m +no_defs").unwrap();

        for (crs, expected) in [
            (Crs::LonLat, false),
            (Crs::WebMercator, false),
            (Crs::Epsg(2056), false),
            (Crs::Custom(u32::MAX), false),
            (custom_proj, false),
            (robinson, false),
            (ntf_grads, true),
            (merchich, true),
        ] {
            assert_eq!(crs.needs_wkt(), expected, "{crs:?}");
            assert_eq!(crs.needs_wkt(), saves_wkt(&crs), "{crs:?}");
        }
    }
}
