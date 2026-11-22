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

/// Opens the package on a "phone": unpacked into a folder of its own, as `project.open` does, and
/// the rows it gave kept, as the app keeps them next.
fn phone(temp: &Temp, bytes: &[u8], name: &str) -> Session {
    let dir = temp.0.join(name);
    let (text, _) = unpack_into(bytes, &dir).unwrap();
    let mut s = Session::new(project_in(&text, &dir).unwrap());
    s.opened_package = Some(temp.0.join(format!("{name}.rgisfield")));
    s.run("offline.keepRows", &Value::Null).unwrap();
    assert!(!s.field_rows_waiting(), "kept, the rows are no longer held for keeping");
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

/// A map imported from another project under an offline title is renamed, so an area of the
/// same name here doesn't take it for its offline copy (and sync it as the area's edits).
#[test]
fn an_imported_offline_titled_map_is_not_taken_for_an_area_copy() {
    let temp = Temp::new("import-offline-title");
    let mut home = home(&temp);
    package(&mut home, &json!({"name": "Crew", "extent": AREA})).unwrap();
    let maps = home.project.maps.len();
    let mut other = crate::Project::default();
    // Another project's offline copy, saved before maps carried their area's ID.
    other.maps[0].name = "Crew (offline)".into();
    home.run("map.import", &json!({"text": serde_json::to_string(&other).unwrap()})).unwrap();
    assert!(home.project.maps.iter().any(|m| m.name == "Crew (imported)" && m.offline_area_id.is_none()));
    assert!(home.notices.iter().any(|n| n.text.contains("renamed")), "{:?}", home.notices);
    home.run("offline.open", &json!({"name": "Crew"})).unwrap();
    assert_eq!(home.map().name, "Crew (offline)");
    assert_eq!(home.project.maps.len(), maps + 2);
}

/// A map imported with its area's ID loses it, and says so; an ordinary map imports with neither
/// notice.
#[test]
fn importing_maps_says_when_an_area_association_is_dropped() {
    let temp = Temp::new("import-detach");
    let mut home = home(&temp);
    package(&mut home, &json!({"name": "Crew", "extent": AREA})).unwrap();
    home.run("offline.open", &json!({"name": "Crew"})).unwrap();
    let path = temp.0.join("with-area.rgis");
    home.run("project.save", &json!({"path": path})).unwrap();

    let mut other = Session::new(crate::Project::default());
    let mark = other.notice_mark();
    other.run("map.import", &json!({"path": path})).unwrap();
    let texts: Vec<_> = other.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(texts.iter().any(|t| t.starts_with("1 imported map(s) were detached")), "{texts:?}");
    assert!(other.project.maps.iter().all(|m| m.offline_area_id.is_none()));

    let mark = other.notice_mark();
    other.run("map.import", &json!({"text": serde_json::to_string(&crate::Project::default()).unwrap()})).unwrap();
    let texts: Vec<_> = other.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(!texts.iter().any(|t| t.contains("detached") || t.contains("renamed")), "{texts:?}");
}

/// Removing an area whose copy is the only map keeps the map but not its association, so the
/// project still saves and reopens.
#[test]
fn removing_an_area_clears_the_association_of_the_copy_it_keeps() {
    let temp = Temp::new("remove-only-copy");
    let mut home = home(&temp);
    package(&mut home, &json!({"name": "Crew", "extent": AREA})).unwrap();
    home.run("offline.open", &json!({"name": "Crew"})).unwrap();
    home.project.maps.retain(|m| m.offline_area_id.is_some());
    home.project.active_map = 0;
    home.run("offline.remove", &json!({"name": "Crew"})).unwrap();
    assert_eq!(home.project.maps.len(), 1);
    assert!(home.project.maps[0].offline_area_id.is_none());
    // Nor does its title still name the area's copy.
    assert_eq!(home.project.maps[0].name, "Crew (kept)");
    assert!(home.notices.iter().any(|n| n.text.contains("kept as 'Crew (kept)'")), "{:?}", home.notices);
    let path = temp.0.join("removed.rgis");
    home.run("project.save", &json!({"path": path})).unwrap();
    home.run("project.open", &json!({"path": path})).unwrap();
}

/// A legacy home tab gains its association on rename, so callers still find it after save and reopen.
#[test]
fn bring_back_refreshes_a_renamed_home_offline_map() {
    let temp = Temp::new("rename-home-offline");
    let mut home = home(&temp);
    let source = home.map().name.clone();
    let (_, out) = package(&mut home, &json!({"name": "Crew", "extent": AREA})).unwrap();
    home.run("offline.open", &json!({"name": "Crew"})).unwrap();
    // A home project saved before maps carried their area ID.
    home.map_mut().offline_area_id = None;
    home.run("map.rename", &json!({"name": "Renamed at home"})).unwrap();
    let path = temp.0.join("home.rgis");
    home.run("project.save", &json!({"path": path})).unwrap();
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(saved["requires"].as_array().unwrap().contains(&json!("offline-map-area-id")));
    home.run("project.open", &json!({"path": path})).unwrap();
    assert_eq!(home.map().name, "Renamed at home");

    let mut field = phone(&temp, &out, "phone");
    let cities = layer(&field, "Cities");
    let name = text_field(&field, cities);
    let count = |s: &Session, label| with_label(s, layer(s, "Cities"), &name, label);
    add_point(&mut field, cities, &name, "Captured after rename", [10.0, 5.0]);
    let (done, _) = bring_back(&temp, &mut field, &mut home, "phone");
    assert_eq!(done["area"], "Crew");
    assert_eq!(done["added"], 1);
    assert_eq!(count(&home, "Captured after rename"), 1);

    home.run("map.activate", &json!({"name": source})).unwrap();
    assert_eq!(count(&home, "Captured after rename"), 1);
    home.run("offline.open", &json!({"name": "Crew"})).unwrap();
    assert_eq!(home.map().name, "Renamed at home");
    assert_eq!(home.project.maps.len(), 2);

    // Reusing the old title for another map must not redirect Sync, Package or Remove.
    home.run("map.new", &json!({"name": "Crew (offline)"})).unwrap();
    home.run("offline.open", &json!({"name": "Crew"})).unwrap();
    assert_eq!(home.map().name, "Renamed at home");
    add_point(&mut home, cities, &name, "Edited at home", [11.0, 5.0]);
    // Packaged once its edit is synced: an offline map with edits not synced is refused.
    home.run("offline.sync", &json!({"name": "Crew"})).unwrap();
    let (area, bytes) = package(&mut home, &Value::Null).unwrap();
    assert_eq!(area, "Crew");
    assert_eq!(home.project.offline_areas.len(), 1);
    let repacked = phone(&temp, &bytes, "repacked");
    assert_eq!(repacked.map().name, "Crew (offline)");
    assert_eq!(count(&repacked, "Edited at home"), 1);
    assert_eq!(repacked.project.field_origin.as_ref().unwrap().area, home.project.offline_areas[0].id);

    home.run("offline.sync", &json!({"name": "Crew"})).unwrap();
    home.run("map.activate", &json!({"name": source})).unwrap();
    assert_eq!(count(&home, "Edited at home"), 1);
    home.run("offline.remove", &json!({"name": "Crew"})).unwrap();
    assert!(home.project.maps.iter().all(|map| map.name != "Renamed at home"));
    assert!(home.project.maps.iter().any(|map| map.name == "Crew (offline)"));
}

/// One package, copied to two phones (emailed to a crew, AirDropped to a second device). Each fixes
/// a city and captures a point. A is brought back first. B's copy is the same package, which has
/// come back already: it is refused and nothing of B's comes home. A new package from the area's
/// offline map is how B's work goes on: its edits are redone on it and come back, beside A's.
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
    assert!(error.contains("brought back to offline area 'Crew' already"), "{error}");
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
/// second time, so it is refused; every feature of the first return is at home once.
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
    assert!(second.unwrap_err().contains("already"), "{said:?}");
    let hc = layer(&home, "Cities");
    let at = |label: &str| with_label(&home, hc, &name, label);
    assert_eq!((at("Past the edge"), at("Inside"), at("Afternoon")), (1, 1, 0));
}

/// Desktop Sync of the area's offline map, between packaging and the return: the office's edits in
/// the offline map reach the map, and the phone's changes come back beside them, as neither changed
/// what the other did.
#[test]
fn a_package_made_before_a_sync_of_its_area_comes_back_beside_it() {
    let temp = Temp::new("synced");
    let mut home = home(&temp);
    let source = home.map().name.clone();
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    home.run("offline.open", &json!({"name": "Day"})).unwrap();
    add_point(&mut home, cities, &name, "Office", [12.0, 5.0]);
    home.run("offline.sync", &json!({"name": "Day"})).unwrap();
    home.run("map.activate", &json!({"name": source})).unwrap();
    let (done, said) = bring_back(&temp, &mut a, &mut home, "a");
    assert_eq!((done["added"].as_u64(), done["conflicts"].as_u64()), (Some(1), Some(0)), "{done} {said:?}");
    let hc = layer(&home, "Cities");
    assert_eq!((with_label(&home, hc, &name, "Captured"), with_label(&home, hc, &name, "Office")), (1, 1));
}

/// A package made before packages carried a return check, or an identity of their own, has no list
/// of changes and nothing to compare: it is brought back as it was, and the notice says it can't be
/// told from a copy of one already returned.
#[test]
fn a_package_without_a_return_check_comes_back_with_a_notice() {
    let temp = Temp::new("no-check");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    a.project.field_origin.as_mut().unwrap().generation.clear();
    a.project.field_origin.as_mut().unwrap().package.clear();
    let mark = a.notice_mark();
    package(&mut a, &Value::Null).unwrap();
    assert!(a.notices_since(mark).iter().any(|n| n.text.contains("whole map") && n.text.contains("no return check")));
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

/// The office adds a column to the layer while the crew is out. The crew's edits come back, and
/// the office's values in its new column stay.
#[test]
fn audit_office_adds_a_field_while_crew_is_out() {
    let temp = Temp::new("schema");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let id = a.map().layer(cities).unwrap().dataset.features[0].id;
    a.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": name, "value": "Crew edit"})).unwrap();
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    let hc = layer(&home, "Cities");
    home.run("edit.addField", &json!({"layer": hc, "name": "inspected", "type": "text"})).unwrap();
    let row = home.map().layer(hc).unwrap().dataset.features.iter().position(|f| f.id == id).unwrap();
    home.run("edit.setValue", &json!({"layer": hc, "row": row, "field": "inspected", "value": "yes"})).unwrap();
    let (done, said) = bring_back(&temp, &mut a, &mut home, "a");
    assert_eq!((done["updated"].as_u64(), done["added"].as_u64(), done["conflicts"].as_u64()), (Some(1), Some(1), Some(0)), "{done}");
    assert_eq!(value_of(&home, hc, id, &name).as_deref(), Some("Crew edit"));
    assert_eq!(value_of(&home, hc, id, "inspected").as_deref(), Some("yes"));
    assert_eq!(with_label(&home, hc, &name, "Captured"), 1);
    assert!(!said.iter().any(|t| t.contains("replaced the map's changes")), "{said:?}");
}

/// The crew deletes a point on the phone that the office edited meanwhile, and the office
/// deletes a point the crew edited. An edit beats a delete both ways: the office's edit keeps the
/// point the crew deleted, and the office's deletion stands over the crew's edit; both are said.
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
    assert_eq!((value_of(&home, hc, f0, &name).as_deref(), value_of(&home, hc, f1, &name)), (Some("Office edit"), None), "{done}");
    assert_eq!(
        (done["deleted"].as_u64(), done["skipped"].as_u64(), done["kept"].as_u64(), done["conflicts"].as_u64()),
        (Some(0), Some(1), Some(1), Some(1)),
        "{done}"
    );
    // Both are said, by feature.
    assert!(said.iter().any(|t| t.contains("so they were kept") && t.contains(&format!("Cities #{f0}"))), "{said:?}");
    assert!(said.iter().any(|t| t.contains("edits were dropped") && t.contains(&format!("Cities #{f1}"))), "{said:?}");
}

/// Two packages of one area, one for each phone. A fixes a city and captures a point; B fixes
/// another, captures a point and deletes a third. A comes back first, then B: each brings back only
/// what it changed, so neither undoes the other, and nothing is said to be in conflict.
#[test]
fn two_phones_with_packages_of_one_area_both_come_back_and_neither_undoes_the_other() {
    let temp = Temp::new("two-packages-one-area");
    let mut home = home(&temp);
    let source = home.map().name.clone();
    let (_, out_a) = package(&mut home, &json!({"name": "Crew", "extent": AREA})).unwrap();
    home.run("offline.open", &json!({"name": "Crew"})).unwrap();
    let (_, out_b) = package(&mut home, &Value::Null).unwrap();
    home.run("map.activate", &json!({"name": source})).unwrap();
    let mut a = phone(&temp, &out_a, "a");
    let mut b = phone(&temp, &out_b, "b");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let ids: Vec<u64> = a.map().layer(cities).unwrap().dataset.features.iter().take(3).map(|f| f.id).collect();
    a.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": name, "value": "Fixed by A"})).unwrap();
    add_point(&mut a, cities, &name, "Well from A", [10.0, 5.0]);
    b.run("edit.setValue", &json!({"layer": cities, "row": 1, "field": name, "value": "Fixed by B"})).unwrap();
    add_point(&mut b, cities, &name, "Well from B", [11.0, 6.0]);
    b.run("select.rows", &json!({"layer": cities, "rows": [2]})).unwrap();
    b.run("edit.delete", &Value::Null).unwrap();
    bring_back(&temp, &mut a, &mut home, "a");
    let (done, said) = bring_back(&temp, &mut b, &mut home, "b");
    assert_eq!((done["updated"].as_u64(), done["added"].as_u64(), done["deleted"].as_u64()), (Some(1), Some(1), Some(1)), "{done}");
    assert_eq!(done["conflicts"].as_u64(), Some(0), "{said:?}");
    // B's point took a new id: A's has the one both phones gave theirs.
    assert_eq!(done["renumbered"].as_u64(), Some(1), "{done}");
    let hc = layer(&home, "Cities");
    assert_eq!(value_of(&home, hc, ids[0], &name).as_deref(), Some("Fixed by A"));
    assert_eq!(value_of(&home, hc, ids[1], &name).as_deref(), Some("Fixed by B"));
    assert_eq!(value_of(&home, hc, ids[2], &name), None);
    assert_eq!((with_label(&home, hc, &name, "Well from A"), with_label(&home, hc, &name, "Well from B")), (1, 1));
    assert!(said.iter().all(|t| !t.contains("replaced the map's changes")), "{said:?}");
}

/// The crew removes a field on the phone that home still has: its values in the field aren't
/// known, so Bring Back refuses, naming it, and changes nothing. Removed at home too, the rest
/// comes back.
#[test]
fn a_field_removed_on_the_phone_but_still_at_home_is_refused_not_dropped() {
    let temp = Temp::new("removed-field");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let gone = a.map().layer(cities).unwrap().dataset.fields.iter().find(|f| f.name != name).unwrap().name.clone();
    a.run("edit.deleteField", &json!({"layer": cities, "field": gone})).unwrap();
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    let hc = layer(&home, "Cities");
    let (refused, _) = try_bring_back(&temp, &mut a, &mut home, "a");
    let error = refused.unwrap_err();
    assert!(error.contains("removed field(s)") && error.contains(&gone), "{error}");
    assert_eq!(with_label(&home, hc, &name, "Captured"), 0);
    home.run("edit.deleteField", &json!({"layer": hc, "field": gone})).unwrap();
    let (done, _) = bring_back(&temp, &mut a, &mut home, "a");
    assert_eq!(done["added"].as_u64(), Some(1), "{done}");
}

/// A city that has a photo at home, deleted on the phone: its photo value as given is home's, so
/// the deletion goes in, with nothing said to be in conflict. The rows are taken as given after
/// `project_in` rewrites `photos/` paths, but a package from home carries no `photos/` (only a
/// return does), so nothing is rewritten and the kept value is home's own path.
#[test]
fn a_feature_with_a_photo_deleted_in_the_field_is_deleted_at_home() {
    let temp = Temp::new("photo-delete");
    let mut home = home(&temp);
    let hc = layer(&home, "Cities");
    home.run("edit.addField", &json!({"layer": hc, "name": "photos", "type": "text"})).unwrap();
    let photo = temp.0.join("at-home.jpg");
    std::fs::write(&photo, b"home photo").unwrap();
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let id = phone(&temp, &out, "probe").map().layer(hc).unwrap().dataset.features[0].id;
    let row = home.map().layer(hc).unwrap().dataset.features.iter().position(|f| f.id == id).unwrap();
    home.run("edit.setValue", &json!({"layer": hc, "row": row, "field": "photos", "value": photo.display().to_string()})).unwrap();
    // A package made with the photo named.
    home.run("offline.open", &json!({"name": "Day"})).unwrap();
    home.run("offline.sync", &json!({"name": "Day"})).unwrap();
    let (_, out) = package(&mut home, &Value::Null).unwrap();
    home.run("map.activate", &json!({"name": home.project.offline_areas[0].map.clone()})).unwrap();
    assert!(rustgis_data::zip::read(&out).unwrap().iter().all(|(n, _)| !n.starts_with("photos/")));
    let mut a = phone(&temp, &out, "a");
    assert!(!temp.0.join("a").join("photos").exists(), "no photos folder, so project_in rewrote nothing");
    let cities = layer(&a, "Cities");
    let r = a.map().layer(cities).unwrap().dataset.features.iter().position(|f| f.id == id).unwrap();
    assert_eq!(
        a.map().layer(cities).unwrap().dataset.value(r, a.map().layer(cities).unwrap().dataset.field_index("photos").unwrap()).to_string(),
        photo.display().to_string()
    );
    a.run("select.rows", &json!({"layer": cities, "rows": [r]})).unwrap();
    a.run("edit.delete", &Value::Null).unwrap();
    let (done, said) = bring_back(&temp, &mut a, &mut home, "a");
    assert_eq!((done["deleted"].as_u64(), done["kept"].as_u64()), (Some(1), Some(0)), "{done} {said:?}");
    assert_eq!(value_of(&home, hc, id, "photos"), None);
}

/// A returned package opened on a device that never had the package from home (a second phone,
/// the office's computer) keeps no rows as given: sent again it goes back as a whole map, and
/// says so, and home refuses it as the package already brought back, rather than taking an
/// empty list of changes for its return.
#[test]
fn a_returned_package_opened_elsewhere_keeps_no_rows_as_given() {
    let temp = Temp::new("return-elsewhere");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    let (_, back) = package(&mut a, &Value::Null).unwrap();
    // The device opening the return has no rows of this package.
    let package_id = a.project.field_origin.as_ref().unwrap().package.clone();
    std::fs::remove_dir_all(crate::cmd::field_changes::test_bases::root().unwrap().join(&package_id)).unwrap();
    let dir = temp.0.join("elsewhere");
    let (text, _) = unpack_into(&back, &dir).unwrap();
    let project = project_in(&text, &dir).unwrap();
    assert!(project.given.is_none());
    let mut other = Session::new(project);
    other.opened_package = Some(temp.0.join("elsewhere.rgisfield"));
    other.run("offline.keepRows", &Value::Null).unwrap();
    let mark = other.notice_mark();
    let (_, again) = package(&mut other, &Value::Null).unwrap();
    assert!(other.notices_since(mark).iter().any(|n| n.text.contains("whole map")));
    assert!(changes_in(&again).is_none());
    std::fs::write(temp.0.join("first.rgisfield"), &back).unwrap();
    home.run("offline.bringBack", &json!({"path": temp.0.join("first.rgisfield")})).unwrap();
    std::fs::write(temp.0.join("again.rgisfield"), &again).unwrap();
    let refused = home.run("offline.bringBack", &json!({"path": temp.0.join("again.rgisfield")})).unwrap_err();
    assert!(refused.contains("already"), "{refused}");
}

/// A layer held in another system than longitude/latitude keeps its rows as given in
/// longitude/latitude, as they are compared on sending: unedited, none is listed.
#[test]
fn rows_held_in_another_system_are_kept_and_compared_alike() {
    let temp = Temp::new("held-elsewhere");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let dir = temp.0.join("held");
    let (text, _) = unpack_into(&out, &dir).unwrap();
    let mut project = project_in(&text, &dir).unwrap();
    let package_id = project.field_origin.as_ref().unwrap().package.clone();
    let cities = project.maps[0].layers.iter().position(|l| l.name == "Cities").unwrap();
    project.maps[0].layers[cities].dataset.hold_in(rustgis_data::raster::Crs::WebMercator).unwrap();
    project.given = Some(std::sync::Arc::new(crate::cmd::field_changes::GivenRows::of(&project, &package_id)));
    let mut a = Session::new(project);
    a.opened_package = Some(temp.0.join("held.rgisfield"));
    a.run("offline.keepRows", &Value::Null).unwrap();
    let (_, back) = package(&mut a, &Value::Null).unwrap();
    let changes = changes_in(&back).unwrap();
    assert_eq!(changes["layers"], json!([]), "{changes}");
}

/// A layer home holds in its own system (Web Mercator) goes out held there and is kept as given in
/// longitude/latitude. Unedited rows aren't listed; one edit and one deletion come back, and home's
/// places stay as stored.
#[test]
fn a_layer_held_in_its_own_system_comes_back_with_only_its_changes() {
    let temp = Temp::new("held-round-trip");
    let mut home = home(&temp);
    let hc = layer(&home, "Cities");
    home.map_mut().layer_mut(hc).unwrap().dataset.hold_in(rustgis_data::raster::Crs::WebMercator).unwrap();
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    assert_eq!(a.map().layer(cities).unwrap().dataset.coord_crs(), rustgis_data::raster::Crs::WebMercator);
    let name = text_field(&a, cities);
    let (edited, gone) = {
        let f = &a.map().layer(cities).unwrap().dataset.features;
        (f[0].id, f[1].id)
    };
    let (_, unedited) = package(&mut a, &Value::Null).unwrap();
    assert_eq!(changes_in(&unedited).unwrap()["layers"], json!([]));
    a.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": name, "value": "Held edit"})).unwrap();
    a.run("select.rows", &json!({"layer": cities, "rows": [1]})).unwrap();
    a.run("edit.delete", &Value::Null).unwrap();
    let place = |s: &Session, id: u64| {
        let l = s.map().layer(layer(s, "Cities")).unwrap();
        l.dataset.features.iter().find(|f| f.id == id).and_then(|f| f.geometry.clone())
    };
    let before = place(&home, edited);
    let (done, said) = bring_back(&temp, &mut a, &mut home, "a");
    assert_eq!(
        (done["updated"].as_u64(), done["deleted"].as_u64(), done["conflicts"].as_u64()),
        (Some(1), Some(1), Some(0)),
        "{done} {said:?}"
    );
    assert_eq!(value_of(&home, hc, edited, &name).as_deref(), Some("Held edit"));
    assert_eq!(value_of(&home, hc, gone, &name), None);
    assert_eq!(place(&home, edited), before, "an attribute edit leaves home's stored place as it was");
    assert_eq!(home.map().layer(hc).unwrap().dataset.coord_crs(), rustgis_data::raster::Crs::WebMercator);
}

/// Two packages of one area. A adds a field and fills it in for a city; B, given the city before
/// the field was there, deletes it. The city has a change B never saw, so it is kept, and said.
#[test]
fn a_deletion_keeps_a_feature_with_a_value_in_a_field_added_since() {
    let temp = Temp::new("delete-new-field");
    let mut home = home(&temp);
    let source = home.map().name.clone();
    let (_, out_a) = package(&mut home, &json!({"name": "Crew", "extent": AREA})).unwrap();
    home.run("offline.open", &json!({"name": "Crew"})).unwrap();
    let (_, out_b) = package(&mut home, &Value::Null).unwrap();
    home.run("map.activate", &json!({"name": source})).unwrap();
    let mut a = phone(&temp, &out_a, "a");
    let mut b = phone(&temp, &out_b, "b");
    let cities = layer(&a, "Cities");
    let id = a.map().layer(cities).unwrap().dataset.features[0].id;
    a.run("edit.addField", &json!({"layer": cities, "name": "inspection", "type": "text"})).unwrap();
    a.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "inspection", "value": "passed"})).unwrap();
    b.run("select.rows", &json!({"layer": cities, "rows": [0]})).unwrap();
    b.run("edit.delete", &Value::Null).unwrap();
    bring_back(&temp, &mut a, &mut home, "a");
    let (done, said) = bring_back(&temp, &mut b, &mut home, "b");
    let hc = layer(&home, "Cities");
    assert_eq!(value_of(&home, hc, id, "inspection").as_deref(), Some("passed"), "{done}");
    assert_eq!((done["deleted"].as_u64(), done["kept"].as_u64()), (Some(0), Some(1)), "{done}");
    assert!(said.iter().any(|t| t.contains("so they were kept") && t.contains(&format!("Cities #{id}"))), "{said:?}");
}

/// Home uses the id the field gave its first new point but not its second's: the first takes a new
/// id, above both, and the second keeps its own, with a list of changes or as a whole map.
#[test]
fn a_new_feature_keeps_its_free_id_when_another_is_renumbered() {
    for whole in [false, true] {
        new_features_keep_free_ids(whole);
    }
}

/// [`a_new_feature_keeps_its_free_id_when_another_is_renumbered`], `whole` with no rows kept.
fn new_features_keep_free_ids(whole: bool) {
    let temp = Temp::new("free-ids");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "All", "extent": [-180.0, -90.0, 180.0, 90.0]})).unwrap();
    let mut a = phone(&temp, &out, "a");
    if whole {
        let package_id = a.project.field_origin.as_ref().unwrap().package.clone();
        std::fs::remove_dir_all(crate::cmd::field_changes::test_bases::root().unwrap().join(package_id)).unwrap();
    }
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let first = add_point(&mut a, cities, &name, "First", [10.0, 5.0]);
    let second = add_point(&mut a, cities, &name, "Second", [11.0, 5.0]);
    let hc = layer(&home, "Cities");
    let taken = add_point(&mut home, hc, &name, "Home's", [12.0, 5.0]);
    assert_eq!((taken, second), (first, first + 1));
    let (done, _) = bring_back(&temp, &mut a, &mut home, "a");
    assert_eq!(done["renumbered"].as_u64(), Some(1), "{done}");
    assert_eq!(value_of(&home, hc, second, &name).as_deref(), Some("Second"));
    assert_eq!(value_of(&home, hc, first, &name).as_deref(), Some("Home's"));
    assert_eq!(value_of(&home, hc, second + 1, &name).as_deref(), Some("First"));
}

/// Two packages given a field; home removes it, and A comes back with no change to it. B then
/// comes back with an edit to it: the field isn't added back, and the return is refused.
#[test]
fn a_field_removed_at_home_is_not_added_back_by_a_later_return() {
    let temp = Temp::new("removed-at-home");
    let mut home = home(&temp);
    let source = home.map().name.clone();
    let (_, out_a) = package(&mut home, &json!({"name": "Crew", "extent": AREA})).unwrap();
    home.run("offline.open", &json!({"name": "Crew"})).unwrap();
    let (_, out_b) = package(&mut home, &Value::Null).unwrap();
    home.run("map.activate", &json!({"name": source})).unwrap();
    let mut a = phone(&temp, &out_a, "a");
    let mut b = phone(&temp, &out_b, "b");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let gone = a.map().layer(cities).unwrap().dataset.fields.iter().find(|f| f.name != name).unwrap().name.clone();
    let hc = layer(&home, "Cities");
    home.run("edit.deleteField", &json!({"layer": hc, "field": gone})).unwrap();
    add_point(&mut a, cities, &name, "From A", [10.0, 5.0]);
    bring_back(&temp, &mut a, &mut home, "a");
    b.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": gone, "value": "1"})).unwrap();
    let (refused, _) = try_bring_back(&temp, &mut b, &mut home, "b");
    assert!(refused.unwrap_err().contains("hasn't the field(s)"));
    assert!(home.map().layer(hc).unwrap().dataset.field_index(&gone).is_none());
}

/// A photo the field didn't change, which home no longer has where it named it, isn't stored
/// at home with nothing naming it: it is left out, and said.
#[test]
fn a_photo_the_field_didnt_change_is_not_stored_unnamed() {
    let temp = Temp::new("unchanged-photo");
    let mut home = home(&temp);
    let hc = layer(&home, "Cities");
    home.run("edit.addField", &json!({"layer": hc, "name": "photos", "type": "text"})).unwrap();
    let photo = temp.0.join("shared.jpg");
    std::fs::write(&photo, b"photo bytes").unwrap();
    home.run("edit.setValue", &json!({"layer": hc, "row": 0, "field": "photos", "value": photo.display().to_string()})).unwrap();
    let (_, out) = package(&mut home, &json!({"name": "All", "extent": [-180.0, -90.0, 180.0, 90.0]})).unwrap();
    // The phone sees the file where home named it (a shared folder), so the photo goes back.
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    let (_, back) = package(&mut a, &Value::Null).unwrap();
    assert!(rustgis_data::zip::read(&back).unwrap().iter().any(|(n, _)| n == "photos/shared.jpg"));
    std::fs::remove_file(&photo).unwrap();
    let path = temp.0.join("back.rgisfield");
    std::fs::write(&path, &back).unwrap();
    let mark = home.notice_mark();
    let done = home.run("offline.bringBack", &json!({"path": path})).unwrap();
    assert_eq!(done["photos"].as_u64(), Some(0), "{done}");
    assert!(!temp.0.join("All photos").join("shared.jpg").exists());
    assert!(home.notices_since(mark).iter().any(|n| n.text.contains("no change it brings back names")));
}

/// Sending a return spends its package: once the file is written, the rows it gave are removed
/// from the phone, and said. A send that fails writes nothing and keeps them.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_sent_return_removes_the_rows_its_package_gave_and_a_failed_one_keeps_them() {
    let temp = Temp::new("spent");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let package_id = a.project.field_origin.as_ref().unwrap().package.clone();
    let kept = crate::cmd::field_changes::test_bases::root().unwrap().join(&package_id);
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    let nowhere = temp.0.join("no such folder").join("back.rgisfield");
    assert!(a.run("offline.package", &json!({"path": nowhere})).is_err());
    assert!(kept.join("layers.json").is_file(), "a failed send keeps the rows");
    let path = temp.0.join("back.rgisfield");
    let mark = a.notice_mark();
    a.run("offline.package", &json!({"path": path})).unwrap();
    assert!(path.is_file());
    assert!(!kept.exists(), "a sent return lets the rows go");
    // Moved out of its place before it was removed, and nothing of it left anywhere.
    let left: Vec<_> = std::fs::read_dir(kept.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&package_id) && !e.file_name().to_string_lossy().ends_with(".spent"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    // Only the mark that it was sent is left, so opening it again keeps nothing.
    assert!(kept.with_extension("spent").is_file());
    assert!(a.notices_since(mark).iter().any(|n| n.text.contains("no longer keeps the rows")), "{:?}", a.notices);
    let back = std::fs::read(&path).unwrap();
    assert!(changes_in(&back).is_some());
    // It comes back home with its changes.
    let done = home.run("offline.bringBack", &json!({"path": path})).unwrap();
    assert_eq!(done["added"].as_u64(), Some(1), "{done}");
}

/// The package made at home to go out is no return: Bring Back refuses it, records nothing, and
/// the phone's real return comes back after.
#[test]
fn a_package_made_to_go_out_is_refused_by_bring_back() {
    let temp = Temp::new("outbound");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let path = temp.0.join("out.rgisfield");
    std::fs::write(&path, &out).unwrap();
    let refused = home.run("offline.bringBack", &json!({"path": path})).unwrap_err();
    assert!(refused.contains("made here to go to the field"), "{refused}");
    assert!(home.project.offline_areas[0].returned.is_empty());
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    let (done, _) = bring_back(&temp, &mut a, &mut home, "a");
    assert_eq!(done["added"].as_u64(), Some(1), "{done}");
}

/// The office edits the area's open offline map and doesn't sync it. Bringing a package back
/// would renew that map from the source and lose the edit, so it is refused with nothing changed,
/// on either path. Synced first, a list of changes comes home beside the edit; a whole map is
/// refused then by its return check.
#[test]
fn bring_back_refuses_while_the_offline_map_has_unsynced_edits() {
    for whole in [false, true] {
        unsynced_offline_map_refuses(whole);
    }
}

/// [`bring_back_refuses_while_the_offline_map_has_unsynced_edits`], `whole` with no rows kept.
fn unsynced_offline_map_refuses(whole: bool) {
    let temp = Temp::new("unsynced");
    let mut home = home(&temp);
    let source = home.map().name.clone();
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    if whole {
        let package_id = a.project.field_origin.as_ref().unwrap().package.clone();
        std::fs::remove_dir_all(crate::cmd::field_changes::test_bases::root().unwrap().join(package_id)).unwrap();
    }
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    home.run("offline.open", &json!({"name": "Day"})).unwrap();
    let id = home.map().layer(cities).unwrap().dataset.features[0].id;
    home.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": name, "value": "Office, unsynced"})).unwrap();
    let (refused, _) = try_bring_back(&temp, &mut a, &mut home, "a");
    assert!(refused.unwrap_err().contains("aren't synced yet"));
    assert_eq!(value_of(&home, cities, id, &name).as_deref(), Some("Office, unsynced"));
    home.run("offline.sync", &json!({"name": "Day"})).unwrap();
    if whole {
        // A whole map is compared with the area as it was: after the sync, its return check refuses it.
        assert!(try_bring_back(&temp, &mut a, &mut home, "a").0.unwrap_err().contains("out of date"));
        return;
    }
    let (done, _) = bring_back(&temp, &mut a, &mut home, "a");
    assert_eq!(done["added"].as_u64(), Some(1), "{done}");
    home.run("map.activate", &json!({"name": source})).unwrap();
    let hc = layer(&home, "Cities");
    assert_eq!(value_of(&home, hc, id, &name).as_deref(), Some("Office, unsynced"));
    assert_eq!(with_label(&home, hc, &name, "Captured"), 1);
}

/// A photo the phone gave a city the office has since deleted: the edit is dropped, and so is the
/// photo, which no feature would name.
#[test]
fn a_photo_of_an_edit_dropped_at_home_is_not_stored() {
    let temp = Temp::new("dropped-photo");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let id = a.map().layer(cities).unwrap().dataset.features[0].id;
    let photo = temp.0.join("taken.jpg");
    std::fs::write(&photo, b"photo bytes").unwrap();
    a.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
    a.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": "photos", "value": photo.display().to_string()})).unwrap();
    let (_, back) = package(&mut a, &Value::Null).unwrap();
    std::fs::remove_file(&photo).unwrap();
    let hc = layer(&home, "Cities");
    let row = home.map().layer(hc).unwrap().dataset.features.iter().position(|f| f.id == id).unwrap();
    home.run("select.rows", &json!({"layer": hc, "rows": [row]})).unwrap();
    home.run("edit.delete", &Value::Null).unwrap();
    let path = temp.0.join("back.rgisfield");
    std::fs::write(&path, &back).unwrap();
    let done = home.run("offline.bringBack", &json!({"path": path})).unwrap();
    assert_eq!((done["skipped"].as_u64(), done["photos"].as_u64()), (Some(1), Some(0)), "{done}");
    assert!(!temp.0.join("Day photos").join("taken.jpg").exists());
}

/// Home's layer holds the highest id there is: a feature added in the field whose id home uses
/// has none left to take, so Bring Back refuses, with nothing changed, on either path.
#[test]
fn a_new_feature_with_no_id_left_is_refused_not_duplicated() {
    for whole in [false, true] {
        no_id_left(whole);
    }
}

/// [`a_new_feature_with_no_id_left_is_refused_not_duplicated`], `whole` with no rows kept.
fn no_id_left(whole: bool) {
    let temp = Temp::new("no-id-left");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    if whole {
        let package_id = a.project.field_origin.as_ref().unwrap().package.clone();
        std::fs::remove_dir_all(crate::cmd::field_changes::test_bases::root().unwrap().join(package_id)).unwrap();
    }
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let new = add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    // Home uses the new feature's id, and the highest there is, both outside the area.
    let hc = layer(&home, "Cities");
    let fields = home.map().layer(hc).unwrap().dataset.fields.len();
    let outside = || Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(150.0, 70.0)));
    let ds = &mut home.map_mut().layer_mut(hc).unwrap().dataset;
    ds.features.push_parts(new, outside(), vec![rustgis_data::Value::Null; fields]);
    ds.features.push_parts(u64::MAX, outside(), vec![rustgis_data::Value::Null; fields]);
    let before = count(&home, hc);
    let (refused, _) = try_bring_back(&temp, &mut a, &mut home, "a");
    assert!(refused.unwrap_err().contains("none is left"));
    assert_eq!(count(&home, hc), before);
}

/// The crew adds a field and fills nothing in: the field still comes home.
#[test]
fn a_field_added_in_the_field_comes_home_with_no_values() {
    let temp = Temp::new("empty-field");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    a.run("edit.addField", &json!({"layer": cities, "name": "inspection", "type": "text"})).unwrap();
    bring_back(&temp, &mut a, &mut home, "a");
    let hc = layer(&home, "Cities");
    assert!(home.map().layer(hc).unwrap().dataset.field_index("inspection").is_some());
}

/// A list naming an edit to a field the package's map hasn't has no new value for it: refused,
/// rather than taken as Null over home's value.
#[test]
fn an_edit_to_a_field_the_package_map_lacks_is_refused() {
    let temp = Temp::new("edit-missing-field");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let id = a.map().layer(cities).unwrap().dataset.features[0].id;
    let gone = a.map().layer(cities).unwrap().dataset.fields.iter().find(|f| f.name != name).unwrap().name.clone();
    a.run("edit.deleteField", &json!({"layer": cities, "field": gone})).unwrap();
    let (_, back) = package(&mut a, &Value::Null).unwrap();
    // The list says the field's value changed, and not that the field was removed.
    let mut entries = rustgis_data::zip::read(&back).unwrap();
    let list = entries.iter_mut().find(|(n, _)| n == crate::cmd::field_changes::ENTRY).unwrap();
    let mut changes: Value = serde_json::from_slice(&list.1).unwrap();
    changes["layers"][0]["removed"] = json!([]);
    changes["layers"][0]["edited"] = json!([{"id": id, "values": [[gone, "Null"]]}]);
    list.1 = serde_json::to_vec(&changes).unwrap();
    let path = temp.0.join("back.rgisfield");
    std::fs::write(&path, rustgis_data::zip::write(&entries).unwrap()).unwrap();
    let hc = layer(&home, "Cities");
    let before = value_of(&home, hc, id, &gone);
    let refused = home.run("offline.bringBack", &json!({"path": path})).unwrap_err();
    assert!(refused.contains("hasn't the field(s)") && refused.contains(&gone), "{refused}");
    assert_eq!(value_of(&home, hc, id, &gone), before);
}

/// The crew deletes the city with the highest id the package gave, then captures a point. The
/// new point never takes the deleted city's id, even after the field project is saved and opened
/// again, so both come home as what they are: with the office unchanged, the city is deleted and
/// the point added; with the office having deleted it, the point is still added; with the office
/// having edited it, the city is kept and the point added.
#[test]
fn a_feature_added_after_deleting_the_highest_id_never_takes_its_id() {
    for office in ["unchanged", "deleted", "edited"] {
        for reopen in [false, true] {
            added_after_deleting_the_highest(office, reopen);
        }
    }
}

/// [`a_feature_added_after_deleting_the_highest_id_never_takes_its_id`] for one office and reopen.
fn added_after_deleting_the_highest(office: &str, reopen: bool) {
    let temp = Temp::new("highest-id");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "All", "extent": [-180.0, -90.0, 180.0, 90.0]})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let (row, top) = {
        let f = &a.map().layer(cities).unwrap().dataset.features;
        f.iter().enumerate().max_by_key(|(_, f)| f.id).map(|(i, f)| (i, f.id)).unwrap()
    };
    a.run("select.rows", &json!({"layer": cities, "rows": [row]})).unwrap();
    a.run("edit.delete", &Value::Null).unwrap();
    if reopen {
        let saved = temp.0.join("field.rgis");
        a.run("project.saveAs", &json!({"path": saved})).unwrap();
        a = Session::new(Project::default());
        a.run("project.open", &json!({"path": saved})).unwrap();
    }
    let new = add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    assert!(new > top, "{office} {reopen}: the new point took id {new}, at or below the package's highest {top}");
    let hc = layer(&home, "Cities");
    let at = |home: &Session| home.map().layer(hc).unwrap().dataset.features.iter().position(|f| f.id == top).unwrap();
    match office {
        "deleted" => {
            home.run("select.rows", &json!({"layer": hc, "rows": [at(&home)]})).unwrap();
            home.run("edit.delete", &Value::Null).unwrap();
        }
        "edited" => {
            let r = at(&home);
            home.run("edit.setValue", &json!({"layer": hc, "row": r, "field": name, "value": "Office edit"})).unwrap();
        }
        _ => {}
    }
    let (done, said) = bring_back(&temp, &mut a, &mut home, "a");
    assert_eq!(done["added"].as_u64(), Some(1), "{office} {reopen}: {done} {said:?}");
    assert_eq!(with_label(&home, hc, &name, "Captured"), 1, "{office} {reopen}");
    let kept = if office == "edited" { Some("Office edit") } else { None };
    assert_eq!(value_of(&home, hc, top, &name).as_deref(), kept, "{office} {reopen}: {done}");
}

/// The office deletes the city with the highest id a package still out was given, then adds one.
/// The new city never takes the deleted city's id, even after home is saved and opened again, so
/// the phone's edit of the deleted city is dropped, and said, rather than written over the
/// office's new city.
#[test]
fn a_feature_added_at_home_never_takes_an_id_a_package_out_was_given() {
    for reopen in [false, true] {
        let temp = Temp::new("home-highest-id");
        let mut home = home(&temp);
        let (_, out) = package(&mut home, &json!({"name": "All", "extent": [-180.0, -90.0, 180.0, 90.0]})).unwrap();
        let mut a = phone(&temp, &out, "a");
        let cities = layer(&a, "Cities");
        let name = text_field(&a, cities);
        let (row, top) = {
            let f = &a.map().layer(cities).unwrap().dataset.features;
            f.iter().enumerate().max_by_key(|(_, f)| f.id).map(|(i, f)| (i, f.id)).unwrap()
        };
        a.run("edit.setValue", &json!({"layer": cities, "row": row, "field": name, "value": "Crew edit"})).unwrap();
        let hc = layer(&home, "Cities");
        let at = home.map().layer(hc).unwrap().dataset.features.iter().position(|f| f.id == top).unwrap();
        home.run("select.rows", &json!({"layer": hc, "rows": [at]})).unwrap();
        home.run("edit.delete", &Value::Null).unwrap();
        if reopen {
            let saved = temp.0.join("home.rgis");
            home.run("project.saveAs", &json!({"path": saved})).unwrap();
            home.run("project.open", &json!({"path": saved})).unwrap();
        }
        let hc = layer(&home, "Cities");
        let new = add_point(&mut home, hc, &name, "Office's new city", [12.0, 5.0]);
        assert!(new > top, "{reopen}: the office's city took id {new}, at or below {top}");
        let (done, said) = bring_back(&temp, &mut a, &mut home, "a");
        assert_eq!(done["skipped"].as_u64(), Some(1), "{reopen}: {done}");
        assert!(said.iter().any(|t| t.contains("edits were dropped") && t.contains(&format!("Cities #{top}"))), "{said:?}");
        assert_eq!(value_of(&home, hc, new, &name).as_deref(), Some("Office's new city"));
    }
}

/// A field project whose package gave the largest id a floor can be still gives each new
/// feature an id of its own, saved and opened again: the floor leaves room above it.
#[test]
fn new_features_are_given_ids_of_their_own_above_the_largest_floor() {
    let temp = Temp::new("largest-floor");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let floor = i64::MAX.unsigned_abs() - 1;
    a.project.field_origin.as_mut().unwrap().ids.insert(cities, floor);
    let saved = temp.0.join("field.rgis");
    a.run("project.saveAs", &json!({"path": saved})).unwrap();
    let mut a = Session::new(Project::default());
    a.run("project.open", &json!({"path": saved})).unwrap();
    let first = add_point(&mut a, cities, &name, "First", [10.0, 5.0]);
    let second = add_point(&mut a, cities, &name, "Second", [11.0, 5.0]);
    assert_eq!((first, second), (floor + 1, floor + 2));
}

/// Package A is out holding the city with the highest id; the office deletes it and syncs, which
/// renews the area's offline map without it. Package B, made from that map, still records the
/// id A holds, so a feature B's phone adds never takes it.
#[test]
fn a_package_made_after_a_renewal_keeps_the_ids_one_still_out_holds() {
    let temp = Temp::new("renewed-floor");
    let mut home = home(&temp);
    let source = home.map().name.clone();
    let (_, out_a) = package(&mut home, &json!({"name": "All", "extent": [-180.0, -90.0, 180.0, 90.0]})).unwrap();
    let a = phone(&temp, &out_a, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let top = a.map().layer(cities).unwrap().dataset.features.iter().map(|f| f.id).max().unwrap();
    let at = home.map().layer(cities).unwrap().dataset.features.iter().position(|f| f.id == top).unwrap();
    home.run("select.rows", &json!({"layer": cities, "rows": [at]})).unwrap();
    home.run("edit.delete", &Value::Null).unwrap();
    home.run("offline.open", &json!({"name": "All"})).unwrap();
    home.run("offline.sync", &json!({"name": "All"})).unwrap();
    let (_, out_b) = package(&mut home, &Value::Null).unwrap();
    home.run("map.activate", &json!({"name": source})).unwrap();
    let mut b = phone(&temp, &out_b, "b");
    assert!(b.project.field_origin.as_ref().unwrap().ids.get(&cities).is_some_and(|id| *id >= top));
    let new = add_point(&mut b, cities, &name, "From B", [10.0, 5.0]);
    assert!(new > top, "B's point took id {new}, at or below {top}, which A still holds");
}

/// A layer holding a feature whose id leaves no room above it isn't packaged: features added in
/// the field couldn't be given ids of their own.
#[test]
fn a_layer_with_no_ids_left_above_is_not_packaged() {
    let temp = Temp::new("no-room");
    let mut home = home(&temp);
    let hc = layer(&home, "Cities");
    let fields = home.map().layer(hc).unwrap().dataset.fields.len();
    let inside = || Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 5.0)));
    // From the largest id a GeoPackage or PostGIS feature can have up: the next would be one neither
    // can hold.
    for top in [i64::MAX.unsigned_abs(), u64::MAX] {
        let ds = &mut home.map_mut().layer_mut(hc).unwrap().dataset;
        ds.features.retain_rows(|r| r.id() < i64::MAX.unsigned_abs());
        ds.features.push_parts(top, inside(), vec![rustgis_data::Value::Null; fields]);
        let refused = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap_err();
        assert!(refused.contains("Cities") && refused.contains("can hold"), "{top}: {refused}");
    }
}

/// A package is out holding the city with the highest id. The office deletes the city and syncs,
/// which renews the area's offline map without it, then adds a feature in that offline map and
/// syncs again. The new feature never takes the city's id, so the package's edit of the city is
/// dropped, and said, rather than written over the office's feature: with its list of changes or
/// as a whole map (refused then by its return check).
#[test]
fn a_feature_added_in_a_renewed_offline_map_never_takes_an_id_a_package_holds() {
    for whole in [false, true] {
        let temp = Temp::new("renewed-offline-floor");
        let mut home = home(&temp);
        let source = home.map().name.clone();
        let (_, out) = package(&mut home, &json!({"name": "All", "extent": [-180.0, -90.0, 180.0, 90.0]})).unwrap();
        let mut a = phone(&temp, &out, "a");
        if whole {
            let package_id = a.project.field_origin.as_ref().unwrap().package.clone();
            std::fs::remove_dir_all(crate::cmd::field_changes::test_bases::root().unwrap().join(package_id)).unwrap();
        }
        let cities = layer(&a, "Cities");
        let name = text_field(&a, cities);
        let (row, top) = {
            let f = &a.map().layer(cities).unwrap().dataset.features;
            f.iter().enumerate().max_by_key(|(_, f)| f.id).map(|(i, f)| (i, f.id)).unwrap()
        };
        a.run("edit.setValue", &json!({"layer": cities, "row": row, "field": name, "value": "Crew edit"})).unwrap();
        let at = home.map().layer(cities).unwrap().dataset.features.iter().position(|f| f.id == top).unwrap();
        home.run("select.rows", &json!({"layer": cities, "rows": [at]})).unwrap();
        home.run("edit.delete", &Value::Null).unwrap();
        home.run("offline.open", &json!({"name": "All"})).unwrap();
        home.run("offline.sync", &json!({"name": "All"})).unwrap();
        let new = add_point(&mut home, cities, &name, "Office, in the offline map", [12.0, 5.0]);
        assert!(new > top, "{whole}: the offline map's feature took id {new}, at or below {top}");
        home.run("offline.sync", &json!({"name": "All"})).unwrap();
        home.run("map.activate", &json!({"name": source})).unwrap();
        let (done, said) = try_bring_back(&temp, &mut a, &mut home, "a");
        if whole {
            assert!(done.unwrap_err().contains("out of date"));
        } else {
            let done = done.unwrap();
            assert_eq!(done["skipped"].as_u64(), Some(1), "{done}");
            assert!(said.iter().any(|t| t.contains("edits were dropped") && t.contains(&format!("Cities #{top}"))), "{said:?}");
        }
        let hc = layer(&home, "Cities");
        assert_eq!(value_of(&home, hc, new, &name).as_deref(), Some("Office, in the offline map"), "{whole}");
    }
}

/// Area A's package is out holding the city with the highest id; the office deletes it, then
/// makes area B. B's snapshot, its package and its offline map all keep new ids above the deleted
/// city's, so neither B's phone nor the office in B's offline map gives a new feature that id.
#[test]
fn a_new_areas_snapshot_keeps_ids_above_those_deleted_since() {
    let temp = Temp::new("snapshot-floor");
    let mut home = home(&temp);
    let source = home.map().name.clone();
    let world = json!([-180.0, -90.0, 180.0, 90.0]);
    let (_, out_a) = package(&mut home, &json!({"name": "A", "extent": world})).unwrap();
    let a = phone(&temp, &out_a, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let top = a.map().layer(cities).unwrap().dataset.features.iter().map(|f| f.id).max().unwrap();
    let at = home.map().layer(cities).unwrap().dataset.features.iter().position(|f| f.id == top).unwrap();
    home.run("select.rows", &json!({"layer": cities, "rows": [at]})).unwrap();
    home.run("edit.delete", &Value::Null).unwrap();
    let (_, out_b) = package(&mut home, &json!({"name": "B", "extent": world})).unwrap();
    let mut b = phone(&temp, &out_b, "b");
    assert!(b.project.field_origin.as_ref().unwrap().ids.get(&cities).is_some_and(|id| *id >= top));
    assert!(add_point(&mut b, cities, &name, "From B", [10.0, 5.0]) > top);
    home.run("map.activate", &json!({"name": source})).unwrap();
    home.run("offline.open", &json!({"name": "B"})).unwrap();
    assert!(add_point(&mut home, cities, &name, "In B's offline map", [11.0, 5.0]) > top);
}

/// Package A is out. The office adds a city, syncs, and makes package B with it; then deletes the
/// city. A's phone captures a point, which its package gave that same id, and A comes back first:
/// the point takes a new id, as the deleted city's may still be held by a package out, so B's edit
/// of the city is dropped, and said, rather than written over A's point.
#[test]
fn a_feature_added_in_the_field_never_keeps_an_id_another_package_holds() {
    let temp = Temp::new("held-elsewhere-id");
    let mut home = home(&temp);
    let source = home.map().name.clone();
    let (_, out_a) = package(&mut home, &json!({"name": "All", "extent": [-180.0, -90.0, 180.0, 90.0]})).unwrap();
    let mut a = phone(&temp, &out_a, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let city = add_point(&mut home, cities, &name, "Office's city", [12.0, 5.0]);
    home.run("offline.open", &json!({"name": "All"})).unwrap();
    home.run("offline.sync", &json!({"name": "All"})).unwrap();
    let (_, out_b) = package(&mut home, &Value::Null).unwrap();
    home.run("map.activate", &json!({"name": source})).unwrap();
    let mut b = phone(&temp, &out_b, "b");
    let at = home.map().layer(cities).unwrap().dataset.features.iter().position(|f| f.id == city).unwrap();
    home.run("select.rows", &json!({"layer": cities, "rows": [at]})).unwrap();
    home.run("edit.delete", &Value::Null).unwrap();
    let point = add_point(&mut a, cities, &name, "A's point", [10.0, 5.0]);
    assert_eq!(point, city, "the test needs A's point to have the office city's id");
    let (done, _) = bring_back(&temp, &mut a, &mut home, "a");
    assert_eq!(done["renumbered"].as_u64(), Some(1), "{done}");
    let r = b.map().layer(cities).unwrap().dataset.features.iter().position(|f| f.id == city).unwrap();
    b.run("edit.setValue", &json!({"layer": cities, "row": r, "field": name, "value": "B's edit"})).unwrap();
    let (done, said) = bring_back(&temp, &mut b, &mut home, "b");
    assert_eq!(done["skipped"].as_u64(), Some(1), "{done}");
    assert!(said.iter().any(|t| t.contains("edits were dropped") && t.contains(&format!("Cities #{city}"))), "{said:?}");
    assert_eq!(with_label(&home, cities, &name, "A's point"), 1);
    assert_eq!(with_label(&home, cities, &name, "B's edit"), 0);
}

/// The office removes a layer while the package is out; the phone adds a feature with a photo to
/// it. The layer's changes aren't brought back, and said, and neither is the photo, which no
/// feature here would name.
#[test]
fn a_photo_of_a_feature_added_to_a_layer_home_removed_is_not_stored() {
    let temp = Temp::new("removed-layer-photo");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let photo = temp.0.join("new.jpg");
    std::fs::write(&photo, b"photo bytes").unwrap();
    a.run("edit.addField", &json!({"layer": cities, "name": "photos", "type": "text"})).unwrap();
    a.run(
        "edit.addFeature",
        &json!({"layer": cities, "geometry": {"type": "Point", "coordinates": [10.0, 5.0]}, "attributes": {name.clone(): "Captured", "photos": photo.display().to_string()}}),
    )
    .unwrap();
    let (_, back) = package(&mut a, &Value::Null).unwrap();
    std::fs::remove_file(&photo).unwrap();
    home.run("layer.remove", &json!({"layer": layer(&home, "Cities")})).unwrap();
    let path = temp.0.join("back.rgisfield");
    std::fs::write(&path, &back).unwrap();
    let mark = home.notice_mark();
    let done = home.run("offline.bringBack", &json!({"path": path})).unwrap();
    assert_eq!(done["photos"].as_u64(), Some(0), "{done}");
    assert!(!temp.0.join("Day photos").join("new.jpg").exists());
    assert!(home.notices_since(mark).iter().any(|n| n.text.contains("are no longer in the map")));
}

/// The office defines another projection for a layer of the area's open offline map, a change to
/// where its features are though its rows are as they were: Bring Back refuses, as for any edit
/// not synced, so renewing the area doesn't lose it.
#[test]
fn a_projection_defined_on_the_offline_map_counts_as_unsynced() {
    let temp = Temp::new("defined-projection");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    home.run("offline.open", &json!({"name": "Day"})).unwrap();
    home.run("layer.defineProjection", &json!({"layer": cities, "crs": "EPSG:3857"})).unwrap();
    let (refused, _) = try_bring_back(&temp, &mut a, &mut home, "a");
    assert!(refused.unwrap_err().contains("aren't synced yet"));
}

/// The office adds a city to the map and sketches another in the area's offline map, both taking
/// the next id, then makes a package from the offline map without syncing. Sent out, the crew's
/// edit to the sketch would come home to the office's city once Bring Back made them sync, and the
/// sketch renumbered: the package is refused until the area is synced, and the edit then comes home
/// to the sketch.
#[test]
fn a_package_from_an_offline_map_with_unsynced_edits_is_refused_until_synced() {
    let temp = Temp::new("unsynced-out");
    let mut home = home(&temp);
    let source = home.map().name.clone();
    package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let hc = layer(&home, "Cities");
    let name = text_field(&home, hc);
    let office = add_point(&mut home, hc, &name, "Office", [10.0, 5.0]);
    home.run("offline.open", &json!({"name": "Day"})).unwrap();
    let oc = layer(&home, "Cities");
    add_point(&mut home, oc, &name, "Sketch", [11.0, 5.0]);
    let refused = package(&mut home, &json!({"name": "Day"})).unwrap_err();
    assert!(refused.contains("aren't synced yet"), "{refused}");
    home.run("offline.sync", &json!({"name": "Day"})).unwrap();
    let (_, out) = package(&mut home, &json!({"name": "Day"})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let ac = layer(&a, "Cities");
    let ds = &a.map().layer(ac).unwrap().dataset;
    let j = ds.field_index(&name).unwrap();
    let row = (0..ds.features.len()).find(|r| ds.value(*r, j).to_string() == "Sketch").unwrap();
    a.run("edit.setValue", &json!({"layer": ac, "row": row, "field": name, "value": "Crew edit"})).unwrap();
    let (done, _) = bring_back(&temp, &mut a, &mut home, "a");
    home.run("map.activate", &json!({"name": source})).unwrap();
    let hc = layer(&home, "Cities");
    assert_eq!(value_of(&home, hc, office, &name).as_deref(), Some("Office"), "{done}");
    assert_eq!(with_label(&home, hc, &name, "Crew edit"), 1, "{done}");
}

/// The package gave the highest id a floor can be, so the phone's new point takes `i64::MAX`, which
/// no GeoPackage or PostGIS table holds: Bring Back refuses it with nothing changed, and the area
/// can still be packaged.
#[test]
fn a_return_needing_an_id_no_table_holds_is_refused_and_the_area_stays_packageable() {
    let temp = Temp::new("no-table-id");
    let mut home = home(&temp);
    let hc = layer(&home, "Cities");
    let fields = home.map().layer(hc).unwrap().dataset.fields.len();
    let inside = || Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 5.0)));
    let top = i64::MAX.unsigned_abs() - 1;
    home.map_mut().layer_mut(hc).unwrap().dataset.features.push_parts(top, inside(), vec![rustgis_data::Value::Null; fields]);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    assert_eq!(add_point(&mut a, cities, &name, "Captured", [11.0, 5.0]), i64::MAX.unsigned_abs());
    let before = count(&home, hc);
    let (refused, _) = try_bring_back(&temp, &mut a, &mut home, "a");
    assert!(refused.unwrap_err().contains("none is left"));
    assert_eq!(count(&home, hc), before);
    assert!(home.project.offline_areas[0].returned.is_empty());
    package(&mut home, &json!({"name": "Day"})).unwrap();
}

/// The top road is deleted and saved, then a script discards the layer's edits, reading its rows
/// again, and adds a feature: the new feature still takes an id above those the package out holds,
/// as the floor is kept after each line, not only once the script ends.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_scripts_later_line_never_takes_an_id_a_package_holds() {
    let temp = Temp::new("script-floor");
    let gpkg = roads_gpkg(&temp.0);
    let mut home = Session::new(crate::project::Project::default());
    home.path = Some(temp.0.join("home.rgis"));
    home.run("gpkg.addLayer", &json!({"path": gpkg, "table": "roads", "name": "R"})).unwrap();
    package(&mut home, &json!({"name": "Roads", "extent": [0.5, 19.0, 11.5, 21.0]})).unwrap();
    let r = layer(&home, "R");
    let floor = home.project.offline_areas[0].ids[&r];
    // The top road goes from the file too, so rereading it no longer shows the id the package holds.
    let rows = &home.map().layer(r).unwrap().dataset.features;
    let top = rows.iter().position(|f| f.id == floor).unwrap();
    home.run("select.rows", &json!({"layer": r, "rows": [top]})).unwrap();
    home.run("edit.delete", &Value::Null).unwrap();
    home.run("gpkg.saveEdits", &json!({"layer": "R"})).unwrap();
    let script = format!(
        "gpkg.discardEdits {{\"layer\": \"R\"}}\nedit.addFeature {{\"layer\": {r}, \"geometry\": {{\"type\": \"Point\", \"coordinates\": [5, 20]}}, \"attributes\": {{}}}}"
    );
    home.run("script.run", &json!({"script": script})).unwrap();
    let added = home.map().layer(r).unwrap().dataset.features.iter().map(|f| f.id).max().unwrap();
    assert!(added > floor, "the script's new feature took id {added}, at or below {floor}");
}

/// A layer removed on the phone is said on sending: its features go back unchanged.
#[test]
fn a_layer_removed_in_the_field_is_said_on_sending() {
    let temp = Temp::new("layer-removed");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let cities = layer(&a, "Cities");
    a.run("layer.remove", &json!({"layer": cities})).unwrap();
    let mark = a.notice_mark();
    package(&mut a, &Value::Null).unwrap();
    assert!(a.notices_since(mark).iter().any(|n| n.text.contains("removed from its map") && n.text.contains("Cities")), "{:?}", a.notices);
}

/// Keeping a package's rows, cancelled at each of its checks in turn: no folder is left behind,
/// half written or complete, and the next try keeps them whole. Whatever ends it, the project no
/// longer holds the rows for it, so an edit afterwards leaves no second copy held.
#[test]
fn keeping_the_rows_cancelled_at_each_check_leaves_no_folder() {
    let temp = Temp::new("keep-cancel");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": [9.0, 44.0, 11.0, 46.0]})).unwrap();
    let dir = temp.0.join("unpacked");
    let (text, _) = unpack_into(&out, &dir).unwrap();
    let project = project_in(&text, &dir).unwrap();
    let package_id = project.field_origin.as_ref().unwrap().package.clone();
    let root = crate::cmd::field_changes::test_bases::root().unwrap();
    let left = || std::fs::read_dir(&root).map(|d| d.count()).unwrap_or(0);
    for n in 1..=256 {
        let mut fresh = project.clone();
        let given = std::sync::Arc::new(crate::cmd::field_changes::GivenRows::of(&fresh, &package_id));
        let held = std::sync::Arc::downgrade(&given);
        fresh.given = Some(given);
        let mut s = Session::new(fresh);
        let read = crate::cmd::reads::read_ahead(&mut s, "offline.keepRows", &json!({})).unwrap();
        assert!(read.reads());
        let control = rustgis_analysis::control::Control::cancelled_at_look(n);
        let result = read.run(&control).and_then(|done| crate::cmd::reads::read_land(&mut s, done));
        assert!(held.upgrade().is_none() && !s.field_rows_waiting(), "{n}: the rows are still held for keeping");
        match result {
            Err(e) => {
                assert!(e.contains("cancelled"), "{n}: {e}");
                assert_eq!(left(), 0, "{n}: a folder was left behind");
            }
            Ok(_) => {
                assert!(root.join(&package_id).join("layers.json").is_file());
                assert_eq!(left(), 1);
                return;
            }
        }
    }
    panic!("keeping the rows looked at Cancel 256 times or more");
}

/// The list of changes a returning package carries.
fn changes_in(back: &[u8]) -> Option<Value> {
    let entries = rustgis_data::zip::read(back).unwrap();
    entries.iter().find(|(n, _)| n == crate::cmd::field_changes::ENTRY).map(|(_, b)| serde_json::from_slice(b).unwrap())
}

/// A field project saved and opened again (every coordinate and value through the file and back)
/// still finds the rows its package gave, and only the one feature edited in it is listed: every
/// other row, of points and of polygons, reads back as it was given.
#[test]
fn an_unedited_row_is_never_listed_as_changed_even_after_saving_and_opening_again() {
    let temp = Temp::new("unedited");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let saved = temp.0.join("field.rgis");
    a.run("project.saveAs", &json!({"path": saved})).unwrap();
    let mut a = Session::new(Project::default());
    a.run("project.open", &json!({"path": saved})).unwrap();
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    let id = a.map().layer(cities).unwrap().dataset.features[0].id;
    a.run("edit.setValue", &json!({"layer": cities, "row": 0, "field": name, "value": "Only this"})).unwrap();
    let mark = a.notice_mark();
    let (_, back) = package(&mut a, &Value::Null).unwrap();
    assert!(a.notices_since(mark).iter().all(|n| !n.text.contains("whole map")), "{:?}", a.notices);
    let changes = changes_in(&back).unwrap();
    let layers = changes["layers"].as_array().unwrap();
    assert_eq!(layers.len(), 1, "{changes}");
    assert_eq!(layers[0]["layer"].as_u64(), Some(u64::from(cities)));
    assert!(layers[0].get("added").is_none() && layers[0].get("deleted").is_none(), "{changes}");
    let edited = layers[0]["edited"].as_array().unwrap();
    assert_eq!(edited.len(), 1, "{changes}");
    assert_eq!(edited[0]["id"].as_u64(), Some(id));
    assert_eq!(edited[0]["values"].as_array().unwrap().len(), 1, "{changes}");
    assert!(edited[0].get("geometry").is_none(), "{changes}");
}

/// A phone that keeps no copy of the rows its package gave (they couldn't be kept, or the project
/// was opened on another device) sends the whole map, and says so; one that keeps them says
/// nothing of it.
#[test]
fn a_phone_without_the_rows_it_was_given_sends_the_whole_map_and_says_so() {
    let temp = Temp::new("no-base");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    let package_id = a.project.field_origin.as_ref().unwrap().package.clone();
    let cities = layer(&a, "Cities");
    let name = text_field(&a, cities);
    add_point(&mut a, cities, &name, "Captured", [10.0, 5.0]);
    let mark = a.notice_mark();
    let (_, kept) = package(&mut a, &Value::Null).unwrap();
    assert!(a.notices_since(mark).iter().all(|n| !n.text.contains("whole map")));
    assert!(changes_in(&kept).is_some());
    std::fs::remove_dir_all(crate::cmd::field_changes::test_bases::root().unwrap().join(&package_id)).unwrap();
    let mark = a.notice_mark();
    let (_, whole) = package(&mut a, &Value::Null).unwrap();
    let said: Vec<String> = a.notices_since(mark).iter().map(|n| n.text.clone()).collect();
    assert!(said.iter().any(|t| t.contains("whole map") && t.contains("doesn't keep the rows")), "{said:?}");
    assert!(changes_in(&whole).is_none());
    // At home it comes back as a whole map, while its area is as it was.
    let path = temp.0.join("whole.rgisfield");
    std::fs::write(&path, &whole).unwrap();
    let done = home.run("offline.bringBack", &json!({"path": path})).unwrap();
    assert_eq!(done["added"].as_u64(), Some(1), "{done}");
}

/// The crew deletes a feature the office has since given a value in a field it added. With a list
/// of changes the office's edit beats the deletion: the feature stays, and is said. As a whole
/// map, the deletion goes in, and the lost value is reported as a conflict.
#[test]
fn deleted_in_the_field_after_the_office_filled_its_new_field() {
    for whole in [false, true] {
        delete_after_office_filled_new_field(whole);
    }
}

/// [`deleted_in_the_field_after_the_office_filled_its_new_field`], `whole` with no rows kept.
fn delete_after_office_filled_new_field(whole: bool) {
    let temp = Temp::new("delete-new-field");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    if whole {
        let package_id = a.project.field_origin.as_ref().unwrap().package.clone();
        std::fs::remove_dir_all(crate::cmd::field_changes::test_bases::root().unwrap().join(package_id)).unwrap();
    }
    let cities = layer(&a, "Cities");
    let id = a.map().layer(cities).unwrap().dataset.features[0].id;
    a.run("select.rows", &json!({"layer": cities, "rows": [0]})).unwrap();
    a.run("edit.delete", &Value::Null).unwrap();
    let hc = layer(&home, "Cities");
    home.run("edit.addField", &json!({"layer": hc, "name": "inspected", "type": "text"})).unwrap();
    let row = home.map().layer(hc).unwrap().dataset.features.iter().position(|f| f.id == id).unwrap();
    home.run("edit.setValue", &json!({"layer": hc, "row": row, "field": "inspected", "value": "flagged"})).unwrap();
    let (done, said) = bring_back(&temp, &mut a, &mut home, "a");
    let (deleted, said_as) = if whole { (1, "replaced the map's changes") } else { (0, "so they were kept") };
    assert_eq!((done["deleted"].as_u64(), done["conflicts"].as_u64()), (Some(deleted), Some(1)), "{done}");
    assert!(said.iter().any(|t| t.contains(said_as) && t.contains(&format!("Cities #{id}"))), "{said:?}");
}

/// A layer added in the field, whose id a layer added at home has taken meanwhile: it isn't the
/// area's, so none of its fields or features reach the home layer of that id, and it is said.
#[test]
fn a_layer_added_in_the_field_stays_out_of_home_layers() {
    use base64::Engine;
    let point = |label: &str| {
        let text = format!(
            r#"{{"type":"FeatureCollection","features":[{{"type":"Feature","properties":{{"crew_only":"{label}"}},"geometry":{{"type":"Point","coordinates":[10,5]}}}}]}}"#
        );
        base64::engine::general_purpose::STANDARD.encode(text)
    };
    let temp = Temp::new("field-layer");
    let mut home = home(&temp);
    let (_, out) = package(&mut home, &json!({"name": "Day", "extent": AREA})).unwrap();
    let mut a = phone(&temp, &out, "a");
    a.run("layer.addDataBytes", &json!({"fileName": "crew.geojson", "data": point("crew"), "name": "Crew layer"})).unwrap();
    home.run("layer.addDataBytes", &json!({"fileName": "office.geojson", "data": point("office"), "name": "Office layer"})).unwrap();
    let (crew, office) = (layer(&a, "Crew layer"), layer(&home, "Office layer"));
    assert_eq!(crew, office, "both sides give their new layer the next id");
    let (done, said) = bring_back(&temp, &mut a, &mut home, "a");
    let ds = &home.map().layer(office).unwrap().dataset;
    assert_eq!(ds.features.len(), 1, "ids {crew} / {office}: {done}");
    assert_eq!(ds.fields.len(), 1, "ids {crew} / {office}");
    assert!(said.iter().any(|t| t.contains("skipped 1 layer(s) that aren't in offline area") && t.contains("Crew layer")), "{said:?}");
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
        package: String::new(),
        ids: Default::default(),
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
