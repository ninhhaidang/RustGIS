//! Background group import/refresh for both desktop and browser. Discovery and updates live in
//! engine commands; this state machine supplies their downloaded documents and preserves the
//! saved connection or anonymous provenance through every deferred add.

use crate::{RustGisApp, Slot, fetch_document_into, fetch_into_limited, i18n};
use rustgis_engine::data::ogc::{self, Pages, Step};
use rustgis_engine::render::rustgis_tiles::{address::Reach, cache};
use serde_json::{Value, json};
use std::collections::VecDeque;

/// A dialog-owned read-ahead job. Replacing or dropping its connection cancels parsing;
/// its result cannot be applied to a different connection's listing.
pub struct ListingRead {
    pub control: std::sync::Arc<rustgis_engine::analysis::control::Control>,
    pub slot: Slot<Result<rustgis_engine::cmd::reads::Read, String>>,
    /// A search result belongs to its original filter, even if typing continues meanwhile.
    pub filter: Option<String>,
}

impl Drop for ListingRead {
    fn drop(&mut self) {
        self.control.cancel();
    }
}

impl ListingRead {
    /// Runs the already admitted supplied-document read on its own desktop thread. Completion
    /// is polled by the owning connection; thread refusal is an ordinary dialog error.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start(s: &mut rustgis_engine::Session, p: &Value) -> Result<Self, String> {
        let read = rustgis_engine::cmd::reads::read_ahead(s, "service.layers", p)?;
        let control = rustgis_engine::analysis::control::Control::stoppable();
        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let (work_control, done) = (control.clone(), slot.clone());
        std::thread::Builder::new()
            .name("service-listing".into())
            .spawn(move || {
                let result = read.run(&work_control);
                *done.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
            })
            .map_err(|error| i18n::fmt("Could not start service discovery: {error}", &[("error", &error.to_string())]))?;
        Ok(Self { control, slot, filter: p.get("filter").and_then(Value::as_str).map(str::to_string) })
    }
}

/// Installs a listing only on the connection that owns its completed read, or starts its next
/// advertised fetch. Failed and cancelled reads clear the busy state beside their error.
pub fn listed(conn: &mut crate::ServiceConn, result: rustgis_engine::CmdResult, filter: Option<String>) {
    conn.pending = None;
    if let Some(filter) = filter {
        conn.search = Some(crate::ServiceSearch { filter, listing: result });
        conn.added = None;
        return;
    }
    match result {
        Ok(value) => {
            conn.error = None;
            conn.failure = None;
            conn.pending = value.get("next").and_then(Value::as_str).map(str::to_string);
            if let Some(next) = &conn.pending {
                conn.slot = crate::fetch_detailed_into(next);
            }
            conn.listing = Some(value);
            conn.added = None;
            conn.search = None;
        }
        Err(error) => conn.error = Some(error),
    }
}

pub struct Import {
    params: Value,
    refresh: bool,
    /// The saved web-service connection to record on imported layers, not a group-plan parameter.
    connection: Option<Value>,
    token: u64,
    /// The originating map: its index and name (names are unique among a project's maps), so a
    /// deleted or replaced map at that index cancels the import.
    map: (usize, String),
    slot: Slot<Result<Vec<u8>, String>>,
    document: Option<String>,
    current: Option<String>,
    /// The collections still to fetch, each with whether the document listing it was read from
    /// the internet ([`from_public`]).
    remaining: VecDeque<(String, bool)>,
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

/// Uses the same translated tileset progress text when discovery starts and when it retires.
pub(crate) fn reading_tileset(name: &str) -> String {
    i18n::fmt("Reading the tileset of {name}", &[("name", name)])
}

/// The line a queued add owns until it succeeds, fails or is cancelled.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn adding(name: &str) -> String {
    i18n::fmt("Adding {name}…", &[("name", name)])
}

/// Uses the same translated coverage progress text when discovery starts and when it retires.
pub(crate) fn describing(name: &str) -> String {
    i18n::fmt("Describing {name}", &[("name", name)])
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

/// A dialog's provenance is usable only for a saved service of the displayed source's origin.
pub(crate) fn connection_for<'a>(app: &RustGisApp, connection: Option<&'a str>, url: &str) -> Option<&'a str> {
    connection.filter(|id| {
        app.session.connections.find(id).is_some_and(|c| {
            rustgis_engine::service_logins::is_service(c)
                && rustgis_engine::connections::login_origin(c)
                    .is_some_and(|origin| rustgis_engine::render::rustgis_tiles::logins::origin(url).as_deref() == Some(origin.as_str()))
        })
    })
}

/// A linked document never acquires another origin's saved login, even without explicit provenance.
pub(crate) fn request_from(app: &mut RustGisApp, connection: Option<&str>, base: &str, url: &str) -> Result<String, String> {
    use rustgis_engine::render::rustgis_tiles::logins;
    if logins::origin(base).is_some_and(|origin| logins::origin(url).as_deref() == Some(origin.as_str())) {
        rustgis_engine::service_logins::request(&mut app.session, connection, url).map_err(|e| crate::connectionui::translated(&e))
    } else {
        Ok(logins::inner(url).to_string())
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
) -> Result<String, String> {
    let connection = dialog.then(|| app.ui.service.as_ref().and_then(|source| source.connection.clone())).flatten();
    add_layer_from(app, src, l, time, area, dialog, None, connection.as_deref())
}

/// [`add_layer`] for a saved connection: `connection` is its id, recorded on the resulting layer;
/// `origin` is its display name for Recent. Both survive deferred documents and downloads.
pub(crate) fn add_layer_from(
    app: &mut RustGisApp,
    src: &ServiceSource,
    l: &Value,
    time: &str,
    area: Option<[f64; 4]>,
    dialog: bool,
    origin: Option<&str>,
    connection: Option<&str>,
) -> Result<String, String> {
    let anonymous = l.get("anonymous").and_then(Value::as_bool) == Some(true);
    let public = from_public(l);
    let connection = connection_for(app, connection, &src.url).filter(|_| !anonymous);
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
        let mut params = json!({"url": items, "name": title, "service": true, "bounds": bounds});
        let anonymous = l.get("anonymous").and_then(Value::as_bool) == Some(true)
            || rustgis_engine::render::rustgis_tiles::logins::origin(&src.url)
                != rustgis_engine::render::rustgis_tiles::logins::origin(items);
        if anonymous {
            params["url"] = json!(rustgis_engine::data::login_address::inner(items));
            params["anonymous"] = json!(true);
        } else if let Some(connection) = connection {
            params["connection"] = json!(connection);
        }
        // An area asked for keeps the view, even on an empty map, as `service.add` does.
        if asked.is_some() {
            params["zoom"] = json!(false);
        }
        if public {
            params["fromDocument"] = json!(true);
        }
        return crate::connectiontree::run_add_url(app, params, origin).map(|_| downloading(&title));
    }
    let origin = origin.map(str::to_string);
    if offer.get("type").and_then(Value::as_str) == Some("coverage") {
        // A coverage: its description first, off the frame (a step asks for it).
        let describe = offer.get("describe").and_then(Value::as_str).unwrap_or("").to_string();
        let mut p = json!({"url": src.url, "layer": name, "text": src.text, "name": title, "documents": src.documents});
        if src.kind != "auto" {
            p["type"] = json!(src.kind);
        }
        if let Some(connection) = connection {
            p["connection"] = json!(connection);
        }
        let describe = if anonymous {
            p["anonymous"] = json!(true);
            p["url"] = json!(rustgis_engine::data::login_address::inner(&src.url));
            rustgis_engine::data::login_address::inner(&describe).to_string()
        } else {
            request_from(app, connection, &src.url, &describe)?
        };
        if public {
            cache::restrict_to_public(&describe);
        }
        // Recorded if read from the internet: the GetCoverage request built from it goes there too.
        let slot = crate::fetch_document_reaching(&describe);
        app.ui.service_steps.push(crate::ServiceStep::Coverage { params: p, describe, slot, dialog, origin, opened: app.session.opened });
        return Ok(describing(&title));
    }
    let mut p = json!({"url": src.url, "layer": name, "text": src.text, "documents": src.documents, "source_url": l.get("source_url")});
    if src.kind != "auto" {
        p["type"] = json!(src.kind);
    }
    if let Some(connection) = connection {
        p["connection"] = json!(connection);
    }
    if anonymous {
        p["anonymous"] = json!(true);
        p["url"] = json!(rustgis_engine::data::login_address::inner(&src.url));
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
            let list = if anonymous {
                rustgis_engine::data::login_address::inner(list).to_string()
            } else {
                request_from(app, connection, &src.url, list)?
            };
            if public {
                cache::restrict_to_public(&list);
            }
            // (Recording one read from the internet: what it names is fetched only from there.)
            let slot = crate::fetch_document_reaching(&list);
            let resolve = crate::TilesResolve {
                params: p,
                list: list.clone(),
                crossed_origin: anonymous
                    || rustgis_engine::render::rustgis_tiles::logins::origin(&src.url)
                        != rustgis_engine::render::rustgis_tiles::logins::origin(&list),
                doc: list,
                read: 0,
                withheld_addresses: 0,
                vector,
                fallback,
                slot,
            };
            app.ui.service_steps.push(crate::ServiceStep::Tiles { resolve, dialog, origin, opened: app.session.opened });
            Ok(reading_tileset(&title))
        }
        None => {
            #[cfg(not(target_arch = "wasm32"))]
            if app.reads_off_frame {
                return crate::readjob::run_service(app, p, &title, origin.as_deref()).map(|_| adding(&title));
            }
            crate::connectiontree::run_named(app, "service.add", p, origin.as_deref()).map(|_| added(&title))
        }
    }
}

/// Whether the document a listed layer `l` comes from (its `source_url`) was read from the
/// internet, as the app's fetch or the service's discovery recorded it: what the layer needs is
/// then fetched only from there, wherever its names point. Every layer `service.layers` lists has
/// a `source_url` (`layer_source` sets it for each one, which is the only way a listed layer
/// reaches here), so `false` for one without it is never a document's layer passed off as the
/// user's.
pub(crate) fn from_public(l: &Value) -> bool {
    l.get("source_url").and_then(Value::as_str).is_some_and(|source| cache::ceiling_for(source) == Reach::Public)
}

/// Starts grouped discovery under its original request, retaining the import target and byte budget.
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
    // ui.importService accepts `connection`, as service.import does. The discovery command does
    // not: keep it apart until the final add, without leaking it into service.groupPlan calls.
    let bound = rustgis_engine::service_logins::request_params(&mut app.session, p).map_err(|e| crate::connectionui::translated(&e))?;
    let mut source = bound.into_owned();
    let connection = if refresh { None } else { source.as_object_mut().and_then(|p| p.remove("connection")) };
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
    // A refresh reads its linked documents and layers through the connection its layers record,
    // which the plan names when every child agrees: not the origin's current choice.
    let connection = if refresh { params.get("connection").filter(|c| !c.is_null()).cloned() } else { connection };
    let mut task = Import {
        params,
        refresh,
        connection,
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
        let caps = match task.request(&caps) {
            Ok(caps) => caps,
            Err(e) => {
                app.session.download_failed(task.token);
                return Err(e);
            }
        };
        task.slot = fetch_document_into(&caps);
        task.document = Some(caps);
    }
    app.dialogs.error = None;
    app.ui.service_import = Some(task);
    Ok(())
}

impl Import {
    /// Anonymous service provenance forbids selecting a login, including for same-origin links.
    fn request(&self, url: &str) -> Result<String, String> {
        if self.params.get("anonymous").and_then(Value::as_bool) == Some(true) {
            return Ok(rustgis_engine::data::login_address::inner(url).to_string());
        }
        let base = self.params.get("url").and_then(Value::as_str).unwrap_or_default();
        // The plan already resolved this exact destination and its login (or absence).
        // Re-selecting from the original saved connection would undo a foreign hop. The plan
        // judged it by where the document naming it was read, and recorded one read from the
        // internet, so it is checked within that record, never by how the service's address is
        // spelled (a public-looking name may connect on the local network).
        let ceiling = cache::ceiling_for(base).min(cache::ceiling_for(url));
        rustgis_engine::render::rustgis_tiles::address::check_reach(url, ceiling).map_err(|e| crate::connectionui::translated(&e))?;
        Ok(url.to_string())
    }

    /// Consumes the engine plan verbatim: foreign-hop anonymity cannot reselect a login.
    fn discover(&mut self, app: &mut RustGisApp) -> Result<(), String> {
        let listing = app.session.run("service.groupPlan", &self.params)?;
        if listing.get("request").and_then(|p| p.get("anonymous")).and_then(Value::as_bool) == Some(true) {
            self.params["anonymous"] = json!(true);
            if let Some(params) = self.params.as_object_mut() {
                params.remove("connection");
            }
            self.connection = None;
        }
        if let Some(next) = listing.get("next").and_then(Value::as_str) {
            let next = self.request(next)?;
            self.slot = fetch_document_into(&next);
            self.document = Some(next);
        } else {
            self.remaining = listing
                .get("layers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|l| l.get("offer").and_then(|o| o.get("url")).and_then(Value::as_str).map(|u| (u.to_string(), from_public(l))))
                .collect();
            self.next()?;
        }
        Ok(())
    }

    /// Fetches one exact planned collection within the remaining shared byte budget.
    fn next(&mut self) -> Result<(), String> {
        let Some((url, public)) = self.remaining.pop_front() else {
            self.current = None;
            return Ok(());
        };
        let asked = self.request(&url)?;
        let max = rustgis_engine::cmd::service::MAX_GROUP_BYTES.saturating_sub(self.bytes);
        self.slot = if public {
            let slot = Slot::default();
            let land = slot.clone();
            cache::download_public_limited(&asked, max, Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)));
            slot
        } else {
            fetch_into_limited(&asked, max)
        };
        self.current = Some(url);
        Ok(())
    }

    /// Accumulates supplied documents or feature responses before the engine import/refresh.
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
            self.next()?;
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
        if let Some(connection) = task.connection {
            task.params["connection"] = connection;
        }
        let result = app.run(if task.refresh { "service.refresh" } else { "service.import" }, task.params);
        match result {
            Ok(_) => {
                app.dialogs.info = Some(tl!("Done").into());
                app.map.invalidate();
            }
            Err(e) => app.dialogs.error = Some(crate::connectionui::translated(&e)),
        }
    } else {
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
        app.ui.service_import = Some(task);
    }
}

/// The add-service dialog's line for a layer on its way: what the dialog shows when a layer's
/// download starts, and what [`RustGisApp::landed_in_dialog`] looks for when it lands (#1453).
pub(crate) fn downloading(name: &str) -> String {
    i18n::fmt("Downloading… {name}", &[("name", name)])
}

/// The add-service dialog's line for a layer added, at once or when its download lands.
pub(crate) fn added(name: &str) -> String {
    i18n::fmt("Added {name}", &[("name", name)])
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
    /// The widest network reach the answer was asked within; once it lands, where it was read
    /// ([`go_on`]).
    reach: Reach,
    slot: Slot<Result<(Vec<u8>, Reach), String>>,
    /// When the answer was asked for: a layer's description that hangs is given up on.
    asked: web_time::Instant,
}

/// Starts reading a service's features (`landed`, its download started but nothing fetched yet)
/// in the background ([`poll_pages`]): an ArcGIS layer whose description lists protocol buffers in
/// them, in pages.
pub(crate) fn read_pages(app: &mut RustGisApp, landed: Landed) {
    let step = ogc::begin_with_pbf(&landed.url, ogc::max_paged_features());
    go_on(app, landed, rustgis_engine::render::rustgis_tiles::cache::MAX_DOWNLOAD, Reach::Local, step);
}

/// How its ArcGIS service draws the layer being read, from its description, goes with it to be
/// added (#2080); why it couldn't be read is said (#2152), also before anything is fetched for a
/// query asking for a window of its own, which has no description.
fn drawn(app: &mut RustGisApp, landed: &mut Landed, step: &mut Step) {
    let Step::Fetch(pages) = step else { return };
    match pages.take_drawing_info() {
        Some(Ok(drawing)) => {
            if let Some(params) = landed.params.as_object_mut() {
                params.insert("drawingInfo".to_string(), drawing);
            }
        }
        Some(Err(note)) => noted(app, landed, Some(note)),
        None => {}
    }
}

/// Hands each page that landed to its read, and adds the layers whose features are all read. A
/// read whose project was closed meanwhile is dropped.
pub fn poll_pages(app: &mut RustGisApp, ctx: &egui::Context) {
    if app.ui.service_pages.is_empty() {
        return;
    }
    for read in std::mem::take(&mut app.ui.service_pages) {
        if !app.session.download_is_pending(read.landed.id) {
            let url = rustgis_engine::sources::address_in_message(&read.landed.url);
            let why = i18n::fmt("{url}: downloaded after its project was closed, so it wasn't added", &[("url", &url)]);
            app.session.request_failed(&read.landed.params, &why);
            app.landed_in_dialog(&read.landed.params, Err(&why));
            app.session.notify(why, false);
            continue;
        }
        let answer = read.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        // A description that takes too long is given up on; a feature page can be retried at a
        // smaller size. Native downloads end then too; a browser's late answer is dropped.
        let describing = read.pages.describing();
        let answer = answer.or_else(|| {
            let timeout = if describing { Some(ogc::DESCRIPTION_TIMEOUT) } else { read.pages.page_timeout() };
            timeout.filter(|timeout| read.asked.elapsed() >= *timeout).map(|_| Err("timeout: service page".to_string()))
        });
        match answer {
            Some(answer) => {
                // The description isn't the features: it leaves them the whole budget.
                let left = if describing { read.left } else { read.left.saturating_sub(answer.as_ref().map_or(0, |(b, _)| b.len())) };
                // Where the answer was read; one that failed leaves the reach it was asked within.
                let reach = answer.as_ref().map_or(read.reach, |(_, reach)| *reach);
                let step = (*read.pages).land(answer.map(|(bytes, _)| bytes));
                go_on(app, read.landed, left, reach, step);
            }
            None => app.ui.service_pages.push(read),
        }
    }
    if !app.ui.service_pages.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
    }
}

/// The address a paged read fetches next, judged within `reach`, where the page before it was
/// read (not by how that page's address is spelled). Its login comes only from the page that led
/// to it (the referrer), so a page reached through another origin stays anonymous even when it
/// points back at the first page's origin.
fn next_request(app: &mut RustGisApp, params: &Value, pages: &Pages, reach: Reach) -> Result<String, String> {
    rustgis_engine::render::rustgis_tiles::address::check_reach(pages.next_url(), reach)?;
    if params.get("anonymous").and_then(Value::as_bool) == Some(true) {
        return Ok(rustgis_engine::data::login_address::inner(pages.next_url()).to_string());
    }
    request_from(app, params.get("connection").and_then(Value::as_str), pages.referrer(), pages.next_url())
}

/// The next answer fetched, or the collection added (with a note when some features were left
/// unread), or why it couldn't be fetched said. Each answer is fetched within `reach`, where the
/// one before it was read (the first within the user's own reach), and within the layer's record:
/// a page read from the internet leads only there, as the engine's paging does.
pub(crate) fn go_on(app: &mut RustGisApp, mut landed: Landed, left: usize, reach: Reach, mut step: Step) {
    if let Err(e) = app.session.download_continues(landed.id) {
        app.session.request_failed(&landed.params, &e);
        app.landed_in_dialog(&landed.params, Err(&e));
        app.session.notify(e, true);
        return;
    }
    // Only for a download still going: one cancelled says nothing of how it would be drawn.
    drawn(app, &mut landed, &mut step);
    let still = app.session.download_still(landed.id);
    rustgis_engine::render::rustgis_tiles::cache::with_optional_still(still, || {
        match step {
            Step::Decoded(dataset, report, note) => {
                noted(app, &landed, note);
                app.add_landed_read(landed, *dataset, report);
            }
            Step::Failed(e) => {
                app.session.request_failed(&landed.params, &e);
                app.session.download_failed(landed.id);
                let url = &landed.url;
                let e = rustgis_engine::sources::message_with_address(url, &e);
                app.landed_in_dialog(&landed.params, Err(&e));
                app.session.notify(e, true);
            }
            Step::Fetch(pages) => {
                let reach = reach.min(cache::ceiling_for(&landed.url));
                let next = next_request(app, &landed.params, &pages, reach);
                let slot = match next {
                    // A layer's description: small, and soon given up on.
                    Ok(url) if pages.describing() => {
                        crate::fetch_within(&url, left.min(ogc::DESCRIPTION_MAX), Some(ogc::DESCRIPTION_TIMEOUT), reach)
                    }
                    Ok(url) => crate::fetch_within(&url, left, pages.page_timeout(), reach),
                    Err(e) => Slot::new(std::sync::Mutex::new(Some(Err(e)))),
                };
                app.ui.service_pages.push(PageRead { landed, pages, left, reach, slot, asked: web_time::Instant::now() });
            }
            Step::Done(bytes, note) => {
                noted(app, &landed, note);
                app.add_landed(landed, bytes);
            }
        }
    });
}

/// Says what was left unread of a service layer's features, by its name.
fn noted(app: &mut RustGisApp, landed: &Landed, note: Option<String>) {
    if let Some(note) = note {
        let name = landed.params.get("name").and_then(Value::as_str).filter(|n| !n.is_empty());
        let shown = name.map(str::to_string).unwrap_or_else(|| rustgis_engine::sources::address_in_message(&landed.url));
        app.session.notify(i18n::fmt("{name}: {note}", &[("name", &shown), ("note", &note)]), false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Add From Service records a document it read from the internet: the document it links to
    /// is fetched only from there, and a layer listed there is added restricted to it, however
    /// their names resolve. A service read from the local network links to its own documents as
    /// before.
    #[test]
    fn documents_read_from_the_internet_hold_what_they_link_to() {
        use rustgis_engine::render::rustgis_tiles::address::{Origin, Reach};
        let items = "https://dialog-links.example/collections/parcels/items";
        let landing = json!({"links":[{"rel":"data","type":"application/json","href":"https://dialog-links.example/collections"}]});
        let collections = json!({"collections":[{"id":"parcels","title":"Parcels","links":[{"rel":"items","type":"application/geo+json","href":items}]}]});
        for observed in [Reach::Public, Reach::Local] {
            let _turn = crate::fetching::turn();
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            let (landing, collections) = (landing.to_string(), collections.to_string());
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                let clean = cache::split_download_limit(&url).0.to_string();
                log.lock().unwrap().push((clean.clone(), ceiling));
                let body = if clean.contains("dialog-links.example") { &collections } else { &landing };
                // A fetch connects no wider than its ceiling.
                let origin = Origin::with_observed_reach(&clean, observed.min(ceiling)).unwrap();
                done(Ok(cache::Fetched { bytes: body.clone().into_bytes(), origin }));
            });
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            app.run("ui.addService", json!({"url": "https://dialog-root.example/ogc", "type": "ogcapi"})).unwrap();
            app.poll_service();
            app.poll_service();
            let conn = app.ui.service.as_ref().unwrap();
            let listing = conn.listing.clone().expect("both documents were read");
            {
                let asked = asked.lock().unwrap();
                assert_eq!(asked.len(), 2, "{asked:?}");
                assert_eq!(asked[0].1, Reach::Local, "the service's own address is the user's choice");
                assert!(asked[1].0.starts_with("https://dialog-links.example/collections"), "{asked:?}");
                assert_eq!(asked[1].1, observed, "the linked document's reach, observed {observed:?}");
            }
            let layer = &listing["layers"][0];
            assert_eq!(from_public(layer), observed == Reach::Public, "{listing}");
            let src = ServiceSource {
                url: conn.url.clone(),
                kind: "ogcapi",
                text: conn.text.clone().unwrap(),
                documents: conn.documents.clone(),
            };
            add_layer_from(&mut app, &src, layer, "", None, false, None, None).unwrap();
            // The features are read off the frame, page by page, within what their address records.
            let asked = layer["offer"]["url"].as_str().unwrap();
            assert!(asked.starts_with(items), "{asked}");
            assert_eq!(cache::ceiling_for(asked) == Reach::Public, observed == Reach::Public, "observed {observed:?}");
        }
    }

    /// The dialog reads a service's features page by page: a page read from the internet has the
    /// next page it links to fetched only from there, however its name resolves, even when the
    /// first page's address is the user's own. Pages read from the local network lead anywhere.
    #[test]
    fn feature_pages_read_from_the_internet_hold_the_next_page() {
        use rustgis_engine::render::rustgis_tiles::address::{Origin, Reach};
        let first = "https://pages-first.example/collections/parcels/items";
        let next = "https://pages-next.example/collections/parcels/items?page=2";
        for observed in [Reach::Public, Reach::Local] {
            let _turn = crate::fetching::turn();
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                let clean = cache::split_download_limit(&url).0.to_string();
                log.lock().unwrap().push((clean.clone(), ceiling));
                let links =
                    if clean.starts_with(first) { json!([{"rel":"next","type":"application/geo+json","href":next}]) } else { json!([]) };
                let body = json!({"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{}}],"links":links});
                // A fetch connects no wider than its ceiling.
                let origin = Origin::with_observed_reach(&clean, observed.min(ceiling)).unwrap();
                done(Ok(cache::Fetched { bytes: body.to_string().into_bytes(), origin }));
            });
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            app.run("ui.addUrl", json!({"url": first, "name": "Parcels", "service": true})).unwrap();
            for _ in 0..4 {
                poll_pages(&mut app, &egui::Context::default());
            }
            let asked = asked.lock().unwrap();
            assert_eq!(asked.len(), 2, "{asked:?}");
            assert_eq!(asked[0].1, Reach::Local, "the first page is the user's own address");
            assert!(asked[1].0.starts_with("https://pages-next.example/"), "{asked:?}");
            assert_eq!(asked[1].1, observed, "the next page, the first observed {observed:?}");
            let layer = app.session.map().layers.iter().find(|l| l.name == "Parcels");
            assert_eq!(layer.map(|l| l.dataset.features.len()), Some(2), "{:?}", app.dialogs.error);
        }
    }

    /// The next page is judged by where the page before it was read, not by how that page's
    /// address is spelled: a first page whose public-looking name connected on the local network
    /// may lead to a local address; one read from the internet may not.
    #[test]
    fn the_next_page_is_judged_by_where_the_page_before_it_was_read() {
        use rustgis_engine::render::rustgis_tiles::address::{Origin, Reach};
        let first = "https://gis-corp.example/collections/a/items";
        let next = "https://10.0.0.5/collections/a/items?page=2";
        for observed in [Reach::Local, Reach::Public] {
            let _turn = crate::fetching::turn();
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                let clean = cache::split_download_limit(&url).0.to_string();
                log.lock().unwrap().push((clean.clone(), ceiling));
                let links =
                    if clean.starts_with(first) { json!([{"rel":"next","type":"application/geo+json","href":next}]) } else { json!([]) };
                let body = json!({"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{}}],"links":links});
                let reach = if clean.starts_with(first) { observed } else { Reach::Local };
                let origin = Origin::with_observed_reach(&clean, reach.min(ceiling)).unwrap();
                done(Ok(cache::Fetched { bytes: body.to_string().into_bytes(), origin }));
            });
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            app.run("ui.addUrl", json!({"url": first, "name": "Corp", "service": true})).unwrap();
            for _ in 0..4 {
                poll_pages(&mut app, &egui::Context::default());
            }
            let asked = asked.lock().unwrap();
            let layer = app.session.map().layers.iter().find(|l| l.name == "Corp").map(|l| l.dataset.features.len());
            if observed == Reach::Local {
                assert_eq!(asked.len(), 2, "{asked:?}");
                assert_eq!(asked[1], (next.to_string(), Reach::Local), "{asked:?}");
                assert_eq!(layer, Some(2), "{:?}", app.dialogs.error);
            } else {
                assert_eq!(asked.len(), 1, "a page read from the internet can't lead to a local address: {asked:?}");
                assert_eq!(layer, Some(1), "the first page is kept");
            }
        }
    }

    /// A long paged read carries its reach from page to page instead of recording each page's
    /// address: every page after one read from the internet is asked only from there, and none of
    /// the pages' addresses is added to the restricted addresses, however many pages there are.
    #[test]
    fn feature_pages_record_none_of_their_addresses() {
        use rustgis_engine::render::rustgis_tiles::address::{Origin, Reach};
        const PAGES: usize = 40;
        let page = |n: usize| format!("https://pages-many.example/collections/parcels/items?page={n}");
        let _turn = crate::fetching::turn();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = asked.clone();
        cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
            let clean = cache::split_download_limit(&url).0.to_string();
            log.lock().unwrap().push((clean.clone(), ceiling));
            let n = clean.rsplit_once("page=").and_then(|(_, n)| n.parse::<usize>().ok()).unwrap_or(1);
            let links = if n < PAGES { json!([{"rel":"next","type":"application/geo+json","href":page(n + 1)}]) } else { json!([]) };
            let body = json!({"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{}}],"links":links});
            // Every page is on the internet; a fetch connects no wider than its ceiling.
            let origin = Origin::with_observed_reach(&clean, Reach::Public.min(ceiling)).unwrap();
            done(Ok(cache::Fetched { bytes: body.to_string().into_bytes(), origin }));
        });
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("ui.addUrl", json!({"url": page(1), "name": "Many", "service": true})).unwrap();
        for _ in 0..PAGES + 4 {
            poll_pages(&mut app, &egui::Context::default());
        }
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), PAGES, "{asked:?}");
        assert_eq!(asked[0].1, Reach::Local, "the first page is the user's own address");
        assert!(asked[1..].iter().all(|(_, ceiling)| *ceiling == Reach::Public), "{asked:?}");
        for n in 2..=PAGES {
            assert_eq!(cache::ceiling_for(&page(n)), Reach::Local, "page {n} was recorded");
        }
        let layer = app.session.map().layers.iter().find(|l| l.name == "Many");
        assert_eq!(layer.map(|l| l.dataset.features.len()), Some(PAGES), "{:?}", app.dialogs.error);
    }

    /// An OGC API layer's tile set documents, read by the dialog, hold what they name to where
    /// they were read from: under a listing from the local network, a tile set list read from the
    /// internet has the tile set it leads to fetched only from there, and the layer's template too.
    /// One read from the local network leaves both as they were.
    #[test]
    fn tile_set_documents_read_from_the_internet_hold_what_they_name() {
        use rustgis_engine::render::rustgis_tiles::address::{Origin, Reach};
        let root = "https://tileset-root.example/ogc";
        let list = "https://tileset-list.example/collections/relief/map/tiles";
        let tileset = "https://tileset-list.example/tiles/WebMercatorQuad";
        let template = "https://tileset-tiles.example/relief/{tileMatrix}/{tileRow}/{tileCol}.png";
        let collections = json!({"collections":[{"id":"relief","title":"Relief","links":[{"rel":"tilesets-map","href":list}]}]});
        for observed in [Reach::Public, Reach::Local] {
            let _turn = crate::fetching::turn();
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                let clean = cache::split_download_limit(&url).0.to_string();
                log.lock().unwrap().push((clean.clone(), ceiling));
                let body = if clean.starts_with(list) {
                    json!({"tilesets":[{"tileMatrixSetId":"WebMercatorQuad","links":[{"rel":"self","href":tileset}]}]})
                } else {
                    json!({"links":[{"rel":"item","type":"image/png","href":template}]})
                };
                // The list is where the test says; a fetch connects no wider than its ceiling.
                let reach = if clean.starts_with(list) { observed } else { Reach::Local };
                let origin = Origin::with_observed_reach(&clean, reach.min(ceiling)).unwrap();
                done(Ok(cache::Fetched { bytes: body.to_string().into_bytes(), origin }));
            });
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let text = collections.to_string();
            let listing = app.session.run("service.layers", &json!({"url": root, "type": "ogcapi", "text": text})).unwrap();
            let layer = listing["layers"].as_array().and_then(|l| l.iter().find(|l| l.get("tilesets").is_some())).unwrap().clone();
            assert!(!from_public(&layer), "the listing is the user's own: {listing}");
            let src = ServiceSource { url: root.into(), kind: "ogcapi", text, documents: Vec::new() };
            add_layer_from(&mut app, &src, &layer, "", None, false, None, None).unwrap();
            app.poll_service_steps();
            app.poll_service_steps();
            assert!(app.ui.service_steps.is_empty(), "both tile set documents were read");
            {
                let asked = asked.lock().unwrap();
                assert_eq!(asked.len(), 2, "{asked:?}");
                assert_eq!(asked[0].1, Reach::Local, "a document the user's listing names");
                assert!(asked[1].0.starts_with(tileset), "{asked:?}");
                assert_eq!(asked[1].1, observed, "the tile set, its list observed {observed:?}");
            }
            let added = app.session.map().tiles.last().map(|t| t.url.clone()).unwrap_or_default();
            assert!(added.starts_with("https://tileset-tiles.example/relief/"), "{added} {:?}", app.dialogs.error);
            assert_eq!(cache::ceiling_for(&added), observed, "the template, its list observed {observed:?}");
        }
    }

    /// A tile set list at a public-looking name read on the local network leads to the tile set it
    /// names on a private address: it is judged by where the list was read, not by how its name is
    /// spelled. The same list read from the internet has that tile set refused, naming it and why,
    /// and nothing more is fetched.
    #[test]
    fn a_tile_set_list_read_on_the_local_network_leads_to_its_local_tile_set() {
        use rustgis_engine::render::rustgis_tiles::address::{Origin, Reach};
        for (n, observed) in [(1, Reach::Local), (2, Reach::Public)] {
            let _turn = crate::fetching::turn();
            let root = format!("https://tileset-root-{n}.example/ogc");
            let list = format!("https://tileset-list-{n}.corp-looks-public.example/collections/relief/map/tiles");
            let tileset = format!("http://10.0.{n}.7/tiles/WebMercatorQuad");
            let template = format!("http://10.0.{n}.7/relief/{{tileMatrix}}/{{tileRow}}/{{tileCol}}.png");
            let collections = json!({"collections":[{"id":"relief","title":"Relief","links":[{"rel":"tilesets-map","href":list}]}]});
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            let (list_at, tileset_body) = (list.clone(), json!({"links":[{"rel":"item","type":"image/png","href":template}]}));
            let list_body = json!({"tilesets":[{"tileMatrixSetId":"WebMercatorQuad","links":[{"rel":"self","href":tileset}]}]});
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                let clean = cache::split_download_limit(&url).0.to_string();
                log.lock().unwrap().push((clean.clone(), ceiling));
                let body = if clean.starts_with(&list_at) { &list_body } else { &tileset_body };
                let origin = Origin::with_observed_reach(&clean, observed.min(ceiling)).unwrap();
                done(Ok(cache::Fetched { bytes: body.to_string().into_bytes(), origin }));
            });
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let text = collections.to_string();
            let listing = app.session.run("service.layers", &json!({"url": root, "type": "ogcapi", "text": text})).unwrap();
            let layer = listing["layers"].as_array().and_then(|l| l.iter().find(|l| l.get("tilesets").is_some())).unwrap().clone();
            let src = ServiceSource { url: root.clone(), kind: "ogcapi", text, documents: Vec::new() };
            add_layer_from(&mut app, &src, &layer, "", None, false, None, None).unwrap();
            app.poll_service_steps();
            app.poll_service_steps();
            let asked = asked.lock().unwrap().clone();
            if observed == Reach::Local {
                assert_eq!(asked.len(), 2, "{asked:?}");
                assert_eq!(asked[1], (format!("{tileset}?f=json"), Reach::Local), "{asked:?}");
                let added = app.session.map().tiles.last().map(|t| t.url.clone()).unwrap_or_default();
                assert!(added.starts_with(&format!("http://10.0.{n}.7/relief/")), "{added} {:?}", app.dialogs.error);
            } else {
                assert_eq!(asked.len(), 1, "the tile set is not fetched: {asked:?}");
                let told: Vec<_> = app.session.notices_since(0).iter().map(|n| n.text.clone()).collect();
                assert!(told.iter().any(|t| t.contains(&tileset) && t.contains("local network")), "{told:?}");
            }
        }
    }

    /// A grouped import's next document is judged by the plan's record of where the document
    /// naming it was read, not by how the service's address is spelled: under a service at a
    /// public-looking name read on the local network, a document on a private address is asked
    /// for; under one recorded as read from the internet, it is refused, naming it and why.
    #[test]
    fn a_grouped_import_judges_its_next_document_by_where_the_service_was_read() {
        use rustgis_engine::render::rustgis_tiles::address::Reach;
        for (n, observed) in [(3, Reach::Local), (4, Reach::Public)] {
            let _turn = crate::fetching::turn();
            let base = format!("https://import-{n}.corp-looks-public.example/ogc");
            let next = format!("http://10.0.{n}.8/collections");
            // The plan records a document it names after one read from the internet.
            if observed == Reach::Public {
                cache::restrict_to_public(&next);
            }
            let task = Import {
                params: json!({"url": base}),
                refresh: false,
                connection: None,
                token: 0,
                map: (0, String::new()),
                slot: Slot::default(),
                document: None,
                current: None,
                remaining: VecDeque::new(),
                responses: Vec::new(),
                bytes: 0,
            };
            match task.request(&next) {
                Ok(asked) => {
                    assert_eq!(observed, Reach::Local);
                    assert_eq!(asked, next);
                }
                Err(e) => {
                    assert_eq!(observed, Reach::Public, "{e}");
                    assert!(e.contains(&next) && e.contains("local network"), "{e}");
                }
            }
        }
    }

    /// A service a document read from the internet named (`fromDocument`: a catalogue's or a
    /// portal's entry) is read only from there: its capabilities and what they list. One whose
    /// address points into the local network is refused, naming it and why. Without it the
    /// address is the user's, as before.
    #[test]
    fn a_service_a_document_named_is_read_only_from_the_internet() {
        use rustgis_engine::render::rustgis_tiles::address::{Origin, Reach};
        for public in [true, false] {
            let _turn = crate::fetching::turn();
            let url = format!("https://named-{public}.example/wms");
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                let clean = cache::split_download_limit(&url).0.to_string();
                log.lock().unwrap().push((clean.clone(), ceiling));
                let origin = Origin::with_observed_reach(&clean, ceiling).unwrap();
                done(Ok(cache::Fetched { bytes: b"<WMS_Capabilities version=\"1.3.0\"/>".to_vec(), origin }));
            });
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            app.run("ui.addService", json!({"url": url, "type": "wms", "fromDocument": public})).unwrap();
            let caps = rustgis_engine::data::ogc::capabilities_url(&url, Some("wms"));
            let want = if public { Reach::Public } else { Reach::Local };
            assert_eq!(asked.lock().unwrap().clone(), [(caps.clone(), want)]);
            assert_eq!(cache::ceiling_for(&caps), want);
            if public {
                app.dialogs.open = None;
                let e = app.run("ui.addService", json!({"url": "http://10.0.5.9/wms", "type": "wms", "fromDocument": true})).unwrap_err();
                assert!(e.contains("10.0.5.9") && e.contains("local network"), "{e}");
                assert_eq!(asked.lock().unwrap().len(), 1, "a refused service is not asked");
                assert_eq!(app.dialogs.open, None, "nothing opens for a refused service");
            }
        }
    }

    /// Discovery's anonymous provenance survives both deferred protocols and their final params.
    #[test]
    fn anonymous_service_offers_keep_descriptions_and_tiles_unbound() {
        use rustgis_engine::render::rustgis_tiles::{cache, logins};
        let _turn = crate::fetching::turn();
        let caps = r#"<wcs:Capabilities version="2.0.1" xmlns:wcs="http://www.opengis.net/wcs/2.0" xmlns:ows="http://www.opengis.net/ows/2.0">
            <wcs:Contents><wcs:CoverageSummary><wcs:CoverageId>soc</wcs:CoverageId><ows:Title>Soil carbon</ows:Title>
            <ows:WGS84BoundingBox><ows:LowerCorner>5 49.2</ows:LowerCorner><ows:UpperCorner>5.8 50</ows:UpperCorner></ows:WGS84BoundingBox>
            </wcs:CoverageSummary></wcs:Contents></wcs:Capabilities>"#;
        let describe = br#"<CoverageDescriptions><CoverageDescription><Envelope srsName="http://www.opengis.net/def/crs/EPSG/0/4326" axisLabels="Lat Long">
            <lowerCorner>49.2 5</lowerCorner><upperCorner>50 5.8</upperCorner></Envelope></CoverageDescription></CoverageDescriptions>"#;
        for coverage in [false, true] {
            for anonymous in [true, false] {
                let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
                let url = "https://anonymous-offer.invalid/collections";
                let kind = if coverage { "wcs" } else { "ogcapi" };
                let id = app.run("connection.new", json!({"kind":kind, "address":url, "auth":"header", "keyName":"X-Key"})).unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .to_string();
                app.run("connection.login", json!({"id":id, "apiKey":"RGSYNTH_anonymous_offer"})).unwrap();
                let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                let capture = seen.clone();
                cache::set_fetcher(move |url, done| {
                    capture.lock().unwrap().push(cache::split_download_limit(&url).0.to_string());
                    done(Err("held description".into()));
                });
                let text = if coverage {
                    caps.to_string()
                } else {
                    json!({"collections":[{"id":"relief","links":[{"rel":"tilesets-map","href":"https://anonymous-offer.invalid/list"}]}]})
                        .to_string()
                };
                let src = ServiceSource { url: url.into(), kind, text, documents: vec![] };
                let listed = app.session.run("service.layers", &json!({"url":url,"type":kind,"text":src.text,"connection":id})).unwrap();
                let mut layer = listed["layers"][0].clone();
                layer["anonymous"] = json!(anonymous);
                add_layer_from(&mut app, &src, &layer, "", None, false, None, Some(&id)).unwrap();
                {
                    let seen = seen.lock().unwrap();
                    assert_eq!(seen.len(), 1);
                    assert_eq!(logins::apply(&seen[0]).unwrap().is_some(), !anonymous, "coverage={coverage}, anonymous={anonymous}");
                }
                let params = match &app.ui.service_steps[0] {
                    crate::ServiceStep::Coverage { params, slot, .. } => {
                        *slot.lock().unwrap() = Some(Ok((describe.to_vec(), Reach::Local)));
                        params
                    }
                    crate::ServiceStep::Tiles { resolve, .. } => {
                        assert_eq!(resolve.crossed_origin, anonymous);
                        *resolve.slot.lock().unwrap() = Some(Ok((br#"{"links":[{"rel":"item","type":"image/png","href":"https://anonymous-offer.invalid/{tileMatrix}/{tileRow}/{tileCol}"}]}"#.to_vec(), Reach::Local)));
                        &resolve.params
                    }
                };
                assert_eq!(params.get("connection").is_some(), !anonymous);
                assert_eq!(params.get("anonymous").and_then(Value::as_bool).unwrap_or(false), anonymous);
                // Deliver through the actual completion handler and its admitted final command.
                app.poll_service_steps();
                assert!(app.ui.service_steps.is_empty());
                if coverage {
                    let seen = seen.lock().unwrap();
                    assert_eq!(seen.len(), 2, "{:?}", app.session.notices);
                    assert!(seen[1].contains("REQUEST=GetCoverage"));
                    assert_eq!(logins::apply(&seen[1]).unwrap().is_some(), !anonymous);
                    let params = &app.ui.downloads.first().expect("the coverage download was dispatched").params;
                    assert_eq!(params.get("connection").and_then(Value::as_str), (!anonymous).then_some(id.as_str()));
                    assert_eq!(params.get("anonymous").and_then(Value::as_bool).unwrap_or(false), anonymous);
                } else {
                    let tile = app.session.map().tiles.first().expect("the resolved tiles were added");
                    assert_eq!(tile.anonymous, anonymous);
                    assert_eq!(tile.connection, if anonymous { "" } else { id.as_str() });
                    assert_eq!(logins::apply(&tile.url).unwrap().is_some(), !anonymous);
                }
            }
        }
    }

    /// A linked page reached through another origin is fetched anonymously even when it points
    /// back at the first page's origin: the login follows the referring page, not the first one.
    #[test]
    fn a_page_reached_through_another_origin_does_not_regain_the_login() {
        let mut app = RustGisApp::with_sample();
        let c = app
            .run("connection.new", json!({"kind":"ogcapi", "address":"https://maps.example.org", "auth":"header", "keyName":"X-Api-Key"}))
            .unwrap();
        let connection = c["id"].as_str().unwrap().to_string();
        let first =
            rustgis_engine::data::login_address::bind("0123456789abcdef", "https://maps.example.org", "https://maps.example.org/items")
                .unwrap();
        let page = |id: u32, href: &str| {
            serde_json::to_vec(&json!({
                "type":"FeatureCollection", "features":[{"id":id}],
                "links":[{"rel":"next","type":"application/geo+json","href":href}]
            }))
            .unwrap()
        };
        let Step::Fetch(start) = ogc::begin(&first, 100) else {
            panic!("the first page must be fetched");
        };
        let Step::Fetch(pages) = start.land(Ok(page(1, "https://other.example.org/items?page=2"))) else {
            panic!("the foreign page must be fetched");
        };
        let Step::Fetch(pages) = pages.land(Ok(page(2, "https://maps.example.org/items?page=3"))) else {
            panic!("the returning page must be fetched");
        };
        assert_eq!(pages.next_url(), "https://maps.example.org/items?page=3");
        assert_eq!(
            next_request(&mut app, &json!({"connection":connection}), &pages, Reach::Public).as_deref(),
            Ok("https://maps.example.org/items?page=3")
        );
    }

    /// Two saved connections at one origin: a group imported through one of them refreshes
    /// through it, even after a login to the other made that one the origin's choice (the plan's
    /// address comes bound to the connection its layers record).
    #[test]
    fn a_refresh_reads_through_the_connection_its_layers_record() {
        let _turn = crate::fetching::turn();
        let mut app = RustGisApp::with_sample();
        let url = "https://127.0.0.1:9/collections";
        let new = |app: &mut RustGisApp, name: &str, key: &str| {
            let definition = json!({"kind":"ogcapi", "name":name, "address":url, "auth":"header", "keyName":"X-Key"});
            let id = app.run("connection.new", definition).unwrap()["id"].as_str().unwrap().to_string();
            app.run("connection.login", json!({"id": id, "apiKey": key})).unwrap();
            id
        };
        let a = new(&mut app, "A", "refresh-a-757");
        let items = format!("{url}/roads/items");
        let document = json!({"collections":[{"id":"roads", "title":"roads",
            "links":[{"rel":"items", "type":"application/geo+json", "href": items}]}]});
        let features = json!({"type":"FeatureCollection", "features":[{"type":"Feature",
            "geometry":{"type":"Point", "coordinates":[1, 2]}, "properties":{}}]});
        let mut p = json!({"url": url, "type":"ogcapi", "connection": a, "text": document.to_string()});
        let plan = app.session.run("service.layers", &p).unwrap();
        let offers = plan["layers"].as_array().unwrap().iter();
        p["responses"] = json!(offers.map(|l| json!({"url": l["offer"]["url"], "text": features.to_string()})).collect::<Vec<_>>());
        let imported = app.session.run("service.import", &p).unwrap();
        // Logging in to B makes it the origin's choice; the group's layers still record A.
        new(&mut app, "B", "refresh-b-757");
        start(&mut app, &json!({"group": imported["group"]}), true).unwrap();
        let task = app.ui.service_import.as_ref().unwrap();
        let capabilities = task.document.as_deref().unwrap();
        let header = rustgis_engine::render::rustgis_tiles::logins::apply(capabilities).unwrap().and_then(|login| login.header);
        assert_eq!(header.map(|(_, key)| key).as_deref(), Some("refresh-a-757"));
        // The linked documents and the layers' features (`Import::discover`, `Import::next`) are
        // requested through the task's connection.
        assert_eq!(task.connection.as_ref().and_then(Value::as_str), Some(a.as_str()));
    }
}
