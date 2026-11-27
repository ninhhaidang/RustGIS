//! The colorbar and its title.

use super::*;

/// How a colorbar is laid out.
pub(super) struct Bar<'a> {
    pub(super) title: &'a str,
    pub(super) orientation: Orientation,
    pub(super) ticks: usize,
    pub(super) side: TickSide,
    pub(super) reverse: bool,
    /// Labels with a decimal comma ([`Layout::decimal_comma`]).
    pub(super) comma: bool,
    /// Its title, outline, ticks and labels (#2257).
    pub(super) ink: Rgba,
}

/// `n` values from `lo` to `hi`, the ends included, with the fewest decimals that keep each within
/// a twentieth of the step of its value.
pub(super) fn tick_labels(lo: f64, hi: f64, n: usize) -> Vec<(f64, String)> {
    if !(lo.is_finite() && hi.is_finite()) {
        return Vec::new();
    }
    let n = if hi > lo { n.clamp(2, rustgis_map::layout::MAX_COLORBAR_TICKS) } else { 1 };
    let step = if n > 1 { (hi - lo) / (n - 1) as f64 } else { 0.0 };
    let values: Vec<f64> = (0..n).map(|i| if i + 1 == n { hi } else { lo + step * i as f64 }).collect();
    if values.iter().any(|v| v.abs() >= 1e6) {
        return values.into_iter().map(|v| (v, rustgis_map::symbology::fmt_num(v))).collect();
    }
    let tol = if step > 0.0 { step * 0.05 } else { (lo.abs() * 1e-3).max(1e-9) };
    let decimals = (0..=6)
        .find(|d| {
            let k = 10f64.powi(*d);
            values.iter().all(|v| ((v * k).round() / k - v).abs() <= tol)
        })
        .unwrap_or(6) as usize;
    // Rounded first, then + 0.0, so a value rounded to zero isn't printed "-0".
    let k = 10f64.powi(decimals as i32);
    values.into_iter().map(|v| (v, format!("{:.*}", decimals, (v * k).round() / k + 0.0))).collect()
}

/// A colorbar laid out in its element ([`colorbar_plan`]): what [`colorbar`] draws, and what the
/// page's cut-off check ([`text_losses`]) says of it, from one layout.
pub(super) struct ColorbarPlan {
    /// The title's lines drawn, each with the middle of its line (pixels).
    pub(super) title: Vec<(String, f32)>,
    /// The bar's blocks or samples, low to high, and its rectangle (x, y, width, height).
    pub(super) blocks: Vec<Rgba>,
    pub(super) rect: (f32, f32, f32, f32),
    /// The bar runs down the page, and from high to low along it (its start is left or top).
    pub(super) vertical: bool,
    pub(super) flip: bool,
    /// The labels drawn: each one's text, its tick's two ends, where it is drawn, and whether
    /// centred on it (else from its left).
    pub(super) labels: Vec<(String, [(f32, f32); 2], (f32, f32), bool)>,
    /// Labels left out because they would spill out of the element (#2638), not those thinned
    /// for clearing the one before: a short or narrow element, or one with no room for a bar.
    pub(super) spilled: usize,
}

/// A colorbar in `r` at `ppp` (#2638): its title's lines that fit over the bar's own room
/// ([`colorbar_title_drawn`]), the bar, leaving room for the labels on their side and for the end
/// labels to stand past its ends, and each label drawn only if it clears the one before and stays
/// inside the element.
pub(super) fn colorbar_plan(r: Rect, scale: &ColorScale, bar: &Bar, ppp: f32) -> ColorbarPlan {
    let px = 8.0 * 1.33 * ppp;
    let width = |s: &str| text::measure(s, px / 1.33, false);
    let (mut top, bottom) = (r.y(), r.y() + r.height());
    let mut title = Vec::new();
    if !bar.title.is_empty() {
        let tpx = COLORBAR_TITLE_PT as f32 * 1.33 * ppp;
        // Wrapped to the element's width (#2493).
        let lines = colorbar_title_lines(bar.title, f64::from(r.width()) / (f64::from(ppp) * PT_PER_MM));
        let drawn = colorbar_title_drawn(lines.len(), top, bottom - colorbar_bar_room(ppp, bar.orientation), tpx);
        title = lines.into_iter().take(drawn).enumerate().map(|(i, line)| (line, top + tpx * (0.6 + 1.25 * i as f32))).collect();
        // A title held back whole takes no room: the bar starts at the element's top.
        if drawn > 0 {
            top += tpx * (1.4 + 1.25 * (drawn - 1) as f32);
        }
    }
    // Where each block or sample sits along the bar (0 low to 1 high), and the labels with theirs.
    let (blocks, labels): (Vec<Rgba>, Vec<(f32, String)>) = match scale {
        ColorScale::Ramp { lo, hi, colors } => {
            let span = hi - lo;
            let labels = tick_labels(*lo, *hi, bar.ticks)
                .into_iter()
                .map(|(v, s)| ((if span > 0.0 { (v - lo) / span } else { 0.5 }) as f32, s))
                .collect();
            (colors.clone(), labels)
        }
        ColorScale::Steps { edges, colors } => {
            let n = colors.len().max(1) as f32;
            let labels = edges.iter().enumerate().map(|(i, v)| (i as f32 / n, rustgis_map::symbology::fmt_num(*v))).collect();
            (colors.clone(), labels)
        }
        ColorScale::Values(v) => {
            let n = v.len().max(1) as f32;
            let labels = v.iter().enumerate().map(|(i, (val, _))| ((i as f32 + 0.5) / n, rustgis_map::symbology::fmt_num(*val))).collect();
            (v.iter().map(|(_, c)| *c).collect(), labels)
        }
    };
    let vertical = bar.orientation == Orientation::Vertical;
    let flip = if vertical { !bar.reverse } else { bar.reverse };
    let mut plan = ColorbarPlan { title, blocks: Vec::new(), rect: (0.0, 0.0, 0.0, 0.0), vertical, flip, labels: Vec::new(), spilled: 0 };
    if blocks.is_empty() {
        return plan;
    }
    // No room for a bar: none is drawn, nor its labels.
    if r.width() < 2.0 || bottom - top < 2.0 {
        plan.spilled = labels.len();
        return plan;
    }
    let labels: Vec<(f32, String)> = labels.into_iter().map(|(at, s)| (at, decimal(s, bar.comma))).collect();
    let tick = 2.5 * ppp;
    let gap = 1.5 * ppp;
    let (bx, by, bw, bh) = if vertical {
        let label_w = labels.iter().map(|(_, s)| width(s)).fold(0.0f32, f32::max);
        let bw = (r.width() - label_w - tick - gap).clamp(2.0 * ppp, (r.width() * 0.45).max(2.0 * ppp)).min(r.width());
        let inset = (px / 2.0).min((bottom - top) * 0.25);
        let bx = if bar.side == TickSide::Start { r.x() + r.width() - bw } else { r.x() };
        (bx, top + inset, bw, (bottom - top - 2.0 * inset).max(1.0))
    } else {
        let ends = labels.first().map(|(_, s)| width(s)).unwrap_or(0.0).max(labels.last().map(|(_, s)| width(s)).unwrap_or(0.0));
        let inset = (ends / 2.0).min(r.width() * 0.25);
        // Too short for its labels (left out below, and said), the bar fills the element.
        let bh = match bottom - top - px * 1.2 - tick - gap {
            h if h >= 3.0 * ppp => h,
            _ => bottom - top,
        };
        let by = if bar.side == TickSide::Start { bottom - bh } else { top };
        (r.x() + inset, by, (r.width() - 2.0 * inset).max(1.0), bh)
    };
    plan.blocks = blocks;
    plan.rect = (bx, by, bw, bh);
    let len = if vertical { bh } else { bw };
    let along = |t: f32| if flip { (1.0 - t) * len } else { t * len };
    // A label (and its tick) is left out where it would spill out of a short or narrow element.
    let inside =
        |x: f32, y: f32, w: f32, h: f32| x >= r.x() - 1.0 && y >= r.y() - 1.0 && x + w <= r.right() + 1.0 && y + h <= r.bottom() + 1.0;
    // Labels in order along the bar, each drawn only if it clears the one before.
    let mut last_end = f32::NEG_INFINITY;
    let mut ordered: Vec<(f32, String)> = labels.into_iter().map(|(t, s)| (along(t.clamp(0.0, 1.0)), s)).collect();
    ordered.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (d, label) in ordered {
        let lw = width(&label);
        let extent = if vertical { px } else { lw };
        if d - extent / 2.0 < last_end + gap {
            continue;
        }
        let (tick_ends, at, centred, fits) = if vertical {
            let (y, x0) = (by + d, if bar.side == TickSide::Start { bx } else { bx + bw });
            let x1 = if bar.side == TickSide::Start { x0 - tick } else { x0 + tick };
            let lx = if bar.side == TickSide::Start { x1 - gap - lw } else { x1 + gap };
            ([(x0, y), (x1, y)], (lx, y), false, inside(lx, y - px / 2.0, lw, px))
        } else {
            let (x, y0) = (bx + d, if bar.side == TickSide::Start { by } else { by + bh });
            let y1 = if bar.side == TickSide::Start { y0 - tick } else { y0 + tick };
            let ly = if bar.side == TickSide::Start { y1 - gap - px * 0.5 } else { y1 + gap + px * 0.5 };
            ([(x, y0), (x, y1)], (x, ly), true, inside(x - lw / 2.0, ly - px / 2.0, lw, px))
        };
        // Only a label drawn takes room: one left out doesn't thin the next.
        if fits {
            last_end = d + extent / 2.0;
            plan.labels.push((label, tick_ends, at, centred));
        } else {
            plan.spilled += 1;
        }
    }
    plan
}

/// A colorbar: the bar, ticks and labels along it, and its title above, as [`colorbar_plan`] lays
/// them out. Title lines and labels left out are said with the page's cut-off texts
/// ([`text_losses`]).
pub(super) fn colorbar(pm: &mut Canvas, r: Rect, scale: &ColorScale, bar: &Bar, ppp: f64) {
    let ppp = ppp as f32;
    let dark = bar.ink;
    let px = 8.0 * 1.33 * ppp;
    let plan = colorbar_plan(r, scale, bar, ppp);
    let tpx = COLORBAR_TITLE_PT as f32 * 1.33 * ppp;
    for (line, y) in &plan.title {
        text::text_at(pm, line, r.x(), *y, tpx, dark, true, text::Align::Left, false);
    }
    if plan.blocks.is_empty() {
        return;
    }
    let (bx, by, bw, bh) = plan.rect;
    let len = if plan.vertical { bh } else { bw };
    let along = |t: f32| if plan.flip { (1.0 - t) * len } else { t * len };
    let n = plan.blocks.len();
    for (i, c) in plan.blocks.iter().enumerate() {
        let (a, b) = (along(i as f32 / n as f32), along((i + 1) as f32 / n as f32));
        let (s0, s1) = (a.min(b), a.max(b));
        // A touch longer than its share, so neighbours leave no seam.
        let rect =
            if plan.vertical { Rect::from_xywh(bx, by + s0, bw, s1 - s0 + 0.6) } else { Rect::from_xywh(bx + s0, by, s1 - s0 + 0.6, bh) };
        if let Some(rect) = rect.and_then(|q| q.intersect(&Rect::from_xywh(bx, by, bw, bh)?)) {
            pm.fill_rect(rect, &paint(*c, 1.0), Transform::identity(), None);
        }
    }
    let line = Stroke { width: 0.6 * ppp, ..Stroke::default() };
    if let Some(b) = Rect::from_xywh(bx, by, bw, bh) {
        pm.stroke_path(&PathBuilder::from_rect(b), &paint(dark, 1.0), &line, Transform::identity(), None);
    }
    for (label, [(x0, y0), (x1, y1)], (x, y), centred) in &plan.labels {
        let align = if *centred { text::Align::Center } else { text::Align::Left };
        text::text_at(pm, label, *x, *y, px, dark, false, align, false);
        let mut pb = PathBuilder::new();
        pb.move_to(*x0, *y0);
        pb.line_to(*x1, *y1);
        if let Some(p) = pb.finish() {
            pm.stroke_path(&p, &paint(dark, 1.0), &line, Transform::identity(), None);
        }
    }
}

/// A colorbar's title's size in points; it is drawn bold.
pub const COLORBAR_TITLE_PT: f64 = 9.0;

/// A colorbar's title's style, as [`colorbar_title_lines`] measures it: bold, from the left edge.
pub(super) fn colorbar_title_style() -> TextStyle {
    TextStyle {
        face: text::Face { bold: true, ..Default::default() },
        align: TextAlign::Left,
        spacing: 0.0,
        valign: TextVAlign::Top,
        line_spacing: USUAL_LINE_SPACING,
    }
}

/// A colorbar's title's width in mm, its widest line, measured as [`colorbar_title_lines`] breaks
/// it.
pub fn colorbar_title_width_mm(title: &str) -> f64 {
    let px = (COLORBAR_TITLE_PT * 1.33 * WRAP_PX_PER_MM / PT_PER_MM) as f32;
    let face = colorbar_title_style().face;
    title.lines().map(|line| f64::from(text::styled_extent(line, px, face, 0.0).1)).fold(0.0, f64::max) / WRAP_PX_PER_MM
}

/// The slack a colorbar's title gets on its width, in mm: a box made exactly as wide as the title
/// still takes it on one line once its width is rounded, or laid out in whole pixels.
pub(super) const COLORBAR_TITLE_SLACK_MM: f64 = 0.01;

/// A colorbar's title as the lines it is drawn in (#2493): broken at spaces to fit `width` mm, a
/// word wider than that across lines, as a wrapped text is ([`wrapped`]), at one resolution
/// whatever it is drawn at, so the page, each export and a publication's sizing break it alike. A
/// title of one line that fits is that line, as it is. Past 200 lines, one more line with text
/// says it goes on, as for a wrapped text.
pub fn colorbar_title_lines(title: &str, width: f64) -> Vec<String> {
    let width = width + COLORBAR_TITLE_SLACK_MM;
    if !title.contains('\n') && colorbar_title_width_mm(title) <= width {
        return vec![title.to_string()];
    }
    wrapped(title, width, COLORBAR_TITLE_PT, colorbar_title_style()).lines().map(str::to_string).collect()
}

/// How many of a colorbar's `lines` of title it draws in an element from `top` (pixels), its
/// title at `px` pixels (#2493): each line only when the room it takes ends by `limit`, the
/// element's bottom less the bar's own room ([`colorbar_bar_room`]), the first too, so the bar
/// always keeps its labels (#2638); at most 200. The page's cut-off check ([`text_losses`])
/// counts the lines left out the same way, so a colorbar too short for its title says so.
pub(super) fn colorbar_title_drawn(lines: usize, top: f32, limit: f32, px: f32) -> usize {
    (0..lines.min(200)).take_while(|i| top + px * (1.4 + 1.25 * *i as f32) <= limit).count()
}

/// The least room a colorbar's bar takes under its title, in pixels at `ppp` (#2638), as
/// [`colorbar`] lays it out: a horizontal bar 3 pixels thick (at 96 dpi) with its ticks and
/// 8-point labels under it; a vertical one, whose labels are at its side, two labels' height, so
/// its end labels, centred on the bar's ends, stay inside the element. A title stops short of it,
/// so its lines never leave the bar without its labels.
pub(super) fn colorbar_bar_room(ppp: f32, orientation: Orientation) -> f32 {
    match orientation {
        Orientation::Horizontal => (8.0 * 1.33 * 1.2 + 2.5 + 1.5 + 3.0) * ppp,
        Orientation::Vertical => 2.0 * 8.0 * 1.33 * ppp,
    }
}
