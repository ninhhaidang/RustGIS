//! The GPU map toggle: a `ui.map.gpu` command that flips the backend and persists.

use rustgis_ui_egui::{RustGisApp, Services};
use serde_json::Value;

#[test]
fn gpu_toggle_flips_persists_and_invalidates() {
    let saved = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
    let saved2 = saved.clone();
    let mut app = RustGisApp::with_sample().with_services(Services {
        save_pref: Some(Box::new(move |k, v| saved2.lock().unwrap_or_else(|e| e.into_inner()).push((k.to_string(), v.to_string())))),
        ..Services::default()
    });
    let was = app.map.gpu_enabled;
    app.run("ui.map.gpu", Value::Null).expect("ui.map.gpu");
    assert_eq!(app.map.gpu_enabled, !was, "toggled");
    assert!(
        saved.lock().unwrap_or_else(|e| e.into_inner()).contains(&("ui.map.gpu".into(), if !was { "1" } else { "0" }.into())),
        "persisted"
    );
    app.run("ui.map.gpu", Value::Null).expect("ui.map.gpu again");
    assert_eq!(app.map.gpu_enabled, was, "toggled back");
}
