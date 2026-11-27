//! The chart element: bars, values and key.

use super::*;

/// A bar chart of `data`'s bars ([`ElementKind::chart_data`]) drawn as `look` says, its title,
/// labels and values in `dark`, its values with a decimal comma when `comma`. With a negative
/// value, bars run either way from a zero line (#2487). Stacked, each bar is drawn as its
/// stretches in the colours of the key under the title, those above zero from it on and those
/// below from it back, its value past the end of the side it ends on.
pub(super) fn chart(pm: &mut Canvas, r: Rect, data: &rustgis_map::layout::ChartData, look: &ChartLook, dark: Rgba, ppp: f64, comma: bool) {
    let ppp = ppp as f32;
    let vals = &data.bars;
    let (min, reach) = chart_span(data);
    // A fixed maximum is the far end of the bars' room, whatever they reach (#2487 part 4).
    let max = look.max.unwrap_or(reach);
    if vals.is_empty() || (reach <= 0.0 && min >= 0.0) {
        placeholder(pm, r, "Chart", ppp as f64);
        return;
    }
    let px = 7.5 * 1.33 * ppp;
    let Some((title_h, key_h)) = chart_head(r, data, look, px, ppp) else {
        placeholder(pm, r, "Chart", ppp as f64);
        return;
    };
    if !data.title.is_empty() {
        text::text_at(pm, &data.title, r.x(), r.y() + title_h / 2.0, px * 1.15, dark, true, text::Align::Left, false);
    }
    chart_key(Some(pm), r, r.y() + title_h, &data.key, px, ppp, dark);
    // Each bar's value as written past its end, when it has them.
    let values: Vec<String> = if look.values {
        vals.iter().map(|(v, _)| decimal(rustgis_map::layout::chart_value_text(*v, look.decimals), comma)).collect()
    } else {
        Vec::new()
    };
    let bars = ChartBars {
        r,
        title_h: title_h + key_h,
        vals,
        values: &values,
        colors: &data.colors,
        stretches: &data.stretches,
        key: &data.key,
        min,
        max,
        px,
        ppp,
        dark,
        look,
    };
    let labelled = data.labelled && !look.hide_labels;
    match look.orientation {
        Orientation::Horizontal => bars.across(pm, labelled),
        Orientation::Vertical => bars.columns(pm, labelled),
    }
}

/// The span a chart's bars run over: from the most a bar reaches below zero to the most above,
/// stacked its stretches below zero added up and those above.
pub fn chart_span(data: &rustgis_map::layout::ChartData) -> (f64, f64) {
    data.bars
        .iter()
        .enumerate()
        .map(|(i, (v, _))| data.stretches.get(i).filter(|s| !s.is_empty()).map_or_else(|| reach(&[*v]), |s| reach(s)))
        .fold((0.0_f64, 0.0_f64), |(lo, hi), (l, h)| (lo.min(l), hi.max(h)))
}

/// The heights of a chart's title and key over its bars in `r`, its text `px` high: no title
/// leaves its room to the bars. Stacked, a box too short to leave the bars a line under its title
/// and key has none (it draws its placeholder rather than a key past its bottom and bars with no
/// room); as columns, the bars' line is past the room their labels and values may take (written
/// at most `px` high, [`ChartBars::columns`]), so they always have it. Not stacked, it draws as
/// before.
pub(super) fn chart_head(r: Rect, data: &rustgis_map::layout::ChartData, look: &ChartLook, px: f32, ppp: f32) -> Option<(f32, f32)> {
    let title_h = if data.title.is_empty() { 0.0 } else { px * 1.6 };
    let key_h = chart_key(None, r, 0.0, &data.key, px, ppp, INK);
    let line = px * 1.4;
    let below = match look.orientation {
        Orientation::Horizontal => 0.0,
        Orientation::Vertical => {
            let (min, max) = chart_span(data);
            let values = |on: bool| if look.values && on { line } else { 0.0 };
            (if data.labelled && !look.hide_labels { line } else { 0.0 }) + values(max > 0.0) + values(min < 0.0)
        }
    };
    (data.key.is_empty() || title_h + key_h + line + below <= r.height()).then_some((title_h, key_h))
}

/// Whether a chart with bars to draw, `w` × `h` mm in its look, draws its placeholder instead as
/// its title and key leave the bars no room ([`chart_head`]): what the commands warn of, so the page and the commands
/// never tell it apart.
pub fn chart_crowded(data: &rustgis_map::layout::ChartData, look: &ChartLook, w: f64, h: f64) -> bool {
    let Some(r) = Rect::from_xywh(0.0, 0.0, (w * PT_PER_MM).max(1.0) as f32, (h * PT_PER_MM).max(1.0) as f32) else { return false };
    !data.bars.is_empty() && chart_head(r, data, look, 7.5 * 1.33, 1.0).is_none()
}

/// How far a bar's stretches reach below zero (its negative ones added up) and above it.
pub(super) fn reach(stretches: &[f64]) -> (f64, f64) {
    stretches.iter().fold((0.0, 0.0), |(lo, hi), v| if *v < 0.0 { (lo + v, hi) } else { (lo, hi + v) })
}

/// A stacked chart's key from `top`: a swatch in each value's colour and its name, in rows as
/// wide as the box, a name cut to fit one; the height it takes, 0 without a key, infinite in a
/// box narrower than a swatch. Without a canvas it only measures.
pub(super) fn chart_key(mut pm: Option<&mut Canvas>, r: Rect, top: f32, key: &[(String, Rgba)], px: f32, ppp: f32, dark: Rgba) -> f32 {
    if key.is_empty() {
        return 0.0;
    }
    let size = px * 0.9;
    let line_h = px * 1.4;
    let (sw, gap) = (size * 0.8, 2.0 * ppp);
    // A box narrower than a swatch has no room for the key at all.
    if r.width() < sw + gap {
        return f32::INFINITY;
    }
    let (mut x, mut y) = (r.x(), top);
    for (name, colour) in key {
        let t = fit_label(name, |t| sw + gap + text::measure(t, size / 1.33, false) > r.width());
        let w = sw + gap + text::measure(&t, size / 1.33, false);
        // A name that doesn't fit after the others starts a row of its own.
        if x > r.x() && x + w > r.right() {
            (x, y) = (r.x(), y + line_h);
        }
        if let Some(pm) = pm.as_deref_mut() {
            fill(pm, *colour, x, y + (line_h - sw) / 2.0, sw, sw);
            text::text_at(pm, &t, x + sw + gap, y + line_h / 2.0, size, dark, false, text::Align::Left, false);
        }
        x += w + 3.0 * gap;
    }
    y + line_h - top
}

/// `name`'s first 30 characters, cut from their end while `too_wide` says they don't fit.
pub(super) fn fit_label(name: &str, too_wide: impl Fn(&str) -> bool) -> String {
    let mut t: String = name.chars().take(30).collect();
    while !t.is_empty() && too_wide(&t) {
        t.pop();
    }
    t
}

/// What [`chart`] draws its bars from: its box, the height of its title (and key) over the bars,
/// the bars and their values written out (none without), each bar's own colour (none: the look's
/// bar colour), stacked each bar's stretches and the key that colours them, the least a bar
/// reaches and the value at the far end of the bars' room (the greatest a bar reaches, or a fixed
/// maximum in its place; 0 among them), its text size and pixels per point.
pub(super) struct ChartBars<'a> {
    pub(super) r: Rect,
    pub(super) title_h: f32,
    pub(super) vals: &'a [(f64, String)],
    pub(super) values: &'a [String],
    pub(super) colors: &'a [Option<Rgba>],
    pub(super) stretches: &'a [Vec<f64>],
    pub(super) key: &'a [(String, Rgba)],
    pub(super) min: f64,
    pub(super) max: f64,
    pub(super) px: f32,
    pub(super) ppp: f32,
    pub(super) dark: Rgba,
    pub(super) look: &'a ChartLook,
}

impl ChartBars<'_> {
    /// Bar `i` (of value `v`) as the stretches it is drawn in, each with the share of the span
    /// ([`ChartBars::share`]) where it starts from zero, its own share (negative running back) and
    /// its colour. A whole bar (none stacked, or no stretches) is one stretch, in its own colour or
    /// the look's; a stacked one a stretch per value of the key, those above zero on from it and
    /// those below back from it, one of 0 not drawn.
    pub(super) fn stretches(&self, i: usize, v: f64) -> Vec<(f32, f32, Rgba)> {
        let Some(s) = self.stretches.get(i).filter(|s| !s.is_empty()) else {
            return vec![(0.0, self.share(v.min(self.max)), self.colors.get(i).copied().flatten().unwrap_or(self.look.bar_color))];
        };
        let (mut lo, mut hi) = (0.0_f64, 0.0_f64);
        // Past a fixed maximum a stretch above zero is cut at it, and one wholly past it isn't
        // drawn; one running back from zero is drawn whole.
        s.iter()
            .zip(self.key)
            .filter(|(v, _)| **v != 0.0)
            .filter_map(|(v, (_, c))| {
                let end = if *v < 0.0 { &mut lo } else { &mut hi };
                let from = *end;
                *end += v;
                if *v < 0.0 {
                    return Some((self.share(from), self.share(*v), *c));
                }
                let len = (from + v).min(self.max) - from;
                (len > 0.0).then(|| (self.share(from), self.share(len), *c))
            })
            .collect()
    }

    /// What part `v` is of the span from the lowest value (or zero) to the highest (or zero). A span
    /// past a number's range (the largest number and its opposite) is taken by halves.
    pub(super) fn share(&self, v: f64) -> f32 {
        let span = self.max - self.min;
        (if span.is_finite() { v / span } else { (v / 2.0) / (self.max / 2.0 - self.min / 2.0) }) as f32
    }

    /// The widest of the values written at `size` pixels past the bars running one way:
    /// positive ones, or negative ones (`negative`); 0 without values.
    pub(super) fn widest(&self, size: f32, negative: bool) -> f32 {
        self.vals
            .iter()
            .zip(self.values)
            .filter(|((v, _), _)| (*v < 0.0) == negative)
            .map(|(_, t)| text::measure(t, size / 1.33, false))
            .fold(0.0, f32::max)
    }

    /// Bars running across, labels in a column at their left, each value past its bar's end with
    /// room kept for it at the side its bar runs to.
    pub(super) fn across(&self, pm: &mut Canvas, labelled: bool) {
        let (r, ppp, px) = (self.r, self.ppp, self.px);
        let label_w = if labelled { r.width() * 0.32 } else { 0.0 };
        let bh = ((r.height() - self.title_h) / self.vals.len() as f32).min(18.0 * ppp);
        let size = px.min(bh * 0.9);
        let gap = 2.0 * ppp;
        let room = |negative: bool, size: f32| {
            let w = self.widest(size, negative);
            if w > 0.0 { w + gap } else { 0.0 }
        };
        // Values take at most half the width beside the labels, written smaller past that.
        let beside = r.width() - label_w - 4.0;
        let mut value_size = size;
        let (mut room_lo, mut room_hi) = (room(true, size), room(false, size));
        if room_lo + room_hi > beside * 0.5 {
            // Only the text grows smaller: the gaps beside it stay as they are.
            let gaps = [room_lo, room_hi].iter().filter(|w| **w > 0.0).count() as f32 * gap;
            let text = room_lo + room_hi - gaps;
            value_size = (size * (beside * 0.5 - gaps) / text).max(0.0);
            (room_lo, room_hi) = (room(true, value_size), room(false, value_size));
        }
        let bar_area = beside - room_lo - room_hi;
        // Where the bars start: the left of the bar area when none is negative.
        let zero = r.x() + label_w + room_lo + bar_area * self.share(-self.min);
        let right = self.look.label_align == TextAlign::Right;
        // Laid out the usual way, then placed as its mirror image when mirrored: a box from `x`,
        // `w` wide, and a text written from `x`, the text itself reading as ever.
        let edges = r.x() + r.right();
        let place = |x: f32, w: f32| if self.look.mirror { edges - x - w } else { x };
        let write = |x: f32, align: text::Align| if self.look.mirror { (edges - x, mirrored(align)) } else { (x, align) };
        for (i, (v, name)) in self.vals.iter().enumerate() {
            let y = r.y() + self.title_h + bh * i as f32;
            // Each stretch at least a pixel wide, running back from its start below zero; where the
            // bar ends at its left and at its right.
            let (mut left, mut right_end) = (zero, zero);
            let mut drawn = Vec::new();
            for (start, share, colour) in self.stretches(i, *v) {
                let w = (bar_area * share.abs()).max(1.0);
                let x = zero + bar_area * start - if share < 0.0 { w } else { 0.0 };
                drawn.push((place(x, w), w, colour));
                (left, right_end) = (left.min(x), right_end.max(x + w));
            }
            self.bar(pm, &drawn, (y + bh * 0.15, bh * 0.7), true);
            if labelled {
                let t = fit_label(name, |t| text::measure(t, 7.5, false) * ppp > label_w - 4.0 * ppp);
                // At the column's right, the label ends where the bars' side begins.
                let (lx, align) = if right { (r.x() + label_w - 2.0 * ppp, text::Align::Right) } else { (r.x(), text::Align::Left) };
                let (lx, align) = write(lx, align);
                text::text_at(pm, &t, lx, y + bh / 2.0, size, self.dark, false, align, false);
            }
            if let Some(t) = self.values.get(i) {
                let (vx, align) = if *v < 0.0 { (left - gap, text::Align::Right) } else { (right_end + gap, text::Align::Left) };
                let (vx, align) = write(vx, align);
                text::text_at(pm, t, vx, y + bh / 2.0, value_size, self.dark, false, align, false);
            }
        }
        if self.min < 0.0
            && let Some(line) =
                Rect::from_xywh(place(zero - 0.375 * ppp, 0.75 * ppp), r.y() + self.title_h, 0.75 * ppp, bh * self.vals.len() as f32)
        {
            pm.fill_rect(line, &paint(self.dark, 1.0), Transform::identity(), None);
        }
    }

    /// Columns standing on a zero line, each label centred under its column and cut to its width,
    /// each value over a column (under one going down), made smaller to fit a narrow column.
    pub(super) fn columns(&self, pm: &mut Canvas, labelled: bool) {
        let (r, ppp, px) = (self.r, self.ppp, self.px);
        let slot = r.width() / self.vals.len() as f32;
        let widest = self.widest(px, false).max(self.widest(px, true));
        let value_px = if widest > slot * 0.95 { px * slot * 0.95 / widest } else { px };
        let value_h = value_px * 1.4;
        let room_hi = if self.max > 0.0 && !self.values.is_empty() { value_h } else { 0.0 };
        let room_lo = if self.min < 0.0 && !self.values.is_empty() { value_h } else { 0.0 };
        let label_h = if labelled { px * 1.4 } else { 0.0 };
        let top = r.y() + self.title_h + room_hi;
        let plot_h = r.bottom() - label_h - room_lo - top;
        if plot_h <= 0.0 {
            placeholder(pm, r, "Chart", ppp as f64);
            return;
        }
        // Where the columns stand: the bottom of the plot when none is negative.
        let zero = top + plot_h * self.share(self.max);
        // Laid out the usual way under its title, then placed upside down there when mirrored: a box
        // from `y`, `h` high, and a text centred on `y`, the text itself reading as ever.
        let edges = r.y() + self.title_h + r.bottom();
        let place = |y: f32, h: f32| if self.look.mirror { edges - y - h } else { y };
        let write = |y: f32| if self.look.mirror { edges - y } else { y };
        for (i, (v, name)) in self.vals.iter().enumerate() {
            let cx = r.x() + slot * (i as f32 + 0.5);
            // Each stretch at least a pixel high, standing up from its start above zero and hanging
            // down below it; where the column ends at its top and at its bottom.
            let (mut top_end, mut bottom) = (zero, zero);
            let mut drawn = Vec::new();
            for (start, share, colour) in self.stretches(i, *v) {
                let h = (plot_h * share.abs()).max(1.0);
                let y = zero - plot_h * start - if share < 0.0 { 0.0 } else { h };
                drawn.push((place(y, h), h, colour));
                (top_end, bottom) = (top_end.min(y), bottom.max(y + h));
            }
            self.bar(pm, &drawn, (cx - slot * 0.35, slot * 0.7), false);
            if let Some(t) = self.values.get(i) {
                let vy = if *v < 0.0 { bottom + value_h / 2.0 } else { top_end - value_h / 2.0 };
                text::text_at(pm, t, cx, write(vy), value_px, self.dark, false, text::Align::Center, false);
            }
            if labelled {
                let t = fit_label(name, |t| text::measure(t, px / 1.33, false) > slot - 2.0 * ppp);
                text::text_at(pm, &t, cx, write(r.bottom() - label_h / 2.0), px, self.dark, false, text::Align::Center, false);
            }
        }
        if self.min < 0.0
            && let Some(line) = Rect::from_xywh(r.x(), place(zero - 0.375 * ppp, 0.75 * ppp), r.width(), 0.75 * ppp)
        {
            pm.fill_rect(line, &paint(self.dark, 1.0), Transform::identity(), None);
        }
    }
}

/// A text's alignment in a chart's mirror image: from the right where it was from the left.
pub(super) fn mirrored(align: text::Align) -> text::Align {
    match align {
        text::Align::Left => text::Align::Right,
        text::Align::Right => text::Align::Left,
        other => other,
    }
}

impl ChartBars<'_> {
    /// A bar, filled stretch by stretch, with the look's outline round it (#2487 part 4b). Each
    /// stretch is its start and length along the bar and its colour; `cross` is where the bar
    /// starts across its run and how thick it is, and `across` says it runs across (else down).
    /// The line runs just inside the bar's edge, so it takes no room the bars leave to their
    /// values, labels and title, and is laid once between two stretches, centred where they meet,
    /// so a see-through one reads the same everywhere. So that every stretch keeps its colour, the
    /// edge's line narrows to a third of the bar's thickness, across each end to a third of the
    /// stretch there too, and each line between two stretches to a third of either, kept within
    /// the edge's line.
    pub(super) fn bar(&self, pm: &mut Canvas, stretches: &[(f32, f32, Rgba)], cross: (f32, f32), across: bool) {
        // A box from where it starts and how long it is along the run, and across it.
        let rect = |along: f32, run: f32, across_at: f32, thick: f32| {
            if across { (along, across_at, run, thick) } else { (across_at, along, thick, run) }
        };
        for (at, len, colour) in stretches {
            let (x, y, w, h) = rect(*at, *len, cross.0, cross.1);
            fill(pm, *colour, x, y, w, h);
        }
        let Some(o) = self.look.bar_outline else { return };
        let mut order: Vec<_> = stretches.iter().map(|(at, len, _)| (*at, *len)).collect();
        order.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (Some(first), Some(last)) = (order.first(), order.last()) else { return };
        let (lo, hi) = (first.0, last.0 + last.1);
        let line = (o.width as f32 * self.ppp).min(cross.1 / 3.0);
        if line <= 0.0 {
            return;
        }
        // Across the ends, narrowed only there by the stretch it would cover.
        let (start, end) = (line.min(first.1 / 3.0), line.min(last.1 / 3.0));
        // The whole line, round the bar and between its stretches, is one shape filled once: boxes
        // filled each on its own would cover a pixel they share in part twice over, leaving a
        // lighter seam where they meet and a darker spot where they overlap. Boxes, not a stroke:
        // a stroke thinner than a pixel is drawn a pixel wide, past the bar's end.
        let mut shape = PathBuilder::new();
        let mut boxed = |(x, y, w, h): (f32, f32, f32, f32)| {
            if let Some(b) = Rect::from_xywh(x, y, w, h) {
                shape.push_rect(b);
            }
        };
        boxed(rect(lo, hi - lo, cross.0, line));
        boxed(rect(lo, hi - lo, cross.0 + cross.1 - line, line));
        boxed(rect(lo, start, cross.0 + line, cross.1 - 2.0 * line));
        boxed(rect(hi - end, end, cross.0 + line, cross.1 - 2.0 * line));
        // Between two stretches, inside the edge's line, and kept there along the bar too: a
        // stretch drawn a pixel long though its share is shorter has the next start inside it,
        // near the bar's end.
        for pair in order.windows(2) {
            let &[(_, a), (at, b)] = pair else { continue };
            let d = line.min(a / 3.0).min(b / 3.0);
            let (from, to) = ((at - d / 2.0).max(lo + start), (at + d / 2.0).min(hi - end));
            boxed(rect(from, to - from, cross.0 + line, cross.1 - 2.0 * line));
        }
        if let Some(shape) = shape.finish() {
            pm.fill_path(&shape, &paint(o.color, 1.0), FillRule::Winding, Transform::identity(), None);
        }
    }
}
