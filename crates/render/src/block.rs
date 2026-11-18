//! Block diagrams (#1914): a tilted view's ground over terrain cut to a box, and the box's sides
//! facing the eye drawn as walls from the ground's profile down to a flat base.

use std::sync::{Arc, Mutex, Weak};

use rustgis_data::{Feature, Geometry};
use rustgis_geom::{Coord, Extent, lonlat_to_merc, merc_to_lonlat};
use rustgis_map::fills::{FillLayer, FillPattern};
use rustgis_map::{Map, Rgba};
use tiny_skia::{FillRule, GradientStop, LineCap, LineJoin, Mask, Paint, PathBuilder, Pixmap, Point, SpreadMode, Stroke, Transform};

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

/// A stretch of one side of the box in one colour: from `lo` to `hi` along it ([`along`]), and
/// the patterns over the colour, bottom first, each in its colour ([`unit_paint`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Run {
    pub lo: f64,
    pub hi: f64,
    pub color: Rgba,
    pub patterns: Vec<(FillPattern, Rgba)>,
}

/// The colours along each side of a block's box, in [`SIDES`]' order.
pub(crate) type Runs = [Vec<Run>; 4];

/// Each side's stretches under one polygon of the walls layer, with the row giving it (none:
/// under no polygon), in [`SIDES`]' order.
type Units = [Vec<(f64, f64, Option<usize>)>; 4];

/// The rows, layer, query, query context and box a [`Units`] was read for.
struct Key {
    rows: Weak<Vec<Feature>>,
    layer: String,
    query: Option<String>,
    page: Option<Arc<rustgis_data::query::SeriesPage>>,
    sides: [f64; 4],
}

impl Key {
    fn same(&self, o: &Key) -> bool {
        self.rows.ptr_eq(&o.rows) && self.layer == o.layer && self.query == o.query && self.page == o.page && self.sides == o.sides
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
/// polygon, with the patterns over it grown or shrunk as the layer's reference scale asks at the
/// map scale 1:`scale` (`Layer::size_factor`), as the ground's are. Else the walls' colour all
/// along.
///
/// Runs on the thread drawing the frame. The pieces the polygons cut the sides into
/// ([`units`]) are kept in [`LAST`], so they are read again only after an edit of the layer, a
/// new definition query or a new box: that read looks at each vertex once (a box test in
/// longitude and latitude, then the rings reaching the box only, put in Web Mercator) and
/// tests each piece's middle against the parts whose box holds it. The colours are read each
/// frame, a few pieces a side.
pub(crate) fn runs(map: &Map, b: &Block, scale: f64, env: &rustgis_data::query::Env) -> Runs {
    let whole = |i: usize| {
        let (lo, hi) = if SIDES[i] & (SOUTH | NORTH) != 0 { (b.x0, b.x1) } else { (b.y0, b.y1) };
        vec![Run { lo, hi, color: b.walls, patterns: Vec::new() }]
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
        page: env.page.as_ref().filter(|_| layer.definition_query.as_deref().is_some_and(rustgis_data::query::reads_series_page)).cloned(),
        sides: [b.x0, b.y0, b.x1, b.y1],
    };
    let kept = LAST.lock().ok().and_then(|last| last.iter().find(|(k, _)| k.same(&key)).map(|(_, u)| u.clone()));
    let units = kept.unwrap_or_else(|| {
        let u = Arc::new(units(layer, b, env));
        if let Ok(mut last) = LAST.lock() {
            last.retain(|(k, _)| k.rows.strong_count() > 0 && !k.same(&key));
            if last.len() >= KEPT {
                last.remove(0);
            }
            last.push((key, u.clone()));
        }
        u
    });
    let styler = layer.styler_with(env.clone());
    let grow = layer.size_factor(scale);
    let look = |row: Option<usize>| match row {
        None => (b.walls, Vec::new()),
        Some(r) => {
            let (sym, _, fills) = styler.style_and_fills(r);
            let (color, mut patterns) = unit_paint(sym.fill, fills, b.walls);
            if grow != 1.0 {
                patterns.iter_mut().for_each(|(p, _)| *p = grown(*p, grow));
            }
            (color, patterns)
        }
    };
    [0, 1, 2, 3].map(|i| {
        let mut out: Vec<Run> = Vec::new();
        for &(lo, hi, row) in &units[i] {
            let (color, patterns) = look(row);
            match out.last_mut() {
                Some(last) if last.color == color && last.patterns == patterns => last.hi = hi,
                _ => out.push(Run { lo, hi, color, patterns }),
            }
        }
        out
    })
}

/// How a wall looks under a polygon whose symbol fills with `fill`, or with the stacked `fills`
/// in its place: the topmost solid fill laid over the walls' own colour `walls` by its opacity
/// (`walls` where nothing solid fills it), and the hatches and markers stacked over that fill,
/// bottom first, each in its colour (a fill without one takes `fill`). Gradients and shapebursts
/// are left off walls.
fn unit_paint(fill: Rgba, fills: &[FillLayer], walls: Rgba) -> (Rgba, Vec<(FillPattern, Rgba)>) {
    let top = fills.iter().rposition(|f| matches!(f.pattern, FillPattern::Solid));
    let solid = if fills.is_empty() { Some(fill) } else { top.and_then(|i| fills.get(i)).map(|f| f.color.unwrap_or(fill)) };
    let color = solid.map_or(walls, |c| walls.lerp(c.with_alpha(255), f64::from(c.a()) / 255.0));
    let patterns = fills
        .iter()
        .skip(top.map_or(0, |i| i + 1))
        .filter(|f| matches!(f.pattern, FillPattern::Hatch { .. } | FillPattern::Dots { .. }) && f.draws(fill))
        .map(|f| (f.pattern, f.color.unwrap_or(fill)))
        .collect();
    (color, patterns)
}

/// `pattern` with its sizes, spacings and shifts `k` times as large.
fn grown(pattern: FillPattern, k: f32) -> FillPattern {
    match pattern {
        FillPattern::Hatch { angle, spacing, width, offset } => {
            FillPattern::Hatch { angle, spacing: spacing * k, width: width * k, offset: offset * k }
        }
        FillPattern::Dots { marker, size, outline, outline_width, spacing, displacement, offset } => FillPattern::Dots {
            marker,
            size: size * k,
            outline,
            outline_width: outline_width * k,
            spacing: spacing.map(|v| v * k),
            displacement: displacement.map(|v| v * k),
            offset: offset.map(|v| v * k),
        },
        other => other,
    }
}

/// Each side of block `b`'s box cut where `layer`'s polygons cross it, with the row of the
/// topmost polygon over each piece, found at its middle: the contacts go straight down.
fn units(layer: &rustgis_map::Layer, b: &Block, env: &rustgis_data::query::Env) -> Units {
    let keep = layer.query_rows_with(&layer.dataset, env);
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

/// The most hatch lines or markers a pattern draws on one stretch of wall: past it (a pattern far
/// finer than the wall on screen), the stretch takes the pattern's ink as one colour instead, as a
/// polygon under a pixel does on the map (`pattern_fill::tint`).
const MAX_MARKS: f64 = 20_000.0;

/// Draws `patterns` (hatch lines, markers on a grid; `tone` gives each colour its light and fog)
/// on a stretch of wall, cut to `mask`. A pattern is laid out in the wall's own plane, across it
/// (left to right as seen from outside the box) and up, both in logical px at the view centre's
/// scale: a pattern's point is one of them, so it is as big on the wall as on the ground at the
/// centre, and its lattice runs from the map's origin and the ground's zero, so neighbouring
/// stretches share it. `across` and `up` bound the stretch; `at` puts a point of the plane on
/// screen (physical px; `None` behind the eye), so each line or marker stands in perspective.
/// `scale` is the physical px a point spans on screen there, for line widths and marker sizes.
fn draw_patterns(
    pm: &mut Pixmap,
    mask: &Mask,
    patterns: &[(FillPattern, Rgba)],
    (across, up): ((f64, f64), (f64, f64)),
    scale: f64,
    tone: impl Fn(Rgba) -> Rgba,
    at: impl Fn(f64, f64) -> Option<(f64, f64)>,
) {
    let pt = |p: (f64, f64)| (p.0 as f32, p.1 as f32);
    for &(pattern, c) in patterns {
        let c = tone(c);
        // Too many lines or markers: the pattern's ink over the stretch as one colour.
        let tint = |pm: &mut Pixmap| {
            // A dot's outline lit and fogged as its fill is.
            let mut pattern = pattern;
            if let FillPattern::Dots { outline: Some(o), .. } = &mut pattern {
                *o = tone(*o);
            }
            let ink = crate::pattern_fill::tint(&[FillLayer { color: Some(c), pattern }], c);
            let all = tiny_skia::Rect::from_xywh(0.0, 0.0, pm.width() as f32, pm.height() as f32);
            if let Some(r) = all {
                pm.fill_rect(r, &paint(ink, 1.0), Transform::identity(), Some(mask));
            }
        };
        match pattern {
            FillPattern::Hatch { angle, spacing, width, offset } => {
                let sp = f64::from(spacing);
                if sp <= 0.0 || width <= 0.0 || !sp.is_finite() {
                    continue;
                }
                // Lines whose stroke reaches into the stretch from just outside it are drawn too
                // (the mask keeps them to the stretch), so a slanted hatch runs on to the contact.
                let pad = f64::from(width) / 2.0 + 1.0;
                let (across, up) = ((across.0 - pad, across.1 + pad), (up.0 - pad, up.1 + pad));
                // Lines along (cos, sin) a spacing apart across them, through the origin shifted
                // by the offset; each cut to the stretch's box, which stays a straight line in
                // perspective.
                let (s, co) = f64::from(angle).to_radians().sin_cos();
                let n = (-s, co);
                let ds = [(across.0, up.0), (across.1, up.0), (across.0, up.1), (across.1, up.1)].map(|p| n.0 * p.0 + n.1 * p.1);
                let (lo, hi) = (ds.iter().copied().fold(f64::INFINITY, f64::min), ds.iter().copied().fold(f64::NEG_INFINITY, f64::max));
                let off = f64::from(offset);
                let (first, last) = (((lo - off) / sp).ceil(), ((hi - off) / sp).floor());
                if !(last - first).is_finite() || last - first >= MAX_MARKS {
                    tint(pm);
                    continue;
                }
                let mut pb = PathBuilder::new();
                let mut j = first;
                while j <= last {
                    let d = off + j * sp;
                    j += 1.0;
                    let o = (d * n.0, d * n.1);
                    let (mut t0, mut t1) = (f64::NEG_INFINITY, f64::INFINITY);
                    for (p, q, (from, to)) in [(o.0, co, across), (o.1, s, up)] {
                        if q.abs() < 1e-12 {
                            if p < from || p > to {
                                t1 = t0;
                            }
                        } else {
                            let (a, b) = ((from - p) / q, (to - p) / q);
                            t0 = t0.max(a.min(b));
                            t1 = t1.min(a.max(b));
                        }
                    }
                    if t1 <= t0 {
                        continue;
                    }
                    // Seen from where the eye is in front of it: the part in front is one piece (the
                    // eye's front is a half-plane of the wall's plane), so an end behind it is
                    // brought back to where the line leaves the view, found by halving.
                    let seen = |t: f64| at(o.0 + co * t, o.1 + s * t);
                    let last_seen = |seen_t: f64, hidden_t: f64| {
                        let (mut good, mut bad) = (seen_t, hidden_t);
                        for _ in 0..40 {
                            let mid = (good + bad) / 2.0;
                            if seen(mid).is_some() { good = mid } else { bad = mid }
                        }
                        seen(good)
                    };
                    let (a, b) = match (seen(t0), seen(t1)) {
                        (Some(a), Some(b)) => (a, b),
                        (Some(a), None) => match last_seen(t0, t1) {
                            Some(b) => (a, b),
                            None => continue,
                        },
                        (None, Some(b)) => match last_seen(t1, t0) {
                            Some(a) => (a, b),
                            None => continue,
                        },
                        (None, None) => continue,
                    };
                    let (a, b) = (pt(a), pt(b));
                    pb.move_to(a.0, a.1);
                    pb.line_to(b.0, b.1);
                }
                if let Some(path) = pb.finish() {
                    let stroke = Stroke { width: (f64::from(width) * scale) as f32, ..Stroke::default() };
                    pm.stroke_path(&path, &paint(c, 1.0), &stroke, Transform::identity(), Some(mask));
                }
            }
            FillPattern::Dots { marker, size, outline, outline_width, spacing, displacement, offset } => {
                let [dx, dy] = spacing.map(f64::from);
                let [sx, sy] = displacement.map(f64::from);
                if dx <= 0.0 || dy <= 0.0 || !dx.is_finite() || !dy.is_finite() {
                    continue;
                }
                // The grid as the map lays it (rows down, odd rows shifted across, odd columns
                // down), down being down the wall; markers reaching in from just outside too.
                let reach = f64::from(size.max(1.0)) + sx.abs().max(sy.abs());
                let (down0, down1) = (-up.1 - reach, -up.0 + reach);
                let (i0, i1) =
                    (((across.0 - reach - f64::from(offset[0])) / dx).floor(), ((across.1 + reach - f64::from(offset[0])) / dx).ceil());
                let (j0, j1) = (((down0 - f64::from(offset[1])) / dy).floor(), ((down1 - f64::from(offset[1])) / dy).ceil());
                let marks = (i1 - i0 + 1.0) * (j1 - j0 + 1.0);
                if !marks.is_finite() || marks >= MAX_MARKS {
                    tint(pm);
                    continue;
                }
                let r = (f64::from(size.max(1.0)) * scale / 2.0) as f32;
                let ink = outline.filter(|o| o.a() > 0 && outline_width > 0.0).map(&tone);
                let (mut i, mut fill, mut edge) = (i0, PathBuilder::new(), PathBuilder::new());
                while i <= i1 {
                    let mut j = j0;
                    while j <= j1 {
                        let (odd_i, odd_j) = ((i as i64).rem_euclid(2) == 1, (j as i64).rem_euclid(2) == 1);
                        let x = (i + 0.5) * dx + if odd_j { sx } else { 0.0 } + f64::from(offset[0]);
                        let y = (j + 0.5) * dy + if odd_i { sy } else { 0.0 } + f64::from(offset[1]);
                        j += 1.0;
                        let Some((px, py)) = at(x, -y) else { continue };
                        if let Some(path) = crate::marker_path(px as f32, py as f32, r, marker) {
                            fill.push_path(&path);
                            if ink.is_some() {
                                edge.push_path(&path);
                            }
                        }
                    }
                    i += 1.0;
                }
                if c.a() > 0
                    && let Some(path) = fill.finish()
                {
                    pm.fill_path(&path, &paint(c, 1.0), FillRule::Winding, Transform::identity(), Some(mask));
                }
                if let (Some(o), Some(path)) = (ink, edge.finish()) {
                    let stroke = Stroke { width: (f64::from(outline_width) * scale) as f32, ..Stroke::default() };
                    pm.stroke_path(&path, &paint(o, 1.0), &stroke, Transform::identity(), Some(mask));
                }
            }
            _ => {}
        }
    }
}

/// Draws the walls of block `b` over the ground already draped in `pm`: the sides facing the eye
/// (the box is convex, so no ground stands in front of them), each stretch of `edges` from the
/// ground down to the base in the colours `runs` gives along it ([`runs`]) with their patterns
/// over them ([`draw_patterns`]), lit by the way its side faces the light as extruded walls are
/// and darker toward its foot, fogged with distance as the ground is (into `fog` toward the view's
/// reach `far`), and outlined along the ground, the base, the box's corners and the contacts
/// between colours.
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
    let res = cam.res.max(1e-9);
    // Where a run's patterns are cut to (one for every run, cleared each time).
    let mut mask: Option<Mask> = None;
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
        // The side's plane for its patterns ([`draw_patterns`]): across in logical px at the
        // centre's scale, left to right seen from outside (the sides run anticlockwise round the
        // box), and up as `Ground::up_px` gives it; from a little under the base to a little over
        // the highest ground along it.
        let dir = if side & (SOUTH | EAST) != 0 { 1.0 } else { -1.0 };
        let on_side = |t: f64| if side & (SOUTH | NORTH) != 0 { Coord::new(t, a.1) } else { Coord::new(a.0, t) };
        let at = |x: f64, up: f64| {
            let (u, v) = cam.ground_px(on_side(x * res * dir));
            cam.project_raised(u, v, up, w, h).map(|(x, y)| (x * k, y * k)).filter(|p| p.0.is_finite() && p.1.is_finite())
        };
        let (mut up_lo, mut up_hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for m in quads.iter().flat_map(|(ends, _, _)| ends) {
            up_lo = up_lo.min((b.base / ground.per_unit(*m) / res).clamp(-1e7, 1e7));
            up_hi = up_hi.max(ground.up_px(*m, &cam));
        }
        let pad = (up_hi - up_lo).abs() * 0.05 + 1.0;
        let up_range = (up_lo - pad, up_hi + pad);
        // A pattern's point on screen at the side's middle: the mean of its spans across and up.
        let mid = Coord::new((a.0 + z.0) / 2.0, (a.1 + z.1) / 2.0);
        let (mid_x, mid_up) = (along(side, mid) / res * dir, (up_range.0 + up_range.1) / 2.0);
        let span = |p: Option<(f64, f64)>, q: Option<(f64, f64)>| p.zip(q).map(|(p, q)| (q.0 - p.0).hypot(q.1 - p.1));
        let scale = match (span(at(mid_x, mid_up), at(mid_x + 1.0, mid_up)), span(at(mid_x, mid_up), at(mid_x, mid_up + 1.0))) {
            (Some(x), Some(y)) if (x * y).sqrt().is_finite() => (x * y).sqrt(),
            _ => k,
        };
        let shade = (top_shade + foot_shade) / 2.0;
        let tone = |c: Rgba| c.shade(shade).lerp(fog.with_alpha(c.a()), f);
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
            if run.patterns.is_empty() {
                continue;
            }
            if mask.is_none() {
                mask = Mask::new(pm.width(), pm.height());
            }
            if let Some(cut) = mask.as_mut() {
                cut.data_mut().fill(0);
                cut.fill_path(&path, FillRule::Winding, true, Transform::identity());
                let (x0, x1) = (run.lo / res * dir, run.hi / res * dir);
                draw_patterns(pm, cut, &run.patterns, ((x0.min(x1), x0.max(x1)), up_range), scale, tone, at);
            }
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
            let u = units(m.layer_by_name("geo").unwrap(), &b, &Default::default());
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
        let fill = unit_paint(m.layer_by_name("geo").unwrap().symbol(0).fill, &[], b.walls).0;
        let r = runs(&m, &b, 10_000.0, &Default::default());
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
        assert!(runs(&m, &b, 10_000.0, &Default::default()).iter().all(|side| side.len() == 1 && side[0].color == b.walls));
        m.layers.pop();
        assert_eq!(runs(&m, &b, 10_000.0, &Default::default())[0].len(), 2);
        // A hatch over the layer's fill is in every run under a polygon; with a reference scale
        // twice the map's, at twice its size, as on the ground.
        let hatch = FillPattern::Hatch { angle: 0.0, spacing: 4.0, width: 1.0, offset: 0.5 };
        m.layers[0].fill_layers = vec![FillLayer { color: None, pattern: FillPattern::Solid }, FillLayer { color: None, pattern: hatch }];
        let ink = m.layer_by_name("geo").unwrap().symbol(0).fill;
        assert_eq!(runs(&m, &b, 10_000.0, &Default::default())[0][0].patterns, vec![(hatch, ink)]);
        assert!(runs(&m, &b, 10_000.0, &Default::default())[0][1].patterns.is_empty(), "off every polygon, none");
        m.layers[0].reference_scale = Some(20_000.0);
        let big = FillPattern::Hatch { angle: 0.0, spacing: 8.0, width: 2.0, offset: 1.0 };
        assert_eq!(runs(&m, &b, 10_000.0, &Default::default())[0][0].patterns, vec![(big, ink)]);
        m.layers[0].reference_scale = None;
        m.layers[0].fill_layers.clear();
        // No such layer: the walls' colour all along.
        m.view.terrain.as_mut().unwrap().block.as_mut().unwrap().walls_layer = Some("none".into());
        assert!(runs(&m, &b, 10_000.0, &Default::default()).iter().all(|side| side.len() == 1 && side[0].color == b.walls));
    }

    #[test]
    fn a_walls_hatch_reaches_its_contacts_and_one_too_fine_shows_as_its_ink() {
        // A 20 px square stretch of wall, its plane straight on the screen (up is up).
        let mut pm = Pixmap::new(20, 20).unwrap();
        let mut mask = Mask::new(20, 20).unwrap();
        let square = PathBuilder::from_rect(tiny_skia::Rect::from_xywh(0.0, 0.0, 20.0, 20.0).unwrap());
        mask.fill_path(&square, FillRule::Winding, false, Transform::identity());
        let at = |x: f64, up: f64| Some((x, 20.0 - up));
        let blue = Rgba::rgb(0, 0, 255);
        let ink = |pm: &Pixmap, x: u32| pm.pixel(x, 10).unwrap().alpha();
        // A line 10 px wide centred at x 0, outside the stretch at 1–4 across: its stroke still
        // covers the stretch.
        let wide = FillPattern::Hatch { angle: 90.0, spacing: 20.0, width: 10.0, offset: 0.0 };
        draw_patterns(&mut pm, &mask, &[(wide, blue)], ((1.0, 4.0), (0.0, 20.0)), 1.0, |c| c, at);
        assert!(ink(&pm, 2) > 200 && ink(&pm, 8) == 0);
        // Lines 1 px apart over a stretch a million wide: too many; the ink as one colour, a
        // quarter of blue for lines a quarter as wide as their spacing.
        let mut pm = Pixmap::new(20, 20).unwrap();
        let fine = FillPattern::Hatch { angle: 0.0, spacing: 1.0, width: 0.25, offset: 0.0 };
        draw_patterns(&mut pm, &mask, &[(fine, blue)], ((0.0, 1e6), (0.0, 1e6)), 1.0, |c| c, at);
        assert!((i32::from(ink(&pm, 10)) - 64).abs() <= 2, "{}", ink(&pm, 10));
        // So does an outline-only dot pattern, its outline toned as its fill is: in full fog
        // (grey), no blue left.
        let mut pm = Pixmap::new(20, 20).unwrap();
        let hollow = FillPattern::Dots {
            marker: rustgis_map::Marker::Circle,
            size: 3.0,
            outline: Some(blue),
            outline_width: 1.0,
            spacing: [1.0, 1.0],
            displacement: [0.0, 0.0],
            offset: [0.0, 0.0],
        };
        let fogged = |c: Rgba| Rgba::rgb(128, 128, 128).with_alpha(c.a());
        draw_patterns(&mut pm, &mask, &[(hollow, Rgba::new(0, 0, 0, 0))], ((0.0, 1e6), (0.0, 1e6)), 1.0, fogged, at);
        let p = pm.pixel(10, 10).unwrap().demultiply();
        assert!(p.alpha() > 0 && p.blue() == p.red(), "{p:?}");
        // A vertical line whose top is behind the eye (here, above 15 up) is drawn up to where it
        // leaves the view, not left off.
        let mut pm = Pixmap::new(20, 20).unwrap();
        let near = |x: f64, up: f64| (up <= 15.0).then_some((x, 20.0 - up));
        let upright = FillPattern::Hatch { angle: 90.0, spacing: 20.0, width: 2.0, offset: 10.0 };
        draw_patterns(&mut pm, &mask, &[(upright, blue)], ((0.0, 20.0), (0.0, 20.0)), 1.0, |c| c, near);
        assert!(ink(&pm, 10) > 200, "drawn");
        assert_eq!(pm.pixel(10, 2).unwrap().alpha(), 0, "not past the view");
    }

    #[test]
    fn a_wall_takes_the_topmost_solid_fill_over_its_own_colour_and_the_patterns_over_it() {
        use rustgis_map::fills::FillLayer;
        let unit_color = |fill, fills: &[FillLayer], walls| unit_paint(fill, fills, walls).0;
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
        // The hatches and markers over the topmost solid fill, bottom first, in their colours (the
        // symbol's without one); those under it, and gradients, are left off.
        let dots = FillLayer {
            color: None,
            pattern: FillPattern::Dots {
                marker: rustgis_map::Marker::Circle,
                size: 2.0,
                outline: None,
                outline_width: 0.0,
                spacing: [6.0, 6.0],
                displacement: [3.0, 0.0],
                offset: [0.0, 0.0],
            },
        };
        let gradient = FillLayer { color: Some(red), pattern: serde_json::from_str(r#"{"kind": "gradient"}"#).unwrap() };
        let blue = Rgba::rgb(0, 0, 255);
        let stack = [hatch, solid(None), hatch, gradient, dots];
        assert_eq!(unit_paint(red, &stack, walls), (red, vec![(hatch.pattern, blue), (dots.pattern, red)]));
        assert_eq!(unit_paint(red, &[hatch], walls), (walls, vec![(hatch.pattern, blue)]));
        // A pattern that draws nothing (no width) is left off too.
        let bare = FillLayer { color: Some(blue), pattern: FillPattern::Hatch { angle: 0.0, spacing: 4.0, width: 0.0, offset: 0.0 } };
        assert_eq!(unit_paint(red, &[bare], walls), (walls, Vec::new()));
    }
}
