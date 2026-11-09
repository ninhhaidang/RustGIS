//! Exercise Reload Data through the real confirmation, not source-code/wiring assertions.
#![cfg(not(target_arch = "wasm32"))]
use egui::{accesskit::Role, vec2};
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use rustgis_engine::{
    Project, Session,
    data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value, raster::Crs, source::gpkg},
};
use rustgis_ui_egui::RustGisApp;
use serde_json::json;

fn fixture() -> (tempfile::TempDir, Harness<'static, RustGisApp>, u32) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("places.gpkg");
    let mut data = Dataset::new("places", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    for x in [0.0, 1.0, 2.0] {
        data.push(Some(Geometry::Point(Coord::new(x, 20.0))), vec![Value::Text("original".into())]);
    }
    gpkg::write_in(&data, &path, Crs::LonLat).unwrap();
    let mut app = RustGisApp::new(Session::new(Project::default()));
    let id =
        app.run("gpkg.addLayer", json!({"path": path, "table": "places", "name": "My Places"})).unwrap()["layer"].as_u64().unwrap() as u32;
    app.run("edit.enable", json!({"on": true})).unwrap();
    app.run("edit.setValue", json!({"layer": id, "row": 0, "field": "name", "value": "pending"})).unwrap();
    let mut h = Harness::builder().with_size(vec2(1366.0, 900.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    h.run_steps(3);
    (dir, h, id)
}

fn search(h: &mut Harness<'_, RustGisApp>) {
    h.state_mut().ui.command_search = Some("layer.reloadData".into());
    h.run_steps(3);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, Some("reloadData"));
}

fn value(h: &Harness<'_, RustGisApp>, id: u32) -> Value {
    h.state().session.map().layer(id).unwrap().dataset.features[0].attrs[0].clone()
}

#[test]
fn dirty_search_counts_cancel_and_explicit_discard_target_the_captured_layer() {
    let (_dir, mut h, id) = fixture();
    h.state_mut().run("select.rows", json!({"layer": id, "rows": [2]})).unwrap();
    h.state_mut().run("edit.delete", json!({})).unwrap();
    h.state_mut()
        .run(
            "edit.addFeature",
            json!({"layer": id, "geometry": {"type": "Point", "coordinates": [4, 20]}, "attributes": {"name": "inserted"}}),
        )
        .unwrap();
    search(&mut h);
    h.get_by_label("Inserts: 1; updates: 1; deletes: 1.");
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, None);
    assert_eq!(value(&h, id), Value::Text("pending".into()));
    assert_eq!(h.state_mut().run("gpkg.pendingEdits", json!({"layer": id})).unwrap()["inserts"], 1);

    search(&mut h);
    // Selection can change while a confirmation is open, but never redirects its mutation.
    h.state_mut().run("layer.template", json!({"template": "points", "name": "Other"})).unwrap();
    let other = h.state().session.map().layer_by_name("Other").unwrap().id;
    h.state_mut().run("layer.select", json!({"layer": other})).unwrap();
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, "Discard Edits and Reload").click();
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, None);
    assert_eq!(value(&h, id), Value::Text("original".into()));
    assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features.len(), 3);
    assert_eq!(h.state().session.map().layer(other).unwrap().dataset.features.len(), 0);
    h.state_mut().run("edit.undo", json!({})).unwrap();
    assert_eq!(value(&h, id), Value::Text("pending".into()), "one undo restores the discarded in-memory edits");
}

#[test]
fn pending_counts_are_rechecked_while_confirmation_is_open() {
    let (_dir, mut h, id) = fixture();
    search(&mut h);
    h.get_by_label("Inserts: 0; updates: 1; deletes: 0.");
    h.state_mut().run("edit.setValue", json!({"layer": id, "row": 1, "field": "name", "value": "also pending"})).unwrap();
    h.run_steps(3);
    h.get_by_label("Inserts: 0; updates: 2; deletes: 0.");
    // Consent captures the state at dispatch, not the older state when the modal opened.
    h.get_by_role_and_label(Role::Button, "Discard Edits and Reload").click();
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, None);
    assert_eq!(value(&h, id), Value::Text("original".into()));
    assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features[1].attrs[0], Value::Text("original".into()));
    h.state_mut().run("edit.undo", json!({})).unwrap();
    assert_eq!(value(&h, id), Value::Text("pending".into()));
    assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features[1].attrs[0], Value::Text("also pending".into()));
    assert_eq!(h.state_mut().run("gpkg.pendingEdits", json!({"layer": id})).unwrap()["updates"], 2);
}

#[test]
fn stale_project_map_layer_and_source_cannot_discard_edits() {
    for change in ["project", "map", "layer", "path", "table"] {
        let (_dir, mut h, id) = fixture();
        search(&mut h);
        match change {
            "project" => {
                h.state_mut().session.opened += 1;
            }
            "map" => {
                // A copied map can reuse every layer ID and name, but not map identity.
                h.state_mut().run("map.new", json!({"name": "Other Map"})).unwrap();
            }
            "layer" => h.state_mut().session.map_mut().layer_mut(id).unwrap().renew_instance_id(),
            "path" => h.state_mut().session.map_mut().layer_mut(id).unwrap().gpkg.as_mut().unwrap().path.push_str(".replacement"),
            "table" => h.state_mut().session.map_mut().layer_mut(id).unwrap().gpkg.as_mut().unwrap().table.push_str("_replacement"),
            _ => unreachable!(),
        }
        // Queue the click against the old rendered confirmation, without an intervening frame.
        h.get_by_role_and_label(Role::Button, "Discard Edits and Reload").click();
        h.run_steps(3);
        let target = h.state().session.project.maps.iter().find_map(|map| map.layer_by_name("My Places")).unwrap();
        assert_eq!(target.dataset.features[0].attrs[0], Value::Text("pending".into()), "{change}");
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
    }
}

#[test]
fn failed_reload_keeps_confirmation_retryable_and_pending_edits_intact() {
    let (dir, mut h, id) = fixture();
    search(&mut h);
    let path = dir.path().join("places.gpkg");
    let saved = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"not a GeoPackage").unwrap();
    h.get_by_role_and_label(Role::Button, "Discard Edits and Reload").click();
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, Some("reloadData"));
    assert_eq!(value(&h, id), Value::Text("pending".into()));
    std::fs::write(&path, saved).unwrap();
    h.get_by_role_and_label(Role::Button, "Discard Edits and Reload").click();
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, None);
    assert_eq!(value(&h, id), Value::Text("original".into()));
}

#[test]
fn data_ribbon_opens_the_same_dirty_confirmation_at_1366_width() {
    let (_dir, mut h, id) = fixture();
    h.state_mut().run("ui.tab", json!({"tab": "Data"})).unwrap();
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, "Reload Data").click();
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, Some("reloadData"));
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, None);
    assert_eq!(value(&h, id), Value::Text("pending".into()));
}

#[test]
fn failed_reload_and_pre_dispatch_rejection_each_post_one_error() {
    let (dir, mut h, id) = fixture();
    h.state_mut().run("gpkg.discardEdits", json!({"layer": id})).unwrap();
    let before = h.state().session.undo_label().map(str::to_string);
    std::fs::write(dir.path().join("places.gpkg"), b"not a GeoPackage").unwrap();
    let mark = h.state().session.notice_mark();
    assert!(h.state_mut().run("ui.reloadData", json!({"layer": id})).is_err());
    assert_eq!(h.state().session.notices_since(mark).iter().filter(|notice| notice.error).count(), 1);
    assert_eq!(value(&h, id), Value::Text("original".into()));
    assert_eq!(h.state().session.undo_label(), before.as_deref());

    let mark = h.state().session.notice_mark();
    assert!(h.state_mut().run("ui.reloadData", json!({"layer": 9999})).is_err());
    assert_eq!(h.state().session.notices_since(mark).iter().filter(|notice| notice.error).count(), 1);
    assert_eq!(h.state().session.undo_label(), before.as_deref());
}

#[test]
fn dirty_reload_preserves_an_open_dialog_and_its_unsubmitted_input() {
    let (_dir, mut h, id) = fixture();
    h.state_mut().run("ui.dialog", json!({"name": "addDatabase"})).unwrap();
    h.state_mut().dialogs.text = "unsubmitted connection input".into();
    h.state_mut().dialogs.a = "unsubmitted table choice".into();
    assert!(h.state_mut().run("ui.reloadData", json!({"layer": id})).is_err());
    assert_eq!(h.state().dialogs.open, Some("addDatabase"));
    assert_eq!(h.state().dialogs.text, "unsubmitted connection input");
    assert_eq!(h.state().dialogs.a, "unsubmitted table choice");
    assert_eq!(value(&h, id), Value::Text("pending".into()));
}

#[test]
fn unsupported_snapshot_reload_stays_visible_but_disabled_in_command_search() {
    let (_dir, mut h, _) = fixture();
    h.state_mut().run("layer.template", json!({"template": "points", "name": "Snapshot"})).unwrap();
    let id = h.state().session.map().layer_by_name("Snapshot").unwrap().id;
    let before = h.state().session.undo_label().map(str::to_string);
    h.state_mut().ui.command_search = Some("layer.reloadData".into());
    h.run_steps(3);
    assert!(h.get_by_label_contains("Reload Data").accesskit_node().is_disabled());
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!(h.state().ui.command_search.is_some());
    assert_eq!(h.state().session.active_layer, Some(id));
    assert_eq!(h.state().session.undo_label(), before.as_deref());
}
