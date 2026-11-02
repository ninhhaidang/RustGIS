//! Markers and hash marks along a line (a stroke's [`LineMarkers`]), as QGIS's marker and hashed
//! lines draw them. They are placed along each part of the line as a whole, in device pixels, so
//! an interval counts from the line's start and its middle is the whole line's however much of it
//! shows; only the marks near the image are drawn.

use crate::Canvas;
use rustgis_map::{LineMark, LineMarkers, Placements, Rgba};
use tiny_skia::{FillRule, LineCap, Path, PathBuilder, Stroke, Transform};

use crate::{marker_path, marker_reach, paint};

/// The most marks one stroke draws on one feature, so an interval far shorter than the line
/// seen whole can't take the time.
const MAX_MARKS: usize = 20_000;

/// A part of a line in device pixels, and how far along it each vertex is. Vertices on top of the
/// one before are left out, so every segment has a direction.
pub(crate) struct Run {
    pts: Vec<(f64, f64)>,
    at: Vec<f64>,
}

impl Run {
    /// The run through `pts`, or `None` when it has no length.
    pub(crate) fn new(pts: impl IntoIterator<Item = (f64, f64)>) -> Option<Run> {
        let (mut out, mut at) = (Vec::<(f64, f64)>::new(), Vec::new());
        for p in pts {
            if !(p.0.is_finite() && p.1.is_finite()) {
                continue;
            }
            match out.last() {
                Some(q) => {
                    let d = (p.0 - q.0).hypot(p.1 - q.1);
                    if d > 1e-9 {
                        at.push(at.last().copied().unwrap_or(0.0) + d);
                        out.push(p);
                    }
                }
                None => {
                    at.push(0.0);
                    out.push(p);
                }
            }
        }
        (out.len() >= 2).then_some(Run { pts: out, at })
    }

    fn len(&self) -> f64 {
        self.at.last().copied().unwrap_or(0.0)
    }

    /// The segment `d` along the line falls on (the first or the last past its ends).
    fn segment(&self, d: f64) -> usize {
        let i = self.at.partition_point(|a| *a <= d);
        i.saturating_sub(1).min(self.pts.len().saturating_sub(2))
    }

    /// The unit direction of segment `i`.
    fn dir(&self, i: usize) -> (f64, f64) {
        match (self.pts.get(i), self.pts.get(i + 1)) {
            (Some(a), Some(b)) => {
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let l = dx.hypot(dy);
                if l > 0.0 { (dx / l, dy / l) } else { (1.0, 0.0) }
            }
            _ => (1.0, 0.0),
        }
    }

    /// The point `d` along the line (kept on it) and the direction there.
    fn at_dist(&self, d: f64) -> ((f64, f64), (f64, f64)) {
        let d = d.clamp(0.0, self.len());
        let i = self.segment(d);
        let (Some(a), Some(a0)) = (self.pts.get(i), self.at.get(i)) else { return ((0.0, 0.0), (1.0, 0.0)) };
        let dir = self.dir(i);
        let t = d - a0;
        ((a.0 + dir.0 * t, a.1 + dir.1 * t), dir)
    }

    /// The direction from `stretch / 2` before `d` to as far after it, or the segment's where that
    /// has no length.
    fn averaged(&self, d: f64, stretch: f64) -> (f64, f64) {
        let ((a, _), (b, _)) = (self.at_dist(d - stretch / 2.0), self.at_dist(d + stretch / 2.0));
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l = dx.hypot(dy);
        if l > 1e-9 { (dx / l, dy / l) } else { self.at_dist(d).1 }
    }

    /// The direction at vertex `i`: halfway between its segments' at an inner vertex.
    fn vertex_dir(&self, i: usize) -> (f64, f64) {
        let after = self.dir(i.min(self.pts.len().saturating_sub(2)));
        if i == 0 || i + 1 >= self.pts.len() {
            return after;
        }
        let before = self.dir(i - 1);
        let (x, y) = (before.0 + after.0, before.1 + after.1);
        let l = x.hypot(y);
        if l > 1e-9 { (x / l, y / l) } else { before }
    }
}

/// Where marks show: the image, widened by how far a mark reaches, in device pixels.
#[derive(Clone, Copy)]
struct Window {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Window {
    fn contains(&self, p: (f64, f64)) -> bool {
        p.0 >= self.x0 && p.0 <= self.x1 && p.1 >= self.y0 && p.1 <= self.y1
    }

    /// The stretch of the segment from `a` to `b` inside it, as fractions of the way from `a`
    /// (Liang–Barsky); `None` when it passes outside.
    fn meets(&self, a: (f64, f64), b: (f64, f64)) -> Option<(f64, f64)> {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        for (p, q) in [(-dx, a.0 - self.x0), (dx, self.x1 - a.0), (-dy, a.1 - self.y0), (dy, self.y1 - a.1)] {
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
        Some((t0, t1))
    }
}

/// The places along `run` a stroke's marks go, in turn: each point and the line's direction
/// there. At most `room`, and only those in `near`; at every interval, without visiting the
/// segments outside it.
fn places(run: &Run, m: &LineMarkers, ppp: f64, near: Window, room: usize) -> Vec<((f64, f64), (f64, f64))> {
    let mut out = Vec::new();
    let len = run.len();
    let along = f64::from(m.offset_along) * ppp;
    let push = |out: &mut Vec<_>, p: ((f64, f64), (f64, f64))| {
        if out.len() < room && near.contains(p.0) {
            out.push(p);
        }
    };
    let last = run.pts.len().saturating_sub(1);
    if m.at.has(Placements::INTERVAL) {
        let step = f64::from(m.interval).max(f64::from(rustgis_map::symbology::MIN_MARKER_INTERVAL)) * ppp;
        let stretch = f64::from(m.average_angle) * ppp;
        // Segment by segment, skipping those off the image without visiting their marks.
        for i in 0..last {
            let (Some(a), Some(b), Some(d0), Some(d1)) = (run.pts.get(i), run.pts.get(i + 1), run.at.get(i), run.at.get(i + 1)) else {
                continue;
            };
            // Only the stretch of it near the image.
            let Some((t0, t1)) = near.meets(*a, *b) else { continue };
            let (from, to) = (d0 + (d1 - d0) * t0, d0 + (d1 - d0) * t1);
            let mut k = ((from - along) / step).ceil().max(0.0);
            while out.len() < room {
                let d = along + k * step;
                // Each mark on one segment: past its end it's the next one's (the last keeps its end).
                if d > to || (d == *d1 && i + 1 < last) {
                    break;
                }
                let (p, dir) = run.at_dist(d);
                let dir = if stretch > 0.0 { run.averaged(d, stretch) } else { dir };
                push(&mut out, (p, dir));
                k += 1.0;
            }
        }
    }
    let vertices = m.at.has(Placements::VERTICES);
    if vertices || m.at.has(Placements::INNER_VERTICES) {
        for i in 0..=last {
            if !vertices && (i == 0 || i == last) {
                continue;
            }
            let (Some(p), Some(d)) = (run.pts.get(i), run.at.get(i)) else { continue };
            if along == 0.0 { push(&mut out, (*p, run.vertex_dir(i))) } else { push(&mut out, run.at_dist(d + along)) }
        }
    }
    // A first or last vertex already marked as a vertex isn't marked again.
    if m.at.has(Placements::FIRST) && !vertices {
        push(&mut out, run.at_dist(along));
    }
    if m.at.has(Placements::LAST) && !vertices {
        let (p, dir) = run.at_dist(len - along);
        push(&mut out, (p, if along == 0.0 { run.dir(last.saturating_sub(1)) } else { dir }));
    }
    if m.at.has(Placements::MIDDLE) {
        push(&mut out, run.at_dist(len / 2.0 + along));
    }
    if m.at.has(Placements::SEGMENT_MIDDLES) {
        for i in 0..last {
            let (Some(d0), Some(d1)) = (run.at.get(i), run.at.get(i + 1)) else { continue };
            push(&mut out, run.at_dist((d0 + d1) / 2.0 + along));
        }
    }
    out
}

/// Draws a stroke's marks (`m`) along each of `runs`: in `color`, `width` their outline's or a hash
/// mark's line's, `offset` points to the right of the line. `ppp` is pixels per point.
pub(crate) fn draw(pm: &mut Canvas, runs: &[Run], m: &LineMarkers, color: Rgba, width: f32, offset: f32, ppp: f32) {
    let outline = m.outline.filter(|c| c.a() > 0 && width > 0.0);
    let hash = matches!(m.mark, LineMark::Hash);
    if (hash && (width <= 0.0 || color.a() == 0)) || (!hash && color.a() == 0 && outline.is_none()) {
        return;
    }
    let size = m.size.max(0.0) * ppp;
    // The mark at the origin, the line running along x: a marker, or a hash mark across it.
    let shape: Option<Path> = match m.mark {
        LineMark::Marker(shape) => marker_path(0.0, 0.0, size.max(ppp) / 2.0, shape),
        LineMark::Hash => {
            let mut pb = PathBuilder::new();
            pb.move_to(0.0, -size / 2.0);
            pb.line_to(0.0, size / 2.0);
            pb.finish()
        }
    };
    let Some(shape) = shape else { return };
    let stroke = Stroke { width: (width * ppp).max(0.5), line_cap: LineCap::Butt, ..Stroke::default() };
    let offset = f64::from(offset * ppp);
    // A character can reach past its size (a shape stays within it).
    let extent = match m.mark {
        LineMark::Marker(shape) => 2.0 * marker_reach(shape).max(0.5),
        LineMark::Hash => 1.0,
    };
    let reach = f64::from(size * extent + width * ppp) + offset.abs() + 2.0;
    let (w, h) = (f64::from(pm.width()), f64::from(pm.height()));
    let near = Window { x0: -reach, y0: -reach, x1: w + reach, y1: h + reach };
    let mut room = MAX_MARKS;
    for run in runs {
        for ((x, y), (dx, dy)) in places(run, m, f64::from(ppp), near, room) {
            room = room.saturating_sub(1);
            // Shifted to the right of the line's direction (on screen, y down).
            let (x, y) = (x - dy * offset, y + dx * offset);
            let turn = if m.rotate { dy.atan2(dx).to_degrees() as f32 } else { 0.0 } + m.angle;
            let t = Transform::from_rotate(turn).post_translate(x as f32, y as f32);
            if hash {
                pm.stroke_path(&shape, &paint(color, 1.0), &stroke, t, None);
                continue;
            }
            if color.a() > 0 {
                pm.fill_path(&shape, &paint(color, 1.0), FillRule::Winding, t, None);
            }
            if let Some(o) = outline {
                pm.stroke_path(&shape, &paint(o, 1.0), &stroke, t, None);
            }
        }
        if room == 0 {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: Window = Window { x0: -1e12, y0: -1e12, x1: 1e12, y1: 1e12 };

    fn run(pts: &[(f64, f64)]) -> Run {
        Run::new(pts.iter().copied()).unwrap()
    }

    fn at(r: &Run, m: LineMarkers) -> Vec<(f64, f64)> {
        places(r, &m, 1.0, ALL, MAX_MARKS).into_iter().map(|(p, _)| p).collect()
    }

    #[test]
    fn marks_go_at_every_interval_from_the_start_and_at_each_place_asked() {
        let r = run(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]);
        let every = LineMarkers { interval: 5.0, ..LineMarkers::default() };
        assert_eq!(at(&r, every), vec![(0.0, 0.0), (5.0, 0.0), (10.0, 0.0), (10.0, 5.0), (10.0, 10.0)]);
        let shifted = LineMarkers { interval: 5.0, offset_along: 2.0, ..LineMarkers::default() };
        assert_eq!(at(&r, shifted), vec![(2.0, 0.0), (7.0, 0.0), (10.0, 2.0), (10.0, 7.0)]);
        let ends = LineMarkers { at: Placements::FIRST.with(Placements::LAST), ..LineMarkers::default() };
        assert_eq!(at(&r, ends), vec![(0.0, 0.0), (10.0, 10.0)]);
        let middle = LineMarkers { at: Placements::MIDDLE, ..LineMarkers::default() };
        assert_eq!(at(&r, middle), vec![(10.0, 0.0)]);
        let inner = LineMarkers { at: Placements::INNER_VERTICES.with(Placements::SEGMENT_MIDDLES), ..LineMarkers::default() };
        assert_eq!(at(&r, inner), vec![(10.0, 0.0), (5.0, 0.0), (10.0, 5.0)]);
        // A vertex mark doesn't double the first and last.
        let vertices = LineMarkers { at: Placements::VERTICES.with(Placements::FIRST), ..LineMarkers::default() };
        assert_eq!(at(&r, vertices).len(), 3);
    }

    #[test]
    fn marks_turn_with_the_line_and_halfway_at_a_bend() {
        let r = run(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]);
        let m = LineMarkers { at: Placements::VERTICES, ..LineMarkers::default() };
        let dirs: Vec<(f64, f64)> = places(&r, &m, 1.0, ALL, 10).into_iter().map(|(_, d)| d).collect();
        let h = std::f64::consts::FRAC_1_SQRT_2;
        assert_eq!(dirs.len(), 3);
        assert!((dirs[0].0 - 1.0).abs() < 1e-12 && dirs[0].1.abs() < 1e-12);
        assert!((dirs[1].0 - h).abs() < 1e-12 && (dirs[1].1 - h).abs() < 1e-12);
        assert!(dirs[2].0.abs() < 1e-12 && (dirs[2].1 - 1.0).abs() < 1e-12);
        // Averaged over a stretch, a mark right at the bend turns halfway too.
        let avg = LineMarkers { interval: 10.0, average_angle: 4.0, ..LineMarkers::default() };
        let d = places(&r, &avg, 1.0, ALL, 10);
        assert!((d[1].1.0 - h).abs() < 1e-12 && (d[1].1.1 - h).abs() < 1e-12);
    }

    #[test]
    fn marks_off_the_image_are_skipped_without_visiting_them() {
        // A line a hundred million pixels long, a mark every pixel: only those near x 0..100 count.
        let r = run(&[(-5e7, 0.0), (5e7, 0.0)]);
        let m = LineMarkers { interval: 1.0, ..LineMarkers::default() };
        let near = Window { x0: 0.0, y0: -10.0, x1: 100.0, y1: 10.0 };
        assert!(near.meets((-5e7, 0.0), (5e7, 0.0)).is_some() && near.meets((-5e7, 20.0), (5e7, 20.0)).is_none());
        let got = places(&r, &m, 1.0, near, MAX_MARKS);
        assert_eq!(got.len(), 101);
        // And however many show, no more than the room.
        let all = places(&r, &m, 1.0, ALL, MAX_MARKS);
        assert_eq!(all.len(), MAX_MARKS);
    }

    #[test]
    fn a_run_without_length_has_no_marks() {
        assert!(Run::new([(1.0, 1.0), (1.0, 1.0)]).is_none());
        assert!(Run::new([(f64::NAN, 0.0), (1.0, 1.0)]).is_none());
    }
}
