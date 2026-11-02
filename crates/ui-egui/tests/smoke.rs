//! Runs frames of the whole UI (no GPU) through every tab, pane, dialog and theme.

use egui_kittest::kittest::Queryable;
use rustgis_ui_egui::RustGisApp;
use serde_json::json;

#[cfg(not(target_arch = "wasm32"))]
mod remembered_database;

mod remembered_s3;
/// Builds a headless World Atlas UI for interaction tests.
fn harness() -> egui_kittest::Harness<'static, RustGisApp> {
    egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        RustGisApp::with_sample(),
    )
}

/// Runs a layer command through Command Search and checks that the search closes.
fn search_layer_command(h: &mut egui_kittest::Harness<'_, RustGisApp>, command: &str) {
    h.state_mut().ui.command_search = Some(command.into());
    for _ in 0..3 {
        h.step();
    }
    h.key_press(egui::Key::Enter);
    for _ in 0..3 {
        h.step();
    }
    assert!(h.state().ui.command_search.is_none());
}

/// Duplicate names must not redirect a source write or discard to the first matching layer.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn geopackage_edit_dialogs_target_numeric_ids_with_duplicate_names() {
    use rustgis_engine::data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value, raster::Crs, source::gpkg};

    let dir = tempfile::tempdir().unwrap();
    let mut h = harness();
    for table in ["alpha", "beta"] {
        let path = dir.path().join(format!("{table}.gpkg"));
        let mut data = Dataset::new(table, GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        for x in [0.0, 1.0] {
            data.push(Some(Geometry::Point(Coord::new(x, 0.0))), vec![Value::Text("original".into())]);
        }
        gpkg::write_in(&data, &path, Crs::LonLat).unwrap();
        let loaded = h.state_mut().run("gpkg.addLayer", json!({"path": path, "table": table})).unwrap();
        h.state_mut().run("layer.rename", json!({"layer": loaded["layer"], "name": "Duplicate"})).unwrap();
    }
    let ids: Vec<_> = h.state().session.map().layers.iter().filter(|l| l.gpkg.is_some()).map(|l| l.id).collect();
    let (upper, lower) = (ids[0], ids[1]);
    let source = |h: &egui_kittest::Harness<'_, RustGisApp>, id| {
        let edit = h.state().session.map().layer(id).unwrap().gpkg.as_ref().unwrap();
        (std::path::PathBuf::from(&edit.path), edit.table.clone())
    };
    let (upper_path, upper_table) = source(&h, upper);
    let (lower_path, lower_table) = source(&h, lower);
    let stored = |path: &std::path::Path, table: &str| {
        gpkg::edit::EditTable::open(path, table).unwrap().read_editable(100).unwrap().features[0].attrs[0].clone()
    };
    let pending = |h: &mut egui_kittest::Harness<'_, RustGisApp>, id| {
        h.state_mut().run("gpkg.pendingEdits", json!({"layer": id})).unwrap()["updates"].as_u64().unwrap()
    };
    let choose = |h: &mut egui_kittest::Harness<'_, RustGisApp>, from, to| {
        let from_label = format!("Duplicate (#{from})");
        h.query_all_by_role(egui::accesskit::Role::ComboBox).find(|n| n.value().as_deref() == Some(from_label.as_str())).unwrap().click();
        h.run_steps(2);
        h.get_by_label(&format!("Duplicate (#{to})")).click();
        h.run_steps(2);
        assert_eq!(h.state().dialogs.gpkg_layer, Some(to));
    };
    let shown_count = |h: &egui_kittest::Harness<'_, RustGisApp>, count: &str| {
        let label = h.get_by_label("Changed rows").rect();
        assert!(
            h.query_all_by_label(count).any(|n| n.rect().left() > label.right() && (n.rect().center().y - label.center().y).abs() < 3.0),
            "the Changed rows value must display {count}"
        );
    };
    h.state_mut().run("edit.enable", json!({"on": true})).unwrap();
    h.state_mut().run("edit.setValue", json!({"layer": upper, "row": 0, "field": "name", "value": "upper pending"})).unwrap();
    for row in [0, 1] {
        h.state_mut().run("edit.setValue", json!({"layer": lower, "row": row, "field": "name", "value": "lower saved"})).unwrap();
    }
    h.state_mut().run("layer.select", json!({"layer": lower})).unwrap();
    h.state_mut().run("ui.dialog", json!({"name": "gpkgSave"})).unwrap();
    h.run_steps(3);
    assert_eq!(h.state().dialogs.gpkg_layer, Some(lower), "the lower duplicate is active");
    h.get_by_label(&lower_table);
    assert_eq!(h.state().dialogs.list[2], "2");
    shown_count(&h, "2");
    choose(&mut h, lower, upper);
    h.get_by_label(&upper_table);
    assert_eq!(h.state().dialogs.list[2], "1", "same display name must still invalidate counts");
    shown_count(&h, "1");
    choose(&mut h, upper, lower);
    h.get_by_label(&lower_table);
    assert_eq!(h.state().dialogs.list[2], "2");
    shown_count(&h, "2");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save").click();
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, None, "{:?}", h.state().dialogs.error);
    assert_eq!(pending(&mut h, lower), 0);
    assert_eq!(pending(&mut h, upper), 1);
    assert_eq!(stored(&lower_path, &lower_table), Value::Text("lower saved".into()));
    assert_eq!(stored(&upper_path, &upper_table), Value::Text("original".into()));

    h.state_mut().run("edit.setValue", json!({"layer": lower, "row": 0, "field": "name", "value": "lower discarded"})).unwrap();
    h.state_mut().run("ui.dialog", json!({"name": "gpkgDiscard"})).unwrap();
    h.run_steps(3);
    assert_eq!(h.state().dialogs.gpkg_layer, Some(lower));
    h.get_by_label(&lower_table);
    choose(&mut h, lower, upper);
    h.get_by_label(&upper_table);
    choose(&mut h, upper, lower);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Discard").click();
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, None, "{:?}", h.state().dialogs.error);
    assert_eq!(pending(&mut h, lower), 0);
    assert_eq!(pending(&mut h, upper), 1);
    assert_eq!(h.state().session.map().layer(lower).unwrap().dataset.features[0].attrs[0], Value::Text("lower saved".into()));
    assert_eq!(h.state().session.map().layer(upper).unwrap().dataset.features[0].attrs[0], Value::Text("upper pending".into()));
    assert_eq!(stored(&lower_path, &lower_table), Value::Text("lower saved".into()));
    assert_eq!(stored(&upper_path, &upper_table), Value::Text("original".into()));

    // Losing the target must never fall back to the active layer or a same-name layer.
    for (dialog, button) in [("gpkgSave", "Save"), ("gpkgDiscard", "Discard")] {
        for target in [None, Some(u32::MAX)] {
            h.state_mut().run("ui.dialog", json!({"name": dialog})).unwrap();
            h.state_mut().dialogs.gpkg_layer = target;
            h.run_steps(3);
            assert!(h.state().dialogs.list.is_empty());
            h.get_by_role_and_label(egui::accesskit::Role::Button, button).click();
            h.run_steps(3);
            assert_eq!(h.state().dialogs.open, Some(dialog));
            assert!(h.state().dialogs.error.is_some());
            assert_eq!(pending(&mut h, upper), 1);
            assert_eq!(pending(&mut h, lower), 0);
            assert_eq!(stored(&upper_path, &upper_table), Value::Text("original".into()));
        }
    }
    h.state_mut().run("layer.select", json!({"layer": "Countries"})).unwrap();
    h.state_mut().run("ui.dialog", json!({"name": "gpkgSave"})).unwrap();
    assert_eq!(h.state().dialogs.gpkg_layer, Some(upper), "a non-editable active layer falls back to the first editable ID");
}

#[cfg(not(target_arch = "wasm32"))]
fn write_dialog_gpkg(path: &std::path::Path) {
    use rustgis_engine::data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value, raster::Crs, source::gpkg};
    let mut data = Dataset::new("roads", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    for x in [0.0, 1.0, 2.0] {
        data.push(Some(Geometry::Point(Coord::new(x, 20.0))), vec![Value::Text("original".into())]);
    }
    gpkg::write_in(&data, path, Crs::LonLat).unwrap();
}

/// Undo rewinds map-local IDs, not a dialog's ownership of a particular live layer.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn geopackage_edit_dialogs_refuse_reused_layer_ids() {
    use rustgis_engine::data::{Value, source::gpkg::edit::EditTable};

    for same_source in [false, true] {
        for intervening_frame in [false, true] {
            for (dialog, button) in [("gpkgSave", "Save"), ("gpkgDiscard", "Discard")] {
                let dir = tempfile::tempdir().unwrap();
                let a = dir.path().join("alias-a.gpkg");
                let b = dir.path().join("alias-b.gpkg");
                write_dialog_gpkg(&a);
                write_dialog_gpkg(&b);
                let mut h = harness();
                h.state_mut().run("project.new", json!({})).unwrap();
                h.state_mut().run("edit.enable", json!({"on": true})).unwrap();
                let id = h.state_mut().run("gpkg.addLayer", json!({"path": a, "table": "roads", "name": "R"})).unwrap()["layer"]
                    .as_u64()
                    .unwrap() as u32;
                assert_eq!(id, 1);
                let instance = h.state().session.map().layer(id).unwrap().instance_id();
                h.state_mut().run("ui.dialog", json!({"name": dialog})).unwrap();
                h.run_steps(3);
                assert_eq!(h.state().dialogs.list[4], a.to_string_lossy());
                assert_eq!(h.state().dialogs.gpkg_instance, Some(instance));

                h.state_mut().run("edit.undo", json!({})).unwrap();
                assert!(h.state().session.map().layer(id).is_none());
                let replacement = if same_source { &a } else { &b };
                let other_id =
                    h.state_mut().run("gpkg.addLayer", json!({"path": replacement, "table": "roads", "name": "R"})).unwrap()["layer"]
                        .as_u64()
                        .unwrap() as u32;
                assert_eq!(other_id, id);
                let other_instance = h.state().session.map().layer(id).unwrap().instance_id();
                assert_ne!(other_instance, instance);
                h.state_mut().run("edit.setValue", json!({"layer": id, "row": 0, "field": "name", "value": "wrong source saved"})).unwrap();
                if intervening_frame {
                    h.run_steps(3);
                    assert!(h.state().dialogs.list.is_empty(), "a replacement must not inherit the old source preview");
                }
                // With no intervening frame this clicks the original rendered confirmation.
                h.get_by_role_and_label(egui::accesskit::Role::Button, button).click();
                h.run_steps(3);
                assert_eq!(h.state().dialogs.open, Some(dialog));
                assert!(h.state().dialogs.error.is_some());
                assert_eq!(h.state().dialogs.gpkg_instance, Some(instance), "rendering must never adopt the replacement");
                assert!(h.state().dialogs.list.is_empty());
                assert_eq!(
                    h.state().session.map().layer(id).unwrap().dataset.features[0].attrs[0],
                    Value::Text("wrong source saved".into()),
                    "Discard must leave the replacement's pending edit intact"
                );
                assert_eq!(h.state_mut().run("gpkg.pendingEdits", json!({"layer": id})).unwrap()["updates"], 1);
                for path in [&a, &b] {
                    let stored = EditTable::open(path, "roads").unwrap().read_editable(10).unwrap();
                    assert_eq!(
                        stored.features[0].attrs[0],
                        Value::Text("original".into()),
                        "{button}: same_source={same_source}, intervening_frame={intervening_frame}, {path:?}"
                    );
                }
                h.state_mut().run("ui.dialog", json!({"name": dialog})).unwrap();
                h.run_steps(3);
                assert_eq!(h.state().dialogs.gpkg_instance, Some(other_instance));
                assert_eq!(h.state().dialogs.list[4], replacement.to_string_lossy());
                assert_eq!(h.state().dialogs.list[2], "1");
                assert!(h.state().dialogs.error.is_none());
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn geopackage_edit_dialog_counts_follow_edits_and_surviving_undo_snapshots() {
    use rustgis_engine::data::{Value, source::gpkg::edit::EditTable};

    for (dialog, button) in [("gpkgSave", "Save"), ("gpkgDiscard", "Discard")] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live-counts.gpkg");
        write_dialog_gpkg(&path);
        let mut h = harness();
        h.state_mut().run("project.new", json!({})).unwrap();
        h.state_mut().run("edit.enable", json!({"on": true})).unwrap();
        let id = h.state_mut().run("gpkg.addLayer", json!({"path": path, "table": "roads"})).unwrap()["layer"].as_u64().unwrap() as u32;
        let instance = h.state().session.map().layer(id).unwrap().instance_id();
        h.state_mut().run("ui.dialog", json!({"name": dialog})).unwrap();
        h.run_steps(3);
        assert_eq!(h.state().dialogs.list[2], "0");
        for row in [0, 1] {
            h.state_mut().run("edit.setValue", json!({"layer": id, "row": row, "field": "name", "value": "pending"})).unwrap();
            h.run_steps(3);
            assert_eq!(h.state().dialogs.list[2], (row + 1).to_string(), "open counts follow subsequent edits");
        }
        h.state_mut().run("edit.undo", json!({})).unwrap();
        h.run_steps(3);
        assert_eq!(h.state().dialogs.list[2], "1");
        assert_eq!(h.state().session.map().layer(id).unwrap().instance_id(), instance);
        assert!(h.state().dialogs.error.is_none());
        h.state_mut().run("edit.redo", json!({})).unwrap();
        h.run_steps(3);
        assert_eq!(h.state().dialogs.list[2], "2");
        assert_eq!(h.state().session.map().layer(id).unwrap().instance_id(), instance);
        assert!(h.state().dialogs.error.is_none());
        let revision = h.state().session.revision;
        h.run_steps(3);
        assert_eq!(h.state().session.revision, revision, "pendingEdits must not invalidate its own count cache");
        // Undoing the layer's addition makes the target unavailable; Redo restores the same
        // instance, and the stale refusal must go with it rather than sit beside valid counts.
        for _ in 0..3 {
            h.state_mut().run("edit.undo", json!({})).unwrap();
        }
        h.run_steps(3);
        assert!(h.state().dialogs.error.as_deref().is_some_and(|e| e.contains("no longer available")), "{:?}", h.state().dialogs.error);
        for _ in 0..3 {
            h.state_mut().run("edit.redo", json!({})).unwrap();
        }
        h.run_steps(3);
        assert_eq!(h.state().session.map().layer(id).unwrap().instance_id(), instance);
        assert_eq!(h.state().dialogs.error, None, "the restored target's stale refusal must clear");
        assert_eq!(h.state().dialogs.list[2], "2");

        h.get_by_role_and_label(egui::accesskit::Role::Button, "Cancel").click();
        h.run_steps(3);
        h.state_mut().run("ui.dialog", json!({"name": dialog})).unwrap();
        h.run_steps(3);
        assert_eq!(h.state().dialogs.list[2], "2", "reopening counts the current state");
        h.get_by_role_and_label(egui::accesskit::Role::Button, button).click();
        h.run_steps(3);
        assert_eq!(h.state().dialogs.open, None, "{:?}", h.state().dialogs.error);
        let expected = Value::Text(if button == "Save" { "pending" } else { "original" }.into());
        let stored = EditTable::open(&path, "roads").unwrap().read_editable(10).unwrap();
        for row in [0, 1] {
            assert_eq!(stored.features[row].attrs[0], expected);
            assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features[row].attrs[0], expected);
        }
        assert_eq!(h.state_mut().run("gpkg.pendingEdits", json!({"layer": id})).unwrap()["updates"], 0);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn geopackage_edit_dialogs_refuse_map_and_project_identity_changes() {
    use rustgis_engine::data::{Value, source::gpkg::edit::EditTable};
    for project_switch in [false, true] {
        for (dialog, button) in [("gpkgSave", "Save"), ("gpkgDiscard", "Discard")] {
            let dir = tempfile::tempdir().unwrap();
            let a = dir.path().join("a.gpkg");
            let b = dir.path().join("b.gpkg");
            write_dialog_gpkg(&a);
            write_dialog_gpkg(&b);
            let mut h = harness();
            h.state_mut().run("map.new", json!({"name": "Editable A"})).unwrap();
            let id = h.state_mut().run("gpkg.addLayer", json!({"path": a, "table": "roads", "name": "R"})).unwrap()["layer"]
                .as_u64()
                .unwrap() as u32;
            h.state_mut().run("edit.enable", json!({"on": true})).unwrap();
            h.state_mut().run("edit.setValue", json!({"layer": id, "row": 0, "field": "name", "value": "A pending"})).unwrap();
            let project = dir.path().join("other.rgis");
            if project_switch {
                // Same map index, map name and layer ID: only the opened-project generation differs.
                let mut other = h.state().session.project.clone();
                let index = other.active_map;
                let layer = other.maps[index].layer_mut(id).unwrap();
                layer.gpkg.as_mut().unwrap().path = b.to_string_lossy().into_owned();
                layer.dataset.features[0].attrs[0] = Value::Text("B pending".into());
                let mut session = rustgis_engine::Session::new(other);
                session.run("project.saveAs", &json!({"path": project})).unwrap();
            } else {
                h.state_mut().run("map.new", json!({"name": "Editable B"})).unwrap();
                let other_id = h.state_mut().run("gpkg.addLayer", json!({"path": b, "table": "roads", "name": "R"})).unwrap()["layer"]
                    .as_u64()
                    .unwrap() as u32;
                assert_eq!(id, other_id, "layer IDs collide across maps");
                h.state_mut().run("edit.enable", json!({"on": true})).unwrap();
                h.state_mut().run("edit.setValue", json!({"layer": id, "row": 0, "field": "name", "value": "B pending"})).unwrap();
                h.state_mut().run("map.activate", json!({"name": "Editable A"})).unwrap();
            }
            h.state_mut().run("ui.dialog", json!({"name": dialog})).unwrap();
            h.run_steps(3);
            assert_eq!(h.state().dialogs.list[4], a.to_string_lossy());
            if project_switch {
                h.state_mut().run("project.open", json!({"path": project})).unwrap();
            } else {
                h.state_mut().run("map.activate", json!({"name": "Editable B"})).unwrap();
            }
            h.run_steps(3);
            assert!(h.state().dialogs.list.is_empty(), "the old source preview must disappear");
            h.get_by_role_and_label(egui::accesskit::Role::Button, button).click();
            h.run_steps(3);
            assert_eq!(h.state().dialogs.open, Some(dialog));
            assert!(h.state().dialogs.error.is_some());
            assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features[0].attrs[0], Value::Text("B pending".into()));
            assert_eq!(h.state_mut().run("gpkg.pendingEdits", json!({"layer": id})).unwrap()["updates"], 1);
            for path in [&a, &b] {
                let stored = EditTable::open(path, "roads").unwrap().read_editable(10).unwrap();
                assert_eq!(stored.features[0].attrs[0], Value::Text("original".into()), "{project_switch}: {button}: {path:?}");
            }
            h.state_mut().run("ui.dialog", json!({"name": dialog})).unwrap();
            h.run_steps(3);
            assert_eq!(h.state().dialogs.list[4], b.to_string_lossy(), "reopening targets the current owner");
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn geopackage_load_dialog_rejects_fractional_row_limits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rows.gpkg");
    write_dialog_gpkg(&path);
    let mut h = harness();
    h.state_mut().run("ui.dialog", json!({"name": "gpkgAddLayer"})).unwrap();
    h.state_mut().dialogs.a = path.to_string_lossy().into_owned();
    h.state_mut().dialogs.c = "roads".into();
    for input in ["1.5", "0.5", "-1", "1,5", "1,,000", "12,34"] {
        h.state_mut().dialogs.e = input.into();
        h.run_steps(3);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Load").click();
        h.run_steps(3);
        assert_eq!(h.state().dialogs.open, Some("gpkgAddLayer"));
        assert!(h.state().dialogs.error.is_some());
        assert!(h.state().session.map().layers.iter().all(|l| l.gpkg.is_none()));
    }
    h.state_mut().dialogs.e = "1".into();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Load").click();
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, None, "{:?}", h.state().dialogs.error);
    let layer = h.state().session.map().layers.iter().find(|l| l.gpkg.is_some()).unwrap();
    assert_eq!(layer.dataset.features.len(), 1);
    assert_eq!(
        layer.dataset.features[0].geometry,
        Some(rustgis_engine::data::Geometry::Point(rustgis_engine::data::Coord::new(0.0, 20.0)))
    );
    h.state_mut().run("ui.dialog", json!({"name": "gpkgAddLayer"})).unwrap();
    h.state_mut().dialogs.a = path.to_string_lossy().into_owned();
    h.state_mut().dialogs.c = "roads".into();
    h.state_mut().dialogs.e = "1,000".into();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Load").click();
    h.run_steps(3);
    assert_eq!(h.state().dialogs.open, None, "{:?}", h.state().dialogs.error);
    let layer = h.state().session.map().layers.iter().find(|l| l.gpkg.is_some()).unwrap();
    assert_eq!(layer.dataset.features.len(), 3, "valid thousands grouping must still load all available rows");
}

/// Both No Fill controls preserve unique-value styling and undo, and stay hidden for points.
#[test]
fn polygon_no_fill_is_reachable_from_the_pane_and_the_ribbon() {
    for pane in [true, false] {
        let mut h = harness();
        h.state_mut().run("layer.select", json!({"layer": "Countries"})).unwrap();
        h.state_mut().run("layer.symbology", json!({"type": "unique", "field": "continent"})).unwrap();
        h.state_mut().run("layer.outline", json!({"color": "#2E6B4F", "width": 2})).unwrap();
        let symbol =
            |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.map().layer_by_name("Countries").unwrap().renderer.base_symbol();
        let before = symbol(&h);
        if pane {
            h.state_mut().run("ui.pane.symbology", json!(null)).unwrap();
        } else {
            h.state_mut().ui.tab = "Style".into();
        }
        h.run_steps(6);
        h.get_by_label("No Fill").click();
        h.run_steps(3);
        let after = symbol(&h);
        assert_eq!(after.fill.a(), 0);
        assert_eq!(after.stroke, before.stroke);
        assert_eq!(after.stroke_width, before.stroke_width);
        assert_eq!(h.state().session.map().layer_by_name("Countries").unwrap().renderer.kind_label(), "Unique Values");
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(symbol(&h), before);
        h.state_mut().run("layer.select", json!({"layer": "Cities"})).unwrap();
        h.run_steps(3);
        assert!(h.query_by_label("No Fill").is_none(), "points must keep their fill");
    }
}

#[test]
fn command_search_adds_tiles_only_after_valid_dialog_submission() {
    let mut h = harness();
    search_layer_command(&mut h, "map.addTiles");
    assert_eq!(h.state().dialogs.open, Some("addTiles"));
    assert!(h.state().session.map().tiles.is_empty());
    assert!(h.state().dialogs.error.is_none(), "opening the dialog must not submit it");
    h.get_by_label("Add").click();
    h.step();
    assert_eq!(h.state().dialogs.open, Some("addTiles"));
    assert!(h.state().dialogs.error.is_some());
    assert!(h.state().session.map().tiles.is_empty());
    h.state_mut().dialogs.text = "https://example.com/{z}/{x}/{y}.png".into();
    h.state_mut().dialogs.a = "My tiles".into();
    h.step();
    h.get_by_label("Add").click();
    h.step();
    let tiles = &h.state().session.map().tiles;
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].name, "My tiles");
    assert_eq!(tiles[0].url, "https://example.com/{z}/{x}/{y}.png");
    assert_eq!(h.state().dialogs.open, None);
}

#[test]
fn command_search_basemap_gallery_selects_and_can_open_tile_dialog() {
    let mut h = harness();
    search_layer_command(&mut h, "map.basemap");
    h.get_by_label("Midnight").click();
    h.step();
    assert_eq!(h.state().session.map().basemap.code(), "midnight");
    assert_eq!(h.state().dialogs.open, None);
    search_layer_command(&mut h, "map.basemap");
    let pos = h.get_by_label("Web Basemaps").rect().center();
    h.event(egui::Event::PointerMoved(pos));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -800.0),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
        source: egui::MouseWheelSource::Wheel,
    });
    for _ in 0..6 {
        h.step();
    }
    h.get_by_label("Add Tile Layer…").click();
    h.step();
    assert_eq!(h.state().dialogs.open, Some("addTiles"), "the gallery must not close the new dialog");
    for _ in 0..3 {
        h.step();
    }
    h.get_by_label("Cancel").click();
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(h.state().dialogs.open, None);
    assert_eq!(h.state().session.map().basemap.code(), "midnight");
    assert!(h.state().session.map().tiles.is_empty());
}

#[test]
fn command_search_celestial_body_opens_body_picker() {
    let mut h = harness();
    search_layer_command(&mut h, "map.body");
    assert_eq!(h.state().dialogs.open, Some("body"));
    assert!(h.state().session.map().body().is_earth(), "opening the picker must not change the body");
    h.get_by_label("Moon").click();
    h.step();
    assert_eq!(h.state().session.map().body().id, "moon");
    assert_eq!(h.state().dialogs.open, None);
}

#[test]
fn command_search_adds_sample_template_and_encoded_polyline_layers() {
    let mut h = harness();
    h.state_mut().run("project.new", json!(null)).unwrap();
    search_layer_command(&mut h, "layer.addSample");
    h.get_by_label("Major Lakes").click();
    h.step();
    assert!(h.state().session.map().layer_by_name("Lakes").is_some());
    assert_eq!(h.state().dialogs.open, None);
    search_layer_command(&mut h, "layer.template");
    h.get_by_label("Points").click();
    h.step();
    let points = h.state().session.active_layer.and_then(|id| h.state().session.map().layer(id)).unwrap();
    assert_eq!(points.geometry_type(), rustgis_ui_egui::rustgis_engine::data::GeometryType::Point);
    assert_eq!(h.state().dialogs.open, None);
    search_layer_command(&mut h, "layer.addEncodedPolyline");
    h.state_mut().dialogs.text = "_p~iF~ps|U_ulLnnqC_mqNvxq`@".into();
    h.state_mut().dialogs.a = "Route".into();
    h.step();
    h.get_by_label("Add").click();
    h.step();
    let route = h.state().session.map().layer_by_name("Route").unwrap();
    let min_x = route.dataset.features[0].geometry.as_ref().unwrap().extent().min_x;
    assert!((min_x - -126.453).abs() < 1e-9, "{min_x}");
    assert_eq!(h.state().dialogs.open, None);
}

#[test]
fn command_search_collects_rename_and_field_inputs_before_changing_layer() {
    let mut h = harness();
    let id = h.state().session.active_layer.unwrap();
    let old_name = h.state().session.map().layer(id).unwrap().name.clone();
    search_layer_command(&mut h, "layer.rename");
    assert_eq!(h.state().session.map().layer(id).unwrap().name, old_name);
    h.state_mut().dialogs.text = "Renamed countries".into();
    h.step();
    h.get_by_label("Cancel").click();
    h.step();
    assert_eq!(h.state().session.map().layer(id).unwrap().name, old_name);
    search_layer_command(&mut h, "layer.rename");
    h.state_mut().dialogs.text = "Renamed countries".into();
    h.step();
    h.get_by_label("Rename").click();
    h.step();
    assert_eq!(h.state().session.map().layer(id).unwrap().name, "Renamed countries");
    search_layer_command(&mut h, "edit.addField");
    assert!(!h.state().session.map().layer(id).unwrap().dataset.fields.iter().any(|f| f.name == "score"));
    h.state_mut().dialogs.a = "score".into();
    h.step();
    h.get_by_label("Add").click();
    h.step();
    assert!(h.state().session.map().layer(id).unwrap().dataset.fields.iter().any(|f| f.name == "score"));
    assert_eq!(h.state().dialogs.open, None);
}

#[test]
fn command_search_required_inputs_without_routes_do_not_execute() {
    let mut h = harness();
    for command in ["map.goToLink", "group.move"] {
        let mark = h.state().session.notice_mark();
        h.state_mut().ui.command_search = Some(command.into());
        for _ in 0..3 {
            h.step();
        }
        h.key_press(egui::Key::Enter);
        h.step();
        assert!(h.state().ui.command_search.is_some(), "{command} must not run without input");
        assert!(h.state().session.notices_since(mark).is_empty(), "{command} must not fail into a toast");
        h.key_press(egui::Key::Escape);
        h.step();
    }
}

#[test]
fn command_search_workflow_collects_inputs_before_georeferencing() {
    let mut h = harness();
    let grid = "bmNvbHMgMgpucm93cyAyCnhsbGNvcm5lciAwCnlsbGNvcm5lciAwCmNlbGxzaXplIDEKTk9EQVRBX3ZhbHVlIC05OTk5CjEgMgozIDQK";
    let result = h.state_mut().run("layer.addDataBytes", json!({"fileName":"grid.asc", "data":grid})).unwrap();
    let id = result["layer"].as_u64().unwrap() as u32;
    let before = h.state().session.map().layer(id).unwrap().extent();
    search_layer_command(&mut h, "raster.georeference");
    assert_eq!(h.state().ui.right, Some(rustgis_ui_egui::RightPane::Workflows));
    assert_eq!(h.state().session.map().layer(id).unwrap().extent(), before);
    h.get_by_label("Fit To Map View").click();
    h.step();
    assert!(h.state().workflow.error.is_none(), "{:?}", h.state().workflow.error);
    assert_ne!(h.state().session.map().layer(id).unwrap().extent(), before);
    // Opening the same workflow again must keep it visible, not toggle its pane closed.
    search_layer_command(&mut h, "raster.georeference");
    assert_eq!(h.state().ui.right, Some(rustgis_ui_egui::RightPane::Workflows));
    search_layer_command(&mut h, "Classification Wizard");
    assert_eq!(h.state().workflow.section, "classify");
    assert_eq!(h.state().ui.right, Some(rustgis_ui_egui::RightPane::Workflows));
    // A section id matches no command row, so the workflow's own hit must stay listed.
    search_layer_command(&mut h, "deeplearning");
    assert_eq!(h.state().workflow.section, "deepLearning");
    assert_eq!(h.state().ui.right, Some(rustgis_ui_egui::RightPane::Workflows));
}

/// Clipping follows a selection until the user explicitly opts out, even after changing it.
#[test]
fn polygon_clip_defaults_to_selection_and_honors_opt_out() {
    let mut h = harness();
    h.state_mut().workflow.layer2 = "Countries".into();
    h.state_mut().run("ui.pane.workflows", json!(null)).unwrap();
    h.state_mut().workflow.section = "process";
    h.state_mut().session.map_mut().layers.iter_mut().find(|l| l.name == "Countries").unwrap().selection.push(0);
    h.run_steps(4);
    assert_eq!(h.state().workflow.selected_only, None);
    let total = h.state().session.map().layer_by_name("Countries").unwrap().dataset.features.len();
    assert!(h.query_by_label(&format!("(1 of {total})")).is_some());
    h.get_by_label("Selected only").click();
    h.run_steps(3);
    assert_eq!(h.state().workflow.selected_only, Some(false));
    h.state_mut().session.map_mut().layers.iter_mut().find(|l| l.name == "Countries").unwrap().selection.push(1);
    h.run_steps(3);
    assert_eq!(h.state().workflow.selected_only, Some(false));
    assert!(h.query_by_label(&format!("(2 of {total})")).is_some());
    h.get_by_label("Selected only").click();
    h.run_steps(3);
    assert_eq!(h.state().workflow.selected_only, Some(true));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn mcp_relay_early_start_wakes_queued_request_and_updates_ui() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::time::{Duration, Instant};

    let mut app = RustGisApp::with_sample();
    app.run("ui.mcpRelay", json!({"port": 0})).expect("start relay before first UI context");
    let port = app.relay.as_ref().expect("relay started").port();
    assert!(app.run("ui.mcpRelay", json!({"port": port})).is_ok());
    assert!(app.run("ui.mcpRelay", json!({})).is_ok());
    let different = if port == 1 { 2 } else { 1 };
    let err = app.run("ui.mcpRelay", json!({"port": different})).expect_err("different port must be refused");
    assert!(err.contains("stop it first"), "{err}");

    let token = rustgis_ui_egui::rustgis_assistant::relay::read_token(port).expect("relay token");
    let mut client = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))).expect("connect to relay");
    client.set_read_timeout(Some(Duration::from_millis(100))).expect("bounded reply poll");
    writeln!(client, "{}", json!({"jsonrpc":"2.0","method":"rustgis/attach","params":{"access":false,"token":token}})).expect("attach");
    writeln!(client, "{}", json!({"jsonrpc":"2.0","id":1,"method":"ping"})).expect("queue ping");
    client.flush().expect("flush request");

    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    let mut input = BufReader::new(client.try_clone().expect("clone stream"));
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut reply = String::new();
    loop {
        h.step();
        match input.read_line(&mut reply) {
            Ok(0) => panic!("relay closed before replying"),
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                assert!(Instant::now() < deadline, "queued request was not executed");
                reply.clear();
            }
            Err(e) => panic!("reading relay reply: {e}"),
        }
    }
    let value: serde_json::Value = serde_json::from_str(&reply).expect("JSON-RPC reply");
    assert_eq!(value["id"], 1);
    assert_eq!(value["result"], json!({}));

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        h.step();
        let attached = h.state().relay.as_ref().map(|r| r.attached_clients());
        if attached == Some(1) && h.query_by_label("MCP relay · 1 attached").is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "attached state {attached:?} was not rendered as 1 attached");
        std::thread::yield_now();
    }
    assert_eq!(h.state().relay.as_ref().map(|r| r.attached_clients()), Some(1));
    assert!(h.query_by_label("MCP relay · 1 attached").is_some(), "authenticated-client indicator rendered");

    client.shutdown(std::net::Shutdown::Both).expect("disconnect attached client");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        h.step();
        let attached = h.state().relay.as_ref().map(|r| r.attached_clients());
        if attached == Some(0) && h.query_by_label("MCP relay · 0 attached").is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "disconnected state {attached:?} was not rendered as 0 attached");
        std::thread::yield_now();
    }
    assert_eq!(h.state().relay.as_ref().map(|r| r.attached_clients()), Some(0));
    assert!(h.query_by_label("MCP relay · 0 attached").is_some(), "disconnect repaints the count");
    h.get_by_label("MCP relay · 0 attached").click();
    h.step();
    assert!(h.state().relay.is_none(), "clicking indicator stops relay");
    h.step();
    assert!(h.query_by_label("MCP relay · 0 attached").is_none());
}

/// Check the actual displayed rows, not just the data that feeds the contents pane.
fn assert_child_order(h: &mut egui_kittest::Harness<'_, RustGisApp>, labels: &[&str]) {
    for _ in 0..3 {
        h.step();
    }
    for pair in labels.windows(2) {
        let before = h.get_by_label(pair[0]).rect();
        let after = h.get_by_label(pair[1]).rect();
        assert!(before.max.y <= after.min.y, "{} must precede {}", pair[0], pair[1]);
    }
}

#[test]
fn nested_children_from_kml_kmz_and_categorized_files_are_sorted() {
    let mut kml = String::from(r#"<kml xmlns="http://www.opengis.net/kml/2.2"><Document>"#);
    for (i, (style, color, name)) in
        [("Zulu", "ff0000ff", "Last"), ("alpha", "ff00ff00", "First"), ("Bravo", "ffff0000", "Middle")].iter().enumerate()
    {
        kml.push_str(&format!("<Style id=\"{style}\"><IconStyle><color>{color}</color></IconStyle></Style>"));
        kml.push_str(&format!(
            "<Placemark><name>{name}</name><styleUrl>#{style}</styleUrl><Point><coordinates>{},50</coordinates></Point></Placemark>",
            10 + i
        ));
    }
    kml.push_str("</Document></kml>");
    let kml = kml.into_bytes();
    let kmz = rustgis_engine::data::zip::write(&[("doc.kml".into(), kml.to_vec())]).unwrap();
    let csv = b"lon,lat,category\n10,50,Zulu\n11,50,alpha\n12,50,Bravo\n";
    let geojson = serde_json::to_vec(&json!({"type":"FeatureCollection", "features":
        (["Zulu", "alpha", "Bravo"].map(|name| json!({"type":"Feature", "properties":{"category":name},
            "geometry":{"type":"Point","coordinates":[10,50]}})))}))
    .unwrap();
    for (file, bytes, categorize) in [
        ("children.kml", kml.as_slice(), false),
        ("children.kmz", kmz.as_slice(), false),
        ("children.csv", csv.as_slice(), true),
        ("children.geojson", geojson.as_slice(), true),
    ] {
        let mut h = harness();
        h.state_mut().run("project.new", json!(null)).unwrap();
        h.state_mut().open_picked(file, bytes).unwrap();
        assert_eq!(h.state().session.map().layers.len(), 1, "{file}");
        let id = h.state().session.map().layers[0].id;
        if categorize {
            h.state_mut().run("layer.symbology", json!({"layer":id,"type":"unique","field":"category"})).unwrap();
        }
        // Every feature has a class, so the unused "<all other values>" row is left out (#207).
        assert_child_order(&mut h, &["alpha", "Bravo", "Zulu"]);
        assert!(h.query_by_label("<all other values>").is_none(), "{file}");
        // Collapsing and reopening uses the same ordering.
        h.state_mut().run("layer.expand", json!({"layer":id,"on":false})).unwrap();
        h.step();
        h.state_mut().run("layer.expand", json!({"layer":id,"on":true})).unwrap();
        assert_child_order(&mut h, &["alpha", "Bravo", "Zulu"]);
    }
}

#[test]
fn nested_ogc_children_sort_on_import_rename_refresh_and_reopen() {
    use ogc_server::{fixture_features, fixture_group_collections, fixture_lines};
    let mut h = harness();
    h.state_mut().run("project.new", json!(null)).unwrap();
    // Use supplied fixture documents/responses: no live service or network needed.
    let params = |app: &mut RustGisApp, names: &[&str]| {
        let fixture: serde_json::Value = serde_json::from_str(fixture_group_collections()).unwrap();
        let collections: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let mut collection = fixture["collections"][i % 2].clone();
                collection["id"] = json!(format!("child-{i}"));
                collection["title"] = json!(name);
                collection["links"][0]["href"] = json!(format!("collections/child-{i}/items"));
                collection
            })
            .collect();
        let mut p = json!({"url":"https://example.org/collections", "text":json!({"collections":collections}).to_string()});
        let plan = app.run("service.groupPlan", p.clone()).unwrap();
        p["responses"] = json!(
            plan["layers"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, l)| { json!({"url":l["offer"]["url"],"text":if i % 2 == 0 { fixture_features() } else { fixture_lines() }}) })
                .collect::<Vec<_>>()
        );
        p
    };
    let mut p = params(h.state_mut(), &["Zulu", "alpha", "Bravo", "ALPHA"]);
    h.state_mut().run("service.import", p.clone()).unwrap();
    let group = h.state().session.map().service_groups[0].id;
    let child = h.state().session.map().layer_by_name("Zulu").unwrap().id;
    let draw_order: Vec<_> = h.state().session.map().layers.iter().map(|l| l.id).collect();
    assert_child_order(&mut h, &["My OGC Layer", "alpha", "ALPHA (2)", "Bravo", "Zulu"]);
    assert_eq!(h.state().session.map().layers.iter().map(|l| l.id).collect::<Vec<_>>(), draw_order);
    h.state_mut().run("layer.rename", json!({"layer":child,"name":"aardvark"})).unwrap();
    assert_child_order(&mut h, &["aardvark", "alpha", "ALPHA (2)", "Bravo"]);
    p["layer"] = json!(child);
    h.state_mut().run("service.refresh", p).unwrap();
    assert_child_order(&mut h, &["aardvark", "alpha", "ALPHA (2)", "Bravo"]);
    let mut p = params(h.state_mut(), &["Zulu", "alpha", "Bravo", "ALPHA", "beta"]);
    p["group"] = json!(group);
    h.state_mut().run("service.refresh", p).unwrap();
    assert_child_order(&mut h, &["aardvark", "alpha", "ALPHA (2)", "beta", "Bravo"]);
    let bytes = serde_json::to_vec(&h.state().session.project).unwrap();
    h.state_mut().run("project.new", json!(null)).unwrap();
    h.state_mut().open_picked("children.rgis", &bytes).unwrap();
    assert_child_order(&mut h, &["My OGC Layer", "aardvark", "alpha", "ALPHA (2)", "beta", "Bravo"]);
}

#[test]
fn every_tab_pane_and_dialog_renders() {
    let mut h = harness();
    // Every language is shown: no face in the monospace chain has some of their letters (ə).
    h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
    for _ in 0..4 {
        h.step();
    }
    for tab in ["Home", "Data", "Style", "Labeling", "Edit", "Analyze", "View", "Publish", "Plugins", "Settings", "Help"] {
        h.state_mut().ui.tab = tab.into();
        h.run();
    }
    for (id, p) in [
        ("ui.pane.symbology", json!(null)),
        ("ui.pane.labeling", json!({"layer": "Cities"})),
        ("ui.pane.geoprocessing", json!(null)),
        ("ui.gp.open", json!({"tool": "buffer"})),
        ("ui.pane.history", json!(null)),
        ("ui.pane.details", json!(null)),
        ("ui.pane.catalog", json!(null)),
        ("table.open", json!(null)),
        ("ui.dialog", json!({"name": "selectByAttributes"})),
        ("ui.dialog", json!({"name": "goToXY"})),
        ("ui.dialog", json!({"name": "calculateField"})),
        ("ui.connection", json!({"kind": "s3"})),
        ("ui.commandSearch", json!(null)),
        ("ui.about", json!(null)),
        ("ui.shortcuts", json!(null)),
        ("ui.theme", json!({"dark": true})),
        ("ui.backstage", json!({"open": true})),
    ] {
        h.state_mut().run(id, p).unwrap_or_else(|e| panic!("{id}: {e}"));
        h.run();
    }
    h.state_mut().ui.backstage = false;
    // Every language, the collapsed and peeked ribbon.
    for l in rustgis_ui_egui::i18n::Lang::all() {
        h.state_mut().run("ui.language", json!({"code": l.code()})).unwrap();
        h.run();
    }
    h.state_mut().run("ui.language", json!({"code": "auto"})).unwrap();
    assert!(h.state_mut().run("ui.language", json!({"code": "xx"})).is_err());
    h.state_mut().run("ui.ribbon", json!({"minimized": true})).unwrap();
    h.state_mut().ui.ribbon_peek = true;
    h.step();
    h.state_mut().run("ui.ribbon", json!({"minimized": false})).unwrap();
    h.step();
    for kind in ["unique", "graduated", "single"] {
        h.state_mut().run("layer.symbology", json!({"layer": "Countries", "type": kind, "field": "continent"})).ok();
        h.step();
    }
    h.state_mut().run("layer.select", json!({"layer": "Cities"})).unwrap();
    h.state_mut().run("layer.symbology", json!({"type": "proportional", "field": "pop_max"})).unwrap();
    h.state_mut().run("project.new", json!(null)).unwrap();
    for _ in 0..4 {
        h.step();
    }
    assert!(h.state().session.map().layers.is_empty());
}

/// Plugins are loaded for the whole process, so the tests that look at them take turns.
static PLUGINS: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn a_ribbon_wider_than_the_window_scrolls_with_arrow_buttons() {
    let mut h = harness();
    h.set_size(egui::vec2(900.0, 600.0));
    h.state_mut().ui.tab = "View".into();
    for _ in 0..4 {
        h.step();
    }
    assert!(h.query_by_label("Scroll the ribbon left").is_none());
    h.get_by_label("Scroll the ribbon right").click();
    for _ in 0..4 {
        h.step();
    }
    let _ = h.get_by_label("Scroll the ribbon left");
    // A window wide enough for every group shows no arrows.
    h.set_size(egui::vec2(3000.0, 600.0));
    h.state_mut().ui.tab = "Help".into();
    for _ in 0..4 {
        h.step();
    }
    assert!(h.query_by_label("Scroll the ribbon right").is_none());
    assert!(h.query_by_label("Scroll the tabs right").is_none());
}

#[test]
fn a_tab_strip_wider_than_the_window_scrolls_with_arrow_buttons() {
    let mut h = harness();
    // Narrower than the tabs' text alone, whatever the system font: at 560 points the tabs just
    // fit on macOS once its semibold is drawn heavier (#591), so they didn't need to scroll.
    h.set_size(egui::vec2(440.0, 500.0));
    for _ in 0..4 {
        h.step();
    }
    assert!(h.query_by_label("Scroll the tabs left").is_none());
    h.get_by_label("Scroll the tabs right").click();
    h.step();
    h.step();
    let _ = h.get_by_label("Scroll the tabs left");
    // Scrolling to the end reaches the last tab.
    for _ in 0..20 {
        let Some(right) = h.query_by_label("Scroll the tabs right") else { break };
        right.click();
        h.step();
        h.step();
    }
    assert!(h.query_by_label("Scroll the tabs right").is_none());
    h.get_by_label("Help").click();
    h.step();
    h.step();
    assert_eq!(h.state().ui.tab, "Help");
    // A tab opened off screen scrolls into view, so the left arrow goes away.
    h.state_mut().ui.tab = "Home".into();
    for _ in 0..4 {
        h.step();
    }
    assert!(h.query_by_label("Scroll the tabs left").is_none());
}

#[test]
fn plugins_install_from_a_file_after_a_prompt() {
    let _turn = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!("rustgis-ui-plugins-{}", std::process::id()));
    rustgis_ui_egui::rustgis_engine::plugins::store::set_dir(Some(dir.join("installed")));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("ui-test.rgplugin");
    std::fs::write(&file, rustgis_ui_egui::rustgis_engine::plugins::testing::package("ui-test", &["edit"])).unwrap();
    let mut h = harness();
    for _ in 0..4 {
        h.step();
    }
    h.state_mut().run("ui.installPlugin", json!({"path": file.display().to_string()})).unwrap();
    h.step();
    h.step();
    assert!(h.state().plugins.confirm.is_some(), "asks first");
    assert!(rustgis_ui_egui::rustgis_engine::plugins::get("ui-test").is_none());
    h.get_by_label("Install").click();
    h.step();
    h.step();
    assert!(rustgis_ui_egui::rustgis_engine::plugins::get("ui-test").is_some());
    // Its commands are on the Plugins tab, and run from there.
    h.state_mut().ui.tab = "Plugins".into();
    h.step();
    h.step();
    h.get_by_label("Hello").click();
    h.step();
    // Its tools: a button on the Plugins tab, and listed in the Tools pane.
    h.get_by_label("Make Points").click();
    h.step();
    h.step();
    assert_eq!(h.state().gp.tool, Some("plugin:ui-test/m"));
    h.state_mut().gp.tool = None;
    h.step();
    h.step();
    // On the ribbon and in the pane's Plugins section.
    assert_eq!(h.query_all_by_label("Select Two").count(), 2);
    for tab in ["installed", "browse", "settings"] {
        // An unreachable registry is an error in the window, not a crash.
        h.state_mut().run("ui.plugins", json!({"open": true, "tab": tab, "registry": "http://localhost:9/registry.json"})).unwrap();
        h.step();
        h.step();
    }
    // The Tools pane has an Uninstall of its own when a GeoLibre runner is installed on this
    // machine: closed, the only one is the plugin's.
    h.state_mut().ui.right = None;
    h.state_mut().run("ui.plugins", json!({"open": true, "tab": "installed"})).unwrap();
    h.step();
    h.get_by_label("Uninstall").click();
    h.step();
    assert!(rustgis_ui_egui::rustgis_engine::plugins::get("ui-test").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn plugins_can_have_a_tab_of_their_own() {
    let _turn = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
    use rustgis_ui_egui::rustgis_engine::plugins as rp;
    let mut m: serde_json::Value = serde_json::from_str(&rp::testing::manifest("ui-tab", &[])).unwrap();
    // Its own names, so the other tests (which share the loaded plugins) don't see them.
    m["commands"][0]["label"] = json!("Tab Hello");
    m["tools"][0]["name"] = json!("Tab Points");
    m["tools"][1]["name"] = json!("Tab Select");
    m["tab"] = json!({"label": "Hello Tab", "groups": [
        {"label": "Things", "items": ["h", "tool:m", {"menu": "More", "items": ["e", "tool:x"]}]}
    ]});
    let pkg = rp::testing::zip_of(&[("plugin.json", m.to_string().as_bytes()), ("plugin.wasm", &rp::testing::wasm())]);
    rp::load(rp::Package::from_zip(&pkg).unwrap(), "test").unwrap();
    let mut h = harness();
    for _ in 0..4 {
        h.step();
    }
    // Its tab is in the tab strip; its buttons are there, not on the Plugins tab.
    h.state_mut().ui.tab = "Plugins".into();
    h.step();
    h.step();
    assert_eq!(h.query_all_by_label("Tab Points").count(), 0);
    h.state_mut().ui.tab = "plugin:ui-tab".into();
    h.step();
    h.step();
    h.get_by_label("Tab Hello").click();
    h.step();
    h.get_by_label("Tab Points").click();
    h.step();
    h.step();
    assert_eq!(h.state().gp.tool, Some("plugin:ui-tab/m"));
    h.get_by_label("More").click();
    for _ in 0..4 {
        h.step();
    }
    h.get_by_label("Tab Select").click();
    h.step();
    assert_eq!(h.state().gp.tool, Some("plugin:ui-tab/x"));
    // Hidden, then shown again, from Settings › Customize › Ribbon.
    h.state_mut().run("ui.customizeRibbon", json!({"tab": "plugin:ui-tab"})).unwrap();
    h.step();
    assert!(h.state().ui.hidden_tabs.contains(&"plugin:ui-tab".to_string()));
    h.state_mut().run("ui.customizeRibbon", json!({"tab": "plugin:ui-tab"})).unwrap();
    // A disabled plugin's tab goes, and the ribbon falls back to the Plugins tab.
    h.state_mut().ui.tab = "plugin:ui-tab".into();
    rp::set_enabled("ui-tab", false).unwrap();
    h.step();
    h.step();
    assert_eq!(h.state().ui.tab, "Plugins");
    rp::unload("ui-tab");
}

#[test]
fn plugin_small_ribbon_columns_and_nested_menus_preserve_actions() {
    let _turn = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
    use rustgis_ui_egui::rustgis_engine::plugins as rp;
    let mut m: serde_json::Value = serde_json::from_str(&rp::testing::manifest("ui-small", &[])).unwrap();
    m["commands"] = json!([
        {"id": "e", "label": "Compact Error"},
        {"id": "h", "label": "Width Probe"},
        {"id": "h-large", "label": "Default Large"}
    ]);
    m["tools"][0]["name"] = json!("Compact Points");
    m["tools"][1]["name"] = json!("Compact Select");
    m["panel"] = json!({"title": "Compact Panel", "items": [
        {"kind": "text", "id": "marker", "label": "Compact panel marker", "text": "Panel is open"}
    ]});
    m["i18n"] = json!({"de": {
        "Width Probe": "Eine besonders lange\nübersetzte Beschriftung für diese Schaltfläche",
        "Nested Tools": "Nested\nTools"
    }});
    m["tab"] = json!({"label": "Compact Tab", "groups": [
        {"label": "Compact controls", "layout": "small", "items": [
            "e", "h", "tool:m", "tool:x",
            {"menu": "Inherited Menu", "items": [
                {"menu": "Nested Tools", "layout": "large", "items": ["tool:x"]}
            ]},
            {"menu": "Large Override", "layout": "large", "items": ["h"]},
            "panel"
        ]},
        {"label": "Mixed controls", "items": [
            "h-large",
            {"menu": "Small Override", "layout": "small", "items": [
                {"menu": "Nested\nCommand", "items": ["e"]}
            ]},
            {"menu": "Small Neighbor", "layout": "small", "items": ["h"]}
        ]}
    ]});
    let pkg = rp::testing::zip_of(&[("plugin.json", m.to_string().as_bytes()), ("plugin.wasm", &rp::testing::wasm())]);
    rp::load(rp::Package::from_zip(&pkg).unwrap(), "test").unwrap();
    let mut h = harness();
    h.set_size(egui::vec2(2400.0, 860.0));
    h.state_mut().run("ui.language", json!({"code": "en"})).unwrap();
    h.state_mut().ui.tab = "plugin:ui-small".into();
    h.run_steps(4);
    let english_width = h.get_by_label("Width Probe").rect().width();
    h.state_mut().run("ui.language", json!({"code": "de"})).unwrap();
    h.run_steps(4);
    let error = h.get_by_label("Compact Error").rect();
    let wide = h.get_by_label("Eine besonders lange übersetzte Beschriftung für diese Schaltfläche").rect();
    let points = h.get_by_label("Compact Points").rect();
    let select = h.get_by_label("Compact Select").rect();
    let inherited = h.get_by_label("Inherited Menu").rect();
    let large = h.get_by_label("Large Override").rect();
    let panel = h.get_by_label("Compact Panel").rect();
    assert!(wide.width() > english_width + 80.0, "the translated label must determine the control's width");
    // The widest control is deliberately in the middle of the first column.
    assert!((error.left() - wide.left()).abs() < 1.0 && (wide.left() - points.left()).abs() < 1.0);
    assert!(error.bottom() <= wide.top() && wide.bottom() <= points.top(), "small controls stack vertically");
    assert!((select.top() - error.top()).abs() < 1.0, "the fourth entry starts a new column");
    for first in [error, wide, points] {
        assert!(first.right() <= select.left(), "the next column must clear every translated label: {first:?} / {select:?}");
    }
    assert!((inherited.left() - select.left()).abs() < 1.0 && select.bottom() <= inherited.top());
    assert!((inherited.height() - error.height()).abs() < 1.0, "a menu inherits the small group layout");
    assert!(select.right().max(inherited.right()) <= large.left(), "the large override follows the compact run");
    assert!(large.height() > 2.0 * error.height(), "an explicitly large menu stays large in a small group");
    assert!(large.right() <= panel.left() && (panel.top() - error.top()).abs() < 1.0, "a large control breaks the small run");
    let default_large = h.get_by_label("Default Large").rect();
    let small_override = h.get_by_label("Small Override").rect();
    let small_neighbor = h.get_by_label("Small Neighbor").rect();
    assert!(default_large.height() > 2.0 * small_override.height(), "an unspecified group layout remains large");
    assert!(default_large.right() <= small_override.left());
    assert!((small_override.left() - small_neighbor.left()).abs() < 1.0 && small_override.bottom() <= small_neighbor.top());
    assert!((small_override.height() - error.height()).abs() < 1.0, "menu-level small overrides stack in a large group");

    let mark = h.state().session.notice_mark();
    h.get_by_label("Compact Error").click();
    h.run_steps(2);
    assert!(h.state().session.notices_since(mark).iter().any(|n| n.error && n.text.contains("nope")));
    h.get_by_label("Compact Points").click();
    h.run_steps(2);
    assert_eq!(h.state().gp.tool, Some("plugin:ui-small/m"));
    h.get_by_label("Compact Select").click();
    h.run_steps(2);
    assert_eq!(h.state().gp.tool, Some("plugin:ui-small/x"));
    h.get_by_label("Compact Points").click();
    h.run_steps(2);
    assert_eq!(h.state().gp.tool, Some("plugin:ui-small/m"));
    h.get_by_label("Inherited Menu").click();
    h.run_steps(4);
    let nested_tools = h.get_by_label("Nested Tools");
    let submenu_height = nested_tools.rect().height();
    nested_tools.click();
    h.run_steps(4);
    // Choose the popup row, not the identically named ribbon button.
    let nested_select = h.query_all_by_label("Compact Select").find(|n| n.rect().top() > inherited.bottom()).unwrap();
    assert!(nested_select.rect().height() < large.height(), "nested entries remain ordinary menu rows");
    assert!((submenu_height - nested_select.rect().height()).abs() < 1.0, "translated submenu labels must stay on one standard-height row");
    nested_select.click();
    h.run_steps(2);
    assert_eq!(h.state().gp.tool, Some("plugin:ui-small/x"), "the nested leaf must activate its tool");
    h.get_by_label("Small Override").click();
    h.run_steps(4);
    h.get_by_label("Nested Command").click();
    h.run_steps(4);
    let mark = h.state().session.notice_mark();
    h.query_all_by_label("Compact Error").find(|n| n.rect().top() > small_override.bottom()).unwrap().click();
    h.run_steps(2);
    assert!(h.state().session.notices_since(mark).iter().any(|n| n.error && n.text.contains("nope")), "the nested command must run");
    h.get_by_label("Compact Panel").click();
    h.run_steps(2);
    assert_eq!(h.state().ui.right, Some(rustgis_ui_egui::RightPane::Plugin));
    h.get_by_label("Panel is open");
    h.query_all_by_label("Compact Panel").find(|n| n.rect().top() < panel.bottom()).unwrap().click();
    h.run_steps(2);
    assert_ne!(h.state().ui.right, Some(rustgis_ui_egui::RightPane::Plugin));
    assert!(h.query_by_label("Panel is open").is_none(), "the compact panel button still toggles its pane");
    rp::unload("ui-small");
}

#[test]
fn browse_lists_thousands_of_plugins_quickly() {
    use rustgis_ui_egui::rustgis_engine::plugins::registry;
    let _turn = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
    let sha = "a".repeat(64);
    let list: Vec<serde_json::Value> = (0..5000)
        .map(|i| {
            let cat = ["Vector", "Raster", "Terrain"][i % 3];
            json!({"id": format!("fake-{i:04}"), "name": format!("Plugin {i}"), "version": "1.0.0",
                "description": format!("Does many things with data for plugin number {i}. ").repeat(3), "author": format!("Author {}", i % 97),
                "categories": [cat, "Utilities"], "package": format!("https://packages.example.com/fake-{i}.rgplugin"),
                "sha256": sha, "size": 123_456, "permissions": ["edit"]})
        })
        .collect();
    let entries = registry::parse(&json!({"plugins": list}).to_string(), "https://plugins.rustgis.app/registry.json").unwrap();
    assert_eq!(entries.len(), 5000);
    let mut h = harness();
    for _ in 0..4 {
        h.step();
    }
    h.state_mut().plugins.set_registry(Some(Ok(entries)));
    h.state_mut().run("ui.plugins", json!({"open": true, "tab": "browse"})).unwrap();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    let _ = h.get_by_label("5000 plugins");
    // Only the visible rows are laid out, so a frame stays cheap with thousands of plugins
    // (about 1 ms in a release build; it was over 100 ms when every card was drawn). The bound
    // is loose for debug builds on busy machines.
    let t0 = std::time::Instant::now();
    for _ in 0..10 {
        h.step();
    }
    let per_frame = t0.elapsed() / 10;
    assert!(per_frame < std::time::Duration::from_millis(100), "{per_frame:?} per frame");
    // Search and the category filter narrow the list.
    h.state_mut().plugins.filter = "plugin 4999".into();
    h.step();
    h.step();
    let _ = h.get_by_label("1 of 5000 plugins");
    h.state_mut().plugins.filter = String::new();
    h.state_mut().plugins.category = "Terrain".into();
    h.step();
    h.step();
    let _ = h.get_by_label("1666 of 5000 plugins");
    // A new registry of the same size replaces what is listed (the cache follows the registry,
    // not its allocation).
    h.state_mut().plugins.category = String::new();
    let two = |a: &str, b: &str| {
        let list: Vec<serde_json::Value> = [a, b]
            .iter()
            .map(|n| json!({"id": n.to_lowercase(), "name": n, "version": "1.0.0", "package": "https://packages.example.com/x.rgplugin", "sha256": sha}))
            .collect();
        registry::parse(&json!({"plugins": list}).to_string(), "https://plugins.rustgis.app/registry.json").unwrap()
    };
    h.state_mut().plugins.set_registry(Some(Ok(two("Alpha", "Beta"))));
    h.step();
    h.step();
    let _ = h.get_by_label("Alpha");
    h.state_mut().plugins.set_registry(Some(Ok(two("Gamma", "Delta"))));
    h.step();
    h.step();
    let _ = h.get_by_label("Gamma");
    assert!(h.query_by_label("Alpha").is_none());
    h.state_mut().plugins.set_registry(None);
    h.state_mut().plugins.open = false;
}

/// Switching to a CJK language keeps drawing the previous one for the frame its fonts are
/// installed in (`set_fonts` takes effect a frame late), so no frame shows missing-glyph boxes.
#[test]
fn language_switch_waits_for_its_font() {
    let mut h = harness();
    // A machine without a CJK face draws Chinese as boxes, whenever it switches.
    h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
    h.state_mut().run("ui.language", json!({"code": "en"})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(h.state().lang().code(), "en");
    h.state_mut().run("ui.language", json!({"code": "zh"})).unwrap();
    assert_eq!(h.state().chosen_lang().code(), "zh");
    h.step();
    assert_eq!(h.state().lang().code(), "en", "fonts installed this frame aren't active yet");
    h.step();
    assert_eq!(h.state().lang().code(), "zh");
    // Back to an alphabetic language: nothing to wait for.
    h.state_mut().run("ui.language", json!({"code": "de"})).unwrap();
    h.step();
    assert_eq!(h.state().lang().code(), "de");
}

/// Settings › Start-up (discussion #81): the preferences are saved, a later start follows them,
/// a shell's overrides win, and the start is neither an edit to undo nor unsaved work.
#[test]
fn start_up_preferences() {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    let store: Arc<Mutex<HashMap<String, String>>> = Arc::default();
    let services = || {
        let (load, save) = (store.clone(), store.clone());
        rustgis_ui_egui::Services {
            load_pref: Some(Box::new(move |k| load.lock().unwrap().get(k).cloned())),
            save_pref: Some(Box::new(move |k, v| {
                save.lock().unwrap().insert(k.to_string(), v.to_string());
            })),
            ..Default::default()
        }
    };
    let mut app = RustGisApp::with_sample().with_services(services());
    assert!(app.run("ui.startup", json!({"projection": "nope"})).is_err());
    let r = app.run("ui.startup", json!({"blank": true, "projection": "epsg:3010"})).unwrap();
    assert_eq!(r, json!({"blank": true, "project": "blank", "lastProject": null, "projection": "EPSG:3010", "extent": null}));
    assert_eq!(store.lock().unwrap().get("start_project").map(String::as_str), Some("blank"));

    let mut app = RustGisApp::with_sample().with_services(services());
    app.start(&Default::default()).unwrap();
    assert!(app.session.map().layers.is_empty());
    assert_eq!(app.session.map().projection.code(), "EPSG:3010");
    assert!(!app.session.dirty() && !app.session.can_undo());

    // The web app's `?projection=robinson` without `?blank`: the sample still goes when the
    // preference says blank; an explicit `Some(false)` keeps it.
    let mut app = RustGisApp::with_sample().with_services(services());
    let start = |blank, projection: &str, extent: Option<&str>| rustgis_ui_egui::Start {
        blank,
        projection: Some(projection.into()),
        extent: extent.map(String::from),
        after_crash: false,
    };
    app.start(&start(Some(false), "robinson", None)).unwrap();
    assert!(!app.session.map().layers.is_empty());
    assert_eq!(app.session.map().projection.code(), "robinson");
    assert!(app.start(&start(Some(false), "EPSG:4326", None)).is_err(), "degrees are refused");

    // A start extent: saved from the view, or given by the shell (`?extent=`).
    let mut app = RustGisApp::with_sample().with_services(services());
    app.start(&start(Some(true), "EPSG:3010", Some("10.5,55,24.5,69.5"))).unwrap();
    let view = app.session.run("map.extent", &json!(null)).unwrap();
    let e = rustgis_ui_egui::parse_extent(&view["extent"].as_array().unwrap().iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","))
        .unwrap();
    assert!(e[0] < 11.0 && e[2] > 24.0 && e[1] < 55.5 && e[3] > 69.0, "Sweden is in view: {e:?}");
    assert!(e[2] - e[0] < 90.0 && e[3] - e[1] < 25.0, "and not the world: {e:?}");
    assert!(app.start(&start(None, "EPSG:3010", Some("10,60,5,50"))).is_err(), "west after east");
    app.run("ui.startup", json!({"extent": "view"})).unwrap();
    assert!(store.lock().unwrap().get("start_extent").is_some_and(|v| v.split(',').count() == 4));
    let mut again = RustGisApp::with_sample().with_services(services());
    assert!(again.ui.start_extent.is_some());
    again.start(&Default::default()).unwrap();
    assert!(again.run("ui.startup", json!({"extent": [1, 2]})).is_err());
    again.run("ui.startup", json!({"extent": null})).unwrap();
    assert_eq!(store.lock().unwrap().get("start_extent").map(String::as_str), Some(""));

    // Web Mercator is the default, stored as nothing.
    app.run("ui.startup", json!({"blank": false, "projection": "mercator"})).unwrap();
    assert_eq!(store.lock().unwrap().get("start_projection").map(String::as_str), Some(""));
}

#[test]
fn connections_persist_and_import_once() {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    let old = json!({"saved":[
        {"kind":"service","name":"State WMS","address":"https://w.example/wms","extra":{"type":"wms"}},
        {"kind":"database","name":"Prod","address":"postgres:host=h dbname=g user=u"},
        {"kind":"database","name":"F","address":"/x/a.duckdb"}
    ]})
    .to_string();
    let old_s3 = json!([{"name":"Lake","endpoint":"","region":"eu-west-1","source":"profile","profile":"work"}]).to_string();
    let store =
        Arc::new(Mutex::new(HashMap::from([("data_sources".to_string(), old.clone()), ("s3_connections".to_string(), old_s3.clone())])));
    let services = || {
        let (load, save) = (store.clone(), store.clone());
        rustgis_ui_egui::Services {
            load_pref: Some(Box::new(move |k| load.lock().unwrap().get(k).cloned())),
            save_pref: Some(Box::new(move |k, v| {
                save.lock().unwrap().insert(k.to_string(), v.to_string());
            })),
            ..Default::default()
        }
    };
    let mut app = RustGisApp::with_sample().with_services(services());
    let kinds: Vec<_> = app.session.connections.connections.iter().map(|c| c.kind.as_str()).collect();
    assert_eq!(kinds, ["wms", "postgres", "s3"]);
    assert_eq!(app.session.connections.find("Lake").unwrap().options["credentials"], "profile");
    assert!(store.lock().unwrap().contains_key("connections"));
    assert_eq!(store.lock().unwrap()["data_sources"], old);
    assert_eq!(store.lock().unwrap()["s3_connections"], old_s3);
    app.run("connection.remove", json!({"id":"Prod"})).unwrap();
    let mut again = RustGisApp::with_sample().with_services(services());
    assert_eq!(again.session.connections.connections.len(), 2);
    assert!(again.session.connections.find("Prod").is_none());
    for id in ["State WMS", "Lake"] {
        again.run("connection.remove", json!({"id":id})).unwrap();
    }
    let empty = RustGisApp::with_sample().with_services(services());
    assert!(empty.session.connections.connections.is_empty(), "an empty saved list must not import again");
}

/// Settings › Start-up › Start With a project (#377): the last one opened or saved, or one picked
/// once. It opens with its own projection and view, a shell's `blank` still wins, one that no
/// longer opens leaves the sample with a notice, and after a crash none is opened.
#[test]
fn start_with_a_project_file_or_the_last_one() {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    let store: Arc<Mutex<HashMap<String, String>>> = Arc::default();
    let services = || {
        let (load, save) = (store.clone(), store.clone());
        rustgis_ui_egui::Services {
            load_pref: Some(Box::new(move |k| load.lock().unwrap().get(k).cloned())),
            save_pref: Some(Box::new(move |k, v| {
                save.lock().unwrap().insert(k.to_string(), v.to_string());
            })),
            ..Default::default()
        }
    };
    let dir = std::env::temp_dir().join(format!("rustgis-start-with-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // Removed however the test ends, passed or failed.
    struct TempDir(std::path::PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = TempDir(dir.clone());
    let (a, b) = (dir.join("a.rgis"), dir.join("b.rgis"));
    let shown = |p: &std::path::Path| p.display().to_string();
    let pref = |k: &str| store.lock().unwrap().get(k).cloned();

    // Saving records the last project, by its full path. It's saved in a projection that is
    // neither the default nor the Start-up one.
    let mut app = RustGisApp::with_sample().with_services(services());
    app.run("map.projection", json!({"type": "EPSG:3010"})).unwrap();
    app.run("project.saveAs", json!({"path": shown(&a)})).unwrap();
    assert_eq!(pref("last_project"), Some(shown(&a)));
    // Last Project opens it with its own projection and view: the Start-up ones are for the
    // sample and a blank map.
    let r = app.run("ui.startup", json!({"project": "last", "projection": "robinson", "extent": [10.5, 55.0, 24.5, 69.5]})).unwrap();
    assert_eq!((r["project"].clone(), r["lastProject"].clone()), (json!("last"), json!(shown(&a))));
    assert_eq!(pref("start_project").as_deref(), Some("last"));
    let mut started = RustGisApp::with_sample().with_services(services());
    started.start(&Default::default()).unwrap();
    assert_eq!(started.session.path.as_deref(), Some(a.as_path()));
    assert_eq!(started.session.map().projection.code(), "EPSG:3010");
    assert!(started.ui.start_view.is_none(), "the Start-up extent isn't applied");
    assert!(!started.session.dirty() && !started.session.can_undo());
    // A shell's blank wins: a file on the command line (true) or --sample (false).
    for (blank, sample) in [(true, false), (false, true)] {
        let mut shell = RustGisApp::with_sample().with_services(services());
        shell.start(&rustgis_ui_egui::Start { blank: Some(blank), ..Default::default() }).unwrap();
        assert!(shell.session.path.is_none() && shell.session.map().layers.is_empty() != sample, "blank: {blank}");
    }
    // After a crash, the start-up project (which may be what crashed) isn't opened.
    let mut crashed = RustGisApp::with_sample().with_services(services());
    crashed.start(&rustgis_ui_egui::Start { after_crash: true, ..Default::default() }).unwrap();
    assert!(crashed.session.path.is_none() && !crashed.session.map().layers.is_empty());
    assert!(crashed.session.notices.iter().any(|n| n.error), "{:?}", crashed.session.notices);

    // A picked project: it must be a project file that is there.
    std::fs::write(dir.join("notes.txt"), "x").unwrap();
    for bad in [
        json!({"path": shown(&dir.join("notes.txt"))}),
        json!({"path": shown(&dir.join("missing.rgis"))}),
        json!({"path": ""}),
        json!(5),
        json!(null),
        json!("nope"),
    ] {
        assert!(app.run("ui.startup", json!({"project": bad})).is_err(), "{bad}");
    }
    assert_eq!(pref("start_project").as_deref(), Some("last"), "a refusal changes nothing");
    app.run("project.saveAs", json!({"path": shown(&b)})).unwrap();
    let r = app.run("ui.startup", json!({"project": {"path": shown(&a)}})).unwrap();
    assert_eq!(r["project"], json!({"path": shown(&a)}));
    let mut started = RustGisApp::with_sample().with_services(services());
    assert_eq!(started.ui.last_project, Some(shown(&b)));
    started.start(&Default::default()).unwrap();
    assert_eq!(started.session.path.as_deref(), Some(a.as_path()), "the picked project, not the last one");

    // A save that doesn't go through RustGisApp::run (the MCP relay, the assistant) counts too,
    // once a frame sees it.
    let c = dir.join("c.rgis");
    started.session.run("project.saveAs", &json!({"path": shown(&c)})).unwrap();
    started.logic(&egui::Context::default());
    assert_eq!(pref("last_project"), Some(shown(&c)));

    // A project that no longer opens leaves the sample, says why, and is tried again next time.
    std::fs::remove_file(&a).unwrap();
    let mut started = RustGisApp::with_sample().with_services(services());
    started.start(&Default::default()).unwrap();
    assert!(started.session.path.is_none() && !started.session.map().layers.is_empty());
    assert!(started.session.notices.iter().any(|n| n.error && n.text.contains("a.rgis")), "{:?}", started.session.notices);
    assert_eq!(pref("start_project"), Some(format!("file:{}", shown(&a))));
    // A stored path is trimmed, and one that isn't a project file isn't opened.
    store.lock().unwrap().insert("start_project".into(), format!("file:  {}  ", shown(&b)));
    let mut started = RustGisApp::with_sample().with_services(services());
    assert_eq!(started.ui.start_with, rustgis_ui_egui::StartWith::File(shown(&b)));
    started.start(&Default::default()).unwrap();
    assert_eq!(started.session.path.as_deref(), Some(b.as_path()));
    store.lock().unwrap().insert("start_project".into(), format!("file:{}", shown(&dir.join("notes.txt"))));
    let mut started = RustGisApp::with_sample().with_services(services());
    started.start(&Default::default()).unwrap();
    assert!(started.session.path.is_none() && started.session.notices.iter().any(|n| n.error && n.text.contains("notes.txt")));
    // Back to the sample, as `blank` still says.
    app.run("ui.startup", json!({"blank": false})).unwrap();
    assert_eq!(pref("start_project").as_deref(), Some("sample"));

    // The web app (no file paths): neither new choice is offered or kept.
    store.lock().unwrap().insert("start_project".into(), "last".into());
    let web = |store: &Arc<Mutex<HashMap<String, String>>>| {
        let load = store.clone();
        rustgis_ui_egui::Services {
            load_pref: Some(Box::new(move |k| load.lock().unwrap().get(k).cloned())),
            open_async: Some(Box::new(|_: &str| {})),
            ..Default::default()
        }
    };
    let mut browser = RustGisApp::with_sample().with_services(web(&store));
    assert_eq!(browser.ui.start_with, rustgis_ui_egui::StartWith::Sample);
    // Refused before the path is looked at: a missing one says so too.
    for path in [shown(&b), shown(&dir.join("missing.rgis"))] {
        let err = browser.run("ui.startup", json!({"project": {"path": path}})).unwrap_err();
        assert!(err.contains("desktop"), "{err}");
    }
    assert!(browser.run("ui.startup", json!({"project": "last"})).is_err());
}

#[test]
fn a_task_is_recorded_from_what_changes_the_project_and_edited_in_its_pane() {
    let mut h = harness();
    h.step();
    let app = h.state_mut();
    assert!(app.run("ui.taskRecord", json!({})).unwrap()["recording"].as_bool().unwrap());
    // A colour dragged in its picker: one step, with the last colour.
    for c in ["#112233", "#223344", "#336699"] {
        app.run("layer.color", json!({"layer": "Lakes", "color": c})).unwrap();
    }
    // A command that names no layer, run twice, is two steps; so are two selections on one layer.
    app.run("map.basemap", json!({"name": "ocean"})).unwrap();
    app.run("map.basemap", json!({"name": "paper"})).unwrap();
    app.run("select.byAttributes", json!({"layer": "Countries", "where": "pop_est > 1000000"})).unwrap();
    app.run("select.byAttributes", json!({"layer": "Countries", "where": "pop_est > 5000000"})).unwrap();
    // Picking the layer to work on isn't a step either.
    app.run("layer.select", json!({"layer": "Lakes"})).unwrap();
    // Neither panes nor the view are steps.
    app.run("ui.pane.symbology", json!(null)).unwrap();
    app.run("map.zoomIn", json!(null)).unwrap();
    app.run("gp.run", json!({"tool": "buffer", "params": {"input": "Cities", "distance": 100, "output": "Near"}})).unwrap();
    app.run("layer.color", json!({"layer": "Near", "color": "#1B8A3A"})).unwrap();
    let r = app.run("ui.taskRecord", json!({"name": "Lesson"})).unwrap();
    assert_eq!(r["steps"], json!(7));
    let task = app.session.project.tasks.iter().find(|t| t.name == "Lesson").unwrap().clone();
    let commands: Vec<&str> = task.steps.iter().map(|s| s.command.as_str()).collect();
    assert_eq!(
        commands,
        ["layer.color", "map.basemap", "map.basemap", "select.byAttributes", "select.byAttributes", "gp.run", "layer.color"]
    );
    assert_eq!(task.steps[5].title, "Buffer", "a tool's step has the tool's name");
    assert_eq!(task.steps[0].params["color"], json!("#336699"));
    // It opens in the Tasks pane, ready to edit.
    assert_eq!(app.ui.right, Some(rustgis_ui_egui::RightPane::Tasks));
    assert!(app.ws.task_editing && app.ws.task == "Lesson");
    // The symbol editor sets the active layer's colour without naming it: the step names the
    // layer, and a drag is one step.
    app.run("ui.taskRecord", json!({})).unwrap();
    app.run("layer.select", json!({"layer": "Lakes"})).unwrap();
    for c in ["#101010", "#202020", "#303030"] {
        app.run("layer.color", json!({"color": c})).unwrap();
    }
    let steps = app.ws.recording.clone().unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!((steps[0].params["layer"].clone(), steps[0].params["color"].clone()), (json!("Lakes"), json!("#303030")));
    app.ws.recording = None;
    // So does any command run on the active layer, named as it was before the command ran (a
    // rename changes it).
    app.run("ui.taskRecord", json!({})).unwrap();
    app.run("layer.select", json!({"layer": "Rivers"})).unwrap();
    app.run("select.all", json!({})).unwrap();
    app.run("layer.rename", json!({"name": "Streams"})).unwrap();
    let steps = app.ws.recording.take().unwrap();
    assert_eq!(steps.iter().map(|s| s.params["layer"].clone()).collect::<Vec<_>>(), [json!("Rivers"), json!("Rivers")]);
    app.run("layer.rename", json!({"layer": "Streams", "name": "Rivers"})).unwrap();
    // A command holding a token isn't recorded, and that is said; the same without one is a step.
    app.run("ui.taskRecord", json!({})).unwrap();
    app.run("map.addTiles", json!({"url": "https://tiles.example.com/{z}/{x}/{y}.png?access_token=pk.abc", "name": "Private"})).unwrap();
    assert_eq!(app.ws.recording.as_ref().map(Vec::len), Some(0));
    assert!(app.session.notices.iter().any(|n| n.text.contains("password, key or token")));
    app.run("map.addTiles", json!({"url": "https://tiles.example.com/{z}/{x}/{y}.png", "name": "Public"})).unwrap();
    assert_eq!(app.ws.recording.as_ref().map(Vec::len), Some(1));
    app.ws.recording = None;
    // Undo while recording keeps the step, and says how to drop it.
    app.run("ui.taskRecord", json!({})).unwrap();
    app.run("layer.color", json!({"layer": "Lakes", "color": "#445566"})).unwrap();
    app.run("edit.undo", json!(null)).unwrap();
    assert_eq!(app.ws.recording.as_ref().map(Vec::len), Some(1));
    assert!(app.session.notices.iter().any(|n| n.text.contains("Edit Steps")));
    // Past 200 steps nothing more is kept, and that is said once.
    let given = app.session.notices.iter().filter(|n| n.error).count();
    for i in 0..205 {
        let layer = if i % 2 == 0 { "Rivers" } else { "Lakes" };
        app.run("layer.color", json!({"layer": layer, "color": "#112233"})).unwrap();
    }
    assert_eq!(app.ws.recording.as_ref().map(Vec::len), Some(200));
    assert_eq!(app.session.notices.iter().filter(|n| n.error).count(), given + 1, "one notice at the cap");
    app.run("ui.taskRecord", json!({"name": "Long"})).unwrap();
    assert_eq!(app.session.project.tasks.iter().find(|t| t.name == "Long").unwrap().steps.len(), 200);
    app.run("task.remove", json!({"task": "Long"})).unwrap();
    // Undo said once per recording.
    let said = |app: &RustGisApp| app.session.notices.iter().filter(|n| n.text.contains("Edit Steps")).count();
    let before = said(app);
    app.run("ui.taskRecord", json!({})).unwrap();
    app.run("layer.color", json!({"layer": "Lakes", "color": "#445566"})).unwrap();
    app.run("edit.undo", json!(null)).unwrap();
    app.run("layer.color", json!({"layer": "Rivers", "color": "#445566"})).unwrap();
    app.run("edit.undo", json!(null)).unwrap();
    assert_eq!(said(app), before + 1, "the undo notice once per recording");
    app.ws.recording = None;
    // Stopping with nothing recorded makes no task, and says so.
    app.run("ui.taskRecord", json!({})).unwrap();
    assert_eq!(app.run("ui.taskRecord", json!({})).unwrap()["steps"], json!(0));
    assert_eq!(app.session.project.tasks.len(), 1);
    assert!(app.session.notices.iter().any(|n| n.text.contains("Nothing was recorded")));

    // The pane draws while a step is being edited, and saving it keeps the new words.
    h.state_mut().ws.task_edit = Some((
        1,
        "Buffer the cities".into(),
        "Analyze › Tools › Buffer.
Distance: 100 km."
            .into(),
    ));
    h.step();
    h.step();
    let app = h.state_mut();
    app.run(
        "task.editStep",
        json!({"task": "Lesson", "step": 1, "title": "Buffer the cities", "instructions": "Analyze › Tools › Buffer."}),
    )
    .unwrap();
    assert_eq!(app.session.project.tasks[0].steps[1].title, "Buffer the cities");
    // Unticking Edit Steps drops an unsaved edit; out of edit mode, a step runs from the pane.
    app.ws.task_edit = Some((0, "Typed".into(), String::new()));
    h.step();
    h.get_by_label("Edit Steps").click();
    h.step();
    h.step();
    let app = h.state_mut();
    assert!(!app.ws.task_editing);
    assert!(app.ws.task_edit.is_none(), "the unsaved edit is dropped");
    // The name typed beside Stop Recording names the task.
    let app = h.state_mut();
    app.run("ui.taskRecord", json!({})).unwrap();
    app.run("layer.color", json!({"layer": "Lakes", "color": "#654321"})).unwrap();
    app.ws.record_name = "Lake colours".into();
    h.step();
    h.step();
    assert!(h.get_by_label("Stop Recording").rect().max.x <= 1400.0, "the name field leaves Stop Recording in the pane");
    h.get_by_label("Stop Recording").click();
    h.step();
    h.step();
    let app = h.state_mut();
    assert!(app.ws.recording.is_none());
    assert!(app.session.project.tasks.iter().any(|t| t.name == "Lake colours"));
    // Opening another project ends a recording, and says so.
    app.run("ui.taskRecord", json!({})).unwrap();
    app.run("layer.color", json!({"layer": "Lakes", "color": "#123456"})).unwrap();
    let errors = app.session.notices.iter().filter(|n| n.error).count();
    app.run("project.new", json!(null)).unwrap();
    assert!(app.ws.recording.is_none());
    assert_eq!(app.session.notices.iter().filter(|n| n.error).count(), errors + 1);
    assert!(app.session.project.tasks.is_empty(), "nothing recorded lands in the new project");
}

#[test]
fn show_me_points_at_where_a_steps_command_is() {
    use rustgis_ui_egui::RightPane;
    use rustgis_ui_egui::rustgis_engine::project::TaskStep;
    let step = |command: &str, params: serde_json::Value| TaskStep {
        title: "x".into(),
        instructions: String::new(),
        command: command.into(),
        params,
    };
    let mut h = harness();
    h.step();
    let show = |h: &mut egui_kittest::Harness<'static, RustGisApp>, st: TaskStep| {
        h.state_mut().run("ui.taskShowMe", serde_json::to_value(&st).unwrap()).unwrap();
        for _ in 0..6 {
            h.step();
        }
    };
    // A tool in the Analyze gallery: its tab, its button outlined, nothing opened for the user.
    show(&mut h, step("gp.run", json!({"tool": "buffer", "params": {"input": "Cities", "distance": 100}})));
    let app = h.state_mut();
    assert_eq!(app.ui.tab, "Analyze");
    assert!(app.ui.show_me.as_ref().is_some_and(|s| s.found), "the Buffer button is outlined");
    assert_ne!(app.ui.right, Some(RightPane::Geoprocessing));
    // With the ribbon minimized, Show Me opens it to outline the button.
    h.state_mut().ui.ribbon_minimized = true;
    show(&mut h, step("gp.run", json!({"tool": "buffer", "params": {}})));
    assert!(h.state_mut().ui.show_me.as_ref().is_some_and(|s| s.found), "found on the peeked ribbon");
    h.state_mut().ui.ribbon_minimized = false;
    // A tool with no button on the ribbon opens, empty, in the Tools pane.
    show(&mut h, step("gp.run", json!({"tool": "xyTableToPoint", "params": {}})));
    assert_eq!(h.state_mut().ui.right, Some(RightPane::Geoprocessing));
    // A command whose button opens a dialog: that button.
    show(&mut h, step("select.byAttributes", json!({"layer": "Countries", "where": "pop_est > 1"})));
    let app = h.state_mut();
    assert_eq!(app.ui.tab, "Home");
    assert!(app.ui.show_me.as_ref().is_some_and(|s| s.found), "Select By Attributes is outlined");
    // A command no ribbon button runs: Command Search on its name.
    h.state_mut().ui.command_search = None;
    show(&mut h, step("layer.zoomTo", json!({"layer": "Lakes"})));
    assert!(h.state_mut().ui.command_search.is_some());
    h.state_mut().ui.command_search = None;
    // Symbology: the Symbology pane, on the step's layer.
    show(&mut h, step("layer.color", json!({"layer": "Lakes", "color": "#336699"})));
    assert_eq!(h.state_mut().ui.right, Some(RightPane::Symbology));
    // A Layers filter hiding the step's layer is cleared, so the layer stays active.
    h.state_mut().ui.layer_filter = "Riv".into();
    h.step();
    show(&mut h, step("layer.color", json!({"layer": "Lakes", "color": "#336699"})));
    let app = h.state_mut();
    let lakes = app.session.map().layer_by_name("Lakes").unwrap().id;
    assert_eq!(app.session.active_layer, Some(lakes), "Lakes stays active");
    assert!(app.ui.layer_filter.is_empty());
    // A tool step that names no tool says so, and opens nothing.
    h.state_mut().ui.right = None;
    let said = h.state_mut().run("ui.taskShowMe", json!({"command": "gp.run", "params": {}})).unwrap();
    assert!(said["said"].as_str().unwrap().contains("names no tool"));
    assert_eq!(h.state_mut().ui.right, None);
    // A step whose layer isn't in the map says so, and opens nothing.
    h.state_mut().ui.right = None;
    let said =
        h.state_mut().run("ui.taskShowMe", json!({"command": "layer.color", "params": {"layer": "Gone", "color": "#336699"}})).unwrap();
    assert!(said["said"].as_str().unwrap().contains("Gone"));
    assert_eq!(h.state_mut().ui.right, None);
    // A null layer is the active one: Symbology opens on it.
    let said =
        h.state_mut().run("ui.taskShowMe", json!({"command": "layer.color", "params": {"layer": null, "color": "#336699"}})).unwrap();
    assert!(!said["said"].as_str().unwrap().contains("null"));
    assert_eq!(h.state_mut().ui.right, Some(RightPane::Symbology));
    h.state_mut().ui.right = None;
    // Nothing is done for the user: Show Me leaves the layer as it was.
    let before = format!("{:?}", h.state_mut().session.map().layer_by_name("Lakes").unwrap().renderer);
    show(&mut h, step("layer.color", json!({"layer": "Lakes", "color": "#336699"})));
    assert_eq!(format!("{:?}", h.state_mut().session.map().layer_by_name("Lakes").unwrap().renderer), before);
}

#[test]
fn old_and_unknown_tab_names_open_a_current_tab() {
    let mut h = harness();
    for (asked, opened) in [
        ("Imagery", "Analyze"),
        ("Feature Layer", "Style"),
        ("Labeling", "Labeling"),
        ("Share", "Publish"),
        ("Map", "Home"),
        ("nope", "Home"),
    ] {
        let _ = h.state_mut().run("ui.tab", json!({"tab": asked}));
        assert_eq!(h.state().ui.tab, opened, "{asked}");
        h.step();
    }
}

/// Settings holds Customize › Ribbon, so it (like Help) can't be hidden.
#[test]
fn settings_and_help_tabs_always_show() {
    let mut h = harness();
    for tab in ["Settings", "Help"] {
        assert!(h.state_mut().run("ui.customizeRibbon", json!({"tab": tab})).is_err(), "{tab}");
    }
    h.state_mut().run("ui.customizeRibbon", json!({"tab": "Edit"})).unwrap();
    assert_eq!(h.state().ui.hidden_tabs, vec!["Edit".to_string()]);
    h.state_mut().ui.tab = "Settings".into();
    h.step();
    h.step();
}

/// Add Delimited Text in the web app with a script's options: they go with the file picked next
/// (and only that one), so a projected CSV lands in place.
#[test]
fn delimited_text_options_go_with_the_picked_file() {
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = asked.clone();
    let services = rustgis_ui_egui::Services {
        open_async: Some(Box::new(move |purpose: &str| log.lock().unwrap().push(purpose.to_string()))),
        ..Default::default()
    };
    let mut app = RustGisApp::with_sample().with_services(services);
    app.run("ui.addDelimited", json!(null)).unwrap();
    app.run("ui.addDelimited", json!({"x": "east", "y": "north", "crs": "EPSG:2263", "delimiter": ";"})).unwrap();
    assert_eq!(asked.lock().unwrap().as_slice(), ["text", "text"]);
    // A drop (or another picker's file) doesn't take the options.
    let r = app.open_picked("dropped.csv", b"lon,lat\n3,4\n").unwrap();
    let e = app.session.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().extent();
    assert_eq!((e.min_x, e.min_y), (3.0, 4.0));
    let picked = format!("{}parks.csv", rustgis_ui_egui::DELIMITED_PICK);
    let r = app.open_picked(&picked, b"name;east;north\nCentral Park;992000;220000\n").unwrap();
    let id = r["layer"].as_u64().unwrap() as u32;
    let e = app.session.map().layer(id).unwrap().extent();
    assert!((e.min_x + 73.96).abs() < 0.05 && (e.min_y - 40.78).abs() < 0.05, "{e:?}");
    // The options were used up: the next pick is read as it is.
    let picked = format!("{}plain.csv", rustgis_ui_egui::DELIMITED_PICK);
    let r = app.open_picked(&picked, b"lon,lat\n1,2\n").unwrap();
    let e = app.session.map().layer(r["layer"].as_u64().unwrap() as u32).unwrap().extent();
    assert_eq!((e.min_x, e.min_y), (1.0, 2.0));
}

/// Add Delimited Text in the web app: the file first, then the dialog lists its columns (the ones
/// found by name picked), and the ones picked there place it.
#[test]
fn delimited_text_dialog_lists_the_picked_files_columns() {
    let services = rustgis_ui_egui::Services { open_async: Some(Box::new(|_: &str| {})), ..Default::default() };
    let app = RustGisApp::with_sample().with_services(services);
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    h.state_mut().run("ui.addDelimited", json!(null)).unwrap();
    let picked = format!("{}parks.csv", rustgis_ui_egui::DELIMITED_PICK);
    h.state_mut().open_picked(&picked, b"name;east;north\nCentral Park;992000;220000\n").unwrap();
    assert_eq!(h.state().dialogs.open, Some("addCsv"));
    assert_eq!(h.state().dialogs.list, ["name", "east", "north"]);
    assert_eq!(h.state().dialogs.mode, ";", "the separator found");
    assert!(h.state().dialogs.a.is_empty(), "east isn't a name RustGIS finds by itself");
    {
        let d = &mut h.state_mut().dialogs;
        (d.a, d.b, d.text) = ("east".into(), "north".into(), "EPSG:2263".into());
    }
    h.step();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Add").click();
    h.step();
    h.step();
    assert_eq!(h.state().dialogs.open, None);
    let e = h.state().session.map().layer_by_name("parks").unwrap().extent();
    assert!((e.min_x + 73.96).abs() < 0.05 && (e.min_y - 40.78).abs() < 0.05, "{e:?}");
    // A file whose columns are found: picked for you.
    h.state_mut().run("ui.addDelimited", json!(null)).unwrap();
    let picked = format!("{}stops.csv", rustgis_ui_egui::DELIMITED_PICK);
    h.state_mut().open_picked(&picked, b"stop,Lat,Lon\nA,1,2\n").unwrap();
    assert_eq!((h.state().dialogs.a.as_str(), h.state().dialogs.b.as_str()), ("Lon", "Lat"));
}

/// Edit Text (#420) on a placed text opens the Format pane on it (#516): it is selected alone, and
/// the text typed there is saved for that text only, as one undo step.
#[test]
fn a_placed_text_is_edited_in_the_format_pane() {
    use egui::accesskit::Role;
    use rustgis_ui_egui::RightPane;
    use rustgis_ui_egui::rustgis_engine::map::graphics;
    let mut h = harness();
    let first = h.state_mut().run("graphics.add", json!({"shape": "text", "points": [[10, 50]], "text": "First", "fontSize": 20})).unwrap();
    let second = h.state_mut().run("graphics.add", json!({"shape": "text", "points": [[20, 40]], "text": "Second"})).unwrap();
    let (layer, row) = (first["layer"].clone(), second["row"].clone());
    h.state_mut().run("select.rows", json!({"layer": layer, "rows": [first["row"]]})).unwrap();
    h.state_mut().run("ui.dialog", json!({"name": "graphicText", "layer": layer, "row": row})).unwrap();
    assert_eq!((h.state().dialogs.open, h.state().ui.right), (None, Some(RightPane::Format)), "the pane, not the dialog");
    let selection = |h: &egui_kittest::Harness<'_, RustGisApp>| {
        h.state().session.map().layer(layer.as_u64().unwrap() as u32).unwrap().selection.clone()
    };
    assert_eq!(selection(&h), [row.as_u64().unwrap() as usize]);
    h.step();
    h.step();
    h.query_all_by_role(Role::MultilineTextInput).find(|n| n.value().as_deref() == Some("Second")).unwrap().focus();
    h.step();
    h.event(egui::Event::Text(", edited".into()));
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
    h.step();
    let s = &h.state().session;
    let ds = &s.map().layer(layer.as_u64().unwrap() as u32).unwrap().dataset;
    let text = |r: &serde_json::Value| graphics::label(ds, r.as_u64().unwrap() as usize).unwrap().to_string();
    assert!(text(&row).contains(", edited"), "{}", text(&row));
    assert_eq!(text(&first["row"]), "First", "the other text is left alone");
    assert_eq!(s.undo_label(), Some("Edit Graphic"));
    // A row that isn't a text graphic opens nothing.
    assert!(h.state_mut().run("ui.dialog", json!({"name": "graphicText", "layer": layer, "row": 99})).is_err());
    assert_eq!(h.state().dialogs.open, None);
}

/// The Text tool places a text, as a layout's is placed (#516): "Text", selected, in the Format
/// pane with its text selected, so typing replaces it; Shift+Enter starts a new line, Enter ends
/// it. The tool goes back to Explore.
#[test]
fn a_placed_text_is_typed_over_in_the_format_pane() {
    let mut h = harness();
    h.state_mut().run("tool.set", json!({"tool": "graphicText"})).unwrap();
    h.step();
    click_map(&mut h);
    assert_eq!(selected_text(&h).as_deref(), Some("Text"));
    assert_eq!((h.state().ui.right, h.state().dialogs.open), (Some(rustgis_ui_egui::RightPane::Format), None));
    assert_eq!(h.state().session.tool.code(), "explore");
    h.event(egui::Event::Text("Hello".into()));
    h.step();
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Enter);
    h.event(egui::Event::Text("World".into()));
    h.step();
    assert_eq!(selected_text(&h).as_deref(), Some("Text"), "still being typed");
    h.key_press(egui::Key::Enter);
    h.step();
    h.step();
    assert_eq!(selected_text(&h).as_deref(), Some("Hello\nWorld"));
    assert_eq!(h.state().session.undo_label(), Some("Edit Graphic"));
}

/// Add Data From URL downloads off the frame and adds the file when it lands; a failed download
/// is said.
#[test]
fn data_from_a_web_address_lands_later() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let body = br#"{"type": "Feature", "properties": {}, "geometry": {"type": "Point", "coordinates": [1, 2]}}"#;
        // Three requests: an address without a file name is asked for its headers and first
        // bytes (is it a GeoTIFF?) before it is downloaded.
        for stream in listener.incoming().take(3) {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(body);
        }
    });
    let mut h = harness();
    h.step();
    let before = h.state().session.map().layers.len();
    h.state_mut().run("ui.addUrl", json!(null)).unwrap();
    h.step();
    h.state_mut().run("ui.addUrl", json!({"url": format!("http://{addr}/pin"), "name": "Pin"})).unwrap();
    h.state_mut().run("ui.addUrl", json!({"url": "http://169.254.169.254/x.geojson"})).unwrap();
    for _ in 0..100 {
        h.step();
        if h.state().session.map().layer_by_name("Pin").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(h.state().session.map().layers.len(), before + 1);
    assert!(h.state().session.map().layer_by_name("Pin").is_some());
    assert!(h.state().session.notices.iter().any(|n| n.error && n.text.contains("never fetched")));
    assert!(h.state().ui.downloads.is_empty());
}

/// Add Data From URL reads a cloud-optimised GeoTIFF in place from an address that names no file
/// (an API such as Vantor's `…/geotiff?…`): its first bytes are probed off the frame, then the
/// layer reads its tiles by range, never downloading it whole.
#[test]
fn an_unnamed_cog_address_is_read_in_place() {
    use std::io::{Read, Write};
    let body = rustgis_engine::data::raster::cog::example(2048, 256);
    let total = body.len();
    let sent = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = sent.clone();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 4096];
            let read = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..read]).to_ascii_lowercase();
            // Its headers announce the TIFF header's length, as Vantor's Transform API does.
            if req.starts_with("head ") {
                let _ = write!(
                    s,
                    "HTTP/1.1 200 OK\r\nX-TIFF-Header-Length: 8192\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                continue;
            }
            let range = req.lines().find_map(|l| l.strip_prefix("range: bytes=")).and_then(|r| {
                let (a, b) = r.trim().split_once('-')?;
                Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
            });
            let (status, part) = match range {
                Some((a, b)) if a < body.len() => ("206 Partial Content", &body[a..=b.min(body.len() - 1)]),
                _ => ("200 OK", &body[..]),
            };
            count.fetch_add(part.len(), std::sync::atomic::Ordering::Relaxed);
            let _ = write!(s, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", part.len());
            let _ = s.write_all(part);
        }
    });
    let mut h = harness();
    h.step();
    let url = format!("http://{addr}/analytics/v1/raster/ortho-with-overviews/geotiff?function=pansharp_ortho&key=k");
    h.state_mut().run("ui.addUrl", json!({"url": url, "name": "Ortho"})).unwrap();
    assert!(h.state().ui.downloads.is_empty(), "probed first, not downloaded");
    for _ in 0..200 {
        h.step();
        if h.state().session.map().tiles.iter().any(|t| t.name == "Ortho") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let t = h.state().session.map().tiles.iter().find(|t| t.name == "Ortho").cloned();
    assert!(t.is_some_and(|t| t.url == url), "added as a tile layer read in place");
    assert!(h.state().ui.downloads.is_empty() && h.state().ui.cog_probes.is_empty());
    let read = sent.load(std::sync::atomic::Ordering::Relaxed);
    assert!(read < total / 4, "{read} of {total} bytes read");
    // `"as": null` is no `"as"`: probed too, not downloaded, and read in place.
    let null_url = format!("{url}&n=2");
    h.state_mut().run("ui.addUrl", json!({"url": null_url, "name": "Null", "as": null})).unwrap();
    assert!(h.state().ui.downloads.is_empty() && h.state().ui.cog_probes.len() == 1);
    for _ in 0..200 {
        h.step();
        if h.state().session.map().tiles.iter().any(|t| t.name == "Null") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let t = h.state().session.map().tiles.iter().find(|t| t.name == "Null").cloned();
    assert!(t.is_some_and(|t| t.url == null_url), "added as a tile layer read in place");
    // A probe under way when another project opens is dropped: its layer never lands there.
    h.state_mut().run("ui.addUrl", json!({"url": format!("{url}&n=3"), "name": "Late"})).unwrap();
    h.state_mut().run("project.new", json!({})).unwrap();
    assert!(h.state().ui.cog_probes.is_empty());
    for _ in 0..50 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!h.state().session.map().tiles.iter().any(|t| t.name == "Late"));
}

/// A service layer added from the app (Add From Service, the Connections tree) is read past its
/// first page off the frame (#764): an ArcGIS layer again from the start in pages ordered by its
/// object ids, an OGC API collection by its `next` links; the layer is added once the pages are
/// in, with no note.
#[test]
fn service_features_are_read_page_by_page_off_the_frame() {
    use ogc_server::{Route, Server, arcgis_layer, arcgis_page};
    let q = "where=1%3D1&outFields=*&f=geojson";
    let layer = "/arcgis/rest/services/P/MapServer/0";
    let page = |offset: u32| format!("{layer}/query?{q}&resultOffset={offset}&resultRecordCount=2&orderByFields=OBJECTID");
    let collection = |ids: &[u32], next: Option<&str>| {
        let mut c: serde_json::Value = serde_json::from_str(&arcgis_page(ids, false)).unwrap();
        if let Some(href) = next {
            c["links"] = json!([{"rel": "next", "href": href}]);
        }
        c.to_string()
    };
    let server = Server::new(vec![
        Route::new(&format!("{layer}/query?{q}"), &arcgis_page(&[3, 0], true)),
        Route::new(layer, &arcgis_layer(2, true)),
        Route::new(&page(0), &arcgis_page(&[0, 1], true)),
        Route::new(&page(2), &arcgis_page(&[2, 3], true)),
        Route::new(&page(4), &arcgis_page(&[4], false)),
        Route::new("/collections/wells/items?page=2", &collection(&[2], None)),
        Route::new("/collections/wells/items", &collection(&[0, 1], Some("items?page=2"))),
    ]);
    let mut h = harness();
    h.step();
    let base = server.base.clone();
    h.state_mut().run("ui.addUrl", json!({"url": format!("{base}{layer}/query?{q}"), "name": "Parcels", "service": true})).unwrap();
    h.state_mut().run("ui.addUrl", json!({"url": format!("{base}/collections/wells/items"), "name": "Wells", "service": true})).unwrap();
    let ids = |app: &RustGisApp, name: &str| {
        app.session.map().layer_by_name(name).map(|l| l.dataset.features.iter().map(|f| f.attrs[0].as_f64()).collect::<Vec<_>>())
    };
    wait_for_ogc(&mut h, |app| ids(app, "Parcels").is_some() && ids(app, "Wells").is_some());
    let notes: Vec<_> = h.state().session.notices.iter().map(|n| n.text.clone()).collect();
    let n = |i: &[u32]| Some(i.iter().map(|&i| Some(f64::from(i))).collect::<Vec<_>>());
    assert_eq!((ids(h.state(), "Parcels"), ids(h.state(), "Wells")), (n(&[0, 1, 2, 3, 4]), n(&[0, 1, 2])), "{notes:?}");
    assert!(!notes.iter().any(|n| n.contains("aren't read")), "{notes:?}");
    assert!(h.state().ui.service_pages.is_empty() && h.state().ui.downloads.is_empty());
    let offsets: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|r| r.path.ends_with("/0/query"))
        .map(|r| r.query.into_iter().find(|(k, _)| k == "resultOffset").map(|(_, v)| v))
        .collect();
    assert_eq!(offsets, [None, Some("0".into()), Some("2".into()), Some("4".into())]);
}

/// A project by its web address (a `?project=` link) downloads off the frame and replaces the
/// open project when it lands, its layers and view with it; a failed download is said and keeps
/// the open project.
#[test]
fn a_project_from_a_web_address_opens_when_it_lands() {
    use std::io::{Read, Write};
    let mut h = harness();
    h.step();
    let mut shared = h.state().session.project.clone();
    shared.name = "Shared map".into();
    let layers = shared.map().layers.len();
    let body = serde_json::to_string(&shared).unwrap().into_bytes();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(1) {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(&body);
        }
    });
    h.state_mut().run("project.new", json!({})).unwrap();
    assert!(h.state_mut().run("ui.openUrl", json!({})).is_err());
    // An API's address, without the extension, is still read as a project.
    h.state_mut().run("ui.openUrl", json!({"url": format!("http://{addr}/maps/7/project?v=2")})).unwrap();
    for _ in 0..100 {
        h.step();
        if h.state().session.project.name == "Shared map" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(h.state().session.project.name, "Shared map");
    assert_eq!(h.state().session.map().layers.len(), layers);
    assert!(h.state().ui.project_download.is_none());
    h.state_mut().run("ui.openUrl", json!({"url": "http://169.254.169.254/a.rgis"})).unwrap();
    h.step();
    assert!(h.state().session.notices.iter().any(|n| n.error && n.text.contains("never fetched")));
    assert_eq!(h.state().session.project.name, "Shared map");
}

/// A project still downloading when another project is opened doesn't replace it; a pending item
/// picker isn't taken by the download either.
#[test]
fn a_late_project_download_leaves_the_newer_project_and_the_picker_alone() {
    use std::io::{Read, Write};
    let mut h = harness();
    h.step();
    let mut shared = h.state().session.project.clone();
    shared.name = "Late map".into();
    let body = serde_json::to_string(&shared).unwrap().into_bytes();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (go, wait) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(2) {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            // the first answer waits until the test has opened another project
            let _ = wait.recv_timeout(std::time::Duration::from_secs(5));
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(&body);
        }
    });
    h.state_mut().run("ui.openUrl", json!({"url": format!("http://{addr}/late.rgis")})).unwrap();
    h.state_mut().run("project.new", json!({})).unwrap();
    let newer = h.state().session.project.name.clone();
    go.send(()).unwrap();
    for _ in 0..100 {
        h.step();
        if h.state().ui.project_download.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(h.state().ui.project_download.is_none());
    assert_eq!(h.state().session.project.name, newer);
    assert!(h.state().session.notices.iter().any(|n| n.text.contains("after another project was opened")));
    // With an item picker waiting, a downloaded project opens as a project and the picker keeps waiting.
    h.state_mut().ui.pending_open = Some("style.add");
    let _ = go.send(());
    h.state_mut().run("ui.openUrl", json!({"url": format!("http://{addr}/late.rgis")})).unwrap();
    for _ in 0..100 {
        h.step();
        if h.state().session.project.name == "late" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(h.state().session.project.name, "late");
    assert_eq!(h.state().ui.pending_open, Some("style.add"));
}

/// Add From Service connects in the background and lists the service's layers in the dialog.
#[test]
fn a_service_is_listed_when_its_capabilities_land() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let body = br#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink"><Service><Title>Local</Title></Service><Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="http://127.0.0.1/wms?"/></Get></HTTP></DCPType></GetMap></Request><Layer><CRS>EPSG:3857</CRS><Layer><Name>a</Name><Title>A</Title></Layer></Layer></Capability></WMS_Capabilities>"#;
        for stream in listener.incoming().take(1) {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(body);
        }
    });
    let mut h = harness();
    h.state_mut().run("ui.addService", json!(null)).unwrap();
    h.step();
    h.state_mut().run("ui.addService", json!({"url": format!("http://{addr}/wms"), "type": "wms"})).unwrap();
    for _ in 0..100 {
        h.step();
        if h.state().ui.service.as_ref().is_some_and(|c| c.listing.is_some() || c.error.is_some() || c.failure.is_some()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let conn = h.state().ui.service.as_ref().unwrap();
    assert_eq!(conn.error, None);
    assert_eq!(conn.listing.as_ref().unwrap()["layers"][0]["name"], "a");
    h.step();
}

#[test]
fn connections_section_and_dialog() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let body = br#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink"><Service><Title>Local</Title></Service><Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="http://127.0.0.1/wms?"/></Get></HTTP></DCPType></GetMap></Request><Layer><CRS>EPSG:3857</CRS><Layer><Name>a</Name><Title>A</Title></Layer></Layer></Capability></WMS_Capabilities>"#;
        for stream in listener.incoming().take(1) {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(body);
        }
    });
    let mut h = harness();
    h.state_mut().run("connection.new", json!({"kind":"wms","address":"https://w.example/wms","name":"State WMS","group":"Work"})).unwrap();
    h.state_mut().run("ui.pane.sources", json!({})).unwrap();
    h.step();
    h.get_by_label("Work");
    h.get_by_label("State WMS");
    assert!(h.query_by_label("STAC APIs").is_none());
    assert!(h.state_mut().run("ui.dialog", json!({"name":"newConnection"})).is_err());
    h.state_mut().run("ui.connection", json!({"kind":"wms"})).unwrap();
    assert_eq!(h.state().dialogs.open, Some("newConnection"));
    let address = format!("http://{addr}/wms");
    h.state_mut().ui.connection.name = "Local".into();
    h.state_mut().ui.connection.address = address.clone();
    h.step();
    h.step();
    h.get_by_label("Test").click();
    for _ in 0..100 {
        h.step();
        if h.state().dialogs.info.as_ref().is_some_and(|s| s.contains("1 layers")) || h.state().dialogs.error.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(h.state().dialogs.error, None);
    assert!(
        h.state().dialogs.info.as_ref().is_some_and(|s| s.contains("Local: 1 layers")),
        "info: {:?}, pending: {}",
        h.state().dialogs.info,
        h.state().ui.connection.test.is_some()
    );
    assert_eq!(h.state().session.connections.connections.len(), 1);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save").click();
    h.step();
    assert_eq!(h.state().dialogs.open, None);
    let c = h.state().session.connections.find("Local").unwrap();
    assert_eq!(c.address, address);
    let id = c.id.clone();
    h.state_mut().run("ui.connection", json!({"id":"Local"})).unwrap();
    assert_eq!(h.state().dialogs.open, Some("editConnection"));
    assert_eq!(h.state().ui.connection.name, "Local");
    h.state_mut().ui.connection.group = "Lab".into();
    h.step();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save").click();
    h.step();
    let c = h.state().session.connections.find("Local").unwrap();
    assert_eq!(c.group, "Lab");
    assert_eq!(c.id, id);
    h.step();
    h.step();
    h.get_by_label("Local").click_secondary();
    // The menu settles over a few frames; clicking its first layout can miss "Remove".
    for _ in 0..4 {
        h.step();
    }
    h.get_by_label("Remove").click();
    h.step();
    assert!(h.state().session.connections.find("Local").is_none());
}

#[test]
fn add_service_keeps_and_picks_connections() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let body = br#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink"><Service><Title>Local</Title></Service><Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="http://127.0.0.1/wms?"/></Get></HTTP></DCPType></GetMap></Request><Layer><CRS>EPSG:3857</CRS><Layer><Name>a</Name><Title>A</Title></Layer></Layer></Capability></WMS_Capabilities>"#;
        for stream in listener.incoming().take(3) {
            let mut stream = stream.unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf).unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(body).unwrap();
        }
    });
    let wait_for_listing = |h: &mut egui_kittest::Harness<'_, RustGisApp>| {
        for _ in 0..100 {
            h.step();
            if h.state().ui.service.as_ref().is_some_and(|c| c.listing.is_some() || c.error.is_some() || c.failure.is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let conn = h.state().ui.service.as_ref().unwrap();
        assert_eq!(conn.error, None);
        assert!(conn.failure.is_none());
        assert_eq!(conn.listing.as_ref().unwrap()["layers"][0]["name"], "a");
        h.step();
    };
    let mut h = harness();
    let url = format!("http://{addr}/wms");
    let other = h.state_mut().run("connection.new", json!({"kind":"wms","address":url,"name":"Other user"})).unwrap();
    let other_id = other["id"].as_str().unwrap().to_string();
    h.state_mut().run("ui.addService", json!({"url":url,"type":"wms"})).unwrap();
    wait_for_listing(&mut h);
    assert_eq!(h.state().session.connections.connections.len(), 1, "connecting never saves automatically");
    assert_eq!(h.state().dialogs.connection, None);
    h.get_by_label("Save as Connection").click();
    h.step();
    let same_address: Vec<_> = h.state().session.connections.connections.iter().filter(|c| c.address == url).collect();
    assert_eq!(same_address.len(), 2, "an existing address is not a connection identity");
    let saved = same_address.into_iter().find(|c| c.id != other_id).unwrap();
    let saved_id = saved.id.clone();
    assert_ne!(saved.name, other["name"].as_str().unwrap(), "the new connection has a unique default name");
    assert_eq!(saved.kind, "wms");
    assert_eq!(h.state().dialogs.connection.as_deref(), Some(saved_id.as_str()));
    h.step();
    assert!(h.query_by_label("Save as Connection").is_none());

    h.state_mut().ui.service = None;
    h.state_mut().run("ui.dialog", json!({"name":"addService"})).unwrap();
    h.step();
    h.step();
    h.query_all_by_role(egui::accesskit::Role::ComboBox).find(|n| n.value().as_deref() == Some("Pick one…")).unwrap().click();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    h.get_by_label("Other user").click();
    h.step();
    assert_eq!(h.state().dialogs.connection.as_deref(), Some(other_id.as_str()));
    assert_eq!(h.state().dialogs.text, url);
    wait_for_listing(&mut h);
    assert!(h.query_by_label("Save as Connection").is_none());

    let edited = format!("{url}?x=1");
    h.state_mut().dialogs.text = edited.clone();
    h.state_mut().run("ui.addService", json!({"url":edited,"type":"wms"})).unwrap();
    wait_for_listing(&mut h);
    assert_eq!(h.state().dialogs.connection.as_deref(), Some(other_id.as_str()));
    h.get_by_label("Save as Connection");
    assert_eq!(h.state().session.connections.connections.len(), 2);
    server.join().unwrap();
}

#[test]
fn add_database_picks_and_keeps_connections() {
    let mut h = harness();
    let address = "postgres:host=db.example dbname=gis user=me";
    let picked = h.state_mut().run("connection.new", json!({"kind":"postgres","address":address,"name":"Picked DB"})).unwrap();
    let picked_id = picked["id"].as_str().unwrap().to_string();
    h.state_mut().run("ui.dialog", json!({"name":"addDatabase"})).unwrap();
    h.step();
    h.step();
    h.query_all_by_role(egui::accesskit::Role::ComboBox).find(|n| n.value().as_deref() == Some("Pick one…")).unwrap().click();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    h.get_by_label("Picked DB").click();
    h.step();
    assert_eq!(h.state().dialogs.a, address);
    assert_eq!(h.state().dialogs.connection.as_deref(), Some(picked_id.as_str()));
    assert!(h.state().dialogs.b.is_empty(), "picking a connection never fills the password");
    // Simulate a successful List Tables without needing a database server.
    h.state_mut().dialogs.listed = Some(address.into());
    h.step();
    assert!(h.query_by_label("Save as Connection").is_none());

    // The same address entered by hand is still a new connection.
    h.state_mut().dialogs.connection = None;
    h.step();
    h.get_by_label("Save as Connection").click();
    h.step();
    let same_address: Vec<_> = h.state().session.connections.connections.iter().filter(|c| c.address == address).collect();
    assert_eq!(same_address.len(), 2);
    let saved = same_address.into_iter().find(|c| c.id != picked_id).unwrap();
    let saved_id = saved.id.clone();
    assert_eq!(saved.kind, "postgres");
    assert_ne!(saved.name, "Picked DB");
    assert_eq!(h.state().dialogs.connection.as_deref(), Some(saved_id.as_str()));
    h.step();
    assert!(h.query_by_label("Save as Connection").is_none());
    h.state_mut().dialogs.connection = None;
    h.state_mut().dialogs.listed = None;
    h.step();
    assert!(h.query_by_label("Save as Connection").is_none(), "an unlisted endpoint cannot be kept");
}

/// A blank name connects as "default"; once that endpoint is kept, the form shows it as the
/// selected saved connection instead of offering to keep it again.
#[test]
fn s3_dialog_selects_the_kept_connection_after_a_blank_name_connect() {
    let mut h = harness();
    h.state_mut().run("ui.dialog", json!({"name":"s3"})).unwrap();
    h.step();
    h.step();
    // Other tests may have live process-wide connections, so the section can start collapsed.
    if h.query_by_role_and_label(egui::accesskit::Role::Button, "Connect").is_none() {
        h.get_by_label("Connection").click();
        h.run_steps(3);
    }
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Connect").click();
    h.step();
    h.get_by_label("Save as Connection").click();
    h.step();
    h.step();
    assert_eq!(h.state().session.connections.connections.len(), 1);
    assert!(h.query_by_label("Save as Connection").is_none());
    assert!(h.query_by_label("Rename it to save it: a connection has this name").is_none());
    assert!(h.query_all_by_role(egui::accesskit::Role::ComboBox).any(|n| n.value().as_deref() == Some("default")));
    rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::s3::disconnect("default");
}

/// A table list and the table chosen belong to the address they were listed for: editing the
/// address by hand drops them, so Add never reads a table name against another database.
#[test]
fn add_database_drops_stale_tables_when_the_address_is_edited() {
    use egui::accesskit::Role;
    let mut h = harness();
    let address = "postgres:host=db.example dbname=gis user=me";
    h.state_mut().run("ui.dialog", json!({"name":"addDatabase"})).unwrap();
    h.state_mut().dialogs.a = address.into();
    h.state_mut().dialogs.list = vec!["public.parcels".into()];
    h.state_mut().dialogs.c = "public.parcels".into();
    h.state_mut().dialogs.listed = Some(address.into());
    h.step();
    h.step();
    assert_eq!(h.state().dialogs.c, "public.parcels", "an unchanged address keeps its tables");
    h.query_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some(address)).unwrap().focus();
    h.step();
    h.key_press(egui::Key::End);
    h.event(egui::Event::Text("x".into()));
    h.step();
    h.step();
    assert_eq!(h.state().dialogs.a, format!("{address}x"));
    assert!(h.state().dialogs.list.is_empty());
    assert!(h.state().dialogs.c.is_empty());
    assert_eq!(h.state().dialogs.listed, None);
}

/// A failed re-list can't leave the old tables behind: they would no longer be tied to an address
/// the stale-listing check can see, and the table chosen could be sent to another database.
#[test]
fn add_database_failed_relist_drops_the_old_tables() {
    let mut h = harness();
    // Nothing listens on port 9 of the loopback address: listing is refused at once.
    let address = "postgres:host=127.0.0.1 port=9 dbname=gis user=me";
    h.state_mut().run("ui.dialog", json!({"name":"addDatabase"})).unwrap();
    h.state_mut().dialogs.a = address.into();
    h.state_mut().dialogs.list = vec!["public.parcels".into()];
    h.state_mut().dialogs.c = "public.parcels".into();
    h.state_mut().dialogs.listed = Some(address.into());
    h.step();
    h.step();
    h.get_by_label("List Tables").click();
    h.step();
    h.step();
    assert!(h.state().dialogs.error.is_some(), "the listing must have failed");
    assert_eq!(h.state().dialogs.listed, None);
    assert!(h.state().dialogs.list.is_empty());
    assert!(h.state().dialogs.c.is_empty());
}

/// Collections belong to the API they were listed from: typing another address drops them.
#[test]
fn stac_dialog_drops_stale_collections_when_the_address_is_edited() {
    use egui::accesskit::Role;
    let mut h = harness();
    let api = "https://one.example/stac/v1";
    h.state_mut().run("ui.dialog", json!({"name":"stac"})).unwrap();
    h.state_mut().ui.stac.api = api.into();
    h.state_mut().ui.stac.collections = vec![("old-collection".into(), "Old".into())];
    h.state_mut().ui.stac.collection = "old-collection".into();
    h.step();
    h.step();
    h.query_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some(api)).unwrap().focus();
    h.step();
    h.key_press(egui::Key::End);
    h.event(egui::Event::Text("2".into()));
    h.step();
    h.step();
    assert_eq!(h.state().ui.stac.api, format!("{api}2"));
    assert!(h.state().ui.stac.collections.is_empty());
    assert!(h.state().ui.stac.collection.is_empty(), "the collection picked belongs to the old API");
}

#[test]
fn stac_dialog_keeps_and_picks_connections() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let body = br#"{"collections":[{"id":"c1","title":"C"}]}"#;
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut buf = [0u8; 2048];
        let _ = stream.read(&mut buf).unwrap();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
            .unwrap();
        stream.write_all(body).unwrap();
    });
    let mut h = harness();
    let api = format!("http://{addr}/stac");
    h.state_mut().ui.stac.api = api.clone();
    h.state_mut().run("ui.dialog", json!({"name":"stac"})).unwrap();
    h.step();
    h.get_by_label("Collections").click();
    for _ in 0..100 {
        h.step();
        if !h.state().ui.stac.collections.is_empty() || h.state().ui.stac.error.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(h.state().ui.stac.error, None);
    assert_eq!(h.state().ui.stac.collections, vec![("c1".to_string(), "C".to_string())]);
    assert_eq!(h.state().ui.stac.listed, api);
    assert!(h.state().session.connections.connections.is_empty(), "listing never saves automatically");
    h.step();
    h.get_by_label("Save as Connection").click();
    h.step();
    assert_eq!(h.state().session.connections.connections.len(), 1);
    let saved = &h.state().session.connections.connections[0];
    let saved_id = saved.id.clone();
    assert_eq!(saved.kind, "stac");
    assert_eq!(saved.address, api);
    assert_eq!(h.state().ui.stac.connection.as_deref(), Some(saved_id.as_str()));
    h.step();
    assert!(h.query_by_label("Save as Connection").is_none());

    h.state_mut().run("connection.edit", json!({"id":saved_id,"name":"Picked STAC"})).unwrap();
    h.state_mut().ui.stac.api = "https://other.example/stac".into();
    h.step();
    h.query_all_by_role(egui::accesskit::Role::ComboBox).find(|n| n.value().as_deref() == Some("Other")).unwrap().click();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    h.get_by_label("Picked STAC").click();
    h.step();
    assert_eq!(h.state().ui.stac.api, api);
    assert!(h.state().ui.stac.collections.is_empty(), "the existing combo resets collections when picking a saved API");
    assert_eq!(h.state().ui.stac.connection.as_deref(), Some(saved_id.as_str()));
    server.join().unwrap();
}

#[test]
fn connections_postgres_requires_an_address() {
    use egui::accesskit::Role;
    let mut h = harness();
    for address in ["", " \t "] {
        h.state_mut().run("ui.connection", json!({"kind":"postgres"})).unwrap();
        h.state_mut().ui.connection.address = address.into();
        h.step();
        h.step();
        h.get_by_role_and_label(Role::Button, "Save").click();
        h.step();
        assert_eq!(h.state().dialogs.open, Some("newConnection"));
        assert!(h.state().dialogs.error.is_some());
        assert!(h.state().session.connections.connections.is_empty());
        h.get_by_role_and_label(Role::Button, "Test").click();
        h.step();
        assert!(h.state().dialogs.error.as_ref().unwrap().contains("isn't a postgres address"));
        assert!(h.state().dialogs.info.is_none());
        assert!(h.state().session.connections.connections.is_empty());
    }
    let address = "host=127.0.0.1 port=9 dbname=connection_form user=u";
    h.state_mut().ui.connection.name = "Bare PostgreSQL".into();
    h.state_mut().ui.connection.address = address.into();
    h.step();
    h.get_by_role_and_label(Role::Button, "Save").click();
    h.step();
    assert_eq!(h.state().dialogs.open, None);
    let connection = h.state().session.connections.find("Bare PostgreSQL").unwrap();
    let id = connection.id.clone();
    assert_eq!(connection.address, format!("postgres:{address}"));
    h.state_mut().run("ui.connection", json!({"id":id})).unwrap();
    h.state_mut().ui.connection.address = "  ".into();
    h.step();
    h.step();
    h.get_by_role_and_label(Role::Button, "Save").click();
    h.step();
    assert_eq!(h.state().dialogs.open, Some("editConnection"));
    assert!(h.state().dialogs.error.is_some());
    assert_eq!(h.state().session.connections.find(&id).unwrap().address, format!("postgres:{address}"));
}

#[test]
fn connections_test_feedback_tracks_edited_inputs() {
    use egui::accesskit::Role;
    use std::io::{Read, Write};
    use std::time::Duration;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}/stac", listener.local_addr().unwrap());
    let (started, requests) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let bodies: [&[u8]; 3] =
            [br#"{"collections":[{"id":"old","title":"Old"}]}"#, br#"{"collections":[{"id":"current","title":"Current"}]}"#, b"not JSON"];
        for body in bodies {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 2048];
            assert!(stream.read(&mut buf).unwrap() > 0, "the client must send a request before the response is released");
            if started.send(()).is_err() || wait.recv_timeout(Duration::from_secs(5)).is_err() {
                break;
            }
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            stream.write_all(body).unwrap();
        }
    });
    let mut h = harness();
    h.state_mut().run("ui.connection", json!({"kind":"stac"})).unwrap();
    h.state_mut().ui.connection.name = "Endpoint".into();
    h.state_mut().ui.connection.address = address.clone();
    h.step();
    h.step();
    h.get_by_role_and_label(Role::Button, "Test").click();
    h.step();
    requests.recv_timeout(Duration::from_secs(5)).unwrap();
    let old = h.state().ui.connection.test.as_ref().unwrap().1.clone();
    h.query_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some(&address)).unwrap().focus();
    h.step();
    h.key_press(egui::Key::End);
    h.event(egui::Event::Text("/changed".into()));
    h.step();
    assert_eq!(h.state().ui.connection.address, format!("{address}/changed"));
    assert!(h.state().ui.connection.test.is_none(), "editing must discard the pending test");
    assert_eq!(h.state().dialogs.info, None);
    release.send(()).unwrap();
    for _ in 0..250 {
        h.step();
        if old.lock().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(old.lock().unwrap().is_some(), "the discarded request must actually finish");
    assert_eq!(h.state().dialogs.info, None);
    assert_eq!(h.state().dialogs.error, None);

    // A new test can run immediately; editing also clears its completed success or failure.
    for succeeds in [true, false] {
        h.get_by_role_and_label(Role::Button, "Test").click();
        h.step();
        requests.recv_timeout(Duration::from_secs(5)).unwrap();
        release.send(()).unwrap();
        for _ in 0..250 {
            h.step();
            if h.state().ui.connection.test.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(h.state().ui.connection.test.is_none());
        if succeeds {
            assert!(h.state().dialogs.info.as_ref().unwrap().contains("1 collections"));
            assert_eq!(h.state().dialogs.error, None);
        } else {
            assert!(h.state().dialogs.error.is_some());
            assert_eq!(h.state().dialogs.info, None);
        }
        let name = h.state().ui.connection.name.clone();
        h.query_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some(&name)).unwrap().focus();
        h.step();
        h.key_press(egui::Key::End);
        h.event(egui::Event::Text(" changed".into()));
        h.step();
        assert_eq!(h.state().ui.connection.name, format!("{name} changed"));
        assert_eq!(h.state().dialogs.info, None);
        assert_eq!(h.state().dialogs.error, None);
    }
    server.join().unwrap();
    assert!(h.state().session.connections.connections.is_empty());
}

/// Expanding a connection lists its datasets, a click selects and Add Selected adds each as its
/// own undo step, and Refresh forgets the list.
#[test]
fn connections_tree_browses_and_adds() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let body = format!(
            r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink"><Service><Title>Local</Title></Service><Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="http://{addr}/wms?"/></Get></HTTP></DCPType></GetMap></Request><Layer><CRS>EPSG:3857</CRS><Layer><Name>a</Name><Title>A</Title></Layer><Layer><Name>b</Name><Title>B</Title></Layer></Layer></Capability></WMS_Capabilities>"#
        );
        for stream in listener.incoming().take(1) {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(body.as_bytes());
        }
    });
    // Frames 0.05 s apart, so two clicks fall within egui's double-click delay.
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_max_steps(64).with_step_dt(0.05).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        RustGisApp::with_sample(),
    );
    h.state_mut().run("connection.new", json!({"kind":"wms","address":format!("http://{addr}/wms"),"name":"Local"})).unwrap();
    h.state_mut().run("ui.pane.sources", json!({})).unwrap();
    h.step();
    assert!(h.query_by_label("A").is_none(), "nothing is listed before the connection is opened");
    h.get_by_label("Local").click();
    for _ in 0..100 {
        h.step();
        if h.query_by_label("A").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(h.query_by_label("A").is_some(), "the layers are listed under the connection");
    let before = h.state().session.map().tiles.len();
    h.get_by_label("A").click();
    h.step();
    h.get_by_label("B").click();
    h.step();
    assert_eq!(h.state().ui.connection_tree.picks.len(), 2);
    // The selection is added from a dataset's menu: each layer is its own undo step.
    h.get_by_label("B").click_secondary();
    h.step();
    h.step();
    h.get_by_label("Add Selected (2)").click();
    h.step();
    h.step();
    assert_eq!(h.state().session.map().tiles.len(), before + 2);
    assert!(h.state().ui.connection_tree.picks.is_empty());
    h.state_mut().run("edit.undo", json!(null)).unwrap();
    // Each layer was its own step: one undo takes one layer.
    assert_eq!(h.state().session.map().tiles.len(), before + 1);
    h.state_mut().run("edit.undo", json!(null)).unwrap();
    assert_eq!(h.state().session.map().tiles.len(), before);
    // A double-click adds the one dataset, leaving nothing selected.
    // (egui counts clicks in a row: let the earlier ones age out of its triple-click window first.)
    for _ in 0..14 {
        h.step();
    }
    h.get_by_label("A").click();
    h.step();
    h.get_by_label("A").click();
    h.step();
    h.step();
    assert_eq!(h.state().session.map().tiles.len(), before + 1);
    assert!(h.state().ui.connection_tree.picks.is_empty());
    h.state_mut().run("edit.undo", json!(null)).unwrap();
    assert_eq!(h.state().session.map().tiles.len(), before);
    // Recent now lists the added layer under "Local" too, above the connection's own row.
    h.query_all_by_label("Local").last().unwrap().click_secondary();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    h.get_by_label("Refresh").click();
    h.step();
    h.step();
    assert!(h.query_by_label("A").is_none(), "refreshing forgets the list until it is read again");
}

/// Save in the web app (issue #197): the browser has no save dialog, so the first Save and every
/// Save As ask for the file name, and Save then downloads under it; an opened file keeps its name.
#[test]
fn web_save_asks_for_the_file_name() {
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, Vec<u8>)>::new()));
    let log = got.clone();
    let services = rustgis_ui_egui::Services {
        download: Some(Box::new(move |name: &str, bytes: Vec<u8>| {
            log.lock().unwrap().push((name.to_string(), bytes));
            Ok(())
        })),
        ..Default::default()
    };
    let app = RustGisApp::with_sample().with_services(services);
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    let names = |got: &std::sync::Mutex<Vec<(String, Vec<u8>)>>| got.lock().unwrap().iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
    // A project never saved: Save asks first, with the project's name.
    h.state_mut().run("layer.color", json!({"layer": "Countries", "color": "#336699"})).unwrap();
    h.state_mut().run("ui.save", json!(null)).unwrap();
    assert_eq!(h.state().dialogs.open, Some("saveProject"));
    assert_eq!(h.state().dialogs.text, "World Atlas.rgis");
    assert!(names(&got).is_empty());
    h.step();
    h.state_mut().dialogs.text = "  ".into();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save").click();
    h.step();
    assert_eq!(h.state().dialogs.open, Some("saveProject"), "an empty name is refused");
    assert!(h.state().dialogs.error.is_some() && names(&got).is_empty());
    h.state_mut().dialogs.text = "Trip plan".into();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save").click();
    h.step();
    h.step();
    assert_eq!(h.state().dialogs.open, None);
    assert_eq!(names(&got), ["Trip plan.rgis"]);
    assert_eq!(h.state().session.project.name, "Trip plan");
    assert!(!h.state().session.dirty());
    // Saved once: Save downloads under that name again without asking; Save As asks.
    h.state_mut().run("ui.save", json!(null)).unwrap();
    assert_eq!(h.state().dialogs.open, None);
    assert_eq!(names(&got), ["Trip plan.rgis", "Trip plan.rgis"]);
    h.state_mut().run("ui.saveAs", json!(null)).unwrap();
    assert_eq!(h.state().dialogs.open, Some("saveProject"));
    assert_eq!(h.state().dialogs.text, "Trip plan.rgis");
    h.state_mut().dialogs.open = None;
    // An opened file names the project, and Save keeps its name.
    let bytes = got.lock().unwrap()[0].1.clone();
    h.state_mut().open_picked("test.rgis", &bytes).unwrap();
    assert_eq!(h.state().session.project.name, "test");
    h.state_mut().run("ui.save", json!(null)).unwrap();
    assert_eq!(h.state().dialogs.open, None);
    assert_eq!(names(&got).last().map(String::as_str), Some("test.rgis"));
    // Another project in its place hasn't been saved: Save asks again, even under the same name.
    let n = names(&got).len();
    h.state_mut().run("project.new", json!({"name": "test"})).unwrap();
    h.state_mut().run("ui.save", json!(null)).unwrap();
    assert_eq!(h.state().dialogs.open, Some("saveProject"));
    assert_eq!(names(&got).len(), n);
    // A name that can't be a file name is offered as one that can.
    h.state_mut().run("project.new", json!({"name": "Roads 2024/25"})).unwrap();
    h.state_mut().run("ui.saveAs", json!(null)).unwrap();
    assert_eq!(h.state().dialogs.text, "Roads 2024-25.rgis");
}

/// Without a browser's downloads, Save Project As (opened by `ui.dialog`) saves nothing and
/// leaves the project as it was.
#[test]
fn save_project_dialog_needs_downloads() {
    let mut h = harness();
    h.state_mut().run("layer.color", json!({"layer": "Countries", "color": "#336699"})).unwrap();
    h.state_mut().run("ui.dialog", json!({"name": "saveProject"})).unwrap();
    h.step();
    h.state_mut().dialogs.text = "Elsewhere".into();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Save").click();
    h.step();
    assert!(h.state().dialogs.error.is_some());
    assert_eq!(h.state().session.project.name, "World Atlas");
    assert!(h.state().session.dirty());
}

#[test]
fn map_context_menu_copies_the_clicked_coordinates() {
    let mut h = harness();
    for _ in 0..4 {
        h.step();
    }
    // Right-click the middle of the map.
    let at = egui::pos2(700.0, 500.0);
    h.input_mut().events.push(egui::Event::PointerMoved(at));
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: Default::default(),
        });
    }
    for _ in 0..4 {
        h.step();
    }
    // The menu's first row is the place, as "lat, lon".
    let c = h.state().map.menu_lonlat.expect("a place under the pointer");
    let label = format!("{:.6}, {:.6}", c.y, c.x);
    // Moving onto the menu leaves the map: the item must stay.
    h.get_by_label(&label).hover();
    for _ in 0..4 {
        h.step();
    }
    h.get_by_label(&label).click();
    h.step();
    let copied = h.output().platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(t) if *t == label));
    assert!(copied, "nothing copied");
}

#[test]
fn bookmarks_menu_removes_bookmarks() {
    let mut h = harness();
    h.state_mut().ui.tab = "Home".into();
    for _ in 0..4 {
        h.step();
    }
    let before = h.state().session.map().bookmarks.len();
    h.get_by_label("Bookmarks").click();
    for _ in 0..4 {
        h.step();
    }
    h.get_by_label("Remove Europe").click();
    for _ in 0..4 {
        h.step();
    }
    let map = h.state().session.map();
    assert_eq!(map.bookmarks.len(), before - 1);
    assert!(!map.bookmarks.iter().any(|b| b.name == "Europe"));
}

#[test]
fn dialogs_can_be_moved() {
    let mut h = harness();
    // A tab without a New Bookmark button: only the dialog's title matches.
    h.state_mut().ui.tab = "Help".into();
    let _ = h.state_mut().run("ui.dialog", json!({"name": "bookmark"}));
    for _ in 0..4 {
        h.step();
    }
    let before = h.get_by_label("New Bookmark").rect();
    // Drag the title bar.
    let from = before.left_top() + egui::vec2(150.0, 12.0);
    let to = from + egui::vec2(-200.0, 120.0);
    let press =
        |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    h.input_mut().events.push(egui::Event::PointerMoved(from));
    h.input_mut().events.push(press(from, true));
    h.step();
    for i in 1..=5 {
        h.input_mut().events.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 5.0)));
        h.step();
    }
    h.input_mut().events.push(press(to, false));
    for _ in 0..4 {
        h.step();
    }
    let after = h.get_by_label("New Bookmark").rect();
    assert!((after.left_top() - (before.left_top() + (to - from))).length() < 2.0, "{before:?} → {after:?}");
}

/// "Let your Agents Control RustGIS", in the AI Assistant settings: ticking it starts the relay,
/// lets attached agents use files and the network, shows what an agent needs to connect and is
/// remembered; unticking it stops the relay.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn letting_agents_control_rustgis_starts_the_relay_and_shows_how_to_connect() {
    use std::sync::{Arc, Mutex};
    let saved: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let mut app = RustGisApp::with_sample();
    let sink = saved.clone();
    app.services.save_pref = Some(Box::new(move |k: &str, v: &str| {
        if let Ok(mut s) = sink.lock() {
            s.push((k.to_string(), v.to_string()));
        }
    }));
    app.run("ui.pane.assistant", json!({})).expect("opens the assistant");
    // The connection instructions expand below the checkbox: keep the settings above the
    // bottom composer so both pointer clicks reach the checkbox.
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 2000.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    // The first frame loads the assistant's settings and closes the form when they're complete (a
    // key in the environment): open it after that.
    h.step();
    h.state_mut().assistant.settings = true;
    h.step();
    assert!(h.state().relay.is_none(), "off until the user turns it on");
    assert!(h.query_by_label("Add this MCP server to your agent:").is_none(), "nothing to connect to while it's off");
    // Whether the default port is free decides which path the box takes here: a RustGIS already
    // open on this machine may hold it.
    let free = std::net::TcpListener::bind(("127.0.0.1", rustgis_ui_egui::rustgis_assistant::relay::DEFAULT_PORT)).is_ok();
    h.get_by_label("Let your Agents Control RustGIS").click();
    h.step();
    h.step();
    let last = |key: &str| saved.lock().unwrap().iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    if !free {
        // The relay couldn't start: the box and the remembered choice go back off.
        assert!(h.state().relay.is_none());
        assert_eq!(last("agents_control").as_deref(), Some("off"), "a failed start isn't remembered as on");
        assert!(h.query_by_label("Add this MCP server to your agent:").is_none());
        return;
    }
    assert!(h.state().relay.is_some(), "ticking the box starts the relay");
    assert_eq!(last("agents_control").as_deref(), Some("on"), "the choice is remembered");
    assert!(h.query_by_label("Add this MCP server to your agent:").is_some(), "what an agent needs to connect");
    assert!(
        h.query_by_label(if cfg!(windows) {
            "Or run in PowerShell, for Claude Code or Codex:"
        } else {
            "Or run in a POSIX shell, for Claude Code or Codex:"
        })
        .is_some()
    );
    h.get_by_label("Let your Agents Control RustGIS").click();
    h.step();
    assert!(h.state().relay.is_none(), "unticking stops the relay");
    assert_eq!(last("agents_control").as_deref(), Some("off"));
}

/// Agents on this computer: the switch and its override can be ticked, and stick.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn agents_on_this_computer_switches_tick_and_stick() {
    use std::sync::{Arc, Mutex};
    let saved: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let mut app = RustGisApp::with_sample();
    let sink = saved.clone();
    app.services.save_pref = Some(Box::new(move |k: &str, v: &str| {
        if let Ok(mut s) = sink.lock() {
            s.push((k.to_string(), v.to_string()));
        }
    }));
    app.run("ui.pane.assistant", json!({})).expect("opens the assistant");
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 2000.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    // The first frame loads the assistant's settings and closes the form when they're complete (a
    // key in the environment): open it after that.
    h.step();
    h.state_mut().assistant.settings = true;
    h.step();
    h.get_by_label("Let the assistant hand tasks to agents on this computer").click();
    h.step();
    h.step();
    h.get_by_label("Approve agents' actions without asking").click();
    h.step();
    h.step();
    let s = saved.lock().unwrap().clone();
    assert!(s.iter().any(|(k, v)| k == "assistant_local_agents" && v == "on"), "{s:?}");
    assert!(s.iter().any(|(k, v)| k == "assistant_agents_auto" && v == "on"), "the override didn't take: {s:?}");
    assert!(h.query_by_label("Agents then run commands, change files and use the web on this computer without asking you.").is_some());
}

#[path = "../../../tests/support/ogc_server.rs"]
mod ogc_server;

fn wait_for_ogc(h: &mut egui_kittest::Harness<'static, RustGisApp>, ready: impl Fn(&RustGisApp) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        h.step();
        if ready(h.state()) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "service did not finish: {:?}", h.state().ui.service.as_ref().map(|c| &c.error));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn ogc_landing_connect_and_add_use_fixture_documents() {
    use ogc_server::{CATALOG, ITEMS, ROOT, Server};
    let server = Server::fixture();
    let mut h = harness();
    h.state_mut().run("ui.language", json!({"code":"en"})).unwrap();
    h.state_mut().run("project.new", json!(null)).unwrap();
    h.state_mut().run("ui.addService", json!(null)).unwrap();
    h.state_mut().dialogs.text = server.root();
    h.step();
    h.step();
    h.get_by_label("Connect").click();
    wait_for_ogc(&mut h, |app| app.ui.service.as_ref().is_some_and(|c| c.pending.is_none()));
    let conn = h.state().ui.service.as_ref().unwrap();
    assert_eq!(conn.error, None);
    assert_eq!(conn.url, server.root());
    assert_eq!(h.state().dialogs.text, server.root());
    assert_eq!(conn.documents.len(), 1);
    assert_eq!(conn.listing.as_ref().unwrap()["count"], 1);
    h.step();
    h.get_by_label_contains("Places");
    h.get_by_label("Import All").click();
    wait_for_ogc(&mut h, |app| app.session.map().layer_by_name("Places").is_some());
    let dataset = &h.state().session.map().layer_by_name("Places").unwrap().dataset;
    assert_eq!(h.state().session.map().service_groups[0].name, "My OGC Layer");
    assert_eq!(dataset.features.len(), 1);
    assert_eq!(dataset.features[0].geometry, Some(rustgis_engine::data::Geometry::Point(rustgis_engine::geom::Coord::new(10., 50.))));
    let name = dataset.fields.iter().position(|f| f.name == "name").unwrap();
    assert_eq!(dataset.features[0].attrs[name], rustgis_engine::data::Value::Text("Test Place".into()));
    server.assert_fixture_requests(&[ROOT, CATALOG, ITEMS]);
}

#[test]
fn ogc_landing_lists_both_offerings_and_keeps_maps_after_failure() {
    use ogc_server::{CATALOG, ROOT, Route, Server, fixture_collections, fixture_landing};
    for fail in [false, true] {
        let mut landing: serde_json::Value = serde_json::from_str(fixture_landing()).unwrap();
        landing["links"].as_array_mut().unwrap().push(json!({"rel":"tilesets-map","href":"/map/tiles"}));
        let mut catalog = Route::new(CATALOG, fixture_collections());
        catalog.delay = std::time::Duration::from_millis(100);
        if fail {
            catalog.status = 503;
        }
        let server = Server::new(vec![Route::new(ROOT, &landing.to_string()), catalog]);
        let mut h = harness();
        h.state_mut().run("ui.language", json!({"code":"en"})).unwrap();
        h.state_mut().run("project.new", json!(null)).unwrap();
        h.state_mut().run("ui.addService", json!(null)).unwrap();
        h.state_mut().run("ui.addService", json!({"url":server.root()})).unwrap();
        wait_for_ogc(&mut h, |app| app.ui.service.as_ref().is_some_and(|c| c.pending.is_none()));
        let conn = h.state().ui.service.as_ref().unwrap();
        let count = if fail { 1 } else { 2 };
        assert_eq!(conn.listing.as_ref().unwrap()["count"], count);
        assert_eq!(conn.failure.as_ref().and_then(|f| f.status()), fail.then_some(503));
        h.step();
        h.get_by_label_contains("Example GIS (map tiles)");
        if !fail {
            h.get_by_label_contains("Places");
        }
        // The root map remains addable with its own document even after another fetch failed.
        h.query_all_by_label("Add").next().unwrap().click();
        wait_for_ogc(&mut h, |app| !app.session.map().tiles.is_empty());
        assert!(h.state().session.map().tiles[0].url.contains("/map/tiles/"));
    }
}

/// A service a firewall refuses (#509): the dialog says what happened plainly, with what the server
/// said folded under Details until asked for.
#[test]
fn a_refused_service_is_told_plainly_with_its_details() {
    use ogc_server::{ROOT, Route, Server};
    let page = "<html><head><title>Attention Required! | Cloudflare</title></head><body>Sorry, you have been blocked</body></html>";
    let mut blocked = Route::new(ROOT, page);
    blocked.status = 403;
    blocked.headers = vec![("Server".into(), "cloudflare".into()), ("CF-RAY".into(), "a4c9-BOG".into())];
    let server = Server::new(vec![blocked]);
    let mut h = harness();
    h.state_mut().run("ui.language", json!({"code":"en"})).unwrap();
    h.state_mut().run("ui.addService", json!(null)).unwrap();
    h.state_mut().run("ui.addService", json!({"url": server.root()})).unwrap();
    wait_for_ogc(&mut h, |app| app.ui.service.as_ref().is_some_and(|c| c.failure.is_some()));
    let f = h.state().ui.service.as_ref().and_then(|c| c.failure.clone()).unwrap();
    assert_eq!((f.status(), f.firewall()), (Some(403), Some("Cloudflare")));
    h.step();
    h.get_by_label("The server refused the request.");
    assert!(h.query_by_label("Copy details").is_none(), "folded until asked for");
    h.get_by_label("Details").click();
    h.step();
    h.step();
    h.get_by_label("Copy details");
    h.get_by_label("\"Attention Required! | Cloudflare\"");
    h.get_by_label("cf-ray a4c9-BOG");
}

#[test]
fn ogc_landing_reconnecting_ignores_a_delayed_response() {
    use ogc_server::{ROOT, Route, Server, fixture_landing};
    let mut delayed = Route::new(ROOT, fixture_landing());
    delayed.delay = std::time::Duration::from_millis(400);
    let old = Server::new(vec![delayed]);
    let current = Server::fixture();
    let mut h = harness();
    h.state_mut().run("ui.addService", json!({"url":old.root()})).unwrap();
    wait_for_ogc(&mut h, |_| !old.requests().is_empty());
    h.state_mut().run("ui.addService", json!({"url":current.root()})).unwrap();
    wait_for_ogc(&mut h, |app| app.ui.service.as_ref().is_some_and(|c| c.pending.is_none()));
    let until = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < until {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let conn = h.state().ui.service.as_ref().unwrap();
    assert_eq!(conn.url, current.root());
    assert_eq!(conn.error, None);
    assert_eq!(conn.listing.as_ref().unwrap()["layers"][0]["title"], "Places");
    assert_eq!(old.requests().len(), 1);
    current.assert_fixture_requests(&[ogc_server::ROOT, ogc_server::CATALOG]);
}

#[test]
fn ogc_group_context_menus_refresh_all_or_one_child() {
    use ogc_server::{CATALOG, ITEMS, LINE_ITEMS, ROOT, Server, fixture_features};
    let server = Server::group_fixture();
    let mut h = harness();
    h.state_mut().run("ui.language", json!({"code":"en"})).unwrap();
    h.state_mut().run("project.new", json!(null)).unwrap();
    h.state_mut().run("ui.importService", json!({"url":server.root()})).unwrap();
    wait_for_ogc(&mut h, |app| app.ui.service_import.is_none());
    assert_eq!(h.state().session.map().layers.len(), 2, "{:?}; requests: {:?}", h.state().dialogs.error, server.requests());
    let group = h.state().session.map().service_groups[0].id;
    let point = h.state().session.map().layer_by_name("Places").unwrap().id;
    for _ in 0..3 {
        h.step();
    }
    h.get_by_label("My OGC Layer");
    h.get_by_label("Places");
    h.get_by_label("Routes");
    server.set(ITEMS, &fixture_features().replace("Test Place", "New Place"));
    let before = server.requests().len();
    h.get_by_label("Places").click_secondary();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    h.get_by_label("Refresh").click();
    wait_for_ogc(&mut h, |app| app.ui.service_import.is_none());
    assert!(serde_json::to_string(&h.state().session.map().layer(point).unwrap().dataset).unwrap().contains("New Place"));
    assert_eq!(server.requests()[before..].iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), [ROOT, CATALOG, ITEMS]);
    for _ in 0..3 {
        h.step();
    }
    let before = server.requests().len();
    h.get_by_label("My OGC Layer").click_secondary();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    h.get_by_label("Refresh").click();
    wait_for_ogc(&mut h, |app| app.ui.service_import.is_none());
    assert_eq!(server.requests()[before..].iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), [ROOT, CATALOG, ITEMS, LINE_ITEMS]);
    assert_eq!(h.state().session.map().service_groups[0].id, group);
    // Collapse hides child rows without removing data or their styles.
    h.state_mut().run("service.group", json!({"group":group,"expanded":false})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    assert!(h.query_by_label("Places").is_none());
    assert!(h.query_by_label("Routes").is_none());
    assert_eq!(h.state().session.map().layers.len(), 2);
}

#[test]
fn ogc_group_background_import_does_not_land_in_a_new_project() {
    use ogc_server::{ITEMS, LINE_ITEMS, ROOT, Route, Server, fixture_features, fixture_group_collections, fixture_lines};
    let mut route = Route::new(ROOT, fixture_group_collections());
    route.delay = std::time::Duration::from_millis(150);
    let server = Server::new(vec![route, Route::new(ITEMS, fixture_features()), Route::new(LINE_ITEMS, fixture_lines())]);
    let mut h = harness();
    h.state_mut().run("ui.importService", json!({"url":server.root()})).unwrap();
    wait_for_ogc(&mut h, |_| !server.requests().is_empty());
    h.state_mut().run("project.new", json!({"name":"Another project"})).unwrap();
    wait_for_ogc(&mut h, |app| app.ui.service_import.is_none());
    let until = std::time::Instant::now() + std::time::Duration::from_millis(250);
    while std::time::Instant::now() < until {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(h.state().session.map().layers.is_empty());
    assert!(h.state().session.map().service_groups.is_empty());
    assert_eq!(server.requests().len(), 1);
}

/// The right side keeps a tab for each pane opened (#303): opening shows a pane and adds its tab,
/// a ribbon toggle closes only the pane in view (and shows one open but not in view), a tab's ✕
/// closes its pane in view or not, the neighbouring tab takes over, pane sets keep the tabs, and
/// the header's last ✕ closes them all.
#[test]
fn right_panes_open_as_tabs() {
    use rustgis_ui_egui::RightPane::{Bookmarks, Geoprocessing, History};
    let mut h = harness();
    let tabs = |h: &egui_kittest::Harness<'_, RustGisApp>| (h.state().ui.right, h.state().ui.right_open.clone());
    for pane in ["ui.pane.history", "ui.pane.geoprocessing", "ui.pane.bookmarks"] {
        h.state_mut().run(pane, json!({})).unwrap();
        h.step();
    }
    assert_eq!(tabs(&h), (Some(Bookmarks), vec![History, Geoprocessing, Bookmarks]));
    // A toggle on a pane open but not in view shows it; on the one in view closes it alone.
    h.state_mut().run("ui.pane.history", json!({})).unwrap();
    assert_eq!(tabs(&h), (Some(History), vec![History, Geoprocessing, Bookmarks]));
    h.state_mut().run("ui.pane.history", json!({})).unwrap();
    assert_eq!(tabs(&h), (Some(Geoprocessing), vec![Geoprocessing, Bookmarks]), "the next tab takes over");
    // A tab's ✕ on a pane not in view leaves the one in view.
    h.state_mut().run("ui.pane.bookmarks", json!({"close": true})).unwrap();
    assert_eq!(tabs(&h), (Some(Geoprocessing), vec![Geoprocessing]));
    h.step();
    // Pane sets keep the tabs and the one in view.
    h.state_mut().run("ui.pane.history", json!({})).unwrap();
    h.state_mut().run("ui.paneSet", json!({"save": "work"})).unwrap();
    h.state_mut().run("ui.pane.history", json!({"close": true})).unwrap();
    h.state_mut().run("ui.pane.geoprocessing", json!({"close": true})).unwrap();
    assert_eq!(tabs(&h), (None, vec![]), "closing the last tab hides the right side");
    h.state_mut().run("ui.paneSet", json!({"load": "work"})).unwrap();
    assert_eq!(tabs(&h), (Some(History), vec![Geoprocessing, History]));
    // The header draws a tab for each, and the ✕ of one closes it.
    h.step();
    h.get_by_label("Close Tools").click();
    h.step();
    assert_eq!(tabs(&h), (Some(History), vec![History]));
    // The ✕ at the header's end closes them all.
    h.state_mut().run("ui.pane.bookmarks", json!({})).unwrap();
    h.step();
    h.get_by_label("Close all panes").click();
    h.step();
    assert_eq!(tabs(&h), (None, vec![]));
    // `close` means close for every pane command, whatever else it is given; on the left too.
    h.state_mut().run("ui.pane.sql", json!({"sql": "SELECT 1"})).unwrap();
    h.state_mut().run("ui.pane.sql", json!({"sql": "SELECT 2", "close": true})).unwrap();
    assert_eq!(tabs(&h), (None, vec![]));
    h.state_mut().run("ui.pane.assistant", json!({"ask": "hello", "close": true})).unwrap();
    assert_eq!(tabs(&h), (None, vec![]), "closing a pane that isn't open opens nothing");
    for _ in 0..2 {
        h.state_mut().run("ui.pane.contents", json!({"close": true})).unwrap();
        h.state_mut().run("ui.pane.catalog", json!({"close": true})).unwrap();
        h.state_mut().run("ui.pane.sources", json!({"close": true})).unwrap();
        assert_eq!(h.state().ui.left, None, "a left pane's close never opens one");
    }
}

#[test]
fn plugin_panels_run_their_commands_when_a_slider_is_let_go() {
    let _turn = PLUGINS.lock().unwrap_or_else(|e| e.into_inner());
    use rustgis_ui_egui::rustgis_engine::plugins as rp;
    rp::load(rp::Package::from_zip(&rp::testing::panel_package("ui-panel")).unwrap(), "test").unwrap();
    let mut h = harness();
    for _ in 0..4 {
        h.step();
    }
    // Its button on the Plugins tab opens it, and its state command fills it in.
    h.state_mut().ui.tab = "Plugins".into();
    h.step();
    h.step();
    h.get_by_label("Historical Imagery").click();
    for _ in 0..3 {
        h.step();
    }
    assert_eq!(h.state().ui.right, Some(rustgis_ui_egui::RightPane::Plugin));
    let release = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.plugin_panels["ui-panel"].slider("release");
    assert_eq!(release(&h), 12);
    assert!(h.query_by_label("Location metadata").is_some());
    // Dragging the slider shows where it would go and runs nothing until it is let go.
    let rect = h.query_all_by_label("Release timeline").map(|n| n.rect()).max_by(|a, b| a.width().total_cmp(&b.width())).unwrap();
    let (from, to) = (egui::pos2(rect.max.x - 4.0, rect.center().y), egui::pos2(rect.center().x, rect.center().y));
    h.event(egui::Event::PointerMoved(from));
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    for i in 1..=6 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.step();
    }
    assert_eq!(release(&h), 12, "nothing sent while dragging");
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
    h.step();
    let moved = release(&h);
    assert!((1..12).contains(&moved), "sent when let go: {moved}");
    // The step buttons and the checkbox run the widgets' command too.
    h.get_by_label("Step back").click();
    h.step();
    h.step();
    assert_eq!(release(&h), moved - 1);
    h.get_by_label("Only versions with local changes").click();
    h.step();
    h.step();
    assert!(h.state().session.plugin_panels["ui-panel"].checked("local"));
    // Its button closes it again; a disabled plugin's panel closes.
    h.state_mut().run("ui.pane.plugin", json!({"id": "ui-panel"})).unwrap();
    h.step();
    assert_eq!(h.state().ui.right, None);
    h.state_mut().run("ui.pane.plugin", json!({"id": "ui-panel"})).unwrap();
    h.step();
    rp::set_enabled("ui-panel", false).unwrap();
    h.step();
    h.step();
    assert_eq!(h.state().ui.right, None);
    assert!(h.state_mut().run("ui.pane.plugin", json!({"id": "ui-panel"})).is_err());
    assert!(h.state_mut().run("ui.pane.plugin", json!({"id": 7})).is_err());
    rp::unload("ui-panel");
}

/// A run in the History pane opens again in its tool with the values it ran with (#539), from
/// its ⋯ menu; the pane says when it ran.
#[test]
fn a_run_opens_again_in_its_tool_from_the_history() {
    use rustgis_ui_egui::RightPane;
    let mut h = harness();
    h.state_mut().run("gp.run", json!({"tool": "buffer", "params": {"input": "Cities", "distance": 100}})).unwrap();
    h.state_mut().run("ui.pane.history", json!({})).unwrap();
    h.step();
    h.step();
    assert!(h.query_all_by_label_contains(" UTC").next().is_some(), "when it ran");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "⋯").click_accesskit();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    h.get_by_label("Open in Tool").click_accesskit();
    h.step();
    let app = h.state();
    assert_eq!((app.ui.right, app.gp.tool), (Some(RightPane::Geoprocessing), Some("buffer")));
    assert_eq!(app.gp.values.get("distance").map(String::as_str), Some("100"));
    assert_eq!(app.gp.values.get("input").map(String::as_str), Some("Cities"));
}

/// The selected text, grabbed by its text, is dragged to another place (#516), as one undo step;
/// the map stays where it was.
#[test]
fn a_selected_text_is_dragged_on_the_map() {
    let mut h = harness();
    // The pane open first: opening narrows the map, which would move the text on the screen.
    h.state_mut().run("ui.pane.format", json!({})).unwrap();
    h.step();
    h.state_mut().run("tool.set", json!({"tool": "graphicText"})).unwrap();
    h.step();
    click_map(&mut h);
    assert_eq!(selected_text(&h).as_deref(), Some("Text"));
    // The pane took the focus to type over the text: Enter ends it.
    h.key_press(egui::Key::Enter);
    h.step();
    let anchor = |h: &egui_kittest::Harness<'_, RustGisApp>| {
        let l = h.state().session.map().layers.iter().find(|l| l.selection.len() == 1).unwrap();
        l.dataset.features[l.selection[0]].geometry.as_ref().unwrap().label_point().unwrap()
    };
    let (before, camera) = (anchor(&h), h.state().session.map().camera.center);
    let (from, to) = (egui::pos2(700.0, 500.0), egui::pos2(780.0, 500.0));
    h.input_mut().events.push(egui::Event::PointerMoved(from));
    h.input_mut().events.push(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    h.step();
    for x in [720.0, 750.0, 780.0] {
        h.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(x, 500.0)));
        h.step();
    }
    h.input_mut().events.push(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    h.step();
    h.step();
    let after = anchor(&h);
    assert!(after.x > before.x + 1e-6 && (after.y - before.y).abs() < 1e-6, "{before:?} → {after:?}");
    assert_eq!(h.state().session.map().camera.center, camera, "the map didn't pan");
    assert_eq!(h.state().session.undo_label(), Some("Edit Graphic"));
}

/// The text of the one text graphic selected, if that is what is selected.
fn selected_text(h: &egui_kittest::Harness<'_, RustGisApp>) -> Option<String> {
    use rustgis_ui_egui::rustgis_engine::map::graphics;
    let mut rows = h.state().session.map().layers.iter().flat_map(|l| l.selection.iter().map(move |r| (l, *r)));
    let (l, row) = rows.next().filter(|_| rows.next().is_none())?;
    graphics::label(&l.dataset, row).map(str::to_string)
}

/// A primary click in the middle of the map.
fn click_map(h: &mut egui_kittest::Harness<'_, RustGisApp>) {
    let at = egui::pos2(700.0, 500.0);
    h.input_mut().events.push(egui::Event::PointerMoved(at));
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        });
    }
    for _ in 0..4 {
        h.step();
    }
}

#[test]
fn dynamic_text_is_a_menu_of_values_that_starts_the_text() {
    // Clicked through accesskit: the group may be scrolled out of the window.
    let mut h = harness();
    h.state_mut().ui.tab = "Edit".into();
    h.step();
    h.get_by_label("Dynamic Text").click_accesskit();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    h.get_by_label("Map scale  {scale}").click_accesskit();
    h.step();
    assert_eq!(h.state().session.tool.code(), "graphicText");
    assert_eq!(h.state().ui.dynamic_text, Some("{scale}"));
    // The text a click on the map places starts with the value (#516: selected, in the Format
    // pane).
    click_map(&mut h);
    assert_eq!(selected_text(&h).as_deref(), Some("{scale}"));
    assert_eq!(h.state().ui.right, Some(rustgis_ui_egui::RightPane::Format));
    // Text starts with "Text" again. The pane is closed first: its Text section's header is a
    // button too.
    h.state_mut().run("ui.pane.format", json!({"close": true})).unwrap();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Text").click_accesskit();
    h.step();
    assert_eq!(h.state().ui.dynamic_text, None);
    click_map(&mut h);
    assert_eq!(selected_text(&h).as_deref(), Some("Text"));
    // Another tool forgets the value.
    h.get_by_label("Dynamic Text").click_accesskit();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    h.get_by_label("Current date  {date}").click_accesskit();
    h.step();
    assert_eq!(h.state().ui.dynamic_text, Some("{date}"));
    h.state_mut().run("tool.set", json!({"tool": "explore"})).unwrap();
    h.step();
    assert_eq!(h.state().ui.dynamic_text, None);
    // A command, for the Assistant and scripts too: only the map's values.
    h.state_mut().run("ui.dynamicText", json!({"text": "{crs}"})).unwrap();
    assert_eq!((h.state().session.tool.code(), h.state().ui.dynamic_text), ("graphicText", Some("{crs}")));
    assert!(h.state_mut().run("ui.dynamicText", json!({"text": "{page}"})).is_err());
    assert!(h.state_mut().run("ui.dynamicText", json!({})).is_err());
    assert_eq!(h.state().ui.dynamic_text, Some("{crs}"), "a refused value leaves the one picked");
    // Choosing Text again by command (Command Search, a script) starts plain text.
    h.state_mut().run("tool.set", json!({"tool": "graphicText"})).unwrap();
    assert_eq!(h.state().ui.dynamic_text, None);
}

#[test]
fn popup_titles_leave_room_for_controls() {
    use egui::accesskit::Role;

    for title in ["Oak".to_string(), "HIDDEN INTERSECTION Tab Sign (Oversized) ".repeat(4), "長い地物名".repeat(30)] {
        for count in [1, 2] {
            let mut h = harness();
            h.ctx.set_missing_glyph_policy(egui::MissingGlyphPolicy::Tofu);
            let features: Vec<_> = (0..count)
                .map(|_| json!({"type": "Feature", "properties": {"name": title}, "geometry": {"type": "Point", "coordinates": [0, 0]}}))
                .collect();
            h.state_mut()
                .run(
                    "layer.addDataText",
                    json!({
                        "fileName": "popup.geojson",
                        "name": "A layer with a name much longer than the popup header can hold",
                        "text": json!({"type": "FeatureCollection", "features": features}).to_string()
                    }),
                )
                .unwrap();
            h.state_mut().run("map.zoomToExtent", json!({"extent": [-5, -4, 5, 4]})).unwrap();
            h.ctx.data_mut(|d| d.insert_temp(egui::Id::unique("identify_popup_limit"), egui::vec2(240.0, 360.0)));
            h.state_mut().run("ui.identify", json!({"lon": 0, "lat": 0})).unwrap();
            h.run_steps(6);

            let title_rect = h.query_all_by_label(&title).map(|n| n.rect()).min_by(|a, b| a.min.y.total_cmp(&b.min.y)).unwrap();
            let close = h.get_by_role_and_label(Role::Button, "×").rect();
            assert!(close.max.x <= title_rect.min.x - 12.0 + 240.0, "close must stay inside the popup");
            assert!(title_rect.max.x < close.min.x, "title must leave room for close");
            if count > 1 {
                let previous = h.get_by_role_and_label(Role::Button, "‹").rect();
                assert!(title_rect.max.x < previous.min.x, "title must leave room for all navigation controls");
                h.get_by_role_and_label(Role::Button, "›").click();
                h.run_steps(3);
                assert_eq!(h.state().map.popup.as_ref().unwrap().index, 1);
                h.get_by_role_and_label(Role::Button, "‹").click();
                h.run_steps(3);
                assert_eq!(h.state().map.popup.as_ref().unwrap().index, 0);
            }
            h.get_by_role_and_label(Role::Button, "×").click();
            h.run_steps(3);
            assert!(h.state().map.popup.is_none(), "close must remain clickable");
        }
    }
}

#[test]
fn the_popup_closes_when_its_feature_is_deleted() {
    let mut h = harness();
    h.step();
    let graphics = || json!({"shape": "point", "points": [[-30.0, -40.0]]});
    h.state_mut().run("graphics.add", graphics()).unwrap();
    h.state_mut().run("ui.identify", json!({"lon": -30.0, "lat": -40.0})).unwrap();
    h.step();
    // The feature the popup shows, by its feature id.
    let shown = |h: &egui_kittest::Harness<'_, RustGisApp>| {
        h.state().map.popup.as_ref().and_then(|p| p.info["results"].get(p.index).unwrap_or(&p.info)["fid"].as_u64())
    };
    let fid = shown(&h);
    assert!(fid.is_some());
    // Another feature deleted: the popup stays.
    h.state_mut().run("graphics.add", json!({"shape": "point", "points": [[30.0, -40.0]]})).unwrap();
    h.state_mut().run("select.rows", json!({"layer": "Graphics", "rows": [1]})).unwrap();
    h.state_mut().run("edit.delete", json!(null)).unwrap();
    h.step();
    assert_eq!(shown(&h), fid, "another feature's deletion leaves it on its feature");
    // Its own feature deleted: it closes.
    h.state_mut().run("select.rows", json!({"layer": "Graphics", "rows": [0]})).unwrap();
    h.state_mut().run("edit.delete", json!(null)).unwrap();
    h.step();
    assert!(h.state().map.popup.is_none(), "the popup outlived its feature");
    // Another map in view: a duplicate has the same layer and feature ids, but it's not the
    // map the popup was opened on.
    h.state_mut().run("graphics.add", graphics()).unwrap();
    h.state_mut().run("ui.identify", json!({"lon": -30.0, "lat": -40.0})).unwrap();
    h.step();
    assert!(h.state().map.popup.is_some());
    h.state_mut().run("map.duplicate", json!({})).unwrap();
    h.step();
    assert!(h.state().map.popup.is_none(), "the popup stayed on another map");
}

#[test]
fn a_shortcut_that_cannot_run_says_why() {
    let mut h = harness();
    h.step();
    h.state_mut().run("edit.enable", json!({"on": false})).unwrap();
    h.state_mut().run("select.rows", json!({"layer": "Countries", "rows": [0]})).unwrap();
    let rows =
        |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.map().layer_by_name("Countries").map(|l| l.dataset.features.len());
    let before = rows(&h);
    let mark = h.state().session.notice_mark();
    h.key_press(egui::Key::Delete);
    h.step();
    assert_eq!(rows(&h), before, "nothing deleted");
    let said = h.state().session.notices_since(mark);
    assert!(said.iter().any(|n| n.error && n.text.contains("editing is turned off")), "{said:?}");
    // Pressed again (or held) while it shows: no second notice.
    let mark = h.state().session.notice_mark();
    h.key_press(egui::Key::Delete);
    h.step();
    assert!(h.state().session.notices_since(mark).is_empty());
    // Nothing left to undo isn't worth a notice.
    while h.state().session.can_undo() {
        h.state_mut().run("edit.undo", json!(null)).unwrap();
    }
    let mark = h.state().session.notice_mark();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.step();
    assert!(h.state().session.notices_since(mark).is_empty());
    // Exhausted redo is equally quiet; available redo still runs.
    while h.state().session.can_redo() {
        h.state_mut().run("edit.redo", json!(null)).unwrap();
    }
    let mark = h.state().session.notice_mark();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Y);
    h.step();
    assert!(h.state().session.notices_since(mark).is_empty());
}

#[test]
fn ctrl_shift_z_redoes_rather_than_undoing() {
    let mut h = harness();
    h.step();
    let rows =
        |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.map().layer_by_name("Countries").map(|l| l.dataset.features.len());
    let before = rows(&h);
    h.state_mut().run("select.rows", json!({"layer": "Countries", "rows": [0]})).unwrap();
    h.state_mut().run("edit.delete", json!(null)).unwrap();
    let deleted = rows(&h);
    assert_ne!(deleted, before);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.step();
    assert_eq!(rows(&h), before, "Ctrl+Z undoes");
    // egui ignores an extra Shift when matching: the more specific key has to be tried first.
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
    h.step();
    assert_eq!(rows(&h), deleted, "Ctrl+Shift+Z redoes");
}

#[test]
fn help_shortcuts_lists_the_keymap() {
    let mut h = harness();
    h.step();
    h.state_mut().run("ui.shortcuts", json!(null)).unwrap();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    // Named by the OS egui reads (a browser's from its user agent), not the build's (#799): the
    // harness tells Linux, whatever runs it.
    assert!(h.query_by_label("Ctrl+Z").is_some(), "Help › Shortcuts lists Undo's key");
    h.ctx.set_os(egui::os::OperatingSystem::Mac);
    h.step();
    assert!(h.query_by_label("⌘Z").is_some(), "with ⌘ on a Mac");
    assert!(h.query_by_label("Map Tool: Explore").is_some(), "and what Esc does");
}

#[test]
fn geometry_calculator_switching_back_creates_a_unique_field() {
    use rustgis_ui_egui::rustgis_engine::{
        data::{Dataset, Field, FieldType, Geometry, GeometryType, Value},
        geom::Coord,
    };

    let mut h = harness();
    h.state_mut().run("ui.language", json!({"code": "en"})).unwrap();

    let mut ds = Dataset::new("Geometry mode test", GeometryType::Point, vec![Field::new("x_deg", FieldType::Double)]);
    ds.push(Some(Geometry::Point(Coord::new(117.0, 3.0))), vec![Value::Float(99.0)]);

    let id = h.state_mut().session.map_mut().add_dataset(ds);
    // Another layer is active: the dialog still calculates the table's layer.
    let mut other = Dataset::new("Active layer", GeometryType::Point, vec![Field::new("x_deg", FieldType::Double)]);
    other.push(Some(Geometry::Point(Coord::new(10.0, 3.0))), vec![Value::Float(5.0)]);
    let other = h.state_mut().session.map_mut().add_dataset(other);
    h.state_mut().session.active_layer = Some(other);
    h.state_mut().run("ui.dialog", json!({"name": "calculateGeometry", "layer": id})).unwrap();
    h.step();
    h.step();

    h.get_by_label("Existing field").click();
    h.step();
    h.step();

    h.get_by_label("New field").click();
    h.step();
    h.step();

    h.get_by_role_and_label(egui::accesskit::Role::Button, "Calculate").click();
    h.step();
    h.step();

    assert!(h.state().dialogs.open.is_none(), "calculation should close the dialog");
    let ds = &h.state().session.map().layer(id).unwrap().dataset;
    let old = ds.field_index("x_deg").unwrap();
    let new = ds.field_index("x_deg_2").expect("a unique field should be created");
    assert_eq!(ds.value(0, old).as_f64(), Some(99.0));
    assert_eq!(ds.value(0, new).as_f64(), Some(117.0));
    let active = &h.state().session.map().layer(other).unwrap().dataset;
    assert!(active.field_index("x_deg_2").is_none(), "the active layer is left alone");
    assert_eq!(active.value(0, 0).as_f64(), Some(5.0));
}

#[test]
fn unique_symbology_lets_the_user_choose_a_text_field_before_applying() {
    use rustgis_ui_egui::rustgis_engine::data::{Dataset, Field, FieldType, Geometry, GeometryType, Value};
    use rustgis_ui_egui::rustgis_engine::geom::Coord;

    let mut h = harness();
    let mut ds = Dataset::new(
        "Text attributes",
        GeometryType::Point,
        vec![Field::new("id", FieldType::Integer), Field::new("category", FieldType::Text), Field::new("other", FieldType::Text)],
    );
    ds.push(Some(Geometry::Point(Coord::new(117.0, 3.0))), vec![Value::Int(1), Value::Text("village".into()), Value::Text("town".into())]);
    let id = h.state_mut().session.map_mut().add_dataset(ds);
    h.state_mut().session.active_layer = Some(id);
    h.state_mut().run("ui.pane.symbology", json!(null)).unwrap();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Unique Values").click();
    h.step();
    assert_eq!(h.state().ui.symbology_pending.as_ref().map(|pending| pending.field.as_str()), Some("category"));
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Field").click();
    h.step();
    h.step(); // Combo popups enter the accessibility tree on the following frame.
    h.get_by_label("other").click();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Apply").click();
    h.step();
    let layer = h.state().session.map().layer(id).unwrap();
    assert!(matches!(layer.renderer, rustgis_ui_egui::rustgis_engine::map::Renderer::Unique { ref field, .. } if field == "other"));
    assert_eq!(h.state().ui.symbology_pending, None);
}

#[test]
fn unique_symbology_pending_choice_follows_its_context() {
    for change in ["map", "project", "renderer", "undo", "external renderer", "layer"] {
        let mut h = harness();
        h.state_mut().run("layer.select", json!({"layer": "Cities"})).unwrap();
        h.state_mut().run("layer.symbology", json!({"type": "unique", "field": "name"})).unwrap();
        h.state_mut().run("layer.symbology", json!({"type": "single"})).unwrap();
        // A second map deliberately has the same layer IDs and renderer as the first.
        let mut other = h.state().session.map().clone();
        other.name = "Other map".into();
        h.state_mut().session.project.maps.push(other);
        h.state_mut().run("ui.pane.symbology", json!(null)).unwrap();
        h.step();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Unique Values").click();
        h.step();
        assert!(h.state().ui.symbology_pending.is_some());
        // Failed commands leave the valid choice available for retry.
        assert!(h.state_mut().run("layer.symbology", json!({"type": "unique", "field": "missing field"})).is_err());
        assert!(h.state().ui.symbology_pending.is_some());
        if change != "external renderer" {
            h.state_mut().ui.close_right(rustgis_ui_egui::RightPane::Symbology);
        }
        match change {
            "map" => {
                h.state_mut().run("map.activate", json!({"name": "Other map"})).unwrap();
            }
            "project" => {
                h.state_mut().run("project.sample", json!(null)).unwrap();
            }
            "renderer" => {
                h.state_mut().run("layer.symbology", json!({"type": "unique", "field": "name"})).unwrap();
            }
            "undo" => {
                h.state_mut().run("edit.undo", json!(null)).unwrap();
            }
            "external renderer" => {
                // Commands from other clients can bypass RustGisApp::run.
                h.state_mut().session.run("layer.symbology", &json!({"type": "unique", "field": "name"})).unwrap();
                h.step();
            }
            "layer" => {
                h.state_mut().run("layer.select", json!({"layer": "Countries"})).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(h.state().ui.symbology_pending.is_none(), "pending choice survived {change}");
    }
}

#[test]
fn map_and_layout_tabs_rename_from_their_menus() {
    let mut h = harness();
    h.state_mut().run("ui.language", json!({"code":"en"})).unwrap();
    h.state_mut().run("map.rename", json!({"name": "Tab Map"})).unwrap();
    h.state_mut().run("layout.new", json!({"name": "Tab Layout"})).unwrap();
    h.state_mut().run("layout.close", json!(null)).unwrap();
    for (tab, new) in [("Tab Map", "Renamed Map"), ("Tab Layout", "Renamed Layout")] {
        for _ in 0..4 {
            h.step();
        }
        // The tab, not the map's name elsewhere in the window.
        h.get_by(|n| n.role() == egui::accesskit::Role::Button && n.label().as_deref() == Some(tab)).click_secondary();
        for _ in 0..4 {
            h.step();
        }
        h.get_by_label("Rename…").click();
        for _ in 0..4 {
            h.step();
        }
        assert_eq!(h.state().dialogs.a, tab);
        h.state_mut().dialogs.text = new.into();
        h.get_by_label("Rename").click();
        for _ in 0..4 {
            h.step();
        }
        assert!(h.state().dialogs.open.is_none(), "{:?}", h.state().dialogs.error);
    }
    let p = &h.state().session.project;
    assert_eq!(p.maps[0].name, "Renamed Map");
    assert_eq!(p.layouts.last().map(|l| l.name.as_str()), Some("Renamed Layout"));
}

#[test]
fn line_strokes_editor_edits_one_class() {
    use rustgis_engine::map::symbology::ClassRef;
    let mut h = harness();
    let app = h.state_mut();
    app.run("layer.select", json!({"layer": "Rivers"})).unwrap();
    app.run("layer.symbology", json!({"layer": "Rivers", "type": "unique", "field": "scalerank"})).unwrap();
    let pair = json!([{"color": "#000000", "width": 6}, {"color": "#FFFFFF", "width": 2}]);
    app.run("layer.lineStrokes", json!({"layer": "Rivers", "value": "1", "strokes": pair})).unwrap();
    app.run("ui.pane.symbology", json!({})).unwrap();
    h.step();
    h.step();
    let class_strokes = |h: &egui_kittest::Harness<'_, RustGisApp>, c: &ClassRef| {
        h.state().session.map().layer_by_name("Rivers").unwrap().renderer.class_strokes(c).unwrap().len()
    };
    // The layer's strokes first; class 1 is marked as having its own.
    h.query_all_by_role(egui::accesskit::Role::ComboBox).find(|n| n.value().as_deref() == Some("All classes")).unwrap().click();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    h.get_by_label("1 •").click_accesskit();
    h.step();
    h.step();
    assert!(h.query_by_label("Stroke 2").is_some(), "class 1's two strokes are listed");
    // Back to the layer's strokes: the class borrows them, and Add Stroke gives it its own again.
    h.get_by_label("Use the Layer's Strokes").click_accesskit();
    h.step();
    h.step();
    assert_eq!(class_strokes(&h, &ClassRef::Value("1".into())), 0);
    assert!(h.query_by_label_contains("drawn with the layer's strokes").is_some());
    h.get_by_label("Add Stroke").click_accesskit();
    h.step();
    h.step();
    assert_eq!(class_strokes(&h, &ClassRef::Value("1".into())), 1);
    assert!(h.state().session.map().layer_by_name("Rivers").unwrap().line_strokes.is_empty(), "the layer's own are untouched");
}

/// Out-of-range band input shows the raster's range and keeps calculation disabled.
#[test]
fn zonal_statistics_shows_available_band_range() {
    use egui_kittest::kittest::NodeT;
    use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster};
    let mut h = harness();
    let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
    let raster = Raster::new("dem", 1, 1, vec![vec![1.0]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
    h.state_mut().session.map_mut().add_raster("dem", rustgis_engine::map::RasterLayer::from_path(raster, "dem.tif"));
    h.state_mut().run("ui.pane.workflows", json!(null)).unwrap();
    h.state_mut().workflow.section = "zonalStats";
    h.state_mut().workflow.layer = "dem".into();
    h.state_mut().workflow.layer2 = "Countries".into();
    h.state_mut().workflow.zonal_stats.bands = "2".into();
    h.run_steps(4);
    assert!(h.query_by_label("Raster has bands 1 to 1.").is_some());
    assert!(h.query_by_label("Band must be a positive integer, list of bands, or 'all'.").is_some());
    assert!(h.get_by_label("Calculate Statistics").accesskit_node().is_disabled());
}
