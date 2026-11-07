//! A layer on another body than the Earth (IAU 2015, opengeos/RustGIS#1448), added and exported
//! through the commands: adding it says so once, exporting it says what is written instead of its
//! system, and an export that would write other numbers than the layer's is refused.
use rustgis_engine::{
    Project, Session,
    data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value, io, raster::Crs},
};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Mars as a sphere, longitude/latitude (IAU_2015:49900), as GDAL writes it to a `.prj`.
const MARS: &str = r#"GEOGCS["GCS_Mars_2015_-_Sphere_Ocentric",DATUM["D_Mars_2015_-_Sphere",SPHEROID["Mars_2015_-_Sphere",3396190.0,0.0]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]]"#;

const ANOTHER_BODY: &str = "belongs to another body than the Earth";

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("rustgis-planetary-export-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Olympus Mons and Jezero crater, as a Shapefile with a Mars `.prj`.
fn craters(dir: &Path) -> PathBuf {
    let mut ds = Dataset::new("craters", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    ds.push(Some(Geometry::Point(Coord::new(-133.8, 18.65))), vec![Value::Text("Olympus Mons".into())]);
    ds.push(Some(Geometry::Point(Coord::new(77.45, 18.44))), vec![Value::Text("Jezero".into())]);
    let (parts, _) = io::shapefile_parts_in(&ds, Crs::LonLat).unwrap();
    for (ext, bytes) in parts {
        let bytes = if ext == "prj" { MARS.as_bytes().to_vec() } else { bytes };
        std::fs::write(dir.join(format!("craters.{ext}")), bytes).unwrap();
    }
    dir.join("craters.shp")
}

fn said(s: &Session, mark: u64, what: &str) -> Vec<String> {
    s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains(what)).collect()
}

#[test]
fn a_mars_layer_is_said_when_added_and_when_exported() {
    let d = dir();
    let shp = craters(&d);
    for stream in [false, true] {
        let mut s = Session::new(Project::default());
        let mark = s.notice_mark();
        let id = s.run("layer.addData", &json!({"path": shp, "stream": stream})).unwrap()["layer"].as_u64().unwrap() as u32;
        assert_eq!(said(&s, mark, ANOTHER_BODY).len(), 1, "stream {stream}: {:?}", s.notices_since(mark));
        assert!(said(&s, mark, "few hundred metres").is_empty(), "stream {stream}");
        assert!(s.map().layer(id).unwrap().dataset.crs.is_some_and(Crs::is_planetary), "stream {stream}");

        // As WGS 84 longitude/latitude: written, and said.
        for ext in ["geojson", "gpkg"] {
            let out = d.join(format!("out-{stream}.{ext}"));
            let _ = std::fs::remove_file(&out);
            let mark = s.notice_mark();
            s.run("share.exportLayer", &json!({"layer": id, "path": out})).unwrap();
            let notes = said(&s, mark, "on another body than the Earth");
            assert_eq!(notes.len(), 1, "{ext}: {:?}", s.notices_since(mark));
            assert!(notes[0].contains("written as WGS 84 (EPSG:4326)"), "{notes:?}");
            assert!(said(&s, mark, "few hundred metres").is_empty(), "{ext}");
            let (back, _) = io::read_path(&out).unwrap();
            assert_eq!(back.features.len(), 2, "{ext}");
        }
        // In its own system, a Shapefile: its .prj names no body.
        let out = d.join(format!("out-{stream}.shp"));
        let mark = s.notice_mark();
        s.run("share.exportLayer", &json!({"layer": id, "path": out})).unwrap();
        assert_eq!(said(&s, mark, "only by the ellipsoid's axes").len(), 1, "{:?}", s.notices_since(mark));

        // Projected onto the Earth, the numbers would be the Earth's metres: refused, nothing written.
        let out = d.join(format!("utm-{stream}.gpkg"));
        let _ = std::fs::remove_file(&out);
        let e = s.run("share.exportLayer", &json!({"layer": id, "path": out, "crs": "EPSG:32631"})).unwrap_err();
        assert!(e.contains(ANOTHER_BODY) && e.contains("EPSG:32631"), "{e}");
        assert!(!out.exists());
    }
}

#[test]
fn an_earth_layer_exports_as_before() {
    let d = dir().join("earth");
    std::fs::create_dir_all(&d).unwrap();
    let mut ds = Dataset::new("parcels", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(5.9, 52.0))), vec![]);
    let src = d.join("parcels.geojson");
    io::write_path(&ds, &src).unwrap();
    let mut s = Session::new(Project::default());
    let mark = s.notice_mark();
    let id = s.run("layer.addData", &json!({"path": src})).unwrap()["layer"].as_u64().unwrap();
    let out = d.join("utm.gpkg");
    let _ = std::fs::remove_file(&out);
    s.run("share.exportLayer", &json!({"layer": id, "path": out, "crs": "EPSG:32631"})).unwrap();
    assert!(s.notices_since(mark).iter().all(|n| !n.text.contains("bod")), "{:?}", s.notices_since(mark));
}
