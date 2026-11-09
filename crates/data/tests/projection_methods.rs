//! Projection methods read from a Shapefile's `.prj` and from a GeoTIFF's GeoKeys, through the
//! public readers: sinusoidal, Robinson, orthographic, Mercator (variant A) and the
//! PROJ-compatible equidistant cylindrical of EPSG:4087. Each places a point where expected,
//! and a method that still isn't supported keeps saying so.
#![cfg(not(target_arch = "wasm32"))]

use rustgis_data::raster::{Crs, Raster};
use rustgis_data::{Coord, Dataset, Geometry, GeometryType, io};

/// WGS 84's semi-major axis and eccentricity squared.
const A: f64 = 6_378_137.0;
const ES: f64 = 0.006_694_379_990_141_317;
/// Mars's IAU 2015 sphere (IAU_2015:49900), as GDAL writes its `.prj`.
const MARS_R: f64 = 3_396_190.0;
const MARS_GEOGCS: &str = r#"GEOGCS["GCS_Mars_2015_-_Sphere_Ocentric",DATUM["D_Mars_2015_-_Sphere",SPHEROID["Mars_2015_-_Sphere",3396190.0,0.0]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]]"#;
const WGS84_GEOGCS: &str = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;

/// The same EPSG method can be declared with or without a CRS authority. Both readers must
/// report the compatibility choice without changing the PROJ/GDAL coordinates.
#[test]
fn explicit_method_1028_warns_in_full_and_streamed_shapefiles() {
    let wkt = r#"PROJCRS["World EQC",BASEGEOGCRS["WGS 84",DATUM["WGS_1984",ELLIPSOID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0]],CONVERSION["EQC",METHOD["Equidistant Cylindrical",ID["EPSG",1028]],PARAMETER["Latitude of 1st standard parallel",0],PARAMETER["Longitude of natural origin",0],PARAMETER["False easting",0],PARAMETER["False northing",0]],CS[Cartesian,2],AXIS["Easting",east],AXIS["Northing",north],LENGTHUNIT["metre",1]]"#;
    let coded = format!("{},ID[\"EPSG\",4087]]", wkt.strip_suffix(']').unwrap());
    let sphere = wkt.replace("WGS_1984", "Sphere").replace("298.257223563", "0");
    let crs = rustgis_data::raster::wkt::crs_from_wkt(wkt).unwrap();
    let wkt1 = rustgis_data::raster::wkt::to_wkt(crs).unwrap();
    for (prj, warned) in [(wkt, true), (coded.as_str(), true), (sphere.as_str(), false), (wkt1.as_str(), false)] {
        let mut ds = Dataset::new("p", GeometryType::Point, Vec::new());
        ds.push(Some(Geometry::Point(Coord::new(0.0, 5_009_377.085_697_311))), Vec::new());
        let dir = tempfile::tempdir().unwrap();
        for (ext, bytes) in io::shapefile_parts(&ds).unwrap().0 {
            let bytes = if ext == "prj" { prj.as_bytes().to_vec() } else { bytes };
            std::fs::write(dir.path().join(format!("p.{ext}")), bytes).unwrap();
        }
        let path = dir.path().join("p.shp");
        let (read, report) = io::read_path(&path).unwrap();
        near(read.features.first().and_then(|f| f.geometry.as_ref()?.label_point()), 0.0, 45.0, 1e-9);
        let streamed = rustgis_data::source::open_path(&path, None).unwrap();
        for notes in [&report.messages, &streamed.info().notes] {
            assert_eq!(notes.iter().filter(|n| n.contains("EPSG method 1028")).count(), usize::from(warned), "{prj}: {notes:?}");
            if warned {
                assert!(notes.iter().any(|n| n.contains("PROJ/GDAL") && n.contains("differ")), "{notes:?}");
            }
        }
    }
}

/// A Shapefile of one point at `x`, `y` in a system its `.prj` gives, read back by
/// [`io::read_path`]: the point as longitude/latitude, and what the read said.
fn read_prj(prj: &str, x: f64, y: f64) -> (Option<Coord>, Vec<String>) {
    let mut ds = Dataset::new("p", GeometryType::Point, Vec::new());
    ds.push(Some(Geometry::Point(Coord::new(x, y))), Vec::new());
    let dir = tempfile::tempdir().unwrap();
    for (ext, bytes) in io::shapefile_parts(&ds).unwrap().0 {
        let bytes = if ext == "prj" { prj.as_bytes().to_vec() } else { bytes };
        std::fs::write(dir.path().join(format!("p.{ext}")), bytes).unwrap();
    }
    let (read, report) = io::read_path(&dir.path().join("p.shp")).unwrap();
    let at = match read.features.first().and_then(|f| f.geometry.clone()) {
        Some(Geometry::Point(c)) => Some(c),
        _ => None,
    };
    (at, report.messages)
}

/// A 2×2 GeoTIFF whose top-left corner is at `corner`, in a user-defined projection: coordinate
/// transformation `ct` with its double parameters by key, on WGS 84 (`earth` None) or on a sphere
/// or ellipsoid of the axes given.
fn tiff(ct: u16, params: &[(u16, f64)], earth: Option<(f64, f64)>, corner: (f64, f64)) -> Vec<u8> {
    use tiff::encoder::{TiffEncoder, colortype};
    use tiff::tags::Tag;
    let mut shorts: Vec<(u16, u16)> = vec![(1024, 1), (1025, 1), (3072, 32767), (3074, 32767), (3075, ct), (3076, 9001)];
    let mut doubles: Vec<(u16, f64)> = params.to_vec();
    match earth {
        None => shorts.push((2048, 4326)),
        Some((a, b)) => {
            shorts.extend([(2048, 32767), (2050, 32767), (2056, 32767)]);
            doubles.extend([(2057, a), (2058, b)]);
        }
    }
    let mut entries: Vec<[u16; 4]> = shorts.iter().map(|(k, v)| [*k, 0, 1, *v]).collect();
    doubles.sort_by_key(|(k, _)| *k);
    for (i, (k, _)) in doubles.iter().enumerate() {
        entries.push([*k, 34736, 1, i as u16]);
    }
    entries.sort_by_key(|e| e[0]);
    let mut keys = vec![1u16, 1, 0, entries.len() as u16];
    keys.extend(entries.iter().flatten());
    let values: Vec<f64> = doubles.iter().map(|(_, v)| *v).collect();
    let mut out = Vec::new();
    {
        let mut enc = TiffEncoder::new(std::io::Cursor::new(&mut out)).unwrap();
        let mut img = enc.new_image::<colortype::Gray32Float>(2, 2).unwrap();
        let e = img.encoder();
        e.write_tag(Tag::ModelPixelScaleTag, &[1000.0f64, 1000.0, 0.0][..]).unwrap();
        e.write_tag(Tag::ModelTiepointTag, &[0.0f64, 0.0, 0.0, corner.0, corner.1, 0.0][..]).unwrap();
        e.write_tag(Tag::GeoKeyDirectoryTag, &keys[..]).unwrap();
        if !values.is_empty() {
            e.write_tag(Tag::Unknown(34736), &values[..]).unwrap();
        }
        img.write_data(&[1.0f32, 2.0, 3.0, 4.0]).unwrap();
    }
    out
}

/// The GeoTIFF read by [`rustgis_data::raster::read_bytes`], and where its top-left corner is.
fn read_tiff(bytes: &[u8]) -> (Raster, Coord) {
    let r = rustgis_data::raster::read_bytes("t.tif", bytes).unwrap_or_else(|e| panic!("{e}"));
    let corner = Coord::new(r.transform.origin_x, r.transform.origin_y);
    let at = r.crs.transformer().unwrap().to_lonlat(corner).unwrap();
    (r, at)
}

/// Written as a GeoTIFF and read again, the raster's system is the same and places its corner in
/// the same place.
fn written_back(r: &Raster) {
    let back = rustgis_data::raster::read_bytes("b.tif", &rustgis_data::raster::geotiff::write(r, 0).unwrap()).unwrap();
    let corner = Coord::new(r.transform.origin_x, r.transform.origin_y);
    let (p, q) = (r.crs.transformer().unwrap().to_lonlat(corner).unwrap(), back.crs.transformer().unwrap().to_lonlat(corner).unwrap());
    assert!((p.x - q.x).abs() < 1e-9 && (p.y - q.y).abs() < 1e-9, "{p:?} {q:?}");
}

fn near(at: Option<Coord>, lon: f64, lat: f64, tolerance: f64) {
    let at = at.unwrap_or_else(|| panic!("no point, expected {lon}, {lat}"));
    assert!((at.x - lon).abs() < tolerance && (at.y - lat).abs() < tolerance, "{at:?}, expected {lon}, {lat}");
}

/// No notice at all: nothing was dropped or assumed.
fn clean(messages: &[String]) {
    assert!(messages.is_empty(), "{messages:?}");
}

/// No "isn't supported": a Mars file may still get datum notes, which don't depend on the method.
fn read_as_projected(messages: &[String]) {
    assert!(!messages.iter().any(|m| m.contains("isn't supported") || m.contains("read as longitude/latitude")), "{messages:?}");
}

/// Sinusoidal: ESRI:54008 (World Sinusoidal) as Esri writes its `.prj`, and the IAU's Mars
/// sinusoidal (IAU_2015:49920) as GDAL writes it. The World Sinusoidal point is pyproj 3.7.1's
/// (PROJ 9.5.1), as `rustgis_geom::sinusoidal`'s tests record it; on a sphere x = R·λ·cos φ,
/// y = R·φ (Snyder 1987, ch. 30, eqs. 30-1, 30-2).
#[test]
fn sinusoidal() {
    let world = format!(
        r#"PROJCS["World_Sinusoidal",{WGS84_GEOGCS},PROJECTION["Sinusoidal"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],UNIT["Meter",1.0]]"#
    );
    let (at, messages) = read_prj(&world, -6_055_475.958, -1_387_927.939);
    near(at, -55.72, -12.55, 1e-7);
    clean(&messages);
    let mars = format!(
        r#"PROJCS["Mars_2015_-_Sphere_Ocentric_Sinusoidal_clon_0",{MARS_GEOGCS},PROJECTION["Sinusoidal"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],UNIT["Meter",1.0]]"#
    );
    let (lon, lat) = (77.45f64, 18.44f64);
    let (at, messages) = read_prj(&mars, MARS_R * lon.to_radians() * lat.to_radians().cos(), MARS_R * lat.to_radians());
    near(at, lon, lat, 1e-9);
    read_as_projected(&messages);
    // A GeoTIFF (coordinate transformation 24) on WGS 84, its centre at 15°E.
    let bytes = tiff(24, &[(3082, 1000.0), (3083, -2000.0), (3088, 15.0)], None, (-6_055_475.958 + 1000.0, -1_387_927.939 - 2000.0));
    let (r, at) = read_tiff(&bytes);
    near(Some(at), -55.72 + 15.0, -12.55, 1e-7);
    written_back(&r);
}

/// Robinson, from Robinson's table (as Snyder and Voxland 1989, *An Album of Map Projections*,
/// USGS Professional Paper 1453, give it): x = 0.8487·R·X(φ)·λ and y = 1.3523·R·Y(φ), with
/// X(40°) = 0.9216, Y(40°) = 0.4958, X(50°) = 0.8679 and Y(50°) = 0.6176 at the table's rows,
/// where no interpolation enters. The `.prj` is Esri's World Robinson moved to 150°E with a false
/// origin of its own, and the IAU's Mars Robinson (IAU_2015:49950) as GDAL writes it.
#[test]
fn robinson() {
    let x = |r: f64, plen: f64, dlon: f64| 0.8487 * r * plen * dlon.to_radians();
    let y = |r: f64, pdfe: f64| 1.3523 * r * pdfe;
    let world = format!(
        r#"PROJCS["World_Robinson",{WGS84_GEOGCS},PROJECTION["Robinson"],PARAMETER["False_Easting",1000.0],PARAMETER["False_Northing",-2000.0],PARAMETER["Central_Meridian",150.0],UNIT["Meter",1.0]]"#
    );
    // 170°E, 40°N: 20° east of the central meridian.
    let (px, py) = (x(A, 0.9216, 20.0), y(A, 0.4958));
    // Projected by the system read from the `.prj` within a decimetre of the table (proj4rs, as
    // PROJ, interpolates it with cubic splines: 1 741 397.303, 4 276 351.584 from the origin);
    // read back to within 2e-6°, where its inverse settles.
    let crs = rustgis_data::raster::wkt::crs_from_wkt(&world).unwrap();
    let m = crs.transformer().unwrap().from_lonlat(Coord::new(170.0, 40.0)).unwrap();
    assert!((m.x - px - 1000.0).abs() < 0.2 && (m.y - py + 2000.0).abs() < 0.2, "{m:?}");
    let (at, messages) = read_prj(&world, px + 1000.0, py - 2000.0);
    near(at, 170.0, 40.0, 2e-6);
    clean(&messages);
    let mars = format!(
        r#"PROJCS["Mars_2015_-_Sphere_Ocentric_Robinson_clon_0",{MARS_GEOGCS},PROJECTION["Robinson"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],UNIT["Meter",1.0]]"#
    );
    let (at, messages) = read_prj(&mars, x(MARS_R, 0.8679, -60.0), -y(MARS_R, 0.6176));
    near(at, -60.0, -50.0, 2e-6);
    read_as_projected(&messages);
    // A GeoTIFF (coordinate transformation 23, CT_Robinson) by the projection centre's key.
    let bytes = tiff(23, &[(3082, 1000.0), (3083, -2000.0), (3088, 150.0)], None, (px + 1000.0, py - 2000.0));
    let (r, at) = read_tiff(&bytes);
    near(Some(at), 170.0, 40.0, 2e-6);
    assert!(r.crs.custom_def().unwrap().proj4.starts_with("+proj=robin +lon_0=150"), "{:?}", r.crs.custom_def());
    written_back(&r);
}

/// The ellipsoidal orthographic of the EPSG Guidance Note 7-2 (method 9840), from the geocentric
/// position projected onto the plane tangent at the origin:
/// E = ν·cos φ·sin(λ − λ₀) and
/// N = ν·(sin φ·cos φ₀ − cos φ·sin φ₀·cos(λ − λ₀)) + e²·(ν₀·sin φ₀ − ν·sin φ)·cos φ₀.
fn ortho_ellipsoidal(lon0: f64, lat0: f64, lon: f64, lat: f64) -> (f64, f64) {
    let nu = |phi: f64| A / (1.0 - ES * phi.sin() * phi.sin()).sqrt();
    let (p0, p, dl) = (lat0.to_radians(), lat.to_radians(), (lon - lon0).to_radians());
    let e = nu(p) * p.cos() * dl.sin();
    let n = nu(p) * (p.sin() * p0.cos() - p.cos() * p0.sin() * dl.cos()) + ES * (nu(p0) * p0.sin() - nu(p) * p.sin()) * p0.cos();
    (e, n)
}

/// Orthographic: on a sphere, Snyder's (1987, ch. 20, eqs. 20-3 and 20-4) worked example
/// (appendix A: R = 1, centre 40°N 100°W, the point 30°N 110°W at x = −0.1503837,
/// y = −0.1651911) on Mars's sphere; IAU_2015:49965 as GDAL writes it; and on WGS 84, EPSG's
/// ellipsoidal method as OGC WKT names it. Esri's Orthographic is a spherical one: on an
/// ellipsoid it is refused, and says so.
#[test]
fn orthographic() {
    let snyder = format!(
        r#"PROJCS["Mars_Orthographic",{MARS_GEOGCS},PROJECTION["Orthographic"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Longitude_Of_Center",-100.0],PARAMETER["Latitude_Of_Center",40.0],UNIT["Meter",1.0]]"#
    );
    let (at, messages) = read_prj(&snyder, -0.150_383_7 * MARS_R, -0.165_191_1 * MARS_R);
    // Seven digits of a unit sphere are a third of a metre on Mars: 1e-5°.
    near(at, -110.0, 30.0, 1e-5);
    read_as_projected(&messages);
    // IAU_2015:49965, centred on 0°, 0°: x = R·cos φ·sin λ, y = R·sin φ.
    let iau = format!(
        r#"PROJCS["Mars_2015_-_Sphere_Ocentric_Orthographic_clon_0",{MARS_GEOGCS},PROJECTION["Orthographic"],PARAMETER["Latitude of natural origin",0.0],PARAMETER["Longitude of natural origin",0.0],PARAMETER["False easting",0.0],PARAMETER["False northing",0.0],UNIT["Meter",1.0]]"#
    );
    let (lon, lat) = (30.0f64, 20.0f64);
    let (at, messages) = read_prj(&iau, MARS_R * lat.to_radians().cos() * lon.to_radians().sin(), MARS_R * lat.to_radians().sin());
    near(at, lon, lat, 1e-9);
    read_as_projected(&messages);
    // On WGS 84 (OGC WKT1, as GDAL writes EPSG's method 9840), centred on 55°N 5°E.
    let ogc = r#"PROJCS["WGS 84 / Orthographic",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Orthographic"],PARAMETER["latitude_of_origin",55],PARAMETER["central_meridian",5],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["metre",1]]"#;
    let (e, n) = ortho_ellipsoidal(5.0, 55.0, 2.129_55, 53.809_39);
    let (at, messages) = read_prj(ogc, e, n);
    near(at, 2.129_55, 53.809_39, 1e-8);
    clean(&messages);
    // Esri's, on an ellipsoid: refused, with the reason.
    let esri = format!(
        r#"PROJCS["World_Orthographic",{WGS84_GEOGCS},PROJECTION["Orthographic"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Longitude_Of_Center",5.0],PARAMETER["Latitude_Of_Center",55.0],UNIT["Meter",1.0]]"#
    );
    let (_, messages) = read_prj(&esri, e, n);
    assert!(messages.iter().any(|m| m.contains("Orthographic") && m.contains("isn't supported on an ellipsoid")), "{messages:?}");
    // A GeoTIFF (coordinate transformation 21, CT_Orthographic) on WGS 84, by the projection
    // centre's keys as GDAL writes them.
    let bytes = tiff(21, &[(3082, 0.0), (3083, 0.0), (3088, 5.0), (3089, 55.0)], None, (e, n));
    let (r, at) = read_tiff(&bytes);
    near(Some(at), 2.129_55, 53.809_39, 1e-8);
    written_back(&r);
    // Or by the natural origin's keys.
    let bytes = tiff(21, &[(3080, 5.0), (3081, 55.0), (3082, 0.0), (3083, 0.0)], None, (e, n));
    near(Some(read_tiff(&bytes).1), 2.129_55, 53.809_39, 1e-8);
}

/// Mercator (variant A), as GDAL writes the IAU's Mars Mercator (IAU_2015:49990) in Esri WKT:
/// on a sphere x = R·k₀·λ and y = R·k₀·ln tan(45° + φ/2) (Snyder 1987, ch. 7, eqs. 7-1 and 7-2,
/// times the scale factor k₀).
#[test]
fn mercator_variant_a() {
    let mars = format!(
        r#"PROJCS["Mars_2015_-_Sphere_Ocentric_Mercator",{MARS_GEOGCS},PROJECTION["Mercator_Variant_A"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],PARAMETER["Scale_Factor",1.0],UNIT["Meter",1.0]]"#
    );
    let (lon, lat) = (137.4f64, -5.4f64);
    let y = |k0: f64| MARS_R * k0 * (std::f64::consts::FRAC_PI_4 + lat.to_radians() / 2.0).tan().ln();
    let (at, messages) = read_prj(&mars, MARS_R * lon.to_radians(), y(1.0));
    near(at, lon, lat, 1e-9);
    read_as_projected(&messages);
    // A scale factor of its own.
    let scaled = mars.replace(r#"PARAMETER["Scale_Factor",1.0]"#, r#"PARAMETER["Scale_Factor",0.997]"#);
    let (at, _) = read_prj(&scaled, MARS_R * 0.997 * lon.to_radians(), y(0.997));
    near(at, lon, lat, 1e-9);
    // A GeoTIFF (coordinate transformation 7) on the same sphere.
    let bytes = tiff(
        7,
        &[(3080, 0.0), (3082, 0.0), (3083, 0.0), (3092, 0.997)],
        Some((MARS_R, MARS_R)),
        (MARS_R * 0.997 * lon.to_radians(), y(0.997)),
    );
    let (r, at) = read_tiff(&bytes);
    near(Some(at), lon, lat, 1e-9);
    written_back(&r);
}

/// EPSG:4087 matches PROJ/GDAL's sphere of WGS 84's semi-major axis. Coordinates from PROJ
/// 9.7.1 are read at the same latitude from both a Shapefile and a GeoTIFF.
#[test]
fn equidistant_cylindrical_epsg_4087() {
    let esri = format!(
        r#"PROJCS["WGS_1984_World_Equidistant_Cylindrical",{WGS84_GEOGCS},PROJECTION["Equidistant_Cylindrical"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],PARAMETER["Standard_Parallel_1",0.0],UNIT["Meter",1.0]]"#
    );
    let coded = esri.strip_suffix(']').unwrap().to_string() + r#",AUTHORITY["EPSG",4087]]"#;
    for prj in [&esri, &coded] {
        // Within a centimetre (1e-7° is 1.1 cm of latitude).
        let (at, messages) = read_prj(prj, 0.0, 5_009_377.085_697_311);
        near(at, 0.0, 45.0, 1e-7);
        clean(&messages);
        let (at, _) = read_prj(prj, 1_113_194.907_932_735_7, 6_122_571.993_630_046);
        near(at, 10.0, 55.0, 1e-7);
    }
    assert_eq!(rustgis_data::raster::wkt::crs_from_wkt(&coded), Ok(Crs::Epsg(4087)));
    // A GeoTIFF with user-defined keys (coordinate transformation 17) on WGS 84, and one by its
    // EPSG code.
    let user_defined = tiff(17, &[], None, (1_113_194.907_932_735_7, 6_122_571.993_630_046));
    let (r, at) = read_tiff(&user_defined);
    assert!(matches!(r.crs, Crs::Custom(_)), "{:?}", r.crs);
    near(Some(at), 10.0, 55.0, 1e-7);
    written_back(&r);
    let gt =
        rustgis_data::raster::GeoTransform { origin_x: 0.0, origin_y: 5_009_377.085_697_311, pixel_width: 1000.0, pixel_height: -1000.0 };
    let r =
        Raster::new("e", 2, 2, vec![vec![1.0; 4]], gt, Crs::Epsg(4087), None, rustgis_data::raster::ColorHint::Gray, "Float32").unwrap();
    let (back, at) = read_tiff(&rustgis_data::raster::geotiff::write(&r, 0).unwrap());
    assert_eq!(back.crs, Crs::Epsg(4087));
    near(Some(at), 0.0, 45.0, 1e-7);
}

/// A method RustGIS still can't project keeps its notice: the coordinates are read as they are,
/// and the read says why. The same file in a method it reads says nothing.
#[test]
fn an_unsupported_method_still_says_so() {
    let vdg = format!(
        r#"PROJCS["World_Van_der_Grinten_I",{WGS84_GEOGCS},PROJECTION["Van_der_Grinten_I"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],UNIT["Meter",1.0]]"#
    );
    let (_, messages) = read_prj(&vdg, 12.0, 34.0);
    assert!(
        messages.iter().any(|m| m.contains("Van_der_Grinten_I projection isn't supported") && m.contains("read as longitude/latitude")),
        "{messages:?}"
    );
    let (_, messages) = read_prj(&vdg.replace("Van_der_Grinten_I", "Robinson"), 12.0, 34.0);
    clean(&messages);
    // A GeoTIFF in Van der Grinten (coordinate transformation 25) in metres is refused, by name.
    let bytes = tiff(25, &[(3088, 0.0)], None, (1_000_000.0, 2_000_000.0));
    let e = rustgis_data::raster::read_bytes("v.tif", &bytes).map(|r| r.crs);
    assert!(e.as_ref().is_err_and(|e| e.to_string().contains("coordinate transformation 25) isn't supported")), "{e:?}");
    // Mercator (Spherical), EPSG's method 1026, on an ellipsoid isn't computed as on a sphere.
    let spherical = format!(
        r#"PROJCS["x",{WGS84_GEOGCS},PROJECTION["Mercator_Spherical"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],UNIT["Meter",1.0]]"#
    );
    let (_, messages) = read_prj(&spherical, 12.0, 34.0);
    assert!(messages.iter().any(|m| m.contains("Mercator (Spherical) projection on an ellipsoid isn't supported")), "{messages:?}");
}
