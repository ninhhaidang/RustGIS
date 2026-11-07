//! ModelBuilder: models drawn as graphs, in a view of their own beside the maps and layouts.
//!
//! A tool palette on the left (drag a tool onto the canvas, or click it), the canvas in the
//! middle (input layers, tools and outputs joined by wires drawn from an output port to an input
//! port; drag the background to pan, Ctrl+wheel or pinch to zoom), the selected node's settings on
//! the right and the messages of the last check or run below. The graph itself lives in the engine
//! (`rustgis_engine::model`), which validates, lays out and runs it: the canvas only edits a copy
//! and saves it with `model.save`.

use std::collections::HashMap;

use egui::{Align2, Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};
use rustgis_engine::analysis::{ParamKind, ToolSpec};
use rustgis_engine::data::GeometryType;
use rustgis_engine::model::{self, IN, OUT, PortKind};
use rustgis_engine::project::{ModelEdge, ModelGraph, ModelNode, ModelNodeKind};
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::icons;
use crate::theme::{Tokens, regular, semibold};

/// A node's width on the canvas; its height depends on its ports ([`node_size`]).
const NODE_W: f32 = 168.0;
const HEADER: f32 = 42.0;
const PORT_ROW: f32 = 19.0;
const PORT_R: f32 = 5.5;
const PALETTE_W: f32 = 230.0;
const INSPECTOR_W: f32 = 270.0;
const LOG_H: f32 = 96.0;
const UNDO_LIMIT: usize = 100;

/// What the palette drags onto the canvas: an input, an output or a tool (by id).
#[derive(Clone, Debug, PartialEq)]
enum Add {
    Input,
    Output,
    Tool(&'static str),
}

/// How a node fared in the last run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    Done,
    Error,
}

/// A log line: plain, a success or an error.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LogKind {
    Info,
    Ok,
    Error,
}

#[derive(Default)]
pub struct ModelBuilder {
    /// The view's tab is in the tab strip.
    pub open: bool,
    /// The view is shown (rather than a map or a layout).
    pub active: bool,
    /// The map and layout in view when ModelBuilder was shown: another one coming into view
    /// (a command, the ribbon) hides it.
    view_key: (usize, Option<usize>),
    /// The model's name in the name box.
    pub name: String,
    /// The project model the canvas was opened from or last saved as.
    pub saved_as: Option<String>,
    /// That model came in a file: its tools may not touch files (the engine checks).
    pub imported: bool,
    pub graph: ModelGraph,
    /// The graph as last opened or saved, to tell unsaved changes.
    saved: ModelGraph,
    pub selected: Option<String>,
    selected_edge: Option<usize>,
    /// Canvas offset (in graph units) and scale.
    pan: Vec2,
    zoom: f32,
    /// Fit the graph into the canvas on the next frame.
    fit: bool,
    /// The node whose output port a wire is being dragged from.
    wiring: Option<String>,
    undo: Vec<ModelGraph>,
    redo: Vec<ModelGraph>,
    pub search: String,
    pub log: Vec<(String, LogKind)>,
    pub status: HashMap<String, Status>,
    /// The graphs of the runs under way and waiting, in the order asked, with the project each
    /// was asked in, until each lands: another one on the canvas by then (edited, or another
    /// model opened) isn't marked with what it did, and one from another project leaves it be.
    running: std::collections::VecDeque<(u64, ModelGraph)>,
    /// The log shows Running… for a run still to land: another graph's landing may say how it
    /// ended in its place, but once a run's ending is shown it stays.
    waiting: bool,
    /// What to do once the user agrees to drop unsaved changes.
    confirm: Option<Pending>,
    /// The problems of the graph, kept until it or the project changes (the project's
    /// revision): checked every frame otherwise.
    problems: Option<(u64, ModelGraph, Vec<model::Problem>)>,
    /// `Session::project_starts` when the canvas was set up: another project drops it.
    project: u64,
}

/// An action waiting for "Discard unsaved changes?".
#[derive(Clone, Debug)]
enum Pending {
    New,
    Open(String),
    Close,
}

impl ModelBuilder {
    /// Whether ModelBuilder is in view.
    pub fn shown(&self) -> bool {
        self.open && self.active
    }

    /// Whether the canvas has changes not saved to the project.
    pub fn dirty(&self) -> bool {
        self.graph != self.saved || self.saved_as.as_deref().is_some_and(|s| s != self.name.trim())
    }

    /// Records the graph before an edit, for undo.
    fn edit(&mut self) {
        self.undo.push(self.graph.clone());
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.status.clear();
    }

    pub fn undo(&mut self) {
        if let Some(g) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.graph, g));
            self.forget_missing();
        }
    }

    pub fn redo(&mut self) {
        if let Some(g) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.graph, g));
            self.forget_missing();
        }
    }

    /// Drops a selection the graph no longer has.
    fn forget_missing(&mut self) {
        if self.selected.as_ref().is_some_and(|id| !self.graph.nodes.iter().any(|n| &n.id == id)) {
            self.selected = None;
        }
        if self.selected_edge.is_some_and(|i| i >= self.graph.edges.len()) {
            self.selected_edge = None;
        }
    }

    /// Starts a model on the canvas: empty, or a saved one.
    fn load(&mut self, name: &str, saved_as: Option<String>, graph: ModelGraph, imported: bool) {
        self.name = name.to_string();
        self.saved_as = saved_as;
        self.imported = imported;
        self.saved = graph.clone();
        self.graph = graph;
        self.selected = None;
        self.selected_edge = None;
        self.undo.clear();
        self.redo.clear();
        self.status.clear();
        self.log.clear();
        self.wiring = None;
        self.fit = true;
    }

    /// Deletes the selected node (with its wires) or wire.
    pub fn delete_selected(&mut self) -> bool {
        if let Some(id) = self.selected.clone() {
            self.edit();
            self.graph.nodes.retain(|n| n.id != id);
            self.graph.edges.retain(|e| e.from != id && e.to != id);
            self.selected = None;
            return true;
        }
        if let Some(i) = self.selected_edge.take().filter(|i| *i < self.graph.edges.len()) {
            self.edit();
            self.graph.edges.remove(i);
            return true;
        }
        false
    }

    /// Copies the selected node beside it (without its wires) and selects the copy.
    pub fn duplicate_selected(&mut self) -> bool {
        let Some(n) = self.selected.as_ref().and_then(|id| self.graph.nodes.iter().find(|n| &n.id == id)).cloned() else {
            return false;
        };
        self.edit();
        let id = model::new_id(&self.graph, prefix(n.kind));
        let copy = ModelNode { id: id.clone(), x: n.x + 24.0, y: n.y + 24.0, ..n };
        self.graph.nodes.push(copy);
        self.selected = Some(id);
        true
    }

    /// Adds a node at `at` (graph units, its top-left corner), clear of the others.
    fn add(&mut self, what: &Add, at: Pos2) -> String {
        self.edit();
        let kind = match what {
            Add::Input => ModelNodeKind::Input,
            Add::Output => ModelNodeKind::Output,
            Add::Tool(_) => ModelNodeKind::Tool,
        };
        let id = model::new_id(&self.graph, prefix(kind));
        let mut node = ModelNode { id: id.clone(), kind, x: at.x, y: at.y, ..Default::default() };
        if let Add::Tool(t) = what {
            node.tool = Some(t.to_string());
            node.params = json!({});
        }
        // Pushed down until it doesn't cover another node.
        let size = node_size(&node);
        for _ in 0..200 {
            let r = Rect::from_min_size(pos2(node.x, node.y), size).expand(8.0);
            if !self.graph.nodes.iter().any(|n| node_rect(n).intersects(r)) {
                break;
            }
            node.y += 24.0;
        }
        self.graph.nodes.push(node);
        self.selected = Some(id.clone());
        self.selected_edge = None;
        id
    }

    /// Connects `from`'s output to `to`'s input port `port`, replacing what that port had (unless
    /// it takes several). Refused when it would make a loop.
    fn connect(&mut self, from: &str, to: &str, port: &str, many: bool) -> Result<(), &'static str> {
        if from == to {
            return Err("A node cannot connect to itself.");
        }
        let edge = ModelEdge { from: from.into(), from_port: OUT.into(), to: to.into(), to_port: port.into() };
        if self.graph.edges.contains(&edge) {
            return Ok(());
        }
        let mut g = self.graph.clone();
        if !many {
            g.edges.retain(|e| !(e.to == to && e.to_port == port));
        }
        g.edges.push(edge);
        if model::topological_order(&g).is_none() {
            return Err("That connection would create a loop.");
        }
        self.edit();
        self.graph = g;
        Ok(())
    }

    fn log(&mut self, text: impl Into<String>, kind: LogKind) {
        self.log.push((text.into(), kind));
        if self.log.len() > 200 {
            self.log.remove(0);
        }
    }
}

fn prefix(kind: ModelNodeKind) -> &'static str {
    match kind {
        ModelNodeKind::Input => "input",
        ModelNodeKind::Tool => "tool",
        ModelNodeKind::Output => "output",
    }
}

/// A node's size in graph units.
fn node_size(n: &ModelNode) -> Vec2 {
    match n.kind {
        ModelNodeKind::Tool => vec2(NODE_W, (HEADER + model::input_ports(n).len() as f32 * PORT_ROW + 8.0).max(60.0)),
        _ => vec2(NODE_W, 50.0),
    }
}

fn node_rect(n: &ModelNode) -> Rect {
    Rect::from_min_size(pos2(n.x, n.y), node_size(n))
}

/// Where a node's input port sits (graph units).
fn in_port_pos(n: &ModelNode, k: usize) -> Pos2 {
    match n.kind {
        ModelNodeKind::Tool => pos2(n.x, n.y + HEADER + k as f32 * PORT_ROW + PORT_ROW / 2.0),
        _ => pos2(n.x, n.y + node_size(n).y / 2.0),
    }
}

fn out_port_pos(n: &ModelNode) -> Pos2 {
    pos2(n.x + NODE_W, n.y + node_size(n).y / 2.0)
}

/// The colour of a kind of node: inputs blue, tools amber, outputs green (as models are
/// usually drawn).
fn kind_color(kind: ModelNodeKind, t: &Tokens) -> Color32 {
    match (kind, t.dark) {
        (ModelNodeKind::Input, false) => Color32::from_rgb(0x2F, 0x6F, 0xD6),
        (ModelNodeKind::Input, true) => Color32::from_rgb(0x6A, 0x9D, 0xF0),
        (ModelNodeKind::Tool, false) => Color32::from_rgb(0xC7, 0x8A, 0x10),
        (ModelNodeKind::Tool, true) => Color32::from_rgb(0xE6, 0xB0, 0x45),
        (ModelNodeKind::Output, false) => Color32::from_rgb(0x2E, 0x8B, 0x57),
        (ModelNodeKind::Output, true) => Color32::from_rgb(0x5C, 0xC0, 0x86),
    }
}

/// `ui.modelBuilder`: `{"name"?: model}` opens ModelBuilder (on that model); `{"close": true}`
/// closes it, asking first about unsaved changes.
pub fn command(app: &mut RustGisApp, p: &Value) -> Result<(), String> {
    if p.get("close").and_then(Value::as_bool) == Some(true) {
        if app.mb.open {
            request(app, Pending::Close);
        }
        return Ok(());
    }
    open(app, p.get("name").and_then(Value::as_str))
}

/// Opens ModelBuilder: on the model named (asking first if the canvas has unsaved changes), or
/// as it was left.
pub fn open(app: &mut RustGisApp, name: Option<&str>) -> Result<(), String> {
    forget_old_project(app);
    if let Some(n) = name {
        if !app.session.project.models.iter().any(|m| m.name.eq_ignore_ascii_case(n)) {
            return Err(crate::i18n::fmt("no model '{name}'", &[("name", n)]));
        }
        if app.mb.open && app.mb.dirty() && !app.mb.saved_as.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(n)) {
            app.mb.confirm = Some(Pending::Open(n.to_string()));
        } else if !(app.mb.open && app.mb.saved_as.as_deref().is_some_and(|s| s.eq_ignore_ascii_case(n))) {
            load_saved(app, n)?;
        }
    } else if !app.mb.open {
        let name = untitled(app);
        app.mb.load(&name, None, ModelGraph::default(), false);
    }
    app.mb.open = true;
    show_view(app);
    Ok(())
}

/// Brings ModelBuilder into view.
fn show_view(app: &mut RustGisApp) {
    app.mb.active = true;
    app.mb.view_key = (app.session.project.active_map, app.session.active_layout);
    app.ws.catalog_view = false;
}

/// A name no model has yet.
fn untitled(app: &RustGisApp) -> String {
    let models = &app.session.project.models;
    let mut name = "Model".to_string();
    let mut i = 1;
    while models.iter().any(|m| m.name.eq_ignore_ascii_case(&name)) && i < 10_000 {
        i += 1;
        name = format!("Model {i}");
    }
    name
}

/// Loads a saved model only when its graph can be shown without truncating its steps.
fn load_saved(app: &mut RustGisApp, name: &str) -> Result<(), String> {
    let Some(m) = app.session.project.models.iter().find(|m| m.name.eq_ignore_ascii_case(name)) else {
        return Err(crate::i18n::fmt("no model '{name}'", &[("name", name)]));
    };
    let graph = match &m.graph {
        Some(graph) => graph.clone(),
        None => model::checked_steps_to_graph(&m.steps)?,
    };
    let imported = !app.session.item_trusted("model", &m.name);
    let n = m.name.clone();
    app.mb.load(&n, Some(n.clone()), graph, imported);
    Ok(())
}

/// Saves the canvas as a project model.
fn save(app: &mut RustGisApp) -> bool {
    let name = app.mb.name.trim().to_string();
    let name = if name.is_empty() { untitled(app) } else { name };
    let mut p = json!({"name": app.mb.saved_as.clone().unwrap_or_else(|| name.clone()), "graph": app.mb.graph});
    if app.mb.saved_as.as_deref().is_some_and(|s| s != name) {
        p["rename"] = json!(name);
    }
    // A model that came in a file stays one: saving it doesn't let its tools touch files.
    p["imported"] = json!(app.mb.imported);
    match app.run("model.save", p) {
        Ok(v) => {
            let saved = v["model"].as_str().unwrap_or(&name).to_string();
            app.mb.name = saved.clone();
            app.mb.saved_as = Some(saved.clone());
            app.mb.saved = app.mb.graph.clone();
            app.mb.log(crate::i18n::fmt("Saved {name}.", &[("name", &saved)]), LogKind::Ok);
            true
        }
        Err(e) => {
            app.mb.log(e, LogKind::Error);
            false
        }
    }
}

/// Runs the model on the canvas: on the desktop its tools on a thread of their own ([`readjob`]),
/// the status bar showing it and Cancel; [`landed`] puts what it did in the canvas once it lands.
fn run_model(app: &mut RustGisApp) {
    app.mb.status.clear();
    app.mb.log.clear();
    let mut p = json!({"name": app.mb.name.trim(), "graph": app.mb.graph});
    // A model from a file runs as saved, so the engine holds its tools to it.
    if app.mb.imported
        && let Some(saved) = app.mb.saved_as.clone()
    {
        if app.mb.graph != app.mb.saved && !save(app) {
            return;
        }
        p = json!({"name": saved});
    }
    app.mb.running.push_back((app.session.project_starts, app.mb.graph.clone()));
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = crate::readjob::run_then(app, "model.run", p, Some(landed));
        // Not landed yet: it says so until it does.
        if app.mb.log.is_empty() {
            app.mb.waiting = true;
            app.mb.log(tl!("Running…").to_string(), LogKind::Info);
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let r = app.run("model.run", p);
        landed(app, crate::Landed::Ran(r));
    }
}

/// Whether `a` and `b` run alike: the same nodes and edges, wherever the nodes sit on the canvas
/// (a node moved while its run is under way still gets its marks).
fn same_run(a: &ModelGraph, b: &ModelGraph) -> bool {
    let placed = |g: &ModelGraph| {
        let mut g = g.clone();
        for n in &mut g.nodes {
            (n.x, n.y) = (0.0, 0.0);
        }
        g
    };
    placed(a) == placed(b)
}

/// A model's run in the canvas: node statuses and messages from the engine's report, when it ran
/// on the graph still there; why it didn't run, when it didn't.
fn landed(app: &mut RustGisApp, landed: crate::Landed) {
    let Some((project, graph)) = app.mb.running.pop_front() else { return };
    // Asked in a project closed since: this canvas isn't the one it ran from.
    if project != app.session.project_starts {
        return;
    }
    let same = same_run(&graph, &app.mb.graph);
    // Another graph's run leaves the canvas as it is while a run of it is still to land (its
    // Running… stays), or once a run's ending is shown there.
    if !same && (!app.mb.running.is_empty() || !app.mb.waiting) {
        return;
    }
    app.mb.waiting = false;
    app.mb.status.clear();
    app.mb.log.clear();
    let res = match landed {
        crate::Landed::Ran(res) => res,
        crate::Landed::NotRun(why) => {
            app.mb.log(crate::i18n::fmt("Run failed: {error}", &[("error", &why)]), LogKind::Error);
            return;
        }
    };
    // Its report, when the run made one (one refused before it ran, a model removed meanwhile,
    // leaves the last run's); and only on the graph it ran: for another, only how it ended.
    let report = app.session.model_run.clone().filter(|r| same && (res.is_ok() || r.error.as_ref() == res.as_ref().err()));
    let report = report.unwrap_or_default();
    let titles: HashMap<String, String> = app.mb.graph.nodes.iter().map(|n| (n.id.clone(), model::title(n))).collect();
    for n in &report.nodes {
        let title = titles.get(&n.node).cloned().unwrap_or_default();
        match n.status {
            "error" => {
                app.mb.status.insert(n.node.clone(), Status::Error);
            }
            _ => {
                app.mb.status.insert(n.node.clone(), Status::Done);
                let tool = app.mb.graph.nodes.iter().any(|m| m.id == n.node && m.kind == ModelNodeKind::Tool);
                if tool {
                    let line = match &n.layer {
                        Some(l) => crate::i18n::fmt("{tool} made {layer}", &[("tool", &title), ("layer", l)]),
                        None => crate::i18n::fmt("{tool} ran", &[("tool", &title)]),
                    };
                    app.mb.log(line, LogKind::Info);
                }
            }
        }
    }
    match res {
        Ok(v) => {
            let outputs: Vec<String> =
                v["outputs"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
            let line = if outputs.is_empty() {
                tl!("Run finished.").to_string()
            } else {
                crate::i18n::fmt("Run finished. Layers kept: {names}.", &[("names", &outputs.join(", "))])
            };
            app.mb.log(line, LogKind::Ok);
        }
        Err(e) => app.mb.log(crate::i18n::fmt("Run failed: {error}", &[("error", &e)]), LogKind::Error),
    }
    app.map.invalidate();
}

/// The model's file: downloaded in the browser, else saved where the user picks.
fn export(app: &mut RustGisApp) {
    match app.run("model.export", json!({"name": app.mb.name.trim(), "graph": app.mb.graph})) {
        Ok(v) => {
            let name = v["fileName"].as_str().unwrap_or("model.model.json").to_string();
            let bytes = v["text"].as_str().unwrap_or("").as_bytes().to_vec();
            if app.services.download.is_some() {
                app.download_bytes(&name, bytes);
            } else if let Some(path) = crate::dialogs::pick_save(&name, &["json"], tl!("RustGIS model")) {
                if let Err(e) = rustgis_engine::data::io::write_atomic(std::path::Path::new(&path), &bytes) {
                    app.mb.log(e.to_string(), LogKind::Error);
                } else {
                    app.mb.log(crate::i18n::fmt("Exported to {path}.", &[("path", &path)]), LogKind::Ok);
                }
            }
        }
        Err(e) => app.mb.log(e, LogKind::Error),
    }
}

/// Imports a model file (`*.model.json`, or a web tool) and opens it.
pub(crate) fn import(app: &mut RustGisApp) {
    if let Some(open) = &app.services.open_async {
        app.ui.pending_open = Some("model.import");
        open("item");
    } else if let Some(path) = crate::dialogs::pick_open(&["json", "rgistool"], tl!("RustGIS model")) {
        match app.run("model.import", json!({"path": path})) {
            Ok(v) => imported(app, &v),
            Err(e) => app.mb.log(e, LogKind::Error),
        }
    }
}

/// Opens a model `model.import` just added.
pub fn imported(app: &mut RustGisApp, v: &Value) {
    if let Some(name) = v["model"].as_str() {
        if let Err(error) = load_saved(app, name) {
            app.session.notify(error, true);
            return;
        }
        app.mb.open = true;
        show_view(app);
    }
}

/// Does what waited for the user to agree to drop unsaved changes.
fn proceed(app: &mut RustGisApp, p: Pending) {
    match p {
        Pending::New => {
            let name = untitled(app);
            app.mb.load(&name, None, ModelGraph::default(), false);
        }
        Pending::Open(n) => {
            if let Err(error) = load_saved(app, &n) {
                app.session.notify(error, true);
            }
        }
        Pending::Close => {
            app.mb.open = false;
            app.mb.active = false;
            app.mb.saved = app.mb.graph.clone();
        }
    }
}

fn request(app: &mut RustGisApp, p: Pending) {
    if app.mb.dirty() && !app.mb.graph.nodes.is_empty() {
        app.mb.confirm = Some(p);
    } else {
        proceed(app, p);
    }
}

/// ModelBuilder's keys, before the app's own: Delete (or Backspace) deletes the selection,
/// Ctrl+D duplicates the selected node, Ctrl+Z and Ctrl+Y undo and redo canvas edits.
pub fn shortcuts(app: &mut RustGisApp, ctx: &egui::Context) {
    if !app.mb.shown() || ctx.egui_wants_keyboard_input() {
        return;
    }
    let cmd = egui::Modifiers::COMMAND;
    let (mut del, mut dup, mut undo, mut redo) = (false, false, false, false);
    ctx.input_mut(|i| {
        undo = i.consume_shortcut(&egui::KeyboardShortcut::new(cmd, egui::Key::Z));
        redo = i.consume_shortcut(&egui::KeyboardShortcut::new(cmd | egui::Modifiers::SHIFT, egui::Key::Z))
            || i.consume_shortcut(&egui::KeyboardShortcut::new(cmd, egui::Key::Y));
        if app.mb.selected.is_some() || app.mb.selected_edge.is_some() {
            del = i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace);
        }
        if app.mb.selected.is_some() {
            dup = i.consume_shortcut(&egui::KeyboardShortcut::new(cmd, egui::Key::D));
        }
    });
    if undo {
        app.mb.undo();
    }
    if redo {
        app.mb.redo();
    }
    if del {
        app.mb.delete_selected();
    }
    if dup {
        app.mb.duplicate_selected();
    }
}

/// The view: toolbar, palette, canvas, inspector and messages.
pub fn show(app: &mut RustGisApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let full = ui.available_rect_before_wrap();
    ui.painter().rect_filled(full, 0.0, t.pane);
    let bar = Rect::from_min_size(full.min, vec2(full.width(), 40.0));
    let body = Rect::from_min_max(pos2(full.min.x, bar.max.y), full.max);
    let narrow = body.width() < 760.0;
    let palette_w = if narrow { 0.0 } else { PALETTE_W };
    let inspector_w = if narrow { 0.0 } else { INSPECTOR_W.min(body.width() * 0.35) };
    let palette = Rect::from_min_max(body.min, pos2(body.min.x + palette_w, body.max.y));
    let inspector = Rect::from_min_max(pos2(body.max.x - inspector_w, body.min.y), body.max);
    let middle = Rect::from_min_max(pos2(palette.max.x, body.min.y), pos2(inspector.min.x, body.max.y));
    let log = Rect::from_min_max(pos2(middle.min.x, middle.max.y - LOG_H), middle.max);
    let canvas = Rect::from_min_max(middle.min, pos2(middle.max.x, log.min.y));

    let fresh = matches!(&app.mb.problems, Some((rev, g, _)) if *rev == app.session.revision && *g == app.mb.graph);
    if !fresh {
        let p = model::validate(&app.session, &app.mb.graph);
        app.mb.problems = Some((app.session.revision, app.mb.graph.clone(), p));
    }
    let problems = app.mb.problems.as_ref().map(|c| c.2.clone()).unwrap_or_default();
    toolbar(app, &mut ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(10.0, 6.0)))), &t, &problems);
    let line = Stroke::new(1.0, t.border);
    ui.painter().line_segment([pos2(full.min.x, bar.max.y), pos2(full.max.x, bar.max.y)], line);
    if palette_w > 0.0 {
        ui.painter().line_segment([pos2(palette.max.x, palette.min.y), pos2(palette.max.x, palette.max.y)], line);
        palette_ui(app, &mut ui.new_child(egui::UiBuilder::new().max_rect(palette.shrink(10.0))), &t, canvas);
    }
    if inspector_w > 0.0 {
        ui.painter().line_segment([pos2(inspector.min.x, inspector.min.y), pos2(inspector.min.x, inspector.max.y)], line);
        inspector_ui(app, &mut ui.new_child(egui::UiBuilder::new().max_rect(inspector.shrink(10.0))), &t, &problems);
    }
    canvas_ui(app, ui, canvas, &t, &problems);
    ui.painter().line_segment([pos2(log.min.x, log.min.y), pos2(log.max.x, log.min.y)], line);
    log_ui(app, &mut ui.new_child(egui::UiBuilder::new().max_rect(log.shrink2(vec2(10.0, 6.0)))), &t, &problems);
    ui.allocate_rect(full, Sense::hover());
    confirm(app, ui.ctx());
}

fn toolbar(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, problems: &[model::Problem]) {
    ui.horizontal_centered(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
        icons::paint(ui.painter(), r, "modelbuilder", t.icon, t.accent);
        ui.add(egui::TextEdit::singleline(&mut app.mb.name).hint_text(tl!("Untitled model")).desired_width(170.0).font(semibold(13.0)));
        if app.mb.dirty() {
            ui.label(egui::RichText::new("●").color(t.amber)).on_hover_text(tl!("Unsaved changes"));
        }
        ui.separator();
        if ui.button(tl!("New")).on_hover_text(tl!("Start an empty model")).clicked() {
            request(app, Pending::New);
        }
        let models: Vec<String> = app.session.project.models.iter().map(|m| m.name.clone()).collect();
        ui.add_enabled_ui(!models.is_empty(), |ui| {
            ui.menu_button(tl!("Open"), |ui| {
                for m in &models {
                    if ui.button(m).clicked() {
                        if app.mb.saved_as.as_deref() != Some(m) {
                            request(app, Pending::Open(m.clone()));
                        }
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text(tl!("Open a model of this project"));
        });
        if ui.button(tl!("Save")).on_hover_text(tl!("Save the model in the project")).clicked() {
            save(app);
        }
        ui.separator();
        let can_run = problems.is_empty();
        let run = ui.add_enabled(can_run, egui::Button::new(egui::RichText::new(format!("▶ {}", tl!("Run"))).strong()));
        let run = if can_run {
            run.on_hover_text(tl!("Run the model"))
        } else {
            run.on_disabled_hover_text(tl!("Fix the problems listed below first"))
        };
        if run.clicked() {
            run_model(app);
        }
        ui.separator();
        if ui.button(tl!("Arrange")).on_hover_text(tl!("Lay the nodes out along the flow")).clicked() && !app.mb.graph.nodes.is_empty() {
            app.mb.edit();
            model::layout(&mut app.mb.graph);
            app.mb.fit = true;
        }
        if ui.button(tl!("Fit")).on_hover_text(tl!("Show the whole model")).clicked() {
            app.mb.fit = true;
        }
        ui.add_enabled_ui(!app.mb.undo.is_empty(), |ui| {
            if crate::widgets::icon_button(ui, "undo", tl!("Undo (Ctrl+Z)")).clicked() {
                app.mb.undo();
            }
        });
        ui.add_enabled_ui(!app.mb.redo.is_empty(), |ui| {
            if crate::widgets::icon_button(ui, "redo", tl!("Redo (Ctrl+Y)")).clicked() {
                app.mb.redo();
            }
        });
        ui.separator();
        if ui.button(tl!("Import…")).on_hover_text(tl!("Open a model file (*.model.json)")).clicked() {
            import(app);
        }
        ui.add_enabled_ui(!app.mb.graph.nodes.is_empty(), |ui| {
            if ui.button(tl!("Export…")).on_hover_text(tl!("Save the model as a file (*.model.json)")).clicked() {
                export(app);
            }
            if ui
                .button(tl!("Copy Command"))
                .on_hover_text(tl!("Copy a model.run command that runs this model, for the Script pane or Python"))
                .clicked()
            {
                let name = serde_json::to_string(app.mb.name.trim()).unwrap_or_default();
                // A model from a file runs by name, as saved, so the engine holds it to its rules.
                let text = match (&app.mb.saved_as, app.mb.imported) {
                    (Some(saved), true) => format!("model.run {}", json!({"name": saved})),
                    _ => format!("model.run {{\"name\": {name}, \"graph\": {}}}", serde_json::to_string(&app.mb.graph).unwrap_or_default()),
                };
                ui.ctx().copy_text(text);
                app.mb.log(tl!("Copied the command."), LogKind::Ok);
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::widgets::icon_button(ui, "close", tl!("Close ModelBuilder")).clicked() {
                request(app, Pending::Close);
            }
            if crate::widgets::icon_button(ui, "map", tl!("Back to the map")).clicked() {
                app.mb.active = false;
            }
        });
    });
}

/// The tools, by toolbox, to drag onto the canvas (or click to add).
fn palette_ui(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, canvas: Rect) {
    crate::widgets::search_field(ui, &mut app.mb.search, tl!("Search tools"));
    ui.add_space(4.0);
    let mut add: Option<Add> = None;
    ui.horizontal(|ui| {
        for (what, label, tip) in [
            (Add::Input, tl!("+ Input"), tl!("A layer the model reads")),
            (Add::Output, tl!("+ Output"), tl!("A result the model keeps, under a name")),
        ] {
            let resp = ui.add(egui::Button::new(label).sense(Sense::click_and_drag()));
            drag_source(ui, &resp, &what, label);
            if resp.on_hover_text(tip).clicked() {
                add = Some(what);
            }
        }
    });
    ui.add_space(4.0);
    ui.separator();
    let q = app.mb.search.trim().to_lowercase();
    let matches = |x: &ToolSpec| {
        q.is_empty()
            || x.name.to_lowercase().contains(&q)
            || tl!(x.name).to_lowercase().contains(&q)
            || x.id.to_lowercase().contains(&q)
            || x.toolbox.to_lowercase().contains(&q)
    };
    let mut groups: Vec<(String, bool, Vec<&'static ToolSpec>)> = Vec::new();
    let mut push = |group: String, open: bool, tool: &'static ToolSpec| match groups.iter_mut().find(|g| g.0 == group) {
        Some(g) => g.2.push(tool),
        None => groups.push((group, open, vec![tool])),
    };
    for tool in rustgis_engine::analysis::TOOLS.iter().filter(|x| matches(x)) {
        push(crate::i18n::location(tool.toolbox.split(" › ").next().unwrap_or(tool.toolbox)).to_string(), true, tool);
    }
    for tool in rustgis_engine::analysis::external_tools_of(rustgis_engine::plugins::tools::SOURCE).iter().filter(|x| matches(x)) {
        push(format!("{} › {}", tl!("Plugins"), tool.toolbox.rsplit(" › ").next().unwrap_or(tool.toolbox)), true, tool);
    }
    let external = rustgis_engine::cmd::gp::geolibre_tools();
    for (k, tool) in external.iter().filter(|x| matches(x)).enumerate() {
        // A thousand tools: by category, closed until opened, and only the first matches.
        if !q.is_empty() && k >= 80 {
            break;
        }
        let top = tool.toolbox.split(" › ").nth(1).unwrap_or("Other");
        push(format!("GeoLibre › {}", crate::i18n::location(top)), !q.is_empty(), tool);
    }
    egui::ScrollArea::vertical().id_salt("mb_palette").auto_shrink([false, false]).show(ui, |ui| {
        if groups.is_empty() {
            ui.label(egui::RichText::new(tl!("No tools match your search.")).size(11.5).color(t.text_dim));
        }
        for (group, open, tools) in groups {
            egui::CollapsingHeader::new(egui::RichText::new(format!("{group} ({})", tools.len())).size(11.5).strong())
                .id_salt(("mb_group", &group, q.is_empty()))
                .default_open(open)
                .show(ui, |ui| {
                    for tool in tools {
                        let resp = {
                            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click_and_drag());
                            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!(tool.name)));
                            if resp.hovered() {
                                ui.painter().rect_filled(r, CornerRadius::same(5), t.hover);
                            }
                            icons::paint(
                                ui.painter(),
                                Rect::from_center_size(pos2(r.min.x + 12.0, r.center().y), vec2(16.0, 16.0)),
                                crate::panes::tool_icon(tool.id),
                                t.icon,
                                t.accent,
                            );
                            let name: String = tl!(tool.name).chars().take(30).collect();
                            ui.painter().text(pos2(r.min.x + 26.0, r.center().y), Align2::LEFT_CENTER, name, regular(12.0), t.text);
                            resp
                        };
                        drag_source(ui, &resp, &Add::Tool(tool.id), tl!(tool.name));
                        let resp = resp.on_hover_text(format!("{}\n{}", tl!(tool.name), tl!(tool.summary)));
                        if resp.clicked() {
                            add = Some(Add::Tool(tool.id));
                        }
                    }
                });
        }
    });
    if let Some(what) = add {
        // Clicked: to the right of the last node, or at the top left of the view.
        let zoom = app.mb.zoom.max(0.1);
        let view_min = (vec2(20.0, 20.0) / zoom - app.mb.pan).to_pos2();
        let at = match app.mb.graph.nodes.iter().max_by(|a, b| a.x.total_cmp(&b.x)) {
            Some(last) if last.x + 2.0 * model::COLUMN < view_min.x + canvas.width() / zoom => pos2(last.x + model::COLUMN, last.y),
            _ => view_min,
        };
        app.mb.add(&what, at);
    }
}

/// Makes a palette item draggable onto the canvas: the payload set when its drag starts, and
/// its name following the pointer meanwhile.
fn drag_source(ui: &Ui, resp: &egui::Response, what: &Add, label: &str) {
    if resp.drag_started() {
        egui::DragAndDrop::set_payload(ui.ctx(), what.clone());
    }
    if resp.dragged()
        && let Some(p) = ui.ctx().pointer_latest_pos()
    {
        let t = Tokens::get(ui.ctx());
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("mb_drag")));
        let galley = painter.layout_no_wrap(label.to_string(), semibold(12.0), t.text);
        let r = Rect::from_min_size(p + vec2(12.0, 8.0), galley.size() + vec2(16.0, 10.0));
        painter.rect(r, CornerRadius::same(6), t.pane, Stroke::new(1.5, kind_color(ModelNodeKind::Tool, &t)), egui::StrokeKind::Inside);
        painter.galley(r.min + vec2(8.0, 5.0), galley, t.text);
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
}

/// From graph units to the screen and back.
#[derive(Clone, Copy)]
struct View {
    origin: Pos2,
    pan: Vec2,
    zoom: f32,
}

impl View {
    fn to_screen(self, p: Pos2) -> Pos2 {
        self.origin + (p.to_vec2() + self.pan) * self.zoom
    }
    fn to_graph(self, p: Pos2) -> Pos2 {
        ((p - self.origin) / self.zoom - self.pan).to_pos2()
    }
    fn rect(self, r: Rect) -> Rect {
        Rect::from_min_max(self.to_screen(r.min), self.to_screen(r.max))
    }
}

fn bezier(a: Pos2, b: Pos2, zoom: f32) -> [Pos2; 4] {
    let dx = ((b.x - a.x).abs() / 2.0).max(40.0 * zoom);
    [a, pos2(a.x + dx, a.y), pos2(b.x - dx, b.y), b]
}

/// The distance from `p` to a wire.
fn wire_distance(pts: [Pos2; 4], p: Pos2) -> f32 {
    let at = |t: f32| {
        let u = 1.0 - t;
        let v = pts[0].to_vec2() * u * u * u
            + pts[1].to_vec2() * 3.0 * u * u * t
            + pts[2].to_vec2() * 3.0 * u * t * t
            + pts[3].to_vec2() * t * t * t;
        v.to_pos2()
    };
    (0..=32).map(|i| at(i as f32 / 32.0).distance(p)).fold(f32::INFINITY, f32::min)
}

fn canvas_ui(app: &mut RustGisApp, ui: &mut Ui, rect: Rect, t: &Tokens, problems: &[model::Problem]) {
    if app.mb.zoom <= 0.0 {
        app.mb.zoom = 1.0;
    }
    if app.mb.fit {
        app.mb.fit = false;
        fit(&mut app.mb, rect);
    }
    let bg = ui.interact(rect, egui::Id::unique("mb_canvas"), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, t.canvas);
    // Zoom (Ctrl+wheel, pinch) about the pointer; the wheel pans.
    if bg.contains_pointer() || ui.rect_contains_pointer(rect) {
        let (zoom, scroll, pointer) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos()));
        if zoom != 1.0
            && let Some(p) = pointer
        {
            let view = View { origin: rect.min, pan: app.mb.pan, zoom: app.mb.zoom };
            let at = view.to_graph(p);
            app.mb.zoom = (app.mb.zoom * zoom).clamp(0.25, 2.5);
            app.mb.pan = (p - rect.min) / app.mb.zoom - at.to_vec2();
        } else if scroll != Vec2::ZERO {
            app.mb.pan += scroll / app.mb.zoom;
        }
    }
    let view = View { origin: rect.min, pan: app.mb.pan, zoom: app.mb.zoom };
    // A dotted grid.
    let step = 24.0 * view.zoom;
    if step > 6.0 {
        let start =
            view.to_screen(pos2((view.to_graph(rect.min).x / 24.0).floor() * 24.0, (view.to_graph(rect.min).y / 24.0).floor() * 24.0));
        let dot = if t.dark { Color32::from_white_alpha(18) } else { Color32::from_black_alpha(22) };
        let mut y = start.y;
        while y < rect.max.y {
            let mut x = start.x;
            while x < rect.max.x {
                painter.circle_filled(pos2(x, y), 1.0, dot);
                x += step;
            }
            y += step;
        }
    }
    if app.mb.graph.nodes.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            tl!("Drag a tool from the palette to start building, or add an input layer."),
            regular(13.0),
            t.text_dim,
        );
    }
    let z = view.zoom;
    let node_of = |g: &ModelGraph, id: &str| g.nodes.iter().position(|n| n.id == id);

    // Wires, under the nodes.
    let pointer = ui.input(|i| i.pointer.hover_pos());
    let mut hovered_edge = None;
    for (i, e) in app.mb.graph.edges.iter().enumerate() {
        let (Some(a), Some(b)) = (node_of(&app.mb.graph, &e.from), node_of(&app.mb.graph, &e.to)) else { continue };
        let (Some(na), Some(nb)) = (app.mb.graph.nodes.get(a), app.mb.graph.nodes.get(b)) else { continue };
        let Some(k) = model::input_ports(nb).iter().position(|p| p.id == e.to_port) else { continue };
        let pts = bezier(view.to_screen(out_port_pos(na)), view.to_screen(in_port_pos(nb, k)), z);
        let near = pointer.is_some_and(|p| bg.hovered() && wire_distance(pts, p) < 6.0);
        if near {
            hovered_edge = Some(i);
        }
        let selected = app.mb.selected_edge == Some(i);
        let color = if selected {
            t.accent
        } else if near {
            t.text
        } else {
            t.text_dim
        };
        let width = if selected || near { 2.6 } else { 1.8 };
        painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
            pts,
            false,
            Color32::TRANSPARENT,
            Stroke::new(width * z.max(0.6), color),
        ));
        // An arrowhead at the port.
        let tip = pts[3];
        let s = 6.0 * z;
        painter.add(egui::Shape::convex_polygon(
            vec![
                pos2(tip.x - PORT_R * z, tip.y),
                pos2(tip.x - PORT_R * z - s, tip.y - s * 0.6),
                pos2(tip.x - PORT_R * z - s, tip.y + s * 0.6),
            ],
            color,
            Stroke::NONE,
        ));
    }

    // Nodes.
    let titles: Vec<String> = app.mb.graph.nodes.iter().map(model::title).collect();
    let mut drag_moved = false;
    let mut clicked_node = None;
    let mut drag_started = false;
    let mut menu_action: Option<(String, &'static str)> = None;
    for i in 0..app.mb.graph.nodes.len() {
        let Some(n) = app.mb.graph.nodes.get(i).cloned() else { continue };
        let r = view.rect(node_rect(&n));
        if !r.intersects(rect.expand(50.0)) {
            continue;
        }
        let resp = ui.interact(r.intersect(rect), egui::Id::unique(("mb_node", &n.id)), Sense::click_and_drag());
        if resp.drag_started() {
            drag_started = true;
            clicked_node = Some(n.id.clone());
        }
        if resp.dragged() {
            let d = resp.drag_delta() / z;
            if let Some(m) = app.mb.graph.nodes.get_mut(i) {
                m.x += d.x;
                m.y += d.y;
            }
            drag_moved = true;
        }
        if resp.clicked() {
            clicked_node = Some(n.id.clone());
        }
        resp.context_menu(|ui| {
            if n.kind == ModelNodeKind::Tool && ui.button(tl!("Keep Result")).clicked() {
                menu_action = Some((n.id.clone(), "keep"));
                ui.close();
            }
            if ui.button(tl!("Duplicate")).clicked() {
                menu_action = Some((n.id.clone(), "duplicate"));
                ui.close();
            }
            if ui.button(tl!("Delete")).clicked() {
                menu_action = Some((n.id.clone(), "delete"));
                ui.close();
            }
        });
        let problem = problems.iter().any(|p| p.node.as_deref() == Some(n.id.as_str()));
        let status = app.mb.status.get(&n.id).copied();
        let selected = app.mb.selected.as_deref() == Some(n.id.as_str());
        paint_node(&painter, t, &n, titles.get(i).map(String::as_str).unwrap_or(""), r, z, selected, problem, status, resp.hovered());
    }
    if drag_started {
        // The position before the drag, for undo.
        let mut before = app.mb.graph.clone();
        if let Some(id) = &clicked_node
            && let Some(n) = before.nodes.iter_mut().find(|n| &n.id == id)
            && let Some(m) = app.mb.graph.nodes.iter().find(|m| &m.id == id)
        {
            n.x = m.x;
            n.y = m.y;
        }
        app.mb.edit();
        if let Some(last) = app.mb.undo.last_mut() {
            *last = before;
        }
    }
    let _ = drag_moved;
    if let Some(id) = clicked_node {
        app.mb.selected = Some(id);
        app.mb.selected_edge = None;
    }
    if let Some((id, action)) = menu_action {
        app.mb.selected = Some(id.clone());
        match action {
            "keep" => keep_result(&mut app.mb, &id),
            "duplicate" => {
                app.mb.duplicate_selected();
            }
            _ => {
                app.mb.delete_selected();
            }
        }
    }

    // Ports, over the nodes: drag from an output to an input to wire them.
    let mut drop_target: Option<(String, &'static str, bool)> = None;
    let pointer_now = ui.input(|i| i.pointer.interact_pos());
    let wiring = app.mb.wiring.clone();
    for n in app.mb.graph.nodes.clone() {
        let ins = model::input_ports(&n);
        for (k, port) in ins.iter().enumerate() {
            let p = view.to_screen(in_port_pos(&n, k));
            let wired = app.mb.graph.edges.iter().any(|e| e.to == n.id && e.to_port == port.id);
            let hot = wiring.as_ref().is_some_and(|w| w != &n.id) && pointer_now.is_some_and(|q| q.distance(p) < 14.0 * z.max(0.7));
            if hot {
                drop_target = Some((n.id.clone(), port.id, port.many));
            }
            let fill = if wired { kind_color(n.kind, t) } else { t.pane };
            let ring = if port.required && !wired { t.red } else { kind_color(n.kind, t) };
            painter.circle(p, PORT_R * z * if hot { 1.5 } else { 1.0 }, fill, Stroke::new(1.5, ring));
        }
        if model::output_ports(&n).is_empty() {
            continue;
        }
        let p = view.to_screen(out_port_pos(&n));
        let pr = Rect::from_center_size(p, Vec2::splat(18.0 * z.max(0.7)));
        let resp = ui.interact(pr, egui::Id::unique(("mb_out", &n.id)), Sense::drag()).on_hover_text(tl!("Drag to an input to connect"));
        if resp.drag_started() {
            app.mb.wiring = Some(n.id.clone());
        }
        let c = kind_color(n.kind, t);
        painter.circle(p, PORT_R * z * if resp.hovered() { 1.4 } else { 1.0 }, c, Stroke::new(1.5, t.pane));
    }
    // The wire being drawn.
    if let Some(from) = app.mb.wiring.clone() {
        if let (Some(n), Some(q)) = (app.mb.graph.nodes.iter().find(|n| n.id == from), pointer_now) {
            let a = view.to_screen(out_port_pos(n));
            let pts = bezier(a, q, z);
            painter.add(egui::epaint::CubicBezierShape::from_points_stroke(pts, false, Color32::TRANSPARENT, Stroke::new(2.0, t.accent)));
        }
        if ui.input(|i| i.pointer.any_released()) {
            app.mb.wiring = None;
            match drop_target {
                Some((to, port, many)) => {
                    if let Err(e) = app.mb.connect(&from, &to, port, many) {
                        app.mb.log(tl!(e), LogKind::Error);
                    }
                }
                None => {
                    // Dropped on an output-less spot: on a node, its first free input.
                    let hit = pointer_now.and_then(|q| {
                        app.mb.graph.nodes.iter().rev().find(|n| n.id != from && view.rect(node_rect(n)).contains(q)).cloned()
                    });
                    if let Some(n) = hit {
                        let ports = model::input_ports(&n);
                        let free =
                            ports.iter().find(|p| !app.mb.graph.edges.iter().any(|e| e.to == n.id && e.to_port == p.id)).or(ports.first());
                        if let Some(p) = free {
                            let (id, many) = (p.id, p.many);
                            if let Err(e) = app.mb.connect(&from, &n.id, id, many) {
                                app.mb.log(tl!(e), LogKind::Error);
                            }
                        }
                    }
                }
            }
        }
        ui.ctx().request_repaint();
    }

    // The background: drag to pan, click to select a wire or clear the selection.
    if bg.dragged() && app.mb.wiring.is_none() {
        app.mb.pan += bg.drag_delta() / z;
    }
    if bg.clicked() {
        app.mb.selected = None;
        app.mb.selected_edge = hovered_edge;
    }
    bg.context_menu(|ui| {
        if let Some(i) = hovered_edge.or(app.mb.selected_edge)
            && ui.button(tl!("Remove Connection")).clicked()
        {
            app.mb.selected = None;
            app.mb.selected_edge = Some(i);
            app.mb.delete_selected();
            ui.close();
        }
        if ui.button(tl!("Add Input")).clicked() {
            let at = ui.ctx().input(|i| i.pointer.latest_pos()).map(|p| view.to_graph(p)).unwrap_or(pos2(40.0, 40.0));
            app.mb.add(&Add::Input, at);
            ui.close();
        }
        if ui.button(tl!("Add Output")).clicked() {
            let at = ui.ctx().input(|i| i.pointer.latest_pos()).map(|p| view.to_graph(p)).unwrap_or(pos2(40.0, 40.0));
            app.mb.add(&Add::Output, at);
            ui.close();
        }
    });
    if hovered_edge.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    // A tool dropped from the palette, centred under the pointer.
    if let Some(what) = bg.dnd_release_payload::<Add>()
        && let Some(p) = ui.input(|i| i.pointer.interact_pos())
    {
        let at = view.to_graph(p) - vec2(NODE_W / 2.0, 25.0);
        app.mb.add(&what, at);
    }
    // Zoom buttons, bottom right.
    let zr = Rect::from_min_size(pos2(rect.max.x - 112.0, rect.max.y - 36.0), vec2(104.0, 28.0));
    let mut zui = ui.new_child(egui::UiBuilder::new().max_rect(zr));
    zui.horizontal(|ui| {
        if ui.small_button("−").on_hover_text(tl!("Zoom out")).clicked() {
            zoom_about(&mut app.mb, rect, 1.0 / 1.25);
        }
        ui.label(egui::RichText::new(format!("{:.0}%", app.mb.zoom * 100.0)).size(11.0).color(t.text_dim));
        if ui.small_button("+").on_hover_text(tl!("Zoom in")).clicked() {
            zoom_about(&mut app.mb, rect, 1.25);
        }
    });
}

fn zoom_about(mb: &mut ModelBuilder, rect: Rect, f: f32) {
    let view = View { origin: rect.min, pan: mb.pan, zoom: mb.zoom };
    let c = view.to_graph(rect.center());
    mb.zoom = (mb.zoom * f).clamp(0.25, 2.5);
    mb.pan = (rect.center() - rect.min) / mb.zoom - c.to_vec2();
}

/// Fits the whole graph into the canvas (no closer than 100%).
fn fit(mb: &mut ModelBuilder, rect: Rect) {
    let Some(bounds) = mb.graph.nodes.iter().map(node_rect).reduce(|a, b| a.union(b)) else {
        mb.zoom = 1.0;
        mb.pan = Vec2::ZERO;
        return;
    };
    let bounds = bounds.expand(30.0);
    let zoom = (rect.width() / bounds.width()).min(rect.height() / bounds.height()).clamp(0.25, 1.0);
    mb.zoom = zoom;
    mb.pan = (rect.size() / zoom - bounds.size()) / 2.0 - bounds.min.to_vec2();
}

#[allow(clippy::too_many_arguments)]
fn paint_node(
    painter: &egui::Painter,
    t: &Tokens,
    n: &ModelNode,
    title: &str,
    r: Rect,
    z: f32,
    selected: bool,
    problem: bool,
    status: Option<Status>,
    hovered: bool,
) {
    let color = kind_color(n.kind, t);
    let round = match n.kind {
        ModelNodeKind::Tool => CornerRadius::same((6.0 * z).round().clamp(1.0, 255.0) as u8),
        _ => CornerRadius::same((r.height() / 2.0).round().clamp(1.0, 255.0) as u8),
    };
    // A soft shadow, then the card.
    painter.rect_filled(r.translate(vec2(0.0, 2.0 * z)), round, t.shadow);
    let fill = t.pane.lerp_to_gamma(color, if t.dark { 0.16 } else { 0.10 });
    let (stroke_c, stroke_w) = match (status, problem, selected) {
        (Some(Status::Error), ..) => (t.red, 2.4),
        (_, _, true) => (t.accent, 2.4),
        (_, true, _) => (t.red, 1.6),
        (Some(Status::Done), ..) => (t.green, 2.0),
        _ if hovered => (color, 1.8),
        _ => (color.gamma_multiply(0.75), 1.2),
    };
    if selected {
        painter.rect_stroke(r.expand(3.0), round, Stroke::new(3.0, t.accent.gamma_multiply(0.25)), egui::StrokeKind::Outside);
    }
    painter.rect(r, round, fill, Stroke::new(stroke_w, stroke_c), egui::StrokeKind::Inside);
    if n.kind == ModelNodeKind::Tool {
        // The tool's band.
        let band = Rect::from_min_max(r.min, pos2(r.min.x + 4.0 * z, r.max.y));
        painter.rect_filled(band, CornerRadius { nw: round.nw, sw: round.sw, ne: 0, se: 0 }, color);
    }
    if z < 0.4 {
        return;
    }
    let pad = if n.kind == ModelNodeKind::Tool { 12.0 } else { 18.0 } * z;
    let kind = match n.kind {
        ModelNodeKind::Input => tl!("INPUT").to_string(),
        ModelNodeKind::Output => tl!("OUTPUT").to_string(),
        ModelNodeKind::Tool => model::tool_of(n)
            .map(|s| crate::i18n::location(s.toolbox.split(" › ").last().unwrap_or("")).to_uppercase())
            .unwrap_or_else(|| tl!("UNKNOWN TOOL").to_string()),
    };
    let top = if n.kind == ModelNodeKind::Tool { r.min.y + 13.0 * z } else { r.center().y - 8.0 * z };
    let max_chars = (24.0 * (NODE_W * z - pad - 20.0 * z) / (NODE_W * z - 30.0 * z)).max(6.0) as usize;
    let clip = |s: &str, n: usize| -> String {
        if s.chars().count() > n { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) } else { s.to_string() }
    };
    painter.text(pos2(r.min.x + pad, top), Align2::LEFT_CENTER, clip(&kind, max_chars + 4), regular(9.5 * z), color);
    let shown = if n.kind == ModelNodeKind::Tool { tl!(title).to_string() } else { title.to_string() };
    painter.text(pos2(r.min.x + pad, top + 15.0 * z), Align2::LEFT_CENTER, clip(&shown, max_chars), semibold(12.5 * z), t.text);
    // Status, top right.
    let badge = pos2(r.max.x - 14.0 * z, r.min.y + 14.0 * z);
    match (status, problem) {
        (Some(Status::Done), _) => {
            painter.circle_filled(badge, 7.0 * z, t.green);
            painter.text(badge, Align2::CENTER_CENTER, "✔", regular(9.0 * z), Color32::WHITE);
        }
        (Some(Status::Error), _) | (None, true) => {
            painter.circle_filled(badge, 7.0 * z, t.red);
            painter.text(badge, Align2::CENTER_CENTER, "!", semibold(10.0 * z), Color32::WHITE);
        }
        _ => {}
    }
    if n.kind == ModelNodeKind::Tool && z >= 0.55 {
        for (k, port) in model::input_ports(n).iter().enumerate() {
            let y = r.min.y + (HEADER + k as f32 * PORT_ROW + PORT_ROW / 2.0) * z;
            let label = format!("{}{}", tl!(port.label), if port.required { " *" } else { "" });
            painter.text(pos2(r.min.x + 12.0 * z, y), Align2::LEFT_CENTER, clip(&label, max_chars + 4), regular(10.5 * z), t.text_dim);
        }
    }
}

/// Adds an output for a tool's result, to its right, named after the tool.
fn keep_result(mb: &mut ModelBuilder, id: &str) {
    let Some(n) = mb.graph.nodes.iter().find(|n| n.id == id).cloned() else { return };
    let kept =
        mb.graph.edges.iter().any(|e| e.from == id && mb.graph.nodes.iter().any(|m| m.id == e.to && m.kind == ModelNodeKind::Output));
    if kept {
        return;
    }
    let base = model::title(&n);
    let mut name = base.clone();
    let mut k = 1;
    while mb.graph.nodes.iter().any(|m| m.kind == ModelNodeKind::Output && m.name.as_deref() == Some(name.as_str())) && k < 1000 {
        k += 1;
        name = format!("{base} {k}");
    }
    let out = mb.add(&Add::Output, pos2(n.x + model::COLUMN, n.y));
    if let Some(o) = mb.graph.nodes.iter_mut().find(|m| m.id == out) {
        o.name = Some(name);
    }
    // One edit with the node just added.
    mb.graph.edges.push(ModelEdge { from: id.into(), from_port: OUT.into(), to: out, to_port: IN.into() });
}

/// The selected node's settings; with none selected, the model's problems and saved models.
fn inspector_ui(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, problems: &[model::Problem]) {
    egui::ScrollArea::vertical().id_salt("mb_inspector").auto_shrink([false, false]).show(ui, |ui| {
        let Some(i) = app.mb.selected.as_ref().and_then(|id| app.mb.graph.nodes.iter().position(|n| &n.id == id)) else {
            overview(app, ui, t, problems);
            return;
        };
        let Some(n) = app.mb.graph.nodes.get(i).cloned() else { return };
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
            ui.painter().circle_filled(r.center(), 5.0, kind_color(n.kind, t));
            let title = match n.kind {
                ModelNodeKind::Tool => tl!(&model::title(&n)).to_string(),
                ModelNodeKind::Input => tl!("Input").to_string(),
                ModelNodeKind::Output => tl!("Output").to_string(),
            };
            ui.label(egui::RichText::new(title).font(semibold(14.0)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::widgets::icon_button(ui, "delete", tl!("Delete node")).clicked() {
                    app.mb.delete_selected();
                }
            });
        });
        for p in problems.iter().filter(|p| p.node.as_deref() == Some(n.id.as_str())) {
            ui.label(egui::RichText::new(&p.message).size(11.5).color(t.red));
        }
        ui.add_space(6.0);
        match n.kind {
            ModelNodeKind::Input => input_settings(app, ui, i, &n),
            ModelNodeKind::Output => {
                let input_label = ui.label(egui::RichText::new(tl!("Result name")).size(12.0));
                let mut name = n.name.clone().unwrap_or_default();
                let r = ui
                    .add(egui::TextEdit::singleline(&mut name).hint_text(tl!("Model output")).desired_width(f32::INFINITY))
                    .labelled_by(input_label.id);
                if r.gained_focus() {
                    app.mb.edit();
                }
                if r.changed()
                    && let Some(m) = app.mb.graph.nodes.get_mut(i)
                {
                    m.name = Some(name);
                }
                ui.label(
                    egui::RichText::new(tl!("The result wired into this output is added to the map under this name."))
                        .size(11.0)
                        .color(t.text_dim),
                );
            }
            ModelNodeKind::Tool => tool_settings(app, ui, t, i, &n),
        }
    });
}

fn input_settings(app: &mut RustGisApp, ui: &mut Ui, i: usize, n: &ModelNode) {
    let input_label = ui.label(egui::RichText::new(tl!("Layer")).size(12.0));
    let mut names: Vec<String> = app.session.map().layers.iter().map(|l| l.name.clone()).collect();
    names.extend(app.session.project.tables.iter().map(|t| t.name.clone()));
    let cur = n.layer.clone().unwrap_or_default();
    let shown = if cur.is_empty() { tl!("Choose a layer…").to_string() } else { cur.clone() };
    let mut picked = None;
    egui::ComboBox::from_id_salt(("mb_layer", &n.id))
        .selected_text(shown)
        .width(ui.available_width() - 4.0)
        .show_ui(ui, |ui| {
            for name in names {
                if ui.selectable_label(name == cur, &name).clicked() {
                    picked = Some(name);
                }
            }
        })
        .response
        .labelled_by(input_label.id);
    if let Some(p) = picked.filter(|p| *p != cur) {
        app.mb.edit();
        if let Some(m) = app.mb.graph.nodes.get_mut(i) {
            m.layer = Some(p);
        }
    }
}

/// Resolves the named input independently, so a join's fields come from its join input.
fn source_input(app: &RustGisApp, n: &ModelNode, input: &str) -> Option<String> {
    match app.mb.graph.edges.iter().find(|e| e.to == n.id && e.to_port == input) {
        Some(e) => app.mb.graph.nodes.iter().find(|u| u.id == e.from && u.kind == ModelNodeKind::Input).and_then(|u| u.layer.clone()),
        None => n.params.get(input).and_then(Value::as_str).map(str::to_string),
    }
}

/// Fields of a known model input. Numeric parameters refer strictly to a standalone table ID;
/// a wire takes precedence, and tool-produced schemas remain unknown until execution.
fn input_fields<'a>(app: &'a RustGisApp, n: &ModelNode, input: &str) -> Option<&'a [rustgis_engine::data::Field]> {
    if !app.mb.graph.edges.iter().any(|e| e.to == n.id && e.to_port == input)
        && let Some(id) = n.params.get(input).and_then(Value::as_u64)
    {
        return app.session.project.table(u32::try_from(id).ok()?).map(|t| t.dataset.fields.as_slice());
    }
    let name = source_input(app, n, input)?;
    crate::panes::input_dataset(&app.session, &name).map(|d| d.fields.as_slice())
}

fn tool_settings(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, i: usize, n: &ModelNode) {
    let Some(spec) = model::tool_of(n) else {
        ui.label(
            egui::RichText::new(tl!("RustGIS doesn't have this tool. Install the tools it comes from, or delete the node."))
                .size(11.5)
                .color(t.text_dim),
        );
        return;
    };
    ui.label(egui::RichText::new(tl!(spec.summary)).size(11.5).color(t.text_dim));
    ui.add_space(6.0);

    let mut set: Option<(&'static str, Value)> = None;
    let mut focus = false;
    let mut unwire: Option<&'static str> = None;
    for p in spec.params {
        let label = ui.label(egui::RichText::new(format!("{}{}", tl!(p.label), if p.required { " *" } else { "" })).size(12.0));
        let label_id = label.id;
        if !p.help.is_empty() {
            label.on_hover_text(p.help);
        }
        let cur = n.params.get(p.name).cloned().unwrap_or(Value::Null);
        let text = match &cur {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            v => v.to_string(),
        };
        let default = p.default.trim_matches('"').to_string();
        if let Some((kind, _)) = model::port_kind(p.kind) {
            let from: Vec<String> = app
                .mb
                .graph
                .edges
                .iter()
                .filter(|e| e.to == n.id && e.to_port == p.name)
                .filter_map(|e| app.mb.graph.nodes.iter().find(|u| u.id == e.from).map(model::title))
                .collect();
            if !from.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("← {}", from.join(", "))).size(12.0).color(t.accent_text));
                    if ui.small_button("×").on_hover_text(tl!("Disconnect")).clicked() {
                        unwire = Some(p.name);
                    }
                });
                ui.add_space(4.0);
                continue;
            }
            // Not wired: a layer of the map by name.
            let names: Vec<String> = app
                .session
                .map()
                .layers
                .iter()
                .filter(|l| match (p.kind, kind) {
                    (ParamKind::PolygonLayer, _) => l.geometry_type() == GeometryType::Polygon,
                    (ParamKind::PointLayer, _) => l.geometry_type() == GeometryType::Point,
                    (_, PortKind::Raster) => l.is_raster(),
                    (_, PortKind::Vector) => !l.is_raster(),
                    _ => true,
                })
                .map(|l| l.name.clone())
                .collect();
            let shown = if text.is_empty() { tl!("Connect, or pick a layer").to_string() } else { text.clone() };
            egui::ComboBox::from_id_salt(("mb_p", &n.id, p.name))
                .selected_text(shown)
                .width(ui.available_width() - 4.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(text.is_empty(), "—").clicked() {
                        set = Some((p.name, Value::Null));
                    }
                    for name in names {
                        if ui.selectable_label(name == text, &name).clicked() {
                            set = Some((p.name, json!(name)));
                        }
                    }
                })
                .response
                .on_hover_text(tl!(p.label));
            ui.add_space(4.0);
            continue;
        }
        if let Some(input) = crate::fieldlists::input(p.kind) {
            let fields = input_fields(app, n, input);
            let edit = ui.push_id(("mb_list", &n.id, p.name), |ui| crate::fieldlists::show(ui, p.kind, &cur, fields)).inner;
            if edit.checkpoint {
                app.mb.edit();
            }
            if let Some(value) = edit.value
                && let Some(node) = app.mb.graph.nodes.get_mut(i)
            {
                set_param(node, p.name, value);
            }
            ui.add_space(4.0);
            continue;
        }
        let fields: Vec<&str> =
            if matches!(p.kind, ParamKind::Field | ParamKind::FieldNames | ParamKind::NumericField | ParamKind::CoordinateField) {
                crate::panes::layer_for_param(spec, p.name)
                    .and_then(|input| input_fields(app, n, input.name))
                    .map(|fields| {
                        // As the Tools pane offers them (`panes::takes_field`).
                        fields
                            .iter()
                            .filter(|f| match p.kind {
                                ParamKind::NumericField => f.ty.is_numeric(),
                                ParamKind::FieldNames => !f.name.contains([',', ';']),
                                _ => true,
                            })
                            .map(|f| f.name.as_str())
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
        match p.kind {
            ParamKind::Bool => {
                let mut on = cur.as_bool().unwrap_or(text == "true" || (cur.is_null() && default == "true"));
                if ui.checkbox(&mut on, "").on_hover_text(tl!(p.label)).changed() {
                    set = Some((p.name, json!(on)));
                }
            }
            ParamKind::Choice(c) => {
                let shown = if text.is_empty() {
                    if default.is_empty() { "—".to_string() } else { tl!(&default).to_string() }
                } else {
                    tl!(&text).to_string()
                };
                egui::ComboBox::from_id_salt(("mb_p", &n.id, p.name))
                    .selected_text(shown)
                    .width(ui.available_width() - 4.0)
                    .show_ui(ui, |ui| {
                        for o in c {
                            if ui.selectable_label(*o == text, tl!(o)).clicked() {
                                set = Some((p.name, json!(o)));
                            }
                        }
                    })
                    .response
                    .on_hover_text(tl!(p.label));
            }
            ParamKind::Field | ParamKind::FieldNames | ParamKind::NumericField
                if spec.id.starts_with("geolibre:") || p.kind == ParamKind::FieldNames =>
            {
                let mut value = text;
                // As the text box below says it, when the input's fields aren't known yet.
                let hint = if default.is_empty() && fields.is_empty() && p.kind != ParamKind::FieldNames {
                    tl!("A field (known once the model runs)")
                } else {
                    &default
                };
                let response = crate::panes::field_suggestions(
                    ui,
                    egui::Id::unique(("mb_p", &n.id, p.name)),
                    &mut value,
                    tl!(p.label),
                    hint,
                    fields.iter().copied(),
                    p.kind == ParamKind::FieldNames,
                );
                if response.gained_focus() {
                    focus = true;
                }
                if response.changed() {
                    let value = if value.is_empty() { Value::Null } else { json!(value) };
                    if response.has_focus() {
                        if let Some(m) = app.mb.graph.nodes.get_mut(i) {
                            set_param(m, p.name, value);
                        }
                    } else {
                        set = Some((p.name, value));
                    }
                }
            }
            ParamKind::Field | ParamKind::NumericField | ParamKind::CoordinateField if !fields.is_empty() => {
                let shown = if text.is_empty() { "—".to_string() } else { text.clone() };
                egui::ComboBox::from_id_salt(("mb_p", &n.id, p.name))
                    .selected_text(shown)
                    .width(ui.available_width() - 4.0)
                    .show_ui(ui, |ui| {
                        if !p.required && ui.selectable_label(text.is_empty(), "—").clicked() {
                            set = Some((p.name, Value::Null));
                        }
                        for f in &fields {
                            if ui.selectable_label(*f == text, *f).clicked() {
                                set = Some((p.name, json!(f)));
                            }
                        }
                    })
                    .response
                    .labelled_by(label_id)
                    .on_hover_text(tl!(p.label));
            }
            _ => {
                let mut s = text.clone();
                let hint =
                    if default.is_empty() && matches!(p.kind, ParamKind::Field | ParamKind::NumericField | ParamKind::CoordinateField) {
                        tl!("A field (known once the model runs)").to_string()
                    } else {
                        default.clone()
                    };
                let r = ui.add(egui::TextEdit::singleline(&mut s).hint_text(hint).desired_width(f32::INFINITY)).on_hover_text(tl!(p.label));
                if r.gained_focus() {
                    focus = true;
                }
                if r.changed() {
                    let v = if s.trim().is_empty() {
                        Value::Null
                    } else if p.kind == ParamKind::Number {
                        s.trim().parse::<f64>().map(|x| json!(x)).unwrap_or_else(|_| json!(s))
                    } else {
                        json!(s)
                    };
                    // Typing goes into the model as it is typed, as one undo step from focus.
                    if let Some(m) = app.mb.graph.nodes.get_mut(i) {
                        set_param(m, p.name, v);
                    }
                }
            }
        }
        ui.add_space(4.0);
    }
    if focus {
        app.mb.edit();
    }
    if let Some((k, v)) = set {
        app.mb.edit();
        if let Some(m) = app.mb.graph.nodes.get_mut(i) {
            set_param(m, k, v);
        }
    }
    if let Some(port) = unwire {
        app.mb.edit();
        app.mb.graph.edges.retain(|e| !(e.to == n.id && e.to_port == port));
    }
    ui.add_space(8.0);
    ui.separator();
    let kept = app
        .mb
        .graph
        .edges
        .iter()
        .any(|e| e.from == n.id && app.mb.graph.nodes.iter().any(|m| m.id == e.to && m.kind == ModelNodeKind::Output));
    ui.label(
        egui::RichText::new(tl!("A result that only feeds other tools is removed once the model has run; add an output to keep it."))
            .size(11.0)
            .color(t.text_dim),
    );
    if ui.add_enabled(!kept, egui::Button::new(if kept { tl!("This result is kept") } else { tl!("Keep this result") })).clicked() {
        keep_result(&mut app.mb, &n.id);
    }
}

fn set_param(n: &mut ModelNode, k: &str, v: Value) {
    if !n.params.is_object() {
        n.params = json!({});
    }
    if let Value::Object(o) = &mut n.params {
        if v.is_null() {
            o.remove(k);
        } else {
            o.insert(k.to_string(), v);
        }
    }
}

/// With nothing selected: how to build, what's wrong, and the project's models.
fn overview(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, problems: &[model::Problem]) {
    ui.label(egui::RichText::new(tl!("Model")).font(semibold(14.0)));
    ui.add_space(4.0);
    for line in [
        "Drag tools from the palette onto the canvas.",
        "Add an input for each layer the model reads.",
        "Drag from a node's right port to another node's left port to connect them.",
        "Add an output to keep a result under a name.",
    ] {
        ui.label(egui::RichText::new(format!("• {}", tl!(line))).size(11.5).color(t.text_dim));
    }
    ui.add_space(8.0);
    let general: Vec<&model::Problem> = problems.iter().filter(|p| p.node.is_none()).collect();
    for p in &general {
        ui.label(egui::RichText::new(&p.message).size(11.5).color(t.red));
    }
    let counted = problems.len() - general.len();
    if counted > 0 {
        ui.label(
            egui::RichText::new(crate::i18n::fmt("{n} nodes need attention (marked !).", &[("n", &counted.to_string())]))
                .size(11.5)
                .color(t.red),
        );
    }
    let models: Vec<(String, usize)> = app
        .session
        .project
        .models
        .iter()
        .map(|m| (m.name.clone(), m.graph.as_ref().map(|g| g.nodes.len()).unwrap_or(m.steps.len())))
        .collect();
    if models.is_empty() {
        return;
    }
    ui.add_space(10.0);
    ui.separator();
    ui.label(egui::RichText::new(tl!("Saved models")).font(semibold(13.0)));
    let mut remove = None;
    for (name, n) in models {
        ui.horizontal(|ui| {
            let current = app.mb.saved_as.as_deref() == Some(name.as_str());
            let label = egui::RichText::new(crate::i18n::fmt("{name} ({n} nodes)", &[("name", &name), ("n", &n.to_string())])).size(12.0);
            if ui.selectable_label(current, label).on_hover_text(tl!("Open")).clicked() && !current {
                request(app, Pending::Open(name.clone()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").on_hover_text(tl!("Delete Model")).clicked() {
                    remove = Some(name.clone());
                }
            });
        });
    }
    if let Some(name) = remove {
        match app.run("model.remove", json!({"name": name})) {
            Ok(_) => {
                // The canvas keeps the model, now unsaved.
                if app.mb.saved_as.as_deref() == Some(name.as_str()) {
                    app.mb.saved_as = None;
                    app.mb.saved = ModelGraph::default();
                }
            }
            Err(e) => app.mb.log(e, LogKind::Error),
        }
    }
}

fn log_ui(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, problems: &[model::Problem]) {
    egui::ScrollArea::vertical().id_salt("mb_log").auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        if problems.is_empty() && app.mb.log.is_empty() {
            let text = if app.mb.graph.nodes.is_empty() { tl!("Messages appear here.") } else { tl!("The model is ready to run.") };
            ui.label(egui::RichText::new(text).size(11.5).color(t.text_dim));
        }
        if app.mb.log.is_empty() {
            for p in problems.iter().take(20) {
                let node = p.node.as_ref().and_then(|id| app.mb.graph.nodes.iter().find(|n| &n.id == id)).map(model::title);
                let line = match node {
                    Some(n) => format!("{n}: {}", p.message),
                    None => p.message.clone(),
                };
                let r = ui.add(egui::Label::new(egui::RichText::new(line).size(11.5).color(t.red)).sense(Sense::click()));
                if let Some(id) = &p.node
                    && r.on_hover_text(tl!("Click to select the node")).clicked()
                {
                    app.mb.selected = Some(id.clone());
                    app.mb.selected_edge = None;
                }
            }
        }
        for (line, kind) in &app.mb.log {
            let c = match kind {
                LogKind::Info => t.text,
                LogKind::Ok => t.green,
                LogKind::Error => t.red,
            };
            ui.label(egui::RichText::new(line).size(11.5).color(c));
        }
    });
}

/// Asks before dropping unsaved changes.
fn confirm(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(p) = app.mb.confirm.clone() else { return };
    let mut answer = None;
    egui::Modal::new(egui::Id::unique("mb_confirm")).show(ctx, |ui| {
        ui.set_width(320.0);
        ui.label(egui::RichText::new(tl!("Discard unsaved changes to the current model?")).strong());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(tl!("Save")).clicked() {
                answer = Some(1);
            }
            if ui.button(tl!("Discard")).clicked() {
                answer = Some(2);
            }
            if ui.button(tl!("Cancel")).clicked() {
                answer = Some(0);
            }
        });
    });
    match answer {
        Some(1) => {
            app.mb.confirm = None;
            if save(app) {
                proceed(app, p);
            }
        }
        Some(2) => {
            app.mb.confirm = None;
            proceed(app, p);
        }
        Some(_) => app.mb.confirm = None,
        None => {}
    }
}

/// Drops the canvas of a project no longer open (its model would be saved into this one).
fn forget_old_project(app: &mut RustGisApp) {
    if app.mb.project != app.session.project_starts {
        // The runs still to land stay, so each one's landing is still matched with its own.
        app.mb = ModelBuilder {
            project: app.session.project_starts,
            search: std::mem::take(&mut app.mb.search),
            running: std::mem::take(&mut app.mb.running),
            ..Default::default()
        };
    }
}

/// Hides ModelBuilder when another map or layout came into view, and closes it when another
/// project was opened.
pub fn follow_view(app: &mut RustGisApp) {
    forget_old_project(app);
    if app.mb.active && app.mb.view_key != (app.session.project.active_map, app.session.active_layout) {
        app.mb.active = false;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use egui::{Event, Key, Modifiers};

    use super::*;

    fn harness(app: RustGisApp) -> egui_kittest::Harness<'static, RustGisApp> {
        let mut fonts = false;
        egui_kittest::Harness::builder().with_size(vec2(1300.0, 800.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                let ctx = ui.ctx().clone();
                crate::chrome::shortcuts(app, &ctx);
                crate::mapview::show(app, ui);
            },
            app,
        )
    }

    /// A model built on the canvas runs: its tools in flow order, the intermediate result
    /// dropped, the output named; the nodes show how they fared.
    #[test]
    fn a_model_drawn_on_the_canvas_runs_and_saves() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.modelBuilder", Value::Null).unwrap();
        assert!(app.mb.shown());
        let input = app.mb.add(&Add::Input, pos2(0.0, 0.0));
        app.mb.graph.nodes.iter_mut().find(|n| n.id == input).unwrap().layer = Some("Cities".into());
        let buffer = app.mb.add(&Add::Tool("buffer"), pos2(220.0, 0.0));
        let dissolve = app.mb.add(&Add::Tool("dissolve"), pos2(440.0, 0.0));
        app.mb.connect(&input, &buffer, "input", false).unwrap();
        app.mb.connect(&buffer, &dissolve, "input", false).unwrap();
        assert_eq!(app.mb.connect(&dissolve, &buffer, "input", false), Err("That connection would create a loop."));
        keep_result(&mut app.mb, &dissolve);
        let out = app.mb.graph.nodes.iter().find(|n| n.kind == ModelNodeKind::Output).unwrap().clone();
        assert_eq!(out.name.as_deref(), Some("Dissolve"));
        assert!(model::validate(&app.session, &app.mb.graph).is_empty());

        let layers = app.session.map().layers.len();
        run_model(&mut app);
        assert!(app.session.map().layer_by_name("Dissolve").is_some(), "{:?}", app.mb.log);
        assert_eq!(app.session.map().layers.len(), layers + 1);
        assert!(app.mb.graph.nodes.iter().all(|n| app.mb.status.get(&n.id) == Some(&Status::Done)));

        // On the desktop its tools run on a thread of their own: the canvas says so until it lands.
        app.reads_off_frame = true;
        run_model(&mut app);
        assert_eq!(app.mb.log, vec![("Running…".to_string(), LogKind::Info)]);
        assert!(app.mb.status.is_empty());
        assert_eq!(app.session.map().layers.len(), layers + 1, "nothing added before it lands");
        // A node moved meanwhile still gets its mark: the graph runs the same.
        app.mb.graph.nodes[0].x += 40.0;
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while crate::readjob::land(&mut app) {
            assert!(std::time::Instant::now() < end, "the model never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(app.session.map().layers.len(), layers + 2);
        assert!(app.mb.graph.nodes.iter().all(|n| app.mb.status.get(&n.id) == Some(&Status::Done)));
        assert!(
            app.mb.log.last().is_some_and(|(line, kind)| *kind == LogKind::Ok && line.starts_with("Run finished.")),
            "{:?}",
            app.mb.log
        );
        // Run pressed twice: the second waits for the first, and each marks the canvas as it lands.
        run_model(&mut app);
        run_model(&mut app);
        while crate::readjob::land(&mut app) {
            assert!(std::time::Instant::now() < end, "the models never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(app.session.map().layers.len(), layers + 4);
        assert!(app.mb.graph.nodes.iter().all(|n| app.mb.status.get(&n.id) == Some(&Status::Done)));
        assert!(app.mb.log.iter().any(|(line, _)| line.starts_with("Buffer made")), "{:?}", app.mb.log);
        assert!(app.mb.running.is_empty());
        // Cancelled: nothing added, and the canvas says so without the last run's marks.
        run_model(&mut app);
        app.reads.cancel_for_tests();
        while crate::readjob::land(&mut app) {
            assert!(std::time::Instant::now() < end, "the model never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(app.session.map().layers.len(), layers + 4);
        assert!(app.mb.status.is_empty());
        assert_eq!(app.mb.log, vec![("Run failed: Run Model of Model was cancelled".to_string(), LogKind::Error)]);
        app.reads_off_frame = false;

        app.mb.name = "Dissolved cities".into();
        assert!(app.mb.dirty());
        assert!(save(&mut app));
        assert!(!app.mb.dirty());
        let m = app.session.project.models.iter().find(|m| m.name == "Dissolved cities").unwrap();
        assert_eq!(m.graph.as_ref().unwrap(), &app.mb.graph);
        assert_eq!(m.steps.len(), 2);
    }

    /// Delete removes the selected node with its wires, Ctrl+Z brings both back, Ctrl+D copies
    /// it; a map tab clicked (or a map activated) hides ModelBuilder, its tab shows it again.
    #[test]
    fn keys_edit_the_canvas_and_tabs_switch_views() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.modelBuilder", Value::Null).unwrap();
        let a = app.mb.add(&Add::Input, pos2(0.0, 0.0));
        let b = app.mb.add(&Add::Tool("buffer"), pos2(220.0, 0.0));
        app.mb.connect(&a, &b, "input", false).unwrap();
        app.mb.selected = Some(b.clone());
        let mut h = harness(app);
        h.run_steps(3);
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let layers = h.state().session.map().layers.len();

        h.event(key(Key::Delete, Modifiers::NONE));
        h.step();
        assert_eq!(h.state().mb.graph.nodes.len(), 1);
        assert!(h.state().mb.graph.edges.is_empty());
        assert_eq!(h.state().session.map().layers.len(), layers, "Delete didn't reach the map's layers");

        h.event(key(Key::Z, Modifiers::COMMAND));
        h.step();
        assert_eq!(h.state().mb.graph.nodes.len(), 2);
        assert_eq!(h.state().mb.graph.edges.len(), 1);

        h.state_mut().mb.selected = Some(b.clone());
        h.event(key(Key::D, Modifiers::COMMAND));
        h.step();
        assert_eq!(h.state().mb.graph.nodes.len(), 3);
        assert_ne!(h.state().mb.selected.as_deref(), Some(b.as_str()), "the copy is selected");

        h.state_mut().run("map.new", Value::Null).unwrap();
        h.run_steps(2);
        assert!(!h.state().mb.shown(), "another map came into view");
        assert!(h.state().mb.open, "the tab stays");
        h.state_mut().run("ui.modelBuilder", Value::Null).unwrap();
        h.run_steps(2);
        assert!(h.state().mb.shown());
        assert_eq!(h.state().mb.graph.nodes.len(), 3, "the canvas is kept");
    }

    /// Closing with unsaved changes asks first; Discard closes, Cancel keeps the model open.
    #[test]
    fn closing_asks_about_unsaved_changes() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.modelBuilder", Value::Null).unwrap();
        app.mb.add(&Add::Tool("buffer"), pos2(0.0, 0.0));
        app.run("ui.modelBuilder", json!({"close": true})).unwrap();
        assert!(app.mb.open && app.mb.confirm.is_some());
        app.mb.confirm = None;
        proceed(&mut app, Pending::Close);
        assert!(!app.mb.open && !app.mb.shown());
    }

    /// Opening another project closes ModelBuilder and drops its canvas.
    #[test]
    fn another_project_drops_the_canvas() {
        let mut app = RustGisApp::with_sample();
        app.run("ui.modelBuilder", Value::Null).unwrap();
        app.mb.add(&Add::Tool("buffer"), pos2(0.0, 0.0));
        app.mb.saved_as = Some("Old".into());
        // A run under way when the project changes still lands after the reset.
        app.mb.running.push_back((app.session.project_starts, app.mb.graph.clone()));
        app.run("project.new", Value::Null).unwrap();
        follow_view(&mut app);
        assert!(!app.mb.open && app.mb.graph.nodes.is_empty() && app.mb.saved_as.is_none());
        // Its landing takes its own place, not that of a run asked in this project, and leaves
        // this canvas as it is.
        app.mb.running.push_back((app.session.project_starts, app.mb.graph.clone()));
        app.mb.log("Running…", LogKind::Info);
        landed(&mut app, crate::Landed::NotRun("another project was opened".into()));
        assert_eq!(app.mb.log, vec![("Running…".to_string(), LogKind::Info)]);
        assert_eq!(app.mb.running.len(), 1);
        // Another model's run landing first, in this project, leaves it too.
        let mut other = app.mb.graph.clone();
        other.nodes.push(Default::default());
        app.mb.running.push_front((app.session.project_starts, other.clone()));
        landed(&mut app, crate::Landed::NotRun("cancelled".into()));
        assert_eq!(app.mb.log, vec![("Running…".to_string(), LogKind::Info)]);
        assert_eq!(app.mb.running.len(), 1);
        // The canvas's own run lands; another graph's landing after it leaves its ending shown.
        app.mb.waiting = true;
        landed(&mut app, crate::Landed::NotRun("cancelled".into()));
        let shown = app.mb.log.clone();
        assert_eq!(shown, vec![("Run failed: cancelled".to_string(), LogKind::Error)]);
        app.mb.running.push_back((app.session.project_starts, other));
        landed(&mut app, crate::Landed::NotRun("cancelled".into()));
        assert_eq!(app.mb.log, shown);
    }

    /// Old models (steps with `%n`) open on the canvas as graphs.
    #[test]
    fn step_models_open_as_graphs() {
        let mut app = RustGisApp::with_sample();
        app.run(
            "model.new",
            json!({"name": "Old", "steps": [{"tool": "buffer", "params": {"input": "Cities", "distance": 5}}, {"tool": "dissolve", "params": {"input": "%1"}}]}),
        )
        .unwrap();
        app.session.project.models.last_mut().unwrap().graph = None;
        app.run("ui.modelBuilder", json!({"name": "Old"})).unwrap();
        assert_eq!(app.mb.graph.nodes.len(), 3);
        assert_eq!(app.mb.graph.edges.len(), 2);
        assert!(!app.mb.imported);
    }

    /// A step model beyond the conversion limit keeps its steps and the current canvas on
    /// every open route, including import and an accepted discard prompt.
    #[test]
    fn oversized_step_models_do_not_replace_the_canvas() {
        let mut app = RustGisApp::with_sample();
        app.run("model.new", json!({"name": "Keep", "steps": [{"tool": "buffer", "params": {"input": "Cities"}}]})).unwrap();
        app.run("ui.modelBuilder", json!({"name": "Keep"})).unwrap();
        let graph = app.mb.graph.clone();
        let steps = vec![json!({"tool": "buffer", "params": {}}); 1001];
        app.run("model.new", json!({"name": "Large", "steps": steps})).unwrap();
        let models = app.session.project.models.clone();

        let error = app.run("ui.modelBuilder", json!({"name": "Large"})).unwrap_err();
        assert!(error.contains("1001 steps") && error.contains("at most 1000"), "{error}");
        for route in 0..2 {
            app.session.notices.clear();
            if route == 0 {
                imported(&mut app, &json!({"model": "Large"}));
            } else {
                proceed(&mut app, Pending::Open("Large".into()));
            }
            assert!(app.session.notices.iter().any(|notice| notice.error && notice.text.contains("1001 steps")));
            assert_eq!(app.mb.graph, graph);
            assert_eq!(app.mb.saved_as.as_deref(), Some("Keep"));
            assert_eq!(app.session.project.models, models);
        }
    }
}
