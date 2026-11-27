//! Pointer and keyboard interaction with the page.

use super::*;
use crate::RustGisApp;
use egui::{CursorIcon, Pos2, Rect, Ui};
use rustgis_engine::map::layout::{ElementKind, Layout, MAX_FRAME_SCALE, MIN_FRAME_SCALE, PT_PER_MM, frame_camera};
use serde_json::json;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Drag {
    /// Moving an element: its id and the pointer's page position (mm) at the start.
    Move(u32, (f64, f64)),
    /// Resizing from a handle (0..8 clockwise from top-left).
    Resize(u32, usize, (f64, f64)),
    /// Panning a map frame's map.
    PanFrame(u32, (f64, f64)),
    /// Panning the page view.
    Pan,
    /// Drawing a lasso frame.
    Lasso,
    /// Dragging a new guide out of a ruler: vertical (from the left ruler) or horizontal.
    NewGuide(bool),
    /// Moving a guide: vertical or horizontal, and its index.
    Guide(bool, usize),
    /// Turning a line or arrow by its rotation handle: its id, its angle at the start, and the
    /// pointer's angle about its centre then (degrees).
    Rotate(u32, f64, f64),
}

/// The map frame under a screen point, when map frames are being navigated.
pub(super) fn nav_frame(app: &RustGisApp, layout: &Layout, page: Rect, s: f32, p: Pos2) -> Option<u32> {
    let (x, y) = to_mm(page, s, p);
    layout.hit(x, y).filter(|id| {
        app.layout.navigate && layout.element(*id).is_some_and(|e| !e.locked && matches!(e.kind, ElementKind::MapFrame { .. }))
    })
}

/// Whether the element is locked (#536): selected, but neither moved nor resized.
pub(super) fn locked(layout: &Layout, id: u32) -> bool {
    layout.element(id).is_some_and(|e| e.locked)
}

/// What the pointer does on the page `resp` covers: the wheel zooms the page or a map frame's map,
/// a click or right-click selects the element under it, a drag moves, resizes, rotates or pans
/// one (or drags a guide out of a ruler), and a tool picked from the ribbon draws a frame.
pub(super) fn interact(app: &mut RustGisApp, ui: &mut Ui, resp: &egui::Response, layout: &Layout, li: usize, page: Rect, s: f32) {
    // A map frame's wheel zoom is applied once the wheel has stopped.
    let now = ui.input(|i| i.time);
    // A pan ending this frame is worked out from `layout` as it was: applying the zoom first
    // would have the pan overwrite its centre. It waits a frame, for the layout after the pan.
    let pan_ends = resp.drag_stopped() && matches!(app.layout.drag, Some(Drag::PanFrame(..)));
    if let Some((name, id, k, anchor, last)) = app.layout.frame_zoom.clone() {
        if name != layout.name {
            // Another layout is open now: the zoom was for a frame of that one.
            app.layout.frame_zoom = None;
        } else if now - last >= FRAME_ZOOM_SETTLE && !pan_ends {
            app.layout.frame_zoom = None;
            zoom_frame(app, layout, li, id, k, anchor);
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(FRAME_ZOOM_SETTLE));
        }
    }
    // Wheel zooms the page about the pointer, or a map frame's map when navigating.
    if let Some(p) = resp.hover_pos() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y + i.zoom_delta().ln() * 300.0);
        if scroll.abs() > 0.1
            && let Some(id) = nav_frame(app, layout, page, s, p)
        {
            // Wheel up zooms in: a smaller scale denominator.
            let step = f64::from(-scroll * 0.0028).exp();
            let same = app.layout.frame_zoom.as_ref().filter(|z| z.1 == id && z.0 == layout.name).map(|z| z.2);
            let k = match same {
                Some(k) => k * step,
                None => {
                    // Another frame's zoom still waiting is applied, not lost, before this one's.
                    if let Some((name, zid, zk, anchor, _)) = app.layout.frame_zoom.take()
                        && name == layout.name
                    {
                        zoom_frame(app, layout, li, zid, zk, anchor);
                    }
                    step
                }
            };
            app.layout.frame_zoom = Some((layout.name.clone(), id, k.clamp(1e-3, 1e3), to_mm(page, s, p), now));
        } else if scroll.abs() > 0.1 {
            let old = if app.layout.zoom > 0.0 { app.layout.zoom } else { 1.0 };
            let new = (old * (scroll * 0.0028).exp()).clamp(0.2, 12.0);
            // Keep the page point under the pointer fixed.
            let centre = resp.rect.center() + app.layout.pan;
            app.layout.pan += (p - centre) * (1.0 - new / old);
            app.layout.zoom = new;
        }
    }
    // The page moves under a drag when the properties open beside it (the drag selects the
    // element): keep the drag's start under the same screen point, so the element follows the
    // pointer rather than the page.
    if let Some((old, old_s)) = app.layout.drag_page
        && (old, old_s) != (page, s)
    {
        let rebase = |p| to_mm(page, s, to_screen(old, old_s, p));
        app.layout.drag = match app.layout.drag {
            Some(Drag::Move(id, p)) => Some(Drag::Move(id, rebase(p))),
            Some(Drag::Resize(id, h, p)) => Some(Drag::Resize(id, h, rebase(p))),
            Some(Drag::PanFrame(id, p)) => Some(Drag::PanFrame(id, rebase(p))),
            d => d,
        };
    }
    app.layout.drag_page = Some((page, s));
    let pointer_mm = resp.interact_pointer_pos().map(|p| to_mm(page, s, p));
    // Drawing a polygon or lasso frame.
    if let Some(tool) = app.layout.tool {
        if tool == "polygon" {
            if resp.double_clicked() {
                finish_frame(app, li, "polygon");
            } else if resp.clicked()
                && let Some(p) = pointer_mm
            {
                app.layout.sketch.push(p);
            }
        } else {
            if resp.drag_started() {
                app.layout.sketch.clear();
                app.layout.drag = Some(Drag::Lasso);
            }
            if app.layout.drag == Some(Drag::Lasso)
                && let Some(p) = pointer_mm
                && app.layout.sketch.last().is_none_or(|l| ((l.0 - p.0).powi(2) + (l.1 - p.1).powi(2)).sqrt() > 1.0)
            {
                app.layout.sketch.push(p);
            }
            if resp.drag_stopped() {
                app.layout.drag = None;
                finish_frame(app, li, "lasso");
            }
        }
        return;
    }
    if resp.is_pointer_button_down_on()
        && let Some(o) = ui.input(|i| i.pointer.press_origin())
    {
        app.layout.pressed = Some(o);
    }
    // Rulers and guides show they can be dragged.
    if let Some(p) = resp.hover_pos() {
        let vertical = match app.layout.drag {
            Some(Drag::NewGuide(v) | Drag::Guide(v, _)) => Some(v),
            Some(_) => None,
            None => ruler_at(&app.layout, resp.rect, p).or_else(|| guide_at(layout, page, s, p).map(|g| g.0)),
        };
        // A text's handle over the frame comes first (`handle_at`).
        let navigating =
            app.layout.drag.is_none() && nav_frame(app, layout, page, s, p).is_some() && handle_at(app, layout, page, s, p).is_none();
        let handle = match app.layout.drag {
            Some(Drag::Resize(_, h, _)) => Some(h),
            None if !navigating => handle_at(app, layout, page, s, p).map(|(_, h)| h),
            _ => None,
        };
        let rotating = matches!(app.layout.drag, Some(Drag::Rotate(..)));
        let knob = app.layout.drag.is_none() && rotate_at(app, layout, page, s, p).is_some();
        match (vertical, handle) {
            _ if rotating => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
            _ if knob => ui.ctx().set_cursor_icon(CursorIcon::Grab),
            (_, Some(h)) => {
                let rot = app.layout.selected.and_then(|id| layout.element(id)).map_or(0.0, |e| e.rotation());
                ui.ctx().set_cursor_icon(handle_cursor(h, rot))
            }
            (Some(true), _) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
            (Some(false), _) => ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical),
            _ if matches!(app.layout.drag, Some(Drag::PanFrame(..))) => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
            _ if navigating => ui.ctx().set_cursor_icon(CursorIcon::Grab),
            _ if matches!(app.layout.drag, Some(Drag::Move(..))) => ui.ctx().set_cursor_icon(CursorIcon::Move),
            _ if app.layout.drag.is_none() && app.layout.tool.is_none() && {
                let (x, y) = to_mm(page, s, p);
                layout.hit(x, y).is_some_and(|id| !locked(layout, id))
            } =>
            {
                ui.ctx().set_cursor_icon(CursorIcon::Move)
            }
            _ => {}
        }
    }
    // A click on a ruler, or on the selected element's handle (which can stick out over another
    // element), keeps the selection: where the click started, as the pointer may have moved a
    // little by the release. A press serves one click or drag, never a later one.
    let pressed = if resp.clicked() || resp.secondary_clicked() || resp.drag_started() { app.layout.pressed.take() } else { None };
    if resp.clicked()
        && let Some(p) = pointer_mm
        && pressed.or(resp.interact_pointer_pos()).is_none_or(|pos| {
            ruler_at(&app.layout, resp.rect, pos).is_none()
                && handle_at(app, layout, page, s, pos).is_none()
                && rotate_at(app, layout, page, s, pos).is_none()
        })
    {
        app.layout.selected = layout.hit(p.0, p.1);
        // A click on the element already selected brings back the pane closed over it.
        if app.layout.selected.is_some_and(|id| app.layout.formatted == Some((li, id))) {
            app.ui.open_right(crate::RightPane::Format);
        }
    }
    // A right-click selects what is under it, unless that is selected already, and so opens the
    // element's menu (see `show`); on empty page it clears the selection, as a click does. Pressed
    // on a ruler, or on the selected element's handle or rotation knob, it keeps the selection, as
    // a click does there.
    if resp.secondary_clicked()
        && let Some(p) = pointer_mm
        && pressed.or(resp.interact_pointer_pos()).is_none_or(|pos| {
            ruler_at(&app.layout, resp.rect, pos).is_none()
                && handle_at(app, layout, page, s, pos).is_none()
                && rotate_at(app, layout, page, s, pos).is_none()
        })
    {
        let hit = layout.hit(p.0, p.1);
        if hit != app.layout.selected {
            app.layout.selected = hit;
        }
    }
    if resp.double_clicked()
        && let Some(id) = app.layout.selected
        && matches!(layout.element(id).map(|e| &e.kind), Some(ElementKind::MapFrame { .. }))
    {
        // Double-clicking a map frame zooms it to the map view.
        let _ = app.run("layout.frameZoom", json!({"layout": li, "element": id, "to": "view"}));
    }
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        // What was pressed decides the drag, and the drag runs from where it was pressed: egui
        // starts one only once the pointer has moved, by then often off a small handle (which
        // then moved the element instead of resizing it).
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
        let p = to_mm(page, s, origin);
        // Navigating, a map frame pans as Shift-dragging one does; its handles stay put.
        let shift = ui.input(|i| i.modifiers.shift) || app.layout.navigate;
        let handle = handle_at(app, layout, page, s, origin);
        let ruler = ruler_at(&app.layout, resp.rect, origin);
        let guide = guide_at(layout, page, s, origin);
        // A line's rotation knob, outside its box, comes before anything under it.
        let rotate = ruler.is_none().then(|| rotate_at(app, layout, page, s, origin)).flatten();
        app.layout.drag = if let Some((id, c)) = rotate {
            element_angle(layout, id).map(|a| Drag::Rotate(id, a, screen_angle(c, origin)))
        } else {
            match (ruler, handle, guide, layout.hit(p.0, p.1)) {
                (Some(vertical), ..) => Some(Drag::NewGuide(vertical)),
                (None, Some((id, h)), ..) => Some(Drag::Resize(id, h, p)),
                (None, None, Some((vertical, i)), _) => Some(Drag::Guide(vertical, i)),
                // A locked element is selected and stays put, its map too.
                (None, None, None, Some(id)) if locked(layout, id) => {
                    app.layout.selected = Some(id);
                    None
                }
                (None, None, None, Some(id))
                    if shift && matches!(layout.element(id).map(|e| &e.kind), Some(ElementKind::MapFrame { .. })) =>
                {
                    app.layout.selected = Some(id);
                    Some(Drag::PanFrame(id, p))
                }
                (None, None, None, Some(id)) => {
                    app.layout.selected = Some(id);
                    Some(Drag::Move(id, p))
                }
                (None, None, None, None) => Some(Drag::Pan),
            }
        };
    }
    if app.layout.drag == Some(Drag::Pan) {
        app.layout.pan += resp.drag_delta();
    }
    if resp.drag_stopped() {
        let end = pointer_mm.or_else(|| ui.input(|i| i.pointer.latest_pos()).map(|p| to_mm(page, s, p)));
        match (app.layout.drag, end) {
            (Some(Drag::Move(_, start)), Some(end)) => {
                // A press that hardly moved is a click, not a move.
                if ((end.0 - start.0).abs() > 0.2 || (end.1 - start.1).abs() > 0.2)
                    && let Some((id, (x, y, _, _), _, _)) = drag_landing(&app.layout, ui, layout, s, end)
                {
                    let _ = app.run("layout.update", json!({"layout": li, "element": id, "x": x, "y": y}));
                }
            }
            (Some(Drag::Resize(..)), Some(end)) => {
                if let Some((id, (x, y, w, hh), _, _)) = drag_landing(&app.layout, ui, layout, s, end) {
                    let _ = app.run("layout.update", json!({"layout": li, "element": id, "x": x, "y": y, "w": w, "h": hh}));
                }
            }
            (Some(Drag::NewGuide(v)), _) => {
                let at = ui.input(|i| i.pointer.latest_pos());
                drop_guide(app, layout, li, page, s, resp.rect, v, None, at);
            }
            (Some(Drag::Guide(v, i)), _) => {
                let at = ui.input(|i| i.pointer.latest_pos());
                drop_guide(app, layout, li, page, s, resp.rect, v, Some(i), at);
            }
            (Some(Drag::PanFrame(id, start)), Some(end)) => pan_frame(app, layout, li, id, end.0 - start.0, end.1 - start.1),
            (Some(Drag::Rotate(id, a0, from)), _) => {
                if let (Some(at), Some(e)) = (ui.input(|i| i.pointer.latest_pos()), layout.element(id)) {
                    let c = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h))).center();
                    let a = turned(a0, from, screen_angle(c, at), ui.input(|i| i.modifiers.shift));
                    if (a - a0).abs() > 0.05 {
                        let _ = app.run("layout.update", json!({"layout": li, "element": id, "angle": a}));
                    }
                }
            }
            _ => {}
        }
        app.layout.drag = None;
    }
}

/// Moves a map frame's map by a page offset (mm).
pub(super) fn pan_frame(app: &mut RustGisApp, layout: &Layout, li: usize, id: u32, dx: f64, dy: f64) {
    let Some(e) = layout.element(id) else { return };
    let ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } = &e.kind else { return };
    let Some(m) = app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)) else { return };
    let mut cam = frame_camera(m, *center, *scale, *bearing, *pitch);
    // Dragged at the frame's centre, so a turned or tilted frame's map follows the pointer.
    let (w, h) = (e.w * PT_PER_MM, e.h * PT_PER_MM);
    cam.drag_px((w / 2.0, h / 2.0), (w / 2.0 + dx * PT_PER_MM, h / 2.0 + dy * PT_PER_MM), w, h);
    if let Some(ll) = m.to_lonlat(cam.center) {
        let _ = app.run("layout.update", json!({"layout": li, "element": id, "center": [ll.x, ll.y]}));
    }
}

/// Zooms a map frame's map by `k` on its scale (below 1 zooms in), keeping the map under the
/// page point `anchor` (mm) where it is.
pub(super) fn zoom_frame(app: &mut RustGisApp, layout: &Layout, li: usize, id: u32, k: f64, (ax, ay): (f64, f64)) {
    let Some(e) = layout.element(id) else { return };
    let ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } = &e.kind else { return };
    let Some(m) = app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)) else { return };
    let cam = frame_camera(m, *center, *scale, *bearing, *pitch);
    let new_scale = (scale * k).clamp(MIN_FRAME_SCALE, MAX_FRAME_SCALE);
    // A turned or tilted frame zooms as the map view does, about the anchor.
    if cam.bearing != 0.0 || cam.pitch != 0.0 {
        let (w, h) = (e.w * PT_PER_MM, e.h * PT_PER_MM);
        let mut c = cam;
        c.zoom_at(*scale / new_scale, (ax - e.x) * PT_PER_MM, (ay - e.y) * PT_PER_MM, w, h);
        if let Some(ll) = m.to_lonlat(c.center) {
            let _ = app.run("layout.update", json!({"layout": li, "element": id, "scale": new_scale, "center": [ll.x, ll.y]}));
        }
        return;
    }
    // The anchor's offset from the frame's centre, in points; times the resolution, map units.
    let (px, py) = ((ax - (e.x + e.w / 2.0)) * PT_PER_MM, -(ay - (e.y + e.h / 2.0)) * PT_PER_MM);
    let target = rustgis_engine::geom::Coord::new(cam.center.x + px * cam.res, cam.center.y + py * cam.res);
    // Centre the frame so the anchor's map point stays put. The resolution for a scale depends on
    // the centre's latitude (Web Mercator stretches away from the equator), so solve for the
    // centre's y by Newton's method, then place x with the resolution there.
    let res_at =
        |x: f64, y: f64| m.to_lonlat(rustgis_engine::geom::Coord::new(x, y)).map(|ll| frame_camera(m, ll, new_scale, 0.0, 0.0).res);
    let mut cx = cam.center.x;
    let mut cy = target.y - py * cam.res * new_scale / scale;
    for _ in 0..3 {
        for _ in 0..20 {
            let h = cy.abs().max(1.0) * 1e-7;
            let (Some(r0), Some(r1)) = (res_at(cx, cy), res_at(cx, cy + h)) else { return };
            let (f0, f1) = (cy + py * r0 - target.y, cy + h + py * r1 - target.y);
            let slope = (f1 - f0) / h;
            if !slope.is_finite() || slope.abs() < 1e-12 {
                break;
            }
            cy -= f0 / slope;
            if f0.abs() < r0 * 1e-3 {
                break;
            }
        }
        let Some(r) = res_at(cx, cy) else { return };
        cx = target.x - px * r;
    }
    let Some(ll) = m.to_lonlat(rustgis_engine::geom::Coord::new(cx, cy)) else { return };
    let _ = app.run("layout.update", json!({"layout": li, "element": id, "scale": new_scale, "center": [ll.x, ll.y]}));
}

/// Adds a polygon or lasso frame from the sketch.
fn finish_frame(app: &mut RustGisApp, li: usize, shape: &str) {
    let pts = std::mem::take(&mut app.layout.sketch);
    app.layout.tool = None;
    if pts.len() < 3 {
        return;
    }
    let (x0, y0) = pts.iter().fold((f64::MAX, f64::MAX), |a, p| (a.0.min(p.0), a.1.min(p.1)));
    let (x1, y1) = pts.iter().fold((f64::MIN, f64::MIN), |a, p| (a.0.max(p.0), a.1.max(p.1)));
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    let unit: Vec<[f64; 2]> = pts.iter().map(|p| [(p.0 - x0) / w, (p.1 - y0) / h]).collect();
    if let Ok(v) = app.run("layout.addFrame", json!({"layout": li, "shape": shape, "points": unit, "x": x0, "y": y0, "w": w, "h": h})) {
        app.layout.selected = v["element"].as_u64().map(|v| v as u32);
    }
}
