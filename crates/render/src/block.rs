//! Block diagrams (#1914): a tilted view's ground over terrain cut to a box, and the box's sides
//! facing the eye drawn as walls from the ground's profile down to a flat base.

use rustgis_geom::Coord;
use rustgis_map::Rgba;
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

/// Draws the walls of block `b` over the ground already draped in `pm`: the sides facing the eye
/// (the box is convex, so no ground stands in front of them), each stretch of `edges` from the
/// ground down to the base, lit by the way its side faces the light as extruded walls are and
/// darker toward its foot, fogged with distance as the ground is (into `fog` toward the view's
/// reach `far`), and outlined along the ground, the base and the box's corners.
pub(crate) fn draw_walls(pm: &mut Pixmap, view: &View, ground: &Ground, b: &Block, edges: &[Edge], fog: Rgba, far: f64) {
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
    for (side, a, z) in corners {
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
        // The whole side in one path, every stretch a quad a hair longer each way along the edge:
        // filled at once, the overlaps leave no seam, where stretches filled one by one striped
        // the wall with their anti-aliased edges.
        let (mut wall, mut outline) = (PathBuilder::new(), PathBuilder::new());
        let (mut foot_mid, mut top_mid, mut count, mut nearest) = ((0.0, 0.0), (0.0, 0.0), 0.0, f64::INFINITY);
        for e in edges.iter().filter(|e| e.side == side) {
            let (Some(fa), Some(fz)) = (foot(e.ends[0]), foot(e.ends[1])) else { continue };
            let [ta, tz] = e.top;
            let grow = |p: (f64, f64), q: (f64, f64)| {
                let (dx, dy) = (p.0 - q.0, p.1 - q.1);
                let l = dx.hypot(dy).max(1e-6);
                pt((p.0 + dx / l * 0.4 * k, p.1 + dy / l * 0.4 * k))
            };
            for (i, p) in [grow(fa, fz), grow(fz, fa), grow(tz, ta), grow(ta, tz)].iter().enumerate() {
                if i == 0 { wall.move_to(p.0, p.1) } else { wall.line_to(p.0, p.1) }
            }
            wall.close();
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
        let Some(path) = wall.finish() else { continue };
        // Shaded, then into the fog: in full fog both ends are the fog.
        let f = fog_at(nearest);
        let (top, foot) = (b.walls.shade(top_shade).lerp(fog, f), b.walls.shade(foot_shade).lerp(fog, f));
        // Darker toward the foot: from the stretches' feet on average to their tops.
        let avg = |p: (f64, f64)| Point::from_xy((p.0 / count) as f32, (p.1 / count) as f32);
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
}
