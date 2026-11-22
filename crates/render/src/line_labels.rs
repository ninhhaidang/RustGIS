//! Labels along lines, in screen points: straight and turned to the line ([`Line::parallel`]), or each
//! character turned with it where it bends ([`Line::curved`]), at the line's middle and kept upright,
//! as QGIS's Parallel and Curved line placements put text on the line.

/// The most a curved label's text turns from one character to the next (radians): QGIS's
/// default limit, 25°. Past it, where a line bends sharply, the label is left out.
pub(super) const MAX_TURN: f32 = 0.436;

/// The least a curved label's characters may close up when set off its line (the distance from
/// one to the next along the text, as a share of the distance on the line): inside a bend they
/// close up as the label moves off it, and past this they would run into one another, so the
/// place is left out, as one past [`MAX_TURN`] is.
pub(super) const MIN_SPREAD: f32 = 0.8;

/// The most characters a label curves along a line: a longer one is placed straight, so a long
/// attribute value on many lines doesn't lay out, and draw, character by character.
pub(super) const MAX_CURVED_CHARS: usize = 200;

/// The most points a curved label's course ([`Line::curved`]) is sampled at.
const MAX_SAMPLES: usize = 4096;

/// Length of curved text including spacing, with no trailing gap or backward character step.
pub(super) fn curved_len(advances: &[f32], gap: f32) -> f32 {
    let step = |adv: f32| (adv + gap).max(0.0);
    advances.iter().map(|a| step(*a)).sum::<f32>() - advances.last().map_or(0.0, |a| step(*a) - a)
}

/// The letter spacing (in `advances`' units) a fitted label is spread to along a line with `room`
/// points of it in view (#2265): none while the text set `gap` apart takes a third of the room or
/// more; from there it grows smoothly, until at a sixth or less of the room it covers `stretch`
/// of it. At most three average letters' widths between letters.
pub(super) fn spread_gap(advances: &[f32], gap: f32, room: f32, stretch: f32) -> Option<f32> {
    let len = curved_len(advances, gap);
    let (n, ratio) = (advances.len(), room / len);
    if n < 2 || !(len > 0.0 && ratio.is_finite() && ratio > 3.0 && stretch.is_finite()) {
        return None;
    }
    let grown = len + ((ratio - 3.0) / 3.0).min(1.0) * (stretch * room - len);
    let widest = 3.0 * advances.iter().sum::<f32>() / n as f32;
    let mut wide = gap + (grown - len) / (n - 1) as f32;
    // Steps closed up to nothing by a negative gap open only once the gap passes them: solved on
    // the same steps as `curved_len`, between none and every step open.
    if advances[..n - 1].iter().any(|a| a + gap <= 0.0) {
        let (mut lo, mut hi) = (gap, advances.iter().map(|a| -a).fold(gap, f32::max) + (grown - len));
        for _ in 0..40 {
            let g = (lo + hi) / 2.0;
            if curved_len(advances, g) < grown { lo = g } else { hi = g }
        }
        wide = hi;
    }
    let wide = wide.min(widest);
    (wide - gap > 0.01).then_some(wide)
}

/// A class's `stretch` on a curved label that isn't fitted (#2262: along a line, a polygon's
/// centre line or a guide line): its letters are spaced so it covers `share` of the line it
/// follows, the spacing held within `lo`..=`hi` (letter spacing's range, in the advances' units).
#[derive(Clone, Copy, Debug)]
pub(super) struct Stretch {
    pub share: f32,
    pub lo: f32,
    pub hi: f32,
}

impl Stretch {
    /// The letter spacing at which `advances` covers `share` of `room`, or as near as the range
    /// lets it; `None` for fewer than two characters or a room or share that isn't a positive
    /// number (the label keeps its own spacing).
    pub(super) fn gap(&self, advances: &[f32], room: f32) -> Option<f32> {
        let target = self.share * room;
        if advances.len() < 2 || !(target.is_finite() && target > 0.0 && self.lo <= self.hi) {
            return None;
        }
        let (mut lo, mut hi) = (self.lo, self.hi);
        if curved_len(advances, hi) <= target {
            return Some(hi);
        }
        if curved_len(advances, lo) >= target {
            return Some(lo);
        }
        // `curved_len` never shrinks as the spacing grows.
        for _ in 0..40 {
            let g = (lo + hi) / 2.0;
            if curved_len(advances, g) < target { lo = g } else { hi = g }
        }
        Some(hi)
    }
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

    /// The line's course from `from` to `to` points along it: sampled every so often and averaged
    /// over `reach` points either side, twice (so it bends smoothly), its detail finer than that
    /// smoothed away. The samples start `2 * reach` before `from` and are spaced the returned
    /// number of points along the line apart.
    fn smoothed(&self, from: f32, to: f32, reach: f32) -> Option<(Path, f32)> {
        if !(from.is_finite() && to.is_finite() && reach.is_finite() && reach > 0.0 && to > from) {
            return None;
        }
        // Eight samples per half window, and no more than a few thousand in all; twice `reach` more each
        // side, so the averages at the text's ends are whole.
        let (lo, hi) = (from - 2.0 * reach, to + 2.0 * reach);
        let every = (reach / 8.0).max((hi - lo) / MAX_SAMPLES as f32);
        let n = ((hi - lo) / every).ceil() as usize + 1;
        let mut pts: Vec<(f32, f32)> = (0..n).map(|k| self.at(lo + k as f32 * every)).collect::<Option<_>>()?;
        let r = (reach / every).round().max(1.0) as usize;
        let mut sum = vec![(0.0f64, 0.0f64); n + 1];
        for _ in 0..2 {
            for (k, p) in pts.iter().enumerate() {
                let s = sum.get(k).copied()?;
                *sum.get_mut(k + 1)? = (s.0 + f64::from(p.0), s.1 + f64::from(p.1));
            }
            for (k, p) in pts.iter_mut().enumerate() {
                let (a, b) = (k.saturating_sub(r), (k + r + 1).min(n));
                let (sa, sb) = (sum.get(a).copied()?, sum.get(b).copied()?);
                let c = (b - a) as f64;
                *p = (((sb.0 - sa.0) / c) as f32, ((sb.1 - sa.1) / c) as f32);
            }
        }
        let mut along = Vec::with_capacity(n);
        let mut d = 0.0f32;
        for (k, p) in pts.iter().enumerate() {
            if let Some(q) = k.checked_sub(1).and_then(|j| pts.get(j)) {
                d += (p.0 - q.0).hypot(p.1 - q.1);
            }
            along.push(d);
        }
        (d.is_finite() && d > 0.0).then_some((Path { pts, along }, every))
    }

    /// The least distance from `p` to the line between `from` and `to` points along it, carried on
    /// past its ends (as text may overhang them).
    fn distance(&self, p: (f32, f32), from: f32, to: f32) -> f32 {
        let first = self.along.partition_point(|a| *a < from).saturating_sub(1);
        let last = self.along.partition_point(|a| *a <= to).min(self.pts.len().saturating_sub(1));
        let mut best = f32::INFINITY;
        for i in first..last {
            let (Some(a), Some(b)) = (self.pts.get(i), self.pts.get(i + 1)) else { continue };
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l2 = dx * dx + dy * dy;
            let low = if i == 0 { f32::NEG_INFINITY } else { 0.0 };
            let high = if i + 2 == self.pts.len() { f32::INFINITY } else { 1.0 };
            let t = if l2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / l2).clamp(low, high) } else { 0.0 };
            best = best.min((p.0 - a.0 - t * dx).hypot(p.1 - a.1 - t * dy));
        }
        best
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
    /// About how many bytes its paths take, for a budget: what they hold room for, since a candidate
    /// kept shares the line rather than copying it.
    pub(super) fn bytes(&self) -> usize {
        let path = |p: &Path| p.pts.capacity() * size_of::<(f32, f32)>() + p.along.capacity() * size_of::<f32>();
        size_of::<Line>() + path(&self.fwd) + path(&self.rev)
    }

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

    /// Where text `len` points long is centred on the line's straightest stretches, straightest
    /// first, at most `most` of them, none within half the text of another (#2265). A stretch's
    /// straightness is its end-to-end distance over its length along the line; on a tie, the one
    /// nearer the middle comes first. A line no longer than the text: its middle.
    pub(super) fn straightest(&self, len: f32, most: usize) -> Vec<f32> {
        let total = self.length();
        let slack = total - len;
        if !(len.is_finite() && len > 0.0 && slack > 0.0) {
            return vec![total / 2.0];
        }
        // A stretch every quarter of the text, at most a few hundred in all, the last at the end.
        let step = (len / 4.0).max(slack / 256.0);
        let starts = (slack / step).ceil() as usize;
        let mut stretches: Vec<(f32, f32)> = (0..=starts)
            .filter_map(|k| {
                let s = (k as f32 * step).min(slack);
                let (a, b) = (self.fwd.at(s)?, self.fwd.at(s + len)?);
                Some(((b.0 - a.0).hypot(b.1 - a.1) / len, s + len / 2.0))
            })
            .collect();
        let mid = total / 2.0;
        stretches.sort_by(|a, b| b.0.total_cmp(&a.0).then((a.1 - mid).abs().total_cmp(&(b.1 - mid).abs())));
        let mut out: Vec<f32> = Vec::with_capacity(most);
        for (_, at) in stretches {
            if out.len() >= most {
                break;
            }
            if out.iter().all(|o| (o - at).abs() >= len / 2.0) {
                out.push(at);
            }
        }
        out
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
    /// 1.33, as its text's `size`), in turn along the line, centred `at` points along it (its middle
    /// when `None`): the label's centre, and each character's centre relative to it and angle
    /// (radians, clockwise, upright).
    ///
    /// The characters go along the line's course rather than the line itself (#2259): the line
    /// smoothed over `reach` either side, the larger of the text's size and the step from one
    /// character to the next, so detail finer than that (a river's jitter, or meanders shorter
    /// than widely spaced letters) neither turns them nor bunches them up, and they stay evenly
    /// spaced. The course must stay within half `reach` of the line (carried on past its ends) at
    /// each character: half the text's size for ordinary spacing, half a step for widely spaced
    /// letters. `None` when the line (with `overrun` past each end) is shorter than the text, the
    /// text there runs past an end by more than that, the course strays further, it turns more than [`MAX_TURN`] from one character to the next, or it turns a
    /// character further past vertical than that (it would stand upside down).
    pub(super) fn curved(
        &self,
        advances: &[f32],
        gap: f32,
        size: f32,
        overrun: f32,
        at: Option<f32>,
    ) -> Option<((f32, f32), Vec<(f32, f32, f32)>)> {
        // Each step from a character to the next: its advance and the letter spacing (never back).
        let step = |adv: f32| (adv + gap).max(0.0);
        let len = curved_len(advances, gap);
        let (path, mid) = self.upright(len, overrun, at)?;
        let pitch = advances.iter().map(|a| step(*a)).sum::<f32>() / advances.len().max(1) as f32;
        let reach = if size.is_finite() { size.max(pitch) } else { pitch }.max(0.5);
        let (from, to) = (mid - len / 2.0, mid + len / 2.0);
        let (course, every) = path.smoothed(from, to, reach)?;
        // The label's middle on the course: sampled every `every` points from `2 * reach` before `from`.
        let k = (mid - from + 2.0 * reach) / every;
        let (i, t) = (k.floor() as usize, k.fract());
        let (a, b) = (course.along.get(i).copied()?, course.along.get(i + 1).copied().unwrap_or(course.length()));
        let cmid = a + (b - a) * t;
        let centre = course.at(cmid)?;
        // Wrapped to within half a turn either way.
        let wrap = |a: f32| (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        let mut d = cmid - len / 2.0;
        let mut out = Vec::with_capacity(advances.len());
        let mut last: Option<f32> = None;
        for adv in advances {
            let char_mid = d + adv / 2.0;
            let w = adv.max(0.5) / 2.0;
            let (a, b) = (course.at(char_mid - w)?, course.at(char_mid + w)?);
            let (p, turn) = (course.at(char_mid)?, angle(a, b));
            // Upright, or no further past vertical than a turn from one character to the next.
            if !turn.is_finite() || wrap(turn).abs() > std::f32::consts::FRAC_PI_2 + MAX_TURN {
                return None;
            }
            if last.is_some_and(|prev| wrap(turn - prev).abs() > MAX_TURN) {
                return None;
            }
            // Kept over the line: its nearest stretch around where the text goes.
            if path.distance(p, from - 2.0 * reach, to + 2.0 * reach) > reach / 2.0 {
                return None;
            }
            last = Some(turn);
            out.push((p.0 - centre.0, p.1 - centre.1, turn));
            d += step(*adv);
        }
        Some((centre, out))
    }
}

/// The words a fitted label abbreviates when it doesn't fit, unless its class says otherwise
/// (#2265): streams' generic names in English, Portuguese and Spanish.
const ABBREVIATIONS: &[(&str, &str)] = &[
    ("Creek", "Cr."),
    ("River", "R."),
    ("Canyon", "Cyn."),
    ("Brook", "Bk."),
    ("Branch", "Br."),
    ("Fork", "Fk."),
    ("Córrego", "Cór."),
    ("Ribeirão", "Rib."),
    ("Rio", "R."),
    ("Riacho", "Rch."),
    ("Arroio", "Arr."),
    ("Igarapé", "Ig."),
    ("Río", "R."),
    ("Arroyo", "Ao."),
    ("Quebrada", "Qda."),
    ("Riachuelo", "Rchlo."),
    ("Cañada", "Cda."),
    ("Barranco", "Bco."),
];

/// `text` with each whole word that `own`, or else the built-in list, abbreviates (matched
/// ignoring case) put short; `None` when it has no such word (#2265).
pub(super) fn abbreviated(text: &str, own: &std::collections::BTreeMap<String, String>) -> Option<String> {
    let short = |word: &str| -> Option<&str> {
        let lower = word.to_lowercase();
        own.iter()
            .find(|(w, _)| w.to_lowercase() == lower)
            .map(|(_, s)| s.as_str())
            .or_else(|| ABBREVIATIONS.iter().find(|(w, _)| w.to_lowercase() == lower).map(|(_, s)| *s))
    };
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    // Each word with the spaces after it.
    for piece in text.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end_matches(char::is_whitespace);
        match short(word) {
            Some(s) if !word.is_empty() => {
                out.push_str(s);
                out.push_str(&piece[word.len()..]);
                changed = true;
            }
            _ => out.push_str(piece),
        }
    }
    changed.then_some(out)
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

/// Which way is up for text turned `angle` (radians, clockwise, upright): a unit step in screen
/// points (y down) from its baseline toward its top.
fn up(angle: f32) -> (f32, f32) {
    (angle.sin(), -angle.cos())
}

/// How far a label's centre sits from its line, in screen points: `offset` points from the line
/// to the text's nearest edge (`height` the text's, both in screen points), above it as the text
/// reads when positive and below when negative; 0 is on the line.
pub(super) fn off_line(offset: f32, height: f32) -> f32 {
    if offset == 0.0 || !offset.is_finite() { 0.0 } else { (offset.abs() + height.max(0.0) / 2.0).copysign(offset) }
}

/// Straight text at `(x, y)` turned `angle` ([`Line::parallel`]'s) moved `off` screen points
/// above itself as it reads (negative: below).
pub(super) fn parallel_off((x, y, angle): (f32, f32, f32), off: f32) -> (f32, f32, f32) {
    let (ux, uy) = up(angle);
    (x + off * ux, y + off * uy, angle)
}

/// Curved text ([`Line::curved`]'s centre and characters) moved `off` screen points above itself as
/// it reads (negative: below), each character along its own way up, so the text keeps following the
/// line. The centre goes with its middle character. `None` where, inside a bend, the characters
/// would close up past [`MIN_SPREAD`] (or cross).
pub(super) fn curved_off(
    ((cx, cy), mut turns): ((f32, f32), Vec<(f32, f32, f32)>),
    off: f32,
) -> Option<((f32, f32), Vec<(f32, f32, f32)>)> {
    if off == 0.0 {
        return Some(((cx, cy), turns));
    }
    let (mx, my) = up(turns.get(turns.len() / 2)?.2);
    let (sx, sy) = (off * mx, off * my);
    // Moved in place (curved places are laid out often): the character before, where it was and
    // where it went.
    let mut before: Option<((f32, f32), (f32, f32))> = None;
    for t in &mut turns {
        let was = (t.0, t.1);
        let (ux, uy) = up(t.2);
        (t.0, t.1) = (t.0 + off * ux - sx, t.1 + off * uy - sy);
        if let Some((b_was, b_now)) = before {
            let (w, n) = ((was.0 - b_was.0, was.1 - b_was.1), (t.0 - b_now.0, t.1 - b_now.1));
            let along = w.0 * w.0 + w.1 * w.1;
            // How far the step now goes the way it went, as a share of how far it went.
            if along > 0.0 && (n.0 * w.0 + n.1 * w.1) / along < MIN_SPREAD {
                return None;
            }
        }
        before = Some((was, (t.0, t.1)));
    }
    Some(((cx + sx, cy + sy), turns))
}

/// The most labels a line repeats a label in (the one at its middle among them).
pub(super) const MAX_REPEATS: usize = 32;

/// The index of the close-set repeat at a line's middle when a spread label went off it (#2265):
/// 0 is the spread label's, and [`repeats`] numbers none this high.
pub(super) const MIDDLE_REPEAT: usize = 2 * MAX_REPEATS + 1;

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

/// The places of [`repeats`] at least `clear` from `at` (where a spread label is, #2265), with
/// their indices, nearest the middle first: those nearer are skipped before counting, so a short
/// repeat still names the line beyond the spread label. The middle is among them when the spread
/// label is clear of it and the repeat names the line more than once, as [`MIDDLE_REPEAT`]. At
/// most [`MAX_REPEATS`] less one.
pub(super) fn repeats_clear_of(total: f32, repeat: Option<f32>, at: f32, clear: f32) -> Vec<(usize, f32)> {
    let mut out = Vec::new();
    let Some(step) = repeat.filter(|r| r.is_finite() && *r > 0.0) else { return out };
    if !(total.is_finite() && at.is_finite() && clear.is_finite()) {
        return out;
    }
    let (mid, step) = (f64::from(total) / 2.0, f64::from(step));
    // (A repeat longer than half the line names it once: the spread label is that one.)
    if step <= mid && (f64::from(at) - mid).abs() >= f64::from(clear) {
        out.push((MIDDLE_REPEAT, total / 2.0));
    }
    // Each side's repeats nearer `at` than `clear`: those whose count from the middle is between.
    let near =
        |side: f64| ((side * (f64::from(at) - mid) - f64::from(clear)) / step, (side * (f64::from(at) - mid) + f64::from(clear)) / step);
    let sides = [(-1.0, near(-1.0)), (1.0, near(1.0))];
    let mut k = 1.0f64;
    while k * step <= mid && out.len() < MAX_REPEATS - 1 {
        let mut past = f64::INFINITY;
        for (n, (side, (lo, hi))) in sides.into_iter().enumerate() {
            if k > lo && k < hi {
                past = past.min(hi.ceil());
            } else if out.len() < MAX_REPEATS - 1 {
                out.push(((k as usize).saturating_mul(2).saturating_sub(1).saturating_add(n), (mid + side * k * step) as f32));
            }
        }
        // Both sides near it: on past the nearer end of the two stretches.
        k = if past.is_finite() && sides.iter().all(|(_, (lo, hi))| k > *lo && k < *hi) { past.max(k + 1.0) } else { k + 1.0 };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text size the curved tests set their characters in (screen points).
    const SIZE: f32 = 12.0;

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
        Line::new(line)?.curved(advances, gap, SIZE, overrun, at)
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

    /// A gentle sine, smooth or drawn with a fine zigzag as a river's vertices are: read over each
    /// character's own width the zigzag turns past [`MAX_TURN`] from one to the next; along the
    /// course, smoothed over the text's height, each character is turned to the sine where it is,
    /// but for the zigzag's own tilt over that height, steps no more than on the smooth line, and
    /// they are evenly spaced (#2259).
    #[test]
    fn curved_text_follows_a_lines_course_not_its_jitter() {
        // A vertex every `every` points, every other one `zigzag` off the course, either side.
        let sine = |every: usize, zigzag: f32| -> Vec<(f32, f32)> {
            (0..=400 / every)
                .map(|i| {
                    let x = (i * every) as f32;
                    let jitter = if i % 2 == 1 { zigzag * if i % 4 == 1 { 1.0 } else { -1.0 } } else { 0.0 };
                    (x, 100.0 + 20.0 * (x / 400.0 * std::f32::consts::TAU).sin() + jitter)
                })
                .collect()
        };
        let (smooth, rough) = (sine(1, 0.0), sine(3, 1.0));
        let along =
            |line: &[(f32, f32)], x: f32| Path::new(line, false).and_then(|p| Some(*p.along.get(p.pts.iter().position(|q| q.0 == x)?)?));
        let steps = |glyphs: &[(f32, f32, f32)]| -> Vec<f32> { glyphs.windows(2).map(|w| (w[1].2 - w[0].2).abs()).collect() };
        let advances = [7.0; 12];
        // The zigzag's own tilt over a text's height either side.
        let tilt = (1.0f32 / SIZE).atan();
        for x in [99.0, 201.0, 300.0] {
            let (_, even) = curved(&smooth, &advances, 0.0, 0.0, along(&smooth, x)).unwrap();
            let (centre, wavy) = curved(&rough, &advances, 0.0, 0.0, along(&rough, x)).expect("placed on the zigzag");
            let most = steps(&even).into_iter().fold(0.0f32, f32::max);
            assert!(steps(&wavy).iter().all(|s| *s <= most + 2.0 * tilt && *s < MAX_TURN), "at {x}: {:?} past {most}", steps(&wavy));
            // The sine's own direction where each character is.
            let course = |gx: f32| (20.0 * std::f32::consts::TAU / 400.0 * (gx / 400.0 * std::f32::consts::TAU).cos()).atan();
            assert!(wavy.iter().all(|g| (g.2 - course(centre.0 + g.0)).abs() <= tilt), "at {x}: turned to the course: {wavy:?}");
            // Evenly spaced, a character's advance apart, as on a straight line.
            assert!(wavy.windows(2).all(|w| ((w[1].0 - w[0].0).hypot(w[1].1 - w[0].1) - 7.0).abs() < 0.2), "at {x}: {wavy:?}");
        }
    }

    /// Read over the text's height, a short label's course can point back against the way the text
    /// reads (here: rightward text whose courses both run back left); it is left out rather than
    /// drawn upside down. With no usable size, a character's angle is read over its own width.
    #[test]
    fn curved_text_never_reads_against_itself() {
        let line = [(20.0, -1.0), (0.0, -1.0), (0.0, 0.0), (8.0, 0.0), (8.0, 1.0), (-12.0, 1.0)];
        assert!(curved(&line, &[3.0; 2], 0.0, 0.0, Some(25.0)).is_none());
        let level = [(0.0, 50.0), (100.0, 50.0), (100.0, 150.0)];
        for size in [f32::NAN, f32::INFINITY] {
            let (_, glyphs) = Line::new(&level).unwrap().curved(&[5.0; 3], 0.0, size, 0.0, Some(30.0)).unwrap();
            assert!(glyphs.iter().all(|g| g.2.abs() < 1e-4), "{size}: {glyphs:?}");
        }
    }

    /// Text may run past a line's ends by its overrun: the line is carried on there, so the text
    /// over a straight line's end is on it, not off it (#2259).
    #[test]
    fn curved_text_may_overhang_a_lines_ends() {
        let line = [(0.0, 50.0), (100.0, 50.0)];
        let (_, glyphs) = curved(&line, &[10.0; 12], 0.36, 12.0, None).expect("overhangs by nearly 12 of 12");
        assert!(glyphs.iter().all(|g| g.1.abs() < 1e-3 && g.2.abs() < 1e-4), "{glyphs:?}");
    }

    /// A bend of nearly half a turn under a label whose start-to-end direction is steep: one whose
    /// characters would stand upside down is refused; one that is placed has none upside down.
    #[test]
    fn curved_text_past_the_turn_bound_is_refused() {
        // Its course turns from -0.5 to 2.9 radians, so the text's ends are -0.3 and 2.7 and its
        // start-to-end direction about 1.2: its far characters turn past the bound, so it is
        // left out. A shorter text round the middle of the turn is placed, every character
        // within the bound.
        let arc: Vec<(f32, f32)> = (0..=340)
            .map(|i| {
                let t = -0.5 + i as f32 / 100.0;
                (200.0 + 58.0 * t.sin(), 200.0 - 58.0 * t.cos())
            })
            .collect();
        assert!(curved(&arc, &[7.0; 25], 0.0, 0.0, None).is_none());
        let (_, glyphs) = curved(&arc, &[7.0; 8], 0.0, 0.0, None).expect("a short text round the turn is placed");
        assert!(glyphs.iter().all(|g| g.2.abs() <= std::f32::consts::FRAC_PI_2 + MAX_TURN), "upside down: {glyphs:?}");
    }

    /// A hairpin tighter than the text, sharper than [`MAX_TURN`] over a text's height: still left out.
    #[test]
    fn curved_text_leaves_a_hairpin() {
        // Out along y = 100, round a turn of radius 5 and back along y = 110: its middle at the turn.
        let mut hairpin: Vec<(f32, f32)> = vec![(0.0, 100.0)];
        hairpin.extend((0..=12).map(|i| {
            let t = i as f32 / 12.0 * std::f32::consts::PI;
            (200.0 + 5.0 * t.sin(), 105.0 - 5.0 * t.cos())
        }));
        hairpin.push((0.0, 110.0));
        assert!(curved(&hairpin, &[7.0; 8], 0.0, 0.0, None).is_none());
        // Off the turn, along one of its arms, the same text fits.
        assert!(curved(&hairpin, &[7.0; 8], 0.0, 0.0, Some(100.0)).is_some());
        // A 45° corner is rounded over the text's height: taken, the characters turning a little at
        // a time from one leg to the other.
        let (c, s) = (45f32.to_radians().cos(), 45f32.to_radians().sin());
        let (_, glyphs) = curved(&[(0.0, 100.0), (100.0, 100.0), (100.0 + 100.0 * c, 100.0 + 100.0 * s)], &[7.0; 8], 0.0, 0.0, None)
            .expect("a 45° corner");
        assert!(glyphs.windows(2).all(|w| w[1].2 >= w[0].2 && w[1].2 - w[0].2 < MAX_TURN), "{glyphs:?}");
        assert!(glyphs.iter().all(|g| (-1e-4..=45f32.to_radians() + 1e-4).contains(&g.2)), "between the legs: {glyphs:?}");
    }

    /// Round a corner the characters keep their spacing along the course, never bunched up at the
    /// vertex, turning a little at a time; a corner too sharp for that (past 60° for this text) is
    /// left out (#2259).
    #[test]
    fn curved_text_round_a_corner_keeps_its_spacing() {
        for degrees in [30.0f32, 45.0, 60.0, 75.0, 90.0] {
            for side in [1.0f32, -1.0] {
                let turn = side * degrees.to_radians();
                let corner = [(0.0, 100.0), (100.0, 100.0), (100.0 + 100.0 * turn.cos(), 100.0 + 100.0 * turn.sin())];
                let on = curved(&corner, &[7.0; 8], 0.0, 0.0, None);
                assert_eq!(on.is_some(), degrees <= 60.0, "{degrees}° {side}");
                if let Some((_, glyphs)) = &on {
                    let gaps: Vec<f32> = glyphs.windows(2).map(|w| (w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)).collect();
                    assert!(gaps.iter().all(|g| *g > 6.8 && *g <= 7.0 + 1e-3), "{degrees}° {side}: {gaps:?}");
                }
            }
        }
    }

    /// Letters spaced wider than the text is tall sweep along the line's trend, smoothed over that
    /// spacing: short meanders hardly turn them, and they stay within half a step of the line. Set
    /// close together, the text follows the same meanders more closely, within half its height.
    #[test]
    fn widely_spaced_curved_text_follows_the_trend_not_the_meanders() {
        let meanders: Vec<(f32, f32)> =
            (0..=1600).map(|i| (i as f32 * 0.5, 100.0 + 4.0 * (i as f32 * 0.5 / 30.0 * std::f32::consts::TAU).sin())).collect();
        let (_, wide) = curved(&meanders, &[7.0; 6], 30.0, 0.0, None).expect("widely spaced");
        assert!(wide.iter().all(|g| g.2.abs() < 0.1), "along the trend: {wide:?}");
        let line = Path::new(&meanders, false).unwrap();
        let (cx, cy) = Line::new(&meanders).unwrap().curved(&[7.0; 6], 30.0, SIZE, 0.0, None).unwrap().0;
        assert!(
            wide.iter().all(|g| line.distance((cx + g.0, cy + g.1), 0.0, line.length()) <= 37.0 / 2.0),
            "within half a step of the line: {wide:?}"
        );
        let (c, close) = Line::new(&meanders).unwrap().curved(&[7.0; 6], 0.0, SIZE, 0.0, None).expect("close together");
        assert!(close.iter().all(|g| line.distance((c.0 + g.0, c.1 + g.1), 0.0, line.length()) <= SIZE / 2.0), "{close:?}");
        let most = |g: &[(f32, f32, f32)]| g.iter().fold(0.0f32, |m, g| m.max(g.2.abs()));
        assert!(most(&close) > most(&wide), "closer to the meanders: {close:?}");
    }

    /// A distance from the line goes to the text's nearest edge, either side, and 0 is on it (#1187).
    #[test]
    fn a_distance_from_the_line_reaches_the_nearest_edge() {
        assert_eq!(off_line(0.0, 12.0), 0.0);
        assert_eq!(off_line(4.0, 12.0), 10.0);
        assert_eq!(off_line(-4.0, 12.0), -10.0);
        assert_eq!(off_line(f32::NAN, 12.0), 0.0);
    }

    /// Above is as the text reads, not north: over a level line it is up the screen, beside a line
    /// read downward it is to the right, and on a line drawn right to left it is still up.
    #[test]
    fn parallel_text_above_the_line_is_above_as_it_reads() {
        let level = parallel(&[(0.0, 50.0), (100.0, 50.0)], 40.0, 0.0, None).unwrap();
        let (x, y, a) = parallel_off(level, 10.0);
        assert!((x - 50.0).abs() < 1e-3 && (y - 40.0).abs() < 1e-3 && a.abs() < 1e-6);
        let (_, y, _) = parallel_off(level, -10.0);
        assert!((y - 60.0).abs() < 1e-3, "below");
        let backward = parallel(&[(100.0, 50.0), (0.0, 50.0)], 40.0, 0.0, None).unwrap();
        assert!((parallel_off(backward, 10.0).1 - 40.0).abs() < 1e-3, "the way it was drawn doesn't count");
        // A vertical line reads downward, so the text's top faces right.
        let vertical = parallel(&[(50.0, 0.0), (50.0, 100.0)], 40.0, 0.0, None).unwrap();
        let (x, y, _) = parallel_off(vertical, 10.0);
        assert!((x - 60.0).abs() < 1e-3 && (y - 50.0).abs() < 1e-3);
    }

    /// Curved text set off its line follows the line's bend: outside a bend its characters spread,
    /// inside they close up, and too far inside (or 0) it is left out (or as on the line).
    #[test]
    fn curved_text_off_the_line_follows_it_and_leaves_a_bend_too_tight_inside() {
        // A hill of radius 200 (its centre below): above the line is outside the bend.
        let arc: Vec<(f32, f32)> = (0..=40)
            .map(|i| {
                let t = (i as f32 / 40.0 - 0.5) * 1.2;
                (200.0 * t.sin() + 200.0, 200.0 - 200.0 * t.cos())
            })
            .collect();
        let on = curved(&arc, &[10.0; 8], 0.0, 0.0, None).unwrap();
        assert_eq!(curved_off(on.clone(), 0.0), Some(on.clone()));
        let spread = |c: &((f32, f32), Vec<(f32, f32, f32)>)| (c.1[7].0 - c.1[0].0).hypot(c.1[7].1 - c.1[0].1);
        let above = curved_off(on.clone(), 20.0).unwrap();
        assert!(above.0.1 < on.0.1 - 19.0, "the centre rises with it");
        assert!(spread(&above) > spread(&on), "outside the bend, apart");
        assert!(above.1.iter().zip(&on.1).all(|(a, b)| a.2 == b.2), "each character keeps its turn");
        let below = curved_off(on.clone(), -20.0).unwrap();
        assert!(spread(&below) < spread(&on), "inside it, closer");
        // A quarter of the way to the bend's centre: they would run together.
        assert!(curved_off(on, -50.0).is_none());
    }

    /// Whole words only, ignoring case, a class's own over the built-in list, spaces kept (#2265).
    #[test]
    fn a_name_is_abbreviated_by_whole_words() {
        let none = std::collections::BTreeMap::new();
        assert_eq!(abbreviated("Ribeirão  das Pedras", &none).as_deref(), Some("Rib.  das Pedras"));
        assert_eq!(abbreviated("Mill CREEK", &none).as_deref(), Some("Mill Cr."));
        assert_eq!(abbreviated("Riozinho", &none), None, "not inside a word");
        assert_eq!(abbreviated("", &none), None);
        let own = std::collections::BTreeMap::from([("creek".to_owned(), "Ck".to_owned()), ("Fundo".to_owned(), "Fdo.".to_owned())]);
        assert_eq!(abbreviated("Creek Fundo\n", &own).as_deref(), Some("Ck Fdo.\n"));
    }

    /// Spread (#2265) only past three times the text, growing to `stretch` of the line at six,
    /// at most three letters apart; on the same clamped steps as `curved_len` with a negative gap.
    #[test]
    fn a_fitted_label_is_spread_only_where_its_line_has_room_to_spare() {
        let advances = [10.0; 5];
        let len = |gap: f32| curved_len(&advances, gap);
        // Taking a third of the line or more: close-set.
        assert_eq!(spread_gap(&advances, 0.0, 3.0 * len(0.0), 0.6), None);
        // Then it grows smoothly with the room, and from six times the text covers `stretch` of it.
        let gaps: Vec<f32> = [3.1, 4.0, 5.0, 6.0].iter().map(|r| spread_gap(&advances, 0.0, r * 50.0, 0.5).unwrap()).collect();
        assert!(gaps.windows(2).all(|w| w[1] > w[0]), "{gaps:?}");
        assert!(gaps[0] < 0.5, "{gaps:?}");
        assert!((len(gaps[3]) - 0.5 * 300.0).abs() < 0.01, "{}", len(gaps[3]));
        // Never more than three letters' widths apart, nor spread by a stretch under the text.
        assert_eq!(spread_gap(&advances, 0.0, 10_000.0, 0.6), Some(30.0));
        assert_eq!(spread_gap(&advances, 0.0, 400.0, 0.1), None);
        assert_eq!(spread_gap(&[10.0], 0.0, 400.0, 0.6), None);
        // A gap closing a step up to nothing: the spread still covers `stretch` of the line.
        let tight = [1.0, 10.0, 1.0];
        let wide = spread_gap(&tight, -5.0, 36.0, 0.6).unwrap();
        assert!((curved_len(&tight, wide) - 0.6 * 36.0).abs() < 0.01, "{wide}");
    }

    /// `stretch` without `fit` (#2262): the text covers that share of its line, within letter
    /// spacing's range, wider or tighter than its own; none for one character.
    #[test]
    fn a_stretched_label_covers_its_share_of_the_line_within_the_spacing_range() {
        let advances = [10.0; 5];
        let s = |share: f32| Stretch { share, lo: -6.65, hi: 66.5 };
        for (share, room) in [(0.7, 400.0), (0.5, 120.0), (0.9, 60.0)] {
            let gap = s(share).gap(&advances, room).unwrap();
            assert!((curved_len(&advances, gap) - share * room).abs() < 0.01, "{share} of {room}: {gap}");
        }
        // Past the range: held at its ends.
        assert_eq!(s(1.0).gap(&advances, 10_000.0), Some(66.5));
        assert_eq!(s(0.1).gap(&advances, 100.0), Some(-6.65));
        assert_eq!(s(0.7).gap(&[10.0], 400.0), None);
        assert_eq!(s(0.7).gap(&advances, f32::NAN), None);
    }

    /// The repeats clear of a spread label (#2265): as [`repeats`] beyond it, the nearer ones
    /// skipped before counting, so a short repeat still names the line toward its ends.
    #[test]
    fn repeats_clear_of_a_spread_label_are_counted_beyond_it() {
        let filtered = |at: f32, clear: f32| -> Vec<(usize, f32)> {
            let middle = repeats(1000.0, Some(100.0)).into_iter().map(|(i, d)| (if i == 0 { MIDDLE_REPEAT } else { i }, d));
            middle.filter(|(_, d)| (d - at).abs() >= clear).collect()
        };
        assert_eq!(repeats_clear_of(1000.0, Some(100.0), 500.0, 150.0), filtered(500.0, 150.0));
        // Off the middle and clear of it: the middle is named close-set too, first.
        assert_eq!(repeats_clear_of(1000.0, Some(100.0), 300.0, 150.0), filtered(300.0, 150.0));
        assert_eq!(repeats_clear_of(1000.0, Some(100.0), 300.0, 150.0)[0], (MIDDLE_REPEAT, 500.0));
        assert!(repeats_clear_of(1000.0, Some(100.0), 400.0, 150.0).iter().all(|(i, _)| *i != MIDDLE_REPEAT));
        assert_eq!(repeats_clear_of(1000.0, Some(600.0), 300.0, 150.0), vec![], "named once: the spread label");
        assert_eq!(repeats_clear_of(1000.0, None, 500.0, 150.0), vec![]);
        // A short step: the nearest 32 would all be near the middle, the ones beyond it go.
        let short = repeats_clear_of(1000.0, Some(5.0), 500.0, 122.0);
        assert_eq!(short.len(), MAX_REPEATS - 1);
        assert!(short.iter().all(|(_, d)| (d - 500.0).abs() >= 122.0), "{short:?}");
        assert_eq!(short[..2], [(49, 375.0), (50, 625.0)], "indices as repeats numbers them");
        let mut indices: Vec<usize> = short.iter().map(|r| r.0).collect();
        indices.dedup();
        assert_eq!(indices.len(), short.len(), "every index once");
        assert!(repeats_clear_of(1000.0, Some(1e-30), 500.0, 122.0).len() < MAX_REPEATS);
    }

    /// Straightest first, none within half the text of another; a line no longer than the text,
    /// its middle (#2265).
    #[test]
    fn the_straightest_stretches_come_first() {
        // A zigzag 200 points long, then 200 points straight.
        let mut line: Vec<(f32, f32)> = (0..=20).map(|i| (i as f32 * 10.0, if i % 2 == 1 { 8.0 } else { 0.0 })).collect();
        line.push((400.0, 0.0));
        let line = Line::new(&line).unwrap();
        let at = line.straightest(60.0, 4);
        assert!(!at.is_empty() && at.len() <= 4, "{at:?}");
        let x = |d: f32| line.anchor(d).unwrap().0;
        assert!(x(at[0]) > 230.0, "on the straight half: {at:?}");
        assert!(at.iter().enumerate().all(|(i, a)| at.iter().skip(i + 1).all(|b| (a - b).abs() >= 30.0)), "{at:?}");
        assert!(at.iter().all(|d| (30.0..=line.length() - 30.0).contains(d)), "{at:?}");
        assert_eq!(Line::new(&[(0.0, 0.0), (50.0, 0.0)]).unwrap().straightest(60.0, 4), vec![25.0]);
        assert_eq!(Line::new(&[(0.0, 0.0), (50.0, 0.0)]).unwrap().straightest(f32::NAN, 4), vec![25.0]);
    }
}
