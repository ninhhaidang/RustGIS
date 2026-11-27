//! The scale bar: its plan, fit and drawing.

use super::*;

/// A scale bar label for `v`, a multiple of `step`: with the decimals the step needs (up to 6), so
/// 0.0125 km stays 0.0125 rather than rounding to 0.01. A step with no exact decimal (a third)
/// keeps two significant digits of it.
pub(super) fn scale_number(v: f64, step: f64) -> String {
    let places = (0..6usize)
        .find(|&n| {
            let k = step * 10f64.powi(n as i32);
            (k - k.round()).abs() < 1e-9 * k.abs().max(1.0)
        })
        .unwrap_or_else(|| {
            let mag = if step.is_finite() && step > 0.0 { step.log10().floor() } else { 0.0 };
            (1.0 - mag).clamp(0.0, 6.0) as usize
        });
    let s = format!("{v:.prec$}", prec = places);
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

/// A scale bar's number for `v`, a multiple of `step`, with a decimal comma when `comma`. With no
/// `decimals` (auto), as [`scale_number`] prints it and no thousands separators, as bars always
/// printed; with 0 to [`MAX_SCALE_DECIMALS`](rustgis_map::layout::MAX_SCALE_DECIMALS), that many
/// places and the thousands apart. A minus only in front of a digit other than zero; NaN and
/// infinity print 0.
pub(super) fn bar_number(v: f64, step: f64, decimals: Option<usize>, comma: bool) -> String {
    let v = if v.is_finite() { v } else { 0.0 };
    let body = match decimals {
        None => decimal(scale_number(v.abs(), step), comma),
        Some(d) => {
            let places = d.min(rustgis_map::layout::MAX_SCALE_DECIMALS);
            let s = format!("{:.*}", places, v.abs());
            let (whole, part) = s.split_once('.').unwrap_or((s.as_str(), ""));
            let whole = rustgis_map::graphics::thousands(whole.parse().unwrap_or(0.0));
            if part.is_empty() { decimal(whole, comma) } else { decimal(format!("{whole}.{part}"), comma) }
        }
    };
    if v < 0.0 && body.chars().any(|c| matches!(c, '1'..='9')) { format!("-{body}") } else { body }
}

/// The share of its element's width a scale bar's length may take.
pub(super) const BAR_SHARE: f64 = 0.85;
/// How far a scale bar starts in from the element's left edge at its labels' default size, in
/// pixels. [`scale_bar_width_pt`] counts that inset as as many points: when a point is a pixel or
/// more, a pixel is no larger than a point, so the box is never too narrow. It follows the labels'
/// size, as their room does (#2581).
pub(super) const BAR_INSET: f32 = 2.0;
/// How far a scale bar label's digits rise above the middle of their line, in label sizes: what
/// the room over the bar holds.
pub(super) const BAR_LABEL_RISE: f32 = 0.5;
/// How far the middle of a scale bar's labels' line sits over the bar's top, in label sizes.
pub(super) const BAR_LABEL_MID: f32 = 0.7;

/// What a scale bar counts: its whole length in its unit, that unit in metres and its abbreviation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BarPlan {
    pub(super) total: f64,
    pub(super) unit_m: f64,
    pub(super) abbr: &'static str,
}

/// The bar a scale bar draws in an element `width_pt` points wide at `ground_per_pt` ground metres
/// a point: the roundest length within [`BAR_SHARE`] of the width. `None` when it draws none.
pub(super) fn bar_plan(ground_per_pt: f64, width_pt: f64, units: ScaleUnits) -> Option<BarPlan> {
    if !(ground_per_pt.is_finite() && ground_per_pt > 0.0 && width_pt.is_finite()) {
        return None;
    }
    let room_m = ground_per_pt * width_pt * BAR_SHARE;
    let (unit_m, abbr) = units.resolve(rustgis_geom::nice_distance(room_m));
    let total = rustgis_geom::nice_distance(room_m / unit_m);
    (total > 0.0).then_some(BarPlan { total, unit_m, abbr })
}

/// A scale bar's label at division `i` of `divisions`, the last with the unit.
pub(super) fn bar_label(total: f64, divisions: usize, i: usize, abbr: &str, comma: bool) -> String {
    let v = decimal(scale_number(total * i as f64 / divisions as f64, total / divisions as f64), comma);
    if i == divisions { format!("{v} {abbr}") } else { v }
}

/// The narrowest element, in points, in which a scale bar draws the same bar as in one `width_pt`
/// wide (its length, unit and divisions), its last label at its `text_size` and `pad_pt` after it
/// included, so a box can be the size of its bar (#1379). `width_pt` when no narrower one draws
/// that bar; `None` when it draws none.
pub fn scale_bar_width_pt(ground_per_pt: f64, width_pt: f64, bar: (ScaleUnits, usize, f64), comma: bool, pad_pt: f64) -> Option<f64> {
    let (units, divisions, text_size) = bar;
    let k = layout_text_scale(text_size, SCALE_BAR_TEXT_PT);
    let plan = bar_plan(ground_per_pt, width_pt, units)?;
    let divisions = divisions.clamp(1, rustgis_map::layout::MAX_SCALE_DIVISIONS);
    let len = plan.total * plan.unit_m / ground_per_pt;
    let last = bar_label(plan.total, divisions, divisions, plan.abbr, comma);
    let half = f64::from(text::measure(&last, (SCALE_BAR_TEXT_PT * k) as f32, false)) / 2.0;
    let inset = f64::from(bar_inset(&bar_label(plan.total, divisions, 0, plan.abbr, comma), k as f32, 1.0));
    // A little over the share the bar takes, so the f32 rectangle an export rebuilds the width
    // from (its error growing with the element's place on the page) doesn't pick the next shorter
    // length: a ten-thousandth, 0.003 mm on a 30 mm box.
    let w = (len / BAR_SHARE * (1.0 + 1e-4)).max(inset + len + half + pad_pt);
    Some(if w < width_pt && bar_plan(ground_per_pt, w, units) == Some(plan) { w } else { width_pt })
}

/// [`scale_bar_width_pt`] for the scale bar `bar` as it is set: the narrowest element, in points,
/// in which it draws the same bar as in one `width_pt` wide, with every setting it has (its
/// subdivisions, divisions before zero, labels, unit label and where it sits, decimals, fit and
/// text size), its ink and `pad_pt` after it included, as [`bar_layout`] places it for the
/// drawing. A bar without the details is measured as [`scale_bar_width_pt`] measures it, so its
/// box is the one it always had. `width_pt` when no narrower one draws that bar; `None` when it
/// draws none, or `bar` is no scale bar.
pub fn scale_bar_box_width_pt(ground_per_pt: f64, width_pt: f64, bar: &ElementKind, comma: bool, pad_pt: f64) -> Option<f64> {
    let options = BarOptions::of(bar)?;
    if !bar.scale_bar_uses_details() {
        return scale_bar_width_pt(ground_per_pt, width_pt, (options.units, options.divisions, options.text_size), comma, pad_pt);
    }
    // At a point a pixel; the height leaves the width as it is.
    let place = |w: f64| Rect::from_xywh(0.0, 0.0, w as f32, 60.0).and_then(|r| bar_layout(r, ground_per_pt, &options, 1.0, comma));
    let wide = place(width_pt)?;
    let unit = |b: &BarLayout| b.unit.as_ref().map(|u| u.text.clone());
    let labels = |b: &BarLayout| b.labels.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>();
    // The same bar: its length and divisions, each label and its unit label written in full as
    // they are in the wider box, and its ink and padding inside the box.
    let same = |w: f64| {
        place(w).is_some_and(|b| {
            b.total == wide.total
                && b.divs == wide.divs
                && labels(&b) == labels(&wide)
                && unit(&b) == unit(&wide)
                && f64::from(b.right) + pad_pt <= w
        })
    };
    if !same(width_pt) {
        return Some(width_pt);
    }
    // A narrower box draws a bar no longer: the narrowest one that draws this one, halved for, to
    // a hundredth of a point: a frame dragged measures its box on every move, so no further.
    let (mut lo, mut hi) = (0.0, width_pt);
    for _ in 0..48 {
        if hi - lo <= 0.01 {
            break;
        }
        let mid = (lo + hi) / 2.0;
        if same(mid) { hi = mid } else { lo = mid }
    }
    // A little over, as for a plain bar, so the f32 rectangle an export rebuilds the width from
    // doesn't pick the next shorter length.
    let w = hi * (1.0 + 1e-4);
    Some(if w < width_pt && same(w) { w } else { width_pt })
}

/// How far a scale bar starts in from its box's left side, in pixels, for its first label `first`
/// at `k` times the default size (#2581): [`BAR_INSET`] in proportion, or more, so that label
/// reaches no further past the side than it does at the default size, as the bar moves down to
/// keep its labels from reaching higher. At the default size, [`BAR_INSET`]: pixels, as the
/// drawing has always placed the bar, not following `ppp`, which only the labels' widths do.
pub(super) fn bar_inset(first: &str, k: f32, ppp: f32) -> f32 {
    let half = |k: f32| text::measure(first, SCALE_BAR_TEXT_PT as f32 * k * ppp, false) / 2.0;
    (BAR_INSET * k).max(half(k) - (half(1.0) - BAR_INSET).max(0.0))
}

/// A scale bar's look: the fields of [`ElementKind::ScaleBar`] but its frame and colours.
#[derive(Clone, Copy, Debug)]
pub(super) struct BarOptions<'a> {
    pub(super) units: ScaleUnits,
    pub(super) divisions: usize,
    pub(super) style: ScaleBarStyle,
    pub(super) subdivisions: usize,
    pub(super) divisions_before_zero: usize,
    pub(super) label_position: ScaleLabelPosition,
    pub(super) unit_label: Option<&'a str>,
    pub(super) unit_label_position: UnitLabelPosition,
    pub(super) decimals: Option<u8>,
    pub(super) text_size: f64,
    pub(super) fit: ScaleBarFit,
}

impl<'a> BarOptions<'a> {
    /// A scale bar element's look; `None` for any other element.
    pub(super) fn of(kind: &'a ElementKind) -> Option<BarOptions<'a>> {
        let ElementKind::ScaleBar {
            units,
            divisions,
            style,
            subdivisions,
            divisions_before_zero,
            label_position,
            unit_label,
            unit_label_position,
            decimals,
            text_size,
            fit,
            ..
        } = kind
        else {
            return None;
        };
        Some(BarOptions {
            units: *units,
            divisions: *divisions,
            style: *style,
            subdivisions: *subdivisions,
            divisions_before_zero: *divisions_before_zero,
            label_position: *label_position,
            unit_label: unit_label.as_deref(),
            unit_label_position: *unit_label_position,
            decimals: *decimals,
            text_size: *text_size,
            fit: *fit,
        })
    }
}

/// A new scale bar's look.
#[cfg(test)]
impl Default for BarOptions<'_> {
    fn default() -> Self {
        static PLAIN: std::sync::LazyLock<ElementKind> = std::sync::LazyLock::new(|| ElementKind::scale_bar(0));
        BarOptions::of(&PLAIN).expect("a scale bar")
    }
}

/// How long a scale bar is and how it divides: its whole length in its unit, any divisions before
/// zero included, and its divisions after zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BarFit {
    pub(super) total: f64,
    pub(super) after: usize,
}

/// A scale bar of `before` divisions before zero and `after` after it, counting in a unit `unit_m`
/// metres long, fitted to `room_m` ground metres by `fit`: the roundest length divided as asked
/// (`AdjustDivisionValue`, the length [`bar_plan`] takes), as many round divisions as fit
/// (`AdjustDivision`), or the asked divisions each a round length (`AdjustWidth`); under every fit
/// at most ten in all, the divisions before zero included. `None` when it draws none.
pub(super) fn bar_fit(room_m: f64, unit_m: f64, fit: ScaleBarFit, before: usize, after: usize) -> Option<BarFit> {
    // Ten in all under every fit, the divisions before zero among them, and at least one after zero.
    let most = rustgis_map::layout::MAX_SCALE_DIVISIONS.saturating_sub(before).max(1);
    let after = after.clamp(1, most);
    let count = (before + after) as f64;
    let (total, after) = match fit {
        ScaleBarFit::AdjustDivisionValue => (rustgis_geom::nice_distance(room_m / unit_m), after),
        ScaleBarFit::AdjustDivision => {
            let most = most as f64;
            let fits = |value: f64| (room_m / unit_m / value + 1e-9).floor() - before as f64;
            // More than ten would fit: the next round division (1, 2, 5, 10 …) until ten do, so
            // the bar fills its room as far as a round value allows rather than stopping short.
            let mut value = rustgis_geom::nice_distance(room_m / unit_m / count);
            for _ in 0..64 {
                if !(fits(value) > most && value.is_finite() && value > 0.0) {
                    break;
                }
                value = rustgis_geom::nice_distance(value * 2.5);
            }
            let after = fits(value).clamp(1.0, most) as usize;
            (value * (before + after) as f64, after)
        }
        ScaleBarFit::AdjustWidth => (rustgis_geom::nice_distance(room_m / unit_m / count) * count, after),
    };
    (total.is_finite() && total > 0.0).then_some(BarFit { total, after })
}

/// Where a scale bar draws its bar and labels, from [`bar_layout`].
pub(super) struct BarLayout {
    /// Where the bar starts, its top, its length and its height, in pixels.
    pub(super) x0: f32,
    pub(super) y0: f32,
    pub(super) len: f32,
    pub(super) bar_h: f32,
    /// Its labels' size in pixels, the middle of their line, and each drawn: its centre across
    /// and its text. One that would touch its neighbour is left out, never the ends.
    pub(super) px: f32,
    pub(super) label_y: f32,
    pub(super) labels: Vec<(f32, String)>,
    /// Its divisions before zero, all its divisions, the parts the first after zero is cut
    /// into, and its whole length in its unit: what it draws.
    pub(super) before: usize,
    pub(super) divs: usize,
    pub(super) parts: usize,
    pub(super) total: f64,
    /// Its unit label when it isn't the last label's: after the bar or on a line of its own.
    pub(super) unit: Option<BarUnit>,
    /// The furthest right its ink reaches, in pixels: its bar, its last label or its unit label.
    pub(super) right: f32,
    /// Its unit label shortened with an ellipsis or left out to fit its room: said, as a cut text.
    pub(super) unit_cut: Option<UnitLabelCut>,
    /// Its box is too small for its labels and bar: too short, the bar drawn below it or its
    /// lines below the bar past its foot, or too narrow, an end label reaching past its side
    /// further than the default size's does or, above the default size, over its first label.
    pub(super) cramped: bool,
}

/// A scale bar's unit label on a line of its own or after the bar: the middle of its line, its
/// place across (its start or its centre, as `align` says) and its text.
pub(super) struct BarUnit {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) text: String,
    pub(super) align: text::Align,
}

/// Where a scale bar in `r` draws as `bar` asks, at `ground_per_pt` ground metres a point of
/// paper: the drawing and the text-cut check ([`losses`]) take the same, so they agree. `None`
/// when it draws no bar. A label that would touch its neighbour is left out, as on any map: the
/// ends always show, and the user guide says each step is labelled where there is room. That is
/// thinning, not a loss to report.
///
/// Its labels, their room and its unit label follow its text size (#2581). Its lines above the
/// bar reach no higher than the box's top, or than a plain bar's labels reach at the default size
/// when that is higher (a publication's short box): the bar moves down to give them room. Its
/// lines below the bar reach no lower than the box's foot, the bar moving up as far as its lines
/// above leave it room. A box too short for its lines and bar is [`BarLayout::cramped`], which is
/// said, rather than any line drawn smaller. A bar with none of the details draws where it always
/// was at the default size.
pub(super) fn bar_layout(r: Rect, ground_per_pt: f64, bar: &BarOptions<'_>, ppp: f64, comma: bool) -> Option<BarLayout> {
    use rustgis_map::layout::{
        MAX_SCALE_BEFORE_ZERO, MAX_SCALE_DECIMALS, MAX_SCALE_DIVISIONS, MAX_SCALE_SUBDIVISIONS, MAX_UNIT_LABEL_CHARS,
    };
    if !(ground_per_pt.is_finite() && ground_per_pt > 0.0 && ppp.is_finite() && ppp > 0.0) {
        return None;
    }
    let p = ppp as f32;
    let k = layout_text_scale(bar.text_size, SCALE_BAR_TEXT_PT) as f32;
    let after = bar.divisions.clamp(1, MAX_SCALE_DIVISIONS);
    let before = bar.divisions_before_zero.min(MAX_SCALE_BEFORE_ZERO);
    // One part is the division whole.
    let parts = bar.subdivisions.clamp(1, MAX_SCALE_SUBDIVISIONS);
    let width_pt = f64::from(r.width()) / ppp;
    let BarPlan { unit_m, abbr, .. } = bar_plan(ground_per_pt, width_pt, bar.units)?;
    let decimals = bar.decimals.map(|d| usize::from(d).min(MAX_SCALE_DECIMALS));
    // The unit's words: its abbreviation unless the bar names it, on one line.
    let unit: String = match bar.unit_label {
        None => abbr.to_string(),
        Some(t) => t.chars().take(MAX_UNIT_LABEL_CHARS).map(|c| if matches!(c, '\n' | '\r') { ' ' } else { c }).collect(),
    };
    let has_unit = !unit.trim().is_empty();
    let unit_at = bar.unit_label_position;
    let rows = bar_rows(bar.label_position, unit_at, has_unit);
    let bar_h = (r.height() * 0.28).min(6.0 * p);
    // Its labels' size, and the default's, in pixels.
    let (px, px8) = (SCALE_BAR_TEXT_PT as f32 * k * 1.33 * p, SCALE_BAR_TEXT_PT as f32 * 1.33 * p);
    let measure_at = |s: &str, px: f32| text::measure(s, px / 1.33, false);
    let measure = |s: &str| measure_at(s, px);
    let half = |s: &str| measure(s) / 2.0;
    let gap = 3.0 * k * p;
    // The pixels a unit after the bar takes from it, at most half the bar's room.
    let beside = if has_unit && unit_at == UnitLabelPosition::AfterBar {
        (measure(&unit) + gap + p).min(r.width() * BAR_SHARE as f32 * 0.5)
    } else {
        0.0
    };
    // Division `k` from zero of a bar `total` long in `divs` divisions.
    let number = |total: f64, divs: usize, k: isize| bar_number(total * k as f64 / divs as f64, total / divs as f64, decimals, comma);
    // Left of zero the first label is a negative one, centred on the bar's end: the bar starts far
    // enough in for it, at labels `px` high and `k` times the default. It takes at most half the
    // bar's room, as a unit after the bar does: a wider one reaches left of the box rather than
    // leave the bar a sliver.
    let most_pad = ((r.width() * BAR_SHARE as f32 - beside) * 0.5).max(0.0);
    let pad_at = |f: &BarFit, px: f32, k: f32| {
        if before > 0 {
            let first = number(f.total, before + f.after, -(before as isize));
            (measure_at(&first, px) / 2.0 - BAR_INSET * k).clamp(0.0, most_pad)
        } else {
            0.0
        }
    };
    let pad_of = |f: &BarFit| pad_at(f, px, k);
    // The bar in what's left of its room once `taken` pixels of the box are its labels'.
    let fitted = |taken: f32| {
        let taken_pt = f64::from(taken) / ppp;
        let room_m = ground_per_pt * width_pt * BAR_SHARE - ground_per_pt * taken_pt;
        bar_fit(room_m, unit_m, bar.fit, before, after)
    };
    let first = fitted(beside)?;
    // The room left of zero follows the bar's length: fitted again with the first label's, and once
    // more if the shorter bar's first label is wider ("-1.25" for "-1"). Where a refit leaves no
    // room, the fit before it draws, its first label perhaps starting a little left of the box.
    let (fit, pad) = if before > 0 {
        let reserved = pad_of(&first);
        match fitted(beside + reserved) {
            Some(f) if pad_of(&f) > reserved => match fitted(beside + pad_of(&f)) {
                // A refit whose own first label is wider still would start past the room it was
                // fitted to: the fit before it draws instead.
                Some(g) if pad_of(&g) <= pad_of(&f) => (g, pad_of(&g)),
                _ => (f, reserved),
            },
            Some(f) => (f, pad_of(&f)),
            None => (first, 0.0),
        }
    } else {
        (first, 0.0)
    };
    let BarFit { total, after } = fit;
    let divs = before + after;
    let len = (total * unit_m / ground_per_pt * ppp) as f32;
    // How far in the bar starts for labels `k` times the default size: past a first label before
    // zero, or as far as the first label at zero needs ([`bar_inset`]).
    let zero = number(total, divs, 0);
    let inset_at = |k: f32, pad: f32| if before > 0 { BAR_INSET * k + pad } else { bar_inset(&zero, k, p) };
    let x0 = r.x() + inset_at(k, pad);
    // The box less the insets the bar keeps.
    let (inner_left, inner_right) = (r.x() + BAR_INSET, r.right() - BAR_INSET);
    // Division `j` counts `j - before` from zero. Its label, the last with the unit when it follows
    // the labels: a unit the bar names shortened so the label, centred on the bar's end, ends
    // inside the box, and left out when not even a letter of it fits. The abbreviation, short,
    // follows the number as it always has.
    let label = |j: usize| {
        let v = number(total, divs, j as isize - before as isize);
        if j == divs && has_unit && unit_at == UnitLabelPosition::AfterLabels {
            // Centred on the bar's end: half of it each side, inside the nearer edge.
            let room = 2.0 * (inner_right - (x0 + len)).min((x0 + len) - inner_left);
            if bar.unit_label.is_some() { last_label(&v, &unit, room, measure) } else { (format!("{v} {unit}"), None) }
        } else {
            (v, None)
        }
    };
    // Each line above the bar moves it down, each below up; labels over it alone leave it where it
    // always was.
    let centre = 0.62 + 0.12 * (rows.up as f32 - 1.0 - rows.down as f32);
    let at = r.y() + r.height() * centre - bar_h / 2.0;
    // How far `n` lines of labels `px` high reach out from the bar: the middle of the last, and
    // its digits past it.
    let reach = |n: usize, px: f32| if n == 0 { 0.0 } else { px * (BAR_LABEL_MID + 1.25 * (n - 1) as f32 + BAR_LABEL_RISE) };
    // The highest its lines may reach: the box's top, or where a plain bar's labels reach at the
    // default size when that is higher (a publication's short box), as they always have. Below
    // the bar, the box's foot.
    let plain_at = r.y() + r.height() * 0.62 - bar_h / 2.0;
    let (top, foot) = (r.y().min(plain_at - reach(1, px8)), r.bottom());
    let highest = top + reach(rows.up, px);
    let mut y0 = at.max(highest);
    if rows.down > 0 {
        y0 = y0.min(foot - bar_h - reach(rows.down, px)).max(highest);
    }
    // The middle of text line `n` out from the bar.
    let row = |n: usize| (BAR_LABEL_MID + 1.25 * n as f32) * px;
    let seg = len / divs as f32;
    let label_y = if bar.label_position == ScaleLabelPosition::Below { y0 + bar_h + row(0) } else { y0 - row(0) };
    // The last label's unit, cut to fit or not.
    let mut unit_cut = None;
    let all: Vec<(f32, String)> = (0..=divs)
        .map(|j| {
            let (s, cut) = label(j);
            unit_cut = unit_cut.or(cut);
            (x0 + seg * j as f32, s)
        })
        .collect();
    let last = all.last().map(|(x, s)| x - half(s)).unwrap_or(f32::MAX);
    let mut right_label = f32::MIN;
    let mut labels = Vec::new();
    for (i, (x, s)) in all.into_iter().enumerate() {
        let end = i == divs;
        if !end && (x - half(&s) < right_label + gap || x + half(&s) + gap > last) && i != 0 {
            continue;
        }
        right_label = x + half(&s);
        labels.push((x, s));
    }
    // The unit after the bar, level with its middle, or centred over or under it on a line of its
    // own: after the bar in the room kept for it; on a line of its own in the box's width, moved
    // in as far as it must be to stay inside it. Shortened with an ellipsis, or left out when not
    // even its first character fits.
    let place_unit = || {
        let mid = x0 + len / 2.0;
        let (x, y, align) = match unit_at {
            UnitLabelPosition::AfterLabels => return (None, None),
            UnitLabelPosition::AfterBar => (x0 + len + gap + p, y0 + bar_h / 2.0, text::Align::Left),
            UnitLabelPosition::Above => (mid, y0 - row(rows.unit_row), text::Align::Center),
            UnitLabelPosition::Below => (mid, y0 + bar_h + row(rows.unit_row), text::Align::Center),
        };
        let room = if unit_at == UnitLabelPosition::AfterBar { beside - gap - p } else { inner_right - inner_left };
        let Some(text) = shortened(&unit, room, measure) else { return (None, Some(UnitLabelCut::LeftOut)) };
        let cut = (text != unit).then_some(UnitLabelCut::Shortened);
        let x = if matches!(align, text::Align::Center) {
            let w = measure(&text) / 2.0;
            x.min(inner_right - w).max(inner_left + w)
        } else {
            x
        };
        (Some(BarUnit { x, y, text, align }), cut)
    };
    let (unit, placed_cut) = if has_unit { place_unit() } else { (None, None) };
    let unit_cut = unit_cut.or(placed_cut);
    let unit_right =
        unit.as_ref().map_or(f32::MIN, |u| if matches!(u.align, text::Align::Left) { u.x + measure(&u.text) } else { u.x + half(&u.text) });
    let right = (x0 + len).max(right_label).max(unit_right);
    // Sideways the bar keeps its length: its first label is kept within its reach by the inset,
    // and its last reaches no further past the box's right side than the default size's does, or
    // than the side itself; further out, the box is too narrow for it, as it is too short when the
    // bar is below it.
    let half8 = |s: &str| measure_at(s, px8) / 2.0;
    let x8 = r.x() + inset_at(1.0, pad_at(&fit, px8, 1.0));
    let past_side = labels.last().is_some_and(|(x, s)| x + half(s) > r.right().max(x8 + (x - x0) + half8(s)) + 0.01);
    let low = y0 + bar_h + reach(rows.down, px) > foot + 0.01;
    // Its end label is drawn as its first is, whatever the room: above the default size, running
    // into the first, the box is too narrow for them too.
    let clash = k > 1.0
        && match (labels.first(), labels.last()) {
            (Some((fx, fs)), Some((ex, es))) if labels.len() > 1 => ex - half(es) < fx + half(fs),
            _ => false,
        };
    let cramped = low || past_side || clash;
    Some(BarLayout { x0, y0, len, bar_h, px, label_y, labels, before, divs, parts, total, unit, right, unit_cut, cramped })
}

/// A scale bar filling most of the element's width as `bar` asks, placed by [`bar_layout`]: its
/// divisions after zero (and any before it) labelled, the first after zero split into its
/// subdivisions. `ground_per_pt` is ground metres per logical point of paper; `colors` are its
/// lines, dark divisions and labels, then the alternating bar's light divisions.
pub(super) fn scale_bar(pm: &mut Canvas, r: Rect, ground_per_pt: f64, bar: &BarOptions<'_>, colors: (Rgba, Rgba), ppp: f64, comma: bool) {
    let Some(BarLayout { x0, y0, len, bar_h, px, label_y, labels, before, divs, parts, unit, .. }) =
        bar_layout(r, ground_per_pt, bar, ppp, comma)
    else {
        return;
    };
    let (dark, light) = colors;
    let seg = len / divs as f32;
    let zero_x = x0 + seg * before as f32;
    // Where the first division after zero is cut into its subdivisions.
    let cuts: Vec<f32> = (1..parts).map(|p| zero_x + seg * p as f32 / parts as f32).collect();
    let dark_at = |j: usize| (j as isize - before as isize).rem_euclid(2) == 0;
    // An alternating bar's fills alternate from zero, the cut division's parts counted one by one:
    // the first part dark, the divisions after them going on from the last part.
    let filled_dark = |j: usize| if j < before { dark_at(j) } else { (parts - 1 + (j - before)).is_multiple_of(2) };
    let line = Stroke { width: 0.6 * ppp as f32, ..Stroke::default() };
    let stroke = |pm: &mut Canvas, pts: &[(f32, f32)], w: &Stroke| {
        let mut pb = PathBuilder::new();
        for (i, (x, y)) in pts.iter().enumerate() {
            if i == 0 { pb.move_to(*x, *y) } else { pb.line_to(*x, *y) }
        }
        if let Some(p) = pb.finish() {
            pm.stroke_path(&p, &paint(dark, 1.0), w, Transform::identity(), None);
        }
    };
    match bar.style {
        ScaleBarStyle::Alternating => {
            for j in 0..divs {
                let x = x0 + seg * j as f32;
                if j == before && parts > 1 {
                    // Alternating parts from zero, the first dark.
                    let part = seg / parts as f32;
                    for p in 0..parts {
                        let c = if p.is_multiple_of(2) { dark } else { light };
                        if let Some(b) = Rect::from_xywh(x + part * p as f32, y0, part, bar_h) {
                            pm.fill_rect(b, &paint(c, 1.0), Transform::identity(), None);
                        }
                    }
                } else if let Some(b) = Rect::from_xywh(x, y0, seg, bar_h) {
                    let c = if filled_dark(j) { dark } else { light };
                    pm.fill_rect(b, &paint(c, 1.0), Transform::identity(), None);
                }
            }
            if let Some(b) = Rect::from_xywh(x0, y0, len, bar_h) {
                pm.stroke_path(&PathBuilder::from_rect(b), &paint(dark, 1.0), &line, Transform::identity(), None);
            }
        }
        ScaleBarStyle::Stepped => {
            let mut pts = vec![(x0, y0 + bar_h)];
            // Its upper steps are an alternating bar's dark divisions; the cut division is upper.
            for j in 0..divs {
                let y = if j == before || filled_dark(j) { y0 } else { y0 + bar_h };
                let (a, b) = (x0 + seg * j as f32, x0 + seg * (j + 1) as f32);
                pts.extend([(a, y), (b, y)]);
            }
            pts.push((x0 + len, y0 + bar_h));
            stroke(pm, &pts, &Stroke { width: 1.0 * ppp as f32, ..Stroke::default() });
            // The first division after zero is on the upper step: its ticks hang from it.
            for &x in &cuts {
                stroke(pm, &[(x, y0), (x, y0 + bar_h * 0.5)], &line);
            }
        }
        ScaleBarStyle::Line => {
            let base = y0 + bar_h;
            stroke(pm, &[(x0, base), (x0 + len, base)], &Stroke { width: 1.0 * ppp as f32, ..Stroke::default() });
            // Tall ticks at the ends and at zero.
            for j in 0..=divs {
                let tall = if j == 0 || j == divs || j == before { bar_h } else { bar_h * 0.6 };
                let x = x0 + seg * j as f32;
                stroke(pm, &[(x, base), (x, base - tall)], &line);
            }
            for &x in &cuts {
                stroke(pm, &[(x, base), (x, base - bar_h * 0.3)], &line);
            }
        }
    }
    // Labels at the divisions, over or under the bar, and the unit label where it is asked.
    for (x, s) in &labels {
        text::text_at(pm, s, *x, label_y, px, dark, false, text::Align::Center, false);
    }
    if let Some(BarUnit { x, y, text: shown, align }) = unit {
        text::text_at(pm, &shown, x, y, px, dark, false, align, false);
    }
}

/// The last label of a scale bar, its number `v` followed by `unit`, as it fits `room` pixels
/// measured by `measure`: the unit shortened with an ellipsis, never the number, and left out when
/// not even its first character fits, with how it was cut.
pub(super) fn last_label(v: &str, unit: &str, room: f32, measure: impl Fn(&str) -> f32) -> (String, Option<UnitLabelCut>) {
    let whole = format!("{v} {unit}");
    if measure(&whole) <= room {
        return (whole, None);
    }
    let after = measure(&format!("{v} "));
    match shortened(unit, room - after, &measure) {
        Some(u) => (format!("{v} {u}"), Some(UnitLabelCut::Shortened)),
        None => (v.to_string(), Some(UnitLabelCut::LeftOut)),
    }
}

/// `t` as it fits `room` pixels measured by `measure`: whole, or cut a character at a time and
/// ended with an ellipsis, keeping at least one character. `None` when not even that fits. A
/// longer prefix measures no less, so the longest that fits is found by halving.
pub(super) fn shortened(t: &str, room: f32, measure: impl Fn(&str) -> f32) -> Option<String> {
    if measure(t) <= room {
        return Some(t.to_string());
    }
    let chars: Vec<char> = t.chars().collect();
    let cut = |n: usize| format!("{}…", chars.iter().take(n).collect::<String>().trim_end());
    // The fewest characters, from 1, that don't fit: `chars.len()` when all shorter ones do.
    let (mut lo, mut hi) = (1, chars.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if measure(&cut(mid)) <= room { lo = mid + 1 } else { hi = mid }
    }
    (lo > 1).then(|| cut(lo - 1))
}

/// The text lines a scale bar has above and below it, and the line its unit takes on its side
/// (0 next to the bar, 1 past its labels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BarRows {
    pub(super) up: usize,
    pub(super) down: usize,
    pub(super) unit_row: usize,
}

/// The lines of a scale bar with its labels at `labels` and its unit (if `has_unit`) at `unit`.
pub(super) fn bar_rows(labels: ScaleLabelPosition, unit: UnitLabelPosition, has_unit: bool) -> BarRows {
    let labels_up = labels == ScaleLabelPosition::Above;
    // The side the unit has a line of its own on: true above.
    let unit_up = match unit {
        UnitLabelPosition::Above if has_unit => Some(true),
        UnitLabelPosition::Below if has_unit => Some(false),
        _ => None,
    };
    BarRows {
        up: usize::from(labels_up) + usize::from(unit_up == Some(true)),
        down: usize::from(!labels_up) + usize::from(unit_up == Some(false)),
        unit_row: usize::from(unit_up == Some(labels_up)),
    }
}
