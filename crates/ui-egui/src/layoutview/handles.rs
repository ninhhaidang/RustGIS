//! The selected element's resize and rotation handles.

use super::*;
use crate::RustGisApp;
use egui::{CursorIcon, Pos2, Rect, pos2};
use rustgis_engine::map::layout::{ElementKind, Layout, line_angle};

/// A resize handle's drawn size, and how near (points) the pointer picks one up.
pub(super) const HANDLE: f32 = 10.0;
const HANDLE_PICK: f32 = 11.0;

/// The selected element's resize handle at a screen point: the element and the handle (0..8
/// clockwise from top-left), the nearest within reach. On a small element the reach shrinks, so
/// its inside still moves it; outside, it keeps a floor, so a thin element's handles can still be
/// picked up. Navigating, a map frame under the pointer pans instead: no handle, but a text's,
/// which often sits on a map (a credit, a turned source line) and sticks out over it. A locked
/// element has none.
pub(super) fn handle_at(app: &RustGisApp, layout: &Layout, page: Rect, s: f32, pos: Pos2) -> Option<(u32, usize)> {
    let e = app.layout.selected.and_then(|id| layout.element(id)).filter(|e| !e.locked)?;
    if !matches!(e.kind, ElementKind::Text { .. }) && nav_frame(app, layout, page, s, pos).is_some() {
        return None;
    }
    let r = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h)));
    let third = r.width().min(r.height()) / 3.0;
    // A turned text's handles are on its turned box.
    let reach = HANDLE_PICK.min(if r.contains(screen_turn(pos, r.center(), -e.rotation())) { third } else { third.max(5.0) });
    handles(r)
        .map(|h| screen_turn(h, r.center(), e.rotation()))
        .iter()
        .enumerate()
        .map(|(i, h)| (i, (h.x - pos.x).abs().max((h.y - pos.y).abs())))
        .filter(|(_, d)| *d <= reach)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| (e.id, i))
}

/// How far (points) above a selected line or arrow its rotation handle sits.
const ROTATE_GAP: f32 = 22.0;

/// The selected line's rotation handle, unless it's locked: the line, where the handle is and the
/// line's centre, on screen.
pub(super) fn rotate_handle(app: &RustGisApp, layout: &Layout, page: Rect, s: f32) -> Option<(u32, Pos2, Pos2)> {
    let e = app.layout.selected.and_then(|id| layout.element(id)).filter(|e| !e.locked && matches!(e.kind, ElementKind::Line { .. }))?;
    let r = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h)));
    Some((e.id, pos2(r.center().x, r.top() - ROTATE_GAP), r.center()))
}

/// The rotation handle at a screen point: the line, and its centre on screen.
pub(super) fn rotate_at(app: &RustGisApp, layout: &Layout, page: Rect, s: f32, pos: Pos2) -> Option<(u32, Pos2)> {
    let (id, h, c) = rotate_handle(app, layout, page, s)?;
    ((h - pos).length() <= HANDLE_PICK).then_some((id, c))
}

/// A line element's angle (degrees, anticlockwise from pointing right).
pub(super) fn element_angle(layout: &Layout, id: u32) -> Option<f64> {
    let e = layout.element(id)?;
    match e.kind {
        ElementKind::Line { dir, head, .. } => Some(line_angle((e.x, e.y, e.w, e.h), dir, head)),
        _ => None,
    }
}

/// The angle (degrees, anticlockwise from pointing right) of `p` seen from `c`, on screen.
pub(super) fn screen_angle(c: Pos2, p: Pos2) -> f64 {
    f64::from(c.y - p.y).atan2(f64::from(p.x - c.x)).to_degrees()
}

/// A line's angle after the pointer went round its centre from `from` to `to` degrees: in 15°
/// steps with `steps` (Shift), else onto a multiple of 45° within 3° of one, so level, upright
/// and diagonal lines are easy to get. Within -180..180.
pub(super) fn turned(start: f64, from: f64, to: f64, steps: bool) -> f64 {
    let a = start + (to - from);
    let a = if steps {
        (a / 15.0).round() * 15.0
    } else {
        let m = (a / 45.0).round() * 45.0;
        if (a - m).abs() <= 3.0 { m } else { a }
    };
    (a + 180.0).rem_euclid(360.0) - 180.0
}

/// `p` turned `degrees` clockwise about `c`, on screen.
pub(super) fn screen_turn(p: Pos2, c: Pos2, degrees: f64) -> Pos2 {
    if degrees == 0.0 {
        return p;
    }
    let (sin, cos) = degrees.to_radians().sin_cos();
    let (sin, cos, d) = (sin as f32, cos as f32, p - c);
    pos2(c.x + d.x * cos - d.y * sin, c.y + d.x * sin + d.y * cos)
}

/// The corners of the box `r` (mm) turned `degrees` clockwise about its centre, on screen, from
/// the top left round.
pub(super) fn turned_corners(page: Rect, s: f32, (x, y, w, h): Box4, degrees: f64) -> [Pos2; 4] {
    let r = Rect::from_min_max(to_screen(page, s, (x, y)), to_screen(page, s, (x + w, y + h)));
    [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()].map(|p| screen_turn(p, r.center(), degrees))
}

/// The cursor for dragging handle `h` of an element turned `degrees`: the handle's own, an
/// eighth of a turn on for every 45°.
pub(super) fn handle_cursor(h: usize, degrees: f64) -> CursorIcon {
    let h = (h as i64 + (degrees / 45.0).round() as i64).rem_euclid(8);
    match h {
        0 | 4 => CursorIcon::ResizeNwSe,
        2 | 6 => CursorIcon::ResizeNeSw,
        1 | 5 => CursorIcon::ResizeVertical,
        _ => CursorIcon::ResizeHorizontal,
    }
}

pub(super) fn handles(r: Rect) -> [Pos2; 8] {
    let (c, m) = (r.center(), r);
    [
        m.left_top(),
        pos2(c.x, m.top()),
        m.right_top(),
        pos2(m.right(), c.y),
        m.right_bottom(),
        pos2(c.x, m.bottom()),
        m.left_bottom(),
        pos2(m.left(), c.y),
    ]
}

/// A rectangle after dragging handle `h` by (dx, dy) mm. A side the handle doesn't move keeps its
/// size, so widening a hairline rule leaves it a hairline.
pub(super) fn resized((x, y, w, h): (f64, f64, f64, f64), handle: usize, dx: f64, dy: f64) -> (f64, f64, f64, f64) {
    let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + h);
    match handle {
        0 => (x0, y0) = (x0 + dx, y0 + dy),
        1 => y0 += dy,
        2 => (x1, y0) = (x1 + dx, y0 + dy),
        3 => x1 += dx,
        4 => (x1, y1) = (x1 + dx, y1 + dy),
        5 => y1 += dy,
        6 => (x0, y1) = (x0 + dx, y1 + dy),
        _ => x0 += dx,
    }
    let (sizes_x, sizes_y) = (!matches!(handle, 1 | 5), !matches!(handle, 3 | 7));
    let w = if sizes_x { (x1 - x0).abs().max(2.0) } else { w };
    let h = if sizes_y { (y1 - y0).abs().max(2.0) } else { h };
    (x0.min(x1), y0.min(y1), w, h)
}
