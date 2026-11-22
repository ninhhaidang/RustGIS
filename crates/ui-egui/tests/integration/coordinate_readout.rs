//! The status bar's coordinate readout: a click steps decimal → DMS → UTM → MGRS, the choice is
//! remembered as a preference, and the readout shows the pointer in it.

use std::sync::{Arc, Mutex};

use egui_kittest::kittest::Queryable;
use rustgis_ui_egui::{CoordReadout, RustGisApp, Services};
use serde_json::{Value, json};

fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
    egui_kittest::Harness::builder().with_size(egui::vec2(1600.0, 900.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    )
}

#[test]
fn the_format_cycles_persists_and_is_restored() {
    let saved = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let writes = saved.clone();
    let mut app = RustGisApp::with_sample().with_services(Services {
        save_pref: Some(Box::new(move |k, v| writes.lock().unwrap_or_else(|e| e.into_inner()).push((k.to_string(), v.to_string())))),
        ..Services::default()
    });
    assert_eq!(app.map.coord_format, CoordReadout::Decimal);
    let mut seen = Vec::new();
    for _ in 0..4 {
        app.run("ui.coordinateFormat", Value::Null).unwrap();
        seen.push(app.map.coord_format.code());
    }
    assert_eq!(seen, ["dms", "utm", "mgrs", "decimal"]);
    let writes: Vec<String> = saved.lock().unwrap().iter().filter(|(k, _)| k == "coordinate_format").map(|(_, v)| v.clone()).collect();
    assert_eq!(writes, ["dms", "utm", "mgrs", "decimal"]);
    app.run("ui.coordinateFormat", json!({"format": "MGRS"})).unwrap();
    assert_eq!(app.map.coord_format, CoordReadout::Mgrs);
    for bad in [json!({"format": "ups"}), json!({"format": 3}), json!({"format": ""})] {
        assert!(app.run("ui.coordinateFormat", bad.clone()).is_err(), "{bad}");
        assert_eq!(app.map.coord_format, CoordReadout::Mgrs, "{bad}");
    }
    // A saved choice comes back, with nothing to say.
    for (pref, want) in [("utm", CoordReadout::Utm), ("mgrs", CoordReadout::Mgrs)] {
        let restored = RustGisApp::with_sample().with_services(Services {
            load_pref: Some(Box::new(move |k| (k == "coordinate_format").then(|| pref.to_string()))),
            ..Services::default()
        });
        assert_eq!(restored.map.coord_format, want, "{pref}");
        assert!(!said_unknown(&restored), "{pref}");
    }
}

fn said_unknown(app: &RustGisApp) -> bool {
    app.session.notices.iter().any(|n| n.text.contains("saved coordinate format isn't known"))
}

#[test]
fn an_unknown_saved_format_is_said_and_kept_until_the_user_picks_one() {
    let saved = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let writes = saved.clone();
    let app = RustGisApp::with_sample().with_services(Services {
        load_pref: Some(Box::new(|k| (k == "coordinate_format").then(|| "lambert".to_string()))),
        save_pref: Some(Box::new(move |k, v| writes.lock().unwrap_or_else(|e| e.into_inner()).push((k.to_string(), v.to_string())))),
        ..Services::default()
    });
    let format_writes =
        || saved.lock().unwrap().iter().filter(|(k, _)| k == "coordinate_format").map(|(_, v)| v.clone()).collect::<Vec<_>>();
    // Read as decimal degrees, and said once.
    assert_eq!(app.map.coord_format, CoordReadout::Decimal);
    assert_eq!(app.session.notices.iter().filter(|n| n.text.contains("saved coordinate format isn't known")).count(), 1);
    // Frames drawn and another setting saved leave the stored format alone.
    let mut h = harness(app);
    hover_map(&mut h);
    h.state_mut().run("ui.measurementUnits", json!({"units": "imperial"})).unwrap();
    hover_map(&mut h);
    assert!(saved.lock().unwrap().iter().any(|(k, _)| k == "measurement_units"));
    assert!(format_writes().is_empty(), "{:?}", format_writes());
    // A click on the readout is the user's choice: it is saved.
    let decimal = rustgis_engine::geom::format_lonlat(h.state().map.pointer_lonlat.expect("the pointer is over the map"));
    h.get_by_label(&decimal).click();
    hover_map(&mut h);
    assert_eq!(h.state().map.coord_format, CoordReadout::Dms);
    assert_eq!(format_writes(), ["dms"]);
}

/// Points the mouse at the middle of the window, over the map, for a few frames.
fn hover_map(h: &mut egui_kittest::Harness<'_, RustGisApp>) {
    for _ in 0..3 {
        h.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(800.0, 450.0)));
        h.step();
    }
}

#[test]
fn the_status_bar_reads_the_pointer_in_the_chosen_format_and_a_click_switches() {
    let mut h = harness(RustGisApp::with_sample());
    // A view about 4 km across around Baghdad (38S MB 44140 84706, at 38north 444141 3684706).
    h.state_mut().run("map.goToXY", json!({"lon": 44.4, "lat": 33.3, "scale": 10000})).unwrap();
    hover_map(&mut h);
    let at = h.state().map.pointer_lonlat.expect("the pointer is over the map");
    assert!((at.x - 44.4).abs() < 0.05 && (at.y - 33.3).abs() < 0.05, "{at:?}");
    let decimal = rustgis_engine::geom::format_lonlat(at);
    h.get_by_label(&decimal).click();
    hover_map(&mut h);
    assert_eq!(h.state().map.coord_format, CoordReadout::Dms);
    let at = h.state().map.pointer_lonlat.unwrap();
    h.get_by_label(&rustgis_engine::geom::format_dms(at));
    h.get_by_label(&rustgis_engine::geom::format_dms(at)).click();
    hover_map(&mut h);
    assert_eq!(h.state().map.coord_format, CoordReadout::Utm);
    let utm = rustgis_engine::geom::format_utm(h.state().map.pointer_lonlat.unwrap()).unwrap();
    assert!(utm.starts_with("38north 44"), "{utm}");
    h.get_by_label(&utm).click();
    hover_map(&mut h);
    assert_eq!(h.state().map.coord_format, CoordReadout::Mgrs);
    let mgrs = rustgis_engine::geom::format_mgrs(h.state().map.pointer_lonlat.unwrap()).unwrap();
    assert!(mgrs.starts_with("38S MB 4"), "{mgrs}");
    h.get_by_label(&mgrs).click();
    hover_map(&mut h);
    assert_eq!(h.state().map.coord_format, CoordReadout::Decimal);
    // Where UTM has no zone the readout gives the polar grid, UPS (MGRS without a zone number).
    h.state_mut().run("ui.coordinateFormat", json!({"format": "mgrs"})).unwrap();
    h.state_mut().run("map.goToXY", json!({"lon": 10.0, "lat": 84.5, "scale": 10000})).unwrap();
    hover_map(&mut h);
    let at = h.state().map.pointer_lonlat.unwrap();
    assert!(at.y > 84.0, "{at:?}");
    let polar = rustgis_engine::geom::format_mgrs(at).unwrap();
    assert!(polar.starts_with("Z "), "{polar}");
    h.get_by_label(&polar);
    // On another body's map UTM and MGRS, the Earth's grids, read nothing either.
    h.state_mut().run("map.body", json!({"name": "mars", "basemap": false})).unwrap();
    h.state_mut().run("map.goToXY", json!({"lon": 44.4, "lat": 33.3, "scale": 10000})).unwrap();
    hover_map(&mut h);
    assert!(h.state().map.pointer_lonlat.is_some_and(|c| (c.y - 33.3).abs() < 0.05));
    h.get_by_label("No MGRS here");
    h.state_mut().run("ui.coordinateFormat", json!({"format": "decimal"})).unwrap();
    hover_map(&mut h);
    h.get_by_label(&rustgis_engine::geom::format_lonlat(h.state().map.pointer_lonlat.unwrap()));
}

/// Moves the mouse onto the status bar, off the map, for a few frames.
fn hover_status_bar(h: &mut egui_kittest::Harness<'_, RustGisApp>) {
    for _ in 0..3 {
        h.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(800.0, 892.0)));
        h.step();
    }
}

fn readout_at_center(h: &mut egui_kittest::Harness<'_, RustGisApp>) -> String {
    h.state_mut().run("map.goToXY", json!({"lon": 44.4, "lat": 33.3, "scale": 10000})).unwrap();
    hover_map(h);
    rustgis_engine::geom::format_lonlat(h.state().map.pointer_lonlat.expect("the pointer is over the map"))
}

#[test]
fn the_readout_is_a_dash_until_the_pointer_has_been_over_the_map() {
    let mut h = harness(RustGisApp::with_sample());
    h.step();
    h.get_by_label("—");
    hover_status_bar(&mut h);
    h.get_by_label("—");
    assert!(h.state().map.pointer_lonlat.is_none());
}

#[test]
fn the_position_stays_when_the_pointer_leaves_the_map_and_a_click_switches_format() {
    let mut h = harness(RustGisApp::with_sample());
    let decimal = readout_at_center(&mut h);
    hover_status_bar(&mut h);
    assert!(h.state().map.pointer_lonlat.is_none());
    h.get_by_label(&decimal);
    // The pointer in the window's corner, or gone altogether, keeps it too.
    h.input_mut().events.push(egui::Event::PointerGone);
    h.step();
    h.get_by_label(&decimal);
    // A click on the readout shows the same position in the next format.
    h.get_by_label(&decimal).click();
    h.step();
    h.step();
    assert_eq!(h.state().map.coord_format, CoordReadout::Dms);
    let at = h.state().map.pointer_lonlat;
    assert!(at.is_none());
    h.get_by_label_contains("44°");
    // The pointer back over the map follows it again.
    hover_map(&mut h);
    assert!(h.state().map.pointer_lonlat.is_some());
}

#[test]
fn the_position_is_dropped_on_another_map_or_project() {
    let mut h = harness(RustGisApp::with_sample());
    let decimal = readout_at_center(&mut h);
    hover_status_bar(&mut h);
    h.get_by_label(&decimal);
    // Another map becoming the active one.
    h.state_mut().run("map.new", json!({"name": "Second Map"})).unwrap();
    h.step();
    h.step();
    assert!(h.query_by_label(&decimal).is_none());
    h.get_by_label("—");
    // Back on the first map it is not remembered from before.
    h.state_mut().run("map.activate", json!({"name": "World Atlas"})).ok();
    h.step();
    h.step();
    h.get_by_label("—");
    // A new position, then another project replacing this one.
    let decimal = readout_at_center(&mut h);
    hover_status_bar(&mut h);
    h.get_by_label(&decimal);
    h.state_mut().run("project.new", json!({})).unwrap();
    h.step();
    h.step();
    assert!(h.query_by_label(&decimal).is_none());
    h.get_by_label("—");
}
