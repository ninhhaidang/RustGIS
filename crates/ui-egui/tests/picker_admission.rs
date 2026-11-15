//! Admission snapshots are taken after options are prepared, before the platform is called.
use rustgis_ui_egui::{RustGisApp, Services};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[test]
fn two_requests_in_one_frame_keep_the_first_admitted_options() {
    let candidate = Arc::new(Mutex::new(None::<Value>));
    let admitted = Arc::new(Mutex::new(None::<Value>));
    let snapshots = candidate.clone();
    let (pending, accepted) = (candidate.clone(), admitted.clone());
    let mut app = RustGisApp::with_sample().with_services(Services {
        open_options: Some(Box::new(move |ui| *snapshots.lock().unwrap() = ui.pending_csv.clone())),
        open_async: Some(Box::new(move |_| {
            let mut first = accepted.lock().unwrap();
            if first.is_none() {
                *first = pending.lock().unwrap().clone();
            }
        })),
        ..Default::default()
    });
    let options = json!({"x": "longitude", "y": "latitude", "delimiter": ";"});
    app.run("ui.addDelimited", options.clone()).unwrap();
    app.run("ui.addData", Value::Null).unwrap();
    assert!(candidate.lock().unwrap().is_none());
    assert_eq!(*admitted.lock().unwrap(), Some(options));
}
