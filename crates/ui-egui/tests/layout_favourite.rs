//! The favourite arrangement of publication layouts (#1379) is kept with the app's preferences.
use rustgis_engine::{Project, Session};
use rustgis_ui_egui::{RustGisApp, Services};
use serde_json::json;
use std::sync::{Arc, Mutex};

const KEY: &str = "layout_favourite_arrangement";

#[test]
fn the_favourite_arrangement_is_saved_and_restored() {
    let saved = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let writes = saved.clone();
    let mut app = RustGisApp::new(Session::new(Project::default())).with_services(Services {
        save_pref: Some(Box::new(move |k, v| writes.lock().unwrap().push((k.to_string(), v.to_string())))),
        ..Services::default()
    });
    // Nothing set, nothing written: the default isn't saved as if chosen.
    app.run("layout.arrangement", json!({})).unwrap();
    assert!(saved.lock().unwrap().iter().all(|(k, _)| k != KEY));
    // Set, it is written under its key; corner by corner too.
    let custom = json!({"locator": "bottom-left", "northArrow": "top-left", "legend": "top-right", "scaleBar": "bottom-right"});
    for favourite in [json!("mirrored"), custom] {
        app.run("layout.arrangement", json!({"favourite": favourite})).unwrap();
        let value = saved.lock().unwrap().iter().rev().find(|(k, _)| k == KEY).map(|(_, v)| v.clone()).unwrap();
        // A new app reading it back has it as its favourite.
        let restored = RustGisApp::new(Session::new(Project::default()))
            .with_services(Services { load_pref: Some(Box::new(move |k| (k == KEY).then(|| value.clone()))), ..Services::default() });
        assert_eq!(restored.session.layout_favourite, favourite);
        // A valid one says nothing.
        assert!(!restored.session.notices.iter().any(|n| n.text.contains("favourite arrangement")));
    }
    // A saved null is the default, named, with nothing to say.
    let restored = RustGisApp::new(Session::new(Project::default()))
        .with_services(Services { load_pref: Some(Box::new(|k| (k == KEY).then(|| "null".to_string()))), ..Services::default() });
    assert_eq!(restored.session.layout_favourite, json!("classic"));
    assert!(!restored.session.notices.iter().any(|n| n.text.contains("favourite arrangement")));
    // One no longer valid is passed over, said, and classic used, whatever the session held.
    for bad in ["\"tidy\"", "not json", "{\"locator\": \"top-left\"}"] {
        let mut session = Session::new(Project::default());
        session.layout_favourite = json!("mirrored");
        let restored = RustGisApp::new(session)
            .with_services(Services { load_pref: Some(Box::new(move |k| (k == KEY).then(|| bad.to_string()))), ..Services::default() });
        assert_eq!(restored.session.layout_favourite, json!("classic"), "{bad}");
        // ... and says so.
        let said =
            restored.session.notices.iter().filter(|n| n.text.contains("favourite arrangement") && n.text.contains("\"classic\" is used"));
        assert_eq!(said.count(), 1, "{bad}");
    }
}
