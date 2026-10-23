//! L4: the session (project, undo, active map/layer/tool) and the command registry.
//!
//! **Everything is a command.** Each user-visible action is a [`CommandSpec`] with an id; the
//! ribbon, keyboard shortcuts, command search and the command line all dispatch through
//! [`Session::run`]. Programmatic calls never open dialogs.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod cmd;
pub mod connections;
pub mod geometry_calc;
pub mod model;
pub mod project;
pub mod sample;
pub mod sources;
#[cfg(test)]
mod tests;

pub use project::Project;
pub use rustgis_analysis as analysis;
pub use rustgis_data as data;
pub use rustgis_geom as geom;
pub use rustgis_map as map;
pub use rustgis_plugins as plugins;
pub use rustgis_render as render;

use std::path::PathBuf;

use rustgis_map::{Camera, Map};
use serde_json::Value;

pub type CmdResult = Result<Value, String>;

/// A command.
#[derive(Clone, Copy)]
pub struct CommandSpec {
    pub id: &'static str,
    pub label: &'static str,
    /// Where it lives in the UI: `Home › Navigate`, `Analyze › Geoprocessing`…
    pub location: &'static str,
    /// Default shortcut (`Mod+S`, `F5`); `Mod` = Ctrl, or ⌘ on macOS. The UI's keymap adds it
    /// to its own defaults when the key is free.
    pub shortcut: &'static str,
    /// Parameter description for scripts and agents.
    pub params: &'static str,
    /// Whether it changes the project (and so takes an undo snapshot).
    pub mutates: bool,
    /// `None` = enabled; `Some(reason)` = disabled.
    pub enabled: fn(&Session) -> Option<&'static str>,
    pub run: fn(&mut Session, &Value) -> CmdResult,
}

fn always(_: &Session) -> Option<&'static str> {
    None
}

impl CommandSpec {
    /// The top-level keys its [`params`](CommandSpec::params) description names, or `None` when
    /// the description is not a `{"key": …}` object (nothing to check against).
    pub fn param_keys(&self) -> Option<Vec<&'static str>> {
        param_keys(self.params)
    }

    /// Whether the parameter description includes a required top-level input.
    pub fn needs_input(&self) -> bool {
        needs_input(self.params)
    }

    pub const fn new(id: &'static str, label: &'static str, location: &'static str, run: fn(&mut Session, &Value) -> CmdResult) -> Self {
        CommandSpec { id, label, location, shortcut: "", params: "{}", mutates: true, enabled: always, run }
    }
    pub const fn key(mut self, s: &'static str) -> Self {
        self.shortcut = s;
        self
    }
    pub const fn params(mut self, p: &'static str) -> Self {
        self.params = p;
        self
    }
    /// Doesn't change the project (no undo snapshot, not dirtying).
    pub const fn view(mut self) -> Self {
        self.mutates = false;
        self
    }
    pub const fn when(mut self, f: fn(&Session) -> Option<&'static str>) -> Self {
        self.enabled = f;
        self
    }
}

/// The keys of a `{"key": …, "other"?: …}` description: quoted words at the top level that open
/// an entry (after `{` or a top-level `,`, followed by `?`, `:`, `,`, `}` or `|`) or an
/// alternative (after `|`, followed by `?` or `:`, as in `"path": folder | "url": …` or
/// `"lon" | "x": number`).
pub fn param_keys(desc: &str) -> Option<Vec<&str>> {
    let mut keys = Vec::new();
    param_entries(desc, |key, _| keys.push(key))?;
    Some(keys)
}

/// One entry of a parameter description: its key, whether it may be left out, and its text as
/// written (`"size"?: number`), for help that lists the parameters one per line.
#[derive(Clone, Debug, PartialEq)]
pub struct Param<'a> {
    pub key: &'a str,
    pub optional: bool,
    pub text: &'a str,
}

/// The entries of a `{"key": …, "other"?: …}` description, split at its top-level commas (commas
/// inside quotes, parentheses, brackets or braces belong to an entry's text). The key is an
/// entry's first quoted word; an entry without one (prose) is left out. None when the
/// description isn't an object.
pub fn param_list(desc: &str) -> Option<Vec<Param<'_>>> {
    // Up to the last brace: a note may follow the object, as in `{"path"?: file} (a style)`.
    let body = desc.trim().strip_prefix('{')?;
    let inner = body.get(..body.rfind('}')?)?;
    let mut out = Vec::new();
    let (mut depth, mut quoted, mut start, mut escaped) = (0usize, false, 0, false);
    for (i, c) in inner.char_indices() {
        // A quote after a backslash (`\"`, in a plugin's description) doesn't open or close one.
        let was_escaped = escaped;
        escaped = c == '\\' && !was_escaped;
        match c {
            '"' if !was_escaped => quoted = !quoted,
            '(' | '[' | '{' if !quoted => depth += 1,
            ')' | ']' | '}' if !quoted => depth = depth.saturating_sub(1),
            ',' if !quoted && depth == 0 => {
                out.extend(inner.get(start..i).and_then(param));
                start = i + 1;
            }
            _ => {}
        }
    }
    out.extend(inner.get(start..).and_then(param));
    Some(out)
}

/// An entry of a parameter description (see `param_list`): None for prose without a key.
fn param(part: &str) -> Option<Param<'_>> {
    let text = part.trim();
    let rest = text.strip_prefix('"')?;
    let (key, after) = rest.split_at(rest.find('"')?);
    let optional = after.get(1..).is_some_and(|a| a.trim_start().starts_with('?'));
    Some(Param { key, optional, text })
}

/// Whether a parameter description has a required key. Question marks in values do not count.
pub fn needs_input(desc: &str) -> bool {
    let mut required = false;
    match param_entries(desc, |_, optional| required |= !optional) {
        Some(()) => required,
        None => desc.contains(':'),
    }
}

/// Visits top-level keys with their optional flag without allocating while checking requirements.
fn param_entries<'a>(desc: &'a str, mut visit: impl FnMut(&'a str, bool)) -> Option<()> {
    #[derive(PartialEq)]
    enum At {
        Value,
        Entry,
        Alternative,
    }
    let bytes = desc.as_bytes();
    if bytes.iter().find(|c| !c.is_ascii_whitespace()) != Some(&b'{') {
        return None;
    }
    let (mut depth, mut at, mut i) = (0usize, At::Value, 0);
    while let Some(&c) = bytes.get(i) {
        match c {
            b'"' => {
                let end = bytes.get(i + 1..).and_then(|r| r.iter().position(|&c| c == b'"')).map_or(bytes.len(), |n| i + 1 + n);
                let next = bytes.get(end + 1..).and_then(|r| r.iter().find(|c| !c.is_ascii_whitespace()));
                let key = match at {
                    At::Entry => matches!(next, Some(b'?' | b':' | b',' | b'}' | b'|')),
                    At::Alternative => matches!(next, Some(b'?' | b':')),
                    At::Value => false,
                };
                if depth == 1
                    && key
                    && let Some(key) = desc.get(i + 1..end)
                {
                    visit(key, next == Some(&b'?'));
                }
                at = At::Value;
                i = end;
            }
            b'{' | b'[' | b'(' => {
                depth += 1;
                at = if depth == 1 { At::Entry } else { At::Value };
            }
            b'}' | b']' | b')' => {
                depth = depth.saturating_sub(1);
                at = At::Value;
            }
            b',' if depth == 1 => at = At::Entry,
            b'|' if depth == 1 => at = At::Alternative,
            c if c.is_ascii_whitespace() => {}
            _ => at = At::Value,
        }
        i += 1;
    }
    Some(())
}

/// `s` cut to `n` characters, with `…` when cut.
fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n { s.chars().take(n).chain(['…']).collect() } else { s.to_string() }
}

pub struct Registry {
    pub commands: Vec<CommandSpec>,
}

impl Registry {
    pub fn get(&self, id: &str) -> Option<&CommandSpec> {
        self.commands.iter().find(|c| c.id == id)
    }
}

/// The interactive map tool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Explore,
    Select,
    ZoomIn,
    Measure,
    MeasureArea,
    CreatePoint,
    CreateLine,
    CreatePolygon,
    /// Moves, inserts and deletes the vertices of the selected features.
    EditVertices,
    /// Draws a graphic: the shape is one of [`GRAPHIC_TOOLS`]' shapes.
    Graphic(&'static str),
}

/// The graphic drawing tools: (shape, tool code, label).
pub const GRAPHIC_TOOLS: &[(&str, &str, &str)] = &[
    ("point", "graphicPoint", "Point"),
    ("text", "graphicText", "Text"),
    ("line", "graphicLine", "Line"),
    ("curve", "graphicCurve", "Curve"),
    ("freehand", "graphicFreehand", "Freehand"),
    ("polygon", "graphicPolygon", "Polygon"),
    ("lasso", "graphicLasso", "Lasso"),
    ("rectangle", "graphicRectangle", "Rectangle"),
    ("circle", "graphicCircle", "Circle"),
    ("ellipse", "graphicEllipse", "Ellipse"),
    ("picture", "graphicPicture", "Picture"),
];

impl Tool {
    pub fn code(self) -> &'static str {
        match self {
            Tool::Explore => "explore",
            Tool::Select => "select",
            Tool::ZoomIn => "zoomIn",
            Tool::Measure => "measure",
            Tool::MeasureArea => "measureArea",
            Tool::CreatePoint => "createPoint",
            Tool::CreateLine => "createLine",
            Tool::CreatePolygon => "createPolygon",
            Tool::EditVertices => "editVertices",
            Tool::Graphic(shape) => GRAPHIC_TOOLS.iter().find(|g| g.0 == shape).map_or("graphicPoint", |g| g.1),
        }
    }
    pub fn from_code(s: &str) -> Option<Tool> {
        if let Some(g) = GRAPHIC_TOOLS.iter().find(|g| g.1.eq_ignore_ascii_case(s.trim())) {
            return Some(Tool::Graphic(g.0));
        }
        [
            Tool::Explore,
            Tool::Select,
            Tool::ZoomIn,
            Tool::Measure,
            Tool::MeasureArea,
            Tool::CreatePoint,
            Tool::CreateLine,
            Tool::CreatePolygon,
            Tool::EditVertices,
        ]
        .into_iter()
        .find(|t| t.code().eq_ignore_ascii_case(s.trim()))
    }
    pub fn label(self) -> &'static str {
        match self {
            Tool::Explore => "Explore",
            Tool::Select => "Select",
            Tool::ZoomIn => "Zoom In",
            Tool::Measure => "Measure Distance",
            Tool::MeasureArea => "Measure Area",
            Tool::CreatePoint => "Create Point",
            Tool::CreateLine => "Create Line",
            Tool::CreatePolygon => "Create Polygon",
            Tool::EditVertices => "Edit Vertices",
            Tool::Graphic(shape) => GRAPHIC_TOOLS.iter().find(|g| g.0 == shape).map_or("Point", |g| g.2),
        }
    }
    pub fn creates(self) -> bool {
        matches!(self, Tool::CreatePoint | Tool::CreateLine | Tool::CreatePolygon)
    }
}

/// One geoprocessing history row.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GpRecord {
    /// The tool's name, as shown.
    pub tool: String,
    /// The tool's id (`buffer`), to run it again (#539); empty in projects saved before it,
    /// where the name finds a built-in tool.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub params: Value,
    /// The command that ran the tool, when it wasn't `gp.run` with `params`: its id and its
    /// parameters, such as a point buffered where the map was clicked (`gp.bufferPoint`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<(String, Value)>,
    /// When it ran, in seconds since 1970 (UTC); 0 in projects saved before it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub at: u64,
    pub ok: bool,
    pub messages: Vec<String>,
    pub millis: u64,
    /// The layers the run added (by name at the time).
    #[serde(default)]
    pub outputs: Vec<String>,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// A topology problem found by `topology.validate` (shown in the Error Inspector).
#[derive(Clone, Debug, PartialEq)]
pub struct TopologyIssue {
    pub rule: String,
    pub layer: String,
    pub rows: Vec<usize>,
    pub lon: f64,
    pub lat: f64,
}

/// Something worth telling the user (status bar / notifications).
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub text: String,
    pub error: bool,
}

const UNDO_LIMIT: usize = 40;

/// An undo or redo step: the project before it, the layout open then, its label and who made it.
type Step = (Project, Option<usize>, &'static str, Option<String>);

/// What a project holds in the session beside itself: what [`Session::clear_history`] forgets
/// when another project starts, and what opening one sets (its file, layer, tool…). When another
/// replaces it within a command that can be rolled back (a task's step), it is put aside rather
/// than dropped, and given back if that command fails and the project is restored. One list
/// ([`Session::take_project_state`]) serves both, and giving it back names every field, so a
/// field forgotten here can't be wiped by a replacement without coming back.
struct PutAside {
    /// The project these belong to (`Session::opened` before the replacement).
    opened: u64,
    undo: Vec<Step>,
    redo: Vec<Step>,
    back: Vec<Camera>,
    forward: Vec<Camera>,
    last_camera: Option<Camera>,
    /// The projection those cameras are in: without it, the extent history given back would be
    /// taken for another projection's and cleared.
    history_projection: String,
    dirty: bool,
    trusted_tasks: std::collections::HashSet<String>,
    trusted_models: std::collections::HashSet<String>,
    trusted_notebooks: std::collections::HashSet<String>,
    project_starts: u64,
    history_from_file: usize,
    active_layer: Option<u32>,
    layer_selection: Vec<u32>,
    layer_selection_of: Option<u32>,
    table_layer: Option<u32>,
    table_standalone: Option<u32>,
    tool: Tool,
    measurements: Vec<serde_json::Value>,
    topology_errors: Vec<TopologyIssue>,
    /// The file it was opened from or saved to: a Save after the rollback writes there, not to
    /// the file the replacement named (or none).
    path: Option<PathBuf>,
    model_run: Option<model::RunReport>,
    /// Its work still under way (tool runs, downloads, plugin fetches, queries): the runs of
    /// the previous project mustn't land in the next one.
    pending: Vec<PendingRun>,
    #[cfg(feature = "geolibre")]
    started: Vec<(u64, rustgis_geolibre::Job, serde_json::Value, u64)>,
    plugin_runs: Vec<cmd::gp::StartedRun>,
    downloading: Vec<u64>,
    downloaded: Vec<(u64, Vec<u8>)>,
    url_requests: Vec<serde_json::Value>,
    plugin_retries: Vec<cmd::plugins::fetch::Retry>,
    /// A style of the project still loading, which mustn't move the next one's view.
    style_view: Option<(String, Camera)>,
    #[cfg(not(target_arch = "wasm32"))]
    sql_results: Vec<(u64, String, Option<String>, rustgis_data::source::duckdb::Output)>,
    trend_results: Vec<(u64, Vec<rustgis_data::raster::Raster>, usize)>,
    edit_counts: std::collections::HashMap<u32, cmd::design::EditCounts>,
}

pub struct Session {
    pub project: Project,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    /// Favourite, recent and saved data sources (kept with the app's preferences, not the
    /// project).
    pub sources: sources::Sources,
    /// Saved connections (`connection.*`): kept with the app's preferences, not the project; never a secret.
    pub connections: connections::Connections,
    /// How rasters added from now on are drawn (`raster.setDefaultStyle`; kept with the app's
    /// preferences, not the project).
    pub raster_defaults: rustgis_map::RasterDefaults,
    pub registry: Registry,
    /// Bumped on every change so views can cache.
    pub revision: u64,
    /// Bumped each time another project replaces this one (new, open, sample, template), however
    /// the command came (the app, an agent, a script): the interface drops what it kept for the
    /// old one.
    pub opened: u64,
    pub tool: Tool,
    /// The layer selected in the Layers pane (the one the Style and Data tabs act on).
    pub active_layer: Option<u32>,
    /// The layers picked together in the Layers pane, the active layer among them. Read through
    /// [`Session::selected_layers`]: it holds only while `layer_selection_of` is still the active
    /// layer, so setting `active_layer` on its own (adding data, a removal) starts over with that
    /// layer alone.
    layer_selection: Vec<u32>,
    layer_selection_of: Option<u32>,
    /// Map view size in logical points (set by the UI each frame).
    pub view_size: (f64, f64),
    /// A MapLibre style tile layer (by its address) whose own view the map goes to once it loads
    /// (`map.addTiles` with `"view": true`), or a PMTiles archive whose bounds it goes to, and the
    /// camera then: moved since, the map stays.
    pub style_view: Option<(String, Camera)>,
    pub notices: Vec<Notice>,
    /// Notices given in this session, including ones no longer kept (see [`Session::notices_since`]),
    /// less those a failed command withdrew.
    notices_given: u64,
    /// Layer whose attribute table is open.
    pub table_layer: Option<u32>,
    /// The standalone table shown in the attribute table instead of a layer, by its id.
    pub table_standalone: Option<u32>,
    /// Edits per layer id since the project was last saved (Edit › Status). A failed command's
    /// are rolled back; undo doesn't subtract them, since they count edits made, not the net change.
    pub edit_counts: std::collections::HashMap<u32, cmd::design::EditCounts>,
    /// The feature cache fills itself after projection changes (Data › Feature Cache).
    pub auto_cache: bool,
    /// Mensuration results (Analyze › Mensuration › Results).
    pub measurements: Vec<serde_json::Value>,
    /// The last topology check's errors (Edit › Error Inspector).
    pub topology_errors: Vec<TopologyIssue>,
    /// The layout shown instead of the map view, if any (an index into `project.layouts`).
    pub active_layout: Option<usize>,
    /// Tasks made in this session (lower-cased names). Others came from files, and their steps
    /// may not touch files when run.
    pub trusted_tasks: std::collections::HashSet<String>,
    /// Models made in this session (lower-cased names); the steps of others may not touch files.
    pub trusted_models: std::collections::HashSet<String>,
    /// What the last model run did, node by node (`model::RunReport`), kept even when the run
    /// failed and the project was restored, so ModelBuilder can show which node stopped it.
    pub model_run: Option<model::RunReport>,
    /// Counts the projects started in this session (new, open): front ends holding state about
    /// the project (ModelBuilder's canvas) drop it when this changes.
    pub project_starts: u64,
    /// Notebooks made in this session (lower-cased names); the cells of others may not touch files.
    pub trusted_notebooks: std::collections::HashSet<String>,
    /// A notebook is running: it may not start another, even through a task or model (no
    /// unbounded recursion).
    pub(crate) notebook_running: bool,
    /// How many geoprocessing history records came with the opened project (from its file).
    pub history_from_file: usize,
    /// The report open in the Report pane, if any.
    pub active_report: Option<usize>,
    /// The presentation and notebook open in their panes, if any.
    pub active_presentation: Option<usize>,
    pub active_notebook: Option<usize>,
    /// Drawing tools stay active after a graphic is drawn (Edit › Graphics).
    pub keep_tool: bool,
    /// Copied features (Edit › Clipboard).
    pub clipboard: Option<cmd::clipboard::Clipboard>,
    /// Copied layers (Layers › Copy, #491).
    pub layer_clipboard: Option<cmd::layer_copy::LayerClip>,
    /// Each step keeps the layout that was open with its project (`active_layout` is the
    /// session's, so a step that added or moved layouts opens the right one when undone).
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// The key the next command's undo step merges by ([`Session::merge_undo`]), and the key the
    /// last step was made under.
    merge: Option<String>,
    merged: Option<String>,
    /// What the project replaced during the command running held beside itself, to give back if
    /// the command fails ([`PutAside`]); and how deep commands run within commands now.
    put_aside: Option<Box<PutAside>>,
    depth: u32,
    /// How many of the commands running now restore the project if they fail: a replacement is
    /// put aside only under one of them, as only then can it be rolled back.
    restorable: u32,
    /// Who runs commands now: `None` is the user; the MCP relay names an attached agent around
    /// its calls. Each undo step remembers who made it ([`Session::undo_actor`]).
    pub actor: Option<String>,
    back: Vec<Camera>,
    forward: Vec<Camera>,
    /// Last camera recorded in the extent history.
    last_camera: Option<Camera>,
    /// The projection the extent history's cameras are in (their units change with it).
    history_projection: String,
    /// Finished asynchronous tool runs waiting for `gp.finish` (the browser's GeoLibre tools).
    pub(crate) pending: Vec<PendingRun>,
    pub(crate) next_pending: u64,
    /// Asynchronous runs started and not yet complete: (id, job, resolved params, start time).
    #[cfg(feature = "geolibre")]
    pub(crate) started: Vec<(u64, rustgis_geolibre::Job, serde_json::Value, u64)>,
    /// Plugin tool runs started and not yet complete (`gp::plugin_start`).
    pub(crate) plugin_runs: Vec<cmd::gp::StartedRun>,
    /// Plugins' panels, by plugin id: the widgets' values and what the plugin last said about
    /// them (it keeps nothing between calls). Not saved with the project: a plugin's state
    /// command reads what it needs from the map when the panel opens.
    pub plugin_panels: std::collections::BTreeMap<String, rustgis_plugins::PanelState>,
    /// Data downloads of this project under way (Add Data › From URL), by id.
    downloading: Vec<u64>,
    /// Downloads that landed, waiting for `layer.addDownload`.
    pub(crate) downloaded: Vec<(u64, Vec<u8>)>,
    /// Downloads a command asked for where nothing may wait (the browser): the parameters of
    /// `layer.addUrl` calls, for the front end to fetch and add (see [`Session::take_url_requests`]).
    pub(crate) url_requests: Vec<serde_json::Value>,
    /// Set by [`Session::request_url`] for [`Session::guarded`]: the command running changed
    /// nothing, it only left a request (a plugin's own answer can't claim that).
    request_only: bool,
    /// Plugin commands waiting for their `host.fetch` answers to run again (the browser).
    pub(crate) plugin_retries: Vec<cmd::plugins::fetch::Retry>,
    /// How many times the plugin command running now has been run again for its fetches.
    pub(crate) retry_tries: u32,
    /// Set for [`Session::guarded`] by a command whose failure is no news (a plugin command
    /// undone to run again once its fetches land): no error notice.
    pub(crate) quiet_failure: bool,
    /// Query results the app ran off the frame (the SQL pane), waiting for `layer.addSql {"ran"}`:
    /// the id, the query and database (label) they came from, and the result.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) sql_results: Vec<(u64, String, Option<String>, rustgis_data::source::duckdb::Output)>,
    /// Trends the app worked out on a thread, waiting for `cube.trend {"ran"}`: the id, the
    /// layers and how many dates (see [`cmd::cube::land`]).
    pub(crate) trend_results: Vec<(u64, Vec<rustgis_data::raster::Raster>, usize)>,
}

/// The most `layer.addUrl` requests kept for the front end at a time.
const MAX_URL_REQUESTS: usize = 16;

/// A finished tool run, held until `gp.finish` applies it.
pub(crate) struct PendingRun {
    pub id: u64,
    pub tool: String,
    /// The tool's id, for the history.
    pub tool_id: String,
    pub params: serde_json::Value,
    pub result: Result<rustgis_analysis::ToolResult, String>,
    /// When the run started, in milliseconds since 1970.
    pub started: u64,
    pub millis: u64,
}

/// Empties the windows of a project's streamed layers (they are read again from their sources).
fn strip_windows(p: &mut Project) {
    for l in p.maps.iter_mut().flat_map(|m| m.layers.iter_mut()) {
        if let Some(src) = &mut l.source {
            src.reset();
            l.dataset.features = Vec::new().into();
            l.selection.clear();
            l.touch_geometry();
        }
    }
}

/// The first streamed layer whose features or fields differ between two versions of a project
/// for a reason other than a read of its source.
fn edited_streamed(before: &Project, after: &Project) -> Option<String> {
    let schema = |l: &rustgis_map::Layer| l.dataset.fields.iter().map(|f| (f.name.clone(), f.ty)).collect::<Vec<_>>();
    for (mb, ma) in before.maps.iter().zip(&after.maps) {
        for la in ma.layers.iter().filter(|l| l.is_streamed()) {
            let Some(lb) = mb.layers.iter().find(|l| l.id == la.id && l.is_streamed()) else { continue };
            let reads = |l: &rustgis_map::Layer| l.source.as_ref().map(|s| s.reads);
            if reads(la) == reads(lb) && (la.dataset.features != lb.dataset.features || schema(la) != schema(lb)) {
                return Some(la.name.clone());
            }
        }
    }
    None
}

/// Why command `label` can't edit streamed layer `layer`, refused up front or undone after.
fn streamed_refusal(label: &str, layer: &str) -> String {
    format!("{label}: {layer} is read in place from its file; Load All Features (layer.loadAll) to edit it")
}

/// Commands that change a layer's features or fields, refused on streamed layers (whose features
/// are a window read again from the file as the view moves, so edits would be lost).
fn edits_features(id: &str) -> bool {
    const SETTINGS: &[&str] =
        &["edit.enable", "edit.settings", "edit.status", "edit.template", "edit.snapping", "edit.snap", "edit.tools", "edit.copy"];
    id.starts_with("edit.") && !SETTINGS.contains(&id)
}

impl Session {
    pub fn new(project: Project) -> Self {
        // Cheap: the suite's tools load the first time a tool list is needed.
        #[cfg(feature = "geolibre")]
        rustgis_geolibre::register();
        rustgis_plugins::tools::register();
        // FlatGeobuf and GeoParquet by their web address are read through the tile fetchers.
        render::rustgis_tiles::remote::install();
        Session {
            project,
            path: None,
            dirty: false,
            sources: sources::Sources::default(),
            connections: connections::Connections::default(),
            raster_defaults: rustgis_map::RasterDefaults::default(),
            registry: Registry { commands: cmd::all() },
            revision: 1,
            opened: 0,
            tool: Tool::Explore,
            active_layer: None,
            layer_selection: Vec::new(),
            layer_selection_of: None,
            view_size: (1200.0, 720.0),
            style_view: None,
            notices: Vec::new(),
            notices_given: 0,
            table_layer: None,
            table_standalone: None,
            clipboard: None,
            layer_clipboard: None,
            keep_tool: false,
            active_layout: None,
            trusted_tasks: Default::default(),
            trusted_models: Default::default(),
            model_run: None,
            project_starts: 0,
            trusted_notebooks: Default::default(),
            notebook_running: false,
            history_from_file: 0,
            active_report: None,
            active_presentation: None,
            active_notebook: None,
            edit_counts: std::collections::HashMap::new(),
            topology_errors: Vec::new(),
            measurements: Vec::new(),
            auto_cache: false,
            undo: Vec::new(),
            redo: Vec::new(),
            merge: None,
            merged: None,
            put_aside: None,
            depth: 0,
            restorable: 0,
            actor: None,
            back: Vec::new(),
            forward: Vec::new(),
            last_camera: None,
            history_projection: String::new(),
            pending: Vec::new(),
            next_pending: 0,
            #[cfg(feature = "geolibre")]
            started: Vec::new(),
            plugin_runs: Vec::new(),
            plugin_panels: std::collections::BTreeMap::new(),
            downloading: Vec::new(),
            downloaded: Vec::new(),
            url_requests: Vec::new(),
            request_only: false,
            plugin_retries: Vec::new(),
            retry_tries: 0,
            quiet_failure: false,
            #[cfg(not(target_arch = "wasm32"))]
            sql_results: Vec::new(),
            trend_results: Vec::new(),
        }
    }

    /// Keeps a query's result the app ran off the frame, for `layer.addSql {"ran": id}`, with the
    /// query and database it came from: the command takes it only for that same query, so the
    /// layer's saved query is the one that made it. A few at a time: past that many, the oldest
    /// are forgotten.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn sql_ran(&mut self, sql: &str, database: Option<&str>, out: rustgis_data::source::duckdb::Output) -> u64 {
        self.next_pending = self.next_pending.wrapping_add(1);
        if self.sql_results.len() >= 8 {
            self.sql_results.remove(0);
        }
        let database = database.and_then(|d| cmd::layer::database_param(&serde_json::json!({"database": d}))).map(|d| d.label());
        self.sql_results.push((self.next_pending, sql.to_string(), database, out));
        self.next_pending
    }

    /// Starts a data download for this project (the front end fetches it): its id, for
    /// [`Session::download_landed`].
    pub fn download_started(&mut self) -> u64 {
        self.next_pending = self.next_pending.wrapping_add(1);
        // A few at a time: past that many, the oldest are forgotten.
        if self.downloading.len() >= 64 {
            self.downloading.remove(0);
        }
        self.downloading.push(self.next_pending);
        self.next_pending
    }

    /// Hands over a download's bytes, for `layer.addDownload {"id"}`; `false` when it was dropped
    /// (another project was opened meanwhile).
    pub fn download_landed(&mut self, id: u64, bytes: Vec<u8>) -> bool {
        let Some(i) = self.downloading.iter().position(|d| *d == id) else { return false };
        self.downloading.remove(i);
        self.downloaded.push((id, bytes));
        true
    }

    /// Forgets a download that failed.
    pub fn download_failed(&mut self, id: u64) {
        self.downloading.retain(|d| *d != id);
    }

    /// Forgets a download the front end opens itself (a project by its address), saying whether
    /// it still belongs to this project: `false` when another project was opened meanwhile.
    pub fn download_done(&mut self, id: u64) -> bool {
        let current = self.downloading.contains(&id);
        self.download_failed(id);
        current
    }

    /// Keeps a `layer.addUrl` request for the front end to download off the frame and add when
    /// it lands (the browser, where a command may not wait): the command's answer, which says
    /// so. An address asked for again while it waits is asked for once, and past a few waiting
    /// requests the next is refused rather than silently dropping one already promised.
    pub fn request_url(&mut self, p: &serde_json::Value) -> Result<serde_json::Value, String> {
        let url = p.get("url").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
        if !self.url_requests.iter().any(|q| q.get("url").and_then(serde_json::Value::as_str) == Some(url.as_str())) {
            if self.url_requests.len() >= MAX_URL_REQUESTS {
                return Err(format!("{url}: {MAX_URL_REQUESTS} downloads are already waiting; try again when they have landed"));
            }
            self.url_requests.push(p.clone());
        }
        self.request_only = true;
        Ok(serde_json::json!({"pending": true, "url": url}))
    }

    /// The `layer.addUrl` requests waiting for the front end ([`Session::request_url`]), taken.
    pub fn take_url_requests(&mut self) -> Vec<serde_json::Value> {
        std::mem::take(&mut self.url_requests)
    }

    /// Whether background work still belongs to this project.
    pub fn download_is_pending(&self, id: u64) -> bool {
        self.downloading.contains(&id)
    }

    pub fn map(&self) -> &Map {
        self.project.map()
    }
    pub fn map_mut(&mut self) -> &mut Map {
        self.project.map_mut()
    }

    /// Moves the map to the view of a style added with `"view": true` once the style has loaded,
    /// or to an archive's bounds once its header is read (the UI asks each frame). True when the
    /// map moved.
    pub fn follow_style_view(&mut self) -> bool {
        cmd::tiles::follow_style_view(self, web_time::Duration::ZERO)
    }

    /// Runs a command by id. Mutating commands snapshot the project for undo; an error or a
    /// panic restores it.
    pub fn run(&mut self, id: &str, params: &Value) -> CmdResult {
        // The merge key asked for ([`Session::merge_undo`]) goes to this command if it runs, and is
        // gone if it's refused first, so it can't merge a later command into the last step.
        let merge = if self.depth == 0 { self.merge.take() } else { None };
        // A style that loaded since: its view first, so the command sees where the map is.
        if self.style_view.is_some() {
            self.follow_style_view();
        }
        if id.starts_with(rustgis_plugins::PREFIX) {
            self.merge = merge;
            return self.run_plugin(id, params);
        }
        let Some(&spec) = self.registry.get(id) else { return Err(format!("unknown command '{id}'")) };
        // Keys the command does not read: said, so a script or an agent can correct itself.
        if let (Value::Object(given), Some(known)) = (params, spec.param_keys()) {
            // Bounded: the keys come from the caller (the first few, each clipped).
            let unknown: Vec<&String> = given.keys().filter(|k| !known.contains(&k.as_str())).collect();
            if !unknown.is_empty() {
                let mut said: Vec<String> = unknown.iter().take(5).map(|k| format!("'{}'", clip(k, 40))).collect();
                if unknown.len() > 5 {
                    said.push(format!("{} more", unknown.len() - 5));
                }
                let reads = if known.is_empty() { "no parameters".to_string() } else { known.join(", ") };
                self.notify(format!("{id} ignores {} (it reads {reads})", said.join(", ")), false);
            }
        }
        // Layer conditions apply to the layer the parameters name (the first of a list), falling
        // back to the active one.
        let named = match params.get("layer") {
            Some(serde_json::Value::Array(list)) => list.first().and_then(|v| self.layer_id(&serde_json::json!({"layer": v})).ok()),
            Some(_) => self.layer_id(params).ok(),
            None => None,
        };
        let active = self.active_layer;
        if named.is_some() {
            self.active_layer = named;
        }
        // Restored on every path, even if a condition were to panic.
        let enabled = spec.enabled;
        let blocked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| enabled(self))).unwrap_or(Some("unavailable"));
        self.active_layer = active;
        if let Some(why) = blocked {
            return Err(format!("{}: {why}", spec.label));
        }
        let (run, mutates, label) = (spec.run, spec.mutates, spec.label);
        if let Some(e) = self.streamed_edit(id, mutates, label, named.or(active)) {
            // Said, as a failure in `guarded` is: this returns before it (#468).
            self.notify(e.clone(), true);
            return Err(e);
        }
        self.merge = merge;
        let res = self.guarded(label, mutates, |s| run(s, params));
        // What was added, for the Data Sources pane's Recent list.
        if res.is_ok()
            && let Some(src) = sources::added_by(id, params)
        {
            self.sources.note(src);
        }
        res
    }

    /// Why a command can't run on `layer`: it edits features, and the layer is read in place from
    /// its file, whose next read would lose the edit.
    pub(crate) fn streamed_edit(&self, id: &str, mutates: bool, label: &str, layer: Option<u32>) -> Option<String> {
        if !(mutates && edits_features(id)) {
            return None;
        }
        let l = layer.and_then(|l| self.map().layer(l)).filter(|l| l.is_streamed())?;
        Some(streamed_refusal(label, &l.name))
    }

    /// Runs `f` as the command `label`: when it `mutates`, the project is snapshotted for undo
    /// and restored if `f` fails or panics.
    pub(crate) fn guarded(&mut self, label: &'static str, mutates: bool, f: impl FnOnce(&mut Session) -> CmdResult) -> CmdResult {
        let snapshot = mutates.then(|| self.project.clone());
        // The merge key asked for applies to this command only, whatever it does; and to a
        // command run by itself, not to those it runs.
        let merge = if self.depth == 0 { self.merge.take() } else { None };
        let open_layout = self.active_layout;
        // The project's generation rolls back with it: a project opened part way (a task's step)
        // and then undone is the one before, still open.
        let opened = self.opened;
        self.depth += 1;
        self.restorable += u32::from(mutates);
        // Edit counts roll back with the project when a command fails part way.
        let counts = mutates.then(|| self.edit_counts.clone());
        self.request_only = false;
        self.quiet_failure = false;
        // Requests a failed command left for the front end go with its other changes.
        let requests = self.url_requests.len();
        let said = self.notices_given;
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)))
            .unwrap_or_else(|_| Err(format!("{label} failed unexpectedly; the project was restored")));
        self.depth = self.depth.saturating_sub(1);
        self.restorable = self.restorable.saturating_sub(u32::from(mutates));
        // A command that changed a streamed layer's features or fields (not by reading its
        // source) would lose the change at the next read: undone, with a way to edit it instead.
        let res = match (&res, &snapshot) {
            (Ok(_), Some(snap)) => match edited_streamed(snap, &self.project) {
                Some(name) => Err(streamed_refusal(label, &name)),
                None => res,
            },
            _ => res,
        };
        match (&res, snapshot) {
            // A command that only left a request for the front end (a download in the browser,
            // [`Session::request_url`]) changed nothing: no undo step, nothing to save yet. The
            // session's own flag says so, not the answer, which a plugin's command could forge.
            (Ok(_), Some(_)) if std::mem::take(&mut self.request_only) => {}
            (Ok(_), Some(mut snap)) => {
                // An edit may change the dates the time slider's window filters on: bound layers
                // whose features or fields the command changed (features are shared with the
                // snapshot until written) take the window again.
                for (i, m) in self.project.maps.iter_mut().enumerate() {
                    // No slider (a layer pasted from a map with one): no window either.
                    let window = m.time.as_ref().map(rustgis_map::time::TimeSlider::window);
                    // Layer ids are a map's own: compared within the same map (any layer, when
                    // maps came or went).
                    let before = snap.maps.get(i).filter(|o| o.name == m.name).map(|o| o.layers.as_slice()).unwrap_or_default();
                    for l in m.layers.iter_mut().filter(|l| l.time.is_some()) {
                        let same = before.iter().find(|o| o.id == l.id).is_some_and(|o| {
                            o.dataset.features.shares(&l.dataset.features) && o.dataset.fields == l.dataset.fields && o.time.is_some()
                        });
                        let rustgis_map::Layer { time, dataset, .. } = l;
                        if let Some(b) = time.as_mut().filter(|_| !same) {
                            b.apply(dataset, window);
                        }
                    }
                }
                // Undo keeps streamed layers' sources, not their windows: they read the view again.
                strip_windows(&mut snap);
                // A step made under the same merge key as the last one is part of it: undo goes
                // back to before the first of them. Only a command run by itself merges.
                let key = merge;
                if key.is_none() || key != self.merged || self.undo.is_empty() {
                    self.undo.push((snap, open_layout, label, self.actor.clone()));
                    if self.undo.len() > UNDO_LIMIT {
                        self.undo.remove(0);
                    }
                }
                if self.depth == 0 {
                    self.merged = key;
                }
                self.redo.clear();
                self.dirty = true;
            }
            (Err(_), Some(snap)) => {
                // The project as it was before the command, so the layout open then, whatever
                // the command opened on the way (a task's steps).
                self.project = snap;
                self.active_layout = open_layout;
                // Replaced during the command: what it held beside itself (its undo history, its
                // trust, its unsaved state) comes back with it.
                if self.opened != opened {
                    self.give_back(opened);
                }
                self.opened = opened;
                if let Some(c) = counts {
                    self.edit_counts = c;
                }
                // What it said it had done ("Deleted 3 features") didn't happen: those notices
                // go; its errors stay.
                let new = usize::try_from(self.notices_given.saturating_sub(said)).unwrap_or(usize::MAX).min(self.notices.len());
                let start = self.notices.len() - new;
                let errors: Vec<Notice> = self.notices.drain(start..).filter(|n| n.error).collect();
                self.notices_given -= (new - errors.len()) as u64;
                self.notices.extend(errors);
            }
            _ => {}
        }
        if res.is_err() {
            self.url_requests.truncate(requests);
        }
        let quiet = std::mem::take(&mut self.quiet_failure);
        if let Err(e) = &res
            && !quiet
        {
            self.notify(e.clone(), true);
        }
        // The command, and those it ran, are done: a replaced project stays replaced.
        if self.depth == 0 {
            self.put_aside = None;
        }
        self.fix_active_layer();
        self.revision = self.revision.wrapping_add(1);
        res
    }

    /// Puts aside what the project about to be replaced holds beside itself (`reset`), while a
    /// command that restores the project if it fails runs: the first replacement in it only, as
    /// that is the project a failure gives back. Elsewhere (opening a project from the menu) it is
    /// dropped as before, so nothing is kept longer.
    pub(crate) fn put_aside(&mut self) {
        if self.restorable == 0 || self.put_aside.is_some() {
            return;
        }
        self.put_aside = Some(Box::new(self.take_project_state()));
    }

    /// Takes out what the project holds beside itself ([`PutAside`]), leaving what a project
    /// just started holds: what `clear_history` forgets is moved out, the rest (its file, layer,
    /// tool…, which opening sets) is copied.
    fn take_project_state(&mut self) -> PutAside {
        use std::mem::take;
        (self.merge, self.merged) = (None, None);
        PutAside {
            opened: self.opened,
            undo: take(&mut self.undo),
            redo: take(&mut self.redo),
            back: take(&mut self.back),
            forward: take(&mut self.forward),
            last_camera: self.last_camera.take(),
            history_projection: self.history_projection.clone(),
            dirty: std::mem::replace(&mut self.dirty, false),
            trusted_tasks: take(&mut self.trusted_tasks),
            trusted_models: take(&mut self.trusted_models),
            trusted_notebooks: take(&mut self.trusted_notebooks),
            project_starts: self.project_starts,
            history_from_file: self.history_from_file,
            active_layer: self.active_layer,
            // Moved, not copied: the next project's layers reuse the same ids.
            layer_selection: take(&mut self.layer_selection),
            layer_selection_of: self.layer_selection_of.take(),
            table_layer: self.table_layer,
            table_standalone: self.table_standalone,
            tool: self.tool,
            measurements: take(&mut self.measurements),
            topology_errors: take(&mut self.topology_errors),
            path: self.path.clone(),
            model_run: self.model_run.take(),
            pending: take(&mut self.pending),
            #[cfg(feature = "geolibre")]
            started: take(&mut self.started),
            plugin_runs: take(&mut self.plugin_runs),
            downloading: take(&mut self.downloading),
            downloaded: take(&mut self.downloaded),
            url_requests: take(&mut self.url_requests),
            plugin_retries: take(&mut self.plugin_retries),
            style_view: self.style_view.take(),
            #[cfg(not(target_arch = "wasm32"))]
            sql_results: take(&mut self.sql_results),
            trend_results: take(&mut self.trend_results),
            edit_counts: take(&mut self.edit_counts),
        }
    }

    /// Gives back what project `opened` held beside itself, when it is restored. Every field is
    /// named (no `..`): one added to [`PutAside`] and not given back doesn't compile.
    fn give_back(&mut self, opened: u64) {
        let Some(a) = self.put_aside.take_if(|a| a.opened == opened) else { return };
        let PutAside {
            opened: _,
            undo,
            redo,
            back,
            forward,
            last_camera,
            history_projection,
            dirty,
            trusted_tasks,
            trusted_models,
            trusted_notebooks,
            project_starts,
            history_from_file,
            active_layer,
            layer_selection,
            layer_selection_of,
            table_layer,
            table_standalone,
            tool,
            measurements,
            topology_errors,
            path,
            model_run,
            pending,
            #[cfg(feature = "geolibre")]
            started,
            plugin_runs,
            downloading,
            downloaded,
            url_requests,
            plugin_retries,
            style_view,
            #[cfg(not(target_arch = "wasm32"))]
            sql_results,
            trend_results,
            edit_counts,
        } = *a;
        (self.undo, self.redo, self.back, self.forward, self.last_camera) = (undo, redo, back, forward, last_camera);
        self.history_projection = history_projection;
        (self.dirty, self.project_starts, self.history_from_file) = (dirty, project_starts, history_from_file);
        (self.trusted_tasks, self.trusted_models, self.trusted_notebooks) = (trusted_tasks, trusted_models, trusted_notebooks);
        (self.active_layer, self.table_layer, self.table_standalone, self.tool) = (active_layer, table_layer, table_standalone, tool);
        (self.layer_selection, self.layer_selection_of) = (layer_selection, layer_selection_of);
        (self.measurements, self.topology_errors, self.edit_counts) = (measurements, topology_errors, edit_counts);
        (self.path, self.model_run, self.style_view) = (path, model_run, style_view);
        (self.pending, self.plugin_runs, self.plugin_retries) = (pending, plugin_runs, plugin_retries);
        (self.downloading, self.downloaded, self.url_requests) = (downloading, downloaded, url_requests);
        self.trend_results = trend_results;
        #[cfg(feature = "geolibre")]
        {
            self.started = started;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.sql_results = sql_results;
        }
    }

    pub fn notify(&mut self, text: impl Into<String>, error: bool) {
        self.notices.push(Notice { text: text.into(), error });
        self.notices_given += 1;
        if self.notices.len() > 200 {
            self.notices.remove(0);
        }
    }

    /// A mark to pass to [`Session::notices_since`].
    pub fn notice_mark(&self) -> u64 {
        self.notices_given
    }

    /// The notices given since `mark` was taken (those still kept): what a command said beside its
    /// result, for front ends without a status bar (the CLI, the MCP server, the AI Assistant).
    pub fn notices_since(&self, mark: u64) -> &[Notice] {
        let n = usize::try_from(self.notices_given.saturating_sub(mark)).unwrap_or(usize::MAX).min(self.notices.len());
        self.notices.get(self.notices.len() - n..).unwrap_or_default()
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    /// Who made the step undo would take back: `Some(None)` for the user, `Some(Some(name))`
    /// for an agent ([`Session::actor`]), `None` when there is nothing to undo.
    pub fn undo_actor(&self) -> Option<Option<&str>> {
        self.undo.last().map(|u| u.3.as_deref())
    }
    /// Who made the step redo would bring back, as [`Session::undo_actor`].
    pub fn redo_actor(&self) -> Option<Option<&str>> {
        self.redo.last().map(|u| u.3.as_deref())
    }
    pub fn undo_label(&self) -> Option<&'static str> {
        self.undo.last().map(|u| u.2)
    }
    pub fn redo_label(&self) -> Option<&'static str> {
        self.redo.last().map(|u| u.2)
    }

    /// Makes the next command's undo step part of the last one, when that was made under the same
    /// `key`: a held arrow key nudging a layout element is one step to undo, not one per repeat
    /// (and doesn't push the work before it out of the history). Any other change in between,
    /// or an undo or redo, starts a new step.
    pub fn merge_undo(&mut self, key: impl Into<String>) {
        self.merge = Some(key.into());
    }

    /// Forgets undo/redo and extent history and marks the project clean (after new/open, or a
    /// front end setting up how the project starts): what [`PutAside`] holds, dropped.
    pub fn clear_history(&mut self) {
        drop(self.take_project_state());
        self.project_starts = self.project_starts.wrapping_add(1);
    }

    pub(crate) fn undo(&mut self) -> bool {
        self.merged = None;
        let Some((p, saved, label, actor)) = self.undo.pop() else { return false };
        let (open, was) = (self.open_layout_name(), self.active_layout);
        let cur = std::mem::replace(&mut self.project, p);
        self.project.keep_table_ids(&cur);
        self.reopen_layout(open, saved);
        self.keep_time_position(&cur);
        self.redo.push((cur, was, label, actor));
        self.touch_all_geometry();
        true
    }

    pub(crate) fn redo(&mut self) -> bool {
        self.merged = None;
        let Some((p, saved, label, actor)) = self.redo.pop() else { return false };
        let (open, was) = (self.open_layout_name(), self.active_layout);
        let cur = std::mem::replace(&mut self.project, p);
        self.project.keep_table_ids(&cur);
        self.reopen_layout(open, saved);
        self.keep_time_position(&cur);
        self.undo.push((cur, was, label, actor));
        self.touch_all_geometry();
        true
    }

    /// After undo or redo swaps the project: each map's time slider stays on the step it was
    /// moved to (moving through time isn't an undo step), its other settings as restored.
    fn keep_time_position(&mut self, before: &Project) {
        for (m, old) in self.project.maps.iter_mut().zip(&before.maps).filter(|(m, o)| m.name == o.name) {
            if let (Some(t), Some(at)) = (m.time.as_mut(), old.time.as_ref().map(|t| t.current)) {
                t.go_to(at);
                m.apply_time();
            }
        }
        // Zarr layers that follow the slider read the step it is back at.
        cmd::zarr::follow_time(self);
    }

    fn open_layout_name(&self) -> Option<String> {
        self.active_layout.and_then(|i| self.project.layouts.get(i)).map(|l| l.name.clone())
    }

    /// After undo or redo swaps the project: the open layout stays open wherever it now is (so
    /// redo doesn't move the view to a layout it brings back); one the swap took away gives way to
    /// the layout open with the restored project (`saved`). With no layout open, none opens.
    fn reopen_layout(&mut self, open: Option<String>, saved: Option<usize>) {
        let ls = &self.project.layouts;
        self.active_layout = open.and_then(|n| ls.iter().position(|l| l.name.eq_ignore_ascii_case(&n)).or(saved.filter(|i| *i < ls.len())));
    }

    fn touch_all_geometry(&mut self) {
        for m in &mut self.project.maps {
            for l in &mut m.layers {
                l.touch_geometry();
            }
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// Drops the extent history when the map has switched projection since it was recorded (by a
    /// command, undo or opening a project): its cameras are in the old projection's units.
    fn check_history_projection(&mut self) {
        let code = self.map().projection.code();
        if code != self.history_projection {
            self.back.clear();
            self.forward.clear();
            self.last_camera = None;
            self.history_projection = code;
        }
    }

    /// How many changes can be undone.
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// Moves every other map to the active map's centre and scale (View › Link Views).
    pub fn sync_linked_views(&mut self) {
        let (c, scale, active) = (self.map().center_lonlat(), self.scale(), self.project.active_map);
        for (i, m) in self.project.maps.iter_mut().enumerate() {
            if i == active || m.projection.is_globe() {
                continue;
            }
            if let Some(mc) = m.to_map(c) {
                m.camera.center = mc;
                m.set_scale(scale);
            }
        }
    }

    /// Records the camera in the previous/next extent history when it settles somewhere new.
    pub fn record_extent(&mut self) {
        if self.project.link_views {
            self.sync_linked_views();
        }
        self.check_history_projection();
        let cam = self.map().camera;
        if let Some(last) = self.last_camera {
            let moved = (last.center.x - cam.center.x).abs() > last.res * 2.0
                || (last.res / cam.res - 1.0).abs() > 0.01
                || rustgis_map::camera::normalize_bearing(last.bearing - cam.bearing).abs() > 0.5
                || (last.pitch - cam.pitch).abs() > 0.5;
            if !moved {
                return;
            }
            self.back.push(last);
            if self.back.len() > 50 {
                self.back.remove(0);
            }
            self.forward.clear();
        }
        self.last_camera = Some(cam);
    }

    pub(crate) fn extent_back(&mut self) -> bool {
        self.check_history_projection();
        let Some(c) = self.back.pop() else { return false };
        let cur = self.map().camera;
        self.forward.push(cur);
        self.map_mut().camera = c;
        self.last_camera = Some(c);
        true
    }

    pub(crate) fn extent_forward(&mut self) -> bool {
        self.check_history_projection();
        let Some(c) = self.forward.pop() else { return false };
        let cur = self.map().camera;
        self.back.push(cur);
        self.map_mut().camera = c;
        self.last_camera = Some(c);
        true
    }

    pub fn can_extent_back(&self) -> bool {
        !self.back.is_empty() && self.map().projection.code() == self.history_projection
    }
    pub fn can_extent_forward(&self) -> bool {
        !self.forward.is_empty() && self.map().projection.code() == self.history_projection
    }

    /// The layers picked together in the Layers pane, top of the drawing order first: the layer
    /// selection if it was made around the current active layer, else that layer alone (none
    /// when there is no active layer). Layers the map no longer has are left out.
    pub fn selected_layers(&self) -> Vec<u32> {
        let map = self.map();
        let picked: &[u32] = if self.layer_selection_of == self.active_layer { &self.layer_selection } else { &[] };
        map.layers.iter().map(|l| l.id).filter(|id| picked.contains(id) || self.active_layer == Some(*id)).collect()
    }

    /// Makes `ids` the layer selection with `active` the active layer (one of them, else none).
    pub(crate) fn set_layer_selection(&mut self, mut ids: Vec<u32>, active: Option<u32>) {
        let mut seen = std::collections::HashSet::new();
        ids.retain(|id| seen.insert(*id));
        let active = active.filter(|a| ids.contains(a)).or_else(|| ids.last().copied());
        self.active_layer = active;
        self.layer_selection = ids;
        self.layer_selection_of = active;
    }

    /// Keeps `active_layer` pointing at a layer of the active map.
    pub(crate) fn fix_active_layer(&mut self) {
        if self.active_layout.is_some_and(|i| i >= self.project.layouts.len()) {
            self.active_layout = None;
        }
        if self.active_report.is_some_and(|i| i >= self.project.reports.len()) {
            self.active_report = None;
        }
        if self.active_presentation.is_some_and(|i| i >= self.project.presentations.len()) {
            self.active_presentation = None;
        }
        if self.active_notebook.is_some_and(|i| i >= self.project.notebooks.len()) {
            self.active_notebook = None;
        }
        let map = self.project.map();
        if let Some(id) = self.active_layer
            && map.layer(id).is_none()
        {
            self.active_layer = map.layers.first().map(|l| l.id);
        }
        // A layer selection made around another active layer is over, for good: the active layer
        // coming back later doesn't bring it back.
        if self.layer_selection_of != self.active_layer {
            self.layer_selection.clear();
            self.layer_selection_of = self.active_layer;
        }
        if let Some(id) = self.table_layer
            && map.layer(id).is_none()
        {
            self.table_layer = None;
        }
        if self.table_standalone.is_some_and(|id| self.project.table(id).is_none()) {
            self.table_standalone = None;
        }
    }

    /// A colour ramp by name: one of the built-in ones, else one of the project's custom ramps.
    pub fn ramp(&self, name: &str) -> Result<rustgis_map::Ramp, String> {
        rustgis_map::Ramp::from_name(name)
            .or_else(|| {
                self.project
                    .ramps
                    .iter()
                    .find(|r| r.name().eq_ignore_ascii_case(name.trim()))
                    .map(|r| rustgis_map::Ramp::Custom(Box::new(r.clone())))
            })
            .ok_or_else(|| format!("unknown colour scheme '{name}'"))
    }

    /// Resolves a `layer` parameter (name or id), defaulting to the active layer.
    pub fn layer_id(&self, p: &Value) -> Result<u32, String> {
        let map = self.map();
        match p.get("layer") {
            Some(Value::Number(n)) => n
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|id| map.layer(*id).is_some())
                .ok_or_else(|| format!("no layer with id {n}")),
            Some(Value::String(s)) => map
                .layer_by_name(s)
                .map(|l| l.id)
                .or_else(|| s.parse::<u32>().ok().filter(|id| map.layer(*id).is_some()))
                .ok_or_else(|| format!("no layer named '{s}'")),
            _ => {
                self.active_layer.filter(|id| map.layer(*id).is_some()).ok_or_else(|| "select a layer in the Layers pane first".to_string())
            }
        }
    }

    /// Resolves a `table` parameter to a standalone table's position in the project: by its id,
    /// or by text as [`project::find_table`] reads it (a name, else an id).
    pub fn table_index(&self, p: &Value) -> Result<usize, String> {
        let tables = &self.project.tables;
        match p.get("table") {
            Some(Value::Number(n)) => n
                .as_u64()
                .and_then(|id| tables.iter().position(|t| u64::from(t.id) == id))
                .ok_or_else(|| format!("no standalone table with id {n}")),
            Some(Value::String(s)) => project::find_table(tables, s).ok_or_else(|| format!("no standalone table named '{s}'")),
            _ => Err("give the table's name or id (\"table\")".to_string()),
        }
    }

    /// The scale denominator now.
    pub fn scale(&self) -> f64 {
        self.map().scale()
    }

    /// The map's zoom level, as web maps count it (512-pixel tiles, MapLibre's zoom); None when
    /// the map's centre is no place on Earth.
    pub fn zoom(&self) -> Option<f64> {
        cmd::navigate::view_of(self.map()).map(|(_, z)| z)
    }

    /// Fits the camera to a lon/lat extent in the current view size.
    pub fn zoom_to(&mut self, e: &rustgis_geom::Extent) {
        let (w, h) = self.view_size;
        let m = self.map_mut();
        if m.projection.is_globe() {
            rustgis_map::Globe::fit(&mut m.camera, e, w, h);
        } else {
            m.fit_lonlat(e, w, h);
        }
        self.record_extent();
    }

    pub fn title(&self) -> String {
        self.project.name.clone()
    }
}
