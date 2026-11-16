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

/// A lone binary DBF from the platform inbox must reach the table reader without decoding
/// the file as UTF-8 text first, including when no CPG is selected alongside it.
#[test]
fn standalone_dbf_inbox_opens_as_a_table() {
    use rustgis_engine::{
        Project, Session,
        data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value as Cell},
    };
    let name = "x".repeat(130);
    let mut ds = Dataset::new("places", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
    ds.push_next(Some(Geometry::Point(Coord::new(1.0, 2.0))), vec![Cell::Text(name.clone())]);
    let (parts, _) = rustgis_engine::data::io::shapefile_parts(&ds).unwrap();
    let dbf = parts.into_iter().find(|(ext, _)| *ext == "dbf").unwrap().1;
    assert!(std::str::from_utf8(&dbf).is_err(), "the DBF header's field width is binary");
    let inbox = Arc::new(Mutex::new(vec![("places.DBF".into(), dbf)]));
    let app = RustGisApp::new(Session::new(Project::default())).with_services(Services { inbox: Some(inbox), ..Default::default() });
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1200.0, 900.0)).build_ui_state(
        |ui, app: &mut RustGisApp| {
            app.logic(ui.ctx());
            app.ui(ui);
        },
        app,
    );
    h.step();
    assert!(h.state().session.map().layers.is_empty());
    assert_eq!(h.state().session.project.tables.len(), 1);
    assert_eq!(h.state().session.project.tables[0].dataset.value(0, 0), &Cell::Text(name));
}
