//! Analyze tab: run geoprocessing tools and keep their history.

use rustgis_analysis::{Inputs, Output, OutputLayer, RasterInput};
use rustgis_data::Dataset;
use rustgis_data::loss::{LossCode, LossMessage};
use rustgis_data::raster::Crs;
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
                r#"{"target": layer, "join": layer, "match"?: "Intersects" (default) | "Contains" | "Within" | "Have their center in" | "Largest overlap" | "Closest" | "Within a distance", "distance"?: number (Closest: how far to look; Within a distance: required), "units"?: "Kilometers" (default) | "Meters" | "Miles" | "Feet", "distanceField"?: text (a field for the distance, Closest and Within a distance), "operation"?: "Join one to one" (default) | "Join one to many", "keepAll"?: "Keep all" (default) | "Keep only matching", "merge"?: [{"field", "rule": "first" | "last" | "sum" | "mean" | "min" | "max" | "count" | "join", "delimiter"?}] (one to one), "prefix"?: "join_", "output"?: text}"#,
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
        C::new("gp.geolibre", "GeoLibre Tools", "Analyze › Geoprocessing", |_, _| {
            let mut status = geolibre_status();
            status["passedOver"] = json!(geolibre_passed_over());
            status["languages"] = geolibre_languages();
            Ok(status)
        }).roles(&[]).effects(crate::policy::Effects::NONE).view(),
        C::new("gp.installGeolibreLanguage", "Download GeoLibre Tool Translations", "Analyze › Geoprocessing", install_geolibre_language_cmd)
            .roles(&[])
            .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved))
            .view()
            .params(r#"{"locale": "the language's code, such as \"pt\" (one GeoLibre publishes a pack for)"}"#),
        C::new("gp.importGeolibreLanguage", "Import GeoLibre Tool Translations", "Analyze › Geoprocessing", import_geolibre_language)
            .roles(&[])
            .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved).with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"path": "a GeoLibre language pack (.json)"}"#),
        C::new("gp.uninstallGeolibreLanguage", "Remove GeoLibre Tool Translations", "Analyze › Geoprocessing", uninstall_geolibre_language)
            .roles(&[])
            .effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved))
            .view()
            .params(r#"{"locale": "the language's code"}"#),
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
            "download": rustgis_geolibre::runner::download_url(),
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

/// Why a GeoLibre runner found or chosen isn't used, its version being outside the range this
/// RustGIS works with (what, and how to fix it), also while another runner is in use: `gp.geolibre`
/// reports it as `passedOver`. Kept out of [`geolibre_status`], which a run's approval compares,
/// so a refused Locate… doesn't change it. `None` in the browser and builds without the suite.
pub fn geolibre_passed_over() -> Option<String> {
    #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
    {
        rustgis_geolibre::runner::passed_over()
    }
    #[cfg(not(all(feature = "geolibre", not(target_arch = "wasm32"))))]
    None
}

/// The GeoLibre tool translations (#2224): the languages GeoLibre publishes a pack for, the ones
/// kept in the data folder, and the one in use.
fn geolibre_languages() -> Value {
    #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
    {
        rustgis_geolibre::langpack::check_kept();
        let errors: serde_json::Map<String, Value> =
            rustgis_geolibre::langpack::load_errors().into_iter().map(|(locale, e)| (locale, Value::String(e))).collect();
        json!({"available": rustgis_geolibre::langpack::LOCALES, "installed": rustgis_geolibre::langpack::kept(),
            "active": rustgis_geolibre::langpack::active_locale(), "errors": errors})
    }
    #[cfg(not(all(feature = "geolibre", not(target_arch = "wasm32"))))]
    {
        json!({"available": [], "installed": [], "active": null, "errors": {}})
    }
}

/// Whether GeoLibre publishes tool translations for `locale`, and this build can download them.
pub fn geolibre_language_available(locale: &str) -> bool {
    #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
    {
        rustgis_geolibre::langpack::url(locale).is_some()
    }
    #[cfg(not(all(feature = "geolibre", not(target_arch = "wasm32"))))]
    {
        let _ = locale;
        false
    }
}

/// Whether tool translations for `locale` are in use (kept in the data folder, read the first
/// time this is asked, then from memory).
pub fn geolibre_language_in_use(locale: &str) -> bool {
    #[cfg(feature = "geolibre")]
    {
        rustgis_geolibre::langpack::in_use(locale)
    }
    #[cfg(not(feature = "geolibre"))]
    {
        let _ = locale;
        false
    }
}

/// `english`, a GeoLibre tool's text (its name, summary, a parameter's label or help, a toolbox
/// name), in `locale`, from the translations kept for it; the interface's catalogue falls back to
/// it as to a plugin's `i18n`. `None` without them.
pub fn geolibre_translate(locale: &str, english: &str) -> Option<String> {
    #[cfg(feature = "geolibre")]
    {
        rustgis_geolibre::langpack::translate(locale, english)
    }
    #[cfg(not(feature = "geolibre"))]
    {
        let _ = (locale, english);
        None
    }
}

/// Why the tool translations kept for `locale` can't be used (a damaged file, another format
/// version), once they were tried: the Tools pane shows it beside **Translate tools**.
pub fn geolibre_language_error(locale: &str) -> Option<String> {
    #[cfg(feature = "geolibre")]
    {
        rustgis_geolibre::langpack::load_error(locale)
    }
    #[cfg(not(feature = "geolibre"))]
    {
        let _ = locale;
        None
    }
}

/// Changes whenever [`geolibre_translate`] may answer differently (translations installed or
/// removed, the tools reloaded), so a cache of its answers starts over.
pub fn geolibre_language_revision() -> u64 {
    #[cfg(feature = "geolibre")]
    {
        rustgis_geolibre::langpack::revision()
    }
    #[cfg(not(feature = "geolibre"))]
    0
}

/// Downloads GeoLibre's tool translations for `locale` (about 2 MB), keeps them and puts them in
/// use; returns how many of the listed tools they name. Blocks on one request: the app calls it from a
/// background thread. Native only.
pub fn install_geolibre_language(locale: &str) -> Result<usize, String> {
    #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
    {
        rustgis_geolibre::langpack::download(locale)
    }
    #[cfg(not(all(feature = "geolibre", not(target_arch = "wasm32"))))]
    {
        let _ = locale;
        Err("this build can't download GeoLibre tool translations".into())
    }
}

/// `gp.installGeolibreLanguage`. It runs where it is called, without Cancel, because its work
/// doesn't grow with anything the user has: one request for one file, read up to 5 MB and given
/// up after 60 s, then one parse, the same for every project. The Tools pane's button, the way
/// the app downloads a pack, runs it on a thread of its own; called from a script, the command line
/// or Python, it waits for that one request like `gp.installGeolibre` waits for the runner's.
fn install_geolibre_language_cmd(_s: &mut Session, p: &Value) -> CmdResult {
    let locale = ps(p, "locale").map(str::trim).filter(|l| !l.is_empty()).ok_or("give the language's \"locale\", such as \"pt\"")?;
    let tools = install_geolibre_language(locale)?;
    Ok(json!({"locale": locale, "tools": tools}))
}

fn import_geolibre_language(_s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
    {
        let path = ps(p, "path").map(str::trim).filter(|p| !p.is_empty()).ok_or("give the pack's \"path\"")?;
        let (locale, tools) = rustgis_geolibre::langpack::import(std::path::Path::new(path))?;
        Ok(json!({"locale": locale, "tools": tools}))
    }
    #[cfg(not(all(feature = "geolibre", not(target_arch = "wasm32"))))]
    {
        let _ = p;
        Err("this build can't import GeoLibre tool translations".into())
    }
}

fn uninstall_geolibre_language(_s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(all(feature = "geolibre", not(target_arch = "wasm32")))]
    {
        let locale = ps(p, "locale").map(str::trim).filter(|l| !l.is_empty()).ok_or("give the language's \"locale\"")?;
        rustgis_geolibre::langpack::remove(locale)?;
        Ok(geolibre_languages())
    }
    #[cfg(not(all(feature = "geolibre", not(target_arch = "wasm32"))))]
    {
        let _ = p;
        Err("this build keeps no GeoLibre tool translations".into())
    }
}

/// Downloads and installs the pinned GeoLibre runner release for this platform; returns the
/// program's path. `progress` gets the bytes received and the expected total. Blocks: the app
/// calls it from a background thread, then picks the runner with `gp.setGeolibre`. Native only.
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
        let op = admitted_saying(s, tool, params)?;
        let continuation = s.tool_continuation(&op)?;
        let crate::policy::ToolOp { pin, params } = op.op();
        let spec = pin.spec();
        if pin.resolved().source() != Some(rustgis_geolibre::SOURCE) {
            return Err(format!("no GeoLibre tool '{tool}'"));
        }
        let params = params.clone();
        let job = preparing(&continuation, || pin.prepare_suite(&map_inputs(s, Some(spec), &params, Tiled::ByPath)?, &params))?;
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
            inputs: Vec::new(),
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
    // (Not the standalone tables converted for a run, [`table_key`]: no layer is updated from them.)
    full.iter().filter(|(k, _)| !k.starts_with('\0')).map(|(k, d)| (k.clone(), d.features.iter().map(|f| f.id).collect())).collect()
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
    let op = admitted_saying(s, tool, params)?;
    let continuation = s.tool_continuation(&op)?;
    let crate::policy::ToolOp { pin, params: resolved } = op.op();
    let spec = pin.spec();
    let plugin = pin.plugin().ok_or_else(|| format!("no plugin tool '{tool}'"))?;
    let (_, tid) = rustgis_plugins::split_id(spec.id).ok_or("invalid plugin tool id")?;
    let (job, full_ids, said) = preparing(&continuation, || {
        let inputs = map_inputs(s, Some(spec), resolved, Tiled::Overview)?;
        let job = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rustgis_plugins::tools::prepare_pinned(plugin.plugin(), tid, &inputs, resolved)
        }))
        .unwrap_or_else(|_| Err(format!("{} failed unexpectedly", spec.name)))?;
        Ok((job, full_ids(&inputs.loaded), inputs.messages.clone()))
    })?;
    // What reading its inputs approximated (a raster's overview read for a file read in place),
    // said now: the run's result is the plugin's own.
    if !said.is_empty() {
        s.notify(format!("{}: {}", spec.name, said.join("; ")), false);
    }
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
        inputs: Vec::new(),
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
    capture_inputs: bool,
}

#[cfg(not(target_arch = "wasm32"))]
/// What a [`ToolRun`] made, for [`tool_complete`].
pub struct ToolDone {
    inputs: Vec<rustgis_data::provenance::Fingerprint>,
    result: Result<rustgis_analysis::ToolResult, String>,
    full_ids: FullIds,
}

#[cfg(not(target_arch = "wasm32"))]
impl ToolRun {
    /// Reads the tool's inputs and runs it, blocking until it ends; `control` stops it and counts
    /// its progress (see [`rustgis_analysis::control`]).
    pub fn run(self, control: &std::sync::Arc<rustgis_analysis::control::Control>) -> ToolDone {
        let captured = self.capture_inputs.then(rustgis_data::provenance::Capture::start);
        let ToolRun { pin, continuation, snap, params, .. } = self;
        let spec = pin.spec();
        let name = spec.name;
        if let Err(e) = continuation.check() {
            return ToolDone { inputs: Vec::new(), result: Err(e), full_ids: FullIds::new() };
        }
        if control.is_cancelled() {
            return ToolDone {
                inputs: Vec::new(),
                result: Err(format!("{name} was cancelled before it started")),
                full_ids: FullIds::new(),
            };
        }
        // Reading and converting the inputs stops on Cancel too, as the tool does: the
        // conversions look at the run's control (`rustgis_data::progress`), and a conversion's
        // refusal isn't taken for a place that can't be held when it is a Cancel.
        let _probe = rustgis_analysis::control::install_progress(Some(control.clone()));
        let current = continuation.clone();
        let still = std::sync::Arc::new(move || current.check().is_ok()) as crate::render::rustgis_tiles::cache::Still;
        // A bug in reading the inputs, like one in a tool, must not take the app down.
        let mut done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::render::rustgis_tiles::cache::with_still(still, || {
                let inputs = match inputs_of(snap, Some(spec), &params, tiled_for(&pin)) {
                    Ok(inputs) => inputs,
                    // A Cancel while the inputs are read names the tool, as one while it runs does.
                    Err(_) if control.is_cancelled() => {
                        let result = Err(format!("{name} was cancelled while it read its inputs"));
                        return ToolDone { inputs: Vec::new(), result, full_ids: FullIds::new() };
                    }
                    Err(e) => return ToolDone { inputs: Vec::new(), result: Err(e), full_ids: FullIds::new() },
                };
                let mut result = pin.run(&inputs, &params, control, &|| continuation.check());
                if let Ok(r) = &mut result {
                    r.messages.extend(inputs.messages.iter().cloned());
                    r.losses.extend(inputs.losses.iter().cloned());
                }
                if let Err(e) = result.as_mut().map_or(Ok(()), |r| inputs.hold_results(r)) {
                    result = Err(e);
                }
                ToolDone { inputs: Vec::new(), result, full_ids: full_ids(&inputs.loaded) }
            })
        }))
        .unwrap_or_else(|_| ToolDone {
            inputs: Vec::new(),
            result: Err(format!("{name} failed unexpectedly")),
            full_ids: FullIds::new(),
        });
        done.inputs = captured.as_ref().map_or_else(Vec::new, rustgis_data::provenance::Capture::inputs);
        done
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
    let op = admitted_saying(s, tool, params)?;
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
    Ok(ToolRun { id, tool: spec.id, snap: Snapshot::owned(s), params: resolved, pin, continuation, capture_inputs: s.log.on() })
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
    complete(s, id, done.result, Some(done.full_ids))?;
    if let Some(run) = s.pending.iter_mut().find(|run| run.id == id) {
        run.inputs = done.inputs;
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
/// A tool's run made ahead on a copy of the session, for a model or script the app runs on a
/// thread of its own ([`super::workflows::ahead_on_copy`]): what it read and what it made, for
/// the run here to take instead of running the tool again.
pub(crate) struct ToolAhead {
    pin: crate::policy::ToolPin,
    params: Value,
    read: Snapshot<'static>,
    /// The view's extent, when the tool asked for it.
    view: Option<Option<rustgis_geom::Extent>>,
    result: Result<rustgis_analysis::ToolResult, String>,
    full_ids: FullIds,
    millis: u64,
    /// Runtime identities of successfully installed outputs on the worker, in return order.
    grid_outputs: Vec<rustgis_map::Layer>,
}

#[cfg(not(target_arch = "wasm32"))]
/// A script's prepared operations in execution order. Table edits retain their output rows,
/// so following tools see the same storage when the live script takes the edits.
pub(crate) enum RanAhead {
    Tool(Box<ToolAhead>),
    Table(Box<super::edit::ScriptTableEdit>),
    Read(ScriptRead),
}

#[cfg(not(target_arch = "wasm32"))]
/// A read-only command's prepared result, replayed by its ordinary admitted handler.
pub(crate) struct ScriptRead {
    command: &'static str,
    params: Value,
    value: Box<dyn std::any::Any + Send>,
}

#[cfg(not(target_arch = "wasm32"))]
/// Keeps a typed worker result in prefix order for the live command's authorized replay.
pub(crate) fn record_read<T: Send + 'static>(s: &mut Session, command: &'static str, params: &Value, value: T) {
    if let Some(Ahead::Recording(ran)) = &mut s.tools_ahead {
        ran.push(RanAhead::Read(ScriptRead { command, params: params.clone(), value: Box::new(value) }));
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// A replayed read must consume its recorded result, never recompute after a mismatch.
/// Past the recorded prefix, return no result so the caller's normal execution guard applies.
pub(crate) fn take_read<T: 'static>(s: &mut Session, command: &str, params: &Value) -> Result<Option<T>, String> {
    let Some(Ahead::Taking(ran, _)) = &mut s.tools_ahead else { return Ok(None) };
    let mismatch = || "the script's prepared read changed: run the script again".to_string();
    let Some(next) = ran.pop_front() else { return Ok(None) };
    let RanAhead::Read(next) = next else { return Err(mismatch()) };
    if next.command != command || next.params != *params {
        return Err(mismatch());
    }
    next.value.downcast::<T>().map(|value| Some(*value)).map_err(|_| mismatch())
}

#[cfg(not(target_arch = "wasm32"))]
/// Whether this typed replay created the worker's layer identity. Callers must still compare
/// map, row storage and metadata with the live output; this never authorizes an existing layer.
pub(super) fn replay_created_layer(s: &Session, map: u64, layer: u32, instance: u64) -> bool {
    matches!(&s.tools_ahead, Some(Ahead::Taking(_, created)) if created.contains(&(map, layer, instance)))
}

#[cfg(not(target_arch = "wasm32"))]
/// The tools of a model or script run ahead: recorded on the copy, then taken in order by the run
/// here.
pub(crate) enum Ahead {
    Recording(Vec<RanAhead>),
    Taking(std::collections::VecDeque<RanAhead>, std::collections::HashSet<(u64, u32, u64)>),
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
    /// Worker-created (map, layer id, instance) identities; live replay creates fresh Layers
    /// over their verified recorded rows. Existing layers never receive this exception.
    created_layers: std::collections::HashSet<(u64, u32, u64)>,
    files: Vec<FileAhead>,
    zones: Vec<super::imagery::ZoneMeans>,
    // The copy's initial project can outlive history while queued or waiting to replay.
    pub(crate) retained: Option<crate::undo_memory::ReadRows>,
    /// Selection effects and inner reads keep the parent admission through live replay.
    pub(crate) selection_admission: Option<(u64, std::sync::Arc<std::sync::atomic::AtomicU64>)>,
    grids: Vec<crate::image_grid::Prepared>,
}

#[cfg(not(target_arch = "wasm32"))]
/// Runs `run` taking the tools `ran` ahead: each one whose inputs are still the same as the run
/// ahead read them is taken; from the first that isn't, the tools run here. A batch takes the
/// files read ahead the same way. Restored on every path, a panic included (raised again), so no
/// later run takes them.
pub(crate) fn taking<T>(s: &mut Session, ran: RanAheads, run: impl FnOnce(&mut Session) -> T) -> T {
    let _retained = ran.retained;
    let previous_admission = s.selection_admission.clone();
    if ran.selection_admission.is_some() {
        s.selection_admission = ran.selection_admission;
    }
    s.tools_ahead = Some(Ahead::Taking(ran.tools.into(), ran.created_layers));
    s.files_ahead = Some(FilesAhead::Taking(ran.files.into()));
    s.grids_ahead = Some(crate::image_grid::Ahead::taking(ran.grids));
    s.zones_ahead = Some(super::imagery::ZonesAhead::Taking(ran.zones.into()));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(s)));
    s.tools_ahead = None;
    s.files_ahead = None;
    s.zones_ahead = None;
    s.selection_admission = previous_admission;
    s.grids_ahead = None;
    r.unwrap_or_else(|p| std::panic::resume_unwind(p))
}

#[cfg(not(target_arch = "wasm32"))]
/// What a copy of the session recorded ([`Ahead::Recording`], [`FilesAhead::Recording`]), taken.
pub(crate) fn ran_ahead(s: &mut Session) -> RanAheads {
    let tools = match s.tools_ahead.take() {
        Some(Ahead::Recording(ran)) => ran,
        _ => Vec::new(),
    };
    // Outputs were captured when installed, before a following step could remove them.
    // Their worker identities must survive until live replay reaches those steps too.
    let created_layers = tools
        .iter()
        .filter_map(|ran| if let RanAhead::Tool(tool) = ran { Some(tool) } else { None })
        .flat_map(|tool| tool.grid_outputs.iter().map(|layer| (tool.read.map.instance_id(), layer.id, layer.instance_id())))
        .collect();
    let files = match s.files_ahead.take() {
        Some(FilesAhead::Recording(read)) => read,
        _ => Vec::new(),
    };
    let zones = match s.zones_ahead.take() {
        Some(super::imagery::ZonesAhead::Recording(read)) => read,
        _ => Vec::new(),
    };
    let grids = match s.grids_ahead.take() {
        Some(crate::image_grid::Ahead::Recording(read)) => read,
        _ => Vec::new(),
    };
    RanAheads { tools, created_layers, files, zones, grids, retained: None, selection_admission: s.selection_admission.clone() }
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
fn take_ahead(s: &mut Session, pin: &crate::policy::ToolPin, params: &Value) -> Option<Box<ToolAhead>> {
    let Some(Ahead::Taking(ran, _)) = &mut s.tools_ahead else {
        crate::image_grid::invalidate_script_grids(s);
        return None;
    };
    let Some(next) = ran.pop_front() else {
        s.tools_ahead = None;
        crate::image_grid::invalidate_script_grids(s);
        return None;
    };
    let RanAhead::Tool(next) = next else {
        s.tools_ahead = None;
        crate::image_grid::invalidate_script_grids(s);
        return None;
    };
    let now = Snapshot::of(s);
    let view = next.view.is_none_or(|v| v == view_extent(&now));
    if next.pin == *pin && next.params == *params && view && same_read(&next.read, &now) {
        #[cfg(test)]
        {
            s.tools_taken += 1;
        }
        return Some(next);
    }
    s.tools_ahead = None;
    crate::image_grid::invalidate_script_grids(s);
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
/// rows of the current window, and a selection leaves out the features the map doesn't draw at its
/// scale (the layer's definition query, time window and display filters; the Select tools read the
/// whole layer). An update from a selection is still
/// refused: it would replace the complete source with just those selected features.
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
        && let Output::Selection { layer, rows } = &mut r.output
        && let Some(l) = s.map().layer_by_name(layer)
    {
        let drawn = l.drawn_rows(s.scale());
        let before = rows.len();
        rows.retain(|row| drawn.get(*row).is_none_or(|d| *d));
        let hidden = before - rows.len();
        if hidden > 0 {
            r.messages
                .push(format!("{hidden} of the features selected in {layer} aren't drawn (its definition query, time window or display filters hide them), and aren't selected"));
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
    rustgis_data::provenance::record(r.inputs.clone());
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
    /// The project's coordinate system ([`crate::project::Project::coordinate_system`]): the one
    /// outputs made in longitude/latitude are held in ([`MapInputs::hold_results`]).
    project: Option<Crs>,
}

impl<'a> Snapshot<'a> {
    /// The session itself, for a run on its thread.
    fn of(s: &'a Session) -> Snapshot<'a> {
        use std::borrow::Cow::Borrowed;
        Snapshot {
            map: Borrowed(s.map()),
            tables: Borrowed(&s.project.tables),
            env: Borrowed(&s.project.gp_env),
            view_size: s.view_size,
            project: s.project.coordinate_system,
        }
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
            project: s.project.coordinate_system,
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
    /// Streamed layers the tool reads, loaded whole, and copies of layers in another system than
    /// the one it works in, converted ([`work_inputs`]), by lowercase name.
    loaded: std::collections::HashMap<String, Dataset>,
    /// The layers read whole for the run (streamed ones) with features read without their
    /// geometry, a place of theirs having none in longitude/latitude: an update of one is refused,
    /// as the map holds only its window, where such a feature's stored geometry may not be.
    left_out_whole: std::collections::HashSet<String>,
    /// The system the tool works in ([`work_system`]): the one its inputs are given in and its
    /// outputs are made in.
    work: Crs,
    /// The layers (by lowercase name) it reads only the attributes of, read as they are
    /// ([`attributes_only`]); none when it works in longitude/latitude.
    unread: std::collections::HashSet<String>,
    /// Import warnings from local file inputs, kept in the tool result and history.
    messages: Vec<String>,
    losses: Vec<LossMessage>,
    /// Raster layers read in place from a file on this computer, read again from it for the run
    /// ([`tiled_inputs`]), by lowercase name.
    rasters: std::collections::HashMap<String, std::sync::Arc<rustgis_data::raster::Raster>>,
    /// [`MapInputs::rasters`] counted as held while the run holds them (kept only to be dropped).
    _held: Vec<std::sync::Arc<rustgis_data::memory::Holding>>,
    /// The flat map's projection (its name, the projection and its unit in metres), built the
    /// first time a tool asks for it (Tissot's indicatrix does), once for the run; `None` on the
    /// globe.
    flat: std::cell::OnceCell<Option<(String, rustgis_map::projection::Flat, f64)>>,
    /// The view's longitude/latitude extent, worked out the first time a tool asks for it (it
    /// inverse-projects the view, which most tools never need).
    view: std::cell::OnceCell<Option<rustgis_geom::Extent>>,
}

impl MapInputs<'_> {
    /// Marks the layers a tool made as held in the system it worked in ([`MapInputs::work`]),
    /// where their coordinates are, so they aren't taken for longitude/latitude: a layer it made
    /// from scratch starts in longitude/latitude ([`Dataset::held_in`]). A tool that worked in
    /// longitude/latitude leaves them there, unless the project has a coordinate system of its own
    /// (#2032 C3a): they are converted into it and hold it as theirs (`crs`), as a layer drawn in
    /// the project is; one with a place it can't hold stays in longitude/latitude, said. Tables
    /// have no coordinates to hold.
    fn hold_results(&self, result: &mut rustgis_analysis::ToolResult) -> Result<(), String> {
        // An update writes back into the window the map holds of a streamed layer, which may not
        // have the stored geometry of a feature read without it: refused, rather than lost.
        if let Output::Update { layer, .. } = &result.output
            && self.left_out_whole.contains(&layer.to_ascii_lowercase())
        {
            return Err(format!(
                "{layer} is read in place and has features with a place that can't be given in longitude/latitude, which an update would lose: Load All Features (layer.loadAll) before a tool updates it"
            ));
        }
        // A geocentric system holds no map coordinates: outputs stay in longitude/latitude then.
        let project = self.snap.project.filter(|c| *c != Crs::LonLat && !c.is_geocentric());
        if self.work == Crs::LonLat && project.is_none() {
            return Ok(());
        }
        // The layers kept in longitude/latitude, and why the first was.
        let (mut kept, mut why) = (0usize, None);
        let mut hold = |ds: &mut Dataset| -> Result<(), String> {
            if ds.geometry_type == rustgis_data::GeometryType::None {
                return Ok(());
            }
            match (self.work, project) {
                // A tool that holds what it made in a system of its own (XY Table To Point, in the
                // one its coordinates are given in) keeps it there.
                (Crs::LonLat, _) if ds.held_in != Crs::LonLat => {}
                (Crs::LonLat, Some(p)) => match ds.hold_as(p, Some(p)) {
                    // A Cancel stops the run, which adds nothing.
                    Err(e) if matches!(rustgis_data::DataError::of_conversion(e.clone()), rustgis_data::DataError::Cancelled) => {
                        return Err(e);
                    }
                    Err(e) => {
                        kept += 1;
                        // Its refusal starts with the layer's name, named once here.
                        let reason = e.strip_prefix(&format!("{}: ", ds.name)).unwrap_or(&e).to_string();
                        why.get_or_insert((ds.name.clone(), reason));
                    }
                    Ok(()) => {}
                },
                (work, _) => ds.held_in = work,
            }
            Ok(())
        };
        match &mut result.output {
            Output::Layer(ds) => hold(ds)?,
            // An update writes into a layer the map has, which keeps its own system.
            Output::Update { dataset: ds, .. } if self.work != Crs::LonLat && ds.geometry_type != rustgis_data::GeometryType::None => {
                ds.held_in = self.work
            }
            Output::Layers(outputs) => {
                for o in outputs {
                    if let rustgis_analysis::OutputLayer::Vector(ds) = o {
                        hold(ds)?;
                    }
                }
            }
            Output::Update { .. } | Output::Selection { .. } | Output::Table(_) | Output::File { .. } => {}
        }
        if let (Some((name, reason)), Some(p)) = (why, project) {
            let others = match kept {
                1 => String::new(),
                n => format!(" (and {} more layer{})", n - 1, if n == 2 { "" } else { "s" }),
            };
            result.messages.push(format!("{name}{others} is kept in longitude/latitude, not the project's {}: {reason}", p.label()));
        }
        Ok(())
    }

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

/// How a tool's inputs read a raster layer read in place from a file on this computer
/// ([`rustgis_map::raster::RasterLayer::tiled`]).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tiled {
    /// From the file, at the finest level the run can hold ([`tiled_inputs`]): a built-in tool's
    /// run, on its own thread or where the caller waits.
    File,
    /// The overview the layer holds, said: a plugin's run, whose inputs are read on the app's
    /// thread.
    Overview,
    /// By its path: a suite tool stages the file itself.
    ByPath,
}

/// How a run of `pin` reads a raster read in place from a file ([`Tiled`]): a plugin reads the
/// overview the layer holds, a suite tool stages the file by its path, a built-in tool reads it.
fn tiled_for(pin: &crate::policy::ToolPin) -> Tiled {
    #[cfg(feature = "geolibre")]
    if pin.resolved().source() == Some(rustgis_geolibre::SOURCE) {
        return Tiled::ByPath;
    }
    if pin.plugin().is_some() { Tiled::Overview } else { Tiled::File }
}

/// [`inputs_of`] from the session as it is; `tiled` as there.
fn map_inputs<'a>(
    s: &'a Session,
    spec: Option<&rustgis_analysis::ToolSpec>,
    params: &Value,
    tiled: Tiled,
) -> Result<MapInputs<'a>, String> {
    inputs_of(Snapshot::of(s), spec, params, tiled)
}

/// A tool's inputs from what it read of the session: the streamed layers and files it names read
/// whole, and the rows of each layer it runs on. It reads, so a background run does this on its
/// thread. `tiled` says how the raster layers it reads that are read in place from a file on this
/// computer are read ([`Tiled`]).
fn inputs_of<'a>(
    snap: Snapshot<'a>,
    spec: Option<&rustgis_analysis::ToolSpec>,
    params: &Value,
    tiled: Tiled,
) -> Result<MapInputs<'a>, String> {
    let (full, said) = streamed_inputs(&snap.map, spec, params)?;
    let (mut messages, mut losses) = (Vec::new(), Vec::new());
    for m in said {
        match m.loss {
            Some(code) => losses.push(LossMessage { code, text: m.text }),
            None => messages.push(m.text),
        }
    }
    let mut full = full;
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(spec) = spec.filter(|t| rustgis_analysis::TOOLS.iter().any(|builtin| std::ptr::eq(builtin, *t))) {
        for parameter in spec.params.iter().filter(|p| p.kind == rustgis_analysis::ParamKind::VectorLayers) {
            for name in rustgis_analysis::layer_list(params.get(parameter.name).unwrap_or(&Value::Null))? {
                if vector_file_input(spec, &snap.map, name) && !full.contains_key(name) {
                    let (dataset, report) = rustgis_data::io::read_path(std::path::Path::new(name)).map_err(|e| format!("{name}: {e}"))?;
                    for message in report.messages.prefixed(name) {
                        match message.loss {
                            Some(code) => losses.push(LossMessage { code, text: message.text }),
                            None => messages.push(message.text),
                        }
                    }
                    full.insert(name.to_string(), dataset);
                }
            }
        }
    }
    let mut left_out_whole = std::collections::HashSet::new();
    let work = work_inputs(&snap.map, &snap.tables, spec, params, &mut full, &mut messages, &mut left_out_whole)?;
    let mut rows = env_inputs(&snap.map, &full, &snap.env, params)?;
    narrow_to_queries(&snap.map, spec, params, &full, &mut rows, &mut messages)?;
    let unread = if work == Crs::LonLat { std::collections::HashSet::new() } else { attributes_only(spec, params) };
    #[cfg(not(target_arch = "wasm32"))]
    let mut held = Vec::new();
    #[cfg(not(target_arch = "wasm32"))]
    let rasters = tiled_inputs(&snap.map, spec, params, tiled, &mut messages, &mut held)?;
    #[cfg(target_arch = "wasm32")]
    let (rasters, held) = {
        let _ = tiled;
        (std::collections::HashMap::new(), Vec::new())
    };
    Ok(MapInputs {
        snap,
        rows,
        loaded: full,
        work,
        left_out_whole,
        unread,
        messages,
        losses,
        rasters,
        _held: held,
        flat: std::cell::OnceCell::new(),
        view: std::cell::OnceCell::new(),
    })
}

/// The raster layers `spec`'s parameters name that are read in place from a file on this
/// computer ([`rustgis_map::raster::RasterLayer::tiled`]), each read again from the file for the
/// run, by lowercase name: such a layer holds only the small overview the map draws from zoomed
/// out, and a tool reads the finest level of the file it can hold, the full image when it fits
/// (#2761). Read on the run's thread; one read from an overview is said. The GeoTIFF reader looks
/// at Cancel between its chunks through the progress probe, which a run from [`tool_start`]
/// installs before reading its inputs; a run made where the caller waits stops there only when
/// the caller installed one, else after the read.
/// Read only for [`Tiled::File`]: for [`Tiled::Overview`] each such layer's held overview is said,
/// and for [`Tiled::ByPath`] nothing is read or said, but a file no longer there is refused.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn tiled_inputs(
    map: &rustgis_map::Map,
    spec: Option<&rustgis_analysis::ToolSpec>,
    params: &Value,
    tiled: Tiled,
    messages: &mut Vec<String>,
    held: &mut Vec<std::sync::Arc<rustgis_data::memory::Holding>>,
) -> Result<std::collections::HashMap<String, std::sync::Arc<rustgis_data::raster::Raster>>, String> {
    use rustgis_analysis::ParamKind;
    let mut read = std::collections::HashMap::new();
    // Raster to PMTiles draws its input as the map does, in place at each zoom's level, and
    // reads no pixels through the run's inputs.
    let Some(spec) = spec.filter(|t| t.id != "rasterToPmtiles") else { return Ok(read) };
    let mut seen = std::collections::HashSet::new();
    let (mut overviews, mut held_overviews, mut rounded) = (Vec::new(), Vec::new(), Vec::new());
    for parameter in spec.params {
        let value = params.get(parameter.name).unwrap_or(&Value::Null);
        let names: Vec<&str> = match parameter.kind {
            // A name, or (as a suite tool may take it) an array of names.
            ParamKind::RasterLayer => match value {
                Value::Array(v) => v.iter().filter_map(Value::as_str).collect(),
                v => v.as_str().into_iter().collect(),
            },
            ParamKind::RasterLayers => rustgis_analysis::layer_list(value).unwrap_or_default(),
            // A suite tool stages any input layer or path a parameter names, not what it writes.
            ParamKind::Layer
            | ParamKind::PolygonLayer
            | ParamKind::PointLayer
            | ParamKind::VectorLayer
            | ParamKind::VectorLayers
            | ParamKind::Path
                if tiled == Tiled::ByPath =>
            {
                match value {
                    Value::String(t) => t.split(';').map(str::trim).collect(),
                    Value::Array(v) => v.iter().filter_map(Value::as_str).collect(),
                    _ => continue,
                }
            }
            _ => continue,
        };
        for name in names {
            let Some(l) = map.layer_by_name(name) else { continue };
            let key = l.name.to_ascii_lowercase();
            let Some(path) = l.raster.as_ref().filter(|rl| rl.tiled).and_then(|rl| rl.path.as_deref()) else { continue };
            if !seen.insert(key.clone()) {
                continue;
            }
            // A suite tool stages the file by its path: one no longer there would have it stage
            // the overview the layer holds instead, so it is refused.
            if tiled == Tiled::ByPath {
                if !std::path::Path::new(path).is_file() {
                    return Err(format!("{}: its file {path} is no longer there: Reload Data or Repair Data Source first", l.name));
                }
                continue;
            }
            if tiled == Tiled::Overview {
                if let Some(held) = l.raster.as_ref().and_then(|rl| rl.data.as_ref())
                    && let Some((fw, fh)) = held.overview
                {
                    held_overviews
                        .push(format!("{} at the {} × {} overview of the {fw} × {fh} raster it holds", l.name, held.width, held.height));
                }
                continue;
            }
            // A batch's read is used again only while the file is the revision it was read at.
            let revision = file_revisions(std::path::Path::new(path));
            let kept = BATCH_READS.with(|b| b.borrow().as_ref().and_then(|reads| reads.get(path).cloned()));
            let (r, count) = match kept.filter(|(.., at)| revision.is_some() && *at == revision) {
                Some((r, count, _)) => (r, count),
                None => {
                    #[cfg(test)]
                    TILED_READS.with(|n| n.set(n.get() + 1));
                    // The last file a batch read alone: let go before another is read, so a batch
                    // over several files holds one, and doesn't hold it uncounted while the next
                    // is planned.
                    BATCH_READS.with(|b| {
                        if let Some(reads) = b.borrow_mut().as_mut() {
                            reads.clear();
                        }
                    });
                    let _turn = tiled_turn().map_err(|e| format!("{}: {e}", l.name))?;
                    let r = std::sync::Arc::new(
                        rustgis_data::raster::read_path_with(std::path::Path::new(path), None, None)
                            .map_err(|e| format!("{}: {e}", l.name))?,
                    );
                    // Counted as held from before the next run's turn, for as long as anything
                    // (this run, the batch) holds it.
                    let bytes: usize = r.bands.iter().map(|b| b.data.len().saturating_mul(4)).sum();
                    let count = std::sync::Arc::new(rustgis_data::memory::Holding::of(bytes as u64));
                    BATCH_READS.with(|b| {
                        if let Some(reads) = b.borrow_mut().as_mut() {
                            reads.insert(path.to_string(), (r.clone(), count.clone(), revision.clone()));
                        }
                    });
                    (r, count)
                }
            };
            held.push(count);
            let n: u64 = r.rounded.iter().sum();
            if n > 0 {
                rounded.push(format!("{} {n}", l.name));
            }
            if let Some((fw, fh)) = r.overview {
                overviews.push(format!("{} at its {} × {} overview of the {fw} × {fh} raster", l.name, r.width, r.height));
            }
            read.insert(key, r);
        }
    }
    // Said once for the run, every raster named.
    if !held_overviews.is_empty() {
        messages.push(format!(
            "read as the layer holds it, as plugin tools don't read a raster read in place from its file: {}",
            held_overviews.join("; ")
        ));
    }
    if !rounded.is_empty() {
        messages.push(format!(
            "values rounded to 32-bit floats as the file was read for the run, the tool's results built on them: {}",
            rounded.join("; ")
        ));
    }
    if !overviews.is_empty() {
        messages.push(format!("read at the finest level each can hold: {}", overviews.join("; ")));
    }
    Ok(read)
}

/// A raster file read in place for a run, its count in [`rustgis_data::memory::held`], which lasts
/// as long as anything holds it, and the revision of the file taken before it was read (a batch
/// uses it again only while the file is still that revision).
#[cfg(not(target_arch = "wasm32"))]
type CountedRead = (
    std::sync::Arc<rustgis_data::raster::Raster>,
    std::sync::Arc<rustgis_data::memory::Holding>,
    Option<[Option<rustgis_data::raster::FileRevision>; 3]>,
);

/// The revisions of a raster file and of the sidecars its whole-file read applies (its
/// `.aux.xml`, a world file): a batch's read of it is used again only while none has changed, or
/// been added or removed. `None` when the file's own can't be read.
#[cfg(not(target_arch = "wasm32"))]
fn file_revisions(path: &std::path::Path) -> Option<[Option<rustgis_data::raster::FileRevision>; 3]> {
    use rustgis_data::raster::{FileRevision, world};
    let own = FileRevision::of(path)?;
    let mut aux = path.as_os_str().to_owned();
    aux.push(".aux.xml");
    let world = world::beside(path).and_then(|(p, _)| FileRevision::of(&p));
    Some([Some(own), FileRevision::of(std::path::Path::new(&aux)), world])
}

/// Taken by each read of a raster file read in place for a run ([`tiled_inputs`]): such reads
/// run one at a time across runs, so each plans its size with the ones before it held.
#[cfg(not(target_arch = "wasm32"))]
static TILED_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// [`TILED_TURN`], waited for while another run reads, looking at the run's Cancel every 50 ms
/// (through the progress probe it installed).
#[cfg(not(target_arch = "wasm32"))]
fn tiled_turn() -> rustgis_data::Result<std::sync::MutexGuard<'static, ()>> {
    loop {
        match TILED_TURN.try_lock() {
            Ok(turn) => return Ok(turn),
            Err(std::sync::TryLockError::Poisoned(e)) => return Ok(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                rustgis_data::progress::check()?;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    /// The raster file read in place that a batch's items last read ([`tiled_inputs`]), by path,
    /// kept while the batch runs on this thread ([`BatchReads`]): items reading the same file read
    /// it once, not once an item, and a batch over several files holds one at a time. `None`
    /// outside a batch, where nothing is kept past the run.
    static BATCH_READS: std::cell::RefCell<Option<std::collections::HashMap<String, CountedRead>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, not(target_arch = "wasm32")))]
thread_local! {
    /// How many raster files read in place [`tiled_inputs`] has read from their file on this
    /// thread, under test.
    pub(crate) static TILED_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Whether a batch's reads are kept on this thread now ([`BATCH_READS`]), under test.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn batch_reads_kept() -> bool {
    BATCH_READS.with(|b| b.borrow().is_some())
}

/// Keeps [`BATCH_READS`] while it lives: the outermost batch on this thread starts it and lets
/// it go, so a file it read is held only as long as the batch runs.
#[cfg(not(target_arch = "wasm32"))]
struct BatchReads(bool);

#[cfg(not(target_arch = "wasm32"))]
impl BatchReads {
    fn start() -> BatchReads {
        BatchReads(BATCH_READS.with(|b| {
            let mut b = b.borrow_mut();
            let outer = b.is_none();
            if outer {
                *b = Some(std::collections::HashMap::new());
            }
            outer
        }))
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for BatchReads {
    fn drop(&mut self) {
        if self.0 {
            BATCH_READS.with(|b| *b.borrow_mut() = None);
        }
    }
}

/// Narrows the rows a tool reads of each layer to the features its definition query keeps, as the
/// map draws them (#1995): a selection is cut to its drawn features, and a layer without one (or
/// with only the environment's extent) reads the kept features instead of every one. A layer whose
/// query hides every feature the tool would read is refused, as an empty list of rows means all of
/// them. A layer a tool reads whole whatever its selection ([`reads_whole`]) is read whole, and a
/// notice says how many features its query hides. The query is evaluated in one pass over the
/// layer's features, without a check for Cancel, as the map's own filter is.
fn narrow_to_queries(
    map: &rustgis_map::Map,
    spec: Option<&rustgis_analysis::ToolSpec>,
    params: &Value,
    full: &std::collections::HashMap<String, Dataset>,
    rows: &mut std::collections::HashMap<String, Vec<usize>>,
    messages: &mut Vec<String>,
) -> Result<(), String> {
    let whole: Vec<&str> = spec
        .iter()
        .flat_map(|t| t.params.iter().filter(|p| reads_whole(t.id, p.name)).filter_map(|p| params.get(p.name)?.as_str()))
        .collect();
    let mut named = layer_names(spec, params);
    // The layer a Select tool selects in is read whole, and its result is cut to the drawn
    // features afterwards ([`fit_to_windows`]): not narrowed here, and never refused (it selects
    // nothing then).
    let selected_in: Vec<&str> = spec
        .iter()
        .flat_map(|t| t.params.iter().filter(|p| selects_in(t.id, p.name)).filter_map(|p| params.get(p.name)?.as_str()))
        .collect();
    for w in whole.iter().chain(&selected_in) {
        if let Some(i) = named.iter().position(|n| n.eq_ignore_ascii_case(w)) {
            named.remove(i);
        }
    }
    let mut seen = std::collections::HashSet::new();
    let mut query_errors = Vec::new();
    for name in named {
        let Some(layer) = map.layer_by_name(name) else { continue };
        let key = layer.name.to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            continue;
        }
        let dataset = full.get(&key).unwrap_or(&layer.dataset);
        let resolved = layer.resolve_rows(dataset, rustgis_map::rows::Policy::SelectionAndQuery, rows.get(&key).map(Vec::as_slice))?;
        if let Some(error) = resolved.query_error {
            query_errors.push(format!("{}: {error}", layer.name));
        }
        if let rustgis_map::rows::Rows::Subset(narrowed) = resolved.rows {
            if narrowed.is_empty() && !dataset.features.is_empty() {
                return Err(format!("{}: its definition query hides every feature the tool would read", layer.name));
            }
            rows.insert(key, narrowed);
        }
    }
    if !query_errors.is_empty() {
        let count = query_errors.len();
        let (layers, queries) = if count == 1 { ("layer", "that query") } else { ("layers", "those queries") };
        messages
            .push(format!("{count} {layers} with an unreadable definition query; reading without {queries}: {}", query_errors.join("; ")));
    }
    let name = spec.map_or("", |t| t.name);
    let mut told = std::collections::HashSet::new();
    for layer in whole.iter().filter_map(|n| map.layer_by_name(n)).filter(|l| told.insert(l.name.to_ascii_lowercase())) {
        let Some(keep) = layer.query_rows(full.get(&layer.name.to_ascii_lowercase()).unwrap_or(&layer.dataset)) else { continue };
        let hidden = keep.iter().filter(|k| !**k).count();
        if hidden > 0 {
            messages.push(format!(
                "{name}: reads all {} features of {}, including the {hidden} its definition query hides",
                keep.len(),
                layer.name
            ));
        }
    }
    Ok(())
}

/// The layer parameter a Select tool replaces the selection of: read whole, as the tool reads it.
fn selects_in(tool: &str, param: &str) -> bool {
    matches!((tool, param), ("selectByAttributes" | "selectByLocation", "input"))
}

/// The layer parameters a tool reads whole whatever the layer's selection or definition query:
/// the one it updates in place, and the polygons it counts every point against (see the
/// analysis guide). The Select tools read the whole layer too, and their selection is cut to the
/// drawn features instead ([`selects_in`], [`fit_to_windows`]).
fn reads_whole(tool: &str, param: &str) -> bool {
    matches!((tool, param), ("calculateGeometry", "input") | ("countPoints", "polygons"))
}

/// Complete streamed inputs without selections, plus selected PMTiles inputs: that exporter
/// requires the complete source. Their selected window rows are remapped in `map_inputs`. With
/// them, what each read couldn't read as it is (rows left out, dates read as null or rounded), for
/// the tool's messages.
fn streamed_inputs(
    map: &rustgis_map::Map,
    spec: Option<&rustgis_analysis::ToolSpec>,
    params: &Value,
) -> Result<(std::collections::HashMap<String, Dataset>, rustgis_data::loss::Notes), String> {
    let mut full = std::collections::HashMap::new();
    let mut said = rustgis_data::loss::Notes::new();
    for n in layer_names(spec, params) {
        let key = n.to_ascii_lowercase();
        if full.contains_key(&key) {
            continue;
        }
        if let Some(l) = map.layer_by_name(n).filter(|l| reads_full_source(l, spec)) {
            let (ds, unread) = l.load_all_counted().map_err(|e| format!("{}: {e}", l.name))?;
            said.extend(super::whole::Unread::of(l.name.clone(), unread).lines());
            full.insert(key, ds);
        }
    }
    Ok((full, said))
}

/// Whether a run works in `crs`: [`rustgis_analysis::works_in`].
fn works_in(crs: Crs) -> bool {
    rustgis_analysis::works_in(crs)
}

/// The system a run works in: its first input's own ([`Dataset::coord_crs`]) when the tool works
/// in it and it is one tools work in ([`works_in`]), else longitude/latitude.
fn work_system(
    map: &rustgis_map::Map,
    spec: Option<&rustgis_analysis::ToolSpec>,
    params: &Value,
    full: &std::collections::HashMap<String, Dataset>,
) -> Crs {
    use rustgis_analysis::ParamKind as K;
    let Some(spec) = spec.filter(|t| t.works_in_own_system()) else { return Crs::LonLat };
    // The tool's first layer parameter, under its name or one of the names it also accepts
    // (Join Field's `input`), taken as the tool takes it: the first that isn't blank. A table
    // given by its number (read unconverted, [`Inputs::table`]) or by name leaves the run in
    // longitude/latitude, as before: only a map layer decides the system.
    let Some(kind) =
        spec.params.iter().find(|p| matches!(p.kind, K::Layer | K::PolygonLayer | K::PointLayer | K::VectorLayer | K::VectorLayers))
    else {
        return Crs::LonLat;
    };
    let aliases = rustgis_analysis::param_aliases(spec, kind.name);
    let keys: &[&str] = if aliases.is_empty() { std::slice::from_ref(&kind.name) } else { aliases };
    let blank = |v: &&Value| v.is_null() || v.as_str().is_some_and(|s| s.trim().is_empty());
    let value = keys.iter().filter_map(|k| params.get(*k)).find(|v| !blank(v)).unwrap_or(&Value::Null);
    let first = if kind.kind == K::VectorLayers {
        rustgis_analysis::layer_list(value).ok().and_then(|v| v.first().copied())
    } else {
        value.as_str()
    };
    let Some(first) = first else { return Crs::LonLat };
    // Files read whole are kept by the name they were given, streamed layers by lowercase name.
    full.get(first)
        .or_else(|| full.get(&first.to_ascii_lowercase()))
        .or_else(|| map.layer_by_name(first).filter(|l| !l.is_raster()).map(|l| &l.dataset))
        .map(Dataset::coord_crs)
        .filter(|c| works_in(*c))
        .unwrap_or(Crs::LonLat)
}

/// The layers (by lowercase name) whose attributes alone a tool reads, given under a parameter
/// whose geometry it doesn't read ([`ToolSpec::reads_geometry_of`]) or one of the names it also
/// accepts for it: they are read as they are, in whatever system, so a place of theirs the system
/// it works in can't hold doesn't send it to longitude/latitude.
///
/// [`ToolSpec::reads_geometry_of`]: rustgis_analysis::ToolSpec::reads_geometry_of
fn attributes_only(spec: Option<&rustgis_analysis::ToolSpec>, params: &Value) -> std::collections::HashSet<String> {
    let Some(spec) = spec else { return std::collections::HashSet::new() };
    spec.params
        .iter()
        .filter(|p| !spec.reads_geometry_of(p.name))
        .flat_map(|p| {
            let aliases = rustgis_analysis::param_aliases(spec, p.name);
            if aliases.is_empty() { vec![p.name] } else { aliases.to_vec() }
        })
        .filter_map(|k| params.get(k))
        // A standalone table given by its number, by the key it would be converted under.
        .filter_map(|v| v.as_str().map(str::to_ascii_lowercase).or_else(|| v.as_u64().and_then(|n| u32::try_from(n).ok()).map(table_key)))
        .collect()
}

/// The layers a tool reads, in the system it works in ([`work_system`], [`Inputs::dataset`]):
/// its first input's own for a tool that works in it, every other input converted into it; else
/// longitude/latitude. An input with a place that system can't hold has the run work in
/// longitude/latitude instead, and a message says so (the conversions made for the first attempt
/// are dropped then, and the inputs read again in longitude/latitude, a feature with a place
/// that has none there either read without its geometry, [`inputs_in`]). Returns the system.
fn work_inputs(
    map: &rustgis_map::Map,
    tables: &[crate::project::Table],
    spec: Option<&rustgis_analysis::ToolSpec>,
    params: &Value,
    full: &mut std::collections::HashMap<String, Dataset>,
    messages: &mut Vec<String>,
    left_out_whole: &mut std::collections::HashSet<String>,
) -> Result<Crs, String> {
    let work = work_system(map, spec, params, full);
    if work != Crs::LonLat {
        match inputs_in(map, tables, spec, params, full, work, left_out_whole) {
            Ok((converted, said)) => {
                full.extend(converted);
                messages.extend(said);
                return Ok(work);
            }
            Err(e) => {
                // A Cancel isn't a place that can't be held.
                rustgis_data::progress::check().map_err(|e| e.to_string())?;
                let name = spec.map_or("The tool", |t| t.name);
                messages.push(format!("{name} worked in longitude/latitude, not {}: {e}", work.label()));
            }
        }
    }
    let (converted, said) = inputs_in(map, tables, spec, params, full, Crs::LonLat, left_out_whole)?;
    full.extend(converted);
    messages.extend(said);
    Ok(Crs::LonLat)
}

/// Copies of the layers a tool reads converted into `work`, by lowercase name: those read whole
/// (streamed layers, files) and each other layer or standalone table with geometry it names whose
/// coordinates are in another system. Nothing is copied for a layer already in `work`. In
/// longitude/latitude, a feature with a place that can't be given there is read without its
/// geometry, and one notice says how many of each layer were.
fn inputs_in(
    map: &rustgis_map::Map,
    tables: &[crate::project::Table],
    spec: Option<&rustgis_analysis::ToolSpec>,
    params: &Value,
    full: &std::collections::HashMap<String, Dataset>,
    work: Crs,
    left_out_whole: &mut std::collections::HashSet<String>,
) -> Result<(std::collections::HashMap<String, Dataset>, Vec<String>), String> {
    let mut converted = std::collections::HashMap::new();
    // The converted inputs with edges that bend ([`bent_edges`]): name, how many, how far.
    let mut bent: Vec<(String, usize, f64)> = Vec::new();
    let reads_edges = work != Crs::LonLat && spec.is_some_and(rustgis_analysis::ToolSpec::reads_edges);
    let mut say = |original: &Dataset, c: &Dataset| -> Result<(), String> {
        if reads_edges {
            let (n, far) = bent_edges(original, c, work)?;
            if n > 0 {
                bent.push((original.name.clone(), n, far));
            }
        }
        Ok(())
    };
    // The inputs with features read without their geometry, a place of theirs having none in
    // longitude/latitude: name, how many. In `work`, another system, such a place sends the run
    // to longitude/latitude instead ([`work_inputs`]), where the feature is read without it,
    // rather than the run refused.
    let mut unplaced: Vec<(String, usize)> = Vec::new();
    let mut convert = |ds: &'_ Dataset| -> Result<Option<Dataset>, String> {
        if work != Crs::LonLat {
            return Ok(match ds.in_crs(work)? {
                std::borrow::Cow::Owned(c) => Some(c),
                std::borrow::Cow::Borrowed(_) => None,
            });
        }
        let (c, n) = ds.in_lonlat_leaving_out()?;
        if n > 0 {
            unplaced.push((ds.name.clone(), n));
        }
        Ok(match c {
            std::borrow::Cow::Owned(c) => Some(c),
            std::borrow::Cow::Borrowed(_) => None,
        })
    };
    // Read only for its attributes in a run in its first input's system: as it is. (The first
    // input, if it is named there too, is in that system already.)
    let unread = if work == Crs::LonLat { std::collections::HashSet::new() } else { attributes_only(spec, params) };
    for (key, ds) in full {
        if unread.contains(&key.to_ascii_lowercase()) {
            continue;
        }
        if let Some(c) = convert(ds)? {
            say(ds, &c)?;
            converted.insert(key.clone(), c);
        }
    }
    // Every text the parameters hold that names a layer, not only layer parameters: a tool may
    // name one elsewhere, and a layer it reads must be in the system it works in. Not the name
    // its output is given, which it doesn't read.
    fn texts<'v>(v: &'v Value, out: &mut Vec<&'v str>) {
        match v {
            Value::String(s) => out.push(s),
            Value::Array(a) => a.iter().for_each(|x| texts(x, out)),
            Value::Object(o) => o.values().for_each(|x| texts(x, out)),
            _ => {}
        }
    }
    let mut names = layer_names(spec, params);
    match params {
        Value::Object(o) => o.iter().filter(|(k, _)| k.as_str() != "output").for_each(|(_, v)| texts(v, &mut names)),
        other => texts(other, &mut names),
    }
    for n in names {
        let key = n.to_ascii_lowercase();
        if full.contains_key(&key) || converted.contains_key(&key) || unread.contains(&key) {
            continue;
        }
        // A layer, or a standalone table with geometry; one without reads alike in any system.
        if let Some(ds) = project_input(map, tables, n, false)
            .filter(|i| !matches!(i, ProjectInput::Layer(l) if l.is_raster()))
            .map(|i| i.dataset())
            .filter(|d| d.geometry_type != rustgis_data::GeometryType::None)
            && let Some(c) = convert(ds)?
        {
            say(ds, &c)?;
            converted.insert(key, c);
        }
    }
    // A standalone table given by its number ([`Inputs::table`]), as one given by name: under a
    // layer parameter (or a name it also accepts), the only place a number is a table's; not one
    // the tool reads only the attributes of, in a run in its first input's system.
    let mut ids = Vec::new();
    if let Some(spec) = spec {
        use rustgis_analysis::ParamKind as K;
        let layers = spec.params.iter().filter(|p| {
            matches!(p.kind, K::Layer | K::PolygonLayer | K::PointLayer | K::VectorLayer | K::VectorLayers)
                && (work == Crs::LonLat || spec.reads_geometry_of(p.name))
        });
        for p in layers {
            let aliases = rustgis_analysis::param_aliases(spec, p.name);
            let keys: &[&str] = if aliases.is_empty() { std::slice::from_ref(&p.name) } else { aliases };
            for v in keys.iter().filter_map(|k| params.get(*k)) {
                let one = std::slice::from_ref(v);
                ids.extend(v.as_array().map_or(one, Vec::as_slice).iter().filter_map(Value::as_u64));
            }
        }
    }
    for t in tables.iter().filter(|t| ids.contains(&u64::from(t.id))) {
        if t.dataset.geometry_type == rustgis_data::GeometryType::None {
            continue;
        }
        if let Some(c) = convert(&t.dataset)? {
            say(&t.dataset, &c)?;
            converted.insert(table_key(t.id), c);
        }
    }
    // One notice for them all, naming each layer.
    let mut said = Vec::new();
    // The layers read whole (streamed ones, files) with features read without their geometry.
    for (key, ds) in full {
        if unplaced.iter().any(|(name, _)| *name == ds.name) {
            left_out_whole.insert(key.to_ascii_lowercase());
        }
    }
    if !unplaced.is_empty() {
        let name = spec.map_or("The tool", |t| t.name);
        let n: usize = unplaced.iter().map(|(_, n)| n).sum();
        let layers = unplaced.iter().map(|(layer, n)| format!("'{layer}' ({n})")).collect::<Vec<_>>().join(", ");
        said.push(format!(
            "{name}: {n} feature(s) of {layers} have a place that can't be given in longitude/latitude, so the tool reads them without their geometry"
        ));
    }
    if !bent.is_empty() {
        let name = spec.map_or("The tool", |t| t.name);
        let n: usize = bent.iter().map(|(_, n, _)| n).sum();
        let far = bent.iter().map(|(.., far)| *far).fold(0.0, f64::max);
        let layers = bent.iter().map(|(layer, n, _)| format!("'{layer}' ({n})")).collect::<Vec<_>>().join(", ");
        said.push(format!(
            "{name}: {n} edge(s) of {layers}, converted into {}, are taken straight there, up to {far:.0} m from where a run in longitude/latitude takes them (straight on a Web Mercator map); their vertices are where they were",
            work.label()
        ));
    }
    Ok((converted, said))
}

/// The key a standalone table given by its number is converted under among a run's inputs
/// ([`inputs_in`]): one no layer's name can be.
fn table_key(id: u32) -> String {
    format!("\u{0}table {id}")
}

/// How far, on the ground, an edge converted into another system may move from where a run in
/// longitude/latitude takes it before a run says so: a metre.
const BENT_M: f64 = 1.0;

/// The edges of `original` that `converted`, its geometry converted vertex by vertex into `work`,
/// takes more than [`BENT_M`] from where a run in longitude/latitude takes them: straight in the
/// analysis plane there (Web Mercator, along a parallel for an edge running east), straight in
/// `work` here. A polygon ring's closing edge counts too, when the ring doesn't repeat its first
/// vertex. Their count, and how far the farthest moved, in metres: measured across the chord at a
/// quarter, half and three quarters of the edge (an edge across a transverse Mercator's central
/// meridian bends both ways, its middle staying on the chord), in ground metres whatever `work`'s
/// units. An edge under a kilometre in the analysis plane is skipped: its two readings part by
/// under a metre short of 88.9° from the equator (about L² tan φ / 8R).
pub(crate) fn bent_edges(original: &Dataset, converted: &Dataset, work: Crs) -> Result<(usize, f64), String> {
    use rustgis_geom::{Coord, lonlat_to_plane, plane_to_lonlat};
    /// Its parts' vertices, each with whether it is a polygon's ring, whose closing edge counts.
    fn parts(g: &rustgis_data::Geometry) -> Vec<(&[Coord], bool)> {
        match g {
            rustgis_data::Geometry::Line(parts) => parts.iter().map(|p| (p.as_slice(), false)).collect(),
            rustgis_data::Geometry::Polygon(polys) => polys.iter().flatten().map(|r| (r.as_slice(), true)).collect(),
            rustgis_data::Geometry::Point(_) | rustgis_data::Geometry::MultiPoint(_) => Vec::new(),
        }
    }
    /// The edges of a part: each pair of neighbours, and a ring's closing pair when it isn't
    /// closed already.
    fn edges(part: &[Coord], ring: bool) -> impl Iterator<Item = (Coord, Coord)> + '_ {
        let closing = ring.then(|| part.first().zip(part.last())).flatten().filter(|(a, b)| a != b).map(|(a, b)| (*b, *a));
        part.windows(2).filter_map(|w| Some((*w.first()?, *w.get(1)?))).chain(closing)
    }
    let (to_lonlat, into, back) = (original.lonlat_conversion()?, Crs::LonLat.to(work)?, work.to(Crs::LonLat)?);
    let (mut n, mut far) = (0usize, 0.0f64);
    for (i, (f, c)) in original.features.rows().zip(converted.features.rows()).enumerate() {
        if i.is_multiple_of(256) {
            rustgis_data::progress::check().map_err(|e| e.to_string())?;
        }
        let (Some(g), Some(h)) = (f.geometry(), c.geometry()) else { continue };
        for ((part, ring), (held, _)) in parts(g).into_iter().zip(parts(h)) {
            for ((a, b), (c0, c1)) in edges(part, ring).zip(edges(held, ring)) {
                let (Some(a), Some(b)) = (to_lonlat.point(a), to_lonlat.point(b)) else { continue };
                let (pa, pb) = (lonlat_to_plane(a), lonlat_to_plane(b));
                // (Also skipped: a length that is no number, a coordinate being missing.)
                let length = (pb.x - pa.x).hypot(pb.y - pa.y);
                if length.is_nan() || length < 1000.0 {
                    continue;
                }
                let (dx, dy) = (c1.x - c0.x, c1.y - c0.y);
                let chord = dx.hypot(dy);
                // In metres: the chord's ground length over its length in `work`'s units.
                let (Some(e0), Some(e1)) = (back.point(c0), back.point(c1)) else { continue };
                let per_unit = rustgis_geom::wgs84_inverse(e0, e1).0 / chord;
                // (Ends that meet in `work` give no chord to measure across.)
                if !per_unit.is_finite() {
                    continue;
                }
                // How far the edge as a run in longitude/latitude takes it lies across the chord
                // (not along it: where a point is along an edge differs between the planes, a
                // meridian's middle by some 280 m over a degree).
                let across = |t: f64| {
                    let on = plane_to_lonlat(Coord::new(pa.x + t * (pb.x - pa.x), pa.y + t * (pb.y - pa.y)));
                    into.point(on).map(|m| ((m.x - c0.x) * dy - (m.y - c0.y) * dx).abs() / chord * per_unit)
                };
                let d = [0.25, 0.5, 0.75].into_iter().filter_map(across).fold(0.0, f64::max);
                if d > BENT_M {
                    n += 1;
                    far = far.max(d);
                }
            }
        }
    }
    Ok((n, far))
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
    let rows = env_rows(map, full, env, params)?;
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
) -> Result<std::collections::HashMap<String, Vec<usize>>, String> {
    let Some(e) = env.extent else { return Ok(std::collections::HashMap::new()) };
    let Some(input) = params.get("target").or_else(|| params.get("input")).and_then(Value::as_str) else {
        return Ok(std::collections::HashMap::new());
    };
    // The layer the name finds (the first of that name), as the tool will.
    map.layer_by_name(input)
        .into_iter()
        .filter(|l| !l.is_raster() && l.selection.is_empty())
        .map(|l| {
            let ds = full.get(&l.name.to_ascii_lowercase()).unwrap_or(&l.dataset);
            // A feature is admitted when its box in longitude/latitude meets the extent, as in a
            // run in longitude/latitude (which reads the whole layer converted too): no box in the
            // layer's own system tests that, a box there around the extent missing a feature whose
            // box in longitude/latitude meets it. One with a place that can't be read in
            // longitude/latitude is admitted by its own box meeting the extent's there, as before;
            // a layer that can't be read in longitude/latitude at all admits none, as before (and
            // the run says none is inside).
            let own = std::cell::OnceCell::new();
            let by_own = |g: &rustgis_data::Geometry| g.extent().intersects(own.get_or_init(|| ds.extent_from_lonlat(&e)));
            let to_lonlat = ds.lonlat_conversion().ok();
            let meets = |g: &rustgis_data::Geometry| match &to_lonlat {
                Some(to) if to.is_identity() => g.extent().intersects(&e),
                Some(to) => Dataset::geometry_to_lonlat_by(to, g).map_or_else(|_| by_own(g), |g| g.extent().intersects(&e)),
                None => false,
            };
            // (Each feature converted: a Cancel is looked for every 256 of them.)
            let mut rows = Vec::new();
            for (i, r) in ds.features.rows().enumerate() {
                if i.is_multiple_of(256) {
                    rustgis_data::progress::check().map_err(|e| format!("{}: {e}", l.name))?;
                }
                if r.geometry().is_some_and(meets) {
                    rows.push(i);
                }
            }
            Ok((l.name.to_ascii_lowercase(), rows))
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
        // (A standalone table converted for the run is had by its number, [`table_key`].)
        if layer.starts_with('\0') {
            return None;
        }
        self.loaded
            .get(layer)
            .or_else(|| self.loaded.get(&layer.to_ascii_lowercase()))
            // A layer read as it is only in the system the tool works in: one in another system is
            // converted before the run ([`work_inputs`]) when the tool names it, and isn't read
            // in its own coordinates otherwise.
            .or_else(|| {
                project_input(&self.snap.map, &self.snap.tables, layer, false)
                    .map(|i| i.dataset())
                    // A table without geometry reads alike in any system, and so does a layer
                    // the tool reads only the attributes of.
                    .filter(|d| {
                        d.geometry_type == rustgis_data::GeometryType::None
                            || self.unread.contains(&layer.to_ascii_lowercase())
                            || d.coord_crs().to(self.work).is_ok_and(|to| to.is_identity())
                    })
            })
    }
    fn table(&self, id: u32) -> Option<&Dataset> {
        // Converted before the run when the tool's parameters give its number ([`inputs_in`]);
        // else read as it is only in the system the tool works in, as a layer is, or when the
        // tool reads only its attributes (Join Field's join table).
        self.loaded.get(&table_key(id)).or_else(|| {
            self.snap.tables.iter().find(|t| t.id == id).map(|t| &t.dataset).filter(|d| {
                d.geometry_type == rustgis_data::GeometryType::None
                    || self.unread.contains(&table_key(id))
                    || d.coord_crs().to(self.work).is_ok_and(|to| to.is_identity())
            })
        })
    }
    fn work_crs(&self) -> Crs {
        self.work
    }
    fn project_crs(&self) -> Option<Crs> {
        self.snap.project
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
    fn unproject(&self, m: rustgis_geom::Coord) -> Option<rustgis_geom::Coord> {
        let (_, flat, to_meter) = self.flat()?;
        flat.inverse(rustgis_geom::Coord::new(m.x / to_meter, m.y / to_meter))
    }
    fn map_edge(&self) -> Option<f64> {
        match &self.flat()?.1 {
            rustgis_map::projection::Flat::Mercator => Some(180.0),
            rustgis_map::projection::Flat::Custom(p) => p.edge().map(|e| e.lon),
        }
    }
    fn view_extent(&self) -> Option<rustgis_geom::Extent> {
        *self.view.get_or_init(|| view_extent(&self.snap))
    }
    fn raster(&self, layer: &str) -> Option<RasterInput<'_>> {
        let l = self.snap.map.layer_by_name(layer)?;
        let rl = l.raster.as_ref()?;
        let read = self.rasters.get(&l.name.to_ascii_lowercase()).map(std::sync::Arc::as_ref);
        Some(RasterInput {
            raster: read.or(rl.data.as_deref())?,
            path: rl.path.as_deref(),
            embedded: rl.embedded.as_deref().map(|b| (rl.file_name.as_deref().unwrap_or("raster.tif"), b.as_slice())),
        })
    }
}

fn run(s: &mut Session, p: &Value) -> CmdResult {
    let tool = ps(p, "tool").ok_or("tool is required")?.to_string();
    let params = p.get("params").cloned().unwrap_or(json!({}));
    run_tool_saying(s, &tool, &params)
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
    // A raster file read in place is read once for the batch, not once an item.
    #[cfg(not(target_arch = "wasm32"))]
    let _reads = BatchReads::start();
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
        .filter(|key| !rustgis_analysis::is_expected_key(spec, &base_params, key) && *key != batch_param)
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
            "{} inputs are more than a batch runs (at most {MAX_BATCH_ITEMS}); split it into smaller batches",
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
    #[cfg_attr(target_arch = "wasm32", expect(unused_mut, reason = "batch inputs are read from files on the desktop only"))]
    let mut reader_warnings: Vec<(String, String, Option<LossCode>)> = Vec::new();
    let mut items_reporting_messages = 0;
    let mut distinct_tool_messages: Vec<(String, Option<LossCode>, usize)> = Vec::new();
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
                        for (msg, code) in report.messages.into_iter().map(|m| (m.text, m.loss)) {
                            item_reader_warnings.push(format!("{target}: {msg}"));
                            reader_warnings.push((target.clone(), msg, code));
                        }
                        Some(super::layer::add_dataset(s, dataset))
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        let rec_params = batch_item_params(spec, &base_params, batch_param, target);
                        let spec = rustgis_analysis::tool(tool_id);
                        let name = spec.map(|t| t.name).unwrap_or("Tool").to_string();
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

        let item = |input: &str| {
            let mut p = batch_item_params(spec, &base_params, batch_param, input);
            if tool_has_output && let Value::Object(ref mut map) = p {
                map.insert("output".to_string(), json!(final_out_name));
            }
            p
        };
        let item_params = item(&target_layer_name);
        let record_params = item(target);

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
                    items_reporting_messages += usize::from(messages.iter().any(|(_, code)| code.is_none()));
                    for (text, code) in &messages {
                        if let Some(entry) = distinct_tool_messages.iter_mut().find(|(m, c, _)| m == text && c == code) {
                            entry.2 += 1;
                        } else {
                            distinct_tool_messages.push((text.clone(), *code, 1));
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
                    let err = messages.last().map(|(text, _)| text.clone()).unwrap_or_else(|| "tool produced no output".into());
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
                        err_obj["messages"] = json!(messages.iter().map(|(text, _)| text).collect::<Vec<_>>());
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
    let mut losses = BatchLosses::default();
    for (text, code, count) in &distinct_tool_messages {
        if let Some(code) = code {
            losses.add(*code, text, *count);
        }
    }
    if items_reporting_messages > 0 {
        let plain = || distinct_tool_messages.iter().filter(|(_, code, _)| code.is_none());
        let sample = plain().take(NAMED).map(|(text, _, count)| batch_message(text, *count)).collect::<Vec<_>>().join("; ");
        let total = plain().count();
        let more = if total > NAMED { format!(" and {} more, in the result", total - NAMED) } else { String::new() };
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
        // Each warning with its distinct inputs, and how many reads gave it: an input named twice
        // is read twice.
        let mut aggregated: Vec<(&str, Option<LossCode>, Vec<&str>, usize)> = Vec::new();
        for (target, msg, code) in &reader_warnings {
            if let Some((_, _, inputs, reads)) = aggregated.iter_mut().find(|(m, c, _, _)| *m == msg.as_str() && c == code) {
                *reads += 1;
                if !inputs.contains(&target.as_str()) {
                    inputs.push(target.as_str());
                }
            } else {
                aggregated.push((msg.as_str(), *code, vec![target.as_str()], 1));
            }
        }
        for (msg, code, inputs, reads) in aggregated {
            let count = inputs.len();
            let inputs_list = inputs.join(", ");
            let label = if count == 1 { "input" } else { "inputs" };
            match code {
                // Its inputs are named in the example, which the batch's loss notice prefixes.
                Some(code) => losses.add_stated(code, &format!("{count} {label} ({inputs_list}): {msg}"), reads),
                None => s.notify(format!("gp.batch: {count} {label} ({inputs_list}): {msg}"), false),
            }
        }
    }
    losses.say(s);

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

    let warnings: Vec<Value> = reader_warnings.iter().map(|(input, msg, _)| json!({"input": input, "warning": msg})).collect();

    let mut ret = json!({
        "tool": tool_id,
        "total": targets.len(),
        "succeeded": num_ok,
        "failed": num_err,
        "layers": layer_ids,
        "outputs": output_names,
        "errors": errors,
        "warnings": warnings,
        "messages": distinct_tool_messages.iter().map(|(m, _, c)| json!({"message": m, "items": c})).collect::<Vec<_>>(),
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

/// A `gp.batch` item's parameters: `base` with `batch_param` set to `input`. A tool reading the
/// input under another name (Join Field's `input` for `target`) mustn't take a static value given
/// under it in place of each batch input, nor keep the tool dialog's name of the table it replaced
/// (`input_name`, `target_name`), so those go. An alias the tool declares itself goes too (Join
/// Field's `target` beside a `target_layer` batch): `gp.batch` says which names it replaced.
fn batch_item_params(spec: &rustgis_analysis::ToolSpec, base: &Value, batch_param: &str, input: &str) -> Value {
    let mut p = base.clone();
    if let Value::Object(ref mut map) = p {
        for name in rustgis_analysis::param_aliases(spec, batch_param).iter().copied().chain([batch_param]) {
            for key in [name.to_string(), format!("{name}_name")] {
                if key != batch_param && (key == name || !rustgis_analysis::is_accepted_param(spec, &key)) {
                    map.remove(&key);
                }
            }
        }
        map.insert(batch_param.to_string(), json!(input));
    }
    p
}

/// What a tool says of the keys in `params` it doesn't read (#2307), as `Session::run` says it of a
/// command's (#194) and `gp.batch` of its items': "Buffer ignores 'unit' (it reads input,
/// distance, units, dissolve, output)". `None` when it reads them all. A misspelled parameter
/// would otherwise leave the tool at its default without a word: `"unit": "Miles"` buffers in
/// kilometers.
pub(crate) fn ignored_params(spec: &rustgis_analysis::ToolSpec, params: &Value) -> Option<String> {
    let unknown: Vec<&str> =
        params.as_object()?.keys().map(String::as_str).filter(|k| !rustgis_analysis::is_expected_key(spec, params, k)).collect();
    if unknown.is_empty() {
        return None;
    }
    let reads: Vec<&str> = spec.params.iter().map(|p| p.name).collect();
    crate::ignored_keys(spec.name, unknown, &reads)
}

/// The run of `tool` admitted, as [`Session::authorize_tool`] admits it, warning which of
/// `params` it ignores ([`ignored_params`]). A refused run's error says them itself when a
/// required parameter is missing, the likely reason (a failed command takes its warnings back).
/// For the runs started from what a caller wrote (`gp.run`, the app's runs). A command that runs a
/// tool with its own parameters (`gp.spatialJoin`, `gp.zonalStats`) declares the tool's keys as
/// its own, so `Session::run` has said any it ignores, as a command.
pub(crate) fn admitted_saying(
    s: &mut Session,
    tool: &str,
    params: &Value,
) -> Result<crate::policy::Authorized<crate::policy::ToolOp>, String> {
    let op = s.authorize_tool(tool, params)?;
    if let Some(said) = ignored_params(op.op().pin.spec(), params) {
        s.warn(said);
    }
    Ok(op)
}

/// [`run_tool`] for parameters a caller wrote (`gp.run`, a model's step), saying first which of
/// them the tool ignores ([`admitted_saying`]). A failed run takes that warning back, so its
/// error says the keys too when the run fails over its parameters or inside the tool (a run the
/// policy refuses is refused for who asked, not for what it was given).
pub(crate) fn run_tool_saying(s: &mut Session, tool: &str, params: &Value) -> CmdResult {
    let control = run_control(s);
    let op = admitted_saying(s, tool, params)?;
    let spec = op.op().pin.spec();
    s.invoke_tool(op, &control).map_err(|e| saying_unread(spec, params, e))
}

/// [`run_tool_saying`] without the warning about ignored parameters: a model's run says them once
/// for all its steps ([`crate::model::run`]), beside its result or its error, so a run that fails
/// later keeps them. The error still says the keys it doesn't read.
pub(crate) fn run_step(s: &mut Session, tool: &str, params: &Value) -> CmdResult {
    let control = run_control(s);
    let op = s.authorize_tool(tool, params)?;
    let spec = op.op().pin.spec();
    s.invoke_tool(op, &control).map_err(|e| saying_unread(spec, params, e))
}

/// `error` with the keys of `params` that `spec` doesn't read after it — " (given 'unit', which
/// Buffer doesn't read)" — unless it says them already (a refused parameter's does).
fn saying_unread(spec: &rustgis_analysis::ToolSpec, params: &Value, error: String) -> String {
    // The same text the tool adds to a refused parameter's error, made once: an error that ends
    // in it says it already.
    let unread = rustgis_analysis::unread(spec, params);
    if unread.is_empty() || error.ends_with(&unread) {
        return error;
    }
    format!("{error}{unread}")
}

/// The control a tool run in the session takes: where a person at a terminal can stop it with
/// Ctrl-C ([`Session::stoppable`](crate::Session::stoppable)), no work cap
/// ([`rustgis_analysis::control::Control::stoppable`]).
fn run_control(s: &Session) -> std::sync::Arc<rustgis_analysis::control::Control> {
    use rustgis_analysis::control::Control;
    if s.stoppable { Control::stoppable() } else { Control::new() }
}

/// Runs a (built-in) tool and applies its result, as `gp.run` does, under [`run_control`].
pub fn run_tool(s: &mut Session, tool: &str, params: &Value) -> CmdResult {
    let control = run_control(s);
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
) -> Result<(Value, ToolMessages), String> {
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
    // A look at Cancel as the work's own are (counted for a test's control): a model's or script's
    // tool on the copy stops here when cancelled between steps.
    if rustgis_data::progress::Probe::cancelled(control.as_ref()) {
        let e = format!("{} was cancelled before it started", spec.name);
        return apply_with(s, run_of(t0), kept(), Err(e), 0, opts);
    }
    let continuation = s.tool_continuation(&op)?;
    #[cfg(not(target_arch = "wasm32"))]
    let output_slot = match &s.tools_ahead {
        Some(Ahead::Recording(recorded)) => Some(recorded.len()),
        _ => None,
    };
    #[cfg(not(target_arch = "wasm32"))]
    let mut ran = take_ahead(s, pin, params);
    #[cfg(not(target_arch = "wasm32"))]
    if crate::image_grid::script_grids_invalidated(s) {
        return Err("the script inputs changed; run the script again".into());
    }
    #[cfg(not(target_arch = "wasm32"))]
    let replayed_outputs = ran.as_mut().map(|tool| std::mem::take(&mut tool.grid_outputs));
    #[cfg(not(target_arch = "wasm32"))]
    let ran = ran.map(|tool| (tool.result, tool.full_ids, tool.millis));
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
            // Reading the inputs stops on Cancel too, as a run the app starts on a thread does.
            #[cfg(not(target_arch = "wasm32"))]
            let _probe = rustgis_analysis::control::install_progress(Some(control.clone()));
            // A built-in tool run here (the command line, a script on its own thread) reads a
            // raster file read in place, as one the app starts on a thread does.
            let tiled = tiled_for(pin);
            let inputs = match map_inputs(s, Some(spec), params, tiled) {
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
                result.losses.extend(inputs.losses.iter().cloned());
            }
            // The conversion stays under this run's Cancel, as in `ToolRun::run`: the tool's own
            // probe ended with it.
            let held = {
                let _probe = rustgis_analysis::control::install_progress(Some(control.clone()));
                res.as_mut().map_or(Ok(()), |r| inputs.hold_results(r))
            };
            if let Err(e) = held {
                res = Err(e);
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
                ran.push(RanAhead::Tool(Box::new(ToolAhead {
                    pin: pin.clone(),
                    params: params.clone(),
                    read,
                    view,
                    result: res.clone(),
                    full_ids: ids.clone(),
                    millis,
                    grid_outputs: Vec::new(),
                })));
            }
            (res, ids, millis)
        }
    };
    pin.current()?;
    continuation.check()?;
    fit_to_windows(s, &mut res, &ids);
    let applied = apply_with(s, run_of(t0), kept(), res, millis, opts);
    #[cfg(not(target_arch = "wasm32"))]
    if let Ok((out, _)) = &applied
        && (recording || replayed_outputs.is_some())
    {
        let outputs = output_layers(s, out);
        if let Some(recorded) = replayed_outputs {
            crate::image_grid::replay_tool_outputs(s, &recorded, &outputs)?;
        } else if let Some(index) = output_slot
            && let Some(Ahead::Recording(recorded)) = &mut s.tools_ahead
            && let Some(RanAhead::Tool(tool)) = recorded.get_mut(index)
        {
            tool.grid_outputs = outputs;
        }
    }
    applied
}

/// The exact layers just installed by a tool, rather than similarly named layers in the map.
#[cfg(not(target_arch = "wasm32"))]
fn output_layers(s: &Session, out: &Value) -> Vec<rustgis_map::Layer> {
    let ids = out.get("layer").into_iter().chain(out.get("layers").and_then(Value::as_array).into_iter().flatten());
    ids.filter_map(Value::as_u64).filter_map(|id| u32::try_from(id).ok()).filter_map(|id| s.map().layer(id).cloned()).collect()
}

/// Counts classified reports across a batch and keeps a bounded sample for each code.
#[derive(Default)]
struct BatchLosses(std::collections::BTreeMap<&'static str, BatchLoss>);

struct BatchLoss {
    code: LossCode,
    reports: usize,
    examples: Vec<String>,
    omitted: usize,
}

impl BatchLosses {
    /// Counts every report, even when its text is beyond the sample.
    fn add(&mut self, code: LossCode, text: &str, count: usize) {
        self.add_stated(code, &batch_message(text, count), count);
    }

    /// [`BatchLosses::add`] for a message that already says how many it covers (`count`):
    /// counted, without a repetition count added to it.
    fn add_stated(&mut self, code: LossCode, text: &str, count: usize) {
        let entry = self.0.entry(code.spec().id).or_insert_with(|| BatchLoss { code, reports: 0, examples: Vec::new(), omitted: 0 });
        entry.reports = entry.reports.saturating_add(count);
        if entry.examples.len() < 3 {
            entry.examples.push(text.to_string());
        } else {
            entry.omitted += 1;
        }
    }

    /// Publishes one warning per code. Full messages are in the batch result.
    fn say(self, s: &mut Session) {
        for loss in self.0.into_values() {
            let sample = loss.examples.join("; ");
            let more = if loss.omitted == 0 { String::new() } else { format!(" and {} more, in the result", loss.omitted) };
            s.notify_loss(loss.code, format!("gp.batch: {} loss report(s) ({sample}{more})", loss.reports).into());
        }
    }
}

/// The text of a distinct batch notice, with its repetition count when it appeared more than once.
fn batch_message(text: &str, count: usize) -> String {
    if count > 1 { format!("{text} (×{count})") } else { text.to_string() }
}

/// Text and optional loss code from a tool that has run.
pub(crate) type ToolMessages = Vec<(String, Option<LossCode>)>;

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
) -> Result<(Value, ToolMessages), String> {
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
    losses: Vec<LossMessage>,
}

#[cfg(any(target_arch = "wasm32", test))]
fn queue_file(s: &mut Session, file_name: String, bytes: Vec<u8>, record: GpRecord, losses: Vec<LossMessage>) -> CmdResult {
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
    s.file_outputs.push(PendingFile {
        id,
        file_name: file_name.clone(),
        bytes: Some(bytes),
        record,
        losses,
        continuation: s.continuation()?,
    });
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
        if file.record.ok {
            s.notify_written(message.clone());
        } else {
            s.notify(message.clone(), true);
        }
    }
    for loss in file.losses {
        if file.record.ok {
            publish_tool_loss(s, loss.code, &loss.text, true);
        } else {
            s.notify(loss.text.clone(), true);
        }
        file.record.messages.push(loss.text);
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
) -> Result<(Value, ToolMessages), String> {
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
    // File outputs reach the message publication below only after the native atomic write.
    // Pending browser downloads return above that point and are retained on delivery instead.
    let wrote_file = matches!(&result.output, Output::File { .. });
    let out = match result.output {
        Output::File { file_name, bytes } => {
            #[cfg(test)]
            if s.queue_gp_files {
                let msgs = result.messages.iter().cloned().map(|m| (m, None)).collect();
                return queue_file(
                    s,
                    file_name,
                    bytes,
                    GpRecord { tool: name, id, params, command, at, ok: false, messages: result.messages, millis, outputs: Vec::new() },
                    result.losses,
                )
                .map(|v| (v, msgs));
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let path = params.get("output").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()).unwrap_or(&file_name);
                if let Err(error) = write_file_output(s, std::path::Path::new(path), &bytes) {
                    let error = format!("{path}: {error}");
                    result.messages.extend(result.losses.into_iter().map(|loss| loss.text));
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
                let msgs = result.messages.iter().cloned().map(|m| (m, None)).collect();
                return queue_file(
                    s,
                    file_name,
                    bytes,
                    GpRecord { tool: name, id, params, command, at, ok: false, messages: result.messages, millis, outputs: Vec::new() },
                    result.losses,
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
            // Made in the system the tool worked in ([`MapInputs::hold_results`]): taken as made
            // when that is the layer's own, else written back in its own coordinates from
            // longitude/latitude. (A streamed layer whose whole read left a feature out is refused
            // before this: [`MapInputs::hold_results`].)
            // A streamed layer's window may be in another system than its source (longitude/latitude
            // unless the map is in the source's own), while the tool read the whole source: a
            // result in the source's own system is taken as made too, the layer then holding it.
            let own = l.source.as_ref().map(|src| src.coord_crs());
            l.dataset = if dataset.coord_crs() == l.dataset.coord_crs() || Some(dataset.coord_crs()) == own {
                dataset
            } else {
                let lonlat = if dataset.coord_crs() == Crs::LonLat { dataset } else { dataset.in_lonlat()?.into_owned() };
                l.dataset.results_from_lonlat(lonlat)?
            };
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
    let messages: ToolMessages = result
        .messages
        .iter()
        .cloned()
        .map(|m| (m, None))
        .chain(result.losses.iter().map(|loss| (loss.text.clone(), Some(loss.code))))
        .collect();
    if !opts.quiet {
        for (message, code) in &messages {
            if let Some(code) = code {
                publish_tool_loss(s, *code, message, wrote_file);
            } else if wrote_file {
                s.notify_written(message.clone());
            } else {
                s.notify(message.clone(), false);
            }
        }
    }
    result.messages.extend(result.losses.into_iter().map(|loss| loss.text));
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

/// Publishes a typed loss, retaining it through rollback when a file has already been delivered.
fn publish_tool_loss(s: &mut Session, code: LossCode, text: &str, written: bool) {
    let mark = s.notice_mark();
    s.notify_loss(code, text.to_string().into());
    if written {
        s.retain_written_notices(mark);
    }
}

#[cfg(test)]
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
                super::queue_file(s, p["name"].as_str().unwrap().into(), bytes, record, Vec::new())
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
                    losses: Vec::new(),
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
        let mark = s.notice_mark();
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
        let notices = s.notices_since(mark);
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

    // Apple's APFS refuses a file name that isn't UTF-8 ("Illegal byte sequence").
    #[cfg(all(unix, not(target_vendor = "apple")))]
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
                    losses: Vec::new(),
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
        // The tool dialog's `input_name` isn't warned of, but nor is it a parameter to run over.
        let res = s.run(
            "gp.batch",
            &json!({"tool": "buffer", "inputs": ["Cities"], "param": "input_name", "params": {"input": "Cities", "distance": 10}}),
        );
        assert!(res.is_err_and(|e| e.contains("tool 'buffer' has no parameter 'input_name'")));
        // Each item drops the dialog's name of the table it replaces, so its history doesn't name
        // the wrong one.
        let n = s.project.gp_history.len();
        s.run("gp.batch", &json!({"tool": "buffer", "inputs": ["Cities"], "params": {"input": 7, "input_name": "Old", "distance": 10}}))
            .unwrap();
        let recorded = &s.project.gp_history[n..];
        assert!(!recorded.is_empty() && recorded.iter().all(|h| h.params.get("input_name").is_none()), "{recorded:?}");
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
        let mark = s.notice_mark();
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
        let notices = s.notices_since(mark);
        assert!(notices.iter().any(|n| n.text.contains("3 invalid input(s) skipped")));
        assert!(notices.iter().any(|n| n.text.contains("'params' must be an object")));

        // Clean input says nothing
        let mut s2 = test_session();
        let mark2 = s2.notice_mark();
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
        let notices2 = s2.notices_since(mark2);
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
            losses: Vec::new(),
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

        let mark = s.notice_mark();
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
        assert!(s.notices_since(mark).iter().any(|n| n.text.contains("1 failed (L1)")));
    }

    fn empty_output_multi_message_runner(
        _spec: &rustgis_analysis::ToolSpec,
        _inputs: &dyn rustgis_analysis::Inputs,
        _params: &serde_json::Value,
    ) -> Result<rustgis_analysis::ToolResult, String> {
        Ok(rustgis_analysis::ToolResult {
            losses: Vec::new(),
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
        let mark = s.notice_mark();
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
        let notices = s.notices_since(mark);
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

        let mark = s.notice_mark();
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
        let notices = s.notices_since(mark);
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

        let mark = s.notice_mark();
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
        let notices = s.notices_since(mark);
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
        let notices = s.notices_since(mark);
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

        let mark = s.notice_mark();
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
        let recent: Vec<_> = s.notices_since(mark).iter().filter(|n| n.text.contains("reported messages")).collect();
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

        let mark = s.notice_mark();
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
        let notices = s.notices_since(mark);
        let msg_notice = notices.iter().find(|n| n.text.contains("reported messages")).unwrap();
        assert!(msg_notice.text.contains("Buffer: 1 features at 10 Kilometers"));
        assert!(msg_notice.text.contains("Buffer: 2 features at 10 Kilometers"));
    }

    #[test]
    fn batch_run_null_options_emit_no_spurious_notices() {
        let mut s = test_session();
        let mark = s.notice_mark();
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
        let notices = s.notices_since(mark);
        assert!(
            !notices.iter().any(|n| n.text.contains("must be an object")
                || n.text.contains("both 'inputs' and 'folder'")
                || n.text.contains("must be a string")
                || n.text.contains("must be an array")),
            "null options must not emit spurious notices: {notices:?}"
        );

        let mark2 = s.notice_mark();
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
        let notices2 = s.notices_since(mark2);
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
        let mark = s.notice_mark();
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
        let mark_clean = s_clean.notice_mark();
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
        let mark = s.notice_mark();
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
        let mark_clean = s_clean.notice_mark();
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

    /// `gp.run`, a run started in the background and a model's step say which parameters a tool
    /// ignores (#2307), as `gp.batch` does: a misspelled `unit` would otherwise leave Buffer at its
    /// default, kilometers, without a word. The run itself is as before; a clean run says nothing;
    /// a batch says it once, its own way; `model.validate` lists it as a warning, which doesn't
    /// keep the model from running.
    #[test]
    fn a_tool_says_which_parameters_it_ignores() {
        let said = |s: &Session, mark: u64| -> Vec<String> {
            s.notices_since(mark).iter().map(|n| n.text.clone()).filter(|t| t.contains(" ignores ")).collect()
        };
        let buffer = "Buffer ignores 'unit' (it reads input, distance, units, dissolve, output)";
        let mut s = test_session();
        let mark = s.notice_mark();
        s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Cities", "distance": 100, "unit": "Miles"}})).unwrap();
        assert_eq!(said(&s, mark), [buffer]);
        // The run is as before: the default units, which its own message names.
        assert!(s.notices_since(mark).iter().any(|n| n.text.contains("at 100 Kilometers")));
        let mark = s.notice_mark();
        s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Cities", "distance": 100, "units": "Miles"}})).unwrap();
        assert!(said(&s, mark).is_empty(), "{:?}", said(&s, mark));
        // The name the tool dialog puts beside a table's id (`input_name`) isn't taken for a stray;
        // `_name` beside a parameter that isn't a layer is.
        let buffer_spec = resolve_tool("buffer").unwrap();
        assert_eq!(ignored_params(buffer_spec, &json!({"input": 7, "distance": 1, "input_name": "Cities"})), None);
        // Beside a layer's name (a built-in tool reads digit text as one), or with no table id at
        // all, it's a stray: no tool reads it.
        for params in [
            json!({"input": "Cities", "distance": 1, "input_name": "Rivers"}),
            json!({"input": "7", "distance": 1, "input_name": "Rivers"}),
            json!({"distance": 1, "input_name": "Rivers"}),
        ] {
            let said = ignored_params(buffer_spec, &params).unwrap();
            assert!(said.starts_with("Buffer ignores 'input_name'"), "{said}");
        }
        let stray = ignored_params(buffer_spec, &json!({"input": "Cities", "distance": 1, "distance_name": "x"})).unwrap();
        assert!(stray.starts_with("Buffer ignores 'distance_name'"), "{stray}");
        // Many keys: the first five, sorted, each cut to 40 characters, then how many more.
        let mark = s.notice_mark();
        let long = "x".repeat(60);
        let mut params = json!({"input": "Cities", "distance": 1, "a": 1, "b": 1, "c": 1, "d": 1, "e": 1, "f": 1});
        params[&long] = json!(1);
        s.run("gp.run", &json!({"tool": "buffer", "params": params})).unwrap();
        let got = said(&s, mark);
        assert_eq!(got.len(), 1, "{got:?}");
        assert!(got[0].starts_with("Buffer ignores 'a', 'b', 'c', 'd', 'e', 2 more"), "{got:?}");
        let mark = s.notice_mark();
        s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Cities", "distance": 1, &long: 1}})).unwrap();
        assert!(said(&s, mark)[0].contains(&format!("'{}…'", "x".repeat(40))), "{:?}", said(&s, mark));
        // A control character in a key reaches no terminal as it is.
        let mark = s.notice_mark();
        s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "Cities", "distance": 1, "a\n\u{1b}[31mb": 1}})).unwrap();
        assert!(said(&s, mark)[0].starts_with("Buffer ignores 'a??[31mb'"), "{:?}", said(&s, mark));
        // A run refused for want of a parameter that was misspelled says which key it didn't read,
        // in its error: a failed command takes its warnings back.
        let e = s.run("gp.run", &json!({"tool": "clip", "params": {"input": "Cities", "clips": "Parks"}})).unwrap_err();
        assert!(e.ends_with("'Clip Features' is required (given 'clips', which Clip doesn't read)"), "{e}");
        // Any failed run says them, not only one missing a required parameter.
        let e = s
            .run("gp.run", &json!({"tool": "buffer", "params": {"input": "Cities", "distance": 1, "dissolve": "maybe", "unit": "Miles"}}))
            .unwrap_err();
        assert!(e.ends_with(" (given 'unit', which Buffer doesn't read)"), "{e}");
        assert_eq!(e.matches("doesn't read").count(), 1, "{e}");
        let e =
            s.run("gp.run", &json!({"tool": "buffer", "params": {"input": "No Such Layer", "distance": 1, "unit": "Miles"}})).unwrap_err();
        assert!(e.ends_with(" (given 'unit', which Buffer doesn't read)"), "{e}");
        // So does a model's step that fails for it.
        s.run("model.new", &json!({"name": "Clips", "steps": [{"tool": "clip", "params": {"input": "Cities", "clips": "Parks"}}]}))
            .unwrap();
        let e = s.run("model.run", &json!({"name": "Clips"})).unwrap_err();
        assert!(e.contains("(given 'clips', which Clip doesn't read)"), "{e}");
        assert_eq!(e.matches("'clips'").count(), 1, "said once: {e}");
        // A command that runs a tool for its own parameters says a stray key once, as a command.
        let mark = s.notice_mark();
        s.run("gp.spatialJoin", &json!({"target": "Cities", "join": "Parks", "prefx": "j_"})).unwrap();
        let got = said(&s, mark);
        assert!(got.len() == 1 && got[0].starts_with("gp.spatialJoin ignores 'prefx'"), "{got:?}");
        // It declares every key its tool reads, so those aren't taken for strays.
        let mark = s.notice_mark();
        let join = json!({"target": "Cities", "join": "Parks", "match": "Within a distance", "distance": 5000, "units": "Miles", "distanceField": "d"});
        s.run("gp.spatialJoin", &join).unwrap();
        assert!(said(&s, mark).is_empty(), "{:?}", said(&s, mark));
        for (cmd, tool) in [("gp.spatialJoin", "spatialJoin"), ("gp.zonalStats", "zonalStats")] {
            let command = s.registry.get(cmd).and_then(|c| c.param_keys()).unwrap();
            let tool = resolve_tool(tool).unwrap();
            let missing: Vec<&str> = tool.params.iter().map(|p| p.name).filter(|k| !command.contains(k)).collect();
            assert!(missing.is_empty(), "{cmd} doesn't declare {missing:?}");
        }
        // Zonal Statistics reads its raster only as `raster`: a run given the old `layer` is
        // refused, and the error names the key it was given.
        let e = s.run("gp.run", &json!({"tool": "zonalStats", "params": {"layer": "Cities", "polygons": "Parks"}})).unwrap_err();
        assert!(e.ends_with("'Input Raster' is required (given 'layer', which Zonal Statistics doesn't read)"), "{e}");
        // A run started in the background says it too, before its work.
        let mark = s.notice_mark();
        let _run = tool_start(&mut s, "buffer", &json!({"input": "Cities", "distance": 100, "unit": "Miles"})).unwrap();
        assert_eq!(said(&s, mark), [buffer]);
        // A batch says it once, its own way, not once per item as well.
        let mark = s.notice_mark();
        s.run("gp.batch", &json!({"tool": "buffer", "inputs": ["Cities"], "params": {"distance": 10, "unit": "Miles"}})).unwrap();
        assert!(said(&s, mark).is_empty(), "{:?}", said(&s, mark));
        assert!(s.notices_since(mark).iter().any(|n| n.text.contains("unrecognised parameter(s) ignored in 'params' (unit)")));
        // A model: validate warns without refusing it, and its step says it as it runs.
        let step = |p: Value| json!({"name": "Typo", "steps": [{"tool": "buffer", "params": p}]});
        s.run("model.new", &step(json!({"input": "Cities", "distance": 100, "unit": "Miles"}))).unwrap();
        let v = s.run("model.validate", &json!({"name": "Typo"})).unwrap();
        assert_eq!(v["ok"], true, "{v}");
        let warnings = v["warnings"].as_array().unwrap();
        assert_eq!(warnings.len(), 1, "{v}");
        assert_eq!((warnings[0]["code"].as_str(), warnings[0]["message"].as_str()), (Some("unknown-param"), Some(buffer)), "{v}");
        let mark = s.notice_mark();
        s.run("model.run", &json!({"name": "Typo"})).unwrap();
        assert_eq!(said(&s, mark), [buffer]);
        s.run("model.new", &json!({"name": "Clean", "steps": [{"tool": "buffer", "params": {"input": "Cities", "distance": 100}}]}))
            .unwrap();
        let v = s.run("model.validate", &json!({"name": "Clean"})).unwrap();
        assert_eq!(v["warnings"], json!([]), "{v}");
    }

    /// A model saved as steps says each kind of ignored parameter once for its run, with the
    /// steps that give it, and a failed run carries it in its error instead of a notice that
    /// would go back with the run's changes.
    #[test]
    fn a_model_of_steps_says_ignored_parameters_once() {
        let said = |s: &Session, mark: u64| -> Vec<String> {
            s.notices_since(mark).iter().filter(|n| !n.error).map(|n| n.text.clone()).filter(|t| t.contains(" ignores ")).collect()
        };
        let buffer = "Buffer ignores 'unit' (it reads input, distance, units, dissolve, output)";
        let mut s = test_session();
        let twice = json!([
            {"tool": "buffer", "params": {"input": "Cities", "distance": 100, "unit": "Miles"}},
            {"tool": "buffer", "params": {"input": "%1", "distance": 10, "unit": "Miles"}}
        ]);
        s.run("model.new", &json!({"name": "Twice", "steps": twice})).unwrap();
        let mark = s.notice_mark();
        s.run("model.run", &json!({"name": "Twice"})).unwrap();
        assert_eq!(said(&s, mark), [format!("{buffer} (in 2 steps)")]);
        // A failing step after an ignoring one: the error carries the warning, and no warning
        // is left behind for the run that failed (its error is posted as one, as any is).
        let stops = json!([
            {"tool": "buffer", "params": {"input": "Cities", "distance": 100, "unit": "Miles"}},
            {"tool": "buffer", "params": {"distance": 10}}
        ]);
        s.run("model.new", &json!({"name": "Stops", "steps": stops})).unwrap();
        let mark = s.notice_mark();
        let e = s.run("model.run", &json!({"name": "Stops"})).unwrap_err();
        assert!(e.starts_with("step 2 (buffer)"), "{e}");
        assert!(e.ends_with(&format!("; {buffer}")), "{e}");
        assert!(said(&s, mark).is_empty(), "{:?}", said(&s, mark));
    }

    #[test]
    fn batch_run_params_batch_param_overwritten_notified() {
        let mut s = test_session();
        let mark = s.notice_mark();
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
        let mark_clean = s_clean.notice_mark();
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
        let mark1 = s.notice_mark();
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
        let mark_clean1 = s_clean1.notice_mark();
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
        let mark2 = s2.notice_mark();
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
        let mark_clean2 = s_clean2.notice_mark();
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

        let mark = s.notice_mark();
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
                    // `input` names Join Field's target too: it must not stand in for each batch input,
                    // nor its dialog-given name (`input_name`) stay beside each.
                    "params": {"input": "Other", "input_name": "Other", "targetField": "GEOID", "joinTable": "Census", "joinField": "FIPS"}
                }),
            )
            .unwrap();
        assert_eq!(res["succeeded"], 2, "{res}");
        let targets: Vec<_> = s.project.gp_history[before..].iter().map(|r| r.params["target_layer"].clone()).collect();
        assert_eq!(targets, [json!("Tracts"), json!("Blocks")]);
        assert!(s.project.gp_history[before..].iter().all(|r| r.params.get("input").is_none() && r.params.get("input_name").is_none()));
        let other = s.map().layer_by_name("Other").unwrap();
        assert!(!other.dataset.fields.iter().any(|f| f.name == "FIPS"), "the static alias's layer was joined");
    }

    #[test]
    fn batch_item_whose_input_cant_be_read_is_kept_in_the_history() {
        let mut s = test_session();
        let before = s.project.gp_history.len();
        // A name that is neither a layer, a table nor a file: the tool's input can't be mapped.
        let params = json!({"input": 7, "input_name": "Old", "distance": 1});
        let res = s.run("gp.batch", &json!({"tool": "buffer", "inputs": ["Cities", "Nowhere"], "params": params})).unwrap();
        assert_eq!(res["failed"], 1, "{res}");
        let failed: Vec<_> = s.project.gp_history[before..].iter().filter(|r| !r.ok).collect();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].params["input"], "Nowhere");
        // It doesn't keep the tool dialog's name of the table it replaced either.
        assert!(failed[0].params.get("input_name").is_none(), "{:?}", failed[0].params);
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
        let mark = s.notice_mark();
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
        let mark = s.notice_mark();
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
        let notices = s.notices_since(mark);
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
        let mark = s.notice_mark();
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
        let notices = s.notices_since(mark);
        assert!(
            !notices.iter().any(|n| n.text.contains("unsupported files skipped")),
            "shapefile companion files must not be reported as unsupported: {notices:?}"
        );

        std::fs::write(root.join("notes.txt"), b"some notes").unwrap();
        let mut s2 = Session::new(crate::Project::default());
        let mark2 = s2.notice_mark();
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
        let notices2 = s2.notices_since(mark2);
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

        let mark = s.notice_mark();
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
        let notices = s.notices_since(mark);
        assert!(notices.iter().any(|n| n.text.contains("1 raster layer(s) left out of '*' (it runs vector layers)")));

        let mut s2 = test_session();
        let mark2 = s2.notice_mark();
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
        let notices2 = s2.notices_since(mark2);
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
        let mark = s.notice_mark();
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
        let notices = s.notices_since(mark);
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
        let mark = s.notice_mark();
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
        // A Z/M loss has its code, so it is said as one loss notice for the batch.
        let notice = s.notices_since(mark).iter().find(|n| n.text.contains("Z or M")).unwrap();
        assert_eq!(notice.loss(), Some(rustgis_data::loss::LossCode::DroppedZm));
        assert!(notice.text.starts_with("gp.batch: 4 loss report(s) (4 inputs ("), "{}", notice.text);
        assert!(!notice.text.contains("(×"), "{}", notice.text);
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

        // An input named twice is read, and its loss counted, twice.
        let mark = s.notice_mark();
        s.run("gp.batch", &json!({"tool": "buffer", "inputs": [&files[0], &files[0]], "params": {"distance": 5}})).unwrap();
        let twice = s.notices_since(mark).iter().find(|n| n.text.contains("Z or M")).unwrap().text.clone();
        assert!(twice.starts_with("gp.batch: 2 loss report(s) (1 input ("), "{twice}");

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

        let mark = s.notice_mark();
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
        let notices = s.notices_since(mark);
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod replay_rollback_tests {
    use super::*;
    use rustgis_data::{Field, FieldType, Geometry, GeometryType, Value as Cell};
    fn prepare(s: &Session, params: &Value) -> Result<RanAheads, String> {
        let mut worker = Session::new(s.project.clone());
        worker.active_layer = s.active_layer;
        worker.opened = s.opened;
        worker.view_size = s.view_size;
        worker.set_extensions(s.extensions().clone());
        worker.tools_ahead = Some(Ahead::Recording(Vec::new()));
        worker.grids_ahead = Some(crate::image_grid::Ahead::Recording(Vec::new()));
        worker.run("script.run", params)?;
        Ok(ran_ahead(&mut worker))
    }
    #[test]
    fn annotation_grid_post_apply_replay_error_rolls_back_outputs_history_and_undo() {
        let mut s = Session::new(crate::Project::default());
        let mut samples = Dataset::new("Samples", GeometryType::Point, vec![Field::new("v", FieldType::Double)]);
        for (x, y, v) in [(0., 0., 1.), (2., 0., 2.), (0., 2., 3.), (2., 2., 4.)] {
            samples.push(Some(Geometry::Point(rustgis_geom::Coord::new(x, y))), vec![Cell::Float(v)]);
        }
        // Another layer first, so a rollback that forgets the chosen one would pick it instead.
        s.map_mut().add_dataset(Dataset::new("Other", GeometryType::Point, vec![]));
        let chosen = s.map_mut().add_dataset(samples);
        s.active_layer = Some(chosen);
        s.run("project.rename", &json!({"name":"Before"})).unwrap();
        s.run("project.rename", &json!({"name":"Future"})).unwrap();
        s.run("edit.undo", &json!({})).unwrap();
        s.mark_saved();
        let params = json!({"script":format!("gp.run {}\nraster.imageGrid {}", json!({"tool":"idwInterpolation","params":{"input":"Samples","field":"v","extent":"0,0,2,2","pixelSize":0.5,"output":"Surface"}}), json!({"layer":"Surface"}))});
        let before = serde_json::to_value(&s.project).unwrap();
        let history = (s.undo_depth(), s.undo_label(), s.redo_label(), s.dirty());
        let mut ran = prepare(&s, &params).unwrap();
        // Corrupt only the recorded output identity: the tool's input snapshot and actual
        // output still match, so replay accepts and applies it before binding fails.
        let Some(RanAhead::Tool(tool)) = ran.tools.first_mut() else { panic!("expected prepared tool") };
        tool.grid_outputs[0].name = "Injected output identity mismatch".into();
        s.statistics_need_read_ahead(true);
        let error = super::super::reads::taking(&mut s, "script.run", ran, |s| s.run("script.run", &params)).unwrap_err();
        assert!(error.contains("tool outputs changed"), "{error}");
        assert_eq!(s.tools_taken, 1, "the error must occur after the prepared tool was taken and applied");
        assert_eq!(serde_json::to_value(&s.project).unwrap(), before, "outputs and successful GP history must roll back");
        assert_eq!(s.active_layer, Some(chosen), "the layer chosen before the script must stay chosen");
        assert_eq!((s.undo_depth(), s.undo_label(), s.redo_label(), s.dirty()), history);
        assert!(s.map().layer_by_name("Surface").is_none());
        assert!(s.grids_ahead.is_none() && s.tools_ahead.is_none());
        s.run("edit.redo", &json!({})).unwrap();
        assert_eq!(s.project.name, "Future", "a failed replay must preserve the prior Redo entry");
        s.run("edit.undo", &json!({})).unwrap();
        let ran = prepare(&s, &params).unwrap();
        let answer = super::super::reads::taking(&mut s, "script.run", ran, |s| s.run("script.run", &params)).unwrap();
        assert_eq!(answer["ran"][1]["result"]["width"], 4);
        assert_eq!(s.map().layers.iter().filter(|layer| layer.name == "Surface").count(), 1);
        assert!(s.map().layer_by_name("Surface (2)").is_none());
        assert_eq!(s.undo_depth(), history.0 + 1);
        s.run("edit.undo", &json!({})).unwrap();
        // Undo deliberately bumps geometry revisions to invalidate rendering caches.
        assert!(s.map().layer_by_name("Surface").is_none());
        assert_eq!(s.map().layers.len(), 2);
        assert_eq!(serde_json::to_value(&s.map().layers[0].dataset).unwrap(), before["maps"][0]["layers"][0]["dataset"]);
        assert!(s.project.gp_history.is_empty());
        assert_eq!(s.undo_depth(), history.0);
    }
}
