//! Snapping, guides and rulers.

use super::*;
use crate::RustGisApp;
use crate::theme::{Tokens, regular};
use egui::{Align2, Pos2, Rect, Stroke, Ui, pos2, vec2};
use rustgis_engine::map::layout::{Element, Layout};
use serde_json::json;

/// The lines a dragged element snaps to (mm): the page's edges and centre, the guides, and the
/// other visible elements' edges and centres; vertical lines (x), then horizontal (y).
fn snap_lines(l: &Layout, moving: u32) -> (Vec<f64>, Vec<f64>) {
    let mut xs = vec![0.0, l.width / 2.0, l.width];
    let mut ys = vec![0.0, l.height / 2.0, l.height];
    xs.extend(&l.guides.x);
    ys.extend(&l.guides.y);
    for e in l.elements.iter().filter(|e| e.id != moving && e.visible) {
        // A turned text by the rectangle it takes up.
        let (x, y, w, h) = e.bounds();
        xs.extend([x, x + w / 2.0, x + w]);
        ys.extend([y, y + h / 2.0, y + h]);
    }
    (xs, ys)
}

/// The smallest shift that puts one of `edges` on one of `lines`, if one is within `tol`, and
/// the line.
pub(super) fn snap_to(edges: &[f64], lines: &[f64], tol: f64) -> Option<(f64, f64)> {
    edges
        .iter()
        .flat_map(|e| lines.iter().map(move |l| (l - e, *l)))
        .filter(|(d, _)| d.abs() <= tol)
        .min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()))
}

/// Where an element lands when moved (no handle) or resized from a handle by the pointer's
/// offset (mm): snapped to `lines` within `tol` mm when given, with the lines it snapped to.
pub(super) fn landing(
    r: Box4,
    handle: Option<usize>,
    (mut dx, mut dy): (f64, f64),
    snap: Option<(&(Vec<f64>, Vec<f64>), f64)>,
) -> (Box4, Option<f64>, Option<f64>) {
    let (x, y, w, h) = r;
    let (mut sx, mut sy) = (None, None);
    if let Some(((xs, ys), tol)) = snap {
        // The edges that move: every edge and the centre for a move, the handle's for a resize.
        let (ex, ey) = match handle {
            None => (vec![x, x + w / 2.0, x + w], vec![y, y + h / 2.0, y + h]),
            Some(hd) => (
                match hd {
                    0 | 6 | 7 => vec![x],
                    2..=4 => vec![x + w],
                    _ => vec![],
                },
                match hd {
                    0..=2 => vec![y],
                    4..=6 => vec![y + h],
                    _ => vec![],
                },
            ),
        };
        let ex: Vec<f64> = ex.iter().map(|e| e + dx).collect();
        let ey: Vec<f64> = ey.iter().map(|e| e + dy).collect();
        if let Some((d, line)) = snap_to(&ex, xs, tol) {
            dx += d;
            sx = Some(line);
        }
        if let Some((d, line)) = snap_to(&ey, ys, tol) {
            dy += d;
            sy = Some(line);
        }
    }
    let out = match handle {
        Some(hd) => resized(r, hd, dx, dy),
        None => (x + dx, y + dy, w, h),
    };
    (out, sx, sy)
}

/// The element being moved or resized, where it lands with the pointer at `at` (mm), and the
/// lines it snapped to.
pub(super) fn drag_landing(
    lv: &LayoutView,
    ui: &Ui,
    layout: &Layout,
    s: f32,
    at: (f64, f64),
) -> Option<(u32, Box4, Option<f64>, Option<f64>)> {
    let (id, handle, start) = match lv.drag? {
        Drag::Move(id, start) => (id, None, start),
        Drag::Resize(id, h, start) => (id, Some(h), start),
        _ => return None,
    };
    let e = layout.element(id)?;
    let d = (at.0 - start.0, at.1 - start.1);
    if let (Some(hd), true) = (handle, e.rotation() != 0.0) {
        return Some((id, turned_resize(e, hd, d), None, None));
    }
    let free = lv.no_snap || ui.input(|i| i.modifiers.alt);
    let lines = (!free).then(|| snap_lines(layout, id));
    let tol = f64::from(SNAP_PT / s.max(1e-3));
    // A turned text moves snapping the rectangle it takes up.
    let b = e.bounds();
    let (r, sx, sy) = landing(b, handle, d, lines.as_ref().map(|l| (l, tol)));
    let r = if handle.is_none() { (e.x + r.0 - b.0, e.y + r.1 - b.1, e.w, e.h) } else { r };
    Some((id, r, sx, sy))
}

/// The box of a turned element after dragging its handle `h` by (dx, dy) mm on the page: resized
/// in its own turned frame (unsnapped), then moved so the side the handle doesn't move stays where
/// it was on the page, though the centre it turns about moved.
fn turned_resize(e: &Element, h: usize, (dx, dy): (f64, f64)) -> Box4 {
    let (sin, cos) = e.rotation().to_radians().sin_cos();
    let local = (dx * cos + dy * sin, -dx * sin + dy * cos);
    let (x, y, w, nh) = resized((e.x, e.y, e.w, e.h), h, local.0, local.1);
    let (mx, my) = (x + w / 2.0 - (e.x + e.w / 2.0), y + nh / 2.0 - (e.y + e.h / 2.0));
    (x + mx * cos - my * sin - mx, y + mx * sin + my * cos - my, w, nh)
}

/// The guide under a screen point: vertical or not, and its index.
pub(super) fn guide_at(l: &Layout, page: Rect, s: f32, pos: Pos2) -> Option<(bool, usize)> {
    let near = |d: f32| d.abs() < GUIDE_PICK;
    l.guides
        .x
        .iter()
        .position(|x| near(to_screen(page, s, (*x, 0.0)).x - pos.x))
        .map(|i| (true, i))
        .or_else(|| l.guides.y.iter().position(|y| near(to_screen(page, s, (0.0, *y)).y - pos.y)).map(|i| (false, i)))
}

/// Which ruler a screen point is on: Some(true) the left one (vertical guides), Some(false) the top.
pub(super) fn ruler_at(lv: &LayoutView, area: Rect, p: Pos2) -> Option<bool> {
    if lv.no_rulers || !area.contains(p) {
        return None;
    }
    let (left, top) = (p.x < area.left() + RULER, p.y < area.top() + RULER);
    match (left, top) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

/// Puts down a guide dragged out of a ruler or moved: dropped on a ruler or off the view, it's
/// removed.
pub(super) fn drop_guide(
    app: &mut RustGisApp,
    layout: &Layout,
    li: usize,
    page: Rect,
    s: f32,
    area: Rect,
    vertical: bool,
    moving: Option<usize>,
    at: Option<Pos2>,
) {
    let mut list = if vertical { layout.guides.x.clone() } else { layout.guides.y.clone() };
    if let Some(i) = moving.filter(|i| *i < list.len()) {
        list.remove(i);
    }
    if let Some(p) = at.filter(|p| area.contains(*p) && (app.layout.no_rulers || (p.x >= area.left() + RULER && p.y >= area.top() + RULER)))
    {
        let (mx, my) = to_mm(page, s, p);
        // To a tenth of a millimetre.
        list.push(((if vertical { mx } else { my }) * 10.0).round() / 10.0);
    }
    let key = if vertical { "x" } else { "y" };
    let _ = app.run("layout.guides", json!({"layout": li, key: list}));
}

/// Rulers along the view's top and left edges, in millimetres from the page's top-left corner,
/// with the guides and the pointer marked.
pub(super) fn rulers(painter: &egui::Painter, area: Rect, page: Rect, s: f32, l: &Layout, pointer: Option<Pos2>, t: &Tokens) {
    const STEPS: [f64; 10] = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0, 1000.0];
    let sf = f64::from(s);
    let minor = STEPS.iter().copied().find(|st| st * sf >= 5.0).unwrap_or(1000.0);
    let major = STEPS.iter().copied().find(|st| st * sf >= 45.0 && (st / minor).fract() == 0.0).unwrap_or(1000.0);
    let top = Rect::from_min_max(area.min, pos2(area.max.x, area.min.y + RULER));
    let left = Rect::from_min_max(area.min, pos2(area.min.x + RULER, area.max.y));
    painter.rect_filled(top, 0.0, t.ribbon);
    painter.rect_filled(left, 0.0, t.ribbon);
    // The page's span, lighter.
    let span_x = Rect::from_min_max(pos2(page.min.x.max(left.max.x), top.min.y), pos2(page.max.x.min(top.max.x), top.max.y));
    let span_y = Rect::from_min_max(pos2(left.min.x, page.min.y.max(top.max.y)), pos2(left.max.x, page.max.y.min(left.max.y)));
    for r in [span_x, span_y] {
        if r.is_positive() {
            painter.rect_filled(r, 0.0, t.input);
        }
    }
    let tick = Stroke::new(1.0, t.text_dim);
    let is_major = |v: f64| ((v / major) - (v / major).round()).abs() < 1e-6;
    // Along the top, then down the side; at most a few thousand ticks either way.
    let (x0, y0) = to_mm(page, s, pos2(left.max.x, top.max.y));
    let (x1, y1) = to_mm(page, s, area.max);
    for k in ((x0 / minor).ceil() as i64..=(x1 / minor).floor() as i64).take(4000) {
        let v = k as f64 * minor;
        let x = to_screen(page, s, (v, 0.0)).x;
        let len = if is_major(v) { RULER * 0.6 } else { RULER * 0.25 };
        painter.line_segment([pos2(x, top.max.y - len), pos2(x, top.max.y)], tick);
        if is_major(v) {
            painter.text(pos2(x + 2.0, top.min.y + 1.0), Align2::LEFT_TOP, format!("{v:.0}"), regular(9.0), t.text_dim);
        }
    }
    for k in ((y0 / minor).ceil() as i64..=(y1 / minor).floor() as i64).take(4000) {
        let v = k as f64 * minor;
        let y = to_screen(page, s, (0.0, v)).y;
        let len = if is_major(v) { RULER * 0.6 } else { RULER * 0.25 };
        painter.line_segment([pos2(left.max.x - len, y), pos2(left.max.x, y)], tick);
        if is_major(v) {
            painter.text(pos2(left.min.x + 1.0, y + 1.0), Align2::LEFT_TOP, format!("{v:.0}"), regular(9.0), t.text_dim);
        }
    }
    // The guides, as small triangles pointing into the page.
    for x in &l.guides.x {
        let x = to_screen(page, s, (*x, 0.0)).x;
        if x > left.max.x {
            let pts = vec![pos2(x - 4.0, top.max.y - 6.0), pos2(x + 4.0, top.max.y - 6.0), pos2(x, top.max.y)];
            painter.add(egui::Shape::convex_polygon(pts, GUIDE_COLOR, Stroke::NONE));
        }
    }
    for y in &l.guides.y {
        let y = to_screen(page, s, (0.0, *y)).y;
        if y > top.max.y {
            let pts = vec![pos2(left.max.x - 6.0, y - 4.0), pos2(left.max.x - 6.0, y + 4.0), pos2(left.max.x, y)];
            painter.add(egui::Shape::convex_polygon(pts, GUIDE_COLOR, Stroke::NONE));
        }
    }
    // Where the pointer is.
    if let Some(p) = pointer.filter(|p| area.contains(*p)) {
        let mark = Stroke::new(1.0, t.accent);
        if p.x > left.max.x {
            painter.line_segment([pos2(p.x, top.min.y), pos2(p.x, top.max.y)], mark);
        }
        if p.y > top.max.y {
            painter.line_segment([pos2(left.min.x, p.y), pos2(left.max.x, p.y)], mark);
        }
    }
    painter.rect_filled(Rect::from_min_size(area.min, vec2(RULER, RULER)), 0.0, t.ribbon);
    painter.line_segment([pos2(left.max.x, top.max.y), pos2(top.max.x, top.max.y)], Stroke::new(1.0, t.border));
    painter.line_segment([pos2(left.max.x, top.max.y), pos2(left.max.x, left.max.y)], Stroke::new(1.0, t.border));
}
