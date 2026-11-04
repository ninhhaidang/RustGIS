//! Analyze tab: run geoprocessing tools and keep their history.

use rustgis_analysis::{Inputs, Output, OutputLayer, RasterInput};
use rustgis_data::Dataset;
use serde_json::{Value, json};

use super::{need_layers, ps};
use crate::project_log::now_ms;
use crate::{CmdResult, CommandSpec as C, GpRecord, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("gp.run", "Run Tool", "Analyze › Geoprocessing", run).when(need_layers).params(r#"{"tool": tool id, "params": {...} (a layer by name; a standalone table by name or id)}"#),
        C::new("gp.spatialJoin", "Spatial Join", "Analyze › Overlay", spatial_join)
            .when(need_layers)
            .params(
                r#"{"target": layer, "join": layer, "match"?: "Intersects" (default) | "Contains" | "Within", "operation"?: "Join one to one" (default) | "Join one to many", "keepAll"?: "Keep all" (default) | "Keep only matching", "prefix"?: "join_", "output"?: text}"#,
            ),
        C::new("gp.zonalStats", "Zonal Statistics", "Analyze › Raster", zonal_stats)
            .when(need_layers)
            .params(
                r#"{"raster": raster, "polygons": polygon layer, "bands"?: [1-based band numbers] or single band (default 1), "stats"?: ["mean", "min", "max", "sum", "count", "stddev"] or "all" (default "all"), "prefix"?: text, "output"?: text}"#,
            ),
        C::new("gp.bufferPoint", "Buffer A Point", "Analyze › Geoprocessing", buffer_point).params(
            r#"{"lon": number, "lat": number, "distance": number, "units"?: "Kilometers" (default) | "Meters" | "Miles" | "Feet", "name"?: output}"#,
        ),
        C::new("gp.tools", "Tools", "Analyze › Geoprocessing", |_, _| {
            Ok(Value::Array(
                rustgis_analysis::tools()
                    .map(|t| {
                        json!({"id": t.id, "name": t.name, "toolbox": t.toolbox, "summary": t.summary,
                            "params": t.params.iter().map(|p| json!({"name": p.name, "label": p.label, "required": p.required, "kind": format!("{:?}", p.kind)})).collect::<Vec<_>>()})
                    })
                    .collect(),
            ))
        })
        .view(),
        C::new("gp.geolibre", "GeoLibre Tools", "Analyze › Geoprocessing", |_, _| Ok(geolibre_status())).view(),
        C::new("gp.installGeolibre", "Install GeoLibre Tools", "Analyze › Geoprocessing", |_, _| {
            let path = install_geolibre(&|_, _| {})?;
            #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
            rustgis_geolibre::set_runner(Some(std::path::PathBuf::from(&path)))?;
            Ok(json!({"path": path, "status": geolibre_status()}))
        })
        .view(),
        C::new("gp.uninstallGeolibre", "Uninstall GeoLibre Tools", "Analyze › Geoprocessing", |_, _| uninstall_geolibre()).view(),
        C::new("gp.finish", "Finish Tool Run", "Analyze › Geoprocessing", finish)
            .params(r#"{"id": a finished run (the browser's GeoLibre tools run asynchronously)}"#),
        C::new("gp.fileDelivered", "Finish File Export", "Analyze › Geoprocessing", file_delivered)
            .params(r#"{"id": pending file id, "error"?: download failure message}"#),
        C::new("gp.setGeolibre", "Locate GeoLibre Runner", "Analyze › Geoprocessing", set_geolibre)
            .view()
            .params(r#"{"path"?: "the geolibre executable (empty: look in the usual places)"}"#),
        C::new("gp.clearHistory", "Clear History", "Analyze › Geoprocessing › History", |s, _| {
            s.project.gp_history.clear();
            Ok(Value::Null)
        }),
    ]
}

/// The GeoLibre tools (empty when the suite isn't installed, or the build doesn't include it).
pub fn geolibre_tools() -> &'static [rustgis_analysis::ToolSpec] {
    #[cfg(feature = "geolibre")]
    {
        rustgis_analysis::external_tools_of(rustgis_geolibre::SOURCE)
    }
    #[cfg(not(feature = "geolibre"))]
    {
        &[]
    }
}

/// Whether a tool is one of the GeoLibre tools (which the browser runs asynchronously).
pub fn is_geolibre_tool(spec: &rustgis_analysis::ToolSpec) -> bool {
    #[cfg(feature = "geolibre")]
    {
        rustgis_analysis::external_source(spec) == Some(rustgis_geolibre::SOURCE)
    }
    #[cfg(not(feature = "geolibre"))]
    {
        let _ = spec;
        false
    }
}

/// Whether the GeoLibre tools are available. Natively: the runner in use, its version and tool
/// count. In the browser (`web`): whether the WASI runner's tools have been loaded.
pub fn geolibre_status() -> Value {
    #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
    {
        let found = rustgis_geolibre::runner::find();
        json!({
            "supported": true,
            "web": false,
            "available": found.is_some(),
            // The runner in use is the one `gp.installGeolibre` put in place (so it can be uninstalled).
            "installed": found.as_ref().is_some_and(|(p, _)| rustgis_geolibre::install::is_installed(p)),
            "path": found.as_ref().map(|(p, _)| p.display().to_string()),
            "version": found.map(|(_, v)| v),
            "tools": geolibre_tools().len(),
            "download": rustgis_geolibre::runner::DOWNLOAD_URL,
        })
    }
    #[cfg(all(feature = "geolibre", target_arch = "wasm32"))]
    {
        let n = geolibre_tools().len();
        json!({"supported": true, "web": true, "available": n > 0, "tools": n})
    }
    #[cfg(not(feature = "geolibre"))]
    json!({"supported": false, "available": false})
}

/// Downloads and installs the latest GeoLibre runner for this platform; returns the program's
/// path. `progress` gets the bytes received and the expected total. Blocks: the app calls it
/// from a background thread, then picks the runner with `gp.setGeolibre`. Native only.
pub fn install_geolibre(progress: &dyn Fn(u64, Option<u64>)) -> Result<String, String> {
    #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
    {
        rustgis_geolibre::install::install(progress).map(|p| p.display().to_string())
    }
    #[cfg(not(all(feature = "geolibre", not(target_arch = "wasm32"))))]
    {
        let _ = progress;
        Err("this build can't install the GeoLibre runner".into())
    }
}

/// Removes the runner `gp.installGeolibre` installed, then looks for one again (a runner of the
/// user's own, found elsewhere, stays in use). In the browser: unloads the tools.
fn uninstall_geolibre() -> CmdResult {
    #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
    {
        rustgis_geolibre::install::uninstall()?;
        rustgis_geolibre::set_runner(None)?;
        Ok(geolibre_status())
    }
    #[cfg(all(feature = "geolibre", target_arch = "wasm32"))]
    {
        rustgis_geolibre::set_manifests("[]")?;
        Ok(geolibre_status())
    }
    #[cfg(not(feature = "geolibre"))]
    Err("this build doesn't include the GeoLibre tools".into())
}

/// Chooses the GeoLibre runner (an optional download), or looks for it again. Native only.
fn set_geolibre(_s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
    {
        let path = ps(p, "path").map(str::trim).filter(|p| !p.is_empty()).map(std::path::PathBuf::from);
        rustgis_geolibre::set_runner(path)?;
        Ok(geolibre_status())
    }
    #[cfg(not(all(feature = "geolibre", not(target_arch = "wasm32"))))]
    {
        let _ = p;
        Err("this build has no GeoLibre runner to choose".into())
    }
}

/// Loads the GeoLibre tool list from manifests (the JSON array the runner lists), as the
/// browser does from its WASI runner; returns how many tools there are.
pub fn external_load_manifests(json: &str) -> Result<usize, String> {
    #[cfg(feature = "geolibre")]
    {
        rustgis_geolibre::set_manifests(json)
    }
    #[cfg(not(feature = "geolibre"))]
    {
        let _ = json;
        Err("this build doesn't include the GeoLibre tools".into())
    }
}

/// How many asynchronous runs (started, or finished and not yet applied) the session keeps.
const MAX_ASYNC_RUNS: usize = 8;

/// A GeoLibre run to carry out asynchronously (the browser's WASI runner): run `tool` with
/// `argv` in a working folder `/work` holding `files`, then pass the new files and the printed
/// lines to [`external_complete`] with `id`.
#[derive(Clone, Debug)]
pub struct ExternalRun {
    pub id: u64,
    pub tool: String,
    pub argv: Vec<String>,
    pub files: Vec<(String, Vec<u8>)>,
}

/// The new files and printed lines of a finished run, or the runner's error.
pub type ExternalOutput = Result<(Vec<(String, Vec<u8>)>, Vec<String>), String>;

/// Prepares a run of a GeoLibre tool on the active map, checking its parameters as `gp.run`
/// does; the session keeps it until [`external_complete`].
pub fn external_start(s: &mut Session, tool: &str, params: &Value) -> Result<ExternalRun, String> {
    #[cfg(feature = "geolibre")]
    {
        let spec = rustgis_analysis::tool(tool)
            .filter(|t| rustgis_analysis::external_source(t) == Some(rustgis_geolibre::SOURCE))
            .ok_or_else(|| format!("no GeoLibre tool '{tool}'"))?;
        let params = rustgis_analysis::resolve_params(spec, params)?;
        let job = rustgis_geolibre::prepare(spec, &map_inputs(s, Some(spec), &params)?, &params)?;
        s.next_pending = s.next_pending.wrapping_add(1);
        let id = s.next_pending;
        let run = ExternalRun { id, tool: job.tool.clone(), argv: job.argv("/work"), files: job.files.clone() };
        s.started.push((id, job, params, now_ms()));
        // A run whose runner never answers is dropped eventually (it holds its input files).
        if s.started.len() > MAX_ASYNC_RUNS {
            s.started.remove(0);
        }
        Ok(run)
    }
    #[cfg(not(feature = "geolibre"))]
    {
        let _ = (s, tool, params);
        Err("this build doesn't include the GeoLibre tools".into())
    }
}

/// Finishes a run from [`external_start`]: turns the runner's files and lines into a result
/// that `gp.finish {"id"}` then applies like `gp.run` would, with undo and history.
pub fn external_complete(s: &mut Session, id: u64, ran: ExternalOutput) -> Result<(), String> {
    #[cfg(feature = "geolibre")]
    {
        let i = s
            .started
            .iter()
            .position(|(n, ..)| *n == id)
            .ok_or("the tool's run was dropped (its project was closed, or too many runs were waiting)")?;
        let (_, job, params, t0) = s.started.remove(i);
        // Reading the runner's files back parses outside `Session::run`'s guard: a bug in a
        // reader must not take the app down.
        let result = ran.map_err(|e| format!("{}: {e}", job.name)).and_then(|(files, lines)| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rustgis_geolibre::finish(&job, &files, &lines, None)))
                .map_err(|_| format!("{}: its output couldn't be read", job.name))
        });
        let tool_id = format!("{}{}", rustgis_geolibre::PREFIX, job.tool);
        s.pending.push(crate::PendingRun {
            id,
            tool: job.name.clone(),
            tool_id,
            params,
            result,
            started: t0,
            millis: now_ms().saturating_sub(t0),
        });
        if s.pending.len() > MAX_ASYNC_RUNS {
            s.pending.remove(0);
        }
        Ok(())
    }
    #[cfg(not(feature = "geolibre"))]
    {
        let _ = (s, id, ran);
        Err("this build doesn't include the GeoLibre tools".into())
    }
}

/// A plugin tool's run, prepared on the session by [`plugin_start`] and carried out elsewhere
/// (the desktop app runs it on a thread of its own, so the window stays responsive): call
/// [`PluginRun::run`] there, hand what it returns to [`plugin_complete`], then apply it with
/// `gp.finish {"id"}`.
pub struct PluginRun {
    pub id: u64,
    /// The tool's id.
    pub tool: &'static str,
    job: rustgis_plugins::tools::Job,
}

impl PluginRun {
    /// Runs the tool, blocking until it ends; `control` stops it and shows its progress.
    pub fn run(&self, control: &rustgis_plugins::tools::Control) -> Result<rustgis_analysis::ToolResult, String> {
        // A bug in a tool must not take the app down.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.job.run(control)))
            .unwrap_or_else(|_| Err(format!("{} failed unexpectedly", self.job.name())))
    }
}

/// A tool's run on another thread, started and not yet complete: a plugin tool's
/// ([`plugin_start`]) or a built-in one's ([`tool_start`]).
pub(crate) struct StartedRun {
    id: u64,
    name: String,
    /// The tool's id, for the history.
    tool: String,
    /// The parameters, as the history records them.
    params: Value,
    t0: u64,
    /// The feature ids of the streamed layers it reads whole, by lowercase name (a built-in tool
    /// reads them on its thread, and hands them back with its result).
    full_ids: FullIds,
    /// The layers it reads, as it read them, by lowercase name.
    read: std::collections::HashMap<String, Read>,
    /// The map it ran on: its identity ([`rustgis_map::Map::instance_id`]), and its name.
    map: (u64, String),
}

/// The feature ids of the streamed layers a tool read whole, by lowercase name: what its
/// selections' rows are.
type FullIds = std::collections::HashMap<String, Vec<u64>>;

fn full_ids(full: &std::collections::HashMap<String, Dataset>) -> FullIds {
    full.iter().map(|(k, d)| (k.clone(), d.features.iter().map(|f| f.id).collect())).collect()
}

/// Prepares a run of a plugin tool on the active map, as `gp.run` would run it: its parameters
/// checked and its layers read now, so that the map can change while it runs.
pub fn plugin_start(s: &mut Session, tool: &str, params: &Value) -> Result<PluginRun, String> {
    let spec = rustgis_analysis::tool(tool_id(tool).unwrap_or(tool))
        .filter(|t| t.id.starts_with(rustgis_plugins::PREFIX))
        .ok_or_else(|| format!("no plugin tool '{tool}'"))?;
    let resolved = rustgis_analysis::resolve_params(spec, params)?;
    let inputs = map_inputs(s, Some(spec), &resolved)?;
    let job = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rustgis_plugins::tools::prepare(spec, &inputs, &resolved)))
        .unwrap_or_else(|_| Err(format!("{} failed unexpectedly", spec.name)))?;
    let full_ids = full_ids(&inputs.loaded);
    drop(inputs);
    let read = read_layers(s, spec, &resolved);
    s.next_pending = s.next_pending.wrapping_add(1);
    let id = s.next_pending;
    let params = recorded(Some(spec), params.clone());
    let map = (s.map().instance_id(), s.map().name.clone());
    s.plugin_runs.push(StartedRun {
        id,
        name: spec.name.to_string(),
        tool: spec.id.to_string(),
        params,
        t0: now_ms(),
        full_ids,
        read,
        map,
    });
    // A run that never completes is dropped eventually.
    if s.plugin_runs.len() > MAX_ASYNC_RUNS {
        s.plugin_runs.remove(0);
    }
    Ok(PluginRun { id, tool: spec.id, job })
}

/// Whether a run from [`plugin_start`] still waits for its result: opening or creating a
/// project drops the runs of the previous one.
pub fn plugin_waiting(s: &Session, id: u64) -> bool {
    s.plugin_runs.iter().any(|r| r.id == id)
}

/// Forgets a run from [`plugin_start`] that won't complete (its thread couldn't start).
pub fn plugin_abandon(s: &mut Session, id: u64) {
    s.plugin_runs.retain(|r| r.id != id);
}

/// Finishes a run from [`plugin_start`] with what [`PluginRun::run`] returned: `gp.finish
/// {"id"}` then applies it like `gp.run` would, with undo and history.
pub fn plugin_complete(s: &mut Session, id: u64, result: Result<rustgis_analysis::ToolResult, String>) -> Result<(), String> {
    complete(s, id, result, None)
}

/// Completes a started run: fits its result to the edits made while it ran and to the windows of
/// the streamed layers it read whole (`full_ids` when it read them on its thread), and leaves it
/// for `gp.finish {"id"}`.
fn complete(
    s: &mut Session,
    id: u64,
    result: Result<rustgis_analysis::ToolResult, String>,
    full_ids: Option<FullIds>,
) -> Result<(), String> {
    let i = s
        .plugin_runs
        .iter()
        .position(|r| r.id == id)
        .ok_or("the tool's run was dropped (its project was closed, or too many runs were waiting)")?;
    let run = s.plugin_runs.remove(i);
    let mut result = result;
    // What it adds to or changes on the map belongs on the map it ran on, whatever its name or
    // place now: not another made since with its name.
    let on_map = matches!(&result, Ok(r) if match &r.output {
        Output::Layer(_) | Output::Selection { .. } | Output::Update { .. } => true,
        Output::Layers(out) => out.iter().any(|o| !matches!(o, rustgis_analysis::OutputLayer::Table(_))),
        _ => false,
    });
    let mut same = (0..s.project.maps.len()).filter(|&i| s.project.maps.get(i).is_some_and(|m| m.instance_id() == run.map.0));
    let ran_on = same.next().filter(|_| same.next().is_none());
    if on_map && ran_on != Some(s.project.active_map) {
        result = Err(format!(
            "{} ran on the map '{}', which was deleted or is no longer the active one: open it and run the tool again",
            run.name, run.map.1
        ));
    }
    // A selection's rows of a streamed layer read whole are found by the name it was read under,
    // whatever the layer is called now.
    let read_as = output_layer(&result);
    let mut full_ids = full_ids.unwrap_or(run.full_ids);
    fit_to_changes(s, &mut result, &run.name, &run.read, &full_ids);
    if let (Some(was), Some(now)) = (read_as, output_layer(&result))
        && was != now
        && let Some(ids) = full_ids.remove(&was)
    {
        full_ids.insert(now, ids);
    }
    fit_to_windows(s, &mut result, &full_ids);
    s.pending.push(crate::PendingRun {
        id,
        tool: run.name,
        tool_id: run.tool,
        params: run.params,
        result,
        started: run.t0,
        millis: now_ms().saturating_sub(run.t0),
    });
    if s.pending.len() > MAX_ASYNC_RUNS {
        s.pending.remove(0);
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
/// A built-in tool's run from [`tool_start`]: what it reads of the session, owned, so it runs on
/// another thread ([`ToolRun::run`]) while the session goes on. Desktop only: the browser runs
/// tools inline, and its GeoLibre tools through its own runner ([`external_start`]).
pub struct ToolRun {
    pub id: u64,
    /// The tool's id.
    pub tool: &'static str,
    snap: Snapshot<'static>,
    params: Value,
}

#[cfg(not(target_arch = "wasm32"))]
/// What a [`ToolRun`] made, for [`tool_complete`].
pub struct ToolDone {
    result: Result<rustgis_analysis::ToolResult, String>,
    full_ids: FullIds,
}

#[cfg(not(target_arch = "wasm32"))]
impl ToolRun {
    /// Reads the tool's inputs and runs it, blocking until it ends; `control` stops it and counts
    /// its progress (see [`rustgis_analysis::control`]).
    pub fn run(self, control: &std::sync::Arc<rustgis_analysis::control::Control>) -> ToolDone {
        let ToolRun { tool, snap, params, .. } = self;
        let name = rustgis_analysis::tool(tool).map_or(tool, |t| t.name);
        if control.is_cancelled() {
            return ToolDone { result: Err(format!("{name} was cancelled before it started")), full_ids: FullIds::new() };
        }
        // A bug in reading the inputs, like one in a tool, must not take the app down.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let inputs = match inputs_of(snap, rustgis_analysis::tool(tool), &params) {
                Ok(inputs) => inputs,
                Err(e) => return ToolDone { result: Err(e), full_ids: FullIds::new() },
            };
            let mut result = rustgis_analysis::run_with(tool, &inputs, &params, control);
            if let Ok(r) = &mut result {
                r.messages.extend(inputs.messages.iter().cloned());
            }
            ToolDone { result, full_ids: full_ids(&inputs.loaded) }
        }))
        .unwrap_or_else(|_| ToolDone { result: Err(format!("{name} failed unexpectedly")), full_ids: FullIds::new() })
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Starts a run of a built-in tool on the active map, as `gp.run` would run it, to go on another
/// thread: its parameters checked and what it reads of the session taken now (cheaply: the
/// features and pixels are shared), its files and streamed layers read on that thread. The run's
/// result goes to [`tool_complete`], then `gp.finish {"id"}` applies it with undo and history; a
/// layer edited meanwhile is handled as for a plugin tool. Plugin tools start with
/// [`plugin_start`].
pub fn tool_start(s: &mut Session, tool: &str, params: &Value) -> Result<ToolRun, String> {
    let spec = rustgis_analysis::tool(tool_id(tool).unwrap_or(tool))
        .filter(|t| !t.id.starts_with(rustgis_plugins::PREFIX))
        .ok_or_else(|| format!("no tool '{tool}'"))?;
    let resolved = rustgis_analysis::resolve_params(spec, params)?;
    let read = read_layers(s, spec, &resolved);
    s.next_pending = s.next_pending.wrapping_add(1);
    let id = s.next_pending;
    let recorded_params = recorded(Some(spec), params.clone());
    let full_ids = FullIds::new();
    s.plugin_runs.push(StartedRun {
        id,
        name: spec.name.to_string(),
        tool: spec.id.to_string(),
        params: recorded_params,
        t0: now_ms(),
        full_ids,
        read,
        map: (s.map().instance_id(), s.map().name.clone()),
    });
    // A run that never completes is dropped eventually.
    if s.plugin_runs.len() > MAX_ASYNC_RUNS {
        s.plugin_runs.remove(0);
    }
    Ok(ToolRun { id, tool: spec.id, snap: Snapshot::owned(s), params: params.clone() })
}

#[cfg(not(target_arch = "wasm32"))]
/// Whether a run from [`tool_start`] still waits for its result: opening or creating a project
/// drops the runs of the previous one.
pub fn tool_waiting(s: &Session, id: u64) -> bool {
    plugin_waiting(s, id)
}

#[cfg(not(target_arch = "wasm32"))]
/// Forgets a run from [`tool_start`] that won't complete (its thread couldn't start).
pub fn tool_abandon(s: &mut Session, id: u64) {
    plugin_abandon(s, id);
}

#[cfg(not(target_arch = "wasm32"))]
/// Finishes a run from [`tool_start`] with what [`ToolRun::run`] made: `gp.finish {"id"}` then
/// applies it like `gp.run` would, with undo and history.
pub fn tool_complete(s: &mut Session, id: u64, done: ToolDone) -> Result<(), String> {
    complete(s, id, done.result, Some(done.full_ids))
}

/// Fits a background run's result to edits made while it ran: its layer is the one it read,
/// whatever its name now; a selection of a layer that changed, or of a streamed layer whose
/// window moved, selects the same features (by id) where they now are; and an update of a layer
/// that changed, which would discard those edits, or of one removed, is refused. A streamed
/// layer it read whole is fitted by [`fit_to_windows`].
fn fit_to_changes(
    s: &Session,
    res: &mut Result<rustgis_analysis::ToolResult, String>,
    tool: &str,
    read: &std::collections::HashMap<String, Read>,
    full_ids: &FullIds,
) {
    let Ok(r) = res else { return };
    let (Output::Selection { layer, .. } | Output::Update { layer, .. }) = &mut r.output else { return };
    let key = layer.to_ascii_lowercase();
    let Some(was) = read.get(&key) else { return };
    // The layer it read, by id: renamed perhaps, but not another of the same name.
    let Some(now) = s.map().layer(was.id) else {
        *res = Err(format!("{layer} was removed while {tool} ran"));
        return;
    };
    // Another layer with its id (on a map that took the name of the one it ran on), or the layer
    // relinked to another table of its file, isn't it.
    if now.dataset.source != was.source || now.linked_table() != was.table.as_deref() {
        *res = Err(format!("{layer} was removed or replaced while {tool} ran"));
        return;
    }
    layer.clone_from(&now.name);
    if Read::unchanged(&was.dataset, &now.dataset) {
        return;
    }
    // A selection of what it read whole (rows of the whole source) is fitted to the layer later,
    // loaded since or not. A layer still streamed has rows that follow the view: an update of it
    // is checked there too.
    let streamed = was.streamed && now.is_streamed();
    match &mut r.output {
        Output::Selection { .. } if was.streamed && full_ids.contains_key(&key) => {}
        Output::Selection { rows, .. } => {
            let ids: std::collections::HashSet<u64> = rows.iter().filter_map(|r| was.dataset.features.get(*r)).map(|f| f.id).collect();
            *rows = now.dataset.features.iter().enumerate().filter(|(_, f)| ids.contains(&f.id)).map(|(i, _)| i).collect();
            let gone = ids.len().saturating_sub(rows.len());
            if gone > 0 {
                let place = if now.is_streamed() { "outside the part of it read for the view" } else { "deleted" };
                r.messages.push(format!(
                    "{gone} of the features {tool} selected in {} are no longer there ({place} while it ran) and aren't selected",
                    now.name
                ));
            }
        }
        _ if streamed => {}
        // Its rows since then can't be told from edits: they are another copy of the source.
        _ if was.streamed => *res = Err(format!("{} was loaded whole or replaced while {tool} ran; run it again", now.name)),
        _ => *res = Err(format!("{} changed while {tool} ran; run it again", now.name)),
    }
}

/// The layer a result selects in or updates, by lowercase name.
fn output_layer(res: &Result<rustgis_analysis::ToolResult, String>) -> Option<String> {
    match &res.as_ref().ok()?.output {
        Output::Selection { layer, .. } | Output::Update { layer, .. } => Some(layer.to_ascii_lowercase()),
        _ => None,
    }
}

/// A layer a background run read, to tell whether it changed while the run went on.
pub(crate) struct Read {
    /// The layer, by id: renamed perhaps, but not another of the same name.
    id: u32,
    /// Its dataset as the run read it: a copy that shares the rows, so taking it costs nothing
    /// however large the layer, and any edit since (which copies the rows held here too) shows.
    /// For a streamed layer, the window in view then.
    dataset: Dataset,
    /// Whether it was read in place from its file, its rows following the view.
    streamed: bool,
    /// Where its data comes from: a layer with the same id and another source is another layer.
    source: Option<String>,
    /// The table of that file it reads: a repair can relink it to another.
    table: Option<String>,
}

impl Read {
    fn of(l: &rustgis_map::Layer) -> Self {
        Read {
            id: l.id,
            dataset: l.dataset.clone(),
            streamed: l.is_streamed(),
            source: l.dataset.source.clone(),
            table: l.linked_table().map(String::from),
        }
    }

    /// Whether `now` is the dataset `was` as it was read: the same rows in memory, and the same
    /// fields, geometry type, coordinate system and source.
    fn unchanged(was: &Dataset, now: &Dataset) -> bool {
        now.features.shares(&was.features)
            && now.fields == was.fields
            && now.geometry_type == was.geometry_type
            && now.crs == was.crs
            && now.crs_incomplete == was.crs_incomplete
            && now.zm_dropped == was.zm_dropped
            && now.source == was.source
    }
}

/// The layers a tool names, as it reads them now, by lowercase name.
fn read_layers(s: &Session, spec: &rustgis_analysis::ToolSpec, resolved: &Value) -> std::collections::HashMap<String, Read> {
    layer_names(Some(spec), resolved)
        .into_iter()
        .filter_map(|n| s.map().layer_by_name(n))
        .map(|l| (l.name.to_ascii_lowercase(), Read::of(l)))
        .collect()
}

/// Fits a tool's result to the map: a selection made on the complete streamed source becomes
/// rows of the current window. An update from a selection is still refused: it would replace
/// the complete source with just those selected features.
fn fit_to_windows(s: &Session, res: &mut Result<rustgis_analysis::ToolResult, String>, full_ids: &FullIds) {
    if let Ok(r) = res
        && let Output::Selection { layer, rows } = &mut r.output
        && let Some(full) = full_ids.get(&layer.to_ascii_lowercase())
        && let Some(l) = s.map().layer_by_name(layer)
    {
        let ids: std::collections::HashSet<u64> = rows.iter().filter_map(|r| full.get(*r)).copied().collect();
        *rows = l.dataset.features.iter().enumerate().filter(|(_, f)| ids.contains(&f.id)).map(|(i, _)| i).collect();
        let gone = ids.len().saturating_sub(rows.len());
        if gone > 0 {
            // Still streamed: outside the window. Loaded whole since: deleted meanwhile.
            let place = if l.is_streamed() { "are outside the part of it read for the view" } else { "were deleted while it ran" };
            r.messages.push(format!("{gone} of the features selected in {layer} {place}, and aren't selected"));
        }
    }
    if let Ok(r) = res
        && let Output::Update { layer, .. } = &r.output
        && s.map()
            .layer_by_name(layer)
            .is_some_and(|l| l.is_streamed() && (!l.selection.is_empty() || !full_ids.contains_key(&layer.to_ascii_lowercase())))
    {
        *res = Err(format!("{layer} is read in place from its file; Load All Features (layer.loadAll) before a tool updates it"));
    }
}

/// Applies a finished run stashed by [`external_complete`] or [`plugin_complete`].
fn finish(s: &mut Session, p: &Value) -> CmdResult {
    let id = p.get("id").and_then(Value::as_u64).ok_or("id is required")?;
    let i = s.pending.iter().position(|r| r.id == id).ok_or("no finished run with that id")?;
    let r = s.pending.remove(i);
    apply(s, Ran { name: r.tool, id: r.tool_id, command: None, started: r.started }, r.params, r.result, r.millis)
}

/// What a tool reads of the session: the active map, the standalone tables, the geoprocessing
/// environment and the view's size. A run on the session's own thread borrows them; one on
/// another thread owns a copy taken when it starts, which shares the features and pixels.
struct Snapshot<'a> {
    map: std::borrow::Cow<'a, rustgis_map::Map>,
    tables: std::borrow::Cow<'a, [crate::project::Table]>,
    env: std::borrow::Cow<'a, crate::project::GpEnvironment>,
    /// The map view's size, for its extent.
    view_size: (f64, f64),
}

impl<'a> Snapshot<'a> {
    /// The session itself, for a run on its thread.
    fn of(s: &'a Session) -> Snapshot<'a> {
        use std::borrow::Cow::Borrowed;
        Snapshot { map: Borrowed(s.map()), tables: Borrowed(&s.project.tables), env: Borrowed(&s.project.gp_env), view_size: s.view_size }
    }

    /// A copy of the session for a run on another thread: cheap, as the features and pixels are
    /// shared.
    #[cfg(not(target_arch = "wasm32"))]
    fn owned(s: &Session) -> Snapshot<'static> {
        use std::borrow::Cow::Owned;
        Snapshot {
            map: Owned(s.map().clone()),
            tables: Owned(s.project.tables.clone()),
            env: Owned(s.project.gp_env.clone()),
            view_size: s.view_size,
        }
    }
}

/// A tool's inputs on the active map.
struct MapInputs<'a> {
    /// The session as the tool started: its active map and its standalone tables.
    snap: Snapshot<'a>,
    /// The rows of each layer (by lowercase name) the tool runs on: the selection or the
    /// environment's extent.
    rows: std::collections::HashMap<String, Vec<usize>>,
    /// Streamed layers the tool reads, loaded whole (by lowercase name).
    loaded: std::collections::HashMap<String, Dataset>,
    /// Import warnings from local file inputs, kept in the tool result and history.
    messages: Vec<String>,
    /// The flat map's projection (its name, the projection and its unit in metres), built the
    /// first time a tool asks for it (Tissot's indicatrix does), once for the run; `None` on the
    /// globe.
    flat: std::cell::OnceCell<Option<(String, rustgis_map::projection::Flat, f64)>>,
    /// The view's longitude/latitude extent, worked out the first time a tool asks for it (it
    /// inverse-projects the view, which most tools never need).
    view: std::cell::OnceCell<Option<rustgis_geom::Extent>>,
}

impl MapInputs<'_> {
    /// The flat map's projection, built on first use.
    fn flat(&self) -> Option<&(String, rustgis_map::projection::Flat, f64)> {
        let map = &self.snap.map;
        self.flat
            .get_or_init(|| {
                (map.projection != rustgis_map::Projection::Globe).then(|| {
                    let flat = map.flat();
                    let to_meter = match &flat {
                        rustgis_map::projection::Flat::Mercator => 1.0,
                        rustgis_map::projection::Flat::Custom(p) => p.to_meter(),
                    };
                    (map.projection.label(), flat, to_meter)
                })
            })
            .as_ref()
    }
}

fn map_inputs<'a>(s: &'a Session, spec: Option<&rustgis_analysis::ToolSpec>, params: &Value) -> Result<MapInputs<'a>, String> {
    inputs_of(Snapshot::of(s), spec, params)
}

/// A tool's inputs from what it read of the session: the streamed layers and files it names read
/// whole, and the rows of each layer it runs on. It reads, so a background run does this on its
/// thread.
fn inputs_of<'a>(snap: Snapshot<'a>, spec: Option<&rustgis_analysis::ToolSpec>, params: &Value) -> Result<MapInputs<'a>, String> {
    let full = streamed_inputs(&snap.map, spec, params)?;
    let messages = Vec::new();
    #[cfg(not(target_arch = "wasm32"))]
    let mut messages = messages;
    #[cfg(not(target_arch = "wasm32"))]
    let mut full = full;
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(spec) = spec.filter(|t| rustgis_analysis::external_source(t).is_none()) {
        for parameter in spec.params.iter().filter(|p| p.kind == rustgis_analysis::ParamKind::VectorLayers) {
            for name in rustgis_analysis::layer_list(params.get(parameter.name).unwrap_or(&Value::Null))? {
                if snap.map.layer_by_name(name).is_none() && !full.contains_key(name) {
                    let (dataset, report) = rustgis_data::io::read_path(std::path::Path::new(name)).map_err(|e| format!("{name}: {e}"))?;
                    messages.extend(report.messages.into_iter().map(|message| format!("{name}: {message}")));
                    full.insert(name.to_string(), dataset);
                }
            }
        }
    }
    let mut rows = env_inputs(&snap.map, &full, &snap.env, params)?;
    for (name, dataset) in &full {
        let Some(layer) = snap.map.layer_by_name(name).filter(|l| !l.selection.is_empty()) else { continue };
        let missing = || format!("{}: a selected feature is no longer in the source", layer.name);
        let mut by_id = layer
            .selection
            .iter()
            .map(|row| layer.dataset.features.get(*row).map(|f| (f.id, None)).ok_or_else(missing))
            .collect::<Result<std::collections::HashMap<u64, Option<usize>>, _>>()?;
        for (row, feature) in dataset.features.iter().enumerate() {
            if let Some(selected_row) = by_id.get_mut(&feature.id) {
                *selected_row = Some(row);
            }
        }
        let selected = layer
            .selection
            .iter()
            .map(|row| layer.dataset.features.get(*row).and_then(|f| by_id.get(&f.id)).copied().flatten().ok_or_else(missing))
            .collect::<Result<Vec<_>, _>>()?;
        rows.insert(name.clone(), selected);
    }
    Ok(MapInputs { snap, rows, loaded: full, messages, flat: std::cell::OnceCell::new(), view: std::cell::OnceCell::new() })
}

/// Complete streamed inputs without selections, plus selected PMTiles inputs: that exporter
/// requires the complete source. Their selected window rows are remapped in `map_inputs`.
fn streamed_inputs(
    map: &rustgis_map::Map,
    spec: Option<&rustgis_analysis::ToolSpec>,
    params: &Value,
) -> Result<std::collections::HashMap<String, Dataset>, String> {
    let mut full = std::collections::HashMap::new();
    for n in layer_names(spec, params) {
        let key = n.to_ascii_lowercase();
        if full.contains_key(&key) {
            continue;
        }
        if let Some(l) =
            map.layer_by_name(n).filter(|l| l.is_streamed() && (l.selection.is_empty() || spec.is_some_and(|t| t.id == "vectorToPmtiles")))
        {
            full.insert(key, l.load_all().map_err(|e| format!("{}: {e}", l.name))?);
        }
    }
    Ok(full)
}

/// The layers a tool's parameters name: the values of its layer parameters (all of them, for a
/// tool without a spec), so a field or text value that happens to match a layer's name isn't one.
fn layer_names<'v>(spec: Option<&rustgis_analysis::ToolSpec>, params: &'v Value) -> Vec<&'v str> {
    fn names<'v>(v: &'v Value, out: &mut Vec<&'v str>) {
        match v {
            Value::String(s) => out.push(s),
            Value::Array(a) => a.iter().for_each(|x| names(x, out)),
            Value::Object(o) => o.values().for_each(|x| names(x, out)),
            _ => {}
        }
    }
    use rustgis_analysis::ParamKind as K;
    let mut named = Vec::new();
    match spec {
        Some(spec) => {
            for p in spec.params {
                if let Some(v) = params.get(p.name) {
                    if p.kind == K::VectorLayers {
                        if let Ok(layers) = rustgis_analysis::layer_list(v) {
                            named.extend(layers);
                        }
                    } else if matches!(p.kind, K::Layer | K::PolygonLayer | K::PointLayer | K::VectorLayer) {
                        names(v, &mut named);
                    }
                }
            }
        }
        None => names(params, &mut named),
    }
    named
}

/// The environment's rows for a run, refusing a run whose main input has no features inside the
/// extent (which would otherwise process everything, or nothing).
fn env_inputs(
    map: &rustgis_map::Map,
    full: &std::collections::HashMap<String, Dataset>,
    env: &crate::project::GpEnvironment,
    params: &Value,
) -> Result<std::collections::HashMap<String, Vec<usize>>, String> {
    let rows = env_rows(map, full, env, params);
    if let Some((name, v)) = rows.iter().next()
        && v.is_empty()
    {
        return Err(format!("no features of {name} are inside the environment's extent"));
    }
    Ok(rows)
}

/// The rows of the tool's main input (its `input` layer) inside the environment's extent, when
/// that layer has no selection. Overlay layers (clip, erase, join targets…) stay whole.
fn env_rows(
    map: &rustgis_map::Map,
    full: &std::collections::HashMap<String, Dataset>,
    env: &crate::project::GpEnvironment,
    params: &Value,
) -> std::collections::HashMap<String, Vec<usize>> {
    let Some(e) = env.extent else { return std::collections::HashMap::new() };
    let Some(input) = params.get("target").or_else(|| params.get("input")).and_then(Value::as_str) else {
        return std::collections::HashMap::new();
    };
    // The layer the name finds (the first of that name), as the tool will.
    map.layer_by_name(input)
        .into_iter()
        .filter(|l| !l.is_raster() && l.selection.is_empty())
        .map(|l| {
            let ds = full.get(&l.name.to_ascii_lowercase()).unwrap_or(&l.dataset);
            let rows: Vec<usize> = ds
                .features
                .iter()
                .enumerate()
                .filter(|(_, f)| f.geometry.as_ref().is_some_and(|g| g.extent().intersects(&e)))
                .map(|(i, _)| i)
                .collect();
            (l.name.to_ascii_lowercase(), rows)
        })
        .collect()
}

impl Inputs for MapInputs<'_> {
    fn vector_to_pmtiles(
        &self,
        dataset: &Dataset,
        rows: &[usize],
        min_zoom: u8,
        max_zoom: u8,
        source_layer: &str,
    ) -> Result<(Vec<u8>, Vec<String>), String> {
        let out = crate::render::rustgis_tiles::vector_export::export(dataset, rows, min_zoom, max_zoom, source_layer)?;
        Ok((out.bytes, out.messages))
    }
    fn raster_to_pmtiles(&self, layer: &str, min_zoom: u8, max_zoom: u8) -> Result<Vec<u8>, String> {
        let map = &self.snap.map;
        let layer = map.layer_by_name(layer).ok_or_else(|| format!("no raster layer named '{layer}'"))?;
        let raster = layer.raster.as_ref().ok_or_else(|| format!("no raster layer named '{}'", layer.name))?;
        let opacity = map.layer_opacity(layer);
        crate::render::raster_export::export(raster, min_zoom, max_zoom, opacity)
    }
    fn dataset(&self, layer: &str) -> Option<&Dataset> {
        self.loaded
            .get(layer)
            .or_else(|| self.loaded.get(&layer.to_ascii_lowercase()))
            .or_else(|| self.snap.map.layer_by_name(layer).map(|l| &l.dataset))
            // A table as commands find one: by its name, else (names can repeat) by its id.
            .or_else(|| crate::project::find_table(&self.snap.tables, layer).and_then(|i| self.snap.tables.get(i)).map(|t| &t.dataset))
    }
    fn table(&self, id: u32) -> Option<&Dataset> {
        self.snap.tables.iter().find(|t| t.id == id).map(|t| &t.dataset)
    }
    fn selection(&self, layer: &str) -> &[usize] {
        let Some(l) = self.snap.map.layer_by_name(layer) else { return &[] };
        // Loaded selections and environment extents are already rows of the dataset returned
        // above. An extent containing none is refused before the run by `env_inputs`.
        self.rows.get(&l.name.to_ascii_lowercase()).map(Vec::as_slice).unwrap_or(&l.selection)
    }
    fn projection_name(&self) -> Option<String> {
        self.flat().map(|(name, _, _)| name.clone())
    }
    fn project(&self, c: rustgis_geom::Coord) -> Option<rustgis_geom::Coord> {
        let (_, flat, to_meter) = self.flat()?;
        flat.forward(c).map(|m| rustgis_geom::Coord::new(m.x * to_meter, m.y * to_meter))
    }
    fn view_extent(&self) -> Option<rustgis_geom::Extent> {
        *self.view.get_or_init(|| {
            // The globe shows the whole Earth (the camera's flat extent means nothing there).
            if self.snap.map.projection.is_globe() {
                return Some(rustgis_geom::Extent::new(-180.0, -90.0, 180.0, 90.0));
            }
            Some(super::workspace::view_extent_of(&self.snap.map, self.snap.view_size)).filter(|e| !e.is_empty())
        })
    }
    fn raster(&self, layer: &str) -> Option<RasterInput<'_>> {
        let rl = self.snap.map.layer_by_name(layer)?.raster.as_ref()?;
        Some(RasterInput {
            raster: rl.data.as_deref()?,
            path: rl.path.as_deref(),
            embedded: rl.embedded.as_deref().map(|b| (rl.file_name.as_deref().unwrap_or("raster.tif"), b.as_slice())),
        })
    }
}

fn run(s: &mut Session, p: &Value) -> CmdResult {
    let tool = ps(p, "tool").ok_or("tool is required")?.to_string();
    let params = p.get("params").cloned().unwrap_or(json!({}));
    run_tool(s, &tool, &params)
}

/// The id of a tool named by its id or its display name (history records keep the name).
pub fn tool_id(name: &str) -> Option<&'static str> {
    rustgis_analysis::tool(name).or_else(|| rustgis_analysis::tools().find(|t| t.name.eq_ignore_ascii_case(name))).map(|t| t.id)
}

/// One point, for a tool run on a place clicked on the map.
struct PointInput(Dataset);

impl Inputs for PointInput {
    fn dataset(&self, layer: &str) -> Option<&Dataset> {
        (layer == self.0.name).then_some(&self.0)
    }
    fn selection(&self, _layer: &str) -> &[usize] {
        &[]
    }
}

/// Buffers a point (the map's right-click Quick Analysis): the Buffer tool on a one-point input
/// that isn't added to the map; the buffer is.
fn buffer_point(s: &mut Session, p: &Value) -> CmdResult {
    use rustgis_data::{Geometry, GeometryType};
    let lon = super::f(p, "lon").ok_or("lon is required")?;
    let lat = super::f(p, "lat").ok_or("lat is required")?;
    if !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
        return Err("longitude must be within ±180 and latitude within ±90".into());
    }
    let mut ds = Dataset::new("Clicked point", GeometryType::Point, Vec::new());
    ds.push_next(Some(Geometry::Point(rustgis_geom::Coord::new(lon, lat))), Vec::new());
    let mut params = json!({"input": ds.name, "distance": p.get("distance").cloned().unwrap_or(Value::Null)});
    // The history keeps this command, with the point, so the run can be copied and run again
    // (#539): its input is no layer of the project.
    let mut command = json!({"lon": lon, "lat": lat, "distance": params["distance"]});
    if let Some(u) = p.get("units") {
        params["units"] = u.clone();
        command["units"] = u.clone();
    }
    if let Some(n) = p.get("name") {
        params["output"] = n.clone();
        command["name"] = n.clone();
    }
    let t0 = now_ms();
    let res = rustgis_analysis::run("buffer", &PointInput(ds), &params);
    let millis = now_ms().saturating_sub(t0);
    let ran = Ran { name: "Buffer".into(), id: "buffer".into(), command: Some(("gp.bufferPoint".into(), command)), started: t0 };
    apply(s, ran, params, res, millis)
}

/// Joins attributes from another layer spatially: `target` receives attributes from `join`.
fn spatial_join(s: &mut Session, p: &Value) -> CmdResult {
    run_tool(s, "spatialJoin", p)
}

/// Calculates raster summary statistics for polygon zones.
fn zonal_stats(s: &mut Session, p: &Value) -> CmdResult {
    run_tool(s, "zonalStats", p)
}

/// Runs a (built-in) tool and applies its result, as `gp.run` does. Where a person at a terminal
/// can stop it with Ctrl-C ([`Session::stoppable`](crate::Session::stoppable)), it takes no work
/// cap ([`rustgis_analysis::control::Control::stoppable`]).
pub fn run_tool(s: &mut Session, tool: &str, params: &Value) -> CmdResult {
    use rustgis_analysis::control::Control;
    let control = if s.stoppable { Control::stoppable() } else { Control::new() };
    run_tool_with(s, tool, params, &control)
}

/// [`run_tool`] under `control`: setting its flag from another thread stops the tool at its next
/// check, with an error and nothing applied, and its count follows the tool's progress. One
/// cancelled before it starts reads nothing and runs nothing, and is kept in the history too.
pub fn run_tool_with(
    s: &mut Session,
    tool: &str,
    params: &Value,
    control: &std::sync::Arc<rustgis_analysis::control::Control>,
) -> CmdResult {
    let tool = tool_id(tool).unwrap_or(tool).to_string();
    let params = params.clone();
    let t0 = now_ms();
    if control.is_cancelled() {
        let spec = rustgis_analysis::tool(&tool);
        let name = spec.map(|t| t.name).unwrap_or("Tool").to_string();
        let e = format!("{name} was cancelled before it started");
        return apply(s, Ran { name, id: tool, command: None, started: t0 }, recorded(spec, params), Err(e), 0);
    }
    let inputs = map_inputs(s, rustgis_analysis::tool(&tool), &params)?;
    let mut res = rustgis_analysis::run_with(&tool, &inputs, &params, control);
    if let Ok(result) = &mut res {
        result.messages.extend(inputs.messages.iter().cloned());
    }
    let ids = full_ids(&inputs.loaded);
    drop(inputs);
    fit_to_windows(s, &mut res, &ids);
    let millis = now_ms().saturating_sub(t0);
    let spec = rustgis_analysis::tool(&tool);
    let name = spec.map(|t| t.name).unwrap_or("Tool").to_string();
    apply(s, Ran { name, id: tool, command: None, started: t0 }, recorded(spec, params), res, millis)
}

/// Whether a tool can read files named by a path or vector-layer-list parameter.
pub fn reads_files(spec: &rustgis_analysis::ToolSpec) -> bool {
    spec.params.iter().any(|p| matches!(p.kind, rustgis_analysis::ParamKind::Path | rustgis_analysis::ParamKind::VectorLayers))
}

/// Whether a tool produces a file rather than a map layer.
pub fn writes_files(spec: &rustgis_analysis::ToolSpec) -> bool {
    spec.params.iter().any(|p| matches!(p.kind, rustgis_analysis::ParamKind::SaveFile { .. }))
}

/// A browser file export waiting for delivery, without a success record until acknowledged.
pub(crate) struct PendingFile {
    pub(crate) id: u64,
    pub(crate) file_name: String,
    pub(crate) bytes: Option<Vec<u8>>,
    record: GpRecord,
}

#[cfg(any(target_arch = "wasm32", test))]
fn queue_file(s: &mut Session, file_name: String, bytes: Vec<u8>, record: GpRecord) -> CmdResult {
    let file_name =
        record.params.get("output").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()).unwrap_or(&file_name).to_string();
    if file_name.is_empty() || file_name.contains(['/', '\\']) {
        return Err("output must be a file name, not a path, in the browser".into());
    }
    const MAX_PENDING_BYTES: usize = 256 * 1024 * 1024;
    let held = s.file_outputs.iter().filter_map(|f| f.bytes.as_ref()).map(Vec::len).sum::<usize>();
    if s.file_outputs.len() >= 8 || bytes.len() > MAX_PENDING_BYTES.saturating_sub(held) {
        return Err("file export queue limit reached (8 files or 256 MiB); split the task, model, or script into smaller runs".into());
    }
    s.next_pending = s.next_pending.wrapping_add(1);
    let id = s.next_pending;
    s.file_outputs.push(PendingFile { id, file_name: file_name.clone(), bytes: Some(bytes), record });
    s.request_only = true;
    Ok(json!({"pending": true, "download": id, "file": file_name}))
}

fn file_delivered(s: &mut Session, p: &Value) -> CmdResult {
    let id = p.get("id").and_then(Value::as_u64).ok_or("id is required")?;
    let i = s.file_outputs.iter().position(|f| f.id == id).ok_or("no pending file with that id")?;
    if s.file_outputs[i].bytes.is_some() {
        return Err("the file has not been handed to the download service".into());
    }
    let mut file = s.file_outputs.remove(i);
    let error = p.get("error").and_then(Value::as_str);
    file.record.ok = error.is_none();
    if let Some(error) = error {
        file.record.messages.push(error.to_string());
    }
    if let Some(report) = &mut s.model_run {
        report.file_delivered(id, error);
    }
    for message in &file.record.messages {
        s.notify(message.clone(), !file.record.ok);
    }
    let out = json!({"file": file.file_name, "delivered": file.record.ok, "messages": file.record.messages});
    s.project.gp_history.push(file.record);
    if s.project.gp_history.len() > 500 {
        s.project.gp_history.remove(0);
    }
    Ok(out)
}

/// The parameters as the history keeps them: a file picked in the browser (`{"name", "data"}`)
/// by its name, not its contents.
fn recorded(spec: Option<&rustgis_analysis::ToolSpec>, mut params: Value) -> Value {
    for p in spec.into_iter().flat_map(|t| t.params).filter(|p| p.kind == rustgis_analysis::ParamKind::Path) {
        if let Some(v) = params.get_mut(p.name)
            && v.get("data").is_some()
        {
            *v = v.get("name").cloned().unwrap_or(Value::Null);
        }
    }
    params
}

/// What ran, as the history keeps it: the tool's name and id, and the command, when not
/// `gp.run`.
struct Ran {
    name: String,
    id: String,
    command: Option<(String, Value)>,
    /// When it started, in milliseconds since 1970: the time the history shows, also for a run
    /// finished later (a plugin's, a suite's).
    started: u64,
}

#[cfg(not(target_arch = "wasm32"))]
fn write_file_output(s: &Session, path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    // A new file has no canonical path yet: resolve its folder, so a file created inside a loaded
    // source directory is refused as well as one replaced there.
    let resolved = path.canonicalize().or_else(|error| {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
        path.file_name().map(|name| parent.canonicalize().map(|p| p.join(name))).unwrap_or(Err(error))
    });
    if let Ok(destination) = resolved {
        if s.path.iter().chain(s.opened_package.iter()).any(|project| project.canonicalize().is_ok_and(|project| project == destination)) {
            return Err("destination is the open project's file; choose a different output file".into());
        }
        for map in &s.project.maps {
            for layer in &map.layers {
                let sources = [
                    layer.source.as_ref().and_then(|source| source.path.as_deref()),
                    layer.dataset.source.as_deref(),
                    layer.raster.as_ref().and_then(|raster| raster.path.as_deref()),
                ];
                if sources.into_iter().flatten().any(|source| names_file(source, &destination)) {
                    return Err(format!("destination is the source of layer '{}'; choose a different output file", layer.name));
                }
            }
            for tile in &map.tiles {
                let archive = tile.url.strip_prefix("file://").unwrap_or(tile.url.as_str());
                let local = matches!(tile.kind, rustgis_map::TileKind::PmTiles | rustgis_map::TileKind::MbTiles);
                if local && rustgis_map::tiles::is_local_archive(&tile.url) && names_file(archive, &destination) {
                    return Err(format!("destination is the archive of tile layer '{}'; choose a different output file", tile.name));
                }
            }
        }
        for table in &s.project.tables {
            if table.dataset.source.as_deref().is_some_and(|source| names_file(source, &destination)) {
                return Err(format!("destination is the source of table '{}'; choose a different output file", table.dataset.name));
            }
        }
    }
    rustgis_data::io::write_atomic(path, bytes).map_err(|error| error.to_string())
}

/// Whether a loaded source reads `destination`: the file itself, a directory's contents, the
/// archive a ZIP member's path (`archive.zip/member`) lies in, or a Shapefile's companion file.
#[cfg(not(target_arch = "wasm32"))]
fn names_file(source: &str, destination: &std::path::Path) -> bool {
    let source = std::path::Path::new(source);
    if source.canonicalize().is_ok_and(|p| destination.starts_with(p))
        || source.ancestors().skip(1).any(|p| p.canonicalize().is_ok_and(|p| p.as_path() == destination))
    {
        return true;
    }
    let shapefile = source.extension().is_some_and(|e| e.eq_ignore_ascii_case("shp"));
    let companion = |ext: &str| source.with_extension(ext).canonicalize().is_ok_and(|p| p.as_path() == destination);
    shapefile && ["dbf", "shx", "prj", "cpg"].iter().any(|&ext| companion(ext) || companion(ext.to_ascii_uppercase().as_str()))
}

/// Adds a tool's result to the map and the history.
fn apply(s: &mut Session, ran: Ran, params: Value, res: Result<rustgis_analysis::ToolResult, String>, millis: u64) -> CmdResult {
    let Ran { name, id, command, started } = ran;
    let at = started / 1000;
    let mut result = match res {
        Err(e) => {
            s.project.gp_history.push(GpRecord {
                tool: name,
                id,
                params,
                command,
                at,
                ok: false,
                messages: vec![e.clone()],
                millis,
                outputs: Vec::new(),
            });
            return Err(e);
        }
        Ok(r) => r,
    };
    let out = match result.output {
        Output::File { file_name, bytes } => {
            #[cfg(test)]
            if s.queue_gp_files {
                return queue_file(
                    s,
                    file_name,
                    bytes,
                    GpRecord { tool: name, id, params, command, at, ok: false, messages: result.messages, millis, outputs: Vec::new() },
                );
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let path = params.get("output").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()).unwrap_or(&file_name);
                if let Err(error) = write_file_output(s, std::path::Path::new(path), &bytes) {
                    let error = format!("{path}: {error}");
                    result.messages.push(error.clone());
                    s.project.gp_history.push(GpRecord {
                        tool: name,
                        id,
                        params,
                        command,
                        at,
                        ok: false,
                        messages: result.messages,
                        millis,
                        outputs: Vec::new(),
                    });
                    return Err(error);
                }
                json!({"file": path, "bytes": bytes.len()})
            }
            #[cfg(target_arch = "wasm32")]
            {
                return queue_file(
                    s,
                    file_name,
                    bytes,
                    GpRecord { tool: name, id, params, command, at, ok: false, messages: result.messages, millis, outputs: Vec::new() },
                );
            }
        }
        Output::Layer(ds) => {
            let id = super::layer::add_dataset(s, ds);
            json!({"layer": id})
        }
        Output::Selection { layer, rows } => {
            let id = s.map().layer_by_name(&layer).map(|l| l.id).ok_or("the input layer disappeared")?;
            let n = rows.len();
            super::select::set(s, id, rows, super::select::Mode::New);
            json!({"selected": n})
        }
        Output::Update { layer, dataset } => {
            let l = s.map_mut().layers.iter_mut().find(|l| l.name.eq_ignore_ascii_case(&layer)).ok_or("the input layer disappeared")?;
            l.dataset = dataset;
            // Under the layer's name now: a background run's layer may have been renamed meanwhile.
            l.dataset.name.clone_from(&l.name);
            // A streamed layer updated by a tool holds the whole result in memory from now on.
            l.dataset.windowed = false;
            l.source = None;
            l.selection.clear();
            l.touch_geometry();
            json!({"updated": layer})
        }
        Output::Table(t) => {
            let n = t.name.clone();
            s.project.put_table(t);
            json!({"table": n})
        }
        Output::Layers(outputs) => {
            let mut layers = Vec::new();
            let mut tables = Vec::new();
            for o in outputs {
                match o {
                    OutputLayer::Vector(ds) => layers.push(super::layer::add_dataset(s, ds)),
                    OutputLayer::Raster { name, file_name, bytes } => {
                        // One unreadable output doesn't undo the others: it's reported instead.
                        let added = rustgis_data::raster::read_bytes(&file_name, &bytes)
                            .map_err(|e| format!("{name}: {e}"))
                            .and_then(|r| super::raster::add_raster(s, &name, rustgis_map::RasterLayer::from_bytes(r, file_name, bytes)));
                        match added {
                            Ok(v) => {
                                if let Some(id) = v.get("layer").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok()) {
                                    layers.push(id);
                                }
                            }
                            Err(e) => result.messages.push(e),
                        }
                    }
                    OutputLayer::Table(t) => {
                        let n = t.name.clone();
                        s.project.put_table(t);
                        tables.push(n);
                    }
                }
            }
            json!({"layers": layers, "tables": tables})
        }
    };
    for m in &result.messages {
        s.notify(m.clone(), false);
    }
    let ids: Vec<u32> = match (out.get("layer"), out.get("layers")) {
        (Some(id), _) => id.as_u64().into_iter().filter_map(|v| u32::try_from(v).ok()).collect(),
        (None, Some(Value::Array(v))) => v.iter().filter_map(Value::as_u64).filter_map(|v| u32::try_from(v).ok()).collect(),
        _ => Vec::new(),
    };
    let outputs = ids.iter().filter_map(|id| s.map().layer(*id).map(|l| l.name.clone())).collect();
    s.project.gp_history.push(GpRecord { tool: name, id, params, command, at, ok: true, messages: result.messages, millis, outputs });
    if s.project.gp_history.len() > 500 {
        s.project.gp_history.remove(0);
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod file_tests {
    use super::*;

    #[test]
    fn raster_to_pmtiles_queues_the_archive_without_changing_the_map() {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let raster = Raster::new(
            "Elevation",
            4,
            4,
            vec![(0..16).map(|v| v as f32).collect()],
            GeoTransform { origin_x: -180.0, origin_y: 80.0, pixel_width: 90.0, pixel_height: -40.0 },
            Crs::LonLat,
            None,
            ColorHint::Gray,
            "Float32",
        )
        .unwrap();
        let mut s = Session::new(crate::Project::default());
        let id = s.map_mut().add_raster("Elevation", rustgis_map::RasterLayer::from_path(raster, "test.tif"));
        s.map_mut().layer_mut(id).unwrap().transparency = 50.0;
        let mut group = rustgis_map::LayerGroup::new("Faded");
        group.children.push(id);
        group.transparency = 50.0;
        s.map_mut().add_group(group);
        let raster = s.map().layer(id).unwrap().raster.as_ref().unwrap();
        let expected = crate::render::raster_export::export(raster, 0, 0, 0.25).unwrap();
        s.queue_gp_files = true;
        let result =
            s.run("gp.run", &json!({"tool": "rasterToPmtiles", "params": {"input": "Elevation", "minZoom": 0, "maxZoom": 0}})).unwrap();
        assert_eq!(result["pending"], true);
        let files = s.take_gp_files();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].1, "Elevation.pmtiles");
        assert!(files[0].2.starts_with(b"PMTiles\x03"));
        assert_eq!(files[0].2, expected, "layer and group transparency are both applied");
        assert_eq!(s.map().layers.len(), 1);
        assert!(
            s.run("gp.run", &json!({"tool": "rasterToPmtiles", "params": {"input": "Elevation", "minZoom": 2, "maxZoom": 1}})).is_err()
        );
        let missing =
            s.run("gp.run", &json!({"tool": "rasterToPmtiles", "params": {"input": "Missing", "minZoom": 0, "maxZoom": 0}})).unwrap_err();
        assert!(missing.contains("no raster layer named"), "{missing}");
        s.map_mut().layer_mut(id).unwrap().raster.as_mut().unwrap().data = None;
        let pending =
            s.run("gp.run", &json!({"tool": "rasterToPmtiles", "params": {"input": "Elevation", "minZoom": 0, "maxZoom": 0}})).unwrap_err();
        assert!(pending.contains("pixels are not loaded"), "{pending}");
    }

    fn record() -> GpRecord {
        GpRecord {
            tool: "File export".into(),
            id: "vectorToPmtiles".into(),
            params: json!({"input": "Cities"}),
            command: None,
            at: 1,
            ok: false,
            messages: vec!["Exported data".into()],
            millis: 3,
            outputs: Vec::new(),
        }
    }

    fn task_session(steps: Value) -> Session {
        let mut s = Session::new(crate::Project::default());
        let mut cities = Dataset::new("Cities", rustgis_data::GeometryType::Point, Vec::new());
        cities.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(20.0, 30.0))), Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(0, cities));
        s.queue_gp_files = true;
        s.run("task.new", &json!({"name": "Export task", "steps": steps})).unwrap();
        s.mark_saved();
        s
    }

    #[test]
    fn task_with_a_prior_edit_and_final_queued_export_has_one_restorable_undo_step() {
        let mut s = task_session(json!([
            {"title": "Make buffer", "command": "gp.bufferPoint",
             "params": {"lon": 20, "lat": 30, "distance": 100, "name": "Task buffer"}},
            {"title": "Export buffer", "command": "gp.run",
             "params": {"tool": "vectorToPmtiles", "params": {"input": "Task buffer", "minZoom": 0, "maxZoom": 0}}}
        ]));
        let depth = s.undo_depth();
        let result = s.run("task.run", &json!({"task": "Export task"})).unwrap();
        assert_eq!(result["ran"][1]["result"]["pending"], true);
        assert!(s.map().layer_by_name("Task buffer").is_some());
        assert_eq!(s.project.gp_history.len(), 1, "the buffer completed, the export has not");
        assert!(s.dirty());
        assert_eq!(s.undo_depth(), depth + 1, "direct step handlers share one task transaction");
        let files = s.take_gp_files();
        assert_eq!(files.len(), 1);
        assert!(files[0].2.starts_with(b"PMTiles\x03"), "the task ran the real exporter");
        assert!(s.undo());
        assert!(s.map().layer_by_name("Task buffer").is_none());
        assert_eq!(s.map().layers.len(), 1);
        assert!(s.project.gp_history.is_empty());
        assert!(!s.dirty(), "undo restores the saved pre-task project");
    }

    #[test]
    fn file_only_task_with_multiple_exports_and_a_view_step_stays_request_only() {
        let mut s = task_session(json!([
            {"title": "Export first", "command": "gp.run",
             "params": {"tool": "vectorToPmtiles", "params": {"input": "Cities", "minZoom": 0, "maxZoom": 0}}},
            {"title": "Export second", "command": "gp.run",
             "params": {"tool": "vectorToPmtiles", "params": {"input": "Cities", "minZoom": 0, "maxZoom": 0}}},
            {"title": "List tools", "command": "gp.tools"}
        ]));
        let depth = s.undo_depth();
        let result = s.run("task.run", &json!({"task": "Export task"})).unwrap();
        assert_eq!(result["ran"][0]["result"]["pending"], true);
        assert_eq!(result["ran"][1]["result"]["pending"], true);
        assert_eq!(s.take_gp_files().len(), 2);
        assert!(s.project.gp_history.is_empty());
        assert!(!s.dirty());
        assert_eq!(s.undo_depth(), depth);
    }

    #[test]
    fn queueing_a_file_is_not_an_edit_before_delivery() {
        let mut s = Session::new(crate::Project::default());
        s.mark_saved();
        let depth = s.undo_depth();
        let pending = s.guarded("File export", true, |s| queue_file(s, "cities.pmtiles".into(), vec![1, 2, 3], record())).unwrap();
        assert!(!s.dirty(), "generated bytes alone do not change the project");
        assert_eq!(s.undo_depth(), depth, "queueing must not add an empty undo step");
        assert!(s.project.gp_history.is_empty());
        s.take_gp_files();
        let out = s.run("gp.fileDelivered", &json!({"id": pending["download"]})).unwrap();
        assert_eq!(out["delivered"], true);
        assert!(s.dirty(), "acknowledged delivery adds history");
        assert_eq!(s.undo_depth(), depth + 1);
        assert_eq!(s.project.gp_history.len(), 1);
    }

    #[test]
    fn file_download_history_waits_for_delivery_and_records_failure() {
        let mut s = Session::new(crate::Project::default());
        let pending = queue_file(&mut s, "cities.pmtiles".into(), vec![1, 2, 3], record()).unwrap();
        let id = pending["download"].as_u64().unwrap();
        assert_eq!(pending["pending"], true);
        assert!(s.project.gp_history.is_empty());
        assert!(s.run("gp.fileDelivered", &json!({"id": id})).is_err());
        assert!(s.project.gp_history.is_empty());
        assert_eq!(s.take_gp_files(), vec![(id, "cities.pmtiles".into(), vec![1, 2, 3])]);
        assert!(s.take_gp_files().is_empty(), "bytes are handed over once");
        let out = s.run("gp.fileDelivered", &json!({"id": id, "error": "browser rejected download"})).unwrap();
        assert_eq!(out["delivered"], false);
        assert_eq!(s.project.gp_history.len(), 1);
        assert!(!s.project.gp_history[0].ok);
        assert!(s.project.gp_history[0].outputs.is_empty());
        assert!(s.project.gp_history[0].messages.iter().any(|m| m == "browser rejected download"));
        assert!(s.run("gp.fileDelivered", &json!({"id": id})).is_err(), "acknowledged once");
    }

    #[test]
    fn file_download_success_and_project_replacement() {
        let mut s = Session::new(crate::Project::default());
        let pending = queue_file(&mut s, "cities.pmtiles".into(), vec![4], record()).unwrap();
        let id = pending["download"].as_u64().unwrap();
        s.take_gp_files();
        let out = s.run("gp.fileDelivered", &json!({"id": id})).unwrap();
        assert_eq!(out["delivered"], true);
        assert!(s.project.gp_history[0].ok);
        assert!(s.project.gp_history[0].outputs.is_empty(), "a file is not a layer");
        let old = queue_file(&mut s, "old.pmtiles".into(), vec![5], record()).unwrap();
        s.run("project.new", &json!({})).unwrap();
        assert!(s.take_gp_files().is_empty());
        assert!(s.run("gp.fileDelivered", &json!({"id": old["download"]})).is_err());
    }

    #[test]
    fn failed_commands_drop_pending_file_deliveries() {
        let mut s = Session::new(crate::Project::default());
        let r = s.guarded("Failed export task", true, |s| {
            queue_file(s, "cancelled.pmtiles".into(), vec![1], record())?;
            Err("task cancelled".into())
        });
        assert!(r.is_err());
        assert!(s.take_gp_files().is_empty());
        assert!(s.project.gp_history.is_empty());
        for _ in 0..8 {
            queue_file(&mut s, "bounded.pmtiles".into(), vec![1], record()).unwrap();
        }
        assert!(queue_file(&mut s, "too-many.pmtiles".into(), vec![1], record()).is_err());
        assert_eq!(s.take_gp_files().len(), 8);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_export_preserves_the_saved_project_file() {
        let root = std::env::temp_dir().join(format!("rustgis-project-output-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let project = root.join("Map.rgis");
        let mut s = Session::new(crate::Project::default());
        let mut points = Dataset::new("Points", rustgis_data::GeometryType::Point, Vec::new());
        points.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(20.0, 30.0))), Vec::new());
        s.map_mut().add_dataset(points);
        s.run("project.saveAs", &json!({"path": project})).unwrap();
        let original = std::fs::read(&project).unwrap();
        let destinations = vec![project.clone(), root.join("nested").join("..").join("Map.rgis")];
        #[cfg(unix)]
        let destinations = {
            let mut destinations = destinations;
            let alias = root.join("alias.rgis");
            std::os::unix::fs::symlink(&project, &alias).unwrap();
            destinations.push(alias);
            destinations
        };
        for destination in destinations {
            let result = s.run(
                "gp.run",
                &json!({"tool": "vectorToPmtiles", "params": {
                    "input": "Points", "minZoom": 0, "maxZoom": 0, "output": destination
                }}),
            );
            assert!(result.is_err(), "export must refuse the saved project: {result:?}");
            assert!(result.unwrap_err().contains("open project's file"));
            assert_eq!(std::fs::read(&project).unwrap(), original);
            assert_eq!(s.path.as_deref(), Some(project.as_path()));
            assert!(s.project.gp_history.last().is_some_and(|record| !record.ok));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_export_preserves_the_opened_project_package() {
        let root = std::env::temp_dir().join(format!("rustgis-package-output-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let package = root.join("Map.rgispkg");
        let mut s = Session::new(crate::Project::default());
        let mut points = Dataset::new("Points", rustgis_data::GeometryType::Point, Vec::new());
        points.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(20.0, 30.0))), Vec::new());
        s.map_mut().add_dataset(points.clone());
        s.run("project.package", &json!({"path": package})).unwrap();
        let original = std::fs::read(&package).unwrap();
        s.run("project.open", &json!({"path": package})).unwrap();
        assert!(s.path.is_none(), "opening a package must still require Save As");
        assert!(s.run("project.save", &Value::Null).is_err());
        let destinations = vec![package.clone(), root.join("nested").join("..").join("Map.rgispkg")];
        #[cfg(unix)]
        let destinations = {
            let mut destinations = destinations;
            let alias = root.join("alias.rgispkg");
            std::os::unix::fs::symlink(&package, &alias).unwrap();
            destinations.push(alias);
            destinations
        };
        let export = |s: &mut Session, destination: &std::path::Path| {
            s.run(
                "gp.run",
                &json!({"tool": "vectorToPmtiles", "params": {
                    "input": "Points", "minZoom": 0, "maxZoom": 0, "output": destination
                }}),
            )
        };
        for destination in destinations {
            let result = export(&mut s, &destination);
            assert!(result.is_err(), "export must refuse the opened package: {result:?}");
            assert!(result.unwrap_err().contains("open project's file"));
            assert_eq!(std::fs::read(&package).unwrap(), original);
            assert!(s.project.gp_history.last().is_some_and(|record| !record.ok));
        }
        let failed = s.guarded("Failed project replacement", true, |s| {
            s.run("project.new", &json!({}))?;
            Err("later step failed".into())
        });
        assert!(failed.is_err());
        assert!(export(&mut s, &package).is_err(), "rollback must restore package protection");
        assert_eq!(std::fs::read(&package).unwrap(), original);
        assert!(s.path.is_none());
        let saved = root.join("Saved.rgis");
        s.run("project.saveAs", &json!({"path": saved})).unwrap();
        assert_eq!(s.path.as_deref(), Some(saved.as_path()));
        assert!(export(&mut s, &package).is_err(), "Save As must not forget the package origin");
        assert_eq!(std::fs::read(&package).unwrap(), original);
        s.run("project.new", &json!({})).unwrap();
        s.map_mut().add_dataset(points);
        export(&mut s, &package).unwrap();
        assert!(std::fs::read(&package).unwrap().starts_with(b"PMTiles\x03"), "the previous package is no longer open");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn points_session() -> Session {
        let mut s = Session::new(crate::Project::default());
        let mut points = Dataset::new("Points", rustgis_data::GeometryType::Point, Vec::new());
        points.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(20.0, 30.0))), Vec::new());
        s.map_mut().add_dataset(points);
        s
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_project_origins_survive_cwd_changes() {
        if std::env::var_os("RUSTGIS_TEST_PROJECT_ORIGIN_CWD").is_none() {
            let root = std::env::temp_dir().join(format!("rustgis-origin-cwd-{}-{}", std::process::id(), now_ms()));
            std::fs::create_dir_all(root.join("elsewhere")).unwrap();
            let mut s = points_session();
            s.run("project.saveAs", &json!({"path": root.join("Opened.rgis")})).unwrap();
            s.run("project.package", &json!({"path": root.join("Pack.rgispkg")})).unwrap();
            // cwd is process-wide: never change it in the parallel suite's parent process.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "cmd::gp::file_tests::native_project_origins_survive_cwd_changes", "--nocapture"])
                .env("RUSTGIS_TEST_PROJECT_ORIGIN_CWD", "1")
                .current_dir(&root)
                .output()
                .unwrap();
            std::fs::remove_dir_all(root).unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
            assert!(stdout.contains("1 passed; 0 failed"), "the isolated child must exercise the guard: {stdout}");
            return;
        }
        let root = std::env::current_dir().unwrap();
        for (mode, name) in [("package", "Pack.rgispkg"), ("project", "Opened.rgis"), ("save", "Saved.rgis")] {
            std::env::set_current_dir(&root).unwrap();
            let mut s = Session::new(crate::Project::default());
            let input = if mode == "save" { "Opened.rgis" } else { name };
            s.run("project.open", &json!({"path": input})).unwrap();
            if mode == "save" {
                s.run("project.saveAs", &json!({"path": name})).unwrap();
            }
            if mode == "package" {
                assert!(s.path.is_none(), "the anchored package is still not a save destination");
            }
            let destination = root.join(name);
            let original = std::fs::read(&destination).unwrap();
            std::env::set_current_dir(root.join("elsewhere")).unwrap();
            let result = s.run(
                "gp.run",
                &json!({"tool": "vectorToPmtiles", "params": {
                    "input": "Points", "minZoom": 0, "maxZoom": 0, "output": destination
                }}),
            );
            assert!(result.is_err(), "{mode}: cwd changes must not allow overwriting the origin: {result:?}");
            assert!(result.unwrap_err().contains("open project's file"));
            assert_eq!(std::fs::read(&destination).unwrap(), original);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_project_origins_follow_undo_redo() {
        let root = std::env::temp_dir().join(format!("rustgis-origin-undo-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        for package in [true, false] {
            let mut s = points_session();
            let source = root.join(if package { "Pack.rgispkg" } else { "Saved.rgis" });
            if package {
                s.run("project.package", &json!({"path": source})).unwrap();
                s.run("project.open", &json!({"path": source})).unwrap();
            } else {
                s.run("project.saveAs", &json!({"path": source})).unwrap();
            }
            let original = std::fs::read(&source).unwrap();
            let points = s.map().layer_by_name("Points").unwrap().dataset.clone();
            s.guarded("Replace project", true, |s| s.run("project.new", &json!({"name": "Other"}))).unwrap();
            assert!(s.undo());
            let result = s.run(
                "gp.run",
                &json!({"tool": "vectorToPmtiles", "params": {
                    "input": "Points", "minZoom": 0, "maxZoom": 0, "output": source
                }}),
            );
            assert!(result.is_err(), "undo must restore the project's file protection: {result:?}");
            assert!(result.unwrap_err().contains("open project's file"));
            assert_eq!(std::fs::read(&source).unwrap(), original);
            assert!(s.redo());
            assert!(s.map().layer_by_name("Points").is_none());
            s.run("script.run", &json!({"script": "edit.undo {}"})).unwrap();
            assert!(s.map().layer_by_name("Points").is_some());
            assert!(s.undo(), "undoing a script that used undo must restore its original file origins");
            assert!(s.map().layer_by_name("Points").is_none());
            s.map_mut().add_dataset(points);
            s.run(
                "gp.run",
                &json!({"tool": "vectorToPmtiles", "params": {
                    "input": "Points", "minZoom": 0, "maxZoom": 0, "output": source
                }}),
            )
            .unwrap();
            assert!(std::fs::read(&source).unwrap().starts_with(b"PMTiles\x03"), "redo must not retain the previous project's guard");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_project_origins_survive_failed_nested_undo() {
        let root = std::env::temp_dir().join(format!("rustgis-origin-rollback-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        let mut s = points_session();
        let first = root.join("First.rgispkg");
        let second = root.join("Second.rgispkg");
        for source in [&first, &second] {
            s.run("project.package", &json!({"path": source})).unwrap();
        }
        let original = std::fs::read(&second).unwrap();
        s.run("project.open", &json!({"path": first})).unwrap();
        let open_second = format!("project.open {}", json!({"path": second}));
        s.run("script.run", &json!({"script": open_second})).unwrap();
        assert!(s.run("script.run", &json!({"script": "edit.undo {}\nmissing.command {}"})).is_err());
        let result = s.run(
            "gp.run",
            &json!({"tool": "vectorToPmtiles", "params": {
                "input": "Points", "minZoom": 0, "maxZoom": 0, "output": second
            }}),
        );
        assert!(result.is_err(), "a failed script must restore the active package origin: {result:?}");
        assert!(result.unwrap_err().contains("open project's file"));
        assert_eq!(std::fs::read(&second).unwrap(), original);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_project_origins_restore_unsaved_after_replacement_saved_later() {
        let root = std::env::temp_dir().join(format!("rustgis-origin-later-save-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        for nested in [false, true] {
            let mut s = points_session();
            s.project.name = "A".into();
            s.run("script.run", &json!({"script": "project.new {\"name\":\"B\"}"})).unwrap();
            if nested {
                s.run("script.run", &json!({"script": "edit.undo {}"})).unwrap();
            }
            let saved = root.join(if nested { "A.rgis" } else { "B.rgis" });
            s.run("project.saveAs", &json!({"path": saved})).unwrap();
            let original = std::fs::read(&saved).unwrap();
            assert!(s.undo());
            assert_eq!(s.project.name, if nested { "B" } else { "A" });
            assert!(s.run("project.save", &Value::Null).is_err(), "the restored unsaved project must not save over the later destination");
            assert_eq!(std::fs::read(&saved).unwrap(), original);
            assert!(s.redo());
            assert_eq!(s.path.as_deref(), Some(saved.as_path()));
            s.run("project.save", &Value::Null).unwrap();
            assert_eq!(crate::Project::load(&saved).unwrap().name, if nested { "A" } else { "B" });
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_project_origin_keeps_save_as_after_nonreplacement_undo() {
        let root = std::env::temp_dir().join(format!("rustgis-origin-save-as-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        for round_trip in [false, true] {
            let mut s = points_session();
            if round_trip {
                let points = s.map().layer_by_name("Points").unwrap().dataset.clone();
                s.run("script.run", &json!({"script": "project.new {}"})).unwrap();
                s.map_mut().add_dataset(points);
                s.run("script.run", &json!({"script": "edit.undo {}\nedit.redo {}"})).unwrap();
            } else {
                s.run("map.rename", &json!({"name": "Edited map"})).unwrap();
            }
            let saved = root.join(if round_trip { "Roundtrip.rgis" } else { "Edited.rgis" });
            s.run("project.saveAs", &json!({"path": saved})).unwrap();
            let original = std::fs::read(&saved).unwrap();
            assert!(s.undo());
            assert_eq!(s.path.as_deref(), Some(saved.as_path()), "undoing a nonreplacement must not undo a later Save As");
            let result = s.run(
                "gp.run",
                &json!({"tool": "vectorToPmtiles", "params": {
                    "input": "Points", "minZoom": 0, "maxZoom": 0, "output": saved
                }}),
            );
            assert!(result.is_err(), "the Save As destination must remain protected: {result:?}");
            assert_eq!(std::fs::read(&saved).unwrap(), original);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_project_origins_keep_earliest_files_when_steps_merge() {
        let root = std::env::temp_dir().join(format!("rustgis-origin-merge-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        let mut s = points_session();
        let first = root.join("A.rgis");
        let second = root.join("B.rgis");
        let third = root.join("C.rgis");
        s.run("project.saveAs", &json!({"path": first})).unwrap();
        s.merge_undo("file-edits");
        s.run("map.rename", &json!({"name": "Edited map"})).unwrap();
        for destination in [&second, &third] {
            s.merge_undo("file-edits");
            let script = format!("project.saveAs {}", json!({"path": destination}));
            s.run("script.run", &json!({"script": script})).unwrap();
        }
        let second_bytes = std::fs::read(&second).unwrap();
        let third_bytes = std::fs::read(&third).unwrap();
        assert!(s.undo());
        assert_eq!(s.path.as_deref(), Some(first.as_path()), "merged origin changes must retain the earliest destination");
        s.run("project.save", &Value::Null).unwrap();
        assert_eq!(std::fs::read(&second).unwrap(), second_bytes);
        assert_eq!(std::fs::read(&third).unwrap(), third_bytes);
        assert!(s.redo());
        assert_eq!(s.path.as_deref(), Some(third.as_path()), "redo must restore the final destination");
        s.run("project.save", &Value::Null).unwrap();
        assert_eq!(crate::Project::load(&third).unwrap().name, "C");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_export_preserves_standalone_table_sources() {
        let root = std::env::temp_dir().join(format!("rustgis-table-output-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let source = root.join("Counts.csv");
        let bytes = b"name,value\nalpha,7\n";
        std::fs::write(&source, bytes).unwrap();
        let mut s = Session::new(crate::Project::default());
        let mut points = Dataset::new("Points", rustgis_data::GeometryType::Point, Vec::new());
        points.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(20.0, 30.0))), Vec::new());
        s.map_mut().add_dataset(points);
        s.run("layer.addData", &json!({"path": source})).unwrap();
        let table = s.project.tables.first().unwrap();
        assert_eq!(table.dataset.name, "Counts");
        assert_eq!(table.dataset.source.as_deref(), source.to_str());
        for destination in [&source, &root.join("nested").join("..").join("Counts.csv")] {
            let result = s.run(
                "gp.run",
                &json!({"tool": "vectorToPmtiles", "params": {
                    "input": "Points", "minZoom": 0, "maxZoom": 0, "output": destination
                }}),
            );
            assert!(result.is_err(), "export must refuse a loaded table source: {result:?}");
            assert!(result.unwrap_err().contains("source of table 'Counts'"));
            assert_eq!(std::fs::read(&source).unwrap(), bytes);
            assert!(s.project.gp_history.last().is_some_and(|record| !record.ok));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_export_preserves_loaded_workbooks() {
        let root = std::env::temp_dir().join(format!("rustgis-workbook-output-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let book = root.join("Counts.xlsx");
        let bytes = rustgis_data::sheets::xlsx(&[("Counts", &[&["name", "value"], &["alpha", "7"]])]);
        std::fs::write(&book, &bytes).unwrap();
        let mut s = points_session();
        s.run("layer.addData", &json!({"path": book})).unwrap();
        assert_eq!(s.project.tables.first().map(|t| t.dataset.source.as_deref()), Some(book.to_str()));
        for destination in [&book, &root.join("nested").join("..").join("Counts.xlsx")] {
            let result = s.run(
                "gp.run",
                &json!({"tool": "vectorToPmtiles", "params": {
                    "input": "Points", "minZoom": 0, "maxZoom": 0, "output": destination
                }}),
            );
            assert!(result.is_err(), "export must refuse a loaded workbook: {result:?}");
            assert!(result.unwrap_err().contains("source of table"));
            assert_eq!(std::fs::read(&book).unwrap(), bytes);
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_export_preserves_loaded_source_directories() {
        let root = std::env::temp_dir().join(format!("rustgis-directory-output-{}-{}", std::process::id(), now_ms()));
        let source = root.join("Store.gdb");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        let catalog = source.join("a00000001.gdbtable");
        let rows = source.join("nested").join("rows.bin");
        std::fs::write(&catalog, b"source catalog").unwrap();
        std::fs::write(&rows, b"source rows").unwrap();
        let mut s = points_session();
        let mut table = Dataset::new("Counts", rustgis_data::GeometryType::None, Vec::new());
        table.source = Some(source.display().to_string());
        s.project.add_table(table);
        let destinations = vec![catalog.clone(), source.join("nested").join("..").join("a00000001.gdbtable"), rows.clone()];
        #[cfg(unix)]
        let destinations = {
            let mut destinations = destinations;
            let alias = root.join("database-link");
            std::os::unix::fs::symlink(&source, &alias).unwrap();
            destinations.push(alias.join("a00000001.gdbtable"));
            let file_alias = root.join("catalog-link.pmtiles");
            std::os::unix::fs::symlink(&catalog, &file_alias).unwrap();
            destinations.push(file_alias);
            destinations
        };
        for destination in destinations {
            let result = s.run(
                "gp.run",
                &json!({"tool": "vectorToPmtiles", "params": {
                    "input": "Points", "minZoom": 0, "maxZoom": 0, "output": destination
                }}),
            );
            assert!(result.is_err(), "export must refuse a file in a loaded source directory: {result:?}");
            assert!(result.unwrap_err().contains("source of table 'Counts'"));
            assert_eq!(std::fs::read(&catalog).unwrap(), b"source catalog");
            assert_eq!(std::fs::read(&rows).unwrap(), b"source rows");
        }
        for created in [source.join("new.pmtiles"), source.join("nested").join("new.pmtiles")] {
            let result = s.run(
                "gp.run",
                &json!({"tool": "vectorToPmtiles", "params": {
                    "input": "Points", "minZoom": 0, "maxZoom": 0, "output": created
                }}),
            );
            assert!(result.is_err(), "export must refuse a new file in a loaded source directory: {result:?}");
            assert!(!created.exists(), "nothing may be written into the source directory");
        }
        let sibling = root.join("Store.gdb.pmtiles");
        std::fs::write(&sibling, b"old output").unwrap();
        s.run(
            "gp.run",
            &json!({"tool": "vectorToPmtiles", "params": {
                "input": "Points", "minZoom": 0, "maxZoom": 0, "output": sibling
            }}),
        )
        .unwrap();
        assert!(std::fs::read(&sibling).unwrap().starts_with(b"PMTiles\x03"), "a shared name prefix is not directory membership");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_export_preserves_loaded_sources_and_path_aliases() {
        let root = std::env::temp_dir().join(format!("rustgis-source-output-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let source = root.join("sites.geojson");
        let bytes = br#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"name":"Site"},"geometry":{"type":"Point","coordinates":[20,30]}}]}"#;
        std::fs::write(&source, bytes).unwrap();
        let destinations = vec![source.clone(), root.join("nested").join("..").join("sites.geojson")];
        #[cfg(unix)]
        let destinations = {
            let mut destinations = destinations;
            let alias = root.join("alias.geojson");
            std::os::unix::fs::symlink(&source, &alias).unwrap();
            destinations.push(alias);
            destinations
        };
        for stream in [false, true] {
            let mut s = Session::new(crate::Project::default());
            s.run("layer.addData", &json!({"path": source, "name": "Sites", "stream": stream})).unwrap();
            for destination in &destinations {
                let error = s
                    .run(
                        "gp.run",
                        &json!({"tool": "vectorToPmtiles", "params": {
                            "input": "Sites", "minZoom": 0, "maxZoom": 0, "output": destination
                        }}),
                    )
                    .unwrap_err();
                assert!(error.contains("source of layer 'Sites'"), "{error}");
                assert_eq!(std::fs::read(&source).unwrap(), bytes, "source must survive a refused export");
                assert!(s.project.gp_history.last().is_some_and(|record| !record.ok));
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn source_paths_name_zip_archives_and_shapefile_companions() {
        let root = std::env::temp_dir().join(format!("rustgis-source-names-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&root).unwrap();
        let archive = root.join("roads.zip");
        let companion = root.join("roads.DBF");
        std::fs::write(&archive, b"zip").unwrap();
        std::fs::write(&companion, b"dbf").unwrap();
        let archive_file = archive.canonicalize().unwrap();
        let companion_file = companion.canonicalize().unwrap();
        assert!(names_file(&archive.join("roads.shp").display().to_string(), &archive_file));
        assert!(names_file(&root.join("roads.shp").display().to_string(), &companion_file));
        assert!(!names_file(&root.join("roads.dbf").display().to_string(), &archive_file));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_file_exports_are_atomic_and_never_map_layers() {
        let mut s = Session::new(crate::Project::default());
        let root = std::env::temp_dir().join(format!("rustgis-gp-file-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir(&root).unwrap();
        let destination = root.join("cities.pmtiles");
        std::fs::write(&destination, b"previous").unwrap();
        let file = || {
            Ok(rustgis_analysis::ToolResult {
                output: Output::File { file_name: "cities.pmtiles".into(), bytes: b"archive".to_vec() },
                messages: vec!["Exported data".into()],
            })
        };
        let ran = || Ran { name: "File export".into(), id: "vectorToPmtiles".into(), command: None, started: 1000 };
        let params = json!({"output": destination.display().to_string()});
        let out = apply(&mut s, ran(), params, file(), 3).unwrap();
        assert_eq!(out["bytes"], 7);
        assert_eq!(std::fs::read(&destination).unwrap(), b"archive");
        assert!(s.map().layers.is_empty());
        assert!(s.project.gp_history[0].ok);
        assert!(s.project.gp_history[0].outputs.is_empty());
        let occupied = root.join("occupied");
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("keep"), b"unchanged").unwrap();
        let params = json!({"output": occupied.display().to_string()});
        assert!(s.guarded("File export", true, |s| apply(s, ran(), params, file(), 3)).is_err());
        assert_eq!(std::fs::read(occupied.join("keep")).unwrap(), b"unchanged");
        assert_eq!(s.project.gp_history.len(), 2, "a failed save must be recorded");
        let failed = &s.project.gp_history[1];
        assert!(!failed.ok);
        assert_eq!(failed.id, "vectorToPmtiles");
        assert_eq!(failed.params["output"], occupied.display().to_string());
        assert_eq!(failed.at, 1);
        assert_eq!(failed.millis, 3);
        assert!(failed.outputs.is_empty());
        assert_eq!(failed.messages.first().map(String::as_str), Some("Exported data"));
        assert!(failed.messages.last().is_some_and(|m| m.contains(occupied.to_string_lossy().as_ref())));
        let previous = s.project.gp_history[0].clone();
        s.project.gp_history.resize(500, previous);
        let params = json!({"output": occupied.display().to_string()});
        assert!(
            s.guarded("Failed export task", true, |s| { s.guarded("File export", true, |s| apply(s, ran(), params, file(), 3)) }).is_err()
        );
        assert_eq!(s.project.gp_history.len(), 500);
        assert!(s.project.gp_history.last().is_some_and(|record| !record.ok));
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2, "no partial staging file");
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod file_permission_tests {
    use super::*;

    #[test]
    fn file_tools_are_file_access_even_without_an_output_path() {
        let s = Session::new(crate::Project::default());
        let tool = rustgis_analysis::tool("vectorToPmtiles").expect("registered file export tool");
        assert!(writes_files(tool));
        assert!(!reads_files(tool), "output files are not input paths");
        for output in [None, Some("archive"), Some("cities.pmtiles")] {
            let mut params = json!({"input": "Cities", "minZoom": 0, "maxZoom": 0});
            if let Some(output) = output {
                params["output"] = json!(output);
            }
            let p = json!({"tool": "vectorToPmtiles", "params": params});
            assert!(crate::cmd::items::touches_files("gp.run", &p));
            let reaches = s.registry.get("gp.run").and_then(|spec| crate::cmd::plugins::reaches(spec, &p));
            assert!(matches!(reaches, Some(crate::cmd::plugins::Reach::Files)));
        }
        let node = crate::project::ModelNode {
            kind: crate::project::ModelNodeKind::Tool,
            tool: Some("vectorToPmtiles".into()),
            ..Default::default()
        };
        assert!(crate::model::output_ports(&node).is_empty(), "files must not be wired as layers");
    }
}
