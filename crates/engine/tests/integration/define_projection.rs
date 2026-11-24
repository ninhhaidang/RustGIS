//! Define Projection reinterprets native coordinates; it never edits the source files.
use rustgis_engine::{
    Project, Session,
    data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value, io, raster::Crs},
};
use serde_json::json;

/// A small projected point layer with a fractional coordinate. The writer
/// receives native coordinates, so fixture construction does not use the conversion under test.
fn native() -> Dataset {
    let mut ds = Dataset::new("parcels", GeometryType::Point, vec![Field::new("owner", FieldType::Text)]);
    ds.push(Some(Geometry::Point(Coord::new(451_219.33, 5_411_550.20))), vec![Value::Text("Paris".into())]);
    ds
}

/// Writes the native numbers with a deliberately chosen (or missing) CRS sidecar.
fn zip(crs: Option<Crs>) -> Vec<u8> {
    let parts = io::shapefile_parts_in(&native(), crs.unwrap_or(Crs::LonLat)).unwrap().0;
    let parts = parts
        .into_iter()
        .filter(|(ext, _)| *ext != "prj" || crs.is_some())
        .map(|(ext, bytes)| (format!("data/PARCELS.{}", ext.to_ascii_uppercase()), bytes))
        .collect::<Vec<_>>();
    rustgis_engine::data::zip::write(&parts).unwrap()
}

/// Uploads exercise the browser/ZIP command path, not the on-disk reader.
fn upload(s: &mut Session, bytes: &[u8], crs: Option<&str>) -> u32 {
    use base64::Engine;
    let mut p = json!({"fileName": "parcels.zip", "data": base64::engine::general_purpose::STANDARD.encode(bytes)});
    if let Some(c) = crs {
        p["crs"] = json!(c);
    }
    s.run("layer.addDataBytes", &p).unwrap()["layer"].as_u64().unwrap() as u32
}

/// Reads the first fixture point without deriving an expected result through a transformer.
fn point(ds: &Dataset) -> Coord {
    // In longitude/latitude, from the system the layer holds its coordinates in (the file's own
    // as it is read, the numbers as they are until a system is defined).
    ds.in_lonlat().unwrap().features[0].geometry.as_ref().unwrap().label_point().unwrap()
}

/// Checks the known surveyed coordinate and its attribute survived the public workflow.
fn paris(ds: &Dataset) {
    let c = point(ds);
    assert!((c.x - 2.335).abs() < 1e-6 && (c.y - 48.855).abs() < 1e-6, "{c:?}");
    assert_eq!(ds.value(0, 0), &Value::Text("Paris".into()));
}

#[test]
fn missing_prj_layer_command_preserves_native_numbers_and_undoes_as_one_step() {
    let mut s = Session::new(Project::default());
    let id = upload(&mut s, &zip(None), None);
    s.map_mut().layer_mut(id).unwrap().dataset.push(None, vec![Value::Text("No shape".into())]);
    let before = s.map().layer(id).unwrap().clone();
    s.run("select.rows", &json!({"layer": id, "rows": [0]})).unwrap();
    s.run("layer.defineProjection", &json!({"layer": id, "crs": "EPSG:32631"})).unwrap();
    let layer = s.map().layer(id).unwrap();
    paris(&layer.dataset);
    assert_eq!(layer.selection, vec![0]);
    assert!(layer.dataset.features[1].geometry.is_none());
    assert!(layer.geometry_rev > before.geometry_rev);
    let exported = io::project(&layer.dataset, Crs::parse("EPSG:32631").unwrap()).unwrap();
    // The numbers written: the layer's own, held in the system defined.
    let c = exported.features[0].geometry.as_ref().unwrap().label_point().unwrap();
    assert!((c.x - 451_219.33).abs() < 1e-5 && (c.y - 5_411_550.20).abs() < 1e-5, "{c:?}");
    assert_eq!(io::default_export_crs(&layer.dataset, "shp"), Crs::parse("EPSG:32631").unwrap());
    let saved = s.project.saved_text(None).unwrap().0;
    let restored = Project::from_json(&saved).unwrap();
    paris(&restored.maps[0].layers[0].dataset);
    s.run("edit.undo", &json!({})).unwrap();
    assert_eq!(s.map().layer(id).unwrap().dataset, before.dataset);
    s.run("edit.redo", &json!({})).unwrap();
    paris(&s.map().layer(id).unwrap().dataset);
}

#[test]
fn wrong_prj_can_be_replaced_without_reprojecting_native_numbers() {
    let mut s = Session::new(Project::default());
    let id = upload(&mut s, &zip(Some(Crs::WebMercator)), None);
    assert!((point(&s.map().layer(id).unwrap().dataset).x - 2.335).abs() > 0.1);
    s.run("layer.defineProjection", &json!({"layer": id, "crs": "EPSG:32631"})).unwrap();
    paris(&s.map().layer(id).unwrap().dataset);
}

/// A `.prj` that gives the shapes no place in longitude/latitude keeps them as the file holds
/// them (#2032 C2f-4b), so Define Projection places them; an override on import does as well.
#[test]
fn zip_override_precedes_wrong_prj_and_define_projection_places_what_it_kept() {
    // This nonzero datum shift rejects projected numbers mislabelled as geographic.
    let wrong = Crs::parse("EPSG:4277").unwrap();
    assert!(wrong.transformer().unwrap().to_lonlat(point(&native())).is_none(), "fixture must have no place with its wrong CRS");
    let bytes = zip(Some(wrong));
    let mut s = Session::new(Project::default());
    let id = upload(&mut s, &bytes, None);
    let ds = &s.map().layer(id).unwrap().dataset;
    assert!(!ds.crs_incomplete);
    let stored = ds.features.row(0).and_then(|r| r.geometry().cloned());
    assert_eq!(stored, Some(Geometry::Point(Coord::new(451_219.33, 5_411_550.20))), "kept as the file holds it");
    s.run("layer.defineProjection", &json!({"layer": id, "crs": "EPSG:32631"})).unwrap();
    paris(&s.map().layer(id).unwrap().dataset);
    let repaired = upload(&mut s, &bytes, Some("EPSG:32631"));
    paris(&s.map().layer(repaired).unwrap().dataset);
    assert!(!s.map().layer(repaired).unwrap().dataset.crs_incomplete);
}

#[test]
fn invalid_crs_or_unconvertible_coordinate_leaves_layer_unchanged() {
    let mut s = Session::new(Project::default());
    let id = upload(&mut s, &zip(None), None);
    let before = s.map().layer(id).unwrap().dataset.clone();
    for crs in ["", "garbage", "EPSG:99999999", "EPSG:4326"] {
        assert!(s.run("layer.defineProjection", &json!({"layer": id, "crs": crs})).is_err(), "{crs}");
        assert_eq!(s.map().layer(id).unwrap().dataset, before);
    }
}

#[test]
fn loose_and_streamed_overrides_survive_project_reopen_and_do_not_write_prj() {
    // Streamed Shapefiles are indexed: keep those out of the user's cache (#1230).
    rustgis_engine::data::source::shp::use_temporary_index_dir();
    let dir = std::env::temp_dir().join(format!("rustgis-m1-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let parts = io::shapefile_parts_in(&native(), Crs::parse("EPSG:4269").unwrap()).unwrap().0;
    for (ext, bytes) in &parts {
        std::fs::write(dir.join(format!("parcels.{ext}")), bytes).unwrap();
    }
    let prj = std::fs::read(dir.join("parcels.prj")).unwrap();
    for crs in ["EPSG:32631", "+proj=utm +zone=31 +datum=WGS84 +units=m +no_defs"] {
        for stream in [false, true] {
            let mut s = Session::new(Project::default());
            let result = s.run("layer.addData", &json!({"path": dir.join("parcels.shp"), "crs": crs, "stream": stream})).unwrap();
            let id = result["layer"].as_u64().unwrap() as u32;
            paris(&s.map().layer(id).unwrap().load_all().unwrap());
            if stream {
                assert!(
                    s.run("layer.defineProjection", &json!({"layer": id, "crs": "EPSG:32631"})).unwrap_err().contains("loaded into memory")
                );
                let saved = s.project.saved_text(None).unwrap().0;
                let restored = Session::new(Project::from_json(&saved).unwrap());
                let restored_data = restored.map().layer(id).unwrap().load_all().unwrap();
                paris(&restored_data);
                assert!(restored_data.crs.is_some(), "the native CRS must remain the export default");
            }
            assert_eq!(std::fs::read(dir.join("parcels.prj")).unwrap(), prj);
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn import_override_accepts_missing_prj_wkt_and_custom_proj_definitions() {
    let utm = Crs::parse("EPSG:32631").unwrap();
    let wkt = rustgis_engine::data::raster::wkt::to_wkt(utm).unwrap();
    for crs in ["EPSG:32631", &wkt, "+proj=utm +zone=31 +datum=WGS84 +units=m +no_defs"] {
        let mut s = Session::new(Project::default());
        let id = upload(&mut s, &zip(None), Some(crs));
        paris(&s.map().layer(id).unwrap().dataset);
        let saved = s.project.saved_text(None).unwrap().0;
        let restored = Session::new(Project::from_json(&saved).unwrap());
        paris(&restored.map().layer(id).unwrap().dataset);
        assert!(restored.map().layer(id).unwrap().dataset.crs.is_some());
    }
}

#[test]
fn failure_on_a_later_feature_does_not_apply_a_partial_projection() {
    let mut ds = Dataset::new("Mixed", GeometryType::Point, vec![]);
    ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
    ds.push(Some(Geometry::Point(Coord::new(f64::MAX, 0.0))), vec![]);
    let mut s = Session::new(Project::default());
    let id = s.map_mut().add_dataset(ds.clone());
    s.active_layer = Some(id);
    assert!(s.run("layer.defineProjection", &json!({"crs": "EPSG:3857"})).is_err());
    assert_eq!(s.map().layer(id).unwrap().dataset, ds);
}

#[test]
fn zip_override_applies_to_each_shapefile_and_keeps_geojson_coordinates() {
    let mut entries = Vec::new();
    for name in ["one", "two"] {
        for (ext, bytes) in io::shapefile_parts(&native()).unwrap().0 {
            entries.push((format!("{name}/parcels.{ext}"), bytes));
        }
    }
    entries.push(("other.geojson".into(), br#"{"type":"Point","coordinates":[10,20]}"#.to_vec()));
    let bytes = rustgis_engine::data::zip::write(&entries).unwrap();
    let read = io::read_zip_in("several.zip", &bytes, Some(Crs::parse("EPSG:32631").unwrap())).unwrap();
    assert_eq!(read.len(), 3);
    paris(&read[0].0);
    paris(&read[1].0);
    assert_eq!(point(&read[2].0), Coord::new(10.0, 20.0));
}

#[test]
fn blank_shapefile_crs_keeps_prj_for_whole_and_streamed_imports() {
    // Streamed Shapefiles are indexed: keep those out of the user's cache (#1230).
    rustgis_engine::data::source::shp::use_temporary_index_dir();
    let dir = std::env::temp_dir().join(format!("rustgis-m1-blank-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (ext, bytes) in io::shapefile_parts_in(&native(), Crs::parse("EPSG:32631").unwrap()).unwrap().0 {
        std::fs::write(dir.join(format!("parcels.{ext}")), bytes).unwrap();
    }
    for crs in ["", " \t ", " EPSG:32631 "] {
        for stream in [false, true] {
            let mut s = Session::new(Project::default());
            let result = s.run("layer.addData", &json!({"path": dir.join("parcels.shp"), "crs": crs, "stream": stream})).unwrap();
            let id = result["layer"].as_u64().unwrap() as u32;
            paris(&s.map().layer(id).unwrap().load_all().unwrap());
            let saved = s.project.saved_text(None).unwrap().0;
            let restored = Session::new(Project::from_json(&saved).unwrap());
            paris(&restored.map().layer(id).unwrap().load_all().unwrap());
        }
    }
    let mut s = Session::new(Project::default());
    assert!(s.run("layer.addData", &json!({"path": dir.join("parcels.shp"), "crs": "not a CRS"})).is_err());
    assert!(s.map().layers.is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
