//! Point labels turned square to a nearby line (#2263): beach names fanning out from a coast.
//! Everything here is on the screen, in points, y down.

use rustgis_map::SquareSide;

/// Segments in each stretch of a [`Run`] the nearest-place search skips as a whole.
const STRETCH: usize = 32;

/// A line on the screen, as the nearest-place search and the course read it: worked out once a
/// frame, so each label searches only the stretches near it and finds places along it by halves.
pub(crate) struct Run {
    pts: Vec<(f32, f32)>,
    /// How far along the run each place is.
    at: Vec<f32>,
    /// Its last place is its first (a polygon's outline): the course goes on round it.
    closed: bool,
    /// Each stretch's first segment and box ([min x, min y, max x, max y]).
    stretches: Vec<(usize, [f32; 4])>,
}

impl Run {
    /// A run of these places; `None` with fewer than two, or one that isn't all numbers.
    pub(crate) fn new(pts: Vec<(f32, f32)>) -> Option<Run> {
        if pts.len() < 2 || pts.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
            return None;
        }
        let mut at = Vec::with_capacity(pts.len());
        let mut along = 0.0;
        at.push(along);
        for w in pts.windows(2) {
            along += (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1);
            at.push(along);
        }
        let stretches = (0..pts.len() - 1)
            .step_by(STRETCH)
            .map(|i| {
                let part = &pts[i..=(i + STRETCH).min(pts.len() - 1)];
                let b = part
                    .iter()
                    .fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p.0), b[1].min(p.1), b[2].max(p.0), b[3].max(p.1)]);
                (i, b)
            })
            .collect();
        let closed = pts.len() > 3 && pts.first() == pts.last();
        Some(Run { pts, at, closed, stretches })
    }

    fn length(&self) -> f32 {
        self.at.last().copied().unwrap_or_default()
    }

    /// The place `s` along the run (clamped to its ends; round a closed run).
    fn point_at(&self, s: f32) -> (f32, f32) {
        let total = self.length();
        let s = if self.closed && total > 0.0 { s.rem_euclid(total) } else { s.clamp(0.0, total) };
        let i = self.at.partition_point(|a| *a < s).clamp(1, self.pts.len() - 1);
        let (a, b, l) = (self.pts[i - 1], self.pts[i], self.at[i] - self.at[i - 1]);
        let t = if l > 0.0 { (s - self.at[i - 1]) / l } else { 0.0 };
        (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1))
    }
}

/// The way a label square to the nearest of `runs` runs from point `p`: the unit normal of that
/// line, its course taken over `reach` of it either side of the nearest place (about a text
/// height, so detail smaller than the letters doesn't turn the label), to the side `side` asks.
/// `None` when no line is within `within` of the point.
pub(crate) fn normal(runs: &[Run], p: (f32, f32), within: f32, reach: f32, side: SquareSide) -> Option<(f32, f32)> {
    // The nearest place: its distance, run, distance along the run and foot.
    let mut best: Option<(f32, usize, f32, (f32, f32))> = None;
    for (r, run) in runs.iter().enumerate() {
        for &(first, b) in &run.stretches {
            // A stretch whose box is farther than the nearest place so far, or than `within`,
            // has none nearer.
            let gap = (b[0] - p.0).max(p.0 - b[2]).max(0.0).hypot((b[1] - p.1).max(p.1 - b[3]).max(0.0));
            if gap > best.map_or(within, |b| b.0.min(within)) {
                continue;
            }
            for i in first..(first + STRETCH).min(run.pts.len() - 1) {
                let (a, b) = (run.pts[i], run.pts[i + 1]);
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let len = dx.hypot(dy);
                if !(len.is_finite() && len > 0.0) {
                    continue;
                }
                let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (len * len)).clamp(0.0, 1.0);
                let foot = (a.0 + t * dx, a.1 + t * dy);
                let d = (p.0 - foot.0).hypot(p.1 - foot.1);
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, r, run.at[i] + t * len, foot));
                }
            }
        }
    }
    let (d, r, s, foot) = best.filter(|b| b.0 <= within)?;
    let (tx, ty) = course(&runs[r], s, reach.max(1.0))?;
    let len = tx.hypot(ty);
    // Left of the line as it runs (y down).
    let left = (ty / len, -tx / len);
    let right = (-left.0, -left.1);
    Some(match side {
        SquareSide::Left => left,
        SquareSide::Right => right,
        // Away from the point's side; a point on the line goes left.
        SquareSide::Away => {
            let on_left = (p.0 - foot.0) * left.0 + (p.1 - foot.1) * left.1 > 0.0;
            if d > 0.5 && on_left { right } else { left }
        }
    })
}

/// Where a label `width` wide starting `start` out from point `p` along `n` is centred, and its
/// angle (radians clockwise), kept upright: past 90° it reads from its far end back towards the
/// point, in the same place. `across` moves it down as the text reads.
pub(crate) fn place(p: (f32, f32), n: (f32, f32), start: f32, width: f32, across: f32) -> (f32, f32, f32) {
    let angle = rustgis_tiles::symbols::upright(n.1.atan2(n.0));
    let out = start + width / 2.0;
    let down = (-angle.sin(), angle.cos());
    (p.0 + n.0 * out + down.0 * across, p.1 + n.1 * out + down.1 * across, angle)
}

/// The line's course at `s` along `run`: the direction its places within `reach` either way
/// line up in (their principal axis, so teeth and wiggles smaller than that cancel out), as it
/// runs. `None` where they don't spread out.
fn course(run: &Run, s: f32, reach: f32) -> Option<(f32, f32)> {
    const SAMPLES: usize = 33;
    let at: Vec<(f32, f32)> = (0..SAMPLES).map(|i| run.point_at(s - reach + 2.0 * reach * i as f32 / (SAMPLES - 1) as f32)).collect();
    let n = SAMPLES as f32;
    let (mx, my) = at.iter().fold((0.0, 0.0), |(x, y), p| (x + p.0 / n, y + p.1 / n));
    let (sxx, syy, sxy) = at.iter().fold((0.0, 0.0, 0.0), |(a, b, c), p| {
        let (dx, dy) = (p.0 - mx, p.1 - my);
        (a + dx * dx, b + dy * dy, c + dx * dy)
    });
    if sxx + syy <= 1e-9 || !(sxx + syy).is_finite() {
        return None;
    }
    let t = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let (cx, cy) = (t.cos(), t.sin());
    // As the line runs: its later places ahead.
    let (first, last) = (at[0], at[SAMPLES - 1]);
    let ahead = (last.0 - first.0) * cx + (last.1 - first.1) * cy;
    Some(if ahead < 0.0 { (-cx, -cy) } else { (cx, cy) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, PI};

    fn runs(lines: &[Vec<(f32, f32)>]) -> Vec<Run> {
        lines.iter().cloned().filter_map(Run::new).collect()
    }

    fn deg(a: f32) -> f32 {
        a.to_degrees().rem_euclid(360.0)
    }

    #[test]
    fn a_straight_east_west_line_gives_a_vertical_label_starting_at_the_point() {
        let line = vec![vec![(0.0, 100.0), (400.0, 100.0)]];
        // A point above the line (north, on land): the text runs down, across it, away from
        // the point's side.
        let n = normal(&runs(&line), (200.0, 90.0), 20.0, 12.0, SquareSide::Away).unwrap();
        assert!((n.0).abs() < 1e-5 && (n.1 - 1.0).abs() < 1e-5, "{n:?}");
        let (x, y, a) = place((200.0, 90.0), n, 3.0, 50.0, 0.0);
        assert!((deg(a) - 270.0).abs() < 1e-3 || (deg(a) - 90.0).abs() < 1e-3, "{}", deg(a));
        // Its near end at the point plus its start: centred half its width beyond.
        assert!((x - 200.0).abs() < 1e-3 && (y - (90.0 + 3.0 + 25.0)).abs() < 1e-3, "{x} {y}");
        // Below it: up.
        let n = normal(&runs(&line), (200.0, 110.0), 20.0, 12.0, SquareSide::Away).unwrap();
        assert!((n.1 + 1.0).abs() < 1e-5);
        // Left and right follow the line's direction (east: left is north).
        assert!((normal(&runs(&line), (200.0, 110.0), 20.0, 12.0, SquareSide::Left).unwrap().1 + 1.0).abs() < 1e-5);
        assert!((normal(&runs(&line), (200.0, 90.0), 20.0, 12.0, SquareSide::Right).unwrap().1 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn points_round_a_semicircular_bay_fan_out_along_the_radii() {
        // A bay's shore: a half circle round (0, 0), radius 100, its beaches just outside it.
        let shore: Vec<(f32, f32)> = (0..=180).map(|i| (i as f32).to_radians()).map(|t| (100.0 * t.cos(), 100.0 * t.sin())).collect();
        for i in [15, 45, 90, 135, 165] {
            let t = (i as f32).to_radians();
            let p = (105.0 * t.cos(), 105.0 * t.sin());
            let n = normal(&runs(std::slice::from_ref(&shore)), p, 20.0, 12.0, SquareSide::Away).unwrap();
            // Inward, into the bay, along the radius.
            assert!((n.0 + t.cos()).abs() < 1e-3 && (n.1 + t.sin()).abs() < 1e-3, "{i}°: {n:?}");
            let (.., a) = place(p, n, 0.0, 40.0, 0.0);
            assert!(a.abs() <= FRAC_PI_2 + 1e-3, "upright at {i}°: {}", a.to_degrees());
            // The angle is the radius's, either way round.
            let radius = (t + PI).rem_euclid(PI);
            assert!((a.rem_euclid(PI) - radius).abs() < 1e-3 || (a.rem_euclid(PI) - radius).abs() > PI - 1e-3, "{i}°");
        }
    }

    #[test]
    fn a_point_beyond_within_has_no_square() {
        let line = vec![vec![(0.0, 100.0), (400.0, 100.0)]];
        assert!(normal(&runs(&line), (200.0, 60.0), 20.0, 12.0, SquareSide::Away).is_none());
        assert!(normal(&[], (200.0, 60.0), 20.0, 12.0, SquareSide::Away).is_none());
    }

    #[test]
    fn a_zigzag_within_a_text_height_gives_its_smoothed_course() {
        // Teeth 2 apart and 2 high along an east–west course: their own segments are at ±63°.
        let zigzag: Vec<(f32, f32)> = (0..=200).map(|i| (i as f32, 100.0 + if i % 2 == 0 { 0.0 } else { 2.0 })).collect();
        for x in [41.0, 100.5, 151.3, 77.7] {
            let n = normal(&runs(std::slice::from_ref(&zigzag)), (x, 90.0), 20.0, 12.0, SquareSide::Away).unwrap();
            assert!(n.0.abs() < 0.05 && n.1 > 0.99, "at {x}: {n:?}");
        }
    }

    #[test]
    fn a_long_line_and_a_closed_ring_are_searched_by_stretches() {
        // 1000 segments east-west: the nearest place is found in the stretch near the point.
        let long: Vec<(f32, f32)> = (0..=1000).map(|i| (i as f32, 100.0)).collect();
        let n = normal(&runs(&[long]), (777.3, 110.0), 20.0, 12.0, SquareSide::Away).unwrap();
        assert!((n.1 + 1.0).abs() < 1e-5, "{n:?}");
        // A square ring, closed: next to the corner where it starts, the course goes on round
        // it (45 degrees), rather than stopping at its end.
        let ring = vec![(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0), (0.0, 0.0)];
        let n = normal(&runs(&[ring]), (-3.0, -3.0), 20.0, 12.0, SquareSide::Away).unwrap();
        assert!((n.0 - n.1).abs() < 1e-3 && n.0 > 0.0, "{n:?}");
    }
}
