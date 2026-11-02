//! The browser shell: file picker, downloads, drag-and-drop, the unsaved-changes guard and the
//! eframe web runner.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use rustgis_ui_egui::rustgis_engine::data::diagnostics::Capture;
use rustgis_ui_egui::{Inbox, RustGisApp, Services, Slot};

use crate::parts::Notes;
use wasm_bindgen::JsCast as _;

const DATA_EXTS: &[&str] = &[
    "geojson",
    // Geotagged photos (points in the Photos layer).
    "jpg",
    "jpeg",
    "json",
    "csv",
    "tsv",
    "txt",
    "xlsx",
    "xlsm",
    "xlsb",
    "xls",
    "ods",
    "rss",
    "atom",
    "georss",
    "xml",
    "landxml",
    "zip",
    // A Shapefile's loose files, picked together.
    "shp",
    "dbf",
    "shx",
    "prj",
    "cpg",
    "kml",
    "kmz",
    "gpx",
    "gpkg",
    "parquet",
    "geoparquet",
    "fgb",
    "mbtiles",
    "gml",
    "dxf",
    "mif",
    "pbf",
    "topojson",
    "las",
    "laz",
    "tif",
    "tiff",
    "asc",
];
const CANVAS_ID: &str = "rustgis_canvas";
const LOADING_ID: &str = "rustgis_loading";

/// The console logger, without the drawing library's warnings. tiny-skia warns of every path
/// it can't fill, stroke or dash (a vector tile's shapes shrunk to nothing, dashes past its
/// limits): thousands a frame on a vector tile map, each written to the console with a stack
/// trace, which made the map crawl and the page run out of memory.
struct Logger(eframe::WebLogger);

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        worth_logging(metadata) && self.0.enabled(metadata)
    }

    fn log(&self, record: &log::Record<'_>) {
        if worth_logging(record.metadata()) {
            self.0.log(record);
        }
    }

    fn flush(&self) {
        self.0.flush();
    }
}

/// Whether a message is worth the console: of tiny-skia's, only errors.
fn worth_logging(metadata: &log::Metadata<'_>) -> bool {
    let target = metadata.target();
    let tiny_skia = target == "tiny_skia" || target.starts_with("tiny_skia::");
    !tiny_skia || metadata.level() <= log::Level::Error
}

pub fn start() {
    log::set_max_level(log::LevelFilter::Info);
    log::set_boxed_logger(Box::new(Logger(eframe::WebLogger::new(log::LevelFilter::Info)))).ok();
    wasm_bindgen_futures::spawn_local(async {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            log::error!("no document");
            return;
        };
        let Some(canvas) = document.get_element_by_id(CANVAS_ID).and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok()) else {
            log::error!("missing <canvas id=\"{CANVAS_ID}\">");
            return;
        };
        let dirty = Rc::new(Cell::new(false));
        // Inside another page (`?embed`) the page holds the work, so leaving it doesn't ask.
        let embedded = flag(&query(), "embed");
        if !embedded && let Err(e) = guard_unload(dirty.clone()) {
            log::error!("no unsaved-changes guard: {e}");
        }
        let taken: TakenKeys = TakenKeys::default();
        if let Err(e) = keep_app_shortcuts(taken.clone()) {
            log::error!("the browser keeps its own shortcuts: {e}");
        }
        let mut options = eframe::WebOptions::default();
        if query().contains("webgl")
            && let eframe::egui_wgpu::WgpuSetup::CreateNew(create) = &mut options.wgpu_options.wgpu_setup
        {
            create.instance_descriptor.backends = eframe::wgpu::Backends::GL;
        }
        let result = eframe::WebRunner::new()
            .start(
                canvas,
                options,
                Box::new(move |cc| {
                    let inbox: Inbox = Inbox::default();
                    let font_inbox: Inbox = Inbox::default();
                    let notes = Notes::default();
                    let q = query();
                    let project = params(&q, "project", 1).into_iter().next();
                    let mut app = RustGisApp::with_sample().with_services(services(
                        inbox.clone(),
                        font_inbox,
                        notes.clone(),
                        cc.egui_ctx.clone(),
                        embedded,
                    ));
                    // Settings › Start-up, unless `?blank`, `?projection=EPSG:3010` (a preset id,
                    // `mercator`, `globe`) or `?extent=minLon,minLat,maxLon,maxLat` says otherwise
                    // for this visit.
                    let start = rustgis_ui_egui::Start {
                        blank: (embedded || project.is_some() || flag(&q, "blank")).then_some(true),
                        projection: param(&q, "projection").filter(|c| !c.is_empty()),
                        extent: param(&q, "extent").filter(|c| !c.is_empty()),
                        ..Default::default()
                    };
                    if let Err(e) = app.start(&start) {
                        log::warn!("start-up: {e}");
                    }
                    // `?gpu` (or `?gpu=0`) turns the GPU map on (off) for this visit.
                    if let Some(v) = param(&q, "gpu").or_else(|| flag(&q, "gpu").then(String::new)) {
                        let on = !matches!(v.as_str(), "0" | "false" | "off");
                        if let Err(e) = app.run("ui.map.gpu", serde_json::json!({"on": on, "remember": false})) {
                            log::warn!("?gpu: {e}");
                        }
                    }
                    // `?lang=de` picks the interface language for this visit.
                    if let Some(code) = q.trim_start_matches('?').split('&').find_map(|kv| kv.strip_prefix("lang=")) {
                        match rustgis_ui_egui::i18n::Lang::from_code(code) {
                            Some(l) => app.ui.language = l.code().to_string(),
                            None => log::warn!("?lang={code}: no such interface language; following the browser's"),
                        }
                    }
                    // `?project=<url>` opens a native project or imports a GeoLibre map asynchronously.
                    // Keep this startup path exclusive: other links could otherwise add data
                    // to different maps depending on when the import completes.
                    if let Some(url) = &project {
                        if let Err(e) = app.run("ui.openUrl", serde_json::json!({"url": url})) {
                            app.session.notify(format!("?project={url}: {e}"), true);
                        }
                        let skipped: Vec<&str> =
                            ["data", "stac", "style", "tiles", "zarr"].into_iter().filter(|k| !params(&q, k, 1).is_empty()).collect();
                        if !skipped.is_empty() {
                            let list = skipped.iter().map(|k| format!("?{k}=")).collect::<Vec<_>>().join(", ");
                            app.session.notify(
                                rustgis_ui_egui::i18n::fmt(
                                    "?project= opens a project or imports a map, so {list} links were left out",
                                    &[("list", &list)],
                                ),
                                false,
                            );
                        }
                    }
                    let links = if project.is_some() { "" } else { q.as_str() };
                    // `#zoom/lat/lon/bearing/pitch` (a MapLibre map link) places the map; without
                    // it, `?extent=` or `?data=`, the first `?style=` opens at the style's own view.
                    let fragment = hash();
                    // A fragment that is no map position (`#about`) is left alone.
                    let linked = project.is_none() && rustgis_engine::cmd::navigate::is_map_link(&fragment) && {
                        let r = app.run("map.goToLink", serde_json::json!({"url": fragment}));
                        if let Err(e) = &r {
                            log::warn!("{fragment}: {e}");
                        }
                        r.is_ok()
                    };
                    let placed = linked || param(&q, "extent").is_some_and(|e| !e.is_empty()) || !params(links, "data", 1).is_empty();
                    // `?data=<url>` (repeatable) adds data by web address, downloaded in the
                    // background, the map zoomed to the first file that lands with something to
                    // show (unless a `#` map link placed it); `?style=<url>` adds a MapLibre style;
                    // `?tiles=<template>` a tile layer. Links that share a map. What fails is said
                    // in a notice.
                    for url in params(links, "data", 16) {
                        if let Err(e) = app.run("ui.addUrl", serde_json::json!({"url": url, "zoom": !linked})) {
                            app.session.notify(format!("?data={url}: {e}"), true);
                        }
                    }
                    // `?stac=<url>`: a STAC item (its default asset is added, signed when it is a
                    // Planetary Computer one) or a collection (STAC Search opens on it).
                    // One link: the dialog follows one document at a time, so others are said to
                    // be skipped rather than lost.
                    let stac = params(links, "stac", 4);
                    if let Some(url) = stac.first()
                        && let Err(e) = app.run("ui.stacLink", serde_json::json!({"url": url}))
                    {
                        app.session.notify(format!("?stac={url}: {e}"), true);
                    }
                    if stac.len() > 1 {
                        app.session.notify(format!("only the first ?stac= link is opened ({} more were left out)", stac.len() - 1), false);
                    }
                    let mut style_view = project.is_none() && !placed;
                    for (key, kind) in [("style", Some("style")), ("tiles", None)] {
                        for url in params(links, key, 8) {
                            let mut p = serde_json::json!({"url": url});
                            if let Some(kind) = kind {
                                p["kind"] = serde_json::json!(kind);
                                if style_view {
                                    p["view"] = serde_json::json!(true);
                                }
                            }
                            match app.run("map.addTiles", p) {
                                // The first style added opens at its view, not one that failed.
                                Ok(_) => style_view &= kind.is_none(),
                                Err(e) => app.session.notify(format!("?{key}={url}: {e}"), true),
                            }
                        }
                    }
                    // `?zarr=<url>` opens Add Zarr Variable on a store, its variables listed.
                    // Read as `?data=` is, so a `+` in the address stays a `+`.
                    if let Some(url) = params(links, "zarr", 1).into_iter().next()
                        && let Err(e) = app.run("ui.addZarr", serde_json::json!({"address": url}))
                    {
                        app.session.notify(format!("?zarr={url}: {e}"), true);
                    }
                    // `?registry=<url>` opens the Plugin Manager on another plugin registry for this
                    // visit (a pull request's preview, say); installing still asks as usual.
                    if let Some(url) = param(&q, "registry").filter(|u| !u.is_empty())
                        && let Err(e) = app.run("ui.plugins", serde_json::json!({"open": true, "registry": url, "tab": "browse"}))
                    {
                        log::warn!("?registry={url}: {e}");
                    }
                    let embed = if embedded {
                        match Embed::listen(cc.egui_ctx.clone()) {
                            Ok(e) => Some(e),
                            Err(e) => {
                                log::error!("?embed: {e}");
                                None
                            }
                        }
                    } else {
                        None
                    };
                    Ok(Box::new(WebShell { app, inbox, notes, dirty, embed, taken, taken_print: None }))
                }),
            )
            .await;
        if let Some(el) = document.get_element_by_id(LOADING_ID) {
            match result {
                Ok(()) => el.remove(),
                Err(e) => {
                    el.set_inner_html(&format!("<p>RustGIS failed to start: {e:?}</p><p>A browser with WebGPU or WebGL2 is required.</p>"))
                }
            }
        }
    });
}

/// Up to `max` values of a query parameter that may repeat (`?data=a&data=b`), percent-decoded.
/// A `+` stays a `+` (it is one in a web address): a value holding `&`, `#` or a space must be
/// percent-encoded.
fn params(q: &str, key: &str, max: usize) -> Vec<String> {
    q.trim_start_matches('?')
        .split('&')
        .filter_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
        .filter(|raw| !raw.trim().is_empty())
        .take(max)
        .filter_map(|raw| js_sys::decode_uri_component(raw).ok().and_then(|s| s.as_string()))
        .filter(|v| !v.trim().is_empty())
        .collect()
}

/// A query parameter's value, percent-decoded.
fn param(q: &str, key: &str) -> Option<String> {
    let raw = q.trim_start_matches('?').split('&').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))?;
    js_sys::decode_uri_component(&raw.replace('+', " ")).ok().and_then(|s| s.as_string())
}

/// Whether a query flag is set (`?blank`, `?embed`).
fn flag(q: &str, key: &str) -> bool {
    q.trim_start_matches('?').split('&').any(|kv| kv == key)
}

fn query() -> String {
    web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default()
}

/// The page address's fragment (`#…`), as MapLibre's map links carry a position.
fn hash() -> String {
    web_sys::window().and_then(|w| w.location().hash().ok()).unwrap_or_default()
}

/// Reads dropped files asynchronously (browsers can't read them synchronously) into the inbox,
/// and mirrors the unsaved-changes flag for the `beforeunload` guard.
struct WebShell {
    app: RustGisApp,
    inbox: Inbox,
    /// Notices from files read off the frame.
    notes: Notes,
    dirty: Rc<Cell<bool>>,
    embed: Option<Embed>,
    /// The app's Ctrl (⌘) shortcuts, kept current for [`keep_app_shortcuts`].
    taken: TakenKeys,
    /// [`RustGisApp::shortcuts_fingerprint`] when `taken` was last filled.
    taken_print: Option<u64>,
}

/// The app's Ctrl (⌘) shortcuts as (Alt, Shift, key): [`RustGisApp::command_keys`], shared with
/// the page's key listener.
type TakenKeys = Rc<RefCell<Vec<(bool, bool, egui::Key)>>>;

/// The link to the page that embeds the app (`?embed`; see `embed.rs`).
struct Embed {
    parent: web_sys::Window,
    /// Messages from the parent, as JSON, waiting for the next frame.
    queue: Rc<RefCell<Vec<serde_json::Value>>>,
    /// The origin of the parent's first message: the only one answered.
    origin: Rc<RefCell<Option<String>>>,
    changes: crate::embed::Changes,
}

impl Embed {
    /// Takes messages from the parent window and tells it the app is ready.
    fn listen(ctx: egui::Context) -> Result<Embed, String> {
        let err = |e: wasm_bindgen::JsValue| format!("{e:?}");
        let window = web_sys::window().ok_or("no window")?;
        let parent = window.parent().map_err(err)?.ok_or("no parent window")?;
        let (queue, origin) = (Rc::new(RefCell::new(Vec::new())), Rc::new(RefCell::new(None::<String>)));
        let (q, o, from) = (queue.clone(), origin.clone(), parent.clone());
        let on_message = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
            if !e.source().is_some_and(|s| js_sys::Object::is(&s, &from)) {
                return;
            }
            let mut origin = o.borrow_mut();
            if origin.as_deref().is_some_and(|known| known != e.origin()) {
                return;
            }
            let Some(text) = js_sys::JSON::stringify(&e.data()).ok().and_then(|t| t.as_string()) else {
                return;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                return;
            };
            if crate::embed::parse(&value).is_some() {
                origin.get_or_insert_with(|| e.origin());
                q.borrow_mut().push(value);
                ctx.request_repaint();
            }
        });
        window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).map_err(err)?;
        on_message.forget();
        let embed = Embed { parent, queue, origin, changes: crate::embed::Changes::default() };
        // Nothing to hide in it, so it goes to whichever page embeds the app.
        embed.post(&crate::embed::ready(), "*");
        Ok(embed)
    }

    fn post(&self, msg: &serde_json::Value, origin: &str) {
        match js_sys::JSON::parse(&msg.to_string()) {
            Ok(v) => {
                if let Err(e) = self.parent.post_message(&v, origin) {
                    log::error!("couldn't post to the embedding page: {e:?}");
                }
            }
            Err(e) => log::error!("couldn't post to the embedding page: {e:?}"),
        }
    }

    /// Posts to the page that sent the first message; nothing before it has.
    fn answer(&self, msg: &serde_json::Value) {
        if let Some(origin) = self.origin.borrow().clone() {
            self.post(msg, &origin);
        }
    }

    /// Runs the page's messages, then posts the project once a change here has settled.
    fn exchange(&mut self, app: &mut RustGisApp, ctx: &egui::Context) {
        use crate::embed::{Msg, key, parse};
        let queued = std::mem::take(&mut *self.queue.borrow_mut());
        for value in queued {
            match parse(&value) {
                Some(Msg::Project(text)) => match app.run("project.openText", serde_json::json!({"text": text})) {
                    Ok(_) => self.changes.seen(key(&app.session)),
                    Err(e) => app.session.notify(e, true),
                },
                Some(Msg::Run { id, command, params }) => {
                    let outcome = if crate::embed::runnable(&command) {
                        app.run(&command, params)
                    } else {
                        Err(format!("{command}: only the app's own commands (ui.*) run from the embedding page"))
                    };
                    self.answer(&crate::embed::result(id, outcome));
                }
                None => {}
            }
        }
        match self.changes.due(key(&app.session), web_time::Instant::now()) {
            (true, _) => match serde_json::to_string(&app.session.project) {
                Ok(text) => self.answer(&crate::embed::project(&text)),
                Err(e) => log::error!("couldn't send the project: {e}"),
            },
            (false, Some(wait)) => ctx.request_repaint_after(wait),
            (false, None) => {}
        }
    }
}

impl eframe::App for WebShell {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // The GPU map backend draws inside egui's wgpu pass; hand over the render state.
        self.app.set_render_state(frame.wgpu_render_state().cloned());
        let dropped = ctx.input_mut(|i| std::mem::take(&mut i.raw.dropped_files));
        let files: Vec<web_sys::File> = dropped
            .iter()
            .filter_map(|f| {
                let file = f.web_file().cloned();
                if file.is_none() {
                    log::error!("a dropped file without its browser file");
                }
                file
            })
            .collect();
        if !files.is_empty() {
            let (inbox, notes, ctx) = (self.inbox.clone(), self.notes.clone(), ctx.clone());
            wasm_bindgen_futures::spawn_local(async move {
                read_together(files, "", &inbox, notes).await;
                ctx.request_repaint();
            });
        }
        let notes = std::mem::take(&mut *self.notes.lock().unwrap_or_else(|e| e.into_inner()));
        for (text, error) in notes {
            self.app.session.notify(text, error);
        }
        if let Some(embed) = &mut self.embed {
            embed.exchange(&mut self.app, ctx);
        }
        self.app.logic(ctx);
        self.dirty.set(self.app.session.dirty());
        // The shortcuts can be changed in the app: the key listener follows, rebuilt only when
        // the keymap changed.
        let print = self.app.shortcuts_fingerprint();
        if self.taken_print != Some(print) {
            self.taken_print = Some(print);
            *self.taken.borrow_mut() = self.app.command_keys();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.ui(ui);
    }
}

/// Reads files picked or dropped together into the inbox, all at once, each Shapefile's loose
/// parts zipped into one (`gather_shapefiles`) so it opens as a layer; `mark` starts each name.
async fn read_together(files: Vec<web_sys::File>, mark: &str, inbox: &Inbox, notes: Notes) {
    let mut read = Vec::new();
    for file in files {
        let name = file.name();
        let shortcut = mark == rustgis_ui_egui::SHORTCUT_PICK || (mark.is_empty() && name.to_ascii_lowercase().ends_with(".rgisshortcuts"));
        if shortcut && file.size() > rustgis_ui_egui::SHORTCUT_FILE_MAX_BYTES as f64 {
            notes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((rustgis_ui_egui::tl!("Shortcut file exceeds the size limit.").into(), true));
            continue;
        }
        if let Some(bytes) = crate::parts::read_file(file, notes.clone()).await {
            read.push((name, bytes));
        }
    }
    let (read, problems) = rustgis_ui_egui::rustgis_engine::data::io::gather_shapefiles(read);
    notes.lock().unwrap_or_else(|e| e.into_inner()).extend(problems.into_iter().map(|p| (p, true)));
    inbox.lock().unwrap_or_else(|e| e.into_inner()).extend(read.into_iter().map(|(name, bytes)| (format!("{mark}{name}"), bytes)));
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten())
}

/// Decodes a PNG, JPEG or WebP picture with the browser's decoder (`createImageBitmap`, off the
/// page's thread) into the tile cache's RAW pixels: decoded in WebAssembly instead, a map's
/// arriving tiles held up its frames for tens of milliseconds each.
async fn decode_picture(bytes: &[u8]) -> Result<Vec<u8>, wasm_bindgen::JsValue> {
    use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache::RAW;
    let window = web_sys::window().ok_or("no window")?;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts)?;
    // No premultiplying or colour conversion on the way in (the canvas premultiplies, so pixels
    // nearly transparent come back a little rounded: no matter for a map tile).
    let options = web_sys::ImageBitmapOptions::new();
    options.set_premultiply_alpha(web_sys::PremultiplyAlpha::None);
    options.set_color_space_conversion(web_sys::ColorSpaceConversion::None);
    let bitmap: web_sys::ImageBitmap =
        wasm_bindgen_futures::JsFuture::from(window.create_image_bitmap_with_blob_and_image_bitmap_options(&blob, &options)?)
            .await?
            .dyn_into()?;
    let (w, h) = (bitmap.width(), bitmap.height());
    let pixels = read_bitmap(&bitmap, w, h);
    // Closed whatever happened: its memory goes now, not when the garbage collector finds it.
    bitmap.close();
    let pixels = pixels?;
    let mut raw = Vec::with_capacity(RAW.len() + 8 + pixels.len());
    raw.extend_from_slice(RAW);
    raw.extend_from_slice(&w.to_le_bytes());
    raw.extend_from_slice(&h.to_le_bytes());
    raw.extend_from_slice(&pixels);
    Ok(raw)
}

/// The straight RGBA pixels of a `w × h` bitmap, drawn into a canvas read back on the CPU.
fn read_bitmap(bitmap: &web_sys::ImageBitmap, w: u32, h: u32) -> Result<Vec<u8>, wasm_bindgen::JsValue> {
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return Err("a picture of the wrong size".into());
    }
    let canvas = web_sys::OffscreenCanvas::new(w, h)?;
    let attributes = js_sys::Object::new();
    js_sys::Reflect::set(&attributes, &"willReadFrequently".into(), &true.into())?;
    let ctx: web_sys::OffscreenCanvasRenderingContext2d =
        canvas.get_context_with_context_options("2d", &attributes)?.ok_or("no 2D canvas")?.dyn_into()?;
    ctx.draw_image_with_image_bitmap(bitmap, 0.0, 0.0)?;
    Ok(ctx.get_image_data(0.0, 0.0, f64::from(w), f64::from(h))?.data().0)
}

/// How long a tile or document fetch may go without anything coming in: the answer's headers,
/// or the next part of its body. A slow but steady answer is never cut off; one that stalls is
/// given up, so it doesn't stay under way forever, holding its server's place in the queue.
/// Downloads the user asked for have none.
const STALL_MS: i32 = 30_000;

/// What a step of a fetch waits for, or `None` once it has gone [`STALL_MS`] with nothing (the
/// fetch is then aborted through `abort`).
async fn unless_stalled(
    step: js_sys::Promise,
    abort: Option<&web_sys::AbortController>,
) -> Option<Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>> {
    let Some(abort) = abort else { return Some(wasm_bindgen_futures::JsFuture::from(step).await) };
    let stalled = wasm_bindgen::JsValue::from_str("rustgis-stalled");
    let marker = stalled.clone();
    let handle = std::cell::Cell::new(None);
    let timer = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            handle.set(w.set_timeout_with_callback_and_timeout_and_arguments_1(&resolve, STALL_MS, &marker).ok());
        }
    });
    let raced = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::race(&js_sys::Array::of2(&step, &timer))).await;
    // Answered in time: the timer goes, rather than one waiting out every step of every fetch.
    if let (Some(w), Some(h)) = (web_sys::window(), handle.get()) {
        w.clear_timeout_with_handle(h);
    }
    if raced.as_ref().is_ok_and(|v| *v == stalled) {
        abort.abort();
        return None;
    }
    Some(raced)
}

/// Downloads `url`, or the byte range it asks for (a PMTiles archive's part: see
/// `rustgis_tiles::cache::split_range`).
/// A status in `omit` is an expected answer (a tile past a map's edge): its failure makes no
/// record of its own, as on the desktop.
async fn fetch_bytes(url: &str, capture: Capture, omit: &'static [u16]) -> Result<Vec<u8>, String> {
    use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::{cache::RequestDiagnostics, failure::Failure};
    let mut diagnostics = RequestDiagnostics::with_capture(url, "GET", capture);
    diagnostics.omit_statuses(omit);
    fetch_bytes_inner(url, &mut diagnostics).await.inspect_err(|error| diagnostics.failed(&Failure::new(url, error)))
}

async fn fetch_bytes_inner(
    url: &str,
    diagnostics: &mut rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache::RequestDiagnostics<'_>,
) -> Result<Vec<u8>, String> {
    let err = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let window = web_sys::window().ok_or("no window")?;
    use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache;
    // A data download carries its caller's budget (at most MAX_DOWNLOAD); other fetches use 64 MB.
    let (url, limit) = cache::split_download_limit(url);
    let max = limit.map(|n| n as u64).unwrap_or(64 << 20);
    let (url, tail) = cache::split_tail(url);
    let (url, range) = cache::split_range(url);
    // An `s3://` address becomes its web address, and a request to an S3 connection's endpoint
    // is signed (the bucket's CORS rules must allow the Authorization and x-amz-* headers).
    let prepared = rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::s3::prepare(url)?;
    let url = prepared.url.as_str();
    // An ArcGIS sign-in's token, for the portal and the servers it covers, renewed first when it
    // is about to expire. It goes as the request's `token` parameter, not a header: ArcGIS's
    // query endpoints answer the header's CORS preflight with an error.
    if !prepared.signed {
        crate::arcgis::renew_if_due(url).await;
    }
    let arcgis = if prepared.signed { None } else { rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::arcgis_auth::with_token(url) };
    // Only the request carries it: messages name the address without it.
    let request = arcgis.as_deref().unwrap_or(url);
    let asked = match (range, tail) {
        (Some((a, b)), _) => Some(format!("bytes={a}-{b}")),
        (None, Some(n)) => Some(format!("bytes=-{n}")),
        (None, None) => None,
    };
    let init = web_sys::RequestInit::new();
    // A tile or document that stalls is given up (see `STALL_MS`).
    let abort = if limit.is_none() { web_sys::AbortController::new().ok() } else { None };
    if let Some(a) = &abort {
        init.set_signal(Some(&a.signal()));
    }
    let stalled = || format!("nothing came in {} seconds", STALL_MS / 1000);
    if asked.is_some() || !prepared.headers.is_empty() {
        let headers = web_sys::Headers::new().map_err(err)?;
        if let Some(r) = &asked {
            headers.set("Range", r).map_err(err)?;
        }
        for (k, v) in &prepared.headers {
            headers.set(k, v).map_err(err)?;
        }
        init.set_headers(&headers);
    }
    // A signed or authorized request goes only where its credentials are for.
    if prepared.signed || arcgis.is_some() {
        init.set_redirect(web_sys::RequestRedirect::Error);
    }
    // The token's address goes nowhere else: no referrer from it, no cookies with it.
    if arcgis.is_some() {
        init.set_referrer_policy(web_sys::ReferrerPolicy::NoReferrer);
        init.set_credentials(web_sys::RequestCredentials::Omit);
    }
    let promise = window.fetch_with_str_and_init(request, &init);
    // A refused fetch is an opaque TypeError ("Failed to fetch") by design: the browser doesn't
    // say why, so the likely reasons are named, with the browser's own message but not its stack.
    let resp: web_sys::Response = unless_stalled(promise, abort.as_ref())
        .await
        .ok_or_else(|| {
            diagnostics.timed_out();
            stalled()
        })?
        .map_err(|e| {
            let why = e.dyn_ref::<js_sys::Error>().map(|e| String::from(e.message())).unwrap_or_else(|| err(e));
            format!(
                "the browser couldn't reach it (offline, blocked by a browser extension or the network, or the server doesn't accept requests from web pages): {why}"
            )
        })?
        .dyn_into()
        .map_err(err)?;
    diagnostics.answered(resp.status());
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    // The last bytes asked for and the whole file answered: fine when it is no longer.
    let whole_tail = tail.filter(|_| resp.status() != 206);
    let max = match whole_tail {
        Some(n) => n.saturating_add(1).min(max),
        None => max,
    };
    // A range asked for and the whole file answered (200): fine for a range from the start that
    // holds the whole file (a small archive), checked once it's read; else no ranges.
    let whole = range.is_some() && resp.status() != 206;
    if range.is_some_and(|(a, _)| whole && a > 0) {
        return Err(format!("{url}: {}", cache::NO_RANGES));
    }
    // Fonts are at most about 10 MB and plugins 64 MB; refuse anything larger than allowed, by
    // header when there is one and by the downloaded size otherwise.
    if let Ok(Some(len)) = resp.headers().get("content-length").map(|v| v.and_then(|s| s.parse::<u64>().ok()))
        && len > max
    {
        // The last bytes asked for and a longer file answered: no ranges, so it is downloaded whole.
        if whole_tail.is_some() {
            return Err(format!("{url}: {}", cache::NO_RANGES));
        }
        return Err(format!("{len} bytes is too large"));
    }
    // Read a chunk at a time, so a body larger than allowed (whatever its header said) is cut
    // off as soon as it is, never held whole.
    // Sized by the header when there is one (checked above), so the chunks don't reallocate.
    let len = resp.headers().get("content-length").ok().flatten().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let mut bytes: Vec<u8> = Vec::with_capacity(usize::try_from(len.min(max)).unwrap_or(0));
    if let Some(body) = resp.body() {
        let reader: web_sys::ReadableStreamDefaultReader =
            body.get_reader().dyn_into().map_err(|_| "the response has no readable body".to_string())?;
        loop {
            // Headers came and then the body stalled: given up as a fetch that doesn't answer.
            // (Aborting the fetch has cancelled its body too.)
            let Some(chunk) = unless_stalled(reader.read(), abort.as_ref()).await else {
                diagnostics.timed_out();
                return Err(stalled());
            };
            let chunk = chunk.map_err(err)?;
            if js_sys::Reflect::get(&chunk, &"done".into()).ok().and_then(|d| d.as_bool()).unwrap_or(true) {
                break;
            }
            let value = js_sys::Uint8Array::new(&js_sys::Reflect::get(&chunk, &"value".into()).map_err(err)?);
            let n = value.length() as usize;
            if (bytes.len() + n) as u64 > max {
                let _ = reader.cancel();
                if whole_tail.is_some() {
                    return Err(format!("{url}: {}", cache::NO_RANGES));
                }
                return Err(format!("larger than {max} bytes"));
            }
            let at = bytes.len();
            bytes.resize(at + n, 0);
            value.copy_to(bytes.get_mut(at..).unwrap_or_default());
        }
    }
    if let Some((a, b)) = range.filter(|_| whole)
        && bytes.len() as u64 > b - a + 1
    {
        return Err(format!("{url}: {}", cache::NO_RANGES));
    }
    if whole_tail.is_some_and(|n| bytes.len() as u64 > n) {
        return Err(format!("{url}: {}", cache::NO_RANGES));
    }
    Ok(bytes)
}

/// POSTs a JSON document and reads the answer (at most the tile cache's `MAX_BYTES`), without
/// following redirects; an error status is `HTTP <status>`.
async fn post_json(url: &str, body: &str, capture: Capture) -> Result<Vec<u8>, String> {
    use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::{cache::RequestDiagnostics, failure::Failure};
    let mut diagnostics = RequestDiagnostics::with_capture(url, "POST", capture);
    post_json_inner(url, body, &mut diagnostics).await.inspect_err(|error| diagnostics.failed(&Failure::new(url, error)))
}

async fn post_json_inner(
    url: &str,
    body: &str,
    diagnostics: &mut rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache::RequestDiagnostics<'_>,
) -> Result<Vec<u8>, String> {
    use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache;
    let err = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let window = web_sys::window().ok_or("no window")?;
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_redirect(web_sys::RequestRedirect::Error);
    let headers = web_sys::Headers::new().map_err(err)?;
    headers.set("Content-Type", "application/json").map_err(err)?;
    headers.set("Accept", "application/json").map_err(err)?;
    init.set_headers(&headers);
    init.set_body(&wasm_bindgen::JsValue::from_str(body));
    let resp: web_sys::Response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str_and_init(url, &init))
        .await
        .map_err(|_| "the browser couldn't reach it (offline, or the server doesn't accept requests from web pages)".to_string())?
        .dyn_into()
        .map_err(err)?;
    diagnostics.answered(resp.status());
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer().map_err(err)?).await.map_err(err)?;
    let bytes = js_sys::Uint8Array::new(&buf);
    if bytes.length() as usize > cache::MAX_BYTES {
        return Err(format!("{} bytes is too large", bytes.length()));
    }
    Ok(bytes.to_vec())
}

/// Asks for an address's headers (`HEAD`): a TIFF header's announced length and the type. The
/// browser shows a page `X-TIFF-Header-Length` only when the server exposes it
/// (`Access-Control-Expose-Headers`); without it, the header is read as any other file's.
async fn head(url: &str, capture: Capture) -> Result<rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache::Head, String> {
    use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::{cache::RequestDiagnostics, failure::Failure};
    let mut diagnostics = RequestDiagnostics::with_capture(url, "HEAD", capture);
    head_inner(url, &mut diagnostics).await.inspect_err(|error| diagnostics.failed(&Failure::new(url, error)))
}

async fn head_inner(
    url: &str,
    diagnostics: &mut rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache::RequestDiagnostics<'_>,
) -> Result<rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache::Head, String> {
    use rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::{cache, s3};
    let err = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let window = web_sys::window().ok_or("no window")?;
    let prepared = s3::prepare(url)?;
    // A request signed for S3 is signed for a GET.
    if prepared.signed {
        return Ok(cache::Head::default());
    }
    let init = web_sys::RequestInit::new();
    init.set_method("HEAD");
    if !prepared.headers.is_empty() {
        let headers = web_sys::Headers::new().map_err(err)?;
        for (k, v) in &prepared.headers {
            headers.set(k, v).map_err(err)?;
        }
        init.set_headers(&headers);
    }
    let resp: web_sys::Response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str_and_init(&prepared.url, &init))
        .await
        .map_err(|_| "the browser couldn't reach it (offline, or the server doesn't accept requests from web pages)".to_string())?
        .dyn_into()
        .map_err(err)?;
    diagnostics.answered(resp.status());
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let header = |k: &str| resp.headers().get(k).ok().flatten();
    Ok(cache::Head {
        tiff_header_length: header("x-tiff-header-length").as_deref().and_then(cache::tiff_header_length),
        content_type: header("content-type"),
        // Seen only where the server exposes it to pages.
        accepts_ranges: header("accept-ranges").as_deref().is_some_and(cache::accepts_byte_ranges),
    })
}

/// Makes an HTTP request (the AI assistant's calls to its model), returning the response of any
/// status.
async fn http(
    req: rustgis_ui_egui::rustgis_assistant::HttpRequest,
) -> Result<rustgis_ui_egui::rustgis_assistant::HttpResponse, rustgis_ui_egui::rustgis_assistant::Error> {
    use rustgis_ui_egui::rustgis_assistant::provider::{allowed_url, reply_limit};
    use rustgis_ui_egui::rustgis_assistant::{Error, error::TransportFailure};
    let limit = reply_limit(&req);
    let err = |e: wasm_bindgen::JsValue| format!("{e:?}");
    if !allowed_url(&req.url) {
        return Err(format!("{}: only HTTPS addresses, or plain HTTP on this computer or network, are allowed", req.url).into());
    }
    let window = web_sys::window().ok_or("no window")?;
    let init = web_sys::RequestInit::new();
    init.set_method(req.method);
    // The API key is never carried on to another address.
    init.set_redirect(web_sys::RequestRedirect::Error);
    let headers = web_sys::Headers::new().map_err(err)?;
    for (k, v) in &req.headers {
        headers.set(k, v).map_err(err)?;
    }
    init.set_headers(&headers);
    if let Some(body) = &req.body {
        init.set_body(&wasm_bindgen::JsValue::from_str(body));
    }
    // A failed fetch here is usually the provider refusing browser requests (CORS) or no network.
    let resp: web_sys::Response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str_and_init(&req.url, &init))
        .await
        .map_err(|_| {
            Error::transport(
                TransportFailure::Unavailable,
                "couldn't reach the provider (offline, or it doesn't accept requests from web pages)",
            )
        })?
        .dyn_into()
        .map_err(err)?;
    let status = resp.status();
    // The headers the assistant reads (a browser shows only those the provider exposes).
    let headers: Vec<(String, String)> = rustgis_ui_egui::rustgis_assistant::provider::KEPT_HEADERS
        .into_iter()
        .filter_map(|h| Some((h.to_string(), resp.headers().get(h).ok().flatten()?)))
        .collect();
    // Too large by its header: refused before it is read.
    if let Ok(Some(len)) = resp.headers().get("content-length").map(|v| v.and_then(|s| s.parse::<u64>().ok()))
        && len > limit as u64
    {
        return Err(format!("{len} bytes is too large").into());
    }
    let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer().map_err(err)?).await.map_err(err)?;
    let bytes = js_sys::Uint8Array::new(&buf);
    if bytes.length() as usize > limit {
        return Err(format!("{} bytes is too large", bytes.length()).into());
    }
    Ok(rustgis_ui_egui::rustgis_assistant::HttpResponse { status, body: bytes.to_vec(), headers })
}

/// The browser's clipboard, where a page may read it: a secure context (HTTPS, or this computer)
/// whose `navigator.clipboard` has `readText`.
fn clipboard() -> Option<web_sys::Clipboard> {
    let window = web_sys::window().filter(web_sys::Window::is_secure_context)?;
    let navigator = window.navigator();
    let clipboard = js_sys::Reflect::get(&navigator, &"clipboard".into()).ok().filter(|c| !c.is_null_or_undefined())?;
    js_sys::Reflect::get(&clipboard, &"readText".into()).ok().filter(wasm_bindgen::JsValue::is_function)?;
    Some(navigator.clipboard())
}

/// The browser's services. Embedded, settings live in memory for the visit: the page that embeds
/// the app must not read what the user saved on the site.
fn services(inbox: Inbox, font_inbox: Inbox, notes: Notes, ctx: egui::Context, embedded: bool) -> Services {
    // Map tiles come through the browser's fetch (the app repaints when they arrive).
    rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache::set_fetcher_omitting(|url, done, omit| {
        let capture = Capture::current();
        wasm_bindgen_futures::spawn_local(async move {
            let result = fetch_bytes(&url, capture.clone(), omit).await;
            let _scope = capture.enter();
            done(result);
        });
    });
    // Their pictures are decoded by the browser, off the page's thread.
    rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache::set_image_decoder(|bytes, done| {
        wasm_bindgen_futures::spawn_local(async move {
            match decode_picture(&bytes).await {
                Ok(raw) => done(Ok(raw)),
                // The browser can't: decoded in WebAssembly instead.
                Err(e) => {
                    log::debug!("the browser didn't decode a picture: {e:?}");
                    done(Err(bytes))
                }
            }
        });
    });
    // So are HEAD requests (a GeoTIFF made on request saying how long its header is).
    rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache::set_header(|url, done| {
        let capture = Capture::current();
        wasm_bindgen_futures::spawn_local(async move {
            let result = head(&url, capture.clone()).await;
            let _scope = capture.enter();
            done(result);
        });
    });
    // So are JSON documents POSTed (the NLDI's flowtrace).
    rustgis_ui_egui::rustgis_engine::render::rustgis_tiles::cache::set_poster(|url, body, done| {
        let capture = Capture::current();
        wasm_bindgen_futures::spawn_local(async move {
            let result = post_json(&url, &body, capture.clone()).await;
            let _scope = capture.enter();
            done(result);
        });
    });
    let open_inbox = inbox.clone();
    let font_ctx = ctx.clone();
    let (gl_list_ctx, gl_run_ctx) = (ctx.clone(), ctx.clone());
    let font_sink = font_inbox.clone();
    let fetch_ctx = ctx.clone();
    let http_ctx = ctx.clone();
    let (load_pref, save_pref): (Box<dyn Fn(&str) -> Option<String>>, Box<dyn Fn(&str, &str)>) = if embedded {
        let prefs = Rc::new(RefCell::new(HashMap::<String, String>::new()));
        let saved = prefs.clone();
        (
            Box::new(move |key: &str| prefs.borrow().get(key).cloned()),
            Box::new(move |key: &str, value: &str| {
                saved.borrow_mut().insert(key.to_string(), value.to_string());
            }),
        )
    } else {
        (
            Box::new(|key: &str| storage().and_then(|s| s.get_item(&format!("rustgis.{key}")).ok().flatten())),
            Box::new(|key: &str, value: &str| {
                if let Some(s) = storage() {
                    let _ = s.set_item(&format!("rustgis.{key}"), value);
                }
            }),
        )
    };
    let arcgis_ctx = ctx.clone();
    let clipboard_ctx = ctx.clone();
    Services {
        // A text field's Paste: the page reads the clipboard (Firefox and Safari show a Paste
        // button to confirm, Chrome asks once); only over HTTPS, where the browser offers it.
        read_clipboard: clipboard().map(|clipboard| -> Box<dyn Fn(Slot<Result<String, String>>)> {
            Box::new(move |slot| {
                let (clipboard, ctx) = (clipboard.clone(), clipboard_ctx.clone());
                wasm_bindgen_futures::spawn_local(async move {
                    let read = wasm_bindgen_futures::JsFuture::from(clipboard.read_text()).await;
                    let text = read.map(|v| v.as_string().unwrap_or_default()).map_err(|e| {
                        // A refusal is a DOMException: its name says why (NotAllowedError).
                        js_sys::Reflect::get(&e, &"name".into()).ok().and_then(|n| n.as_string()).unwrap_or_else(|| format!("{e:?}"))
                    });
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(text);
                    ctx.request_repaint();
                });
            })
        }),
        arcgis_sign_in: Some(Box::new(move |portal, client_id, slot| crate::arcgis::sign_in(portal, client_id, slot, arcgis_ctx.clone()))),
        load_pref: Some(load_pref),
        save_pref: Some(save_pref),
        // No keychain in the browser: script on the page could read anything stored (#754).
        secrets: None,
        // No autosave in the browser yet: localStorage is too small for a project (#873).
        recovery: None,
        fetch_font: Some(Box::new(move |url: &str| {
            let (url, inbox, ctx) = (url.to_string(), font_sink.clone(), font_ctx.clone());
            let capture = Capture::current();
            wasm_bindgen_futures::spawn_local(async move {
                match fetch_bytes(&url, capture, &[]).await {
                    Ok(bytes) => {
                        inbox.lock().unwrap_or_else(|e| e.into_inner()).push((url, bytes));
                        ctx.request_repaint();
                    }
                    Err(e) => {
                        log::error!("couldn't load font {url}: {e}");
                        // Empty bytes tell the app the download failed.
                        inbox.lock().unwrap_or_else(|e| e.into_inner()).push((url, Vec::new()));
                        ctx.request_repaint();
                    }
                }
            });
        })),
        font_inbox: Some(font_inbox),
        open_async: Some(Box::new(move |purpose: &str| {
            let inbox = open_inbox.clone();
            let notes = notes.clone();
            let ctx = ctx.clone();
            // A file picked for Add Delimited Text carries its dialog's options.
            // So does a file picked for a tool's file parameter, which goes to the Tools pane.
            let tool_exts = purpose.strip_prefix(rustgis_ui_egui::TOOL_FILE_PURPOSE);
            let mark = match (purpose, tool_exts) {
                ("shortcuts", _) => rustgis_ui_egui::SHORTCUT_PICK,
                ("text", _) => rustgis_ui_egui::DELIMITED_PICK,
                ("shapefile", _) => rustgis_ui_egui::SHAPEFILE_PICK,
                (_, Some(_)) => rustgis_ui_egui::TOOL_FILE_PICK,
                _ => "",
            };
            let dialog = if let Some(exts) = tool_exts {
                let exts: Vec<&str> = exts.split(',').filter(|e| !e.is_empty()).collect();
                if exts.is_empty() { rfd::AsyncFileDialog::new() } else { rfd::AsyncFileDialog::new().add_filter(exts.join(", "), &exts) }
            } else if purpose == "shortcuts" {
                rfd::AsyncFileDialog::new().add_filter(rustgis_ui_egui::tl!("Keyboard shortcuts"), &["json", "rgisshortcuts"])
            } else if purpose == "shapefile" {
                rfd::AsyncFileDialog::new().add_filter("Shapefile", &["shp", "dbf", "shx", "prj", "cpg", "zip"])
            } else if purpose == "project" {
                rfd::AsyncFileDialog::new().add_filter(
                    rustgis_ui_egui::tl!("Projects and JSON data (RustGIS, GeoLibre)"),
                    &["rgis", "rgisx", "rgispkg", "geolibre", "json"],
                )
            } else if purpose == "qgisProject" {
                rfd::AsyncFileDialog::new().add_filter(rustgis_ui_egui::tl!("QGIS project"), &["qgs", "qgz"])
            } else if purpose == "item" {
                rfd::AsyncFileDialog::new().add_filter(
                    "RustGIS items",
                    &[
                        "rgismap",
                        "rgismpk",
                        "rgispkg",
                        "rgislyr",
                        "rgisstyle",
                        "rgisramp",
                        "rgistemplate",
                        "rgistask",
                        "rgisbookmarks",
                        "rgis",
                        "rgisx",
                    ],
                )
            } else if purpose == "ramp" {
                rfd::AsyncFileDialog::new().add_filter("Colour ramps", &["rgisramp", "xml", "qml", "txt", "clr", "cpt"])
            } else if purpose == "layout" {
                rfd::AsyncFileDialog::new().add_filter("RustGIS layout", &["rgislayout"])
            } else if purpose == "plugin" {
                rfd::AsyncFileDialog::new().add_filter("RustGIS plugin", &["rgplugin"])
            } else if purpose == "text" {
                rfd::AsyncFileDialog::new()
                    .add_filter("Delimited text and spreadsheets", &["csv", "tsv", "txt", "xlsx", "xlsm", "xlsb", "xls", "ods"])
            } else if purpose == "picture" {
                rfd::AsyncFileDialog::new().add_filter("Pictures", &["png", "jpg", "jpeg"])
            } else {
                rfd::AsyncFileDialog::new().add_filter("Vector and raster data", DATA_EXTS)
            };
            // Data may come several files at once (photos gather in one layer).
            let many = matches!(purpose, "data" | "shapefile");
            wasm_bindgen_futures::spawn_local(async move {
                let files: Vec<rfd::FileHandle> =
                    if many { dialog.pick_files().await.unwrap_or_default() } else { dialog.pick_file().await.into_iter().collect() };
                read_together(files.iter().map(|f| f.inner().clone()).collect(), mark, &inbox, notes).await;
                ctx.request_repaint();
            });
        })),
        download: Some(Box::new(|name: &str, bytes: Vec<u8>| download(name, &bytes))),
        inbox: Some(inbox),
        geolibre_manifests: Some(Box::new(move |slot| crate::geolibre::manifests(slot, gl_list_ctx.clone()))),
        geolibre_run: Some(Box::new(move |job, slot| crate::geolibre::run(job, slot, gl_run_ctx.clone()))),
        http: Some(Box::new(move |req, slot| {
            let ctx = http_ctx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let r = http(req).await;
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                ctx.request_repaint();
            });
        })),
        fetch: Some(Box::new(move |url: &str, slot| {
            let (url, ctx) = (url.to_string(), fetch_ctx.clone());
            let capture = Capture::current();
            wasm_bindgen_futures::spawn_local(async move {
                let r = fetch_bytes(&url, capture, &[]).await.map_err(|e| format!("{url}: {e}"));
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                ctx.request_repaint();
            });
        })),
    }
}

/// Saves bytes through a temporary `<a download>` link.
fn download(name: &str, bytes: &[u8]) -> Result<(), String> {
    let err = |e: wasm_bindgen::JsValue| format!("{e:?}");
    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let body = document.body().ok_or("no body")?;
    let a: web_sys::HtmlAnchorElement = document.create_element("a").map_err(err)?.dyn_into().map_err(|_| "not an anchor".to_string())?;
    let array = js_sys::Uint8Array::from(bytes);
    let parts = js_sys::Array::new();
    parts.push(&array);
    let props = web_sys::BlobPropertyBag::new();
    props.set_type("application/octet-stream");
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &props).map_err(err)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(err)?;
    a.set_href(&url);
    a.set_download(name);
    // Some browsers only follow links that are in the document.
    if let Err(error) = body.append_child(&a) {
        let _ = web_sys::Url::revoke_object_url(&url);
        return Err(err(error));
    }
    // Schedule cleanup before submission: a timer error must not mark an already offered file
    // as failed. The browser may start reading the URL after `click` returns.
    let cleanup_url = url.clone();
    let revoke = wasm_bindgen::closure::Closure::once_into_js(move || {
        let _ = web_sys::Url::revoke_object_url(&cleanup_url);
    });
    if let Err(error) = window.set_timeout_with_callback_and_timeout_and_arguments_0(revoke.unchecked_ref(), 10_000) {
        a.remove();
        let _ = web_sys::Url::revoke_object_url(&url);
        return Err(err(error));
    }
    a.click();
    a.remove();
    Ok(())
}

/// Keeps the browser from acting on the app's Ctrl (⌘) shortcuts too: ⌘D duplicating a layout
/// element mustn't bookmark the page, ⌘Y (redo) open its history, or ⌘= (zoom in) zoom it. eframe
/// stops only ⌘, ⌘O, ⌘P and ⌘S. Seen first (the capture phase), the default is prevented but the
/// key still reaches egui. Browsers keep a few for themselves whatever a page does (⌘T, ⌘N, ⌘W).
fn keep_app_shortcuts(taken: TakenKeys) -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;
    let on_key = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |e: web_sys::KeyboardEvent| {
        // AltGr reports as Ctrl+Alt on Windows: it types a character, it isn't a shortcut.
        if e.get_modifier_state("AltGraph") {
            return;
        }
        // As eframe reads it, Ctrl or ⌘ is egui's command on every platform, so the app's
        // shortcut runs on either and the browser's is kept from both.
        let command = e.ctrl_key() || e.meta_key();
        if RustGisApp::takes_browser_key(&taken.borrow(), &e.key(), &e.code(), command, e.alt_key(), e.shift_key()) {
            e.prevent_default();
        }
    });
    window.add_event_listener_with_callback_and_bool("keydown", on_key.as_ref().unchecked_ref(), true).map_err(|e| format!("{e:?}"))?;
    on_key.forget();
    Ok(())
}

/// Closing, reloading or leaving the tab with unsaved changes asks first.
fn guard_unload(dirty: Rc<Cell<bool>>) -> Result<(), String> {
    let window = web_sys::window().ok_or("no window")?;
    let on_unload = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::BeforeUnloadEvent)>::new(move |e: web_sys::BeforeUnloadEvent| {
        if dirty.get() {
            e.prevent_default();
            e.set_return_value("unsaved");
        }
    });
    window.add_event_listener_with_callback("beforeunload", on_unload.as_ref().unchecked_ref()).map_err(|e| format!("{e:?}"))?;
    on_unload.forget();
    Ok(())
}
