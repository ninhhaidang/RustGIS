//! Headless interaction tests for the units users choose and the results they see.
use egui_kittest::kittest::{NodeT, Queryable};
use rustgis_engine::{Project, Session, data::Geometry, geom::lonlat_to_merc};
use rustgis_ui_egui::{RustGisApp, Services};
use serde_json::json;

fn harness() -> egui_kittest::Harness<'static, RustGisApp> {
    harness_with_services(Services::default())
}

fn harness_with_services(services: Services) -> egui_kittest::Harness<'static, RustGisApp> {
    let mut app = RustGisApp::new(Session::new(Project::default())).with_services(services);
    app.run("layer.template", json!({"template": "lines", "name": "Paths"})).unwrap();
    app.run("edit.addFeature", json!({"geometry": {"type": "LineString", "coordinates": [[0, 0], [0, 0.001]]}})).unwrap();
    app.run("select.rows", json!({"layer": "Paths", "rows": [0]})).unwrap();
    egui_kittest::Harness::builder().with_size(egui::vec2(1800.0, 1000.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    )
}

fn first_x_metres(h: &egui_kittest::Harness<'_, RustGisApp>) -> f64 {
    let Geometry::Line(parts) = h.state().session.map().layer_by_name("Paths").unwrap().dataset.features[0].geometry.as_ref().unwrap()
    else {
        panic!("line")
    };
    lonlat_to_merc(parts[0][0]).x
}

fn choose_editing_units(h: &mut egui_kittest::Harness<'_, RustGisApp>, unit: &str) {
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Distance units").click();
    // The list opens on the frame after the click.
    h.step();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, unit).click();
    h.step();
}

#[test]
fn editing_pane_moves_in_feet() {
    let mut h = harness();
    h.state_mut().run("ui.pane.editing", json!({})).unwrap();
    h.state_mut().forms.dx = "500".into();
    h.state_mut().forms.dy = "0".into();
    for _ in 0..3 {
        h.step();
    }
    choose_editing_units(&mut h, "Feet");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Move").click();
    h.step();
    let Geometry::Line(parts) = h.state().session.map().layer_by_name("Paths").unwrap().dataset.features[0].geometry.as_ref().unwrap()
    else {
        panic!("line")
    };
    assert!((lonlat_to_merc(parts[0][0]).x - 152.4).abs() < 1e-6);
}

#[test]
fn editing_pane_sets_constant_elevation_in_feet() {
    let mut h = harness();
    h.state_mut().run("ui.pane.editing", json!({})).unwrap();
    h.state_mut().forms.z = "1000".into();
    for _ in 0..3 {
        h.step();
    }
    choose_editing_units(&mut h, "Feet");
    h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Constant").click();
    h.step();
    assert_eq!(h.state().session.map().editing.settings.elevation, rustgis_engine::map::design::Elevation::Constant(304.8));
}

#[test]
fn editing_units_are_remembered_and_applied_explicitly() {
    use std::sync::{Arc, Mutex};
    let saved = Arc::new(Mutex::new(Vec::new()));
    let writes = saved.clone();
    let mut h = harness_with_services(Services {
        save_pref: Some(Box::new(move |key, value| writes.lock().unwrap().push((key.to_owned(), value.to_owned())))),
        ..Services::default()
    });
    h.state_mut().run("edit.elevation", json!({"mode": "constant", "z": 1000})).unwrap();
    h.state_mut().forms.z = "1000".into();
    h.state_mut().run("ui.pane.editing", json!({})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    choose_editing_units(&mut h, "Feet");
    assert!(saved.lock().unwrap().contains(&("editing_distance_units".into(), "Feet".into())), "editing units were not saved");
    assert_eq!(h.state().session.map().editing.settings.elevation, rustgis_engine::map::design::Elevation::Constant(1000.0));
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Apply elevation").click();
    h.step();
    assert_eq!(h.state().session.map().editing.settings.elevation, rustgis_engine::map::design::Elevation::Constant(304.8));
    drop(h);

    let reads = saved.clone();
    let mut restored = harness_with_services(Services {
        load_pref: Some(Box::new(move |key| reads.lock().unwrap().iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.clone()))),
        ..Services::default()
    });
    restored.state_mut().forms.dx = "500".into();
    restored.state_mut().forms.dy = "0".into();
    restored.state_mut().run("ui.pane.editing", json!({})).unwrap();
    for _ in 0..3 {
        restored.step();
    }
    restored.get_by_role_and_label(egui::accesskit::Role::Button, "Move").click();
    restored.step();
    assert!((first_x_metres(&restored) - 152.4).abs() < 1e-6);

    // Unset and obsolete preferences retain the existing metre default.
    for value in [None, Some("unsupported")] {
        let mut h = harness_with_services(Services {
            load_pref: Some(Box::new(move |key| if key == "editing_distance_units" { value.map(str::to_owned) } else { None })),
            ..Services::default()
        });
        h.state_mut().forms.dx = "500".into();
        h.state_mut().forms.dy = "0".into();
        h.state_mut().run("ui.pane.editing", json!({})).unwrap();
        for _ in 0..3 {
            h.step();
        }
        h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Distance units").click();
        h.step();
        // What it opened shows on the next frame.
        h.step();
        let metres = h.get_by_role_and_label(egui::accesskit::Role::Button, "Meters");
        assert_eq!(metres.accesskit_node().toggled(), Some(egui::accesskit::Toggled::True), "default metres option is not selected");
        metres.click();
        h.step();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Move").click();
        h.step();
        assert!((first_x_metres(&h) - 500.0).abs() < 1e-6);
    }
}

fn click_map(h: &mut egui_kittest::Harness<'_, RustGisApp>, at: egui::Pos2) {
    h.event(egui::Event::PointerMoved(at));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
    }
    for _ in 0..3 {
        h.step();
    }
}

fn painted_text(h: &egui_kittest::Harness<'_, RustGisApp>) -> Vec<String> {
    fn collect(shape: &egui::Shape, text: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => text.push(t.galley.text().to_string()),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    collect(shape, text);
                }
            }
            _ => {}
        }
    }
    let mut text = Vec::new();
    for shape in &h.output().shapes {
        collect(&shape.shape, &mut text);
    }
    text
}

fn profile_harness(length: f64) -> egui_kittest::Harness<'static, RustGisApp> {
    let mut h = harness();
    let grid = "ncols 2\nnrows 2\nxllcorner 0\nyllcorner 0\ncellsize 1\n30 30\n30 30\n";
    let raster = rustgis_engine::data::raster::ascii::read(grid, "Terrain", None).unwrap();
    h.state_mut()
        .session
        .map_mut()
        .add_raster("Terrain", rustgis_engine::map::RasterLayer::from_bytes(raster, "terrain.asc", grid.as_bytes().to_vec()));
    h.state_mut().ws.profile = Some(json!({
        "length": length, "min": 30, "max": 40, "gain": 10, "loss": 0,
        "samples": [{"distance": 0, "elevation": 30}, {"distance": length, "elevation": 40}]
    }));
    h.state_mut().run("ui.pane.profile", json!({})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    h
}

#[test]
fn measurement_preference_updates_an_existing_profile_length() {
    let mut h = profile_harness(1000.0);
    assert!(painted_text(&h).iter().any(|s| s.starts_with("Length 1.0 km")));
    h.state_mut().run("ui.measurementUnits", json!({"units": "imperial"})).unwrap();
    h.step();
    let text = painted_text(&h);
    assert!(text.iter().any(|s| s.starts_with("Length 3281 ft")), "{text:?}");
    assert!(text.iter().any(|s| s == "3281 ft"), "profile chart still uses metric: {text:?}");
    h.state_mut().run("ui.measurementUnits", json!({"units": "metric"})).unwrap();
    h.step();
    assert!(painted_text(&h).iter().any(|s| s.starts_with("Length 1.0 km")));
}

#[test]
fn half_foot_profile_elevations_and_gain_remain_nonzero() {
    let mut h = profile_harness(1000.0);
    // A rise from half a foot below sea level to sea level is exactly half a foot.
    h.state_mut().ws.profile = Some(json!({
        "length": 1000, "min": -0.1524, "max": 0, "gain": 0.1524, "loss": 0,
        "samples": [{"distance": 0, "elevation": -0.1524}, {"distance": 1000, "elevation": 0}]
    }));
    h.state_mut().run("ui.measurementUnits", json!({"units": "imperial"})).unwrap();
    h.step();
    let text = painted_text(&h);
    assert!(text.iter().any(|s| s.contains("gain 0.50 ft") && s.contains("loss 0 ft")), "{text:?}");
    assert!(text.iter().any(|s| s == "-0.50 ft"), "negative elevation lost its precision: {text:?}");
}

#[test]
fn short_profile_reports_its_measured_length() {
    let mut h = profile_harness(0.1524);
    h.state_mut().run("ui.measurementUnits", json!({"units": "imperial"})).unwrap();
    h.step();
    let text = painted_text(&h);
    assert!(text.iter().any(|s| s.starts_with("Length 0.50 ft")), "{text:?}");
    assert!(text.iter().any(|s| s == "0.50 ft"), "chart distance differs from the measured length: {text:?}");
}

#[test]
fn distance_controls_follow_display_units_and_keep_metre_command_values() {
    let mut h = harness();
    h.set_size(egui::vec2(3200.0, 1800.0));
    h.state_mut().run("labels.distanceRange", json!({"closest": 1000, "farthest": 100000})).unwrap();
    h.state_mut().run("ui.pane.labeling", json!({"layer": "Paths"})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    // The distances are in the pane's Range tab (#858).
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Range").click();
    h.step();
    h.step();
    assert!(painted_text(&h).iter().any(|s| s == "1.0 km"));
    h.state_mut().run("ui.measurementUnits", json!({"units": "imperial"})).unwrap();
    h.step();
    let text = painted_text(&h);
    assert!(text.iter().any(|s| s == "3281 ft") && text.iter().any(|s| s == "62.1 mi"), "{text:?}");

    h.state_mut().run("ui.pane.labeling", json!({})).unwrap();
    h.state_mut().run("ui.tab", json!({"tab": "Labeling"})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Closest").click();
    // The list opens on the frame after the click.
    h.step();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "6.2 mi").click();
    h.step();
    assert_eq!(h.state().session.map().layer_by_name("Paths").unwrap().labels.default.closest, Some(10000.0));

    h.state_mut().run("ui.tab", json!({"tab": "View"})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    h.get_by_role_and_label(egui::accesskit::Role::Button, "View Clipping").click();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "62.1 mi").click();
    h.step();
    assert_eq!(h.state().session.map().view.clip_distance, Some(100000.0));

    h.state_mut().run("ui.measurementUnits", json!({"units": "metric"})).unwrap();
    h.step();
    h.get_by_role_and_label(egui::accesskit::Role::Button, "View Clipping").click();
    h.step();
    // What it opened shows on the next frame.
    h.step();
    assert!(h.query_by_role_and_label(egui::accesskit::Role::RadioButton, "100 km").is_some());
    assert_eq!(h.state().session.map().view.clip_distance, Some(100000.0));
}

#[test]
fn measure_menu_switches_live_distance_and_area_readouts_to_imperial() {
    let mut h = harness();
    h.state_mut().run("map.goToXY", json!({"lon": 0, "lat": 0, "scale": 1000})).unwrap();
    h.state_mut().run("tool.set", json!({"tool": "measure"})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    let center = h.state().map.view_rect.unwrap().0.center();
    click_map(&mut h, center - egui::vec2(50.0, 0.0));
    click_map(&mut h, center + egui::vec2(50.0, 0.0));
    assert!(painted_text(&h).iter().any(|s| s.starts_with("Total ") && s.contains(" m")));
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Measure").click();
    for _ in 0..3 {
        h.step();
    }
    h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Imperial").click();
    h.step();
    // At 1:1000, 100 screen points span about 28 m, or 92 ft at the equator.
    let text = painted_text(&h);
    assert!(text.iter().any(|s| s.starts_with("Total 92 ft")), "{text:?}");
    assert!(text.iter().any(|s| s == "100 ft"), "scale bar: {text:?}");
    h.state_mut().run("tool.set", json!({"tool": "measureArea"})).unwrap();
    h.state_mut().map.cancel_sketch();
    h.step();
    click_map(&mut h, center - egui::vec2(50.0, 50.0));
    click_map(&mut h, center + egui::vec2(50.0, -50.0));
    click_map(&mut h, center + egui::vec2(50.0, 50.0));
    let text = painted_text(&h);
    assert!(text.iter().any(|s| s.starts_with("Area ") && s.contains("ft²") && s.contains("Perimeter") && s.ends_with("ft")), "{text:?}");
    for (scale, unit) in [(10_000, "acres"), (1_000_000, "mi²")] {
        h.state_mut().map.cancel_sketch();
        h.state_mut().run("map.goToXY", json!({"lon": 0, "lat": 0, "scale": scale})).unwrap();
        for _ in 0..3 {
            h.step();
        }
        click_map(&mut h, center - egui::vec2(50.0, 50.0));
        click_map(&mut h, center + egui::vec2(50.0, -50.0));
        click_map(&mut h, center + egui::vec2(50.0, 50.0));
        let text = painted_text(&h);
        assert!(text.iter().any(|s| s.starts_with("Area ") && s.contains(unit)), "{unit}: {text:?}");
    }
}

#[test]
fn imperial_measurements_preserve_small_positive_distances_and_areas() {
    let mut h = harness();
    h.state_mut().run("map.goToXY", json!({"lon": 0, "lat": 80, "scale": 1})).unwrap();
    h.state_mut().run("ui.measurementUnits", json!({"units": "imperial"})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    let center = h.state().map.view_rect.unwrap().0.center();
    // At this latitude and the closest zoom, five screen points span about 0.13 m.
    // A very thin polygon exercises tiny areas while keeping its corners far enough apart
    // to be separate clicks, rather than duplicate-click gestures.
    for (line_end, area_second, area_third) in [
        (egui::vec2(5.0, 0.0), egui::vec2(5.0, 0.0), egui::vec2(5.0, 5.0)),
        (egui::vec2(0.05, 0.0), egui::vec2(20.0, 0.0), egui::vec2(-20.0, 0.0001)),
    ] {
        for (tool, prefix, unit) in [("measure", "Total ", "ft"), ("measureArea", "Area ", "ft²")] {
            h.state_mut().run("tool.set", json!({"tool": tool})).unwrap();
            h.state_mut().map.cancel_sketch();
            h.step();
            click_map(&mut h, center);
            // Separate nearby clicks in time so they are not interpreted as a double-click.
            for _ in 0..32 {
                h.step();
            }
            click_map(&mut h, center + if tool == "measure" { line_end } else { area_second });
            if tool == "measureArea" {
                for _ in 0..32 {
                    h.step();
                }
                click_map(&mut h, center + area_third);
            }
            let text = painted_text(&h);
            let readout = text.iter().find(|s| s.starts_with(prefix)).unwrap_or_else(|| panic!("{text:?}"));
            let mut words = readout.split_whitespace();
            let value: f64 = words.nth(1).unwrap().parse().unwrap();
            assert_eq!(words.next(), Some(unit), "{readout}");
            assert!(value > 0.0, "positive measurement rounded to zero: {readout}");
        }
    }
}

#[test]
fn measurement_unit_choice_is_saved_restored_and_available_to_scripts() {
    use std::sync::{Arc, Mutex};
    let saved = Arc::new(Mutex::new(Vec::new()));
    let writes = saved.clone();
    let mut h = harness();
    h.state_mut().services.save_pref = Some(Box::new(move |k, v| writes.lock().unwrap().push((k.to_string(), v.to_string()))));
    for _ in 0..3 {
        h.step();
    }
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Measure").click();
    for _ in 0..3 {
        h.step();
    }
    h.get_by_role_and_label(egui::accesskit::Role::RadioButton, "Imperial").click();
    h.step();
    assert!(saved.lock().unwrap().contains(&("measurement_units".into(), "imperial".into())));
    let mut restored = RustGisApp::new(Session::new(Project::default())).with_services(Services {
        load_pref: Some(Box::new(|k| (k == "measurement_units").then(|| "imperial".into()))),
        ..Services::default()
    });
    assert!(restored.ui.measurement_imperial);
    restored.run("ui.measurementUnits", json!({"units": "metric"})).unwrap();
    assert!(!restored.ui.measurement_imperial);
    assert!(restored.run("ui.measurementUnits", json!({"units": "unknown"})).is_err());
    assert!(!restored.ui.measurement_imperial);
}

#[test]
fn moon_measure_readout_uses_its_sphere_instead_of_wgs84() {
    let mut h = harness();
    h.state_mut().run("map.body", json!({"name": "moon", "basemap": false})).unwrap();
    h.state_mut().run("map.goToXY", json!({"lon": 0, "lat": 0, "scale": 100000})).unwrap();
    h.state_mut().run("tool.set", json!({"tool": "measure"})).unwrap();
    for _ in 0..3 {
        h.step();
    }
    let center = h.state().map.view_rect.unwrap().0.center();
    click_map(&mut h, center - egui::vec2(50.0, 0.0));
    let first = h.state().map.pointer_lonlat.unwrap();
    click_map(&mut h, center + egui::vec2(50.0, 0.0));
    let last = h.state().map.pointer_lonlat.unwrap();
    assert_eq!(h.state().map.sketch_len(), 2);
    let body = h.state().session.map().body();
    let expected = body.length(rustgis_engine::geom::geodesic_length_m(&[first, last]));
    let expected = rustgis_engine::geom::format_distance(expected);
    let text = painted_text(&h);
    let readout = text.iter().find(|s| s.starts_with("Total ")).unwrap_or_else(|| panic!("{text:?}"));
    assert!(readout.starts_with(&format!("Total {expected}   ")), "{readout}; expected {expected}");
}
