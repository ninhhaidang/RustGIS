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
#[cfg(not(target_arch = "wasm32"))]
pub mod prefs;
pub mod project;
pub mod project_log;
pub mod s3_connections;
pub mod s3_keys;
#[cfg(test)]
mod s3_saved_tests;
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

/// A download that landed read already: its layer, and what reading it skipped or noticed.
pub(crate) type LandedRead = Box<(rustgis_data::Dataset, rustgis_data::ReadReport)>;

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
    let (mut depth, mut start, mut q) = (0usize, 0, Quotes::default());
    for (i, c) in inner.char_indices() {
        if !q.read(c) {
            continue;
        }
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
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
    let (key, after) = rest.split_at(string_end(rest)?);
    let optional = after.get(1..).is_some_and(|a| a.trim_start().starts_with('?'));
    Some(Param { key, optional, text })
}

/// Quoted text read one char at a time (`read`), as JSON has it: a `"` opens a string and the
/// next one not escaped closes it; a backslash escapes the char after it only in a string. The
/// one rule for everything that reads a description or a Script line by hand (#741).
#[derive(Clone, Copy, Debug, Default)]
pub struct Quotes {
    quoted: bool,
    escaped: bool,
}

impl Quotes {
    /// Inside a string, after its opening quote.
    pub fn in_string() -> Self {
        Quotes { quoted: true, escaped: false }
    }

    /// Reads `c`: whether it's code, outside every string (a string's quotes aren't).
    pub fn read(&mut self, c: char) -> bool {
        if !self.quoted {
            self.quoted = c == '"';
            return !self.quoted;
        }
        if self.escaped {
            self.escaped = false;
        } else if c == '\\' {
            self.escaped = true;
        } else if c == '"' {
            self.quoted = false;
        }
        false
    }

    /// Whether what was read ends in a string.
    pub fn quoted(&self) -> bool {
        self.quoted
    }
}

/// Where the string whose text starts `s` ends: the byte index of its closing quote. None when
/// nothing closes it.
pub fn string_end(s: &str) -> Option<usize> {
    let mut q = Quotes::in_string();
    for (i, c) in s.char_indices() {
        q.read(c);
        if !q.quoted() {
            return Some(i);
        }
    }
    None
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
                let end = desc.get(i + 1..).and_then(string_end).map_or(bytes.len(), |n| i + 1 + n);
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
    SelectPolygon,
    SelectLasso,
    SelectRadius,
    ZoomIn,
    Measure,
    MeasureArea,
    CreatePoint,
    CreateLine,
    CreatePolygon,
    /// Moves, inserts and deletes the vertices of the selected features.
    EditVertices,
    /// Draws a line across the selected polygons to cut them (`edit.cutPolygon`).
    CutPolygon,
    /// Draws a line across the selected feature's boundary to reshape it (`edit.reshape`).
    Reshape,
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
            Tool::SelectPolygon => "selectPolygon",
            Tool::SelectLasso => "selectLasso",
            Tool::SelectRadius => "selectRadius",
            Tool::ZoomIn => "zoomIn",
            Tool::Measure => "measure",
            Tool::MeasureArea => "measureArea",
            Tool::CreatePoint => "createPoint",
            Tool::CreateLine => "createLine",
            Tool::CreatePolygon => "createPolygon",
            Tool::EditVertices => "editVertices",
            Tool::CutPolygon => "cutPolygon",
            Tool::Reshape => "reshape",
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
            Tool::SelectPolygon,
            Tool::SelectLasso,
            Tool::SelectRadius,
            Tool::ZoomIn,
            Tool::Measure,
            Tool::MeasureArea,
            Tool::CreatePoint,
            Tool::CreateLine,
            Tool::CreatePolygon,
            Tool::EditVertices,
            Tool::CutPolygon,
            Tool::Reshape,
        ]
        .into_iter()
        .find(|t| t.code().eq_ignore_ascii_case(s.trim()))
    }
    pub fn label(self) -> &'static str {
        match self {
            Tool::Explore => "Explore",
            Tool::Select => "Select",
            Tool::SelectPolygon => "Select by Polygon",
            Tool::SelectLasso => "Select by Lasso",
            Tool::SelectRadius => "Select by Radius",
            Tool::ZoomIn => "Zoom In",
            Tool::Measure => "Measure Distance",
            Tool::MeasureArea => "Measure Area",
            Tool::CreatePoint => "Create Point",
            Tool::CreateLine => "Create Line",
            Tool::CreatePolygon => "Create Polygon",
            Tool::EditVertices => "Edit Vertices",
            Tool::CutPolygon => "Cut Polygon",
            Tool::Reshape => "Reshape",
            Tool::Graphic(shape) => GRAPHIC_TOOLS.iter().find(|g| g.0 == shape).map_or("Point", |g| g.2),
        }
    }
    pub fn creates(self) -> bool {
        matches!(self, Tool::CreatePoint | Tool::CreateLine | Tool::CreatePolygon)
    }
    /// Whether the tool draws a line or polygon that edits features: vertices snap, and can
    /// follow an edge with Trace.
    pub fn draws_edit(self) -> bool {
        matches!(self, Tool::CreateLine | Tool::CreatePolygon | Tool::CutPolygon | Tool::Reshape)
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

struct ProjectFiles {
    identity: u64,
    path: Option<PathBuf>,
    opened_package: Option<PathBuf>,
}

/// An undo or redo step: the project before it, the layout open then, its label, who made it,
/// and which state of the project it is ([`Session::dirty`]). File origins accompany changes
/// made by the command only: undoing an ordinary edit must not undo a subsequent Save As.
type Step = (Project, Option<usize>, &'static str, Option<String>, u64, Option<ProjectFiles>);

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
    state: u64,
    saved: Option<u64>,
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
    opened_package: Option<PathBuf>,
    file_identity: u64,
    model_run: Option<model::RunReport>,
    /// Its work still under way (tool runs, downloads, plugin fetches, queries): the runs of
    /// the previous project mustn't land in the next one.
    pending: Vec<PendingRun>,
    file_outputs: Vec<cmd::gp::PendingFile>,
    #[cfg(feature = "geolibre")]
    started: Vec<(u64, rustgis_geolibre::Job, serde_json::Value, u64)>,
    plugin_runs: Vec<cmd::gp::StartedRun>,
    downloading: Vec<u64>,
    downloaded: Vec<(u64, Vec<u8>)>,
    landed_reads: Vec<(u64, LandedRead)>,
    url_requests: Vec<serde_json::Value>,
    plugin_retries: Vec<cmd::plugins::fetch::Retry>,
    /// A style of the project still loading, which mustn't move the next one's view.
    style_view: Option<(String, Camera)>,
    #[cfg(not(target_arch = "wasm32"))]
    sql_results: Vec<(u64, String, Option<String>, rustgis_data::source::duckdb::Output)>,
    trend_results: Vec<(u64, Vec<rustgis_data::raster::Raster>, usize)>,
    raster_results: Vec<(u64, cmd::workflows::Made)>,
    raster_runs: Vec<u64>,
    edit_counts: std::collections::HashMap<u32, cmd::design::EditCounts>,
}

pub struct Session {
    pub project: Project,
    pub path: Option<PathBuf>,
    /// What was run in this session (#797), whatever project was open: kept in memory only, so
    /// a saved or shared project carries none of it.
    pub log: project_log::SessionLog,
    /// Package this project was opened from, protected as an input without making it the save
    /// destination. Kept after Save As; cleared when a different project starts.
    opened_package: Option<PathBuf>,
    /// Undoable identity of the project owning these origins. Allocated from the unique state
    /// counter at reset; separate from `opened`, whose values must not be reused by undo.
    file_identity: u64,
    /// Which state of the project is open: a new one after each change, an earlier one again
    /// after undo. Each is given once in a session (`states` counts them).
    state: u64,
    states: u64,
    /// The state last saved, or opened from its file; `None` when no state of the open project is
    /// (recovered work). The project has unsaved changes while another is open ([`Session::dirty`]).
    saved: Option<u64>,
    /// Favourite, recent and saved data sources (kept with the app's preferences, not the
    /// project).
    pub sources: sources::Sources,
    /// Saved connections (`connection.*`): kept with the app's preferences, not the project; never a secret.
    pub connections: connections::Connections,
    /// The runtime token of each saved S3 connection's login in this session, by saved id (see
    /// `rustgis_tiles::s3::connect_saved`): given on the first successful login, kept after.
    pub(crate) s3_bindings: std::collections::BTreeMap<String, String>,
    /// Project sources that read an S3 bucket through a saved connection and wait for the user to
    /// choose which (see [`s3_connections`]); none is fetched meanwhile.
    pub s3_pending: Vec<s3_connections::PendingS3>,
    /// This run's connection credential store (desktop only), plus database passwords, selections
    /// and login requests. S3 uses the same store through `DatabaseLogins::secret_store`.
    /// Never saved; see [`connections::DatabaseLogins`].
    pub database_logins: connections::DatabaseLogins,
    /// How rasters added from now on are drawn (`raster.setDefaultStyle`; kept with the app's
    /// preferences, not the project).
    pub raster_defaults: rustgis_map::RasterDefaults,
    /// The favourite arrangement of publication layouts (`layout.arrangement`, #1379): a name or a
    /// corner for each surround, kept with the app's preferences, not the project.
    pub layout_favourite: serde_json::Value,
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
    /// Troubleshooting history, independent of the active pane, project files and undo.
    pub diagnostics: rustgis_data::diagnostics::History,
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
    /// The PostGIS database that layers loaded for editing work with (`pg.connect`): its
    /// connection, never with its password. Kept for the session, not in the project.
    pub postgis: Option<String>,
    /// Source layers' row versions that `pg.saveEdits` or `gpkg.saveEdits` saved over: a layer back on one
    /// (undone, or restored by a failed command, past a save) would write its edits again.
    /// Outside the project, so undo keeps it.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) saved_over: Vec<rustgis_map::source::Versions>,
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
    /// A task is executing: its steps may not start another task, even through scripts.
    /// Kept across project replacement until the active call returns; never part of undo.
    pub(crate) task_running: bool,
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
    /// Only the command's registered id, never its parameters. Nested commands restore this.
    diagnostic_source: Option<&'static str>,
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
    /// Tool files waiting for the front end to deliver and acknowledge.
    pub(crate) file_outputs: Vec<cmd::gp::PendingFile>,
    /// Exercises the browser delivery path with real tools in native engine tests.
    #[cfg(test)]
    queue_gp_files: bool,
    /// Asynchronous runs started and not yet complete: (id, job, resolved params, start time).
    #[cfg(feature = "geolibre")]
    pub(crate) started: Vec<(u64, rustgis_geolibre::Job, serde_json::Value, u64)>,
    /// Plugin tool runs started and not yet complete (`gp::plugin_start`).
    pub(crate) plugin_runs: Vec<cmd::gp::StartedRun>,
    /// Plugins' panels, by plugin id: the widgets' values and what the plugin last said about
    /// them (it keeps nothing between calls). Not saved with the project: a plugin's state
    /// command reads what it needs from the map when the panel opens.
    pub plugin_panels: std::collections::BTreeMap<String, rustgis_plugins::PanelState>,
    /// Data downloads of this project under way (Home › Add › Add Data › From URL), by id.
    downloading: Vec<u64>,
    /// Downloads that landed, waiting for `layer.addDownload`.
    pub(crate) downloaded: Vec<(u64, Vec<u8>)>,
    /// Downloads that landed already read (an ArcGIS layer's protocol buffer pages, joined),
    /// waiting for `layer.addDownload` too.
    pub(crate) landed_reads: Vec<(u64, LandedRead)>,
    /// Downloads a command asked for where nothing may wait (the browser): the parameters of
    /// `layer.addUrl` calls, for the front end to fetch and add (see [`Session::take_url_requests`]).
    pub(crate) url_requests: Vec<serde_json::Value>,
    /// Set by [`Session::request_url`] for [`Session::guarded`]: the command running changed
    /// nothing, it only left a request (a plugin's own answer can't claim that).
    request_only: bool,
    /// Set by [`Session::run_leaving_downloads`]: `layer.addUrl` leaves what it would wait for
    /// to the front end ([`Session::request_url`]), as in the browser.
    leave_downloads: bool,
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
    /// Rasters the app made on a thread for an Analysis workflow, waiting for its command with
    /// `{"ran"}` (see [`cmd::workflows::raster_land`]).
    pub(crate) raster_results: Vec<(u64, cmd::workflows::Made)>,
    /// The ids of the Analysis workflows running on a thread (see
    /// [`cmd::workflows::raster_start`]), apart from the downloads'.
    pub(crate) raster_runs: Vec<u64>,
    /// Set where nothing waits on a command but a shell (`rustgis-cli run`): long runs a command
    /// works out where it's called (a trend over a state from a data cube) take the app's larger
    /// bounds, as its panes do on a thread, and tell their progress (a share, 0 to 1, and 1 when
    /// one succeeds) through this. Python leaves it unset: a run there holds the interpreter's
    /// lock and can't be interrupted, so it keeps the Script pane's bounds.
    pub long_runs: Option<fn(&str, f64)>,
    /// Set where a person at a terminal waits on a command (`rustgis-cli run` with stdin and
    /// stderr a terminal), who can stop it with Ctrl-C: tools and rasters take no work cap
    /// ([`rustgis_analysis::control::Control::stoppable`]). A program that calls it (an agent, a
    /// CI job) may wait with a timeout, so there they keep their caps.
    pub stoppable: bool,
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

/// New features of a layer loaded for source editing never keep the key of a row it loaded (one
/// deleted since, whose id an editing tool can hand out again): saved, they would be that row,
/// changed. They take ids past every key instead. Layers whose rows were loaded or saved again by
/// the command (their versions replaced) are left alone.
fn fresh_source_ids(before: &Project, after: &mut Project) {
    for (mb, ma) in before.maps.iter().zip(after.maps.iter_mut()) {
        for la in ma.layers.iter_mut() {
            let Some(versions) = la.edit_versions() else { continue };
            let Some(lb) = mb.layers.iter().find(|l| l.id == la.id) else { continue };
            let same_rows = lb.edit_versions().is_some_and(|b| b.shares(versions));
            if !same_rows || la.dataset.features.shares(&lb.dataset.features) {
                continue;
            }
            // A layer bound to both sources (a hand-made project) reserves both baselines' keys.
            let also = la.postgis.as_ref().and(la.gpkg.as_ref()).map(|g| &g.versions);
            let loaded = |id: u64| versions.get(id).is_some() || also.is_some_and(|v| v.get(id).is_some());
            let had: std::collections::HashSet<u64> = lb.dataset.features.iter().map(|f| f.id).collect();
            let reused: Vec<usize> = (0..la.dataset.features.len())
                .filter(|&i| la.dataset.features.get(i).is_some_and(|f| !had.contains(&f.id) && loaded(f.id)))
                .collect();
            if reused.is_empty() {
                continue;
            }
            // Past the largest key that isn't negative (a negative key's bits are the largest).
            let mut next = la
                .dataset
                .features
                .iter()
                .map(|f| f.id)
                .chain(versions.iter().map(|(k, _)| k))
                .chain(also.into_iter().flat_map(|v| v.iter().map(|(k, _)| k)))
                .filter(|&k| k.cast_signed() >= 0)
                .max()
                .unwrap_or(0);
            for i in reused {
                next = next.saturating_add(1);
                if let Some(f) = la.dataset.features.get_mut(i) {
                    f.id = next;
                }
            }
        }
    }
}

/// A lock names each label by its feature's id, and [`rustgis_data::Dataset::push`] hands the
/// largest id out again once its feature is deleted: the new feature would take the lock's place
/// without having been placed (#1135). So a command that removes features removes their entries,
/// whichever way it removes them. Undo brings them back with the rest of the project. A streamed
/// layer holds only the window in view, so its other features aren't gone. It compares ids, so an
/// id a single command both removes and hands out stays locked: only restoring from an archive can.
fn forget_locks_of_removed_features(before: &Project, after: &mut Project) {
    for (i, m) in after.maps.iter_mut().enumerate() {
        let was = before.maps.get(i).filter(|o| o.name == m.name);
        let Some(lock) = m.label_lock.as_mut() else { continue };
        for l in m.layers.iter().filter(|l| !l.is_streamed()) {
            if was.and_then(|o| o.layers.iter().find(|o| o.id == l.id)).is_some_and(|o| o.dataset.features.shares(&l.dataset.features)) {
                continue;
            }
            // The lock's ids for this layer, less those it still has: sized by the lock, not the layer.
            let mut gone: std::collections::HashSet<u64> =
                lock.iter().filter(|((layer, ..), _)| *layer == l.id).map(|((_, id, ..), _)| *id).collect();
            if gone.is_empty() {
                continue;
            }
            for f in l.dataset.features.iter() {
                gone.remove(&f.id);
                if gone.is_empty() {
                    break;
                }
            }
            lock.retain(|((layer, id, ..), _)| *layer != l.id || !gone.contains(id));
        }
    }
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
        // The engine's tests never add Shapefile indexes to the user's cache (#1230).
        #[cfg(all(test, not(target_arch = "wasm32")))]
        rustgis_data::source::shp::use_temporary_index_dir();
        // Cheap: the suite's tools load the first time a tool list is needed.
        #[cfg(feature = "geolibre")]
        rustgis_geolibre::register();
        rustgis_plugins::tools::register();
        // FlatGeobuf and GeoParquet by their web address are read through the tile fetchers.
        render::rustgis_tiles::remote::install();
        rustgis_data::diagnostics::set_clock(|| {
            web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
        });
        Session {
            project,
            path: None,
            log: project_log::SessionLog::default(),
            opened_package: None,
            file_identity: 0,
            state: 0,
            states: 0,
            saved: Some(0),
            sources: sources::Sources::default(),
            connections: connections::Connections::default(),
            s3_bindings: std::collections::BTreeMap::new(),
            s3_pending: Vec::new(),
            database_logins: connections::DatabaseLogins::default(),
            raster_defaults: rustgis_map::RasterDefaults::default(),
            layout_favourite: serde_json::Value::String("classic".into()),
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
            diagnostics: rustgis_data::diagnostics::History::default(),
            notices_given: 0,
            table_layer: None,
            table_standalone: None,
            clipboard: None,
            layer_clipboard: None,
            keep_tool: false,
            active_layout: None,
            trusted_tasks: Default::default(),
            task_running: false,
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
            postgis: None,
            saved_over: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            merge: None,
            merged: None,
            put_aside: None,
            depth: 0,
            diagnostic_source: None,
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
            landed_reads: Vec::new(),
            url_requests: Vec::new(),
            file_outputs: Vec::new(),
            #[cfg(test)]
            queue_gp_files: false,
            request_only: false,
            leave_downloads: false,
            plugin_retries: Vec::new(),
            retry_tries: 0,
            quiet_failure: false,
            #[cfg(not(target_arch = "wasm32"))]
            sql_results: Vec::new(),
            trend_results: Vec::new(),
            raster_results: Vec::new(),
            raster_runs: Vec::new(),
            long_runs: None,
            stoppable: false,
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
        let landed = self.landed(id);
        if landed {
            self.downloaded.push((id, bytes));
        }
        landed
    }

    /// Hands over a download read already (an ArcGIS layer's protocol buffer pages, joined, see
    /// [`ogc::Step::Decoded`](rustgis_data::ogc::Step::Decoded)), for `layer.addDownload {"id"}`
    /// as [`Session::download_landed`] does.
    pub fn download_landed_read(&mut self, id: u64, dataset: rustgis_data::Dataset, report: rustgis_data::ReadReport) -> bool {
        let landed = self.landed(id);
        if landed {
            self.landed_reads.push((id, Box::new((dataset, report))));
        }
        landed
    }

    /// Takes a download off those under way: `false` when it isn't one of them (another project
    /// was opened meanwhile).
    fn landed(&mut self, id: u64) -> bool {
        let Some(i) = self.downloading.iter().position(|d| *d == id) else { return false };
        self.downloading.remove(i);
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
    /// so. A request made again while it waits is made once (the same address read another way,
    /// as another layer or with other columns, is another request), and past a few waiting
    /// requests the next is refused rather than silently dropping one already promised.
    pub fn request_url(&mut self, p: &serde_json::Value) -> Result<serde_json::Value, String> {
        let url = p.get("url").and_then(serde_json::Value::as_str).unwrap_or("").to_string();
        if !self.url_requests.iter().any(|q| q == p) {
            if self.url_requests.len() >= MAX_URL_REQUESTS {
                return Err(format!("{url}: {MAX_URL_REQUESTS} downloads are already waiting; try again when they have landed"));
            }
            self.url_requests.push(p.clone());
        }
        self.request_only = true;
        Ok(serde_json::json!({"pending": true, "url": url}))
    }

    /// Runs a command as [`Session::run`] does, except that the downloads `layer.addUrl` would
    /// wait for (on the desktop) are left to the front end, as in the browser: the app runs the
    /// commands of plugins this way, so a plugin adding data by address doesn't freeze it. The
    /// command line, scripts and the assistant run commands that wait, and see what was added.
    pub fn run_leaving_downloads(&mut self, id: &str, params: &serde_json::Value) -> CmdResult {
        let was = std::mem::replace(&mut self.leave_downloads, true);
        let r = self.run(id, params);
        self.leave_downloads = was;
        r
    }

    /// The `layer.addUrl` requests waiting for the front end ([`Session::request_url`]), taken.
    pub fn take_url_requests(&mut self) -> Vec<serde_json::Value> {
        std::mem::take(&mut self.url_requests)
    }

    /// Takes tool files awaiting download. Metadata stays until `gp.fileDelivered` acknowledges
    /// delivery, so producing bytes alone never records a successful run.
    pub fn take_gp_files(&mut self) -> Vec<(u64, String, Vec<u8>)> {
        self.file_outputs.iter_mut().filter_map(|f| f.bytes.take().map(|bytes| (f.id, f.file_name.clone(), bytes))).collect()
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

    /// Runs command `id` as a part of the one running now (a command made of others): its
    /// changes are in that command's one undo step, not a step of their own. A part that fails
    /// (or panics) is undone by its own `run`, which restores what it started from; the parts
    /// before it stay until the command made of them fails too, whose `run` restores the project
    /// as the whole command found it. The stacks are put back on every return.
    pub(crate) fn run_part(&mut self, id: &str, params: &Value) -> CmdResult {
        let (undo, redo) = (std::mem::take(&mut self.undo), std::mem::take(&mut self.redo));
        let res = self.run(id, params);
        (self.undo, self.redo) = (undo, redo);
        res
    }

    /// Runs a command by id. Mutating commands snapshot the project for undo; an error or a
    /// panic restores it. A command run by itself (not as a part of another) that changes the
    /// project, or reaches a file or a service, goes into the session's log (#797).
    pub fn run(&mut self, id: &str, params: &Value) -> CmdResult {
        let _diagnostic_scope = self.diagnostics.enter();
        let source = self.registry.get(id).map(|spec| spec.id).or_else(|| id.starts_with(rustgis_plugins::PREFIX).then_some("Plugin"));
        // Restored on every path, including panics outside `guarded`, so a later notice is not
        // attributed to an aborted command. Mirrors the capture scope's restore-on-drop.
        let previous = std::mem::replace(&mut self.diagnostic_source, source);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let notice_mark = self.notice_mark();
            if self.depth > 0 {
                return self.run_command(id, params);
            }
            // Undo and redo are logged with the step they take back or bring back.
            let step = match id {
                "edit.undo" => self.undo_label(),
                "edit.redo" => self.redo_label(),
                _ => None,
            };
            // A step that joins the last one (a held arrow key nudging an element: `merge_undo`) is
            // one entry too, as it is one step to undo.
            let joins = self.merge.is_some() && self.merge == self.merged && !self.undo.is_empty();
            let (at, start) = (project_log::now_ms(), web_time::Instant::now());
            let res = self.run_command(id, params);
            // A plugin's command waiting for its fetches answers `pending` and runs again when they
            // land: logged then, once. (Another command's `pending`, a download the web app makes, is
            // logged as it is asked for: nothing runs it again.)
            let pending = id.starts_with(rustgis_plugins::PREFIX)
                && res.as_ref().is_ok_and(|v| v.get("pending").and_then(Value::as_bool) == Some(true));
            // With logging off, only its own switching and clearing go in: nothing else is clipped
            // or redacted for nothing.
            let wanted = self.log.on() || matches!(id, "project.logCommands" | "project.clearLog");
            // Undo and redo go in when they take a step back or bring one back: pressed with nothing
            // to undo, they change nothing, and each press would be an entry.
            let stepped = !matches!(id, "edit.undo" | "edit.redo") || res.is_ok();
            if !pending && wanted && stepped {
                // Outside `guarded`'s catch: logging is best-effort, so a panic in it leaves the
                // command's result as it was, without an entry, and says so in the app's log.
                let logged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if project_log::is_logged(self, id, params) {
                        let params = match step {
                            Some(label) => serde_json::json!({"step": label}),
                            None => project_log::kept_params(params),
                        };
                        let e = project_log::entry(id, params, &res, at, start.elapsed());
                        if joins && res.is_ok() { self.log.join_last(e) } else { self.log.push(e) }
                    }
                }));
                if logged.is_err() {
                    log::warn!("the session log has no entry for {id}: writing it failed unexpectedly");
                }
            }
            // A refused command is said once, without secrets, as the front ends show it.
            if let Err(error) = &res {
                let text = rustgis_data::diagnostics::Redacted::new(error);
                if !self.notices_since(notice_mark).iter().any(|notice| notice.error && notice.text == text.as_str()) {
                    self.notify_redacted(text, true);
                }
            }
            res
        }));
        self.diagnostic_source = previous;
        match result {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    }

    fn run_command(&mut self, id: &str, params: &Value) -> CmdResult {
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
                self.warn(format!("{id} ignores {} (it reads {reads})", said.join(", ")));
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
        if id == "layer.addDownload"
            && res.is_ok()
            && let Some(token) = params.get("geolibreLayer").and_then(|v| v.get("token")).and_then(serde_json::Value::as_str)
        {
            self.retire_import_history(token);
        }
        // What was added, for the Data Sources pane's Recent list.
        if res.is_ok()
            && let Some(src) = sources::added_by(id, params)
        {
            self.sources.note(src);
        }
        res
    }

    /// Retires only this consumed reservation, preserving unrelated pending imports in history.
    pub(crate) fn retire_import_history(&mut self, token: &str) {
        if token.is_empty() {
            return;
        }
        for (project, ..) in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            project.discard_import_reservation(token);
        }
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
        let state = self.state;
        // The merge key asked for applies to this command only, whatever it does; and to a
        // command run by itself, not to those it runs.
        let merge = if self.depth == 0 { self.merge.take() } else { None };
        let open_layout = self.active_layout;
        // The project's generation rolls back with it: a project opened part way (a task's step)
        // and then undone is the one before, still open.
        let opened = self.opened;
        let files = mutates.then(|| ProjectFiles {
            identity: self.file_identity,
            path: self.path.clone(),
            opened_package: self.opened_package.clone(),
        });
        self.depth += 1;
        self.restorable += u32::from(mutates);
        // Edit counts roll back with the project when a command fails part way.
        let counts = mutates.then(|| self.edit_counts.clone());
        self.request_only = false;
        self.quiet_failure = false;
        // Requests a failed command left for the front end go with its other changes.
        let requests = self.url_requests.len();
        let file_outputs = self.file_outputs.len();
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
                fresh_source_ids(&snap, &mut self.project);
                forget_locks_of_removed_features(&snap, &mut self.project);
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
                let files = files.filter(|files| {
                    files.identity != self.file_identity || files.path != self.path || files.opened_package != self.opened_package
                });
                if key.is_none() || key != self.merged || self.undo.is_empty() {
                    self.undo.push((snap, open_layout, label, self.actor.clone(), state, files));
                    if self.undo.len() > UNDO_LIMIT {
                        self.undo.remove(0);
                    }
                } else if files.is_some()
                    && let Some(step) = self.undo.last_mut()
                    && step.5.is_none()
                {
                    step.5 = files;
                }
                if self.depth == 0 {
                    self.merged = key;
                }
                self.redo.clear();
                self.state = self.new_state();
            }
            (Err(_), Some(snap)) => {
                // File exports report delivery failures as history, not as a successful map
                // edit. Keep those failure records while rolling back the command's edits.
                let history_from = snap.gp_history.len();
                let failed_history = if self.opened == opened { std::mem::take(&mut self.project.gp_history) } else { Vec::new() };
                // The project as it was before the command, so the layout open then, whatever
                // the command opened on the way (a task's steps).
                self.project = snap;
                self.active_layout = open_layout;
                // Replaced during the command: what it held beside itself (its undo history, its
                // trust, its unsaved state) comes back with it.
                if self.opened != opened {
                    self.give_back(opened);
                }
                if let Some(files) = files {
                    self.file_identity = files.identity;
                    self.path = files.path;
                    self.opened_package = files.opened_package;
                }
                self.opened = opened;
                self.state = state;
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
                let mut recorded_failure = false;
                for record in failed_history
                    .into_iter()
                    .skip(history_from)
                    .filter(|record| !record.ok && rustgis_analysis::tool(&record.id).is_some_and(cmd::gp::writes_files))
                {
                    self.project.gp_history.push(record);
                    recorded_failure = true;
                }
                if recorded_failure {
                    if self.depth == 0 && self.project.gp_history.len() > 500 {
                        let excess = self.project.gp_history.len() - 500;
                        self.project.gp_history.drain(..excess);
                    }
                    self.state = self.new_state();
                }
            }
            _ => {}
        }
        if res.is_err() {
            self.url_requests.truncate(requests);
            self.file_outputs.truncate(file_outputs);
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
            state: self.state,
            saved: self.saved,
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
            opened_package: self.opened_package.clone(),
            file_identity: self.file_identity,
            model_run: self.model_run.take(),
            pending: take(&mut self.pending),
            file_outputs: take(&mut self.file_outputs),
            #[cfg(feature = "geolibre")]
            started: take(&mut self.started),
            plugin_runs: take(&mut self.plugin_runs),
            downloading: take(&mut self.downloading),
            downloaded: take(&mut self.downloaded),
            landed_reads: take(&mut self.landed_reads),
            url_requests: take(&mut self.url_requests),
            plugin_retries: take(&mut self.plugin_retries),
            style_view: self.style_view.take(),
            #[cfg(not(target_arch = "wasm32"))]
            sql_results: take(&mut self.sql_results),
            trend_results: take(&mut self.trend_results),
            raster_results: take(&mut self.raster_results),
            raster_runs: take(&mut self.raster_runs),
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
            state,
            saved,
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
            opened_package,
            file_identity,
            model_run,
            pending,
            file_outputs,
            #[cfg(feature = "geolibre")]
            started,
            plugin_runs,
            downloading,
            downloaded,
            landed_reads,
            url_requests,
            plugin_retries,
            style_view,
            #[cfg(not(target_arch = "wasm32"))]
            sql_results,
            trend_results,
            raster_results,
            raster_runs,
            edit_counts,
        } = *a;
        (self.undo, self.redo, self.back, self.forward, self.last_camera) = (undo, redo, back, forward, last_camera);
        self.history_projection = history_projection;
        (self.state, self.saved) = (state, saved);
        (self.project_starts, self.history_from_file) = (project_starts, history_from_file);
        (self.trusted_tasks, self.trusted_models, self.trusted_notebooks) = (trusted_tasks, trusted_models, trusted_notebooks);
        (self.active_layer, self.table_layer, self.table_standalone, self.tool) = (active_layer, table_layer, table_standalone, tool);
        (self.layer_selection, self.layer_selection_of) = (layer_selection, layer_selection_of);
        (self.measurements, self.topology_errors, self.edit_counts) = (measurements, topology_errors, edit_counts);
        (self.path, self.model_run, self.style_view) = (path, model_run, style_view);
        self.opened_package = opened_package;
        self.file_identity = file_identity;
        (self.pending, self.plugin_runs, self.plugin_retries) = (pending, plugin_runs, plugin_retries);
        self.file_outputs = file_outputs;
        (self.downloading, self.downloaded, self.url_requests) = (downloading, downloaded, url_requests);
        self.landed_reads = landed_reads;
        (self.trend_results, self.raster_results, self.raster_runs) = (trend_results, raster_results, raster_runs);
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
        use rustgis_data::diagnostics::Severity;
        self.notice(text.into(), if error { Severity::Error } else { Severity::Info });
    }

    /// A nonfatal problem: existing notice consumers still see it as a non-error, while
    /// troubleshooting history can distinguish it from ordinary informational notices.
    pub fn warn(&mut self, text: impl Into<String>) {
        self.notice(text.into(), rustgis_data::diagnostics::Severity::Warning);
    }

    fn notice(&mut self, text: String, severity: rustgis_data::diagnostics::Severity) {
        // Redacted for the status bar and the history alike. `redact` bounds the notice to 8 KiB
        // (an ellipsis marks the cut) and omits one over 16 KiB whole, so a clipped credential
        // can't survive; the history's copy is bounded further, to a record's 2 KiB.
        self.notice_redacted(rustgis_data::diagnostics::Redacted::new(&text), severity);
    }

    /// [`Session::notify`] for text already redacted (a failure the caller redacted to compare it
    /// with the notices): the type says so, so it isn't redacted again.
    pub fn notify_redacted(&mut self, text: rustgis_data::diagnostics::Redacted, error: bool) {
        use rustgis_data::diagnostics::Severity;
        self.notice_redacted(text, if error { Severity::Error } else { Severity::Info });
    }

    fn notice_redacted(&mut self, text: rustgis_data::diagnostics::Redacted, severity: rustgis_data::diagnostics::Severity) {
        self.diagnostics.push_redacted(severity, self.diagnostic_source.unwrap_or("Application"), &text);
        self.notices.push(Notice { text: text.into_string(), error: severity == rustgis_data::diagnostics::Severity::Error });
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
        // A state of its own: undo back past a replacement made within a command (a task's step)
        // reaches a state of the project before, which isn't this one's saved state.
        self.state = self.new_state();
        self.saved = Some(self.state);
    }

    /// Whether the project has changes that aren't saved: it is in another state than the one
    /// last saved or opened. Undo or redo back to that state leaves none.
    pub fn dirty(&self) -> bool {
        self.saved != Some(self.state)
    }

    /// Which state the project is in: another after each change, an earlier one after undo. A
    /// front end compares it to tell whether the project changed since it last looked.
    pub fn state(&self) -> u64 {
        self.state
    }

    /// The project as it is now is saved (written to its file, or downloaded in the browser).
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.state);
    }

    /// The project has changes that aren't saved, whatever undo does (work recovered after a
    /// crash: no state of it is in a file).
    pub fn mark_unsaved(&mut self) {
        self.saved = None;
    }

    /// A state of the project not given before in this session, for a change just made.
    fn new_state(&mut self) -> u64 {
        self.states = self.states.wrapping_add(1);
        self.states
    }

    pub(crate) fn undo(&mut self) -> bool {
        self.merged = None;
        let Some((p, saved, label, actor, state, mut files)) = self.undo.pop() else { return false };
        let (open, was) = (self.open_layout_name(), self.active_layout);
        let cur = std::mem::replace(&mut self.project, p);
        if let Some(files) = &mut files {
            std::mem::swap(&mut self.file_identity, &mut files.identity);
            std::mem::swap(&mut self.path, &mut files.path);
            std::mem::swap(&mut self.opened_package, &mut files.opened_package);
        }
        self.project.keep_table_ids(&cur);
        self.reopen_layout(open, saved);
        self.keep_time_position(&cur);
        self.keep_tile_pick(&cur);
        self.redo.push((cur, was, label, actor, std::mem::replace(&mut self.state, state), files));
        self.touch_all_geometry();
        true
    }

    pub(crate) fn redo(&mut self) -> bool {
        self.merged = None;
        let Some((p, saved, label, actor, state, mut files)) = self.redo.pop() else { return false };
        let (open, was) = (self.open_layout_name(), self.active_layout);
        let cur = std::mem::replace(&mut self.project, p);
        if let Some(files) = &mut files {
            std::mem::swap(&mut self.file_identity, &mut files.identity);
            std::mem::swap(&mut self.path, &mut files.path);
            std::mem::swap(&mut self.opened_package, &mut files.opened_package);
        }
        self.project.keep_table_ids(&cur);
        self.reopen_layout(open, saved);
        self.keep_time_position(&cur);
        self.keep_tile_pick(&cur);
        self.undo.push((cur, was, label, actor, std::mem::replace(&mut self.state, state), files));
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

    /// After undo or redo swaps the project: the tile layer picked in the Layers pane stays the one
    /// picked (picking isn't an undo step), found again on the map of its name by its address,
    /// kind, role and name (without the name when only one layer has the rest: a rename undone);
    /// the restored project's own picks are dropped.
    fn keep_tile_pick(&mut self, before: &Project) {
        for t in self.project.maps.iter_mut().flat_map(|m| m.tiles.iter_mut()) {
            t.selected = false;
        }
        for m in &mut self.project.maps {
            let Some(old) = before.maps.iter().find(|o| o.name == m.name) else { continue };
            let Some(p) = old.tiles.iter().find(|t| t.selected) else { continue };
            // Of the same address, kind and role (a basemap and an overlay of one service can
            // both be there); a name that leaves more than one is too unsure to pick any.
            let same = |t: &&mut rustgis_map::tiles::TileLayer| t.url == p.url && t.kind == p.kind && t.overlay == p.overlay;
            let mut like: Vec<_> = m.tiles.iter_mut().filter(same).collect();
            let named: Vec<usize> = like.iter().enumerate().filter(|(_, t)| t.name == p.name).map(|(i, _)| i).collect();
            let at = match named[..] {
                [at] => Some(at),
                [] if like.len() == 1 => Some(0),
                _ => None,
            };
            if let Some(t) = at.and_then(|at| like.get_mut(at)) {
                t.selected = true;
            }
        }
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

    /// The tile layer picked in the Layers pane (`map.selectTiles`): the topmost one marked, while
    /// no map layer is active (making one active ends it).
    pub fn selected_tile(&self) -> Option<usize> {
        if self.active_layer.is_some() {
            return None;
        }
        self.map().tiles.iter().position(|t| t.selected)
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
        // A tile layer picked in the Layers pane stops being picked once a map layer is active,
        // for good, as a layer selection does.
        if self.active_layer.is_some() && self.project.map().tiles.iter().any(|t| t.selected) {
            for t in &mut self.project.map_mut().tiles {
                t.selected = false;
            }
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
