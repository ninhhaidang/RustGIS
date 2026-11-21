//! Block diagrams (#1914): a tilted view's ground over terrain cut to a box, and the box's sides
//! facing the eye drawn as walls from the ground's profile down to a flat base.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use rustgis_data::{Feature, Geometry};
use rustgis_geom::{Coord, Extent, lonlat_to_merc, merc_to_lonlat};
use rustgis_map::fills::{FillLayer, FillPattern};
use rustgis_map::{Map, Rgba};
use tiny_skia::{FillRule, GradientStop, LineCap, LineJoin, Mask, Paint, PathBuilder, Pixmap, Point, SpreadMode, Stroke, Transform};

use crate::effects::LIGHT;
use crate::terrain::{Block, Ground, Outline};
use crate::{View, paint};

/// The box's sides as bits: west (x0), east (x1), south (y0), north (y1). A point outside the box
/// has the bits of the sides it lies beyond ([`outcode`]); a corner of a clipped triangle, those
/// of the sides it lies on ([`clip`]).
pub(crate) const WEST: u8 = 1;
pub(crate) const EAST: u8 = 2;
pub(crate) const SOUTH: u8 = 4;
pub(crate) const NORTH: u8 = 8;

/// A round or polygon block's point off its outline but beyond none of the tangents (or box
/// sides) [`outcode`] tells, so a triangle of such points may still reach inside and is cut, not
/// left out.
pub(crate) const ROUND_OFF: u16 = 0x100;

/// The sides of a round block's outline (#2318): a regular polygon in its circle, fine enough
/// that its walls look round (a side sags under 2e-5 of the radius from the circle).
pub(crate) const ROUND_SIDES: usize = 512;

/// Each side of a round block's outline: its outward normal and, from the centre, the direction
/// of its first corner, anticlockwise from east.
fn round_sides() -> &'static [((f64, f64), (f64, f64))] {
    static SIDES: OnceLock<Vec<((f64, f64), (f64, f64))>> = OnceLock::new();
    SIDES.get_or_init(|| {
        let step = std::f64::consts::TAU / ROUND_SIDES as f64;
        (0..ROUND_SIDES)
            .map(|k| {
                let ((s, c), (vs, vc)) = (((k as f64 + 0.5) * step).sin_cos(), (k as f64 * step).sin_cos());
                ((c, s), (vc, vs))
            })
            .collect()
    })
}

/// How far inside side `k`'s line of the round outline about `o` of radius `r` the point `m`
/// lies (negative beyond it).
fn round_inside(o: Coord, r: f64, k: usize, m: Coord) -> f64 {
    let (n, _) = round_sides()[k % ROUND_SIDES];
    r * (std::f64::consts::PI / ROUND_SIDES as f64).cos() - ((m.x - o.x) * n.0 + (m.y - o.y) * n.1)
}

/// Corner `k` of the round outline about `o` of radius `r`, where side `k` begins.
fn round_corner(o: Coord, r: f64, k: usize) -> Coord {
    let (_, d) = round_sides()[k % ROUND_SIDES];
    Coord::new(o.x + r * d.0, o.y + r * d.1)
}

/// The side of the round outline about `o` whose sector holds `m`: the nearest line to it.
fn round_side_at(o: Coord, m: Coord) -> usize {
    let a = (m.y - o.y).atan2(m.x - o.x).rem_euclid(std::f64::consts::TAU);
    ((a / std::f64::consts::TAU * ROUND_SIDES as f64) as usize).min(ROUND_SIDES - 1)
}

/// The sides of `b` the point `m` lies beyond (Web Mercator): 0 inside. A round block's are the
/// sides of the box round its circle and its four diagonal tangents (each wholly off the
/// circle), with [`ROUND_OFF`] for a point off its outline.
pub(crate) fn outcode(b: &Block, m: Coord) -> u16 {
    let mut c = 0;
    if m.x < b.x0 {
        c |= u16::from(WEST);
    }
    if m.x > b.x1 {
        c |= u16::from(EAST);
    }
    if m.y < b.y0 {
        c |= u16::from(SOUTH);
    }
    if m.y > b.y1 {
        c |= u16::from(NORTH);
    }
    if let Some((o, r)) = b.circle {
        let d = std::f64::consts::FRAC_1_SQRT_2;
        for (i, n) in [(d, d), (-d, d), (-d, -d), (d, -d)].into_iter().enumerate() {
            if (m.x - o.x) * n.0 + (m.y - o.y) * n.1 > r {
                c |= 16 << i;
            }
        }
        if round_inside(o, r, round_side_at(o, m), m) < 0.0 {
            c |= ROUND_OFF;
        }
    }
    if b.outline.as_ref().is_some_and(|o| !o.contains(m)) {
        c |= ROUND_OFF;
    }
    c
}

/// A corner of a draped triangle clipped to a block's outline: its weights of the triangle's
/// three corners, its place (Web Mercator) and the sides it lies on: a box's as bits; a round
/// block's as up to two side numbers, each plus one, in the low and high 16 bits ([`with_side`]).
pub(crate) type Clipped = ([f64; 3], Coord, u32);

/// A round block's sides `on` with side `k` added (a third is left out: a corner lies on two).
fn with_side(on: u32, k: usize) -> u32 {
    let k = k as u32 + 1;
    match (on & 0xFFFF, on >> 16) {
        (a, b) if a == k || b == k => on,
        (0, _) => on | k,
        (_, 0) => on | (k << 16),
        _ => on,
    }
}

/// The side numbers (plus one, 0 for none) of a round block's sides `on`.
fn sides_of(on: u32) -> [u32; 2] {
    [on & 0xFFFF, on >> 16]
}

/// The sides both `a` and `q` lie on, of a round block's.
fn shared(a: u32, q: u32) -> u32 {
    sides_of(a).into_iter().filter(|s| *s != 0 && sides_of(q).contains(s)).fold(0, |on, s| with_side(on, s as usize - 1))
}

/// `poly` cut to the side of a line where `inside` is at least 0, Sutherland–Hodgman: a corner on
/// the line takes `mark`, a corner where an edge crosses it the sides `common` its ends share,
/// marked. Empty when under 3 corners are left.
fn cut(poly: &[Clipped], inside: impl Fn(Coord) -> f64, mark: impl Fn(u32) -> u32, common: impl Fn(u32, u32) -> u32) -> Vec<Clipped> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    for (i, &a) in poly.iter().enumerate() {
        let Some(&q) = poly.get((i + 1) % poly.len()) else { continue };
        let (da, dq) = (inside(a.1), inside(q.1));
        if da >= 0.0 {
            out.push(if da == 0.0 { (a.0, a.1, mark(a.2)) } else { a });
        }
        if (da >= 0.0) != (dq >= 0.0) && da != 0.0 && dq != 0.0 {
            let t = da / (da - dq);
            let w = [0, 1, 2].map(|k| a.0[k] + (q.0[k] - a.0[k]) * t);
            let c = Coord::new(a.1.x + (q.1.x - a.1.x) * t, a.1.y + (q.1.y - a.1.y) * t);
            // On this side, and on any other both ends lie on.
            out.push((w, c, mark(common(a.2, q.2))));
        }
    }
    if out.len() < 3 { Vec::new() } else { out }
}

/// The part of the triangle with the corners `m` (Web Mercator) inside the outline of `b`, a
/// convex polygon (empty when none is), Sutherland–Hodgman against each side. Corners are carried
/// as weights of the triangle's own, so the screen and the picture points follow from the
/// triangle's as its affine map draws it: the ground cut at the outline and the wall's top under
/// it are then the same points. A round block's sides the triangle lies wholly inside are skipped.
pub(crate) fn clip(m: [Coord; 3], b: &Block) -> Vec<Clipped> {
    let mut poly: Vec<Clipped> = vec![([1.0, 0.0, 0.0], m[0], 0), ([0.0, 1.0, 0.0], m[1], 0), ([0.0, 0.0, 1.0], m[2], 0)];
    if let Some((o, r)) = b.circle {
        for k in 0..ROUND_SIDES {
            if poly.iter().all(|c| round_inside(o, r, k, c.1) > 0.0) {
                continue;
            }
            poly = cut(&poly, |c| round_inside(o, r, k, c), |on| with_side(on, k), shared);
            if poly.is_empty() {
                break;
            }
        }
        return poly;
    }
    let sides: [(u8, fn(&Block, Coord) -> f64); 4] =
        [(WEST, |b, c| c.x - b.x0), (EAST, |b, c| b.x1 - c.x), (SOUTH, |b, c| c.y - b.y0), (NORTH, |b, c| b.y1 - c.y)];
    for (bit, inside) in sides {
        poly = cut(&poly, |c| inside(b, c), |on| on | u32::from(bit), |a, q| a & q);
        if poly.is_empty() {
            break;
        }
    }
    poly
}

/// The parts of the triangle with the corners `m` (Web Mercator) inside the outline of `b`: the
/// one convex part [`clip`] gives a box or a circle, or a polygon block's parts
/// ([`clip_outline`]), each corner marked as [`clip`] marks it.
pub(crate) fn pieces(m: [Coord; 3], b: &Block) -> Vec<Vec<Clipped>> {
    match &b.outline {
        Some(o) => clip_outline(m, o),
        None => vec![clip(m, b)],
    }
}

/// The box round three points: its lowest and its highest corner.
fn corners_box(m: [Coord; 3]) -> (Coord, Coord) {
    let (xs, ys) = (m.map(|c| c.x), m.map(|c| c.y));
    let lo = Coord::new(xs.iter().copied().fold(f64::INFINITY, f64::min), ys.iter().copied().fold(f64::INFINITY, f64::min));
    let hi = Coord::new(xs.iter().copied().fold(f64::NEG_INFINITY, f64::max), ys.iter().copied().fold(f64::NEG_INFINITY, f64::max));
    (lo, hi)
}

/// The parts of the triangle `m` (Web Mercator) inside the polygon outline `o`, each a polygon
/// (concave, maybe) anticlockwise: each stretch of a ring inside the triangle runs from where it
/// enters to where it leaves, then along the triangle's edge (anticlockwise, as the rings run)
/// to where the next stretch of the same ring enters, until the part closes. A ring wholly inside
/// is a part of its own. A ring that crosses none of it but holds it (its middle) adds the
/// whole triangle, which several touching or overlapping polygons can give alongside the parts of
/// another ring that crosses it. A corner is marked with the outline's side (plus one) the part's edge from it runs along, 0
/// along the triangle's edge. Only the sides near the triangle are looked at ([`Outline::near`]).
fn clip_outline(m: [Coord; 3], o: &Outline) -> Vec<Vec<Clipped>> {
    const EPS: f64 = 1e-9;
    let cross = |a: Coord, b: Coord, p: Coord| (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
    let area = cross(m[0], m[1], m[2]);
    if !(area.abs() > 0.0 && area.is_finite()) {
        return Vec::new();
    }
    let t = if area > 0.0 { m } else { [m[0], m[2], m[1]] };
    let mark = |p: Coord, on: u32| -> Clipped { ([0, 1, 2].map(|i| cross(m[(i + 1) % 3], m[(i + 2) % 3], p) / area), p, on) };
    // The whole triangle when a ring that doesn't cross it holds its middle (a point strictly
    // inside, so a ring only touching it doesn't count).
    let middle = Coord::new((t[0].x + t[1].x + t[2].x) / 3.0, (t[0].y + t[1].y + t[2].y) / 3.0);
    let whole = |crossing: &[u32]| -> Option<Vec<Clipped>> {
        o.inside_ring(middle, |r| !crossing.contains(&r)).then(|| t.iter().map(|c| mark(*c, 0)).collect())
    };
    // How far inside edge `e` (from corner e to the next) `p` lies.
    let len = [0, 1, 2].map(|e| (t[(e + 1) % 3].x - t[e].x).hypot(t[(e + 1) % 3].y - t[e].y).max(1e-300));
    let inside = |e: usize, p: Coord| cross(t[e], t[(e + 1) % 3], p) / len[e];
    let (lo, hi) = corners_box(t);
    let mut near = Vec::new();
    o.near(lo, hi, &mut near);
    let sides = o.sides();
    // Each near side's stretch inside the triangle, as shares of the side, by side.
    let mut parts: Vec<(usize, f64, f64)> = Vec::new();
    for &k in &near {
        let Some(s) = sides.get(k as usize) else { continue };
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        for e in 0..3 {
            let (fa, fz) = (inside(e, s.a), inside(e, s.z));
            if fa < 0.0 && fz < 0.0 {
                (t0, t1) = (1.0, 0.0);
            } else if fa < 0.0 {
                t0 = t0.max(fa / (fa - fz));
            } else if fz < 0.0 {
                t1 = t1.min(fa / (fa - fz));
            }
        }
        if t1 - t0 > 1e-12 {
            parts.push((k as usize, t0, t1));
        }
    }
    if parts.is_empty() {
        return whole(&[]).into_iter().collect();
    }
    let mut crossing: Vec<u32> = parts.iter().map(|p| sides[p.0].ring).collect();
    crossing.sort_unstable();
    crossing.dedup();
    let find = |k: usize| parts.binary_search_by_key(&k, |p| p.0).ok();
    let at = |k: usize, u: f64| sides.get(k).map_or(t[0], |s| Coord::new(s.a.x + (s.z.x - s.a.x) * u, s.a.y + (s.z.y - s.a.y) * u));
    // Where on the triangle's edge a point lies: the edge's number plus the share along it.
    let place = |p: Coord| {
        let e = (0..3).min_by(|a, b| inside(*a, p).abs().total_cmp(&inside(*b, p).abs())).unwrap_or(0);
        let (a, z) = (t[e], t[(e + 1) % 3]);
        let u = ((p.x - a.x) * (z.x - a.x) + (p.y - a.y) * (z.y - a.y)) / (len[e] * len[e]);
        (e as f64 + u.clamp(0.0, 1.0)).rem_euclid(3.0)
    };
    // The stretch after stretch `i` round its ring, when it goes on from it inside the triangle.
    let next = |i: usize| {
        let (k, _, t1) = parts[i];
        (t1 >= 1.0 - EPS).then(|| find(o.neighbours(k).1)).flatten().filter(|j| parts[*j].1 <= EPS)
    };
    let goes_on = |i: usize| {
        let (k, t0, _) = parts[i];
        t0 <= EPS && find(o.neighbours(k).0).is_some_and(|j| next(j) == Some(i))
    };
    // Stretch `start` and those going on from it, as corners, and where the last leaves.
    let follow = |start: usize, used: &mut [bool]| -> (Vec<Clipped>, Coord) {
        let (k0, t0, _) = parts[start];
        let (mut i, mut corners) = (start, vec![mark(at(k0, t0), k0 as u32 + 1)]);
        loop {
            used[i] = true;
            match next(i).filter(|j| !used[*j]) {
                Some(j) => {
                    corners.push(mark(at(parts[j].0, 0.0), parts[j].0 as u32 + 1));
                    i = j;
                }
                None => return (corners, at(parts[i].0, parts[i].2)),
            }
        }
    };
    // The chains entering the triangle: (ring, corners, where it enters, where it leaves).
    let mut used = vec![false; parts.len()];
    let mut chains: Vec<(u32, Vec<Clipped>, f64, f64)> = Vec::new();
    for i in 0..parts.len() {
        if used[i] || goes_on(i) {
            continue;
        }
        let (mut corners, out) = follow(i, &mut used);
        let into = corners.first().map_or(0.0, |c| place(c.1));
        corners.push(mark(out, 0));
        chains.push((sides[parts[i].0].ring, corners, into, place(out)));
    }
    // Rings wholly inside the triangle: parts of their own.
    let mut out: Vec<Vec<Clipped>> = Vec::new();
    for i in 0..parts.len() {
        if !used[i] {
            let (corners, _) = follow(i, &mut used);
            if corners.len() >= 3 {
                out.push(corners);
            }
        }
    }
    out.extend(whole(&crossing));
    if chains.is_empty() {
        return out;
    }
    // Each chain, then the triangle's edge on to the next chain of its ring, until back.
    let mut done = vec![false; chains.len()];
    for c0 in 0..chains.len() {
        if done[c0] {
            continue;
        }
        let (mut piece, mut c) = (Vec::new(), c0);
        loop {
            done[c] = true;
            piece.extend(chains[c].1.iter().copied());
            let (ring, leaves) = (chains[c].0, chains[c].3);
            let ahead = |s: f64| (s - leaves).rem_euclid(3.0);
            let Some(n) =
                (0..chains.len()).filter(|j| chains[*j].0 == ring).min_by(|a, b| ahead(chains[*a].2).total_cmp(&ahead(chains[*b].2)))
            else {
                break;
            };
            let reach = ahead(chains[n].2);
            for step in 1..=3 {
                let corner = (leaves.floor() as usize + step) % 3;
                let d = ahead(corner as f64);
                if d > EPS && d < reach - EPS {
                    piece.push(mark(t[corner], 0));
                }
            }
            if n == c0 || done[n] {
                break;
            }
            c = n;
        }
        if piece.len() >= 3 {
            out.push(piece);
        }
    }
    out
}

/// How near a corner must lie to a side of the box to be on it (Web Mercator metres).
const SIDE_TOL: f64 = 1e-6;

/// Whether a corner of the triangle `m` (Web Mercator) lies on a side of the outline of `b`. A
/// triangle wholly inside it can still have a stretch of its edge under it: the triangles across
/// that edge are clipped to nothing, so this one has to give the wall its stretch ([`edges`]).
pub(crate) fn on_side(m: [Coord; 3], b: &Block) -> bool {
    let near = |d: f64| d.abs() <= SIDE_TOL;
    // A polygon block's: whether one of its sides reaches the triangle's box ([`clip_outline`]
    // tells what of it is inside).
    if let Some(o) = &b.outline {
        let (lo, hi) = corners_box(m);
        let mut sides = Vec::new();
        o.near(lo, hi, &mut sides);
        return !sides.is_empty();
    }
    if let Some((o, r)) = b.circle {
        return m.iter().any(|c| near(round_inside(o, r, round_side_at(o, *c), *c)));
    }
    m.iter().any(|c| near(c.x - b.x0) || near(c.x - b.x1) || near(c.y - b.y0) || near(c.y - b.y1))
}

/// A stretch of the outline under a clipped triangle of ground: its side (a box's bit, a round
/// block's number), its ends (Web Mercator) and the ground's screen points over them (physical
/// px), running round the outline anticlockwise (east along the south side, north along the
/// east…), and how far it lies.
pub(crate) struct Edge {
    pub side: u16,
    pub ends: [Coord; 2],
    pub top: [(f64, f64); 2],
    pub depth: f64,
}

/// The stretches of the outline of `b` along the clipped polygon `poly`, whose corners stand on
/// screen at `screen` (physical px), `depth` away.
pub(crate) fn edges(b: &Block, poly: &[Clipped], screen: &[(f64, f64)], depth: f64, out: &mut Vec<Edge>) {
    for (i, a) in poly.iter().enumerate() {
        let j = (i + 1) % poly.len();
        let (Some(q), Some(&sa), Some(&sq)) = (poly.get(j), screen.get(i), screen.get(j)) else { continue };
        let (side, forward) = if let Some(o) = &b.outline {
            let Some(s) = (a.2 as usize).checked_sub(1).and_then(|k| o.sides().get(k)) else { continue };
            ((a.2 - 1) as u16, (q.1.x - a.1.x) * (s.z.x - s.a.x) + (q.1.y - a.1.y) * (s.z.y - s.a.y) > 0.0)
        } else if let Some((o, r)) = b.circle {
            let on = shared(a.2, q.2) & 0xFFFF;
            if on == 0 {
                continue;
            }
            let k = on as usize - 1;
            let (p, z) = (round_corner(o, r, k), round_corner(o, r, k + 1));
            (k as u16, (q.1.x - a.1.x) * (z.x - p.x) + (q.1.y - a.1.y) * (z.y - p.y) > 0.0)
        } else {
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
            (u16::from(side), forward)
        };
        let (ends, top) = if forward { ([a.1, q.1], [sa, sq]) } else { ([q.1, a.1], [sq, sa]) };
        out.push(Edge { side, ends, top, depth });
    }
}

/// The box's sides in the order walls are drawn and [`Runs`] keeps them.
const SIDES: [u8; 4] = [SOUTH, EAST, NORTH, WEST];

impl Block {
    /// How many sides the outline has: a box's 4, a round block's [`ROUND_SIDES`].
    pub(crate) fn sides(&self) -> usize {
        match (&self.outline, self.circle) {
            (Some(o), _) => o.sides().len(),
            (None, Some(_)) => ROUND_SIDES,
            (None, None) => SIDES.len(),
        }
    }

    /// The sides before and after side `i` round its ring (a box's round the box).
    pub(crate) fn neighbours(&self, i: usize) -> (usize, usize) {
        match &self.outline {
            Some(o) => o.neighbours(i),
            None => ((i + self.sides() - 1) % self.sides(), (i + 1) % self.sides()),
        }
    }

    /// The length round the closed outline side `key` lies on, where its walls meet themselves
    /// (Web Mercator metres): a round block's circle, a polygon block's ring. None: a box's side,
    /// whose wall ends at its corners.
    pub(crate) fn around(&self, key: u16) -> Option<f64> {
        match (&self.outline, self.circle) {
            (Some(o), _) => Some(o.around(usize::from(key))),
            (None, Some((_, r))) => Some(r * std::f64::consts::TAU),
            (None, None) => None,
        }
    }

    /// Side `i` of the outline in drawing order ([`Runs`]'): its key in [`Edge::side`] and its
    /// ends, anticlockwise round the outline.
    pub(crate) fn side(&self, i: usize) -> (u16, Coord, Coord) {
        if let Some(s) = self.outline.as_ref().and_then(|o| o.sides().get(i)) {
            return (i as u16, s.a, s.z);
        }
        if let Some((o, r)) = self.circle {
            return (i as u16, round_corner(o, r, i), round_corner(o, r, i + 1));
        }
        let (x0, y0, x1, y1) = (self.x0, self.y0, self.x1, self.y1);
        let side = SIDES[i % 4];
        let (a, z) = match side {
            SOUTH => ((x0, y0), (x1, y0)),
            EAST => ((x1, y0), (x1, y1)),
            NORTH => ((x1, y1), (x0, y1)),
            _ => ((x0, y1), (x0, y0)),
        };
        (u16::from(side), Coord::new(a.0, a.1), Coord::new(z.0, z.1))
    }

    /// Where a point lies along side `key` of the outline: a box's x along its south and north
    /// sides, its y along the east and west; along a round block's, the arc from east
    /// anticlockwise (Web Mercator metres), straight along each side, so its walls' patterns run
    /// on round it; a polygon block's, along its ring from its first corner.
    pub(crate) fn along(&self, key: u16, m: Coord) -> f64 {
        if let Some(s) = self.outline.as_ref().and_then(|o| o.sides().get(usize::from(key))) {
            let (dx, dy) = (s.z.x - s.a.x, s.z.y - s.a.y);
            return s.along + ((m.x - s.a.x) * dx + (m.y - s.a.y) * dy) / dx.hypot(dy).max(1e-300);
        }
        if let Some((o, r)) = self.circle {
            let k = usize::from(key);
            let (p, z) = (round_corner(o, r, k), round_corner(o, r, k + 1));
            let (dx, dy) = (z.x - p.x, z.y - p.y);
            let t = ((m.x - p.x) * dx + (m.y - p.y) * dy) / (dx * dx + dy * dy).max(1e-300);
            return r * std::f64::consts::TAU / ROUND_SIDES as f64 * (k as f64 + t);
        }
        if key & u16::from(SOUTH | NORTH) != 0 { m.x } else { m.y }
    }

    /// The point `t` along side `key` ([`Block::along`]) on its line.
    pub(crate) fn point(&self, key: u16, t: f64) -> Coord {
        if let Some(s) = self.outline.as_ref().and_then(|o| o.sides().get(usize::from(key))) {
            let (dx, dy) = (s.z.x - s.a.x, s.z.y - s.a.y);
            let u = (t - s.along) / dx.hypot(dy).max(1e-300);
            return Coord::new(s.a.x + dx * u, s.a.y + dy * u);
        }
        if let Some((o, r)) = self.circle {
            let k = usize::from(key);
            let (p, z) = (round_corner(o, r, k), round_corner(o, r, k + 1));
            let s = t / (r * std::f64::consts::TAU / ROUND_SIDES as f64) - k as f64;
            return Coord::new(p.x + (z.x - p.x) * s, p.y + (z.y - p.y) * s);
        }
        match key as u8 {
            SOUTH => Coord::new(t, self.y0),
            NORTH => Coord::new(t, self.y1),
            EAST => Coord::new(self.x1, t),
            _ => Coord::new(self.x0, t),
        }
    }

    /// How far beyond side `key`'s line `m` lies, as a box's side's coordinate across it less
    /// the side's (a round block's: positive outside).
    fn beyond(&self, key: u16, m: Coord) -> f64 {
        // A polygon's rings run anticlockwise: outside is to the right of each side.
        if let Some(s) = self.outline.as_ref().and_then(|o| o.sides().get(usize::from(key))) {
            let (dx, dy) = (s.z.x - s.a.x, s.z.y - s.a.y);
            return ((m.x - s.a.x) * dy - (m.y - s.a.y) * dx) / dx.hypot(dy).max(1e-300);
        }
        if let Some((o, r)) = self.circle {
            return -round_inside(o, r, usize::from(key), m);
        }
        match key as u8 {
            SOUTH => m.y - self.y0,
            NORTH => m.y - self.y1,
            EAST => m.x - self.x1,
            _ => m.x - self.x0,
        }
    }
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

/// The colours along each side of a block's outline, in [`Block::side`]'s order.
pub(crate) type Runs = Vec<Vec<Run>>;

/// Each side's stretches under one polygon of the walls layer, with the row giving it (none:
/// under no polygon), in [`Block::side`]'s order.
type Units = Vec<Vec<(f64, f64, Option<usize>)>>;

/// A polygon reaching a block's box, each part of a multipolygon on its own: its row, its rings
/// (Web Mercator) and their box.
type Part = (usize, Vec<Vec<Coord>>, Extent);

/// A layer's [`Units`] along a block's outline, and for the water (#2317) the polygons reaching
/// the box, whose shores shape the water's floor: each by its row and its place among the row's
/// parts, read again from the layer's rows (the key holds them), so a large lake or sea crossing
/// the box is not kept twice.
struct Cut {
    units: Units,
    parts: Vec<(usize, usize)>,
}

/// The rows, layer, query, query context, box, circle and outline a [`Cut`] was read for, and
/// whether for the water.
struct Key {
    rows: Weak<Vec<Feature>>,
    layer: String,
    query: Option<String>,
    page: Option<Arc<rustgis_data::query::SeriesPage>>,
    sides: [f64; 4],
    circle: Option<(Coord, f64)>,
    outline: Option<Weak<Outline>>,
    water: bool,
}

impl Key {
    fn same(&self, o: &Key) -> bool {
        self.rows.ptr_eq(&o.rows)
            && self.layer == o.layer
            && self.query == o.query
            && self.page == o.page
            && self.sides == o.sides
            && self.circle == o.circle
            && match (&self.outline, &o.outline) {
                (Some(a), Some(b)) => a.ptr_eq(b),
                (a, b) => a.is_none() && b.is_none(),
            }
            && self.water == o.water
    }
}

/// The cuts read last, kept while their layer's rows and query and the box stay the same: a
/// view drawn as the map moves crosses no polygon with the box again. Up to [`KEPT`].
static LAST: Mutex<Vec<(Key, Arc<Cut>)>> = Mutex::new(Vec::new());
/// How many cuts [`LAST`] keeps (maps drawn side by side, a layout's frames, walls and water).
const KEPT: usize = 4;

/// `layer`'s [`Cut`] along block `b`'s outline, from [`LAST`] when it was read already.
fn layer_cut(layer: &rustgis_map::Layer, b: &Block, water: bool, env: &rustgis_data::query::Env) -> Arc<Cut> {
    let key = Key {
        rows: layer.dataset.features.allocation_key(),
        layer: layer.name.clone(),
        query: layer.definition_query.clone(),
        page: env.page.as_ref().filter(|_| layer.definition_query.as_deref().is_some_and(rustgis_data::query::reads_series_page)).cloned(),
        sides: [b.x0, b.y0, b.x1, b.y1],
        circle: b.circle,
        outline: b.outline.as_ref().map(Arc::downgrade),
        water,
    };
    let kept = LAST.lock().ok().and_then(|last| last.iter().find(|(k, _)| k.same(&key)).map(|(_, u)| u.clone()));
    kept.unwrap_or_else(|| {
        let u = Arc::new(units(layer, b, water, env));
        if let Ok(mut last) = LAST.lock() {
            last.retain(|(k, _)| k.rows.strong_count() > 0 && !k.same(&key));
            if last.len() >= KEPT {
                last.remove(0);
            }
            last.push((key, u.clone()));
        }
        u
    })
}

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
        let (key, a, z) = b.side(i);
        let (lo, hi) = (b.along(key, a), b.along(key, z));
        vec![Run { lo: lo.min(hi), hi: lo.max(hi), color: b.walls, patterns: Vec::new() }]
    };
    let layer = map
        .view
        .terrain
        .as_ref()
        .and_then(|t| t.block.as_ref())
        .and_then(|t| t.walls_layer.as_deref())
        .and_then(|n| crate::terrain::walls_layer(map, n));
    let Some(layer) = layer else { return (0..b.sides()).map(whole).collect() };
    crate::note_held_read(layer);
    let cut = layer_cut(layer, b, false, env);
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
    let side_runs = |side: &Vec<(f64, f64, Option<usize>)>| {
        let mut out: Vec<Run> = Vec::new();
        for &(lo, hi, row) in side {
            let (color, patterns) = look(row);
            match out.last_mut() {
                Some(last) if last.color == color && last.patterns == patterns => last.hi = hi,
                _ => out.push(Run { lo, hi, color, patterns }),
            }
        }
        out
    };
    cut.units.iter().map(side_runs).collect()
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

/// How close (metres along a side, a millimetre) two places where polygons cross a block's side are
/// taken as one: far below anything drawn, far above the rounding of any conversion.
const SEAM: f64 = 1e-3;

/// Each side of block `b`'s outline cut where `layer`'s polygons cross it, with the row of the
/// topmost polygon over each piece, found at its middle: the contacts go straight down. For the
/// water (`water`), the first polygon in row order instead, as the ground's level takes it, given
/// as its place in the polygons reaching the box (each part of a multipolygon on its own), which
/// are kept by row and part.
fn units(layer: &rustgis_map::Layer, b: &Block, water: bool, env: &rustgis_data::query::Env) -> Cut {
    let keep = layer.query_rows_with(&layer.dataset, env);
    // The box in longitude and latitude (Web Mercator keeps north-up boxes north-up), so a part
    // off it is left before its rings are put in Web Mercator.
    let (lo, hi) = (merc_to_lonlat(Coord::new(b.x0, b.y0)), merc_to_lonlat(Coord::new(b.x1, b.y1)));
    // The polygons reaching the box (Web Mercator rings, and their box), each part on its own, in
    // row order.
    let (mut parts, mut ids): (Vec<Part>, Vec<(usize, usize)>) = (Vec::new(), Vec::new());
    // Its conversion to longitude/latitude, made once for its polygons (none: a system that can't
    // be converted, of which the map draws nothing either).
    let to = layer.dataset.shown_conversion().ok();
    for (row, ft) in layer.dataset.features.iter().enumerate() {
        if keep.as_ref().is_some_and(|k| !k.get(row).copied().unwrap_or(false)) {
            continue;
        }
        // In longitude/latitude, whatever system the layer holds its coordinates in; a polygon
        // that can't be given there makes no contact, as the map draws none of it.
        let Some(lonlat) = ft.geometry.as_ref().zip(to.as_ref()).and_then(|(g, to)| rustgis_data::Dataset::converted_to_lonlat(to, g))
        else {
            continue;
        };
        let Geometry::Polygon(polys) = &*lonlat else { continue };
        for (k, rings) in polys.iter().enumerate() {
            let mut e = Extent::EMPTY;
            rings.iter().flatten().for_each(|c| e.expand(*c));
            if e.is_empty() || e.max_x < lo.x || e.min_x > hi.x || e.max_y < lo.y || e.min_y > hi.y {
                continue;
            }
            let rings: Vec<Vec<Coord>> = rings.iter().map(|r| r.iter().map(|c| lonlat_to_merc(*c)).collect()).collect();
            let mut e = Extent::EMPTY;
            rings.iter().flatten().for_each(|c| e.expand(*c));
            parts.push((row, rings, e));
            ids.push((row, k));
        }
    }
    let side_units = |i: usize| {
        // The side as a line: across it as `beyond` tells, along it from `lo` to `hi`.
        let (side, p, z) = b.side(i);
        let (lo, hi) = (b.along(side, p).min(b.along(side, z)), b.along(side, p).max(b.along(side, z)));
        // Only the parts reaching the side can cross it (a round block has many short sides).
        let mut reach = Extent::EMPTY;
        reach.expand(p);
        reach.expand(z);
        let near = |e: &Extent| e.max_x >= reach.min_x && e.min_x <= reach.max_x && e.max_y >= reach.min_y && e.min_y <= reach.max_y;
        // Where the polygons cross the side, away from its ends.
        let mut cuts = Vec::new();
        for r in parts.iter().filter(|(_, _, e)| near(e)).flat_map(|(_, rings, _)| rings) {
            for (j, a) in r.iter().enumerate() {
                let Some(&q) = r.get((j + 1) % r.len()) else { continue };
                let (da, dq) = (b.beyond(side, *a), b.beyond(side, q));
                if (da > 0.0) != (dq > 0.0) {
                    let t = b.along(side, *a) + da / (da - dq) * (b.along(side, q) - b.along(side, *a));
                    if t > lo + SEAM && t < hi - SEAM {
                        cuts.push(t);
                    }
                }
            }
        }
        cuts.sort_by(f64::total_cmp);
        // Neighbours sharing an edge cross the side at one place, give or take the rounding of
        // their coordinates (a layer held in another system), so cuts closer than `SEAM` are
        // one: no sliver of bare wall between them.
        cuts.dedup_by(|a, b| *a - *b < SEAM);
        cuts.insert(0, lo);
        cuts.push(hi);
        let mut out: Vec<(f64, f64, Option<usize>)> = Vec::new();
        for w in cuts.windows(2) {
            let mid = b.point(side, (w[0] + w[1]) / 2.0);
            let over = |p: &Part| p.2.contains(mid) && inside(&p.1, mid);
            // The water's pieces keep their part, so each part's shore shapes its own floor.
            let row = if water { parts.iter().position(over) } else { parts.iter().rev().find(|p| over(p)).map(|p| p.0) };
            match out.last_mut() {
                Some(last) if last.2 == row => last.1 = w[1],
                _ => out.push((w[0], w[1], row)),
            }
        }
        out
    };
    let units = (0..b.sides()).map(side_units).collect();
    Cut { units, parts: if water { ids } else { Vec::new() } }
}

/// A stretch of one side of the outline inside a water body (#2317): from `lo` to `hi` along it
/// ([`Block::along`]), in the water layer's fill `color`, from the water's `level` down to
/// `depth` below it (metres, exaggerated as [`Ground::height`]'s are) where the shore is `ramp`
/// or more away (Web Mercator), less nearer it; `shore` holds the shore's edges near the side
/// (Web Mercator), and `dark` how dark the line along the floor is, as the shore band at its
/// outline.
pub(crate) struct WaterRun {
    pub lo: f64,
    pub hi: f64,
    pub color: Rgba,
    pub level: f64,
    pub depth: f64,
    ramp: f64,
    shore: Vec<(Coord, Coord)>,
    dark: f64,
}

impl WaterRun {
    /// The floor's height at `m` on the side: the level at the shore, falling evenly to the full
    /// depth `ramp` in from it, so the section shows a basin and no cliff at the shoreline.
    fn floor(&self, m: Coord) -> f64 {
        let near = self.shore.iter().map(|&(a, q)| segment_distance(m, a, q)).fold(f64::INFINITY, f64::min);
        let share = if self.ramp > 0.0 { (near / self.ramp).min(1.0) } else { 1.0 };
        self.level - self.depth * share
    }
}

/// How far `p` lies from the segment from `a` to `q`.
fn segment_distance(p: Coord, a: Coord, q: Coord) -> f64 {
    let (dx, dy) = (q.x - a.x, q.y - a.y);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p.x - a.x) * dx + (p.y - a.y) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (p.x - a.x - t * dx).hypot(p.y - a.y - t * dy)
}

/// The water along each side of a block's outline, in [`Block::side`]'s order.
pub(crate) type WaterRuns = Vec<Vec<WaterRun>>;

/// The colour a water body's section takes where its symbol has no solid fill.
const SECTION_WATER: Rgba = Rgba::rgb(0x8F, 0xB8, 0xDE);

/// The water block `b`'s walls cut through on `map` (#2317), when its block has `water` and its
/// terrain a water layer ([`crate::terrain::walls_layer`]'s checks): each stretch of the
/// outline inside one of its polygons (the first in row order, among those its definition query
/// keeps) that has a level on `ground` and a depth (its row's value of the depth field, else the
/// depth given), with the shore band's reach at the view centre's scale (`res`, map units a
/// logical pixel) as the floor's ramp, styled at the view centre's drawing scale (`scale`, its
/// fill's `@map_scale`). Runs on the thread drawing the frame; the stretches are
/// kept as the walls' units are ([`layer_cut`]). Each frame keeps, once for each water body a stretch
/// lies in, the edges of its rings within the ramp of the box, then for each stretch those
/// within the ramp of it, a few comparisons an edge (the ramp follows the view's scale, so they
/// can't be kept with the stretches).
pub(crate) fn water_runs(map: &Map, b: &Block, ground: &Ground, res: f64, scale: f64, env: &rustgis_data::query::Env) -> WaterRuns {
    let mut out: WaterRuns = (0..b.sides()).map(|_| Vec::new()).collect();
    let Some(t) = map.view.terrain.as_ref() else { return out };
    let (Some(bw), Some(tw), Some(water)) = (t.block.as_ref().and_then(|b| b.water.as_ref()), t.water.as_ref(), ground.water()) else {
        return out;
    };
    let Some(layer) = crate::terrain::walls_layer(map, &tw.layer) else { return out };
    crate::note_held_read(layer);
    let field = bw.field.as_deref().and_then(|f| layer.dataset.field_index(f));
    let cut = layer_cut(layer, b, true, env);
    // Styled at the view centre's drawing scale, as the water layer is drawn on the surface.
    let styler = layer.styler_with(env.at_scale(Some(scale)));
    let x = ground.exaggeration();
    let ramp = f64::from(crate::terrain::SHORE_PX) * res;
    // Each water body's edges within the ramp of the box (a round block has many short sides,
    // each crossing few of them).
    let mut near_box: std::collections::HashMap<usize, Vec<(Coord, Coord)>> = std::collections::HashMap::new();
    // The layer's conversion to longitude/latitude, as the cut's parts were read (`units`).
    let to = layer.dataset.shown_conversion().ok();
    for (i, runs) in out.iter_mut().enumerate() {
        let (side, _, _) = b.side(i);
        let Some(units) = cut.units.get(i) else { continue };
        for &(lo, hi, part) in units {
            let Some(p) = part else { continue };
            let Some(&(row, k)) = cut.parts.get(p) else { continue };
            let given = field.and_then(|f| layer.dataset.features.value(row, f).as_f64()).filter(|d| d.is_finite() && *d > 0.0);
            let Some(depth) = given.or(bw.depth).filter(|d| *d > 0.0) else { continue };
            let mid = b.point(side, (lo + hi) / 2.0);
            let Some(level) = water.level(mid) else { continue };
            // Its topmost solid fill, as the walls' units take theirs, laid over a light blue by its
            // opacity: a section is opaque.
            let (sym, _, fills) = styler.style_and_fills(row);
            let color = unit_paint(sym.fill, fills, SECTION_WATER).0;
            let edges = near_box.entry(p).or_insert_with(|| {
                // Tested in longitude and latitude (Web Mercator keeps each axis in order, so an
                // edge's box meets the box's the same), only those near it put in Web Mercator.
                let (lo, hi) = (merc_to_lonlat(Coord::new(b.x0 - ramp, b.y0 - ramp)), merc_to_lonlat(Coord::new(b.x1 + ramp, b.y1 + ramp)));
                let lonlat = layer
                    .dataset
                    .features
                    .geometry(row)
                    .zip(to.as_ref())
                    .and_then(|(g, to)| rustgis_data::Dataset::converted_to_lonlat(to, g));
                let Some(Geometry::Polygon(polys)) = lonlat.as_deref() else { return Vec::new() };
                polys
                    .get(k)
                    .into_iter()
                    .flatten()
                    .flat_map(|r| r.iter().zip(r.iter().cycle().skip(1)).map(|(a, q)| (*a, *q)))
                    .filter(|(a, q)| a.x.max(q.x) >= lo.x && a.x.min(q.x) <= hi.x && a.y.max(q.y) >= lo.y && a.y.min(q.y) <= hi.y)
                    .map(|(a, q)| (lonlat_to_merc(a), lonlat_to_merc(q)))
                    .collect()
            });
            // The shore's edges that can lie within the ramp of the stretch.
            let shore = edges
                .iter()
                .copied()
                .filter(|&(a, q)| {
                    let (da, dq) = (b.beyond(side, a), b.beyond(side, q));
                    let (sa, sq) = (b.along(side, a), b.along(side, q));
                    (da.abs() <= ramp || dq.abs() <= ramp || (da > 0.0) != (dq > 0.0)) && sa.max(sq) >= lo - ramp && sa.min(sq) <= hi + ramp
                })
                .collect();
            let (level, depth) = (level * x, depth * x);
            runs.push(WaterRun { lo, hi, color, level, depth, ramp, shore, dark: tw.shore });
        }
    }
    out
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

/// How much to stretch `pattern` across a wall `around` (logical px) long that closes on itself,
/// so it holds a whole number of the pattern's periods across and meets itself where it closes:
/// under 1% on a wall a hundred periods round.
fn closing(pattern: FillPattern, around: f64) -> f64 {
    let period = match pattern {
        // Lines repeat across every spacing / sin(angle); level ones don't change across.
        FillPattern::Hatch { angle, spacing, .. } => {
            let s = f64::from(angle).to_radians().sin().abs();
            if s < 1e-6 { 0.0 } else { f64::from(spacing) / s }
        }
        // Odd columns are shifted down, so the grid repeats every two columns.
        FillPattern::Dots { spacing, displacement, .. } => f64::from(spacing[0]) * if displacement[1] == 0.0 { 1.0 } else { 2.0 },
        _ => 0.0,
    };
    if !(period > 0.0 && period.is_finite() && around > 0.0 && around.is_finite()) {
        return 1.0;
    }
    (around / period).round().max(1.0) * period / around
}

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
/// (a box and a circle are convex, so no ground stands in front of them), each stretch of
/// `edges` from the ground down to the base in the colours `runs` gives along it ([`runs`]) with
/// their patterns
/// over them ([`draw_patterns`]), lit by the way its side faces the light as extruded walls are
/// and darker toward its foot, fogged with distance as the ground is (into `fog` toward the view's
/// reach `far`), and outlined along the ground, the base, the box's corners (a round block's
/// where its walls turn away, a polygon block's there and where its outline turns sharply) and
/// the contacts between colours. The water the outline runs through (`water`, [`water_runs`]) is
/// drawn over them from its level down to its floor, in its colour lit as the side is and darker
/// toward the full depth (half as bright there), with a line at the level and one along the floor.
///
/// A polygon block's outline can be concave: ground and other walls can stand in front of a
/// wall. Its walls are drawn among the ground instead, far to near, a few stretches of a side at
/// a time ([`WALL_RUN`]): before each run, `ground_to` draws the ground's triangles at least as
/// far as the nearest of those its stretches came from (the ground behind it), and the caller
/// the rest after. It says whether to go on: a frame cancelled meanwhile draws no more walls.
#[expect(clippy::too_many_arguments, reason = "the walls are drawn with what the frame drew the ground with")]
pub(crate) fn draw_walls(
    pm: &mut Pixmap,
    view: &View,
    ground: &Ground,
    b: &Block,
    edges: &[Edge],
    runs: &Runs,
    water: &WaterRuns,
    fog: Rgba,
    far: f64,
    mut ground_to: impl FnMut(&mut Pixmap, f64) -> bool,
) {
    let fog_at = |depth: f64| crate::fog_share(depth, far);
    let k = f64::from(pm.width()) / view.width.max(1e-9);
    let cam = view.camera;
    let (w, h) = (view.width, view.height);
    let (eu, ev, _) = cam.eye(h);
    let centre = cam.ground_px(Coord::new((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0));
    // Each side's outward normal on the ground (away from the outline's centre; a polygon's to
    // the right of its side, its rings anticlockwise), where it faces the eye.
    let faces: Vec<Option<(f64, f64)>> = (0..b.sides())
        .map(|i| {
            let (_, a, z) = b.side(i);
            let (ga, gz) = (cam.ground_px(a), cam.ground_px(z));
            let (du, dv) = (gz.0 - ga.0, gz.1 - ga.1);
            let len = du.hypot(dv);
            if len <= 0.0 {
                return None;
            }
            let mid = ((ga.0 + gz.0) / 2.0, (ga.1 + gz.1) / 2.0);
            let mut n = (dv / len, -du / len);
            let inward = if b.outline.is_some() {
                let o = cam.ground_px(Coord::new((a.x + z.x) / 2.0 + (z.y - a.y), (a.y + z.y) / 2.0 - (z.x - a.x)));
                n.0 * (o.0 - mid.0) + n.1 * (o.1 - mid.1) < 0.0
            } else {
                n.0 * (centre.0 - mid.0) + n.1 * (centre.1 - mid.1) > 0.0
            };
            if inward {
                n = (-n.0, -n.1);
            }
            (n.0 * (eu - mid.0) + n.1 * (ev - mid.1) > 0.0).then_some(n)
        })
        .collect();
    let round = b.circle.is_some();
    // A round or polygon block's walls run on round a closed outline.
    let closed = round || b.outline.is_some();
    // The stretches by side, in the order they came.
    let mut by_side: Vec<&Edge> = edges.iter().collect();
    by_side.sort_by_key(|e| e.side);
    let of_side = |side: u16| {
        let (lo, hi) = (by_side.partition_point(|e| e.side < side), by_side.partition_point(|e| e.side <= side));
        by_side.get(lo..hi).unwrap_or_default()
    };
    // The sides and their stretches in drawing order: a box's and a circle's sides whole, in
    // turn; a polygon block's facing the eye in runs of [`WALL_RUN`] stretches along them (a long
    // side reaches from far to near), far to near, each as far as the nearest triangle of ground
    // its stretches came from.
    let walk: Vec<(usize, f64, Vec<&Edge>)> = if b.outline.is_some() {
        let mut walk = Vec::new();
        for s in (0..faces.len()).filter(|s| faces[*s].is_some()) {
            let key = b.side(s).0;
            let mut stretches = of_side(key).to_vec();
            stretches.sort_by(|p, q| b.along(key, p.ends[0]).total_cmp(&b.along(key, q.ends[0])));
            for run in stretches.chunks(WALL_RUN) {
                walk.push((s, run.iter().map(|e| e.depth).fold(f64::INFINITY, f64::min), run.to_vec()));
            }
        }
        walk.sort_by(|a, b| b.1.total_cmp(&a.1));
        walk
    } else {
        (0..faces.len()).map(|s| (s, f64::INFINITY, of_side(b.side(s).0).to_vec())).collect()
    };
    // How sharply the outline turns from side `p` to side `q` (the cosine of the turn).
    let turn = |p: usize, q: usize| {
        let ((_, a, z), (_, c, d)) = (b.side(p), b.side(q));
        let (u, v) = ((z.x - a.x, z.y - a.y), (d.x - c.x, d.y - c.y));
        (u.0 * v.0 + u.1 * v.1) / (u.0.hypot(u.1) * v.0.hypot(v.1)).max(1e-300)
    };
    // A round block's outlines, stroked once all its sides are filled: a side's fill, a hair
    // longer each way, would nick its neighbour's.
    let mut outlines: Vec<(tiny_skia::Path, Rgba)> = Vec::new();
    // The water in the walls' section, drawn over their outlines ([`draw_water`]).
    let mut waters: Vec<WaterPaint> = Vec::new();
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
    // The part of it the last run filled, cleared before the next: a round block has hundreds of sides.
    let mut dirty: Option<tiny_skia::Rect> = None;
    for (s, key, stretches) in &walk {
        let (s, key) = (*s, *key);
        let Some(n) = faces[s] else { continue };
        if b.outline.is_some() && !ground_to(pm, key) {
            return;
        }
        let (side, a, z) = b.side(s);
        // A box's corners go straight down; a round block's only where its walls turn away, a
        // polygon block's there and where its outline turns by over 45°.
        let (before, after) = b.neighbours(s);
        let down = |o: usize| faces[o].is_none() || (b.outline.is_some() && turn(o, s) < std::f64::consts::FRAC_1_SQRT_2);
        let (down_a, down_z) = if closed { (down(before), down(after)) } else { (true, true) };
        let lit = ((n.0 * LIGHT.0 + n.1 * LIGHT.1) / light_len).max(0.0);
        let (top_shade, foot_shade) = (0.6 + 0.4 * lit, (0.6 + 0.4 * lit) * 0.72);
        let near = |p: Coord, q: Coord| (p.x - q.x).abs() + (p.y - q.y).abs() <= 1e-9 * (b.x1 - b.x0 + b.y1 - b.y0);
        // Each stretch's ends (Web Mercator), feet and tops (physical px), and the outline along
        // the ground, the base and the box's corners.
        let mut quads: Vec<([Coord; 2], [(f64, f64); 2], [(f64, f64); 2])> = Vec::new();
        let mut outline = PathBuilder::new();
        let (mut foot_mid, mut top_mid, mut count, mut nearest) = ((0.0, 0.0), (0.0, 0.0), 0.0, f64::INFINITY);
        for e in stretches {
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
                if (down_a && near(*end, a)) || (down_z && near(*end, z)) {
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
        // A round block's sides meet at a slant in different shades: a whole pixel over its
        // neighbour, so the anti-aliased edges between them leave no light seam.
        let reach = if closed { 1.0 } else { 0.4 };
        let grow = |p: (f64, f64), q: (f64, f64)| {
            let (dx, dy) = (p.0 - q.0, p.1 - q.1);
            let l = dx.hypot(dy).max(1e-6);
            pt((p.0 + dx / l * reach * k, p.1 + dy / l * reach * k))
        };
        let lerp = |p: (f64, f64), q: (f64, f64), t: f64| (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t);
        // The side's plane for its patterns ([`draw_patterns`]): across in logical px at the
        // centre's scale, left to right seen from outside (the sides run anticlockwise round the
        // box), and up as `Ground::up_px` gives it; from a little under the base to a little over
        // the highest ground along it.
        // (A round or polygon block's sides run on round it, anticlockwise: left to right too.)
        let dir = if closed || side & u16::from(SOUTH | EAST) != 0 { 1.0 } else { -1.0 };
        let on_side = |t: f64| b.point(side, t);
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
        let mid = Coord::new((a.x + z.x) / 2.0, (a.y + z.y) / 2.0);
        let (mid_x, mid_up) = (b.along(side, mid) / res * dir, (up_range.0 + up_range.1) / 2.0);
        let span = |p: Option<(f64, f64)>, q: Option<(f64, f64)>| p.zip(q).map(|(p, q)| (q.0 - p.0).hypot(q.1 - p.1));
        let scale = match (span(at(mid_x, mid_up), at(mid_x + 1.0, mid_up)), span(at(mid_x, mid_up), at(mid_x, mid_up + 1.0))) {
            (Some(x), Some(y)) if (x * y).sqrt().is_finite() => (x * y).sqrt(),
            _ => k,
        };
        let shade = (top_shade + foot_shade) / 2.0;
        let tone = |c: Rgba| c.shade(shade).lerp(fog.with_alpha(c.a()), f);
        // A round or polygon block's contact at the corner it shares with the side before, when
        // the colours either side of it differ.
        let before = runs.get(before).and_then(|r| r.last());
        let Some(runs) = runs.get(s) else { continue };
        let corner = runs.first().filter(|r| closed && before.is_some_and(|p| p.color != r.color || p.patterns != r.patterns));
        for run in runs {
            // The side in one path a colour, every stretch (cut to the colour's) a quad a hair
            // longer each way along the edge: filled at once, the overlaps leave no seam, where
            // stretches filled one by one striped the wall with their anti-aliased edges.
            let mut wall = PathBuilder::new();
            for (ends, [fa, fz], [ta, tz]) in &quads {
                let (ca, cz) = (b.along(side, ends[0]), b.along(side, ends[1]));
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
                if let Some(r) = dirty.take() {
                    clear_mask(cut, r);
                }
                cut.fill_path(&path, FillRule::Winding, true, Transform::identity());
                dirty = Some(path.bounds());
                let (x0, x1) = (run.lo / res * dir, run.hi / res * dir);
                // A round block's wall closes on itself at the east point, where `along` goes
                // back to 0 (a polygon block's at its ring's first corner): each pattern is
                // stretched across so it runs on over that seam.
                let around = b.around(side).map(|c| c / res);
                for p in &run.patterns {
                    let f = around.map_or(1.0, |c| closing(p.0, c));
                    let span = ((x0.min(x1) * f, x0.max(x1) * f), up_range);
                    draw_patterns(pm, cut, std::slice::from_ref(p), span, scale, tone, |x, up| at(x / f, up));
                }
            }
        }
        // The contacts between colours, straight down.
        for c in runs.windows(2).map(|w| w[0].hi).chain(corner.map(|r| r.lo)) {
            let at = quads.iter().find_map(|(ends, [fa, fz], [ta, tz])| {
                let (ca, cz) = (b.along(side, ends[0]), b.along(side, ends[1]));
                // (A round block's corner contact is where its first stretch begins, near enough.)
                let start = closed && (ca - c).abs() <= 1e-9 * (b.x1 - b.x0);
                let inside = (cz - ca).abs() >= 1e-12 && ((ca - c) * (cz - c) <= 0.0 || start);
                inside.then(|| ((c - ca) / (cz - ca), *ta, *tz, *fa, *fz))
            });
            if let Some((t, ta, tz, fa, fz)) = at {
                let (top, ft) = (pt(lerp(ta, tz, t)), pt(lerp(fa, fz, t)));
                outline.move_to(top.0, top.1);
                outline.line_to(ft.0, ft.1);
            }
        }
        if let Some(path) = outline.finish() {
            outlines.push((path, b.walls.shade(0.45).lerp(fog, f)));
            if !round {
                stroke_outlines(pm, &mut outlines, k);
            }
        }
        // The water the side cuts through (#2317), over the wall and its contacts: from its level
        // down to the floor, sampled along each stretch so the floor follows the shore.
        for wr in water.get(s).into_iter().flatten() {
            // Along the run: each point's place along the side, its level and floor on screen,
            // and the floor at the full depth there, for the shading.
            type Sample = (f64, (f64, f64), (f64, f64), (f64, f64));
            let mut samples: Vec<Sample> = Vec::new();
            for (ends, _, [ta, tz]) in &quads {
                let (ca, cz) = (b.along(side, ends[0]), b.along(side, ends[1]));
                let span = cz - ca;
                if span.abs() < 1e-12 {
                    continue;
                }
                let (u, v) = ((wr.lo - ca) / span, (wr.hi - ca) / span);
                let (t0, t1) = (u.min(v).max(0.0), u.max(v).min(1.0));
                for i in (t1 > t0).then_some(0..=WATER_STEPS).into_iter().flatten() {
                    let t = t0 + (t1 - t0) * f64::from(i) / f64::from(WATER_STEPS);
                    let m = Coord::new(ends[0].x + (ends[1].x - ends[0].x) * t, ends[0].y + (ends[1].y - ends[0].y) * t);
                    let (gu, gv) = cam.ground_px(m);
                    let raised = |height: f64| {
                        let up = (height.max(b.base) / ground.per_unit(m) / res).clamp(-1e7, 1e7);
                        cam.project_raised(gu, gv, up, w, h).map(|(x, y)| (x * k, y * k)).filter(|p| p.0.is_finite() && p.1.is_finite())
                    };
                    if let (Some(floor), Some(deep)) = (raised(wr.floor(m)), raised(wr.level - wr.depth)) {
                        // The water's level, or the ground's edge where that runs lower: the mesh
                        // draws a stretch reaching past the shore partly at the land's heights
                        // (up is up the screen in a tilted view).
                        let edge = lerp(*ta, *tz, t);
                        let top = raised(wr.level).filter(|l| l.1 > edge.1).unwrap_or(edge);
                        samples.push((b.along(side, m), top, floor, deep));
                    }
                }
            }
            samples.sort_by(|p, q| p.0.total_cmp(&q.0));
            samples.dedup_by(|p, q| (p.0 - q.0).abs() < 1e-9);
            // The wall's stretches within the run, joined where they meet or overlap: a band of
            // ground that failed to draw (`render_tilted` goes on without it) leaves the wall in
            // pieces, and the water keeps to each, crossing no gap where no wall stands.
            let tol = 1e-6 * (b.x1 - b.x0).abs().max(1.0);
            let (rlo, rhi) = (wr.lo.min(wr.hi), wr.lo.max(wr.hi));
            let mut spans: Vec<(f64, f64)> = quads
                .iter()
                .filter_map(|(ends, _, _)| {
                    let (ca, cz) = (b.along(side, ends[0]), b.along(side, ends[1]));
                    let (lo, hi) = (ca.min(cz).max(rlo), ca.max(cz).min(rhi));
                    (hi > lo).then_some((lo, hi))
                })
                .collect();
            spans.sort_by(|p, q| p.0.total_cmp(&q.0));
            let mut pieces: Vec<(f64, f64)> = Vec::new();
            for (lo, hi) in spans {
                match pieces.last_mut() {
                    Some(last) if lo <= last.1 + tol => last.1 = last.1.max(hi),
                    _ => pieces.push((lo, hi)),
                }
            }
            for (lo, hi) in pieces {
                let mut samples: Vec<Sample> = samples.iter().filter(|p| p.0 >= lo - tol && p.0 <= hi + tol).copied().collect();
                if samples.len() < 2 {
                    continue;
                }
                // A round block's water runs on over hundreds of sides: each band a hair longer each
                // way, as its walls are, so no light seam shows between them.
                if round {
                    let n = samples.len();
                    let (first, second, last, before) = (samples[0], samples[1], samples[n - 1], samples[n - 2]);
                    let out = |p: (f64, f64), q: (f64, f64)| {
                        let g = grow(p, q);
                        (f64::from(g.0), f64::from(g.1))
                    };
                    samples[0] = (first.0, out(first.1, second.1), out(first.2, second.2), first.3);
                    samples[n - 1] = (last.0, out(last.1, before.1), out(last.2, before.2), last.3);
                }
                let (mut band, mut level, mut floor) = (PathBuilder::new(), PathBuilder::new(), PathBuilder::new());
                for (i, (_, top, bed, _)) in samples.iter().enumerate() {
                    let (top, bed) = (pt(*top), pt(*bed));
                    if i == 0 {
                        band.move_to(top.0, top.1);
                        level.move_to(top.0, top.1);
                        floor.move_to(bed.0, bed.1);
                    } else {
                        band.line_to(top.0, top.1);
                        level.line_to(top.0, top.1);
                        floor.line_to(bed.0, bed.1);
                    }
                }
                for (_, _, bed, _) in samples.iter().rev() {
                    band.line_to(pt(*bed).0, pt(*bed).1);
                }
                band.close();
                let n = samples.len() as f64;
                let mean = |pick: fn(&Sample) -> (f64, f64)| {
                    let s = samples.iter().map(pick).fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
                    Point::from_xy((s.0 / n) as f32, (s.1 / n) as f32)
                };
                let (top, deep) = (wr.color.shade(top_shade).lerp(fog, f), wr.color.shade(top_shade * 0.5).lerp(fog, f));
                let shader = tiny_skia::LinearGradient::new(
                    mean(|p| p.1),
                    mean(|p| p.3),
                    vec![GradientStop::new(0.0, color(top)), GradientStop::new(1.0, color(deep))],
                    SpreadMode::Pad,
                    Transform::identity(),
                );
                let paint_water = match shader {
                    Some(shader) => Paint { shader, anti_alias: true, ..Paint::default() },
                    None => paint(top, 1.0),
                };
                // A line at the level, and one along the floor as dark as the shore band at its outline.
                let bed = Rgba::rgb(0, 0, 0).lerp(fog, f).with_alpha((wr.dark.clamp(0.0, 1.0) * 255.0).round() as u8);
                let lines = [(floor, bed), (level, wr.color.shade(0.6).lerp(fog, f))]
                    .into_iter()
                    .filter_map(|(line, c)| line.finish().filter(|_| c.a() > 0).map(|p| (p, c)))
                    .collect();
                waters.push((band.finish(), paint_water, lines));
            }
        }
        // A box's water over its side's outline at once; a round block's once all are stroked.
        if !round {
            draw_water(pm, &mut waters, k);
        }
    }
    stroke_outlines(pm, &mut outlines, k);
    draw_water(pm, &mut waters, k);
}

/// The points each stretch of wall is cut into for the water's floor (#2317): enough for the
/// ramp at the shore, which spans a few pixels, inside a stretch of the settled mesh.
const WATER_STEPS: u32 = 4;

/// The water in a wall's section, ready to draw: its band and paint, and its lines (at the level
/// and along the floor) in their colours.
type WaterPaint = (Option<tiny_skia::Path>, Paint<'static>, Vec<(tiny_skia::Path, Rgba)>);

/// Draws the walls' `waters` at the scale `k` (physical px a logical one), and empties them.
fn draw_water(pm: &mut Pixmap, waters: &mut Vec<WaterPaint>, k: f64) {
    let stroke = Stroke { width: k as f32, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() };
    for (band, paint_water, lines) in waters.drain(..) {
        if let Some(path) = band {
            pm.fill_path(&path, &paint_water, FillRule::Winding, Transform::identity(), None);
        }
        for (line, c) in lines {
            pm.stroke_path(&line, &paint(c, 1.0), &stroke, Transform::identity(), None);
        }
    }
}

/// How many stretches of a polygon block's wall side are drawn at once ([`draw_walls`]): about as
/// many triangles of ground along the outline, near enough in depth for one place in the far to
/// near order.
const WALL_RUN: usize = 8;

/// Clears `mask` over the box `r` (physical px), a pixel round it for the anti-aliased edge.
fn clear_mask(mask: &mut Mask, r: tiny_skia::Rect) {
    let (w, h) = (mask.width() as usize, mask.height() as usize);
    let span = |lo: f32, hi: f32, n: usize| ((lo.floor() - 1.0).max(0.0) as usize, ((hi.ceil() + 1.0).max(0.0) as usize).min(n));
    let ((x0, x1), (y0, y1)) = (span(r.left(), r.right(), w), span(r.top(), r.bottom(), h));
    let data = mask.data_mut();
    for y in y0..y1 {
        if let Some(row) = data.get_mut(y * w + x0..y * w + x1.max(x0)) {
            row.fill(0);
        }
    }
}

/// Strokes the walls' `outlines` (each in its colour) at the scale `k` (physical px a logical
/// one), and empties them.
fn stroke_outlines(pm: &mut Pixmap, outlines: &mut Vec<(tiny_skia::Path, Rgba)>, k: f64) {
    let stroke = Stroke { width: (0.7 * k) as f32, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Stroke::default() };
    for (path, edge) in outlines.drain(..) {
        pm.stroke_path(&path, &paint(edge, 1.0), &stroke, Transform::identity(), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_block() -> Block {
        Block {
            x0: 0.0,
            y0: 0.0,
            x1: 10.0,
            y1: 10.0,
            base: -100.0,
            settled: true,
            walls: Rgba::rgb(200, 184, 154),
            circle: None,
            outline: None,
        }
    }

    fn round_block() -> Block {
        Block { circle: Some((Coord::new(0.0, 0.0), 10.0)), x0: -10.0, y0: -10.0, x1: 10.0, y1: 10.0, ..unit_block() }
    }

    /// A block cut to the polygon of these corners (Web Mercator), its box the one round it.
    fn polygon_block(ring: &[(f64, f64)]) -> Block {
        let o = Outline::from_rings(vec![ring.iter().map(|&(x, y)| Coord::new(x, y)).collect()]).expect("an outline");
        let [x0, y0, x1, y1] = o.extent();
        Block { outline: Some(Arc::new(o)), x0, y0, x1, y1, ..unit_block() }
    }

    /// The area of the parts of a clipped triangle.
    fn area(parts: &[Vec<Clipped>]) -> f64 {
        let ring = |p: &Vec<Clipped>| p.iter().zip(p.iter().cycle().skip(1)).map(|(a, b)| a.1.x * b.1.y - b.1.x * a.1.y).sum::<f64>() / 2.0;
        parts.iter().map(ring).sum()
    }

    #[test]
    fn a_polygon_block_cuts_a_triangle_across_its_inner_corner_to_the_part_inside() {
        // #2318: an L, the square 10 a side without its north-east quarter (given clockwise: it is
        // turned round). A triangle across the inner corner keeps all but the corner's notch,
        // in one concave part whose edges along the outline name its sides; one in the notch is
        // left out; one inside, whole.
        let b = polygon_block(&[(0.0, 0.0), (0.0, 10.0), (5.0, 10.0), (5.0, 5.0), (10.0, 5.0), (10.0, 0.0)]);
        assert!(outcode(&b, Coord::new(2.0, 2.0)) == 0 && outcode(&b, Coord::new(7.0, 7.0)) & ROUND_OFF != 0);
        let across = [Coord::new(4.0, 4.0), Coord::new(8.0, 4.0), Coord::new(4.0, 8.0)];
        let parts = pieces(across, &b);
        assert_eq!(parts.len(), 1);
        assert!((area(&parts) - 6.0).abs() < 1e-9, "{}", area(&parts));
        // Its weights put each corner where it is.
        for (w, c, _) in &parts[0] {
            let at = (0..3).fold((0.0, 0.0), |p, i| (p.0 + w[i] * across[i].x, p.1 + w[i] * across[i].y));
            assert!((at.0 - c.x).abs() < 1e-9 && (at.1 - c.y).abs() < 1e-9);
        }
        let along: Vec<Coord> = parts[0].iter().filter(|c| c.2 != 0).map(|c| b.side(c.2 as usize - 1).1).collect();
        assert!(along.contains(&Coord::new(10.0, 5.0)) && along.contains(&Coord::new(5.0, 5.0)), "{along:?}");
        let mut out = Vec::new();
        edges(&b, &parts[0], &parts[0].iter().map(|c| (c.1.x, c.1.y)).collect::<Vec<_>>(), 1.0, &mut out);
        assert_eq!(out.len(), 2);
        assert!(pieces([Coord::new(6.0, 6.0), Coord::new(9.0, 6.0), Coord::new(6.0, 9.0)], &b).is_empty());
        let inside = pieces([Coord::new(1.0, 1.0), Coord::new(3.0, 1.0), Coord::new(1.0, 3.0)], &b);
        assert!(inside.len() == 1 && (area(&inside) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_polygon_blocks_ring_holding_a_triangle_keeps_it_whole_and_one_touching_it_none() {
        // A triangle touching the square's south side from outside at one point is left out.
        let square = polygon_block(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
        assert!(pieces([Coord::new(5.0, 0.0), Coord::new(4.0, -2.0), Coord::new(6.0, -2.0)], &square).is_empty());
        // The square holds a triangle that a strip crosses: the whole triangle is kept, beside the
        // strip's part, which gives its walls their stretches.
        let o = Outline::from_rings(vec![
            vec![Coord::new(0.0, 0.0), Coord::new(10.0, 0.0), Coord::new(10.0, 10.0), Coord::new(0.0, 10.0)],
            vec![Coord::new(3.0, 0.0), Coord::new(5.0, 0.0), Coord::new(5.0, 10.0), Coord::new(3.0, 10.0)],
        ])
        .expect("an outline");
        let b = Block { outline: Some(Arc::new(o)), x0: 0.0, y0: 0.0, x1: 10.0, y1: 10.0, ..unit_block() };
        let parts = pieces([Coord::new(2.0, 2.0), Coord::new(8.0, 2.0), Coord::new(2.0, 8.0)], &b);
        assert!(parts.iter().any(|p| p.len() == 3 && p.iter().all(|c| c.2 == 0) && (area(std::slice::from_ref(p)) - 18.0).abs() < 1e-9));
        assert!(parts.iter().any(|p| p.iter().any(|c| c.2 != 0)));
        // Two 10 m squares 100 km apart both stay, each with its four sides.
        let small = |x: f64| vec![Coord::new(x, 0.0), Coord::new(x + 10.0, 0.0), Coord::new(x + 10.0, 10.0), Coord::new(x, 10.0)];
        assert_eq!(Outline::from_rings(vec![small(0.0), small(100_000.0)]).map(|o| o.side_count()), Some(8));
        // 7000 decagons 10 m across, too many sides even at their own eighth: the smallest are left
        // out, and the spacing said is the one they were thinned at, not the gap doubled past it.
        let decagon = |i: usize| -> Vec<Coord> {
            let (cx, cy) = ((i % 84) as f64 * 100.0, (i / 84) as f64 * 100.0);
            (0..10)
                .map(|k| (k as f64 * std::f64::consts::TAU / 10.0).sin_cos())
                .map(|(s, c)| Coord::new(cx + 5.0 * c, cy + 5.0 * s))
                .collect()
        };
        let many = Outline::from_rings((0..7000).map(decagon).collect()).expect("an outline");
        assert!(many.dropped() > 0 && many.side_count() <= 65_535, "{} {}", many.dropped(), many.side_count());
        assert!(many.spacing().0 <= 10.0 / 8.0 + 1e-9, "{:?}", many.spacing());
        // A ring that doesn't fit is left out, and smaller ones that still fit are kept: 6553
        // decagons, one smaller that doesn't fit, then ten triangles, of which one fits.
        let gon = |i: usize, n: usize, r: f64| -> Vec<Coord> {
            let (cx, cy) = ((i % 100) as f64 * 100.0, (i / 100) as f64 * 100.0);
            (0..n)
                .map(|k| (k as f64 * std::f64::consts::TAU / n as f64).sin_cos())
                .map(|(s, c)| Coord::new(cx + r * c, cy + r * s))
                .collect()
        };
        let rings: Vec<Vec<Coord>> =
            (0..6553).map(|i| gon(i, 10, 5.0)).chain([gon(6553, 10, 4.0)]).chain((6554..6564).map(|i| gon(i, 3, 2.0))).collect();
        let fit = Outline::from_rings(rings).expect("an outline");
        assert_eq!((fit.side_count(), fit.dropped()), (65_533, 10));
        // The box is the kept rings': a far ring without an area, or one left out, doesn't widen it.
        let flat = vec![Coord::new(500_000.0, 0.0), Coord::new(500_010.0, 0.0), Coord::new(500_020.0, 0.0)];
        let one = Outline::from_rings(vec![small(0.0), flat]).expect("an outline");
        assert_eq!(one.extent(), [0.0, 0.0, 10.0, 10.0]);
        let near = || (0..6553).map(|i| gon(i, 10, 5.0));
        let kept = Outline::from_rings(near().chain([gon(500_000, 10, 4.0)]).collect()).expect("an outline");
        let alone = Outline::from_rings(near().collect()).expect("an outline");
        assert_eq!((kept.dropped(), kept.extent()), (1, alone.extent()));
    }

    #[test]
    fn a_polygon_blocks_spacing_and_count_leave_out_rings_without_an_area() {
        // A far ring without an area doesn't widen the spacing a 5 km square is thinned at.
        let square = |x: f64, w: f64| vec![Coord::new(x, 0.0), Coord::new(x + w, 0.0), Coord::new(x + w, w), Coord::new(x, w)];
        let flat = vec![Coord::new(5.0e6, 0.0), Coord::new(5.0e6 + 10.0, 0.0), Coord::new(5.0e6 + 20.0, 0.0)];
        let alone = Outline::from_rings(vec![square(0.0, 5000.0)]).expect("an outline");
        let with = Outline::from_rings(vec![square(0.0, 5000.0), flat]).expect("an outline");
        assert_eq!(with.spacing(), alone.spacing());
        assert_eq!(alone.narrow(), 0);
        // A 1000 m by 5 m strip beside a square 100 km away: thinned at 50 m it has no area left,
        // and is counted as left out.
        let strip = vec![Coord::new(0.0, 0.0), Coord::new(1000.0, 0.0), Coord::new(1000.0, 5.0), Coord::new(0.0, 5.0)];
        let o = Outline::from_rings(vec![strip, square(100_000.0, 1000.0)]).expect("an outline");
        assert_eq!((o.side_count(), o.narrow(), o.dropped()), (4, 1, 0));
    }

    #[test]
    fn a_polygon_block_cuts_a_triangle_across_a_narrow_bay_in_two() {
        // A U, its bay 2 wide from y = 2 up: a triangle over the bay's mouth, reaching past the
        // U's top, keeps the two arms' parts below the top, apart.
        let b = polygon_block(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (6.0, 10.0), (6.0, 2.0), (4.0, 2.0), (4.0, 10.0), (0.0, 10.0)]);
        let parts = pieces([Coord::new(1.0, 8.0), Coord::new(9.0, 8.0), Coord::new(5.0, 12.0)], &b);
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|p| (area(std::slice::from_ref(p)) - 4.0).abs() < 1e-9), "{}", area(&parts));
        // Along its sides as round a circle: from its first corner, on round.
        let (key, a, z) = b.side(3);
        assert_eq!((a, z), (Coord::new(6.0, 10.0), Coord::new(6.0, 2.0)));
        assert!((b.along(key, Coord::new(6.0, 6.0)) - 28.0).abs() < 1e-9);
        assert_eq!(b.point(key, 28.0), Coord::new(6.0, 6.0));
        assert!(b.beyond(key, Coord::new(5.0, 6.0)) > 0.0 && b.beyond(key, Coord::new(7.0, 6.0)) < 0.0);
        assert_eq!((b.neighbours(0), b.neighbours(7)), ((7, 1), (6, 0)));
        assert_eq!(b.around(key), Some(56.0));
    }

    #[test]
    fn a_round_block_keeps_a_triangle_inside_its_circle_and_drops_one_past_a_tangent() {
        // #2318: inside the circle, whole; past the north-east tangent (outside the circle's box
        // on no side), left out by its code; between the circle and its box, cut to nothing.
        let b = round_block();
        let inside = [Coord::new(0.0, 0.0), Coord::new(5.0, 0.0), Coord::new(0.0, 5.0)];
        assert!(inside.iter().all(|m| outcode(&b, *m) == 0));
        assert_eq!(clip(inside, &b).len(), 3);
        let beyond = [Coord::new(8.0, 8.0), Coord::new(9.5, 8.0), Coord::new(8.0, 9.5)];
        assert_ne!(beyond.iter().map(|m| outcode(&b, *m)).fold(!0, |a, c| a & c) & !ROUND_OFF, 0);
        let gap = [Coord::new(9.9, 2.0), Coord::new(9.95, 2.5), Coord::new(9.9, 3.0)];
        assert!(gap.iter().all(|m| outcode(&b, *m) & ROUND_OFF != 0));
        assert!(clip(gap, &b).is_empty());
    }

    #[test]
    fn a_triangle_across_a_round_blocks_edge_gives_its_wall_stretches_along_the_circle() {
        // A triangle over the circle's east point: cut on it, the stretches of wall under it run
        // northward, end to end, on the circle and along the sides round its first corner.
        let b = round_block();
        let poly = clip([Coord::new(8.0, -2.0), Coord::new(12.0, 0.0), Coord::new(8.0, 2.0)], &b);
        assert!(poly.iter().all(|c| c.1.x.hypot(c.1.y) <= 10.0 + 1e-9), "{poly:?}");
        let screen: Vec<(f64, f64)> = poly.iter().map(|c| (c.1.x, c.1.y)).collect();
        let mut out = Vec::new();
        edges(&b, &poly, &screen, 1.0, &mut out);
        assert!(out.len() >= 4, "{}", out.len());
        out.sort_by(|p, q| p.ends[0].y.total_cmp(&q.ends[0].y));
        let sag = 10.0 * (1.0 - (std::f64::consts::PI / ROUND_SIDES as f64).cos()) + 1e-9;
        for e in &out {
            assert!(e.ends[1].y > e.ends[0].y, "northward");
            assert!(e.ends.iter().all(|m| (m.x.hypot(m.y) - 10.0).abs() <= sag), "on the circle");
            let k = usize::from(e.side);
            assert!(!(32..=ROUND_SIDES - 32).contains(&k), "near east: {k}");
            assert!(e.ends.iter().all(|m| b.beyond(e.side, *m).abs() < 1e-9), "on its side's line");
        }
        for w in out.windows(2) {
            assert!((w[0].ends[1].y - w[1].ends[0].y).abs() < 1e-9, "end to end");
        }
        // Along the circle, a side's end is where the next one begins.
        let (key, _, z) = b.side(3);
        assert!((b.along(key, z) - b.along(key + 1, z)).abs() < 1e-9);
        assert!((b.point(key, b.along(key, z)).x - z.x).abs() < 1e-9);
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
        edges(&b, &poly, &screen, 1.0, &mut out);
        assert_eq!(out.len(), 1);
        let e = &out[0];
        assert_eq!(e.side, u16::from(EAST));
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
        edges(&b, &poly, &screen, 1.0, &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].side, u16::from(EAST));
        assert!((out[0].ends[0].y - 2.0).abs() < 1e-9 && (out[0].ends[1].y - 6.0).abs() < 1e-9);
    }

    #[test]
    fn a_triangle_over_a_corner_gives_a_stretch_on_each_side() {
        let b = unit_block();
        let poly = clip([Coord::new(8.0, 8.0), Coord::new(14.0, 8.0), Coord::new(8.0, 14.0)], &b);
        let screen: Vec<(f64, f64)> = poly.iter().map(|c| (c.1.x, c.1.y)).collect();
        let mut out = Vec::new();
        edges(&b, &poly, &screen, 1.0, &mut out);
        let mut sides: Vec<u16> = out.iter().map(|e| e.side).collect();
        sides.sort_unstable();
        assert_eq!(sides, vec![u16::from(EAST), u16::from(NORTH)]);
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
                shape: rustgis_map::view::BlockShape::Box,
                water: None,
            }),
            water: None,
        });
        let b = Block {
            x0: -1000.0,
            y0: -1000.0,
            x1: 1000.0,
            y1: 1000.0,
            base: -100.0,
            settled: true,
            walls: rustgis_map::view::BLOCK_WALLS,
            circle: None,
            outline: None,
        };
        (m, b)
    }

    /// A walls layer held in another system cuts the block's sides where the same layer held in
    /// longitude/latitude does (#2032 C2).
    #[test]
    fn a_held_walls_layer_cuts_the_sides_alike() {
        let (mut m, b) = walls_by_units();
        let cuts = |m: &Map| {
            let u = units(m.layer_by_name("geo").unwrap(), &b, false, &Default::default()).units;
            u.iter()
                .map(|side| side.iter().map(|&(lo, hi, row)| ((lo * 1e3).round(), (hi * 1e3).round(), row)).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        };
        let plain = cuts(&m);
        let l = m.layers.iter_mut().find(|l| l.name == "geo").unwrap();
        l.dataset.hold_in(rustgis_data::raster::Crs::Epsg(32631)).unwrap();
        l.touch_geometry();
        assert_eq!(cuts(&m), plain);
        assert!(plain.iter().flatten().any(|c| c.2.is_some()), "the layer cuts the sides");
    }

    /// A polygon block's layer held in another system gives the outline it gives in
    /// longitude/latitude (#2598): its coordinates aren't read as degrees.
    #[test]
    fn a_held_polygon_blocks_outline_is_where_its_lonlat_one_is() {
        let (mut m, _) = walls_by_units();
        let plain = Outline::of(&m, "geo", None).expect("an outline").extent();
        let l = m.layers.iter_mut().find(|l| l.name == "geo").unwrap();
        l.dataset.hold_in(rustgis_data::raster::Crs::Epsg(32631)).unwrap();
        l.touch_geometry();
        let held = Outline::of(&m, "geo", None).expect("an outline").extent();
        assert!(held.iter().zip(plain).all(|(a, b)| (a - b).abs() < 0.01), "{held:?} {plain:?}");
    }

    /// A held polygon block's polygon with a place that can't be given in longitude/latitude is
    /// left out of the outline and counted by the drawing that read it, as a walls layer's are.
    #[test]
    fn a_held_polygon_blocks_unplaced_polygon_is_counted() {
        let (mut m, _) = walls_by_units();
        let l = m.layers.iter_mut().find(|l| l.name == "geo").unwrap();
        l.dataset.hold_in(rustgis_data::raster::Crs::Epsg(32631)).unwrap();
        let far = |x: f64| Coord::new(1e12 + x, 1e12);
        l.dataset.push(
            Some(Geometry::Polygon(vec![vec![vec![far(0.0), far(1.0), far(2.0), far(0.0)]]])),
            vec![rustgis_data::Value::Text("D".into())],
        );
        l.touch_geometry();
        let o = crate::held_reads_counted(&mut m, |m| Outline::of(m, "geo", None)).expect("an outline");
        assert_eq!(o.extent().map(|v| v.round()), Outline::of(&m, "geo", Some("unit <> 'D'")).unwrap().extent().map(|v| v.round()));
        assert_eq!(m.layer_by_name("geo").and_then(|l| l.unplaced()), Some(&Ok(1)));
    }

    /// The north side's stretches when unit A's east edge lies `gap` west of x 0 and unit B's
    /// (without its hole) `gap` east of it.
    fn north_with_a_gap(gap: f64) -> Vec<(f64, f64, Option<usize>)> {
        let (mut m, b) = walls_by_units();
        let l = m.layers.iter_mut().find(|l| l.name == "geo").unwrap();
        let deg = |x: f64, y: f64| rustgis_geom::merc_to_lonlat(Coord::new(x, y));
        let a = vec![deg(-5000.0, -5000.0), deg(-gap, -5000.0), deg(-gap, 5000.0), deg(-5000.0, 5000.0), deg(-5000.0, -5000.0)];
        let bb = vec![deg(gap, 0.0), deg(5000.0, 0.0), deg(5000.0, 5000.0), deg(gap, 5000.0), deg(gap, 0.0)];
        l.dataset.features[0].geometry = Some(Geometry::Polygon(vec![vec![a]]));
        l.dataset.features[1].geometry = Some(Geometry::Polygon(vec![vec![bb]]));
        l.touch_geometry();
        let u = units(m.layer_by_name("geo").unwrap(), &b, false, &Default::default()).units;
        u.get(2).cloned().unwrap_or_default()
    }

    /// Neighbours whose shared edge crosses a side a few rounding steps apart (as converting
    /// their coordinates leaves them), or up to `SEAM` apart (a polar system's round trip), meet
    /// there, with no sliver of bare wall between; a wider gap keeps its own bare stretch.
    #[test]
    fn neighbours_a_rounding_apart_meet_on_the_side() {
        // 2 nm and 0.5 mm apart: one seam at x 0.
        for gap in [1e-9, 2.5e-4] {
            let north = north_with_a_gap(gap);
            let rounded: Vec<_> = north.iter().map(|&(lo, hi, row)| (lo.round(), hi.round(), row)).collect();
            assert_eq!(rounded.first(), Some(&(-1000.0, 0.0, Some(0))), "gap {gap}");
            assert_eq!(rounded.get(1), Some(&(0.0, 500.0, Some(1))), "gap {gap}");
            assert!(north.iter().all(|&(lo, hi, _)| hi - lo > SEAM), "no sliver at gap {gap}: {north:?}");
        }
        // 5 mm apart: a real gap, bare wall between A and B.
        let north = north_with_a_gap(2.5e-3);
        let bare = north.get(1).copied();
        assert!(bare.is_some_and(|(lo, hi, row)| row.is_none() && (hi - lo - 5e-3).abs() < 1e-6), "{north:?}");
    }

    #[test]
    fn walls_take_the_unit_under_the_boxs_edge_with_contacts_straight_down() {
        let (mut m, b) = walls_by_units();
        let cuts = |m: &Map| {
            let u = units(m.layer_by_name("geo").unwrap(), &b, false, &Default::default()).units;
            let u: Vec<Vec<_>> = u.iter().map(|side| side.iter().map(|&(lo, hi, row)| (lo.round(), hi.round(), row)).collect()).collect();
            <[Vec<_>; 4]>::try_from(u).unwrap()
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

    #[test]
    fn a_pattern_round_a_round_blocks_wall_holds_whole_periods_so_it_meets_itself_at_the_east_point() {
        let hatch = |angle| FillPattern::Hatch { angle, spacing: 6.0, width: 1.0, offset: 0.0 };
        // At 30° the lines repeat every 12 px across: 1000 px round holds 83 of them, stretched by 0.4%.
        let f = closing(hatch(30.0), 1000.0);
        assert!(((1000.0 * f / 12.0) - 83.0).abs() < 1e-9 && (f - 1.0).abs() < 0.01, "{f}");
        assert!(((1000.0 * closing(hatch(-75.0), 1000.0) / (6.0 / 75f64.to_radians().sin())).fract()).abs() < 1e-9);
        // Level lines don't change across, and a solid fill has nothing to meet.
        assert_eq!(closing(hatch(0.0), 1000.0), 1.0);
        assert_eq!(closing(FillPattern::Solid, 1000.0), 1.0);
        // Odd columns shifted down: the grid repeats every two columns.
        let dots = |dy| FillPattern::Dots {
            marker: Default::default(),
            size: 1.0,
            outline: None,
            outline_width: 0.0,
            spacing: [7.0, 7.0],
            displacement: [0.0, dy],
            offset: [0.0, 0.0],
        };
        assert!((1000.0 * closing(dots(0.0), 1000.0) / 7.0 - 143.0).abs() < 1e-9);
        assert!((1000.0 * closing(dots(3.0), 1000.0) / 14.0 - 71.0).abs() < 1e-9);
    }

    /// A flat DEM at 500 m round (0, 0), exaggerated twice, cut to the box ±1000 m (`shape`) with
    /// `water` in its walls, and a water layer of `lakes` (Web Mercator boxes, each with its value
    /// of the depth field "prof" or none), seen at 10 m a pixel.
    fn block_over_lakes_shaped(
        lakes: &[(&[[f64; 4]], Option<f64>)],
        water: Option<rustgis_map::view::BlockWater>,
        shape: rustgis_map::view::BlockShape,
    ) -> (Map, Block, Ground, f64) {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
        use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
        let gt = GeoTransform { origin_x: -5000.0, origin_y: 5000.0, pixel_width: 20.0, pixel_height: -20.0 };
        let dem =
            Raster::new("dem", 500, 500, vec![vec![500.0; 500 * 500]], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap();
        let mut m = Map::new("Map");
        m.add_raster("dem", rustgis_map::RasterLayer::from_path(dem, "dem.tif"));
        let deg = |x: f64, y: f64| merc_to_lonlat(Coord::new(x, y));
        let mut d = Dataset::new("lagos", GeometryType::Polygon, vec![Field::new("prof", FieldType::Double)]);
        for &(parts, prof) in lakes {
            let ring = |[x0, y0, x1, y1]: [f64; 4]| vec![vec![deg(x0, y0), deg(x1, y0), deg(x1, y1), deg(x0, y1), deg(x0, y0)]];
            let polygon = Geometry::Polygon(parts.iter().map(|p| ring(*p)).collect());
            d.push(Some(polygon), vec![prof.map_or(Value::Null, Value::Float)]);
        }
        m.add_dataset(d);
        m.view.terrain = Some(rustgis_map::view::Terrain {
            source: rustgis_map::view::TerrainSource::Layer("dem".into()),
            exaggeration: 2.0,
            block: Some(rustgis_map::view::TerrainBlock {
                extent: Some([-1000.0, -1000.0, 1000.0, 1000.0]),
                depth: None,
                walls: rustgis_map::view::BLOCK_WALLS,
                walls_layer: None,
                shape,
                water,
            }),
            water: Some(rustgis_map::view::TerrainWater { layer: "lagos".into(), level: None, glint: 0.0, shore: 0.35 }),
        });
        let camera = rustgis_map::Camera { center: Coord::new(0.0, 0.0), res: 10.0, bearing: 0.0, pitch: 60.0 };
        let g = Ground::of(&m, &camera).unwrap();
        let b = g.block(&m).unwrap();
        (m, b, g, camera.res)
    }

    /// [`block_over_lakes_shaped`] cut to the box.
    fn block_over_lakes(lakes: &[(&[[f64; 4]], Option<f64>)], water: Option<rustgis_map::view::BlockWater>) -> (Map, Block, Ground, f64) {
        block_over_lakes_shaped(lakes, water, rustgis_map::view::BlockShape::Box)
    }

    #[test]
    fn a_wall_across_a_lake_shows_its_water_down_to_a_basins_floor() {
        use rustgis_map::view::BlockWater;
        // #2317: the east side crosses a lake from y −300 to 300; another lake lies inside the box.
        let lakes: [(&[[f64; 4]], _); 2] = [(&[[500.0, -300.0, 1500.0, 300.0]], None), (&[[-500.0, -500.0, -200.0, -200.0]], None)];
        let (m, b, g, res) = block_over_lakes(&lakes, Some(BlockWater { depth: Some(10.0), field: None }));
        let w = water_runs(&m, &b, &g, res, 1.0, &Default::default());
        assert!(w[0].is_empty() && w[2].is_empty() && w[3].is_empty(), "only the east side crosses water");
        let [r] = &w[1][..] else { panic!("one stretch of water on the east side") };
        assert!((r.lo + 300.0).abs() < 1e-6 && (r.hi - 300.0).abs() < 1e-6, "{} {}", r.lo, r.hi);
        // At the shore's level, 10 m deep, both doubled.
        assert!((r.level - 1000.0).abs() < 1e-6 && (r.depth - 20.0).abs() < 1e-6, "{} {}", r.level, r.depth);
        // None at the shore, the full depth a shore band (4 px of 10 m) in from it, half halfway.
        for (y, floor) in [(300.0, 1000.0), (280.0, 990.0), (260.0, 980.0), (0.0, 980.0), (-290.0, 995.0)] {
            assert!((r.floor(Coord::new(1000.0, y)) - floor).abs() < 1e-6, "{y}: {}", r.floor(Coord::new(1000.0, y)));
        }
        // Each lake its own depth from the field; one without a value has none, unless a depth is
        // given for those.
        let lakes: [(&[[f64; 4]], _); 2] = [(&[[500.0, -300.0, 1500.0, 300.0]], Some(4.0)), (&[[-300.0, 600.0, 300.0, 1500.0]], None)];
        let field = |depth| Some(BlockWater { depth, field: Some("prof".into()) });
        let (m, b, g, res) = block_over_lakes(&lakes, field(None));
        let w = water_runs(&m, &b, &g, res, 1.0, &Default::default());
        assert_eq!((w[1].len(), w[2].len()), (1, 0));
        assert!((w[1][0].depth - 8.0).abs() < 1e-6);
        let (m, b, g, res) = block_over_lakes(&lakes, field(Some(10.0)));
        let w = water_runs(&m, &b, &g, res, 1.0, &Default::default());
        assert!((w[1][0].depth - 8.0).abs() < 1e-6 && (w[2][0].depth - 20.0).abs() < 1e-6);
        // No water in the block: none in the walls.
        let (m, b, g, res) = block_over_lakes(&lakes, None);
        assert!(water_runs(&m, &b, &g, res, 1.0, &Default::default()).iter().all(Vec::is_empty));
    }

    #[test]
    fn each_part_of_a_lake_shapes_its_own_floor_in_its_own_colour() {
        use rustgis_map::view::BlockWater;
        // One row of two parts meeting on y 0 across the east side: a stretch each, each floor
        // shaped by its own part's shore.
        let halves: [[f64; 4]; 2] = [[500.0, -300.0, 1500.0, 0.0], [500.0, 0.0, 1500.0, 300.0]];
        let (mut m, b, g, res) = block_over_lakes(&[(&halves, None)], Some(BlockWater { depth: Some(10.0), field: None }));
        let w = water_runs(&m, &b, &g, res, 1.0, &Default::default());
        let [south, north] = &w[1][..] else { panic!("a stretch a part: {}", w[1].len()) };
        assert!((south.hi - 0.0).abs() < 1e-6 && (north.lo - 0.0).abs() < 1e-6);
        for (run, y, floor) in [(south, -290.0, 995.0), (south, -150.0, 980.0), (north, 290.0, 995.0), (north, 150.0, 980.0)] {
            assert!((run.floor(Coord::new(1000.0, y)) - floor).abs() < 1e-6, "{y}: {}", run.floor(Coord::new(1000.0, y)));
        }
        // Styled at the drawing scale given, as the surface's water is: a fill reading `@map_scale`.
        use rustgis_map::symbol_by::SymbolBy;
        if let Some(l) = m.layers.iter_mut().find(|l| l.name == "lagos") {
            l.symbol_by =
                SymbolBy { fill: Some("CASE WHEN @map_scale BETWEEN 4000 AND 6000 THEN '#00ff00' END".into()), ..Default::default() };
        }
        let green = Rgba::rgb(0, 255, 0);
        assert!(water_runs(&m, &b, &g, res, 5000.0, &Default::default())[1].iter().all(|r| r.color == green));
        assert!(water_runs(&m, &b, &g, res, 50_000.0, &Default::default())[1].iter().all(|r| r.color != green));
        if let Some(l) = m.layers.iter_mut().find(|l| l.name == "lagos") {
            l.symbol_by = SymbolBy::default();
        }
        // The section takes the topmost solid fill of the layer's stack, opaque.
        let blue = Rgba::rgb(20, 60, 200);
        if let Some(l) = m.layers.iter_mut().find(|l| l.name == "lagos") {
            l.fill_layers =
                vec![FillLayer { color: None, pattern: FillPattern::Solid }, FillLayer { color: Some(blue), pattern: FillPattern::Solid }];
        }
        assert!(water_runs(&m, &b, &g, res, 1.0, &Default::default())[1].iter().all(|r| r.color == blue));
    }

    #[test]
    fn a_round_blocks_wall_across_a_lake_shows_its_water_round_its_east_point() {
        use rustgis_map::view::{BlockShape, BlockWater};
        // The circle of radius 1000 crosses the lake (y −300 to 300 east of x 500) on the sides
        // round its east point, and nowhere else.
        let lakes: [(&[[f64; 4]], _); 1] = [(&[[500.0, -300.0, 1500.0, 300.0]], None)];
        let water = Some(BlockWater { depth: Some(10.0), field: None });
        let (m, b, g, res) = block_over_lakes_shaped(&lakes, water, BlockShape::Circle { radius: None });
        let w = water_runs(&m, &b, &g, res, 1.0, &Default::default());
        assert_eq!(w.len(), ROUND_SIDES);
        // asin(0.3) of the turn each way from east: about 25 sides each.
        let reach = (0.3f64.asin() / std::f64::consts::TAU * ROUND_SIDES as f64).ceil() as usize;
        let wet: Vec<usize> = (0..ROUND_SIDES).filter(|&i| !w[i].is_empty()).collect();
        assert!(wet.iter().all(|&i| i < reach || i >= ROUND_SIDES - reach), "{wet:?}");
        assert!(wet.len() >= 2 * reach - 2, "{} of {}", wet.len(), 2 * reach);
        // Each at the shore's level, 10 m deep, both doubled.
        assert!(wet.iter().flat_map(|&i| &w[i]).all(|r| (r.level - 1000.0).abs() < 1e-6 && (r.depth - 20.0).abs() < 1e-6));
        // The full depth at the east point, none where the circle meets the shore (y 300).
        let east = b.point(0, w[0][0].lo);
        assert!((w[0][0].floor(east) - 980.0).abs() < 1e-6, "{}", w[0][0].floor(east));
        let side = reach - 1;
        let last = w[side].last().unwrap();
        let shore = b.point(side as u16, last.hi);
        assert!((shore.y - 300.0).abs() < 1e-6, "{shore:?}");
        assert!((last.floor(shore) - 1000.0).abs() < 1e-6, "{}", last.floor(shore));
    }

    #[test]
    fn a_polygon_blocks_walls_stop_when_its_ground_does() {
        // #2318: a frame cancelled while its ground is drawn among the walls (`ground_to` says
        // stop) draws no more of them.
        let square: [(&[[f64; 4]], _); 1] = [(&[[-1000.0, -1000.0, 1000.0, 1000.0]], None)];
        let shape = rustgis_map::view::BlockShape::Polygon { layer: "lagos".into(), filter: None };
        let (m, b, g, res) = block_over_lakes_shaped(&square, None, shape);
        assert!(b.outline.is_some());
        let camera = rustgis_map::Camera { center: Coord::new(0.0, 0.0), res, bearing: 0.0, pitch: 60.0 };
        let view = View { width: 600.0, height: 500.0, pixels_per_point: 1.0, camera, sky: false };
        // The south side, which faces the eye.
        let south = (0..b.sides())
            .find(|&k| {
                let (_, a, z) = b.side(k);
                (a.y + 1000.0).abs() < 1.0 && (z.y + 1000.0).abs() < 1.0
            })
            .expect("a south side");
        let top = |x: f64| {
            let m = Coord::new(x, -1000.0);
            let (u, v) = camera.ground_px(m);
            camera.project_raised(u, v, g.up_px(m, &camera), view.width, view.height).expect("in view")
        };
        let edges: Vec<Edge> = (0..8)
            .map(|i| {
                let (x0, x1) = (-1000.0 + 250.0 * f64::from(i), -750.0 + 250.0 * f64::from(i));
                Edge { side: south as u16, ends: [Coord::new(x0, -1000.0), Coord::new(x1, -1000.0)], top: [top(x0), top(x1)], depth: 1.0 }
            })
            .collect();
        let runs = runs(&m, &b, 1.0, &Default::default());
        let water: WaterRuns = (0..b.sides()).map(|_| Vec::new()).collect();
        let draw = |go_on: bool| {
            let (mut pm, mut asked) = (Pixmap::new(600, 500).unwrap(), 0);
            draw_walls(&mut pm, &view, &g, &b, &edges, &runs, &water, Rgba::rgb(255, 255, 255), rustgis_map::camera::FAR, |_, _| {
                asked += 1;
                go_on
            });
            (pm.pixels().iter().filter(|p| p.alpha() > 0).count(), asked)
        };
        let (drawn, asked) = draw(true);
        assert!(drawn > 0 && asked >= 1, "{drawn} {asked}");
        assert_eq!(draw(false), (0, 1));
    }

    #[test]
    fn a_wall_in_pieces_keeps_its_water_to_each_piece() {
        use rustgis_map::view::BlockWater;
        // #2317: the south side crosses a lake from x −800 to 800, but a band of ground that
        // failed to draw left its wall only at x −1000 to −300 and 300 to 1000: water on each
        // piece, none in the gap between them.
        let lakes: [(&[[f64; 4]], _); 1] = [(&[[-800.0, -1500.0, 800.0, -500.0]], None)];
        let (m, b, g, res) = block_over_lakes(&lakes, Some(BlockWater { depth: Some(50.0), field: None }));
        let water = water_runs(&m, &b, &g, res, 1.0, &Default::default());
        let camera = rustgis_map::Camera { center: Coord::new(0.0, 0.0), res, bearing: 0.0, pitch: 60.0 };
        let view = View { width: 600.0, height: 500.0, pixels_per_point: 1.0, camera, sky: false };
        let (w, h) = (view.width, view.height);
        // On the south side at `x`, `metres` high (exaggerated), on screen.
        let at = |x: f64, metres: f64| {
            let m = Coord::new(x, -1000.0);
            let (u, v) = camera.ground_px(m);
            camera.project_raised(u, v, metres / g.per_unit(m) / res, w, h).expect("in view")
        };
        let top = |x: f64| {
            let m = Coord::new(x, -1000.0);
            let (u, v) = camera.ground_px(m);
            camera.project_raised(u, v, g.up_px(m, &camera), w, h).expect("in view")
        };
        let stretch = |x0: f64, x1: f64| Edge {
            side: u16::from(SOUTH),
            ends: [Coord::new(x0, -1000.0), Coord::new(x1, -1000.0)],
            top: [top(x0), top(x1)],
            depth: 1.0,
        };
        let edges: Vec<Edge> =
            [(-1000.0, -650.0), (-650.0, -300.0), (300.0, 650.0), (650.0, 1000.0)].iter().map(|&(a, z)| stretch(a, z)).collect();
        let runs = runs(&m, &b, 1.0, &Default::default());
        let draw = |water: &WaterRuns| {
            let mut pm = Pixmap::new(600, 500).unwrap();
            draw_walls(&mut pm, &view, &g, &b, &edges, &runs, water, Rgba::rgb(255, 255, 255), rustgis_map::camera::FAR, |_, _| true);
            pm
        };
        let (wet, dry) = (draw(&water), draw(&(0..b.sides()).map(|_| Vec::new()).collect()));
        let pixel = |pm: &Pixmap, x: f64, metres: f64| {
            let (px, py) = at(x, metres);
            pm.pixel(px as u32, py as u32).expect("in the picture").demultiply()
        };
        // Halfway down the water (level 1000, floor 900 away from the shore), on each piece: the
        // water's colour over the wall's.
        let [r] = &water[0][..] else { panic!("one stretch of water on the south side") };
        // The water is its colour shaded, so its colour's shares of red, green and blue.
        let shares = |p: [u8; 3]| {
            let sum = p.iter().map(|v| f64::from(*v)).sum::<f64>().max(1.0);
            p.map(|v| f64::from(v) / sum)
        };
        let apart = |p: [f64; 3], q: [f64; 3]| p.iter().zip(q).map(|(a, b)| (a - b).abs()).sum::<f64>();
        let rgb = |c: tiny_skia::ColorU8| shares([c.red(), c.green(), c.blue()]);
        let water_rgb = shares([r.color.r(), r.color.g(), r.color.b()]);
        for x in [-500.0, 500.0] {
            let (c, d) = (pixel(&wet, x, 950.0), pixel(&dry, x, 950.0));
            assert!(c.alpha() == 255 && d.alpha() == 255, "{x}: {c:?} over {d:?}");
            assert!(apart(rgb(c), water_rgb) < 0.05 && apart(rgb(d), water_rgb) > 0.1, "{x}: {c:?} over {d:?}, water {:?}", r.color);
        }
        // Nothing in the gap: no wall there, and no water across it.
        for x in [-150.0, 0.0, 150.0] {
            assert_eq!(pixel(&wet, x, 950.0).alpha(), 0, "{x}: {:?}", pixel(&wet, x, 950.0));
        }
    }
}
