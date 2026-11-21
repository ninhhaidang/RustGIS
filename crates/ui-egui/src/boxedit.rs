//! Constrained box gestures. A drag previews only; release commits one ordinary edit command.
use crate::mapview::{lonlat_to_screen, screen_to_lonlat};
use crate::{RightPane, RustGisApp};
use egui::{Color32, Painter, Pos2, Rect, Stroke, vec2};
use rustgis_engine::box_edit::{BoxSpec, Frame};
use rustgis_engine::data::Feature;
use rustgis_engine::{Tool, geom::Coord};
use serde_json::json;

#[derive(Default)]
pub(crate) struct State {
    gesture: Option<Gesture>,
    preview: Option<BoxSpec>,
    error: Option<String>,
}
struct Gesture {
    project: u64,
    map: usize,
    map_name: String,
    layer: u32,
    instance: u64,
    replacement: u64,
    rows: std::sync::Weak<Vec<Feature>>,
    row: Option<usize>,
    feature: Option<u64>,
    original: BoxSpec,
    start: [f64; 2],
    handle: Handle,
}
#[derive(Clone, Copy)]
enum Handle {
    New,
    Move,
    Rotate,
    Resize(f64, f64),
}

pub(crate) fn ready(app: &RustGisApp) -> bool {
    app.session.active_layer.is_some_and(|id| {
        rustgis_engine::box_edit::check_tool(&app.session, id).is_ok()
            && app.session.map().layer(id).is_some_and(|l| app.session.map().draws_at(l, app.session.scale()))
    })
}
fn selected(app: &RustGisApp) -> Option<(u32, usize, BoxSpec)> {
    let id = app.session.active_layer?;
    let l = app.session.map().layer(id)?;
    let [row] = l.selection.as_slice() else { return None };
    Some((id, *row, app.session.box_spec(id, *row)?))
}
pub(crate) fn drawing(app: &RustGisApp) -> bool {
    app.boxes.gesture.is_some()
}
pub(crate) fn new_drawing(app: &RustGisApp) -> bool {
    app.boxes.gesture.as_ref().is_some_and(|g| g.row.is_none())
}
pub(crate) fn error(app: &RustGisApp) -> Option<&str> {
    app.boxes.error.as_deref()
}
pub(crate) fn clear_idle_error(app: &mut RustGisApp) {
    if app.boxes.gesture.is_none() {
        app.boxes.error = None;
    }
}
pub(crate) fn cancel(app: &mut RustGisApp) {
    app.boxes = State::default();
    app.map.cancel_rotated_box_drag();
}
fn valid(app: &RustGisApp, g: &Gesture) -> bool {
    ready(app)
        && app.session.tool == Tool::RotatedBox
        && g.project == app.session.opened
        && g.map == app.session.project.active_map
        && g.map_name == app.session.map().name
        && app.session.active_layer == Some(g.layer)
        && (g.row.is_some() || crate::annotation::drawing_frame(app).is_ok_and(|frame| frame == g.original.frame))
        && app.session.map().layer(g.layer).is_some_and(|l| {
            l.instance_id() == g.instance
                && l.replacement_revision() == g.replacement
                && g.rows.ptr_eq(&l.dataset.features.allocation_key())
                && g.row.is_none_or(|row| l.dataset.features.get(row).map(|f| f.id) == g.feature)
        })
}
pub(crate) fn cancel_stale(app: &mut RustGisApp) {
    if let Some(grid_changed) = app.boxes.gesture.as_ref().filter(|g| !valid(app, g)).map(|g| {
        g.row.is_none()
            && (g.project != app.session.opened
                || g.map != app.session.project.active_map
                || g.map_name != app.session.map().name
                || !crate::annotation::drawing_frame(app).is_ok_and(|frame| frame == g.original.frame))
    }) {
        if grid_changed {
            crate::annotation::drawing_cancelled(app);
        }
        cancel(app);
    }
    if !app.forms.template.is_empty()
        && !app
            .session
            .active_layer
            .and_then(|id| app.session.map().layer(id))
            .is_some_and(|l| l.design.templates.iter().any(|t| t.name == app.forms.template))
    {
        app.forms.template.clear();
    }
}
fn at(app: &RustGisApp, rect: Rect, frame: &Frame, p: [f64; 2]) -> Option<Pos2> {
    lonlat_to_screen(app, rect, frame.to_lonlat(p)?)
}
fn handles(app: &RustGisApp, rect: Rect, b: &BoxSpec) -> Vec<(Handle, Pos2)> {
    let mut out = Vec::with_capacity(9);
    for (x, y) in [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.), (0., -1.), (1., 0.), (0., 1.), (-1., 0.)] {
        if let Some(p) = at(app, rect, &b.frame, b.point(x * b.width / 2., y * b.height / 2.)) {
            out.push((Handle::Resize(x, y), p));
        }
    }
    if let (Some(center), Some(top)) = (at(app, rect, &b.frame, b.center), at(app, rect, &b.frame, b.point(0., -b.height / 2.))) {
        let delta = top - center;
        if delta.length() > 0.01 {
            out.push((Handle::Rotate, top + delta.normalized() * 30.));
        }
    }
    out
}
fn handle_at(app: &RustGisApp, rect: Rect, p: Pos2, b: &BoxSpec) -> Option<Handle> {
    handles(app, rect, b).into_iter().rev().find(|(_, at)| at.distance(p) <= 8.).map(|(h, _)| h)
}
pub(crate) fn hits(app: &mut RustGisApp, c: Coord) -> Vec<(u32, usize)> {
    let Some(id) = app.session.active_layer else { return Vec::new() };
    let Some(p) = app.session.map().to_map(c) else { return Vec::new() };
    let (tol, scale) = (app.session.map().camera.res * 5., app.session.scale());
    app.session.map_mut().hits_at(p, tol, scale, |l| l.id == id && l.selectable, true, 64)
}
pub(crate) fn click(app: &mut RustGisApp, c: Coord, shift: bool, alt: bool) {
    if !ready(app) {
        return;
    }
    let hits = hits(app, c);
    let current = app.session.active_layer.and_then(|id| {
        let [row] = app.session.map().layer(id)?.selection.as_slice() else { return None };
        Some((id, *row))
    });
    let index = if alt { current.and_then(|c| hits.iter().position(|h| *h == c)).map_or(0, |i| (i + 1) % hits.len().max(1)) } else { 0 };
    if let Some(&(layer, row)) = hits.get(index) {
        let _ = app.run("select.rows", json!({"layer":layer,"rows":[row],"mode":if shift {"add"} else {"new"}}));
    } else if !shift {
        let _ = app.run("select.clear", json!({}));
    }
}
pub(crate) fn drag_start(app: &mut RustGisApp, rect: Rect, origin: Pos2, force_new: bool) -> bool {
    if !ready(app) {
        return false;
    }
    let Some(c) = screen_to_lonlat(app, rect, origin) else { return false };
    let chosen = if force_new {
        None
    } else {
        selected(app)
            .and_then(|(id, row, b)| {
                handle_at(app, rect, origin, &b).or_else(|| contains(&b, c).then_some(Handle::Move)).map(|h| (id, row, b, h))
            })
            .or_else(|| hits(app, c).into_iter().find_map(|(id, row)| app.session.box_spec(id, row).map(|b| (id, row, b, Handle::Move))))
    };
    let Some(id) = app.session.active_layer else { return false };
    let (row, original, handle) = match chosen {
        Some((_, row, b, h)) => (Some(row), b, h),
        None => {
            let frame = match crate::annotation::drawing_frame(app) {
                Ok(frame) => frame,
                Err(error) => {
                    if app.boxes.error.as_deref() != Some(error.as_str()) {
                        app.session.notify(error.clone(), true);
                    }
                    app.boxes.error = Some(error);
                    return false;
                }
            };
            (None, BoxSpec { frame, center: [0., 0.], width: 1., height: 1., angle: 0. }, Handle::New)
        }
    };
    let Some(start) = original.frame.to_local(c) else { return false };
    if let Some(row) = row {
        rustgis_engine::cmd::select::set(&mut app.session, id, vec![row], rustgis_engine::cmd::select::Mode::New);
        app.map.invalidate();
    }
    let Some(l) = app.session.map().layer(id) else { return false };
    app.boxes = State {
        gesture: Some(Gesture {
            project: app.session.opened,
            map: app.session.project.active_map,
            map_name: app.session.map().name.clone(),
            layer: id,
            instance: l.instance_id(),
            replacement: l.replacement_revision(),
            rows: l.dataset.features.allocation_key(),
            row,
            feature: row.and_then(|r| l.dataset.features.get(r).map(|f| f.id)),
            original,
            start,
            handle,
        }),
        preview: None,
        error: None,
    };
    true
}
fn moved(b: &BoxSpec, start: [f64; 2], p: [f64; 2], h: Handle, shift: bool) -> BoxSpec {
    let mut next = b.clone();
    match h {
        Handle::New => {
            let (mut dx, mut dy) = (p[0] - start[0], p[1] - start[1]);
            if shift {
                let side = dx.abs().max(dy.abs());
                dx = side * dx.signum();
                dy = side * dy.signum();
            }
            next.center = [start[0] + dx / 2., start[1] + dy / 2.];
            next.width = dx.abs();
            next.height = dy.abs();
        }
        Handle::Move => next.center = [b.center[0] + p[0] - start[0], b.center[1] + p[1] - start[1]],
        Handle::Rotate => {
            let angle = |p: [f64; 2]| (p[1] - b.center[1]).atan2(p[0] - b.center[0]);
            next.angle = (b.angle + (angle(p) - angle(start)).to_degrees()).rem_euclid(360.);
            if shift {
                next.angle = ((next.angle / 15.).round() * 15.).rem_euclid(360.);
            }
        }
        Handle::Resize(x, y) => {
            let (u, v) = b.axes();
            let opposite = b.point(-x * b.width / 2., -y * b.height / 2.);
            let d = [p[0] - opposite[0], p[1] - opposite[1]];
            let (mut du, mut dv) = (d[0] * u[0] + d[1] * u[1], d[0] * v[0] + d[1] * v[1]);
            if shift && x != 0. && y != 0. {
                let ratio = b.width / b.height;
                if du.abs() / ratio > dv.abs() {
                    dv = du.abs() / ratio * dv.signum();
                } else {
                    du = dv.abs() * ratio * du.signum();
                }
            }
            if x != 0. {
                next.width = du.abs();
            } else {
                du = 0.;
            }
            if y != 0. {
                next.height = dv.abs();
            } else {
                dv = 0.;
            }
            next.center = [opposite[0] + (du * u[0] + dv * v[0]) / 2., opposite[1] + (du * u[1] + dv * v[1]) / 2.];
        }
    }
    next
}
pub(crate) fn drag_to(app: &mut RustGisApp, rect: Rect, p: Pos2, shift: bool) {
    cancel_stale(app);
    let Some(g) = app.boxes.gesture.as_ref() else { return };
    let Some(p) = screen_to_lonlat(app, rect, p).and_then(|c| g.original.frame.to_local(c)) else { return };
    let b = moved(&g.original, g.start, p, g.handle, shift);
    app.boxes.error = b.geometry().err();
    app.boxes.preview = Some(b);
}
pub(crate) fn drag_stop(app: &mut RustGisApp) {
    cancel_stale(app);
    let State { gesture, preview, error } = std::mem::take(&mut app.boxes);
    let (Some(g), Some(b)) = (gesture, preview) else { return };
    if let Some(e) = error {
        app.session.notify(e, true);
        return;
    }
    if let Some(row) = g.row {
        if b != g.original {
            let _ = app.run("edit.updateBox", json!({"layer":g.layer,"row":row,"featureId":g.feature,"box":b}));
        }
    } else {
        let mut p = json!({"layer":g.layer,"box":b});
        if app.session.map().layer(g.layer).is_some_and(|l| l.design.templates.iter().any(|t| t.name == app.forms.template)) {
            p["template"] = json!(app.forms.template);
        }
        let _ = app.run("edit.addBox", p);
    }
}
pub(crate) fn cursor(app: &RustGisApp, rect: Rect, p: Pos2) -> Option<egui::CursorIcon> {
    let (_, _, b) = selected(app)?;
    if handle_at(app, rect, p, &b).is_some() {
        return Some(egui::CursorIcon::Crosshair);
    }
    contains(&b, screen_to_lonlat(app, rect, p)?).then_some(egui::CursorIcon::Grab)
}
fn contains(b: &BoxSpec, c: Coord) -> bool {
    let Some(q) = b.frame.to_local(c) else { return false };
    let d = [q[0] - b.center[0], q[1] - b.center[1]];
    let (u, v) = b.axes();
    (d[0] * u[0] + d[1] * u[1]).abs() <= b.width / 2. && (d[0] * v[0] + d[1] * v[1]).abs() <= b.height / 2.
}
pub(crate) fn draw(app: &RustGisApp, painter: &Painter, rect: Rect, accent: Color32) {
    if !ready(app) {
        return;
    }
    let b = app.boxes.preview.clone().or_else(|| selected(app).map(|(_, _, b)| b));
    let Some(b) = b else { return };
    let points: Option<Vec<Pos2>> = b.corners().into_iter().map(|p| at(app, rect, &b.frame, p)).collect();
    let Some(points) = points else { return };
    let color = if app.boxes.error.is_some() { Color32::RED } else { accent };
    painter.add(egui::Shape::convex_polygon(points, color.gamma_multiply(0.08), Stroke::new(1.5, color)));
    for (h, p) in handles(app, rect, &b) {
        if matches!(h, Handle::Rotate) {
            if let Some(top) = at(app, rect, &b.frame, b.point(0., -b.height / 2.)) {
                painter.line_segment([top, p], Stroke::new(1.5, color));
            }
            painter.circle_filled(p, 6., color);
            painter.circle_stroke(p, 6., Stroke::new(1., Color32::WHITE));
        } else {
            painter.rect_filled(Rect::from_center_size(p, vec2(7., 7.)), 1., Color32::WHITE);
            painter.rect_stroke(Rect::from_center_size(p, vec2(7., 7.)), 1., Stroke::new(1.5, color), egui::StrokeKind::Outside);
        }
    }
}
pub(crate) fn panel(app: &mut RustGisApp, ui: &mut egui::Ui) {
    if app.session.tool != Tool::RotatedBox {
        return;
    }
    ui.separator();
    ui.strong(tl!("Rotated Box"));
    ui.small(tl!("Drag to draw. Drag the paddle to rotate. Shift snaps angles; Ctrl/Cmd+drag draws over existing boxes. Space or middle-drag pans; releasing Space keeps this tool."));
    if !ready(app) {
        ui.label(tl!("Turn editing on and choose a loaded polygon layer in a flat Earth map."));
        return;
    }
    if let Some((layer, row, mut b)) = selected(app) {
        let feature = app.session.map().layer(layer).and_then(|l| l.dataset.features.get(row)).map(|f| f.id);
        let name = app.session.map().layer(layer).map(|l| l.name.clone()).unwrap_or_default();
        let shown = feature.map(|id| id.to_string()).unwrap_or_default();
        ui.small(crate::i18n::fmt("Layer: {layer} · Feature {feature}", &[("layer", &name), ("feature", &shown)]));
        ui.label(if b.frame == Frame::default() {
            tl!("Web Mercator grid metres, not ground metres.")
        } else {
            tl!("Grid pixels; angle clockwise from grid right.")
        });
        let mut changed = false;
        let [cx, cy] = &mut b.center;
        egui::Grid::new("rotated_box_dimensions").show(ui, |ui| {
            for (name, v) in [
                (tl!("Center X"), cx),
                (tl!("Center Y"), cy),
                (tl!("Width"), &mut b.width),
                (tl!("Height"), &mut b.height),
                (tl!("Angle (°)"), &mut b.angle),
            ] {
                let label = ui.label(name);
                let r = ui.add(egui::DragValue::new(v).update_while_editing(false).speed(0.1)).labelled_by(label.id);
                if crate::widgets::changed(&r) {
                    crate::widgets::one_step_per_press(app, &r);
                    changed = true;
                }
                ui.end_row();
            }
        });
        if changed {
            let _ = app.run("edit.updateBox", json!({"layer":layer,"row":row,"featureId":feature,"box":b}));
        }
        if ui.button(tl!("Delete Selected Box")).clicked() {
            let _ = app.run("edit.deleteBox", json!({"layer":layer,"row":row,"featureId":feature}));
        }
    } else {
        ui.label(tl!("Drag a new box or click one created in this session. Other polygons use Edit Vertices."));
    }
    ui.small(tl!("Saved and exported boxes are ordinary polygons. Box construction is kept only in this session."));
    if let Some(e) = &app.boxes.error {
        ui.colored_label(Color32::RED, e);
    }
}
pub(crate) fn context_menu(app: &mut RustGisApp, ui: &mut egui::Ui) {
    crate::shapeconvert::menu(app, ui);
    if selected(app).is_some() {
        if ui.button(tl!("Edit Box Dimensions")).clicked() {
            app.ui.open_right(RightPane::Editing);
            ui.close();
        }
        if ui.button(tl!("Delete Selected Box")).clicked() {
            if let Some((id, row, _)) = selected(app) {
                let _ = app.run("edit.deleteBox", json!({"layer":id,"row":row}));
            }
            ui.close();
        }
    } else {
        ui.label(tl!("Select a box created in this session."));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::{
        Project, Session,
        data::{Dataset, GeometryType},
    };
    fn app() -> RustGisApp {
        let mut s = Session::new(Project::default());
        let id = s.map_mut().add_dataset(Dataset::new("Boxes", GeometryType::Polygon, vec![]));
        s.active_layer = Some(id);
        s.map_mut().camera.res = 1000.;
        s.map_mut().camera.center = Coord::new(0., 0.);
        s.run("edit.enable", &json!({"on":true})).unwrap();
        s.run("tool.set", &json!({"tool":"rotatedBox"})).unwrap();
        RustGisApp::new(s)
    }
    fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        egui_kittest::Harness::builder().with_size(vec2(1000., 800.)).with_max_steps(1000).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        )
    }
    fn rect() -> Rect {
        Rect::from_min_size(Pos2::ZERO, vec2(1000., 800.))
    }
    fn create(app: &mut RustGisApp) -> (u32, usize, BoxSpec) {
        assert!(drag_start(app, rect(), Pos2::new(400., 350.), false));
        drag_to(app, rect(), Pos2::new(600., 450.), false);
        drag_stop(app);
        selected(app).unwrap()
    }
    #[test]
    fn box_edit_drag_previews_without_mutation_and_commits_one_undo_step() {
        let mut app = app();
        let id = app.session.active_layer.unwrap();
        let depth = app.session.undo_depth();
        assert!(drag_start(&mut app, rect(), Pos2::new(400., 350.), false));
        for x in [420., 450., 500., 600.] {
            drag_to(&mut app, rect(), Pos2::new(x, 450.), false);
        }
        assert_eq!(app.session.map().layer(id).unwrap().dataset.features.len(), 0);
        assert_eq!(app.session.undo_depth(), depth);
        drag_stop(&mut app);
        assert_eq!(app.session.map().layer(id).unwrap().dataset.features.len(), 1);
        assert_eq!(app.session.undo_depth(), depth + 1);
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.map().layer(id).unwrap().dataset.features.len(), 0);
        app.run("edit.redo", json!({})).unwrap();
        assert!(app.session.box_spec(id, 0).is_some());
    }
    #[test]
    fn box_edit_paddle_snap_resize_and_move_preserve_the_frozen_rectangle() {
        let mut app = app();
        let (id, row, original) = create(&mut app);
        let depth = app.session.undo_depth();
        let start = handles(&app, rect(), &original).into_iter().find(|(h, _)| matches!(h, Handle::Rotate)).unwrap().1;
        assert!(drag_start(&mut app, rect(), start, false));
        let center = at(&app, rect(), &original.frame, original.center).unwrap();
        let v = start - center;
        let angle = 37_f32.to_radians();
        let end = center + vec2(v.x * angle.cos() - v.y * angle.sin(), v.x * angle.sin() + v.y * angle.cos());
        drag_to(&mut app, rect(), end, true);
        drag_stop(&mut app);
        let rotated = app.session.box_spec(id, row).unwrap();
        assert!((rotated.angle / 15. - (rotated.angle / 15.).round()).abs() < 1e-9);
        assert_eq!((rotated.width, rotated.height), (original.width, original.height));
        assert_eq!(app.session.undo_depth(), depth + 1);
        let corner = at(&app, rect(), &rotated.frame, rotated.point(rotated.width / 2., rotated.height / 2.)).unwrap();
        let opposite = rotated.point(-rotated.width / 2., -rotated.height / 2.);
        assert!(drag_start(&mut app, rect(), corner, false));
        drag_to(&mut app, rect(), corner + vec2(25., 30.), false);
        drag_stop(&mut app);
        let resized = app.session.box_spec(id, row).unwrap();
        let fixed = resized.point(-resized.width / 2., -resized.height / 2.);
        assert!((fixed[0] - opposite[0]).abs() < 1e-6 && (fixed[1] - opposite[1]).abs() < 1e-6);
        assert_eq!(resized.angle, rotated.angle);
        let center = at(&app, rect(), &resized.frame, resized.center).unwrap();
        assert!(drag_start(&mut app, rect(), center, false));
        drag_to(&mut app, rect(), center + vec2(20., 20.), false);
        drag_stop(&mut app);
        let moved = app.session.box_spec(id, row).unwrap();
        assert_eq!((moved.width, moved.height, moved.angle), (resized.width, resized.height, resized.angle));
        assert_ne!(moved.center, resized.center);
    }
    #[test]
    fn box_edit_snapping_back_to_zero_does_not_add_an_undo_step() {
        let mut app = app();
        let (id, row, b) = create(&mut app);
        assert_eq!(b.angle, 0.);
        let depth = app.session.undo_depth();
        let paddle = handles(&app, rect(), &b).into_iter().find(|(h, _)| matches!(h, Handle::Rotate)).unwrap().1;
        assert!(drag_start(&mut app, rect(), paddle, false));
        drag_to(&mut app, rect(), paddle - vec2(1., 0.), true);
        assert_eq!(app.boxes.preview.as_ref().unwrap().angle, 0.);
        drag_stop(&mut app);
        assert_eq!(app.session.box_spec(id, row), Some(b));
        assert_eq!(app.session.undo_depth(), depth);
    }

    #[test]
    fn box_edit_replaced_rows_or_project_cancel_a_held_gesture() {
        let mut app = app();
        let (id, row, b) = create(&mut app);
        let center = at(&app, rect(), &b.frame, b.center).unwrap();
        assert!(drag_start(&mut app, rect(), center, false));
        drag_to(&mut app, rect(), center + vec2(50., 20.), false);
        app.session.map_mut().layer_mut(id).unwrap().dataset.features[row].id += 1;
        drag_stop(&mut app);
        assert!(app.session.box_spec(id, row).is_none());
        let depth = app.session.undo_depth();
        assert!(drag_start(&mut app, rect(), Pos2::new(100., 100.), true));
        drag_to(&mut app, rect(), Pos2::new(200., 200.), false);
        app.session.opened = app.session.opened.wrapping_add(1);
        drag_stop(&mut app);
        assert_eq!(app.session.undo_depth(), depth);
    }
    #[test]
    fn box_edit_cancel_and_tool_switch_never_commit_a_preview() {
        let mut app = app();
        let id = app.session.active_layer.unwrap();
        assert!(drag_start(&mut app, rect(), Pos2::new(100., 100.), true));
        drag_to(&mut app, rect(), Pos2::new(200., 200.), false);
        cancel(&mut app);
        drag_stop(&mut app);
        assert_eq!(app.session.tool, Tool::RotatedBox);
        assert!(drag_start(&mut app, rect(), Pos2::new(100., 100.), true));
        drag_to(&mut app, rect(), Pos2::new(200., 200.), false);
        app.run("tool.set", json!({"tool":"explore"})).unwrap();
        drag_stop(&mut app);
        assert!(app.session.map().layer(id).unwrap().dataset.features.is_empty());
    }
    #[test]
    fn box_edit_escape_cancels_before_leaving_the_tool() {
        let mut h = harness(app());
        for _ in 0..6 {
            h.step();
        }
        let view = h.state().map.view_rect.unwrap().0;
        let id = h.state().session.active_layer.unwrap();
        let start = view.center() - vec2(100., 100.);
        assert!(drag_start(h.state_mut(), view, start, true));
        drag_to(h.state_mut(), view, start + vec2(100., 100.), false);
        let key = |pressed| egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        h.input_mut().events.push(key(true));
        h.step();
        assert!(!drawing(h.state()));
        assert_eq!(h.state().session.tool, Tool::RotatedBox);
        assert!(h.state().session.map().layer(id).unwrap().dataset.features.is_empty());
        h.input_mut().events.push(key(false));
        h.step();
        h.input_mut().events.push(key(true));
        h.step();
        assert_eq!(h.state().session.tool, Tool::Explore);
    }

    #[test]
    fn box_edit_alt_click_cycles_overlap_and_forced_draw_keeps_existing_boxes() {
        let mut app = app();
        let (id, _, b) = create(&mut app);
        app.run("edit.addBox", json!({"box":b})).unwrap();
        let c = b.frame.to_lonlat(b.center).unwrap();
        click(&mut app, c, false, false);
        let one = selected(&app).unwrap().1;
        click(&mut app, c, false, true);
        let two = selected(&app).unwrap().1;
        assert_ne!(one, two);
        let center = at(&app, rect(), &b.frame, b.center).unwrap();
        assert!(drag_start(&mut app, rect(), center, true));
        drag_to(&mut app, rect(), center + vec2(50., 50.), false);
        drag_stop(&mut app);
        assert_eq!(app.session.map().layer(id).unwrap().dataset.features.len(), 3);
    }
    #[test]
    fn box_edit_cycles_past_ordinary_polygons_and_moves_the_selected_overlap() {
        let mut app = app();
        let (id, first, b) = create(&mut app);
        app.run("edit.addBox", json!({"box":b})).unwrap();
        app.run("edit.addFeature", json!({"geometry":rustgis_engine::data::io::geometry_to_geojson(&b.geometry().unwrap())})).unwrap();
        let c = b.frame.to_lonlat(b.center).unwrap();
        click(&mut app, c, false, false);
        assert_eq!(app.session.map().layer(id).unwrap().selection, vec![2]);
        assert!(selected(&app).is_none());
        click(&mut app, c, false, true);
        assert_eq!(selected(&app).unwrap().1, 1);
        click(&mut app, c, false, true);
        assert_eq!(selected(&app).unwrap().1, first);
        let p = at(&app, rect(), &b.frame, b.center).unwrap();
        assert!(drag_start(&mut app, rect(), p, false));
        drag_to(&mut app, rect(), p + vec2(20., 10.), false);
        drag_stop(&mut app);
        assert_eq!(selected(&app).unwrap().1, first);
        assert_ne!(app.session.box_spec(id, first).unwrap().center, b.center);
        assert_eq!(app.session.box_spec(id, 1), Some(b));
    }

    #[test]
    fn box_edit_ignores_another_layers_template_and_keeps_valid_defaults() {
        let mut app = app();
        let target = app.session.map_mut().add_dataset(Dataset::new(
            "Target",
            GeometryType::Polygon,
            vec![rustgis_engine::data::Field::new("class", rustgis_engine::data::FieldType::Text)],
        ));
        app.forms.template = "Previous layer template".into();
        app.session.active_layer = Some(target);
        cancel_stale(&mut app);
        assert!(app.forms.template.is_empty());
        assert!(drag_start(&mut app, rect(), Pos2::new(400., 350.), false));
        drag_to(&mut app, rect(), Pos2::new(600., 450.), false);
        app.forms.template = "Previous layer template".into();
        drag_stop(&mut app);
        assert_eq!(app.session.map().layer(target).unwrap().dataset.features.len(), 1);
        app.session.map_mut().layer_mut(target).unwrap().design.templates.push(rustgis_engine::map::design::FeatureTemplate {
            name: "Vehicle".into(),
            description: String::new(),
            defaults: vec![("class".into(), rustgis_engine::data::Value::Text("vehicle".into()))],
        });
        app.forms.template = "Vehicle".into();
        assert!(drag_start(&mut app, rect(), Pos2::new(400., 350.), true));
        drag_to(&mut app, rect(), Pos2::new(600., 450.), false);
        drag_stop(&mut app);
        assert_eq!(app.session.map().layer(target).unwrap().dataset.value(1, 0), &rustgis_engine::data::Value::Text("vehicle".into()));
    }

    #[test]
    fn box_edit_context_menu_wins_over_a_stale_ordinary_map_menu() {
        use egui_kittest::kittest::Queryable;
        let mut app = app();
        let (id, row, b) = create(&mut app);
        app.map.menu_lonlat = Some(b.frame.to_lonlat(b.center).unwrap());
        let mut h = harness(app);
        for _ in 0..6 {
            h.step();
        }
        let view = h.state().map.view_rect.unwrap().0;
        let p = at(h.state(), view, &b.frame, b.center).unwrap();
        h.input_mut().events.push(egui::Event::PointerMoved(p));
        for pressed in [true, false] {
            h.input_mut().events.push(egui::Event::PointerButton {
                pos: p,
                button: egui::PointerButton::Secondary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
            h.step();
        }
        h.step();
        assert!(h.query_by_label("Delete Selected Box").is_some());
        assert!(h.query_by_label("Edit Box Dimensions").is_some());
        assert_eq!(h.state().session.map().layer(id).unwrap().selection, vec![row]);
    }

    #[test]
    fn box_edit_space_pan_returns_to_editing_without_an_identify_popup_or_new_feature() {
        let mut app = app();
        let (id, row, _) = create(&mut app);
        let mut h = harness(app);
        for _ in 0..6 {
            h.step();
        }
        let view = h.state().map.view_rect.unwrap().0;
        let center = view.center();
        let camera = h.state().session.map().camera.center;
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.input_mut().events.push(egui::Event::PointerMoved(center));
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: center,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        h.input_mut().events.push(egui::Event::PointerMoved(center + vec2(50., 30.)));
        h.step();
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: center + vec2(50., 30.),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::Space,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        assert_ne!(h.state().session.map().camera.center, camera);
        assert_eq!(h.state().session.tool, Tool::RotatedBox);
        let b = h.state().session.box_spec(id, row).unwrap();
        let view = h.state().map.view_rect.unwrap().0;
        let p = at(h.state(), view, &b.frame, b.center).unwrap();
        h.input_mut().events.push(egui::Event::PointerMoved(p));
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        h.input_mut().events.push(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
        assert_eq!(h.state().session.tool, Tool::RotatedBox);
        assert_eq!(h.state().session.map().layer(id).unwrap().selection, vec![row]);
        assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features.len(), 1);
        assert!(h.state().map.popup.is_none());
    }
}
