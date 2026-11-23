use super::*;
use crate::raster::Crs;
use crate::raster::wkt::tests::{PlacementCase, metres_apart, placement_cases};
use crate::{Dataset, Field, FieldType, Geometry, Value, wkb};
use rustgis_geom::{Coord, lonlat_to_merc};
use std::path::PathBuf;
use std::sync::Arc;

/// Builds a minimal synthetic X.509 v3 CA certificate in DER and PEM for offline CA-loader tests.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn synthetic_ca_cert() -> (Vec<u8>, String) {
    use base64::Engine;
    fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        if body.len() < 128 {
            out.push(body.len() as u8);
        } else {
            out.push(0x81);
            out.push(body.len() as u8);
        }
        out.extend_from_slice(body);
        out
    }
    let alg = tlv(0x30, &[0x06, 0x03, 0x2b, 0x65, 0x70]); // Ed25519 OID 1.3.101.112
    let cn = tlv(0x30, &[0x06, 0x03, 0x55, 0x04, 0x03, 0x0c, 0x02, b'c', b'a']);
    let name = tlv(0x30, &tlv(0x31, &cn));
    let mut times = Vec::new();
    times.extend(tlv(0x17, b"250101000000Z"));
    times.extend(tlv(0x17, b"350101000000Z"));
    let validity = tlv(0x30, &times);
    let mut pk_bits = vec![0x00];
    pk_bits.extend_from_slice(&[0x11; 32]);
    let mut spki_body = alg.clone();
    spki_body.extend(tlv(0x03, &pk_bits));
    let spki = tlv(0x30, &spki_body);
    // BasicConstraints CA=TRUE (OID 2.5.29.19)
    let bc_val = tlv(0x04, &tlv(0x30, &[0x01, 0x01, 0xff]));
    let mut bc_ext = vec![0x06, 0x03, 0x55, 0x1d, 0x13, 0x01, 0x01, 0xff];
    bc_ext.extend(bc_val);
    let exts = tlv(0xa3, &tlv(0x30, &tlv(0x30, &bc_ext)));
    let mut tbs_body = vec![0xa0, 0x03, 0x02, 0x01, 0x02, 0x02, 0x01, 0x01];
    tbs_body.extend(&alg);
    tbs_body.extend(&name);
    tbs_body.extend(&validity);
    tbs_body.extend(&name);
    tbs_body.extend(&spki);
    tbs_body.extend(&exts);
    let tbs = tlv(0x30, &tbs_body);
    let mut sig_bits = vec![0x00];
    sig_bits.extend_from_slice(&[0x22; 64]);
    let mut cert_body = tbs;
    cert_body.extend(&alg);
    cert_body.extend(tlv(0x03, &sig_bits));
    let der = tlv(0x30, &cert_body);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&der);
    let pem = format!("-----BEGIN CERTIFICATE-----\n{b64}\n-----END CERTIFICATE-----\n");
    (der, pem)
}

#[test]
fn explicit_method_1028_warns_for_known_geopackage_authorities() {
    let wkt = r#"PROJCRS["World EQC",BASEGEOGCRS["WGS 84",DATUM["WGS_1984",ELLIPSOID["WGS 84",6378137,298.257223563]],PRIMEM["Greenwich",0]],CONVERSION["EQC",METHOD["Equidistant Cylindrical",ID["EPSG",1028]]],CS[Cartesian,2],AXIS["Easting",east],AXIS["Northing",north],LENGTHUNIT["metre",1]]"#;
    let sphere = wkt.replace("298.257223563", "0");
    let wkt1 = crate::raster::wkt::to_wkt(Crs::from_epsg(4087).unwrap()).unwrap();
    for (definition, code, warned) in [(wkt, 4087, true), (&sphere, 4087, false), (&wkt1, 4087, false), (wkt, 3857, false)] {
        let mut notes = Vec::new();
        let row =
            GpkgSrs { organization: Some("EPSG".into()), code: Some(code), definition: Some(definition.into()), definition_12_063: None };
        let read = gpkg_reproject(100001, Some(row), &mut notes).unwrap();
        assert_eq!(notes.iter().filter(|n| n.contains("EPSG method 1028")).count(), usize::from(warned), "{definition}: {notes:?}");
        if warned {
            assert!(
                notes[0].contains("srs_id 100001")
                    && notes[0].contains("PROJ 9.8")
                    && notes[0].contains("using EPSG:4087 parameters in preference to the WKT definition"),
                "{notes:?}"
            );
            let point = Geometry::Point(Coord::new(0.0, 4_984_944.377_977_743));
            let Geometry::Point(at) = read.to_lonlat_geometry(point).unwrap() else { panic!("not a point") };
            assert!(at.x.abs() < 1e-9 && (at.y - 45.0).abs() < 1e-9, "{at:?}");
        }
    }
    // The WKT1 names the method when the WKT2 can't be parsed: still said. A WKT2 that is read
    // counts, also when the WKT1 says otherwise (here, on a sphere: nothing to say).
    for (wkt2, warned) in
        [("GEOGCRS[", true), (r#"ENGCRS["site",EDATUM["site"]]"#, true), (&wkt.replace(r#",ID["EPSG",1028]"#, ""), true), (&sphere, false)]
    {
        assert_ne!(wkt2, wkt);
        let mut notes = Vec::new();
        let row = GpkgSrs {
            organization: Some("EPSG".into()),
            code: Some(4087),
            definition: Some(wkt.into()),
            definition_12_063: Some(wkt2.into()),
        };
        gpkg_reproject(100001, Some(row), &mut notes).unwrap();
        assert_eq!(notes.iter().filter(|n| n.contains("EPSG method 1028")).count(), usize::from(warned), "{wkt2}: {notes:?}");
    }
}

/// PROJ 9.8.1's WKT2 for EPSG:23031 (ED50 / UTM zone 31N), without its usage.
pub(crate) const ED50_UTM31: &str = r#"PROJCRS["ED50 / UTM zone 31N",BASEGEOGCRS["ED50",DATUM["European Datum 1950",ELLIPSOID["International 1924",6378388,297,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4230]],CONVERSION["UTM zone 31N",METHOD["Transverse Mercator",ID["EPSG",9807]],PARAMETER["Latitude of natural origin",0,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",3,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["Scale factor at natural origin",0.9996,SCALEUNIT["unity",1],ID["EPSG",8805]],PARAMETER["False easting",500000,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",0,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["(E)",east,ORDER[1],LENGTHUNIT["metre",1]],AXIS["(N)",north,ORDER[2],LENGTHUNIT["metre",1]],ID["EPSG",23031]]"#;

/// NAD 27 longitude/latitude in WKT2. Its shift to WGS 84 is a grid, so RustGIS knows none.
const NAD27_WKT2: &str = r#"GEOGCRS["NAD27",DATUM["North American Datum 1927",ELLIPSOID["Clarke 1866",6378206.4,294.978698213898]],CS[ellipsoidal,2],AXIS["lat",north],AXIS["lon",east],ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",4267]]"#;

/// [`ED50_UTM31`] bound to WGS 84 by Spain's NTv2 grid from ED50 (PROJ reads it).
pub(crate) fn ed50_utm31_bound() -> String {
    format!(
        r#"BOUNDCRS[SOURCECRS[{ED50_UTM31}],TARGETCRS[GEOGCRS["WGS 84",DATUM["World Geodetic System 1984",ELLIPSOID["WGS 84",6378137,298.257223563,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]],CS[ellipsoidal,2],AXIS["latitude",north,ORDER[1],ANGLEUNIT["degree",0.0174532925199433]],AXIS["longitude",east,ORDER[2],ANGLEUNIT["degree",0.0174532925199433]],ID["EPSG",4326]]],ABRIDGEDTRANSFORMATION["ED50 to WGS 84 (NTv2)",METHOD["NTv2",ID["EPSG",9615]],PARAMETERFILE["Latitude and longitude difference file","SPED2ETV2.gsb",ID["EPSG",8656]]]]"#
    )
}

/// A GeoPackage row read by its EPSG code (ED50 / UTM zone 31N, EPSG:23031) whose definition binds
/// it to WGS 84 by an NTv2 grid (Spain's, from ED50) is still read by the code, and says the grid
/// isn't applied, as `read_wkt` would; with the system alone as its definition it says nothing
/// (#1678).
#[test]
fn a_geopackage_row_read_by_its_code_says_its_definitions_grid() {
    let bound = ed50_utm31_bound();
    // Read by its code, whose own Helmert shift stands in for the grid.
    let said = "its coordinate system (srs_id 100001): the definition's NTv2 transformation to WGS 84 (SPED2ETV2.gsb) isn't applied; EPSG:23031's own shift to WGS 84 is used in its place, so positions may be off by the difference between the two";
    // Near Barcelona, where EPSG:23031 (and its own Helmert shift) places it.
    let xy = Coord::new(430_000.0, 4_580_000.0);
    let by_code = Crs::from_epsg(23031).unwrap().transformer().unwrap().to_lonlat(xy).unwrap();
    for (definition, wkt2, notice) in [(bound.as_str(), false, true), (bound.as_str(), true, true), (ED50_UTM31, false, false)] {
        let mut notes = Vec::new();
        let row = GpkgSrs {
            organization: Some("EPSG".into()),
            code: Some(23031),
            definition: (!wkt2).then(|| definition.into()),
            definition_12_063: wkt2.then(|| definition.into()),
        };
        let read = gpkg_reproject(100001, Some(row), &mut notes).unwrap();
        assert_eq!(notes, if notice { vec![said] } else { vec![] }, "{definition}");
        let Geometry::Point(at) = read.to_lonlat_geometry(Geometry::Point(xy)).unwrap() else { panic!("not a point") };
        assert!((at.x - by_code.x).abs() < 1e-9 && (at.y - by_code.y).abs() < 1e-9, "{at:?} {by_code:?}");
    }
}

/// EPSG:27700's own shift to WGS 84, as RustGIS writes it in WKT1.
const BNG_SHIFT: &str = "TOWGS84[446.448,-125.157,542.06,0.15,0.247,0.842,-20.489]";

/// A GeoPackage row's EPSG code is used as a `.prj`'s own `AUTHORITY` is (#1677): when its
/// definition gives no shift to WGS 84, or the code's own, with nothing to say. Another shift is
/// read from the definition: a `TOWGS84`, a WKT2 `BOUNDCRS`'s over a WKT1 with the code's own, or
/// a shift of zero, which is said. A definition that can't be read with its shift leaves the code,
/// said; so does one that can't be parsed, as it is without a code. One that parses but whose
/// system can't be read from its parameters leaves the code silently: its shift is the code's.
#[test]
fn a_geopackage_rows_code_gives_way_to_its_definitions_shift() {
    use crate::raster::wkt::tests::{WGS84_WKT2, bound_crs};
    use crate::raster::wkt::{read_wkt, to_wkt};
    let row = |code: i64, definition: &str, wkt2: Option<&str>| GpkgSrs {
        organization: Some("EPSG".into()),
        code: Some(code),
        definition: Some(definition.into()),
        definition_12_063: wkt2.map(Into::into),
    };
    let read = |row: GpkgSrs| {
        let mut notes = Vec::new();
        let read = gpkg_reproject(100001, Some(row), &mut notes).unwrap();
        (read, notes)
    };
    // Where a reading puts `xy`, and where a `.prj` of `wkt` would.
    let at = |r: &Reproject, xy: Coord| match r.to_lonlat_geometry(Geometry::Point(xy)) {
        Some(Geometry::Point(c)) => c,
        g => panic!("{g:?}"),
    };
    let prj = |wkt: &str, xy: Coord| read_wkt(wkt).unwrap().0.transformer().unwrap().to_lonlat(xy).unwrap();
    // In England, in British National Grid metres.
    let xy = Coord::new(450_000.0, 250_000.0);
    let by_code = Crs::Epsg(27700).transformer().unwrap().to_lonlat(xy).unwrap();
    let bng = crate::raster::wkt::tests::bng_wkt1();
    assert!(bng.contains(BNG_SHIFT), "{bng}");
    let unshifted = bng.replace(&format!(",{BNG_SHIFT}"), "");
    assert_ne!(unshifted, bng);
    for definition in [&bng, &unshifted] {
        let (r, notes) = read(row(27700, definition, None));
        assert_eq!(r.crs, Crs::Epsg(27700));
        assert!(notes.is_empty(), "{notes:?}");
    }
    // Another `TOWGS84` puts the point some 4 m from where the code's does.
    let other = bng.replace(BNG_SHIFT, "TOWGS84[375,-111,431,0,0,0,0]");
    let (r, notes) = read(row(27700, &other, None));
    assert!(metres_apart(at(&r, xy), prj(&other, xy)) < 0.001 && metres_apart(at(&r, xy), by_code) > 1.0, "{:?}", at(&r, xy));
    assert!(notes.is_empty(), "{notes:?}");
    // A WKT2 `BOUNDCRS` (ED50 / UTM zone 31N, near Barcelona) with another Helmert shift than
    // EPSG:23031's counts over the code's own WKT1.
    let xy = Coord::new(430_000.0, 4_580_000.0);
    let bound = bound_crs(ED50_UTM31, WGS84_WKT2, &[-100.0, -100.0, -100.0]);
    let (r, notes) = read(row(23031, &to_wkt(Crs::Epsg(23031)).unwrap(), Some(&bound)));
    let by_code = Crs::Epsg(23031).transformer().unwrap().to_lonlat(xy).unwrap();
    assert!(metres_apart(at(&r, xy), prj(&bound, xy)) < 0.001 && metres_apart(at(&r, xy), by_code) > 1.0, "{:?}", at(&r, xy));
    assert!(notes.is_empty(), "{notes:?}");
    // A shift of zero where the code has one.
    let (r, notes) = read(row(27700, &bng.replace(BNG_SHIFT, "TOWGS84[0,0,0,0,0,0,0]"), None));
    assert!(matches!(r.crs, Crs::Custom(_)), "{:?}", r.crs);
    assert_eq!(
        notes,
        [
            "its coordinate system (srs_id 100001): the file gives a shift of zero to WGS 84, which is used in place of EPSG:27700's transformation"
        ]
    );
    // A projection that can't be read from its parameters, with its own shift: the code, said.
    let unread = other.replace(r#"PROJECTION["Transverse_Mercator"]"#, r#"PROJECTION["Unheard_Of"]"#);
    assert_ne!(unread, other);
    let (r, notes) = read(row(27700, &unread, None));
    assert_eq!(r.crs, Crs::Epsg(27700));
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0]
            .starts_with("its coordinate system (srs_id 100001): the file's own shift to WGS 84 is set aside for EPSG:27700's, because"),
        "{notes:?}"
    );
    // Text that isn't WKT, said as without a code.
    let (r, notes) = read(row(27700, "PROJCS[", None));
    assert_eq!(r.crs, Crs::Epsg(27700));
    let e = read_wkt("PROJCS[").unwrap_err();
    assert_eq!(notes, [format!("its coordinate system (srs_id 100001): its definition can't be read ({e}), so EPSG:27700 is used")]);
    // Hungary's EOV (EPSG:23700) as ArcGIS writes it: the method isn't read from its parameters, but
    // the code names the system.
    let (r, notes) = read(row(23700, EOV_ESRI, None));
    assert!(read_wkt(EOV_ESRI).is_err());
    assert_eq!(r.crs, Crs::Epsg(23700));
    assert!(notes.is_empty(), "{notes:?}");
}

/// EPSG:23700 in Esri's WKT, with no `AUTHORITY`, by a method `read_wkt` doesn't read.
const EOV_ESRI: &str = r#"PROJCS["Hungarian_1972_Egyseges_Orszagos_Vetuleti",GEOGCS["GCS_Hungarian_1972",DATUM["D_Hungarian_1972",SPHEROID["GRS_1967",6378160.0,298.247167427]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Hotine_Oblique_Mercator_Azimuth_Center"],PARAMETER["False_Easting",650000.0],PARAMETER["False_Northing",200000.0],PARAMETER["Scale_Factor",0.99993],PARAMETER["Azimuth",90.0],PARAMETER["Longitude_Of_Center",19.0485717777778],PARAMETER["Latitude_Of_Center",47.1443937222222],UNIT["Meter",1.0]]"#;

/// A GeoPackage row's WKT2 that can't be read is said as it is without a code (#1677), with what is
/// used in its place: the WKT1 when it is read in place of the row's code, else the code. A WKT2
/// that is read says nothing of the kind.
#[test]
fn a_geopackage_rows_unread_wkt2_is_said_beside_its_code() {
    use crate::raster::wkt::tests::{WGS84_WKT2, bound_crs};
    use crate::raster::wkt::{read_wkt, to_wkt};
    let row = |code: Option<i64>, definition: &str, wkt2: &str| GpkgSrs {
        organization: code.map(|_| "EPSG".into()),
        code,
        definition: Some(definition.into()),
        definition_12_063: Some(wkt2.into()),
    };
    let read = |row: GpkgSrs| {
        let mut notes = Vec::new();
        let read = gpkg_reproject(100001, Some(row), &mut notes).unwrap();
        (read, notes)
    };
    let other = crate::raster::wkt::tests::bng_wkt1().replace(BNG_SHIFT, "TOWGS84[375,-111,431,0,0,0,0]");
    assert!(other.contains("TOWGS84[375,"), "{other}");
    let (r, notes) = read(row(Some(27700), &other, "PROJCRS["));
    assert_eq!(Ok(r.crs), read_wkt(&other).map(|r| r.0));
    assert!(matches!(r.crs, Crs::Custom(_)), "{:?}", r.crs);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].starts_with("its coordinate system (srs_id 100001): its WKT2 definition can't be read (")
            && notes[0].ends_with("), so its WKT1 definition is used"),
        "{notes:?}"
    );
    // The same notice as without a code.
    assert_eq!(read(row(None, &other, "PROJCRS[")).1, notes);
    // And when the code is used: the WKT1 gives the code's own shift, or none, or there is none.
    let by_code = notes[0].replace("so its WKT1 definition is used", "so EPSG:27700 is used");
    let bng = crate::raster::wkt::tests::bng_wkt1();
    for wkt1 in [Some(bng.clone()), Some(bng.replace(&format!(",{BNG_SHIFT}"), "")), None] {
        let (r, used) = read(GpkgSrs { definition: wkt1, ..row(Some(27700), "", "PROJCRS[") });
        assert_eq!(r.crs, Crs::Epsg(27700));
        assert_eq!(used, [by_code.as_str()]);
    }
    // A WKT2 bound to a system whose shift to WGS 84 isn't known can't be read either. Its
    // transformation is passed over with it, which the notice says.
    let unknown = bound_crs(ED50_UTM31, NAD27_WKT2, &[-100.0, -100.0, -100.0]);
    let ed50 = to_wkt(Crs::Epsg(23031)).unwrap();
    let (r, notes) = read(row(Some(23031), &ed50, &unknown));
    assert_eq!(r.crs, Crs::Epsg(23031));
    assert_eq!(
        notes,
        [
            "its coordinate system (srs_id 100001): its WKT2 definition can't be read (the shift from NAD27 to WGS 84 isn't known), so EPSG:23031 is used"
        ]
    );
    // A WKT2 that is read, with another shift than the code's, says nothing of the WKT1.
    let bound = bound_crs(ED50_UTM31, WGS84_WKT2, &[-100.0, -100.0, -100.0]);
    let (r, notes) = read(row(Some(23031), &other, &bound));
    assert_eq!(Ok(r.crs), read_wkt(&bound).map(|r| r.0));
    assert!(notes.is_empty(), "{notes:?}");
    // Nor does one that leaves the code in use, whatever the WKT1 beside it.
    for wkt1 in [ed50.as_str(), "PROJCS["] {
        let (r, notes) = read(row(Some(23031), wkt1, ED50_UTM31));
        assert_eq!(r.crs, Crs::Epsg(23031));
        assert!(notes.is_empty(), "{notes:?}");
    }
}

/// A GeoPackage row whose only definition is a `BOUNDCRS` that can't be read with its
/// transformation (to a system whose shift to WGS 84 isn't known) is read by its code, and says
/// that the definition's shift is set aside (#1677). Without a code it is read as
/// longitude/latitude, said.
#[test]
fn a_geopackage_rows_unread_bound_definition_is_said_beside_its_code() {
    use crate::raster::wkt::tests::bound_crs;
    let row = |code: Option<i64>, definition: &str, wkt2: Option<&str>| GpkgSrs {
        organization: code.map(|_| "EPSG".into()),
        code,
        definition: Some(definition.into()),
        definition_12_063: wkt2.map(Into::into),
    };
    let read = |row: GpkgSrs| {
        let mut notes = Vec::new();
        let read = gpkg_reproject(100001, Some(row), &mut notes).unwrap();
        (read, notes)
    };
    let unknown = bound_crs(ED50_UTM31, NAD27_WKT2, &[-100.0, -100.0, -100.0]);
    let set_aside = "its coordinate system (srs_id 100001): the file's own shift to WGS 84 is set aside for EPSG:23031's, because the shift from NAD27 to WGS 84 isn't known";
    // As the WKT2 beside a `definition` that defines nothing, and as the `definition` itself.
    for (definition, wkt2) in [("undefined", Some(unknown.as_str())), (unknown.as_str(), None)] {
        let (r, notes) = read(row(Some(23031), definition, wkt2));
        assert_eq!(r.crs, Crs::Epsg(23031));
        assert_eq!(notes, [set_aside]);
        let (r, notes) = read(row(None, definition, wkt2));
        assert_eq!(r.crs, Crs::LonLat);
        assert_eq!(
            notes,
            [
                "its coordinate system (srs_id 100001) can't be read (the shift from NAD27 to WGS 84 isn't known); read as longitude/latitude"
            ]
        );
    }
    // A definition that can't be read and holds a transformation bound to no system gives no
    // shift to set aside: the code, said only as a definition that can't be read.
    let unbound = r#"COMPOUNDCRS["no horizontal part",ABRIDGEDTRANSFORMATION["t",PARAMETER["X-axis translation",375]]]"#;
    let (r, notes) = read(row(Some(23031), unbound, None));
    assert_eq!(r.crs, Crs::Epsg(23031));
    assert_eq!(
        notes,
        [
            "its coordinate system (srs_id 100001): its definition can't be read (the compound coordinate system has no horizontal part), so EPSG:23031 is used"
        ]
    );
}

/// A database layer's `crs` is read by its EPSG code. WKT whose system has one but binds it to WGS
/// 84 by a grid (ED50 / UTM zone 31N, by Spain's) says the grid isn't applied; a code, or the
/// system alone, says nothing; text without a code is refused, as before (#1678).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_database_crs_read_by_its_code_says_its_definitions_grid() {
    let said = "crs: the definition's NTv2 transformation to WGS 84 (SPED2ETV2.gsb) isn't applied; EPSG:23031's own shift to WGS 84 is used in its place, so positions may be off by the difference between the two";
    assert_eq!(given_epsg(&ed50_utm31_bound()).unwrap(), (23031, Some(said.to_string())));
    assert_eq!(given_epsg(ED50_UTM31).unwrap(), (23031, None));
    assert_eq!(given_epsg("EPSG:23031").unwrap(), (23031, None));
    for refused in [crate::raster::wkt::tests::BOUND_BY_A_GRID, "Mercator"] {
        assert_eq!(given_epsg(refused).unwrap_err().to_string(), format!("'{refused}': give the coordinate system as EPSG:<code>"));
    }
}

#[test]
fn file_geodatabase_names() {
    assert!(is_gdb_name("/x/roads.gdb") && is_gdb_name("Roads.GDB/") && is_gdb_name(r"C:\x\roads.gdb\"));
    assert!(!is_gdb_name("roads.gdbtable") && !is_gdb_name("gdb") && !is_gdb_name("roads.shp"));
    // A file picked inside a geodatabase stands for the geodatabase.
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/a00000001.gdbtable")), Some(Path::new("/x/roads.gdb")));
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/gdb")), Some(Path::new("/x/roads.gdb")));
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/a0000000A.spx")), Some(Path::new("/x/roads.gdb")));
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/a00000004.CatItemsByType.atx")), Some(Path::new("/x/roads.gdb")));
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/timestamps")), Some(Path::new("/x/roads.gdb")));
    // A file named like a table, but of another kind, is itself.
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/a00000001.csv")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/a0000000b.bak")), None);
    // Other files kept in the folder are themselves.
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/readme.txt")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/export.csv")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/a1.gdbtable")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/x0000000a.gdbtable")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.gdb")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.gdb/")), None);
    assert_eq!(gdb_of(Path::new("/x/roads.shp")), None);
    assert_eq!(gdb_of(Path::new("a00000001.gdbtable")), None);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn tls_provider_builds_client_config() {
    tls_provider();
    let _ = rustls::ClientConfig::builder();
}

/// SQLite's upload copies never replace the original input in provenance, including later scans.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn geopackage_provenance_excludes_temporary_copies() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("original.gpkg");
    write_gpkg(&path, true);
    let captured = crate::provenance::Capture::start();
    let bytes = crate::provenance::read(&path).unwrap();
    let upload = gpkg::GpkgUpload::new(&bytes).unwrap();
    assert!(upload.unsupported_table_counts().unwrap().is_empty());
    let source = upload.open(None).unwrap();
    assert!(!source.read(&Request { extent: None, limit: 1 }).unwrap().features.is_empty());
    assert_eq!(captured.inputs().len(), 1);
    assert_eq!(captured.inputs()[0].path, path.display().to_string());
    assert!(captured.inputs()[0].sha256.is_some());
    drop(captured);
    let captured = crate::provenance::Capture::start();
    assert!(!source.read(&Request { extent: None, limit: 1 }).unwrap().features.is_empty());
    let mut count = 0;
    source
        .visit_chunks(None, 1 << 20, &mut |features| {
            count += features.len();
            Ok(())
        })
        .unwrap();
    assert_eq!(count, points().len());
    assert!(captured.inputs().is_empty(), "in-memory inputs have no local path to observe");
}

/// A retained SQLite connection must not describe the replacement at its former path.
#[cfg(unix)]
#[test]
fn geopackage_provenance_skips_a_replaced_path() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("original.gpkg");
    write_gpkg(&path, true);
    let source = open_path(&path, None).unwrap();
    let captured = crate::provenance::Capture::start();
    source.read(&Request { extent: None, limit: 1 }).unwrap();
    assert!(captured.inputs().iter().any(|i| i.path == path.display().to_string()));
    drop(captured);
    std::fs::rename(&path, folder.path().join("retained.gpkg")).unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    let captured = crate::provenance::Capture::start();
    assert!(!source.read(&Request { extent: None, limit: 1 }).unwrap().features.is_empty());
    assert!(captured.inputs().is_empty());
}

/// A GeoPackage reached through a linked folder is recorded at the path opened, as macOS's
/// temporary folder (`/var` → `/private/var`) is, its SQLite companions too; through a link to the
/// file itself, the companions are found where SQLite puts them, beside the target.
#[cfg(unix)]
#[test]
fn geopackage_provenance_keeps_a_linked_path() {
    let folder = tempfile::tempdir().unwrap();
    let real = folder.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let target = real.join("original.gpkg");
    write_gpkg(&target, true);
    std::fs::write(real.join("original.gpkg-wal"), b"wal").unwrap();
    let inputs = |path: &Path| {
        let source = open_path(path, None).unwrap();
        let captured = crate::provenance::Capture::start();
        assert!(!source.read(&Request { extent: None, limit: 1 }).unwrap().features.is_empty());
        captured.inputs().into_iter().map(|i| i.path).collect::<Vec<String>>()
    };
    let resolved = std::fs::canonicalize(&target).unwrap();
    let link = folder.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let path = link.join("original.gpkg");
    let paths = inputs(&path);
    assert!(paths.contains(&path.display().to_string()), "{paths:?}");
    assert!(paths.contains(&format!("{}-wal", path.display())), "{paths:?}");
    assert!(!paths.iter().any(|p| p.starts_with(&resolved.display().to_string())), "{paths:?}");
    let alias = folder.path().join("alias.gpkg");
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    let paths = inputs(&alias);
    assert!(paths.contains(&alias.display().to_string()), "{paths:?}");
    assert!(paths.contains(&format!("{}-wal", resolved.display())), "{paths:?}");
}

/// A retained conversion read after its original changed names the original as it was when the
/// conversion was opened, not as it is now; a DuckDB database's write-ahead log with it (#2639).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_conversion_names_its_original_as_it_was_when_opened() {
    use crate::provenance::{Capture, Fingerprint};
    let folder = tempfile::tempdir().unwrap();
    let original = folder.path().join("drawing.dwg");
    std::fs::write(&original, b"first").unwrap();
    let converted = folder.path().join("cache.fgb");
    write_fgb(&converted);
    let source = super::fgb::FgbSource::open_converted_path(&converted, "grid", &original, None).unwrap();
    let inputs = || {
        let captured = Capture::start();
        assert!(!source.read(&Request { extent: None, limit: 1 }).unwrap().features.is_empty());
        captured.inputs()
    };
    let before = inputs();
    assert!(before.len() == 1 && before[0].size == Some(5) && before[0].path == original.display().to_string(), "{before:?}");
    std::fs::write(&original, b"changed since").unwrap();
    assert_eq!(inputs(), before, "the conversion holds the original as it was");
    // GeoParquet from a DuckDB database: the database and its log as they were.
    let db = folder.path().join("places.duckdb");
    std::fs::write(&db, b"db").unwrap();
    std::fs::write(folder.path().join("places.duckdb.wal"), b"wal").unwrap();
    let parquet = folder.path().join("cache.parquet");
    write_parquet(&parquet, true);
    let source = super::parquet::ParquetSource::open_converted_path(&parquet, "grid", Some(&db), None).unwrap();
    let inputs = || {
        let captured = Capture::start();
        assert!(!source.read(&Request { extent: None, limit: 1 }).unwrap().features.is_empty());
        captured.inputs()
    };
    let before: Vec<Fingerprint> = inputs();
    assert_eq!(before.len(), 2, "{before:?}");
    assert!(before.iter().any(|i| i.path.ends_with("places.duckdb.wal") && i.size == Some(3)), "{before:?}");
    assert!(!before.iter().any(|i| i.path.ends_with("cache.parquet")), "{before:?}");
    std::fs::write(folder.path().join("places.duckdb.wal"), b"more changes").unwrap();
    assert_eq!(inputs(), before);
}

/// 30 × 20 points over the world, named and numbered.
fn points() -> Vec<(Coord, String, i64)> {
    let mut v = Vec::new();
    for i in 0..30 {
        for j in 0..20 {
            let c = Coord::new(-174.0 + 12.0 * i as f64, -76.0 + 8.0 * j as f64);
            v.push((c, format!("p{i}_{j}"), (i * 20 + j) as i64));
        }
    }
    v
}

fn in_box(e: &Extent) -> usize {
    points().iter().filter(|(c, ..)| e.contains(*c)).count()
}

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("rustgis-source-tests-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn write_parquet(path: &Path, covering: bool) {
    write_parquet_typed(path, covering, r#"["Point"]"#);
}

/// [`write_parquet`], with the `geo` metadata's `geometry_types` (a JSON list) given.
fn write_parquet_typed(path: &Path, covering: bool, types: &str) {
    use arrow_array::{BinaryArray, Float32Array, Int64Array, RecordBatch, StringArray, StructArray};
    use arrow_schema::{DataType, Field as AField, Schema};
    let pts = points();
    let wkbs: Vec<Vec<u8>> = pts.iter().map(|(c, ..)| wkb::encode(&Geometry::Point(*c))).collect();
    let bbox_fields = ["xmin", "ymin", "xmax", "ymax"].map(|n| Arc::new(AField::new(n, DataType::Float32, false)));
    let coord = |f: fn(&Coord) -> f64| Arc::new(Float32Array::from(pts.iter().map(|(c, ..)| f(c) as f32).collect::<Vec<_>>()));
    let bbox =
        StructArray::new(bbox_fields.iter().cloned().collect(), vec![coord(|c| c.x), coord(|c| c.y), coord(|c| c.x), coord(|c| c.y)], None);
    let mut fields = vec![
        AField::new("name", DataType::Utf8, false),
        AField::new("value", DataType::Int64, false),
        AField::new("geometry", DataType::Binary, false),
    ];
    let mut cols: Vec<arrow_array::ArrayRef> = vec![
        Arc::new(StringArray::from(pts.iter().map(|p| p.1.clone()).collect::<Vec<_>>())),
        Arc::new(Int64Array::from(pts.iter().map(|p| p.2).collect::<Vec<_>>())),
        Arc::new(BinaryArray::from(wkbs.iter().map(|w| w.as_slice()).collect::<Vec<_>>())),
    ];
    let geo = if covering {
        fields.push(AField::new("geometry_bbox", DataType::Struct(bbox_fields.iter().cloned().collect()), false));
        cols.push(Arc::new(bbox));
        format!(
            r#"{{"version":"1.1.0","primary_column":"geometry","columns":{{"geometry":{{"encoding":"WKB","geometry_types":{types},
            "covering":{{"bbox":{{"xmin":["geometry_bbox","xmin"],"ymin":["geometry_bbox","ymin"],"xmax":["geometry_bbox","xmax"],"ymax":["geometry_bbox","ymax"]}}}}}}}}}}"#
        )
    } else {
        format!(
            r#"{{"version":"1.0.0","primary_column":"geometry","columns":{{"geometry":{{"encoding":"WKB","geometry_types":{types}}}}}}}"#
        )
    };
    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();
    let props = ::parquet::file::properties::WriterProperties::builder()
        .set_max_row_group_row_count(Some(50))
        .set_key_value_metadata(Some(vec![::parquet::file::metadata::KeyValue::new("geo".to_string(), geo.to_string())]))
        .build();
    let mut w = ::parquet::arrow::ArrowWriter::try_new(std::fs::File::create(path).unwrap(), schema, Some(props)).unwrap();
    w.write(&batch).unwrap();
    w.close().unwrap();
}

fn write_fgb(path: &Path) {
    use flatgeobuf::geozero::{ColumnValue, PropertyProcessor};
    use flatgeobuf::{ColumnType, FgbWriter, GeometryType as G};
    let mut w = FgbWriter::create("grid", G::Point).unwrap();
    w.add_column("name", ColumnType::String, |_, _| {});
    w.add_column("value", ColumnType::Long, |_, _| {});
    for (c, name, v) in points() {
        let g = flatgeobuf::geozero::wkb::Wkb(wkb::encode(&Geometry::Point(c)));
        w.add_feature_geom(g, |f| {
            f.property(0, "name", &ColumnValue::String(&name)).unwrap();
            f.property(1, "value", &ColumnValue::Long(v)).unwrap();
        })
        .unwrap();
    }
    w.write(std::fs::File::create(path).unwrap()).unwrap();
}

/// A GeoPackage of the points in Web Mercator, with an R-tree index (or without).
pub(super) fn write_gpkg(path: &Path, rtree: bool) {
    let _ = std::fs::remove_file(path);
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch(
        "CREATE TABLE gpkg_spatial_ref_sys (srs_name TEXT, srs_id INTEGER PRIMARY KEY, organization TEXT, organization_coordsys_id INTEGER, definition TEXT, description TEXT);
         INSERT INTO gpkg_spatial_ref_sys VALUES ('Web Mercator', 3857, 'EPSG', 3857, '', '');
         CREATE TABLE gpkg_contents (table_name TEXT PRIMARY KEY, data_type TEXT, identifier TEXT, description TEXT, last_change TEXT, min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE, srs_id INTEGER);
         CREATE TABLE gpkg_geometry_columns (table_name TEXT, column_name TEXT, geometry_type_name TEXT, srs_id INTEGER, z TINYINT, m TINYINT);
         INSERT INTO gpkg_contents (table_name, data_type, srs_id) VALUES ('grid \"q\"', 'features', 3857);
         INSERT INTO gpkg_geometry_columns VALUES ('grid \"q\"', 'geom', 'POINT', 3857, 0, 0);
         CREATE TABLE \"grid \"\"q\"\"\" (fid INTEGER PRIMARY KEY AUTOINCREMENT, geom BLOB, name TEXT, value MEDIUMINT, ok BOOLEAN);",
    )
    .unwrap();
    if rtree {
        c.execute_batch("CREATE VIRTUAL TABLE \"rtree_grid \"\"q\"\"_geom\" USING rtree(id, minx, maxx, miny, maxy);").unwrap();
    }
    // One transaction: a commit per row is slow on Windows (minutes for the fixture).
    let tx = c.unchecked_transaction().unwrap();
    for (i, (p, name, v)) in points().into_iter().enumerate() {
        let m = lonlat_to_merc(p);
        // A header with an xy envelope, then WKB.
        let mut blob = b"GP\x00\x03".to_vec();
        blob.extend_from_slice(&3857i32.to_le_bytes());
        for x in [m.x, m.x, m.y, m.y] {
            blob.extend_from_slice(&x.to_le_bytes());
        }
        blob.extend(wkb::encode(&Geometry::Point(m)));
        let fid = i as i64 + 1;
        tx.execute("INSERT INTO \"grid \"\"q\"\"\" VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params![fid, blob, name, v, v % 2]).unwrap();
        if rtree {
            tx.execute("INSERT INTO \"rtree_grid \"\"q\"\"_geom\" VALUES (?1, ?2, ?3, ?4, ?5)", rusqlite::params![fid, m.x, m.x, m.y, m.y])
                .unwrap();
        }
    }
    tx.commit().unwrap();
}

/// A read's features in longitude/latitude, from the system the source gives them in
/// ([`FeatureSource::coord_crs`]), for a test to say where they are.
fn in_lonlat(src: &dyn FeatureSource, mut batch: Batch) -> Batch {
    for f in &mut batch.features {
        f.geometry = lonlat_of(src, f.geometry.take());
    }
    batch
}

/// A geometry a source gave, in longitude/latitude ([`in_lonlat`]).
fn lonlat_of(src: &dyn FeatureSource, g: Option<Geometry>) -> Option<Geometry> {
    let to = src.coord_crs().to(Crs::LonLat).unwrap();
    g.map(|g| to.geometry(&g))
}

/// The same checks for every provider.
fn check(src: &dyn FeatureSource, expect_indexed: bool) {
    let info = src.info();
    assert_eq!(info.geometry_type, GeometryType::Point, "{}", info.format);
    assert_eq!(info.count, Some(600), "{}", info.format);
    assert_eq!(info.indexed, expect_indexed, "{}", info.format);
    assert!(info.fields.iter().any(|f| f.name == "name" && f.ty == FieldType::Text), "{:?}", info.fields);
    let vi = info.fields.iter().position(|f| f.name == "value").unwrap();
    assert_eq!(info.fields[vi].ty, FieldType::Integer);

    // Everything.
    let all = in_lonlat(src, src.read(&Request::all()).unwrap());
    assert_eq!(all.features.len(), 600, "{}", info.format);
    assert!(all.complete);
    let f = all.features.iter().find(|f| f.attrs.contains(&Value::Text("p3_4".into()))).unwrap();
    let c = match f.geometry {
        Some(Geometry::Point(c)) => c,
        _ => panic!("not a point"),
    };
    assert!((c.x - (-138.0)).abs() < 1e-4 && (c.y - (-44.0)).abs() < 1e-4, "{c:?} in {}", info.format);
    assert_eq!(f.attrs[vi], Value::Int(64));
    // Ids stay put between reads.
    let near = in_lonlat(src, src.read(&Request { extent: Some(Extent::new(-140.0, -46.0, -130.0, -40.0)), limit: 100 }).unwrap());
    let g = near.features.iter().find(|g| g.attrs.contains(&Value::Text("p3_4".into()))).unwrap();
    assert_eq!(g.id, f.id, "{}", info.format);

    // An extent: exactly the points in it.
    let e = Extent::new(-60.0, -10.0, 40.0, 50.0);
    let got = in_lonlat(src, src.read(&Request { extent: Some(e), limit: 10_000 }).unwrap());
    assert_eq!(got.features.len(), in_box(&e), "{}", info.format);
    assert!(got.complete);
    assert!(got.features.iter().all(|f| f.geometry.as_ref().is_some_and(|g| e.contains(g.extent().center()))));
    // Every feature keeps the id a whole read gives it.
    let id_of: std::collections::HashMap<String, u64> =
        all.features.iter().map(|f| (f.attrs[0].to_string() + &f.attrs[1].to_string(), f.id)).collect();
    for f in &got.features {
        assert_eq!(id_of.get(&(f.attrs[0].to_string() + &f.attrs[1].to_string())), Some(&f.id), "{}", info.format);
    }

    // A limit: a sample spread over the whole extent, not the first rows.
    let s = in_lonlat(src, src.read(&Request { extent: None, limit: 60 }).unwrap());
    assert!(!s.complete);
    assert!(s.features.len() <= 60 && s.features.len() >= 50, "{} of {}", s.features.len(), info.format);
    let xs: Vec<f64> = s.features.iter().filter_map(|f| f.geometry.as_ref()).map(|g| g.extent().min_x).collect();
    assert!(xs.iter().any(|x| *x < -100.0) && xs.iter().any(|x| *x > 100.0), "{} sample isn't spread", info.format);
    assert!(s.matched >= 600);

    // An extent outside the data.
    let none = in_lonlat(src, src.read(&Request { extent: Some(Extent::new(-179.9, 80.0, -179.0, 84.0)), limit: 100 }).unwrap());
    assert!(none.features.is_empty());

    // A whole read into a dataset.
    let (ds, report) = materialize(src).unwrap();
    assert_eq!(ds.features.len(), 600);
    assert!(report.messages.is_empty());
}

#[test]
fn materialize_refuses_oversized_and_incomplete_sources() {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Source {
        info: SourceInfo,
        batch: Option<Batch>,
        read: AtomicBool,
    }
    impl FeatureSource for Source {
        fn info(&self) -> &SourceInfo {
            &self.info
        }

        fn read(&self, _: &Request) -> Result<Batch> {
            self.read.store(true, Ordering::Relaxed);
            self.batch.clone().ok_or_else(|| DataError::Format("read reached".into()))
        }
    }
    let mut src = Source {
        info: SourceInfo {
            name: "large".into(),
            format: "Test",
            geometry_type: GeometryType::None,
            fields: vec![],
            extent: Extent::EMPTY,
            count: Some(MAX_LIMIT as u64 + 1),
            crs: "EPSG:4326".into(),
            indexed: false,
            notes: Default::default(),
            zm_dropped: false,
        },
        batch: None,
        read: AtomicBool::new(false),
    };
    let error = materialize(&src).unwrap_err().to_string();
    assert!(error.contains(&(MAX_LIMIT + 1).to_string()) && error.contains(&MAX_LIMIT.to_string()), "{error}");
    assert!(!src.read.load(Ordering::Relaxed), "an oversized source must be refused before reading");

    // Exactly the limit is allowed through to the provider; no huge allocation needed to check.
    src.info.count = Some(MAX_LIMIT as u64);
    assert!(materialize(&src).unwrap_err().to_string().contains("read reached"));
    assert!(src.read.load(Ordering::Relaxed));

    // Unknown or stale metadata must not allow an incomplete batch to become a dataset.
    for count in [None, Some(1)] {
        src.info.count = count;
        src.batch = Some(Batch { matched: MAX_LIMIT as u64 + 1, complete: false, ..Batch::default() });
        let error = materialize(&src).unwrap_err().to_string();
        assert!(error.contains(&(MAX_LIMIT + 1).to_string()) && error.contains(&MAX_LIMIT.to_string()), "{error}");
    }
    // Even an incomplete answer below the cap isn't a whole read.
    src.batch = Some(Batch { matched: 1, complete: false, ..Batch::default() });
    let error = materialize(&src).unwrap_err().to_string();
    assert!(error.contains("large") && error.contains(&MAX_LIMIT.to_string()), "{error}");

    // Unreadable rows reported by a complete scan still follow ReadReport's skip contract.
    src.batch = Some(Batch { matched: 1, complete: true, skipped: 1, ..Batch::default() });
    let (ds, report) = materialize(&src).unwrap();
    assert!(ds.features.is_empty());
    assert_eq!(report.skipped, 1);
}

/// A whole read of a source that can be read a chunk at a time holds what one read gives, and is
/// refused once its features outgrow the memory budget (#1111).
#[test]
fn whole_reads_by_chunk_stay_within_the_memory_budget() {
    let d = dir().join("budget");
    std::fs::create_dir_all(&d).unwrap();
    let mut ds = Dataset::new("lines", GeometryType::Polyline, vec![Field::new("name", FieldType::Text)]);
    for i in 0..200 {
        let x = f64::from(i) * 0.01;
        ds.push_next(Some(Geometry::Line(vec![vec![Coord::new(x, 1.0), Coord::new(x, 2.0)]])), vec![Value::Text(format!("line {i}"))]);
    }
    for ext in ["gpkg", "fgb", "parquet", "shp"] {
        let p = d.join(format!("lines.{ext}"));
        let _ = std::fs::remove_file(&p);
        crate::io::write_path(&ds, &p).unwrap();
        let src = open_path(&p, None).unwrap();
        assert!(src.visits_chunks(), "{ext}");
        let once = src.read(&Request::all()).unwrap().features;
        let (whole, report) = materialize_within(src.as_ref(), MAX_READ_BYTES).unwrap();
        assert_eq!(whole.features.to_vec(), once, "{ext}");
        assert_eq!(report.skipped, 0, "{ext}");
        let half = chunk_memory::features_bytes(&once) as u64 / 2;
        let error = materialize_within(src.as_ref(), half).unwrap_err().to_string();
        assert!(error.contains("clip or filter it first"), "{ext}: {error}");
    }
}

/// A GeoParquet column in another body's system (PROJJSON, as PROJ writes IAU_2015) is read as
/// its longitude/latitude with a notice, refused when its coordinates are projected or count west,
/// and an EPSG one says nothing of bodies.
#[test]
fn geoparquet_planetary_systems_are_said_or_refused() {
    let d = dir().join("parquet-planetary");
    std::fs::create_dir_all(&d).unwrap();
    let body = |m: &String| m.contains("belongs to another body than the Earth");
    let mars = |kind: &str, code: u32, lon: &str| {
        format!(
            r#"["Point"],"crs":{{"type":"{kind}","name":"Mars (2015)","datum":{{"type":"GeodeticReferenceFrame","name":"Mars (2015) - Sphere","ellipsoid":{{"name":"Mars (2015) - Sphere","radius":3396190}}}},"coordinate_system":{{"subtype":"ellipsoidal","axis":[{{"name":"Geodetic latitude","direction":"north","unit":"degree"}},{{"name":"Geodetic longitude","direction":"{lon}","unit":"degree"}}]}},"id":{{"authority":"IAU","version":2015,"code":{code}}}}}"#
        )
    };
    let p = d.join("mars.parquet");
    write_parquet_typed(&p, false, &mars("GeographicCRS", 49900, "east"));
    let src = open_path(&p, None).unwrap();
    assert_eq!(src.info().notes.iter().filter(|m| body(&m.text)).count(), 1, "{:?}", src.info().notes);
    assert!(!src.info().notes.iter().any(|m| m.contains("no EPSG code")), "{:?}", src.info().notes);
    let (ds, _) = materialize(src.as_ref()).unwrap();
    assert_eq!(
        ds.features.iter().filter_map(|f| f.geometry.as_ref()?.label_point()).collect::<Vec<_>>(),
        points().iter().map(|p| p.0).collect::<Vec<_>>()
    );
    // The definition goes before the code: a projected or mirrored one under a planetocentric
    // code is refused too.
    for (kind, code, lon, why) in [
        ("ProjectedCRS", 49910, "east", "metres"),
        ("GeographicCRS", 49901, "west", "west"),
        ("ProjectedCRS", 49900, "east", "metres"),
        ("GeographicCRS", 49900, "west", "west"),
    ] {
        write_parquet_typed(&p, false, &mars(kind, code, lon));
        let e = open_path(&p, None).err().unwrap().to_string();
        assert!(e.contains("belongs to another body than the Earth") && e.contains(why), "{kind} {code} {lon}: {e}");
    }
    let no_id = |s: String| s.replace(r#","id":{"authority":"IAU","version":2015,"code":49900}"#, "");
    // Latitudes counted south, by code or by ellipsoid.
    for crs in [mars("GeographicCRS", 49900, "east"), no_id(mars("GeographicCRS", 49900, "east"))] {
        write_parquet_typed(&p, false, &crs.replace(r#""direction":"north""#, r#""direction":"south""#));
        let e = open_path(&p, None).err().unwrap().to_string();
        assert!(e.contains("belongs to another body than the Earth") && e.contains("south"), "{e}");
    }
    // By its ellipsoid alone (no IAU code): said by the ellipsoid, not by a code it hasn't; and an
    // Earth one.
    write_parquet_typed(&p, false, &no_id(mars("GeographicCRS", 49900, "east")));
    let notes = open_path(&p, None).unwrap().info().notes.clone();
    let said: Vec<&crate::loss::Note> = notes.iter().filter(|m| body(&m.text)).collect();
    assert_eq!(said.len(), 1, "{notes:?}");
    assert!(said[0].contains("as only its ellipsoid says") && !said[0].contains("code"), "{said:?}");
    write_parquet_typed(&p, false, r#"["Point"],"crs":{"type":"GeographicCRS","name":"WGS 84","id":{"authority":"EPSG","code":4326}}"#);
    assert!(open_path(&p, None).unwrap().info().notes.is_empty());

    // Its coordinate system's own axes, by code or by ellipsoid: geocentric X/Y/Z, and
    // longitude/latitude in another unit than degrees, would be read as degrees, so are refused.
    let axes = |kind: &str, id: &str, cs: &str| {
        format!(
            r#"["Point"],"crs":{{"type":"{kind}","name":"Mars (2015)","datum":{{"type":"GeodeticReferenceFrame","name":"Mars (2015) - Sphere","ellipsoid":{{"name":"Mars (2015) - Sphere","radius":3396190}}}},"coordinate_system":{cs}{id}}}"#
        )
    };
    const IAU: &str = r#","id":{"authority":"IAU","version":2015,"code":49900}"#;
    let lonlat = |lat: &str, lon: &str| {
        format!(
            r#"{{"subtype":"ellipsoidal","axis":[{{"name":"Geodetic latitude","abbreviation":"Lat","direction":"north","unit":{lat}}},{{"name":"Geodetic longitude","abbreviation":"Lon","direction":"east","unit":{lon}}}]}}"#
        )
    };
    let xyz = r#"{"subtype":"Cartesian","axis":[{"name":"Geocentric X","abbreviation":"X","direction":"geocentricX","unit":"metre"},{"name":"Geocentric Y","abbreviation":"Y","direction":"geocentricY","unit":"metre"},{"name":"Geocentric Z","abbreviation":"Z","direction":"geocentricZ","unit":"metre"}]}"#;
    let grad = r#"{"type":"AngularUnit","name":"grad","conversion_factor":0.015707963267949}"#;
    let degree = r#"{"type":"AngularUnit","name":"degree","conversion_factor":0.0174532925199433}"#;
    for id in ["", IAU] {
        for (kind, cs, why) in [
            ("GeodeticCRS", xyz.to_string(), "geocentric X/Y/Z"),
            ("GeographicCRS", lonlat(r#""radian""#, r#""radian""#), "in radian, not degrees"),
            ("GeographicCRS", lonlat(r#""degree""#, grad), "in grad, not degrees"),
        ] {
            write_parquet_typed(&p, false, &axes(kind, id, &cs));
            let e = open_path(&p, None).err().unwrap().to_string();
            assert!(e.contains("belongs to another body than the Earth") && e.contains(why), "{kind} {id}: {e}");
            let bytes = std::fs::read(&p).unwrap();
            assert!(open_bytes("mars.parquet", bytes, None).err().unwrap().to_string().contains(why), "{kind} {id}");
        }
        // Degrees, by name or by factor; a geodetic system on ellipsoidal or spherical axes; and an
        // ellipsoidal height in metres: read, with the notice.
        let height = lonlat(r#""degree""#, r#""degree""#)
            .replace("]}", r#",{"name":"Ellipsoidal height","abbreviation":"h","direction":"up","unit":"metre"}]}"#);
        for (kind, cs) in [
            ("GeodeticCRS", lonlat(r#""degree""#, r#""degree""#)),
            ("GeodeticCRS", lonlat(r#""degree""#, r#""degree""#).replace("ellipsoidal", "spherical")),
            ("GeographicCRS", lonlat(degree, r#"{"name":"degree"}"#)),
            ("GeographicCRS", lonlat(r#""Degree""#, r#""deg""#)),
            ("GeographicCRS", height),
        ] {
            write_parquet_typed(&p, false, &axes(kind, id, &cs));
            let src = open_path(&p, None).unwrap_or_else(|e| panic!("{kind} {cs} {id}: {e}"));
            assert_eq!(src.info().notes.iter().filter(|m| body(&m.text)).count(), 1, "{kind} {cs} {id}: {:?}", src.info().notes);
            let (ds, _) = materialize(src.as_ref()).unwrap();
            assert_eq!(ds.features.len(), points().len());
        }
    }
    // An Earth system, in radians or as X/Y/Z, says nothing of bodies (it is read as before).
    for cs in [lonlat(r#""radian""#, r#""radian""#), xyz.to_string()] {
        write_parquet_typed(&p, false, &axes("GeographicCRS", "", &cs).replace("3396190", "6378137"));
        let notes = open_path(&p, None).unwrap().info().notes.clone();
        assert!(!notes.iter().any(|m| body(&m.text)), "{notes:?}");
    }
}

#[test]
fn geoparquet_with_and_without_a_covering() {
    let d = dir();
    let p = d.join("grid.parquet");
    write_parquet(&p, true);
    let src = open_path(&p, None).unwrap();
    check(src.as_ref(), true);
    // The covering's row-group statistics give the extent.
    assert!(src.info().extent.min_x <= -174.0 && src.info().extent.max_x >= 174.0);
    let bytes = std::fs::read(&p).unwrap();
    check(open_bytes("grid.parquet", bytes, None).unwrap().as_ref(), true);

    let p2 = d.join("plain.parquet");
    write_parquet(&p2, false);
    check(open_path(&p2, None).unwrap().as_ref(), false);
}

/// Rewrites a valid file's footer, preserving its pages and schema.
fn parquet_with_bad_chunk(original: &[u8], column: usize, kind: usize) -> Vec<u8> {
    use ::parquet::file::reader::{FileReader, SerializedFileReader};
    let reader = SerializedFileReader::new(bytes::Bytes::copy_from_slice(original)).unwrap();
    let md = reader.metadata().clone();
    let mut groups = md.row_groups().to_vec();
    let mut cols = groups[0].columns().to_vec();
    let data = cols[column].data_page_offset();
    let builder = cols[column].clone().into_builder();
    cols[column] = match kind {
        0 => builder.set_dictionary_page_offset(Some(-1)),
        1 => builder.set_dictionary_page_offset(None).set_data_page_offset(-1),
        2 => builder.set_total_compressed_size(-1),
        3 => builder.set_dictionary_page_offset(Some(data + 1)),
        _ => builder.set_dictionary_page_offset(None).set_data_page_offset(i64::MAX).set_total_compressed_size(1),
    }
    .build()
    .unwrap();
    groups[0] = groups[0].clone().into_builder().set_column_metadata(cols).build().unwrap();
    let md = md.into_builder().set_row_groups(groups).build();
    let end = original.len() - 8;
    let len = u32::from_le_bytes(original[end..end + 4].try_into().unwrap()) as usize;
    let mut bytes = original[..end - len].to_vec();
    ::parquet::file::metadata::ParquetMetaDataWriter::new(&mut bytes, &md).finish().unwrap();
    bytes
}

#[test]
fn geoparquet_refuses_invalid_column_ranges() {
    let path = dir().join("bad-chunk.parquet");
    write_parquet(&path, true);
    let original = std::fs::read(&path).unwrap();
    // Attribute, geometry and bbox columns all pass through the same metadata check.
    for col in [0, 2, 3] {
        for kind in 0..5 {
            let bytes = parquet_with_bad_chunk(&original, col, kind);
            let error = open_bytes("bad.parquet", bytes.clone(), None).err().unwrap().to_string();
            assert!(error.contains("column chunk"), "{col}/{kind}: {error}");
            assert!(crate::io::read_layers("bad.parquet", &bytes).is_err());
            std::fs::write(&path, &bytes).unwrap();
            assert!(matches!(open_path(&path, None), Err(DataError::Format(_))));
            let url = format!("https://test.example/bad-chunk-{col}-{kind}.parquet");
            remote::testing::serve(&url, bytes);
            assert!(matches!(open_url_blocking(&url), Err(DataError::Format(_))));
        }
    }
}

/// A dictionary page whose header says it holds two billion values is refused before the `parquet`
/// crate makes room for them (64 GB for a binary column; fuzzing, #1498). The page patched is the
/// file's last column chunk, so no other chunk moves.
#[test]
fn geoparquet_refuses_dictionary_counts_its_page_cannot_hold() {
    use ::parquet::file::reader::{FileReader, SerializedFileReader};
    let path = dir().join("dictionary-count.parquet");
    write_parquet(&path, false);
    let original = std::fs::read(&path).unwrap();
    assert!(crate::io::read_layers("x.parquet", &original).is_ok());
    let md = SerializedFileReader::new(bytes::Bytes::copy_from_slice(&original)).unwrap().metadata().clone();
    let mut groups = md.row_groups().to_vec();
    let last = groups.len() - 1;
    let mut cols = groups[last].columns().to_vec();
    let geometry = cols.len() - 1;
    let start = cols[geometry].dictionary_page_offset().unwrap() as usize;
    // In the page header, the dictionary page header (field 7, a struct) and its first field, the
    // count (an i32 varint).
    let at = start + original[start..].windows(2).position(|w| w == [0x4c, 0x15]).unwrap() + 2;
    let end = at + original[at..].iter().position(|b| b & 0x80 == 0).unwrap() + 1;
    let count = [0x80, 0xd0, 0xac, 0xf3, 0x0e]; // zigzag(2_000_000_000)
    let grown = (count.len() - (end - at)) as i64;
    let mut bytes = [&original[..at], &count, &original[end..start + cols[geometry].compressed_size() as usize]].concat();
    let size = cols[geometry].compressed_size() + grown;
    cols[geometry] = cols[geometry].clone().into_builder().set_total_compressed_size(size).build().unwrap();
    groups[last] = groups[last].clone().into_builder().set_column_metadata(cols).build().unwrap();
    let md = md.into_builder().set_row_groups(groups).build();
    ::parquet::file::metadata::ParquetMetaDataWriter::new(&mut bytes, &md).finish().unwrap();
    let error = crate::io::read_layers("x.parquet", &bytes).err().unwrap().to_string();
    assert!(error.contains("dictionary page says it holds 2000000000 values"), "{error}");
}

#[test]
fn flatgeobuf_refuses_feature_sizes_before_allocating() {
    let path = dir().join("bad-feature-size.fgb");
    write_fgb(&path);
    let original = std::fs::read(&path).unwrap();
    let h = flatgeobuf::size_prefixed_root_as_header(&original[8..]).unwrap();
    let index = flatgeobuf::packed_r_tree::PackedRTree::index_size(h.features_count() as usize, h.index_node_size());
    let start = 12 + u32::from_le_bytes(original[8..12].try_into().unwrap()) as usize + index;
    // Both the first prefix (read by selection) and a later prefix (read by next()).
    let second = start + 4 + u32::from_le_bytes(original[start..start + 4].try_into().unwrap()) as usize;
    for (which, offset) in [start, second].into_iter().enumerate() {
        let mut bytes = original.clone();
        bytes[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, &original).unwrap();
        let src = open_path(&path, None).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        for extent in [None, Some(Extent::WORLD_LONLAT)] {
            let error = src.read(&Request { extent, ..Request::all() }).unwrap_err().to_string();
            assert!(error.contains("feature size"), "{error}");
            let error = src.visit_chunks(extent, 1 << 20, &mut |_| Ok(())).unwrap_err().to_string();
            assert!(error.contains("feature size"), "{error}");
        }
        let src = open_bytes("bad.fgb", bytes.clone(), None).unwrap();
        assert!(src.read(&Request::all()).is_err());
        assert!(src.visit_chunks(None, 1 << 20, &mut |_| Ok(())).is_err());
        assert!(crate::io::read_layers("bad.fgb", &bytes).is_err());
        let url = format!("https://test.example/bad-feature-{which}.fgb");
        remote::testing::serve(&url, bytes);
        let src = open_driven(&url);
        assert!(src.read(&Request::all()).is_err());
    }
}

#[test]
fn flatgeobuf_refuses_impossible_index_counts() {
    let path = dir().join("impossible-index.fgb");
    write_fgb(&path);
    let original = std::fs::read(&path).unwrap();
    let header = flatgeobuf::size_prefixed_root_as_header(&original[8..]).unwrap();
    let slot = header._tab.vtable().get(flatgeobuf::Header::VT_FEATURES_COUNT) as usize;
    assert_ne!(slot, 0);
    let offset = 8 + header._tab.loc() + slot;
    for count in [1u64 << 63, u64::MAX, (1u64 << 40) + 1] {
        let mut bytes = original.clone();
        bytes[offset..offset + 8].copy_from_slice(&count.to_le_bytes());
        assert!(matches!(open_bytes("impossible.fgb", bytes.clone(), None), Err(DataError::Format(_))));
        assert!(crate::io::read_layers("impossible.fgb", &bytes).is_err());
        std::fs::write(&path, &original).unwrap();
        let opened = open_path(&path, None).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(open_path(&path, None), Err(DataError::Format(_))));
        // A path is reopened on every request: validate again if its file changed.
        for extent in [None, Some(Extent::WORLD_LONLAT)] {
            assert!(matches!(opened.read(&Request { extent, ..Request::all() }), Err(DataError::Format(_))));
        }
        let url = format!("https://test.example/impossible-index-{count}.fgb");
        remote::testing::serve(&url, bytes);
        assert!(matches!(open_url_blocking(&url), Err(DataError::Format(_))));
    }
}

#[test]
fn flatgeobuf() {
    let p = dir().join("grid.fgb");
    write_fgb(&p);
    let src = open_path(&p, None).unwrap();
    check(src.as_ref(), true);
    assert_eq!(src.info().name, "grid");
    check(open_bytes("grid.fgb", std::fs::read(&p).unwrap(), None).unwrap().as_ref(), true);
}

#[test]
fn geopackage_reprojects_and_uses_its_rtree() {
    let d = dir();
    let p = d.join("grid.gpkg");
    write_gpkg(&p, true);
    assert_eq!(layers(&p).unwrap(), vec!["grid \"q\"".to_string()]);
    let src = open_path(&p, Some("GRID \"Q\"")).unwrap();
    assert_eq!(src.info().crs, "EPSG:3857");
    check(src.as_ref(), true);
    // Booleans from integers.
    let all = src.read(&Request::all()).unwrap();
    let ok = src.info().fields.iter().position(|f| f.name == "ok").unwrap();
    assert!(all.features.iter().any(|f| f.attrs[ok] == Value::Bool(true)));
    check(open_bytes("grid.gpkg", std::fs::read(&p).unwrap(), None).unwrap().as_ref(), true);

    let p2 = d.join("scan.gpkg");
    write_gpkg(&p2, false);
    check(open_path(&p2, None).unwrap().as_ref(), false);
    assert!(open_path(&p2, Some("missing")).is_err());
}

/// The browser's GeoPackage reader (pure Rust) gives what SQLite gives, without the R-tree.
#[test]
fn geopackage_in_memory() {
    let p = dir().join("mem.gpkg");
    write_gpkg(&p, true);
    let bytes = Shared(Arc::new(std::fs::read(&p).unwrap()));
    assert_eq!(gpkg_mem::layers(bytes.clone()).unwrap(), vec!["grid \"q\"".to_string()]);
    let src = gpkg_mem::GpkgMemSource::open_shared(bytes.clone(), Some("GRID \"Q\"")).unwrap();
    assert_eq!(src.info().crs, "EPSG:3857");
    check(&src, false);
    // The same features, ids and values as SQLite reads.
    let sql = open_path(&p, None).unwrap();
    let (mut a, mut b) = (src.read(&Request::all()).unwrap().features, sql.read(&Request::all()).unwrap().features);
    a.sort_by_key(|f| f.id);
    b.sort_by_key(|f| f.id);
    assert_eq!(a, b);
    // SQLite's R-tree keeps 32-bit floats; the headers' envelopes are exact.
    let (e, f) = (src.info().extent, sql.info().extent);
    assert!([e.min_x - f.min_x, e.min_y - f.min_y, e.max_x - f.max_x, e.max_y - f.max_y].iter().all(|d| d.abs() < 1e-4), "{e:?} {f:?}");
    assert!(gpkg_mem::GpkgMemSource::open_shared(bytes.clone(), Some("missing")).is_err());
    // Every truncation is an error or a short read, never a panic.
    for n in (0..bytes.0.len()).step_by(131) {
        if let Ok(s) = gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(bytes.0[..n].to_vec())), None) {
            let _ = s.read(&Request::all());
        }
    }
}

#[test]
fn geopackage_in_memory_handles_mutated_headers() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("mutated.gpkg");
    write_gpkg(&path, false);
    let original = std::fs::read(path).unwrap();
    let mut offsets: Vec<usize> = (0..100).collect();
    // SQLite pages have a short header; exercise the beginning of each one.
    for page in (4096..original.len()).step_by(4096) {
        offsets.extend(page..(page + 12).min(original.len()));
    }
    // Exercise flags and envelope metadata in actual GeoPackage geometry blobs.
    let geometries: Vec<_> =
        original.windows(4).enumerate().filter_map(|(i, bytes)| (bytes == b"GP\x00\x03").then_some(i)).take(3).collect();
    assert_eq!(geometries.len(), 3);
    for i in geometries {
        offsets.extend(i..i + 8);
    }
    offsets.sort_unstable();
    offsets.dedup();
    for offset in offsets {
        let mut bytes = original.clone();
        bytes[offset] ^= 0xff;
        if let Ok(source) = gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(bytes)), None) {
            let _ = source.read(&Request::all());
        }
    }
}

#[test]
fn an_unknown_coordinate_system_is_reported() {
    let p = dir().join("unknown_crs.gpkg");
    write_gpkg(&p, true);
    {
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute("UPDATE gpkg_spatial_ref_sys SET organization_coordsys_id = 9999999, srs_id = 3857", []).unwrap();
    }
    let src = open_path(&p, None).unwrap();
    assert!(src.info().notes.iter().any(|n| n.contains("EPSG:9999999")), "{:?}", src.info().notes);
    let (_, report) = materialize(src.as_ref()).unwrap();
    assert!(report.messages.iter().any(|m| m.contains("read as longitude/latitude")), "{:?}", report.messages);
}

/// MAGNA-SIRGAS / Origen-Nacional (EPSG:9377) as a GeoPackage writer may define it: no authority.
const ORIGEN_NACIONAL: &str = r#"PROJCS["MAGNA-SIRGAS / Origen-Nacional",GEOGCS["MAGNA-SIRGAS",DATUM["Marco_Geocentrico_Nacional_de_Referencia",SPHEROID["GRS 1980",6378137.0,298.257222101]],PRIMEM["Greenwich",0.0],UNIT["degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["false_easting",5000000.0],PARAMETER["false_northing",2000000.0],PARAMETER["central_meridian",-73.0],PARAMETER["scale_factor",0.9992],PARAMETER["latitude_of_origin",4.0],UNIT["Metre",1.0]]"#;

/// A GeoPackage with one point in srs_id 300001, defined by `organization`, `code` and
/// `definition`, at Origen-Nacional metres (4 874 000, 2 580 000).
fn write_custom_srs_gpkg(path: &Path, organization: &str, code: i64, definition: &str) {
    let _ = std::fs::remove_file(path);
    let c = rusqlite::Connection::open(path).unwrap();
    c.execute_batch(
        "CREATE TABLE gpkg_spatial_ref_sys (srs_name TEXT, srs_id INTEGER PRIMARY KEY, organization TEXT, organization_coordsys_id INTEGER, definition TEXT, description TEXT);
         CREATE TABLE gpkg_contents (table_name TEXT PRIMARY KEY, data_type TEXT, identifier TEXT, description TEXT, last_change TEXT, min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE, srs_id INTEGER);
         CREATE TABLE gpkg_geometry_columns (table_name TEXT, column_name TEXT, geometry_type_name TEXT, srs_id INTEGER, z TINYINT, m TINYINT);
         INSERT INTO gpkg_contents (table_name, data_type, srs_id) VALUES ('via', 'features', 300001);
         INSERT INTO gpkg_geometry_columns VALUES ('via', 'geom', 'POINT', 300001, 0, 0);
         CREATE TABLE via (fid INTEGER PRIMARY KEY AUTOINCREMENT, geom BLOB);",
    )
    .unwrap();
    c.execute(
        "INSERT INTO gpkg_spatial_ref_sys VALUES ('Custom', 300001, ?1, ?2, ?3, '')",
        rusqlite::params![organization, code, definition],
    )
    .unwrap();
    let p = Coord::new(4_874_000.0, 2_580_000.0);
    let mut blob = b"GP\x00\x01".to_vec();
    blob.extend_from_slice(&300_001i32.to_le_bytes());
    blob.extend(wkb::encode(&Geometry::Point(p)));
    c.execute("INSERT INTO via (geom) VALUES (?1)", [blob]).unwrap();
}

/// The point of [`write_custom_srs_gpkg`] and the notes, from the desktop and the browser readers.
fn custom_srs_reads(path: &Path) -> Vec<(Option<Geometry>, Vec<String>)> {
    let desktop = open_path(path, None).unwrap();
    let browser = gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(path).unwrap())), None).unwrap();
    [desktop.as_ref(), &browser as &dyn FeatureSource]
        .into_iter()
        .map(|src| {
            let batch = src.read(&Request::all()).unwrap();
            (
                lonlat_of(src, batch.features.first().and_then(|f| f.geometry.clone())),
                src.info().notes.texts().map(str::to_string).collect(),
            )
        })
        .collect()
}

#[test]
fn a_gpkg_system_without_an_epsg_code_is_read_from_its_definition() {
    // #1476: a custom row (organization CUSTOM, code 0) with the WKT of Origen-Nacional.
    let p = dir().join("origen_nacional.gpkg");
    write_custom_srs_gpkg(&p, "CUSTOM", 0, ORIGEN_NACIONAL);
    for (geometry, notes) in custom_srs_reads(&p) {
        let Some(Geometry::Point(c)) = geometry else { panic!("{geometry:?}") };
        // Northern Colombia, where EPSG:9377 puts those metres.
        let expected =
            crate::raster::Crs::from_epsg(9377).unwrap().transformer().unwrap().to_lonlat(Coord::new(4_874_000.0, 2_580_000.0)).unwrap();
        assert!((c.x - expected.x).abs() < 1e-6 && (c.y - expected.y).abs() < 1e-6, "{c:?} vs {expected:?}");
        assert!((-74.3..-74.0).contains(&c.x) && (9.1..9.4).contains(&c.y), "{c:?}");
        // Nothing to say: MAGNA-SIRGAS needs no datum shift.
        assert!(notes.is_empty(), "{notes:?}");
    }
    // The layer keeps that system as its own (Layer Properties, Define Projection, exports), in a
    // whole read too, on both readers.
    let desktop = open_path(&p, None).unwrap();
    let browser = gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(&p).unwrap())), None).unwrap();
    for src in [desktop.as_ref(), &browser as &dyn FeatureSource] {
        let crs = src.info().coordinate_system();
        assert!(crs.is_some_and(|c| c.custom_def().is_some_and(|d| d.proj4.contains("+proj=tmerc"))), "{:?}", src.info().crs);
        assert_eq!(materialize(src).unwrap().0.crs, crs);
    }
    drop((desktop, browser));
    // A row without a definition is still said to be read as longitude/latitude.
    write_custom_srs_gpkg(&p, "NONE", 300001, "undefined");
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.iter().any(|n| n.contains("srs_id 300001") && n.contains("has no EPSG code")), "{notes:?}");
    }
    // No row for the id at all: said too.
    rusqlite::Connection::open(&p).unwrap().execute("DELETE FROM gpkg_spatial_ref_sys WHERE srs_id = 300001", []).unwrap();
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.iter().any(|n| n.contains("srs_id 300001") && n.contains("has no EPSG code")), "{notes:?}");
    }
    // A writer's "undefined …" text defines nothing: the same note, not a parse failure.
    write_custom_srs_gpkg(&p, "NONE", 300001, "undefined geographic coordinate system");
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.iter().any(|n| n.contains("has no EPSG code")) && !notes.iter().any(|n| n.contains("can't be read")), "{notes:?}");
    }
    // A definition that can't be read says so, and why.
    write_custom_srs_gpkg(&p, "CUSTOM", 0, "PROJCS[");
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.iter().any(|n| n.contains("srs_id 300001") && n.contains("can't be read")), "{notes:?}");
    }
    // The spec's undefined ids: -1 (cartesian) is assumed longitude/latitude, said; 0
    // (geographic) is longitude/latitude, nothing to say.
    for (id, said) in [(-1, true), (0, false)] {
        write_custom_srs_gpkg(&p, "NONE", 300001, "undefined");
        rusqlite::Connection::open(&p)
            .unwrap()
            .execute_batch(&format!(
                "UPDATE gpkg_spatial_ref_sys SET srs_id = {id}, organization_coordsys_id = {id};
                 UPDATE gpkg_contents SET srs_id = {id};
                 UPDATE gpkg_geometry_columns SET srs_id = {id};"
            ))
            .unwrap();
        for (_, notes) in custom_srs_reads(&p) {
            assert_eq!(
                notes.iter().any(|n| n.contains(&format!("srs_id {id})")) && n.contains("has no EPSG code")),
                said,
                "{id}: {notes:?}"
            );
        }
    }
    // A row naming neither organization nor code but defining the system is read by its
    // definition, not as the system its id happens to be.
    write_custom_srs_gpkg(&p, "NONE", 0, ORIGEN_NACIONAL);
    rusqlite::Connection::open(&p)
        .unwrap()
        .execute_batch(
            "UPDATE gpkg_spatial_ref_sys SET srs_id = 3857, organization = NULL, organization_coordsys_id = NULL;
             UPDATE gpkg_contents SET srs_id = 3857;
             UPDATE gpkg_geometry_columns SET srs_id = 3857;",
        )
        .unwrap();
    for (geometry, notes) in custom_srs_reads(&p) {
        assert!(matches!(geometry, Some(Geometry::Point(c)) if (-74.3..-74.0).contains(&c.x)), "{geometry:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }
    // MAGNA-SIRGAS on an ellipsoid other than its own keeps that ellipsoid, and PROJ 9.8.1's
    // MAGNA-SIRGAS to WGS 84 (1), a null transformation on it: nothing to say.
    write_custom_srs_gpkg(
        &p,
        "CUSTOM",
        0,
        &ORIGEN_NACIONAL.replace(r#"SPHEROID["GRS 1980",6378137.0,298.257222101]"#, r#"SPHEROID["International 1924",6378388.0,297.0]"#),
    );
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.is_empty(), "{notes:?}");
    }
    // EPSG code 0 is no code: said, not read as longitude/latitude in silence.
    write_custom_srs_gpkg(&p, "EPSG", 0, "undefined");
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.iter().any(|n| n.contains("srs_id 300001") && n.contains("has no EPSG code")), "{notes:?}");
    }
    // A row naming neither organization nor code leaves Web Mercator to its id, as a missing row.
    write_custom_srs_gpkg(&p, "NONE", 0, "undefined");
    rusqlite::Connection::open(&p)
        .unwrap()
        .execute_batch(
            "UPDATE gpkg_spatial_ref_sys SET srs_id = 3857, organization = NULL, organization_coordsys_id = NULL;
             UPDATE gpkg_contents SET srs_id = 3857;
             UPDATE gpkg_geometry_columns SET srs_id = 3857;",
        )
        .unwrap();
    for (geometry, notes) in custom_srs_reads(&p) {
        // Those metres in Web Mercator: about 43.8° E, 22.6° N.
        assert!(matches!(geometry, Some(Geometry::Point(c)) if (43.0..45.0).contains(&c.x) && (22.0..23.0).contains(&c.y)), "{geometry:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }
    // An EPSG code stored as a REAL (9377.0, in a column a writer declared DOUBLE) is that code on
    // both readers, with no definition.
    write_custom_srs_gpkg(&p, "EPSG", 0, "undefined");
    rusqlite::Connection::open(&p)
        .unwrap()
        .execute_batch(
            "DROP TABLE gpkg_spatial_ref_sys;
             CREATE TABLE gpkg_spatial_ref_sys (srs_name TEXT, srs_id INTEGER PRIMARY KEY, organization TEXT, organization_coordsys_id DOUBLE, definition TEXT, description TEXT);
             INSERT INTO gpkg_spatial_ref_sys VALUES ('Origen-Nacional', 300001, 'EPSG', 9377.0, 'undefined', '');",
        )
        .unwrap();
    for (geometry, notes) in custom_srs_reads(&p) {
        assert!(matches!(geometry, Some(Geometry::Point(c)) if (-74.3..-74.0).contains(&c.x)), "{geometry:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }
    // An EPSG code this build doesn't know falls back to the definition.
    write_custom_srs_gpkg(&p, "EPSG", 9_999_999, ORIGEN_NACIONAL);
    for (geometry, notes) in custom_srs_reads(&p) {
        assert!(matches!(geometry, Some(Geometry::Point(c)) if (-74.3..-74.0).contains(&c.x)), "{geometry:?}");
        assert!(notes.is_empty(), "{notes:?}");
    }
}

/// [`write_custom_srs_gpkg`] with its point at `xy` and, when given, `wkt2` in the column
/// GeoPackage 1.2's `gpkg_crs_wkt` extension adds for WKT2, `definition_12_063`.
fn write_wkt2_srs_gpkg(path: &Path, definition: &str, wkt2: Option<&str>, xy: Coord) {
    write_custom_srs_gpkg(path, "CUSTOM", 0, definition);
    let c = rusqlite::Connection::open(path).unwrap();
    let mut blob = b"GP\x00\x01".to_vec();
    blob.extend_from_slice(&300_001i32.to_le_bytes());
    blob.extend(wkb::encode(&Geometry::Point(xy)));
    c.execute("UPDATE via SET geom = ?1", [blob]).unwrap();
    if let Some(wkt2) = wkt2 {
        c.execute("ALTER TABLE gpkg_spatial_ref_sys ADD COLUMN definition_12_063 TEXT", []).unwrap();
        c.execute("UPDATE gpkg_spatial_ref_sys SET definition_12_063 = ?1", [wkt2]).unwrap();
    }
}

/// The point a reader read, said to be within a centimetre of `lonlat`.
fn assert_placed(what: &str, geometry: Option<&Geometry>, lonlat: Coord) {
    let Some(Geometry::Point(c)) = geometry else { panic!("{what}: {geometry:?}") };
    let off = metres_apart(*c, lonlat);
    assert!(off < 0.01, "{what}: {off} m off");
}

/// #1486: the placement cases, each read from a Shapefile's `.prj` (in place and whole), a
/// GeoPackage's definition and a FlatGeobuf header's WKT without a code (#1855), on the desktop and
/// in the browser: WKT1 in `definition`, WKT2 in `definition_12_063`.
#[test]
fn placement_cases_are_placed_from_a_prj_a_geopackage_and_a_flatgeobuf() {
    let d = dir().join("placement");
    std::fs::create_dir_all(&d).unwrap();
    for (i, PlacementCase { what, wkt, xy, lonlat }) in placement_cases().into_iter().enumerate() {
        let p = d.join(format!("case{i}.shp"));
        let mut ds = Dataset::new("case", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        ds.push(Some(Geometry::Point(xy)), vec![Value::Text(what.into())]);
        write_shapefile(&ds, &p);
        std::fs::write(p.with_extension("prj"), &wkt).unwrap();
        let src = open_path(&p, None).unwrap();
        let read = in_lonlat(src.as_ref(), src.read(&Request::all()).unwrap());
        assert_placed(what, read.features.first().and_then(|f| f.geometry.as_ref()), lonlat);
        assert!(src.info().notes.is_empty(), "{what}: {:?}", src.info().notes);
        let (whole, report) = crate::io::read_path(&p).unwrap();
        // Held as the file holds it, in the system its `.prj` gives.
        let whole = whole.in_lonlat().unwrap();
        assert_placed(what, whole.features.first().and_then(|f| f.geometry.as_ref()), lonlat);
        assert!(report.messages.is_empty(), "{what}: {:?}", report.messages);

        let g = d.join(format!("case{i}.gpkg"));
        if wkt.starts_with("PROJCS[") || wkt.starts_with("GEOGCS[") {
            write_wkt2_srs_gpkg(&g, &wkt, None, xy);
        } else {
            write_wkt2_srs_gpkg(&g, "undefined", Some(&wkt), xy);
        }
        for (geometry, notes) in custom_srs_reads(&g) {
            assert_placed(what, geometry.as_ref(), lonlat);
            assert!(notes.is_empty(), "{what}: {notes:?}");
        }

        let f = d.join(format!("case{i}.fgb"));
        write_header_fgb(&f, None, 0, Some(&wkt), xy);
        for (geometry, notes) in fgb_reads(&f) {
            assert_placed(what, geometry.as_ref(), lonlat);
            assert!(notes.is_empty(), "{what}: {notes:?}");
        }
    }
}

/// A FlatGeobuf of one point at `xy` whose header names its system by `org` and `code`, with
/// `wkt`.
fn write_header_fgb(path: &Path, org: Option<&str>, code: i32, wkt: Option<&str>, xy: Coord) {
    use flatgeobuf::{FgbCrs, FgbWriter, FgbWriterOptions, GeometryType as G};
    let options = FgbWriterOptions { crs: FgbCrs { org, code, wkt, ..Default::default() }, ..Default::default() };
    let mut w = FgbWriter::create_with_options("point", G::Point, options).unwrap();
    w.add_feature_geom(flatgeobuf::geozero::wkb::Wkb(wkb::encode(&Geometry::Point(xy))), |_| {}).unwrap();
    w.write(std::fs::File::create(path).unwrap()).unwrap();
}

/// A FlatGeobuf's first geometry and notes, as the desktop opens the file and as the browser opens
/// its bytes.
fn fgb_reads(path: &Path) -> Vec<(Option<Geometry>, Vec<String>)> {
    let desktop = open_path(path, None).unwrap();
    let browser = open_shared("point.fgb", Arc::new(std::fs::read(path).unwrap()), None).unwrap();
    [desktop, browser]
        .into_iter()
        .map(|src| {
            let batch = src.read(&Request::all()).unwrap();
            (
                lonlat_of(src.as_ref(), batch.features.first().and_then(|f| f.geometry.clone())),
                src.info().notes.texts().map(str::to_string).collect(),
            )
        })
        .collect()
}

/// #1855: a FlatGeobuf header's system is read as a GeoPackage row's is, on the desktop and in the
/// browser. Its EPSG code is used when its WKT gives no other shift to WGS 84, and the WKT
/// otherwise, where a `.prj` of it places the point. A grid the WKT gives beside the code is said.
/// Without an EPSG code the WKT is read; one that can't be read is said. A code without a WKT is
/// read as before. A negative code without a WKT is longitude/latitude, said.
#[test]
fn a_flatgeobuf_headers_system_is_read_as_a_geopackage_rows_is() {
    use crate::raster::wkt::{read_wkt, to_wkt};
    let d = dir().join("fgb_header");
    std::fs::create_dir_all(&d).unwrap();
    let prj = |wkt: &str, xy: Coord| read_wkt(wkt).unwrap().0.transformer().unwrap().to_lonlat(xy).unwrap();
    let mut n = 0;
    let mut read = |org: Option<&str>, code: i32, wkt: Option<&str>, xy: Coord| {
        n += 1;
        let p = d.join(format!("case{n}.fgb"));
        write_header_fgb(&p, org, code, wkt, xy);
        fgb_reads(&p)
    };
    // British National Grid, in England: its own shift is the code's; another `TOWGS84` puts the
    // point some 4 m away, where a `.prj` of it does.
    let xy = Coord::new(450_000.0, 250_000.0);
    let by_code = Crs::Epsg(27700).transformer().unwrap().to_lonlat(xy).unwrap();
    let bng = crate::raster::wkt::tests::bng_wkt1();
    let other = bng.replace(BNG_SHIFT, "TOWGS84[375,-111,431,0,0,0,0]");
    assert_ne!(other, bng);
    assert!(metres_apart(prj(&other, xy), by_code) > 1.0);
    for (org, wkt, lonlat) in [
        (Some("EPSG"), Some(bng.as_str()), by_code),
        (Some("EPSG"), Some(other.as_str()), prj(&other, xy)),
        (None, Some(&other), prj(&other, xy)),
        (Some("EPSG"), None, by_code),
    ] {
        for (geometry, notes) in read(org, 27700, wkt, xy) {
            assert_placed(&format!("{wkt:?}"), geometry.as_ref(), lonlat);
            assert!(notes.is_empty(), "{notes:?}");
        }
    }
    // Text that isn't WKT beside the code: the code, said.
    let e = read_wkt("PROJCS[").unwrap_err();
    for (geometry, notes) in read(Some("EPSG"), 27700, Some("PROJCS["), xy) {
        assert_placed("not WKT", geometry.as_ref(), by_code);
        assert_eq!(notes, [format!("its coordinate system: its definition can't be read ({e}), so EPSG:27700 is used")]);
    }
    // ED50 / UTM zone 31N bound by Spain's NTv2 grid: read by the code, whose own Helmert shift
    // stands in for the grid, said.
    let xy = Coord::new(430_000.0, 4_580_000.0);
    let by_code = Crs::Epsg(23031).transformer().unwrap().to_lonlat(xy).unwrap();
    for (geometry, notes) in read(Some("EPSG"), 23031, Some(&ed50_utm31_bound()), xy) {
        assert_placed("a grid", geometry.as_ref(), by_code);
        assert_eq!(
            notes,
            [
                "its coordinate system: the definition's NTv2 transformation to WGS 84 (SPED2ETV2.gsb) isn't applied; EPSG:23031's own shift to WGS 84 is used in its place, so positions may be off by the difference between the two"
            ]
        );
    }
    // Without an EPSG code: WGS 84 / UTM zone 33N, whose name says WGS 84, was read as
    // longitude/latitude with nothing said; it is read by its WKT. So is another organization's
    // system. One that can't be read is longitude/latitude, said.
    let xy = Coord::new(500_000.0, 5_800_000.0);
    let utm = to_wkt(Crs::Epsg(32633)).unwrap();
    assert!(utm.contains("WGS 84"), "{utm}");
    let utm = utm.replace(r#",AUTHORITY["EPSG","32633"]]"#, "]");
    let lonlat = Crs::Epsg(32633).transformer().unwrap().to_lonlat(xy).unwrap();
    for (org, code) in [(None, 0), (Some("EPSG"), 0), (Some("EPSG"), -5), (Some("ESRI"), 102_033)] {
        for (geometry, notes) in read(org, code, Some(&utm), xy) {
            assert_placed(&format!("{org:?} {code}"), geometry.as_ref(), lonlat);
            assert!(notes.is_empty(), "{notes:?}");
        }
    }
    for (geometry, notes) in read(None, 0, Some("PROJCS["), Coord::new(5.0, 50.0)) {
        assert_placed("lon/lat", geometry.as_ref(), Coord::new(5.0, 50.0));
        assert_eq!(notes, [format!("its coordinate system can't be read ({e}); read as longitude/latitude")]);
    }
    // Another organization's code without a WKT: longitude/latitude, said, as before.
    for (_, notes) in read(Some("ESRI"), 102_033, None, Coord::new(5.0, 50.0)) {
        assert_eq!(notes, ["its coordinate system is defined by ESRI, not EPSG; read as longitude/latitude"]);
    }
    // A negative code isn't an EPSG one: longitude/latitude, said. EPSG's 0 is no code, so nothing
    // is (a header without an organization is read in `flatgeobuf`).
    let lonlat = Coord::new(5.0, 50.0);
    for org in [None, Some("EPSG")] {
        for (geometry, notes) in read(org, -5, None, lonlat) {
            assert_placed("a negative code", geometry.as_ref(), lonlat);
            assert_eq!(notes, ["its coordinate system's code -5 isn't an EPSG code; read as longitude/latitude"]);
        }
    }
    for (geometry, notes) in read(Some("EPSG"), 0, None, lonlat) {
        assert_placed("no code", geometry.as_ref(), lonlat);
        assert!(notes.is_empty(), "{notes:?}");
    }
}

/// A GeoPackage system defined in WKT2, in `definition_12_063`, is read from it: when
/// `definition` is `undefined`, empty or missing, and over a WKT1 `definition` too (OGC 12-128r19
/// F.10). An EPSG code still comes first.
#[test]
fn a_gpkg_system_defined_in_wkt2_is_read_from_it() {
    let p = dir().join("wkt2_definition.gpkg");
    // Origen-Nacional (EPSG:9377) in WKT2, as PROJ writes it, without its ID.
    let wkt2 = r#"PROJCRS["MAGNA-SIRGAS / Origen-Nacional",BASEGEOGCRS["MAGNA-SIRGAS",DATUM["Marco Geocentrico Nacional de Referencia",ELLIPSOID["GRS 1980",6378137,298.257222101,LENGTHUNIT["metre",1]]],PRIMEM["Greenwich",0,ANGLEUNIT["degree",0.0174532925199433]]],CONVERSION["Colombia Transverse Mercator",METHOD["Transverse Mercator",ID["EPSG",9807]],PARAMETER["Latitude of natural origin",4,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8801]],PARAMETER["Longitude of natural origin",-73,ANGLEUNIT["degree",0.0174532925199433],ID["EPSG",8802]],PARAMETER["Scale factor at natural origin",0.9992,SCALEUNIT["unity",1],ID["EPSG",8805]],PARAMETER["False easting",5000000,LENGTHUNIT["metre",1],ID["EPSG",8806]],PARAMETER["False northing",2000000,LENGTHUNIT["metre",1],ID["EPSG",8807]]],CS[Cartesian,2],AXIS["northing (N)",north,ORDER[1],LENGTHUNIT["metre",1]],AXIS["easting (E)",east,ORDER[2],LENGTHUNIT["metre",1]]]"#;
    let xy = Coord::new(4_874_000.0, 2_580_000.0);
    let expected = crate::raster::Crs::from_epsg(9377).unwrap().transformer().unwrap().to_lonlat(xy).unwrap();
    let placed = |p: &Path| {
        for (geometry, notes) in custom_srs_reads(p) {
            assert_placed("Origen-Nacional", geometry.as_ref(), expected);
            assert!(notes.is_empty(), "{notes:?}");
        }
    };
    for definition in ["undefined", "", "  "] {
        write_wkt2_srs_gpkg(&p, definition, Some(wkt2), xy);
        placed(&p);
    }
    // A null definition, and none at all.
    write_wkt2_srs_gpkg(&p, "undefined", Some(wkt2), xy);
    rusqlite::Connection::open(&p).unwrap().execute("UPDATE gpkg_spatial_ref_sys SET definition = NULL", []).unwrap();
    placed(&p);
    rusqlite::Connection::open(&p).unwrap().execute("ALTER TABLE gpkg_spatial_ref_sys DROP COLUMN definition", []).unwrap();
    placed(&p);
    // Neither defines it: longitude/latitude, said.
    write_wkt2_srs_gpkg(&p, "undefined", Some("undefined"), xy);
    for (_, notes) in custom_srs_reads(&p) {
        assert!(notes.iter().any(|n| n.contains("srs_id 300001") && n.contains("has no EPSG code")), "{notes:?}");
    }
    // The WKT2 takes priority over a WKT1 definition: Web Mercator's, here, which would put the
    // point near 44°E.
    let mercator = crate::raster::wkt::to_wkt(crate::raster::Crs::WebMercator).unwrap();
    write_wkt2_srs_gpkg(&p, &mercator, Some(wkt2), xy);
    placed(&p);
    // A WKT2 that can't be read gives way to the WKT1, said.
    write_wkt2_srs_gpkg(&p, &mercator, Some(r#"PROJCRS["x",CONVERSION["unknown"]]"#), xy);
    for (geometry, notes) in custom_srs_reads(&p) {
        assert!(matches!(geometry, Some(Geometry::Point(c)) if (43.0..45.0).contains(&c.x)), "{geometry:?}");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("srs_id 300001") && notes[0].contains("WKT1 definition is used"), "{notes:?}");
    }
    // An EPSG code comes before both.
    write_wkt2_srs_gpkg(&p, "undefined", Some(&mercator), xy);
    rusqlite::Connection::open(&p)
        .unwrap()
        .execute("UPDATE gpkg_spatial_ref_sys SET organization = 'EPSG', organization_coordsys_id = 9377", [])
        .unwrap();
    placed(&p);
}

/// #1677: a GeoPackage row naming EPSG:27700 whose definition gives another shift to WGS 84 places
/// its point where the same WKT in a Shapefile's `.prj` does, on the desktop and in the browser,
/// not where the code alone would. So does one whose WKT2 is a `BOUNDCRS` with another shift. A
/// GeoPackage RustGIS writes is read back by its code, with nothing to say.
#[test]
fn a_geopackage_definitions_shift_places_points_as_a_prj_does() {
    use crate::raster::wkt::tests::{WGS84_WKT2, bound_crs};
    use crate::raster::wkt::to_wkt;
    let d = dir().join("code_and_shift");
    std::fs::create_dir_all(&d).unwrap();
    // Where a Shapefile with `prj` puts `xy`.
    let from_prj = |name: &str, prj: &str, xy: Coord| {
        let p = d.join(format!("{name}.shp"));
        let mut ds = Dataset::new(name, GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        ds.push(Some(Geometry::Point(xy)), vec![Value::Text(name.into())]);
        write_shapefile(&ds, &p);
        std::fs::write(p.with_extension("prj"), prj).unwrap();
        let src = open_path(&p, None).unwrap();
        assert!(src.info().notes.is_empty(), "{name}: {:?}", src.info().notes);
        match lonlat_of(src.as_ref(), src.read(&Request::all()).unwrap().features.first().and_then(|f| f.geometry.clone())) {
            Some(Geometry::Point(c)) => c,
            g => panic!("{name}: {g:?}"),
        }
    };
    // A GeoPackage whose row names EPSG `code`, with its point at `xy`.
    let geopackage = |name: &str, code: i64, definition: &str, wkt2: Option<&str>, xy: Coord| {
        let p = d.join(format!("{name}.gpkg"));
        write_wkt2_srs_gpkg(&p, definition, wkt2, xy);
        rusqlite::Connection::open(&p)
            .unwrap()
            .execute("UPDATE gpkg_spatial_ref_sys SET organization = 'EPSG', organization_coordsys_id = ?1", [code])
            .unwrap();
        p
    };
    let bng = crate::raster::wkt::tests::bng_wkt1();
    let other = bng.replace(BNG_SHIFT, "TOWGS84[375,-111,431,0,0,0,0]");
    assert_ne!(other, bng);
    let xy = Coord::new(450_000.0, 250_000.0);
    let by_code = Crs::Epsg(27700).transformer().unwrap().to_lonlat(xy).unwrap();
    // Some 4 m from where the code puts it.
    let shifted = from_prj("bng_other", &other, xy);
    assert!(metres_apart(shifted, by_code) > 1.0, "{shifted:?} {by_code:?}");
    for (geometry, notes) in custom_srs_reads(&geopackage("bng_other", 27700, &other, None, xy)) {
        assert_placed("another TOWGS84", geometry.as_ref(), shifted);
        assert!(notes.is_empty(), "{notes:?}");
    }
    assert!(metres_apart(from_prj("bng", &bng, xy), by_code) < 0.001);
    for (geometry, notes) in custom_srs_reads(&geopackage("bng", 27700, &bng, None, xy)) {
        assert_placed("the code's own TOWGS84", geometry.as_ref(), by_code);
        assert!(notes.is_empty(), "{notes:?}");
    }
    // ED50 / UTM zone 31N, near Barcelona.
    let xy = Coord::new(430_000.0, 4_580_000.0);
    let bound = bound_crs(ED50_UTM31, WGS84_WKT2, &[-100.0, -100.0, -100.0]);
    let shifted = from_prj("ed50_bound", &bound, xy);
    let by_code = Crs::Epsg(23031).transformer().unwrap().to_lonlat(xy).unwrap();
    assert!(metres_apart(shifted, by_code) > 1.0, "{shifted:?} {by_code:?}");
    let ed50 = to_wkt(Crs::Epsg(23031)).unwrap();
    for (geometry, notes) in custom_srs_reads(&geopackage("ed50_bound", 23031, &ed50, Some(&bound), xy)) {
        assert_placed("a BOUNDCRS's shift", geometry.as_ref(), shifted);
        assert!(notes.is_empty(), "{notes:?}");
    }
    // A `BOUNDCRS` to a system whose shift to WGS 84 isn't known can't be read: the point is
    // where the code puts it, and each reader says that the definition's shift isn't used.
    let unknown = bound_crs(ED50_UTM31, NAD27_WKT2, &[-100.0, -100.0, -100.0]);
    for (name, definition, said) in [
        ("ed50_unknown", "undefined", "the file's own shift to WGS 84 is set aside for EPSG:23031's, because the shift from NAD27"),
        ("ed50_unknown_wkt1", ed50.as_str(), "its WKT2 definition can't be read (the shift from NAD27"),
    ] {
        for (geometry, notes) in custom_srs_reads(&geopackage(name, 23031, definition, Some(&unknown), xy)) {
            assert_placed(name, geometry.as_ref(), by_code);
            assert_eq!(notes.len(), 1, "{name}: {notes:?}");
            assert!(notes[0].contains(said), "{name}: {notes:?}");
        }
    }
    // As RustGIS writes them: `definition` is the system's WKT, with the code's own shift.
    for code in [27700, 23031, 31467, 2056, 32631, 3857] {
        let crs = Crs::from_epsg(code).unwrap();
        let p = d.join(format!("written_{code}.gpkg"));
        let _ = std::fs::remove_file(&p);
        let xy = crs.transformer().unwrap().from_lonlat(Coord::new(5.0, 47.0)).unwrap();
        let mut ds = Dataset::new("written", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        ds.push(Some(Geometry::Point(xy)), vec![Value::Text("p".into())]);
        gpkg::write_in(&ds, &p, crs).unwrap();
        let desktop = open_path(&p, None).unwrap();
        let browser = gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(&p).unwrap())), None).unwrap();
        for src in [desktop.as_ref(), &browser as &dyn FeatureSource] {
            assert_eq!(src.info().coordinate_system(), Some(crs), "EPSG:{code}");
            assert!(src.info().notes.is_empty(), "EPSG:{code}: {:?}", src.info().notes);
        }
    }
}

#[test]
fn flatgeobuf_round_trips_a_dataset() {
    let mut ds = Dataset::new("mixed", GeometryType::Polygon, vec![Field::new("n", FieldType::Integer), Field::new("s", FieldType::Text)]);
    ds.fields.push(Field::new("ok", FieldType::Boolean));
    ds.fields.push(Field::new("x", FieldType::Double));
    let sq = |x: f64| vec![vec![Coord::new(x, 0.0), Coord::new(x + 1.0, 0.0), Coord::new(x + 1.0, 1.0), Coord::new(x, 0.0)]];
    ds.push(Some(Geometry::Polygon(vec![sq(0.0)])), vec![Value::Int(7), Value::Text("a".into()), Value::Bool(true), Value::Float(1.5)]);
    ds.push(Some(Geometry::Polygon(vec![sq(5.0), sq(9.0)])), vec![Value::Null, Value::Text("b".into()), Value::Null, Value::Null]);
    let p = dir().join("rt.fgb");
    crate::io::write_path(&ds, &p).unwrap();
    let (back, _) = crate::io::read_path(&p).unwrap();
    assert_eq!(back.features.len(), 2);
    assert_eq!(back.fields.iter().map(|f| f.ty).collect::<Vec<_>>(), ds.fields.iter().map(|f| f.ty).collect::<Vec<_>>());
    let a = back.features.iter().find(|f| f.attrs[1] == Value::Text("a".into())).unwrap();
    assert_eq!(a.attrs, ds.features[0].attrs);
    let b = back.features.iter().find(|f| f.attrs[1] == Value::Text("b".into())).unwrap();
    assert_eq!(b.attrs[0], Value::Null);
    assert!(matches!(&b.geometry, Some(Geometry::Polygon(p)) if p.len() == 2));
}

#[test]
fn hostile_files_are_errors_not_panics() {
    let mut junk = vec![b"PAR1".to_vec(), b"fgb\x03fgb\x00".to_vec(), b"SQLite format 3\0".to_vec(), vec![0xff; 4096], Vec::new()];
    // Every truncation of a real file.
    let d = dir();
    let p = d.join("t.parquet");
    write_parquet(&p, true);
    let pq = std::fs::read(&p).unwrap();
    let f = d.join("t.fgb");
    write_fgb(&f);
    let fgb = std::fs::read(&f).unwrap();
    for n in (0..pq.len()).step_by(97) {
        junk.push(pq[..n].to_vec());
    }
    for n in (0..fgb.len()).step_by(89) {
        junk.push(fgb[..n].to_vec());
    }
    for bytes in junk {
        for name in ["x.parquet", "x.fgb", "x.gpkg"] {
            if let Ok(s) = open_bytes(name, bytes.clone(), None) {
                let _ = s.read(&Request::all());
                let _ = s.read(&Request { extent: Some(Extent::new(-10.0, -10.0, 10.0, 10.0)), limit: 3 });
            }
        }
    }
    assert!(open_path(Path::new("nope.shp"), None).is_err());
}

/// Reads every vector file in `RUSTGIS_SAMPLE_VECTORS` (the opengeos sample data, e.g.
/// <https://source.coop/giswqs/opengeos>): `cargo test -p rustgis-data -- --ignored vector_samples --nocapture`.
#[test]
#[ignore = "needs external vector samples in RUSTGIS_SAMPLE_VECTORS"]
fn vector_samples() {
    let Ok(dir) = std::env::var("RUSTGIS_SAMPLE_VECTORS") else { return };
    let mut paths: Vec<_> =
        std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| is_source_name(&p.to_string_lossy())).collect();
    paths.sort();
    assert!(!paths.is_empty());
    for p in paths {
        let names = match layers(&p) {
            Ok(n) => n,
            Err(e) => {
                println!("{}: {e}", p.display());
                continue;
            }
        };
        for layer in names {
            let t = std::time::Instant::now();
            let src = match open_path(&p, Some(&layer)) {
                Ok(s) => s,
                Err(e) => {
                    println!("{} [{layer}]: {e}", p.display());
                    continue;
                }
            };
            let i = src.info();
            let open = t.elapsed();
            let e = i.extent;
            let view = Extent::new(
                e.center().x - e.width() / 8.0,
                e.center().y - e.height() / 8.0,
                e.center().x + e.width() / 8.0,
                e.center().y + e.height() / 8.0,
            );
            let t2 = std::time::Instant::now();
            let b = match src.read(&Request { extent: Some(view), limit: 200_000 }) {
                Ok(b) => b,
                Err(e) => {
                    println!("{} [{layer}]: {e}", p.display());
                    continue;
                }
            };
            println!(
                "{} [{}] {} {:?} {} fields, {:?} features, {} indexed={} extent={:?} open {:?}; view: {} of {} (complete={}, skipped {}) in {:?}",
                p.file_name().unwrap().to_string_lossy(),
                i.name,
                i.format,
                i.geometry_type,
                i.fields.len(),
                i.count,
                i.crs,
                i.indexed,
                i.extent,
                open,
                b.features.len(),
                b.matched,
                b.complete,
                b.skipped,
                t2.elapsed()
            );
        }
    }
}

/// A polygon layer with every field type, a hole, a multipolygon, a feature without geometry,
/// long and clashing field names and non-ASCII text.
fn export_sample() -> Dataset {
    let mut ds = Dataset::new(
        "Parcels",
        GeometryType::Polygon,
        vec![
            Field::new("population_total", FieldType::Integer),
            Field::new("population_density", FieldType::Double),
            Field::new("name", FieldType::Text),
            Field::new("ok", FieldType::Boolean),
            Field::new("geometry", FieldType::Text),
        ],
    );
    let sq = |x: f64, y: f64, s: f64| {
        vec![Coord::new(x, y), Coord::new(x + s, y), Coord::new(x + s, y + s), Coord::new(x, y + s), Coord::new(x, y)]
    };
    ds.push(
        Some(Geometry::Polygon(vec![vec![sq(0.0, 0.0, 10.0), sq(2.0, 2.0, 2.0)]])),
        vec![Value::Int(1_234_567), Value::Float(12.5), Value::Text("Zürich — 東京".into()), Value::Bool(true), Value::Text("g".into())],
    );
    ds.push(
        Some(Geometry::Polygon(vec![vec![sq(20.0, 20.0, 1.0)], vec![sq(-30.0, -40.0, 5.0)]])),
        vec![Value::Int(-3), Value::Float(-0.25), Value::Null, Value::Bool(false), Value::Null],
    );
    ds.push(None, vec![Value::Null, Value::Null, Value::Text("no shape".into()), Value::Null, Value::Null]);
    ds
}

#[test]
fn writes_geopackage_geoparquet_and_shapefile() {
    let ds = export_sample();
    let workspace = tempfile::tempdir().unwrap();
    let d = workspace.path();
    for ext in ["gpkg", "parquet", "shp", "fgb"] {
        let p = d.join(format!("parcels.{ext}"));
        let left_out = crate::io::write_path(&ds, &p).unwrap();
        let holds_all = matches!(ext, "gpkg" | "parquet");
        assert_eq!(left_out, if holds_all { 0 } else { 1 }, "{ext}");
        let (back, _) = crate::io::read_path(&p).unwrap();
        // GeoParquet and GeoPackage keep a row without a shape; the other formats can't hold it.
        assert_eq!(back.features.len(), if holds_all { 3 } else { 2 }, "{ext}");
        assert_eq!(back.geometry_type, GeometryType::Polygon, "{ext}");
        let e = back.extent();
        assert_eq!((e.min_x, e.min_y, e.max_x, e.max_y), (-30.0, -40.0, 21.0, 21.0), "{ext}");
        let field = |name: &str| back.field_index(name).unwrap_or_else(|| panic!("{ext}: no field {name} in {:?}", back.fields));
        let pop = if ext == "shp" { field("population") } else { field("population_total") };
        let dens = if ext == "shp" { field("populati_1") } else { field("population_density") };
        let name = field("name");
        let first = back.features.iter().find(|f| f.attrs[name] == Value::Text("Zürich — 東京".into())).unwrap();
        assert_eq!(first.attrs[pop].as_f64(), Some(1_234_567.0), "{ext}");
        assert_eq!(first.attrs[dens].as_f64(), Some(12.5), "{ext}");
        assert!(matches!(&first.geometry, Some(Geometry::Polygon(p)) if p.len() == 1 && p[0].len() == 2), "{ext}: the hole is kept");
        let second = back.features.iter().find(|f| f.attrs[pop].as_f64() == Some(-3.0)).unwrap();
        assert!(matches!(&second.geometry, Some(Geometry::Polygon(p)) if p.len() == 2), "{ext}: both parts are kept");
        assert_eq!(second.attrs[name], Value::Null, "{ext}");
        if holds_all {
            // The clashing field keeps its name; the geometry column took another.
            assert_eq!(back.value(0, field("geometry")), &Value::Text("g".into()), "{ext}");
        }
    }
    // The Shapefile's companion files.
    for e in ["shx", "dbf", "prj", "cpg"] {
        assert!(d.join(format!("parcels.{e}")).exists(), "{e}");
    }
    // GeoPackage and GeoParquet keep feature ids and answer extent queries by their index.
    for ext in ["gpkg", "parquet"] {
        let s = open_path(&d.join(format!("parcels.{ext}")), None).unwrap();
        assert_eq!(s.info().count, Some(3), "{ext}");
        let b = s.read(&Request { extent: Some(Extent::new(15.0, 15.0, 25.0, 25.0)), limit: 100 }).unwrap();
        assert_eq!(b.features.len(), 1, "{ext}");
        if ext == "gpkg" {
            assert_eq!(b.features[0].id, 2);
        }
    }
    // Single-file formats can replace their old exports, but GeoPackages may hold other tables.
    let mut fewer = ds.clone();
    fewer.features.truncate(1);
    for ext in ["parquet", "shp"] {
        let p = d.join(format!("parcels.{ext}"));
        crate::io::write_path(&fewer, &p).unwrap();
        assert_eq!(crate::io::read_path(&p).unwrap().0.features.len(), 1, "{ext}");
    }
}

/// Whole reads retain null geometry and its attributes; spatial queries require a location.
#[test]
fn geoparquet_exports_keep_exact_values_and_remote_extent_reads() {
    let mut ds =
        Dataset::new("points", GeometryType::Point, vec![Field::new("bbox", FieldType::Text), Field::new("value", FieldType::Double)]);
    ds.push(None, vec![Value::Text("no shape".into()), Value::Float(-0.25)]);
    ds.push(
        Some(Geometry::Point(Coord::new(-89.12345678901234, 36.181582500000005))),
        vec![Value::Text("東京".into()), Value::Float(0.12345678901234568)],
    );
    ds.push(Some(Geometry::Point(Coord::new(1.2345678901234567, -12.345678901234567))), vec![Value::Null, Value::Null]);
    let bytes = parquet::write(&ds).unwrap();
    let local = open_bytes("exact.parquet", bytes.clone(), None).unwrap();
    assert_eq!(local.info().count, Some(3));
    assert_eq!(local.info().fields, ds.fields);
    assert!(local.info().notes.is_empty());
    let back = local.read(&Request::all()).unwrap();
    assert_eq!(back.features.len(), 3);
    assert!(back.complete && back.skipped == 0);
    for (got, original) in back.features.iter().zip(&ds.features) {
        // Comparing WKB bytes also checks the coordinate bits, rather than numeric equality.
        assert_eq!(got.geometry.as_ref().map(wkb::encode), original.geometry.as_ref().map(wkb::encode));
        assert_eq!(got.attrs, original.attrs);
    }
    assert_eq!(back.features.iter().map(|f| f.id).collect::<Vec<_>>(), [1, 2, 3]);
    let (rows, summary, _) = visit_all(local.as_ref(), None, 256);
    assert_eq!(rows, back.features);
    assert_eq!((summary.matched, summary.skipped), (3, 0));
    let (whole, report) = materialize(local.as_ref()).unwrap();
    assert_eq!(whole.features.to_vec(), back.features);
    assert!(report.skipped == 0 && report.messages.is_empty());
    let url = "https://test.example/exact-export.parquet";
    remote::testing::serve(url, bytes);
    let source = open_driven(url);
    assert!(source.info().indexed);
    assert_eq!(source.read(&Request::all()).unwrap().features, back.features);
    // Null rows must not consume a sample slot, whether the group is partly or wholly inside.
    for (extent, matched) in [(Extent::new(-90.0, 36.0, -89.0, 37.0), 1), (Extent::new(-180.0, -90.0, 180.0, 90.0), 2)] {
        for limit in [1, 10] {
            let req = Request { extent: Some(extent), limit };
            let remote = source.read(&req).unwrap();
            let local_batch = local.read(&req).unwrap();
            assert_eq!(remote.features.len(), matched.min(limit));
            assert_eq!((remote.matched, local_batch.matched), (matched as u64, matched as u64));
            assert_eq!(remote.features, local_batch.features);
            assert_eq!(remote.complete, matched <= limit);
            if remote.complete {
                let (rows, summary, _) = visit_all(local.as_ref(), req.extent, 256);
                assert_eq!(rows, remote.features);
                assert_eq!(summary.matched, remote.matched);
            }
        }
    }
}

/// Inferred pre-1.1 boxes may be absent independently of geometry; sample actual spatial matches.
#[test]
fn geoparquet_inferred_covering_nulls_fall_back_to_geometry() {
    use arrow_array::{BinaryArray, Float64Array, RecordBatch, StructArray};
    use arrow_schema::{DataType, Field as AField, Schema};
    let point = Coord::new(-89.5, 36.5);
    let wkb = wkb::encode(&Geometry::Point(point));
    let bbox_fields = ["xmin", "ymin", "xmax", "ymax"].map(|name| Arc::new(AField::new(name, DataType::Float64, true)));
    let coords: Vec<arrow_array::ArrayRef> = [0.0, 0.0, 0.0, 0.0]
        .into_iter()
        .map(|value| Arc::new(Float64Array::from(vec![Some(value), None])) as arrow_array::ArrayRef)
        .collect();
    let bbox = StructArray::new(bbox_fields.iter().cloned().collect(), coords, None);
    let schema = Arc::new(Schema::new(vec![
        AField::new("geometry", DataType::Binary, true),
        AField::new("bbox", DataType::Struct(bbox_fields.iter().cloned().collect()), false),
    ]));
    // This is an ordinary struct column in 1.0, with no declared covering/null-presence contract.
    let geo = r#"{"version":"1.0.0","primary_column":"geometry","columns":{"geometry":{"encoding":"WKB","geometry_types":["Point"]}}}"#;
    let props = ::parquet::file::properties::WriterProperties::builder()
        .set_key_value_metadata(Some(vec![::parquet::file::metadata::KeyValue::new("geo".to_string(), geo.to_string())]))
        .build();
    let cols: Vec<arrow_array::ArrayRef> = vec![Arc::new(BinaryArray::from(vec![None, Some(wkb.as_slice())])), Arc::new(bbox)];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();
    let mut writer = ::parquet::arrow::ArrowWriter::try_new(Vec::new(), schema, Some(props)).unwrap();
    writer.write(&batch).unwrap();
    let bytes = writer.into_inner().unwrap();
    let local = open_bytes("inferred.parquet", bytes.clone(), None).unwrap();
    assert!(local.info().indexed);
    assert_eq!(local.read(&Request::all()).unwrap().features.len(), 2);
    let url = "https://test.example/inferred-covering.parquet";
    remote::testing::serve(url, bytes);
    let remote = open_driven(url);
    for extent in [Extent::new(-90.0, 36.0, -89.0, 37.0), Extent::new(-180.0, -90.0, 180.0, 90.0)] {
        for limit in [1, 10] {
            let req = Request { extent: Some(extent), limit };
            let expected = local.read(&req).unwrap();
            assert_eq!(expected.features.len(), 1);
            assert_eq!((expected.matched, expected.complete, expected.skipped), (1, true, 0));
            assert_eq!(expected.features[0].id, 2);
            let by_address = remote.read(&req).unwrap();
            assert_eq!(by_address.features, expected.features);
            assert_eq!((by_address.matched, by_address.complete, by_address.skipped), (1, true, 0));
            assert_eq!(visit_all(local.as_ref(), req.extent, 256).0, expected.features);
        }
    }
}

#[test]
fn geopackage_export_preserves_existing_database_and_wal() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("county.gpkg");
    let mut parcels = Dataset::new("parcels", GeometryType::Point, vec![]);
    parcels.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![]);
    crate::io::write_path(&parcels, &path).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         CREATE TABLE zoning (name TEXT);
         INSERT INTO zoning VALUES ('residential');
         INSERT INTO gpkg_contents (table_name, data_type, identifier) VALUES ('zoning', 'attributes', 'zoning');",
    )
    .unwrap();
    let wal = workspace.path().join("county.gpkg-wal");
    let before = std::fs::read(&path).unwrap();
    let wal_before = std::fs::read(&wal).unwrap();
    assert!(!wal_before.is_empty());
    let mut permits = parcels.clone();
    permits.name = "permits".into();
    let result = crate::io::write_path(&permits, &path);
    assert!(result.is_err(), "export must refuse to replace an existing GeoPackage");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    let names: Vec<String> = conn
        .prepare("SELECT table_name FROM gpkg_contents ORDER BY table_name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(names, ["parcels", "zoning"]);
    assert_eq!(conn.query_row("SELECT name FROM zoning", [], |r| r.get::<_, String>(0)).unwrap(), "residential");
    // A fresh filename still exports normally, including a round trip through the reader.
    let fresh = workspace.path().join("permits.gpkg");
    crate::io::write_path(&permits, &fresh).unwrap();
    assert_eq!(crate::io::read_path(&fresh).unwrap().0.features.len(), 1);
}

#[test]
fn geopackage_export_refuses_existing_entries_and_orphaned_journals() {
    let workspace = tempfile::tempdir().unwrap();
    let ds = Dataset::new("permits", GeometryType::Point, vec![]);
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = workspace.path().join("county.gpkg");
        let entry = workspace.path().join(format!("county.gpkg{suffix}"));
        std::fs::write(&entry, b"existing data").unwrap();
        assert!(crate::io::write_path(&ds, &path).is_err());
        assert_eq!(std::fs::read(&entry).unwrap(), b"existing data");
        std::fs::remove_file(entry).unwrap();
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
    }
    let path = workspace.path().join("directory.gpkg");
    std::fs::create_dir(&path).unwrap();
    assert!(crate::io::write_path(&ds, &path).is_err());
    assert!(path.is_dir());
}

#[cfg(unix)]
#[test]
fn geopackage_export_uses_regular_file_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let workspace = tempfile::tempdir().unwrap();
    let regular = workspace.path().join("regular");
    std::fs::write(&regular, b"control").unwrap();
    let path = workspace.path().join("permits.gpkg");
    crate::io::write_path(&Dataset::new("permits", GeometryType::Point, vec![]), &path).unwrap();
    // Compare with an ordinary file under the current umask, without changing process-wide state.
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), mode(&regular));
}

#[cfg(unix)]
#[test]
fn geopackage_export_preserves_destination_symlinks() {
    let workspace = tempfile::tempdir().unwrap();
    let ds = Dataset::new("permits", GeometryType::Point, vec![]);
    let target = workspace.path().join("target.gpkg");
    let path = workspace.path().join("county.gpkg");
    std::os::unix::fs::symlink(&target, &path).unwrap();
    for exists in [false, true] {
        if exists {
            std::fs::write(&target, b"existing data").unwrap();
        }
        assert!(crate::io::write_path(&ds, &path).is_err());
        assert_eq!(std::fs::read_link(&path).unwrap(), target);
        if exists {
            assert_eq!(std::fs::read(&target).unwrap(), b"existing data");
        } else {
            assert!(!target.exists());
        }
    }
}

#[test]
fn writes_points_lines_kmz_and_zipped_shapefiles() {
    let mut pts = Dataset::new("Cities", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    pts.push(Some(Geometry::Point(Coord::new(2.35, 48.85))), vec![Value::Text("Paris".into())]);
    pts.push(Some(Geometry::MultiPoint(vec![Coord::new(1.0, 1.0), Coord::new(2.0, 2.0)])), vec![Value::Text("Pair".into())]);
    let mut lines = Dataset::new("Roads", GeometryType::Polyline, vec![Field::new("len", FieldType::Double)]);
    lines.push(
        Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 1.0)], vec![Coord::new(5.0, 5.0), Coord::new(6.0, 5.0)]])),
        vec![Value::Float(1e30)],
    );
    lines.push(Some(Geometry::Line(vec![vec![Coord::new(3.0, 3.0)]])), vec![Value::Float(2.0)]);
    let d = dir().join("export2");
    std::fs::create_dir_all(&d).unwrap();
    for ds in [&pts, &lines] {
        for ext in ["gpkg", "parquet", "shp"] {
            let p = d.join(format!("{}.{ext}", ds.name));
            let left_out = crate::io::write_path(ds, &p).unwrap();
            let (back, _) = crate::io::read_path(&p).unwrap();
            assert_eq!(back.geometry_type, ds.geometry_type, "{ext}");
            // A one-point line can't be a Shapefile polyline (and isn't read back from the others).
            let lost = usize::from(ds.name == "Roads");
            assert_eq!((left_out, back.features.len()), (if ext == "shp" { lost } else { 0 }, 2 - lost), "{} {ext}", ds.name);
            assert_eq!(
                back.extent(),
                ds.features.iter().take(2 - lost).filter_map(|f| f.geometry.as_ref()).fold(Extent::EMPTY, |e, g| e.union(&g.extent()))
            );
        }
    }
    // A number too large for a dBase field is null rather than cut short.
    let (roads, _) = crate::io::read_path(&d.join("Roads.shp")).unwrap();
    assert_eq!(roads.value(0, 0), &Value::Null);
    // A whole number past 2^53 would only be stored rounded, so it's null too.
    let mut ids = Dataset::new("ids", GeometryType::Point, vec![Field::new("id", FieldType::Integer)]);
    ids.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Int((1 << 53) + 1)]);
    ids.push(Some(Geometry::Point(Coord::new(1.0, 1.0))), vec![Value::Int(1 << 52)]);
    crate::io::write_path(&ids, &d.join("ids.shp")).unwrap();
    let (back, _) = crate::io::read_path(&d.join("ids.shp")).unwrap();
    assert_eq!((back.value(0, 0), back.value(1, 0)), (&Value::Null, &Value::Int(1 << 52)));
    // An upper-case .SHP keeps its name, so the path given is the file written.
    crate::io::write_path(&ids, &d.join("Upper.SHP")).unwrap();
    assert!(crate::io::read_path(&d.join("Upper.SHP")).is_ok());
    // No temporary files are left beside the outputs.
    assert!(std::fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
    // KMZ: the KML zipped as doc.kml.
    let (kmz, left_out) = crate::io::write_bytes(&pts, "kmz").unwrap();
    assert_eq!(left_out, 0);
    let entries = crate::zip::read(&kmz).unwrap();
    assert_eq!(entries[0].0, "doc.kml");
    assert!(String::from_utf8_lossy(&entries[0].1).contains("<name>Paris</name>"));
    // A zipped Shapefile opens as one.
    let (zipped, _) = crate::io::write_bytes(&pts, "shp").unwrap();
    let entries = crate::zip::read(&zipped).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names, ["Cities.shp", "Cities.shx", "Cities.dbf", "Cities.prj", "Cities.cpg"]);
    let unzipped = d.join("unzipped");
    std::fs::create_dir_all(&unzipped).unwrap();
    for (n, b) in &entries {
        std::fs::write(unzipped.join(n), b).unwrap();
    }
    let (back, _) = crate::io::read_path(&unzipped.join("Cities.shp")).unwrap();
    assert_eq!(back.features.len(), 2);
    // Tables have no Shapefile or FlatGeobuf form, but GeoPackage and GeoParquet hold them.
    let mut table = Dataset::new("t", GeometryType::None, vec![Field::new("a", FieldType::Integer)]);
    table.push(None, vec![Value::Int(1)]);
    assert!(crate::io::write_bytes(&table, "shp").is_err());
    assert!(crate::io::write_bytes(&table, "parquet").is_ok());
    crate::io::write_path(&table, &d.join("t.gpkg")).unwrap();
    // Field names differing only in case are distinct in a dataset but not in SQLite.
    let mut cased = Dataset::new("c", GeometryType::Point, vec![Field::new("A", FieldType::Integer), Field::new("a", FieldType::Text)]);
    cased.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Int(1), Value::Text("x".into())]);
    crate::io::write_path(&cased, &d.join("c.gpkg")).unwrap();
    let (back, _) = crate::io::read_path(&d.join("c.gpkg")).unwrap();
    assert_eq!(back.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["A", "a_1"]);
    assert_eq!(back.features[0].attrs, [Value::Int(1), Value::Text("x".into())]);
    // And the export says so, as the file has it; names kept (an id or geometry column moves
    // aside for a field) say nothing.
    assert_eq!(
        crate::io::geopackage_names_note(&cased.fields, "c.gpkg").as_deref(),
        Some("c.gpkg: 1 field name was changed to fit a GeoPackage (unique ignoring case): 'a' → 'a_1'")
    );
    let kept: Vec<Field> = ["fid", "geom", "a"].iter().map(|n| Field::new(*n, FieldType::Integer)).collect();
    assert_eq!(crate::io::geopackage_names_note(&kept, "k.gpkg"), None);
    // A long name shows its start and the ending that makes it unique.
    let long: Vec<Field> =
        ["Ab", "ab", "Bb", "bb"].iter().map(|n| Field::new(format!("{n}{}", "x".repeat(50)), FieldType::Integer)).collect();
    let note = crate::io::geopackage_names_note(&long, "l.gpkg").unwrap();
    let x = |n| "x".repeat(n);
    assert!(
        note.ends_with(&format!(
            "'ab{}…{}' → 'ab{}…{}_1', 'bb{}…{}' → 'bb{}…{}_1'",
            x(18),
            x(20),
            x(18),
            x(18),
            x(18),
            x(20),
            x(18),
            x(18)
        )),
        "{note}"
    );
    // So does one that fits until it is made unique.
    let edge: Vec<Field> = ["A".repeat(40), "a".repeat(40)].iter().map(|n| Field::new(n, FieldType::Integer)).collect();
    let note = crate::io::geopackage_names_note(&edge, "e.gpkg").unwrap();
    assert!(note.ends_with(&format!("'{}' → '{}…{}_1'", "a".repeat(40), "a".repeat(20), "a".repeat(18))), "{note}");
    // An empty layer writes too.
    let empty = Dataset::new("e", GeometryType::Point, vec![]);
    assert!(crate::io::write_bytes(&empty, "parquet").is_ok());
    crate::io::write_path(&empty, &d.join("e.gpkg")).unwrap();
    assert!(crate::io::write_bytes(&empty, "shp").is_err());
}

/// Compares the browser's GeoPackage reader with SQLite on every table of every `.gpkg` in
/// `RUSTGIS_SAMPLE_VECTORS`: `cargo test -p rustgis-data --release -- --ignored gpkg_mem_matches --nocapture`.
#[test]
#[ignore = "needs external GeoPackage samples in RUSTGIS_SAMPLE_VECTORS"]
fn gpkg_mem_matches_sqlite() {
    let Ok(dir) = std::env::var("RUSTGIS_SAMPLE_VECTORS") else { return };
    let mut paths: Vec<_> = std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| ext(p) == "gpkg").collect();
    paths.sort();
    let (mut tables, mut differ) = (0, 0);
    for p in paths {
        let bytes = Shared(Arc::new(std::fs::read(&p).unwrap()));
        let names = match layers(&p) {
            Ok(n) => n,
            Err(e) => {
                println!("{}: {e}", p.display());
                continue;
            }
        };
        for name in names {
            tables += 1;
            let t = std::time::Instant::now();
            let mem = gpkg_mem::GpkgMemSource::open_shared(bytes.clone(), Some(&name))
                .and_then(|s| s.read(&Request::all()).map(|b| (s.info().clone(), b)));
            let ms = t.elapsed().as_millis();
            let sql = open_path(&p, Some(&name)).and_then(|s| s.read(&Request::all()).map(|b| (s.info().clone(), b)));
            match (mem, sql) {
                (Ok((mi, mut m)), Ok((si, mut s))) => {
                    m.features.sort_by_key(|f| f.id);
                    s.features.sort_by_key(|f| f.id);
                    let same =
                        m.features == s.features && mi.fields == si.fields && mi.count == si.count && mi.geometry_type == si.geometry_type;
                    differ += usize::from(!same);
                    println!("{} {name}: {} features, {ms} ms{}", p.display(), m.features.len(), if same { "" } else { "  DIFFERENT" });
                }
                (m, s) => {
                    differ += usize::from(m.is_ok() != s.is_ok());
                    println!("{} {name}: mem {:?} / sqlite {:?}", p.display(), m.err(), s.err());
                }
            }
        }
    }
    println!("{tables} tables, {differ} different");
    assert_eq!(differ, 0);
}

fn open_driven(url: &str) -> Fetching {
    Fetching(std::sync::Arc::from(open_url_blocking(url).unwrap()))
}

/// `n` × `n` points over the world, a FlatGeobuf or a GeoParquet (row groups of 2 000), larger
/// than one fetch.
fn big(n: usize, parquet: bool) -> Vec<u8> {
    let mut ds =
        Dataset::new("big", GeometryType::Point, vec![Field::new("name", FieldType::Text), Field::new("value", FieldType::Integer)]);
    // Built directly: `push` per feature is slow for this many.
    for i in 0..n {
        for j in 0..n {
            let c = Coord::new(-179.0 + 358.0 * i as f64 / n as f64, -80.0 + 160.0 * j as f64 / n as f64);
            let id = (i * n + j) as u64;
            ds.features.push(crate::Feature {
                id: id + 1,
                geometry: Some(Geometry::Point(c)),
                attrs: vec![Value::Text(format!("p{i}_{j}")), Value::Int(id as i64)],
            });
        }
    }
    if !parquet {
        return fgb::write(&ds).unwrap();
    }
    // GeoParquet in row groups of 2 000 rows (with a bbox covering), as large files are written.
    use arrow_array::{BinaryArray, Float64Array, Int64Array, RecordBatch, StringArray, StructArray};
    use arrow_schema::{DataType, Field as AField, Schema};
    let pts: Vec<Coord> = ds
        .features
        .iter()
        .filter_map(|f| match f.geometry {
            Some(Geometry::Point(c)) => Some(c),
            _ => None,
        })
        .collect();
    let bbox_fields = ["xmin", "ymin", "xmax", "ymax"].map(|n| Arc::new(AField::new(n, DataType::Float64, false)));
    let coord = |f: fn(&Coord) -> f64| Arc::new(Float64Array::from(pts.iter().map(f).collect::<Vec<_>>()));
    let bbox =
        StructArray::new(bbox_fields.iter().cloned().collect(), vec![coord(|c| c.x), coord(|c| c.y), coord(|c| c.x), coord(|c| c.y)], None);
    let schema = Arc::new(Schema::new(vec![
        AField::new("name", DataType::Utf8, false),
        AField::new("value", DataType::Int64, false),
        AField::new("geometry", DataType::Binary, false),
        AField::new("bbox", DataType::Struct(bbox_fields.iter().cloned().collect()), false),
    ]));
    let wkbs: Vec<Vec<u8>> = pts.iter().map(|c| wkb::encode(&Geometry::Point(*c))).collect();
    let cols: Vec<arrow_array::ArrayRef> = vec![
        Arc::new(StringArray::from(ds.features.iter().map(|f| f.attrs[0].to_string()).collect::<Vec<_>>())),
        Arc::new(Int64Array::from(ds.features.iter().map(|f| f.attrs[1].as_f64().unwrap_or(0.0) as i64).collect::<Vec<_>>())),
        Arc::new(BinaryArray::from(wkbs.iter().map(|w| w.as_slice()).collect::<Vec<_>>())),
        Arc::new(bbox),
    ];
    let geo = r#"{"version":"1.1.0","primary_column":"geometry","columns":{"geometry":{"encoding":"WKB","geometry_types":["Point"],
        "covering":{"bbox":{"xmin":["bbox","xmin"],"ymin":["bbox","ymin"],"xmax":["bbox","xmax"],"ymax":["bbox","ymax"]}}}}}"#;
    let props = ::parquet::file::properties::WriterProperties::builder()
        .set_max_row_group_row_count(Some(2_000))
        .set_key_value_metadata(Some(vec![::parquet::file::metadata::KeyValue::new("geo".to_string(), geo.to_string())]))
        .build();
    let mut out = Vec::new();
    let mut w = ::parquet::arrow::ArrowWriter::try_new(&mut out, schema.clone(), Some(props)).unwrap();
    w.write(&RecordBatch::try_new(schema, cols).unwrap()).unwrap();
    w.close().unwrap();
    out
}

#[test]
fn flatgeobuf_and_geoparquet_by_address() {
    let d = dir();
    // The same checks as for a file on disk, read by range.
    let f = d.join("r.fgb");
    write_fgb(&f);
    remote::testing::serve("https://test.example/grid.fgb", std::fs::read(&f).unwrap());
    let src = open_driven("https://test.example/grid.fgb");
    check(&src, true);
    assert_eq!(src.info().name, "grid");
    let p = d.join("r.parquet");
    write_parquet(&p, true);
    remote::testing::serve("https://test.example/grid.parquet", std::fs::read(&p).unwrap());
    check(&open_driven("https://test.example/grid.parquet"), true);

    // Opening keeps reading the file it fetches for, even when other files push it out of those
    // kept while its bytes are on their way.
    let busy = "https://test.example/busy.fgb#busy";
    remote::testing::serve(busy, std::fs::read(&f).unwrap());
    assert_eq!(open_driven(busy).info().count, src.info().count);

    // A view of a large file fetches a small part of it, and the same features as the file on disk.
    for (name, parquet) in [("big.fgb", false), ("big.parquet", true)] {
        let bytes = big(400, parquet);
        let total = bytes.len();
        let url = format!("https://test.example/{name}");
        let sent = remote::testing::serve(&url, bytes.clone());
        let src = open_driven(&url);
        assert_eq!(src.info().count, Some(160_000), "{name}");
        let e = Extent::new(10.0, 10.0, 12.0, 12.0);
        let req = Request { extent: Some(e), limit: 10_000 };
        let got = src.read(&req).unwrap();
        let local = open_bytes(name, bytes, None).unwrap().read(&req).unwrap();
        let ids = |b: &Batch| b.features.iter().map(|f| f.id).collect::<std::collections::BTreeSet<_>>();
        assert!(!got.features.is_empty() && ids(&got) == ids(&local), "{name}: {} vs {}", got.features.len(), local.features.len());
        assert_eq!(
            got.features.first().map(|f| &f.attrs),
            local.features.iter().find(|f| Some(f.id) == got.features.first().map(|g| g.id)).map(|f| &f.attrs)
        );
        let fetched = sent.load(std::sync::atomic::Ordering::Relaxed);
        assert!(fetched < total / 4, "{name}: fetched {fetched} of {total} bytes");
        // A sample of the whole file, spread over it.
        let s = src.read(&Request { extent: None, limit: 500 }).unwrap();
        assert!(!s.complete && s.features.len() <= 500 && s.features.len() >= 250, "{name}: {}", s.features.len());
    }

    // Files without an index or a covering are read whole instead. Not plain.parquet: the
    // tests run at once and share this folder, and geoparquet_with_and_without_a_covering
    // writes and reads that one.
    let p = d.join("r-plain.parquet");
    write_parquet(&p, false);
    remote::testing::serve("https://test.example/plain.parquet", std::fs::read(&p).unwrap());
    let e = open_url_blocking("https://test.example/plain.parquet").err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains(remote::READ_WHOLE), "{e}");
    let mut ds = Dataset::new("x", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![]);
    let unindexed = {
        use flatgeobuf::{FgbWriter, FgbWriterOptions, GeometryType as G};
        let mut w = FgbWriter::create_with_options("x", G::Point, FgbWriterOptions { write_index: false, ..Default::default() }).unwrap();
        w.add_feature_geom(flatgeobuf::geozero::wkb::Wkb(wkb::encode(&Geometry::Point(Coord::new(1.0, 2.0)))), |_| {}).unwrap();
        let mut out = Vec::new();
        w.write(&mut out).unwrap();
        out
    };
    remote::testing::serve("https://test.example/noindex.fgb", unindexed);
    let e = open_url_blocking("https://test.example/noindex.fgb").err().map(|e| e.to_string()).unwrap_or_default();
    assert!(e.contains(remote::READ_WHOLE), "{e}");

    // Cut short and junk: errors, never a panic or an endless read.
    let fgb = big(40, false);
    let pq = big(40, true);
    for (k, n) in (0..fgb.len()).step_by(997).enumerate() {
        let url = format!("https://test.example/cut{k}.fgb");
        remote::testing::serve(&url, fgb[..n].to_vec());
        if let Ok(s) = open_url_blocking(&url) {
            let s: std::sync::Arc<dyn FeatureSource> = std::sync::Arc::from(s);
            let _ = read_blocking(&s, &Request::all());
        }
    }
    for (k, n) in (0..pq.len()).step_by(991).enumerate() {
        let url = format!("https://test.example/cut{k}.parquet");
        remote::testing::serve(&url, pq[pq.len() - n..].to_vec());
        if let Ok(s) = open_url_blocking(&url) {
            let s: std::sync::Arc<dyn FeatureSource> = std::sync::Arc::from(s);
            let _ = read_blocking(&s, &Request::all());
        }
    }
    assert!(open_url_blocking("https://test.example/missing.fgb").is_err());
    assert!(is_remote_name("https://x.org/a/b.fgb?sig=1") && is_remote_name("http://x.org/b.geoparquet") && !is_remote_name("/tmp/a.fgb"));
    assert!(is_remote_name("s3://bucket/a/b.parquet") && !is_remote_name("s3://bucket/a/b.tif") && !is_remote_name("ftp://x.org/a.fgb"));
}

#[test]
fn login_bound_remote_files_keep_their_request_address() {
    crate::login_address::set_validator(remote::testing::validate_login);
    for (name, parquet) in [("bound-grid.fgb", false), ("bound-grid.geoparquet", true)] {
        let inner = format!("https://test.example/{name}?signature=a%2Fb#view");
        let address = crate::login_address::bind("0123456789abcdef", "https://test.example", &inner).unwrap();
        assert!(is_remote_name(&address));
        assert_eq!(url_name(&address), "bound-grid");
        remote::testing::serve(&address, big(4, parquet));
        let file = remote::file(&address);
        assert_eq!(file.url(), address);
        let source = open_driven(&address);
        assert_eq!(source.info().count, Some(16));
    }
    let image = crate::login_address::bind("0123456789abcdef", "https://test.example", "https://test.example/a.tif").unwrap();
    assert!(!is_remote_name(&image));
}

#[test]
fn flatgeobuf_stand_in_is_a_feature() {
    // The stand-in a sample passes over must pass the flatbuffers verifier, as real features do.
    let body = &fgb::STAND_IN_FOR_TESTS[4..];
    assert!(flatgeobuf::size_prefixed_root_as_feature(fgb::STAND_IN_FOR_TESTS.as_slice()).is_ok(), "{body:?}");
}

/// Writes a dataset as a Shapefile at `path` (its `.shp`), with the other files beside it.
fn write_shapefile(ds: &Dataset, path: &Path) {
    let (parts, _) = crate::io::shapefile_parts(ds).unwrap();
    for (ext, bytes) in parts {
        std::fs::write(path.with_extension(ext), bytes).unwrap();
    }
}

/// The points as a Shapefile.
fn points_shapefile(path: &Path) {
    let mut ds =
        Dataset::new("grid", GeometryType::Point, vec![Field::new("name", FieldType::Text), Field::new("value", FieldType::Integer)]);
    for (c, name, v) in points() {
        ds.push(Some(Geometry::Point(c)), vec![Value::Text(name), Value::Int(v)]);
    }
    write_shapefile(&ds, path);
}

#[test]
fn shapefile_read_in_place() {
    let p = dir().join("grid.shp");
    points_shapefile(&p);
    let src = open_path(&p, None).unwrap();
    assert_eq!(src.info().format, "Shapefile");
    check(src.as_ref(), true);
    assert_eq!(shp::record_count(&p), Some(600));
}

#[test]
fn shapefile_in_place_matches_one_read_whole() {
    let d = dir().join("shp-whole");
    std::fs::create_dir_all(&d).unwrap();
    let mut lines =
        Dataset::new("roads", GeometryType::Polyline, vec![Field::new("len", FieldType::Double), Field::new("ok", FieldType::Boolean)]);
    lines.push(
        Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.5, 2.25)], vec![Coord::new(3.0, 3.0), Coord::new(4.0, 5.0)]])),
        vec![Value::Float(2.5), Value::Bool(true)],
    );
    lines.push(Some(Geometry::Line(vec![vec![Coord::new(-7.0, 8.0), Coord::new(9.0, -10.0)]])), vec![Value::Null, Value::Bool(false)]);
    let mut multi = Dataset::new("pairs", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    multi.push(Some(Geometry::MultiPoint(vec![Coord::new(1.0, 1.0), Coord::new(2.0, 2.0)])), vec![Value::Text("pair".into())]);
    multi.push(Some(Geometry::MultiPoint(vec![Coord::new(-3.0, 4.0)])), vec![Value::Text("one".into())]);
    for ds in [export_sample(), lines, multi] {
        let p = d.join(format!("{}.shp", ds.name));
        write_shapefile(&ds, &p);
        let (whole, _) = crate::io::read_path(&p).unwrap();
        let (inplace, report) = materialize(open_path(&p, None).unwrap().as_ref()).unwrap();
        assert!(report.messages.is_empty(), "{:?}", report.messages);
        assert_eq!(inplace.geometry_type, whole.geometry_type, "{}", ds.name);
        assert_eq!(inplace.fields, whole.fields, "{}", ds.name);
        assert_eq!(inplace.features, whole.features, "{}", ds.name);
    }
}

/// A `.dbf`'s text read in place and whole alike: spaces before the text kept, the padding after it
/// dropped, and `<Null>` read as null and counted (#2059).
#[test]
fn shapefile_text_keeps_its_leading_spaces_and_counts_null_markers() {
    let d = dir().join("shp-text");
    std::fs::create_dir_all(&d).unwrap();
    let mut ds = Dataset::new("codes", GeometryType::Point, vec![Field::new("code", FieldType::Text)]);
    for (i, t) in [" 2535", "<Null>", "plain  ", "<null>"].into_iter().enumerate() {
        ds.push(Some(Geometry::Point(Coord::new(i as f64, 0.0))), vec![Value::Text(t.into())]);
    }
    let p = d.join("codes.shp");
    write_shapefile(&ds, &p);
    let (whole, whole_report) = crate::io::read_path(&p).unwrap();
    let (inplace, report) = materialize(open_path(&p, None).unwrap().as_ref()).unwrap();
    let values: Vec<Value> = whole.features.iter().map(|f| f.attrs[0].clone()).collect();
    assert_eq!(values, [Value::Text(" 2535".into()), Value::Null, Value::Text("plain".into()), Value::Null]);
    assert_eq!(inplace.features, whole.features);
    let said = "codes: 2 values written as Null were read as empty, in 'code'";
    assert!(whole_report.messages.iter().any(|m| m == said), "{:?}", whole_report.messages);
    assert!(report.messages.iter().any(|m| m == said), "{:?}", report.messages);

    // Text without the marker says nothing.
    let mut clean = Dataset::new("clean", GeometryType::Point, vec![Field::new("code", FieldType::Text)]);
    clean.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::Text(" 2535".into())]);
    let p = d.join("clean.shp");
    write_shapefile(&clean, &p);
    let (_, whole_report) = crate::io::read_path(&p).unwrap();
    let (_, report) = materialize(open_path(&p, None).unwrap().as_ref()).unwrap();
    assert!(whole_report.messages.iter().chain(&report.messages).all(|m| !m.contains("written as Null")), "{:?}", report.messages);
}

#[test]
fn shapefile_in_place_reprojects_from_its_prj() {
    let d = dir().join("shp-utm");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("paris.shp");
    let mut ds = Dataset::new("paris", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    // Paris in UTM zone 31N.
    ds.push(Some(Geometry::Point(Coord::new(451_219.33, 5_411_550.20))), vec![Value::Text("paris".into())]);
    write_shapefile(&ds, &p);
    let utm = r#"PROJCS["WGS_1984_UTM_Zone_31N",GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",3.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#;
    std::fs::write(p.with_extension("prj"), utm).unwrap();
    let src = open_path(&p, None).unwrap();
    let e = src.info().extent;
    assert!((e.min_x - 2.335).abs() < 1e-6 && (e.min_y - 48.855).abs() < 1e-6, "{e:?}");
    assert!(src.info().notes.is_empty(), "{:?}", src.info().notes);
    // A longitude/latitude extent finds it; one beside it doesn't.
    let near = in_lonlat(src.as_ref(), src.read(&Request { extent: Some(Extent::new(2.0, 48.5, 2.5, 49.0)), limit: 10 }).unwrap());
    let c = near.features.first().and_then(|f| f.geometry.as_ref()).and_then(Geometry::label_point).unwrap();
    assert!((c.x - 2.335).abs() < 1e-6 && (c.y - 48.855).abs() < 1e-6, "{c:?}");
    assert!(src.read(&Request { extent: Some(Extent::new(3.0, 48.5, 3.5, 49.0)), limit: 10 }).unwrap().features.is_empty());
    // Without the .prj the coordinates are taken as they are, and that's said.
    std::fs::remove_file(p.with_extension("prj")).unwrap();
    let src = open_path(&p, None).unwrap();
    assert!(src.info().notes.iter().any(|n| n.contains("not longitude/latitude")), "{:?}", src.info().notes);
}

#[test]
fn damaged_shapefiles_are_errors_or_skips_not_panics() {
    let d = dir().join("shp-junk");
    std::fs::create_dir_all(&d).unwrap();
    let good = d.join("good.shp");
    write_shapefile(&export_sample(), &good);
    let (shp, dbf) = (std::fs::read(&good).unwrap(), std::fs::read(good.with_extension("dbf")).unwrap());
    let p = d.join("cut.shp");
    let read_all = |p: &Path| {
        let read = || {
            if let Ok(s) = open_path(p, None) {
                let _ = s.read(&Request::all());
                let _ = s.read(&Request { extent: Some(Extent::new(-10.0, -10.0, 10.0, 10.0)), limit: 1 });
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        shp::with_test_cache(d.join("indexes"), read);
        #[cfg(target_arch = "wasm32")]
        read();
    };
    std::fs::write(p.with_extension("dbf"), &dbf).unwrap();
    for n in 0..shp.len() {
        std::fs::write(&p, &shp[..n]).unwrap();
        read_all(&p);
    }
    // Every byte of the shapes flipped in turn.
    for i in 100..shp.len() {
        let mut bad = shp.clone();
        bad[i] ^= 0xA5;
        std::fs::write(&p, &bad).unwrap();
        read_all(&p);
    }
    std::fs::write(&p, &shp).unwrap();
    for n in 0..dbf.len() {
        std::fs::write(p.with_extension("dbf"), &dbf[..n]).unwrap();
        read_all(&p);
    }
    std::fs::remove_file(p.with_extension("dbf")).unwrap();
    assert!(open_path(&p, None).is_err());
}

/// Reads every Shapefile under `RUSTGIS_SAMPLE_SHAPEFILES` both ways, in place and whole, and
/// compares them: `cargo test --release -p rustgis-data -- --ignored shapefile_samples --nocapture`.
#[test]
#[ignore = "needs external Shapefile samples in RUSTGIS_SAMPLE_SHAPEFILES"]
fn shapefile_samples() {
    let Ok(dir) = std::env::var("RUSTGIS_SAMPLE_SHAPEFILES") else { return };
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("shp")) {
                out.push(p);
            }
        }
    }
    let mut paths = Vec::new();
    walk(Path::new(&dir), &mut paths);
    paths.sort();
    let mut differ = Vec::new();
    for p in paths {
        let t = std::time::Instant::now();
        let whole = crate::io::read_path(&p);
        let whole_t = t.elapsed();
        let t = std::time::Instant::now();
        let inplace = open_path(&p, None).and_then(|s| materialize(s.as_ref()));
        let inplace_t = t.elapsed();
        let (Ok((whole, _)), Ok((inplace, _))) = (&whole, &inplace) else {
            println!("{}: whole {:?} / in place {:?}", p.display(), whole.as_ref().err(), inplace.as_ref().err());
            continue;
        };
        let same_geometry = whole.features.iter().zip(&inplace.features).all(|(a, b)| a.geometry == b.geometry);
        let same = whole.features == inplace.features && whole.fields == inplace.fields;
        println!(
            "{}: {} features, whole {whole_t:?}, in place {inplace_t:?}; same={same} (geometry {same_geometry}, fields {})",
            p.display(),
            whole.features.len(),
            whole.fields == inplace.fields
        );
        if !same_geometry || whole.features.len() != inplace.features.len() {
            differ.push(p.display().to_string());
        }
    }
    assert!(differ.is_empty(), "{differ:?}");
}

#[test]
fn shapefile_in_place_keeps_large_whole_numbers_exact() {
    let d = dir().join("shp-ids");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("ids.shp");
    let mut ds = Dataset::new("ids", GeometryType::Point, vec![Field::new("id", FieldType::Integer)]);
    for i in 0..5 {
        ds.push(Some(Geometry::Point(Coord::new(i as f64, 0.0))), vec![Value::Int(i)]);
    }
    write_shapefile(&ds, &p);
    // The id column (18 digits, no decimals) as another program writes it: values past f64's
    // exact integers, whole ones written with a point or exponent, and one that isn't whole.
    let mut dbf = std::fs::read(p.with_extension("dbf")).unwrap();
    let (header, row) = (u16::from_le_bytes([dbf[8], dbf[9]]) as usize, u16::from_le_bytes([dbf[10], dbf[11]]) as usize);
    for (i, text) in ["123456789012345678", "-98765432109876543", "12.0", "1e3", "12.5"].iter().enumerate() {
        let at = header + i * row + 1;
        dbf[at..at + 18].copy_from_slice(format!("{text:>18}").as_bytes());
    }
    std::fs::write(p.with_extension("dbf"), &dbf).unwrap();
    let src = open_path(&p, None).unwrap();
    assert_eq!(src.info().fields[0].ty, FieldType::Integer);
    let got: Vec<Value> = src.read(&Request::all()).unwrap().features.into_iter().map(|f| f.attrs[0].clone()).collect();
    assert_eq!(
        got,
        vec![
            Value::Int(123_456_789_012_345_678),
            Value::Int(-98_765_432_109_876_543),
            Value::Int(12),
            Value::Int(1000),
            Value::Float(12.5)
        ]
    );
}

#[test]
fn geojson_read_in_place() {
    let mut ds =
        Dataset::new("grid", GeometryType::Point, vec![Field::new("name", FieldType::Text), Field::new("value", FieldType::Integer)]);
    for (c, name, v) in points() {
        ds.push(Some(Geometry::Point(c)), vec![Value::Text(name), Value::Int(v)]);
    }
    let p = dir().join("grid.geojson");
    crate::io::write_path(&ds, &p).unwrap();
    let src = open_path(&p, None).unwrap();
    assert_eq!(src.info().format, "GeoJSON");
    check(src.as_ref(), false);
}

#[test]
fn geojson_in_place_rejects_partial_collections_with_a_notice() {
    let text = r#"{"type":"FeatureCollection","features":[
        {"type":"Feature","properties":{},"geometry":{"type":"GeometryCollection","geometries":[
            {"type":"Point","coordinates":[1,2]},
            {"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,0]]]}
        ]}},
        {"type":"Feature","properties":{},"geometry":{"type":"GeometryCollection","geometries":[
            {"type":"Point","coordinates":[8,9]}
        ]}}
    ]}"#;
    let p = dir().join("collections.geojson");
    std::fs::write(&p, text).unwrap();
    let (whole, report) = crate::io::read_geojson(text, "x").unwrap();
    assert_eq!(whole.features.len(), 1);
    assert_eq!(report.skipped, 1);
    for in_memory in [crate::io::BYTES_FLOOR, 0] {
        let src = geojson::GeoJsonSource::open_with(&p, "x", in_memory).unwrap();
        assert_eq!(src.info().count, Some(1));
        assert_eq!(src.skipped_at_open(), 1);
        assert_eq!(src.info().notes, ["1 features had unreadable geometry"]);
        let (ds, streamed_report) = materialize(&src).unwrap();
        assert_eq!(ds.features, whole.features);
        assert_eq!(streamed_report.skipped, report.skipped);
        assert_eq!(streamed_report.messages, report.messages.clone().prefixed("x"));
    }
    std::fs::remove_file(p).unwrap();
}

#[test]
fn geojson_in_place_matches_one_read_whole() {
    let d = dir().join("geojson-whole");
    std::fs::create_dir_all(&d).unwrap();
    // Types widened over features, a feature with geometry that can't be read (its properties
    // still count), one without geometry, mixed kinds, nested values and repeated keys.
    let mixed = r#"{"features":[
        {"type":"Feature","properties":{"a":1,"b":null,"n":{"x":[1,2]}},"geometry":{"type":"Point","coordinates":[1,2]}},
        {"type":"Feature","properties":{"a":2.5,"c":"x"},"geometry":{"type":"Nope"}},
        {"type":"Feature","properties":{"c":true,"d":null},"geometry":null},
        {"type":"Feature","properties":{"a":3},"geometry":{"type":"Polygon","coordinates":[[[0,0],[4,0],[4,4],[0,0]],[[1,1],[2,1],[2,2],[1,1]]]}},
        {"type":"Feature","properties":{"b":"t"},"geometry":{"type":"MultiLineString","coordinates":[[[0,0],[1,1]],[[5,5],[6,7]]]}},
        null,
        {"type":"Feature","properties":{},"geometry":{"type":"MultiPoint","coordinates":[[9,9],[8,8]]}}
    ],"type":"FeatureCollection","name":"m"}"#;
    let docs = [("mixed.geojson", mixed.to_string()), ("parcels.json", crate::io::to_geojson(&export_sample()).to_string())];
    for ((file, text), in_memory) in docs.iter().flat_map(|d| [(d, crate::io::BYTES_FLOOR), (d, 0)]) {
        let p = d.join(file);
        std::fs::write(&p, text).unwrap();
        let (whole, whole_report) = crate::io::read_geojson(text, "x").unwrap();
        // Scanned in memory, and through a buffer, as a file too large to hold is.
        let src = geojson::GeoJsonSource::open_with(&p, "x", in_memory).unwrap();
        let (inplace, report) = materialize(&src).unwrap();
        assert_eq!(inplace.fields, whole.fields, "{file}");
        assert_eq!(inplace.geometry_type, whole.geometry_type, "{file}");
        assert_eq!(inplace.features, whole.features, "{file}");
        assert_eq!(report.messages.len(), whole_report.messages.len(), "{file}: {:?}", report.messages);
        // Extent reads skip features without geometry, and find each by its box.
        let b = src.read(&Request { extent: Some(Extent::new(0.5, 0.5, 1.5, 2.5)), limit: 100 }).unwrap();
        assert!(b.features.iter().all(|f| f.geometry.is_some()), "{file}");
    }
    // Not a FeatureCollection, or not GeoJSON: not read in place.
    for (file, text) in [
        ("one.geojson", r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}"#),
        ("bare.geojson", r#"{"type":"Point","coordinates":[1,2]}"#),
        ("topo.json", r#"{"type":"Topology","objects":{},"arcs":[]}"#),
        ("list.json", "[1, 2]"),
        ("trailing.geojson", r#"{"type":"FeatureCollection","features":[]} {"#),
    ] {
        let p = d.join(file);
        std::fs::write(&p, text).unwrap();
        assert!(open_path(&p, None).is_err() && geojson::GeoJsonSource::open_with(&p, "x", 0).is_err(), "{file}");
    }
}

/// A GeoJSON FeatureCollection of Arnhem's municipal outline's box and a point in RD New
/// (EPSG:28992), `crs` being what the file declares (left out for `None`).
fn rd_geojson(crs: Option<&str>) -> String {
    let crs = crs.map(|c| format!(r#""crs":{c},"#)).unwrap_or_default();
    format!(
        r#"{{"type":"FeatureCollection","name":"arnhem",{crs}"features":[
        {{"type":"Feature","properties":{{"n":"area"}},"geometry":{{"type":"Polygon","coordinates":[[[183548,438459],[196442,438459],[196442,454549],[183548,454549],[183548,438459]]]}}}},
        {{"type":"Feature","properties":{{"n":"point"}},"geometry":{{"type":"Point","coordinates":[188000,445000]}}}}]}}"#
    )
}

fn named(name: &str) -> String {
    format!(r#"{{"type":"name","properties":{{"name":"{name}"}}}}"#)
}

/// What a GeoJSON text reads as through every path: whole, and in place scanned in memory and
/// through a buffer; the paths must agree. Returns the dataset, its messages and the source's
/// `info.crs`.
fn geojson_paths(file: &str, text: &str) -> (Dataset, Vec<String>, String) {
    let d = dir().join("geojson-crs");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join(file);
    std::fs::write(&p, text).unwrap();
    let (whole, report) = crate::io::read_geojson(text, "x").unwrap();
    for in_memory in [crate::io::BYTES_FLOOR, 0] {
        let src = geojson::GeoJsonSource::open_with(&p, "x", in_memory).unwrap();
        let (inplace, in_report) = materialize(&src).unwrap();
        assert_eq!(inplace.features, whole.features, "{file}");
        assert_eq!(inplace.crs, whole.crs, "{file}");
        assert_eq!(inplace.crs_incomplete, whole.crs_incomplete, "{file}");
        assert_eq!(inplace.geometry_type, whole.geometry_type, "{file}");
        assert_eq!(report.skipped, in_report.skipped, "{file}");
        let strip = |m: &crate::loss::Notes| m.iter().map(|m| m.text.trim_start_matches("x: ").to_string()).collect::<Vec<_>>();
        assert_eq!(strip(&report.messages), strip(&in_report.messages), "{file}");
        // The extent a source says is that of the features it reads (those that convert).
        assert_eq!(src.info().extent, whole.lonlat_extent(), "{file}");
        assert_eq!(inplace.held_in, whole.held_in, "{file}");
        assert_eq!(src.info().count, Some(whole.features.len() as u64), "{file}");
        assert_eq!(src.info().coordinate_system(), whole.crs, "{file}");
    }
    let src = geojson::GeoJsonSource::open_with(&p, "x", crate::io::BYTES_FLOOR).unwrap();
    // In longitude/latitude, for the tests to say where its features are: it holds them in its
    // declared system, as the file does.
    (whole.in_lonlat().unwrap().into_owned(), report.messages.texts().map(str::to_string).collect(), src.info().crs.clone())
}

fn in_arnhem(e: &Extent) -> bool {
    (5.8..6.0).contains(&e.min_x) && (5.8..6.0).contains(&e.max_x) && (51.9..52.1).contains(&e.min_y) && (51.9..52.1).contains(&e.max_y)
}

#[test]
fn geojson_legacy_crs_is_honoured_by_both_paths() {
    // The issue #930 declaration, and the other ways EPSG:28992 is named.
    for name in [
        "urn:ogc:def:crs:EPSG::28992",
        "urn:ogc:def:crs:EPSG:6.6:28992",
        "EPSG:28992",
        "epsg:28992",
        "http://www.opengis.net/def/crs/EPSG/0/28992",
    ] {
        let (ds, messages, info_crs) = geojson_paths("rd.geojson", &rd_geojson(Some(&named(name))));
        assert!(in_arnhem(&ds.extent()), "{name}: {:?}", ds.extent());
        assert_eq!(ds.crs, Some(crate::raster::Crs::Epsg(28992)), "{name}");
        assert_eq!(info_crs, "EPSG:28992", "{name}");
        assert!(messages.is_empty(), "{name}: {messages:?}");
        assert!(!ds.crs_incomplete);
        assert_eq!(ds.geometry_type, GeometryType::Polygon);
    }
    // Reads in place go by the layer's own coordinates: a window of longitude/latitude finds the
    // features, one elsewhere finds none.
    let p = dir().join("geojson-crs").join("rd.geojson");
    for in_memory in [crate::io::BYTES_FLOOR, 0] {
        let src = geojson::GeoJsonSource::open_with(&p, "x", in_memory).unwrap();
        let at = |e: Extent| src.read(&Request { extent: Some(e), limit: 100 }).unwrap().features.len();
        assert_eq!(at(Extent::new(5.85, 51.95, 5.86, 51.96)), 1, "the polygon only");
        assert_eq!(at(Extent::new(5.95, 52.02, 5.96, 52.03)), 1, "the polygon only");
        assert_eq!(at(Extent::new(5.8, 51.9, 6.0, 52.1)), 2);
        assert_eq!(at(Extent::new(0.0, 0.0, 1.0, 1.0)), 0);
        assert_eq!(at(Extent::new(500000.0, 438459.0, 600000.0, 454549.0)), 0, "the file's coordinates aren't lon/lat");
    }
}

#[test]
fn geojson_without_a_crs_or_in_lonlat_is_unchanged() {
    let fc = |crs: &str| {
        format!(
            r#"{{"type":"FeatureCollection",{crs}"features":[{{"type":"Feature","properties":{{}},"geometry":{{"type":"Polygon","coordinates":[[[5.8,51.9],[6.0,51.9],[6.0,52.1],[5.8,52.1],[5.8,51.9]]]}}}},
            {{"type":"Feature","properties":{{}},"geometry":{{"type":"Point","coordinates":[-122.5,37.75]}}}}]}}"#
        )
    };
    let wgs84 = [
        String::new(),
        r#""crs":null,"#.to_string(),
        format!(r#""crs":{},"#, named("urn:ogc:def:crs:OGC:1.3:CRS84")),
        format!(r#""crs":{},"#, named("urn:ogc:def:crs:OGC::CRS84")),
        format!(r#""crs":{},"#, named("OGC::CRS84")),
        format!(r#""crs":{},"#, named("OGC:1.3:CRS84")),
        format!(r#""crs":{},"#, named("OGC:CRS84")),
        format!(r#""crs":{},"#, named("CRS84")),
        format!(r#""crs":{},"#, named("http://www.opengis.net/def/crs/OGC/1.3/CRS84")),
        format!(r#""crs":{},"#, named("EPSG:4326")),
        format!(r#""crs":{},"#, named("urn:ogc:def:crs:EPSG::4326")),
    ];
    let (base, _, _) = geojson_paths("plain.geojson", &fc(""));
    assert_eq!(base.extent(), Extent::new(-122.5, 37.75, 6.0, 52.1));
    for crs in &wgs84 {
        let (ds, messages, info_crs) = geojson_paths("wgs84.geojson", &fc(crs));
        assert_eq!(ds.features, base.features, "{crs}: not a coordinate moved");
        assert_eq!(ds.crs, None, "{crs}");
        assert_eq!(info_crs, "EPSG:4326", "{crs}");
        assert!(messages.is_empty(), "{crs}: {messages:?}");
        assert!(!ds.crs_incomplete);
    }
}

#[test]
fn geojson_geographic_crs_keeps_x_y_order() {
    // ETRS89 (EPSG:4258) is latitude/longitude in EPSG, but a legacy GeoJSON crs still gives
    // longitude, latitude: the point stays in the Netherlands, not at 52 E 5.9 N.
    let text = r#"{"type":"FeatureCollection","crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::4258"}},
        "features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[5.9,52.0]}}]}"#;
    let (ds, messages, info_crs) = geojson_paths("etrs89.geojson", text);
    assert!(messages.is_empty(), "{messages:?}");
    assert_eq!(ds.crs, Some(crate::raster::Crs::Epsg(4258)));
    assert_eq!(info_crs, "EPSG:4258");
    let Some(Geometry::Point(c)) = ds.features.first().and_then(|f| f.geometry.clone()) else { panic!("a point") };
    assert!((c.x - 5.9).abs() < 1e-3 && (c.y - 52.0).abs() < 1e-3, "{c:?}");
}

#[test]
fn geojson_crs_not_understood_is_said_and_coordinates_kept() {
    let long = "x".repeat(100_000);
    let declared = [
        (r#"{"type":"link","properties":{"href":"http://example.com/crs","type":"proj4"}}"#.to_string(), "of type \"link\""),
        (named("urn:ogc:def:crs:OGC:1.3:CRS83"), "CRS83"),
        (named("EPSG:1"), "EPSG:1"),
        (named("EPSG:99999999999"), "EPSG:99999999999"),
        (named("EPSG:abc"), "EPSG:abc"),
        (named("Amersfoort / RD New"), "Amersfoort"),
        (named("VN-2000 / UTM zone 48N EPSG:28992"), "VN-2000"),
        (named("OGC:CRS84:EPSG"), "OGC:CRS84:EPSG"),
        (named("x:EPSG:28992"), "x:EPSG:28992"),
        (named("urn:ogc:def:crs:EPSG::28992:extra"), "extra"),
        (named("urn:x:EPSG:6.6:28992"), "urn:x"),
        (named("http://example.com/def/crs/EPSG/0/28992"), "example.com"),
        (named("EPSG:28992:28992"), "EPSG:28992:28992"),
        (named("+proj=utm +zone=31 +datum=WGS84 EPSG"), "+proj=utm"),
        (named("PROJCS[\\\"RD\\\",EPSG]"), "PROJCS"),
        (named(&format!("EPSG:28992{long}")), "…"),
        (r#"{"type":"name","properties":{"name":28992}}"#.to_string(), "of type \"name\""),
        (r#"{"type":"name","properties":{}}"#.to_string(), "of type \"name\""),
        (r#"{"type":"name"}"#.to_string(), "of type \"name\""),
        (r#"{"type":5}"#.to_string(), "of an unknown shape"),
        (r#"{}"#.to_string(), "of an unknown shape"),
        (r#""EPSG:28992""#.to_string(), "of an unknown shape"),
        (r#"28992"#.to_string(), "of an unknown shape"),
        (r#"true"#.to_string(), "of an unknown shape"),
        (r#"["EPSG:28992"]"#.to_string(), "of an unknown shape"),
        (r#"[[[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]]]"#.to_string(), "of an unknown shape"),
        (format!(r#"{{"type":"{long}"}}"#), "…"),
    ];
    for (crs, shown) in &declared {
        let (ds, messages, info_crs) = geojson_paths("odd.geojson", &rd_geojson(Some(crs)));
        // Not taken as WGS 84 without saying so: one message naming the declaration.
        assert_eq!(messages.len(), 1, "{crs:.60}: {messages:?}");
        assert!(messages[0].starts_with("GeoJSON crs ") && messages[0].contains(shown), "{crs:.60}: {}", messages[0]);
        assert!(messages[0].contains("longitude/latitude"), "{}", messages[0]);
        assert!(messages[0].len() < 400, "a declaration is clipped in a message: {}", messages[0].len());
        // Read as they are, nothing converted or lost.
        assert_eq!(ds.features.len(), 2, "{crs:.60}");
        assert_eq!(ds.extent(), Extent::new(183548.0, 438459.0, 196442.0, 454549.0), "{crs:.60}");
        assert_eq!(ds.crs, None);
        assert_eq!(info_crs, "EPSG:4326");
    }
}

#[test]
fn geojson_crs_is_read_only_from_the_feature_collection() {
    // Not part of the legacy text: on a feature, a geometry or a lone Feature. Ignored, quietly.
    let crs = named("EPSG:28992");
    let docs = [
        format!(
            r#"{{"type":"FeatureCollection","features":[{{"type":"Feature","crs":{crs},"properties":{{}},"geometry":{{"type":"Point","coordinates":[188000,445000]}}}}]}}"#
        ),
        format!(
            r#"{{"type":"FeatureCollection","features":[{{"type":"Feature","properties":{{}},"geometry":{{"type":"Point","coordinates":[188000,445000],"crs":{crs}}}}}]}}"#
        ),
        format!(r#"{{"type":"Feature","crs":{crs},"properties":{{}},"geometry":{{"type":"Point","coordinates":[188000,445000]}}}}"#),
        format!(r#"{{"type":"Point","coordinates":[188000,445000],"crs":{crs}}}"#),
    ];
    for text in &docs {
        let (ds, report) = crate::io::read_geojson(text, "x").unwrap();
        assert!(report.messages.is_empty(), "{text}");
        assert_eq!(ds.crs, None, "{text}");
        assert_eq!(ds.extent(), Extent::new(188000.0, 445000.0, 188000.0, 445000.0), "{text}");
    }
    // A FeatureCollection whose features aren't an array is read whole, its crs still honoured.
    let (ds, report) = crate::io::read_geojson(&format!(r#"{{"type":"FeatureCollection","crs":{crs},"features":7}}"#), "x").unwrap();
    assert!(ds.features.is_empty() && report.messages.is_empty());
}

#[test]
fn geojson_features_that_cant_be_converted_are_skipped_and_counted() {
    // A coordinate the declared system can't convert (UTM 1e9 m east), or converts to beyond
    // longitude/latitude (Web Mercator 1e9 m east is longitude 8983°): the feature is dropped, and
    // its height with it (no Z notice for the 2D features kept).
    for (crs, good, bad) in [("EPSG:32617", "[500000,4649776]", "[1e9,0,30]"), ("EPSG:3857", "[1000000,6000000]", "[1e9,0,30]")] {
        let text = format!(
            r#"{{"type":"FeatureCollection","crs":{},"features":[
            {{"type":"Feature","properties":{{"n":1}},"geometry":{{"type":"Point","coordinates":{good}}}}},
            {{"type":"Feature","properties":{{"n":2}},"geometry":{{"type":"Point","coordinates":{bad}}}}},
            {{"type":"Feature","properties":{{"n":3}},"geometry":{{"type":"Point","coordinates":{good}}}}}]}}"#,
            named(crs)
        );
        let (ds, messages, info_crs) = geojson_paths("far.geojson", &text);
        assert_eq!(ds.features.len(), 2, "{crs}");
        assert_eq!(ds.features.iter().map(|f| f.id).collect::<Vec<_>>(), [1, 3], "{crs}: ids are the features' places in the file");
        assert_eq!(messages.len(), 1, "{crs}: {messages:?}");
        assert!(messages[0].contains(&format!("1 features couldn't be converted from {crs}")), "{}", messages[0]);
        assert_eq!(info_crs, crs);
        assert!(ds.crs_incomplete, "{crs}: a projection can't be defined again over lost features");
        assert!(!ds.zm_dropped, "{crs}");
        let Some(Geometry::Point(c)) = ds.features.first().and_then(|f| f.geometry.clone()) else { panic!("a point") };
        assert!(c.x.abs() < 180.0 && c.y.abs() < 90.0, "{crs}: {c:?}");
        if crs == "EPSG:32617" {
            assert!((c.x + 81.0).abs() < 1e-6 && (c.y - 42.0).abs() < 1e-3, "{c:?}");
        }
    }
}

/// A point in Web Mercator that converts, one that doesn't (longitude 8983°), and a polygon.
const MERC_OK: &str = r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1000000,6000000]}}"#;
const MERC_BAD: &str = r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1e9,0]}}"#;
const MERC_POLY: &str = r#"{"type":"Feature","properties":{},"geometry":{"type":"Polygon","coordinates":[[[1000000,6000000],[1100000,6000000],[1100000,6100000],[1000000,6000000]]]}}"#;

fn merc_collection(features: &[&str]) -> String {
    format!(r#"{{"type":"FeatureCollection","crs":{},"features":[{}]}}"#, named("EPSG:3857"), features.join(","))
}

#[test]
fn geojson_dropped_features_do_not_set_the_geometry_type() {
    // Two points that can't be placed and one polygon that can: a polygon layer, as read whole.
    let (ds, _, _) = geojson_paths("kinds.geojson", &merc_collection(&[MERC_BAD, MERC_POLY, MERC_BAD]));
    assert_eq!(ds.geometry_type, GeometryType::Polygon);
    assert_eq!(ds.features.len(), 1);
    // Nothing that can be placed: no geometry type at all, both ways.
    let (ds, messages, _) = geojson_paths("none.geojson", &merc_collection(&[MERC_BAD, MERC_BAD]));
    assert_eq!(ds.geometry_type, GeometryType::None);
    assert!(ds.features.is_empty() && ds.crs_incomplete);
    assert_eq!(messages.len(), 1, "{messages:?}");
}

#[test]
fn geojson_sampling_skips_dropped_features() {
    // Unplaceable features at every other place must not use up the sample: a read asking for
    // two of the three placeable ones gets two, and says it matched three.
    let text = merc_collection(&[MERC_BAD, MERC_OK, MERC_BAD, MERC_OK, MERC_BAD, MERC_OK]);
    let p = dir().join("geojson-crs").join("alternating.geojson");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, &text).unwrap();
    for in_memory in [crate::io::BYTES_FLOOR, 0] {
        let src = geojson::GeoJsonSource::open_with(&p, "x", in_memory).unwrap();
        assert_eq!(src.info().count, Some(3));
        let all = src.read(&Request { extent: None, limit: 100 }).unwrap();
        assert_eq!((all.features.len(), all.matched, all.skipped, all.complete), (3, 3, 0, true));
        let some = src.read(&Request { extent: None, limit: 2 }).unwrap();
        assert_eq!((some.features.len(), some.matched, some.skipped, some.complete), (2, 3, 0, false));
        assert!(some.features.iter().all(|f| f.geometry.is_some()));
        let ids: Vec<u64> = all.features.iter().map(|f| f.id).collect();
        assert_eq!(ids, [2, 4, 6]);
    }
}

#[test]
fn geojson_unreadable_feature_alone_does_not_make_the_layer_incomplete() {
    // A declared system, one feature with geometry that can't be read, and all the others
    // placed: nothing was lost to the system, so Define Projection stays available, both ways.
    let bad = r#"{"type":"Feature","properties":{},"geometry":{"type":"Nope"}}"#;
    let (ds, messages, _) = geojson_paths("unreadable.geojson", &merc_collection(&[MERC_OK, bad]));
    assert_eq!(ds.features.len(), 1);
    assert!(!ds.crs_incomplete);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("unreadable geometry"), "{}", messages[0]);
}

#[test]
fn damaged_geojson_is_an_error_not_a_panic() {
    let d = dir().join("geojson-junk");
    std::fs::create_dir_all(&d).unwrap();
    let text = crate::io::to_geojson(&export_sample()).to_string();
    let p = d.join("cut.geojson");
    for n in 0..text.len() {
        std::fs::write(&p, &text.as_bytes()[..n]).unwrap();
        for in_memory in [crate::io::BYTES_FLOOR, 0] {
            if let Ok(s) = geojson::GeoJsonSource::open_with(&p, "x", in_memory) {
                let _ = s.read(&Request::all());
            }
        }
    }
    // A file that changes after it's opened: features are skipped, not misread or a panic.
    std::fs::write(&p, &text).unwrap();
    let src = open_path(&p, None).unwrap();
    std::fs::write(&p, &text.as_bytes()[..text.len() / 2]).unwrap();
    let b = src.read(&Request::all()).unwrap();
    assert!(b.skipped > 0);
}

/// The issue #514 squares: ETRS89 / UTM zone 32N (EPSG:25832), or the same with the zone prefixed
/// to the easting (EPSG:4647).
fn utm_squares(code: u32) -> Dataset {
    let e0 = if code == 4647 { 32_500_000.0 } else { 500_000.0 };
    let text: String = std::iter::once("side;wkt\n".to_string())
        .chain([50.0, 100.0, 1000.0].iter().map(|s| {
            let (x0, y0, x1, y1) = (e0, 5_700_000.0, e0 + s, 5_700_000.0 + s);
            format!("{s} m;POLYGON (({x0} {y0}, {x1} {y0}, {x1} {y1}, {x0} {y1}, {x0} {y0}))\n")
        }))
        .collect();
    let opts = crate::io::CsvOptions {
        wkt: Some("wkt".into()),
        crs: Some(Crs::from_epsg(code).unwrap()),
        delimiter: Some(b';'),
        ..Default::default()
    };
    crate::io::read_csv_with(&text, "squares", &opts).unwrap().0
}

#[test]
fn exports_are_written_in_the_system_the_layer_was_read_in() {
    let d = dir().join("export-crs");
    std::fs::create_dir_all(&d).unwrap();
    for code in [25832u32, 4647] {
        let ds = utm_squares(code);
        let crs = Crs::from_epsg(code).unwrap();
        assert_eq!(ds.crs, Some(crs));
        let e0 = if code == 4647 { 32_500_000.0 } else { 500_000.0 };
        for ext in ["gpkg", "shp", "fgb"] {
            let p = d.join(format!("squares-{code}.{ext}"));
            crate::io::write_path_in(&ds, &p, crs).unwrap();
            // The file says the system, and reading it back gives the same places.
            let src = open_path(&p, None).unwrap();
            assert_eq!(src.info().crs, format!("EPSG:{code}"), "{ext}");
            let (back, _) = materialize(src.as_ref()).unwrap();
            assert_eq!(back.crs, Some(crs), "{ext}");
            // (FlatGeobuf orders features along its index.)
            for b in &back.features {
                let a = ds.features.iter().find(|a| a.attrs == b.attrs).unwrap();
                let (a, b) = (a.geometry.as_ref().unwrap().extent(), b.geometry.as_ref().unwrap().extent());
                assert!((a.min_x - b.min_x).abs() < 1e-9 && (a.max_y - b.max_y).abs() < 1e-9, "{ext}: {a:?} {b:?}");
            }
        }
        // The Shapefile holds the system's own coordinates, and a .prj GDAL reads by its code.
        let shp = d.join(format!("squares-{code}.shp"));
        let shapes = shapefile::read_shapes_as::<_, shapefile::Polygon>(&shp).unwrap();
        let b = shapes.last().unwrap().bbox();
        assert!((b.min.x - e0).abs() < 1e-6 && (b.max.x - e0 - 1000.0).abs() < 1e-6, "{b:?}");
        assert!((b.min.y - 5_700_000.0).abs() < 1e-6 && (b.max.y - 5_701_000.0).abs() < 1e-6, "{b:?}");
        let prj = std::fs::read_to_string(shp.with_extension("prj")).unwrap();
        assert!(prj.starts_with("PROJCS[") && prj.ends_with(&format!("AUTHORITY[\"EPSG\",\"{code}\"]]")), "{prj}");
    }
    // By default an export keeps the layer's system where the format can say it. A system without
    // an EPSG code goes in by its WKT definition, read back as the same system; one WKT can't
    // describe only into a Shapefile (which says so), else the layer is written in WGS 84, said.
    let mut ds = utm_squares(25832);
    let utm = Crs::from_epsg(25832).unwrap();
    assert_eq!(crate::io::default_export_crs(&ds, "GPKG"), utm);
    assert_eq!(crate::io::default_export_crs(&ds, "geojson"), Crs::LonLat);
    assert_eq!(crate::io::default_export_note(&ds, "a.gpkg"), None);
    let wkt =
        crate::raster::wkt::to_wkt(utm).unwrap().replace(",AUTHORITY[\"EPSG\",\"25832\"]", "").replace("ETRS89 / UTM zone 32N", "My grid");
    let custom = crate::raster::wkt::crs_from_wkt(&wkt).unwrap();
    assert_eq!(custom.epsg(), 0);
    ds.crs = Some(custom);
    for ext in ["shp", "gpkg", "fgb"] {
        assert_eq!(crate::io::default_export_crs(&ds, ext), custom, "{ext}");
        let p = d.join(format!("custom.{ext}"));
        crate::io::write_path_in(&ds, &p, custom).unwrap();
        if ext != "shp" {
            let (back, _) = materialize(open_path(&p, None).unwrap().as_ref()).unwrap();
            // The same system: its WKT is written as terrio-crs writes it, so it is read back
            // as a system of its own that places points alike.
            let back = back.crs.unwrap();
            assert_eq!(back.custom_def().map(|d| d.proj4.clone()), custom.custom_def().map(|d| d.proj4.clone()), "{ext}");
            let at = Coord::new(9.5, 51.5);
            let (a, b) = (back.transformer().unwrap().from_lonlat(at).unwrap(), custom.transformer().unwrap().from_lonlat(at).unwrap());
            assert!((a.x - b.x).hypot(a.y - b.y) < 1e-9, "{ext}: {a:?} {b:?}");
        }
        assert_eq!(crate::io::default_export_note(&ds, &p.display().to_string()), None, "{ext}");
    }
    let gnomonic = Crs::parse("+proj=gnom +lat_0=51 +lon_0=9 +datum=WGS84 +units=m +no_defs").unwrap();
    assert!(crate::raster::wkt::to_wkt(gnomonic).is_none());
    ds.crs = Some(gnomonic);
    assert_eq!(crate::io::default_export_crs(&ds, "shp"), gnomonic);
    for ext in ["gpkg", "fgb"] {
        assert_eq!(crate::io::default_export_crs(&ds, ext), Crs::LonLat, "{ext}");
        let note = crate::io::default_export_note(&ds, &format!("a.{ext}")).unwrap();
        assert!(note.contains("WGS 84") && note.contains(&gnomonic.label()), "{note}");
        // Asked for, it is refused: the file would read back as longitude/latitude.
        assert!(crate::io::write_path_in(&ds, &d.join(format!("gnom.{ext}")), gnomonic).is_err(), "{ext}");
    }
    assert_eq!(crate::io::default_export_note(&ds, "a.shp"), None);
    // GeoJSON is longitude/latitude only.
    let ds = utm_squares(25832);
    assert!(crate::io::write_path_in(&ds, &d.join("squares.geojson"), Crs::from_epsg(25832).unwrap()).is_err());
}

#[test]
fn a_geoparquet_export_keeps_a_system_with_an_epsg_code() {
    let d = dir().join("export-parquet-crs");
    std::fs::create_dir_all(&d).unwrap();
    let ds = utm_squares(25832);
    let utm = Crs::from_epsg(25832).unwrap();
    for ext in ["parquet", "geoparquet"] {
        assert_eq!(crate::io::default_export_crs(&ds, ext), utm, "{ext}");
        assert_eq!(crate::io::default_export_note(&ds, &format!("a.{ext}")), None, "{ext}");
        let p = d.join(format!("utm.{ext}"));
        crate::io::write_path_in(&ds, &p, utm).unwrap();
        // Read back in the same system, at the same coordinates, by its EPSG code.
        let (back, _) = materialize(open_path(&p, None).unwrap().as_ref()).unwrap();
        assert_eq!(back.crs, Some(utm), "{ext}");
        assert_eq!(back.coord_crs(), utm, "{ext}");
        let coords = |ds: &Dataset| {
            let mut v = Vec::new();
            for f in &ds.features {
                f.geometry.as_ref().unwrap().for_each_coord(|c| v.push((c.x, c.y)));
            }
            v
        };
        assert_eq!(coords(&back), coords(&ds), "{ext}");
        // The written bytes are the whole-file and the in-memory writer alike.
        let (bytes, _) = crate::io::write_bytes_in(&ds, ext, utm).unwrap();
        let (mem, _) = crate::io::read_bytes(&format!("m.{ext}"), &bytes).unwrap();
        assert_eq!(mem.crs, Some(utm), "{ext}");
    }
    // Longitude/latitude writes the file as before: no `crs` on the geometry column.
    let lonlat = crate::io::project(&ds, Crs::LonLat).unwrap();
    let mut plain = lonlat.clone();
    plain.crs = None;
    let (a, _) = crate::io::write_bytes_in(&plain, "parquet", Crs::LonLat).unwrap();
    assert_eq!(a, crate::source::parquet::write(&plain).unwrap());
    let geo = |bytes: &[u8]| {
        use ::parquet::file::reader::FileReader;
        let r = ::parquet::file::reader::SerializedFileReader::new(bytes::Bytes::copy_from_slice(bytes)).unwrap();
        let kv = r.metadata().file_metadata().key_value_metadata().unwrap().clone();
        let geo = kv.into_iter().find(|k| k.key == "geo").unwrap().value.unwrap();
        serde_json::from_str::<serde_json::Value>(&geo).unwrap()
    };
    assert!(geo(&a)["columns"]["geometry"].get("crs").is_none());
    let (b, _) = crate::io::write_bytes_in(&ds, "parquet", utm).unwrap();
    assert_eq!(geo(&b)["columns"]["geometry"]["crs"]["id"]["code"], 25832);
    // A system without an EPSG code: written in WGS 84 by default (said), refused when asked for.
    let wkt =
        crate::raster::wkt::to_wkt(utm).unwrap().replace(",AUTHORITY[\"EPSG\",\"25832\"]", "").replace("ETRS89 / UTM zone 32N", "My grid");
    let custom = crate::raster::wkt::crs_from_wkt(&wkt).unwrap();
    let mut own = ds.clone();
    own.crs = Some(custom);
    assert_eq!(crate::io::default_export_crs(&own, "parquet"), Crs::LonLat);
    let note = crate::io::default_export_note(&own, "a.parquet").unwrap();
    assert!(note.contains("has no EPSG code,") && note.contains("WGS 84"), "{note}");
    let e = crate::io::write_path_in(&own, &d.join("custom.parquet"), custom).unwrap_err().to_string();
    assert!(e.contains("no EPSG code"), "{e}");
}

#[test]
fn a_csv_export_takes_x_and_y_in_a_system_asked_for() {
    use crate::{Field, FieldType, Value};
    let d = dir().join("export-csv-crs");
    std::fs::create_dir_all(&d).unwrap();
    let lv95 = Crs::from_epsg(2056).unwrap();
    let mut ds = Dataset::new("pts", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    ds.push(Some(Geometry::Point(Coord::new(2_600_000.5, 1_200_000.25))), vec![Value::Text("Bern".into())]);
    ds.crs = Some(lv95);
    ds.held_in = lv95;
    // By default it stays longitude/latitude, as before.
    assert_eq!(crate::io::default_export_crs(&ds, "csv"), Crs::LonLat);
    assert!(crate::io::holds_a_system("CSV") && !crate::io::holds_a_system("geojson"));
    let lonlat = String::from_utf8(crate::io::write_bytes(&ds, "csv").unwrap().0).unwrap();
    assert!(lonlat.starts_with("longitude,latitude,name\n7.43"), "{lonlat}");
    assert_eq!(crate::io::csv_system_note("a.csv", ds.geometry_type, Crs::LonLat), None);
    // Asked for, its own coordinates go in x and y, exactly.
    let p = d.join("pts.csv");
    crate::io::write_path_in(&ds, &p, lv95).unwrap();
    let text = std::fs::read_to_string(&p).unwrap();
    assert_eq!(text, "x,y,name\n2600000.5,1200000.25,Bern\n");
    let (bytes, _) = crate::io::write_bytes_in(&ds, "csv", lv95).unwrap();
    assert_eq!(String::from_utf8(bytes).unwrap(), text);
    // Read back with the system given, at the same place.
    let opts = crate::io::CsvOptions { crs: Some(lv95), ..Default::default() };
    let (back, _) = crate::io::read_csv_with(&text, "pts", &opts).unwrap();
    let mut at = Vec::new();
    back.features[0].geometry.as_ref().unwrap().for_each_coord(|c| at.push((c.x, c.y)));
    let mut want = Vec::new();
    ds.features[0].geometry.as_ref().unwrap().for_each_coord(|c| want.push((c.x, c.y)));
    let back_lonlat = crate::io::project(&back, Crs::LonLat).unwrap();
    let ds_lonlat = crate::io::project(&ds, Crs::LonLat).unwrap();
    let pt = |ds: &Dataset| match ds.features[0].geometry {
        Some(Geometry::Point(c)) => c,
        _ => panic!(),
    };
    assert!((pt(&back_lonlat).x - pt(&ds_lonlat).x).abs() < 1e-9 && (pt(&back_lonlat).y - pt(&ds_lonlat).y).abs() < 1e-9);
    // The file can't say its system, so that is said.
    let note = crate::io::csv_system_note("pts.csv", ds.geometry_type, lv95).unwrap();
    assert!(note.contains("EPSG:2056") && note.contains("can't say"), "{note}");
    assert_eq!(crate::io::csv_system_note("pts.gpkg", ds.geometry_type, lv95), None);
    // Fields named x and y push the columns to easting and northing; every name taken is said.
    let mut xy = Dataset::new("xy", GeometryType::Point, vec![Field::new("X", FieldType::Integer), Field::new("y", FieldType::Integer)]);
    xy.push(Some(Geometry::Point(Coord::new(2_600_000.0, 1_200_000.0))), vec![Value::Int(1), Value::Int(2)]);
    xy.crs = Some(lv95);
    xy.held_in = lv95;
    let text = crate::io::to_csv_in(&xy, lv95).unwrap();
    assert!(text.starts_with("easting,northing,X,y\n"), "{text}");
    assert!(crate::io::export_losses_notes_in(&xy, "csv", None, "xy.csv", lv95).unwrap().is_empty());
    let mut every = xy.clone();
    every.fields.push(Field::new("easting", FieldType::Integer));
    every.fields.push(Field::new("northing", FieldType::Integer));
    for f in &mut every.features {
        f.attrs.extend([Value::Int(3), Value::Int(4)]);
    }
    assert!(crate::io::to_csv_in(&every, lv95).unwrap().starts_with("x_1,y_1,"));
    let notes = crate::io::export_losses_notes_in(&every, "csv", None, "xy.csv", lv95).unwrap();
    assert!(notes.iter().any(|n| n.contains("'x', 'easting', 'y' and 'northing'") && n.contains("'x_1' and 'y_1'")), "{notes:?}");
    // Each axis on its own: 'x' and 'easting' taken move only the x column, and only those two
    // names are said.
    let mut one =
        Dataset::new("one", GeometryType::Point, vec![Field::new("x", FieldType::Integer), Field::new("easting", FieldType::Integer)]);
    one.push(Some(Geometry::Point(Coord::new(2_600_000.0, 1_200_000.0))), vec![Value::Int(1), Value::Int(2)]);
    one.crs = Some(lv95);
    one.held_in = lv95;
    assert!(crate::io::to_csv_in(&one, lv95).unwrap().starts_with("x_1,y,x,easting\n"));
    let notes = crate::io::export_losses_notes_in(&one, "csv", None, "one.csv", lv95).unwrap();
    assert!(
        notes.iter().any(|n| n.contains("the names 'x' and 'easting', so") && n.contains("'x_1' and 'y'") && !n.contains("northing")),
        "{notes:?}"
    );
    // Into a geographic system other than WGS 84: longitude and latitude columns, and no note
    // of unusable positions, which are counted on the coordinates written.
    // Fields named x and y are a pair reading could take instead, which a note names when rows
    // lack a usable longitude/latitude: counted on LV95's metres, every row would.
    let etrs89 = Crs::from_epsg(4258).unwrap();
    let (bytes, _) = crate::io::write_bytes_in(&ds, "csv", etrs89).unwrap();
    assert!(String::from_utf8(bytes).unwrap().starts_with("longitude,latitude,name\n7.43"));
    assert!(crate::io::export_losses_notes_in(&ds, "csv", None, "pts.csv", etrs89).unwrap().is_empty());
    assert_eq!(xy.coord_crs(), lv95);
    let notes = crate::io::export_losses_notes_in(&xy, "csv", None, "xy.csv", etrs89).unwrap();
    assert!(notes.is_empty(), "{notes:?}");
}

/// A catalog index as AlphaEarth's is (a `bbox` struct, a nested `assets.data.href`), and patch
/// embeddings as Earth Index's are (a fixed-size list of floats a point), in row groups of 50.
fn write_index_and_vectors() -> Vec<u8> {
    use arrow_array::builder::{FixedSizeListBuilder, Float32Builder};
    use arrow_array::{BinaryArray, Float64Array, RecordBatch, StringArray, StructArray};
    use arrow_schema::{DataType, Field as AField, Fields, Schema};
    let pts = points();
    let wkbs: Vec<Vec<u8>> = pts.iter().map(|(c, ..)| wkb::encode(&Geometry::Point(*c))).collect();
    let bbox_fields: Fields =
        ["xmin", "ymin", "xmax", "ymax"].map(|n| Arc::new(AField::new(n, DataType::Float64, false))).into_iter().collect();
    let coord =
        |f: fn(&Coord) -> f64| Arc::new(Float64Array::from(pts.iter().map(|(c, ..)| f(c)).collect::<Vec<_>>())) as arrow_array::ArrayRef;
    let bbox = StructArray::new(bbox_fields.clone(), vec![coord(|c| c.x), coord(|c| c.y), coord(|c| c.x), coord(|c| c.y)], None);
    let href =
        Arc::new(StringArray::from(pts.iter().map(|p| format!("s3://bucket/{}.tiff", p.1)).collect::<Vec<_>>())) as arrow_array::ArrayRef;
    let data_fields: Fields = vec![Arc::new(AField::new("href", DataType::Utf8, true))].into_iter().collect();
    let data = Arc::new(StructArray::new(data_fields.clone(), vec![href], None)) as arrow_array::ArrayRef;
    let asset_fields: Fields = vec![Arc::new(AField::new("data", DataType::Struct(data_fields), true))].into_iter().collect();
    let assets = StructArray::new(asset_fields.clone(), vec![data], None);
    let mut emb = FixedSizeListBuilder::new(Float32Builder::new(), 4);
    for (_, _, v) in &pts {
        for k in 0..4 {
            emb.values().append_value((*v * 10 + k) as f32);
        }
        emb.append(true);
    }
    let emb = emb.finish();
    let schema = Arc::new(Schema::new(vec![
        AField::new("geometry", DataType::Binary, false),
        AField::new("bbox", DataType::Struct(bbox_fields), false),
        AField::new("assets", DataType::Struct(asset_fields), true),
        AField::new("embedding", arrow_array::Array::data_type(&emb).clone(), true),
    ]));
    let cols: Vec<arrow_array::ArrayRef> = vec![
        Arc::new(BinaryArray::from(wkbs.iter().map(|w| w.as_slice()).collect::<Vec<_>>())),
        Arc::new(bbox),
        Arc::new(assets),
        Arc::new(emb),
    ];
    let geo = r#"{"version":"1.0.0","primary_column":"geometry","columns":{"geometry":{"encoding":"WKB","geometry_types":["Point"]}}}"#;
    let props = ::parquet::file::properties::WriterProperties::builder()
        .set_max_row_group_row_count(Some(50))
        .set_key_value_metadata(Some(vec![::parquet::file::metadata::KeyValue::new("geo".to_string(), geo.to_string())]))
        .build();
    let mut out = Vec::new();
    let mut w = ::parquet::arrow::ArrowWriter::try_new(&mut out, schema.clone(), Some(props)).unwrap();
    w.write(&RecordBatch::try_new(schema, cols).unwrap()).unwrap();
    w.close().unwrap();
    out
}

#[test]
fn nested_columns_and_vectors_by_address() {
    let bytes = write_index_and_vectors();
    let url = "https://test.example/index.parquet";
    remote::testing::serve(url, bytes.clone());
    let e = Extent::new(10.0, 10.0, 40.0, 30.0);
    // The rows in the extent, with the nested column as a field.
    let (fields, batch) =
        query_url_blocking(url, vec!["assets.data.href".into(), "no.such".into()], Request { extent: Some(e), limit: 1000 }).unwrap();
    let k = fields.iter().position(|f| f.name == "assets.data.href").unwrap();
    assert!(!fields.iter().any(|f| f.name == "no.such"));
    assert_eq!(batch.features.len(), in_box(&e));
    let names: Vec<String> = points().iter().filter(|(c, ..)| e.contains(*c)).map(|p| format!("s3://bucket/{}.tiff", p.1)).collect();
    for f in &batch.features {
        assert!(names.contains(&f.attrs[k].to_string()), "{:?}", f.attrs[k]);
    }
    // The points in the extent with their vectors: four values each, as written.
    let v = vectors_url_blocking(url, e, "embedding".into(), (100, 10_000)).unwrap();
    assert_eq!((v.points.len(), v.dims, v.values.len()), (in_box(&e), 4, in_box(&e) * 4));
    for (i, p) in v.points.iter().enumerate() {
        let (_, _, id) = points().into_iter().find(|(c, ..)| (c.x, c.y) == *p).unwrap();
        assert_eq!(&v.values[i * 4..i * 4 + 4], &[(id * 10) as f32, (id * 10 + 1) as f32, (id * 10 + 2) as f32, (id * 10 + 3) as f32]);
    }
    // Past the limits: said, not read.
    let err = vectors_url_blocking(url, Extent::new(-180.0, -90.0, 180.0, 90.0), "embedding".into(), (2, 10_000)).unwrap_err();
    assert!(err.to_string().contains("zoom in"), "{err}");
    assert!(vectors_url_blocking(url, e, "nothing".into(), (100, 10_000)).is_err());
    // A cut file: errors, never a panic.
    for n in [0, 10, 100, bytes.len() / 2] {
        let cut = format!("https://test.example/cut-{n}.parquet");
        remote::testing::serve(&cut, bytes[bytes.len() - n..].to_vec());
        let _ = vectors_url_blocking(&cut, e, "embedding".into(), (100, 10_000));
    }
}

#[test]
fn shapefile_complete_chunks_keep_all_rows_order_attributes_and_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunks.shp");
    let mut ds = Dataset::new("chunks", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    for i in 0..523 {
        ds.push(Some(Geometry::Point(Coord::new(i as f64 * 0.01, 1.0))), vec![Value::Text(format!("point {i}"))]);
    }
    write_shapefile(&ds, &path);
    let source = shp::ShpSource::open_path(&path, "chunks").unwrap();
    let expected = source.read(&Request::all()).unwrap();
    assert!(source.streams_chunks());
    for budget in [256, 1024, 8192] {
        let mut rows = Vec::new();
        let mut calls = 0;
        let summary = source
            .visit_chunks(None, budget, &mut |chunk| {
                calls += 1;
                assert!(chunk_memory::features_bytes(&chunk) <= budget);
                rows.extend(chunk);
                Ok(())
            })
            .unwrap();
        assert!(calls > 1);
        assert!(summary.complete);
        assert!(summary.features.is_empty());
        assert_eq!(summary.matched, 523);
        assert_eq!(rows, expected.features);
    }
    let mut oversized = Vec::new();
    source
        .visit_chunks(None, 1, &mut |chunk| {
            assert_eq!(chunk.len(), 1);
            oversized.extend(chunk);
            Ok(())
        })
        .unwrap();
    assert_eq!(oversized, expected.features, "indivisible rows must remain complete");
    let mut calls = 0;
    assert!(
        source
            .visit_chunks(None, 256, &mut |_| {
                calls += 1;
                Err(DataError::Format("cancelled".into()))
            })
            .is_err()
    );
    assert_eq!(calls, 1);
    assert!(source.visit_chunks(None, 0, &mut |_| { panic!("zero-budget traversal must not emit a row") }).is_err());
    assert!(source.visit_chunks(Some(Extent::EMPTY), 0, &mut |_| panic!("zero budget must not emit")).is_err());
    let empty_path = dir.path().join("empty.shp");
    // The production exporter deliberately refuses an empty layer. Build a valid zero-record
    // fixture from its headers rather than changing that existing export contract.
    for ext in ["shp", "shx"] {
        let mut header = std::fs::read(path.with_extension(ext)).unwrap();
        header.truncate(100);
        header[24..28].copy_from_slice(&50i32.to_be_bytes());
        std::fs::write(empty_path.with_extension(ext), header).unwrap();
    }
    let mut header = std::fs::read(path.with_extension("dbf")).unwrap();
    let len = u16::from_le_bytes([header[8], header[9]]) as usize;
    header.truncate(len);
    header[4..8].copy_from_slice(&0u32.to_le_bytes());
    header.push(0x1a);
    std::fs::write(empty_path.with_extension("dbf"), header).unwrap();
    let empty = shp::ShpSource::open_path(&empty_path, "empty").unwrap();
    assert!(empty.visit_chunks(None, 0, &mut |_| panic!("zero budget must not emit")).is_err());
    let extent = Some(Extent::new(0.02, 0.0, 0.1, 2.0));
    let expected = source.read(&Request { extent, limit: 1000 }).unwrap();
    let mut rows = Vec::new();
    source
        .visit_chunks(extent, 256, &mut |chunk| {
            rows.extend(chunk);
            Ok(())
        })
        .unwrap();
    assert_eq!(rows, expected.features);
}

/// GeoPackage rows without geometry (NULL, or an empty geometry) keep their row and attributes in
/// whole reads and visits, with and without the R-tree and in the browser's reader, and are left
/// out of a read of an extent; one flagged empty with an unreadable header is skipped and counted.
#[test]
fn geopackage_keeps_rows_without_geometry() {
    let d = dir().join("geometryless");
    std::fs::create_dir_all(&d).unwrap();
    let (indexed, scan) = (d.join("indexed.gpkg"), d.join("scan.gpkg"));
    for (p, rtree) in [(&indexed, true), (&scan, false)] {
        write_gpkg(p, rtree);
        let c = rusqlite::Connection::open(p).unwrap();
        let empty = b"GP\x00\x11\x11\x0f\x00\x00".to_vec();
        c.execute("INSERT INTO \"grid \"\"q\"\"\" VALUES (601, NULL, 'no place', 7, 1)", []).unwrap();
        c.execute("INSERT INTO \"grid \"\"q\"\"\" VALUES (602, ?1, 'empty', 8, 0)", [empty]).unwrap();
        // Flagged empty but unreadable, each skipped and counted: cut off before its header ends,
        // with an envelope code the standard doesn't define, an envelope declared but missing, and a
        // header version the standard doesn't define.
        for (fid, blob) in [(603, "47500011"), (604, "4750001b0f000000"), (605, "475000130f000000"), (606, "475001110f000000")] {
            c.execute(&format!("INSERT INTO \"grid \"\"q\"\"\" VALUES ({fid}, X'{blob}', 'cut', 9, 1)"), []).unwrap();
        }
    }
    let sources: Vec<Box<dyn FeatureSource>> = vec![
        open_path(&indexed, None).unwrap(),
        open_path(&scan, None).unwrap(),
        Box::new(gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(&scan).unwrap())), None).unwrap()),
    ];
    for src in &sources {
        let format = src.info().format;
        assert_eq!(src.info().count, Some(606), "{format}");
        let whole = src.read(&Request::all()).unwrap();
        assert_eq!((whole.features.len(), whole.skipped), (602, 4), "{format}");
        let bare: Vec<_> = whole.features.iter().filter(|f| f.geometry.is_none()).map(|f| (f.id, f.attrs[0].clone())).collect();
        assert_eq!(bare, vec![(601, Value::Text("no place".into())), (602, Value::Text("empty".into()))], "{format}");
        let (rows, summary, _) = visit_all(src.as_ref(), None, 1024);
        assert_eq!((rows, summary.matched, summary.skipped), (whole.features, 602, 4), "{format}");
        let world = Extent::new(-180.0, -85.0, 180.0, 85.0);
        let part = src.read(&Request { extent: Some(world), limit: 10_000 }).unwrap();
        assert_eq!(part.features.len(), 600, "{format}");
        assert_eq!(visit_all(src.as_ref(), Some(world), 1024).0.len(), 600, "{format}");
    }
}

/// Every feature of a complete traversal, in order, checking each chunk against the budget.
fn visit_all(src: &dyn FeatureSource, extent: Option<Extent>, budget: usize) -> (Vec<Feature>, Batch, usize) {
    let (mut rows, mut calls) = (Vec::new(), 0);
    let summary = src
        .visit_chunks(extent, budget, &mut |chunk| {
            calls += 1;
            // Only a row that alone exceeds the budget comes over it, by itself.
            let alone_too_big = |c: &Vec<Feature>| {
                c.len() == 1 && c.iter().all(|f| chunk_memory::feature_bytes(f) + std::mem::size_of::<Feature>() > budget)
            };
            assert!(chunk_memory::features_bytes(&chunk) <= budget || alone_too_big(&chunk), "{}", src.info().format);
            rows.extend(chunk);
            Ok(())
        })
        .unwrap();
    (rows, summary, calls)
}

/// GeoPackage, FlatGeobuf and GeoParquet visit, a chunk at a time, exactly the features a whole
/// read gives (ids, attributes, order and skipped rows), for exports. They still draw from a
/// view window: a complete traversal on every render would be far slower.
#[test]
fn geopackage_flatgeobuf_and_geoparquet_visit_what_a_whole_read_gives() {
    let d = dir().join("visit");
    std::fs::create_dir_all(&d).unwrap();
    let (gpkg, gpkg_scan, fgb, pq, pq_plain) =
        (d.join("grid.gpkg"), d.join("scan.gpkg"), d.join("grid.fgb"), d.join("grid.parquet"), d.join("plain.parquet"));
    write_gpkg(&gpkg, true);
    write_gpkg(&gpkg_scan, false);
    write_fgb(&fgb);
    write_parquet(&pq, true);
    write_parquet(&pq_plain, false);
    let mut sources: Vec<Box<dyn FeatureSource>> = Vec::new();
    for p in [&gpkg, &gpkg_scan, &fgb, &pq, &pq_plain] {
        sources.push(open_path(p, None).unwrap());
    }
    for p in [&gpkg, &fgb, &pq] {
        sources.push(open_bytes(&p.file_name().unwrap().to_string_lossy(), std::fs::read(p).unwrap(), None).unwrap());
    }
    // The browser's GeoPackage reader.
    sources.push(Box::new(gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(&gpkg_scan).unwrap())), None).unwrap()));
    let extent = Extent::new(-100.0, -30.0, 20.0, 45.0);
    let by_id = |mut v: Vec<Feature>| {
        v.sort_by_key(|f| f.id);
        v
    };
    for src in &sources {
        let format = src.info().format;
        assert!(src.visits_chunks() && !src.streams_chunks(), "{format}");
        let whole = src.read(&Request::all()).unwrap();
        assert!(whole.complete && whole.features.len() == 600, "{format}");
        for budget in [256, 4096, 1 << 20] {
            let (rows, summary, calls) = visit_all(src.as_ref(), None, budget);
            assert_eq!(rows, whole.features, "{format} at {budget} bytes");
            assert!(summary.complete && summary.features.is_empty(), "{format}");
            assert_eq!((summary.matched, summary.skipped), (600, whole.skipped), "{format}");
            assert!(budget > 4096 || calls > 1, "{format} at {budget} bytes came in one chunk");
        }
        // Each row alone when none fits the budget.
        assert_eq!(visit_all(src.as_ref(), None, 1).0, whole.features, "{format}");
        // An extent: the features a complete read of it gives (the R-tree's order may differ).
        let part = src.read(&Request { extent: Some(extent), limit: 10_000 }).unwrap();
        assert!(part.complete && part.features.len() == in_box(&extent), "{format}");
        let (rows, summary, _) = visit_all(src.as_ref(), Some(extent), 1024);
        assert!(summary.complete && summary.features.is_empty() && summary.matched == rows.len() as u64, "{format}");
        if src.info().indexed {
            assert_eq!(by_id(rows), by_id(part.features), "{format}");
        } else {
            assert_eq!(rows, part.features, "{format}");
        }
        assert!(visit_all(src.as_ref(), Some(Extent::new(1.0, 1.0, 2.0, 2.0)), 1024).0.is_empty(), "{format}");
        // A visitor's error stops the traversal; a zero budget is refused before any row.
        let mut calls = 0;
        let stopped = src.visit_chunks(None, 256, &mut |_| {
            calls += 1;
            Err(DataError::Format("cancelled".into()))
        });
        assert!(stopped.is_err() && calls == 1, "{format}");
        assert!(src.visit_chunks(None, 0, &mut |_| panic!("a zero budget must not emit")).is_err(), "{format}");
        // The map's reads of the layer go on while a traversal waits on its visitor.
        src.visit_chunks(None, 256, &mut |_| {
            assert_eq!(src.read(&Request { extent: None, limit: 1 }).unwrap().features.len(), 1, "{format}");
            Ok(())
        })
        .unwrap();
    }
    // A row group longer than a read takes from one: a traversal refuses it rather than leave
    // its last rows out.
    let capped = parquet::ParquetSource::open_path(&pq_plain, "plain").unwrap().with_group_rows(40);
    let error = capped.visit_chunks(None, 4096, &mut |_| Ok(())).unwrap_err().to_string();
    assert!(error.contains("row group 0 has 50 rows, more than the 40"), "{error}");
    // A capped row group outside the extent is passed over, not refused.
    let capped = parquet::ParquetSource::open_path(&pq, "grid").unwrap().with_group_rows(40);
    assert!(visit_all(&capped, Some(Extent::new(178.0, 0.0, 179.0, 1.0)), 4096).0.is_empty());
    // A GeoPackage put in place of the one added is refused, not read as the layer.
    #[cfg(unix)]
    {
        let src = open_path(&gpkg, None).unwrap();
        let other = d.join("other.gpkg");
        write_gpkg(&other, false);
        std::fs::rename(&other, &gpkg).unwrap();
        let error = src.visit_chunks(None, 4096, &mut |_| Ok(())).unwrap_err().to_string();
        assert!(error.contains("replaced"), "{error}");
        assert_eq!(src.read(&Request::all()).unwrap().features.len(), 600);
        // A file removed is missing, not replaced.
        std::fs::remove_file(&gpkg).unwrap();
        let error = src.visit_chunks(None, 4096, &mut |_| Ok(())).unwrap_err();
        assert!(matches!(&error, crate::DataError::Io(e) if e.kind() == std::io::ErrorKind::NotFound), "{error}");
    }
    // An unreadable geometry is skipped and counted, and a row without one kept, as a read does.
    drop(sources);
    let c = rusqlite::Connection::open(&gpkg_scan).unwrap();
    c.execute_batch("INSERT INTO \"grid \"\"q\"\"\" (geom, name) VALUES (X'4750000300000000DEAD', 'bad'), (NULL, 'none');").unwrap();
    drop(c);
    let src = open_path(&gpkg_scan, None).unwrap();
    let whole = src.read(&Request::all()).unwrap();
    assert_eq!((whole.features.len(), whole.skipped), (601, 1));
    let (rows, summary, _) = visit_all(src.as_ref(), None, 4096);
    assert_eq!((rows, summary.skipped), (whole.features, 1));
    // The browser's reader of the same file skips and keeps the same rows.
    let mem = gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(&gpkg_scan).unwrap())), None).unwrap();
    let whole = mem.read(&Request::all()).unwrap();
    assert_eq!((whole.features.len(), whole.skipped), (601, 1));
    let (rows, summary, _) = visit_all(&mem, None, 4096);
    assert_eq!((rows, summary.skipped), (whole.features, 1));
}

/// GeoPackage (desktop and web readers) and GeoParquet tables that declare Z or M for every
/// geometry say they lose them, counting the features; the same tables declaring 2D say nothing.
#[test]
fn declared_z_and_m_are_said_and_counted() {
    let d = tempfile::tempdir().unwrap();
    let says = |info: &SourceInfo| info.zm_dropped && info.notes.says(&crate::zm_note(Some(600)));
    let quiet = |info: &SourceInfo| !info.zm_dropped && !info.notes.iter().any(|n| n.contains("Z or M"));

    let gpkg = d.path().join("grid.gpkg");
    write_gpkg(&gpkg, false);
    assert!(quiet(open_path(&gpkg, None).unwrap().info()));
    let mem = |path: &Path| gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(path).unwrap())), None).unwrap();
    assert!(quiet(mem(&gpkg).info()));
    rusqlite::Connection::open(&gpkg).unwrap().execute("UPDATE gpkg_geometry_columns SET z = 1", []).unwrap();
    let info = open_path(&gpkg, None).unwrap().info().clone();
    assert!(says(&info), "{:?}", info.notes);
    // The web reader doesn't scan for the count, and says so without one.
    let info = mem(&gpkg).info().clone();
    assert!(info.zm_dropped && info.notes.says(&crate::zm_note(None)), "{:?}", info.notes);

    let parquet = d.path().join("grid.parquet");
    write_parquet_typed(&parquet, false, r#"["Point"]"#);
    assert!(quiet(open_path(&parquet, None).unwrap().info()));
    write_parquet_typed(&parquet, false, r#"["Point Z"]"#);
    let info = open_path(&parquet, None).unwrap().info().clone();
    assert!(says(&info), "{:?}", info.notes);
    // Some types with Z, some without: said, without a count.
    write_parquet_typed(&parquet, false, r#"["Point", "Point Z"]"#);
    let info = open_path(&parquet, None).unwrap().info().clone();
    assert!(info.zm_dropped && info.notes.says(&crate::zm_note(None)), "{:?}", info.notes);
}

/// A GeoParquet whose columns RustGIS has no field type for (a decimal too wide for a 64-bit
/// integer, binary, a list, a struct, a leaf beside the bbox covering's box) are said and left
/// out, while dates and timestamps become native values; the box's
/// zmin and zmax aren't, and a file of ordinary columns says nothing.
#[test]
fn geoparquet_columns_not_kept_or_read_as_numbers_are_said() {
    use arrow_array::builder::{Int32Builder, ListBuilder};
    use arrow_array::{
        BinaryArray, Date32Array, Decimal128Array, Float64Array, RecordBatch, StringArray, StructArray, TimestampMicrosecondArray,
        TimestampMillisecondArray,
    };
    use arrow_schema::{DataType, Field as AField, Fields, Schema, TimeUnit};
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("columns.parquet");
    let wkb = wkb::encode(&Geometry::Point(Coord::new(1.0, 2.0)));
    let mut tags = ListBuilder::new(Int32Builder::new());
    tags.values().append_value(7);
    tags.append(true);
    let tags = tags.finish();
    let inner: Fields = vec![Arc::new(AField::new("a", DataType::Int64, true))].into_iter().collect();
    let extra = StructArray::new(inner.clone(), vec![Arc::new(arrow_array::Int64Array::from(vec![1]))], None);
    // The covering's box, with z bounds (part of the box) and a label (not).
    let mut box_fields: Vec<Arc<AField>> =
        ["xmin", "ymin", "xmax", "ymax", "zmin", "zmax"].map(|n| Arc::new(AField::new(n, DataType::Float64, false))).into();
    box_fields.push(Arc::new(AField::new("label", DataType::Utf8, true)));
    let mut box_cols: Vec<arrow_array::ArrayRef> =
        [1.0, 2.0, 1.0, 2.0, 0.0, 0.0].map(|v| Arc::new(Float64Array::from(vec![v])) as _).into();
    box_cols.push(Arc::new(StringArray::from(vec!["a"])));
    let box_fields: Fields = box_fields.into_iter().collect();
    let bbox = StructArray::new(box_fields.clone(), box_cols, None);
    let schema = Arc::new(Schema::new(vec![
        AField::new("name", DataType::Utf8, true),
        AField::new("price", DataType::Decimal128(30, 2), true),
        AField::new("photo", DataType::Binary, true),
        AField::new("tags", arrow_array::Array::data_type(&tags).clone(), true),
        AField::new("extra", DataType::Struct(inner), true),
        AField::new("seen", DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())), true),
        AField::new("logged", DataType::Timestamp(TimeUnit::Microsecond, None), true),
        AField::new("day", DataType::Date32, true),
        AField::new("geometry", DataType::Binary, false),
        AField::new("geometry_bbox", DataType::Struct(box_fields), false),
    ]));
    let cols: Vec<arrow_array::ArrayRef> = vec![
        Arc::new(StringArray::from(vec!["a"])),
        Arc::new(Decimal128Array::from(vec![12345]).with_precision_and_scale(30, 2).unwrap()),
        Arc::new(BinaryArray::from(vec![&b"\x89PNG"[..]])),
        Arc::new(tags),
        Arc::new(extra),
        Arc::new(TimestampMillisecondArray::from(vec![1_790_856_000_000]).with_timezone("UTC")),
        Arc::new(TimestampMicrosecondArray::from(vec![1_790_856_000_000_000])),
        Arc::new(Date32Array::from(vec![20_727])),
        Arc::new(BinaryArray::from(vec![wkb.as_slice()])),
        Arc::new(bbox),
    ];
    let geo = r#"{"version":"1.1.0","primary_column":"geometry","columns":{"geometry":{"encoding":"WKB","geometry_types":["Point"],
        "covering":{"bbox":{"xmin":["geometry_bbox","xmin"],"ymin":["geometry_bbox","ymin"],"xmax":["geometry_bbox","xmax"],"ymax":["geometry_bbox","ymax"]}}}}}"#;
    let props = ::parquet::file::properties::WriterProperties::builder()
        .set_key_value_metadata(Some(vec![::parquet::file::metadata::KeyValue::new("geo".to_string(), geo.to_string())]))
        .build();
    let mut w = ::parquet::arrow::ArrowWriter::try_new(std::fs::File::create(&path).unwrap(), schema.clone(), Some(props)).unwrap();
    w.write(&RecordBatch::try_new(schema, cols).unwrap()).unwrap();
    w.close().unwrap();

    let left_out = "5 columns of types RustGIS has no field for were left out: \
                    price (decimal), photo (binary), tags (list), extra (struct), geometry_bbox (struct)";
    // Read in place: the source's notes, which adding the layer gives as notices.
    let src = open_path(&path, None).unwrap();
    let info = src.info();
    assert_eq!(info.notes, vec![left_out.to_string()]);
    let names: Vec<&str> = info.fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["name", "seen", "logged", "day"]);
    // Read whole: the same, in the read's report, and the values as said.
    let (ds, report) = materialize(src.as_ref()).unwrap();
    assert_eq!(report.messages, vec![format!("columns: {left_out}")]);
    assert_eq!(
        ds.features[0].attrs,
        vec![
            Value::Text("a".into()),
            Value::DateTime(crate::DateTime::with_offset(1_790_856_000_000_000, 0).unwrap()),
            Value::DateTime(crate::DateTime::local(1_790_856_000_000_000).unwrap()),
            Value::Date(crate::Date::from_days(20_727).unwrap())
        ]
    );

    // Ordinary columns, with a bbox covering: nothing to say.
    let clean = d.path().join("clean.parquet");
    write_parquet(&clean, true);
    let src = open_path(&clean, None).unwrap();
    assert!(src.info().notes.is_empty(), "{:?}", src.info().notes);
    assert!(materialize(src.as_ref()).unwrap().1.messages.is_empty());
}

/// A FlatGeobuf's binary column, which RustGIS has no field type for, is said and left out, the
/// columns after it keeping their values; a file without one says nothing.
#[test]
fn flatgeobuf_binary_columns_are_said_and_left_out() {
    use flatgeobuf::geozero::{ColumnValue, PropertyProcessor};
    use flatgeobuf::{ColumnType, FgbWriter, GeometryType as G};
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("blobs.fgb");
    let mut w = FgbWriter::create("blobs", G::Point).unwrap();
    w.add_column("name", ColumnType::String, |_, _| {});
    w.add_column("blob", ColumnType::Binary, |_, _| {});
    w.add_column("value", ColumnType::Long, |_, _| {});
    let g = flatgeobuf::geozero::wkb::Wkb(wkb::encode(&Geometry::Point(Coord::new(1.0, 2.0))));
    w.add_feature_geom(g, |f| {
        f.property(0, "name", &ColumnValue::String("a")).unwrap();
        f.property(1, "blob", &ColumnValue::Binary(b"\x89PNG")).unwrap();
        f.property(2, "value", &ColumnValue::Long(42)).unwrap();
    })
    .unwrap();
    w.write(std::fs::File::create(&path).unwrap()).unwrap();

    let note = "1 column of a type RustGIS has no field for was left out: blob (binary)";
    let src = open_path(&path, None).unwrap();
    assert_eq!(src.info().notes, vec![note.to_string()]);
    let names: Vec<&str> = src.info().fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["name", "value"]);
    let (ds, report) = materialize(src.as_ref()).unwrap();
    assert_eq!(report.messages, vec![format!("blobs: {note}")]);
    assert_eq!(ds.features[0].attrs, vec![Value::Text("a".into()), Value::Int(42)]);

    let clean = d.path().join("clean.fgb");
    write_fgb(&clean);
    let src = open_path(&clean, None).unwrap();
    assert!(src.info().notes.is_empty(), "{:?}", src.info().notes);
}

/// The tables of a GeoPackage, by name.
fn gpkg_tables(path: &Path) -> Vec<String> {
    let c = rusqlite::Connection::open(path).unwrap();
    let mut st = c.prepare("SELECT table_name FROM gpkg_contents ORDER BY table_name").unwrap();
    st.query_map([], |r| r.get(0)).unwrap().collect::<std::result::Result<_, _>>().unwrap()
}

/// Text in a date-time field with digits past the microsecond is written as a read takes it: as
/// that date-time, to the millisecond and counted, not kept as text a read would call no date.
#[test]
fn geopackage_writes_fine_date_time_text_as_a_date_time() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("fine.gpkg");
    let mut ds = Dataset::new("fine", GeometryType::Point, vec![Field::new("seen", FieldType::DateTime)]);
    ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Text("2026-03-05T14:30:00.1234567Z".into())]);
    assert_eq!(crate::io::fine_dates(ds.features.iter(), &ds.fields), [1]);
    assert_eq!(crate::io::text_in_dates(ds.features.iter(), &ds.fields), [0], "a read takes it as a date-time");
    crate::io::write_path(&ds, &path).unwrap();
    let stored: String = rusqlite::Connection::open(&path).unwrap().query_row("SELECT seen FROM fine", [], |r| r.get(0)).unwrap();
    assert_eq!(stored, "2026-03-05T14:30:00.123Z");
}

/// A GeoPackage export writes DATE and DATETIME columns, as GeoPackage gives them: a date-time with
/// a zone in UTC with `Z` (its other offset said), one without a zone without, milliseconds always.
/// The desktop and browser readers read them back as dates; a value that isn't one, as null, said
/// with its field.
#[test]
fn geopackage_dates_are_written_and_read_as_dates() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("permits.gpkg");
    let fields = vec![Field::new("issued", FieldType::Date), Field::new("seen", FieldType::DateTime)];
    let mut permits = Dataset::new("permits", GeometryType::Point, fields);
    let (day, at) = (|d| Value::parse(d, FieldType::Date), |t| Value::parse(t, FieldType::DateTime));
    let rows = [
        [day("2026-03-05"), at("2026-03-05T14:30:00+02:00")],
        [day("1999-12-31"), at("2026-03-05T14:30:00.250")],
        [Value::Null, Value::Null],
        // What another program may leave: text that isn't a date, a number, and empty text (no loss).
        [Value::Text("2026-02-30".into()), Value::Int(1_772_700_000)],
        [Value::Text(String::new()), Value::Null],
        // A midnight without a zone says a day: a date.
        [Value::Text("2026-01-01T00:00:00".into()), Value::Null],
    ];
    for row in &rows {
        permits.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), row.to_vec());
    }
    crate::io::write_path(&permits, &path).unwrap();
    assert_eq!(
        crate::io::utc_dates_note(&permits.fields, &crate::io::offset_dates(permits.features.iter(), &permits.fields), "permits.gpkg")
            .as_deref(),
        Some("permits.gpkg: 1 date-time was written in UTC, as GeoPackage stores them, without their own offsets, in 'seen'")
    );
    // Milliseconds are all a DATETIME holds: finer digits are rounded, and said; none lost, none said.
    assert_eq!(crate::io::fine_dates(permits.features.iter(), &permits.fields), [0, 0]);
    let mut fine = Dataset::new("fine", GeometryType::Point, permits.fields.clone());
    for t in ["2026-03-05T14:30:00.000750+02:00", "2026-03-05T14:30:00.000400", "9999-12-31T23:59:59.999999Z", "2026-03-05T14:30:00.5"] {
        fine.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Null, at(t)]);
    }
    assert_eq!(
        crate::io::fine_dates_note(&fine.fields, &crate::io::fine_dates(fine.features.iter(), &fine.fields), "fine.gpkg").as_deref(),
        Some("fine.gpkg: 3 date-times were rounded to the millisecond, as GeoPackage stores them, in 'seen'")
    );
    let rounded = workspace.path().join("fine.gpkg");
    crate::io::write_path(&fine, &rounded).unwrap();
    let stored: Vec<String> = rusqlite::Connection::open(&rounded)
        .unwrap()
        .prepare("SELECT seen FROM fine ORDER BY fid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(stored, ["2026-03-05T12:30:00.001Z", "2026-03-05T14:30:00.000", "9999-12-31T23:59:59.999Z", "2026-03-05T14:30:00.500"]);
    let c = rusqlite::Connection::open(&path).unwrap();
    let declared: Vec<String> = c
        .prepare("SELECT type FROM pragma_table_info('permits') WHERE name IN ('issued', 'seen') ORDER BY cid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(declared, ["DATE", "DATETIME"]);
    let stored: Vec<(Option<String>, Option<String>)> = c
        .prepare("SELECT issued, seen FROM permits ORDER BY fid LIMIT 3")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    let text = |s: &str| Some(s.to_string());
    assert_eq!(
        stored,
        [(text("2026-03-05"), text("2026-03-05T12:30:00.000Z")), (text("1999-12-31"), text("2026-03-05T14:30:00.250")), (None, None)]
    );
    drop(c);
    let desktop = open_path(&path, None).unwrap();
    let browser = gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(&path).unwrap())), None).unwrap();
    for src in [desktop.as_ref(), &browser as &dyn FeatureSource] {
        assert_eq!(src.info().fields.iter().map(|f| f.ty).collect::<Vec<_>>(), [FieldType::Date, FieldType::DateTime]);
        let (ds, report) = materialize(src).unwrap();
        let read: Vec<_> = ds.features.iter().map(|f| f.attrs.clone()).collect();
        let null = vec![Value::Null; 2];
        // The moment is kept, at UTC.
        let utc = vec![day("2026-03-05"), at("2026-03-05T12:30:00Z")];
        assert_eq!(read, [utc, rows[1].to_vec(), null.clone(), null.clone(), null.clone(), vec![day("2026-01-01"), Value::Null]]);
        assert!(report.messages.says("permits: 2 values aren't dates and were read as empty, in 'issued', 'seen'"), "{report:?}");
    }
    // The export says the text it wrote in a date field that isn't a date, and a date-time with a
    // time in one, which reads back empty too.
    let mut moment = Dataset::new("m", GeometryType::None, permits.fields.clone());
    moment.push(None, vec![at("2026-03-05T14:30:00"), Value::Int(1_772_700_000)]);
    assert_eq!(crate::io::text_in_dates(moment.features.iter(), &moment.fields), [1, 1]);
    let text = crate::io::text_in_dates(permits.features.iter(), &permits.fields);
    assert_eq!(
        crate::io::text_in_dates_note(&permits.fields, &text, "permits.gpkg").as_deref(),
        Some("permits.gpkg: 2 values aren't dates and were written as they are, and read back empty, in 'issued', 'seen'")
    );
    // Dates by a declared type's own name, not a part of another.
    let ty = |d: &str| super::gpkg_blob::field_type(d);
    assert_eq!(
        ["DATE", "datetime", "TIMESTAMP(3)", "UPDATED_INT", "DATEish TEXT", "TEXT"].map(ty),
        [
            Some(FieldType::Date),
            Some(FieldType::DateTime),
            Some(FieldType::DateTime),
            Some(FieldType::Integer),
            Some(FieldType::Text),
            Some(FieldType::Text)
        ]
    );
    // A clean table says nothing.
    let clean = workspace.path().join("clean.gpkg");
    permits.features = permits.features.iter().take(3).cloned().collect::<Vec<_>>().into();
    crate::io::write_path(&permits, &clean).unwrap();
    let (_, report) = materialize(open_path(&clean, None).unwrap().as_ref()).unwrap();
    assert!(!report.messages.iter().any(|m| m.contains("aren't dates") || m.contains("isn't a date")), "{report:?}");
}

/// An export into an existing GeoPackage adds a table beside the others, refuses one whose name
/// is taken, and replaces a feature or attribute table (its spatial index too) when asked; the
/// other tables, an attribute table among them, are as they were.
#[test]
fn geopackage_export_adds_and_replaces_tables_and_keeps_the_others() {
    use crate::io::{GpkgMode, GpkgTable, GpkgWritten, write_path_to};
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("county.gpkg");
    let mut parcels = Dataset::new("parcels", GeometryType::Point, vec![Field::new("apn", FieldType::Text)]);
    parcels.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Text("001".into())]);
    crate::io::write_path(&parcels, &path).unwrap();
    // An attribute table, and the feature counts GDAL keeps.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE owners (name TEXT); INSERT INTO owners VALUES ('Shelby');
             INSERT INTO gpkg_contents (table_name, data_type, identifier) VALUES ('owners', 'attributes', 'owners');
             CREATE TABLE gpkg_ogr_contents (table_name TEXT NOT NULL PRIMARY KEY, feature_count INTEGER DEFAULT NULL);
             INSERT INTO gpkg_ogr_contents VALUES ('parcels', 1);
             CREATE TRIGGER wipe AFTER INSERT ON gpkg_contents BEGIN DELETE FROM owners; END;",
        )
        .unwrap();
    let mut permits = Dataset::new("permits", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    for i in 0..3 {
        permits.push(Some(Geometry::Point(Coord::new(10.0 + i as f64, 20.0))), vec![Value::Int(i)]);
    }
    let to = |mode, name: Option<&str>| GpkgTable { mode, name: name.map(String::from) };
    let written = write_path_to(&permits, &path, Crs::LonLat, &to(GpkgMode::AddTable, None)).unwrap();
    assert_eq!(written, (0, Some(GpkgWritten { table: "permits".into(), replaced: false, dependents: vec![] }), Vec::new()));
    rusqlite::Connection::open(&path).unwrap().execute_batch("INSERT INTO gpkg_ogr_contents VALUES ('permits', 3)").unwrap();
    assert_eq!(gpkg_tables(&path), ["owners", "parcels", "permits"]);
    // The file's own trigger on its contents didn't run.
    let owners =
        |p: &Path| rusqlite::Connection::open(p).unwrap().query_row("SELECT count(*) FROM owners", [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(owners(&path), 1);
    let read = |t: &str, extent| open_path(&path, Some(t)).unwrap().read(&Request { extent, limit: 100 }).unwrap().features;
    assert_eq!(read("permits", None).len(), 3);
    // Its spatial index answers an extent query.
    assert_eq!(read("permits", Some(Extent::new(10.5, 19.0, 11.5, 21.0))).len(), 1);
    assert_eq!(read("parcels", None)[0].attrs, [Value::Text("001".into())]);

    // A name taken (whatever its case) is refused unless replacing, and the file is unchanged.
    let before = std::fs::read(&path).unwrap();
    let mut before_index = before.clone();
    for name in [None, Some("PERMITS"), Some("owners_idx")] {
        if name == Some("owners_idx") {
            assert_eq!(std::fs::read(&path).unwrap(), before, "the refusals changed nothing");
            rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE INDEX owners_idx ON owners (name)").unwrap();
            before_index = std::fs::read(&path).unwrap();
        }
        let e = write_path_to(&permits, &path, Crs::LonLat, &to(GpkgMode::AddTable, name)).unwrap_err().to_string();
        assert!(e.contains("already has"), "{name:?}: {e}");
    }
    assert_eq!(std::fs::read(&path).unwrap(), before_index, "the index refusal changed nothing");
    assert_ne!(before, before_index, "the index was added");

    // Replacing: fewer features, in another system, and its old spatial index gone. A view on the
    // table is named, as it reads the new one.
    rusqlite::Connection::open(&path).unwrap().execute_batch("CREATE VIEW recent_permits AS SELECT * FROM permits").unwrap();
    let mut fewer = permits.clone();
    fewer.features.truncate(1);
    // The replaced table's `last_change` (here later than now, as a pinned time can be) is moved
    // past, so saving edits loaded before can't take the new table for the old one.
    let stamp = |p: &Path| {
        rusqlite::Connection::open(p)
            .unwrap()
            .query_row("SELECT last_change FROM gpkg_contents WHERE table_name = 'permits'", [], |r| r.get::<_, String>(0))
            .unwrap()
    };
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("UPDATE gpkg_contents SET last_change = '2999-01-01T00:00:00.000Z' WHERE table_name = 'permits'", [])
        .unwrap();
    let replaced = write_path_to(&fewer, &path, Crs::WebMercator, &to(GpkgMode::ReplaceTable, None)).unwrap();
    assert_eq!(replaced.1, Some(GpkgWritten { table: "permits".into(), replaced: true, dependents: vec!["recent_permits".into()] }));
    assert_eq!(stamp(&path), "2999-01-01T00:00:00.001Z");
    assert_eq!(gpkg_tables(&path), ["owners", "parcels", "permits"]);
    let s = open_path(&path, Some("permits")).unwrap();
    assert_eq!(s.info().count, Some(1));
    assert_eq!(s.info().coordinate_system(), Some(Crs::WebMercator));
    let c = rusqlite::Connection::open(&path).unwrap();
    let count = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(count("SELECT count(*) FROM rtree_permits_geom"), 1);
    assert_eq!(count("SELECT count(*) FROM gpkg_geometry_columns WHERE table_name = 'permits'"), 1);
    assert_eq!(count("SELECT count(*) FROM gpkg_extensions WHERE table_name = 'permits'"), 1);
    assert_eq!(count("SELECT count(*) FROM gpkg_spatial_ref_sys WHERE srs_id = 3857"), 1);
    // The old table's cached count is gone with it; the others' are kept.
    assert_eq!(count("SELECT count(*) FROM gpkg_ogr_contents WHERE table_name = 'permits'"), 0);
    assert_eq!(count("SELECT count(*) FROM gpkg_ogr_contents WHERE table_name = 'parcels'"), 1);
    assert_eq!(c.query_row("SELECT name FROM owners", [], |r| r.get::<_, String>(0)).unwrap(), "Shelby");
    drop(c);
    // A second table in that system reuses its entry.
    let more = write_path_to(&fewer, &path, Crs::WebMercator, &to(GpkgMode::AddTable, Some("permits_2027"))).unwrap();
    assert_eq!(more.1, Some(GpkgWritten { table: "permits_2027".into(), replaced: false, dependents: vec![] }));
    // Its spatial index triggers mention `permits` within their own table's name, but don't use
    // it, so replacing `permits` again names only the view.
    let again = write_path_to(&fewer, &path, Crs::WebMercator, &to(GpkgMode::ReplaceTable, Some("permits"))).unwrap();
    assert_eq!(again.1, Some(GpkgWritten { table: "permits".into(), replaced: true, dependents: vec!["recent_permits".into()] }));
    // Replacing a table not there (a misspelt name) is refused rather than adding one.
    let e = write_path_to(&fewer, &path, Crs::LonLat, &to(GpkgMode::ReplaceTable, Some("permit"))).unwrap_err().to_string();
    assert!(e.contains("no table permit to replace"), "{e}");
    let c = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(c.query_row("SELECT count(*) FROM gpkg_spatial_ref_sys WHERE srs_id = 3857", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    // A table without geometry replaces an attribute table, adding no spatial reference system.
    let systems = || {
        let c = rusqlite::Connection::open(&path).unwrap();
        c.query_row("SELECT count(*) FROM gpkg_spatial_ref_sys", [], |r| r.get::<_, i64>(0)).unwrap()
    };
    let before_systems = systems();
    let mut owners = Dataset::new("owners", GeometryType::None, vec![Field::new("name", FieldType::Text)]);
    owners.push(None, vec![Value::Text("Memphis".into())]);
    owners.push(None, vec![Value::Text("Germantown".into())]);
    drop(c);
    write_path_to(&owners, &path, Crs::from_epsg(32616).unwrap(), &to(GpkgMode::ReplaceTable, None)).unwrap();
    assert_eq!(systems(), before_systems);
    let c = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(c.query_row("SELECT count(*) FROM owners", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    drop(c);
    assert_eq!(read("parcels", None).len(), 1, "untouched throughout");
}

/// An export into an existing GeoPackage never replaces what isn't a feature or attribute table
/// (a tile table, a view, an unregistered table) or a file that isn't there, and refuses a file
/// that isn't a GeoPackage; adding a table where there is no file makes a new one.
#[test]
fn geopackage_export_into_a_file_refuses_what_it_cannot_replace() {
    use crate::io::{GpkgMode, GpkgTable, write_path_to};
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("county.gpkg");
    let replace = |name: &str| GpkgTable { mode: GpkgMode::ReplaceTable, name: Some(name.into()) };
    let mut ds = Dataset::new("permits", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![]);
    // No file: nothing to replace in, but a table can be added to a new GeoPackage.
    assert!(write_path_to(&ds, &path, Crs::LonLat, &replace("permits")).unwrap_err().to_string().contains("no GeoPackage there"));
    assert!(!path.exists());
    let add = GpkgTable { mode: GpkgMode::AddTable, name: None };
    assert!(write_path_to(&ds, &path, Crs::LonLat, &add).unwrap().1.is_some_and(|t| !t.replaced));
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE ortho (id INTEGER PRIMARY KEY, tile_data BLOB);
             INSERT INTO gpkg_contents (table_name, data_type, identifier) VALUES ('ortho', 'tiles', 'ortho');
             CREATE VIEW recent AS SELECT * FROM permits; CREATE TABLE scratch (x);",
        )
        .unwrap();
    let before = std::fs::read(&path).unwrap();
    for name in ["ortho", "recent", "scratch"] {
        let e = write_path_to(&ds, &path, Crs::LonLat, &replace(name)).unwrap_err().to_string();
        assert!(e.contains("doesn't replace"), "{name}: {e}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "{name}");
    }
    // A related-tables relation's table isn't replaced; another application's database isn't
    // written to.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE gpkgext_relations (id INTEGER PRIMARY KEY, base_table_name TEXT, base_primary_column TEXT,
                related_table_name TEXT, related_primary_column TEXT, relation_name TEXT, mapping_table_name TEXT);
             INSERT INTO gpkgext_relations VALUES (1, 'permits', 'fid', 'owners', 'id', 'attributes', 'permits_owners');",
        )
        .unwrap();
    let e = write_path_to(&ds, &path, Crs::LonLat, &replace("permits")).unwrap_err().to_string();
    assert!(e.contains("related-tables relation"), "{e}");
    // Nor is a table others refer to by foreign key: their rows would be deleted with it.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DELETE FROM gpkgext_relations;
             CREATE TABLE inspections (id INTEGER PRIMARY KEY, permit INTEGER REFERENCES permits (fid) ON DELETE CASCADE);
             INSERT INTO inspections VALUES (1, 1);",
        )
        .unwrap();
    let e = write_path_to(&ds, &path, Crs::LonLat, &replace("permits")).unwrap_err().to_string();
    assert!(e.contains("inspections refer to permits"), "{e}");
    let kept: i64 = rusqlite::Connection::open(&path).unwrap().query_row("SELECT count(*) FROM inspections", [], |r| r.get(0)).unwrap();
    assert_eq!(kept, 1);
    // A table referring to the GeoPackage's own records keeps its rows when a replace deletes them.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DROP TABLE inspections;
             CREATE TABLE notes (layer TEXT REFERENCES gpkg_contents (table_name) ON DELETE CASCADE);
             INSERT INTO notes VALUES ('permits');",
        )
        .unwrap();
    write_path_to(&ds, &path, Crs::LonLat, &replace("permits")).unwrap();
    let notes: i64 = rusqlite::Connection::open(&path).unwrap().query_row("SELECT count(*) FROM notes", [], |r| r.get(0)).unwrap();
    assert_eq!(notes, 1);
    // An ordinary table named as the spatial index is kept, and the replace refused.
    let squatted = workspace.path().join("squatted.gpkg");
    write_path_to(&ds, &squatted, Crs::LonLat, &GpkgTable { mode: GpkgMode::AddTable, name: Some("sites".into()) }).unwrap();
    rusqlite::Connection::open(&squatted)
        .unwrap()
        .execute_batch(
            "DROP TABLE rtree_sites_geom; CREATE TABLE rtree_sites_geom (note TEXT); INSERT INTO rtree_sites_geom VALUES ('keep');",
        )
        .unwrap();
    let before = std::fs::read(&squatted).unwrap();
    let e = write_path_to(&ds, &squatted, Crs::LonLat, &replace("sites")).unwrap_err().to_string();
    assert!(e.contains("rtree_sites_geom"), "{e}");
    assert_eq!(std::fs::read(&squatted).unwrap(), before, "the failed replace changed nothing");
    let kept: String =
        rusqlite::Connection::open(&squatted).unwrap().query_row("SELECT note FROM rtree_sites_geom", [], |r| r.get(0)).unwrap();
    assert_eq!(kept, "keep");
    // A spatial index declared over lines, with the module quoted, is still the index; an
    // `rtree_i32` table of that name isn't.
    for (module, replaced) in [("\n  \"rtree\" (", true), ("[rtree](", true), ("rtree_i32(", false)] {
        let file = workspace.path().join(format!("index-{}.gpkg", module.len()));
        write_path_to(&ds, &file, Crs::LonLat, &GpkgTable { mode: GpkgMode::AddTable, name: Some("sites".into()) }).unwrap();
        rusqlite::Connection::open(&file)
            .unwrap()
            .execute_batch(&format!(
                "DROP TABLE rtree_sites_geom; CREATE VIRTUAL TABLE rtree_sites_geom USING {module}id, minx, maxx, miny, maxy);"
            ))
            .unwrap();
        assert_eq!(write_path_to(&ds, &file, Crs::LonLat, &replace("sites")).is_ok(), replaced, "{module:?}");
    }
    let other = workspace.path().join("other.gpkg");
    std::fs::copy(&path, &other).unwrap();
    rusqlite::Connection::open(&other).unwrap().execute_batch("PRAGMA application_id = 1297105496").unwrap();
    let add = GpkgTable { mode: GpkgMode::AddTable, name: Some("more".into()) };
    assert!(write_path_to(&ds, &other, Crs::LonLat, &add).unwrap_err().to_string().contains("application id"));
    let plain = workspace.path().join("plain.gpkg");
    rusqlite::Connection::open(&plain).unwrap().execute_batch("CREATE TABLE t (x)").unwrap();
    let e = write_path_to(&ds, &plain, Crs::LonLat, &replace("permits")).unwrap_err().to_string();
    assert!(e.contains("not a GeoPackage"), "{e}");
    let text = workspace.path().join("text.gpkg");
    std::fs::write(&text, b"not a database").unwrap();
    assert!(write_path_to(&ds, &text, Crs::LonLat, &replace("permits")).is_err());
    assert_eq!(std::fs::read(&text).unwrap(), b"not a database");
}

#[test]
fn geojson_latitude_past_the_poles_is_said_once_with_a_count_by_every_path() {
    let point = |x: f64, y: f64| format!(r#"{{"type":"Feature","properties":{{}},"geometry":{{"type":"Point","coordinates":[{x},{y}]}}}}"#);
    let line = r#"{"type":"Feature","properties":{},"geometry":{"type":"LineString","coordinates":[[1,2],[3,-95]]}}"#;
    let fc = |features: &[String]| format!(r#"{{"type":"FeatureCollection","features":[{}]}}"#, features.join(","));
    // One feature with a swapped position, then two (a line counts once, not by position).
    let (ds, messages, _) = geojson_paths("lat95.geojson", &fc(&[point(10.0, 60.0), point(60.0, 95.0)]));
    assert_eq!(ds.features.len(), 2, "the data stays as read");
    assert_eq!(messages, ["1 feature has a latitude past ±90°: longitude and latitude may be swapped"]);
    let (_, messages, _) = geojson_paths("lat95b.geojson", &fc(&[point(60.0, 95.0), point(1.0, -91.0), point(2.0, 3.0), line.to_string()]));
    assert_eq!(messages, ["3 features have a latitude past ±90°: longitude and latitude may be swapped"]);
    // A swapped position is said even when another position of the feature is far east.
    let multi = r#"{"type":"Feature","properties":{},"geometry":{"type":"MultiPoint","coordinates":[[60,95],[200,10]]}}"#;
    let (_, messages, _) = geojson_paths("lat95c.geojson", &fc(&[multi.to_string()]));
    assert_eq!(messages, ["1 feature has a latitude past ±90°: longitude and latitude may be swapped"]);
    // Clean files, even at the poles and the antimeridian, give nothing; a lone Feature as well.
    let (_, messages, _) = geojson_paths("clean.geojson", &fc(&[point(10.0, 60.0), point(180.0, -90.0), point(-180.0, 90.0)]));
    assert!(messages.is_empty(), "{messages:?}");
    // Within the readers' ±90.5° tolerance is still not past the poles.
    let (_, messages, _) =
        geojson_paths("tolerance.geojson", &fc(&[point(1.0, 90.2), point(1.0, -90.2), point(180.5, 90.5), point(-180.5, -90.5)]));
    assert!(messages.is_empty(), "{messages:?}");
    let (_, report) = crate::io::read_geojson(&point(60.0, 95.0), "x").unwrap();
    assert_eq!(report.messages.len(), 1, "{:?}", report.messages);
    let (_, report) = crate::io::read_geojson(&point(10.0, 60.0), "x").unwrap();
    assert!(report.messages.is_empty());
    // Eastings and northings (x past 180 as well) are not longitude/latitude swapped.
    let (_, messages, _) = geojson_paths("rd-not-swapped.geojson", &fc(&[point(188000.0, 445000.0)]));
    assert!(messages.is_empty(), "{messages:?}");
}

#[test]
fn geojson_in_a_declared_projected_system_is_not_taken_for_swapped() {
    // Northings far past 90 are what RD New coordinates are: they convert, so nothing is said.
    let (ds, messages, _) = geojson_paths("rd-quiet.geojson", &rd_geojson(Some(&named("EPSG:28992"))));
    assert!(in_arnhem(&ds.extent()));
    assert!(messages.is_empty(), "{messages:?}");
    // Web Mercator metres that would look like a latitude past the poles if read as degrees.
    let point = r#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[10,100]}}"#;
    let (ds, messages, _) = geojson_paths("merc-quiet.geojson", &merc_collection(&[point]));
    assert_eq!(ds.features.len(), 1);
    assert!(messages.is_empty(), "{messages:?}");
}

/// Native writer schemas and independent Arrow decoding distinguish UTC instants from local
/// clocks. Mixed columns fall back to text; invalid native values are counted as null.
#[test]
fn native_file_dates_preserve_zone_policy_and_fractional_seconds() {
    use ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use arrow_schema::{DataType, TimeUnit};
    let fields = vec![
        Field::new("day", FieldType::Date),
        Field::new("local", FieldType::DateTime),
        Field::new("instant", FieldType::DateTime),
        Field::new("mixed", FieldType::DateTime),
    ];
    let mut ds = Dataset::new("dates", GeometryType::Point, fields);
    for (i, zone) in ["", "Z"].iter().enumerate() {
        ds.push(
            Some(Geometry::Point(Coord::new(i as f64, 0.0))),
            vec![
                Value::parse("2024-02-29", FieldType::Date),
                Value::parse("1969-12-31T23:59:59.123456", FieldType::DateTime),
                Value::parse("2026-03-05T14:30:00.123456+02:00", FieldType::DateTime),
                Value::parse(&format!("2026-01-01T00:00:00{zone}"), FieldType::DateTime),
            ],
        );
    }
    let bytes = {
        use std::sync::atomic::Ordering;
        let progress = Arc::new(crate::progress::tests::Counted::default());
        let _guard = crate::progress::install(Some(progress.clone()));
        crate::progress::begin(ds.features.len());
        let bytes = parquet::write(&ds).unwrap();
        assert_eq!(progress.done.load(Ordering::Relaxed), ds.features.len() as u64, "the layout pass must not count written rows");
        bytes
    };
    let arrow = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(bytes.clone())).unwrap();
    assert_eq!(arrow.schema().field_with_name("day").unwrap().data_type(), &DataType::Date32);
    assert_eq!(arrow.schema().field_with_name("local").unwrap().data_type(), &DataType::Timestamp(TimeUnit::Microsecond, None));
    assert_eq!(
        arrow.schema().field_with_name("instant").unwrap().data_type(),
        &DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
    );
    assert_eq!(arrow.schema().field_with_name("mixed").unwrap().data_type(), &DataType::Utf8);
    let (back, report) = crate::io::read_bytes("dates.parquet", &bytes).unwrap();
    assert!(report.messages.is_empty(), "{report:?}");
    assert_eq!(back.features[0].attrs[0], ds.features[0].attrs[0]);
    assert_eq!(back.features[0].attrs[1], ds.features[0].attrs[1]);
    assert_eq!(back.features[0].attrs[2].to_string(), "2026-03-05T12:30:00.123456Z");
    assert_eq!(back.features[1].attrs[3], Value::Text("2026-01-01T00:00:00Z".into()));
    let note = parquet::values_note(&ds, "out.parquet").unwrap();
    assert!(note.contains("2 date-times were written in UTC") && note.contains("2 date-times were written as ISO text"), "{note}");
    // A replayable source feed chooses the same schema as an in-memory export.
    let input = fgb::write(&ds).unwrap();
    let source = open_bytes("dates.fgb", input, None).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("stream.parquet");
    let summary = crate::export::write_source_in(source.as_ref(), &ds, &path, Crs::LonLat).unwrap();
    assert_eq!(summary.written, 2);
    // Without a label of their own: the export adds its path once.
    assert_eq!(
        summary.date_messages,
        [
            "2 date-times were written in UTC without their original offsets, in 'instant'",
            "2 date-times were written as ISO text because their column mixes zoned and unzoned values, in 'mixed'"
        ]
    );
    let (streamed, _) = crate::io::read_path(&path).unwrap();
    assert_eq!(streamed.fields, back.fields);
    ds.features[0].attrs[0] = Value::Text("2026-02-30".into());
    assert!(fgb::write(&ds).unwrap_err().to_string().contains("invalid date"));
    let note = parquet::values_note(&ds, "bad.parquet").unwrap();
    assert!(note.contains("1 value couldn't be written"));
    let (bad, _) = crate::io::read_bytes("bad.parquet", &parquet::write(&ds).unwrap()).unwrap();
    assert!(bad.features[0].attrs[0].is_null());
    // Date fields accept the same unzoned midnight text as stored-value conversion;
    // non-midnight times and zoned midnight still cannot be narrowed to a calendar date.
    for (text, valid) in [("2026-03-05T00:00:00", true), ("2026-03-05T00:00:01", false), ("2026-03-05T00:00:00Z", false)] {
        ds.features[0].attrs[0] = Value::Text(text.into());
        let (read, _) = crate::io::read_bytes("midnight.parquet", &parquet::write(&ds).unwrap()).unwrap();
        assert_eq!(read.features[0].attrs[0].is_null(), !valid, "{text}");
        if valid {
            assert_eq!(read.features[0].attrs[0], Value::parse("2026-03-05", FieldType::Date));
        }
    }
}

/// FlatGeobuf has a shared DateTime wire type. Local classification sees the whole column;
/// a remote source preserves text with a notice rather than scanning the file at open.
#[test]
fn flatgeobuf_classifies_local_dates_and_preserves_remote_text() {
    use flatgeobuf::geozero::{ColumnValue, PropertyProcessor, wkb::Wkb};
    use flatgeobuf::{ColumnType, FgbWriter, FgbWriterOptions, GeometryType as G};
    let mut writer = FgbWriter::create_with_options("dates", G::Point, FgbWriterOptions::default()).unwrap();
    for name in ["day", "moment", "clock"] {
        writer.add_column(name, ColumnType::DateTime, |_, c| c.nullable = true);
    }
    for (i, values) in
        [["2024-02-29", "2026-03-05T14:30:00.123456+02:00", "12:30:00.123456789"], ["2026-02-30", "99:99:99", "13:00:00-05:00"]]
            .iter()
            .enumerate()
    {
        writer
            .add_feature_geom(Wkb(wkb::encode(&Geometry::Point(Coord::new(i as f64, 0.0)))), |out| {
                for (k, s) in values.iter().enumerate() {
                    out.property(k, ["day", "moment", "clock"][k], &ColumnValue::DateTime(s)).unwrap();
                }
            })
            .unwrap();
    }
    let mut bytes = Vec::new();
    writer.write(&mut bytes).unwrap();
    let local = open_bytes("dates.fgb", bytes.clone(), None).unwrap();
    assert_eq!(local.info().fields.iter().map(|f| f.ty).collect::<Vec<_>>(), [FieldType::Date, FieldType::DateTime, FieldType::Text]);
    let batch = local.read(&Request::all()).unwrap();
    // A malformed clock must not force the moment column to Text; real clock fractions stay intact.
    assert_eq!(batch.unread_dates, [1, 1]);
    assert!(batch.features.iter().any(|f| f.attrs[2].to_string() == "12:30:00.123456789"));
    let (_, report) = materialize(local.as_ref()).unwrap();
    assert!(report.unread_dates.unwrap().contains("2 values"));
    let mut rows = Vec::new();
    let streamed = local
        .visit_chunks(None, 256, &mut |chunk| {
            rows.extend(chunk);
            Ok(())
        })
        .unwrap();
    assert_eq!(streamed.unread_dates, batch.unread_dates);
    assert_eq!(rows, batch.features);
    let url = "https://test.example/native-date-fields.fgb";
    remote::testing::serve(url, bytes);
    let remote = open_driven(url);
    assert!(remote.info().fields.iter().all(|f| f.ty == FieldType::Text));
    assert!(remote.info().notes.iter().any(|n| n.contains("3 date/time columns are kept as ISO text")));
    let remote_batch = remote.read(&Request::all()).unwrap();
    assert!(remote_batch.features.iter().any(|f| f.attrs[0] == Value::Text("2026-02-30".into())));
}

/// A FlatGeobuf column with a date and a date-time with digits past the microsecond is a
/// date-time column: classification reads the date-time as the read does, rounded and counted.
#[test]
fn flatgeobuf_classifies_fine_date_times_as_date_times() {
    use flatgeobuf::geozero::{ColumnValue, PropertyProcessor, wkb::Wkb};
    use flatgeobuf::{ColumnType, FgbWriter, FgbWriterOptions, GeometryType as G};
    let mut writer = FgbWriter::create_with_options("fine", G::Point, FgbWriterOptions::default()).unwrap();
    writer.add_column("at", ColumnType::DateTime, |_, c| c.nullable = true);
    for (i, s) in ["2026-03-05", "2026-03-05T14:30:00.123456789Z"].iter().enumerate() {
        writer
            .add_feature_geom(Wkb(wkb::encode(&Geometry::Point(Coord::new(i as f64, 0.0)))), |out| {
                out.property(0, "at", &ColumnValue::DateTime(s)).unwrap();
            })
            .unwrap();
    }
    let mut bytes = Vec::new();
    writer.write(&mut bytes).unwrap();
    let src = open_bytes("fine.fgb", bytes, None).unwrap();
    assert_eq!(src.info().fields[0].ty, FieldType::DateTime);
    let batch = src.read(&Request::all()).unwrap();
    // FlatGeobuf keeps features in its spatial index's order.
    let mut shown: Vec<String> = batch.features.iter().map(|f| f.attrs[0].to_string()).collect();
    shown.sort();
    assert_eq!(shown, ["2026-03-05T00:00:00", "2026-03-05T14:30:00.123457Z"]);
    assert!(batch.unread_dates.is_empty());
    assert_eq!(batch.rounded_dates, [1]);
}

/// Date-time text a read takes (digits past the microsecond) is exported to FlatGeobuf as it is,
/// not refused as an invalid date; reading it back rounds it and says so.
#[test]
fn flatgeobuf_exports_fine_date_time_text_as_it_is() {
    let mut ds = Dataset::new("fine", GeometryType::Point, vec![Field::new("at", FieldType::DateTime)]);
    ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Text("2026-03-05T14:30:00.1234567Z".into())]);
    let bytes = fgb::write(&ds).unwrap();
    let batch = open_bytes("fine.fgb", bytes, None).unwrap().read(&Request::all()).unwrap();
    assert_eq!(batch.features[0].attrs[0].to_string(), "2026-03-05T14:30:00.123457Z");
    assert_eq!(batch.rounded_dates, [1]);
}

/// A date field's text that is a midnight is exported to FlatGeobuf as its day, so the column
/// still reads back as dates.
#[test]
fn flatgeobuf_exports_a_midnight_in_a_date_field_as_its_day() {
    let mut ds = Dataset::new("days", GeometryType::Point, vec![Field::new("day", FieldType::Date)]);
    ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::Text("2026-03-05T00:00:00".into())]);
    ds.push(Some(Geometry::Point(Coord::new(2.0, 2.0))), vec![Value::parse("2026-03-06", FieldType::Date)]);
    // A date-time value that is a midnight too.
    ds.push(Some(Geometry::Point(Coord::new(3.0, 2.0))), vec![Value::parse("2026-03-07T00:00:00", FieldType::DateTime)]);
    let src = open_bytes("days.fgb", fgb::write(&ds).unwrap(), None).unwrap();
    assert_eq!(src.info().fields[0].ty, FieldType::Date);
    let mut days: Vec<String> = src.read(&Request::all()).unwrap().features.iter().map(|f| f.attrs[0].to_string()).collect();
    days.sort();
    assert_eq!(days, ["2026-03-05", "2026-03-06", "2026-03-07"]);
}

/// A moment in a GeoPackage date field isn't rounded into a midnight: it reads back as null, as
/// the export counted it.
#[test]
fn geopackage_keeps_a_moment_in_a_date_field_from_becoming_a_day() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("days.gpkg");
    let mut ds = Dataset::new("days", GeometryType::Point, vec![Field::new("day", FieldType::Date)]);
    ds.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Value::parse("2026-03-05T00:00:00.000400", FieldType::DateTime)]);
    assert_eq!(crate::io::text_in_dates(ds.features.iter(), &ds.fields), [1]);
    // Said once, as a value that reads as null: it isn't rounded to the millisecond.
    assert_eq!(crate::io::fine_dates(ds.features.iter(), &ds.fields), [0]);
    // In a text field, the same date-time is written to the millisecond, and counted.
    let mut text = Dataset::new("days", GeometryType::Point, vec![Field::new("day", FieldType::Text)]);
    text.push(Some(Geometry::Point(Coord::new(1.0, 2.0))), ds.features[0].attrs.clone());
    assert_eq!(crate::io::fine_dates(text.features.iter(), &text.fields), [1]);
    crate::io::write_path(&ds, &path).unwrap();
    let (back, _) = crate::io::read_path(&path).unwrap();
    assert!(back.value(0, 0).is_null(), "{:?}", back.value(0, 0));
}

/// A property length that runs past the feature's properties (a hostile or damaged file) skips
/// that feature as unreadable, whole and streamed, instead of panicking in the reader; the
/// features around it are read. Found by fuzzing `read_layers`.
#[test]
fn flatgeobuf_property_lengths_past_the_end_skip_the_feature() {
    let mut ds = Dataset::new("places", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    for (i, name) in ["first", "BROKEN", "third"].into_iter().enumerate() {
        ds.push(Some(Geometry::Point(Coord::new(i as f64, 0.0))), vec![Value::Text(name.into())]);
    }
    let mut bytes = fgb::write(&ds).unwrap();
    // The u32 length before "BROKEN" (6) made far longer than the properties.
    let at = bytes.windows(10).position(|w| w == [6, 0, 0, 0, b'B', b'R', b'O', b'K', b'E', b'N']).unwrap();
    bytes[at..at + 4].copy_from_slice(&0x7fff_0000u32.to_le_bytes());
    let src = open_bytes("places.fgb", bytes, None).unwrap();
    let batch = src.read(&Request::all()).unwrap();
    assert_eq!(batch.skipped, 1);
    let mut names: Vec<String> = batch.features.iter().map(|f| f.attrs[0].to_string()).collect();
    names.sort();
    assert_eq!(names, ["first", "third"]);
    let mut rows = 0;
    let streamed = src
        .visit_chunks(None, 256, &mut |chunk| {
            rows += chunk.len();
            Ok(())
        })
        .unwrap();
    assert_eq!((rows, streamed.skipped), (2, 1));
}

/// A readable size prefix can contain invalid UTF-8 properties. Classification ignores
/// that row just as whole and streamed reads do, and the read reports the skipped feature.
#[test]
fn flatgeobuf_dates_skip_malformed_properties() {
    let mut ds = Dataset::new("dates", GeometryType::Point, vec![Field::new("day", FieldType::Date)]);
    for day in ["2001-01-01", "2024-02-29"] {
        ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![Value::parse(day, FieldType::Date)]);
    }
    let mut bytes = fgb::write(&ds).unwrap();
    let at = bytes.windows(10).position(|s| s == b"2001-01-01").unwrap();
    bytes[at] = 0xff;
    let source = open_bytes("dates.fgb", bytes, None).unwrap();
    assert_eq!(source.info().fields[0].ty, FieldType::Date);
    let read = source.read(&Request::all()).unwrap();
    assert_eq!(read.skipped, 1);
    assert_eq!(read.features.len(), 1);
    assert_eq!(read.features[0].attrs[0].to_string(), "2024-02-29");
    let streamed = source.visit_chunks(None, 256, &mut |_| Ok(())).unwrap();
    assert_eq!(streamed.skipped, 1);
}

/// Independently authored Arrow columns exercise units, UTC flags, pre-epoch values, nulls,
/// out-of-range dates and nanosecond values in both whole and streamed reads.
#[test]
fn parquet_date_units_invalid_values_and_nulls() {
    use arrow_array::{
        ArrayRef, BinaryArray, Date32Array, RecordBatch, TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    };
    use arrow_schema::{DataType, Field as AField, Schema, TimeUnit};
    let geoms: Vec<Vec<u8>> = (0..3).map(|x| wkb::encode(&Geometry::Point(Coord::new(f64::from(x), 0.0)))).collect();
    let schema = Arc::new(Schema::new(vec![
        AField::new("geometry", DataType::Binary, false),
        AField::new("day", DataType::Date32, true),
        AField::new("ms", DataType::Timestamp(TimeUnit::Millisecond, Some("UTC".into())), true),
        AField::new("us", DataType::Timestamp(TimeUnit::Microsecond, None), true),
        AField::new("ns", DataType::Timestamp(TimeUnit::Nanosecond, None), true),
    ]));
    let cols: Vec<ArrayRef> = vec![
        Arc::new(BinaryArray::from(geoms.iter().map(Vec::as_slice).collect::<Vec<_>>())),
        Arc::new(Date32Array::from(vec![Some(-1), Some(i32::MAX), None])),
        Arc::new(TimestampMillisecondArray::from(vec![Some(-1), Some(i64::MAX), None]).with_timezone("UTC")),
        Arc::new(TimestampMicrosecondArray::from(vec![Some(-1), Some(i64::MAX), None])),
        Arc::new(TimestampNanosecondArray::from(vec![Some(-1000), Some(1001), None])),
    ];
    let mut bytes = Vec::new();
    let mut w = ::parquet::arrow::ArrowWriter::try_new(&mut bytes, schema.clone(), None).unwrap();
    w.write(&RecordBatch::try_new(schema, cols).unwrap()).unwrap();
    w.close().unwrap();
    let source = open_bytes("units.parquet", bytes, None).unwrap();
    let batch = source.read(&Request::all()).unwrap();
    // Three out-of-range values are read as empty; 1001 ns is rounded to a microsecond, and counted.
    assert_eq!(batch.unread_dates, [1, 1, 1]);
    assert_eq!(batch.rounded_dates, [0, 0, 0, 1]);
    assert_eq!(
        batch.features[0].attrs.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["1969-12-31", "1969-12-31T23:59:59.999Z", "1969-12-31T23:59:59.999999", "1969-12-31T23:59:59.999999"]
    );
    assert!(batch.features[1].attrs[..3].iter().all(Value::is_null));
    assert_eq!(batch.features[1].attrs[3].to_string(), "1970-01-01T00:00:00.000001");
    assert!(batch.features[2].attrs.iter().all(Value::is_null));
    let extent = Extent::new(-0.1, -0.1, 0.1, 0.1);
    let filtered = source.read(&Request { extent: Some(extent), limit: usize::MAX }).unwrap();
    assert_eq!(filtered.features.len(), 1);
    assert!(filtered.unread_dates.is_empty(), "invalid dates in filtered-out rows aren't read losses");
    let filtered = source.visit_chunks(Some(extent), 256, &mut |_| Ok(())).unwrap();
    assert!(filtered.unread_dates.is_empty());
    let mut rows = Vec::new();
    let streamed = source
        .visit_chunks(None, 256, &mut |chunk| {
            rows.extend(chunk);
            Ok(())
        })
        .unwrap();
    assert_eq!(rows, batch.features);
    assert_eq!(streamed.unread_dates, batch.unread_dates);
}

/// INT96 stores Julian days and nanoseconds of day with no UTC flag. Negative epochs must use
/// the day plus its positive time, and finer-than-microsecond input is rounded and counted.
#[test]
fn parquet_int96_is_unzoned_and_checks_submicroseconds() {
    use ::parquet::basic::{LogicalType, Repetition, TimeUnit, Type as Physical};
    use ::parquet::data_type::{ByteArray, ByteArrayType, Int64Type, Int96, Int96Type};
    use ::parquet::file::{properties::WriterProperties, writer::SerializedFileWriter};
    use ::parquet::schema::types::Type;
    let base = ::parquet::schema::parser::parse_message_type("message m { REQUIRED BINARY geometry; OPTIONAL INT96 legacy; }").unwrap();
    let mut fields = base.get_fields().to_vec();
    for (name, utc) in [("clock_local", false), ("clock_utc", true)] {
        fields.push(Arc::new(
            Type::primitive_type_builder(name, Physical::INT64)
                .with_repetition(Repetition::OPTIONAL)
                .with_logical_type(Some(LogicalType::time(utc, TimeUnit::MICROS)))
                .build()
                .unwrap(),
        ));
    }
    let schema = Arc::new(Type::group_type_builder("m").with_fields(fields).build().unwrap());
    let mut bytes = Vec::new();
    let mut w = SerializedFileWriter::new(&mut bytes, schema, Arc::new(WriterProperties::builder().build())).unwrap();
    let mut rg = w.next_row_group().unwrap();
    let geom = ByteArray::from(wkb::encode(&Geometry::Point(Coord::new(0.0, 0.0))));
    let mut col = rg.next_column().unwrap().unwrap();
    col.typed::<ByteArrayType>().write_batch(&[geom.clone(), geom], None, None).unwrap();
    col.close().unwrap();
    let make = |ns: u64, day| {
        let mut t = Int96::new();
        t.set_data(ns as u32, (ns >> 32) as u32, day);
        t
    };
    let mut col = rg.next_column().unwrap().unwrap();
    col.typed::<Int96Type>().write_batch(&[make(86_399_999_999_000, 2_440_587), make(1, 2_440_588)], Some(&[1, 1]), None).unwrap();
    col.close().unwrap();
    for _ in 0..2 {
        let mut col = rg.next_column().unwrap().unwrap();
        col.typed::<Int64Type>().write_batch(&[0, 1], Some(&[1, 1]), None).unwrap();
        col.close().unwrap();
    }
    rg.close().unwrap();
    w.close().unwrap();
    let src = open_bytes("legacy.parquet", bytes, None).unwrap();
    let batch = src.read(&Request::all()).unwrap();
    assert_eq!(batch.features[0].attrs[0].to_string(), "1969-12-31T23:59:59.999999");
    // One nanosecond is rounded to the microsecond, and counted.
    assert_eq!(batch.features[1].attrs[0].to_string(), "1970-01-01T00:00:00");
    assert!(batch.unread_dates.is_empty());
    assert_eq!(batch.rounded_dates, [1]);
    assert_eq!(batch.features[0].attrs[1].to_string(), "00:00:00");
    assert_eq!(batch.features[1].attrs[1].to_string(), "00:00:00.000001");
    assert_eq!(batch.features[0].attrs[2].to_string(), "00:00:00.000000Z");
    assert_eq!(batch.features[1].attrs[2].to_string(), "00:00:00.000001Z");
    assert!(batch.features[0].attrs[2] < batch.features[1].attrs[2]);
}

/// A feature just below an extent, within the rounding of the Shapefile and GeoJSON indexes'
/// single-precision boxes, is left out of a read of that extent, as it is in a whole read's
/// filter and the other formats.
#[test]
fn a_feature_just_outside_an_extent_is_left_out_of_its_read() {
    let d = dir().join("just-outside");
    std::fs::create_dir_all(&d).unwrap();
    let mut ds = Dataset::new("edge", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(0.0, 0.999_999_999))), vec![]);
    let extent = Some(Extent::new(-1.0, 1.0, 1.0, 2.0));
    let req = Request { extent, limit: 1000 };
    let (shp_path, geojson_path) = (d.join("edge.shp"), d.join("edge.geojson"));
    crate::io::write_path(&ds, &shp_path).unwrap();
    crate::io::write_path(&ds, &geojson_path).unwrap();
    let shp = shp::ShpSource::open_path(&shp_path, "edge").unwrap();
    assert!(shp.read(&req).unwrap().features.is_empty());
    let mut rows = Vec::new();
    shp.visit_chunks(extent, 1 << 20, &mut |chunk| {
        rows.extend(chunk);
        Ok(())
    })
    .unwrap();
    assert!(rows.is_empty());
    let geojson = geojson::GeoJsonSource::open_path(&geojson_path, "edge").unwrap();
    assert!(geojson.read(&req).unwrap().features.is_empty());
    std::fs::remove_dir_all(&d).unwrap();
}

/// A view the layer's projection refuses part of the edge of (#1925): the box queried in the
/// layer's coordinates must still hold every place in the view the projection takes, or a read
/// leaves out features without a word. Here, a UTM zone 17 layer and views reaching past 90°
/// from its meridian, where transverse Mercator refuses places: such a view isn't bounded.
#[test]
fn a_query_box_leaves_out_nothing_in_view_when_its_edge_is_refused() {
    let r = Reproject::from_crs(Crs::from_epsg(32617).unwrap(), &mut Vec::new());
    let t = r.transformer().unwrap();
    for view in [
        Extent::new(-120.0, -30.0, 10.0, 30.0),
        Extent::new(-180.0, -60.0, 180.0, 60.0),
        Extent::new(-81.5, -5.0, 15.0, 5.0),
        Extent::new(-175.0, -1.0, -60.0, 1.0),
    ] {
        // Unbounded (`None`) reads everything in view.
        let Some(native) = r.extent_from_lonlat(&view) else { continue };
        for i in 0..=40 {
            for j in 0..=40 {
                let p = Coord::new(view.min_x + view.width() * f64::from(i) / 40.0, view.min_y + view.height() * f64::from(j) / 40.0);
                if let Some(q) = t.from_lonlat(p) {
                    assert!(native.contains(q), "{view:?}: {p:?} goes to {q:?}, outside the box queried, {native:?}");
                }
            }
        }
    }
    // A view in the zone is still a box.
    assert!(r.extent_from_lonlat(&Extent::new(-84.0, 30.0, -78.0, 40.0)).is_some_and(|e| !e.is_empty()));
}

/// The ISO text database servers give for dates (PostgreSQL with `DateStyle` ISO and `TimeZone`
/// UTC, MySQL's casts with `Z` added for a `TIMESTAMP`, SQL Server's style 126 cut to
/// microseconds) reads as dates without a count. What RustGIS can't hold (`infinity`, a year BC
/// or past 9999, MySQL's zero and partial dates) is null and counted, and none is null uncounted.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn database_date_text_reads_as_dates_and_counts_the_rest() {
    let mut unread = Vec::new();
    let clean = [
        (Some("2026-03-05"), FieldType::Date, "2026-03-05"),
        (Some("2026-03-05 14:30:00.25"), FieldType::DateTime, "2026-03-05T14:30:00.250"),
        (Some("2026-03-05 14:30:00+00"), FieldType::DateTime, "2026-03-05T14:30:00Z"),
        (Some("2026-03-05 14:30:00.123456Z"), FieldType::DateTime, "2026-03-05T14:30:00.123456Z"),
        (Some("2026-03-05T14:30:00.123456+02:00"), FieldType::DateTime, "2026-03-05T14:30:00.123456+02:00"),
        (None, FieldType::Date, "<Null>"),
    ];
    for (text, ty, shown) in clean {
        assert_eq!(server_date(text, ty, &mut unread, 1).to_string(), shown, "{text:?}");
    }
    assert!(unread.is_empty(), "clean text counts nothing: {unread:?}");
    for text in ["infinity", "-infinity", "0044-03-15 BC", "12026-01-01", "0000-00-00", "2026-03-00", "0000-00-00 00:00:00Z"] {
        assert_eq!(server_date(Some(text), FieldType::DateTime, &mut unread, 1).to_string(), "<Null>", "{text}");
    }
    assert_eq!(unread, [0, 7]);
    // A server gives at most microseconds: more digits aren't rounded, so it isn't a date.
    assert_eq!(server_date(Some("2026-03-05 14:30:00.1234567"), FieldType::DateTime, &mut unread, 1).to_string(), "<Null>");
    assert_eq!(unread, [0, 8]);
}

/// A read of a view whose box the layer's system can't bound (#1925) still matches the view: a
/// UTM zone 17 layer with 200 points in view, 2000 in the zone outside it and a row without
/// geometry, read with a limit of 10, gives a sample of the points in view and not the row. Sampling
/// the whole layer before testing the view would keep about one.
#[test]
fn a_view_its_system_cant_bound_reads_what_is_in_view() {
    let d = dir().join("unbounded-view");
    std::fs::create_dir_all(&d).unwrap();
    let mut ds = Dataset::new("pts", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    for i in 0..2200 {
        let (lat, k) = if i < 200 { (0.0, 25.0 * f64::from(i) / 200.0) } else { (40.0, 40.0 * f64::from(i - 200) / 2000.0) };
        let lon = -84.0 + 6.0 * f64::from(i % 37) / 37.0;
        ds.push(Some(Geometry::Point(Coord::new(lon, lat + k))), vec![Value::Int(i64::from(i))]);
    }
    ds.push(None, vec![Value::Int(-1)]);
    let view = Extent::new(-120.0, -30.0, 10.0, 30.0);
    let r = Reproject::from_crs(Crs::from_epsg(32617).unwrap(), &mut Vec::new());
    assert!(r.extent_from_lonlat(&view).is_none(), "the view's edge is refused");
    for ext in ["shp", "fgb", "gpkg", "parquet", "geojson"] {
        let p = d.join(format!("pts.{ext}"));
        write_unbounded_view_fixture(&ds, &p);
        let source = open_path(&p, None).unwrap();
        let mut sources: Vec<&dyn FeatureSource> = vec![source.as_ref()];
        let memory = if ext == "gpkg" {
            let bytes = Shared(std::sync::Arc::new(std::fs::read(&p).unwrap()));
            Some(gpkg_mem::GpkgMemSource::open_shared(bytes, None).unwrap())
        } else {
            None
        };
        if let Some(memory) = &memory {
            sources.push(memory);
        }
        let fetched = if matches!(ext, "fgb" | "parquet") {
            let url = format!("https://test.example/unbounded-view.{ext}");
            remote::testing::serve(&url, std::fs::read(&p).unwrap());
            Some(open_driven(&url))
        } else {
            None
        };
        if let Some(fetched) = &fetched {
            sources.push(fetched);
        }
        for source in sources {
            {
                // A zero limit cannot give a count for a view requiring a complete geometry scan.
                let probe = crate::progress::tests::Counted::cancelling_after(0);
                let _guard = crate::progress::install(Some(probe.clone()));
                let error = source.read(&Request { extent: Some(view), limit: 0 }).unwrap_err().to_string();
                assert!(error.contains("zero-limit") && error.contains("count"), "{ext}: {error}");
                assert_eq!(probe.done.load(std::sync::atomic::Ordering::Relaxed), 0, "{ext}");
            }
            for limit in [10, 2201] {
                let read = in_lonlat(source, source.read(&Request { extent: Some(view), limit }).unwrap());
                assert_eq!(read.matched, 200, "{ext}: count only view matches before sampling");
                if limit == 10 {
                    // Adaptive sampling keeps between half the limit and the limit.
                    assert!((5..=10).contains(&read.features.len()), "{ext}: {} features", read.features.len());
                } else {
                    // A full view read excludes the null-geometry row and all out-of-view points.
                    assert_eq!(read.features.len(), 200, "{ext}");
                    assert!(read.complete, "{ext}");
                }
                for f in &read.features {
                    let g = f.geometry.as_ref().unwrap_or_else(|| panic!("{ext}: feature {} has no geometry", f.id));
                    assert!(g.extent().intersects(&view), "{ext}: feature {} is outside the view", f.id);
                }
            }
            {
                // Cancel while scanning, not before the read starts; the view needs a complete scan.
                let probe = crate::progress::tests::Counted::cancelling_after(3);
                let _guard = crate::progress::install(Some(probe.clone()));
                assert!(matches!(source.read(&Request { extent: Some(view), limit: 10 }), Err(DataError::Cancelled)), "{ext}");
                assert_eq!(probe.done.load(std::sync::atomic::Ordering::Relaxed), 3, "{ext}");
            }
            if source.visits_chunks() {
                let probe = crate::progress::tests::Counted::cancelling_after(3);
                let _guard = crate::progress::install(Some(probe.clone()));
                assert!(matches!(source.visit_chunks(Some(view), 1 << 20, &mut |_| Ok(())), Err(DataError::Cancelled)));
                assert_eq!(probe.done.load(std::sync::atomic::Ordering::Relaxed), 3);
            }
        }
    }
}

/// A view match after the row cap must not disappear from an unbounded GeoParquet read.
#[test]
fn unbounded_parquet_view_refuses_a_capped_group() {
    let p = dir().join("capped-view.parquet");
    let mut ds = Dataset::new("pts", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
    ds.push(Some(Geometry::Point(Coord::new(-81.0, 40.0))), vec![Value::Int(0)]);
    ds.push(Some(Geometry::Point(Coord::new(-81.0, 0.0))), vec![Value::Int(1)]);
    write_unbounded_view_fixture(&ds, &p);
    let source = parquet::ParquetSource::open_path(&p, "pts").unwrap().with_group_rows(1);
    let req = Request { extent: Some(Extent::new(-120.0, -30.0, 10.0, 30.0)), limit: 10 };
    let error = source.read(&req).unwrap_err().to_string();
    assert!(error.contains("more than the 1 read from one"), "{error}");
}

/// Repeated text can expand from a small dictionary into one owned string per scanned row.
#[test]
fn unbounded_parquet_view_refuses_decoded_columns_over_budget() {
    let p = dir().join("decoded-view.parquet");
    let mut ds = Dataset::new("pts", GeometryType::Point, vec![Field::new("label", FieldType::Text)]);
    for _ in 0..20 {
        ds.push(Some(Geometry::Point(Coord::new(-81.0, 0.0))), vec![Value::Text("x".repeat(1024))]);
    }
    write_unbounded_view_fixture(&ds, &p);
    let source = parquet::ParquetSource::open_path(&p, "pts").unwrap().with_scan_bytes(16 << 10);
    let req = Request { extent: Some(Extent::new(-120.0, -30.0, 10.0, 30.0)), limit: 1 };
    let error = source.read(&req).unwrap_err().to_string();
    assert!(error.contains("decoded-column budget"), "{error}");
    let error = source.visit_chunks(req.extent, 4096, &mut |_| Ok(())).unwrap_err().to_string();
    assert!(error.contains("decoded-column budget"), "{error}");
    let clean = Request { extent: Some(Extent::new(-84.0, -1.0, -78.0, 1.0)), limit: 1 };
    assert_eq!(source.read(&clean).unwrap().features.len(), 1);
    let source = parquet::ParquetSource::open_path(&p, "pts").unwrap();
    let batch = source.read(&req).unwrap();
    assert_eq!(batch.matched, 20);
    assert_eq!(batch.features[0].attrs[0], Value::Text("x".repeat(1024)));
}

/// The same small file must remain readable with plain or dictionary-encoded binary columns.
#[test]
fn unbounded_parquet_scan_accepts_two_thousand_distinct_points() {
    let p = dir().join("ordinary-view.parquet");
    let mut ds = Dataset::new("pts", GeometryType::Point, vec![]);
    for i in 0..2000 {
        ds.push(Some(Geometry::Point(Coord::new(-82.0 + f64::from(i) / 2000.0, 0.0))), vec![]);
    }
    write_projected_parquet_fixture(&ds, &p, 2000, true);
    let source = parquet::ParquetSource::open_path(&p, "pts").unwrap();
    let req = Request { extent: Some(Extent::new(-120.0, -30.0, 10.0, 30.0)), limit: 10 };
    let batch = source.read(&req).unwrap();
    assert_eq!(batch.matched, 2000);
    assert!((5..=10).contains(&batch.features.len()));
    drop(source);
    write_projected_parquet_fixture(&ds, &p, 2000, false);
    let source = parquet::ParquetSource::open_path(&p, "pts").unwrap();
    assert_eq!(source.read(&req).unwrap().matched, 2000);
}

/// Streamed sources own the scan count; nested export writers must still observe Cancel.
#[test]
fn streamed_export_progress_counts_each_source_row_once() {
    use std::sync::atomic::Ordering;
    let d = dir();
    let mut ds = Dataset::new("pts", GeometryType::Point, vec![]);
    for i in 0..10 {
        ds.push(Some(Geometry::Point(Coord::new(f64::from(i), 0.0))), vec![]);
    }
    for input in ["shp", "fgb", "gpkg", "parquet"] {
        let path = d.join(format!("input.{input}"));
        crate::io::write_path(&ds, &path).unwrap();
        let source = open_path(&path, None).unwrap();
        let memory = if input == "gpkg" {
            Some(gpkg_mem::GpkgMemSource::open_shared(Shared(Arc::new(std::fs::read(&path).unwrap())), None).unwrap())
        } else {
            None
        };
        let mut sources: Vec<&dyn FeatureSource> = vec![source.as_ref()];
        if let Some(memory) = &memory {
            sources.push(memory);
        }
        for (i, source) in sources.into_iter().enumerate() {
            for output in ["geojson", "csv", "gpkg"] {
                let progress = Arc::new(crate::progress::tests::Counted::default());
                let _guard = crate::progress::install(Some(progress.clone()));
                let out = d.join(format!("output-{input}-{i}.{output}"));
                assert_eq!(crate::export::write_source_in(source, &ds, &out, Crs::LonLat).unwrap().written, 10);
                assert_eq!(progress.total.load(Ordering::Relaxed), 10, "{input} to {output}");
                assert_eq!(progress.done.load(Ordering::Relaxed), 10, "{input} to {output}");
            }
        }
    }
}

/// Per-group limits do not bound the sum of the masks retained across many small groups.
#[test]
fn unbounded_parquet_view_bounds_aggregate_row_masks() {
    let p = dir().join("many-groups.parquet");
    let mut ds = Dataset::new("pts", GeometryType::Point, vec![]);
    ds.features =
        (0..40_000).map(|id| crate::Feature { id, geometry: Some(Geometry::Point(Coord::new(-81.0, 0.0))), attrs: vec![] }).collect();
    write_projected_parquet_fixture(&ds, &p, 100, true);
    let source = parquet::ParquetSource::open_path(&p, "pts").unwrap().with_scan_bytes(32 << 10);
    let req = Request { extent: Some(Extent::new(-120.0, -30.0, 10.0, 30.0)), limit: 10 };
    let error = source.read(&req).unwrap_err().to_string();
    assert!(error.contains("row-mask budget"), "{error}");
    let source = parquet::ParquetSource::open_path(&p, "pts").unwrap();
    assert_eq!(source.read(&req).unwrap().matched, 40_000);
}

/// Writes the projected reader fixture, including formats whose public writer accepts only WGS 84.
fn write_unbounded_view_fixture(ds: &Dataset, path: &Path) {
    let crs = Crs::from_epsg(32617).unwrap();
    match path.extension().and_then(|s| s.to_str()).unwrap() {
        "geojson" => {
            let projected = crate::io::project(ds, crs).unwrap();
            let mut json = crate::io::to_geojson(&projected);
            json["crs"] = serde_json::json!({"type": "name", "properties": {"name": "EPSG:32617"}});
            std::fs::write(path, serde_json::to_vec(&json).unwrap()).unwrap();
        }
        "parquet" => write_projected_parquet_fixture(ds, path, 100, true),
        _ => {
            crate::io::write_path_in(ds, path, crs).unwrap();
        }
    }
}

/// A UTM fixture with selectable row-group size and binary encoding, using the same covering.
fn write_projected_parquet_fixture(ds: &Dataset, path: &Path, rows: usize, dictionary: bool) {
    let crs = Crs::from_epsg(32617).unwrap();
    use arrow_array::{BinaryArray, Float64Array, RecordBatch, StringArray, StructArray};
    use arrow_schema::{DataType, Field as AField, Schema};
    let projected = crate::io::project(ds, crs).unwrap();
    let wkbs: Vec<_> = projected.features.iter().map(|f| f.geometry.as_ref().map(wkb::encode)).collect();
    let geometry = BinaryArray::from(wkbs.iter().map(|w| w.as_deref()).collect::<Vec<_>>());
    let bbox_fields = ["xmin", "ymin", "xmax", "ymax"].map(|n| Arc::new(AField::new(n, DataType::Float64, true)));
    let coord = |at: fn(Extent) -> f64| -> arrow_array::ArrayRef {
        Arc::new(Float64Array::from(projected.features.iter().map(|f| f.geometry.as_ref().map(|g| at(g.extent()))).collect::<Vec<_>>()))
    };
    let bbox = StructArray::new(
        bbox_fields.iter().cloned().collect(),
        vec![coord(|e| e.min_x), coord(|e| e.min_y), coord(|e| e.max_x), coord(|e| e.max_y)],
        None,
    );
    let mut fields = vec![
        AField::new("geometry", DataType::Binary, true),
        AField::new("bbox", DataType::Struct(bbox_fields.iter().cloned().collect()), false),
    ];
    let mut columns: Vec<arrow_array::ArrayRef> = vec![Arc::new(geometry), Arc::new(bbox)];
    for (i, field) in ds.fields.iter().enumerate().filter(|(_, f)| f.ty == FieldType::Text) {
        fields.push(AField::new(&field.name, DataType::Utf8, true));
        columns.push(Arc::new(StringArray::from(
            ds.features
                .iter()
                .map(|f| match f.attrs.get(i) {
                    Some(Value::Text(s)) => Some(s.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
        )));
    }
    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(schema.clone(), columns).unwrap();
    let geo = serde_json::json!({"version": "1.1.0", "primary_column": "geometry", "columns": {"geometry": {
        "encoding": "WKB", "geometry_types": ["Point"], "crs": {"type": "ProjectedCRS", "id": {"authority": "EPSG", "code": 32617}},
        "covering": {"bbox": {"xmin": ["bbox", "xmin"], "ymin": ["bbox", "ymin"], "xmax": ["bbox", "xmax"], "ymax": ["bbox", "ymax"]}}
    }}});
    let props = ::parquet::file::properties::WriterProperties::builder()
        .set_max_row_group_row_count(Some(rows))
        .set_dictionary_enabled(dictionary)
        .set_key_value_metadata(Some(vec![::parquet::file::metadata::KeyValue::new("geo".into(), geo.to_string())]))
        .build();
    let mut writer = ::parquet::arrow::ArrowWriter::try_new(std::fs::File::create(path).unwrap(), schema, Some(props)).unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}
