//! The layout view: the page with its elements, shown instead of the map when a layout is open.
//! Click an element to select it, drag it to move, drag its handles to resize, Shift-drag a map
//! frame to pan its map; the wheel zooms the page. The properties panel edits the selection.
//! Rulers run along the top and left: drag out of one to add a guide, drag a guide back onto one
//! to remove it. Dragged elements snap to guides, the page and each other (Alt places freely).

use crate::RustGisApp;
use crate::theme::{Tokens, regular, semibold};
use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use rustgis_engine::map::layout::{ElementKind, Layout, PT_PER_MM, frame_camera, line_length};
use serde_json::{Value, json};

mod actions;
mod format;
mod handles;
mod interact;
mod snap;
#[cfg(test)]
mod tests;

pub(crate) use actions::locked_delete;
use actions::*;
pub use format::format;
#[cfg(test)]
use format::*;
use handles::*;
use interact::*;
use snap::*;

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

/// Says, once each, why frames drawn for a page or a preview drew layers without their cast
/// shadows (see [`rustgis_engine::render::shadows_said`]), as the map view says them.
pub(crate) fn say_frame_shadows(app: &mut RustGisApp, said: &rustgis_engine::render::ShadowsSaid) {
    // Keyed by kind, as the map view keys its own (`say_approximations`): an approximation's text
    // carries a count that changes as the page is drawn again, and the map view and a page say
    // each kind once between them.
    for (i, e) in said.notices.iter().enumerate() {
        let key = said.keys.get(i).cloned().unwrap_or_else(|| e.clone());
        if app.map.tile_errors.insert(key) {
            app.session.warn(e.clone());
        }
    }
}

/// The key a preview drawn for `base` is kept under: with the tile cache's generation read before
/// it was drawn (`drawn_at`) when its frames drew without cast shadows still being worked out (on
/// the interface's thread, which doesn't wait for them), so it is drawn again as they land, even
/// while it was drawn (see [`preview_current`]).
pub(crate) fn preview_key(base: &str, said: &rustgis_engine::render::ShadowsSaid, drawn_at: u64) -> String {
    if said.to_come { format!("{base}#shadows@{drawn_at}") } else { base.to_string() }
}

/// Whether a preview kept under `kept` (see [`preview_key`]) is still the one `base` asks for:
/// a preview waiting for cast shadows is drawn again once anything lands.
pub(crate) fn preview_current(kept: &str, base: &str) -> bool {
    kept == base || kept == format!("{base}#shadows@{}", rustgis_engine::render::rustgis_tiles::cache::generation())
}

/// Whether a preview kept under `kept` waits for cast shadows: the frame is asked again shortly.
pub(crate) fn preview_waits(kept: &str) -> bool {
    kept.contains("#shadows@")
}

/// What has landed so far: the tile cache's generation (tiles, styles, legend pictures, a raster's
/// parts read in place) and the background reads' (a streamed layer's features, a source or
/// raster opened by its address). Each goes up as something lands.
fn landings() -> (u64, u64) {
    (rustgis_engine::render::rustgis_tiles::cache::generation(), rustgis_engine::map::source::generation())
}

/// A page drawn while something it shows was still on its way: a map frame's tiles or streamed
/// reads (in the browser), cast shadows, or a legend's pictures (on every platform).
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
    /// What the page was drawn without: drawn again once it lands.
    page_wait: Option<PageWait>,
    /// The last clipping warning shown, cleared when the text fits or another layout opens.
    text_warning: Option<String>,
    /// The texts of the layout drawn (by its index) cut off on its page, with their lines left
    /// out of their box, as the Format pane tells them (#2346).
    text_losses: Option<(usize, Vec<rustgis_engine::render::layout::TextLoss>)>,
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
    /// The layouts (their [`Layout::instance_id`]), in their order, when `typing` last followed
    /// them ([`follow_project`]).
    typing_order: Vec<u64>,
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
        app.layout.text_warning = None;
        app.layout.text_losses = None;
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
    // Text being typed is kept by its layout's place: when the layouts change places (a move, a
    // removal, a copy, an undo, from the app, an agent or a script), it follows its layout, renamed
    // or not (#2802). With its layout gone it goes too, even for a new layout given its name.
    if !app.layout.typing_order.iter().copied().eq(layouts.iter().map(Layout::instance_id)) {
        let was = std::mem::replace(&mut app.layout.typing_order, layouts.iter().map(Layout::instance_id).collect());
        let now = &app.layout.typing_order;
        let typing = std::mem::take(&mut app.layout.typing);
        app.layout.typing = typing
            .into_iter()
            .filter_map(|((l, id, field), text)| Some(((now.iter().position(|m| Some(m) == was.get(l))?, id, field), text)))
            .collect();
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
    let env = rustgis_engine::render::layout::series_env(layout, &app.session.project.maps);
    let m = app.session.project.maps.iter_mut().find(|m| m.name.eq_ignore_ascii_case(map))?;
    let cam = frame_camera(m, *center, *scale, *bearing, *pitch);
    let k = 1.0 + 2.0 * PAN_OVERSCAN;
    let (w, h) = (e.w * PT_PER_MM * k, e.h * PT_PER_MM * k);
    let screen = f64::from(s) / PT_PER_MM * f64::from(ctx.pixels_per_point());
    // Within the pixel budget, and within the longest side the GPU takes for a texture (a long,
    // thin frame could pass the first and not the second).
    let side = (ctx.input(|i| i.max_texture_side) as f64).min(PAN_PREVIEW_SIDE);
    let ppp = screen.min((PAN_PREVIEW_PIXELS / (w * h).max(1.0)).sqrt()).min(side / w.max(h).max(1.0)).max(0.01);
    let pm = rustgis_engine::render::layout::frame_map_preview_with(m, cam, w, h, ppp, &env)?;
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
        self.text_warning = None;
        self.text_losses = None;
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

/// The layout view: the active layout's page with its elements, rulers and guides, drawn from a
/// texture rendered at screen resolution and redrawn when the layout changes. Clicks, drags and
/// keys go through `interact`; the selected element's right-click menu is `element_menu`.
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
    // The right-click menu of the selected element: a right-click on empty page cleared the
    // selection, so none opens there, nor while a drawing tool is picked. Before the page is
    // drawn, as its rows change the layout.
    if app.layout.tool.is_none()
        && let Some(id) = app.layout.selected.filter(|&id| layout.element(id).is_some())
    {
        resp.context_menu(|ui| element_menu(app, ui, &layout, li, id));
    }
    // The properties, a drag or the menu may have just changed the layout: draw the page as it is
    // now, or the texture would be keyed to the new revision but show the old layout (#325).
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
        let (drawn, said) = rustgis_engine::render::shadows_said(|| {
            rustgis_engine::render::layout::render_pending(&layout, &mut app.session.project.maps, dpi, &info)
        });
        if let Ok((pm, pending)) = drawn {
            let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
            match &mut app.layout.texture {
                Some(tex) => tex.set(img, egui::TextureOptions::LINEAR),
                None => app.layout.texture = Some(ui.ctx().load_texture("layout", img, egui::TextureOptions::LINEAR)),
            }
            app.layout.key = Some(key);
            // Natively a frame's tiles were waited for, so drawing it again would wait again; only
            // cast shadows still being worked out (`said.to_come`) come back as they land. Legend
            // pictures are never waited for here, on any platform: `pending.legends` draws the page
            // again once they have all landed (below).
            let web = cfg!(target_arch = "wasm32");
            let frames = (web && pending.frames) || said.to_come;
            say_frame_shadows(app, &said);
            let losses = rustgis_engine::render::layout::text_losses(&layout, &app.session.project.maps, dpi, &info);
            let clipped = losses.iter().map(|l| (l.id, l.name.clone())).collect::<Vec<_>>();
            let note = rustgis_engine::render::layout::clipped_text_notice(&layout, &clipped);
            if note != app.layout.text_warning {
                if let Some(note) = &note {
                    app.session.warn(note.clone());
                }
                app.layout.text_warning = note;
            }
            // The Format pane is drawn before the page: a count that changed shows on the next
            // frame, without waiting for an input event.
            let losses = Some((li, losses));
            if app.layout.text_losses != losses {
                app.layout.text_losses = losses;
                ui.ctx().request_repaint();
            }
            // Waiting for the frames draws it again anyway: the legends are asked about once
            // they are whole.
            let legends = !frames && pending.legends;
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
    // The page's colour (#2855), shown until the page's texture is drawn over it.
    painter.rect_filled(page, 0.0, layout.paper.map_or(Color32::WHITE, crate::theme::c32));
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
        && let Some((dragged, r, sx, sy)) = drag_landing(&app.layout, ui, &layout, s, to_mm(page, s, p))
    {
        if let Some(x) = sx {
            vline(to_screen(page, s, (x, 0.0)).x, SNAP_COLOR);
        }
        if let Some(y) = sy {
            hline(to_screen(page, s, (0.0, y)).y, SNAP_COLOR);
        }
        // Turned as the element being dragged is.
        let rot = layout.element(dragged).map_or(0.0, |e| e.rotation());
        painter.add(egui::Shape::closed_line(turned_corners(page, s, r, rot).to_vec(), Stroke::new(1.5, accent)));
    }
    if let Some(e) = app.layout.selected.and_then(|id| layout.element(id)) {
        let r = Rect::from_min_max(to_screen(page, s, (e.x, e.y)), to_screen(page, s, (e.x + e.w, e.y + e.h)));
        // A turned text's box turned with it.
        let [lt, rt, rb, lb] = turned_corners(page, s, (e.x, e.y, e.w, e.h), e.rotation());
        painter.extend(egui::Shape::dashed_line(&[lt, rt, rb, lb, lt], Stroke::new(1.2, accent), 5.0, 3.0));
        // The handle under the pointer, or being dragged, is filled.
        let active = match app.layout.drag {
            Some(Drag::Resize(id, h, _)) if id == e.id => Some(h),
            None => pointer.and_then(|p| handle_at(app, &layout, page, s, p)).map(|(_, h)| h),
            _ => None,
        };
        if e.locked {
            // No handles: a lock at the top-right corner says why.
            crate::icons::paint(&painter, Rect::from_center_size(rt, vec2(16.0, 16.0)), "lock", accent, accent);
        } else {
            for (i, h) in handles(r).map(|h| screen_turn(h, r.center(), e.rotation())).into_iter().enumerate() {
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
        // The name over the turned box's topmost corner.
        let top = [rt, rb, lb].into_iter().fold(lt, |a, p| if p.y < a.y - 0.5 { p } else { a });
        painter.text(top + vec2(0.0, -4.0), Align2::LEFT_BOTTOM, &e.name, regular(11.0), accent);
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
            ("title", ElementKind::Chart { title, .. }) => title.clone(),
            ("unitLabel", ElementKind::ScaleBar { unit_label, .. }) => Some(unit_label.clone().unwrap_or_default()),
            _ => None,
        });
        if current.is_some_and(|c| c != text) {
            // An emptied unit label gives the unit's abbreviation back, as the field does.
            let value = if field == "unitLabel" && text.is_empty() { Value::Null } else { json!(text) };
            let _ = app.run("layout.update", json!({"layout": l, "element": id, field: value}));
        }
    }
}

/// Saves all text being typed, the field the Format pane shows included: for a command that
/// draws the page before the pane runs this frame (the ribbon is drawn first).
pub(crate) fn commit_drafts(app: &mut RustGisApp) {
    app.layout.drawn = None;
    save_drafts(app);
}
