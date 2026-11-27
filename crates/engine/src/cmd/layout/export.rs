//! Exporting a layout as PNG, PDF or SVG, and what the export says.

use super::*;

/// What an export of a layout whose texts name installed families is told while the app is still
/// indexing the fonts (its first seconds; the app's exports don't wait for them): those texts
/// are drawn in their fallbacks. A script or the command line indexes them at once.
pub(in crate::cmd::layout) fn named_fonts_unread(layout: &Layout) -> Option<&'static str> {
    let named = layout.elements.iter().any(|e| matches!(&e.kind, ElementKind::Text { font_name: Some(_), .. }));
    (named && rustgis_render::families::available().is_none()).then_some(
        "the installed fonts are still being read, so texts in a named font family are drawn in their fallback: export again in a moment",
    )
}

/// Layout `li` as a PDF (kept as vectors; every map series page when `all`; its map frames
/// georeferenced) or a PNG of the current page, at `dpi`.
pub fn layout_bytes(s: &mut Session, li: usize, pdf: bool, dpi: f64, all: bool) -> Result<Vec<u8>, String> {
    layout_bytes_with(s, li, pdf, dpi, all, true, rustgis_render::pdf::Options::default())
}

/// [`layout_bytes`], a PDF's map frames georeferenced only when `georeference` (#1191), written
/// as `options` say (its layers and text, #1815). Returns an actionable error if a linked archive changes
/// while the pages are drawn.
pub fn layout_bytes_with(
    s: &mut Session,
    li: usize,
    pdf: bool,
    dpi: f64,
    all: bool,
    georeference: bool,
    options: rustgis_render::pdf::Options,
) -> Result<Vec<u8>, String> {
    let layout = s.project.layouts.get(li).ok_or("no layout")?;
    let revisions = rustgis_render::layout::archive_revisions(layout, &s.project.maps);
    let result = layout_bytes_inner(s, li, pdf, dpi, all, georeference, options);
    let layout = s.project.layouts.get(li).ok_or("no layout")?;
    revisions.finish_revision(
        rustgis_render::layout::archive_revisions(layout, &s.project.maps),
        Some(result),
        "could not draw the page",
    )?
}

/// Layout `li`'s current page as a PNG at `dpi`, or only its part in `region` (`[x, y, w, h]` in
/// millimetres from the page's top left), drawn at that `dpi` rather than cut out of the whole
/// page: what the assistant's `view_map` shows of a layout. It says what exporting the page says
/// of it: texts cut off, named fonts still being read, frames drawn without their cast shadows.
pub fn layout_png(s: &mut Session, li: usize, dpi: f64, region: Option<[f64; 4]>) -> Result<Vec<u8>, String> {
    let layout = s.project.layouts.get(li).ok_or("no layout")?.clone();
    if let Some(note) = named_fonts_unread(&layout) {
        s.warn(note);
    }
    let page = layout.series.as_ref().map_or(0, |sr| sr.page);
    let series = series_info(s, li, page);
    let info = rustgis_render::layout::PageInfo {
        page: page + 1,
        pages: series.as_ref().map_or(1, |i| i.0),
        page_name: series.map(|i| i.1).unwrap_or_default(),
    };
    let mut clipped = rustgis_render::layout::clipped_text(&layout, &s.project.maps, dpi, &info);
    // A region says the texts cut off in it, not those elsewhere on the page it doesn't show.
    if let Some(r) = region {
        clipped.retain(|(id, _)| layout.element(*id).is_some_and(|e| rustgis_render::layout::meets(e, r, 0.0)));
    }
    let img = crate::cmd::share::saying_shadows(s, "The layout", |s| match region {
        Some(r) => rustgis_render::layout::render_region(&layout, &mut s.project.maps, dpi, &info, r),
        None => rustgis_render::layout::render(&layout, &mut s.project.maps, dpi, &info),
    })?;
    if let Some(note) = rustgis_render::layout::clipped_text_notice(&layout, &clipped) {
        s.warn(note);
    }
    img.encode_png().map_err(|e| e.to_string())
}

/// What `view_map` draws when it is given a `layout`: the layout's frames' maps and their tiles,
/// as exporting it reads them, rather than the active map's. `None` without a `layout`.
pub(crate) fn view_inputs(input: &crate::cmd::InputContext<'_>, p: &Value) -> Option<Result<crate::cmd::Inputs, String>> {
    p.get("layout").filter(|v| !v.is_null()).map(|_| layout_inputs(input, p))
}

/// The layout `p` names (`layout`, a name or an index), as Export Layout finds it.
pub fn named(s: &Session, p: &Value) -> Result<usize, String> {
    index(s, p)
}

pub(in crate::cmd::layout) fn layout_bytes_inner(
    s: &mut Session,
    li: usize,
    pdf: bool,
    dpi: f64,
    all: bool,
    georeference: bool,
    options: rustgis_render::pdf::Options,
) -> Result<Vec<u8>, String> {
    let saved = s.project.layouts.get(li).ok_or("no layout")?.clone();
    if let Some(note) = named_fonts_unread(&saved) {
        s.warn(note);
    }
    let dpi = if dpi.is_finite() { dpi.clamp(36.0, 600.0) } else { 150.0 };
    let current = saved.series.as_ref().map_or(0, |sr| sr.page);
    // Only a whole-series PDF names every page.
    let info = series_info(s, li, current);
    let names = if all && pdf { series_pages(s, li) } else { None };
    // A series whose index layer is gone has no pages to step through (#3075).
    if all
        && pdf
        && info.is_none()
        && let Some(series) = &saved.series
    {
        s.warn(format!("The map series' index layer '{}' isn't in its frame's map: the PDF holds the current page only", series.layer));
    }
    let total = info.as_ref().map_or(1, |i| i.0);
    // Every page of a map series (PDF), or just the layout as it is now.
    let pages: Vec<usize> = match (&names, all && pdf) {
        (Some(n), true) => {
            if n.len() > 500 {
                s.warn(format!("The map series has {} pages; the PDF holds the first 500", n.len()));
            }
            (0..n.len().min(500)).collect()
        }
        _ => vec![current],
    };
    let stepping = names.is_some() && pages.len() > 1;
    // What the PDF draws as pictures because its vectors would grow too large: frames, and
    // whole pages (their surrounds).
    let (mut frames_pictured, mut pages_pictured) = (Vec::new(), Vec::new());
    // The map frames a PDF can't georeference, and those it georeferences by their corners only,
    // each with the page (from 1) it is on.
    let (mut left_out, mut by_corners): (Vec<(usize, u32, String, &'static str)>, Vec<(usize, u32, String)>) = (Vec::new(), Vec::new());
    let mut clipped = BTreeMap::new();
    let render_pages = || -> Result<(Vec<rustgis_render::pdf::Page>, Option<Vec<u8>>), String> {
        let mut out = Vec::new();
        let mut png = None;
        let mut held = 0usize;
        // How far a series' pages have come, when the export runs with progress.
        if pages.len() > 1 {
            rustgis_data::progress::begin(pages.len());
        }
        for page in &pages {
            if stepping {
                series_page(s, &json!({"layout": li, "page": page}))?;
            }
            let layout = s.project.layouts.get(li).ok_or("no layout")?;
            let info = rustgis_render::layout::PageInfo {
                page: page + 1,
                pages: total,
                page_name: match &names {
                    Some(n) => n.get(*page).cloned().unwrap_or_default(),
                    None => info.as_ref().map(|i| i.1.clone()).unwrap_or_default(),
                },
            };
            clipped.extend(rustgis_render::layout::clipped_text(layout, &s.project.maps, dpi, &info));
            // Each page is kept as vectors (its pictures compressed) as soon as it's drawn, within
            // a memory budget.
            if pdf {
                let (w, h) = (layout.width, layout.height);
                // Where its map frames show, for readers to locate and measure in: a GeoPDF (#1191).
                let geo = if georeference {
                    rustgis_render::layout::frame_viewports(layout, &s.project.maps, dpi)
                } else {
                    rustgis_render::layout::Georeferenced::default()
                };
                left_out.extend(geo.left_out.into_iter().map(|(id, n, why)| (page + 1, id, n, why)));
                by_corners.extend(geo.by_corners.into_iter().map(|(id, n)| (page + 1, id, n)));
                let (drawing, pictured) = rustgis_render::layout::render_drawing(layout, &mut s.project.maps, dpi, &info)?;
                if drawing.overflowed() {
                    pages_pictured.push(page + 1);
                    let layout = s.project.layouts.get(li).ok_or("no layout")?;
                    let img = rustgis_render::layout::render(layout, &mut s.project.maps, dpi, &info)?;
                    out.push(rustgis_render::pdf::Page::new(&img, w, h).with_viewports(geo.viewports));
                } else {
                    frames_pictured.extend(pictured);
                    out.push(rustgis_render::pdf::Page::drawing(drawing, w, h).with_viewports(geo.viewports));
                }
                held += out.last().map_or(0, rustgis_render::pdf::Page::len);
                if held > PDF_BUDGET {
                    return Err(format!(
                        "the PDF passed {} MB at page {}; export at a lower dpi, or fewer pages",
                        PDF_BUDGET >> 20,
                        page + 1
                    ));
                }
            } else {
                let img = rustgis_render::layout::render(layout, &mut s.project.maps, dpi, &info)?;
                png = Some(img.encode_png().map_err(|e| e.to_string())?);
            }
            // A cancelled export stops here, its layout put back below.
            rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
        }
        Ok((out, png))
    };
    // Layers its frames drew without their cast shadows (read in place, too large for the
    // browser, no memory, still being worked out on the interface's thread): said, as the map's
    // export says them.
    let (result, said) = rustgis_render::shadows_said(render_pages);
    if result.is_ok() {
        for w in crate::cmd::share::shadow_warnings("The layout", &said) {
            s.warn(w);
        }
    }
    // Stepping through the series moved the frame; put the layout back however that went.
    if stepping && let Some(cur) = s.project.layouts.get_mut(li) {
        *cur = saved.clone();
    }
    let (pages, png) = result?;
    if let Some(note) = rustgis_render::layout::clipped_text_notice(&saved, &clipped.into_iter().collect::<Vec<_>>()) {
        s.warn(note);
    }
    pictured_notices(s, &frames_pictured, &pages_pictured);
    georeference_notices(s, &left_out, &by_corners, pages.len());
    if !pdf {
        return png.ok_or_else(|| "nothing to draw".to_string());
    }
    let (bytes, outlined) = rustgis_render::pdf::write_with(&pages, &saved.name, options);
    outlined_notice(s, &outlined);
    Ok(bytes)
}

/// Says which faces a PDF set in outlines though its text was to be in fonts (#1815), with why:
/// their text is drawings, not text, in a vector editor. One notice, the faces counted.
pub(crate) fn outlined_notice(s: &mut Session, outlined: &[rustgis_render::pdf::Outlined]) {
    let n = outlined.len();
    if n == 0 {
        return;
    }
    let mut each: Vec<String> = outlined.iter().take(10).map(|o| format!("{} ({})", o.face, o.why)).collect();
    if n > 10 {
        each.push(format!("and {} more", n - 10));
    }
    s.warn(format!(
        "The PDF sets the text of {n} face{} as outlines, not as text a vector editor can edit: {}",
        if n == 1 { "" } else { "s" },
        each.join(", ")
    ));
}

/// Says which map frames a PDF doesn't georeference, with why, and which it georeferences by their
/// corners only (#1191): one notice for each, with a count. Each entry has the page (from 1) it
/// is on, of the `pages` exported. A frame is counted once however many pages of a series it is
/// on (by its id: two frames sharing a name are two), with the first page's reason, and named with
/// its pages when it is so on some of them only: a series moves each frame, which may cross the
/// 180° meridian on one page and not the next. Information, not warnings: nothing wrong is
/// written (a frame left out carries no coordinates, and one placed by its corners is checked to
/// within half a millimetre).
pub(crate) fn georeference_notices(
    s: &mut Session,
    left_out: &[(usize, u32, String, &'static str)],
    by_corners: &[(usize, u32, String)],
    pages: usize,
) {
    let listed = |items: Vec<String>| {
        let more = items.len().saturating_sub(10);
        let mut text = items.into_iter().take(10).collect::<Vec<_>>().join(", ");
        if more > 0 {
            text.push_str(&format!(" and {more} more"));
        }
        text
    };
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    // Each frame once, in the order first met: its name, the first page's reason and its pages.
    let frames = |entries: Vec<(usize, u32, &str, &'static str)>| {
        let mut frames: Vec<(u32, String, &'static str, Vec<usize>)> = Vec::new();
        for (page, id, name, why) in entries {
            match frames.iter_mut().find(|f| f.0 == id) {
                Some(f) if !f.3.contains(&page) => f.3.push(page),
                Some(_) => {}
                None => frames.push((id, name.to_string(), why, vec![page])),
            }
        }
        frames
    };
    // A frame's name, with its pages when it is so on some of the pages only.
    let named = |name: &str, on: &[usize]| {
        if pages < 2 || on.len() >= pages {
            return name.to_string();
        }
        let mut shown: Vec<String> = on.iter().map(usize::to_string).collect();
        let more = shown.len().saturating_sub(10);
        shown.truncate(10);
        let listed = match (shown.split_last(), more) {
            (Some((last, rest)), 0) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            (_, 0) => shown.join(", "),
            (_, more) => format!("{} and {more} more", shown.join(", ")),
        };
        format!("{name} on page{} {listed}", plural(on.len()))
    };
    let out: Vec<String> = frames(left_out.iter().map(|(p, id, n, why)| (*p, *id, n.as_str(), *why)).collect())
        .into_iter()
        .map(|(_, n, why, on)| format!("{}: {why}", named(&n, &on)))
        .collect();
    if !out.is_empty() {
        let n = out.len();
        s.notify(format!("The PDF doesn't georeference {n} map frame{} ({})", plural(n), listed(out)), false);
    }
    let corners: Vec<String> = frames(by_corners.iter().map(|(p, id, n)| (*p, *id, n.as_str(), "")).collect())
        .into_iter()
        .map(|(_, n, _, on)| named(&n, &on))
        .collect();
    if !corners.is_empty() {
        let n = corners.len();
        s.notify(
            format!(
                "The PDF georeferences {n} map frame{} by {} corners in longitude/latitude, as {} can't be written as WKT: on a 9 × 9 grid over each, it puts places within 0.5 mm of where they are drawn, not proven between them ({})",
                plural(n),
                if n == 1 { "its" } else { "their" },
                if n == 1 { "its coordinate system" } else { "their coordinate systems" },
                listed(corners)
            ),
            false,
        );
    }
}

/// Says which layout elements, map frames and surrounds (by name), and pages (by number) an export
/// draws as pictures because their
/// vectors would pass the size a page's content may come to: one notice for each, with a count.
pub(crate) fn pictured_notices(s: &mut Session, frames: &[(u32, String)], pages: &[usize]) {
    let mb = rustgis_render::vector::MAX_COST >> 20;
    // The first of a list, and how many more there are.
    let listed = |items: Vec<String>, most: usize| {
        let more = items.len().saturating_sub(most);
        let mut text = items.into_iter().take(most).collect::<Vec<_>>().join(", ");
        if more > 0 {
            text.push_str(&format!(" and {more} more"));
        }
        text
    };
    // Each element once (a frame on every page of a series is one frame), by its id and name: two
    // elements sharing a name are two.
    let mut elements: Vec<(u32, String)> = frames.to_vec();
    elements.sort_unstable();
    elements.dedup();
    let names: Vec<String> = elements.into_iter().map(|(_, name)| name).collect();
    if !names.is_empty() {
        let n = names.len();
        s.warn(format!(
            "The export draws {n} layout element{} as pictures, not vectors: their vectors would pass {mb} MB ({})",
            if n == 1 { "" } else { "s" },
            listed(names, 10)
        ));
    }
    if !pages.is_empty() {
        let n = pages.len();
        s.warn(format!(
            "The export draws {n} page{} as pictures, not vectors: their vectors would pass {mb} MB (page {})",
            if n == 1 { "" } else { "s" },
            listed(pages.iter().map(usize::to_string).collect(), 20)
        ));
    }
}

/// The keys `layout.export`'s `"pdf"` takes.
pub(in crate::cmd::layout) const PDF_KEYS: &[&str] = &["layers", "text"];

/// How `layout.export`'s `"pdf": {…}` asks a PDF to be written (#1815): with its layers unless
/// `"layers": false`, its text in its fonts unless `"text": "outlines"`. It is checked whatever
/// the export writes, a PNG or SVG too, so a script's mistake shows on its first run; a key it
/// doesn't know is refused, with the keys it takes.
pub(in crate::cmd::layout) fn pdf_options(p: &Value) -> Result<rustgis_render::pdf::Options, String> {
    use rustgis_render::pdf::Text;
    let mut options = rustgis_render::pdf::Options::default();
    let Some(pdf) = p.get("pdf").filter(|v| !v.is_null()) else { return Ok(options) };
    let Some(pdf) = pdf.as_object() else { return Err(r#"pdf is an object: {"layers": true|false, "text": "fonts"|"outlines"}"#.into()) };
    if let Some(key) = pdf.keys().find(|k| !PDF_KEYS.contains(&k.as_str())) {
        return Err(format!("pdf has no key \"{key}\": its keys are {}", PDF_KEYS.join(", ")));
    }
    match pdf.get("layers") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(on)) => options.layers = *on,
        Some(_) => return Err("pdf.layers is true or false".into()),
    }
    options.text = match pdf.get("text") {
        None | Some(Value::Null) => Text::Fonts,
        Some(v) if v == "fonts" => Text::Fonts,
        Some(v) if v == "outlines" => Text::Outlines,
        Some(_) => return Err(r#"pdf.text is "fonts" or "outlines""#.into()),
    };
    Ok(options)
}

/// `layout.export`: a layout to a PNG, PDF or SVG. It draws a copy of the project, so the project
/// can change meanwhile (a map series steps through its pages on the copy); a cancelled export
/// stops at the next page, or its map's wait for tiles, and writes nothing.
pub(crate) fn export_work(s: &Session, p: &Value) -> Result<crate::cmd::exports::Work, String> {
    let path = ps(p, "path").ok_or("path is required")?.to_string();
    let li = index(s, p)?;
    let lower = path.to_ascii_lowercase();
    let (pdf, svg) = (lower.ends_with(".pdf"), lower.ends_with(".svg"));
    if !pdf && !svg && !lower.ends_with(".png") {
        return Err("export to a .png, .pdf or .svg file".into());
    }
    let (dpi, all, georeference) = (f(p, "dpi").unwrap_or(150.0), b(p, "allPages").unwrap_or(true), b(p, "georeference").unwrap_or(true));
    let options = pdf_options(p)?;
    let mut project = s.project.clone();
    for m in &mut project.maps {
        m.detach_reads();
    }
    Ok(Box::new(move || {
        let mut copy = Session::new(project);
        let written = (|| {
            let bytes = if svg {
                layout_svg(&mut copy, li, dpi)?.into_bytes()
            } else {
                layout_bytes_with(&mut copy, li, pdf, dpi, all, georeference, options)?
            };
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
            rustgis_data::io::write_atomic(Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path, "bytes": bytes.len()}))
        })();
        // What drawing it said (frames drawn as pictures, fonts it lacks), as the command says it
        // here.
        crate::cmd::exports::drawn_on(&mut copy, written, path)
    }))
}

/// The layout `li` as it is now (a map series at its current page) as SVG, kept as vectors with
/// pictures at `dpi`; a frame too large for vectors is a picture, and the export says so.
pub(in crate::cmd::layout) fn layout_svg(s: &mut Session, li: usize, dpi: f64) -> Result<String, String> {
    let dpi = if dpi.is_finite() { dpi.clamp(36.0, 600.0) } else { 150.0 };
    let current = s.project.layouts.get(li).ok_or("no layout")?.series.as_ref().map_or(0, |sr| sr.page);
    let series = series_info(s, li, current);
    let info = rustgis_render::layout::PageInfo {
        page: current + 1,
        pages: series.as_ref().map_or(1, |i| i.0),
        page_name: series.map(|i| i.1).unwrap_or_default(),
    };
    // Rendering borrows the current layout; it does not step or modify the series page.
    let layout = s.project.layouts.get(li).ok_or("no layout")?;
    let clipped = rustgis_render::layout::clipped_text(layout, &s.project.maps, dpi, &info);
    let note = rustgis_render::layout::clipped_text_notice(layout, &clipped);
    let (text, pictured) = rustgis_render::svg::layout_svg(layout, &mut s.project.maps, dpi, &info)?;
    if let Some(note) = note {
        s.warn(note);
    }
    pictured_notices(s, &pictured, &[]);
    Ok(text)
}
