//! The layout view: the page with its elements, shown instead of the map when a layout is open.
//! Click an element to select it, drag it to move, drag its handles to resize, Shift-drag a map
//! frame to pan its map; the wheel zooms the page. The properties panel edits the selection.
//! Rulers run along the top and left: drag out of one to add a guide, drag a guide back onto one
//! to remove it. Dragged elements snap to guides, the page and each other (Alt places freely).

use egui::{Align2, Color32, CornerRadius, CursorIcon, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use rustgis_engine::map::layout::{
    Arrange, ArrowStyle, CoordFormat, ElementKind, Grid, GridKind, GridStyle, Layout, LineHead, MAX_COLORBAR_TICKS, MAX_FRAME_SCALE,
    MAX_GRID_WIDTH, MAX_LEGEND_COLUMNS, MAX_SCALE_DIVISIONS, MAX_TICK_MM, MIN_FRAME_SCALE, Orientation, PT_PER_MM, ScaleBarStyle,
    ScaleUnits, TickSide, frame_camera, line_angle, line_length,
};
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::{Tokens, regular, semibold};
use crate::widgets::Dragged;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
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

/// Width of the rulers along the view's top and left edges (points).
const RULER: f32 = 18.0;
/// How near (points) a dragged edge snaps to a line.
const SNAP_PT: f32 = 6.0;
/// How near (points) the pointer picks up a guide.
const GUIDE_PICK: f32 = 4.0;
const GUIDE_COLOR: Color32 = Color32::from_rgb(0x00, 0x9E, 0xD8);
const SNAP_COLOR: Color32 = Color32::from_rgb(0xE0, 0x3E, 0x8C);

/// An element's box: x, y, width, height (mm).
type Box4 = (f64, f64, f64, f64);

/// Seconds at least between two drawings of the page while what its map frames show lands: each
/// tile or read landing would draw the whole page again. Anything landing counts, the page's or
/// another map's: telling them apart would mean working out what each frame asks for, and the
/// wait ends once the page is whole.
const FRAMES_REDRAW_S: f64 = 0.25;

/// What has landed so far: the tile cache's generation (tiles, styles, legend pictures, a raster's
/// parts read in place) and the background reads' (a streamed layer's features, a source or
/// raster opened by its address). Each goes up as something lands.
fn landings() -> (u64, u64) {
    (rustgis_engine::render::rustgis_tiles::cache::generation(), rustgis_engine::map::source::generation())
}

/// A page drawn in the browser while something it shows was still on its way: a map frame's tiles
/// or streamed reads, or a legend's pictures (WMS, WMTS).
#[derive(Clone, Copy)]
struct PageWait {
    /// [`landings`] when it was drawn: anything landing since changes it.
    landed: (u64, u64),
    /// Whether a frame's tiles or reads were among what was missing: the page is drawn again as
    /// they land, at most every [`FRAMES_REDRAW_S`]. Legend pictures alone draw it again only once
    /// they all have, not on every fetch (the map's own tiles land too).
    frames: bool,
    /// When it was drawn (egui's time, seconds).
    drawn: f64,
}

/// Whether a page drawn while `w` was missing is due to be drawn again: something has landed since
/// (it is now at `landed`), and either a frame was drawn short and the last drawing is at least
/// [`FRAMES_REDRAW_S`] old at `now`, or only legend pictures were missing and none is still on its
/// way (`legends_pending`, asked only then).
fn redraw_due(w: PageWait, landed: (u64, u64), now: f64, legends_pending: impl FnOnce() -> bool) -> bool {
    w.landed != landed && if w.frames { now - w.drawn >= FRAMES_REDRAW_S } else { !legends_pending() }
}

#[derive(Default)]
pub struct LayoutView {
    texture: Option<egui::TextureHandle>,
    key: Option<(u64, usize, [usize; 2], i64, i64, i64)>,
    /// What the page was drawn without (in the browser, which doesn't wait): drawn again once it
    /// lands.
    page_wait: Option<PageWait>,
    /// Page zoom (0 or 1 = fit) and pan, in screen points, of the layout `viewed` names.
    zoom: f32,
    pan: egui::Vec2,
    /// The layout `zoom` and `pan` are for, by name (unique, unlike an index, which shifts when
    /// layouts are removed).
    viewed: Option<String>,
    /// The zoom and pan of the project's other layouts opened so far, by name.
    views: std::collections::HashMap<String, (f32, egui::Vec2)>,
    pub selected: Option<u32>,
    /// The selection the Format pane last opened for, by layout and element (ids are per layout):
    /// a new one opens it again, so closing the pane keeps the selection without the pane coming
    /// back by itself.
    formatted: Option<(usize, u32)>,
    /// The element whose fields the Format pane drew this frame, by layout and element: text
    /// typed into any other is saved at the end of the frame (`save_drafts`).
    drawn: Option<(usize, u32)>,
    /// The project (`Session::opened`) the selection, the text being typed and the page views are for.
    project: Option<u64>,
    drag: Option<Drag>,
    /// The page rectangle and points per millimetre the drag last saw.
    drag_page: Option<(Rect, f32)>,
    /// A frame being drawn: "polygon" (clicked) or "lasso" (dragged), and its points (mm).
    pub tool: Option<&'static str>,
    sketch: Vec<(f64, f64)>,
    /// Text being typed into the element properties, by (layout, element, field): kept here so
    /// keystrokes survive the frame, and saved when the field loses focus or stops being shown.
    typing: std::collections::HashMap<(usize, u32, &'static str), String>,
    /// The layout shown, shared between frames until the project changes (layouts can hold
    /// megabytes of pictures, too much to copy every frame), keyed by layout and revision.
    shown: Option<(usize, u64, std::rc::Rc<Layout>)>,
    /// The map series page count and the current page's name, by the same key.
    series: Option<(usize, u64, usize, String)>,
    /// Dragged elements don't snap (they do by default; Alt places freely for one drag).
    pub no_snap: bool,
    /// The rulers are hidden.
    pub no_rulers: bool,
    /// Map frames are navigated rather than moved: dragging one pans its map, the wheel zooms it.
    pub navigate: bool,
    /// A wheel zoom of a map frame shown but not yet applied (one undo step per gesture, not per
    /// wheel step): its layout's name (unique, unlike an index, which shifts when layouts are
    /// removed), the frame, the factor on its scale, the page point (mm) kept under the pointer,
    /// and when the wheel last turned (seconds).
    frame_zoom: Option<(String, u32, f64, (f64, f64), f64)>,
    /// Where the primary button last went down: egui forgets it on release, and a click is
    /// judged by where it started.
    pressed: Option<Pos2>,
    /// The map of a frame being panned, with [`PAN_OVERSCAN`] of map past each side: drawn once
    /// as the pan starts and slid under the pointer, so the map comes into view as it moves; None
    /// when it couldn't be drawn (it isn't tried again for that pan).
    pan_preview: Option<(u32, Option<egui::TextureHandle>)>,
    /// The arrow-key nudges so far: how many runs of them there have been, and the last one's
    /// layout, element and time (seconds), for [`LayoutView::nudge_key`].
    nudges: u64,
    nudged: Option<(usize, u32, f64)>,
}

/// How far (mm) an arrow key nudges the selected layout element, and with Shift.
pub const NUDGE_MM: f64 = 1.0;
pub const NUDGE_FAST_MM: f64 = 10.0;

impl LayoutView {
    /// The undo key for an arrow key nudging element `id` of layout `li` at `now` (seconds): the
    /// last nudge's while the same element keeps being nudged, at most a second apart, so a held
    /// key (or quick taps) is one step to undo.
    pub fn nudge_key(&mut self, li: usize, id: u32, now: f64) -> String {
        let same = self.nudged.is_some_and(|(l, i, t)| l == li && i == id && (0.0..=1.0).contains(&(now - t)));
        if !same {
            self.nudges = self.nudges.wrapping_add(1);
        }
        self.nudged = Some((li, id, now));
        format!("layout nudge {}", self.nudges)
    }
}

/// Forgets the selection, the text typed into its fields and the layouts' page views once another
/// project has replaced the one they were for: its elements and layouts may have the same ids and
/// names. Run after every command, after what agents and scripts sent, and before drafts are saved.
pub(crate) fn follow_project(app: &mut RustGisApp) {
    let opened = app.session.opened;
    if app.layout.project.is_some_and(|p| p != opened) {
        app.layout.selected = None;
        app.layout.formatted = None;
        app.layout.drawn = None;
        app.layout.typing.clear();
        app.layout.viewed = None;
        app.layout.views.clear();
    }
    app.layout.project = Some(opened);
    // A page view stays with its layout: one removed or renamed leaves none for a new layout
    // given that name, and comes back fitted to the page if that's undone.
    let layouts = &app.session.project.layouts;
    let named = |name: &str| layouts.iter().any(|l| l.name == name);
    app.layout.views.retain(|name, _| named(name));
    if app.layout.viewed.as_deref().is_some_and(|name| !named(name)) {
        app.layout.viewed = None;
    }
}

/// How much map a pan's preview draws past each side of the frame, in frame widths and heights.
const PAN_OVERSCAN: f64 = 1.0;
/// Most pixels a pan's preview is drawn with: it's drawn on the spot as the pan starts.
const PAN_PREVIEW_PIXELS: f64 = 6_000_000.0;
/// Longest side, in pixels, a pan's preview is drawn with, within what the GPU takes.
const PAN_PREVIEW_SIDE: f64 = 8192.0;

/// A map frame's map with [`PAN_OVERSCAN`] past each side, at the screen's resolution (or less,
/// within [`PAN_PREVIEW_PIXELS`] and [`PAN_PREVIEW_SIDE`]), for a pan's preview. It doesn't wait
/// for tiles still on their way, as it's drawn while the pointer moves.
fn pan_preview(app: &mut RustGisApp, ctx: &egui::Context, layout: &Layout, id: u32, s: f32) -> Option<egui::TextureHandle> {
    let e = layout.element(id)?;
    let ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } = &e.kind else { return None };
    // A tilted view's perspective follows the drawing's height, so a taller drawing wouldn't match
    // the frame: a pitched frame slides the page as before.
    if *pitch != 0.0 {
        return None;
    }
    let m = app.session.project.maps.iter_mut().find(|m| m.name.eq_ignore_ascii_case(map))?;
    let cam = frame_camera(m, *center, *scale, *bearing, *pitch);
    let k = 1.0 + 2.0 * PAN_OVERSCAN;
    let (w, h) = (e.w * PT_PER_MM * k, e.h * PT_PER_MM * k);
    let screen = f64::from(s) / PT_PER_MM * f64::from(ctx.pixels_per_point());
    // Within the pixel budget, and within the longest side the GPU takes for a texture (a long,
    // thin frame could pass the first and not the second).
    let side = (ctx.input(|i| i.max_texture_side) as f64).min(PAN_PREVIEW_SIDE);
    let ppp = screen.min((PAN_PREVIEW_PIXELS / (w * h).max(1.0)).sqrt()).min(side / w.max(h).max(1.0)).max(0.01);
    let pm = rustgis_engine::render::layout::frame_map_preview(m, cam, w, h, ppp)?;
    let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
    Some(ctx.load_texture("layout_pan_preview", img, egui::TextureOptions::LINEAR))
}

/// Seconds after the wheel stops before a map frame's zoom is applied.
const FRAME_ZOOM_SETTLE: f64 = 0.35;

impl LayoutView {
    /// Draws the page again on the next frame, though the project hasn't changed (the installed
    /// fonts a text names have just been indexed).
    pub fn redraw(&mut self) {
        self.key = None;
    }

    /// Makes `zoom` and `pan` those of the layout named `name`: each layout keeps its own view of
    /// the page, and one not opened yet fits the page.
    fn view_layout(&mut self, name: &str) {
        if self.viewed.as_deref() == Some(name) {
            return;
        }
        if let Some(old) = self.viewed.replace(name.to_string()) {
            self.views.insert(old, (self.zoom, self.pan));
        }
        (self.zoom, self.pan) = self.views.remove(name).unwrap_or_default();
    }

    /// Stops drawing a polygon or lasso frame, or navigating map frames (Esc).
    pub fn cancel_tool(&mut self) {
        self.navigate = false;
        self.tool = None;
        self.sketch.clear();
        if self.drag == Some(Drag::Lasso) {
            self.drag = None;
        }
    }
}

/// Page geometry on screen: the page rectangle and points per millimetre.
fn page_rect(lv: &LayoutView, area: Rect, l: &Layout) -> (Rect, f32) {
    let fit = ((area.width() - 48.0) / l.width as f32).min((area.height() - 48.0) / l.height as f32).max(0.05);
    let s = fit * if lv.zoom > 0.0 { lv.zoom } else { 1.0 };
    let size = vec2(l.width as f32 * s, l.height as f32 * s);
    (Rect::from_center_size(area.center() + lv.pan, size), s)
}

fn to_mm(page: Rect, s: f32, p: Pos2) -> (f64, f64) {
    (((p.x - page.min.x) / s) as f64, ((p.y - page.min.y) / s) as f64)
}

fn to_screen(page: Rect, s: f32, (x, y): (f64, f64)) -> Pos2 {
    pos2(page.min.x + x as f32 * s, page.min.y + y as f32 * s)
}

/// A resize handle's drawn size, and how near (points) the pointer picks one up.
const HANDLE: f32 = 10.0;
const HANDLE_PICK: f32 = 11.0;

/// The selected element's resize handle at a screen point: the element and the handle (0..8
/// clockwise from top-left), the nearest within reach. On a small element the reach shrinks, so
/// its inside still moves it; outside, it keeps a floor, so a thin element's handles can still be
/// picked up. Navigating, a map frame under the pointer pans instead: no handle. A locked
/// element has none.
fn handle_at(app: &RustGisApp, layout: &Layout, page: Rect, s: f32, pos: Pos2) -> Option<(u32, usize)> {
    if nav_frame(app, layout, page, s, pos).is_some() {
        return None;
    }
    let e = app.layout.selected.and_then(|id| layout.element(id)).filter(|e| !e.locked)?;
    let r = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h)));
    let third = r.width().min(r.height()) / 3.0;
    let reach = HANDLE_PICK.min(if r.contains(pos) { third } else { third.max(5.0) });
    handles(r)
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
fn rotate_handle(app: &RustGisApp, layout: &Layout, page: Rect, s: f32) -> Option<(u32, Pos2, Pos2)> {
    let e = app.layout.selected.and_then(|id| layout.element(id)).filter(|e| !e.locked && matches!(e.kind, ElementKind::Line { .. }))?;
    let r = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h)));
    Some((e.id, pos2(r.center().x, r.top() - ROTATE_GAP), r.center()))
}

/// The rotation handle at a screen point: the line, and its centre on screen.
fn rotate_at(app: &RustGisApp, layout: &Layout, page: Rect, s: f32, pos: Pos2) -> Option<(u32, Pos2)> {
    let (id, h, c) = rotate_handle(app, layout, page, s)?;
    ((h - pos).length() <= HANDLE_PICK).then_some((id, c))
}

/// A line element's angle (degrees, anticlockwise from pointing right).
fn element_angle(layout: &Layout, id: u32) -> Option<f64> {
    let e = layout.element(id)?;
    match e.kind {
        ElementKind::Line { dir, head, .. } => Some(line_angle((e.x, e.y, e.w, e.h), dir, head)),
        _ => None,
    }
}

/// The angle (degrees, anticlockwise from pointing right) of `p` seen from `c`, on screen.
fn screen_angle(c: Pos2, p: Pos2) -> f64 {
    f64::from(c.y - p.y).atan2(f64::from(p.x - c.x)).to_degrees()
}

/// A line's angle after the pointer went round its centre from `from` to `to` degrees: in 15°
/// steps with `steps` (Shift), else onto a multiple of 45° within 3° of one, so level, upright
/// and diagonal lines are easy to get. Within -180..180.
fn turned(start: f64, from: f64, to: f64, steps: bool) -> f64 {
    let a = start + (to - from);
    let a = if steps {
        (a / 15.0).round() * 15.0
    } else {
        let m = (a / 45.0).round() * 45.0;
        if (a - m).abs() <= 3.0 { m } else { a }
    };
    (a + 180.0).rem_euclid(360.0) - 180.0
}

/// The cursor for dragging handle `h`.
fn handle_cursor(h: usize) -> CursorIcon {
    match h {
        0 | 4 => CursorIcon::ResizeNwSe,
        2 | 6 => CursorIcon::ResizeNeSw,
        1 | 5 => CursorIcon::ResizeVertical,
        _ => CursorIcon::ResizeHorizontal,
    }
}

fn handles(r: Rect) -> [Pos2; 8] {
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
fn resized((x, y, w, h): (f64, f64, f64, f64), handle: usize, dx: f64, dy: f64) -> (f64, f64, f64, f64) {
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

/// The lines a dragged element snaps to (mm): the page's edges and centre, the guides, and the
/// other visible elements' edges and centres; vertical lines (x), then horizontal (y).
fn snap_lines(l: &Layout, moving: u32) -> (Vec<f64>, Vec<f64>) {
    let mut xs = vec![0.0, l.width / 2.0, l.width];
    let mut ys = vec![0.0, l.height / 2.0, l.height];
    xs.extend(&l.guides.x);
    ys.extend(&l.guides.y);
    for e in l.elements.iter().filter(|e| e.id != moving && e.visible) {
        xs.extend([e.x, e.x + e.w / 2.0, e.x + e.w]);
        ys.extend([e.y, e.y + e.h / 2.0, e.y + e.h]);
    }
    (xs, ys)
}

/// The smallest shift that puts one of `edges` on one of `lines`, if one is within `tol`, and
/// the line.
fn snap_to(edges: &[f64], lines: &[f64], tol: f64) -> Option<(f64, f64)> {
    edges
        .iter()
        .flat_map(|e| lines.iter().map(move |l| (l - e, *l)))
        .filter(|(d, _)| d.abs() <= tol)
        .min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()))
}

/// Where an element lands when moved (no handle) or resized from a handle by the pointer's
/// offset (mm): snapped to `lines` within `tol` mm when given, with the lines it snapped to.
fn landing(
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
fn drag_landing(lv: &LayoutView, ui: &Ui, layout: &Layout, s: f32, at: (f64, f64)) -> Option<(u32, Box4, Option<f64>, Option<f64>)> {
    let (id, handle, start) = match lv.drag? {
        Drag::Move(id, start) => (id, None, start),
        Drag::Resize(id, h, start) => (id, Some(h), start),
        _ => return None,
    };
    let e = layout.element(id)?;
    let free = lv.no_snap || ui.input(|i| i.modifiers.alt);
    let lines = (!free).then(|| snap_lines(layout, id));
    let tol = f64::from(SNAP_PT / s.max(1e-3));
    let (r, sx, sy) = landing((e.x, e.y, e.w, e.h), handle, (at.0 - start.0, at.1 - start.1), lines.as_ref().map(|l| (l, tol)));
    Some((id, r, sx, sy))
}

/// The guide under a screen point: vertical or not, and its index.
fn guide_at(l: &Layout, page: Rect, s: f32, pos: Pos2) -> Option<(bool, usize)> {
    let near = |d: f32| d.abs() < GUIDE_PICK;
    l.guides
        .x
        .iter()
        .position(|x| near(to_screen(page, s, (*x, 0.0)).x - pos.x))
        .map(|i| (true, i))
        .or_else(|| l.guides.y.iter().position(|y| near(to_screen(page, s, (0.0, *y)).y - pos.y)).map(|i| (false, i)))
}

/// Which ruler a screen point is on: Some(true) the left one (vertical guides), Some(false) the top.
fn ruler_at(lv: &LayoutView, area: Rect, p: Pos2) -> Option<bool> {
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
#[allow(clippy::too_many_arguments)]
fn drop_guide(
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
fn rulers(painter: &egui::Painter, area: Rect, page: Rect, s: f32, l: &Layout, pointer: Option<Pos2>, t: &Tokens) {
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

/// The layout to show, copied from the project only when it has changed since the last copy.
fn shown_layout(app: &mut RustGisApp, li: usize) -> Option<std::rc::Rc<Layout>> {
    let rev = app.session.revision;
    match &app.layout.shown {
        Some((i, r, l)) if *i == li && *r == rev => Some(l.clone()),
        _ => {
            let l = std::rc::Rc::new(app.session.project.layouts.get(li).cloned()?);
            app.layout.shown = Some((li, rev, l.clone()));
            Some(l)
        }
    }
}

pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    let Some(li) = app.session.active_layout else { return };
    let Some(layout) = shown_layout(app, li) else { return };
    app.layout.view_layout(&layout.name);
    let t = Tokens::get(ui.ctx());
    if app.layout.selected.is_some_and(|id| layout.element(id).is_none()) {
        app.layout.selected = None;
    }
    // Selecting an element shows its properties in the Format pane (#516). The pane is drawn
    // before the page, so it shows them from the next frame.
    let selected = app.layout.selected.map(|id| (li, id));
    if selected != app.layout.formatted {
        app.layout.formatted = selected;
        if selected.is_some() {
            app.ui.open_right(crate::RightPane::Format);
            ui.ctx().request_repaint();
        }
    }
    let area = ui.available_rect_before_wrap();
    // A stable id: dragging an element selects it and opens the Format pane beside the page,
    // and an automatic id would change with it, losing the drag before it is dropped (#325).
    ui.advance_cursor_after_rect(area);
    let resp = ui.interact(area, ui.scope_id().with("layout_page"), Sense::click_and_drag());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, if t.dark { Color32::from_gray(38) } else { Color32::from_rgb(0xD9, 0xDD, 0xE3) });
    let (page, s) = page_rect(&app.layout, area, &layout);
    interact(app, ui, &resp, &layout, li, page, s);
    // The properties or a drag may have just changed the layout: draw the page as it is now,
    // or the texture would be keyed to the new revision but show the old layout (#325).
    let Some(layout) = shown_layout(app, li) else { return };
    let (page, s) = page_rect(&app.layout, area, &layout);

    // Render the page at screen resolution when it changes.
    let ppp = ui.ctx().pixels_per_point();
    let px_per_mm = s * ppp;
    // The texture can't be larger than the GPU allows: zoomed far in, the page draws coarser.
    let max_side = ui.ctx().input(|i| i.max_texture_side) as f64;
    let dpi = (px_per_mm as f64 * 25.4).min(max_side / layout.width.max(layout.height) * 25.4).clamp(20.0, 300.0);
    let size = [(page.width() * ppp) as usize, (page.height() * ppp) as usize];
    let key = (app.session.revision, li, size, (dpi * 10.0) as i64, layout.width as i64, layout.height as i64);
    // Read before drawing: something landing while it draws is a reason to draw again.
    let landed = landings();
    let now = ui.input(|i| i.time);
    let due = app.layout.page_wait.is_some_and(|w| {
        redraw_due(w, landed, now, || rustgis_engine::render::layout::legends_pending(&layout, &app.session.project.maps))
    });
    if (app.layout.key != Some(key) || due) && size[0] > 2 && size[1] > 2 {
        let info = page_info(app, li);
        if let Some((pm, frames_pending)) =
            rustgis_engine::render::layout::render_pending(&layout, &mut app.session.project.maps, dpi, &info)
        {
            let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
            match &mut app.layout.texture {
                Some(tex) => tex.set(img, egui::TextureOptions::LINEAR),
                None => app.layout.texture = Some(ui.ctx().load_texture("layout", img, egui::TextureOptions::LINEAR)),
            }
            app.layout.key = Some(key);
            // Natively the page waited for them already: drawn again, it would wait again.
            let web = cfg!(target_arch = "wasm32");
            let frames = web && frames_pending;
            // Waiting for the frames draws it again anyway: the legends are asked about once
            // they are whole.
            let legends = web && !frames && rustgis_engine::render::layout::legends_pending(&layout, &app.session.project.maps);
            app.layout.page_wait = (legends || frames).then_some(PageWait { landed, frames, drawn: now });
        }
    }
    if app.layout.page_wait.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
    // A pan's preview is drawn as the pan starts, and let go when it ends.
    match app.layout.drag {
        Some(Drag::PanFrame(id, _)) if app.layout.pan_preview.as_ref().is_none_or(|(p, _)| *p != id) => {
            let ctx = ui.ctx().clone();
            let map = pan_preview(app, &ctx, &layout, id, s);
            app.layout.pan_preview = Some((id, map));
        }
        Some(Drag::PanFrame(..)) => {}
        _ => app.layout.pan_preview = None,
    }
    // Shadow, page, elements.
    painter.rect_filled(page.translate(vec2(3.0, 4.0)), 2.0, Color32::from_black_alpha(50));
    painter.rect_filled(page, 0.0, Color32::WHITE);
    let pointer = ui.input(|i| i.pointer.latest_pos());
    if let Some(tex) = &app.layout.texture {
        painter.image(tex.id(), page, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        // A map frame being panned or zoomed: its part of the page drawn shifted or scaled at
        // once, until the change is applied and the page redrawn.
        let preview = match (app.layout.drag, pointer) {
            (Some(Drag::PanFrame(id, start)), Some(p)) => {
                let (mx, my) = to_mm(page, s, p);
                Some((id, 1.0, (mx - start.0, my - start.1), (0.0, 0.0)))
            }
            _ => app.layout.frame_zoom.as_ref().filter(|z| z.0 == layout.name).map(|z| (z.1, z.2, (0.0, 0.0), z.3)),
        };
        if let Some((id, k, (dx, dy), (ax, ay))) = preview
            && let Some(e) = layout.element(id)
        {
            let frame = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h)));
            // The page region the frame now shows: moved back by the pan, scaled about the anchor.
            let src = |(x, y): (f64, f64)| {
                let (u, v) = (ax + (x - ax) * k - dx, ay + (y - ay) * k - dy);
                pos2((u / layout.width) as f32, (v / layout.height) as f32)
            };
            let uv = Rect::from_min_max(src((e.x, e.y)), src((e.x + e.w, e.y + e.h)));
            let clip = painter.with_clip_rect(frame);
            clip.rect_filled(frame, 0.0, Color32::WHITE);
            match &app.layout.pan_preview {
                // A pan: the frame's map with the overscan around it, moved with the pointer, so
                // what comes into view is map, not the page beside the frame.
                Some((pid, Some(map))) if *pid == id && k == 1.0 => {
                    let o = PAN_OVERSCAN as f32;
                    let around = Rect::from_min_max(frame.min - frame.size() * o, frame.max + frame.size() * o);
                    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    clip.image(map.id(), around.translate(vec2(dx as f32, dy as f32) * s), full, Color32::WHITE);
                }
                _ => {
                    clip.image(tex.id(), frame, uv, Color32::WHITE);
                }
            }
        }
    }
    let accent = t.accent;
    // Guides across the view; one being moved is drawn at the pointer instead.
    let moving = match app.layout.drag {
        Some(Drag::Guide(v, i)) => Some((v, i)),
        _ => None,
    };
    let vline = |x: f32, c: Color32| painter.line_segment([pos2(x, area.top()), pos2(x, area.bottom())], Stroke::new(1.0, c));
    let hline = |y: f32, c: Color32| painter.line_segment([pos2(area.left(), y), pos2(area.right(), y)], Stroke::new(1.0, c));
    for (_, x) in layout.guides.x.iter().enumerate().filter(|(i, _)| moving != Some((true, *i))) {
        vline(to_screen(page, s, (*x, 0.0)).x, GUIDE_COLOR);
    }
    for (_, y) in layout.guides.y.iter().enumerate().filter(|(i, _)| moving != Some((false, *i))) {
        hline(to_screen(page, s, (0.0, *y)).y, GUIDE_COLOR);
    }
    if let (Some(Drag::NewGuide(v) | Drag::Guide(v, _)), Some(p)) = (app.layout.drag, pointer) {
        let (mx, my) = to_mm(page, s, p);
        let label = if v {
            vline(p.x, GUIDE_COLOR);
            format!("{mx:.1} mm")
        } else {
            hline(p.y, GUIDE_COLOR);
            format!("{my:.1} mm")
        };
        painter.text(p + vec2(8.0, 8.0), Align2::LEFT_TOP, label, regular(11.0), GUIDE_COLOR);
    }
    // The dragged element's outline where it will land, and the lines it snapped to.
    if let Some(p) = pointer
        && let Some((_, r, sx, sy)) = drag_landing(&app.layout, ui, &layout, s, to_mm(page, s, p))
    {
        if let Some(x) = sx {
            vline(to_screen(page, s, (x, 0.0)).x, SNAP_COLOR);
        }
        if let Some(y) = sy {
            hline(to_screen(page, s, (0.0, y)).y, SNAP_COLOR);
        }
        let rr = Rect::from_min_max(to_screen(page, s, (r.0, r.1)), to_screen(page, s, (r.0 + r.2, r.1 + r.3)));
        painter.rect_stroke(rr, 0.0, Stroke::new(1.5, accent), egui::StrokeKind::Middle);
    }
    if let Some(e) = app.layout.selected.and_then(|id| layout.element(id)) {
        let r = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h)));
        painter.extend(egui::Shape::dashed_line(
            &[r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()],
            Stroke::new(1.2, accent),
            5.0,
            3.0,
        ));
        // The handle under the pointer, or being dragged, is filled.
        let active = match app.layout.drag {
            Some(Drag::Resize(id, h, _)) if id == e.id => Some(h),
            None => pointer.and_then(|p| handle_at(app, &layout, page, s, p)).map(|(_, h)| h),
            _ => None,
        };
        if e.locked {
            // No handles: a lock at the top-right corner says why.
            crate::icons::paint(&painter, Rect::from_center_size(r.right_top(), vec2(16.0, 16.0)), "lock", accent, accent);
        } else {
            for (i, h) in handles(r).into_iter().enumerate() {
                let hr = Rect::from_center_size(h, vec2(HANDLE, HANDLE));
                painter.rect_filled(hr, 2.0, if active == Some(i) { accent } else { Color32::WHITE });
                painter.rect_stroke(hr, 2.0, Stroke::new(1.5, accent), egui::StrokeKind::Middle);
            }
            // A line's rotation handle: a round knob on a stem above it, filled while it's under
            // the pointer or turning.
            if let Some((_, knob, _)) = rotate_handle(app, &layout, page, s) {
                let on = matches!(app.layout.drag, Some(Drag::Rotate(..)))
                    || (app.layout.drag.is_none() && pointer.is_some_and(|p| rotate_at(app, &layout, page, s, p).is_some()));
                painter.line_segment([pos2(r.center().x, r.top()), knob], Stroke::new(1.2, accent));
                painter.circle(knob, HANDLE * 0.6, if on { accent } else { Color32::WHITE }, Stroke::new(1.5, accent));
            }
        }
        painter.text(r.left_top() + vec2(0.0, -4.0), Align2::LEFT_BOTTOM, &e.name, regular(11.0), accent);
    }
    // A line being turned: where it will point, and its angle.
    if let (Some(Drag::Rotate(id, a0, from)), Some(p)) = (app.layout.drag, pointer)
        && let Some(e) = layout.element(id)
        && let ElementKind::Line { dir, .. } = e.kind
    {
        let r = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h)));
        let c = r.center();
        let a = turned(a0, from, screen_angle(c, p), ui.input(|i| i.modifiers.shift));
        let half = (line_length((e.x, e.y, e.w, e.h), dir) * f64::from(s) / 2.0) as f32;
        let u = vec2(a.to_radians().cos() as f32, -(a.to_radians().sin() as f32));
        painter.line_segment([c - u * half, c + u * half], Stroke::new(2.0, accent));
        painter.circle_filled(c + u * half, 3.5, accent);
        painter.text(p + vec2(12.0, 12.0), Align2::LEFT_TOP, format!("{a:.0}°"), semibold(12.0), accent);
    }
    // A frame being drawn.
    if app.layout.tool.is_some() && !app.layout.sketch.is_empty() {
        let mut pts: Vec<Pos2> = app.layout.sketch.iter().map(|p| to_screen(page, s, *p)).collect();
        if let (Some("polygon"), Some(p)) = (app.layout.tool, pointer) {
            pts.push(p);
        }
        painter.add(egui::Shape::closed_line(pts, Stroke::new(2.0, Color32::from_rgb(0x16, 0x9A, 0x5A))));
    }
    // The rulers, and room for them beside the notes drawn over the view.
    let inset = if app.layout.no_rulers {
        0.0
    } else {
        rulers(&painter, area, page, s, &layout, pointer, &t);
        RULER
    };
    // Status: page size, zoom and hint.
    let hint = match app.layout.tool {
        Some("polygon") => tl!("Click the frame's corners · double-click to finish · Esc to stop").to_string(),
        Some(_) => tl!("Drag the frame's outline · Esc to stop").to_string(),
        None if app.layout.navigate => tl!("Drag a map frame to pan its map, wheel to zoom it · Esc to stop").to_string(),
        None if app.layout.selected.is_some_and(|id| locked(&layout, id)) => {
            tl!("Locked: unlock it in the Format pane to move or change it").to_string()
        }
        None => crate::i18n::fmt(
            "{w} × {h} mm · {zoom}% · drag to move (Alt: no snapping), Shift-drag a map frame to pan it, wheel to zoom",
            &[
                ("w", &format!("{:.0}", layout.width)),
                ("h", &format!("{:.0}", layout.height)),
                ("zoom", &format!("{:.0}", (if app.layout.zoom > 0.0 { app.layout.zoom } else { 1.0 }) * 100.0)),
            ],
        ),
    };
    let g = painter.layout_no_wrap(hint, regular(11.5), t.text_dim);
    let bg = Rect::from_min_size(pos2(area.min.x + inset + 10.0, area.max.y - g.size().y - 14.0), g.size() + vec2(14.0, 8.0));
    painter.rect_filled(bg, CornerRadius::same(10), t.ribbon.gamma_multiply(0.9));
    painter.galley(bg.min + vec2(7.0, 4.0), g, t.text_dim);
    if layout.series.is_some()
        && let Some((total, name)) = series_label(app, li)
    {
        let page_no = layout.series.as_ref().map_or(0, |s| s.page);
        let label = crate::i18n::fmt(
            "Page {n} of {total}: {name}",
            &[("n", &(page_no + 1).to_string()), ("total", &total.to_string()), ("name", &name)],
        );
        let g = painter.layout_no_wrap(label, semibold(12.0), t.text);
        let bg = Rect::from_min_size(pos2(area.min.x + inset + 10.0, area.min.y + inset + 10.0), g.size() + vec2(16.0, 10.0));
        painter.rect_filled(bg, CornerRadius::same(10), t.ribbon);
        painter.galley(bg.min + vec2(8.0, 5.0), g, t.text);
    }
}

/// The map series page count and the current page's name, worked out once per project change.
fn series_label(app: &mut RustGisApp, li: usize) -> Option<(usize, String)> {
    let rev = app.session.revision;
    if let Some((i, r, n, name)) = &app.layout.series
        && *i == li
        && *r == rev
    {
        return Some((*n, name.clone()));
    }
    let page = app.session.project.layouts.get(li).and_then(|l| l.series.as_ref()).map_or(0, |s| s.page);
    let (n, name) = rustgis_engine::cmd::layout::series_info(&app.session, li, page)?;
    app.layout.series = Some((li, rev, n, name.clone()));
    Some((n, name))
}

/// Page values for dynamic text in the view.
pub fn page_info(app: &mut RustGisApp, li: usize) -> rustgis_engine::render::layout::PageInfo {
    let page = app.session.project.layouts.get(li).and_then(|l| l.series.as_ref()).map_or(0, |s| s.page);
    let (pages, page_name) = series_label(app, li).unwrap_or((1, String::new()));
    rustgis_engine::render::layout::PageInfo { page: page + 1, pages, page_name }
}

/// The map frame under a screen point, when map frames are being navigated.
fn nav_frame(app: &RustGisApp, layout: &Layout, page: Rect, s: f32, p: Pos2) -> Option<u32> {
    let (x, y) = to_mm(page, s, p);
    layout.hit(x, y).filter(|id| {
        app.layout.navigate && layout.element(*id).is_some_and(|e| !e.locked && matches!(e.kind, ElementKind::MapFrame { .. }))
    })
}

/// Whether the element is locked (#536): selected, but neither moved nor resized.
fn locked(layout: &Layout, id: u32) -> bool {
    layout.element(id).is_some_and(|e| e.locked)
}

fn interact(app: &mut RustGisApp, ui: &mut Ui, resp: &egui::Response, layout: &Layout, li: usize, page: Rect, s: f32) {
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
        let navigating = app.layout.drag.is_none() && nav_frame(app, layout, page, s, p).is_some();
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
            (_, Some(h)) => ui.ctx().set_cursor_icon(handle_cursor(h)),
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
    let pressed = if resp.clicked() || resp.drag_started() { app.layout.pressed.take() } else { None };
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
fn pan_frame(app: &mut RustGisApp, layout: &Layout, li: usize, id: u32, dx: f64, dy: f64) {
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
fn zoom_frame(app: &mut RustGisApp, layout: &Layout, li: usize, id: u32, k: f64, (ax, ay): (f64, f64)) {
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

/// Saves the text typed into fields the Format pane didn't draw this frame: the selection or the
/// layout changed, or the pane was closed or hidden, before the field could notice it lost focus.
/// Runs once a frame, after the panes and the view.
pub(crate) fn save_drafts(app: &mut RustGisApp) {
    follow_project(app);
    let shown = app.layout.drawn.take();
    let stale: Vec<_> = app.layout.typing.keys().filter(|(l, id, _)| Some((*l, *id)) != shown).copied().collect();
    for key in stale {
        let Some(text) = app.layout.typing.remove(&key) else { continue };
        let (l, id, field) = key;
        let current = app.session.project.layouts.get(l).and_then(|x| x.element(id)).and_then(|e| match (field, &e.kind) {
            ("name", _) => Some(e.name.clone()),
            ("text", ElementKind::Text { text, .. }) => Some(text.clone()),
            ("title", ElementKind::Legend { title, .. } | ElementKind::Colorbar { title, .. }) => Some(title.clone()),
            _ => None,
        });
        if current.is_some_and(|c| c != text) {
            let _ = app.run("layout.update", json!({"layout": l, "element": id, field: text}));
        }
    }
}

/// Saves all text being typed, the field the Format pane shows included: for a command that
/// draws the page before the pane runs this frame (the ribbon is drawn first).
pub(crate) fn commit_drafts(app: &mut RustGisApp) {
    app.layout.drawn = None;
    save_drafts(app);
}

/// A text field whose typing is kept in the view's state while it has focus. Returns the new text
/// when the field loses focus with a change.
fn text_field(
    app: &mut RustGisApp,
    ui: &mut Ui,
    key: (usize, u32, &'static str),
    current: &str,
    multiline: bool,
    label_id: egui::Id,
) -> Option<String> {
    let mut buf = app.layout.typing.remove(&key).unwrap_or_else(|| current.to_string());
    let edit = if multiline {
        egui::TextEdit::multiline(&mut buf).desired_rows(3).desired_width(240.0)
    } else {
        egui::TextEdit::singleline(&mut buf)
    };
    let r = ui.add(edit).labelled_by(label_id);
    if r.lost_focus() {
        return (buf != current).then_some(buf);
    }
    if r.has_focus() {
        app.layout.typing.insert(key, buf);
    }
    None
}

/// A map frame's grid options; `set` sends the changed ones (`false` removes the grid).
fn grid_properties(app: &mut RustGisApp, ui: &mut Ui, grid: Option<&Grid>, id: u32, set: &dyn Fn(&mut RustGisApp, Value)) {
    let mut on = grid.is_some();
    if ui.checkbox(&mut on, tl!("Show grid")).changed() {
        set(app, json!(on));
    }
    let Some(g) = grid else { return };
    // Each number kept across a drag until it is let go (#607).
    let key = |field: &str| egui::Id::unique(("grid_drag", id, field));
    let measured = g.kind == GridKind::Measured;
    egui::Grid::new(("grid_props", id)).num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
        // Latitudes and longitudes, or eastings and northings (ArcGIS Pro's measured grid).
        let input_label = ui.label(tl!("Type"));
        egui::ComboBox::from_id_salt(("grid_kind", id))
            .selected_text(tl!(g.kind.label()))
            .show_ui(ui, |ui| {
                for v in GridKind::ALL {
                    if ui.selectable_label(g.kind == v, tl!(v.label())).clicked() && g.kind != v {
                        set(app, json!({"type": v.code()}));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        if measured {
            crs_row(app, ui, g, id, set);
        }
        let input_label = ui.label(tl!("Interval"));
        let mut step = g.step;
        let drag = Dragged::lend(ui, key("step"), &mut step);
        let (most, speed) = if measured {
            (rustgis_engine::map::layout::MAX_MEASURED_STEP, 10.0)
        } else {
            (rustgis_engine::map::layout::MAX_GRATICULE_STEP, 0.05)
        };
        let r = ui
            .add(
                egui::DragValue::new(&mut step)
                    .update_while_editing(false)
                    .range(0.0..=most)
                    .speed(speed)
                    .max_decimals(4)
                    .custom_formatter(|v, _| {
                        if v <= 0.0 {
                            tl!("Auto").to_string()
                        } else if measured {
                            // (The system's units: metres for UTM.)
                            format!("{v}")
                        } else {
                            format!("{v}°")
                        }
                    }),
            )
            .on_hover_text(if measured { tl!("In the coordinate system's units: metres for UTM.") } else { tl!("In degrees.") })
            .labelled_by(input_label.id);
        if drag.settled(ui, &r, &mut step) && (step - g.step).abs() > 1e-9 {
            set(app, json!({"step": step}));
        }
        ui.end_row();
        let input_label = ui.label(tl!("Style"));
        egui::ComboBox::from_id_salt(("grid_style", id))
            .selected_text(tl!(g.style.label()))
            .show_ui(ui, |ui| {
                for v in GridStyle::ALL {
                    if ui.selectable_label(g.style == v, tl!(v.label())).clicked() && g.style != v {
                        set(app, json!({"style": v.code()}));
                    }
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        ui.label(tl!("Color"));
        let mut c = crate::theme::c32(g.color);
        let resp = crate::widgets::color_button(ui, &mut c);
        if resp.changed() {
            crate::widgets::one_step_per_press(app, &resp);
            set(app, json!({"color": crate::theme::rgba(c).hex()}));
        }
        ui.end_row();
        let input_label = ui.label(tl!("Line width"));
        let mut w = g.width;
        let drag = Dragged::lend(ui, key("width"), &mut w);
        let r = ui
            .add(
                egui::DragValue::new(&mut w)
                    .update_while_editing(false)
                    .range(0.0..=MAX_GRID_WIDTH)
                    .speed(0.05)
                    .max_decimals(2)
                    .suffix(" pt"),
            )
            .labelled_by(input_label.id);
        if drag.settled(ui, &r, &mut w) && (w - g.width).abs() > 1e-9 {
            set(app, json!({"width": w}));
        }
        ui.end_row();
        if g.style.ticks() {
            let input_label = ui.label(tl!("Tick length"));
            let mut t = g.tick_length;
            let drag = Dragged::lend(ui, key("tickLength"), &mut t);
            let r = ui
                .add(
                    egui::DragValue::new(&mut t)
                        .update_while_editing(false)
                        .range(0.0..=MAX_TICK_MM)
                        .speed(0.1)
                        .max_decimals(1)
                        .suffix(" mm"),
                )
                .labelled_by(input_label.id);
            if drag.settled(ui, &r, &mut t) && (t - g.tick_length).abs() > 1e-9 {
                set(app, json!({"tickLength": t}));
            }
            ui.end_row();
        }
        let input_label = ui.label(tl!("Labels"));
        let mut labels = g.labels;
        if ui.checkbox(&mut labels, "").labelled_by(input_label.id).changed() {
            set(app, json!({"labels": labels}));
        }
        ui.end_row();
        if !g.labels {
            return;
        }
        if measured {
            let input_label = ui.label(tl!("Short labels"));
            let mut short = g.short;
            if ui
                .checkbox(&mut short, "")
                .labelled_by(input_label.id)
                .on_hover_text(tl!("The kilometres' last two digits, as topographic maps print them: 43 for 543000."))
                .changed()
            {
                set(app, json!({"short": short}));
            }
            ui.end_row();
        } else {
            // Degrees: decimal or degrees, minutes and seconds, with hemisphere letters or signs.
            ui.label(tl!("Format"));
            ui.horizontal(|ui| {
                for (label, v, code) in [("Decimal", CoordFormat::Decimal, "decimal"), ("DMS", CoordFormat::Dms, "dms")] {
                    if ui.selectable_label(g.format == v, tl!(label)).clicked() && g.format != v {
                        set(app, json!({"format": code}));
                    }
                }
            });
            ui.end_row();
            let input_label = ui.label(tl!("N/S/E/W"));
            let mut hemi = g.hemisphere;
            if ui
                .checkbox(&mut hemi, "")
                .labelled_by(input_label.id)
                .on_hover_text(tl!("Off, west and south are negative: -112.5°."))
                .changed()
            {
                set(app, json!({"hemisphere": hemi}));
            }
            ui.end_row();
        }
        // Two rows, so the four edges fit the panel's width.
        let e = g.edges;
        let all = [("Top", "top", e.top), ("Bottom", "bottom", e.bottom), ("Left", "left", e.left), ("Right", "right", e.right)];
        for (row, label) in [(0, tl!("Edges")), (2, "")] {
            ui.label(label);
            ui.horizontal(|ui| {
                for (i, (label, _, shown)) in all.iter().enumerate().skip(row).take(2) {
                    if ui.selectable_label(*shown, tl!(label)).clicked() {
                        let names: Vec<&str> = all
                            .iter()
                            .enumerate()
                            .filter(|(j, (_, _, s))| if *j == i { !*s } else { *s })
                            .map(|(_, (_, n, _))| *n)
                            .collect();
                        set(app, json!({"edges": names}));
                    }
                }
            });
            ui.end_row();
        }
        ui.label(tl!("Side labels"));
        ui.horizontal(|ui| {
            let vertical = g.label_orientation == Orientation::Vertical;
            for (label, v) in [("Horizontal", false), ("Vertical", true)] {
                if ui.selectable_label(vertical == v, tl!(label)).clicked() && vertical != v {
                    set(app, json!({"labelOrientation": if v { "vertical" } else { "horizontal" }}));
                }
            }
        });
        ui.end_row();
    });
}

/// A measured grid's coordinate system, typed (EPSG:25832, WKT or a PROJ string) and set when the
/// field is left; empty is the map's when projected, else the UTM zone at the frame's centre.
fn crs_row(app: &mut RustGisApp, ui: &mut Ui, g: &Grid, id: u32, set: &dyn Fn(&mut RustGisApp, Value)) {
    let input_label = ui.label(tl!("Coordinate system"));
    let key = egui::Id::unique(("grid_crs", id));
    let current = g.crs.clone().unwrap_or_default();
    let mut buf = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| current.clone());
    let r = ui
        .add(egui::TextEdit::singleline(&mut buf).hint_text(tl!("The map's, else its UTM zone")).desired_width(150.0))
        .labelled_by(input_label.id)
        .on_hover_text(tl!("EPSG:25832, WKT or a PROJ string; empty: the map's when projected, else the UTM zone at the frame's centre."));
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(key, buf));
    } else {
        ui.data_mut(|d| d.remove::<String>(key));
        if r.lost_focus() && buf.trim() != current {
            set(app, json!({"crs": buf.trim()}));
        }
    }
    ui.end_row();
}

/// The Format pane (#516): the selected layout element's properties, while its layout is on
/// screen. On the map (no layout open) or under the Model Builder, it waits for a selection.
pub fn format(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    let on_screen = app.session.active_layout.filter(|_| !app.mb.shown());
    let layout = on_screen.and_then(|li| Some((li, shown_layout(app, li)?)));
    let selected = layout.as_ref().and_then(|(li, l)| Some((*li, app.layout.selected.filter(|id| l.element(*id).is_some())?)));
    match (layout, selected) {
        (Some((li, layout)), Some(drawn)) => {
            app.layout.drawn = Some(drawn);
            let Some(e) = layout.element(drawn.1) else { return };
            // Where it is ("Layout: Text"), as the pane also formats a text on the map ("Map: Text").
            let heading = crate::i18n::fmt("Layout: {kind}", &[("kind", tl_raw!(e.kind.label()))]);
            ui.label(egui::RichText::new(heading).font(semibold(14.0)));
            ui.separator();
            // Locked (#536), its properties show but take no change until it is unlocked.
            let mut lock = e.locked;
            if ui
                .checkbox(&mut lock, tl!("Locked"))
                .on_hover_text(tl!("Keep it from being moved, resized, deleted or edited by mistake"))
                .changed()
            {
                let _ = app.run("layout.update", json!({"layout": li, "element": e.id, "locked": lock}));
            }
            ui.add_enabled_ui(!e.locked, |ui| properties(app, ui, &layout, li));
            element_actions(app, ui, &layout, li, e.id);
        }
        _ => {
            ui.label(egui::RichText::new(tl!("Select an element to format it.")).color(t.text_dim));
        }
    }
}

/// The selected element's properties.
fn properties(app: &mut RustGisApp, ui: &mut Ui, layout: &Layout, li: usize) {
    let Some(e) = app.layout.selected.and_then(|id| layout.element(id)).cloned() else { return };
    let id = e.id;
    // Each number kept across a drag until it is let go (#607): one key per layout, element and field.
    let drag_key = |field: &str| egui::Id::unique(("layout_drag", li, id, field));
    let update = |app: &mut RustGisApp, p: Value| {
        let mut p = p;
        p["layout"] = json!(li);
        p["element"] = json!(id);
        let _ = app.run("layout.update", p);
    };
    crate::textform::section(ui, "Element", |ui| {
        egui::Grid::new(("layout_props", id)).num_columns(2).min_col_width(crate::textform::LABEL_W).spacing(vec2(8.0, 6.0)).show(
            ui,
            |ui| {
                let input_label = ui.label(tl!("Name"));
                if let Some(name) = text_field(app, ui, (li, id, "name"), &e.name, false, input_label.id) {
                    update(app, json!({"name": name}));
                }
                ui.end_row();
                for (label, key, v) in [("X (mm)", "x", e.x), ("Y (mm)", "y", e.y), ("Width (mm)", "w", e.w), ("Height (mm)", "h", e.h)] {
                    let input_label = ui.label(tl!(label));
                    let mut val = v;
                    let drag = Dragged::lend(ui, drag_key(key), &mut val);
                    let r = ui
                        .add(egui::DragValue::new(&mut val).update_while_editing(false).speed(0.5).max_decimals(1))
                        .labelled_by(input_label.id);
                    if drag.settled(ui, &r, &mut val) && (val - v).abs() > 1e-6 {
                        update(app, json!({key: val}));
                    }
                    ui.end_row();
                }
                let input_label = ui.label(tl!("Visible"));
                let mut vis = e.visible;
                if ui.checkbox(&mut vis, "").labelled_by(input_label.id).changed() {
                    update(app, json!({"visible": vis}));
                }
                ui.end_row();
                match &e.kind {
                    // Its text, size, colour and boldness: the shared form, below the grid.
                    ElementKind::Text { .. } => {}
                    ElementKind::Legend { title, all_classes, hide_other, columns, .. } => {
                        let input_label = ui.label(tl!("Title"));
                        if let Some(tt) = text_field(app, ui, (li, id, "title"), title, false, input_label.id) {
                            update(app, json!({"title": tt}));
                        }
                        ui.end_row();
                        // Off, the legend lists only what the frame draws (#207).
                        let input_label = ui.label(tl!("All classes"));
                        let mut all = *all_classes;
                        if ui.checkbox(&mut all, "").labelled_by(input_label.id).changed() {
                            update(app, json!({"allClasses": all}));
                        }
                        ui.end_row();
                        let input_label = ui.label(tl!("Hide other values"));
                        let mut hide = *hide_other;
                        if ui.checkbox(&mut hide, "").labelled_by(input_label.id).changed() {
                            update(app, json!({"hideOther": hide}));
                        }
                        ui.end_row();
                        let input_label = ui.label(tl!("Columns"));
                        let mut n = *columns as u64;
                        let drag = Dragged::lend(ui, drag_key("columns"), &mut n);
                        let r = ui
                            .add(egui::DragValue::new(&mut n).update_while_editing(false).range(1..=MAX_LEGEND_COLUMNS as u64))
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut n) && n as usize != *columns {
                            update(app, json!({"columns": n}));
                        }
                        ui.end_row();
                    }
                    ElementKind::Colorbar { frame, layer, title, orientation, ticks, tick_side, reverse } => {
                        let input_label = ui.label(tl!("Title"));
                        if let Some(tt) = text_field(app, ui, (li, id, "title"), title, false, input_label.id) {
                            update(app, json!({"title": tt}));
                        }
                        ui.end_row();
                        // The rasters of the frame's map; none picked follows the top visible one.
                        let input_label = ui.label(tl!("Layer"));
                        let rasters: Vec<String> = match layout.element(*frame).map(|f| &f.kind) {
                            Some(ElementKind::MapFrame { map, .. }) => app
                                .session
                                .project
                                .maps
                                .iter()
                                .find(|m| m.name.eq_ignore_ascii_case(map))
                                .map(|m| m.layers.iter().filter(|l| l.is_raster()).map(|l| l.name.clone()).collect())
                                .unwrap_or_default(),
                            _ => Vec::new(),
                        };
                        let top = tl!("Top visible raster");
                        egui::ComboBox::from_id_salt(("colorbar_layer", id))
                            .selected_text(layer.as_deref().unwrap_or(top))
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(layer.is_none(), top).clicked() {
                                    update(app, json!({"layer": ""}));
                                }
                                for n in rasters {
                                    if ui.selectable_label(layer.as_deref() == Some(n.as_str()), n.as_str()).clicked() {
                                        update(app, json!({"layer": n}));
                                    }
                                }
                            })
                            .response
                            .labelled_by(input_label.id);
                        ui.end_row();
                        let vertical = *orientation == Orientation::Vertical;
                        ui.label(tl!("Orientation"));
                        ui.horizontal(|ui| {
                            for (label, v) in [("Horizontal", false), ("Vertical", true)] {
                                if ui.selectable_label(vertical == v, tl!(label)).clicked() && vertical != v {
                                    update(app, json!({"orientation": if v { "vertical" } else { "horizontal" }}));
                                }
                            }
                        });
                        ui.end_row();
                        ui.label(tl!("Labels"));
                        ui.horizontal(|ui| {
                            let sides =
                                if vertical { [("Right", "right"), ("Left", "left")] } else { [("Below", "below"), ("Above", "above")] };
                            for ((label, code), side) in sides.into_iter().zip([TickSide::End, TickSide::Start]) {
                                if ui.selectable_label(*tick_side == side, tl!(label)).clicked() && *tick_side != side {
                                    update(app, json!({"tickSide": code}));
                                }
                            }
                        });
                        ui.end_row();
                        let input_label = ui.label(tl!("Ticks"));
                        let mut n = *ticks as u64;
                        let drag = Dragged::lend(ui, drag_key("ticks"), &mut n);
                        let r = ui
                            .add(egui::DragValue::new(&mut n).update_while_editing(false).range(2..=MAX_COLORBAR_TICKS as u64))
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut n) && n as usize != *ticks {
                            update(app, json!({"ticks": n}));
                        }
                        ui.end_row();
                        let input_label = ui.label(tl!("Reverse"));
                        let mut rev = *reverse;
                        if ui.checkbox(&mut rev, "").labelled_by(input_label.id).changed() {
                            update(app, json!({"reverse": rev}));
                        }
                        ui.end_row();
                    }
                    ElementKind::MapFrame { map, scale, bearing, pitch, .. } => {
                        let input_label = ui.label(tl!("Map"));
                        let maps: Vec<String> = app.session.project.maps.iter().map(|m| m.name.clone()).collect();
                        egui::ComboBox::from_id_salt(("frame_map", id))
                            .selected_text(map.as_str())
                            .show_ui(ui, |ui| {
                                for m in maps {
                                    if ui.selectable_label(m == *map, m.as_str()).clicked() {
                                        update(app, json!({"map": m}));
                                    }
                                }
                            })
                            .response
                            .labelled_by(input_label.id);
                        ui.end_row();
                        let input_label = ui.label(tl!("Scale 1:"));
                        let mut sc = *scale;
                        let speed = sc * 0.01;
                        // Below 1:10, tenths: 1:0.5 is a frame at 2:1 (#902).
                        let decimals = if sc < 10.0 { 1 } else { 0 };
                        let drag = Dragged::lend(ui, drag_key("scale"), &mut sc);
                        let r = ui
                            .add(
                                egui::DragValue::new(&mut sc)
                                    .update_while_editing(false)
                                    .range(MIN_FRAME_SCALE..=MAX_FRAME_SCALE)
                                    .speed(speed)
                                    .max_decimals(decimals),
                            )
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut sc) && (sc / scale - 1.0).abs() > 1e-3 {
                            update(app, json!({"scale": sc}));
                        }
                        ui.end_row();
                        // Turned and tilted as the map view is (Ctrl+drag there; Shift+arrows).
                        for (label, key, current, range) in
                            [("Bearing", "bearing", *bearing, -180.0..=180.0), ("Pitch", "pitch", *pitch, 0.0..=85.0)]
                        {
                            let input_label = ui.label(tl!(label));
                            let mut v = current;
                            let drag = Dragged::lend(ui, drag_key(key), &mut v);
                            let r = ui
                                .add(
                                    egui::DragValue::new(&mut v)
                                        .update_while_editing(false)
                                        .range(range)
                                        .speed(0.5)
                                        .max_decimals(1)
                                        .suffix("°"),
                                )
                                .labelled_by(input_label.id);
                            if drag.settled(ui, &r, &mut v) && (v - current).abs() > 1e-6 {
                                update(app, json!({key: v}));
                            }
                            ui.end_row();
                        }
                    }
                    ElementKind::NorthArrow { style, fill, outline, .. } => {
                        let input_label = ui.label(tl!("Style"));
                        egui::ComboBox::from_id_salt(("arrow_style", id))
                            .selected_text(tl!(style.label()))
                            .show_ui(ui, |ui| {
                                for v in ArrowStyle::ALL {
                                    if ui.selectable_label(*style == v, tl!(v.label())).clicked() && *style != v {
                                        update(app, json!({"style": v.code()}));
                                    }
                                }
                            })
                            .response
                            .labelled_by(input_label.id);
                        ui.end_row();
                        for (label, key, current) in [("Fill", "fill", *fill), ("Outline", "outline", *outline)] {
                            ui.label(tl!(label));
                            let mut c = crate::theme::c32(current);
                            let resp = crate::widgets::color_button(ui, &mut c);
                            if resp.changed() {
                                crate::widgets::one_step_per_press(app, &resp);
                                update(app, json!({key: crate::theme::rgba(c).hex()}));
                            }
                            ui.end_row();
                        }
                    }
                    ElementKind::ScaleBar { units, divisions, style, color, fill, .. } => {
                        let input_label = ui.label(tl!("Style"));
                        egui::ComboBox::from_id_salt(("scale_style", id))
                            .selected_text(tl!(style.label()))
                            .show_ui(ui, |ui| {
                                for v in ScaleBarStyle::ALL {
                                    if ui.selectable_label(*style == v, tl!(v.label())).clicked() && *style != v {
                                        update(app, json!({"style": v.code()}));
                                    }
                                }
                            })
                            .response
                            .labelled_by(input_label.id);
                        ui.end_row();
                        let input_label = ui.label(tl!("Units"));
                        egui::ComboBox::from_id_salt(("scale_units", id))
                            .selected_text(tl!(units.label()))
                            .show_ui(ui, |ui| {
                                for v in ScaleUnits::ALL {
                                    if ui.selectable_label(*units == v, tl!(v.label())).clicked() && *units != v {
                                        update(app, json!({"units": v.code()}));
                                    }
                                }
                            })
                            .response
                            .labelled_by(input_label.id);
                        ui.end_row();
                        let input_label = ui.label(tl!("Divisions"));
                        let mut n = *divisions as u64;
                        let drag = Dragged::lend(ui, drag_key("divisions"), &mut n);
                        let r = ui
                            .add(egui::DragValue::new(&mut n).update_while_editing(false).range(1..=MAX_SCALE_DIVISIONS as u64))
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut n) && n as usize != *divisions {
                            update(app, json!({"divisions": n}));
                        }
                        ui.end_row();
                        // The fill is the alternating bar's light divisions: the other styles draw none.
                        let colors = [("Color", "color", *color), ("Fill", "fill", *fill)];
                        let shown = if *style == ScaleBarStyle::Alternating { 2 } else { 1 };
                        for (label, key, current) in colors.into_iter().take(shown) {
                            ui.label(tl!(label));
                            let mut c = crate::theme::c32(current);
                            let resp = crate::widgets::color_button(ui, &mut c);
                            if resp.changed() {
                                crate::widgets::one_step_per_press(app, &resp);
                                update(app, json!({key: crate::theme::rgba(c).hex()}));
                            }
                            ui.end_row();
                        }
                    }
                    ElementKind::Table { rows, .. } => {
                        let input_label = ui.label(tl!("Rows"));
                        let mut n = *rows as u64;
                        let drag = Dragged::lend(ui, drag_key("rows"), &mut n);
                        let r = ui.add(egui::DragValue::new(&mut n).update_while_editing(false).range(1..=500)).labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut n) && n as usize != *rows {
                            update(app, json!({"rows": n}));
                        }
                        ui.end_row();
                    }
                    ElementKind::Rectangle { fill, outline, width, .. } | ElementKind::Ellipse { fill, outline, width } => {
                        // A fill with no opacity leaves only the outline; an outline 0 wide, only the fill.
                        for (label, key, current) in [("Fill", "fill", *fill), ("Outline", "outline", *outline)] {
                            ui.label(tl!(label));
                            let mut c = crate::theme::c32(current);
                            let resp = crate::widgets::color_button(ui, &mut c);
                            if resp.changed() {
                                crate::widgets::one_step_per_press(app, &resp);
                                update(app, json!({key: crate::theme::rgba(c).hex()}));
                            }
                            ui.end_row();
                        }
                        let input_label = ui.label(tl!("Outline width"));
                        let mut wd = *width;
                        let drag = Dragged::lend(ui, drag_key("width"), &mut wd);
                        let r = ui
                            .add(
                                egui::DragValue::new(&mut wd)
                                    .update_while_editing(false)
                                    .range(0.0..=50.0)
                                    .speed(0.1)
                                    .max_decimals(1)
                                    .suffix(" pt"),
                            )
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut wd) && (wd - width).abs() > 1e-6 {
                            update(app, json!({"width": wd}));
                        }
                        ui.end_row();
                    }
                    ElementKind::Line { color, width, dir, head } => {
                        ui.label(tl!("Color"));
                        let mut c = crate::theme::c32(*color);
                        let resp = crate::widgets::color_button(ui, &mut c);
                        if resp.changed() {
                            crate::widgets::one_step_per_press(app, &resp);
                            update(app, json!({"color": crate::theme::rgba(c).hex()}));
                        }
                        ui.end_row();
                        let input_label = ui.label(tl!("Width"));
                        let mut wd = *width;
                        let drag = Dragged::lend(ui, drag_key("width"), &mut wd);
                        let r = ui
                            .add(
                                egui::DragValue::new(&mut wd)
                                    .update_while_editing(false)
                                    .range(0.1..=50.0)
                                    .speed(0.1)
                                    .max_decimals(1)
                                    .suffix(" pt"),
                            )
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut wd) && (wd - width).abs() > 1e-6 {
                            update(app, json!({"width": wd}));
                        }
                        ui.end_row();
                        let input_label = ui.label(tl!("Arrowheads"));
                        egui::ComboBox::from_id_salt(("line_head", id))
                            .selected_text(tl!(head.label()))
                            .show_ui(ui, |ui| {
                                for h in LineHead::ALL {
                                    if ui.selectable_label(h == *head, tl!(h.label())).clicked() {
                                        update(app, json!({"head": h.code()}));
                                    }
                                }
                            })
                            .response
                            .labelled_by(input_label.id);
                        ui.end_row();
                        // Anticlockwise from pointing right; the rotation handle above it turns it too.
                        let input_label = ui.label(tl!("Angle"));
                        let current = line_angle((e.x, e.y, e.w, e.h), *dir, *head);
                        let mut a = current;
                        let drag = Dragged::lend(ui, drag_key("angle"), &mut a);
                        let r = ui
                            .add(
                                egui::DragValue::new(&mut a)
                                    .update_while_editing(false)
                                    .range(-180.0..=180.0)
                                    .speed(1.0)
                                    .max_decimals(1)
                                    .suffix("°"),
                            )
                            .labelled_by(input_label.id);
                        if drag.settled(ui, &r, &mut a) && (a - current).abs() > 1e-6 {
                            update(app, json!({"angle": a}));
                        }
                        ui.end_row();
                    }
                    _ => {}
                }
            },
        );
    });
    // A layout's text: the form a text graphic has too (#516, #535), with the page's dynamic text.
    if let ElementKind::Text { text, size, color, bold, italic, align, letter_spacing, font, font_name } = &e.kind {
        let key = (li, id, "text");
        let mut buf = app.layout.typing.remove(&key).unwrap_or_else(|| text.clone());
        let (mut sz, mut c, mut bb) = (*size, crate::theme::c32(*color), *bold);
        let (mut it, mut al, mut ls, mut fam, mut named) = (*italic, *align, *letter_spacing, *font, font_name.clone());
        let tokens = [rustgis_engine::map::graphics::TOKENS, rustgis_engine::map::layout::PAGE_TOKENS];
        let fields = crate::textform::Fields {
            text: &mut buf,
            size: &mut sz,
            color: Some(&mut c),
            bold: Some(&mut bb),
            italic: Some(&mut it),
            align: Some(&mut al),
            letter_spacing: Some(&mut ls),
            font: Some(&mut fam),
            font_name: Some(&mut named),
            named_fonts: true,
            select: false,
        };
        let settled = crate::textform::section(ui, "Text", |ui| crate::textform::show(ui, ("layout_text", li, id), &tokens, fields))
            .unwrap_or_default();
        if settled.typing {
            app.layout.typing.insert(key, buf);
        } else if settled.text && buf != *text {
            update(app, json!({"text": buf}));
        }
        if settled.size {
            update(app, json!({"size": sz}));
        }
        if let Some(resp) = &settled.color {
            crate::widgets::one_step_per_press(app, resp);
            update(app, json!({"color": crate::theme::rgba(c).hex()}));
        }
        if settled.bold {
            update(app, json!({"bold": bb}));
        }
        if settled.italic {
            update(app, json!({"italic": it}));
        }
        if settled.align {
            update(app, json!({"align": al.code()}));
        }
        if settled.letter_spacing {
            update(app, json!({"letterSpacing": ls}));
        }
        if settled.font {
            // A named family with its fallback (#762), else the generic one.
            let font = match &named {
                Some(n) => json!({"font": n, "fallback": fam.code()}),
                None => json!({"font": fam.code()}),
            };
            update(app, font);
        }
    }
    // The layers a legend lists: a checkbox for each vector layer of its frame's map and each
    // WMS or WMTS layer with a legend, ticked when listed. Names gone from the map drop out of
    // the list as it's written back.
    if let ElementKind::Legend { frame, exclude, .. } = &e.kind {
        let map = layout.element(*frame).and_then(|f| match &f.kind {
            ElementKind::MapFrame { map, .. } => app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)),
            _ => None,
        });
        let names: Vec<String> = map
            .map(|m| {
                // Once each: layers may share a name, and the legend leaves names out.
                let mut names: Vec<String> = Vec::new();
                let listed = m.layers.iter().filter(|l| !l.graphics && !l.is_raster()).map(|l| &l.name);
                for name in listed.chain(m.tiles.iter().filter(|t| !t.legends.is_empty()).map(|t| &t.name)) {
                    if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                        names.push(name.clone());
                    }
                }
                names
            })
            .unwrap_or_default();
        // The names left out that the map still has, those not listed here (a raster's) too.
        let kept: Vec<String> = exclude
            .iter()
            .filter(|x| {
                map.is_some_and(|m| {
                    m.layers.iter().map(|l| &l.name).chain(m.tiles.iter().map(|t| &t.name)).any(|n| n.eq_ignore_ascii_case(x))
                })
            })
            .cloned()
            .collect();
        if !names.is_empty() {
            ui.add_space(4.0);
            egui::CollapsingHeader::new(tl!("Layers")).id_salt(("legend_layers", id)).default_open(!exclude.is_empty()).show(ui, |ui| {
                let left_out = |n: &str| exclude.iter().any(|x| x.eq_ignore_ascii_case(n));
                for n in &names {
                    let mut listed = !left_out(n);
                    if ui.checkbox(&mut listed, n.as_str()).on_hover_text(tl!("List this layer in the legend")).changed() {
                        let mut next: Vec<&String> = kept.iter().filter(|x| !x.eq_ignore_ascii_case(n)).collect();
                        if !listed {
                            next.push(n);
                        }
                        update(app, json!({"exclude": next}));
                    }
                }
            });
        }
    }
    if let ElementKind::MapFrame { grid, .. } = &e.kind {
        ui.add_space(4.0);
        egui::CollapsingHeader::new(tl!("Grid")).id_salt(("frame_grid", id)).default_open(grid.is_some()).show(ui, |ui| {
            grid_properties(app, ui, grid.as_ref(), id, &|app, g| update(app, json!({"grid": g})));
        });
    }
    // The extent indicator this frame draws (another frame's extent): its colour and width.
    if let ElementKind::MapFrame { extent_of: Some(of), extent_color, extent_width, .. } = &e.kind {
        let other = layout.element(*of).map_or_else(|| of.to_string(), |o| o.name.clone());
        ui.add_space(4.0);
        egui::CollapsingHeader::new(tl!("Extent Indicator")).id_salt(("frame_indicator", id)).default_open(true).show(ui, |ui| {
            ui.label(egui::RichText::new(crate::i18n::fmt("Outlines {frame}", &[("frame", &other)])).size(11.5));
            egui::Grid::new(("indicator_props", id)).num_columns(2).show(ui, |ui| {
                ui.label(tl!("Color"));
                let mut c = crate::theme::c32(*extent_color);
                let resp = crate::widgets::color_button(ui, &mut c);
                if resp.changed() {
                    crate::widgets::one_step_per_press(app, &resp);
                    update(app, json!({"extentIndicator": {"color": crate::theme::rgba(c).hex()}}));
                }
                ui.end_row();
                let input_label = ui.label(tl!("Line width"));
                let mut w = *extent_width;
                let drag = Dragged::lend(ui, drag_key("extentWidth"), &mut w);
                let r = ui
                    .add(
                        egui::DragValue::new(&mut w)
                            .update_while_editing(false)
                            .range(rustgis_engine::map::layout::MIN_INDICATOR_WIDTH..=rustgis_engine::map::layout::MAX_INDICATOR_WIDTH)
                            .speed(0.05)
                            .max_decimals(2)
                            .suffix(" pt"),
                    )
                    .labelled_by(input_label.id);
                if drag.settled(ui, &r, &mut w) && (w - extent_width).abs() > 1e-6 {
                    update(app, json!({"extentIndicator": {"width": w}}));
                }
                ui.end_row();
            });
            if ui.button(tl!("Remove Extent Indicator")).clicked() {
                update(app, json!({"extentIndicator": false}));
            }
        });
    }
    ui.add_space(8.0);
    if let ElementKind::MapFrame { .. } = e.kind {
        ui.toggle_value(&mut app.layout.navigate, tl!("Pan and Zoom Map"))
            .on_hover_text(tl!("Drag inside map frames to pan their maps and turn the wheel to zoom them, instead of moving them."));
        ui.horizontal_wrapped(|ui| {
            for (label, to) in [("Zoom To Map View", "view"), ("Full Extent", "full"), ("Selection", "selection")] {
                if ui.button(tl!(label)).clicked() {
                    let _ = app.run("layout.frameZoom", json!({"layout": li, "element": id, "to": to}));
                }
            }
        });
    }
}

/// Duplicate and Delete, under the properties: a locked element can be copied (the copy isn't
/// locked) but not deleted, nor a frame with a locked surround. Then the drawing order: Bring to
/// Front, Bring Forward, Send Backward, Send to Back.
fn element_actions(app: &mut RustGisApp, ui: &mut Ui, layout: &Layout, li: usize, id: u32) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(tl!("Duplicate")).on_hover_text(tl!("Copy this element (Ctrl+D, ⌘D on a Mac)")).clicked()
            && let Ok(v) = app.run("layout.duplicateElement", json!({"layout": li, "element": id}))
        {
            app.layout.selected = v["element"].as_u64().and_then(|v| u32::try_from(v).ok());
        }
        if ui
            .add_enabled(layout.locked_in_removal(id).is_none(), egui::Button::new(tl!("Delete Element")))
            .on_hover_text(tl!("Delete (or Backspace)"))
            .on_disabled_hover_text(locked_delete())
            .clicked()
        {
            let _ = app.run("layout.removeElement", json!({"layout": li, "element": id}));
            app.layout.selected = None;
        }
    });
    // The drawing order: a legend brought to the front is drawn over the map frame under it.
    // A step it can't take (already on top, or at the bottom) is greyed, as is a locked element.
    let Some((at, n)) = layout.draw_order(id) else { return };
    let locked = layout.element(id).is_some_and(|e| e.locked);
    ui.horizontal_wrapped(|ui| {
        for to in Arrange::ALL {
            let can = match to {
                Arrange::Front | Arrange::Forward => at + 1 < n,
                Arrange::Backward | Arrange::Back => at > 0,
            };
            let tip = match to {
                Arrange::Front => "Draw it over every other element",
                Arrange::Forward => "Draw it over the element above it",
                Arrange::Backward => "Draw it under the element below it",
                Arrange::Back => "Draw it under every other element",
            };
            if ui.add_enabled(can && !locked, egui::Button::new(tl!(to.label()))).on_hover_text(tl!(tip)).clicked() {
                let _ = app.run("layout.arrange", json!({"layout": li, "element": id, "to": to.code()}));
            }
        }
    });
}

/// Why a locked element, or a frame with a locked surround, isn't deleted.
pub(crate) fn locked_delete() -> &'static str {
    tl!("It is locked, or one of its surrounds is: unlock it first")
}

#[cfg(test)]
mod tests {
    use super::{FRAMES_REDRAW_S, PageWait, RULER, landing, page_rect, redraw_due, resized, rotate_handle, snap_to, to_screen, turned};
    use crate::RustGisApp;
    use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, vec2};
    use serde_json::json;
    use std::{cell::Cell, rc::Rc};

    /// In the browser, a page drawn while its frames' tiles were on their way is drawn again as
    /// they land, at most every `FRAMES_REDRAW_S`, without asking about legends.
    #[test]
    fn a_page_missing_tiles_is_drawn_again_as_they_land() {
        let w = PageWait { landed: (7, 3), frames: true, drawn: 10.0 };
        let legends = || panic!("the frames' wait doesn't ask about legends");
        assert!(redraw_due(w, (8, 3), 10.0 + FRAMES_REDRAW_S, legends), "a tile landed, long enough since the last drawing");
        assert!(!redraw_due(w, (8, 3), 10.0 + FRAMES_REDRAW_S / 2.0, legends), "a tile landed, but too soon after the last drawing");
        assert!(!redraw_due(w, (7, 3), 60.0, legends), "nothing landed");
    }

    /// A streamed layer's read, or a source opened by its address, lands outside the tile cache:
    /// a frame drawn short of it is drawn again too (#1534).
    #[test]
    fn a_page_missing_a_streamed_read_is_drawn_again_as_it_lands() {
        let w = PageWait { landed: (7, 3), frames: true, drawn: 10.0 };
        assert!(redraw_due(w, (7, 4), 10.0 + FRAMES_REDRAW_S, || false), "a read landed");
        assert!(!redraw_due(w, (7, 4), 10.0 + FRAMES_REDRAW_S / 2.0, || false), "a read landed, but too soon");
    }

    /// A page waiting only for legend pictures is drawn again once they have all landed, not on
    /// every fetch: the map's own tiles land too (#1177).
    #[test]
    fn a_page_missing_legend_pictures_is_drawn_again_once_they_land() {
        let w = PageWait { landed: (7, 3), frames: false, drawn: 10.0 };
        assert!(redraw_due(w, (8, 3), 10.0, || false), "the pictures landed");
        assert!(!redraw_due(w, (8, 3), 60.0, || true), "a fetch landed, but a picture is still on its way");
        assert!(!redraw_due(w, (7, 3), 60.0, || false), "nothing landed");
    }

    #[test]
    fn turns_snap_to_level_upright_and_diagonal() {
        assert_eq!(turned(0.0, 90.0, 0.0, false), -90.0, "a quarter turn clockwise");
        assert_eq!(turned(10.0, 0.0, 33.0, false), 45.0, "43° snaps to 45°");
        assert!((turned(10.0, 0.0, 20.0, false) - 30.0).abs() < 1e-9, "30° is left alone");
        assert_eq!(turned(0.0, 0.0, 20.0, true), 15.0, "Shift: 15° steps");
        assert!((turned(170.0, 0.0, 30.0, false) + 160.0).abs() < 1e-9, "200° is -160°");
    }

    /// Dragging an arrow's round handle from above its centre round to its right turns it a
    /// quarter turn clockwise, about its centre, keeping its length.
    #[test]
    fn an_arrow_turns_by_its_rotation_handle() {
        use rustgis_engine::map::layout::{ElementKind, line_angle, line_length};
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Turn", "template": "blank"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let id = app.run("layout.addElement", json!({"layout": li, "kind": "arrow", "angle": 0, "length": 60})).unwrap()["element"]
            .as_u64()
            .unwrap() as u32;
        app.layout.selected = Some(id);
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        let (_, knob, c) = rotate_handle(h.state(), &layout, page, s).unwrap();
        let to = c + vec2((knob - c).length(), 0.0);
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(knob));
        h.step();
        h.event(button(knob, true));
        h.step();
        for i in 1..=6 {
            h.event(Event::PointerMoved(knob + (to - knob) * i as f32 / 6.0));
            h.step();
        }
        h.event(button(to, false));
        h.step();
        let app = h.state();
        let e = app.session.project.layouts[li].element(id).unwrap();
        let ElementKind::Line { dir, head, .. } = e.kind else { panic!("not a line") };
        let r = (e.x, e.y, e.w, e.h);
        assert!((line_angle(r, dir, head) + 90.0).abs() < 1e-9, "points down: {}°", line_angle(r, dir, head));
        assert!((line_length(r, dir) - 60.0).abs() < 1e-6);
        let old = layout.element(id).unwrap();
        assert!(((e.x + e.w / 2.0) - (old.x + old.w / 2.0)).abs() < 1e-9 && ((e.y + e.h / 2.0) - (old.y + old.h / 2.0)).abs() < 1e-9);
        assert_eq!(app.layout.selected, Some(id), "still selected");
    }

    /// While a map frame is panned, its map is drawn past the frame's sides and slid with the
    /// pointer, so what comes into view is map rather than the page beside the frame; the drawing
    /// is let go when the pan ends, which moves the frame's map as before.
    #[test]
    fn a_panned_frame_shows_map_beyond_its_edges() {
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Pan"})).unwrap();
        app.layout.navigate = true;
        let li = app.session.active_layout.unwrap();
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let layout = h.state().session.project.layouts[li].clone();
        let id = layout.first_frame().unwrap();
        let center = |l: &rustgis_engine::map::layout::Layout| match &l.element(id).unwrap().kind {
            ElementKind::MapFrame { center, .. } => *center,
            k => panic!("{k:?}"),
        };
        let before = center(&layout);
        let e = layout.element(id).unwrap();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        let from = to_screen(page, s, (e.x + e.w / 2.0, e.y + e.h / 2.0));
        let to = from + vec2(120.0, 0.0);
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(from));
        h.step();
        h.event(button(from, true));
        h.step();
        for i in 1..=4 {
            h.event(Event::PointerMoved(from + (to - from) * i as f32 / 4.0));
            h.step();
        }
        // Mid-pan: the frame's map with its overscan is drawn, three frames wide and high.
        let (pid, map) = h.state().layout.pan_preview.clone().expect("a preview while panning");
        assert_eq!(pid, id);
        let size = map.expect("the frame's map was drawn").size();
        let frame_px = (e.w as f32 * s * h.ctx.pixels_per_point(), e.h as f32 * s * h.ctx.pixels_per_point());
        assert!(
            (size[0] as f32 - 3.0 * frame_px.0).abs() < 3.0 && (size[1] as f32 - 3.0 * frame_px.1).abs() < 3.0,
            "{size:?} for a frame of {frame_px:?} px"
        );
        h.event(button(to, false));
        h.step();
        assert!(h.state().layout.pan_preview.is_none(), "let go after the pan");
        assert_ne!(center(&h.state().session.project.layouts[li]), before, "the pan moved the frame's map");
    }

    /// Pressing a corner handle and dragging away fast resizes the element, though by the time
    /// egui starts the drag the pointer is already off the handle; it used to move the element.
    #[test]
    fn a_fast_drag_from_a_corner_handle_resizes() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Resize"})).unwrap();
        app.layout.no_snap = true;
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
        app.layout.selected = Some(title.id);
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        // A little inside the bottom-right handle, not on its centre: no bull's-eye needed.
        let corner = to_screen(page, s, (title.x + title.w, title.y + title.h)) + vec2(-4.0, -3.0);
        let to = corner + vec2(60.0, 30.0);
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(corner));
        h.step();
        h.event(button(corner, true));
        h.step();
        // Three big steps: the first already leaves the handle behind.
        for i in 1..=3 {
            h.event(Event::PointerMoved(corner + (to - corner) * i as f32 / 3.0));
            h.step();
        }
        h.event(button(to, false));
        h.step();
        let app = h.state();
        let (_, s) = app.layout.drag_page.unwrap();
        let e = app.session.project.layouts[li].element(title.id).unwrap();
        assert!(
            (e.x - title.x).abs() < 0.01 && (e.y - title.y).abs() < 0.01,
            "the top-left stays: {:?} → {:?}",
            (title.x, title.y),
            (e.x, e.y)
        );
        let (dw, dh) = ((e.w - title.w) * s as f64, (e.h - title.h) * s as f64);
        assert!((59.0..=61.0).contains(&dw) && (29.0..=31.0).contains(&dh), "grew by {dw} × {dh} points, not 60 × 30");
    }

    /// A click that starts on the selected element's handle keeps the selection, though the
    /// pointer moved off the handle (and off the element) before the release.
    #[test]
    fn a_click_started_on_a_handle_keeps_the_selection() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Click"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
        app.layout.selected = Some(title.id);
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        // Pressed at the edge of the bottom-right handle's reach, released a few points further
        // out: still a click, but on nothing.
        let corner = to_screen(page, s, (title.x + title.w, title.y + title.h));
        let (from, to) = (corner + vec2(8.0, 8.0), corner + vec2(12.0, 12.0));
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(from));
        h.step();
        h.event(button(from, true));
        h.step();
        h.event(Event::PointerMoved(to));
        h.step();
        h.event(button(to, false));
        h.run_steps(2);
        let (x, y) = super::to_mm(page, s, to);
        assert!(layout.hit(x, y).is_none(), "the release is off every element");
        assert_eq!(h.state().layout.selected, Some(title.id));
    }

    /// Dragging an element redraws the page with it where it was dropped (#325).
    #[test]
    fn dropped_element_is_drawn_where_it_lands() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Drag"})).unwrap();
        // The drop itself, not snapping (tested below).
        app.layout.no_snap = true;
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        // Fonts take effect on the frame after they are installed.
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let li = h.state().session.active_layout.unwrap();
        let layout = h.state().session.project.layouts[li].clone();
        let title = layout.elements.iter().find(|e| e.name == "Title").unwrap().clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        let from = to_screen(page, s, (title.x + title.w / 2.0, title.y + title.h / 2.0));
        let to = from + vec2(0.0, 120.0);
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        h.event(Event::PointerMoved(from));
        h.step();
        h.event(button(from, true));
        h.step();
        for i in 1..=6 {
            h.event(Event::PointerMoved(from + (to - from) * i as f32 / 6.0));
            h.step();
        }
        let before = h.state().session.revision;
        h.event(button(to, false));
        h.step();
        let app = h.state();
        assert!(app.session.revision > before, "the drop should update the layout");
        let moved = app.session.project.layouts[li].element(title.id).unwrap();
        // A vertical drag moves it straight down by the pointer's motion, although the
        // properties opened beside the page and re-fitted it mid-drag.
        let (_, s) = app.layout.drag_page.unwrap();
        assert!((moved.x - title.x).abs() < 0.01, "x moved from {} to {}", title.x, moved.x);
        // The drag runs from where the button went down, so the element moved with the pointer.
        let dy = (moved.y - title.y) * s as f64;
        assert!((119.0..=120.5).contains(&dy), "y moved {dy} points");
        // The page was drawn, and keyed, from the layout as it is after the drop.
        let (i, rev, drawn) = app.layout.shown.as_ref().unwrap();
        assert_eq!((*i, *rev), (li, app.session.revision));
        assert_eq!(drawn.element(title.id).map(|e| e.y), Some(moved.y));
        assert_eq!(app.layout.key.map(|k| k.0), Some(app.session.revision));
    }

    /// Dragging the map in a turned frame moves it the way the pointer goes, as in the map view.
    #[test]
    fn panning_a_turned_frame_follows_the_pointer() {
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Turned"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let id = app.session.project.layouts[li].first_frame().unwrap();
        // East at the top: the page's right is south, its left north.
        app.run("layout.update", json!({"layout": li, "element": id, "bearing": 90, "center": [10.0, 20.0]})).unwrap();
        let layout = app.session.project.layouts[li].clone();
        super::pan_frame(&mut app, &layout, li, id, 10.0, 0.0);
        let Some(ElementKind::MapFrame { center, .. }) = app.session.project.layouts[li].element(id).map(|e| e.kind.clone()) else {
            panic!("not a map frame")
        };
        // Dragged right, the map brings in what lay to the left: the centre moves north.
        assert!(center.y > 20.5 && (center.x - 10.0).abs() < 0.01, "{center:?}");
    }

    /// Zooming a map frame about a point keeps the map under that point where it was.
    #[test]
    fn zooming_a_frame_keeps_the_map_under_the_pointer() {
        use rustgis_engine::map::layout::{ElementKind, PT_PER_MM, frame_camera};
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Zoom"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let layout = app.session.project.layouts[li].clone();
        let frame = layout.elements.iter().find(|e| matches!(e.kind, ElementKind::MapFrame { .. })).unwrap().clone();
        // A point a quarter of the way into the frame.
        let anchor = (frame.x + frame.w / 4.0, frame.y + frame.h / 4.0);
        // The map point (map units) at the anchor, the frame's scale, and map units per point.
        let map_at = |app: &RustGisApp| {
            let e = app.session.project.layouts[li].element(frame.id).unwrap().clone();
            let ElementKind::MapFrame { map, center, scale, .. } = &e.kind else { panic!("not a map frame") };
            let m = app.session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(map)).unwrap();
            let cam = frame_camera(m, *center, *scale, 0.0, 0.0);
            let x = cam.center.x + (anchor.0 - (e.x + e.w / 2.0)) * PT_PER_MM * cam.res;
            let y = cam.center.y - (anchor.1 - (e.y + e.h / 2.0)) * PT_PER_MM * cam.res;
            (x, y, *scale, cam.res)
        };
        let before = map_at(&app);
        let undo = app.session.undo_depth();
        super::zoom_frame(&mut app, &layout, li, frame.id, 0.5, anchor);
        let after = map_at(&app);
        assert!((after.2 - before.2 * 0.5).abs() < 1e-6 * before.2, "the scale halves: {} → {}", before.2, after.2);
        assert!((after.0 - before.0).hypot(after.1 - before.1) < after.3, "the anchor's map point moved: {before:?} → {after:?}");
        assert_eq!(app.session.undo_depth(), undo + 1, "one undo step");
    }

    /// A wheel zoom still waiting is applied, not lost, when another frame is zoomed; one waiting
    /// for a frame of another layout is dropped, never applied to the open layout.
    #[test]
    fn waiting_frame_zooms_are_applied_or_dropped_never_misapplied() {
        use rustgis_engine::map::layout::ElementKind;
        let scale_of = |app: &RustGisApp, li: usize, id: u32| match app.session.project.layouts[li].element(id).map(|e| &e.kind) {
            Some(ElementKind::MapFrame { scale, .. }) => *scale,
            k => panic!("not a map frame: {k:?}"),
        };
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Two"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let name = app.session.project.layouts[li].name.clone();
        let a = app.session.project.layouts[li].first_frame().unwrap();
        let b = app.run("layout.addFrame", json!({"layout": li, "x": 20, "y": 20, "w": 60, "h": 40})).unwrap()["element"].as_u64().unwrap()
            as u32;
        app.layout.navigate = true;
        let (sa, sb) = (scale_of(&app, li, a), scale_of(&app, li, b));
        // Frame A's zoom waits: its wheel turned "later than ever", so it can't settle by itself.
        app.layout.frame_zoom = Some((name.clone(), a, 0.5, (100.0, 100.0), f64::MAX));
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(2);
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        // Zoom in over frame B.
        h.event(Event::PointerMoved(to_screen(page, s, (50.0, 40.0))));
        h.step();
        h.event(Event::Zoom(1.25));
        h.step();
        assert!((scale_of(h.state(), li, a) - sa * 0.5).abs() < 1e-6 * sa, "A's waiting zoom was applied");
        assert_eq!(h.state().layout.frame_zoom.as_ref().map(|z| z.1), Some(b), "B's zoom waits for the wheel to stop");
        h.run_steps(3);
        assert!(h.state().layout.frame_zoom.is_none());
        assert!(scale_of(h.state(), li, b) < sb, "B zoomed in once the wheel stopped");

        // A zoom left waiting by another layout is dropped without touching this one.
        h.state_mut().layout.frame_zoom = Some(("Another layout".into(), a, 0.5, (100.0, 100.0), 0.0));
        let rev = h.state().session.revision;
        h.run_steps(2);
        assert!(h.state().layout.frame_zoom.is_none());
        assert_eq!(h.state().session.revision, rev, "nothing was applied");
    }

    /// Arrow keys nudge the selected element 1 mm, 10 mm with Shift; a run of them, held or
    /// tapped, is one step to undo; a locked element stays put.
    #[test]
    fn arrow_keys_nudge_the_selected_element_and_undo_as_one() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Nudge"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
        app.layout.selected = Some(title.id);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                let ctx = ui.ctx().clone();
                crate::chrome::shortcuts(app, &ctx);
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let key = |key, modifiers, repeat| Event::Key { key, physical_key: None, pressed: true, repeat, modifiers };
        let at = |h: &egui_kittest::Harness<'_, RustGisApp>| {
            let e = h.state().session.project.layouts[li].element(title.id).unwrap();
            (e.x - title.x, e.y - title.y)
        };
        let depth = h.state().session.undo_depth();
        h.event(key(Key::ArrowRight, Modifiers::NONE, false));
        h.step();
        assert_eq!(at(&h), (1.0, 0.0), "→ nudges 1 mm");
        h.event(key(Key::ArrowDown, Modifiers::SHIFT, false));
        h.step();
        assert_eq!(at(&h), (1.0, 10.0), "Shift-↓ nudges 10 mm");
        // A held key repeats, several times in a frame.
        for _ in 0..3 {
            h.event(key(Key::ArrowLeft, Modifiers::NONE, true));
        }
        h.step();
        assert_eq!(at(&h), (-2.0, 10.0));
        assert_eq!(h.state().session.undo_depth(), depth + 1, "one step to undo, not five");
        h.state_mut().run("edit.undo", serde_json::Value::Null).unwrap();
        assert_eq!(at(&h), (0.0, 0.0), "one undo puts it back");

        // A locked element stays put.
        h.state_mut().run("layout.update", json!({"layout": li, "element": title.id, "locked": true})).unwrap();
        h.event(key(Key::ArrowUp, Modifiers::NONE, false));
        h.step();
        assert_eq!(at(&h), (0.0, 0.0));
    }

    /// The selected element's Bring to Front and Send to Back buttons move it in the drawing
    /// order; a step it can't take is greyed.
    #[test]
    fn the_arrange_buttons_move_an_element_up_and_down() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Arrange"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let frame = app.session.project.layouts[li].first_frame().unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(vec2(600.0, 200.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                let layout = app.session.project.layouts[li].clone();
                super::element_actions(app, ui, &layout, li, frame);
            },
            app,
        );
        h.run_steps(2);
        let at = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.project.layouts[li].draw_order(frame).unwrap();
        let n = at(&h).1;
        h.get_by_label("Bring to Front").click();
        h.run_steps(2);
        assert_eq!(at(&h), (n - 1, n));
        // On top, it can't go further up.
        assert!(h.get_by_label("Bring Forward").accesskit_node().is_disabled());
        h.get_by_label("Send Backward").click();
        h.run_steps(2);
        assert_eq!(at(&h).0, n - 2);
        h.get_by_label("Send to Back").click();
        h.run_steps(2);
        assert_eq!(at(&h).0, 0);
        assert!(h.get_by_label("Send to Back").accesskit_node().is_disabled());
    }

    /// Ctrl+D (Cmd+D) duplicates the selected element and selects the copy; Backspace, the Mac's
    /// delete key, deletes it.
    #[test]
    fn keys_duplicate_and_delete_the_selected_element() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Keys"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
        app.layout.selected = Some(title.id);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                let ctx = ui.ctx().clone();
                crate::chrome::shortcuts(app, &ctx);
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let n = h.state().session.project.layouts[li].elements.len();

        h.event(key(Key::D, Modifiers::COMMAND));
        h.step();
        let app = h.state();
        let layout = &app.session.project.layouts[li];
        assert_eq!(layout.elements.len(), n + 1, "Ctrl+D added a copy");
        let copy = layout.element(app.layout.selected.unwrap()).unwrap();
        assert_ne!(copy.id, title.id, "the copy is selected");
        assert_eq!((copy.name.as_str(), copy.x, copy.y, copy.w), ("Title 2", title.x + 5.0, title.y + 5.0, title.w));
        let copy = copy.id;

        h.event(key(Key::Backspace, Modifiers::NONE));
        h.step();
        let app = h.state();
        let layout = &app.session.project.layouts[li];
        assert_eq!(layout.elements.len(), n, "Backspace deleted the copy");
        assert!(layout.element(copy).is_none() && layout.element(title.id).is_some());
        assert_eq!(app.layout.selected, None);
    }

    /// A locked element is selected, but a drag from inside it or from where its corner handle
    /// would be doesn't move or resize it, and Delete keeps it and says why (#536).
    #[test]
    fn a_locked_element_stays_put() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Locked"})).unwrap();
        app.layout.no_snap = true;
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
        app.run("layout.update", json!({"layout": li, "element": title.id, "locked": true})).unwrap();
        let title = app.session.project.layouts[li].element(title.id).unwrap().clone();
        app.layout.selected = Some(title.id);
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                let ctx = ui.ctx().clone();
                crate::chrome::shortcuts(app, &ctx);
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        let button = |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let middle = to_screen(page, s, (title.x + title.w / 2.0, title.y + title.h / 2.0));
        let corner = to_screen(page, s, (title.x + title.w, title.y + title.h)) + vec2(-4.0, -3.0);
        for from in [middle, corner] {
            h.event(Event::PointerMoved(from));
            h.step();
            h.event(button(from, true));
            h.step();
            for i in 1..=3 {
                h.event(Event::PointerMoved(from + vec2(20.0, 15.0) * i as f32));
                h.step();
            }
            h.event(button(from + vec2(60.0, 45.0), false));
            h.step();
            let app = h.state();
            assert_eq!(app.session.project.layouts[li].element(title.id), Some(&title), "dragged from {from:?}");
            assert_eq!(app.layout.selected, Some(title.id), "it stays selected");
        }
        let key = Event::Key { key: Key::Backspace, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE };
        h.event(key);
        h.step();
        let app = h.state();
        assert!(app.session.project.layouts[li].element(title.id).is_some(), "Backspace kept it");
        assert_eq!(app.layout.selected, Some(title.id));
        assert!(app.session.notices.iter().any(|n| n.text.contains(super::locked_delete())), "it says why");
    }

    /// A text element's properties set italic, alignment and letter spacing (#213, point c).
    #[test]
    fn a_text_element_takes_italic_alignment_and_spacing() {
        use egui_kittest::kittest::{NodeT, Queryable};
        use rustgis_engine::map::layout::{ElementKind, TextAlign};
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Type"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
        app.layout.selected = Some(title);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        use egui::accesskit::{Action, ActionData, ActionRequest, Role};
        assert!(h.query_by_label("Alignment").is_some());
        // Ticking Italic, and setting the spacing as assistive technology does, store them.
        h.get_by_role_and_label(Role::CheckBox, "Italic").click();
        h.run_steps(2);
        let (node, tree) = h.get_by_role_and_label(Role::SpinButton, "Letter spacing").accesskit_node().locate();
        h.event(egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetValue,
            target_tree: tree,
            target_node: node,
            data: Some(ActionData::NumericValue(3.5)),
        }));
        h.run_steps(3);
        let kind = &h.state().session.project.layouts[li].element(title).unwrap().kind;
        assert!(
            matches!(kind, ElementKind::Text { italic: true, align: TextAlign::Left, letter_spacing, .. } if *letter_spacing == 3.5),
            "{kind:?}"
        );
        // The family (#517): Serif picked from the Font list is stored.
        h.get_by_role_and_label(Role::ComboBox, "Font").click();
        h.run_steps(2);
        h.get_by_label("Serif").click();
        h.run_steps(3);
        let kind = &h.state().session.project.layouts[li].element(title).unwrap().kind;
        assert!(matches!(kind, ElementKind::Text { font: rustgis_engine::map::layout::FontFamily::Serif, .. }), "{kind:?}");
        // Said where no serif is installed, and only there.
        let said = h.query_by_label_contains("No serif font is installed").is_some();
        assert_eq!(said, !rustgis_engine::render::fonts::serif_available());
    }

    /// A number dragged in the Format pane and let go is applied (#607): X moves the element by
    /// the drag, though the pane lends the saved X each frame (it used to end where it started).
    #[test]
    fn dragging_x_in_the_format_pane_moves_the_element() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Drag"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().clone();
        app.layout.selected = Some(title.id);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        // X is the pane's first number, the topmost: Name, X, Y, Width, Height.
        let at = h.query_all_by_role(Role::SpinButton).map(|n| n.rect()).min_by(|a, b| a.min.y.total_cmp(&b.min.y)).unwrap().center();
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let undo = h.state().session.undo_depth();
        h.event(Event::PointerMoved(at));
        h.step();
        h.event(button(at, true));
        h.step();
        // Dragged right over several frames (0.5 mm a point), then let go without moving.
        for i in 1..=4 {
            h.event(Event::PointerMoved(at + vec2(10.0 * i as f32, 0.0)));
            h.step();
        }
        h.event(button(at + vec2(40.0, 0.0), false));
        h.run_steps(3);
        let e = h.state().session.project.layouts[li].element(title.id).unwrap().clone();
        assert!(e.x - title.x > 10.0, "moved from {} to {}", title.x, e.x);
        assert_eq!(e.y, title.y, "only X changed");
        // Applied once, on release: one undo step, not one a frame of the drag.
        assert_eq!(h.state().session.undo_depth(), undo + 1, "one undo step");
        assert_eq!(h.state().session.undo_label(), Some("Element Properties"));
    }

    #[test]
    fn dragging_the_angle_in_the_format_pane_turns_the_arrow() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::{ElementKind, line_angle};
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Turn", "template": "blank"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let id =
            app.run("layout.addElement", json!({"kind": "arrow", "angle": 0, "length": 60})).unwrap()["element"].as_u64().unwrap() as u32;
        app.layout.selected = Some(id);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        // Angle is the line's last number, the bottommost.
        let at = h.query_all_by_role(Role::SpinButton).map(|n| n.rect()).max_by(|a, b| a.min.y.total_cmp(&b.min.y)).unwrap().center();
        let button = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        let undo = h.state().session.undo_depth();
        h.event(Event::PointerMoved(at));
        h.step();
        h.event(button(at, true));
        h.step();
        // Dragged right over several frames, then let go without moving: the turn is kept.
        for i in 1..=4 {
            h.event(Event::PointerMoved(at + vec2(10.0 * i as f32, 0.0)));
            h.step();
        }
        h.event(button(at + vec2(40.0, 0.0), false));
        h.run_steps(3);
        let e = h.state().session.project.layouts[li].element(id).unwrap().clone();
        let ElementKind::Line { dir, head, .. } = e.kind else { panic!("{:?}", e.kind) };
        let a = line_angle((e.x, e.y, e.w, e.h), dir, head);
        assert!(a > 10.0, "turned to {a}°");
        assert_eq!(h.state().session.undo_depth(), undo + 1, "one undo step");
    }

    /// A text size typed in the Format pane is set once, on Enter; Escape brings back the size
    /// from before, without a step (#1194).
    #[test]
    fn a_size_typed_in_the_format_pane_is_one_undo_step_and_escape_brings_it_back() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Sizes"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
        app.layout.selected = Some(title);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                // As the app does each frame: Escape noted, then the shortcuts, which take it.
                crate::widgets::note_escape(ui.ctx());
                crate::chrome::shortcuts(app, &ui.ctx().clone());
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let size = |app: &RustGisApp| match &app.session.project.layouts[li].element(title).unwrap().kind {
            ElementKind::Text { size, .. } => *size,
            kind => panic!("{kind:?}"),
        };
        let before = size(h.state());
        let undo = h.state().session.undo_depth();
        let type_size = |h: &mut egui_kittest::Harness<'_, RustGisApp>, text: &str, key: egui::Key, steps: usize| {
            h.get_by_role_and_label(Role::SpinButton, "Size").focus();
            h.run();
            h.event(egui::Event::Text(text.into()));
            h.run();
            assert_eq!(h.state().session.undo_depth(), steps, "nothing is set while typing");
            h.key_press(key);
            h.run();
        };
        type_size(&mut h, "23", egui::Key::Enter, undo);
        assert_eq!(size(h.state()), 23.0);
        assert_eq!(h.state().session.undo_depth(), undo + 1);
        type_size(&mut h, "30", egui::Key::Escape, undo + 1);
        assert_eq!(size(h.state()), 23.0, "Escape brings back the size from before");
        assert_eq!(h.state().session.undo_depth(), undo + 1);
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(size(h.state()), before);
    }

    /// A drag in the picker of a text's colour, in the shared text form, is one undo step (#1194).
    #[test]
    fn a_colour_drag_in_the_format_pane_is_one_undo_step() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Colour"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
        app.layout.selected = Some(title);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let color = |app: &RustGisApp| match &app.session.project.layouts[li].element(title).unwrap().kind {
            ElementKind::Text { color, .. } => *color,
            kind => panic!("{kind:?}"),
        };
        let before = color(h.state());
        let undo = h.state().session.undo_depth();
        let pane_numbers = h.query_all_by_role(Role::SpinButton).count();
        h.get_by_role(Role::ColorWell).click();
        h.run();
        // The picker's first number (red) follows the pane's own.
        let at = h.query_all_by_role(Role::SpinButton).nth(pane_numbers).unwrap().rect().center();
        h.hover_at(at);
        h.drag_at(at);
        h.step();
        for dx in [16.0, 32.0, 48.0] {
            h.hover_at(at + vec2(dx, 0.0));
            h.step();
            assert_ne!(color(h.state()), before, "the page follows the picker");
            assert_eq!(h.state().session.undo_depth(), undo + 1, "one step however many frames");
        }
        h.drop_at(at + vec2(48.0, 0.0));
        h.run();
        assert_eq!(h.state().session.undo_depth(), undo + 1);
        h.state_mut().run("edit.undo", json!(null)).unwrap();
        assert_eq!(color(h.state()), before);
    }

    /// A layout's text has the shared text form (#516): the page's dynamic text as well as the
    /// map's, each added to the text and saved; the size in the range text graphics have.
    #[test]
    fn a_layout_text_offers_the_page_s_dynamic_text() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Tokens"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
        app.layout.selected = Some(title);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let text = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(title).unwrap().kind {
            ElementKind::Text { text, .. } => text.clone(),
            _ => panic!("a text"),
        };
        let before = text(&h);
        for item in ["Page number  {page}", "Map scale  {scale}"] {
            h.get_by_label("Insert Dynamic Text").click();
            h.run_steps(2);
            h.get_by_label(item).click();
            h.run_steps(2);
        }
        assert_eq!(text(&h), format!("{before}{{page}}{{scale}}"));
        // Larger than text graphics drew before, as a layout's text could already be.
        h.state_mut().run("layout.update", json!({"layout": li, "element": title, "size": 999})).unwrap();
        let size = match &h.state().session.project.layouts[li].element(title).unwrap().kind {
            ElementKind::Text { size, .. } => *size,
            _ => panic!("a text"),
        };
        assert_eq!(size, *rustgis_engine::map::graphics::FONT_SIZE.end());
    }

    /// An overview frame's properties show its extent indicator's colour and width (#213), and
    /// its Remove button takes the indicator away.
    #[test]
    fn an_overview_frame_shows_its_extent_indicator() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Overview"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let main = app.session.project.layouts[li].elements.iter().find(|e| matches!(e.kind, ElementKind::MapFrame { .. })).unwrap().id;
        let inset = app.run("layout.addFrame", json!({"x": 200, "y": 150, "w": 60, "h": 40})).unwrap()["element"].as_u64().unwrap() as u32;
        app.run("layout.addElement", json!({"kind": "extentIndicator", "frame": inset, "of": main, "width": 1.5})).unwrap();
        app.layout.selected = Some(inset);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        assert!(h.query_by_label("Extent Indicator").is_some(), "the section shows for an overview frame");
        h.get_by_label("Remove Extent Indicator").click();
        h.run_steps(2);
        let kind = &h.state().session.project.layouts[li].element(inset).unwrap().kind;
        assert!(matches!(kind, ElementKind::MapFrame { extent_of: None, .. }), "removed");
        h.run_steps(2);
        assert!(h.query_by_label("Remove Extent Indicator").is_none(), "and its section with it");
    }

    /// A legend's properties list the layers of its frame's map; unticking one leaves it out of
    /// the legend.
    #[test]
    fn a_legend_leaves_out_the_layers_unticked() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Legend"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let legend = app.session.project.layouts[li].elements.iter().find(|e| matches!(e.kind, ElementKind::Legend { .. })).unwrap().id;
        // A layer the section doesn't list (graphics here), left out through the command, stays
        // left out whatever is ticked.
        app.session.map_mut().layers.iter_mut().find(|l| l.name == "Cities").unwrap().graphics = true;
        app.run("layout.update", json!({"element": legend, "exclude": ["Cities"]})).unwrap();
        app.layout.selected = Some(legend);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        assert!(h.query_by_role_and_label(Role::CheckBox, "Cities").is_none());
        h.get_by_role_and_label(Role::CheckBox, "Rivers").click();
        h.run_steps(2);
        let left_out =
            |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(legend).unwrap().kind {
                ElementKind::Legend { exclude, .. } => exclude.clone(),
                _ => panic!("a legend"),
            };
        assert_eq!(left_out(&h), ["Cities", "Rivers"]);
        // Ticked again, it's listed again.
        h.get_by_role_and_label(Role::CheckBox, "Rivers").click();
        h.run_steps(2);
        assert_eq!(left_out(&h), ["Cities"]);
    }

    /// The layout view with the Format pane beside it, as the app draws them: after following
    /// what agents ran (`RustGisApp::logic`), and saving drafts at the end.
    fn with_format(app: &mut RustGisApp, ui: &mut egui::Ui) {
        super::follow_project(app);
        let t = crate::theme::Tokens::get(ui.ctx());
        if app.ui.right == Some(crate::RightPane::Format) {
            egui::Panel::right("format_pane").exact_size(320.0).show(ui, |ui| super::format(app, ui, &t));
        }
        super::show(app, ui);
        super::save_drafts(app);
    }

    /// A frame's grid in the Format pane: a graticule's degree rows, or a measured grid's
    /// coordinate system (typed, set once left) and short labels.
    #[test]
    fn a_measured_grid_is_set_in_the_format_pane() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::map::layout::ElementKind;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Hike"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let frame = app.session.project.layouts[li].elements.iter().find(|e| e.kind.code() == "mapFrame").unwrap().id;
        app.run("layout.addElement", json!({"kind": "grid"})).unwrap();
        app.run("ui.pane.format", json!({})).unwrap();
        app.layout.selected = Some(frame);
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 1400.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let grid = |h: &egui_kittest::Harness<'_, RustGisApp>| match &h.state().session.project.layouts[li].element(frame).unwrap().kind {
            ElementKind::MapFrame { grid, .. } => grid.clone().unwrap(),
            _ => panic!("not a frame"),
        };
        assert!(h.query_by_label("N/S/E/W").is_some() && h.query_by_label("Coordinate system").is_none(), "a graticule's rows");
        h.state_mut().run("layout.update", json!({"element": frame, "grid": {"type": "measured"}})).unwrap();
        h.run_steps(3);
        assert!(h.query_by_label("N/S/E/W").is_none(), "no degrees for a measured grid");
        assert!(h.query_by_label("Short labels").is_some());
        // A system typed is set once the field is left; until then, the grid's stays.
        h.get_by_role_and_label(Role::TextInput, "Coordinate system").focus();
        h.run_steps(2);
        h.get_by_role_and_label(Role::TextInput, "Coordinate system").type_text("EPSG:2056");
        h.run_steps(2);
        assert_eq!(grid(&h).crs, None, "not while typing");
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert_eq!(grid(&h).crs.as_deref(), Some("EPSG:2056"));
        h.get_by_label("Short labels").click();
        h.run_steps(2);
        assert!(grid(&h).short);
    }

    /// Selecting an element opens the Format pane, a tab like the others (#516). Closing it keeps
    /// the selection, and selecting another element brings it back.
    #[test]
    fn a_selected_element_is_formatted_in_its_pane() {
        use crate::RightPane::{Bookmarks, Format};
        use egui_kittest::kittest::Queryable;
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Format"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let ids: Vec<u32> = app.session.project.layouts[li].elements.iter().map(|e| e.id).collect();
        app.run("ui.pane.bookmarks", json!({})).unwrap();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(2);
        assert_eq!(h.state().ui.right, Some(Bookmarks), "nothing selected, nothing opened");
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.run_steps(2);
        assert!(h.query_by_label("Select an element to format it.").is_some(), "opened by hand: nothing to format");
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.state_mut().layout.selected = ids.first().copied();
        h.run_steps(2);
        assert_eq!(
            (h.state().ui.right, h.state().ui.right_open.clone()),
            (Some(Format), vec![Bookmarks, Format]),
            "a tab beside the others"
        );
        assert!(h.query_all_by_label("Name").next().is_some(), "the element's properties");
        // Closed: the selection stays, and the pane doesn't come back by itself.
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.run_steps(2);
        assert_eq!((h.state().ui.right, h.state().layout.selected), (Some(Bookmarks), ids.first().copied()));
        // Another element selected: it is back.
        h.state_mut().layout.selected = ids.get(1).copied();
        h.run_steps(2);
        assert_eq!(h.state().ui.right, Some(Format));
        // A name being typed when the pane closes is kept, the element still selected.
        use egui::accesskit::Role;
        let name = |h: &egui_kittest::Harness<'_, RustGisApp>| h.state().session.project.layouts[li].element(ids[1]).unwrap().name.clone();
        let before = name(&h);
        h.query_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some(before.as_str())).unwrap().focus();
        h.run_steps(2);
        h.query_all_by_role(Role::TextInput).find(|n| n.is_focused()).unwrap().type_text("Draft");
        h.run_steps(2);
        assert_eq!(name(&h), before, "still being typed");
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.run_steps(2);
        assert!(name(&h).contains("Draft"), "saved when the pane closed: {}", name(&h));
        assert_eq!(h.state().layout.selected, ids.get(1).copied());
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.run_steps(2);
        // The layout out of sight, under the Model Builder or for the map: nothing to edit.
        let hint = |h: &egui_kittest::Harness<'_, RustGisApp>| h.query_by_label("Select an element to format it.").is_some();
        (h.state_mut().mb.open, h.state_mut().mb.active) = (true, true);
        h.run_steps(2);
        assert!(hint(&h), "under the Model Builder");
        (h.state_mut().mb.open, h.state_mut().mb.active) = (false, false);
        h.run_steps(2);
        assert!(!hint(&h));
        h.state_mut().run("layout.close", json!({})).unwrap();
        h.run_steps(2);
        assert!(hint(&h), "on the map");
        assert_eq!(h.state().ui.right, Some(Format), "the pane stays, as other panes do");
        // Closed, then the element of the same id selected in another layout: that is another
        // element, so the pane opens for it.
        h.state_mut().run("ui.pane.format", json!({})).unwrap();
        h.state_mut().run("layout.new", json!({"name": "Other"})).unwrap();
        let other = h.state().session.active_layout.unwrap();
        assert!(h.state().session.project.layouts[other].element(ids[1]).is_some(), "the id exists there too");
        h.state_mut().layout.selected = ids.get(1).copied();
        h.run_steps(2);
        assert_eq!(h.state().ui.right, Some(Format));
    }

    /// Capture To Clipboard draws the text still being typed into the Format pane.
    #[test]
    fn a_capture_keeps_the_text_being_typed() {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("layout.new", json!({"name": "Typing"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
        app.layout.typing.insert((li, title, "text"), "Typed".into());
        // The pane drew this field last frame and it still has focus.
        app.layout.drawn = Some((li, title));
        app.run("ui.captureLayout", json!(null)).unwrap();
        let text = match &app.session.project.layouts[li].element(title).unwrap().kind {
            super::ElementKind::Text { text, .. } => text.clone(),
            _ => String::new(),
        };
        assert_eq!(text, "Typed");
    }

    /// Text being typed when another project opens is for the old one: it never reaches the
    /// element of the same id in the new one, whether the app opened it or an agent did through
    /// the session itself (the MCP relay, the file bridge).
    #[test]
    fn a_draft_is_dropped_with_its_project() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        for by_agent in [false, true] {
            let mut app = RustGisApp::with_sample();
            app.run("layout.new", json!({"name": "Drafts"})).unwrap();
            let li = app.session.active_layout.unwrap();
            let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
            let text = serde_json::to_string(&app.session.project).unwrap();
            app.layout.selected = Some(title);
            let mut fonts = false;
            let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
                move |ui, app: &mut RustGisApp| {
                    if !fonts {
                        crate::theme::install_fonts(ui.ctx(), &[], None);
                        fonts = true;
                        return;
                    }
                    with_format(app, ui);
                },
                app,
            );
            h.run_steps(3);
            h.query_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some("Title")).unwrap().focus();
            h.run_steps(2);
            h.query_all_by_role(Role::TextInput).find(|n| n.is_focused()).unwrap().type_text("Draft");
            h.run_steps(2);
            assert!(!h.state().layout.typing.is_empty(), "being typed");
            let open = json!({"text": text});
            if by_agent {
                // As the relay runs an agent's command: on the session, past `RustGisApp::run`;
                // the copy's layout opened in the same go.
                h.state_mut().session.run("project.openText", &open).unwrap();
                h.state_mut().session.run("layout.activate", &json!({"layout": li})).unwrap();
            } else {
                h.state_mut().run("project.openText", open).unwrap();
            }
            h.run_steps(2);
            let app = h.state();
            assert_eq!(app.session.project.layouts[li].element(title).map(|e| e.name.as_str()), Some("Title"), "agent: {by_agent}");
            assert!(app.layout.typing.is_empty() && app.layout.selected.is_none(), "agent: {by_agent}");
        }
    }

    /// Element ids are per layout: opening another layout ends the selection, so Delete or
    /// Ctrl+D never reaches the element of the same id there.
    #[test]
    fn opening_another_layout_clears_the_selection() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "First"})).unwrap();
        app.run("layout.new", json!({"name": "Second"})).unwrap();
        app.run("layout.activate", json!({"name": "First"})).unwrap();
        let first = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[first].elements.iter().find(|e| e.name == "Title").unwrap().id;
        app.layout.selected = Some(title);
        app.run("layout.removeElement", json!({"element": 999_999})).ok();
        assert_eq!(app.layout.selected, Some(title), "a command in the same layout keeps it");
        app.run("layout.activate", json!({"name": "Second"})).unwrap();
        assert_ne!(app.session.active_layout, Some(first));
        assert!(app.session.project.layouts[app.session.active_layout.unwrap()].element(title).is_some(), "the id exists there too");
        assert_eq!(app.layout.selected, None);
    }

    #[test]
    fn dragged_elements_snap_to_the_nearest_line() {
        let lines = (vec![0.0, 100.0, 150.0], vec![0.0, 50.0]);
        assert_eq!(snap_to(&[98.5, 120.0], &lines.0, 2.0), Some((1.5, 100.0)));
        assert_eq!(snap_to(&[90.0], &lines.0, 2.0), None);
        let r = (10.0, 10.0, 40.0, 20.0);
        // Moved: the right edge (50 + 49) is nearest a line, so the box lands with it on 100.
        let (moved, sx, sy) = landing(r, None, (49.0, 0.5), Some((&lines, 2.0)));
        assert_eq!((moved, sx, sy), ((60.0, 10.5, 40.0, 20.0), Some(100.0), None));
        // Without snapping it lands where the pointer put it.
        assert_eq!(landing(r, None, (49.0, 0.5), None).0, (59.0, 10.5, 40.0, 20.0));
        // Resized from the bottom-right handle: only those edges snap (148.5 → 150, 49 → 50);
        // the top-left stays.
        let (sized, sx, sy) = landing(r, Some(4), (98.5, 19.0), Some((&lines, 2.0)));
        assert_eq!((sized, sx, sy), ((10.0, 10.0, 140.0, 40.0), Some(150.0), Some(50.0)));
    }

    /// Dragging out of the top ruler adds a horizontal guide; dragging it back onto the ruler
    /// removes it.
    #[test]
    fn guides_come_out_of_the_rulers_and_go_back() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Guides", "template": "blank"})).unwrap();
        let area = Rc::new(Cell::new(Rect::NOTHING));
        let shown = area.clone();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                shown.set(ui.available_rect_before_wrap());
                super::show(app, ui);
            },
            app,
        );
        h.run_steps(3);
        let li = h.state().session.active_layout.unwrap();
        let layout = h.state().session.project.layouts[li].clone();
        let (page, s) = page_rect(&h.state().layout, area.get(), &layout);
        let drag = |h: &mut egui_kittest::Harness<'_, RustGisApp>, from: Pos2, to: Pos2| {
            let button =
                |pos: Pos2, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
            h.event(Event::PointerMoved(from));
            h.step();
            h.event(button(from, true));
            h.step();
            for i in 1..=6 {
                h.event(Event::PointerMoved(from + (to - from) * i as f32 / 6.0));
                h.step();
            }
            h.event(button(to, false));
            h.step();
        };
        let ruler = Pos2::new(page.center().x, area.get().top() + RULER / 2.0);
        let at = to_screen(page, s, (0.0, 40.0));
        drag(&mut h, ruler, Pos2::new(ruler.x, at.y));
        let y = h.state().session.project.layouts[li].guides.y.clone();
        assert_eq!(y.len(), 1, "a guide was added: {y:?}");
        assert!((y[0] - 40.0).abs() < 0.5, "at about 40 mm: {y:?}");
        drag(&mut h, Pos2::new(page.center().x, to_screen(page, s, (0.0, y[0])).y), ruler);
        assert!(h.state().session.project.layouts[li].guides.y.is_empty(), "dropped on the ruler, it's gone");
    }

    /// Each layout keeps its own zoom and pan of the page. One not opened yet fits the page, and
    /// so does a new layout given the name of one renamed, removed or of another project.
    #[test]
    fn each_layout_keeps_its_page_view() {
        type H<'a> = egui_kittest::Harness<'a, RustGisApp>;
        type View = (f32, egui::Vec2);
        const FIT: View = (0.0, egui::Vec2::ZERO);
        fn run(h: &mut H<'_>, id: &str, params: serde_json::Value) -> View {
            h.state_mut().run(id, params).unwrap();
            h.run_steps(2);
            (h.state().layout.zoom, h.state().layout.pan)
        }
        fn set(h: &mut H<'_>, view: View) {
            let layout = &mut h.state_mut().layout;
            (layout.zoom, layout.pan) = view;
            h.run_steps(2);
        }
        let (a, b): (View, View) = ((2.0, vec2(30.0, -20.0)), (3.0, vec2(-40.0, 10.0)));
        let open = |name: &str| json!({"name": name});

        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "A", "template": "blank"})).unwrap();
        app.run("layout.new", json!({"name": "B", "template": "blank"})).unwrap();
        let text = serde_json::to_string(&app.session.project).unwrap();
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1000.0, 760.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                with_format(app, ui);
            },
            app,
        );
        h.run_steps(3);

        set(&mut h, b);
        assert_eq!(run(&mut h, "layout.activate", open("A")), FIT, "not opened yet: fits the page");
        set(&mut h, a);
        assert_eq!(run(&mut h, "layout.activate", open("B")), b, "back as it was left");
        assert_eq!(run(&mut h, "layout.activate", open("A")), a);

        // Showing the map and coming back keeps it; a copy starts with its own.
        run(&mut h, "layout.close", json!({}));
        assert_eq!(run(&mut h, "layout.activate", open("A")), a, "closed and opened again");
        assert_eq!(run(&mut h, "layout.duplicate", json!({"name": "A", "newName": "A2"})), FIT, "a copy fits the page");
        assert_eq!(run(&mut h, "layout.activate", open("A")), a, "the copy didn't change it");

        // B's name freed by renaming it: a new B doesn't get its view.
        run(&mut h, "layout.activate", open("B"));
        assert_eq!(run(&mut h, "layout.rename", json!({"name": "B", "newName": "C"})), FIT, "renamed: fits the page");
        run(&mut h, "layout.activate", open("A"));
        assert_eq!(run(&mut h, "layout.new", json!({"name": "B", "template": "blank"})), FIT, "a new layout with a used name");

        // A's name freed by removing it while open.
        run(&mut h, "layout.activate", open("A"));
        set(&mut h, a);
        run(&mut h, "layout.remove", open("A"));
        assert_eq!(run(&mut h, "layout.new", json!({"name": "A", "template": "blank"})), FIT, "a new layout after the removal");

        // Another project with a layout of the same name.
        set(&mut h, a);
        run(&mut h, "project.openText", json!({"text": text}));
        assert_eq!(run(&mut h, "layout.activate", open("A")), FIT, "another project's layout");
    }

    #[test]
    fn widening_a_hairline_keeps_it_thin() {
        let rule = (10.0, 30.0, 100.0, 0.15);
        assert_eq!(resized(rule, 3, 20.0, 5.0), (10.0, 30.0, 120.0, 0.15));
        assert_eq!(resized(rule, 7, -10.0, 0.0), (0.0, 30.0, 110.0, 0.15));
        // Corner and top/bottom handles size the height, at least 2 mm.
        assert_eq!(resized(rule, 4, 0.0, 0.5), (10.0, 30.0, 100.0, 2.0));
        assert_eq!(resized(rule, 5, 0.0, 0.5), (10.0, 30.0, 100.0, 2.0));
    }
}
