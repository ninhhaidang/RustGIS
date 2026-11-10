//! A layout as SVG: the page in millimetres, each element in a group of its own, named after it.
//! Text, rectangles, ellipses and lines are SVG text and shapes, placed and styled as
//! [`crate::layout::render`] draws them. Map frames and the surrounds are recorded as the PDF
//! records them ([`crate::layout::render_drawing`]) and written as SVG ([`vectors`]): paths,
//! glyph outlines, gradients and groups, with what is pixels by nature (rasters, tiles, a tilted
//! or globe frame) as pictures in them. A frame whose vectors would pass the budget is a picture
//! of that element alone, in its place.

mod vectors;

use std::fmt::Write as _;

use base64::Engine;
use rustgis_map::Map;
use rustgis_map::layout::{ElementKind, FontFamily, Layout, LineHead, PT_PER_MM, TextAlign, frame_camera};

use crate::layout::{MAX_PIXELS, PageInfo, expand, page_geometry, record_element, render_element};

/// Writes `layout` as an SVG document, pictures drawn at `dpi`, and the elements (id and name)
/// drawn as pictures because their vectors would pass the budget. Refuses a source changed during drawing.
pub fn layout_svg(layout: &Layout, maps: &mut [Map], dpi: f64, info: &PageInfo) -> Result<(String, Vec<(u32, String)>), String> {
    let revisions = crate::layout::archive_revisions(layout, maps);
    let result = layout_svg_inner(layout, maps, dpi, info);
    revisions.finish_revision(
        crate::layout::archive_revisions(layout, maps),
        result,
        "could not draw the page; check its map sources and export size",
    )
}

fn layout_svg_inner(layout: &Layout, maps: &mut [Map], dpi: f64, info: &PageInfo) -> Option<(String, Vec<(u32, String)>)> {
    let mut defs = vectors::Defs::default();
    let mut pictured = Vec::new();
    let mut body = elements(layout, maps, dpi, info, &mut defs, &mut pictured)?;
    let (pw, ph) = (layout.width, layout.height);
    // The head (with the defs the body refers to) goes in front of the body in place: a large
    // export's pictures are never held twice.
    let mut s = String::new();
    let _ = write!(
        s,
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            "\n",
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" "#,
            r#"xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape" "#,
            r#"width="{w}mm" height="{h}mm" viewBox="0 0 {w} {h}">"#,
            "\n",
        ),
        w = num(pw),
        h = num(ph)
    );
    if !defs.text.is_empty() {
        let _ = write!(s, "<defs>\n{}</defs>\n", defs.text);
    }
    let _ = writeln!(s, r##"<rect width="{}" height="{}" fill="#ffffff"/>"##, num(pw), num(ph));
    body.insert_str(0, &s);
    body.push_str("</svg>\n");
    Some((body, pictured))
}

/// Each visible element of `layout` as an SVG group (see the module), what they refer to added to
/// `defs`; `None` when one can't be drawn.
fn elements(
    layout: &Layout,
    maps: &mut [Map],
    dpi: f64,
    info: &PageInfo,
    defs: &mut vectors::Defs,
    pictured: &mut Vec<(u32, String)>,
) -> Option<String> {
    let ppp = page_geometry(layout, dpi).2 / PT_PER_MM;
    let mut s = String::new();
    // The elements drawn as pictures (past the budget): their places, filled in at the end at
    // the resolution they share.
    let mut as_pictures: Vec<u32> = Vec::new();
    // What the elements kept as vectors come to: held to one page's budget, as a PDF page is.
    let mut written = 0usize;
    for e in layout.elements.iter().filter(|e| e.visible) {
        let _ = writeln!(s, r#"<g id="element-{}" inkscape:label="{}" inkscape:groupmode="layer">"#, e.id, esc(&e.name));
        let (x, y, w, h) = (e.x, e.y, e.w, e.h);
        match &e.kind {
            ElementKind::Text { text: t, size, color, bold, italic, align, letter_spacing, font, font_name } => {
                let first = layout.first_frame().and_then(|id| layout.element(id)).and_then(|f| match &f.kind {
                    ElementKind::MapFrame { map, center, scale, bearing, pitch, .. } => {
                        let m = maps.iter().find(|m| m.name.eq_ignore_ascii_case(map))?;
                        Some((m, frame_camera(m, *center, *scale, *bearing, *pitch)))
                    }
                    _ => None,
                });
                let text = expand(t, layout, info, first);
                // As the page draws it: the size in points, lines 1.25 apart, the first baseline
                // 0.6 of the size below the top, lines past the frame left out.
                let em = size.clamp(2.0, 400.0) * 1.33 / PT_PER_MM;
                let (anchor, tx) = match align {
                    TextAlign::Left => ("start", x),
                    TextAlign::Center => ("middle", x + w / 2.0),
                    TextAlign::Right => ("end", x + w),
                };
                let lines: Vec<(f64, &str)> = text
                    .lines()
                    .take(200)
                    .enumerate()
                    .map(|(i, l)| (y + em * (0.6 + 1.25 * i as f64), l))
                    .take_while(|(ly, _)| *ly <= y + h + em)
                    .collect();
                // A named family first (#762), quoted as CSS has it (its own backslashes and
                // quotes escaped), then the generic one, for a viewer without it.
                let generic = if *font == FontFamily::Serif { "serif" } else { "sans-serif" };
                let family = match font_name {
                    Some(n) => format!("'{}', {generic}", n.replace('\\', "\\\\").replace('\'', "\\'")),
                    None => generic.to_string(),
                };
                let _ = write!(
                    s,
                    r#"<text x="{}" y="{}" font-family="{}" font-size="{}" font-weight="{}" font-style="{}" text-anchor="{anchor}" letter-spacing="{}"{} xml:space="preserve">"#,
                    num(tx),
                    num(lines.first().map_or(y + em * 0.6, |l| l.0)),
                    esc(&family),
                    num(em),
                    if *bold { "bold" } else { "normal" },
                    if *italic { "italic" } else { "normal" },
                    num(*letter_spacing * 1.33 / PT_PER_MM),
                    paint("fill", *color),
                );
                match lines.as_slice() {
                    [(_, only)] => s.push_str(&esc(only)),
                    many => {
                        for (ly, l) in many {
                            let _ = write!(s, r#"<tspan x="{}" y="{}">{}</tspan>"#, num(tx), num(*ly), esc(l));
                        }
                    }
                }
                s.push_str("</text>\n");
            }
            ElementKind::Rectangle { fill, outline, width, radius } => {
                let rx = radius.clamp(0.0, w.min(h) / 2.0);
                let _ = writeln!(
                    s,
                    r#"<rect x="{}" y="{}" width="{}" height="{}" rx="{}"{}{}/>"#,
                    num(x),
                    num(y),
                    num(w),
                    num(h),
                    num(rx),
                    paint("fill", *fill),
                    stroke(*outline, *width)
                );
            }
            ElementKind::Ellipse { fill, outline, width } => {
                let _ = writeln!(
                    s,
                    r#"<ellipse cx="{}" cy="{}" rx="{}" ry="{}"{}{}/>"#,
                    num(x + w / 2.0),
                    num(y + h / 2.0),
                    num(w / 2.0),
                    num(h / 2.0),
                    paint("fill", *fill),
                    stroke(*outline, *width)
                );
            }
            ElementKind::Line { color, width, dir, head } => line(&mut s, dir.ends((x, y, w, h)), *color, *width, *head, ppp),
            _ => {
                // Recorded at the export's dpi and written as vectors, placed by its pixel
                // bounds; past the budget (its own, or the page's with the elements before it), a
                // picture of it at the pictures' resolution.
                let mut frames = Vec::new();
                if let Some((drawing, [ix, iy, ..], k)) = record_element(layout, e.id, maps, dpi, info, &mut frames)? {
                    let total = written.saturating_add(drawing.cost());
                    if !drawing.overflowed() && total <= crate::vector::max_cost() {
                        written = total;
                        pictured.append(&mut frames);
                        let _ =
                            writeln!(s, r#"<g transform="translate({} {}) scale({})">"#, num(ix), num(iy), vectors::fine((1.0 / k) as f32));
                        let (_, _, items) = drawing.finish();
                        vectors::items(&mut s, defs, &items)?;
                        s.push_str("</g>\n");
                        s.push_str("</g>\n");
                        continue;
                    }
                    pictured.push((e.id, e.name.clone()));
                }
                s.push_str(&picture_place(e.id));
                as_pictures.push(e.id);
            }
        }
        s.push_str("</g>\n");
    }
    // The pictures, at the resolution only they share (the elements kept as vectors take none).
    let picture_dpi = picture_dpi(layout, dpi, |e| as_pictures.contains(&e.id));
    for id in as_pictures {
        let mut image_tag = String::new();
        if let Some((image, [ix, iy, iw, ih])) = render_element(layout, id, maps, picture_dpi, info)? {
            let png = image.encode_png().ok()?;
            let _ = writeln!(
                image_tag,
                r#"<image x="{}" y="{}" width="{}" height="{}" preserveAspectRatio="none" xlink:href="data:image/png;base64,{}"/>"#,
                num(ix),
                num(iy),
                num(iw),
                num(ih),
                base64::engine::general_purpose::STANDARD.encode(png)
            );
        }
        let place = picture_place(id);
        if let Some(at) = s.find(&place) {
            s.replace_range(at..at + place.len(), &image_tag);
        }
    }
    Some(s)
}

/// Where element `id`'s picture goes until it is drawn: a mark no SVG text holds (text is
/// escaped, which drops control characters).
fn picture_place(id: u32) -> String {
    format!("\u{0}picture-{id}\u{0}")
}

/// Whether an element is drawn as an SVG element of its own (text and shapes), not recorded.
#[cfg(test)]
fn written_as_svg(e: &rustgis_map::layout::Element) -> bool {
    matches!(e.kind, ElementKind::Text { .. } | ElementKind::Rectangle { .. } | ElementKind::Ellipse { .. } | ElementKind::Line { .. })
}

/// The resolution the pictures of the elements `drawn` (as pictures) are drawn at: `dpi`, unless
/// together they would take more than twice the pixels of the page (surrounds usually sit on a map
/// frame), in which case all of them are drawn at the resolution that fits, so many large frames
/// don't grow the SVG without bound.
fn picture_dpi(layout: &Layout, dpi: f64, drawn: impl Fn(&rustgis_map::layout::Element) -> bool) -> f64 {
    let on_page = |start: f64, len: f64, page: f64| ((start + len).min(page) - start.max(0.0)).max(0.0);
    let area: f64 = layout
        .elements
        .iter()
        .filter(|e| e.visible && drawn(e))
        .map(|e| on_page(e.x, e.w, layout.width) * on_page(e.y, e.h, layout.height))
        .sum();
    let k = page_geometry(layout, dpi).2;
    let fit = (2.0 * MAX_PIXELS / (area * k * k)).sqrt();
    if fit < 1.0 { k * fit / PT_PER_MM * 96.0 } else { dpi }
}

/// A line element as the page draws it: a shaft and a filled triangle for each head, in millimetres.
fn line(s: &mut String, [(x0, y0), (x1, y1)]: [(f64, f64); 2], color: rustgis_map::Rgba, width: f64, head: LineHead, ppp: f64) {
    let w = (width / PT_PER_MM).max(0.5 / (ppp * PT_PER_MM));
    let (at_start, at_end) = (matches!(head, LineHead::Start | LineHead::Both), matches!(head, LineHead::End | LineHead::Both));
    let len = (x1 - x0).hypot(y1 - y0);
    let heads = f64::from(u8::from(at_start) + u8::from(at_end));
    let head_len = if heads == 0.0 || len <= 0.0 { 0.0 } else { ((6.0 + 2.5 * width) / PT_PER_MM).min(len * 0.4 / heads) };
    let (ux, uy) = if len > 0.0 { ((x1 - x0) / len, (y1 - y0) / len) } else { (1.0, 0.0) };
    let inset = head_len * 0.8;
    let (sx, sy) = if at_start { (x0 + ux * inset, y0 + uy * inset) } else { (x0, y0) };
    let (ex, ey) = if at_end { (x1 - ux * inset, y1 - uy * inset) } else { (x1, y1) };
    let _ = writeln!(s, r#"<path d="M{} {}L{} {}" fill="none"{}/>"#, num(sx), num(sy), num(ex), num(ey), stroke(color, w * PT_PER_MM));
    let half = (head_len * 0.45).max(w);
    for ((tx, ty), (dx, dy)) in
        [(at_start, (x0, y0), (-ux, -uy)), (at_end, (x1, y1), (ux, uy))].into_iter().filter(|h| h.0).map(|h| (h.1, h.2))
    {
        let (bx, by) = (tx - dx * head_len, ty - dy * head_len);
        let _ = writeln!(
            s,
            r#"<path d="M{} {}L{} {}L{} {}Z"{}/>"#,
            num(tx),
            num(ty),
            num(bx - dy * half),
            num(by + dx * half),
            num(bx + dy * half),
            num(by - dx * half),
            paint("fill", color)
        );
    }
}

/// ` stroke="…" stroke-width="…"` for an outline `width` points wide, or `stroke="none"`.
fn stroke(color: rustgis_map::Rgba, width: f64) -> String {
    if width > 0.0 {
        format!(r#"{} stroke-width="{}""#, paint("stroke", color), num(width / PT_PER_MM))
    } else {
        r#" stroke="none""#.into()
    }
}

/// ` fill="#rrggbb"` (or stroke), with its opacity when it isn't opaque.
fn paint(attr: &str, c: rustgis_map::Rgba) -> String {
    let [r, g, b, a] = c.0;
    let mut out = format!(r##" {attr}="#{r:02x}{g:02x}{b:02x}""##);
    if a < 255 {
        let _ = write!(out, r#" {attr}-opacity="{}""#, num(f64::from(a) / 255.0));
    }
    out
}

/// A number with at most three decimals and no trailing zeros.
fn num(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let t = format!("{v:.3}");
    let t = t.trim_end_matches('0').trim_end_matches('.');
    if t == "-0" { "0".into() } else { t.into() }
}

/// Text escaped for SVG content and attributes.
fn esc(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    for ch in t.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Keep text and attributes valid in XML 1.0, including hostile layout names.
            c if (c.is_control() && c != '\t') || matches!(c, '\u{fffe}' | '\u{ffff}') => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small page with a frame whose context is also used by surrounds.
    fn page() -> (Layout, Vec<Map>, u32) {
        let mut layout = Layout::new("SVG", 100.0, 80.0);
        let kind = serde_json::from_value(serde_json::json!({"MapFrame": {
            "map": "Map", "center": {"x": 0, "y": 0}, "scale": 100000000
        }}))
        .unwrap();
        let frame = layout.add("Map", (0.0, 0.0, 100.0, 80.0), kind);
        (layout, vec![Map::new("Map")], frame)
    }

    /// An A0 page with `n` map frames, each covering the whole page.
    fn stacked_frames(n: usize) -> Layout {
        let mut layout = Layout::new("A0", 841.0, 1189.0);
        for i in 0..n {
            let kind = serde_json::from_value(serde_json::json!({"MapFrame": {
                "map": "Map", "center": {"x": 0, "y": 0}, "scale": 100000000
            }}))
            .unwrap();
            layout.add(format!("Map {i}"), (0.0, 0.0, 841.0, 1189.0), kind);
        }
        layout
    }

    #[test]
    fn surrounds_on_a_frame_keep_the_picture_resolution() {
        let (mut layout, _, frame) = page();
        layout.add(
            "Legend",
            (10.0, 10.0, 40.0, 30.0),
            ElementKind::Legend { frame, title: "Map legend".into(), all_classes: true, hide_other: false, exclude: vec![], columns: 1 },
        );
        assert_eq!(picture_dpi(&layout, 600.0, |e| !written_as_svg(e)), 600.0);
        // A frame and an inset filling an A0 page that is already drawn at its pixel limit.
        let layout = stacked_frames(2);
        assert_eq!(picture_dpi(&layout, 600.0, |e| !written_as_svg(e)), 600.0);
    }

    #[test]
    fn stacked_frames_share_the_picture_budget() {
        let layout = stacked_frames(8);
        let dpi = picture_dpi(&layout, 600.0, |e| !written_as_svg(e));
        assert!(dpi < 600.0);
        let k = page_geometry(&layout, dpi).2;
        let pixels = 8.0 * 841.0 * 1189.0 * k * k;
        assert!(pixels <= 2.0 * MAX_PIXELS * (1.0 + 1e-9), "{pixels} pixels");
        assert!(pixels >= 1.99 * MAX_PIXELS, "the pictures use the budget they have: {pixels} pixels");
        // Pictures off the page take nothing from it.
        let mut layout = stacked_frames(8);
        for e in layout.elements.iter_mut().skip(1) {
            e.x = 900.0;
        }
        assert_eq!(picture_dpi(&layout, 600.0, |e| !written_as_svg(e)), 600.0);
    }

    #[test]
    fn frames_and_surrounds_are_vectors_and_a_surrounds_picture_keeps_its_page_context() {
        let (mut layout, mut maps, frame) = page();
        let legend = layout.add(
            "Legend",
            (10.0, 10.0, 40.0, 30.0),
            ElementKind::Legend { frame, title: "Map legend".into(), all_classes: true, hide_other: false, exclude: vec![], columns: 1 },
        );
        let info = PageInfo::default();
        let (svg, pictured) = layout_svg(&layout, &mut maps, 72.0, &info).unwrap();
        assert!(pictured.is_empty());
        // The frame: its land as paths, cut to the frame; the legend: its title as glyphs, no picture.
        let element =
            |id: u32| svg.split(&format!("<g id=\"element-{id}\"")).nth(1).unwrap().split("<g id=\"element-").next().unwrap().to_string();
        let (map, key) = (element(frame), element(legend));
        assert!(map.contains("clip-path=\"url(#clip") && map.matches("<path ").count() > 50, "{}", &map[..400.min(map.len())]);
        assert!(!map.contains("data:image") && !key.contains("data:image"));
        assert!(key.contains(r#"aria-label="Map legend""#) && key.contains("<use xlink:href=\"#glyph"));
        assert!(svg.contains("<defs>") && svg.find("<defs>") < svg.find("<g id=\"element-"));
        // A surround drawn as a picture (past the budget) keeps its page context and transparency.
        let (pm, _) = render_element(&layout, legend, &mut maps, 72.0, &info).unwrap().unwrap();
        assert!(pm.pixels().iter().any(|p| p.alpha() == 0), "no opaque page behind the legend");
        assert!(pm.pixels().iter().any(|p| p.alpha() > 0), "the title still uses the frame context");
        let (mut base, mut maps, _) = page();
        let mut expected = crate::layout::render(&base, &mut maps, 72.0, &info).unwrap();
        assert!(pm.width() < expected.width() / 2 && pm.height() < expected.height() / 2, "only the surround's bounds are allocated");
        let (image, [x, y, ..]) = render_element(&layout, legend, &mut maps, 72.0, &info).unwrap().unwrap();
        let k = PT_PER_MM * 72.0 / 96.0;
        expected.draw_pixmap(
            (x * k).round() as i32,
            (y * k).round() as i32,
            image.as_ref(),
            &tiny_skia::PixmapPaint::default(),
            tiny_skia::Transform::identity(),
            None,
        );
        base.elements.push(layout.element(legend).unwrap().clone());
        let actual = crate::layout::render(&base, &mut maps, 72.0, &info).unwrap();
        // Drawing onto transparency and compositing rounds the antialias coverage twice.
        let difference = actual.data().iter().zip(expected.data()).map(|(a, b)| a.abs_diff(*b)).max().unwrap_or(0);
        assert!(difference <= 2, "composited SVG pictures match the page within antialias rounding: {difference}");
    }

    #[test]
    fn an_element_too_large_for_vectors_is_a_picture_and_named() {
        use crate::vector::TEST_MAX_COST;
        let (layout, mut maps, frame) = page();
        TEST_MAX_COST.with(|l| l.set(Some(20_000)));
        let result = layout_svg(&layout, &mut maps, 72.0, &PageInfo::default());
        TEST_MAX_COST.with(|l| l.set(None));
        let (svg, pictured) = result.unwrap();
        assert_eq!(pictured, [(frame, "Map".to_string())]);
        let group = svg.split(&format!("<g id=\"element-{frame}\"")).nth(1).unwrap();
        assert!(group.contains("data:image/png;base64,"), "the frame as a picture");
    }

    #[test]
    fn cropped_images_use_their_actual_pixel_bounds() {
        let (mut layout, mut maps, _) = page();
        let id = layout.add("Picture", (-5.3, 70.2, 20.7, 20.0), ElementKind::Picture { image: String::new() });
        let info = PageInfo::default();
        let (pm, bounds) = render_element(&layout, id, &mut maps, 36.0, &info).unwrap().unwrap();
        let [x, y, w, h] = bounds;
        let k = PT_PER_MM * 36.0 / 96.0;
        assert_eq!(x, 0.0);
        assert!((y * k - (70.2 * k).floor()).abs() < 1e-9);
        assert!((w * k - f64::from(pm.width())).abs() < 1e-9);
        assert!((h * k - f64::from(pm.height())).abs() < 1e-9);
        let (svg, _) = layout_svg(&layout, &mut maps, 36.0, &info).unwrap();
        // Placed at its pixel bounds, as its picture would be.
        assert!(svg.contains(&format!(r#"<g transform="translate({} {}) scale("#, num(x), num(y))), "{svg}");
        layout.element_mut(id).unwrap().x = 110.0;
        assert!(render_element(&layout, id, &mut maps, 36.0, &info).unwrap().is_none());
        assert!(layout_svg(&layout, &mut maps, 36.0, &info).is_ok(), "an off-page image is harmless");
    }

    #[test]
    fn failed_required_map_render_fails_the_svg_export() {
        use rustgis_data::{Dataset, GeometryType};
        let (layout, mut maps, _) = page();
        maps[0].add_dataset(Dataset::new("Missing", GeometryType::Point, vec![]));
        let dir = tempfile::tempdir().unwrap();
        let mut source = rustgis_map::source::SourceLayer::default();
        source.path = Some(dir.path().join("missing.shp").display().to_string());
        maps[0].layers[0].source = Some(source);
        assert!(layout_svg(&layout, &mut maps, 72.0, &PageInfo::default()).is_err());
    }

    #[test]
    fn line_heads_remain_vector_paths() {
        let mut layout = Layout::new("Arrows", 100.0, 80.0);
        layout.add(
            "Arrow",
            (10.0, 10.0, 30.0, 20.0),
            serde_json::from_value(serde_json::json!({"Line": {
                "color": [20, 40, 60, 255], "width": 2.0, "head": "Both"
            }}))
            .unwrap(),
        );
        let (svg, _) = layout_svg(&layout, &mut [], 72.0, &PageInfo::default()).unwrap();
        assert_eq!(svg.matches("<path ").count(), 3, "shaft and two arrowheads");
        assert!(!svg.contains("<image"));
    }

    #[test]
    fn thin_lines_use_the_page_renderers_device_pixel_minimum() {
        let mut layout = Layout::new("Thin lines", 100.0, 80.0);
        layout.add(
            "Line",
            (10.0, 10.0, 30.0, 20.0),
            serde_json::from_value(serde_json::json!({"Line": {
                "color": [20, 40, 60, 255], "width": 0.1, "head": "Both"
            }}))
            .unwrap(),
        );
        for (dpi, stroke_width) in [(36.0, "0.353"), (600.0, "0.026")] {
            let (svg, _) = layout_svg(&layout, &mut [], dpi, &PageInfo::default()).unwrap();
            assert!(svg.contains(&format!(r#"stroke-width="{stroke_width}""#)), "{dpi} DPI: {svg}");
        }
    }

    #[test]
    fn numbers_and_text_are_written_plainly() {
        assert_eq!((num(297.0), num(0.1234), num(-0.0001), num(f64::NAN)), ("297".into(), "0.123".into(), "0".into(), "0".into()));
        assert_eq!(esc("a & <b> \"c\" 'd'\u{1}"), "a &amp; &lt;b&gt; &quot;c&quot; &apos;d&apos;");
        assert_eq!(esc("valid\u{fffe}\u{ffff}\t🙂"), "valid\t🙂");
        assert_eq!(paint("fill", rustgis_map::Rgba::new(255, 0, 16, 128)), r##" fill="#ff0010" fill-opacity="0.502""##);
        assert_eq!(stroke(rustgis_map::Rgba::rgb(0, 0, 0), 0.0), r#" stroke="none""#);
    }
}
