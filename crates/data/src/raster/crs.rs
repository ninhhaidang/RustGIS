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
/// [`super::epsg_gap`]). `None` for a code that is neither. terrio-crs computes the code from it
/// ([`Crs::transformer`]).
pub fn epsg_proj4(code: u16) -> Option<&'static str> {
    crs_definitions::from_code(code).map(|d| d.proj4).or_else(|| super::epsg_gap::proj4(code))
}

/// Why a file's EPSG code can't be used.
pub fn unusable_epsg(code: u32) -> String {
    format!("the coordinate system EPSG:{code} isn't supported")
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
/// table.
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
            if !t.iter().any(|d| d.proj4 == def.proj4) {
                if t.len() >= MAX_CUSTOM {
                    return Err("too many coordinate systems defined by files".into());
                }
                if typed && t.iter().filter(|d| d.label == TYPED_LABEL).count() >= MAX_TYPED {
                    return Err("too many coordinate systems typed as PROJ strings".into());
                }
            }
        }
        transformer_of(&def.proj4).map(|_| ()).map_err(|e| format!("{}: {e}", def.proj4))?;
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

    /// A transformer between this system and WGS 84 longitude/latitude, built once a run per
    /// system and shared ([`Transformer`] is cheap to clone).
    pub fn transformer(self) -> Result<Transformer, String> {
        match self {
            Crs::LonLat => Ok(Transformer { kind: Kind::LonLat }),
            Crs::WebMercator => Ok(Transformer { kind: Kind::Merc }),
            // By the code's PROJ definition, with its datum shift, as a WKT naming the same
            // system is read: both put a place at the same point.
            Crs::Epsg(c) => match epsg_proj4(c) {
                Some(def) => {
                    note_used(def, || format!("EPSG:{c}"));
                    let t = transformer_of(def).map_err(|e| format!("EPSG:{c}: {e}"))?;
                    probe_grid(def, || area_centre(c));
                    Ok(t)
                }
                None => Err(format!("unknown coordinate system EPSG:{c}")),
            },
            _ => {
                let def = self.custom_def().ok_or("an unknown custom coordinate system")?;
                note_used(&def.proj4, || def.label.clone());
                transformer_of(&def.proj4).map_err(|e| format!("{}: {e}", def.label))
            }
        }
    }
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
    let mut map = built().lock().unwrap_or_else(|e| e.into_inner());
    // The first one built is kept and shared: another thread may have built it meanwhile.
    let t = if map.len() < MAX_BUILT || map.contains_key(def) { map.entry(def.to_string()).or_insert(t).clone() } else { t };
    drop(map);
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
    let t = Transformer::of(&sr, geographic_def(def), (lat_first, west))?;
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
    transformer_of(&geographic).ok()?.from_lonlat(c)
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
    Terrio(std::sync::Arc<Terrio>),
}

/// A system terrio-crs computes: the transformers to and from longitude/latitude, built once.
struct Terrio {
    to_lonlat: terrio_crs::Transformer,
    from_lonlat: terrio_crs::Transformer,
    /// For a projection: WGS 84 longitude/latitude onto its own datum's, and its coordinates back
    /// to that longitude/latitude without a datum shift, so [`Transformer::from_lonlat`] checks
    /// the projection's round trip alone. A datum transformation chosen for each place (by its
    /// area of use) may differ there and back near the edge of that area, by tens of metres, and
    /// that is no fold.
    own: Option<(terrio_crs::Transformer, terrio_crs::Transformer, terrio_crs::Transformer)>,
    /// Its coordinates are degrees.
    geographic: bool,
    /// Its coordinates are north first (`+axis=neu`).
    lat_first: bool,
    /// Its east-west coordinate points west (`+axis=wnu`, `wsu`): -1, else 1.
    lon_sign: f64,
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
/// features about 10 m off). The flags terrio-crs keeps for a transformer are turned into notes
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

/// Whether a coordinate system RustGIS can't name, one of more than [`MAX_BUILT`] used this run,
/// needed a missing datum grid since this was last asked: for the session to say so.
pub fn take_unnamed_grid_need() -> bool {
    UNNAMED_NEEDS_GRID.swap(false, std::sync::atomic::Ordering::Relaxed)
}

thread_local! {
    /// The definition whose transformer [`grid_note_of`] is asking, for [`record_warning`].
    static FLUSHING: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
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

/// The PROJ definition a system is transformed by, and what a note calls it; `None` for WGS 84
/// longitude/latitude and Web Mercator, which need no datum transformation.
pub fn definition_and_label(crs: Crs) -> Option<(String, String)> {
    match crs {
        Crs::Epsg(c) => Some((epsg_proj4(c)?.to_string(), format!("EPSG:{c}"))),
        Crs::Custom(_) => crs.custom_def().map(|d| (d.proj4.clone(), d.label.clone())),
        Crs::LonLat | Crs::WebMercator => None,
    }
}

/// The note of `count` systems whose most accurate datum transformation needed a grid
/// ([`needs_a_missing_grid`]), `names` naming them (the app lists a few and counts the rest):
/// one notice for all of them, with their count.
pub fn grid_notes_text(count: usize, names: &str) -> String {
    const TAIL: &str = "the most accurate datum transformation needs a grid RustGIS doesn't have yet, so the next one was used; places may be metres off (NAD27, for one, is about 10 m off without its grid)";
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

/// Whether a definition's most accurate datum transformation needs a grid RustGIS doesn't have,
/// asked once per definition at a place where it is used (`at`: the middle of an EPSG code's area
/// of use): terrio-crs notes a missing grid by the first place a transformer takes, which may be
/// one where the grid doesn't reach (a world map's corner), and then never again. A system
/// without a code, which has no area of use to ask at, has only that first place. The first use
/// of a code reads terrio-crs's catalog of areas ([`area_centre`]: about 50 ms, once a run).
pub fn probe_grid(def: &str, at: impl FnOnce() -> Option<Coord>) {
    static PROBED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    {
        let probed = PROBED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        // Without a limit: only EPSG codes' definitions are probed, a table's worth at most.
        if probed.contains(def) {
            return;
        }
    }
    let Some(at) = at().filter(|c| c.is_finite()) else { return };
    PROBED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).insert(def.to_string());
    no_grids();
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

/// The notes ([`grid_note`]) of the systems this thread asked for a transformer for since it
/// last looked: what a command used beyond its project's layers and maps.
pub fn take_used_notes() -> Vec<String> {
    take_used().iter().filter_map(|(def, label)| grid_note_of(def, label)).collect()
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
    let map = built().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(Ok(Transformer { kind: Kind::Terrio(t) })) = map.get(def) {
        let _ = FLUSHING.try_with(|l| *l.borrow_mut() = Some(def.to_string()));
        t.to_lonlat.flush_warnings();
        t.from_lonlat.flush_warnings();
        if let Some((onto, unproject, _)) = &t.own {
            onto.flush_warnings();
            unproject.flush_warnings();
        }
        let _ = FLUSHING.try_with(|l| *l.borrow_mut() = None);
    }
    drop(map);
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
            Kind::Terrio(t) => write!(f, "Transformer({})", t.params.as_ref().map_or("?", |p| p.method.as_str())),
        }
    }
}

/// The world projections whose outline's east and west edges are the meridian opposite their
/// central one (#1701), by PROJ's name.
const WORLD_METHODS: [&str; 3] = ["natearth", "wintri", "eck4"];

/// No datum grids yet (opengeos/RustGIS#2032, A4): none read from the computer's PROJ folders,
/// from paths a definition names (a project file's), or downloaded, so a place lands where it does
/// on every computer and in the browser. A transformation that needs one takes the next PROJ
/// lists, and says so ([`grid_note`]). Set before terrio-crs builds any transformer.
fn no_grids() {
    static NO_GRIDS: std::sync::Once = std::sync::Once::new();
    NO_GRIDS.call_once(|| {
        terrio_crs::set_grid_search_paths(Some(Vec::new()));
        terrio_crs::set_network_enabled(Some(false));
    });
}

impl Transformer {
    /// The transformer between `sr` and WGS 84 longitude/latitude; `geographic` when its
    /// coordinates are degrees. A system on another body than the Earth goes to and from its own
    /// longitude/latitude instead, which is read as it is, on the Earth ([`Crs::is_planetary`]).
    fn of(sr: &terrio_crs::SpatialRef, geographic: bool, (lat_first, west): (bool, bool)) -> Result<Transformer, String> {
        no_grids();
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
        // the projection's own.
        let own = (!geographic)
            .then(|| sr.geographic_wkt())
            .flatten()
            .map(|g| terrio_crs::SpatialRef::from_wkt(&g))
            .filter(|g| !g.same_as(&wgs84))
            .and_then(|g| {
                let unproject = terrio_crs::Transformer::new(sr, &g).ok()?.with_warning_handler(record_warning);
                let project = unproject.inverse().ok()?;
                Some((terrio_crs::Transformer::new(&wgs84, &g).ok()?.with_warning_handler(record_warning), unproject, project))
            });
        let method = |names: &[&str]| params.as_ref().is_some_and(|p| !geographic && names.contains(&p.method.as_str()));
        let (wraps, eck4) = (method(&WRAPPING), method(&["eck4"]));
        let t = Terrio { to_lonlat, from_lonlat, own, geographic, lat_first, lon_sign: if west { -1.0 } else { 1.0 }, params, wraps, eck4 };
        Ok(Transformer { kind: Kind::Terrio(std::sync::Arc::new(t)) })
    }

    /// Native coordinates to longitude/latitude.
    pub fn to_lonlat(&self, c: Coord) -> Option<Coord> {
        let out = match &self.kind {
            Kind::LonLat => c,
            Kind::Merc => merc_to_lonlat(c),
            Kind::Terrio(t) => {
                // A latitude past a pole is no place (terrio-crs would fold it back to one).
                let lat = if t.lat_first { c.x } else { c.y };
                if !c.is_finite() || (t.geographic && lat.abs() > 90.0 + 1e-9) {
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
        if let Kind::Terrio(t) = &self.kind
            && t.geographic
        {
            let lon = t.lon_sign * if t.lat_first { c.y } else { c.x };
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
        if let Kind::Terrio(t) = &self.kind
            && !t.geographic
        {
            let back = match &t.own {
                // On the projection's own datum: the place there, and where its coordinates go back.
                Some((onto, unproject, project)) => {
                    let there = onto.transform(c.x, c.y).ok().map(|(x, y)| Coord::new(x, y));
                    there.zip(t.unproject(unproject, project, out))
                }
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
            Kind::Terrio(t) => {
                if !c.is_finite() || c.y.abs() > 90.0 + 1e-9 {
                    return None;
                }
                let (x, y) = t.from_lonlat.transform(c.x, c.y).ok()?;
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
            Kind::Terrio(t) => t.geographic,
        }
    }

    /// The conversion's parameters (method, centre, offsets, unit, prime meridian), for a system
    /// terrio-crs computes that has some.
    pub fn params(&self) -> Option<&terrio_crs::ProjParams> {
        match &self.kind {
            Kind::Terrio(t) => t.params.as_ref(),
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
        // NAD27 / US National Atlas Equal Area: Washington, DC, without NAD27's datum shift (the
        // gap table's definition takes none; see epsg_gap.rs).
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
        // Azimuthal equidistant works in every build; a code that doesn't exist is unknown.
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
        // Left out: World Miller (left out when RustGIS computed it on a sphere), a lunar system, a
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
            assert_eq!(epsg_proj4(code), Some(def));
            let crs = Crs::from_epsg(u32::from(code)).unwrap_or_else(|| panic!("EPSG:{code}"));
            assert_eq!(crs.is_geographic(), def.contains("+proj=longlat"), "EPSG:{code}");
            crs.transformer().unwrap_or_else(|e| panic!("EPSG:{code}: {e}"));
        }
    }
}
