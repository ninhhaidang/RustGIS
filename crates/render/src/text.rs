//! Text for exported images: label rasterisation (with halos) and the scale bar.

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, GlyphId, PxScale, ScaleFont, VariableFont};
use tiny_skia::{Paint, Pixmap, Rect, Transform};

use crate::fonts::CjkRegion;
use crate::{Canvas, PlacedLabel, View};
use rustgis_map::Rgba;
use rustgis_map::layout::TextAlign;

mod widths;

fn font(bold: bool) -> Option<&'static FontRef<'static>> {
    static R: OnceLock<Option<FontRef<'static>>> = OnceLock::new();
    static B: OnceLock<Option<FontRef<'static>>> = OnceLock::new();
    let f = crate::fonts::ui_fonts();
    if bold {
        B.get_or_init(|| {
            let mut face = FontRef::try_from_slice(&f.semibold).ok()?;
            // Heavier where the face is variable (macOS's system font, #591); static ones ignore it.
            face.set_variation(b"wght", crate::fonts::SEMIBOLD_WEIGHT);
            Some(face)
        })
        .as_ref()
    } else {
        R.get_or_init(|| FontRef::try_from_slice(&f.regular).ok()).as_ref()
    }
}

/// The family and style a line is drawn in: a layout text's serif (#517), with its bold and
/// italic, or the label font's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Face {
    pub bold: bool,
    pub italic: bool,
    /// The installed serif; the sans where there is none.
    pub serif: bool,
    /// An installed family, by its place in [`crate::families::installed`] (a label's named
    /// font); the serif or sans where it can't be drawn.
    pub family: Option<u16>,
}

/// The installed serif's faces, parsed once: (regular, bold, italic, bold italic).
type SerifFaces = (FontRef<'static>, Option<FontRef<'static>>, Option<FontRef<'static>>, Option<FontRef<'static>>);

/// The weight a serif's bold faces are drawn at where the file is a variable font (a variable
/// Noto Serif is one file for every weight); static faces ignore it.
const SERIF_BOLD_WEIGHT: f32 = 700.0;

fn serif() -> Option<&'static SerifFaces> {
    static S: OnceLock<Option<SerifFaces>> = OnceLock::new();
    S.get_or_init(|| {
        let f = crate::fonts::serif_fonts()?;
        let parse = |b: Option<&'static [u8]>| b.and_then(|b| FontRef::try_from_slice(b).ok());
        let heavy = |b: Option<&'static [u8]>| {
            let mut face = parse(b)?;
            face.set_variation(b"wght", SERIF_BOLD_WEIGHT);
            Some(face)
        };
        // A variable family's bold faces are its regular and italic bytes parsed again (a
        // `FontRef` only borrows them), drawn at the bold weight.
        let (bold, bold_italic) =
            if f.variable { (Some(f.regular.as_slice()), f.italic.as_deref()) } else { (f.bold.as_deref(), f.bold_italic.as_deref()) };
        Some((FontRef::try_from_slice(&f.regular).ok()?, heavy(bold), parse(f.italic.as_deref()), heavy(bold_italic)))
    })
    .as_ref()
}

/// The face `face` is drawn in from `serif`'s faces, and whether it is slanted by hand: a real
/// italic face where the family has one (bold italic, else italic), else the bold or regular
/// slanted, as the sans always is. Without a serif, the label font.
fn face_in(face: Face, serif: Option<&'static SerifFaces>) -> Option<(&'static FontRef<'static>, bool)> {
    let Some((regular, bold, italic, bold_italic)) = serif.filter(|_| face.serif) else {
        return font(face.bold).map(|f| (f, face.italic));
    };
    let upright = if face.bold { bold.as_ref().unwrap_or(regular) } else { regular };
    let real_italic = if face.bold { bold_italic.as_ref() } else { italic.as_ref() };
    Some(match real_italic.filter(|_| face.italic) {
        Some(f) => (f, false),
        None => (upright, face.italic),
    })
}

fn face_font(face: Face) -> Option<(&'static FontRef<'static>, bool)> {
    let named = face.family.and_then(|i| crate::families::installed().get(usize::from(i)));
    named.and_then(|fam| family_font(fam, face)).or_else(|| face_in(face, serif()))
}

/// The face of an installed family that draws `face`'s weight and slope, parsed once and kept,
/// and whether it is slanted by hand (the family has no italic face): see
/// [`crate::families::pick`]. `None` when its file can't be read.
fn family_font(fam: &crate::families::Family, face: Face) -> Option<(&'static FontRef<'static>, bool)> {
    type Parsed = std::collections::HashMap<(std::path::PathBuf, u32, bool), Option<&'static FontRef<'static>>>;
    static PARSED: OnceLock<std::sync::Mutex<Parsed>> = OnceLock::new();
    let (installed, slanted) = crate::families::pick(fam, face.bold, face.italic)?;
    let heavier = crate::families::heavier(installed, face.bold);
    let key = (installed.path.clone(), installed.index, heavier);
    let lock = PARSED.get_or_init(Default::default);
    if let Some(f) = lock.lock().ok()?.get(&key) {
        return f.map(|f| (f, slanted));
    }
    // Read and parsed with the lock let go: a file can be 48 MB, and other text is drawn meanwhile.
    let read = crate::families::bytes(installed).and_then(|b| FontRef::try_from_slice_and_index(b, installed.index).ok());
    let mut parsed = lock.lock().ok()?;
    // Another thread parsed it meanwhile: its face is the one kept.
    if let Some(f) = parsed.get(&key) {
        return f.map(|f| (f, slanted));
    }
    let f = read.map(|mut f| {
        if heavier {
            f.set_variation(b"wght", crate::families::BOLD_WEIGHT);
        }
        &*Box::leak(Box::new(f))
    });
    parsed.insert(key, f);
    f.map(|f| (f, slanted))
}

/// The label font's regular face.
pub(crate) fn label_font() -> Option<&'static FontRef<'static>> {
    font(false)
}

/// The regular face of the installed family `name` a character marker draws in, and whether that
/// is final: `None` and `false` while the families are still being indexed (see
/// [`crate::families::find_ready`]), `None` and `true` for a family this machine lacks (or none).
pub(crate) fn marker_font(name: &str) -> (Option<&'static FontRef<'static>>, bool) {
    if name.trim().is_empty() {
        return (None, true);
    }
    let Some(families) = crate::families::available() else { return (None, false) };
    let named = crate::families::find_ready(name).and_then(|i| families.get(usize::from(i)));
    (named.and_then(|fam| family_font(fam, Face::default())).map(|(f, _)| f), true)
}

/// The label font of `face`'s weight, for the characters `f` lacks: `None` when `f` is it.
fn sans_beside(f: &'static FontRef<'static>, face: Face) -> Option<&'static FontRef<'static>> {
    font(face.bold).filter(|s| !std::ptr::eq(*s, f))
}

/// Whether a glyph drawn from `g` leans: the whole line does when its face is slanted by hand
/// (`slanted`), and in a real italic face `f` a glyph taken from another face (the sans or a
/// CJK face, for a character the italic lacks) is slanted to match it.
fn leans(face: Face, f: &FontRef<'static>, slanted: bool, g: &FontRef<'static>) -> bool {
    slanted || (face.italic && !std::ptr::eq(f, g))
}

/// The scale that draws `f` with an em `px` pixels tall, as CSS sizes a font and the map view
/// (egui) draws it. (ab_glyph's own pixel scale is the height from descender to ascender,
/// a good third more than the em in most faces.)
fn em(f: &FontRef, px: f32) -> PxScale {
    let (h, upm) = (f.height_unscaled(), f.units_per_em().unwrap_or(0.0));
    let k = if h.is_finite() && upm.is_finite() && h > 0.0 && upm > 0.0 { h / upm } else { 1.0 };
    PxScale::from(px * k)
}

/// Characters that never need a glyph: spaces, controls, zero-width joiners, variation selectors,
/// soft hyphens.
fn invisible(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(c, '\u{AD}' | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}' | '\u{E0100}'..='\u{E01EF}')
}

/// An installed CJK face for `region`, parsed once; `None` when there is none (or on the web).
fn cjk(region: CjkRegion) -> Option<&'static FontRef<'static>> {
    static FACES: [OnceLock<Option<FontRef<'static>>>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let slot = match region {
        CjkRegion::SimplifiedChinese => &FACES[0],
        CjkRegion::Japanese => &FACES[1],
        CjkRegion::Korean => &FACES[2],
    };
    slot.get_or_init(|| crate::fonts::cjk_font(region).and_then(|(b, i)| FontRef::try_from_slice_and_index(b, *i).ok())).as_ref()
}

/// The CJK faces to try for `c`, the region whose glyph style suits it first: Hangul looks for a
/// Korean face, kana for a Japanese one, everything else (Han ideographs) for Simplified Chinese.
fn cjk_order(c: char) -> [CjkRegion; 3] {
    use CjkRegion::{Japanese as Jp, Korean as Kr, SimplifiedChinese as Sc};
    match c {
        '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}' | '\u{A960}'..='\u{A97F}' | '\u{AC00}'..='\u{D7FF}' => [Kr, Sc, Jp],
        '\u{3040}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}' | '\u{FF66}'..='\u{FF9F}' => [Jp, Sc, Kr],
        _ => [Sc, Jp, Kr],
    }
}

/// The face and glyph that draw `c`: `f` when it has the glyph, else `sans` (the label font a
/// serif text falls back to: an arrow or a script the serif lacks), else the first installed CJK
/// font that does. `None` when nothing covers it, so no missing-glyph box is drawn.
fn glyph(c: char, f: &'static FontRef<'static>, sans: Option<&'static FontRef<'static>>) -> Option<(&'static FontRef<'static>, GlyphId)> {
    let id = f.glyph_id(c);
    if id.0 != 0 || invisible(c) {
        return (id.0 != 0).then_some((f, id));
    }
    if let Some(s) = sans {
        let id = s.glyph_id(c);
        if id.0 != 0 {
            return Some((s, id));
        }
    }
    cjk_order(c).into_iter().filter_map(cjk).find_map(|f| {
        let id = f.glyph_id(c);
        (id.0 != 0).then_some((f, id))
    })
}

/// Whether the label fonts (`bold` or regular, with the installed CJK fallbacks) have a glyph for
/// every character of `text`, invisible characters aside.
pub fn covers(text: &str, bold: bool) -> bool {
    let Some(f) = font(bold) else { return true };
    text.chars().all(|c| invisible(c) || glyph(c, f, None).is_some())
}

/// Lays out `text` at `px` pixels in `face` (with `sans` for the characters it lacks, as
/// [`glyph`] takes them): each drawable glyph with its face, pen position (from 0) and character,
/// and the total advance. Characters no font covers are left out.
fn shape(
    text: &str,
    px: f32,
    face: &'static FontRef<'static>,
    sans: Option<&'static FontRef<'static>>,
) -> (Vec<(&'static FontRef<'static>, GlyphId, f32, char)>, f32) {
    let mut out = Vec::new();
    let mut x = 0.0;
    let mut prev: Option<(&'static FontRef<'static>, GlyphId)> = None;
    for ch in text.chars().take(500) {
        let Some((f, id)) = glyph(ch, face, sans) else { continue };
        let sf = f.as_scaled(em(f, px));
        if let Some((pf, pid)) = prev
            && std::ptr::eq(pf, f)
        {
            x += sf.kern(pid, id);
        }
        out.push((f, id, x, ch));
        x += sf.h_advance(id);
        prev = Some((f, id));
    }
    (out, x)
}

/// Most lines a label draws: its text comes from files and attributes.
const MAX_LINES: usize = 200;

/// The lines of a label, split as the map view splits them (#577): at each `\n`, a trailing one
/// making an empty last line. Lines past [`MAX_LINES`] are left out (measured and drawn alike).
pub fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n').take(MAX_LINES)
}

/// How many lines a label draws.
pub fn line_count(text: &str) -> usize {
    lines(text).count()
}

/// Text width in points at `size` (font px ≈ size × 1.33 for point sizes), drawn on one line.
/// Repeated measurements share a bounded retention cache; fonts are discovered once per process.
/// Long strings and non-finite sizes keep the uncached behavior, including its glyph limit.
pub fn measure(text: &str, size: f32, bold: bool) -> f32 {
    static WIDTHS: OnceLock<widths::Cache> = OnceLock::new();
    WIDTHS.get_or_init(widths::Cache::default).measure(text, size, bold, || measure_uncached(text, size, bold))
}

/// The original measurement, including em scaling, font fallbacks and the missing-font estimate.
/// Cache misses call this outside the cache lock; no glyphs or font faces are retained by the cache.
fn measure_uncached(text: &str, size: f32, bold: bool) -> f32 {
    let Some(f) = font(bold) else {
        return text.chars().count() as f32 * size * 0.6;
    };
    shape(text, size * 1.33, f, None).1
}

/// [`measure`] in `face`: a label in a named family is as wide as that family draws it.
pub fn measure_in(text: &str, size: f32, face: Face) -> f32 {
    if face.family.is_none() && !face.serif {
        return measure(text, size, face.bold);
    }
    // Labels in a named family are measured as often as those in the label font (every feature,
    // every frame), so they share its retention, keyed by the whole face.
    static WIDTHS: OnceLock<widths::Cache<(bool, bool, bool, Option<u16>)>> = OnceLock::new();
    let key = (face.bold, face.italic, face.serif, face.family);
    WIDTHS.get_or_init(widths::Cache::default).measure(text, size, key, || {
        let Some((f, _)) = face_font(face) else {
            return text.chars().count() as f32 * size * 0.6;
        };
        shape(text, size * 1.33, f, sans_beside(f, face)).1
    })
}

/// How wide `text` is on one line at `size` with `spacing` points between letters, in points.
fn spaced_width(text: &str, size: f32, bold: bool, spacing: f32) -> f32 {
    let w = measure(text, size, bold);
    if spacing == 0.0 {
        return w;
    }
    (w + spacing * 1.33 * text.chars().count().saturating_sub(1) as f32).max(0.0)
}

/// A line of a label broken into lines about `max_w` points wide, as MapLibre wraps a label to
/// `text-max-width`: at spaces, and after a hyphen, dash, slash or the like; the lines balanced
/// (each near the same width) rather than filled, and a word longer than that a line of its
/// own. A line that fits, or can't be broken, is kept as it is.
pub fn wrap(line: &str, size: f32, bold: bool, spacing: f32, max_w: f32) -> Vec<String> {
    // The pieces a line may break after: each with whether a space followed it.
    let mut pieces: Vec<(String, bool)> = Vec::new();
    let mut cur = String::new();
    for c in line.chars().take(2000) {
        if c == ' ' {
            if !cur.is_empty() {
                pieces.push((std::mem::take(&mut cur), true));
            } else if let Some(last) = pieces.last_mut() {
                // A space after a piece that ended at a dash or "&".
                last.1 = true;
            }
            continue;
        }
        cur.push(c);
        if matches!(c, '-' | '/' | '&' | '+' | ')' | '\u{AD}' | '\u{B7}' | '\u{200B}' | '\u{2010}' | '\u{2013}') {
            pieces.push((std::mem::take(&mut cur), false));
        }
    }
    if !cur.is_empty() {
        pieces.push((cur, true));
    }
    let width = |s: &str| spaced_width(s, size, bold, spacing);
    let total = width(line);
    if pieces.len() < 2 || !(max_w.is_finite() && max_w > 0.0) || total <= max_w {
        return vec![line.to_string()];
    }
    // As many lines as the width takes, each aiming at an equal share of it.
    let n = (total / max_w).ceil().max(1.0);
    let target = total / n;
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut spaced = false;
    for (piece, space_after) in pieces {
        if cur.is_empty() {
            cur = piece;
            spaced = space_after;
            continue;
        }
        let longer = if spaced { format!("{cur} {piece}") } else { format!("{cur}{piece}") };
        let (now, then) = (width(&cur), width(&longer));
        // The piece goes on this line if that brings it nearer the target (and no wider than
        // the most a line may be), else it starts the next.
        if then <= target || (then <= max_w.max(target) && (then - target).abs() <= (now - target).abs()) {
            cur = longer;
        } else {
            out.push(std::mem::take(&mut cur));
            cur = piece;
        }
        spaced = space_after;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The distance between a label's lines in points, as [`draw`] sets them: 1.25 × the font's
/// pixel size, as a layout's text.
pub fn line_pitch(size: f32) -> f32 {
    size * 1.33 * 1.25
}

/// A label's height in points, for its collision box: one line as labels have always taken, and
/// a line pitch more for each further line.
pub fn block_height(text: &str, size: f32) -> f32 {
    size * 1.25 + line_count(text).saturating_sub(1) as f32 * line_pitch(size)
}

/// Whether a label is slanted or letter-spaced (a text graphic's, #535), or in a named family,
/// drawn with [`styled_at`].
fn styled(l: &PlacedLabel) -> bool {
    l.italic || l.spacing != 0.0 || l.font.is_some()
}

/// The face a label is drawn in.
pub fn label_face(l: &PlacedLabel) -> Face {
    Face { bold: l.bold, italic: l.italic, serif: false, family: l.font }
}

/// How wide a line of `l` is laid out, in pixels at `ppp`: its advance, and its letter spacing
/// between letters (not past the last), as the map view lays it out (#535), so a text is placed
/// alike on screen and in an export. A slant's overhang isn't counted, as it isn't there either.
fn line_width(l: &PlacedLabel, line: &str, ppp: f32) -> f32 {
    spaced_line_width(line, l.size, label_face(l), l.spacing, ppp)
}

/// [`line_width`] of a line of `size` in `face`, `spacing` points between its letters.
fn spaced_line_width(line: &str, size: f32, face: Face, spacing: f32, ppp: f32) -> f32 {
    let advance = measure_in(line, size, face) * ppp;
    if spacing == 0.0 {
        return advance;
    }
    let letters = letters_of(line, face);
    // Letters drawn tighter than their own width (the most negative spacing) don't make it less
    // than nothing.
    (advance + spacing * 1.33 * ppp * letters.saturating_sub(1) as f32).max(0.0)
}

/// How many glyphs a line is drawn with in `face` (letter spacing goes between them): shaped once
/// for each line and face, as a size doesn't change it, and kept while the lines kept are few.
fn letters_of(line: &str, face: Face) -> usize {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static KEPT: OnceLock<Mutex<HashMap<(String, Face), usize>>> = OnceLock::new();
    let kept = KEPT.get_or_init(Mutex::default);
    let key = (line.to_string(), face);
    if let Some(n) = kept.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return *n;
    }
    let n = match face_font(face) {
        Some((f, _)) => shape(line, 12.0, f, sans_beside(f, face)).0.len(),
        None => line.chars().count(),
    };
    let mut kept = kept.lock().unwrap_or_else(|e| e.into_inner());
    if kept.len() >= 4096 {
        kept.clear();
    }
    kept.insert(key, n);
    n
}

/// A label's width in points: its widest line, as [`draw`] lays its lines out (#577), `spacing`
/// points between the letters. Text drawn on one line (table cells, chart names…) is measured
/// with [`measure`].
pub fn spaced_block_width(text: &str, size: f32, face: Face, spacing: f32) -> f32 {
    lines(text).map(|line| spaced_line_width(line, size, face, spacing, 1.0)).fold(0.0, f32::max)
}

/// A label's width in points: its widest line as [`draw`] lays it out, its style included.
pub fn label_width(l: &PlacedLabel) -> f32 {
    lines(&l.text).map(|line| line_width(l, line, 1.0)).fold(0.0, f32::max)
}

/// The rectangle a label's background fills (points: [left, top, right, bottom], upright about
/// the label's centre): its text's block, as tall as its lines are drawn, grown by the padding;
/// none without a background that draws.
pub fn background_box(l: &PlacedLabel) -> Option<[f32; 4]> {
    let b = l.background.as_ref().filter(|b| b.draws() && l.glyphs.is_empty() && !l.text.is_empty())?;
    let w = label_width(l);
    let h = l.size * 1.33 * 1.2 + line_count(&l.text).saturating_sub(1) as f32 * line_pitch(l.size);
    let (px, py) = (b.padding[0] * 1.33, b.padding[1] * 1.33);
    let cx = l.center_x();
    let r = [cx - w / 2.0 - px, l.y - h / 2.0 - py, cx + w / 2.0 + px, l.y + h / 2.0 + py];
    r.iter().all(|v| v.is_finite()).then_some(r)
}

/// How far a label's background, its outline's outer half included, reaches past the box its
/// text collides with (points: across, and up and down), none without one: other labels keep off
/// it as off a halo.
pub fn background_reach(l: &PlacedLabel) -> [f32; 2] {
    let (Some([x0, y0, _, y1]), Some(b)) = (background_box(l), l.background.as_ref()) else { return [0.0; 2] };
    let outline = if b.stroke.is_some_and(|s| s.a() > 0) { b.stroke_width * 1.33 / 2.0 } else { 0.0 };
    let across = l.center_x() - label_width(l) / 2.0 - x0;
    let up_down = (y1 - y0) / 2.0 - block_height(&l.text, l.size) / 2.0;
    [across, up_down].map(|r| (r + outline).max(0.0))
}

/// Draws a label's background (logical coordinates, upright) at `ppp` pixels a point.
fn draw_background(pm: &mut Canvas, l: &PlacedLabel, ppp: f32) {
    let (Some(r), Some(b)) = (background_box(l), l.background.as_ref()) else { return };
    let [x0, y0, x1, y1] = r.map(|v| v * ppp);
    let Some(rect) = Rect::from_ltrb(x0, y0, x1, y1) else { return };
    let path = crate::layout::rounded_rect(rect, b.radius * 1.33 * ppp).unwrap_or_else(|| tiny_skia::PathBuilder::from_rect(rect));
    if let Some(f) = b.fill.filter(|f| f.a() > 0) {
        pm.fill_path(&path, &crate::paint(f, 1.0), tiny_skia::FillRule::Winding, Transform::identity(), None);
    }
    if let Some(c) = b.stroke.filter(|c| c.a() > 0 && b.stroke_width > 0.0) {
        let stroke = tiny_skia::Stroke { width: b.stroke_width * 1.33 * ppp, ..tiny_skia::Stroke::default() };
        pm.stroke_path(&path, &crate::paint(c, 1.0), &stroke, Transform::identity(), None);
    }
}

/// The selection frame [`draw`] puts round a label, in points: [left, top, right, bottom].
fn frame_box(l: &PlacedLabel) -> [f32; 4] {
    let (w, pad) = (label_width(l), 3.0);
    let h = l.size * 1.33 * 1.2 + line_count(&l.text).saturating_sub(1) as f32 * line_pitch(l.size);
    let cx = l.center_x();
    [cx - w / 2.0 - pad, l.y - h / 2.0 - pad, cx + w / 2.0 + pad, l.y + h / 2.0 + pad]
}

fn blend(pm: &mut Pixmap, x: i32, y: i32, c: Rgba, cov: f32) {
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    if x < 0 || y < 0 || x >= w || y >= h || cov <= 0.0 {
        return;
    }
    let idx = (y * w + x) as usize;
    let data = pm.pixels_mut();
    let Some(px) = data.get_mut(idx) else { return };
    let a = (c.a() as f32 / 255.0) * cov.min(1.0);
    let inv = 1.0 - a;
    let r = (c.r() as f32 * a + px.red() as f32 * inv).round() as u8;
    let g = (c.g() as f32 * a + px.green() as f32 * inv).round() as u8;
    let b = (c.b() as f32 * a + px.blue() as f32 * inv).round() as u8;
    let al = ((a * 255.0) + px.alpha() as f32 * inv).round().min(255.0) as u8;
    if let Some(p) = tiny_skia::PremultipliedColorU8::from_rgba(r.min(al), g.min(al), b.min(al), al) {
        *px = p;
    }
}

/// Draws one line of `text` in the label font, centred on (`cx`, `cy`) and moved by `offset`.
fn draw_text(pm: &mut Canvas, text: &str, cx: f32, cy: f32, px_size: f32, color: Rgba, bold: bool, offset: (f32, f32)) {
    let Some(f) = font(bold) else { return };
    let sf = f.as_scaled(em(f, px_size));
    let (glyphs, width) = shape(text, px_size, f, None);
    let x0 = cx - width / 2.0 + offset.0;
    let base = cy + (sf.ascent() + sf.descent()) / 2.0 + offset.1;
    if pm.is_recording() {
        keep_glyphs(pm, &glyphs, (x0, base), px_size, 0.0, color, |_| false);
        return;
    }
    for (f, id, x, _) in glyphs {
        let g = id.with_scale_and_position(em(f, px_size), ab_glyph::point(x0 + x, base));
        if let Some(og) = f.outline_glyph(g) {
            let bb = og.px_bounds();
            og.draw(|gx, gy, cov| blend(pm.pixels(), bb.min.x as i32 + gx as i32, bb.min.y as i32 + gy as i32, color, cov));
        }
    }
}

/// The slant of synthetic italics (the fonts have no italic face): tan 12°, as type designers
/// slant an oblique.
const SLANT: f32 = 0.2126;

/// The ink `text` draws at `px` in `face` with `spacing`, from the pen's start: its leftmost
/// and rightmost pixel offsets. With a negative spacing the letters can run backwards past the
/// pen, and a slant moves their tops right and their descenders left; (0, 0) when nothing draws.
pub fn styled_extent(text: &str, px: f32, face: Face, spacing: f32) -> (f32, f32) {
    let Some((f, slanted)) = face_font(face) else { return (0.0, 0.0) };
    let sf = f.as_scaled(em(f, px));
    let base = (sf.ascent() + sf.descent()) / 2.0;
    let (glyphs, _) = shape(text, px, f, sans_beside(f, face));
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for (i, (gf, id, x, _)) in glyphs.into_iter().enumerate() {
        // Measured as `styled_at` draws it.
        let italic = leans(face, f, slanted, gf);
        let g = id.with_scale_and_position(em(gf, px), ab_glyph::point(x + spacing * i as f32, base));
        let Some(og) = gf.outline_glyph(g) else { continue };
        let bb = og.px_bounds();
        let (lean_top, lean_bottom) = if italic { (SLANT * (base - bb.min.y), SLANT * (base - bb.max.y)) } else { (0.0, 0.0) };
        lo = lo.min(bb.min.x + lean_top.min(lean_bottom));
        hi = hi.max(bb.max.x + lean_top.max(lean_bottom) + if italic { 1.0 } else { 0.0 });
    }
    if lo.is_finite() && hi.is_finite() { (lo, hi) } else { (0.0, 0.0) }
}

/// Draws `text` from `left` (pixels), its line centred on `cy`, at `px` in `face` with `spacing`
/// pixels between letters: italic in the family's italic face where it has one, else slanted.
/// Plain (sans, no spacing, upright) it draws as [`text_at`] does from the left.
#[allow(clippy::too_many_arguments)]
pub fn styled_at(pm: &mut Canvas, text: &str, left: f32, cy: f32, px: f32, color: Rgba, face: Face, spacing: f32) {
    if !(left.is_finite() && cy.is_finite() && px.is_finite() && spacing.is_finite()) || px <= 0.0 {
        return;
    }
    let Some((f, slanted)) = face_font(face) else { return };
    let sf = f.as_scaled(em(f, px));
    let base = cy + (sf.ascent() + sf.descent()) / 2.0;
    let (glyphs, _) = shape(text, px, f, sans_beside(f, face));
    if pm.is_recording() {
        keep_glyphs(pm, &glyphs, (left, base), px, spacing, color, |gf| leans(face, f, slanted, gf));
        return;
    }
    for (i, (gf, id, x, _)) in glyphs.into_iter().enumerate() {
        let italic = leans(face, f, slanted, gf);
        let g = id.with_scale_and_position(em(gf, px), ab_glyph::point(left + x + spacing * i as f32, base));
        let Some(og) = gf.outline_glyph(g) else { continue };
        let bb = og.px_bounds();
        if italic {
            slanted_glyph(pm, &og, base, color);
            continue;
        }
        og.draw(|gx, gy, cov| {
            // (Floored, so a row just above or left of the pixmap is clipped, not drawn on row 0.)
            blend(pm.pixels(), (bb.min.x + gx as f32).floor() as i32, (bb.min.y + gy as f32).floor() as i32, color, cov);
        });
    }
}

/// The most pixels of one slanted glyph's coverage gathered before it is drawn.
const MAX_SLANTED_PIXELS: usize = 1 << 22;

/// Draws `og` slanted: each row leans right by its height above the baseline `base`, its coverage
/// split across the two pixels it falls between so the slant stays smooth. The halves are added up
/// before a pixel is drawn once: blended one after the other, two halves cover only three quarters
/// of it, and rows that fall between pixels came out lighter, in stripes across large letters.
fn slanted_glyph(pm: &mut Canvas, og: &ab_glyph::OutlinedGlyph, base: f32, color: Rgba) {
    let bb = og.px_bounds();
    let shift = |px_x: f32, px_y: f32| px_x + SLANT * (base - px_y - 0.5);
    // Columns from the leftmost a row can lean to, one more on the right for the split.
    let left = shift(bb.min.x, bb.max.y).min(shift(bb.min.x, bb.min.y)).floor();
    let right = shift(bb.max.x, bb.min.y).max(shift(bb.max.x, bb.max.y)).ceil() + 1.0;
    let (w, h) = ((right - left).max(0.0) as usize + 1, bb.height().max(0.0).ceil() as usize + 1);
    let Some(mut acc) = w.checked_mul(h).filter(|n| *n <= MAX_SLANTED_PIXELS).map(|n| vec![0.0f32; n]) else { return };
    og.draw(|gx, gy, cov| {
        let s = shift(bb.min.x + gx as f32, bb.min.y + gy as f32);
        let (ix, frac) = (s.floor(), s - s.floor());
        let col = (ix - left) as usize;
        let at = gy as usize * w + col;
        if let Some(a) = acc.get_mut(at) {
            *a += cov * (1.0 - frac);
        }
        if col + 1 < w
            && let Some(a) = acc.get_mut(at + 1)
        {
            *a += cov * frac;
        }
    });
    for (i, cov) in acc.into_iter().enumerate() {
        if cov > 0.0 {
            let (col, row) = ((i % w) as f32, (i / w) as f32);
            blend(pm.pixels(), (left + col) as i32, (bb.min.y + row).floor() as i32, color, cov);
        }
    }
}

/// Draws a placed label (logical coordinates) with its halo. A label turned with the map (one
/// along a street) is drawn upright into a picture of its own, which is turned into place.
pub fn draw(pm: &mut Canvas, l: &PlacedLabel, ppp: f32) {
    // Curved along a line: each character turned into its place, every character's halo
    // first, so none covers the letter before it.
    if !l.glyphs.is_empty() {
        let inks: &[Ink] = if l.halo.is_some() { &[Ink::Halo, Ink::Text] } else { &[Ink::Text] };
        // (The label without its characters, copied for each: not the characters each time.)
        let base = PlacedLabel { text: String::new(), align: 0.0, angle: None, glyphs: Vec::new(), ..l.clone() };
        for &ink in inks {
            for g in &l.glyphs {
                let one = PlacedLabel { text: g.text.clone(), x: l.x + g.dx, y: l.y + g.dy, ..base.clone() };
                draw_turned(pm, &one, g.angle, ppp, ink);
            }
        }
        return;
    }
    match l.angle {
        Some(a) if a.is_finite() && a.abs() > 1e-3 => draw_turned(pm, l, a, ppp, Ink::Both),
        _ => draw_upright(pm, l, ppp, Ink::Both),
    }
}

/// What of a label to draw: its halo, its text, or both (the halo under the text).
#[derive(Clone, Copy, PartialEq)]
enum Ink {
    Halo,
    Text,
    Both,
}

/// Draws `l` turned by `angle` (radians, clockwise) about its centre.
fn draw_turned(pm: &mut Canvas, l: &PlacedLabel, angle: f32, ppp: f32, ink: Ink) {
    let (w, h) = (label_width(l), block_height(&l.text, l.size).max(line_pitch(l.size)));
    // Room for the halo, and for the glyphs reaching past the line.
    // (The halo is drawn at most MAX_HALO_PX wide, so no more room is kept for it.)
    let pad = 4.0
        + l.size * 0.5
        + drawn_halo_reach(l).min(MAX_HALO_PX / ppp.max(f32::EPSILON))
        + background_reach(l).into_iter().fold(0.0, f32::max);
    let (pw, ph) = (((w + 2.0 * pad) * ppp).ceil(), ((h + 2.0 * pad) * ppp).ceil());
    if !(pw.is_finite() && ph.is_finite()) || pw > 4096.0 || ph > 4096.0 {
        return;
    }
    let (cx, cy) = (pw / ppp / 2.0, ph / ppp / 2.0);
    let upright = PlacedLabel { x: cx + l.align * w, y: cy, angle: None, framed: false, icon: None, ..l.clone() };
    let (tx, ty) = (l.center_x() * ppp, l.y * ppp);
    let t = Transform::from_translate(-pw / 2.0, -ph / 2.0).post_rotate(angle.to_degrees()).post_translate(tx, ty);
    if pm.is_recording() {
        // Kept turned: the upright label's glyphs and halo with the turn as their transform.
        pm.with_transform(t, |c| draw_upright(c, &upright, ppp, ink));
        return;
    }
    let Some(mut pic) = Canvas::new(pw.max(1.0) as u32, ph.max(1.0) as u32) else { return };
    draw_upright(&mut pic, &upright, ppp, ink);
    let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..tiny_skia::PixmapPaint::default() };
    pm.draw_pixmap(0, 0, pic.pixmap().as_ref(), &paint, t, None);
}

/// Draws an upright label: its frame when selected, then its halo and text as `ink` says.
fn draw_upright(pm: &mut Canvas, l: &PlacedLabel, ppp: f32, ink: Ink) {
    let px = l.size * 1.33 * ppp;
    let (cx, cy) = (l.center_x() * ppp, l.y * ppp);
    // Several lines (#577) as the map view draws them: each aligned in a block as wide as the
    // widest (from its left unless a text graphic says otherwise, #535), the block centred on the
    // label's point. Each line is measured once: its width places it, the widest sizes the block.
    let widths: Vec<(&str, f32)> = lines(&l.text).map(|line| (line, line_width(l, line, ppp))).collect();
    let (n, line_h) = (widths.len(), line_pitch(l.size) * ppp);
    let w = widths.iter().map(|(_, lw)| *lw).fold(0.0, f32::max);
    if l.framed {
        let [x0, y0, x1, y1] = frame_box(l).map(|v| v * ppp);
        if let Some(r) = Rect::from_ltrb(x0, y0, x1, y1) {
            frame(pm, r, ppp);
        }
    }
    let top = cy - n.saturating_sub(1) as f32 * line_h / 2.0;
    // Each line: its text, its left edge (where the pen starts), its width, its centre y.
    let placed: Vec<(&str, f32, f32, f32)> = widths
        .iter()
        .enumerate()
        .map(|(i, (line, lw))| {
            let free = match l.line_align {
                TextAlign::Left => 0.0,
                TextAlign::Center => (w - lw) / 2.0,
                TextAlign::Right => w - lw,
            };
            (*line, cx - w / 2.0 + free, *lw, top + i as f32 * line_h)
        })
        .collect();
    let (styled, spacing) = (styled(l), l.spacing * 1.33 * ppp);
    let put = |pm: &mut Canvas, &(line, left, lw, y): &(&str, f32, f32, f32), color: Rgba, (dx, dy): (f32, f32)| {
        if styled {
            styled_at(pm, line, left + dx, y + dy, px, color, label_face(l), spacing);
        } else {
            draw_text(pm, line, left + lw / 2.0, y, px, color, l.bold, (dx, dy));
        }
    };
    if pm.is_recording() {
        // Kept as vectors: the background, then each line's halo as its letters' outlines spread
        // all round (filled, and stroked as wide again), then the text as glyphs.
        if ink != Ink::Text {
            draw_background(pm, l, ppp);
        }
        if let Some(h) = l.halo.filter(|_| ink != Ink::Text) {
            let r = match l.halo_width {
                Some(width) => (halo_reach(width) * 1.33 * ppp).min(MAX_HALO_PX),
                None => 1.2 * ppp,
            };
            // (No width, no halo, as the pixels have it; none built once the recording has
            // overflowed, as nothing more is kept.)
            for &(line, left, lw, y) in placed.iter().filter(|_| r > 0.0) {
                if pm.overflowed() {
                    break;
                }
                let outline = if styled {
                    styled_outline(line, left, y, px, label_face(l), spacing)
                } else {
                    plain_outline(line, left + lw / 2.0, y, px, l.bold)
                };
                keep_halo(pm, outline, r, h);
            }
        }
        if ink != Ink::Halo {
            for line in &placed {
                put(pm, line, l.color, (0.0, 0.0));
            }
        }
        return;
    }
    // Its background under everything.
    if ink != Ink::Text {
        draw_background(pm, l, ppp);
    }
    // Every line's halo first, so none covers the line above it.
    if let Some(h) = l.halo.filter(|_| ink != Ink::Text) {
        match l.halo_width {
            // As wide as its class sets it: the letters spread that far all round, as QGIS's
            // buffer, in one coat (a see-through halo isn't darker where letters are close; on a
            // curved label, each letter's is its own).
            Some(width) => {
                // At most MAX_HALO_PX: a halo grown far past any map's needs (a reference scale
                // at a high resolution) can't take the memory and time of a huge one.
                let r = (halo_reach(width) * 1.33 * ppp).min(MAX_HALO_PX);
                // Its reach, and room for a slant's overhang (the lines' rows already hold the
                // letters' height).
                let (pad, x0, x1) = (
                    r + 0.4 * px + 2.0,
                    placed.iter().map(|p| p.1).fold(f32::MAX, f32::min),
                    placed.iter().map(|p| p.1 + p.2).fold(f32::MIN, f32::max),
                );
                let (y0, y1) = (top - line_h, top + n as f32 * line_h);
                // Only what can show: the picture's part of it, and its halo's reach beyond.
                let (pw, ph) = (pm.width() as f32, pm.height() as f32);
                let (x0, y0) = ((x0 - pad).max(-pad).floor(), (y0 - pad).max(-pad).floor());
                let (x1, y1) = ((x1 + pad).min(pw + pad).ceil(), (y1 + pad).min(ph + pad).ceil());
                // (None at all for a halo no wider than nothing.)
                if let Some(mut mask) = (r > 0.0 && x1 > x0 && y1 > y0).then(|| Canvas::new((x1 - x0) as u32, (y1 - y0) as u32)).flatten() {
                    for line in &placed {
                        put(&mut mask, line, Rgba::rgb(0, 0, 0), (-x0, -y0));
                    }
                    let (w, cover) = (mask.width() as usize, spread(mask.pixmap(), r));
                    for (i, c) in cover.into_iter().enumerate() {
                        blend(pm.pixels(), x0 as i32 + (i % w) as i32, y0 as i32 + (i / w) as i32, h, c);
                    }
                }
            }
            None => {
                let r = 1.2 * ppp;
                let ring = [
                    (-r, 0.0),
                    (r, 0.0),
                    (0.0, -r),
                    (0.0, r),
                    (-r * 0.7, -r * 0.7),
                    (r * 0.7, r * 0.7),
                    (-r * 0.7, r * 0.7),
                    (r * 0.7, -r * 0.7),
                ];
                for line in &placed {
                    for offset in ring {
                        put(pm, line, h, offset);
                    }
                }
            }
        }
    }
    if ink == Ink::Halo {
        return;
    }
    for line in &placed {
        put(pm, line, l.color, (0.0, 0.0));
    }
}

/// The widest halo a label class sets (points); a reference scale grows it up to
/// [`rustgis_map::SIZE_FACTORS`] times.
const MAX_HALO: f32 = *rustgis_map::symbology::HALO_WIDTH.end() as f32;

/// How far past its letters a label's halo of its own width reaches (the label's units: points ×
/// 1.33, as its text's size), none when no halo is drawn (turned off, or fully see-through). Labels
/// are placed once for every pixel density they are drawn at, so this is the reach the class asks
/// for; only drawing holds a halo to [`MAX_HALO_PX`] pixels.
pub fn drawn_halo_reach(l: &PlacedLabel) -> f32 {
    l.halo.filter(|h| h.a() > 0).and(l.halo_width).map_or(0.0, |w| halo_reach(w) * 1.33)
}

/// How far a label's halo of `width` reaches (points): at most [`MAX_HALO`] grown by the most a
/// reference scale grows a label ([`rustgis_map::SIZE_FACTORS`]), so one grown with its label isn't
/// cut back (its class's own width is already within [`MAX_HALO`]); none for a width of no number.
pub fn halo_reach(width: f32) -> f32 {
    let most = MAX_HALO * rustgis_map::SIZE_FACTORS.1 as f32;
    if width.is_finite() { width.clamp(0.0, most) } else { 0.0 }
}

/// The widest halo drawn on a picture (pixels; about 64 points at 300 dpi): with the mask held to
/// the picture and this reach round it, a halo's memory and time stay near the picture's own.
const MAX_HALO_PX: f32 = 256.0;

/// How much of each of `mask`'s pixels a halo `r` pixels wide covers: its letters' coverage spread
/// `r` pixels all round, the edge smooth (a disc, as a round-joined stroke reaches). The distance
/// from each pixel to the nearest one the letters mostly cover is exact (a squared Euclidean
/// distance transform, Felzenszwalb and Huttenlocher), so the work grows with the mask's area,
/// not with the halo's width.
fn spread(mask: &Pixmap, r: f32) -> Vec<f32> {
    const FAR: f64 = 1e20;
    let (w, h) = (mask.width() as usize, mask.height() as usize);
    let cover: Vec<f32> = mask.pixels().iter().map(|p| f32::from(p.alpha()) / 255.0).collect();
    let mut d: Vec<f64> = cover.iter().map(|&c| if c >= 0.5 { 0.0 } else { FAR }).collect();
    let mut edt = Edt::default();
    // Down each column, then along each row.
    let mut col = vec![FAR; h];
    for x in 0..w {
        for (y, c) in col.iter_mut().enumerate() {
            *c = d.get(y * w + x).copied().unwrap_or(FAR);
        }
        for (y, v) in edt.run(&col).iter().enumerate() {
            if let Some(slot) = d.get_mut(y * w + x) {
                *slot = *v;
            }
        }
    }
    for y in 0..h {
        if let Some(row) = d.get_mut(y * w..(y + 1) * w) {
            let done = edt.run(row);
            row.copy_from_slice(done);
        }
    }
    cover.iter().zip(&d).map(|(&c, &d2)| c.max((r + 0.5 - d2.sqrt() as f32).clamp(0.0, 1.0))).collect()
}

/// The one-dimensional squared distance transform (the lower envelope of parabolas), its buffers
/// kept between rows.
#[derive(Default)]
struct Edt {
    v: Vec<usize>,
    z: Vec<f64>,
    out: Vec<f64>,
}

impl Edt {
    /// For each `q`, the least `(q - p)² + f[p]` over `p`.
    fn run(&mut self, f: &[f64]) -> &[f64] {
        let n = f.len();
        self.v.clear();
        self.v.resize(n.max(1), 0);
        self.z.clear();
        self.z.resize(n + 1, f64::INFINITY);
        self.out.clear();
        self.out.resize(n, 0.0);
        if n == 0 {
            return &self.out;
        }
        let (v, z) = (&mut self.v, &mut self.z);
        let at = |p: usize| f[p] + (p * p) as f64;
        let mut k = 0;
        z[0] = f64::NEG_INFINITY;
        for q in 1..n {
            let mut s = (at(q) - at(v[k])) / (2.0 * (q - v[k]) as f64);
            // z[0] is -inf, so this stops at k = 0.
            while s <= z[k] {
                k -= 1;
                s = (at(q) - at(v[k])) / (2.0 * (q - v[k]) as f64);
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f64::INFINITY;
        }
        k = 0;
        for q in 0..n {
            while z[k + 1] < q as f64 {
                k += 1;
            }
            let p = v[k];
            let dq = q as f64 - p as f64;
            self.out[q] = dq * dq + f[p];
        }
        &self.out
    }
}

/// [`spread`] read directly, for tests: each pixel's distance to every pixel the letters mostly
/// cover.
#[cfg(test)]
fn spread_direct(mask: &Pixmap, r: f32) -> Vec<f32> {
    let (w, h) = (mask.width() as usize, mask.height() as usize);
    let cover: Vec<f32> = mask.pixels().iter().map(|p| f32::from(p.alpha()) / 255.0).collect();
    let seeds: Vec<(usize, usize)> = (0..w * h).filter(|&i| cover[i] >= 0.5).map(|i| (i % w, i / w)).collect();
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as f32, (i / w) as f32);
            let d = seeds.iter().map(|&(sx, sy)| (x - sx as f32).hypot(y - sy as f32)).fold(f32::INFINITY, f32::min);
            cover[i].max((r + 0.5 - d).clamp(0.0, 1.0))
        })
        .collect()
}

/// The selection's frame: a light fill and an outline in the selection colour.
fn frame(pm: &mut Canvas, r: Rect, ppp: f32) {
    let path = tiny_skia::PathBuilder::from_rect(r);
    let fill = crate::paint(crate::SELECTION.with_alpha(40), 1.0);
    pm.fill_path(&path, &fill, tiny_skia::FillRule::Winding, Transform::identity(), None);
    let stroke = tiny_skia::Stroke { width: 2.0 * ppp, ..tiny_skia::Stroke::default() };
    pm.stroke_path(&path, &crate::paint(crate::SELECTION, 1.0), &stroke, Transform::identity(), None);
}

/// A two-tone scale bar in the lower-left corner.
/// `ground_per_pt` is ground metres per logical pixel at the centre of view.
pub fn scale_bar(pm: &mut Canvas, view: &View, ground_per_pt: f64) {
    let ppp = view.pixels_per_point as f32;
    let target = rustgis_geom::nice_distance(ground_per_pt * 120.0);
    if target <= 0.0 {
        return;
    }
    let len = (target / ground_per_pt) as f32;
    let (x0, y0) = (16.0 * ppp, pm.height() as f32 - 22.0 * ppp);
    let mut bg = Paint::default();
    bg.set_color_rgba8(255, 255, 255, 210);
    if let Some(r) = Rect::from_xywh(x0 - 6.0 * ppp, y0 - 18.0 * ppp, len * ppp + 12.0 * ppp, 30.0 * ppp) {
        pm.fill_rect(r, &bg, Transform::identity(), None);
    }
    for i in 0..4 {
        let mut p = Paint::default();
        if i % 2 == 0 {
            p.set_color_rgba8(40, 40, 40, 255);
        } else {
            p.set_color_rgba8(250, 250, 250, 255);
        }
        if let Some(r) = Rect::from_xywh(x0 + len * ppp * i as f32 / 4.0, y0, len * ppp / 4.0, 5.0 * ppp) {
            pm.fill_rect(r, &p, Transform::identity(), None);
        }
    }
    let label = PlacedLabel {
        text: rustgis_geom::format_distance(target),
        x: (x0 / ppp) + len / 2.0,
        y: y0 / ppp - 8.0,
        size: 8.0,
        color: Rgba::rgb(30, 30, 30),
        halo: None,
        halo_width: None,
        bold: false,
        icon: None,
        align: 0.0,
        framed: false,
        italic: false,
        spacing: 0.0,
        font: None,
        line_align: TextAlign::Left,
        angle: None,
        collide: Default::default(),
        glyphs: Vec::new(),
        background: None,
    };
    draw(pm, &label, ppp);
}

/// Horizontal alignment for [`text_at`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
}

/// Draws one line of text at pixel position (`x`, `y`) (`y` is the line's middle), `px_size`
/// pixels tall, optionally with a white halo.
pub fn text_at(pm: &mut Canvas, text: &str, x: f32, y: f32, px_size: f32, color: Rgba, bold: bool, align: Align, halo: bool) {
    if !(x.is_finite() && y.is_finite() && px_size.is_finite()) || px_size <= 0.0 {
        return;
    }
    let cx = match align {
        Align::Center => x,
        Align::Left => x + measure(text, px_size / 1.33, bold) / 2.0,
    };
    if halo && pm.is_recording() {
        keep_halo(pm, plain_outline(text, cx, y, px_size, bold), (px_size * 0.09).max(1.0), Rgba::new(255, 255, 255, 230));
    } else if halo {
        let r = (px_size * 0.09).max(1.0);
        for (dx, dy) in [(-r, 0.0), (r, 0.0), (0.0, -r), (0.0, r)] {
            draw_text(pm, text, cx, y, px_size, Rgba::new(255, 255, 255, 230), bold, (dx, dy));
        }
    }
    draw_text(pm, text, cx, y, px_size, color, bold, (0.0, 0.0));
}

/// A glyph [`shape`] lays out: its face, glyph, pen position and character.
type Shaped = (&'static FontRef<'static>, GlyphId, f32, char);

/// Keeps `glyphs` on a recording canvas as runs of glyphs, from `origin` (the pen's start on the
/// baseline) with `spacing` more between letters: a run for each stretch in one face and lean,
/// `lean` saying which faces' glyphs lean (a slant where the family has no italic).
fn keep_glyphs(
    pm: &mut Canvas,
    glyphs: &[Shaped],
    origin: (f32, f32),
    px: f32,
    spacing: f32,
    color: Rgba,
    lean: impl Fn(&FontRef<'static>) -> bool,
) {
    let placed: Vec<(usize, &Shaped)> = glyphs.iter().enumerate().collect();
    for run in placed.chunk_by(|a, b| std::ptr::eq(a.1.0, b.1.0)) {
        let Some(&(_, &(font, ..))) = run.first() else { continue };
        let kx = if lean(font) { -SLANT } else { 0.0 };
        pm.glyphs(crate::vector::GlyphRun {
            font,
            size: px,
            color: tiny_skia::Color::from_rgba8(color.r(), color.g(), color.b(), color.a()),
            transform: Transform::from_row(1.0, 0.0, kx, 1.0, origin.0, origin.1),
            glyphs: run.iter().map(|&(i, &(_, id, x, ch))| crate::vector::Glyph { id, x: x + spacing * i as f32, ch }).collect(),
        });
    }
}

/// The outline of one line of [`draw_text`]'s, centred on (`cx`, `cy`), as one path.
fn plain_outline(text: &str, cx: f32, cy: f32, px: f32, bold: bool) -> Option<tiny_skia::Path> {
    let f = font(bold)?;
    let sf = f.as_scaled(em(f, px));
    let (glyphs, width) = shape(text, px, f, None);
    let base = cy + (sf.ascent() + sf.descent()) / 2.0;
    outline_of(&glyphs, (cx - width / 2.0, base), px, 0.0, |_| false)
}

/// The outline of one line of [`styled_at`]'s, from `left`, as one path.
fn styled_outline(text: &str, left: f32, cy: f32, px: f32, face: Face, spacing: f32) -> Option<tiny_skia::Path> {
    let (f, slanted) = face_font(face)?;
    let sf = f.as_scaled(em(f, px));
    let base = cy + (sf.ascent() + sf.descent()) / 2.0;
    let (glyphs, _) = shape(text, px, f, sans_beside(f, face));
    outline_of(&glyphs, (left, base), px, spacing, |gf| leans(face, f, slanted, gf))
}

/// `glyphs`' outlines placed as [`keep_glyphs`] places them, in pixels, as one path.
fn outline_of(
    glyphs: &[Shaped],
    (x0, base): (f32, f32),
    px: f32,
    spacing: f32,
    lean: impl Fn(&FontRef<'static>) -> bool,
) -> Option<tiny_skia::Path> {
    let mut pb = tiny_skia::PathBuilder::new();
    for (i, &(f, id, x, _)) in glyphs.iter().enumerate() {
        // A face with no usable units per em is read at the usual 1000, as its glyphs are in the
        // PDF's fonts.
        let upm = f.units_per_em().filter(|u| u.is_finite() && *u > 0.0).unwrap_or(1000.0);
        let Some(o) = f.outline(id) else { continue };
        let (k, slant, pen) = (px / upm, if lean(f) { SLANT } else { 0.0 }, x0 + x + spacing * i as f32);
        // Font units (y up, from the pen on the baseline) to pixels.
        let at = |p: ab_glyph::Point| (pen + (p.x + slant * p.y) * k, base - p.y * k);
        let mut last: Option<ab_glyph::Point> = None;
        for c in &o.curves {
            let (start, end) = match c {
                ab_glyph::OutlineCurve::Line(a, b) | ab_glyph::OutlineCurve::Quad(a, _, b) | ab_glyph::OutlineCurve::Cubic(a, _, _, b) => {
                    (*a, *b)
                }
            };
            if last != Some(start) {
                if last.is_some() {
                    pb.close();
                }
                let (sx, sy) = at(start);
                pb.move_to(sx, sy);
            }
            match c {
                ab_glyph::OutlineCurve::Line(_, b) => {
                    let (bx, by) = at(*b);
                    pb.line_to(bx, by);
                }
                ab_glyph::OutlineCurve::Quad(_, k1, b) => {
                    let ((kx, ky), (bx, by)) = (at(*k1), at(*b));
                    pb.quad_to(kx, ky, bx, by);
                }
                ab_glyph::OutlineCurve::Cubic(_, k1, k2, b) => {
                    let ((ax, ay), (cx, cy), (bx, by)) = (at(*k1), at(*k2), at(*b));
                    pb.cubic_to(ax, ay, cx, cy, bx, by);
                }
            }
            last = Some(end);
        }
        if last.is_some() {
            pb.close();
        }
    }
    pb.finish()
}

/// Keeps a halo `r` pixels wide round `outline` in `color`: the letters filled and their outlines
/// stroked twice `r` wide, rounded, both opaque in a group at the colour's alpha, so a see-through
/// halo is one coat where they overlap.
fn keep_halo(pm: &mut Canvas, outline: Option<tiny_skia::Path>, r: f32, color: Rgba) {
    let Some(path) = outline else { return };
    let stroke = tiny_skia::Stroke {
        width: 2.0 * r,
        line_cap: tiny_skia::LineCap::Round,
        line_join: tiny_skia::LineJoin::Round,
        ..tiny_skia::Stroke::default()
    };
    let solid = crate::paint(color.with_alpha(255), 1.0);
    pm.group(f32::from(color.a()) / 255.0, |c| {
        c.fill_path(&path, &solid, tiny_skia::FillRule::Winding, Transform::identity(), None);
        c.stroke_path(&path, &solid, &stroke, Transform::identity(), None);
    });
}

#[cfg(test)]
mod spread_tests {
    use super::*;

    /// A mask with letter-like strokes: solid and partly covered pixels, and empty rows and columns.
    fn mask(w: u32, h: u32) -> Pixmap {
        let mut pm = Pixmap::new(w, h).unwrap();
        for (x0, y0, x1, y1, a) in
            [(3, 2, 5, 18, 255u8), (5, 9, 14, 10, 255), (20, 4, 21, 5, 140), (24, 12, 27, 16, 255), (26, 2, 27, 3, 90)]
        {
            for y in y0..y1 {
                for x in x0..x1 {
                    if let Some(px) = pm.pixels_mut().get_mut((y * w + x) as usize) {
                        *px = tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, a).unwrap();
                    }
                }
            }
        }
        pm
    }

    /// A spaced line's letters are counted once for each line and face, whatever its size or
    /// pixel density, and its width is as when they were counted for each query.
    #[test]
    fn spaced_widths_count_the_letters_once() {
        let face = Face::default();
        let n = letters_of("Rhein", face);
        assert_eq!((n, letters_of("Rhein", face)), (n, n));
        assert!(n >= 5, "{n}");
        let (plain, spaced) = (spaced_line_width("Rhein", 12.0, face, 0.0, 2.0), spaced_line_width("Rhein", 12.0, face, 1.0, 2.0));
        assert!((spaced - plain - 1.33 * 2.0 * (n - 1) as f32).abs() < 1e-3, "{plain} {spaced}");
        assert_eq!(spaced_line_width("Rhein", 30.0, face, 1.0, 2.0) - spaced_line_width("Rhein", 30.0, face, 0.0, 2.0), spaced - plain);
    }

    /// The distance transform gives what reading every pixel's distance to every letter pixel
    /// gives, at halos thinner than a pixel and wider than the mask.
    #[test]
    fn the_spread_is_the_direct_reading() {
        let m = mask(32, 22);
        for r in [0.3, 0.6, 1.0, 2.3, 4.5, 7.0, 40.0] {
            let (fast, direct) = (spread(&m, r), spread_direct(&m, r));
            let worst = fast.iter().zip(&direct).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(worst < 1e-4, "r {r}: off by {worst}");
        }
        // An empty mask (no letters) spreads nothing, and a mask of one row or column works.
        assert!(spread(&Pixmap::new(9, 7).unwrap(), 3.0).iter().all(|&c| c == 0.0));
        let line = mask(32, 1);
        assert_eq!(spread(&line, 2.0).len(), 32);
    }

    /// A halo grown far past what a map needs is drawn at most MAX_HALO_PX wide: a pixel past that
    /// reach from the letter, though within the halo's own width, is left as it was.
    #[test]
    fn a_huge_halo_is_bounded() {
        let label = |halo: Option<Rgba>, halo_width: Option<f32>| crate::PlacedLabel {
            text: "I".into(),
            x: 200.0,
            y: 170.0,
            size: 100.0,
            color: Rgba::rgb(0, 0, 0),
            halo,
            halo_width,
            bold: false,
            icon: None,
            align: 0.0,
            framed: false,
            italic: false,
            spacing: 0.0,
            font: None,
            line_align: TextAlign::Left,
            angle: None,
            collide: Default::default(),
            glyphs: Vec::new(),
            background: None,
        };
        // At 3 pixels a point: the letter's row through its middle (170 points down).
        let (w, row) = (3000usize, 510usize);
        let draw_at = |l: &crate::PlacedLabel| {
            let mut pm = Pixmap::new(w as u32, 1500).unwrap();
            crate::canvas::with_canvas(&mut pm, |c| draw(c, l, 3.0));
            pm.pixels().get(row * w..(row + 1) * w).unwrap().iter().map(|p| p.alpha()).collect::<Vec<u8>>()
        };
        let bare = draw_at(&label(None, None));
        let right = bare.iter().rposition(|&a| a > 0).expect("the letter crosses its middle row");
        // A 100 pt halo would reach 100 × 1.33 × 3 ≈ 400 pixels; it reaches MAX_HALO_PX (256).
        let haloed = draw_at(&label(Some(Rgba::rgb(255, 0, 0)), Some(100.0)));
        assert!(haloed[right + 200] > 0, "within the cap: painted");
        assert_eq!(haloed[right + 300], 0, "past the cap, within the halo's own width: left bare");
    }
}

#[cfg(test)]
mod coverage_tests {
    /// Large letters slanted by hand are solid: every row of an "I"'s stem has a fully covered
    /// pixel (rows that fell between pixels used to come out lighter, in stripes).
    #[test]
    fn slanted_letters_have_no_stripes() {
        use super::{Face, styled_at};
        use rustgis_map::Rgba;
        let mut pm = tiny_skia::Pixmap::new(200, 200).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        let face = Face { italic: true, ..Face::default() };
        crate::canvas::with_canvas(&mut pm, |c| styled_at(c, "I", 60.0, 100.0, 120.0, Rgba::rgb(0, 0, 0), face, 0.0));
        let rows: Vec<u8> =
            (0..200).filter_map(|y| (0..200).map(|x| pm.pixel(x, y).unwrap().red()).min().filter(|darkest| *darkest < 128)).collect();
        assert!(rows.len() > 60, "a tall stem: {} rows", rows.len());
        // The stem's inner rows (not its top and foot, which antialias).
        let inner = &rows[3..rows.len() - 3];
        assert!(inner.iter().all(|d| *d < 8), "{inner:?}");
    }

    #[test]
    fn knows_which_characters_it_can_draw() {
        // Which scripts are covered depends on the fonts this computer has (a system font may
        // cover Arabic); Latin always is, and a private-use character never is.
        for bold in [false, true] {
            assert!(super::covers("Syria", bold));
            assert!(super::covers("Türkiye Ελλάδα Беларусь", bold));
            assert!(!super::covers("Syria \u{10FFFD}", bold));
            // Invisible characters don't need a glyph.
            assert!(super::covers("Syria\u{200B}\u{FE0F}\u{E0100}", bold));
        }
    }

    #[test]
    fn draws_nothing_for_characters_no_font_has() {
        // A private-use character has no glyph anywhere: it used to come out as a .notdef box.
        let mut pm = tiny_skia::Pixmap::new(80, 40).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| {
            super::text_at(c, "\u{10FFFD}\u{10FFFD}", 40.0, 20.0, 24.0, rustgis_map::Rgba::rgb(0, 0, 0), false, super::Align::Center, true)
        });
        assert!(pm.pixels().iter().all(|p| p.alpha() == 0), "a missing-glyph box was drawn");
        assert_eq!(super::measure("\u{10FFFD}", 10.0, false), 0.0);
    }

    #[test]
    fn falls_back_to_an_installed_cjk_font() {
        use crate::fonts::{CjkRegion, cjk_font};
        use ab_glyph::Font;
        // Only meaningful on a computer with a CJK font (Windows has Microsoft YaHei).
        if cjk_font(CjkRegion::SimplifiedChinese).is_none() {
            return;
        }
        for bold in [false, true] {
            assert!(super::covers("碑林区", bold));
            assert!(super::covers("Mixed 中英 label", bold));
        }
        // The fallback glyphs take up room, so CJK labels are measured and centred correctly.
        assert!(super::measure("Mixed 中英 label", 10.0, false) > super::measure("Mixed  label", 10.0, false) + 10.0);
        // And the fallback glyphs are drawn, not just measured.
        let Some(ch) = "碑林区".chars().find(|&c| {
            let (Some(label), Some(cjk)) = (super::font(false), super::cjk(CjkRegion::SimplifiedChinese)) else { return false };
            label.glyph_id(c).0 == 0 && cjk.glyph_id(c).0 != 0
        }) else {
            return;
        };
        let mut pm = tiny_skia::Pixmap::new(80, 40).unwrap();
        crate::canvas::with_canvas(&mut pm, |c| {
            super::text_at(c, &ch.to_string(), 40.0, 20.0, 24.0, rustgis_map::Rgba::rgb(0, 0, 0), false, super::Align::Center, false)
        });
        assert!(pm.pixels().iter().any(|p| p.alpha() > 0), "the fallback glyph was not drawn");
    }

    /// A text asking for a serif (#517) where none is installed is drawn in the label font, slanted
    /// as before for italic: nothing goes blank, and the result is the same on every machine.
    #[test]
    fn a_missing_serif_draws_in_the_sans() {
        use super::{Face, face_in, font};
        for bold in [false, true] {
            for italic in [false, true] {
                let (f, slant) = face_in(Face { bold, italic, serif: true, family: None }, None).unwrap();
                assert!(std::ptr::eq(f, font(bold).unwrap()) && slant == italic);
            }
        }
    }

    /// The serif's faces picked for each style, a character it lacks taken from the sans, and that
    /// glyph slanted in a real italic, with faces of the test's own (egui's bundled Hack as the
    /// serif's regular, Ubuntu Light as its italic): run on every machine, serif installed or not.
    #[test]
    fn serif_faces_are_picked_and_fall_back() {
        use super::{Face, SerifFaces, face_in, leans, shape};
        use ab_glyph::{Font, FontRef};
        let parse = |b: &'static [u8]| FontRef::try_from_slice(b).unwrap();
        let faces: &'static SerifFaces =
            Box::leak(Box::new((parse(epaint_default_fonts::HACK_REGULAR), None, Some(parse(epaint_default_fonts::UBUNTU_LIGHT)), None)));
        let (regular, italic) = (&faces.0, faces.2.as_ref().unwrap());
        let pick = |bold, it| face_in(Face { bold, italic: it, serif: true, family: None }, Some(faces)).unwrap();
        let is = |(f, slant): (&FontRef, bool), g: &FontRef, s: bool| std::ptr::eq(f, g) && slant == s;
        assert!(is(pick(false, false), regular, false));
        assert!(is(pick(false, true), italic, false), "the real italic, not slanted");
        // No bold file: the regular; no bold italic: the regular, slanted.
        assert!(is(pick(true, false), regular, false));
        assert!(is(pick(true, true), regular, true));
        // A sans text never takes them.
        assert!(!std::ptr::eq(face_in(Face::default(), Some(faces)).unwrap().0, regular));
        // A character the serif lacks and the sans has: drawn from the sans.
        let sans = italic;
        let c = ('\u{100}'..='\u{2FFF}').find(|&c| regular.glyph_id(c).0 == 0 && sans.glyph_id(c).0 != 0).expect("a character Hack lacks");
        let (glyphs, _) = shape(&format!("a{c}"), 24.0, regular, Some(sans));
        assert!(glyphs.len() == 2 && std::ptr::eq(glyphs[1].0, sans), "{c:?} left out");
        // In a real italic, a glyph from another face leans; the italic's own don't.
        let it = Face { italic: true, serif: true, ..Face::default() };
        assert!(!leans(it, italic, false, italic) && leans(it, italic, false, regular));
        assert!(!leans(Face { serif: true, ..Face::default() }, regular, false, sans), "upright text stays upright");
    }

    /// A label in an installed family is drawn in its face, parsed once; a family that isn't
    /// installed is drawn in the label font.
    #[test]
    fn a_label_is_drawn_in_its_installed_family() {
        use super::{Face, family_font, measure, measure_in, shape};
        use crate::families::{Family, InstalledFace};
        // Hack, a monospaced face, as an installed family: every letter as wide.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Hack-Regular.ttf");
        std::fs::write(&path, epaint_default_fonts::HACK_REGULAR).unwrap();
        let fam = Family {
            name: "Hack".into(),
            listed: true,
            faces: vec![InstalledFace { path, index: 0, weight: 400, italic: false }],
            serif: false,
        };
        let (f, slanted) = family_font(&fam, Face::default()).unwrap();
        assert_eq!(shape("iiii", 20.0, f, None).1, shape("MMMM", 20.0, f, None).1);
        assert!(std::ptr::eq(f, family_font(&fam, Face::default()).unwrap().0), "parsed once");
        assert!(!slanted && family_font(&fam, Face { italic: true, ..Face::default() }).unwrap().1, "no italic face: slanted");
        // A family that isn't installed measures and draws as the label font.
        let missing = Face { family: Some(u16::MAX), ..Face::default() };
        assert_eq!(measure_in("Winterthur", 12.0, missing), measure("Winterthur", 12.0, false));
    }

    /// With a serif installed (Georgia on Windows, Times on macOS, Noto or DejaVu Serif on Linux),
    /// a text asking for it is drawn in it, and italic in the family's italic face, not slanted
    /// by hand; the sans is untouched.
    #[test]
    fn an_installed_serif_is_drawn() {
        use super::{Face, face_font, font};
        let Some(serif) = super::serif() else { return };
        let (f, slant) = face_font(Face { serif: true, ..Face::default() }).unwrap();
        assert!(std::ptr::eq(f, &serif.0) && !slant);
        let (sans, _) = face_font(Face::default()).unwrap();
        assert!(std::ptr::eq(sans, font(false).unwrap()));
        if let Some(italic) = &serif.2 {
            let (it, slant) = face_font(Face { italic: true, serif: true, ..Face::default() }).unwrap();
            assert!(std::ptr::eq(it, italic) && !slant);
        }
        // The same words in the serif and the sans draw differently.
        let draw = |face: Face| {
            let mut pm = tiny_skia::Pixmap::new(300, 40).unwrap();
            crate::canvas::with_canvas(&mut pm, |c| {
                super::styled_at(c, "Rio Amazonas", 2.0, 20.0, 24.0, rustgis_map::Rgba::rgb(0, 0, 0), face, 0.0)
            });
            pm.data().to_vec()
        };
        assert_ne!(draw(Face { serif: true, ..Face::default() }), draw(Face::default()));
        // A character the serif lacks and the label font has is drawn from the label font, not
        // left out.
        use ab_glyph::Font;
        let sans = font(false).unwrap();
        let lacking = ('\u{2190}'..='\u{2BFF}').find(|&c| serif.0.glyph_id(c).0 == 0 && sans.glyph_id(c).0 != 0);
        if let Some(c) = lacking {
            let (glyphs, _) = super::shape(&c.to_string(), 24.0, &serif.0, super::sans_beside(&serif.0, Face::default()));
            assert!(glyphs.len() == 1 && std::ptr::eq(glyphs[0].0, sans), "{c:?} left out");
        }
    }
}
