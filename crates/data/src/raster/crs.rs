//! Coordinate reference systems for rasters, by EPSG code or by definition.
//!
//! WGS 84 longitude/latitude and Web Mercator are handled directly; any other EPSG code is
//! looked up in a built-in table of PROJ definitions ([`epsg_proj4`]) and transformed with a
//! pure-Rust PROJ port, so the browser build works too. A file's own (user-defined) projection,
//! such as the Albers of the NLCD land cover, is a [`Crs::Custom`]: its PROJ definition, kept in a
//! table for the run.

use rustgis_geom::{Coord, lonlat_to_merc, merc_to_lonlat};
use serde::{Deserialize, Serialize};

/// The PROJ definition of an EPSG code: from the `crs-definitions` table, or from the built-in
/// table of the codes it lacks (EPSG 9020–20003 and newer ones such as Equi7's,
/// [`super::epsg_gap`]). `None` for a code that is neither, or whose projection this build lacks
/// (an azimuthal equidistant one in the browser: see [`aeqd_supported`]).
pub fn epsg_proj4(code: u16) -> Option<&'static str> {
    listed_epsg(code).filter(|d| aeqd_supported() || !d.contains("+proj=aeqd"))
}

fn listed_epsg(code: u16) -> Option<&'static str> {
    crs_definitions::from_code(code).map(|d| d.proj4).or_else(|| super::epsg_gap::proj4(code))
}

/// Whether proj4rs' azimuthal equidistant projection (`aeqd`, Equi7's) is in this build. The
/// browser build leaves it out (Cargo.toml); asked of proj4rs itself, as rustgis-map's
/// `local_views_supported` does, so the lookup and the tests stay in step with the build.
pub fn aeqd_supported() -> bool {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(|| proj4rs::Proj::from_proj_string("+proj=aeqd +lat_0=0 +lon_0=0 +datum=WGS84 +units=m +no_defs").is_ok())
}

/// Why a file's EPSG code can't be used: a code the desktop app knows but this build can't project
/// (Equi7's azimuthal equidistant ones in the web app) says so, rather than that it's unknown.
pub fn unusable_epsg(code: u32) -> String {
    let elsewhere = u16::try_from(code).ok().and_then(listed_epsg).is_some_and(|d| d.contains("+proj=aeqd")) && !aeqd_supported();
    if elsewhere {
        format!("the coordinate system EPSG:{code} (azimuthal equidistant) can't be used in the web app: open the file in the desktop app")
    } else {
        format!("the coordinate system EPSG:{code} isn't supported")
    }
}

/// The PROJ definition of one of Esri's projected coordinate systems (`ESRI:102033`, South
/// America Albers), from the built-in table ([`super::esri`]). `None` for a code it lacks.
pub fn esri_proj4(code: u32) -> Option<&'static str> {
    super::esri::proj4(code)
}

/// Every ESRI code [`esri_proj4`] knows, in order.
pub fn esri_codes() -> impl Iterator<Item = u32> {
    super::esri::DEFS.iter().map(|&(c, _)| c)
}

/// A raster's coordinate system. Saved as its EPSG code, or a custom one as its definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
/// table.
#[derive(Serialize, Deserialize)]
enum SavedCrs {
    LonLat,
    WebMercator,
    Epsg(u16),
    Proj { proj4: String, label: String },
}

impl From<Crs> for SavedCrs {
    fn from(c: Crs) -> Self {
        match c {
            Crs::LonLat => SavedCrs::LonLat,
            Crs::WebMercator => SavedCrs::WebMercator,
            Crs::Epsg(e) => SavedCrs::Epsg(e),
            Crs::Custom(_) => match c.custom_def() {
                Some(d) => SavedCrs::Proj { proj4: d.proj4.clone(), label: d.label.clone() },
                None => SavedCrs::LonLat,
            },
        }
    }
}

impl TryFrom<SavedCrs> for Crs {
    type Error = String;
    fn try_from(s: SavedCrs) -> Result<Self, String> {
        Ok(match s {
            SavedCrs::LonLat => Crs::LonLat,
            SavedCrs::WebMercator => Crs::WebMercator,
            SavedCrs::Epsg(e) => Crs::Epsg(e),
            SavedCrs::Proj { proj4, label } => {
                Crs::custom(CustomCrs { proj4, label, geokeys: Vec::new(), doubles: Vec::new(), ascii: String::new() })?
            }
        })
    }
}

/// A coordinate system a file defines itself.
#[derive(Clone, Debug, PartialEq)]
pub struct CustomCrs {
    /// The PROJ definition (`+proj=aea +lat_1=29.5 …`).
    pub proj4: String,
    /// What to call it: the file's citation, or the projection's name.
    pub label: String,
    /// The GeoTIFF keys it came from, written back when the raster is saved as a GeoTIFF: the key
    /// directory, its double parameters and its ASCII parameters.
    pub geokeys: Vec<u16>,
    pub doubles: Vec<f64>,
    pub ascii: String,
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
    /// A file's own coordinate system: checked (its PROJ definition must parse, or be a
    /// projection RustGIS computes: a sinusoidal one, or an equidistant cylindrical one on an
    /// ellipsoid) and added to the run's table, once per definition.
    pub fn custom(def: CustomCrs) -> Result<Crs, String> {
        Crs::register(def, false)
    }

    /// Adds a system to the run's table; one `typed` as a PROJ string counts against
    /// `MAX_TYPED` too, checked under the same lock as the insertion.
    fn register(def: CustomCrs, typed: bool) -> Result<Crs, String> {
        // As for an EPSG code ([`unusable_epsg`]): say where it can be used.
        let checked = if def.proj4.contains("+proj=aeqd") && !aeqd_supported() {
            Err("azimuthal equidistant can't be used in the web app: open the file in the desktop app".to_string())
        } else {
            transformer_of(&def.proj4).map(|_| ())
        };
        checked.map_err(|e| format!("{}: {e}", def.proj4))?;
        let mut t = custom_table().lock().unwrap_or_else(|e| e.into_inner());
        let index = |i: usize| u32::try_from(i).map(Crs::Custom).map_err(|_| "too many coordinate systems defined by files".to_string());
        if let Some(i) = t.iter().position(|d| d.proj4 == def.proj4) {
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

    /// A coordinate system given as text: `EPSG:2263`, `2263`, an OGC URN or URL ending in the
    /// code, WKT, a PROJ string (`+proj=…`, naming no grid or definition files), or an explicit
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
                CustomCrs { proj4, label: TYPED_LABEL.to_string(), geokeys: Vec::new(), doubles: Vec::new(), ascii: String::new() },
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
        let known = t.chars().all(|c| c.is_ascii_digit()) || up.contains("EPSG");
        match code.parse::<u32>() {
            Ok(c) if known => Crs::from_epsg(c).ok_or_else(|| format!("unknown coordinate system EPSG:{c}")),
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
            Crs::Epsg(c) => epsg_proj4(c).is_some_and(|d| d.contains("+proj=longlat")),
            Crs::Custom(_) => self.custom_def().is_some_and(|d| d.proj4.contains("+proj=longlat")),
        }
    }

    /// Whether its coordinates are geocentric (X, Y and Z from the Earth's centre), not a map's.
    pub(crate) fn is_geocentric(self) -> bool {
        match self {
            Crs::LonLat | Crs::WebMercator => false,
            Crs::Epsg(c) => epsg_proj4(c).is_some_and(|d| d.contains("+proj=geocent")),
            Crs::Custom(_) => self.custom_def().is_some_and(|d| d.proj4.contains("+proj=geocent")),
        }
    }

    /// The unit its heights are in, when that isn't metres (`us-ft`, `0.3048 m`): its vertical unit,
    /// else a projected system's linear unit, the only sign of its heights' unit a definition may
    /// give. A geographic system with no vertical unit says nothing of its heights. A name goes
    /// before a factor, as proj4rs reads them.
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
        let geographic = definition.contains("+proj=longlat") || definition.contains("+proj=latlong");
        stated("+vunits=", "+vto_meter=").or_else(|| if geographic { None } else { stated("+units=", "+to_meter=") }).flatten()
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
        let Some(def) = self.custom_def().filter(|_| !self.is_planetary()) else { return false };
        let keys: Vec<&str> = def.proj4.split_whitespace().collect();
        let has = |prefix: &str| keys.iter().any(|k| k.starts_with(prefix));
        let shifted = has("+towgs84=") || has("+datum=") || has("+nadgrids=");
        // Web Mercator's sphere, of WGS 84's major axis, is a projection's, not a datum's: in a
        // Mercator only, as a datum on that sphere elsewhere is unknown (and reading it says so).
        let axis = |key: &str| keys.iter().find_map(|k| k.strip_prefix(key)?.parse::<f64>().ok()).is_some_and(|v| v == 6_378_137.0);
        let sphere = keys.contains(&"+proj=merc") && (axis("+R=") || (axis("+a=") && axis("+b=")));
        let wgs84 = keys.contains(&"+ellps=WGS84") || keys.contains(&"+ellps=GRS80") || sphere;
        let ellipsoid = has("+ellps=") || has("+a=") || has("+R=");
        ellipsoid && !wgs84 && !shifted
    }

    /// Whether the system is another body's than the Earth's: a custom one whose ellipsoid's
    /// semi-major axis isn't the Earth's ([`super::planetary`]). Its longitude/latitude are read as
    /// they are, on the Earth.
    pub fn is_planetary(self) -> bool {
        self.custom_def().and_then(|d| super::planetary::proj4_axes(&d.proj4)).is_some_and(|(a, _)| super::planetary::off_earth(a))
    }

    /// A transformer between this system and WGS 84 longitude/latitude.
    pub fn transformer(self) -> Result<Transformer, String> {
        match self {
            Crs::LonLat => Ok(Transformer { kind: Kind::LonLat }),
            Crs::WebMercator => Ok(Transformer { kind: Kind::Merc }),
            Crs::Epsg(c) => {
                let def = epsg_proj4(c).ok_or_else(|| format!("unknown coordinate system EPSG:{c}"))?;
                transformer_of(def).map_err(|e| format!("EPSG:{c}: {e}"))
            }
            Crs::Custom(_) => {
                let def = self.custom_def().ok_or("an unknown custom coordinate system")?;
                transformer_of(&def.proj4).map_err(|e| format!("{}: {e}", def.label))
            }
        }
    }
}

/// A transformer between the system a PROJ definition describes and WGS 84 longitude/latitude.
/// The projections proj4rs lacks or computes only on a sphere are computed here: sinusoidal
/// (MODIS's grid), and equidistant cylindrical on an ellipsoid (EPSG:4087).
pub(crate) fn transformer_of(def: &str) -> Result<Transformer, String> {
    if let Some(sinu) = rustgis_geom::Sinusoidal::from_proj4(def) {
        return Ok(Transformer { kind: Kind::Sinusoidal(sinu?) });
    }
    if let Some(eqc) = ellipsoidal_eqc(def) {
        return eqc;
    }
    let native = proj4rs::Proj::from_proj_string(def).map_err(|e| e.to_string())?;
    let geographic = def.contains("+proj=longlat");
    Ok(Transformer { kind: Kind::Proj { native: Box::new(native), wgs84: Box::new(wgs84()?), geographic } })
}

fn wgs84() -> Result<proj4rs::Proj, String> {
    proj4rs::Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs").map_err(|e| e.to_string())
}

/// The transformer for a PROJ definition of an equidistant cylindrical projection on an
/// ellipsoid (EPSG:4087's), computed on it: `None` for any other definition, `eqc` on a sphere
/// included. A map drawn in such a system uses it too (rustgis-map's `Projector`), so the map,
/// the data read in the system and a raster drawn in its own coordinates all agree.
pub fn ellipsoidal_eqc(def: &str) -> Option<Result<Transformer, String>> {
    let eqc = EllipsoidalEqc::from_proj4(def)?;
    Some(eqc.and_then(|eqc| Ok(Transformer { kind: Kind::Eqc { eqc: Box::new(eqc), wgs84: Box::new(wgs84()?) } })))
}

/// Longitude/latitude on the datum (its ellipsoid, shift to WGS 84 and prime meridian) of a PROJ
/// definition of an equidistant cylindrical projection: every parameter but the projection's own.
/// proj4rs replaces the ellipsoid of the projection itself with a sphere of its semi-major axis.
pub(crate) fn eqc_geographic(def: &str) -> Result<proj4rs::Proj, String> {
    const PROJECTION: [&str; 13] =
        ["proj", "lat_ts", "lat_0", "lon_0", "x_0", "y_0", "units", "to_meter", "k", "k_0", "axis", "type", "wktext"];
    let datum: Vec<String> = def
        .split_whitespace()
        .map(|t| t.trim_start_matches('+'))
        .map(|t| t.split_once('=').unwrap_or((t, "")))
        .filter(|(k, _)| !PROJECTION.contains(k))
        .map(|(k, v)| if v.is_empty() { format!("+{k}") } else { format!("+{k}={v}") })
        .collect();
    proj4rs::Proj::from_proj_string(&format!("+proj=longlat {}", datum.join(" "))).map_err(|e| e.to_string())
}

/// An equidistant cylindrical projection on an ellipsoid (EPSG method 1028), which proj4rs
/// computes on a sphere of the semi-major axis only: the projection by
/// [`rustgis_geom::EquidistantCylindrical`], the datum (its ellipsoid, shift to WGS 84 and prime
/// meridian) by proj4rs, as longitude/latitude on that datum.
#[derive(Clone)]
struct EllipsoidalEqc {
    eqc: rustgis_geom::EquidistantCylindrical,
    /// Longitude/latitude on the projection's own datum and prime meridian.
    geo: proj4rs::Proj,
    /// Metres per unit of its coordinates.
    to_meter: f64,
}

impl EllipsoidalEqc {
    /// The projection a PROJ definition describes when it is `+proj=eqc` on an ellipsoid: `None`
    /// for any other projection, and for `eqc` on a sphere, which proj4rs computes exactly (with
    /// its axes and `+over`, as before); an error for one it can't compute (axes other than east
    /// and north, `+over`, a latitude of true scale that isn't a number of degrees, an ellipsoid
    /// flatter than [`rustgis_geom::eqc::MAX_ES`]).
    fn from_proj4(def: &str) -> Option<Result<EllipsoidalEqc, String>> {
        // Every parameter, however many: a datum shift or `+lat_ts` late in a long definition is
        // still the definition's.
        let tokens: Vec<(&str, &str)> =
            def.split_whitespace().map(|t| t.trim_start_matches('+')).map(|t| t.split_once('=').unwrap_or((t, ""))).collect();
        if !tokens.iter().any(|(k, v)| *k == "proj" && *v == "eqc") {
            return None;
        }
        let built = (|| {
            let native = proj4rs::Proj::from_proj_string(def).map_err(|e| e.to_string())?;
            let geo = eqc_geographic(def)?;
            let (a, b) = geo.ellipse_parameters();
            if !(a.is_finite() && b.is_finite() && a > 0.0 && b > 0.0) {
                return Err("the ellipsoid's axes aren't usable".into());
            }
            if ((a - b) / a).abs() < 1e-12 {
                return Ok(None);
            }
            if !native.is_normalized_axis() || tokens.iter().any(|(k, _)| *k == "over") {
                return Err(
                    "an equidistant cylindrical projection on an ellipsoid takes axes east and north only, with no +over".to_string()
                );
            }
            let es = 1.0 - (b / a) * (b / a);
            if es > rustgis_geom::eqc::MAX_ES {
                return Err(format!(
                    "an equidistant cylindrical projection on an ellipsoid flatter than about 1/100 isn't supported (this one's is 1/{:.1})",
                    a / (a - b)
                ));
            }
            let lat_ts = match tokens.iter().find(|(k, _)| *k == "lat_ts") {
                Some((_, v)) => v.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| format!("+lat_ts={v} isn't a number"))?,
                None => 0.0,
            };
            let to_meter = native.to_meter();
            if !(to_meter.is_finite() && to_meter > 0.0) {
                return Err("its unit isn't usable".into());
            }
            let eqc = rustgis_geom::EquidistantCylindrical::new(
                a,
                es,
                lat_ts,
                native.latitude_of_origin().to_degrees(),
                native.longitude_of_origin().to_degrees(),
                native.false_easting(),
                native.false_northing(),
            )
            .ok_or("not a possible equidistant cylindrical projection")?;
            Ok(Some(EllipsoidalEqc { eqc, geo, to_meter }))
        })();
        built.transpose()
    }

    /// Its coordinates to longitude/latitude on its datum, then to WGS 84.
    fn unproject(&self, wgs84: &proj4rs::Proj, c: Coord) -> Option<Coord> {
        let own = self.eqc.inverse(Coord::new(c.x * self.to_meter, c.y * self.to_meter))?;
        let mut p = (own.x.to_radians(), own.y.to_radians(), 0.0);
        proj4rs::transform::transform(&self.geo, wgs84, &mut p).ok()?;
        Some(Coord::new(p.0.to_degrees(), p.1.to_degrees()))
    }

    /// WGS 84 longitude/latitude to its datum's, then to its coordinates.
    fn project(&self, wgs84: &proj4rs::Proj, c: Coord) -> Option<Coord> {
        let mut p = (c.x.to_radians(), c.y.to_radians(), 0.0);
        proj4rs::transform::transform(wgs84, &self.geo, &mut p).ok()?;
        let m = self.eqc.forward(Coord::new(p.0.to_degrees(), p.1.to_degrees()))?;
        Some(Coord::new(m.x / self.to_meter, m.y / self.to_meter))
    }
}

#[derive(Clone)]
enum Kind {
    LonLat,
    Merc,
    Proj { native: Box<proj4rs::Proj>, wgs84: Box<proj4rs::Proj>, geographic: bool },
    Sinusoidal(rustgis_geom::Sinusoidal),
    Eqc { eqc: Box<EllipsoidalEqc>, wgs84: Box<proj4rs::Proj> },
}

/// Converts between a raster's coordinates and WGS 84 longitude/latitude.
#[derive(Clone)]
pub struct Transformer {
    kind: Kind,
}

impl std::fmt::Debug for Transformer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let k = match self.kind {
            Kind::LonLat => "lonlat",
            Kind::Merc => "mercator",
            Kind::Proj { .. } => "proj",
            Kind::Sinusoidal(_) => "sinusoidal",
            Kind::Eqc { .. } => "eqc",
        };
        write!(f, "Transformer({k})")
    }
}

impl Transformer {
    /// Native coordinates to longitude/latitude.
    pub fn to_lonlat(&self, c: Coord) -> Option<Coord> {
        let out = match &self.kind {
            Kind::LonLat => c,
            Kind::Merc => merc_to_lonlat(c),
            Kind::Proj { native, wgs84, geographic } => {
                let mut p = if *geographic { (c.x.to_radians(), c.y.to_radians(), 0.0) } else { (c.x, c.y, 0.0) };
                proj4rs::transform::transform(native, wgs84, &mut p).ok()?;
                Coord::new(p.0.to_degrees(), p.1.to_degrees())
            }
            Kind::Sinusoidal(s) => s.inverse(c)?,
            Kind::Eqc { eqc, wgs84 } => eqc.unproject(wgs84, c)?,
        };
        out.is_finite().then_some(out)
    }

    /// Extent samples keep a geographic grid's winding without losing its prime meridian or
    /// datum shift. Only whole turns change; other consumers keep `to_lonlat`'s usual branch.
    pub(super) fn to_lonlat_for_extent(&self, c: Coord) -> Option<Coord> {
        let mut out = self.to_lonlat(c)?;
        if let Kind::Proj { native, geographic: true, .. } = &self.kind {
            let greenwich = c.x + native.from_greenwich().to_degrees();
            out.x += 360.0 * ((greenwich - out.x) / 360.0).round();
        }
        out.is_finite().then_some(out)
    }

    /// Longitude/latitude to native coordinates.
    pub fn from_lonlat(&self, c: Coord) -> Option<Coord> {
        let out = match &self.kind {
            Kind::LonLat => c,
            Kind::Merc => lonlat_to_merc(c),
            Kind::Proj { native, wgs84, geographic } => {
                let mut p = (c.x.to_radians(), c.y.to_radians(), 0.0);
                proj4rs::transform::transform(wgs84, native, &mut p).ok()?;
                if *geographic { Coord::new(p.0.to_degrees(), p.1.to_degrees()) } else { Coord::new(p.0, p.1) }
            }
            Kind::Sinusoidal(s) => s.forward(c)?,
            Kind::Eqc { eqc, wgs84 } => eqc.project(wgs84, c)?,
        };
        out.is_finite().then_some(out)
    }

    /// Whether the raster's own coordinates are longitude/latitude degrees.
    pub fn is_geographic(&self) -> bool {
        matches!(self.kind, Kind::LonLat | Kind::Proj { geographic: true, .. })
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

    /// EPSG:4087 and the other equidistant cylindrical systems on an ellipsoid are computed on it
    /// (EPSG method 1028), not on a sphere of its semi-major axis as proj4rs does: 45°N is
    /// 4 984 944.378 m north of the equator on WGS 84, not 5 009 377.086 m (24 km more). 55°N 10°E
    /// is the EPSG Guidance Note 7-2's example: E = 1 113 194.91 m, N = 6 097 230.31 m.
    #[test]
    fn equidistant_cylindrical_on_an_ellipsoid() {
        check(4087, (10.0, 55.0), (1_113_194.91, 6_097_230.31), 0.005);
        check(4087, (0.0, 45.0), (0.0, 4_984_944.378), 0.001);
        let t = Crs::from_epsg(4087).unwrap().transformer().unwrap();
        assert_eq!(format!("{t:?}"), "Transformer(eqc)");
        let spherical = std::f64::consts::FRAC_PI_4 * 6_378_137.0;
        let at = t.to_lonlat(Coord::new(0.0, spherical)).unwrap();
        assert!((at.y - 45.0).abs() > 0.2, "{at:?}");
        // ESRI's World Equidistant Cylindrical, true to scale at 60°: an easting per degree of
        // ν₁·cos 60° (Snyder 1987, eq. 4-20 for ν₁).
        let es = 0.006_694_379_990_141_317;
        let nu1 = 6_378_137.0 / (1.0 - es * 0.75f64).sqrt();
        let world = Crs::parse("+proj=eqc +lat_ts=60 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs").unwrap();
        let m = world.transformer().unwrap().from_lonlat(Coord::new(1.0, 45.0)).unwrap();
        assert!((m.x - nu1 * 0.5 * 1f64.to_radians()).abs() < 1e-6 && (m.y - 4_984_944.378).abs() < 0.001, "{m:?}");
        // On a sphere it stays proj4rs's, unchanged (IAU_2015:49910, Mars).
        for sphere in ["+proj=eqc +lat_ts=0 +lon_0=0 +R=3396190 +units=m +no_defs", "+proj=eqc +lon_0=0 +a=3396190 +b=3396190 +no_defs"] {
            let t = Crs::parse(sphere).unwrap().transformer().unwrap();
            assert_eq!(format!("{t:?}"), "Transformer(proj)", "{sphere}");
            let m = t.from_lonlat(Coord::new(77.45, 18.44)).unwrap();
            assert!((m.x - 3_396_190.0 * 77.45f64.to_radians()).abs() < 1e-6 && (m.y - 3_396_190.0 * 18.44f64.to_radians()).abs() < 1e-6);
        }
    }

    /// The datum of an ellipsoidal equidistant cylindrical system is kept: its prime meridian, its
    /// shift to WGS 84, its unit and its origin, each as proj4rs would apply it.
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
        // A shift to WGS 84 is applied as proj4rs applies it to longitude/latitude on that datum.
        let shifted = t("+units=m +towgs84=100,-50,30").to_lonlat(m).unwrap();
        let geo = proj4rs::Proj::from_proj_string("+proj=longlat +ellps=GRS80 +towgs84=100,-50,30 +no_defs").unwrap();
        let wgs84 = proj4rs::Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs").unwrap();
        let mut p = (plain.x.to_radians(), plain.y.to_radians(), 0.0);
        proj4rs::transform::transform(&geo, &wgs84, &mut p).unwrap();
        assert!((shifted.x - p.0.to_degrees()).abs() < 1e-12 && (shifted.y - p.1.to_degrees()).abs() < 1e-12, "{shifted:?} {p:?}");
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
        for refused in [
            "+proj=eqc +ellps=WGS84 +lat_ts=abc",
            // Degrees and minutes, which proj4rs reads: not taken as 0.
            "+proj=eqc +ellps=WGS84 +lat_ts=30d30",
            "+proj=eqc +ellps=WGS84 +lat_ts=0.5r",
            "+proj=eqc +ellps=WGS84 +lat_ts=NaN",
            "+proj=eqc +ellps=WGS84 +lat_ts=90",
            "+proj=eqc +ellps=WGS84 +axis=wsu",
            "+proj=eqc +ellps=WGS84 +over",
            "+proj=eqc +ellps=WGS84 +to_meter=0",
        ] {
            assert!(Crs::parse(refused).is_err(), "{refused}");
        }
        // On a sphere, axes and +over stay proj4rs's, as before the ellipsoidal form.
        for sphere in ["+proj=eqc +R=3396190 +over +no_defs", "+proj=eqc +R=3396190 +axis=wsu +no_defs"] {
            let t = Crs::parse(sphere).and_then(Crs::transformer).unwrap_or_else(|e| panic!("{sphere}: {e}"));
            assert_eq!(format!("{t:?}"), "Transformer(proj)", "{sphere}");
        }
        // Flatter than the meridian-distance series holds for (Jupiter's 1/15.4): refused, saying
        // so, not approximated.
        let e = Crs::parse("+proj=eqc +a=71492000 +b=66854000 +no_defs").unwrap_err();
        assert!(e.contains("flatter than about 1/100") && e.contains("1/15.4"), "{e}");
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

    /// EPSG codes 9020–20003 come from the built-in gap table (opengeos/RustGIS#198); the
    /// expected coordinates are pyproj's (PROJ 9.8, EPSG dataset v12.029).
    #[test]
    fn codes_crs_definitions_lacks() {
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
        let p3405 = epsg_proj4(3405).expect("EPSG:3405 must be present in crs_definitions");
        assert!(p3405.contains("+proj=utm") && p3405.contains("+zone=48"), "{p3405}");
        let p3406 = epsg_proj4(3406).expect("EPSG:3406 must be present in crs_definitions");
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
        let file = CustomCrs { proj4: merc.into(), label: "m".into(), geokeys: Vec::new(), doubles: Vec::new(), ascii: String::new() };
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
        // NAD27 / US National Atlas Equal Area: Washington, DC, without NAD27's datum shift
        // (the grids `+datum=NAD27` stands for, which proj4rs can't load; see epsg_gap.rs).
        check(9311, (-77.04, 38.89), (1_958_685.805, -405_348.265), 0.01);
        // EPSG:10622 (an orthographic) has no implementation, so it is left out rather than
        // listed and refused.
        assert_eq!(Crs::from_epsg(10622), None);
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
        // On the desktop azimuthal equidistant is there; a code that doesn't exist is unknown.
        assert!(aeqd_supported());
        assert_eq!(unusable_epsg(28707), "the coordinate system EPSG:28707 isn't supported");
    }

    /// The ESRI table (opengeos/RustGIS#213) is in order and holds only projected systems on Earth.
    #[test]
    fn esri_table_is_sound() {
        let defs = super::super::esri::DEFS;
        assert!(defs.len() > 1900, "{}", defs.len());
        for pair in defs.windows(2) {
            assert!(pair[0].0 < pair[1].0, "out of order at ESRI:{}", pair[1].0);
        }
        for &(code, def) in defs {
            assert_eq!(esri_proj4(code), Some(def));
            assert!(def.starts_with("+proj=") && !def.contains("+proj=longlat") && !def.contains("+proj=geocent"), "ESRI:{code}");
        }
        assert_eq!(esri_codes().count(), defs.len());
        // South America Albers Equal Area Conic, which opened the issue.
        assert!(esri_proj4(102033).is_some_and(|d| d.contains("+proj=aea") && d.contains("+lon_0=-60")));
        // Left out: World Miller (RustGIS's sphere is kilometres from PROJ's), a lunar system, a
        // Hammer–Aitoff PROJ can't write as a string, and codes that don't exist.
        for code in [54003, 103877, 54044, 0, 4326, u32::MAX] {
            assert_eq!(esri_proj4(code), None, "ESRI:{code}");
        }
    }

    /// Every entry in the gap table is wanted (upstream still lacks it), in order, and usable.
    #[test]
    fn gap_table_is_sound() {
        let defs = super::super::epsg_gap::DEFS;
        assert!(defs.len() > 400, "{}", defs.len());
        for pair in defs.windows(2) {
            assert!(pair[0].0 < pair[1].0, "out of order at EPSG:{}", pair[1].0);
        }
        for &(code, def) in defs {
            assert!(crs_definitions::from_code(code).is_none(), "crs-definitions has EPSG:{code} now: drop it from the table");
            // A build without azimuthal equidistant leaves Equi7's out, and says why.
            if def.contains("+proj=aeqd") && !aeqd_supported() {
                assert_eq!(epsg_proj4(code), None);
                assert!(unusable_epsg(u32::from(code)).contains("web app"));
                continue;
            }
            assert_eq!(epsg_proj4(code), Some(def));
            let crs = Crs::from_epsg(u32::from(code)).unwrap_or_else(|| panic!("EPSG:{code}"));
            assert_eq!(crs.is_geographic(), def.contains("+proj=longlat"), "EPSG:{code}");
            crs.transformer().unwrap_or_else(|e| panic!("EPSG:{code}: {e}"));
        }
    }
}
