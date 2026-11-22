//! Real Ribbon and pointer events for the opt-in shared polygon move command.
use egui::{Event, Modifiers, PointerButton, Pos2};
use egui_kittest::{Harness, kittest::Queryable};
use rustgis_engine::{
    Project, Session,
    data::{Dataset, Feature, Geometry, GeometryType, VertexId},
    geom::{Coord, merc_to_lonlat},
};
use rustgis_ui_egui::RustGisApp;
use serde_json::json;

/// Stores metre-based test geometry in longitude/latitude without import rounding.
fn c(x: f64, y: f64) -> Coord {
    merc_to_lonlat(Coord::new(x, y))
}

/// Four parcels meet at zero, but only the north-east parcel is selected.
fn harness() -> Harness<'static, RustGisApp> {
    let mut session = Session::new(Project::new("Shared polygon vertices"));
    let mut data = Dataset::new("Parcels", GeometryType::Polygon, vec![]);
    data.features = [(0., 0., 160., 160.), (-160., 0., 0., 160.), (-160., -160., 0., 0.), (0., -160., 160., 0.)]
        .into_iter()
        .enumerate()
        .map(|(i, (l, b, r, t))| Feature {
            id: i as u64 + 40,
            geometry: Some(Geometry::Polygon(vec![vec![vec![c(l, b), c(r, b), c(r, t), c(l, t), c(l, b)]]])),
            attrs: vec![],
        })
        .collect::<Vec<_>>()
        .into();
    let id = session.map_mut().add_dataset(data);
    session.active_layer = Some(id);
    session.map_mut().layer_mut(id).unwrap().selection = vec![0];
    session.map_mut().camera.center = Coord::new(0., 0.);
    session.map_mut().camera.res = 0.8;
    session.map_mut().camera.set_bearing(17.);
    session.map_mut().editing.snapping.enabled = false;
    let mut app = RustGisApp::new(session);
    app.run("tool.set", json!({"tool":"editVertices"})).unwrap();
    app.ui.tab = "Edit".into();
    app.session.clear_history();
    let mut h = Harness::builder().with_accessibility_check(false).with_size(egui::vec2(1366., 900.)).with_max_steps(64).build_ui_state(
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

/// Returns each parcel's complete geometry, independent of selected vertex handles.
fn geometries(h: &Harness<'_, RustGisApp>) -> Vec<Geometry> {
    h.state().session.map().layers[0].dataset.features.iter().map(|f| f.geometry.clone().unwrap()).collect()
}

/// Projects a test point with the actual map rectangle and rotated camera.
fn screen(h: &Harness<'_, RustGisApp>, at: Coord) -> Pos2 {
    let rect = h.state().map.view_rect.unwrap().0;
    let map = h.state().session.map();
    let (x, y) = map.camera.to_screen(map.to_map(at).unwrap(), rect.width() as f64, rect.height() as f64);
    rect.min + egui::vec2(x as f32, y as f32)
}

/// Starts and holds a real drag; the caller can inspect preview purity or change ownership.
fn drag(h: &mut Harness<'_, RustGisApp>, from: Coord, to: Coord) -> Pos2 {
    let from = screen(h, from);
    let to = screen(h, to);
    h.event(Event::PointerMoved(from));
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for i in 1..=8 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 8.)));
        h.step();
    }
    assert!(h.state().map.vertex_drag.is_some());
    to
}

/// Releases the primary pointer through the same production path as a desktop or browser gesture.
fn release(h: &mut Harness<'_, RustGisApp>, to: Pos2) {
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    for _ in 0..3 {
        h.step();
    }
}

/// The actual Ribbon toggle is transient; preview is pure and one release moves all four corners.
#[test]
fn ribbon_toggle_routes_polygon_drag_and_undoes_all_neighbours() {
    let mut h = harness();
    let before = geometries(&h);
    assert!(!h.state().map.move_shared_vertices);
    h.get_by_label("Shared Vertices").click();
    h.run();
    assert!(h.state().map.move_shared_vertices);
    assert_eq!(h.state().session.undo_depth(), 0);
    assert!(!h.state().session.dirty());
    let to = drag(&mut h, c(0., 0.), c(45., 30.));
    let at = h.state().map.vertex_drag.unwrap().at;
    assert_eq!(geometries(&h), before);
    release(&mut h, to);
    let after = geometries(&h);
    for (old, new) in before.iter().zip(&after) {
        for (v, p) in old.vertices() {
            assert_eq!(new.vertex(v), Some(if p == c(0., 0.) { at } else { p }));
        }
    }
    assert_eq!(h.state().session.undo_depth(), 1);
    h.state_mut().run("edit.undo", json!(null)).unwrap();
    assert_eq!(geometries(&h), before);
    h.state_mut().run("edit.redo", json!(null)).unwrap();
    assert_eq!(geometries(&h), after);
    let project = serde_json::to_string(&h.state().session.project).unwrap();
    let fresh = RustGisApp::new(Session::new(Project::from_json(&project).unwrap()));
    assert!(!fresh.map.move_shared_vertices);
}

/// The default/off path retains a single-feature move even when other corners coincide.
#[test]
fn disabled_toggle_keeps_the_existing_vertex_command() {
    let mut h = harness();
    let before = geometries(&h);
    let to = drag(&mut h, c(0., 0.), c(45., 30.));
    release(&mut h, to);
    let after = geometries(&h);
    assert_ne!(after[0], before[0]);
    assert_eq!(after[1..], before[1..]);
    assert_eq!(h.state().session.undo_depth(), 1);
}

/// Neither enabling the switch nor an owned drag changes the existing insertion/deletion commands.
#[test]
fn insert_and_delete_remain_single_feature_edits() {
    let mut h = harness();
    h.state_mut().run("ui.moveSharedVertices", json!({"on":true})).unwrap();
    let before = geometries(&h);
    let to = drag(&mut h, c(80., 0.), c(80., -30.));
    assert!(h.state().map.vertex_drag.unwrap().insert);
    release(&mut h, to);
    assert_eq!(geometries(&h)[1..], before[1..]);
    assert_eq!(geometries(&h)[0].vertices().len(), 5);
    h.key_press(egui::Key::Delete);
    h.step();
    assert_eq!(geometries(&h), before);
    assert_eq!(h.state().session.undo_depth(), 2);
}

/// A changed neighbour or switch invalidates the held shared move and swallows its release.
#[test]
fn changed_layer_or_toggle_cancels_a_shared_gesture() {
    for toggle in [false, true] {
        let mut h = harness();
        h.state_mut().run("ui.moveSharedVertices", json!({"on":true})).unwrap();
        let to = drag(&mut h, c(0., 0.), c(45., 30.));
        if toggle {
            h.state_mut().run("ui.moveSharedVertices", json!({"on":false})).unwrap();
        } else {
            h.state_mut().session.map_mut().layers[0].dataset.features[1].id = 999;
        }
        let before = geometries(&h);
        release(&mut h, to);
        assert_eq!(geometries(&h), before);
        assert_eq!(h.state().session.undo_depth(), 0);
        assert!(h.state().map.vertex_drag.is_none());
    }
}

/// Escape preserves data/history, and a held click at its starting point remains a UI no-op.
#[test]
fn escape_and_same_position_release_do_not_commit() {
    let mut h = harness();
    h.state_mut().run("ui.moveSharedVertices", json!({"on":true})).unwrap();
    let before = geometries(&h);
    let to = drag(&mut h, c(0., 0.), c(45., 30.));
    h.key_press(egui::Key::Escape);
    h.step();
    release(&mut h, to);
    assert_eq!(geometries(&h), before);
    assert_eq!(h.state().session.undo_depth(), 0);
    // Escape may end the active tool; test return-to-origin in an independent owned gesture.
    let mut h = harness();
    h.state_mut().run("ui.moveSharedVertices", json!({"on":true})).unwrap();
    let before = geometries(&h);
    drag(&mut h, c(0., 0.), c(45., 30.));
    let from = screen(&h, c(0., 0.));
    h.event(Event::PointerMoved(from));
    h.step();
    release(&mut h, from);
    assert_eq!(geometries(&h), before);
    assert_eq!(h.state().session.undo_depth(), 0);
}

/// Non-polygons keep single-feature moves even while the transient polygon mode is enabled.
#[test]
fn lines_and_points_keep_the_existing_move_path() {
    for geometry in [Geometry::Line(vec![vec![c(0., 0.), c(0., 160.)]]), Geometry::Point(c(0., 0.))] {
        let mut h = harness();
        let data = &mut h.state_mut().session.map_mut().layers[0].dataset;
        data.geometry_type = geometry.kind();
        data.features = vec![
            Feature { id: 1, geometry: Some(geometry.clone()), attrs: vec![] },
            Feature { id: 2, geometry: Some(geometry.clone()), attrs: vec![] },
        ]
        .into();
        h.state_mut().run("ui.moveSharedVertices", json!({"on":true})).unwrap();
        h.step();
        let to = drag(&mut h, c(0., 0.), c(45., 30.));
        release(&mut h, to);
        assert_ne!(geometries(&h)[0], geometry);
        assert_eq!(geometries(&h)[1], geometry);
        assert_eq!(h.state().session.undo_depth(), 1);
    }
}

/// A malformed UI switch cannot change its state, source ownership, or project/history.
#[test]
fn malformed_toggle_is_rejected() {
    let mut h = harness();
    for on in [json!("yes"), json!(1), json!([]), json!(null)] {
        assert!(h.state_mut().run("ui.moveSharedVertices", json!({"on":on})).is_err());
        assert!(!h.state().map.move_shared_vertices);
    }
    assert_eq!(h.state().session.undo_depth(), 0);
    assert!(geometries(&h)[0].vertex(VertexId::new(0, 0, 0)).is_some());
}
