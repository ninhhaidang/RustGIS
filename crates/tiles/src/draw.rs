//! Drawing a tile layer into a pixmap in Web Mercator.
//!
//! The style's layers draw in order; each one draws from every tile of its source in view.
//! A tile that hasn't arrived yet is stood in for by the nearest ancestor already in the cache
//! (or, for raster tiles, its children), clipped to the missing tile's square.

use std::collections::HashMap;
use std::sync::Arc;

use rustgis_geom::Extent;
use tiny_skia::{
    FillRule, FilterQuality, LineCap, LineJoin, Paint, PathBuilder, Pattern, Pixmap, PixmapPaint, Rect, SpreadMode, Stroke, StrokeDash,
    Transform,
};

use crate::cache::{self, Payload, State, Want};
use crate::dem;
use crate::mvt::GeomKind;
use crate::source::{Resolve, Resolved, Spec, resolve};
use crate::sprite::{self, Load, Sprite};
use crate::style::{FeatureCtx, LayerKind, Rgba, Source, StyleLayer};
use crate::template::{self, Placed, TileId};

/// Where the pixmap is: the Web Mercator point at its top-left corner and the metres per
/// (device) pixel, with the device pixels per logical point.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub left: f64,
    pub top: f64,
    pub m_per_px: f64,
    pub ppp: f32,
}

impl Frame {
    fn px(&self, x: f64, y: f64) -> (f32, f32) {
        (((x - self.left) / self.m_per_px) as f32, ((self.top - y) / self.m_per_px) as f32)
    }
    pub(crate) fn extent(&self, w: u32, h: u32) -> Extent {
        Extent::new(self.left, self.top - f64::from(h) * self.m_per_px, self.left + f64::from(w) * self.m_per_px, self.top)
    }
}

pub use crate::symbols::{Collide, Label, LabelIcon};

/// What drawing a layer found.
#[derive(Debug, Default)]
pub struct Drawn {
    pub labels: Vec<Label>,
    pub attribution: Vec<String>,
    /// Some of it is still on its way: drawing again later shows more.
    pub pending: bool,
    pub error: Option<String>,
}

/// One tile's worth of drawing: the tile data, where that tile is, and the square it stands in for.
#[derive(Clone)]
pub(crate) struct Item {
    pub(crate) payload: Arc<Payload>,
    pub(crate) at: Extent,
    pub(crate) z: u8,
    pub(crate) clip: Extent,
}

/// What of a resolved style was left out, as a drawing's error: the style still draws.
fn left_out(r: &Resolved) -> Option<String> {
    (!r.left_out.is_empty()).then(|| r.left_out.join("; "))
}

/// Draws `spec` into `pm` at `opacity`. Labels are collected when `labels` is set.
pub fn draw(pm: &mut Pixmap, spec: &Spec, opacity: f32, frame: &Frame, labels: bool) -> Drawn {
    let mut out = Drawn::default();
    let r = match resolve(spec) {
        Resolve::Ready(r) => r,
        Resolve::Pending => {
            out.pending = true;
            return out;
        }
        Resolve::Failed(e) => {
            out.error = Some(e);
            return out;
        }
    };
    out.attribution = r.attribution.clone();
    out.error = left_out(&r);
    out.pending |= r.waiting;
    if !(frame.m_per_px.is_finite() && frame.m_per_px > 0.0) || opacity <= 0.0 {
        return out;
    }
    let (w, h) = (pm.width(), pm.height());
    // A style drawing vector shapes draws them from tile pictures drawn once (see `picture`):
    // a map moving across them only puts them in place.
    if let Some(pictures) = crate::picture::cover(&r, frame, &frame.extent(w, h), None, &mut out.pending) {
        for it in &pictures {
            draw_raster(pm, it, frame, opacity);
        }
        if labels {
            draw_layers(None, (w, h), &frame.extent(w, h), &r, frame, Pass::Labels, &mut out);
        }
        return out;
    }
    let mut layer_pm = if opacity < 0.999 { Pixmap::new(w, h) } else { None };
    let target: &mut Pixmap = match layer_pm.as_mut() {
        Some(p) => p,
        None => pm,
    };
    draw_layers(Some(target), (w, h), &frame.extent(w, h), &r, frame, if labels { Pass::All } else { Pass::Shapes }, &mut out);
    if let Some(lp) = layer_pm {
        pm.draw_pixmap(0, 0, lp.as_ref(), &PixmapPaint { opacity, ..PixmapPaint::default() }, Transform::identity(), None);
    }
    out
}

/// Which of a style's layers [`draw_layers`] draws.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pass {
    /// Everything, labels collected.
    All,
    /// Everything but labels.
    Shapes,
    /// Only the labels, collected (nothing is drawn).
    Labels,
}

/// Draws the layers of a resolved style for `pass` into `target` (a `w × h` pixmap at `frame`)
/// from the tiles over `view`, collecting labels into `out`.
pub(crate) fn draw_layers(
    mut target: Option<&mut Pixmap>,
    (w, h): (u32, u32),
    view: &Extent,
    r: &Resolved,
    frame: &Frame,
    pass: Pass,
    out: &mut Drawn,
) {
    let zoom = template::style_zoom(frame.m_per_px * f64::from(frame.ppp));
    let mut items: HashMap<String, Vec<Item>> = HashMap::new();
    let mut boxes: Boxes = HashMap::new();
    // An elevation source's heights, read once for its layers, by source and resampling.
    let mut grids: HashMap<(String, bool), dem::Grid> = HashMap::new();
    // The sprite, fetched the first time a layer needs it.
    let mut sprite_state: Option<Option<Arc<Sprite>>> = None;
    let mut sprite = |pending: &mut bool| -> Option<Arc<Sprite>> {
        sprite_state
            .get_or_insert_with(|| match r.sprite.as_deref().map(|b| sprite::load(b, frame.ppp >= 1.5)) {
                Some(Load::Ready(s)) => Some(s),
                Some(Load::Pending) => {
                    *pending = true;
                    None
                }
                Some(Load::Failed) | None => None,
            })
            .clone()
    };
    for (li, layer) in r.style.layers.iter().enumerate() {
        let wanted = match pass {
            Pass::All => true,
            Pass::Shapes => layer.kind != LayerKind::Symbol,
            Pass::Labels => layer.kind == LayerKind::Symbol,
        };
        if !wanted || !layer.visible_at(zoom) {
            continue;
        }
        if layer.kind == LayerKind::Background {
            if let (Some(c), Some(rect), Some(t)) =
                (layer.background_color(zoom), Rect::from_xywh(0.0, 0.0, w as f32, h as f32), target.as_deref_mut())
            {
                let mut p = Paint::default();
                p.set_color(color(c, layer.background_opacity(zoom)));
                t.fill_rect(rect, &p, Transform::identity(), None);
            }
            continue;
        }
        let Some(src_id) = layer.source.as_deref() else { continue };
        // A colour ramp it can't draw needs no heights read.
        if layer.kind == LayerKind::ColorRelief && layer.color_relief_ramp(zoom).is_none() {
            continue;
        }
        if !items.contains_key(src_id) {
            let Some(src) = r.style.sources.iter().find(|s| s.0 == src_id).map(|s| &s.1) else { continue };
            // Hillshade reads the heights a pixel past the view's edges: those tiles too.
            let pad = 2.0 * frame.m_per_px;
            let reach = Extent::new(view.min_x - pad, view.min_y - pad, view.max_x + pad, view.max_y + pad);
            let list = tiles_for(src, if matches!(src, Source::RasterDem { .. }) { &reach } else { view }, frame, zoom, &mut out.pending);
            items.insert(src_id.to_string(), list);
        }
        let Some(list) = items.get(src_id) else { continue };
        match layer.kind {
            LayerKind::Hillshade | LayerKind::ColorRelief => {
                let Some(t) = target.as_deref_mut() else { continue };
                let Some(Source::RasterDem { encoding, .. }) = r.style.source(src_id) else { continue };
                let nearest = layer.resampling_nearest(zoom);
                let grid =
                    grids.entry((src_id.to_string(), nearest)).or_insert_with(|| dem::Grid::read(list, *encoding, frame, (w, h), nearest));
                dem::draw(t, layer, grid, zoom);
            }
            LayerKind::Raster => {
                let Some(t) = target.as_deref_mut() else { continue };
                let o = layer.raster_opacity(zoom);
                for it in list {
                    draw_raster(t, it, frame, o);
                }
            }
            LayerKind::Fill | LayerKind::FillExtrusion | LayerKind::Line | LayerKind::Circle => {
                let Some(t) = target.as_deref_mut() else { continue };
                if !drawable(layer) {
                    continue;
                }
                // A pattern fill without its sprite isn't drawn: its fallback colour is black.
                let sheet = if pattern(layer).is_some() {
                    match sprite(&mut out.pending) {
                        Some(s) => Some(s),
                        None => continue,
                    }
                } else {
                    None
                };
                for it in list {
                    let b = boxes_of(&mut boxes, it, layer);
                    draw_vector(t, it, layer, frame, zoom, sheet.as_deref(), &b);
                }
            }
            LayerKind::Symbol if pass != Pass::Shapes => {
                let sheet = if layer.has_layout("icon-image") { sprite(&mut out.pending) } else { None };
                for it in list {
                    crate::symbols::collect(
                        &mut out.labels,
                        it,
                        layer,
                        frame,
                        zoom,
                        crate::symbols::placement_rank(li),
                        sheet.as_deref(),
                        (w as f32, h as f32),
                    );
                }
            }
            _ => {}
        }
    }
    // The cap keeps the labels placed first, whichever layers they came from.
    crate::symbols::keep_first(&mut out.labels, crate::symbols::MAX_LABELS);
}

/// One raster tile placed on the map, for a renderer that draws each tile itself (the GPU map
/// keeps a texture per tile picture and moves the tiles with the camera, as MapLibre does).
#[derive(Clone)]
pub struct Placement {
    /// The tile's picture, shared with the cache: always a [`Payload::Image`].
    pub picture: Arc<Payload>,
    /// The Web Mercator square the picture covers.
    pub at: Extent,
    /// The square it is drawn in: `at` itself, or the missing tile a parent or child stands in
    /// for.
    pub clip: Extent,
    pub opacity: f32,
}

/// A tile layer as placed tiles, bottom first, rather than drawn into a pixmap.
#[derive(Default)]
pub struct PlacedTiles {
    pub tiles: Vec<Placement>,
    /// The labels of its symbol layers, when asked for.
    pub labels: Vec<Label>,
    pub attribution: Vec<String>,
    pub pending: bool,
    pub error: Option<String>,
}

/// `spec` as placed tiles over `view` (fetching those missing) and out to `reach` (only those
/// already in the cache, so a camera moving there finds tiles to show), or `None` when it can't
/// be, which [`draw`] then draws into a pixmap. Raster tiles are placed as they are, vector tiles
/// as pictures of their shapes (see `picture`); their labels are collected for a `labels`-sized
/// pixmap at `frame` when that is given.
pub fn place(spec: &Spec, opacity: f32, frame: &Frame, view: &Extent, reach: &Extent, labels: Option<(u32, u32)>) -> Option<PlacedTiles> {
    place_as(spec, opacity, frame, view, reach, labels, true)
}

/// `spec`'s raster tiles placed over `view` and out to `reach`, as [`place`] places them, or
/// `None` when it draws anything but raster tiles. Cheap (cache lookups, nothing drawn): the GPU
/// map's tile layer places them again for every frame it shows (#635).
pub fn place_raster(spec: &Spec, opacity: f32, frame: &Frame, view: &Extent, reach: &Extent) -> Option<PlacedTiles> {
    place_as(spec, opacity, frame, view, reach, None, false)
}

fn place_as(
    spec: &Spec,
    opacity: f32,
    frame: &Frame,
    view: &Extent,
    reach: &Extent,
    labels: Option<(u32, u32)>,
    pictures: bool,
) -> Option<PlacedTiles> {
    let mut out = PlacedTiles::default();
    let r = match resolve(spec) {
        Resolve::Ready(r) => r,
        // A style still loading may draw more than raster tiles: not placed raster-only yet.
        Resolve::Pending if !pictures => return None,
        Resolve::Pending => {
            out.pending = true;
            return Some(out);
        }
        Resolve::Failed(e) => {
            out.error = Some(e);
            return Some(out);
        }
    };
    out.attribution = r.attribution.clone();
    out.error = left_out(&r);
    out.pending |= r.waiting;
    if !(frame.m_per_px.is_finite() && frame.m_per_px > 0.0) || opacity <= 0.0 {
        return Some(out);
    }
    // Without pictures, a style drawing shapes is left to the check below (no raster layers only).
    if let Some(pictures) = pictures.then(|| crate::picture::cover(&r, frame, view, Some(reach), &mut out.pending)).flatten() {
        let opacity = opacity.clamp(0.0, 1.0);
        out.tiles = pictures.into_iter().map(|it| Placement { picture: it.payload, at: it.at, clip: it.clip, opacity }).collect();
        if let Some(size) = labels {
            let mut d = Drawn::default();
            draw_layers(None, size, &frame.extent(size.0, size.1), &r, frame, Pass::Labels, &mut d);
            out.labels = d.labels;
            out.pending |= d.pending;
        }
        return Some(out);
    }
    let zoom = template::style_zoom(frame.m_per_px * f64::from(frame.ppp));
    let shown: Vec<&StyleLayer> = r.style.layers.iter().filter(|l| l.visible_at(zoom) && l.kind != LayerKind::Other).collect();
    // Raster layers only; several are composited together before the layer's opacity applies.
    if shown.iter().any(|l| l.kind != LayerKind::Raster) || (shown.len() > 1 && opacity < 0.999) {
        return None;
    }
    let mut items: HashMap<&str, Vec<Item>> = HashMap::new();
    for layer in shown {
        let Some(src_id) = layer.source.as_deref() else { continue };
        if !items.contains_key(src_id) {
            let Some(src) = r.style.sources.iter().find(|s| s.0 == src_id).map(|s| &s.1) else { continue };
            items.insert(src_id, tiles_in(src, view, Some(reach), frame, zoom, &mut out.pending));
        }
        let o = (layer.raster_opacity(zoom) * opacity).clamp(0.0, 1.0);
        for it in items.get(src_id).into_iter().flatten() {
            if matches!(*it.payload, Payload::Image(_)) {
                out.tiles.push(Placement { picture: it.payload.clone(), at: it.at, clip: it.clip, opacity: o });
            }
        }
    }
    Some(out)
}

/// A vector tile feature under a point (see [`pick`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Picked {
    /// The tile layer it is in (`roads`, `buildings`).
    pub source_layer: String,
    pub id: Option<u64>,
    pub kind: GeomKind,
    pub props: Vec<(String, crate::mvt::Value)>,
}

/// The vector tile features drawn under the Web Mercator point (`x`, `y`), topmost first: those
/// of the fill, line, circle and symbol layers shown at this scale whose shapes come within
/// `tol_px` pixels of it (lines and circles counting their width). `m_per_px` is the map's metres
/// per (logical) pixel, from which the tile zoom is chosen as drawing chooses it, so the tiles
/// looked at are those the map draws there (one not fetched yet is asked for, and its features
/// come once it arrives). At most `limit` are returned, each once however many style layers
/// draw it.
pub fn pick(spec: &Spec, x: f64, y: f64, m_per_px: f64, tol_px: f32, limit: usize) -> Vec<Picked> {
    let mut out: Vec<Picked> = Vec::new();
    let Resolve::Ready(r) = resolve(spec) else { return out };
    if !(m_per_px.is_finite() && m_per_px > 0.0 && x.is_finite() && y.is_finite()) {
        return out;
    }
    let tol_px = tol_px.clamp(0.0, 50.0);
    let tol = f64::from(tol_px) * m_per_px;
    let frame = Frame { left: x - tol, top: y + tol, m_per_px, ppp: 1.0 };
    let view = Extent::new(x - tol, y - tol, x + tol, y + tol);
    let zoom = template::style_zoom(m_per_px);
    let mut items: HashMap<String, Vec<Item>> = HashMap::new();
    let mut pending = false;
    // A feature drawn by several style layers (a fill and its outline) is reported once: known by
    // its source, tile and place in the tile.
    let mut seen: std::collections::HashSet<(String, usize, String, usize)> = std::collections::HashSet::new();
    // The layer drawn last is on top.
    for layer in r.style.layers.iter().rev() {
        if out.len() >= limit {
            break;
        }
        // The layers `draw` draws, and labels only where they are placed at points.
        let kind_ok = match layer.kind {
            LayerKind::Fill | LayerKind::FillExtrusion | LayerKind::Circle => true,
            LayerKind::Line => drawable(layer),
            LayerKind::Symbol => matches!(layer.symbol_placement(&FeatureCtx::zoom_only(zoom)), crate::style::SymbolPlacement::Point),
            _ => false,
        };
        if !kind_ok || !layer.visible_at(zoom) {
            continue;
        }
        let (Some(src_id), Some(src_layer)) = (layer.source.as_deref(), layer.source_layer.as_deref()) else { continue };
        if !items.contains_key(src_id) {
            let Some(src) = r.style.sources.iter().find(|s| s.0 == src_id).map(|s| &s.1) else { continue };
            if !matches!(src, Source::Vector { .. }) {
                continue;
            }
            items.insert(src_id.to_string(), tiles_for(src, &view, &frame, zoom, &mut pending));
        }
        let Some(list) = items.get(src_id) else { continue };
        // Every tile within reach (a feature near a tile's edge may be in its neighbour), the real
        // ones (after the stand-ins) first.
        for (ti, it) in list.iter().enumerate().rev().filter(|(_, it)| it.clip.intersects(&view)) {
            let Payload::Vector(tile) = &*it.payload else { continue };
            let Some(l) = tile.layers.iter().find(|l| l.name == src_layer) else { continue };
            // The point and tolerance in the tile's units.
            let units = f64::from(l.extent.max(1)) / TileId::span(it.z);
            let p = [((x - it.at.min_x) * units) as f32, ((it.at.max_y - y) * units) as f32];
            let px = (m_per_px * units) as f32;
            for (fi, f) in l.features.iter().enumerate() {
                let ctx = FeatureCtx { zoom, kind: f.kind, id: f.id, props: &f.props };
                if !layer.matches(&ctx) {
                    continue;
                }
                // How near counts, for what `draw` would draw of it (nothing: not reported).
                let reach = match layer.kind {
                    LayerKind::Fill | LayerKind::FillExtrusion if f.kind == GeomKind::Polygon && layer.fill_color(&ctx).is_some() => tol_px,
                    LayerKind::Line if matches!(f.kind, GeomKind::LineString | GeomKind::Polygon) && layer.line_color(&ctx).is_some() => {
                        let w = layer.line_width(&ctx);
                        if !(w.is_finite() && w > 0.0) {
                            continue;
                        }
                        tol_px + w / 2.0
                    }
                    LayerKind::Circle if f.kind == GeomKind::Point && layer.circle_color(&ctx).is_some() => {
                        let r = layer.circle_radius(&ctx);
                        if !(r.is_finite() && r > 0.0) {
                            continue;
                        }
                        tol_px + r
                    }
                    // A label or icon's point, as near as a small symbol.
                    LayerKind::Symbol
                        if f.kind == GeomKind::Point
                            && (layer.has_layout("icon-image") || layer.text_field(&ctx).is_some_and(|t| !t.trim().is_empty())) =>
                    {
                        tol_px + 6.0
                    }
                    _ => continue,
                };
                if !hits(f, layer.kind, p, reach * px) || !seen.insert((src_id.to_string(), ti, l.name.clone(), fi)) {
                    continue;
                }
                out.push(Picked { source_layer: l.name.clone(), id: f.id, kind: f.kind, props: f.props.clone() });
                if out.len() >= limit {
                    return out;
                }
            }
        }
    }
    out
}

/// Whether a feature's shape (in tile units) comes within `reach` of `p`: inside a polygon fill,
/// or near its outline, a line or a point.
fn hits(f: &crate::mvt::Feature, kind: LayerKind, p: [f32; 2], reach: f32) -> bool {
    let near = |a: [f32; 2], b: [f32; 2]| {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
        let (ex, ey) = (a[0] + t * dx - p[0], a[1] + t * dy - p[1]);
        ex * ex + ey * ey <= reach * reach
    };
    let along = |part: &[[f32; 2]], closed: bool| {
        part.windows(2).any(|w| matches!(w, [a, b] if near(*a, *b)))
            || (closed && matches!((part.last(), part.first()), (Some(a), Some(b)) if near(*a, *b)))
            || (part.len() == 1 && part.first().is_some_and(|a| near(*a, *a)))
    };
    match f.kind {
        GeomKind::Point => f.geometry.iter().flatten().any(|a| near(*a, *a)),
        GeomKind::LineString => f.geometry.iter().any(|part| along(part, false)),
        GeomKind::Polygon if kind == LayerKind::Line => f.geometry.iter().any(|part| along(part, true)),
        // Inside by the even-odd rule over every ring (holes and separate parts alike), or on an edge.
        GeomKind::Polygon => {
            let mut inside = false;
            for ring in &f.geometry {
                let n = ring.len();
                for i in 0..n {
                    let (Some(a), Some(b)) = (ring.get(i), ring.get((i + n - 1) % n)) else { continue };
                    if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
                        inside = !inside;
                    }
                }
            }
            inside || f.geometry.iter().any(|part| along(part, true))
        }
        GeomKind::Unknown => false,
    }
}

/// The tiles of a source in view, with stand-ins for those not here yet.
fn tiles_for(src: &Source, view: &Extent, frame: &Frame, zoom: f32, pending: &mut bool) -> Vec<Item> {
    tiles_in(src, view, None, frame, zoom, pending)
}

/// The tiles of a source in view and out to `reach` (those in the cache only), with stand-ins
/// for those not here yet.
pub(crate) fn tiles_in(src: &Source, view: &Extent, reach: Option<&Extent>, frame: &Frame, zoom: f32, pending: &mut bool) -> Vec<Item> {
    let (tiles, min_z, max_z, want, z) = match src {
        Source::Raster { tiles, tile_size, min_zoom, max_zoom, .. } | Source::RasterDem { tiles, tile_size, min_zoom, max_zoom, .. } => {
            let z = template::zoom_for(frame.m_per_px * f64::from(frame.ppp), *tile_size, *max_zoom);
            (tiles, *min_zoom, *max_zoom, Want::Image, z)
        }
        Source::Vector { tiles, min_zoom, max_zoom, .. } => {
            (tiles, *min_zoom, *max_zoom, Want::Vector, (zoom.floor() as u8).min(*max_zoom))
        }
        Source::Other => return Vec::new(),
    };
    if tiles.is_empty() || z < min_z || zoom + 0.5 < f32::from(min_z) {
        return Vec::new();
    }
    // A mosaic over half the world wide (the globe's, a world projection's) takes the zoom whose
    // tiles all fit, as its far edges are in view; a flat view keeps those nearest its centre.
    let z = if view.width() > template::HALF_WORLD { template::fitting_zoom(view, z, min_z) } else { z };
    let url = |t: TileId| {
        let i = (t.x as usize).wrapping_add(t.y as usize) % tiles.len();
        tiles.get(i).map(|tpl| template::expand(tpl, t, 256, false, &[]))
    };
    let mut exact = Vec::new();
    let mut stand_ins = Vec::new();
    // A missing tile is stood in for by its nearest ancestor in the cache and, for raster tiles,
    // by its children in the cache, the ancestor only in the quarters they leave: zooming in, the
    // tiles of the zoom being passed through are often missing while those of the zoom it ends
    // at are already here, and they are sharper. Nothing is drawn twice, so a see-through layer
    // stays even.
    let stand_in = |out: &mut Vec<Item>, p: &Placed| {
        let parent = ancestor(p, min_z, &url);
        if want != Want::Image || p.id.z >= max_z {
            out.extend(parent);
            return;
        }
        for c in p.id.children() {
            let cp = Placed { id: c, wx: p.wx * 2 + i64::from(c.x & 1) };
            match url(c).and_then(|u| cache::peek(&u)) {
                Some(payload) => out.push(Item { payload, at: cp.extent(), z: c.z, clip: cp.extent() }),
                None => out.extend(parent.as_ref().map(|a| Item { clip: cp.extent(), ..a.clone() })),
            }
        }
    };
    let mut placed = reach.map(|e| template::covering(e, z)).unwrap_or_default();
    if placed.is_empty() {
        placed = template::covering(view, z);
    }
    for p in placed {
        let Some(u) = url(p.id) else { continue };
        // Past the view, only what the cache already has.
        let state = if p.extent().intersects(view) {
            cache::get_tile(&u, want)
        } else {
            match cache::peek(&u) {
                Some(payload) => State::Ready(payload),
                None => {
                    stand_in(&mut stand_ins, &p);
                    continue;
                }
            }
        };
        match state {
            State::Ready(payload) => exact.push(Item { payload, at: p.extent(), z: p.id.z, clip: p.extent() }),
            State::Failed(_) => {}
            State::Pending => {
                *pending = true;
                stand_in(&mut stand_ins, &p);
            }
        }
    }
    // Stand-ins first, so the real tiles draw over their edges.
    stand_ins.extend(exact);
    stand_ins
}

/// The nearest ancestor of a missing tile already in the cache, clipped to the missing tile.
fn ancestor(p: &Placed, min_z: u8, url: &dyn Fn(TileId) -> Option<String>) -> Option<Item> {
    for lv in 1..=p.id.z.saturating_sub(min_z).min(8) {
        let a = p.id.ancestor(lv)?;
        if let Some(payload) = url(a).and_then(|u| cache::peek(&u)) {
            let ap = Placed { id: a, wx: p.wx.div_euclid(1 << lv) };
            return Some(Item { payload, at: ap.extent(), z: a.z, clip: p.extent() });
        }
    }
    None
}

/// Whether a fill, line or circle layer can be drawn. Lines painted with a pattern can't (the
/// pattern would have to turn along the line), and the colour the spec falls back to, black,
/// would cover the map.
fn drawable(layer: &StyleLayer) -> bool {
    !layer.has_paint("line-pattern")
}

/// The paint property a fill layer takes its pattern from, if it has one.
fn pattern(layer: &StyleLayer) -> Option<&'static str> {
    ["fill-pattern", "fill-extrusion-pattern"].into_iter().find(|p| layer.has_paint(p))
}

fn color(c: Rgba, opacity: f32) -> tiny_skia::Color {
    let a = (f32::from(c[3]) / 255.0 * opacity.clamp(0.0, 1.0)).clamp(0.0, 1.0);
    tiny_skia::Color::from_rgba(f32::from(c[0]) / 255.0, f32::from(c[1]) / 255.0, f32::from(c[2]) / 255.0, a)
        .unwrap_or(tiny_skia::Color::TRANSPARENT)
}

/// The square an item draws in, on whole pixels: neighbouring tiles then meet on a pixel edge,
/// so anti-aliased fills cut there neither leave a seam nor overlap.
pub(crate) fn clip_rect(it: &Item, frame: &Frame) -> Option<Rect> {
    let (x0, y0) = frame.px(it.clip.min_x, it.clip.max_y);
    let (x1, y1) = frame.px(it.clip.max_x, it.clip.min_y);
    Rect::from_ltrb(x0.round(), y0.round(), x1.round(), y1.round())
}

fn draw_raster(pm: &mut Pixmap, it: &Item, frame: &Frame, opacity: f32) {
    let Payload::Image(img) = &*it.payload else { return };
    let Some(clip) = clip_rect(it, frame) else { return };
    let (x0, y0) = frame.px(it.at.min_x, it.at.max_y);
    let side = (TileId::span(it.z) / frame.m_per_px) as f32;
    let k = side / img.width().max(1) as f32;
    if !(k.is_finite() && k > 0.0) {
        return;
    }
    if (k - 1.0).abs() < 0.01 {
        copy_over(pm, img, clip, (x0, y0), k, opacity);
        return;
    }
    let quality = FilterQuality::Bilinear;
    let shader = Pattern::new(img.as_ref(), SpreadMode::Pad, quality, opacity.clamp(0.0, 1.0), Transform::from_row(k, 0.0, 0.0, k, x0, y0));
    // Without anti-aliasing, neighbouring tiles meet without a seam.
    let paint = Paint { shader, anti_alias: false, ..Paint::default() };
    // Only the part in the pixmap.
    let Some(clip) = clip.intersect(&Rect::from_ltrb(0.0, 0.0, pm.width() as f32, pm.height() as f32).unwrap_or(clip)) else { return };
    pm.fill_rect(clip, &paint, Transform::identity(), None);
}

/// Draws a picture at its own size (`k`, the pixmap's pixels to one of its own, near 1) over the
/// `clip` pixels of `pm`, its top-left corner at `at`: each pixel takes the picture's nearest,
/// source over. Much faster than a pattern fill, for a map made of tile pictures.
fn copy_over(pm: &mut Pixmap, img: &Pixmap, clip: Rect, at: (f32, f32), k: f32, opacity: f32) {
    let (w, h) = (i64::from(pm.width()), i64::from(pm.height()));
    let (x0, y0) = ((clip.left() as i64).max(0), (clip.top() as i64).max(0));
    let (x1, y1) = ((clip.right() as i64).min(w), (clip.bottom() as i64).min(h));
    let (iw, ih) = (i64::from(img.width()), i64::from(img.height()));
    if x0 >= x1 || y0 >= y1 || iw == 0 || ih == 0 {
        return;
    }
    let nearest = |p: i64, o: f32, n: i64| (((p as f32 + 0.5 - o) / k).floor() as i64).clamp(0, n - 1) as usize;
    let cols: Vec<usize> = (x0..x1).map(|x| nearest(x, at.0, iw)).collect();
    let o = (opacity.clamp(0.0, 1.0) * 255.0).round() as u32;
    let (src, dst) = (img.data(), pm.data_mut());
    for y in y0..y1 {
        let from = nearest(y, at.1, ih) * iw as usize * 4;
        let to = (y as usize * w as usize + x0 as usize) * 4;
        for (i, &sx) in cols.iter().enumerate() {
            let (si, di) = (from + sx * 4, to + i * 4);
            let (Some(s), Some(d)) = (src.get(si..si + 4), dst.get_mut(di..di + 4)) else { continue };
            // Premultiplied: the picture's pixel at the opacity, over what is there.
            let s = [s[0], s[1], s[2], s[3]].map(|c| u32::from(c) * o / 255);
            let a = s[3];
            if a == 255 {
                for (d, s) in d.iter_mut().zip(s) {
                    *d = s as u8;
                }
            } else if a > 0 {
                for (d, s) in d.iter_mut().zip(s) {
                    *d = (s + u32::from(*d) * (255 - a) / 255).min(255) as u8;
                }
            }
        }
    }
}

/// A tile's geometry in pixels, clipped to the item's square (lines and polygons) or kept only
/// inside it (points).
pub(crate) fn to_px(it: &Item, layer_extent: u32, frame: &Frame) -> impl Fn(&[f32; 2]) -> (f32, f32) + use<> {
    let (x0, y0) = frame.px(it.at.min_x, it.at.max_y);
    let k = (TileId::span(it.z) / frame.m_per_px) as f32 / layer_extent.max(1) as f32;
    move |p: &[f32; 2]| (x0 + p[0] * k, y0 + p[1] * k)
}

/// The bounding boxes of a tile layer's features (in tile units), by the tile's payload and the
/// layer's name: found once for every style layer and picture drawing it.
type Boxes = HashMap<(usize, String), Arc<Vec<[f32; 4]>>>;

/// The boxes of the features `layer` draws from `it`, from those known (`known`, for this draw)
/// or those kept for every draw.
type Kept = HashMap<(usize, String), (std::sync::Weak<Payload>, Arc<Vec<[f32; 4]>>)>;
static KEPT: std::sync::OnceLock<std::sync::Mutex<Kept>> = std::sync::OnceLock::new();

/// Forgets the features' boxes kept for every draw (with [`crate::cache::clear`]).
pub(crate) fn forget_boxes() {
    if let Some(k) = KEPT.get() {
        k.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

fn boxes_of(known: &mut Boxes, it: &Item, layer: &StyleLayer) -> Arc<Vec<[f32; 4]>> {
    let (Payload::Vector(tile), Some(name)) = (&*it.payload, layer.source_layer.as_deref()) else { return Arc::default() };
    let key = (Arc::as_ptr(&it.payload) as usize, name.to_string());
    if let Some(b) = known.get(&key) {
        return b.clone();
    }
    let kept = KEPT.get_or_init(Default::default);
    let lock = || kept.lock().unwrap_or_else(|e| e.into_inner());
    let found = lock().get(&key).filter(|(p, _)| p.upgrade().is_some_and(|p| Arc::ptr_eq(&p, &it.payload))).map(|(_, b)| b.clone());
    let b = found.unwrap_or_else(|| {
        let b: Vec<[f32; 4]> = tile
            .layers
            .iter()
            .find(|l| l.name == name)
            .map(|l| {
                l.features
                    .iter()
                    .map(|f| {
                        f.geometry.iter().flatten().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| {
                            [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let b = Arc::new(b);
        let mut k = lock();
        // Those of tiles the cache let go go too.
        if k.len() > 1024 {
            k.retain(|_, (p, _)| p.strong_count() > 0);
        }
        k.insert(key.clone(), (Arc::downgrade(&it.payload), b.clone()));
        b
    });
    known.insert(key, b.clone());
    b
}

/// How far past the pixmap a line or circle may lie and still be drawn into it, in points, at
/// least: its half width.
const OVERHANG: f32 = 16.0;

/// How far a layer's shapes reach past their geometry at `zoom`, in points: a line's half width
/// (twice its width at a mitred corner), a circle's radius and outline. Widths that depend on
/// the feature are taken at the layer's own, and never less than [`OVERHANG`].
pub(crate) fn overhang(layer: &StyleLayer, zoom: f32) -> f32 {
    let ctx = FeatureCtx::zoom_only(zoom);
    let reach = match layer.kind {
        LayerKind::Line => {
            let k = if layer.line_join(&ctx) == crate::style::LineJoin::Miter { 2.0 } else { 0.75 };
            layer.line_width(&ctx) * k
        }
        LayerKind::Circle => layer.circle_radius(&ctx) + layer.circle_stroke_width(&ctx),
        _ => 0.0,
    };
    if reach.is_finite() { reach.clamp(OVERHANG, 512.0) } else { OVERHANG }
}

fn draw_vector(pm: &mut Pixmap, it: &Item, layer: &StyleLayer, frame: &Frame, zoom: f32, sheet: Option<&Sprite>, boxes: &[[f32; 4]]) {
    let Payload::Vector(tile) = &*it.payload else { return };
    let Some(src_layer) = layer.source_layer.as_deref() else { return };
    let Some(l) = tile.layers.iter().find(|l| l.name == src_layer) else { return };
    let Some(clip) = clip_rect(it, frame) else { return };
    let bounds = [clip.left(), clip.top(), clip.right(), clip.bottom()];
    let to = to_px(it, l.extent, frame);
    let ppp = frame.ppp;
    // The part of the tile drawn into the pixmap, in tile units: features outside it are skipped
    // before the style looks at them (a picture of a tile's corner has most of its features
    // outside).
    let (x0, y0) = frame.px(it.at.min_x, it.at.max_y);
    let k = (TileId::span(it.z) / frame.m_per_px) as f32 / l.extent.max(1) as f32;
    // A fill reaches past its shape only by its outline.
    let pad = if matches!(layer.kind, LayerKind::Fill | LayerKind::FillExtrusion) { 2.0 } else { overhang(layer, zoom) * ppp };
    let (pw, ph) = (pm.width() as f32, pm.height() as f32);
    // Shapes are cut a little past the pixmap too (in pixels, by `pad`), so a long road or a
    // large park reaching far past it costs no more than the part in it.
    let cut = |pad: f32| [bounds[0].max(-pad), bounds[1].max(-pad), bounds[2].min(pw + pad), bounds[3].min(ph + pad)];
    let reach = cut(pad);
    if reach[0] >= reach[2] || reach[1] >= reach[3] {
        return;
    }
    let area = [(reach[0] - x0) / k, (reach[1] - y0) / k, (reach[2] - x0) / k, (reach[3] - y0) / k];
    for (i, f) in l.features.iter().enumerate() {
        if boxes.get(i).is_some_and(|b| b[0] > area[2] || b[2] < area[0] || b[1] > area[3] || b[3] < area[1]) {
            continue;
        }
        let ctx = FeatureCtx { zoom, kind: f.kind, id: f.id, props: &f.props };
        if !layer.matches(&ctx) {
            continue;
        }
        match layer.kind {
            LayerKind::Fill | LayerKind::FillExtrusion if f.kind == GeomKind::Polygon => {
                let Some(c) = layer.fill_color(&ctx) else { continue };
                let mut pb = PathBuilder::new();
                for ring in &f.geometry {
                    let pts: Vec<(f32, f32)> = ring.iter().map(&to).collect();
                    let clipped = clip_polygon(&pts, cut(1.0));
                    push_ring(&mut pb, &clipped);
                }
                let Some(path) = pb.finish() else { continue };
                let opacity = layer.fill_opacity(&ctx);
                let mut paint = Paint::default();
                paint.set_color(color(c, opacity));
                paint.anti_alias = true;
                // A pattern from the sprite, repeated from a fixed point of the world so it
                // lines up across tiles and stays put as the map moves.
                let image = pattern(layer).and_then(|p| {
                    let name = layer.paint_value(p, &ctx)?;
                    sheet?.image(name.as_str()?, None)
                });
                if pattern(layer).is_some() && image.is_none() {
                    continue;
                }
                if let Some(img) = &image {
                    let s = frame.ppp / img.ratio;
                    let (iw, ih) = (img.pixmap.width() as f32 * s, img.pixmap.height() as f32 * s);
                    let (ox, oy) = frame.px(0.0, 0.0);
                    let at = Transform::from_row(s, 0.0, 0.0, s, ox.rem_euclid(iw.max(1.0)), oy.rem_euclid(ih.max(1.0)));
                    paint.shader =
                        Pattern::new(Pixmap::as_ref(&img.pixmap), SpreadMode::Repeat, FilterQuality::Bilinear, opacity.clamp(0.0, 1.0), at);
                }
                pm.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
                // The outline follows the rings themselves, not where the tile cut them.
                if let Some(oc) = layer.fill_outline_color(&ctx) {
                    let mut ob = PathBuilder::new();
                    for ring in &f.geometry {
                        let mut pts: Vec<(f32, f32)> = ring.iter().map(&to).collect();
                        if let (Some(&a), Some(&b)) = (pts.first(), pts.last())
                            && a != b
                        {
                            pts.push(a);
                        }
                        for piece in clip_line(&pts, cut(2.0)) {
                            push_line(&mut ob, &piece);
                        }
                    }
                    if let Some(outline) = ob.finish() {
                        paint.set_color(color(oc, opacity));
                        pm.stroke_path(
                            &outline,
                            &paint,
                            &Stroke { width: ppp.max(1.0) * 0.75, ..Stroke::default() },
                            Transform::identity(),
                            None,
                        );
                    }
                }
            }
            LayerKind::Line if matches!(f.kind, GeomKind::LineString | GeomKind::Polygon) => {
                let Some(c) = layer.line_color(&ctx) else { continue };
                let width = layer.line_width(&ctx) * ppp;
                if !(width.is_finite() && width > 0.0) {
                    continue;
                }
                let join = layer.line_join(&ctx);
                // Cut past the pixmap by as far as its stroke reaches (a mitred corner up to
                // twice the width).
                let edge = cut(if join == crate::style::LineJoin::Miter { 2.0 } else { 0.75 } * width.min(512.0) + 2.0);
                let mut paint = Paint::default();
                paint.set_color(color(c, layer.line_opacity(&ctx)));
                paint.anti_alias = true;
                let mut stroke = Stroke {
                    width: width.min(512.0),
                    line_cap: match layer.line_cap(&ctx) {
                        crate::style::LineCap::Round => LineCap::Round,
                        crate::style::LineCap::Square => LineCap::Square,
                        crate::style::LineCap::Butt => LineCap::Butt,
                    },
                    line_join: match join {
                        crate::style::LineJoin::Round => LineJoin::Round,
                        crate::style::LineJoin::Bevel => LineJoin::Bevel,
                        crate::style::LineJoin::Miter => LineJoin::Miter,
                    },
                    ..Stroke::default()
                };
                let dashes = layer.line_dasharray(&ctx).map(|d| {
                    let mut v: Vec<f32> = d.iter().take(32).map(|x| (x * width.max(1.0)).max(0.5)).collect();
                    // An odd number of lengths repeats once over (as in SVG).
                    if !v.len().is_multiple_of(2) {
                        v.extend(v.clone());
                    }
                    v
                });
                let Some(dashes) = dashes else {
                    let mut pb = PathBuilder::new();
                    for line in &f.geometry {
                        let pts: Vec<(f32, f32)> = line.iter().map(&to).collect();
                        for piece in clip_line(&pts, edge) {
                            push_line(&mut pb, &piece);
                        }
                    }
                    if let Some(path) = pb.finish() {
                        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                    }
                    continue;
                };
                // The dashes start at the tile's edge, as the line is cut there; each piece of it
                // near the pixmap goes on from as far along as it is, so they line up with those
                // drawn next to the pixmap.
                let period: f32 = dashes.iter().sum();
                for line in &f.geometry {
                    let pts: Vec<(f32, f32)> = line.iter().map(&to).collect();
                    for piece in clip_line(&pts, bounds) {
                        for (part, along) in clip_line_along(&piece, edge) {
                            let mut pb = PathBuilder::new();
                            push_line(&mut pb, &part);
                            let Some(path) = pb.finish() else { continue };
                            stroke.dash = StrokeDash::new(dashes.clone(), along.rem_euclid(period.max(1e-3)));
                            pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
                        }
                    }
                }
            }
            LayerKind::Circle if f.kind == GeomKind::Point => {
                let Some(c) = layer.circle_color(&ctx) else { continue };
                let r = layer.circle_radius(&ctx) * ppp;
                if !(r.is_finite() && r > 0.0) {
                    continue;
                }
                let opacity = layer.circle_opacity(&ctx);
                let stroke = layer.circle_stroke_color(&ctx).map(|sc| (sc, layer.circle_stroke_width(&ctx) * ppp));
                for pt in f.geometry.iter().flatten() {
                    let (x, y) = to(pt);
                    if !inside(x, y, bounds) {
                        continue;
                    }
                    let Some(path) = PathBuilder::from_circle(x, y, r.min(256.0)) else { continue };
                    let mut paint = Paint::default();
                    paint.set_color(color(c, opacity));
                    paint.anti_alias = true;
                    pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
                    if let Some((sc, sw)) = stroke
                        && sw > 0.0
                    {
                        paint.set_color(color(sc, opacity));
                        pm.stroke_path(&path, &paint, &Stroke { width: sw.min(64.0), ..Stroke::default() }, Transform::identity(), None);
                    }
                }
            }
            _ => {}
        }
    }
}

fn inside(x: f32, y: f32, b: [f32; 4]) -> bool {
    x >= b[0] && x < b[2] && y >= b[1] && y < b[3]
}

fn push_ring(pb: &mut PathBuilder, pts: &[(f32, f32)]) {
    let mut first = true;
    for &(x, y) in pts {
        if !(x.is_finite() && y.is_finite()) {
            continue;
        }
        if first {
            pb.move_to(x, y);
            first = false;
        } else {
            pb.line_to(x, y);
        }
    }
    if !first {
        pb.close();
    }
}

fn push_line(pb: &mut PathBuilder, pts: &[(f32, f32)]) {
    if pts.len() < 2 {
        return;
    }
    for (i, &(x, y)) in pts.iter().enumerate() {
        if i == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
    }
}

/// Sutherland–Hodgman: a ring clipped to a rectangle `[left, top, right, bottom]`.
pub(crate) fn clip_polygon(pts: &[(f32, f32)], b: [f32; 4]) -> Vec<(f32, f32)> {
    // Fully inside: nothing to clip.
    if pts.iter().all(|&(x, y)| x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]) {
        return pts.to_vec();
    }
    let mut poly = pts.to_vec();
    for edge in 0..4 {
        if poly.is_empty() {
            break;
        }
        let inside = |p: (f32, f32)| match edge {
            0 => p.0 >= b[0],
            1 => p.0 <= b[2],
            2 => p.1 >= b[1],
            _ => p.1 <= b[3],
        };
        let cross = |a: (f32, f32), c: (f32, f32)| -> (f32, f32) {
            let t = match edge {
                0 => (b[0] - a.0) / (c.0 - a.0),
                1 => (b[2] - a.0) / (c.0 - a.0),
                2 => (b[1] - a.1) / (c.1 - a.1),
                _ => (b[3] - a.1) / (c.1 - a.1),
            };
            let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
            (a.0 + (c.0 - a.0) * t, a.1 + (c.1 - a.1) * t)
        };
        let mut next = Vec::with_capacity(poly.len() + 4);
        let mut prev = poly.last().copied().unwrap_or((0.0, 0.0));
        for &p in &poly {
            match (inside(prev), inside(p)) {
                (true, true) => next.push(p),
                (true, false) => next.push(cross(prev, p)),
                (false, true) => {
                    next.push(cross(prev, p));
                    next.push(p);
                }
                (false, false) => {}
            }
            prev = p;
        }
        poly = next;
    }
    poly
}

/// A polyline clipped to a rectangle: the pieces inside it.
pub(crate) fn clip_line(pts: &[(f32, f32)], b: [f32; 4]) -> Vec<Vec<(f32, f32)>> {
    let mut out: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut cur: Vec<(f32, f32)> = Vec::new();
    for w in pts.windows(2) {
        let (Some(&a), Some(&c)) = (w.first(), w.get(1)) else { continue };
        match clip_segment(a, c, b) {
            Some((p, q)) => {
                if cur.last().is_none_or(|l| (l.0 - p.0).abs() > 1e-3 || (l.1 - p.1).abs() > 1e-3) {
                    if cur.len() >= 2 {
                        out.push(std::mem::take(&mut cur));
                    }
                    cur.clear();
                    cur.push(p);
                }
                cur.push(q);
            }
            None => {
                if cur.len() >= 2 {
                    out.push(std::mem::take(&mut cur));
                }
                cur.clear();
            }
        }
    }
    if cur.len() >= 2 {
        out.push(cur);
    }
    out
}

/// A polyline clipped to a rectangle: the pieces inside it, each with how far along the line it
/// starts.
pub(crate) fn clip_line_along(pts: &[(f32, f32)], b: [f32; 4]) -> Vec<(Vec<(f32, f32)>, f32)> {
    let mut out: Vec<(Vec<(f32, f32)>, f32)> = Vec::new();
    let mut cur: Vec<(f32, f32)> = Vec::new();
    let mut start = 0.0;
    let mut along = 0.0f32;
    for w in pts.windows(2) {
        let (Some(&a), Some(&c)) = (w.first(), w.get(1)) else { continue };
        let len = (c.0 - a.0).hypot(c.1 - a.1);
        match clip_segment(a, c, b) {
            Some((p, q)) => {
                if cur.last().is_none_or(|l| (l.0 - p.0).abs() > 1e-3 || (l.1 - p.1).abs() > 1e-3) {
                    if cur.len() >= 2 {
                        out.push((std::mem::take(&mut cur), start));
                    }
                    cur.clear();
                    cur.push(p);
                    start = along + (p.0 - a.0).hypot(p.1 - a.1);
                }
                cur.push(q);
            }
            None => {
                if cur.len() >= 2 {
                    out.push((std::mem::take(&mut cur), start));
                }
                cur.clear();
            }
        }
        if len.is_finite() {
            along += len;
        }
    }
    if cur.len() >= 2 {
        out.push((cur, start));
    }
    out
}

/// Liang–Barsky.
fn clip_segment(a: (f32, f32), c: (f32, f32), b: [f32; 4]) -> Option<((f32, f32), (f32, f32))> {
    let (dx, dy) = (c.0 - a.0, c.1 - a.1);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [(-dx, a.0 - b[0]), (dx, b[2] - a.0), (-dy, a.1 - b[1]), (dy, b[3] - a.1)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
            if t0 > t1 {
                return None;
            }
        }
    }
    Some(((a.0 + t0 * dx, a.1 + t0 * dy), (a.0 + t1 * dx, a.1 + t1 * dy)))
}

/// Area of a ring in tile units (for the tests).
#[cfg(test)]
fn area(r: &[(f32, f32)]) -> f32 {
    let mut s = 0.0;
    for i in 0..r.len() {
        let (a, b) = (r[i], r[(i + 1) % r.len()]);
        s += a.0 * b.1 - b.0 * a.1;
    }
    (s / 2.0).abs()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn skips_sprite_patterns() {
        let layer = |paint: serde_json::Value| {
            StyleLayer::parse(&serde_json::json!({"id": "a", "type": "fill", "source": "s", "source-layer": "l", "paint": paint})).unwrap()
        };
        let line =
            StyleLayer::parse(&serde_json::json!({"id": "l", "type": "line", "source": "s", "paint": {"line-pattern": "rail"}})).unwrap();
        assert!(!drawable(&line));
        let fill = layer(serde_json::json!({"fill-pattern": "pedestrian_polygon"}));
        assert!(drawable(&fill) && pattern(&fill) == Some("fill-pattern"));
        assert!(pattern(&layer(serde_json::json!({"fill-color": "#ccc"}))).is_none());
    }

    /// A one-layer tile ("l") holding `features`, as the item for tile 0/0/0.
    pub(crate) fn item(features: Vec<crate::mvt::Feature>) -> Item {
        let tile = crate::mvt::Tile { layers: vec![crate::mvt::Layer { name: "l".into(), extent: 4096, features }] };
        let at = TileId { z: 0, x: 0, y: 0 }.extent();
        Item { payload: Arc::new(Payload::Vector(tile)), at, z: 0, clip: at }
    }

    /// A sprite of two 4×4 images: red "red" and an SDF "dot".
    pub(crate) fn sheet() -> Sprite {
        let mut p = Pixmap::new(8, 4).unwrap();
        p.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        let index = serde_json::json!({
            "red": {"x": 0, "y": 0, "width": 4, "height": 4, "pixelRatio": 1},
            "dot": {"x": 4, "y": 0, "width": 4, "height": 4, "pixelRatio": 1, "sdf": true}
        });
        Sprite::new(&index, p)
    }

    /// The whole world in a 64×64 pixmap.
    pub(crate) fn world_frame() -> Frame {
        let half = crate::template::HALF_WORLD;
        Frame { left: -half, top: half, m_per_px: 2.0 * half / 64.0, ppp: 1.0 }
    }

    #[test]
    fn fills_with_sprite_patterns() {
        let square = crate::mvt::Feature {
            id: None,
            kind: GeomKind::Polygon,
            geometry: vec![vec![[0.0, 0.0], [4096.0, 0.0], [4096.0, 4096.0], [0.0, 4096.0], [0.0, 0.0]]],
            props: Vec::new(),
        };
        let layer = StyleLayer::parse(
            &serde_json::json!({"id": "a", "type": "fill", "source": "s", "source-layer": "l", "paint": {"fill-pattern": "red"}}),
        )
        .unwrap();
        let mut pm = Pixmap::new(64, 64).unwrap();
        draw_vector(&mut pm, &item(vec![square.clone()]), &layer, &world_frame(), 0.0, Some(&sheet()), &[]);
        let px = pm.pixel(32, 32).unwrap();
        assert_eq!((px.red(), px.green(), px.alpha()), (255, 0, 255));
        // Without the sprite, or with an image it lacks, nothing is drawn (not black).
        let mut pm = Pixmap::new(64, 64).unwrap();
        draw_vector(&mut pm, &item(vec![square.clone()]), &layer, &world_frame(), 0.0, None, &[]);
        let missing = StyleLayer::parse(
            &serde_json::json!({"id": "a", "type": "fill", "source": "s", "source-layer": "l", "paint": {"fill-pattern": "nope"}}),
        )
        .unwrap();
        draw_vector(&mut pm, &item(vec![square]), &missing, &world_frame(), 0.0, Some(&sheet()), &[]);
        assert_eq!(pm.pixel(32, 32).unwrap().alpha(), 0);
    }

    /// A one-feature MVT layer with one string property, `name`.
    fn mvt_layer(name: &str, kind: GeomKind, pts: &[[f32; 2]], value: &str) -> crate::mvt::Layer {
        let geometry = match kind {
            GeomKind::Point => pts.iter().map(|p| vec![*p]).collect(),
            _ => vec![pts.to_vec()],
        };
        let props = vec![("name".to_string(), crate::mvt::Value::String(value.into()))];
        crate::mvt::Layer { name: name.into(), extent: 4096, features: vec![crate::mvt::Feature { id: Some(7), kind, geometry, props }] }
    }

    #[test]
    fn picks_the_vector_tile_features_under_a_point() {
        let tile = crate::mvt::encode(&crate::mvt::Tile {
            layers: vec![
                mvt_layer("parks", GeomKind::Polygon, &[[1024.0, 1024.0], [3072.0, 1024.0], [3072.0, 3072.0], [1024.0, 3072.0]], "Central"),
                mvt_layer("roads", GeomKind::LineString, &[[0.0, 500.0], [4096.0, 500.0]], "Main Street"),
                mvt_layer("pois", GeomKind::Point, &[[3500.0, 3500.0]], "Cafe"),
                // Two unnamed squares alike in every property, one over the other.
                crate::mvt::Layer {
                    name: "plots".into(),
                    extent: 4096,
                    features: vec![
                        crate::mvt::Feature {
                            id: None,
                            kind: GeomKind::Polygon,
                            geometry: vec![vec![[200.0, 2500.0], [800.0, 2500.0], [800.0, 3100.0], [200.0, 3100.0], [200.0, 2500.0]]],
                            props: Vec::new(),
                        };
                        2
                    ],
                },
            ],
        });
        let dir = std::env::temp_dir().join(format!("rustgis-pick-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pick.pmtiles");
        let meta = r#"{"vector_layers":[{"id":"parks"},{"id":"roads"},{"id":"pois"},{"id":"plots"}]}"#;
        std::fs::write(&path, crate::pmtiles::tests::archive(&[(0, 0, 0, tile)], 1, false, meta)).unwrap();
        let spec = Spec {
            kind: crate::source::Kind::PmTiles,
            url: path.display().to_string(),
            subdomains: Vec::new(),
            min_zoom: 0,
            max_zoom: 14,
            tile_size: 512,
            tms: false,
            attribution: String::new(),
            layers: Vec::new(),
            style_layers: String::new(),
            hidden_layers: Vec::new(),
        };
        // Zoom 0: one tile over the world, 4096 units across it.
        let half = 20_037_508.342_789_244;
        let at = |u: f64, v: f64| (-half + u / 4096.0 * 2.0 * half, half - v / 4096.0 * 2.0 * half);
        let m_per_px = 2.0 * half / 256.0;
        let names = |(x, y): (f64, f64)| -> Vec<String> {
            // The archive and its tile arrive in the background.
            for _ in 0..500 {
                let got = pick(&spec, x, y, m_per_px, 3.0, 10);
                if !got.is_empty() {
                    return got.iter().map(|p| format!("{}:{:?}", p.source_layer, p.props.first().map(|v| &v.1))).collect();
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Vec::new()
        };
        assert_eq!(names(at(2048.0, 2048.0)), vec![r#"parks:Some(String("Central"))"#]);
        assert_eq!(names(at(200.0, 505.0)), vec![r#"roads:Some(String("Main Street"))"#]);
        assert_eq!(names(at(3510.0, 3495.0)), vec![r#"pois:Some(String("Cafe"))"#]);
        // Alike, but two features: both.
        assert_eq!(names(at(500.0, 2800.0)), vec!["plots:None", "plots:None"]);
        // On the park's edge, from outside: within the tolerance. Farther: nothing (the tile is in).
        assert_eq!(names(at(1024.0 - 40.0, 2048.0)).len(), 1);
        let (x, y) = at(100.0, 3900.0);
        assert!(pick(&spec, x, y, m_per_px, 3.0, 10).is_empty());
        let (x, y) = at(1024.0 - 80.0, 2048.0);
        assert!(pick(&spec, x, y, m_per_px, 3.0, 10).is_empty());
        // What a style doesn't draw isn't picked: a label with no text, a line drawn with a pattern.
        let styled = Spec {
            style_layers: r#"[{"id": "l", "type": "symbol", "source": "x", "source-layer": "pois", "layout": {"text-field": "{missing}"}},
                {"id": "r", "type": "line", "source": "x", "source-layer": "roads", "paint": {"line-pattern": "dots"}},
                {"id": "p", "type": "fill", "source": "x", "source-layer": "plots", "paint": {"fill-color": "green"}}]"#
                .into(),
            ..spec.clone()
        };
        // (The style is read: its fill is picked.)
        let (x, y) = at(500.0, 2800.0);
        let mut got = Vec::new();
        for _ in 0..500 {
            got = pick(&styled, x, y, m_per_px, 3.0, 10);
            if !got.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(got.len(), 2);
        for (u, v) in [(3510.0, 3495.0), (200.0, 505.0)] {
            let (x, y) = at(u, v);
            for _ in 0..50 {
                assert!(pick(&styled, x, y, m_per_px, 3.0, 10).is_empty());
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        // A hidden source layer is left out of the style: not picked, the others still are.
        assert_eq!(crate::source::source_layers(spec.kind, &spec.url).unwrap(), ["parks", "roads", "pois", "plots"]);
        let without_roads = Spec { hidden_layers: vec!["roads".into()], ..spec.clone() };
        let (x, y) = at(2048.0, 2048.0);
        assert_eq!(pick(&without_roads, x, y, m_per_px, 3.0, 10).len(), 1, "the park is still there");
        let (x, y) = at(200.0, 505.0);
        assert!(pick(&without_roads, x, y, m_per_px, 3.0, 10).is_empty(), "the road isn't");
        // An own style's layers too.
        let (x, y) = at(500.0, 2800.0);
        assert_eq!(pick(&Spec { hidden_layers: vec!["roads".into()], ..styled.clone() }, x, y, m_per_px, 3.0, 10).len(), 2);
        assert!(pick(&Spec { hidden_layers: vec!["plots".into()], ..styled.clone() }, x, y, m_per_px, 3.0, 10).is_empty());
        // Hostile scales and points pick nothing.
        for (x, y, m) in [(f64::NAN, 0.0, m_per_px), (0.0, 0.0, 0.0), (0.0, 0.0, f64::INFINITY)] {
            assert!(pick(&spec, x, y, m, 3.0, 10).is_empty());
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn picks_a_feature_across_a_tile_edge() {
        // Zoom 1: a point at the right edge of the western tile (1/0/0); the click is just over
        // the edge, in the eastern one (1/1/0).
        let tile = crate::mvt::encode(&crate::mvt::Tile { layers: vec![mvt_layer("pois", GeomKind::Point, &[[4094.0, 2048.0]], "Edge")] });
        let new = crate::pmtiles::NewArchive {
            tile_type: crate::pmtiles::TileType::Mvt,
            gzipped_tiles: false,
            bounds: [-180.0, -85.0, 180.0, 85.0],
            metadata: serde_json::json!({"vector_layers": [{"id": "pois"}]}),
        };
        let tiles = vec![(1, 0, 0, tile), (1, 1, 0, crate::mvt::encode(&crate::mvt::Tile::default()))];
        let dir = std::env::temp_dir().join(format!("rustgis-pick-edge-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("edge.pmtiles");
        std::fs::write(&path, crate::pmtiles::write(tiles, &new).unwrap()).unwrap();
        let spec = Spec {
            kind: crate::source::Kind::PmTiles,
            url: path.display().to_string(),
            subdomains: Vec::new(),
            min_zoom: 0,
            max_zoom: 14,
            tile_size: 512,
            tms: false,
            attribution: String::new(),
            layers: Vec::new(),
            style_layers: String::new(),
            hidden_layers: Vec::new(),
        };
        let half = 20_037_508.342_789_244;
        // Just inside zoom 1 (the archive has no other).
        let m_per_px = half / 512.0 * 0.9;
        // 4094 of 4096 units: under a pixel west of the edge at this scale; the click a pixel east of it.
        let (x, y) = (m_per_px, half / 2.0);
        let mut got = Vec::new();
        for _ in 0..500 {
            got = pick(&spec, x, y, m_per_px, 3.0, 10);
            if !got.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(got.iter().map(|p| p.source_layer.as_str()).collect::<Vec<_>>(), vec!["pois"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wide_lines_reach_in_from_past_the_pixmap() {
        // The top-left 64 pixels of a tile drawn 256 across; a road 120 points wide runs 100
        // pixels down, its edge 40 pixels down: in the pixmap, though its line isn't.
        let road = crate::mvt::Feature {
            id: None,
            kind: GeomKind::LineString,
            geometry: vec![vec![[0.0, 1600.0], [4096.0, 1600.0]]],
            props: Vec::new(),
        };
        let layer = StyleLayer::parse(&serde_json::json!({"id": "r", "type": "line", "source": "s", "source-layer": "l",
            "layout": {"line-join": "round"}, "paint": {"line-color": "#f00", "line-width": 120}}))
        .unwrap();
        assert_eq!(overhang(&layer, 0.0), 90.0);
        let it = item(vec![road]);
        let half = crate::template::HALF_WORLD;
        let frame = Frame { left: -half, top: half, m_per_px: 2.0 * half / 256.0, ppp: 1.0 };
        let mut pm = Pixmap::new(64, 64).unwrap();
        let mut boxes = HashMap::new();
        let b = boxes_of(&mut boxes, &it, &layer);
        draw_vector(&mut pm, &it, &layer, &frame, 0.0, None, &b);
        assert_eq!(pm.pixel(32, 60).unwrap().red(), 255);
        assert_eq!(pm.pixel(32, 30).unwrap().alpha(), 0);
    }

    #[test]
    fn dashed_lines_know_how_far_along_they_are_cut() {
        // In at 10 along, out, and in again at 30 along the line.
        let pieces = clip_line_along(&[(-10.0, 5.0), (5.0, 5.0), (5.0, 15.0), (5.0, 25.0), (5.0, 0.0)], [0.0, 0.0, 10.0, 10.0]);
        let near = |a: &[(f32, f32)], b: &[(f32, f32)]| {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| (a.0 - b.0).abs() + (a.1 - b.1).abs() < 1e-3)
        };
        assert_eq!(pieces.len(), 2);
        assert!(near(&pieces[0].0, &[(0.0, 5.0), (5.0, 5.0), (5.0, 10.0)]) && (pieces[0].1 - 10.0).abs() < 1e-3, "{pieces:?}");
        assert!(near(&pieces[1].0, &[(5.0, 10.0), (5.0, 0.0)]) && (pieces[1].1 - 50.0).abs() < 1e-3, "{pieces:?}");
        assert!(clip_line_along(&[(f32::NAN, 0.0), (1.0, 1.0)], [0.0, 0.0, 10.0, 10.0]).len() <= 1);
    }

    #[test]
    fn copies_pictures_as_a_pattern_draws_them() {
        let mut img = Pixmap::new(8, 8).unwrap();
        for (i, p) in img.pixels_mut().iter_mut().enumerate() {
            let a = [255, 128, 0, 40][i % 4];
            *p = tiny_skia::ColorU8::from_rgba((i * 7 % 256) as u8, 90, 200, a).premultiply();
        }
        let it = Item {
            payload: Arc::new(Payload::Image(img.clone())),
            at: TileId { z: 0, x: 0, y: 0 }.extent(),
            z: 0,
            clip: TileId { z: 0, x: 0, y: 0 }.extent(),
        };
        // The world 8 pixels across, its corner 2.4 pixels in.
        let half = crate::template::HALF_WORLD;
        let frame =
            Frame { left: -half - 2.4 * (2.0 * half / 8.0), top: half + 1.6 * (2.0 * half / 8.0), m_per_px: 2.0 * half / 8.0, ppp: 1.0 };
        for opacity in [1.0, 0.5] {
            let mut copied = Pixmap::new(12, 12).unwrap();
            copied.fill(tiny_skia::Color::from_rgba8(10, 200, 30, 255));
            let mut painted = copied.clone();
            draw_raster(&mut copied, &it, &frame, opacity);
            // As a pattern fill would.
            let paint = Paint {
                shader: Pattern::new(img.as_ref(), SpreadMode::Pad, FilterQuality::Nearest, opacity, Transform::from_translate(2.4, 1.6)),
                anti_alias: false,
                ..Paint::default()
            };
            painted.fill_rect(clip_rect(&it, &frame).unwrap(), &paint, Transform::identity(), None);
            let worst = copied.data().iter().zip(painted.data()).map(|(a, b)| a.abs_diff(*b)).max();
            assert!(worst <= Some(2), "opacity {opacity}: {worst:?}");
        }
        // Off the pixmap: nothing, and no panic.
        let mut pm = Pixmap::new(4, 4).unwrap();
        copy_over(&mut pm, &Pixmap::new(2, 2).unwrap(), Rect::from_ltrb(-50.0, -50.0, -10.0, -10.0).unwrap(), (-50.0, -50.0), 1.0, 1.0);
        copy_over(&mut pm, &Pixmap::new(2, 2).unwrap(), Rect::from_ltrb(0.0, 0.0, 1e9, 1e9).unwrap(), (f32::NAN, 0.0), 1.0, 1.0);
        assert!(pm.data().iter().all(|b| *b == 0));
    }

    #[test]
    fn clips_polygons_and_lines() {
        let sq = [(-5.0, -5.0), (15.0, -5.0), (15.0, 15.0), (-5.0, 15.0)];
        let c = clip_polygon(&sq, [0.0, 0.0, 10.0, 10.0]);
        assert!((area(&c) - 100.0).abs() < 1e-3);
        assert!(clip_polygon(&[(20.0, 20.0), (30.0, 20.0), (30.0, 30.0)], [0.0, 0.0, 10.0, 10.0]).is_empty());
        let pieces = clip_line(&[(-5.0, 5.0), (5.0, 5.0), (5.0, 15.0), (5.0, 20.0), (8.0, 8.0)], [0.0, 0.0, 10.0, 10.0]);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0], vec![(0.0, 5.0), (5.0, 5.0), (5.0, 10.0)]);
        assert!(clip_line(&[(f32::NAN, 0.0), (1.0, 1.0)], [0.0, 0.0, 10.0, 10.0]).len() <= 1);
    }
}
