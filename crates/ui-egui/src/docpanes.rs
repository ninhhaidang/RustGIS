//! The Presentation pane (pages with a preview, page tools and playback) and the Notebook pane
//! (text and command cells, run in place).

use std::collections::HashMap;

use egui::{Color32, Ui, vec2};
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::{Tokens, semibold};
use rustgis_engine::cmd::notebook::{Cell, CellKind};

/// A notebook cell's image output in the pane.
pub enum NotebookImage {
    Shown(egui::TextureHandle),
    /// Not a PNG, or larger than the pane draws.
    Unreadable,
    /// Waiting for room in the image budget: its size as RGBA bytes.
    Waiting(usize),
}

/// State of these panes.
#[derive(Default)]
pub struct DocState {
    pub page: usize,
    /// The drawn page (or why it couldn't be drawn), by presentation, page and revision.
    pub preview: Option<(String, Result<egui::TextureHandle, String>)>,
    pub video_source: String,
    /// Playing: when the current page started (seconds), shown in the pane.
    pub playing: Option<f64>,
    /// Cell text being edited, by (notebook, cell), with the cell text it started from: saved
    /// when the field loses focus, and dropped if the cell changed underneath (undo, a command).
    pub edits: HashMap<(usize, usize), (String, String)>,
    /// Image outputs of notebook cells, by a hash of their base64 PNG.
    pub images: HashMap<u64, NotebookImage>,
    /// The hash of each (notebook, cell, image) at `image_revision`, so a payload is hashed once
    /// per change to the project rather than every frame.
    pub image_keys: HashMap<(usize, usize, usize), u64>,
    pub image_revision: u64,
    /// The revision whose unused textures were last dropped.
    pub image_pruned: u64,
    /// Keys the image hashes per run, so a notebook can't make two images collide on purpose.
    pub image_hasher: std::collections::hash_map::RandomState,
    pub message: Option<(String, bool)>,
    /// The page whose fields are being edited, and their text and duration.
    pub editing: Option<(usize, usize)>,
    pub title: String,
    pub body: String,
    pub notes: String,
    pub seconds: f64,
}

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!(text)).font(semibold(12.5)));
    ui.add_space(2.0);
}

fn run(app: &mut RustGisApp, id: &str, p: Value) -> Option<Value> {
    match app.run(id, p) {
        Ok(v) => {
            app.docs.message = None;
            Some(v)
        }
        Err(e) => {
            app.docs.message = Some((e, true));
            None
        }
    }
}

fn message(app: &RustGisApp, ui: &mut Ui, t: &Tokens) {
    if let Some((m, err)) = &app.docs.message {
        let c = if *err { Color32::from_rgb(0xC0, 0x30, 0x30) } else { t.text_dim };
        ui.label(egui::RichText::new(m.as_str()).small().color(c));
    }
}

pub fn presentation(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    message(app, ui, t);
    if app.session.project.presentations.is_empty() {
        ui.label(egui::RichText::new(tl!("No presentations yet. Publish › New Presentation starts one.")).weak());
        if ui.button(tl!("New Presentation")).clicked() {
            run(app, "presentation.new", Value::Null);
        }
        return;
    }
    let Ok(i) = rustgis_engine::cmd::presentation::index(&app.session, &Value::Null) else { return };
    let names: Vec<String> = app.session.project.presentations.iter().map(|r| r.name.clone()).collect();
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("pres_pick")
            .selected_text(names.get(i).map_or("", |s| s.as_str()))
            .width(ui.available_width() - 70.0)
            .show_ui(ui, |ui| {
                for (j, n) in names.iter().enumerate() {
                    if ui.selectable_label(j == i, n.as_str()).clicked() {
                        let _ = app.run("presentation.activate", json!({"presentation": j}));
                        app.docs.page = 0;
                    }
                }
            })
            .response
            .on_hover_text(tl!("Presentation"));
        if ui.small_button(tl!("Delete")).clicked() {
            run(app, "presentation.remove", json!({"presentation": i}));
            app.docs.page = 0;
        }
    });
    // Borrowed, not cloned: pages can hold pictures of many megabytes.
    let Some(n) = app.session.project.presentations.get(i).map(|p| p.pages.len()) else { return };
    app.docs.page = app.docs.page.min(n.saturating_sub(1));
    // Playback: each page shows for its duration.
    if let Some(start) = app.docs.playing {
        let now = ui.input(|i| i.time);
        let secs = app.session.project.presentations.get(i).and_then(|p| p.pages.get(app.docs.page)).map_or(5.0, |p| p.seconds);
        if now - start >= secs {
            if app.docs.page + 1 < n {
                app.docs.page += 1;
                app.docs.playing = Some(now);
            } else {
                app.docs.playing = None;
            }
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
    if n > 0 {
        preview(app, ui, i, t);
        ui.horizontal(|ui| {
            if ui.add_enabled(app.docs.page > 0, egui::Button::new("◀")).clicked() {
                app.docs.page -= 1;
            }
            ui.label(crate::i18n::fmt("Page {n} of {total}", &[("n", &(app.docs.page + 1).to_string()), ("total", &n.to_string())]));
            if ui.add_enabled(app.docs.page + 1 < n, egui::Button::new("▶")).clicked() {
                app.docs.page += 1;
            }
            let playing = app.docs.playing.is_some();
            if ui.button(if playing { tl!("Stop") } else { tl!("Play") }).clicked() {
                app.docs.playing = if playing { None } else { Some(ui.input(|i| i.time)) };
            }
        });
    }
    // The current page's text only, without its picture.
    let current = app.session.project.presentations.get(i).and_then(|p| p.pages.get(app.docs.page)).map(|p| {
        let body = match &p.kind {
            rustgis_engine::map::presentation::PageKind::Blank { text, .. } => Some(text.clone()),
            _ => None,
        };
        (p.title.clone(), p.notes.clone(), p.seconds, body)
    });
    if let Some((title, notes, seconds, body_now)) = current {
        let page = PageText { title, notes, seconds };
        let k = app.docs.page;
        heading(ui, "Page");
        // The text being typed lives in the pane's state (rebuilt each frame it would lose
        // keystrokes): it follows the page while a field isn't focused and is saved when it loses
        // focus.
        if app.docs.editing != Some((i, k)) {
            app.docs.editing = Some((i, k));
            (app.docs.title, app.docs.notes, app.docs.seconds) = (page.title.clone(), page.notes.clone(), page.seconds);
            app.docs.body = body_now.clone().unwrap_or_default();
        }
        // One text field: saved under `key` when it loses focus, refreshed from `now` otherwise.
        let field = |app: &mut RustGisApp, resp: egui::Response, key: &str, value: String, now: &str| -> Option<String> {
            if resp.lost_focus() {
                if value != now {
                    run(app, "presentation.updatePage", json!({"presentation": i, "page": k, key: value}));
                }
                None
            } else if !resp.has_focus() {
                Some(now.to_string())
            } else {
                None
            }
        };
        ui.horizontal(|ui| {
            let input_label = ui.label(tl!("Title"));
            let resp =
                ui.add(egui::TextEdit::singleline(&mut app.docs.title).desired_width(ui.available_width())).labelled_by(input_label.id);
            let v = app.docs.title.clone();
            if let Some(fresh) = field(app, resp, "title", v, &page.title) {
                app.docs.title = fresh;
            }
        });
        if let Some(text) = &body_now {
            let resp = ui.add(egui::TextEdit::multiline(&mut app.docs.body).desired_rows(3).desired_width(ui.available_width()));
            let v = app.docs.body.clone();
            if let Some(fresh) = field(app, resp, "text", v, text) {
                app.docs.body = fresh;
            }
        }
        let resp = ui.add(
            egui::TextEdit::multiline(&mut app.docs.notes)
                .hint_text(tl!("Speaker notes"))
                .desired_rows(2)
                .desired_width(ui.available_width()),
        );
        let v = app.docs.notes.clone();
        if let Some(fresh) = field(app, resp, "notes", v, &page.notes) {
            app.docs.notes = fresh;
        }
        ui.horizontal(|ui| {
            let input_label = ui.label(tl!("Seconds"));
            let resp = ui
                .add(egui::DragValue::new(&mut app.docs.seconds).update_while_editing(false).range(0.5..=3600.0).speed(0.5))
                .labelled_by(input_label.id);
            if (resp.drag_stopped() || resp.lost_focus()) && !crate::widgets::escaped(&resp) && app.docs.seconds != page.seconds {
                let secs = app.docs.seconds;
                run(app, "presentation.updatePage", json!({"presentation": i, "page": k, "seconds": secs}));
            } else if !resp.dragged() && !resp.has_focus() {
                app.docs.seconds = page.seconds;
            }
            if ui.add_enabled(k > 0, egui::Button::new(tl!("Move Up")).small()).clicked()
                && run(app, "presentation.updatePage", json!({"presentation": i, "page": k, "moveTo": k - 1})).is_some()
            {
                app.docs.page = k - 1;
            }
            if ui.add_enabled(k + 1 < n, egui::Button::new(tl!("Move Down")).small()).clicked()
                && run(app, "presentation.updatePage", json!({"presentation": i, "page": k, "moveTo": k + 1})).is_some()
            {
                app.docs.page = k + 1;
            }
            if ui.small_button(tl!("Delete")).clicked() {
                run(app, "presentation.removePage", json!({"presentation": i, "page": k}));
            }
        });
    }
    heading(ui, "Add a page");
    let at = (app.docs.page + 1).min(n);
    ui.horizontal_wrapped(|ui| {
        if ui.button(tl!("Map")).on_hover_text(tl!("The map view as it is now")).clicked()
            && run(app, "presentation.mapPage", json!({"presentation": i, "at": at})).is_some()
        {
            app.docs.page = at;
        }
        if ui.button(tl!("Blank")).clicked()
            && run(app, "presentation.blankPage", json!({"presentation": i, "at": at, "title": "Title"})).is_some()
        {
            app.docs.page = at;
        }
        if ui.button(tl!("Image…")).clicked() {
            let _ = app.run("ui.presentationPicture", Value::Null);
        }
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut app.docs.video_source)
                .hint_text(tl!("Video file or URL"))
                .desired_width(ui.available_width() - 60.0),
        );
        if ui.button(tl!("Video")).clicked() {
            let source = app.docs.video_source.trim().to_string();
            if run(app, "presentation.videoPage", json!({"presentation": i, "source": source, "at": at})).is_some() {
                app.docs.video_source.clear();
                app.docs.page = at;
            }
        }
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button(tl!("Export…")).clicked() {
            let _ = app.run("ui.exportPresentation", Value::Null);
        }
        if ui.button(tl!("Save As…")).clicked() {
            let _ = app.run("ui.saveItem", json!({"kind": "presentation"}));
        }
    });
}

/// The current page's fields shown in the pane.
struct PageText {
    title: String,
    notes: String,
    seconds: f64,
}

fn preview(app: &mut RustGisApp, ui: &mut Ui, i: usize, t: &Tokens) {
    let key = format!("{i}#{}#{}", app.docs.page, app.session.revision);
    if app.docs.preview.as_ref().is_none_or(|p| p.0 != key) {
        let (s, page) = (&mut app.session, app.docs.page);
        let drawn =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rustgis_engine::cmd::presentation::draw_page(s, i, page, 30.0)))
                .unwrap_or_else(|_| Err("the preview failed unexpectedly".into()));
        let tex = drawn.map(|pm| {
            let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
            ui.ctx().load_texture("presentation_preview", img, egui::TextureOptions::LINEAR)
        });
        app.docs.preview = Some((key, tex));
    }
    let tex = match &app.docs.preview {
        Some((_, Ok(tex))) => tex,
        Some((_, Err(e))) => {
            ui.label(egui::RichText::new(e.as_str()).small().color(Color32::from_rgb(0xC0, 0x30, 0x30)));
            return;
        }
        None => return,
    };
    let size = tex.size_vec2();
    let w = ui.available_width().min(320.0);
    let h = w * size.y / size.x.max(1.0);
    let id = tex.id();
    egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.border)).show(ui, |ui| {
        ui.image((id, vec2(w, h)));
    });
}

pub fn notebook(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    message(app, ui, t);
    if app.session.project.notebooks.is_empty() {
        // No notebook left to show the images of.
        app.docs.images.clear();
        ui.label(
            egui::RichText::new(tl!("No notebooks yet. Analyze › New Notebook starts one, or Add and Open Notebook opens one.")).weak(),
        );
        if ui.button(tl!("New Notebook")).clicked() {
            run(app, "notebook.new", Value::Null);
        }
        return;
    }
    let Ok(i) = rustgis_engine::cmd::notebook::index(&app.session, &Value::Null) else { return };
    let names: Vec<String> = app.session.project.notebooks.iter().map(|r| r.name.clone()).collect();
    // A Python (or other language) notebook from Jupyter: its code cells are shown, not run.
    let language = app.session.project.notebooks.get(i).and_then(|nb| nb.language_label());
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("nb_pick")
            .selected_text(names.get(i).map_or("", |s| s.as_str()))
            .width(ui.available_width() - 130.0)
            .show_ui(ui, |ui| {
                for (j, n) in names.iter().enumerate() {
                    if ui.selectable_label(j == i, n.as_str()).clicked() {
                        let _ = app.run("notebook.activate", json!({"notebook": j}));
                    }
                }
            })
            .response
            .on_hover_text(tl!("Notebook"));
        if language.is_none() && ui.small_button(tl!("Run All")).clicked() {
            run(app, "notebook.run", json!({"notebook": i}));
            app.map.invalidate();
        }
        if ui.small_button(tl!("Save As…")).clicked() {
            let _ = app.run("ui.saveItem", json!({"kind": "notebook"}));
        }
    });
    if let Some(lang) = &language {
        let hint =
            crate::i18n::fmt("{language} cells don't run here: open this notebook in Jupyter to run them.", &[("language", lang.as_str())]);
        ui.label(egui::RichText::new(hint).small().color(t.text_dim));
    }
    // One cell's shown fields are copied at a time (the commands below need the app), not the
    // whole notebook, nor the cell's Jupyter fields, which can hold megabytes of outputs.
    let n = app.session.project.notebooks.get(i).map_or(0, |nb| nb.cells.len());
    for k in 0..n {
        let (session, docs) = (&app.session, &mut app.docs);
        let Some(c) = session.project.notebooks.get(i).and_then(|nb| nb.cells.get(k)) else { break };
        let (images, hidden) = cell_images(docs, ui.ctx(), session.revision, (i, k), c);
        let cell = Cell { kind: c.kind, source: c.source.clone(), output: c.output.clone(), error: c.error, jupyter: None };
        egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.border)).corner_radius(4).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let kind = match (cell.kind, &language) {
                    (CellKind::Code, Some(lang)) => lang.clone(),
                    (CellKind::Code, None) => tl!("Commands").to_string(),
                    (CellKind::Markdown, _) => tl!("Text").to_string(),
                };
                ui.label(egui::RichText::new(kind).small().color(t.text_dim));
                if cell.kind == CellKind::Code && language.is_none() && ui.small_button(tl!("Run")).clicked() {
                    // Save an edit in progress first.
                    if let Some((text, _)) = app.docs.edits.remove(&(i, k)) {
                        run(app, "notebook.editCell", json!({"notebook": i, "cell": k, "source": text}));
                    }
                    run(app, "notebook.run", json!({"notebook": i, "cell": k}));
                    app.map.invalidate();
                }
                if ui.small_button(tl!("Delete")).clicked() {
                    // Other cells' typing is saved before the cells after this one move up.
                    app.docs.edits.remove(&(i, k));
                    let pending: Vec<_> = app.docs.edits.drain().collect();
                    for ((n, c), (text, _)) in pending {
                        run(app, "notebook.editCell", json!({"notebook": n, "cell": c, "source": text}));
                    }
                    run(app, "notebook.removeCell", json!({"notebook": i, "cell": k}));
                }
            });
            if app.docs.edits.get(&(i, k)).is_some_and(|(_, from)| *from != cell.source) {
                app.docs.edits.remove(&(i, k));
            }
            let mut text = app.docs.edits.get(&(i, k)).map_or_else(|| cell.source.clone(), |(t, _)| t.clone());
            let edit = egui::TextEdit::multiline(&mut text).desired_rows(2).desired_width(ui.available_width());
            let edit = if cell.kind == CellKind::Code { edit.code_editor() } else { edit };
            let resp = ui.add(edit);
            if resp.changed() {
                app.docs.edits.insert((i, k), (text.clone(), cell.source.clone()));
            }
            if resp.lost_focus()
                && let Some((text, _)) = app.docs.edits.remove(&(i, k))
                && text != cell.source
            {
                run(app, "notebook.editCell", json!({"notebook": i, "cell": k, "source": text}));
            }
            if let Some(out) = &cell.output {
                let c = if cell.error { Color32::from_rgb(0xC0, 0x30, 0x30) } else { t.text_dim };
                ui.label(egui::RichText::new(out.as_str()).small().monospace().color(c));
            }
            for tex in images {
                let size = tex.size_vec2();
                let w = size.x.min(ui.available_width()).max(1.0);
                ui.image((tex.id(), vec2(w, w * size.y / size.x.max(1.0))));
            }
            if hidden > 0 {
                let note = tl!("Some images aren't shown: the notebook's images use all the memory set aside for them.");
                ui.label(egui::RichText::new(note).small().color(t.text_dim));
            }
        });
        ui.add_space(4.0);
    }
    // Every cell has now named its images: after a change, or when some wait for room, drop the
    // textures of images not in this notebook (a closed one's, or another's: they decode again
    // when shown).
    let waiting = app.docs.images.values().any(|e| matches!(e, NotebookImage::Waiting(_)));
    if waiting || app.docs.image_pruned != app.session.revision {
        app.docs.image_pruned = app.session.revision;
        let live: std::collections::HashSet<u64> = app.docs.image_keys.values().copied().collect();
        let before = app.docs.images.len();
        app.docs.images.retain(|k, _| live.contains(k));
        // The waiting images try the freed room on the next frame.
        if app.docs.images.len() < before {
            ui.ctx().request_repaint();
        }
    }
    ui.horizontal(|ui| {
        let add = match &language {
            Some(lang) => crate::i18n::fmt("Add {language}", &[("language", lang.as_str())]),
            None => tl!("Add Commands").to_string(),
        };
        if ui.button(add).clicked() {
            run(app, "notebook.addCell", json!({"notebook": i, "kind": "code"}));
        }
        if ui.button(tl!("Add Text")).clicked() {
            run(app, "notebook.addCell", json!({"notebook": i, "kind": "markdown"}));
        }
    });
}

/// Longest side of a notebook image drawn, and the most image memory (RGBA) kept for them.
const MAX_IMAGE_SIDE: u32 = 4096;
const IMAGE_BUDGET: usize = 256 << 20;

/// The textures of a cell's PNG outputs, and how many are waiting for room in the budget.
fn cell_images(
    docs: &mut DocState,
    ctx: &egui::Context,
    revision: u64,
    at: (usize, usize),
    cell: &Cell,
) -> (Vec<egui::TextureHandle>, usize) {
    use std::hash::BuildHasher;
    if docs.image_revision != revision {
        docs.image_keys.clear();
        docs.image_revision = revision;
    }
    let max_side = ctx.input(|i| i.max_texture_side).min(MAX_IMAGE_SIDE as usize) as u32;
    let used = |docs: &DocState| -> usize {
        docs.images.values().map(|e| if let NotebookImage::Shown(t) = e { t.size()[0] * t.size()[1] * 4 } else { 0 }).sum()
    };
    let (mut out, mut hidden) = (Vec::new(), 0);
    for (j, png) in cell.images().iter().enumerate() {
        let key = *docs.image_keys.entry((at.0, at.1, j)).or_insert_with(|| docs.image_hasher.hash_one(png.as_ref()));
        let (bytes, png) = match docs.images.get(&key) {
            Some(NotebookImage::Shown(tex)) => {
                out.push(tex.clone());
                continue;
            }
            Some(NotebookImage::Unreadable) => continue,
            // Retried once something else frees room.
            Some(NotebookImage::Waiting(bytes)) => {
                if used(docs) + bytes > IMAGE_BUDGET {
                    hidden += 1;
                    continue;
                }
                (*bytes, png_bytes(png))
            }
            None => {
                // The header gives the size, so an image that can't fit isn't decoded.
                let png = png_bytes(png);
                let Some((w, h)) = png.as_deref().and_then(|p| png_dimensions(p, max_side)) else {
                    docs.images.insert(key, NotebookImage::Unreadable);
                    continue;
                };
                let bytes = w as usize * h as usize * 4;
                // Evicting happens after every cell has named its images (in `notebook`).
                if used(docs) + bytes > IMAGE_BUDGET {
                    docs.images.insert(key, NotebookImage::Waiting(bytes));
                    hidden += 1;
                    continue;
                }
                (bytes, png)
            }
        };
        let entry = match png.as_deref().and_then(|p| decode_png(p, max_side)) {
            Some(img) if img.width() * img.height() * 4 == bytes => {
                let tex = ctx.load_texture("notebook_image", img, egui::TextureOptions::LINEAR);
                out.push(tex.clone());
                NotebookImage::Shown(tex)
            }
            _ => NotebookImage::Unreadable,
        };
        docs.images.insert(key, entry);
    }
    (out, hidden)
}

/// A base64 PNG from a notebook file (hostile input) as bytes.
fn png_bytes(b64: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let clean: Vec<u8> = b64.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    base64::engine::general_purpose::STANDARD.decode(clean).ok()
}

/// A PNG reader that refuses images over `max_side` pixels a side (at most `MAX_IMAGE_SIDE`).
fn png_reader(bytes: &[u8], max_side: u32) -> image::ImageReader<std::io::Cursor<&[u8]>> {
    let max_side = max_side.min(MAX_IMAGE_SIDE);
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(max_side);
    limits.max_image_height = Some(max_side);
    limits.max_alloc = Some(u64::from(max_side) * u64::from(max_side) * 8);
    reader.limits(limits);
    reader
}

/// The size a PNG's header gives, if within `max_side` pixels a side; nothing is decoded.
fn png_dimensions(png: &[u8], max_side: u32) -> Option<(u32, u32)> {
    let (w, h) = png_reader(png, max_side).into_dimensions().ok()?;
    (w <= max_side.min(MAX_IMAGE_SIDE) && h <= max_side.min(MAX_IMAGE_SIDE)).then_some((w, h))
}

/// A PNG from a notebook file (hostile input): `None` unless it decodes within `max_side`
/// pixels a side (at most `MAX_IMAGE_SIDE`, and what the GPU takes).
fn decode_png(png: &[u8], max_side: u32) -> Option<egui::ColorImage> {
    let img = png_reader(png, max_side).decode().ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()))
}

/// Shared items (Publish › Share As › Jobs) with their Manage tools, and the data stores.
pub fn shares(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    message(app, ui, t);
    let items = app.session.project.shared.clone();
    if items.is_empty() {
        ui.label(
            egui::RichText::new(tl!("Nothing shared yet. Publish › Share As makes web maps, layers, styles, scenes and more.")).weak(),
        );
    }
    for it in items.iter().rev() {
        egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.border)).corner_radius(4).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let label = rustgis_engine::cmd::sharing::KINDS.iter().find(|k| k.0 == it.kind).map_or("File", |k| k.2);
            ui.label(egui::RichText::new(format!("{} · {}", it.name, tl!(label))).font(semibold(12.0)));
            ui.label(egui::RichText::new(&it.path).small().color(t.text_dim));
            let manage = match it.kind.as_str() {
                "webMap" => Some(("Save Web Map", "share.saveWebMap")),
                "webLayer" => Some(("Replace Web Layer", "share.replaceWebLayer")),
                "webScene" => Some(("Save Web Scene", "share.saveWebScene")),
                "linkChart" => Some(("Save Web Link Chart", "share.saveWebLinkChart")),
                _ => None,
            };
            if let Some((text, cmd)) = manage {
                let mut p = json!({"item": it.id});
                // A web layer is replaced from the selected layer.
                if cmd == "share.replaceWebLayer"
                    && let Some(l) = app.session.active_layer.and_then(|id| app.session.map().layer(id)).filter(|l| !l.is_raster())
                {
                    p["layer"] = json!(l.name);
                }
                if ui.small_button(tl!(text)).clicked() {
                    run(app, cmd, p);
                }
            }
        });
        ui.add_space(4.0);
    }
    heading(ui, "Data Stores");
    let stores = app.session.project.data_stores.clone();
    for d in &stores {
        ui.horizontal(|ui| {
            ui.label(format!("{} — {}", d.name, d.path));
            if ui.small_button(tl!("Remove")).clicked() {
                run(app, "share.dataStore", json!({"remove": d.name}));
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    ui.horizontal(|ui| {
        if ui.button(tl!("Add Data Store…")).clicked()
            && let Some(dir) = rfd::FileDialog::new().pick_folder()
        {
            run(app, "share.dataStore", json!({"path": dir.display().to_string()}));
        }
        if !stores.is_empty()
            && ui.button(tl!("Upload File…")).clicked()
            && let Some(f) = rfd::FileDialog::new().pick_file()
        {
            run(app, "share.upload", json!({"path": f.display().to_string()}));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::RgbaImage::new(1, 1).write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png).unwrap();
        // Rewrite the header's size, and its checksum, so only the size is wrong.
        bytes[16..20].copy_from_slice(&w.to_be_bytes());
        bytes[20..24].copy_from_slice(&h.to_be_bytes());
        let crc = !bytes[12..29]
            .iter()
            .fold(!0u32, |c, &b| (0..8).fold(c ^ u32::from(b), |c, _| (c >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(c & 1))));
        bytes[29..33].copy_from_slice(&crc.to_be_bytes());
        bytes
    }

    #[test]
    fn notebook_images_decode_within_limits() {
        use base64::Engine;
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        // A real 3000 x 1 image, so the side limit is what refuses it.
        let mut wide = Vec::new();
        image::RgbaImage::new(3000, 1).write_to(&mut std::io::Cursor::new(&mut wide), image::ImageFormat::Png).unwrap();
        assert!(png_bytes(&b64(&wide)).and_then(|p| decode_png(&p, MAX_IMAGE_SIDE)).is_some());
        let decode = |b64: &str, side| png_bytes(b64).and_then(|p| decode_png(&p, side));
        let dimensions = |b64: &str, side| png_bytes(b64).and_then(|p| png_dimensions(&p, side));
        let small = decode(&format!("{}\n", b64(&png(1, 1))), MAX_IMAGE_SIDE).expect("a small PNG decodes, line break and all");
        assert_eq!(small.size, [1, 1]);
        // A header claiming a huge image is refused before anything that size is allocated.
        assert!(decode(&b64(&png(100_000, 100_000)), MAX_IMAGE_SIDE).is_none());
        // Nor one over the GPU's largest texture.
        assert!(decode(&b64(&png(1, 1)), 2048).is_some());
        assert!(decode(&b64(&wide), 2048).is_none());
        assert!(decode("not base64!", MAX_IMAGE_SIDE).is_none());
        // The size is read from the header alone, and refused the same way.
        assert_eq!(dimensions(&b64(&png(1, 1)), MAX_IMAGE_SIDE), Some((1, 1)));
        assert_eq!(dimensions(&b64(&wide), 2048), None);
        assert_eq!(dimensions(&b64(&png(100_000, 100_000)), MAX_IMAGE_SIDE), None);
    }
}
