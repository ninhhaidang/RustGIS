//! Background group import/refresh for both desktop and browser. Discovery and updates live in
//! engine commands; this state machine supplies their downloaded documents and preserves the
//! saved connection or anonymous provenance through every deferred add.

use crate::{RustGisApp, Slot, fetch_into, fetch_into_limited, i18n};
use rustgis_engine::data::ogc::{self, Pages, Step};
use serde_json::{Value, json};
use std::collections::VecDeque;

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

/// Uses the same translated tileset progress text when discovery starts and when it retires.
pub(crate) fn reading_tileset(name: &str) -> String {
    i18n::fmt("Reading the tileset of {name}", &[("name", name)])
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
#[allow(clippy::too_many_arguments)]
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
        let slot = crate::fetch_into(&describe);
        app.ui.service_steps.push(crate::ServiceStep::Coverage { params: p, slot, dialog, origin, opened: app.session.opened });
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
            let slot = crate::fetch_into(&list);
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
        None => crate::connectiontree::run_named(app, "service.add", p, origin.as_deref()).map(|_| added(&title)),
    }
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
        task.slot = fetch_into(&caps);
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
        // Re-selecting from the original saved connection would undo a foreign hop.
        rustgis_engine::render::rustgis_tiles::address::check(url, Some(base)).map_err(|e| crate::connectionui::translated(&e))?;
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
            self.slot = fetch_into(&next);
            self.document = Some(next);
        } else {
            self.remaining = listing
                .get("layers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|l| l.get("offer").and_then(|o| o.get("url")).and_then(Value::as_str).map(str::to_string))
                .collect();
            self.next()?;
        }
        Ok(())
    }

    /// Fetches one exact planned collection within the remaining shared byte budget.
    fn next(&mut self) -> Result<(), String> {
        self.current = self.remaining.pop_front();
        if let Some(url) = &self.current {
            let url = self.request(url)?;
            self.slot = fetch_into_limited(&url, rustgis_engine::cmd::service::MAX_GROUP_BYTES.saturating_sub(self.bytes));
        }
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
    slot: Slot<Result<Vec<u8>, String>>,
    /// When the answer was asked for: a layer's description that hangs is given up on.
    asked: web_time::Instant,
}

/// Starts reading a service's features (`landed`, its download started but nothing fetched yet)
/// in the background ([`poll_pages`]): an ArcGIS layer whose description lists protocol buffers in
/// them, in pages.
pub(crate) fn read_pages(app: &mut RustGisApp, landed: Landed) {
    let step = ogc::begin_with_pbf(&landed.url, ogc::max_paged_features());
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
            let url = rustgis_engine::sources::address_in_message(&read.landed.url);
            let why = i18n::fmt("{url}: downloaded after its project was closed, so it wasn't added", &[("url", &url)]);
            app.session.request_failed(&read.landed.params, &why);
            app.landed_in_dialog(&read.landed.params, Err(&why));
            app.session.notify(why, false);
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

/// The address a paged read fetches next. Its login comes only from the page that led to it
/// (the referrer), so a page reached through another origin stays anonymous even when it points
/// back at the first page's origin.
fn next_request(app: &mut RustGisApp, params: &Value, pages: &Pages) -> Result<String, String> {
    rustgis_engine::render::rustgis_tiles::address::check(pages.next_url(), Some(pages.referrer()))?;
    if params.get("anonymous").and_then(Value::as_bool) == Some(true) {
        return Ok(rustgis_engine::data::login_address::inner(pages.next_url()).to_string());
    }
    request_from(app, params.get("connection").and_then(Value::as_str), pages.referrer(), pages.next_url())
}

/// The next answer fetched, or the collection added (with a note when some features were left
/// unread), or why it couldn't be fetched said.
pub(crate) fn go_on(app: &mut RustGisApp, landed: Landed, left: usize, step: Step) {
    if let Err(e) = app.session.download_continues(landed.id) {
        app.session.request_failed(&landed.params, &e);
        app.landed_in_dialog(&landed.params, Err(&e));
        app.session.notify(e, true);
        return;
    }
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
                let next = next_request(app, &landed.params, &pages);
                let slot = match next {
                    // A layer's description: small, and soon given up on.
                    Ok(url) if pages.describing() => fetch_description(&url, left.min(ogc::DESCRIPTION_MAX)),
                    Ok(url) => fetch_into_limited(&url, left),
                    Err(e) => Slot::new(std::sync::Mutex::new(Some(Err(e)))),
                };
                app.ui.service_pages.push(PageRead { landed, pages, left, slot, asked: web_time::Instant::now() });
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
                        *slot.lock().unwrap() = Some(Ok(describe.to_vec()));
                        params
                    }
                    crate::ServiceStep::Tiles { resolve, .. } => {
                        assert_eq!(resolve.crossed_origin, anonymous);
                        *resolve.slot.lock().unwrap() = Some(Ok(br#"{"links":[{"rel":"item","type":"image/png","href":"https://anonymous-offer.invalid/{tileMatrix}/{tileRow}/{tileCol}"}]}"#.to_vec()));
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
            next_request(&mut app, &json!({"connection":connection}), &pages).as_deref(),
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
