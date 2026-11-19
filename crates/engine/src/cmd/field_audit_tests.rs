//! Field capture round-trips (an audit of mobile offline capture and sync). The `#[ignore]`d tests
//! reproduce known bugs and fail today; run them with `--ignored`. The others pass, and guard
//! what was fixed.
//!
//! Field capture round-trips as a crew would run them: packages sent to phones, edited offline,
//! and brought back to the home project, sometimes by more than one phone, sometimes more than once.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::Session;

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Temp {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("rustgis-audit-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Temp(dir)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn home(temp: &Temp) -> Session {
    let mut s = Session::new(crate::sample::world_project());
    s.path = Some(temp.0.join("home.rgis"));
    s
}

/// Opens the package on a "phone": unpacked into a folder of its own, as `project.open` does.
fn phone(temp: &Temp, bytes: &[u8], name: &str) -> Session {
    let dir = temp.0.join(name);
    let text = unpack_into(bytes, &dir).unwrap();
    let mut s = Session::new(project_in(&text, &dir).unwrap());
    s.opened_package = Some(temp.0.join(format!("{name}.rgisfield")));
    s
}

/// Sends the phone's map back and tries to bring it back at home: the result and the notices.
fn try_bring_back(temp: &Temp, field: &mut Session, home: &mut Session, name: &str) -> (Result<Value, String>, Vec<String>) {
    let (_, back) = package(field, &Value::Null).unwrap();
    let path = temp.0.join(format!("{name}-back.rgisfield"));
    std::fs::write(&path, &back).unwrap();
    let mark = home.notice_mark();
    let done = home.run("offline.bringBack", &json!({"path": path.display().to_string()}));
    let said = home.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    (done, said)
}

/// [`try_bring_back`] for a return that is accepted.
fn bring_back(temp: &Temp, field: &mut Session, home: &mut Session, name: &str) -> (Value, Vec<String>) {
    let (done, said) = try_bring_back(temp, field, home, name);
    (done.unwrap(), said)
}

fn layer(s: &Session, name: &str) -> u32 {
    s.map().layers.iter().find(|l| l.name == name).map(|l| l.id).unwrap()
}

fn text_field(s: &Session, id: u32) -> String {
    s.map().layer(id).unwrap().dataset.fields.iter().find(|f| f.ty == rustgis_data::FieldType::Text).unwrap().name.clone()
}

fn value_of(s: &Session, id: u32, feature: u64, field: &str) -> Option<String> {
    let l = s.map().layer(id)?;
    let row = l.dataset.features.iter().position(|f| f.id == feature)?;
    Some(l.dataset.value(row, l.dataset.field_index(field)?).to_string())
}

fn count(s: &Session, id: u32) -> usize {
    s.map().layer(id).unwrap().dataset.features.len()
}

fn add_point(s: &mut Session, id: u32, field: &str, label: &str, at: [f64; 2]) -> u64 {
    s.run("edit.addFeature", &json!({"layer": id, "geometry": {"type": "Point", "coordinates": at}, "attributes": {field: label}}))
        .unwrap();
    s.map().layer(id).unwrap().dataset.features.last().unwrap().id
}

fn with_label(s: &Session, id: u32, field: &str, label: &str) -> usize {
    let l = s.map().layer(id).unwrap();
    let fi = l.dataset.field_index(field).unwrap();
    (0..l.dataset.features.len()).filter(|r| l.dataset.value(*r, fi).to_string() == label).count()
}

const AREA: [f64; 4] = [-20.0, -40.0, 60.0, 40.0];

/// One package, copied to two phones (emailed to a crew, AirDropped to a second device). Each fixes
/// a city and captures a point. A is brought back first. B's copy is of a state the area has left:
/// bringing it back would undo A's fix and delete A's point (B's copy lacks it), so it is refused
/// and nothing of B's comes home. A new package from the area's offline map is how B's work goes
/// on: its edits are redone on it and come back, beside A's.
#[test]
fn a_second_phone_with_the_same_package_is_refused_and_the_first_phones_work_stays() {
    let temp = Temp::new("two-phones");
    let mut home = home(&temp);
    let source = home.map().name.clone();
    let (_, out) = package(&mut home, &json!({"name": "Crew", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let mut b = phone(&temp, &out, "b");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let fixed = a.map().layer(cities).unwrap().dataset.features[0].id;
    let other = a.map().layer(cities).unwrap().dataset.features[1].id;
    a.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": name, "value": "Fixed by A"})).unwrap();
    add_point(&mut a, cities, &name, "Well from A", [10.0, 5.0]);
    b.run("edit.setValue", &json!({"layer": cities, "row": 1, "field": name, "value": "Fixed by B"})).unwrap();
    add_point(&mut b, cities, &name, "Well from B", [11.0, 6.0]);
    bring_back(&temp, &mut a, &mut home, "a");
    let hc = layer(&home, "Cities");
    let at_home = count(&home, hc);

    let (refused, said) = try_bring_back(&temp, &mut b, &mut home, "b");
    let error = refused.unwrap_err();
    assert!(error.contains("out of date") && error.contains("Crew"), "{error}");
    assert!(said.iter().all(|t| !t.contains("Brought the field's edits back")), "{said:?}");
    assert_eq!(value_of(&home, hc, fixed, &name).as_deref(), Some("Fixed by A"));
    assert_ne!(value_of(&home, hc, other, &name).as_deref(), Some("Fixed by B"));
    assert_eq!((with_label(&home, hc, &name, "Well from A"), with_label(&home, hc, &name, "Well from B")), (1, 0));
    assert_eq!(count(&home, hc), at_home);

    // B's work, redone on a package of the area as it is now, comes back beside A's.
    home.run("offline.open", &json!({"name": "Crew"})).unwrap();
    let (_, again) = package(&mut home, &Value::Null).unwrap();
    home.run("map.activate", &json!({"name": source})).unwrap();
    let mut b2 = phone(&temp, &again, "b2");
    b2.run("edit.setValue", &json!({"layer": cities, "row": 1, "field": name, "value": "Fixed by B"})).unwrap();
    add_point(&mut b2, cities, &name, "Well from B", [11.0, 6.0]);
    bring_back(&temp, &mut b2, &mut home, "b2");
    assert_eq!(value_of(&home, hc, fixed, &name).as_deref(), Some("Fixed by A"));
    assert_eq!(value_of(&home, hc, other, &name).as_deref(), Some("Fixed by B"));
    assert_eq!((with_label(&home, hc, &name, "Well from A"), with_label(&home, hc, &name, "Well from B")), (1, 1));
}

/// One phone, two trips with the same package: the crew brings back at lunch, keeps working, and
/// sends the same map again in the evening. The second return would add the first's new points a
/// second time (those it captured past the packaged extent come back as new every time, and an id
/// taken at home is given a new one), so it is refused; every feature of the first return is at
/// home once.
#[test]
fn a_second_return_of_one_package_is_refused_and_leaves_nothing_twice() {
    let temp = Temp::new("repeat");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    // One past the edge of the area, one inside.
    add_point(&mut a, cities, &name, "Past the edge", [65.0, 5.0]);
    add_point(&mut a, cities, &name, "Inside", [10.0, 5.0]);
    bring_back(&temp, &mut a, &mut home, "lunch");
    add_point(&mut a, cities, &name, "Afternoon", [11.0, 5.0]);
    let (second, said) = try_bring_back(&temp, &mut a, &mut home, "evening");
    assert!(second.unwrap_err().contains("out of date"), "{said:?}");
    let hc = layer(&home, "Cities");
    let at = |label: &str| with_label(&home, hc, &name, label);
    assert_eq!((at("Past the edge"), at("Inside"), at("Afternoon")), (1, 1, 0));
}

/// Desktop Sync of the area's offline map, between packaging and the return, moves the area on as
/// a Bring Back does: the phone's copy lacks what the office put in the map before it, so its return
/// would undo that.
#[test]
fn a_package_made_before_a_sync_of_its_area_is_refused() {
    let temp = Temp::new("synced");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    home.run("offline.sync", &json!({"name": "Day"})).unwrap();
    let (back, _) = try_bring_back(&temp, &mut a, &mut home, "a");
    assert!(back.unwrap_err().contains("out of date"));
}

/// A package made before packages carried a return check has none to compare: it is brought back as
/// it was, and the notice says it can't be told from a copy of one already returned.
#[test]
fn a_package_without_a_return_check_comes_back_with_a_notice() {
    let temp = Temp::new("no-check");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    a.project.field_origin.as_mut().unwrap().generation.clear();
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    let (done, said) = bring_back(&temp, &mut a, &mut home, "a");
    assert_eq!(done["added"].as_u64(), Some(1), "{done}");
    assert!(said.iter().any(|t| t.contains("return check") && t.contains("Day")), "{said:?}");
    // A package with one comes back without it.
    let (_, out) = package(&mut home, &json!({"name": "Clean", "extent": AREA})).unwrap();
    let mut b = phone(&temp, &out, "b");
    let (_, said) = bring_back(&temp, &mut b, &mut home, "b");
    assert!(said.iter().all(|t| !t.contains("return check")), "{said:?}");
}

/// Two crews, each with their own package of overlapping areas (the documented way), edit the
/// same city differently. The second one back wins; is the first crew's value said?
#[test]
fn audit_two_packages_same_feature_last_back_wins_and_is_said() {
    let temp = Temp::new("two-packages");
    let mut home = home(&temp);
    let (_, out_a) = package(&mut home, &json!({"name": "North", "extent": AREA})).unwrap();
    let (_, out_b) = package(&mut home, &json!({"name": "South", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out_a, "a");
    let mut b = phone(&temp, &out_b, "b");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let id = a.map().layer(cities).unwrap().dataset.features[0].id;
    a.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": name, "value": "A says"})).unwrap();
    b.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": name, "value": "B says"})).unwrap();
    let ida = add_point(&mut a, cities, &name, "New A", [10.0, 5.0]);
    let idb = add_point(&mut b, cities, &name, "New B", [11.0, 6.0]);
    eprintln!("phone ids: A {ida}, B {idb}");
    bring_back(&temp, &mut a, &mut home, "a");
    let (done, said) = bring_back(&temp, &mut b, &mut home, "b");
    eprintln!("B: {done}\nnotices: {said:#?}");
    let hc = layer(&home, "Cities");
    assert_eq!(value_of(&home, hc, id, &name).as_deref(), Some("B says"));
    assert_eq!((with_label(&home, hc, &name, "New A"), with_label(&home, hc, &name, "New B")), (1, 1));
    assert!(said.iter().any(|t| t.contains("Cities #")), "the overwritten edit should be named: {said:?}");
}

/// The office adds a column to the layer while the crew is out. The crew's edits come back.
#[test]
#[ignore = "repro of a known field-sync bug (mobile offline audit); run with --ignored"]
fn audit_office_adds_a_field_while_crew_is_out() {
    let temp = Temp::new("schema");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    let hc = layer(&home, "Cities");
    home.run("edit.addField", &json!({"layer": hc, "name": "inspected", "type": "text"})).unwrap();
    let (_, back) = package(&mut a, &Value::Null).unwrap();
    let path = temp.0.join("back.rgisfield");
    std::fs::write(&path, &back).unwrap();
    let r = home.run("offline.bringBack", &json!({"path": path.display().to_string()}));
    eprintln!("bring back after office schema change: {r:?}");
    assert!(r.is_ok(), "the whole day's edits were refused: {r:?}");
}

/// The crew deletes a point on the phone that the office edited meanwhile, and the office
/// deletes a point the crew edited.
#[test]
fn audit_delete_vs_edit_both_ways() {
    let temp = Temp::new("delete-edit");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let f0 = a.map().layer(cities).unwrap().dataset.features[0].id;
    let f1 = a.map().layer(cities).unwrap().dataset.features[1].id;
    // Phone: delete f0, edit f1.
    a.run("edit.setValue", &json!({"layer": cities, "row": 1, "field": name, "value": "Crew edit"})).unwrap();
    a.run("select.rows", &json!({"layer": cities, "rows": [0]})).unwrap();
    a.run("edit.delete", &Value::Null).unwrap();
    // Office: edit f0, delete f1.
    let hc = layer(&home, "Cities");
    let r0 = home.map().layer(hc).unwrap().dataset.features.iter().position(|f| f.id == f0).unwrap();
    home.run("edit.setValue", &json!({"layer": hc, "row": r0, "field": name, "value": "Office edit"})).unwrap();
    let r1 = home.map().layer(hc).unwrap().dataset.features.iter().position(|f| f.id == f1).unwrap();
    home.run("select.rows", &json!({"layer": hc, "rows": [r1]})).unwrap();
    home.run("edit.delete", &Value::Null).unwrap();
    let (done, said) = bring_back(&temp, &mut a, &mut home, "a");
    // The crew's deletion wins over the office's edit; the office's deletion wins over the crew's edit.
    assert_eq!((value_of(&home, hc, f0, &name), value_of(&home, hc, f1, &name)), (None, None), "{done}");
    assert_eq!((done["deleted"].as_u64(), done["skipped"].as_u64(), done["conflicts"].as_u64()), (Some(1), Some(1), Some(1)), "{done}");
    // Both losses are said, by feature.
    assert!(said.iter().any(|t| t.contains("replaced the map's changes") && t.contains(&format!("Cities #{f0}"))), "{said:?}");
    assert!(said.iter().any(|t| t.contains("edits were dropped") && t.contains(&format!("Cities #{f1}"))), "{said:?}");
}

#[cfg(not(target_arch = "wasm32"))]
fn roads_gpkg(dir: &Path) -> PathBuf {
    let path = dir.join("roads.gpkg");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE gpkg_spatial_ref_sys (srs_name TEXT, srs_id INTEGER PRIMARY KEY, organization TEXT,
             organization_coordsys_id INTEGER, definition TEXT, description TEXT);
         INSERT INTO gpkg_spatial_ref_sys VALUES ('WGS 84', 4326, 'EPSG', 4326, '', '');
         CREATE TABLE gpkg_contents (table_name TEXT PRIMARY KEY, data_type TEXT, identifier TEXT,
             description TEXT, last_change TEXT, min_x DOUBLE, min_y DOUBLE, max_x DOUBLE, max_y DOUBLE, srs_id INTEGER);
         INSERT INTO gpkg_contents (table_name, data_type, srs_id, last_change) VALUES ('roads', 'features', 4326, '2000-01-01T00:00:00.000Z');
         CREATE TABLE gpkg_geometry_columns (table_name TEXT, column_name TEXT, geometry_type_name TEXT,
             srs_id INTEGER, z TINYINT, m TINYINT);
         INSERT INTO gpkg_geometry_columns VALUES ('roads', 'geom', 'POINT', 4326, 0, 0);
         CREATE TABLE roads (fid INTEGER PRIMARY KEY, geom BLOB, name TEXT, lanes INTEGER DEFAULT 2);",
    )
    .unwrap();
    let blob = |x: f64, y: f64| {
        let mut b = b"GP\x00\x03".to_vec();
        b.extend_from_slice(&4326i32.to_le_bytes());
        for o in [x, x, y, y] {
            b.extend_from_slice(&o.to_le_bytes());
        }
        b.push(1);
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&x.to_le_bytes());
        b.extend_from_slice(&y.to_le_bytes());
        b
    };
    // Ten points, x = 1..=10: a packaged area of x 1..3 holds three.
    for k in 1..=10i64 {
        db.execute("INSERT INTO roads (fid, geom, name) VALUES (?1, ?2, ?3)", rusqlite::params![k, blob(k as f64, 20.0), format!("r{k}")])
            .unwrap();
    }
    path
}

/// The table's row count.
#[cfg(not(target_arch = "wasm32"))]
fn roads(gpkg: &Path) -> i64 {
    rusqlite::Connection::open(gpkg).unwrap().query_row("SELECT count(*) FROM roads", [], |r| r.get(0)).unwrap()
}

/// A pre-origin field archive retains full-table descriptors on its clipped copies. Opening
/// the same archive as a field or project package must disable both write-back paths.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn pre_origin_field_package_never_saves_to_its_home_tables() {
    use rustgis_map::source::{PostgisEdit, PostgisOrigin};

    let temp = Temp::new("pre-origin-extension");
    let gpkg = roads_gpkg(&temp.0);
    let mut home = Session::new(Project::new("Home"));
    home.run("gpkg.addLayer", &json!({"path": gpkg, "table": "roads", "name": "R"})).unwrap();
    let link = home.map().layers[0].gpkg.clone().unwrap();
    // Give the fixture both edit descriptors so the test needs only one local table.
    let postgis = PostgisEdit {
        origin: PostgisOrigin {
            database: "postgres:host=127.0.0.1 port=1 dbname=gis".into(),
            table: "roads".into(),
            family: None,
            crs: None,
        },
        key: "fid".into(),
        srid: 4326,
        writable: true,
        condition: None,
        limit: 0,
        versions: link.versions.clone(),
    };
    let (_, out) = package(&mut home, &json!({"name": "Site", "extent": [0.5, 19.0, 3.5, 21.0]})).unwrap();

    // Model the old writer's one-map project, without an origin and with the stale links.
    let mut legacy = phone(&temp, &out, "fixture").project;
    legacy.field_origin = None;
    legacy.maps[0].layers[0].gpkg = Some(link);
    legacy.maps[0].layers[0].postgis = Some(postgis);
    let text = serde_json::to_string(&legacy).unwrap();
    let raw: Value = serde_json::from_str(&text).unwrap();
    assert!(raw.get("field_origin").is_none() && raw.get("origin_id").is_none());
    let bytes = rustgis_data::zip::write(&[
        ("Site.rgis".into(), text.as_bytes().to_vec()),
        ("README.txt".into(), b"Site: a RustGIS field package.\n".to_vec()),
    ])
    .unwrap();

    let stem = temp.0.file_name().unwrap().to_str().unwrap();
    let _unpacked = Temp(unpack_dir(stem).unwrap());
    let field = temp.0.join(format!("{stem}.rgisfield"));
    let renamed = field.with_extension("rgispkg");
    std::fs::write(&field, &bytes).unwrap();
    std::fs::write(&renamed, &bytes).unwrap();
    let check = |command, params: Value| {
        let mut opened = Session::new(Project::default());
        let mark = opened.notice_mark();
        opened.run(command, &params).unwrap();
        let layer = &opened.map().layers[0];
        assert_eq!(layer.dataset.features.len(), 3);
        assert!(layer.gpkg.is_none());
        assert!(layer.postgis.is_none());
        let notices: Vec<_> = opened
            .notices_since(mark)
            .iter()
            .filter(|notice| notice.text.contains("doesn't save to the table"))
            .map(|notice| notice.text.clone())
            .collect();
        assert_eq!(notices.len(), 1);
        assert!(notices[0].contains("1 layer(s) of this field project"));
        for (cmd, provider) in [("gpkg.saveEdits", "GeoPackage"), ("pg.saveEdits", "PostGIS")] {
            let error = opened.run(cmd, &json!({"layer": "R"})).unwrap_err();
            assert!(error.contains(&format!("not a {provider} layer loaded for editing")), "{error}");
        }
        assert_eq!(roads(&gpkg), 10);
        notices
    };
    let expected = check("project.open", json!({"path": field}));
    assert_eq!(check("project.open", json!({"path": renamed})), expected);
    let text = crate::cmd::items::package_text(&bytes).unwrap();
    assert_eq!(check("project.openText", json!({"text": text, "package": true})), expected);
}

/// A full table stays editable from both a saved project and an ordinary project package,
/// even when its map's name ends in the same suffix as a field map.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn ordinary_projects_and_packages_keep_full_table_edit_links() {
    let temp = Temp::new("full-table");
    let gpkg = roads_gpkg(&temp.0);
    let mut home = Session::new(Project::new("Home"));
    home.map_mut().name = "Home (offline)".into();
    home.run("gpkg.addLayer", &json!({"path": gpkg, "table": "roads", "name": "R"})).unwrap();
    let project = temp.0.join("Home.rgis");
    home.run("project.saveAs", &json!({"path": project})).unwrap();
    // An offline suffix alone is not the signature of a field package.
    home.project.name = "Survey".into();
    let package = temp.0.join("Survey.rgispkg");
    home.run("project.package", &json!({"path": package})).unwrap();

    for path in [project, package] {
        let mut opened = Session::new(Project::default());
        let mark = opened.notice_mark();
        opened.run("project.open", &json!({"path": path})).unwrap();
        let layer = &opened.map().layers[0];
        assert_eq!(layer.dataset.features.len(), 10);
        let link = layer.gpkg.as_ref().unwrap();
        assert!(link.versions.changes(&layer.dataset.features).is_empty());
        assert!(!opened.notices_since(mark).iter().any(|notice| notice.text.contains("doesn't save to the table")));
    }
}

/// A GeoPackage layer loaded for editing goes to the field. Its copy holds only the roads in the
/// area: it mustn't save to the table, where the roads it lacks would be deleted. Its edits come
/// back by Bring Back, and the table is saved from the home map, whole.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_field_copy_of_a_geopackage_layer_never_saves_to_the_table() {
    let temp = Temp::new("gpkg");
    let gpkg = roads_gpkg(&temp.0);
    let mut home = Session::new(crate::project::Project::default());
    home.path = Some(temp.0.join("home.rgis"));
    home.run("gpkg.addLayer", &json!({"path": gpkg, "table": "roads", "name": "R"})).unwrap();
    let mark = home.notice_mark();
    let (_, out) = package(&mut home, &json!({"name": "Roads", "extent": [0.5, 19.0, 3.5, 21.0]})).unwrap();
    let said: Vec<String> = home.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(
        said.iter().any(|t| t.contains("1 layer(s) of the offline area 'Roads'") && t.contains("doesn't save to the table")),
        "{said:?}"
    );
    // The home map keeps its link.
    assert!(home.map().layer(layer(&home, "R")).unwrap().gpkg.is_some());
    let mut a = phone(&temp, &out, "a");
    let r = layer(&a, "R");
    assert_eq!(count(&a, r), 3);
    assert!(a.map().layer(r).unwrap().gpkg.is_none());
    a.map_mut().editing.settings.enabled = true;
    a.run("edit.setValue", &json!({"layer": r, "row": 0, "field": "name", "value": "fixed"})).unwrap();
    let e = a.run("gpkg.saveEdits", &json!({"layer": "R"})).unwrap_err();
    assert!(e.contains("not a GeoPackage layer loaded for editing"), "{e}");
    assert_eq!(roads(&gpkg), 10);
    // Back home, the edit is saved to the table, and only it.
    let (done, _) = bring_back(&temp, &mut a, &mut home, "a");
    assert_eq!((done["updated"].as_u64(), done["deleted"].as_u64()), (Some(1), Some(0)), "{done}");
    let saved = home.run("gpkg.saveEdits", &json!({"layer": "R"})).unwrap();
    assert_eq!((saved["updated"].as_u64(), saved["deleted"].as_u64()), (Some(1), Some(0)), "{saved}");
    assert_eq!(roads(&gpkg), 10);
}

/// No phone at all: Download Map, Open Offline Map, edit, Save to GeoPackage, on the desktop.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_offline_map_of_a_geopackage_layer_never_saves_to_the_table() {
    let temp = Temp::new("gpkg-desktop");
    let gpkg = roads_gpkg(&temp.0);
    let mut s = Session::new(crate::project::Project::default());
    s.run("gpkg.addLayer", &json!({"path": gpkg, "table": "roads", "name": "R"})).unwrap();
    s.run("offline.download", &json!({"name": "Site", "extent": [0.5, 19.0, 3.5, 21.0]})).unwrap();
    s.run("offline.open", &json!({"name": "Site"})).unwrap();
    assert_eq!(s.map().name, "Site (offline)");
    s.map_mut().editing.settings.enabled = true;
    let r = layer(&s, "R");
    s.run("edit.setValue", &json!({"layer": r, "row": 0, "field": "name", "value": "fixed"})).unwrap();
    assert!(s.run("gpkg.saveEdits", &json!({"layer": "R"})).is_err());
    assert_eq!(roads(&gpkg), 10);
}

/// An offline area saved before offline copies were unlinked still holds the link: opening its
/// map takes it away, and says so. A clean area says nothing.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_offline_area_saved_with_a_table_link_opens_without_it() {
    let temp = Temp::new("gpkg-legacy-area");
    let gpkg = roads_gpkg(&temp.0);
    let mut s = Session::new(crate::project::Project::default());
    s.run("gpkg.addLayer", &json!({"path": gpkg, "table": "roads", "name": "R"})).unwrap();
    s.run("offline.download", &json!({"name": "Clean", "extent": [0.5, 19.0, 3.5, 21.0]})).unwrap();
    s.run("offline.download", &json!({"name": "Old", "extent": [0.5, 19.0, 3.5, 21.0]})).unwrap();
    let link = s.map().layers[0].gpkg.clone();
    s.project.offline_areas[1].data.layers[0].gpkg = link;
    let mark = s.notice_mark();
    s.run("offline.open", &json!({"name": "Clean"})).unwrap();
    assert!(s.notices_since(mark).iter().all(|n| !n.text.contains("doesn't save to the table")));
    let mark = s.notice_mark();
    s.run("offline.open", &json!({"name": "Old"})).unwrap();
    assert!(s.map().layers.iter().all(|l| l.gpkg.is_none()));
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(said.iter().any(|t| t.contains("the offline map 'Old (offline)'")), "{said:?}");
    s.map_mut().editing.settings.enabled = true;
    assert!(s.run("gpkg.saveEdits", &json!({"layer": "R"})).is_err());
    assert_eq!(roads(&gpkg), 10);
}

/// A field project saved before offline copies were unlinked (from a field package, so it names
/// its home) opens without the link, and says so.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_field_project_saved_with_a_table_link_opens_without_it() {
    let temp = Temp::new("gpkg-legacy-field");
    let gpkg = roads_gpkg(&temp.0);
    let mut s = Session::new(crate::project::Project::default());
    s.run("gpkg.addLayer", &json!({"path": gpkg, "table": "roads", "name": "R"})).unwrap();
    s.project.field_origin = Some(crate::project::FieldOrigin {
        project: crate::project::field_id().unwrap(),
        area: crate::project::field_id().unwrap(),
        generation: String::new(),
    });
    let path = temp.0.join("field.rgis");
    s.project.save(&path).unwrap();
    let mut opened = Session::new(crate::project::Project::default());
    let mark = opened.notice_mark();
    opened.run("project.open", &json!({"path": path})).unwrap();
    assert!(opened.map().layers.iter().all(|l| l.gpkg.is_none()));
    let said: Vec<String> = opened.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(said.iter().any(|t| t.contains("this field project")), "{said:?}");
    // A home project keeps its link.
    s.project.field_origin = None;
    let home = temp.0.join("home.rgis");
    s.project.save(&home).unwrap();
    opened.run("project.open", &json!({"path": home})).unwrap();
    assert!(opened.map().layers[0].gpkg.is_some());
}

/// A home project saved, before offline copies were unlinked, with an offline area's map open:
/// that map loses its link when the project opens, and Open Offline Map, reusing it, keeps it
/// unlinked. The source map keeps its link.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_home_projects_offline_map_saved_with_a_table_link_opens_without_it() {
    let temp = Temp::new("gpkg-legacy-home");
    let gpkg = roads_gpkg(&temp.0);
    let mut s = Session::new(crate::project::Project::default());
    s.run("gpkg.addLayer", &json!({"path": gpkg, "table": "roads", "name": "R"})).unwrap();
    s.run("offline.download", &json!({"name": "Site", "extent": [0.5, 19.0, 3.5, 21.0]})).unwrap();
    s.run("offline.open", &json!({"name": "Site"})).unwrap();
    let link = s.project.maps[0].layers[0].gpkg.clone();
    let offline = s.project.active_map;
    s.project.maps[offline].layers[0].gpkg = link.clone();
    // Open Offline Map again, on the map already there.
    let mark = s.notice_mark();
    s.run("offline.open", &json!({"name": "Site"})).unwrap();
    assert!(s.map().layers.iter().all(|l| l.gpkg.is_none()));
    assert!(s.notices_since(mark).iter().any(|n| n.text.contains("the offline map 'Site (offline)'")));
    // Saved with the link, then opened.
    s.project.maps[offline].layers[0].gpkg = link;
    let path = temp.0.join("home.rgis");
    s.project.save(&path).unwrap();
    let mut opened = Session::new(crate::project::Project::default());
    let mark = opened.notice_mark();
    opened.run("project.open", &json!({"path": path})).unwrap();
    let maps = &opened.project.maps;
    assert!(maps.iter().find(|m| m.name == "Site (offline)").unwrap().layers.iter().all(|l| l.gpkg.is_none()));
    assert!(maps.iter().find(|m| m.name != "Site (offline)").unwrap().layers[0].gpkg.is_some());
    let said: Vec<String> = opened.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(said.iter().any(|t| t.contains("this project's offline maps")), "{said:?}");
    // The browser opens a project from its text: the same.
    let text = std::fs::read_to_string(&path).unwrap();
    let mut browser = Session::new(crate::project::Project::default());
    let mark = browser.notice_mark();
    browser.run("project.openText", &json!({"text": text})).unwrap();
    let maps = &browser.project.maps;
    assert!(maps.iter().find(|m| m.name == "Site (offline)").unwrap().layers.iter().all(|l| l.gpkg.is_none()));
    assert!(maps.iter().find(|m| m.name != "Site (offline)").unwrap().layers[0].gpkg.is_some());
    assert!(browser.notices_since(mark).iter().any(|n| n.text.contains("this project's offline maps")));
    opened.project.active_map = opened.project.maps.iter().position(|m| m.name == "Site (offline)").unwrap();
    opened.map_mut().editing.settings.enabled = true;
    assert!(opened.run("gpkg.saveEdits", &json!({"layer": "R"})).is_err());
    assert_eq!(roads(&gpkg), 10);
}
