//! Layout texts: their style, fonts, fit and figures.

use super::*;

/// For each layout, the map its `{map}` texts name: the one its first map frame shows, as the
/// project names it; None when that frame or its map is gone.
pub(crate) fn maps_layouts_name(project: &crate::project::Project) -> Vec<Option<String>> {
    let named = |l: &Layout| {
        let frame = l.first_frame().and_then(|id| l.element(id))?;
        let ElementKind::MapFrame { map, .. } = &frame.kind else { return None };
        crate::render::layout::map_index(&project.maps, map).and_then(|i| project.maps.get(i)).map(|m| m.name.clone())
    };
    project.layouts.iter().map(named).collect()
}

/// Says which `{map}` texts a change (`before` it, [`maps_layouts_name`], layouts in the same
/// order) left naming another map or none: `{map}` follows the layout's first map frame, which
/// is the lowest in the drawing order, so removing or reordering frames can change it. One
/// notice for each, with the count.
pub(crate) fn say_map_texts(s: &mut Session, before: &[Option<String>]) {
    let after = maps_layouts_name(&s.project);
    let (mut empty, mut moved) = (Vec::new(), Vec::new());
    // The layouts whose first frame shows another map, or none: their texts' layer statistics
    // (#2853) are read from it.
    let mut figures = Vec::new();
    for (i, (l, (was, now))) in s.project.layouts.iter().zip(before.iter().zip(&after)).enumerate() {
        let Some(was) = was else { continue };
        if now.as_ref().is_none_or(|now| !now.eq_ignore_ascii_case(was)) {
            figures.push(i);
        }
        let n = l.elements.iter().filter(|e| matches!(&e.kind, ElementKind::Text { text, .. } if text.contains("{map}"))).count();
        match now {
            _ if n == 0 => {}
            None => empty.push((l.name.as_str(), n)),
            Some(now) if !now.eq_ignore_ascii_case(was) => moved.push((l.name.as_str(), n, now.as_str())),
            Some(_) => {}
        }
    }
    let texts = |n: usize| if n == 1 { format!("{n} text with {{map}}") } else { format!("{n} texts with {{map}}") };
    let n: usize = empty.iter().map(|(_, n)| n).sum();
    let notice = match empty.as_slice() {
        [] => None,
        [(name, _)] => Some(format!("Layout '{name}': {}", texts(n))),
        more => Some(format!("{} layouts: {}", more.len(), texts(n))),
    };
    let mut said = Vec::new();
    if let Some(whose) = notice {
        let have = if n == 1 { "has" } else { "have" };
        said.push(format!("{whose} {have} no map frame showing a map to name, so {{map}} draws empty"));
    }
    let n: usize = moved.iter().map(|(_, n, _)| n).sum();
    let notice = match moved.as_slice() {
        [] => None,
        [(name, _, map)] => Some(format!(
            "Layout '{name}': its first map frame, the lowest in the drawing order, now shows '{map}', so {} {} it",
            texts(n),
            if n == 1 { "names" } else { "name" }
        )),
        more => Some(format!(
            "{} layouts: their first map frame, the lowest in the drawing order, now shows another map, so {} {} that one",
            more.len(),
            texts(n),
            if n == 1 { "names" } else { "name" }
        )),
    };
    said.extend(notice);
    for text in said {
        s.notify(text, false);
    }
    for i in figures {
        say_layout_text_statistics(s, i);
    }
}

/// A sheet's footer of two columns, its right one moved to start where a legend at the right of the
/// frame (`frame` its left edge and width, page mm) does, when that is between 45 % and 70 % of the
/// frame's width, so the two line up (#1379). Each column's heading and lines are wrapped again to
/// its new width (`sections`, as the footer wraps them); when they'd need a taller box, the
/// footer stays as it is.
pub(crate) fn align_footer(
    around: &mut [(String, Rect, ElementKind)],
    legend: Rect,
    (fx, fw): (f64, f64),
    sections: &dyn Fn(f64, f64, f64) -> Vec<(&'static str, Broken, Broken)>,
) {
    let at = (legend.0 - fx) / fw;
    if !(0.45..=0.70).contains(&at) || legend.0 + legend.2 < fx + 0.9 * fw {
        return;
    }
    let find = |name: &str| around.iter().position(|(n, ..)| n == name);
    let size = |i: Option<usize>| match i.and_then(|i| around.get(i)).map(|e| &e.2) {
        Some(ElementKind::Text { size, .. }) => Some(*size),
        _ => None,
    };
    let (Some(a), Some(b), Some(head_at)) = (find("Sources Box"), find("Reference Box"), find("Sources Heading")) else { return };
    let (Some(head_pt), Some(body_pt)) = (size(Some(head_at)), size(find("Sources")).or_else(|| size(find("Reference")))) else { return };
    let (Some(left), Some(right), Some(head)) = (around.get(a), around.get(b), around.get(head_at)) else { return };
    let (left, right, inner) = (left.1, right.1, head.1.0 - left.1.0);
    let gap = right.0 - (left.0 + left.2);
    let (lw, rw) = (legend.0 - gap - left.0, right.0 + right.2 - legend.0);
    // Side by side, and the move a real one.
    if (left.1 - right.1).abs() > 1e-6 || lw <= 2.0 * inner || rw <= 2.0 * inner || (legend.0 - right.0).abs() < 0.5 {
        return;
    }
    let pitch = crate::render::layout::line_pitch_mm;
    let column = |w: f64, name: &str| sections(w - 2.0 * inner, head_pt, body_pt).into_iter().find(|(n, ..)| *n == name);
    let (Some((_, l_head, l_lines)), Some((_, r_head, r_lines))) = (column(lw, "Sources"), column(rw, "Reference")) else { return };
    let head_h = l_head.lines.len().max(r_head.lines.len()) as f64 * pitch(head_pt);
    let rows = l_lines.lines.len().max(r_lines.lines.len());
    if 2.0 * inner + head_h + rows as f64 * pitch(body_pt) > left.3 + 1e-6 {
        return;
    }
    for (name, x, w, head, lines) in [("Sources", left.0, lw, l_head, l_lines), ("Reference", legend.0, rw, r_head, r_lines)] {
        let y = left.1;
        for (part, r, text) in [
            (format!("{name} Box"), (x, y, w, left.3), None),
            (format!("{name} Heading"), (x + inner, y + inner, w - 2.0 * inner, head.lines.len() as f64 * pitch(head_pt)), Some(head)),
            (name.to_string(), (x + inner, y + inner + head_h, w - 2.0 * inner, lines.lines.len() as f64 * pitch(body_pt)), Some(lines)),
        ] {
            if let Some(e) = around.iter_mut().find(|(n, ..)| *n == part) {
                e.1 = r;
                if let (Some(new), ElementKind::Text { text, wrap, .. }) = (text, &mut e.2) {
                    (*text, *wrap) = (new.text, new.wrap);
                }
            }
        }
    }
}

pub(in crate::cmd::layout) const ONLY_LAYOUT_TEXT_SIZE: &str = "only legends and scale bars take textSize";

/// Says how many lines of the text `id` of the layout `li` don't fit its box (#2346), as the
/// layout view and an export count them: on the series' current page, measured at 300 dpi as a
/// wrapped text is broken, and, a notice of its own, that its ink runs off the page (#2703). A
/// `lines` of false (a move or a turn, which change no line) says only the edge. For a scale bar
/// `id`, whether its box is too short for its labels or one runs off the page (#2581).
pub(in crate::cmd::layout) fn say_text_fit(s: &mut Session, li: usize, id: u32, lines: bool) {
    let bar = match s.project.layouts.get(li).and_then(|l| l.element(id)).map(|e| &e.kind) {
        Some(ElementKind::Text { .. }) => false,
        Some(ElementKind::ScaleBar { .. }) => true,
        _ => return,
    };
    let Some(loss) = text_fit_loss(s, li, id) else { return };
    // A scale bar's labels (#2581): its box too small for them, or one off the page.
    let notices = if bar {
        rustgis_render::layout::scale_bar_fit_notices(&[loss])
    } else {
        let left_out = rustgis_render::layout::lines_left_out_notice(&loss).filter(|_| lines);
        left_out.into_iter().chain(rustgis_render::layout::off_page_notice(&loss)).collect()
    };
    for notice in notices {
        s.warn(notice);
    }
}

/// Whether two maps of a first frame are the same map, as the project names maps (case aside); two
/// frames with none are.
pub(in crate::cmd::layout) fn same_map(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        (None, None) => true,
        _ => false,
    }
}

/// The map of the first frame of the layout `li`, which the figures of its texts are read from.
pub(in crate::cmd::layout) fn first_frame_map(s: &Session, li: usize) -> Option<String> {
    let l = s.project.layouts.get(li)?;
    match &l.element(l.first_frame()?)?.kind {
        ElementKind::MapFrame { map, .. } => Some(map.clone()),
        _ => None,
    }
}

/// [`say_text_statistics`] for every text of the layout `li`, a publication's or an import's, which
/// come with their texts already in it.
pub(in crate::cmd::layout) fn say_layout_text_statistics(s: &mut Session, li: usize) {
    let texts: Vec<u32> = s
        .project
        .layouts
        .get(li)
        .map(|l| l.elements.iter().filter(|e| matches!(e.kind, ElementKind::Text { .. })).map(|e| e.id).collect())
        .unwrap_or_default();
    for id in texts {
        say_text_statistics(s, li, id);
    }
}

/// Says which layer statistics (#2853) of the text `id` of the layout `li` are drawn as written,
/// and what each misses ([`rustgis_render::layout::statistic_notices`]).
/// One pass over each layer the text names, whatever the number of its figures (they are tallied
/// together), as the text-fit measure beside it makes one: about 8 to 33 ms a pass over a million
/// rows (a debug build; a layer read in place holds only its loaded window), the same read a bar
/// chart on the layer makes each time it is drawn.
pub(in crate::cmd::layout) fn say_text_statistics(s: &mut Session, li: usize, id: u32) {
    let said = match s.project.layouts.get(li).map(|l| (l, l.element(id).map(|e| &e.kind))) {
        Some((l, Some(ElementKind::Text { text, .. }))) => rustgis_render::layout::statistic_notices(l, &s.project.maps, text),
        _ => return,
    };
    for w in said {
        s.warn(w);
    }
}

/// What the element `id` of the layout `li` loses of its text on the page its series shows, as
/// an export at 300 dpi measures it ([`rustgis_render::layout::text_loss`]).
pub(in crate::cmd::layout) fn text_fit_loss(s: &Session, li: usize, id: u32) -> Option<rustgis_render::layout::TextLoss> {
    let l = s.project.layouts.get(li)?;
    let page = l.series.as_ref().map_or(0, |sr| sr.page);
    let series = series_info(s, li, page);
    let info = rustgis_render::layout::PageInfo {
        page: page + 1,
        pages: series.as_ref().map_or(1, |i| i.0),
        page_name: series.map(|i| i.1).unwrap_or_default(),
    };
    rustgis_render::layout::text_loss(l, &s.project.maps, 300.0, &info, id)
}

/// What a text is given that changes the lines it breaks into or the room they have, or shows it
/// (#2346): its line spacing, which moves its lines apart, and its case, which widens or narrows
/// them, among them. A move, a turn or a block alignment changes no line, so `update` says only
/// whether the text runs off the page then (#2703); nor does a colour (the Format pane sets one at
/// each step of a pick), except one that shows a text without ink, which `update` tells apart.
pub(in crate::cmd::layout) const TEXT_FIT_KEYS: [&str; 14] =
    ["text", "size", "w", "h", "wrap", "font", "fallback", "bold", "italic", "letterSpacing", "align", "visible", "lineSpacing", "case"];

/// A text size given as `key` (#2581): a legend's or a scale bar's `textSize`, a grid's
/// `labelSize`, in points within [`LAYOUT_TEXT_SIZE`]; `null` gives its `default` back, as for its
/// ink. Not given, `None`; a size out of the range is refused, as `columns` is, rather than drawn
/// at another size than the one asked for.
pub(in crate::cmd::layout) fn text_size_param(p: &Value, key: &str, default: f64) -> Result<Option<f64>, String> {
    match p.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(default)),
        Some(v) => v
            .as_f64()
            .filter(|v| LAYOUT_TEXT_SIZE.contains(v))
            .map(Some)
            .ok_or_else(|| format!("{key} is a number of points, {} to {}", LAYOUT_TEXT_SIZE.start(), LAYOUT_TEXT_SIZE.end())),
    }
}

/// A text element's italic, alignment and letter spacing (points) from `italic`, `align` and
/// `letterSpacing` in `p`, each keeping the current one when not given; one given but not of its
/// kind is an error.
pub(crate) fn text_style(p: &Value, italic: bool, align: TextAlign, spacing: f64) -> Result<(bool, TextAlign, f64), String> {
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    let italic = match p.get("italic") {
        None | Some(Value::Null) => italic,
        Some(Value::Bool(v)) => *v,
        Some(Value::String(t)) if ["true", "yes", "1", "on"].contains(&t.trim()) => true,
        Some(Value::String(t)) if ["false", "no", "0", "off"].contains(&t.trim()) => false,
        Some(_) => return Err("italic must be true or false".into()),
    };
    let align = match ps(p, "align") {
        Some(a) => TextAlign::from_name(a).ok_or_else(|| format!("unknown alignment '{a}' (left, center or right)"))?,
        None if given("align") => return Err("align must be left, center or right".into()),
        None => align,
    };
    let spacing = match (given("letterSpacing"), f(p, "letterSpacing")) {
        (_, Some(v)) => v.clamp(rustgis_map::layout::MIN_LETTER_SPACING, rustgis_map::layout::MAX_LETTER_SPACING),
        (true, None) => return Err("letterSpacing must be a number of points".into()),
        (false, None) => spacing,
    };
    Ok((italic, align, spacing))
}

/// What a `rotation` given for an element that isn't a text is told.
pub(in crate::cmd::layout) const ONLY_TEXT_ROTATION: &str = "only text elements take a rotation";

/// A text's `rotation` when given: degrees clockwise, kept within -180..=180 (450 is 90); refused
/// when not a number, as the other numbers of a layout are.
pub(in crate::cmd::layout) fn text_rotation(p: &Value) -> Result<Option<f64>, String> {
    Ok(given_num(p, "rotation", "a number of degrees, clockwise")?.map(rustgis_map::layout::rotation))
}

/// What a `wrap` given for an element that isn't a text is told.
pub(in crate::cmd::layout) const ONLY_TEXT_WRAP: &str = "only text elements wrap";

/// A text's `wrap` when given (#2346): true or false as [`crate::cmd::bool_value`] reads it; refused
/// when neither, so a script isn't left with a text that silently doesn't wrap.
pub(in crate::cmd::layout) fn text_wrap(p: &Value) -> Result<Option<bool>, String> {
    match p.get("wrap").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => crate::cmd::bool_value(v).map(Some).ok_or_else(|| "wrap must be true or false".into()),
    }
}

/// What a `valign`, `lineSpacing` or `case` given for an element that isn't a text is told.
pub(in crate::cmd::layout) const ONLY_TEXT_BLOCK: &str = "only text elements take a vertical alignment, line spacing or case";

/// Whether `p` gives a text's `valign`, `lineSpacing` or `case`: `null` is not given.
pub(in crate::cmd::layout) fn gives_text_block(p: &Value) -> bool {
    ["valign", "lineSpacing", "case"].iter().any(|k| gives(p, k))
}

/// A text's block alignment, line spacing and case from `p`, `current` where a key is not given
/// or null: `valign` and `case` a name as [`TextVAlign::from_name`] and [`TextCase::from_name`]
/// read it, `lineSpacing` a number (or a numeric string) from [`MIN_LINE_SPACING`] to
/// [`MAX_LINE_SPACING`]. Anything else is refused, so nothing changes, rather than drawn otherwise
/// than asked. The one reader of these keys: the commands and their capability check
/// (`command_uses_text_block`) both read them here.
pub(crate) fn text_block_settings(p: &Value, current: (TextVAlign, f64, TextCase)) -> Result<(TextVAlign, f64, TextCase), String> {
    let (mut valign, mut spacing, mut case) = current;
    if gives(p, "valign") {
        valign = ps(p, "valign").and_then(TextVAlign::from_name).ok_or("valign is top, middle or bottom")?;
    }
    if gives(p, "lineSpacing") {
        spacing = f(p, "lineSpacing")
            .filter(|v| (MIN_LINE_SPACING..=MAX_LINE_SPACING).contains(v))
            .ok_or("lineSpacing is a multiple of the usual line spacing, from 0.5 to 3")?;
    }
    if gives(p, "case") {
        case = ps(p, "case").and_then(TextCase::from_name).ok_or("case is none, upper or lower")?;
    }
    Ok((valign, spacing, case))
}

/// What a `font` given for an element that isn't a text is told.
pub(in crate::cmd::layout) const ONLY_TEXT_FONT: &str = "only text elements take a font or a fallback";

/// Whether `p` gives a `font` or a `fallback`, which only text elements take.
pub(in crate::cmd::layout) fn gives_font(p: &Value) -> bool {
    ["font", "fallback"].iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null()))
}

/// A text element's family from `font` and `fallback` in `p`, the current ones when not given:
/// `font` is "sans" or "serif" (#517), or an installed family's name (#762), kept as the font names
/// it where it is installed (its case), else as given. A named family is drawn in `fallback`
/// ("sans" or "serif") where it isn't installed: by default the one it has when it is named again,
/// else its own class (a serif's is the serif), else the sans.
pub(crate) fn text_font(p: &Value, current: (FontFamily, Option<String>)) -> Result<(FontFamily, Option<String>), String> {
    let fallback = match (ps(p, "fallback"), p.get("fallback").is_some_and(|v| !v.is_null())) {
        (Some(n), _) => Some(FontFamily::from_name(n).ok_or("fallback must be sans or serif")?),
        (None, true) => return Err("fallback must be sans or serif".into()),
        (None, false) => None,
    };
    let Some(font) = p.get("font").filter(|v| !v.is_null()) else {
        return match (fallback, current) {
            (Some(_), (_, None)) => Err("fallback goes with a font family's name".into()),
            (Some(f), (_, name)) => Ok((f, name)),
            (None, cur) => Ok(cur),
        };
    };
    let n = font.as_str().ok_or("font must be sans, serif or an installed font family's name")?;
    // Kept as a label's font is (trimmed, control characters out), then read as a generic name
    // or a family's.
    let name = rustgis_map::symbology::font_name(n).ok_or("font must be sans, serif or an installed font family's name")?;
    if let Some(generic) = FontFamily::from_name(&name) {
        return match fallback {
            Some(_) => Err("fallback goes with a font family's name, not sans or serif".into()),
            None => Ok((generic, None)),
        };
    }
    let installed = installed_family(&name);
    let kept = installed.map_or(name, |f| f.name.clone());
    let fallback = fallback.unwrap_or_else(|| default_fallback(installed, &kept, &current));
    Ok((fallback, Some(kept)))
}

/// The installed family named `name` (in any case, as the index compares names): none while the
/// app is still indexing them (a command runs on its interface thread, which mustn't wait).
pub(in crate::cmd::layout) fn installed_family(name: &str) -> Option<&'static rustgis_render::families::Family> {
    use rustgis_render::families;
    families::available()?.get(usize::from(families::find_ready(name)?))
}

/// Whether the face of `family` a text in `style` (bold, italic) draws in can be read (a file past
/// the size RustGIS reads, or a damaged one, can't): that face only, read once and kept, as
/// drawing it would.
pub(in crate::cmd::layout) fn readable(family: &rustgis_render::families::Family, (bold, italic): (bool, bool)) -> bool {
    use rustgis_render::families;
    families::pick(family, bold, italic).and_then(|(face, _)| families::bytes(face)).is_some()
}

/// A named family's fallback when none is given: the one it has when it is named again, else its
/// own class (a serif's is the serif), else, where the family isn't known (not installed, or
/// the fonts not indexed yet), a name that says "Serif" without "Sans" (Noto Serif), else the
/// sans.
pub(in crate::cmd::layout) fn default_fallback(
    installed: Option<&rustgis_render::families::Family>,
    name: &str,
    current: &(FontFamily, Option<String>),
) -> FontFamily {
    if current.1.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(name)) {
        return current.0;
    }
    let lower = name.to_lowercase();
    match installed {
        Some(f) if f.serif => FontFamily::Serif,
        None if lower.contains("serif") && !lower.contains("sans") => FontFamily::Serif,
        _ => FontFamily::Sans,
    }
}

/// What a text is told when it isn't drawn as asked: a named family this computer hasn't is drawn
/// in its fallback, and a serif asked for where none is installed (the web app, a bare Linux box)
/// in the sans, so an export never differs from what was asked unsaid. While the app is still
/// indexing the installed fonts (its first seconds), a family named with no `fallback` given is
/// told that its fallback came from its name, not the font.
pub(in crate::cmd::layout) fn font_warning(
    font: FontFamily,
    name: Option<&str>,
    fallback_given: bool,
    style: (bool, bool),
) -> Option<String> {
    let serif = rustgis_render::fonts::serif_available();
    let drawn = if font == FontFamily::Serif && serif { "serif" } else { "sans-serif" };
    let indexed = rustgis_render::families::available().is_some();
    match name {
        Some(n) if !indexed && !fallback_given => Some(format!(
            "the installed fonts are still being read, so {n}'s fallback ({drawn}) comes from its name, not the font: give \"fallback\" to choose it"
        )),
        // Installed, but its face can't be loaded (a file too large or damaged, or past the font
        // bytes RustGIS keeps in a session): drawn in its fallback too.
        Some(n) if indexed && installed_family(n).is_some_and(|f| !readable(f, style)) => Some(format!(
            "{n} is installed but its font can't be loaded (a file too large or damaged, or past the fonts RustGIS keeps in memory), so the text is drawn in the {drawn}"
        )),
        Some(n) if indexed && rustgis_render::families::find_ready(n).is_none() => {
            Some(format!("{n} is not installed on this computer, so the text is drawn in the {drawn}"))
        }
        Some(_) => None,
        None => {
            (font == FontFamily::Serif && !serif).then(|| "no serif font is installed here, so the text is drawn in the sans-serif".into())
        }
    }
}
