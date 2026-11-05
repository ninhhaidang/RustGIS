//! Interaction tests for customizable shortcuts.
use egui_kittest::kittest::Queryable;
use rustgis_engine::{Project, Session};
use rustgis_ui_egui::{RustGisApp, Services};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

fn harness(prefs: Arc<Mutex<HashMap<String, String>>>) -> egui_kittest::Harness<'static, RustGisApp> {
    let reads = prefs.clone();
    let writes = prefs;
    let services = Services {
        load_pref: Some(Box::new(move |key| reads.lock().unwrap().get(key).cloned())),
        save_pref: Some(Box::new(move |key, value| {
            writes.lock().unwrap().insert(key.to_string(), value.to_string());
        })),
        ..Services::default()
    };
    let app = RustGisApp::new(Session::new(Project::default())).with_services(services);
    egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    )
}

#[test]
fn assigns_a_key_confirms_conflicts_and_restores_saved_preferences() {
    let prefs = Arc::new(Mutex::new(HashMap::new()));
    let mut h = harness(prefs.clone());
    h.state_mut().run("ui.customizeShortcuts", Value::Null).unwrap();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Search commands").focus();
    h.event(egui::Event::Text("Toggle Dark Mode".into()));
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Add…").click();
    h.step();
    h.key_press(egui::Key::T);
    h.run_steps(3);
    assert!(prefs.lock().unwrap().get("shortcut_changes").is_some_and(|p| p.contains("ui.theme")));

    h.get_by_role_and_label(egui::accesskit::Role::Button, "Add…").click();
    h.step();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::T);
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Cancel").click();
    h.run_steps(3);
    let before = prefs.lock().unwrap().get("shortcut_changes").cloned().unwrap();
    assert!(!before.contains("Mod+T"));

    let restored = harness(prefs);
    assert!(restored.state().ui.keymap.bindings.iter().any(|b| b.command == "ui.theme" && b.chord.key == egui::Key::T));
    assert!(
        restored.state().ui.keymap.bindings.iter().any(|b| b.command == "table.open" && b.chord.key == egui::Key::T && b.chord.command)
    );
}

#[test]
fn tab_and_arrow_keys_can_be_captured() {
    let mut h = harness(Arc::new(Mutex::new(HashMap::new())));
    h.state_mut().run("ui.customizeShortcuts", Value::Null).unwrap();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Search commands").focus();
    h.event(egui::Event::Text("Toggle Dark Mode".into()));
    h.run_steps(3);
    for key in [egui::Key::Tab, egui::Key::ArrowLeft] {
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Add…").click();
        h.step();
        h.key_press(key);
        h.run_steps(3);
        assert!(h.state().ui.keymap.bindings.iter().any(|b| b.command == "ui.theme" && b.chord.key == key), "{key:?} was not captured");
    }
}

#[test]
fn escape_cancels_capture_but_can_be_assigned_explicitly_and_reset() {
    let prefs = Arc::new(Mutex::new(HashMap::new()));
    let mut h = harness(prefs.clone());
    h.state_mut().run("ui.customizeShortcuts", Value::Null).unwrap();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Search commands").focus();
    h.event(egui::Event::Text("Toggle Dark Mode".into()));
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Add…").focus();
    h.run_steps(3);
    h.key_press(egui::Key::Enter);
    h.step();
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.query_by_label("Assign Escape").is_none());
    assert!(prefs.lock().unwrap().get("shortcut_changes").is_none());
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Add…").click();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Assign Escape").click();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Replace").click();
    h.run_steps(3);
    assert!(h.state().ui.keymap.bindings.iter().any(|b| b.command == "ui.theme" && b.chord.key == egui::Key::Escape));
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Reset All").click();
    h.run_steps(3);
    assert!(h.state().ui.keymap.bindings.iter().any(|b| b.command == "tool.set" && b.chord.key == egui::Key::Escape));
    assert!(!h.state().ui.keymap.bindings.iter().any(|b| b.command == "ui.theme"));
    assert_eq!(prefs.lock().unwrap().get("shortcut_changes").unwrap(), "[]");
}

#[test]
fn command_search_context_menu_opens_matching_editor_row() {
    let mut h = harness(Arc::new(Mutex::new(HashMap::new())));
    h.state_mut().run("ui.commandSearch", Value::Null).unwrap();
    h.run_steps(3);
    h.event(egui::Event::Text("Toggle Dark Mode".into()));
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Toggle Dark Mode Settings › Theme").click_secondary();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Keyboard Shortcuts").click();
    h.run_steps(3);
    // The command-id prefix also matches Choose Theme and Reload Themes; filter the intended row.
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Search commands").focus();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("Toggle Dark Mode".into()));
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Add…").click();
    h.step();
    h.key_press(egui::Key::T);
    h.run_steps(3);
    assert!(h.state().ui.keymap.bindings.iter().any(|b| b.command == "ui.theme" && b.chord.key == egui::Key::T));
}

#[test]
fn unreadable_preferences_survive_edits_until_an_explicit_reset() {
    let too_many = serde_json::to_string(&vec![serde_json::json!({"keys":"D","command":"select.clear"}); 1_001]).unwrap();
    for original in ["not json".to_string(), " ".repeat(rustgis_ui_egui::SHORTCUT_FILE_MAX_BYTES + 1), too_many] {
        let prefs = Arc::new(Mutex::new(HashMap::from([("shortcut_changes".into(), original.clone())])));
        let mut h = harness(prefs.clone());
        h.state_mut().run("ui.customizeShortcuts", Value::Null).unwrap();
        h.run_steps(3);
        assert!(h.query_by_label("Saved shortcuts are unreadable or exceed the limit. Changes apply only to this session; Reset All replaces the saved shortcuts.").is_some());
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Search commands").focus();
        h.event(egui::Event::Text("Toggle Dark Mode".into()));
        h.run_steps(3);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Add…").click();
        h.step();
        h.key_press(egui::Key::T);
        h.run_steps(3);
        assert!(h.state().ui.keymap.bindings.iter().any(|b| b.command == "ui.theme"));
        assert_eq!(prefs.lock().unwrap().get("shortcut_changes").unwrap(), &original);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Reset All").click();
        h.run_steps(3);
        assert_eq!(prefs.lock().unwrap().get("shortcut_changes").unwrap(), "[]");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Add…").click();
        h.step();
        h.key_press(egui::Key::T);
        h.run_steps(3);
        let reloaded = harness(prefs);
        assert!(reloaded.state().ui.keymap.bindings.iter().any(|b| b.command == "ui.theme" && b.chord.key == egui::Key::T));
    }
}

/// Preview and confirmation drive the same widgets used after a desktop or browser file pick.
#[test]
fn import_preview_cancel_confirm_and_export_round_trip() {
    use serde_json::json;
    let prefs = Arc::new(Mutex::new(HashMap::new()));
    let mut h = harness(prefs.clone());
    let text = json!({"rustgis_shortcuts":1,"bindings":[
        {"keys":"Mod+T","command":"tool.set","params":{"tool":"select"}},
        {"keys":"F8","command":"plugin.not-installed","params":{"distance":12.5}},
        {"keys":"Home","command":"map.zoomFull","remove":true}
    ]})
    .to_string();
    let before = h.state().ui.keymap.clone();
    let summary = h.state_mut().run("ui.shortcuts.import", json!({"text":text})).unwrap();
    assert_eq!(summary, json!({"bindings":3,"conflicts":1,"unknown_commands":1}));
    assert_eq!(h.state().ui.keymap, before);
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Cancel").click();
    h.run_steps(3);
    assert_eq!(h.state().ui.keymap, before);
    assert!(!prefs.lock().unwrap().contains_key("shortcut_changes"));
    h.state_mut().run("ui.shortcuts.import", json!({"text":text})).unwrap();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Import").click();
    h.run_steps(3);
    assert_ne!(h.state().ui.keymap, before);
    let exported = h.state_mut().run("ui.shortcuts.export", json!({"text":true})).unwrap();
    let document: Value = serde_json::from_str(exported["text"].as_str().unwrap()).unwrap();
    assert_eq!(document["rustgis_shortcuts"], 1);
    assert!(document["bindings"].as_array().unwrap().len() < before.bindings.len());
    assert!(exported["text"].as_str().unwrap().contains("Mod+T"));
    let restored = harness(prefs);
    assert_eq!(restored.state().ui.keymap, h.state().ui.keymap);
    let mut other = harness(Arc::new(Mutex::new(HashMap::new())));
    other.state_mut().run("ui.shortcuts.import", json!({"text":exported["text"],"confirm":true})).unwrap();
    assert_eq!(other.state().ui.keymap, h.state().ui.keymap);
}

#[test]
fn invalid_imports_are_atomic_and_shortcut_commands_reset_defaults() {
    use serde_json::json;
    let mut h = harness(Arc::new(Mutex::new(HashMap::new())));
    let before = h.state().ui.keymap.clone();
    for text in [
        "not json".to_string(),
        json!({"rustgis_shortcuts":2,"bindings":[]}).to_string(),
        json!({"rustgis_shortcuts":1,"bindings":[{"keys":"D","command":"select.clear"},{"keys":"Mod+C","command":"ui.theme"}]}).to_string(),
        json!({"rustgis_shortcuts":1,"bindings":[{"keys":"D","command":"select.clear"},{"keys":42}]}).to_string(),
        json!({"rustgis_shortcuts":1,"bindings":vec![json!({"keys":"D","command":"select.clear"});1001]}).to_string(),
        " ".repeat(rustgis_ui_egui::SHORTCUT_FILE_MAX_BYTES + 1),
    ] {
        assert!(h.state_mut().run("ui.shortcuts.import", json!({"text":text,"confirm":true})).is_err());
        assert_eq!(h.state().ui.keymap, before);
    }
    h.state_mut().run("ui.shortcuts.set", json!({"keys":"D","command":"tool.set","params":{"tool":"select"}})).unwrap();
    assert_ne!(h.state().ui.keymap, before);
    h.state_mut().run("ui.shortcuts.reset", json!({"keys":"D"})).unwrap();
    assert_eq!(h.state().ui.keymap, before);
    h.state_mut().run("ui.shortcuts.set", json!({"keys":"Mod+T","command":"select.clear"})).unwrap();
    h.state_mut().run("ui.shortcuts.reset", Value::Null).unwrap();
    assert_eq!(h.state().ui.keymap, before);
}

#[test]
fn browser_picker_payload_and_download_use_the_portable_document() {
    let picked = Arc::new(Mutex::new(String::new()));
    let downloaded = Arc::new(Mutex::new(Vec::new()));
    let pick = picked.clone();
    let download = downloaded.clone();
    let mut h = harness(Arc::new(Mutex::new(HashMap::new())));
    h.state_mut().services.open_async = Some(Box::new(move |purpose| *pick.lock().unwrap() = purpose.to_string()));
    h.state_mut().services.download = Some(Box::new(move |name, bytes| {
        download.lock().unwrap().push((name.to_owned(), bytes));
        Ok(())
    }));
    // A cancelled item picker leaves legacy routing state; the next picker must supersede it.
    h.state_mut().run("ui.openItem", serde_json::json!({"kind":"styles"})).unwrap();
    h.state_mut().run("ui.shortcuts.import", Value::Null).unwrap();
    assert_eq!(*picked.lock().unwrap(), "shortcuts");
    // Cancelling returns no payload. An unrelated drop must still use normal data routing.
    h.state_mut().open_bytes("drop.geojson", br#"{"type":"FeatureCollection","features":[]}"#).unwrap();
    let text = r#"{"rustgis_shortcuts":1,"bindings":[{"keys":"D","command":"select.clear"}]}"#;
    let marked = format!("{}class.json", rustgis_ui_egui::SHORTCUT_PICK);
    h.state_mut().open_picked(&marked, text.as_bytes()).unwrap();
    h.run_steps(3);
    assert!(h.query_by_label("Bindings: 1 · Conflicts: 0 · Unknown commands: 0").is_some());
    assert!(!h.state().ui.keymap.bindings.iter().any(|b| b.chord.key == egui::Key::D && !b.chord.command));
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Import").click();
    h.run_steps(3);
    assert!(h.state().ui.keymap.bindings.iter().any(|b| b.chord.key == egui::Key::D && b.command == "select.clear"));
    h.state_mut().run("ui.shortcuts.export", Value::Null).unwrap();
    let files = downloaded.lock().unwrap();
    assert_eq!(files[0].0, "rustgis-shortcuts.json");
    let document: Value = serde_json::from_slice(&files[0].1).unwrap();
    assert_eq!(document["bindings"][0]["command"], "select.clear");
}

#[test]
fn import_counts_final_conflicting_keys_and_recognizes_help() {
    use serde_json::json;
    let mut app = RustGisApp::new(Session::new(Project::default()));
    let text = json!({"rustgis_shortcuts":1,"bindings":[
        {"keys":"Mod+T","command":"select.clear"},
        {"keys":"Mod+T","command":"ui.shortcuts"},
        {"keys":"F8","command":"ui.theme"},
        {"keys":"F8","command":"select.clear"},
        {"keys":"F9","command":"ui.about"},
        {"keys":"F10","command":"ui.pane.contents"},
        {"keys":"F11","command":"plugin:absent/h","remove":true},
        {"keys":"F12","command":"plugin:overwritten/h"},
        {"keys":"F12","command":"ui.theme"}
    ]})
    .to_string();
    let summary = app.run("ui.shortcuts.import", json!({"text":text})).unwrap();
    assert_eq!(summary, json!({"bindings":9,"conflicts":1,"unknown_commands":0}));
}

#[test]
fn desktop_file_round_trip_and_failed_download() {
    use serde_json::json;
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("keys.json");
    let mut app = RustGisApp::new(Session::new(Project::default()));
    app.run("ui.shortcuts.set", json!({"keys":"F8","command":"select.clear"})).unwrap();
    app.run("ui.shortcuts.export", json!({"path":path})).unwrap();
    app.run("ui.shortcuts.reset", Value::Null).unwrap();
    app.run("ui.shortcuts.import", json!({"path":path,"confirm":true})).unwrap();
    assert!(app.ui.keymap.bindings.iter().any(|b| b.chord.key == egui::Key::F8 && b.command == "select.clear"));
    app.services.download = Some(Box::new(|_, _| Err("download rejected".into())));
    assert_eq!(app.run("ui.shortcuts.export", Value::Null).unwrap_err(), "download rejected");
}

#[test]
fn imported_unknown_plugin_binding_runs_after_plugin_load() {
    use rustgis_engine::plugins as rp;
    use serde_json::json;
    let mut h = harness(Arc::new(Mutex::new(HashMap::new())));
    let text = json!({"rustgis_shortcuts":1,"bindings":[{"keys":"F8","command":"plugin:shortcut-late/f","params":{"op":"host.notify","params":{"text":"imported shortcut ran"}}}]}).to_string();
    let summary = h.state_mut().run("ui.shortcuts.import", json!({"text":text,"confirm":true})).unwrap();
    assert_eq!(summary["unknown_commands"], 1);
    rp::load(rp::Package::from_zip(&rp::testing::package("shortcut-late", &[])).unwrap(), "test").unwrap();
    h.key_press(egui::Key::F8);
    h.run_steps(3);
    // Only real keyboard dispatch through the newly loaded plugin emits this notification.
    assert!(h.state().session.notices.iter().any(|n| n.text.contains("imported shortcut ran")));
    assert!(rp::unload("shortcut-late"));
}

/// Saved preferences remain forward tolerant, while imported typos are rejected atomically.
#[test]
fn preference_extra_fields_load_but_import_extra_fields_are_rejected() {
    use serde_json::json;
    let prefs = Arc::new(Mutex::new(HashMap::from([(
        "shortcut_changes".into(),
        r#"[{"keys":"D","command":"select.clear","future":true}]"#.into(),
    )])));
    let mut h = harness(prefs.clone());
    assert!(h.state().ui.keymap.bindings.iter().any(|b| b.chord.key == egui::Key::D && b.command == "select.clear"));
    h.state_mut().run("ui.shortcuts.set", json!({"keys":"F8","command":"ui.theme"})).unwrap();
    assert!(prefs.lock().unwrap()["shortcut_changes"].contains("F8"), "extra fields must not block subsequent saves");
    let reloaded = harness(prefs.clone());
    assert!(reloaded.state().ui.keymap.bindings.iter().any(|b| b.chord.key == egui::Key::D && b.command == "select.clear"));
    let before = h.state().ui.keymap.clone();
    let text = json!({"rustgis_shortcuts":1,"bindings":[{"keys":"D","command":"ui.theme","parms":{}}]}).to_string();
    assert!(h.state_mut().run("ui.shortcuts.import", json!({"text":text,"confirm":true})).is_err());
    assert_eq!(h.state().ui.keymap, before);
}

/// Export/import must work even when valid saved preferences reach the exact payload limit.
#[test]
fn full_size_saved_preferences_export_compactly_and_import_again() {
    use serde_json::json;
    let prefs = Arc::new(Mutex::new(HashMap::new()));
    let mut h = harness(prefs.clone());
    h.state_mut().run("ui.shortcuts.set", json!({"keys":"F8","command":"ui.theme","params":{"value":""}})).unwrap();
    let overhead = prefs.lock().unwrap()["shortcut_changes"].len();
    let value = "x".repeat(100_000 - overhead);
    h.state_mut().run("ui.shortcuts.set", json!({"keys":"F8","command":"ui.theme","params":{"value":value}})).unwrap();
    assert_eq!(prefs.lock().unwrap()["shortcut_changes"].len(), 100_000);
    let exported = h.state_mut().run("ui.shortcuts.export", json!({"text":true})).unwrap();
    let text = exported["text"].as_str().unwrap();
    assert!(text.len() > 100_000, "the document envelope needs its own allowance");
    assert!(text.len() <= rustgis_ui_egui::SHORTCUT_FILE_MAX_BYTES);
    let mut other = harness(Arc::new(Mutex::new(HashMap::new())));
    other.state_mut().run("ui.shortcuts.import", json!({"text":text,"confirm":true})).unwrap();
    assert_eq!(other.state().ui.keymap, h.state().ui.keymap);
}

/// A script changing keys while a preview is open must invalidate the displayed conflict count.
#[test]
fn shortcut_commands_invalidate_an_open_import_preview() {
    use serde_json::json;
    let mut h = harness(Arc::new(Mutex::new(HashMap::new())));
    let text = json!({"rustgis_shortcuts":1,"bindings":[{"keys":"F8","command":"ui.theme"}]}).to_string();
    for (id, params) in [
        ("ui.shortcuts.set", json!({"keys":"F8","command":"select.clear"})),
        ("ui.shortcuts.reset", json!({"keys":"F8"})),
        ("ui.shortcuts.reset", Value::Null),
    ] {
        h.state_mut().run("ui.shortcuts.import", json!({"text":text})).unwrap();
        h.run_steps(3);
        assert!(h.query_by_label("Import Keyboard Shortcuts").is_some());
        h.state_mut().run(id, params).unwrap();
        h.run_steps(3);
        assert!(h.query_by_label("Import Keyboard Shortcuts").is_none(), "{id} left a stale preview");
    }
}

/// Browser inbox callers discard returned errors, so picker failures must also notify the user.
#[test]
fn browser_shortcut_errors_are_visible_and_future_versions_are_identified() {
    use serde_json::json;
    let mut h = harness(Arc::new(Mutex::new(HashMap::new())));
    for bytes in [b"invalid".as_slice(), &[255], &vec![b' '; rustgis_ui_egui::SHORTCUT_FILE_MAX_BYTES + 1]] {
        let before = h.state().session.notices.len();
        assert!(h.state_mut().open_picked(&format!("{}bad.json", rustgis_ui_egui::SHORTCUT_PICK), bytes).is_err());
        assert!(h.state().session.notices.len() > before);
    }
    let text = json!({"rustgis_shortcuts":2,"bindings":[{"new_field":true}]}).to_string();
    let error = h.state_mut().run("ui.shortcuts.import", json!({"text":text})).unwrap_err();
    assert_eq!(error, "Unsupported shortcut file version.");
}
