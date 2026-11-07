//! The datasets a saved connection offers, listed under it in Data Sources › Connections: one
//! level at a time as it is expanded (`connection.browse` names what each level reads, the app
//! fetches it off the frame and asks again with the answer), with the selection that
//! Add Selected (in a dataset's menu) adds. Each dataset added is its own undo step and retains
//! its saved web-service connection id, separately from its display name in Recent.

use std::collections::HashMap;
use std::sync::Arc;

use egui::collapsing_header::CollapsingState;
use egui::{Label, RichText, Sense, Ui};
use rustgis_engine::connections::{Connection, DATABASE_KINDS};
use rustgis_engine::render::rustgis_tiles::stac;
use rustgis_engine::sources::Source;
use serde_json::{Value, json};

use crate::serviceui::{self, ServiceSource};
use crate::theme::Tokens;
use crate::{RustGisApp, Slot, i18n};

type Key = (String, String);

#[derive(Default)]
pub struct State {
    /// What each level of each connection lists: (connection id, node; empty for the top).
    entries: HashMap<Key, Entry>,
    /// The definition each connection's entries were made for: an edit makes them stale.
    seen: HashMap<String, Connection>,
    pub picks: Vec<Pick>,
    /// STAC assets waiting on a token.
    tokens: Vec<Signing>,
    /// The connection a `ui.addUrl` being run is for, handed to it out of band ([`run_add_url`]),
    /// so it never shows in the command's parameters (recordings, the log).
    origin: Option<String>,
}

/// A STAC asset waiting on its signing token: the token's address, the asset, the layer's name,
/// the connection it was added from (named in Recent once it is added) and the token on its way.
struct Signing {
    /// The project that asked for the asset, checked before caching its token or adding data.
    opened: u64,
    token_url: String,
    href: String,
    name: String,
    origin: Option<String>,
    connection: Option<String>,
    slot: Slot<Result<Vec<u8>, String>>,
}

/// The key a `ui.addUrl` still waiting on a probe or a download carries the connection's name
/// under, in its parameters. Taken off before any command sees them.
pub(crate) const ORIGIN: &str = "_origin";

/// Runs `ui.addUrl` for a dataset of the connection named `origin` (none: for any other source):
/// whichever way the download ends, the add is noted in Recent under that name.
pub(crate) fn run_add_url(app: &mut RustGisApp, params: Value, origin: Option<&str>) -> Result<Value, String> {
    app.ui.connection_tree.origin = origin.map(str::to_string);
    let r = app.run("ui.addUrl", params);
    app.ui.connection_tree.origin = None;
    r
}

/// The connection `ui.addUrl` was run for ([`run_add_url`]).
pub(crate) fn take_origin(app: &mut RustGisApp) -> Option<String> {
    app.ui.connection_tree.origin.take()
}

/// The connection a pending `ui.addUrl`'s parameters carry ([`ORIGIN`]), taken off them.
pub(crate) fn take_origin_key(params: &mut Value) -> Option<String> {
    match params.as_object_mut()?.remove(ORIGIN)? {
        Value::String(name) => Some(name),
        _ => None,
    }
}

/// Runs a command that adds a dataset now, noting it in Recent under the connection's name.
pub(crate) fn run_named(app: &mut RustGisApp, id: &str, params: Value, origin: Option<&str>) -> Result<Value, String> {
    let before = app.session.sources.generation;
    let r = app.run(id, params);
    if r.is_ok() {
        name_recent(app, before, origin);
    }
    r
}

/// Names the source the session noted since `before` (a [`Sources::generation`]) as Recent
/// shows it, after the connection it came from. The kind, address and `extra` are the ones noted
/// (cleaned and redacted), so reopening it targets the same dataset; a name is a snapshot, taken
/// when added. Nothing noted (a failed add, one still on its way) leaves Recent as it was.
pub(crate) fn name_recent(app: &mut RustGisApp, before: u64, origin: Option<&str>) {
    let Some(origin) = origin else { return };
    if app.session.sources.generation == before {
        return;
    }
    let Some(mut added) = app.session.sources.recent.first().cloned() else { return };
    added.name = origin.to_string();
    app.session.sources.note(added);
}

pub enum Entry {
    Loading(Fetch),
    Ready(Arc<Listing>),
    Failed(String),
}

/// A level whose answer is on its way.
pub struct Fetch {
    node: String,
    /// The first answer, and the further documents a service's discovery asked for.
    first: Option<String>,
    documents: Vec<Value>,
    /// The address being fetched.
    pending: String,
    slot: Slot<Result<Vec<u8>, String>>,
}

pub struct Listing {
    pub children: Vec<Value>,
    pub truncated: bool,
    pub matched: Option<u64>,
    pub notes: Vec<String>,
    /// The service its layers belong to, to add them from the documents already read.
    pub source: Option<Arc<ServiceSource>>,
}

/// A dataset selected for Add Selected.
pub struct Pick {
    pub conn: String,
    pub key: String,
    pub name: String,
    pub child: Value,
    pub source: Option<Arc<ServiceSource>>,
}

fn params(id: &str, node: &str) -> Value {
    let mut p = json!({"id": id});
    if !node.is_empty() {
        p["node"] = json!(node);
    }
    p
}

/// Starts reading a level: what it needs fetched is asked for, and a level read without a fetch
/// (a database's tables, an S3 connection's own buckets) is read now. Errors go into the entry.
fn start(app: &mut RustGisApp, c: &Connection, node: &str) {
    let mut probe = params(&c.id, node);
    probe["probe"] = json!(true);
    let entry = match app.session.run("connection.browse", &probe) {
        Err(e) => Entry::Failed(e),
        Ok(v) => match v["fetch"].as_str() {
            None => match app.session.run("connection.browse", &params(&c.id, node)) {
                Ok(v) => Entry::Ready(Arc::new(listing(&v, None, Vec::new()))),
                Err(e) => Entry::Failed(e),
            },
            Some(url) => Entry::Loading(Fetch {
                node: node.to_string(),
                first: None,
                documents: Vec::new(),
                pending: url.to_string(),
                slot: crate::fetch_into(url),
            }),
        },
    };
    app.ui.connection_tree.entries.insert((c.id.clone(), node.to_string()), entry);
}

fn listing(v: &Value, first: Option<String>, documents: Vec<Value>) -> Listing {
    let source = v.get("service").filter(|s| s.is_object()).map(|s| {
        Arc::new(ServiceSource {
            url: s["url"].as_str().unwrap_or("").to_string(),
            kind: serviceui::kind_of(s["type"].as_str().unwrap_or("")),
            text: first.unwrap_or_default(),
            documents,
        })
    });
    Listing {
        children: v["children"].as_array().cloned().unwrap_or_default(),
        truncated: v["truncated"] == true,
        matched: v["matched"].as_u64(),
        notes: v["notes"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect(),
        source,
    }
}

/// An answer arrived: asks again with it (and what came before). A service's discovery may need
/// further documents, which are fetched in turn.
fn advance(app: &mut RustGisApp, id: &str, mut f: Fetch, bytes: Vec<u8>) -> Entry {
    let Ok(text) = crate::service_text(bytes) else {
        return Entry::Failed(tl!("The answer isn't UTF-8 text").to_string());
    };
    if f.first.is_none() {
        f.first = Some(text);
    } else {
        f.documents.push(json!({"url": f.pending, "text": text}));
    }
    let mut p = params(id, &f.node);
    p["answer"] = json!(f.first);
    if !f.documents.is_empty() {
        p["documents"] = json!(f.documents);
    }
    match app.session.run("connection.browse", &p) {
        Err(e) => Entry::Failed(e),
        Ok(v) => match v["next"].as_str() {
            Some(next) => {
                f.pending = next.to_string();
                f.slot = crate::fetch_into(next);
                Entry::Loading(f)
            }
            None => Entry::Ready(Arc::new(listing(&v, f.first, f.documents))),
        },
    }
}

/// Advances connection listings and only the asset signings owned by the current project.
pub fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    let tree = &mut app.ui.connection_tree;
    let mut landed = Vec::new();
    for (key, entry) in &tree.entries {
        if let Entry::Loading(f) = entry
            && let Some(r) = f.slot.lock().unwrap_or_else(|e| e.into_inner()).take()
        {
            landed.push((key.clone(), r));
        }
    }
    for (key, r) in landed {
        let Some(Entry::Loading(f)) = app.ui.connection_tree.entries.remove(&key) else { continue };
        let entry = match r {
            Ok(bytes) => advance(app, &key.0, f, bytes),
            Err(e) => Entry::Failed(e),
        };
        app.ui.connection_tree.entries.insert(key, entry);
    }
    // STAC assets that needed a token: signed and added when it lands.
    for signing in std::mem::take(&mut app.ui.connection_tree.tokens) {
        if !app.service_read_current(signing.opened, &signing.href, None) {
            continue;
        }
        let Some(r) = signing.slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            app.ui.connection_tree.tokens.push(signing);
            continue;
        };
        match r.and_then(|b| stac::parse_sas(&b)) {
            Ok(token) => {
                stac::remember_token(&signing.token_url, &token);
                let mut p = json!({"url": stac::sign(&signing.href, &token), "name": signing.name, "zoom": true});
                if let Some(connection) = signing.connection {
                    p["connection"] = json!(connection);
                }
                if let Err(e) = run_add_url(app, p, signing.origin.as_deref()) {
                    app.session.notify(crate::connectionui::translated(&e), true);
                }
                app.map.invalidate();
            }
            Err(e) => app.session.notify(e, true),
        }
    }
    let tree = &app.ui.connection_tree;
    if tree.entries.values().any(|e| matches!(e, Entry::Loading(_))) || !tree.tokens.is_empty() || !app.ui.service_steps.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Adds one dataset a listing offers. Each branch is one command, so each is one undo step.
/// `origin` names the connection in Recent; `connection` is its saved web-service id, kept
/// through asynchronous discovery, signing and downloads so the resulting layer records it.
fn add_from(
    app: &mut RustGisApp,
    child: &Value,
    source: Option<&ServiceSource>,
    origin: Option<&str>,
    connection: Option<&str>,
) -> Result<(), String> {
    let add = child.get("add").filter(|a| a.is_object()).ok_or_else(|| tl!("Nothing to add here").to_string())?;
    let command = add["command"].as_str().unwrap_or("");
    let mut params = add["params"].clone();
    if let Some(connection) = connection {
        if params.is_null() {
            params = json!({});
        }
        // Params that aren't an object are the command's to refuse.
        if let Some(object) = params.as_object_mut() {
            object.insert("connection".into(), json!(connection));
        }
    }
    match (source, child.get("layer").filter(|l| l.is_object())) {
        (Some(source), Some(layer)) => {
            serviceui::add_layer_from(app, source, layer, "", None, false, origin, connection)?;
        }
        _ => match command {
            "layer.addUrl" => {
                run_add_url(app, params, origin)?;
            }
            "stac.addAsset" => {
                add_asset(app, child["href"].as_str().unwrap_or(""), params["name"].as_str().unwrap_or(""), origin, connection)?;
            }
            _ => {
                run_named(app, command, params, origin)?;
            }
        },
    }
    app.map.invalidate();
    Ok(())
}

/// A STAC asset by its address, signed first when it needs a token (one in hand is used; one to
/// fetch is fetched off the frame and the asset added when it lands).
fn add_asset(app: &mut RustGisApp, href: &str, name: &str, origin: Option<&str>, connection: Option<&str>) -> Result<(), String> {
    let mut p = json!({"url": href, "name": name, "zoom": true});
    if let Some(connection) = connection {
        p["connection"] = json!(connection);
    }
    if !stac::needs_signing(href) {
        run_add_url(app, p, origin)?;
        return Ok(());
    }
    let token_url = stac::sas_token_url(href)?;
    if let Some(token) = stac::cached_token(&token_url) {
        p["url"] = json!(stac::sign(href, &token));
        run_add_url(app, p, origin)?;
    } else {
        let slot = crate::fetch_into(&token_url);
        let (href, name, origin) = (href.to_string(), name.to_string(), origin.map(str::to_string));
        let connection = connection.map(str::to_string);
        app.ui.connection_tree.tokens.push(Signing { opened: app.session.opened, token_url, href, name, origin, connection, slot });
    }
    Ok(())
}

/// Opens the kind's own add dialog on a dataset: a table in the database dialog, a service layer
/// in the service dialog, a STAC collection in the STAC dialog.
fn add_with_options(app: &mut RustGisApp, c: &Connection, child: &Value, source: Option<&ServiceSource>) -> Result<(), String> {
    match child["what"].as_str().unwrap_or("") {
        "table" => {
            crate::sourcespane::open(
                app,
                &Source {
                    kind: "database".into(),
                    name: c.name.clone(),
                    address: c.address.clone(),
                    extra: json!({"layer": child["key"]}),
                },
            )?;
            app.dialogs.connection = Some(c.id.clone());
            Ok(())
        }
        "collection" => {
            crate::sourcespane::open(
                app,
                &Source { kind: "stac".into(), name: c.name.clone(), address: c.address.clone(), extra: json!({"api": "true"}) },
            )?;
            app.ui.stac.collection = child["detail"].as_str().unwrap_or("").to_string();
            app.ui.stac.connection = Some(c.id.clone());
            Ok(())
        }
        _ => match (source, child.get("layer").filter(|l| l.is_object())) {
            (Some(source), Some(layer)) => {
                app.run("ui.addService", json!({"url": source.url, "type": source.kind, "connection": c.id}))?;
                app.dialogs.connection = Some(c.id.clone());
                app.ui.service_browse.filter = layer["name"].as_str().unwrap_or("").to_string();
                Ok(())
            }
            _ => Err(tl!("Nothing to add here").to_string()),
        },
    }
}

/// What a listed child is, in words.
fn what_label(what: &str) -> &'static str {
    match what {
        "schema" => tl!("schema"),
        "table" => tl!("table"),
        "folder" => tl!("folder"),
        "service" => tl!("service"),
        "map" => tl!("map"),
        "vectorTiles" => tl!("vector tiles"),
        "features" => tl!("features"),
        "coverage" => tl!("coverage"),
        "style" => tl!("style"),
        "record" => tl!("record"),
        "download" => tl!("download"),
        "collection" => tl!("collection"),
        "catalog" => tl!("catalog"),
        "item" => tl!("item"),
        "asset" => tl!("asset"),
        "bucket" => tl!("bucket"),
        "prefix" => tl!("folder"),
        "object" => tl!("file"),
        _ => "",
    }
}

fn picked(app: &RustGisApp, conn: &str, key: &str) -> bool {
    app.ui.connection_tree.picks.iter().any(|p| p.conn == conn && p.key == key)
}

fn toggle(app: &mut RustGisApp, c: &Connection, child: &Value, source: Option<&Arc<ServiceSource>>) {
    let key = child["key"].as_str().unwrap_or("");
    let picks = &mut app.ui.connection_tree.picks;
    if let Some(i) = picks.iter().position(|p| p.conn == c.id && p.key == key) {
        picks.remove(i);
    } else {
        picks.push(Pick {
            conn: c.id.clone(),
            key: key.to_string(),
            name: child["name"].as_str().unwrap_or("").to_string(),
            child: child.clone(),
            source: source.cloned(),
        });
    }
}

/// Forgets what a connection (or, with `None`, every connection) listed and what was selected
/// from it: the next expansion reads it again.
pub fn refresh(app: &mut RustGisApp, id: Option<&str>) {
    let tree = &mut app.ui.connection_tree;
    match id {
        Some(id) => {
            tree.entries.retain(|(c, _), _| c != id);
            tree.picks.retain(|p| p.conn != id);
        }
        None => {
            tree.entries.clear();
            tree.picks.clear();
        }
    }
}

/// Drops the picks of a connection that was edited or removed since (even while its row is
/// folded, when its list isn't refreshed): they would add from the old definition.
pub fn prune(app: &mut RustGisApp) {
    let (connections, tree) = (&app.session.connections, &mut app.ui.connection_tree);
    let seen = &tree.seen;
    tree.picks.retain(|p| connections.find(&p.conn).is_some_and(|c| seen.get(&p.conn) == Some(c)));
}

/// Adds every selected dataset, each as its own undo step, and clears the selection.
fn add_picks(app: &mut RustGisApp) {
    prune(app);
    for pick in std::mem::take(&mut app.ui.connection_tree.picks) {
        // The name as it is now, for the whole add (pruned above: the connection is there).
        let connection = app.session.connections.find(&pick.conn);
        let origin = connection.map(|c| c.name.clone());
        let service = connection.filter(|c| rustgis_engine::service_logins::is_service(c)).map(|c| c.id.clone());
        if let Err(e) = add_from(app, &pick.child, pick.source.as_deref(), origin.as_deref(), service.as_deref()) {
            app.session.notify(crate::connectionui::translated(&e), true);
        }
    }
}

/// A connection's expanded body: its top level, listed (or its error).
pub fn show(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, c: &Connection) {
    // An edited connection lists what it now offers.
    if app.ui.connection_tree.seen.get(&c.id) != Some(c) {
        refresh(app, Some(&c.id));
        app.ui.connection_tree.seen.insert(c.id.clone(), c.clone());
    }
    level(app, ui, t, c, "");
}

/// One level: loading, its error with the ways to go on, or its children.
fn level(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, c: &Connection, node: &str) {
    let key = (c.id.clone(), node.to_string());
    let shown = match app.ui.connection_tree.entries.get(&key) {
        None => None,
        Some(Entry::Loading(_)) => Some(Err(None)),
        Some(Entry::Failed(e)) => Some(Err(Some(e.clone()))),
        Some(Entry::Ready(l)) => Some(Ok(l.clone())),
    };
    match shown {
        None => {
            start(app, c, node);
            ui.ctx().request_repaint();
        }
        Some(Err(None)) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tl!("Loading…"));
            });
        }
        Some(Err(Some(e))) => {
            ui.label(RichText::new(crate::connectionui::translated(&e)).small().color(t.red));
            ui.horizontal(|ui| {
                if ui.small_button(tl!("Retry")).clicked() {
                    app.ui.connection_tree.entries.remove(&key);
                }
                let keys = c.kind == "s3" && c.options.get("credentials").is_some_and(|v| v == "keys");
                let service_login = rustgis_engine::service_logins::is_service(c) && !c.auth.is_empty();
                if (DATABASE_KINDS.contains(&c.kind.as_str()) || keys || service_login) && ui.small_button(tl!("Log In…")).clicked() {
                    crate::connectionui::log_in(app, &c.id);
                }
            });
        }
        Some(Ok(l)) => {
            children(app, ui, t, c, &l, &l.children);
            if l.children.is_empty() {
                ui.label(RichText::new(tl!("Nothing here")).small().weak());
            }
            if l.truncated {
                let n = l.children.len().to_string();
                let text = match l.matched {
                    Some(m) => i18n::fmt("{n} of {m} are listed.", &[("n", &n), ("m", &m.to_string())]),
                    None => i18n::fmt("The first {n} are listed.", &[("n", &n)]),
                };
                ui.label(RichText::new(text).small().weak());
            }
            for note in &l.notes {
                ui.label(RichText::new(note).small().weak());
            }
        }
    }
}

/// A folder in the tree: a header that opens on a click, and a body drawn when it is open.
/// `node` is set when the body is a level of its own (read when first opened, refreshed from
/// the header's menu).
fn branch(
    app: &mut RustGisApp,
    ui: &mut Ui,
    c: &Connection,
    child: &Value,
    node: Option<&str>,
    body: impl FnOnce(&mut RustGisApp, &mut Ui),
) {
    let salt = node.or(child["key"].as_str()).unwrap_or("");
    let id = ui.make_persistent_id(("conntree", &c.id, salt));
    let state = CollapsingState::load_with_default_open(ui.ctx(), id, false);
    let open = state.is_open();
    let (mut clicked, mut refreshed) = (false, false);
    let mut header = state.show_header(ui, |ui| {
        ui.label(RichText::new(what_label(child["what"].as_str().unwrap_or(""))).small().weak());
        let r = ui.add(Label::new(child["name"].as_str().unwrap_or("")).sense(Sense::click()));
        clicked = r.clicked();
        if node.is_some() {
            r.context_menu(|ui| {
                if ui.button(tl!("Refresh")).clicked() {
                    refreshed = true;
                    ui.close();
                }
            });
        }
    });
    if clicked {
        header.set_open(!open);
    }
    header.body(|ui| body(app, ui));
    if refreshed && let Some(node) = node {
        app.ui.connection_tree.entries.remove(&(c.id.clone(), node.to_string()));
    }
}

fn children(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, c: &Connection, listing: &Listing, list: &[Value]) {
    for child in list {
        if let Some(node) = child["node"].as_str() {
            branch(app, ui, c, child, Some(node), |app, ui| level(app, ui, t, c, node));
            continue;
        }
        if let Some(inline) = child["children"].as_array() {
            branch(app, ui, c, child, None, |app, ui| {
                children(app, ui, t, c, listing, inline);
                if inline.is_empty() {
                    ui.label(RichText::new(tl!("Nothing here")).small().weak());
                }
            });
            continue;
        }
        dataset(app, ui, c, listing, child);
    }
}

/// A dataset: click selects it, double-click adds it, the menu adds it (with options).
fn dataset(app: &mut RustGisApp, ui: &mut Ui, c: &Connection, listing: &Listing, child: &Value) {
    let name = child["name"].as_str().unwrap_or("");
    let is_picked = picked(app, &c.id, child["key"].as_str().unwrap_or(""));
    let r = ui
        .horizontal(|ui| {
            let r = ui.selectable_label(is_picked, name);
            ui.label(RichText::new(what_label(child["what"].as_str().unwrap_or(""))).small().weak());
            r
        })
        .inner;
    // A dataset without an add (an OGC API feature collection, imported together) is added
    // with options only.
    let addable = child.get("add").is_some_and(Value::is_object);
    let hint = if addable { tl!("Click to select, double-click to add, right-click for more") } else { tl!("Right-click for more") };
    let mut hover = hint.to_string();
    if let Some(detail) = child["detail"].as_str().filter(|d| !d.is_empty()) {
        hover = format!("{hover}\n{detail}");
    }
    let r = r.on_hover_text(hover);
    let (mut add_now, mut with_options, mut add_selected, mut clear) = (false, false, false, false);
    let selected = app.ui.connection_tree.picks.len();
    r.context_menu(|ui| {
        if addable && ui.button(tl!("Add")).clicked() {
            add_now = true;
            ui.close();
        }
        if child["options"] == true && ui.button(tl!("Add With Options…")).clicked() {
            with_options = true;
            ui.close();
        }
        if selected > 0 {
            ui.separator();
            if ui.button(i18n::fmt("Add Selected ({n})", &[("n", &selected.to_string())])).clicked() {
                add_selected = true;
                ui.close();
            }
            if ui.button(tl!("Clear Selection")).clicked() {
                clear = true;
                ui.close();
            }
        }
    });
    if addable && r.double_clicked() {
        // The first click selected it: a double-click adds it alone.
        let key = child["key"].as_str().unwrap_or("");
        app.ui.connection_tree.picks.retain(|p| !(p.conn == c.id && p.key == key));
        add_now = true;
    } else if addable && r.clicked() {
        toggle(app, c, child, listing.source.as_ref());
    }
    if clear {
        app.ui.connection_tree.picks.clear();
    }
    if add_selected {
        add_picks(app);
    }
    let result = if with_options {
        add_with_options(app, c, child, listing.source.as_deref())
    } else if add_now {
        let connection = rustgis_engine::service_logins::is_service(c).then_some(c.id.as_str());
        add_from(app, child, listing.source.as_deref(), Some(c.name.as_str()), connection)
    } else {
        Ok(())
    };
    if let Err(e) = result {
        app.session.notify(e, true);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    use rustgis_engine::sources::{self, Source};
    use std::io::{Read, Write};

    type Harness = egui_kittest::Harness<'static, RustGisApp>;

    fn harness() -> Harness {
        egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_max_steps(64).with_step_dt(0.05).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            RustGisApp::with_sample(),
        )
    }

    /// Answers every request on a local port with `answer(request head)`: status and body.
    fn serve(answer: impl Fn(&str) -> (u16, Vec<u8>) + Send + 'static) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let (status, body) = answer(&String::from_utf8_lossy(&buf[..n]));
                let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(&body);
            }
        });
        base
    }

    fn until(h: &mut Harness, done: impl Fn(&RustGisApp) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done(h.state()) {
            assert!(std::time::Instant::now() < deadline, "timed out waiting");
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Capabilities whose GetMap endpoint is loopback (port 9 refuses): no test reaches the network.
    const WMS: &str = r#"<WMS_Capabilities version="1.3.0" xmlns="http://www.opengis.net/wms" xmlns:xlink="http://www.w3.org/1999/xlink"><Service><Title>Local</Title></Service><Capability><Request><GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="http://127.0.0.1:9/wms?"/></Get></HTTP></DCPType></GetMap></Request><Layer><CRS>EPSG:3857</CRS><Layer><Name>a</Name><Title>A</Title></Layer><Layer><Name>b</Name><Title>B</Title></Layer></Layer></Capability></WMS_Capabilities>"#;

    fn wms(url: &str) -> ServiceSource {
        let endpoint = format!("{}?", url.split('?').next().unwrap_or(url));
        ServiceSource { url: url.into(), kind: "wms", text: WMS.replace("http://127.0.0.1:9/wms?", &endpoint), documents: Vec::new() }
    }

    /// A listed WMS layer, as the tree adds it.
    fn layer(name: &str) -> Value {
        json!({"add": {"command": "service.add", "params": {}}, "layer": {"name": name, "title": name.to_uppercase()}})
    }

    /// The Recent entry a `service.add` of this layer gives, named `name`: what the session
    /// notes (cleaned, redacted), with only the name replaced.
    fn expected(url: &str, layer: &str, name: &str) -> Source {
        let mut added = sources::added_by("service.add", &json!({"url": url, "layer": layer, "type": "wms"})).unwrap();
        added.name = name.into();
        sources::clean(added).unwrap()
    }

    #[test]
    fn a_tree_add_is_recent_under_the_connection_name_with_its_own_source() {
        let mut h = harness();
        let app = h.state_mut();
        let url = "http://127.0.0.1:9/wms";
        let layers = app.session.map().tiles.len();
        add_from(app, &layer("a"), Some(&wms(url)), Some("Local"), None).unwrap();
        assert_eq!(app.session.map().tiles.len(), layers + 1);
        assert_eq!(app.session.sources.recent.first(), Some(&expected(url, "a", "Local")));
        assert_eq!(app.session.sources.recent[0].extra, json!({"layer": "a", "type": "wms"}));
        // The layer keeps the name the service gave it.
        assert_ne!(app.session.map().tiles.last().map(|t| t.name.as_str()), Some("Local"));

        // A name is a snapshot: renaming the connection leaves it until something is added again.
        let id = app.session.run("connection.new", &json!({"kind": "wms", "address": url, "name": "Local"})).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        app.session.run("connection.edit", &json!({"id": id, "name": "Renamed"})).unwrap();
        assert_eq!(app.session.sources.recent[0].name, "Local");
        let name = app.session.connections.find(&id).unwrap().name.clone();
        add_from(app, &layer("b"), Some(&wms(url)), Some(&name), Some(&id)).unwrap();
        // One entry per kind and address: the re-add takes the current name and layer.
        assert_eq!(app.session.map().tiles.last().unwrap().connection, id);
        assert_eq!(app.session.sources.recent.iter().filter(|s| s.address == url).count(), 1);
        assert_eq!(app.session.sources.recent.first(), Some(&expected(url, "b", "Renamed")));

        // Another connection to the same address shares the entry.
        add_from(app, &layer("a"), Some(&wms(url)), Some("Twin"), None).unwrap();
        assert_eq!(app.session.sources.recent.iter().filter(|s| s.address == url).count(), 1);
        assert_eq!(app.session.sources.recent.first(), Some(&expected(url, "a", "Twin")));

        // An address with a secret is redacted exactly as a plain add redacts it.
        let secret = "http://127.0.0.1:9/wms?token=SECRET&access_token=SECRET";
        add_from(app, &layer("a"), Some(&wms(secret)), Some("Local"), None).unwrap();
        let noted = app.session.sources.recent.first().unwrap();
        assert!(!noted.address.contains("SECRET"), "{}", noted.address);
        assert_eq!(noted.name, "Local");
    }

    #[test]
    fn a_failed_add_is_not_renamed_or_recorded() {
        let mut h = harness();
        let app = h.state_mut();
        let before = app.session.sources.recent.clone();
        assert!(add_from(app, &layer("nope"), Some(&wms("http://127.0.0.1:9/wms")), Some("Local"), None).is_err());
        let missing = json!({"add": {"command": "layer.addData", "params": {"path": "/no/such/file.geojson"}}});
        assert!(add_from(app, &missing, None, Some("Local"), None).is_err());
        assert_eq!(app.session.sources.recent, before);
    }

    #[test]
    fn selected_datasets_are_each_named_after_their_own_connection() {
        let mut h = harness();
        let app = h.state_mut();
        let (one, two) = ("http://127.0.0.1:9/one", "http://127.0.0.1:9/two");
        let mut ids = Vec::new();
        for (name, url) in [("One", one), ("Two", two)] {
            ids.push(
                app.session.run("connection.new", &json!({"kind": "wms", "address": url, "name": name})).unwrap()["id"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            );
        }
        for (id, url) in ids.iter().zip([one, two]) {
            let pick = Pick { conn: id.clone(), key: "a".into(), name: "A".into(), child: layer("a"), source: Some(Arc::new(wms(url))) };
            app.ui.connection_tree.picks.push(pick);
            app.ui.connection_tree.seen.insert(id.clone(), app.session.connections.find(id).unwrap().clone());
        }
        add_picks(app);
        assert!(app.ui.connection_tree.picks.is_empty());
        assert_eq!(app.session.sources.recent.first(), Some(&expected(two, "a", "Two")));
        assert_eq!(app.session.sources.recent.get(1), Some(&expected(one, "a", "One")));
        let added = &app.session.map().tiles;
        for (id, url) in ids.iter().zip([one, two]) {
            let tile = added.iter().find(|tile| tile.url.starts_with(url)).unwrap();
            assert_eq!(&tile.connection, id);
        }
    }

    /// A download lands after the add returned: its entry is named for the connection it came
    /// from, whatever happened to the connection meanwhile, and a download that fails has none.
    #[test]
    fn a_delayed_download_is_named_when_it_lands_and_a_failed_one_is_not() {
        let _turn = crate::fetching::turn();
        let mut h = harness();
        let base = serve(|head| {
            if head.contains("/missing") {
                (404, b"no".to_vec())
            } else {
                (200, br#"{"type": "Feature", "properties": {}, "geometry": {"type": "Point", "coordinates": [1, 2]}}"#.to_vec())
            }
        });
        let (good, bad) = (format!("{base}/points.geojson"), format!("{base}/missing.geojson"));
        let url_add = |address: &str| json!({"add": {"command": "layer.addUrl", "params": {"url": address, "name": "Points"}}});
        let app = h.state_mut();
        let id =
            app.session.run("connection.new", &json!({"kind": "wms", "address": format!("{base}/wms"), "name": "Local"})).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_string();
        let before = app.session.sources.recent.clone();
        add_from(app, &url_add(&good), None, Some("Local"), Some(&id)).unwrap();
        add_from(app, &url_add(&bad), None, Some("Local"), Some(&id)).unwrap();
        assert_eq!(app.session.sources.recent, before, "nothing is recorded before the data lands");
        // The connection is renamed and the selection changes before it lands.
        app.session.run("connection.edit", &json!({"id": id, "name": "Renamed"})).unwrap();
        app.ui.connection_tree.picks.clear();
        until(&mut h, |app| {
            app.session.sources.recent.iter().any(|s| s.address == good) && app.ui.downloads.is_empty() && app.ui.cog_probes.is_empty()
        });
        let recent = &h.state().session.sources.recent;
        let landed = recent.iter().find(|s| s.address == good).unwrap();
        assert_eq!((landed.kind.as_str(), landed.name.as_str()), ("url", "Local"));
        assert!(recent.iter().all(|s| s.address != bad), "a failed download isn't recorded");
        assert_eq!(h.state().session.map().layer_by_name("Points").unwrap().connection, id);
    }

    #[test]
    fn a_service_dialog_download_keeps_the_browsed_connection_not_the_current_selection() {
        let _turn = crate::fetching::turn();
        let base = serve(|_| (200, br#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}"#.to_vec()));
        let mut h = harness();
        let app = h.state_mut();
        let url = format!("{base}/wms");
        let one = app.run("connection.new", json!({"kind":"wms", "name":"One", "address":url})).unwrap();
        let two = app.run("connection.new", json!({"kind":"wms", "name":"Two", "address":url})).unwrap();
        app.run("ui.addService", json!({"url":url, "type":"wms", "connection":one["id"]})).unwrap();
        app.dialogs.connection = two["id"].as_str().map(str::to_string);
        let src = wms(&url);
        let feature = json!({"name":"points", "offer":{"type":"features", "url":format!("{base}/points.geojson")}});
        serviceui::add_layer(app, &src, &feature, "", None, true).unwrap();
        until(&mut h, |app| app.session.map().layer_by_name("points").is_some());
        assert_eq!(h.state().session.map().layer_by_name("points").unwrap().connection, one["id"].as_str().unwrap());
    }

    #[test]
    fn url_downloads_use_the_explicit_basic_or_header_connection_and_record_it() {
        let _turn = crate::fetching::turn();
        for basic in [false, true] {
            let base = serve(move |head| {
                let head = head.to_ascii_lowercase();
                let authorized = if basic {
                    head.contains("authorization: basic cmvhzgvyonbhc3n3b3jk")
                } else {
                    head.contains("x-api-key: ui-download-key")
                };
                if authorized {
                    (200, br#"{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}"#.to_vec())
                } else {
                    (401, b"Login required".to_vec())
                }
            });
            let mut h = harness();
            let app = h.state_mut();
            let c = app
                .run(
                    "connection.new",
                    json!({
                        "kind":"service", "address":base, "auth":if basic { "basic" } else { "header" },
                        "user":if basic { "reader" } else { "" }, "keyName":if basic { "" } else { "X-Api-Key" }
                    }),
                )
                .unwrap();
            let mut login = json!({"id":c["id"], "allowInsecure":true});
            login[if basic { "password" } else { "apiKey" }] = json!(if basic { "password" } else { "ui-download-key" });
            app.run("connection.login", login).unwrap();
            app.run("ui.addUrl", json!({"url":format!("{base}/points.geojson"), "name":"Private points", "connection":c["id"]})).unwrap();
            until(&mut h, |app| app.ui.downloads.is_empty());
            let layer = h.state().session.map().layer_by_name("Private points").unwrap();
            assert_eq!(layer.connection, c["id"].as_str().unwrap());
        }
    }

    #[test]
    fn a_listing_whose_params_are_not_an_object_cannot_panic_the_add() {
        let mut h = harness();
        let app = h.state_mut();
        for params in [json!("text"), json!(7), json!([1]), Value::Null] {
            let child = json!({"add": {"command": "map.zoomIn", "params": params}});
            let _ = add_from(app, &child, None, Some("Local"), Some("c1"));
        }
    }

    /// A plugin's anonymous group remains anonymous when the user refreshes it, including new
    /// children and every discovery/download phase after saving and reopening the project.
    #[test]
    fn anonymous_service_groups_refresh_without_logins_before_and_after_reopen() {
        use rustgis_engine::render::rustgis_tiles::logins;
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        let _turn = crate::fetching::turn();
        for basic in [false, true] {
            let revision = Arc::new(AtomicUsize::new(0));
            let requests = Arc::new(Mutex::new(Vec::<String>::new()));
            let (served_revision, seen) = (revision.clone(), requests.clone());
            let base = serve(move |head| {
                seen.lock().unwrap().push(head.to_string());
                let path = head.split_whitespace().nth(1).unwrap_or("").split('?').next().unwrap_or("");
                let revision = served_revision.load(Ordering::SeqCst);
                let body = match path {
                    "/api" => json!({"links":[{"rel":"data","type":"application/json","href":"/collections"}]}),
                    "/collections" => {
                        let collections = ["roads", "buildings"]
                            .into_iter()
                            .take(if revision == 0 { 1 } else { 2 })
                            .map(|id| {
                                json!({"id":id,"title":id,"links":[
                                    {"rel":"items","type":"application/geo+json","href":format!("/{id}/items")}
                                ]})
                            })
                            .collect::<Vec<_>>();
                        json!({"collections":collections})
                    }
                    "/roads/items" | "/buildings/items" => json!({
                        "type":"FeatureCollection",
                        "features":(0..=revision).map(|id| json!({
                            "type":"Feature","id":id,
                            "geometry":{"type":"Point","coordinates":[1,2]},
                            "properties":{"revision":revision}
                        })).collect::<Vec<_>>()
                    }),
                    _ => return (404, b"Unknown test path".to_vec()),
                };
                (200, serde_json::to_vec(&body).unwrap())
            });
            let url = format!("{base}/api");
            let mut h = harness();
            let app = h.state_mut();
            app.run("ui.language", json!({"code":"en"})).unwrap();
            app.run("project.new", json!(null)).unwrap();
            let connection = app
                .run(
                    "connection.new",
                    json!({
                        "kind":"ogcapi","address":url,"auth":if basic { "basic" } else { "header" },
                        "user":if basic { "reader" } else { "" },"keyName":if basic { "" } else { "X-Api-Key" }
                    }),
                )
                .unwrap();
            let mut login = json!({"id":connection["id"],"allowInsecure":true});
            login[if basic { "password" } else { "apiKey" }] = json!("anonymous-group-secret-757");
            app.run("connection.login", login.clone()).unwrap();
            let authenticated = rustgis_engine::service_logins::request(&mut app.session, None, &url).unwrap();
            assert!(logins::apply(&authenticated).unwrap().is_some(), "the same origin has a selected session login");
            // This is the persisted marker produced for a plugin-selected service, without
            // keeping an anonymous engine context active while the GUI downloads its documents.
            app.run("ui.importService", json!({"url":url,"type":"ogcapi","name":"Anonymous plugin group","anonymous":true})).unwrap();
            until(&mut h, |app| app.ui.service_import.is_none());
            assert!(h.state().dialogs.error.is_none(), "{:?}", h.state().dialogs.error);
            let group = h.state().session.map().service_groups[0].id;
            assert!(h.state().session.map().service_groups[0].anonymous);
            let roads = h.state().session.map().layer_by_name("roads").unwrap().id;
            assert_eq!(h.state().session.map().layer(roads).unwrap().dataset.features.len(), 1);
            let dir = tempfile::tempdir().unwrap();
            let project = dir.path().join("anonymous-service.rgis");
            for step in 1..=3 {
                if step == 2 {
                    h.state_mut().run("project.saveAs", json!({"path":project.display().to_string()})).unwrap();
                    h.state_mut().run("project.open", json!({"path":project.display().to_string()})).unwrap();
                    h.state_mut().run("connection.login", login.clone()).unwrap();
                    assert!(h.state().session.map().service_groups[0].anonymous);
                }
                revision.store(step, Ordering::SeqCst);
                let before = requests.lock().unwrap().len();
                for _ in 0..4 {
                    h.step();
                }
                // Refresh the newly discovered child on its own too: it cannot silently adopt
                // the selected connection when it later becomes the user's refresh target.
                h.get_by_label(if step == 3 { "buildings" } else { "Anonymous plugin group" }).click_secondary();
                h.step();
                h.step();
                h.get_by_label("Refresh").click();
                h.step();
                until(&mut h, |app| app.ui.service_import.is_none());
                assert!(h.state().dialogs.error.is_none(), "{:?}", h.state().dialogs.error);
                let app = h.state();
                let saved = &app.session.map().service_groups[0];
                assert_eq!(saved.id, group);
                assert!(saved.anonymous);
                assert_eq!(saved.children.len(), 2, "refresh discovers a new anonymous child");
                assert_eq!(app.session.map().layer_by_name("roads").unwrap().id, roads);
                for name in ["roads", "buildings"] {
                    let layer = app.session.map().layer_by_name(name).unwrap();
                    assert!(layer.anonymous, "{name} must keep anonymous provenance");
                    assert!(layer.connection.is_empty(), "{name} cannot record the user's connection");
                    let expected = if step == 3 && name == "roads" { 3 } else { step + 1 };
                    assert_eq!(layer.dataset.features.len(), expected, "{name} data is updated");
                    assert!(logins::apply(layer.dataset.source.as_deref().unwrap()).unwrap().is_none());
                }
                let requests = requests.lock().unwrap();
                let paths = requests[before..]
                    .iter()
                    .map(|head| head.split_whitespace().nth(1).unwrap().split('?').next().unwrap())
                    .collect::<Vec<_>>();
                let mut expected = vec!["/api", "/collections"];
                if step != 3 {
                    expected.push("/roads/items");
                }
                expected.push("/buildings/items");
                assert_eq!(paths, expected, "capabilities, linked collections and all features are fetched");
            }
            for head in requests.lock().unwrap().iter() {
                let lower = head.to_ascii_lowercase();
                assert!(!lower.contains("authorization:"), "Basic login leaked: {head}");
                assert!(!lower.contains("x-api-key:"), "header login leaked: {head}");
                assert!(!head.contains("anonymous-group-secret-757"));
            }
        }
    }

    #[test]
    fn a_foreign_followup_does_not_adopt_that_origins_saved_login() {
        let _turn = crate::fetching::turn();
        let mut app = RustGisApp::with_sample();
        let foreign = "https://foreign-ui-followup.invalid/points.geojson";
        let c = app
            .run(
                "connection.new",
                json!({
                    "kind":"service", "address":foreign, "auth":"header", "keyName":"X-Api-Key"
                }),
            )
            .unwrap();
        app.run("connection.login", json!({"id":c["id"], "apiKey":"foreign-followup-key"})).unwrap();
        let address = serviceui::request_from(&mut app, None, "https://original-ui-followup.invalid/service", foreign).unwrap();
        assert_eq!(address, foreign);
        assert!(rustgis_engine::render::rustgis_tiles::logins::apply(&address).unwrap().is_none());
    }

    #[test]
    fn a_tileset_step_names_its_layer_and_a_refused_one_is_not_recorded() {
        let _turn = crate::fetching::turn();
        let mut h = harness();
        let api = "https://x.org/collections";
        let tiled = r#"{"collections": [{"id": "relief", "links": [{"rel": "tilesets-map", "href": "https://x.org/relief/map/tiles"}]}]}"#;
        let opened = h.state().session.opened;
        let step = |fallback: &str, origin: Option<&str>| {
            let slot = Slot::default();
            *slot.lock().unwrap() = Some(Err("unreachable".to_string()));
            let params = json!({"url": api, "layer": "relief (map tiles)", "text": tiled, "name": "Relief"});
            let resolve = crate::TilesResolve {
                params,
                list: "l".into(),
                doc: "l".into(),
                crossed_origin: false,
                read: 0,
                withheld_addresses: 0,
                vector: false,
                fallback: fallback.into(),
                slot,
            };
            crate::ServiceStep::Tiles { resolve, dialog: false, origin: origin.map(str::to_string), opened }
        };
        let before = h.state().session.sources.recent.clone();
        h.state_mut().ui.service_steps.push(step("http://127.0.0.1/{z}/{x}/{y}", Some("Atlas")));
        h.step();
        assert_eq!(h.state().session.sources.recent, before, "a refused template adds nothing");
        h.state_mut().ui.service_steps.push(step("https://x.org/relief/map/tiles/smerc/{z}/{y}/{x}.png", Some("Atlas")));
        h.step();
        let noted = h.state().session.sources.recent.first().unwrap().clone();
        assert_eq!((noted.kind.as_str(), noted.address.as_str(), noted.name.as_str()), ("service", api, "Atlas"));
        assert_eq!(noted.extra["layer"], "relief (map tiles)");
        // Added from a dialog, it keeps the name the address gives.
        h.state_mut().ui.service_steps.push(step("https://x.org/relief/map/tiles/smerc/{z}/{y}/{x}.png", None));
        h.step();
        assert_eq!(h.state().session.sources.recent.first().unwrap().name, sources::default_name(api));
    }

    /// A WCS coverage through a local server: its description is read, then the GeoTIFF is
    /// downloaded; Recent names the coverage's source after the connection once it lands.
    #[test]
    fn a_coverage_is_named_once_its_download_lands() {
        let _turn = crate::fetching::turn();
        use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
        let r = Raster::new(
            "soc",
            8,
            8,
            vec![vec![3.0; 64]],
            GeoTransform { origin_x: 5.0, origin_y: 50.0, pixel_width: 0.1, pixel_height: -0.1 },
            Crs::LonLat,
            None,
            ColorHint::Gray,
            "Float32",
        )
        .unwrap();
        let tiff = geotiff::write(&r, 0).unwrap();
        let caps = r#"<wcs:Capabilities version="2.0.1" xmlns:wcs="http://www.opengis.net/wcs/2.0" xmlns:ows="http://www.opengis.net/ows/2.0">
        <ows:ServiceIdentification><ows:Title>Soils</ows:Title></ows:ServiceIdentification>
        <wcs:Contents><wcs:CoverageSummary><wcs:CoverageId>soc</wcs:CoverageId><ows:Title>Soil carbon</ows:Title>
          <ows:WGS84BoundingBox><ows:LowerCorner>5 49.2</ows:LowerCorner><ows:UpperCorner>5.8 50</ows:UpperCorner></ows:WGS84BoundingBox>
        </wcs:CoverageSummary></wcs:Contents></wcs:Capabilities>"#;
        let describe = r#"<CoverageDescriptions><CoverageDescription><Envelope srsName="http://www.opengis.net/def/crs/EPSG/0/4326" axisLabels="Lat Long">
        <lowerCorner>49.2 5</lowerCorner><upperCorner>50 5.8</upperCorner></Envelope></CoverageDescription></CoverageDescriptions>"#;
        let base = serve(move |head| {
            if head.contains("GetCoverage") {
                (200, tiff.clone())
            } else if head.contains("DescribeCoverage") {
                (200, describe.as_bytes().to_vec())
            } else {
                (200, caps.as_bytes().to_vec())
            }
        });
        let url = format!("{base}/wcs");
        let mut h = harness();
        let connection = h.state_mut().run("connection.new", json!({"kind":"wcs", "address":url, "name":"Soils"})).unwrap();
        let id = connection["id"].as_str().unwrap().to_string();
        let app = h.state_mut();
        app.run("map.zoomToExtent", json!({"extent": [5.0, 49.2, 5.8, 50.0]})).unwrap();
        let listed = app.session.run("service.layers", &json!({"url": url, "text": caps})).unwrap();
        let source = ServiceSource { url: url.clone(), kind: "wcs", text: caps.into(), documents: Vec::new() };
        let child = json!({"add": {"command": "service.add", "params": {}}, "layer": listed["layers"][0].clone()});
        let before = app.session.sources.recent.clone();
        add_from(app, &child, Some(&source), Some("Soils"), Some(&id)).unwrap();
        assert_eq!(app.session.sources.recent, before, "nothing is recorded while the description is read");
        until(&mut h, |app| {
            app.ui.service_steps.is_empty()
                && app.ui.downloads.is_empty()
                && app.ui.cog_probes.is_empty()
                && app.session.sources.recent.first().is_some_and(|s| s.name == "Soils")
        });
        let noted = h.state().session.sources.recent.first().unwrap();
        assert_eq!(noted.kind, "url");
        assert!(noted.address.starts_with(&url) && noted.address.contains("GetCoverage"), "{}", noted.address);
        assert_eq!(h.state().session.map().layer_by_name("Soil carbon").unwrap().connection, id);
    }

    /// A signing response cannot turn an old click into an addition to a replacement project.
    #[test]
    fn deferred_asset_signing_cannot_add_to_a_replacement_project() {
        use rustgis_engine::render::rustgis_tiles::cache;
        use std::sync::Mutex;
        let _turn = crate::fetching::turn();
        for (case, (replace, ready)) in
            [(None, true), (Some(false), false), (Some(false), true), (Some(true), false), (Some(true), true)].into_iter().enumerate()
        {
            let href = format!("https://review29{case}.blob.core.windows.net/old-assets/points.csv");
            let token_url = stac::sas_token_url(&href).unwrap();
            let (asset, token) = (href.clone(), token_url.clone());
            let sent = Arc::new(Mutex::new(Vec::new()));
            let held = Arc::new(Mutex::new(Vec::<cache::Done>::new()));
            let (seen, callbacks) = (sent.clone(), held.clone());
            cache::set_fetcher(move |url, done| {
                let plain = cache::split_download_limit(&url).0;
                // Earlier frames' workers can still reach the process-wide fetcher.
                if plain != token && plain.split('?').next() != Some(asset.as_str()) {
                    done(Err("outside the signing fixture".into()));
                    return;
                }
                seen.lock().unwrap().push(plain.to_string());
                callbacks.lock().unwrap().push(done);
            });
            let mut app = RustGisApp::with_sample();
            let (text, _) = app.session.project.saved_text(None).unwrap();
            add_asset(&mut app, &href, "Old asset", None, None).unwrap();
            assert_eq!(app.ui.connection_tree.tokens.len(), 1);
            assert_eq!(sent.lock().unwrap().as_slice(), std::slice::from_ref(&token_url));
            let finish = || {
                for done in std::mem::take(&mut *held.lock().unwrap()) {
                    done(Ok(br#"{"token":"sig=review29-synthetic"}"#.to_vec()));
                }
            };
            if ready {
                finish();
            }
            match replace {
                Some(true) => app.open_peer_text(&text).unwrap(),
                Some(false) => {
                    app.session.run("project.openText", &json!({"text":text})).unwrap();
                }
                None => {}
            }
            let mark = app.session.notice_mark();
            poll(&mut app, &egui::Context::default());
            let drained = app.ui.connection_tree.tokens.is_empty();
            if !ready {
                finish();
            }
            poll(&mut app, &egui::Context::default());
            for done in std::mem::take(&mut *held.lock().unwrap()) {
                done(Err("test download finished".into()));
            }
            assert!(drained && app.ui.connection_tree.tokens.is_empty(), "old signing must retire before inspecting its slot");
            if replace.is_some() {
                assert_eq!(sent.lock().unwrap().as_slice(), std::slice::from_ref(&token_url), "the old asset must not be requested");
                assert!(app.ui.downloads.is_empty() && app.ui.cog_probes.is_empty());
                assert!(stac::cached_token(&token_url).is_none(), "an old response must not update the token cache");
                assert_eq!(app.session.notices_since(mark).iter().filter(|n| n.text.contains("project was closed")).count(), 1);
            } else {
                assert_eq!(
                    *sent.lock().unwrap(),
                    [token_url.clone(), stac::sign(&href, "sig=review29-synthetic")],
                    "a current signing response starts exactly its asset download"
                );
                assert_eq!(app.ui.downloads.len(), 1);
                assert!(stac::cached_token(&token_url).is_some());
            }
        }
    }

    /// The whole path on screen: open a connection in the Data Sources pane and double-click a
    /// layer. Recent then lists it under the connection's name with the source as added, and
    /// clicking that entry opens the service browser on the same address.
    #[test]
    fn recent_in_the_pane_shows_the_connection_and_reopens_its_service() {
        let _turn = crate::fetching::turn();
        let base = serve(|_| (200, WMS.as_bytes().to_vec()));
        let url = format!("{base}/wms");
        let mut h = harness();
        let id = h.state_mut().run("connection.new", json!({"kind": "wms", "address": url, "name": "Local"})).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        h.state_mut().run("ui.pane.sources", json!({})).unwrap();
        h.step();
        h.get_by_label("Local").click();
        until(&mut h, |app| matches!(app.ui.connection_tree.entries.get(&(id.clone(), String::new())), Some(Entry::Ready(_))));
        // (egui counts clicks in a row: let the connection's click age out of its window first.)
        for _ in 0..14 {
            h.step();
        }
        let layers = h.state().session.map().tiles.len();
        // A double-click adds it: the first click selects, the second adds.
        h.get_by_label("A").click();
        h.step();
        h.get_by_label("A").click();
        h.step();
        h.step();
        assert_eq!(h.state().session.map().tiles.len(), layers + 1);
        let noted = h.state().session.sources.recent.first().unwrap().clone();
        assert_eq!((noted.kind.as_str(), noted.address.as_str(), noted.name.as_str()), ("service", url.as_str(), "Local"));
        assert_eq!(noted.extra, json!({"layer": "a", "type": "wms"}));

        // A rename leaves the entry as it was.
        h.state_mut().run("connection.edit", json!({"id": id, "name": "Renamed"})).unwrap();
        h.step();
        assert_eq!(h.state().session.sources.recent.first().map(|s| s.name.as_str()), Some("Local"));

        // Recent is listed above Connections: its row is the first labelled "Local".
        h.state_mut().ui.service = None;
        h.query_all_by_label("Local").next().unwrap().click();
        h.step();
        assert_eq!(h.state().ui.service.as_ref().map(|c| c.url.as_str()), Some(url.as_str()));
    }
}
