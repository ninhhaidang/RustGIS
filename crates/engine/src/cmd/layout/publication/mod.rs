//! `layout.publication`: a sheet or a figure laid out around its map.

use super::*;

mod arrangement;
pub use arrangement::*;
mod coverage;
pub(in crate::cmd::layout) use coverage::*;
mod parts;
use parts::*;
mod placement;
pub(crate) use placement::*;
mod sheet;
pub use sheet::*;
mod texts;
pub(crate) use texts::*;

/// A `"decimal"` parameter: `","` for a decimal comma, `"."` for a point; `None` when not given.
pub(in crate::cmd::layout) fn decimal_mark(p: &Value) -> Result<Option<bool>, String> {
    match p.get("decimal").filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => match v.as_str().map(str::trim) {
            Some(",") => Ok(Some(true)),
            Some(".") => Ok(Some(false)),
            _ => Err("decimal is \".\" or \",\"".into()),
        },
    }
}

/// What a publication layout locates its area on.
#[derive(Clone, Copy, PartialEq)]
pub(in crate::cmd::layout) enum Locator {
    Globe,
    Inset,
    None,
}

/// The scale at which `map` on the globe, centred on `center`, draws a globe `d_mm` across in a
/// frame: the globe's visible disc as the renderer lays it out for the frame's camera at a probe
/// scale, its size going as one over the scale.
pub(in crate::cmd::layout) fn globe_scale(map: &rustgis_map::Map, center: Coord, d_mm: f64) -> f64 {
    const PROBE: f64 = 1e8;
    let cam = rustgis_map::layout::frame_camera(map, center, PROBE, 0.0, 0.0);
    let side = d_mm.max(1.0) * rustgis_map::layout::PT_PER_MM;
    let radius = rustgis_map::globe::Globe::new(&cam, side, side).disc_radius();
    if radius.is_finite() && radius > 0.0 {
        (PROBE * 2.0 * radius / side).clamp(rustgis_map::layout::MIN_FRAME_SCALE, rustgis_map::layout::MAX_FRAME_SCALE)
    } else {
        PROBE
    }
}

/// Whether a sheet `short` × `long` mm suits an area `aspect` times as tall as it is wide better
/// turned to landscape: the map comes out clearly wider there (by more than 3 %, portrait winning
/// a near tie), as near as the sheet's texts can be told before they are laid out (a header and a
/// footer of about 70 mm on A4 between them, a panel's title in its panel; a style with a column
/// leaves the map two thirds of the width).
pub(in crate::cmd::layout) fn landscape_suits(aspect: f64, short: f64, long: f64, style: Style) -> bool {
    let k = short / 210.0;
    let map_w = |w: f64, h: f64| {
        let texts = if style == Style::Panel { 50.0 } else { 100.0 } * k;
        // An edge sheet's map takes the page's whole width.
        let sides = if style == Style::Edge { 0.0 } else { 30.0 * k };
        let (iw, ih) = (w - sides, h - texts);
        let fw = if matches!(style, Style::Panel | Style::Editorial | Style::Modular) { 0.68 * iw } else { iw };
        let (fw, fh) = (fw - 2.0 * LABEL_ROOM_SIDE * k, ih - 2.0 * LABEL_ROOM_END * k);
        fw.min(fh / aspect.max(1e-6))
    };
    map_w(long, short) > 1.03 * map_w(short, long)
}

/// `layout.publication`: a finished sheet or figure of the active map, its extent fitted at a
/// round scale, with a graticule, legend, scale bar and north arrow; a sheet adds a frame, title,
/// subtitle and a footer of two boxed columns (sources, cartographic reference) with a note and
/// authorship under them, each text wrapped to its box with the layout's own font.
///
/// It runs on the UI thread, with no Cancel, as it did before the veil: none of the background
/// paths (tools, exports, commands that make a raster, models and scripts on a copy) takes a
/// command that adds a layout, and a locator map, to the project yet. Its cost grows with the
/// map's features: placing the surrounds draws the frame once on a grid of 384 cells, and when no
/// shape of the legend fits, the search draws it at most four times more, once per scale it tries,
/// each time on a view wider than the frame that a streamed layer is read for, waited for here,
/// and where the projection stretches, up to four moved frames per scale on their own (in Web
/// Mercator, the two moved up and down)
/// (#1379; the whole command took about 0.7 s on a 14 MB geology map, 0.2 s before the search, and
/// 0.9 s with its layers read in place).
pub(in crate::cmd::layout) fn publication(s: &mut Session, p: &Value) -> CmdResult {
    let (auto, profile) = publication_profile(p)?;
    let (sheet, page_w, page_h) = (profile.sheet, profile.width, profile.height);
    let whole_globe = shows_whole_globe(s.map(), p);
    let extent = match p.get("extent").filter(|v| !v.is_null()) {
        // The view; on a globe the whole Earth, as geoprocessing takes it (the globe camera's flat
        // extent means nothing there).
        None if whole_globe => Extent::new(-180.0, -90.0, 180.0, 90.0),
        None => {
            let e = crate::cmd::workspace::view_extent(s);
            Extent::new(e.min_x.max(-180.0), e.min_y.max(-90.0), e.max_x.min(180.0), e.max_y.min(90.0))
        }
        Some(v) => {
            let b: Option<Vec<f64>> =
                v.as_array().filter(|a| a.len() == 4).and_then(|a| a.iter().map(|x| x.as_f64().filter(|x| x.is_finite())).collect());
            match b.as_deref() {
                Some([w, so, e, n]) if w < e && so < n && *w >= -180.0 && *e <= 180.0 && *so >= -90.0 && *n <= 90.0 => {
                    Extent::new(*w, *so, *e, *n)
                }
                _ => return Err("extent is [west, south, east, north] in degrees".into()),
            }
        }
    };
    if !(extent.width() > 0.0 && extent.height() > 0.0) {
        return Err("the map's extent isn't known; give extent".into());
    }
    let one = |k: &str| texts_of(p, k).map(|t| t.map(|t| t.join(" ")).filter(|t| !t.trim().is_empty()));
    let (title, subtitle, note) = (one("title")?, one("subtitle")?, one("note")?);
    let sources = texts_of(p, "sources")?.unwrap_or_default();
    let reference = texts_of(p, "reference")?;
    // An empty author leaves the line out; none given, the default.
    let author = match texts_of(p, "author")? {
        None => Some(PUBLICATION_DEFAULT_CREDIT.to_string()),
        Some(t) => Some(t.join(" ")).filter(|t| !t.trim().is_empty()),
    };
    let headings = texts_of(p, "headings")?.unwrap_or_default();
    let comma = decimal_mark(p)?.unwrap_or(false);
    let locator = publication_locator(p, sheet, whole_globe)?;
    let colorbar = publication_colorbar(p)?;
    let may_veil = publication_veil(p)?;
    let context_in_legend = publication_flag(p, "contextInLegend")?.unwrap_or(false);
    let (north_arrow, mut arrow_kind) = publication_north_arrow(p)?;
    let legend_box_always = publication_legend_box(p)?;
    // A style or an accent that isn't text is refused, as an unknown one is, rather than left out.
    for k in ["style", "accent"] {
        if p.get(k).is_some_and(|v| !v.is_null() && !v.is_string()) {
            return Err(format!("{k} is text: {}", if k == "style" { "a style's name, such as \"panel\"" } else { "a colour as #RRGGBB" }));
        }
    }
    let style = match ps(p, "style") {
        None => Style::Classic,
        Some(raw) => Style::parse(raw).ok_or_else(|| {
            let x = raw.trim();
            format!("style is \"classic\", \"banner\", \"panel\", \"editorial\", \"modular\", \"poster\" or \"edge\", not \"{x}\"")
        })?,
    };
    let accent = color(p, "accent", style.accent())?;
    let tries = arrangements_to_try(s, p.get("arrangement").unwrap_or(&Value::Null))?;
    // A translucent accent would leave a band, a poster's page or the headings faint or unseen.
    if accent.a() != 255 {
        return Err("accent is an opaque colour as #RRGGBB".into());
    }
    // Only an edge sheet draws these: given to another, they are refused rather than left out.
    let edge = sheet && style == Style::Edge;
    if let Some(k) = ["paper", "kicker"].into_iter().find(|k| !edge && p.get(*k).is_some_and(|v| !v.is_null())) {
        return Err(format!("{k} is for a sheet with \"style\": \"edge\""));
    }
    // White is the page's own colour, kept as none, as `layout.page` keeps it.
    let page_colour = crate::cmd::opaque_color(p, "paper")?.flatten().filter(|c| *c != Rgba::rgb(0xFF, 0xFF, 0xFF));
    let kicker = one("kicker")?;
    // Where the coverage grid the placement searches is written as an image (#1379).
    let coverage_path = match p.get("coverage") {
        None | Some(Value::Null) => None,
        Some(Value::String(path)) if path.trim().to_ascii_lowercase().ends_with(".png") => Some(path.trim().to_string()),
        Some(_) => return Err("coverage is the path of a .png file to write the coverage grid to".into()),
    };
    // No orientation asked: the one the area's map comes out larger on, from its extents east to
    // west and north to south on the map's projection and the room the style leaves the map.
    let profile = match &auto {
        Some(size) if aspect(s, &extent).is_some_and(|a| landscape_suits(a, page_w.min(page_h), page_w.max(page_h), style)) => {
            let id = format!("{size}-landscape");
            PUBLICATION_PROFILES.iter().find(|pr| pr.id == id).unwrap_or(profile)
        }
        _ => profile,
    };
    let (page_w, page_h) = (profile.width, profile.height);
    let heading = |i: usize, or: &str| headings.get(i).cloned().unwrap_or_else(|| or.to_string());
    let (projection, datum) = projection_and_datum(s);
    // A sheet's sizes follow its short side (A4's 210 mm is 1); a figure keeps its texts at their
    // printed size, whatever its width.
    let k = if sheet { page_w.min(page_h) / 210.0 } else { 1.0 };
    // So do its legend's rows, its scale bar's and graticule's labels and the strips those take
    // inside the frame (#2581): A4's sizes times k, within the 4 to 100 points a text size takes
    // on every sheet size (k from 0.70 on A5 to 4.0 on A0).
    let (legend_pt, bar_pt, grid_pt) = (rustgis_map::layout::LEGEND_TEXT_PT * k, SCALE_BAR_TEXT_PT * k, GRID_LABEL_PT * k);
    // Labels along the left and bottom edges of a figure's frame, along all four of a sheet's,
    // unless "grid" says otherwise.
    let edges = GridEdges { top: sheet, bottom: true, left: true, right: sheet };
    let grid = publication_grid(p, Grid { step: grid_step(&extent), edges, label_size: grid_pt, ..Grid::default() })?;
    let mut scale_kind = publication_scale_bar(p, bar_pt)?;
    // The step of the frame's graticule: none without a grid, or with a measured one.
    let step = grid.as_ref().filter(|g| g.kind == GridKind::Graticule).map(|g| g.step);
    // The edges that carry labels, and the strips they take inside the frame.
    let labelled =
        grid.as_ref().filter(|g| g.labels).map_or(GridEdges { top: false, bottom: false, left: false, right: false }, |g| g.edges);
    let strip_scale = grid.as_ref().map_or(k, |g| k * (g.label_size / grid_pt));
    let room_side = if labelled.left || labelled.right { LABEL_ROOM_SIDE * strip_scale } else { 0.0 };
    let room_end = if labelled.top || labelled.bottom { LABEL_ROOM_END * strip_scale } else { 0.0 };
    let reference = reference.unwrap_or_else(|| {
        let line = if step.is_some() { "Datum: {datum} · Graticule: {grid}" } else { "Datum: {datum}" };
        vec!["Projection: {projection}".into(), line.into()]
    });
    let step_said = step.map_or_else(|| "—".to_string(), step_text);
    let fill = |t: &str| t.replace("{projection}", &projection).replace("{datum}", &datum).replace("{grid}", &step_said);
    // Text is measured as it is drawn: bold as bold, and the tokens the layout fills only when it
    // draws (`{date}`, `{scale}`…) as wide as a typical value of theirs.
    let (map_name, crs) = (s.map().name.clone(), s.map().projection.label());
    // A layer statistic (#2853) by the figure it shows now, as `{map}` is by the map's name: the
    // layers are in the project, so the box is sized for what is drawn, not for the token written.
    let figures = {
        let texts: Vec<&str> = [&kicker, &title, &subtitle, &note, &author]
            .into_iter()
            .filter_map(|t| t.as_deref())
            .chain(sources.iter().chain(&reference).chain(&headings).map(String::as_str))
            .collect();
        crate::render::layout::statistic_figures(s.map(), comma, &texts)
    };
    let drawn = move |t: &str| {
        // Most text has no token: measured as it is.
        if !t.contains('{') {
            return t.to_string();
        }
        let t = figures.iter().fold(t.to_string(), |t, (token, figure)| t.replace(token.as_str(), figure));
        [
            ("{date}", "0000-00-00"),
            ("{time}", "00:00"),
            ("{scale}", "1:10,000,000"),
            ("{pages}", "00"),
            ("{page}", "00"),
            ("{layers}", "00"),
            ("{center}", "00.000°S, 000.000°W"),
            ("{map}", map_name.as_str()),
            ("{crs}", crs.as_str()),
        ]
        .iter()
        .fold(t, |t, (token, like)| t.replace(token, like))
    };
    // A text breaks at spaces, but `{map}` is one word as wide as the map's name. Where a line
    // holding it is wider than the box with the name in it, the text is stored as it is and its
    // element wraps to the box when drawn, so it still follows a rename of the map; its box is
    // sized by the lines the page draws it in, which measures a line's ink.
    let broken = |t: &str, w: f64, pt: f64, bold: bool, align: TextAlign, font: FontFamily| {
        let (t, w) = (fill(t), w.max(1.0));
        let width = |x: &str| match font {
            FontFamily::Serif => crate::render::report::serif_width_mm(&drawn(x), pt as f32, bold),
            FontFamily::Sans => crate::render::report::width_mm(&drawn(x), pt as f32, bold),
        };
        // A line with `{map}`, too wide for its box with the name in it, is kept whole and wrapped by
        // the page, so it still follows a rename of the map. So is any line with a layer statistic
        // (#2853): its figure is as wide as its data makes it, which is only known when the page is
        // drawn, and a token whose layer has a space in its name must not be broken at it.
        if t.lines().any(|l| (l.contains("{map}") && width(l) > w) || !rustgis_map::layout::stat_tokens(l).is_empty()) {
            let lines = crate::render::layout::wrapped_lines(&drawn(&t), w, pt, bold, font, align);
            Broken { text: t, lines, wrap: true }
        } else {
            Broken::from(crate::render::report::wrap_with(&t, w, width))
        }
    };
    let wrap_as = |t: &str, w: f64, pt: f64, bold: bool| broken(t, w, pt, bold, TextAlign::Left, FontFamily::Sans);
    let wrap = |t: &str, w: f64, pt: f64| wrap_as(t, w, pt, false);
    let pitch = crate::render::layout::line_pitch_mm;
    let ink = Rgba::rgb(0x2B, 0x26, 0x20);
    let soft = Rgba::rgb(0x5E, 0x55, 0x47);
    let clear = Rgba::new(0, 0, 0, 0);
    let mut warnings: Vec<String> = Vec::new();
    // A sheet's default locator would show the globe again, smaller; one asked for still comes,
    // and "none" asked for is no news.
    if sheet && whole_globe && p.get("locator").is_none_or(Value::is_null) {
        warnings.push("no locator: the map shows the whole globe; \"locator\": \"globe\" or \"inset\" adds one".into());
    }
    // Texts and boxes around the map, placed while its frame is worked out.
    let mut around: Vec<(String, (f64, f64, f64, f64), ElementKind)> = Vec::new();
    // What a style draws under the map and its surrounds: a page's colour, a card or a panel.
    let mut behind: Vec<(String, (f64, f64, f64, f64), ElementKind)> = Vec::new();
    // What spans the whole page (a sheet's neatline, a poster's colour), laid on the full page
    // whatever a pass below frees, so never moved: over the map (`true`) or under everything,
    // each before the rest of its side.
    let mut page_wide: Vec<(String, (f64, f64, f64, f64), ElementKind, bool)> = Vec::new();
    // The sources and the reference that have something in them, each its heading in `head_pt` and
    // lines in `body_pt`, `tw` wide. Each one's elements keep their names (`Sources`, `Reference`)
    // whatever its heading reads, the heading filled and wrapped like the rest.
    let sections = |tw: f64, head_pt: f64, body_pt: f64| -> Vec<(&'static str, Broken, Broken)> {
        [("Sources", heading(0, "Sources"), &sources), ("Reference", heading(1, "Cartographic reference"), &reference)]
            .into_iter()
            .filter(|(.., texts)| !texts.is_empty())
            .map(|(name, head, texts)| {
                // Filled first, so `{projection}` reads in capitals too; the live tokens left keep
                // their case (wrap_as fills again, which then finds nothing).
                let head = wrap_as(&capitals(&fill(&head)), tw, head_pt, true);
                let parts: Vec<Broken> = texts.iter().map(|t| wrap(t, tw, body_pt)).filter(|b| !b.lines.is_empty()).collect();
                let text = parts.iter().map(|b| b.text.as_str()).collect::<Vec<_>>().join("\n");
                // One of them wrapping as drawn wraps their element, so all are measured so.
                let body = if parts.iter().any(|b| b.wrap) {
                    let (font, align) = (FontFamily::Sans, TextAlign::Left);
                    let lines = crate::render::layout::wrapped_lines(&drawn(&text), tw.max(1.0), body_pt, false, font, align);
                    Broken { text, lines, wrap: true }
                } else {
                    Broken { text, lines: parts.into_iter().flat_map(|b| b.lines).collect(), wrap: false }
                };
                (name, head, body)
            })
            .collect()
    };
    // A panel's or an editorial page's map is as wide as its column: where the area is less tall
    // than the frame the sheet leaves it, the sheet is laid out again with the frame the area's
    // height (and a little room), and the whole put in the middle of the page, rather than the map
    // showing more of its surroundings.
    let page_full = page_h;
    let mut freed = 0.0f64;
    let (mut target, mut passes): (Option<f64>, u32) = (None, 0);
    // A later pass failing where an earlier one held (its shorter page leaving the texts too
    // little room) settles on the last that held: only the first pass's failure is the request's.
    let (mut held, mut settled) = (0.0f64, false);
    let warned = warnings.len();
    let (w, h, (fx, fy, fw, fh), column) = loop {
        around.clear();
        behind.clear();
        page_wide.clear();
        warnings.truncate(warned);
        let page_h = page_full - freed;
        let attempt = (|| -> Result<_, String> {
            Ok(if sheet {
                let (m, pad) = (10.0 * k, 5.0 * k);
                let (x0, iw) = (m + pad, page_w - 2.0 * (m + pad));
                let (title_pt, sub_pt, head_pt, body_pt, small_pt) = (18.0 * k, 10.5 * k, 7.5 * k, 7.0 * k, 6.5 * k);
                // A neatline round the sheet, but on a poster's coloured page, a page of cards or a
                // sheet whose map runs to its edges.
                if !style.cards() && !edge {
                    page_wide.push(("Frame".into(), (m, m, page_w - 2.0 * m, page_full - 2.0 * m), pub_box(clear, ink, 0.8), true));
                }
                // The footer, from the bottom up across the sheet: authorship, the note (unless the style
                // sets it beside the map), then the sources and reference boxed side by side (unless it
                // stacks them in its panel), as tall as the longer column. Its top.
                type Around = Vec<(String, (f64, f64, f64, f64), ElementKind)>;
                let footer = |around: &mut Around, look: &Look, note_here: bool, boxes_here: bool| -> f64 {
                    let bottom = page_h - m - pad;
                    let author_lines =
                        author.as_deref().map(|t| broken(t, iw, small_pt, false, TextAlign::Right, FontFamily::Sans)).unwrap_or_default();
                    let author_h = author_lines.lines.len() as f64 * pitch(small_pt);
                    let author_y = bottom - author_h;
                    if !author_lines.lines.is_empty() {
                        around.push((
                            "Author".into(),
                            (x0, author_y, iw, author_h),
                            pub_text(author_lines, small_pt, look.small, false, TextAlign::Right),
                        ));
                    }
                    let mut foot = author_y;
                    let note_lines = note.as_deref().filter(|_| note_here).map(|t| wrap(t, iw, body_pt)).unwrap_or_default();
                    if !note_lines.lines.is_empty() {
                        let nh = note_lines.lines.len() as f64 * pitch(body_pt);
                        foot -= nh + 1.5 * k;
                        around.push(("Note".into(), (x0, foot, iw, nh), pub_text(note_lines, body_pt, look.small, false, TextAlign::Left)));
                    }
                    if !boxes_here {
                        return foot;
                    }
                    // The columns side by side (one alone takes the width). A look with neither fill
                    // nor outline has no box, which would only be in the way of clicks.
                    let boxed = look.fill.a() > 0 || look.outline.a() > 0;
                    let (gap, inner) = (4.0 * k, if boxed { 2.5 * k } else { 0.0 });
                    let n = [!sources.is_empty(), !reference.is_empty()].iter().filter(|x| **x).count().max(1) as f64;
                    let cw = (iw - gap * (n - 1.0)) / n;
                    let tw = cw - 2.0 * inner;
                    let columns = sections(tw, head_pt, body_pt);
                    let heads = columns.iter().map(|(_, h, _)| h.lines.len()).max().unwrap_or(1);
                    let rows = columns.iter().map(|(.., l)| l.lines.len()).max().unwrap_or(0);
                    if rows > 0 {
                        let head_h = heads as f64 * pitch(head_pt);
                        let bh = 2.0 * inner + head_h + rows as f64 * pitch(body_pt);
                        foot -= bh + 2.0 * k;
                        for (i, (name, head, lines)) in columns.into_iter().enumerate() {
                            let bx = x0 + i as f64 * (cw + gap);
                            if boxed {
                                around.push((
                                    format!("{name} Box"),
                                    (bx, foot, cw, bh),
                                    pub_card(look.fill, look.outline, look.width, look.radius),
                                ));
                            }
                            let tx = bx + inner;
                            around.push((
                                format!("{name} Heading"),
                                (tx, foot + inner, tw, head.lines.len() as f64 * pitch(head_pt)),
                                pub_text(head, head_pt, look.head, true, TextAlign::Left),
                            ));
                            if !lines.lines.is_empty() {
                                let body = (tx, foot + inner + head_h, tw, lines.lines.len() as f64 * pitch(body_pt));
                                around.push((name.to_string(), body, pub_text(lines, body_pt, look.body, false, TextAlign::Left)));
                            }
                        }
                    }
                    foot
                };
                // The map's frame between `top` and `bottom`, at least 40 mm tall (on A4).
                let room = |top: f64, bottom: f64| {
                    if bottom - top < 40.0 * k {
                        Err("the texts leave the map no room: shorten the footer, the note or the subtitle".to_string())
                    } else {
                        Ok(bottom - top)
                    }
                };
                let white = Rgba::rgb(0xFF, 0xFF, 0xFF);
                let plain = Look {
                    head: ink,
                    body: soft,
                    small: soft,
                    fill: clear,
                    outline: Rgba::new(0x2B, 0x26, 0x20, 140),
                    width: 0.5,
                    radius: 0.0,
                };
                let (card_r, card_pad) = (3.0 * k, 2.5 * k);
                let card_line = Rgba::new(0x2B, 0x26, 0x20, 60);
                match style {
                    Style::Classic => {
                        let mut top = m + pad;
                        if let Some(t) = &title {
                            let lines = broken(t, iw, title_pt, true, TextAlign::Center, FontFamily::Sans);
                            let th = lines.lines.len() as f64 * pitch(title_pt) + 1.5 * k;
                            around.push(("Title".into(), (x0, top, iw, th), pub_text(lines, title_pt, ink, true, TextAlign::Center)));
                            top += th;
                        }
                        if let Some(t) = &subtitle {
                            let lines = broken(t, iw, sub_pt, false, TextAlign::Center, FontFamily::Sans);
                            let sh = lines.lines.len() as f64 * pitch(sub_pt) + 1.0 * k;
                            around.push(("Subtitle".into(), (x0, top, iw, sh), pub_text(lines, sub_pt, soft, false, TextAlign::Center)));
                            top += sh;
                        }
                        let foot = footer(&mut around, &plain, true, true);
                        let (ft, fb) = (top + 3.0 * k, foot - 4.0 * k);
                        (page_w, page_h, (x0, ft, iw, room(ft, fb)?), None)
                    }
                    Style::Banner | Style::Poster => {
                        // A band in the accent colour across the top: the title at its left in white, the
                        // subtitle beside it past a thin rule. With neither, no band. A poster's whole page
                        // is that colour, its map in a light card and its footer in darker blocks.
                        let poster = style == Style::Poster;
                        if poster {
                            page_wide.push(("Background".into(), (0.0, 0.0, page_w, page_full), pub_box(accent, clear, 0.0), false));
                        }
                        let mut top = m + pad;
                        if title.is_some() || subtitle.is_some() {
                            let big = title_pt * 1.2;
                            let t_max = if subtitle.is_some() { 0.42 * iw } else { iw };
                            let mut t_lines = title.as_deref().map(|t| wrap_as(t, t_max, big, true)).unwrap_or_default();
                            let t_w = t_lines
                                .lines
                                .iter()
                                .map(|x| crate::render::report::width_mm(&drawn(x), big as f32, true))
                                .fold(0.0, f64::max)
                                .min(t_max)
                                .ceil();
                            // A title that wraps as it is drawn is measured again at the box it got.
                            if let Some(t) = title.as_deref().filter(|_| t_lines.wrap) {
                                t_lines = wrap_as(t, t_w, big, true);
                            }
                            let rule = if t_lines.lines.is_empty() { 0.0 } else { 6.0 * k };
                            let s_x = x0 + t_w + rule;
                            let s_w = (x0 + iw - s_x).max(20.0 * k);
                            let s_lines = subtitle.as_deref().map(|t| wrap(t, s_w, sub_pt)).unwrap_or_default();
                            let (t_h, s_h) = (t_lines.lines.len() as f64 * pitch(big), s_lines.lines.len() as f64 * pitch(sub_pt));
                            let (titled, subtitled) = (!t_lines.lines.is_empty(), !s_lines.lines.is_empty());
                            let band_h = t_h.max(s_h) + 2.0 * pad;
                            let mid = m + band_h / 2.0;
                            if !poster {
                                around.push(("Banner".into(), (m, m, page_w - 2.0 * m, band_h), pub_box(accent, clear, 0.0)));
                            }
                            if titled {
                                around.push((
                                    "Title".into(),
                                    (x0, mid - t_h / 2.0, t_w, t_h),
                                    pub_text(t_lines, big, white, true, TextAlign::Left),
                                ));
                            }
                            if titled && subtitled {
                                around.push((
                                    "Banner Rule".into(),
                                    (x0 + t_w + rule / 2.0 - 0.5, mid - t_h.max(s_h) / 2.0, 1.0, t_h.max(s_h)),
                                    ElementKind::Line {
                                        color: Rgba::new(0xFF, 0xFF, 0xFF, 170),
                                        width: 0.75,
                                        dir: LineDir::Vertical,
                                        head: LineHead::None,
                                        dash: None,
                                    },
                                ));
                            }
                            if subtitled {
                                around.push((
                                    "Subtitle".into(),
                                    (s_x, mid - s_h / 2.0, s_w, s_h),
                                    pub_text(s_lines, sub_pt, Rgba::rgb(0xF2, 0xEE, 0xE6), false, TextAlign::Left),
                                ));
                            }
                            top = m + band_h;
                        }
                        // A poster's footer: blocks a shade lighter than its page, amber headings, light text.
                        let shade = |c: u8| c.saturating_add(((255 - u16::from(c)) / 8) as u8);
                        let look = if poster {
                            Look {
                                head: Rgba::rgb(0xE8, 0xB0, 0x4B),
                                body: Rgba::rgb(0xED, 0xE6, 0xDA),
                                small: Rgba::rgb(0xCF, 0xC6, 0xB8),
                                fill: Rgba::rgb(shade(accent.r()), shade(accent.g()), shade(accent.b())),
                                outline: clear,
                                width: 0.0,
                                radius: card_r,
                            }
                        } else {
                            plain
                        };
                        let foot = footer(&mut around, &look, true, true);
                        let (ft, fb) = (top + 4.0 * k, foot - 4.0 * k);
                        let h = room(ft, fb)?;
                        if poster {
                            behind.push(("Map Card".into(), (x0, ft, iw, h), pub_card(Rgba::rgb(0xF6, 0xF1, 0xE7), clear, 0.0, card_r)));
                            (page_w, page_h, (x0 + card_pad, ft + card_pad, iw - 2.0 * card_pad, h - 2.0 * card_pad), None)
                        } else {
                            (page_w, page_h, (x0, ft, iw, h), None)
                        }
                    }
                    Style::Modular => {
                        // Cards on a light page: the title and subtitle at the top left over a short bar of
                        // the accent colour; the map in a card at the left; at the right the locator and
                        // the legend, each in a card; the sources and reference in tinted cards side by
                        // side under them, the note and authorship under all.
                        let mut y = m + pad;
                        let tw = 0.75 * iw;
                        if let Some(t) = &title {
                            let big = title_pt * 1.3;
                            let lines = wrap_as(t, tw, big, true);
                            let th = lines.lines.len() as f64 * pitch(big);
                            around.push(("Title".into(), (x0, y, tw, th), pub_text(lines, big, ink, true, TextAlign::Left)));
                            y += th;
                        }
                        if let Some(t) = &subtitle {
                            let lines = wrap(t, tw, sub_pt);
                            let sh = lines.lines.len() as f64 * pitch(sub_pt);
                            around.push(("Subtitle".into(), (x0, y, tw, sh), pub_text(lines, sub_pt, soft, false, TextAlign::Left)));
                            y += sh;
                        }
                        if y > m + pad {
                            around.push(("Title Rule".into(), (x0, y + 1.5 * k, 14.0 * k, 1.2 * k), pub_box(accent, clear, 0.0)));
                            y += 5.0 * k;
                        }
                        let look = Look {
                            head: accent,
                            body: soft,
                            small: soft,
                            fill: Rgba::new(accent.r(), accent.g(), accent.b(), 0x1A),
                            outline: clear,
                            width: 0.0,
                            radius: card_r,
                        };
                        let foot = footer(&mut around, &look, true, true);
                        let bottom = foot - 4.0 * k;
                        let h = room(y, bottom)?;
                        let gap = 5.0 * k;
                        let cw = (0.3 * iw).max(48.0 * k).min(0.45 * iw);
                        let (mw, cx) = (iw - cw - gap, x0 + iw - cw);
                        behind.push(("Map Card".into(), (x0, y, mw, h), pub_card(white, card_line, 0.4, card_r)));
                        let frame = (x0 + card_pad, y + card_pad, mw - 2.0 * card_pad, h - 2.0 * card_pad);
                        let column = Column { x: cx + card_pad, y: y + card_pad, w: cw - 2.0 * card_pad, h: h - 2.0 * card_pad };
                        (page_w, page_h, frame, Some(column))
                    }
                    Style::Edge => {
                        // A kicker, the title and the subtitle at the top left; the map from one side
                        // of the page to the other; under a thin rule, the footer with no boxes. On a
                        // dark paper (the rule a presentation's blank page uses) the texts are light.
                        let dark = page_colour
                            .is_some_and(|c| 0.299 * f64::from(c.r()) + 0.587 * f64::from(c.g()) + 0.114 * f64::from(c.b()) < 140.0);
                        let (ink, soft) = if dark { (Rgba::rgb(0xF2, 0xEE, 0xE6), Rgba::rgb(0xCF, 0xC6, 0xB8)) } else { (ink, soft) };
                        let mut y = m + pad;
                        if let Some(t) = &kicker {
                            let pt = 9.0 * k;
                            // Letter spacing in points, and the millimetres it adds to each character.
                            let spacing = 0.18 * pt;
                            // As the renderer spaces them: 1.33 points of the drawing per point of spacing.
                            let added = spacing * 1.33 / rustgis_map::layout::PT_PER_MM;
                            let t = capitals(&fill(t));
                            let width = |x: &str| {
                                let x = drawn(x);
                                crate::render::report::width_mm(&x, pt as f32, true) + x.chars().count() as f64 * added
                            };
                            // As `broken`: a text with the map's name or a layer statistic is kept
                            // whole and wraps as it is drawn.
                            let live = t.contains("{map}") || !rustgis_map::layout::stat_tokens(&t).is_empty();
                            let lines = if live {
                                let lines = crate::render::report::wrap_with(&drawn(&t), iw, width);
                                Broken { text: t, lines, wrap: true }
                            } else {
                                Broken::from(crate::render::report::wrap_with(&t, iw, width))
                            };
                            let kh = lines.lines.len() as f64 * pitch(pt) + 1.0 * k;
                            let mut kind = pub_text(lines, pt, accent, true, TextAlign::Left);
                            if let ElementKind::Text { letter_spacing, .. } = &mut kind {
                                *letter_spacing = spacing;
                            }
                            around.push(("Kicker".into(), (x0, y, iw, kh), kind));
                            y += kh;
                        }
                        if let Some(t) = &title {
                            let big = title_pt * 1.3;
                            let lines = wrap_as(t, iw, big, true);
                            let th = lines.lines.len() as f64 * pitch(big);
                            around.push(("Title".into(), (x0, y, iw, th), pub_text(lines, big, ink, true, TextAlign::Left)));
                            y += th;
                        }
                        if let Some(t) = &subtitle {
                            let lines = wrap(t, iw, sub_pt);
                            let sh = lines.lines.len() as f64 * pitch(sub_pt);
                            around.push(("Subtitle".into(), (x0, y, iw, sh), pub_text(lines, sub_pt, soft, false, TextAlign::Left)));
                            y += sh;
                        }
                        let ft = if y > m + pad { y + 4.0 * k } else { y };
                        let look = Look { head: ink, body: soft, small: soft, fill: clear, outline: clear, width: 0.0, radius: 0.0 };
                        let rule = footer(&mut around, &look, true, true) - 3.0 * k;
                        around.push((
                            "Footer Rule".into(),
                            (x0, rule, iw, 1.0),
                            ElementKind::Line { color: soft, width: 0.5, dir: LineDir::Horizontal, head: LineHead::None, dash: None },
                        ));
                        (page_w, page_h, (0.0, ft, page_w, room(ft, rule - 3.0 * k)?), None)
                    }
                    Style::Panel => {
                        // The map at the left, as tall as the sheet allows; a pale panel at the right with
                        // the title and subtitle, then the locator and the legend, and the sources and the
                        // reference at its foot; the note and authorship across the bottom.
                        let gap = 5.0 * k;
                        let pw = (0.3 * iw).max(48.0 * k).min(0.45 * iw);
                        let (mw, px) = (iw - pw - gap, x0 + iw - pw);
                        let top = m + pad;
                        // With its frame cut to the area, the sources and reference go under the map, in
                        // the room the frame left, and the panel holds the title, locator and legend.
                        let under = freed > 0.0;
                        let foot = footer(&mut around, &plain, true, under);
                        let panel_bottom = foot - 4.0 * k;
                        let tint = Rgba::new(accent.r(), accent.g(), accent.b(), 0x18);
                        behind.push(("Panel Box".into(), (px, top, pw, panel_bottom - top), pub_box(tint, clear, 0.0)));
                        let ip = 3.0 * k;
                        let (ix, tw) = (px + ip, pw - 2.0 * ip);
                        let mut y = top + ip;
                        if let Some(t) = &title {
                            let lines = wrap_as(t, tw, title_pt, true);
                            let th = lines.lines.len() as f64 * pitch(title_pt);
                            around.push(("Title".into(), (ix, y, tw, th), pub_text(lines, title_pt, ink, true, TextAlign::Left)));
                            y += th + 1.0 * k;
                        }
                        if let Some(t) = &subtitle {
                            let lines = wrap(t, tw, sub_pt);
                            let sh = lines.lines.len() as f64 * pitch(sub_pt);
                            around.push(("Subtitle".into(), (ix, y, tw, sh), pub_text(lines, sub_pt, soft, false, TextAlign::Left)));
                            y += sh;
                        }
                        if y > top + ip {
                            around.push((
                                "Panel Rule".into(),
                                (ix, y + 1.5 * k, tw, 1.0),
                                ElementKind::Line { color: accent, width: 1.0, dir: LineDir::Horizontal, head: LineHead::None, dash: None },
                            ));
                            y += 4.0 * k;
                        }
                        // The sources and the reference, stacked up from the panel's foot.
                        let mut yb = panel_bottom - ip;
                        for (name, head, lines) in sections(tw, head_pt, body_pt).into_iter().rev().filter(|_| !under) {
                            let (hh, bh) = (head.lines.len() as f64 * pitch(head_pt), lines.lines.len() as f64 * pitch(body_pt));
                            yb -= hh + bh;
                            around.push((
                                format!("{name} Heading"),
                                (ix, yb, tw, hh),
                                pub_text(head, head_pt, accent, true, TextAlign::Left),
                            ));
                            if !lines.lines.is_empty() {
                                around.push((
                                    name.to_string(),
                                    (ix, yb + hh, tw, bh),
                                    pub_text(lines, body_pt, soft, false, TextAlign::Left),
                                ));
                            }
                            yb -= 3.0 * k;
                        }
                        if yb - y < 30.0 * k {
                            return Err(
                            "the panel's texts leave no room for the locator and the legend: shorten the title, the sources or the reference"
                                .into(),
                        );
                        }
                        (page_w, page_h, (x0, top, mw, room(top, panel_bottom)?), Some(Column { x: ix, y, w: tw, h: yb - y }))
                    }
                    Style::Editorial => {
                        // A magazine's page: the title large in serif at the top left in the accent colour,
                        // the subtitle under it, the map below; a column at the right, under a rule, with
                        // its heading (the third of `headings`), the note (the map's story), the locator
                        // and the legend; the sources and reference boxed under both.
                        // Its title and subtitle are serif: with none installed, drawn in the sans, said.
                        if (title.is_some() || subtitle.is_some())
                            && let Some(w) = font_warning(FontFamily::Serif, None, false, (false, false))
                        {
                            warnings.push(format!("the editorial title and subtitle: {w}"));
                        }
                        let gap = 6.0 * k;
                        let cw = (0.3 * iw).max(45.0 * k).min(0.4 * iw);
                        let (mw, cx) = (iw - cw - gap, x0 + iw - cw);
                        let serif_wrap = |t: &str, w: f64, pt: f64, bold: bool| broken(t, w, pt, bold, TextAlign::Left, FontFamily::Serif);
                        let (big, sub) = (30.0 * k, 13.0 * k);
                        let mut y = m + pad;
                        if let Some(t) = &title {
                            let lines = serif_wrap(t, mw, big, true);
                            let th = lines.lines.len() as f64 * pitch(big);
                            around.push((
                                "Title".into(),
                                (x0, y, mw, th),
                                pub_text_in(lines, big, accent, true, TextAlign::Left, FontFamily::Serif),
                            ));
                            y += th + 1.0 * k;
                        }
                        if let Some(t) = &subtitle {
                            let lines = serif_wrap(t, mw, sub, false);
                            let sh = lines.lines.len() as f64 * pitch(sub);
                            around.push((
                                "Subtitle".into(),
                                (x0, y, mw, sh),
                                pub_text_in(lines, sub, soft, false, TextAlign::Left, FontFamily::Serif),
                            ));
                            y += sh;
                        }
                        let foot = footer(&mut around, &plain, false, true);
                        let fb = foot - 4.0 * k;
                        let mut cy = m + pad;
                        around.push((
                            "Column Rule".into(),
                            (cx, cy, cw, 1.0),
                            ElementKind::Line { color: accent, width: 1.5, dir: LineDir::Horizontal, head: LineHead::None, dash: None },
                        ));
                        cy += 3.0 * k;
                        if let Some(hd) = headings.get(2).filter(|h| !h.trim().is_empty()) {
                            let pt = head_pt * 1.2;
                            let lines = wrap_as(&capitals(&fill(hd)), cw, pt, true);
                            let hh = lines.lines.len() as f64 * pitch(pt);
                            around.push(("Column Heading".into(), (cx, cy, cw, hh), pub_text(lines, pt, accent, true, TextAlign::Left)));
                            cy += hh + 2.0 * k;
                        }
                        if let Some(t) = &note {
                            let pt = body_pt * 1.15;
                            let lines = wrap(t, cw, pt);
                            let nh = lines.lines.len() as f64 * pitch(pt);
                            around.push(("Note".into(), (cx, cy, cw, nh), pub_text(lines, pt, soft, false, TextAlign::Left)));
                            cy += nh + 4.0 * k;
                        }
                        if fb - cy < 30.0 * k {
                            warnings
                                .push("the column's note leaves the locator and the legend little room under it: shorten the note".into());
                        }
                        let ft = y + 4.0 * k;
                        (page_w, page_h, (x0, ft, mw, room(ft, fb)?), Some(Column { x: cx, y: cy, w: cw, h: (fb - cy).max(0.0) }))
                    }
                }
            } else {
                if style != Style::Classic {
                    warnings.push(format!(
                        "style \"{}\" arranges a sheet's texts, which a figure leaves to its caption: left out",
                        style.code()
                    ));
                }
                // A figure: the map edge to edge, a line of the projection and datum under it.
                // Every text a figure leaves to its caption, given anyway, is said to be left out.
                let asked = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
                for (field, given) in [
                    ("title", title.is_some()),
                    ("subtitle", subtitle.is_some()),
                    ("sources", !sources.is_empty()),
                    ("note", note.is_some()),
                    // An empty author asks for none: nothing left out.
                    ("author", asked("author") && author.is_some()),
                    ("headings", !headings.is_empty()),
                ] {
                    if given {
                        warnings.push(format!("{field} goes in the figure's caption, not on it: left out"));
                    }
                }
                let line = reference.iter().map(|t| fill(t)).collect::<Vec<_>>().join(" · ");
                // A millimetre in from the paper's edges, which a journal may trim.
                let lines = wrap(&line, page_w - 2.0, FIGURE_PT);
                if lines.lines.len() > 1 {
                    warnings.push("the reference is wider than the figure: it wraps under the map".into());
                }
                let note_h = lines.lines.len() as f64 * pitch(FIGURE_PT) + 1.0;
                let fh = if page_h > 0.0 {
                    page_h - note_h
                } else {
                    let shape = aspect(s, &extent).unwrap_or_else(|| {
                        warnings.push("the extent's shape on the map's projection isn't known: the figure is 4 wide by 3 tall".into());
                        0.75
                    });
                    (page_w * shape).clamp(0.45 * page_w, 1.3 * page_w).round()
                };
                if fh < 40.0 {
                    return Err("the reference leaves the map no room: shorten it".into());
                }
                let h = if page_h > 0.0 { page_h } else { (fh + note_h).ceil() };
                around.push((
                    "Reference".into(),
                    (1.0, fh + 1.0, page_w - 2.0, note_h - 1.0),
                    pub_text(lines, FIGURE_PT, ink, false, TextAlign::Left),
                ));
                (page_w, h, (0.0, 0.0, page_w, fh), None)
            })
        })();
        let laid = match attempt {
            Ok(laid) => laid,
            Err(e) if passes == 0 || settled => return Err(e),
            Err(_) => {
                (freed, settled) = (held, true);
                continue;
            }
        };
        let (_, _, (_, _, lw, lh), _) = laid;
        // The frame's height for the area at its width, with a seventh more of its surroundings;
        // worked out on the first pass (the width doesn't change), aimed at on the others, whose
        // footer can grow (a panel's sources move under the map) and so need a pass or two more.
        if passes == 0 {
            target = (sheet && matches!(style, Style::Panel | Style::Editorial))
                .then(|| aspect(s, &extent))
                .flatten()
                .map(|a| (((lw - 2.0 * room_side).max(1.0) * a + 2.0 * room_end) * 1.15).max(40.0 * k))
                // Only a frame clearly too tall is cut.
                .filter(|n| *n < 0.97 * lh);
        }
        passes += 1;
        match target {
            Some(n) if !settled && (lh - n).abs() > 0.5 && passes < 5 => (held, freed) = (freed, (freed + lh - n).max(0.0)),
            _ => break laid,
        }
    };
    // The content in the middle of the page: what the sheet laid out on its shorter page moves down
    // by half of what it freed (what spans the whole page was laid on it, and stays).
    for (_, r, _) in around.iter_mut().chain(behind.iter_mut()) {
        r.1 += freed / 2.0;
    }
    let (fy, column, h) = if freed > 0.0 && sheet {
        (fy + freed / 2.0, column.map(|c| Column { y: c.y + freed / 2.0, ..c }), page_full)
    } else {
        (fy, column, h)
    };
    // The extent fits within the grid's label strips (drawn inside the frame along its labelled
    // edges), on both sides so it stays centred, and its labels cover none of it.
    let (strip_w, strip_h) = (2.0 * room_side, 2.0 * room_end);
    if fw - strip_w < fw / 2.0 || fh - strip_h < fh / 2.0 {
        warnings.push("the frame is too small to keep its grid's labels off the map's edges: they may cover them".into());
    }
    let (inner_w, inner_h) = ((fw - strip_w).max(fw / 2.0), (fh - strip_h).max(fh / 2.0));
    let (center, scale) = if whole_globe {
        // The globe seen whole fills the frame, centred where the view looks (#2337): fitting the
        // whole Earth's flat footprint instead drew it a fraction of the frame, around 0°, 0°.
        // Its size at a scale is the same at any latitude, so it is worked out on the equator,
        // where the probe stays within the camera's resolutions.
        let view = s.map().center_lonlat();
        let scale = round_scale(globe_scale(s.map(), Coord::new(view.x, 0.0), inner_w.min(inner_h) * WHOLE_GLOBE_FILL));
        let center = drawable_globe_center(s.map(), view, scale);
        if center != view {
            warnings.push(format!(
                "the globe is centred at {:.1}° rather than the view's {:.1}° of latitude: nearer the pole, the frame can't draw it small enough to fit",
                center.y, view.y
            ));
        }
        (center, scale)
    } else {
        fit_scale(s.map(), &extent, inner_w, inner_h).ok_or("the extent can't be drawn on the map's projection")?
    };
    let scale = round_scale(scale);
    // A name given past the limit is refused, as layout.new refuses it, not cut (#2639).
    let given = crate::cmd::within("name", "a layout's name", ps(p, "name").unwrap_or("Publication").trim(), crate::cmd::MAX_NAME)?;
    let (name, name_cut) = unique_name(s, "New Publication Layout", "the name given", &given);
    let mut l = Layout::new(name.clone(), w, h);
    (l.decimal_comma, l.paper) = (comma, page_colour);
    let (over, under): (Vec<_>, Vec<_>) = page_wide.into_iter().partition(|e| e.3);
    for (n, r, kind, _) in under {
        l.add(&n, r, kind);
    }
    for (n, r, kind) in behind {
        l.add(&n, r, kind);
    }
    let frame = l.add(
        "Map Frame",
        (fx, fy, fw, fh),
        ElementKind::MapFrame {
            map: s.map().name.clone(),
            center,
            scale,
            shape: FrameShape::Rectangle,
            grid: grid.clone(),
            extent_of: None,
            extent_color: rustgis_map::layout::INDICATOR_COLOR,
            extent_width: rustgis_map::layout::INDICATOR_WIDTH,
            border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
            // A map that runs to the page's edges has no border.
            border_width: if edge { 0.0 } else { rustgis_map::layout::FRAME_BORDER_WIDTH },
            bearing: 0.0,
            pitch: 0.0,
        },
    );
    // Over the map, clear of the grid's labels (drawn inside the frame along its labelled
    // edges): the north arrow, the legend sized to what it lists, the scale bar with the numeric
    // scale under it, and the locator, each on a pale box. Each takes the corner where it covers
    // the least of what the map draws; by default (and when it makes no difference) the locator at
    // the top left, the arrow at the top right, the legend at the bottom left and the scale at the
    // bottom right.
    let paper = Rgba::new(0xFF, 0xFF, 0xFF, 225);
    let (e, small_pt) = if sheet { (4.0 * k, 6.5 * k) } else { (2.0, 6.0) };
    let room = |labelled: bool, side: bool| {
        e + if !labelled {
            0.0
        } else if side {
            room_side
        } else {
            room_end
        }
    };
    let (left, right, top, bottom) =
        (room(labelled.left, true), room(labelled.right, true), room(labelled.top, false), room(labelled.bottom, false));
    let (aw, ah) = if sheet { (8.0 * k, 12.0 * k) } else { (5.0, 7.5) };
    // The locator: a globe (a copy of the map's polygon layers on the globe, its diameter nearly
    // the frame's) or the surrounding region on the map itself, the area outlined on either by
    // the map frame's extent indicator.
    let side = match column {
        // In a column, its width up to an A4 portrait column's (a landscape sheet's wider
        // column doesn't make it larger), leaving the legend most of the height under it.
        Some(c) => c.w.min(52.0 * k).min(0.45 * c.h).max(0.0),
        None if sheet => (0.24 * fw.min(fh)).clamp(25.0 * k, 70.0 * k),
        None => (0.3 * fw.min(fh)).clamp(18.0, 45.0),
    };
    let mut locator_cut = None;
    let locator_map = match locator {
        Locator::None => None,
        Locator::Globe => {
            let src = s.map();
            let base = format!("{} Locator", src.name);
            // A locator an earlier sheet made that no layout shows any more (its layout deleted)
            // is replaced, so sheets made again and again don't leave a trail of them; one still
            // shown is kept, and the new one numbered beside it.
            let orphan = locator_orphan(s);
            let name = match orphan.and_then(|i| s.project.maps.get(i)) {
                Some(x) => x.name.clone(),
                None => {
                    let name = numbered(&base, |n| s.project.maps.iter().any(|x| x.name.eq_ignore_ascii_case(n)));
                    locator_cut = crate::cmd::name_cut("New Publication Layout", "the locator map's name", &base, &name);
                    name
                }
            };
            // A new map of what the globe draws, not a clone of the whole map (every layer's
            // features): its basemap (the land and borders around the area), graticule and body,
            // and the map's own polygon layers that show, unlabelled: the land at a glance, no
            // raster or streamed layer (read again) nor points and lines cluttering a small disc.
            // Not its web tiles (fetched and drawn over the basemap on the disc), swipe, time
            // slider, layer groups (their hidden layers left out here), bookmarks or animation.
            let mut m = rustgis_map::Map::new(name);
            (m.basemap, m.paper) = (src.basemap, src.paper);
            m.graticule = src.graticule;
            m.body = src.body.clone();
            m.set_projection(rustgis_map::Projection::Globe);
            // Sized on the equator and centred as near the area as the camera draws a globe that
            // small, as the whole globe's frame is (#2337): at the area's own latitude near a pole
            // the camera stops at its coarsest resolution, and the disc came out larger than its
            // circle, clipped. Further from the pole, the area is still on the disc and outlined.
            let globe = globe_scale(&m, Coord::new(center.x, 0.0), side * 0.92);
            let (at, whole) = globe_locator_center(&m, center, globe, side);
            if at != center {
                warnings.push(format!(
                    "the globe locator is centred at {:.1}° rather than the map frame's {:.1}° of latitude: nearer the pole, it can't draw the globe small enough to fit",
                    at.y, center.y
                ));
            }
            if !whole {
                warnings.push("the globe locator is too small to show the whole globe: its circle clips it".into());
            }
            // Each polygon simplified to a tenth of a millimetre on the disc (its scale in metres),
            // so the project keeps a sketch of the layers for it, not a second full copy of them.
            let tolerance_m = globe * 1e-4;
            // What Simplify left alone or changed beyond simplifying in what the sketch keeps (a
            // latitude past a pole taken as the pole, a polygon left as it was): said in the
            // warnings, one per kind (#1811).
            let notes = std::cell::Cell::new(rustgis_analysis::SimplifyNotes::default());
            // On the locator, sub-tolerance rings keep their outline rather than becoming triangles.
            let sketch = |g: &rustgis_data::Geometry| {
                let (mut simple, mut said) = rustgis_analysis::simplify_geometry(g, tolerance_m);
                if let (rustgis_data::Geometry::Polygon(original), rustgis_data::Geometry::Polygon(reduced)) = (g, &mut simple) {
                    for (original, reduced) in original.iter().flatten().zip(reduced.iter_mut().flatten()) {
                        let mut extent = Extent::EMPTY;
                        // Keep the extent continuous across the antimeridian.
                        let mut longitude = original.iter().find(|c| c.is_finite()).map_or(0.0, |c| c.x);
                        for &c in original.iter().filter(|c| c.is_finite()) {
                            longitude += (c.x - longitude + 180.0).rem_euclid(360.0) - 180.0;
                            extent.expand(Coord::new(longitude, c.y));
                        }
                        let projected = rustgis_geom::extent_to_merc(&extent);
                        let size_m =
                            projected.width().hypot(projected.height()) * extent.center().y.clamp(-85.0, 85.0).to_radians().cos().max(0.01);
                        if size_m < tolerance_m {
                            // The ring as it was: its vertices past a pole stay where they were.
                            said.past_pole = said.past_pole.saturating_sub(rustgis_analysis::past_pole(original.iter()));
                            *reduced = original.clone();
                        }
                    }
                }
                match &simple {
                    rustgis_data::Geometry::Polygon(polys) if polys.iter().flatten().all(|ring| ring.len() >= 4) => {
                        let mut all = notes.get();
                        all.add(&said);
                        notes.set(all);
                        simple
                    }
                    // The polygon as it was: nothing Simplify did is kept, but one it left as it was
                    // (a missing coordinate) is still said.
                    _ => {
                        let mut all = notes.get();
                        all.add(&rustgis_analysis::SimplifyNotes { broken: said.broken, capped: said.capped, ..Default::default() });
                        notes.set(all);
                        g.clone()
                    }
                }
            };
            m.layers = src
                .layers
                .iter()
                .filter(|l| {
                    src.shows(l) && !l.is_raster() && l.source.is_none() && l.geometry_type() == rustgis_data::GeometryType::Polygon
                })
                .map(|l| {
                    // (Sharing the rows: `Features` is copy-on-write, replaced just below.)
                    let mut layer = l.clone();
                    layer.renew_instance_id();
                    // Simplified sketches aren't the source's rows: saving them would overwrite it.
                    layer.gpkg = None;
                    layer.postgis = None;
                    layer.synced = None;
                    // Sketched in longitude/latitude, whatever system the layer holds its
                    // coordinates in. A feature with a place that can't be given there isn't
                    // drawn, which the map says of the layer it draws ([`Layer::unplaced`]).
                    let (lonlat, _) = l.dataset.in_lonlat_leaving_out()?;
                    layer.dataset.held_in = rustgis_data::raster::Crs::LonLat;
                    layer.dataset.features = lonlat
                        .features
                        .iter()
                        .map(|f| {
                            let mut f = f.clone();
                            f.geometry = f.geometry.as_ref().map(sketch);
                            f
                        })
                        .collect();
                    layer.touch_geometry();
                    layer.selection.clear();
                    layer.labels.enabled = false;
                    Ok(layer)
                })
                .collect::<Result<_, String>>()?;
            // All but reaching beyond 85.05°, which any world map does (`without_polar`).
            warnings.extend(notes.get().without_polar().messages("the globe locator, simplifying its polygons"));
            let name = m.name.clone();
            match orphan.and_then(|i| s.project.maps.get_mut(i)) {
                Some(old) => {
                    *old = m;
                    warnings.push(format!("the map '{name}', which no layout showed, is replaced by this sheet's globe"));
                }
                None => s.project.maps.push(m),
            }
            Some((name, globe, FrameShape::Circle, at))
        }
        Locator::Inset => Some((s.map().name.clone(), round_scale(scale * fw / side * 4.0), FrameShape::Rectangle, center)),
    };
    // The legend lists what the frame draws: its map synced to the frame's view first, as an
    // export does, so a streamed layer has its features there. (This changes the session's map,
    // so it comes after the last refusal: nothing below fails.) While one is still reading, the
    // classes can't be known: the legend gets a fixed size, to fit by hand. Nothing drawn in the
    // frame, no legend.
    let render_scale = crate::render::layout::frame_render_scale(s.map(), center, scale);
    let view = crate::render::View {
        width: fw * rustgis_map::layout::PT_PER_MM,
        height: fh * rustgis_map::layout::PT_PER_MM,
        pixels_per_point: 1.0,
        camera: rustgis_map::layout::frame_camera(s.map(), center, scale, 0.0, 0.0),
        sky: false,
    };
    let lonlat = crate::render::view_lonlat(s.map(), &view);
    let own = crate::render::view_own(s.map(), &view);
    let reading = s.map_mut().sync_sources_in(&lonlat, own.as_ref(), render_scale, true, &rustgis_data::query::Env::default());
    // Context around the data (neighbouring borders, the states around in one colour), which the
    // placement already sets aside, is left out of the legend too, unless "contextInLegend" (#1379).
    // It is judged again at each other scale the search below draws the map at, as the placement
    // judges it there, and the legend takes the one at the scale settled on.
    let context_at = |s: &Session, center: Coord, scale: f64| {
        if context_in_legend { Vec::new() } else { legend_context(s.map(), center, scale, (fw, fh)) }
    };
    let mut context = context_at(s, center, scale);
    let mut sizes = crate::render::layout::legend_sizes_with_no_data_mm(
        s.map_mut(),
        render_scale,
        false,
        false,
        (false, legend_pt),
        (&context, &Default::default()),
    );
    let (mut cw, mut ch) = match sizes {
        Some(sizes) => sizes[0],
        None => {
            // A complete source that can't be read: the export says why; the legend is sized by hand.
            warnings.push("a layer's source couldn't be read through, so the legend has a fixed size; exporting says why".into());
            (fw * 0.3, fh * 0.25)
        }
    };
    let listed = s.map().layers.iter().any(|l| s.map().shows(l) && crate::render::layout::legend_lists(l));
    if reading && listed {
        (cw, ch) = (cw.max(fw * 0.3), ch.max(fh * 0.25));
        sizes = None;
        warnings.push("the legend's classes aren't known yet (a layer still reading): it has a fixed size to fit by hand".into());
    } else if crate::render::layout::tile_legends_pending(s.map(), render_scale, &[]) {
        // Not waited for: a slow legend server would hold the command up.
        (cw, ch) = (cw.max(fw * 0.3), ch.max(fh * 0.25));
        sizes = None;
        warnings.push("a WMS or WMTS layer's legend picture is still loading: the legend has a fixed size to fit by hand".into());
    }
    // A scale bar given a larger text than the sheet's gets the height for it.
    let text_growth = match &scale_kind {
        Some(ElementKind::ScaleBar { text_size, .. }) => (text_size / bar_pt).max(1.0),
        _ => 1.0,
    };
    let bar_h = if sheet { 8.0 * k } else { 6.0 };
    let (sh, nh) = (bar_h * text_growth, pitch(small_pt) + 0.5);
    // The scale bar's box at a scale: the size of its bar and labels and of the scale under them,
    // with the padding (#1379). The bar is the one a box 30 % of the frame's width (25 mm at least)
    // would draw, which sets its length and divisions. The box is the narrowest that draws that same
    // bar: the renderer gives a bar at most 85 % of its box, so the box is at least the bar's length
    // over 0.85.
    if let Some(ElementKind::ScaleBar { frame: of, .. }) = &mut scale_kind {
        *of = frame;
    }
    // Nothing, without a scale bar.
    let scale_box = |ground_per_pt: f64, scale: f64| {
        let Some(bar) = &scale_kind else { return (0.0, 0.0) };
        let text = ("{scale}", small_pt, false);
        (scale_box_width(ground_per_pt, scale, fw, bar, comma, text), sh + nh)
    };
    let ground_per_pt = |s: &Session, center: Coord, scale: f64| {
        s.map().ground_res_at(&rustgis_map::layout::frame_camera(s.map(), center, scale, 0.0, 0.0))
    };
    let pad = 2.0;
    // The surrounds to place: what each is and its size (the arrangement gives its corner).
    let mut surrounds: Vec<(Surround, f64, f64)> = Vec::new();
    if locator_map.is_some() && column.is_none() {
        surrounds.push((Surround::Locator, side, side));
    }
    // At a continental span the graticule's lines give north, and one arrow can't point north
    // across the whole frame: none, unless "northArrow": "always" (#1379). Left out so, it is said;
    // "northArrow": "none" asks for it, so isn't.
    let arrow = north_arrow.unwrap_or(extent_span_deg(&extent) < CONTINENTAL_DEG);
    if arrow {
        surrounds.push((Surround::Arrow, aw, ah));
    } else if north_arrow.is_none() {
        let north = if step.is_some() { "where the graticule gives north" } else { "where one arrow can't point north across the frame" };
        warnings
            .push(format!("no north arrow: the extent spans {CONTINENTAL_DEG:.0}° or more, {north}; \"northArrow\": \"always\" draws one"));
    }
    // The topmost visible raster with a colour scale that the legend doesn't list by its classes
    // gets a colorbar, under the legend in its box (a map whose theme is a raster on a ramp has its
    // key there, which the legend's rows don't give): an RGB composite over a DEM leaves it to the
    // DEM. A raster drawn by classes has them in the legend's rows instead (#1679). `"colorbar":
    // "none"` leaves it out, and its room in the box with it (#1680).
    // One on top whose pixels aren't read yet has no scale to show: no colorbar, rather than the
    // scale of a raster under it under its name.
    let top_raster = colorbar.then(|| rustgis_map::layout::colorbar_layer(s.map(), None)).flatten();
    let scaled = |l: &&rustgis_map::Layer| l.raster.as_ref().is_some_and(|r| rustgis_map::layout::color_scale(r).is_some());
    let bar = top_raster.filter(scaled).map(|l| l.name.clone());
    if let Some(l) = top_raster.filter(|l| !scaled(l)) {
        let why = if l.raster.as_ref().is_some_and(|r| r.error.is_some()) { "couldn't be read" } else { "is still reading" };
        warnings.push(format!(
            "{} {why}, so its colour scale isn't known and the layout has no colorbar for it; add one with layout.addElement once it has read",
            l.name
        ));
    }
    // A colour table whose values can't be known (too many to list) gives the legend no class, and
    // the sheet no key for it; classes from an overview may miss a rare one: said, not left out.
    warnings.extend(raster_key_notices(s.map(), &[], false));
    // A page of cards gives the locator in its column a card of the column's width, and the legend
    // under it one as wide.
    let card = style == Style::Modular && column.is_some();
    let (card_r, card_pad) = (3.0 * k, 2.5 * k);
    // The colorbar's title is the raster's name, over the bar. The box is as wide as the title, up
    // to the legend's cap (in a side column, the column's width); a longer title wraps there, each
    // line past the first making the colorbar taller (#2493).
    let title_room = match column {
        Some(c) => c.w + if card { 2.0 * card_pad } else { 0.0 } - 2.0 * pad,
        None => fw * 0.45 - 2.0 * pad,
    };
    let base_w = if sheet { 50.0 * k } else { 40.0 };
    let bar_w = base_w.max(bar.as_deref().map_or(0.0, crate::render::layout::colorbar_title_width_mm).min(title_room));
    let bar_measured = bar.as_deref().map_or(COLORBAR_MM, |title| colorbar_mm(title, bar_w.min(title_room)));
    let bar_gap = if ch > 0.0 { 2.0 } else { 0.0 };
    // Over the map the box is never placed taller than 60% of the frame, so the title's lines
    // count only up to that, less the rows of the legend beside them (a colorbar's own height, as
    // before, at the least): the map isn't moved, veiled or scaled for room the box can't have, and
    // each candidate's rows keep their height (the fit test and the placement below). The lines past
    // it are said as cut off.
    let over_map = column.is_none();
    let cap = |rows_h: f64| (fh * 0.6 - 2.0 * pad - rows_h - bar_gap).max(COLORBAR_MM);
    let bar_reserve = |rows_h: f64| if over_map { bar_measured.min(cap(rows_h)) } else { bar_measured };
    let bar_part = if bar.is_some() { bar_reserve(ch) + bar_gap } else { 0.0 };
    let (content_w, content_h) = (if bar.is_some() { cw.max(bar_w) } else { cw }, ch + bar_part);
    let legend_in_column = column.is_some();
    if content_h > 0.0 && !legend_in_column {
        surrounds.push((Surround::Legend, (content_w + 2.0 * pad).min(fw * 0.45), (content_h + 2.0 * pad).min(fh * 0.6)));
    }
    // Its width is set once the frame's centre and scale are settled, below; nothing reads it before.
    // A globe seen whole has none: its scale holds at its centre only, falling to nothing at its
    // rim, so a bar would read true nowhere else (#2337).
    if scale_kind.is_some() {
        if whole_globe {
            warnings.push("no scale bar: the map shows the whole globe, whose scale holds at its centre only".into());
        } else {
            surrounds.push((Surround::Scale, 0.0, sh + nh));
        }
    }
    // A corner's rectangle for a surround `w` × `h`.
    let at = |corner: usize, w: f64, h: f64| {
        let x = if corner.is_multiple_of(2) { fx + left } else { fx + fw - right - w };
        let y = if corner < 2 { fy + top } else { fy + fh - bottom - h };
        (x, y, w, h)
    };
    // What each surround is called in the answer and in warnings.
    let named = |kind: Surround| match kind {
        Surround::Locator => "locator",
        Surround::Arrow => "north arrow",
        Surround::Legend => "legend",
        Surround::Scale => "scale bar",
    };
    let area = (fx + left, fy + top, fx + fw - right, fy + fh - bottom);
    // What the map draws where, on a grid of the frame (its basemap left out). The map
    // itself is drawn for it, on purpose, as the screen draws it: a grid of 384 cells, a small part
    // of what exporting the sheet draws; a streamed layer draws what it holds and starts reading
    // the frame's area in the background (which the export reads anyway); web tiles aren't fetched.
    let coverage = crate::render::layout::frame_coverage(s.map_mut(), center, scale, fw, fh, 384);
    // The legend's whole box, around its rows and the colorbar under them.
    // A candidate's colorbar reservation follows its own rows, which its column count sets.
    let whole = |(w, h): (f64, f64)| {
        let part = if bar.is_some() { bar_reserve(h) + bar_gap } else { 0.0 };
        ((if bar.is_some() { w.max(bar_w) } else { w }) + 2.0 * pad, h + part + 2.0 * pad)
    };
    // The largest rectangle a coverage leaves free inside the graticule's margins, in mm.
    let free_room = |(gw, gh, cells): &(u32, u32, Vec<bool>)| {
        let (gw, gh) = (*gw as usize, *gh as usize);
        let col = |v: f64| ((v - fx) / fw * gw as f64).clamp(0.0, gw as f64) as usize;
        let row = |v: f64| ((v - fy) / fh * gh as f64).clamp(0.0, gh as f64) as usize;
        let (cw, ch) = largest_free_cells(gw, cells, (col(area.0), row(area.1), col(area.2), row(area.3)));
        (cw as f64 * fw / gw as f64, ch as f64 * fh / gh as f64)
    };
    // When no shape of the legend fits the room the map leaves, the map moves towards an edge, so
    // the room on both its sides joins, by part or all of the way the asked extent can go and stay
    // in the frame: of the moves that make room, the one keeping it nearest the page's visual
    // centre. Failing
    // that, on a continuous map (drawn over most of the extent and running on past it) with a long
    // legend (a short one sits over the map as well as ever), what is outside the extent is veiled,
    // and counts as room; and failing that, the map is drawn at up to half the scale, for a band of
    // it. Failing all those, it stays as it was (#1379).
    let (mut center, mut scale, mut coverage, mut map_shift) = (center, scale, coverage, (0.0, 0.0));
    let mut veiled: Option<Rect> = None;
    // The bands of the veil around an extent's place on the page, the widest first, short of the
    // extent by the gap the surrounds keep in a group.
    const GROUP_GAP: f64 = 3.0;
    let bands_around = |clip: Rect| {
        let (clip_right, clip_below) = (clip.0 + clip.2 + GROUP_GAP, clip.1 + clip.3 + GROUP_GAP);
        let mut bands = [
            (area.0, area.1, clip.0 - GROUP_GAP - area.0, area.3 - area.1),
            (clip_right, area.1, area.2 - clip_right, area.3 - area.1),
            (area.0, area.1, area.2 - area.0, clip.1 - GROUP_GAP - area.1),
            (area.0, clip_below, area.2 - area.0, area.3 - clip_below),
        ];
        bands.sort_by(|a, b| (b.2 * b.3).total_cmp(&(a.2 * a.3)));
        bands
    };
    let size_of = |kind: Surround| surrounds.iter().find(|x| x.0 == kind).map(|x| (x.1, x.2));
    let (locator_side, arrow_size) = (size_of(Surround::Locator).map(|x| x.0), size_of(Surround::Arrow));
    // The fewest columns from `from` up in which the surrounds group in a band around `clip`, with
    // their places; the scale bar's box `scale_bar` is the one at the scale tried.
    let grouped = |clip: Rect, from: usize, known: &[(f64, f64); MAX_LEGEND_COLUMNS], scale_bar: (f64, f64)| {
        (from..known.len()).find_map(|i| {
            bands_around(clip)
                .iter()
                .find_map(|band| veil_panel(*band, whole(known[i]), locator_side, arrow_size, scale_bar, GROUP_GAP))
                .map(|places| (i, places))
        })
    };
    if let Some(known) = sizes.filter(|_| column.is_none() && content_h > 0.0) {
        let need = |room: (f64, f64)| known.iter().position(|size| whole(*size).0 <= room.0 && whole(*size).1 <= room.1);
        let flat = s.map().flat();
        let asked = projected_extent(s, &extent);
        let at = flat.forward_checked(center).filter(|_| !asked.is_empty());
        if let Some(at) = at.filter(|_| coverage.as_ref().is_some_and(|c| need(free_room(c)).is_none())) {
            let (centred, asked_scale) = (center, scale);
            let continuous = coverage.as_ref().is_some_and(|(gw, gh, cells)| {
                let mm = 1000.0 * flat.ground_per_unit(asked.center()) * s.map().body().ratio() / asked_scale;
                let (half_w, half_h) = (asked.width() * mm / 2.0, asked.height() * mm / 2.0);
                let (gw, gh) = (*gw as usize, *gh as usize);
                let (mut inside, mut inside_drawn, mut outside, mut outside_drawn) = (0usize, 0usize, 0usize, 0usize);
                for (i, drawn) in cells.iter().enumerate() {
                    let x = ((i % gw) as f64 + 0.5) / gw as f64 * fw - fw / 2.0;
                    let y = ((i / gw) as f64 + 0.5) / gh as f64 * fh - fh / 2.0;
                    if x.abs() <= half_w && y.abs() <= half_h {
                        inside += 1;
                        inside_drawn += usize::from(*drawn);
                    } else {
                        outside += 1;
                        outside_drawn += usize::from(*drawn);
                    }
                }
                whole(known[0]).1 > fh * 0.25
                    && inside > 0
                    && inside_drawn * 5 >= inside * 4
                    && (outside == 0 || outside_drawn * 10 >= outside * 3)
            });
            // `"veil": false` keeps the map unveiled, and so at its scale.
            let continuous = continuous && may_veil;
            let passes = [(1.0, false), (1.0, true), (1.25, true), (1.5, true), (2.0, true)];
            // Each scale's frame is drawn once, wide enough for every move, and each move's frame
            // cut from it: a move is a translation on the page. So the search draws the map once per
            // scale it tries (at most four times), not once per place. Its legend is measured at
            // that scale, once, and those sizes are the ones laid out.
            type Drawn = (f64, Option<(u32, u32, Vec<bool>)>, Option<[(f64, f64); MAX_LEGEND_COLUMNS]>, Vec<String>);
            let mut drawn: Option<Drawn> = None;
            let (mut chosen, mut chosen_context) = (known, context.clone());
            let mut unknown_at: Option<f64> = None;
            // Moved frames drawn on their own (below), by scale and move: a pass with the veil
            // tries the same moves as the one before it.
            // With the legend's context judged there, and its sizes with that context.
            type Own = ((f64, f64, f64), (u32, u32, Vec<bool>), Vec<String>, [(f64, f64); MAX_LEGEND_COLUMNS]);
            let mut own_drawn: Vec<Own> = Vec::new();
            // A figure keeps its scale: its size is fixed, and a smaller scale costs it the most.
            for (factor, veil) in passes.into_iter().filter(|(factor, veil)| (!*veil || continuous) && (*factor == 1.0 || sheet)) {
                let tried = if factor == 1.0 { asked_scale } else { round_scale(asked_scale * factor) };
                let mm = 1000.0 * flat.ground_per_unit(asked.center()) * s.map().body().ratio() / tried;
                // How far the extent, centred, can go each way and stay inside that side's margin
                // (a figure labels only its left and bottom edges, so they differ).
                let (half_w, half_h) = (asked.width() * mm / 2.0, asked.height() * mm / 2.0);
                let to = |margin: f64, half_frame: f64, half_extent: f64| (half_frame - margin - half_extent).max(0.0);
                let (to_left, to_right) = (to(left, fw / 2.0, half_w), to(right, fw / 2.0, half_w));
                let (to_top, to_bottom) = (to(top, fh / 2.0, half_h), to(bottom, fh / 2.0, half_h));
                // The wide drawing is centred as the frame is, so it reaches the farther way on both.
                let (slack_x, slack_y) = (to_left.max(to_right), to_top.max(to_bottom));
                let wide = (fw + 2.0 * slack_x, fh + 2.0 * slack_y);
                if drawn.as_ref().is_none_or(|d| d.0 != tried) {
                    // A streamed layer has its features read for the wide view, at the asked scale
                    // too (the frame's read reaches past the frame by half its size in degrees,
                    // which the wide view outgrows where the projection stretches, as Web Mercator
                    // does far from the equator), and a smaller scale can draw more classes. While
                    // one is still reading there, or a legend picture still loading, what the map
                    // draws there isn't known: that scale isn't tried, which is said.
                    let render_scale = crate::render::layout::frame_render_scale(s.map(), centred, tried);
                    let view = crate::render::View {
                        width: wide.0 * rustgis_map::layout::PT_PER_MM,
                        height: wide.1 * rustgis_map::layout::PT_PER_MM,
                        pixels_per_point: 1.0,
                        camera: rustgis_map::layout::frame_camera(s.map(), centred, tried, 0.0, 0.0),
                        sky: false,
                    };
                    let lonlat = crate::render::view_lonlat(s.map(), &view);
                    let own = crate::render::view_own(s.map(), &view);
                    let reading =
                        s.map_mut().sync_sources_in(&lonlat, own.as_ref(), render_scale, true, &rustgis_data::query::Env::default());
                    let here_context = if tried == asked_scale { context.clone() } else { context_at(s, centred, tried) };
                    let here =
                        if reading || (tried != asked_scale && crate::render::layout::tile_legends_pending(s.map(), render_scale, &[])) {
                            unknown_at.get_or_insert(tried);
                            None
                        } else if tried == asked_scale {
                            Some(known)
                        } else {
                            // A legend that can't be measured there (a source that can't be read
                            // through) leaves that scale untried too, which is said.
                            let measured = crate::render::layout::legend_sizes_with_no_data_mm(
                                s.map_mut(),
                                render_scale,
                                false,
                                false,
                                (false, legend_pt),
                                (&here_context, &Default::default()),
                            );
                            if measured.is_none() {
                                unknown_at.get_or_insert(tried);
                            }
                            measured
                        };
                    let cells = (384.0 * wide.0.max(wide.1) / fw.max(fh)).round() as u32;
                    // What is context is judged on the frame asked for, not the wide drawing, so
                    // every move's cut leaves out the same layers.
                    let whole_frame = here
                        .and_then(|_| crate::render::layout::frame_coverage_judged_on(s.map_mut(), centred, tried, wide, (fw, fh), cells));
                    drawn = Some((tried, whole_frame, here, here_context));
                }
                let Some((_, Some(whole_frame), Some(here), here_context)) = &drawn else { continue };
                let need = |sizes: &[(f64, f64); MAX_LEGEND_COLUMNS], room: (f64, f64)| {
                    sizes.iter().position(|size| whole(*size).0 <= room.0 && whole(*size).1 <= room.1)
                };
                // Of the places with the fewest columns, the one leaving the extent nearest the
                // page's visual centre (a little above its middle) is kept, a move away from that
                // centre as short as makes room, one towards it as far as stays nearest it; within a
                // millimetre of the nearest, the one with the most room (#1379).
                // Each place that fits: its columns, its distance from that centre, its room, and the
                // map's centre, coverage, move, veil and legend there.
                type Fit = (
                    usize,
                    f64,
                    f64,
                    Coord,
                    (u32, u32, Vec<bool>),
                    (f64, f64),
                    Option<Rect>,
                    [(f64, f64); MAX_LEGEND_COLUMNS],
                    Vec<String>,
                );
                let mut fits: Vec<Fit> = Vec::new();
                // Each way a quarter, half, three quarters and all of the way the extent can go, so
                // a move can stop short of the edge. A way with no room to go is left out (its moves
                // are the centred place, tried first).
                let moves = [(to_right, 0.0), (-to_left, 0.0), (0.0, to_bottom), (0.0, -to_top)]
                    .into_iter()
                    .filter(|&(x, y)| x != 0.0 || y != 0.0)
                    .flat_map(|(x, y)| [0.25, 0.5, 0.75, 1.0].map(|f| (x * f, y * f)));
                for (dx, dy) in std::iter::once((0.0, 0.0)).chain(moves) {
                    // Centred with no veil at the asked scale is what has no room; a shift of nothing
                    // is centred.
                    let still = dx == 0.0 && dy == 0.0;
                    if (still && !veil) || (!still && dx.abs() + dy.abs() < 1.0) {
                        continue;
                    }
                    let (cx, cy) = (at.x - dx / mm, at.y + dy / mm);
                    let there = if still { Some(centred) } else { flat.inverse(Coord::new(cx, cy)) };
                    let Some(there) = there else { continue };
                    // A frame is drawn at its own centre's ground scale, which a move changes where
                    // the projection stretches (Web Mercator's, with the latitude; a conic's, a
                    // little): the extent is placed on the page at the moved centre's scale, and a
                    // move that leaves it out of its margins there isn't made. The frame moved east
                    // by `dx` and south by `dy` is the wide one's part that far west and north of
                    // its middle while that scale moves a corner of it one cell of the coverage grid
                    // at most, which the cut is exact to anyway; past that, it is drawn on its own
                    // (what is context then judged on itself, for the legend too), once.
                    let here_mm = mm * flat.ground_per_unit(Coord::new(cx, cy)) / flat.ground_per_unit(asked.center());
                    let clip = (
                        fx + fw / 2.0 + (asked.min_x - cx) * here_mm,
                        fy + fh / 2.0 - (asked.max_y - cy) * here_mm,
                        asked.width() * here_mm,
                        asked.height() * here_mm,
                    );
                    let kept_whole = clip.0 >= fx + left - 0.5
                        && clip.1 >= fy + top - 0.5
                        && clip.0 + clip.2 <= fx + fw - right + 0.5
                        && clip.1 + clip.3 <= fy + fh - bottom + 0.5;
                    if !still && !kept_whole {
                        continue;
                    }
                    let drift = (1.0 - flat.ground_per_unit(Coord::new(cx, cy)) / flat.ground_per_unit(at)).abs() * fw.hypot(fh) / 2.0;
                    let (mut moved, legend_sizes, legend_context) = if drift <= fw.max(fh) / 384.0 {
                        (crop_cells(whole_frame, wide, (slack_x - dx, slack_y - dy), (fw, fh)), *here, here_context.clone())
                    } else if let Some((_, own, own_context, own_sizes)) = own_drawn.iter().find(|o| o.0 == (tried, dx, dy)) {
                        (own.clone(), *own_sizes, own_context.clone())
                    } else {
                        let Some(own) = crate::render::layout::frame_coverage(s.map_mut(), there, tried, fw, fh, 384) else { continue };
                        let own_context = context_at(s, there, tried);
                        let own_sizes = if own_context == *here_context {
                            *here
                        } else {
                            let at_there = crate::render::layout::frame_render_scale(s.map(), there, tried);
                            let measured = crate::render::layout::legend_sizes_with_no_data_mm(
                                s.map_mut(),
                                at_there,
                                false,
                                false,
                                (false, legend_pt),
                                (&own_context, &Default::default()),
                            );
                            // A legend that can't be measured there leaves that move untried, which is
                            // said, as at a scale.
                            let Some(measured) = measured else {
                                unknown_at.get_or_insert(tried);
                                continue;
                            };
                            measured
                        };
                        own_drawn.push(((tried, dx, dy), own.clone(), own_context.clone(), own_sizes));
                        (own, own_sizes, own_context)
                    };
                    if veil {
                        let (gw, gh) = (moved.0 as usize, moved.1 as usize);
                        for (i, cell) in moved.2.iter_mut().enumerate() {
                            let x = fx + ((i % gw) as f64 + 0.5) / gw as f64 * fw;
                            let y = fy + ((i / gw) as f64 + 0.5) / gh as f64 * fh;
                            *cell &= x >= clip.0 && x <= clip.0 + clip.2 && y >= clip.1 && y <= clip.1 + clip.3;
                        }
                    }
                    let room = free_room(&moved);
                    let Some(n) = need(&legend_sizes, room) else { continue };
                    // Under a veil the whole group has to fit, not the legend alone, the scale bar's box
                    // as drawn there.
                    let in_group = |s: &Session| grouped(clip, n, &legend_sizes, scale_box(ground_per_pt(s, there, tried), tried));
                    let Some(n) = (if veil { in_group(s).map(|(i, _)| i) } else { Some(n) }) else { continue };
                    let off = visual_centre_offset((clip.0 + clip.2 / 2.0, clip.1 + clip.3 / 2.0), (w, h));
                    fits.push((n, off, room.0 * room.1, there, moved, (dx, dy), veil.then_some(clip), legend_sizes, legend_context));
                }
                // The fewest columns; of those, the nearest that centre, and within a millimetre of
                // the nearest, the most room (the first tried on a tie).
                let Some(fewest) = fits.iter().map(|f| f.0).min() else { continue };
                let nearest = fits.iter().filter(|f| f.0 == fewest).map(|f| f.1).fold(f64::INFINITY, f64::min);
                let best = fits
                    .into_iter()
                    .filter(|f| f.0 == fewest && f.1 <= nearest + 1.0)
                    .reduce(|kept, f| if f.2 > kept.2 { f } else { kept });
                if let Some((_, _, _, there, moved, shift, clip, legend_sizes, legend_context)) = best {
                    (center, scale, coverage, map_shift) = (there, tried, Some(moved), shift);
                    veiled = clip;
                    (chosen, chosen_context) = (legend_sizes, legend_context);
                    break;
                }
            }
            if let Some(at_scale) = unknown_at {
                let or_veiled = if may_veil { " or veiled" } else { "" };
                warnings.push(format!(
                    "the map isn't moved{or_veiled} at 1:{at_scale:.0} for the legend's room: a layer there is still reading, a legend picture still loading, or a layer's source can't be read through (exporting says why)"
                ));
            }
            // A move changes what the frame shows past the extent, so it is said, as the veil and
            // the scale are.
            if map_shift != (0.0, 0.0) {
                let (dx, dy) = map_shift;
                let (by, way) =
                    if dx != 0.0 { (dx, if dx > 0.0 { "right" } else { "left" }) } else { (dy, if dy > 0.0 { "down" } else { "up" }) };
                warnings.push(format!(
                    "the map is moved {:.0} mm {way} in its frame (\"mapShift\"), the extent still whole, for the legend's room",
                    by.abs()
                ));
            }
            if scale != asked_scale {
                warnings.push(format!("the map is drawn at 1:{scale:.0}, not 1:{asked_scale:.0}, to leave the legend room"));
            }
            // The legend as measured where the map settled: at another scale, or moved to where
            // the context is judged apart.
            if scale != asked_scale || chosen_context != context {
                (sizes, context) = (Some(chosen), chosen_context);
                (cw, ch) = chosen[0];
                let (w, h) = whole(chosen[0]);
                if let Some(entry) = surrounds.iter_mut().find(|x| x.0 == Surround::Legend) {
                    (entry.1, entry.2) = (w.min(fw * 0.45), h.min(fh * 0.6));
                }
            }
        }
    }
    if !context.is_empty() {
        warnings.push(format!(
            "the legend leaves out {} context layer{} around the data ({}): \"contextInLegend\": true lists {}",
            context.len(),
            if context.len() == 1 { "" } else { "s" },
            context.join(", "),
            if context.len() == 1 { "it" } else { "them" }
        ));
    }
    // The scale bar's box as the frame is drawn, moved or at another scale or not.
    let bar_box = scale_box(ground_per_pt(s, center, scale), scale);
    if let Some(entry) = surrounds.iter_mut().find(|x| x.0 == Surround::Scale) {
        (entry.1, entry.2) = bar_box;
    }
    // Its cells summed from the top left (a summed-area table), so a rectangle's count is four
    // lookups, however large: the search off the corners tries thousands.
    let sums: Option<(usize, usize, Vec<u32>)> = coverage.as_ref().map(|(gw, gh, cells)| {
        let (gw, gh) = (*gw as usize, *gh as usize);
        let mut t = vec![0u32; (gw + 1) * (gh + 1)];
        for r in 0..gh {
            for c in 0..gw {
                let v = u32::from(cells.get(r * gw + c).copied().unwrap_or(false));
                t[(r + 1) * (gw + 1) + c + 1] = v + t[r * (gw + 1) + c + 1] + t[(r + 1) * (gw + 1) + c] - t[r * (gw + 1) + c];
            }
        }
        (gw, gh, t)
    });
    // The share of a rectangle (in page mm) over the frame's drawn cells.
    let covered = |(x, y, w, h): (f64, f64, f64, f64)| -> f64 {
        let Some((gw, gh, t)) = &sums else { return 0.0 };
        let (gw, gh) = (*gw, *gh);
        let col = |v: f64| ((v - fx) / fw * gw as f64).floor().clamp(0.0, gw as f64) as usize;
        let row = |v: f64| ((v - fy) / fh * gh as f64).floor().clamp(0.0, gh as f64) as usize;
        let (c0, r0) = (col(x).min(gw.saturating_sub(1)), row(y).min(gh.saturating_sub(1)));
        let (c1, r1) = (col(x + w).max(c0 + 1).min(gw), row(y + h).max(r0 + 1).min(gh));
        let n = (c1 - c0) * (r1 - r0);
        let at = |r: usize, c: usize| f64::from(t[r * (gw + 1) + c]);
        let drawn = at(r1, c1) - at(r0, c1) - at(r1, c0) + at(r0, c0);
        if n == 0 { 0.0 } else { drawn / n as f64 }
    };
    // An arrangement laid out (#1379), the legend's box `legend` at most: each surround over the map
    // in its own corner, the legend short of the surrounds beside and above or below it (`None` when
    // the locator leaves it too small, so it is left out); then, the largest first, each one covering
    // what the map draws there leaves it for the freest place in the frame, clear of those placed
    // before it ([`free_spot`]), when that covers less, and one whose corner a larger one took leaves
    // it in any case. Data everywhere alike leaves nowhere better: it stays. Gives the legend's corner
    // and size, each surround's place, the ones moved, and the area (mm²) of those moved and of what
    // still covers the drawing.
    type Laid = (Option<(usize, f64, f64)>, Vec<(Surround, Rect)>, Vec<&'static str>, (f64, f64));
    let lay_out = |homes: Homes, legend: (f64, f64)| -> Laid {
        let corner_of = |what: Surround| {
            surrounds.iter().find(|(kind, ..)| *kind == what).map(|&(kind, w, h)| {
                let (w, h) = if kind == Surround::Legend { legend } else { (w, h) };
                (homes[home_slot(kind)], w, h)
            })
        };
        let legend_at = corner_of(Surround::Legend).and_then(|(corner, w, h)| {
            let other = |c: usize| surrounds.iter().find(|&&(kind, ..)| homes[home_slot(kind)] == c).copied();
            let beside = other(corner ^ 1).map(|(kind, ow, _)| (kind, ow));
            let over = other(corner ^ 2).map(|(kind, _, oh)| (kind, oh));
            legend_room(w, h, fw - left - right - 2.0, fh - top - bottom - e, beside, over).map(|(lw, lh)| (corner, lw, lh))
        });
        let mut spots: Vec<(Surround, Rect)> = [Surround::Locator, Surround::Arrow, Surround::Legend, Surround::Scale]
            .into_iter()
            .filter_map(|kind| match kind {
                Surround::Legend => legend_at.map(|(c, lw, lh)| (kind, at(c, lw, lh))),
                _ => corner_of(kind).map(|(c, w, h)| (kind, at(c, w, h))),
            })
            .collect();
        // The largest first, the legend as a rule: it has the most to lose, so it takes the freest
        // place, and a smaller surround whose corner it took moves out of its way.
        let mut order: Vec<usize> = (0..spots.len()).collect();
        order.sort_by(|&a, &b| (spots[b].1.2 * spots[b].1.3).total_cmp(&(spots[a].1.2 * spots[a].1.3)));
        let mut moved: Vec<&'static str> = Vec::new();
        let mut settled: Vec<Rect> = Vec::new();
        for i in order {
            let (kind, home) = spots[i];
            let here = covered(home);
            let overlaps = |r: Rect| settled.iter().any(|o| r.0 < o.0 + o.2 && o.0 < r.0 + r.2 && r.1 < o.1 + o.3 && o.1 < r.1 + r.3);
            let taken = overlaps(home);
            if here > LEAK || taken {
                // Out of a corner taken by a larger one whatever it covers; off the data only when
                // that covers less.
                if let Some(r) = free_spot(home, area, &settled, covered).filter(|r| taken || covered(*r) < here - LEAK) {
                    spots[i].1 = r;
                    moved.push(named(kind));
                }
            }
            settled.push(spots[i].1);
        }
        // Named in the order the surrounds are listed, whatever order they were placed in.
        moved.sort_by_key(|m| spots.iter().position(|(kind, _)| named(*kind) == *m));
        let shifted: f64 = spots.iter().filter(|(kind, _)| moved.contains(&named(*kind))).map(|(_, r)| r.2 * r.3).sum();
        let over: f64 = spots.iter().map(|(_, r)| covered(*r) * r.2 * r.3).sum();
        // A legend left out costs the whole frame, more than any move, so an arrangement that keeps
        // it always wins over one that drops it.
        let dropped = if surrounds.iter().any(|x| x.0 == Surround::Legend) && legend_at.is_none() { fw * fh } else { 0.0 };
        // A legend cut short of its neighbours loses rows, which costs half the frame, more than any
        // move, and ten times what it loses, so an arrangement that keeps them wins, also by moving
        // the legend itself; a rounding error costs nothing.
        let lost = legend_at.map_or(0.0, |(_, lw, lh)| legend.0 * legend.1 - lw * lh);
        let cut = if lost > 1.0 { fw * fh / 2.0 + 10.0 * lost } else { 0.0 };
        // Surrounds left over each other (nowhere clear for one a larger one's corner took) cost ten
        // times their overlap, so an arrangement that keeps them apart wins.
        let clash = 10.0 * overlap_area(&spots.iter().map(|(_, r)| *r).collect::<Vec<_>>());
        (legend_at, spots, moved, (shifted + dropped + cut + clash, over))
    };
    // Each arrangement tried laid out, and one picked ([`pick_arrangement`]): the least area moved
    // off its corners (so a north arrow moved costs less than a legend), then the least left over
    // the drawing, the first tried (the favourite) on a tie. Every arrangement fills the same four
    // corners, so it is what goes where that differs. A single arrangement asked for is the one
    // laid out.
    let arranged = |legend: (f64, f64)| -> (Homes, Laid) {
        let mut laid: Vec<(Homes, Laid)> = tries.iter().map(|(_, h)| (*h, lay_out(*h, legend))).collect();
        let chosen = pick_arrangement(&laid.iter().map(|(_, l)| l.3).collect::<Vec<_>>()).map(|i| laid.swap_remove(i));
        chosen.unwrap_or_else(|| (ARRANGEMENTS[0].1, lay_out(ARRANGEMENTS[0].1, legend)))
    };
    let at_most_one = surrounds.iter().find(|x| x.0 == Surround::Legend).map_or((0.0, 0.0), |x| (x.1, x.2));
    let mut legend_cols = 1;
    let mut best = arranged(at_most_one);
    // The legend takes the fewest columns that fit the largest rectangle the map leaves free
    // (#1379). Past those, or when none fits, a long one is tried in more, up to the width beside
    // the north arrow, when in fewer it is cut or over what the map draws. One in a side column
    // stays in one: that column is about as wide as its rows.
    if let Some(sizes) = sizes.filter(|_| column.is_none() && content_h > 0.0) {
        let shown = |laid: &Laid, (w, h): (f64, f64)| laid.0.map_or(0.0, |(_, lw, lh)| (lw / w).min(1.0) * (lh / h).min(1.0));
        let long = whole(sizes[0]).1 > fh * 0.25;
        let mut score = (shown(&best.1, whole(sizes[0])), best.1.3.1);
        let wide = fw - left - right - 2.0 - arrow_size.map_or(0.0, |(w, _)| w);
        let free = coverage.as_ref().map(free_room);
        let fits = free.and_then(|(rw, rh)| sizes.iter().position(|size| whole(*size).0 <= rw && whole(*size).1 <= rh));
        // One column too, when the rectangle holds what the box a legend starts with cuts.
        if let Some(i) = fits.filter(|i| *i > 0 || whole(sizes[0]).0 > at_most_one.0 + 1e-6 || whole(sizes[0]).1 > at_most_one.1 + 1e-6) {
            let (w, h) = whole(sizes[i]);
            let more = arranged((w.min(wide), h));
            let new = (shown(&more.1, (w, h)), more.1.3.1);
            if more.1.0.is_some() && new.0 >= score.0 - 1e-6 {
                (legend_cols, best, score) = (i + 1, more, new);
            }
        }
        for (i, size) in sizes.iter().enumerate().skip(legend_cols) {
            if score.0 > 1.0 - 1e-6 && (!long || score.1 <= LEAK * fw * fh) {
                break;
            }
            let (w, h) = whole(*size);
            let more = arranged((w.min(wide), h.min(fh * 0.6)));
            let new = (shown(&more.1, (w, h)), more.1.3.1);
            if more.1.0.is_some() && more_columns_win(new, score) {
                (legend_cols, best, score) = (i + 1, more, new);
            }
        }
        (cw, ch) = sizes[legend_cols - 1];
    }
    let (homes, (_, mut spots, mut moved, _)) = best;
    // A veiled map has its surrounds grouped in the widest band of the veil that takes them, in the
    // fewest columns that do, rather than each in a corner of its own. The search found that group
    // with these very sizes, so it is found again; the legend goes there too when the corners left
    // it no room.
    let mut arrangement_used =
        tries.iter().find(|(_, h)| *h == homes).map_or_else(|| arrangement_name(homes).to_string(), |(n, _)| n.clone());
    if let (Some(clip), Some(known)) = (veiled, sizes)
        && let Some((i, (legend_place, locator_place, arrow_place, scale_place))) =
            grouped(clip, 0, &known, scale_box(ground_per_pt(s, center, scale), scale))
    {
        legend_cols = i + 1;
        (cw, ch) = known[i];
        for (kind, place) in [
            (Surround::Legend, Some(legend_place)),
            (Surround::Locator, locator_place),
            (Surround::Arrow, arrow_place),
            (Surround::Scale, Some(scale_place)),
        ] {
            let Some(place) = place.filter(|_| surrounds.iter().any(|x| x.0 == kind)) else { continue };
            match spots.iter_mut().find(|(k, _)| *k == kind) {
                Some(spot) => spot.1 = place,
                None => spots.push((kind, place)),
            }
        }
        // "moved" names each surround off its corner, the group's places judged as any other.
        moved = spots
            .iter()
            .filter(|(kind, r)| {
                let home = at(homes[home_slot(*kind)], r.2, r.3);
                (home.0 - r.0).abs() > 0.5 || (home.1 - r.1).abs() > 0.5
            })
            .map(|(kind, _)| named(*kind))
            .collect();
        arrangement_used = "veil".into();
    }
    if let Some(ElementKind::MapFrame { center: at, scale: drawn_at, .. }) = l.element_mut(frame).map(|e| &mut e.kind) {
        (*at, *drawn_at) = (center, scale);
    }
    // The veil over the frame and under its surrounds, in four bands around the extent, and the
    // extent's outline: it covers what the map draws there, which is said. It stops at the
    // graticule's labels, drawn in the frame's own picture under it, so they stay as dark as on an
    // unveiled map; the map under them stays unveiled, which is said too.
    if let Some((x, y, w, h)) = veiled {
        warnings.push(
            "what the map draws outside the extent is veiled to make room for the legend (except the strips under the graticule's labels), and the surrounds are grouped there: \"veil\": false leaves it unveiled"
                .into(),
        );
        let (x0, y0) = (x.max(area.0), y.max(area.1));
        let (x1, y1) = ((x + w).min(area.2), (y + h).min(area.3));
        let veil = || pub_card(Rgba::new(255, 255, 255, 150), Rgba::new(0, 0, 0, 0), 0.0, 0.0);
        for (name, band) in [
            ("Veil Above", (area.0, area.1, area.2 - area.0, y0 - area.1)),
            ("Veil Below", (area.0, y1, area.2 - area.0, area.3 - y1)),
            ("Veil Left", (area.0, y0, x0 - area.0, y1 - y0)),
            ("Veil Right", (x1, y0, area.2 - x1, y1 - y0)),
        ] {
            // A band thinner than half a millimetre (a rounding) is left out.
            if band.2 > 0.5 && band.3 > 0.5 {
                l.add(name, band, veil());
            }
        }
        l.add("Extent Outline", (x0, y0, x1 - x0, y1 - y0), pub_card(Rgba::new(0, 0, 0, 0), Rgba::rgb(0x2B, 0x26, 0x20), 0.8, 0.0));
    }
    if surrounds.iter().any(|x| x.0 == Surround::Legend) && !spots.iter().any(|(kind, _)| *kind == Surround::Legend) {
        warnings.push(
            "the frame leaves the legend no room beside the locator, so it is left out: give the map more room, or set \"locator\": \"none\""
                .into(),
        );
    }
    let mut placed: Vec<(&'static str, (f64, f64, f64, f64))> = Vec::new();
    let spot_of = |what: Surround| spots.iter().find(|(kind, _)| *kind == what).map(|(_, r)| *r);
    // In a column, at its top, centred across it; otherwise in its corner over the map.
    let locator_rect = match column {
        Some(c) => Some((c.x + (c.w - side) / 2.0, c.y, side, side)),
        None => spot_of(Surround::Locator).inspect(|&r| placed.push(("locator", r))),
    };
    let surround_box = |fill: Rgba| if style.cards() { pub_card(fill, clear, 0.0, 2.0 * k) } else { pub_box(fill, clear, 0.0) };
    if let (Some((map, locator_scale, shape, locator_center)), Some(r)) = (&locator_map, locator_rect) {
        if let (true, Some(c)) = (card, column) {
            l.add(
                "Locator Card",
                (c.x - card_pad, r.1 - card_pad, c.w + 2.0 * card_pad, r.3 + 2.0 * card_pad),
                pub_card(Rgba::rgb(0xFF, 0xFF, 0xFF), Rgba::new(0x2B, 0x26, 0x20, 60), 0.4, card_r),
            );
        }
        l.add(
            "Locator",
            r,
            ElementKind::MapFrame {
                map: map.clone(),
                center: *locator_center,
                scale: *locator_scale,
                shape: shape.clone(),
                grid: None,
                extent_of: Some(frame),
                extent_color: rustgis_map::layout::INDICATOR_COLOR,
                extent_width: rustgis_map::layout::INDICATOR_WIDTH,
                border_color: rustgis_map::layout::FRAME_BORDER_COLOR,
                border_width: rustgis_map::layout::FRAME_BORDER_WIDTH,
                bearing: 0.0,
                pitch: 0.0,
            },
        );
    }
    if let Some(r) = spot_of(Surround::Arrow) {
        placed.push(("north arrow", r));
        if let ElementKind::NorthArrow { frame: of, .. } = &mut arrow_kind {
            *of = frame;
        }
        l.add("North Arrow", r, arrow_kind);
    }
    // In a panel, the legend under the locator, as tall as it needs within what is left.
    let legend_rect = match column {
        Some(c) if content_h > 0.0 => {
            let ly = c.y + if locator_map.is_some() { side + 3.0 * k + if card { 2.0 * card_pad } else { 0.0 } } else { 0.0 };
            let room_h = c.y + c.h - ly;
            if room_h < MIN_LEGEND_MM {
                warnings.push("the side column leaves the legend no room under the locator, so it is left out: shorten its texts".into());
                None
            } else {
                // On a page of cards, a card as wide as the locator's.
                let (x, w) = if card { (c.x - card_pad, c.w + 2.0 * card_pad) } else { (c.x, c.w) };
                Some((x, ly, w, (content_h + 2.0 * pad).min(room_h)))
            }
        }
        Some(_) => None,
        None => spot_of(Surround::Legend).inspect(|&r| placed.push(("legend", r))),
    };
    // Over the map, where it shows nothing under the legend but its plain paper or water (no layer,
    // context, basemap shape or graticule line, and no veil), the legend needs no box to be read:
    // the box is kept, clear, unless "legendBox": "always" (#1379). A card is the style's look,
    // and stays.
    let legend_bare =
        legend_rect.filter(|_| column.is_none() && !style.cards() && veiled.is_none() && !legend_box_always).is_some_and(|r| {
            covered(r) == 0.0
                // A measured grid's lines aren't traced, so the box stays under one.
                && grid.as_ref().is_none_or(|_| {
                    step.is_some_and(|step| !graticule_crosses(s, center, scale, (fw, fh), (r.0 - fx, r.1 - fy, r.2, r.3), step))
                })
                && crate::render::layout::frame_plain_under(s.map_mut(), center, scale, (fw, fh), (r.0 - fx, r.1 - fy, r.2, r.3), 384)
        });
    if let Some(r) = legend_rect {
        let (lw, lh) = (r.2, r.3);
        l.add(
            "Legend Box",
            r,
            if card {
                pub_card(Rgba::rgb(0xFF, 0xFF, 0xFF), Rgba::new(0x2B, 0x26, 0x20, 60), 0.4, card_r)
            } else {
                surround_box(if legend_bare { clear } else { paper })
            },
        );
        // The colorbar at the bottom of the box, as tall as the box allows; the legend's rows in
        // what is left above it. Each says when it doesn't fit, rather than one blaming the other.
        // The colorbar's title wraps to the box as it is drawn (#2493), which may be wider than
        // the colorbar asked for (the rows in more columns): fewer lines leave the rows more room.
        let (ix, iy, iw, ih) = (r.0 + pad, r.1 + pad, lw - 2.0 * pad, lh - 2.0 * pad);
        let bar_need = bar.as_deref().map_or(0.0, |title| colorbar_mm(title, iw));
        let bar_fit = colorbar_fit(bar_need, ih, ch + bar_gap, over_map);
        let rows_h = (ih - bar_fit - if bar.is_some() { bar_gap } else { 0.0 }).max(0.0);
        warnings.extend(legend_fit_warnings(legend_cols, (cw, ch), (iw, rows_h)).into_iter().map(String::from));
        let short = bar_need > bar_fit + LEGEND_FIT_MM;
        // In a column the colorbar takes the column's width, as its rows do.
        if let Some(layer) = bar.as_deref().filter(|_| (base_w > iw + LEGEND_FIT_MM && !legend_in_column) || short) {
            // With no height left in the box the bar is not drawn at all, so the notice says so.
            if bar_fit > 0.0 {
                warnings.push(format!("the legend's box is smaller than the colorbar of '{layer}': it is drawn narrower or shorter"));
            } else {
                warnings.push(format!("the legend's box has no room for the colorbar of '{layer}': it is left out"));
            }
        }
        if ch > 0.0 && rows_h >= 1.0 {
            l.add(
                "Legend",
                (ix, iy, iw, rows_h),
                ElementKind::Legend {
                    frame,
                    title: String::new(),
                    all_classes: false,
                    hide_other: false,
                    show_no_data: false,
                    exclude: context.clone(),
                    columns: legend_cols,
                    labels: Default::default(),
                    color: None,
                    text_size: legend_pt,
                },
            );
        }
        if let Some(layer) = &bar {
            l.add(
                "Colorbar",
                (ix, iy + ih - bar_fit, iw, bar_fit),
                ElementKind::Colorbar {
                    frame,
                    layer: Some(layer.clone()),
                    title: layer.clone(),
                    orientation: Orientation::Horizontal,
                    ticks: 5,
                    tick_side: TickSide::End,
                    reverse: false,
                    color: None,
                },
            );
        }
    }
    let mut bar_id = None;
    if let (Some((sx, sy, w, h)), Some(bar)) = (spot_of(Surround::Scale), scale_kind) {
        placed.push(("scale bar", (sx, sy, w, h)));
        l.add("Scale Box", (sx, sy, w, sh + nh), surround_box(paper));
        bar_id = Some(l.add("Scale Bar", (sx, sy, w, sh), bar));
        l.add("Scale", (sx, sy + sh, w, nh), pub_text(Broken::from(vec!["{scale}".to_string()]), small_pt, ink, false, TextAlign::Center));
    }
    // What placing couldn't avoid: a surround over the map's drawing while a clearly freer corner
    // went to another (more surrounds than free corners), or two overlapping (a frame too small
    // for them all). Data under every corner alike (a context layer filling the frame) leaves
    // nothing to move, so it isn't said. One warning names them all.
    let crowded: Vec<&str> = placed
        .iter()
        .filter(|(_, r)| {
            let share = covered(*r);
            let freest = (0..4).map(|c| covered(at(c, r.2, r.3))).fold(f64::INFINITY, f64::min);
            share > OVER_DATA && share - freest > OVER_DATA
        })
        .map(|(what, _)| *what)
        .collect();
    if let Some((last, rest)) = crowded.split_last() {
        let (names, verb) = match rest {
            [] => (format!("the {last}"), "sits"),
            _ => (format!("the {} and the {last}", rest.join(", the ")), "sit"),
        };
        warnings
            .push(format!("{names} {verb} on what the map shows, the freer places having gone to the others: move by hand what matters"));
    }
    for (i, (a, ra)) in placed.iter().enumerate() {
        for (b, rb) in placed.iter().skip(i + 1) {
            if ra.0 < rb.0 + rb.2 && rb.0 < ra.0 + ra.2 && ra.1 < rb.1 + rb.3 && rb.1 < ra.1 + ra.3 {
                warnings.push(format!("the {a} and the {b} overlap: the frame is too small for both"));
            }
        }
    }
    if let (None, Some(legend)) = (column, spot_of(Surround::Legend)) {
        align_footer(&mut around, legend, (fx, fw), &sections);
    }
    for (n, r, kind, _) in over {
        l.add(&n, r, kind);
    }
    for (n, r, kind) in around {
        l.add(&n, r, kind);
    }
    if s.project.layouts.len() >= crate::project::MAX_LAYOUTS {
        return Err(format!("a project holds at most {} layouts; remove one first", crate::project::MAX_LAYOUTS));
    }
    // The coverage grid as the placement searched it, with what was placed on it, for a look at
    // why a surround moved (#1379). The layout is made by then (nothing below refuses it): a file
    // that can't be written is said, and the layout kept.
    let mut coverage_written = Value::Null;
    if let Some(path) = &coverage_path {
        let rects: Vec<Rect> = placed.iter().map(|(_, r)| *r).collect();
        let png = coverage
            .as_ref()
            .ok_or_else(|| "the map couldn't be drawn for it".to_string())
            .and_then(|grid| coverage_png(grid, (fx, fy, fw, fh), area, &rects));
        match png.and_then(|bytes| rustgis_data::io::write_atomic(Path::new(path), &bytes).map_err(|e| format!("{path}: {e}"))) {
            Ok(()) => coverage_written = json!(path),
            Err(why) => warnings.push(format!("the coverage image isn't written: {why}")),
        }
    }
    // Every element by name and id, for `layout.update`.
    let elements = element_list(&l);
    s.project.layouts.push(l);
    let i = s.project.layouts.len() - 1;
    s.active_layout = Some(i);
    for cut in [name_cut, locator_cut].into_iter().flatten() {
        s.notify(cut, false);
    }
    say_layout_text_statistics(s, i);
    // What a scale bar or a grid asked for in other than its default look loses or has to say.
    let asked = |key: &str| p.get(key).is_some_and(Value::is_object);
    if let Some(bar) = bar_id.filter(|_| asked("scaleBar")) {
        say_text_fit(s, i, bar, true);
    }
    if let Some(crs) = p.get("grid").and_then(|g| g.get("crs")).and_then(Value::as_str) {
        crate::cmd::say_crs_note(s, crs);
    }
    let locator = locator_map.map(|(map, ..)| map);
    Ok(
        json!({"layout": i, "name": name, "profile": profile.id, "page": [w, h], "scale": scale, "grid": step, "locator": locator, "style": style.code(), "arrangement": arrangement_used, "legendColumns": legend_cols, "mapShift": [map_shift.0, map_shift.1], "veiled": veiled.is_some(), "legendBox": legend_rect.is_some() && !legend_bare, "elements": elements, "moved": moved, "coverage": coverage_written, "warnings": warnings}),
    )
}

/// Whether the publication gives the top shown raster with a colour scale a colorbar: `"auto"` (the
/// default) or `"none"`.
pub(in crate::cmd::layout) fn publication_colorbar(p: &Value) -> Result<bool, String> {
    match p.get("colorbar").filter(|v| !v.is_null()) {
        None => Ok(true),
        Some(v) => match v.as_str().map(str::trim) {
            Some("auto") => Ok(true),
            Some("none") => Ok(false),
            _ => Err("colorbar is \"auto\" or \"none\"".into()),
        },
    }
}

/// The names a publication's legend leaves out as context around the data: the layers the placement
/// takes as context in a frame `size` (mm) showing `map` at 1:`scale` around `center`
/// ([`crate::render::layout::context_layers`], #1379). The legend leaves layers out by name, so a
/// name is left out only when every layer it would list under that name is context, and no web
/// tile layer has it.
pub(in crate::cmd::layout) fn legend_context(map: &Map, center: Coord, scale: f64, size: (f64, f64)) -> Vec<String> {
    let ids = crate::render::layout::context_layers(map, center, scale, size);
    let shown: Vec<&rustgis_map::Layer> = map.layers.iter().filter(|l| map.shows(l) && crate::render::layout::legend_lists(l)).collect();
    let mut names: Vec<String> = Vec::new();
    for l in shown.iter().filter(|l| ids.contains(&l.id)) {
        let same = |name: &str| name.eq_ignore_ascii_case(&l.name);
        let all = shown.iter().filter(|o| same(&o.name)).all(|o| ids.contains(&o.id));
        if all && !map.tiles.iter().any(|t| same(&t.name)) && !names.iter().any(|n| same(n)) {
            names.push(l.name.clone());
        }
    }
    names
}

/// A publication's `key` given as `true` or `false`; `None` when it isn't given.
pub(in crate::cmd::layout) fn publication_flag(p: &Value, key: &str) -> Result<Option<bool>, String> {
    match p.get(key).filter(|v| !v.is_null()) {
        None => Ok(None),
        Some(v) => v.as_bool().map(Some).ok_or_else(|| format!("{key} is true or false")),
    }
}

/// The span in degrees, on its longer side, from which a publication's extent is continental: its
/// graticule's meridians then turn too far across the frame for one north arrow to point north
/// everywhere, and they give north themselves (#1379).
pub(crate) const CONTINENTAL_DEG: f64 = 20.0;

/// An extent's span in degrees on its longer side, its width as long at its middle latitude.
pub(crate) fn extent_span_deg(e: &Extent) -> f64 {
    let mid = ((e.min_y + e.max_y) / 2.0).to_radians();
    e.height().max(e.width() * mid.cos())
}

/// Whether a publication's legend always has its box: `true` for "always", `false` for "auto"
/// (the default: none where the map shows nothing under it but its plain background, #1379).
pub(in crate::cmd::layout) fn publication_legend_box(p: &Value) -> Result<bool, String> {
    match p.get("legendBox").filter(|v| !v.is_null()) {
        None => Ok(false),
        Some(v) => match v.as_str().map(str::trim) {
            Some("auto") => Ok(false),
            Some("always") => Ok(true),
            _ => Err("legendBox is \"auto\" or \"always\"".into()),
        },
    }
}

/// Whether a line of a graticule every `step` degrees crosses `under` (mm from the top left of a
/// frame `size` mm wide and tall showing the active map at 1:`scale` around `center`): the
/// rectangle's longitudes or latitudes, read on a grid of points over it, run past a multiple of
/// the step. A point off the projection, or longitudes over half the globe apart, count as
/// crossed.
pub(in crate::cmd::layout) fn graticule_crosses(
    s: &Session,
    center: Coord,
    scale: f64,
    (fw, fh): (f64, f64),
    under: Rect,
    step: f64,
) -> bool {
    const N: usize = 8;
    let flat = s.map().flat();
    let Some(at) = flat.forward_checked(center) else { return true };
    let paper_mm_per_unit = 1000.0 * flat.ground_per_unit(at) * s.map().body().ratio() / scale;
    let (mut lon, mut lat) = ((f64::INFINITY, f64::NEG_INFINITY), (f64::INFINITY, f64::NEG_INFINITY));
    for i in 0..=N {
        for j in 0..=N {
            let x = under.0 + under.2 * i as f64 / N as f64 - fw / 2.0;
            let y = under.1 + under.3 * j as f64 / N as f64 - fh / 2.0;
            let Some(ll) = flat.inverse(Coord::new(at.x + x / paper_mm_per_unit, at.y - y / paper_mm_per_unit)) else { return true };
            (lon, lat) = ((lon.0.min(ll.x), lon.1.max(ll.x)), (lat.0.min(ll.y), lat.1.max(ll.y)));
        }
    }
    let crossed = |(lo, hi): (f64, f64)| (lo / step).floor() != (hi / step).floor() || (lo / step).fract() == 0.0;
    !step.is_finite() || step <= 0.0 || lon.1 - lon.0 > 180.0 || crossed(lon) || crossed(lat)
}

/// Whether the publication may veil what lies outside the extent when nothing else leaves its
/// legend room: `true` (the default) or `false`.
pub(in crate::cmd::layout) fn publication_veil(p: &Value) -> Result<bool, String> {
    match p.get("veil").filter(|v| !v.is_null()) {
        None => Ok(true),
        Some(v) => v.as_bool().ok_or_else(|| "veil is true or false".into()),
    }
}

/// Whether a publication of `map` shows the whole globe: the map is on the globe and no extent is
/// given, so the sheet's map is the globe seen whole (#2337).
pub(in crate::cmd::layout) fn shows_whole_globe(map: &rustgis_map::Map, p: &Value) -> bool {
    map.projection.is_globe() && p.get("extent").is_none_or(Value::is_null)
}

/// How much of the frame, inside its graticule's label strips, a globe seen whole spans: a little
/// less than all of it, for the atmosphere's glow at its rim.
pub(in crate::cmd::layout) const WHOLE_GLOBE_FILL: f64 = 0.94;

/// Where a frame of `map` at 1:`scale` centres the globe seen whole: at `view`, or as near it on
/// its meridian as the frame's camera can draw the globe that small. Towards a pole the camera's
/// resolution for a scale grows as one over the cosine of the latitude, and past its coarsest
/// ([`rustgis_map::camera::MAX_RES`]) the globe would be drawn larger than the frame (#2337).
pub(in crate::cmd::layout) fn drawable_globe_center(map: &Map, view: Coord, scale: f64) -> Coord {
    let fits =
        |lat: f64| rustgis_map::layout::frame_camera(map, Coord::new(view.x, lat), scale, 0.0, 0.0).res < rustgis_map::camera::MAX_RES;
    if fits(view.y) {
        return view;
    }
    // The latitude in between that the camera still draws at the scale, found by halving.
    let (mut fit, mut over) = (0.0f64, view.y.abs());
    for _ in 0..40 {
        let mid = (fit + over) / 2.0;
        if fits(mid.copysign(view.y)) { fit = mid } else { over = mid }
    }
    Coord::new(view.x, fit.copysign(view.y))
}

/// Centres the globe locator as close to the area as its circle can hold the rendered disc.
/// If even the equator does not fit, or the area would be behind the rim, keep the locator
/// centred on the area and let its circle clip the globe (#2394).
pub(crate) fn globe_locator_center(map: &Map, area: Coord, scale: f64, side_mm: f64) -> (Coord, bool) {
    let side = side_mm * rustgis_map::layout::PT_PER_MM;
    let globe = |at| {
        let camera = rustgis_map::layout::frame_camera(map, at, scale, 0.0, 0.0);
        rustgis_map::globe::Globe::new(&camera, side, side)
    };
    let fits = |at| 2.0 * globe(at).disc_radius() <= side;
    if fits(area) {
        return (area, true);
    }
    let equator = Coord::new(area.x, 0.0);
    if !fits(equator) {
        return (area, false);
    }
    // At a fixed scale the disc only grows towards the pole, even after resolution is clamped.
    let (mut inside, mut outside) = (equator, area);
    for _ in 0..40 {
        let mid = Coord::new(area.x, (inside.y + outside.y) / 2.0);
        if fits(mid) {
            inside = mid;
        } else {
            outside = mid;
        }
    }
    if globe(inside).forward(area).is_some() { (inside, true) } else { (area, false) }
}

/// Parses the publication's locator choice with its default: a globe on a sheet, none on a figure
/// or where the map is itself the globe seen whole (it would only show it again, smaller).
pub(in crate::cmd::layout) fn publication_locator(p: &Value, sheet: bool, whole_globe: bool) -> Result<Locator, String> {
    Ok(match p.get("locator").filter(|v| !v.is_null()) {
        None => {
            if sheet && !whole_globe {
                Locator::Globe
            } else {
                Locator::None
            }
        }
        Some(v) => match v.as_str().map(str::trim) {
            Some("globe") => Locator::Globe,
            Some("inset") => Locator::Inset,
            Some("none") => Locator::None,
            _ => return Err("locator is \"globe\", \"inset\" or \"none\"".into()),
        },
    })
}

/// Resolves the profile and automatic sheet orientation without laying out or rendering.
pub(in crate::cmd::layout) fn publication_profile(p: &Value) -> Result<(Option<String>, &'static PublicationProfile), String> {
    let id = match p.get("profile").filter(|v| !v.is_null()) {
        None => "a4".to_string(),
        Some(v) => v.as_str().map(|t| t.trim().to_ascii_lowercase()).ok_or("profile is a profile's name, such as \"a4-portrait\"")?,
    };
    // A sheet's size alone ("a4", the default, or any page size `layout.new` takes): its
    // orientation chosen below, from the area's shape, portrait until then.
    let auto = PAGES.iter().any(|(size, ..)| *size == id).then(|| id.clone());
    let id = match &auto {
        Some(size) => format!("{size}-portrait"),
        None => id,
    };
    let profile = PUBLICATION_PROFILES.iter().find(|pr| pr.id == id).ok_or_else(|| {
        let sizes: Vec<&str> = PAGES.iter().map(|(size, ..)| *size).collect();
        let names: Vec<&str> = PUBLICATION_PROFILES.iter().map(|pr| pr.id).collect();
        format!("unknown profile '{id}': {} (turned to the area's shape), {}", sizes.join(", "), names.join(", "))
    })?;
    Ok((auto, profile))
}
