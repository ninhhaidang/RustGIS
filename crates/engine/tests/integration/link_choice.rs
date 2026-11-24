//! Add Data asks whether to copy a file into the project or link to it (#2350): `info.fileLayers`
//! says, when asked (`"link": true`), for a file Add Data can link, which it would do without
//! being told, and whether it can copy it at all. What it says is what `layer.addData` does
//! without `"stream"`. Unasked, it says nothing more than before.

use crate::portable_projects::common::{Fixture, gpkg_file, session};
use rustgis_engine::Session;
use rustgis_engine::data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value as V};
use serde_json::{Value, json};
use std::path::Path;

/// `n` points with a name each.
fn points(n: usize) -> Dataset {
    let mut ds = Dataset::new("points", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    for i in 0..n {
        let x = (i % 1000) as f64 * 0.01;
        let y = (i / 1000) as f64 * 0.01;
        ds.push(Some(Geometry::Point(Coord::new(x, y))), vec![V::Text(format!("p{i}"))]);
    }
    ds
}

fn listed(s: &mut Session, path: &Path) -> Value {
    s.run("info.fileLayers", &json!({"path": path, "link": true})).unwrap()
}

/// Whether `layer.addData` without `"stream"` keeps the file linked (read in place).
fn added_linked(path: &Path) -> bool {
    let mut s = session();
    let id = s.run("layer.addData", &json!({"path": path})).unwrap()["layer"].as_u64().unwrap() as u32;
    s.map().layer(id).unwrap().is_streamed()
}

#[test]
fn a_linkable_file_says_what_add_data_does_without_being_told() {
    let f = Fixture::new("link-choice");
    let small = points(10);
    let large = points(rustgis_engine::map::source::STREAM_ABOVE as usize + 1);
    let mut s = session();
    for ext in ["geojson", "shp", "fgb", "parquet", "gpkg"] {
        for (size, ds) in [("small", &small), ("large", &large)] {
            let path = f.file(&format!("{size}/points.{ext}"));
            rustgis_engine::data::io::write_path(ds, &path).unwrap();
            if ext == "geojson" && size == "large" {
                // A GeoJSON file is linked by its size: past it, with spaces after the document.
                use std::io::Write;
                let past = rustgis_engine::data::source::geojson::IN_PLACE_ABOVE + 1;
                let len = std::fs::metadata(&path).unwrap().len();
                let pad = vec![b' '; past.saturating_sub(len) as usize];
                std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&pad).unwrap();
            }
            let v = listed(&mut s, &path);
            let link = &v["link"];
            assert_eq!(link["linked"], size == "large", "{ext} {size}: {v}");
            assert_eq!(link["copy"], true, "{ext} {size}: {v}");
            assert_eq!(added_linked(&path), size == "large", "{ext} {size}: layer.addData disagrees with {v}");
        }
    }
}

#[test]
fn other_files_are_not_asked_about() {
    let f = Fixture::new("link-none");
    let ds = points(3);
    let mut s = session();
    // A .json may be a city model and is decided by size as before; the others are always read
    // whole, so the project holds them.
    for ext in ["json", "csv", "kml", "gpx"] {
        let path = f.file(&format!("points.{ext}"));
        rustgis_engine::data::io::write_path(&ds, &path).unwrap();
        let v = listed(&mut s, &path);
        assert!(v.get("link").is_none(), "{ext}: {v}");
    }
}

#[test]
fn a_file_too_large_to_read_whole_can_only_be_linked() {
    let f = Fixture::new("link-only");
    let ds = points(200);
    let mut s = session();
    for ext in ["geojson", "shp"] {
        let path = f.file(&format!("points.{ext}"));
        rustgis_engine::data::io::write_path(&ds, &path).unwrap();
        let v = rustgis_engine::data::io::with_max_bytes(1024, || listed(&mut s, &path));
        assert_eq!(v["link"]["copy"], false, "{ext}: {v}");
        assert_eq!(listed(&mut s, &path)["link"]["copy"], true, "{ext}");
    }
}

#[test]
fn a_shapefile_with_an_upper_case_table_can_be_copied() {
    let f = Fixture::new("link-dbf");
    let path = f.file("points.shp");
    rustgis_engine::data::io::write_path(&points(3), &path).unwrap();
    std::fs::rename(path.with_extension("dbf"), path.with_extension("DBF")).unwrap();
    let mut s = session();
    assert_eq!(listed(&mut s, &path)["link"], json!({"linked": false, "copy": true}));
}

#[test]
fn a_geopackage_of_several_tables_has_one_choice_for_them_all() {
    let f = Fixture::new("link-gpkg");
    let path = f.file("survey.gpkg");
    gpkg_file(&path, &["roads", "wells"]);
    let mut s = session();
    let v = listed(&mut s, &path);
    assert_eq!(v["count"], 2, "{v}");
    assert_eq!(v["link"], json!({"linked": false, "copy": true}), "{v}");
    // Unasked (List Tables, Repair Data Source): no tables opened, nothing more said.
    let plain = s.run("info.fileLayers", &json!({"path": path})).unwrap();
    assert!(plain.get("link").is_none(), "{plain}");
}

#[test]
fn a_geojson_file_read_whole_is_copied_whatever_its_count() {
    // Up to its size it is read whole, so many features still give a copy, as the question says.
    let f = Fixture::new("link-geojson-count");
    let path = f.file("many.geojson");
    rustgis_engine::data::io::write_path(&points(rustgis_engine::map::source::STREAM_ABOVE as usize + 1), &path).unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() <= rustgis_engine::data::source::geojson::IN_PLACE_ABOVE);
    let mut s = session();
    assert_eq!(listed(&mut s, &path)["link"]["linked"], false);
    assert!(!added_linked(&path), "read whole: copied");
}

#[test]
fn a_listing_stopped_between_tables_asks_nothing() {
    // Cancel is looked at between a GeoPackage's tables: once it is pressed, the tables left
    // aren't opened and counted, and no question is made of what was read.
    let f = Fixture::new("link-stop");
    let path = f.file("survey.gpkg");
    gpkg_file(&path, &["roads", "wells"]);
    let mut s = session();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let _stopped = rustgis_engine::data::progress::also_stop_when(stop);
    let v = listed(&mut s, &path);
    assert!(v.get("link").is_none(), "{v}");
}
