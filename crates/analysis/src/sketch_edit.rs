//! Editing a feature with a drawn line: cutting polygons in pieces, reshaping a boundary or a
//! line, and tracing along a feature's edge. Computed in Web Mercator metres, as the other
//! editing tools are; vertices an edit doesn't move keep their exact longitude and latitude.

use std::collections::HashMap;

use geo::Validation;
use rustgis_data::Geometry;
use rustgis_geom::{Coord, lonlat_to_merc, merc_to_lonlat};

/// Points closer than this (Web Mercator metres) are one point: a vertex snapped onto a boundary meets it.
const EPS: f64 = 1e-3;
/// A drawn line's first or last point this near an edge (Web Mercator metres) is on it: a point snapped onto
/// an edge in another map projection can sit a few millimetres off it in Web Mercator.
const END_SNAP: f64 = 0.01;
/// Most segment pairs one edit compares, so a huge shape or line can't stall the app.
const MAX_WORK: usize = 50_000_000;

/// Refuses an edit comparing more than [`MAX_WORK`] segment pairs.
fn within_work(a: usize, b: usize) -> Result<(), String> {
    if a.saturating_mul(b) > MAX_WORK { Err(TOO_MUCH.into()) } else { Ok(()) }
}

/// Why an edit past [`MAX_WORK`] is refused.
const TOO_MUCH: &str = "the shapes and the line have too many vertices to edit together";

/// A point in Web Mercator metres and the longitude/latitude it came from.
#[derive(Clone, Copy, Debug)]
struct Pt {
    m: Coord,
    ll: Coord,
}

impl Pt {
    /// An input vertex: its longitude/latitude kept as given.
    fn of(ll: Coord) -> Pt {
        Pt { m: lonlat_to_merc(ll), ll }
    }
    /// A new point worked out in metres.
    fn at(m: Coord) -> Pt {
        Pt { m, ll: merc_to_lonlat(m) }
    }
}

/// The straight distance between two points in metres.
fn dist(a: Coord, b: Coord) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

/// The input as metres, refusing coordinates that don't project (a pole, NaN).
/// Longitudes beyond ±360° (as `edit.vertex` takes, for features unwrapped across the
/// antimeridian) aren't coordinates on the Earth.
fn project(path: &[Coord]) -> Result<Vec<Pt>, String> {
    let pts: Vec<Pt> = path.iter().map(|c| Pt::of(*c)).collect();
    if pts.iter().all(|p| p.m.is_finite() && p.ll.x.abs() <= 360.0 && p.ll.y.abs() <= 90.0) {
        Ok(pts)
    } else {
        Err("the shape has coordinates that can't be drawn on the map".into())
    }
}

/// A polygon ring as metres, closed: a ring stored without repeating its first point gets its
/// closing edge.
fn project_ring(ring: &[Coord]) -> Result<Vec<Pt>, String> {
    let mut r = project(ring)?;
    if let (Some(first), Some(last)) = (r.first().copied(), r.last())
        && first.ll != last.ll
    {
        r.push(first);
    }
    Ok(r)
}

/// Twice the signed area of a ring in metres (positive anticlockwise).
fn area2(ring: &[Pt]) -> f64 {
    ring.windows(2).filter_map(|w| Some((w.first()?.m, w.get(1)?.m))).map(|(a, b)| a.x * b.y - b.x * a.y).sum()
}

/// [`area2`] of a ring in longitude/latitude.
fn area2_ll(ring: &[Coord]) -> f64 {
    area2(&ring.iter().map(|c| Pt::of(*c)).collect::<Vec<_>>())
}

/// A path's length in metres.
fn length(path: &[Pt]) -> f64 {
    path.windows(2).filter_map(|w| Some(dist(w.first()?.m, w.get(1)?.m))).sum()
}

/// Where `p` falls on segment `a`–`b`: the parameter (0 at `a`, 1 at `b`) and the distance off it.
fn foot(p: Coord, a: Coord, b: Coord) -> (f64, f64) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (t, dist(p, Coord::new(a.x + t * dx, a.y + t * dy)))
}

/// Where segments `a` and `b` meet, as (parameter on `a`, parameter on `b`, point), including
/// touches within [`EPS`] at their ends and both ends of a shared, overlapping stretch.
fn meet(a: (Coord, Coord), b: (Coord, Coord)) -> Vec<(f64, f64, Coord)> {
    let mut out = Vec::new();
    let mut add = |ta: f64, tb: f64, c: Coord| {
        if !out.iter().any(|(_, _, o): &(f64, f64, Coord)| dist(*o, c) <= EPS) {
            out.push((ta, tb, c));
        }
    };
    // An end of one lying on the other.
    for (own, p) in [(0.0, a.0), (1.0, a.1)] {
        let (t, d) = foot(p, b.0, b.1);
        if d <= EPS {
            add(own, t, p);
        }
    }
    for (own, p) in [(0.0, b.0), (1.0, b.1)] {
        let (t, d) = foot(p, a.0, a.1);
        if d <= EPS {
            add(t, own, p);
        }
    }
    // A crossing in the middle of both.
    let (rx, ry, sx, sy) = (a.1.x - a.0.x, a.1.y - a.0.y, b.1.x - b.0.x, b.1.y - b.0.y);
    let den = rx * sy - ry * sx;
    if den.abs() > f64::EPSILON * (rx.hypot(ry) * sx.hypot(sy)).max(f64::MIN_POSITIVE) {
        let (qx, qy) = (b.0.x - a.0.x, b.0.y - a.0.y);
        let t = (qx * sy - qy * sx) / den;
        let u = (qx * ry - qy * rx) / den;
        if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
            add(t, u, Coord::new(a.0.x + t * rx, a.0.y + t * ry));
        }
    }
    out
}

/// Points merged within [`EPS`], found through a grid of [`EPS`]-sized cells.
#[derive(Default)]
struct Nodes {
    pts: Vec<Pt>,
    grid: HashMap<(i64, i64), Vec<usize>>,
}

impl Nodes {
    /// The grid cell holding `m`.
    fn cell(m: Coord) -> (i64, i64) {
        ((m.x / EPS).floor() as i64, (m.y / EPS).floor() as i64)
    }
    /// The node at `p`: an earlier one within [`EPS`] (keeping its exact coordinates), or a new one.
    fn add(&mut self, p: Pt) -> usize {
        let (cx, cy) = Nodes::cell(p.m);
        for dx in -1..=1 {
            for dy in -1..=1 {
                // Saturating: an absurd coordinate's cell sits at the edge of the grid.
                for &i in self.grid.get(&(cx.saturating_add(dx), cy.saturating_add(dy))).into_iter().flatten() {
                    if self.pts.get(i).is_some_and(|q| dist(q.m, p.m) <= EPS) {
                        return i;
                    }
                }
            }
        }
        self.pts.push(p);
        let i = self.pts.len() - 1;
        self.grid.entry((cx, cy)).or_default().push(i);
        i
    }
    /// Node `i` in metres (the origin for an index that isn't one, which the graph never asks).
    fn m(&self, i: usize) -> Coord {
        self.pts.get(i).map_or(Coord::new(0.0, 0.0), |p| p.m)
    }
}

/// An edge of the cutting graph: from `u` to `v`, along a ring (with the polygon on its left)
/// or along the cut.
#[derive(Clone, Copy)]
struct Edge {
    u: usize,
    v: usize,
    ring: bool,
}

/// Whether `p` is inside a polygon's rings (even-odd), in metres.
fn inside(p: Coord, rings: &[Vec<Pt>]) -> bool {
    let mut odd = false;
    for ring in rings {
        for w in ring.windows(2) {
            let (Some(a), Some(b)) = (w.first(), w.get(1)) else { continue };
            let (a, b) = (a.m, b.m);
            if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
                odd = !odd;
            }
        }
    }
    odd
}

/// Whether `p` lies within [`EPS`] of a ring.
fn on_boundary(p: Coord, rings: &[Vec<Pt>]) -> bool {
    rings.iter().any(|r| r.windows(2).any(|w| matches!((w.first(), w.get(1)), (Some(a), Some(b)) if foot(p, a.m, b.m).1 <= EPS)))
}

/// The faces of a planar graph, each to the left of its cycle of half-edges. Half-edge `2k` runs
/// `u`→`v` along edge `k`, `2k + 1` back. Returns each half-edge's face and the faces' cycles.
fn faces(nodes: &Nodes, edges: &[Edge]) -> (Vec<usize>, Vec<Vec<usize>>) {
    let half = |h: usize| -> (usize, usize) {
        let e = edges.get(h / 2).copied().unwrap_or(Edge { u: 0, v: 0, ring: false });
        if h.is_multiple_of(2) { (e.u, e.v) } else { (e.v, e.u) }
    };
    // Each node's outgoing half-edges, anticlockwise by direction.
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); nodes.pts.len()];
    for h in 0..edges.len() * 2 {
        if let Some(o) = out.get_mut(half(h).0) {
            o.push(h);
        }
    }
    let angle = |h: usize| {
        let (a, b) = half(h);
        let (a, b) = (nodes.m(a), nodes.m(b));
        (b.y - a.y).atan2(b.x - a.x)
    };
    let mut pos = vec![0usize; edges.len() * 2];
    for o in &mut out {
        o.sort_by(|x, y| angle(*x).total_cmp(&angle(*y)));
        for (i, h) in o.iter().enumerate() {
            if let Some(p) = pos.get_mut(*h) {
                *p = i;
            }
        }
    }
    // Arriving along h, leave by the next half-edge clockwise from h's twin.
    let next = |h: usize| -> usize {
        let twin = h ^ 1;
        let at = half(twin).0;
        let o = out.get(at).map_or(&[][..], Vec::as_slice);
        let i = pos.get(twin).copied().unwrap_or(0);
        o.get((i + o.len().saturating_sub(1)) % o.len().max(1)).copied().unwrap_or(twin)
    };
    let mut face = vec![usize::MAX; edges.len() * 2];
    let mut cycles = Vec::new();
    for start in 0..edges.len() * 2 {
        if face.get(start) != Some(&usize::MAX) {
            continue;
        }
        let id = cycles.len();
        let mut cycle = Vec::new();
        let mut h = start;
        // Every half-edge is on one face, so a walk is never longer than all of them.
        while face.get(h) == Some(&usize::MAX) && cycle.len() <= edges.len() * 2 {
            if let Some(f) = face.get_mut(h) {
                *f = id;
            }
            cycle.push(h);
            h = next(h);
        }
        cycles.push(cycle);
    }
    (face, cycles)
}

/// One polygon (outer ring, then holes) cut by a line: its pieces, or `None` when the line
/// doesn't divide it.
fn cut_part(rings: &[Vec<Coord>], line: &[Pt], work: &mut usize) -> Result<Option<Vec<Vec<Vec<Coord>>>>, String> {
    // Rings as metres, the outer one anticlockwise and holes clockwise: the inside on the left.
    let mut rs: Vec<Vec<Pt>> = Vec::new();
    for (i, r) in rings.iter().enumerate() {
        let mut r = project_ring(r)?;
        let a = area2(&r);
        if (i == 0) != (a > 0.0) {
            r.reverse();
        }
        rs.push(r);
    }
    // Drawn and existing vertices first, so a crossing on one of them keeps its exact coordinates.
    let mut nodes = Nodes::default();
    for p in rs.iter().flatten().chain(line) {
        nodes.add(*p);
    }
    // Segments with the points where they're cut, as (parameter, point).
    struct Seg {
        a: Pt,
        b: Pt,
        ring: bool,
        at: Vec<(f64, Coord)>,
    }
    let mut segs: Vec<Seg> = Vec::new();
    for r in &rs {
        for w in r.windows(2) {
            if let (Some(a), Some(b)) = (w.first(), w.get(1)) {
                segs.push(Seg { a: *a, b: *b, ring: true, at: Vec::new() });
            }
        }
    }
    let first_cut = segs.len();
    for w in line.windows(2) {
        if let (Some(a), Some(b)) = (w.first(), w.get(1)) {
            segs.push(Seg { a: *a, b: *b, ring: false, at: Vec::new() });
        }
    }
    // Two segments' boxes, grown by EPS, overlap: only then can they meet.
    let boxes_meet = |a: &Seg, b: &Seg| {
        a.a.m.x.min(a.b.m.x) <= b.a.m.x.max(b.b.m.x) + EPS
            && b.a.m.x.min(b.b.m.x) <= a.a.m.x.max(a.b.m.x) + EPS
            && a.a.m.y.min(a.b.m.y) <= b.a.m.y.max(b.b.m.y) + EPS
            && b.a.m.y.min(b.b.m.y) <= a.a.m.y.max(a.b.m.y) + EPS
    };
    let span = Seg {
        a: Pt::at(line.iter().fold(Coord::new(f64::MAX, f64::MAX), |lo, p| Coord::new(lo.x.min(p.m.x), lo.y.min(p.m.y)))),
        b: Pt::at(line.iter().fold(Coord::new(f64::MIN, f64::MIN), |hi, p| Coord::new(hi.x.max(p.m.x), hi.y.max(p.m.y)))),
        ring: false,
        at: Vec::new(),
    };
    // The cut against the ring segments near it and against itself; rings aren't checked against
    // each other. Every pair compared counts against the edit's budget.
    let near: Vec<usize> = (0..first_cut).filter(|&i| segs.get(i).is_some_and(|si| boxes_meet(si, &span))).collect();
    for j in first_cut..segs.len() {
        for i in near.iter().copied().chain(first_cut..j) {
            *work += 1;
            if *work > MAX_WORK {
                return Err(TOO_MUCH.into());
            }
            let (Some(si), Some(sj)) = (segs.get(i), segs.get(j)) else { continue };
            if !boxes_meet(si, sj) {
                continue;
            }
            let hits = meet((si.a.m, si.b.m), (sj.a.m, sj.b.m));
            for (ti, tj, c) in hits {
                if let Some(s) = segs.get_mut(i) {
                    s.at.push((ti, c));
                }
                if let Some(s) = segs.get_mut(j) {
                    s.at.push((tj, c));
                }
            }
        }
    }
    // Edges between consecutive points, a ring's kept where the cut runs along it.
    let mut edges: Vec<Edge> = Vec::new();
    let mut seen: HashMap<(usize, usize), usize> = HashMap::new();
    for s in &mut segs {
        s.at.sort_by(|x, y| x.0.total_cmp(&y.0));
        let mut ids = vec![nodes.add(s.a)];
        for (t, c) in &s.at {
            if *t > 0.0 && *t < 1.0 {
                ids.push(nodes.add(Pt::at(*c)));
            }
        }
        ids.push(nodes.add(s.b));
        for w in ids.windows(2) {
            let (Some(&u), Some(&v)) = (w.first(), w.get(1)) else { continue };
            if u == v {
                continue;
            }
            let key = (u.min(v), u.max(v));
            match seen.get(&key) {
                // The cut ran along the ring here: the ring's edge, in the ring's direction.
                Some(&k) => {
                    if let Some(e) = edges.get_mut(k).filter(|e| s.ring && !e.ring) {
                        *e = Edge { u, v, ring: true };
                    }
                }
                None => {
                    seen.insert(key, edges.len());
                    edges.push(Edge { u, v, ring: s.ring });
                }
            }
        }
    }
    // Only the cut's stretches inside the polygon divide it. Each test walks every ring twice
    // (inside, on the boundary): that counts against the budget too.
    let ring_segments: usize = rs.iter().map(Vec::len).sum();
    *work = work.saturating_add(edges.iter().filter(|e| !e.ring).count().saturating_mul(ring_segments).saturating_mul(2));
    if *work > MAX_WORK {
        return Err(TOO_MUCH.into());
    }
    edges.retain(|e| {
        if e.ring {
            return true;
        }
        let (a, b) = (nodes.m(e.u), nodes.m(e.v));
        let mid = Coord::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        inside(mid, &rs) && !on_boundary(mid, &rs)
    });
    // A stretch with the same face on both sides (a loose end, or one that only joins a hole
    // to itself) divides nothing: drop it and look again.
    let cycles = loop {
        let (face, cycles) = faces(&nodes, &edges);
        let before = edges.len();
        let mut k = 0;
        edges.retain(|e| {
            let same = !e.ring && face.get(2 * k) == face.get(2 * k + 1);
            k += 1;
            !same
        });
        if edges.len() == before {
            break cycles;
        }
    };
    if !edges.iter().any(|e| !e.ring) {
        return Ok(None);
    }
    // A face is inside the polygon when its rings run the rings' way (inside on the left).
    let mut pieces: Vec<(Vec<Pt>, f64)> = Vec::new();
    let mut holes: Vec<Vec<Pt>> = Vec::new();
    for cycle in &cycles {
        let outside = cycle.iter().any(|h| !h.is_multiple_of(2) && edges.get(h / 2).is_some_and(|e| e.ring));
        if outside {
            continue;
        }
        let pts: Vec<Pt> = cycle
            .iter()
            .filter_map(|h| edges.get(h / 2).map(|e| if h.is_multiple_of(2) { e.u } else { e.v }))
            .filter_map(|n| nodes.pts.get(n).copied())
            .collect();
        let Some(first) = pts.first().copied() else { continue };
        let mut ring = pts;
        ring.push(first);
        let a = area2(&ring) / 2.0;
        if a > 0.0 {
            pieces.push((ring, a));
        } else if a < 0.0 {
            holes.push(ring);
        }
    }
    if pieces.len() < 2 {
        return Ok(None);
    }
    // Each hole the cut left alone goes in the smallest piece around it.
    let mut out: Vec<Vec<Vec<Pt>>> = pieces.iter().map(|(r, _)| vec![r.clone()]).collect();
    for h in holes {
        let Some(p) = h.first().map(|p| p.m) else { continue };
        let home = pieces
            .iter()
            .enumerate()
            .filter(|(_, (r, _))| inside(p, std::slice::from_ref(r)))
            .min_by(|a, b| a.1.1.total_cmp(&b.1.1))
            .map(|(i, _)| i);
        match home.and_then(|i| out.get_mut(i)) {
            Some(piece) => piece.push(h),
            None => return Err("the cut couldn't be worked out for this polygon".into()),
        }
    }
    Ok(Some(out.into_iter().map(|rings| rings.into_iter().map(|r| r.into_iter().map(|p| p.ll).collect()).collect()).collect()))
}

/// Whether a polygon is valid (simple rings, holes inside, no overlaps), in metres.
fn valid(rings: &[Vec<Coord>]) -> bool {
    geo_polygon(rings).is_some_and(|p| p.is_valid())
}

/// A polygon (outer ring, then holes) as a `geo` polygon in metres.
fn geo_polygon(rings: &[Vec<Coord>]) -> Option<geo::Polygon<f64>> {
    let ring = |r: &Vec<Coord>| geo::LineString::new(r.iter().map(|c| lonlat_to_merc(*c)).map(|m| geo::coord! {x: m.x, y: m.y}).collect());
    let mut it = rings.iter();
    let outer = it.next()?;
    Some(geo::Polygon::new(ring(outer), it.map(ring).collect()))
}

/// The area two polygons share, in square metres (0 when their boxes don't meet).
fn shared_area(a: &geo::Polygon<f64>, b: &geo::Polygon<f64>) -> f64 {
    use geo::{Area, BooleanOps, BoundingRect, Intersects};
    match (a.bounding_rect(), b.bounding_rect()) {
        (Some(x), Some(y)) if x.intersects(&y) => a.intersection(b).unsigned_area(),
        _ => 0.0,
    }
}

/// A polygon's area in square metres: its outer ring less its holes.
fn polygon_area(rings: &[Vec<Coord>]) -> f64 {
    rings
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let a = area2_ll(r).abs() / 2.0;
            if i == 0 { a } else { -a }
        })
        .sum()
}

/// Whether two paths meet anywhere. Past the work budget it answers yes, the cautious answer
/// for a hole deciding whether to stay (a kept hole that crosses is then refused as invalid).
fn touches(a: &[Pt], b: &[Pt]) -> bool {
    if within_work(a.len(), b.len()).is_err() {
        return true;
    }
    let seg = |w: &[Pt]| Some((w.first()?.m, w.get(1)?.m));
    a.windows(2).filter_map(seg).any(|x| {
        b.windows(2).filter_map(seg).any(|y| {
            x.0.x.min(x.1.x) <= y.0.x.max(y.1.x) + EPS
                && y.0.x.min(y.1.x) <= x.0.x.max(x.1.x) + EPS
                && x.0.y.min(x.1.y) <= y.0.y.max(y.1.y) + EPS
                && y.0.y.min(y.1.y) <= x.0.y.max(x.1.y) + EPS
                && !meet(x, y).is_empty()
        })
    })
}

/// Moves a drawn line's first and last points onto the nearest edge of `paths` when they are
/// within [`END_SNAP`] of it.
fn settle_ends(line: &mut [Pt], paths: &[Vec<Pt>]) {
    let last = line.len().saturating_sub(1);
    for i in [0, last] {
        let Some(p) = line.get(i).map(|p| p.m) else { continue };
        let near = paths.iter().filter_map(|path| nearest(path, p)).min_by(|a, b| a.1.total_cmp(&b.1));
        if let (Some((_, d, m)), Some(slot)) = (near, line.get_mut(i))
            && d > EPS
            && d <= END_SNAP
        {
            *slot = Pt::at(m);
        }
    }
}

/// Every ring or part of a line or polygon, in metres.
fn paths_of(g: &Geometry) -> Result<Vec<Vec<Pt>>, String> {
    match g {
        Geometry::Line(parts) => parts.iter().map(|p| project(p)).collect(),
        Geometry::Polygon(polys) => polys.iter().flatten().map(|r| project_ring(r)).collect(),
        _ => Ok(Vec::new()),
    }
}

/// A line's coordinates, at least two, that project.
fn sketch(line: &[Coord]) -> Result<Vec<Pt>, String> {
    let pts = project(line)?;
    if pts.len() < 2 || length(&pts) <= EPS {
        return Err("draw a line of at least two points".into());
    }
    Ok(pts)
}

/// Cuts a polygon feature along a line drawn across it. Returns `None` when the line divides no
/// part of it; otherwise its new shapes, the largest piece first with the parts the line didn't
/// cross, then each other piece on its own. `work` counts the segment pairs compared so far in
/// this edit, across every feature it cuts; past 50 million the edit is refused.
pub fn cut_polygon(g: &Geometry, line: &[Coord], work: &mut usize) -> Result<Option<Vec<Geometry>>, String> {
    let Geometry::Polygon(parts) = g else { return Ok(None) };
    let mut line = sketch(line)?;
    settle_ends(&mut line, &paths_of(g)?);
    let mut pieces: Vec<Vec<Vec<Coord>>> = Vec::new();
    let mut kept: Vec<Vec<Vec<Coord>>> = Vec::new();
    for part in parts {
        match cut_part(part, &line, work)? {
            Some(p) => {
                if valid(part) && !p.iter().all(|q| valid(q)) {
                    return Err(
                        "cutting there would leave an invalid shape (the line may touch a hole or a corner without crossing it): draw it again"
                            .into(),
                    );
                }
                pieces.extend(p);
            }
            None => kept.push(part.clone()),
        }
    }
    if pieces.is_empty() {
        return Ok(None);
    }
    let largest = pieces.iter().enumerate().max_by(|a, b| polygon_area(a.1).total_cmp(&polygon_area(b.1))).map_or(0, |(i, _)| i);
    let first = pieces.remove(largest);
    let mut out = vec![Geometry::Polygon(std::iter::once(first).chain(kept).collect())];
    out.extend(pieces.into_iter().map(|p| Geometry::Polygon(vec![p])));
    Ok(Some(out))
}

/// Where a drawn line meets a path: (position along the line, position along the path, point),
/// a position being a segment's index plus the parameter along it. Sorted along the line, with
/// repeats (a touch at a shared vertex) left out.
fn crossings(line: &[Pt], path: &[Pt]) -> Vec<(f64, f64, Coord)> {
    let mut hits = Vec::new();
    for (i, a) in line.windows(2).enumerate() {
        let (Some(a0), Some(a1)) = (a.first(), a.get(1)) else { continue };
        for (j, b) in path.windows(2).enumerate() {
            let (Some(b0), Some(b1)) = (b.first(), b.get(1)) else { continue };
            for (ta, tb, c) in meet((a0.m, a1.m), (b0.m, b1.m)) {
                hits.push((i as f64 + ta, j as f64 + tb, c));
            }
        }
    }
    hits.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut out: Vec<(f64, f64, Coord)> = Vec::new();
    for h in hits {
        if !out.iter().any(|o| dist(o.2, h.2) <= EPS) {
            out.push(h);
        }
    }
    out
}

/// The vertex of `path` nearest position `r`, when it's within [`EPS`] of `m`.
fn vertex_at(path: &[Pt], r: f64, m: Coord) -> Option<Pt> {
    path.get(r.round() as usize).filter(|v| dist(v.m, m) <= EPS).copied()
}

/// Where a drawn line meets a path, `(position on line, position on path, point)`: the path's
/// vertex there, else the line's (a snapped point), else a new point.
fn meeting(path: &[Pt], line: &[Pt], hit: &(f64, f64, Coord)) -> Pt {
    vertex_at(path, hit.1, hit.2).or_else(|| vertex_at(line, hit.0, hit.2)).unwrap_or_else(|| Pt::at(hit.2))
}

/// A path's vertices strictly between positions `from` and `to`, in that direction.
fn between(path: &[Pt], from: f64, to: f64) -> Vec<Pt> {
    let near = |k: usize, r: f64| (k as f64 - r).abs() < 1e-9;
    if from <= to {
        path.iter()
            .enumerate()
            .filter(|(k, _)| *k as f64 > from && (*k as f64) < to && !near(*k, from) && !near(*k, to))
            .map(|(_, p)| *p)
            .collect()
    } else {
        path.iter()
            .enumerate()
            .rev()
            .filter(|(k, _)| (*k as f64) < from && *k as f64 > to && !near(*k, from) && !near(*k, to))
            .map(|(_, p)| *p)
            .collect()
    }
}

/// A ring's vertices strictly between positions `from` and `to`, going forward (with the ring's
/// order) or back, round the closing vertex as needed.
fn round_ring(ring: &[Pt], from: f64, to: f64, forward: bool) -> Vec<Pt> {
    let n = ring.len().saturating_sub(1);
    if n == 0 {
        return Vec::new();
    }
    let nf = n as f64;
    let to = match (forward, to > from) {
        (true, true) | (false, false) => to,
        (true, false) => to + nf,
        (false, true) => to - nf,
    };
    let mut out = Vec::new();
    let mut k = if forward { from.floor() + 1.0 } else { from.ceil() - 1.0 };
    while out.len() <= n && if forward { k < to - 1e-9 } else { k > to + 1e-9 } {
        if (k - from).abs() > 1e-9 {
            let i = k.rem_euclid(nf) as usize;
            if let Some(p) = ring.get(i) {
                out.push(*p);
            }
        }
        k += if forward { 1.0 } else { -1.0 };
    }
    out
}

/// Reshapes one feature with a line drawn across its boundary. A polygon's boundary between the
/// line's first and last crossings of one ring is replaced by the line, keeping the larger of
/// the two shapes this could make; a line's stretch between two crossings is replaced, or with
/// one crossing, its shorter end. Holes left wholly outside a reshaped outer ring were in the
/// part reshaped away: they are removed, and counted.
pub fn reshape(g: &Geometry, line: &[Coord]) -> Result<Reshaped, String> {
    let mut line = sketch(line)?;
    settle_ends(&mut line, &paths_of(g)?);
    match g {
        Geometry::Polygon(parts) => {
            // The whole shape against the line, before any ring is compared.
            within_work(parts.iter().flatten().map(Vec::len).sum(), line.len())?;
            for (pi, part) in parts.iter().enumerate() {
                for (ri, ring) in part.iter().enumerate() {
                    let r = project_ring(ring)?;
                    let hits = crossings(&line, &r);
                    let (Some(p), Some(q)) = (hits.first(), hits.last()) else { continue };
                    if hits.len() < 2 {
                        continue;
                    }
                    let (pp, qp) = (meeting(&r, &line, p), meeting(&r, &line, q));
                    // The drawn stretch, starting and ending on the ring (on its own vertices
                    // where the line meets one).
                    let mut drawn = vec![pp];
                    drawn.extend(between(&line, p.0, q.0));
                    drawn.push(qp);
                    let make = |forward: bool| -> Vec<Coord> {
                        let mut c: Vec<Coord> = drawn.iter().map(|p| p.ll).collect();
                        c.extend(round_ring(&r, q.1, p.1, forward).into_iter().map(|p| p.ll));
                        c.push(pp.ll);
                        c
                    };
                    let (a, b) = (make(true), make(false));
                    let size = |c: &[Coord]| area2_ll(c).abs();
                    // The larger shape: a bigger outer ring, or a smaller hole.
                    let mut new = if (size(&a) >= size(&b)) == (ri == 0) { a } else { b };
                    if (area2_ll(&new) > 0.0) != (area2(&r) > 0.0) {
                        new.reverse();
                    }
                    let mut part = part.clone();
                    let part_len = part.len();
                    if let Some(slot) = part.get_mut(ri) {
                        *slot = new;
                    }
                    // A hole the new outer ring leaves wholly outside was in the area cut away.
                    if ri == 0
                        && let Some(outer) = part.first().map(|o| vec![o.iter().map(|c| Pt::of(*c)).collect::<Vec<_>>()])
                    {
                        // Every hole vertex against the new outer ring, within the budget.
                        within_work(part.iter().skip(1).map(Vec::len).sum(), part.first().map_or(0, Vec::len))?;
                        let mut k = 0;
                        part.retain(|r| {
                            k += 1;
                            let hole: Vec<Pt> = r.iter().map(|c| Pt::of(*c)).collect();
                            k == 1 || hole.iter().any(|p| inside(p.m, &outer)) || outer.iter().any(|o| touches(&hole, o))
                        });
                    }
                    let holes_removed = part_len - part.len();
                    if part.first().is_some_and(|o| o.len() < 4) || !valid(&part) {
                        return Err("reshaping there would make the boundary cross itself or another ring".into());
                    }
                    // The other parts of a multipart feature: a reshape mustn't push into one it
                    // didn't overlap before (a square metre counts, as in the overlap check).
                    if let (Some(new), Some(old)) = (geo_polygon(&part), parts.get(pi).and_then(|p| geo_polygon(p))) {
                        // Only siblings whose box meets the new part's are validated and measured.
                        // An invalid sibling is left out: its overlap can't be measured reliably,
                        // and an already broken feature shouldn't block the edit.
                        use geo::{BoundingRect, Intersects};
                        let new_box = new.bounding_rect();
                        let near = |p: &geo::Polygon<f64>| matches!((p.bounding_rect(), new_box), (Some(a), Some(b)) if a.intersects(&b));
                        for sib in parts
                            .iter()
                            .enumerate()
                            .filter(|(j, _)| *j != pi)
                            .filter_map(|(_, p)| geo_polygon(p))
                            .filter(|p| near(p) && p.is_valid())
                        {
                            if shared_area(&new, &sib) > 1.0 && shared_area(&old, &sib) <= 1.0 {
                                return Err("reshaping there would overlap another part of the same feature".into());
                            }
                        }
                    }
                    let mut parts = parts.clone();
                    if let Some(slot) = parts.get_mut(pi) {
                        *slot = part;
                    }
                    return Ok(Reshaped { geometry: Geometry::Polygon(parts), holes_removed });
                }
            }
            Err("draw the line across the polygon's boundary, leaving and coming back".into())
        }
        Geometry::Line(parts) => {
            within_work(parts.iter().map(Vec::len).sum(), line.len())?;
            let found: Vec<(usize, Vec<Pt>, Vec<(f64, f64, Coord)>)> = parts
                .iter()
                .enumerate()
                .map(|(i, p)| project(p).map(|r| (i, r.clone(), crossings(&line, &r))))
                .collect::<Result<Vec<_>, String>>()?
                .into_iter()
                .filter(|(_, _, h)| !h.is_empty())
                .collect();
            let pick = found.iter().find(|f| f.2.len() >= 2).or(found.first());
            let Some((pi, r, hits)) = pick else { return Err("draw the line from a point on the line".into()) };
            let new: Vec<Pt> = match (hits.first(), hits.last()) {
                (Some(p), Some(q)) if hits.len() >= 2 => {
                    let (pp, qp) = (meeting(r, &line, p), meeting(r, &line, q));
                    let mut drawn = vec![pp];
                    drawn.extend(between(&line, p.0, q.0));
                    drawn.push(qp);
                    let (start, end, drawn) = if p.1 <= q.1 { (p.1, q.1, drawn) } else { (q.1, p.1, drawn.into_iter().rev().collect()) };
                    let mut c: Vec<Pt> = r.iter().enumerate().filter(|(k, _)| (*k as f64) < start - 1e-9).map(|(_, p)| *p).collect();
                    c.extend(drawn);
                    c.extend(r.iter().enumerate().filter(|(k, _)| *k as f64 > end + 1e-9).map(|(_, p)| *p));
                    c
                }
                (Some(p), _) => {
                    let pp = meeting(r, &line, p);
                    // The line's longer side of the crossing stays; the drawn line's longer side
                    // of it becomes the new end.
                    let mut head: Vec<Pt> = r.iter().enumerate().filter(|(k, _)| (*k as f64) < p.1 - 1e-9).map(|(_, p)| *p).collect();
                    head.push(pp);
                    let mut tail = vec![pp];
                    tail.extend(r.iter().enumerate().filter(|(k, _)| *k as f64 > p.1 + 1e-9).map(|(_, p)| *p));
                    let mut after = between(&line, p.0, f64::MAX);
                    let before = between(&line, p.0, -1.0);
                    if length(&before) > length(&after) {
                        after = before;
                    }
                    if length(&head) >= length(&tail) {
                        head.extend(after);
                        head
                    } else {
                        let mut c: Vec<Pt> = after.into_iter().rev().collect();
                        c.extend(tail);
                        c
                    }
                }
                _ => return Err("draw the line from a point on the line".into()),
            };
            if new.len() < 2 || length(&new) <= EPS {
                return Err("reshaping there would leave no line".into());
            }
            let mut parts = parts.clone();
            if let Some(slot) = parts.get_mut(*pi) {
                *slot = new.into_iter().map(|p| p.ll).collect();
            }
            Ok(Reshaped { geometry: Geometry::Line(parts), holes_removed: 0 })
        }
        _ => Err("only lines and polygons can be reshaped".into()),
    }
}

/// A reshaped feature's new shape, and how many holes it lost (left outside its new outer ring).
#[derive(Clone, Debug, PartialEq)]
pub struct Reshaped {
    pub geometry: Geometry,
    pub holes_removed: usize,
}

/// A path traced along a feature's edge, and how far (Web Mercator metres, which exceed ground
/// metres by 1/cos(latitude)) its ends were from the points asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct Traced {
    pub path: Vec<Coord>,
    pub from_m: f64,
    pub to_m: f64,
}

/// The nearest position along a path to `c`: (position, distance).
fn nearest(path: &[Pt], c: Coord) -> Option<(f64, f64, Coord)> {
    path.windows(2)
        .enumerate()
        .filter_map(|(i, w)| {
            let (a, b) = (w.first()?.m, w.get(1)?.m);
            let (t, d) = foot(c, a, b);
            Some((i as f64 + t, d, Coord::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y))))
        })
        .min_by(|x, y| x.1.total_cmp(&y.1))
}

/// Traces along the edge of a line or polygon from the point on it nearest `from` to the point
/// on the same part or ring nearest `to`: along a line between them, round a ring the shorter
/// way.
/// `part`, when given, is the line part or polygon ring to follow (polygons' rings numbered in
/// order, holes after their outer ring), as already chosen on the map.
pub fn trace(g: &Geometry, from: Coord, to: Coord, part: Option<usize>) -> Result<Traced, String> {
    let (f, t) = (lonlat_to_merc(from), lonlat_to_merc(to));
    let closed = matches!(g, Geometry::Polygon(_));
    if !matches!(g, Geometry::Line(_) | Geometry::Polygon(_)) {
        return Err("only lines and polygons can be traced".into());
    }
    // The part or ring passing nearest both points, or the one given.
    let (path, (rf, df, mf), (rt, dt, mt)) = paths_of(g)?
        .into_iter()
        .enumerate()
        .filter(|(i, _)| part.is_none_or(|k| k == *i))
        .filter_map(|(_, p)| {
            let (a, b) = (nearest(&p, f)?, nearest(&p, t)?);
            Some((p, a, b))
        })
        .min_by(|x, y| x.1.1.max(x.2.1).total_cmp(&y.1.1.max(y.2.1)))
        .ok_or("the feature has no edge to trace")?;
    // An end on a vertex is that vertex; one already on the edge (a snapped point) stays as given.
    let end = |r: f64, d: f64, m: Coord, asked: Coord| match vertex_at(&path, r, m) {
        Some(v) => v,
        None if d <= EPS => Pt::of(asked),
        None => Pt::at(m),
    };
    let (a, b) = (end(rf, df, mf, from), end(rt, dt, mt, to));
    let walk = |mid: Vec<Pt>| -> Vec<Pt> { std::iter::once(a).chain(mid).chain(std::iter::once(b)).collect() };
    let pts = if closed {
        let (x, y) = (walk(round_ring(&path, rf, rt, true)), walk(round_ring(&path, rf, rt, false)));
        if length(&x) <= length(&y) { x } else { y }
    } else {
        walk(between(&path, rf, rt))
    };
    Ok(Traced { path: pts.into_iter().map(|p| p.ll).collect(), from_m: df, to_m: dt })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A longitude/latitude.
    fn c(x: f64, y: f64) -> Coord {
        Coord::new(x, y)
    }

    /// A closed anticlockwise rectangle ring.
    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Coord> {
        vec![c(x0, y0), c(x1, y0), c(x1, y1), c(x0, y1), c(x0, y0)]
    }

    /// A polygon geometry's area in square metres (Web Mercator), every part summed.
    fn area(g: &Geometry) -> f64 {
        match g {
            Geometry::Polygon(p) => p.iter().map(|r| polygon_area(r)).sum(),
            _ => 0.0,
        }
    }

    #[test]
    fn a_line_across_a_square_cuts_it_in_two() {
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0)]]);
        let out = cut_polygon(&g, &[c(0.3, -1.0), c(0.3, 2.0)], &mut 0).unwrap().unwrap();
        assert_eq!(out.len(), 2);
        // The larger piece first, and the two make up the whole.
        assert!(area(&out[0]) > area(&out[1]));
        assert!(((area(&out[0]) + area(&out[1])) / area(&g) - 1.0).abs() < 1e-9);
        // The square's own corners keep their exact coordinates.
        let Geometry::Polygon(p) = &out[0] else { panic!() };
        assert!(p[0][0].contains(&c(1.0, 1.0)));
    }

    #[test]
    fn a_line_that_stops_inside_or_misses_cuts_nothing() {
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0)]]);
        assert!(cut_polygon(&g, &[c(0.5, -1.0), c(0.5, 0.5)], &mut 0).unwrap().is_none());
        assert!(cut_polygon(&g, &[c(2.0, -1.0), c(2.0, 2.0)], &mut 0).unwrap().is_none());
        assert!(cut_polygon(&g, &[c(0.5, -1.0)], &mut 0).is_err());
    }

    #[test]
    fn line_ends_a_few_millimetres_short_of_the_edge_still_cut() {
        // About 4 mm inside the bottom and top edges, as a point snapped in another projection.
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0)]]);
        let out = cut_polygon(&g, &[c(0.3, 4e-8), c(0.3, 1.0 - 4e-8)], &mut 0).unwrap().unwrap();
        assert_eq!(out.len(), 2);
        // A centimetre and more short is not on the edge.
        assert!(cut_polygon(&g, &[c(0.3, 2e-7), c(0.3, 1.0 - 2e-7)], &mut 0).unwrap().is_none());
    }

    #[test]
    fn a_zigzag_makes_three_pieces_and_holes_stay_in_theirs() {
        let hole = vec![c(0.7, 0.4), c(0.7, 0.6), c(0.9, 0.6), c(0.9, 0.4), c(0.7, 0.4)];
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0), hole]]);
        let out = cut_polygon(&g, &[c(0.2, -1.0), c(0.2, 2.0), c(0.4, 2.0), c(0.4, -1.0)], &mut 0).unwrap().unwrap();
        assert_eq!(out.len(), 3);
        let total: f64 = out.iter().map(area).sum();
        assert!((total / area(&g) - 1.0).abs() < 1e-9);
        // The hole is in the largest (eastern) piece.
        let Geometry::Polygon(p) = &out[0] else { panic!() };
        assert_eq!(p[0].len(), 2);
    }

    #[test]
    fn a_cut_through_a_hole_divides_the_ring_around_it() {
        let hole = vec![c(0.4, 0.4), c(0.4, 0.6), c(0.6, 0.6), c(0.6, 0.4), c(0.4, 0.4)];
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0), hole]]);
        let out = cut_polygon(&g, &[c(0.5, -1.0), c(0.5, 2.0)], &mut 0).unwrap().unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|g| matches!(g, Geometry::Polygon(p) if p.len() == 1 && p[0].len() == 1)));
        let total: f64 = out.iter().map(area).sum();
        assert!((total / area(&g) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_part_the_line_misses_stays_with_the_largest_piece() {
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0)], vec![square(5.0, 5.0, 6.0, 6.0)]]);
        let out = cut_polygon(&g, &[c(0.5, -1.0), c(0.5, 2.0)], &mut 0).unwrap().unwrap();
        assert_eq!(out.len(), 2);
        assert!(matches!(&out[0], Geometry::Polygon(p) if p.len() == 2));
    }

    #[test]
    fn reshaping_a_square_moves_one_side_out() {
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0)]]);
        let out = reshape(&g, &[c(0.8, 0.2), c(1.5, 0.2), c(1.5, 0.8), c(0.8, 0.8)]).unwrap().geometry;
        let grown = area(&out) / area(&g);
        assert!(grown > 1.29 && grown < 1.31, "{grown}");
        // And in, the larger side kept.
        let out = reshape(&g, &[c(1.2, 0.2), c(0.7, 0.2), c(0.7, 0.8), c(1.2, 0.8)]).unwrap().geometry;
        let shrunk = area(&out) / area(&g);
        assert!(shrunk > 0.81 && shrunk < 0.83, "{shrunk}");
        assert!(reshape(&g, &[c(2.0, 0.0), c(3.0, 0.0)]).is_err());
    }

    #[test]
    fn reshaping_away_the_side_with_a_hole_drops_the_hole_with_it() {
        let hole = vec![c(0.1, 0.4), c(0.1, 0.6), c(0.2, 0.6), c(0.2, 0.4), c(0.1, 0.4)];
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0), hole]]);
        let r = reshape(&g, &[c(0.3, -1.0), c(0.3, 2.0)]).unwrap();
        assert_eq!(r.holes_removed, 1);
        let out = r.geometry;
        let Geometry::Polygon(p) = &out else { panic!() };
        assert_eq!(p[0].len(), 1, "the hole was on the side reshaped away");
        let kept = area(&out) / area(&Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0)]]));
        assert!(kept > 0.69 && kept < 0.71, "{kept}");
    }

    #[test]
    fn a_hole_on_the_kept_side_stays_and_one_crossing_the_new_boundary_is_refused() {
        let hole = vec![c(0.4, 0.4), c(0.4, 0.6), c(0.6, 0.6), c(0.6, 0.4), c(0.4, 0.4)];
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0), hole]]);
        let r = reshape(&g, &[c(1.2, 0.2), c(0.7, 0.2), c(0.7, 0.8), c(1.2, 0.8)]).unwrap();
        assert_eq!(r.holes_removed, 0);
        assert!(matches!(&r.geometry, Geometry::Polygon(p) if p[0].len() == 2));
        // Every vertex of this hole is outside the new boundary, but its top edge crosses it.
        let hole = vec![c(0.4, 0.05), c(0.4, 0.15), c(0.6, 0.15), c(0.6, 0.05), c(0.4, 0.05)];
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0), hole]]);
        let finger = [c(-0.1, 0.2), c(0.45, 0.2), c(0.45, 0.1), c(0.55, 0.1), c(0.55, 0.2), c(1.1, 0.2)];
        assert!(reshape(&g, &finger).is_err());
    }

    #[test]
    fn a_reshape_into_another_part_of_the_feature_is_refused() {
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0)], vec![square(2.0, 0.0, 3.0, 1.0)]]);
        // Out from the first part's east side, across the second part.
        assert!(reshape(&g, &[c(0.8, 0.2), c(2.5, 0.2), c(2.5, 0.8), c(0.8, 0.8)]).is_err());
        // Out into the gap between them is fine.
        assert!(reshape(&g, &[c(0.8, 0.2), c(1.5, 0.2), c(1.5, 0.8), c(0.8, 0.8)]).is_ok());
    }

    #[test]
    fn absurd_longitudes_are_handled_without_a_panic() {
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0)]]);
        for lon in [1e11, -1e11, 1e300] {
            let _ = cut_polygon(&g, &[c(lon, -1.0), c(lon, 2.0)], &mut 0);
            let _ = reshape(&g, &[c(lon, -1.0), c(lon, 2.0)]);
        }
    }

    #[test]
    fn a_ring_far_off_the_earth_is_refused_quickly() {
        // 50,000 vertices at one latitude and an absurd longitude: refused before any grid work.
        let ring: Vec<Coord> = (0..50_000).map(|i| c(1e11 + i as f64, 10.0)).chain(std::iter::once(c(1e11, 10.0))).collect();
        let g = Geometry::Polygon(vec![vec![ring]]);
        assert!(cut_polygon(&g, &[c(0.0, 0.0), c(1.0, 1.0)], &mut 0).is_err());
        assert!(reshape(&g, &[c(0.0, 0.0), c(1.0, 1.0)]).is_err());
    }

    #[test]
    fn a_ring_stored_open_is_traced_and_cut_as_closed() {
        let open = vec![c(0.0, 0.0), c(1.0, 0.0), c(1.0, 1.0), c(0.0, 1.0)];
        let g = Geometry::Polygon(vec![vec![open]]);
        // Round the corner at (0, 0), over the closing edge.
        let t = trace(&g, c(0.0, 0.5), c(0.5, 0.0), None).unwrap();
        assert_eq!(t.path, vec![c(0.0, 0.5), c(0.0, 0.0), c(0.5, 0.0)]);
        assert_eq!(cut_polygon(&g, &[c(0.5, -1.0), c(0.5, 2.0)], &mut 0).unwrap().map(|p| p.len()), Some(2));
    }

    #[test]
    fn reshaping_a_line_replaces_a_stretch_or_an_end() {
        let g = Geometry::Line(vec![vec![c(0.0, 0.0), c(1.0, 0.0), c(2.0, 0.0), c(3.0, 0.0)]]);
        let out = reshape(&g, &[c(0.5, -0.1), c(0.5, 1.0), c(2.5, 1.0), c(2.5, -0.1)]).unwrap().geometry;
        let Geometry::Line(p) = &out else { panic!() };
        assert_eq!(p[0].first(), Some(&c(0.0, 0.0)));
        assert_eq!(p[0].last(), Some(&c(3.0, 0.0)));
        assert!(!p[0].contains(&c(1.0, 0.0)) && p[0].len() == 6);
        // From a point near the end, out to somewhere new: the end is replaced.
        let out = reshape(&g, &[c(2.5, 0.0), c(2.5, 2.0)]).unwrap().geometry;
        let Geometry::Line(p) = &out else { panic!() };
        assert_eq!(p[0].first(), Some(&c(0.0, 0.0)));
        assert!(p[0].last().is_some_and(|e| (e.y - 2.0).abs() < 1e-9));
    }

    #[test]
    fn tracing_follows_a_ring_the_short_way() {
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0)]]);
        let t = trace(&g, c(0.5, 0.0), c(1.0, 0.5), None).unwrap();
        assert_eq!(t.path.len(), 3);
        assert_eq!(t.path[1], c(1.0, 0.0));
        let t = trace(&g, c(0.5, 0.0), c(0.0, 0.5), None).unwrap();
        assert_eq!(t.path[1], c(0.0, 0.0));
        let l = Geometry::Line(vec![vec![c(0.0, 0.0), c(1.0, 0.0), c(2.0, 0.0)]]);
        let t = trace(&l, c(1.8, 0.0), c(0.2, 0.0), None).unwrap();
        assert_eq!(t.path.len(), 3);
        assert!(t.from_m < 1e-6);
    }

    #[test]
    fn tracing_follows_the_part_given_even_when_another_is_nearer() {
        let g = Geometry::Polygon(vec![vec![square(0.0, 0.0, 1.0, 1.0)], vec![square(5.0, 0.0, 6.0, 1.0)]]);
        let t = trace(&g, c(0.5, 0.0), c(1.0, 0.5), Some(1)).unwrap();
        assert_eq!(t.path.len(), 2, "the short way along ring 1: {:?}", t.path);
        assert_eq!(t.path[0], c(5.0, 0.0));
        assert!((t.path[1].x - 5.0).abs() < 1e-9 && (t.path[1].y - 0.5).abs() < 1e-9, "{:?}", t.path);
    }
}
