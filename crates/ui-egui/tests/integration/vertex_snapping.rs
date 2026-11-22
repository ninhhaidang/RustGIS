//! Real pointer gestures for source-addressed snapping in the application.
use egui::{Event, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use rustgis_engine::{
    Project, Session,
    data::{Dataset, Feature, Geometry, VertexId},
    geom::{Coord, merc_to_lonlat},
    map::design::Snapping,
};
use rustgis_ui_egui::RustGisApp;
use serde_json::json;

/// Stores a small metre-based fixture in the same longitude/latitude format as imported data.
fn c(x: f64, y: f64) -> Coord {
    merc_to_lonlat(Coord::new(x, y))
}

/// A selected multipart line exposes both an ordinary vertex and an insertion midpoint.
fn harness(bearing: f64) -> Harness<'static, RustGisApp> {
    let mut session = Session::new(Project::new("Vertex snapping"));
    let geometry = Geometry::Line(vec![vec![c(0., 0.), c(0., 160.)], vec![c(180., 0.), c(180., 160.)]]);
    let mut data = Dataset::new("Editable paths", geometry.kind(), vec![]);
    data.features = vec![Feature { id: 1, geometry: Some(geometry), attrs: vec![] }].into();
    let id = session.map_mut().add_dataset(data);
    session.map_mut().layer_mut(id).unwrap().selection.push(0);
    session.map_mut().camera.center = Coord::new(90., 80.);
    session.map_mut().camera.res = 1.;
    session.map_mut().camera.set_bearing(bearing);
    session.map_mut().editing.snapping = Snapping { enabled: true, tolerance_px: 8., vertex: true, edge: false, end: false };
    let mut app = RustGisApp::new(session);
    app.run("tool.set", json!({"tool":"editVertices"})).unwrap();
    app.ui.tab = "Edit".into();
    app.session.clear_history();
    let mut h = Harness::builder().with_accessibility_check(false).with_size(egui::vec2(1480., 900.)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        app,
    );
    for _ in 0..6 {
        h.step();
    }
    h
}

/// Converts fixture coordinates using the actual map rectangle and rotated camera.
fn screen(h: &Harness<'_, RustGisApp>, at: Coord) -> Pos2 {
    let rect = h.state().map.view_rect.unwrap().0;
    let map = h.state().session.map();
    let (x, y) = map.camera.to_screen(map.to_map(at).unwrap(), rect.width() as f64, rect.height() as f64);
    rect.min + egui::vec2(x as f32, y as f32)
}

/// A held drag follows the production threshold and event path instead of calling tool helpers.
fn drag(h: &mut Harness<'_, RustGisApp>, from: Pos2, to: Pos2) {
    h.event(Event::PointerMoved(from));
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for i in 1..=8 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 8.)));
        h.step();
    }
}

/// Releases the held primary pointer and lets the edit command finish.
fn release(h: &mut Harness<'_, RustGisApp>, at: Pos2) {
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    for _ in 0..3 {
        h.step();
    }
}

/// Reads the first source node; previews must never replace this stored geometry.
fn first(h: &Harness<'_, RustGisApp>) -> Coord {
    h.state().session.map().layers[0].dataset.features[0].geometry.as_ref().unwrap().vertex(VertexId::new(0, 0, 0)).unwrap()
}

/// Rotation does not change snap tolerance; a preview is read-only and release is one undo step.
#[test]
fn rotated_drag_snaps_to_another_part_and_undoes_once() {
    let mut h = harness(27.);
    let camera = h.state().session.map().camera;
    let from = screen(&h, c(0., 0.));
    let to = screen(&h, c(180., 0.)) + egui::vec2(3., 2.);
    drag(&mut h, from, to);
    let preview = h.state().map.vertex_drag.expect("vertex drag");
    assert!(preview.snapped && !preview.insert);
    assert!((preview.at.x - c(180., 0.).x).abs() < 1e-9);
    assert_eq!(first(&h), c(0., 0.));
    assert_eq!(h.state().session.undo_depth(), 0);
    release(&mut h, to);
    assert!((first(&h).x - c(180., 0.).x).abs() < 1e-9);
    assert_eq!(h.state().session.undo_depth(), 1);
    assert_eq!(h.state().session.map().camera, camera);
    h.state_mut().run("edit.undo", json!(null)).unwrap();
    assert_eq!(first(&h), c(0., 0.));
    h.state_mut().run("edit.redo", json!(null)).unwrap();
    assert!((first(&h).x - c(180., 0.).x).abs() < 1e-9);
}

/// Escape cancels a snapped preview without geometry, camera or undo changes.
#[test]
fn escape_cancels_the_snapped_preview() {
    let mut h = harness(0.);
    let camera = h.state().session.map().camera;
    let from = screen(&h, c(0., 0.));
    let to = screen(&h, c(180., 0.)) + egui::vec2(3., 2.);
    drag(&mut h, from, to);
    assert!(h.state().map.vertex_drag.unwrap().snapped);
    h.key_press(egui::Key::Escape);
    h.step();
    release(&mut h, to);
    assert!(h.state().map.vertex_drag.is_none());
    assert_eq!(first(&h), c(0., 0.));
    assert_eq!(h.state().session.undo_depth(), 0);
    assert_eq!(h.state().session.map().camera, camera);
}

/// Midpoint insertion retains whole-feature exclusion, so it cannot snap back onto itself.
#[test]
fn midpoint_insertion_keeps_whole_feature_exclusion() {
    let mut h = harness(0.);
    let from = screen(&h, c(0., 80.));
    let to = screen(&h, c(180., 0.)) + egui::vec2(3., 2.);
    drag(&mut h, from, to);
    let preview = h.state().map.vertex_drag.unwrap();
    assert!(preview.insert && !preview.snapped);
    release(&mut h, to);
    let geometry = h.state().session.map().layers[0].dataset.features[0].geometry.as_ref().unwrap();
    assert_eq!(geometry.vertex(VertexId::new(0, 0, 1)), Some(preview.at));
    assert_eq!(geometry.vertex(VertexId::new(0, 0, 2)), Some(c(0., 160.)));
    assert_eq!(h.state().session.undo_depth(), 1);
}

/// Disabled snapping and a pointer beyond tolerance both leave the preview at the pointer.
#[test]
fn disabled_and_outside_tolerance_drags_remain_free() {
    for enabled in [false, true] {
        let mut h = harness(0.);
        h.state_mut().session.map_mut().editing.snapping.enabled = enabled;
        let from = screen(&h, c(0., 0.));
        let to = screen(&h, c(180., 0.)) + egui::vec2(if enabled { 12. } else { 3. }, 2.);
        drag(&mut h, from, to);
        assert!(!h.state().map.vertex_drag.unwrap().snapped);
        release(&mut h, to);
        assert_eq!(h.state().session.undo_depth(), 1);
        assert!((first(&h).x - c(180., 0.).x).abs() > 1e-6);
    }
}

/// Turning editing off prevents this gesture from starting a node edit or creating undo state.
#[test]
fn read_only_map_does_not_start_a_vertex_edit() {
    let mut h = harness(0.);
    h.state_mut().session.map_mut().editing.settings.enabled = false;
    let from = screen(&h, c(0., 0.));
    let to = screen(&h, c(180., 0.));
    drag(&mut h, from, to);
    assert!(h.state().map.vertex_drag.is_none());
    release(&mut h, to);
    assert_eq!(first(&h), c(0., 0.));
    assert_eq!(h.state().session.undo_depth(), 0);
}
