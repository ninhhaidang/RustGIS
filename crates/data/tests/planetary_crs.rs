//! Coordinate systems of other bodies than the Earth (IAU 2015) through the real readers and
//! writers: their longitude/latitude are read as they are with one notice, coordinates that would
//! be in the wrong place are refused, Earth systems say nothing new, and exports say what they
//! write instead of the layer's system.
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};

use rustgis_data::raster::{self, Crs};
use rustgis_data::source::FeatureSource;
use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value, io, source};

/// Mars as a sphere, longitude/latitude (IAU_2015:49900), as GDAL writes it to a Shapefile's `.prj`.
const MARS_ESRI: &str = r#"GEOGCS["GCS_Mars_2015_-_Sphere_Ocentric",DATUM["D_Mars_2015_-_Sphere",SPHEROID["Mars_2015_-_Sphere",3396190.0,0.0]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]]"#;

/// The same system as PROJ writes it in WKT2 (2019), with its IAU identifier.
const MARS_WKT2: &str = r#"GEOGCRS["Mars (2015) - Sphere / Ocentric",DATUM["Mars (2015) - Sphere",ELLIPSOID["Mars (2015) - Sphere",3396190,0,LENGTHUNIT["metre",1]],ANCHOR["Viking 1 lander: 47.95137 W"]],PRIMEM["Reference Meridian",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["geodetic latitude (Lat)",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["geodetic longitude (Lon)",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["IAU",49900,2015],REMARK["Source of IAU Coordinate systems: https://doi.org/10.1007/s10569-017-9805-5"]]"#;

/// Mars planetographic (IAU_2015:49901): its longitudes count west.
const MARS_WEST: &str = r#"GEOGCRS["Mars (2015) / Ographic",DATUM["Mars (2015)",ELLIPSOID["Mars (2015)",3396190,169.894447223612,LENGTHUNIT["metre",1]],ANCHOR["Viking 1 lander: 47.95137 W"]],PRIMEM["Reference Meridian",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["geodetic latitude (Lat)",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["geodetic longitude (Lon)",west,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["IAU",49901,2015]]"#;

/// Mars equirectangular, central meridian 0 (IAU_2015:49910), as GDAL writes it to a `.prj`.
const MARS_EQC: &str = r#"PROJCS["Mars_2015_-_Sphere_Ocentric_Equirectangular_clon0",GEOGCS["GCS_Mars_2015_-_Sphere_Ocentric",DATUM["D_Mars_2015_-_Sphere",SPHEROID["Mars_2015_-_Sphere",3396190.0,0.0]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Equidistant_Cylindrical"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],PARAMETER["Standard_Parallel_1",0.0],UNIT["Meter",1.0]]"#;

/// A projection of Mars RustGIS doesn't read (Bonne, which no IAU code uses): its metres can't
/// be converted.
const MARS_BONNE: &str = r#"PROJCS["Mars_Bonne",GEOGCS["GCS_Mars_2015_-_Sphere_Ocentric",DATUM["D_Mars_2015_-_Sphere",SPHEROID["Mars_2015_-_Sphere",3396190.0,0.0]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Bonne"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],PARAMETER["Standard_Parallel_1",30.0],UNIT["Meter",1.0]]"#;

/// The same projection on the Earth: read as longitude/latitude with a notice, as before.
const EARTH_BONNE: &str = r#"PROJCS["World_Bonne",GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Bonne"],PARAMETER["False_Easting",0.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",0.0],PARAMETER["Standard_Parallel_1",30.0],UNIT["Meter",1.0]]"#;

/// NAD 27 with no shift to WGS 84: the Earth datum whose notice planetary data mustn't get.
const NAD27_ESRI: &str = r#"GEOGCS["GCS_North_American_1927",DATUM["D_North_American_1927",SPHEROID["Clarke_1866",6378206.4,294.9786982]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;

const ANOTHER_BODY: &str = "belongs to another body than the Earth";

/// Olympus Mons and Jezero crater (planetocentric, east positive).
fn craters(points: [(f64, f64); 2]) -> Dataset {
    let mut ds = Dataset::new("craters", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    for ((x, y), name) in points.into_iter().zip(["Olympus Mons", "Jezero"]) {
        ds.push(Some(Geometry::Point(Coord::new(x, y))), vec![Value::Text(name.into())]);
    }
    ds
}

const LONLAT: [(f64, f64); 2] = [(-133.8, 18.65), (77.45, 18.44)];

fn points(ds: &Dataset) -> Vec<Coord> {
    ds.features.iter().filter_map(|f| f.geometry.as_ref()?.label_point()).collect()
}

/// A Shapefile of `native` numbers with `prj` as its `.prj`.
fn shapefile(dir: &Path, name: &str, native: [(f64, f64); 2], prj: &str) -> PathBuf {
    let (parts, _) = io::shapefile_parts_in(&craters(native), Crs::LonLat).unwrap();
    for (ext, bytes) in parts {
        let bytes = if ext == "prj" { prj.as_bytes().to_vec() } else { bytes };
        std::fs::write(dir.join(format!("{name}.{ext}")), bytes).unwrap();
    }
    dir.join(format!("{name}.shp"))
}

fn planetary_notes(messages: &[String]) -> Vec<&String> {
    messages.iter().filter(|m| m.contains(ANOTHER_BODY)).collect()
}

fn datum_notes(messages: &[String]) -> Vec<&String> {
    messages.iter().filter(|m| m.contains("few hundred metres")).collect()
}

#[test]
fn a_mars_shapefile_is_read_as_its_longitude_latitude_with_one_planetary_notice() {
    let dir = tempfile::tempdir().unwrap();
    for (name, prj, datum) in [("esri", MARS_ESRI, "D_Mars_2015_-_Sphere"), ("wkt2", MARS_WKT2, "Mars (2015) - Sphere")] {
        let path = shapefile(dir.path(), name, LONLAT, prj);
        // Read whole.
        let (ds, report) = io::read_path(&path).unwrap();
        assert_eq!(points(&ds), LONLAT.map(|(x, y)| Coord::new(x, y)).to_vec(), "{name}");
        let said = planetary_notes(&report.messages);
        assert_eq!(said.len(), 1, "{name}: {:?}", report.messages);
        assert!(said[0].contains(datum) && said[0].contains("drawn and measured"), "{name}: {said:?}");
        // Not the Earth's datum note as well: it doesn't apply to another body.
        assert!(datum_notes(&report.messages).is_empty(), "{name}: {:?}", report.messages);
        let crs = ds.crs.unwrap();
        assert!(crs.is_planetary() && crs.is_geographic() && !crs.lacks_datum_shift(), "{name}");
        // Read in place.
        let src = source::open_path(&path, None).unwrap();
        let info = src.info();
        assert_eq!(planetary_notes(&info.notes).len(), 1, "{name}: {:?}", info.notes);
        assert!(datum_notes(&info.notes).is_empty(), "{name}: {:?}", info.notes);
        let (read, _) = source::materialize(src.as_ref()).unwrap();
        assert_eq!(points(&read), LONLAT.map(|(x, y)| Coord::new(x, y)).to_vec(), "{name}");
        // Its system goes with the layer, for an export to say.
        assert_eq!(read.crs, Some(crs), "{name}");
    }
}

#[test]
fn a_mars_projection_is_unprojected_on_its_sphere_with_the_notice() {
    let dir = tempfile::tempdir().unwrap();
    let r = 3_396_190.0f64;
    let native = LONLAT.map(|(lon, lat)| (r * lon.to_radians(), r * lat.to_radians()));
    let path = shapefile(dir.path(), "eqc", native, MARS_EQC);
    let (ds, report) = io::read_path(&path).unwrap();
    for (got, (lon, lat)) in points(&ds).into_iter().zip(LONLAT) {
        assert!((got.x - lon).abs() < 1e-9 && (got.y - lat).abs() < 1e-9, "{got:?}");
    }
    assert_eq!(planetary_notes(&report.messages).len(), 1, "{:?}", report.messages);
    assert!(datum_notes(&report.messages).is_empty(), "{:?}", report.messages);
}

#[test]
fn longitudes_counted_west_and_unread_mars_projections_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    // West positive: Olympus Mons is 133.8°W, which read as east would be mirrored to 133.8°E.
    let west = shapefile(dir.path(), "west", [(133.8, 18.65), (282.55, 18.44)], MARS_WEST);
    let bonne = shapefile(dir.path(), "bonne", [(1_000_000.0, 2_000_000.0), (-500_000.0, 10.0)], MARS_BONNE);
    for (path, why) in [(&west, "west"), (&bonne, "Bonne")] {
        let e = io::read_path(path).unwrap_err().to_string();
        assert!(e.contains(ANOTHER_BODY) && e.contains(why) && e.contains("wrong place"), "{e}");
        let e = source::open_path(path, None).err().unwrap().to_string();
        assert!(e.contains(ANOTHER_BODY) && e.contains(why), "{e}");
    }
    // The same projection on the Earth is read as before, with its notice.
    let earth = shapefile(dir.path(), "earth_bonne", [(1.0, 2.0), (3.0, 4.0)], EARTH_BONNE);
    let (ds, report) = io::read_path(&earth).unwrap();
    assert_eq!(ds.features.len(), 2);
    assert!(report.messages.iter().any(|m| m.contains("Bonne") && m.contains("read as longitude/latitude")), "{:?}", report.messages);
    assert!(planetary_notes(&report.messages).is_empty());
}

/// Mars planetographic as Esri WKT, named as [`MARS_ESRI`] is: an Esri `.prj` has no axes, so
/// nothing says its longitudes count east.
const MARS_OGRAPHIC_ESRI: &str = r#"GEOGCS["GCS_Mars_2015_Ographic",DATUM["D_Mars_2015",SPHEROID["Mars_2015",3396190.0,169.894447223612]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]]"#;

/// A planetographic system, or a projection of one, whose axes don't say they count east is
/// refused (most bodies' planetographic longitudes count west, and PROJ gives a projection of them
/// westings), by its name, its IAU code or its geographic system; one whose axes say east is read.
#[test]
fn planetographic_systems_without_east_axes_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let r = 3_396_190.0f64;
    let metres = LONLAT.map(|(lon, lat)| (r * lon.to_radians(), r * lat.to_radians()));
    // The planetographic systems are on the flattened Mars (2015) ellipsoid, where equidistant
    // cylindrical northings are meridian distances (EPSG method 1028), not r·φ.
    let f = 1.0 / 169.894447223612;
    let eqc = rustgis_geom::EquidistantCylindrical::new(r, f * (2.0 - f), 0.0, 0.0, 0.0, 0.0, 0.0).unwrap();
    let on_ellipsoid = LONLAT.map(|(lon, lat)| {
        let c = eqc.forward(Coord::new(lon, lat)).unwrap();
        (c.x, c.y)
    });
    let with_axes = |wkt: &str, axes: &str| format!("{},{axes}]", wkt.strip_suffix(']').unwrap());
    let east_north = r#"AXIS["Lon",EAST],AXIS["Lat",NORTH]"#;
    let unnamed = MARS_OGRAPHIC_ESRI.replace("GCS_Mars_2015_Ographic", "GCS_Mars");
    let eqc_on = |geogcs: &str, name: &str| {
        MARS_EQC.replace("Mars_2015_-_Sphere_Ocentric_Equirectangular_clon0", name).replace(
            r#"GEOGCS["GCS_Mars_2015_-_Sphere_Ocentric",DATUM["D_Mars_2015_-_Sphere",SPHEROID["Mars_2015_-_Sphere",3396190.0,0.0]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]]"#,
            geogcs,
        )
    };
    let wkt2_eqc = |base: &str, x: &str| {
        format!(
            r#"PROJCRS["Mars (2015) / Ographic / Equirectangular, clon = 0",BASEGEOGCRS["{base}",DATUM["Mars (2015)",ELLIPSOID["Mars (2015)",3396190,169.894447223612,LENGTHUNIT["metre",1]]],PRIMEM["Reference Meridian",0,ANGLEUNIT["degree",0.0174532925199433]]],CONVERSION["Equirectangular, clon = 0",METHOD["Equidistant Cylindrical",ID["EPSG",1028]],PARAMETER["Latitude of 1st standard parallel",0,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["Longitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433]],PARAMETER["False easting",0,LENGTHUNIT["metre",1]],PARAMETER["False northing",0,LENGTHUNIT["metre",1]]],CS[Cartesian,2],AXIS["{x}",{x},ORDER[1],LENGTHUNIT["metre",1]],AXIS["northing",north,ORDER[2],LENGTHUNIT["metre",1]]]"#
        )
    };
    let without_axes = MARS_WEST.replace(r#"CS[ellipsoidal,2],"#, "").replace(
        r#"AXIS["geodetic latitude (Lat)",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["geodetic longitude (Lon)",west,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],"#,
        "",
    );
    assert!(!without_axes.contains("AXIS"), "{without_axes}");
    // Refused: longitude/latitude by its name or its IAU code (WKT1 and WKT2), and projections of it
    // by their own name, IAU code or axes, or by their geographic system's name or axes.
    let refused: Vec<(&str, String, [(f64, f64); 2], &str)> = vec![
        ("esri", MARS_OGRAPHIC_ESRI.to_string(), LONLAT, "planetographic longitudes may count west"),
        ("code", with_axes(&unnamed, r#"AUTHORITY["IAU_2015","49901"]"#), LONLAT, "planetographic longitudes"),
        ("wkt2", without_axes.clone(), LONLAT, "planetographic longitudes"),
        ("eqc_base", eqc_on(MARS_OGRAPHIC_ESRI, "Mars_Equirectangular"), metres, "x coordinates may count west"),
        ("eqc_name", eqc_on(&unnamed, "Mars_2015_Ographic_Equirectangular_clon0"), metres, "x coordinates may count west"),
        (
            "eqc_code",
            with_axes(&eqc_on(&unnamed, "Mars_Equirectangular"), r#"AUTHORITY["IAU_2015","49911"]"#),
            metres,
            "x coordinates may count west",
        ),
        (
            "eqc_west_base",
            eqc_on(&with_axes(&unnamed, r#"AXIS["Lon",WEST],AXIS["Lat",NORTH]"#), "Mars_Equirectangular"),
            metres,
            "x coordinates may count west",
        ),
        ("eqc_westing", with_axes(MARS_EQC, r#"AXIS["Westing",WEST],AXIS["Northing",NORTH]"#), metres, "west or south"),
        ("wkt2_eqc_westing", wkt2_eqc("Mars (2015) / Ographic", "west"), metres, "west or south"),
    ];
    for (name, prj, native, why) in &refused {
        let path = shapefile(dir.path(), name, *native, prj);
        let e = io::read_path(&path).unwrap_err().to_string();
        assert!(e.contains(ANOTHER_BODY) && e.contains(why) && e.contains("wrong place"), "{name}: {e}");
        let e = source::open_path(&path, None).err().unwrap().to_string();
        assert!(e.contains(ANOTHER_BODY) && e.contains(why), "{name}: {e}");
    }
    // Read, with the planetary notice: axes that say east, and planetocentric names and codes.
    let read: Vec<(&str, String, [(f64, f64); 2])> = vec![
        ("esri_east", with_axes(MARS_OGRAPHIC_ESRI, east_north), LONLAT),
        ("wkt2_east", MARS_WEST.replace(",west,", ",east,"), LONLAT),
        ("ocentric_code", with_axes(&unnamed, r#"AUTHORITY["IAU_2015","49900"]"#), LONLAT),
        ("eqc_easting", eqc_on(&with_axes(MARS_OGRAPHIC_ESRI, east_north), "Mars_Equirectangular"), on_ellipsoid),
        (
            "eqc_base_easting",
            with_axes(&eqc_on(MARS_OGRAPHIC_ESRI, "Mars_Equirectangular"), r#"AXIS["Easting",EAST],AXIS["Northing",NORTH]"#),
            on_ellipsoid,
        ),
        ("eqc_ocentric_code", with_axes(MARS_EQC, r#"AUTHORITY["IAU_2015","49910"]"#), metres),
        ("wkt2_eqc_easting", wkt2_eqc("Mars (2015) / Ographic", "east"), on_ellipsoid),
    ];
    for (name, prj, native) in &read {
        let path = shapefile(dir.path(), name, *native, prj);
        let (ds, report) = io::read_path(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        for (got, (lon, lat)) in points(&ds).into_iter().zip(LONLAT) {
            assert!((got.x - lon).abs() < 1e-6 && (got.y - lat).abs() < 1e-6, "{name}: {got:?}");
        }
        assert_eq!(planetary_notes(&report.messages).len(), 1, "{name}: {:?}", report.messages);
        let src = source::open_path(&path, None).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(planetary_notes(&src.info().notes).len(), 1, "{name}: {:?}", src.info().notes);
    }
    // The Earth's: its own IAU planetographic system (named so, without axes), and a name that
    // merely ends so, as before.
    let earth = [
        r#"GEOGCS["Earth (2015) / Ographic",DATUM["Earth (2015)",SPHEROID["Earth (2015)",6378136.6,298.257]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433],AUTHORITY["IAU_2015","39901"]]"#,
        r#"GEOGCS["Cartographic",DATUM["d",SPHEROID["Clarke_1866",6378206.4,294.9786982]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#,
    ];
    for (i, prj) in earth.iter().enumerate() {
        let path = shapefile(dir.path(), &format!("earth{i}"), [(5.0, 50.0), (6.0, 51.0)], prj);
        let (ds, report) = io::read_path(&path).unwrap_or_else(|e| panic!("{prj}: {e}"));
        assert_eq!(ds.features.len(), 2);
        assert!(planetary_notes(&report.messages).is_empty(), "{prj}: {:?}", report.messages);
        assert!(!report.messages.iter().any(|m| m.contains("planetographic")), "{prj}: {:?}", report.messages);
    }
    // An Earth system with longitudes counted west: read as longitude/latitude, saying so, as
    // before. A projection on such a system is read as before (only another body's is refused).
    let wgs84 = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;
    let west = with_axes(wgs84, r#"AXIS["Lon",WEST],AXIS["Lat",NORTH]"#);
    let path = shapefile(dir.path(), "earth_west", LONLAT, &west);
    let (ds, report) = io::read_path(&path).unwrap();
    assert_eq!(points(&ds), LONLAT.map(|(x, y)| Coord::new(x, y)).to_vec());
    let said = |notes: &[String]| notes.iter().filter(|m| m.contains("west or south") && m.contains("read as longitude/latitude")).count();
    assert_eq!(said(&report.messages), 1, "{:?}", report.messages);
    assert!(planetary_notes(&report.messages).is_empty(), "{:?}", report.messages);
    let src = source::open_path(&path, None).unwrap();
    assert_eq!(said(&src.info().notes), 1, "{:?}", src.info().notes);
    let f = 1.0 / 298.257223563;
    let wgs84_eqc = rustgis_geom::EquidistantCylindrical::new(6_378_137.0, f * (2.0 - f), 0.0, 0.0, 0.0, 0.0, 0.0).unwrap();
    let earth_metres = LONLAT.map(|(lon, lat)| {
        let c = wgs84_eqc.forward(Coord::new(lon, lat)).unwrap();
        (c.x, c.y)
    });
    let earth_eqc = eqc_on(&west, "World_Equidistant_Cylindrical");
    assert!(earth_eqc.contains("WGS_1984") && earth_eqc.contains("WEST"), "{earth_eqc}");
    let path = shapefile(dir.path(), "earth_eqc_west", earth_metres, &earth_eqc);
    let (ds, report) = io::read_path(&path).unwrap();
    for (got, (lon, lat)) in points(&ds).into_iter().zip(LONLAT) {
        assert!((got.x - lon).abs() < 1e-6 && (got.y - lat).abs() < 1e-6, "{got:?}");
    }
    assert!(planetary_notes(&report.messages).is_empty() && said(&report.messages) == 0, "{:?}", report.messages);
    // In a GeoPackage, on the desktop and in the browser: the same definition, under its own code.
    let mem =
        |path: &Path| source::gpkg_mem::GpkgMemSource::open_shared(source::Shared(std::sync::Arc::new(std::fs::read(path).unwrap())), None);
    let path = geopackage(dir.path(), "ographic", LONLAT, ("IAU_2015", 49901, MARS_OGRAPHIC_ESRI));
    let e = io::read_path(&path).unwrap_err().to_string();
    assert!(e.contains(ANOTHER_BODY) && e.contains("planetographic"), "{e}");
    let e = mem(&path).err().unwrap().to_string();
    assert!(e.contains(ANOTHER_BODY) && e.contains("planetographic"), "{e}");
    let east = with_axes(MARS_OGRAPHIC_ESRI, east_north);
    let path = geopackage(dir.path(), "ographic_east", LONLAT, ("IAU_2015", 49901, &east));
    let (ds, report) = io::read_path(&path).unwrap();
    assert_eq!(points(&ds), LONLAT.map(|(x, y)| Coord::new(x, y)).to_vec());
    assert_eq!(planetary_notes(&report.messages).len(), 1, "{:?}", report.messages);
    assert_eq!(planetary_notes(&mem(&path).unwrap().info().notes).len(), 1);
}

#[test]
fn earth_prj_files_say_nothing_of_other_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let mut prjs: Vec<(String, String)> = [
        4326, 4269, 4267, 4258, 4674, 7844, 4283, 27700, 2056, 31370, 28992, 3035, 2193, 32631, 32761, 26917, 3413, 3031, 3857, 5514, 3067,
        2154, 25832, 21781, 3006, 2263, 3395, 4087, 6933, 8857,
    ]
    .into_iter()
    .filter_map(|c| Some((format!("EPSG:{c}"), raster::wkt::to_wkt(Crs::from_epsg(c)?)?)))
    .collect();
    prjs.push(("NAD27 (Esri)".into(), NAD27_ESRI.into()));
    assert!(prjs.len() > 25, "{}", prjs.len());
    for (i, (code, prj)) in prjs.iter().enumerate() {
        let path = shapefile(dir.path(), &format!("earth{i}"), [(5.0, 50.0), (6.0, 51.0)], prj);
        let (ds, report) = io::read_path(&path).unwrap_or_else(|e| panic!("{code}: {e}"));
        assert!(planetary_notes(&report.messages).is_empty(), "{code}: {:?}", report.messages);
        assert!(!ds.crs.is_some_and(Crs::is_planetary), "{code}");
    }
    // NAD 27 still gets its own note.
    let path = shapefile(dir.path(), "nad27", [(-100.0, 40.0), (-101.0, 41.0)], NAD27_ESRI);
    assert_eq!(datum_notes(&io::read_path(&path).unwrap().1.messages).len(), 1);
}

fn geojson(crs: &str, coords: [(f64, f64); 2]) -> String {
    let features: Vec<String> = coords
        .iter()
        .map(|(x, y)| format!(r#"{{"type":"Feature","properties":{{}},"geometry":{{"type":"Point","coordinates":[{x},{y}]}}}}"#))
        .collect();
    format!(r#"{{"type":"FeatureCollection","crs":{{"type":"name","properties":{{"name":"{crs}"}}}},"features":[{}]}}"#, features.join(","))
}

#[test]
fn geojson_iau_codes_are_read_said_or_refused() {
    let dir = tempfile::tempdir().unwrap();
    // As GDAL writes them, and the other spellings of the same code.
    for name in [
        "urn:ogc:def:crs:IAU_2015::49900",
        "urn:ogc:def:crs:IAU:2015:49900",
        "IAU_2015:49900",
        "http://www.opengis.net/def/crs/IAU/2015/49900",
        " iau_2015:49902 ",
    ] {
        let text = geojson(name, LONLAT);
        let (ds, report) = io::read_bytes("mars.geojson", text.as_bytes()).unwrap();
        assert_eq!(points(&ds), LONLAT.map(|(x, y)| Coord::new(x, y)).to_vec(), "{name}");
        let said = planetary_notes(&report.messages);
        assert_eq!(said.len(), 1, "{name}: {:?}", report.messages);
        assert!(!report.messages.iter().any(|m| m.contains("only EPSG codes")), "{name}: {:?}", report.messages);
        // Known only by its code, the layer keeps no system: the notice says what an export writes.
        assert!(said[0].contains("as only its code says"), "{said:?}");
        assert!(said[0].contains("exported as if on the Earth"), "{said:?}");
        // In place too.
        let path = dir.path().join("mars.geojson");
        std::fs::write(&path, &text).unwrap();
        let src = source::open_path(&path, None).unwrap();
        assert_eq!(planetary_notes(&src.info().notes).len(), 1, "{name}: {:?}", src.info().notes);
    }
    // Projected metres, and planetographic longitudes (west on Mars), are refused.
    for (name, why) in [("urn:ogc:def:crs:IAU_2015::49910", "metres"), ("urn:ogc:def:crs:IAU_2015::49901", "west")] {
        let text = geojson(name, [(-7_930_954.0, 1_105_492.0), (4_591_220.0, 1_093_043.0)]);
        let e = io::read_bytes("mars.geojson", text.as_bytes()).unwrap_err().to_string();
        assert!(e.contains(ANOTHER_BODY) && e.contains(why) && e.contains(name), "{e}");
        let path = dir.path().join("refused.geojson");
        std::fs::write(&path, &text).unwrap();
        assert!(source::open_path(&path, None).err().unwrap().to_string().contains(ANOTHER_BODY));
    }
    // The Earth's own IAU code, and other codes, are what they were: not EPSG, read as lon/lat.
    for name in ["urn:ogc:def:crs:IAU_2015::39900", "urn:ogc:def:crs:ESRI::104900", "IAU_2015:abc", "IAU_2015:", "IAUS:49910"] {
        let (_, report) = io::read_bytes("x.geojson", geojson(name, LONLAT).as_bytes()).unwrap();
        assert!(planetary_notes(&report.messages).is_empty(), "{name}: {:?}", report.messages);
        assert!(report.messages.iter().any(|m| m.contains("only EPSG codes and CRS84")), "{name}: {:?}", report.messages);
    }
    // EPSG codes say nothing.
    let (_, report) = io::read_bytes("x.geojson", geojson("urn:ogc:def:crs:EPSG::4326", LONLAT).as_bytes()).unwrap();
    assert!(report.messages.is_empty(), "{:?}", report.messages);
}

/// A GeoPackage of `native` numbers whose spatial reference system is `srs` (organization, code,
/// definition).
fn geopackage(dir: &Path, name: &str, native: [(f64, f64); 2], srs: (&str, i64, &str)) -> PathBuf {
    let path = dir.join(format!("{name}.gpkg"));
    io::write_path(&craters(native), &path).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "INSERT INTO gpkg_spatial_ref_sys (srs_name, srs_id, organization, organization_coordsys_id, definition) VALUES ('m', 990001, ?1, ?2, ?3)",
        rusqlite::params![srs.0, srs.1, srs.2],
    )
    .unwrap();
    conn.execute("UPDATE gpkg_geometry_columns SET srs_id = 990001", []).unwrap();
    conn.execute("UPDATE gpkg_contents SET srs_id = 990001", []).unwrap();
    path
}

#[test]
fn geopackage_iau_systems_are_read_said_or_refused() {
    let dir = tempfile::tempdir().unwrap();
    // GDAL writes the IAU code as the organization's and the WKT as the definition.
    let wkt1 = raster::wkt::to_wkt(raster::wkt::crs_from_wkt(MARS_WKT2).unwrap()).unwrap();
    for (name, srs) in
        [("wkt2", ("IAU_2015", 49900, MARS_WKT2)), ("wkt1", ("IAU_2015", 49900, wkt1.as_str())), ("code", ("IAU_2015", 49900, "undefined"))]
    {
        let path = geopackage(dir.path(), name, LONLAT, srs);
        // The browser's reader, from the file's bytes, says the same.
        let mem =
            source::gpkg_mem::GpkgMemSource::open_shared(source::Shared(std::sync::Arc::new(std::fs::read(&path).unwrap())), None).unwrap();
        assert_eq!(planetary_notes(&mem.info().notes).len(), 1, "{name}: {:?}", mem.info().notes);
        let (ds, report) = io::read_path(&path).unwrap();
        assert_eq!(points(&ds), LONLAT.map(|(x, y)| Coord::new(x, y)).to_vec(), "{name}");
        assert_eq!(planetary_notes(&report.messages).len(), 1, "{name}: {:?}", report.messages);
        assert!(!report.messages.iter().any(|m| m.contains("no EPSG code")), "{name}: {:?}", report.messages);
        // A definition gives the layer its system, not WGS 84's name.
        assert_eq!(ds.crs.is_some_and(Crs::is_planetary), name != "code", "{name}");
    }
    // Mars metres: a projection its definition gives, and RustGIS reads, is unprojected on the
    // body's sphere, with the notice, on the desktop and in the browser, whatever the code says.
    let mem =
        |path: &Path| source::gpkg_mem::GpkgMemSource::open_shared(source::Shared(std::sync::Arc::new(std::fs::read(path).unwrap())), None);
    let r = 3_396_190.0f64;
    let metres = LONLAT.map(|(lon, lat)| (r * lon.to_radians(), r * lat.to_radians()));
    for (name, srs) in [("eqc", ("IAU_2015", 49910, MARS_EQC)), ("eqc_none", ("NONE", 1, MARS_EQC))] {
        let path = geopackage(dir.path(), name, metres, srs);
        let (ds, report) = io::read_path(&path).unwrap();
        for (got, (lon, lat)) in points(&ds).into_iter().zip(LONLAT) {
            assert!((got.x - lon).abs() < 1e-9 && (got.y - lat).abs() < 1e-9, "{name}: {got:?}");
        }
        assert_eq!(planetary_notes(&report.messages).len(), 1, "{name}: {:?}", report.messages);
        assert!(datum_notes(&report.messages).is_empty(), "{name}: {:?}", report.messages);
        assert!(ds.crs.is_some_and(|c| c.is_planetary() && !c.is_geographic()), "{name}");
        assert_eq!(planetary_notes(&mem(&path).unwrap().info().notes).len(), 1, "{name}");
    }
    // Refused, as they would be in the wrong place: a projection its definition gives that
    // RustGIS doesn't read, and one only its IAU code says.
    let bonne = [(1_000_000.0, 2_000_000.0), (-500_000.0, 10.0)];
    for (name, native, srs, why) in [
        ("bonne", bonne, ("NONE", 1, MARS_BONNE), "Bonne"),
        ("bonne_iau", bonne, ("IAU_2015", 49900, MARS_BONNE), "Bonne"),
        ("eqc_code", metres, ("IAU_2015", 49910, "undefined"), "metres"),
    ] {
        let path = geopackage(dir.path(), name, native, srs);
        let e = io::read_path(&path).unwrap_err().to_string();
        assert!(e.contains(ANOTHER_BODY) && e.contains(why) && e.contains("srs_id 990001"), "{name}: {e}");
        let e = mem(&path).err().unwrap().to_string();
        assert!(e.contains(ANOTHER_BODY) && e.contains(why), "{name}: {e}");
    }
    // An Earth system without an EPSG code is read by its definition, saying nothing of bodies.
    let path = geopackage(dir.path(), "earth", LONLAT, ("NONE", 1, NAD27_ESRI));
    let (ds, report) = io::read_path(&path).unwrap();
    assert_eq!(ds.features.len(), 2);
    assert!(planetary_notes(&report.messages).is_empty(), "{:?}", report.messages);
    assert!(planetary_notes(&mem(&path).unwrap().info().notes).is_empty());
}

/// A FlatGeobuf of `native` points whose header names its system by `org` and `code`, with `wkt`,
/// as GDAL writes an IAU one.
fn flatgeobuf(path: &Path, native: [(f64, f64); 2], org: Option<&str>, code: i32, wkt: Option<&str>) {
    use flatgeobuf::geozero::wkb::Wkb;
    use flatgeobuf::{FgbCrs, FgbWriter, FgbWriterOptions, GeometryType as G};
    let options = FgbWriterOptions { crs: FgbCrs { org, code, wkt, ..Default::default() }, ..Default::default() };
    let mut w = FgbWriter::create_with_options("craters", G::Point, options).unwrap();
    for (x, y) in native {
        w.add_feature_geom(Wkb(rustgis_data::wkb::encode(&Geometry::Point(Coord::new(x, y)))), |_| {}).unwrap();
    }
    let mut out = std::fs::File::create(path).unwrap();
    w.write(&mut out).unwrap();
}

#[test]
fn flatgeobuf_iau_systems_are_read_said_or_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mars.fgb");
    for (org, code, wkt) in [(Some("IAU_2015"), 49900, Some(MARS_WKT2)), (Some("IAU_2015"), 49900, None), (None, 0, Some(MARS_ESRI))] {
        flatgeobuf(&path, LONLAT, org, code, wkt);
        let (ds, report) = io::read_path(&path).unwrap();
        // In the spatial index's order.
        let mut got = points(&ds);
        got.sort_by(|a, b| a.x.total_cmp(&b.x));
        assert_eq!(got, [Coord::new(-133.8, 18.65), Coord::new(77.45, 18.44)]);
        assert_eq!(planetary_notes(&report.messages).len(), 1, "{org:?} {wkt:?}: {:?}", report.messages);
        assert!(!report.messages.iter().any(|m| m.contains("not EPSG") || m.contains("no EPSG code")), "{:?}", report.messages);
        assert_eq!(ds.crs.is_some_and(Crs::is_planetary), wkt.is_some());
    }
    // Mars metres: the reader converts EPSG codes only, so it would read them as degrees.
    let metres = [(-7_930_954.0, 1_105_492.0), (4_591_220.0, 1_093_043.0)];
    for (org, code, wkt) in [(Some("IAU_2015"), 49910, Some(MARS_EQC)), (Some("IAU_2015"), 49910, None), (None, 0, Some(MARS_EQC))] {
        flatgeobuf(&path, metres, org, code, wkt);
        let e = io::read_path(&path).unwrap_err().to_string();
        assert!(e.contains(ANOTHER_BODY) && e.contains("metres"), "{e}");
    }
    // Earth systems say what they said before.
    flatgeobuf(&path, LONLAT, Some("ESRI"), 102_100, None);
    let (_, report) = io::read_path(&path).unwrap();
    assert!(
        planetary_notes(&report.messages).is_empty() && report.messages.iter().any(|m| m.contains("not EPSG")),
        "{:?}",
        report.messages
    );
    flatgeobuf(&path, LONLAT, None, 0, Some(NAD27_ESRI));
    let (_, report) = io::read_path(&path).unwrap();
    assert!(
        planetary_notes(&report.messages).is_empty() && report.messages.iter().any(|m| m.contains("no EPSG code")),
        "{:?}",
        report.messages
    );
    flatgeobuf(&path, LONLAT, Some("EPSG"), 4326, None);
    assert!(io::read_path(&path).unwrap().1.messages.is_empty());
}

/// A GeoTIFF of 8 × 4 cells of 45°, from longitude `west` to `west` + 360, row by row numbered
/// from 0, with `keys` (and their double and ASCII parameters).
fn geotiff(west: f64, keys: &[u16], doubles: &[f64], ascii: &str) -> Vec<u8> {
    use tiff::encoder::{TiffEncoder, colortype};
    use tiff::tags::Tag;
    let px: Vec<u8> = (0..32).collect();
    let mut out = std::io::Cursor::new(Vec::new());
    let mut enc = TiffEncoder::new(&mut out).unwrap();
    let mut img = enc.new_image::<colortype::Gray8>(8, 4).unwrap();
    img.encoder().write_tag(Tag::Unknown(33550), &[45.0f64, 45.0, 0.0][..]).unwrap();
    img.encoder().write_tag(Tag::Unknown(33922), &[0.0f64, 0.0, 0.0, west, 90.0, 0.0][..]).unwrap();
    img.encoder().write_tag(Tag::GeoKeyDirectoryTag, keys).unwrap();
    if !doubles.is_empty() {
        img.encoder().write_tag(Tag::Unknown(34736), doubles).unwrap();
    }
    if !ascii.is_empty() {
        img.encoder().write_tag(Tag::Unknown(34737), ascii).unwrap();
    }
    img.write_data(&px).unwrap();
    out.into_inner()
}

/// GeoKeys of a geographic Mars file, as GDAL writes IAU_2015:49900.
fn mars_geographic_keys(citation: &str) -> Vec<u16> {
    vec![
        1,
        1,
        0,
        9,
        1024,
        0,
        1,
        2,
        1025,
        0,
        1,
        1,
        2048,
        0,
        1,
        32767,
        2049,
        34737,
        citation.len() as u16,
        0,
        2050,
        0,
        1,
        32767,
        2054,
        0,
        1,
        9102,
        2056,
        0,
        1,
        32767,
        2057,
        34736,
        1,
        0,
        2059,
        34736,
        1,
        1,
    ]
}

const MARS_CITATION: &str = "GCS Name = Mars (2015) - Sphere / Ocentric|Datum = Mars (2015) - Sphere|Ellipsoid = Mars (2015) - Sphere|Primem = Reference Meridian|AUnits = degree|";

#[test]
fn a_geographic_mars_geotiff_is_its_own_system_not_wgs_84_and_keeps_it_when_written() {
    let keys = mars_geographic_keys(MARS_CITATION);
    let wgs84: &[u16] = &[1, 1, 0, 3, 1024, 0, 1, 2, 1025, 0, 1, 1, 2048, 0, 1, 4326];
    for west in [-180.0, 0.0] {
        let mars = raster::read_bytes("mars.tif", &geotiff(west, &keys, &[3_396_190.0, 0.0], MARS_CITATION)).unwrap();
        let earth = raster::read_bytes("earth.tif", &geotiff(west, wgs84, &[], "")).unwrap();
        assert!(mars.crs.is_planetary() && mars.crs.is_geographic());
        assert_eq!(mars.crs.custom_def().unwrap().proj4, "+proj=longlat +a=3396190 +b=3396190 +no_defs");
        let said = planetary_notes(&mars.lost);
        assert_eq!(said.len(), 1, "{:?}", mars.lost);
        assert!(said[0].contains(&mars.crs.label()), "{said:?}");
        assert_eq!(earth.crs, Crs::LonLat);
        assert!(earth.lost.is_empty(), "{:?}", earth.lost);
        // The same place and cells as the Earth file read as WGS 84: what the coordinates mean is
        // unchanged (a 0–360 file wraps as before).
        let (m, e) = (mars.extent(), earth.extent());
        assert!((m.min_x - e.min_x).abs() < 1e-9 && (m.max_x - e.max_x).abs() < 1e-9, "{m:?} {e:?}");
        assert!((m.min_y - e.min_y).abs() < 1e-9 && (m.max_y - e.max_y).abs() < 1e-9, "{m:?} {e:?}");
        for (lon, lat) in [(-133.8, 18.65), (77.45, 18.44), (179.9, -89.0), (-179.9, 89.0), (0.1, 0.1)] {
            let at = Coord::new(lon, lat);
            assert_eq!(mars.pixel_at(at), earth.pixel_at(at), "{west}: {at:?}");
            assert!(mars.pixel_at(at).is_some(), "{west}: {at:?}");
        }
        // Written as a GeoTIFF, it is Mars still (the keys it came in: below, on a body of its own).
        let again = raster::read_bytes("again.tif", &raster::geotiff::write(&mars, 0).unwrap()).unwrap();
        assert_eq!(again.crs, mars.crs);
        assert!(again.crs.is_planetary() && again.crs.is_geographic());
        assert_eq!(planetary_notes(&again.lost).len(), 1);
        // A cloud-optimised one, drawn in place, says it as well.
        let cog = raster::cogwrite::write(&mars, 0).unwrap();
        let len = cog.len() as u64;
        let raster::cog::Parse::Done(layout) = raster::cog::layout(&raster::cog::Sparse::default().with(0, cog).with_len(len)).unwrap()
        else {
            panic!("not read in place")
        };
        assert_eq!(layout.crs, mars.crs);
        assert_eq!(planetary_notes(&layout.lost).len(), 1, "{:?}", layout.lost);
        let earth_cog = raster::cogwrite::write(&earth, 0).unwrap();
        let len = earth_cog.len() as u64;
        let raster::cog::Parse::Done(layout) =
            raster::cog::layout(&raster::cog::Sparse::default().with(0, earth_cog).with_len(len)).unwrap()
        else {
            panic!("not read in place")
        };
        assert!(layout.lost.is_empty(), "{:?}", layout.lost);
    }
    // Written back with the keys, axes and citation it came in, as the written file holds them. (On
    // a body no other test reads, Callisto: the run's table keeps the keys a system first came in.)
    let citation = "GCS Name = Callisto (2015) - Sphere / Ocentric|Datum = Callisto (2015) - Sphere|Ellipsoid = Callisto (2015) - Sphere|Primem = Reference Meridian|AUnits = degree|";
    let keys = mars_geographic_keys(citation);
    let callisto = raster::read_bytes("callisto.tif", &geotiff(-180.0, &keys, &[2_410_300.0, 0.0], citation)).unwrap();
    assert_eq!(callisto.crs.custom_def().unwrap().proj4, "+proj=longlat +a=2410300 +b=2410300 +no_defs");
    let written = raster::geotiff::write(&callisto, 0).unwrap();
    let mut dec = tiff::decoder::Decoder::new(std::io::Cursor::new(&written)).unwrap();
    let tag = |dec: &mut tiff::decoder::Decoder<_>, t: u16| dec.find_tag(tiff::tags::Tag::Unknown(t)).unwrap().unwrap();
    assert_eq!(tag(&mut dec, 34735).into_u16_vec().unwrap(), keys);
    assert_eq!(tag(&mut dec, 34736).into_f64_vec().unwrap(), [2_410_300.0, 0.0]);
    assert_eq!(tag(&mut dec, 34737).into_string().unwrap().trim_end_matches('\0'), citation);
    let again = raster::read_bytes("again.tif", &written).unwrap();
    assert_eq!((again.crs, again.crs.label()), (callisto.crs, "Callisto (2015) - Sphere / Ocentric".to_string()));
    // Named by its citation, else by its axis (on a body no other test reads: the run's table
    // names a system as it first knew it).
    let named = raster::read_bytes(
        "io.tif",
        &geotiff(-180.0, &mars_geographic_keys("GCS Name = Io (2015) - Sphere|"), &[1_821_490.0], "GCS Name = Io (2015) - Sphere|"),
    )
    .unwrap();
    assert_eq!(named.crs.label(), "Io (2015) - Sphere");
    let bare = raster::read_bytes("bare.tif", &geotiff(-180.0, &mars_geographic_keys(""), &[2_631_200.0, 0.0], "")).unwrap();
    assert!(bare.crs.is_planetary() && bare.crs.label().contains("2631200"), "{}", bare.crs.label());
}

/// A geographic Mars GeoTIFF whose cells would be in the wrong place read as degrees counted east
/// is refused: a planetographic citation (GeoTIFF has no key for longitudes counted west) or
/// angles in another unit; degrees, by code or by size, and a planetocentric citation, are read.
#[test]
fn a_planetographic_or_non_degree_mars_geotiff_is_refused() {
    let read = |keys: &[u16], doubles: &[f64], citation: &str| raster::read_bytes("mars.tif", &geotiff(-180.0, keys, doubles, citation));
    let ographic =
        "GCS Name = Mars (2015) / Ographic|Datum = Mars (2015)|Ellipsoid = Mars (2015)|Primem = Reference Meridian|AUnits = degree|";
    let e = read(&mars_geographic_keys(ographic), &[3_396_190.0, 169.894_447_223_612], ographic).unwrap_err().to_string();
    assert!(e.contains(ANOTHER_BODY) && e.contains("planetographic") && e.contains("Mars (2015) / Ographic"), "{e}");
    // GeogAngularUnits, by code: radians and grads refused, the two degree codes read.
    let with_unit = |unit: u16| {
        let mut keys = mars_geographic_keys(MARS_CITATION);
        let at = keys.iter().position(|k| *k == 2054).unwrap();
        keys[at + 3] = unit;
        keys
    };
    for (unit, ok) in [(9101, false), (9105, false), (9102, true), (9122, true)] {
        match read(&with_unit(unit), &[3_396_190.0, 0.0], MARS_CITATION) {
            Ok(r) => {
                assert!(ok && r.crs.is_planetary(), "{unit}");
                assert_eq!(planetary_notes(&r.lost).len(), 1, "{unit}: {:?}", r.lost);
            }
            Err(e) => assert!(!ok && e.to_string().contains(&format!("GeogAngularUnits {unit}, not degrees")), "{unit}: {e}"),
        }
    }
    // A user-defined unit, by its size in radians (GeogAngularUnitSize).
    let sized = |size: Option<f64>| {
        let mut keys = with_unit(32767);
        let mut doubles = vec![3_396_190.0, 0.0];
        if let Some(size) = size {
            let at = keys.iter().position(|k| *k == 2054).unwrap() + 4;
            keys.splice(at..at, [2055, 34736, 1, 2]);
            keys[3] += 1;
            doubles.push(size);
        }
        read(&keys, &doubles, MARS_CITATION)
    };
    assert!(sized(Some(1f64.to_radians())).unwrap().crs.is_planetary());
    assert!(sized(Some(1.0)).unwrap_err().to_string().contains("units of 1 radians, not degrees"));
    assert!(sized(None).unwrap_err().to_string().contains("a user-defined unit of no size"));
    // No GeogAngularUnits at all: degrees. A planetocentric citation, and none (named by its axis,
    // which says "Geographic"; on a body no other test reads, Europa, as the run's table names a
    // system as it first knew it), are read.
    let mut keys = mars_geographic_keys(MARS_CITATION);
    let at = keys.iter().position(|k| *k == 2054).unwrap();
    keys.drain(at..at + 4);
    keys[3] -= 1;
    assert!(read(&keys, &[3_396_190.0, 0.0], MARS_CITATION).unwrap().crs.is_planetary());
    let bare = read(&mars_geographic_keys(""), &[1_560_800.0, 0.0], "").unwrap();
    assert!(bare.crs.is_planetary() && bare.crs.label().contains("Geographic"), "{}", bare.crs.label());
    // By a word of its own, not one that merely ends so.
    for (named, refused) in [
        ("GCS Name = GCS_Mars_2000_Geographic|", false),
        ("GCS Name = Mars cartographic grid|", false),
        ("GCS Name = Mars photographic mosaic|", false),
        ("GCS Name = GCS_Mars_2000_Ographic|", true),
        ("GCS Name = Mars planetographic|", true),
    ] {
        let r = read(&mars_geographic_keys(named), &[3_396_190.0, 0.0], named);
        assert_eq!(r.as_ref().err().is_some_and(|e| e.to_string().contains("planetographic")), refused, "{named}");
        assert_eq!(r.is_ok(), !refused, "{named}");
    }
    // An Earth file's units are what they were (GeogAngularUnits isn't read for the Earth).
    let earth = read(&with_unit(9101), &[6_378_137.0, 298.257_223_563], "").unwrap();
    assert!(!earth.crs.is_planetary());
}

#[test]
fn geotiff_axes_of_the_earth_stay_wgs_84_and_feet_are_converted() {
    // A geographic file with its own Earth ellipsoid (Clarke 1866) is read as it was.
    let clarke = raster::read_bytes("clarke.tif", &geotiff(-180.0, &mars_geographic_keys(""), &[6_378_206.4, 294.978_698_2], "")).unwrap();
    assert_eq!(clarke.crs, Crs::LonLat);
    assert!(clarke.lost.is_empty(), "{:?}", clarke.lost);
    // The Earth's semi-major axis in feet (GeogLinearUnits 9002) is still the Earth's.
    let mut keys = mars_geographic_keys("");
    keys[3] += 1;
    keys.extend([2052, 0, 1, 9002]);
    let feet = raster::read_bytes("feet.tif", &geotiff(-180.0, &keys, &[20_925_646.3, 294.978_698_2], "")).unwrap();
    assert_eq!(feet.crs, Crs::LonLat);
    // A projection on it, in feet: the Earth's axes in metres, not another body's.
    let projected: Vec<u16> = vec![
        1, 1, 0, 9, 1024, 0, 1, 1, 1025, 0, 1, 1, 2048, 0, 1, 32767, 2052, 0, 1, 9002, 2057, 34736, 1, 0, 2059, 34736, 1, 1, 3072, 0, 1,
        32767, 3075, 0, 1, 17, 3076, 0, 1, 9001,
    ];
    let r = raster::read_bytes("eqc_ft.tif", &geotiff(-1_000_000.0, &projected, &[20_925_646.3, 294.978_698_2], "")).unwrap();
    assert!(!r.crs.is_planetary() && r.lost.is_empty(), "{:?}", r.lost);
    let def = r.crs.custom_def().unwrap().proj4.clone();
    let a: f64 = def.split_whitespace().find_map(|t| t.strip_prefix("+a=")).unwrap().parse().unwrap();
    assert!((a - 6_378_137.0).abs() < 0.1, "{def}");
    // Some writers declare feet for axes they give in metres: the Earth's still.
    let r = raster::read_bytes("eqc_m.tif", &geotiff(-1_000_000.0, &projected, &[6_378_206.4, 294.978_698_2], "")).unwrap();
    assert!(!r.crs.is_planetary() && r.crs.custom_def().unwrap().proj4.contains("+a=6378206.4 "));
}

#[test]
fn a_mars_projection_in_a_geotiff_is_said_or_refused() {
    // Equirectangular (coordinate transformation 17) on Mars's sphere: read, and said.
    let eqc: Vec<u16> = vec![
        1, 1, 0, 8, 1024, 0, 1, 1, 1025, 0, 1, 1, 2048, 0, 1, 32767, 2057, 34736, 1, 0, 3072, 0, 1, 32767, 3074, 0, 1, 32767, 3075, 0, 1,
        17, 3076, 0, 1, 9001,
    ];
    let r = raster::read_bytes("eqc.tif", &geotiff(-1_000_000.0, &eqc, &[3_396_190.0], "")).unwrap();
    assert!(r.crs.is_planetary() && !r.crs.is_geographic());
    assert_eq!(planetary_notes(&r.lost).len(), 1, "{:?}", r.lost);
    // A projection of planetographic longitude/latitude, by a citation (GeoTIFF has no key for its
    // axes, which PROJ makes westings): refused; a planetocentric one is read.
    let cited_on = |a: f64, key: u16, citation: &str| {
        let mut keys = eqc.clone();
        let at = keys.chunks(4).position(|k| k[0] > key).unwrap() * 4;
        keys.splice(at..at, [key, 34737, citation.len() as u16, 0]);
        keys[3] += 1;
        raster::read_bytes("cited.tif", &geotiff(-1_000_000.0, &keys, &[a], citation))
    };
    let cited = |key: u16, citation: &str| cited_on(3_396_190.0, key, citation);
    for (key, citation) in [
        (2049, "GCS Name = Mars (2015) / Ographic|Datum = Mars (2015)|"),
        (3073, "Mars (2015) / Ographic / Equirectangular, clon = 0|"),
        (1026, "Mars_2015_Ographic_Equirectangular_clon0|"),
    ] {
        let e = cited(key, citation).unwrap_err().to_string();
        assert!(e.contains(ANOTHER_BODY) && e.contains("x coordinates may count west"), "{key}: {e}");
    }
    for (key, citation) in [(2049, "GCS Name = Mars (2015) - Sphere / Ocentric|"), (3073, "Mars cartographic grid|")] {
        let r = cited(key, citation).unwrap_or_else(|e| panic!("{key}: {e}"));
        assert!(r.crs.is_planetary() && planetary_notes(&r.lost).len() == 1, "{key}: {:?}", r.lost);
    }
    // On the Earth, such a citation changes nothing.
    let earth = cited_on(6_378_137.0, 3073, "Ographic test grid|").unwrap();
    assert!(!earth.crs.is_planetary() && earth.lost.is_empty(), "{:?}", earth.lost);
    // A transformation RustGIS doesn't read (26, the New Zealand Map Grid's): refused on Mars, even
    // where the numbers look like degrees, as they are metres.
    let mut unread = eqc.clone();
    let at = unread.chunks(4).position(|k| k[0] == 3075).unwrap() * 4 + 3;
    unread[at] = 26;
    let e = raster::read_bytes("unread.tif", &geotiff(-180.0, &unread, &[3_396_190.0], "")).unwrap_err().to_string();
    assert!(e.contains(ANOTHER_BODY) && e.contains("transformation 26"), "{e}");
    // On the Earth it is read as longitude/latitude where the numbers fit, as before.
    let earth = raster::read_bytes("earth.tif", &geotiff(-180.0, &unread, &[6_378_137.0], "")).unwrap();
    assert_eq!(earth.crs, Crs::LonLat);
    assert!(planetary_notes(&earth.lost).is_empty());
}

#[test]
fn an_ascii_grid_with_a_mars_prj_is_said_or_refused() {
    let dir = tempfile::tempdir().unwrap();
    let grid = "ncols 2\nnrows 2\nxllcorner 10\nyllcorner 10\ncellsize 1\n1 2\n3 4\n";
    for (name, prj) in [("mars", MARS_ESRI), ("bonne", MARS_BONNE), ("earth", NAD27_ESRI)] {
        std::fs::write(dir.path().join(format!("{name}.asc")), grid).unwrap();
        std::fs::write(dir.path().join(format!("{name}.prj")), prj).unwrap();
    }
    let mars = raster::read_path(&dir.path().join("mars.asc")).unwrap();
    assert!(mars.crs.is_planetary());
    assert_eq!(planetary_notes(&mars.lost).len(), 1, "{:?}", mars.lost);
    let e = raster::read_path(&dir.path().join("bonne.asc")).unwrap_err().to_string();
    assert!(e.contains(ANOTHER_BODY) && e.contains("Bonne"), "{e}");
    let earth = raster::read_path(&dir.path().join("earth.asc")).unwrap();
    assert!(planetary_notes(&earth.lost).is_empty(), "{:?}", earth.lost);
    assert!(!earth.lost.iter().any(|l| l.contains(".prj")), "{:?}", earth.lost);
}

/// An Earth `.prj` next to an ASCII grid that RustGIS can't read leaves the grid in
/// longitude/latitude, and says so, as a Shapefile's does; one it reads, or none, says nothing.
#[test]
fn an_ascii_grid_whose_earth_prj_is_not_read_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let grid = "ncols 2\nnrows 2\nxllcorner 10\nyllcorner 10\ncellsize 1\n1 2\n3 4\n";
    const GEOCENTRIC: &str =
        r#"GEOCCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["metre",1]]"#;
    let write = |name: &str, prj: Option<&str>| {
        let path = dir.path().join(format!("{name}.asc"));
        std::fs::write(&path, grid).unwrap();
        if let Some(prj) = prj {
            std::fs::write(dir.path().join(format!("{name}.prj")), prj).unwrap();
        }
        path
    };
    let said =
        |r: &raster::Raster| r.lost.iter().filter(|l| l.contains("the .prj file") && l.contains("read as longitude/latitude")).count();
    for (name, prj, why) in [("geocentric", GEOCENTRIC, "GEOCCS"), ("junk", "not a coordinate system", "")] {
        let r = raster::read_path(&write(name, Some(prj))).unwrap();
        assert_eq!(r.crs, Crs::LonLat, "{name}");
        assert_eq!(said(&r), 1, "{name}: {:?}", r.lost);
        assert!(r.lost.iter().any(|l| l.contains(why)), "{name}: {:?}", r.lost);
        assert!(planetary_notes(&r.lost).is_empty(), "{name}: {:?}", r.lost);
    }
    // One that isn't text (not UTF-8) is said too.
    let binary = write("binary", None);
    std::fs::write(dir.path().join("binary.prj"), [0xff, 0xfe, 0x00, 0x47]).unwrap();
    let r = raster::read_path(&binary).unwrap();
    assert_eq!(said(&r), 1, "{:?}", r.lost);
    assert!(r.lost.iter().any(|l| l.contains("can't be read as text")), "{:?}", r.lost);
    // Read again for a sidecar's no-data that only another band gives: still said, once.
    let again = write("again", Some(GEOCENTRIC));
    std::fs::write(
        dir.path().join("again.asc.aux.xml"),
        r#"<PAMDataset><PAMRasterBand band="2"><NoDataValue>3</NoDataValue></PAMRasterBand></PAMDataset>"#,
    )
    .unwrap();
    assert_eq!(said(&raster::read_path(&again).unwrap()), 1);
    // A .prj it reads, and none at all, say nothing of it.
    for (name, prj) in [("wgs84", Some(raster::wkt::to_wkt(Crs::LonLat).unwrap())), ("none", None)] {
        let r = raster::read_path(&write(name, prj.as_deref())).unwrap();
        assert_eq!(r.crs, Crs::LonLat, "{name}");
        assert_eq!(said(&r), 0, "{name}: {:?}", r.lost);
    }
}

#[test]
fn exports_say_what_they_write_instead_of_another_body_s_system() {
    let mars = raster::wkt::crs_from_wkt(MARS_ESRI).unwrap();
    let eqc = raster::wkt::crs_from_wkt(MARS_EQC).unwrap();
    let nad27 = raster::wkt::crs_from_wkt(NAD27_ESRI).unwrap();
    let mut layer = craters(LONLAT);
    layer.crs = Some(mars);
    let mut moon = craters(LONLAT);
    moon.name = "moon".into();
    moon.crs = Some(raster::wkt::crs_from_wkt(&MARS_ESRI.replace("Mars", "Moon").replace("3396190.0", "1737400.0")).unwrap());
    let mut earth = craters(LONLAT);
    earth.name = "parcels".into();
    earth.crs = Some(nad27);
    let extent = |ds: &Dataset| ds.extent();

    // As WGS 84 longitude/latitude (GeoJSON, a GeoPackage by default): one notice, not the
    // Earth's datum note.
    let notes = rustgis_data::export_notes([(&layer, extent(&layer))], "out.geojson", Crs::LonLat);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("craters") && notes[0].contains("written as WGS 84 (EPSG:4326)") && notes[0].contains(&mars.label()),
        "{notes:?}"
    );
    let notes = rustgis_data::export_notes([&layer, &moon, &earth].map(|ds| (ds, extent(ds))), "map.json", Crs::LonLat);
    assert_eq!(notes.len(), 2, "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("2 layers (craters, moon) are on other bodies")), "{notes:?}");
    assert!(notes.iter().any(|n| n.contains("parcels") && n.contains("few hundred metres")), "{notes:?}");
    // In its own system (a Shapefile by default): the .prj loses the body's name.
    assert_eq!(io::default_export_crs(&layer, "shp"), mars);
    let notes = rustgis_data::export_notes([(&layer, extent(&layer))], "out.shp", mars);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("only by the ellipsoid's axes"), "{notes:?}");
    let prj = raster::wkt::to_wkt(mars).unwrap();
    assert!(!prj.contains("Mars") && prj.contains("3396190"), "{prj}");
    // An Earth layer says nothing of bodies.
    let notes = rustgis_data::export_notes([(&earth, extent(&earth))], "out.geojson", Crs::LonLat);
    assert!(!notes.iter().any(|n| n.contains("bod")), "{notes:?}");

    // Projected onto the Earth (or another body), the numbers would be wrong: refused.
    for to in
        [Crs::from_epsg(32631).unwrap(), Crs::WebMercator, raster::wkt::crs_from_wkt(&MARS_EQC.replace("3396190.0", "1737400.0")).unwrap()]
    {
        let e = rustgis_data::export_error(&layer, to).unwrap_or_else(|| panic!("{}", to.label()));
        assert!(e.contains(ANOTHER_BODY) && e.contains(&to.label()), "{e}");
    }
    // As longitude/latitude, in its own system or a projection on its own sphere: written.
    for to in [Crs::LonLat, Crs::from_epsg(4269).unwrap(), mars, eqc] {
        assert!(rustgis_data::export_error(&layer, to).is_none(), "{}", to.label());
    }
    assert!(rustgis_data::export_error(&earth, Crs::from_epsg(32631).unwrap()).is_none());
    assert!(rustgis_data::export_error(&craters(LONLAT), eqc).is_none());
    // Written to a Mars projection, the numbers are Mars metres.
    let projected = io::project(&layer, eqc).unwrap();
    let x = points(&projected)[0].x;
    assert!((x - 3_396_190.0 * (-133.8f64).to_radians()).abs() < 1e-3, "{x}");
}

#[test]
fn an_epsg_code_that_is_not_converted_leaves_another_body_s_definition_to_say() {
    // An EPSG organization with a code RustGIS doesn't convert would otherwise be read as
    // longitude/latitude with only "unknown coordinate system": its definition still says the body.
    const UNKNOWN: i32 = 104_971;
    assert!(Crs::from_epsg(UNKNOWN as u32).is_none());
    let dir = tempfile::tempdir().unwrap();
    let metres = [(-7_930_954.0, 1_105_492.0), (4_591_220.0, 1_093_043.0)];
    let mem =
        |path: &Path| source::gpkg_mem::GpkgMemSource::open_shared(source::Shared(std::sync::Arc::new(std::fs::read(path).unwrap())), None);
    // GeoPackage, desktop and browser: the definition is read (Mars metres unprojected on its
    // sphere, Mars longitude/latitude kept), with the notice; one it doesn't read is refused.
    let r = 3_396_190.0f64;
    let eqc = LONLAT.map(|(lon, lat)| (r * lon.to_radians(), r * lat.to_radians()));
    for (name, native, wkt) in [("epsg_eqc", eqc, MARS_EQC), ("epsg_mars", LONLAT, MARS_WKT2)] {
        let path = geopackage(dir.path(), name, native, ("EPSG", i64::from(UNKNOWN), wkt));
        let (ds, report) = io::read_path(&path).unwrap();
        for (got, (lon, lat)) in points(&ds).into_iter().zip(LONLAT) {
            assert!((got.x - lon).abs() < 1e-9 && (got.y - lat).abs() < 1e-9, "{name}: {got:?}");
        }
        assert_eq!(planetary_notes(&report.messages).len(), 1, "{name}: {:?}", report.messages);
        assert!(!report.messages.iter().any(|m| m.contains("unknown coordinate system")), "{name}: {:?}", report.messages);
        assert!(ds.crs.is_some_and(Crs::is_planetary), "{name}");
        assert_eq!(planetary_notes(&mem(&path).unwrap().info().notes).len(), 1, "{name}");
    }
    let path = geopackage(dir.path(), "epsg_bonne", metres, ("EPSG", i64::from(UNKNOWN), MARS_BONNE));
    let e = io::read_path(&path).unwrap_err().to_string();
    assert!(e.contains(ANOTHER_BODY) && e.contains("Bonne"), "{e}");
    let e = mem(&path).err().unwrap().to_string();
    assert!(e.contains(ANOTHER_BODY) && e.contains("Bonne"), "{e}");
    // An Earth definition under the same code says nothing of bodies.
    let path = geopackage(dir.path(), "epsg_earth", LONLAT, ("EPSG", i64::from(UNKNOWN), NAD27_ESRI));
    let (ds, report) = io::read_path(&path).unwrap();
    assert_eq!(ds.features.len(), 2);
    assert!(planetary_notes(&report.messages).is_empty(), "{:?}", report.messages);
    assert!(planetary_notes(&mem(&path).unwrap().info().notes).is_empty());
    // A code that converts is read by it, as always.
    let path = geopackage(dir.path(), "epsg_known", LONLAT, ("EPSG", 4326, NAD27_ESRI));
    assert!(planetary_notes(&io::read_path(&path).unwrap().1.messages).is_empty());

    // FlatGeobuf: the same.
    let path = dir.path().join("epsg.fgb");
    flatgeobuf(&path, metres, Some("EPSG"), UNKNOWN, Some(MARS_EQC));
    let e = io::read_path(&path).unwrap_err().to_string();
    assert!(e.contains(ANOTHER_BODY) && e.contains("metres"), "{e}");
    flatgeobuf(&path, LONLAT, Some("EPSG"), UNKNOWN, Some(MARS_WKT2));
    let (ds, report) = io::read_path(&path).unwrap();
    assert_eq!(planetary_notes(&report.messages).len(), 1, "{:?}", report.messages);
    assert!(ds.crs.is_some_and(Crs::is_planetary));
    flatgeobuf(&path, LONLAT, None, UNKNOWN, Some(MARS_ESRI));
    assert_eq!(planetary_notes(&io::read_path(&path).unwrap().1.messages).len(), 1);
    flatgeobuf(&path, LONLAT, Some("EPSG"), UNKNOWN, Some(NAD27_ESRI));
    let (_, report) = io::read_path(&path).unwrap();
    assert!(planetary_notes(&report.messages).is_empty(), "{:?}", report.messages);
    assert!(report.messages.iter().any(|m| m.contains(&format!("EPSG:{UNKNOWN}"))), "{:?}", report.messages);
    flatgeobuf(&path, LONLAT, Some("EPSG"), 4326, Some(NAD27_ESRI));
    assert!(planetary_notes(&io::read_path(&path).unwrap().1.messages).is_empty());
}

#[test]
fn geotiff_axes_in_a_unit_that_is_not_sized_are_not_taken_for_metres() {
    // Mars equirectangular with its radius in kilometres, declared by a GeogLinearUnits code that
    // isn't read (9030, the nautical mile) and no GeogLinearUnitSize: taken for metres, the
    // sphere would be 3.4 km across and the cells in the wrong place.
    let keys: Vec<u16> = vec![
        1, 1, 0, 9, 1024, 0, 1, 1, 1025, 0, 1, 1, 2048, 0, 1, 32767, 2052, 0, 1, 9030, 2057, 34736, 1, 0, 3072, 0, 1, 32767, 3074, 0, 1,
        32767, 3075, 0, 1, 17, 3076, 0, 1, 9001,
    ];
    let e = raster::read_bytes("km.tif", &geotiff(-1_000_000.0, &keys, &[3_396.19], "")).unwrap_err().to_string();
    assert!(e.contains("GeogLinearUnits 9030"), "{e}");
    // A unit it sizes (GeogLinearUnitSize, metres per unit) is converted.
    let mut sized = keys.clone();
    sized[3] += 1;
    sized.extend([2053, 34736, 1, 1]);
    let r = raster::read_bytes("sized.tif", &geotiff(-1_000_000.0, &sized, &[6_792_380.0, 0.5], "")).unwrap();
    assert!(r.crs.is_planetary() && r.crs.custom_def().unwrap().proj4.contains("+a=3396190 "), "{}", r.crs.custom_def().unwrap().proj4);
    // Axes that are the Earth's in metres whatever unit is declared: read as before.
    let r = raster::read_bytes("earth.tif", &geotiff(-1_000_000.0, &keys, &[6_378_137.0], "")).unwrap();
    assert!(!r.crs.is_planetary() && r.lost.is_empty(), "{:?}", r.lost);
}
