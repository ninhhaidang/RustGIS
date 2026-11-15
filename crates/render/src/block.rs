//! Block diagrams (#1914): a tilted view's ground over terrain cut to a box, and the box's sides
//! facing the eye drawn as walls from the ground's profile down to a flat base.

use std::sync::{Arc, Mutex, Weak};

use rustgis_data::{Feature, Geometry};
use rustgis_geom::{Coord, Extent, lonlat_to_merc, merc_to_lonlat};
use rustgis_map::fills::{FillLayer, FillPattern};
use rustgis_map::{Map, Rgba};
use tiny_skia::{FillRule, GradientStop, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Point, SpreadMode, Stroke, Transform};

use crate::effects::LIGHT;
use crate::terrain::{Block, Ground};
use crate::{View, paint};

/// The box's sides as bits: west (x0), east (x1), south (y0), north (y1). A point outside the box
/// has the bits of the sides it lies beyond ([`outcode`]); a corner of a clipped triangle, those
/// of the sides it lies on ([`clip`]).
pub(crate) const WEST: u8 = 1;
pub(crate) const EAST: u8 = 2;
pub(crate) const SOUTH: u8 = 4;
pub(crate) const NORTH: u8 = 8;

/// The sides of `b` the point `m` lies beyond (Web Mercator): 0 inside.
pub(crate) fn outcode(b: &Block, m: Coord) -> u8 {
    let mut c = 0;
    if m.x < b.x0 {
        c |= WEST;
    }
    if m.x > b.x1 {
        c |= EAST;
    }
    if m.y < b.y0 {
        c |= SOUTH;
    }
    if m.y > b.y1 {
        c |= NORTH;
    }
    c
}

/// A corner of a draped triangle clipped to a block's box: its weights of the triangle's three
/// corners, its place (Web Mercator) and the box's sides it lies on.
pub(crate) type Clipped = ([f64; 3], Coord, u8);

/// The part of the triangle with the corners `m` (Web Mercator) inside the box of `b`, a convex
/// polygon (empty when none is), Sutherland–Hodgman against each side. Corners are carried as
/// weights of the triangle's own, so the screen and the picture points follow from the triangle's
/// as its affine map draws it: the ground cut at the box's edge and the wall's top under it are
/// then the same points.
pub(crate) fn clip(m: [Coord; 3], b: &Block) -> Vec<Clipped> {
    let mut poly: Vec<Clipped> = vec![([1.0, 0.0, 0.0], m[0], 0), ([0.0, 1.0, 0.0], m[1], 0), ([0.0, 0.0, 1.0], m[2], 0)];
    let sides: [(u8, fn(&Block, Coord) -> f64); 4] =
        [(WEST, |b, c| c.x - b.x0), (EAST, |b, c| b.x1 - c.x), (SOUTH, |b, c| c.y - b.y0), (NORTH, |b, c| b.y1 - c.y)];
    for (bit, inside) in sides {
        let mut out = Vec::with_capacity(poly.len() + 1);
        for (i, &a) in poly.iter().enumerate() {
            let Some(&q) = poly.get((i + 1) % poly.len()) else { continue };
            let (da, dq) = (inside(b, a.1), inside(b, q.1));
            if da >= 0.0 {
                out.push(if da == 0.0 { (a.0, a.1, a.2 | bit) } else { a });
            }
            if (da >= 0.0) != (dq >= 0.0) && da != 0.0 && dq != 0.0 {
                let t = da / (da - dq);
                let w = [0, 1, 2].map(|k| a.0[k] + (q.0[k] - a.0[k]) * t);
                let c = Coord::new(a.1.x + (q.1.x - a.1.x) * t, a.1.y + (q.1.y - a.1.y) * t);
                // On this side, and on any other both ends lie on.
                out.push((w, c, (a.2 & q.2) | bit));
            }
        }
        poly = out;
        if poly.len() < 3 {
            return Vec::new();
        }
    }
    poly
}

/// How near a corner must lie to a side of the box to be on it (Web Mercator metres).
const SIDE_TOL: f64 = 1e-6;

/// Whether a corner of the triangle `m` (Web Mercator) lies on a side of the box of `b`. A triangle
/// wholly inside the box can still have a stretch of its edge under it: the triangles across that
/// edge are clipped to nothing, so this one has to give the wall its stretch ([`edges`]).
pub(crate) fn on_side(m: [Coord; 3], b: &Block) -> bool {
    let near = |d: f64| d.abs() <= SIDE_TOL;
    m.iter().any(|c| near(c.x - b.x0) || near(c.x - b.x1) || near(c.y - b.y0) || near(c.y - b.y1))
}

/// A stretch of the box's edge under a clipped triangle of ground: its side, its ends (Web
/// Mercator) and the ground's screen points over them (physical px), running round the box
/// anticlockwise (east along the south side, north along the east…), and how far it lies.
pub(crate) struct Edge {
    pub side: u8,
    pub ends: [Coord; 2],
    pub top: [(f64, f64); 2],
    pub depth: f64,
}

/// The stretches of the box's edge along the clipped polygon `poly`, whose corners stand on
/// screen at `screen` (physical px), `depth` away.
pub(crate) fn edges(poly: &[Clipped], screen: &[(f64, f64)], depth: f64, out: &mut Vec<Edge>) {
    for (i, a) in poly.iter().enumerate() {
        let j = (i + 1) % poly.len();
        let (Some(q), Some(&sa), Some(&sq)) = (poly.get(j), screen.get(i), screen.get(j)) else { continue };
        let common = a.2 & q.2;
        if common == 0 {
            continue;
        }
        let side = 1u8 << common.trailing_zeros();
        let forward = match side {
            SOUTH => q.1.x > a.1.x,
            EAST => q.1.y > a.1.y,
            NORTH => q.1.x < a.1.x,
            _ => q.1.y < a.1.y,
        };
        let (ends, top) = if forward { ([a.1, q.1], [sa, sq]) } else { ([q.1, a.1], [sq, sa]) };
        out.push(Edge { side, ends, top, depth });
    }
}

/// The box's sides in the order walls are drawn and [`Runs`] keeps them.
const SIDES: [u8; 4] = [SOUTH, EAST, NORTH, WEST];

/// Where a point lies along a side of the box: its x along the south and north sides, its y
/// along the east and west.
fn along(side: u8, m: Coord) -> f64 {
    if side & (SOUTH | NORTH) != 0 { m.x } else { m.y }
}

/// A stretch of one side of the box in one colour: from `lo` to `hi` along it ([`along`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Run {
    pub lo: f64,
    pub hi: f64,
    pub color: Rgba,
}

/// The colours along each side of a block's box, in [`SIDES`]' order.
pub(crate) type Runs = [Vec<Run>; 4];

/// Each side's stretches under one polygon of the walls layer, with the row giving it (none:
/// under no polygon), in [`SIDES`]' order.
type Units = [Vec<(f64, f64, Option<usize>)>; 4];

/// The rows, layer, definition query and box a [`Units`] was read for.
struct Key {
    rows: Weak<Vec<Feature>>,
    layer: String,
    query: Option<String>,
    sides: [f64; 4],
}

impl Key {
    fn same(&self, o: &Key) -> bool {
        self.rows.ptr_eq(&o.rows) && self.layer == o.layer && self.query == o.query && self.sides == o.sides
    }
}

/// The units read last, kept while their layer's rows and query and the box stay the same: a
/// view drawn as the map moves crosses no polygon with the box again. Up to [`KEPT`].
static LAST: Mutex<Vec<(Key, Arc<Units>)>> = Mutex::new(Vec::new());
/// How many units [`LAST`] keeps (maps drawn side by side, a layout's frames).
const KEPT: usize = 4;

/// The colours along the walls of block `b` on `map`. With a walls layer
/// (`TerrainBlock::walls_layer`, as [`crate::terrain::walls_layer`] takes it): the colour its
/// symbology gives the polygon under each stretch of the box's edge (the topmost where polygons
/// overlap, among the rows its definition query keeps), and the walls' own colour off every
/// polygon. Else the walls' colour all along.
///
/// Runs on the thread drawing the frame. The pieces the polygons cut the sides into
/// ([`units`]) are kept in [`LAST`], so they are read again only after an edit of the layer, a
/// new definition query or a new box: that read looks at each vertex once (a box test in
/// longitude and latitude, then the rings reaching the box only, put in Web Mercator) and
/// tests each piece's middle against the parts whose box holds it. The colours are read each
/// frame, a few pieces a side.
pub(crate) fn runs(map: &Map, b: &Block) -> Runs {
    let whole = |i: usize| {
        let (lo, hi) = if SIDES[i] & (SOUTH | NORTH) != 0 { (b.x0, b.x1) } else { (b.y0, b.y1) };
        vec![Run { lo, hi, color: b.walls }]
    };
    let layer = map
        .view
        .terrain
        .as_ref()
        .and_then(|t| t.block.as_ref())
        .and_then(|t| t.walls_layer.as_deref())
        .and_then(|n| crate::terrain::walls_layer(map, n));
    let Some(layer) = layer else { return [0, 1, 2, 3].map(whole) };
    let key = Key {
        rows: layer.dataset.features.allocation_key(),
        layer: layer.name.clone(),
        query: layer.definition_query.clone(),
        sides: [b.x0, b.y0, b.x1, b.y1],
    };
    let kept = LAST.lock().ok().and_then(|last| last.iter().find(|(k, _)| k.same(&key)).map(|(_, u)| u.clone()));
    let units = kept.unwrap_or_else(|| {
        let u = Arc::new(units(layer, b));
        if let Ok(mut last) = LAST.lock() {
            last.retain(|(k, _)| k.rows.strong_count() > 0 && !k.same(&key));
            if last.len() >= KEPT {
                last.remove(0);
            }
            last.push((key, u.clone()));
        }
        u
    });
    let styler = layer.styler(None);
    let color = |row: Option<usize>| match row {
        None => b.walls,
        Some(r) => {
            let (sym, _, fills) = styler.style_and_fills(r);
            unit_color(sym.fill, fills, b.walls)
        }
    };
    [0, 1, 2, 3].map(|i| {
        let mut out: Vec<Run> = Vec::new();
        for &(lo, hi, row) in &units[i] {
            let color = color(row);
            match out.last_mut() {
                Some(last) if last.color == color => last.hi = hi,
                _ => out.push(Run { lo, hi, color }),
            }
        }
        out
    })
}

/// The colour a wall takes under a polygon whose symbol fills with `fill`, or with the stacked
/// `fills` in its place: the topmost solid fill (patterns aren't drawn on walls), laid over the
/// walls' own colour `walls` by its opacity; `walls` where nothing solid fills it.
fn unit_color(fill: Rgba, fills: &[FillLayer], walls: Rgba) -> Rgba {
    let solid = if fills.is_empty() {
        Some(fill)
    } else {
        fills.iter().rev().find(|f| matches!(f.pattern, FillPattern::Solid)).map(|f| f.color.unwrap_or(fill))
    };
    solid.map_or(walls, |c| walls.lerp(c.with_alpha(255), f64::from(c.a()) / 255.0))
}

/// Each side of block `b`'s box cut where `layer`'s polygons cross it, with the row of the
/// topmost polygon over each piece, found at its middle: the contacts go straight down.
fn units(layer: &rustgis_map::Layer, b: &Block) -> Units {
    let keep = layer.query_rows(&layer.dataset);
    // The box in longitude and latitude (Web Mercator keeps north-up boxes north-up), so a part
    // off it is left before its rings are put in Web Mercator.
    let (lo, hi) = (merc_to_lonlat(Coord::new(b.x0, b.y0)), merc_to_lonlat(Coord::new(b.x1, b.y1)));
    // The polygons reaching the box (Web Mercator rings, and their box), each part on its own, in
    // row order.
    let mut parts: Vec<(usize, Vec<Vec<Coord>>, Extent)> = Vec::new();
    for (row, ft) in layer.dataset.features.iter().enumerate() {
        if keep.as_ref().is_some_and(|k| !k.get(row).copied().unwrap_or(false)) {
            continue;
        }
        let Some(Geometry::Polygon(polys)) = &ft.geometry else { continue };
        for rings in polys {
            let mut e = Extent::EMPTY;
            rings.iter().flatten().for_each(|c| e.expand(*c));
            if e.is_empty() || e.max_x < lo.x || e.min_x > hi.x || e.max_y < lo.y || e.min_y > hi.y {
                continue;
            }
            let rings: Vec<Vec<Coord>> = rings.iter().map(|r| r.iter().map(|c| lonlat_to_merc(*c)).collect()).collect();
            let mut e = Extent::EMPTY;
            rings.iter().flatten().for_each(|c| e.expand(*c));
            parts.push((row, rings, e));
        }
    }
    [0, 1, 2, 3].map(|i| {
        let side = SIDES[i];
        // The side as a line: across it at `at`, along it from `lo` to `hi`.
        let (at, lo, hi) = match side {
            SOUTH => (b.y0, b.x0, b.x1),
            NORTH => (b.y1, b.x0, b.x1),
            EAST => (b.x1, b.y0, b.y1),
            _ => (b.x0, b.y0, b.y1),
        };
        let across = |c: Coord| if side & (SOUTH | NORTH) != 0 { c.y } else { c.x };
        let point = |t: f64| if side & (SOUTH | NORTH) != 0 { Coord::new(t, at) } else { Coord::new(at, t) };
        let mut cuts = vec![lo, hi];
        for r in parts.iter().flat_map(|(_, rings, _)| rings) {
            for (j, a) in r.iter().enumerate() {
                let Some(&q) = r.get((j + 1) % r.len()) else { continue };
                let (da, dq) = (across(*a) - at, across(q) - at);
                if (da > 0.0) != (dq > 0.0) {
                    let t = along(side, *a) + da / (da - dq) * (along(side, q) - along(side, *a));
                    if t > lo && t < hi {
                        cuts.push(t);
                    }
                }
            }
        }
        cuts.sort_by(f64::total_cmp);
        cuts.dedup();
        let mut out: Vec<(f64, f64, Option<usize>)> = Vec::new();
        for w in cuts.windows(2) {
            let mid = point((w[0] + w[1]) / 2.0);
            let row = parts.iter().rev().find(|(_, rings, e)| e.contains(mid) && inside(rings, mid)).map(|(row, _, _)| *row);
            match out.last_mut() {
                Some(last) if last.2 == row => last.1 = w[1],
                _ => out.push((w[0], w[1], row)),
            }
        }
        out
    })
}

/// Whether `p` lies inside a polygon's `rings`, even-odd, so its holes are outside.
fn inside(rings: &[Vec<Coord>], p: Coord) -> bool {
    let mut inside = false;
    for r in rings {
        for (i, a) in r.iter().enumerate() {
            let Some(&q) = r.get((i + 1) % r.len()) else { continue };
            if (a.y > p.y) != (q.y > p.y) && p.x < a.x + (p.y - a.y) / (q.y - a.y) * (q.x - a.x) {
                inside = !inside;
            }
        }
    }
    inside
}

/// Draws the walls of block `b` over the ground already draped in `pm`: the sides facing the eye
/// (the box is convex, so no ground stands in front of them), each stretch of `edges` from the
/// ground down to the base in the colours `runs` gives along it ([`runs`]), lit by the way its
/// side faces the light as extruded walls are and darker toward its foot, fogged with distance as
/// the ground is (into `fog` toward the view's reach `far`), and outlined along the ground, the
/// base, the box's corners and the contacts between colours.
pub(crate) fn draw_walls(pm: &mut Pixmap, view: &View, ground: &Ground, b: &Block, edges: &[Edge], runs: &Runs, fog: Rgba, far: f64) {
    let fog_at = |depth: f64| crate::fog_share(depth, far);
    let k = f64::from(pm.width()) / view.width.max(1e-9);
    let cam = view.camera;
    let (w, h) = (view.width, view.height);
    let (eu, ev, _) = cam.eye(h);
    let centre = cam.ground_px(Coord::new((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0));
    let corners = [
        (SOUTH, (b.x0, b.y0), (b.x1, b.y0)),
        (EAST, (b.x1, b.y0), (b.x1, b.y1)),
        (NORTH, (b.x1, b.y1), (b.x0, b.y1)),
        (WEST, (b.x0, b.y1), (b.x0, b.y0)),
    ];
    let foot = |m: Coord| {
        let (u, v) = cam.ground_px(m);
        let up = (b.base / ground.per_unit(m) / cam.res.max(1e-9)).clamp(-1e7, 1e7);
        cam.project_raised(u, v, up, w, h).map(|(x, y)| (x * k, y * k)).filter(|p| p.0.is_finite() && p.1.is_finite())
    };
    let light_len = LIGHT.0.hypot(LIGHT.1).max(1e-9);
    let pt = |p: (f64, f64)| (p.0 as f32, p.1 as f32);
    let color = |c: Rgba| tiny_skia::Color::from_rgba8(c.r(), c.g(), c.b(), 255);
    for (s, (side, a, z)) in corners.into_iter().enumerate() {
        let (ga, gz) = (cam.ground_px(Coord::new(a.0, a.1)), cam.ground_px(Coord::new(z.0, z.1)));
        let (du, dv) = (gz.0 - ga.0, gz.1 - ga.1);
        let len = du.hypot(dv);
        if len <= 0.0 {
            continue;
        }
        let mid = ((ga.0 + gz.0) / 2.0, (ga.1 + gz.1) / 2.0);
        // The outward normal: away from the box's centre.
        let mut n = (dv / len, -du / len);
        if n.0 * (centre.0 - mid.0) + n.1 * (centre.1 - mid.1) > 0.0 {
            n = (-n.0, -n.1);
        }
        if n.0 * (eu - mid.0) + n.1 * (ev - mid.1) <= 0.0 {
            continue;
        }
        let lit = ((n.0 * LIGHT.0 + n.1 * LIGHT.1) / light_len).max(0.0);
        let (top_shade, foot_shade) = (0.6 + 0.4 * lit, (0.6 + 0.4 * lit) * 0.72);
        let near = |p: Coord, q: (f64, f64)| (p.x - q.0).abs() + (p.y - q.1).abs() <= 1e-9 * (b.x1 - b.x0 + b.y1 - b.y0);
        // Each stretch's ends (Web Mercator), feet and tops (physical px), and the outline along
        // the ground, the base and the box's corners.
        let mut quads: Vec<([Coord; 2], [(f64, f64); 2], [(f64, f64); 2])> = Vec::new();
        let mut outline = PathBuilder::new();
        let (mut foot_mid, mut top_mid, mut count, mut nearest) = ((0.0, 0.0), (0.0, 0.0), 0.0, f64::INFINITY);
        for e in edges.iter().filter(|e| e.side == side) {
            let (Some(fa), Some(fz)) = (foot(e.ends[0]), foot(e.ends[1])) else { continue };
            let [ta, tz] = e.top;
            quads.push((e.ends, [fa, fz], [ta, tz]));
            foot_mid = (foot_mid.0 + fa.0 + fz.0, foot_mid.1 + fa.1 + fz.1);
            top_mid = (top_mid.0 + ta.0 + tz.0, top_mid.1 + ta.1 + tz.1);
            count += 2.0;
            nearest = nearest.min(e.depth);
            for (p, q) in [(ta, tz), (fa, fz)] {
                outline.move_to(pt(p).0, pt(p).1);
                outline.line_to(pt(q).0, pt(q).1);
            }
            // The box's corners, straight down.
            for (i, end) in e.ends.iter().enumerate() {
                if near(*end, a) || near(*end, z) {
                    let (t, f) = if i == 0 { (ta, fa) } else { (tz, fz) };
                    outline.move_to(pt(t).0, pt(t).1);
                    outline.line_to(pt(f).0, pt(f).1);
                }
            }
        }
        if quads.is_empty() {
            continue;
        }
        // Shaded, then into the fog: in full fog both ends are the fog.
        let f = fog_at(nearest);
        // Darker toward the foot: from the stretches' feet on average to their tops, the same
        // across the side's colours so the shading runs on through their contacts.
        let avg = |p: (f64, f64)| Point::from_xy((p.0 / count) as f32, (p.1 / count) as f32);
        let grow = |p: (f64, f64), q: (f64, f64)| {
            let (dx, dy) = (p.0 - q.0, p.1 - q.1);
            let l = dx.hypot(dy).max(1e-6);
            pt((p.0 + dx / l * 0.4 * k, p.1 + dy / l * 0.4 * k))
        };
        let lerp = |p: (f64, f64), q: (f64, f64), t: f64| (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t);
        let runs = &runs[s];
        for run in runs {
            // The side in one path a colour, every stretch (cut to the colour's) a quad a hair
            // longer each way along the edge: filled at once, the overlaps leave no seam, where
            // stretches filled one by one striped the wall with their anti-aliased edges.
            let mut wall = PathBuilder::new();
            for (ends, [fa, fz], [ta, tz]) in &quads {
                let (ca, cz) = (along(side, ends[0]), along(side, ends[1]));
                let span = cz - ca;
                // The part of the stretch within the run, as shares of the stretch.
                let (t0, t1) = if span.abs() < 1e-12 {
                    if ca < run.lo || ca > run.hi { continue } else { (0.0, 1.0) }
                } else {
                    let (u, v) = ((run.lo - ca) / span, (run.hi - ca) / span);
                    (u.min(v).max(0.0), u.max(v).min(1.0))
                };
                if t1 <= t0 {
                    continue;
                }
                // The feet move along the base as the ends do; the tops as the ground's edge does.
                let (fa2, fz2) = (lerp(*fa, *fz, t0), lerp(*fa, *fz, t1));
                let (ta2, tz2) = (lerp(*ta, *tz, t0), lerp(*ta, *tz, t1));
                for (i, p) in [grow(fa2, fz2), grow(fz2, fa2), grow(tz2, ta2), grow(ta2, tz2)].iter().enumerate() {
                    if i == 0 { wall.move_to(p.0, p.1) } else { wall.line_to(p.0, p.1) }
                }
                wall.close();
            }
            let Some(path) = wall.finish() else { continue };
            let (top, foot) = (run.color.shade(top_shade).lerp(fog, f), run.color.shade(foot_shade).lerp(fog, f));
            let shader = tiny_skia::LinearGradient::new(
                avg(foot_mid),
                avg(top_mid),
                vec![GradientStop::new(0.0, color(foot)), GradientStop::new(1.0, color(top))],
                SpreadMode::Pad,
                Transform::identity(),
            );
            let paint_wall = match shader {
                Some(shader) => Paint { shader, anti_alias: true, ..Paint::default() },
                None => paint(top, 1.0),
            };
            pm.fill_path(&path, &paint_wall, FillRule::Winding, Transform::identity(), None);
        }
        // The contacts between colours, straight down.
        for c in runs.windows(2).map(|w| w[0].hi) {
            let at = quads.iter().find_map(|(ends, [fa, fz], [ta, tz])| {
                let (ca, cz) = (along(side, ends[0]), along(side, ends[1]));
                let inside = (cz - ca).abs() >= 1e-12 && (ca - c) * (cz - c) <= 0.0;
                inside.then(|| ((c - ca) / (cz - ca), *ta, *tz, *fa, *fz))
            });
            if let Some((t, ta, tz, fa, fz)) = at {
                let (top, ft) = (pt(lerp(ta, tz, t)), pt(lerp(fa, fz, t)));
                outline.move_to(top.0, top.1);
                outline.line_to(ft.0, ft.1);
            }
        }
        if let Some(path) = outline.finish() {
            let edge = b.walls.shade(0.45).lerp(fog, f);
            let stroke = Stroke { width: (0.7 * k) as f32, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() };
            pm.stroke_path(&path, &paint(edge, 1.0), &stroke, Transform::identity(), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_block() -> Block {
        Block { x0: 0.0, y0: 0.0, x1: 10.0, y1: 10.0, base: -100.0, settled: true, walls: Rgba::rgb(200, 184, 154) }
    }

    #[test]
    fn a_triangle_inside_the_box_is_kept_whole_and_one_outside_is_dropped() {
        let b = unit_block();
        let inside = clip([Coord::new(1.0, 1.0), Coord::new(5.0, 1.0), Coord::new(1.0, 5.0)], &b);
        assert_eq!(inside.len(), 3);
        assert!(inside.iter().all(|c| c.2 == 0));
        assert!(clip([Coord::new(11.0, 1.0), Coord::new(15.0, 1.0), Coord::new(11.0, 5.0)], &b).is_empty());
    }

    #[test]
    fn a_triangle_across_the_east_side_is_cut_there_and_gives_one_stretch_of_wall() {
        let b = unit_block();
        let m = [Coord::new(8.0, 2.0), Coord::new(12.0, 2.0), Coord::new(8.0, 6.0)];
        let poly = clip(m, &b);
        assert_eq!(poly.len(), 4);
        // Every corner's weights put it where it is.
        for (w, c, _) in &poly {
            let x = w[0] * m[0].x + w[1] * m[1].x + w[2] * m[2].x;
            let y = w[0] * m[0].y + w[1] * m[1].y + w[2] * m[2].y;
            assert!((x - c.x).abs() < 1e-9 && (y - c.y).abs() < 1e-9);
            assert!(c.x <= 10.0 + 1e-9);
        }
        let screen: Vec<(f64, f64)> = poly.iter().map(|c| (c.1.x, -c.1.y)).collect();
        let mut out = Vec::new();
        edges(&poly, &screen, 1.0, &mut out);
        assert_eq!(out.len(), 1);
        let e = &out[0];
        assert_eq!(e.side, EAST);
        // Northward along the east side: (10, 2) to (10, 4).
        assert!((e.ends[0].x - 10.0).abs() < 1e-9 && (e.ends[0].y - 2.0).abs() < 1e-9);
        assert!((e.ends[1].x - 10.0).abs() < 1e-9 && (e.ends[1].y - 4.0).abs() < 1e-9);
        assert_eq!(e.top, [(e.ends[0].x, -e.ends[0].y), (e.ends[1].x, -e.ends[1].y)]);
    }

    #[test]
    fn a_triangle_inside_with_an_edge_on_a_side_gives_the_wall_its_stretch() {
        // A mesh column exactly on the east side: the triangle inside it is wholly within the box,
        // and its edge on x = 10 is the wall's stretch (the outside triangles across it clip away).
        let b = unit_block();
        let m = [Coord::new(10.0, 2.0), Coord::new(10.0, 6.0), Coord::new(5.0, 4.0)];
        assert!(on_side(m, &b));
        assert!(!on_side([Coord::new(1.0, 1.0), Coord::new(5.0, 1.0), Coord::new(1.0, 5.0)], &b));
        let poly = clip(m, &b);
        let screen: Vec<(f64, f64)> = poly.iter().map(|c| (c.1.x, -c.1.y)).collect();
        let mut out = Vec::new();
        edges(&poly, &screen, 1.0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].side, EAST);
        assert!((out[0].ends[0].y - 2.0).abs() < 1e-9 && (out[0].ends[1].y - 6.0).abs() < 1e-9);
    }

    #[test]
    fn a_triangle_over_a_corner_gives_a_stretch_on_each_side() {
        let b = unit_block();
        let poly = clip([Coord::new(8.0, 8.0), Coord::new(14.0, 8.0), Coord::new(8.0, 14.0)], &b);
        let screen: Vec<(f64, f64)> = poly.iter().map(|c| (c.1.x, c.1.y)).collect();
        let mut out = Vec::new();
        edges(&poly, &screen, 1.0, &mut out);
        let mut sides: Vec<u8> = out.iter().map(|e| e.side).collect();
        sides.sort_unstable();
        assert_eq!(sides, vec![EAST, NORTH]);
        // Both meet at the box's corner (10, 10).
        assert!(out.iter().all(|e| e.ends.iter().any(|c| (c.x - 10.0).abs() < 1e-9 && (c.y - 10.0).abs() < 1e-9)));
    }

    /// A map whose block (the box ±1000 m round (0, 0)) has its walls coloured by `geo`: unit A
    /// over the west half, unit B over the north-east quarter with a hole round x 200–400 on the
    /// north side, and unit C over B at x 500–700 there, drawn above it.
    fn walls_by_units() -> (Map, Block) {
        use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
        let deg = |x: f64, y: f64| rustgis_geom::merc_to_lonlat(Coord::new(x, y));
        let rect = |x0: f64, y0: f64, x1: f64, y1: f64| vec![deg(x0, y0), deg(x1, y0), deg(x1, y1), deg(x0, y1), deg(x0, y0)];
        let mut d = Dataset::new("geo", GeometryType::Polygon, vec![Field::new("unit", FieldType::Text)]);
        d.push(Some(Geometry::Polygon(vec![vec![rect(-5000.0, -5000.0, 0.0, 5000.0)]])), vec![Value::Text("A".into())]);
        let b_rings = vec![rect(0.0, 0.0, 5000.0, 5000.0), rect(200.0, 900.0, 400.0, 1100.0)];
        d.push(Some(Geometry::Polygon(vec![b_rings])), vec![Value::Text("B".into())]);
        d.push(Some(Geometry::Polygon(vec![vec![rect(500.0, 900.0, 700.0, 1100.0)]])), vec![Value::Text("C".into())]);
        let mut m = Map::new("Map");
        m.add_dataset(d);
        m.view.terrain = Some(rustgis_map::view::Terrain {
            source: rustgis_map::view::TerrainSource::Layer("dem".into()),
            exaggeration: 1.0,
            block: Some(rustgis_map::view::TerrainBlock {
                extent: Some([-1000.0, -1000.0, 1000.0, 1000.0]),
                depth: None,
                walls: rustgis_map::view::BLOCK_WALLS,
                walls_layer: Some("geo".into()),
            }),
            water: None,
        });
        let b =
            Block { x0: -1000.0, y0: -1000.0, x1: 1000.0, y1: 1000.0, base: -100.0, settled: true, walls: rustgis_map::view::BLOCK_WALLS };
        (m, b)
    }

    #[test]
    fn walls_take_the_unit_under_the_boxs_edge_with_contacts_straight_down() {
        let (mut m, b) = walls_by_units();
        let cuts = |m: &Map| {
            let u = units(m.layer_by_name("geo").unwrap(), &b);
            u.map(|side| side.iter().map(|&(lo, hi, row)| (lo.round(), hi.round(), row)).collect::<Vec<_>>())
        };
        let [south, east, north, west] = cuts(&m);
        assert_eq!(south, vec![(-1000.0, 0.0, Some(0)), (0.0, 1000.0, None)]);
        assert_eq!(east, vec![(-1000.0, 0.0, None), (0.0, 1000.0, Some(1))]);
        // B's hole shows the walls' own colour; C, drawn above B, its own.
        assert_eq!(
            north,
            vec![
                (-1000.0, 0.0, Some(0)),
                (0.0, 200.0, Some(1)),
                (200.0, 400.0, None),
                (400.0, 500.0, Some(1)),
                (500.0, 700.0, Some(2)),
                (700.0, 1000.0, Some(1))
            ]
        );
        assert_eq!(west, vec![(-1000.0, 1000.0, Some(0))]);
        // In one symbol, the polygons merge into one colour; off them, the walls' colour.
        let fill = unit_color(m.layer_by_name("geo").unwrap().symbol(0).fill, &[], b.walls);
        let r = runs(&m, &b);
        assert_eq!(r[0].iter().map(|r| r.color).collect::<Vec<_>>(), vec![fill, b.walls]);
        assert_eq!(r[2].len(), 3, "{:?}", r[2]);
        // A definition query leaves its rows out: without A, the west wall is the walls' colour.
        m.layers[0].definition_query = Some("unit <> 'A'".into());
        assert_eq!(cuts(&m)[3], vec![(-1000.0, 1000.0, None)]);
        // Two layers of its name: which one colours the walls can't be told, so neither does.
        m.layers[0].definition_query = None;
        let mut copy = m.layers[0].clone();
        copy.id = 99;
        m.layers.push(copy);
        assert!(runs(&m, &b).iter().all(|side| side.len() == 1 && side[0].color == b.walls));
        m.layers.pop();
        assert_eq!(runs(&m, &b)[0].len(), 2);
        // No such layer: the walls' colour all along.
        m.view.terrain.as_mut().unwrap().block.as_mut().unwrap().walls_layer = Some("none".into());
        assert!(runs(&m, &b).iter().all(|side| side.len() == 1 && side[0].color == b.walls));
    }

    #[test]
    fn a_wall_takes_the_topmost_solid_fill_over_its_own_colour() {
        use rustgis_map::fills::FillLayer;
        let walls = Rgba::rgb(200, 184, 154);
        let red = Rgba::rgb(255, 0, 0);
        assert_eq!(unit_color(red, &[], walls), red);
        // Half see-through: half the walls' colour.
        assert_eq!(unit_color(red.with_alpha(128), &[], walls), walls.lerp(red, 128.0 / 255.0));
        // No fill: the walls' colour.
        assert_eq!(unit_color(red.with_alpha(0), &[], walls), walls);
        // Stacked fills: the topmost solid, its own colour or the symbol's; a hatch alone, none.
        let hatch = FillLayer {
            color: Some(Rgba::rgb(0, 0, 255)),
            pattern: FillPattern::Hatch { angle: 0.0, spacing: 4.0, width: 1.0, offset: 0.0 },
        };
        let solid = |c| FillLayer { color: c, pattern: FillPattern::Solid };
        assert_eq!(unit_color(red, &[solid(Some(Rgba::rgb(0, 128, 0))), solid(None), hatch], walls), red);
        assert_eq!(unit_color(red, &[hatch], walls), walls);
    }
}
