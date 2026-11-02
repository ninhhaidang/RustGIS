//! The Data Sources pane: favourites, recent sources, folders, named connections and a library
//! of public sources. Rows add a source or open its browser through the existing commands.
//! Sources and connections are kept with the app's preferences, never with secrets.

use egui::{RichText, Sense, Ui, vec2};
use rustgis_engine::connections::{Connection, DATABASE_KINDS};
use rustgis_engine::sources::{Source, library};
use serde_json::{Value, json};

use crate::theme::{Tokens, semibold};
use crate::{RustGisApp, icons};

/// The preference the sources are kept under.
pub const PREF: &str = "data_sources";

/// Preference loading and generation-keyed snapshots for the pane.
#[derive(Default)]
pub struct State {
    loaded: bool,
    /// The sources as last shown, and their generation.
    shown: Option<(u64, std::sync::Arc<rustgis_engine::sources::Sources>)>,
    connections: Option<(u64, std::sync::Arc<Vec<Connection>>)>,
}

/// The sources to show this frame: the last snapshot unless they changed since.
fn snapshot(app: &mut RustGisApp) -> std::sync::Arc<rustgis_engine::sources::Sources> {
    let generation = app.session.sources.generation;
    match &app.ui.sources.shown {
        Some((g, s)) if *g == generation => s.clone(),
        _ => {
            let s = std::sync::Arc::new(app.session.sources.clone());
            app.ui.sources.shown = Some((generation, s.clone()));
            s
        }
    }
}

pub(crate) fn connections_snapshot(app: &mut RustGisApp) -> std::sync::Arc<Vec<Connection>> {
    let generation = app.session.connections.generation;
    match &app.ui.sources.connections {
        Some((g, list)) if *g == generation => list.clone(),
        _ => {
            let list = std::sync::Arc::new(app.session.connections.connections.clone());
            app.ui.sources.connections = Some((generation, list.clone()));
            list
        }
    }
}

/// Reads the sources kept with the preferences, once.
pub fn load(app: &mut RustGisApp) {
    if app.ui.sources.loaded {
        return;
    }
    app.ui.sources.loaded = true;
    if let Some(text) = app.services.load_pref.as_ref().and_then(|load| load(PREF)) {
        // What this session already has (added, saved or made favourite before the first read)
        // stays; the stored lists are added after it.
        app.session.sources.merge(rustgis_engine::sources::Sources::from_json(&text));
    }
    app.ui.sources_saved = app.session.sources.generation;
}

/// Keeps the sources with the preferences when they changed.
pub fn save(app: &mut RustGisApp) {
    if !app.ui.sources.loaded {
        load(app);
    }
    if app.session.sources.generation == app.ui.sources_saved {
        return;
    }
    app.ui.sources_saved = app.session.sources.generation;
    if let Some(save) = &app.services.save_pref {
        save(PREF, &app.session.sources.to_json());
    }
}

/// Loads connections before commands can run, importing legacy saved endpoints only once.
pub fn load_connections(app: &mut RustGisApp) {
    use rustgis_engine::{connections::Connections, sources::Sources};
    let Some(load) = &app.services.load_pref else { return };
    app.session.connections = if let Some(text) = load(rustgis_engine::connections::PREF) {
        Connections::from_json(&text)
    } else {
        let imported =
            Connections::import(&Sources::from_json(&load(PREF).unwrap_or_default()), &load(crate::s3ui::SAVED).unwrap_or_default());
        // Even an empty import is saved, so removing everything cannot re-import old entries.
        if let Some(save) = &app.services.save_pref {
            save(rustgis_engine::connections::PREF, &imported.to_json());
        }
        imported
    };
    app.ui.connections_saved = app.session.connections.generation;
}

/// Keeps command and assistant changes with the app's preferences, never the project.
pub fn save_connections(app: &mut RustGisApp) {
    if app.session.connections.generation == app.ui.connections_saved {
        return;
    }
    app.ui.connections_saved = app.session.connections.generation;
    if let Some(save) = &app.services.save_pref {
        save(rustgis_engine::connections::PREF, &app.session.connections.to_json());
    }
}

fn icon_of(kind: &str) -> &'static str {
    match kind {
        "file" => "add_data",
        "url" => "web_map",
        "tiles" => "basemap",
        "service" => "globe_view",
        "database" => "table",
        "zarr" => "raster_layer",
        "stac" => "search",
        "sample" => "map",
        "s3" => "package",
        _ => "add_data",
    }
}

fn text<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// Adds a source, or opens the dialog that browses it (a service's layers, a database's tables,
/// a STAC API's collections), connected.
pub fn open(app: &mut RustGisApp, s: &Source) -> Result<(), String> {
    let x = &s.extra;
    match s.kind.as_str() {
        "file" => match text(x, "layer") {
            Some(l) => app.run("layer.addData", json!({"path": s.address, "layer": l})).map(|_| ()),
            // A file holding several layers asks which to add, as Add Data does (#934).
            None => crate::layerchooser::add_file(app, &s.address).map(|_| ()),
        },
        "url" => {
            let mut p = json!({"url": s.address, "name": s.name});
            if let Some(l) = text(x, "layer") {
                p["layer"] = json!(l);
            }
            app.run("ui.addUrl", p).map(|_| ())
        }
        "tiles" => {
            let mut p = json!({"url": s.address, "name": s.name});
            for k in ["kind", "attribution"] {
                if let Some(v) = text(x, k) {
                    p[k] = json!(v);
                }
            }
            if let Some(z) = text(x, "maxZoom").and_then(|z| z.parse::<u32>().ok()) {
                p["maxZoom"] = json!(z);
            }
            app.run("map.addTiles", p).map(|_| ())
        }
        "service" => {
            app.run("ui.dialog", json!({"name": "addService"}))?;
            app.run("ui.addService", json!({"url": s.address, "type": text(x, "type").unwrap_or("auto")})).map(|_| ())
        }
        "database" => {
            app.run("ui.dialog", json!({"name": "addDatabase"}))?;
            app.dialogs.a = s.address.clone();
            if let Some(l) = text(x, "layer") {
                app.dialogs.c = l.to_string();
            }
            Ok(())
        }
        "zarr" => app.run("ui.addZarr", json!({"address": s.address})).map(|_| ()),
        "stac" if text(x, "api").is_some() => {
            app.ui.stac.api = s.address.clone();
            app.ui.stac.preset = None;
            (app.ui.stac.node, app.ui.stac.node_title) = (String::new(), String::new());
            app.ui.stac.trail.clear();
            app.ui.stac.children.clear();
            app.ui.stac.collections.clear();
            app.ui.stac.items.clear();
            app.run("ui.dialog", json!({"name": "stac"})).map(|_| ())
        }
        "stac" => app.run("ui.stacLink", json!({"url": s.address})).map(|_| ()),
        "sample" => app.run("layer.addSample", json!({"name": s.address})).map(|_| ()),
        "s3" => {
            crate::s3ui::open_connection(app, &s.address);
            app.run("ui.dialog", json!({"name": "s3"})).map(|_| ())
        }
        other => Err(format!("unknown source kind '{other}'")),
    }
}

/// One source's row: its icon and name (a click adds or opens it), and a menu.
fn row(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, s: &Source, list: Option<&str>) {
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        icons::paint(ui.painter(), r, icon_of(&s.kind), t.icon, t.accent);
        let resp = ui.link(&s.name).on_hover_text(&s.address);
        if resp.clicked()
            && let Err(e) = open(app, s)
        {
            app.session.notify(e, true);
        }
        resp.context_menu(|ui| {
            let fav = app.session.sources.favourites.iter().any(|f| f.is(&s.kind, &s.address));
            let p = json!({"kind": s.kind, "address": s.address, "name": s.name, "extra": s.extra});
            if !fav && ui.button(tl!("Add to Favourites")).clicked() {
                let _ = app.run("sources.favourite", p.clone());
                ui.close();
            }
            if fav && ui.button(tl!("Remove from Favourites")).clicked() {
                let mut q = p.clone();
                q["on"] = json!(false);
                let _ = app.run("sources.favourite", q);
                ui.close();
            }
            if let Some(list) = list.filter(|l| *l != "favourites")
                && ui.button(tl!("Remove")).clicked()
            {
                let _ = app.run("sources.remove", json!({"list": list, "kind": s.kind, "address": s.address}));
                ui.close();
            }
            if ui.button(tl!("Copy Address")).clicked() {
                ui.ctx().copy_text(s.address.clone());
                ui.close();
            }
        });
    });
}

fn header(ui: &mut Ui, title: &str, open: bool, body: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).font(semibold(12.5))).id_salt(("sources", title)).default_open(open).show(ui, body);
}

fn empty(ui: &mut Ui, what: &str) {
    ui.label(RichText::new(what).small().weak());
}

fn connections(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, list: &[Connection]) {
    crate::connectiontree::prune(app);
    // A project's S3 sources waiting for the saved connection that reads them.
    if !app.session.s3_pending.is_empty()
        && ui
            .button(tl!("Choose S3 connection…"))
            .on_hover_text(tl!("Some of the project's S3 sources wait for you to choose the saved connection that reads them"))
            .clicked()
        && let Err(e) = app.run("ui.s3Connections", Value::Null)
    {
        app.session.notify(e, true);
    }
    ui.horizontal(|ui| {
        ui.menu_button(tl!("New Connection ▾"), |ui| {
            for (kind, label) in crate::connectionui::KINDS {
                if cfg!(target_arch = "wasm32") && DATABASE_KINDS.contains(&kind) {
                    continue;
                }
                if ui.button(tl!(label)).clicked() {
                    if let Err(e) = app.run("ui.connection", json!({"kind": kind})) {
                        app.session.notify(e, true);
                    }
                    ui.close();
                }
            }
        });
        if ui.small_button("⟳").on_hover_text(tl!("Refresh every connection's lists")).clicked() {
            for c in list.iter().filter(|c| DATABASE_KINDS.contains(&c.kind.as_str())) {
                app.session.database_logins.retry_saved(&c.id);
            }
            crate::connectiontree::refresh(app, None);
        }
    });
    if list.is_empty() {
        empty(ui, tl!("Connections you add are listed here"));
    }
    let mut groups = std::collections::BTreeMap::new();
    for c in list.iter().filter(|c| !c.group.is_empty()) {
        groups.entry(c.group.to_lowercase()).or_insert(c.group.as_str());
    }
    for (lower, group) in groups {
        egui::CollapsingHeader::new(group).id_salt(("connections", &lower)).default_open(true).show(ui, |ui| {
            for c in list.iter().filter(|c| c.group.to_lowercase() == lower) {
                connection_row(app, ui, t, c);
            }
        });
    }
    for c in list.iter().filter(|c| c.group.is_empty()) {
        connection_row(app, ui, t, c);
    }
}

fn connection_row(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, c: &Connection) {
    let unavailable = DATABASE_KINDS.contains(&c.kind.as_str()) && cfg!(target_arch = "wasm32");
    if unavailable {
        ui.horizontal(|ui| {
            connection_header(app, ui, t, c, true);
        });
        return;
    }
    // A click on the name opens the row: the datasets the connection offers are listed under it.
    let state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), ui.make_persistent_id(("connection", &c.id)), false);
    let open = state.is_open();
    let mut clicked = false;
    let mut header = state.show_header(ui, |ui| clicked = connection_header(app, ui, t, c, false));
    if clicked {
        header.set_open(!open);
    }
    header.body(|ui| crate::connectiontree::show(app, ui, t, c));
}

/// A connection's icon, name, kind and menu. True when the name was clicked.
fn connection_header(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, c: &Connection, unavailable: bool) -> bool {
    let icon = if DATABASE_KINDS.contains(&c.kind.as_str()) {
        "table"
    } else {
        match c.kind.as_str() {
            "stac" => "search",
            "s3" => "package",
            _ => "globe_view",
        }
    };
    let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    icons::paint(ui.painter(), r, icon, t.icon, t.accent);
    let response = if unavailable {
        ui.link(RichText::new(&c.name).weak()).on_hover_text(tl!("Database connections are used by the desktop app"))
    } else {
        ui.link(&c.name).on_hover_text(if c.kind == "s3" && c.address.is_empty() { tl!("Amazon S3") } else { &c.address })
    };
    // The status precedes the kind: the narrow pane clips trailing row text.
    if DATABASE_KINDS.contains(&c.kind.as_str()) && c.remembered {
        ui.label(RichText::new(tl!("Saved password")).small().weak());
    }
    // The saved-keys marker (the store isn't asked while drawing).
    if c.kind == "s3" && c.saved_keys {
        if app.session.database_logins.has_store() {
            ui.label(RichText::new(tl!("Saved keys")).small().weak())
                .on_hover_text(tl!("This computer keeps this connection's keys: logging in reads them."));
        } else {
            ui.label(RichText::new(tl!("Saved keys unavailable")).small().weak())
                .on_hover_text(tl!("Keys were saved for this connection, but there is no credential store here to read them from."));
        }
    }
    ui.label(RichText::new(tl!(crate::connectionui::label(&c.kind))).small().weak());
    response.context_menu(|ui| {
        let mut items = vec![
            (tl!("Edit…"), "ui.connection", json!({"id":c.id})),
            (tl!("Test"), "ui.connection", json!({"id":c.id,"test":true})),
            (tl!("Duplicate"), "connection.duplicate", json!({"id":c.id})),
        ];
        if unavailable {
            items.remove(1);
        }
        for (label, command, params) in items {
            if ui.button(label).clicked() {
                if let Err(e) = app.run(command, params) {
                    app.session.notify(e, true);
                }
                ui.close();
            }
        }
        if !unavailable {
            let keys = c.kind == "s3" && c.options.get("credentials").is_some_and(|v| v == "keys");
            if (DATABASE_KINDS.contains(&c.kind.as_str()) || keys) && ui.button(tl!("Log In…")).clicked() {
                crate::connectionui::log_in(app, &c.id);
                ui.close();
            }
            if DATABASE_KINDS.contains(&c.kind.as_str()) && c.remembered && ui.button(tl!("Forget Saved Password")).clicked() {
                match app.run("connection.forgetPassword", json!({"id":c.id})) {
                    Ok(_) => crate::connectiontree::refresh(app, Some(&c.id)),
                    Err(e) => app.session.notify(e, true),
                }
                ui.close();
            }
            if ui.button(tl!("Refresh")).clicked() {
                if DATABASE_KINDS.contains(&c.kind.as_str()) {
                    app.session.database_logins.retry_saved(&c.id);
                }
                crate::connectiontree::refresh(app, Some(&c.id));
                ui.close();
            }
            // The kind's own add dialog, as a click on the connection opened it before.
            if ui.button(tl!("Add With Options…")).clicked() {
                if let Err(e) = crate::connectionui::open_connection(app, c) {
                    app.session.notify(e, true);
                }
                ui.close();
            }
            if c.kind == "s3" && c.saved_keys && ui.button(tl!("Forget Saved Keys")).clicked() {
                match crate::connectionui::forget_keys(app, &c.id) {
                    Ok(()) => app.session.notify(
                        tl!("The keys saved on this computer were forgotten; a login in this session keeps working.").to_string(),
                        false,
                    ),
                    Err(e) => app.session.notify(e, true),
                }
                ui.close();
            }
        }
        if !c.address.is_empty() && ui.button(tl!("Copy Address")).clicked() {
            ui.ctx().copy_text(c.address.clone());
            ui.close();
        }
        if ui.button(tl!("Remove")).clicked() {
            match app.run("connection.remove", json!({"id":c.id})) {
                Ok(_) => crate::connectiontree::refresh(app, Some(&c.id)),
                Err(e) => app.session.notify(e, true),
            }
            ui.close();
        }
    });
    !unavailable && response.clicked()
}

pub fn show(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    load(app);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.label(
                RichText::new(tl!("Click a source to add it, or to open its browser connected. Right-click for favourites."))
                    .small()
                    .weak(),
            );
            // A snapshot shared across frames, taken again only when the sources change.
            let src = snapshot(app);
            header(ui, tl!("Favourites"), true, |ui| {
                if src.favourites.is_empty() {
                    empty(ui, tl!("Right-click a source to add it here"));
                }
                for s in &src.favourites {
                    row(app, ui, t, s, Some("favourites"));
                }
            });
            header(ui, tl!("Recent"), true, |ui| {
                if src.recent.is_empty() {
                    empty(ui, tl!("Data you add is listed here"));
                }
                for s in &src.recent {
                    row(app, ui, t, s, Some("recent"));
                }
            });
            // A web page can't reach local folders.
            if !cfg!(target_arch = "wasm32") {
                header(ui, tl!("Folders"), false, |ui| {
                    let folders = app.session.project.folders.clone();
                    for f in folders {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                            icons::paint(ui.painter(), r, "add_folder", t.icon, t.accent);
                            if ui.link(&f).on_hover_text(tl!("Browse it in the Browser")).clicked() {
                                app.table.folder = Some(std::path::PathBuf::from(&f));
                                app.ui.left = Some(crate::LeftPane::Catalog);
                            }
                        });
                    }
                    if ui.small_button(tl!("Add Folder…")).clicked() {
                        let _ = app.run("ui.addFolder", Value::Null);
                    }
                });
            }
            let list = connections_snapshot(app);
            header(ui, tl!("Connections"), true, |ui| connections(app, ui, t, &list));
            header(ui, tl!("Library"), false, |ui| {
                let mut groups: Vec<&str> = Vec::new();
                let lib = library();
                for (g, _) in lib {
                    if !groups.contains(g) {
                        groups.push(g);
                    }
                }
                for g in groups {
                    egui::CollapsingHeader::new(tl!(g)).id_salt(("library", g)).default_open(false).show(ui, |ui| {
                        for (_, s) in lib.iter().filter(|(x, _)| x == &g) {
                            row(app, ui, t, s, None);
                        }
                    });
                }
            });
        });
    });
}
