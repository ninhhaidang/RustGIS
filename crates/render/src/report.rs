//! Draws reports: a [`Doc`]'s blocks flowed onto pages (column headings repeated at the top of
//! each page, headings kept with what follows, long text split between pages), with a page
//! number footer. Layout sections are drawn as whole pages by the layout renderer.

use rustgis_map::layout::Layout;
use rustgis_map::report::{Block, Doc};
use rustgis_map::{Map, Rgba};
use tiny_skia::{Pixmap, PixmapPaint, Rect, Transform};

use crate::{paint, text};

/// Page margins and the footer band, in millimetres.
const MARGIN: f64 = 15.0;
const FOOTER: f64 = 8.0;
/// Most pages a report runs to.
pub const MAX_PAGES: usize = 5000;
/// Indent of related records.
const INDENT: f64 = 8.0;
/// Largest picture box, in millimetres.
const PICTURE_H: f64 = 70.0;
/// The heading of a record's attachment list.
const ATTACHMENTS: &str = "Attachments";
/// Most bars a chart draws.
const MAX_BARS: usize = 25;

const DARK: Rgba = Rgba::rgb(0x22, 0x22, 0x22);
const DIM: Rgba = Rgba::rgb(0x6B, 0x72, 0x80);
const ACCENT: Rgba = Rgba::rgb(0x3B, 0x82, 0xC4);
const BAND: Rgba = Rgba::rgb(0xE4, 0xE8, 0xEE);

/// One page of a report.
#[derive(Clone, Debug, PartialEq)]
pub enum Page {
    /// Blocks flowed down the page.
    Flow(Vec<Placed>),
    /// A layout (by index in the project's layouts) as the whole page.
    Layout(usize),
}

/// A block placed on a page: its top and height in millimetres, and for text the lines that
/// fell on this page.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub block: usize,
    pub y: f64,
    pub h: f64,
    pub lines: Vec<String>,
}

/// Text width in millimetres at `pt` points, as layouts and reports draw it.
pub fn width_mm(s: &str, pt: f32, bold: bool) -> f64 {
    text::measure(s, pt, bold) as f64 * 25.4 / 96.0
}

/// [`width_mm`] in the serif a layout draws a `"font": "serif"` text in (the sans where no serif is
/// installed): the ink's width, which the line has to hold.
pub fn serif_width_mm(s: &str, pt: f32, bold: bool) -> f64 {
    let (lo, hi) = text::styled_extent(s, pt * 1.33, text::Face { bold, italic: false, serif: true, family: None }, 0.0);
    f64::from((hi - lo).max(0.0)) * 25.4 / 96.0
}

/// Point size and line height (mm) of each kind of block.
fn style(b: &Block) -> (f32, bool, f64) {
    match b {
        Block::Title(_) => (18.0, true, 11.0),
        Block::Heading { level: 0, .. } => (12.5, true, 8.0),
        Block::Heading { .. } => (10.5, true, 7.0),
        Block::Columns(_) => (8.5, true, 5.5),
        Block::SubColumns(_) => (7.5, true, 4.6),
        Block::SubRow(_) => (7.5, false, 4.4),
        Block::Stats(_) | Block::Attachments(_) => (8.5, false, 4.8),
        _ => (9.0, false, 5.0),
    }
}

/// Splits text into lines no wider than `w` millimetres at `pt` points: whole words, a word wider
/// than the line broken across lines.
pub fn wrap(s: &str, w: f64, pt: f32) -> Vec<String> {
    wrap_with(s, w, |t| width_mm(t, pt, false))
}

/// [`wrap`] with the width of a piece of text (in mm) measured by `width`: bold text, or text whose
/// tokens are filled in only when it is drawn.
pub fn wrap_with(s: &str, w: f64, width: impl Fn(&str) -> f64) -> Vec<String> {
    let mut out = Vec::new();
    for para in s.lines().take(2000) {
        let mut line = String::new();
        for word in para.split_whitespace().take(5000) {
            // A word wider than the line (a long URL…) is broken across lines: the most of its
            // characters that fit, found by halving (a few measures a line, not one a character).
            let mut word = word.to_string();
            while width(&word) > w && word.chars().count() > 1 {
                let chars: Vec<char> = word.chars().collect();
                let (mut fits, mut over) = (1, chars.len());
                while over - fits > 1 {
                    let mid = (fits + over) / 2;
                    if width(&chars[..mid].iter().collect::<String>()) > w {
                        over = mid;
                    } else {
                        fits = mid;
                    }
                }
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                out.push(chars[..fits].iter().collect());
                word = chars[fits..].iter().collect();
            }
            let word = word.as_str();
            let next = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if !line.is_empty() && width(&next) > w {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            } else {
                line = next;
            }
        }
        out.push(line);
    }
    out
}

/// The size a picture is drawn at, in millimetres, within the content width and height.
fn picture_box(w: u32, h: u32, content_w: f64, content_h: f64) -> (f64, f64) {
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    let bw = (content_w * 0.6).max(10.0);
    let s = (bw / w).min(PICTURE_H.min(content_h - 4.0).max(5.0) / h);
    (w * s, h * s)
}

/// How many bars of a chart fit on a page (the rest are left off).
fn chart_bars(n: usize, content_h: f64) -> usize {
    n.clamp(1, MAX_BARS).min((((content_h - 11.0) / 5.0).floor().max(1.0)) as usize)
}

/// Heights in millimetres of a block (text blocks are measured line by line when placed).
fn height(b: &Block, content_w: f64, content_h: f64) -> f64 {
    let (_, _, lh) = style(b);
    match b {
        Block::Stats(v) => lh * v.len().max(1) as f64 + 1.5,
        Block::Attachments(v) => lh * (v.len().max(1) + 1) as f64,
        Block::Picture { width, height, .. } => picture_box(*width, *height, content_w, content_h).1 + 3.0,
        Block::Chart { bars, .. } => 8.0 + 5.0 * chart_bars(bars.len(), content_h) as f64 + 3.0,
        Block::PageBreak | Block::Layout(_) => 0.0,
        _ => lh,
    }
}

/// Flows the blocks onto pages.
pub fn paginate(doc: &Doc) -> Vec<Page> {
    let content_w = (doc.width - 2.0 * MARGIN).max(10.0);
    let bottom = (doc.height - MARGIN - FOOTER).max(MARGIN + 20.0);
    let content_h = bottom - MARGIN;
    let mut pages: Vec<Page> = Vec::new();
    let mut cur: Vec<Placed> = Vec::new();
    let mut y = MARGIN;
    // The column headings in force, repeated at the top of a new page before rows.
    let mut columns: Option<usize> = None;
    let mut sub_columns: Option<usize> = None;
    let flush = |pages: &mut Vec<Page>, cur: &mut Vec<Placed>, y: &mut f64| {
        if !cur.is_empty() {
            pages.push(Page::Flow(std::mem::take(cur)));
        }
        *y = MARGIN;
    };
    for (i, b) in doc.blocks.iter().enumerate() {
        if pages.len() >= MAX_PAGES {
            break;
        }
        match b {
            Block::PageBreak => {
                flush(&mut pages, &mut cur, &mut y);
                continue;
            }
            Block::Layout(l) => {
                flush(&mut pages, &mut cur, &mut y);
                pages.push(Page::Layout(*l));
                continue;
            }
            Block::Heading { .. } | Block::Title(_) => {
                columns = None;
                sub_columns = None;
            }
            Block::Columns(_) => {
                columns = Some(i);
                sub_columns = None;
            }
            Block::SubColumns(_) => sub_columns = Some(i),
            Block::Row(_) => sub_columns = None,
            _ => {}
        }
        // Blocks made of lines (text, statistics, attachments) split between pages when they
        // don't fit, however long they are.
        let line_block: Option<Vec<String>> = match b {
            Block::Text(t) => Some(wrap(t, content_w, style(b).0)),
            Block::Stats(v) => Some(v.clone()),
            Block::Attachments(names) => {
                Some(std::iter::once(ATTACHMENTS.to_string()).chain(names.iter().map(|a| format!("• {a}"))).collect())
            }
            _ => None,
        };
        if let Some(all) = line_block {
            let (_, _, lh) = style(b);
            let mut lines = all.into_iter().peekable();
            while lines.peek().is_some() && pages.len() < MAX_PAGES {
                let room = ((bottom - y) / lh).floor().max(0.0) as usize;
                if room == 0 {
                    flush(&mut pages, &mut cur, &mut y);
                    continue;
                }
                let part: Vec<String> = lines.by_ref().take(room).collect();
                let h = lh * part.len() as f64;
                cur.push(Placed { block: i, y, h, lines: part });
                y += h + 1.5;
            }
            continue;
        }
        let h = height(b, content_w, content_h);
        // Headings and column headings stay with what follows them, down to the first record
        // (a heading, its column headings and one row move to a new page together).
        let keep = if matches!(b, Block::Heading { .. } | Block::Columns(_) | Block::SubColumns(_) | Block::Title(_)) {
            let mut k = 0.0;
            for next in doc.blocks.iter().skip(i + 1).take(6) {
                k += height(next, content_w, content_h).min(content_h / 2.0);
                if !matches!(next, Block::Heading { .. } | Block::Columns(_) | Block::SubColumns(_)) {
                    break;
                }
            }
            k.max(5.0)
        } else {
            0.0
        };
        if y + h + keep > bottom && !cur.is_empty() {
            flush(&mut pages, &mut cur, &mut y);
            // Rows carried onto a new page get their column headings again.
            if matches!(b, Block::Row(_) | Block::SubRow(_) | Block::SubColumns(_))
                && let Some(c) = columns
                && let Some(cb) = doc.blocks.get(c)
            {
                let ch = height(cb, content_w, content_h);
                cur.push(Placed { block: c, y, h: ch, lines: Vec::new() });
                y += ch;
            }
            if matches!(b, Block::SubRow(_))
                && let Some(c) = sub_columns
                && let Some(cb) = doc.blocks.get(c)
            {
                let ch = height(cb, content_w, content_h);
                cur.push(Placed { block: c, y, h: ch, lines: Vec::new() });
                y += ch;
            }
        }
        cur.push(Placed { block: i, y, h, lines: Vec::new() });
        y += h;
    }
    if !cur.is_empty() && pages.len() < MAX_PAGES {
        pages.push(Page::Flow(cur));
    }
    if pages.is_empty() {
        pages.push(Page::Flow(Vec::new()));
    }
    pages
}

/// Largest page drawn, in pixels (less in the browser): bigger pages are drawn at a lower dpi.
const MAX_PIXELS: f64 = if cfg!(target_arch = "wasm32") { 25_000_000.0 } else { 60_000_000.0 };

/// Draws page `n` (0-based) at `dpi`, returning it and its size in millimetres. `layouts` and
/// `maps` are the project's, for layout pages.
pub fn draw_page(doc: &Doc, pages: &[Page], n: usize, dpi: f64, layouts: &[Layout], maps: &mut [Map]) -> Option<(Pixmap, f64, f64)> {
    let page = pages.get(n)?;
    let dpi = if dpi.is_finite() { dpi.clamp(24.0, 600.0) } else { 96.0 };
    if let Page::Layout(li) = page {
        let l = layouts.get(*li)?;
        let info = crate::layout::PageInfo { page: n + 1, pages: pages.len(), page_name: String::new() };
        return crate::layout::render(l, maps, dpi, &info).map(|pm| (pm, l.width, l.height));
    }
    flow_canvas(doc, pages, n, dpi, false).map(|(pm, w, h)| (pm.into_pixmap(), w, h))
}

/// Page `n` as a vector PDF page: a layout's as [`crate::layout::render_drawing`] keeps it, a
/// flowing page's text, bands and charts as vectors and its pictures as pictures at `dpi`; with
/// the names of a layout's frames drawn as pictures (see `render_drawing`).
pub fn pdf_page(
    doc: &Doc,
    pages: &[Page],
    n: usize,
    dpi: f64,
    layouts: &[Layout],
    maps: &mut [Map],
) -> Option<(crate::pdf::Page, Vec<String>)> {
    let page = pages.get(n)?;
    let dpi = if dpi.is_finite() { dpi.clamp(24.0, 600.0) } else { 96.0 };
    if let Page::Layout(li) = page {
        let l = layouts.get(*li)?;
        let info = crate::layout::PageInfo { page: n + 1, pages: pages.len(), page_name: String::new() };
        return crate::layout::render_drawing(l, maps, dpi, &info)
            .map(|(d, pictured)| (crate::pdf::Page::drawing(d, l.width, l.height), pictured));
    }
    let (pm, w, h) = flow_canvas(doc, pages, n, dpi, true)?;
    pm.into_drawing().map(|d| (crate::pdf::Page::drawing(d, w, h), Vec::new()))
}

/// Flowing page `n` on a canvas `dpi` dots an inch (recording when `record`), and its size in
/// millimetres; `None` for a layout's page.
fn flow_canvas(doc: &Doc, pages: &[Page], n: usize, dpi: f64, record: bool) -> Option<(crate::Canvas, f64, f64)> {
    let Some(Page::Flow(items)) = pages.get(n) else { return None };
    let (wmm, hmm) = (doc.width.clamp(10.0, 5000.0), doc.height.clamp(10.0, 5000.0));
    let mut k = dpi / 25.4; // pixels per millimetre
    let px = wmm * k * hmm * k;
    if px > MAX_PIXELS {
        k *= (MAX_PIXELS / px).sqrt();
    }
    let (pw, ph) = ((wmm * k).round().max(1.0) as u32, (hmm * k).round().max(1.0) as u32);
    let mut pm = if record { crate::Canvas::recording(pw, ph)? } else { crate::Canvas::new(pw, ph)? };
    pm.fill(tiny_skia::Color::WHITE);
    let content_w = (wmm - 2.0 * MARGIN).max(10.0);
    let content_h = ((hmm - MARGIN - FOOTER).max(MARGIN + 20.0)) - MARGIN;
    let kf = k as f32;
    let at = |mm: f64| (mm * k) as f32;
    let pt_px = |pt: f32| pt * (k * 25.4 / 72.0) as f32;
    // Fits text into `w` millimetres, ending it with an ellipsis when cut.
    let fit = |s: &str, w: f64, pt: f32, bold: bool| -> String {
        if width_mm(s, pt, bold) <= w {
            return s.to_string();
        }
        let mut t: String = s.chars().take(200).collect();
        while !t.is_empty() && width_mm(&format!("{t}…"), pt, bold) > w {
            t.pop();
        }
        format!("{t}…")
    };
    for it in items {
        let Some(b) = doc.blocks.get(it.block) else { continue };
        let (pt, bold, lh) = style(b);
        let mid = |row: f64| at(it.y + row * lh + lh / 2.0);
        match b {
            Block::Title(t) => {
                text::text_at(&mut pm, &fit(t, content_w, pt, true), at(MARGIN), mid(0.0), pt_px(pt), DARK, true, text::Align::Left, false);
                if let Some(r) = Rect::from_xywh(at(MARGIN), at(it.y + it.h - 1.5), at(content_w), (0.6 * k) as f32) {
                    pm.fill_rect(r, &paint(ACCENT, 1.0), Transform::identity(), None);
                }
            }
            Block::Heading { level, text: t } => {
                let x = MARGIN + *level as f64 * 4.0;
                let color = if *level == 0 { ACCENT } else { DARK };
                text::text_at(&mut pm, &fit(t, content_w, pt, true), at(x), mid(0.0), pt_px(pt), color, bold, text::Align::Left, false);
            }
            Block::Columns(cols) | Block::Row(cols) | Block::SubColumns(cols) | Block::SubRow(cols) => {
                let sub = matches!(b, Block::SubColumns(_) | Block::SubRow(_));
                let (x0, w) = if sub { (MARGIN + INDENT, content_w - INDENT) } else { (MARGIN, content_w) };
                if matches!(b, Block::Columns(_) | Block::SubColumns(_))
                    && let Some(r) = Rect::from_xywh(at(x0), at(it.y), at(w), at(it.h))
                {
                    pm.fill_rect(r, &paint(BAND, if sub { 0.6 } else { 1.0 }), Transform::identity(), None);
                }
                let n = cols.len().max(1);
                let cw = w / n as f64;
                for (c, v) in cols.iter().enumerate() {
                    let x = x0 + cw * c as f64 + 1.0;
                    text::text_at(&mut pm, &fit(v, cw - 2.0, pt, bold), at(x), mid(0.0), pt_px(pt), DARK, bold, text::Align::Left, false);
                }
                if matches!(b, Block::Row(_))
                    && let Some(r) = Rect::from_xywh(at(x0), at(it.y + it.h) - 0.5 * kf.min(2.0), at(w), (0.2 * k).max(1.0) as f32)
                {
                    pm.fill_rect(r, &paint(BAND, 1.0), Transform::identity(), None);
                }
            }
            Block::Stats(_) => {
                let lines = &it.lines;
                for (row, l) in lines.iter().enumerate() {
                    text::text_at(
                        &mut pm,
                        &fit(l, content_w, pt, false),
                        at(MARGIN + 2.0),
                        mid(row as f64),
                        pt_px(pt),
                        DARK,
                        false,
                        text::Align::Left,
                        false,
                    );
                }
                if let Some(r) = Rect::from_xywh(at(MARGIN), at(it.y), (0.6 * k) as f32, at(lh * lines.len().max(1) as f64)) {
                    pm.fill_rect(r, &paint(ACCENT, 1.0), Transform::identity(), None);
                }
            }
            Block::Attachments(_) => {
                for (row, l) in it.lines.iter().enumerate() {
                    // The heading line in grey and bold, then the names.
                    let head = l == ATTACHMENTS;
                    text::text_at(
                        &mut pm,
                        &fit(l, content_w - INDENT, pt, head),
                        at(MARGIN + INDENT),
                        mid(row as f64),
                        pt_px(pt),
                        if head { DIM } else { DARK },
                        head,
                        text::Align::Left,
                        false,
                    );
                }
            }
            Block::Picture { image, width, height } => {
                let (w, h) = picture_box(*width, *height, content_w, content_h);
                if let Some(img) = crate::picture::decode(image) {
                    let (bw, bh) = (at(w), at(h));
                    let t = Transform::from_row(
                        bw / img.width() as f32,
                        0.0,
                        0.0,
                        bh / img.height() as f32,
                        at(MARGIN + INDENT),
                        at(it.y + 1.0),
                    );
                    let paint = PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() };
                    pm.draw_pixmap(0, 0, img.as_ref(), &paint, t, None);
                }
            }
            Block::Chart { title, bars } => {
                text::text_at(
                    &mut pm,
                    &fit(title, content_w, 10.0, true),
                    at(MARGIN),
                    at(it.y + 4.0),
                    pt_px(10.0),
                    DARK,
                    true,
                    text::Align::Left,
                    false,
                );
                let max = bars.iter().map(|b| b.1).fold(0.0_f64, f64::max);
                let label_w = content_w * 0.3;
                let bar_w = content_w - label_w - 22.0;
                for (row, (name, v)) in bars.iter().take(chart_bars(bars.len(), content_h)).enumerate() {
                    let y = it.y + 8.0 + 5.0 * row as f64;
                    text::text_at(
                        &mut pm,
                        &fit(name, label_w - 2.0, 8.0, false),
                        at(MARGIN),
                        at(y + 2.5),
                        pt_px(8.0),
                        DARK,
                        false,
                        text::Align::Left,
                        false,
                    );
                    let w = if max > 0.0 { (bar_w * (v.max(0.0) / max)).max(0.3) } else { 0.3 };
                    if let Some(r) = Rect::from_xywh(at(MARGIN + label_w), at(y + 0.8), at(w), at(3.4)) {
                        pm.fill_rect(r, &paint(ACCENT, 1.0), Transform::identity(), None);
                    }
                    let num = rustgis_map::symbology::fmt_num(*v);
                    text::text_at(
                        &mut pm,
                        &num,
                        at(MARGIN + label_w + w + 1.5),
                        at(y + 2.5),
                        pt_px(7.5),
                        DIM,
                        false,
                        text::Align::Left,
                        false,
                    );
                }
            }
            Block::Text(_) => {
                for (row, l) in it.lines.iter().enumerate() {
                    text::text_at(&mut pm, l, at(MARGIN), mid(row as f64), pt_px(pt), DARK, false, text::Align::Left, false);
                }
            }
            Block::PageBreak | Block::Layout(_) => {}
        }
    }
    // Footer: the title and the page number.
    let fy = at(hmm - MARGIN + 2.0);
    text::text_at(&mut pm, &fit(&doc.title, content_w * 0.6, 7.5, false), at(MARGIN), fy, pt_px(7.5), DIM, false, text::Align::Left, false);
    let num = format!("Page {} of {}", n + 1, pages.len());
    let nw = width_mm(&num, 7.5, false);
    text::text_at(&mut pm, &num, at(MARGIN + content_w - nw), fy, pt_px(7.5), DIM, false, text::Align::Left, false);
    Some((pm, wmm, hmm))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(rows: usize) -> Doc {
        let mut blocks = vec![Block::Title("Cities".into()), Block::Heading { level: 0, text: "Region: West".into() }];
        blocks.push(Block::Columns(vec!["name".into(), "pop".into()]));
        for i in 0..rows {
            blocks.push(Block::Row(vec![format!("City {i}"), i.to_string()]));
        }
        blocks.push(Block::Stats(vec!["Count: 3".into()]));
        blocks.push(Block::Text("word ".repeat(3000)));
        blocks.push(Block::Chart { title: "Records per region".into(), bars: vec![("West".into(), 3.0), ("East".into(), 1.0)] });
        Doc { title: "Cities".into(), width: 215.9, height: 279.4, blocks }
    }

    /// A word wider than the line breaks into pieces that each fit, nothing lost, found in a few
    /// measures a piece rather than one a character (a hostile 2000-character word stays cheap).
    #[test]
    fn a_long_word_breaks_in_few_measures() {
        let word = "x".repeat(2000);
        let count = std::cell::Cell::new(0usize);
        let lines = wrap_with(&word, 40.0, |t| {
            count.set(count.get() + 1);
            width_mm(t, 7.0, false)
        });
        assert!(lines.len() > 10, "{} lines", lines.len());
        assert!(lines.iter().all(|l| width_mm(l, 7.0, false) <= 40.0), "every piece fits");
        assert_eq!(lines.concat(), word, "nothing lost");
        assert!(count.get() < 20 * lines.len(), "{} measures for {} lines", count.get(), lines.len());
    }

    #[test]
    fn flows_onto_pages_with_repeated_column_headings() {
        let d = doc(120);
        let pages = paginate(&d);
        assert!(pages.len() >= 3, "{} pages", pages.len());
        // Every page that holds rows starts with the column headings.
        for p in &pages {
            let Page::Flow(items) = p else { panic!() };
            let first_row = items.iter().position(|i| matches!(d.blocks[i.block], Block::Row(_)));
            if let Some(r) = first_row {
                assert!(items[..r].iter().any(|i| matches!(d.blocks[i.block], Block::Columns(_))));
            }
            // Nothing runs into the footer.
            assert!(items.iter().all(|i| i.y + i.h <= d.height - MARGIN - FOOTER + 1e-6));
        }
        // The long text was split across pages, with every word kept.
        let words: usize = pages
            .iter()
            .flat_map(|p| if let Page::Flow(items) = p { items.clone() } else { Vec::new() })
            .filter(|i| matches!(d.blocks[i.block], Block::Text(_)))
            .flat_map(|i| i.lines)
            .map(|l| l.split_whitespace().count())
            .sum();
        assert_eq!(words, 3000);
        let (pm, w, _) = draw_page(&d, &pages, 0, 72.0, &[], &mut []).unwrap();
        assert_eq!(w, 215.9);
        assert!(pm.pixels().iter().any(|p| p.red() < 100), "the page has text");
        assert!(draw_page(&d, &pages, 10_000, 72.0, &[], &mut []).is_none());
    }

    #[test]
    fn hostile_docs_stay_bounded() {
        let d = Doc { title: String::new(), width: f64::NAN, height: -5.0, blocks: vec![Block::Text("x".into()); 50] };
        let pages = paginate(&d);
        assert!(!pages.is_empty() && pages.len() <= MAX_PAGES);
        let d = Doc { title: "Empty".into(), width: 100.0, height: 100.0, blocks: Vec::new() };
        assert_eq!(paginate(&d).len(), 1);
        assert!(draw_page(&d, &paginate(&d), 0, 1e9, &[], &mut []).is_some());
    }

    #[test]
    fn long_statistics_and_attachments_split_between_pages() {
        let blocks = vec![
            Block::Stats((0..120).map(|i| format!("Sum of f{i}: {i}")).collect()),
            Block::Attachments((0..90).map(|i| format!("file{i}.pdf")).collect()),
        ];
        let d = Doc { title: "Long".into(), width: 150.0, height: 120.0, blocks };
        let pages = paginate(&d);
        assert!(pages.len() > 3);
        let mut lines = 0;
        for p in &pages {
            let Page::Flow(items) = p else { panic!() };
            assert!(items.iter().all(|i| i.y + i.h <= d.height - MARGIN - FOOTER + 1e-6), "nothing runs into the footer");
            lines += items.iter().map(|i| i.lines.len()).sum::<usize>();
        }
        assert_eq!(lines, 120 + 91, "every statistic, the heading and every attachment");
    }

    #[test]
    fn headings_stay_with_their_rows_and_tall_blocks_fit() {
        // Rows fill the page until a heading would land at its bottom.
        let mut blocks: Vec<Block> = (0..39).map(|i| Block::Row(vec![format!("r{i}")])).collect();
        blocks.push(Block::Heading { level: 0, text: "Group".into() });
        blocks.push(Block::Columns(vec!["name".into()]));
        blocks.push(Block::Row(vec!["first".into()]));
        blocks.push(Block::Chart { title: "Big".into(), bars: (0..25).map(|i| (format!("b{i}"), i as f64)).collect() });
        blocks.push(Block::Text("x".repeat(400)));
        let d = Doc { title: "T".into(), width: 150.0, height: 220.0, blocks };
        let pages = paginate(&d);
        for p in &pages {
            let Page::Flow(items) = p else { panic!() };
            // A heading is never the last thing on a page.
            if let Some(last) = items.last() {
                assert!(!matches!(d.blocks[last.block], Block::Heading { .. } | Block::Columns(_)));
            }
            assert!(items.iter().all(|i| i.y + i.h <= d.height - MARGIN - FOOTER + 1e-6), "nothing runs into the footer");
        }
        // The long word was broken into lines no wider than the page.
        let text_lines: Vec<String> = pages
            .iter()
            .flat_map(|p| if let Page::Flow(items) = p { items.clone() } else { Vec::new() })
            .filter(|i| matches!(d.blocks[i.block], Block::Text(_)))
            .flat_map(|i| i.lines)
            .collect();
        assert!(text_lines.len() > 1 && text_lines.iter().all(|l| width_mm(l, 9.0, false) <= 150.0 - 2.0 * MARGIN + 1e-6));
        assert_eq!(text_lines.concat().len(), 400);
    }
}
