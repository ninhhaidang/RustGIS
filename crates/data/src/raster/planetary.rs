//! Coordinate systems of other bodies than the Earth (Mars, the Moon, other planets and their
//! moons), as the IAU defines them: the `IAU_2015` authority of PROJ and GDAL.
//!
//! RustGIS has no model of other bodies' coordinates yet (opengeos/RustGIS#1448). Every layer is
//! read as the Earth's longitude/latitude, so a planetary system is only reported:
//! - Its longitude/latitude, or a projection RustGIS unprojects, are read as they are, with a
//!   notice that the map, its scale bar and measurements take them for the Earth's. This works
//!   because such a system has no shift to WGS 84, so the conversion leaves its numbers unchanged.
//! - Data that would land in the wrong place as longitude/latitude is refused: metres that would
//!   be read as degrees, or longitudes counted west that would be read as east.
//!
//! A system is another body's when:
//! - its ellipsoid's semi-major axis is outside [`EARTH_SEMI_MAJOR`] (6 350 to 6 400 km). Every
//!   Earth ellipsoid and sphere in the EPSG and ESRI definitions RustGIS bundles lies inside it,
//!   and a test checks each one. Venus, the body nearest the Earth in size, has a radius of
//!   6 051.8 km (IAU 2015). This is how a definition is told apart: WKT, a GeoTIFF's keys, a PROJ
//!   string or PROJJSON.
//! - or, for a system given only by a code, its authority is the IAU's (`IAU_2015`, `IAU`) and the
//!   code's body isn't the Earth. An IAU 2015 code is the body's NAIF id times 100, plus the kind
//!   of system ([`IauKind`]). So `49900` is Mars (499) as a sphere in longitude/latitude, and
//!   `39900` is the Earth (399).

use super::crs::Crs;

/// The semi-major axes, in metres, of the Earth's ellipsoids and spheres, with room to spare.
pub const EARTH_SEMI_MAJOR: std::ops::RangeInclusive<f64> = 6_350_000.0..=6_400_000.0;

/// The Earth's NAIF id, the body of the IAU codes `399xx`.
const EARTH_NAIF: i64 = 399;

/// Whether a semi-major axis (or radius) in metres is another body's than the Earth's.
pub fn off_earth(semi_major: f64) -> bool {
    semi_major.is_finite() && semi_major > 0.0 && !EARTH_SEMI_MAJOR.contains(&semi_major)
}

/// The semi-major and semi-minor axes, in metres, that a PROJ definition gives by `+a` or `+R`,
/// and `+b` or `+rf`. Without `+b` or `+rf` it is a sphere. `None` for an ellipsoid named by
/// `+ellps` or `+datum`, since PROJ's named ellipsoids are all the Earth's.
pub fn proj4_axes(proj4: &str) -> Option<(f64, f64)> {
    let value = |key: &str| {
        proj4.split_whitespace().take(64).find_map(|t| t.strip_prefix(key)).and_then(|v| v.parse::<f64>().ok()).filter(|v| v.is_finite())
    };
    let a = value("+a=").or_else(|| value("+R=")).filter(|a| *a > 0.0)?;
    let b = match (value("+b="), value("+rf=")) {
        (Some(b), _) if b > 0.0 => b,
        (_, Some(rf)) if rf > 0.0 => a - a / rf,
        _ => a,
    };
    Some((a, b))
}

/// What the last two digits of an IAU 2015 code say the system is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IauKind {
    /// Planetocentric longitude/latitude, on the body's sphere (`00`) or ellipsoid (`02`). East is
    /// positive.
    Planetocentric,
    /// Planetographic longitude/latitude (`01`). West is positive on most bodies.
    Planetographic,
    /// A projection (`10` and up: equirectangular, sinusoidal, polar stereographic …), in metres.
    Projected,
}

/// Whether an authority is the IAU's: `IAU`, or `IAU_` and the year of its report (`IAU_2015`), in
/// any case.
pub fn is_iau(authority: &str) -> bool {
    let authority = authority.trim().to_ascii_uppercase();
    authority == "IAU" || authority.strip_prefix("IAU_").is_some_and(|y| y.len() == 4 && y.bytes().all(|b| b.is_ascii_digit()))
}

/// The body (its NAIF id) and kind of system of an IAU code, `None` when the authority isn't the
/// IAU's or the body is the Earth.
pub fn iau_code(authority: &str, code: i64) -> Option<(i64, IauKind)> {
    if !is_iau(authority) || code <= 0 {
        return None;
    }
    let body = code / 100;
    let kind = match code % 100 {
        0 | 2 => IauKind::Planetocentric,
        1 => IauKind::Planetographic,
        _ => IauKind::Projected,
    };
    (body != EARTH_NAIF).then_some((body, kind))
}

/// The last two digits of a body's first projection in PROJ's `IAU_2015` table.
const FIRST_PROJECTION: i64 = 10;

/// Whether an IAU code is planetographic longitude/latitude, or a projection of it.
/// In PROJ's `IAU_2015` table, a body's projections come in groups of three, starting at `…10`,
/// `…15`, `…20` and so on. The first of a group is on the sphere, the second on the planetographic
/// system and the third on the ellipsoid. So `49911` and `49916` project Mars's planetographic
/// longitude/latitude, and PROJ gives them westings.
pub fn iau_ographic(authority: &str, code: i64) -> bool {
    match iau_code(authority, code) {
        Some((_, IauKind::Planetographic)) => true,
        Some((_, IauKind::Projected)) => code % 100 >= FIRST_PROJECTION && code % 5 == 1,
        Some((_, IauKind::Planetocentric)) | None => false,
    }
}

/// Whether a coordinate system's name says it is planetographic, by a word of its own
/// (`Mars (2015) / Ographic`, `GCS_Mars_2015_Ographic`, `planetographic`), not one that merely
/// ends so (`cartographic`).
pub fn ographic_name(name: &str) -> bool {
    name.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w.eq_ignore_ascii_case("ographic") || w.eq_ignore_ascii_case("planetographic"))
}

/// The notice for data read in another body's coordinate `system`, named as the reader shows it.
/// Its longitude/latitude are kept as they are, so the map takes them for the Earth's.
pub fn read_note(system: &str) -> String {
    format!("{system} belongs to another body than the Earth; it is drawn and measured as if on the Earth")
}

/// [`read_note`] for a system known only by its code. The layer keeps no system, because RustGIS
/// has no definition of the body to give it. An export of the layer can't tell, so this notice
/// says now that exports take the data for the Earth's too.
pub fn code_note(system: &str) -> String {
    unkept_note(system, "its code")
}

/// [`code_note`] for a system known by its ellipsoid, in a definition the reader doesn't keep
/// (GeoParquet's PROJJSON without an EPSG code).
pub fn ellipsoid_note(system: &str) -> String {
    unkept_note(system, "its ellipsoid")
}

/// The notice for a layer that keeps no system, `told_by` naming what said the body.
fn unkept_note(system: &str, told_by: &str) -> String {
    format!(
        "{system} belongs to another body than the Earth, as only {told_by} says; it is drawn, measured and exported as if on the Earth"
    )
}

/// The notice for a layer read in `crs` when it is another body's ([`read_note`]). It is for the
/// readers that don't name the datum themselves (rasters).
pub fn note_of(crs: Crs) -> Option<String> {
    crs.is_planetary().then(|| read_note(&crs.label()))
}

/// The error for data in another body's `system` that would land in the wrong place if read as
/// longitude/latitude. `why` says what is wrong with it.
pub fn refused(system: &str, why: &str) -> String {
    format!("{system} belongs to another body than the Earth, and {why}; read as longitude/latitude, it would land in the wrong place")
}

/// The error for a WKT `text` that couldn't be read (`why`) when its datum is another body's.
/// Read as longitude/latitude instead, as an unreadable Earth system is, its coordinates would
/// land in the wrong place. `None` for the Earth's.
pub fn refused_wkt(text: &str, why: &str) -> Option<String> {
    super::wkt::planetary_datum(text).map(|datum| refused(&format!("the datum {datum}"), why))
}

/// Why another body's projected system is refused when the reader can't unproject it. That is
/// when only its code is given, or a definition the reader doesn't convert.
pub const PROJECTED: &str = "its coordinates are projected, in metres";

/// Why a geodetic system in geocentric X/Y/Z (Cartesian) coordinates is refused.
pub const GEOCENTRIC: &str = "its coordinates are geocentric X/Y/Z, in metres";

/// The name of a PROJJSON axis `unit` that isn't the degree (`"radian"`, or
/// `{"name": "grad", "conversion_factor": 0.0157…}`), `None` for the degree. A unit given by its
/// factor is the degree when that is π/180 radians, whatever its name.
fn not_degrees(unit: &serde_json::Value) -> Option<String> {
    let name = |n: &str| n.trim().chars().take(40).collect::<String>();
    let degree = |n: &str| matches!(n.trim().to_ascii_lowercase().as_str(), "degree" | "degrees" | "deg");
    if let Some(n) = unit.as_str() {
        return (!degree(n)).then(|| name(n));
    }
    let n = unit.get("name").and_then(serde_json::Value::as_str).unwrap_or("");
    match unit.get("conversion_factor").and_then(serde_json::Value::as_f64) {
        // Named by its factor when the name says nothing or says the degree.
        Some(f) => ((f - 1f64.to_radians()).abs() > 1e-12)
            .then(|| if n.trim().is_empty() || degree(n) { format!("units of {f} radians") } else { name(n) }),
        None => (!degree(n)).then(|| if n.trim().is_empty() { "an unnamed unit".to_string() } else { name(n) }),
    }
}

/// Why a planetographic system is refused, unless its axes say its longitudes count east.
pub const OGRAPHIC: &str = "its planetographic longitudes may count west";

/// Why a projection of planetographic longitude/latitude is refused when its axes don't say x
/// counts east. PROJ gives such a projection westings.
pub const OGRAPHIC_PROJECTED: &str = "it is a planetographic projection, so its x coordinates may count west";

/// What a reader that converts only EPSG codes makes of a coordinate system declared some other
/// way, when it is another body's.
#[derive(Debug)]
pub enum Declared {
    /// Longitude/latitude, read as they are with this notice. `crs` is the system itself when a
    /// definition gave it, for the layer to keep.
    LonLat { note: String, crs: Option<Crs> },
    /// Coordinates that would be in the wrong place as longitude/latitude: the error.
    Refused(String),
}

/// A coordinate system declared by `authority` and `code` (a GeoPackage's organization, a
/// FlatGeobuf header's, a GeoJSON `crs` name's) or by a `wkt` definition, for a reader that
/// converts only EPSG codes. `None` for the Earth's, or when neither says which body it is on.
/// The definition goes first, because it is what says the body and the axes.
pub fn declared(authority: Option<&str>, code: Option<i64>, wkt: Option<&str>) -> Option<Declared> {
    let wkt = wkt.map(str::trim).filter(|w| w.contains('['));
    if let Some(w) = wkt {
        match super::wkt::read_wkt(w) {
            Ok((crs, note)) if crs.is_planetary() => {
                let system = crs.label();
                return Some(if crs.is_geographic() {
                    Declared::LonLat { note: note.unwrap_or_else(|| read_note(&system)), crs: Some(crs) }
                } else {
                    Declared::Refused(refused(&system, PROJECTED))
                });
            }
            // The Earth's, whatever the code says.
            Ok(_) => return None,
            Err(e) => {
                if let Some(refused) = refused_wkt(w, &e) {
                    return Some(Declared::Refused(refused));
                }
            }
        }
    }
    let authority = authority?;
    let code = code?;
    let (_, kind) = iau_code(authority, code)?;
    let system = format!("{}:{code}", authority.trim());
    Some(match kind {
        IauKind::Planetocentric => Declared::LonLat { note: code_note(&system), crs: None },
        IauKind::Planetographic => Declared::Refused(refused(&system, OGRAPHIC)),
        IauKind::Projected => Declared::Refused(refused(&system, PROJECTED)),
    })
}

/// A GeoParquet column's `crs` (PROJJSON) when it is another body's: by its `id` (an IAU code),
/// else by its ellipsoid's semi-major axis or radius, in metres. GeoParquet's readers convert only
/// EPSG codes, so only planetocentric longitude/latitude in degrees is read, as it is. These are
/// refused, whatever the code says:
/// - a projected system;
/// - geocentric X/Y/Z (a geodetic system's Cartesian coordinates);
/// - longitudes counted west, or latitudes counted south;
/// - longitude/latitude in another unit than degrees (radians, grads).
pub fn projjson(crs: &serde_json::Value) -> Option<Declared> {
    let id = crs.get("id");
    let authority = id.and_then(|i| i.get("authority")).and_then(serde_json::Value::as_str);
    let code = id.and_then(|i| i.get("code")).and_then(|c| c.as_i64().or_else(|| c.as_str().and_then(|s| s.trim().parse().ok())));
    let name = crs.get("name").and_then(serde_json::Value::as_str).unwrap_or("").chars().take(80).collect::<String>();
    let system = match (authority, code) {
        (Some(a), Some(c)) if !name.trim().is_empty() => format!("{} ({}:{c})", name.trim(), a.trim()),
        (Some(a), Some(c)) => format!("{}:{c}", a.trim()),
        _ if name.trim().is_empty() => "its coordinate system".to_string(),
        _ => name.trim().to_string(),
    };
    let kind = crs.get("type").and_then(serde_json::Value::as_str);
    let geographic = matches!(kind, Some("GeographicCRS" | "GeodeticCRS"));
    let cs = crs.get("coordinate_system");
    let axes = cs.and_then(|cs| cs.get("axis")).and_then(serde_json::Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let direction = |a: &serde_json::Value| a.get("direction").and_then(serde_json::Value::as_str).map(str::to_ascii_lowercase);
    // What would put its coordinates in the wrong place read as longitude/latitude in degrees:
    // geocentric X/Y/Z in metres, longitudes counted west or latitudes counted south (read
    // mirrored), or longitude/latitude in another unit (an ellipsoidal height's metres are no
    // matter).
    let cartesian =
        cs.and_then(|cs| cs.get("subtype")).and_then(serde_json::Value::as_str).is_some_and(|s| s.eq_ignore_ascii_case("cartesian"));
    let mirrored = axes.iter().find_map(|a| match direction(a).as_deref() {
        Some("west") => Some("its longitudes count west".to_string()),
        Some("south") => Some("its latitudes count south".to_string()),
        _ => None,
    });
    let unit = axes
        .iter()
        .filter(|a| matches!(direction(a).as_deref(), Some("north" | "south" | "east" | "west")))
        .find_map(|a| not_degrees(a.get("unit")?))
        .map(|u| format!("its longitude/latitude are in {u}, not degrees"));
    let misread = if cartesian { Some(GEOCENTRIC.to_string()) } else { mirrored.or(unit) };
    if let (Some(a), Some(c)) = (authority, code)
        && let Some((_, iau)) = iau_code(a, c)
    {
        // The definition goes before the code: a projected or mirrored system under a
        // planetocentric code is what it says.
        return Some(match iau {
            IauKind::Planetocentric if matches!(kind, Some("ProjectedCRS" | "DerivedProjectedCRS")) => {
                Declared::Refused(refused(&system, PROJECTED))
            }
            IauKind::Planetocentric => match misread {
                Some(why) => Declared::Refused(refused(&system, &why)),
                None => Declared::LonLat { note: code_note(&system), crs: None },
            },
            IauKind::Planetographic => Declared::Refused(refused(&system, OGRAPHIC)),
            IauKind::Projected => Declared::Refused(refused(&system, PROJECTED)),
        });
    }
    // A projected system's ellipsoid is its base system's.
    let base = if geographic { Some(crs) } else { crs.get("base_crs") };
    let datum = base.and_then(|b| b.get("datum").or_else(|| b.get("datum_ensemble")));
    let ellipsoid = datum.and_then(|d| d.get("ellipsoid"));
    let axis = ellipsoid.and_then(|e| e.get("semi_major_axis").or_else(|| e.get("radius")));
    // In metres, or `{"value": …, "unit": {"conversion_factor": …}}`.
    let metres = axis.and_then(|a| {
        a.as_f64().or_else(|| {
            let v = a.get("value")?.as_f64()?;
            let factor = a.get("unit").and_then(|u| u.get("conversion_factor")).and_then(serde_json::Value::as_f64).unwrap_or(1.0);
            Some(v * factor)
        })
    })?;
    if !off_earth(metres) {
        return None;
    }
    Some(match misread {
        _ if !geographic => Declared::Refused(refused(&system, PROJECTED)),
        Some(why) => Declared::Refused(refused(&system, &why)),
        None => Declared::LonLat { note: ellipsoid_note(&system), crs: None },
    })
}

/// Why exporting a layer in another body's system `from` to `to` would write other data than the
/// layer's. Projected onto another ellipsoid (the Earth's, or another body's), the coordinates
/// would be metres on that ellipsoid, not the layer's. `None` when the export writes the same
/// numbers, perhaps under another system's name, which [`crate::export_notes`] says.
pub fn export_error(from: Crs, to: Crs) -> Option<String> {
    if !from.is_planetary() || to.is_geographic() || from == to {
        return None;
    }
    let axes = |c: Crs| c.custom_def().and_then(|d| proj4_axes(&d.proj4));
    let same = match (axes(from), axes(to)) {
        (Some((a1, b1)), Some((a2, b2))) => (a1 - a2).abs() < 0.01 && (b1 - b2).abs() < 0.01,
        _ => false,
    };
    (!same).then(|| {
        format!(
            "{} belongs to another body than the Earth, and {} is on another ellipsoid, so the coordinates would be wrong; export it in its own system or as longitude/latitude",
            from.label(),
            to.label()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_earth_band_holds_every_bundled_earth_system() {
        // Every EPSG and ESRI definition RustGIS bundles is the Earth's: none may be taken for
        // another body's.
        let (mut low, mut high) = (f64::INFINITY, 0.0f64);
        let defs = (0..=u16::MAX)
            .filter_map(super::super::crs::epsg_proj4)
            .chain(super::super::crs::esri_codes().filter_map(super::super::crs::esri_proj4));
        let mut n = 0;
        for def in defs {
            let Ok(proj) = proj4rs::Proj::from_proj_string(def) else { continue };
            let (a, _) = proj.ellipse_parameters();
            // Geocentric and vertical definitions have an ellipsoid too.
            assert!(!off_earth(a), "{def}: {a}");
            if let Some((a, _)) = proj4_axes(def) {
                assert!(!off_earth(a), "{def}: {a}");
            }
            low = low.min(a);
            high = high.max(a);
            n += 1;
        }
        assert!(n > 5000, "{n}");
        // The bundled systems' axes, for the band's margin (see the module's notes).
        assert!(low >= 6_370_000.0 && high < 6_379_000.0, "{low}..{high}");
        // Bodies near the band: Venus (IAU 2015) and the Earth's own IAU sphere.
        assert!(off_earth(6_051_800.0) && off_earth(3_396_190.0) && off_earth(1_737_400.0) && off_earth(71_492_000.0));
        assert!(!off_earth(6_378_136.6) && !off_earth(6_371_000.0));
        assert!(!off_earth(f64::NAN) && !off_earth(0.0) && !off_earth(-3_396_190.0) && !off_earth(f64::INFINITY));
    }

    #[test]
    fn proj4_axes_by_a_r_b_and_rf() {
        assert_eq!(proj4_axes("+proj=longlat +a=3396190 +b=3376200 +no_defs"), Some((3_396_190.0, 3_376_200.0)));
        assert_eq!(proj4_axes("+proj=eqc +R=1737400 +no_defs"), Some((1_737_400.0, 1_737_400.0)));
        let (a, b) = proj4_axes("+proj=longlat +a=3396190 +rf=169.8944472236118").unwrap();
        assert!(a == 3_396_190.0 && (b - 3_376_200.0).abs() < 0.01, "{b}");
        assert_eq!(proj4_axes("+proj=longlat +ellps=WGS84 +no_defs"), None);
        assert_eq!(proj4_axes("+proj=longlat +a=x +no_defs"), None);
        assert_eq!(proj4_axes("+proj=longlat +a=-5 +no_defs"), None);
        assert_eq!(proj4_axes(""), None);
    }

    #[test]
    fn iau_codes_by_body_and_kind() {
        assert_eq!(iau_code("IAU_2015", 49900), Some((499, IauKind::Planetocentric)));
        assert_eq!(iau_code("iau_2015", 49902), Some((499, IauKind::Planetocentric)));
        assert_eq!(iau_code("IAU", 49901), Some((499, IauKind::Planetographic)));
        assert_eq!(iau_code("IAU_2015", 49910), Some((499, IauKind::Projected)));
        assert_eq!(iau_code("IAU_2015", 30135), Some((301, IauKind::Projected)));
        // The Earth's own IAU codes, other authorities and junk aren't another body's.
        assert_eq!(iau_code("IAU_2015", 39900), None);
        assert_eq!(iau_code("EPSG", 49900), None);
        assert_eq!(iau_code("IAU_2015", 0), None);
        assert_eq!(iau_code("IAU_2015", -49900), None);
        assert_eq!(iau_code("", 49900), None);
        // Only the IAU's own names: not another authority that starts the same.
        assert_eq!(iau_code(" IAU_2015 ", 49900), Some((499, IauKind::Planetocentric)));
        for other in ["IAUS", "IAUX", "IAU_", "IAU_15", "IAU_2O15", "IAU_20150"] {
            assert_eq!(iau_code(other, 49910), None, "{other}");
        }
    }

    #[test]
    fn planetographic_codes_and_names() {
        // Planetographic longitude/latitude and the projections of it, by PROJ's numbering.
        for code in [49901, 49911, 49916, 49921, 49931, 49991, 30111] {
            assert!(iau_ographic("IAU_2015", code), "{code}");
        }
        // Planetocentric ones, their projections, the Earth's and other authorities' aren't.
        for code in [49900, 49902, 49910, 49912, 49915, 49917, 49930, 49992, 39901, 39911] {
            assert!(!iau_ographic("IAU_2015", code), "{code}");
        }
        assert!(!iau_ographic("EPSG", 49901));
        for name in ["Mars (2015) / Ographic", "GCS_Mars_2015_Ographic", "Mars_2015_Ographic_Equirectangular_clon0", "Mars planetographic"]
        {
            assert!(ographic_name(name), "{name}");
        }
        for name in ["Mars (2015) - Sphere / Ocentric", "Mars cartographic grid", "photographic mosaic", "Ographics", ""] {
            assert!(!ographic_name(name), "{name}");
        }
    }

    #[test]
    fn declared_by_code_or_definition() {
        let lonlat = |d: Option<Declared>| matches!(d, Some(Declared::LonLat { .. }));
        let refused = |d: Option<Declared>| matches!(d, Some(Declared::Refused(_)));
        assert!(lonlat(declared(Some("IAU_2015"), Some(49900), None)));
        assert!(refused(declared(Some("IAU_2015"), Some(49901), None)));
        assert!(refused(declared(Some("IAU_2015"), Some(49910), None)));
        assert!(declared(Some("IAU_2015"), Some(39900), None).is_none());
        assert!(declared(Some("EPSG"), Some(4326), None).is_none());
        assert!(declared(Some("NONE"), Some(100_001), Some("undefined")).is_none());
        assert!(declared(None, None, None).is_none());
        // The definition goes before the code: an Earth definition under any organization is the
        // Earth's, a Mars one is Mars's.
        let earth = r#"GEOGCS["NAD27",DATUM["North_American_Datum_1927",SPHEROID["Clarke 1866",6378206.4,294.978698213898]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]"#;
        assert!(declared(Some("NONE"), Some(100_001), Some(earth)).is_none());
        assert!(declared(Some("IAU_2015"), Some(49910), Some(earth)).is_none());
        let mars = r#"GEOGCS["Mars (2015) - Sphere / Ocentric",DATUM["Mars (2015) - Sphere",SPHEROID["Mars (2015) - Sphere",3396190,0]],PRIMEM["Reference Meridian",0],UNIT["degree",0.0174532925199433]]"#;
        match declared(Some("NONE"), Some(100_001), Some(mars)) {
            Some(Declared::LonLat { note, crs: Some(crs) }) => {
                assert!(note.contains("Mars (2015) - Sphere") && note.contains("drawn and measured"), "{note}");
                assert!(crs.is_planetary() && crs.is_geographic());
            }
            other => panic!("{other:?}"),
        }
        let eqc = r#"PROJCS["Mars (2015) - Sphere / Ocentric / Equirectangular, clon = 0",GEOGCS["Mars (2015) - Sphere / Ocentric",DATUM["Mars (2015) - Sphere",SPHEROID["Mars (2015) - Sphere",3396190,0]],PRIMEM["Reference Meridian",0],UNIT["degree",0.0174532925199433]],PROJECTION["Equirectangular"],PARAMETER["standard_parallel_1",0],PARAMETER["central_meridian",0],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["metre",1]]"#;
        assert!(refused(declared(Some("IAU_2015"), Some(49910), Some(eqc))));
        assert!(refused(declared(Some("NONE"), Some(1), Some(eqc))));
    }

    #[test]
    fn projjson_by_id_and_by_ellipsoid() {
        let lonlat = |d: Option<Declared>| matches!(d, Some(Declared::LonLat { .. }));
        let refused = |d: Option<Declared>| matches!(d, Some(Declared::Refused(_)));
        let json = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
        assert!(lonlat(projjson(&json(
            r#"{"type":"GeographicCRS","name":"Mars (2015) - Sphere / Ocentric","id":{"authority":"IAU_2015","code":49900}}"#
        ))));
        assert!(refused(projjson(&json(r#"{"type":"ProjectedCRS","id":{"authority":"IAU_2015","code":"49910"}}"#))));
        let sphere = |kind: &str, dir: &str, unit: &str| {
            json(&format!(
                r#"{{"type":"{kind}","name":"m","datum":{{"type":"GeodeticReferenceFrame","name":"Mars","ellipsoid":{{"name":"Mars","radius":{unit}}}}},"coordinate_system":{{"subtype":"ellipsoidal","axis":[{{"name":"lat","direction":"north"}},{{"name":"lon","direction":"{dir}"}}]}}}}"#
            ))
        };
        assert!(lonlat(projjson(&sphere("GeographicCRS", "east", "3396190"))));
        assert!(lonlat(projjson(&sphere(
            "GeographicCRS",
            "east",
            r#"{"value":3396.19,"unit":{"type":"LinearUnit","name":"km","conversion_factor":1000}}"#
        ))));
        assert!(refused(projjson(&sphere("GeographicCRS", "west", "3396190"))));
        assert!(projjson(&sphere("GeographicCRS", "east", "6378137")).is_none());
        let projected = json(
            r#"{"type":"ProjectedCRS","name":"eqc","base_crs":{"name":"Mars","datum":{"name":"Mars","ellipsoid":{"name":"Mars","semi_major_axis":3396190,"inverse_flattening":169.8944}}},"conversion":{"name":"x"}}"#,
        );
        assert!(refused(projjson(&projected)));
        // Geocentric X/Y/Z, and longitude/latitude in another unit than degrees, by code or by
        // ellipsoid; degrees by name or factor, and a height in metres, are read.
        let axes = |kind: &str, subtype: &str, unit: &str, id: &str| {
            json(&format!(
                r#"{{"type":"{kind}","name":"m","datum":{{"name":"Mars","ellipsoid":{{"name":"Mars","radius":3396190}}}},"coordinate_system":{{"subtype":"{subtype}","axis":[{{"name":"lat","direction":"north","unit":{unit}}},{{"name":"lon","direction":"east","unit":{unit}}},{{"name":"h","direction":"up","unit":"metre"}}]}}{id}}}"#
            ))
        };
        let why = |d: Option<Declared>| match d {
            Some(Declared::Refused(e)) => e,
            other => format!("{other:?}"),
        };
        for id in ["", r#","id":{"authority":"IAU_2015","code":49900}"#] {
            assert!(why(projjson(&axes("GeodeticCRS", "Cartesian", r#""metre""#, id))).contains(GEOCENTRIC), "{id}");
            assert!(why(projjson(&axes("GeodeticCRS", "cartesian", r#""degree""#, id))).contains(GEOCENTRIC), "{id}");
            assert!(why(projjson(&axes("GeographicCRS", "ellipsoidal", r#""radian""#, id))).contains("in radian, not degrees"), "{id}");
            let grad = r#"{"name":"grad","conversion_factor":0.015707963267949}"#;
            assert!(why(projjson(&axes("GeographicCRS", "ellipsoidal", grad, id))).contains("in grad, not degrees"), "{id}");
            let unnamed = r#"{"conversion_factor":1}"#;
            assert!(why(projjson(&axes("GeographicCRS", "ellipsoidal", unnamed, id))).contains("units of 1 radians"), "{id}");
            // A unit named degree but sized otherwise is what its factor says.
            let liar = r#"{"name":"degree","conversion_factor":1}"#;
            assert!(why(projjson(&axes("GeographicCRS", "ellipsoidal", liar, id))).contains("in units of 1 radians, not degrees"), "{id}");
            for unit in [r#""degree""#, r#""Degrees""#, r#"{"name":"degree"}"#, r#"{"name":"deg","conversion_factor":0.0174532925199433}"#]
            {
                assert!(lonlat(projjson(&axes("GeographicCRS", "ellipsoidal", unit, id))), "{unit} {id}");
                assert!(lonlat(projjson(&axes("GeodeticCRS", "spherical", unit, id))), "{unit} {id}");
            }
        }
        // Junk units are no unit RustGIS knows, not a crash.
        assert!(refused(projjson(&axes("GeographicCRS", "ellipsoidal", "[1,2]", ""))));
        assert!(refused(projjson(&axes("GeographicCRS", "ellipsoidal", r#"{"conversion_factor":"x"}"#, ""))));
        // The definition goes before the code, as in WKT: a projected or mirrored one under a
        // planetocentric code is refused.
        assert!(refused(projjson(&json(r#"{"type":"ProjectedCRS","id":{"authority":"IAU_2015","code":49900}}"#))));
        let mut west = sphere("GeographicCRS", "west", "3396190");
        west["id"] = json(r#"{"authority":"IAU_2015","code":49900}"#);
        assert!(refused(projjson(&west)));
        let mut east = sphere("GeographicCRS", "east", "3396190");
        east["id"] = json(r#"{"authority":"IAU_2015","code":49900}"#);
        assert!(lonlat(projjson(&east)));
        // Latitudes counted south, by code or by ellipsoid.
        let south = |v: &serde_json::Value| json(&v.to_string().replace(r#""direction":"north""#, r#""direction":"south""#));
        for v in [&east, &sphere("GeographicCRS", "east", "3396190")] {
            match projjson(&south(v)) {
                Some(Declared::Refused(e)) => assert!(e.contains("latitudes count south"), "{e}"),
                other => panic!("{other:?}"),
            }
        }
        // The notice says what told the body: the code, or the ellipsoid (with no code to name).
        let note = |v: &serde_json::Value| match projjson(v) {
            Some(Declared::LonLat { note, crs: None }) => note,
            other => panic!("{other:?}"),
        };
        assert!(note(&east).contains("as only its code says"), "{}", note(&east));
        let by_ellipsoid = note(&sphere("GeographicCRS", "east", "3396190"));
        assert!(by_ellipsoid.contains("as only its ellipsoid says") && !by_ellipsoid.contains("code"), "{by_ellipsoid}");
        // EPSG systems, junk and nothing say no body.
        assert!(projjson(&json(r#"{"type":"GeographicCRS","id":{"authority":"EPSG","code":4326}}"#)).is_none());
        assert!(projjson(&json(r#"{"datum":{"ellipsoid":{"radius":"x"}}}"#)).is_none());
        assert!(projjson(&json("null")).is_none() && projjson(&json("[]")).is_none());
    }
}
