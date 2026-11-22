//! Actual map gestures share the engine's Point/MultiPoint target switches.
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

/// Keeps synthetic feature coordinates in longitude/latitude while laying them out in metres.
fn c(x: f64, y: f64) -> Coord {
    merc_to_lonlat(Coord::new(x, y))
}

/// Creates an ordinary upstream feature layer; no files, provider or product shell is involved.
fn add(session: &mut Session, name: &str, geometry: Geometry) -> u32 {
    let mut data = Dataset::new(name, geometry.kind(), vec![]);
    data.features = vec![Feature { id: 1, geometry: Some(geometry), attrs: vec![] }].into();
    session.map_mut().add_dataset(data)
}

/// Shows one selected source and an independent target in the application's actual map view.
fn harness(source: Geometry, target: Geometry, vertex: bool, bearing: f64) -> Harness<'static, RustGisApp> {
    let mut session = Session::new(Project::new("Point target switches"));
    let id = add(&mut session, "Editable source", source);
    add(&mut session, "Snap targets", target);
    // New layers are inserted at the front; keep the source first for the stored-node assertions.
    session.map_mut().layers.reverse();
    session.active_layer = Some(id);
    session.map_mut().layer_mut(id).unwrap().selection.push(0);
    session.map_mut().camera.center = Coord::new(100., 80.);
    session.map_mut().camera.res = 1.;
    session.map_mut().camera.set_bearing(bearing);
    session.map_mut().editing.snapping = Snapping { enabled: true, tolerance_px: 8., vertex, edge: !vertex, end: false };
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

/// Supplies a line whose first node can move without changing any target geometry.
fn line() -> Geometry {
    Geometry::Line(vec![vec![c(0., 0.), c(0., 160.)]])
}

/// Converts a fixture coordinate using the actual map rectangle and camera bearing.
fn screen(h: &Harness<'_, RustGisApp>, at: Coord) -> Pos2 {
    let rect = h.state().map.view_rect.unwrap().0;
    let map = h.state().session.map();
    let (x, y) = map.camera.to_screen(map.to_map(at).unwrap(), rect.width() as f64, rect.height() as f64);
    rect.min + egui::vec2(x as f32, y as f32)
}

/// Holds the primary button while moving through real pointer events past the drag threshold.
fn drag(h: &mut Harness<'_, RustGisApp>, from: Pos2, to: Pos2) {
    h.event(Event::PointerMoved(from));
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for i in 1..=8 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 8.)));
        h.step();
    }
}

/// Releases the pointer and processes the single edit or point-creation command.
fn release(h: &mut Harness<'_, RustGisApp>, at: Pos2) {
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    for _ in 0..3 {
        h.step();
    }
}

/// Reads a stored source node, which a drag preview must leave untouched.
fn first(h: &Harness<'_, RustGisApp>) -> Coord {
    h.state().session.map().layers[0].dataset.features[0].geometry.as_ref().unwrap().vertex(VertexId::new(0, 0, 0)).unwrap()
}

/// Compares projected round trips without allowing a visible fraction of a fixture pixel.
fn near(actual: Coord, expected: Coord) {
    assert!((actual.x - expected.x).abs() < 1e-9 && (actual.y - expected.y).abs() < 1e-9, "{actual:?} != {expected:?}");
}

/// Finds the map's painted magenta snap ring without exposing the application's private cache.
fn marker(h: &Harness<'_, RustGisApp>) -> Option<Pos2> {
    /// Searches nested paint lists for the existing seven-point snap-ring style.
    fn find(shape: &egui::Shape) -> Option<Pos2> {
        match shape {
            egui::Shape::Circle(circle) if circle.radius == 7. && circle.stroke.color == egui::Color32::from_rgb(0xD0, 0x2A, 0xB0) => {
                Some(circle.center)
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(find),
            _ => None,
        }
    }
    h.output().shapes.iter().find_map(|shape| find(&shape.shape))
}

/// A rotated drag targets a later MultiPoint member, previews read-only and commits once.
#[test]
fn rotated_drag_to_multipoint_commits_and_undoes_once() {
    let target = c(180., 40.);
    let mut h = harness(line(), Geometry::MultiPoint(vec![c(260., 160.), target]), true, 27.);
    let camera = h.state().session.map().camera;
    let from = screen(&h, c(0., 0.));
    let to = screen(&h, target) + egui::vec2(3., 2.);
    drag(&mut h, from, to);
    let preview = h.state().map.vertex_drag.unwrap();
    assert!(preview.snapped && !preview.insert);
    near(preview.at, target);
    assert!(marker(&h).unwrap().distance(screen(&h, target)) < 0.1);
    near(first(&h), c(0., 0.));
    assert_eq!(h.state().session.undo_depth(), 0);
    release(&mut h, to);
    near(first(&h), target);
    assert_eq!(h.state().session.undo_depth(), 1);
    assert_eq!(h.state().session.map().camera, camera);
    h.state_mut().run("edit.undo", json!(null)).unwrap();
    near(first(&h), c(0., 0.));
    h.state_mut().run("edit.redo", json!(null)).unwrap();
    near(first(&h), target);
}

/// With only edges enabled, a nearby Point supplies neither a snap preview nor a snapped commit.
#[test]
fn edge_only_drag_ignores_point_target() {
    let target = c(180., 40.);
    let mut h = harness(line(), Geometry::Point(target), false, 0.);
    let from = screen(&h, c(0., 0.));
    let to = screen(&h, target) + egui::vec2(3., 2.);
    drag(&mut h, from, to);
    let preview = h.state().map.vertex_drag.unwrap();
    assert!(!preview.snapped);
    assert!(marker(&h).is_none());
    assert!((preview.at.x - target.x).abs() > 1e-6);
    near(first(&h), c(0., 0.));
    release(&mut h, to);
    near(first(&h), preview.at);
    assert_eq!(h.state().session.undo_depth(), 1);
}

/// Escape cancels a new multi-point target preview, and a read-only map refuses the same drag.
#[test]
fn cancelled_and_read_only_drags_leave_source_untouched() {
    for read_only in [false, true] {
        let target = c(180., 40.);
        let mut h = harness(line(), Geometry::MultiPoint(vec![target]), true, 0.);
        h.state_mut().session.map_mut().editing.settings.enabled = !read_only;
        let from = screen(&h, c(0., 0.));
        let to = screen(&h, target) + egui::vec2(3., 2.);
        drag(&mut h, from, to);
        if read_only {
            assert!(h.state().map.vertex_drag.is_none());
        } else {
            assert!(h.state().map.vertex_drag.unwrap().snapped);
            h.key_press(egui::Key::Escape);
            h.step();
        }
        release(&mut h, to);
        assert!(h.state().map.vertex_drag.is_none());
        near(first(&h), c(0., 0.));
        assert_eq!(h.state().session.undo_depth(), 0);
    }
}

/// Moving a MultiPoint member continues to exclude its entire own feature, including other members.
#[test]
fn multipoint_source_remains_excluded_as_a_whole() {
    let own = c(180., 40.);
    let mut h = harness(Geometry::MultiPoint(vec![c(0., 0.), own]), Geometry::Point(c(300., 160.)), true, 0.);
    let from = screen(&h, c(0., 0.));
    let to = screen(&h, own) + egui::vec2(3., 2.);
    drag(&mut h, from, to);
    let preview = h.state().map.vertex_drag.unwrap();
    assert!(!preview.snapped);
    release(&mut h, to);
    near(first(&h), preview.at);
    let geometry = h.state().session.map().layers[0].dataset.features[0].geometry.as_ref().unwrap();
    assert_eq!(geometry.vertex(VertexId::new(0, 0, 1)), Some(own));
    assert_eq!(h.state().session.undo_depth(), 1);
}

/// Creation's cached marker and command-based placement agree for enabled multi-points and disabled points.
#[test]
fn creation_preview_and_committed_position_agree() {
    let target = c(180., 40.);
    for (geometry, vertex) in [(Geometry::MultiPoint(vec![target]), true), (Geometry::Point(target), false)] {
        let mut h = harness(Geometry::MultiPoint(vec![]), geometry, vertex, 0.);
        h.state_mut().run("tool.set", json!({"tool":"createPoint"})).unwrap();
        h.state_mut().session.clear_history();
        let to = screen(&h, target) + egui::vec2(3., 2.);
        h.event(Event::PointerMoved(to));
        for _ in 0..3 {
            h.step();
        }
        let preview = marker(&h);
        assert_eq!(preview.is_some(), vertex);
        if let Some(preview) = preview {
            assert!(preview.distance(screen(&h, target)) < 0.1);
        }
        h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.step();
        release(&mut h, to);
        let source = &h.state().session.map().layers[0].dataset.features;
        assert_eq!(source.len(), 2);
        let Geometry::Point(at) = source[1].geometry.as_ref().unwrap() else { panic!("created point") };
        if preview.is_some() {
            near(*at, target);
        } else {
            assert!((at.x - target.x).abs() > 1e-6);
        }
        assert_eq!(h.state().session.undo_depth(), 1);
    }
}
