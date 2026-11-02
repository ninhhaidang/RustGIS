//! Layer effects drawn in place of plain features: point aggregation into counted bins, and
//! polygons extruded with lit walls and cast shadows: in an oblique 2.5D view on a flat map, in
//! perspective on a tilted one.

use std::collections::BTreeMap;

use crate::Canvas;
use rustgis_data::{Dataset, Geometry};
use rustgis_map::effects::{Aggregation, Extrusion};
use rustgis_map::{Ramp, Rgba, Symbol};
use tiny_skia::{FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, PixmapPaint, Point, SpreadMode, Stroke, Transform};

use crate::{Ctx, View, paint};

/// Points binned into hexagons or squares, each bin coloured by its count.
#[derive(Default)]
pub(crate) struct AggregationBins {
    bins: BTreeMap<(i64, i64), u32>,
}

impl AggregationBins {
    pub fn add(&mut self, size: (u32, u32), ctx: &Ctx, geoms: &[Option<&Geometry>], rows: &[bool], a: Aggregation) {
        let s = (a.size.clamp(4.0, 400.0) * ctx.ppp as f64) as f32;
        let (w, h) = (size.0 as f32, size.1 as f32);
        let bins = &mut self.bins;
        let sqrt3 = 3f32.sqrt();
        for (row, g) in geoms.iter().enumerate() {
            let Some(g) = g else { continue };
            if rows.get(row) != Some(&true) {
                continue;
            }
            let pts: Vec<rustgis_geom::Coord> = match g {
                Geometry::Point(c) => vec![*c],
                Geometry::MultiPoint(v) => v.clone(),
                _ => continue,
            };
            for c in pts {
                let (x, y) = ctx.px(c);
                if !(x.is_finite() && y.is_finite()) || x < -s || y < -s || x > w + s || y > h + s {
                    continue;
                }
                let key = if a.hexagons {
                    // Pointy-top hexagons in axial coordinates, rounded through cube coordinates.
                    let q = (sqrt3 / 3.0 * x - y / 3.0) / s;
                    let r = (2.0 / 3.0 * y) / s;
                    let (cx, cz) = (q, r);
                    let cy = -cx - cz;
                    let (mut rx, ry, mut rz) = (cx.round(), cy.round(), cz.round());
                    let (dx, dy, dz) = ((rx - cx).abs(), (ry - cy).abs(), (rz - cz).abs());
                    if dx > dy && dx > dz {
                        rx = -ry - rz;
                    } else if dy <= dz {
                        rz = -rx - ry;
                    }
                    (rx as i64, rz as i64)
                } else {
                    ((x / s).floor() as i64, (y / s).floor() as i64)
                };
                let count = bins.entry(key).or_insert(0);
                *count = count.saturating_add(1);
            }
        }
    }
    /// Draws the bins as hexagons or squares `a.size` points across, shaded by their counts on
    /// the ember ramp, each outlined.
    pub fn draw(&self, pm: &mut Canvas, ctx: &Ctx, a: Aggregation, base: &Symbol) {
        let bins = &self.bins;
        let s = (a.size.clamp(4.0, 400.0) * ctx.ppp as f64) as f32;
        let sqrt3 = 3f32.sqrt();
        let max = bins.values().copied().max().unwrap_or(1).max(1) as f64;
        let ramp = Ramp::Ember;
        let stroke = Stroke { width: 0.8 * ctx.ppp, ..Stroke::default() };
        for (&(q, r), &n) in bins {
            let t = if max > 1.0 { (n as f64).ln() / max.ln() } else { 1.0 };
            let fill = ramp.at(0.15 + 0.85 * t).with_alpha(210);
            let mut pb = PathBuilder::new();
            if a.hexagons {
                let (cx, cy) = (s * sqrt3 * (q as f32 + r as f32 / 2.0), s * 1.5 * r as f32);
                for k in 0..6 {
                    let ang = (60.0 * k as f32 - 30.0).to_radians();
                    let (x, y) = (cx + s * ang.cos(), cy + s * ang.sin());
                    if k == 0 { pb.move_to(x, y) } else { pb.line_to(x, y) }
                }
                pb.close();
            } else if let Some(rect) = tiny_skia::Rect::from_xywh(q as f32 * s, r as f32 * s, s, s) {
                pb.push_rect(rect);
            }
            if let Some(p) = pb.finish() {
                pm.fill_path(&p, &paint(fill, 1.0), FillRule::Winding, Transform::identity(), None);
                pm.stroke_path(&p, &paint(base.stroke.with_alpha(160), 1.0), &stroke, Transform::identity(), None);
            }
        }
    }
}

/// Polygons extruded upward on screen by their height (`px_per_m` pixels per metre), drawn far
/// to near, with shadows first, then walls (lit by their facing), then roofs.
pub(crate) fn extrude(
    pm: &mut Canvas,
    ctx: &Ctx,
    ds: &Dataset,
    geoms: &[Option<&Geometry>],
    rows: &[bool],
    x: &Extrusion,
    px_per_m: f64,
    symbol: impl Fn(usize) -> Symbol,
) {
    let shapes = shapes(ctx, ds, geoms, rows, x, px_per_m, symbol);
    paint_shapes(pm, ctx, x, &shapes, true, true);
}

type Shape = (f32, Vec<Vec<(f32, f32)>>, f32, Symbol);

fn shapes(
    ctx: &Ctx,
    ds: &Dataset,
    geoms: &[Option<&Geometry>],
    rows: &[bool],
    x: &Extrusion,
    px_per_m: f64,
    symbol: impl Fn(usize) -> Symbol,
) -> Vec<Shape> {
    let fi = x.field.as_deref().and_then(|f| ds.field_index(f));
    // (base centre y, outer rings in pixels, height in pixels, row)
    let mut shapes: Vec<Shape> = Vec::new();
    for (row, g) in geoms.iter().enumerate() {
        let Some(Geometry::Polygon(polys)) = g else { continue };
        if rows.get(row) != Some(&true) {
            continue;
        }
        let metres = fi.and_then(|i| ds.value(row, i).as_f64()).filter(|v| v.is_finite()).unwrap_or(x.height) * x.factor;
        let hp = (metres * px_per_m).clamp(0.0, 4000.0) as f32;
        let rings: Vec<Vec<(f32, f32)>> = polys
            .iter()
            .filter_map(|p| p.first())
            .map(|r| r.iter().map(|c| ctx.px(*c)).filter(|(a, b)| a.is_finite() && b.is_finite()).collect::<Vec<_>>())
            .filter(|r| r.len() >= 3)
            .collect();
        if rings.is_empty() {
            continue;
        }
        let count = rings.iter().map(Vec::len).sum::<usize>().max(1);
        let sum = rings.iter().flatten().map(|p| p.1).sum::<f32>();
        // Preserve ordinary painter order exactly; only widen an overflowing sum.
        let cy = if sum.is_finite() {
            sum / count as f32
        } else {
            (rings.iter().flatten().map(|p| f64::from(p.1)).sum::<f64>() / count as f64) as f32
        };
        shapes.push((cy, rings, hp, symbol(row)));
    }
    shapes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    shapes
}

/// Draws extruded `shapes`: their shadows (`shadows`), then their walls and roofs (`solids`).
fn paint_shapes(pm: &mut Canvas, ctx: &Ctx, x: &Extrusion, shapes: &[Shape], shadows: bool, solids: bool) {
    let poly_path = |rings: &[Vec<(f32, f32)>], dx: f32, dy: f32| {
        let mut pb = PathBuilder::new();
        for r in rings {
            for (i, (px, py)) in r.iter().enumerate() {
                if i == 0 { pb.move_to(px + dx, py + dy) } else { pb.line_to(px + dx, py + dy) }
            }
            pb.close();
        }
        pb.finish()
    };
    let quad = |a: (f32, f32), b: (f32, f32), dx: f32, dy: f32| {
        let mut pb = PathBuilder::new();
        pb.move_to(a.0, a.1);
        pb.line_to(b.0, b.1);
        pb.line_to(b.0 + dx, b.1 + dy);
        pb.line_to(a.0 + dx, a.1 + dy);
        pb.close();
        pb.finish()
    };
    if x.shadows && shadows {
        // Light from the north-west casts shadows to the south-east.
        let shade = paint(Rgba::new(0, 0, 0, 70), 1.0);
        for (_, rings, hp, _) in shapes {
            let (sx, sy) = (hp * 0.7, hp * 0.35);
            for r in rings {
                for i in 0..r.len() {
                    if let (Some(a), Some(b)) = (r.get(i), r.get((i + 1) % r.len()))
                        && let Some(p) = quad(*a, *b, sx, sy)
                    {
                        pm.fill_path(&p, &shade, FillRule::Winding, Transform::identity(), None);
                    }
                }
            }
            if let Some(p) = poly_path(rings, sx, sy) {
                pm.fill_path(&p, &shade, FillRule::EvenOdd, Transform::identity(), None);
            }
        }
    }
    let light = (-std::f32::consts::FRAC_1_SQRT_2, -std::f32::consts::FRAC_1_SQRT_2);
    let edge_stroke = Stroke { width: 0.35 * ctx.ppp, ..Stroke::default() };
    if !solids {
        return;
    }
    for (_, rings, hp, sym) in shapes {
        let base = sym.fill.with_alpha(255);
        let up = -hp;
        // Walls: the back ones first (when drawn), then the ones facing the viewer.
        let mut walls: Vec<(bool, (f32, f32), (f32, f32), f32)> = Vec::new();
        for r in rings {
            let area2: f32 =
                (0..r.len()).filter_map(|i| Some((r.get(i)?, r.get((i + 1) % r.len())?))).map(|(a, b)| a.0 * b.1 - b.0 * a.1).sum();
            for i in 0..r.len() {
                let (Some(a), Some(b)) = (r.get(i), r.get((i + 1) % r.len())) else { continue };
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let len = (dx * dx + dy * dy).sqrt();
                // Sub-pixel edges (detailed coastlines) add nothing but streaks.
                if len < 0.75 {
                    continue;
                }
                let n = if area2 > 0.0 { (dy / len, -dx / len) } else { (-dy / len, dx / len) };
                let facing = n.1 > 0.0;
                let lit = n.0 * light.0 + n.1 * light.1;
                walls.push((facing, *a, *b, lit));
            }
        }
        walls.sort_by_key(|w| w.0);
        for (facing, a, b, lit) in walls {
            if x.face_culling && !facing {
                continue;
            }
            let f = if x.lighting { 0.55 + 0.4 * lit.max(0.0) as f64 } else { 0.75 };
            if let Some(p) = quad(a, b, 0.0, up) {
                pm.fill_path(&p, &paint(base.shade(f), 1.0), FillRule::Winding, Transform::identity(), None);
                pm.stroke_path(&p, &paint(sym.stroke.with_alpha(45), 1.0), &edge_stroke, Transform::identity(), None);
            }
        }
        if let Some(p) = poly_path(rings, 0.0, up) {
            let roof = if x.lighting { base.shade(1.08) } else { base };
            pm.fill_path(&p, &paint(roof, 1.0), FillRule::EvenOdd, Transform::identity(), None);
            pm.stroke_path(
                &p,
                &paint(sym.stroke, 1.0),
                &Stroke { width: sym.stroke_width.max(0.5) * ctx.ppp, ..Stroke::default() },
                Transform::identity(),
                None,
            );
        }
    }
}

// Store float bits as integers so JSON cannot turn a non-finite height or symbol into null.
// This is a disposable internal payload, not a persisted file format.
#[cfg(not(target_arch = "wasm32"))]
type StoredShape = (u32, Vec<Vec<(u32, u32)>>, u32, Rgba, Rgba, u32, u32, rustgis_map::Marker);

#[cfg(not(target_arch = "wasm32"))]
fn encode_shape(shape: &Shape) -> rustgis_data::Result<Vec<u8>> {
    let (cy, rings, hp, sym) = shape;
    let stored: StoredShape = (
        cy.to_bits(),
        rings.iter().map(|r| r.iter().map(|(x, y)| (x.to_bits(), y.to_bits())).collect()).collect(),
        hp.to_bits(),
        sym.fill,
        sym.stroke,
        sym.stroke_width.to_bits(),
        sym.size.to_bits(),
        sym.marker,
    );
    serde_json::to_vec(&stored).map_err(|e| rustgis_data::DataError::Format(e.to_string()))
}

#[cfg(not(target_arch = "wasm32"))]
fn decode_shape(bytes: &[u8]) -> rustgis_data::Result<Shape> {
    let (cy, rings, hp, fill, stroke, width, size, marker): StoredShape =
        serde_json::from_slice(bytes).map_err(|e| rustgis_data::DataError::Format(e.to_string()))?;
    Ok((
        f32::from_bits(cy),
        rings.into_iter().map(|r| r.into_iter().map(|(x, y)| (f32::from_bits(x), f32::from_bits(y))).collect()).collect(),
        f32::from_bits(hp),
        Symbol { fill, stroke, stroke_width: f32::from_bits(width), size: f32::from_bits(size), marker },
    ))
}

/// Global painter ordering and a separate shadow pass, without retaining every feature.
pub(crate) struct ExtrusionSpool {
    #[cfg(not(target_arch = "wasm32"))]
    disk: rustgis_data::source::spill::Ordered,
    #[cfg(target_arch = "wasm32")]
    shapes: Vec<Shape>,
}
#[cfg(test)]
thread_local! {
    pub(crate) static SPOOL_FAILURE: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn check_spool_failure(stage: &str) -> rustgis_data::Result<()> {
    if SPOOL_FAILURE.with(|s| s.get() == Some(stage)) {
        return Err(rustgis_data::DataError::Format(format!("injected spool {stage} failure")));
    }
    Ok(())
}

impl ExtrusionSpool {
    pub fn new() -> rustgis_data::Result<Self> {
        #[cfg(test)]
        check_spool_failure("create")?;
        Ok(Self {
            #[cfg(not(target_arch = "wasm32"))]
            disk: rustgis_data::source::spill::Ordered::new()?,
            #[cfg(target_arch = "wasm32")]
            shapes: Vec::new(),
        })
    }
    pub fn add(
        &mut self,
        ctx: &Ctx,
        ds: &Dataset,
        geoms: &[Option<&Geometry>],
        rows: &[bool],
        x: &Extrusion,
        px_per_m: f64,
        symbol: impl Fn(usize) -> Symbol,
    ) -> rustgis_data::Result<()> {
        #[cfg(test)]
        check_spool_failure("insert")?;
        for shape in shapes(ctx, ds, geoms, rows, x, px_per_m, symbol) {
            #[cfg(not(target_arch = "wasm32"))]
            self.disk.insert(f64::from(shape.0), &encode_shape(&shape)?)?;
            #[cfg(target_arch = "wasm32")]
            self.shapes.push(shape);
        }
        Ok(())
    }
    /// Draws the spooled shapes as [`paint_shapes`] does, every shadow before any wall; an error
    /// if the spool can't be read back.
    pub fn draw(&mut self, pm: &mut Canvas, ctx: &Ctx, x: &Extrusion) -> rustgis_data::Result<()> {
        #[cfg(test)]
        check_spool_failure("draw")?;
        #[cfg(not(target_arch = "wasm32"))]
        for shadows in [true, false].into_iter().filter(|shadow| !*shadow || x.shadows) {
            self.disk.visit(|bytes| {
                let shape = decode_shape(bytes)?;
                paint_shapes(pm, ctx, x, std::slice::from_ref(&shape), shadows, !shadows);
                Ok(())
            })?;
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.shapes.sort_by(|a, b| a.0.total_cmp(&b.0));
            paint_shapes(pm, ctx, x, &self.shapes, true, true);
        }
        Ok(())
    }
}

/// How high (metres) feature `row` of `ds` is extruded.
pub(crate) fn extrusion_metres(ds: &Dataset, x: &Extrusion, row: usize) -> f64 {
    let fi = x.field.as_deref().and_then(|f| ds.field_index(f));
    fi.and_then(|i| ds.value(row, i).as_f64()).filter(|v| v.is_finite()).unwrap_or(x.height) * x.factor
}

/// The light of a tilted view's extrusions, as MapLibre's default: from the view's upper left,
/// mostly overhead (toward it: right, down the screen and up, in ground px).
pub const LIGHT: (f64, f64, f64) = (-0.25, -0.43, 0.87);

/// The part of the polygon `pts` (ground px) down the view no farther than `v_max`: what stands
/// in front of the eye ([`rustgis_map::Camera::near_v`]). Sutherland–Hodgman against one edge.
/// A polygon wholly behind the eye (or with no number in it) comes back empty: nothing of it is
/// drawn. Where an edge crosses the limit its ends differ in v, so the division is safe.
pub fn clip_near(pts: &[(f64, f64)], v_max: f64) -> Vec<(f64, f64)> {
    let mut out = Vec::with_capacity(pts.len() + 2);
    for i in 0..pts.len() {
        let (Some(&a), Some(&b)) = (pts.get(i), pts.get((i + 1) % pts.len())) else { continue };
        let (ina, inb) = (a.1 <= v_max, b.1 <= v_max);
        if ina {
            out.push(a);
        }
        if ina != inb {
            let t = (v_max - a.1) / (b.1 - a.1);
            out.push((a.0 + (b.0 - a.0) * t, v_max));
        }
    }
    out
}

/// The part of the segment a–b (ground px) no farther down the view than `v_max`.
fn clip_segment(a: (f64, f64), b: (f64, f64), v_max: f64) -> Option<((f64, f64), (f64, f64))> {
    let at = |p: (f64, f64), q: (f64, f64)| {
        let t = (v_max - p.1) / (q.1 - p.1);
        (p.0 + (q.0 - p.0) * t, v_max)
    };
    match (a.1 <= v_max, b.1 <= v_max) {
        (true, true) => Some((a, b)),
        (true, false) => Some((a, at(a, b))),
        (false, true) => Some((at(b, a), b)),
        (false, false) => None,
    }
}

/// A wall of an extruded feature in a tilted view ([`tilted_walls`]).
pub struct TiltedWall {
    /// How far up the view it stands (ground px; smaller is farther).
    pub depth: f64,
    /// Its corners on screen (logical px): foot a, foot b, top b, top a.
    pub quad: [(f64, f64); 4],
    /// The shades (multipliers of the fill colour) of its top and its foot.
    pub top: f64,
    pub foot: f64,
}

/// The walls of a feature whose outer `rings` (ground px, [`rustgis_map::Camera::ground_px`])
/// are raised `up` px, in a tilted `w`×`h` view of `cam`, far first: with `culling`, only those
/// facing the eye; with `lighting`, lit by the way they face MapLibre's default light and
/// darker toward their foot. Both the settled frame and the moving view's mesh draw these.
pub fn tilted_walls(
    cam: &rustgis_map::Camera,
    w: f64,
    h: f64,
    rings: &[Vec<(f64, f64)>],
    holes: &[Vec<(f64, f64)>],
    up: f64,
    culling: bool,
    lighting: bool,
) -> Vec<TiltedWall> {
    let (eu, ev, _) = cam.eye(h);
    let light_len = LIGHT.0.hypot(LIGHT.1).max(1e-9);
    let at = |u: f64, v: f64, up: f64| cam.project_raised(u, v, up, w, h).filter(|p| p.0.is_finite() && p.1.is_finite());
    // Walls reaching behind the eye are cut where they still stand in front of it.
    let v_max = cam.near_v(up, h);
    let mut walls = Vec::new();
    // A hole's walls face into it (a courtyard).
    for (r, hole) in rings.iter().map(|r| (r, false)).chain(holes.iter().map(|r| (r, true))) {
        let area2: f64 =
            (0..r.len()).filter_map(|i| Some((r.get(i)?, r.get((i + 1) % r.len())?))).map(|(a, b)| a.0 * b.1 - b.0 * a.1).sum();
        for i in 0..r.len() {
            let (Some(&a), Some(&b)) = (r.get(i), r.get((i + 1) % r.len())) else { continue };
            let (du, dv) = (b.0 - a.0, b.1 - a.1);
            let len = du.hypot(dv);
            if len <= 0.0 {
                continue;
            }
            // The outward normal (the rings wind either way).
            let nrm = if (area2 > 0.0) != hole { (dv / len, -du / len) } else { (-dv / len, du / len) };
            let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
            if culling && nrm.0 * (eu - mid.0) + nrm.1 * (ev - mid.1) <= 0.0 {
                continue;
            }
            let Some((a, b)) = clip_segment(a, b, v_max) else { continue };
            let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
            let (Some(fa), Some(fb), Some(tb), Some(ta)) = (at(a.0, a.1, 0.0), at(b.0, b.1, 0.0), at(b.0, b.1, up), at(a.0, a.1, up))
            else {
                continue;
            };
            let lit = ((nrm.0 * LIGHT.0 + nrm.1 * LIGHT.1) / light_len).max(0.0);
            // Darker toward the foot, as MapLibre's vertical gradient.
            let (top, foot) = if lighting { (0.6 + 0.4 * lit, (0.6 + 0.4 * lit) * 0.72) } else { (0.8, 0.8) };
            walls.push(TiltedWall { depth: mid.1, quad: [fa, fb, tb, ta], top, foot });
        }
    }
    walls.sort_by(|a, b| a.depth.partial_cmp(&b.depth).unwrap_or(std::cmp::Ordering::Equal));
    walls
}

/// How far (ground px) the shadow of a feature raised `up` px falls from it, away from the light.
pub fn shadow_shift(up: f64) -> (f64, f64) {
    (-LIGHT.0 / LIGHT.2 * up, -LIGHT.1 / LIGHT.2 * up)
}

/// The sides of the shadow a feature raised `up` px casts on the ground in a tilted view: a
/// convex polygon (screen, logical px) between each edge of its outer `rings` (ground px) and the
/// edge moved by [`shadow_shift`], cut where it reaches behind the eye. Its footprint moved by
/// the same shift completes it.
pub fn tilted_shadow_quads(cam: &rustgis_map::Camera, w: f64, h: f64, rings: &[Vec<(f64, f64)>], up: f64) -> Vec<Vec<(f64, f64)>> {
    let (du, dv) = shadow_shift(up);
    let at = |p: (f64, f64)| cam.project_raised(p.0, p.1, 0.0, w, h).filter(|p| p.0.is_finite() && p.1.is_finite());
    let v_max = cam.near_v(0.0, h);
    let mut out = Vec::new();
    for r in rings {
        for i in 0..r.len() {
            let (Some(&a), Some(&b)) = (r.get(i), r.get((i + 1) % r.len())) else { continue };
            // On the ground, cut where it reaches behind the eye.
            let side = clip_near(&[a, b, (b.0 + du, b.1 + dv), (a.0 + du, a.1 + dv)], v_max);
            let pts: Option<Vec<_>> = side.into_iter().map(at).collect();
            if let Some(pts) = pts.filter(|p| p.len() >= 3) {
                out.push(pts);
            }
        }
    }
    out
}

/// Polygons extruded in perspective in a tilted view, as MapLibre's fill extrusions: each
/// feature's walls stand on its outline and its roof is raised by its height (converted at
/// the feature's place by `m_per_unit`, the ground metres per map unit there). Features and
/// walls are drawn far to near; walls facing away from the eye are culled, walls are lit by the way they face the light and
/// shade darker toward their base, and features fade into `fog` with distance as the ground
/// does.
#[allow(clippy::too_many_arguments)]
pub(crate) fn extrude_tilted(
    pm: &mut Pixmap,
    view: &View,
    ds: &Dataset,
    geoms: &[Option<&Geometry>],
    rows: &[bool],
    x: &Extrusion,
    m_per_unit: impl Fn(rustgis_geom::Coord) -> f64,
    fog: Rgba,
    symbol: impl Fn(usize) -> Symbol,
) {
    let cam = view.camera;
    let (w, h) = (view.width, view.height);
    let ppp = view.pixels_per_point as f32;
    let far = rustgis_map::camera::FAR;
    type Rings = Vec<Vec<(f64, f64)>>;
    // (centroid depth along the view, outer rings and holes in ground px, height in ground px,
    // fog, row)
    let mut shapes: Vec<(f64, Rings, Rings, f64, f64, usize)> = Vec::new();
    for (row, g) in geoms.iter().enumerate() {
        let Some(Geometry::Polygon(polys)) = g else { continue };
        if rows.get(row) != Some(&true) {
            continue;
        }
        // Thinned on screen: a point closer than about a pixel to the last kept adds a sliver.
        let mut rings = Vec::new();
        let (mut sum, mut n) = (rustgis_geom::Coord::new(0.0, 0.0), 0.0);
        // Drawn while any of its outline is in front of the eye and within the view's reach.
        let mut in_reach = false;
        let mut holes = Vec::new();
        for (k, r) in polys.iter().flat_map(|p| p.iter().enumerate()) {
            let mut kept: Vec<(f64, f64)> = Vec::new();
            let mut last: Option<(f64, f64)> = None;
            for c in r {
                let g = cam.ground_px(*c);
                let s = cam.project_raised(g.0, g.1, 0.0, w, h);
                let near = match (s, last) {
                    (Some(s), Some(l)) => (s.0 - l.0).hypot(s.1 - l.1) < 0.75,
                    _ => false,
                };
                if !near {
                    kept.push(g);
                    last = s;
                }
                if k == 0 {
                    sum = rustgis_geom::Coord::new(sum.x + c.x, sum.y + c.y);
                    n += 1.0;
                }
                in_reach |= cam.depth_scale(*c, h).is_some_and(|k| k * far >= 1.0);
            }
            if kept.len() >= 3 {
                if k == 0 { rings.push(kept) } else { holes.push(kept) }
            }
        }
        if rings.is_empty() || n == 0.0 || !in_reach {
            continue;
        }
        let centroid = rustgis_geom::Coord::new(sum.x / n, sum.y / n);
        // At the feature's own scale (Web Mercator stretches the ground toward the poles).
        let up = (extrusion_metres(ds, x, row) / m_per_unit(centroid).max(1e-9) / cam.res.max(1e-9)).clamp(0.0, 1e7);
        // Fog on the way to the view's reach, by the middle of the feature.
        let depth = cam.depth_scale(centroid, h).map_or(1.0, |k| 1.0 / k.max(1e-9));
        let t = ((depth - far * 0.5) / (far * 0.5)).clamp(0.0, 1.0);
        shapes.push((cam.ground_px(centroid).1, rings, holes, up, t * t * (3.0 - 2.0 * t), row));
    }
    // Far (up the screen) first.
    shapes.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let at = |u: f64, v: f64, up: f64| cam.project_raised(u, v, up, w, h).map(|(x, y)| (x as f32 * ppp, y as f32 * ppp));
    let path = |pts: &[(f32, f32)]| {
        let mut pb = PathBuilder::new();
        for (i, p) in pts.iter().enumerate() {
            if i == 0 { pb.move_to(p.0, p.1) } else { pb.line_to(p.0, p.1) }
        }
        pb.close();
        pb.finish()
    };
    let px = |p: (f64, f64)| (p.0 as f32 * ppp, p.1 as f32 * ppp);
    if x.shadows
        && let Some(mut shade_layer) = Pixmap::new(pm.width(), pm.height())
    {
        // Cast on the ground away from the light: every piece opaque on a layer of its own, laid
        // over the map once, so where pieces (or neighbours' shadows) overlap it is no darker.
        let shade = paint(Rgba::new(0, 0, 0, 255), 1.0);
        for (_, rings, _, up, _, _) in &shapes {
            for q in tilted_shadow_quads(&cam, w, h, rings, *up) {
                let q: Vec<_> = q.into_iter().map(px).collect();
                if let Some(p) = path(&q) {
                    shade_layer.fill_path(&p, &shade, FillRule::Winding, Transform::identity(), None);
                }
            }
            let (du, dv) = shadow_shift(*up);
            for r in rings {
                let moved: Vec<_> = r.iter().map(|&(u, v)| (u + du, v + dv)).collect();
                let shifted: Option<Vec<_>> = clip_near(&moved, cam.near_v(0.0, h)).into_iter().map(|(u, v)| at(u, v, 0.0)).collect();
                if let Some(p) = shifted.as_deref().and_then(path) {
                    shade_layer.fill_path(&p, &shade, FillRule::Winding, Transform::identity(), None);
                }
            }
        }
        let over = PixmapPaint { opacity: 60.0 / 255.0, ..PixmapPaint::default() };
        pm.draw_pixmap(0, 0, shade_layer.as_ref(), &over, Transform::identity(), None);
    }
    for (_, rings, holes, up, fogged, row) in &shapes {
        let sym = symbol(*row);
        let base = sym.fill.with_alpha(255).lerp(fog, *fogged);
        for wall in tilted_walls(&cam, w, h, rings, holes, *up, x.face_culling, x.lighting) {
            let [fa, fb, tb, ta] = wall.quad.map(px);
            let (top, foot) = (base.shade(wall.top), base.shade(wall.foot));
            // A hair longer each way, so neighbouring walls leave no seam between them.
            let grow = |p: (f32, f32), q: (f32, f32)| {
                let (dx, dy) = (p.0 - q.0, p.1 - q.1);
                let l = dx.hypot(dy).max(1e-6);
                (p.0 + dx / l * 0.4 * ppp, p.1 + dy / l * 0.4 * ppp)
            };
            let quad = [grow(fa, fb), grow(fb, fa), grow(tb, ta), grow(ta, tb)];
            let Some(p) = path(&quad) else { continue };
            let mid = |p: (f32, f32), q: (f32, f32)| Point::from_xy((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0);
            let color = |c: Rgba| tiny_skia::Color::from_rgba8(c.r(), c.g(), c.b(), 255);
            let shader = LinearGradient::new(
                mid(fa, fb),
                mid(ta, tb),
                vec![GradientStop::new(0.0, color(foot)), GradientStop::new(1.0, color(top))],
                SpreadMode::Pad,
                Transform::identity(),
            );
            let paint_wall = match shader {
                Some(shader) => Paint { shader, anti_alias: true, ..Paint::default() },
                None => paint(top, 1.0),
            };
            pm.fill_path(&p, &paint_wall, FillRule::Winding, Transform::identity(), None);
        }
        // The roof, over its walls, open over its holes.
        let mut pb = PathBuilder::new();
        for r in rings.iter().chain(holes) {
            // Cut where it reaches behind the eye.
            let pts: Option<Vec<_>> = clip_near(r, cam.near_v(*up, h)).into_iter().map(|(u, v)| at(u, v, *up)).collect();
            let pts = pts.filter(|p| p.len() >= 3);
            let Some(pts) = pts else { continue };
            for (i, p) in pts.iter().enumerate() {
                if i == 0 { pb.move_to(p.0, p.1) } else { pb.line_to(p.0, p.1) }
            }
            pb.close();
        }
        if let Some(p) = pb.finish() {
            pm.fill_path(&p, &paint(base, 1.0), FillRule::EvenOdd, Transform::identity(), None);
            let edge = sym.stroke.lerp(fog, *fogged).with_alpha(110);
            pm.stroke_path(&p, &paint(edge, 1.0), &Stroke { width: 0.6 * ppp, ..Stroke::default() }, Transform::identity(), None);
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod spool_tests {
    use super::*;
    #[test]
    fn spool_payload_preserves_all_float_bits_and_large_finite_centres() {
        let symbol = Symbol { stroke_width: f32::NAN, size: f32::INFINITY, ..Symbol::default_for(rustgis_data::GeometryType::Polygon, 1) };
        let shape = (1.0, vec![vec![(f32::MAX, -0.0)]], f32::NAN, symbol);
        let decoded = decode_shape(&encode_shape(&shape).unwrap()).unwrap();
        assert_eq!(decoded.0.to_bits(), shape.0.to_bits());
        assert_eq!(decoded.1[0][0].0.to_bits(), f32::MAX.to_bits());
        assert_eq!(decoded.1[0][0].1.to_bits(), (-0.0f32).to_bits());
        assert_eq!(decoded.2.to_bits(), shape.2.to_bits());
        assert_eq!(decoded.3.stroke_width.to_bits(), symbol.stroke_width.to_bits());
        assert_eq!(decoded.3.size.to_bits(), symbol.size.to_bits());
        let view = View {
            width: 100.0,
            height: 100.0,
            pixels_per_point: 1.0,
            camera: rustgis_map::Camera::new(rustgis_geom::Coord::new(0.0, 0.0), 1.0),
            sky: false,
        };
        let ctx = Ctx {
            view: &view,
            ppp: 1.0,
            wireframe: false,
            cull: rustgis_geom::Extent::EMPTY,
            globe: None,
            sprites: Default::default(),
            bursts: Default::default(),
        };
        let geom = Geometry::Polygon(vec![vec![vec![
            rustgis_geom::Coord::new(0.0, -2e38),
            rustgis_geom::Coord::new(1.0, -2e38),
            rustgis_geom::Coord::new(1.0, -2e38),
            rustgis_geom::Coord::new(0.0, -2e38),
        ]]]);
        let ds = Dataset::new("large", rustgis_data::GeometryType::Polygon, vec![]);
        let x = Extrusion { height: 1.0, field: None, factor: 1.0, face_culling: true, lighting: true, shadows: true };
        let got = shapes(&ctx, &ds, &[Some(&geom)], &[true], &x, 1.0, |_| symbol);
        assert_eq!(got.len(), 1);
        assert!(got[0].0.is_finite());
        let ordered = rustgis_data::source::spill::Ordered::new().unwrap();
        ordered.insert(f64::from(got[0].0), &encode_shape(&got[0]).unwrap()).unwrap();
        ordered
            .visit(|bytes| {
                decode_shape(bytes)?;
                Ok(())
            })
            .unwrap();
    }
}
