//! Background group import/refresh for both desktop and browser. Discovery and updates live in
//! engine commands; this state machine only supplies their downloaded documents.

use crate::{RustGisApp, Slot, fetch_into, fetch_into_limited, i18n};
use rustgis_engine::data::ogc::{self, Pages, Step};
use serde_json::{Value, json};
use std::collections::VecDeque;

pub struct Import {
    params: Value,
    refresh: bool,
    token: u64,
    /// The originating map: its index and name (names are unique among a project's maps), so a
    /// deleted or replaced map at that index cancels the import.
    map: (usize, String),
    slot: Slot<Result<Vec<u8>, String>>,
    document: Option<String>,
    current: Option<String>,
    remaining: VecDeque<String>,
    responses: Vec<Value>,
    bytes: usize,
}

/// A service whose layers are listed: its address, type, capabilities document and the further
/// documents its discovery read. `service.add` reads layers from them without fetching again.
pub struct ServiceSource {
    pub url: String,
    /// `wms`, `wmts`, `wfs`, `wcs`, `ogcapi`, `arcgis`, or `auto`.
    pub kind: &'static str,
    pub text: String,
    pub documents: Vec<Value>,
}

/// A service type as the add dialog and `ui.addService` name it, `auto` for any other text.
pub(crate) fn kind_of(t: &str) -> &'static str {
    match t {
        "wms" => "wms",
        "wmts" => "wmts",
        "wfs" => "wfs",
        "wcs" => "wcs",
        "ogcapi" => "ogcapi",
        "arcgis" => "arcgis",
        _ => "auto",
    }
}

/// Adds one of a service's layers (as `service.layers` lists it): features are downloaded in the
/// background as From URL… does, map layers are added now, and an OGC API layer or a WCS coverage
/// waits on a document (a step the app moves along). A feature layer is asked only for the
/// features crossing `area` (longitude/latitude) when given, whole otherwise. `dialog` tells where
/// the result is reported.
pub(crate) fn add_layer(
    app: &mut RustGisApp,
    src: &ServiceSource,
    l: &Value,
    time: &str,
    area: Option<[f64; 4]>,
    dialog: bool,
) -> Result<&'static str, String> {
    add_layer_from(app, src, l, time, area, dialog, None)
}

/// [`add_layer`] for a layer of the connection named `origin` (the Connections tree): once the
/// layer is added, now or when its document or download lands, Recent shows its source under that
/// name. An add that fails or is dropped is not renamed.
pub(crate) fn add_layer_from(
    app: &mut RustGisApp,
    src: &ServiceSource,
    l: &Value,
    time: &str,
    area: Option<[f64; 4]>,
    dialog: bool,
    origin: Option<&str>,
) -> Result<&'static str, String> {
    let name = l.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let title = l.get("title").and_then(Value::as_str).unwrap_or(&name).to_string();
    let offer = l.get("offer").cloned().unwrap_or_default();
    if offer.get("type").and_then(Value::as_str) == Some("features") {
        let items = offer.get("url").and_then(Value::as_str).unwrap_or("");
        // Over an area when its request can carry one (not a WFS 1.0's, nor across the
        // antimeridian): whole otherwise, as `service.add` does.
        let asked = area.and_then(|a| ogc::features_in_area(items, a));
        if area.is_some() && asked.is_none() {
            app.session.notify(
                crate::i18n::fmt("{name}: its request can't carry an area, so the whole layer is asked for", &[("name", &title)]),
                false,
            );
        }
        let items = asked.as_deref().unwrap_or(items);
        // The layer's bounds go along, for its features' axes (#1053).
        let bounds = l.get("bbox").cloned().unwrap_or_default();
        return crate::connectiontree::run_add_url(app, json!({"url": items, "name": title, "service": true, "bounds": bounds}), origin)
            .map(|_| tl!("Downloading…"));
    }
    let origin = origin.map(str::to_string);
    if offer.get("type").and_then(Value::as_str) == Some("coverage") {
        // A coverage: its description first, off the frame (a step asks for it).
        let describe = offer.get("describe").and_then(Value::as_str).unwrap_or("").to_string();
        let mut p = json!({"url": src.url, "layer": name, "text": src.text, "name": title, "documents": src.documents});
        if src.kind != "auto" {
            p["type"] = json!(src.kind);
        }
        let slot = crate::fetch_into(&describe);
        app.ui.service_steps.push(crate::ServiceStep::Coverage { params: p, slot, dialog, origin });
        return Ok(tl!("Describing"));
    }
    let mut p = json!({"url": src.url, "layer": name, "text": src.text, "documents": src.documents, "source_url": l.get("source_url")});
    if src.kind != "auto" {
        p["type"] = json!(src.kind);
    }
    if l.get("time").is_some() && !time.is_empty() {
        p["time"] = json!(time);
    }
    match l.get("tilesets").and_then(Value::as_str) {
        // An OGC API layer: its tileset's advertised template is read first, off the frame (a
        // step adds the layer).
        Some(list) => {
            p["name"] = json!(title);
            let fallback = offer.get("url").and_then(Value::as_str).unwrap_or("").to_string();
            let vector = offer.get("type").and_then(Value::as_str) == Some("vectorTiles");
            let slot = crate::fetch_into(list);
            let resolve = crate::TilesResolve { params: p, list: list.to_string(), doc: list.to_string(), read: 0, vector, fallback, slot };
            app.ui.service_steps.push(crate::ServiceStep::Tiles { resolve, dialog, origin });
            Ok(tl!("Reading the tileset of"))
        }
        None => crate::connectiontree::run_named(app, "service.add", p, origin.as_deref()).map(|_| tl!("Added")),
    }
}

pub fn start(app: &mut RustGisApp, p: &Value, refresh: bool) -> Result<(), String> {
    if !p.is_object() {
        return Err(tl!("Give the service's web address").into());
    }
    if refresh && p.get("group").is_none() && p.get("layer").is_none() {
        return Err(tl!("Select a layer in the Layers pane.").into());
    }
    if app.ui.service_import.is_some() {
        return Err(tl!("A service import or refresh is already running.").into());
    }
    let mut source = p.clone();
    source["sourceOnly"] = json!(true);
    let mut params = app.session.run("service.groupPlan", &source)?;
    if let Some(p) = params.as_object_mut() {
        p.remove("sourceOnly");
        // Refresh uses the stored connection, not a previous discovery result.
        if refresh {
            p.remove("text");
            p.remove("documents");
        }
    }
    let mut task = Import {
        params,
        refresh,
        token: app.session.download_started(),
        map: active_map(app),
        slot: Slot::default(),
        document: None,
        current: None,
        remaining: VecDeque::new(),
        responses: Vec::new(),
        bytes: 0,
    };
    if task.params.get("text").and_then(Value::as_str).is_some() {
        if let Err(e) = task.discover(app) {
            app.session.download_failed(task.token);
            return Err(e);
        }
    } else {
        let url = task.params.get("url").and_then(Value::as_str).unwrap_or_default();
        let kind = task.params.get("type").and_then(Value::as_str);
        let caps = rustgis_engine::data::ogc::capabilities_url(url, kind);
        task.slot = fetch_into(&caps);
        task.document = Some(caps);
    }
    app.dialogs.error = None;
    app.ui.service_import = Some(task);
    Ok(())
}

impl Import {
    fn discover(&mut self, app: &mut RustGisApp) -> Result<(), String> {
        let listing = app.session.run("service.groupPlan", &self.params)?;
        if let Some(next) = listing.get("next").and_then(Value::as_str) {
            self.document = Some(next.into());
            self.slot = fetch_into(next);
        } else {
            self.remaining = listing
                .get("layers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|l| l.get("offer").and_then(|o| o.get("url")).and_then(Value::as_str).map(str::to_string))
                .collect();
            self.next();
        }
        Ok(())
    }

    fn next(&mut self) {
        self.current = self.remaining.pop_front();
        if let Some(url) = &self.current {
            self.slot = fetch_into_limited(url, rustgis_engine::cmd::service::MAX_GROUP_BYTES.saturating_sub(self.bytes));
        }
    }

    fn landed(&mut self, app: &mut RustGisApp, bytes: Vec<u8>) -> Result<(), String> {
        if let Some(document) = self.document.take() {
            let text = crate::service_text(bytes).map_err(|_| tl!("The service's answer isn't UTF-8 text").to_string())?;
            if self.params.get("text").is_none() {
                self.params["text"] = json!(text);
                self.params["documents"] = json!([]);
            } else if let Some(docs) = self.params.get_mut("documents").and_then(Value::as_array_mut) {
                docs.push(json!({"url":document,"text":text}));
            } else {
                self.params["documents"] = json!([{"url":document,"text":text}]);
            }
            self.discover(app)
        } else {
            self.bytes = self.bytes.saturating_add(bytes.len());
            if self.bytes > rustgis_engine::cmd::service::MAX_GROUP_BYTES {
                return Err(tl!("Service group data exceeds 64 MB.").into());
            }
            let text = String::from_utf8(bytes).map_err(|_| tl!("The service's answer isn't UTF-8 text").to_string())?;
            self.responses.push(json!({"url":self.current.take(),"text":text}));
            self.next();
            Ok(())
        }
    }
}

fn active_map(app: &RustGisApp) -> (usize, String) {
    let i = app.session.project.active_map;
    (i, app.session.project.maps.get(i).map(|m| m.name.clone()).unwrap_or_default())
}

pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(mut task) = app.ui.service_import.take() else { return };
    if !app.session.download_is_pending(task.token) || active_map(app) != task.map {
        app.session.download_failed(task.token);
        return;
    }
    let landed = task.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(answer) = landed
        && let Err(e) = answer.and_then(|bytes| task.landed(app, bytes))
    {
        app.session.download_failed(task.token);
        app.session.notify(e.clone(), true);
        app.dialogs.error = Some(e);
        return;
    }
    if task.document.is_none() && task.current.is_none() {
        app.session.download_failed(task.token);
        task.params["responses"] = json!(task.responses);
        // The plan's connection parameters are not public refresh parameters.
        if task.refresh
            && let Some(p) = task.params.as_object_mut()
        {
            for key in ["url", "type", "name"] {
                p.remove(key);
            }
        }
        let result = app.run(if task.refresh { "service.refresh" } else { "service.import" }, task.params);
        match result {
            Ok(_) => {
                app.dialogs.info = Some(tl!("Done").into());
                app.map.invalidate();
            }
            Err(e) => app.dialogs.error = Some(e),
        }
    } else {
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
        app.ui.service_import = Some(task);
    }
}

/// A download that landed, with what it is added with: its id, address and `layer.addDownload`
/// parameters, and the connection it came from (named in Recent).
pub(crate) struct Landed {
    pub id: u64,
    pub url: String,
    pub params: Value,
    pub origin: Option<String>,
}

/// A service layer's features read page by page off the frame (#764), from the start (#1326): each
/// answer [`Pages`] asks for is fetched in the background and handed to it, until the collection
/// is read; then it is added as the download it began as.
pub struct PageRead {
    landed: Landed,
    pages: Box<Pages>,
    /// The bytes the answers still to come may take (the download's budget, less the features
    /// read).
    left: usize,
    slot: Slot<Result<Vec<u8>, String>>,
    /// When the answer was asked for: a layer's description that hangs is given up on.
    asked: web_time::Instant,
}

/// Starts reading a service's features (`landed`, its download started but nothing fetched yet)
/// in the background ([`poll_pages`]).
pub(crate) fn read_pages(app: &mut RustGisApp, landed: Landed) {
    let step = ogc::begin(&landed.url, ogc::max_paged_features());
    go_on(app, landed, rustgis_engine::render::rustgis_tiles::cache::MAX_DOWNLOAD, step);
}

/// An ArcGIS layer's description fetched in the background: at most `max` bytes, its download
/// given up on natively after [`ogc::DESCRIPTION_TIMEOUT`].
fn fetch_description(url: &str, max: usize) -> Slot<Result<Vec<u8>, String>> {
    let slot: Slot<Result<Vec<u8>, String>> = Slot::default();
    let land = slot.clone();
    rustgis_engine::render::rustgis_tiles::cache::download_within(
        url,
        max,
        ogc::DESCRIPTION_TIMEOUT,
        Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)),
    );
    slot
}

/// Hands each page that landed to its read, and adds the layers whose features are all read. A
/// read whose project was closed meanwhile is dropped.
pub fn poll_pages(app: &mut RustGisApp, ctx: &egui::Context) {
    if app.ui.service_pages.is_empty() {
        return;
    }
    for read in std::mem::take(&mut app.ui.service_pages) {
        if !app.session.download_is_pending(read.landed.id) {
            let url = &read.landed.url;
            app.session.notify(i18n::fmt("{url}: downloaded after its project was closed, so it wasn't added", &[("url", url)]), false);
            continue;
        }
        let answer = read.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        // A layer's description that doesn't come in time is given up on, and the layer's query
        // fetched as it is (natively its download ends then too; the browser's fetch keeps its
        // own time, and what it brings is dropped).
        let describing = read.pages.describing();
        let answer = answer.or_else(|| {
            (describing && read.asked.elapsed() >= ogc::DESCRIPTION_TIMEOUT)
                .then(|| Err("the layer's description took too long".to_string()))
        });
        match answer {
            Some(answer) => {
                // The description isn't the features: it leaves them the whole budget.
                let left = if describing { read.left } else { read.left.saturating_sub(answer.as_ref().map_or(0, Vec::len)) };
                let step = (*read.pages).land(answer);
                go_on(app, read.landed, left, step);
            }
            None => app.ui.service_pages.push(read),
        }
    }
    if !app.ui.service_pages.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
    }
}

/// The next answer fetched, or the collection added (with a note when some features were left
/// unread), or why it couldn't be fetched said.
fn go_on(app: &mut RustGisApp, landed: Landed, left: usize, step: Step) {
    match step {
        // Not given by `ogc::begin`, which reads in GeoJSON.
        Step::Decoded(..) => go_on(app, landed, left, Step::Failed("the pages were read as protocol buffers".into())),
        Step::Failed(e) => {
            app.session.download_failed(landed.id);
            let url = &landed.url;
            app.session.notify(if e.starts_with(url.as_str()) { e } else { format!("{url}: {e}") }, true);
        }
        Step::Fetch(pages) => {
            let slot = match rustgis_engine::render::rustgis_tiles::address::check(pages.next_url(), Some(pages.referrer())) {
                // A layer's description: small, and soon given up on.
                Ok(()) if pages.describing() => fetch_description(pages.next_url(), left.min(ogc::DESCRIPTION_MAX)),
                Ok(()) => fetch_into_limited(pages.next_url(), left),
                Err(e) => Slot::new(std::sync::Mutex::new(Some(Err(e)))),
            };
            app.ui.service_pages.push(PageRead { landed, pages, left, slot, asked: web_time::Instant::now() });
        }
        Step::Done(bytes, note) => {
            if let Some(note) = note {
                let name = landed.params.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or(&landed.url);
                app.session.notify(format!("{name}: {note}"), false);
            }
            app.add_landed(landed, bytes);
        }
    }
}
