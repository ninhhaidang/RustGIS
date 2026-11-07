//! The Workflows pane (the Analyze tab's workflows, raster functions and models) and the Script
//! pane (commands run from text). Map clicks fill in points where a workflow needs them.

use egui::{Ui, vec2};
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::{Tokens, semibold};

/// What the next map clicks are for, and the points so far.
#[derive(Clone, Debug, Default)]
pub struct Pick {
    pub purpose: &'static str,
    pub needed: usize,
    pub points: Vec<rustgis_engine::geom::Coord>,
}

/// The Workflows pane's inputs and results.
#[derive(Default)]
pub struct Workflow {
    pub section: &'static str,
    pub layer: String,
    pub layer2: String,
    pub field: String,
    pub number: String,
    pub number2: String,
    pub text: String,
    pub function: String,
    pub chain: Vec<String>,
    pub format: String,
    pub pick: Option<Pick>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub script: String,
    pub script_out: Vec<(String, bool)>,
    /// The Script pane's last run, shown above its results.
    pub script_last: Option<ScriptRun>,
    /// The last run's Details window is open.
    pub script_details: bool,
    /// When each Script pane run under way or waiting started (seconds since 1970, and the
    /// instant), and the project it was asked in ([`rustgis_engine::Session::opened`]), in the
    /// order asked, for [`script_ran`] to tell as each lands.
    script_started: std::collections::VecDeque<(u64, web_time::Instant, u64)>,
    /// When the last Script pane run landed: one waiting behind it started then, not when asked.
    script_landed: Option<web_time::Instant>,
    /// The project the Script pane's script, results and count are for
    /// ([`rustgis_engine::Session::opened`]); `None` until the pane first shows.
    script_project: Option<u64>,
    pub trend: TrendForm,
    pub selected_only: Option<bool>,
    pub zonal_stats: ZonalStatsForm,
    /// A workflow running on a thread of its own: its progress and Cancel show, and what it made
    /// lands through [`land_job`].
    #[cfg(not(target_arch = "wasm32"))]
    pub job: Option<Job>,
}

/// A workflow running on a thread of its own, so the window goes on while it works (#1291 step 3).
#[cfg(not(target_arch = "wasm32"))]
pub struct Job {
    /// What it is called, for its progress line.
    name: &'static str,
    /// Stops it, and counts its progress.
    control: std::sync::Arc<rustgis_engine::analysis::control::Control>,
    kind: JobKind,
}

#[cfg(not(target_arch = "wasm32"))]
enum JobKind {
    /// A built-in tool (Zonal Statistics), applied by `gp.finish` as a Tools pane run is.
    Tool { id: u64, slot: crate::Slot<rustgis_engine::cmd::gp::ToolDone> },
    /// A command that makes a raster, run again with what it made (`ran`).
    Raster { id: u64, command: &'static str, params: Value, slot: crate::Slot<Result<rustgis_engine::cmd::workflows::Made, String>> },
}

/// The Zonal Statistics section's inputs.
#[derive(Default)]
pub struct ZonalStatsForm {
    pub bands: String,
    pub prefix: String,
    pub output: String,
}

/// A Script pane run, as the line above its results tells it: its number since the project opened
/// (so a run made again by mistake shows), how long it took and when it started (seconds since
/// 1970; 0 for a clock that can't tell). Its lines' whole results, or why it didn't run, for its
/// Details window.
#[derive(Default)]
pub struct ScriptRun {
    pub n: u32,
    pub secs: f64,
    pub at: u64,
    pub ran: Vec<ScriptLine>,
    pub error: Option<String>,
}

/// A line a Script pane run ran, with its whole result. `text`, the result laid out to read a
/// line of text a row, is made the first time its Details section opens, not as the run lands.
pub struct ScriptLine {
    pub line: u64,
    pub command: String,
    pub result: Value,
    pub text: Vec<String>,
}

/// The Trend Analysis section's inputs (#579), and a run under way.
#[derive(Default)]
pub struct TrendForm {
    /// The cube: a stack of layers (`true`), or one layer (a Zarr variable, or a raster of bands).
    pub stack: bool,
    /// A cube of the catalog of data cubes instead (#683): its id and name. A STAC cube is read
    /// where the map is, a Zarr variable whole.
    pub cube: Option<(String, String)>,
    /// The layers ticked for a stack.
    pub ticked: Vec<String>,
    pub test: String,
    pub seasons: u32,
    pub alpha: String,
    pub decade: bool,
    pub start: String,
    pub end: String,
    pub months: String,
    pub min_valid: String,
    /// The dates typed, a layer's (or band's) each in their order; empty to read them from the
    /// names.
    pub dates: String,
    #[cfg(not(target_arch = "wasm32"))]
    pub running: Option<Trending>,
}

/// A trend running on a thread of its own: Cancel, the work done, and the result.
#[cfg(not(target_arch = "wasm32"))]
pub struct Trending {
    pub total: usize,
    /// Its place among the project's work under way: a trend that lands after another project
    /// opened is dropped.
    pub id: u64,
    pub params: Value,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub done: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub result: TrendResult,
}

#[cfg(not(target_arch = "wasm32"))]
type TrendResult = std::sync::Arc<std::sync::Mutex<Option<Result<(Vec<rustgis_engine::data::raster::Raster>, usize), String>>>>;

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tl!(text)).font(semibold(12.5)));
    ui.add_space(2.0);
}

fn combo(ui: &mut Ui, id: &str, name: &str, value: &mut String, options: &[String]) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(value.as_str())
        .width(180.0)
        .show_ui(ui, |ui| {
            for o in options {
                ui.selectable_value(value, o.clone(), o.as_str());
            }
        })
        .response
        .on_hover_text(tl!(name));
}

/// Runs a workflow's command: on the desktop the slow ones on a thread of their own
/// ([`background`]), the rest here.
fn run(app: &mut RustGisApp, id: &str, p: Value) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some((command, name)) = background(id) {
        start(app, command, name, p);
        return;
    }
    run_now(app, id, p);
}

/// Runs a workflow's command from outside the pane (the map's menu). On the desktop a slow one runs
/// in the background, the pane opened at `section` to show its progress, Cancel and result; one
/// that can't start (another is under way) is said in the status bar too. The rest run here, a
/// failure said in the status bar.
#[cfg_attr(target_arch = "wasm32", allow(unused_variables))]
pub fn run_elsewhere(app: &mut RustGisApp, section: &str, id: &str, p: Value) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some((command, name)) = background(id) {
        // Opened first: opening a section clears its last result and error.
        let _ = app.run("ui.pane.workflows", json!({"section": section}));
        start(app, command, name, p);
        if let Some(e) = app.workflow.error.clone() {
            app.session.notify(e, true);
        }
        return;
    }
    if let Err(e) = app.run(id, p) {
        app.session.notify(e, true);
    }
}

fn run_now(app: &mut RustGisApp, id: &str, p: Value) {
    let r = app.run(id, p);
    show(app, r);
}

/// Shows how a model's run from the Models section went, or why it didn't run, while that
/// section is open: in another, its result isn't that section's (a failure is said in the
/// status bar either way).
#[cfg(not(target_arch = "wasm32"))]
fn model_ran(app: &mut RustGisApp, landed: crate::Landed) {
    if app.workflow.section != "models" {
        app.map.invalidate();
        return;
    }
    match landed {
        crate::Landed::Ran(r) => show(app, r),
        crate::Landed::NotRun(why) => show(app, Err(why)),
    }
}

/// Shows how a workflow's command went, and redraws the map it may have changed.
fn show(app: &mut RustGisApp, r: Result<Value, String>) {
    match r {
        Ok(v) => {
            app.workflow.result = Some(v);
            app.workflow.error = None;
        }
        Err(e) => {
            app.workflow.error = Some(e);
            app.workflow.result = None;
        }
    }
    app.map.invalidate();
}

/// The workflows that run on a thread of their own on the desktop, as their command and name:
/// Zonal Statistics, and those that make a raster
/// ([`rustgis_engine::cmd::workflows::runs_in_background`]). Their work grows with the raster and
/// the points, and the parallel helpers spread it over the cores from that thread.
#[cfg(not(target_arch = "wasm32"))]
fn background(id: &str) -> Option<(&'static str, &'static str)> {
    match id {
        "gp.zonalStats" => Some(("gp.zonalStats", "Zonal Statistics")),
        _ => rustgis_engine::cmd::workflows::runs_in_background(id),
    }
}

/// Starts `command` on a thread of its own; [`land_job`] applies what it made. One at a time: a
/// second waits for the first to finish or be cancelled.
#[cfg(not(target_arch = "wasm32"))]
fn start(app: &mut RustGisApp, command: &'static str, name: &'static str, p: Value) {
    use rustgis_engine::cmd::{gp, workflows};
    if let Some(job) = &app.workflow.job {
        app.workflow.error = Some(crate::i18n::fmt("{tool} is still running: wait for it, or cancel it.", &[("tool", tl_raw!(job.name))]));
        return;
    }
    // Cancel stops it, so it takes no work cap.
    let control = rustgis_engine::analysis::control::Control::stoppable();
    let ctl = control.clone();
    let kind = if command == "gp.zonalStats" {
        gp::tool_start(&mut app.session, "zonalStats", &p).and_then(|run| {
            let (id, slot) = (run.id, crate::Slot::default());
            let out = slot.clone();
            spawn(name, move || fill(&out, run.run(&ctl)))
                .map(|()| JobKind::Tool { id, slot })
                .inspect_err(|_| gp::tool_abandon(&mut app.session, id))
        })
    } else {
        workflows::raster_start(&mut app.session, command, &p).and_then(|run| {
            let (id, slot) = (run.id, crate::Slot::default());
            let out = slot.clone();
            spawn(name, move || fill(&out, run.run(&ctl)))
                .map(|()| JobKind::Raster { id, command, params: p, slot })
                .inspect_err(|_| workflows::raster_abandon(&mut app.session, id))
        })
    };
    match kind {
        Ok(kind) => {
            app.workflow.job = Some(Job { name, control, kind });
            (app.workflow.result, app.workflow.error) = (None, None);
        }
        Err(e) => show(app, Err(e)),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn(name: &str, work: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new().name(name.into()).spawn(work).map(|_| ()).map_err(|e| e.to_string())
}

/// Fills a job's slot: even when its lock is poisoned, or the pane would wait for it forever.
#[cfg(not(target_arch = "wasm32"))]
fn fill<T>(slot: &crate::Slot<T>, v: T) {
    *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(v);
}

/// A workflow that has finished on its thread, applied (so it's in the History and can be undone):
/// called every frame, whatever pane is in view, so a run lands though the pane closed. `true`
/// while one is still running.
#[cfg(not(target_arch = "wasm32"))]
pub fn land_job(app: &mut RustGisApp) -> bool {
    use rustgis_engine::cmd::{gp, workflows};
    let Some(job) = &app.workflow.job else { return false };
    let ready = match &job.kind {
        JobKind::Tool { id, slot } => {
            // Its project was closed: stop it, as its result has nowhere to go, and say so.
            if !gp::tool_waiting(&app.session, *id) {
                job.control.cancel();
                let name = job.name;
                app.workflow.job = None;
                dropped(app, name);
                return false;
            }
            filled(slot)
        }
        JobKind::Raster { id, slot, .. } => {
            // Likewise: its project was closed, so the raster it is making has nowhere to go.
            if !workflows::raster_waiting(&app.session, *id) {
                job.control.cancel();
                let name = job.name;
                app.workflow.job = None;
                dropped(app, name);
                return false;
            }
            filled(slot)
        }
    };
    if !ready {
        return true;
    }
    let Some(job) = app.workflow.job.take() else { return false };
    match job.kind {
        JobKind::Tool { id, slot } => {
            let Some(done) = take(&slot) else { return false };
            let r = gp::tool_complete(&mut app.session, id, done).and_then(|_| app.finish_gp(id));
            show(app, r);
        }
        JobKind::Raster { id, command, params, slot } => match take(&slot) {
            // Added by its command, as if it had run here.
            Some(Ok(made)) => match workflows::raster_land(&mut app.session, id, made) {
                Ok(true) => {
                    let result = app.finish_raster(command, id, &params);
                    show(app, result);
                }
                Ok(false) => dropped(app, job.name),
                Err(e) => show(app, Err(e)),
            },
            Some(Err(e)) => {
                workflows::raster_abandon(&mut app.session, id);
                show(app, Err(e));
            }
            None => {}
        },
    }
    false
}

#[cfg(not(target_arch = "wasm32"))]
fn filled<T>(slot: &crate::Slot<T>) -> bool {
    slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some()
}

#[cfg(not(target_arch = "wasm32"))]
fn take<T>(slot: &crate::Slot<T>) -> Option<T> {
    slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
}

/// Says that a workflow's run was dropped: its project closed while it ran.
#[cfg(not(target_arch = "wasm32"))]
fn dropped(app: &mut RustGisApp, name: &'static str) {
    app.workflow.error = Some(crate::i18n::fmt("{tool} was stopped: its project was closed while it ran.", &[("tool", tl_raw!(name))]));
    app.workflow.result = None;
}

/// A workflow under way: its name, progress and Cancel.
#[cfg(not(target_arch = "wasm32"))]
fn job_running(app: &RustGisApp, ui: &mut Ui) {
    let Some(job) = &app.workflow.job else { return };
    ui.add_space(8.0);
    let mut cancel = false;
    ui.horizontal(|ui| {
        ui.spinner();
        // Two labels, not one sentence put together from translated pieces.
        ui.label(egui::RichText::new(tl!("Running…")).size(12.0));
        ui.label(egui::RichText::new(tl!(job.name)).size(12.0).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            cancel = ui.add_enabled(!job.control.is_cancelled(), egui::Button::new(tl!("Cancel"))).clicked();
        });
    });
    // How far its current step has come, when it counts its work.
    if let Some((done, total)) = job.control.progress() {
        ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).show_percentage().desired_height(14.0));
    }
    if cancel {
        job.control.cancel();
    }
}

fn num(s: &str, d: f64) -> f64 {
    s.trim().parse().unwrap_or(d)
}

/// The sections of the Workflows pane: (id, title).
pub const SECTIONS: &[(&str, &str)] = &[
    ("rasterFunctions", "Raster Functions"),
    ("functionEditor", "Function Editor"),
    ("suitability", "Suitability Modeler"),
    ("zonalStats", "Zonal Statistics"),
    ("visibility", "Visibility Analysis"),
    ("lineOfSight", "Exploratory 3D Analysis"),
    ("interpolate", "Geostatistical Wizard"),
    ("network", "Network Analysis"),
    ("neighborhood", "Neighborhood Explorer"),
    ("simulation", "Simulation"),
    ("profile", "Data Engineering"),
    ("explore", "Exploratory Analysis"),
    ("similar", "Embeddings"),
    ("interop", "Data Interop"),
    ("environments", "Environments"),
    ("models", "ModelBuilder"),
    ("georeference", "Georeference"),
    ("change", "Change Detection"),
    ("trend", "Trend Analysis"),
    ("spectral", "Spectral Analysis"),
    ("classify", "Classification Wizard"),
    ("supervised", "Classification Tools"),
    ("deepLearning", "Deep Learning Tools"),
    ("hosted", "Hosted Imagery"),
    ("indices", "Indices"),
    ("process", "Process"),
    ("pixels", "Pixel Editor"),
    ("mensuration", "Mensuration"),
    ("ortho", "Ortho Mapping"),
    ("reality", "Reality Mapping"),
];

/// Starts picking `needed` map points for a workflow.
fn pick_button(app: &mut RustGisApp, ui: &mut Ui, purpose: &'static str, needed: usize, label: &str) {
    let active = app.workflow.pick.as_ref().is_some_and(|p| p.purpose == purpose);
    if ui.selectable_label(active, tl!(label)).clicked() {
        app.workflow.pick = if active { None } else { Some(Pick { purpose, needed, points: Vec::new() }) };
    }
}

/// The picked points once a pick is complete (and clears it).
fn picked(app: &mut RustGisApp, purpose: &str) -> Option<Vec<rustgis_engine::geom::Coord>> {
    let done = app.workflow.pick.as_ref().is_some_and(|p| p.purpose == purpose && p.points.len() >= p.needed);
    if done { app.workflow.pick.take().map(|p| p.points) } else { None }
}

pub fn workflows(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    if app.workflow.section.is_empty() {
        app.workflow.section = "rasterFunctions";
    }
    egui::ComboBox::from_id_salt("wf_section")
        .selected_text(tl!(SECTIONS.iter().find(|s| s.0 == app.workflow.section).map_or("", |s| s.1)))
        .width(ui.available_width() - 8.0)
        .show_ui(ui, |ui| {
            for (id, title) in SECTIONS {
                if ui.selectable_label(app.workflow.section == *id, tl!(*title)).clicked() {
                    app.workflow.section = id;
                    app.workflow.result = None;
                    app.workflow.error = None;
                    app.workflow.pick = None;
                }
            }
        })
        .response
        .on_hover_text(tl!("Workflow"));
    let map = app.session.map();
    let rasters: Vec<String> = map.layers.iter().filter(|l| l.is_raster()).map(|l| l.name.clone()).collect();
    let features: Vec<String> = map.layers.iter().filter(|l| !l.is_raster() && !l.graphics).map(|l| l.name.clone()).collect();
    let by_type = |g: rustgis_engine::data::GeometryType| -> Vec<String> {
        map.layers.iter().filter(|l| !l.is_raster() && l.geometry_type() == g).map(|l| l.name.clone()).collect()
    };
    let (points, lines, polygons) = (
        by_type(rustgis_engine::data::GeometryType::Point),
        by_type(rustgis_engine::data::GeometryType::Polyline),
        by_type(rustgis_engine::data::GeometryType::Polygon),
    );
    let fields_of = |n: &str, numeric: bool| -> Vec<String> {
        map.layer_by_name(n)
            .map(|l| l.dataset.fields.iter().filter(|f| !numeric || f.ty.is_numeric()).map(|f| f.name.clone()).collect())
            .unwrap_or_default()
    };
    let numeric = fields_of(&app.workflow.layer, true);
    let w = &mut app.workflow;
    let inputs = match w.section {
        "rasterFunctions" | "functionEditor" | "suitability" | "visibility" | "lineOfSight" | "georeference" | "change" | "spectral"
        | "classify" | "supervised" | "deepLearning" | "hosted" | "indices" | "process" | "pixels" | "mensuration" | "reality"
        | "zonalStats"
        // The trend's one-layer mode takes a raster (a Zarr variable, or a raster of several bands).
        | "trend" => &rasters,
        "interpolate" => &points,
        "network" => &lines,
        "simulation" => &polygons,
        _ => &features,
    };
    if !inputs.contains(&w.layer) {
        w.layer = inputs.first().cloned().unwrap_or_default();
    }
    if (w.section == "zonalStats" || w.section == "process") && !polygons.contains(&w.layer2) {
        w.layer2 = polygons.first().cloned().unwrap_or_default();
    }
    ui.add_space(6.0);
    match app.workflow.section {
        "rasterFunctions" | "functionEditor" => {
            heading(ui, if app.workflow.section == "rasterFunctions" { "Apply a function to a raster" } else { "Chain functions" });
            combo(ui, "wf_r", "Layer", &mut app.workflow.layer, &rasters);
            let funcs: Vec<String> =
                ["slope", "aspect", "hillshade", "ndvi", "bandMath", "threshold"].iter().map(|s| s.to_string()).collect();
            if app.workflow.function.is_empty() {
                app.workflow.function = "hillshade".into();
            }
            ui.horizontal(|ui| {
                combo(ui, "wf_f", "Function", &mut app.workflow.function, &funcs);
                match app.workflow.function.as_str() {
                    "bandMath" => {
                        ui.add(egui::TextEdit::singleline(&mut app.workflow.text).hint_text("(b4-b3)/(b4+b3)").desired_width(120.0));
                    }
                    "threshold" => {
                        ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(tl!("value")).desired_width(70.0));
                    }
                    _ => {}
                }
            });
            let step = json!({"function": app.workflow.function, "expression": app.workflow.text, "value": num(&app.workflow.number, 0.0)});
            if app.workflow.section == "rasterFunctions" {
                if ui.button(tl!("Apply")).clicked() {
                    let mut p = step;
                    p["layer"] = json!(app.workflow.layer);
                    run(app, "raster.function", p);
                }
            } else {
                ui.horizontal(|ui| {
                    if ui.button(tl!("Add Step")).clicked() {
                        app.workflow.chain.push(step.to_string());
                    }
                    if ui.button(tl!("Clear")).clicked() {
                        app.workflow.chain.clear();
                    }
                });
                for (i, st) in app.workflow.chain.iter().enumerate() {
                    let v: Value = serde_json::from_str(st).unwrap_or_default();
                    ui.label(egui::RichText::new(format!("{}. {}", i + 1, v["function"].as_str().unwrap_or(""))).small());
                }
                if !app.workflow.chain.is_empty() && ui.button(tl!("Run Chain")).clicked() {
                    let steps: Vec<Value> = app.workflow.chain.iter().filter_map(|s| serde_json::from_str(s).ok()).collect();
                    run(app, "raster.functionChain", json!({"layer": app.workflow.layer, "steps": steps}));
                }
            }
        }
        "suitability" => {
            heading(ui, "Score two rasters 1–10 and weight them");
            ui.label(egui::RichText::new(tl!("High values of the first raster and low values of the second score best.")).small().weak());
            combo(ui, "wf_s1", "First raster", &mut app.workflow.layer, &rasters);
            combo(ui, "wf_s2", "Second raster", &mut app.workflow.layer2, &rasters);
            ui.horizontal(|ui| {
                let input_label = ui.label(tl!("Weights"));
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text("1").desired_width(40.0)).labelled_by(input_label.id);
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number2).hint_text("1").desired_width(40.0))
                    .labelled_by(input_label.id);
            });
            if ui.button(tl!("Run")).clicked() {
                let crit = |app: &RustGisApp, n: &str, high: bool, w: f64| -> Value {
                    let st = app
                        .session
                        .map()
                        .layer_by_name(n)
                        .and_then(|l| l.raster.as_ref())
                        .and_then(|r| r.data.as_ref())
                        .and_then(|d| d.bands.first())
                        .map(|b| (b.stats.min, b.stats.max))
                        .unwrap_or((0.0, 1.0));
                    let step = (st.1 - st.0) / 10.0;
                    let ranges: Vec<Value> = (0..10)
                        .map(|i| {
                            let lo = st.0 + step * i as f64;
                            let hi = if i == 9 { f64::MAX } else { lo + step };
                            json!([if i == 0 { f64::MIN } else { lo }, hi, if high { i + 1 } else { 10 - i }])
                        })
                        .collect();
                    json!({"layer": n, "ranges": ranges, "weight": w})
                };
                let c1 = crit(app, &app.workflow.layer, true, num(&app.workflow.number, 1.0));
                let c2 = crit(app, &app.workflow.layer2, false, num(&app.workflow.number2, 1.0));
                run(app, "analysis.suitability", json!({"criteria": [c1, c2]}));
            }
        }
        "zonalStats" => {
            heading(ui, "Calculate raster statistics per polygon");
            ui.label(egui::RichText::new(tl!("Raster layer")).small().weak());
            combo(ui, "wf_zs_raster", "Raster layer", &mut app.workflow.layer, &rasters);
            ui.label(egui::RichText::new(tl!("Polygon layer")).small().weak());
            combo(ui, "wf_zs_poly", "Polygon layer", &mut app.workflow.layer2, &polygons);
            if app.session.map().layer_by_name(&app.workflow.layer2).is_some_and(|l| !l.selection.is_empty()) {
                ui.label(egui::RichText::new(tl!("Only selected polygons will be included in the output.")).small().weak());
            }
            ui.horizontal(|ui| {
                let band_label = ui.label(tl!("Band"));
                ui.add(egui::TextEdit::singleline(&mut app.workflow.zonal_stats.bands).hint_text("1").desired_width(40.0))
                    .labelled_by(band_label.id);
                let prefix_label = ui.label(tl!("Prefix"));
                ui.add(egui::TextEdit::singleline(&mut app.workflow.zonal_stats.prefix).hint_text("elev").desired_width(70.0))
                    .labelled_by(prefix_label.id);
            });
            ui.horizontal(|ui| {
                let output_label = ui.label(tl!("Output"));
                ui.add(
                    egui::TextEdit::singleline(&mut app.workflow.zonal_stats.output)
                        .hint_text(tl!("output layer name"))
                        .desired_width(120.0),
                )
                .labelled_by(output_label.id);
            });
            let band_str = app.workflow.zonal_stats.bands.trim();
            let raster_data =
                app.session.map().layer_by_name(&app.workflow.layer).and_then(|l| l.raster.as_ref()).and_then(|r| r.data.as_ref());
            let total_bands = raster_data.map_or(0, |d| d.band_count());
            let raster_ready = raster_data.is_some();
            let band_res = rustgis_engine::analysis::zonal_bands(&json!(band_str), total_bands);
            let band_valid = band_res.is_ok();
            let band_err = band_res.err();
            let layers_ready = !app.workflow.layer.is_empty() && !app.workflow.layer2.is_empty();
            let can_run = layers_ready && raster_ready && band_valid;
            if !layers_ready {
                ui.label(egui::RichText::new(tl!("Add a raster and a polygon layer to calculate zonal statistics.")).small().weak());
            } else if !raster_ready {
                ui.label(egui::RichText::new(tl!("Raster data is unavailable.")).small().color(t.amber));
            } else if band_err.is_some() {
                ui.label(egui::RichText::new(tl!("Band must be a positive integer, list of bands, or 'all'.")).small().color(t.amber));
                ui.label(
                    egui::RichText::new(crate::i18n::fmt("Raster has bands 1 to {count}.", &[("count", &total_bands.to_string())]))
                        .small()
                        .color(t.amber),
                );
            }
            if ui.add_enabled(can_run, egui::Button::new(tl!("Calculate Statistics"))).clicked() {
                let mut params = json!({
                    "raster": app.workflow.layer,
                    "polygons": app.workflow.layer2,
                });
                if !band_str.is_empty() {
                    params["bands"] = json!(band_str);
                }
                if !app.workflow.zonal_stats.prefix.trim().is_empty() {
                    params["prefix"] = json!(app.workflow.zonal_stats.prefix.trim());
                }
                if !app.workflow.zonal_stats.output.trim().is_empty() {
                    params["output"] = json!(app.workflow.zonal_stats.output.trim());
                }
                run(app, "gp.zonalStats", params);
            }
        }
        "visibility" => {
            heading(ui, "Viewshed from a point");
            combo(ui, "wf_v", "Layer", &mut app.workflow.layer, &rasters);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(tl!("observer height m")).desired_width(110.0));
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number2).hint_text(tl!("radius m")).desired_width(80.0));
            });
            pick_button(app, ui, "viewshed", 1, "Click the observer on the map");
            if let Some(p) = picked(app, "viewshed").and_then(|v| v.first().copied()) {
                let mut params = json!({"layer": app.workflow.layer, "lon": p.x, "lat": p.y, "height": num(&app.workflow.number, 1.7)});
                if let Ok(r) = app.workflow.number2.trim().parse::<f64>() {
                    params["radius"] = json!(r);
                }
                run(app, "analysis.viewshed", params);
            }
        }
        "lineOfSight" => {
            heading(ui, "Line of sight between two points");
            combo(ui, "wf_l", "Layer", &mut app.workflow.layer, &rasters);
            pick_button(app, ui, "los", 2, "Click the observer, then the target");
            if let Some(p) = picked(app, "los")
                && let (Some(a), Some(b)) = (p.first(), p.get(1))
            {
                run(app, "analysis.lineOfSight", json!({"layer": app.workflow.layer, "from": [a.x, a.y], "to": [b.x, b.y]}));
            }
        }
        "interpolate" => {
            heading(ui, "Interpolate a surface from points (IDW)");
            combo(ui, "wf_i", "Layer", &mut app.workflow.layer, &points);
            combo(ui, "wf_if", "Field", &mut app.workflow.field, &numeric);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(tl!("cell size °")).desired_width(80.0));
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number2).hint_text(tl!("power 2")).desired_width(60.0));
            });
            if ui.button(tl!("Run")).clicked() {
                let mut p = json!({"layer": app.workflow.layer, "field": app.workflow.field, "power": num(&app.workflow.number2, 2.0)});
                if let Ok(c) = app.workflow.number.trim().parse::<f64>() {
                    p["cellSize"] = json!(c);
                }
                run(app, "analysis.interpolate", p);
            }
        }
        "network" => {
            heading(ui, "Shortest route over a line network");
            combo(ui, "wf_n", "Layer", &mut app.workflow.layer, &lines);
            let cost_fields = fields_of(&app.workflow.layer, true);
            ui.horizontal(|ui| {
                ui.label(tl!("Cost"));
                let mut options = vec![String::new()];
                options.extend(cost_fields);
                combo(ui, "wf_nc", "Cost", &mut app.workflow.field, &options);
            });
            pick_button(app, ui, "route", 2, "Click the start, then the end");
            if let Some(p) = picked(app, "route")
                && let (Some(a), Some(b)) = (p.first(), p.get(1))
            {
                let mut params = json!({"layer": app.workflow.layer, "from": [a.x, a.y], "to": [b.x, b.y], "snapDistance": 50_000});
                if !app.workflow.field.is_empty() {
                    params["costField"] = json!(app.workflow.field);
                }
                run(app, "analysis.route", params);
            }
        }
        "neighborhood" => {
            heading(ui, "What's near a point");
            ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(tl!("radius km")).desired_width(80.0));
            pick_button(app, ui, "near", 1, "Click a point on the map");
            if let Some(p) = picked(app, "near").and_then(|v| v.first().copied()) {
                run(
                    app,
                    "analysis.neighborhood",
                    json!({"lon": p.x, "lat": p.y, "radius": num(&app.workflow.number, 100.0) * 1000.0, "select": true}),
                );
            }
        }
        "simulation" => {
            heading(ui, "Random points in polygons");
            combo(ui, "wf_sim", "Layer", &mut app.workflow.layer, &polygons);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(tl!("points each")).desired_width(80.0));
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number2).hint_text(tl!("seed")).desired_width(60.0));
            });
            if ui.button(tl!("Run")).clicked() {
                run(
                    app,
                    "analysis.simulate",
                    json!({"layer": app.workflow.layer, "count": num(&app.workflow.number, 100.0), "seed": num(&app.workflow.number2, 42.0)}),
                );
            }
        }
        "profile" | "explore" => {
            heading(ui, if app.workflow.section == "profile" { "Profile every field" } else { "Histograms and correlations" });
            combo(ui, "wf_p", "Layer", &mut app.workflow.layer, &features);
            let cmd = if app.workflow.section == "profile" { "data.profile" } else { "data.explore" };
            if ui.button(tl!("Run")).clicked() {
                run(app, cmd, json!({"layer": app.workflow.layer}));
            }
        }
        "similar" => {
            heading(ui, "Features most like the selected one");
            combo(ui, "wf_e", "Layer", &mut app.workflow.layer, &features);
            if ui.button(tl!("Find Similar")).clicked() {
                run(app, "data.similar", json!({"layer": app.workflow.layer, "count": 10}));
            }
        }
        "interop" => {
            heading(ui, "Convert a layer to another format");
            combo(ui, "wf_x", "Layer", &mut app.workflow.layer, &features);
            let formats: Vec<String> = ["kml", "gpx", "wkt", "geojson", "csv"].iter().map(|s| s.to_string()).collect();
            if app.workflow.format.is_empty() {
                app.workflow.format = "kml".into();
            }
            combo(ui, "wf_xf", "Format", &mut app.workflow.format, &formats);
            if ui.button(tl!("Convert…")).clicked() {
                match app.session.run("data.convert", &json!({"layer": app.workflow.layer, "format": app.workflow.format})) {
                    Ok(v) => {
                        let ext = if app.workflow.format == "wkt" { "csv".to_string() } else { app.workflow.format.clone() };
                        let name = format!("{}.{ext}", app.workflow.layer);
                        let bytes = v["text"].as_str().unwrap_or("").as_bytes().to_vec();
                        if app.services.download.is_some() {
                            app.download_bytes(&name, bytes);
                        } else if let Some(path) = crate::dialogs::pick_save(&name, &[ext.as_str()], tl!("Converted data")) {
                            let r = rustgis_engine::data::io::write_atomic(std::path::Path::new(&path), &bytes).map_err(|e| e.to_string());
                            app.workflow.error = r.err();
                        }
                    }
                    Err(e) => app.workflow.error = Some(e),
                }
            }
        }
        "environments" => {
            heading(ui, "Where tools work");
            let env = app.session.project.gp_env.clone();
            let shown = env
                .extent
                .map(|e| format!("{:.2}, {:.2} – {:.2}, {:.2}", e.min_x, e.min_y, e.max_x, e.max_y))
                .unwrap_or_else(|| tl!("Everywhere").to_string());
            ui.label(egui::RichText::new(crate::i18n::fmt("Extent: {extent}", &[("extent", &shown)])).small());
            ui.horizontal_wrapped(|ui| {
                for (label, v) in [("Map View", json!("view")), ("Full Extent", json!("full")), ("Everywhere", Value::Null)] {
                    if ui.button(tl!(label)).clicked() {
                        run(app, "gp.env", json!({"extent": v}));
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(tl!("cell size °")).desired_width(80.0));
                if ui.button(tl!("Set Cell Size")).clicked() {
                    let v = app.workflow.number.trim().parse::<f64>().ok();
                    run(app, "gp.env", json!({"cellSize": v}));
                }
            });
        }
        "georeference" => {
            heading(ui, "Place a raster on the map");
            combo(ui, "wf_g", "Layer", &mut app.workflow.layer, &rasters);
            ui.label(
                egui::RichText::new(tl!("Stretch it over the map view, or give control points (one per line: column row lon lat)."))
                    .small()
                    .weak(),
            );
            if ui.button(tl!("Fit To Map View")).clicked() {
                let m = app.session.map();
                let (w, h) = app.session.view_size;
                let e = m.camera.extent(w, h);
                let (a, b) = (
                    m.to_lonlat(rustgis_engine::geom::Coord::new(e.min_x, e.min_y)),
                    m.to_lonlat(rustgis_engine::geom::Coord::new(e.max_x, e.max_y)),
                );
                if let (Some(a), Some(b)) = (a, b) {
                    run(app, "raster.georeference", json!({"layer": app.workflow.layer, "extent": [a.x, a.y, b.x, b.y]}));
                }
            }
            ui.add(
                egui::TextEdit::multiline(&mut app.workflow.text)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("0 0 23.70 38.00\n512 512 23.80 37.90"),
            );
            if ui.button(tl!("Apply Control Points")).clicked() {
                // Every non-blank line must be four numbers; a typo is reported, not skipped.
                let mut pts: Vec<Vec<f64>> = Vec::new();
                let mut bad = None;
                for (i, l) in app.workflow.text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
                    let v: Vec<f64> = l.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                    if v.len() == 4 && l.split_whitespace().count() == 4 {
                        pts.push(v);
                    } else {
                        bad = Some(i + 1);
                        break;
                    }
                }
                match bad {
                    Some(line) => {
                        app.workflow.error =
                            Some(crate::i18n::fmt("Line {n} isn't four numbers (column row lon lat)", &[("n", &line.to_string())]));
                    }
                    None => run(app, "raster.georeference", json!({"layer": app.workflow.layer, "points": pts})),
                }
            }
        }
        "change" => {
            heading(ui, "Compare two rasters");
            combo(ui, "wf_c1", "Before", &mut app.workflow.layer, &rasters);
            combo(ui, "wf_c2", "After", &mut app.workflow.layer2, &rasters);
            let methods: Vec<String> = ["difference", "ratio", "percent"].iter().map(|s| s.to_string()).collect();
            if app.workflow.format.is_empty() || !methods.contains(&app.workflow.format) {
                app.workflow.format = "difference".into();
            }
            combo(ui, "wf_cm", "Method", &mut app.workflow.format, &methods);
            if ui.button(tl!("Run")).clicked() {
                run(
                    app,
                    "raster.change",
                    json!({"before": app.workflow.layer, "after": app.workflow.layer2, "method": app.workflow.format}),
                );
            }
        }
        "trend" => trend(app, ui, &rasters),
        "spectral" => {
            heading(ui, "Band values at a point");
            combo(ui, "wf_sp", "Layer", &mut app.workflow.layer, &rasters);
            pick_button(app, ui, "spectral", 1, "Click a pixel on the map");
            if let Some(p) = picked(app, "spectral").and_then(|v| v.first().copied()) {
                run(app, "raster.spectral", json!({"layer": app.workflow.layer, "lon": p.x, "lat": p.y}));
            }
        }
        "classify" => {
            heading(ui, "Unsupervised classes (k-means)");
            combo(ui, "wf_k", "Layer", &mut app.workflow.layer, &rasters);
            ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(tl!("classes")).desired_width(60.0));
            if ui.button(tl!("Classify")).clicked() {
                run(app, "raster.classify", json!({"layer": app.workflow.layer, "classes": num(&app.workflow.number, 5.0).round() as u64}));
            }
        }
        "supervised" => {
            heading(ui, "Classify from training polygons");
            combo(ui, "wf_su", "Raster", &mut app.workflow.layer, &rasters);
            combo(ui, "wf_st", "Training polygons", &mut app.workflow.layer2, &polygons);
            let fields = fields_of(&app.workflow.layer2, false);
            combo(ui, "wf_sf", "Field", &mut app.workflow.field, &fields);
            if ui.button(tl!("Classify")).clicked() {
                run(
                    app,
                    "raster.classifySupervised",
                    json!({"layer": app.workflow.layer, "training": app.workflow.layer2, "classField": app.workflow.field}),
                );
            }
        }
        "deepLearning" => {
            heading(ui, "Train a neural network on training polygons");
            combo(ui, "wf_dl", "Raster", &mut app.workflow.layer, &rasters);
            combo(ui, "wf_dt", "Training polygons", &mut app.workflow.layer2, &polygons);
            let fields = fields_of(&app.workflow.layer2, false);
            combo(ui, "wf_df", "Field", &mut app.workflow.field, &fields);
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(tl!("epochs")).desired_width(60.0));
                if ui.button(tl!("Train And Classify")).clicked() {
                    let epochs = app.workflow.number.trim().parse::<f64>().unwrap_or(60.0);
                    run(
                        app,
                        "dl.classify",
                        json!({"layer": app.workflow.layer, "training": app.workflow.layer2, "classField": app.workflow.field, "epochs": epochs}),
                    );
                }
            });
            let models: Vec<(String, f64)> = app.session.project.dl_models.iter().map(|m| (m.name.clone(), m.accuracy)).collect();
            if !models.is_empty() {
                heading(ui, "Trained models");
            }
            for (m, acc) in models {
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::fmt("{name} ({acc}% accurate)", &[("name", &m), ("acc", &format!("{:.0}", acc * 100.0))]));
                    if ui.small_button(tl!("Classify")).clicked() {
                        run(app, "dl.classify", json!({"layer": app.workflow.layer, "model": m}));
                    }
                    if ui.small_button(tl!("Package")).clicked() {
                        let _ = app.run("ui.saveItem", json!({"kind": "dlPackage", "model": m}));
                    }
                });
            }
            if ui.button(tl!("Import Package…")).clicked() {
                let _ = app.run("ui.openItem", json!({"kind": "dlPackage"}));
            }
        }
        "hosted" => {
            heading(ui, "Cut a raster into web map tiles");
            combo(ui, "wf_ho", "Layer", &mut app.workflow.layer, &rasters);
            if ui.button(tl!("Create Hosted Imagery…")).clicked() {
                let _ = app.run("ui.saveItem", json!({"kind": "hostedImagery", "layer": app.workflow.layer}));
            }
        }
        "indices" => {
            heading(ui, "Spectral indices");
            combo(ui, "wf_ix", "Layer", &mut app.workflow.layer, &rasters);
            let input_label = ui.label(egui::RichText::new(tl!("Bands: blue green red NIR SWIR")).small().weak());
            ui.add(egui::TextEdit::singleline(&mut app.workflow.text).hint_text("1 2 3 4 5").desired_width(120.0))
                .labelled_by(input_label.id);
            ui.horizontal_wrapped(|ui| {
                for idx in ["ndvi", "ndwi", "savi", "nbr", "ndbi", "evi"] {
                    if ui.button(idx.to_uppercase()).clicked() {
                        let b: Vec<u64> = app.workflow.text.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                        let g = |i: usize, d: u64| b.get(i).copied().unwrap_or(d);
                        run(app, "raster.index", json!({"layer": app.workflow.layer, "index": idx, "blue": g(0, 1), "green": g(1, 2), "red": g(2, 3), "nir": g(3, 4), "swir": g(4, 5)}));
                    }
                }
            });
        }
        "process" => {
            heading(ui, "Clip or resample");
            combo(ui, "wf_pr", "Raster layer", &mut app.workflow.layer, &rasters);

            ui.add_space(4.0);
            ui.label(egui::RichText::new(tl!("Clip to polygon layer")).small().weak());
            let has_polys = !polygons.is_empty();
            if has_polys {
                let layer2_before = app.workflow.layer2.clone();
                combo(ui, "wf_pr_poly", "Polygon layer", &mut app.workflow.layer2, &polygons);
                if app.workflow.layer2 != layer2_before {
                    app.workflow.selected_only = None;
                }
                let poly_layer = app.session.map().layer_by_name(&app.workflow.layer2);
                let sel_count = poly_layer.as_ref().map_or(0, |l| l.selection.len());
                let total_features = poly_layer.as_ref().map_or(0, |l| l.dataset.features.len());

                let effective_selected_only = app.workflow.selected_only.unwrap_or(sel_count > 0);
                let mut checked = effective_selected_only;
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut checked, tl!("Selected only")).changed() {
                        app.workflow.selected_only = Some(checked);
                    }
                    if sel_count > 0 {
                        let text = crate::i18n::fmt(
                            "({selected} of {total})",
                            &[("selected", &sel_count.to_string()), ("total", &total_features.to_string())],
                        );
                        ui.label(egui::RichText::new(text).small().weak());
                    }
                });

                let sel_invalid = effective_selected_only && sel_count == 0;
                let can_clip_poly = !app.workflow.layer.is_empty() && !app.workflow.layer2.is_empty() && !sel_invalid;

                if sel_invalid {
                    ui.label(egui::RichText::new(tl!("No features are selected in this layer.")).small().color(t.amber));
                }

                if ui.add_enabled(can_clip_poly, egui::Button::new(tl!("Clip To Polygon"))).clicked() {
                    let mut p = json!({
                        "layer": app.workflow.layer,
                        "operation": "clip",
                        "polygon": app.workflow.layer2,
                    });
                    p["selectedOnly"] = json!(effective_selected_only);
                    run(app, "raster.process", p);
                }
            } else {
                ui.label(egui::RichText::new(tl!("No polygon layer available in the map.")).small().weak());
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(!app.workflow.layer.is_empty(), egui::Button::new(tl!("Clip To Map View"))).clicked() {
                    run(app, "raster.process", json!({"layer": app.workflow.layer, "operation": "clip"}));
                }
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let factor_label = ui.label(tl!("factor"));
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(tl!("factor")).desired_width(50.0))
                    .labelled_by(factor_label.id);
                if ui.add_enabled(!app.workflow.layer.is_empty(), egui::Button::new(tl!("Resample"))).clicked() {
                    run(
                        app,
                        "raster.process",
                        json!({"layer": app.workflow.layer, "operation": "resample", "factor": num(&app.workflow.number, 2.0).round() as u64}),
                    );
                }
            });
        }
        "pixels" => {
            heading(ui, "Set the pixels inside a polygon");
            combo(ui, "wf_px", "Layer", &mut app.workflow.layer, &rasters);
            ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(tl!("value (empty: no-data)")).desired_width(140.0));
            pick_button(app, ui, "pixels", 100, "Click the polygon's corners");
            let n = app.workflow.pick.as_ref().filter(|p| p.purpose == "pixels").map_or(0, |p| p.points.len());
            if n >= 3 && ui.button(tl!("Apply")).clicked() {
                let pts: Vec<[f64; 2]> =
                    app.workflow.pick.take().map(|p| p.points.iter().map(|c| [c.x, c.y]).collect()).unwrap_or_default();
                let mut params = json!({"layer": app.workflow.layer, "polygon": pts});
                if let Ok(v) = app.workflow.number.trim().parse::<f64>() {
                    params["value"] = json!(v);
                }
                run(app, "raster.editPixels", params);
            }
        }
        "mensuration" => {
            heading(ui, "Measure with an elevation raster");
            combo(ui, "wf_me", "Layer", &mut app.workflow.layer, &rasters);
            ui.horizontal_wrapped(|ui| {
                pick_button(app, ui, "m_distance", 2, "Distance");
                pick_button(app, ui, "m_height", 2, "Height");
                pick_button(app, ui, "m_area", 100, "Area");
                pick_button(app, ui, "m_volume", 100, "Volume");
            });
            for (purpose, kind) in [("m_distance", "distance"), ("m_height", "height")] {
                if let Some(p) = picked(app, purpose) {
                    let pts: Vec<[f64; 2]> = p.iter().map(|c| [c.x, c.y]).collect();
                    run(app, "raster.measure", json!({"layer": app.workflow.layer, "kind": kind, "points": pts}));
                }
            }
            let poly =
                app.workflow.pick.as_ref().filter(|p| matches!(p.purpose, "m_area" | "m_volume") && p.points.len() >= 3).map(|p| p.purpose);
            if let Some(purpose) = poly
                && ui.button(tl!("Finish Polygon")).clicked()
            {
                let pts: Vec<[f64; 2]> =
                    app.workflow.pick.take().map(|p| p.points.iter().map(|c| [c.x, c.y]).collect()).unwrap_or_default();
                let kind = if purpose == "m_area" { "area" } else { "volume" };
                run(app, "raster.measure", json!({"layer": app.workflow.layer, "kind": kind, "points": pts}));
            }
            // Read directly: a command each frame would mark the map changed.
            let results = app.session.measurements.clone();
            if !results.is_empty() {
                heading(ui, "Results");
                for r in results.iter().rev().take(10) {
                    ui.label(egui::RichText::new(r.to_string()).small().monospace());
                }
            }
        }
        "ortho" | "reality" => {
            let kind = app.workflow.section;
            heading(
                ui,
                if kind == "ortho" { "Mosaic images into an orthomosaic" } else { "Build a surface model and mesh from elevation rasters" },
            );
            let ws: Vec<(String, usize)> =
                app.session.project.workspaces.iter().filter(|w| w.kind == kind).map(|w| (w.name.clone(), w.images.len())).collect();
            for (n, k) in ws {
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::fmt("{name} ({n} images)", &[("name", &n), ("n", &k.to_string())]));
                    if ui.small_button(tl!("Build")).clicked() {
                        run(app, "imagery.mosaic", json!({"name": n}));
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(tl!("cell size °")).desired_width(80.0));
                if ui.button(tl!("New Workspace From Visible Rasters")).clicked() {
                    let images: Vec<String> =
                        app.session.map().layers.iter().filter(|l| l.is_raster() && l.visible).map(|l| l.name.clone()).collect();
                    let mut p = json!({"kind": kind, "images": images});
                    if let Ok(c) = app.workflow.number.trim().parse::<f64>() {
                        p["cellSize"] = json!(c);
                    }
                    run(app, "imagery.workspace", p);
                }
            });
            if kind == "reality" {
                combo(ui, "wf_mesh", "Layer", &mut app.workflow.layer, &rasters);
                if ui.button(tl!("Export Mesh (OBJ)…")).clicked() {
                    match app.session.run("imagery.mesh", &json!({"layer": app.workflow.layer})) {
                        Ok(v) => {
                            let name = format!("{}.obj", app.workflow.layer);
                            let bytes = v["text"].as_str().unwrap_or("").as_bytes().to_vec();
                            if app.services.download.is_some() {
                                app.download_bytes(&name, bytes);
                            } else if let Some(path) = crate::dialogs::pick_save(&name, &["obj"], tl!("3D mesh")) {
                                app.workflow.error = rustgis_engine::data::io::write_atomic(std::path::Path::new(&path), &bytes)
                                    .err()
                                    .map(|e| e.to_string());
                            }
                        }
                        Err(e) => app.workflow.error = Some(e),
                    }
                }
            }
        }
        _ => {
            heading(ui, "Models");
            let models: Vec<(String, usize)> = app.session.project.models.iter().map(|m| (m.name.clone(), m.steps.len())).collect();
            if ui
                .button(tl!("Open ModelBuilder"))
                .on_hover_text(tl!("Draw a model: chain tools from input layers to the results to keep"))
                .clicked()
            {
                let _ = app.run("ui.modelBuilder", Value::Null);
            }
            for (n, steps) in models {
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::fmt("{name} ({n} steps)", &[("name", &n), ("n", &steps.to_string())]));
                    if ui.small_button(tl!("Open")).on_hover_text(tl!("Open in ModelBuilder")).clicked() {
                        let _ = app.run("ui.modelBuilder", json!({"name": n}));
                    }
                    // On the desktop its tools run on a thread of their own, the status bar
                    // showing it and Cancel; its result shows here once it lands.
                    if ui.small_button(tl!("Run")).clicked() {
                        (app.workflow.result, app.workflow.error) = (None, None);
                        #[cfg(not(target_arch = "wasm32"))]
                        let _ = crate::readjob::run_then(app, "model.run", json!({"name": n}), Some(model_ran));
                        #[cfg(target_arch = "wasm32")]
                        run_now(app, "model.run", json!({"name": n}));
                    }
                    if ui.small_button("×").clicked() {
                        run(app, "model.remove", json!({"name": n}));
                    }
                });
            }
            if ui
                .button(tl!("New Model From History"))
                .on_hover_text(tl!("Each geoprocessing run becomes a step, chained by its outputs"))
                .clicked()
            {
                match app.run("model.new", json!({"name": "Model", "fromHistory": true})) {
                    Ok(v) => {
                        let name = v["model"].as_str().unwrap_or_default().to_string();
                        let _ = app.run("ui.modelBuilder", json!({"name": name}));
                    }
                    Err(e) => app.workflow.error = Some(e),
                }
            }
        }
    }
    if let Some(p) = &app.workflow.pick {
        ui.label(
            egui::RichText::new(crate::i18n::fmt(
                "Click the map: {n} of {total}",
                &[("n", &p.points.len().to_string()), ("total", &p.needed.to_string())],
            ))
            .color(t.accent),
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    job_running(app, ui);
    if let Some(e) = &app.workflow.error {
        ui.label(egui::RichText::new(e.as_str()).color(egui::Color32::from_rgb(0xC0, 0x30, 0x30)).small());
    }
    if let Some(r) = app.workflow.result.clone() {
        result_view(ui, &r, t);
    }
}

/// Whether the catalog's cube `id` is a STAC collection, read where the map is (a Zarr variable is
/// read whole).
fn stac_cube(id: &str) -> bool {
    use rustgis_engine::render::rustgis_tiles::cubes;
    cubes::find(id).is_ok_and(|e| matches!(e.source, cubes::Source::Stac { .. }))
}

/// The Trend Analysis section (opengeos/RustGIS#579): a cube (raster layers a date each, a Zarr
/// variable, or a raster's bands), Sen's slope and the modified Mann–Kendall test, worked out on a
/// thread with progress and Cancel, and added as four layers.
fn trend(app: &mut RustGisApp, ui: &mut Ui, rasters: &[String]) {
    #[cfg(not(target_arch = "wasm32"))]
    if trend_running(app, ui) {
        return;
    }
    heading(ui, "Trend over a series of rasters");
    let f = &mut app.workflow.trend;
    if f.test.is_empty() {
        (f.test, f.seasons, f.alpha) = ("hamedRao".into(), 12, "0.05".into());
    }
    if let Some((id, name)) = f.cube.clone() {
        // A catalog's cube (Home › Web Data › Data Cubes).
        ui.horizontal_wrapped(|ui| {
            let args = [("name", name.as_str())];
            ui.label(if stac_cube(&id) {
                crate::i18n::fmt("Data cube: {name}, where the map is", &args)
            } else {
                crate::i18n::fmt("Data cube: {name}, read whole", &args)
            });
            if ui.small_button(tl!("Use layers instead")).clicked() {
                f.cube = None;
            }
        });
    } else {
        let was_stack = f.stack;
        ui.horizontal(|ui| {
            ui.radio_value(&mut f.stack, true, tl!("Layers, a date each"));
            ui.radio_value(&mut f.stack, false, tl!("One layer: a Zarr variable or a raster's bands"));
        });
        // Dates typed for one kind of series don't carry over to the other.
        if f.stack != was_stack {
            f.dates.clear();
        }
    }
    if f.cube.is_some() {
        // Read from the catalog's cube: no layers to pick.
    } else if f.stack {
        f.ticked.retain(|n| rasters.contains(n));
        ui.horizontal(|ui| {
            if ui.small_button(tl!("All")).clicked() {
                f.ticked = rasters.to_vec();
            }
            if ui.small_button(tl!("None")).clicked() {
                f.ticked.clear();
            }
        });
        egui::ScrollArea::vertical().id_salt("trend_layers").max_height(160.0).show(ui, |ui| {
            for n in rasters {
                let mut on = f.ticked.contains(n);
                let date = rustgis_engine::cmd::cube::layer_date(&app.session, n)
                    .map_or_else(|| tl!("no date in the name").to_string(), |d| rustgis_engine::data::time::format(d, "YYYY-MM-DD"));
                if ui.checkbox(&mut on, format!("{n}  ({date})")).changed() {
                    if on {
                        f.ticked.push(n.clone());
                    } else {
                        f.ticked.retain(|x| x != n);
                    }
                }
            }
        });
    } else {
        combo(ui, "wf_tr_layer", "Layer", &mut app.workflow.layer, rasters);
    }
    let f = &mut app.workflow.trend;
    egui::Grid::new("trend_grid").num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
        // A catalog's cube takes its dates from its items or its store.
        if f.cube.is_none() {
            let input_label = ui.label(tl!("Dates"));
            ui.add(egui::TextEdit::singleline(&mut f.dates).hint_text(tl!("from the names")).desired_width(240.0))
                .labelled_by(input_label.id);
            ui.end_row();
        }
        let input_label = ui.label(tl!("Test"));
        let tests = [
            ("hamedRao", "Modified Mann–Kendall (Hamed & Rao)"),
            ("yueWang", "Modified Mann–Kendall (Yue & Wang)"),
            ("seasonal", "Seasonal Mann–Kendall"),
            ("original", "Mann–Kendall"),
        ];
        let shown = tests.iter().find(|t| t.0 == f.test).map_or("", |t| t.1);
        egui::ComboBox::from_id_salt("trend_test")
            .selected_text(tl!(shown))
            .width(240.0)
            .show_ui(ui, |ui| {
                for (code, label) in tests {
                    ui.selectable_value(&mut f.test, code.to_string(), tl!(label));
                }
            })
            .response
            .labelled_by(input_label.id);
        ui.end_row();
        if f.test == "seasonal" {
            let input_label = ui.label(tl!("Seasons a year"));
            ui.add(egui::DragValue::new(&mut f.seasons).range(2..=366)).labelled_by(input_label.id);
            ui.end_row();
        }
        let input_label = ui.label(tl!("Significance level"));
        ui.add(egui::TextEdit::singleline(&mut f.alpha).desired_width(60.0)).labelled_by(input_label.id);
        ui.end_row();
        ui.label(tl!("Slope per"));
        ui.horizontal(|ui| {
            ui.radio_value(&mut f.decade, false, tl!("year"));
            ui.radio_value(&mut f.decade, true, tl!("decade"));
        });
        ui.end_row();
        let input_label = ui.label(tl!("From"));
        ui.add(egui::TextEdit::singleline(&mut f.start).hint_text("2000-01-01").desired_width(110.0)).labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("To"));
        ui.add(egui::TextEdit::singleline(&mut f.end).hint_text("2024-12-31").desired_width(110.0)).labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Months"));
        ui.add(egui::TextEdit::singleline(&mut f.months).hint_text("12, 1, 2").desired_width(110.0)).labelled_by(input_label.id);
        ui.end_row();
        let input_label = ui.label(tl!("Dates a cell needs"));
        ui.add(egui::TextEdit::singleline(&mut f.min_valid).hint_text(tl!("half")).desired_width(60.0)).labelled_by(input_label.id);
        ui.end_row();
    });
    ui.label(
        egui::RichText::new(tl!("Writes Sen's slope, the test's Z and p-value, and the slope where the trend is significant."))
            .weak()
            .small(),
    );
    if !ui.button(tl!("Run")).clicked() {
        return;
    }
    let mut p = json!({"test": f.test, "per": if f.decade { "decade" } else { "year" }});
    if let Some((id, _)) = &f.cube {
        // Its area is the map's (a Zarr variable is read whole), its period the one typed.
        p["cube"] = json!(id);
        if stac_cube(id) {
            p["bbox"] = json!("view");
        }
        if f.start.trim().is_empty() || f.end.trim().is_empty() {
            app.workflow.error = Some(tl!("A data cube's trend needs its period: fill in From and To").to_string());
            return;
        }
    } else if f.stack {
        // In the list's order, not the order they were ticked: the dates typed go with them in
        // that order, and the first gives the grid.
        let ticked: Vec<&String> = rasters.iter().filter(|n| f.ticked.contains(n)).collect();
        p["layers"] = json!(ticked);
    } else {
        p["layer"] = json!(app.workflow.layer);
    }
    if f.test == "seasonal" {
        p["seasons"] = json!(f.seasons);
    }
    // A field filled in but not read as a number is said, never dropped for the default.
    let mut bad: Option<String> = None;
    match f.alpha.trim() {
        "" => {}
        a => match a.parse::<f64>() {
            Ok(a) => p["alpha"] = json!(a),
            Err(_) => bad = Some(tl!("The significance level is a number, such as 0.05").to_string()),
        },
    }
    match f.min_valid.trim() {
        "" => {}
        m => match m.parse::<u64>() {
            Ok(m) => p["minValid"] = json!(m),
            Err(_) => bad = Some(tl!("Dates a cell needs is a whole number, or empty for half").to_string()),
        },
    }
    for (k, v) in [("start", &f.start), ("end", &f.end)] {
        if !v.trim().is_empty() {
            p[k] = json!(v.trim());
        }
    }
    // Dates typed, a layer's or band's each in their order, apart by commas or semicolons (a date
    // can hold a space before its time): checked by the command, which says which it can't read.
    let typed_dates = if f.cube.is_some() { "" } else { f.dates.as_str() };
    let parts: Vec<&str> = typed_dates.split([',', ';']).map(str::trim).collect();
    // An empty one before the last would move the dates after it onto other layers; a comma at
    // the end is let be.
    if parts.iter().rev().skip(1).any(|d| d.is_empty()) {
        bad = Some(tl!("A date is missing between two commas").to_string());
    }
    let dates: Vec<&str> = parts.into_iter().filter(|d| !d.is_empty()).collect();
    if !dates.is_empty() {
        p["dates"] = json!(dates);
    }
    let typed: Vec<&str> = f.months.split([',', ' ']).map(str::trim).filter(|m| !m.is_empty()).collect();
    let months: Vec<Value> = typed.iter().filter_map(|m| m.parse::<u64>().ok()).map(|m| json!(m)).collect();
    if months.len() != typed.len() {
        bad = Some(tl!("Months are numbers from 1 to 12 (12, 1, 2 for December to February)").to_string());
    }
    if let Some(e) = bad {
        app.workflow.error = Some(e);
        return;
    }
    if !months.is_empty() {
        p["months"] = Value::Array(months);
    }
    // The web app has no threads: the run is made here.
    #[cfg(target_arch = "wasm32")]
    run(app, "cube.trend", p);
    #[cfg(not(target_arch = "wasm32"))]
    match rustgis_engine::cmd::cube::trend_start(&mut app.session, &p) {
        Ok(job) => {
            let id = app.session.download_started();
            let r = Trending {
                total: job.total(),
                id,
                params: p,
                cancel: Default::default(),
                done: Default::default(),
                result: Default::default(),
            };
            let (cancel, done, result) = (r.cancel.clone(), r.done.clone(), r.result.clone());
            // A panic on the thread (a hostile store) lands as an error, so the run always ends;
            // a thread the system can't start is said.
            let diagnostics = app.session.diagnostics.capture();
            let spawned = std::thread::Builder::new().name("trend".into()).spawn(move || {
                let _diagnostics = diagnostics.enter();
                let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    job.run(&cancel, &done).map(|rasters| (rasters, job.dates()))
                }))
                .unwrap_or_else(|_| Err(tl!("The trend failed unexpectedly").to_string()));
                if let Ok(mut g) = result.lock() {
                    *g = Some(out);
                }
            });
            match spawned {
                Ok(_) => {
                    app.workflow.trend.running = Some(r);
                    app.workflow.error = None;
                }
                Err(e) => {
                    app.session.download_failed(id);
                    app.workflow.error = Some(crate::i18n::fmt("The trend couldn't start: {error}", &[("error", &e.to_string())]));
                }
            }
        }
        Err(e) => app.workflow.error = Some(e),
    }
}

/// A trend that has finished, its layers added through `cube.trend` (so it's in the History):
/// called every frame, whatever pane is in view, so a run lands though the Workflows pane
/// closed. `true` while one is still running.
#[cfg(not(target_arch = "wasm32"))]
pub fn land_trend(app: &mut RustGisApp) -> bool {
    use std::sync::atomic::Ordering;
    let Some(r) = &app.workflow.trend.running else { return false };
    let Some(res) = r.result.lock().ok().and_then(|mut g| g.take()) else { return true };
    let mut p = r.params.clone();
    // A run the user cancelled stops quietly: no error for it.
    let cancelled = r.cancel.load(Ordering::Relaxed);
    // Still the project it was run in (`false` once another opened).
    let current = app.session.download_done(r.id);
    app.workflow.trend.running = None;
    match res {
        // Cancelled as it finished: nothing added.
        Ok(_) if cancelled => {}
        // Another project opened meanwhile: nothing added, and said.
        Ok(_) if !current => {
            app.workflow.error = Some(tl!("The trend finished after another project was opened, so it wasn't added").to_string());
        }
        Ok((rasters, dates)) => {
            p["ran"] = json!(rustgis_engine::cmd::cube::land(&mut app.session, rasters, dates));
            run(app, "cube.trend", p);
        }
        Err(_) if cancelled => {}
        Err(e) => app.workflow.error = Some(e),
    }
    false
}

/// A trend under way: its progress and Cancel (its result lands through `land_trend`). Whether
/// one is still running.
#[cfg(not(target_arch = "wasm32"))]
fn trend_running(app: &mut RustGisApp, ui: &mut Ui) -> bool {
    use std::sync::atomic::Ordering;
    if !land_trend(app) {
        return false;
    }
    let Some(r) = &app.workflow.trend.running else { return false };
    let (done, total) = (r.done.load(Ordering::Relaxed), r.total.max(1));
    heading(ui, "Trend over a series of rasters");
    ui.horizontal(|ui| {
        ui.add(egui::ProgressBar::new(done as f32 / total as f32).text(format!("{done} / {total}")).desired_width(200.0));
        if ui.button(tl!("Cancel")).clicked() {
            r.cancel.store(true, Ordering::Relaxed);
        }
    });
    true
}

/// Shows a workflow's result: profiles and histograms as tables and bars, the rest as JSON.
fn result_view(ui: &mut Ui, r: &Value, t: &Tokens) {
    ui.add_space(8.0);
    if let Some(fields) = r["fields"].as_array().filter(|f| f.first().is_some_and(|x| x.get("nulls").is_some())) {
        egui::Grid::new("wf_prof").striped(true).spacing(vec2(8.0, 2.0)).show(ui, |ui| {
            for h in ["Field", "Nulls", "Distinct", "Min", "Max", "Mean"] {
                ui.label(egui::RichText::new(tl!(h)).small().strong());
            }
            ui.end_row();
            let n = |v: &Value| v.as_f64().map(rustgis_engine::map::symbology::fmt_num).unwrap_or_default();
            for f in fields {
                ui.label(egui::RichText::new(f["field"].as_str().unwrap_or("")).small());
                ui.label(egui::RichText::new(f["nulls"].to_string()).small());
                ui.label(egui::RichText::new(f["distinct"].to_string()).small());
                ui.label(egui::RichText::new(n(&f["min"])).small());
                ui.label(egui::RichText::new(n(&f["max"])).small());
                ui.label(egui::RichText::new(n(&f["mean"])).small());
                ui.end_row();
            }
        });
        return;
    }
    if let Some(hists) = r["histograms"].as_array() {
        for h in hists {
            ui.label(egui::RichText::new(h["field"].as_str().unwrap_or("")).small().strong());
            let counts: Vec<f32> =
                h["counts"].as_array().map(|a| a.iter().filter_map(Value::as_f64).map(|v| v as f32).collect()).unwrap_or_default();
            let max = counts.iter().copied().fold(1.0_f32, f32::max);
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(260.0), 40.0), egui::Sense::hover());
            let bw = rect.width() / counts.len().max(1) as f32;
            for (i, c) in counts.iter().enumerate() {
                let hgt = rect.height() * c / max;
                let bar = egui::Rect::from_min_max(
                    egui::pos2(rect.min.x + i as f32 * bw + 1.0, rect.max.y - hgt),
                    egui::pos2(rect.min.x + (i + 1) as f32 * bw - 1.0, rect.max.y),
                );
                ui.painter().rect_filled(bar, 1.0, t.accent);
            }
        }
        return;
    }
    let text = serde_json::to_string_pretty(r).unwrap_or_default();
    let short: String = text.chars().take(3000).collect();
    ui.label(egui::RichText::new(short).monospace().small());
}

/// The lines the Script pane's box starts with; the fewest it keeps when the pane is short.
const SCRIPT_MIN_ROWS: usize = 10;
/// The most lines a run's results take below the Script pane's buttons; more scroll there.
const SCRIPT_OUT_ROWS: f32 = 6.0;

/// The Script pane, laid out to the pane's own height, out of the right pane's scroll (as the SQL
/// and Assistant panes are): the box grows with the script down to the buttons and then scrolls
/// inside, so Run and a run's results stay in view, and its height doesn't follow a scroll of the
/// pane. Only a pane too short for the box's first lines and the buttons scrolls as a whole.
pub fn script_pane(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    start_over_in_another_project(app);
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
        // The hint above the box (two lines, in a narrow pane), the box's first lines, the rest.
        let hint = (ui.text_style_height(&egui::TextStyle::Small) + ui.spacing().item_spacing.y) * 2.0;
        let need = hint + script_rows(ui) + script_below(app, ui);
        if ui.available_height() >= need {
            script(app, ui, t, true);
        } else {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| script(app, ui, t, false));
        }
    });
}

/// Another project replaced the Script pane's, however it came (the interface, a script, the
/// assistant, an agent): the pane starts blank, as the project does (#1511). Its script, results
/// and count were for what was closed. A run still to land from it is left by the read jobs, and
/// [`script_ran`] keeps it out of the pane; its notice stays among the session's.
fn start_over_in_another_project(app: &mut RustGisApp) {
    let opened = app.session.opened;
    if app.workflow.script_project.is_some_and(|p| p != opened) {
        clear_script(app);
    }
    app.workflow.script_project = Some(opened);
}

/// The Clear button: [`clear_script`], with the script first kept as an undo step of the box and
/// the focus given to the box, so Ctrl+Z brings the script back. The box records undo steps only
/// while focused, so the step is added here, as `scriptcomplete::accept` does.
fn clear_keeping_undo(app: &mut RustGisApp, ui: &Ui, box_id: egui::Id) {
    use egui::text::{CCursor, CCursorRange};
    let mut state = egui::widgets::text_edit::TextEditState::load(ui.ctx(), box_id).unwrap_or_default();
    let mut undoer = state.undoer();
    let end = CCursorRange::one(CCursor::new(app.workflow.script.chars().count()));
    undoer.add_undo(&(state.cursor.char_range().unwrap_or(end), app.workflow.script.clone()));
    state.set_undoer(undoer);
    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(0))));
    state.store(ui.ctx(), box_id);
    clear_script(app);
    ui.memory_mut(|m| m.request_focus(box_id));
}

/// The Script pane emptied: its script, results and count, so the next run is `Run #1`, and the
/// Details window with the run it showed. Clear does it, and another project opening.
fn clear_script(app: &mut RustGisApp) {
    let w = &mut app.workflow;
    w.script.clear();
    w.script_out.clear();
    w.script_last = None;
    w.script_details = false;
}

/// The height of the Script box's first lines.
fn script_rows(ui: &Ui) -> f32 {
    ui.text_style_height(&egui::TextStyle::Monospace) * SCRIPT_MIN_ROWS as f32
}

/// The height the Script pane needs below its box: the buttons, the last run's line, and its
/// results (their own scroll, at most SCRIPT_OUT_ROWS lines) when there are any.
fn script_below(app: &RustGisApp, ui: &Ui) -> f32 {
    let gap = ui.spacing().item_spacing.y;
    // A row: as tall as a button, whatever its text.
    let last = if app.workflow.script_last.is_none() { 0.0 } else { ui.spacing().interact_size.y + gap };
    let results = if app.workflow.script_out.is_empty() { 0.0 } else { script_out_height(ui) + gap };
    ui.spacing().interact_size.y + gap * 3.0 + last + results
}

/// Runs the Script pane's `script`: on the desktop its tools on a thread of their own first, the
/// status bar showing it and Cancel, the results saying so until it lands ([`script_ran`]).
fn start_script(app: &mut RustGisApp, script: String) {
    // When it started and how long it took: a tool run outside the app in the browser
    // (`gp.finish`) ends later.
    let at = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    app.workflow.script_started.push_back((at, web_time::Instant::now(), app.session.opened));
    app.workflow.script_out = vec![(tl!("Running…").to_string(), false)];
    #[cfg(not(target_arch = "wasm32"))]
    let _ = crate::readjob::run_then(app, "script.run", json!({"script": script}), Some(script_ran));
    #[cfg(target_arch = "wasm32")]
    {
        let r = app.run("script.run", json!({"script": script}));
        script_ran(app, crate::Landed::Ran(r));
    }
}

/// A Script pane run as it lands, in the pane: its number, how long it took, and each line's
/// result, or why it didn't run.
fn script_ran(app: &mut RustGisApp, landed: crate::Landed) {
    let now = web_time::Instant::now();
    let opened = app.session.opened;
    let (asked_at, asked, asked_in) = app.workflow.script_started.pop_front().unwrap_or((0, now, opened));
    // Asked in a project since replaced and not run (left as another opened, or cancelled): the
    // pane started blank for the new one, and its notice is the session's.
    if asked_in != opened && !matches!(landed, crate::Landed::Ran(_)) {
        app.workflow.script_landed = Some(now);
        return;
    }
    // One that waited for the run before it started as that one landed.
    let started = app.workflow.script_landed.filter(|&l| l > asked).unwrap_or(asked);
    let at = if asked_at == 0 { 0 } else { asked_at.saturating_add(started.duration_since(asked).as_secs()) };
    app.workflow.script_landed = Some(now);
    // A script that opened a project itself is kept with its results, in the project it opened,
    // as its first run there.
    let n = match app.workflow.script_last.as_ref() {
        Some(r) if asked_in == opened => r.n.saturating_add(1),
        _ => 1,
    };
    app.workflow.script_project = Some(opened);
    // A run that stops is undone whole (`Session::guarded` restores the project), so its error
    // is all it has to tell.
    let (ran, error) = match landed {
        crate::Landed::Ran(Ok(mut v)) => match v.get_mut("ran").map(Value::take) {
            Some(Value::Array(a)) => (a, None),
            _ => (Vec::new(), None),
        },
        crate::Landed::Ran(Err(e)) | crate::Landed::NotRun(e) => (Vec::new(), Some(e)),
    };
    app.workflow.script_out = match &error {
        None => ran.iter().map(|r| (format!("{} → {}", r["command"].as_str().unwrap_or(""), preview(&r["result"])), false)).collect(),
        Some(e) => vec![(e.clone(), true)],
    };
    let ran = ran
        .into_iter()
        .map(|mut r| ScriptLine {
            line: r["line"].as_u64().unwrap_or(0),
            command: r["command"].as_str().unwrap_or("").to_string(),
            result: r.get_mut("result").map(Value::take).unwrap_or_default(),
            text: Vec::new(),
        })
        .collect();
    app.workflow.script_last = Some(ScriptRun { n, secs: now.duration_since(started).as_secs_f64(), at, ran, error });
    // Another still to land: the results say so first, above this one's, where their scroll
    // starts, however many lines it ran. One asked in a project this run replaced is left, not
    // run, so it isn't told.
    if app.workflow.script_started.iter().any(|&(_, _, asked_in)| asked_in == opened) {
        app.workflow.script_out.insert(0, (tl!("Running…").to_string(), false));
    }
    app.map.invalidate();
}

/// The most height a run's results take.
fn script_out_height(ui: &Ui) -> f32 {
    (ui.text_style_height(&egui::TextStyle::Small) + ui.spacing().item_spacing.y) * SCRIPT_OUT_ROWS
}

/// The Script pane's content: commands typed one per line, run in order, coloured as they're typed
/// (#653). `fit`: the box grows down to the buttons (the pane's own layout); else it keeps its
/// first lines, in a pane that scrolls as a whole.
fn script(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens, fit: bool) {
    ui.label(egui::RichText::new(tl!("One command per line: id {json}. Lines starting with # are comments.")).small().weak());
    let unknown = crate::scriptcolor::unknown_ids_cached(ui, &app.session, &app.workflow.script);
    let palette = crate::scriptcolor::Palette::of(t);
    let mut layouter =
        |ui: &Ui, buf: &dyn egui::TextBuffer, wrap: f32| crate::scriptcolor::layout(ui, buf.as_str(), &unknown, palette, wrap);
    // The box grows with the script down to what the buttons and results leave (the pane's own
    // layout: `available_height` is what's left of the pane), then scrolls inside it.
    let min = script_rows(ui);
    let room = if fit { (ui.available_height() - script_below(app, ui)).max(min) } else { min };
    let out_h = script_out_height(ui);
    // One id whether the pane scrolls as a whole or not, so resizing it across that keeps the
    // box's focus, cursor and selection.
    let box_id = egui::Id::unique("script_text");
    // Ctrl+Enter (⌘+Enter) runs the script (#773), taken before the box sees it, so it runs rather
    // than breaking the line; once a press, not again for each repeat of a key held down. The box
    // keeps the focus, so the list closes, as Escape closes it.
    let ctrl_enter = ui.memory(|m| m.has_focus(box_id))
        && ui.input_mut(|i| {
            let mut run = false;
            i.events.retain(|e| match e {
                egui::Event::Key { key: egui::Key::Enter, pressed: true, repeat, modifiers, .. }
                    if modifiers.matches_logically(egui::Modifiers::COMMAND) =>
                {
                    run |= !repeat;
                    false
                }
                _ => true,
            });
            run
        });
    if ctrl_enter {
        crate::scriptcomplete::close(ui, box_id);
    }
    // The suggestion list's keys (#662), taken before the box sees them.
    if let Some((offer, item)) = crate::scriptcomplete::keys(ui, box_id, &app.workflow.script) {
        crate::scriptcomplete::accept(ui, box_id, &mut app.workflow.script, &offer, &item, &app.session);
    }
    // Braces, brackets and quotes closing themselves (#662): what the box does with the keys,
    // followed through after it.
    let before = crate::scriptcomplete::before(ui, box_id, &app.workflow.script);
    let out = egui::ScrollArea::vertical().id_salt("script_box").max_height(room).auto_shrink([false, true]).show(ui, |ui| {
        egui::TextEdit::multiline(&mut app.workflow.script)
            .id(box_id)
            .code_editor()
            .event_filter(crate::scriptcomplete::filter(ui, box_id))
            .layouter(&mut layouter)
            .desired_rows(SCRIPT_MIN_ROWS)
            .desired_width(f32::INFINITY)
            .hint_text("map.goToXY {\"lon\": 2.35, \"lat\": 48.85, \"scale\": 5000000}\nselect.byAttributes {\"layer\": \"Cities\", \"where\": \"pop_max > 1000000\"}")
            .show(ui)
    });
    let clicked = crate::scriptcomplete::show(ui, box_id, &out.inner, out.inner_rect, &app.workflow.script, &app.session);
    crate::scriptcomplete::hint(ui, box_id, &out.inner, out.inner_rect, &app.workflow.script, &app.session);
    if let Some((offer, item)) = clicked {
        crate::scriptcomplete::accept(ui, box_id, &mut app.workflow.script, &offer, &item, &app.session);
    } else {
        crate::scriptcomplete::braces(ui, before, &out.inner, &mut app.workflow.script);
    }
    ui.horizontal(|ui| {
        // As egui reads the keys: ⌘ on a Mac, the browser's too (from its user agent).
        let mac = ui.ctx().os().is_mac();
        let shortcut = if mac { "⌘+Enter" } else { "Ctrl+Enter" };
        let run = ui.button(tl!("Run")).on_hover_text(crate::i18n::fmt("Run the script ({keys})", &[("keys", shortcut)])).clicked();
        if run || ctrl_enter {
            let script = app.workflow.script.clone();
            start_script(app, script);
        }
        if ui.button(tl!("Commands…")).on_hover_text(tl!("Search every command and its parameters")).clicked() {
            let _ = app.run("ui.commandSearch", Value::Null);
        }
        // Not while a run is under way: its results would land in the emptied pane.
        let w = &app.workflow;
        let running = !w.script_started.is_empty();
        let anything = !w.script.is_empty() || !w.script_out.is_empty() || w.script_last.is_some();
        let undo = if mac { "⌘+Z" } else { "Ctrl+Z" };
        let mut clear = ui
            .add_enabled(anything && !running, egui::Button::new(tl!("Clear")))
            .on_hover_text(crate::i18n::fmt("Empty the script and its results ({keys} brings the script back)", &[("keys", undo)]));
        if running {
            clear = clear.on_disabled_hover_text(tl!("Wait for the run to finish"));
        }
        if clear.clicked() {
            clear_keeping_undo(app, ui, box_id);
        }
    });
    // Above the results' scroll, so it stays in view: its number and how long it took, and on
    // the right when it started, as the Geoprocessing History tells it (UTC: the app has no time
    // zone).
    let mut details = false;
    if let Some(r) = &app.workflow.script_last {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(run_line(r)).small().strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                details = ui.small_button(tl!("Details…")).on_hover_text(tl!("Each line's whole result, to read or copy")).clicked();
                if let Some(time) = run_time(r) {
                    ui.label(egui::RichText::new(time).small().weak());
                }
            });
        });
    }
    app.workflow.script_details |= details;
    // In a scroll of their own, so a long error (it wraps) can't push them out of the pane. A
    // result is cut here; Details… shows it whole.
    egui::ScrollArea::vertical().id_salt("script_out").max_height(out_h).auto_shrink([false, true]).show(ui, |ui| {
        for (line, err) in &app.workflow.script_out {
            let c = if *err { SCRIPT_ERROR } else { ui.visuals().text_color() };
            ui.label(egui::RichText::new(line.chars().take(SCRIPT_PREVIEW).collect::<String>()).monospace().small().color(c));
        }
    });
}

/// The colour of a Script pane run's error.
const SCRIPT_ERROR: egui::Color32 = egui::Color32::from_rgb(0xC0, 0x30, 0x30);

/// A run's number and how long it took: `Run #2 (0.84 s)`.
fn run_line(r: &ScriptRun) -> String {
    crate::i18n::fmt("Run #{n} ({secs} s)", &[("n", &r.n.to_string()), ("secs", &format!("{:.2}", r.secs))])
}

/// When a run started, in UTC (the app has no time zone), if the clock could tell.
fn run_time(r: &ScriptRun) -> Option<String> {
    (r.at > 0).then(|| crate::i18n::fmt("{time} UTC", &[("time", &rustgis_engine::map::graphics::civil(r.at).1)]))
}

/// The most rows a result shows at once in the Details window; a longer one scrolls.
const SCRIPT_DETAILS_ROWS: f32 = 24.0;

/// The most characters of a result, or of an error, the pane shows: a line of the results.
const SCRIPT_PREVIEW: usize = 400;

/// The most characters in a row of the Details window: a longer line is split into rows.
const SCRIPT_ROW_CHARS: usize = 200;

/// A result laid out to read: indented JSON, a key a line.
fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// A result as the pane shows it: its JSON on one line, written no further than the pane shows
/// (SCRIPT_PREVIEW characters), so a long one (an export's text) isn't written out whole only to
/// be cut.
fn preview(v: &Value) -> String {
    /// Takes bytes until it holds enough for SCRIPT_PREVIEW characters (at most 4 bytes each),
    /// then refuses the rest, which stops the writing.
    struct Capped(Vec<u8>);
    impl std::io::Write for Capped {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            let room = (SCRIPT_PREVIEW * 4).saturating_sub(self.0.len());
            if room == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            let taken = bytes.get(..bytes.len().min(room)).unwrap_or_default();
            self.0.extend_from_slice(taken);
            Ok(taken.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Capped(Vec::new());
    // Stopped by Capped once it's full: what was written is the preview.
    let _ = serde_json::to_writer(&mut out, v);
    // A character cut at the end lies past the SCRIPT_PREVIEW kept.
    String::from_utf8_lossy(&out.0).chars().take(SCRIPT_PREVIEW).collect()
}

/// A result's text as the Details window's rows: a line of the indented JSON a row, and a line
/// longer than SCRIPT_ROW_CHARS (a long string, such as an export's text, which JSON keeps on one
/// line) split into rows of that many characters, so no row in view is long to lay out.
fn rows(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut chars = line.chars().peekable();
        if chars.peek().is_none() {
            out.push(String::new());
        }
        while chars.peek().is_some() {
            out.push(chars.by_ref().take(SCRIPT_ROW_CHARS).collect());
        }
    }
    out
}

/// A run's line as the Details window heads it: `line 3: raster.gcpFit`.
fn line_head(l: &ScriptLine) -> String {
    crate::i18n::fmt("line {n}: {command}", &[("n", &l.line.to_string()), ("command", &l.command)])
}

/// The whole of a run as Copy All gives it: its line, then each line's head and result, then why
/// it stopped.
fn run_text(r: &ScriptRun) -> String {
    let mut out = run_line(r);
    if let Some(time) = run_time(r) {
        out.push(' ');
        out.push_str(&time);
    }
    for l in &r.ran {
        out.push_str("\n\n");
        out.push_str(&line_head(l));
        out.push('\n');
        out.push_str(&pretty(&l.result));
    }
    if let Some(e) = &r.error {
        out.push_str("\n\n");
        out.push_str(e);
    }
    out
}

/// The Script pane's last run in a window of its own (Details…), as View Details in ArcGIS Pro's
/// Geoprocessing History: each line's whole result laid out to read, to copy one or all, where
/// the pane cuts each to a line.
pub fn script_details(app: &mut RustGisApp, ctx: &egui::Context) {
    // Shown with the Script pane hidden too: another project's run isn't this one's.
    start_over_in_another_project(app);
    if !app.workflow.script_details {
        return;
    }
    let Some(r) = app.workflow.script_last.as_mut() else {
        app.workflow.script_details = false;
        return;
    };
    let mut open = true;
    egui::Window::new(tl!("Script Results"))
        .id(egui::Id::unique("script_details"))
        .open(&mut open)
        .resizable(true)
        .default_size(vec2(560.0, 420.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(run_line(r)).strong());
                if let Some(time) = run_time(r) {
                    ui.label(egui::RichText::new(time).weak());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tl!("Copy All")).clicked() {
                        ui.ctx().copy_text(run_text(r));
                    }
                });
            });
            ui.separator();
            let n = r.n;
            let row = ui.text_style_height(&egui::TextStyle::Monospace);
            egui::ScrollArea::vertical().id_salt("script_details_scroll").auto_shrink([false, false]).show(ui, |ui| {
                for (i, l) in r.ran.iter_mut().enumerate() {
                    // Folded, one line each, a click opening one; a run of its own each time, so
                    // what was opened in the last one folds again.
                    egui::CollapsingHeader::new(egui::RichText::new(line_head(l)).monospace())
                        .id_salt(("script_details_line", n, i))
                        .default_open(false)
                        .show(ui, |ui| {
                            if l.text.is_empty() {
                                l.text = rows(&pretty(&l.result));
                            }
                            // The result as it is, not its rows: a long line isn't split.
                            if ui.small_button(tl!("Copy")).clicked() {
                                ui.ctx().copy_text(pretty(&l.result));
                            }
                            // Only the rows in view are laid out, each short, however long the
                            // result: in a scroll of its own, at most SCRIPT_DETAILS_ROWS rows tall.
                            egui::ScrollArea::both()
                                .id_salt(("script_details_result", n, i))
                                .max_height(row * SCRIPT_DETAILS_ROWS)
                                .auto_shrink([false, true])
                                .show_rows(ui, row, l.text.len(), |ui, rows| {
                                    for text in l.text.get(rows).unwrap_or_default() {
                                        ui.add(egui::Label::new(egui::RichText::new(text).monospace()).extend());
                                    }
                                });
                        });
                }
                if let Some(e) = &r.error {
                    ui.label(egui::RichText::new(e).monospace().color(SCRIPT_ERROR));
                } else if r.ran.is_empty() {
                    // A script of comments or blank lines only.
                    ui.label(egui::RichText::new(tl!("Nothing ran: the script has no commands.")).weak());
                }
            });
        });
    if !open {
        app.workflow.script_details = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    /// A Script pane run works its tools out on a thread of its own: the results say Running…
    /// until it lands, runs land in the order asked, and one cancelled or left says so.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_scripts_tools_run_on_a_thread_and_land_in_the_pane() {
        let mut app = RustGisApp::with_sample();
        app.reads_off_frame = true;
        let layers = app.session.map().layers.len();
        let script = r#"gp.run {"tool": "buffer", "params": {"input": "Cities", "distance": 100}}"#.to_string();
        let landed = |app: &mut RustGisApp| {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while crate::readjob::land(app) {
                assert!(std::time::Instant::now() < end, "the script never landed");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        // The results say Running… until it lands, then each line's result, as Run #1.
        start_script(&mut app, script.clone());
        assert_eq!(app.workflow.script_out, vec![("Running…".to_string(), false)]);
        assert_eq!(app.session.map().layers.len(), layers, "nothing added before it lands");
        landed(&mut app);
        assert_eq!(app.session.map().layers.len(), layers + 1);
        assert!(
            app.workflow.script_out.first().is_some_and(|(l, err)| !err && l.starts_with("gp.run → ")),
            "{:?}",
            app.workflow.script_out
        );
        assert_eq!(app.workflow.script_last.as_ref().map(|r| r.n), Some(1));
        // Run twice: the second waits for the first, and each lands as its own run.
        start_script(&mut app, script.clone());
        start_script(&mut app, script.clone());
        landed(&mut app);
        assert_eq!(app.session.map().layers.len(), layers + 3);
        assert_eq!(app.workflow.script_last.as_ref().map(|r| r.n), Some(3));
        assert!(app.workflow.script_started.is_empty());
        // Cancelled: nothing added, and the results say so.
        start_script(&mut app, script.clone());
        app.reads.cancel_for_tests();
        landed(&mut app);
        assert_eq!(app.session.map().layers.len(), layers + 3);
        assert_eq!(app.workflow.script_out, vec![("Run Script was cancelled".to_string(), true)]);
        // Another project opened meanwhile: it is left, and the notice names it as a script; the
        // pane starts blank for the new project (#1511).
        start_script(&mut app, script);
        app.session.run("project.new", &Value::Null).unwrap();
        landed(&mut app);
        start_over_in_another_project(&mut app);
        assert!(app.workflow.script_out.is_empty() && app.workflow.script_last.is_none());
        assert!(app.session.notices.iter().any(|n| n.text == "Run Script was left: another project was opened"));
    }

    /// Another project opening starts the Script pane blank, whoever opened it (#1511): the
    /// script, written or run, its results and the count, at once, a run still to land or not. A
    /// script that opened a project itself stays, with its results, as its first run there.
    #[test]
    fn another_project_starts_the_script_pane_blank() {
        let mut app = RustGisApp::with_sample();
        let blank =
            |app: &RustGisApp| app.workflow.script.is_empty() && app.workflow.script_out.is_empty() && app.workflow.script_last.is_none();
        let ran = |line: &str| crate::Landed::Ran(Ok(json!({"ran": [{"line": 1, "command": line, "result": null}]})));
        let ask = |app: &mut RustGisApp| app.workflow.script_started.push_back((0, web_time::Instant::now(), app.session.opened));
        // Shown first: its own project's results stay, and the count goes on.
        start_over_in_another_project(&mut app);
        app.workflow.script = "select.all".to_string();
        for _ in 0..3 {
            ask(&mut app);
            script_ran(&mut app, ran("select.all"));
        }
        start_over_in_another_project(&mut app);
        assert_eq!(app.workflow.script_out.len(), 1, "kept in its own project");
        assert_eq!(app.workflow.script_last.as_ref().map(|r| r.n), Some(3));
        // A script that opened the project itself, after Run #3: it stays, with its results, as
        // Run #1 of the project it opened.
        app.workflow.script = "project.sample".to_string();
        ask(&mut app);
        app.session.run("project.sample", &Value::Null).unwrap();
        script_ran(&mut app, ran("project.sample"));
        start_over_in_another_project(&mut app);
        assert_eq!(app.workflow.script, "project.sample");
        assert_eq!(app.workflow.script_out.len(), 1);
        assert_eq!(app.workflow.script_last.as_ref().map(|r| r.n), Some(1));
        // The same with another run of the old project waiting behind it: that one is left, so
        // the results don't say Running… for it, and it adds nothing when it lands.
        ask(&mut app);
        ask(&mut app);
        app.session.run("project.sample", &Value::Null).unwrap();
        script_ran(&mut app, ran("project.sample"));
        assert_eq!(app.workflow.script_out.len(), 1, "no Running… for a run that will be left: {:?}", app.workflow.script_out);
        script_ran(&mut app, crate::Landed::NotRun("Run Script was left: another project was opened".into()));
        assert_eq!(app.workflow.script_out.len(), 1);
        assert_eq!(app.workflow.script_last.as_ref().map(|r| r.n), Some(1));
        assert!(app.workflow.script_started.is_empty());
        // Opened by a command (an agent, the assistant), the pane hidden and Details open:
        // everything goes, the window too.
        app.workflow.script_details = true;
        app.session.run("project.new", &Value::Null).unwrap();
        script_details(&mut app, &egui::Context::default());
        assert!(blank(&app) && !app.workflow.script_details);
        // A script written and never run goes too.
        app.workflow.script = "map.zoomFull".to_string();
        app.session.run("project.sample", &Value::Null).unwrap();
        start_over_in_another_project(&mut app);
        assert!(blank(&app));
        // A run under way when another project opens: the pane goes at once, so its script can't
        // be run again in the new project, and the run, left, adds nothing when it lands.
        app.workflow.script = "select.all".to_string();
        ask(&mut app);
        app.session.run("project.new", &Value::Null).unwrap();
        start_over_in_another_project(&mut app);
        assert!(blank(&app));
        // A script written meanwhile in the new project, and run: the old one lands first, left,
        // then the new one, as Run #1.
        app.workflow.script = "map.zoomFull".to_string();
        ask(&mut app);
        script_ran(&mut app, crate::Landed::NotRun("Run Script was left: another project was opened".into()));
        assert!(app.workflow.script_last.is_none(), "the left run adds nothing");
        script_ran(&mut app, ran("map.zoomFull"));
        start_over_in_another_project(&mut app);
        assert_eq!(app.workflow.script, "map.zoomFull");
        assert_eq!(app.workflow.script_last.as_ref().map(|r| r.n), Some(1));
        assert_eq!(app.workflow.script_out.len(), 1);
    }

    /// A Script pane run that waited for the one before it is timed from when that one landed,
    /// and the results say Running… while another is still to land.
    #[test]
    fn a_queued_script_run_is_timed_from_its_start() {
        let mut app = RustGisApp::with_sample();
        let asked = web_time::Instant::now() - std::time::Duration::from_secs(10);
        app.workflow.script_started.extend([(1_000, asked, app.session.opened), (1_000, asked, app.session.opened)]);
        let ran: Vec<Value> = (1..=8).map(|n| json!({"line": n, "command": "select.all", "result": null})).collect();
        script_ran(&mut app, crate::Landed::Ran(Ok(json!({"ran": ran}))));
        let first = app.workflow.script_last.as_ref().map(|r| (r.secs, r.at)).unwrap();
        assert!(first.0 >= 10.0 && first.1 == 1_000, "{first:?}");
        // First, where the results' scroll starts, above its eight lines.
        assert_eq!(app.workflow.script_out.len(), 9);
        assert_eq!(app.workflow.script_out[0], ("Running…".to_string(), false));
        script_ran(&mut app, crate::Landed::NotRun("cancelled".into()));
        let second = app.workflow.script_last.as_ref().map(|r| (r.secs, r.at)).unwrap();
        assert!(second.0 < 5.0 && second.1 >= 1_010, "{second:?}");
        assert_eq!(app.workflow.script_out, vec![("cancelled".to_string(), true)]);
    }

    /// Clear empties the Script pane as another project opening does (#1511): the script, its
    /// results and count, and Details. The box keeps the focus, so Ctrl+Z brings the script back.
    /// With nothing to clear, or a run under way, the button is off.
    #[test]
    fn clear_empties_the_script_pane_and_undo_brings_the_script_back() {
        use egui_kittest::kittest::NodeT;
        let mut app = RustGisApp::with_sample();
        app.workflow.script = "map.zoomFull".to_string();
        script_ran(&mut app, crate::Landed::Ran(Ok(json!({"ran": [{"line": 1, "command": "map.zoomFull", "result": null}]}))));
        // Details open (its window isn't drawn here: it would sit over the button the pointer clicks).
        app.workflow.script_details = true;
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(900.0, 800.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                script_pane(app, ui, &Tokens::light());
            },
            app,
        );
        h.run();
        let on = |h: &egui_kittest::Harness<'_, RustGisApp>| !h.get_by_label("Clear").accesskit_node().is_disabled();
        assert!(on(&h));
        h.get_by_label("Clear").click();
        h.run();
        let w = &h.state().workflow;
        assert!(w.script.is_empty() && w.script_out.is_empty() && w.script_last.is_none(), "script, results and count");
        assert!(!w.script_details, "Details closes with the run it showed");
        assert!(h.get_by_role(egui::accesskit::Role::MultilineTextInput).is_focused(), "the box keeps the focus");
        assert!(!on(&h), "nothing left to clear");
        // The box's own undo brings the script back, with nothing clicked first.
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run();
        assert_eq!(h.state().workflow.script, "map.zoomFull");
        assert!(h.state().workflow.script_last.is_none(), "the results stay gone");
        // A run under way: off, as its results would land in the emptied pane.
        h.state_mut().workflow.script_started.push_back((0, web_time::Instant::now(), 0));
        h.run();
        assert!(!on(&h));
    }

    /// Details… opens the last run in a window of its own, each line folded: a result the pane cuts
    /// shows whole there, laid out to read, Copy copies it and Copy All the run; a run that stopped
    /// shows why.
    #[test]
    fn script_details_show_a_runs_whole_results() {
        let mut app = RustGisApp::with_sample();
        let long: serde_json::Map<String, Value> = (0..60).map(|i| (format!("k{i:02}"), json!("xxxxxxxxxx"))).collect();
        let ran = json!([{"line": 1, "command": "raster.gcpFit", "result": long}, {"line": 3, "command": "map.zoomFull", "result": null}]);
        script_ran(&mut app, crate::Landed::Ran(Ok(json!({"ran": ran}))));
        assert!(!app.workflow.script_out[0].0.chars().take(400).collect::<String>().contains("k59"), "the pane cuts it");
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(900.0, 800.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                // The app's fonts, for the results' arrow.
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                script_pane(app, ui, &Tokens::light());
                script_details(app, ui.ctx());
            },
            app,
        );
        h.run();
        assert!(h.query_by_label("Copy All").is_none(), "closed until asked");
        h.get_by_label("Details…").click();
        h.run();
        assert!(h.state().workflow.script_details);
        // Each line folded, its result shown when it's opened.
        assert!(h.query_by_label("line 3: map.zoomFull").is_some());
        assert!(h.query_by_label_contains("\"k59\"").is_none(), "folded");
        h.get_by_label("line 1: raster.gcpFit").click();
        h.run();
        // A key a row, only the rows in view laid out; its Copy has them all.
        assert!(h.query_by_label_contains("\"k00\": \"xxxxxxxxxx\"").is_some(), "laid out to read, one key a row");
        assert!(h.query_by_label_contains("\"k59\"").is_none(), "a row out of view isn't laid out");
        let copied = |h: &egui_kittest::Harness<'_, RustGisApp>| {
            h.output().platform_output.commands.iter().find_map(|c| match c {
                egui::OutputCommand::CopyText(t) => Some(t.clone()),
                _ => None,
            })
        };
        h.get_by_label("Copy").click();
        h.step();
        let one = copied(&h).expect("Copy copies");
        assert!(one.starts_with("{\n  \"k00\"") && one.contains("\"k59\": \"xxxxxxxxxx\"\n}"), "{one}");
        h.get_by_label("Copy All").click();
        h.step();
        let all = copied(&h).expect("Copy All copies");
        assert!(
            all.starts_with("Run #1 (") && all.contains("line 1: raster.gcpFit\n{") && all.contains("line 3: map.zoomFull\nnull"),
            "{all}"
        );
        // A run that stopped: the window, still open, shows why.
        script_ran(h.state_mut(), crate::Landed::Ran(Err("line 2: raster.gcpFit: needs 3 points".into())));
        h.run();
        assert_eq!(h.query_all_by_label("line 2: raster.gcpFit: needs 3 points").count(), 2, "in the pane and the window");
        assert!(h.query_by_label_contains("\"k59\"").is_none(), "the last run only");
        // A script of comments only: the window says nothing ran rather than staying blank.
        script_ran(h.state_mut(), crate::Landed::Ran(Ok(json!({"ran": []}))));
        h.run();
        assert!(h.query_by_label("Nothing ran: the script has no commands.").is_some());
        // Its ✕ closes it, until Details… again.
        h.get_by_label("Close window").click();
        h.run();
        assert!(!h.state().workflow.script_details);
        assert!(h.query_by_label("Copy All").is_none());
    }

    /// A long result is cut as the pane writes it, not after; and in Details a long line (an
    /// export's text, one JSON string) is split into short rows, while Copy keeps it whole.
    #[test]
    fn long_results_are_bounded_where_they_show() {
        let long = json!({"text": "a".repeat(100_000)});
        let shown = preview(&long);
        assert_eq!(shown.chars().count(), SCRIPT_PREVIEW);
        assert!(shown.starts_with("{\"text\":\"aaa"), "{shown}");
        assert_eq!(preview(&json!({"a": 1})), "{\"a\":1}", "a short one whole, as before");
        let wide = preview(&json!("é".repeat(1_000)));
        assert!(wide.chars().count() == SCRIPT_PREVIEW && !wide.contains('\u{FFFD}'), "cut between characters");
        // In rows of at most SCRIPT_ROW_CHARS characters, nothing lost or added.
        let text = pretty(&long);
        let split = rows(&text);
        assert!(split.len() > 500 && split.iter().all(|r| r.chars().count() <= SCRIPT_ROW_CHARS));
        assert_eq!(split.concat(), text.lines().collect::<String>());
        let counts: Vec<usize> = rows(&"é".repeat(450)).iter().map(|r| r.chars().count()).collect();
        assert_eq!(counts, vec![200, 200, 50]);
        assert_eq!(rows("{\n\n}"), vec!["{", "", "}"], "a blank line stays a row");
        // In the window: the long line's rows, and Copy with the whole text.
        let mut app = RustGisApp::with_sample();
        script_ran(&mut app, crate::Landed::Ran(Ok(json!({"ran": [{"line": 1, "command": "data.convert", "result": long}]}))));
        app.workflow.script_details = true;
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(900.0, 800.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                script_details(app, ui.ctx());
            },
            app,
        );
        h.run();
        h.get_by_label("line 1: data.convert").click();
        h.run();
        let made = h.state().workflow.script_last.as_ref().map_or(0, |r| r.ran.iter().map(|l| l.text.len()).sum());
        assert!(made > 500, "laid out in rows: {made}");
        h.get_by_label("Copy").click();
        h.step();
        let copied = h.output().platform_output.commands.iter().find_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        });
        assert_eq!(copied, Some(pretty(&long)), "Copy keeps the long line whole");
    }

    /// Interpolate runs on a thread of its own from the pane (#1291 step 3): the pane goes on while
    /// it works, a second run waits for the first, and its layer lands through its command, which
    /// Record Task takes without the raster it handed over (a replay works it out again).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_slow_workflow_runs_on_a_thread_and_lands() {
        let mut app = RustGisApp::with_sample();
        app.ws.recording = Some(Vec::new());
        let layers = app.session.map().layers.len();
        let p = json!({"layer": "Cities", "field": "pop_max", "cellSize": 2});
        run(&mut app, "analysis.interpolate", p.clone());
        assert!(app.workflow.job.is_some() && app.workflow.error.is_none(), "{:?}", app.workflow.error);
        run(&mut app, "analysis.interpolate", p.clone());
        assert!(app.workflow.error.as_deref().is_some_and(|e| e.contains("still running")), "{:?}", app.workflow.error);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while land_job(&mut app) {
            assert!(std::time::Instant::now() < end, "the run never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(app.workflow.job.is_none() && app.workflow.error.is_none(), "{:?}", app.workflow.error);
        assert!(app.workflow.result.as_ref().is_some_and(|r| r["layer"].is_u64()), "{:?}", app.workflow.result);
        assert_eq!(app.session.map().layers.len(), layers + 1);
        let steps = app.ws.recording.as_deref().unwrap_or_default();
        let step = steps.last().expect("recorded");
        assert_eq!(step.command, "analysis.interpolate");
        assert!(step.params.get("ran").is_none() && step.params["field"] == "pop_max", "{:?}", step.params);
        // Opening another project stops a run at once, and says so: the pane is free again.
        run(&mut app, "analysis.interpolate", p.clone());
        let control = app.workflow.job.as_ref().map(|j| j.control.clone()).expect("started");
        app.session.run("project.new", &Value::Null).unwrap();
        assert!(!land_job(&mut app));
        assert!(app.workflow.job.is_none() && control.is_cancelled());
        assert!(app.workflow.error.as_deref().is_some_and(|e| e.contains("project was closed")), "{:?}", app.workflow.error);
        // From the map's menu: the pane opens at the section to show it, and a second run, which
        // must wait, is said in the status bar too.
        app.session.run("project.sample", &Value::Null).unwrap();
        run_elsewhere(&mut app, "interpolate", "analysis.interpolate", p.clone());
        assert!(app.workflow.job.is_some() && app.workflow.section == "interpolate");
        assert_eq!(app.ui.right, Some(crate::RightPane::Workflows));
        run_elsewhere(&mut app, "interpolate", "analysis.interpolate", p);
        assert!(app.session.notices.iter().any(|n| n.text.contains("still running")));
        while land_job(&mut app) {
            assert!(std::time::Instant::now() < end, "the run never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Every workflow has named inputs, including both weights sharing a row (#790).
    #[test]
    fn every_workflow_names_its_inputs() {
        for &(section, _) in SECTIONS {
            let mut app = RustGisApp::with_sample();
            app.workflow.section = section;
            let mut fonts = false;
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(500.0, 1200.0)).build_ui_state(
                move |ui, app: &mut RustGisApp| {
                    if !fonts {
                        crate::theme::install_fonts(ui.ctx(), &[], None);
                        fonts = true;
                        return;
                    }
                    workflows(app, ui, &Tokens::light());
                },
                app,
            );
            h.run();
            if section == "suitability" {
                assert_eq!(h.query_all_by_role_and_label(egui::accesskit::Role::TextInput, "Weights").count(), 2);
                for label in ["First raster", "Second raster"] {
                    assert!(h.query_by_role_and_label(egui::accesskit::Role::ComboBox, label).is_some());
                }
            }
            if section == "zonalStats" {
                for label in ["Band", "Prefix", "Output"] {
                    assert!(h.query_by_role_and_label(egui::accesskit::Role::TextInput, label).is_some());
                }
            }
        }
    }
}
