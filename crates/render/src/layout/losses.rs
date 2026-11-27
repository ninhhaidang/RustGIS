//! What a page's texts lose: cut lines, boxes off the page, and the notices that say so.

use super::*;

/// Visible text elements (and legend and colorbar titles) whose ink crosses the page boundary,
/// or whose lines the text renderer omits at its box's vertical limit or its 200-line limit.
/// Text is not clipped horizontally to its element: a line wider than its box alone is not a
/// loss. Dynamic text uses this page and its first frame, rotated text is checked after its
/// turn, and a wrapped text (a colorbar's title too) by the lines it is broken into.
pub fn clipped_text(layout: &Layout, maps: &[Map], dpi: f64, info: &PageInfo) -> Vec<(u32, String)> {
    text_losses(layout, maps, dpi, info).into_iter().map(|l| (l.id, l.name)).collect()
}

/// A text element (or a legend's title) [`clipped_text`] finds cut off, with the lines of it
/// left out of the page (#2346).
#[derive(Debug, Clone, PartialEq)]
pub struct TextLoss {
    pub id: u32,
    pub name: String,
    /// Its lines with ink that aren't drawn: below its box, or past the 200 a text draws. 0 when
    /// only its ink crosses the page's edge.
    pub left_out: usize,
    /// It has more lines left out than `left_out` counts: a wrapped text is broken only up to
    /// the line past the 200 drawn ([`wrapped`]).
    pub at_least: bool,
    /// Some of them are left out below its box, among the first 200: a taller box or a smaller
    /// text draws them. For a scale bar, its box is too small for its labels and bar
    /// ([`BarLayout::cramped`]): its bar drawn below it, or its end label past its side or over
    /// its first label. For a colorbar, also set, with no line counted, when its labels would
    /// spill out of its box ([`colorbar_plan`]).
    pub below_box: bool,
    /// Some of them come past the 200 lines a text draws, whatever its box: only splitting the
    /// text draws them.
    pub past_limit: bool,
    /// Some of its ink crosses the page's edge: a text's drawn lines, or a scale bar's labels.
    pub off_page: bool,
    /// A scale bar's unit label shortened with an ellipsis to fit its room, or left out with no
    /// room for even its first character; `None` for a text, or a unit label drawn whole.
    pub unit_label: Option<UnitLabelCut>,
}

/// How a scale bar's unit label is cut to fit its room ([`TextLoss::unit_label`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitLabelCut {
    /// Shortened with an ellipsis.
    Shortened,
    /// Left out: not even its first character fits.
    LeftOut,
}

/// [`clipped_text`], with how many of each text's lines are left out of its box ([`TextLoss`]):
/// what the layout view, an export and the commands say, so they never tell it apart.
pub fn text_losses(layout: &Layout, maps: &[Map], dpi: f64, info: &PageInfo) -> Vec<TextLoss> {
    losses(layout, maps, dpi, info, None)
}

/// [`text_losses`] for the element `id` alone, measuring no other: `None` when it isn't cut off.
pub fn text_loss(layout: &Layout, maps: &[Map], dpi: f64, info: &PageInfo, id: u32) -> Option<TextLoss> {
    losses(layout, maps, dpi, info, Some(id)).pop()
}

/// Whether a scale bar `e` drawn in `r` puts a label's ink past the page's `size` in pixels, and
/// whether its box is too small for its labels and bar ([`BarLayout::cramped`]), its labels placed
/// by [`bar_layout`] as the drawing places them: their line's middle at `label_y`, each centred on
/// its advance as `text_at` draws it, and its unit label where it is drawn; and whether that unit
/// label is shortened or left out to fit ([`BarLayout::unit_cut`]). `None` when it draws no bar.
pub(super) fn bar_labels_cut(
    layout: &Layout,
    maps: &[Map],
    e: &rustgis_map::layout::Element,
    r: Rect,
    ppp: f32,
    size: (f32, f32),
) -> Option<(bool, bool, Option<UnitLabelCut>)> {
    let ElementKind::ScaleBar { frame, .. } = &e.kind else { return None };
    let ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } = &layout.element(*frame)?.kind else { return None };
    let m = maps.get(map_index(maps, map)?)?;
    let ground = m.ground_res_at(&frame_camera(m, *center, *scale, *bearing, *pitch));
    let bar = bar_layout(r, ground, &BarOptions::of(&e.kind)?, f64::from(ppp), layout.decimal_comma)?;
    let (w, h) = size;
    // Its labels, centred on their divisions, and its unit label where it is drawn.
    let unit = bar.unit.as_ref().map(|u| (u.x, u.y, u.text.as_str(), matches!(u.align, text::Align::Center)));
    let drawn = bar.labels.iter().map(|(x, s)| (*x, bar.label_y, s.as_str(), true)).chain(unit);
    let off_page = drawn.into_iter().any(|(x, y, s, centred)| {
        let pen = if centred { x - text::measure(s, bar.px / 1.33, false) / 2.0 } else { x };
        text::styled_bounds(s, bar.px, text::Face::default(), 0.0)
            .is_some_and(|ink| pen + ink.left() < 0.0 || y + ink.top() < 0.0 || pen + ink.right() > w || y + ink.bottom() > h)
    });
    Some((off_page, bar.cramped, bar.unit_cut))
}

/// The texts cut off, of the whole page or of the element `only`. A text's lines are the ones the
/// page draws ([`drawn_text`]), placed as [`text_block`] places them: its block shifted down its
/// box by its alignment and stepped by its spaced pitch. A block taller than its box starts at its
/// top whatever its alignment, so the same trailing lines are left out and counted as for a text
/// at the top.
pub(super) fn losses(layout: &Layout, maps: &[Map], dpi: f64, info: &PageInfo, only: Option<u32>) -> Vec<TextLoss> {
    let (w, h, k) = page_geometry(layout, dpi);
    let ppp = (k / PT_PER_MM) as f32;
    let first = layout.first_frame().and_then(|id| layout.element(id)).and_then(|e| {
        let ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } = &e.kind else { return None };
        let m = maps.get(map_index(maps, map)?)?;
        Some((m, frame_camera(m, *center, *scale, *bearing, *pitch)))
    });
    layout
        .elements
        .iter()
        .filter(|e| e.visible && only.is_none_or(|id| id == e.id))
        .filter_map(|e| {
            // At least a pixel each way, as the page draws it.
            let r = Rect::from_xywh((e.x * k) as f32, (e.y * k) as f32, ((e.w * k) as f32).max(1.0), ((e.h * k) as f32).max(1.0))?;
            // A scale bar's labels where it draws them, at their size (#2581); a bar drawn without
            // ink draws none, as a text without ink.
            if let ElementKind::ScaleBar { color, .. } = &e.kind {
                if color.a() == 0 {
                    return None;
                }
                let (off_page, cramped, unit_label) = bar_labels_cut(layout, maps, e, r, ppp, (w as f32, h as f32))?;
                return (off_page || cramped || unit_label.is_some()).then(|| TextLoss {
                    id: e.id,
                    name: e.name.clone(),
                    left_out: 0,
                    at_least: false,
                    below_box: cramped,
                    past_limit: false,
                    off_page,
                    unit_label,
                });
            }
            // A colorbar's labels left out for want of height, said with its title (#2638).
            let mut labels_cut = false;
            // `fits`: how many of its lines the element draws, when it decides that itself.
            let (s, size, face, align, spacing, rotation, title, broken, fits, block) = match &e.kind {
                ElementKind::Text {
                    text: t,
                    size,
                    color,
                    bold,
                    italic,
                    align,
                    letter_spacing,
                    font,
                    font_name,
                    rotation,
                    wrap,
                    valign,
                    line_spacing,
                    case,
                    ..
                } if color.a() > 0 => {
                    let style =
                        text_style(*bold, *italic, *font, font_name.as_deref(), *align, *letter_spacing).with_block(*valign, *line_spacing);
                    // Its lines as they are drawn: in its case, a wrapped text's broken to its box.
                    let s = drawn_text(expand(t, layout, info, first), *case, *wrap, e.w, *size, style);
                    (
                        s,
                        *size as f32,
                        style.face,
                        *align,
                        *letter_spacing as f32,
                        *rotation as f32,
                        false,
                        *wrap,
                        None,
                        (style.valign, style.line_spacing),
                    )
                }
                // Its title at the size it is drawn at, its legend's text size (#2581).
                ElementKind::Legend { title, frame, text_size, .. } => {
                    let ElementKind::MapFrame { map, .. } = &layout.element(*frame)?.kind else { return None };
                    map_index(maps, map)?;
                    let size = LEGEND_TITLE_PT * legend_size(*text_size);
                    (
                        title.clone(),
                        size,
                        text::Face { bold: true, ..Default::default() },
                        TextAlign::Left,
                        0.0,
                        0.0,
                        true,
                        false,
                        None,
                        (TextVAlign::Top, USUAL_LINE_SPACING),
                    )
                }
                // A colorbar's title in the lines it is drawn in (#2493), when it draws one: its
                // frame's map has a raster with a colour scale (else it draws a placeholder).
                // Its labels, too, when they would spill out of the element (#2638), as the colorbar
                // lays them out ([`colorbar_plan`]).
                ElementKind::Colorbar { frame, layer, title, orientation, ticks, tick_side, reverse, .. } => {
                    let ElementKind::MapFrame { map, .. } = &layout.element(*frame)?.kind else { return None };
                    let m = maps.get(map_index(maps, map)?)?;
                    let scale = rustgis_map::layout::colorbar_layer(m, layer.as_deref())
                        .and_then(|l| l.raster.as_ref())
                        .and_then(rustgis_map::layout::color_scale)?;
                    let bar = Bar {
                        title,
                        orientation: *orientation,
                        ticks: *ticks,
                        side: *tick_side,
                        reverse: *reverse,
                        comma: layout.decimal_comma,
                        ink: INK,
                    };
                    let plan = colorbar_plan(r, &scale, &bar, ppp);
                    let lines = if title.is_empty() { Vec::new() } else { colorbar_title_lines(title, e.w) };
                    let fits = plan.title.len();
                    labels_cut = plan.spilled > 0;
                    if lines.is_empty() && !labels_cut {
                        return None;
                    }
                    let s = lines.join("\n");
                    (
                        s,
                        COLORBAR_TITLE_PT as f32,
                        colorbar_title_style().face,
                        TextAlign::Left,
                        0.0,
                        0.0,
                        false,
                        true,
                        Some(fits),
                        (TextVAlign::Top, USUAL_LINE_SPACING),
                    )
                }
                _ => return None,
            };
            let px = size.clamp(2.0, 400.0) * 1.33 * ppp;
            // A legend's title sits in the middle of its first row, which grows with its size.
            // A text's first line, where its block alignment puts it ([`block_shift`]).
            let mut cy = if title {
                r.y() + LEGEND_ROW * (size / LEGEND_TITLE_PT) * ppp / 2.0
            } else {
                let shift = text_shift(block.0, f64::from(r.height()), f64::from(px), &s, f64::from(block.1)) as f32;
                r.y() + shift + px * 0.6
            };
            let turn = Transform::from_rotate_at(rotation, r.x() + r.width() / 2.0, r.y() + r.height() / 2.0);
            // Legend titles are drawn on one line, unlike text elements' explicit line breaks.
            let lines = std::iter::once(s.as_str()).filter(|_| title).chain(s.lines().filter(|_| !title));
            let (mut cut, mut left_out, mut below_box, mut past_limit, mut off_page) = (false, 0, false, false, false);
            for (i, line) in lines.enumerate() {
                let drawn = match fits {
                    Some(n) => i < n,
                    None => i < 200 && (title || cy <= r.bottom() + px),
                };
                // A line not drawn is only looked at for a letter, as a wrapped text tells its
                // ink ([`crate::report::wrap_lines`]): a long text isn't shaped past its box.
                if !drawn {
                    if line.chars().any(|c| !c.is_whitespace()) {
                        (cut, left_out) = (true, left_out + 1);
                        if i < 200 {
                            below_box = true;
                        } else {
                            past_limit = true;
                        }
                    }
                    continue;
                }
                let ink = text::styled_bounds(line, px, face, spacing * 1.33 * ppp);
                off_page |= ink.is_some_and(|ink| {
                    let left = match align {
                        TextAlign::Left => r.x(),
                        TextAlign::Center => r.x() + (r.width() - ink.width()) / 2.0 - ink.left(),
                        TextAlign::Right => r.right() - ink.width() - ink.left(),
                    };
                    let mut corners = [
                        tiny_skia::Point::from_xy(left + ink.left(), cy + ink.top()),
                        tiny_skia::Point::from_xy(left + ink.right(), cy + ink.top()),
                        tiny_skia::Point::from_xy(left + ink.left(), cy + ink.bottom()),
                        tiny_skia::Point::from_xy(left + ink.right(), cy + ink.bottom()),
                    ];
                    turn.map_points(&mut corners);
                    corners.iter().any(|p| p.x < 0.0 || p.y < 0.0 || p.x > w as f32 || p.y > h as f32)
                });
                cy += px * 1.25 * block.1;
            }
            // A wrapped text ends at the line past the 200 drawn ([`wrapped`]): it may go on.
            let at_least = broken && s.lines().nth(200).is_some();
            below_box |= labels_cut;
            (cut || off_page || labels_cut).then(|| TextLoss {
                id: e.id,
                name: e.name.clone(),
                left_out,
                at_least,
                below_box,
                past_limit,
                off_page,
                unit_label: None,
            })
        })
        .collect()
}

/// One counted notice per kind, naming the affected elements by their stable address. Exports
/// can merge repeated element IDs across series pages before calling this, so a PDF's fallback
/// rendering and repeated pages never multiply the count. Its advice fits the texts of `layout`
/// named: breaking lines by hand or wrapping them for a text not wrapped (#2346), only the box or
/// the size when all are wrapped to their boxes, and line breaks without the wrap for a legend's
/// title, which has none.
pub fn clipped_text_notice(layout: &Layout, clipped: &[(u32, String)]) -> Option<String> {
    if clipped.is_empty() {
        return None;
    }
    let mut names = clipped.iter().take(10).map(|(id, name)| format!("{name} ({id})")).collect::<Vec<_>>().join(", ");
    if clipped.len() > 10 {
        names.push_str(&format!(" and {} more", clipped.len() - 10));
    }
    // Each text's wrap; `None` for a legend's or a colorbar's title, or an element gone since,
    // which keep the advice they always had.
    let wraps = clipped
        .iter()
        .map(|(id, _)| match layout.element(*id).map(|e| &e.kind) {
            Some(ElementKind::Text { wrap, .. }) => Some(*wrap),
            _ => None,
        })
        .collect::<Vec<_>>();
    // Scale bars alone: their labels are no text to break (#2581).
    let bars = clipped.iter().all(|(id, _)| matches!(layout.element(*id).map(|e| &e.kind), Some(ElementKind::ScaleBar { .. })));
    // Colorbars alone: their titles already wrap, and a tick label isn't fixed by a line break
    // (#2638).
    let colorbars = clipped.iter().all(|(id, _)| matches!(layout.element(*id).map(|e| &e.kind), Some(ElementKind::Colorbar { .. })));
    let advice = if bars && clipped.len() == 1 {
        "move it inside the page or make its box larger, or reduce its text size"
    } else if bars {
        "move them inside the page or make their boxes larger, or reduce their text size"
    } else if colorbars {
        "make the colorbar larger or its title shorter, so its title lines and tick labels fit"
    } else if wraps.contains(&Some(false)) {
        "resize or move the text, reduce its size, add line breaks, or turn on Wrap to the box"
    } else if wraps.iter().all(|w| *w == Some(true)) {
        "resize or move the text, or reduce its size"
    } else {
        "resize or move the text, reduce its size, or add line breaks"
    };
    Some(format!("Text is cut off in {} layout element(s): {names}; {advice}", clipped.len()))
}

/// What scale bars whose labels don't fit are told (#2581), from their [`TextLoss`]es: one notice
/// a kind, with the bars' count and names. Their boxes too small for their labels and bars (a bar
/// drawn below its box, or an end label past its side), a label off the page, and a unit label
/// shortened or left out to fit ([`UnitLabelCut`]), each said of every bar it is true of.
pub fn scale_bar_fit_notices(losses: &[TextLoss]) -> Vec<String> {
    let said = |small: bool| {
        let kind = |l: &&TextLoss| if small { l.below_box } else { l.off_page };
        let names: Vec<String> = losses.iter().filter(kind).map(|l| format!("\"{}\"", l.name)).collect();
        let (n, names) = (names.len(), names.join(", "));
        match (n, small) {
            (0, _) => None,
            (1, true) => Some(format!(
                "Scale bar {names}: its box is too small for its labels at their size, so they or its bar run past it; make it larger or reduce its text size"
            )),
            (1, false) => Some(format!("Scale bar {names}: a label runs off the page; move it inside the page or reduce its text size")),
            (_, true) => Some(format!(
                "{n} scale bars ({names}): their boxes are too small for their labels at their size, so they or their bars run past them; make them larger or reduce their text size"
            )),
            (_, false) => {
                Some(format!("{n} scale bars ({names}): a label runs off the page; move them inside the page or reduce their text size"))
            }
        }
    };
    // A unit label shortened to fit, or left out with no room for it.
    let unit_said = |cut: UnitLabelCut| {
        let names: Vec<String> = losses.iter().filter(|l| l.unit_label == Some(cut)).map(|l| format!("\"{}\"", l.name)).collect();
        let (n, names) = (names.len(), names.join(", "));
        match (n, cut) {
            (0, _) => None,
            (1, UnitLabelCut::Shortened) => Some(format!(
                "Scale bar {names}: its unit label is shortened to fit; make its box larger, reduce its text size or shorten the unit label"
            )),
            (1, UnitLabelCut::LeftOut) => Some(format!(
                "Scale bar {names}: its unit label is left out, as there is no room for it; make its box larger, reduce its text size or shorten the unit label"
            )),
            (_, UnitLabelCut::Shortened) => Some(format!(
                "{n} scale bars ({names}): their unit labels are shortened to fit; make their boxes larger, reduce their text size or shorten the unit labels"
            )),
            (_, UnitLabelCut::LeftOut) => Some(format!(
                "{n} scale bars ({names}): their unit labels are left out, as there is no room for them; make their boxes larger, reduce their text size or shorten the unit labels"
            )),
        }
    };
    [said(true), said(false), unit_said(UnitLabelCut::Shortened), unit_said(UnitLabelCut::LeftOut)].into_iter().flatten().collect()
}

/// What a text that loses lines is told (#2346), from its [`TextLoss`]: `None` when none of its
/// lines is left out. Lines below its box want a taller box or a smaller text; lines past the 200
/// a text draws, splitting the text, as no box draws them.
pub fn lines_left_out_notice(loss: &TextLoss) -> Option<String> {
    let n = loss.left_out;
    if n == 0 && !loss.at_least {
        return None;
    }
    let (count, one) = match (n, loss.at_least) {
        (1, false) => ("1 line".to_string(), true),
        (n, false) => (format!("{n} lines"), false),
        (n, true) => (format!("{} or more lines", n.max(1)), false),
    };
    let said = match (loss.below_box, loss.past_limit) {
        (_, false) => {
            let verbs = if one { "doesn't fit its box and isn't" } else { "don't fit its box and aren't" };
            format!("{verbs} drawn; make the box taller or the text smaller")
        }
        (false, true) => {
            format!("{} drawn: a text draws at most 200 lines; split it into more than one text", if one { "isn't" } else { "aren't" })
        }
        (true, true) => format!(
            "{} drawn: make the box taller or the text smaller, and split it, as a text draws at most 200 lines",
            if one { "isn't" } else { "aren't" }
        ),
    };
    Some(format!("{count} of the text \"{}\" ({}) {said}", loss.name, loss.id))
}

/// What a text whose ink crosses the page's edge is told (#2703), from its [`TextLoss`]: `None`
/// when none does. Its lines left out below its box are [`lines_left_out_notice`]'s: a text can
/// have both, and each is said.
pub fn off_page_notice(loss: &TextLoss) -> Option<String> {
    loss.off_page.then(|| {
        format!(
            "The text \"{}\" ({}) runs off the page, so part of it isn't drawn; move it inside the page, resize it or reduce its size",
            loss.name, loss.id
        )
    })
}
