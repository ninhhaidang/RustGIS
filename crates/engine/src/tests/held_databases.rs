//! Database tables and GeoPackage edit sessions held in their own coordinate system (#2032
//! C2f-2): read as the table holds it, and edits written back into it, without a round trip
//! through longitude/latitude.

use crate::{Project, Session};
use serde_json::json;

/// A table in LV95 is loaded for editing held in LV95, with the table's coordinates exactly; an
/// attribute edit leaves its geometry as it was, to the bit, and a feature added lands in LV95
/// where `cs2cs` puts it. A layer held in longitude/latitude, as a project saved by an older
/// release holds one, still saves through the server's conversion, leaving the geometry of a row
/// whose name changed untouched. `RUSTGIS_TEST_POSTGIS` names a database (`postgresql://…`)
/// where the test may make and drop a table of its own.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "needs a live PostGIS database with table creation rights in RUSTGIS_TEST_POSTGIS"]
fn postgis_lv95_live() {
    /// Drops the test's table, however the test ends.
    struct Cleanup(String, String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Ok(mut c) = postgres::Client::connect(&self.0, postgres::NoTls) {
                let _ = c.batch_execute(&format!("DROP TABLE IF EXISTS public.{}", self.1));
            }
        }
    }
    let Ok(url) = std::env::var("RUSTGIS_TEST_POSTGIS") else { return };
    let url = url.trim_start_matches("postgres:").to_string();
    let mut db = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let table = format!("rustgis_lv95_test_{}", std::process::id());
    let _cleanup = Cleanup(url.clone(), table.clone());
    db.batch_execute(&format!(
        "CREATE TABLE public.{table} (id int PRIMARY KEY, name text, geom geometry(Point, 2056));
         INSERT INTO public.{table} VALUES (1, 'Bern', ST_SetSRID(ST_MakePoint(2600000.123, 1200000.456), 2056)),
           (2, 'Thun', ST_SetSRID(ST_MakePoint(2614000.5, 1178000.25), 2056));"
    ))
    .unwrap();
    let text = |db: &mut postgres::Client, id: i32| -> String {
        db.query_one(format!("SELECT ST_AsText(geom) || ' ' || ST_SRID(geom) FROM public.{table} WHERE id = $1").as_str(), &[&id])
            .unwrap()
            .get(0)
    };
    let before = text(&mut db, 1);

    let mut s = Session::new(Project::default());
    s.run("pg.connect", &json!({"connection": url})).unwrap();
    let r = s.run("pg.addLayer", &json!({"table": format!("public.{table}"), "name": "T"})).unwrap();
    let id = r["layers"][0].as_u64().unwrap() as u32;
    s.map_mut().editing.settings.enabled = true;
    let lv95 = rustgis_data::raster::Crs::from_epsg(2056).unwrap();
    {
        let l = s.map().layer(id).unwrap();
        assert_eq!(l.dataset.coord_crs(), lv95);
        let bern = l.dataset.features.iter().find(|f| f.id == 1).unwrap();
        assert_eq!(bern.geometry, Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(2_600_000.123, 1_200_000.456))));
    }
    let row_of = |s: &Session, key: u64| s.map().layer(id).unwrap().dataset.features.iter().position(|f| f.id == key).unwrap();
    s.run("edit.setValue", &json!({"layer": "T", "row": row_of(&s, 1), "field": "name", "value": "Bärn"})).unwrap();
    // Thun in WGS 84, as `cs2cs EPSG:4326 EPSG:2056` places it: 2614000.0 1178000.0 (to 1 mm).
    s.run(
        "edit.addFeature",
        &json!({"layer": "T", "geometry": {"type": "Point", "coordinates": [7.6280021, 46.7536016]}, "attributes": {"id": 3, "name": "new"}}),
    )
    .unwrap();
    let r = s.run("pg.saveEdits", &json!({"layer": "T"})).unwrap();
    assert_eq!((r["inserted"].as_u64(), r["updated"].as_u64()), (Some(1), Some(1)), "{r}");
    assert_eq!(text(&mut db, 1), before, "an attribute edit leaves the geometry as the table held it");
    let (x, y, srid): (f64, f64, i32) = {
        let row =
            db.query_one(format!("SELECT ST_X(geom), ST_Y(geom), ST_SRID(geom) FROM public.{table} WHERE id = 3").as_str(), &[]).unwrap();
        (row.get(0), row.get(1), row.get(2))
    };
    assert_eq!(srid, 2056);
    let want = rustgis_data::raster::Crs::LonLat
        .to(lv95)
        .unwrap()
        .geometry(&rustgis_data::Geometry::Point(rustgis_geom::Coord::new(7.6280021, 46.7536016)));
    let rustgis_data::Geometry::Point(want) = want else { panic!() };
    assert!((x - want.x).abs() < 1e-6 && (y - want.y).abs() < 1e-6, "{x} {y} vs {want:?}");

    // Held in longitude/latitude, as a project saved before RustGIS kept a table's own
    // coordinates holds it: saved through the server's conversion, as before.
    s.run("pg.discardEdits", &json!({"layer": "T"})).unwrap();
    {
        // Its geometry as the server converted it then.
        let l = s.map_mut().layer_mut(id).unwrap();
        let mut rows: Vec<_> = l.dataset.features.iter().cloned().collect();
        for f in &mut rows {
            let wkb: Vec<u8> = db
                .query_one(
                    format!("SELECT ST_AsBinary(ST_Transform(geom, 4326)) FROM public.{table} WHERE id = $1").as_str(),
                    &[&(f.id as i32)],
                )
                .unwrap()
                .get(0);
            f.geometry = rustgis_data::wkb::decode(&wkb);
        }
        l.dataset.features = rows.into();
        l.dataset.held_in = rustgis_data::raster::Crs::LonLat;
        l.touch_geometry();
    }
    let baseline = s.map().layer(id).unwrap().dataset.features.clone();
    if let Some(pg) = s.map_mut().layer_mut(id).unwrap().postgis.as_mut() {
        pg.versions = rustgis_map::source::versions_of(&baseline, &pg.versions, &Default::default());
    }
    let before = text(&mut db, 2);
    s.run("edit.setValue", &json!({"layer": "T", "row": row_of(&s, 2), "field": "name", "value": "Thoune"})).unwrap();
    let r = s.run("pg.saveEdits", &json!({"layer": "T"})).unwrap();
    assert_eq!(r["updated"].as_u64(), Some(1), "{r}");
    assert_eq!(text(&mut db, 2), before, "the server's lon/lat compares equal, so the geometry isn't rewritten");
    let name: String = db.query_one(format!("SELECT name FROM public.{table} WHERE id = 2").as_str(), &[]).unwrap().get(0);
    assert_eq!(name, "Thoune");

    // Held in a third system (UTM 32N): converted straight to LV95 and back, so an attribute
    // edit leaves the stored geometry as it is, byte for byte, and a moved point lands where
    // LV95 puts it.
    s.run("pg.discardEdits", &json!({"layer": "T"})).unwrap();
    let utm = rustgis_data::raster::Crs::from_epsg(32632).unwrap();
    {
        let l = s.map_mut().layer_mut(id).unwrap();
        l.dataset = rustgis_data::io::project(&l.dataset, utm).unwrap();
        l.touch_geometry();
    }
    let baseline = s.map().layer(id).unwrap().dataset.features.clone();
    if let Some(pg) = s.map_mut().layer_mut(id).unwrap().postgis.as_mut() {
        pg.versions = rustgis_map::source::versions_of(&baseline, &pg.versions, &Default::default());
    }
    let bytes = |db: &mut postgres::Client, id: i32| -> String {
        db.query_one(
            format!("SELECT encode(ST_AsBinary(geom), 'hex') || ' ' || ST_SRID(geom) FROM public.{table} WHERE id = $1").as_str(),
            &[&id],
        )
        .unwrap()
        .get(0)
    };
    let before = bytes(&mut db, 1);
    s.run("edit.setValue", &json!({"layer": "T", "row": row_of(&s, 1), "field": "name", "value": "Berne"})).unwrap();
    let r = s.run("pg.saveEdits", &json!({"layer": "T"})).unwrap();
    assert_eq!(r["updated"].as_u64(), Some(1), "{r}");
    assert_eq!(bytes(&mut db, 1), before, "an attribute edit leaves the geometry as the table holds it");
    assert_eq!(s.map().layer(id).unwrap().dataset.coord_crs(), utm);
}

/// A GeoPackage table in LV95 loaded for editing is held in LV95 with its stored coordinates; an
/// attribute edit leaves the stored geometry as it was, byte for byte, and a feature added lands
/// where `cs2cs` puts it. A layer held in longitude/latitude, as a project saved by an older
/// release holds one, still saves, leaving the geometry of a row whose name changed untouched.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_geopackage_edit_session_works_in_the_tables_own_system() {
    use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value, raster::Crs};
    let dir = std::env::temp_dir().join(format!("rustgis-held-gpkg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("towns.gpkg");
    let lv95 = Crs::from_epsg(2056).unwrap();
    let mut ds = Dataset::new("towns", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    ds.push(Some(Geometry::Point(Coord::new(2_600_000.123, 1_200_000.456))), vec![Value::Text("Bern".into())]);
    ds.push(Some(Geometry::Point(Coord::new(2_614_000.5, 1_178_000.25))), vec![Value::Text("Thun".into())]);
    ds.crs = Some(lv95);
    ds.held_in = lv95;
    rustgis_data::io::write_path_in(&ds, &path, lv95).unwrap();
    let blob = |fid: i64| -> Vec<u8> {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.query_row("SELECT geom FROM towns WHERE fid = ?1", [fid], |r| r.get(0)).unwrap()
    };
    let (bern, thun) = (blob(1), blob(2));

    let mut s = Session::new(Project::default());
    let loaded = s.run("gpkg.addLayer", &json!({"path": path, "table": "towns", "name": "T"})).unwrap();
    let id = loaded["layer"].as_u64().unwrap() as u32;
    s.map_mut().editing.settings.enabled = true;
    {
        let l = s.map().layer(id).unwrap();
        assert_eq!(l.dataset.coord_crs(), lv95);
        assert_eq!(l.dataset.features.iter().next().unwrap().geometry, Some(Geometry::Point(Coord::new(2_600_000.123, 1_200_000.456))));
    }
    let row_of = |s: &Session, key: u64| s.map().layer(id).unwrap().dataset.features.iter().position(|f| f.id == key).unwrap();
    s.run("edit.setValue", &json!({"layer": "T", "row": row_of(&s, 1), "field": "name", "value": "Bärn"})).unwrap();
    s.run(
        "edit.addFeature",
        &json!({"layer": "T", "geometry": {"type": "Point", "coordinates": [7.6280021, 46.7536016]}, "attributes": {"name": "new"}}),
    )
    .unwrap();
    let r = s.run("gpkg.saveEdits", &json!({"layer": "T"})).unwrap();
    assert_eq!(r["saved"], true, "{r}");
    assert_eq!(blob(1), bern, "an attribute edit leaves the stored geometry as it was");
    let (back, _) = rustgis_data::io::read_path(&path).unwrap();
    let added = back.features.iter().find(|f| f.attrs[0] == Value::Text("new".into())).unwrap();
    let want = Crs::LonLat.to(lv95).unwrap().geometry(&Geometry::Point(Coord::new(7.6280021, 46.7536016)));
    let (Some(Geometry::Point(a)), Geometry::Point(w)) = (added.geometry.clone(), want) else { panic!() };
    assert!((a.x - w.x).abs() < 1e-6 && (a.y - w.y).abs() < 1e-6, "{a:?} vs {w:?}");
    assert_eq!(s.map().layer(id).unwrap().dataset.coord_crs(), lv95);

    // Saved and opened again: the project says it keeps native coordinates, and the layer, held
    // in LV95 as read back, saves its next edit in LV95.
    let (text, _) = s.project.saved_text(None).unwrap();
    assert!(text.contains("native-coordinates"), "the project requires native coordinates");
    let mut s = Session::new(Project::from_json(&text).unwrap());
    let id = s.map().layers.iter().find(|l| l.name == "T").unwrap().id;
    s.map_mut().editing.settings.enabled = true;
    assert_eq!(s.map().layer(id).unwrap().dataset.coord_crs(), lv95);
    s.run("edit.setValue", &json!({"layer": "T", "row": row_of(&s, 1), "field": "name", "value": "Berne"})).unwrap();
    let r = s.run("gpkg.saveEdits", &json!({"layer": "T"})).unwrap();
    assert_eq!(r["saved"], true, "{r}");
    assert_eq!(blob(1), bern, "still the stored geometry");

    // Held in longitude/latitude, as a project saved before RustGIS kept a table's own
    // coordinates holds it: saved as before.
    {
        let l = s.map_mut().layer_mut(id).unwrap();
        l.dataset = rustgis_data::io::project(&l.dataset, Crs::LonLat).unwrap();
        l.touch_geometry();
        let features: Vec<_> = l.dataset.features.iter().cloned().collect();
        if let Some(g) = l.gpkg.as_mut() {
            g.versions = rustgis_map::source::versions_of(&features, &g.versions, &Default::default());
        }
    }
    s.run("edit.setValue", &json!({"layer": "T", "row": row_of(&s, 2), "field": "name", "value": "Thoune"})).unwrap();
    let r = s.run("gpkg.saveEdits", &json!({"layer": "T"})).unwrap();
    assert_eq!(r["saved"], true, "{r}");
    assert_eq!(blob(2), thun, "the geometry of a renamed row isn't rewritten");
    assert_eq!(s.map().layer(id).unwrap().dataset.coord_crs(), Crs::LonLat);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A GeoPackage layer held in a system that is neither the table's nor longitude/latitude saves
/// straight from it, and the rows read back come in it too, so two attribute saves leave the
/// stored geometry and the layer's as they were, byte for byte.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_geopackage_layer_held_in_a_third_system_saves_twice_in_place() {
    use rustgis_data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value, raster::Crs};
    let dir = std::env::temp_dir().join(format!("rustgis-held-gpkg3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("towns.gpkg");
    let lv95 = Crs::from_epsg(2056).unwrap();
    let utm = Crs::from_epsg(32632).unwrap();
    let mut ds = Dataset::new("towns", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    ds.push(Some(Geometry::Point(Coord::new(2_600_000.123, 1_200_000.456))), vec![Value::Text("Bern".into())]);
    ds.crs = Some(lv95);
    ds.held_in = lv95;
    rustgis_data::io::write_path_in(&ds, &path, lv95).unwrap();
    let mut s = Session::new(Project::default());
    let loaded = s.run("gpkg.addLayer", &json!({"path": path, "table": "towns", "name": "T"})).unwrap();
    let id = loaded["layer"].as_u64().unwrap() as u32;
    s.map_mut().editing.settings.enabled = true;
    let in_utm = {
        let l = s.map_mut().layer_mut(id).unwrap();
        l.dataset = rustgis_data::io::project(&l.dataset, utm).unwrap();
        l.touch_geometry();
        let features: Vec<_> = l.dataset.features.iter().cloned().collect();
        if let Some(g) = l.gpkg.as_mut() {
            g.versions = rustgis_map::source::versions_of(&features, &g.versions, &Default::default());
        }
        let Some(Geometry::Point(c)) = features[0].geometry.clone() else { panic!() };
        c
    };
    let blob = || -> Vec<u8> {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.query_row("SELECT geom FROM towns WHERE fid = 1", [], |r| r.get(0)).unwrap()
    };
    let bern = blob();
    // Converted straight from LV95 and back, as the layer was: an attribute edit leaves the
    // stored geometry as it is, byte for byte, and the layer keeps its point.
    for name in ["Bärn", "Berne"] {
        s.run("edit.setValue", &json!({"layer": "T", "row": 0, "field": "name", "value": name})).unwrap();
        let r = s.run("gpkg.saveEdits", &json!({"layer": "T"})).unwrap();
        assert_eq!(r["saved"], true, "{r}");
        let l = s.map().layer(id).unwrap();
        assert_eq!(l.dataset.coord_crs(), utm);
        assert_eq!(l.dataset.features.iter().next().unwrap().geometry, Some(Geometry::Point(in_utm)), "{name}");
        assert_eq!(blob(), bern, "{name}: the table keeps it as stored");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A column of no fixed coordinate system whose rows say none (SRID 0), loaded with `"crs"`, is
/// held in that system: read as stored, and edits written back as stored, still with SRID 0.
/// `RUSTGIS_TEST_POSTGIS` names a database (`postgresql://…`) where the test may make and drop a
/// table of its own.
#[cfg(not(target_arch = "wasm32"))]
#[test]
#[ignore = "needs a live PostGIS database with table creation rights in RUSTGIS_TEST_POSTGIS"]
fn postgis_srid_zero_with_a_crs_live() {
    /// Drops the test's table, however the test ends.
    struct Cleanup(String, String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Ok(mut c) = postgres::Client::connect(&self.0, postgres::NoTls) {
                let _ = c.batch_execute(&format!("DROP TABLE IF EXISTS public.{}", self.1));
            }
        }
    }
    let Ok(url) = std::env::var("RUSTGIS_TEST_POSTGIS") else { return };
    let url = url.trim_start_matches("postgres:").to_string();
    let mut db = postgres::Client::connect(&url, postgres::NoTls).unwrap();
    let table = format!("rustgis_srid0_test_{}", std::process::id());
    let _cleanup = Cleanup(url.clone(), table.clone());
    db.batch_execute(&format!(
        "CREATE TABLE public.{table} (id int PRIMARY KEY, name text, geom geometry);
         INSERT INTO public.{table} VALUES (1, 'Bern', ST_MakePoint(2600000.123, 1200000.456));"
    ))
    .unwrap();
    let mut s = Session::new(Project::default());
    s.run("pg.connect", &json!({"connection": url})).unwrap();
    let r = s.run("pg.addLayer", &json!({"table": format!("public.{table}"), "name": "Z", "crs": "EPSG:2056"})).unwrap();
    let id = r["layers"][0].as_u64().unwrap() as u32;
    s.map_mut().editing.settings.enabled = true;
    let lv95 = rustgis_data::raster::Crs::from_epsg(2056).unwrap();
    let l = s.map().layer(id).unwrap();
    assert_eq!(l.dataset.coord_crs(), lv95);
    assert_eq!(
        l.dataset.features.iter().next().unwrap().geometry,
        Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(2_600_000.123, 1_200_000.456)))
    );
    s.run(
        "edit.addFeature",
        &json!({"layer": "Z", "geometry": {"type": "Point", "coordinates": [7.6280021, 46.7536016]}, "attributes": {"id": 2, "name": "new"}}),
    )
    .unwrap();
    let r = s.run("pg.saveEdits", &json!({"layer": "Z"})).unwrap();
    assert_eq!(r["inserted"].as_u64(), Some(1), "{r}");
    let row = db.query_one(format!("SELECT ST_X(geom), ST_Y(geom), ST_SRID(geom) FROM public.{table} WHERE id = 2").as_str(), &[]).unwrap();
    let (x, y, srid): (f64, f64, i32) = (row.get(0), row.get(1), row.get(2));
    assert_eq!(srid, 0, "the column's rows keep saying none");
    let want = rustgis_data::raster::Crs::LonLat
        .to(lv95)
        .unwrap()
        .geometry(&rustgis_data::Geometry::Point(rustgis_geom::Coord::new(7.6280021, 46.7536016)));
    let rustgis_data::Geometry::Point(want) = want else { panic!() };
    assert!((x - want.x).abs() < 1e-6 && (y - want.y).abs() < 1e-6, "{x} {y} vs {want:?}");
}
