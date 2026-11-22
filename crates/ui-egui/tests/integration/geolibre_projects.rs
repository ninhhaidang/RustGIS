//! Import entry points through the actual app frame and file-picker completion boundary.
use egui_kittest::kittest::Queryable;
use rustgis_ui_egui::{RustGisApp, Services};
use serde_json::json;
use std::sync::{Arc, Mutex};

/// A hand-authored project with one inline layer and no network dependencies.
fn project() -> Vec<u8> {
    json!({"version":"0.2.0","name":"GeoLibre map","mapView":{"center":[0,0],"zoom":4},"basemapStyleUrl":"","layers":[{"id":"points","name":"Imported points","type":"geojson","geojson":{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"name":"Test"}}]}}]}).to_string().into_bytes()
}

#[test]
fn geolibre_card_picker_completion_imports_a_map_without_replacing_work() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let picked = calls.clone();
    let mut app = RustGisApp::with_sample().with_services(Services {
        open_async: Some(Box::new(move |purpose| picked.lock().unwrap().push(purpose.to_string()))),
        ..Default::default()
    });
    let original = app.session.map().name.clone();
    let maps = app.session.project.maps.len();
    app.ui.backstage = true;
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1800.0, 1000.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "GeoLibre Project…").click();
    h.step();
    h.step();
    assert_eq!(*calls.lock().unwrap(), vec!["project"]);
    h.state_mut().open_picked("map.geolibre", &project()).unwrap();
    h.step();
    assert_eq!(h.state().session.project.maps.len(), maps + 1);
    assert_eq!(h.state().session.project.maps[0].name, original);
    assert!(h.state().session.map().layer_by_name("Imported points").is_some());
    assert!(!h.state().ui.backstage);
}

#[test]
fn geolibre_drop_and_legacy_name_share_the_import_path() {
    for name in ["MAP.GEOLIBRE", "map.geolibre.json", "plan#2.geolibre", "plan?2.geolibre.json"] {
        let mut app = RustGisApp::with_sample();
        let maps = app.session.project.maps.len();
        app.open_bytes(name, &project()).unwrap();
        assert_eq!(app.session.project.maps.len(), maps + 1);
        assert_eq!(app.session.map().name, "GeoLibre map");
        assert_eq!(rustgis_engine::project::open_command(name), "project.importGeolibre");
    }
}
