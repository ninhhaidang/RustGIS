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

/// Arrangements saved by name are kept with the preferences too, read back before the favourite
/// that may name one; those that can't be used are left out and counted.
#[test]
fn saved_arrangements_are_kept_and_restored() {
    const SAVED: &str = "layout_saved_arrangements";
    let saved = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let writes = saved.clone();
    let mut app = RustGisApp::new(Session::new(Project::default())).with_services(Services {
        save_pref: Some(Box::new(move |k, v| writes.lock().unwrap().push((k.to_string(), v.to_string())))),
        ..Services::default()
    });
    let atlas = json!({"locator": "bottom-left", "northArrow": "top-left", "legend": "top-right", "scaleBar": "bottom-right"});
    app.run("layout.arrangement", json!({"save": "Atlas", "arrangement": atlas, "favourite": "Atlas"})).unwrap();
    // With nothing passed over, a save drops nothing and says nothing of it.
    assert!(!app.session.notices.iter().any(|n| n.text.contains("are dropped")));
    let last = |key: &str| saved.lock().unwrap().iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.clone()).unwrap();
    let (arrangements, favourite) = (last(SAVED), last(KEY));
    assert_eq!(serde_json::from_str::<serde_json::Value>(&arrangements).unwrap(), json!({"Atlas": atlas}));
    let restored = RustGisApp::new(Session::new(Project::default())).with_services(Services {
        load_pref: Some(Box::new(move |k| match k {
            SAVED => Some(arrangements.clone()),
            KEY => Some(favourite.clone()),
            _ => None,
        })),
        ..Services::default()
    });
    assert_eq!(restored.session.layout_arrangements.get("Atlas"), Some(&atlas));
    assert_eq!(restored.session.layout_favourite, json!("Atlas"));
    assert!(!restored.session.notices.iter().any(|n| n.text.contains("arrangement")));
    // What can't be used is left out and counted, once, and saved again as it was.
    let half = json!({"locator": "top-left"});
    let text = json!({"Atlas": atlas, "atlas": half, "best": atlas, "Half": half}).to_string();
    let writes = saved.clone();
    let mut restored = RustGisApp::new(Session::new(Project::default())).with_services(Services {
        load_pref: Some(Box::new(move |k| (k == SAVED).then(|| text.clone()))),
        save_pref: Some(Box::new(move |k, v| writes.lock().unwrap().push((k.to_string(), v.to_string())))),
        ..Services::default()
    });
    assert_eq!(restored.session.layout_arrangements.len(), 1);
    // One notice for them all, with their count.
    let said: Vec<_> = restored.session.notices.iter().filter(|n| n.text.contains("arrangements of publication layouts")).collect();
    assert_eq!(said.len(), 1);
    assert_eq!(
        said[0].text,
        "Saved arrangements of publication layouts left out: 3 (not usable by this version, a second name for one kept, or past the limit of 20). They are kept with the preferences."
    );
    restored.run("layout.arrangement", json!({"save": "Half", "arrangement": "mirrored"})).unwrap();
    let written: serde_json::Value = serde_json::from_str(&last(SAVED)).unwrap();
    // Kept as they were, a name in other capitals too; the one saved here under that very name
    // takes its place.
    assert_eq!((written["best"].clone(), written["atlas"].clone()), (atlas.clone(), half.clone()), "{written}");
    assert_eq!(written["Half"]["locator"], "top-right", "{written}");
    // Text that isn't a list of arrangements is said, and never saved over.
    let writes = saved.clone();
    let mut restored = RustGisApp::new(Session::new(Project::default())).with_services(Services {
        load_pref: Some(Box::new(|k| (k == SAVED).then(|| "a newer format".to_string()))),
        save_pref: Some(Box::new(move |k, v| writes.lock().unwrap().push((k.to_string(), v.to_string())))),
        ..Services::default()
    });
    assert!(restored.session.notices.iter().any(|n| n.text.contains("can't be read: they are left as they are")));
    let before = saved.lock().unwrap().len();
    restored.run("layout.arrangement", json!({"save": "Atlas", "arrangement": "classic"})).unwrap();
    assert!(saved.lock().unwrap()[before..].iter().all(|(k, _)| k != SAVED));
    assert!(restored.session.layout_arrangements.contains_key("Atlas"), "for the session");
    // An arrangement removed takes with it one passed over beside it under the same name but for
    // case, so it doesn't come back at the next start.
    let mirrored = json!({"locator": "top-right", "northArrow": "top-left", "legend": "bottom-right", "scaleBar": "bottom-left"});
    let text = json!({"Atlas": atlas, "atlas": mirrored, "ATLAS": {"locator": "middle"}, "Keep": mirrored}).to_string();
    let writes = saved.clone();
    let mut restored = RustGisApp::new(Session::new(Project::default())).with_services(Services {
        load_pref: Some(Box::new(move |k| (k == SAVED).then(|| text.clone()))),
        save_pref: Some(Box::new(move |k, v| writes.lock().unwrap().push((k.to_string(), v.to_string())))),
        ..Services::default()
    });
    restored.run("layout.arrangement", json!({"remove": "Atlas"})).unwrap();
    let written: serde_json::Value = serde_json::from_str(&last(SAVED)).unwrap();
    // One this version can't read stays as it was.
    assert_eq!(written, json!({"Keep": mirrored, "ATLAS": {"locator": "middle"}}));
    // A name saved here takes the place of a usable entry passed over under it but for case, so
    // the next start keeps the new one; one this version can't read stays.
    let text = json!({"Atlas": atlas, "atlas": mirrored, "zed": {"locator": "middle"}}).to_string();
    let writes = saved.clone();
    let mut restored = RustGisApp::new(Session::new(Project::default())).with_services(Services {
        load_pref: Some(Box::new(move |k| (k == SAVED).then(|| text.clone()))),
        save_pref: Some(Box::new(move |k, v| writes.lock().unwrap().push((k.to_string(), v.to_string())))),
        ..Services::default()
    });
    // Replaced under its very name, it takes the place of the usable entry passed over beside it.
    let replaced = json!({"Atlas": atlas, "atlas": mirrored}).to_string();
    let writes = saved.clone();
    let mut kept = RustGisApp::new(Session::new(Project::default())).with_services(Services {
        load_pref: Some(Box::new(move |k| (k == SAVED).then(|| replaced.clone()))),
        save_pref: Some(Box::new(move |k, v| writes.lock().unwrap().push((k.to_string(), v.to_string())))),
        ..Services::default()
    });
    kept.run("layout.arrangement", json!({"save": "Atlas", "arrangement": "legendTop"})).unwrap();
    let written: serde_json::Value = serde_json::from_str(&last(SAVED)).unwrap();
    assert_eq!(
        written,
        json!({"Atlas": {"locator": "bottom-left", "northArrow": "top-right", "legend": "top-left", "scaleBar": "bottom-right"}})
    );
    restored.run("layout.arrangement", json!({"remove": "Atlas", "save": "ATLAS", "arrangement": "legendTop"})).unwrap();
    // The entry passed over that goes with it is said.
    assert!(restored.session.notices.iter().any(|n| n.text.ends_with("are dropped: 1.")), "said");
    restored.run("layout.arrangement", json!({"save": "Zed", "arrangement": "classic"})).unwrap();
    let written: serde_json::Value = serde_json::from_str(&last(SAVED)).unwrap();
    let names: Vec<&String> = written.as_object().unwrap().keys().collect();
    assert_eq!(names.len(), 3, "{written}");
    assert_eq!(
        (written["ATLAS"]["locator"].clone(), written["zed"].clone()),
        (json!("bottom-left"), json!({"locator": "middle"})),
        "{written}"
    );
    assert_eq!(written["Zed"]["locator"], "top-left", "{written}");
}
