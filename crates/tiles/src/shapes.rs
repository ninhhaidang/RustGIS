//! A style's vector shapes tile by tile, for a renderer that turns them into meshes once and
//! draws them at any zoom (the GPU map, as MapLibre draws its tiles).
//!
//! [`vector_tiles`] gives the data tiles of a style over a view (fetching those missing, with
//! stand-ins for them as [`draw`](crate::draw::draw) has), and [`shapes`] the fills, lines and
//! circles one of them draws, in the style's order. A tile's shapes serve every zoom of a
//! level: those of `level` and of the level above are given for every paint value that changes
//! with the zoom (colours, widths, radii), for the renderer to blend between, and its filters
//! and the rest are taken at the level itself. They are drawn as [`draw`](crate::draw::draw)
//! draws them, but for what that ignores too (`line-gap-width`, `line-offset`,
//! `fill-translate`).

use std::sync::Arc;

use rustgis_geom::Extent;

use crate::cache::Payload;
use crate::draw::{Drawn, Frame, Label, Pass, draw_layers, tiles_in};
use crate::mvt::{GeomKind, Tile};
use crate::source::{Resolve, Resolved, Spec, resolve};
use crate::sprite::{self, Load, Sprite};
use crate::style::{FeatureCtx, LayerKind, LineCap, LineJoin, PATTERNS, Rgba, Source, StyleLayer};
use crate::template;

/// The width of a fill's outline, points (as [`draw`](crate::draw::draw) strokes it).
pub const OUTLINE_WIDTH: f32 = 0.75;

/// The most lengths a dash pattern drawn from [`shapes`] has, an odd count repeated once over
/// (as tiny-skia draws it): a style showing a longer one is declined.
pub const MAX_DASHES: usize = 4;

/// The data tiles of a style's vector source over a view.
pub struct VectorTiles {
    /// The resolved style ([`shapes`] reads its layers).
    pub style: Arc<Resolved>,
    /// The source's id in the style.
    pub source: String,
    /// Bottom first: stand-ins for missing tiles, then the tiles themselves.
    pub tiles: Vec<DataTile>,
    /// The level the view's style zoom is at: shapes for it are drawn.
    pub level: u8,
    /// Its labels, when asked for, for the renderer to place with the map's own.
    pub labels: Vec<Label>,
    pub attribution: Vec<String>,
    /// Some tiles (or the sprite) are still on their way.
    pub pending: bool,
    /// What of the style was left out and why, as a drawing's error.
    pub error: Option<String>,
}

/// One data tile placed on the map.
#[derive(Clone)]
pub struct DataTile {
    /// The tile, shared with the tile cache (a [`Payload::Vector`]).
    pub payload: Arc<Payload>,
    /// The Web Mercator square it covers.
    pub at: Extent,
    /// Its zoom.
    pub z: u8,
    /// The square it is drawn in: `at` itself, or the missing tile it stands in for.
    pub clip: Extent,
}

/// The data tiles of `spec` over `view` (fetching those missing) and out to `reach` (those the
/// cache has), drawn at `frame`'s scale, when they can be drawn from [`shapes`] alone; `None`
/// otherwise (the style isn't ready, has more than one vector source or shows raster layers
/// here), for the caller to draw it as before. With `labels`, the labels of a pixmap that size
/// at `frame`.
pub fn vector_tiles(spec: &Spec, frame: &Frame, view: &Extent, reach: Option<&Extent>, labels: Option<(u32, u32)>) -> Option<VectorTiles> {
    let Resolve::Ready(r) = resolve(spec) else { return None };
    if !(frame.m_per_px.is_finite() && frame.m_per_px > 0.0 && frame.ppp.is_finite() && frame.ppp > 0.0) {
        return None;
    }
    let zoom = template::style_zoom(frame.m_per_px * f64::from(frame.ppp));
    let level = zoom.floor().clamp(0.0, 30.0) as u8;
    let source = drawn_source(&r, level)?;
    let src = r.style.sources.iter().find(|(id, _)| *id == source).map(|(_, s)| s)?;
    let mut pending = false;
    let tiles = tiles_in(src, view, reach, frame, zoom, &mut pending)
        .into_iter()
        .map(|it| DataTile { payload: it.payload, at: it.at, z: it.z, clip: it.clip })
        .collect();
    let mut d = Drawn::default();
    if let Some(size) = labels {
        draw_layers(None, size, &frame.extent(size.0, size.1), &r, frame, Pass::Labels, &mut d);
    }
    let attribution = r.attribution.clone();
    let error = crate::draw::left_out(&r, frame, true);
    Some(VectorTiles { style: r, source, tiles, level, labels: d.labels, attribution, pending: pending || d.pending, error })
}

/// Whether `style` fills with patterns at any zoom of `level`: only then do its meshes need the
/// sprites.
pub fn draws_patterns(style: &Resolved, level: u8) -> bool {
    let (lo, hi) = (f32::from(level), f32::from(level) + 1.0);
    style.style.layers.iter().any(|l| shown_in(l, lo, hi) && l.draws_patterns())
}

/// The one vector source whose shapes are all a style shows at any zoom of `level` (besides its
/// background and labels), if there is one: a mesh made for the level serves all of them.
fn drawn_source(r: &Resolved, level: u8) -> Option<String> {
    let (lo, hi) = (f32::from(level), f32::from(level) + 1.0);
    let mut source: Option<&str> = None;
    for l in r.style.layers.iter().filter(|l| shown_in(l, lo, hi)) {
        match l.kind {
            LayerKind::Background | LayerKind::Symbol | LayerKind::Other => {}
            // Lines painted with a pattern aren't drawn (see `draw::drawable`).
            LayerKind::Line if l.has_paint("line-pattern") => {}
            // A mesh's fill has one pattern's colour for the level: one changing with the zoom
            // is drawn zoom by zoom. (Its name may be an `image`, so only the expression tells.)
            LayerKind::Fill | LayerKind::FillExtrusion if PATTERNS.iter().any(|p| l.paint_uses_zoom(p)) => return None,
            LayerKind::Fill | LayerKind::FillExtrusion | LayerKind::Line | LayerKind::Circle => {
                let at = FeatureCtx::zoom_only;
                // The level runs up to the next, excluded: what steps there belongs to it.
                let top = hi.next_down();
                if [lo, top].iter().any(|&z| l.line_dasharray(&at(z)).is_some_and(|d| d.len() * (1 + d.len() % 2) > MAX_DASHES)) {
                    return None;
                }
                // Dashes, caps and joins are taken at the level: a style changing them within it
                // is drawn otherwise.
                if l.kind == LayerKind::Line
                    && (l.line_dasharray(&at(lo)) != l.line_dasharray(&at(top))
                        || l.line_cap(&at(lo)) != l.line_cap(&at(top))
                        || l.line_join(&at(lo)) != l.line_join(&at(top)))
                {
                    return None;
                }
                let id = l.source.as_deref()?;
                if !r.style.sources.iter().any(|(s, src)| s == id && matches!(src, Source::Vector { .. })) {
                    return None;
                }
                match source {
                    Some(s) if s != id => return None,
                    _ => source = Some(id),
                }
            }
            LayerKind::Raster | LayerKind::Hillshade | LayerKind::ColorRelief => return None,
        }
    }
    source.map(String::from)
}

/// The shapes of one style layer in a tile, the zooms they show at, and the size of the tile's
/// square in its own units.
pub struct Run {
    /// The style layer's place in the style (its draw order).
    pub layer: usize,
    /// It shows from `min_zoom` up to `max_zoom` (excluded).
    pub min_zoom: f32,
    pub max_zoom: f32,
    /// The tile layer's side, in tile units (its features' coordinates run `0..extent`, y down).
    pub extent: u32,
    pub shapes: Vec<Shape>,
}

/// One shape of a run.
pub struct Shape {
    /// Which feature of the tile layer (by its place), or `None` for the whole square (a
    /// background).
    pub feature: Option<(usize, usize)>,
    pub paint: Paint,
}

/// A value at the level and at the level above.
pub type Pair<T> = [T; 2];

/// How a shape is painted. Colours are straight RGBA, 0..1, their opacity applied.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    /// Polygons filled (even-odd) with `color`. A fill with the sprite's image `pattern` (to
    /// be repeated at its own size in points) has its image's average colour for `color`.
    Fill { color: Pair<[f32; 4]>, pattern: Option<String> },
    /// Lines, or a polygon's rings: `width` points wide, `dashes` (if any) in widths.
    Line { color: Pair<[f32; 4]>, width: Pair<f32>, cap: LineCap, join: LineJoin, dashes: Option<Vec<f32>>, rings: bool },
    /// Points as discs `radius` points across, outlined by a ring `stroke.1` points wide
    /// centred on their edge.
    Circle { color: Pair<[f32; 4]>, radius: Pair<f32>, stroke: Option<(Pair<[f32; 4]>, Pair<f32>)> },
}

fn rgba(c: Rgba, opacity: f32) -> [f32; 4] {
    let a = f32::from(c[3]) / 255.0 * opacity.clamp(0.0, 1.0);
    [f32::from(c[0]) / 255.0, f32::from(c[1]) / 255.0, f32::from(c[2]) / 255.0, a.clamp(0.0, 1.0)]
}

/// Widths and radii as [`draw`](crate::draw::draw) takes them: finite and at most `max`, else 0.
fn sane(v: f32, max: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, max) } else { 0.0 }
}

/// Whether `l` shows at some zoom from `lo` up to `hi`.
fn shown_in(l: &StyleLayer, lo: f32, hi: f32) -> bool {
    l.visible && l.min_zoom < hi && l.max_zoom > lo
}

/// What a tile draws (see [`shapes`]).
pub struct TileShapes {
    pub runs: Vec<Run>,
    /// Pattern fills are left out until the sprite has arrived.
    pub waiting: bool,
    /// The ids of the style's sprites its pattern fills were made from, if it has any: other
    /// sprites read (one that failed read after all, say) change them.
    pub sheets: Option<Vec<String>>,
}

/// The average colour of a sprite's image (straight RGBA, 0..1).
fn average(img: &tiny_skia::Pixmap) -> Option<[f32; 4]> {
    let n = img.pixels().len();
    if n == 0 {
        return None;
    }
    let mut sum = [0.0f64; 4];
    for p in img.pixels() {
        for (s, v) in sum.iter_mut().zip([p.red(), p.green(), p.blue(), p.alpha()]) {
            *s += f64::from(v);
        }
    }
    let a = sum[3];
    if a <= 0.0 {
        return Some([0.0; 4]);
    }
    // Premultiplied sums: the colour is theirs over the alpha's.
    Some([(sum[0] / a) as f32, (sum[1] / a) as f32, (sum[2] / a) as f32, (a / n as f64 / 255.0) as f32])
}

/// The fills, lines and circles `style` draws from `tile` of `source` at zooms `level` up to
/// the next level, in its order. Its background, when it has one, is the first run, filling the
/// square.
pub fn shapes(style: &Resolved, source: &str, tile: &Tile, level: u8) -> TileShapes {
    shapes_with(style, source, tile, level, &|| sprite::load(&style.sprites, false))
}

/// [`shapes`], with `load` to load the style's sprites.
fn shapes_with(style: &Resolved, source: &str, tile: &Tile, level: u8, load: &dyn Fn() -> Load) -> TileShapes {
    // The sprite, loaded the first time a pattern needs it.
    let mut sheet: Option<Option<Arc<Sprite>>> = None;
    // Each pattern's average colour, found once (a pass over its pixels).
    let mut averages: std::collections::HashMap<String, Option<[f32; 4]>> = std::collections::HashMap::new();
    let mut waiting = false;
    let (lo, hi) = (f32::from(level), f32::from(level) + 1.0);
    // The level's last zoom (the next level excluded): a step at the next level isn't blended
    // across this one; an interpolation is the same there, near enough.
    let top = hi.next_down();
    let shown = |l: &StyleLayer| shown_in(l, lo, hi);
    let mut runs = Vec::new();
    for (li, layer) in style.style.layers.iter().enumerate() {
        if !shown(layer) {
            continue;
        }
        if layer.kind == LayerKind::Background {
            let at = |z: f32| layer.background_color(z).map(|c| rgba(c, layer.background_opacity(z)));
            if let (Some(a), Some(b)) = (at(lo), at(top)) {
                let paint = Paint::Fill { color: [a, b], pattern: None };
                let extent = tile.layers.first().map_or(4096, |l| l.extent);
                runs.push(Run {
                    layer: li,
                    min_zoom: layer.min_zoom,
                    max_zoom: layer.max_zoom,
                    extent,
                    shapes: vec![Shape { feature: None, paint }],
                });
            }
            continue;
        }
        if !matches!(layer.kind, LayerKind::Fill | LayerKind::FillExtrusion | LayerKind::Line | LayerKind::Circle)
            || layer.source.as_deref() != Some(source)
            || layer.has_paint("line-pattern")
        {
            continue;
        }
        let Some(name) = layer.source_layer.as_deref() else { continue };
        let Some((tli, tl)) = tile.layers.iter().enumerate().find(|(_, l)| l.name == name) else { continue };
        let mut shapes = Vec::new();
        let free = layer.paint_feature_free();
        let mut fixed: [Option<Vec<Paint>>; 4] = Default::default();
        for (fi, f) in tl.features.iter().enumerate() {
            let ctx = |zoom| FeatureCtx { zoom, kind: f.kind, id: f.id, props: &f.props, images: None };
            if !layer.matches(&ctx(lo)) {
                continue;
            }
            // The same for every feature: worked out once (for each kind of geometry).
            let kind = f.kind as usize;
            if let Some(Some(paints)) = fixed.get(kind) {
                shapes.extend(paints.iter().map(|paint| Shape { feature: Some((tli, fi)), paint: paint.clone() }));
                continue;
            }
            let paints: Vec<Paint> = 'p: {
                let (a, b) = (ctx(lo), ctx(top));
                let mut out = Vec::new();
                let paint = match layer.kind {
                    LayerKind::Fill | LayerKind::FillExtrusion if f.kind == GeomKind::Polygon => {
                        let (Some(ca), Some(cb)) = (layer.fill_color(&a), layer.fill_color(&b)) else { break 'p out };
                        let (oa, ob) = (layer.fill_opacity(&a), layer.fill_opacity(&b));
                        let mut color = [rgba(ca, oa), rgba(cb, ob)];
                        // A pattern names an image of the sprite; one that names none isn't drawn.
                        let pattern = match PATTERNS.into_iter().find(|p| layer.has_paint(p)) {
                            Some(p) => {
                                // A feature with no name needs no sprite, unless it is the sprite
                                // that says (an `image` expression).
                                let named = |v: &crate::style::Val| v.as_str().is_some_and(|n| !n.is_empty());
                                if !layer.paint_uses_image(p) && !layer.paint_value(p, &a).is_some_and(|v| named(&v)) {
                                    break 'p out;
                                }
                                let sprite = sheet.get_or_insert_with(|| match load() {
                                    Load::Ready(s) => Some(s),
                                    Load::Pending => {
                                        waiting = true;
                                        None
                                    }
                                    Load::Failed => None,
                                });
                                let images = sprite.as_deref();
                                let Some(crate::style::Val::Str(name)) = layer.paint_value(p, &FeatureCtx { images, ..a }).filter(named)
                                else {
                                    break 'p out;
                                };
                                let found = match averages.get(&name) {
                                    Some(a) => *a,
                                    None => {
                                        let a = sprite.as_ref().and_then(|s| s.image(&name, None)).and_then(|img| average(&img.pixmap));
                                        // Only once the sprite is here: until then it isn't known.
                                        if sprite.is_some() {
                                            averages.insert(name.clone(), a);
                                        }
                                        a
                                    }
                                };
                                let Some(avg) = found else {
                                    break 'p out;
                                };
                                color = [
                                    [avg[0], avg[1], avg[2], avg[3] * oa.clamp(0.0, 1.0)],
                                    [avg[0], avg[1], avg[2], avg[3] * ob.clamp(0.0, 1.0)],
                                ];
                                Some(name)
                            }
                            None => None,
                        };
                        let paint = Paint::Fill { color, pattern };
                        out.push(paint);
                        // The outline over it, along the rings themselves.
                        let (Some(la), Some(lb)) = (layer.fill_outline_color(&a), layer.fill_outline_color(&b)) else { break 'p out };
                        Paint::Line {
                            color: [rgba(la, oa), rgba(lb, ob)],
                            width: [OUTLINE_WIDTH; 2],
                            cap: LineCap::Butt,
                            join: LineJoin::Miter,
                            dashes: None,
                            rings: true,
                        }
                    }
                    LayerKind::Line if matches!(f.kind, GeomKind::LineString | GeomKind::Polygon) => {
                        let (Some(ca), Some(cb)) = (layer.line_color(&a), layer.line_color(&b)) else { break 'p out };
                        let width = [sane(layer.line_width(&a), 512.0), sane(layer.line_width(&b), 512.0)];
                        if width == [0.0, 0.0] {
                            break 'p out;
                        }
                        Paint::Line {
                            color: [rgba(ca, layer.line_opacity(&a)), rgba(cb, layer.line_opacity(&b))],
                            width,
                            cap: layer.line_cap(&a),
                            join: layer.line_join(&a),
                            dashes: layer.line_dasharray(&a),
                            rings: f.kind == GeomKind::Polygon,
                        }
                    }
                    LayerKind::Circle if f.kind == GeomKind::Point => {
                        let (Some(ca), Some(cb)) = (layer.circle_color(&a), layer.circle_color(&b)) else { break 'p out };
                        let radius = [sane(layer.circle_radius(&a), 256.0), sane(layer.circle_radius(&b), 256.0)];
                        if radius == [0.0, 0.0] {
                            break 'p out;
                        }
                        let (oa, ob) = (layer.circle_opacity(&a), layer.circle_opacity(&b));
                        let stroke = match (layer.circle_stroke_color(&a), layer.circle_stroke_color(&b)) {
                            (Some(sa), Some(sb)) => {
                                let w = [sane(layer.circle_stroke_width(&a), 64.0), sane(layer.circle_stroke_width(&b), 64.0)];
                                (w != [0.0, 0.0]).then_some(([rgba(sa, oa), rgba(sb, ob)], w))
                            }
                            _ => None,
                        };
                        Paint::Circle { color: [rgba(ca, oa), rgba(cb, ob)], radius, stroke }
                    }
                    _ => break 'p out,
                };
                out.push(paint);
                out
            };
            if free && let Some(slot) = fixed.get_mut(kind) {
                *slot = Some(paints.clone());
            }
            shapes.extend(paints.into_iter().map(|paint| Shape { feature: Some((tli, fi)), paint }));
        }
        if !shapes.is_empty() {
            runs.push(Run { layer: li, min_zoom: layer.min_zoom, max_zoom: layer.max_zoom, extent: tl.extent, shapes });
        }
    }
    // (None could be read, or none yet: none.)
    let sheets = sheet.map(|s| s.map_or_else(Vec::new, |s| s.sheet_ids()));
    TileShapes { runs, waiting, sheets }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::{Feature, Layer, Value};
    use crate::source::Kind;

    /// A one-tile archive (zoom 0) of a park, two roads and a cafe, with `style` drawing it.
    fn archive(name: &str, style: &str) -> (Spec, std::path::PathBuf) {
        let feature = |kind, geometry: Vec<Vec<[f32; 2]>>, class: &str| Feature {
            id: None,
            kind,
            geometry,
            props: vec![("class".into(), Value::String(class.into()))],
        };
        let tile = Tile {
            layers: vec![
                Layer {
                    name: "parks".into(),
                    extent: 4096,
                    features: vec![feature(
                        GeomKind::Polygon,
                        vec![vec![[1000.0, 1000.0], [3000.0, 1000.0], [3000.0, 3000.0], [1000.0, 1000.0]]],
                        "park",
                    )],
                },
                Layer {
                    name: "roads".into(),
                    extent: 4096,
                    features: vec![
                        feature(GeomKind::LineString, vec![vec![[0.0, 300.0], [4096.0, 3900.0]]], "major"),
                        feature(GeomKind::LineString, vec![vec![[200.0, 3800.0], [3900.0, 200.0]]], "path"),
                    ],
                },
                Layer { name: "pois".into(), extent: 4096, features: vec![feature(GeomKind::Point, vec![vec![[2048.0, 2048.0]]], "cafe")] },
            ],
        };
        let dir = std::env::temp_dir().join(format!("rustgis-shapes-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.pmtiles");
        let new = crate::pmtiles::NewArchive {
            tile_type: crate::pmtiles::TileType::Mvt,
            gzipped_tiles: false,
            bounds: [-180.0, -85.0, 180.0, 85.0],
            metadata: serde_json::json!({"vector_layers": [{"id": "parks"}, {"id": "roads"}, {"id": "pois"}]}),
        };
        std::fs::write(&path, crate::pmtiles::write(vec![(0, 0, 0, crate::mvt::encode(&tile))], &new).unwrap()).unwrap();
        let spec = Spec {
            kind: Kind::PmTiles,
            url: path.display().to_string(),
            subdomains: Vec::new(),
            min_zoom: 0,
            max_zoom: 14,
            tile_size: 512,
            tms: false,
            attribution: String::new(),
            layers: Vec::new(),
            style_layers: style.into(),
            hidden_layers: Vec::new(),
        };
        (spec, dir)
    }

    const STYLE: &str = r##"[
        {"id": "bg", "type": "background", "paint": {"background-color": "#f0eee8"}},
        {"id": "park", "type": "fill", "source": "x", "source-layer": "parks",
            "paint": {"fill-color": "#99cc99", "fill-opacity": 0.5, "fill-outline-color": "#336633"}},
        {"id": "major", "type": "line", "source": "x", "source-layer": "roads", "filter": ["==", "class", "major"],
            "layout": {"line-join": "round", "line-cap": "round"},
            "paint": {"line-color": "#ee9922", "line-width": ["interpolate", ["linear"], ["zoom"], 0, 2, 4, 10]}},
        {"id": "path", "type": "line", "source": "x", "source-layer": "roads", "filter": ["==", "class", "path"],
            "paint": {"line-color": "#333333", "line-width": 2, "line-dasharray": [3, 2]}},
        {"id": "poi", "type": "circle", "source": "x", "source-layer": "pois",
            "paint": {"circle-color": "#cc0000", "circle-radius": 7, "circle-stroke-color": "#ffffff", "circle-stroke-width": 2}},
        {"id": "label", "type": "symbol", "source": "x", "source-layer": "pois", "layout": {"text-field": ["get", "class"]}}
    ]"##;

    fn ready(spec: &Spec) -> Arc<Resolved> {
        for _ in 0..500 {
            if let Resolve::Ready(r) = resolve(spec) {
                return r;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the archive wasn't read");
    }

    /// A 300 × 200 pixel view at `zoom` about the world's middle.
    fn frame(zoom: f64) -> Frame {
        let m_per_px = 2.0 * template::HALF_WORLD / (512.0 * zoom.exp2());
        Frame { left: -150.0 * m_per_px, top: 100.0 * m_per_px, m_per_px, ppp: 1.0 }
    }

    fn tile(spec: &Spec, zoom: f64) -> VectorTiles {
        for _ in 0..500 {
            let f = frame(zoom);
            if let Some(v) = vector_tiles(spec, &f, &f.extent(300, 200), None, Some((300, 200)))
                && !v.pending
                && !v.tiles.is_empty()
            {
                return v;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("no tiles");
    }

    #[test]
    fn shapes_are_what_the_style_draws() {
        let _turn = crate::cache::turn();
        let (spec, dir) = archive("draws", STYLE);
        ready(&spec);
        let v = tile(&spec, 2.5);
        assert_eq!(v.level, 2);
        let Payload::Vector(t) = &*v.tiles[0].payload else { panic!("a vector tile") };
        let runs = shapes(&v.style, &v.source, t, v.level).runs;
        // In the style's order, the labels left to the renderer's own placement.
        assert_eq!(runs.iter().map(|r| r.layer).collect::<Vec<_>>(), [0, 1, 2, 3, 4]);
        // The background fills the square.
        assert!(runs[0].shapes[0].feature.is_none());
        // The park, half seen through, and its outline over it.
        let park: Vec<&Paint> = runs[1].shapes.iter().map(|s| &s.paint).collect();
        assert!(matches!(park[0], Paint::Fill { color, pattern: None } if (color[0][3] - 0.5).abs() < 1e-3));
        assert!(matches!(park[1], Paint::Line { width, rings: true, .. } if *width == [OUTLINE_WIDTH; 2]));
        // Each road by its filter, the major one's width at the level and the next.
        assert_eq!(runs[2].shapes.len(), 1);
        let Paint::Line { width, cap, join, dashes, .. } = &runs[2].shapes[0].paint else { panic!("a line") };
        assert!((width[0] - 6.0).abs() < 1e-3 && (width[1] - 8.0).abs() < 1e-3);
        assert_eq!((*cap, *join, dashes.is_none()), (LineCap::Round, LineJoin::Round, true));
        assert!(matches!(&runs[3].shapes[0].paint, Paint::Line { dashes: Some(d), .. } if d == &[3.0, 2.0]));
        let Paint::Circle { radius, stroke: Some((_, w)), .. } = &runs[4].shapes[0].paint else { panic!("a circle") };
        assert_eq!((*radius, *w), ([7.0, 7.0], [2.0, 2.0]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn patterns_without_a_sprite_and_line_patterns_are_left_out() {
        let _turn = crate::cache::turn();
        let style = STYLE.replacen(
            '[',
            r#"[{"id": "p", "type": "fill", "source": "x", "source-layer": "parks", "paint": {"fill-pattern": "dots"}},
                {"id": "lp", "type": "line", "source": "x", "source-layer": "roads", "paint": {"line-pattern": "arrows"}},"#,
            1,
        );
        let (spec, dir) = archive("patterns", &style);
        ready(&spec);
        let v = tile(&spec, 2.5);
        let Payload::Vector(t) = &*v.tiles[0].payload else { panic!("a vector tile") };
        let s = shapes(&v.style, &v.source, t, v.level);
        // As `draw` leaves them out: the style has no sprite to take the pattern from.
        assert!(!s.waiting && s.runs.iter().all(|r| r.layer > 1));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pattern_chosen_with_image_asks_the_loaded_sprite() {
        let _turn = crate::cache::turn();
        let pattern = |name: &str, value: &str| {
            let style = STYLE.replacen(
                '[',
                &format!(
                    r#"[{{"id": "p", "type": "fill", "source": "x", "source-layer": "parks", "paint": {{"fill-pattern": {value}}}}},"#
                ),
                1,
            );
            let (spec, dir) = archive(name, &style);
            ready(&spec);
            (tile(&spec, 2.5), dir)
        };
        let shaped = |v: &VectorTiles, load: &dyn Fn() -> Load| {
            let Payload::Vector(t) = &*v.tiles[0].payload else { panic!("a vector tile") };
            shapes_with(&v.style, &v.source, t, v.level, load)
        };
        let loaded = || Load::Ready(Arc::new(crate::draw::tests::sheet()));
        let (v, dir) = pattern("image-pattern", r#"["coalesce", ["image", "nope"], ["image", "red"]]"#);
        // With the sprite: the fill is drawn, and says from how many sprites.
        let s = shaped(&v, &loaded);
        assert!(!s.waiting && s.sheets == Some(vec!["default".to_owned()]) && s.runs.iter().any(|r| r.layer == 0));
        // While it's on its way: left out, and made again.
        let s = shaped(&v, &|| Load::Pending);
        assert!(s.waiting && s.sheets == Some(Vec::new()) && s.runs.iter().all(|r| r.layer != 0));
        // Failed: left out, and not waiting.
        let s = shaped(&v, &|| Load::Failed);
        assert!(!s.waiting && s.sheets == Some(Vec::new()) && s.runs.iter().all(|r| r.layer != 0));
        let _ = std::fs::remove_dir_all(dir);
        // A name no feature has, with no `image`: nothing asks the sprite, so nothing waits for it.
        let (v, dir) = pattern("no-pattern-name", r#"["get", "no-such-field"]"#);
        let s = shaped(&v, &|| Load::Pending);
        assert!(!s.waiting && s.sheets.is_none() && s.runs.iter().all(|r| r.layer != 0));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pattern_changing_with_the_zoom_is_drawn_zoom_by_zoom() {
        let _turn = crate::cache::turn();
        let source = |name: &str, value: &str| {
            let style = STYLE.replacen(
                '[',
                &format!(
                    r#"[{{"id": "p", "type": "fill", "source": "x", "source-layer": "parks", "paint": {{"fill-pattern": {value}}}}},"#
                ),
                1,
            );
            let (spec, dir) = archive(name, &style);
            let found = drawn_source(&ready(&spec), 2);
            let _ = std::fs::remove_dir_all(dir);
            found
        };
        // One mesh colour for the level can't show a step within it, whatever names the steps give.
        assert!(source("zoom-image-pattern", r#"["step", ["zoom"], ["image", "red"], 2.5, ["image", "blue"]]"#).is_none());
        assert!(source("zoom-named-pattern", r#"["step", ["zoom"], "red", 2.5, "blue"]"#).is_none());
        assert!(source("fixed-pattern", r#""red""#).is_some());
    }

    #[test]
    fn a_step_at_the_next_level_holds_over_this_one() {
        let _turn = crate::cache::turn();
        let style = STYLE.replace(r#""line-width": 2,"#, r#""line-width": ["step", ["zoom"], 2, 3, 6],"#);
        let (spec, dir) = archive("step", &style);
        ready(&spec);
        let v = tile(&spec, 2.5);
        let Payload::Vector(t) = &*v.tiles[0].payload else { panic!("a vector tile") };
        let runs = shapes(&v.style, &v.source, t, v.level).runs;
        assert!(matches!(&runs[3].shapes[0].paint, Paint::Line { width, .. } if *width == [2.0, 2.0]));
        // An interpolation, still at the next level's value.
        assert!(matches!(&runs[2].shapes[0].paint, Paint::Line { width, .. } if (width[1] - 8.0).abs() < 1e-3));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn styles_are_judged_over_the_whole_level() {
        let _turn = crate::cache::turn();
        // A raster layer from the middle of the level: its meshes would serve where it shows.
        let style = STYLE.replacen('[', r#"[{"id": "r", "type": "raster", "source": "x", "minzoom": 2.8},"#, 1);
        let (spec, dir) = archive("level", &style);
        ready(&spec);
        let f = frame(2.5);
        assert!(vector_tiles(&spec, &f, &f.extent(300, 200), None, None).is_none());
        // Shown from the next level only: these meshes are fine.
        let style = STYLE.replacen('[', r#"[{"id": "r", "type": "raster", "source": "x", "minzoom": 3},"#, 1);
        let (spec, dir2) = archive("next-level", &style);
        ready(&spec);
        assert!(vector_tiles(&spec, &f, &f.extent(300, 200), None, None).is_some());
        // Caps changing within the level.
        let style = STYLE.replace(r#""line-cap": "round""#, r#""line-cap": ["step", ["zoom"], "butt", 2.5, "round"]"#);
        let (spec, dir4) = archive("caps", &style);
        ready(&spec);
        assert!(vector_tiles(&spec, &f, &f.extent(300, 200), None, None).is_none());
        let _ = std::fs::remove_dir_all(dir4);
        // Caps changing at the next level: not within this one.
        let style = STYLE.replace(r#""line-cap": "round""#, r#""line-cap": ["step", ["zoom"], "butt", 3, "round"]"#);
        let (spec, dir5) = archive("next-caps", &style);
        ready(&spec);
        assert!(vector_tiles(&spec, &f, &f.extent(300, 200), None, None).is_some());
        let _ = std::fs::remove_dir_all(dir5);
        // A long dash pattern from the end of the level.
        let style = STYLE.replace(
            r#""line-dasharray": [3, 2]"#,
            r#""line-dasharray": ["step", ["zoom"], ["literal", [3, 2]], 2.9, ["literal", [3, 2, 1]]]"#,
        );
        let (spec, dir3) = archive("late-dashes", &style);
        ready(&spec);
        assert!(vector_tiles(&spec, &f, &f.extent(300, 200), None, None).is_none());
        for d in [dir, dir2, dir3] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn styles_with_long_dash_patterns_are_declined() {
        let _turn = crate::cache::turn();
        // Three lengths repeat to six: more than the meshes hold.
        let style = STYLE.replace(r#""line-dasharray": [3, 2]"#, r#""line-dasharray": [3, 2, 1]"#);
        let (spec, dir) = archive("dashes", &style);
        ready(&spec);
        let f = frame(2.5);
        assert!(vector_tiles(&spec, &f, &f.extent(300, 200), None, None).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pattern_fills_with_its_average_colour() {
        // Half the pixels opaque red, half clear.
        let mut img = tiny_skia::Pixmap::new(2, 1).unwrap();
        img.pixels_mut()[0] = tiny_skia::PremultipliedColorU8::from_rgba(255, 0, 0, 255).unwrap();
        let avg = average(&img).unwrap();
        assert!((avg[0] - 1.0).abs() < 1e-6 && avg[1] == 0.0 && (avg[3] - 0.5).abs() < 1e-6);
        assert_eq!(average(&tiny_skia::Pixmap::new(1, 1).unwrap()), Some([0.0; 4]));
    }

    #[test]
    fn styles_drawn_otherwise_are_declined() {
        let _turn = crate::cache::turn();
        // A raster layer is a picture of its own.
        let style = STYLE.replacen('[', r#"[{"id": "r", "type": "raster", "source": "x"},"#, 1);
        let (spec, dir) = archive("raster", &style);
        ready(&spec);
        let f = frame(2.5);
        assert!(vector_tiles(&spec, &f, &f.extent(300, 200), None, None).is_none());
        let _ = std::fs::remove_dir_all(dir);
        // Nor a broken scale.
        let (spec, dir) = archive("scale", STYLE);
        ready(&spec);
        let f = Frame { m_per_px: f64::NAN, ..frame(2.5) };
        assert!(vector_tiles(&spec, &f, &frame(2.5).extent(300, 200), None, None).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
