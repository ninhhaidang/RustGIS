//! Add From Service › Catalogue (CSW): a catalogue searched by words and, optionally, the map's
//! area; its records listed with their titles, abstracts and the links RustGIS can follow — a
//! service (connected to in the same dialog) or a file (downloaded as From URL… does). Requests go
//! off the frame; `catalog.search` reads the answers.

use egui::vec2;
use rustgis_engine::render::rustgis_tiles::{address::Reach, cache};
use serde_json::{Value, json};

use crate::{RustGisApp, Slot};

/// The catalogue search: its form, the request under way and the records found so far.
#[derive(Default)]
pub struct State {
    pub url: String,
    pub query: String,
    /// Only records within the map's area.
    pub use_view: bool,
    /// `catalog.search`'s parameters and its answer on the way.
    waiting: Option<(Value, Slot<Result<Vec<u8>, String>>)>,
    records: Vec<Value>,
    matched: u64,
    next: Option<u64>,
    /// The first page's parameters, which More pages through (not the form, edited since).
    asked: Value,
    notes: Vec<String>,
    pub error: Option<String>,
}

/// Links shown for a record.
const SHOWN_LINKS: usize = 6;

/// The link kinds shown, with their labels; `web` pages aren't.
const KINDS: [(&str, &str); 7] = [
    ("wms", "WMS"),
    ("wmts", "WMTS"),
    ("wfs", "WFS"),
    ("wcs", "WCS"),
    ("ogcapi", "OGC API"),
    ("arcgis", "ArcGIS"),
    ("download", "Download"),
];

/// Asks for a page of records: the first (`start` 1) replaces those found, a later one adds to them.
pub fn search(app: &mut RustGisApp, start: u64) {
    let st = &app.ui.csw;
    let mut p = if start <= 1 || !st.asked.is_object() {
        let mut p = json!({"url": st.url.trim(), "query": st.query.trim()});
        // The map's area now, not when More is pressed.
        if st.use_view {
            p["bbox"] =
                rustgis_engine::cmd::catalogs::area(&app.session, &json!({"bbox": "view"})).map_or_else(|_| json!("view"), |a| json!(a));
        }
        p
    } else {
        st.asked.clone()
    };
    if start <= 1 {
        let url = p.get("url").and_then(Value::as_str).unwrap_or("");
        if let Some(id) = crate::serviceui::connection_for(app, app.dialogs.connection.as_deref(), url) {
            p["connection"] = json!(id);
        }
    }
    match rustgis_engine::service_logins::request_params(&mut app.session, &p) {
        Ok(bound) => p = bound.into_owned(),
        Err(e) => {
            app.ui.csw.error = Some(crate::connectionui::translated(&e));
            return;
        }
    }
    let asked = p.clone();
    p["start"] = json!(start);
    match rustgis_engine::cmd::service::records_request(&app.session, &p) {
        Ok(request) => {
            let st = &mut app.ui.csw;
            st.error = None;
            if start <= 1 {
                st.records.clear();
                st.notes.clear();
                st.matched = 0;
                st.next = None;
                st.asked = asked;
            }
            // A catalogue held to the internet is searched only there; an answer read from the
            // internet is recorded, so `catalog.search` holds the links it lists there.
            if p.get("url").and_then(Value::as_str).is_some_and(|url| cache::ceiling_for(url) == Reach::Public) {
                cache::restrict_to_public(&request);
            }
            st.waiting = Some((p, crate::fetch_document_into(&request)));
        }
        Err(e) => app.ui.csw.error = Some(e),
    }
}

/// Reads a page of records once it has arrived.
pub fn poll(app: &mut RustGisApp) {
    let Some((_, slot)) = &app.ui.csw.waiting else { return };
    let Some(r) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
    let Some((mut p, _)) = app.ui.csw.waiting.take() else { return };
    let r = r.and_then(|bytes| {
        p["text"] = json!(String::from_utf8_lossy(&bytes));
        // Not `app.run`: an error belongs in the dialog, not the notices.
        app.session.run("catalog.search", &p)
    });
    let st = &mut app.ui.csw;
    match r {
        Ok(v) => {
            st.matched = v.get("matched").and_then(Value::as_u64).unwrap_or(0);
            st.next = v.get("next").and_then(Value::as_u64);
            st.records.extend(v.get("records").and_then(Value::as_array).into_iter().flatten().cloned());
            st.notes.extend(v.get("notes").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(String::from));
        }
        Err(e) => st.error = Some(e),
    }
}

/// The catalogue form (the Type row is the dialog's) and its records.
pub fn form(app: &mut RustGisApp, ui: &mut egui::Ui, kinds: impl FnOnce(&mut RustGisApp, &mut egui::Ui)) {
    let mut go = false;
    egui::Grid::new("csw").num_columns(2).spacing(vec2(10.0, 8.0)).show(ui, |ui| {
        let input_label = ui.label(tl!("Address"));
        let r = ui
            .add(egui::TextEdit::singleline(&mut app.ui.csw.url).hint_text("https://…/csw").desired_width(420.0))
            .labelled_by(input_label.id);
        go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.end_row();
        kinds(app, ui);
        ui.end_row();
        let input_label = ui.label(tl!("Words"));
        let r = ui
            .add(egui::TextEdit::singleline(&mut app.ui.csw.query).hint_text(tl!("anywhere in the records")).desired_width(420.0))
            .labelled_by(input_label.id);
        go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.end_row();
        ui.label("");
        ui.checkbox(&mut app.ui.csw.use_view, tl!("Only where the map is"));
        ui.end_row();
    });
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        go |= crate::widgets::dialog_button(ui, tl!("Search"), Some(t.accent), vec2(96.0, 28.0)).clicked();
        if app.ui.csw.waiting.is_some() {
            ui.spinner();
        }
    });
    if go {
        if app.ui.csw.url.trim().is_empty() {
            app.ui.csw.error = Some(tl!("Give the catalogue's web address").to_string());
        } else {
            search(app, 1);
        }
    }
    if let Some(e) = &app.ui.csw.error {
        ui.colored_label(t.red, e);
    }
    records(app, ui);
}

/// The records found: each with its abstract (on hover), a Zoom to its bounds and a button per
/// link RustGIS can follow.
fn records(app: &mut RustGisApp, ui: &mut egui::Ui) {
    let list = crate::sourcespane::connections_snapshot(app);
    let st = &app.ui.csw;
    if st.records.is_empty() && st.matched == 0 {
        return;
    }
    ui.separator();
    // The address as typed: one read through a login is matched and kept without its binding.
    let url = rustgis_engine::data::login_address::inner(st.asked["url"].as_str().unwrap_or("")).to_string();
    let keepable = !url.is_empty() && crate::connectionui::picked(&list, app.dialogs.connection.as_deref(), &url).is_none();
    let mut keep = false;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(crate::i18n::fmt(
                "{shown} of {matched} records",
                &[("shown", &st.records.len().to_string()), ("matched", &st.matched.to_string())],
            ))
            .strong(),
        );
        if keepable {
            keep = crate::connectionui::keep_button(ui);
        }
    });
    for n in &st.notes {
        ui.label(egui::RichText::new(n).small().weak());
    }
    let (mut zoom, mut follow, mut more) = (None, None, false);
    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
        for r in &st.records {
            let title = r.get("title").and_then(Value::as_str).unwrap_or("");
            let summary = r.get("abstract").and_then(Value::as_str).unwrap_or("");
            let label = ui.add(egui::Label::new(egui::RichText::new(title).strong()).truncate());
            if !summary.is_empty() {
                label.on_hover_text(summary.chars().take(600).collect::<String>());
            }
            ui.horizontal_wrapped(|ui| {
                if let Some(b) = r.get("bbox").filter(|b| b.is_array())
                    && ui.small_button(tl!("Zoom")).clicked()
                {
                    zoom = Some(b.clone());
                }
                let mut any = false;
                let links = r.get("links").and_then(Value::as_array).into_iter().flatten();
                for l in links.filter(|l| l.get("kind").and_then(Value::as_str).is_some_and(|k| k != "web")).take(SHOWN_LINKS) {
                    let kind = l.get("kind").and_then(Value::as_str).unwrap_or("");
                    let Some((_, label)) = KINDS.iter().find(|(k, _)| *k == kind) else { continue };
                    let url = l.get("url").and_then(Value::as_str).unwrap_or("");
                    any = true;
                    // The link's name, else the file or path it ends in: links of a kind told apart.
                    let path = url.split(['?', '#']).next().unwrap_or(url).trim_end_matches('/');
                    let what = l
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|n| !n.is_empty())
                        .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(""));
                    let what: String = what.chars().take(28).collect();
                    let text = if what.is_empty() { tl!(*label).to_string() } else { format!("{} · {what}", tl!(*label)) };
                    if ui.small_button(text).on_hover_text(url).clicked() {
                        let public = l.get("fromDocument").and_then(Value::as_bool) == Some(true);
                        follow = Some((kind.to_string(), url.to_string(), title.to_string(), public));
                    }
                }
                if !any {
                    ui.label(egui::RichText::new(tl!("no service or file links")).small().weak());
                }
            });
            ui.add_space(4.0);
        }
        if st.next.is_some() && st.waiting.is_none() && ui.button(tl!("More")).clicked() {
            more = true;
        }
    });
    if more {
        let next = app.ui.csw.next.unwrap_or(1);
        search(app, next);
    }
    if let Some(b) = zoom {
        let _ = app.run("map.zoomToExtent", json!({"extent": b}));
    }
    if let Some((kind, url, title, public)) = follow {
        app.dialogs.error = None;
        // A link from a catalogue read from the internet leads only there (`fromDocument`).
        let r = if kind == "download" {
            app.run("ui.addUrl", json!({"url": url, "name": title, "fromDocument": public})).map(|_| ())
        } else {
            // A service: connected to in this dialog, its type picked (the catalogue's results
            // stay for Catalogue).
            app.dialogs.mode = match kind.as_str() {
                "wms" => "wms",
                "wmts" => "wmts",
                "wfs" => "wfs",
                "wcs" => "wcs",
                "ogcapi" => "ogcapi",
                _ => "arcgis",
            };
            app.run("ui.addService", json!({"url": url, "type": app.dialogs.mode, "fromDocument": public})).map(|_| ())
        };
        if let Err(e) = r {
            app.ui.csw.error = Some(e);
        }
    }
    if keep {
        match crate::connectionui::keep(app, json!({"kind": "csw", "address": url})) {
            Ok(id) => app.dialogs.connection = Some(id),
            Err(e) => app.ui.csw.error = Some(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::render::rustgis_tiles::{
        address::{Origin, Reach},
        cache,
    };

    /// A catalogue's answer read from the internet holds the services its records link to there:
    /// clicking a record's service reads its capabilities only from the internet. Read on the local
    /// network, at a public-looking name, its records keep a service on a private address, and
    /// clicking it reads that service as the user's own.
    #[test]
    fn a_catalogues_services_are_read_within_the_reach_of_its_answer() {
        use egui_kittest::kittest::Queryable;
        for (n, observed) in [(1, Reach::Public), (2, Reach::Local)] {
            let _turn = crate::fetching::turn();
            let csw = format!("https://csw-{n}.corp-looks-public.example/csw");
            let service =
                if observed == Reach::Public { format!("https://csw-service-{n}.example/wms") } else { format!("http://10.0.{n}.4/wms") };
            let answer = format!(
                r#"<csw:GetRecordsResponse xmlns:csw="http://www.opengis.net/cat/csw/2.0.2" xmlns:dc="http://purl.org/dc/elements/1.1/"><csw:SearchResults numberOfRecordsMatched="1"><csw:Record><dc:identifier>a</dc:identifier><dc:title>Roads</dc:title><dc:URI protocol="OGC:WMS">{service}</dc:URI></csw:Record></csw:SearchResults></csw:GetRecordsResponse>"#
            );
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                let clean = cache::split_download_limit(&url).0.to_string();
                log.lock().unwrap().push((clean.clone(), ceiling));
                let origin = Origin::with_observed_reach(&clean, observed.min(ceiling)).unwrap();
                done(Ok(cache::Fetched { bytes: answer.clone().into_bytes(), origin }));
            });
            let app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(16).build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
            h.state_mut().run("ui.addService", json!({"url": csw, "type": "csw"})).unwrap();
            h.run_steps(3);
            assert_eq!(h.state().ui.csw.records.len(), 1, "{:?}", h.state().ui.csw.error);
            h.get_by_label("WMS · wms").click();
            h.run_steps(2);
            let caps = rustgis_engine::data::ogc::capabilities_url(&service, Some("wms"));
            let asked = asked.lock().unwrap().clone();
            assert!(asked.contains(&(caps.clone(), observed)), "the service, its catalogue read {observed:?}: {asked:?}");
            assert_eq!(cache::ceiling_for(&caps), observed);
        }
    }

    /// A catalogue a document read from the internet named (`fromDocument`) is searched only there.
    #[test]
    fn a_catalogue_a_document_named_is_searched_only_on_the_internet() {
        for public in [true, false] {
            let _turn = crate::fetching::turn();
            let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let log = asked.clone();
            cache::set_fetcher_with_origin(move |url, ceiling, done, _| {
                log.lock().unwrap().push(ceiling);
                let origin = Origin::with_observed_reach(cache::split_download_limit(&url).0, ceiling).unwrap();
                done(Ok(cache::Fetched {
                    bytes: b"<csw:GetRecordsResponse xmlns:csw=\"http://www.opengis.net/cat/csw/2.0.2\"/>".to_vec(),
                    origin,
                }));
            });
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let csw = format!("https://csw-named-{public}.example/csw");
            app.run("ui.addService", json!({"url": csw, "type": "csw", "fromDocument": public})).unwrap();
            let want = if public { Reach::Public } else { Reach::Local };
            assert_eq!(asked.lock().unwrap().clone(), [want]);
        }
    }
}
