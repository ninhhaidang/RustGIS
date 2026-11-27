//! How much of a map frame its layers cover.

use super::*;

/// The renderer's scale denominator for a frame showing `map` at paper scale 1:`scale` (the one
/// [`legend_sizes_mm`] takes).
pub fn frame_render_scale(map: &Map, center: Coord, scale: f64) -> f64 {
    let cam = frame_camera(map, center, scale, 0.0, 0.0);
    map.ground_res_at(&cam) / rustgis_geom::PIXEL_M
}

/// Whether a layer only outlines its polygons, with no fill and no labels: context around a map's
/// data, such as the neighbouring states' borders, which a surround can sit on. No Fill counts,
/// whatever the renderer; a transparent symbol under stacked fills such as a hatch (the layer's or a
/// class's), a fill by expression, diagrams or extrusion are more than an outline.
pub(super) fn only_outlines(l: &rustgis_map::Layer) -> bool {
    use rustgis_map::NoFill;
    let hollow = l.no_fill == Some(NoFill::Override)
        || (!l.has_fills()
            && (l.no_fill == Some(NoFill::Symbols) || matches!(&l.renderer, rustgis_map::Renderer::Single(s) if s.fill.0[3] == 0)));
    l.geometry_type() == GeometryType::Polygon
        && !l.labels.enabled
        && hollow
        && l.symbol_by.fill.is_none()
        && l.diagram.is_none()
        && l.extrusion.is_none()
}

/// Whether a layer fills its polygons in one colour, with no labels and nothing else: beside a layer
/// drawn by classes or a raster it is context too, such as the neighbouring states in grey.
pub(super) fn plain_fill(l: &rustgis_map::Layer) -> bool {
    l.geometry_type() == GeometryType::Polygon
        && !l.labels.enabled
        && !l.has_fills()
        && matches!(&l.renderer, rustgis_map::Renderer::Single(_))
        && l.symbol_by.fill.is_none()
        && l.diagram.is_none()
        && l.extrusion.is_none()
}

/// Where a frame showing `map` at 1:`scale` draws its layers: a grid of about `cells` cells across
/// the frame's longer side, row by row from the top, each `true` where a layer (or its labels)
/// drew. The basemap is left out, so the frame's background isn't counted, and so are layers that
/// only outline their polygons ([`only_outlines`]) unless nothing else is shown, and those that
/// fill them in one colour ([`plain_fill`]) beside a layer drawn by classes or a raster, when they
/// run on past the frame, as neighbouring states do: one inside it, such as a study area, is what
/// the map shows. For keeping a layout's surrounds off what its map shows. `None` when the frame
/// can't be drawn.
///
/// The map itself is drawn, not a copy of it (every layer's features): its basemap, web tiles
/// (a background, like the basemap) and graticule are set aside for the drawing and put back,
/// however it ends, as is a label lock waiting to be filled, which the coarse drawing would
/// otherwise fill.
pub fn frame_coverage(map: &mut Map, center: Coord, scale: f64, w_mm: f64, h_mm: f64, cells: u32) -> Option<(u32, u32, Vec<bool>)> {
    frame_coverage_judged_on(map, center, scale, (w_mm, h_mm), (w_mm, h_mm), cells)
}

/// The visible layers a frame showing `map` at 1:`scale` around `center` takes as context around
/// its data, by id: those that only outline their polygons ([`only_outlines`]) unless nothing else
/// is shown, and those that fill them in one colour ([`plain_fill`]) beside a layer drawn by classes
/// or a raster, when they run on past a frame `judged` (mm) wide and tall, as neighbouring states
/// do. [`frame_coverage`] leaves them out of what the map draws; a publication's legend leaves them
/// out too (#1379).
pub fn context_layers(map: &Map, center: Coord, scale: f64, judged: (f64, f64)) -> Vec<u32> {
    let at = frame_render_scale(map, center, scale);
    if map.layers.iter().filter(|l| map.draws_at(l, at)).all(only_outlines) {
        return Vec::new();
    }
    let classes =
        map.layers.iter().any(|l| map.draws_at(l, at) && (l.is_raster() || !matches!(&l.renderer, rustgis_map::Renderer::Single(_))));
    let judged_view = View {
        width: judged.0 * PT_PER_MM,
        height: judged.1 * PT_PER_MM,
        pixels_per_point: 1.0,
        camera: frame_camera(map, center, scale, 0.0, 0.0),
        sky: false,
    };
    let frame = crate::view_lonlat(map, &judged_view);
    let past_the_frame = |l: &rustgis_map::Layer| {
        let e = l.shown_extent();
        !e.is_empty() && (e.min_x < frame.min_x || e.max_x > frame.max_x || e.min_y < frame.min_y || e.max_y > frame.max_y)
    };
    map.layers.iter().filter(|l| l.visible && (only_outlines(l) || (classes && plain_fill(l) && past_the_frame(l)))).map(|l| l.id).collect()
}

/// [`frame_coverage`] of a frame `size` (mm), with what runs on past the frame judged on a frame
/// `judged` (mm) with the same centre: a drawing wider than the frame asked for, cut for frames
/// moved inside it, leaves out the same one-colour layers as that frame does.
pub fn frame_coverage_judged_on(
    map: &mut Map,
    center: Coord,
    scale: f64,
    (w_mm, h_mm): (f64, f64),
    judged: (f64, f64),
    cells: u32,
) -> Option<(u32, u32, Vec<bool>)> {
    if !(w_mm > 0.0 && h_mm > 0.0 && judged.0 > 0.0 && judged.1 > 0.0) {
        return None;
    }
    let (w, h) = (w_mm * PT_PER_MM, h_mm * PT_PER_MM);
    let ppp = f64::from(cells.clamp(8, 1024)) / w.max(h);
    let view = View { width: w, height: h, pixels_per_point: ppp, camera: frame_camera(map, center, scale, 0.0, 0.0), sky: false };
    // Put back when this ends, a panic in the drawing included.
    struct Aside<'a> {
        basemap: rustgis_map::Basemap,
        paper: Option<rustgis_map::Rgba>,
        tiles: Vec<rustgis_map::TileLayer>,
        graticule: bool,
        lock: Option<Vec<(rustgis_map::LabelKey, rustgis_map::LockedPlace)>>,
        anchors: std::collections::HashMap<rustgis_map::LabelKey, Coord>,
        outlines: Vec<u32>,
        map: &'a mut Map,
    }
    impl Drop for Aside<'_> {
        fn drop(&mut self) {
            for l in self.map.layers.iter_mut().filter(|l| self.outlines.contains(&l.id)) {
                l.visible = true;
            }
            self.map.basemap = self.basemap;
            self.map.paper = self.paper;
            self.map.tiles = std::mem::take(&mut self.tiles);
            self.map.graticule = self.graticule;
            self.map.label_lock = self.lock.take();
            self.map.label_lock_line_anchors = std::mem::take(&mut self.anchors);
        }
    }
    // The space a globe is drawn on here: the blank basemap's, light.
    let space = map.projection.is_globe().then(|| crate::globe_space(false)).map(|c| (c.r(), c.g(), c.b()));
    let outlines = context_layers(map, center, scale, judged);
    for l in map.layers.iter_mut().filter(|l| outlines.contains(&l.id)) {
        l.visible = false;
    }
    let aside = Aside {
        outlines,
        basemap: std::mem::replace(&mut map.basemap, rustgis_map::Basemap::Blank),
        // Judged on white paper: a blank basemap's paper colour (#2532) would count as drawn.
        paper: map.paper.take(),
        tiles: std::mem::take(&mut map.tiles),
        graticule: std::mem::replace(&mut map.graticule, false),
        lock: map.label_lock.take(),
        anchors: std::mem::take(&mut map.label_lock_line_anchors),
        map,
    };
    let pm = crate::render(&mut *aside.map, &view)?.pixmap;
    drop(aside);
    let (gw, gh) = (pm.width(), pm.height());
    // Anything not the blank basemap's paper, pure opaque white (the graticule set aside too): a
    // pale or translucent fill counts (only a fill of that very white, invisible on it, doesn't).
    // On a globe, the space around it is paper too: counted, it took every cell and the surrounds
    // landed on the globe (#2337). The atmosphere's glow at the rim is not, which keeps them off
    // the globe's edge.
    let paper = |r: u8, g: u8, b: u8| (r, g, b) == (255, 255, 255) || space == Some((r, g, b));
    let drawn = pm.pixels().iter().map(|p| p.alpha() > 0 && !(p.alpha() == 255 && paper(p.red(), p.green(), p.blue()))).collect();
    Some((gw, gh, drawn))
}

/// Whether a frame showing `map` at 1:`scale` around `center`, `size` (mm) wide and tall, shows
/// one light plain colour under `under` (mm from the frame's top left), the basemap's water or
/// land colour, and nothing else: no layer, label or context layer, no outline of the basemap's.
/// Drawn as the map is, on a grid of about `cells` cells across the frame's longer side, its label lock set aside (and put
/// back) as [`frame_coverage`] does. Web tiles aren't fetched for it, so a map with any is not
/// plain anywhere; nor is a frame that can't be drawn. For a publication's legend, which needs no
/// box there (#1379).
pub fn frame_plain_under(
    map: &mut Map,
    center: Coord,
    scale: f64,
    (w_mm, h_mm): (f64, f64),
    under: (f64, f64, f64, f64),
    cells: u32,
) -> bool {
    if !map.tiles.is_empty() || !(w_mm > 0.0 && h_mm > 0.0) {
        return false;
    }
    let (w, h) = (w_mm * PT_PER_MM, h_mm * PT_PER_MM);
    let ppp = f64::from(cells.clamp(8, 1024)) / w.max(h);
    let view = View { width: w, height: h, pixels_per_point: ppp, camera: frame_camera(map, center, scale, 0.0, 0.0), sky: false };
    // The background as the map draws it: on another body, the land's grey (Map::palette).
    let (water, land, ..) = map.palette();
    struct Lock<'a> {
        lock: Option<Vec<(rustgis_map::LabelKey, rustgis_map::LockedPlace)>>,
        anchors: std::collections::HashMap<rustgis_map::LabelKey, Coord>,
        map: &'a mut Map,
    }
    impl Drop for Lock<'_> {
        fn drop(&mut self) {
            self.map.label_lock = self.lock.take();
            self.map.label_lock_line_anchors = std::mem::take(&mut self.anchors);
        }
    }
    let lock = Lock { lock: map.label_lock.take(), anchors: std::mem::take(&mut map.label_lock_line_anchors), map };
    let Some(img) = crate::render(&mut *lock.map, &view) else { return false };
    drop(lock);
    // A streamed layer still reading, or a raster read in place whose tiles are on their way or
    // were drawn from less than the view needs: what lies under the rectangle isn't known yet.
    if img.data_pending || !img.pending_in_place.is_empty() || !img.partial.is_empty() {
        return false;
    }
    // The labels are drawn apart from the features, as the frame draws them over its map.
    let crate::Rendered { pixmap, labels, unplaced, label_layers, .. } = img;
    let mut canvas = Canvas::from_pixmap(pixmap);
    crate::draw_labels(&mut canvas, (&unplaced, &labels, &label_layers), ppp as f32);
    let pm = canvas.into_pixmap();
    let (gw, gh) = (pm.width() as usize, pm.height() as usize);
    // Every cell the rectangle touches, partly too.
    let cell = |v: f64, size: f64, n: usize, round: fn(f64) -> f64| (round(v / size * n as f64).max(0.0) as usize).min(n);
    let (c0, c1) = (cell(under.0, w_mm, gw, f64::floor), cell(under.0 + under.2, w_mm, gw, f64::ceil));
    let (r0, r1) = (cell(under.1, h_mm, gh, f64::floor), cell(under.1 + under.3, h_mm, gh, f64::ceil));
    let px = pm.pixels();
    let mut seen = (r0..r1).flat_map(|r| (c0..c1).map(move |c| r * gw + c)).map(|i| px[i]);
    // One colour, and that the basemap's water or land: a polygon filling the whole rectangle in
    // one colour, data or context, isn't the background.
    let rgba = |p: tiny_skia::PremultipliedColorU8| (p.red(), p.green(), p.blue(), p.alpha());
    let Some(first) = seen.next().map(rgba) else { return false };
    // A publication's legend has no ink (#2257): its text stays dark grey (INK), so dark water
    // or land must keep its paper backing.
    let background = [water, land]
        .iter()
        .any(|c| (c.0[0], c.0[1], c.0[2], 255) == first && u16::from(c.r()) + u16::from(c.g()) + u16::from(c.b()) > 3 * 160);
    background && seen.all(|p| rgba(p) == first)
}
