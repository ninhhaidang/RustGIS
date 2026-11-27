//! Draws a page layout: the page, its map frames (each a map rendered at the frame's scale and
//! clipped to its shape, with grids and extent indicators), and the surrounds.

use std::collections::BTreeMap;

use crate::Canvas;
use rustgis_data::GeometryType;
use rustgis_data::query::{Env, SeriesPage};
use rustgis_geom::{Coord, Extent};
use rustgis_map::layout::{
    ArrowStyle, ChartLook, ColorScale, ElementKind, FrameShape, GRID_LABEL_PT, Grid, GridKind, INK, Layout, LineHead, MAX_LEGEND_COLUMNS,
    Orientation, PT_PER_MM, SCALE_BAR_TEXT_PT, ScaleBarFit, ScaleBarStyle, ScaleLabelPosition, ScaleUnits, TextAlign, TextCase, TextVAlign,
    TickSide, UnitLabelPosition, frame_camera, layout_text_scale,
};
use rustgis_map::symbology::LegendEntry;
use rustgis_map::{Map, Rgba};
use tiny_skia::{FillRule, Mask, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke, Transform};

use crate::{View, paint, point_marks, text};

mod chart;
mod colorbar;
mod coverage;
mod dynamic_text;
mod frame;
mod grid;
mod legend;
mod losses;
mod north;
mod scale_bar;
mod swatch;
mod table;
mod textbox;

pub use chart::*;
pub use colorbar::*;
pub use coverage::*;
pub use dynamic_text::*;
pub use frame::*;
pub use grid::*;
pub use legend::*;
pub use losses::*;
use north::*;
pub use scale_bar::*;
use swatch::*;
use table::*;
pub use textbox::*;

/// Page values for dynamic text.
#[derive(Clone, Debug, Default)]
pub struct PageInfo {
    /// 1-based page number and count (a map series has many).
    pub page: usize,
    pub pages: usize,
    /// The map series page's name.
    pub page_name: String,
}

/// Takes the series page's expression values before any frame changes a streamed layer's
/// drawing window. An ordinary layout or a missing index row has no page variables.
pub fn series_env(layout: &Layout, maps: &[Map]) -> Env {
    let page = || {
        let row = layout.series.as_ref()?.page;
        let (layer, field) = layout.series_index(maps)?;
        Some(std::sync::Arc::new(SeriesPage {
            feature_id: layer.dataset.features.get(row)?.id,
            name: rustgis_map::layout::series_page_name(layer, field, row),
            number: row + 1,
        }))
    };
    Env { page: page(), ..Env::default() }
}

/// The environment a map frame's features are styled in on the page: `env` (the series page) at
/// the scale the frame draws at, its map's ground resolution in screen pixels.
pub fn frame_style_env(map: &Map, camera: &rustgis_map::Camera, env: &Env) -> Env {
    env.at_scale(Some(map.ground_res_at(camera) / rustgis_geom::PIXEL_M))
}

/// Map frame `frame`'s map and the environment its features are styled in on `layout`'s current
/// page ([`series_env`] at [`frame_style_env`]'s scale): what a chart coloured by classes reads its
/// bars' colours in, so what is said of them matches what is drawn.
pub fn frame_page_env<'a>(layout: &Layout, maps: &'a [Map], frame: u32) -> Option<(&'a Map, Env)> {
    let ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } = &layout.element(frame)?.kind else { return None };
    let m = maps.get(map_index(maps, map)?)?;
    let camera = frame_camera(m, *center, *scale, *bearing, *pitch);
    Some((m, frame_style_env(m, &camera, &series_env(layout, maps))))
}

/// Largest page drawn, in pixels per side and in all (less in the browser, where memory is
/// tighter): a larger page is drawn at a lower resolution.
pub const MAX_SIDE: f64 = 12_000.0;
pub(crate) const MAX_PIXELS: f64 = if cfg!(target_arch = "wasm32") { 25_000_000.0 } else { 60_000_000.0 };

/// Whether skipping a failed frame would omit required complete-source content.
pub(crate) fn frame_requires_complete(map: &Map, view: &View) -> bool {
    let mut scales = vec![map.ground_res_at(&view.camera) / rustgis_geom::PIXEL_M];
    if view.camera.pitch != 0.0 && !map.projection.is_globe() {
        scales.extend(view.camera.tilt_bands(view.width, view.height).iter().map(|b| map.ground_res_at(&b.camera) / rustgis_geom::PIXEL_M));
    }
    let complete = |source: &rustgis_map::source::SourceLayer| {
        source.chunk_source().is_some() || (!source.is_open() && crate::is_local_shapefile(source))
    };
    map.layers.iter().filter(|l| scales.iter().any(|scale| map.draws_at(l, *scale))).any(|layer| {
        layer.source.as_ref().is_some_and(complete)
            || layer
                .mask
                .as_deref()
                .and_then(|name| {
                    map.layers.iter().find(|m| m.name.eq_ignore_ascii_case(name) && m.geometry_type() == GeometryType::Polygon)
                })
                .and_then(|mask| mask.source.as_ref())
                .is_some_and(complete)
    })
}

/// Only maps placed in visible frames participate in this page's archive revision.
pub fn archive_revisions(layout: &Layout, maps: &[Map]) -> crate::ArchiveRevision {
    crate::archive_revisions(layout.elements.iter().filter_map(|e| {
        if !e.visible {
            return None;
        }
        let ElementKind::MapFrame { map, .. } = &e.kind else { return None };
        map_index(maps, map).and_then(|i| maps.get(i))
    }))
}

/// Renders a layout at `dpi` (dots per inch). `maps` are the project's maps, found by name.
/// Refuses a linked archive changed while composing the page, including its different map frames.
pub fn render(layout: &Layout, maps: &mut [Map], dpi: f64, info: &PageInfo) -> Result<Pixmap, String> {
    render_page(layout, maps, dpi, info, true, None).map(|(pm, _)| pm)
}

/// [`render`] of the part of the page in `region` (`[x, y, w, h]` in millimetres from its top
/// left, cut to the page), drawn at `dpi` rather than cut out of a whole page drawn smaller: a
/// closer look at small text. Its scale is the region's own, lowered only when the region's
/// picture passes the page budget ([`MAX_SIDE`], `MAX_PIXELS`), not the whole page's. A frame the
/// region meets is drawn whole and placed, as on the page; one it doesn't meet isn't drawn (its
/// view still serves legends and extent indicators), unless a table or chart reads it. The caller checks the region meets the page.
pub fn render_region(layout: &Layout, maps: &mut [Map], dpi: f64, info: &PageInfo, region: [f64; 4]) -> Result<Pixmap, String> {
    // Tables and charts read their frame's features as its drawing reads them (a streamed
    // layer holds the view drawn last), so the frames they show stay drawn.
    let read: Vec<u32> = layout
        .elements
        .iter()
        .filter(|e| e.visible)
        .filter_map(|e| match &e.kind {
            ElementKind::Table { frame, .. } | ElementKind::Chart { frame, .. } => Some(*frame),
            _ => None,
        })
        .collect();
    let mut part = layout.clone();
    for e in &mut part.elements {
        // With room for its border and grid labels.
        if matches!(e.kind, ElementKind::MapFrame { .. }) && !read.contains(&e.id) && !meets(e, region, 10.0) {
            e.visible = false;
        }
    }
    render_page(&part, maps, dpi, info, true, Some(region)).map(|(pm, _)| pm)
}

/// Whether element `e`'s box, grown by `margin` millimetres, meets `region` (`[x, y, w, h]` mm).
pub fn meets(e: &rustgis_map::layout::Element, [x, y, w, h]: [f64; 4], margin: f64) -> bool {
    e.x - margin < x + w && x < e.x + e.w + margin && e.y - margin < y + h && y < e.y + e.h + margin
}

/// What a page actually drew without, kept separately so legend-only waits redraw only on completion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PagePending {
    /// A frame drew without tiles, streamed data or cast shadows still on their way.
    pub frames: bool,
    /// A legend drew while one of its own pictures was still on its way.
    pub legends: bool,
}

/// [`render`], and whether a frame or legend was drawn while content was still on its way.
/// The Layout view never waits for legend pictures or cast shadows still being worked out.
/// Its caller redraws once the missing content lands, on desktop as well as in the browser.
pub fn render_pending(layout: &Layout, maps: &mut [Map], dpi: f64, info: &PageInfo) -> Result<(Pixmap, PagePending), String> {
    render_page(layout, maps, dpi, info, false, None)
}

/// [`render_pending`], its frames waiting for cast shadows when `wait_shadows` (an export), of
/// the part of the page in `region` alone when one is given ([`render_region`]).
fn render_page(
    layout: &Layout,
    maps: &mut [Map],
    dpi: f64,
    info: &PageInfo,
    wait_shadows: bool,
    region: Option<[f64; 4]>,
) -> Result<(Pixmap, PagePending), String> {
    let _budget = legend_budget();
    let revisions = archive_revisions(layout, maps);
    let mut source_error = None;
    let result = if wait_shadows {
        render_pending_inner(layout, maps, dpi, info, &mut source_error, true, region)
    } else {
        crate::without_shadow_wait(|| render_pending_inner(layout, maps, dpi, info, &mut source_error, false, region))
    };
    if wait_shadows {
        finish_legends()?;
    }
    revisions.finish_revision(archive_revisions(layout, maps), result, source_error.as_deref().unwrap_or("the page is too large to draw"))
}

fn render_pending_inner(
    layout: &Layout,
    maps: &mut [Map],
    dpi: f64,
    info: &PageInfo,
    source_error: &mut Option<String>,
    wait_legends: bool,
    region: Option<[f64; 4]>,
) -> Option<(Pixmap, PagePending)> {
    let (w, h, k) = page_geometry(layout, dpi);
    let (bounds, k) = match region {
        None => (tiny_skia::IntRect::from_xywh(0, 0, w, h)?, k),
        // In whole pixels, cut to the page, as an element's own bounds are, at the region's own
        // scale within the budget a page has.
        Some([x, y, rw, rh]) => {
            let k = PT_PER_MM * (if dpi.is_finite() { dpi } else { 96.0 } / 96.0).clamp(0.25, 8.0);
            let k = k.min((MAX_PIXELS / (rw * rh).max(1e-6)).sqrt()).min(MAX_SIDE / rw.max(rh).max(1e-6));
            let (pw, ph) = (layout.width * k, layout.height * k);
            let (x0, y0) = ((x * k).floor().clamp(0.0, pw), (y * k).floor().clamp(0.0, ph));
            let (x1, y1) = (((x + rw) * k).ceil().clamp(0.0, pw), ((y + rh) * k).ceil().clamp(0.0, ph));
            (tiny_skia::IntRect::from_xywh(x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32)?, k)
        }
    };
    let mut pending = PagePending::default();
    let env = series_env(layout, maps);
    let pm = draw_page(layout, maps, (info, &env), k, bounds, None, None, (&mut pending, wait_legends), source_error)?.into_pixmap();
    Some((pm, pending))
}

/// [`render`] kept as vectors (for a vector PDF): text, shapes, surrounds and flat, north-up map
/// frames as paths and glyphs, what is pixels by nature (rasters, tiles, pictures, a tilted or
/// globe frame) as pictures drawn at `dpi`, in the drawing's pixels at that resolution. With it,
/// the frames (id and name) drawn as pictures because their vectors passed
/// [`crate::vector::MAX_COST`]. A drawing that overflowed all the same (its surrounds) is
/// incomplete: [`crate::vector::Drawing::overflowed`] says so.
pub fn render_drawing(
    layout: &Layout,
    maps: &mut [Map],
    dpi: f64,
    info: &PageInfo,
) -> Result<(crate::vector::Drawing, Vec<(u32, String)>), String> {
    let _budget = legend_budget();
    let revisions = archive_revisions(layout, maps);
    let mut source_error = None;
    let result = render_drawing_inner(layout, maps, dpi, info, &mut source_error);
    finish_legends()?;
    revisions.finish_revision(archive_revisions(layout, maps), result, source_error.as_deref().unwrap_or("the page is too large to draw"))
}

fn render_drawing_inner(
    layout: &Layout,
    maps: &mut [Map],
    dpi: f64,
    info: &PageInfo,
    source_error: &mut Option<String>,
) -> Option<(crate::vector::Drawing, Vec<(u32, String)>)> {
    let (w, h, k) = page_geometry(layout, dpi);
    let bounds = tiny_skia::IntRect::from_xywh(0, 0, w, h)?;
    let mut pictured = Vec::new();
    let env = series_env(layout, maps);
    let drawing =
        draw_page(layout, maps, (info, &env), k, bounds, None, Some(&mut pictured), (&mut PagePending::default(), true), source_error)?
            .into_drawing()?;
    Some((drawing, pictured))
}

/// Page pixel dimensions and the shared millimetre scale, capped to the renderer's budget.
pub(crate) fn page_geometry(layout: &Layout, dpi: f64) -> (u32, u32, f64) {
    let ppp = (if dpi.is_finite() { dpi } else { 96.0 } / 96.0).clamp(0.25, 8.0);
    let k = PT_PER_MM * ppp;
    let k = k.min((MAX_PIXELS / (layout.width * layout.height).max(1.0)).sqrt());
    let (w, h) = ((layout.width * k).clamp(1.0, MAX_SIDE), (layout.height * k).clamp(1.0, MAX_SIDE));
    (w as u32, h as u32, k.min(w / layout.width).min(h / layout.height))
}

/// One element on transparency, allocating only its on-page pixel bounds.
/// Outer `None` means rendering failed; inner `None` means the element is off-page.
/// A fatal source error is retained for the composed SVG's revision check and error report.
pub(crate) fn render_element(
    layout: &Layout,
    id: u32,
    maps: &mut [Map],
    dpi: f64,
    page: (&PageInfo, &Env),
    source_error: &mut Option<String>,
) -> Option<Option<(Pixmap, [f64; 4])>> {
    let Some((bounds, k, mm)) = element_bounds(layout, id, dpi)? else { return Some(None) };
    let pm = draw_page(layout, maps, page, k, bounds, Some(id), None, (&mut PagePending::default(), true), source_error)?.into_pixmap();
    Some(Some((pm, mm)))
}

/// [`render_element`] kept as vectors (see [`render_drawing`]): the element alone, recorded in
/// the pixels of its on-page bounds at `dpi`, those bounds in millimetres, and the pixels a
/// millimetre. The frames drawn as pictures (past the budget) go to `pictured`.
pub(crate) fn record_element(
    layout: &Layout,
    id: u32,
    maps: &mut [Map],
    dpi: f64,
    page: (&PageInfo, &Env),
    pictured: &mut Vec<(u32, String)>,
    source_error: &mut Option<String>,
) -> Option<Option<(crate::vector::Drawing, [f64; 4], f64)>> {
    let Some((bounds, k, mm)) = element_bounds(layout, id, dpi)? else { return Some(None) };
    let drawing = draw_page(layout, maps, page, k, bounds, Some(id), Some(pictured), (&mut PagePending::default(), true), source_error)?
        .into_drawing()?;
    Some(Some((drawing, mm, k)))
}

/// Element `id`'s on-page bounds in whole pixels at `dpi`, the pixels a millimetre, and the bounds
/// in millimetres; inner `None` for an element off the page.
fn element_bounds(layout: &Layout, id: u32, dpi: f64) -> Option<Option<(tiny_skia::IntRect, f64, [f64; 4])>> {
    let e = layout.element(id)?;
    let (w, h, k) = page_geometry(layout, dpi);
    let x0 = (e.x * k).floor().clamp(0.0, f64::from(w));
    let y0 = (e.y * k).floor().clamp(0.0, f64::from(h));
    let x1 = ((e.x + e.w) * k).ceil().clamp(0.0, f64::from(w));
    let y1 = ((e.y + e.h) * k).ceil().clamp(0.0, f64::from(h));
    let bounds = tiny_skia::IntRect::from_xywh(x0 as i32, y0 as i32, (x1 - x0) as u32, (y1 - y0) as u32);
    Some(bounds.map(|b| (b, k, [x0 / k, y0 / k, (x1 - x0) / k, (y1 - y0) / k])))
}

/// Draws the selected region with the full layout's frame and dynamic-text context, recorded
/// when `record` holds a list (see [`render_drawing`]), which gets the names of the frames
/// drawn as pictures. `pending` records the frames and legends drawn without content still on
/// its way (see [`render_pending`]); its flag says whether legend pictures are waited for: the
/// interactive page never waits for them, exports and other pages wait, as they always did.
/// A source read that stops the page supplies its named error; the caller checks the page's
/// archive revision before reporting it.
fn draw_page(
    layout: &Layout,
    maps: &mut [Map],
    (info, env): (&PageInfo, &Env),
    k: f64,
    bounds: tiny_skia::IntRect,
    element: Option<u32>,
    mut record: Option<&mut Vec<(u32, String)>>,
    (pending, wait_legends): (&mut PagePending, bool),
    source_error: &mut Option<String>,
) -> Option<Canvas> {
    let ppp = k / PT_PER_MM;
    let mut pm =
        if record.is_some() { Canvas::recording(bounds.width(), bounds.height())? } else { Canvas::new(bounds.width(), bounds.height())? };
    if element.is_none() {
        // The page's colour (#2855), white unless given.
        let paper = layout.paper.unwrap_or(Rgba::rgb(0xFF, 0xFF, 0xFF));
        pm.fill(tiny_skia::Color::from_rgba8(paper.r(), paper.g(), paper.b(), 255));
    }
    let visible = |e: &&rustgis_map::layout::Element| e.visible && element.is_none_or(|id| e.id == id);
    // Frame cameras first: surrounds and extent indicators read them.
    let frames: Vec<(u32, usize, rustgis_map::Camera, f64, f64)> = layout
        .elements
        .iter()
        .filter_map(|e| match &e.kind {
            ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } => {
                let mi = map_index(maps, map)?;
                let m = maps.get(mi)?;
                Some((e.id, mi, frame_camera(m, *center, *scale, *bearing, *pitch), e.w * PT_PER_MM, e.h * PT_PER_MM))
            }
            _ => None,
        })
        .collect();
    let frame = |id: u32| frames.iter().find(|f| f.0 == id);
    // What each legend's frame draws, worked out before any element is drawn: its map is synced
    // to that frame's view first, so a streamed layer holds that frame's features whichever order
    // the frames and legends come in, and whichever other frame showed the map last.
    let mut shown = Vec::new();
    for e in layout.elements.iter().filter(visible) {
        if let ElementKind::Legend { frame: f, all_classes: false, show_no_data, .. } = &e.kind
            && !shown.iter().any(|(id, no_data, _)| id == f && no_data == show_no_data)
            && let Some(&(_, mi, cam, fw, fh)) = frame(*f)
            && let Some(map) = maps.get_mut(mi)
        {
            let view = View { width: fw, height: fh, pixels_per_point: 1.0, camera: cam, sky: false };
            let lonlat = crate::view_lonlat(map, &view);
            let own = crate::view_own(map, &view);
            let scale = map.ground_res_at(&cam) / rustgis_geom::PIXEL_M;
            map.sync_sources_in(&lonlat, own.as_ref(), scale, true, env);
            let mut failed_source = None;
            let Some(entries) = complete_shown_legends(map, scale, *show_no_data, &mut failed_source, env) else {
                *source_error = failed_source_error(map, failed_source);
                return None;
            };
            shown.push((*f, *show_no_data, entries));
        }
    }
    for e in layout.elements.iter().filter(visible) {
        // Each element is a layer of the recording, named after it (#1815) and identified by its
        // id: the same layer on every page of a series, whatever other elements draw there.
        pm.layer_with_id(if e.name.trim().is_empty() { e.kind.code() } else { &e.name }, u64::from(e.id));
        // Its background under its content (#2257), on the whole page only: an element drawn
        // alone is an SVG's picture of it, whose background the SVG writes as a shape of its own.
        if element.is_none() {
            draw_background(&mut pm, e, k, (f64::from(bounds.x()), f64::from(bounds.y())));
        }
        let (x, y, ew, eh) =
            ((e.x * k - f64::from(bounds.x())) as f32, (e.y * k - f64::from(bounds.y())) as f32, (e.w * k) as f32, (e.h * k) as f32);
        let Some(r) = Rect::from_xywh(x, y, ew.max(1.0), eh.max(1.0)) else { continue };
        match &e.kind {
            ElementKind::MapFrame { shape, grid, extent_of, extent_color, extent_width, border_color, border_width, .. } => {
                let Some(&(_, mi, cam, fw, fh)) = frame(e.id) else {
                    placeholder(&mut pm, r, "Missing map", ppp);
                    continue;
                };
                let Some(map) = maps.get_mut(mi) else { continue };
                // A frame larger than the page budget is drawn at a lower resolution and scaled up.
                let fppp = ppp.min((MAX_PIXELS / (fw * fh).max(1.0)).sqrt()).max(0.01);
                let view = View { width: fw, height: fh, pixels_per_point: fppp, camera: cam, sky: false };
                // The grid is drawn with the frame's map, before its attribution, which keeps
                // clear of the grid's labels.
                let grid = grid.as_ref().map(|g| (g, layout.decimal_comma));
                let mut failed_source = None;
                let Some(mut frame_pm) = frame_canvas(map, &view, pm.is_recording(), &mut pending.frames, grid, &mut failed_source, env)
                else {
                    if frame_requires_complete(map, &view) {
                        *source_error = failed_source_error(map, failed_source);
                        return None;
                    }
                    // Preserve upstream's best-effort page export for ordinary maps.
                    continue;
                };
                // A frame whose vectors grew past the budget, or would take the page past it, is
                // drawn as a picture instead.
                let too_large =
                    frame_pm.overflowed() || pm.recorded_cost().saturating_add(frame_pm.recorded_cost()) > crate::vector::max_cost();
                if frame_pm.is_recording()
                    && too_large
                    && let Some(pixels) = frame_canvas(map, &view, false, &mut pending.frames, grid, &mut None, env)
                {
                    frame_pm = pixels;
                    if let Some(list) = record.as_deref_mut() {
                        list.push((e.id, e.name.clone()));
                    }
                }
                if let Some(other) = extent_of.and_then(frame)
                    && let Some(om) = maps.get(other.1)
                {
                    let ring = frame_outline_lonlat(om, &other.2, other.3, other.4);
                    if let Some(map) = maps.get(mi) {
                        let from_globe = om.projection.is_globe();
                        outline_lonlat(&mut frame_pm, map, &view, &ring, from_globe, *extent_color, *extent_width as f32);
                    }
                }
                // Whole pixels, so the clipped image and its outline line up.
                let (x0, y0) = (x.round(), y.round());
                let r = Rect::from_xywh(x0, y0, ew.max(1.0), eh.max(1.0)).unwrap_or(r);
                let path = shape_path(shape, r);
                let up = (ppp / fppp) as f32;
                // Shaped frames are clipped by a mask of the frame's own size.
                if !matches!(shape, FrameShape::Rectangle)
                    && !frame_pm.is_recording()
                    && let Some(p) =
                        path.as_ref().and_then(|p| p.clone().transform(Transform::from_translate(-x0, -y0).post_scale(1.0 / up, 1.0 / up)))
                    && let Some(mut m) = Mask::new(frame_pm.width(), frame_pm.height())
                {
                    m.fill_path(&p, FillRule::Winding, true, Transform::identity());
                    frame_pm.pixels().apply_mask(&m);
                }
                let place = Transform::from_row(up, 0.0, 0.0, up, x0, y0);
                if frame_pm.is_recording() {
                    // Kept as vectors, cut to the frame (its paths reach past it).
                    let clip =
                        path.clone().map(|p| crate::vector::Clip { path: p, rule: FillRule::Winding, transform: Transform::identity() });
                    pm.place(frame_pm, place, 1.0, rustgis_map::effects::Blend::Normal, clip);
                } else {
                    pm.draw_pixmap(
                        0,
                        0,
                        frame_pm.pixmap().as_ref(),
                        &PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() },
                        place,
                        None,
                    );
                }
                // Its border along its shape (#2557); a width of 0 or a clear colour draws none.
                if let Some(p) = &path
                    && *border_width > 0.0
                    && border_color.0[3] > 0
                {
                    pm.stroke_path(
                        p,
                        &paint(*border_color, 1.0),
                        &Stroke { width: *border_width as f32 * ppp as f32, ..Stroke::default() },
                        Transform::identity(),
                        None,
                    );
                }
            }
            ElementKind::NorthArrow { frame: f, style, fill, outline } => {
                // Turned with its frame's map.
                let bearing = frame(*f).map_or(0.0, |fr| fr.2.bearing);
                north_arrow(&mut pm, r, *style, *fill, *outline, ppp, bearing)
            }
            ElementKind::ScaleBar { frame: f, color, fill, .. } => {
                if let Some(&(_, mi, cam, _, _)) = frame(*f)
                    && let Some(map) = maps.get(mi)
                    && let Some(bar) = BarOptions::of(&e.kind)
                {
                    scale_bar(&mut pm, r, map.ground_res_at(&cam), &bar, (*color, *fill), ppp, layout.decimal_comma);
                }
            }
            ElementKind::Legend { frame: f, title, all_classes, hide_other, show_no_data, exclude, columns, labels, color, text_size } => {
                if let Some(&(_, mi, cam, ..)) = frame(*f)
                    && let Some(map) = maps.get(mi)
                {
                    let drawn = (!*all_classes).then(|| {
                        shown
                            .iter()
                            .find(|(id, no_data, _)| id == f && no_data == show_no_data)
                            .map(|(_, _, v)| v.as_slice())
                            .unwrap_or(&[])
                    });
                    let scale = map.ground_res_at(&cam) / rustgis_geom::PIXEL_M;
                    // A legend wholly off the canvas (off the page, or outside the element asked
                    // for) shows none of its pictures: nothing to wait for or to be missing.
                    let on_canvas = element.is_some() || on_page(layout, e);
                    // The export's legend clock starts at the first picture actually on its way,
                    // not at the first legend drawn: a ready legend leaves the budget untouched.
                    if wait_legends && on_canvas && tile_legends_pending(map, scale, exclude) {
                        wait_for_tile_legends(map, scale, exclude, legend_deadline())?;
                    }
                    let size = legend_size(*text_size);
                    let opts = LegendOptions { show_no_data: *show_no_data, hide_other: *hide_other, exclude, labels, scale, size };
                    // Whether a picture was still on its way when the rows were made: the very lookup
                    // that drew the legend, not an earlier one a retry or an eviction could outdate.
                    let ink = color.unwrap_or(INK);
                    let missing = legend_missing(&mut pm, r, map, (title, *columns), drawn, opts, ink, ppp) && on_canvas;
                    pending.legends |= missing;
                    // An export's page drawn without a picture stays incomplete, whenever the
                    // picture lands after (see `finish_legends`).
                    if missing && wait_legends && LEGEND_BUDGET.get().is_some() {
                        LEGEND_MISSING.set(true);
                    }
                }
            }
            ElementKind::Colorbar { frame: f, layer, title, orientation, ticks, tick_side, reverse, color } => {
                let scale = frame(*f)
                    .and_then(|fr| maps.get(fr.1))
                    .and_then(|m| rustgis_map::layout::colorbar_layer(m, layer.as_deref()))
                    .and_then(|l| l.raster.as_ref())
                    .and_then(rustgis_map::layout::color_scale);
                match scale {
                    Some(sc) => colorbar(
                        &mut pm,
                        r,
                        &sc,
                        &Bar {
                            title,
                            orientation: *orientation,
                            ticks: *ticks,
                            side: *tick_side,
                            reverse: *reverse,
                            comma: layout.decimal_comma,
                            ink: color.unwrap_or(INK),
                        },
                        ppp,
                    ),
                    None => placeholder(&mut pm, r, "Colorbar", ppp),
                }
            }
            ElementKind::Text {
                text: t,
                size,
                color,
                bold,
                italic,
                align,
                letter_spacing,
                font,
                font_name,
                rotation,
                wrap,
                valign,
                line_spacing,
                case,
                ..
            } => {
                let first = layout.first_frame().and_then(frame);
                let style =
                    text_style(*bold, *italic, *font, font_name.as_deref(), *align, *letter_spacing).with_block(*valign, *line_spacing);
                // Its tokens filled in, then its case, then its wrap ([`drawn_text`]).
                let s = expand(t, layout, info, first.and_then(|f| maps.get(f.1).map(|m| (m, f.2))));
                let s = drawn_text(s, *case, *wrap, e.w, *size, style);
                if *rotation == 0.0 {
                    text_block(&mut pm, r, &s, *size as f32, *color, style, ppp);
                } else if let Some(area) = text_area(r, &s, *size as f32, style, ppp) {
                    draw_turned_box(&mut pm, r, *rotation as f32, area, |c, r, k| {
                        text_block(c, r, &s, *size as f32, *color, style, ppp * k)
                    });
                }
            }
            ElementKind::Picture { image } => {
                if let Some(img) = crate::picture::decode(image) {
                    let (iw, ih) = (img.width() as f32, img.height() as f32);
                    let s = (ew / iw).min(eh / ih);
                    let (dw, dh) = (iw * s, ih * s);
                    let t = Transform::from_row(s, 0.0, 0.0, s, x + (ew - dw) / 2.0, y + (eh - dh) / 2.0);
                    pm.draw_pixmap(
                        0,
                        0,
                        img.as_ref(),
                        &PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() },
                        t,
                        None,
                    );
                } else {
                    placeholder(&mut pm, r, "Picture", ppp);
                }
            }
            ElementKind::Table { frame: f, layer, rows, color } => {
                let ds = frame(*f).and_then(|fr| maps.get(fr.1)).and_then(|m| m.layer_by_name(layer)).map(|l| &l.dataset);
                match ds {
                    Some(ds) => table(&mut pm, r, ds, *rows, *color, ppp),
                    None => placeholder(&mut pm, r, "Table", ppp),
                }
            }
            ElementKind::Chart { frame: f, layer, color, .. } => {
                // Coloured by classes, its bars read their features' colours at the frame's scale
                // and on this page, as the frame draws them.
                let data = frame(*f).and_then(|&(_, mi, cam, _, _)| {
                    let m = maps.get(mi)?;
                    e.kind.chart_data_of(m.layer_by_name(layer)?, &frame_style_env(m, &cam, env))
                });
                match data {
                    Some(data) => {
                        let look = e.kind.chart_look().unwrap_or_default();
                        chart(&mut pm, r, &data, &look, color.unwrap_or(INK), ppp, layout.decimal_comma)
                    }
                    None => placeholder(&mut pm, r, "Chart", ppp),
                }
            }
            ElementKind::Rectangle { fill, outline, width, radius, dash } => {
                // Square corners filled as before, so an existing layout draws to the same pixels.
                let rounded = rounded_rect(r, (*radius * PT_PER_MM * ppp) as f32);
                match &rounded {
                    Some(p) => pm.fill_path(p, &paint(*fill, 1.0), FillRule::Winding, Transform::identity(), None),
                    None => pm.fill_rect(r, &paint(*fill, 1.0), Transform::identity(), None),
                }
                let p = rounded.unwrap_or_else(|| PathBuilder::from_rect(r));
                if *width > 0.0 {
                    let stroke = dashed(Stroke { width: (*width as f32) * ppp as f32, ..Stroke::default() }, dash.as_ref(), ppp, &p);
                    pm.stroke_path(&p, &paint(*outline, 1.0), &stroke, Transform::identity(), None);
                }
            }
            ElementKind::Ellipse { fill, outline, width, dash } => {
                if let Some(p) = PathBuilder::from_oval(r) {
                    pm.fill_path(&p, &paint(*fill, 1.0), FillRule::Winding, Transform::identity(), None);
                    if *width > 0.0 {
                        let stroke = dashed(Stroke { width: (*width as f32) * ppp as f32, ..Stroke::default() }, dash.as_ref(), ppp, &p);
                        pm.stroke_path(&p, &paint(*outline, 1.0), &stroke, Transform::identity(), None);
                    }
                }
            }
            ElementKind::Line { color, width, dir, head, dash } => {
                let [(x0, y0), (x1, y1)] = dir.ends((r.x().into(), r.y().into(), r.width().into(), r.height().into()));
                let (a, b) = ((x0 as f32, y0 as f32), (x1 as f32, y1 as f32));
                let stroke_w = ((*width as f32) * ppp as f32).max(0.5);
                let (at_start, at_end) = (matches!(head, LineHead::Start | LineHead::Both), matches!(head, LineHead::End | LineHead::Both));
                let len = (b.0 - a.0).hypot(b.1 - a.1);
                // A head grows with the line's width, but never past 40% of the line.
                let heads = u8::from(at_start) + u8::from(at_end);
                let head_len = if heads == 0 || len <= 0.0 {
                    0.0
                } else {
                    ((6.0 + 2.5 * *width as f32) * ppp as f32).min(len * 0.4 / f32::from(heads))
                };
                let (ux, uy) = if len > 0.0 { ((b.0 - a.0) / len, (b.1 - a.1) / len) } else { (1.0, 0.0) };
                // The shaft stops inside each head, so its square end doesn't poke through the tip.
                let inset = head_len * 0.8;
                let (s0, s1) = (
                    if at_start { (a.0 + ux * inset, a.1 + uy * inset) } else { a },
                    if at_end { (b.0 - ux * inset, b.1 - uy * inset) } else { b },
                );
                let mut pb = PathBuilder::new();
                pb.move_to(s0.0, s0.1);
                pb.line_to(s1.0, s1.1);
                if let Some(p) = pb.finish() {
                    // Only the shaft is dashed: the heads below are filled.
                    let stroke = dashed(Stroke { width: stroke_w, ..Stroke::default() }, dash.as_ref(), ppp, &p);
                    pm.stroke_path(&p, &paint(*color, 1.0), &stroke, Transform::identity(), None);
                }
                // Each head: a filled triangle, its tip on the line's end.
                let half = (head_len * 0.45).max(stroke_w);
                for (tip, (dx, dy)) in [(at_start, a, (-ux, -uy)), (at_end, b, (ux, uy))].into_iter().filter(|h| h.0).map(|h| (h.1, h.2)) {
                    let base = (tip.0 - dx * head_len, tip.1 - dy * head_len);
                    let mut pb = PathBuilder::new();
                    pb.move_to(tip.0, tip.1);
                    pb.line_to(base.0 - dy * half, base.1 + dx * half);
                    pb.line_to(base.0 + dy * half, base.1 - dx * half);
                    pb.close();
                    if let Some(p) = pb.finish() {
                        pm.fill_path(&p, &paint(*color, 1.0), FillRule::Winding, Transform::identity(), None);
                    }
                }
            }
        }
    }
    pm.layer(None);
    Some(pm)
}

/// `stroke` dashed along `path` by an element's pattern (points, at `ppp` pixels a point), as a
/// line layer's stroke is ([`crate::dash_steps`]): solid when it has none, or when the pattern is
/// under a pixel a round or would cut the path into too many dashes.
fn dashed(stroke: Stroke, dash: Option<&rustgis_map::symbology::Dash>, ppp: f64, path: &tiny_skia::Path) -> Stroke {
    let dash = dash.and_then(|d| crate::dash_steps(d, ppp as f32, path)).and_then(|steps| tiny_skia::StrokeDash::new(steps, 0.0));
    Stroke { dash, ..stroke }
}

/// An element's background (#2257): its box grown by the padding, at `k` pixels a millimetre from
/// `origin` (pixels), filled, outlined when the outline has a width, its corners rounded, turned
/// about the element's centre with a turned text.
fn draw_background(pm: &mut Canvas, e: &rustgis_map::layout::Element, k: f64, origin: (f64, f64)) {
    let Some(b) = e.background else { return };
    let (bx, by, bw, bh) = e.background_box();
    let Some(r) =
        Rect::from_xywh((bx * k - origin.0) as f32, (by * k - origin.1) as f32, ((bw * k) as f32).max(1.0), ((bh * k) as f32).max(1.0))
    else {
        return;
    };
    let turn = Transform::from_rotate_at(e.rotation() as f32, r.x() + r.width() / 2.0, r.y() + r.height() / 2.0);
    let path = rounded_rect(r, (b.radius * k) as f32).unwrap_or_else(|| PathBuilder::from_rect(r));
    pm.fill_path(&path, &paint(b.fill, 1.0), FillRule::Winding, turn, None);
    if b.width > 0.0 {
        let stroke = Stroke { width: (b.width * k / PT_PER_MM) as f32, ..Stroke::default() };
        pm.stroke_path(&path, &paint(b.outline, 1.0), &stroke, turn, None);
    }
}

/// A rectangle with its corners rounded by `rad` pixels (at most half its shorter side), each a
/// quarter circle drawn as a cubic; `None` for square corners, drawn as the plain rectangle.
pub(crate) fn rounded_rect(r: tiny_skia::Rect, rad: f32) -> Option<tiny_skia::Path> {
    let rad = rad.min(r.width() / 2.0).min(r.height() / 2.0);
    if rad.is_nan() || rad <= 0.0 {
        return None;
    }
    // The control points' distance that makes a cubic a quarter circle.
    let c = rad * 0.552_284_8;
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    let mut pb = PathBuilder::new();
    pb.move_to(l + rad, t);
    pb.line_to(rt - rad, t);
    pb.cubic_to(rt - rad + c, t, rt, t + rad - c, rt, t + rad);
    pb.line_to(rt, b - rad);
    pb.cubic_to(rt, b - rad + c, rt - rad + c, b, rt - rad, b);
    pb.line_to(l + rad, b);
    pb.cubic_to(l + rad - c, b, l, b - rad + c, l, b - rad);
    pb.line_to(l, t + rad);
    pb.cubic_to(l, t + rad - c, l + rad - c, t, l + rad, t);
    pb.close();
    pb.finish()
}

/// A number as printed with a decimal comma when `comma` (`0.05` to `0,05`, `6,300,000` to
/// `6.300.000`): its points and commas swapped.
fn decimal(s: String, comma: bool) -> String {
    if !comma {
        return s;
    }
    s.chars()
        .map(|c| match c {
            '.' => ',',
            ',' => '.',
            c => c,
        })
        .collect()
}

/// A grey box with `label` in it, where a frame's map can't be drawn.
fn placeholder(pm: &mut Canvas, r: Rect, label: &str, ppp: f64) {
    pm.fill_rect(r, &paint(Rgba::rgb(0xF0, 0xF1, 0xF3), 1.0), Transform::identity(), None);
    let p = PathBuilder::from_rect(r);
    pm.stroke_path(
        &p,
        &paint(Rgba::rgb(0xA0, 0xA4, 0xAA), 1.0),
        &Stroke { width: ppp as f32, ..Stroke::default() },
        Transform::identity(),
        None,
    );
    text::text_at(
        pm,
        label,
        r.x() + r.width() / 2.0,
        r.y() + r.height() / 2.0,
        10.0 * 1.33 * ppp as f32,
        Rgba::rgb(0x70, 0x74, 0x7A),
        false,
        text::Align::Center,
        false,
    );
}

/// Fills `pts` with `fill` and outlines it with `outline`.
fn shape(pm: &mut Canvas, pts: &[(f32, f32)], fill: Rgba, outline: Rgba, width: f32) {
    let mut pb = PathBuilder::new();
    for (i, (x, y)) in pts.iter().enumerate() {
        if i == 0 { pb.move_to(*x, *y) } else { pb.line_to(*x, *y) }
    }
    pb.close();
    if let Some(p) = pb.finish() {
        pm.fill_path(&p, &paint(fill, 1.0), FillRule::Winding, Transform::identity(), None);
        pm.stroke_path(&p, &paint(outline, 1.0), &Stroke { width, ..Stroke::default() }, Transform::identity(), None);
    }
}

/// A box filled with `colour`; none where it has no size.
fn fill(pm: &mut Canvas, colour: Rgba, x: f32, y: f32, w: f32, h: f32) {
    if let Some(b) = Rect::from_xywh(x, y, w, h) {
        pm.fill_rect(b, &paint(colour, 1.0), Transform::identity(), None);
    }
}

/// The first case-insensitive frame-map name match; a missing reference renders no map.
pub fn map_index(maps: &[rustgis_map::Map], name: &str) -> Option<usize> {
    maps.iter().position(|map| map.name.eq_ignore_ascii_case(name))
}

/// Pages of one text each with none of the block settings, in the combinations main draws: each
/// alignment, wrapped or not, turned 0, 90, -90 or 33 degrees, spaced 0 or 3 points (bold, and
/// italic when turned 33), in a box that holds its lines and one that cuts them; and two texts
/// across the page's right and bottom edges.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn plain_text_pages() -> Vec<Layout> {
    let text = "Population counts\nfrom the 2020 census; boundaries simplified for print\nPage {page} of {pages}";
    let page = |at: (f64, f64, f64, f64), f: &dyn Fn(&mut ElementKind)| {
        let mut l = Layout::new("Plain", 120.0, 80.0);
        let mut kind = ElementKind::text(text, 11.0, Rgba::rgb(0, 0, 0), false);
        f(&mut kind);
        l.add("Note", at, kind);
        l
    };
    let mut pages = Vec::new();
    for align in TextAlign::ALL {
        for wrap in [false, true] {
            for turn in [0.0, 90.0, -90.0, 33.0] {
                for spacing in [0.0, 3.0] {
                    for h in [40.0, 9.0] {
                        pages.push(page((10.0, 10.0, 60.0, h), &|k| {
                            if let ElementKind::Text { align: a, wrap: w, rotation, letter_spacing, italic, bold, .. } = k {
                                (*a, *w, *rotation, *letter_spacing) = (align, wrap, turn, spacing);
                                (*italic, *bold) = (turn == 33.0, spacing > 0.0);
                            }
                        }));
                    }
                }
            }
        }
    }
    pages.push(page((90.0, 10.0, 60.0, 30.0), &|_| {}));
    pages.push(page((10.0, 74.0, 60.0, 30.0), &|_| {}));
    pages
}

#[expect(clippy::disallowed_methods, reason = "a thread the system refuses fails the test, as it should")]
#[cfg(test)]
mod tests;
