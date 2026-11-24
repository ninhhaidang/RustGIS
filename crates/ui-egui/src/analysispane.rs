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

/// The systems a layer holds its coordinates in and was read in, and the project's.
type LayerSystems =
    (rustgis_engine::data::raster::Crs, Option<rustgis_engine::data::raster::Crs>, Option<rustgis_engine::data::raster::Crs>);

/// The Workflows pane's inputs and results.
#[derive(Default)]
pub struct Workflow {
    pub section: &'static str,
    pub layer: String,
    pub layer2: String,
    pub field: String,
    pub number: String,
    pub number2: String,
    /// The viewshed reads the ground between cells (bilinear heights), not the nearest cell.
    pub bilinear: bool,
    pub text: String,
    pub function: String,
    /// Slope output is degrees unless the user chooses percent rise.
    pub slope_percent: bool,
    pub chain: Vec<String>,
    pub format: String,
    pub pick: Option<Pick>,
    pub result: Option<Value>,
    pub error: Option<String>,
    /// Only this request may replace the Similar section's pending result.
    #[cfg(not(target_arch = "wasm32"))]
    similar_request: Option<SimilarRequest>,
    #[cfg(not(target_arch = "wasm32"))]
    similar_next: u64,
    pub script: String,
    pub script_out: Vec<(String, bool)>,
    /// The Script pane's last run, shown above its results.
    pub script_last: Option<ScriptRun>,
    /// The last run's Details window is open.
    pub script_details: bool,
    /// A script file picked with Load… in the browser or on a phone ([`script_picked`]), for
    /// [`RustGisApp::logic`] to open in the pane once the inbox is read ([`open_script`]): what
    /// keeps its undo needs the frame's context, which `open_picked` hasn't.
    pub(crate) script_picked: Option<String>,
    /// When each Script pane run under way or waiting started (seconds since 1970, and the
    /// instant), the project it was asked in ([`rustgis_engine::Session::opened`]) and the script it
    /// runs, in the order asked, for [`script_ran`] to tell as each lands: where it stopped is read
    /// in that script, whatever the box holds by then.
    script_started: std::collections::VecDeque<(u64, web_time::Instant, u64, String)>,
    /// While [`start_script`] asks for a run: one refused as it's asked lands at once, ahead of any
    /// still running, and is the last in [`Self::script_started`], not the first.
    script_asking: bool,
    /// When the last Script pane run landed: one waiting behind it started then, not when asked.
    script_landed: Option<web_time::Instant>,
    /// The project the Script pane's script, results and count are for
    /// ([`rustgis_engine::Session::opened`]); `None` until the pane first shows.
    script_project: Option<u64>,
    pub trend: TrendForm,
    /// The Spectral Analysis section's profile: the points read and charted.
    pub spectral: crate::spectralui::Spectral,
    pub selected_only: Option<bool>,
    /// The projected coordinate system the Geostatistical Wizard's surface is made in; empty: the
    /// environment's, else the projected one the layer holds its coordinates in, else the one it
    /// was read in, else degrees.
    pub crs: String,
    /// The system the Wizard's surface was last to be made in (its own, else the environment's):
    /// when it changes, the cell size typed for it is cleared.
    pub wizard_system: String,
    /// The systems the Wizard's layer holds its coordinates in and was read in, and the project's,
    /// and the one its surface is made in from them (the held one when projected,
    /// [`rustgis_engine::analysis::works_in`], else the read-in one,
    /// [`rustgis_engine::analysis::layer_raster_crs`], else the project's for a layer in degrees),
    /// kept so it is worked out once.
    pub layer_system: Option<(LayerSystems, Option<rustgis_engine::data::raster::Crs>)>,
    /// The coordinate system typed for a new ortho or reality workspace; empty: the images'
    /// projected system, else degrees.
    pub workspace_crs: String,
    /// The projected system that text last read as (`None`: none, or degrees), so a cell size
    /// typed for one system is cleared when another is typed, not at each keystroke.
    pub workspace_system: Option<rustgis_engine::data::raster::Crs>,
    /// The projected coordinate system typed for the environment's Set Coordinate System.
    pub env_crs: String,
    /// The cell size typed for the environment, apart from the Wizard's, which is the run's own.
    pub env_cell: String,
    pub zonal_stats: ZonalStatsForm,
    /// A workflow running on a thread of its own: its progress and Cancel show, and what it made
    /// lands through [`land_job`].
    #[cfg(not(target_arch = "wasm32"))]
    pub job: Option<Job>,
}

#[cfg(not(target_arch = "wasm32"))]
struct SimilarRequest {
    id: u64,
    opened: u64,
    map: u64,
    layer: String,
    instance: Option<u64>,
}

/// A pane/input change relinquishes ownership of a pending ranking. The read still completes
/// through its ordinary command, but cannot overwrite another section or a newer request.
pub(crate) fn forget_similar(workflow: &mut Workflow) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        workflow.similar_request = None;
    }
    #[cfg(target_arch = "wasm32")]
    let _ = workflow;
}

#[cfg(not(target_arch = "wasm32"))]
fn start_similar(app: &mut RustGisApp) {
    app.workflow.similar_next = app.workflow.similar_next.wrapping_add(1);
    let id = app.workflow.similar_next;
    app.workflow.similar_request = Some(SimilarRequest {
        id,
        opened: app.session.opened,
        map: app.session.map().instance_id(),
        layer: app.workflow.layer.clone(),
        instance: app.session.map().layer_by_name(&app.workflow.layer).map(|l| l.instance_id()),
    });
    (app.workflow.result, app.workflow.error) = (None, None);
    let params = json!({"layer": app.workflow.layer, "count": 10});
    // Success here is only an admission receipt. Completion owns success and every failure.
    let _ = crate::readjob::run_workflow_similar(app, params, id);
}

#[cfg(not(target_arch = "wasm32"))]
/// Delivers only to the still-current pane request; the admission receipt never becomes output.
pub(crate) fn similar_ran(app: &mut RustGisApp, request: u64, landed: crate::Landed) {
    if !app.workflow.similar_request.as_ref().is_some_and(|r| r.id == request) {
        return;
    }
    let Some(asked) = app.workflow.similar_request.take() else { return };
    if app.workflow.section != "similar"
        || app.workflow.layer != asked.layer
        || app.session.opened != asked.opened
        || app.session.map().instance_id() != asked.map
        || app.session.map().layer_by_name(&asked.layer).map(|l| l.instance_id()) != asked.instance
    {
        return;
    }
    match landed {
        crate::Landed::Ran(r) => show(app, r),
        crate::Landed::NotRun(why) => show(app, Err(why)),
    }
}

/// A workflow running on a thread of its own, so the window goes on while it works (#1291 step 3).
#[cfg(not(target_arch = "wasm32"))]
pub struct Job {
    /// What it is called, for its progress line.
    name: &'static str,
    /// Only this section owns the pane output; data and error notices still land elsewhere.
    section: &'static str,
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
    /// Where it stopped in its script, when its error names a line of it.
    pub stopped_at: Option<StoppedAt>,
}

/// Where a Script pane run stopped in its script (#2815): the line and, for JSON that doesn't
/// read, the column (1-based, in characters; [`rustgis_engine::cmd::workflows::script_error_at`]),
/// the box's cursor index there, and the hash of the script it ran, so the pane marks it only
/// while the box holds that script.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StoppedAt {
    pub line: usize,
    pub column: Option<usize>,
    pub index: usize,
    pub script: u64,
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

/// Asks where a desktop conversion goes before starting its export worker. Browser downloads
/// retain the text-returning command; that command refuses a streamed layer's partial view.
fn convert_layer(app: &mut RustGisApp) -> Result<(), String> {
    let format = app.workflow.format.clone();
    let ext = if format == "wkt" { "csv" } else { &format };
    let name = format!("{}.{ext}", app.workflow.layer);
    let mut params = json!({"layer": app.workflow.layer, "format": format});
    if app.services.download.is_some() {
        let answer = app.session.run("data.convert", &params)?;
        app.download_bytes(&name, answer["text"].as_str().unwrap_or("").as_bytes().to_vec());
    } else if let Some(path) = crate::dialogs::pick_save(&name, &[ext], tl!("Converted data")) {
        params["path"] = json!(path);
        #[cfg(not(target_arch = "wasm32"))]
        {
            crate::exportjob::start(app, "data.convert", params)?;
        }
        #[cfg(target_arch = "wasm32")]
        {
            app.session.run("data.convert", &params)?;
        }
    }
    Ok(())
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
#[cfg_attr(target_arch = "wasm32", expect(unused_variables, reason = "the browser has no background runs"))]
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

/// A background workflow changes only its own section's output. Its applied data redraws the
/// map either way, and failures remain visible as notices after the user changes sections.
#[cfg(not(target_arch = "wasm32"))]
fn show_job(app: &mut RustGisApp, section: &'static str, r: Result<Value, String>) {
    if app.workflow.section == section {
        show(app, r);
    } else {
        if let Err(error) = r {
            app.session.notify(error, true);
        }
        app.map.invalidate();
    }
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

/// Starts a command that makes a raster (`workflows::runs_in_background`) asked for outside the
/// pane through the app's dispatch (Command Search): on a thread of its own, the Workflows pane
/// opened to show its progress and Cancel, as from a section; its layer is added when it lands, a
/// failure then said in the status bar. The answer says it started; one that can't start (another
/// is under way) is an error.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn start_from_dispatch(app: &mut RustGisApp, command: &'static str, name: &'static str, p: Value) -> Result<Value, String> {
    // Its result is shown in the section the pane shows as it starts (the first, when it never
    // opened), as a run from that section's would be.
    settle_section(app);
    start(app, command, name, p);
    match &app.workflow.job {
        Some(job) if job.name == name && app.workflow.error.is_none() => {
            // Opened, not toggled: a pane already open stays so; one that didn't start doesn't open it.
            app.ui.open_right(crate::RightPane::Workflows);
            Ok(json!({"started": name}))
        }
        _ => Err(app.workflow.error.clone().unwrap_or_else(|| crate::i18n::fmt("{tool} didn't start", &[("tool", tl_raw!(name))]))),
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
            app.workflow.job = Some(Job { name, section: app.workflow.section, control, kind });
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
/// while one is still running. Pane output stays in its originating section; failures still
/// reach session notices after a section change.
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
                let section = job.section;
                app.workflow.job = None;
                dropped(app, section, name);
                return false;
            }
            filled(slot)
        }
        JobKind::Raster { id, slot, .. } => {
            // Likewise: its project was closed, so the raster it is making has nowhere to go.
            if !workflows::raster_waiting(&app.session, *id) {
                job.control.cancel();
                let name = job.name;
                let section = job.section;
                app.workflow.job = None;
                dropped(app, section, name);
                return false;
            }
            filled(slot)
        }
    };
    if !ready {
        return true;
    }
    let Some(job) = app.workflow.job.take() else { return false };
    // Cancelled once its work was done, before it landed (in the frame between): dropped, so
    // nothing is added, and a file it wrote beside its destination is removed.
    let cancelled = || Err(crate::i18n::fmt("{tool} was cancelled", &[("tool", tl_raw!(job.name))]));
    match job.kind {
        JobKind::Tool { id, slot } if job.control.is_cancelled() => {
            drop(take(&slot));
            gp::tool_abandon(&mut app.session, id);
            show_job(app, job.section, cancelled());
        }
        JobKind::Raster { id, slot, .. } if job.control.is_cancelled() => {
            drop(take(&slot));
            workflows::raster_abandon(&mut app.session, id);
            show_job(app, job.section, cancelled());
        }
        JobKind::Tool { id, slot } => {
            let Some(done) = take(&slot) else { return false };
            let r = gp::tool_complete(&mut app.session, id, done).and_then(|_| app.finish_gp(id));
            show_job(app, job.section, r);
        }
        JobKind::Raster { id, command, params, slot } => match take(&slot) {
            // Added by its command, as if it had run here.
            Some(Ok(made)) => match workflows::raster_land(&mut app.session, id, made) {
                Ok(true) => {
                    let result = app.finish_raster(command, id, &params);
                    show_job(app, job.section, result);
                }
                Ok(false) => dropped(app, job.section, job.name),
                Err(e) => show_job(app, job.section, Err(e)),
            },
            Some(Err(e)) => {
                workflows::raster_abandon(&mut app.session, id);
                show_job(app, job.section, Err(e));
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
fn dropped(app: &mut RustGisApp, section: &'static str, name: &'static str) {
    let error = crate::i18n::fmt("{tool} was stopped: its project was closed while it ran.", &[("tool", tl_raw!(name))]);
    show_job(app, section, Err(error));
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

/// The viewshed's parameters from the pane, seen from `at`: an empty radius field leaves the
/// radius out, so the command takes the whole DEM.
fn viewshed_params(w: &Workflow, at: rustgis_engine::geom::Coord) -> Value {
    let mut params = json!({"layer": w.layer, "lon": at.x, "lat": at.y});
    // A number is the height or radius; other text goes as it is, for the command to refuse,
    // naming it (`inf`, `NaN` and `1e400` parse, but aren't numbers JSON holds: they'd be null).
    // Empty: the command's own (1.7 m up; the whole raster).
    for (key, text) in [("height", &w.number), ("radius", &w.number2)] {
        match text.trim() {
            "" => {}
            t => params[key] = t.parse::<f64>().ok().filter(|v| v.is_finite()).map_or_else(|| json!(t), |v| json!(v)),
        }
    }
    if w.bilinear {
        params["heights"] = json!("bilinear");
    }
    params
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

/// The extension (#1752) that owns Workflows section `section`: it is hidden while that
/// extension is off.
pub fn section_extension(section: &str) -> Option<&'static str> {
    match section {
        "georeference" | "change" | "spectral" | "classify" | "supervised" | "deepLearning" | "hosted" | "indices" | "process"
        | "pixels" | "mensuration" | "ortho" | "reality" => Some(rustgis_engine::extensions::IMAGERY.id),
        _ => None,
    }
}

/// Whether Workflows section `section` shows: its extension, if it has one, is on.
pub fn section_shown(session: &rustgis_engine::Session, section: &str) -> bool {
    section_extension(section).is_none_or(|e| session.extensions().is_on(e))
}

/// Why Workflows section `section` can't open: its extension is off. `None` when it can.
pub fn section_blocked(session: &rustgis_engine::Session, section: &str) -> Option<String> {
    let e = section_extension(section).and_then(rustgis_engine::extensions::get)?;
    (!session.extensions().is_on(e.id)).then(|| {
        crate::i18n::fmt(
            "{section} is part of the {name} extension, which is off; turn it on in Plugins › Extensions",
            &[("section", tl_raw!(SECTIONS.iter().find(|s| s.0 == section).map_or(section, |s| s.1))), ("name", tl_raw!(e.name))],
        )
    })
}

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

/// The section the pane shows: none yet (the pane never opened), or one whose extension was turned
/// off while it was open, is the first.
fn settle_section(app: &mut RustGisApp) {
    if app.workflow.section.is_empty() || !section_shown(&app.session, app.workflow.section) {
        // Its map pick ends with it (a pick started elsewhere, with no section open, stays).
        if !app.workflow.section.is_empty() {
            app.workflow.pick = None;
        }
        forget_similar(&mut app.workflow);
        app.workflow.section = "rasterFunctions";
    }
}

/// Draws the active workflow form; raster functions share their step parameters with the
/// Function Editor so Apply and Add Step honor the same choices.
pub fn workflows(app: &mut RustGisApp, ui: &mut Ui, t: &Tokens) {
    settle_section(app);
    egui::ComboBox::from_id_salt("wf_section")
        .selected_text(tl!(SECTIONS.iter().find(|s| s.0 == app.workflow.section).map_or("", |s| s.1)))
        .width(ui.available_width() - 8.0)
        .show_ui(ui, |ui| {
            for (id, title) in SECTIONS.iter().filter(|s| section_shown(&app.session, s.0)) {
                if ui.selectable_label(app.workflow.section == *id, tl!(*title)).clicked()
                    && (app.workflow.section != "similar" || *id != "similar")
                {
                    forget_similar(&mut app.workflow);
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
        let layer = inputs.first().cloned().unwrap_or_default();
        if w.section == "similar" && w.layer != layer {
            forget_similar(w);
            (w.result, w.error) = (None, None);
        }
        w.layer = layer;
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
            if app.workflow.function == "slope" {
                ui.horizontal(|ui| {
                    let label = ui.label(tl!("Slope units"));
                    egui::ComboBox::from_id_salt("wf_slope_units")
                        .selected_text(if app.workflow.slope_percent { tl!("Percent") } else { tl!("Degrees (0–90)") })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut app.workflow.slope_percent, false, tl!("Degrees (0–90)"));
                            ui.selectable_value(&mut app.workflow.slope_percent, true, tl!("Percent"));
                        })
                        .response
                        .labelled_by(label.id);
                });
            }
            let mut step =
                json!({"function": app.workflow.function, "expression": app.workflow.text, "value": num(&app.workflow.number, 0.0)});
            if app.workflow.function == "slope" {
                step["units"] = json!(if app.workflow.slope_percent { "percent" } else { "degrees" });
            }
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
                    // A constant raster gives empty middle ranges: they match nothing, so leave them out.
                    let ranges: Vec<Value> = (0..10)
                        .filter_map(|i| {
                            let lo = if i == 0 { f64::MIN } else { st.0 + step * i as f64 };
                            let hi = if i == 9 { f64::MAX } else { st.0 + step * (i + 1) as f64 };
                            (lo < hi).then(|| json!([lo, hi, if high { i + 1 } else { 10 - i }]))
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
            // How the ground along each ray is read.
            ui.horizontal(|ui| {
                ui.label(tl!("Ground heights"));
                let (nearest, between) = (tl!("Nearest cell"), tl!("Between cells (bilinear)"));
                egui::ComboBox::from_id_salt("wf_v_heights")
                    .selected_text(if app.workflow.bilinear { between } else { nearest })
                    .width(170.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut app.workflow.bilinear, false, nearest);
                        ui.selectable_value(&mut app.workflow.bilinear, true, between);
                    })
                    .response
                    .on_hover_text(tl!(
                        "How the ground between the observer and a cell is read: the cell each point falls in, or between the cells' centres. Between cells is an estimate too, but closer on ground that bulges, a hill seen from its top, where the nearest cell can hide cells just inside the horizon."
                    ));
            });
            pick_button(app, ui, "viewshed", 1, "Click the observer on the map");
            if let Some(p) = picked(app, "viewshed").and_then(|v| v.first().copied()) {
                let params = viewshed_params(&app.workflow, p);
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
            // Empty: the environment's system, else the projected one the layer holds its
            // coordinates in, else the one it was read in, else the project's, else degrees; the
            // hint shows which.
            let project = app.session.project.coordinate_system;
            let systems = app.session.map().layer_by_name(&app.workflow.layer).map(|l| (l.dataset.coord_crs(), l.dataset.crs, project));
            // The run's choice; kept for the layer's systems, so a projection isn't built every
            // frame.
            let layer_crs = match (systems, app.workflow.layer_system) {
                (Some(c), Some((seen, chosen))) if seen == c => chosen,
                (Some((held, read_in, project)), _) => {
                    use rustgis_engine::analysis::layer_raster_crs;
                    let from = if rustgis_engine::analysis::works_in(held) { Some(held) } else { read_in };
                    let chosen = match layer_raster_crs(from) {
                        Some(c) => c.ok(),
                        None => layer_raster_crs(project).and_then(Result::ok),
                    };
                    app.workflow.layer_system = Some(((held, read_in, project), chosen));
                    chosen
                }
                (None, _) => None,
            };
            let empty_crs = app.session.project.gp_env.projected.map(|g| g.crs).or(layer_crs);
            let degrees = app.workflow.crs.trim().is_empty() && empty_crs.is_none();
            ui.add(
                egui::TextEdit::singleline(&mut app.workflow.crs)
                    .hint_text(match empty_crs {
                        Some(c) => c.label(),
                        None => tl!("projected system (EPSG:32616), or degrees").to_string(),
                    })
                    .desired_width(f32::INFINITY),
            );
            // A cell size typed for one system means nothing in another: when the system the surface
            // will be made in changes (typed here, or the environment's), the size is cleared.
            let system = match app.workflow.crs.trim() {
                "" => empty_crs.map(|c| c.code()).unwrap_or_default(),
                typed => typed.to_string(),
            };
            if system != app.workflow.wizard_system {
                app.workflow.number.clear();
                app.workflow.wizard_system = system;
            }
            ui.horizontal(|ui| {
                let hint = if degrees { tl!("cell size °") } else { tl!("cell size") };
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(hint).desired_width(80.0));
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number2).hint_text(tl!("power 2")).desired_width(60.0));
            });
            if ui.button(tl!("Run")).clicked() {
                let mut p = json!({"layer": app.workflow.layer, "field": app.workflow.field, "power": num(&app.workflow.number2, 2.0)});
                if let Ok(c) = app.workflow.number.trim().parse::<f64>() {
                    p["cellSize"] = json!(c);
                }
                if !app.workflow.crs.trim().is_empty() {
                    p["crs"] = json!(app.workflow.crs.trim());
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
            let before = app.workflow.layer.clone();
            combo(ui, "wf_e", "Layer", &mut app.workflow.layer, &features);
            if app.workflow.layer != before {
                forget_similar(&mut app.workflow);
                (app.workflow.result, app.workflow.error) = (None, None);
            }
            if ui.button(tl!("Find Similar")).clicked() {
                #[cfg(not(target_arch = "wasm32"))]
                start_similar(app);
                #[cfg(target_arch = "wasm32")]
                run(app, "data.similar", json!({"layer": app.workflow.layer, "count": 10}));
            }
            #[cfg(not(target_arch = "wasm32"))]
            if app.workflow.similar_request.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tl!("Running…"));
                });
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
                app.workflow.error = convert_layer(app).err();
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
            let system =
                env.projected.map(|g| g.crs.label()).unwrap_or_else(|| tl!("the layer's projected system, else degrees").to_string());
            let cells = env.cell_in(env.projected.map_or(rustgis_engine::data::raster::Crs::LonLat, |g| g.crs));
            // A size in degrees is used only for a surface in degrees: said so, also beside a system
            // with no size of its own.
            let degrees = |c: f64| crate::i18n::fmt("{c}° (in degrees only)", &[("c", &c.to_string())]);
            let cells = match (cells, env.projected) {
                (Some(c), Some(_)) => c.to_string(),
                (Some(c), None) => degrees(c),
                (None, _) => match env.cell_size {
                    Some(d) => crate::i18n::fmt("the tool's own; {d}", &[("d", &degrees(d))]),
                    None => tl!("the tool's own").to_string(),
                },
            };
            ui.label(
                egui::RichText::new(crate::i18n::fmt("Rasters: {system}, cell size {cells}", &[("system", &system), ("cells", &cells)]))
                    .small(),
            );
            ui.horizontal_wrapped(|ui| {
                let name = ui.label(tl!("Projected system"));
                let recent = app.ui.recent_crs.clone();
                // A row taken fills the box; Set Coordinate System applies it, since another
                // system starts without the cell size set for this one.
                let (edit, _) = crate::crspicker::crs_box(
                    ui,
                    "environment",
                    &mut app.workflow.env_crs,
                    &recent,
                    crate::crspicker::Offer::Projected,
                    130.0,
                    "EPSG:32616",
                    t,
                );
                // Named by the label beside it, for a screen reader.
                edit.labelled_by(name.id);
                // An empty box sets nothing; clearing the system is its own button, so a click never
                // clears it and its cell size by accident. Cleared, a run takes the layer's system.
                let typed = app.workflow.env_crs.trim().to_string();
                if ui.add_enabled(!typed.is_empty(), egui::Button::new(tl!("Set Coordinate System"))).clicked() {
                    let crs = crate::dialogs::typed_crs(&typed);
                    run(app, "gp.env", json!({"crs": crs}));
                    if app.workflow.error.is_none() {
                        app.remember_crs_if_listed(&crs);
                    }
                }
                if ui.add_enabled(env.projected.is_some(), egui::Button::new(tl!("Clear Coordinate System"))).clicked() {
                    run(app, "gp.env", json!({"crs": null}));
                }
            });
            ui.horizontal(|ui| {
                let hint = if env.projected.is_some() { tl!("cell size") } else { tl!("cell size °") };
                ui.add(egui::TextEdit::singleline(&mut app.workflow.env_cell).hint_text(hint).desired_width(80.0));
                if ui.button(tl!("Set Cell Size")).clicked() {
                    // Empty clears the size; text that isn't a number is refused, not taken as empty.
                    let typed = app.workflow.env_cell.trim().to_string();
                    match typed.parse::<f64>() {
                        // `inf` and `NaN` parse, but aren't sizes (and go out as null, which clears).
                        Ok(v) if v.is_finite() => run(app, "gp.env", json!({"cellSize": v})),
                        Err(_) if typed.is_empty() => run(app, "gp.env", json!({"cellSize": null})),
                        _ => {
                            app.workflow.error = Some(crate::i18n::fmt("'{size}' isn't a cell size: give a number", &[("size", &typed)]));
                        }
                    }
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
            // In the map's own system on a flat map, so the raster fills the view as drawn.
            if ui.button(tl!("Fit To Map View")).clicked() {
                run(app, "raster.georeference", json!({"layer": app.workflow.layer, "extent": "view"}));
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
        "spectral" => crate::spectralui::section(app, ui, t),
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
                    // A blank box is the default; what is typed goes as it is, for the command to
                    // read or refuse (a fraction or text isn't cut or replaced without a word).
                    let mut p = json!({"layer": app.workflow.layer, "training": app.workflow.layer2, "classField": app.workflow.field});
                    let typed = app.workflow.number.trim();
                    if !typed.is_empty() {
                        p["epochs"] = json!(typed);
                    }
                    run(app, "dl.classify", p);
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
            // The hints say the grid the workspace gets: without a system, a cell size is in
            // degrees and keeps the mosaic in degrees, as in older projects.
            // Read once as it's typed (below), not each frame.
            let degrees = app.workflow.workspace_system.is_none();
            let crs_hint = if app.workflow.number.trim().is_empty() {
                tl!("the images' projected system, else degrees")
            } else {
                tl!("degrees, as the cell size is in degrees")
            };
            let system_label = ui.label(tl!("Coordinate system"));
            let system = ui
                .add(egui::TextEdit::singleline(&mut app.workflow.workspace_crs).hint_text(crs_hint).desired_width(f32::INFINITY))
                .labelled_by(system_label.id)
                .on_hover_text(tl!("A projected system such as EPSG:32616, or EPSG:4326 for degrees"));
            // A cell size typed for another system isn't sent in this one's units: it is cleared
            // once the text reads as another system, and kept while it reads as none yet.
            if system.changed() {
                let typed = app.workflow.workspace_crs.trim();
                let now = if typed.is_empty() {
                    Some(None)
                } else {
                    rustgis_engine::data::raster::Crs::parse(typed)
                        .ok()
                        .map(|c| Some(c).filter(|c| *c != rustgis_engine::data::raster::Crs::LonLat))
                };
                if let Some(now) = now
                    && now != app.workflow.workspace_system
                {
                    app.workflow.workspace_system = now;
                    app.workflow.number.clear();
                }
            }
            ui.horizontal(|ui| {
                let hint = if degrees { tl!("cell size °") } else { tl!("cell size") };
                let size_label = ui.label(tl!("Cell size"));
                ui.add(egui::TextEdit::singleline(&mut app.workflow.number).hint_text(hint).desired_width(80.0))
                    .labelled_by(size_label.id)
                    .on_hover_text(tl!("In the coordinate system's units, or degrees without one"));
            });
            // On a line of its own: beside the cell size, its label ran past the pane's edge.
            if ui.button(tl!("New Workspace From Visible Rasters")).clicked() {
                let images: Vec<String> =
                    app.session.map().layers.iter().filter(|l| l.is_raster() && l.visible).map(|l| l.name.clone()).collect();
                let mut p = json!({"kind": kind, "images": images});
                if !app.workflow.workspace_crs.trim().is_empty() {
                    p["crs"] = json!(app.workflow.workspace_crs.trim());
                }
                // Text that isn't a number goes as it is, for the command to say why.
                let size = app.workflow.number.trim();
                if !size.is_empty() {
                    p["cellSize"] = size.parse::<f64>().ok().filter(|c| c.is_finite()).map_or_else(|| json!(size), |c| json!(c));
                }
                run(app, "imagery.workspace", p);
            }
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
    // The spectral profile picks on until turned off: its button shows it, with no count.
    if let Some(p) = app.workflow.pick.as_ref().filter(|p| p.purpose != crate::spectralui::PURPOSE) {
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
/// closed. Output stays with the Trend section; failures elsewhere become notices.
/// `true` while one is still running.
#[cfg(not(target_arch = "wasm32"))]
pub fn land_trend(app: &mut RustGisApp) -> bool {
    use std::sync::atomic::Ordering;
    let Some(r) = &app.workflow.trend.running else { return false };
    let Some(res) = r.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() else { return true };
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
            show_job(app, "trend", Err(tl!("The trend finished after another project was opened, so it wasn't added").to_string()));
        }
        Ok((rasters, dates)) => {
            p["ran"] = json!(rustgis_engine::cmd::cube::land(&mut app.session, rasters, dates));
            let result = app.run("cube.trend", p);
            show_job(app, "trend", result);
        }
        Err(_) if cancelled => {}
        Err(e) => show_job(app, "trend", Err(e)),
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
    keep_for_undo(app, ui.ctx(), box_id);
    clear_script(app);
    ui.memory_mut(|m| m.request_focus(box_id));
}

/// The Script box's script kept as an undo step of the box, its cursor at the start, before
/// something else replaces it (Clear, Open in Script Pane).
fn keep_for_undo(app: &RustGisApp, ctx: &egui::Context, box_id: egui::Id) {
    use egui::text::{CCursor, CCursorRange};
    let mut state = egui::widgets::text_edit::TextEditState::load(ctx, box_id).unwrap_or_default();
    let mut undoer = state.undoer();
    let end = CCursorRange::one(CCursor::new(app.workflow.script.chars().count()));
    undoer.add_undo(&(state.cursor.char_range().unwrap_or(end), app.workflow.script.clone()));
    state.set_undoer(undoer);
    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(0))));
    state.store(ctx, box_id);
}

/// Whether a Script pane run is under way or waiting: what lands from it would land in a script
/// replaced meanwhile.
pub(crate) fn script_running(app: &RustGisApp) -> bool {
    !app.workflow.script_started.is_empty()
}

/// Opens `script` in the Script pane (the session log's Open in Script Pane), in place of the
/// pane's script and results, as Clear empties them: the script that was there comes back with
/// Ctrl+Z, even one left from a project since replaced (kept first, then the pane started over,
/// so that doesn't empty it when it next shows). Nothing while a run is under way
/// ([`script_running`]).
pub(crate) fn open_script(app: &mut RustGisApp, ctx: &egui::Context, script: String) {
    if script_running(app) {
        return;
    }
    let box_id = egui::Id::unique("script_text");
    keep_for_undo(app, ctx, box_id);
    start_over_in_another_project(app);
    clear_script(app);
    app.workflow.script = script;
    app.ui.open_right(crate::RightPane::Script);
    ctx.memory_mut(|m| m.request_focus(box_id));
}

/// Load…: a script file read into the Script pane in place of its script ([`open_script`]). In
/// the browser and on a phone the shell's picker answers later, through [`script_picked`].
fn load_script(app: &mut RustGisApp, ctx: &egui::Context) {
    if let Some(open) = &app.services.open_async {
        open("script");
        return;
    }
    let Some(path) = crate::dialogs::pick_open(&["txt"], tl!("Script")) else { return };
    // One byte past the limit is enough to refuse it, whatever the file's size.
    let read = std::fs::File::open(&path).and_then(|file| {
        use std::io::Read;
        let mut bytes = Vec::new();
        file.take(crate::SCRIPT_FILE_MAX_BYTES as u64 + 1).read_to_end(&mut bytes).map(|_| bytes)
    });
    let name = std::path::Path::new(&path).file_name().map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
    let unread = |e: std::io::Error| crate::i18n::fmt("{path}: {error}", &[("path", &path), ("error", &e.to_string())]);
    match read.map_err(unread).and_then(|bytes| script_text(app, &name, &bytes)) {
        Ok(script) => open_script(app, ctx, script),
        Err(e) => app.session.notify(e, true),
    }
}

/// A script file the shell's picker read for Load…, kept for [`RustGisApp::logic`] to open once
/// the inbox is read. A file [`script_text`] refuses is said, and the pane's script stays.
pub(crate) fn script_picked(app: &mut RustGisApp, name: &str, bytes: &[u8]) -> Result<Value, String> {
    let script = script_text(app, name, bytes).inspect_err(|e| app.session.notify(e.clone(), true))?;
    app.workflow.script_picked = Some(script);
    Ok(Value::Null)
}

/// Script file `name`'s text for the Script pane, without the byte-order mark some editors
/// start a UTF-8 file with (it would make the first line's command unknown): refused when it is
/// over [`crate::SCRIPT_FILE_MAX_BYTES`] or isn't UTF-8 text, or while a run is under way (its
/// results would land in the replaced script).
fn script_text(app: &RustGisApp, name: &str, bytes: &[u8]) -> Result<String, String> {
    if script_running(app) {
        return Err(tl!("Wait for the run to finish").to_string());
    }
    if bytes.len() > crate::SCRIPT_FILE_MAX_BYTES {
        return Err(crate::i18n::fmt("{file} is larger than 1 MB", &[("file", name)]));
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    std::str::from_utf8(bytes).map(str::to_owned).map_err(|_| crate::i18n::fmt("{file} isn't UTF-8 text", &[("file", name)]))
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

/// The height the Script pane needs below its box: the cursor's line and column, the buttons, the
/// last run's line, and its results (their own scroll, at most SCRIPT_OUT_ROWS lines) when there
/// are any.
fn script_below(app: &RustGisApp, ui: &Ui) -> f32 {
    let gap = ui.spacing().item_spacing.y;
    // A row: as tall as a button, whatever its text.
    let last = if app.workflow.script_last.is_none() { 0.0 } else { ui.spacing().interact_size.y + gap };
    let results = if app.workflow.script_out.is_empty() { 0.0 } else { script_out_height(ui) + gap };
    // The row that says where the cursor is (#2831).
    let cursor = script_cursor_height(ui) + gap;
    ui.spacing().interact_size.y + gap * 3.0 + cursor + last + results
}

/// Runs the Script pane's `script`: on the desktop its tools on a thread of their own first, the
/// status bar showing it and Cancel, the results saying so until it lands ([`script_ran`]).
fn start_script(app: &mut RustGisApp, script: String) {
    // When it started and how long it took: a tool run outside the app in the browser
    // (`gp.finish`) ends later.
    let at = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    app.workflow.script_started.push_back((at, web_time::Instant::now(), app.session.opened, script.clone()));
    app.workflow.script_out = vec![(tl!("Running…").to_string(), false)];
    // Its error, which the mark goes with, is no longer shown.
    if let Some(last) = app.workflow.script_last.as_mut() {
        last.stopped_at = None;
    }
    app.workflow.script_asking = true;
    #[cfg(not(target_arch = "wasm32"))]
    let _ = crate::readjob::run_then(app, "script.run", json!({"script": script}), Some(script_ran));
    #[cfg(target_arch = "wasm32")]
    {
        let r = app.run("script.run", json!({"script": script}));
        script_ran(app, crate::Landed::Ran(r));
    }
    app.workflow.script_asking = false;
}

/// A Script pane run as it lands, in the pane: its number, how long it took, and each line's
/// result, or why it didn't run.
fn script_ran(app: &mut RustGisApp, landed: crate::Landed) {
    let now = web_time::Instant::now();
    let opened = app.session.opened;
    let queue = &mut app.workflow.script_started;
    let this = if std::mem::take(&mut app.workflow.script_asking) { queue.pop_back() } else { queue.pop_front() };
    let (asked_at, asked, asked_in, ran_script) = this.unwrap_or_else(|| (0, now, opened, app.workflow.script.clone()));
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
    // Read in the script that ran, which the box may no longer hold: typed over meanwhile, the
    // mark waits for that text to come back (undo).
    let stopped_at =
        error.as_deref().and_then(|e| rustgis_engine::cmd::workflows::script_error_at(&ran_script, e)).map(|(line, column)| StoppedAt {
            line,
            column,
            index: stopped_index(&ran_script, line, column),
            script: egui::util::hash(&ran_script),
        });
    app.workflow.script_last = Some(ScriptRun { n, secs: now.duration_since(started).as_secs_f64(), at, ran, error, stopped_at });
    // Another still to land: the results say so first, above this one's, where their scroll
    // starts, however many lines it ran. One asked in a project this run replaced is left, not
    // run, so it isn't told.
    if app.workflow.script_started.iter().any(|&(_, _, asked_in, _)| asked_in == opened) {
        app.workflow.script_out.insert(0, (tl!("Running…").to_string(), false));
    }
    app.map.invalidate();
}

/// The most height a run's results take.
fn script_out_height(ui: &Ui) -> f32 {
    (ui.text_style_height(&egui::TextStyle::Small) + ui.spacing().item_spacing.y) * SCRIPT_OUT_ROWS
}

/// The height of the row that says where the Script box's cursor is: a line of small text.
fn script_cursor_height(ui: &Ui) -> f32 {
    ui.text_style_height(&egui::TextStyle::Small)
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
    let before = crate::completion::before(ui, box_id, &app.workflow.script);
    let stopped = stopped_at(app);
    let out = egui::ScrollArea::vertical().id_salt("script_box").max_height(room).auto_shrink([false, true]).show(ui, |ui| {
        let out = egui::TextEdit::multiline(&mut app.workflow.script)
            .id(box_id)
            .code_editor()
            .event_filter(crate::scriptcomplete::filter(ui, box_id))
            .layouter(&mut layouter)
            .desired_rows(SCRIPT_MIN_ROWS)
            .desired_width(f32::INFINITY)
            .hint_text("map.goToXY {\"lon\": 2.35, \"lat\": 48.85, \"scale\": 5000000}\nselect.byAttributes {\"layer\": \"Cities\", \"where\": \"pop_max > 1000000\"}")
            .show(ui);
        if let Some(at) = stopped {
            paint_stopped(ui, &out, at);
            // Once, asked by a click on the error: inside this scroll, which takes it as it ends.
            // The box scrolls to its cursor only when the cursor moves within it.
            if ui.data_mut(|d| d.remove_temp::<bool>(box_id.with("go_to_stopped"))).is_some() {
                let origin = (out.galley_pos - vec2(out.galley.rect.left(), 0.0)).to_vec2();
                let cursor = out.galley.pos_from_cursor(egui::text::CCursor::new(at.index)).translate(origin);
                ui.scroll_to_rect(cursor, Some(egui::Align::Center));
            }
        }
        out
    });
    let clicked = crate::scriptcomplete::show(ui, box_id, &out.inner, out.inner_rect, &app.workflow.script, &app.session);
    crate::scriptcomplete::hint(ui, box_id, &out.inner, out.inner_rect, &app.workflow.script, &app.session);
    if let Some((offer, item)) = clicked {
        crate::scriptcomplete::accept(ui, box_id, &mut app.workflow.script, &offer, &item, &app.session);
    } else {
        crate::scriptcomplete::braces(ui, before, &out.inner, &mut app.workflow.script);
    }
    // Where the box's cursor is (#2831), focused or not, on the right of a row of its own: the
    // row of buttons has no room for it in a narrow pane. The row is there before the box has a
    // cursor too, so nothing below moves when it gets one. Read from the box's state as it is
    // now, not from `out`: a suggestion accepted from the list moves the cursor after the box, and
    // `out.cursor_range` is None while the box isn't focused.
    let size = vec2(ui.available_width(), script_cursor_height(ui));
    ui.allocate_ui_with_layout(size, egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // The row keeps only what's drawn in it: its height, with no label in it yet.
        ui.set_min_height(size.y);
        let cursor = egui::widgets::text_edit::TextEditState::load(ui.ctx(), box_id).and_then(|s| s.cursor.char_range());
        if let Some(range) = cursor {
            let (line, col) = line_col(&app.workflow.script, range.primary.index.0);
            let at = crate::i18n::fmt("Ln {line}, Col {col}", &[("line", &line.to_string()), ("col", &col.to_string())]);
            ui.label(egui::RichText::new(at).small().weak()).on_hover_text(tl!("The cursor's line and column"));
        }
    });
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
        // Not while a run is under way: its results would land in the replaced or emptied pane.
        let running = script_running(app);
        let undo = if mac { "⌘+Z" } else { "Ctrl+Z" };
        let mut load = ui
            .add_enabled(!running, egui::Button::new(tl!("Load…")))
            .on_hover_text(crate::i18n::fmt("Read a script from a file in place of this one ({keys} brings it back)", &[("keys", undo)]));
        if running {
            load = load.on_disabled_hover_text(tl!("Wait for the run to finish"));
        }
        if load.clicked() {
            load_script(app, ui.ctx());
        }
        let w = &app.workflow;
        let anything = !w.script.is_empty() || !w.script_out.is_empty() || w.script_last.is_some();
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
            let text = egui::RichText::new(line.chars().take(SCRIPT_PREVIEW).collect::<String>()).monospace().small().color(c);
            // The error, while the box holds the script that stopped, goes to where it did.
            match stopped.filter(|_| *err) {
                Some(at) => {
                    let go = ui
                        .add(egui::Label::new(text).sense(egui::Sense::click()))
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(crate::i18n::fmt("Go to line {line}", &[("line", &at.line.to_string())]));
                    if go.clicked() {
                        go_to_stopped(ui, box_id, at.index);
                    }
                }
                None => {
                    ui.label(text);
                }
            }
        }
    });
}

/// Where the last run stopped, while the box holds the script it ran: a hash of the script each
/// frame, only after a run that stopped (the colours' cache hashes it every frame already).
fn stopped_at(app: &RustGisApp) -> Option<StoppedAt> {
    let at = app.workflow.script_last.as_ref()?.stopped_at?;
    (at.script == egui::util::hash(&app.workflow.script)).then_some(at)
}

/// The box's cursor index (in characters) of `line`'s `column` in `script` (1-based), else of the
/// line's start.
fn stopped_index(script: &str, line: usize, column: Option<usize>) -> usize {
    let before: usize = script.split_inclusive('\n').take(line.saturating_sub(1)).map(|l| l.chars().count()).sum();
    before + column.map_or(0, |c| c.saturating_sub(1))
}

/// The line and column (1-based, in characters) of the box's cursor `index` in `script`: the
/// script's lines, a line that wraps being one, as a run's error names them ([`stopped_index`]
/// the other way). An index past the end, left from a text since replaced, counts to the end. The
/// `\r` of a Windows line end, which a script loaded from a file keeps, has no column of its own.
/// Counted each frame, up to the cursor: less than the colours' cache, which hashes the whole
/// script each frame.
fn line_col(script: &str, index: usize) -> (usize, usize) {
    script.chars().take(index).fold((1, 1), |(line, col), c| match c {
        '\n' => (line + 1, 1),
        '\r' => (line, col),
        _ => (line, col + 1),
    })
}

/// Where the last run stopped, marked in the box (#2815): its line's rows tinted red across the
/// box, and for JSON that doesn't read, its column underlined. Over the text, translucent: the box
/// paints its own background over anything drawn before it.
fn paint_stopped(ui: &Ui, out: &egui::text_edit::TextEditOutput, at: StoppedAt) {
    let galley = &out.galley;
    let origin = (out.galley_pos - vec2(galley.rect.left(), 0.0)).to_vec2();
    let clip = out.text_clip_rect;
    let painter = ui.painter().with_clip_rect(clip.intersect(ui.clip_rect()));
    let mut line = 1;
    for row in &galley.rows {
        if line == at.line {
            let y = row.rect().translate(origin).y_range();
            painter.rect_filled(egui::Rect::from_x_y_ranges(clip.x_range(), y), 0.0, SCRIPT_ERROR.gamma_multiply(0.2));
        }
        line += usize::from(row.ends_with_newline);
        if line > at.line {
            break;
        }
    }
    if at.column.is_some() {
        use egui::text::CCursor;
        let a = galley.pos_from_cursor(CCursor::new(at.index)).translate(origin);
        let b = galley.pos_from_cursor(CCursor::new(at.index + 1)).translate(origin);
        // The next character on the same row, else a character's width.
        let width = if b.min.y == a.min.y && b.min.x > a.min.x { b.min.x - a.min.x } else { a.height() * 0.5 };
        let y = a.bottom() - 1.0;
        painter.line_segment([egui::pos2(a.min.x, y), egui::pos2(a.min.x + width, y)], egui::Stroke::new(2.0, SCRIPT_ERROR));
    }
}

/// The box's cursor put where the last run stopped, the box focused, and asked to scroll there
/// on the next frame (as it lays the box out: [`paint_stopped`]'s caller).
fn go_to_stopped(ui: &Ui, box_id: egui::Id, index: usize) {
    use egui::text::{CCursor, CCursorRange};
    let mut state = egui::widgets::text_edit::TextEditState::load(ui.ctx(), box_id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(index))));
    state.store(ui.ctx(), box_id);
    ui.memory_mut(|m| m.request_focus(box_id));
    ui.data_mut(|d| d.insert_temp(box_id.with("go_to_stopped"), true));
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

    /// An empty radius field (or one of spaces) runs the viewshed over the whole DEM, about
    /// 630 km long: one whose radius square, the DEM's diagonal each way, holds far more cells
    /// than the DEM.
    #[test]
    fn a_viewshed_with_an_empty_radius_field_sees_the_whole_dem() {
        use base64::Engine;
        use rustgis_engine::geom::Coord;
        let mut app = RustGisApp::with_sample();
        use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
        // A strip 4,000 cells long and 2 high, 0.002° (about 160 m) a cell: about 630 km. The
        // DEM's diagonal each way spans 8,001 × 8,001 cells, 64 million, of which 8,000 are the
        // DEM's.
        let (w, h) = (4_000u32, 2u32);
        let gt = GeoTransform { origin_x: 10.0, origin_y: 45.004, pixel_width: 0.002, pixel_height: -0.002 };
        let dem =
            Raster::new("strip", w, h, vec![vec![200.0; (w * h) as usize]], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(geotiff::write(&dem, 0).unwrap());
        app.session.run("layer.addDataBytes", &json!({"fileName": "strip.tif", "data": data})).unwrap();
        let layer = app.session.map().layers.iter().rev().find(|l| l.is_raster()).unwrap().name.clone();
        // Cells of a viewshed worked out (not left no-data), and whether the strip's far end is.
        let worked = |app: &RustGisApp, name: &str| {
            let r = app.session.map().layer_by_name(name).and_then(|l| l.raster.as_ref()?.data.clone()).unwrap();
            let n = (0..h as i64)
                .flat_map(|row| (0..w as i64).map(move |col| (col, row)))
                .filter(|&(c, rr)| r.value(0, c, rr).is_some())
                .count();
            (n, r.value(0, i64::from(w) - 1, 0).is_some())
        };
        let at = Coord::new(10.001, 45.001);
        // A radius given short of the far end leaves it out; one past the DEM's diagonal works
        // out every cell, as an empty field does.
        app.session
            .run("analysis.viewshed", &json!({"layer": layer, "lon": at.x, "lat": at.y, "height": 2.0, "radius": 100_000, "name": "short"}))
            .unwrap();
        assert!(!worked(&app, "short").1);
        app.session
            .run("analysis.viewshed", &json!({"layer": layer, "lon": at.x, "lat": at.y, "height": 2.0, "radius": 1e9, "name": "given"}))
            .unwrap();
        assert_eq!(worked(&app, "given"), ((w * h) as usize, true));
        for (i, radius) in ["", "   "].into_iter().enumerate() {
            let workflow = Workflow { layer: layer.clone(), number: "2".into(), number2: radius.into(), ..Default::default() };
            let mut p = viewshed_params(&workflow, at);
            assert!(p.get("radius").is_none(), "radius {radius:?}: {p}");
            let name = format!("empty{i}");
            p["name"] = json!(name);
            app.session.run("analysis.viewshed", &p).unwrap_or_else(|e| panic!("radius {radius:?}: {e}"));
            assert_eq!(worked(&app, &name), ((w * h) as usize, true), "radius {radius:?}: every cell worked out");
        }
        // A number in the field is the radius; other text is refused, not taken as no radius.
        let workflow = Workflow { layer: layer.clone(), number2: " 5000 ".into(), ..Default::default() };
        assert_eq!(viewshed_params(&workflow, Coord::new(10.0005, 45.0005))["radius"], json!(5000.0));
        // So is the height, and an empty one is the command's 1.7 m.
        for (text, e) in [("abc", r#"not "abc""#), ("1,7", r#"not "1,7""#), ("-3", "not -3.0")] {
            let workflow = Workflow { layer: layer.clone(), number: text.into(), ..Default::default() };
            let got = app.session.run("analysis.viewshed", &viewshed_params(&workflow, at)).unwrap_err();
            assert!(got.starts_with("height must be a number of metres from 0 to 10,000") && got.ends_with(e), "{got}");
        }
        assert!(viewshed_params(&Workflow { layer: layer.clone(), ..Default::default() }, at).get("height").is_none());
        for text in ["500km", "inf", "NaN", "1e400", "-infinity"] {
            let workflow = Workflow { layer: layer.clone(), number2: text.into(), ..Default::default() };
            let e = app.session.run("analysis.viewshed", &viewshed_params(&workflow, at)).unwrap_err();
            assert_eq!(e, format!(r#"radius must be a number of metres above 0, not "{text}""#));
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn annotation_grid_script_pane_receives_the_frame_after_worker_delivery() {
        use rustgis_engine::data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff};
        for off_frame in [false, true] {
            let raster = Raster::new(
                "Image",
                2,
                3,
                vec![vec![1.; 6]],
                GeoTransform { origin_x: 100., origin_y: 500., pixel_width: 2., pixel_height: -3. },
                Crs::from_epsg(3857).unwrap(),
                None,
                ColorHint::Gray,
                "UInt8",
            )
            .unwrap();
            let bytes = geotiff::write(&raster, 0).unwrap();
            let expected =
                rustgis_engine::box_edit::Frame::from_grid(&geotiff::read_image_grid(std::io::Cursor::new(&bytes)).unwrap()).unwrap();
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let image = app.session.map_mut().add_raster("Image", rustgis_engine::map::RasterLayer::from_bytes(raster, "image.tif", bytes));
            app.reads_off_frame = off_frame;
            start_script(&mut app, format!("raster.imageGrid {}", json!({"layer":image})));
            let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while crate::readjob::land(&mut app) {
                assert!(std::time::Instant::now() < end, "grid script never landed");
                std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: await the script worker's real frame delivery
            }
            assert!(app.workflow.script_started.is_empty());
            let run = app.workflow.script_last.as_ref().unwrap();
            assert!(run.error.is_none(), "{:?}", app.workflow.script_out);
            assert_eq!(run.ran.len(), 1);
            assert_eq!(serde_json::from_value::<rustgis_engine::box_edit::Frame>(run.ran[0].result.clone()).unwrap(), expected);
            assert!(app.workflow.script_out.iter().all(|(_, error)| !error));
        }
    }

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
                std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: legacy polling for script completion
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
        let ask =
            |app: &mut RustGisApp| app.workflow.script_started.push_back((0, web_time::Instant::now(), app.session.opened, String::new()));
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
        app.workflow
            .script_started
            .extend([(1_000, asked, app.session.opened, String::new()), (1_000, asked, app.session.opened, String::new())]);
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
        h.state_mut().workflow.script_started.push_back((0, web_time::Instant::now(), 0, String::new()));
        h.run();
        assert!(!on(&h));
    }

    /// Load… reads a script file in place of the pane's script, emptying its results as Clear
    /// does, and Ctrl+Z brings the old script back; a cancelled picker and a file that isn't UTF-8
    /// text leave it (the latter said), and it is off while a run is under way.
    #[test]
    fn load_reads_a_script_file_in_place_of_the_panes() {
        let dir = std::env::temp_dir().join(format!("rustgis-script-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (good, bad) = (dir.join("zoom.txt"), dir.join("latin1.txt"));
        std::fs::write(&good, "map.zoomFull\n").unwrap();
        std::fs::write(&bad, b"map.new {\"name\": \"Espa\xf1a\"}").unwrap();
        let mut app = RustGisApp::with_sample();
        app.workflow.script = "kept".into();
        script_ran(&mut app, crate::Landed::Ran(Ok(json!({"ran": [{"line": 1, "command": "map.zoomFull", "result": null}]}))));
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
        let load = |h: &mut egui_kittest::Harness<'_, RustGisApp>, path: Option<&std::path::Path>| {
            crate::dialogs::PICKED.with(|c| *c.borrow_mut() = path.map(|p| p.display().to_string()));
            h.get_by_label("Load…").click();
            h.run();
        };
        load(&mut h, None);
        assert_eq!(h.state().workflow.script, "kept", "a cancelled picker changes nothing");
        assert!(h.state().workflow.script_last.is_some());
        load(&mut h, Some(&bad));
        assert_eq!(h.state().workflow.script, "kept", "a file that isn't UTF-8 leaves the script");
        assert!(h.state().workflow.script_last.is_some());
        assert!(h.state().session.notices.iter().any(|n| n.text == "latin1.txt isn't UTF-8 text"), "{:?}", h.state().session.notices);
        let said = h.state().session.notices.len();
        load(&mut h, Some(&good));
        let w = &h.state().workflow;
        assert_eq!(w.script, "map.zoomFull\n");
        assert!(w.script_out.is_empty() && w.script_last.is_none(), "the results go, as with Clear");
        assert_eq!(h.state().session.notices.len(), said, "a clean file says nothing");
        assert!(h.get_by_role(egui::accesskit::Role::MultilineTextInput).is_focused(), "the box has the focus");
        // The box's own undo brings the old script back, with nothing clicked first.
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        h.run();
        assert_eq!(h.state().workflow.script, "kept", "Ctrl+Z brings it back");
        assert!(h.state().workflow.script_last.is_none(), "the results stay gone");
        // A run under way: off, as its results would land in the replaced script.
        h.state_mut().workflow.script_started.push_back((0, web_time::Instant::now(), 0, String::new()));
        h.run();
        assert!(egui_kittest::kittest::NodeT::accesskit_node(&h.get_by_label("Load…")).is_disabled());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// In the browser and on a phone, Load… asks the shell's `"script"` picker; its file opens in
    /// the pane on the next frame, a cancelled one (an empty answer) changes nothing, and one
    /// landing while a run is under way is refused and said.
    #[test]
    fn a_script_from_the_shells_picker_opens_in_the_pane() {
        use std::sync::{Arc, Mutex};
        let mut app = RustGisApp::with_sample();
        app.workflow.script = "kept".into();
        let asked = Arc::new(Mutex::new(Vec::<String>::new()));
        let ask = asked.clone();
        app.services.open_async = Some(Box::new(move |purpose| ask.lock().unwrap().push(purpose.to_string())));
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(900.0, 800.0)).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                app.logic(ui.ctx());
                script_pane(app, ui, &Tokens::light());
            },
            app,
        );
        h.run();
        h.get_by_label("Load…").click();
        h.run();
        assert_eq!(*asked.lock().unwrap(), ["script"]);
        h.state_mut().open_picked(crate::SCRIPT_PICK, &[]).unwrap();
        h.run();
        assert_eq!(h.state().workflow.script, "kept", "a cancelled picker changes nothing");
        assert!(h.state_mut().open_picked(&format!("{}bad.txt", crate::SCRIPT_PICK), b"\xff").is_err());
        let big = vec![b'#'; crate::SCRIPT_FILE_MAX_BYTES + 1];
        assert!(h.state_mut().open_picked(&format!("{}big.txt", crate::SCRIPT_PICK), &big).is_err());
        assert!(h.state().session.notices.iter().any(|n| n.text == "big.txt is larger than 1 MB"));
        h.run();
        assert_eq!(h.state().workflow.script, "kept");
        // A byte-order mark (Notepad's UTF-8) isn't kept: the first line's command stays known.
        let said = h.state().session.notices.len();
        h.state_mut().open_picked(&format!("{}zoom.txt", crate::SCRIPT_PICK), b"\xEF\xBB\xBFmap.zoomFull").unwrap();
        h.run();
        assert_eq!(h.state().workflow.script, "map.zoomFull");
        assert_eq!(h.state().session.notices.len(), said, "a clean file says nothing");
        assert_eq!(h.state().ui.right, Some(crate::RightPane::Script));
        h.state_mut().workflow.script_started.push_back((0, web_time::Instant::now(), 0, String::new()));
        assert!(h.state_mut().open_picked(&format!("{}other.txt", crate::SCRIPT_PICK), b"map.new").is_err());
        h.run();
        assert_eq!(h.state().workflow.script, "map.zoomFull", "not while a run is under way");
        assert!(h.state().session.notices.iter().any(|n| n.text == "Wait for the run to finish"), "{:?}", h.state().session.notices);
    }

    /// A run refused as it's asked, while another runs on a thread, lands at once as its own: each
    /// one's stop is read in the script it was asked with, and the one on the thread keeps its
    /// own when it lands (#2815).
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_run_refused_while_another_runs_keeps_each_ones_script() {
        let mut app = RustGisApp::with_sample();
        app.reads_off_frame = true;
        let at = |app: &RustGisApp| stopped_at(app).map(|s| (s.line, s.column));
        let first = "gp.run {\"tool\": \"buffer\", \"params\": {\"input\": \"Cities\", \"distance\": 100}}\nlayer.rename {\"layer\": \"Nope\", \"name\": \"x\"}";
        app.workflow.script = first.to_string();
        start_script(&mut app, first.to_string());
        assert!(script_running(&app));
        // Refused before it can wait behind the first: its error names its line 1.
        let refused = "map.zoomFull\nraster.imageGrid {}";
        app.workflow.script = refused.to_string();
        start_script(&mut app, refused.to_string());
        let e = app.workflow.script_last.as_ref().and_then(|r| r.error.clone()).unwrap_or_default();
        assert!(e.starts_with("line 1 (map.zoomFull)"), "{e}");
        assert_eq!(at(&app), Some((1, None)), "read in the refused script");
        // The first lands: its stop, line 2, is read in its own script.
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while crate::readjob::land(&mut app) {
            assert!(std::time::Instant::now() < end, "the script never landed");
            std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: polling for the run to land
        }
        assert!(!script_running(&app));
        assert_eq!(at(&app), None, "the box holds the refused script");
        app.workflow.script = first.to_string();
        assert_eq!(at(&app), Some((2, None)), "read in the first script");
    }

    /// A run that stops marks its line, and its column for JSON that doesn't read, while the script
    /// is the one it ran (#2815); a click on its error puts the cursor there.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_stopped_run_marks_its_line_and_its_error_goes_there() {
        let stopped = |app: &mut RustGisApp, script: &str| {
            app.workflow.script = script.to_string();
            let e = app.session.run("script.run", &json!({"script": script})).unwrap_err();
            script_ran(app, crate::Landed::Ran(Err(e.clone())));
            e
        };
        let at = |app: &RustGisApp| stopped_at(app).map(|s| (s.line, s.column));
        let mut app = RustGisApp::with_sample();
        // The last line, then the first.
        stopped(&mut app, "map.zoomFull\nno.such {}");
        assert_eq!(at(&app), Some((2, None)));
        let e = stopped(&mut app, "no.such {}\nmap.zoomFull");
        assert_eq!(at(&app), Some((1, None)), "{e}");
        // Edited: no mark, and back to the text it ran, the mark again.
        app.workflow.script.push('x');
        assert_eq!(at(&app), None);
        app.workflow.script.pop();
        assert_eq!(at(&app), Some((1, None)));
        // Run again, on a thread as the desktop runs it: the mark goes as the run starts, and comes
        // back where it stops again. A run that didn't stop leaves none.
        let script = "gp.run {\"tool\": \"buffer\", \"params\": {\"input\": \"Cities\", \"distance\": 100}}\nlayer.rename {\"layer\": \"Nope\", \"name\": \"x\"}";
        stopped(&mut app, script);
        assert_eq!(at(&app), Some((2, None)));
        app.reads_off_frame = true;
        start_script(&mut app, script.to_string());
        assert_eq!(at(&app), None, "while it runs");
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while crate::readjob::land(&mut app) {
            assert!(std::time::Instant::now() < end, "the script never landed");
            std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: polling for the run to land
        }
        assert_eq!(at(&app), Some((2, None)), "stopped again");
        app.reads_off_frame = false;
        script_ran(&mut app, crate::Landed::Ran(Ok(json!({"ran": []}))));
        assert_eq!(at(&app), None);
        // Edited while it ran: no mark on the text the box holds, and back to the text that ran
        // (undo), the mark where that text stopped.
        app.workflow.script_started.push_back((0, web_time::Instant::now(), app.session.opened, "no.such {}".into()));
        app.workflow.script = "map.zoomFull\nno.such {}".into();
        script_ran(&mut app, crate::Landed::Ran(Err("line 1: unknown command 'no.such'".into())));
        assert_eq!(at(&app), None);
        app.workflow.script = "no.such {}".into();
        assert_eq!(at(&app), Some((1, None)), "restored");
        // An error naming no line.
        app.workflow.script = "map.zoomFull".into();
        script_ran(&mut app, crate::Landed::NotRun("cancelled".into()));
        assert_eq!(at(&app), None);
        // JSON that doesn't read, after two lines; a click on the error goes to its column.
        let e = stopped(&mut app, "# move\nmap.zoomFull\nlayer.rename {\"layer\": }");
        assert_eq!(at(&app), Some((3, Some(24))), "{e}");
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(900.0, 800.0))
            .build_ui_state(|ui, app: &mut RustGisApp| script_pane(app, ui, &Tokens::light()), app);
        h.run();
        let box_id = egui::Id::unique("script_text");
        let cursor = |h: &egui_kittest::Harness<'_, RustGisApp>| {
            egui::widgets::text_edit::TextEditState::load(&h.ctx, box_id).and_then(|s| s.cursor.char_range()).map(|r| r.primary.index.0)
        };
        assert_ne!(cursor(&h), Some(7 + 13 + 23));
        h.get_by_label(&e).click();
        h.run();
        assert_eq!(cursor(&h), Some(7 + 13 + 23), "line 3, column 24");
        assert!(h.ctx.memory(|m| m.has_focus(box_id)));
        // From the keyboard: the error takes the focus (Tab), and Enter goes there too.
        let mut state = egui::widgets::text_edit::TextEditState::load(&h.ctx, box_id).unwrap_or_default();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(0))));
        state.store(&h.ctx, box_id);
        h.get_by_label(&e).focus();
        h.run();
        assert_eq!(cursor(&h), Some(0), "moved away first");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(cursor(&h), Some(7 + 13 + 23), "by the keyboard");
        // Edited since: the error no longer goes anywhere.
        h.state_mut().workflow.script.insert(0, '\n');
        h.run();
        let before = cursor(&h);
        h.get_by_label(&e).click();
        h.run();
        assert_eq!(cursor(&h), before);
    }

    /// The line and column of a cursor index (#2831): the script's lines, as a run's error names
    /// them, and columns from 1 in characters, as its column; where [`stopped_index`] puts it.
    #[test]
    fn line_col_counts_the_scripts_lines_and_characters() {
        let script = "map.zoomFull\n# café ☕\n\nlayer.rename {}";
        assert_eq!(line_col(script, 0), (1, 1), "the first character");
        assert_eq!(line_col(script, 12), (1, 13), "the end of the first line, before its newline");
        assert_eq!(line_col(script, 13), (2, 1), "just after a newline");
        // Characters, not bytes: `é` and `☕` take two and three bytes.
        assert_eq!(line_col(script, 13 + 8), (2, 9), "the end of a line of characters past ASCII");
        assert_eq!(line_col(script, 13 + 9), (3, 1), "an empty line");
        assert_eq!(line_col(script, 13 + 10 + 15), (4, 16), "the end of the script");
        assert_eq!(line_col(script, 1000), (4, 16), "an index past the end, left from another text");
        assert_eq!(line_col("", 0), (1, 1), "an empty script");
        // A script loaded from a file with Windows line ends keeps their `\r`: no column of its own.
        let crlf = "map.zoomFull\r\nno.such {}";
        assert_eq!(line_col(crlf, 12), (1, 13), "before the `\\r`");
        assert_eq!(line_col(crlf, 13), (1, 13), "after the `\\r`, still the end of the line");
        assert_eq!(line_col(crlf, 14), (2, 1), "after its newline");
        for (line, column) in [(1, None), (2, Some(3)), (4, Some(14))] {
            assert_eq!(line_col(script, stopped_index(script, line, column)), (line, column.unwrap_or(1)));
        }
    }

    /// The Script box says, on the right of a row of its own between it and its buttons (clear of
    /// them in a narrow pane), the line and column of its cursor (#2831): as it moves, as text is
    /// typed, of the caret's end of a selection, of a line that wraps, and still while the box
    /// isn't focused; nothing before it has a cursor.
    #[test]
    fn the_script_box_says_where_its_cursor_is() {
        use egui::text::{CCursor, CCursorRange};
        let mut app = RustGisApp::with_sample();
        // The second line wraps in the box: a long JSON line.
        let long = format!("layer.rename {{\"layer\": \"Cities\", \"name\": \"{}\"}}", "x".repeat(300));
        app.workflow.script = format!("map.zoomFull\n{long}");
        // As narrow as the pane opens beside the map: the row of buttons fills it.
        let mut h = egui_kittest::Harness::builder()
            .with_size(vec2(300.0, 800.0))
            .build_ui_state(|ui, app: &mut RustGisApp| script_pane(app, ui, &Tokens::light()), app);
        h.run();
        assert!(h.query_by_label_contains("Ln ").is_none(), "no cursor yet");
        let no_cursor = h.get_by_label("Clear").rect();
        let box_id = egui::Id::unique("script_text");
        let set = |h: &mut egui_kittest::Harness<'_, RustGisApp>, range: CCursorRange| {
            let mut state = egui::widgets::text_edit::TextEditState::load(&h.ctx, box_id).unwrap_or_default();
            state.cursor.set_char_range(Some(range));
            state.store(&h.ctx, box_id);
            h.run();
        };
        // On the wrapped line, past its first row: one line, its column from the line's start.
        set(&mut h, CCursorRange::one(CCursor::new(13 + 250)));
        assert!(h.query_by_label("Ln 2, Col 251").is_some(), "on a wrapped line");
        let label = h.get_by_label("Ln 2, Col 251").rect();
        let clear = h.get_by_label("Clear").rect();
        assert!(!label.intersects(clear) && label.bottom() <= clear.top(), "above the buttons, clear of them: {label:?} {clear:?}");
        assert_eq!(clear, no_cursor, "the buttons stay where they were before the box had a cursor");
        // Not focused, it still says where the cursor is.
        assert!(!h.ctx.memory(|m| m.has_focus(box_id)));
        set(&mut h, CCursorRange::one(CCursor::new(3)));
        assert!(h.query_by_label("Ln 1, Col 4").is_some(), "unfocused");
        // Focused: an arrow key, then a character typed, each in the frame it happens.
        h.ctx.memory_mut(|m| m.request_focus(box_id));
        h.run();
        h.key_press(egui::Key::ArrowRight);
        h.run();
        assert!(h.query_by_label("Ln 1, Col 5").is_some(), "moved");
        h.event(egui::Event::Text("x".into()));
        h.run();
        assert!(h.query_by_label("Ln 1, Col 6").is_some(), "typed");
        // A selection made backwards, from line 2 up to line 1: where its caret is, not its end.
        set(&mut h, CCursorRange { primary: CCursor::new(1), secondary: CCursor::new(14 + 4), h_pos: None });
        assert!(h.query_by_label("Ln 1, Col 2").is_some(), "a selection's caret");
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

    /// Ready Trend completions land through the real app frame without replacing another
    /// section's output; Trend still receives its own result/error and Cancel stays quiet.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn trend_completion_preserves_other_workflow_output() {
        for section in ["similar", "trend"] {
            for outcome in ["failure", "project changed", "success", "cancelled"] {
                let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
                let id = app.session.download_started();
                if outcome == "project changed" {
                    app.session.run("project.new", &Value::Null).unwrap();
                }
                app.run("ui.pane.workflows", json!({"section":section})).unwrap();
                let kept = json!({"similar": [{"row":1,"distance":0.5}]});
                app.workflow.result = Some(kept.clone());
                let result = match outcome {
                    "project changed" | "success" => Ok((Vec::new(), 0)),
                    _ => Err("trend test failure".into()),
                };
                app.workflow.trend.running = Some(Trending {
                    total: 1,
                    id,
                    params: Value::Null,
                    cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(outcome == "cancelled")),
                    done: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1)),
                    result: std::sync::Arc::new(std::sync::Mutex::new(Some(result))),
                });
                let mark = app.session.notice_mark();
                let mut fonts = false;
                let mut h = egui_kittest::Harness::builder().with_size(vec2(1400.0, 900.0)).build_ui_state(
                    move |ui, app: &mut RustGisApp| {
                        if !fonts {
                            crate::theme::install_fonts(ui.ctx(), &[], None);
                            fonts = true;
                            return;
                        }
                        app.logic(ui.ctx());
                        app.ui(ui);
                    },
                    app,
                );
                for _ in 0..4 {
                    h.step();
                }
                assert!(h.state().workflow.trend.running.is_none(), "the real frame consumed the worker completion");
                let notices = h.state().session.notices_since(mark);
                if outcome == "cancelled" || (section == "similar" && outcome == "success") {
                    assert!(h.state().workflow.error.is_none());
                    assert_eq!(h.state().workflow.result, Some(kept));
                    assert!(notices.is_empty());
                } else if outcome == "success" {
                    assert!(h.state().workflow.error.is_none());
                    assert_eq!(h.state().workflow.result, Some(json!({"layers":[],"dates":0})));
                    assert!(notices.is_empty());
                } else {
                    let needle = if outcome == "project changed" { "after another project" } else { "trend test failure" };
                    if section == "similar" {
                        assert!(h.state().workflow.error.is_none(), "Trend's error belongs outside the Similar pane");
                        assert_eq!(h.state().workflow.result, Some(kept));
                        assert_eq!(notices.len(), 1);
                        assert!(notices[0].error && notices[0].text.contains(needle));
                    } else {
                        assert!(h.state().workflow.error.as_deref().is_some_and(|e| e.contains(needle)));
                        assert!(h.state().workflow.result.is_none());
                        assert!(notices.is_empty());
                    }
                }
            }
        }
    }

    /// A command that makes a raster, asked for through the app's dispatch (Command Search) rather
    /// than a section, runs on a thread of its own too: Polygons to Raster from a
    /// text field answers that it started, the pane shows it running, and its layer and class table
    /// land with it.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_raster_command_from_the_dispatch_runs_on_a_thread() {
        let mut app = RustGisApp::with_sample();
        // As the app runs, a frame at a time.
        app.reads_off_frame = true;
        let (layers, tables) = (app.session.map().layers.len(), app.session.project.tables.len());
        // One that can't start doesn't open the pane.
        assert!(app.run("raster.rasterize", json!({"layer": "Countries", "field": "name"})).unwrap_err().contains("cellSize is required"));
        assert!(!app.ui.right_open.contains(&crate::RightPane::Workflows));
        let p = json!({"layer": "Countries", "field": "name", "cellSize": 5});
        let started = app.run("raster.rasterize", p.clone()).unwrap();
        assert_eq!(started, json!({"started": "Polygons to Raster"}));
        assert!(app.workflow.job.is_some(), "{:?}", app.workflow.error);
        assert!(app.ui.right_open.contains(&crate::RightPane::Workflows));
        // Its result goes to the section the pane shows (the first: it had never opened).
        assert_eq!(app.workflow.section, "rasterFunctions");
        assert_eq!(app.workflow.job.as_ref().map(|j| j.section), Some("rasterFunctions"));
        // A second, while it runs, is refused and says why.
        let again = app.run("raster.rasterize", p).unwrap_err();
        assert!(again.contains("is still running"), "{again}");
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while land_job(&mut app) {
            assert!(std::time::Instant::now() < end, "the run never landed");
            std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: polling for the workflow to land, as above
        }
        assert!(app.workflow.job.is_none() && app.workflow.error.is_none(), "{:?}", app.workflow.error);
        assert!(app.workflow.result.as_ref().is_some_and(|r| r["classTable"].is_string()), "{:?}", app.workflow.result);
        assert_eq!(app.session.map().layers.len(), layers + 1);
        assert_eq!(app.session.project.tables.len(), tables + 1);
    }

    /// A Cancel that comes once the work is done, before the job lands, still holds: nothing is
    /// added, and the GeoTIFF it wrote beside its destination is removed, the destination left as
    /// it was.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_cancel_after_the_work_is_done_drops_what_it_made() {
        let dir = std::env::temp_dir().join(format!("rustgis-late-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("burned.tif");
        std::fs::write(&path, b"before").unwrap();
        let mut app = RustGisApp::with_sample();
        app.reads_off_frame = true;
        let layers = app.session.map().layers.len();
        let p = json!({"layer": "Countries", "field": "name", "cellSize": 5, "path": path.to_string_lossy()});
        app.run("raster.rasterize", p).unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !matches!(&app.workflow.job, Some(Job { kind: JobKind::Raster { slot, .. }, .. }) if filled(slot)) {
            assert!(std::time::Instant::now() < end, "the run never finished");
            std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: polling for the worker to fill its slot
        }
        // It made its raster (and wrote its file beside the destination): looked at, not taken.
        let Some(Job { kind: JobKind::Raster { slot, .. }, control, .. }) = &app.workflow.job else { panic!("no raster job") };
        assert!(matches!(&*slot.lock().unwrap(), Some(Ok(_))), "the run failed");
        control.cancel();
        assert!(!land_job(&mut app));
        assert!(app.workflow.job.is_none());
        assert!(app.workflow.error.as_deref().is_some_and(|e| e.contains("was cancelled")), "{:?}", app.workflow.error);
        assert_eq!(app.session.map().layers.len(), layers);
        assert_eq!(std::fs::read(&path).unwrap(), b"before");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "a staged file was left");
        std::fs::remove_dir_all(dir).unwrap();
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
            std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: legacy polling for workflow completion
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
            std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: legacy polling for workflow completion
        }
    }

    /// The real Convert button asks for a destination first, starts the export job and lands
    /// its file; dismissing the picker never starts a conversion.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn interop_button_starts_an_export_after_picking_a_file() {
        let mut app = RustGisApp::with_sample();
        app.ui.right = Some(crate::RightPane::Workflows);
        app.workflow.section = "interop";
        app.workflow.layer = "Cities".into();
        app.workflow.format = "wkt".into();
        let dir = std::env::temp_dir().join(format!("rustgis-interop-button-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cities.csv");
        let mut fonts = false;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1480.0, 900.0)).with_pixels_per_point(1.3).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                app.ui(ui);
            },
            app,
        );
        h.run();
        let logged = h.state().session.log.entries().len();
        crate::dialogs::PICKED.with(|c| *c.borrow_mut() = None);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Convert…").click();
        h.run();
        assert!(h.state().export_job.is_none());
        assert_eq!(h.state().session.log.entries().len(), logged, "dismissing the picker does no conversion");
        assert!(h.state().workflow.error.is_none());
        crate::dialogs::PICKED.with(|c| *c.borrow_mut() = Some(path.display().to_string()));
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Convert…").click();
        h.step();
        assert!(h.state().export_job.is_some(), "{:?}", h.state().workflow.error);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while h.state().export_job.is_some() {
            assert!(std::time::Instant::now() < end, "conversion never landed");
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: legacy polling for the export result
        }
        h.run();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("wkt,"));
        assert_eq!(text.lines().count() - 1, h.state().session.map().layer_by_name("Cities").unwrap().dataset.features.len());
        assert_eq!(h.state().session.log.entries().last().unwrap().command, "data.convert");
        std::fs::remove_dir_all(dir).unwrap();
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
