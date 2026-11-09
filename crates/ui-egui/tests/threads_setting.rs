//! Settings › Performance › Threads (#1562): `ui.threads` sets the shared thread count, keeps it as
//! a preference, and a start reads it back. A file of its own, so its process holds the setting.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rustgis_engine::data::threads;
use rustgis_engine::{Project, Session};
use rustgis_ui_egui::{RustGisApp, Services};
use serde_json::{Value, json};

type Prefs = Arc<Mutex<HashMap<String, String>>>;

fn start(prefs: &Prefs) -> RustGisApp {
    let (load, save) = (prefs.clone(), prefs.clone());
    let services = Services {
        load_pref: Some(Box::new(move |k| load.lock().unwrap().get(k).cloned())),
        save_pref: Some(Box::new(move |k, v| {
            save.lock().unwrap().insert(k.to_string(), v.to_string());
        })),
        ..Services::default()
    };
    RustGisApp::new(Session::new(Project::default())).with_services(services)
}

#[test]
fn the_thread_setting_is_kept_and_read_at_start() {
    let cores = threads::cores();
    let prefs: Prefs = Arc::new(Mutex::new(HashMap::from([("max_threads".to_string(), "3".to_string())])));
    let mut app = start(&prefs);
    assert_eq!(threads::setting(), Some(3), "read at start");

    let r = app.run("ui.threads", json!({"threads": 2})).unwrap();
    let default = cores.min(threads::DEFAULT_MAX);
    assert_eq!(r, json!({"threads": 2, "max": cores.min(2), "default": default, "cores": cores}));
    assert_eq!(prefs.lock().unwrap()["max_threads"], "2");
    // `gp.threads` says the same.
    assert_eq!(app.run("gp.threads", json!({})).unwrap()["threads"], json!(2));

    for bad in [json!(0), json!(2000), json!("8")] {
        assert!(app.run("ui.threads", json!({"threads": bad})).is_err(), "{bad}");
        assert_eq!(threads::setting(), Some(2), "a refused value changes nothing");
        assert_eq!(prefs.lock().unwrap()["max_threads"], "2");
    }

    let r = app.run("ui.threads", json!({"threads": null})).unwrap();
    assert_eq!(r["threads"], Value::Null);
    assert_eq!(prefs.lock().unwrap()["max_threads"], "auto");

    // A start with the default kept leaves it, quietly; a value edited by hand leaves it too,
    // and says so.
    for (kept, said) in [("auto", false), ("lots", true), ("0", true), ("2000", true)] {
        // Whatever an earlier start in this process set: "auto" leaves it, a bad value resets it.
        threads::set_max(Some(5)).unwrap();
        prefs.lock().unwrap().insert("max_threads".into(), kept.into());
        let app = start(&prefs);
        assert_eq!(threads::setting(), if said { None } else { Some(5) }, "{kept}");
        let told = app.session.notices.iter().any(|n| n.text.contains("thread setting"));
        assert_eq!(told, said, "{kept}");
    }
}
