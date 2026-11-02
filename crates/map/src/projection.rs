//! Map projections: how the flat map lays the Earth out (the globe is in [`crate::globe`]).
//!
//! A map is drawn in Web Mercator by default, or in any projected coordinate system: a preset
//! (world and polar projections, regional equal-area grids), an EPSG code or an ESRI code. The camera then
//! works in that system's units, so panning, zooming and hit testing are the same for all of
//! them; only conversions between longitude/latitude and map coordinates go through here.

use rustgis_geom::{Coord, Extent, lonlat_to_merc, merc_to_lonlat};

use rustgis_data::vn2000_towgs84;

/// A named projection: (id, label, PROJ definition).
pub const PRESETS: &[(&str, &str, &str)] = &[
    ("equirectangular", "Equirectangular (Plate Carrée)", "+proj=eqc +lat_ts=0 +lon_0=0 +datum=WGS84 +units=m +no_defs"),
    ("miller", "Miller Cylindrical", "+proj=mill +lon_0=0 +R_A +datum=WGS84 +units=m +no_defs"),
    ("mollweide", "Mollweide (equal-area world)", "+proj=moll +lon_0=0 +datum=WGS84 +units=m +no_defs"),
    ("cylindricalEqualArea", "Lambert Cylindrical Equal Area", "+proj=cea +lon_0=0 +lat_ts=0 +datum=WGS84 +units=m +no_defs"),
    ("robinson", "Robinson", "+proj=robin +lon_0=0 +datum=WGS84 +units=m +no_defs"),
    ("equalEarth", "Equal Earth", "+proj=eqearth +lon_0=0 +datum=WGS84 +units=m +no_defs"),
    ("naturalEarth", "Natural Earth", "+proj=natearth +lon_0=0 +datum=WGS84 +units=m +no_defs"),
    ("winkelTripel", "Winkel Tripel", "+proj=wintri +lon_0=0 +datum=WGS84 +units=m +no_defs"),
    ("eckertIV", "Eckert IV (equal-area world)", "+proj=eck4 +lon_0=0 +datum=WGS84 +units=m +no_defs"),
    ("northPolar", "North Polar Stereographic", "+proj=stere +lat_0=90 +lat_ts=70 +lon_0=0 +datum=WGS84 +units=m +no_defs"),
    ("southPolar", "South Polar Stereographic", "+proj=stere +lat_0=-90 +lat_ts=-71 +lon_0=0 +datum=WGS84 +units=m +no_defs"),
    (
        "europeEqualArea",
        "Europe Equal Area (ETRS89 LAEA)",
        "+proj=laea +lat_0=52 +lon_0=10 +x_0=4321000 +y_0=3210000 +ellps=GRS80 +units=m +no_defs",
    ),
    (
        "usaAlbers",
        "USA Contiguous Albers Equal Area",
        "+proj=aea +lat_0=23 +lon_0=-96 +lat_1=29.5 +lat_2=45.5 +datum=NAD83 +units=m +no_defs",
    ),
    ("albers", "Albers Equal Area Conic", "+proj=aea +lat_0=0 +lon_0=0 +lat_1=20 +lat_2=50 +datum=WGS84 +units=m +no_defs"),
    ("lambertConformal", "Lambert Conformal Conic", "+proj=lcc +lat_0=0 +lon_0=0 +lat_1=20 +lat_2=50 +datum=WGS84 +units=m +no_defs"),
    (
        "vn2000tm3",
        "VN-2000 / TM-3 (Vietnam)",
        concat!("+proj=tmerc +lat_0=0 +lon_0=105 +k=0.9999 +x_0=500000 +y_0=0 +ellps=WGS84 ", vn2000_towgs84!(), " +units=m +no_defs"),
    ),
    ("vn2000Utm48", "VN-2000 / UTM zone 48N", concat!("+proj=utm +zone=48 +ellps=WGS84 ", vn2000_towgs84!(), " +units=m +no_defs")),
    ("vn2000Utm49", "VN-2000 / UTM zone 49N", concat!("+proj=utm +zone=49 +ellps=WGS84 ", vn2000_towgs84!(), " +units=m +no_defs")),
];

/// The VN-2000 presets and the EPSG systems they are: a name `Crs::parse` reads as one of these
/// (`VN-2000 / UTM zone 48N`, `vn2000_utm48`…) picks the preset too.
const VN2000_PRESET_EPSG: &[(&str, u16)] = &[("vn2000tm3", 5897), ("vn2000Utm48", 3405), ("vn2000Utm49", 3406)];

/// A province or regional hub in Vietnam and its VN-2000 3-degree central meridian
/// (Circular 25/2014/TT-BTNMT Annex 02, inheriting QĐ 05/2007/QĐ-BTNMT).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vn2000Province {
    /// Name in Vietnamese, e.g. "Hà Nội".
    pub name: &'static str,
    /// Central meridian in degrees east (lon_0).
    pub lon_0: f64,
    /// Normalized search identifier (ASCII lowercase without diacritics).
    pub id: &'static str,
    /// Additional search aliases (ASCII lowercase normalized).
    pub aliases: &'static [&'static str],
}

/// The central meridians of Vietnam's 63 provinces and cities for VN-2000 / TM-3 (Circular
/// 973/2001/TT-TCĐC, kept by Circular 25/2014/TT-BTNMT). Every meridian matches the area of use
/// EPSG gives its VN-2000 / TM-3 system (EPSG:5899 and 9205 to 9218 name the provinces of each);
/// the provinces at 105° and 108° are those EPSG lists under no other meridian. The table predates
/// the 2025 mergers of provinces, as the cadastral maps made with it do.
pub const VN2000_PROVINCES: &[Vn2000Province] = &[
    Vn2000Province { name: "An Giang", lon_0: 104.75, id: "angiang", aliases: &[] },
    Vn2000Province { name: "Bà Rịa - Vũng Tàu", lon_0: 107.75, id: "bariavungtau", aliases: &["vungtau"] },
    Vn2000Province { name: "Bạc Liêu", lon_0: 105.0, id: "baclieu", aliases: &[] },
    Vn2000Province { name: "Bắc Giang", lon_0: 107.0, id: "bacgiang", aliases: &[] },
    Vn2000Province { name: "Bắc Kạn", lon_0: 106.5, id: "backan", aliases: &[] },
    Vn2000Province { name: "Bắc Ninh", lon_0: 105.5, id: "bacninh", aliases: &[] },
    Vn2000Province { name: "Bến Tre", lon_0: 105.75, id: "bentre", aliases: &[] },
    Vn2000Province { name: "Bình Dương", lon_0: 105.75, id: "binhduong", aliases: &[] },
    Vn2000Province { name: "Bình Định", lon_0: 108.25, id: "binhdinh", aliases: &["quynhon"] },
    Vn2000Province { name: "Bình Phước", lon_0: 106.25, id: "binhphuoc", aliases: &[] },
    Vn2000Province { name: "Bình Thuận", lon_0: 108.5, id: "binhthuan", aliases: &["phantiet"] },
    Vn2000Province { name: "Cà Mau", lon_0: 104.5, id: "camau", aliases: &[] },
    Vn2000Province { name: "Cao Bằng", lon_0: 105.75, id: "caobang", aliases: &[] },
    Vn2000Province { name: "Cần Thơ", lon_0: 105.0, id: "cantho", aliases: &[] },
    Vn2000Province { name: "Đà Nẵng", lon_0: 107.75, id: "danang", aliases: &[] },
    Vn2000Province { name: "Đắk Lắk", lon_0: 108.5, id: "daklak", aliases: &["buonmathuot", "daclac"] },
    Vn2000Province { name: "Đắk Nông", lon_0: 108.5, id: "daknong", aliases: &["dacnong"] },
    Vn2000Province { name: "Điện Biên", lon_0: 103.0, id: "dienbien", aliases: &["dienbienphu"] },
    Vn2000Province { name: "Đồng Nai", lon_0: 107.75, id: "dongnai", aliases: &["bienhoa"] },
    Vn2000Province { name: "Đồng Tháp", lon_0: 105.0, id: "dongthap", aliases: &["caolanh"] },
    Vn2000Province { name: "Gia Lai", lon_0: 108.5, id: "gialai", aliases: &["pleiku"] },
    Vn2000Province { name: "Hà Giang", lon_0: 105.5, id: "hagiang", aliases: &[] },
    Vn2000Province { name: "Hà Nam", lon_0: 105.0, id: "hanam", aliases: &["phuly"] },
    Vn2000Province { name: "Hà Nội", lon_0: 105.0, id: "hanoi", aliases: &[] },
    Vn2000Province { name: "Hà Tĩnh", lon_0: 105.5, id: "hatinh", aliases: &[] },
    Vn2000Province { name: "Hải Dương", lon_0: 105.5, id: "haiduong", aliases: &[] },
    Vn2000Province { name: "Hải Phòng", lon_0: 105.75, id: "haiphong", aliases: &[] },
    Vn2000Province { name: "Hậu Giang", lon_0: 105.0, id: "haugiang", aliases: &["vithanh", "vitri"] },
    Vn2000Province { name: "Hòa Bình", lon_0: 106.0, id: "hoabinh", aliases: &[] },
    Vn2000Province { name: "Hưng Yên", lon_0: 105.5, id: "hungyen", aliases: &[] },
    Vn2000Province { name: "Khánh Hòa", lon_0: 108.25, id: "khanhhoa", aliases: &["nhatrang"] },
    Vn2000Province { name: "Kiên Giang", lon_0: 104.5, id: "kiengiang", aliases: &["rachgia", "phuquoc"] },
    Vn2000Province { name: "Kon Tum", lon_0: 107.5, id: "kontum", aliases: &[] },
    Vn2000Province { name: "Lai Châu", lon_0: 103.0, id: "laichau", aliases: &[] },
    Vn2000Province { name: "Lạng Sơn", lon_0: 107.25, id: "langson", aliases: &[] },
    Vn2000Province { name: "Lào Cai", lon_0: 104.75, id: "laocai", aliases: &["sapa"] },
    Vn2000Province { name: "Lâm Đồng", lon_0: 107.75, id: "lamdong", aliases: &["dalat"] },
    Vn2000Province { name: "Long An", lon_0: 105.75, id: "longan", aliases: &["tanan"] },
    Vn2000Province { name: "Nam Định", lon_0: 105.5, id: "namdinh", aliases: &[] },
    Vn2000Province { name: "Nghệ An", lon_0: 104.75, id: "nghean", aliases: &["vinh"] },
    Vn2000Province { name: "Ninh Bình", lon_0: 105.0, id: "ninhbinh", aliases: &[] },
    Vn2000Province { name: "Ninh Thuận", lon_0: 108.25, id: "ninhthuan", aliases: &["phanrang"] },
    Vn2000Province { name: "Phú Thọ", lon_0: 104.75, id: "phutho", aliases: &["viettri"] },
    Vn2000Province { name: "Phú Yên", lon_0: 108.5, id: "phuyen", aliases: &["tuyhoa"] },
    Vn2000Province { name: "Quảng Bình", lon_0: 106.0, id: "quangbinh", aliases: &["donghoi"] },
    Vn2000Province { name: "Quảng Nam", lon_0: 107.75, id: "quangnam", aliases: &["tamky", "hoian"] },
    Vn2000Province { name: "Quảng Ngãi", lon_0: 108.0, id: "quangngai", aliases: &[] },
    Vn2000Province { name: "Quảng Ninh", lon_0: 107.75, id: "quangninh", aliases: &["halong"] },
    Vn2000Province { name: "Quảng Trị", lon_0: 106.25, id: "quangtri", aliases: &["dongha"] },
    Vn2000Province { name: "Sóc Trăng", lon_0: 105.5, id: "soctrang", aliases: &[] },
    Vn2000Province { name: "Sơn La", lon_0: 104.0, id: "sonla", aliases: &[] },
    Vn2000Province { name: "Tây Ninh", lon_0: 105.5, id: "tayninh", aliases: &[] },
    Vn2000Province { name: "Thái Bình", lon_0: 105.5, id: "thaibinh", aliases: &[] },
    Vn2000Province { name: "Thái Nguyên", lon_0: 106.5, id: "thainguyen", aliases: &[] },
    Vn2000Province { name: "Thanh Hóa", lon_0: 105.0, id: "thanhhoa", aliases: &[] },
    Vn2000Province { name: "Thừa Thiên Huế", lon_0: 107.0, id: "thuathienhue", aliases: &["hue"] },
    Vn2000Province { name: "Tiền Giang", lon_0: 105.75, id: "tiengiang", aliases: &["mytho"] },
    Vn2000Province { name: "TP. Hồ Chí Minh", lon_0: 105.75, id: "tphochiminh", aliases: &["tphcm", "saigon", "hochiminh", "hcm"] },
    Vn2000Province { name: "Trà Vinh", lon_0: 105.5, id: "travinh", aliases: &[] },
    Vn2000Province { name: "Tuyên Quang", lon_0: 106.0, id: "tuyenquang", aliases: &[] },
    Vn2000Province { name: "Vĩnh Long", lon_0: 105.5, id: "vinhlong", aliases: &[] },
    Vn2000Province { name: "Vĩnh Phúc", lon_0: 105.0, id: "vinhphuc", aliases: &["vinhyen"] },
    Vn2000Province { name: "Yên Bái", lon_0: 104.75, id: "yenbai", aliases: &[] },
];

/// Distinct representative regional hubs for UI central meridian shortcut buttons.
///
/// This is not an exhaustive list of all 63 provinces, but provides quick-access
/// shortcuts covering each major geographic zone:
/// - Northern delta: Hà Nội (105.0°)
/// - Southern region: TP. Hồ Chí Minh (105.75°)
/// - North Central: Thừa Thiên Huế (107.0°)
/// - Central Coast: Đà Nẵng (107.75°)
/// - South Central Coast: Khánh Hòa (108.25°)
/// - Central Highlands: Đắk Lắk (108.5°)
pub const VN2000_UI_PROVINCES: &[(&str, f64)] = &[
    ("Hà Nội", 105.0),
    ("TP. Hồ Chí Minh", 105.75),
    ("Thừa Thiên Huế", 107.0),
    ("Đà Nẵng", 107.75),
    ("Khánh Hòa", 108.25),
    ("Đắk Lắk", 108.5),
];

/// A parameter a preset takes (opengeos/RustGIS#517): its PROJ name and the range, in degrees,
/// it is checked against.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PresetParam {
    pub key: &'static str,
    pub min: f64,
    pub max: f64,
}

const LON_0: PresetParam = PresetParam { key: "lon_0", min: -180.0, max: 180.0 };
const LAT_1: PresetParam = PresetParam { key: "lat_1", min: -89.0, max: 89.0 };
const LAT_2: PresetParam = PresetParam { key: "lat_2", min: -89.0, max: 89.0 };

/// The parameters preset `id` takes: the central meridian for the world and polar presets, and
/// the standard parallels too for the conics. None for the regional grids (ETRS89 LAEA, USA
/// Albers), which are standard definitions with a fixed area of use, nor for a preset not listed
/// here, so a new one takes none until it is.
pub fn preset_params(id: &str) -> &'static [PresetParam] {
    match id {
        "equirectangular"
        | "miller"
        | "mollweide"
        | "cylindricalEqualArea"
        | "robinson"
        | "equalEarth"
        | "naturalEarth"
        | "winkelTripel"
        | "eckertIV"
        | "northPolar"
        | "southPolar"
        | "vn2000tm3" => &[LON_0],
        "albers" | "lambertConformal" => &[LON_0, LAT_1, LAT_2],
        _ => &[],
    }
}

/// The value preset `id`'s own definition gives parameter `key` (`+lon_0=0` gives 0).
pub fn preset_value(id: &str, key: &str) -> Option<f64> {
    let (_, _, def) = PRESETS.iter().find(|(p, _, _)| *p == id)?;
    def.split_whitespace().find_map(|t| t.strip_prefix('+')?.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
}

/// A preset with its parameters, as a projection code writes it: the preset's id or name, then
/// `+key=value` for each parameter (`robinson +lon_0=150`). Gives the preset's id and the values
/// that differ from its own, in [`preset_params`]' order; `Ok(None)` when `code` names no preset.
/// A parameter the preset doesn't take, or a value out of its range or given twice, is an error;
/// [`lookup`] also refuses conic standard parallels that average within 0.5° of the equator
/// (mirrored, or both next to it), which make no cone (so a form being filled in can still read
/// them).
pub fn parse_preset(code: &str) -> Result<Option<(&'static str, Vec<(&'static str, f64)>)>, String> {
    let (head, rest) = code.trim().split_once('+').unwrap_or((code.trim(), ""));
    let head = head.trim();
    if rustgis_data::raster::crs::is_bare_vn2000(head) {
        return Err(rustgis_data::raster::crs::bare_vn2000_error(head));
    }
    // Matched before `resolve_vn2000_alias` below, which splits `VN-2000 / <province>` on `/`.
    let vn2000 = rustgis_data::raster::crs::vn2000_epsg(head);
    let Some((id, _, _)) = PRESETS.iter().find(|(id, label, _)| {
        id.eq_ignore_ascii_case(head) || label.eq_ignore_ascii_case(head) || vn2000.is_some_and(|c| VN2000_PRESET_EPSG.contains(&(*id, c)))
    }) else {
        // `vn2000:hanoi`: VN-2000 / TM-3 on a province's meridian, written as that preset so a
        // saved project's code never depends on the province names.
        if rest.trim().is_empty()
            && let Some(lon) = resolve_vn2000_alias(head)
        {
            let own = preset_value("vn2000tm3", "lon_0");
            return Ok(Some(("vn2000tm3", if own == Some(lon) { Vec::new() } else { vec![("lon_0", lon)] })));
        }
        return match (head, rest) {
            ("", _) if !rest.is_empty() => Err("PROJ strings aren't taken: pick a preset, or an EPSG or ESRI code".into()),
            (_, r) if r.contains('=') => Err(format!("only presets take parameters: '{head}' isn't one")),
            // Not a preset (`epsg:+3035` too): lookup says what's wrong with it.
            _ => Ok(None),
        };
    };
    let takes = preset_params(id);
    let mut given: Vec<(&'static str, f64)> = Vec::new();
    for t in rest.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        let (k, v) = t.split_once('=').ok_or_else(|| format!("a parameter is +key=value, not '+{t}'"))?;
        let p = takes.iter().find(|p| p.key.eq_ignore_ascii_case(k.trim())).ok_or_else(|| match takes {
            [] => format!("{id} takes no parameters"),
            _ => format!("{id} takes {}, not '{}'", takes.iter().map(|p| p.key).collect::<Vec<_>>().join(", "), k.trim()),
        })?;
        let v = v
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| format!("{} is a number of degrees, not '{}'", p.key, v.trim()))?;
        if !(p.min..=p.max).contains(&v) {
            return Err(format!("{} is from {} to {} degrees, not {v}", p.key, p.min, p.max));
        }
        if given.iter().any(|(g, _)| *g == p.key) {
            return Err(format!("{} is given twice", p.key));
        }
        // `-0` is 0 and the meridian 180° is -180°, so the code is written one way.
        let v = if p.key == "lon_0" && v == 180.0 { -180.0 } else { v + 0.0 };
        given.push((p.key, v));
    }
    let own = |k: &str| preset_value(id, k);
    let params =
        takes.iter().filter_map(|p| given.iter().find(|(g, _)| *g == p.key)).filter(|(k, v)| own(k) != Some(*v)).copied().collect();
    Ok(Some((id, params)))
}

/// The projection code of preset `id` with `params`: `robinson +lon_0=150`, or the id alone.
pub fn preset_code(id: &str, params: &[(&str, f64)]) -> String {
    params.iter().fold(id.to_string(), |s, (k, v)| format!("{s} +{k}={v}"))
}

/// `def` with `params` in place of its own values.
fn with_params(def: &str, params: &[(&str, f64)]) -> String {
    def.split_whitespace()
        .map(|t| {
            let set = params.iter().find(|(k, _)| t.strip_prefix('+').and_then(|t| t.strip_prefix(k)).is_some_and(|r| r.starts_with('=')));
            set.map_or_else(|| t.to_string(), |(k, v)| format!("+{k}={v}"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether local views (`local:lon,lat`, azimuthal equidistant) can be drawn in this build. They
/// need proj4rs' `aeqd`, which the browser build leaves out (Cargo.toml); asking the projection
/// itself, rather than the target, keeps the command, the ribbon and the tests in step with it.
pub fn local_views_supported() -> bool {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(|| Projector::new("local:0,0").is_ok())
}

/// Strips Vietnamese diacritics and converts to lowercase alphanumeric ASCII.
pub fn normalize_vietnamese(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        for lower in c.to_lowercase() {
            match lower {
                'à' | 'á' | 'ả' | 'ã' | 'ạ' | 'â' | 'ầ' | 'ấ' | 'ẩ' | 'ẫ' | 'ậ' | 'ă' | 'ằ' | 'ắ' | 'ẳ' | 'ẵ' | 'ặ' => {
                    out.push('a')
                }
                'è' | 'é' | 'ẻ' | 'ẽ' | 'ẹ' | 'ê' | 'ề' | 'ế' | 'ể' | 'ễ' | 'ệ' => out.push('e'),
                'ì' | 'í' | 'ỉ' | 'ĩ' | 'ị' => out.push('i'),
                'ò' | 'ó' | 'ỏ' | 'õ' | 'ọ' | 'ô' | 'ồ' | 'ố' | 'ổ' | 'ỗ' | 'ộ' | 'ơ' | 'ờ' | 'ớ' | 'ở' | 'ỡ' | 'ợ' => {
                    out.push('o')
                }
                'ù' | 'ú' | 'ủ' | 'ũ' | 'ụ' | 'ư' | 'ừ' | 'ứ' | 'ử' | 'ữ' | 'ự' => out.push('u'),
                'ỳ' | 'ý' | 'ỷ' | 'ỹ' | 'ỵ' => out.push('y'),
                'đ' => out.push('d'),
                c if c.is_ascii_alphanumeric() => out.push(c),
                _ => {}
            }
        }
    }
    out
}

/// Resolves a bare Vietnamese province name, ID, or alias to its VN-2000 central meridian (lon_0).
pub fn resolve_vn2000_province(name: &str) -> Option<f64> {
    let norm = normalize_vietnamese(name.trim());
    if norm.is_empty() {
        return None;
    }
    for prov in VN2000_PROVINCES {
        if norm == prov.id || prov.aliases.iter().any(|&a| norm == a) {
            return Some(prov.lon_0);
        }
    }
    None
}

/// Resolves an explicitly prefixed VN-2000 alias (e.g. `vn2000:hcm`, `vn2000/danang`, `vn2000tm3:vinh`)
/// to its central meridian (lon_0). Explicit prefix is required to prevent bare names from hijacking lookup.
pub fn resolve_vn2000_alias(code: &str) -> Option<f64> {
    let trimmed = code.trim();
    let rest = [":", "/"].iter().find_map(|sep| {
        let (prefix, rest) = trimmed.split_once(sep)?;
        let prefix = prefix.trim();
        (prefix.eq_ignore_ascii_case("vn2000")
            || prefix.eq_ignore_ascii_case("vn-2000")
            || prefix.eq_ignore_ascii_case("vn2000tm3")
            || prefix.eq_ignore_ascii_case("vn-2000-tm3"))
        .then_some(rest)
    })?;
    resolve_vn2000_province(rest)
}

/// The PROJ definition and label of a projection code: a preset id, `EPSG:<code>`, `ESRI:<code>`
/// or `local:lon,lat`.
pub fn lookup(code: &str) -> Result<(String, String), String> {
    let code = code.trim();
    // `local:lon,lat`: an azimuthal equidistant projection centred on a point (a local scene).
    if let Some(rest) = code.strip_prefix("local:") {
        // Exactly two finite numbers; anything else is refused rather than half-read.
        let n: Option<Vec<f64>> = rest.split(',').map(|x| x.trim().parse::<f64>().ok().filter(|v| v.is_finite())).collect();
        let Some(&[lon, lat]) = n.as_deref() else { return Err("a local view is local:lon,lat".into()) };
        let (lon, lat) = ((lon + 180.0).rem_euclid(360.0) - 180.0, lat.clamp(-89.0, 89.0));
        return Ok((
            format!("+proj=aeqd +lat_0={lat} +lon_0={lon} +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"),
            format!("Local ({lon:.2}°, {lat:.2}°)"),
        ));
    }
    if let Some((id, params)) = parse_preset(code)? {
        let (_, label, def) = PRESETS.iter().find(|(p, _, _)| *p == id).ok_or("no such preset")?;
        let value = |k: &str| params.iter().find(|(g, _)| *g == k).map(|(_, v)| *v).or_else(|| preset_value(id, k));
        // A sum under 1° is a mean under 0.5°.
        if let (Some(a), Some(b)) = (value("lat_1"), value("lat_2"))
            && (a + b).abs() < 1.0
        {
            // Name a parallel taken from the preset, as only the other may have been typed.
            let own = |k: &str| if params.iter().any(|(g, _)| *g == k) { "" } else { " (the preset's own)" };
            return Err(format!(
                "standard parallels {a}{} and {b}{} average within 0.5° of the equator, which makes no cone: pick a cylindrical projection",
                own("lat_1"),
                own("lat_2")
            ));
        }
        if params.is_empty() {
            return Ok((def.to_string(), label.to_string()));
        }
        let shown: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
        return Ok((with_params(def, &params), format!("{label} ({})", shown.join(", "))));
    }
    // `3035`, `EPSG:3035` or `epsg 3035`: an optional prefix, then only digits.
    let lower = code.to_ascii_lowercase();
    // `ESRI:102033`: one of Esri's projected coordinate systems, from a table checked against PROJ.
    // Only `esri`, then a colon, a space or the number: any other name goes on to the end.
    if let Some(rest) = lower.strip_prefix("esri").filter(|r| r.starts_with([':', ' ']) || r.starts_with(|c: char| c.is_ascii_digit())) {
        let digits = rest.trim_start_matches([':', ' ']);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("unknown coordinate system '{code}'"));
        }
        let shown = match digits.trim_start_matches('0') {
            "" => "0",
            d => d,
        };
        let unknown = || format!("unknown coordinate system ESRI:{shown}");
        let n = digits.parse::<u32>().map_err(|_| unknown())?;
        let def = rustgis_data::raster::crs::esri_proj4(n).ok_or_else(unknown)?;
        return Ok((def.to_string(), format!("ESRI:{n}")));
    }
    let digits = lower.strip_prefix("epsg").map(|r| r.trim_start_matches([':', ' '])).unwrap_or(&lower);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("unknown projection '{code}'"));
    }
    let unknown = || format!("unknown coordinate system EPSG:{}", digits.trim_start_matches('0'));
    let n = digits.parse::<u16>().map_err(|_| unknown())?;
    let def = rustgis_data::raster::crs::epsg_proj4(n).ok_or_else(unknown)?;
    if def.contains("+proj=longlat") {
        return Err(format!("EPSG:{n} is in degrees: pick a projected coordinate system"));
    }
    if def.contains("+proj=geocent") {
        return Err(format!("EPSG:{n} isn't a map projection"));
    }
    Ok((def.to_string(), format!("EPSG:{n}")))
}

/// Where a projection is meant to be used. Points outside are not drawn: far from it most
/// projections distort wildly, fold back or run off to infinity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Domain {
    /// The whole world (cylindrical and pseudo-cylindrical projections).
    World,
    /// A longitude/latitude box (regional presets).
    Box(Extent),
    /// Within `radius` degrees of a centre (azimuthal and stereographic projections).
    Cap { center: Coord, radius: f64 },
    /// Within `half` degrees of longitude of a central meridian (transverse Mercator, UTM).
    Band { lon0: f64, half: f64 },
}

impl Domain {
    /// The area of use for a regional preset, or one worked out from the PROJ parameters.
    /// `(lon0, lat0)` is the projection's centre (`lon0` east of Greenwich), as proj4rs reads it,
    /// and `pm` the prime meridian the definition's other longitudes (a UTM zone's) are counted from.
    fn of(id: Option<&str>, def: &str, (lon0, lat0): (f64, f64), pm: f64) -> Domain {
        match id {
            Some("europeEqualArea") => return Domain::Box(Extent::new(-35.0, 25.0, 50.0, 75.0)),
            Some("usaAlbers") => return Domain::Box(Extent::new(-130.0, 18.0, -60.0, 55.0)),
            _ => {}
        }
        let param = |k: &str| -> Option<f64> {
            def.split_whitespace().find_map(|t| t.strip_prefix(&format!("+{k}="))).and_then(|v| v.parse().ok())
        };
        let proj = def.split_whitespace().find_map(|t| t.strip_prefix("+proj=")).unwrap_or("");
        match proj {
            "tmerc" | "etmerc" | "utm" => {
                let lon0 = if proj == "utm" { param("zone").map(|z| z * 6.0 - 183.0 + pm).unwrap_or(lon0) } else { lon0 };
                Domain::Band { lon0, half: 30.0 }
            }
            "stere" | "sterea" | "somerc" | "krovak" | "cass" | "poly" | "omerc" => {
                Domain::Cap { center: Coord::new(lon0, lat0), radius: 60.0 }
            }
            "laea" | "aeqd" => Domain::Cap { center: Coord::new(lon0, lat0), radius: 120.0 },
            "aea" | "lcc" => {
                let (a, b) = (param("lat_1").unwrap_or(lat0), param("lat_2").unwrap_or(lat0));
                let (lo, hi) = (a.min(b) - 35.0, a.max(b) + 35.0);
                Domain::Box(Extent::new(lon0 - 90.0, lo.max(-89.0), lon0 + 90.0, hi.min(89.0)))
            }
            "geos" => Domain::Cap { center: Coord::new(lon0, 0.0), radius: 75.0 },
            _ => Domain::World,
        }
    }

    pub fn contains(&self, c: Coord) -> bool {
        match *self {
            Domain::World => true,
            // A box centred near the antimeridian (lon_0 = 150) runs past ±180°.
            Domain::Box(e) => [0.0, 360.0, -360.0].iter().any(|d| e.contains(Coord::new(c.x + d, c.y))),
            Domain::Cap { center, radius } => rustgis_geom::haversine_m(center, c) <= radius.to_radians() * rustgis_geom::EARTH_RADIUS_M,
            Domain::Band { lon0, half } => ((c.x - lon0 + 540.0).rem_euclid(360.0) - 180.0).abs() <= half && c.y.abs() <= 84.0,
        }
    }
}

/// The prime meridian of a definition (`+pm=`: a name such as `paris`, or degrees), in degrees
/// east of Greenwich: `lon_0` and the definition's other longitudes are counted from it. 0 when
/// there is none, or when it can't be read (proj4rs then refuses the definition anyway).
fn prime_meridian(def: &str) -> f64 {
    angle(def, "pm").unwrap_or(0.0)
}

/// A definition's `+lon_0`, `+lat_0` or `+pm` in degrees, read the way proj4rs reads it (degrees,
/// DMS, or a prime meridian's name), so the world projections computed here and the areas of use
/// agree with it. `None` when it is missing or can't be read.
fn angle(def: &str, key: &str) -> Option<f64> {
    let v = def.split_whitespace().find_map(|t| t.strip_prefix(&format!("+{key}=")))?;
    let p = proj4rs::Proj::from_proj_string(&format!("+proj=longlat +ellps=WGS84 +{key}={v}")).ok()?;
    let d = match key {
        "pm" => p.from_greenwich(),
        "lat_0" => p.latitude_of_origin(),
        _ => p.longitude_of_origin(),
    }
    .to_degrees();
    d.is_finite().then_some(d)
}

/// How a projection is computed: by the PROJ port, or here (world projections it lacks).
#[derive(Clone)]
enum Engine {
    Proj {
        native: Box<proj4rs::Proj>,
        wgs84: Box<proj4rs::Proj>,
    },
    World(crate::world::World),
    /// Sinusoidal (MODIS's grid, ESRI:54008), which proj4rs lacks.
    Sinusoidal(rustgis_geom::Sinusoidal),
}

/// Converts between longitude/latitude and the coordinates of a custom projection.
#[derive(Clone)]
pub struct Projector {
    engine: Engine,
    /// Metres per map unit.
    to_meter: f64,
    domain: Domain,
    /// The central meridian east of Greenwich, for cutting geometry along the map's edge.
    lon0: f64,
    /// Where the map's edge runs in WGS84 longitudes, worked out on first use (see [`Projector::edge`]).
    edge: std::sync::OnceLock<Edge>,
}

/// The edge is sampled every `EDGE_STEP`° of latitude from 90°S to 90°N: `EDGE_SAMPLES` latitudes,
/// the equator's at `EQUATOR`.
const EDGE_STEP: f64 = 5.0;
const EDGE_SAMPLES: usize = 37;
const EQUATOR: usize = EDGE_SAMPLES / 2;

/// A world projection's edge in WGS84 longitudes: the meridian it runs along on average, and how far
/// it strays from it every 5° of latitude. A datum shift moves the edge off the meridian opposite
/// the central one (by hundreds of metres for a local datum) and bends it, more and more towards the
/// poles, where a shift in metres spans more longitude.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    /// Not normalised to ±180°: 330 for a projection centred on 150°E.
    pub lon: f64,
    /// How far the edge strays from `lon`, east or west (an absolute distance in degrees), at
    /// latitudes -90, -85, … 90 (±89.9 at the poles).
    strays: [f64; EDGE_SAMPLES],
}

impl Edge {
    /// How far the edge may stray at a latitude: the larger of the two samples around it (it
    /// grows steadily towards the poles).
    pub fn stray_at(&self, lat: f64) -> f64 {
        let i = ((lat.clamp(-90.0, 90.0) + 90.0) / EDGE_STEP).floor() as usize;
        let (a, b) = (self.strays.get(i).copied().unwrap_or(0.0), self.strays.get(i + 1).copied().unwrap_or(0.0));
        a.max(b)
    }
}

/// The edge's stray at every sampled latitude, from the ones `measured`. A latitude the projection
/// can't show (`shown` false: Mercator's poles) needs no margin. One it can show but where the edge
/// wasn't found gets its nearest measured neighbours' stray scaled to it (a shift in metres spans
/// longitude as 1 / cos latitude), the larger of the two, with half again to spare, rather than
/// the much wider stray found nearer a pole.
fn fill_strays(measured: &[Option<f64>], lats: &[f64], shown: &[bool]) -> [f64; EDGE_SAMPLES] {
    let scaled = |v: f64, from: f64, to: f64| v * from.to_radians().cos() / to.to_radians().cos().max(1e-3);
    let nearest = |i: usize, step: isize| -> Option<f64> {
        let mut j = i as isize + step;
        while let Ok(k) = usize::try_from(j) {
            let m = measured.get(k)?;
            if let (Some(v), Some(from), Some(to)) = (m, lats.get(k), lats.get(i)) {
                return Some(scaled(*v, *from, *to));
            }
            j += step;
        }
        None
    };
    let mut strays = [0.0; EDGE_SAMPLES];
    for (i, s) in strays.iter_mut().enumerate() {
        *s = match measured.get(i).copied().flatten() {
            Some(v) => v,
            None if shown.get(i).copied().unwrap_or(false) => 1.5 * nearest(i, -1).unwrap_or(0.0).max(nearest(i, 1).unwrap_or(0.0)),
            None => 0.0,
        };
    }
    strays
}

impl std::fmt::Debug for Projector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match &self.engine {
            Engine::Proj { native, .. } => native.projname(),
            Engine::World(w) => w.kind.proj_name(),
            Engine::Sinusoidal(_) => "sinu",
        };
        write!(f, "Projector({name})")
    }
}

/// Map coordinates beyond this are treated as unprojectable (the projection's edge).
const LIMIT: f64 = 4.0e7;

impl Projector {
    /// The projector for a code `lookup` accepts: a preset id or name, `EPSG:<n>` or
    /// `local:lon,lat`. World projections proj4rs lacks are computed here; the rest go to
    /// proj4rs. Fails when the code is unknown, or when its method can't be computed (its
    /// `+proj=` name is in the message), so a map is never drawn in another projection than the
    /// one it names.
    pub fn new(code: &str) -> Result<Projector, String> {
        let (def, _) = lookup(code)?;
        let preset = parse_preset(code)?.map(|(id, _)| id);
        let proj = def.split_whitespace().find_map(|t| t.strip_prefix("+proj=")).unwrap_or("");
        // Sinusoidal, computed here on the sphere or ellipsoid the definition gives.
        if let Some(sinu) = rustgis_geom::Sinusoidal::from_proj4(&def) {
            let sinu = sinu.map_err(|e| format!("{code}: {e}"))?;
            let domain = Domain::of(preset, &def, (sinu.lon0, 0.0), 0.0);
            let lon0 = sinu.lon0;
            return Ok(Projector { engine: Engine::Sinusoidal(sinu), to_meter: 1.0, domain, lon0, edge: std::sync::OnceLock::new() });
        }
        if let Some(kind) = crate::world::Kind::from_proj_name(proj) {
            // Computed here: the centre is read from the definition as proj4rs would read it.
            let pm = prime_meridian(&def);
            let lon0 = angle(&def, "lon_0").unwrap_or(0.0) + pm;
            let lat0 = angle(&def, "lat_0").unwrap_or(0.0);
            let domain = Domain::of(preset, &def, (lon0, lat0), pm);
            // A sphere's radius when the definition gives one (ESRI's Sphere_* systems), else WGS 84's.
            let radius = def
                .split_whitespace()
                .find_map(|t| t.strip_prefix("+R=")?.parse::<f64>().ok())
                .filter(|r| r.is_finite() && *r > 0.0)
                .unwrap_or(rustgis_geom::MERCATOR_R);
            let engine = Engine::World(crate::world::World { kind, lon0, radius });
            return Ok(Projector { engine, to_meter: 1.0, domain, lon0, edge: std::sync::OnceLock::new() });
        }
        let native = proj4rs::Proj::from_proj_string(&def).map_err(|e| match e {
            // A known code whose method isn't implemented (EPSG:8441's +proj=labrd, say): name it.
            proj4rs::errors::Error::ProjectionNotFound => format!("{code}: its projection method (+proj={proj}) isn't supported"),
            e => format!("{code}: {e}"),
        })?;
        let wgs84 = proj4rs::Proj::from_proj_string("+proj=longlat +datum=WGS84 +no_defs").map_err(|e| e.to_string())?;
        let to_meter = native.to_meter();
        // The prime and central meridians proj4rs itself projects with, so the area of use, the
        // edge and the projection all agree (it reads degrees, DMS, radians and names alike).
        let finite = |d: f64| if d.is_finite() { d } else { 0.0 };
        let pm = finite(native.from_greenwich().to_degrees());
        let lon0 = finite(native.longitude_of_origin().to_degrees()) + pm;
        let lat0 = finite(native.latitude_of_origin().to_degrees());
        let domain = Domain::of(preset, &def, (lon0, lat0), pm);
        let engine = Engine::Proj { native: Box::new(native), wgs84: Box::new(wgs84) };
        let to_meter = if to_meter > 0.0 { to_meter } else { 1.0 };
        Ok(Projector { engine, to_meter, domain, lon0, edge: std::sync::OnceLock::new() })
    }

    /// The central meridian, in WGS84 longitudes, of a projection that shows the whole world: the
    /// meridian opposite its edge (`edge().lon - 180`), which a datum shift moves slightly off
    /// `lon_0` (plus `+pm`). `None` for one limited to an area of use, which never reaches it.
    pub fn central_meridian(&self) -> Option<f64> {
        self.edge().map(|e| e.lon - 180.0)
    }

    /// Where the edge of a projection that shows the whole world runs, in WGS84 longitudes, worked
    /// out once; `None` for one limited to an area of use.
    ///
    /// Without a datum shift it is the meridian opposite the central one: a probe at the equator
    /// and at 60°N and S finds the map jumping sides exactly there. With one, it is found every 5°
    /// of latitude by bisecting for where the map jumps from one side to the other. If even the
    /// equator's edge can't be found, it falls back to the nominal edge with no margin;
    /// `every_world_projection_wraps_opposite_its_central_meridian` checks that no EPSG world
    /// projection does.
    pub fn edge(&self) -> Option<&Edge> {
        if self.domain != Domain::World {
            return None;
        }
        Some(self.edge.get_or_init(|| {
            let exact = Edge { lon: self.lon0 + 180.0, strays: [0.0; EDGE_SAMPLES] };
            let Engine::Proj { .. } = self.engine else { return exact };
            let shifted = [0.0, 60.0, -60.0].iter().any(|lat| self.edge_at(*lat, exact.lon).is_none_or(|x| (x - exact.lon).abs() > 1e-9));
            if shifted { self.find_edge(exact.lon).unwrap_or(exact) } else { exact }
        }))
    }

    fn find_edge(&self, guess: f64) -> Option<Edge> {
        let lats: Vec<f64> = (0..EDGE_SAMPLES).map(|i| (-90.0 + EDGE_STEP * i as f64).clamp(-89.9, 89.9)).collect();
        let found: Vec<Option<f64>> = lats.iter().map(|lat| self.edge_at(*lat, guess)).collect();
        // The equator's edge: most of the map, and where the edge strays least.
        let lon = (*found.get(EQUATOR)?)?;
        let measured: Vec<Option<f64>> = found.iter().map(|f| f.map(|x| (x - lon).abs())).collect();
        let shown: Vec<bool> = lats.iter().map(|lat| self.forward(Coord::new(guess - 180.0, *lat)).is_some()).collect();
        Some(Edge { lon, strays: fill_strays(&measured, &lats, &shown) })
    }

    /// The WGS84 longitude where the map jumps sides at a latitude, near `guess`.
    fn edge_at(&self, lat: f64, guess: f64) -> Option<f64> {
        let centre = self.forward(Coord::new(guess - 180.0, lat))?.x;
        // West of the edge a point draws on the map's east side, past it on the west side.
        let east_side = |lon: f64| self.forward(Coord::new(lon, lat)).map(|m| m.x > centre);
        // Near the poles a shift of a few hundred metres spans a degree or more: widen the search
        // until it holds the jump.
        let (mut a, mut b) = [10.0, 30.0, 90.0]
            .into_iter()
            .map(|h| (guess - h, guess + h))
            .find(|&(a, b)| east_side(a) == Some(true) && east_side(b) == Some(false))?;
        for _ in 0..60 {
            let m = (a + b) / 2.0;
            if east_side(m)? {
                a = m;
            } else {
                b = m;
            }
        }
        Some((a + b) / 2.0)
    }

    /// Longitude/latitude to map coordinates, `None` outside the projection.
    pub fn forward(&self, c: Coord) -> Option<Coord> {
        if !c.is_finite() {
            return None;
        }
        let (native, wgs84) = match &self.engine {
            Engine::Proj { native, wgs84 } => (native, wgs84),
            Engine::World(w) => return w.forward(c),
            Engine::Sinusoidal(s) => return s.forward(c).filter(|m| m.x.abs() < LIMIT && m.y.abs() < LIMIT),
        };
        let mut p = (c.x.to_radians(), c.y.clamp(-90.0, 90.0).to_radians(), 0.0);
        proj4rs::transform::transform(wgs84, native, &mut p).ok()?;
        (p.0.is_finite() && p.1.is_finite() && p.0.abs() < LIMIT && p.1.abs() < LIMIT).then(|| Coord::new(p.0, p.1))
    }

    /// Like [`Projector::forward`], but also rejects points that don't map back to themselves
    /// (wrapped or folded parts of a projection, such as the far hemisphere of a polar one).
    pub fn forward_checked(&self, c: Coord) -> Option<Coord> {
        if !self.domain.contains(c) {
            return None;
        }
        let m = self.forward(c)?;
        // World projections don't fold, and on their edges (±180°) the inverse is too imprecise
        // to compare: only projections limited to an area of use need the round trip.
        if self.domain == Domain::World {
            return Some(m);
        }
        let back = self.inverse(m)?;
        let dlon = ((back.x - c.x + 540.0).rem_euclid(360.0) - 180.0).abs();
        // At a pole every longitude is the same point.
        let same = (dlon < 1e-3 && (back.y - c.y).abs() < 1e-3) || (c.y.abs() > 89.99 && (back.y - c.y).abs() < 1e-3);
        same.then_some(m)
    }

    /// Map coordinates to longitude/latitude.
    pub fn inverse(&self, m: Coord) -> Option<Coord> {
        if !m.is_finite() {
            return None;
        }
        let (native, wgs84) = match &self.engine {
            Engine::Proj { native, wgs84 } => (native, wgs84),
            Engine::World(w) => return w.inverse(m),
            Engine::Sinusoidal(s) => return s.inverse(m),
        };
        let mut p = (m.x, m.y, 0.0);
        proj4rs::transform::transform(native, wgs84, &mut p).ok()?;
        let c = Coord::new(p.0.to_degrees(), p.1.to_degrees());
        (c.is_finite() && c.y.abs() <= 90.0 + 1e-9).then_some(c)
    }

    pub fn to_meter(&self) -> f64 {
        self.to_meter
    }

    /// Ground metres per map unit at a map point, measured: a 1 km step east-west (the scale
    /// bar's direction; north-south on the projection's edge) compared with the great-circle
    /// distance it covers, on the sphere Web Mercator's scale uses so both agree. Falls back to
    /// the unit size where no step stays in the projection.
    pub fn ground_per_unit(&self, at: Coord) -> f64 {
        let Some(c) = self.inverse(at) else { return self.to_meter };
        let step = 1000.0 / self.to_meter;
        let radius_ratio = rustgis_geom::MERCATOR_R / rustgis_geom::EARTH_RADIUS_M;
        let steps = [Coord::new(step, 0.0), Coord::new(-step, 0.0), Coord::new(0.0, step), Coord::new(0.0, -step)];
        steps
            .iter()
            .filter_map(|s| self.inverse(Coord::new(at.x + s.x, at.y + s.y)))
            .map(|d| rustgis_geom::haversine_m(c, d) * radius_ratio / step)
            .find(|k| k.is_finite() && *k > 0.0)
            .unwrap_or(self.to_meter)
    }

    pub fn domain(&self) -> Domain {
        self.domain
    }
}

/// The flat map's projection, ready to convert coordinates.
#[derive(Clone, Debug)]
pub enum Flat {
    /// Web Mercator (also the camera space of the globe).
    Mercator,
    Custom(std::sync::Arc<Projector>),
}

impl Flat {
    /// Longitude/latitude to map coordinates.
    pub fn forward(&self, c: Coord) -> Option<Coord> {
        match self {
            Flat::Mercator => c.is_finite().then(|| lonlat_to_merc(c)),
            Flat::Custom(p) => p.forward(c),
        }
    }

    /// For geometry and other bulk conversion: drops points a projection folds or wraps.
    pub fn forward_checked(&self, c: Coord) -> Option<Coord> {
        match self {
            Flat::Mercator => self.forward(c),
            Flat::Custom(p) => p.forward_checked(c),
        }
    }

    /// Rejects a selection outline that leaves the projection or crosses its wrapping edge.
    /// Unlike feature projection, selection paths must never be cut or silently clamped.
    pub fn validate_selection_path(&self, points: &[Coord]) -> Result<(), String> {
        for &c in points {
            if !c.is_finite() || !(-180.0..=180.0).contains(&c.x) || !(-90.0..=90.0).contains(&c.y) {
                return Err("Selection outline contains an invalid longitude or latitude.".into());
            }
            if matches!(self, Flat::Mercator) && c.y.abs() > rustgis_geom::MAX_LAT {
                return Err("Selection outline exceeds the Web Mercator latitude range.".into());
            }
            if !self.forward_checked(c).is_some_and(|p| p.is_finite()) {
                return Err("Selection outline is outside the active projection.".into());
            }
        }
        let edge = match self {
            Flat::Mercator => None,
            Flat::Custom(p) => match p.edge() {
                Some(edge) => Some(edge),
                None => return Ok(()),
            },
        };
        let east = edge.map_or(180.0, |e| e.lon);
        let west = east - 360.0;
        let normalize = |lon: f64| (lon - west).rem_euclid(360.0) + west;
        for (a, b) in points.iter().zip(points.iter().cycle().skip(1)).take(points.len()) {
            let (a_lon, b_lon) = (normalize(a.x), normalize(b.x));
            if (a_lon - b_lon).abs() > 180.0 {
                return Err("Selection outline crosses the active projection's wrap seam.".into());
            }
            let mut margin = edge.map_or(0.0, |e| e.stray_at(a.y).max(e.stray_at(b.y)));
            if let Some(edge) = edge {
                let (south, north) = (a.y.min(b.y), a.y.max(b.y));
                for i in 0..EDGE_SAMPLES {
                    let lat = -90.0 + EDGE_STEP * i as f64;
                    if lat >= south && lat <= north {
                        margin = margin.max(edge.stray_at(lat));
                    }
                }
            }
            margin += 1e-9;
            if a_lon <= west + margin || a_lon >= east - margin || b_lon <= west + margin || b_lon >= east - margin {
                return Err("Selection outline is too close to the active projection's wrap seam.".into());
            }
        }
        Ok(())
    }

    /// A longitude/latitude geometry in map coordinates, for drawing. Under a world projection it
    /// is first cut along the map's edge, so a feature across it (Russia under a 90°W central
    /// meridian) is drawn on both sides rather than as a band across the map (#209). Points the
    /// projection can't show become NaN, which the renderer skips.
    pub fn project(&self, g: &rustgis_data::Geometry) -> rustgis_data::Geometry {
        let f = |c: Coord| self.forward_checked(c).unwrap_or(Coord::new(f64::NAN, f64::NAN));
        if let Flat::Custom(p) = self
            && let Some(edge) = p.edge()
        {
            return crate::antimeridian::cut_with_margin(g, edge.lon - 180.0, &|lat| edge.stray_at(lat)).map_coords(&f);
        }
        g.map_coords(&f)
    }

    /// Map coordinates to longitude/latitude.
    pub fn inverse(&self, m: Coord) -> Option<Coord> {
        match self {
            Flat::Mercator => m.is_finite().then(|| merc_to_lonlat(m)),
            Flat::Custom(p) => p.inverse(m),
        }
    }

    /// Ground metres per map unit at a map point (Mercator stretches with latitude).
    pub fn ground_per_unit(&self, at: Coord) -> f64 {
        match self {
            Flat::Mercator => merc_to_lonlat(at).y.to_radians().cos().abs().max(0.01),
            Flat::Custom(p) => p.ground_per_unit(at),
        }
    }

    /// The map-coordinate box around a longitude/latitude extent (from points along its edges and
    /// inside it, so curved projections are covered).
    pub fn extent(&self, e: &Extent) -> Extent {
        let mut out = Extent::EMPTY;
        if e.is_empty() {
            return out;
        }
        let n = 16;
        for i in 0..=n {
            for j in 0..=n {
                let c = Coord::new(e.min_x + e.width() * i as f64 / n as f64, e.min_y + e.height() * j as f64 / n as f64);
                if let Some(m) = self.forward_checked(c) {
                    out.expand(m);
                }
            }
        }
        out
    }
}

thread_local! {
    static PROJECTORS: std::cell::RefCell<std::collections::HashMap<String, Option<std::sync::Arc<Projector>>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

impl Flat {
    /// The flat projection behind a map projection (the globe's camera space is Web Mercator; an
    /// unusable custom code falls back to it). Parsed projections are kept per thread.
    pub fn of(p: &crate::Projection) -> Flat {
        let crate::Projection::Custom(code) = p else { return Flat::Mercator };
        let key = code.trim().to_ascii_lowercase();
        PROJECTORS.with(|cell| {
            let mut map = cell.borrow_mut();
            // Codes come from project files and scripts: keep the cache bounded.
            if map.len() >= 32 && !map.contains_key(&key) {
                map.clear();
            }
            let entry = map.entry(key).or_insert_with(|| Projector::new(code).ok().map(std::sync::Arc::new));
            entry.clone().map(Flat::Custom).unwrap_or(Flat::Mercator)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_paths_reject_clamps_seams_and_unprojectable_points() {
        let mercator = Flat::Mercator;
        assert!(mercator.validate_selection_path(&[Coord::new(0.0, 0.0)]).is_ok());
        for points in [
            vec![Coord::new(179.0, 0.0), Coord::new(-179.0, 1.0)],
            vec![Coord::new(-180.0, 0.0)],
            vec![Coord::new(180.0, 0.0)],
            vec![Coord::new(0.0, rustgis_geom::MAX_LAT + 0.01)],
            vec![Coord::new(181.0, 0.0)],
            vec![Coord::new(f64::NAN, 0.0)],
        ] {
            assert!(mercator.validate_selection_path(&points).is_err(), "{points:?}");
        }
        // A small raw-longitude jump still crosses the shifted projection's edge at 30°W.
        let shifted = Flat::Custom(std::sync::Arc::new(Projector::new("robinson +lon_0=150").unwrap()));
        assert!(shifted.validate_selection_path(&[Coord::new(-35.0, 5.0), Coord::new(-25.0, 15.0)]).is_err());
        // Conversely, the geographic antimeridian is not this projection's seam.
        assert!(shifted.validate_selection_path(&[Coord::new(175.0, 5.0), Coord::new(-175.0, 15.0)]).is_ok());
        // Include the closing edge: each preceding edge is short, but the last wraps.
        assert!(mercator.validate_selection_path(&[Coord::new(-170.0, 0.0), Coord::new(0.0, 1.0), Coord::new(170.0, 0.0)]).is_err());
        let regional = Flat::Custom(std::sync::Arc::new(Projector::new("EPSG:32631").unwrap()));
        assert!(regional.validate_selection_path(&[Coord::new(3.0, 50.0), Coord::new(4.0, 51.0)]).is_ok());
        assert!(regional.validate_selection_path(&[Coord::new(100.0, 0.0)]).is_err());
    }

    #[test]
    fn selection_paths_use_intervening_edge_stray_samples() {
        let projector = Projector::new("robinson").unwrap();
        let mut strays = [0.0; EDGE_SAMPLES];
        strays[EQUATOR] = 5.0;
        projector.edge.set(Edge { lon: 180.0, strays }).unwrap();
        let flat = Flat::Custom(std::sync::Arc::new(projector));
        // Both endpoints individually clear the seam, but the segment crosses a latitude where
        // a datum-shifted edge may stray farther inland.
        assert!(flat.validate_selection_path(&[Coord::new(178.0, -10.0)]).is_ok());
        assert!(flat.validate_selection_path(&[Coord::new(178.0, 10.0)]).is_ok());
        assert!(flat.validate_selection_path(&[Coord::new(178.0, -10.0), Coord::new(178.0, 10.0)]).is_err());
    }

    /// Presets with parameters (opengeos/RustGIS#517): read strictly, written one way, and drawn
    /// where they say, against pyproj 3.7.1 (PROJ 9.5.1).
    #[test]
    fn presets_take_parameters() {
        // The id, then the values that differ from the preset's own, in a fixed order.
        assert_eq!(parse_preset("Robinson +lon_0=150").unwrap(), Some(("robinson", vec![("lon_0", 150.0)])));
        assert_eq!(parse_preset("robinson +lon_0=0").unwrap(), Some(("robinson", vec![])));
        assert_eq!(parse_preset("robinson").unwrap(), Some(("robinson", vec![])));
        assert_eq!(parse_preset("EPSG:3035").unwrap(), None);
        let (id, p) = parse_preset("albers +lat_2=-35 +LON_0=-55 + lat_1 = -5").unwrap().unwrap();
        assert_eq!(preset_code(id, &p), "albers +lon_0=-55 +lat_1=-5 +lat_2=-35");
        assert_eq!(lookup("robinson +lon_0=150").unwrap().1, "Robinson (lon_0=150)");
        assert!(lookup("+proj=robin +lon_0=150").unwrap_err().contains("PROJ strings"));
        assert!(lookup("albers +lat_1=-50").unwrap_err().contains("50 (the preset's own)"));
        let (id, p) = parse_preset("albers +lat_1=-0").unwrap().unwrap();
        assert_eq!(preset_code(id, &p), "albers +lat_1=0");
        assert_eq!(parse_preset("robinson +lon_0=180").unwrap(), parse_preset("robinson +lon_0=-180").unwrap());
        assert_eq!(parse_preset("epsg:+3035").unwrap(), None);
        assert!(lookup("epsg:+3035").unwrap_err().contains("unknown"));
        // Every preset is classified: the regional grids alone take nothing.
        for (id, _, _) in PRESETS {
            assert_eq!(preset_params(id).is_empty(), ["europeEqualArea", "usaAlbers", "vn2000Utm48", "vn2000Utm49"].contains(id), "{id}");
        }
        for bad in [
            "robinson +lon_0=181",
            "robinson +lon_0=x",
            "robinson +lon_0=inf",
            "robinson +lon_0",
            "robinson +lat_1=10",
            "robinson +lon_0=1 +lon_0=2",
            "usaAlbers +lon_0=-90",
            "EPSG:3035 +lon_0=10",
            "albers +lat_1=-30 +lat_2=30",
            "albers +lat_1=-89.5",
            "albers +lat_1=-30 +lat_2=29.5",
            "lambertConformal +lat_1=0.3 +lat_2=0.3",
            "+proj=robin +lon_0=150",
        ] {
            assert!(lookup(bad).is_err(), "{bad}");
            assert!(Projector::new(bad).is_err(), "{bad}");
        }
        // Each parameter a preset takes is in its definition, for the value to replace.
        for (id, _, def) in PRESETS {
            for p in preset_params(id) {
                assert!(def.contains(&format!("+{}=", p.key)), "{id}: {}", p.key);
            }
        }
        // Centred on 150°E: that meridian runs down the middle, and the edge along 30°W.
        for id in [
            "equirectangular",
            "miller",
            "mollweide",
            "cylindricalEqualArea",
            "robinson",
            "equalEarth",
            "naturalEarth",
            "winkelTripel",
            "eckertIV",
        ] {
            let p = Projector::new(&format!("{id} +lon_0=150")).unwrap();
            assert!(p.forward(Coord::new(150.0, 20.0)).unwrap().x.abs() < 1e-6, "{id}");
            let (w, e) = (p.forward(Coord::new(-30.5, 0.0)).unwrap(), p.forward(Coord::new(-29.5, 0.0)).unwrap());
            assert!(w.x > 0.0 && e.x < 0.0, "{id}: {w:?} {e:?}");
            let cm = p.central_meridian().unwrap();
            assert!((cm - 150.0).rem_euclid(360.0).min((150.0 - cm).rem_euclid(360.0)) < 0.01, "{id}: {cm}");
        }
        let check = |code: &str, (lon, lat): (f64, f64), (x, y): (f64, f64)| {
            let m = Projector::new(code).unwrap().forward(Coord::new(lon, lat)).unwrap();
            assert!((m.x - x).abs() < 1.0 && (m.y - y).abs() < 1.0, "{code}: {m:?}, PROJ ({x}, {y})");
        };
        check("robinson +lon_0=150", (170.0, 40.0), (1741397.303, 4276351.584));
        check("mollweide +lon_0=150", (-170.0, -30.0), (3667234.822, -3643853.564));
        check("northPolar +lon_0=-45", (0.0, 70.0), (1547098.478, -1547098.478));
        check("albers +lon_0=-55 +lat_1=-5 +lat_2=-35", (-40.0, -10.0), (1613154.256, -1174639.608));
        check("lambertConformal +lon_0=-55 +lat_1=-5 +lat_2=-35", (-40.0, -10.0), (1611663.972, -1180384.624));
        // A conic's area of use follows its parallels: South America, not the far north.
        let p = Projector::new("albers +lon_0=-55 +lat_1=-5 +lat_2=-35").unwrap();
        assert!(p.domain.contains(Coord::new(-50.0, -15.0)) && !p.domain.contains(Coord::new(-55.0, 60.0)));
    }

    /// Sinusoidal (opengeos/RustGIS#563), computed here before proj4rs 0.3 grew one: Esri's
    /// systems draw, against pyproj 3.7.1 (PROJ 9.5.1), and wrap opposite their central meridian.
    #[test]
    fn sinusoidal_codes_draw() {
        let check = |code: &str, (lon, lat): (f64, f64), (x, y): (f64, f64), lon0: f64| {
            let p = Projector::new(code).unwrap_or_else(|e| panic!("{code}: {e}"));
            let m = p.forward(Coord::new(lon, lat)).unwrap();
            assert!((m.x - x).abs() < 0.01 && (m.y - y).abs() < 0.01, "{code}: {m:?}");
            let back = p.inverse(m).unwrap();
            assert!((back.x - lon).abs() < 1e-9 && (back.y - lat).abs() < 1e-9, "{code}: {back:?}");
            assert_eq!(p.domain(), Domain::World, "{code}");
            assert!((p.central_meridian().unwrap() - lon0).abs() < 1e-9, "{code}");
            assert_eq!(format!("{p:?}"), "Projector(sinu)");
        };
        check("ESRI:54008", (-55.72, -12.55), (-6_055_475.958, -1_387_927.939), 0.0);
        check("ESRI:102011", (36.82, -1.29), (2_428_379.790, -142_641.058), 15.0);
        check("esri:53008", (10.75, 59.91), (599_298.080, 6_661_688.055), 0.0);
        // Off the outline: nothing.
        let p = Projector::new("ESRI:54008").unwrap();
        assert!(p.inverse(Coord::new(19_000_000.0, 9_000_000.0)).is_none());
    }

    #[test]
    fn local_codes_are_read_strictly() {
        assert!(lookup("local:12.5,41.9").is_ok());
        for bad in ["local:a,10,20", "local:10", "local:10,20,30", "local:10,inf", "local:"] {
            assert!(lookup(bad).is_err(), "{bad}");
        }
    }

    /// ESRI codes (opengeos/RustGIS#213), against pyproj 3.7.2 (PROJ 9.7.1) given the table's
    /// definitions.
    #[test]
    fn esri_codes() {
        let check = |code: &str, (lon, lat): (f64, f64), (x, y): (f64, f64), tolerance: f64| {
            let p = Projector::new(code).unwrap_or_else(|e| panic!("{code}: {e}"));
            let m = p.forward(Coord::new(lon, lat)).unwrap_or_else(|| panic!("{code}"));
            assert!((m.x - x).abs() < tolerance && (m.y - y).abs() < tolerance, "{code}: {m:?}, want ({x}, {y})");
            let back = p.inverse(m).unwrap_or_else(|| panic!("{code}"));
            assert!((back.x - lon).abs() < 1e-6 && (back.y - lat).abs() < 1e-6, "{code}: {back:?}");
        };
        // South America Albers Equal Area Conic, with its datum shift (South American 1969: the
        // first Helmert PROJ lists, 7 m from the regional one PROJ picks for this point).
        check("ESRI:102033", (-60.0, -15.0), (46.993, 1_977_375.429), 0.01);
        // World Robinson, and Sphere Robinson on its own 6371 km sphere (+R), computed here.
        check("ESRI:54030", (20.0, 30.0), (1_813_955.515, 3_208_557.612), 1.0);
        check("esri:53030", (20.0, 30.0), (1_811_925.737, 3_204_967.304), 1.0);
        // A state plane in US survey feet (deprecated, but what ArcGIS writes): Manhattan.
        check("ESRI 102718", (-73.95, 40.75), (998_103.643, 212_525.840), 0.01);
        assert_eq!(lookup(" esri:102033 ").map(|(_, label)| label), Ok("ESRI:102033".into()));
        assert_eq!(crate::Projection::from_code("esri:102033"), Ok(crate::Projection::Custom("ESRI:102033".into())));
        assert_eq!(crate::Projection::from_code("ESRI:102033").map(|p| p.label()), Ok("ESRI:102033".into()));
        // One system, one saved code, however it was typed.
        for typed in ["ESRI 102033", "esri:0102033", " Esri:102033 "] {
            assert_eq!(crate::Projection::from_code(typed), Ok(crate::Projection::Custom("ESRI:102033".into())), "{typed}");
        }
        assert_eq!(crate::Projection::from_code("epsg:03035"), Ok(crate::Projection::Custom("EPSG:3035".into())));
        // ESRI's Web Mercator codes are the default map.
        for web in ["ESRI:102100", "esri:102113", "ESRI 102100", "esri:0102100", "EPSG:3857", "3857", "epsg 3857", "EPSG:900913"] {
            assert_eq!(crate::Projection::from_code(web), Ok(crate::Projection::WebMercator), "{web}");
        }
        // Not in the table (World Miller is kilometres off in proj4rs), not a number, too big.
        for bad in ["ESRI:54003", "ESRI:", "ESRI:abc", "ESRI:-1", "ESRI:99999999999", "ESRI:1e5"] {
            let e = lookup(bad).unwrap_err();
            assert!(e.contains("unknown coordinate system") && e.contains("ESRI:"), "{bad}: {e}");
        }
        // The message names the code as typed, or as the number it read.
        assert_eq!(lookup("ESRI:abc").unwrap_err(), "unknown coordinate system 'ESRI:abc'");
        assert_eq!(lookup("esri:0").unwrap_err(), "unknown coordinate system ESRI:0");
        assert_eq!(lookup("esri:00054003").unwrap_err(), "unknown coordinate system ESRI:54003");
        // Any other name starting with "esri" isn't read as a code.
        assert_eq!(lookup("esrifoo").unwrap_err(), "unknown projection 'esrifoo'");
        // Every code in the table draws (local azimuthal ones only where aeqd is built in).
        for n in rustgis_data::raster::crs::esri_codes() {
            let code = format!("ESRI:{n}");
            let aeqd = lookup(&code).is_ok_and(|(def, _)| def.contains("+proj=aeqd"));
            let p = Projector::new(&code);
            assert!(p.is_ok() || (aeqd && !local_views_supported()), "{code}: {:?}", p.err());
        }
    }

    #[test]
    fn presets_and_epsg_codes() {
        for (id, _, _) in PRESETS {
            let p = Projector::new(id).unwrap_or_else(|e| panic!("{id}: {e}"));
            let c = match *id {
                "vn2000tm3" | "vn2000Utm48" | "vn2000Utm49" => Coord::new(105.0, 21.0),
                _ => Coord::new(10.0, if id.contains("south") { -75.0 } else { 50.0 }),
            };
            let m = p.forward(c).unwrap_or_else(|| panic!("{id}"));
            let back = p.inverse(m).unwrap();
            assert!((back.x - c.x).abs() < 1e-6 && (back.y - c.y).abs() < 1e-6, "{id}: {back:?}");
        }
        assert_eq!(preset_params("vn2000tm3"), &[LON_0]);
        assert_eq!(preset_params("vn2000"), &[]);
        assert!(Projector::new("EPSG:3035").is_ok());
        assert!(Projector::new("epsg:32633").is_ok());
        assert!(Projector::new("3857").is_ok());
        assert!(Projector::new("EPSG:4326").is_err(), "degrees are not a map projection");
        assert!(Projector::new("EPSG:1").is_err());
        assert!(Projector::new("bogus").is_err());
        assert!(Projector::new("12x").is_err());
        assert!(Projector::new("epsgfoo32617").is_err());
        // A conic centred on the Pacific keeps points across the antimeridian.
        let pacific = Domain::of(None, "+proj=lcc +lat_1=30 +lat_2=60 +lon_0=150", (150.0, 0.0), 0.0);
        assert!(pacific.contains(Coord::new(-170.0, 45.0)) && pacific.contains(Coord::new(120.0, 45.0)));
        assert!(!pacific.contains(Coord::new(0.0, 45.0)));
        assert!(Projector::new("EPSG 32617").is_ok());
        assert!(lookup("EPSG:99999").unwrap_err().contains("coordinate system"), "out of range is an unknown code");
        // Equal Earth by its EPSG code goes to the built-in engine too.
        let ee = Projector::new("EPSG:8857").unwrap();
        assert!(format!("{ee:?}").contains("eqearth"));
        // Codes crs-definitions lacks (EPSG 9020–20003) come from the data crate's own table
        // (opengeos/RustGIS#198): MAGNA-SIRGAS 2018 / Origen-Nacional, SIRGAS-Chile 2016 / UTM 19S.
        let (bogota, santiago) = (Coord::new(-74.08, 4.65), Coord::new(-70.65, -33.45));
        for (ok, proj, city) in [("EPSG:9377", "tmerc", bogota), ("epsg:9155", "utm", santiago)] {
            let (def, label) = lookup(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            assert!(def.contains(&format!("+proj={proj}")) && label == ok.to_ascii_uppercase(), "{ok}: {def} {label}");
            let p = Projector::new(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
            let back = p.inverse(p.forward(city).unwrap()).unwrap();
            assert!((back.x - city.x).abs() < 1e-7 && (back.y - city.y).abs() < 1e-7, "{ok}: {back:?}");
        }
        assert!(lookup("EPSG:9148").unwrap_err().contains("degrees"), "SIRGAS-Chile 2013 is geographic");
    }

    #[test]
    fn codes_whose_method_cannot_be_drawn_are_refused() {
        // EPSG:8441 (Laborde) is in the EPSG table, but its method (+proj=labrd) has no
        // implementation: refuse it rather than accept it and draw the map in Web Mercator while
        // naming EPSG:8441 (opengeos/RustGIS#208).
        assert!(lookup("EPSG:8441").is_ok(), "the code itself is known");
        let e = Projector::new("EPSG:8441").unwrap_err();
        assert!(e.contains("EPSG:8441") && e.contains("labrd"), "{e}");
        let e = crate::Projection::from_code("EPSG:8441").unwrap_err();
        assert!(e.contains("labrd"), "{e}");
        // proj4rs 0.3 added Cassini, the polyconic, Equal Earth and the oblique Mercator: their
        // codes are accepted (EPSG:5880's polyconic, whose refusal was #208), and draw where PROJ
        // does: 54°W 10°S lands at (5 000 000, 8 894 145.167) in pyproj 3.8.0's EPSG:5880, its
        // false easting less a million-odd metres.
        for ok in ["EPSG:5880", "EPSG:2057", "EPSG:3068", "EPSG:8858"] {
            crate::Projection::from_code(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        let brazil = Projector::new("EPSG:5880").unwrap_or_else(|e| panic!("{e}"));
        let m = brazil.forward(Coord::new(-54.0, -10.0)).unwrap();
        assert!((m.x - 5_000_000.0).abs() < 0.01 && (m.y - 8_894_145.167).abs() < 0.01, "{m:?}");
        // Codes that can be drawn are still accepted, by number or preset.
        for ok in ["EPSG:3035", "epsg:32633", "EPSG:8858", "equalEarth", "usaAlbers", "mercator", "globe"] {
            crate::Projection::from_code(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        // Local views are accepted exactly where they can be drawn (`aeqd` is native-only, Cargo.toml).
        let local = crate::Projection::from_code("local:12.5,41.9");
        assert_eq!(local.is_ok(), local_views_supported(), "{local:?}");
        // Pinned to the target on purpose, so the probe can't drift from the Cargo.toml split unnoticed.
        assert_eq!(local_views_supported(), !cfg!(target_arch = "wasm32"), "aeqd on native targets only");
    }

    #[test]
    fn local_views_are_azimuthal_equidistant() {
        if !local_views_supported() {
            return; // the browser build (see codes_whose_method_cannot_be_drawn_are_refused)
        }
        // map.viewMode "local" relies on +proj=aeqd: it must build (not fall back to Web Mercator)
        // and keep true distances from its centre.
        let p = Projector::new("local:12.5,41.9").unwrap_or_else(|e| panic!("{e}"));
        let o = p.forward(Coord::new(12.5, 41.9)).unwrap();
        assert!(o.x.abs() < 1e-6 && o.y.abs() < 1e-6, "the centre is the origin: {o:?}");
        let c = Coord::new(13.5, 42.4);
        let m = p.forward(c).unwrap();
        let back = p.inverse(m).unwrap();
        assert!((back.x - c.x).abs() < 1e-7 && (back.y - c.y).abs() < 1e-7, "{back:?}");
        // Distance from the centre on the map is the WGS84 geodesic distance (Vincenty: 99 580.4 m).
        let d = (m.x * m.x + m.y * m.y).sqrt();
        assert!((d - 99_580.4).abs() < 1.0, "{d}");
    }

    #[test]
    fn folded_points_are_rejected_and_extents_project() {
        let north = Projector::new("northPolar").unwrap();
        assert!(north.forward_checked(Coord::new(0.0, 60.0)).is_some());
        assert!(north.forward_checked(Coord::new(0.0, -80.0)).is_none(), "the far pole");
        assert!(north.forward_checked(Coord::new(120.0, -5.0)).is_none(), "the southern hemisphere");
        let utm = Projector::new("EPSG:32633").unwrap();
        assert_eq!(utm.domain(), Domain::Band { lon0: 15.0, half: 30.0 });
        assert!(utm.forward_checked(Coord::new(15.0, 50.0)).is_some() && utm.forward_checked(Coord::new(-100.0, 40.0)).is_none());
        let usa = Projector::new("usaAlbers").unwrap();
        assert!(usa.forward_checked(Coord::new(-95.0, 38.0)).is_some() && usa.forward_checked(Coord::new(100.0, 30.0)).is_none());
        let moll = Flat::Custom(std::sync::Arc::new(Projector::new("mollweide").unwrap()));
        let e = moll.extent(&Extent::new(-180.0, -90.0, 180.0, 90.0));
        assert!(e.width() > 3.55e7 && e.height() > 1.75e7, "the whole world, edges included: {e:?}");
        // Mollweide's parallels are true at about ±40.7° and squeezed (0.9003) at the equator:
        // a map unit there covers 1/0.9003 m of ground.
        assert!((moll.ground_per_unit(Coord::new(0.0, 0.0)) - 1.0 / 0.9003).abs() < 2e-3);
        let at = moll.forward(Coord::new(10.0, 40.73)).unwrap();
        assert!((moll.ground_per_unit(at) - 1.0).abs() < 5e-3);
        // A preset by label gets the same area of use as by id.
        assert_eq!(Projector::new("USA Contiguous Albers Equal Area").unwrap().domain(), Projector::new("usaAlbers").unwrap().domain());
        assert!(Flat::Mercator.inverse(Coord::new(f64::NAN, 0.0)).is_none());
    }

    #[test]
    fn longitudes_are_counted_from_the_prime_meridian() {
        // EPSG:5329: Mercator with lon_0 = 3.19° east of Jakarta (106.81°E), so centred on 110°E,
        // where its false easting puts x = 3 900 000 m (give or take the datum shift).
        let merc = Projector::new("EPSG:5329").unwrap();
        let seam = merc.central_meridian().unwrap();
        // Its edge is nominally at 70°W, opposite 110°E; its datum shift (Bessel, towgs84
        // -403,684,41) moves it about 290 m from there in WGS84 longitudes.
        assert!((seam - 110.0).abs() < 0.01, "{seam}");
        let edge = merc.edge().unwrap();
        assert!(edge.stray_at(0.0) < 1e-4 && edge.stray_at(45.0) < 0.005, "{edge:?}");
        // Towards the poles the same shift spans more longitude.
        assert!(edge.stray_at(87.0) > edge.stray_at(45.0), "{edge:?}");
        // A projection on WGS84 has its edge exactly opposite its central meridian.
        let pdc = Projector::new("EPSG:3832").unwrap();
        let pdc = pdc.edge().unwrap();
        assert_eq!(pdc.lon, 330.0, "no datum shift: the nominal edge, exactly, without a search");
        assert!(pdc.strays.iter().all(|s| *s == 0.0), "{pdc:?}");
        let centre = merc.forward(Coord::new(110.0, 0.0)).unwrap();
        assert!((centre.x - 3_900_000.0).abs() < 2_000.0, "{centre:?}");
        // EPSG:31251: Gauss–Krüger with lon_0 = 28° east of Ferro (17.67°W), so its band is around
        // 10.33°E, not 28°E: Ireland is in it, the Caspian isn't.
        let gk = Projector::new("EPSG:31251").unwrap();
        let Domain::Band { lon0, half } = gk.domain() else { panic!("{:?}", gk.domain()) };
        assert!((lon0 - (28.0 - 17.666666666667)).abs() < 1e-6 && half == 30.0, "{lon0}");
        assert!(gk.forward_checked(Coord::new(13.0, 47.5)).is_some(), "Austria");
        assert!(gk.forward_checked(Coord::new(-8.0, 53.0)).is_some(), "Ireland");
        assert!(gk.forward_checked(Coord::new(50.0, 42.0)).is_none(), "the Caspian");
        // Names, degrees and nothing at all.
        assert!((prime_meridian("+proj=lcc +pm=paris") - 2.337229166667).abs() < 1e-9);
        assert!((prime_meridian("+proj=merc +pm=-17.5") + 17.5).abs() < 1e-9);
        assert_eq!(prime_meridian("+proj=merc +lon_0=10"), 0.0);
        assert_eq!(prime_meridian("+proj=merc +pm=nowhere"), 0.0);
        // lon_0 in DMS, read as proj4rs reads it, for the world projections computed here too.
        assert!(angle("+proj=eqearth +lon_0=90d30'W", "lon_0").is_some_and(|d| (d + 90.5).abs() < 1e-9));
        assert_eq!(angle("+proj=eqearth", "lon_0"), None);
        assert!(angle("+proj=stere +lat_0=-90 +lon_0=0", "lat_0").is_some_and(|d| (d + 90.0).abs() < 1e-9));
        assert!(angle("+proj=laea +lat_0=52d30'N", "lat_0").is_some_and(|d| (d - 52.5).abs() < 1e-9));
    }

    #[test]
    fn every_world_projection_wraps_opposite_its_central_meridian() {
        // For every EPSG code drawn as a whole-world map, the map jumps sides at `central_meridian() + 180°`:
        // just west of it draws on the east, just east of it on the west.
        let mut checked = 0;
        let epsg = (1024..=32767u32).map(|n| format!("EPSG:{n}"));
        for n in epsg.chain(rustgis_data::raster::crs::esri_codes().map(|n| format!("ESRI:{n}"))) {
            // Codes with a prime meridian other than Greenwich (+pm) and a datum shift included.
            let Ok(p) = Projector::new(&n) else { continue };
            let Some(lon0) = p.central_meridian() else { continue };
            let x = |lon: f64| p.forward(Coord::new(lon, 0.0)).map(|m| m.x);
            let (Some(west), Some(centre), Some(east)) = (x(lon0 + 179.5), x(lon0), x(lon0 + 180.5)) else { continue };
            assert!(west > centre && centre > east, "{n}: lon_0 {lon0}: {west} / {centre} / {east}");
            // With a datum shift the edge is found by search, which must not fall back to the
            // nominal edge (no margin) for any of them.
            if let Engine::Proj { .. } = p.engine {
                assert!(p.find_edge(p.lon0 + 180.0).is_some(), "{n}: the edge search fell back");
            }
            checked += 1;
        }
        // 31 in crs-definitions 0.6 (Mercator, equirectangular, cylindrical equal-area, Equal Earth),
        // 6 of them Mercator from Jakarta (+pm).
        assert!(checked >= 31, "only {checked} world projections checked");
    }

    #[test]
    fn unfound_edges_get_their_neighbours_stray() {
        let lats: Vec<f64> = (0..EDGE_SAMPLES).map(|i| (-90.0 + EDGE_STEP * i as f64).clamp(-89.9, 89.9)).collect();
        // A shift spanning 0.001° at the equator, as 1 / cos latitude.
        let mut measured: Vec<Option<f64>> = lats.iter().map(|l| Some(0.001 / l.to_radians().cos())).collect();
        let shown = vec![true; EDGE_SAMPLES];
        let at = |lat: f64| ((lat + 90.0) / EDGE_STEP) as usize;
        // Not found at 60°N, though shown: about the true 0.002°, with half again to spare, not the
        // pole's 0.57°.
        measured[at(60.0)] = None;
        let strays = fill_strays(&measured, &lats, &shown);
        assert!((strays[at(60.0)] - 1.5 * 0.002).abs() < 1e-4, "{}", strays[at(60.0)]);
        assert!(strays[at(60.0)] < strays[EDGE_SAMPLES - 1] / 10.0);
        // Not shown (Mercator's poles): no margin. Not found next to an unshown one: its other side.
        measured[EDGE_SAMPLES - 1] = None;
        measured[EDGE_SAMPLES - 2] = None;
        let mut shown = shown;
        shown[EDGE_SAMPLES - 1] = false;
        let strays = fill_strays(&measured, &lats, &shown);
        assert_eq!(strays[EDGE_SAMPLES - 1], 0.0);
        let expected = 1.5 * 0.001 / 85.0_f64.to_radians().cos(); // 80°N's, scaled to 85°N
        assert!((strays[EDGE_SAMPLES - 2] - expected).abs() / expected < 0.01, "{}", strays[EDGE_SAMPLES - 2]);
    }

    #[test]
    fn features_across_the_map_edge_are_cut_there() {
        use rustgis_data::Geometry;
        // #209: Equal Earth Americas (computed here, edge at 90°E) and PDC Mercator (proj4rs,
        // lon_0 = 150°, edge at 30°W). Before, these drew as bands across the whole map.
        // EPSG:5329 is centred on 110°E through its prime meridian (+pm=jakarta), with a datum shift.
        for (code, edge) in [("EPSG:8858", 90.0), ("EPSG:3832", -30.0), ("EPSG:5329", -70.0)] {
            let flat = Flat::of(&crate::Projection::Custom(code.into()));
            assert!(matches!(flat, Flat::Custom(_)), "{code}");
            let width = flat.extent(&Extent::new(-180.0, -80.0, 180.0, 80.0)).width();
            let line = Geometry::Line(vec![vec![Coord::new(edge - 10.0, 50.0), Coord::new(edge + 10.0, 52.0)]]);
            let square = Geometry::Polygon(vec![vec![vec![
                Coord::new(edge - 10.0, 40.0),
                Coord::new(edge + 10.0, 40.0),
                Coord::new(edge + 10.0, 60.0),
                Coord::new(edge - 10.0, 60.0),
                Coord::new(edge - 10.0, 40.0),
            ]]]);
            for g in [line, square] {
                let parts: Vec<Vec<Coord>> = match flat.project(&g) {
                    Geometry::Line(parts) => parts,
                    Geometry::Polygon(polys) => polys.into_iter().flatten().collect(),
                    other => panic!("{other:?}"),
                };
                assert_eq!(parts.len(), 2, "{code}: one piece on each side: {parts:?}");
                for part in &parts {
                    assert!(part.iter().all(|c| c.is_finite()), "{code}: {part:?}");
                    let jump = part.windows(2).map(|w| (w[1].x - w[0].x).abs()).fold(0.0, f64::max);
                    assert!(jump < width / 4.0, "{code}: no edge across the map ({jump} of {width})");
                }
                let side = |p: &Vec<Coord>| p.iter().map(|c| c.x).sum::<f64>().signum();
                assert_ne!(side(&parts[0]), side(&parts[1]), "{code}: the pieces sit on opposite edges");
            }
            // A hole across the edge is cut with its polygon: each side keeps its part of it.
            let ring = |x0: f64, x1: f64, y0: f64, y1: f64| {
                vec![Coord::new(x0, y0), Coord::new(x1, y0), Coord::new(x1, y1), Coord::new(x0, y1), Coord::new(x0, y0)]
            };
            let holed = Geometry::Polygon(vec![vec![ring(edge - 10.0, edge + 10.0, 40.0, 60.0), ring(edge - 5.0, edge + 5.0, 45.0, 55.0)]]);
            let Geometry::Polygon(polys) = flat.project(&holed) else { panic!("{code}: a polygon") };
            assert_eq!(polys.len(), 2, "{code}: one piece on each side: {polys:?}");
            for p in &polys {
                assert_eq!(p.len(), 2, "{code}: each piece keeps its part of the hole: {p:?}");
                assert!(p.iter().flatten().all(|c| c.is_finite()), "{code}: {p:?}");
            }
            let side = |p: &Vec<Vec<Coord>>| p[1].iter().map(|c| c.x).sum::<f64>().signum();
            assert_ne!(side(&polys[0]), side(&polys[1]), "{code}: the hole's parts sit on opposite edges");
        }
        // Limited projections never reach the far meridian, so nothing is cut.
        assert_eq!(Projector::new("EPSG:32633").unwrap().central_meridian(), None);
        assert_eq!(Projector::new("equalEarth").unwrap().central_meridian(), Some(0.0));
        assert!(
            Projector::new("EPSG:3832").unwrap().central_meridian().is_some_and(|s| (s - 150.0).abs() < 1e-9),
            "PDC Mercator: lon_0 = 150"
        );
    }

    #[test]
    fn vn2000_vietnam_projections() {
        // The presets are EPSG's VN-2000 systems word for word.
        for (id, code) in [("vn2000tm3", 5897), ("vn2000Utm48", 3405), ("vn2000Utm49", 3406)] {
            let def = PRESETS.iter().find(|(p, _, _)| *p == id).map(|(_, _, d)| *d);
            assert_eq!(def, rustgis_data::raster::crs::epsg_proj4(code), "{id}");
        }

        // `vietnam` names no projection: the country spans two UTM zones and many TM-3 meridians.
        assert!(lookup("vietnam").is_err());
        // Bare VN-2000 is ambiguous and rejected
        for bare in ["vn2000", "VN-2000", "VN2000", "vn-2000", "VN 2000", "vn_2000"] {
            let err = lookup(bare).unwrap_err();
            assert!(err.contains("ambiguous"), "{bare}: {err}");
            assert!(parse_preset(bare).is_err(), "{bare}");
        }

        // The presets and `Crs::parse` (rasters, command parameters) take the same names.
        for (name, id, code) in [
            ("vn2000_utm48", "vn2000Utm48", 3405),
            ("vn-2000-utm48", "vn2000Utm48", 3405),
            ("VN200048", "vn2000Utm48", 3405),
            ("VN-2000 / UTM zone 49", "vn2000Utm49", 3406),
            ("VN2000TM3105", "vn2000tm3", 5897),
            ("vn2000 tm-3", "vn2000tm3", 5897),
        ] {
            assert_eq!(parse_preset(name).unwrap().map(|(p, _)| p), Some(id), "{name}");
            assert_eq!(rustgis_data::raster::crs::Crs::parse(name).map(|c| c.epsg()), Ok(code), "{name}");
        }

        // Explicit TM-3 alias lookups
        for name in ["vn2000tm3", "vn2000-tm3", "vn2000_tm3", "vn-2000-tm3", "VN-2000 / TM-3", "vn-2000 / tm-3", "VN2000 / TM-3"] {
            let (def, label) = lookup(name).unwrap();
            assert!(def.contains("+proj=tmerc") && def.contains("+towgs84="), "{name}");
            assert!(def.contains("+lon_0=105") && def.contains("+k=0.9999"), "{name}");
            assert_eq!(label, "VN-2000 / TM-3 (Vietnam)", "{name}");
        }
        for name in [
            "vn2000Utm48",
            "vn-2000-48",
            "vn2000-48",
            "vn2000utm48",
            "VN-2000 / UTM zone 48N",
            "vn-2000 / utm zone 48n",
            "VN-2000 / UTM 48N",
        ] {
            let (def, label) = lookup(name).unwrap();
            assert!(def.contains("+proj=utm") && def.contains("+zone=48"), "{name}");
            assert_eq!(label, "VN-2000 / UTM zone 48N", "{name}");
        }
        for name in [
            "vn2000Utm49",
            "vn-2000-49",
            "vn2000-49",
            "vn2000utm49",
            "VN-2000 / UTM zone 49N",
            "vn-2000 / utm zone 49n",
            "VN-2000 / UTM 49N",
        ] {
            let (def, label) = lookup(name).unwrap();
            assert!(def.contains("+proj=utm") && def.contains("+zone=49"), "{name}");
            assert_eq!(label, "VN-2000 / UTM zone 49N", "{name}");
        }

        // Bare province names and aliases must NOT hijack lookup: they must return unknown projection error
        for bare in [
            "hanoi",
            "Hà Nội",
            "tphcm",
            "TP. Hồ Chí Minh",
            "saigon",
            "Sài Gòn",
            "danang",
            "Đà Nẵng",
            "Gia Lai",
            "Đắk Lắk",
            "Huế",
            "Nghệ An",
            "vinh",
            "hue",
            "sapa",
            "hcm",
            "daknong",
            "Đắk Nông",
            "haugiang",
            "Hậu Giang",
            "atlantis",
            "unknown_province_xyz",
        ] {
            assert!(lookup(bare).is_err(), "{bare} should not be recognized as bare projection");
        }

        // Explicitly prefixed aliases resolve correctly
        assert_eq!(lookup("vn2000:hanoi").unwrap().0, lookup("vn2000tm3 +lon_0=105").unwrap().0);
        assert_eq!(lookup("VN2000:hanoi").unwrap().0, lookup("vn2000tm3 +lon_0=105").unwrap().0);
        assert_eq!(lookup("VN-2000:Hà Nội").unwrap().0, lookup("vn2000tm3 +lon_0=105").unwrap().0);
        assert_eq!(lookup("VN2000/Hà Nội").unwrap().0, lookup("vn2000tm3 +lon_0=105").unwrap().0);
        assert_eq!(lookup("vn2000:tphcm").unwrap().0, lookup("vn2000tm3 +lon_0=105.75").unwrap().0);
        assert_eq!(lookup("VN2000:TP. Hồ Chí Minh").unwrap().0, lookup("vn2000tm3 +lon_0=105.75").unwrap().0);
        assert_eq!(lookup("vn2000:saigon").unwrap().0, lookup("vn2000tm3 +lon_0=105.75").unwrap().0);
        assert_eq!(lookup("vn2000:danang").unwrap().0, lookup("vn2000tm3 +lon_0=107.75").unwrap().0);
        assert_eq!(lookup("VN2000/Đà Nẵng").unwrap().0, lookup("vn2000tm3 +lon_0=107.75").unwrap().0);
        assert_eq!(lookup("vn2000tm3:danang").unwrap().0, lookup("vn2000tm3 +lon_0=107.75").unwrap().0);
        assert_eq!(lookup("vn2000:vinh").unwrap().0, lookup("vn2000tm3 +lon_0=104.75").unwrap().0);
        assert_eq!(lookup("vn2000:hue").unwrap().0, lookup("vn2000tm3 +lon_0=107").unwrap().0);
        assert_eq!(lookup("vn2000:sapa").unwrap().0, lookup("vn2000tm3 +lon_0=104.75").unwrap().0);
        assert_eq!(lookup("vn2000:hcm").unwrap().0, lookup("vn2000tm3 +lon_0=105.75").unwrap().0);
        assert_eq!(lookup("vn2000:daknong").unwrap().0, lookup("vn2000tm3 +lon_0=108.5").unwrap().0);
        assert_eq!(lookup("vn2000:haugiang").unwrap().0, lookup("vn2000tm3 +lon_0=105").unwrap().0);

        // Numeric EPSG codes and unrelated projections must never be intercepted by province alias resolution
        assert_eq!(lookup("3405").unwrap().1, "EPSG:3405");
        assert_eq!(lookup("3406").unwrap().1, "EPSG:3406");
        assert_eq!(lookup("3035").unwrap().1, "EPSG:3035");
        assert_eq!(lookup("EPSG:3035").unwrap().1, "EPSG:3035");
        assert_eq!(lookup("ESRI:102033").unwrap().1, "ESRI:102033");
        assert_eq!(lookup("robinson").unwrap().1, "Robinson");

        // Unknown aliases and malformed parameters must return errors
        assert!(lookup("unknown_province_xyz").is_err());
        assert!(lookup("atlantis").is_err());
        assert!(lookup("vn2000tm3 +lon_0=abc").is_err());
        assert!(lookup("vn2000tm3 +lon_0=999").is_err());
        assert!(lookup("vn2000tm3 +invalid=10").is_err());

        // Parameter replacement preserves all other PROJ parameters uncorrupted
        let (danang_def, _) = lookup("vn2000:danang").unwrap();
        assert!(danang_def.contains("+proj=tmerc"));
        assert!(danang_def.contains("+lat_0=0"));
        assert!(danang_def.contains("+lon_0=107.75"));
        assert!(danang_def.contains("+k=0.9999"));
        assert!(danang_def.contains("+x_0=500000"));
        assert!(danang_def.contains("+y_0=0"));
        assert!(danang_def.contains("+ellps=WGS84"));
        assert!(danang_def.contains("+towgs84="));
        assert!(danang_def.contains("+units=m"));
        assert!(danang_def.contains("+no_defs"));

        // All 63 provinces in VN2000_PROVINCES resolve via resolve_vn2000_province and prefixed lookup
        assert_eq!(VN2000_PROVINCES.len(), 63);
        for prov in VN2000_PROVINCES {
            let lon = resolve_vn2000_province(prov.name).unwrap();
            assert_eq!(lon, prov.lon_0, "{}", prov.name);
            let prefixed = format!("vn2000:{}", prov.id);
            let (def, _) = lookup(&prefixed).unwrap();
            if (prov.lon_0 - 105.0).abs() < 1e-6 {
                assert!(def.contains("+lon_0=105"), "{}: {def}", prov.name);
            } else {
                assert!(def.contains(&format!("+lon_0={}", prov.lon_0)), "{}: {def}", prov.name);
            }
            assert!(Projector::new(&prefixed).is_ok(), "{}", prov.name);
        }

        // Every id is its name normalised.
        for prov in VN2000_PROVINCES {
            assert_eq!(prov.id, normalize_vietnamese(prov.name), "{}", prov.name);
        }
        // A province is saved as the preset on its meridian, so projects don't depend on the names.
        assert_eq!(parse_preset("vn2000:tphcm"), Ok(Some(("vn2000tm3", vec![("lon_0", 105.75)]))));
        assert_eq!(parse_preset("VN2000/Hà Nội"), Ok(Some(("vn2000tm3", vec![]))));
        assert_eq!(parse_preset("VN-2000 / Đà Nẵng"), Ok(Some(("vn2000tm3", vec![("lon_0", 107.75)]))));
        assert_eq!(parse_preset("hanoi"), Ok(None));
        assert!(parse_preset("vn2000:hanoi +lon_0=106").is_err(), "a province takes no parameters");
        assert_eq!(lookup("vn2000:tphcm").unwrap().1, "VN-2000 / TM-3 (Vietnam) (lon_0=105.75)");
        assert_eq!(resolve_vn2000_province("vithanh"), Some(105.0));
        assert_eq!(resolve_vn2000_province("vitri"), Some(105.0));
        assert_eq!(parse_preset("vn2000:vithanh"), Ok(Some(("vn2000tm3", vec![]))));
        assert_eq!(parse_preset("vn2000:vitri"), Ok(Some(("vn2000tm3", vec![]))));

        // All UI shortcut hubs are canonical entries with matching central meridians
        for &(ui_name, ui_lon) in VN2000_UI_PROVINCES {
            let prov = VN2000_PROVINCES.iter().find(|p| p.name == ui_name).unwrap();
            assert_eq!(prov.lon_0, ui_lon, "{ui_name}");
        }

        // Decomposed NFD Vietnamese unicode normalization
        let nfd_hanoi = "Ha\u{300} N\u{f4}\u{323}i";
        assert_eq!(lookup(&format!("vn2000:{nfd_hanoi}")).unwrap().0, lookup("vn2000tm3 +lon_0=105").unwrap().0);

        // Forward and inverse coordinate projection accuracy
        // Hanoi (Hoan Kiem lake: 105.8525°E, 21.0285°N) in VN-2000 TM-3 (lon_0 = 105.0)
        let p_hn = Projector::new("vn2000tm3 +lon_0=105").unwrap();
        let c_hn = Coord::new(105.8525, 21.0285);
        let m_hn = p_hn.forward(c_hn).unwrap();
        // False easting is 500,000m, point is east of 105°E so x > 500,000; latitude ~21° so y ~ 2,326,000m
        assert!(m_hn.x > 500_000.0 && m_hn.x < 700_000.0);
        assert!(m_hn.y > 2_000_000.0 && m_hn.y < 2_500_000.0);
        let back_hn = p_hn.inverse(m_hn).unwrap();
        assert!((back_hn.x - c_hn.x).abs() < 1e-6 && (back_hn.y - c_hn.y).abs() < 1e-6);

        // Ho Chi Minh City (Ben Thanh: 106.6983°E, 10.7725°N) in VN-2000 TM-3 (lon_0 = 105.75)
        let p_hcm = Projector::new("vn2000tm3 +lon_0=105.75").unwrap();
        let c_hcm = Coord::new(106.6983, 10.7725);
        let m_hcm = p_hcm.forward(c_hcm).unwrap();
        assert!(m_hcm.x > 500_000.0 && m_hcm.x < 700_000.0);
        assert!(m_hcm.y > 1_100_000.0 && m_hcm.y < 1_300_000.0);
        let back_hcm = p_hcm.inverse(m_hcm).unwrap();
        assert!((back_hcm.x - c_hcm.x).abs() < 1e-6 && (back_hcm.y - c_hcm.y).abs() < 1e-6);

        // UTM 48N and 49N roundtrip
        for utm in ["vn2000Utm48", "vn2000Utm49"] {
            let p = Projector::new(utm).unwrap();
            let c = Coord::new(106.0, 15.0);
            let m = p.forward(c).unwrap();
            let back = p.inverse(m).unwrap();
            assert!((back.x - c.x).abs() < 1e-6 && (back.y - c.y).abs() < 1e-6);
        }
    }
}
