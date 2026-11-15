//! Home › Add › Web Data panels for single services, laid out after GeoLibre's: Fields of the World (a
//! year and a score threshold), NASA Earthdata (GIBS layers by category, with a date), EnviroAtlas
//! (its folders, services and their layers as a tree, with search), FEMA NFHL (the flood hazard
//! layers, filtered) and The National Map (its services by category, with search). Each item is a
//! card: its title, a dim line about it and a row of small buttons.
//!
//! Every panel fits a narrow pane (sizes from the room given, no scroll area of its own). The
//! services' documents are fetched off the frame (the tile cache's downloads, so the web app too)
//! and read with `service.layers`; layers are added with `service.add`, `nationalMap.add` and
//! `fieldsOfTheWorld.add`.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{RichText, vec2};
use serde_json::{Value, json};

use crate::{RustGisApp, Slot};

/// NASA's Global Imagery Browse Services, in Web Mercator.
pub const NASA_GIBS: &str = "https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/1.0.0/WMTSCapabilities.xml";
/// US EPA EnviroAtlas's ArcGIS services directory.
pub const ENVIROATLAS: &str = "https://enviroatlas.epa.gov/arcgis/rest/services";
/// FEMA's National Flood Hazard Layer as an ArcGIS map service (its WMS is often unreachable).
pub const FEMA_NFHL: &str = "https://hazards.fema.gov/arcgis/rest/services/public/NFHL/MapServer";

/// EnviroAtlas folders that hold no data for people (as GeoLibre leaves them out).
const ENVIROATLAS_HIDDEN: [&str; 3] = ["test_services", "monitor", "Utilities"];
/// Most search results listed.
const MAX_RESULTS: usize = 200;
/// Most folder documents a search fetches at once.
const MAX_FETCHES: usize = 3;

/// A service document: on its way, read (its text and what `service.layers` made of it), or failed.
enum Doc {
    Waiting(Slot<Result<Vec<u8>, String>>),
    Ready(Arc<String>, Arc<Value>),
    Failed(String),
}

/// What a panel sees of a document.
enum Look {
    Loading,
    Failed(String),
    Ready(Arc<String>, Arc<Value>),
}

/// The panels' documents, searches and the layers waiting for their service's document.
#[derive(Default)]
pub struct State {
    docs: HashMap<String, Doc>,
    /// Each panel's search words.
    search: HashMap<&'static str, String>,
    /// The date NASA's time-enabled layers are added at (their latest when empty).
    pub date: String,
    /// NASA's time-enabled layers are added at a fixed date (`date`, or their latest when empty)
    /// rather than following the time slider, as they do by default.
    pub fixed: bool,
    /// Layers to add once their service's document is read: (service, its type, `service.add`'s
    /// layer, the name).
    adding: Vec<(String, &'static str, String, String, String)>,
    /// Fields of the World's year and lowest score.
    year: Option<i64>,
    min_score: f64,
    /// GIBS's layers' search text (title and name, lower case), read once per document.
    gibs_hay: Option<(usize, Arc<Vec<String>>)>,
    /// The National Map's catalog (`nationalMap.services`), read once.
    national_map: Option<Arc<Value>>,
    /// What was last done, or went wrong.
    note: Option<(bool, String)>,
}

/// The document of the service at `url` (of `kind`: `wmts`, `arcgis`), asked for the first time
/// it's wanted and read with `service.layers` when it lands.
fn listing(app: &mut RustGisApp, url: &str, kind: &'static str) -> Look {
    let state = &mut app.ui.web_services;
    let doc = state.docs.entry(url.to_string()).or_insert_with(|| {
        let caps = rustgis_engine::data::ogc::capabilities_url(url, Some(kind));
        Doc::Waiting(crate::fetch_into(&caps))
    });
    if let Doc::Waiting(slot) = doc {
        let landed = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        match landed {
            None => return Look::Loading,
            Some(Err(e)) => *doc = Doc::Failed(e),
            Some(Ok(bytes)) => {
                *doc = match crate::service_text(bytes) {
                    Ok(text) => match app.session.run("service.layers", &json!({"url": url, "type": kind, "text": text})) {
                        Ok(v) => Doc::Ready(Arc::new(text), Arc::new(v)),
                        Err(e) => Doc::Failed(e),
                    },
                    Err(_) => Doc::Failed(tl!("The service's answer isn't UTF-8 text").into()),
                };
            }
        }
    }
    match app.ui.web_services.docs.get(url) {
        Some(Doc::Ready(text, v)) => Look::Ready(text.clone(), v.clone()),
        Some(Doc::Failed(e)) => Look::Failed(e.clone()),
        _ => Look::Loading,
    }
}

/// Forgets the services' documents that failed, so they're asked for again: a panel reopened
/// retries what a passing network error (or a busy server) refused.
pub fn forget_failed(app: &mut RustGisApp) {
    forget_failed_under(app, "");
}

/// Forgets the failed documents whose addresses start with `prefix` (one service's).
fn forget_failed_under(app: &mut RustGisApp, prefix: &str) {
    app.ui.web_services.docs.retain(|url, d| !(matches!(d, Doc::Failed(_)) && url.starts_with(prefix)));
}

/// Adds a service's layer now, or once its document is read.
fn add_layer(app: &mut RustGisApp, url: &str, kind: &'static str, layer: &str, name: &str) {
    // The date as it was when Add was clicked, or the time slider's (`service.add`'s "slider").
    let date = fixed_or_slider(app.ui.web_services.fixed, &app.ui.web_services.date);
    app.ui.web_services.adding.push((url.to_string(), kind, layer.to_string(), name.to_string(), date));
    poll(app);
}

/// What `service.add` takes as a time-enabled layer's `time` from a dialog: none, to follow the
/// time slider (its default, which adds a layer whose times it can't step to at the service's
/// default), or the date typed, or the service's default when none is.
pub(crate) fn fixed_or_slider(fixed: bool, date: &str) -> String {
    match (fixed, date.trim()) {
        (false, _) => String::new(),
        (true, "") => "default".into(),
        (true, d) => d.to_string(),
    }
}

/// Adds the layers whose documents have arrived. Run by every panel each frame.
fn poll(app: &mut RustGisApp) {
    let waiting = std::mem::take(&mut app.ui.web_services.adding);
    for (url, kind, layer, name, date) in waiting {
        match listing(app, &url, kind) {
            Look::Loading => app.ui.web_services.adding.push((url, kind, layer, name, date)),
            Look::Failed(e) => app.ui.web_services.note = Some((false, e)),
            Look::Ready(doc, v) => {
                // The date goes only to a layer that changes with time, and names it (one following
                // the time slider has no date of its own).
                let timed = layers_of(&v).iter().any(|l| text(l, "name") == layer && l.get("time").is_some());
                let dated = kind == "wmts" && timed && !date.is_empty();
                let name = if dated && !matches!(date.as_str(), "slider" | "default") { format!("{name} {date}") } else { name };
                let mut p = json!({"url": url, "type": kind, "layer": layer, "name": name, "text": doc.as_str()});
                if dated {
                    p["time"] = json!(date);
                }
                app.ui.web_services.note = None;
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let _ = crate::readjob::run_then(app, "service.add", p, Some(service_added));
                }
                #[cfg(target_arch = "wasm32")]
                {
                    let r = app.run("service.add", p);
                    service_added(app, crate::Landed::Ran(r));
                }
            }
        }
    }
}

/// Reports the command's actual completion, including a queued add that never ran.
fn service_added(app: &mut RustGisApp, result: crate::Landed) {
    let result = match result {
        crate::Landed::Ran(result) => result,
        crate::Landed::NotRun(why) => Err(why),
    };
    done(app, result.map(|v| text(&v, "name").to_string()));
}

/// Notes what an Add did.
fn done(app: &mut RustGisApp, r: Result<String, String>) {
    app.ui.web_services.note = Some(match r {
        Ok(name) => (true, crate::i18n::fmt("Added {name}", &[("name", &name)])),
        Err(e) => (false, e),
    });
    app.map.invalidate();
}

/// Keeps the frames coming while documents or layers are on their way.
fn repaint_while_waiting(app: &RustGisApp, ui: &egui::Ui) {
    let s = &app.ui.web_services;
    if !s.adding.is_empty() || s.docs.values().any(|d| matches!(d, Doc::Waiting(_))) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Starts a panel's search with `words` (`ui.dialog`'s `"search"`): `dialog` names the panel.
pub fn set_search(app: &mut RustGisApp, dialog: &str, words: &str) {
    let key = match dialog {
        "nasaEarthdata" => "gibs",
        "enviroAtlas" => "enviroatlas",
        "femaNfhl" => "fema",
        "nationalMap" => "nationalmap",
        _ => return,
    };
    app.ui.web_services.search.insert(key, words.chars().take(200).collect());
}

/// A panel's search box (`what` names the panel); its words, lower case.
fn search_box(app: &mut RustGisApp, ui: &mut egui::Ui, what: &'static str, hint: &str) -> Vec<String> {
    let text = app.ui.web_services.search.entry(what).or_default();
    ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(ui.available_width()));
    // `_` reads as a space, as in service names.
    text.replace('_', " ").split_whitespace().map(str::to_lowercase).collect()
}

/// Whether every word is in `hay`.
fn matches(words: &[String], hay: &str) -> bool {
    let hay = hay.replace('_', " ").to_lowercase();
    words.iter().all(|w| hay.contains(w.as_str()))
}

/// An item: its title, a dim line about it and a row of small buttons.
fn card<R>(ui: &mut egui::Ui, title: &str, meta: &str, buttons: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(title).strong()).wrap());
            if !meta.is_empty() {
                ui.add(egui::Label::new(RichText::new(meta).small().weak()).wrap());
            }
            ui.horizontal_wrapped(buttons).inner
        })
        .inner
}

/// The panel's last result and what's loading.
fn status(app: &mut RustGisApp, ui: &mut egui::Ui, look: Option<&Look>) {
    let t = crate::theme::Tokens::get(ui.ctx());
    match look {
        Some(Look::Loading) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tl!("Loading…"));
            });
        }
        Some(Look::Failed(e)) => {
            ui.colored_label(t.red, e);
            // A busy server's refusal is often passing.
            if ui.button(tl!("Retry")).clicked() {
                forget_failed(app);
            }
        }
        _ => {}
    }
    if !app.ui.web_services.adding.is_empty() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(tl!("Adding…"));
        });
    }
    match &app.ui.web_services.note {
        Some((true, n)) => {
            ui.label(RichText::new(n).small().color(t.green));
        }
        Some((false, n)) => {
            ui.colored_label(t.red, n);
        }
        None => {}
    }
}

fn layers_of(v: &Value) -> &[Value] {
    v.get("layers").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

// ---------------------------------------------------------------------------------------------
// NASA Earthdata

/// NASA Earthdata: GIBS's layers grouped by mission or instrument (the name's first part), with a
/// search and the date time-enabled layers are added at.
pub fn nasa_earthdata(app: &mut RustGisApp, ui: &mut egui::Ui) {
    poll(app);
    ui.label(RichText::new(tl!("NASA's Global Imagery Browse Services (GIBS): over a thousand layers of satellite imagery and science data, most of them daily.")).weak());
    let words = search_box(app, ui, "gibs", tl!("Search layers (e.g. temperature)"));
    ui.horizontal(|ui| {
        let mut follow = !app.ui.web_services.fixed;
        if ui
            .checkbox(&mut follow, tl!("Follow the time slider"))
            .on_hover_text(tl!("Step through the days with the time slider (View › Time), which starts with the layer's days"))
            .changed()
        {
            app.ui.web_services.fixed = !follow;
        }
        let input_label = ui.label(tl!("Date"));
        ui.add_enabled(
            app.ui.web_services.fixed,
            egui::TextEdit::singleline(&mut app.ui.web_services.date).hint_text(tl!("latest")).desired_width(110.0),
        )
        .labelled_by(input_label.id);
        ui.label(RichText::new(tl!("YYYY-MM-DD, for time layers")).small().weak());
    });
    let look = listing(app, NASA_GIBS, "wmts");
    let mut add = None;
    if let Look::Ready(_, v) = &look {
        // Each layer's search text, made once for the document (not every frame).
        let key = Arc::as_ptr(v) as usize;
        let hay = match &app.ui.web_services.gibs_hay {
            Some((k, h)) if *k == key => h.clone(),
            _ => {
                let h: Arc<Vec<String>> = Arc::new(
                    layers_of(v)
                        .iter()
                        .map(|l| format!("{} {}", text(l, "title"), text(l, "name")).replace('_', " ").to_lowercase())
                        .collect(),
                );
                app.ui.web_services.gibs_hay = Some((key, h.clone()));
                h
            }
        };
        let layers: Vec<&Value> =
            layers_of(v).iter().zip(hay.iter()).filter(|(_, h)| words.iter().all(|w| h.contains(w.as_str()))).map(|(l, _)| l).collect();
        // Categories by the name's first part; ones with fewer than three layers go in Other.
        let mut groups: Vec<(String, Vec<&Value>)> = Vec::new();
        for l in &layers {
            let prefix = text(l, "name").split('_').next().unwrap_or("").to_string();
            match groups.iter_mut().find(|(g, _)| *g == prefix) {
                Some((_, ls)) => ls.push(l),
                None => groups.push((prefix, vec![l])),
            }
        }
        let other = tl!("Other").to_string();
        let mut rest = Vec::new();
        groups.retain_mut(|(_, ls)| {
            if ls.len() < 3 {
                rest.append(ls);
                false
            } else {
                true
            }
        });
        groups.sort_by_key(|g| g.0.to_lowercase());
        if !rest.is_empty() {
            groups.push((other, rest));
        }
        ui.label(
            RichText::new(crate::i18n::fmt(
                "{n} layers in {k} categories",
                &[("n", &layers.len().to_string()), ("k", &groups.len().to_string())],
            ))
            .small()
            .weak(),
        );
        if layers.is_empty() {
            ui.label(tl!("No layers match your search."));
        }
        let searching = !words.is_empty();
        for (group, ls) in &groups {
            egui::CollapsingHeader::new(RichText::new(format!("{group}  ({})", ls.len())).strong())
                .id_salt(("gibs", group.as_str()))
                .open(searching.then_some(true))
                .show(ui, |ui| {
                    if ls.len() > MAX_RESULTS {
                        ui.label(
                            RichText::new(crate::i18n::fmt(
                                "The first {n} are shown: search to narrow them.",
                                &[("n", &MAX_RESULTS.to_string())],
                            ))
                            .small()
                            .weak(),
                        );
                    }
                    for l in ls.iter().take(MAX_RESULTS) {
                        let name = text(l, "name");
                        let mut meta = name.to_string();
                        if let Some(t) = l.get("time") {
                            let range =
                                t.get("values").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str).unwrap_or("");
                            let range = range.rsplit_once('/').map(|(r, _)| r.replace('/', " – ")).unwrap_or_else(|| range.to_string());
                            meta.push_str(&format!(" · {} {range}", tl!("time")));
                        }
                        if card(ui, text(l, "title"), &meta, |ui| ui.small_button(tl!("Add")).clicked()) {
                            add = Some((name.to_string(), text(l, "title").to_string()));
                        }
                    }
                });
        }
    }
    if let Some((layer, title)) = add {
        add_layer(app, NASA_GIBS, "wmts", &layer, &title);
    }
    status(app, ui, Some(&look));
    repaint_while_waiting(app, ui);
}

// ---------------------------------------------------------------------------------------------
// EnviroAtlas

/// A service's address and type (`MapServer`, `ImageServer`, …) from a directory's offer.
fn service_of(l: &Value) -> Option<(String, String)> {
    let url = l.get("offer")?.get("url")?.as_str()?.to_string();
    let kind = url.rsplit('/').next()?.to_string();
    Some((url, kind))
}

/// EnviroAtlas: its folders, their services and the services' layers as a tree, each added on
/// its own; a search looks through every folder's services.
pub fn enviroatlas(app: &mut RustGisApp, ui: &mut egui::Ui) {
    poll(app);
    ui.label(
        RichText::new(tl!(
            "US EPA EnviroAtlas: maps of ecosystem services, land cover, people and the built environment across the United States."
        ))
        .weak(),
    );
    let words = search_box(app, ui, "enviroatlas", tl!("Search services and layers…"));
    let look = listing(app, ENVIROATLAS, "arcgis");
    let mut add: Option<(String, &'static str, String, String)> = None;
    if let Look::Ready(_, root) = &look {
        let folders: Vec<String> = layers_of(root)
            .iter()
            .filter_map(|l| service_of(l).map(|(url, _)| (url, text(l, "title"))))
            .filter(|(_, t)| t.ends_with('/') && !ENVIROATLAS_HIDDEN.contains(&t.trim_end_matches('/')))
            .map(|(url, _)| url)
            .collect();
        if words.is_empty() {
            for folder in &folders {
                let label = folder.rsplit('/').next().unwrap_or(folder).to_string();
                egui::CollapsingHeader::new(RichText::new(&label).strong()).id_salt(("ea", folder.as_str())).show(ui, |ui| {
                    match listing(app, folder, "arcgis") {
                        Look::Ready(_, v) => {
                            for s in layers_of(&v) {
                                if let Some(a) = service_tree(app, ui, s) {
                                    add = Some(a);
                                }
                            }
                        }
                        Look::Loading => {
                            ui.spinner();
                        }
                        Look::Failed(e) => {
                            ui.label(RichText::new(e).small().weak());
                            if ui.small_button(tl!("Retry")).clicked() {
                                app.ui.web_services.docs.remove(folder.as_str());
                            }
                        }
                    }
                });
            }
        } else {
            // A search reads every folder and the folders in them (one level), a few at a time,
            // then lists the services whose names match.
            let mut found = 0;
            let mut loading = false;
            let mut failed: Vec<String> = Vec::new();
            let mut to_search: Vec<(String, bool)> = folders.iter().map(|f| (f.clone(), true)).collect();
            let mut i = 0;
            while let Some((folder, top)) = to_search.get(i).cloned() {
                i += 1;
                let known = app.ui.web_services.docs.contains_key(&folder);
                let waiting = app.ui.web_services.docs.values().filter(|d| matches!(d, Doc::Waiting(_))).count();
                if !known && waiting >= MAX_FETCHES {
                    loading = true;
                    continue;
                }
                match listing(app, &folder, "arcgis") {
                    Look::Ready(_, v) => {
                        for s in layers_of(&v) {
                            // A folder in the folder: searched too, once.
                            if top && text(s, "name").ends_with('/') {
                                if let Some((url, _)) = service_of(s) {
                                    to_search.push((url, false));
                                }
                                continue;
                            }
                            if found < MAX_RESULTS && matches(&words, text(s, "name")) {
                                found += 1;
                                if let Some(a) = service_tree(app, ui, s) {
                                    add = Some(a);
                                }
                            }
                        }
                    }
                    Look::Loading => loading = true,
                    Look::Failed(_) => failed.push(folder.rsplit('/').next().unwrap_or(&folder).to_string()),
                }
            }
            if !failed.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(crate::i18n::fmt("Not searched, as they didn't load: {folders}", &[("folders", &failed.join(", "))]))
                            .small()
                            .weak(),
                    );
                    if ui.small_button(tl!("Retry")).clicked() {
                        forget_failed_under(app, ENVIROATLAS);
                    }
                });
            }
            if loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tl!("Searching the folders…"));
                });
            } else if found == 0 {
                ui.label(tl!("No services match your search."));
            }
        }
    }
    if let Some((url, kind, layer, name)) = add {
        add_layer(app, &url, kind, &layer, &name);
    }
    status(app, ui, Some(&look));
    repaint_while_waiting(app, ui);
}

/// A directory's service as a branch: Add adds its whole map (or image); opened, its layers each
/// with an Add of their own. What to add, when a button was clicked.
fn service_tree(app: &mut RustGisApp, ui: &mut egui::Ui, s: &Value) -> Option<(String, &'static str, String, String)> {
    let (url, kind) = service_of(s)?;
    // A folder inside a folder: a branch of its own, its services under it.
    if text(s, "name").ends_with('/') {
        let label = text(s, "title").trim_end_matches('/').to_string();
        let mut add = None;
        egui::CollapsingHeader::new(RichText::new(&label).strong()).id_salt(("ea", url.as_str())).show(ui, |ui| {
            match listing(app, &url, "arcgis") {
                Look::Ready(_, v) => {
                    for s in layers_of(&v) {
                        if let Some(a) = service_tree(app, ui, s) {
                            add = Some(a);
                        }
                    }
                }
                Look::Loading => {
                    ui.spinner();
                }
                Look::Failed(e) => {
                    ui.label(RichText::new(e).small().weak());
                }
            }
        });
        return add;
    }
    let name = text(s, "name").trim_end_matches(&format!("/{kind}")).to_string();
    let short = name.rsplit('/').next().unwrap_or(&name).replace('_', " ");
    // Feature and vector tile services: named, for Add From Service, but not drawn as maps here.
    if !matches!(kind.as_str(), "MapServer" | "ImageServer") {
        ui.horizontal(|ui| {
            ui.label(&short).on_hover_text(&url);
            ui.label(RichText::new(&kind).small().weak());
        });
        return None;
    }
    let mut add = None;
    let whole = if kind == "ImageServer" { "image" } else { "map" };
    let id = ui.make_persistent_id(("ea-svc", url.as_str()));
    egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
        .show_header(ui, |ui| {
            ui.label(&short).on_hover_text(&url);
            ui.label(RichText::new(&kind).small().weak());
            if ui.small_button(tl!("Add")).clicked() {
                add = Some((url.clone(), "arcgis", whole.to_string(), short.clone()));
            }
        })
        .body(|ui| match listing(app, &url, "arcgis") {
            Look::Ready(_, v) => {
                for l in layers_of(&v).iter().filter(|l| text(l, "name").starts_with("map:")) {
                    let title = text(l, "title").trim_end_matches(" (map)");
                    ui.horizontal(|ui| {
                        if ui.small_button(tl!("Add")).clicked() {
                            add = Some((url.clone(), "arcgis", text(l, "name").to_string(), format!("{short}: {title}")));
                        }
                        ui.add(egui::Label::new(title).wrap());
                    });
                }
            }
            Look::Loading => {
                ui.spinner();
            }
            Look::Failed(e) => {
                ui.label(RichText::new(e).small().weak());
                if ui.small_button(tl!("Retry")).clicked() {
                    app.ui.web_services.docs.remove(url.as_str());
                }
            }
        });
    add
}

// ---------------------------------------------------------------------------------------------
// FEMA NFHL

/// FEMA's National Flood Hazard Layer: its layers, filtered, each added drawn alone (or its
/// features); the whole map first.
pub fn fema_nfhl(app: &mut RustGisApp, ui: &mut egui::Ui) {
    poll(app);
    ui.label(RichText::new(tl!("FEMA's National Flood Hazard Layer: effective flood zones, floodways, base flood elevations, levees and map panels. Its flood layers are drawn only from zoom 15 (closer than about 1:36,000), its availability map only farther out.")).weak());
    let words = search_box(app, ui, "fema", tl!("Search layers…"));
    let look = listing(app, FEMA_NFHL, "arcgis");
    let mut add = None;
    let mut features = None;
    if let Look::Ready(_, v) = &look {
        let all = layers_of(v);
        if words.is_empty()
            && card(ui, tl!("Whole map"), tl!("Every layer, as FEMA draws them"), |ui| ui.small_button(tl!("Add")).clicked())
        {
            add = Some(("map".to_string(), "FEMA NFHL".to_string()));
        }
        let maps: Vec<&Value> =
            all.iter().filter(|l| text(l, "name").starts_with("map:")).filter(|l| matches(&words, text(l, "title"))).collect();
        for l in maps {
            let title = text(l, "title").trim_end_matches(" (map)");
            let id = text(l, "name").trim_start_matches("map:");
            // The same layer's features, when the service offers them.
            let feature = all.iter().find(|f| text(f, "name") == id && f.pointer("/offer/type") == Some(&json!("features")));
            let meta = crate::i18n::fmt("layer {id}", &[("id", id)]);
            card(ui, title, &meta, |ui| {
                if ui.small_button(tl!("Add")).clicked() {
                    add = Some((text(l, "name").to_string(), format!("FEMA NFHL {title}")));
                }
                if let Some(f) = feature
                    && ui
                        .small_button(tl!("Features"))
                        .on_hover_text(tl!("Download its features in the view (up to the server's limit)"))
                        .clicked()
                {
                    features = f
                        .pointer("/offer/url")
                        .and_then(Value::as_str)
                        .map(|u| (u.to_string(), format!("FEMA NFHL {title}"), f.get("bbox").cloned().unwrap_or_default()));
                }
            });
        }
    }
    if let Some((layer, name)) = add {
        add_layer(app, FEMA_NFHL, "arcgis", &layer, &name);
    }
    if let Some((url, name, bounds)) = features {
        // The layer's bounds go along, for its features' axes (#1053).
        let r = app.run("ui.addUrl", json!({"url": url, "name": name, "service": true, "bounds": bounds}));
        // Not added yet: on its way.
        app.ui.web_services.note = Some(match r {
            Ok(_) => (true, tl!("Downloading…").to_string()),
            Err(e) => (false, e),
        });
    }
    status(app, ui, Some(&look));
    repaint_while_waiting(app, ui);
}

// ---------------------------------------------------------------------------------------------
// The National Map

/// The National Map: its services by category, with a search, each added as tiles.
pub fn national_map(app: &mut RustGisApp, ui: &mut egui::Ui) {
    ui.label(RichText::new(tl!("USGS The National Map's map services (topographic maps, imagery, elevation, hydrography, boundaries and more) and a few partners'. Each is added as a tile layer.")).weak());
    let words = search_box(app, ui, "nationalmap", tl!("Search services…"));
    // The catalog is fixed: read once.
    if app.ui.web_services.national_map.is_none() {
        match app.session.run("nationalMap.services", &Value::Null) {
            Ok(v) => app.ui.web_services.national_map = Some(Arc::new(v)),
            Err(e) => {
                // Not asked again every frame: an empty catalog, and the error shown once.
                app.ui.web_services.national_map = Some(Arc::new(Value::Null));
                app.ui.web_services.note = Some((false, e));
            }
        }
    }
    let catalog = app.ui.web_services.national_map.clone().unwrap_or_default();
    let services = layers_of_key(&catalog, "services");
    let mut add = None;
    let shown_total =
        services.iter().filter(|s| matches(&words, &format!("{} {} {}", text(s, "title"), text(s, "id"), text(s, "category")))).count();
    ui.label(
        RichText::new(crate::i18n::fmt("Showing {n} of {m}", &[("n", &shown_total.to_string()), ("m", &services.len().to_string())]))
            .small()
            .weak(),
    );
    for category in layers_of_key(&catalog, "categories").iter().filter_map(Value::as_str) {
        let shown: Vec<&Value> = services
            .iter()
            .filter(|s| text(s, "category") == category)
            .filter(|s| matches(&words, &format!("{} {} {category}", text(s, "title"), text(s, "id"))))
            .collect();
        if shown.is_empty() {
            continue;
        }
        egui::CollapsingHeader::new(RichText::new(format!("{}  ({})", category_label(category), shown.len())).strong())
            .id_salt(("nm", category))
            .default_open(true)
            .show(ui, |ui| {
                for s in shown {
                    let url = text(s, "url");
                    let host = url.split('/').nth(2).unwrap_or("");
                    let kind = url.rsplit('/').next().unwrap_or("");
                    let how = if text(s, "tiles").contains("/tile/") { tl!("cached tiles") } else { tl!("drawn by the server") };
                    if card(ui, text(s, "title"), &format!("{kind} · {host} · {how}"), |ui| ui.small_button(tl!("Add")).clicked()) {
                        add = Some(text(s, "id").to_string());
                    }
                }
            });
    }
    if let Some(id) = add {
        let r = app.run("nationalMap.add", json!({"service": id}));
        done(app, r.map(|v| text(&v, "name").to_string()));
    }
    status(app, ui, None);
}

/// A National Map category, translated (each name a literal, so the catalogs pick it up).
fn category_label(category: &str) -> &str {
    match category {
        "Basemaps" => tl!("Basemaps"),
        "Hydrography" => tl!("Hydrography"),
        "Elevation" => tl!("Elevation"),
        "Imagery" => tl!("Imagery"),
        "Cartography" => tl!("Cartography"),
        "Hazards" => tl!("Hazards"),
        "Other Data" => tl!("Other Data"),
        "Indexes" => tl!("Indexes"),
        other => other,
    }
}

fn layers_of_key<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

// ---------------------------------------------------------------------------------------------
// Fields of the World

/// Fields of the World: a year and a score threshold, then the field boundaries or the field
/// density added, with the score's colours.
pub fn fields_of_the_world(app: &mut RustGisApp, ui: &mut egui::Ui) {
    ui.label(RichText::new(tl!("Agricultural field boundaries the Fields of the World model found in Sentinel-2 imagery, worldwide, a year at a time (2nd Edition, CC BY 4.0). These are model predictions, not surveyed parcels.")).weak());
    let years = rustgis_engine::cmd::webdata::FIELDS_YEARS;
    let year = *app.ui.web_services.year.get_or_insert(*years.end());
    let mut picked = year;
    ui.horizontal(|ui| {
        let input_label = ui.label(tl!("Year"));
        egui::ComboBox::from_id_salt("ftw_year")
            .selected_text(picked.to_string())
            .show_ui(ui, |ui| {
                for y in years.clone().rev() {
                    ui.selectable_value(&mut picked, y, y.to_string());
                }
            })
            .response
            .labelled_by(input_label.id);
    });
    app.ui.web_services.year = Some(picked);
    let input_label = ui.label(tl!("Score threshold"));
    ui.add(egui::Slider::new(&mut app.ui.web_services.min_score, 0.0..=100.0).step_by(1.0)).labelled_by(input_label.id);
    ui.label(RichText::new(tl!("Hides fields scoring below this. The score is the model's mean field probability × 100, a ranking rather than a calibrated probability.")).small().weak());
    // The score's colours, as the fields are drawn.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 2.0);
        for (c, label) in
            [((215, 48, 39), "0"), ((253, 174, 97), "45"), ((255, 255, 191), "55"), ((166, 217, 106), "65"), ((26, 152, 80), "80+")]
        {
            let (r, _) = ui.allocate_exact_size(vec2(14.0, 10.0), egui::Sense::hover());
            ui.painter().rect_filled(r, 2.0, egui::Color32::from_rgb(c.0, c.1, c.2));
            ui.label(RichText::new(label).small());
        }
    });
    let mut draw = None;
    card(ui, tl!("Field boundaries"), tl!("Fields colored by score, drawn from zoom 9"), |ui| {
        if ui.small_button(tl!("Add field boundaries")).clicked() {
            draw = Some("fields");
        }
    });
    card(ui, tl!("Field density"), tl!("The share of each cell covered by fields, for the zoomed-out view"), |ui| {
        if ui.small_button(tl!("Add field density")).clicked() {
            draw = Some("density");
        }
    });
    if let Some(draw) = draw {
        let p = json!({"year": picked, "draw": draw, "minScore": app.ui.web_services.min_score});
        let r = app.run("fieldsOfTheWorld.add", p).map(|v| {
            let mut n = text(&v, "name").to_string();
            if v.get("note").is_some() {
                n.push_str(". ");
                n.push_str(tl!("Fields are drawn from zoom 9: zoom in to see them."));
            }
            n
        });
        done(app, r);
    }
    status(app, ui, None);
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn ogc_dataset_web_data_waits_for_add_completion() {
        let url = "https://maps.example.org/MapServer";
        let doc = json!({"mapName":"Fixture","layers":[]}).to_string();
        for outcome in ["success", "failure", "cancel"] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let listing = app.session.run("service.layers", &json!({"url":url,"type":"arcgis","text":doc})).unwrap();
            assert_eq!(listing["layers"][0]["name"], "map");
            app.ui.web_services.docs.insert(url.into(), Doc::Ready(Arc::new(doc.clone()), Arc::new(listing)));
            app.reads_off_frame = true;
            add_layer(&mut app, url, "arcgis", if outcome == "failure" { "absent" } else { "map" }, "Fixture");
            assert!(crate::readjob::showing(&app));
            assert!(app.session.map().tiles.is_empty());
            assert!(
                app.ui.web_services.note.as_ref().is_none_or(|(_, note)| !note.starts_with("Added")),
                "an enqueued add is not a success"
            );
            if outcome == "cancel" {
                app.reads.cancel_for_tests();
            }
            let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while crate::readjob::land(&mut app) {
                assert!(std::time::Instant::now() < until);
                std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: legacy polling for dataset add completion
            }
            let (ok, note) = app.ui.web_services.note.as_ref().unwrap();
            assert_eq!(*ok, outcome == "success", "{note}");
            assert_eq!(app.session.map().tiles.len(), usize::from(*ok));
            if *ok {
                assert_eq!(note, "Added Fixture");
            } else if outcome == "cancel" {
                assert!(note.contains("cancelled"), "{note}");
            } else {
                assert!(note.contains("absent"), "{note}");
            }
        }
    }
}
