//! Add From Service › Catalogue (CSW): a catalogue searched by words and, optionally, the map's
//! area; its records listed with their titles, abstracts and the links RustGIS can follow — a
//! service (connected to in the same dialog) or a file (downloaded as From URL… does). Requests go
//! off the frame; `catalog.search` reads the answers.

use egui::vec2;
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
            st.waiting = Some((p, crate::fetch_into(&request)));
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
                        follow = Some((kind.to_string(), url.to_string(), title.to_string()));
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
    if let Some((kind, url, title)) = follow {
        app.dialogs.error = None;
        let r = if kind == "download" {
            app.run("ui.addUrl", json!({"url": url, "name": title})).map(|_| ())
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
            app.run("ui.addService", json!({"url": url, "type": app.dialogs.mode})).map(|_| ())
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
