//! Publish › Presentation (New and Import Presentation), Publish › Page (Map, Blank, Image, Video), and
//! Publish › Save As (Presentation File) and Output (Export Presentation): presentations.

use std::path::Path;

use base64::Engine;
use rustgis_geom::Coord;
use rustgis_map::Rgba;
use rustgis_map::layout::{ElementKind, FrameShape, Layout};
use rustgis_map::presentation::{MAX_CAPTION, MAX_PAGES, MAX_SOURCE, MAX_TEXT, MAX_TITLE, Page, PageKind, Presentation};
use serde_json::{Value, json};

use super::{f, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// Presentation files: JSON with this extension.
pub const EXTENSION: &str = "rgispres";
/// Largest export held in memory (less in the browser).
const BUDGET: usize = if cfg!(target_arch = "wasm32") { 256 << 20 } else { 1 << 30 };

fn need_presentation(s: &Session) -> Option<&'static str> {
    if s.project.presentations.is_empty() { Some("add a presentation first (Publish › New Presentation)") } else { None }
}

/// Registers presentation actions, with public generated video labels and guarded outputs.
pub fn commands() -> Vec<C> {
    vec![
        C::new("presentation.new", "New Presentation", "Publish › Presentation", |s, p| {
            let name = unique_name(s, ps(p, "name").unwrap_or("Presentation"));
            let mut pres = Presentation { name: name.clone(), ..Presentation::default() };
            // A title page to start from.
            let title: String = ps(p, "title").unwrap_or(&s.project.name).chars().take(MAX_TITLE).collect();
            pres.pages.push(Page {
                title,
                kind: PageKind::Blank { text: String::new(), background: Rgba::rgb(0x1F, 0x3A, 0x5F) },
                notes: String::new(),
                seconds: 5.0,
            });
            if s.project.presentations.len() >= crate::project::MAX_ITEMS {
                return Err(format!("a project holds at most {} presentations", crate::project::MAX_ITEMS));
            }
            s.project.presentations.push(pres);
            let i = s.project.presentations.len() - 1;
            s.active_presentation = Some(i);
            Ok(json!({"presentation": i, "name": name}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name"?, "title"?: the title page's text}"#),
        C::new("presentation.import", "Import Presentation", "Publish › Presentation", |s, p| {
            let text = super::items::read_text(p)?;
            let mut pres: Presentation = serde_json::from_str(&text).map_err(|e| format!("not a RustGIS presentation: {e}"))?;
            let repairs = pres.clamp();
            pres.name = unique_name(s, &pres.name);
            let name = pres.name.clone();
            if s.project.presentations.len() >= crate::project::MAX_ITEMS {
                return Err(format!("a project holds at most {} presentations", crate::project::MAX_ITEMS));
            }
            s.project.presentations.push(pres);
            let i = s.project.presentations.len() - 1;
            s.active_presentation = Some(i);
            for notice in crate::project::content_notices(repairs, &name) {
                s.notify_text(notice, false);
            }
            Ok(json!({"presentation": i, "name": name}))
        }).roles(&[crate::ParamRole { key: "text", role: crate::Role::Document(crate::Document::Presentation) }]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .params(r#"{"path"?: "*.rgispres", "text"?: presentation JSON, "data"?: base64 file}"#),
        C::new("presentation.activate", "Open Presentation", "Browser", |s, p| {
            let i = index(s, p)?;
            s.active_presentation = Some(i);
            Ok(json!({"presentation": i}))
        }).inputs(object_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .when(need_presentation)
        .params(r#"{"presentation": name|index}"#),
        C::new("presentation.remove", "Delete Presentation", "Browser", |s, p| {
            let i = index(s, p)?;
            s.project.presentations.remove(i);
            s.active_presentation = None;
            Ok(Value::Null)
        }).inputs(object_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_presentation)
        .params(r#"{"presentation"?}"#),
        C::new("presentation.mapPage", "Map Page", "Publish › Page", |s, p| {
            let map = map_page_source(s,p)?;
            let name = map.name.clone();
            // The map view as it is now, unless a place is given.
            let c = map.center_lonlat();
            let center = Coord::new(f(p, "lon").unwrap_or(c.x).clamp(-180.0, 180.0), f(p, "lat").unwrap_or(c.y).clamp(-85.0, 85.0));
            let width_m = f(p, "widthM").unwrap_or(s.view_size.0 * map.ground_res()).clamp(10.0, 4e7);
            let title = ps(p, "title").unwrap_or(&name).to_string();
            add_page(s, p, title, PageKind::Map { map: name, center, width_m })
        }).inputs(map_page_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_presentation)
        .params(r#"{"presentation"?, "map"?: name (default: the active map, at its current view), "lon"?, "lat"?, "widthM"?: ground width shown (m), "title"?, "at"?: page index}"#),
        C::new("presentation.blankPage", "Blank Page", "Publish › Page", |s, p| {
            let background = super::layout::color(p, "background", Rgba::rgb(0xFF, 0xFF, 0xFF))?;
            let text: String = ps(p, "text").unwrap_or("").chars().take(MAX_TEXT).collect();
            add_page(s, p, ps(p, "title").unwrap_or("").to_string(), PageKind::Blank { text, background })
        }).inputs(object_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_presentation)
        .params(r##"{"presentation"?, "title"?, "text"?, "background"?: "#RRGGBB"|"#RRGGBBAA", "at"?}"##),
        C::new("presentation.imagePage", "Image Page", "Publish › Page", |s, p| {
            // A picture opened in the browser arrives as base64 `data`.
            let image = match ps(p, "data") {
                Some(d) => super::graphics::picture(&json!({"image": d}))?,
                None => super::graphics::picture(p)?,
            };
            let caption: String = ps(p, "caption").unwrap_or("").chars().take(MAX_CAPTION).collect();
            add_page(s, p, ps(p, "title").unwrap_or("").to_string(), PageKind::Image { image, caption })
        }).inputs(object_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .when(need_presentation)
        .params(r#"{"presentation"?, "path"?: PNG/JPEG file, "image"?: base64, "data"?: base64, "title"?, "caption"?, "at"?}"#),
        C::new("presentation.videoPage", "Video Page", "Publish › Page", |s, p| {
            let source: String = ps(p, "source").map(str::trim).filter(|v| !v.is_empty()).ok_or("source is required (a video file or URL)")?.chars().take(MAX_SOURCE).collect();
            let poster = match (ps(p, "poster"), ps(p, "posterImage")) {
                (Some(path), _) => Some(super::graphics::picture(&json!({"path": path}))?),
                (None, Some(b64)) => Some(super::graphics::picture(&json!({"image": b64}))?),
                (None, None) => None,
            };
            let caption: String = ps(p, "caption").unwrap_or("").chars().take(MAX_CAPTION).collect();
            let title = ps(p, "title").map(str::to_string).unwrap_or_else(|| video_label(&source));
            add_page(s, p, title, PageKind::Video { source, poster, caption })
        }).inputs(object_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .when(need_presentation)
        .params(r#"{"presentation"?, "source": video file or URL, "poster"?: PNG/JPEG file, "posterImage"?: base64, "title"?, "caption"?, "at"?}"#),
        C::new("presentation.updatePage", "Page Properties", "Presentation", update_page).inputs(page_inputs).roles(&[]).effects(crate::policy::Effects::NONE).when(need_presentation).params(
            r##"{"presentation"?, "page": index, "title"?, "text"?, "caption"?, "notes"?, "seconds"?, "background"?: "#RRGGBB"|"#RRGGBBAA", "moveTo"?: index}"##,
        ),
        C::new("presentation.removePage", "Delete Page", "Presentation", |s, p| {
            let (i, k) = page_of(s, p)?;
            let pres = s.project.presentations.get_mut(i).ok_or("no presentation")?;
            pres.pages.remove(k);
            Ok(json!({"pages": pres.pages.len()}))
        }).inputs(page_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_presentation)
        .params(r#"{"presentation"?, "page": index}"#),
        C::new("presentation.save", "Presentation File", "Publish › Save As", |s, p| {
            let path = ps(p, "path").ok_or("path is required")?;
            let i = index(s, p)?;
            let bytes = presentation_file(s, i)?;
            rustgis_data::io::write_atomic(Path::new(path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path}))
        }).inputs(object_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .view()
        .when(need_presentation)
        .params(r#"{"presentation"?, "path": "*.rgispres"}"#),
        C::new("presentation.export", "Export Presentation", "Publish › Output", |s, p| {
            let path = ps(p, "path").ok_or("path is required")?.to_string();
            let i = index(s, p)?;
            let dpi = f(p, "dpi").unwrap_or(120.0).clamp(48.0, 300.0);
            let lower = path.to_ascii_lowercase();
            let format = if lower.ends_with(".html") || lower.ends_with(".htm") {
                "html"
            } else if lower.ends_with(".png") {
                "png"
            } else if lower.ends_with(".pdf") {
                "pdf"
            } else {
                return Err("export to a .pdf, .html or .png file".into());
            };
            let (bytes, pages) = export_bytes(s, i, format, dpi)?;
            rustgis_data::io::write_atomic(Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path, "pages": pages, "bytes": bytes.len()}))
        }).inputs(export_inputs).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .view()
        .when(need_presentation)
        .params(r#"{"presentation"?, "path": "*.pdf|*.html (a self-contained slideshow, videos playable)|*.png (the first page)", "dpi"?: 48..300}"#),
    ]
}

/// Derives generated labels from the public address before extracting a path or host. User
/// titles/captions and local filenames (including '?' and '#') are data and retain their bytes.
pub(crate) fn video_label(source: &str) -> String {
    use crate::durable::{Kept, public_address};
    let clean = match public_address(source) {
        Kept::Unchanged => source.to_string(),
        Kept::Changed(s) => s,
        Kept::Withheld(_) => return "Video".into(),
    };
    let inner = crate::sources::template_archive(&clean).map_or(clean.as_str(), |(_, a)| a);
    let inner = rustgis_data::s3_address::parse(inner).ok().flatten().map_or(inner, |b| b.url);
    let inner = inner.strip_prefix(crate::render::rustgis_tiles::s3::ANONYMOUS).unwrap_or(inner);
    let remote = inner.split_once("://").filter(|(s, _)| ["http", "https", "s3"].iter().any(|p| p.eq_ignore_ascii_case(s)));
    let name = if let Some((_, rest)) = remote {
        let rest = rest.split(['?', '#']).next().unwrap_or("");
        rest.rsplit('/').find(|s| !s.is_empty()).unwrap_or("Video")
    } else {
        inner.rsplit(['/', '\\']).next().filter(|s| !s.is_empty()).unwrap_or(inner)
    };
    name.chars().take(200).collect()
}

/// The presentation named or numbered by `presentation`, else the active one, else the last.
fn index_input(input: &impl super::InputSource, p: &Value) -> Result<usize, String> {
    let context = input.input_context();
    let s = &context;
    let ps_ = &s.project.presentations;
    match p.get("presentation") {
        Some(Value::Number(n)) => {
            n.as_u64().and_then(|n| usize::try_from(n).ok()).filter(|i| *i < ps_.len()).ok_or_else(|| format!("no presentation {n}"))
        }
        Some(Value::String(n)) => ps_
            .iter()
            .position(|r| r.name.eq_ignore_ascii_case(n))
            .or_else(|| n.parse::<usize>().ok().filter(|i| *i < ps_.len()))
            .ok_or_else(|| format!("no presentation named '{n}'")),
        _ => s
            .active_presentation
            .filter(|i| *i < ps_.len())
            .or(ps_.len().checked_sub(1))
            .ok_or_else(|| "no presentations yet: add one with presentation.new".to_string()),
    }
}

fn page_of(input: &impl super::InputSource, p: &Value) -> Result<(usize, usize), String> {
    let context = input.input_context();
    let s = &context;
    let i = index_input(input, p)?;
    let n = s.project.presentations.get(i).map_or(0, |x| x.pages.len());
    let k = p.get("page").and_then(Value::as_u64).and_then(|k| usize::try_from(k).ok()).ok_or("page is required")?;
    if k >= n {
        return Err(format!("no page {k}"));
    }
    Ok((i, k))
}

fn unique_name(s: &Session, base: &str) -> String {
    let base: String = base.trim().chars().take(200).collect();
    let base = if base.is_empty() { "Presentation".to_string() } else { base };
    let taken = |n: &str| s.project.presentations.iter().any(|r| r.name.eq_ignore_ascii_case(n));
    let mut name = base.clone();
    let mut k = 1;
    while taken(&name) {
        k += 1;
        name = format!("{base} {k}");
    }
    name
}

fn add_page(s: &mut Session, p: &Value, title: String, kind: PageKind) -> CmdResult {
    let i = index(s, p)?;
    let pres = s.project.presentations.get_mut(i).ok_or("no presentation")?;
    if pres.pages.len() >= MAX_PAGES {
        return Err(format!("presentations hold at most {MAX_PAGES} pages"));
    }
    let at = p.get("at").and_then(Value::as_u64).and_then(|k| usize::try_from(k).ok()).unwrap_or(pres.pages.len()).min(pres.pages.len());
    pres.pages.insert(at, Page { title: title.chars().take(MAX_TITLE).collect(), kind, notes: String::new(), seconds: 5.0 });
    Ok(json!({"presentation": i, "page": at, "pages": pres.pages.len()}))
}

fn update_page(s: &mut Session, p: &Value) -> CmdResult {
    let (i, k) = page_of(s, p)?;
    let background = match ps(p, "background") {
        Some(_) => Some(super::layout::color(p, "background", Rgba::rgb(0xFF, 0xFF, 0xFF))?),
        None => None,
    };
    let pres = s.project.presentations.get_mut(i).ok_or("no presentation")?;
    let n = pres.pages.len();
    let page = pres.pages.get_mut(k).ok_or("no page")?;
    if let Some(t) = ps(p, "title") {
        page.title = t.chars().take(MAX_TITLE).collect();
    }
    if let Some(t) = ps(p, "notes") {
        page.notes = t.chars().take(MAX_TEXT).collect();
    }
    if let Some(v) = f(p, "seconds") {
        page.seconds = v.clamp(0.5, 3600.0);
    }
    match &mut page.kind {
        PageKind::Blank { text, background: bg } => {
            if let Some(t) = ps(p, "text") {
                *text = t.chars().take(MAX_TEXT).collect();
            }
            if let Some(c) = background {
                *bg = c;
            }
        }
        PageKind::Image { caption, .. } | PageKind::Video { caption, .. } => {
            if let Some(t) = ps(p, "caption") {
                *caption = t.chars().take(MAX_CAPTION).collect();
            }
        }
        PageKind::Map { .. } => {}
    }
    let mut at = k;
    if let Some(to) = p.get("moveTo").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()) {
        at = to.min(n - 1);
        let page = pres.pages.remove(k);
        pres.pages.insert(at, page);
    }
    Ok(json!({"page": at}))
}

/// A presentation file.
pub fn presentation_file(s: &mut Session, i: usize) -> Result<Vec<u8>, String> {
    let pres = s.project.presentations.get_mut(i).ok_or("no presentation")?;
    let (bytes, report) = crate::durable::output(pres, None, |p| serde_json::to_vec_pretty(p).map_err(|e| e.to_string()))?;
    crate::durable::notify(s, &report);
    Ok(bytes)
}

/// A page drawn as a page layout: its title across the top and its content below.
pub fn page_layout(s: &Session, pres: &Presentation, page: &Page) -> Layout {
    let (w, h) = (pres.width, pres.height);
    let mut l = Layout::new(page.title.clone(), w, h);
    let m = w * 0.04;
    let dark = Rgba::rgb(0x1F, 0x29, 0x37);
    let has_title = !page.title.trim().is_empty();
    let top = if has_title { m + h * 0.14 } else { m };
    let text = |text: &str, size: f64, color: Rgba, bold: bool| ElementKind::text(text, size, color, bold);
    match &page.kind {
        PageKind::Blank { text: body, background } => {
            l.add(
                "Background",
                (0.0, 0.0, w, h),
                ElementKind::Rectangle { fill: *background, outline: *background, width: 0.0, radius: 0.0 },
            );
            // Light text on dark pages.
            let lum = 0.299 * background.r() as f64 + 0.587 * background.g() as f64 + 0.114 * background.b() as f64;
            let fg = if lum < 140.0 { Rgba::rgb(0xFF, 0xFF, 0xFF) } else { dark };
            if body.trim().is_empty() && has_title {
                // A title page: the title large, in the middle.
                l.add("Title", (m, h * 0.38, w - 2.0 * m, h * 0.2), text(&page.title, 44.0, fg, true));
                return l;
            }
            if has_title {
                l.add("Title", (m, m, w - 2.0 * m, h * 0.12), text(&page.title, 30.0, fg, true));
            }
            l.add("Text", (m, top, w - 2.0 * m, h - top - m), text(body, 20.0, fg, false));
        }
        PageKind::Map { map, center, width_m } => {
            if has_title {
                l.add("Title", (m, m, w - 2.0 * m, h * 0.12), text(&page.title, 30.0, dark, true));
            }
            let (fw, fh) = (w - 2.0 * m, h - top - m);
            let map = page_map(s, map).name.clone();
            let scale = (width_m / (fw / 1000.0)).clamp(rustgis_map::layout::MIN_FRAME_SCALE, rustgis_map::layout::MAX_FRAME_SCALE);
            l.add(
                "Map",
                (m, top, fw, fh),
                ElementKind::MapFrame {
                    map,
                    center: *center,
                    scale,
                    shape: FrameShape::Rectangle,
                    grid: None,
                    extent_of: None,
                    extent_color: rustgis_map::layout::INDICATOR_COLOR,
                    extent_width: rustgis_map::layout::INDICATOR_WIDTH,
                    bearing: 0.0,
                    pitch: 0.0,
                },
            );
        }
        PageKind::Image { image, caption } | PageKind::Video { poster: Some(image), caption, .. } => {
            if has_title {
                l.add("Title", (m, m, w - 2.0 * m, h * 0.12), text(&page.title, 30.0, dark, true));
            }
            let cap_h = if caption.trim().is_empty() { 0.0 } else { h * 0.08 };
            let (bw, bh) = (w - 2.0 * m, h - top - m - cap_h);
            let (pw, ph) = rustgis_render::picture_size(image).map_or((bw, bh), |(iw, ih)| {
                let k = (bw / iw.max(1) as f64).min(bh / ih.max(1) as f64);
                (iw as f64 * k, ih as f64 * k)
            });
            l.add("Picture", (m + (bw - pw) / 2.0, top + (bh - ph) / 2.0, pw, ph), ElementKind::Picture { image: image.clone() });
            if let PageKind::Video { source, .. } = &page.kind {
                l.add(
                    "Play",
                    (m + bw * 0.3, top + bh / 2.0 - 6.0, bw * 0.4, 12.0),
                    text(&format!("Video: {}", video_label(source)), 18.0, dark, true),
                );
            }
            if cap_h > 0.0 {
                l.add("Caption", (m, h - m - cap_h, w - 2.0 * m, cap_h), text(caption, 15.0, Rgba::rgb(0x55, 0x5B, 0x66), false));
            }
        }
        PageKind::Video { source, poster: None, caption } => {
            if has_title {
                l.add("Title", (m, m, w - 2.0 * m, h * 0.12), text(&page.title, 30.0, dark, true));
            }
            let (bw, bh) = (w - 2.0 * m, h - top - m - h * 0.08);
            let bg = Rgba::rgb(0x11, 0x18, 0x27);
            l.add("Video", (m, top, bw, bh), ElementKind::Rectangle { fill: bg, outline: bg, width: 0.0, radius: 0.0 });
            l.add(
                "Play",
                (m + bw * 0.3, top + bh / 2.0 - 6.0, bw * 0.4, 12.0),
                text(&format!("Video: {}", video_label(source)), 20.0, Rgba::rgb(0xFF, 0xFF, 0xFF), true),
            );
            if !caption.trim().is_empty() {
                l.add("Caption", (m, h - m - h * 0.07, w - 2.0 * m, h * 0.07), text(caption, 15.0, Rgba::rgb(0x55, 0x5B, 0x66), false));
            }
        }
    }
    l
}

/// Draws page `k` at `dpi`.
pub fn draw_page(s: &mut Session, i: usize, k: usize, dpi: f64) -> Result<rustgis_render::Pixmap, String> {
    let pres = s.project.presentations.get(i).ok_or("no presentation")?;
    let n = pres.pages.len();
    let page = pres.pages.get(k).ok_or_else(|| format!("no page {k}"))?;
    let layout = page_layout(s, pres, page);
    let info = rustgis_render::layout::PageInfo { page: k + 1, pages: n, page_name: page.title.clone() };
    rustgis_render::layout::render(&layout, &mut s.project.maps, dpi, &info)
}

/// Page `k` of presentation `i` as a vector PDF page at `dpi` (see
/// [`rustgis_render::layout::render_drawing`]), and the frames on it drawn as pictures.
fn pdf_page(
    s: &mut Session,
    i: usize,
    k: usize,
    dpi: f64,
    (width, height): (f64, f64),
) -> Result<(rustgis_render::pdf::Page, Vec<(u32, String)>), String> {
    let pres = s.project.presentations.get(i).ok_or("no presentation")?;
    let n = pres.pages.len();
    let page = pres.pages.get(k).ok_or_else(|| format!("no page {k}"))?;
    let layout = page_layout(s, pres, page);
    let info = rustgis_render::layout::PageInfo { page: k + 1, pages: n, page_name: page.title.clone() };
    let (drawing, pictured) = rustgis_render::layout::render_drawing(&layout, &mut s.project.maps, dpi, &info)?;
    Ok((rustgis_render::pdf::Page::drawing(drawing, width, height), pictured))
}

fn html_escape(t: &str) -> String {
    t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A video source the slideshow may play: a web address (http or https) or a file path. Any
/// other scheme (an imported file's author's choice) plays nothing.
pub(crate) fn playable(source: &str) -> &str {
    let lower = source.trim().to_ascii_lowercase();
    let scheme = lower.split_once(':').map(|(s, _)| s).filter(|s| !s.contains(['/', '\\']));
    match scheme {
        // A Windows drive letter is a path.
        Some(s) if s.len() > 1 && s != "http" && s != "https" => "",
        _ => source,
    }
}

/// The presentation as a PDF, a self-contained HTML slideshow or a PNG of its first page, and
/// the number of pages written.
pub fn export_bytes(s: &mut Session, i: usize, format: &str, dpi: f64) -> Result<(Vec<u8>, usize), String> {
    let revision = |s: &Session| {
        rustgis_render::archive_revisions(s.project.presentations.get(i).into_iter().flat_map(|p| {
            p.pages
                .iter()
                .take(if format == "png" { 1 } else { usize::MAX })
                .filter_map(|page| if let PageKind::Map { map, .. } = &page.kind { Some(page_map(s, map)) } else { None })
        }))
    };
    let before = revision(s);
    let result = export_bytes_inner(s, i, format, dpi);
    before.finish_revision(revision(s), Some(result), "could not draw the presentation")?
}

fn export_bytes_inner(s: &mut Session, i: usize, format: &str, dpi: f64) -> Result<(Vec<u8>, usize), String> {
    // Only the size and name are copied: the pages can hold many megabytes of pictures, so each
    // is read in place after it's drawn.
    let (name, width, height, n) =
        s.project.presentations.get(i).map(|p| (p.name.clone(), p.width, p.height, p.pages.len())).ok_or("no presentation")?;
    if n == 0 {
        return Err("the presentation has no pages".into());
    }
    if format == "png" {
        let img = draw_page(s, i, 0, dpi)?;
        return Ok((img.encode_png().map_err(|e| e.to_string())?, 1));
    }
    let mut pdf_pages = Vec::new();
    let mut slides = String::new();
    let mut output_report = crate::durable::Report::default();
    let mut held = 0usize;
    let (mut frames_pictured, mut pages_pictured) = (Vec::new(), Vec::new());
    for k in 0..n {
        if format == "pdf" {
            let (page, pictured) = pdf_page(s, i, k, dpi, (width, height))?;
            if page.overflowed() {
                // Too large for vectors all the same: the page as one picture.
                pages_pictured.push(k + 1);
                pdf_pages.push(rustgis_render::pdf::Page::new(&draw_page(s, i, k, dpi)?, width, height));
            } else {
                frames_pictured.extend(pictured);
                pdf_pages.push(page);
            }
            held += pdf_pages.last().map_or(0, rustgis_render::pdf::Page::len);
        } else {
            let img = draw_page(s, i, k, dpi)?;
            let page = s.project.presentations.get(i).and_then(|p| p.pages.get(k)).ok_or("no page")?;
            let png = base64::engine::general_purpose::STANDARD.encode(img.encode_png().map_err(|e| e.to_string())?);
            held += png.len();
            let body = match &page.kind {
                // The poster is the drawn page, which already shows the title and caption. The
                // source is played as given (a file next to the slideshow or a web address).
                PageKind::Video { source, .. } => {
                    let source = match crate::durable::public_address(source) {
                        crate::durable::Kept::Unchanged => source.clone(),
                        crate::durable::Kept::Changed(s) => {
                            output_report.videos = output_report.videos.saturating_add(1);
                            s
                        }
                        crate::durable::Kept::Withheld(prefix) => {
                            output_report.videos = output_report.videos.saturating_add(1);
                            output_report.withheld = output_report.withheld.saturating_add(1);
                            prefix.into()
                        }
                    };
                    format!(
                        "<video controls preload=\"metadata\" title=\"{}\" poster=\"data:image/png;base64,{png}\" src=\"{}\"></video>",
                        html_escape(&page.title),
                        html_escape(playable(&source))
                    )
                }
                _ => format!("<img alt=\"{}\" src=\"data:image/png;base64,{png}\">", html_escape(&page.title)),
            };
            let notes = if page.notes.is_empty() { String::new() } else { format!("<aside>{}</aside>", html_escape(&page.notes)) };
            slides.push_str(&format!("<section data-seconds=\"{}\">{body}{notes}</section>\n", page.seconds));
        }
        if held > BUDGET {
            return Err(format!("the export passed {} MB at page {}; export at a lower dpi", BUDGET >> 20, k + 1));
        }
    }
    crate::durable::notify(s, &output_report);
    if format == "pdf" {
        super::layout::pictured_notices(s, &frames_pictured, &pages_pictured);
        return Ok((rustgis_render::pdf::write(&pdf_pages, &name), n));
    }
    let html = format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{title}</title>
<style>
html,body{{margin:0;height:100%;background:#111;color:#eee;font-family:system-ui,sans-serif}}
section{{display:none;height:100%;box-sizing:border-box;padding:2vh;flex-direction:column;align-items:center;justify-content:center}}
section.on{{display:flex}}
section img,section video{{max-width:100%;max-height:86vh;object-fit:contain;background:#fff}}
aside{{display:none}} body.notes aside{{display:block;max-width:80ch;opacity:.8}}
nav{{position:fixed;bottom:1vh;right:2vw;opacity:.6;font-size:14px}}
</style></head><body>
{slides}<nav><span id="n"></span> · ←/→ pages · N notes · P play</nav>
<script>
const s=[...document.querySelectorAll('section')];let i=0,t=null;
function go(k){{i=Math.max(0,Math.min(s.length-1,k));s.forEach((x,j)=>x.classList.toggle('on',j===i));document.getElementById('n').textContent=(i+1)+' / '+s.length;}}
function play(){{if(t){{clearTimeout(t);t=null;return}}const next=()=>{{if(i<s.length-1){{go(i+1);t=setTimeout(next,1000*s[i].dataset.seconds)}}else t=null}};t=setTimeout(next,1000*s[i].dataset.seconds)}}
document.addEventListener('keydown',e=>{{if(e.key==='ArrowRight'||e.key===' ')go(i+1);else if(e.key==='ArrowLeft')go(i-1);else if(e.key==='n'||e.key==='N')document.body.classList.toggle('notes');else if(e.key==='p'||e.key==='P')play()}});
document.addEventListener('click',e=>{{if(e.target.tagName!=='VIDEO')go(i+1)}});go(0);
</script></body></html>
"#,
        title = html_escape(&name)
    );
    Ok((html.into_bytes(), n))
}

/// Resolves a supplied string map or the active-map default used to initialize a page.
fn map_page_source<'a>(input: &'a impl super::InputSource, p: &Value) -> Result<&'a rustgis_map::Map, String> {
    let context = input.input_context();
    let s = &context;
    match ps(p, "map") {
        Some(name) => s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(name)).ok_or_else(|| format!("no map '{name}'")),
        None => Ok(s.map()),
    }
}
/// Stored page map names fall back to the active map when absent, matching rendering.
fn page_map<'a>(input: &'a impl super::InputSource, name: &str) -> &'a rustgis_map::Map {
    let context = input.input_context();
    let s = &context;
    s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(name)).unwrap_or_else(|| s.map())
}
/// Binds the map whose current view initializes the page.
fn map_page_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let map = map_page_source(input, p)?;
    let mut inputs = object_inputs(input, p)?;
    inputs.maps.push(map.instance_id());
    Ok(inputs)
}
/// PNG consumes page zero; PDF/HTML consume all pages, using the renderer's stored-map fallback.
fn export_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let i = index_input(input, p)?;
    let pres = s.project.presentations.get(i).ok_or("no presentation")?;
    let png = ps(p, "path").is_some_and(|p| p.to_ascii_lowercase().ends_with(".png"));
    let mut inputs = super::Inputs::layers(std::iter::empty());
    for page in pres.pages.iter().take(if png { 1 } else { usize::MAX }) {
        if let PageKind::Map { map, .. } = &page.kind {
            let map = page_map(input, map);
            inputs.extend(super::tiles::render_inputs(map));
        }
    }
    inputs.state = json!({"presentation":i,"name":pres.name,"maps":pres.pages.iter().take(if png {1}else {usize::MAX}).enumerate().filter_map(|(i,p)|match &p.kind {PageKind::Map{map,..}=>Some((i,map)),_=>None}).collect::<Vec<_>>(),"tiles":inputs.state});
    Ok(inputs)
}

/// Stored page references are not read by ordinary object edits or serialization.
fn object_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let i = index_input(input, p)?;
    let mut inputs = super::Inputs::layers(std::iter::empty());
    inputs.state = json!({"presentation":i,"name":s.project.presentations.get(i).map(|p|&p.name)});
    Ok(inputs)
}
/// Page editing uses the executor's required numeric page selector.
fn page_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let mut inputs = object_inputs(input, p)?;
    inputs.state["page"] = json!(page_of(input, p)?.1);
    Ok(inputs)
}

/// The presentation/report selected by the live session, using the same pure resolver as approval.
pub fn index(s: &Session, p: &Value) -> Result<usize, String> {
    index_input(s, p)
}
