//! Checkbox menus stay open for consecutive edits; choosing an action still dismisses them.
use egui_kittest::kittest::{NodeT, Queryable};
use rustgis_engine::{Project, Session, data::io::read_geojson};
use rustgis_ui_egui::RustGisApp;
use serde_json::json;

type Harness = egui_kittest::Harness<'static, RustGisApp>;

fn harness(app: RustGisApp) -> Harness {
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1800.0, 1200.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    h.run();
    h
}

fn empty_app() -> RustGisApp {
    RustGisApp::new(Session::new(Project::default()))
}

fn click(h: &mut Harness, label: &str) {
    h.get_by_role_and_label(egui::accesskit::Role::Button, label).click();
    h.run();
}

fn toggle(h: &mut Harness, label: &str) {
    h.get_by_role_and_label(egui::accesskit::Role::CheckBox, label).click();
    h.run();
    assert!(egui::Popup::is_any_open(&h.ctx), "{label} must keep its menu open");
    assert!(h.query_by_role_and_label(egui::accesskit::Role::CheckBox, label).is_some());
}

#[test]
fn basemap_stacking_stays_open_until_a_basemap_is_chosen() {
    let mut app = empty_app();
    app.run("map.addTiles", json!({"url": "https://example.test/{z}/{x}/{y}.png", "id": "existing", "name": "Existing"})).unwrap();
    let mut h = harness(app);
    click(&mut h, "Basemap");
    toggle(&mut h, "Stack on the current basemaps");
    click(&mut h, "USGS Imagery");
    let tiles = &h.state().session.map().tiles;
    assert!(tiles.iter().any(|t| t.id == "existing"), "stacking keeps the previous basemap");
    assert!(tiles.iter().any(|t| t.name == "USGS Imagery"), "the chosen basemap is added");
    assert!(!egui::Popup::is_any_open(&h.ctx), "choosing a basemap dismisses the menu");

    click(&mut h, "Basemap");
    toggle(&mut h, "Stack on the current basemaps");
    click(&mut h, "OpenFreeMap Liberty");
    let tiles = &h.state().session.map().tiles;
    assert_eq!(tiles.len(), 1, "turning stacking off restores replacement");
    assert_eq!(tiles[0].id, "openfreemap-liberty");
}

#[test]
fn basemap_tile_visibility_can_be_toggled_without_reopening() {
    let mut app = empty_app();
    app.run("map.addTiles", json!({"url": "https://example.test/{z}/{x}/{y}.png", "id": "existing", "name": "Existing"})).unwrap();
    let mut h = harness(app);
    click(&mut h, "Basemap");
    for visible in [false, true] {
        toggle(&mut h, "Show or hide");
        assert_eq!(h.state().session.map().tiles[0].visible, visible);
    }
}

#[test]
fn visual_effect_checkboxes_allow_consecutive_changes() {
    let mut app = empty_app();
    app.ui.tab = "View".into();
    let before = app.session.map().view.effects.clone();
    let mut h = harness(app);
    click(&mut h, "Visual Effect");
    toggle(&mut h, "Vignette");
    toggle(&mut h, "Edges");
    toggle(&mut h, "Stars");
    let effects = &h.state().session.map().view.effects;
    assert_eq!(effects.vignette, !before.vignette);
    assert_eq!(effects.edges, !before.edges);
    assert_eq!(effects.stars, !before.stars);
    let pos = egui::pos2(1750.0, 1100.0);
    h.event(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
    }
    h.run();
    assert!(!egui::Popup::is_any_open(&h.ctx), "an outside click still dismisses the menu");
}

#[test]
fn camera_effect_checkboxes_allow_consecutive_changes() {
    let mut app = empty_app();
    app.ui.tab = "View".into();
    let before = app.session.map().view.effects.clone();
    let mut h = harness(app);
    click(&mut h, "Camera Effect");
    toggle(&mut h, "Tilt-shift");
    toggle(&mut h, "Film grain");
    let effects = &h.state().session.map().view.effects;
    assert_eq!(effects.tilt_shift, !before.tilt_shift);
    assert_eq!(effects.grain, !before.grain);
}

#[test]
fn ribbon_customization_checkboxes_allow_consecutive_changes() {
    let mut app = empty_app();
    app.ui.tab = "Settings".into();
    let mut h = harness(app);
    click(&mut h, "Ribbon");
    toggle(&mut h, "Data");
    toggle(&mut h, "Edit");
    assert!(h.state().ui.hidden_tabs.iter().any(|t| t == "Data"));
    assert!(h.state().ui.hidden_tabs.iter().any(|t| t == "Edit"));
    toggle(&mut h, "Data");
    assert!(!h.state().ui.hidden_tabs.iter().any(|t| t == "Data"));
}

#[test]
fn quick_access_checkboxes_allow_consecutive_changes() {
    let mut app = empty_app();
    app.ui.tab = "Settings".into();
    app.ui.quick_access.clear();
    let mut h = harness(app);
    click(&mut h, "Quick Access");
    toggle(&mut h, "Save As");
    toggle(&mut h, "Export Map");
    assert!(h.state().ui.quick_access.iter().any(|c| c == "ui.saveAs"));
    assert!(h.state().ui.quick_access.iter().any(|c| c == "ui.exportMap"));
}

#[test]
fn time_layer_checkboxes_allow_consecutive_bindings() {
    let text = json!({"type": "FeatureCollection", "features": [{
        "type": "Feature", "geometry": {"type": "Point", "coordinates": [0, 0]},
        "properties": {"when": "2025-01-01"}
    }]})
    .to_string();
    let mut app = empty_app();
    for name in ["First events", "Second events"] {
        let (dataset, _) = read_geojson(&text, name).unwrap();
        app.session.map_mut().add_dataset(dataset);
    }
    app.run("time.bind", json!({"layer": "First events", "field": "when"})).unwrap();
    let mut h = harness(app);
    click(&mut h, "Layers (1)");
    toggle(&mut h, "Second events");
    assert!(h.state().session.map().layer_by_name("Second events").unwrap().time.is_some());
    toggle(&mut h, "First events");
    assert!(h.state().session.map().layer_by_name("First events").unwrap().time.is_none());
}

#[test]
fn basemap_tile_actions_still_dismiss_the_gallery() {
    let mut app = empty_app();
    for id in ["first", "second"] {
        app.run(
            "map.addTiles",
            json!({"url": format!("https://example.test/{id}/{{z}}/{{x}}/{{y}}.png"), "id": id, "name": id, "replace": false}),
        )
        .unwrap();
    }
    let mut h = harness(app);
    for label in ["▲", "▼", "Reload", "Remove", "Remove All"] {
        click(&mut h, "Basemap");
        h.query_all_by_label(label)
            .find(|n| !n.accesskit_node().is_disabled())
            .unwrap_or_else(|| panic!("missing enabled {label}"))
            .click();
        h.run();
        assert!(!egui::Popup::is_any_open(&h.ctx), "{label} must still dismiss the gallery");
    }
    assert!(h.state().session.map().tiles.is_empty(), "Remove All still removes the layers");
}

#[test]
fn celestial_body_choices_dismiss_the_basemap_gallery() {
    let mut h = harness(empty_app());
    for body in rustgis_engine::geom::BODIES {
        click(&mut h, "Basemap");
        click(&mut h, body.name);
        assert_eq!(h.state().session.map().body().id, body.id, "the selected body is applied");
        assert!(!egui::Popup::is_any_open(&h.ctx), "{} must dismiss the gallery", body.name);
    }
}

/// The primary button `pressed`, or let go, at `pos`.
fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
}

/// A click on the suggestion list of Display Filters' box writes the item and keeps the menu open,
/// on a row past the menu's bottom edge too, where it's no longer over the menu: whether the
/// button is let go frames after the press, as a click usually is, or in the same frame (a quick
/// tap, a slow frame). A click elsewhere still closes the menu, either way (#1240).
#[test]
fn display_filters_keeps_its_menu_open_on_a_click_in_its_list() {
    use egui::accesskit::Role;
    for held in [true, false] {
        let mut app = empty_app();
        let text = r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"pop_est":1},"geometry":{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0]]]}}]}"#;
        let (dataset, _) = read_geojson(text, "Areas").unwrap();
        app.session.map_mut().add_dataset(dataset);
        app.run("layer.select", json!({"layer": "Areas"})).unwrap();
        app.ui.tab = "Style".into();
        let mut h = harness(app);
        click(&mut h, "Display Filters");
        h.get_by_role_and_label(Role::TextInput, "Below 1:5,000,000 show everything; beyond it only what matches:").click();
        h.run();
        h.event(egui::Event::Text("p".into()));
        h.run();
        let menu_bottom = h.get_by_role_and_label(Role::Button, "Clear Filters").rect().max.y;
        let row = h.get_by_role_and_label(Role::Button, "perimeter()");
        assert!(row.rect().min.y > menu_bottom + 8.0, "the row is past the menu's edge");
        let at = row.rect().center();
        h.event(egui::Event::PointerMoved(at));
        h.step();
        if held {
            h.event(button(at, true));
            h.step();
            h.step();
            h.event(button(at, false));
        } else {
            // kittest gives each pointer event a frame of its own: both into one frame by hand.
            h.input_mut().events.extend([button(at, true), button(at, false)]);
        }
        h.run();
        assert!(egui::Popup::is_any_open(&h.ctx), "a click in the list keeps the menu open (held: {held})");
        assert_eq!(h.state().forms.expression, "perimeter()", "held: {held}");
        // A click on the map, outside the menu, still closes it.
        let away = egui::pos2(1750.0, 1100.0);
        h.event(egui::Event::PointerMoved(away));
        h.step();
        if held {
            h.event(button(away, true));
            h.step();
            h.event(button(away, false));
        } else {
            h.input_mut().events.extend([button(away, true), button(away, false)]);
        }
        h.run();
        assert!(!egui::Popup::is_any_open(&h.ctx), "a click outside closes the menu (held: {held})");
    }
}
