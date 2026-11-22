//! Sources kept in their own coordinate system (#2032 C2f-3): files GDAL converts and DuckDB
//! results, held in the system they say when RustGIS reads it, and converted to longitude/latitude
//! as before, and said, when it doesn't.

use crate::{Project, Session};
use rustgis_data::raster::Crs;
use serde_json::json;

/// A folder removed however the test ends.
struct Dir(std::path::PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(name: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("rustgis-sources-own-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    Dir(d)
}

fn first_point(s: &Session, id: u32) -> rustgis_geom::Coord {
    let l = s.map().layer(id).unwrap();
    let Some(rustgis_data::Geometry::Point(c)) = l.dataset.features.iter().next().and_then(|f| f.geometry.clone()) else { panic!() };
    c
}

/// A MapInfo table in LV95 added through GDAL is held in LV95 with its coordinates as stored; a
/// GMT file in a projection RustGIS can't read is converted to longitude/latitude, and said.
#[test]
fn files_gdal_reads_are_held_in_their_own_system() {
    use rustgis_data::source::gdal;
    if !gdal::available() {
        return;
    }
    let d = dir("gdal");
    let made = |name: &str, driver: &str, ext: &str, srs: &str| {
        let csv = d.0.join(format!("{name}.csv"));
        std::fs::write(&csv, "WKT,name\n\"POINT (2600000 1200000)\",Bern\n").unwrap();
        let file = d.0.join(format!("{name}.{ext}"));
        let out = std::process::Command::new(gdal::program("ogr2ogr").unwrap())
            .args(["-f", driver, "-a_srs", srs])
            .arg(&file)
            .arg(&csv)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        file
    };
    let mut s = Session::new(Project::default());
    let mark = s.notice_mark();
    let r = s.run("layer.addData", &json!({"path": made("lv95", "MapInfo File", "tab", "EPSG:2056"), "stream": false})).unwrap();
    let id = r["layer"].as_u64().unwrap() as u32;
    assert_ne!(s.map().layer(id).unwrap().dataset.coord_crs(), Crs::LonLat);
    let c = first_point(&s, id);
    assert!((c.x - 2_600_000.0).abs() < 0.01 && (c.y - 1_200_000.0).abs() < 0.01, "{c:?}");
    assert!(!s.notices_since(mark).iter().any(|n| n.text.contains("longitude/latitude (WGS 84)")));
    let mark = s.notice_mark();
    let r =
        s.run("layer.addData", &json!({"path": made("nell", "GMT", "gmt", "+proj=nell +datum=WGS84 +units=m"), "stream": false})).unwrap();
    let id = r["layer"].as_u64().unwrap() as u32;
    assert_eq!(s.map().layer(id).unwrap().dataset.coord_crs(), Crs::LonLat);
    let c = first_point(&s, id);
    assert!(c.x.abs() <= 180.0 && c.y.abs() <= 90.0, "{c:?}");
    let said = s.notices_since(mark);
    assert!(said.iter().any(|n| n.text.contains("can't read its coordinate system, so GDAL converted it")), "{said:?}");
}

/// A DuckDB result whose geometry says its system, or is given one, is held in it with its
/// coordinates as the query gives them.
#[test]
fn duckdb_results_are_held_in_their_own_system() {
    if !rustgis_data::source::duckdb::available() {
        return;
    }
    let lv95 = Crs::from_epsg(2056).unwrap();
    let mut s = Session::new(Project::default());
    for p in [
        json!({"sql": "SELECT 'Bern' AS name, ST_Point(2600000, 1200000) AS geom", "crs": "EPSG:2056", "stream": false}),
        json!({"sql": "SELECT 'Bern' AS name, ST_Point(2600000, 1200000)::GEOMETRY('EPSG:2056') AS geom", "stream": false}),
    ] {
        let r = s.run("layer.addSql", &p).unwrap();
        let id = r["layer"].as_u64().unwrap() as u32;
        assert_eq!(s.map().layer(id).unwrap().dataset.coord_crs(), lv95, "{p}");
        assert_eq!(first_point(&s, id), rustgis_geom::Coord::new(2_600_000.0, 1_200_000.0), "{p}");
    }
    // One in longitude/latitude stays so.
    let r = s.run("layer.addSql", &json!({"sql": "SELECT ST_Point(7.5, 47) AS geom", "stream": false})).unwrap();
    let id = r["layer"].as_u64().unwrap() as u32;
    assert_eq!(s.map().layer(id).unwrap().dataset.coord_crs(), Crs::LonLat);
    assert_eq!(first_point(&s, id), rustgis_geom::Coord::new(7.5, 47.0));
}
