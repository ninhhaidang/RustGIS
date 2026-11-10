//! Analyze tab: run geoprocessing tools and keep their history.

use rustgis_analysis::{Inputs, Output, OutputLayer, RasterInput};
use rustgis_data::Dataset;
use serde_json::{Value, json};

use super::{need_layers, ps};
use crate::project_log::now_ms;
use crate::{CmdResult, CommandSpec as C, GpRecord, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("gp.run", "Run Tool", "Analyze › Geoprocessing", run).roles(&[crate::ParamRole { key: "params", role: crate::Role::ToolParams }]).effects(crate::policy::Effects::NONE).when(need_layers).params(r#"{"tool": tool id, "params": {...} (a layer by name; a standalone table by name or id)}"#),
        // A batch reads files it names (inputs, a folder) and runs a tool many times, which the
        // reviewed plan doesn't expand item by item: it stays the user's to run, asked each time.
        C::new("gp.batch", "Batch Run", "Analyze › Geoprocessing", batch)
            .roles(&[crate::ParamRole { key: "params", role: crate::Role::ToolParams }])
            .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files).with(crate::policy::Effect::Automation))
            .params(
                r#"{"tool": tool id, "inputs"?: [layer names or file paths] | "*", "folder"?: folder path, "param"?: input parameter name (default: the tool's first layer parameter, else "input"), "batch_param"?: input parameter name, "outputPattern"?: text (default "{input}_{tool}"), "output_pattern"?: text, "params"?: {...}}"#,
            ),
        C::new("gp.spatialJoin", "Spatial Join", "Analyze › Overlay", spatial_join).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_layers)
            .params(
                r#"{"target": layer, "join": layer, "match"?: "Intersects" (default) | "Contains" | "Within", "operation"?: "Join one to one" (default) | "Join one to many", "keepAll"?: "Keep all" (default) | "Keep only matching", "prefix"?: "join_", "output"?: text}"#,
            ),
        C::new("gp.zonalStats", "Zonal Statistics", "Analyze › Raster", zonal_stats).roles(&[]).effects(crate::policy::Effects::NONE)
            .when(need_layers)
            .params(
                r#"{"raster": raster, "polygons": polygon layer, "bands"?: [1-based band numbers] or single band (default 1), "stats"?: ["mean", "min", "max", "sum", "count", "stddev"] or "all" (default "all"), "prefix"?: text, "output"?: text}"#,
            ),
        C::new("gp.bufferPoint", "Buffer A Point", "Analyze › Geoprocessing", buffer_point).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"lon": number, "lat": number, "distance": number, "units"?: "Kilometers" (default) | "Meters" | "Miles" | "Feet", "name"?: output}"#,
        ),
        C::new("gp.tools", "Tools", "Analyze › Geoprocessing", |_, _| {
            Ok(Value::Array(
                rustgis_analysis::published_tools()
                    .map(|t| {
                        json!({"id": t.id, "name": t.name, "toolbox": t.toolbox, "summary": t.summary,
                            "params": t.params.iter().map(|p| json!({"name": p.name, "label": p.label, "required": p.required, "kind": format!("{:?}", p.kind)})).collect::<Vec<_>>()})
                    })
                    .collect(),
            ))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view(),
        C::new("gp.geolibre", "GeoLibre Tools", "Analyze › Geoprocessing", |_, _| Ok(geolibre_status())).roles(&[]).effects(crate::policy::Effects::NONE).view(),
        C::new("gp.installGeolibre", "Install GeoLibre Tools", "Analyze › Geoprocessing", |_, _| {
            let path = install_geolibre(&|_, _| {})?;
            #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
            rustgis_geolibre::set_runner(Some(std::path::PathBuf::from(&path)))?;
            Ok(json!({"path": path, "status": geolibre_status()}))
        }).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved))
        .view(),
        C::new("gp.uninstallGeolibre", "Uninstall GeoLibre Tools", "Analyze › Geoprocessing", |_, _| uninstall_geolibre()).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view(),
        C::new("gp.finish", "Finish Tool Run", "Analyze › Geoprocessing", finish).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"id": a finished run (the browser's GeoLibre tools run asynchronously)}"#),
        C::new("gp.fileDelivered", "Finish File Export", "Analyze › Geoprocessing", file_delivered).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"id": pending file id, "error"?: download failure message}"#),
        C::new("gp.setGeolibre", "Locate GeoLibre Runner", "Analyze › Geoprocessing", set_geolibre).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved).with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"path"?: "the geolibre executable (empty: look in the usual places)"}"#),
        C::new("gp.clearHistory", "Clear History", "Analyze › Geoprocessing › History", |s, _| {
            s.project.gp_history.clear();
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE),
    ]
}

/// The GeoLibre tools (empty when the suite isn't installed, or the build doesn't include it).
pub fn geolibre_tools() -> &'static [rustgis_analysis::ToolSpec] {
    #[cfg(feature = "geolibre")]
    {
        rustgis_analysis::published_external_tools_of(rustgis_geolibre::SOURCE)
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
        rustgis_analysis::published_external_tools_of(rustgis_geolibre::SOURCE).iter().any(|t| std::ptr::eq(t, spec))
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
        let found = rustgis_geolibre::runner::selected();
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
#[derive(Clone)]
pub struct ExternalRun {
    continuation: crate::policy::Continuation,
    pub id: u64,
    pub tool: String,
    pub argv: Vec<String>,
    pub files: Vec<(String, Vec<u8>)>,
}

impl ExternalRun {
    pub fn can_start(&self) -> Result<(), String> {
        self.continuation.check()
    }
    /// An owned principal-free predicate for preparation and the actual browser tool start.
    pub fn still(&self) -> crate::render::rustgis_tiles::cache::Still {
        let c = self.continuation.clone();
        std::sync::Arc::new(move || c.check().is_ok())
    }
}

/// The new files and printed lines of a finished run, or the runner's error.
pub type ExternalOutput = Result<(Vec<(String, Vec<u8>)>, Vec<String>), String>;

/// Prepares a run of a GeoLibre tool on the active map, checking its parameters as `gp.run`
/// does; the session keeps it until [`external_complete`].
pub fn external_start(s: &mut Session, tool: &str, params: &Value) -> Result<ExternalRun, String> {
    #[cfg(feature = "geolibre")]
    {
        let op = s.authorize_tool(tool, params)?;
        let continuation = s.tool_continuation(&op)?;
        let crate::policy::ToolOp { pin, params } = op.op();
        let spec = pin.spec();
        if pin.resolved().source() != Some(rustgis_geolibre::SOURCE) {
            return Err(format!("no GeoLibre tool '{tool}'"));
        }
        let params = params.clone();
        let job = preparing(&continuation, || pin.prepare_suite(&map_inputs(s, Some(spec), &params)?, &params))?;
        s.next_pending = s.next_pending.wrapping_add(1);
        let id = s.next_pending;
        let run = ExternalRun {
            id,
            tool: job.tool.clone(),
            argv: job.argv("/work"),
            files: job.files.clone(),
            continuation: continuation.clone(),
        };
        s.started.push((id, job, params, now_ms(), continuation));
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
        s.check_delivery(&s.started.get(i).ok_or("no started tool with that id")?.4)?;
        let (_, job, params, t0, continuation) = s.started.remove(i);
        s.check_continuation(&continuation)?;
        // Reading the runner's files back parses outside `Session::run`'s guard: a bug in a
        // reader must not take the app down.
        let result = ran.map_err(|e| format!("{}: {e}", job.name)).and_then(|(files, lines)| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rustgis_geolibre::finish(&job, &files, &lines, None)))
                .map_err(|_| format!("{}: its output couldn't be read", job.name))
        });
        let tool_id = format!("{}{}", rustgis_geolibre::PREFIX, job.tool);
        s.pending.push(crate::PendingRun {
            id,
            continuation,
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
    continuation: crate::policy::Continuation,
    pub id: u64,
    /// The tool's id.
    pub tool: &'static str,
    job: rustgis_plugins::tools::Job,
}

impl PluginRun {
    /// Runs the tool, blocking until it ends; `control` stops it and shows its progress.
    pub fn run(&self, control: &rustgis_plugins::tools::Control) -> Result<rustgis_analysis::ToolResult, String> {
        self.continuation.check()?;
        // A bug in a tool must not take the app down.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.job.run(control)))
            .unwrap_or_else(|_| Err(format!("{} failed unexpectedly", self.job.name())))
    }
}

/// A tool's run on another thread, started and not yet complete: a plugin tool's
/// ([`plugin_start`]) or a built-in one's ([`tool_start`]).
pub(crate) struct StartedRun {
    continuation: crate::policy::Continuation,
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

/// Checks and carries the admitted owner through every synchronous input/preparation request.
fn preparing<T>(continuation: &crate::policy::Continuation, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    continuation.check()?;
    let owner = continuation.clone();
    let still = std::sync::Arc::new(move || owner.check().is_ok()) as crate::render::rustgis_tiles::cache::Still;
    crate::render::rustgis_tiles::cache::with_still(still, || {
        let result = f()?;
        continuation.check()?;
        Ok(result)
    })
}

/// Prepares a run of a plugin tool on the active map, as `gp.run` would run it: its parameters
/// checked and its layers read now, so that the map can change while it runs.
pub fn plugin_start(s: &mut Session, tool: &str, params: &Value) -> Result<PluginRun, String> {
    let op = s.authorize_tool(tool, params)?;
    let continuation = s.tool_continuation(&op)?;
    let crate::policy::ToolOp { pin, params: resolved } = op.op();
    let spec = pin.spec();
    let plugin = pin.plugin().ok_or_else(|| format!("no plugin tool '{tool}'"))?;
    let (_, tid) = rustgis_plugins::split_id(spec.id).ok_or("invalid plugin tool id")?;
    let (job, full_ids) = preparing(&continuation, || {
        let inputs = map_inputs(s, Some(spec), resolved)?;
        let job = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rustgis_plugins::tools::prepare_pinned(plugin.plugin(), tid, &inputs, resolved)
        }))
        .unwrap_or_else(|_| Err(format!("{} failed unexpectedly", spec.name)))?;
        Ok((job, full_ids(&inputs.loaded)))
    })?;
    let read = read_layers(s, spec, resolved);
    s.next_pending = s.next_pending.wrapping_add(1);
    let id = s.next_pending;
    let params = recorded(Some(spec), params.clone());
    let map = (s.map().instance_id(), s.map().name.clone());
    s.plugin_runs.push(StartedRun {
        continuation: continuation.clone(),
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
    Ok(PluginRun { id, tool: spec.id, job, continuation })
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

/// Where a sealed tool result lands; payload parameters cannot select this route.
#[derive(Clone, Copy)]
pub(crate) enum Landing {
    Failure,
    Project,
    Map,
}
/// Classifies the actual owned output, shared by routing and the completion authority check.
pub(crate) fn landing(result: &Result<rustgis_analysis::ToolResult, String>) -> Landing {
    match result {
        Err(_) => Landing::Failure,
        Ok(r) => match &r.output {
            Output::Layer(_) | Output::Selection { .. } | Output::Update { .. } => Landing::Map,
            Output::Layers(out) if out.iter().any(|o| !matches!(o, rustgis_analysis::OutputLayer::Table(_))) => Landing::Map,
            _ => Landing::Project,
        },
    }
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
    s.check_gp_delivery(&s.plugin_runs.get(i).ok_or("no started tool with that id")?.continuation)?;
    let mut run = s.plugin_runs.remove(i);
    s.refit_gp_sources(&mut run.continuation, &run.read)?;
    let mut result = result;
    // What it adds to or changes on the map belongs on the map it ran on, whatever its name or
    // place now: not another made since with its name.
    let on_map = matches!(landing(&result), Landing::Map);
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
        continuation: run.continuation,
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
    pin: crate::policy::ToolPin,
    continuation: crate::policy::Continuation,
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
        let ToolRun { pin, continuation, snap, params, .. } = self;
        let spec = pin.spec();
        let name = spec.name;
        if let Err(e) = continuation.check() {
            return ToolDone { result: Err(e), full_ids: FullIds::new() };
        }
        if control.is_cancelled() {
            return ToolDone { result: Err(format!("{name} was cancelled before it started")), full_ids: FullIds::new() };
        }
        let current = continuation.clone();
        let still = std::sync::Arc::new(move || current.check().is_ok()) as crate::render::rustgis_tiles::cache::Still;
        // A bug in reading the inputs, like one in a tool, must not take the app down.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::render::rustgis_tiles::cache::with_still(still, || {
                let inputs = match inputs_of(snap, Some(spec), &params) {
                    Ok(inputs) => inputs,
                    Err(e) => return ToolDone { result: Err(e), full_ids: FullIds::new() },
                };
                let mut result = pin.run(&inputs, &params, control, &|| continuation.check());
                if let Ok(r) = &mut result {
                    r.messages.extend(inputs.messages.iter().cloned());
                }
                ToolDone { result, full_ids: full_ids(&inputs.loaded) }
            })
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
    let op = s.authorize_tool(tool, params)?;
    let continuation = s.tool_continuation(&op)?;
    let crate::policy::ToolOp { pin, params: resolved } = op.op();
    let pin = pin.clone();
    let resolved = resolved.clone();
    let spec = pin.spec();
    if pin.plugin().is_some() {
        return Err(format!("no built-in or suite tool '{tool}'"));
    }
    let read = read_layers(s, spec, &resolved);
    s.next_pending = s.next_pending.wrapping_add(1);
    let id = s.next_pending;
    let recorded_params = recorded(Some(spec), params.clone());
    let full_ids = FullIds::new();
    s.plugin_runs.push(StartedRun {
        continuation: continuation.clone(),
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
    Ok(ToolRun { id, tool: spec.id, snap: Snapshot::owned(s), params: resolved, pin, continuation })
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

#[cfg(not(target_arch = "wasm32"))]
/// A tool's run made ahead on a copy of the session, for a model or script the app runs on a
/// thread of its own ([`super::workflows::ahead_on_copy`]): what it read and what it made, for
/// the run here to take instead of running the tool again.
pub(crate) struct RanAhead {
    pin: crate::policy::ToolPin,
    params: Value,
    read: Snapshot<'static>,
    /// The view's extent, when the tool asked for it.
    view: Option<Option<rustgis_geom::Extent>>,
    result: Result<rustgis_analysis::ToolResult, String>,
    full_ids: FullIds,
    millis: u64,
}

#[cfg(not(target_arch = "wasm32"))]
/// The tools of a model or script run ahead: recorded on the copy, then taken in order by the run
/// here.
pub(crate) enum Ahead {
    Recording(Vec<RanAhead>),
    Taking(std::collections::VecDeque<RanAhead>),
}

#[cfg(not(target_arch = "wasm32"))]
/// What a model's or script's run on a copy of the session stops at, as its run here does them: a
/// tool that writes files (which would be written twice), a plugin tool (a plugin's program may do
/// more than hand back its layers, so it runs once, where the user ran it), and a script's line that may reach outside the session. A
/// GeoLibre tool runs ahead like a built-in one (#1562): the outputs it declares are written to a
/// scratch folder of its own and handed back, so the run here takes the result instead of running
/// it again, and Cancel stops its program. (The runner is trusted like any program the user
/// installs: a tool asked to write elsewhere through its parameters would, and would again if its
/// result isn't taken.)
pub(crate) const NOT_AHEAD: &str = "runs where the model or script runs";

#[cfg(not(target_arch = "wasm32"))]
/// A file a batch read on a copy of the session ([`super::workflows::ahead_on_copy`]), for the
/// batch's run here to take instead of reading it again.
pub(crate) struct FileAhead {
    path: std::path::PathBuf,
    /// The file's size and modification time when it was read: one changed since is read again.
    stamp: Option<(u64, std::time::SystemTime)>,
    read: Result<(Dataset, rustgis_data::ReadReport), String>,
}

#[cfg(not(target_arch = "wasm32"))]
/// A file's size and modification time, when the system gives them.
fn file_stamp(path: &std::path::Path) -> Option<(u64, std::time::SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

#[cfg(not(target_arch = "wasm32"))]
/// The files a batch of a model or script run ahead read: recorded on the copy, then taken in
/// order by the run here.
pub(crate) enum FilesAhead {
    Recording(Vec<FileAhead>),
    Taking(std::collections::VecDeque<FileAhead>),
}

#[cfg(not(target_arch = "wasm32"))]
/// What a model's or script's run on a copy of the session made: its tools' runs and the files
/// its batches read, in order.
pub(crate) struct RanAheads {
    tools: Vec<RanAhead>,
    files: Vec<FileAhead>,
    zones: Vec<super::imagery::ZoneMeans>,
}

#[cfg(not(target_arch = "wasm32"))]
/// Runs `run` taking the tools `ran` ahead: each one whose inputs are still the same as the run
/// ahead read them is taken; from the first that isn't, the tools run here. A batch takes the
/// files read ahead the same way. Restored on every path, a panic included (raised again), so no
/// later run takes them.
pub(crate) fn taking<T>(s: &mut Session, ran: RanAheads, run: impl FnOnce(&mut Session) -> T) -> T {
    s.tools_ahead = Some(Ahead::Taking(ran.tools.into()));
    s.files_ahead = Some(FilesAhead::Taking(ran.files.into()));
    s.zones_ahead = Some(super::imagery::ZonesAhead::Taking(ran.zones.into()));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(s)));
    s.tools_ahead = None;
    s.files_ahead = None;
    s.zones_ahead = None;
    r.unwrap_or_else(|p| std::panic::resume_unwind(p))
}

#[cfg(not(target_arch = "wasm32"))]
/// What a copy of the session recorded ([`Ahead::Recording`], [`FilesAhead::Recording`]), taken.
pub(crate) fn ran_ahead(s: &mut Session) -> RanAheads {
    let tools = match s.tools_ahead.take() {
        Some(Ahead::Recording(ran)) => ran,
        _ => Vec::new(),
    };
    let files = match s.files_ahead.take() {
        Some(FilesAhead::Recording(read)) => read,
        _ => Vec::new(),
    };
    let zones = match s.zones_ahead.take() {
        Some(super::imagery::ZonesAhead::Recording(read)) => read,
        _ => Vec::new(),
    };
    RanAheads { tools, files, zones }
}

#[cfg(not(target_arch = "wasm32"))]
/// Reads a batch's input file: on a copy of the session, recorded for the run here
/// ([`FilesAhead::Recording`]); there, the copy's read of it when it is the next one taken and
/// the file hasn't changed since (its size and modification time), else read here, as are the
/// rest (whose tools then run here too, on what is read now).
fn read_batch_file(s: &mut Session, path: &std::path::Path) -> Result<(Dataset, rustgis_data::ReadReport), String> {
    if let Some(FilesAhead::Taking(files)) = &mut s.files_ahead {
        match files.pop_front() {
            Some(next) if next.path == path && next.stamp.is_some() && next.stamp == file_stamp(path) => return next.read,
            _ => s.files_ahead = None,
        }
    }
    let stamp = file_stamp(path);
    let read = rustgis_data::io::read_path(path).map_err(|e| e.to_string());
    // The dataset's rows are shared with the copy's layer, not copied.
    if let Some(FilesAhead::Recording(files)) = &mut s.files_ahead {
        files.push(FileAhead { path: path.to_path_buf(), stamp, read: read.clone() });
    }
    read
}

#[cfg(not(target_arch = "wasm32"))]
/// The next tool run ahead, if it is this one on the same inputs: its result, the streamed rows
/// it read whole and how long it took. Any other drops the rest, which came after it.
fn take_ahead(
    s: &mut Session,
    pin: &crate::policy::ToolPin,
    params: &Value,
) -> Option<(Result<rustgis_analysis::ToolResult, String>, FullIds, u64)> {
    let Some(Ahead::Taking(ran)) = &mut s.tools_ahead else { return None };
    let next = ran.pop_front()?;
    let now = Snapshot::of(s);
    let view = next.view.is_none_or(|v| v == view_extent(&now));
    if next.pin == *pin && next.params == *params && view && same_read(&next.read, &now) {
        #[cfg(test)]
        {
            s.tools_taken += 1;
        }
        return Some((next.result, next.full_ids, next.millis));
    }
    s.tools_ahead = None;
    None
}

#[cfg(not(target_arch = "wasm32"))]
/// Whether a tool reading `now` reads what it read in `was`: the same layers with the same rows
/// (for a streamed layer read whole, the same source), selections, rasters and projection, the
/// same tables and environment.
fn same_read(was: &Snapshot, now: &Snapshot) -> bool {
    let layer = |a: &rustgis_map::Layer, b: &rustgis_map::Layer| {
        let rows = match (&a.source, &b.source) {
            // Read whole from its source, unless it has a selection (rows of the window).
            (Some(x), Some(y)) if a.selection.is_empty() => x.same_opening(y) && a.dataset.fields == b.dataset.fields,
            (None, None) => Read::unchanged(&a.dataset, &b.dataset),
            _ => false,
        } && a.selection == b.selection;
        let raster = match (&a.raster, &b.raster) {
            (Some(x), Some(y)) => {
                x.path == y.path
                    && match (&x.data, &y.data) {
                        (Some(p), Some(q)) => std::sync::Arc::ptr_eq(p, q),
                        (None, None) => true,
                        _ => false,
                    }
            }
            (None, None) => true,
            _ => false,
        };
        a.id == b.id && a.name == b.name && a.definition_query == b.definition_query && rows && raster
    };
    let (m, n) = (&was.map, &now.map);
    m.layers.len() == n.layers.len()
        && m.layers.iter().zip(&n.layers).all(|(a, b)| layer(a, b))
        && m.projection == n.projection
        && was.tables.len() == now.tables.len()
        && was.tables.iter().zip(now.tables.iter()).all(|(a, b)| a.id == b.id && Read::unchanged(&a.dataset, &b.dataset))
        && was.env == now.env
}

/// The view's longitude/latitude extent, as a tool asks for it ([`Inputs::view_extent`]): the
/// globe shows the whole Earth (the camera's flat extent means nothing there).
fn view_extent(snap: &Snapshot) -> Option<rustgis_geom::Extent> {
    if snap.map.projection.is_globe() {
        return Some(rustgis_geom::Extent::new(-180.0, -90.0, 180.0, 90.0));
    }
    Some(super::workspace::view_extent_of(&snap.map, snap.view_size)).filter(|e| !e.is_empty())
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
    if now.instance_id() != was.instance || now.dataset.source != was.source || now.linked_table() != was.table.as_deref() {
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
    /// Numeric ids may be reused after removal; only this incarnation can receive an update.
    instance: u64,
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
    /// Loading the same streamed source whole changes its representation, not its origin.
    pub(crate) fn materialized(&self, id: u32, instance: u64, now: &rustgis_map::Layer) -> bool {
        self.id == id
            && self.instance == instance
            && self.streamed
            && !now.is_streamed()
            && now.dataset.source == self.source
            && now.linked_table() == self.table.as_deref()
    }
    /// Retains the same in-memory incarnation and source while names and streamed windows move.
    fn of(l: &rustgis_map::Layer) -> Self {
        Read {
            id: l.id,
            instance: l.instance_id(),
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
    s.check_gp_finish(&s.pending[i].continuation, landing(&s.pending[i].result))?;
    let r = s.pending.remove(i);
    let c = r.continuation.clone();
    s.resume_gp(&c, landing(&r.result), |s| {
        apply(s, Ran { name: r.tool, id: r.tool_id, command: None, started: r.started }, r.params, r.result, r.millis)
    })?
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
    if let Some(spec) = spec.filter(|t| rustgis_analysis::TOOLS.iter().any(|builtin| std::ptr::eq(builtin, *t))) {
        for parameter in spec.params.iter().filter(|p| p.kind == rustgis_analysis::ParamKind::VectorLayers) {
            for name in rustgis_analysis::layer_list(params.get(parameter.name).unwrap_or(&Value::Null))? {
                if vector_file_input(spec, &snap.map, name) && !full.contains_key(name) {
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
        if let Some(l) = map.layer_by_name(n).filter(|l| reads_full_source(l, spec)) {
            full.insert(key, l.load_all().map_err(|e| format!("{}: {e}", l.name))?);
        }
    }
    Ok(full)
}

/// The executor's source-read choice, shared with pure permission planning.
/// Selected in-memory windows need no whole read, except for the PMTiles exporter.
pub(crate) fn reads_full_source(layer: &rustgis_map::Layer, spec: Option<&rustgis_analysis::ToolSpec>) -> bool {
    layer.is_streamed() && (layer.selection.is_empty() || spec.is_some_and(|t| t.id == "vectorToPmtiles"))
}

/// The layers a tool's parameters name: the values of its layer parameters (all of them, for a
/// tool without a spec), so a field or text value that happens to match a layer's name isn't one.
pub(crate) fn layer_names<'v>(spec: Option<&rustgis_analysis::ToolSpec>, params: &'v Value) -> Vec<&'v str> {
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
            .or_else(|| project_input(&self.snap.map, &self.snap.tables, layer, false).map(|i| i.dataset()))
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
        *self.view.get_or_init(|| view_extent(&self.snap))
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

/// Whether `inputs` is the wildcard `"*"` (either as a string or a single-element array `["*"]`).
pub(crate) fn is_wildcard_inputs(v: Option<&Value>) -> bool {
    v.and_then(Value::as_str).is_some_and(|val| val == "*")
        || v.and_then(Value::as_array).is_some_and(|arr| arr.len() == 1 && arr.first().and_then(Value::as_str) == Some("*"))
}

/// The most inputs one `gp.batch` runs over.
pub(crate) const MAX_BATCH_ITEMS: usize = 10_000;

/// Whether a folder batch runs files with this lowercase extension.
#[cfg(not(target_arch = "wasm32"))]
fn is_folder_target_ext(ext: &str) -> bool {
    matches!(ext, "shp" | "geojson" | "json" | "gpkg" | "fgb" | "parquet" | "geoparquet" | "kml" | "kmz" | "gpx" | "csv")
}

/// Whether a file with this lowercase extension may be a Shapefile or GeoPackage sidecar.
#[cfg(not(target_arch = "wasm32"))]
fn is_folder_sidecar_ext(ext: &str) -> bool {
    matches!(ext, "dbf" | "shx" | "prj" | "cpg" | "qpj" | "sbn" | "sbx" | "qix" | "xml" | "gpkg-wal" | "gpkg-shm" | "wal" | "shm")
}

/// Runs a geoprocessing tool in batch across multiple layers or files: under the control of the
/// run it is part of when there is one (a script the app runs on a thread of its own, with Cancel
/// in the status bar), so cancelling that run stops the batch between items and the batch says
/// how many it ran; under its own otherwise.
fn batch(s: &mut Session, p: &Value) -> CmdResult {
    use rustgis_analysis::control::Control;
    let control = rustgis_analysis::control::current().unwrap_or_else(|| if s.stoppable { Control::stoppable() } else { Control::new() });
    run_batch_with(s, p, &control)
}

/// Runs a geoprocessing tool in batch across multiple layers or files with cancellation and progress reporting.
pub fn run_batch_with(s: &mut Session, p: &Value, control: &std::sync::Arc<rustgis_analysis::control::Control>) -> CmdResult {
    let tool_name = ps(p, "tool").ok_or("tool is required")?;
    let tool_id = tool_id(tool_name).ok_or_else(|| format!("no tool '{tool_name}'"))?;
    let spec = rustgis_analysis::tool(tool_id).ok_or_else(|| format!("no tool '{tool_name}'"))?;

    if p.get("param").is_some_and(|v| !v.is_null() && !v.is_string()) {
        s.notify("gp.batch: 'param' must be a string, ignoring invalid value", false);
    }
    if p.get("batch_param").is_some_and(|v| !v.is_null() && !v.is_string()) {
        s.notify("gp.batch: 'batch_param' must be a string, ignoring invalid value", false);
    }
    if p.get("outputPattern").is_some_and(|v| !v.is_null() && !v.is_string()) {
        s.notify("gp.batch: 'outputPattern' must be a string, ignoring invalid value", false);
    }
    if p.get("output_pattern").is_some_and(|v| !v.is_null() && !v.is_string()) {
        s.notify("gp.batch: 'output_pattern' must be a string, ignoring invalid value", false);
    }
    if p.get("folder").is_some_and(|v| !v.is_null() && !v.is_string()) {
        s.notify("gp.batch: 'folder' must be a string, ignoring invalid value", false);
    }

    if let Value::Object(map) = p {
        let recognized = ["tool", "param", "batch_param", "outputPattern", "output_pattern", "params", "inputs", "folder"];
        let mut unknown_keys: Vec<&str> = map.keys().map(String::as_str).filter(|k| !recognized.contains(k)).collect();
        if !unknown_keys.is_empty() {
            unknown_keys.sort();
            let count = unknown_keys.len();
            let sample = unknown_keys.first().copied().unwrap_or_default();
            let plural = if count == 1 { "option" } else { "options" };
            s.notify(format!("gp.batch: {count} unrecognised {plural} ignored ({sample})"), false);
        }
    }

    // Identify which parameter to bind the input to.
    let param_opt = ps(p, "param");
    let batch_param_opt = ps(p, "batch_param");
    if param_opt.is_some() && batch_param_opt.is_some() {
        s.notify("gp.batch: 'batch_param' ignored because 'param' is set", false);
    }
    let batch_param = param_opt.or(batch_param_opt).unwrap_or_else(|| {
        spec.params
            .iter()
            .find(|param| {
                matches!(
                    param.kind,
                    rustgis_analysis::ParamKind::Layer
                        | rustgis_analysis::ParamKind::PolygonLayer
                        | rustgis_analysis::ParamKind::PointLayer
                        | rustgis_analysis::ParamKind::VectorLayer
                        | rustgis_analysis::ParamKind::RasterLayer
                )
            })
            .map(|param| param.name)
            .unwrap_or("input")
    });

    if !rustgis_analysis::is_accepted_param(spec, batch_param) {
        return Err(format!("tool '{tool_id}' has no parameter '{batch_param}'"));
    }

    if writes_files(spec) {
        return Err(format!("tool '{tool_id}' writes a file, not a layer; gp.batch runs tools that add layers"));
    }

    let pattern_camel = ps(p, "outputPattern");
    let pattern_snake = ps(p, "output_pattern");
    if pattern_camel.is_some() && pattern_snake.is_some() {
        s.notify("gp.batch: 'output_pattern' ignored because 'outputPattern' is set", false);
    }
    let explicit_pattern = pattern_camel.or(pattern_snake);
    let pattern = explicit_pattern.unwrap_or("{input}_{tool}");
    if !pattern.contains("{input}") {
        return Err("outputPattern must include '{input}' to avoid output name collisions".into());
    }
    let tool_has_output = spec.params.iter().any(|param| param.name == "output");

    let base_params = match p.get("params").filter(|v| !v.is_null()) {
        Some(v) if v.is_object() => v.clone(),
        Some(_) => {
            s.notify("gp.batch: 'params' must be an object, ignoring invalid value", false);
            json!({})
        }
        None => json!({}),
    };
    if tool_has_output && base_params.get("output").is_some_and(|v| !v.is_null()) {
        s.notify("params.output is ignored for batch runs; outputPattern determines output names", false);
    }
    if base_params.get(batch_param).is_some_and(|v| !v.is_null()) {
        s.notify(format!("gp.batch: 'params.{batch_param}' is replaced by the batch inputs"), false);
    }
    // The other names the tool reads its input under are dropped from each item too.
    let shadowed: Vec<&str> = rustgis_analysis::param_aliases(spec, batch_param)
        .iter()
        .copied()
        .filter(|alias| *alias != batch_param && base_params.get(*alias).is_some_and(|v| !v.is_null()))
        .collect();
    if !shadowed.is_empty() {
        s.notify(
            format!(
                "gp.batch: {} other name(s) for '{batch_param}' in 'params' replaced by the batch inputs ({})",
                shadowed.len(),
                shadowed.join(", ")
            ),
            false,
        );
    }

    let mut unknown: Vec<&str> = base_params
        .as_object()
        .into_iter()
        .flat_map(|map| map.keys().map(String::as_str))
        .filter(|key| !rustgis_analysis::is_accepted_param(spec, key) && *key != batch_param)
        .collect();
    if !unknown.is_empty() {
        unknown.sort();
        let sample = unknown.first().copied().unwrap_or_default();
        s.notify(format!("gp.batch: {} unrecognised parameter(s) ignored in 'params' ({sample})", unknown.len()), false);
    }

    // Collect inputs: from "inputs" array or single string, wildcard "*", or "folder" directory path.
    let mut targets: Vec<String> = Vec::new();
    let mut dropped_inputs = 0;

    let is_wildcard = is_wildcard_inputs(p.get("inputs"));

    if is_wildcard {
        let mut skipped_rasters = 0;
        let mut skipped_graphics = 0;
        for layer in &s.map().layers {
            if layer.is_raster() {
                skipped_rasters += 1;
            } else if layer.graphics {
                skipped_graphics += 1;
            } else {
                targets.push(layer.name.clone());
            }
        }
        if skipped_rasters > 0 {
            s.notify(format!("gp.batch: {skipped_rasters} raster layer(s) left out of '*' (it runs vector layers)"), false);
        }
        if skipped_graphics > 0 {
            s.notify(format!("gp.batch: {skipped_graphics} graphics layer(s) left out of '*' (it runs vector layers)"), false);
        }
    } else if let Some(arr) = p.get("inputs").and_then(Value::as_array) {
        for item in arr {
            if let Some(str_val) = item.as_str() {
                if !str_val.trim().is_empty() {
                    targets.push(str_val.to_string());
                } else {
                    dropped_inputs += 1;
                }
            } else {
                dropped_inputs += 1;
            }
        }
    } else if let Some(single_str) = p.get("inputs").and_then(Value::as_str) {
        if !single_str.trim().is_empty() {
            targets.push(single_str.to_string());
        } else {
            dropped_inputs += 1;
        }
    } else if p.get("inputs").is_some_and(|v| !v.is_null()) {
        s.notify("gp.batch: 'inputs' must be an array of layer names/paths or a single layer name", false);
    }

    if dropped_inputs > 0 {
        s.notify(format!("gp.batch: {dropped_inputs} invalid input(s) skipped (not a valid layer name or file path)"), false);
    }

    let folder_given = ps(p, "folder").is_some_and(|f| !f.trim().is_empty());
    if folder_given && (is_wildcard || !targets.is_empty()) {
        s.notify("both 'inputs' and 'folder' were provided; running 'inputs' and ignoring 'folder'", false);
    }

    #[cfg(not(target_arch = "wasm32"))]
    if targets.is_empty()
        && !is_wildcard
        && let Some(folder) = ps(p, "folder").map(str::trim).filter(|f| !f.is_empty())
    {
        let dir = std::path::Path::new(folder);
        if !dir.is_dir() {
            return Err(format!("folder '{folder}' does not exist or is not a directory"));
        }
        let mut files = Vec::new();
        let mut skipped = 0;
        let mut subfolders = 0;
        let mut symlinks = 0;
        let mut non_utf8 = 0;
        let mut unreadable = 0;
        // The folder is read twice so only the files a batch runs are held, however many other
        // entries it has: the first pass finds them and the Shapefile/GeoPackage names, the
        // second counts the sidecar-like files that match neither as skipped.
        let mut shp_stems = std::collections::HashSet::new();
        let mut gpkg_stems = std::collections::HashSet::new();
        let entries = std::fs::read_dir(dir).map_err(|e| format!("folder '{folder}': {e}"))?;
        for entry in entries {
            let Ok(entry) = entry else {
                unreadable += 1;
                continue;
            };
            let Ok(ft) = entry.file_type() else {
                unreadable += 1;
                continue;
            };
            if ft.is_symlink() {
                symlinks += 1;
                continue;
            }
            let path = entry.path();
            if path.to_str().is_none() {
                non_utf8 += 1;
                continue;
            }
            if ft.is_dir() {
                subfolders += 1;
                continue;
            }
            if !ft.is_file() {
                unreadable += 1;
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if is_folder_target_ext(&ext) {
                if files.len() >= MAX_BATCH_ITEMS {
                    return Err(format!(
                        "folder '{folder}' has more than {MAX_BATCH_ITEMS} supported files, more than a batch runs; split it into smaller folders"
                    ));
                }
                let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("").to_ascii_lowercase();
                if ext == "shp" {
                    shp_stems.insert(stem);
                } else if ext == "gpkg" {
                    gpkg_stems.insert(stem);
                }
                files.push(path.display().to_string());
            } else if !is_folder_sidecar_ext(&ext) {
                skipped += 1;
            }
        }
        // Errors here were already counted by the first pass.
        let entries = std::fs::read_dir(dir).map_err(|e| format!("folder '{folder}': {e}"))?;
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if !ft.is_file() || ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            if path.to_str().is_none() {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if !is_folder_sidecar_ext(&ext) {
                continue;
            }
            let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("").to_ascii_lowercase();
            let is_shp_sidecar = (matches!(ext.as_str(), "dbf" | "shx" | "prj" | "cpg" | "qpj" | "sbn" | "sbx" | "qix")
                && shp_stems.contains(&stem))
                || (ext == "xml" && stem.strip_suffix(".shp").map(|s| shp_stems.contains(s)).unwrap_or(false));
            let is_gpkg_sidecar = matches!(ext.as_str(), "gpkg-wal" | "gpkg-shm" | "wal" | "shm")
                && (gpkg_stems.contains(&stem) || stem.strip_suffix(".gpkg").map(|s| gpkg_stems.contains(s)).unwrap_or(false));
            if !is_shp_sidecar && !is_gpkg_sidecar {
                skipped += 1;
            }
        }
        if symlinks > 0 {
            s.notify(format!("folder '{folder}': {symlinks} symlink(s) skipped"), false);
        }
        if non_utf8 > 0 {
            s.notify(format!("folder '{folder}': {non_utf8} file name(s) that are not valid UTF-8 skipped"), false);
        }
        if subfolders > 0 {
            s.notify(format!("folder '{folder}': {subfolders} subfolder(s) skipped"), false);
        }
        if skipped > 0 {
            s.notify(format!("folder '{folder}': {skipped} unsupported files skipped"), false);
        }
        if unreadable > 0 {
            s.notify(format!("folder '{folder}': {unreadable} unreadable directory entries skipped"), true);
        }
        files.sort();
        targets.extend(files);
    }

    #[cfg(target_arch = "wasm32")]
    if targets.is_empty() && !is_wildcard && folder_given {
        return Err("folder batch runs are not supported in the browser; specify 'inputs' as an array of layers".into());
    }

    if targets.is_empty() && is_wildcard {
        return Err("'*' matched no vector layers on the active map; add a vector layer or name the inputs".into());
    }

    #[cfg(not(target_arch = "wasm32"))]
    if targets.is_empty()
        && let Some(folder) = ps(p, "folder").map(str::trim).filter(|f| !f.is_empty())
    {
        return Err(format!("folder '{folder}' has no supported vector files"));
    }

    if targets.is_empty() {
        return Err(
            "no inputs provided (specify 'inputs' as an array of layers/paths, '*' for all layers, or 'folder' as a directory path)".into(),
        );
    }

    // Each item keeps its output as a layer: a hostile list or folder can't make the batch unbounded.
    if targets.len() > MAX_BATCH_ITEMS {
        return Err(format!(
            "{} inputs is more than a batch runs (at most {MAX_BATCH_ITEMS}); split it into smaller batches",
            targets.len()
        ));
    }

    let mut succeeded = Vec::new();
    let mut failed = Vec::new();
    let mut errors = Vec::new();
    let mut interrupted_error = None;
    let mut layer_ids = Vec::new();
    let mut output_names = Vec::new();
    let mut seen_outputs = std::collections::HashSet::new();
    let mut collisions = 0;
    let mut first_collision = None;
    #[allow(unused_mut)]
    let mut reader_warnings: Vec<(String, String)> = Vec::new();
    let mut items_reporting_messages = 0;
    let mut distinct_tool_messages: Vec<(String, usize)> = Vec::new();
    let mut cancelled_after = None;
    let mut next_suffix: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    #[cfg(not(target_arch = "wasm32"))]
    let mut trimmed_paths = 0;

    for (i, target) in targets.iter().enumerate() {
        if control.is_cancelled() {
            cancelled_after = Some(i);
            break;
        }

        let active_before = s.active_layer;
        let is_file_input = s.map().layer_by_name(target).is_none() && crate::project::find_table(&s.project.tables, target).is_none();

        #[cfg(not(target_arch = "wasm32"))]
        let mut item_reader_warnings: Vec<String> = Vec::new();

        #[cfg(not(target_arch = "wasm32"))]
        let mut temp_layer_id = {
            let trimmed = target.trim();
            let (file_path, was_trimmed) = if std::path::Path::new(target).is_file() {
                (Some(std::path::Path::new(target)), false)
            } else if trimmed != target && std::path::Path::new(trimmed).is_file() {
                (Some(std::path::Path::new(trimmed)), true)
            } else {
                (None, false)
            };
            if is_file_input && let Some(path) = file_path {
                if was_trimmed {
                    trimmed_paths += 1;
                }
                match read_batch_file(s, path) {
                    Ok((dataset, report)) => {
                        for msg in report.messages {
                            item_reader_warnings.push(format!("{target}: {msg}"));
                            reader_warnings.push((target.clone(), msg));
                        }
                        Some(super::layer::add_dataset(s, dataset))
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        let spec = rustgis_analysis::tool(tool_id);
                        let name = spec.map(|t| t.name).unwrap_or("Tool").to_string();
                        let mut rec_params = base_params.clone();
                        if let Value::Object(ref mut map) = rec_params {
                            map.insert(batch_param.to_string(), json!(target));
                        }
                        let mut record_json = json!({
                            "tool": tool_id,
                            "inputs": [target],
                            "params": base_params,
                            "param": batch_param,
                        });
                        if let Some(pat) = explicit_pattern {
                            record_json["outputPattern"] = json!(pat);
                        }
                        let record_cmd = Some(("gp.batch".to_string(), record_json));
                        s.push_gp_record(GpRecord {
                            tool: name,
                            id: tool_id.to_string(),
                            params: rec_params,
                            command: record_cmd,
                            at: now_ms() / 1000,
                            ok: false,
                            messages: vec![err_str.clone()],
                            millis: 0,
                            outputs: Vec::new(),
                        });
                        if s.project.gp_history.len() > 500 {
                            s.project.gp_history.remove(0);
                        }
                        failed.push(json!({"input": target, "error": err_str.clone()}));
                        errors.push(json!({"input": target, "error": err_str}));
                        s.active_layer = active_before;
                        continue;
                    }
                }
            } else {
                None
            }
        };
        #[cfg(target_arch = "wasm32")]
        let temp_layer_id: Option<u32> = None;

        let target_layer_name = {
            #[cfg(not(target_arch = "wasm32"))]
            {
                if let Some(lid) = temp_layer_id {
                    s.map().layer(lid).map(|l| l.name.clone()).unwrap_or_else(|| target.to_string())
                } else {
                    target.to_string()
                }
            }
            #[cfg(target_arch = "wasm32")]
            {
                target.to_string()
            }
        };

        let base_name = match s.map().layer_by_name(target) {
            Some(layer) => layer.name.as_str(),
            None if crate::project::find_table(&s.project.tables, target).is_some() => target.as_str(),
            None => std::path::Path::new(target.trim()).file_stem().and_then(|stem| stem.to_str()).unwrap_or(target.trim()),
        };
        let out_name = pattern.replace("{input}", base_name).replace("{tool}", tool_id);
        let mut final_out_name = out_name.clone();
        let mut had_collision = None;
        if tool_has_output {
            let collision_exists = |name: &str| {
                seen_outputs.contains(name)
                    || s.map().layer_by_name(name).map(|l| Some(l.id) != temp_layer_id).unwrap_or(false)
                    || crate::project::find_table(&s.project.tables, name).is_some()
            };
            if collision_exists(&final_out_name) {
                let stem = final_out_name.clone();
                let mut counter = next_suffix.get(&stem).copied().unwrap_or(2);
                while collision_exists(&format!("{stem}_{counter}")) {
                    counter += 1;
                }
                final_out_name = format!("{stem}_{counter}");
                had_collision = Some((stem, counter + 1));
            }
        }

        let mut item_params = base_params.clone();
        if let Value::Object(ref mut map) = item_params {
            // A tool reading the input under another name (Join Field's `input` for `target`) mustn't
            // take a static value given under it in place of each batch input.
            for alias in rustgis_analysis::param_aliases(spec, batch_param).iter().filter(|a| **a != batch_param) {
                map.remove(*alias);
            }
            map.insert(batch_param.to_string(), json!(target_layer_name));
            if tool_has_output {
                map.insert("output".to_string(), json!(final_out_name));
            }
        }

        let mut record_params = base_params.clone();
        if let Value::Object(ref mut map) = record_params {
            for alias in rustgis_analysis::param_aliases(spec, batch_param).iter().filter(|a| **a != batch_param) {
                map.remove(*alias);
            }
            map.insert(batch_param.to_string(), json!(target));
            if tool_has_output {
                map.insert("output".to_string(), json!(final_out_name));
            }
        }

        let cmd_record = if is_file_input {
            let mut record_json = json!({
                "tool": tool_id,
                "inputs": [target],
                "params": base_params,
                "param": batch_param,
            });
            if let Some(pat) = explicit_pattern {
                record_json["outputPattern"] = json!(pat);
            }
            Some(("gp.batch".to_string(), record_json))
        } else {
            None
        };

        #[cfg(not(target_arch = "wasm32"))]
        let mut res_was_selected = false;

        match run_tool_quiet(s, tool_id, &item_params, Some(&record_params), cmd_record.clone(), control) {
            Ok((res, messages)) => {
                succeeded.push(target.clone());
                if !messages.is_empty() {
                    items_reporting_messages += 1;
                    for msg in &messages {
                        if let Some(entry) = distinct_tool_messages.iter_mut().find(|(m, _)| m == msg) {
                            entry.1 += 1;
                        } else {
                            distinct_tool_messages.push((msg.clone(), 1));
                        }
                    }
                }

                let item_was_selected = res.get("selected").is_some();
                #[cfg(not(target_arch = "wasm32"))]
                {
                    res_was_selected = item_was_selected;
                }

                let mut produced_output = false;

                if let Some(lid) = res.get("layer").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok()) {
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(temp_id) = temp_layer_id.take() {
                        super::layer::drop_layers(s, &[temp_id]);
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    let should_rename = s.map().layer(lid).map(|l| l.name == format!("{final_out_name} (2)")).unwrap_or(false)
                        && s.map().layer_by_name(&final_out_name).is_none();
                    #[cfg(not(target_arch = "wasm32"))]
                    if should_rename && let Some(l) = s.map_mut().layer_mut(lid) {
                        l.name = final_out_name.clone();
                        l.dataset.name = final_out_name.clone();
                    }
                    layer_ids.push(lid);
                    let actual_name = s.map().layer(lid).map(|l| l.name.clone()).unwrap_or_else(|| final_out_name.clone());
                    seen_outputs.insert(actual_name.clone());
                    output_names.push(actual_name);
                    produced_output = true;
                }
                if let Some(upd) = res.get("updated").and_then(Value::as_str) {
                    if let Some(l) = s.map().layer_by_name(upd) {
                        layer_ids.push(l.id);
                        seen_outputs.insert(l.name.clone());
                        output_names.push(l.name.clone());
                    } else {
                        seen_outputs.insert(upd.to_string());
                        output_names.push(upd.to_string());
                    }
                    produced_output = true;
                }
                if let Some(lids) = res.get("layers").and_then(Value::as_array) {
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(temp_id) = temp_layer_id.take() {
                        super::layer::drop_layers(s, &[temp_id]);
                    }
                    for l in lids {
                        if let Some(lid) = l.as_u64().and_then(|id| u32::try_from(id).ok()) {
                            layer_ids.push(lid);
                            if let Some(layer) = s.map().layer(lid) {
                                seen_outputs.insert(layer.name.clone());
                                output_names.push(layer.name.clone());
                                produced_output = true;
                            }
                        }
                    }
                }
                if let Some(table) = res.get("table").and_then(Value::as_str) {
                    if let Some(orig) = res.get("table_renamed_from").and_then(Value::as_str) {
                        collisions += 1;
                        if first_collision.is_none() {
                            first_collision = Some(format!("'{orig}' → '{table}'"));
                        }
                    }
                    seen_outputs.insert(table.to_string());
                    output_names.push(table.to_string());
                    produced_output = true;
                }
                if let Some(tables) = res.get("tables").and_then(Value::as_array) {
                    for t in tables {
                        if let Some(table) = t.as_str() {
                            seen_outputs.insert(table.to_string());
                            output_names.push(table.to_string());
                            produced_output = true;
                        }
                    }
                }
                if let Some(renamed_list) = res.get("tables_renamed").and_then(Value::as_array) {
                    for item in renamed_list {
                        if let (Some(orig), Some(to)) = (item.get("from").and_then(Value::as_str), item.get("to").and_then(Value::as_str)) {
                            collisions += 1;
                            if first_collision.is_none() {
                                first_collision = Some(format!("'{orig}' → '{to}'"));
                            }
                        }
                    }
                }
                if produced_output {
                    if let Some((stem, next_c)) = had_collision {
                        collisions += 1;
                        if first_collision.is_none() {
                            first_collision = Some(format!("'{out_name}' → '{final_out_name}'"));
                        }
                        next_suffix.insert(stem, next_c);
                    }
                } else if item_was_selected {
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(lid) = temp_layer_id {
                        layer_ids.push(lid);
                        seen_outputs.insert(target_layer_name.clone());
                        output_names.push(target_layer_name.clone());
                    }
                } else {
                    succeeded.pop();
                    let err = messages.last().cloned().unwrap_or_else(|| "tool produced no output".into());
                    if let Some(last) = s.project.gp_history.last_mut() {
                        let matches_item = last.id == tool_id
                            && (last.params.get(batch_param).and_then(Value::as_str) == Some(target)
                                || last
                                    .command
                                    .as_ref()
                                    .and_then(|(_, c)| c.get("inputs").and_then(Value::as_array))
                                    .and_then(|a| a.first())
                                    .and_then(Value::as_str)
                                    == Some(target));
                        if matches_item {
                            last.ok = false;
                            if !last.messages.contains(&err) {
                                last.messages.push(err.clone());
                            }
                        }
                    }
                    failed.push(json!({"input": target, "error": err.clone()}));
                    let mut err_obj = json!({"input": target, "error": err});
                    if messages.len() > 1 {
                        err_obj["messages"] = json!(messages);
                    }
                    errors.push(err_obj);
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(temp_id) = temp_layer_id.take() {
                        super::layer::drop_layers(s, &[temp_id]);
                    }
                }
            }
            Err(err) => {
                // On a copy of the session, a tool that runs where the script runs stops the copy
                // there, as in a model: the run here runs it and the items after it.
                #[cfg(not(target_arch = "wasm32"))]
                if err == NOT_AHEAD && matches!(s.tools_ahead, Some(Ahead::Recording(_))) {
                    return Err(err);
                }
                if control.is_cancelled() {
                    cancelled_after = Some(i + 1);
                    interrupted_error = Some(err.clone());
                    errors.push(json!({"input": target, "error": err, "interrupted": true}));
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(temp_id) = temp_layer_id.take() {
                        super::layer::drop_layers(s, &[temp_id]);
                    }
                    s.active_layer = active_before;
                    break;
                }
                failed.push(json!({"input": target, "error": err.clone()}));
                errors.push(json!({"input": target, "error": err}));
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        if !item_reader_warnings.is_empty() {
            let matched = if let Some(record) = s.project.gp_history.last_mut() {
                let matches_item = record.id == tool_id
                    && (record.params.get(batch_param).and_then(Value::as_str) == Some(target)
                        || record
                            .command
                            .as_ref()
                            .and_then(|(_, c)| c.get("inputs").and_then(Value::as_array))
                            .and_then(|a| a.first())
                            .and_then(Value::as_str)
                            == Some(target));
                if matches_item {
                    for msg in &item_reader_warnings {
                        if !record.messages.contains(msg) {
                            record.messages.push(msg.clone());
                        }
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if !matched {
                let spec = rustgis_analysis::tool(tool_id);
                let name = spec.map(|t| t.name).unwrap_or("Tool").to_string();
                let mut msgs = item_reader_warnings.clone();
                if let Some(last_err) = errors.last().and_then(|e| e.get("error")).and_then(Value::as_str)
                    && !msgs.iter().any(|m| m == last_err)
                {
                    msgs.insert(0, last_err.to_string());
                }
                s.push_gp_record(GpRecord {
                    tool: name,
                    id: tool_id.to_string(),
                    params: record_params.clone(),
                    command: cmd_record.clone(),
                    at: now_ms() / 1000,
                    ok: false,
                    messages: msgs,
                    millis: 0,
                    outputs: Vec::new(),
                });
                if s.project.gp_history.len() > 500 {
                    s.project.gp_history.remove(0);
                }
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(temp_id) = temp_layer_id {
            let was_updated = layer_ids.contains(&temp_id);
            if !was_updated && !res_was_selected {
                super::layer::drop_layers(s, &[temp_id]);
            }
        }
        s.active_layer = active_before;
    }

    #[cfg(not(target_arch = "wasm32"))]
    if trimmed_paths > 0 {
        s.notify(format!("gp.batch: {trimmed_paths} file path(s) trimmed of surrounding whitespace"), false);
    }

    // Every distinct message is named (a loss reported by the fourth kind must not be hidden),
    // up to a bound a hostile run can't flood; all of them are in the result's `messages`.
    const NAMED: usize = 50;
    if items_reporting_messages > 0 {
        let sample = distinct_tool_messages
            .iter()
            .take(NAMED)
            .map(|(m, c)| if *c > 1 { format!("{m} (×{c})") } else { m.clone() })
            .collect::<Vec<_>>()
            .join("; ");
        let more = if distinct_tool_messages.len() > NAMED {
            format!(" and {} more, in the result", distinct_tool_messages.len() - NAMED)
        } else {
            String::new()
        };
        s.notify(format!("gp.batch: {items_reporting_messages} item(s) reported messages ({sample}{more})"), false);
    }

    if explicit_pattern.is_some() && !tool_has_output && !succeeded.is_empty() {
        let count = succeeded.len();
        s.notify(
            format!(
                "gp.batch: 'outputPattern' does not apply to '{tool_id}' (it names its outputs itself); {count} item(s) used their own names"
            ),
            false,
        );
    }

    if collisions > 0 {
        let first = first_collision.unwrap_or_default();
        s.notify(format!("gp.batch: {collisions} output name(s) collided and were renamed (first: {first})"), false);
    }

    #[cfg(not(target_arch = "wasm32"))]
    if !reader_warnings.is_empty() {
        let mut aggregated: Vec<(&str, Vec<&str>)> = Vec::new();
        for (target, msg) in &reader_warnings {
            if let Some((_, inputs)) = aggregated.iter_mut().find(|(m, _)| *m == msg.as_str()) {
                if !inputs.contains(&target.as_str()) {
                    inputs.push(target.as_str());
                }
            } else {
                aggregated.push((msg.as_str(), vec![target.as_str()]));
            }
        }
        for (msg, inputs) in aggregated {
            let count = inputs.len();
            let inputs_list = inputs.join(", ");
            let label = if count == 1 { "input" } else { "inputs" };
            s.notify(format!("gp.batch: {count} {label} ({inputs_list}): {msg}"), false);
        }
    }

    if !failed.is_empty() {
        let sample = failed.iter().take(3).filter_map(|v| v.get("input").and_then(Value::as_str)).collect::<Vec<_>>().join(", ");
        let more = if failed.len() > 3 { format!(" and {} more", failed.len() - 3) } else { String::new() };
        s.notify(format!("Batch '{tool_id}': {} failed ({sample}{more})", failed.len()), true);
    }

    let is_cancelled = cancelled_after.is_some();
    let num_ok = succeeded.len();
    let num_err = failed.len();
    let attempted = num_ok + num_err + if interrupted_error.is_some() { 1 } else { 0 };

    let summary_msg = if is_cancelled {
        if let Some(err) = &interrupted_error {
            format!("Batch '{tool_id}' cancelled after {attempted} of {} ({err})", targets.len())
        } else {
            format!("Batch '{tool_id}' cancelled after {attempted} of {}", targets.len())
        }
    } else if num_err == 0 {
        format!("Batch '{tool_id}' complete: all {num_ok} succeeded")
    } else {
        format!("Batch '{tool_id}' complete: {num_ok} succeeded, {num_err} failed")
    };
    s.notify(summary_msg, num_err > 0 || is_cancelled);

    let warnings: Vec<Value> = reader_warnings.iter().map(|(input, msg)| json!({"input": input, "warning": msg})).collect();

    let mut ret = json!({
        "tool": tool_id,
        "total": targets.len(),
        "succeeded": num_ok,
        "failed": num_err,
        "layers": layer_ids,
        "outputs": output_names,
        "errors": errors,
        "warnings": warnings,
        "messages": distinct_tool_messages.iter().map(|(m, c)| json!({"message": m, "items": c})).collect::<Vec<_>>(),
    });
    if is_cancelled {
        ret["cancelled"] = json!(true);
        ret["attempted"] = json!(attempted);
    }

    if !is_cancelled && num_ok == 0 {
        let details = errors
            .iter()
            .map(|e| {
                let input = e.get("input").and_then(Value::as_str).unwrap_or("?");
                let err = e.get("error").and_then(Value::as_str).unwrap_or("unknown error");
                format!("{input}: {err}")
            })
            .collect::<Vec<_>>()
            .join("; ");
        let err_msg = if num_err == 1 {
            format!("Batch '{tool_id}' failed: {details}")
        } else {
            format!("Batch '{tool_id}' failed: all {num_err} inputs failed: {details}")
        };
        return Err(err_msg);
    }

    Ok(ret)
}

/// Resolves the same tool for execution and permission checks, keeping built-in name precedence.
/// History records may name a tool by its display name rather than its id.
pub fn resolve_tool(name: &str) -> Option<&'static rustgis_analysis::ToolSpec> {
    rustgis_analysis::resolve_published(name).map(|r| r.spec())
}

/// The id of a tool named by its id or its display name (history records keep the name).
pub fn tool_id(name: &str) -> Option<&'static str> {
    resolve_tool(name).map(|t| t.id)
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
    let op = s.point_tool(&params)?;
    let c = s.tool_continuation(&op)?;
    s.check_continuation(&c)?;
    let tool = op.op();
    let res = tool.pin.run(&PointInput(ds), &tool.params, &rustgis_analysis::control::Control::new(), &|| c.check());
    s.check_continuation(&c)?;
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
    let op = s.authorize_tool(tool, params)?;
    s.invoke_tool(op, control)
}

/// Runs only the immutable tool/schema/load admitted by policy.
pub(crate) fn execute(
    s: &mut Session,
    op: crate::policy::Authorized<crate::policy::ToolOp>,
    control: &std::sync::Arc<rustgis_analysis::control::Control>,
    opts: &RunOpts,
) -> Result<(Value, Vec<String>), String> {
    let crate::policy::ToolOp { pin, params } = op.op();
    pin.current()?;
    let spec = pin.spec();
    let run_of = |t0| Ran { name: spec.name.into(), id: spec.id.into(), command: opts.command.clone(), started: t0 };
    let kept = || recorded(Some(spec), opts.record.clone().unwrap_or_else(|| params.clone()));
    #[cfg(not(target_arch = "wasm32"))]
    let recording = matches!(s.tools_ahead, Some(Ahead::Recording(_)));
    #[cfg(not(target_arch = "wasm32"))]
    let current = rustgis_analysis::control::current().filter(|_| recording);
    #[cfg(not(target_arch = "wasm32"))]
    let control = current.as_ref().unwrap_or(control);
    let t0 = now_ms();
    if control.is_cancelled() {
        let e = format!("{} was cancelled before it started", spec.name);
        return apply_with(s, run_of(t0), kept(), Err(e), 0, opts);
    }
    let continuation = s.tool_continuation(&op)?;
    #[cfg(not(target_arch = "wasm32"))]
    let ran = take_ahead(s, pin, params);
    #[cfg(target_arch = "wasm32")]
    let ran = None;
    let (mut res, ids, millis) = match ran {
        Some(ran) => ran,
        None => {
            #[cfg(not(target_arch = "wasm32"))]
            if recording && (pin.plugin().is_some() || writes_files(spec)) {
                return Err(NOT_AHEAD.into());
            }
            #[cfg(not(target_arch = "wasm32"))]
            let read = recording.then(|| Snapshot::owned(s));
            let inputs = match map_inputs(s, Some(spec), params) {
                Ok(inputs) => inputs,
                // A batch item that can't read its input is kept in the history as a failed run.
                Err(e) if opts.record.is_some() => return apply_with(s, run_of(t0), kept(), Err(e), now_ms().saturating_sub(t0), opts),
                Err(e) => return Err(e),
            };
            let mut res = if let Some(plugin) = pin.plugin() {
                let (_, tool) = rustgis_plugins::split_id(spec.id).ok_or("invalid plugin tool id")?;
                rustgis_analysis::run_under(spec.name, control, || {
                    rustgis_plugins::tools::prepare_pinned(plugin.plugin(), tool, &inputs, params)?
                        .run(&rustgis_plugins::tools::Control::default())
                })
            } else {
                pin.run(&inputs, params, control, &|| continuation.check())
            };
            if let Ok(result) = &mut res {
                result.messages.extend(inputs.messages.iter().cloned());
            }
            let ids = full_ids(&inputs.loaded);
            #[cfg(not(target_arch = "wasm32"))]
            let view = inputs.view.get().copied();
            drop(inputs);
            // A file made, with a SaveFile parameter or not, is written where the model or script runs.
            #[cfg(not(target_arch = "wasm32"))]
            if recording && matches!(&res, Ok(r) if matches!(r.output, Output::File { .. })) {
                return Err(NOT_AHEAD.into());
            }
            let millis = now_ms().saturating_sub(t0);
            #[cfg(not(target_arch = "wasm32"))]
            if let (Some(read), Some(Ahead::Recording(ran))) = (read, &mut s.tools_ahead) {
                ran.push(RanAhead {
                    pin: pin.clone(),
                    params: params.clone(),
                    read,
                    view,
                    result: res.clone(),
                    full_ids: ids.clone(),
                    millis,
                });
            }
            (res, ids, millis)
        }
    };
    pin.current()?;
    continuation.check()?;
    fit_to_windows(s, &mut res, &ids);
    apply_with(s, run_of(t0), kept(), res, millis, opts)
}

/// How [`execute`] applies a run: by default as `gp.run` does. A batch item keeps the
/// parameters and command it is replayed with, leaves its messages to the batch's one notice
/// per kind, and adds a table beside one of the same name instead of replacing it.
#[derive(Default)]
pub(crate) struct RunOpts {
    pub(crate) record: Option<Value>,
    pub(crate) command: Option<(String, Value)>,
    pub(crate) quiet: bool,
    pub(crate) keep_tables: bool,
}

/// Runs one batch item through the same policy admission as [`run_tool_with`], returning the
/// tool's messages for the batch to report.
pub(crate) fn run_tool_quiet(
    s: &mut Session,
    tool: &str,
    params: &Value,
    record_params: Option<&Value>,
    command: Option<(String, Value)>,
    control: &std::sync::Arc<rustgis_analysis::control::Control>,
) -> Result<(Value, Vec<String>), String> {
    let op = s.authorize_tool(tool, params)?;
    let opts = RunOpts { record: record_params.cloned(), command, quiet: true, keep_tables: true };
    s.invoke_tool_with(op, control, &opts)
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
    pub(crate) continuation: crate::policy::Continuation,
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
    s.file_outputs.push(PendingFile { id, file_name: file_name.clone(), bytes: Some(bytes), record, continuation: s.continuation()? });
    s.request_only = true;
    Ok(json!({"pending": true, "download": id, "file": file_name}))
}

/// Records a completed file delivery through the generated-message history seam.
fn file_delivered(s: &mut Session, p: &Value) -> CmdResult {
    let id = p.get("id").and_then(Value::as_u64).ok_or("id is required")?;
    let i = s.file_outputs.iter().position(|f| f.id == id).ok_or("no pending file with that id")?;
    s.gp_file_owner(&s.file_outputs[i].continuation)?;
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
    s.push_gp_record(file.record);
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

/// Adds a tool's table to the project under its name: in place of a table of that name, or, for
/// a batch item, beside it with a numbered name. Returns the name and the one it was renamed from.
fn put_table(s: &mut Session, mut t: rustgis_data::Dataset, opts: &RunOpts) -> (String, Option<String>) {
    if !opts.keep_tables {
        let n = t.name.clone();
        s.project.put_table(t);
        return (n, None);
    }
    let taken = |s: &Session, name: &str| s.project.tables.iter().any(|existing| existing.name == name);
    let mut renamed_from = None;
    if taken(s, &t.name) {
        let stem = t.name.clone();
        let mut counter = 2u32;
        while taken(s, &format!("{stem}_{counter}")) {
            counter = counter.saturating_add(1);
        }
        renamed_from = Some(std::mem::replace(&mut t.name, format!("{stem}_{counter}")));
    }
    let n = t.name.clone();
    s.project.add_table(t);
    (n, renamed_from)
}

/// Adds a tool's result to the map and the history.
fn apply(s: &mut Session, ran: Ran, params: Value, res: Result<rustgis_analysis::ToolResult, String>, millis: u64) -> CmdResult {
    apply_with(s, ran, params, res, millis, &RunOpts::default()).map(|(v, _)| v)
}

/// [`apply`] as `opts` says, returning the run's messages too.
fn apply_with(
    s: &mut Session,
    ran: Ran,
    params: Value,
    res: Result<rustgis_analysis::ToolResult, String>,
    millis: u64,
    opts: &RunOpts,
) -> Result<(Value, Vec<String>), String> {
    let continuation = s.continuation()?;
    s.check_continuation(&continuation)?;
    let Ran { name, id, command, started } = ran;
    let at = started / 1000;
    let mut result = match res {
        Err(e) => {
            s.push_gp_record(GpRecord {
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
                let msgs = result.messages.clone();
                return queue_file(
                    s,
                    file_name,
                    bytes,
                    GpRecord { tool: name, id, params, command, at, ok: false, messages: result.messages, millis, outputs: Vec::new() },
                )
                .map(|v| (v, msgs));
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let path = params.get("output").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()).unwrap_or(&file_name);
                if let Err(error) = write_file_output(s, std::path::Path::new(path), &bytes) {
                    let error = format!("{path}: {error}");
                    result.messages.push(error.clone());
                    s.push_gp_record(GpRecord {
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
                let msgs = result.messages.clone();
                return queue_file(
                    s,
                    file_name,
                    bytes,
                    GpRecord { tool: name, id, params, command, at, ok: false, messages: result.messages, millis, outputs: Vec::new() },
                )
                .map(|v| (v, msgs));
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
            let (n, renamed_from) = put_table(s, t, opts);
            if let Some(orig) = renamed_from { json!({"table": n, "table_renamed_from": orig}) } else { json!({"table": n}) }
        }
        Output::Layers(outputs) => {
            let mut layers = Vec::new();
            let mut tables = Vec::new();
            let mut tables_renamed = Vec::new();
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
                        let (n, renamed_from) = put_table(s, t, opts);
                        tables.push(n.clone());
                        if let Some(orig) = renamed_from {
                            tables_renamed.push(json!({"from": orig, "to": n}));
                        }
                    }
                }
            }
            let mut ret = json!({"layers": layers, "tables": tables});
            if !tables_renamed.is_empty() {
                ret["tables_renamed"] = json!(tables_renamed);
            }
            ret
        }
    };
    let messages = result.messages.clone();
    if !opts.quiet {
        for m in &messages {
            s.notify(m.clone(), false);
        }
    }
    let ids: Vec<u32> = match (out.get("layer"), out.get("layers")) {
        (Some(id), _) => id.as_u64().into_iter().filter_map(|v| u32::try_from(v).ok()).collect(),
        (None, Some(Value::Array(v))) => v.iter().filter_map(Value::as_u64).filter_map(|v| u32::try_from(v).ok()).collect(),
        _ => Vec::new(),
    };
    let outputs = ids.iter().filter_map(|id| s.map().layer(*id).map(|l| l.name.clone())).collect();
    s.push_gp_record(GpRecord { tool: name, id, params, command, at, ok: true, messages: result.messages, millis, outputs });
    if s.project.gp_history.len() > 500 {
        s.project.gp_history.remove(0);
    }
    Ok((out, messages))
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

    /// Unit bytes still enter through Session::run so their late acknowledgment has an owner.
    fn queue_file(s: &mut Session, name: String, bytes: Vec<u8>, record: GpRecord) -> CmdResult {
        if s.registry.get("test.queueFile").is_none() {
            s.registry.commands.push(C::new("test.queueFile", "File export", "Test", |s, p| {
                let record = serde_json::from_value(p["record"].clone()).unwrap();
                let bytes = serde_json::from_value(p["bytes"].clone()).unwrap();
                super::queue_file(s, p["name"].as_str().unwrap().into(), bytes, record)
            }));
        }
        s.run("test.queueFile", &json!({"name":name,"bytes":bytes,"record":record}))
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

    /// Browser file bytes are an owned snapshot; native delivery does not target the displayed map.
    #[test]
    fn native_file_delivery_survives_a_map_switch() {
        for take_before_switch in [false, true] {
            let mut s = task_session(json!([]));
            let pending = s.run("gp.run", &json!({"tool":"vectorToPmtiles", "params":{"input":"Cities","minZoom":0,"maxZoom":0}})).unwrap();
            let id = pending["download"].as_u64().unwrap();
            let early = take_before_switch.then(|| s.take_gp_files());
            s.run("map.new", &json!({"name":"Other"})).unwrap();
            let displayed = s.map().instance_id();
            let files = early.unwrap_or_else(|| s.take_gp_files());
            assert_eq!(files.len(), 1, "the native snapshot survives either delivery boundary");
            assert_eq!(files[0].0, id);
            assert!(files[0].2.starts_with(b"PMTiles\x03"), "the real exporter produced the queued file");
            assert!(s.project.gp_history.is_empty(), "bytes alone do not record success");
            assert_eq!(s.acknowledge_gp_file(id, None).unwrap()["delivered"], true);
            assert_eq!(s.map().instance_id(), displayed, "delivery does not activate its origin map");
            assert_eq!(s.project.gp_history.len(), 1);
            assert!(s.project.gp_history[0].ok);
            assert!(s.take_gp_files().is_empty(), "bytes are handed over once");
            assert!(s.acknowledge_gp_file(id, None).is_err(), "delivery is acknowledged once");
        }
    }

    /// Browser-style bytes and final delivery remain one-use and owned by their initiating agent.
    #[test]
    fn permission_policy_file_delivery_owner_and_replay() {
        struct Live(std::sync::atomic::AtomicBool);
        impl crate::policy::Revocation for Live {
            fn allows(&self, _: crate::policy::Effects) -> bool {
                self.0.load(std::sync::atomic::Ordering::SeqCst)
            }
        }
        let mut s = task_session(json!([]));
        let live = std::sync::Arc::new(Live(std::sync::atomic::AtomicBool::new(true)));
        let owner =
            crate::policy::Scope::agent(crate::policy::AgentAccess::Allow, false, "file-owner", Some("file-owner".into()), live.clone());
        let foreign = crate::policy::Scope::agent(
            crate::policy::AgentAccess::Allow,
            false,
            "file-foreign",
            None,
            std::sync::Arc::new(Live(std::sync::atomic::AtomicBool::new(true))),
        );
        let call = crate::policy::Call::tool("vectorToPmtiles", json!({"input":"Cities","minZoom":0,"maxZoom":0}));
        let pending = s.run_scoped(owner.clone(), &call).unwrap();
        let id = pending["download"].as_u64().unwrap();
        assert!(s.run_scoped(foreign.clone(), &crate::policy::Call::command("gp.fileDelivered", json!({"id":id}))).is_err());
        let mark = s.notice_mark();
        assert!(s.with_scope(foreign, |s| s.take_gp_files()).unwrap().is_empty());
        assert!(s.notices_since(mark).is_empty(), "a foreign drain leaves the owner's bytes and notices alone");
        assert_eq!(s.take_gp_files().len(), 1);
        assert!(s.take_gp_files().is_empty());
        assert_eq!(s.acknowledge_gp_file(id, None).unwrap()["delivered"], true);
        assert_eq!(s.undo_actor(), Some(Some("file-owner")));
        assert!(s.acknowledge_gp_file(id, None).is_err());
        s.run_scoped(owner.clone(), &crate::policy::Call::command("edit.undo", Value::Null)).unwrap();
        // Several exports can be revoked together before the UI takes their bytes.
        let mut retired = Vec::new();
        for name in ["first.pmtiles", "second.pmtiles"] {
            let call = crate::policy::Call::tool("vectorToPmtiles", json!({"input":"Cities","minZoom":0,"maxZoom":0,"output":name}));
            let pending = s.run_scoped(owner.clone(), &call).unwrap();
            retired.push(pending["download"].as_u64().unwrap());
        }
        live.0.store(false, std::sync::atomic::Ordering::SeqCst);
        let mark = s.notice_mark();
        assert!(s.take_gp_files().is_empty());
        let errors: Vec<_> = s.notices_since(mark).iter().filter(|notice| notice.error).map(|notice| notice.text.as_str()).collect();
        assert_eq!(
            errors,
            [
                "2 pending file export(s) could not be completed (\"first.pmtiles\", \"second.pmtiles\"): this request's authority was revoked"
            ]
        );
        assert!(s.project.gp_history.is_empty(), "revoked queued files cannot record success");
        assert!(s.take_gp_files().is_empty());
        for id in retired {
            assert!(s.acknowledge_gp_file(id, None).is_err(), "a retired file cannot replay its acknowledgment");
        }
        assert_eq!(s.notices_since(mark).len(), 1, "retired file exports report their count once");
        live.0.store(true, std::sync::atomic::Ordering::SeqCst);
        // A host callback may revoke authority after taking the bytes but before acknowledgment.
        let pending = s.run_scoped(owner.clone(), &call).unwrap();
        let id = pending["download"].as_u64().unwrap();
        assert_eq!(s.take_gp_files().len(), 1);
        live.0.store(false, std::sync::atomic::Ordering::SeqCst);
        let notice_mark = s.notice_mark();
        assert!(s.acknowledge_gp_file(id, None).unwrap_err().contains("revoked"));
        assert!(s.notices_since(notice_mark).is_empty(), "the UI must report failures before command dispatch");
        assert!(s.project.gp_history.is_empty(), "revoked delivery cannot record success");
        live.0.store(true, std::sync::atomic::Ordering::SeqCst);
        let pending = s.run_scoped(owner, &call).unwrap();
        s.run("map.new", &json!({"name":"Other"})).unwrap();
        assert!(s.take_gp_files().is_empty(), "restricted delivery still requires its original active map");
        assert!(s.acknowledge_gp_file(pending["download"].as_u64().unwrap(), None).is_err());
        assert!(s.project.gp_history.is_empty(), "refused restricted delivery cannot record success");
    }

    #[test]
    fn queueing_a_file_is_not_an_edit_before_delivery() {
        let mut s = Session::new(crate::Project::default());
        s.mark_saved();
        let depth = s.undo_depth();
        let pending = queue_file(&mut s, "cities.pmtiles".into(), vec![1, 2, 3], record()).unwrap();
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

    /// Draining an old refused file cannot let a new export escape a failed handler's rollback.
    #[test]
    fn failed_commands_cannot_replace_retired_pending_files() {
        let mut s = task_session(json!([]));
        let login = s
            .run("connection.new", &json!({"kind":"wms","address":"https://file-rollback.invalid/wms","auth":"header","keyName":"X-Key"}))
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        s.run("connection.login", &json!({"id":login,"apiKey":"RGSYNTH_file_rollback"})).unwrap();
        let old =
            s.run("gp.run", &json!({"tool":"vectorToPmtiles","params":{"input":"Cities","minZoom":0,"maxZoom":0}})).unwrap()["download"]
                .as_u64()
                .unwrap();
        s.run("connection.logout", &json!({"id":login})).unwrap();
        s.registry.commands.push(C::new("test.replaceFileThenFail", "Failed replacement export", "Test", |s, _| {
            s.take_gp_files();
            s.run("gp.run", &json!({"tool":"vectorToPmtiles","params":{"input":"Cities","minZoom":0,"maxZoom":0}}))?;
            Err("fail after replacing a refused file".into())
        }));
        assert!(s.run("test.replaceFileThenFail", &Value::Null).is_err());
        assert!(s.take_gp_files().is_empty(), "the failed handler's replacement file cannot reach the UI");
        assert!(s.file_outputs.is_empty());
        assert!(s.acknowledge_gp_file(old, None).is_err(), "the old refused file must not be restored either");
        assert!(s.project.gp_history.is_empty());
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
        s.registry.commands.push(C::new("test.applyFile", "File export", "Test", |s, p| {
            apply(
                s,
                Ran { name: "File export".into(), id: "vectorToPmtiles".into(), command: None, started: 1000 },
                p.clone(),
                Ok(rustgis_analysis::ToolResult {
                    output: Output::File { file_name: "cities.pmtiles".into(), bytes: b"archive".to_vec() },
                    messages: vec!["Exported data".into()],
                }),
                3,
            )
        }));
        let params = json!({"output": destination.display().to_string()});
        let out = s.run("test.applyFile", &params).unwrap();
        assert_eq!(out["bytes"], 7);
        assert_eq!(std::fs::read(&destination).unwrap(), b"archive");
        assert!(s.map().layers.is_empty());
        assert!(s.project.gp_history[0].ok);
        assert!(s.project.gp_history[0].outputs.is_empty());
        let occupied = root.join("occupied");
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("keep"), b"unchanged").unwrap();
        let params = json!({"output": occupied.display().to_string()});
        assert!(s.guarded("File export", true, |s| s.run("test.applyFile", &params)).is_err());
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
            s.guarded("Failed export task", true, |s| { s.guarded("File export", true, |s| s.run("test.applyFile", &params)) }).is_err()
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
        let tool = rustgis_analysis::resolve_published("vectorToPmtiles").expect("registered file export tool").spec();
        assert!(writes_files(tool));
        assert!(!reads_files(tool), "output files are not input paths");
        for output in [None, Some("archive"), Some("cities.pmtiles")] {
            let mut params = json!({"input": "Cities", "minZoom": 0, "maxZoom": 0});
            if let Some(output) = output {
                params["output"] = json!(output);
            }
            let p = json!({"tool": "vectorToPmtiles", "params": params});
            assert!(crate::cmd::items::touches_files(&s, "gp.run", &p));
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

    #[test]
    fn batch_reaches_files_and_is_reserved_to_the_user() {
        let s = Session::new(crate::Project::default());
        let spec = s.registry.get("gp.batch").copied().expect("gp.batch is registered");
        // Every batch, a wildcard over the map's layers included: its items aren't reviewed one by one.
        for p in [
            json!({"tool": "convexHull", "folder": "data_dir"}),
            json!({"tool": "convexHull", "inputs": ["cities.gpkg"]}),
            json!({"tool": "convexHull", "inputs": ["Cities", "Parks"]}),
            json!({"tool": "convexHull", "inputs": "*"}),
        ] {
            assert!(crate::cmd::items::touches_files(&s, "gp.batch", &p), "{p}");
            assert!(matches!(crate::cmd::plugins::reaches(&spec, &p), Some(crate::cmd::plugins::Reach::Files)), "{p}");
        }
        assert!(crate::cmd::plugins::user_only("gp.batch"));
        // A plain gp.run of a built-in tool on project layers still stays in the project.
        assert!(!crate::cmd::items::touches_files(&s, "gp.run", &json!({"tool": "buffer", "params": {"input": "Cities", "distance": 1}})));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod batch_tests {
    use super::*;

    fn test_session() -> Session {
        let mut s = Session::new(crate::Project::default());
        let mut cities = Dataset::new("Cities", rustgis_data::GeometryType::Point, Vec::new());
        cities.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 20.0))), Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(0, cities));

        let mut parks = Dataset::new("Parks", rustgis_data::GeometryType::Point, Vec::new());
        parks.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(15.0, 25.0))), Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(1, parks));
        s
    }

    #[test]
    fn batch_run_over_multiple_layers() {
        let mut s = test_session();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities", "Parks"],
                    "output_pattern": "{input}_buf",
                    "params": {"distance": 50, "units": "Kilometers"}
                }),
            )
            .unwrap();

        assert_eq!(res["total"], 2);
        assert_eq!(res["succeeded"], 2);
        assert_eq!(res["failed"], 0);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0], "Cities_buf");
        assert_eq!(outputs[1], "Parks_buf");

        assert!(s.map().layer_by_name("Cities_buf").is_some());
        assert!(s.map().layer_by_name("Parks_buf").is_some());
    }

    #[test]
    fn batch_run_wildcard_all_layers() {
        let mut s = test_session();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "convexHull",
                    "inputs": "*",
                    "output_pattern": "{input}_hull"
                }),
            )
            .unwrap();

        assert_eq!(res["total"], 2);
        assert_eq!(res["succeeded"], 2);
        assert_eq!(res["failed"], 0);
        assert!(s.map().layer_by_name("Cities_hull").is_some());
        assert!(s.map().layer_by_name("Parks_hull").is_some());
    }

    #[test]
    fn batch_run_fault_tolerance() {
        let mut s = test_session();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities", "MissingLayer", "Parks"],
                    "params": {"distance": 20}
                }),
            )
            .unwrap();

        assert_eq!(res["total"], 3);
        assert_eq!(res["succeeded"], 2);
        assert_eq!(res["failed"], 1);
        let errs = res["errors"].as_array().unwrap();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0]["input"], "MissingLayer");

        // The valid layers succeeded and were added to map
        assert!(s.map().layer_by_name("Cities_buffer").is_some());
        assert!(s.map().layer_by_name("Parks_buffer").is_some());
    }

    #[test]
    fn batch_run_validation_errors() {
        let mut s = test_session();
        // Missing tool
        assert!(s.run("gp.batch", &json!({"inputs": ["Cities"]})).is_err());
        // Nonexistent tool
        assert!(s.run("gp.batch", &json!({"tool": "nonexistent", "inputs": ["Cities"]})).is_err());
        // Missing inputs and folder
        assert!(s.run("gp.batch", &json!({"tool": "buffer"})).is_err());
        // Nonexistent folder
        assert!(s.run("gp.batch", &json!({"tool": "buffer", "folder": "/path/does/not/exist_12345"})).is_err());
    }

    #[test]
    fn batch_run_folder_scanning() {
        let root = std::env::temp_dir()
            .join(format!("rustgis_batch_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("layer_a.geojson");
        let f2 = root.join("layer_b.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();
        std::fs::write(&f2, sample_geojson).unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "convexHull",
                    "folder": root.display().to_string(),
                    "output_pattern": "{input}_hull"
                }),
            )
            .unwrap();

        assert_eq!(res["total"], 2);
        assert_eq!(res["succeeded"], 2);
        assert_eq!(res["failed"], 0);
        assert!(s.map().layer_by_name("layer_a_hull").is_some());
        assert!(s.map().layer_by_name("layer_b_hull").is_some());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_dotted_layer_name() {
        let mut s = Session::new(crate::Project::default());
        let mut roads = Dataset::new("Roads.2020", rustgis_data::GeometryType::Point, Vec::new());
        roads.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(1.0, 2.0))), Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(0, roads));

        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Roads.2020"],
                    "output_pattern": "{input}_buf",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs[0], "Roads.2020_buf");
        assert!(s.map().layer_by_name("Roads.2020_buf").is_some());
    }

    #[test]
    fn batch_run_output_collision_deduplication() {
        let mut s = test_session();
        let pre = Dataset::new("Cities_buffer", rustgis_data::GeometryType::Point, Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(10, pre));

        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs[0], "Cities_buffer_2");
        assert!(s.map().layer_by_name("Cities_buffer_2").is_some());
    }

    #[test]
    fn batch_run_output_pattern_must_include_input() {
        let mut s = test_session();
        let res = s.run(
            "gp.batch",
            &json!({
                "tool": "buffer",
                "inputs": ["Cities", "Parks"],
                "outputPattern": "constant_output_name"
            }),
        );
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("must include '{input}'"));
    }

    #[test]
    fn batch_run_folder_skips_unsupported_files() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_skip_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("layer.geojson");
        let txt = root.join("readme.txt");
        let doc = root.join("notes.pdf");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();
        std::fs::write(&txt, "text file").unwrap();
        std::fs::write(&doc, "pdf file").unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "convexHull",
                    "folder": root.display().to_string(),
                    "output_pattern": "{input}_hull"
                }),
            )
            .unwrap();

        assert_eq!(res["total"], 1);
        assert_eq!(res["succeeded"], 1);
        assert!(s.map().layer_by_name("layer_hull").is_some());

        assert!(s.notices.iter().any(|n| n.text.contains("2 unsupported files skipped")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_folder_includes_gpx_kml_csv() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_formats_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let csv = root.join("points.csv");
        let gpx = root.join("track.gpx");
        let kml = root.join("places.kml");
        let txt = root.join("info.txt");

        std::fs::write(&csv, "id,val\n1,10\n2,20\n").unwrap();
        std::fs::write(&gpx, r#"<?xml version="1.0"?><gpx version="1.1"><wpt lat="10.0" lon="20.0"><name>W1</name></wpt></gpx>"#).unwrap();
        std::fs::write(
            &kml,
            r#"<?xml version="1.0"?><kml xmlns="http://www.opengis.net/kml/2.2"><Placemark><Point><coordinates>20.0,10.0</coordinates></Point></Placemark></kml>"#,
        )
        .unwrap();
        std::fs::write(&txt, "skip me").unwrap();

        let mut s = Session::new(crate::Project::default());
        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "convexHull",
                    "folder": root.display().to_string(),
                    "output_pattern": "{input}_out"
                }),
            )
            .unwrap();

        assert_eq!(res["total"], 3);
        let notices = &s.notices[mark..];
        assert!(notices.iter().any(|n| n.text.contains("1 unsupported files skipped")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_folder_skips_symlinks() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_symlink_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("valid.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();

        let target_file = root.join("target.geojson");
        std::fs::write(&target_file, sample_geojson).unwrap();
        let symlink_file = root.join("link.geojson");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&target_file, &symlink_file).unwrap();
        #[cfg(windows)]
        let _ = std::os::windows::fs::symlink_file(&target_file, &symlink_file);

        let linked = symlink_file.exists();
        if linked {
            let mut s = Session::new(crate::Project::default());
            let res = s
                .run(
                    "gp.batch",
                    &json!({
                        "tool": "convexHull",
                        "folder": root.display().to_string(),
                        "output_pattern": "{input}_hull"
                    }),
                )
                .unwrap();

            assert_eq!(res["total"], 2);
            assert_eq!(res["succeeded"], 2);
            assert!(s.notices.iter().any(|n| n.text.contains("1 symlink(s) skipped")));
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_folder_skips_subfolders() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_subfolder_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("layer.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();
        let sub = root.join("subfolder");
        std::fs::create_dir_all(&sub).unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "convexHull",
                    "folder": root.display().to_string(),
                    "output_pattern": "{input}_hull"
                }),
            )
            .unwrap();

        assert_eq!(res["total"], 1);
        assert_eq!(res["succeeded"], 1);
        assert!(s.notices.iter().any(|n| n.text.contains("1 subfolder(s) skipped")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn batch_run_folder_skips_non_utf8_filename() {
        use std::os::unix::ffi::OsStrExt;
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_non_utf8_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("valid.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();

        let bad_name = std::ffi::OsStr::from_bytes(b"bad_\xff.geojson");
        let bad_file = root.join(bad_name);
        let _ = std::fs::write(&bad_file, sample_geojson);

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "convexHull",
                    "folder": root.display().to_string(),
                    "output_pattern": "{input}_hull"
                }),
            )
            .unwrap();

        assert_eq!(res["total"], 1);
        assert_eq!(res["succeeded"], 1);
        assert!(s.notices.iter().any(|n| n.text.contains("1 file name(s) that are not valid UTF-8 skipped")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_table_output_reported() {
        let mut s = Session::new(crate::Project::default());
        let mut ds = Dataset::new(
            "Cities",
            rustgis_data::GeometryType::Point,
            vec![rustgis_data::Field::new("pop", rustgis_data::FieldType::Integer)],
        );
        ds.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 20.0))), vec![rustgis_data::Value::Int(100)]);
        s.map_mut().add_layer(rustgis_map::Layer::new(0, ds));

        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "summaryStatistics",
                    "inputs": ["Cities"],
                    "params": {"field": "pop"}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0], "Cities_Statistics");
        assert!(crate::project::find_table(&s.project.tables, "Cities_Statistics").is_some());
    }

    #[test]
    fn batch_run_table_collision_with_existing_project_table() {
        let mut s = Session::new(crate::Project::default());
        let mut ds = Dataset::new(
            "Cities",
            rustgis_data::GeometryType::Point,
            vec![rustgis_data::Field::new("pop", rustgis_data::FieldType::Integer)],
        );
        ds.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 20.0))), vec![rustgis_data::Value::Int(100)]);
        s.map_mut().add_layer(rustgis_map::Layer::new(0, ds));

        let existing = Dataset::new(
            "Cities_Statistics",
            rustgis_data::GeometryType::None,
            vec![rustgis_data::Field::new("old", rustgis_data::FieldType::Integer)],
        );
        s.project.put_table(existing);

        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "summaryStatistics",
                    "inputs": ["Cities"],
                    "params": {"field": "pop"}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0], "Cities_Statistics_2");
        assert!(crate::project::find_table(&s.project.tables, "Cities_Statistics").is_some());
        assert!(crate::project::find_table(&s.project.tables, "Cities_Statistics_2").is_some());
        let ids: std::collections::HashSet<u32> = s.project.tables.iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), s.project.tables.len(), "all table ids must be distinct");
        assert!(s.notices.iter().any(|n| n.text.contains("output name(s) collided and were renamed")));
    }

    #[test]
    fn batch_run_table_collision_duplicate_inputs() {
        let mut s = Session::new(crate::Project::default());
        let mut ds = Dataset::new(
            "Cities",
            rustgis_data::GeometryType::Point,
            vec![rustgis_data::Field::new("pop", rustgis_data::FieldType::Integer)],
        );
        ds.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 20.0))), vec![rustgis_data::Value::Int(100)]);
        s.map_mut().add_layer(rustgis_map::Layer::new(0, ds));

        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "summaryStatistics",
                    "inputs": ["Cities", "Cities"],
                    "params": {"field": "pop"}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 2);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0], "Cities_Statistics");
        assert_eq!(outputs[1], "Cities_Statistics_2");
        assert!(crate::project::find_table(&s.project.tables, "Cities_Statistics").is_some());
        assert!(crate::project::find_table(&s.project.tables, "Cities_Statistics_2").is_some());
        let ids: std::collections::HashSet<u32> = s.project.tables.iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), s.project.tables.len(), "all table ids must be distinct");
        assert!(s.notices.iter().any(|n| n.text.contains("output name(s) collided and were renamed")));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn batch_run_table_file_input_no_false_collision_notice() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_file_stat_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("cities.geojson");
        std::fs::write(&path, r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[10.0,20.0]},"properties":{"pop":100}}]}"#).unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "summaryStatistics",
                    "inputs": [path.to_str().unwrap()],
                    "params": {"field": "pop"}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0], "cities_Statistics");
        assert!(crate::project::find_table(&s.project.tables, "cities_Statistics").is_some());
        assert!(
            !s.notices.iter().any(|n| n.text.contains("output name(s) collided and were renamed")),
            "file-input summaryStatistics must not emit false collision notice"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn batch_run_file_input_pattern_no_false_collision_with_temp_layer() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_file_coll_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("cities.geojson");
        std::fs::write(&path, r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[10.0,20.0]},"properties":{}}]}"#).unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": [path.to_str().unwrap()],
                    "outputPattern": "{input}",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0], "cities");
        assert!(s.map().layer_by_name("cities").is_some());
        assert!(!s.notices.iter().any(|n| n.text.contains("output name(s) collided and were renamed")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn batch_run_failed_item_does_not_bump_collision_suffix() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_fail_suffix_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let dir1 = root.join("d1");
        let dir2 = root.join("d2");
        std::fs::create_dir_all(&dir1).unwrap();
        std::fs::create_dir_all(&dir2).unwrap();
        let f1 = dir1.join("Cities.geojson");
        let f2 = dir2.join("Cities.geojson");
        std::fs::write(&f1, "bad json").unwrap();
        std::fs::write(&f2, r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[10.0,20.0]},"properties":{}}]}"#).unwrap();

        let mut s = test_session();
        let pre = Dataset::new("Cities_buffer", rustgis_data::GeometryType::Point, Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(10, pre));

        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": [f1.display().to_string(), f2.display().to_string()],
                    "outputPattern": "{input}_{tool}",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["failed"], 1);
        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0], "Cities_buffer_2");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_output_layers_only_table_collected() {
        rustgis_analysis::register_external(
            "test_output_layers_table_suite",
            || {
                vec![rustgis_analysis::ToolSpec {
                    id: "test:table_only",
                    name: "Table Only Tool",
                    toolbox: "Test",
                    summary: "",
                    params: &[rustgis_analysis::ParamSpec {
                        name: "input",
                        label: "Input Layer",
                        kind: rustgis_analysis::ParamKind::Layer,
                        required: true,
                        default: "",
                        help: "",
                    }],
                    run: rustgis_analysis::external_placeholder,
                }]
            },
            |_spec, _inputs, _params| {
                let table = Dataset::new(
                    "CustomPluginTable",
                    rustgis_data::GeometryType::None,
                    vec![rustgis_data::Field::new("col", rustgis_data::FieldType::Integer)],
                );
                Ok(rustgis_analysis::ToolResult {
                    output: rustgis_analysis::Output::Layers(vec![rustgis_analysis::OutputLayer::Table(table)]),
                    messages: Vec::new(),
                })
            },
        );

        let mut s = Session::new(crate::Project::default());
        let ds = Dataset::new(
            "InputLayer",
            rustgis_data::GeometryType::Point,
            vec![rustgis_data::Field::new("val", rustgis_data::FieldType::Integer)],
        );
        s.map_mut().add_layer(rustgis_map::Layer::new(0, ds));

        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "test:table_only",
                    "inputs": ["InputLayer"],
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0], "CustomPluginTable");
        assert!(crate::project::find_table(&s.project.tables, "CustomPluginTable").is_some());
    }

    #[test]
    fn batch_run_non_string_param_notified() {
        let mut s = test_session();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "param": 123,
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res["succeeded"], 1);
        assert!(s.notices.iter().any(|n| n.text.contains("'param' must be a string")));
    }

    #[test]
    fn batch_run_preserves_tool_progress() {
        let mut s = test_session();
        let control = rustgis_analysis::control::Control::new();
        let res = run_batch_with(
            &mut s,
            &json!({
                "tool": "buffer",
                "inputs": ["Cities", "Parks"],
                "params": {"distance": 10}
            }),
            &control,
        )
        .unwrap();

        assert_eq!(res["total"], 2);
        assert_eq!(res["succeeded"], 2);
        // Progress reflects the last executed tool's completed features (Parks has 1 point feature)
        assert_eq!(control.progress(), Some((1, 1)));
    }

    #[test]
    fn batch_run_invalid_param_rejected() {
        let mut s = test_session();
        let res = s.run(
            "gp.batch",
            &json!({
                "tool": "buffer",
                "inputs": ["Cities"],
                "param": "imput",
                "params": {"distance": 10}
            }),
        );
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("tool 'buffer' has no parameter 'imput'"));
    }

    #[test]
    fn batch_run_single_input_string() {
        let mut s = test_session();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": "Cities",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res["total"], 1);
        assert_eq!(res["succeeded"], 1);
        assert!(s.map().layer_by_name("Cities_buffer").is_some());
    }

    #[test]
    fn batch_run_lossy_inputs_notified() {
        let mut s = test_session();
        let mark = s.notices.len();
        // Lossy input: empty string and non-string in array, plus non-object params
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities", "", 42, "   "],
                    "params": "not_an_object"
                }),
            )
            .unwrap();

        assert_eq!(res["total"], 1);
        assert_eq!(res["succeeded"], 1);
        let notices = &s.notices[mark..];
        assert!(notices.iter().any(|n| n.text.contains("3 invalid input(s) skipped")));
        assert!(notices.iter().any(|n| n.text.contains("'params' must be an object")));

        // Clean input says nothing
        let mut s2 = test_session();
        let mark2 = s2.notices.len();
        let _ = s2
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        let notices2 = &s2.notices[mark2..];
        assert!(
            !notices2.iter().any(|n| n.text.contains("skipped") || n.text.contains("invalid") || n.text.contains("params")),
            "clean input must give no skip notices: {notices2:?}"
        );
    }

    #[test]
    fn batch_run_file_input_update_retained() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_upd_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("parcels.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[0.0,0.0],[1.0,0.0],[1.0,1.0],[0.0,1.0],[0.0,0.0]]]},"properties":{}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "calculateGeometry",
                    "inputs": [f1.display().to_string()]
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let layers = res["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 1, "layers must include the updated layer id");
        let lid = layers[0].as_u64().unwrap() as u32;
        assert!(s.map().layer(lid).is_some());
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 1);
        assert!(s.map().layer_by_name(outputs[0].as_str().unwrap()).is_some());
        assert_eq!(s.project.gp_history[0].params["input"], f1.display().to_string());

        let (replay_cmd, replay_params) = crate::cmd::history::replay(&s.project.gp_history[0]).unwrap();
        assert_eq!(replay_cmd, "gp.batch");
        assert_eq!(replay_params["tool"], "calculateGeometry");
        assert_eq!(replay_params["inputs"][0], f1.display().to_string());
        let replay_res = s.run(&replay_cmd, &replay_params).unwrap();
        assert_eq!(replay_res["succeeded"], 1);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_cancelled_stops_remaining_items() {
        let mut s = test_session();
        let control = rustgis_analysis::control::Control::new();
        control.cancel();
        let res = run_batch_with(
            &mut s,
            &json!({
                "tool": "buffer",
                "inputs": ["Cities", "Parks"],
                "params": {"distance": 10}
            }),
            &control,
        )
        .unwrap();

        assert_eq!(res["total"], 2);
        assert_eq!(res["attempted"], 0);
        assert_eq!(res["succeeded"], 0);
        assert_eq!(res["failed"], 0);
        assert_eq!(res["cancelled"], true);
        assert!(s.notices.iter().any(|n| n.text.contains("cancelled after 0 of 2")));
    }

    static CANCEL_CTL: std::sync::Mutex<Option<std::sync::Arc<rustgis_analysis::control::Control>>> = std::sync::Mutex::new(None);

    fn cancel_item_runner(
        _spec: &rustgis_analysis::ToolSpec,
        _inputs: &dyn rustgis_analysis::Inputs,
        _params: &serde_json::Value,
    ) -> Result<rustgis_analysis::ToolResult, String> {
        if let Ok(guard) = CANCEL_CTL.lock()
            && let Some(ctl) = guard.as_ref()
        {
            ctl.cancel();
        }
        Err("cancelled in tool".into())
    }

    #[test]
    fn batch_run_cancel_during_item_records_interrupted_error() {
        let control = rustgis_analysis::control::Control::new();
        *CANCEL_CTL.lock().unwrap() = Some(control.clone());
        rustgis_analysis::register_external(
            "test_cancel_during_item_suite",
            || {
                vec![rustgis_analysis::ToolSpec {
                    id: "test:cancel_item",
                    name: "Cancel Item Tool",
                    toolbox: "Test",
                    summary: "",
                    params: &[rustgis_analysis::ParamSpec {
                        name: "input",
                        label: "Input Layer",
                        kind: rustgis_analysis::ParamKind::Layer,
                        required: true,
                        default: "",
                        help: "",
                    }],
                    run: rustgis_analysis::external_placeholder,
                }]
            },
            cancel_item_runner,
        );

        let mut s = Session::new(crate::Project::default());
        s.map_mut().add_layer(rustgis_map::Layer::new(0, Dataset::new("L1", rustgis_data::GeometryType::Point, Vec::new())));
        s.map_mut().add_layer(rustgis_map::Layer::new(1, Dataset::new("L2", rustgis_data::GeometryType::Point, Vec::new())));
        s.map_mut().add_layer(rustgis_map::Layer::new(2, Dataset::new("L3", rustgis_data::GeometryType::Point, Vec::new())));

        let res = run_batch_with(
            &mut s,
            &json!({
                "tool": "test:cancel_item",
                "inputs": ["L1", "L2", "L3"],
            }),
            &control,
        )
        .unwrap();

        *CANCEL_CTL.lock().unwrap() = None;

        assert_eq!(res["total"], 3);
        assert_eq!(res["attempted"], 1);
        assert_eq!(res["failed"], 0);
        assert_eq!(res["succeeded"], 0);
        assert_eq!(res["cancelled"], true);
        assert_eq!(res["errors"].as_array().unwrap().len(), 1);
        assert_eq!(res["errors"][0]["input"], "L1");
        assert_eq!(res["errors"][0]["error"], "Cancel Item Tool was cancelled");
        assert_eq!(res["errors"][0]["interrupted"], true);
        assert!(!s.notices.iter().any(|n| n.text.contains("failed")));
        assert!(s.notices.iter().any(|n| n.text.contains("cancelled after 1 of 3 (Cancel Item Tool was cancelled)")));
    }

    static OUTER_CTL: std::sync::Mutex<Option<std::sync::Arc<rustgis_analysis::control::Control>>> = std::sync::Mutex::new(None);
    static OUTER_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// Presses Cancel on the run the batch is part of (not the batch's own control).
    fn cancel_outer_runner(
        _spec: &rustgis_analysis::ToolSpec,
        _inputs: &dyn rustgis_analysis::Inputs,
        _params: &serde_json::Value,
    ) -> Result<rustgis_analysis::ToolResult, String> {
        OUTER_RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Ok(guard) = OUTER_CTL.lock()
            && let Some(ctl) = guard.as_ref()
        {
            ctl.cancel();
        }
        Err("cancelled from outside".into())
    }

    #[test]
    fn batch_run_stops_with_the_run_it_is_part_of() {
        rustgis_analysis::register_external(
            "test_cancel_outer_suite",
            || {
                vec![rustgis_analysis::ToolSpec {
                    id: "test:cancel_outer",
                    name: "Cancel Outer Tool",
                    toolbox: "Test",
                    summary: "",
                    params: &[rustgis_analysis::ParamSpec {
                        name: "input",
                        label: "Input Layer",
                        kind: rustgis_analysis::ParamKind::Layer,
                        required: true,
                        default: "",
                        help: "",
                    }],
                    run: rustgis_analysis::external_placeholder,
                }]
            },
            cancel_outer_runner,
        );
        let mut s = Session::new(crate::Project::default());
        for (i, name) in ["L1", "L2", "L3"].into_iter().enumerate() {
            s.map_mut().add_layer(rustgis_map::Layer::new(i as u32, Dataset::new(name, rustgis_data::GeometryType::Point, Vec::new())));
        }
        // The batch runs as a command under the control of the run it is part of (a script run
        // ahead with Cancel in the status bar): cancelling that run stops it after the item under
        // way, and it says how many it ran.
        let control = rustgis_analysis::control::Control::new();
        *OUTER_CTL.lock().unwrap() = Some(control.clone());
        let mut res = None;
        let ran = rustgis_analysis::run_under("Run Script", &control, || {
            res = Some(s.run("gp.batch", &json!({"tool": "test:cancel_outer", "inputs": ["L1", "L2", "L3"]})));
            Ok(())
        });
        *OUTER_CTL.lock().unwrap() = None;
        assert!(ran.is_err(), "the run it is part of was cancelled");
        let res = res.unwrap().unwrap();
        assert_eq!(OUTER_RUNS.load(std::sync::atomic::Ordering::SeqCst), 1, "the items after it never ran");
        assert_eq!(res["cancelled"], true);
        assert_eq!(res["attempted"], 1);
        assert_eq!(res["total"], 3);
        assert!(s.notices.iter().any(|n| n.text.contains("Batch 'test:cancel_outer' cancelled after 1 of 3")), "{:?}", s.notices);
    }

    fn empty_output_runner(
        _spec: &rustgis_analysis::ToolSpec,
        _inputs: &dyn rustgis_analysis::Inputs,
        _params: &serde_json::Value,
    ) -> Result<rustgis_analysis::ToolResult, String> {
        Ok(rustgis_analysis::ToolResult {
            output: rustgis_analysis::Output::Layers(Vec::new()),
            messages: vec!["custom message from empty tool".to_string()],
        })
    }

    #[test]
    fn batch_run_tool_producing_no_output_is_failure() {
        rustgis_analysis::register_external(
            "test_empty_output_suite",
            || {
                vec![rustgis_analysis::ToolSpec {
                    id: "test:empty_output",
                    name: "Empty Output Tool",
                    toolbox: "Test",
                    summary: "",
                    params: &[rustgis_analysis::ParamSpec {
                        name: "input",
                        label: "Input Layer",
                        kind: rustgis_analysis::ParamKind::Layer,
                        required: true,
                        default: "",
                        help: "",
                    }],
                    run: rustgis_analysis::external_placeholder,
                }]
            },
            empty_output_runner,
        );

        let mut s = Session::new(crate::Project::default());
        let ds = Dataset::new("L1", rustgis_data::GeometryType::Point, Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(0, ds));

        let mark = s.notices.len();
        let res = s.run(
            "gp.batch",
            &json!({
                "tool": "test:empty_output",
                "inputs": ["L1"],
            }),
        );
        let err = res.unwrap_err();
        assert!(err.contains("Batch 'test:empty_output' failed"));
        assert!(err.contains("custom message from empty tool"));
        assert!(s.notices[mark..].iter().any(|n| n.text.contains("1 failed (L1)")));
    }

    fn empty_output_multi_message_runner(
        _spec: &rustgis_analysis::ToolSpec,
        _inputs: &dyn rustgis_analysis::Inputs,
        _params: &serde_json::Value,
    ) -> Result<rustgis_analysis::ToolResult, String> {
        Ok(rustgis_analysis::ToolResult {
            output: rustgis_analysis::Output::Layers(Vec::new()),
            messages: vec!["Processed 10 features".to_string(), "0 features matched criteria".to_string()],
        })
    }

    #[test]
    fn batch_run_tool_producing_no_output_takes_last_message_as_error() {
        rustgis_analysis::register_external(
            "test_empty_output_multi_msg_suite",
            || {
                vec![rustgis_analysis::ToolSpec {
                    id: "test:empty_output_multi_msg",
                    name: "Empty Output Multi Message Tool",
                    toolbox: "Test",
                    summary: "",
                    params: &[rustgis_analysis::ParamSpec {
                        name: "input",
                        label: "Input Layer",
                        kind: rustgis_analysis::ParamKind::Layer,
                        required: true,
                        default: "",
                        help: "",
                    }],
                    run: rustgis_analysis::external_placeholder,
                }]
            },
            empty_output_multi_message_runner,
        );

        let mut s = Session::new(crate::Project::default());
        let ds = Dataset::new("L1", rustgis_data::GeometryType::Point, Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(0, ds));

        let res = s.run(
            "gp.batch",
            &json!({
                "tool": "test:empty_output_multi_msg",
                "inputs": ["L1"],
            }),
        );
        let err = res.unwrap_err();
        assert!(err.contains("0 features matched criteria"));
    }

    fn write_test_shapefile(path: &std::path::Path) {
        let mut ds = Dataset::new("test", rustgis_data::GeometryType::Point, Vec::new());
        ds.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 20.0))), Vec::new());
        rustgis_data::io::write_path(&ds, path).unwrap();
    }

    #[test]
    fn batch_run_folder_skips_shp_xml_sidecar() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_shpxml_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("rivers.shp");
        write_test_shapefile(&f1);
        let f2 = root.join("rivers.shp.xml");
        std::fs::write(&f2, b"<xml></xml>").unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "convexHull",
                    "folder": root.display().to_string(),
                }),
            )
            .unwrap();

        assert_eq!(res["total"], 1);
        assert!(!s.notices.iter().any(|n| n.text.contains("unsupported files skipped")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_folder_unrelated_xml_counted_as_unsupported() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_unrelxml_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("rivers.shp");
        write_test_shapefile(&f1);
        let f2 = root.join("rivers.shp.xml");
        std::fs::write(&f2, b"<xml></xml>").unwrap();
        let f3 = root.join("rivers.xml");
        std::fs::write(&f3, b"<xml></xml>").unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "convexHull",
                    "folder": root.display().to_string(),
                }),
            )
            .unwrap();

        assert_eq!(res["total"], 1);
        assert!(s.notices.iter().any(|n| n.text.contains("1 unsupported files skipped")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_folder_lone_wal_emits_notice() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_lonewal_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let valid = root.join("valid.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{}}]}"#;
        std::fs::write(&valid, sample_geojson).unwrap();

        let f = root.join("stray.wal");
        std::fs::write(&f, b"stray wal content").unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s.run(
            "gp.batch",
            &json!({
                "tool": "convexHull",
                "folder": root.display().to_string(),
            }),
        );
        assert!(res.is_ok());
        assert!(s.notices.iter().any(|n| n.text.contains("1 unsupported files skipped")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_empty_inputs_with_folder_no_bogus_notice() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_empty_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("layer.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();

        let mut s = Session::new(crate::Project::default());
        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "convexHull",
                    "inputs": [],
                    "folder": root.display().to_string()
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let notices = &s.notices[mark..];
        assert!(!notices.iter().any(|n| n.text.contains("both 'inputs' and 'folder'")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_aggregated_collision_notice() {
        let mut s = test_session();
        let pre1 = Dataset::new("Cities_buffer", rustgis_data::GeometryType::Point, Vec::new());
        let pre2 = Dataset::new("Cities_buffer_2", rustgis_data::GeometryType::Point, Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(10, pre1));
        s.map_mut().add_layer(rustgis_map::Layer::new(11, pre2));

        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let notices = &s.notices[mark..];
        assert!(notices.iter().any(|n| n.text.contains("1 output name(s) collided and were renamed")));
    }

    #[test]
    fn batch_run_rejects_file_writing_tools() {
        let mut s = test_session();
        let res = s.run(
            "gp.batch",
            &json!({
                "tool": "vectorToPmtiles",
                "inputs": ["Cities"],
            }),
        );
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("writes a file, not a layer"));
    }

    #[test]
    fn batch_run_active_layer_preserved_after_file_batch() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_active_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("cities.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();

        let mut s = test_session();
        s.active_layer = Some(1); // "Parks" is active

        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": [f1.display().to_string()],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        assert_eq!(s.active_layer, Some(1), "user active layer must be preserved");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_selection_tool_on_file_retained() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_sel_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("cities.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{"pop":100}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "selectByAttributes",
                    "inputs": [f1.display().to_string()],
                    "params": {"where": "pop > 50"}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let layers = res["layers"].as_array().unwrap();
        assert_eq!(layers.len(), 1, "layers must include the selected layer id");
        let lid = layers[0].as_u64().unwrap() as u32;
        assert!(s.map().layer(lid).is_some());
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 1);
        assert!(s.map().layer_by_name(outputs[0].as_str().unwrap()).is_some());
        assert_eq!(s.project.gp_history[0].params["input"], f1.display().to_string());

        let (replay_cmd, replay_params) = crate::cmd::history::replay(&s.project.gp_history[0]).unwrap();
        assert_eq!(replay_cmd, "gp.batch");
        assert_eq!(replay_params["tool"], "selectByAttributes");
        assert_eq!(replay_params["inputs"][0], f1.display().to_string());
        assert_eq!(replay_params["param"], "input");
        let replay_res = s.run(&replay_cmd, &replay_params).unwrap();
        assert_eq!(replay_res["succeeded"], 1);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_selection_tool_on_map_layer_outputs_empty() {
        let mut s = Session::new(crate::Project::default());
        let mut ds = Dataset::new(
            "Cities",
            rustgis_data::GeometryType::Point,
            vec![rustgis_data::Field::new("pop", rustgis_data::FieldType::Integer)],
        );
        ds.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 20.0))), vec![rustgis_data::Value::Int(100)]);
        s.map_mut().add_layer(rustgis_map::Layer::new(0, ds));

        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "selectByAttributes",
                    "inputs": ["Cities"],
                    "params": {"where": "pop > 50"}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert!(outputs.is_empty(), "selection on existing map layer must not report layer as an output: {outputs:?}");
        assert_eq!(s.map().layer_by_name("Cities").unwrap().selection.len(), 1);
    }

    #[test]
    fn batch_run_tool_without_output_param_set_emits_no_ignored_notice() {
        let mut s = Session::new(crate::Project::default());
        let mut ds = Dataset::new(
            "Cities",
            rustgis_data::GeometryType::Point,
            vec![rustgis_data::Field::new("pop", rustgis_data::FieldType::Integer)],
        );
        ds.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 20.0))), vec![rustgis_data::Value::Int(100)]);
        s.map_mut().add_layer(rustgis_map::Layer::new(0, ds));

        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "summaryStatistics",
                    "inputs": ["Cities"],
                    "params": {"field": "pop", "output": "ignored_or_passthrough"}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let notices = &s.notices[mark..];
        assert!(
            !notices.iter().any(|n| n.text.contains("params.output is ignored")),
            "tool without output param must not emit 'params.output is ignored' notice: {notices:?}"
        );
    }

    #[test]
    fn batch_run_explicit_param_retained_in_history_replay() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_exp_param_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("cities.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{"pop":100}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "selectByAttributes",
                    "inputs": [f1.display().to_string()],
                    "param": "input",
                    "params": {"where": "pop > 50"}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let (replay_cmd, replay_params) = crate::cmd::history::replay(&s.project.gp_history[0]).unwrap();
        assert_eq!(replay_cmd, "gp.batch");
        assert_eq!(replay_params["param"], "input");
        let replay_res = s.run(&replay_cmd, &replay_params).unwrap();
        assert_eq!(replay_res["succeeded"], 1);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_aggregates_tool_messages() {
        let mut s = test_session();
        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities", "Parks"],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 2);
        let notices = &s.notices[mark..];
        assert!(notices.iter().any(|n| n.text.contains("2 item(s) reported messages (Buffer: 1 features at 10 Kilometers (×2))")));
    }

    #[test]
    fn batch_run_different_feature_counts_emits_single_message_notice() {
        let mut s = Session::new(crate::Project::default());
        let mut l1 = Dataset::new("Layer1", rustgis_data::GeometryType::Point, Vec::new());
        l1.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 20.0))), Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(0, l1));

        let mut l2 = Dataset::new("Layer2", rustgis_data::GeometryType::Point, Vec::new());
        l2.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(15.0, 25.0))), Vec::new());
        l2.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(16.0, 26.0))), Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(1, l2));

        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Layer1", "Layer2"],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 2);
        let recent: Vec<_> = s.notices[mark..].iter().filter(|n| n.text.contains("reported messages")).collect();
        assert_eq!(recent.len(), 1, "must emit exactly one tool messages notice: {recent:?}");
        assert_eq!(
            recent[0].text,
            "gp.batch: 2 item(s) reported messages (Buffer: 1 features at 10 Kilometers; Buffer: 2 features at 10 Kilometers)"
        );
    }

    #[test]
    fn batch_run_different_tool_messages_named_in_notice() {
        let mut s = Session::new(crate::Project::default());
        let mut l1 = Dataset::new("Layer1", rustgis_data::GeometryType::Point, Vec::new());
        l1.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 20.0))), Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(0, l1));

        let mut l2 = Dataset::new("Layer2", rustgis_data::GeometryType::Point, Vec::new());
        l2.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(15.0, 25.0))), Vec::new());
        l2.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(16.0, 26.0))), Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(1, l2));

        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Layer1", "Layer2"],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 2);
        let notices = &s.notices[mark..];
        let msg_notice = notices.iter().find(|n| n.text.contains("reported messages")).unwrap();
        assert!(msg_notice.text.contains("Buffer: 1 features at 10 Kilometers"));
        assert!(msg_notice.text.contains("Buffer: 2 features at 10 Kilometers"));
    }

    #[test]
    fn batch_run_null_options_emit_no_spurious_notices() {
        let mut s = test_session();
        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "params": null,
                    "folder": null,
                    "param": null,
                    "outputPattern": null
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let notices = &s.notices[mark..];
        assert!(
            !notices.iter().any(|n| n.text.contains("must be an object")
                || n.text.contains("both 'inputs' and 'folder'")
                || n.text.contains("must be a string")
                || n.text.contains("must be an array")),
            "null options must not emit spurious notices: {notices:?}"
        );

        let mark2 = s.notices.len();
        let res2 = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "params": {
                        "input": null,
                        "output": null,
                        "distance": 10
                    }
                }),
            )
            .unwrap();

        assert_eq!(res2["succeeded"], 1);
        let notices2 = &s.notices[mark2..];
        assert!(
            !notices2.iter().any(|n| n.text.contains("output is ignored") || n.text.contains("is replaced by the batch inputs")),
            "null param fields must not emit spurious notices: {notices2:?}"
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn batch_run_output_pattern_on_tool_without_output_emits_notice() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_opt_notice_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let p1 = root.join("cities1.geojson");
        let p2 = root.join("cities2.geojson");
        let content = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[10.0,20.0]},"properties":{"pop":100}}]}"#;
        std::fs::write(&p1, content).unwrap();
        std::fs::write(&p2, content).unwrap();

        // 1. Run with outputPattern: emits notice
        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "summaryStatistics",
                    "inputs": [p1.to_str().unwrap(), p2.to_str().unwrap()],
                    "outputPattern": "{input}_stats",
                    "params": {"field": "pop"}
                }),
            )
            .unwrap();
        assert_eq!(res["succeeded"], 2);
        assert!(
            s.notices.iter().any(|n| n.text.contains(
                "'outputPattern' does not apply to 'summaryStatistics' (it names its outputs itself); 2 item(s) used their own names"
            )),
            "notices: {:?}",
            s.notices
        );

        // 2. Clean run without outputPattern: emits no notice
        let mut s_clean = Session::new(crate::Project::default());
        let res_clean = s_clean
            .run(
                "gp.batch",
                &json!({
                    "tool": "summaryStatistics",
                    "inputs": [p1.to_str().unwrap(), p2.to_str().unwrap()],
                    "params": {"field": "pop"}
                }),
            )
            .unwrap();
        assert_eq!(res_clean["succeeded"], 2);
        assert!(!s_clean.notices.iter().any(|n| n.text.contains("'outputPattern' does not apply")), "clean run must not emit notice");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_unrecognised_option_emits_notice() {
        let mut s = test_session();
        let mark = s.notices.len() as u64;
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "outputpattern": "{input}_out",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res["succeeded"], 1);
        let recent = s.notices_since(mark);
        assert!(recent.iter().any(|n| n.text.contains("1 unrecognised option ignored (outputpattern)")), "recent notices: {recent:?}");

        // Clean call emits no unrecognised option notice
        let mut s_clean = test_session();
        let mark_clean = s_clean.notices.len() as u64;
        let res_clean = s_clean
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "outputPattern": "{input}_out",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res_clean["succeeded"], 1);
        let recent_clean = s_clean.notices_since(mark_clean);
        assert!(
            !recent_clean.iter().any(|n| n.text.contains("unrecognised")),
            "clean call must not emit unrecognised option notice: {recent_clean:?}"
        );
    }

    #[test]
    fn batch_run_unrecognised_param_in_params_emits_notice() {
        let mut s = test_session();
        let mark = s.notices.len() as u64;
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "params": {"distanc": 50, "distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res["succeeded"], 1);
        let recent = s.notices_since(mark);
        assert!(
            recent.iter().any(|n| n.text.contains("1 unrecognised parameter(s) ignored in 'params' (distanc)")),
            "recent notices: {recent:?}"
        );

        // Clean call emits no unrecognised notice
        let mut s_clean = test_session();
        let mark_clean = s_clean.notices.len() as u64;
        let res_clean = s_clean
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res_clean["succeeded"], 1);
        let recent_clean = s_clean.notices_since(mark_clean);
        assert!(
            !recent_clean.iter().any(|n| n.text.contains("unrecognised")),
            "clean call must not emit unrecognised notice: {recent_clean:?}"
        );
    }

    #[test]
    fn batch_run_params_batch_param_overwritten_notified() {
        let mut s = test_session();
        let mark = s.notices.len() as u64;
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "params": {"input": "Parks", "distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res["succeeded"], 1);
        let recent = s.notices_since(mark);
        assert!(
            recent.iter().any(|n| n.text.contains("gp.batch: 'params.input' is replaced by the batch inputs")),
            "recent notices: {recent:?}"
        );

        // Clean call without batch_param in params gives no replacement notice
        let mut s_clean = test_session();
        let mark_clean = s_clean.notices.len() as u64;
        let res_clean = s_clean
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res_clean["succeeded"], 1);
        let recent_clean = s_clean.notices_since(mark_clean);
        assert!(
            !recent_clean.iter().any(|n| n.text.contains("is replaced by the batch inputs")),
            "clean call must not emit replacement notice: {recent_clean:?}"
        );
    }

    #[test]
    fn batch_run_alias_precedence_emits_notice() {
        let mut s = test_session();

        // Both param and batch_param
        let mark1 = s.notices.len() as u64;
        let res1 = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "param": "input",
                    "batch_param": "input",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res1["succeeded"], 1);
        let recent1 = s.notices_since(mark1);
        assert!(
            recent1.iter().any(|n| n.text.contains("gp.batch: 'batch_param' ignored because 'param' is set")),
            "expected notice for dropped batch_param: {recent1:?}"
        );

        // Clean call with only param
        let mut s_clean1 = test_session();
        let mark_clean1 = s_clean1.notices.len() as u64;
        let res_clean1 = s_clean1
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "param": "input",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res_clean1["succeeded"], 1);
        let recent_clean1 = s_clean1.notices_since(mark_clean1);
        assert!(
            !recent_clean1.iter().any(|n| n.text.contains("ignored because")),
            "clean call must not emit dropped alias notice: {recent_clean1:?}"
        );

        // Both outputPattern and output_pattern
        let mut s2 = test_session();
        let mark2 = s2.notices.len() as u64;
        let res2 = s2
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "outputPattern": "{input}_pat1",
                    "output_pattern": "{input}_pat2",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res2["succeeded"], 1);
        let recent2 = s2.notices_since(mark2);
        assert!(
            recent2.iter().any(|n| n.text.contains("gp.batch: 'output_pattern' ignored because 'outputPattern' is set")),
            "expected notice for dropped output_pattern: {recent2:?}"
        );

        // Clean call with only outputPattern
        let mut s_clean2 = test_session();
        let mark_clean2 = s_clean2.notices.len() as u64;
        let res_clean2 = s_clean2
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "outputPattern": "{input}_pat1",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res_clean2["succeeded"], 1);
        let recent_clean2 = s_clean2.notices_since(mark_clean2);
        assert!(
            !recent_clean2.iter().any(|n| n.text.contains("ignored because")),
            "clean call must not emit dropped alias notice: {recent_clean2:?}"
        );
    }

    #[test]
    fn batch_run_join_field_aliases_accepted_without_unrecognised_notice() {
        let mut s = Session::new(crate::Project::default());
        let mut ds_target = Dataset::new(
            "Tracts",
            rustgis_data::GeometryType::Point,
            vec![rustgis_data::Field::new("GEOID", rustgis_data::FieldType::Text)],
        );
        ds_target.push_next(
            Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.0, 0.0))),
            vec![rustgis_data::Value::Text("01".into())],
        );
        s.map_mut().add_layer(rustgis_map::Layer::new(0, ds_target));

        let mut ds_join =
            Dataset::new("Census", rustgis_data::GeometryType::None, vec![rustgis_data::Field::new("FIPS", rustgis_data::FieldType::Text)]);
        ds_join.push_next(None, vec![rustgis_data::Value::Text("01".into())]);
        s.project.add_table(ds_join);

        let mark = s.notices.len() as u64;
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "joinField",
                    "inputs": ["Tracts"],
                    "params": {
                        "target_field": "GEOID",
                        "join_table": "Census",
                        "join_field": "FIPS",
                        "duplicate_strategy": "First"
                    }
                }),
            )
            .unwrap();
        assert_eq!(res["succeeded"], 1);
        let recent = s.notices_since(mark);
        assert!(
            !recent.iter().any(|n| n.text.contains("unrecognised")),
            "joinField aliases must not emit unrecognised parameter notice: {recent:?}"
        );
    }

    #[test]
    fn batch_input_takes_precedence_over_a_static_alias() {
        let mut s = Session::new(crate::Project::default());
        for (i, name) in ["Tracts", "Blocks", "Other"].into_iter().enumerate() {
            let mut ds = Dataset::new(
                name,
                rustgis_data::GeometryType::Point,
                vec![rustgis_data::Field::new("GEOID", rustgis_data::FieldType::Text)],
            );
            ds.push_next(
                Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.0, 0.0))),
                vec![rustgis_data::Value::Text("01".into())],
            );
            s.map_mut().add_layer(rustgis_map::Layer::new(i as u32, ds));
        }
        let mut join =
            Dataset::new("Census", rustgis_data::GeometryType::None, vec![rustgis_data::Field::new("FIPS", rustgis_data::FieldType::Text)]);
        join.push_next(None, vec![rustgis_data::Value::Text("01".into())]);
        s.project.add_table(join);
        let before = s.project.gp_history.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "joinField",
                    "inputs": ["Tracts", "Blocks"],
                    "param": "target_layer",
                    // `input` names Join Field's target too: it must not stand in for each batch input.
                    "params": {"input": "Other", "targetField": "GEOID", "joinTable": "Census", "joinField": "FIPS"}
                }),
            )
            .unwrap();
        assert_eq!(res["succeeded"], 2, "{res}");
        let targets: Vec<_> = s.project.gp_history[before..].iter().map(|r| r.params["target_layer"].clone()).collect();
        assert_eq!(targets, [json!("Tracts"), json!("Blocks")]);
        assert!(s.project.gp_history[before..].iter().all(|r| r.params.get("input").is_none()));
        let other = s.map().layer_by_name("Other").unwrap();
        assert!(!other.dataset.fields.iter().any(|f| f.name == "FIPS"), "the static alias's layer was joined");
    }

    #[test]
    fn batch_item_whose_input_cant_be_read_is_kept_in_the_history() {
        let mut s = test_session();
        let before = s.project.gp_history.len();
        // A name that is neither a layer, a table nor a file: the tool's input can't be mapped.
        let res = s.run("gp.batch", &json!({"tool": "buffer", "inputs": ["Cities", "Nowhere"], "params": {"distance": 1}})).unwrap();
        assert_eq!(res["failed"], 1, "{res}");
        let failed: Vec<_> = s.project.gp_history[before..].iter().filter(|r| !r.ok).collect();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].params["input"], "Nowhere");
    }

    #[test]
    fn a_batch_over_too_many_inputs_is_refused() {
        let mut s = test_session();
        let inputs = vec![json!("Cities"); super::MAX_BATCH_ITEMS + 1];
        let before = s.map().layers.len();
        let err = s.run("gp.batch", &json!({"tool": "buffer", "inputs": inputs, "params": {"distance": 1}})).unwrap_err();
        assert!(err.contains(&format!("{} inputs", super::MAX_BATCH_ITEMS + 1)), "{err}");
        assert_eq!(s.map().layers.len(), before, "nothing ran");
    }

    #[test]
    fn a_batch_at_the_input_limit_is_accepted() {
        let mut s = test_session();
        let control = rustgis_analysis::control::Control::new();
        control.cancel();
        let inputs = vec![json!("Cities"); super::MAX_BATCH_ITEMS];
        let res = super::run_batch_with(&mut s, &json!({"tool": "buffer", "inputs": inputs, "params": {"distance": 1}}), &control).unwrap();
        assert_eq!(res["total"], super::MAX_BATCH_ITEMS);
        assert_eq!(res["cancelled"], true);
    }

    #[test]
    fn a_folder_over_the_limit_is_refused_before_any_item_runs() {
        let dir = std::env::temp_dir().join(format!("rustgis-batch-limit-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..=super::MAX_BATCH_ITEMS {
            std::fs::write(dir.join(format!("{i}.csv")), "x,y\n").unwrap();
        }
        let mut s = test_session();
        let layers = s.map().layers.len();
        let history = s.project.gp_history.len();
        let res = s.run("gp.batch", &json!({"tool": "buffer", "folder": dir, "params": {"distance": 1}}));
        let _ = std::fs::remove_dir_all(&dir);
        let err = res.unwrap_err();
        assert!(err.contains(&format!("more than {} supported files", super::MAX_BATCH_ITEMS)), "{err}");
        assert_eq!(s.map().layers.len(), layers, "no item ran");
        assert_eq!(s.project.gp_history.len(), history, "no item was attempted");
    }

    #[test]
    fn a_folder_of_other_and_orphan_sidecar_files_runs_its_data_files() {
        let dir = std::env::temp_dir().join(format!("rustgis-batch-other-{}-{}", std::process::id(), now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pts.csv"), "x,y\n1,2\n").unwrap();
        // Text files, `.xml` files that aren't `<name>.shp.xml` and `.dbf` files with no `.shp`
        // are none of them sidecars of a file the batch runs.
        for i in 0..50 {
            for ext in ["txt", "xml", "dbf"] {
                std::fs::write(dir.join(format!("{i}.{ext}")), "").unwrap();
            }
        }
        let mut s = test_session();
        let res = s.run("gp.batch", &json!({"tool": "buffer", "folder": dir, "params": {"distance": 1}}));
        let _ = std::fs::remove_dir_all(&dir);
        let res = res.unwrap();
        assert_eq!(res["total"], 1, "{res}");
        assert!(s.notices.iter().any(|n| n.text.contains("150 unsupported files skipped")), "{:?}", s.notices);
    }

    #[test]
    fn a_static_alias_dropped_from_each_item_is_reported() {
        let mut s = Session::new(crate::Project::default());
        let mut ds = Dataset::new(
            "Tracts",
            rustgis_data::GeometryType::Point,
            vec![rustgis_data::Field::new("GEOID", rustgis_data::FieldType::Text)],
        );
        ds.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.0, 0.0))), vec![rustgis_data::Value::Text("01".into())]);
        s.map_mut().add_layer(rustgis_map::Layer::new(0, ds));
        let mut join =
            Dataset::new("Census", rustgis_data::GeometryType::None, vec![rustgis_data::Field::new("FIPS", rustgis_data::FieldType::Text)]);
        join.push_next(None, vec![rustgis_data::Value::Text("01".into())]);
        s.project.add_table(join);
        let shared = json!({"targetField": "GEOID", "joinTable": "Census", "joinField": "FIPS"});
        let mark = s.notice_mark();
        s.run("gp.batch", &json!({"tool": "joinField", "inputs": ["Tracts"], "params": shared})).unwrap();
        assert!(!s.notices_since(mark).iter().any(|n| n.text.contains("other name(s)")), "a clean run says nothing");
        let mut with_alias = shared.clone();
        with_alias["input"] = json!("Tracts");
        let mark = s.notice_mark();
        s.run("gp.batch", &json!({"tool": "joinField", "inputs": ["Tracts"], "params": with_alias})).unwrap();
        assert!(
            s.notices_since(mark)
                .iter()
                .any(|n| n.text.contains("1 other name(s) for 'target' in 'params' replaced by the batch inputs (input)")),
            "{:?}",
            s.notices_since(mark)
        );
    }

    #[test]
    fn blank_folder_beside_inputs_is_not_reported() {
        let mut s = test_session();
        let mark = s.notices.len() as u64;
        s.run("gp.batch", &json!({"tool": "buffer", "inputs": ["Cities"], "folder": "  ", "params": {"distance": 1}})).unwrap();
        assert!(!s.notices_since(mark).iter().any(|n| n.text.contains("both 'inputs' and 'folder'")));
    }

    #[test]
    fn batch_run_failed_item_does_not_reserve_output_name() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_fail_name_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let dir1 = root.join("bad");
        let dir2 = root.join("good");
        std::fs::create_dir_all(&dir1).unwrap();
        std::fs::create_dir_all(&dir2).unwrap();

        let f1 = dir1.join("sample.geojson");
        let f2 = dir2.join("sample.geojson");
        std::fs::write(&f1, "not-valid-json").unwrap();
        std::fs::write(&f2, r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{}}]}"#).unwrap();

        let mut s = Session::new(crate::Project::default());
        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": [f1.display().to_string(), f2.display().to_string()],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["failed"], 1);
        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].as_str().unwrap(), "sample_buffer");
        let notices = &s.notices[mark..];
        assert!(!notices.iter().any(|n| n.text.contains("collided and were renamed")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_shapefile_with_sidecars_not_counted_as_unsupported() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_sidecars_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        write_test_shapefile(&root.join("test.shp"));

        let mut s = Session::new(crate::Project::default());
        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "folder": root.display().to_string(),
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res["total"], 1);
        let notices = &s.notices[mark..];
        assert!(
            !notices.iter().any(|n| n.text.contains("unsupported files skipped")),
            "shapefile companion files must not be reported as unsupported: {notices:?}"
        );

        std::fs::write(root.join("notes.txt"), b"some notes").unwrap();
        let mut s2 = Session::new(crate::Project::default());
        let mark2 = s2.notices.len();
        let res2 = s2
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "folder": root.display().to_string(),
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res2["total"], 1);
        let notices2 = &s2.notices[mark2..];
        assert!(
            notices2.iter().any(|n| n.text.contains("1 unsupported files skipped")),
            "exactly 1 unsupported file (notes.txt) should be reported: {notices2:?}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_tool_messages_notices_since_not_desynced() {
        let mut s = test_session();
        s.notify("Prior notification before batch", false);
        let mark = s.notice_mark();

        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities", "Parks"],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 2);
        let recent = s.notices_since(mark);
        assert!(!recent.iter().any(|t| t.text.contains("Prior notification")));
        assert!(recent.iter().any(|t| t.text.contains("2 item(s) reported messages (Buffer: 1 features at 10 Kilometers (×2))")));
    }

    #[test]
    fn batch_run_wildcard_skips_raster_with_notice() {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let raster = Raster::new(
            "Elevation",
            2,
            2,
            vec![vec![0.0, 1.0, 2.0, 3.0]],
            GeoTransform { origin_x: 0.0, origin_y: 0.0, pixel_width: 1.0, pixel_height: -1.0 },
            Crs::LonLat,
            None,
            ColorHint::Gray,
            "Float32",
        )
        .unwrap();

        let mut s = test_session();
        s.map_mut().add_raster("Elevation", rustgis_map::RasterLayer::from_path(raster, "test.tif"));

        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": "*",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 2);
        let notices = &s.notices[mark..];
        assert!(notices.iter().any(|n| n.text.contains("1 raster layer(s) left out of '*' (it runs vector layers)")));

        let mut s2 = test_session();
        let mark2 = s2.notices.len();
        let _ = s2
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": "*",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        let notices2 = &s2.notices[mark2..];
        assert!(!notices2.iter().any(|n| n.text.contains("raster layer(s)")));
    }

    #[test]
    fn batch_run_wildcard_with_folder_on_raster_only_map_returns_wildcard_error_and_reads_no_files() {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform, Raster};
        let raster = Raster::new(
            "Elevation",
            2,
            2,
            vec![vec![0.0, 1.0, 2.0, 3.0]],
            GeoTransform { origin_x: 0.0, origin_y: 0.0, pixel_width: 1.0, pixel_height: -1.0 },
            Crs::LonLat,
            None,
            ColorHint::Gray,
            "Float32",
        )
        .unwrap();

        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_wc_folder_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let dummy_file = root.join("should_not_run.geojson");
        std::fs::write(&dummy_file, b"{}").unwrap();

        let mut s = Session::new(crate::Project::default());
        s.map_mut().add_raster("Elevation", rustgis_map::RasterLayer::from_path(raster, "test.tif"));

        let control = std::sync::Arc::new(rustgis_analysis::control::Control::new());
        let res = run_batch_with(
            &mut s,
            &json!({
                "tool": "buffer",
                "inputs": "*",
                "folder": root.display().to_string(),
                "params": {"distance": 10}
            }),
            &control,
        );

        let err = res.unwrap_err();
        assert!(err.contains("'*' matched no vector layers on the active map"));
        assert!(s.notices.iter().any(|n| n.text.contains("both 'inputs' and 'folder' were provided")));
        assert!(s.notices.iter().any(|n| n.text.contains("1 raster layer(s) left out of '*'")));
        assert!(s.project.gp_history.is_empty(), "must read no files and run no tools");
        assert_eq!(s.map().layers.len(), 1, "no layers added");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_active_layer_preserved_on_selection_and_update() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_act_sel_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("items.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Polygon","coordinates":[[[0.0,0.0],[1.0,0.0],[1.0,1.0],[0.0,1.0],[0.0,0.0]]]},"properties":{"val":10}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();

        let mut s = test_session();
        s.active_layer = Some(1); // "Parks" is active

        let res_sel = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "selectByAttributes",
                    "inputs": [f1.display().to_string()],
                    "params": {"where": "val > 5"}
                }),
            )
            .unwrap();
        assert_eq!(res_sel["succeeded"], 1);
        assert_eq!(s.active_layer, Some(1), "active layer must remain Parks after selectByAttributes");

        let res_upd = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "calculateGeometry",
                    "inputs": [f1.display().to_string()]
                }),
            )
            .unwrap();
        assert_eq!(res_upd["succeeded"], 1);
        assert_eq!(s.active_layer, Some(1), "active layer must remain Parks after calculateGeometry");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_wildcard_on_empty_map_returns_specific_error() {
        let mut s = Session::new(crate::Project::default());
        let res = s.run(
            "gp.batch",
            &json!({
                "tool": "buffer",
                "inputs": "*",
                "params": {"distance": 10}
            }),
        );
        assert_eq!(res.unwrap_err(), "'*' matched no vector layers on the active map; add a vector layer or name the inputs");
    }

    #[test]
    fn batch_run_preserves_layer_name_with_surrounding_whitespace() {
        let mut s = Session::new(crate::Project::default());
        let mut ds1 = Dataset::new(" Cities ", rustgis_data::GeometryType::Point, Vec::new());
        ds1.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 20.0))), Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(0, ds1));

        let mut ds2 = Dataset::new("Cities", rustgis_data::GeometryType::Point, Vec::new());
        ds2.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(30.0, 40.0))), Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(1, ds2));

        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": [" Cities "],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs[0], "Cities _buffer");
        let out_layer = s.map().layer_by_name("Cities _buffer").unwrap();
        let extent = out_layer.dataset.extent();
        assert!(extent.contains(rustgis_geom::Coord::new(10.0, 20.0)));
        assert!(!extent.contains(rustgis_geom::Coord::new(30.0, 40.0)));
    }

    #[test]
    fn batch_run_trimmed_file_path_emits_notice() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_trim_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("cities.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();

        let mut s = Session::new(crate::Project::default());
        let padded = format!("  {}  ", f1.display());
        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": [padded],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs[0], "cities_buffer");
        let notices = &s.notices[mark..];
        assert!(notices.iter().any(|n| n.text.contains("1 file path(s) trimmed of surrounding whitespace")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_empty_folder_returns_specific_error() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_empty_folder_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s.run(
            "gp.batch",
            &json!({
                "tool": "convexHull",
                "folder": root.display().to_string()
            }),
        );
        assert_eq!(res.unwrap_err(), format!("folder '{}' has no supported vector files", root.display()));

        let res_blank = s.run(
            "gp.batch",
            &json!({
                "tool": "convexHull",
                "folder": "   "
            }),
        );
        assert_eq!(
            res_blank.unwrap_err(),
            "no inputs provided (specify 'inputs' as an array of layers/paths, '*' for all layers, or 'folder' as a directory path)"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_file_input_named_like_dataset_records_gp_batch_in_history() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_samename_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let f1 = root.join("cities.geojson");
        let sample_geojson = r#"{"type":"FeatureCollection","name":"cities.geojson","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{}}]}"#;
        std::fs::write(&f1, sample_geojson).unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": [f1.display().to_string()],
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let (replay_cmd, _) = crate::cmd::history::replay(s.project.gp_history.last().unwrap()).unwrap();
        assert_eq!(replay_cmd, "gp.batch");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_numeric_folder_notified_and_ignored() {
        let mut s = test_session();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": ["Cities"],
                    "folder": 123,
                    "params": {"distance": 10}
                }),
            )
            .unwrap();
        assert_eq!(res["succeeded"], 1);
        assert!(s.notices.iter().any(|n| n.text.contains("gp.batch: 'folder' must be a string, ignoring invalid value")));
        assert!(!s.notices.iter().any(|n| n.text.contains("both 'inputs' and 'folder' were provided")));
    }

    #[test]
    fn batch_run_all_failed_returns_error_with_failure_details() {
        let mut s = Session::new(crate::Project::default());
        let res = s.run(
            "gp.batch",
            &json!({
                "tool": "buffer",
                "inputs": ["NonExistentLayer1", "NonExistentLayer2"],
                "params": {"distance": 10}
            }),
        );
        let err = res.unwrap_err();
        assert!(err.contains("Batch 'buffer' failed"));
        assert!(err.contains("NonExistentLayer1"));
        assert!(err.contains("NonExistentLayer2"));
        assert!(s.notices.iter().any(|n| n.text.contains("2 failed (NonExistentLayer1, NonExistentLayer2)")));
    }

    #[test]
    fn batch_run_reader_warnings_aggregated_and_retained_in_history_and_result() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_reader_warn_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let geojson_content = json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {
                    "type": "Point",
                    "coordinates": [10.0, 20.0, 30.0]
                },
                "properties": {"name": "Test"}
            }]
        })
        .to_string();

        let files: Vec<_> = (1..=4)
            .map(|i| {
                let p = root.join(format!("file_{i}.geojson"));
                std::fs::write(&p, &geojson_content).unwrap();
                p.display().to_string()
            })
            .collect();

        let mut s = Session::new(crate::Project::default());
        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": files,
                    "params": {"distance": 5}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 4);
        assert_eq!(res["failed"], 0);

        // Result retained full diagnostics in "warnings"
        let warnings = res["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 4);
        for (i, w) in warnings.iter().enumerate() {
            assert!(w["input"].as_str().unwrap().contains(&format!("file_{}", i + 1)));
            assert!(w["warning"].as_str().unwrap().contains("Z or M"));
        }

        // Notices aggregated into a single notice per warning kind without truncating later files
        let notice = s.notices[mark..].iter().find(|n| n.text.contains("Z or M")).unwrap();
        assert!(notice.text.starts_with("gp.batch: 4 inputs ("));
        assert!(notice.text.contains("file_1.geojson"));
        assert!(notice.text.contains("file_2.geojson"));
        assert!(notice.text.contains("file_3.geojson"));
        assert!(notice.text.contains("file_4.geojson"));
        assert!(!notice.text.contains("and 1 more"));

        // History retains reader warnings for each run
        assert_eq!(s.project.gp_history.len(), 4);
        for record in &s.project.gp_history {
            assert!(record.ok);
            assert!(record.messages.iter().any(|m| m.contains("Z or M")));
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_setup_failure_records_failed_history_and_attaches_reader_warnings() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_setup_fail_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        // f1 is inside extent [0, 0, 50, 50]
        let f1 = root.join("f1.geojson");
        let content_inside = json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {
                    "type": "Point",
                    "coordinates": [10.0, 20.0, 30.0]
                },
                "properties": {"name": "Inside"}
            }]
        })
        .to_string();
        std::fs::write(&f1, content_inside).unwrap();

        // f2 is outside extent [0, 0, 50, 50], with a latitude past the poles (a second reader warning)
        let f2 = root.join("f2.geojson");
        let content_outside = json!({
            "type": "FeatureCollection",
            "features": [{
                "type": "Feature",
                "geometry": {
                    "type": "Point",
                    "coordinates": [100.0, 100.0, 30.0]
                },
                "properties": {"name": "Outside"}
            }]
        })
        .to_string();
        std::fs::write(&f2, content_outside).unwrap();

        let mut s = Session::new(crate::Project::default());
        s.project.gp_env.extent = Some(rustgis_geom::Extent::new(0.0, 0.0, 50.0, 50.0));

        let f1_str = f1.display().to_string();
        let f2_str = f2.display().to_string();

        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": [f1_str.clone(), f2_str.clone()],
                    "params": {"distance": 5}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        assert_eq!(res["failed"], 1);

        // Result retained diagnostics for both
        let warnings = res["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 3);
        assert!(warnings[0]["input"].as_str().unwrap().contains("f1.geojson"));
        for notice in ["Z or M", "latitude past ±90°"] {
            assert!(
                warnings.iter().any(|w| w["input"] == f2_str && w["warning"].as_str().is_some_and(|m| m.contains(notice))),
                "{notice}: {warnings:?}"
            );
        }

        let errors = res["errors"].as_array().unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["input"], f2_str);
        assert!(errors[0]["error"].as_str().unwrap().contains("environment's extent"));

        // History: each run has its own GpRecord
        assert_eq!(s.project.gp_history.len(), 2);

        // f1 record
        let rec1 = &s.project.gp_history[0];
        assert!(rec1.ok);
        assert_eq!(rec1.params["input"], f1_str);
        assert!(rec1.messages.iter().any(|m| m.contains("Z or M")));
        assert!(!rec1.messages.iter().any(|m| m.contains("environment's extent")));

        // f2 record: created for setup failure, contains its own error and reader warning
        let rec2 = &s.project.gp_history[1];
        assert!(!rec2.ok);
        assert_eq!(rec2.params["input"], f2_str);
        assert!(rec2.messages.iter().any(|m| m.contains("environment's extent")));
        assert!(rec2.messages.iter().any(|m| m.contains("Z or M")));
        assert!(rec2.messages.iter().any(|m| m.contains("latitude past ±90°")));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn run_missing_layer_returns_error_without_history_record() {
        let mut s = Session::new(crate::Project::default());
        let res = s.run(
            "gp.run",
            &json!({
                "tool": "buffer",
                "params": {"input": "NonExistentLayer", "distance": 10}
            }),
        );
        assert!(res.is_err());
        assert_eq!(s.project.gp_history.len(), 0);
    }

    #[test]
    fn batch_run_read_failure_preserves_output_pattern_in_history() {
        let root = std::env::temp_dir().join(format!(
            "rustgis_batch_test_read_fail_{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let good = root.join("good.geojson");
        let sample = r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},"properties":{}}]}"#;
        std::fs::write(&good, sample).unwrap();

        let bad = root.join("bad.geojson");
        std::fs::write(&bad, "not-json").unwrap();

        let mut s = Session::new(crate::Project::default());
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": [good.display().to_string(), bad.display().to_string()],
                    "outputPattern": "{input}_custom_buf",
                    "params": {"distance": 10}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        assert_eq!(res["failed"], 1);

        assert_eq!(s.project.gp_history.len(), 2);
        let bad_rec = &s.project.gp_history[1];
        assert!(!bad_rec.ok);
        let (_, replay_params) = bad_rec.command.as_ref().unwrap();
        assert_eq!(replay_params["outputPattern"], "{input}_custom_buf");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn batch_run_wildcard_skips_graphics_with_notice() {
        let mut s = Session::new(crate::Project::default());
        let mut ds_vec = Dataset::new("Rivers", rustgis_data::GeometryType::Point, Vec::new());
        ds_vec.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(10.0, 20.0))), Vec::new());
        s.map_mut().add_layer(rustgis_map::Layer::new(0, ds_vec));

        let mut ds_notes = Dataset::new("Notes", rustgis_data::GeometryType::Point, Vec::new());
        ds_notes.push_next(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(5.0, 5.0))), Vec::new());
        let mut notes_layer = rustgis_map::Layer::new(1, ds_notes);
        notes_layer.graphics = true;
        s.map_mut().add_layer(notes_layer);

        let mark = s.notices.len();
        let res = s
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": "*",
                    "params": {"distance": 5}
                }),
            )
            .unwrap();

        assert_eq!(res["succeeded"], 1);
        let outputs = res["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0], "Rivers_buffer");
        let notices = &s.notices[mark..];
        assert!(notices.iter().any(|n| n.text.contains("1 graphics layer(s) left out of '*'")));

        let mut s_empty = Session::new(crate::Project::default());
        let ds_only_notes = Dataset::new("Notes", rustgis_data::GeometryType::Point, Vec::new());
        let mut notes_only_layer = rustgis_map::Layer::new(0, ds_only_notes);
        notes_only_layer.graphics = true;
        s_empty.map_mut().add_layer(notes_only_layer);
        let err = s_empty
            .run(
                "gp.batch",
                &json!({
                    "tool": "buffer",
                    "inputs": "*",
                    "params": {"distance": 5}
                }),
            )
            .unwrap_err();
        assert_eq!(err, "'*' matched no vector layers on the active map; add a vector layer or name the inputs");
    }
}

/// A project dataset chosen with the same precedence as the actual tool input provider.
pub(crate) enum ProjectInput<'a> {
    Layer(&'a rustgis_map::Layer),
    Table(&'a crate::project::Table),
}
impl<'a> ProjectInput<'a> {
    fn dataset(self) -> &'a Dataset {
        match self {
            Self::Layer(l) => &l.dataset,
            Self::Table(t) => &t.dataset,
        }
    }
}
pub(crate) fn project_input<'a>(
    map: &'a rustgis_map::Map,
    tables: &'a [crate::project::Table],
    name: &str,
    raster: bool,
) -> Option<ProjectInput<'a>> {
    map.layer_by_name(name).map(ProjectInput::Layer).or_else(|| {
        (!raster).then(|| crate::project::find_table(tables, name).and_then(|i| tables.get(i)).map(ProjectInput::Table)).flatten()
    })
}
/// Builtin VectorLayers reads absent map names as files before MapInputs' table fallback.
pub(crate) fn vector_file_input(spec: &rustgis_analysis::ToolSpec, map: &rustgis_map::Map, name: &str) -> bool {
    rustgis_analysis::TOOLS.iter().any(|t| std::ptr::eq(t, spec)) && map.layer_by_name(name).is_none()
}
