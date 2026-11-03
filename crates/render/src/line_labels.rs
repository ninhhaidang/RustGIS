//! Labels along lines, in screen points: straight and turned to the line ([`Line::parallel`]), or each
//! character turned with it where it bends ([`Line::curved`]), at the line's middle and kept upright,
//! as QGIS's Parallel and Curved line placements put text on the line.

/// The most a curved label's text turns from one character to the next (radians): QGIS's
/// default limit, 25°. Past it, where a line bends sharply, the label is left out.
pub(super) const MAX_TURN: f32 = 0.436;

/// The most characters a label curves along a line: a longer one is placed straight, so a long
/// attribute value on many lines doesn't lay out, and draw, character by character.
pub(super) const MAX_CURVED_CHARS: usize = 200;

/// Length of curved text including spacing, with no trailing gap or backward character step.
pub(super) fn curved_len(advances: &[f32], gap: f32) -> f32 {
    let step = |adv: f32| (adv + gap).max(0.0);
    advances.iter().map(|a| step(*a)).sum::<f32>() - advances.last().map_or(0.0, |a| step(*a) - a)
}

/// A line in screen points, with how far along it each vertex is.
#[derive(Debug)]
struct Path {
    pts: Vec<(f32, f32)>,
    along: Vec<f32>,
}

impl Path {
    /// The finite vertices of `line` (repeats dropped), reversed when `reversed`; `None` for
    /// one with fewer than two.
    fn new(line: &[(f32, f32)], reversed: bool) -> Option<Path> {
        let mut pts: Vec<(f32, f32)> = Vec::with_capacity(line.len());
        for p in line.iter().copied().filter(|p| p.0.is_finite() && p.1.is_finite()) {
            if pts.last() != Some(&p) {
                pts.push(p);
            }
        }
        if reversed {
            pts.reverse();
        }
        let mut along = Vec::with_capacity(pts.len());
        let mut d = 0.0f32;
        for (i, p) in pts.iter().enumerate() {
            if let Some(q) = i.checked_sub(1).and_then(|j| pts.get(j)) {
                d += (p.0 - q.0).hypot(p.1 - q.1);
            }
            along.push(d);
        }
        (pts.len() >= 2 && d.is_finite() && d > 0.0).then_some(Path { pts, along })
    }

    fn length(&self) -> f32 {
        self.along.last().copied().unwrap_or(0.0)
    }

    /// The point `d` along the line; before its start or past its end, on the first or last
    /// segment carried on.
    fn at(&self, d: f32) -> Option<(f32, f32)> {
        let n = self.pts.len();
        // The segment holding `d`: the first or last one for a place off the line's ends.
        let i = self.along.partition_point(|a| *a < d).clamp(1, n - 1);
        let (a, b) = (self.pts.get(i - 1)?, self.pts.get(i)?);
        let (da, db) = (self.along.get(i - 1)?, self.along.get(i)?);
        let t = if db > da { (d - da) / (db - da) } else { 0.0 };
        Some((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t))
    }
}

/// The stretches of a line in screen points that are in a view `w` × `h` points: cut where it
/// crosses the view's edges and at points out of sight (`None`, or of no number), each with two
/// points or more.
pub(super) fn in_view(line: impl Iterator<Item = Option<(f32, f32)>>, w: f32, h: f32) -> Vec<Vec<(f32, f32)>> {
    let mut runs: Vec<Vec<(f32, f32)>> = Vec::new();
    let mut run: Vec<(f32, f32)> = Vec::new();
    let mut prev: Option<(f32, f32)> = None;
    let end = |run: &mut Vec<(f32, f32)>, runs: &mut Vec<Vec<(f32, f32)>>| {
        let run = std::mem::take(run);
        if run.len() >= 2 {
            runs.push(run);
        }
    };
    for p in line {
        let p = p.filter(|p| p.0.is_finite() && p.1.is_finite());
        match (prev, p) {
            (Some(a), Some(b)) => match clip(a, b, w, h) {
                Some((a, b, cut_end)) => {
                    if run.last() != Some(&a) {
                        end(&mut run, &mut runs);
                        run.push(a);
                    }
                    run.push(b);
                    // Left the view: what comes back in is another stretch.
                    if cut_end {
                        end(&mut run, &mut runs);
                    }
                }
                None => end(&mut run, &mut runs),
            },
            _ => end(&mut run, &mut runs),
        }
        prev = p;
    }
    end(&mut run, &mut runs);
    runs
}

/// The part of the segment `a`–`b` inside a view `w` × `h` points (Liang–Barsky), and whether it
/// stops short of `b`; `None` when none of it is.
fn clip(a: (f32, f32), b: (f32, f32), w: f32, h: f32) -> Option<((f32, f32), (f32, f32), bool)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, q) in [(-dx, a.0), (dx, w - a.0), (-dy, a.1), (dy, h - a.1)] {
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
        }
    }
    if t0.is_nan() || t1.is_nan() || t0 > t1 {
        return None;
    }
    let at = |t: f32| (a.0 + dx * t, a.1 + dy * t);
    Some((if t0 > 0.0 { at(t0) } else { a }, if t1 < 1.0 { at(t1) } else { b }, t1 < 1.0))
}

/// The angle (radians, clockwise) from `a` to `b`.
fn angle(a: (f32, f32), b: (f32, f32)) -> f32 {
    (b.1 - a.1).atan2(b.0 - a.0)
}

/// A line in screen points as labels go along it: its path each way, built once for all the
/// labels (and repeats) placed along it.
#[derive(Debug)]
pub(super) struct Line {
    fwd: Path,
    rev: Path,
}

impl Line {
    /// `None` for a line of fewer than two distinct points.
    pub(super) fn new(line: &[(f32, f32)]) -> Option<Line> {
        Some(Line { fwd: Path::new(line, false)?, rev: Path::new(line, true)? })
    }

    /// Upright local direction at a preview anchor, even when the text cannot fit there.
    pub(super) fn direction(&self, at: f32) -> Option<f32> {
        let (a, b) = (self.fwd.at(at - 0.5)?, self.fwd.at(at + 0.5)?);
        Some(rustgis_tiles::symbols::upright(angle(a, b)))
    }

    /// Its length in screen points.
    pub(super) fn length(&self) -> f32 {
        self.fwd.length()
    }

    /// The line as text `len` points long, centred `at` points along it as drawn (its middle when
    /// `None`), reads along it, left to right: reversed when that stretch runs leftward, so the
    /// text is never upside down; with where its centre is along the path returned (from its other
    /// end when reversed). A vertical stretch reads downward, as street labels turned with the map
    /// do ([`rustgis_tiles::symbols::upright`]), whichever way the line was drawn. `None` when the
    /// line, with `overrun` past each end, is shorter than the text, or the text there runs past an
    /// end by more than that.
    fn upright(&self, len: f32, overrun: f32, at: Option<f32>) -> Option<(&Path, f32)> {
        let total = self.fwd.length();
        let overrun = overrun.max(0.0);
        if !(len.is_finite() && len > 0.0) || len > total + 2.0 * overrun {
            return None;
        }
        let d = at.unwrap_or(total / 2.0);
        if !d.is_finite() || d - len / 2.0 < -overrun || d + len / 2.0 > total + overrun {
            return None;
        }
        let (a, b) = (self.fwd.at(d - len / 2.0)?, self.fwd.at(d + len / 2.0)?);
        let a_to_b = angle(a, b);
        // Turned half round to stand upright: read from the other end.
        let flipped = (rustgis_tiles::symbols::upright(a_to_b) - a_to_b).abs() > 1.0;
        if flipped { Some((&self.rev, total - d)) } else { Some((&self.fwd, d)) }
    }

    /// Squared screen distance and distance along the path of its nearest point.
    pub(super) fn nearest(&self, point: (f32, f32), overrun: f32) -> Option<(f32, f32)> {
        let mut best = None;
        let mut distance = f32::INFINITY;
        for (i, edge) in self.fwd.pts.windows(2).enumerate() {
            let (a, b) = (edge.first()?, edge.get(1)?);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = dx.hypot(dy);
            if !len.is_finite() || len <= 0.0 {
                continue;
            }
            let t = ((point.0 - a.0) * (dx / len) + (point.1 - a.1) * (dy / len)) / len;
            // Locked overrun placements may lie beyond an end; carry the end segment on.
            let reach = overrun.max(0.0) / len;
            let low = if i == 0 { -reach } else { 0.0 };
            let high = if i + 2 == self.fwd.pts.len() { 1.0 + reach } else { 1.0 };
            let t = t.clamp(low, high);
            let d = (point.0 - a.0 - t * dx).powi(2) + (point.1 - a.1 - t * dy).powi(2);
            if d < distance {
                distance = d;
                best = Some((d, *self.fwd.along.get(i)? + t * len));
            }
        }
        best
    }

    /// A placement's centre on the path, without laying out its characters.
    pub(super) fn anchor(&self, at: f32) -> Option<(f32, f32)> {
        self.fwd.at(at)
    }

    /// Text `len` points long straight along the line, centred `at` points along it (its middle when
    /// `None`): its centre and angle (radians, clockwise, upright). `None` when the line (with
    /// `overrun` past each end) is shorter, or the text there runs past an end by more than that.
    pub(super) fn parallel(&self, len: f32, overrun: f32, at: Option<f32>) -> Option<(f32, f32, f32)> {
        let (path, d) = self.upright(len, overrun, at)?;
        let (a, b) = (path.at(d - len / 2.0)?, path.at(d + len / 2.0)?);
        let a_to_b = angle(a, b);
        a_to_b.is_finite().then_some(((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0, a_to_b))
    }

    /// Characters `advances` wide, `gap` apart (letter spacing; both in the label's units, points ×
    /// 1.33, as its text's size), in turn along the line, centred `at` points along it (its middle when
    /// `None`): the label's centre, and each character's centre relative to it and angle (radians,
    /// clockwise, upright). `None` when the line (with `overrun` past each end) is shorter than the
    /// text, the text there runs past an end by more than that, or the line turns more than
    /// [`MAX_TURN`] from one character to the next.
    pub(super) fn curved(&self, advances: &[f32], gap: f32, overrun: f32, at: Option<f32>) -> Option<((f32, f32), Vec<(f32, f32, f32)>)> {
        // Each step from a character to the next: its advance and the letter spacing (never back).
        let step = |adv: f32| (adv + gap).max(0.0);
        let len = curved_len(advances, gap);
        let (path, mid) = self.upright(len, overrun, at)?;
        let centre = path.at(mid)?;
        let mut d = mid - len / 2.0;
        let mut out = Vec::with_capacity(advances.len());
        let mut last: Option<f32> = None;
        for adv in advances {
            let char_mid = d + adv / 2.0;
            // A character's angle: across its own width, so it sits on the bend rather than a vertex.
            let (a, b) = (path.at(char_mid - adv.max(0.5) / 2.0)?, path.at(char_mid + adv.max(0.5) / 2.0)?);
            let (p, turn) = (path.at(char_mid)?, angle(a, b));
            if !turn.is_finite() {
                return None;
            }
            if let Some(prev) = last {
                let mut step = turn - prev;
                // Wrapped to within half a turn either way.
                step = (step + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                if step.abs() > MAX_TURN {
                    return None;
                }
            }
            last = Some(turn);
            out.push((p.0 - centre.0, p.1 - centre.1, turn));
            d += step(*adv);
        }
        Some((centre, out))
    }
}

/// Middle first, then the nearest places either side, at most eight places in all.
/// The ends allow the same overrun as ordinary line labels; no alternative is past that range.
pub(super) fn positions(total: f32, width: f32, overrun: f32) -> Vec<f32> {
    let mid = total / 2.0;
    // Keep endpoints a few ULPs inside upright's strict fit bounds.
    let room = (mid - width / 2.0 + overrun.max(0.0)) * (1.0 - 8.0 * f32::EPSILON);
    let mut out = vec![mid];
    if !room.is_finite() || room <= 0.0 || !width.is_finite() || width <= 0.0 {
        return out;
    }
    let step = (width * 0.75).max(room / 4.0).max(1.0).min(room);
    for k in 1..=4 {
        let d = step * k as f32;
        if d > room {
            break;
        }
        for side in [-1.0, 1.0] {
            if out.len() < 8 {
                out.push(mid + side * d);
            }
        }
    }
    out
}

/// The most labels a line repeats a label in (the one at its middle among them).
pub(super) const MAX_REPEATS: usize = 32;

/// Where along a line `total` points long a label is centred, each with its repeat's index: its
/// middle (0), and with `repeat`, every that many points either side of it (odd indices before
/// the middle, even ones after), nearest first, at most [`MAX_REPEATS`]. A place too near an
/// end for the label is dropped by the placement, not here, so the indices stay put.
pub(super) fn repeats(total: f32, repeat: Option<f32>) -> Vec<(usize, f32)> {
    let mid = total / 2.0;
    let mut out = vec![(0, mid)];
    let Some(step) = repeat.filter(|r| r.is_finite() && *r > 0.0) else { return out };
    for k in 1..=MAX_REPEATS {
        if k as f32 * step > mid || out.len() >= MAX_REPEATS {
            break;
        }
        for (side, i) in [(-1.0, 2 * k - 1), (1.0, 2 * k)] {
            if out.len() < MAX_REPEATS {
                out.push((i, mid + side * k as f32 * step));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parallel(line: &[(f32, f32)], len: f32, overrun: f32, at: Option<f32>) -> Option<(f32, f32, f32)> {
        Line::new(line)?.parallel(len, overrun, at)
    }

    fn curved(
        line: &[(f32, f32)],
        advances: &[f32],
        gap: f32,
        overrun: f32,
        at: Option<f32>,
    ) -> Option<((f32, f32), Vec<(f32, f32, f32)>)> {
        Line::new(line)?.curved(advances, gap, overrun, at)
    }

    #[test]
    fn parallel_text_sits_at_the_middle_turned_to_the_line_and_upright() {
        // Along a line rising to the right at 45° (screen y down: up-right is −45°).
        let (x, y, a) = parallel(&[(0.0, 100.0), (100.0, 0.0)], 40.0, 0.0, None).unwrap();
        assert!((x - 50.0).abs() < 1e-3 && (y - 50.0).abs() < 1e-3);
        assert!((a + std::f32::consts::FRAC_PI_4).abs() < 1e-4, "{a}");
        // The same line drawn right to left reads the same way.
        let (_, _, b) = parallel(&[(100.0, 0.0), (0.0, 100.0)], 40.0, 0.0, None).unwrap();
        assert!((a - b).abs() < 1e-4, "{a} {b}");
        // Too short for the text, unless it may run past the ends.
        assert!(parallel(&[(0.0, 0.0), (30.0, 0.0)], 40.0, 0.0, None).is_none());
        assert!(parallel(&[(0.0, 0.0), (30.0, 0.0)], 40.0, 5.0, None).is_some());
        assert!(parallel(&[(0.0, 0.0), (0.0, 0.0)], 1.0, 5.0, None).is_none(), "no length");
    }

    #[test]
    fn a_vertical_line_reads_downward_whichever_way_it_was_drawn() {
        // Screen y down: downward is +90°. Drawn up or down, exactly vertical or a hair off it.
        let down = std::f32::consts::FRAC_PI_2;
        for line in
            [[(50.0, 0.0), (50.0, 100.0)], [(50.0, 100.0), (50.0, 0.0)], [(50.0, 100.0), (50.001, 0.0)], [(50.001, 100.0), (50.0, 0.0)]]
        {
            let (_, _, a) = parallel(&line, 40.0, 0.0, None).unwrap();
            assert!((a - down).abs() < 1e-3, "{line:?}: {a}");
            let (_, glyphs) = curved(&line, &[10.0; 4], 0.0, 0.0, None).unwrap();
            assert!(glyphs.iter().all(|g| (g.2 - down).abs() < 1e-3), "{line:?}: {glyphs:?}");
            assert!(glyphs.windows(2).all(|w| w[1].1 > w[0].1), "first character at the top: {glyphs:?}");
        }
    }

    /// Repeats along a line: its middle first (index 0), then every so far either side, nearest
    /// first, each index for one place whether or not a label fits there; at most [`MAX_REPEATS`].
    #[test]
    fn repeats_spread_out_from_the_middle_each_with_its_own_index() {
        assert_eq!(repeats(100.0, None), vec![(0, 50.0)]);
        assert_eq!(repeats(100.0, Some(30.0)), vec![(0, 50.0), (1, 20.0), (2, 80.0)]);
        assert_eq!(repeats(100.0, Some(60.0)), vec![(0, 50.0)], "none past the ends");
        assert_eq!(repeats(100.0, Some(f32::NAN)), vec![(0, 50.0)]);
        assert_eq!(repeats(100.0, Some(-5.0)), vec![(0, 50.0)]);
        let many = repeats(1e6, Some(1.0));
        assert_eq!(many.len(), MAX_REPEATS);
        let mut indices: Vec<usize> = many.iter().map(|r| r.0).collect();
        indices.dedup();
        assert_eq!(indices.len(), MAX_REPEATS, "every index once");
        // Placed there: centred at that place, upright on its stretch, and none where the text runs
        // off an end. The middle is today's single label.
        let line = [(0.0, 50.0), (100.0, 50.0), (100.0, 150.0)];
        assert_eq!(parallel(&line, 20.0, 0.0, None), parallel(&line, 20.0, 0.0, Some(100.0)));
        let (x, y, a) = parallel(&line, 20.0, 0.0, Some(30.0)).unwrap();
        assert!((x - 30.0).abs() < 1e-3 && (y - 50.0).abs() < 1e-3 && a.abs() < 1e-4, "{x} {y} {a}");
        let (x, y, a) = parallel(&line, 20.0, 0.0, Some(170.0)).unwrap();
        assert!((x - 100.0).abs() < 1e-3 && (y - 120.0).abs() < 1e-3 && (a - std::f32::consts::FRAC_PI_2).abs() < 1e-3, "{x} {y} {a}");
        assert!(parallel(&line, 20.0, 0.0, Some(5.0)).is_none(), "runs off the start");
        assert!(parallel(&line, 20.0, 5.0, Some(5.0)).is_some(), "unless it may run past");
        assert!(parallel(&line, 20.0, 0.0, Some(f32::NAN)).is_none());
        let (centre, glyphs) = curved(&line, &[5.0; 3], 0.0, 0.0, Some(30.0)).unwrap();
        assert!((centre.0 - 30.0).abs() < 1e-3 && glyphs.len() == 3, "{centre:?}");
        assert!(curved(&line, &[5.0; 3], 0.0, 0.0, Some(198.0)).is_none());
    }

    #[test]
    fn letter_spacing_sets_curved_characters_apart_and_needs_more_line() {
        let line = [(0.0, 50.0), (100.0, 50.0)];
        let (_, tight) = curved(&line, &[10.0; 3], 0.0, 0.0, None).unwrap();
        let (_, spaced) = curved(&line, &[10.0; 3], 5.0, 0.0, None).unwrap();
        assert!((tight[1].0 - tight[0].0 - 10.0).abs() < 1e-3 && (spaced[1].0 - spaced[0].0 - 15.0).abs() < 1e-3);
        assert!(spaced[1].0.abs() < 1e-3, "still centred: {spaced:?}");
        // 3 × 10 and two gaps of 40: longer than the line.
        assert!(curved(&line, &[10.0; 3], 40.0, 0.0, None).is_none());
        // Spacing tighter than a letter doesn't step back.
        let (_, back) = curved(&line, &[10.0; 3], -20.0, 0.0, None).unwrap();
        assert!(back.windows(2).all(|w| w[1].0 >= w[0].0), "{back:?}");
    }

    #[test]
    fn a_line_in_view_is_cut_at_its_edges_and_where_it_is_out_of_sight() {
        // Across the view and out, back in and out again: two stretches, cut at the edges.
        let line = [(-50.0, 50.0), (50.0, 50.0), (50.0, 150.0), (80.0, 150.0), (80.0, 50.0), (150.0, 50.0)];
        let runs = in_view(line.iter().copied().map(Some), 100.0, 100.0);
        assert_eq!(runs, [vec![(0.0, 50.0), (50.0, 50.0), (50.0, 100.0)], vec![(80.0, 100.0), (80.0, 50.0), (100.0, 50.0)]]);
        // A point out of sight splits the line; a stretch of one point is none.
        let runs =
            in_view([Some((10.0, 10.0)), Some((20.0, 10.0)), None, Some((30.0, 10.0)), Some((f32::NAN, 0.0))].into_iter(), 100.0, 100.0);
        assert_eq!(runs, [vec![(10.0, 10.0), (20.0, 10.0)]]);
        assert!(in_view([Some((-10.0, -10.0)), Some((-20.0, 200.0))].into_iter(), 100.0, 100.0).is_empty());
    }

    #[test]
    fn curved_text_follows_a_gentle_bend_and_leaves_a_sharp_one() {
        // A shallow arc: each character a little more turned than the one before.
        let arc: Vec<(f32, f32)> = (0..=40)
            .map(|i| {
                let t = (i as f32 / 40.0 - 0.5) * 1.2;
                (200.0 * t.sin() + 200.0, 200.0 - 200.0 * t.cos())
            })
            .collect();
        let (_, glyphs) = curved(&arc, &[10.0; 8], 0.0, 0.0, None).unwrap();
        assert_eq!(glyphs.len(), 8);
        assert!(glyphs.windows(2).all(|w| w[1].2 > w[0].2), "turning steadily clockwise");
        assert!(glyphs.windows(2).all(|w| w[1].0 > w[0].0), "left to right");
        // A right angle at the middle: too sharp for the text.
        assert!(curved(&[(0.0, 100.0), (100.0, 100.0), (100.0, 0.0)], &[10.0; 6], 0.0, 0.0, None).is_none());
    }
}
