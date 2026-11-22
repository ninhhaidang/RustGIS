//! Real pointer releases must not apply an old vertex preview to changed source rows.
use egui::{Event, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use rustgis_engine::{
    Project, Session,
    data::{Dataset, Feature, Geometry, VertexId},
    geom::{Coord, merc_to_lonlat},
};
use rustgis_ui_egui::RustGisApp;
use serde_json::json;

/// Places synthetic metre-based fixtures in the source longitude/latitude coordinate system.
fn c(x: f64, y: f64) -> Coord {
    merc_to_lonlat(Coord::new(x, y))
}

/// Shows one selected line in the normal application, with snapping disabled for exact ownership assertions.
fn harness() -> Harness<'static, RustGisApp> {
    harness_with(line(0.))
}

/// Gives lines and polygon rings the same production map/command path without changing their topology.
fn harness_with(geometry: Geometry) -> Harness<'static, RustGisApp> {
    let mut session = Session::new(Project::new("Vertex drag ownership"));
    let mut data = Dataset::new("Paths", geometry.kind(), vec![]);
    data.features = vec![Feature { id: 1, geometry: Some(geometry), attrs: vec![] }].into();
    let id = session.map_mut().add_dataset(data);
    session.map_mut().layer_mut(id).unwrap().selection.push(0);
    session.map_mut().camera.center = Coord::new(80., 80.);
    session.map_mut().camera.res = 1.;
    session.map_mut().editing.snapping.enabled = false;
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

/// Keeps the replacement topology and node address valid, so command validation alone cannot reject it.
fn line(x: f64) -> Geometry {
    Geometry::Line(vec![vec![c(x, 0.), c(x, 160.)]])
}

/// Converts source coordinates through the actual map rectangle and camera.
fn screen(h: &Harness<'_, RustGisApp>, at: Coord) -> Pos2 {
    let rect = h.state().map.view_rect.unwrap().0;
    let map = h.state().session.map();
    let (x, y) = map.camera.to_screen(map.to_map(at).unwrap(), rect.width() as f64, rect.height() as f64);
    rect.min + egui::vec2(x as f32, y as f32)
}

/// Starts a held production drag, leaving the stored feature untouched until release.
fn drag(h: &mut Harness<'_, RustGisApp>, insert: bool) -> Pos2 {
    let from = screen(h, c(0., if insert { 80. } else { 0. }));
    let to = screen(h, c(90., 40.));
    h.event(Event::PointerMoved(from));
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for i in 1..=8 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 8.)));
        h.step();
    }
    assert_eq!(h.state().map.vertex_drag.unwrap().insert, insert);
    to
}

/// Delivers a queued release to mapview rather than directly invoking the commit helper.
fn release(h: &mut Harness<'_, RustGisApp>, at: Pos2) {
    h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    for _ in 0..3 {
        h.step();
    }
}

/// Reads the source node, independently of the drag overlay.
fn first(h: &Harness<'_, RustGisApp>) -> Coord {
    h.state().session.map().layers[0].dataset.features[0].geometry.as_ref().unwrap().vertex(VertexId::new(0, 0, 0)).unwrap()
}

/// A queued release must not overwrite a valid replacement at the same layer, row and vertex address.
#[test]
fn replaced_geometry_rejects_queued_release() {
    let mut h = harness();
    let to = drag(&mut h, false);
    h.state_mut()
        .run(
            "edit.setGeometry",
            json!({"layer":"Paths", "row":0, "geometry":{"type":"LineString", "coordinates":[[0.001,0.],[0.001,0.002]]}}),
        )
        .unwrap();
    let before = first(&h);
    let depth = h.state().session.undo_depth();
    release(&mut h, to);
    assert_eq!(first(&h), before, "old preview overwrote the replacement geometry");
    assert_eq!(h.state().session.undo_depth(), depth);
    assert!(h.state().map.vertex_drag.is_none());
}

/// Undo while holding a drag must not turn the eventual release into a new edit of restored rows.
#[test]
fn undo_during_drag_rejects_queued_release() {
    let mut h = harness();
    h.state_mut().run("edit.vertex", json!({"layer":"Paths", "row":0, "action":"move", "index":1, "lon":0., "lat":0.002})).unwrap();
    h.step();
    let to = drag(&mut h, false);
    h.state_mut().run("edit.undo", json!(null)).unwrap();
    let before = first(&h);
    release(&mut h, to);
    assert_eq!(first(&h), before, "old preview became a new edit after undo");
    assert_eq!(h.state().session.undo_depth(), 0);
    assert!(h.state().session.can_redo());
}

/// Redo replaces rows too; the pending release must preserve the redone geometry and history.
#[test]
fn redo_during_drag_rejects_queued_release() {
    let mut h = harness();
    h.state_mut().run("edit.vertex", json!({"layer":"Paths", "row":0, "action":"move", "index":1, "lon":0., "lat":0.002})).unwrap();
    h.state_mut().run("edit.undo", json!(null)).unwrap();
    h.step();
    let to = drag(&mut h, false);
    h.state_mut().run("edit.redo", json!(null)).unwrap();
    let expected = h.state().session.map().layers[0].dataset.features[0].geometry.clone();
    release(&mut h, to);
    assert_eq!(h.state().session.map().layers[0].dataset.features[0].geometry, expected);
    assert_eq!(h.state().session.undo_depth(), 1);
}

/// Reusing a row and even the same feature id cannot revive an old gesture, without a geometry revision bump.
#[test]
fn row_reuse_without_touch_rejects_queued_release() {
    let mut h = harness();
    let to = drag(&mut h, false);
    let layer = &mut h.state_mut().session.map_mut().layers[0];
    let revision = layer.geometry_rev;
    layer.dataset.features = vec![Feature { id: 1, geometry: Some(line(30.)), attrs: vec![] }].into();
    assert_eq!(layer.geometry_rev, revision);
    release(&mut h, to);
    assert_eq!(first(&h), c(30., 0.));
    assert_eq!(h.state().session.undo_depth(), 0);
}

/// A clone can share the same feature allocation while belonging to a different map slot.
#[test]
fn activating_cloned_map_rejects_queued_release() {
    let mut h = harness();
    let mut other = h.state().session.map().clone();
    other.name = "Other".into();
    h.state_mut().session.project.maps.push(other);
    let to = drag(&mut h, false);
    h.state_mut().run("map.activate", json!({"name":"Other"})).unwrap();
    release(&mut h, to);
    assert_eq!(first(&h), c(0., 0.));
    assert_eq!(h.state().session.undo_depth(), 0);
    assert!(h.state().map.vertex_drag.is_none());
}

/// Renaming in place cancels movement and insertion even when rows, map slot, project and
/// history are unchanged; the queued release must preserve the whole source geometry.
#[test]
fn renaming_active_map_rejects_queued_release() {
    for insert in [false, true] {
        let mut h = harness();
        let rows = h.state().session.map().layers[0].dataset.features.allocation_key();
        let map = h.state().session.project.active_map;
        let project = h.state().session.opened;
        let before = h.state().session.map().layers[0].dataset.features[0].geometry.clone();
        let to = drag(&mut h, insert);
        // Isolate the rename-only branch rather than introducing a row edit or an undo step.
        h.state_mut().session.map_mut().name = "Renamed".into();
        assert!(rows.ptr_eq(&h.state().session.map().layers[0].dataset.features.allocation_key()));
        assert_eq!(h.state().session.project.active_map, map);
        assert_eq!(h.state().session.opened, project);
        h.step();
        assert!(h.state().map.vertex_drag.is_none());
        assert!(h.state().map.vertex.is_none());
        release(&mut h, to);
        assert_eq!(h.state().session.map().layers[0].dataset.features[0].geometry, before);
        assert_eq!(h.state().session.undo_depth(), 0);
    }
}

/// Deleting the original map can replace its slot with a clone sharing the same source allocation.
#[test]
fn replacing_map_slot_with_clone_rejects_queued_release() {
    let mut h = harness();
    let old_name = h.state().session.map().name.clone();
    h.state_mut().run("map.duplicate", json!({"name":"Other"})).unwrap();
    // Duplication clears selection; keep the replacement editable to exercise context identity.
    h.state_mut().session.map_mut().layers[0].selection.push(0);
    h.state_mut().run("map.activate", json!({"name":old_name})).unwrap();
    h.step();
    let to = drag(&mut h, false);
    h.state_mut().run("map.remove", json!({"name":old_name})).unwrap();
    let depth = h.state().session.undo_depth();
    release(&mut h, to);
    assert_eq!(first(&h), c(0., 0.));
    assert_eq!(h.state().session.undo_depth(), depth);
    assert!(h.state().map.vertex_drag.is_none());
}

/// Reopening a serialized project must not allow the previous project's queued release to edit it.
#[test]
fn reopening_project_rejects_queued_release() {
    let mut h = harness();
    let text = serde_json::to_string(&h.state().session.project).unwrap();
    let to = drag(&mut h, false);
    let opened = h.state().session.opened;
    h.state_mut().run("project.openText", json!({"text":text})).unwrap();
    assert_ne!(h.state().session.opened, opened);
    release(&mut h, to);
    assert_eq!(first(&h), c(0., 0.));
    assert_eq!(h.state().session.undo_depth(), 0);
}

/// Losing selection, visibility or editability cancels both movement and midpoint insertion.
#[test]
fn ineligible_source_cancels_move_and_insertion() {
    for insert in [false, true] {
        for change in 0..3 {
            let mut h = harness();
            let to = drag(&mut h, insert);
            let before = h.state().session.map().layers[0].dataset.features[0].geometry.clone();
            match change {
                0 => h.state_mut().session.map_mut().layers[0].selection.clear(),
                1 => h.state_mut().session.map_mut().layers[0].visible = false,
                _ => h.state_mut().session.map_mut().editing.settings.enabled = false,
            }
            h.step();
            assert!(h.state().map.vertex_drag.is_none(), "stale preview remained visible");
            assert!(h.state().map.vertex.is_none());
            release(&mut h, to);
            assert_eq!(h.state().session.map().layers[0].dataset.features[0].geometry, before);
            assert_eq!(h.state().session.undo_depth(), 0);
        }
    }
}

/// A source changed through the public mutable rows API invalidates an insertion without retaining a strong row owner.
#[test]
fn in_place_geometry_change_cancels_insertion() {
    let mut h = harness();
    let to = drag(&mut h, true);
    h.state_mut().session.map_mut().layers[0].dataset.features[0].geometry = Some(line(30.));
    let expected = h.state().session.map().layers[0].dataset.features[0].geometry.clone();
    h.step();
    assert!(h.state().map.vertex_drag.is_none());
    release(&mut h, to);
    assert_eq!(h.state().session.map().layers[0].dataset.features[0].geometry, expected);
    assert_eq!(h.state().session.undo_depth(), 0);
}

/// Style and camera changes can bump Session revision without changing which source rows own the gesture.
#[test]
fn style_and_camera_changes_keep_a_valid_drag() {
    let mut h = harness();
    let to = drag(&mut h, false);
    let revision = h.state().session.revision;
    h.state_mut().run("layer.color", json!({"layer":"Paths", "color":"#3684B5"})).unwrap();
    h.state_mut().run("map.setBearing", json!({"bearing":27.})).unwrap();
    assert_ne!(h.state().session.revision, revision);
    h.step();
    assert!(h.state().map.vertex_drag.is_some());
    let depth = h.state().session.undo_depth();
    release(&mut h, to);
    assert_ne!(first(&h), c(0., 0.));
    assert_eq!(h.state().session.undo_depth(), depth + 1);
    h.state_mut().run("edit.undo", json!(null)).unwrap();
    assert_eq!(first(&h), c(0., 0.));
}

/// Normal movement and insertion stay read-only until release, commit once, and undo/redo once.
#[test]
fn unchanged_source_commits_once_and_undoes_once() {
    for insert in [false, true] {
        let mut h = harness();
        let before = h.state().session.map().layers[0].dataset.features[0].geometry.clone();
        let to = drag(&mut h, insert);
        assert_eq!(h.state().session.map().layers[0].dataset.features[0].geometry, before);
        assert_eq!(h.state().session.undo_depth(), 0);
        release(&mut h, to);
        let after = h.state().session.map().layers[0].dataset.features[0].geometry.clone();
        assert_ne!(after, before);
        assert_eq!(h.state().session.undo_depth(), 1);
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(h.state().session.map().layers[0].dataset.features[0].geometry, before);
        h.state_mut().run("edit.redo", json!(null)).unwrap();
        assert_eq!(h.state().session.map().layers[0].dataset.features[0].geometry, after);
    }
}

/// Returning to the source node before release must still leave history unchanged.
#[test]
fn returning_to_origin_keeps_noop_semantics() {
    let mut h = harness();
    drag(&mut h, false);
    let origin = screen(&h, c(0., 0.));
    h.event(Event::PointerMoved(origin));
    h.step();
    release(&mut h, origin);
    assert_eq!(first(&h), c(0., 0.));
    assert_eq!(h.state().session.undo_depth(), 0);
}

/// The owner guard preserves the independent ends of a closed line and the alias of a polygon ring's closure.
#[test]
fn closed_line_and_polygon_keep_their_existing_closure_semantics() {
    for polygon in [false, true] {
        let path = vec![c(0., 0.), c(0., 160.), c(160., 160.), c(160., 0.), c(0., 0.)];
        let source = if polygon { Geometry::Polygon(vec![vec![path]]) } else { Geometry::Line(vec![path]) };
        let mut h = harness_with(source.clone());
        let to = drag(&mut h, false);
        let preview = h.state().map.vertex_drag.unwrap();
        release(&mut h, to);
        let actual = h.state().session.map().layers[0].dataset.features[0].geometry.as_ref().unwrap();
        assert_eq!(actual.vertex(VertexId::new(0, 0, 0)), Some(preview.at));
        match actual {
            Geometry::Polygon(parts) => assert_eq!(parts[0][0].first(), parts[0][0].last()),
            Geometry::Line(parts) => assert_eq!(parts[0].last(), Some(&c(0., 0.))),
            _ => panic!("fixture changed kind"),
        }
        assert_eq!(h.state().session.undo_depth(), 1);
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(h.state().session.map().layers[0].dataset.features[0].geometry, Some(source));
    }
}
