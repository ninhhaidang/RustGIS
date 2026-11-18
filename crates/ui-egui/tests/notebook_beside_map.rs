//! #1056: the Notebook opens beside the map on half the room, and a right pane can be dragged
//! wider than 640 points while the map keeps its minimum width. #2416: the right panes other than
//! the Notebook and Web Data share one width.

use egui::vec2;
use egui_kittest::Harness;
use rustgis_ui_egui::RustGisApp;
use serde_json::Value;

/// The window with the pane `cmd` opens in view.
fn window(cmd: &str) -> Harness<'static, RustGisApp> {
    let mut h = Harness::builder().with_size(vec2(1600.0, 900.0)).with_max_steps(64).build_ui_state(
        |ui, app: &mut RustGisApp| {
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            app.ui(ui);
        },
        RustGisApp::with_sample(),
    );
    h.state_mut().run(cmd, Value::Null).expect("open the pane");
    for _ in 0..6 {
        h.step();
    }
    h
}

/// The right pane, and the room between the left pane (or the window's edge) and the window's right.
fn rects(h: &Harness<'_, RustGisApp>) -> (egui::Rect, f32) {
    let [left, right] = h.state().ui.pane_rects;
    let right = right.expect("a right pane in view");
    let start = left.map_or(h.ctx.content_rect().min.x, |l| l.max.x);
    (right, h.ctx.content_rect().max.x - start)
}

/// Drags the right pane's edge `dx` points towards the map.
fn drag_edge(h: &mut Harness<'_, RustGisApp>, dx: f32) {
    let (pane, _) = rects(h);
    // Grabbed just inside the pane, as the map's own drag sense is on top at the boundary.
    let edge = egui::pos2(pane.min.x + 2.0, pane.center().y);
    h.hover_at(edge);
    h.step();
    h.drag_at(edge);
    h.step();
    for k in 1..=10 {
        h.hover_at(edge - vec2(dx * k as f32 / 10.0, 0.0));
        h.step();
    }
    h.event(egui::Event::PointerButton {
        pos: edge - vec2(dx, 0.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    for _ in 0..4 {
        h.step();
    }
}

#[test]
fn notebook_opens_on_half_the_room_beside_the_map() {
    let h = window("ui.pane.notebook");
    let (pane, room) = rects(&h);
    assert!((pane.width() - room / 2.0).abs() < 24.0, "the notebook is {} wide in {room} of room", pane.width());
}

#[test]
fn a_right_pane_drags_past_640_and_leaves_the_map_room() {
    let mut h = window("ui.pane.notebook");
    drag_edge(&mut h, 300.0);
    let (pane, room) = rects(&h);
    assert!(pane.width() > 700.0, "the notebook stopped at {} wide", pane.width());
    // Dragged as far as it goes, the map keeps its minimum.
    drag_edge(&mut h, 2000.0);
    let (pane, room_after) = rects(&h);
    assert!((room - room_after).abs() < 0.5);
    assert!(room - pane.width() >= 320.0 - 1.0, "the map was left {} of {room}", room - pane.width());

    // Other panes still open at their own width.
    let mut h = window("ui.pane.history");
    assert!((rects(&h).0.width() - 320.0).abs() < 2.0, "the History pane opened {} wide", rects(&h).0.width());
    drag_edge(&mut h, 600.0);
    let (pane, _) = rects(&h);
    assert!(pane.width() > 700.0, "the History pane stopped at {} wide", pane.width());
}

/// #2416: Messages had a width of its own, so switching between it and Script moved the pane's
/// edge. It shares the width of the other right panes, as dragged.
#[test]
fn messages_and_script_share_the_right_pane_width() {
    let mut h = window("ui.pane.script");
    drag_edge(&mut h, 250.0);
    let script = rects(&h).0.width();
    assert!(script > 500.0, "the Script pane stopped at {script} wide");
    for cmd in ["ui.pane.diagnosticLog", "ui.pane.script"] {
        h.state_mut().run(cmd, Value::Null).expect("show the pane");
        for _ in 0..6 {
            h.step();
        }
        let width = rects(&h).0.width();
        assert!((width - script).abs() < 0.5, "{cmd} showed the pane {width} wide, Script was {script}");
    }
}
