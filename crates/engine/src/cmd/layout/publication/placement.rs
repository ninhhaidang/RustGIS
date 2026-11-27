//! Where a publication layout's surrounds go over or beside the map.

/// The surrounds a publication layout places in the map frame's corners.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Surround {
    Locator,
    Arrow,
    Legend,
    Scale,
}

/// The least a publication layout's legend is cut to, each way, by the surrounds beside and above
/// or below it, in mm.
pub(in crate::cmd::layout) const MIN_LEGEND_MM: f64 = 10.0;

/// A publication layout's legend, `w` × `h` in its corner, cut short of the surround beside it
/// (`beside`: what it is and its width) and the one above or below it (`over`: what it is and its
/// height), in a frame with `across` and `down` mm of room (its margins and the gap between
/// surrounds taken off). Beside the arrow or the scale bar the legend keeps [`MIN_LEGEND_MM`] each
/// way, as a layout without a locator always has; the locator leaving it less, `None`: left out,
/// rather than drawn over the locator.
pub(crate) fn legend_room(
    w: f64,
    h: f64,
    across: f64,
    down: f64,
    beside: Option<(Surround, f64)>,
    over: Option<(Surround, f64)>,
) -> Option<(f64, f64)> {
    let room = |other: Option<(Surround, f64)>, total: f64| other.map_or(f64::INFINITY, |(_, size)| total - size);
    let (room_w, room_h) = (room(beside, across), room(over, down));
    let locator_in_the_way =
        |room: f64, other: Option<(Surround, f64)>| room < MIN_LEGEND_MM && other.is_some_and(|(kind, _)| kind == Surround::Locator);
    if locator_in_the_way(room_w, beside) || locator_in_the_way(room_h, over) {
        return None;
    }
    Some((w.min(room_w.max(MIN_LEGEND_MM)), h.min(room_h.max(MIN_LEGEND_MM))))
}

/// Whether a publication legend laid out in more columns beats it in fewer, each as the share of it
/// shown and the area (mm²) the surrounds leave over the drawing: it shows more of itself, or as
/// much and covers at least a quarter less, as a low wide box does above a map's data (#1379).
pub(crate) fn more_columns_win((shown, over): (f64, f64), (best_shown, best_over): (f64, f64)) -> bool {
    shown > best_shown + 1e-6 || (shown > best_shown - 1e-6 && over < 0.75 * best_over)
}

/// The width and height, in cells, of the largest rectangle with no drawn cell in
/// `(c0, r0, c1, r1)` of a grid `gw` cells wide, its cells row by row: where a legend's shape can
/// go (#1379).
pub(crate) fn largest_free_cells(gw: usize, cells: &[bool], (c0, r0, c1, r1): (usize, usize, usize, usize)) -> (usize, usize) {
    let mut heights = vec![0usize; c1.saturating_sub(c0) + 1];
    let (mut best, mut stack) = ((0, 0), Vec::<usize>::new());
    for r in r0..r1 {
        for c in c0..c1 {
            let free = !cells.get(r * gw + c).copied().unwrap_or(true);
            heights[c - c0] = if free { heights[c - c0] + 1 } else { 0 };
        }
        // Each column's height from this row up: the widest run at least that high, by a stack of
        // rising heights. The last entry stays 0, so every run ends.
        stack.clear();
        for i in 0..heights.len() {
            while let Some(&top) = stack.last().filter(|&&top| heights[top] >= heights[i]) {
                stack.pop();
                let wide = stack.last().map_or(i, |&left| i - left - 1);
                if wide * heights[top] > best.0 * best.1 {
                    best = (wide, heights[top]);
                }
            }
            stack.push(i);
        }
    }
    best
}

/// The part of a coverage grid of a page `wide` (mm) that a frame `size` (mm) with its top left at
/// `at` (mm, from the wide page's top left) covers, to the nearest cell, as a grid of its own; a
/// frame reaching past the wide page is held inside it.
pub(crate) fn crop_cells(
    (gw, gh, cells): &(u32, u32, Vec<bool>),
    wide: (f64, f64),
    at: (f64, f64),
    size: (f64, f64),
) -> (u32, u32, Vec<bool>) {
    let (gw, gh) = ((*gw as usize).max(1), (*gh as usize).max(1));
    let (cell_w, cell_h) = (wide.0 / gw as f64, wide.1 / gh as f64);
    let (w, h) = (((size.0 / cell_w).round() as usize).clamp(1, gw), ((size.1 / cell_h).round() as usize).clamp(1, gh));
    let (c0, r0) = (((at.0 / cell_w).round().max(0.0) as usize).min(gw - w), ((at.1 / cell_h).round().max(0.0) as usize).min(gh - h));
    let out = (r0..r0 + h).flat_map(|r| (c0..c0 + w).map(move |c| cells.get(r * gw + c).copied().unwrap_or(false))).collect();
    (w as u32, h as u32, out)
}

/// The surrounds of a veiled map grouped in a band of its veil: the locator with the north arrow at
/// its right and the scale bar under them, beside the legend or over it, whichever the band's shape
/// suits, centred in it. Gives the legend's, locator's, arrow's and scale bar's places (the locator's
/// and the arrow's when there is one); `None` when they don't fit either way.
pub(crate) fn veil_panel(
    band: Rect,
    legend: (f64, f64),
    locator: Option<f64>,
    arrow: Option<(f64, f64)>,
    scale: (f64, f64),
    gap: f64,
) -> Option<(Rect, Option<Rect>, Option<Rect>, Rect)> {
    let (bx, by, bw, bh) = band;
    let (aw, ah) = arrow.unwrap_or((0.0, 0.0));
    // The locator and the arrow side by side, with a gap between them only when both are there.
    let between = if locator.is_some() && arrow.is_some() { gap } else { 0.0 };
    let top = (locator.map_or(0.0, |side| side + between) + aw, locator.unwrap_or(0.0).max(ah));
    // The gap over the scale bar only when something sits above it.
    let lead = if top.1 > 0.0 { gap } else { 0.0 };
    let group = (top.0.max(scale.0), top.1 + lead + scale.1);
    let beside = legend.0 + gap + group.0 <= bw && legend.1.max(group.1) <= bh;
    let over = legend.0.max(group.0) <= bw && legend.1 + gap + group.1 <= bh;
    if !beside && !over {
        return None;
    }
    let (legend_at, group_at) = if beside && (bw >= bh || !over) {
        let (x, y) = (bx + (bw - legend.0 - gap - group.0) / 2.0, by + (bh - legend.1.max(group.1)) / 2.0);
        ((x, y), (x + legend.0 + gap, y))
    } else {
        let (x, y) = (bx + (bw - legend.0.max(group.0)) / 2.0, by + (bh - legend.1 - gap - group.1) / 2.0);
        ((x, y + group.1 + gap), (x, y))
    };
    Some((
        (legend_at.0, legend_at.1, legend.0, legend.1),
        locator.map(|side| (group_at.0, group_at.1, side, side)),
        arrow.map(|(aw, ah)| (group_at.0 + group.0 - aw, group_at.1, aw, ah)),
        (group_at.0, group_at.1 + top.1 + lead, scale.0, scale.1),
    ))
}

/// A publication layout's colorbar's height, in mm, with a title of one line: its title, the bar and
/// its labels.
pub(in crate::cmd::layout) const COLORBAR_MM: f64 = 15.0;

/// The height a publication gives its colorbar in a legend's box `ih` mm high inside, in mm,
/// when its title needs `need` (#2493). Over the map it leaves the rows the room they were sized
/// to (`rows`, with the gap above the colorbar), so a title past the box's cap is cut short
/// rather than taking them; but never below a colorbar's own height while the box has it, so
/// the bar isn't lost to rows that fill the box: those are cut and said instead (#2638). In a
/// side column it takes what it needs of the box.
pub(crate) fn colorbar_fit(need: f64, ih: f64, rows: f64, over_map: bool) -> f64 {
    let ih = ih.max(0.0);
    if over_map { need.min((ih - rows).max(COLORBAR_MM.min(ih))) } else { need.min(ih) }
}

/// A publication layout's colorbar's height, in mm, `width` mm wide: [`COLORBAR_MM`] and a line's
/// pitch more for each line its title wraps to past the first (#2493).
pub(in crate::cmd::layout) fn colorbar_mm(title: &str, width: f64) -> f64 {
    let lines = crate::render::layout::colorbar_title_lines(title, width).len();
    COLORBAR_MM + lines.saturating_sub(1) as f64 * crate::render::layout::line_pitch_mm(crate::render::layout::COLORBAR_TITLE_PT)
}

/// The share of what the map draws a surround may cover before the layout warns about it, when a
/// corner freer by at least as much went to another surround.
pub(in crate::cmd::layout) const OVER_DATA: f64 = 0.25;

/// The share of what the map draws a surround may cover in the corner it was given before it
/// leaves the corner for the freest place in the frame ([`free_spot`], #1379).
pub(in crate::cmd::layout) const LEAK: f64 = 0.005;

/// The step of the search for a place off the corners, in mm.
pub(in crate::cmd::layout) const STEP_MM: f64 = 1.0;

/// The room kept between surrounds placed off the corners, in mm.
pub(in crate::cmd::layout) const CLEAR_MM: f64 = 2.0;

/// A rectangle (x, y, w, h) in page millimetres.
pub(in crate::cmd::layout) type Rect = (f64, f64, f64, f64);

/// Where a surround that covers what the map draws in its corner goes instead (#1379): the place
/// in `area` (x0, y0, x1, y1: the frame inside the graticule labels' margins) at least
/// [`CLEAR_MM`] from each of `others`, on a [`STEP_MM`] step and on the lines through the area's
/// edges and centre and the others' edges, costing least: what it covers of the drawing (a share,
/// from `covered`) times 1000, plus its distance from `home` times 0.05, plus its distance from the
/// nearest of those lines, up to 4 mm each way, times 0.25. So it goes where the map is free, near
/// its corner, and in line with the frame or another surround (the scale bar under the locator's
/// left edge, the legend on the scale bar's baseline). The first of the cheapest, scanning row by
/// row, so a frame always gives the same place; `None` when nothing fits clear of the others.
pub(crate) fn free_spot(home: Rect, area: (f64, f64, f64, f64), others: &[Rect], covered: impl Fn(Rect) -> f64) -> Option<Rect> {
    let (hx, hy, w, h) = home;
    let (x0, y0, x1, y1) = area;
    let (xmax, ymax) = (x1 - w, y1 - h);
    if xmax < x0 || ymax < y0 {
        return None;
    }
    let mut gx = vec![x0, x1, (x0 + x1) / 2.0];
    let mut gy = vec![y0, y1, (y0 + y1) / 2.0];
    for o in others {
        gx.extend([o.0, o.0 + o.2]);
        gy.extend([o.1, o.1 + o.3]);
    }
    // Every step, then where an edge or the centre falls on a line.
    let along = |lo: f64, hi: f64, size: f64, lines: &[f64]| -> Vec<f64> {
        let steps = ((hi - lo) / STEP_MM).floor() as usize;
        let mut v: Vec<f64> = (0..=steps).map(|i| lo + i as f64 * STEP_MM).collect();
        v.extend(lines.iter().flat_map(|g| [*g, g - size, g - size / 2.0]).filter(|p| (lo..=hi).contains(p)));
        v
    };
    let off = |p: f64, size: f64, lines: &[f64]| {
        lines.iter().map(|g| (p - g).abs().min((p + size - g).abs()).min((p + size / 2.0 - g).abs())).fold(4.0_f64, f64::min)
    };
    let clear = |r: Rect| {
        others.iter().all(|o| {
            r.0 >= o.0 + o.2 + CLEAR_MM || o.0 >= r.0 + r.2 + CLEAR_MM || r.1 >= o.1 + o.3 + CLEAR_MM || o.1 >= r.1 + r.3 + CLEAR_MM
        })
    };
    // Each place along an axis with how far it is off the nearest line, worked out once per axis.
    let xs: Vec<(f64, f64)> = along(x0, xmax, w, &gx).into_iter().map(|x| (x, off(x, w, &gx))).collect();
    let ys: Vec<(f64, f64)> = along(y0, ymax, h, &gy).into_iter().map(|y| (y, off(y, h, &gy))).collect();
    let mut best: Option<(f64, Rect)> = None;
    for &(y, off_y) in &ys {
        for &(x, off_x) in &xs {
            let r = (x, y, w, h);
            if !clear(r) {
                continue;
            }
            let cost = 1000.0 * covered(r) + 0.05 * (x - hx).hypot(y - hy) + 0.25 * (off_x + off_y);
            if best.is_none_or(|(c, _)| cost < c - 1e-9) {
                best = Some((cost, r));
            }
        }
    }
    best.map(|(_, r)| r)
}
