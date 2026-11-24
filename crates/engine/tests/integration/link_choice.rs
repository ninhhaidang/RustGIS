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

/// Sets the count GDAL keeps for `table` in `gpkg_ogr_contents`, with the triggers it keeps it by.
fn keep_count(path: &Path, table: &str, count: u64) {
    rusqlite::Connection::open(path)
        .unwrap()
        .execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS gpkg_ogr_contents (table_name TEXT NOT NULL PRIMARY KEY, feature_count INTEGER DEFAULT NULL);
             INSERT OR REPLACE INTO gpkg_ogr_contents VALUES ('{table}', {count});
             CREATE TRIGGER \"trigger_insert_feature_count_{table}\" AFTER INSERT ON \"{table}\" BEGIN UPDATE gpkg_ogr_contents
               SET feature_count = feature_count + 1 WHERE lower(table_name) = lower('{table}'); END;
             CREATE TRIGGER \"trigger_delete_feature_count_{table}\" AFTER DELETE ON \"{table}\" BEGIN UPDATE gpkg_ogr_contents
               SET feature_count = feature_count - 1 WHERE lower(table_name) = lower('{table}'); END;"
        ))
        .unwrap();
}

#[test]
fn a_geopackage_is_linked_by_the_count_gdal_keeps() {
    // One row in the file, but GDAL's count says it is past what is read whole: the question
    // and `layer.addData` both go by that count, without counting the rows (#2849).
    let f = Fixture::new("link-gpkg-kept");
    let path = f.file("survey.gpkg");
    gpkg_file(&path, &["roads", "wells"]);
    keep_count(&path, "wells", rustgis_engine::map::source::STREAM_ABOVE + 1);
    let mut s = session();
    assert_eq!(listed(&mut s, &path)["link"], json!({"linked": true, "copy": true}));
    let mut s = session();
    let id = s.run("layer.addData", &json!({"path": path, "layer": "wells"})).unwrap()["layer"].as_u64().unwrap() as u32;
    assert!(s.map().layer(id).unwrap().is_streamed(), "layer.addData reads it in place too");
}

#[test]
fn a_geopackage_stops_at_its_first_table_read_in_place() {
    // Tables are listed by name. One read in place links them all, so those after it aren't
    // opened: a table that can't be opened after it no longer leaves the file without a
    // question (`layer.addData` says why when it is added); before it, it still does.
    let f = Fixture::new("link-gpkg-first");
    let broken = |path: &Path, table: &str| {
        // A table whose geometry column names no coordinate system can't be opened.
        rusqlite::Connection::open(path)
            .unwrap()
            .execute(&format!("UPDATE gpkg_geometry_columns SET srs_id = NULL WHERE table_name = '{table}'"), [])
            .unwrap();
    };
    let mut s = session();
    let after = f.file("after.gpkg");
    gpkg_file(&after, &["a_large", "b_broken"]);
    keep_count(&after, "a_large", rustgis_engine::map::source::STREAM_ABOVE + 1);
    broken(&after, "b_broken");
    assert_eq!(listed(&mut s, &after)["link"], json!({"linked": true, "copy": true}));
    // Adding it is where the broken table is said.
    assert!(s.run("layer.addData", &json!({"path": after, "layer": "b_broken", "stream": true})).is_err());
    let before = f.file("before.gpkg");
    gpkg_file(&before, &["a_broken", "b_large"]);
    keep_count(&before, "b_large", rustgis_engine::map::source::STREAM_ABOVE + 1);
    broken(&before, "a_broken");
    let v = listed(&mut s, &before);
    assert!(v.get("link").is_none(), "{v}");
    // The last table read in place links them all too.
    let last = f.file("last.gpkg");
    gpkg_file(&last, &["a_small", "b_small", "c_large"]);
    keep_count(&last, "c_large", rustgis_engine::map::source::STREAM_ABOVE + 1);
    assert_eq!(listed(&mut s, &last)["link"], json!({"linked": true, "copy": true}));
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

#[test]
fn a_geopackage_whose_kept_counts_add_past_the_largest_total_says_the_largest() {
    // A file may keep any count: three tables at the largest one add past what a total holds,
    // so `layer.addData` says the largest total rather than failing or wrapping (#2849), and a
    // table read whole after them adds to that total without wrapping it either.
    let f = Fixture::new("link-gpkg-huge");
    let path = f.file("survey.gpkg");
    gpkg_file(&path, &["a", "b", "c", "d"]);
    for table in ["a", "b", "c"] {
        keep_count(&path, table, i64::MAX as u64);
    }
    let mut s = session();
    let r = s.run("layer.addData", &json!({"path": path})).unwrap();
    assert_eq!((r["layers"].as_array().map(Vec::len), r["features"].as_u64()), (Some(4), Some(u64::MAX)), "{r}");
}

#[test]
fn a_listing_stopped_while_its_last_table_opens_asks_nothing() {
    // Cancel pressed after the last table's open began: that open finishes, and no question is
    // made of it, as for a table before it (#2849).
    struct AfterFirstLook(std::sync::atomic::AtomicU64);
    impl rustgis_engine::data::progress::Probe for AfterFirstLook {
        fn cancelled(&self) -> bool {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 0
        }
        fn begin(&self, _: u64) {}
        fn advance(&self, _: u64) {}
    }
    let f = Fixture::new("link-stop-last");
    let path = f.file("survey.gpkg");
    gpkg_file(&path, &["roads"]);
    let mut s = session();
    let probe = std::sync::Arc::new(AfterFirstLook(std::sync::atomic::AtomicU64::new(0)));
    let _stopped = rustgis_engine::data::progress::install(Some(probe.clone()));
    let v = listed(&mut s, &path);
    assert!(v.get("link").is_none(), "{v}");
    assert_eq!(probe.0.load(std::sync::atomic::Ordering::Relaxed), 2, "looked at before the open and after it");
}
