//! Exports and write-backs in a layer's own coordinate system (#2032 C2f-1): GeoParquet with its
//! system, CSV x and y in a system asked for, GeoJSON published back in the system its legacy
//! `crs` member names, and the sync record kept over the layer's own coordinates.

use crate::Session;
use serde_json::{Value, json};

/// A folder removed however the test ends.
struct Dir(std::path::PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn dir(name: &str) -> Dir {
    let d = std::env::temp_dir().join(format!("rustgis-formats-out-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    Dir(d)
}

const LV95: &str = r#"{"type":"FeatureCollection","crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::2056"}},"features":[{"type":"Feature","properties":{"name":"Bern"},"geometry":{"type":"Point","coordinates":[2600000,1200000]}},{"type":"Feature","properties":{"name":"Thun"},"geometry":{"type":"Point","coordinates":[2614000,1178000]}}]}"#;

/// A GeoJSON in LV95 with a legacy `crs` member is read, edited, published back in LV95 with its
/// member, and reads in sync after; its record holds the digest of its own coordinates, which
/// a project keeps.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_legacy_crs_geojson_publishes_back_in_its_system_and_stays_in_sync() {
    let d = dir("geojson");
    let path = d.0.join("towns.geojson");
    std::fs::write(&path, LV95).unwrap();
    let mut s = Session::new(crate::Project::default());
    let id = s.run("layer.addData", &json!({"path": path, "name": "towns"})).unwrap()["layer"].as_u64().unwrap() as u32;
    let state = |s: &mut Session| s.run("source.syncState", &json!({"layer": id})).unwrap()["state"].clone();
    let synced = s.map().layer(id).unwrap().synced.clone().unwrap();
    assert!(synced.native.is_some(), "{synced:?}");
    assert_eq!(state(&mut s), "inSync");
    s.run("edit.setValue", &json!({"layer": id, "row": 0, "field": "name", "value": "Bärn"})).unwrap();
    assert_eq!(state(&mut s), "edited");
    let out = s.run("source.publish", &json!({"layer": id})).unwrap();
    assert_eq!(out["published"], true, "{out}");
    let text: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let want: Value = serde_json::from_str(&LV95.replace("\"Bern\"", "\"Bärn\"")).unwrap();
    assert_eq!(text, want);
    assert_eq!(state(&mut s), "inSync");
    // Saved and opened again, the record still answers for the layer.
    let (saved, _) = s.project.saved_text(None).unwrap();
    let again = crate::project::Project::from_json(&saved).unwrap();
    let l = again.maps[0].layers.iter().find(|l| l.name == "towns").unwrap();
    let record = l.synced.clone().unwrap();
    assert!(record.native.is_some());
    assert_eq!(record.digest_of(&l.dataset).unwrap(), record.digest());
}

/// The fixture saved with a record of the layer's own coordinates opens with the layer in sync
/// with it, and the same record without them (as a release before it saved it) does too.
#[test]
fn a_saved_record_of_the_layers_own_coordinates_is_read() {
    let p = crate::project::Project::from_json(include_str!("../../../../schema/fixtures/held_synced.rgis")).unwrap();
    let l = &p.maps[0].layers[0];
    let record = l.synced.clone().unwrap();
    assert!(record.native.is_some());
    assert_eq!(record.digest_of(&l.dataset).unwrap(), record.digest());
    let older = rustgis_data::sync::Synced { native: None, ..record };
    assert_eq!(older.digest_of(&l.dataset).unwrap(), older.digest());
}

/// `share.exportLayer` writes a held layer's GeoParquet in its own system by default, and a CSV
/// in it when asked for, saying the CSV can't hold it; without asking, CSV stays WGS 84.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn exports_take_the_layers_own_system() {
    let d = dir("exports");
    let path = d.0.join("towns.geojson");
    std::fs::write(&path, LV95).unwrap();
    let mut s = Session::new(crate::Project::default());
    s.run("layer.addData", &json!({"path": path, "name": "towns"})).unwrap();
    let parquet = d.0.join("towns.parquet");
    let out = s.run("share.exportLayer", &json!({"layer": "towns", "path": parquet})).unwrap();
    assert_eq!(out["crs"], "EPSG:2056", "{out}");
    let (back, _) = rustgis_data::io::read_path(&parquet).unwrap();
    assert_eq!(back.coord_crs(), rustgis_data::raster::Crs::from_epsg(2056).unwrap());
    let csv = d.0.join("towns.csv");
    let out = s.run("share.exportLayer", &json!({"layer": "towns", "path": csv})).unwrap();
    assert_eq!(out["crs"], "EPSG:4326");
    assert!(std::fs::read_to_string(&csv).unwrap().starts_with("longitude,latitude,name\n"));
    let mark = s.notice_mark();
    let out = s.run("share.exportLayer", &json!({"layer": "towns", "path": csv, "crs": "EPSG:2056"})).unwrap();
    assert_eq!(out["crs"], "EPSG:2056");
    assert_eq!(std::fs::read_to_string(&csv).unwrap(), "x,y,name\n2600000,1200000,Bern\n2614000,1178000,Thun\n");
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(said.iter().any(|n| n.contains("EPSG:2056") && n.contains("can't say")), "{said:?}");
    // GeoJSON still refuses another system.
    assert!(s.run("share.exportLayer", &json!({"layer": "towns", "path": d.0.join("t.geojson"), "crs": "EPSG:2056"})).is_err());
}

/// A Shapefile layer in LV95 that a project saved before RustGIS kept a file's own coordinates
/// holds in longitude/latitude, with a record that has no digest of its own coordinates, is
/// published: it takes the file's rows in LV95 with the system they're in, and reads in sync.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_older_projects_layer_takes_the_files_own_system_when_published() {
    let d = dir("older");
    let geojson = d.0.join("towns.geojson");
    std::fs::write(&geojson, LV95).unwrap();
    let mut s = Session::new(crate::Project::default());
    s.run("layer.addData", &json!({"path": geojson, "name": "source"})).unwrap();
    let shp = d.0.join("towns.shp");
    s.run("share.exportLayer", &json!({"layer": "source", "path": shp})).unwrap();
    let id = s.run("layer.addData", &json!({"path": shp, "name": "towns"})).unwrap()["layer"].as_u64().unwrap() as u32;
    let lv95 = rustgis_data::raster::Crs::from_epsg(2056).unwrap();
    // As an older release held it: in longitude/latitude, its record of those rows only.
    {
        let l = s.map_mut().layer_mut(id).unwrap();
        assert_eq!(l.dataset.coord_crs(), lv95);
        l.dataset = rustgis_data::io::project(&l.dataset, rustgis_data::raster::Crs::LonLat).unwrap();
        l.touch_geometry();
        let rows = rustgis_data::sync::rows_digest(&l.dataset).unwrap();
        let files = l.synced.clone().unwrap().files;
        l.synced = Some(rustgis_data::sync::Synced { files, rows, native: None });
    }
    let state = |s: &mut Session| s.run("source.syncState", &json!({"layer": id})).unwrap()["state"].clone();
    assert_eq!(state(&mut s), "inSync");
    s.run("edit.setValue", &json!({"layer": id, "row": 0, "field": "name", "value": "Bärn"})).unwrap();
    let out = s.run("source.publish", &json!({"layer": id})).unwrap();
    assert_eq!(out["published"], true, "{out}");
    let l = s.map().layer(id).unwrap();
    assert_eq!(l.dataset.coord_crs(), lv95, "the file's rows come with the system they're in");
    let first = l.dataset.features.iter().next().unwrap().geometry.clone().unwrap();
    let rustgis_data::Geometry::Point(c) = first else { panic!("{first:?}") };
    assert!((c.x - 2_600_000.0).abs() < 1e-6 && (c.y - 1_200_000.0).abs() < 1e-6, "{c:?}");
    assert!(l.synced.as_ref().unwrap().native.is_some());
    assert_eq!(state(&mut s), "inSync");
}
