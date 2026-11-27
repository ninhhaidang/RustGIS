//! Text in a box: style, wrapping, vertical alignment and drawing.

use super::*;

/// How a text element's lines are drawn, beyond their size and colour.
#[derive(Clone, Copy)]
pub(crate) struct TextStyle {
    /// Bold, italic and the family.
    pub(crate) face: text::Face,
    pub(crate) align: TextAlign,
    /// Letter spacing in points.
    pub(crate) spacing: f32,
    /// Where its block of lines sits down its box ([`block_shift`]).
    pub(crate) valign: TextVAlign,
    /// The distance between its lines, a multiple of the usual (0.5 to 3).
    pub(crate) line_spacing: f32,
}

impl TextStyle {
    /// This style with a text's block alignment and line spacing, brought into range here, so every
    /// reader of `line_spacing` may trust it: an unknown alignment is the top, and the spacing is
    /// as [`rustgis_map::layout::line_spacing_in_range`] reads it (not finite is the usual one, one
    /// outside 0.5..=3 the nearer limit). The usual spacing is exactly 1, so a plain text is placed
    /// as it always was.
    pub(crate) fn with_block(self, valign: TextVAlign, line_spacing: f64) -> TextStyle {
        TextStyle { valign: valign.kept(), line_spacing: rustgis_map::layout::line_spacing_in_range(line_spacing) as f32, ..self }
    }
}

/// A text element's style: its face in an installed family by name (#762), else its generic one.
/// The app doesn't wait for the font index while it is being built; a script or the command line
/// builds it now (`families::find_ready`). One for the page, its clipping check and the SVG, so
/// each measures and breaks a text alike.
pub(crate) fn text_style(
    bold: bool,
    italic: bool,
    font: rustgis_map::layout::FontFamily,
    font_name: Option<&str>,
    align: TextAlign,
    letter_spacing: f64,
) -> TextStyle {
    let family = font_name.and_then(crate::families::find_ready);
    let face = text::Face { bold, italic, serif: font == rustgis_map::layout::FontFamily::Serif, family };
    TextStyle { face, align, spacing: letter_spacing as f32, valign: TextVAlign::Top, line_spacing: USUAL_LINE_SPACING }
}

/// Pixels per millimetre a wrapped text's lines are measured at (300 dpi), whatever the
/// resolution it is drawn at, so the layout view, each export and the SVG break the same lines.
pub(super) const WRAP_PX_PER_MM: f64 = 300.0 / 25.4;

/// A wrapped text (#2346) as the lines it is drawn in, joined by line breaks: each of its own
/// lines broken at spaces to fit `width` mm at `size` points, a word wider than that broken
/// across lines ([`crate::report::wrap_lines`]). A line is measured as [`text_block`] places it:
/// a left-aligned one from the box's edge to its ink's right, an aligned one by its ink.
pub(crate) fn wrapped(s: &str, width: f64, size: f64, style: TextStyle) -> String {
    let ppp = (WRAP_PX_PER_MM / PT_PER_MM) as f32;
    let px = (size as f32).clamp(2.0, 400.0) * 1.33 * ppp;
    let spacing = style.spacing * 1.33 * ppp;
    let measure = |t: &str| {
        let (lo, hi) = text::styled_extent(t, px, style.face, spacing);
        f64::from(if style.align == TextAlign::Left { hi } else { hi - lo })
    };
    // Lines past the 200 a text draws aren't broken: one more, with text, tells the cut-off check
    // it goes on.
    crate::report::wrap_lines(s, width * WRAP_PX_PER_MM, measure, 200).join("\n")
}

/// A text element's lines as they are drawn, from its text with its tokens filled in ([`expand`]):
/// in its case, then, when it wraps, broken to its box `width_mm` wide ([`wrapped`]), so an upper
/// case text breaks at its own, wider, width. One for the page, its cut-off check and the SVG, so
/// they draw and count the same lines. Those that don't fit its box are left out from the bottom
/// whatever its block alignment: a block taller than its box starts at its top ([`block_shift`]).
pub(crate) fn drawn_text(expanded: String, case: TextCase, wrap: bool, width_mm: f64, size: f64, style: TextStyle) -> String {
    let s = case.kept().apply(expanded);
    if wrap { wrapped(&s, width_mm, size, style) } else { s }
}

/// The most lines a text draws.
pub(super) const MAX_TEXT_LINES: usize = 200;

/// [`block_shift`] for the text `s` as it is drawn ([`drawn_text`]), by its lines. A text of more
/// lines than a text draws is cut at them, so it starts at the top like any block that doesn't fit
/// its box, though the lines kept would: one rule for the page, its cut-off check, a turned text's
/// picture and the SVG.
pub(crate) fn text_shift(valign: TextVAlign, box_h: f64, px: f64, s: &str, line_spacing: f64) -> f64 {
    let lines = s.lines().take(MAX_TEXT_LINES + 1).count();
    if lines > MAX_TEXT_LINES { 0.0 } else { block_shift(valign, box_h, px, lines, line_spacing) }
}

/// How far down its box of `box_h` pixels a text's block of `lines` lines at `px` pixels, spaced
/// `line_spacing` times the usual, starts: 0 at the top; half the room it leaves in the middle;
/// all of it at the bottom, its last line's pitch ending on the box's bottom edge. A block taller
/// than its box (or with no line) starts at its top whatever its alignment, so its first lines are
/// the ones kept and none is drawn above the box.
pub(crate) fn block_shift(valign: TextVAlign, box_h: f64, px: f64, lines: usize, line_spacing: f64) -> f64 {
    if lines == 0 || valign.kept() == TextVAlign::Top {
        return 0.0;
    }
    let block = px * 1.25 * (1.0 + (lines - 1) as f64 * line_spacing);
    let free = box_h - block;
    // A measure not a number, or past any page, is no room.
    if !free.is_finite() || free <= 0.0 {
        return 0.0;
    }
    match valign.kept() {
        TextVAlign::Middle => free / 2.0,
        TextVAlign::Bottom => free,
        _ => 0.0,
    }
}

/// The lines a wrapped text element (#2346), upright and without letter spacing or a named family,
/// is drawn in: `s`, its text with the dynamic text filled in, in a box `width` mm wide. For a
/// caller that sizes the box around the text.
pub fn wrapped_lines(s: &str, width: f64, size: f64, bold: bool, font: rustgis_map::layout::FontFamily, align: TextAlign) -> Vec<String> {
    wrapped(s, width, size, text_style(bold, false, font, None, align, 0.0)).lines().map(str::to_string).collect()
}

/// The usual line spacing ([`rustgis_map::layout::LINE_SPACING`]) as the drawing holds it.
pub(super) const USUAL_LINE_SPACING: f32 = rustgis_map::layout::LINE_SPACING as f32;

/// The distance between two lines of text at `size` points, in mm, at the usual line spacing (1),
/// as [`text_block`] sets them there; a text's line spacing multiplies it.
pub fn line_pitch_mm(size: f64) -> f64 {
    size.clamp(2.0, 400.0) * 1.33 * 1.25 * 25.4 / 96.0
}

/// The part of a text element's box (pixels, from its top-left corner) [`text_block`] inks, a line's
/// height more each way: across, its lines as each is aligned (past the box where one overruns
/// it); down, its lines from where its block starts ([`block_shift`]), or the box and the line
/// [`text_block`] still draws past it. `None` when it has no line.
pub(super) fn text_area(r: Rect, s: &str, size: f32, style: TextStyle, ppp: f64) -> Option<Rect> {
    let px = size.clamp(2.0, 400.0) * 1.33 * ppp as f32;
    let spacing = style.spacing * 1.33 * ppp as f32;
    let w = r.width();
    let (mut x0, mut x1, mut lines) = (f32::INFINITY, f32::NEG_INFINITY, 0u16);
    for line in s.lines().take(200) {
        let (lo, hi) = text::styled_extent(line, px, style.face, spacing);
        // Where `text_block` puts the ink: from the edge (left), or by the ink (centre, right).
        let (a, b) = match style.align {
            TextAlign::Left => (lo, hi),
            TextAlign::Center => ((w - (hi - lo)) / 2.0, (w + (hi - lo)) / 2.0),
            TextAlign::Right => (w - (hi - lo), w),
        };
        (x0, x1, lines) = (x0.min(a), x1.max(b), lines + 1);
    }
    // Down to its last line, where its block alignment and line spacing put it.
    let shift = text_shift(style.valign, f64::from(r.height()), f64::from(px), s, f64::from(style.line_spacing)) as f32;
    let block = (1.0 + (f32::from(lines) - 1.0) * style.line_spacing) * px * 1.25;
    let down = (r.height() + px).min(shift + block);
    (lines > 0).then(|| Rect::from_ltrb(x0 - px, shift - px, x1 + px, down + px)).flatten()
}

/// Draws with `draw` (given the box to draw in and how many of the page's pixels one of its own
/// is) turned `degrees` clockwise about the centre of `r`: kept turned on a recording canvas (a
/// vector PDF), else drawn upright in a picture of `area` (pixels from the box's top-left corner:
/// what it inks), which is placed turned. A picture past the page's budget is drawn at a lower
/// resolution and scaled up, as a frame that large is. Its size is at least a pixel and at most
/// the budget, and `r` and `area` have a size, so the picture is always made.
///
/// A quarter turn at full resolution maps the picture's pixels one for one onto the page's: it is
/// placed on whole pixels and not filtered, the part of a pixel that moved it drawn into the
/// picture instead, so its text is sampled once, as upright text is, and keeps as sharp. Another
/// turn is sampled once more, by the turn.
pub(super) fn draw_turned_box(pm: &mut Canvas, r: Rect, degrees: f32, area: Rect, draw: impl FnOnce(&mut Canvas, Rect, f64)) {
    let (cx, cy) = (r.x() + r.width() / 2.0, r.y() + r.height() / 2.0);
    if pm.is_recording() {
        pm.with_transform(Transform::from_rotate_at(degrees, cx, cy), |c| draw(c, r, 1.0));
        return;
    }
    let (w, h) = (f64::from(area.width()), f64::from(area.height()));
    let k = (MAX_PIXELS / (w * h)).sqrt().min(MAX_SIDE / w.max(h)).min(1.0) as f32;
    let (pw, ph) = ((w as f32 * k).ceil().max(1.0), (h as f32 * k).ceil().max(1.0));
    let mut place =
        Transform::from_scale(1.0 / k, 1.0 / k).post_translate(r.x() + area.x(), r.y() + area.y()).post_rotate_at(degrees, cx, cy);
    let quarter = k == 1.0 && degrees.rem_euclid(90.0) == 0.0;
    let (mut dx, mut dy) = (0.0, 0.0);
    if quarter {
        // Its turn exactly (0 or ±1 each way), and its corner on a whole pixel: the picture's
        // content moves back by as much, turned back (the turn's inverse is its transpose).
        let [sx, ky, kx, sy] = [place.sx, place.ky, place.kx, place.sy].map(f32::round);
        let (ex, ey) = (place.tx.round() - place.tx, place.ty.round() - place.ty);
        (dx, dy) = (-(sx * ex + ky * ey), -(kx * ex + sy * ey));
        place = Transform::from_row(sx, ky, kx, sy, place.tx.round(), place.ty.round());
    }
    // The box in the picture: its corner `area`'s offset away from the picture's (and less than a
    // pixel more, within the room `area` leaves round the ink).
    let inner = Rect::from_xywh(-area.x() * k + dx, -area.y() * k + dy, r.width() * k, r.height() * k);
    let (Some(mut pic), Some(inner)) = (Canvas::new(pw as u32, ph as u32), inner) else { return };
    draw(&mut pic, inner, f64::from(k));
    let quality = if quarter { tiny_skia::FilterQuality::Nearest } else { tiny_skia::FilterQuality::Bilinear };
    let paint = PixmapPaint { quality, ..PixmapPaint::default() };
    pm.draw_pixmap(0, 0, pic.pixmap().as_ref(), &paint, place, None);
}

/// Text in an element: lines split at `\n`, the block of them at the top, middle or bottom of the
/// box ([`block_shift`]) and spaced by the style's line spacing, each placed by the style's
/// alignment. A block taller than its box starts at its top whatever its alignment, so its first
/// lines are drawn and the ones past the box left out ([`text_losses`] counts them). A centred or
/// right-aligned line wider than the element runs out past its left edge, as a left-aligned one
/// runs past its right.
pub(super) fn text_block(pm: &mut Canvas, r: Rect, s: &str, size: f32, color: Rgba, style: TextStyle, ppp: f64) {
    let px = size.clamp(2.0, 400.0) * 1.33 * ppp as f32;
    // Letter spacing is in points, as the size is.
    let spacing = style.spacing * 1.33 * ppp as f32;
    let shift = text_shift(style.valign, f64::from(r.height()), f64::from(px), s, f64::from(style.line_spacing)) as f32;
    let mut y = r.y() + shift + px * 0.6;
    for line in s.lines().take(200) {
        if y > r.y() + r.height() + px {
            break;
        }
        // Centred and right-aligned lines are placed by the ink they draw (italic overhang and a
        // negative spacing included), which an advance doesn't measure; left-aligned ones start
        // at the edge as plain text always has.
        let left = match style.align {
            TextAlign::Left => r.x(),
            aligned => {
                let (ink_left, ink_right) = text::styled_extent(line, px, style.face, spacing);
                let free = r.width() - (ink_right - ink_left);
                r.x() + if aligned == TextAlign::Center { free / 2.0 } else { free } - ink_left
            }
        };
        text::styled_at(pm, line, left, y, px, color, style.face, spacing);
        y += px * 1.25 * style.line_spacing;
    }
}
