//! Add Data's whole-file reads, layer repair/reload, Load All Features, Convert To Annotation's read of a
//! layer read in place, local archive validation and model/script tools on the desktop run on a
//! thread of their own (#1291 step 3), so large reads
//! and tools don't hold the window: the status bar shows the run under way,
//! how far it has come and Cancel, and [`land`] then runs the command, which takes what was made
//! ([`reads`]) and adds it as it does when it runs here. They go one at a time, in the order
//! asked for: files dropped together wait for the one before, and so does a file added while one
//! is read, so the layers come in that order.

use std::collections::VecDeque;
use std::sync::Arc;

use egui::Ui;
use rustgis_engine::analysis::control::Control;
use rustgis_engine::cmd::reads;
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::Tokens;

/// The reads under way and waiting.
#[derive(Default)]
pub struct Reads {
    job: Option<ReadJob>,
    /// The commands waiting for the read before, in order.
    waiting: VecDeque<Asked>,
    /// Scoped cancellations deliver their callbacks on the next landing pass, after the UI
    /// has replaced the old request, rather than dropping waiting completions silently.
    cancelled: VecDeque<Asked>,
    #[cfg(test)]
    fail_spawn: bool,
}

impl Reads {
    /// Closing a conversion preview cancels only its read, leaving unrelated queued work alone.
    pub(crate) fn cancel_shape_conversion(&mut self, request: u64) {
        let belongs = |a: &Asked| {
            a.command == "edit.previewShapeConversion"
                && a.relay.is_none()
                && a.params.get("request").and_then(Value::as_u64) == Some(request)
        };
        self.cancel_where(belongs);
    }
    pub(crate) fn cancel_annotation_grid(&mut self, request: u64) {
        self.cancel_where(|a| {
            a.command == "raster.imageGrid" && a.relay.is_none() && matches!(a.then, Some(Completion::ImageGrid(id)) if id == request)
        });
    }
    fn cancel_where(&mut self, belongs: impl Fn(&Asked) -> bool) {
        if let Some(job) = &mut self.job
            && belongs(&job.asked)
        {
            // A request-specific cancel must not downgrade an earlier status-bar Cancel.
            if !job.control.is_cancelled() {
                job.cancel_waiting = false;
            }
            job.control.cancel();
        }
        let waiting = std::mem::take(&mut self.waiting);
        for asked in waiting {
            if belongs(&asked) {
                self.cancelled.push_back(asked);
            } else {
                self.waiting.push_back(asked);
            }
        }
    }
}

/// A command asked for, with where it was asked: the project, map and layer then open.
/// A reload also retains its source and confirmed editable state until it lands.
struct Asked {
    command: &'static str,
    params: Value,
    /// Admitted when asked, including while another read runs; never recreated at start or landing.
    ahead: Option<reads::ReadAhead>,
    /// What it reads, as its notices name it: the file, or the layer.
    what: String,
    opened: u64,
    map: u64,
    layer: Option<u64>,
    /// The exact archive and its presentation when asked, not a reusable list index.
    archive: Option<(usize, rustgis_engine::map::TileLayer)>,
    reload: Option<crate::reloaddata::Form>,
    /// A service add keeps the connection's display name until its actual landing.
    origin: Option<String>,
    /// Given what the command answered when it runs, or why it didn't (cancelled, or left).
    then: Option<Completion>,
    /// A direct relay archive call owns its real completion, rather than a Native callback.
    relay: Option<rustgis_assistant::relay::Relay>,
}

/// What a caller does once the command lands (ModelBuilder shows its run).
pub type Then = fn(&mut RustGisApp, Landed);

#[derive(Clone, Copy)]
enum Completion {
    Command(Then),
    ShapeConversion(Option<u64>),
    WorkflowSimilar(u64),
    ImageGrid(u64),
}
impl Completion {
    fn deliver(self, app: &mut RustGisApp, result: Landed) {
        match self {
            Self::Command(then) => then(app, result),
            Self::ShapeConversion(request) => crate::shapeconvert::landed(app, request, result),
            Self::WorkflowSimilar(request) => crate::analysispane::similar_ran(app, request, result),
            Self::ImageGrid(request) => crate::annotation::landed(app, request, result),
        }
    }
}

pub use crate::Landed;

/// A read under way: what it is for, its Cancel and progress, and what it read once done.
struct ReadJob {
    asked: Asked,
    /// What it was for went while it read: cancelled, it keeps its place until its thread ends
    /// (a reader that doesn't stop for Cancel still runs), so reads still go one at a time.
    gone: Option<Gone>,
    name: &'static str,
    control: Arc<Control>,
    /// Status-bar cancellation stops the queue; closing one conversion leaves other reads alone.
    cancel_waiting: bool,
    slot: crate::Slot<Result<reads::Read, String>>,
    /// A reload's target and what its file held, looked at on its thread as the read started.
    seen: Option<(crate::diskwatch::Target, crate::Slot<crate::diskwatch::Seen>)>,
}

/// Runs Add Data, Load All, Repair/Reload Data, Convert To Annotation, archive reload/repair, model/script
/// tools or the topology check with `p`: its read on a thread of its own when the desktop app makes it
/// so ([`reads::read_ahead`]), after any under way; here and now otherwise. While one is under way or
/// waiting it answers that, not what the command installs, which comes as it lands.
pub fn run(app: &mut RustGisApp, command: &'static str, p: Value) -> Result<Value, String> {
    run_then(app, command, p, None)
}

/// [`run`], then `then` with what the command answered once it ran, here or as it lands, or why
/// it never ran.
pub fn run_then(app: &mut RustGisApp, command: &'static str, p: Value, then: Option<Then>) -> Result<Value, String> {
    run_presented(app, command, p, then.map(Completion::Command), None, None)
}

/// Carries the originating request through success, failure and abandonment callbacks alike.
pub(crate) fn run_shape_conversion(app: &mut RustGisApp, p: Value) -> Result<Value, String> {
    let request = p.get("request").and_then(Value::as_u64);
    run_presented(app, "edit.previewShapeConversion", p, Some(Completion::ShapeConversion(request)), None, None)
}

/// Gives the final ranking to the pane request that asked for it, rather than its queue receipt.
pub(crate) fn run_workflow_similar(app: &mut RustGisApp, p: Value, request: u64) -> Result<Value, String> {
    run_presented(app, "data.similar", p, Some(Completion::WorkflowSimilar(request)), None, None)
}

/// Original grid lookup shares the read queue and carries its request through every outcome.
pub(crate) fn run_image_grid(app: &mut RustGisApp, p: Value, request: u64) -> Result<Value, String> {
    run_presented(app, "raster.imageGrid", p, Some(Completion::ImageGrid(request)), None, None)
}

/// Keeps a service layer's title and connection display name outside recorded parameters
/// until its admitted discovery lands. The ordinary read queue owns progress and Cancel.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn run_service(app: &mut RustGisApp, p: Value, name: &str, origin: Option<&str>) -> Result<Value, String> {
    run_presented(app, "service.add", p, None, Some((name.to_string(), origin.map(str::to_string))), None)
}

/// Converts only the rows whose date interpretation the user previewed.
pub(crate) fn run_date_preview(app: &mut RustGisApp, p: Value, dataset: rustgis_engine::data::Dataset) -> Result<Value, String> {
    run_presented(app, "edit.convertField", p, None, None, Some(dataset))
}

/// Queues the admitted read with optional presentation supplied by the service dialog/tree.
fn run_presented(
    app: &mut RustGisApp,
    command: &'static str,
    mut p: Value,
    then: Option<Completion>,
    presentation: Option<(String, Option<String>)>,
    preview: Option<rustgis_engine::data::Dataset>,
) -> Result<Value, String> {
    if !app.reads_off_frame {
        // Read here and now: what its file holds now is what it reads.
        if matches!(command, "layer.reloadData" | "map.reloadTiles") {
            crate::diskwatch::reading(app, command, &p);
        }
        let r = app.run(command, p);
        return answered(app, then, r);
    }
    // A field conversion given no layer or table works on the active layer: bound to it now, so
    // it lands on that layer and its recorded step names it, whichever layer is active by then.
    if command == "edit.convertField"
        && ["layer", "table"].iter().all(|k| p.get(*k).is_none_or(Value::is_null))
        && let Ok(id) = app.session.layer_id(&p)
    {
        if p.is_null() {
            p = json!({});
        }
        if let Some(params) = p.as_object_mut() {
            params.insert("layer".into(), json!(id));
        }
    }
    let layer = matches!(
        command,
        "layer.loadAll"
            | "layer.reloadData"
            | "layer.repairSource"
            | "labels.convert"
            | "source.syncState"
            | "table.statistics"
            | "data.similar"
            | "raster.imageGrid"
            | "edit.previewShapeConversion"
            | "layer.sizeBy"
            | "style.importQgis"
    )
    .then(|| app.session.layer_id(&p).ok().and_then(|id| app.session.map().layer(id)).map(|l| (l.instance_id(), l.name.clone())))
    .flatten();
    let notice_mark = app.session.notice_mark();
    if layer.is_some() {
        let id = app.session.layer_id(&p)?;
        if p.is_null() {
            p = json!({});
        }
        let Some(params) = p.as_object_mut() else {
            let error = tl!("Load All parameters must be an object").to_string();
            app.command_failed(command, &error, notice_mark);
            not_run(app, then, error.clone());
            return Err(error);
        };
        params.insert("layer".into(), json!(id));
    }
    let mut ahead = match reads::read_ahead(&mut app.session, command, &p) {
        Ok(ahead) => ahead,
        Err(error) => {
            app.command_failed(command, &error, notice_mark);
            not_run(app, then, error.clone());
            return Err(error);
        }
    };
    if let Some(dataset) = preview {
        ahead.keep_preview(dataset);
    }
    // Load All asked again for a layer it is reading already.
    if command == "layer.loadAll"
        && let Some((instance, _)) = &layer
        // (One whose map or project went never loads it: a new request waits behind it.)
        && app.reads.job.iter().filter(|j| j.gone.is_none()).map(|j| &j.asked).chain(&app.reads.waiting)
            .any(|a| a.command == command && a.layer == Some(*instance))
    {
        return Ok(json!({"reading": instance}));
    }
    let archive = matches!(command, "map.reloadTiles" | "map.repairTiles").then(|| reads::archive_target(&app.session, &p)).flatten();
    if let Some((i, _)) = &archive
        && let Some(params) = p.as_object_mut()
    {
        params.insert("index".into(), json!(i));
    }
    let what = match &layer {
        Some((_, name)) => name.clone(),
        None if archive.is_some() => archive.as_ref().map(|(_, tile)| tile.name.clone()).unwrap_or_default(),
        // A model by its name (one drawn but not yet named has none: see `called`).
        None if command == "model.run" => p.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string(),
        None if command == "service.add" => {
            p.get("name").or_else(|| p.get("layer")).and_then(Value::as_str).unwrap_or_default().to_string()
        }
        None => p.get("path").and_then(Value::as_str).map(|path| file_name(path).to_string()).unwrap_or_default(),
    };
    let (what, origin) = presentation.unwrap_or((what, None));
    let reload = if command == "layer.reloadData" {
        match app.session.layer_id(&p).and_then(|id| crate::reloaddata::Form::for_read(&app.session, id)) {
            Ok(form) => Some(form),
            Err(why) => {
                not_run(app, then, why.clone());
                return Err(why);
            }
        }
    } else {
        None
    };
    let asked = Asked {
        command,
        params: p,
        ahead: Some(ahead),
        what,
        opened: app.session.opened,
        map: app.session.map().instance_id(),
        layer: layer.map(|(instance, _)| instance),
        archive,
        reload,
        origin,
        then,
        relay: None,
    };
    if busy(app) {
        app.reads.waiting.push_back(asked);
        return Ok(json!({"waiting": app.reads.waiting.len()}));
    }
    match placed(app, &asked) {
        Ok(p) => start(app, asked, p),
        Err(why) => {
            let why = why.to_string();
            not_run(app, then, why.clone());
            Err(why)
        }
    }
}

/// Queues an archive or statistics read already admitted by the relay, without dispatching another command.
pub fn admitted(app: &mut RustGisApp, ahead: reads::ReadAhead, relay: rustgis_assistant::relay::Relay) {
    let Some((command, params)) = ahead.command() else {
        finish_relay(app, &relay, Err(tl!("This read was already started").to_string()));
        return;
    };
    let params = params.clone();
    let archive = matches!(command, "map.reloadTiles" | "map.repairTiles").then(|| reads::archive_target(&app.session, &params)).flatten();
    let layer = matches!(command, "table.statistics" | "data.similar" | "layer.sizeBy")
        .then(|| app.session.layer_id(&params).ok().and_then(|id| app.session.map().layer(id)).map(|l| (l.instance_id(), l.name.clone())))
        .flatten();
    let what =
        layer.as_ref().map(|(_, name)| name.clone()).or_else(|| archive.as_ref().map(|(_, tile)| tile.name.clone())).unwrap_or_default();
    let asked = Asked {
        command,
        params,
        ahead: Some(ahead),
        what,
        opened: app.session.opened,
        map: app.session.map().instance_id(),
        layer: layer.map(|(instance, _)| instance),
        archive,
        reload: None,
        origin: None,
        then: None,
        relay: Some(relay),
    };
    if busy(app) {
        app.reads.waiting.push_back(asked);
    } else {
        match placed(app, &asked) {
            Ok(p) => {
                let _ = start(app, asked, p);
            }
            Err(why) => not_run_asked(app, &asked, why.to_string()),
        }
    }
}

fn not_run_asked(app: &mut RustGisApp, asked: &Asked, why: String) {
    service_result(app, asked, Err(why.clone()));
    if matches!(asked.command, "layer.reloadData" | "map.reloadTiles") {
        crate::diskwatch::not_read(app, asked.command, &asked.params);
    }
    not_run(app, asked.then, why.clone());
    if let Some(relay) = &asked.relay {
        relay.begin_archive_landing(&app.session);
        finish_relay(app, relay, Err(why));
    }
}

/// Updates only this service add's pending dialog line; generic failures already have notices.
fn service_result(app: &mut RustGisApp, asked: &Asked, result: Result<(), String>) {
    let pending = crate::serviceui::adding(&asked.what);
    if asked.command == "service.add"
        && asked.opened == app.session.opened
        && asked.map == app.session.map().instance_id()
        && app.dialogs.open == Some("addService")
        && app.dialogs.info.as_deref() == Some(pending.as_str())
    {
        app.report_step(true, Some(&pending), result.map(|()| crate::serviceui::added(&asked.what)));
    }
}

/// Resumed relay suffixes have the same interaction guard as calls run by `Relay::poll`.
fn finish_relay(app: &mut RustGisApp, relay: &rustgis_assistant::relay::Relay, result: Result<Value, String>) {
    let state = app.session.state();
    relay.finish_archive_read(&mut app.session, result);
    if app.session.state() != state {
        app.map.cancel_label_drag();
    }
}

/// What the command answered, `r`, given to `then` first.
fn answered(app: &mut RustGisApp, then: Option<Completion>, r: Result<Value, String>) -> Result<Value, String> {
    if let Some(then) = then {
        then.deliver(app, Landed::Ran(r.clone()));
    }
    r
}

/// Why the command never ran, given to `then`.
fn not_run(app: &mut RustGisApp, then: Option<Completion>, why: String) {
    if let Some(then) = then {
        then.deliver(app, Landed::NotRun(why));
    }
}

/// `asked` left for `why`: its `then` told now, and kept in `left` for the notice.
fn leave(app: &mut RustGisApp, left: &mut Vec<(String, Gone)>, asked: &Asked, why: Gone) {
    let what = called(asked.command, &asked.what).to_string();
    not_run_asked(app, asked, crate::i18n::fmt("{what} was left: {why}", &[("what", &what), ("why", &why.to_string())]));
    left.push((what, why));
}

/// What a command asked for is called in a notice: a script, an unnamed model or anything else
/// that gives no name, by the command's name.
fn called<'a>(command: &str, what: &'a str) -> &'a str {
    match (what.is_empty(), command) {
        (false, _) => what,
        (true, "script.run") => tl_raw!("Run Script"),
        (true, "model.run") => tl_raw!("Run Model"),
        (true, "topology.validate") => tl_raw!("Error Inspector"),
        (true, "table.statistics") => tl_raw!("Column Statistics"),
        (true, "data.similar") => tl_raw!("Find Similar"),
        (true, "layer.sizeBy") => tl_raw!("Size by Field"),
        (true, "style.importQgis") => tl_raw!("Import QGIS Style"),
        (true, "service.add") => tl_raw!("Add Service Layer"),
        (true, "service.layers") => tl_raw!("List Service Layers"),
        (true, "edit.previewShapeConversion") => tl_raw!("Preview Shape Conversion"),
        (true, "raster.imageGrid") => tl_raw!("Original Image Grid"),
        (true, "source.syncState") => tl_raw!("Source File State"),
        (true, "map.reloadTiles") => tl_raw!("Reload Tile Archive"),
        (true, "map.repairTiles") => tl_raw!("Repair Tile Archive"),
        (true, _) => tl_raw!("A background read"),
    }
}

/// A path's last part, as a notice names the file.
fn file_name(path: &str) -> &str {
    std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path)
}

/// Whether a read is under way or waiting.
fn busy(app: &RustGisApp) -> bool {
    app.reads.job.is_some() || !app.reads.waiting.is_empty() || !app.reads.cancelled.is_empty()
}

/// Why a command asked for can't run now: what was open when it was asked isn't any more.
#[derive(Clone, Copy, PartialEq)]
enum Gone {
    Project,
    Map,
    Layer,
    Archive,
    Source,
    Data,
}

impl std::fmt::Display for Gone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            // Filled into a notice's template: in logical order (`tl_raw!`).
            Gone::Project => tl_raw!("another project was opened"),
            Gone::Map => tl_raw!("the map it was asked for isn't open"),
            Gone::Layer => tl_raw!("its layer was removed"),
            Gone::Archive => tl_raw!("its tile archive changed or was removed"),
            Gone::Source => tl_raw!("its data source changed"),
            Gone::Data => tl_raw!("its data or pending edits changed; confirm Reload Data again"),
        })
    }
}

/// `asked`'s parameters for the command now (Load All's layer by its id in the open map), or why
/// it can't run.
fn placed(app: &RustGisApp, asked: &Asked) -> Result<Value, Gone> {
    if app.session.opened != asked.opened {
        return Err(Gone::Project);
    }
    if app.session.map().instance_id() != asked.map {
        return Err(Gone::Map);
    }
    if let Some((i, original)) = &asked.archive
        && app.session.map().tiles.get(*i) != Some(original)
    {
        return Err(Gone::Archive);
    }
    let Some(instance) = asked.layer else { return Ok(asked.params.clone()) };
    let l = app.session.map().layers.iter().find(|l| l.instance_id() == instance).ok_or(Gone::Layer)?;
    if asked.reload.as_ref().is_some_and(|reload| !reload.is_current(&app.session)) {
        return Err(Gone::Source);
    }
    if asked.reload.as_ref().is_some_and(|reload| !reload.data_is_current(&app.session)) {
        return Err(Gone::Data);
    }
    let mut p = asked.params.clone();
    p["layer"] = json!(l.id);
    Ok(p)
}

/// Starts `asked`'s read with `p`, or runs it here when it reads nothing ahead (or its thread
/// can't start).
fn start(app: &mut RustGisApp, mut asked: Asked, p: Value) -> Result<Value, String> {
    let Some(ahead) = asked.ahead.take() else {
        return complete_here(app, asked, p, Err(tl!("This read was already started").to_string()));
    };
    let ahead = match ahead.refresh_table_input(&mut app.session) {
        Ok(ahead) => ahead,
        Err(why) => return complete_here(app, asked, p, Err(why)),
    };
    let name = ahead.name;
    let reload = matches!(asked.command, "layer.reloadData" | "map.reloadTiles");
    let control = Control::stoppable();
    if !ahead.reads() {
        // Read here and now: what its file holds now is what it reads.
        if reload {
            crate::diskwatch::reading(app, asked.command, &p);
        }
        let read = ahead.run(&control);
        return complete_here(app, asked, p, read);
    }
    // A reload's file is looked at on its thread, just before it reads it: a stalled drive holds
    // the read, not the window.
    let watched = if reload { crate::diskwatch::read_target(app, asked.command, &p) } else { None };
    let looked = crate::Slot::default();
    let look_out = looked.clone();
    let look_path = watched.as_ref().map(|(_, path)| path.clone());
    let (ctl, slot) = (control.clone(), crate::Slot::default());
    let out = slot.clone();
    // Retain the admitted operation if the OS cannot start its worker: the synchronous
    // fallback must use that same owner, rather than dispatching a new native User command.
    let held = crate::Slot::default();
    *held.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ahead);
    let work = held.clone();
    let diagnostics = app.session.diagnostics.capture();
    let spawn = || {
        std::thread::Builder::new().name(name.into()).spawn(move || {
            let _diagnostics = diagnostics.enter();
            if let Some(path) = look_path {
                *look_out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(crate::diskwatch::observe(&path));
            }
            let ahead = work.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
            let read =
                ahead.ok_or_else(|| tl!("This read was already started").to_string()).and_then(|ahead| ahead.run_waiting_for_grids(&ctl));
            *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(read);
        })
    };
    #[cfg(test)]
    let spawn = || if app.reads.fail_spawn { Err(std::io::Error::other("worker start refused for test")) } else { spawn() };
    let spawned = spawn();
    if let Err(e) = spawned {
        if matches!(
            asked.command,
            "map.reloadTiles"
                | "map.repairTiles"
                | "service.add"
                | "service.layers"
                | "script.run"
                | "edit.previewShapeConversion"
                | "raster.imageGrid"
                | "layer.sizeBy"
                | "data.similar"
                | "topology.validate"
        ) || rustgis_engine::cmd::edit::table_edit_in_background(asked.command).is_some()
        {
            let why = crate::i18n::fmt("The background worker could not start ({error}). Retry the command.", &[("error", &e.to_string())]);
            app.session.notify(why.clone(), true);
            not_run_asked(app, &asked, why.clone());
            return Err(why);
        }
        log::warn!("{name} reads here: its thread didn't start: {e}");
        if reload {
            crate::diskwatch::reading(app, asked.command, &p);
        }
        let read = held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| tl!("This read was already started").to_string())
            .and_then(|ahead| ahead.run(&control));
        return complete_here(app, asked, p, read);
    }
    let seen = watched.map(|(target, _)| (target, looked));
    app.reads.job = Some(ReadJob { asked, gone: None, name, control, cancel_waiting: true, slot, seen });
    Ok(json!({"started": name}))
}

/// Completes the admitted read, then calls its callback with the actual outcome.
/// A pre-dispatch failure is reported once even when a queued start discards its error.
fn complete_here(app: &mut RustGisApp, asked: Asked, p: Value, done: Result<reads::Read, String>) -> Result<Value, String> {
    let read = match done {
        Ok(read) => read,
        Err(error) => {
            let notice_mark = app.session.notice_mark();
            app.command_failed(asked.command, &error, notice_mark);
            not_run_asked(app, &asked, error.clone());
            return Err(error);
        }
    };
    if let Some(relay) = &asked.relay {
        relay.begin_archive_landing(&app.session);
    }
    let before = app.session.sources.generation;
    let r = app.run_read(asked.command, p, read);
    service_result(app, &asked, r.as_ref().map(|_| ()).map_err(Clone::clone));
    if r.is_ok() {
        crate::connectiontree::name_recent(app, before, asked.origin.as_deref());
    }
    if asked.command == "map.repairTiles"
        && let Ok(answer) = &r
        && let Some(index) = answer.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok())
        && let Some(tile) = app.session.map().tiles.get(index)
    {
        let text = crate::i18n::fmt("Repaired {name}", &[("name", &tile.name)]);
        app.session.notify(text, false);
    }
    if let Some(relay) = &asked.relay {
        finish_relay(app, relay, r.clone());
    }
    answered(app, asked.then, r)
}

/// Runs the command a read was for once it's done (or says why it wasn't), then starts the next
/// waiting: called every frame. `true` while one is under way or waiting.
pub fn land(app: &mut RustGisApp) -> bool {
    // History an ended edit set aside comes back now, not at the next command: Undo must see it.
    app.session.reclaim_rooms();
    // Each one's caller hears why; the notices say it once a kind, with a count.
    let mut cancelled: Vec<(String, usize)> = Vec::new();
    for asked in std::mem::take(&mut app.reads.cancelled) {
        let what = called(asked.command, &asked.what).to_string();
        let why = crate::i18n::fmt("{what} was cancelled before starting", &[("what", &what)]);
        match cancelled.iter_mut().find(|(w, _)| *w == what) {
            Some((_, n)) => *n += 1,
            None => cancelled.push((what, 1)),
        }
        not_run_asked(app, &asked, why);
    }
    for (what, n) in cancelled {
        let why = if n == 1 {
            crate::i18n::fmt("{what} was cancelled before starting", &[("what", &what)])
        } else {
            crate::i18n::fmt("{what} was cancelled before starting ({n} requests)", &[("what", &what), ("n", &n.to_string())])
        };
        app.session.notify(why, false);
    }
    if !busy(app) {
        return false;
    }
    // Those left, for the notices below; each one's `then` is told as it is left, so callers
    // hear in the order asked, before a later one starts.
    let mut left: Vec<(String, Gone)> = Vec::new();
    // What it was for is gone: it is cancelled, and said with the rest below once it stops.
    let gone = app.reads.job.as_ref().filter(|j| j.gone.is_none()).and_then(|j| placed(app, &j.asked).err());
    if let (Some(why), Some(job)) = (gone, app.reads.job.as_mut()) {
        job.control.cancel();
        job.gone = Some(why);
    }
    if let Some(job) = &app.reads.job {
        let done = job.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let Some(done) = done else { return true };
        let Some(job) = app.reads.job.take() else { return false };
        match (job.gone, placed(app, &job.asked)) {
            (Some(why), _) | (None, Err(why)) => leave(app, &mut left, &job.asked, why),
            (None, Ok(p)) => {
                if job.control.is_cancelled() {
                    // Cancelled even after reading: add nothing. Only queue-wide cancellation also drops waiting reads.
                    let said = if job.asked.what.is_empty() {
                        crate::i18n::fmt("{tool} was cancelled", &[("tool", tl_raw!(job.name))])
                    } else {
                        crate::i18n::fmt("{tool} of {what} was cancelled", &[("tool", tl_raw!(job.name)), ("what", &job.asked.what)])
                    };
                    app.session.notify(said.clone(), false);
                    not_run_asked(app, &job.asked, said);
                    if job.cancel_waiting {
                        let waiting = std::mem::take(&mut app.reads.waiting);
                        let n = waiting.len();
                        let left_too = match n {
                            1 => tl!("The one waiting after it was left too").to_string(),
                            _ => crate::i18n::fmt("The {n} waiting after it were left too", &[("n", &n.to_string())]),
                        };
                        if n > 0 {
                            app.session.notify(left_too.clone(), false);
                        }
                        for asked in waiting {
                            not_run_asked(app, &asked, left_too.clone());
                        }
                    }
                } else {
                    // What its file held as its read started is what this reload acknowledges.
                    if let Some((target, looked)) = job.seen {
                        let seen = looked.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
                        if let Some(seen) = seen {
                            crate::diskwatch::read_started(app, target, seen);
                        }
                    }
                    // The engine consumes the original owner before the callback sees its result.
                    let _ = complete_here(app, job.asked, p, done);
                    app.map.invalidate();
                }
            }
        }
    }
    // The next waiting, until one reads on its thread.
    while app.reads.job.is_none() {
        let Some(asked) = app.reads.waiting.pop_front() else { break };
        match placed(app, &asked) {
            // A failure is said by the command.
            Ok(p) => {
                let _ = start(app, asked, p);
                app.map.invalidate();
            }
            Err(why) => leave(app, &mut left, &asked, why),
        }
    }
    // Those left, one notice for each reason, with a count, naming the first.
    for why in [Gone::Project, Gone::Map, Gone::Layer, Gone::Archive, Gone::Source, Gone::Data] {
        let of: Vec<&String> = left.iter().filter(|(_, w)| *w == why).map(|(what, _)| what).collect();
        let reason = why.to_string();
        match of.as_slice() {
            [] => {}
            [what] => app.session.notify(crate::i18n::fmt("{what} was left: {why}", &[("what", what), ("why", &reason)]), false),
            [first, ..] => app.session.notify(
                crate::i18n::fmt(
                    "{n} reads were left, the first {what}: {why}",
                    &[("n", &of.len().to_string()), ("what", first), ("why", &reason)],
                ),
                false,
            ),
        }
    }
    busy(app)
}

/// Cancels the read under way, and what waits, and waits up to `wait` for its thread to stop.
/// Each one that never ran is told so, the one under way first and then those waiting in the
/// order asked: its `then`, and a relay's agent, whose batch is ended unrun rather than resumed.
pub fn stop(app: &mut RustGisApp, wait: std::time::Duration) {
    let why = tl!("RustGIS is closing").to_string();
    let job = app.reads.job.take();
    let waiting: Vec<Asked> = app.reads.cancelled.drain(..).chain(app.reads.waiting.drain(..)).collect();
    if let Some(job) = job {
        job.control.cancel();
        let end = std::time::Instant::now() + wait;
        while job.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() && std::time::Instant::now() < end {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        abandon_asked(app, &job.asked, why.clone());
    }
    for asked in &waiting {
        abandon_asked(app, asked, why.clone());
    }
}

/// `asked` will never run: its `then` is told, and a relay's agent is answered with `why` for the
/// command and everything after it in its message, none of which runs.
fn abandon_asked(app: &mut RustGisApp, asked: &Asked, why: String) {
    not_run(app, asked.then, why.clone());
    if let Some(relay) = &asked.relay {
        relay.begin_archive_landing(&app.session);
        relay.abandon_archive_read(&mut app.session, why);
    }
}

#[cfg(test)]
impl Reads {
    /// Presses Cancel on the run under way.
    pub fn cancel_for_tests(&self) {
        if let Some(job) = &self.job {
            job.control.cancel();
        }
    }

    /// Whether the run under way has finished reading on its thread (its answer waits to land).
    pub fn read_finished_for_tests(&self) -> bool {
        self.job.as_ref().is_some_and(|job| job.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some())
    }
}

/// Whether the status bar shows a read.
pub fn showing(app: &RustGisApp) -> bool {
    app.reads.job.is_some()
}

/// The read under way in the status bar: its name, progress, how many wait, and Cancel.
pub fn status(app: &RustGisApp, ui: &mut Ui, t: &Tokens) {
    let Some(job) = &app.reads.job else { return };
    ui.spinner();
    // Two labels, not one sentence put together from translated pieces.
    ui.label(egui::RichText::new(tl!("Running…")).size(11.5).color(t.text_dim));
    ui.label(egui::RichText::new(tl!(job.name)).size(11.5).strong());
    // How far its current step has come, when its reader counts.
    if let Some((done, total)) = job.control.progress() {
        ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).show_percentage().desired_width(120.0).desired_height(12.0));
    }
    let n = app.reads.waiting.len();
    if n > 0 {
        ui.label(egui::RichText::new(crate::i18n::fmt("{n} waiting", &[("n", &n.to_string())])).size(11.5).color(t.text_dim));
    }
    if ui.add_enabled(!job.control.is_cancelled(), egui::Button::new(tl!("Cancel")).small()).clicked() {
        job.control.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Lands every read under way and waiting, failing past `secs`.
    fn landed(app: &mut RustGisApp, secs: u64) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while land(app) {
            assert!(std::time::Instant::now() < end, "the reads never landed");
            std::thread::sleep(std::time::Duration::from_millis(5)); // test-wait: legacy polling for background read completion
        }
    }

    #[test]
    fn a_read_without_a_name_is_called_by_its_own_command() {
        assert_eq!(called("model.run", "Flood"), "Flood");
        assert_eq!(called("model.run", ""), "Run Model");
        assert_eq!(called("service.layers", ""), "List Service Layers");
        for command in ["edit.previewShapeConversion", "raster.imageGrid", "source.syncState", "map.reloadTiles", "map.repairTiles"] {
            assert_ne!(called(command, ""), "Run Model", "{command}");
        }
        assert_eq!(called("layer.loadAll", ""), "A background read");
    }

    #[test]
    fn annotation_grid_cancel_preserves_unrelated_waiting_reads() {
        annotation_grid_cancel_case(false);
    }

    #[test]
    fn annotation_grid_cancel_preserves_an_earlier_queue_wide_cancel() {
        annotation_grid_cancel_case(true);
    }

    fn annotation_grid_cancel_case(queue_wide: bool) {
        use rustgis_engine::{
            Project, Session,
            data::raster::{ColorHint, Crs, GeoTransform, Raster, geotiff},
        };
        let raster = Raster::new(
            "image",
            1,
            1,
            vec![vec![1.]],
            GeoTransform { origin_x: 0., origin_y: 1., pixel_width: 1., pixel_height: -1. },
            Crs::from_epsg(3857).unwrap(),
            None,
            ColorHint::Gray,
            "UInt8",
        )
        .unwrap();
        let bytes = geotiff::write(&raster, 0).unwrap();
        let mut s = Session::new(Project::default());
        let id = s.map_mut().add_raster("Image", rustgis_engine::map::RasterLayer::from_bytes(raster, "image.tif", bytes));
        let mut app = RustGisApp::new(s);
        app.reads_off_frame = true;
        app.run("ui.annotationImage", json!({"image":id})).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("queued.csv");
        std::fs::write(&file, "name,lon,lat\nQueued,1,2\n").unwrap();
        run(&mut app, "layer.addData", json!({"path":file,"name":"Queued after grid"})).unwrap();
        if queue_wide {
            app.reads.cancel_for_tests();
        }
        app.run("ui.annotationCancel", Value::Null).unwrap();
        app.run("ui.annotationCancel", Value::Null).unwrap();
        let job = app.reads.job.as_ref().unwrap();
        assert!(job.control.is_cancelled());
        assert_eq!(job.cancel_waiting, queue_wide);
        assert_eq!(app.reads.waiting.len(), 1);
        landed(&mut app, 60);
        assert_eq!(app.session.map().layer_by_name("Queued after grid").is_some(), !queue_wide);
        assert!(crate::annotation::drawing_frame(&app).is_err());
    }

    #[test]
    fn annotation_grid_scoped_cancel_delivers_waiting_callbacks_once_and_keeps_other_reads() {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("points.csv");
        std::fs::write(&path, "name,lon,lat\nA,1,2\n").unwrap();
        app.reads_off_frame = true;
        fn cancelled(app: &mut RustGisApp, result: Landed) {
            let Landed::NotRun(why) = result else { panic!("cancelled waiting read must not run") };
            assert!(why.contains("cancelled before starting"));
            app.session.notify("Waiting callback delivered", false);
        }
        run(&mut app, "layer.addData", json!({"path":path,"name":"First"})).unwrap();
        run_then(&mut app, "layer.addData", json!({"path":path,"name":"Cancel me"}), Some(cancelled)).unwrap();
        run(&mut app, "layer.addData", json!({"path":path,"name":"Keep me"})).unwrap();
        app.reads.cancel_where(|a| a.params["name"] == "Cancel me");
        app.reads.cancel_where(|a| a.params["name"] == "Cancel me");
        assert_eq!(app.reads.waiting.len(), 1);
        landed(&mut app, 60);
        assert!(app.session.map().layer_by_name("First").is_some());
        assert!(app.session.map().layer_by_name("Keep me").is_some());
        assert!(app.session.map().layer_by_name("Cancel me").is_none());
        assert_eq!(app.session.notices.iter().filter(|n| n.text == "Waiting callback delivered").count(), 1);
        assert!(app.reads.cancelled.is_empty());
        assert!(!land(&mut app));
    }

    #[test]
    fn annotation_grid_worker_failure_never_reads_headers_on_the_frame() {
        use rustgis_engine::{
            Project, Session,
            data::raster::{ColorHint, Crs, GeoTransform, Raster},
        };
        let raster = Raster::new(
            "image",
            1,
            1,
            vec![vec![1.]],
            GeoTransform { origin_x: 0., origin_y: 1., pixel_width: 1., pixel_height: -1. },
            Crs::from_epsg(3857).unwrap(),
            None,
            ColorHint::Gray,
            "UInt8",
        )
        .unwrap();
        let mut s = Session::new(Project::default());
        let id = s.map_mut().add_raster("Image", rustgis_engine::map::RasterLayer::from_bytes(raster, "image.tif", vec![0; 32]));
        let mut app = RustGisApp::new(s);
        app.reads_off_frame = true;
        app.reads.fail_spawn = true;
        app.run("ui.annotationImage", json!({"image":id})).unwrap();
        assert!(!showing(&app));
        assert!(crate::annotation::drawing_frame(&app).unwrap_err().contains("worker could not start"));
        assert!(app.session.notices.iter().any(|n| n.error && n.text.contains("worker could not start")));
    }

    #[test]
    fn shape_conversion_worker_failure_does_not_fit_or_apply_on_the_frame() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::{
            Project, Session,
            data::{Dataset, GeometryType},
        };
        let mut session = Session::new(Project::default());
        let id = session.map_mut().add_dataset(Dataset::new("Polygons", GeometryType::Polygon, vec![]));
        session.active_layer = Some(id);
        session.run("edit.enable", &json!({"on":true})).unwrap();
        session
            .run(
                "edit.addFeature",
                &json!({"geometry":{"type":"Polygon","coordinates":[[[0.,0.],[0.01,0.],[0.01,0.01],[0.,0.01],[0.,0.]]]}}),
            )
            .unwrap();
        let before = session.map().layer(id).unwrap().dataset.features[0].clone();
        let depth = session.undo_depth();
        let mut app = RustGisApp::new(session);
        app.reads_off_frame = true;
        app.reads.fail_spawn = true;
        app.run("ui.convertShape", json!({"kind":"circle"})).unwrap();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400., 900.)).with_max_steps(1000).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.get_by_label("Convert Now").click();
        h.run();
        assert_eq!(h.state().session.map().layer(id).unwrap().dataset.features[0], before);
        assert_eq!(h.state().session.undo_depth(), depth);
        assert!(h.state().session.circle_spec(id, 0).is_none());
        assert!(!showing(h.state()));
        assert!(h.state().session.notices.iter().any(|n| n.error && n.text.contains("worker could not start")));
        assert!(h.query_by_label("Convert Now").is_some(), "the form remains available for retry");
    }

    #[test]
    fn shape_conversion_reopening_cancels_running_and_queued_fits() {
        use rustgis_engine::{
            Project, Session,
            data::{Dataset, GeometryType},
        };
        let mut session = Session::new(Project::default());
        let id = session.map_mut().add_dataset(Dataset::new("Polygons", GeometryType::Polygon, vec![]));
        session.active_layer = Some(id);
        session.run("edit.enable", &json!({"on":true})).unwrap();
        session
            .run(
                "edit.addFeature",
                &json!({"geometry":{"type":"Polygon","coordinates":[[[0.,0.],[0.01,0.],[0.01,0.01],[0.,0.01],[0.,0.]]]}}),
            )
            .unwrap();
        let mut app = RustGisApp::new(session);
        app.reads_off_frame = true;
        app.run("ui.convertShape", json!({"kind":"circle"})).unwrap();
        run_shape_conversion(&mut app, json!({"row":0,"kind":"circle","request":1})).unwrap();
        let control = app.reads.job.as_ref().unwrap().control.clone();
        app.run("ui.convertShape", json!({"kind":"rotatedBox"})).unwrap();
        assert!(control.is_cancelled());
        assert!(!app.reads.job.as_ref().unwrap().cancel_waiting);
        run_shape_conversion(&mut app, json!({"row":0,"kind":"rotatedBox","request":2})).unwrap();
        assert_eq!(app.reads.waiting.len(), 1);
        app.run("ui.convertShape", json!({"kind":"circle"})).unwrap();
        assert!(app.reads.waiting.is_empty(), "replacing a queued fit must remove only that fit");
        run_shape_conversion(&mut app, json!({"row":0,"kind":"circle","request":3})).unwrap();
        landed(&mut app, 60);
        assert!(!showing(&app));
        assert_eq!(app.session.map().layer(id).unwrap().dataset.features.len(), 1);
    }

    #[test]
    fn shape_conversion_cancel_keeps_other_reads_and_other_preview_requests() {
        use rustgis_engine::{
            Project, Session,
            box_edit::{BoxSpec, Frame},
            data::{Dataset, GeometryType},
        };
        let mut session = Session::new(Project::default());
        let id = session.map_mut().add_dataset(Dataset::new("Polygons", GeometryType::Polygon, vec![]));
        session.active_layer = Some(id);
        session.run("edit.enable", &json!({"on":true})).unwrap();
        let shape = BoxSpec { frame: Frame::default(), center: [1000., 1000.], width: 100., height: 50., angle: 20. };
        session
            .run("edit.addFeature", &json!({"geometry":rustgis_engine::data::io::geometry_to_geojson(&shape.geometry().unwrap())}))
            .unwrap();
        let mut app = RustGisApp::new(session);
        app.reads_off_frame = true;
        fn completed(app: &mut RustGisApp, result: Landed) {
            let Landed::Ran(Ok(value)) = result else { panic!("unrelated preview was cancelled") };
            assert_eq!(value["request"], 18);
            app.session.notify("Unrelated preview completed", false);
        }
        run(&mut app, "edit.previewShapeConversion", json!({"layer":id,"row":0,"kind":"circle","request":17})).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("queued.csv");
        std::fs::write(&file, "name,lon,lat\nQueued,1,2\n").unwrap();
        run(&mut app, "layer.addData", json!({"path":file,"name":"Queued layer"})).unwrap();
        run_then(&mut app, "edit.previewShapeConversion", json!({"layer":id,"row":0,"kind":"circle","request":18}), Some(completed))
            .unwrap();
        assert_eq!(app.reads.waiting.len(), 2);
        app.reads.cancel_shape_conversion(17);
        landed(&mut app, 60);
        assert!(app.session.map().layer_by_name("Queued layer").is_some());
        assert!(app.session.notices.iter().any(|n| n.text == "Unrelated preview completed"));
        assert!(app.session.circle_spec(id, 0).is_none());
    }

    fn relay_command(id: u64, command: &str, params: Value) -> Value {
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"run_command","arguments":{"command":command,"params":params}}})
    }

    fn paused_relay(
        app: &mut RustGisApp,
        batch: Value,
    ) -> (rustgis_assistant::relay::Relay, rustgis_assistant::relay::Grant, std::net::TcpStream, reads::ReadAhead) {
        use rustgis_assistant::relay::{Relay, read_token_file};
        use std::io::Write;
        use std::net::TcpStream;
        let relay = Relay::start(0).unwrap();
        let grant = relay.grant(true).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let token = read_token_file(grant.token_file()).unwrap();
        writeln!(client, "{}", json!({"jsonrpc":"2.0","method":"rustgis/attach","params":{"token":token,"access":true}})).unwrap();
        writeln!(client, "{batch}").unwrap();
        client.flush().unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            relay.poll(&mut app.session);
            if let Some(ahead) = relay.take_archive_read() {
                return (relay, grant, client, ahead);
            }
            assert!(std::time::Instant::now() < end, "relay never paused at its archive read");
            std::thread::sleep(Duration::from_millis(5)); // test-wait: legacy polling for the relay archive barrier
        }
    }

    fn label_drag_harness(path: &std::path::Path) -> (egui_kittest::Harness<'static, RustGisApp>, egui::Pos2) {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("map.addTiles", json!({"url":path,"name":"Linked","overlay":true})).unwrap();
        app.run("layer.template", json!({"template":"points","name":"Labelled"})).unwrap();
        app.run(
            "edit.addFeature",
            json!({"layer":"Labelled","geometry":{"type":"Point","coordinates":[1,1]},"attributes":{"name":"Relay drag"}}),
        )
        .unwrap();
        app.run("labels.field", json!({"layer":"Labelled","field":"name"})).unwrap();
        app.run("tool.set", json!({"tool":"moveLabel"})).unwrap();
        app.reads_off_frame = true;
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1400.0, 860.0))
            .with_step_dt(0.016)
            .with_max_steps(10_000)
            .build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                    app.ui(ui);
                },
                app,
            );
        h.step();
        h.state_mut().run("map.zoomToExtent", json!({"extent":[-1,-1,5,5]})).unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(10);
        let at = loop {
            h.step();
            if let Some(at) = h.state().map.label_position_for_tests("Relay drag")
                && !h.state().map.drawing()
            {
                break at;
            }
            assert!(std::time::Instant::now() < end, "the label was never drawn");
            std::thread::sleep(Duration::from_millis(2)); // test-wait: legacy polling for the draggable label
        };
        label_pointer(&mut h, at, None);
        label_pointer(&mut h, at, Some(true));
        for i in 1..=6 {
            label_pointer(&mut h, at + egui::vec2(0.0, -8.0 * i as f32), None);
        }
        assert!(h.state().map.label_dragging_for_tests(), "a real label drag must be active");
        (h, at + egui::vec2(0.0, -48.0))
    }

    fn label_pointer(h: &mut egui_kittest::Harness<'_, RustGisApp>, at: egui::Pos2, pressed: Option<bool>) {
        h.event(egui::Event::PointerMoved(at));
        if let Some(pressed) = pressed {
            h.event(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        h.step();
    }

    #[test]
    fn resumed_relay_edits_cancel_label_drags_on_success_failure_and_cancel_but_reads_preserve_them() {
        use std::io::{BufRead, BufReader};
        let _turn = crate::fetching::turn();
        let dir = std::env::temp_dir().join(format!("rustgis-relay-label-drag-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for outcome in ["success", "failure", "cancel", "not_run"] {
            for removes in [true, false] {
                let path = dir.join(format!("{outcome}-{removes}.pmtiles"));
                archive(&path, "Label drag");
                let (mut h, release) = label_drag_harness(&path);
                let suffix = if removes {
                    relay_command(2, "layer.remove", json!({"layer":"Labelled"}))
                } else {
                    relay_command(2, "map.tiles", json!({}))
                };
                let batch = json!([relay_command(1, "map.reloadTiles", json!({"index":0})), suffix]);
                let (relay, _grant, client, ahead) = paused_relay(h.state_mut(), batch);
                assert!(h.state().map.label_dragging_for_tests());
                if outcome == "failure" {
                    std::fs::write(&path, b"not a PMTiles archive").unwrap();
                }
                admitted(h.state_mut(), ahead, relay.clone());
                if outcome == "cancel" {
                    h.state().reads.cancel_for_tests();
                } else if outcome == "not_run" {
                    let job = h.state_mut().reads.job.take().unwrap();
                    job.control.cancel();
                    not_run_asked(h.state_mut(), &job.asked, "The archive read could not start".into());
                    let end = std::time::Instant::now() + Duration::from_secs(10);
                    while job.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() {
                        assert!(std::time::Instant::now() < end, "cancelled worker did not stop");
                        std::thread::sleep(Duration::from_millis(2)); // test-wait: legacy polling for cancelled worker completion
                    }
                }
                landed(h.state_mut(), 60);
                let mut line = String::new();
                BufReader::new(client).read_line(&mut line).unwrap();
                let reply: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(reply[0]["result"]["isError"], outcome != "success", "{outcome}: {reply}");
                assert_eq!(reply[1]["result"]["isError"], false, "{reply}");
                assert_eq!(h.state().map.label_dragging_for_tests(), !removes, "{outcome}: resumed edit guard");
                assert_eq!(h.state().session.map().layer_by_name("Labelled").is_none(), removes);
                let mark = h.state().session.notice_mark();
                let depth = h.state().session.undo_depth();
                label_pointer(&mut h, release, Some(false));
                assert!(!h.state().map.label_dragging_for_tests());
                if removes {
                    assert_eq!(h.state().session.undo_depth(), depth, "release must not dispatch labels.move");
                    assert!(h.state().session.notices_since(mark).is_empty(), "no stale labels.move error after removal");
                } else {
                    assert_eq!(h.state().session.map().layer_by_name("Labelled").unwrap().labels.default.moved.len(), 1);
                    assert_eq!(h.state().session.undo_depth(), depth + 1, "a read-only suffix preserves the pending move");
                }
                relay.stop();
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn archive_parameter_normalization_uses_objects_and_rejects_non_objects_without_panicking() {
        let _turn = crate::fetching::turn();
        let dir = std::env::temp_dir().join(format!("rustgis-archive-params-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("original.pmtiles");
        let replacement = dir.join("replacement.pmtiles");
        archive(&original, "Original");
        archive(&replacement, "Replacement");
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("map.addTiles", json!({"url":original,"name":"Linked","overlay":true})).unwrap();
        app.reads_off_frame = true;
        for command in ["map.reloadTiles", "map.repairTiles"] {
            for params in [Value::Null, json!(true), json!(7), json!([]), json!("not an object")] {
                assert!(run(&mut app, command, params).is_err());
                assert!(!busy(&app), "hostile parameters must not start a read");
            }
            let id = app.session.map().tiles[0].id.clone();
            for mut params in [json!({"index":0}), json!({"name":"Linked"}), json!({"id":id})] {
                if command == "map.repairTiles" {
                    params.as_object_mut().unwrap().insert("path".into(), json!(replacement));
                }
                run(&mut app, command, params).unwrap();
                let asked = &app.reads.job.as_ref().unwrap().asked;
                assert_eq!(asked.params.get("index"), Some(&json!(0)));
                assert_eq!(asked.archive.as_ref().unwrap().0, 0);
                landed(&mut app, 60);
            }
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ogc_dataset_add_reads_on_a_thread_and_keeps_its_origin() {
        let base = "https://maps.example.org";
        let docs = [
            ("/dataset", json!({"links":[{"rel":"tilesets-vector","href":"/tiles?f=json"},{"rel":"styles","href":"/styles?f=json"}]})),
            (
                "/tiles?f=json",
                json!({"tilesets":[{"tileMatrixSetId":"WebMercatorQuad","dataType":"vector","links":[{"rel":"item","type":"application/vnd.mapbox-vector-tile","href":"/wm/{tileMatrix}/{tileRow}/{tileCol}"}]}]}),
            ),
            (
                "/styles?f=json",
                json!({"styles":[{"id":"good","links":[{"rel":"stylesheet","type":"application/vnd.mapbox.style+json","href":"/good?f=mapbox"}]}]}),
            ),
            (
                "/good?f=mapbox",
                json!({"version":8,"sources":{"base":{"type":"vector","tiles":["/wm/{z}/{y}/{x}"]}},"layers":[{"id":"roads","type":"line","source":"base","source-layer":"roads"}]}),
            ),
        ];
        let p = json!({"url":format!("{base}/dataset"),"type":"ogcapi","layer":"good","text":docs[0].1.to_string(),"documents":docs.iter().skip(1).map(|(path,body)|json!({"url":format!("{base}{path}"),"text":body.to_string()})).collect::<Vec<_>>()});
        for cancel in [false, true] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            app.reads_off_frame = true;
            let result = crate::connectiontree::run_named(&mut app, "service.add", p.clone(), Some("Fixture connection")).unwrap();
            assert_eq!(result["started"], "Add Service Layer");
            app.dialogs.open = Some("addService");
            app.dialogs.info = Some(crate::serviceui::adding("good"));
            assert!(showing(&app));
            assert!(app.session.map().tiles.is_empty());
            if cancel {
                let end = std::time::Instant::now() + Duration::from_secs(5);
                while app.reads.job.as_ref().unwrap().slot.lock().unwrap().is_none() {
                    assert!(std::time::Instant::now() < end);
                    std::thread::sleep(Duration::from_millis(1)); // test-wait: legacy polling for the dataset result slot
                }
                app.reads.cancel_for_tests();
            }
            landed(&mut app, 5);
            assert_eq!(app.session.map().tiles.len(), usize::from(!cancel));
            if cancel {
                assert!(app.dialogs.info.is_none());
                assert!(app.dialogs.error.as_ref().is_some_and(|e| e.contains("cancelled")));
            } else {
                assert_eq!(app.dialogs.info, Some(crate::serviceui::added("good")));
                assert_eq!(app.session.map().tiles[0].url, format!("{base}/good?f=mapbox"));
                assert_eq!(app.session.sources.recent[0].name, "Fixture connection");
                app.run("edit.undo", Value::Null).unwrap();
                assert!(app.session.map().tiles.is_empty());
            }
        }
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.reads_off_frame = true;
        crate::connectiontree::run_named(&mut app, "service.add", p.clone(), None).unwrap();
        app.run("project.new", json!({})).unwrap();
        crate::connectiontree::run_named(&mut app, "service.add", p, None).unwrap();
        app.dialogs.open = Some("addService");
        app.dialogs.info = Some(crate::serviceui::adding("good"));
        landed(&mut app, 30);
        assert_eq!(app.session.map().tiles.len(), 1);
        assert_eq!(app.dialogs.info, Some(crate::serviceui::added("good")), "an old project must not replace the new add's progress line");
    }

    #[test]
    fn files_added_read_on_a_thread_and_land_in_order() {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.reads_off_frame = true;
        let dir = std::env::temp_dir().join(format!("rustgis-read-job-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |name: &str| {
            let path = dir.join(name);
            std::fs::write(&path, "name,x,y\nA,8,47\nB,9,46\n").unwrap();
            path.display().to_string()
        };
        let (first, second, third) = (file("first.csv"), file("second.csv"), file("third.csv"));
        assert_eq!(crate::layerchooser::add_file(&mut app, &first).unwrap()["started"], "Add Data");
        // One at a time: the next waits for it, and lands after it.
        assert_eq!(crate::layerchooser::add_file(&mut app, &second).unwrap()["waiting"], 1);
        assert_eq!(crate::layerchooser::add_file(&mut app, &third).unwrap()["waiting"], 2);
        assert!(app.session.map().layers.is_empty(), "nothing is added before its read lands");
        landed(&mut app, 60);
        let names: Vec<&str> = app.session.map().layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.iter().all(|n| ["first", "second", "third"].contains(n)), "{names:?}");
        // The layer list's order: the last added on top, as when each reads as it's added.
        let mut again = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        for path in [&first, &second, &third] {
            crate::layerchooser::add_file(&mut again, path).unwrap();
        }
        let order: Vec<&str> = again.session.map().layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, order);
        // Undo takes the last back, as for one added here.
        app.run("edit.undo", Value::Null).unwrap();
        assert_eq!(app.session.map().layers.len(), 2);

        // Cancel leaves the one under way and what waits, and says so.
        let before = app.session.map().layers.len();
        crate::layerchooser::add_file(&mut app, &first).unwrap();
        crate::layerchooser::add_file(&mut app, &second).unwrap();
        app.reads.job.as_ref().unwrap().control.cancel();
        landed(&mut app, 60);
        assert_eq!(app.session.map().layers.len(), before);
        assert!(app.session.notices.iter().any(|n| n.text == "Add Data of first.csv was cancelled"), "{:?}", app.session.notices.last());
        assert!(app.session.notices.iter().any(|n| n.text == "The one waiting after it was left too"), "{:?}", app.session.notices.last());

        // Another project opened meanwhile: they aren't added to it.
        crate::layerchooser::add_file(&mut app, &first).unwrap();
        crate::layerchooser::add_file(&mut app, &second).unwrap();
        app.run("project.new", Value::Null).unwrap();
        landed(&mut app, 60);
        assert!(app.session.map().layers.is_empty());
        assert!(app.session.notices.iter().any(|n| n.text == "2 reads were left, the first first.csv: another project was opened"));

        // Closing the app cancels a read under way and waits for it.
        crate::layerchooser::add_file(&mut app, &first).unwrap();
        stop(&mut app, std::time::Duration::from_secs(60));
        assert!(!busy(&app));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file added on a thread gives the project its coordinate system when the project takes
    /// it from the first layer added: the read lands through the same seam as a command.
    #[test]
    fn a_file_added_on_a_thread_gives_the_project_its_system() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bern.shp");
        let mut w = rustgis_engine::Session::new(rustgis_engine::Project::default());
        w.run("project.coordinateSystem", &json!({"crs": "EPSG:2056"})).unwrap();
        w.run("layer.template", &json!({"template": "points", "name": "Bern"})).unwrap();
        w.map_mut().editing.settings.enabled = true;
        w.run("edit.addFeature", &json!({"layer": "Bern", "geometry": {"type": "Point", "coordinates": [7.43863, 46.95108]}})).unwrap();
        w.run("share.exportLayer", &json!({"layer": "Bern", "path": path})).unwrap();
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.reads_off_frame = true;
        app.run("project.coordinateSystem", json!({"fromFirstLayer": true})).unwrap();
        assert_eq!(crate::layerchooser::add_file(&mut app, &path.display().to_string()).unwrap()["started"], "Add Data");
        landed(&mut app, 60);
        assert_eq!(app.session.map().layers.len(), 1);
        assert_eq!(app.session.project.coordinate_system, Some(rustgis_engine::data::raster::Crs::Epsg(2056)));
        app.run("edit.undo", Value::Null).unwrap();
        assert_eq!(app.session.project.coordinate_system, None, "Undo takes it back with the layer");
    }

    /// Scripted Delimited Text picks join the same background queue as ordinary file picks.
    #[test]
    fn scripted_delimited_picks_run_in_background() {
        use base64::Engine as _;
        let dir = tempfile::tempdir().unwrap();
        let gzip = base64::engine::general_purpose::STANDARD.decode("H4sIAAAAAAAC/8tMqUlNLC6pycsvKsngcqwxNKgxMuACACT4xC0WAAAA").unwrap();
        for (name, bytes) in [("scripted.dat", b"id|east|north\nA|10|20\n".as_slice()), ("scripted.csv.gz", gzip.as_slice())] {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            app.reads_off_frame = true;
            crate::dialogs::PICKED.with(|p| *p.borrow_mut() = Some(path.display().to_string()));
            let result = app.run("ui.addDelimited", json!({"x":"east", "y":"north", "delimiter":"|"})).unwrap();
            assert_eq!(result["started"], "Add Data");
            assert!(app.session.map().layers.is_empty());
            landed(&mut app, 30);
            let point = app.session.map().layers[0].dataset.features[0].geometry.as_ref().unwrap().label_point().unwrap();
            assert_eq!((point.x, point.y), (10.0, 20.0));
        }
    }

    /// The Error Inspector's check runs on a thread of its own (#1291), and its errors are kept as
    /// it lands.
    #[test]
    fn a_topology_check_runs_on_a_thread() {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.reads_off_frame = true;
        let square = |x: f64| {
            json!({"type": "Feature", "properties": {}, "geometry": {"type": "Polygon",
            "coordinates": [[[x, 0.0], [x + 1.0, 0.0], [x + 1.0, 1.0], [x, 1.0], [x, 0.0]]]}})
        };
        let parcels = json!({"type": "FeatureCollection", "features": [square(0.0), square(0.5)]});
        app.run("layer.addDataText", json!({"fileName": "parcels.geojson", "text": parcels.to_string()})).unwrap();
        app.run("topology.rules", json!({"add": {"rule": "mustNotOverlap", "layer": "parcels"}})).unwrap();
        let mark = app.session.notice_mark();
        let started = app.run("topology.validate", Value::Null).unwrap();
        assert_eq!(started["started"], "Error Inspector", "{started}");
        landed(&mut app, 60);
        assert_eq!(app.session.topology_errors.len(), 1);
        assert!(app.session.notices_since(mark).iter().all(|n| !n.error), "it landed without an error");
    }

    #[test]
    fn load_all_reads_on_a_thread() {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.reads_off_frame = true;
        let dir = std::env::temp_dir().join(format!("rustgis-read-job-all-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("streamed.geojson");
        std::fs::write(&path, r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[8,47]}}]}"#)
            .unwrap();
        let id = app.run("layer.addData", json!({"path": path.display().to_string(), "stream": true})).unwrap()["layer"].as_u64().unwrap()
            as u32;
        assert_eq!(run(&mut app, "layer.loadAll", json!({"layer": id})).unwrap()["started"], "Load All Features");
        assert!(app.session.map().layer(id).unwrap().is_streamed());
        landed(&mut app, 60);
        let l = app.session.map().layer(id).unwrap();
        assert!(!l.is_streamed() && l.dataset.features.len() == 1);

        // Asked twice, it reads once; another map open when it lands leaves it, and says so.
        let id = app.run("layer.addData", json!({"path": path.display().to_string(), "stream": true})).unwrap()["layer"].as_u64().unwrap()
            as u32;
        run(&mut app, "layer.loadAll", json!({"layer": id})).unwrap();
        assert!(run(&mut app, "layer.loadAll", json!({"layer": id})).unwrap().get("reading").is_some());
        assert_eq!(app.reads.waiting.len(), 0);
        app.run("map.new", json!({"name": "Other"})).unwrap();
        landed(&mut app, 60);
        let notice = format!("{} was left: the map it was asked for isn't open", app.session.project.maps[0].layer(id).unwrap().name);
        assert!(app.session.notices.iter().any(|n| n.text == notice), "{:?}", app.session.notices.last());
        assert!(app.session.project.maps[0].layer(id).unwrap().is_streamed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn archive(path: &std::path::Path, credit: &str) {
        use rustgis_engine::render::rustgis_tiles::{mvt, pmtiles};
        let metadata = pmtiles::NewArchive {
            tile_type: pmtiles::TileType::Mvt,
            gzipped_tiles: false,
            bounds: [-180.0, -85.0, 180.0, 85.0],
            metadata: json!({"attribution":credit}),
        };
        let bytes = pmtiles::write(vec![(0, 0, 0, mvt::encode(&mvt::Tile { layers: Vec::new() }))], &metadata).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn direct_relay_archive_joins_existing_read_queue_and_answers_only_after_landing() {
        use rustgis_assistant::relay::{Relay, read_token_file};
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;
        let _turn = crate::fetching::turn();
        let dir = std::env::temp_dir().join(format!("rustgis-relay-read-queue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("original.pmtiles");
        let replacement = dir.join("replacement.pmtiles");
        let csv = dir.join("first.csv");
        archive(&original, "Original");
        archive(&replacement, "Replacement");
        std::fs::write(&csv, "name,x,y\nA,8,47\n").unwrap();
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("map.addTiles", json!({"url":original,"name":"Linked","overlay":true})).unwrap();
        app.reads_off_frame = true;
        run(&mut app, "layer.addData", json!({"path":csv})).unwrap();
        let relay = Relay::start(0).unwrap();
        let grant = relay.grant_reviewed(false).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let token = read_token_file(grant.token_file()).unwrap();
        writeln!(client, "{}", json!({"jsonrpc":"2.0","method":"rustgis/attach","params":{"token":token,"access":true}})).unwrap();
        writeln!(client, "{}", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"run_command","arguments":{"command":"map.repairTiles","params":{"index":0,"path":replacement}}}})).unwrap();
        client.flush().unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            relay.poll(&mut app.session);
            if let Some(approval) = grant.pending().first() {
                grant.decide(approval.id, true);
            }
            if let Some(ahead) = relay.take_archive_read() {
                admitted(&mut app, ahead, relay.clone());
                break;
            }
            assert!(std::time::Instant::now() < end, "relay never admitted the archive");
            std::thread::sleep(Duration::from_millis(5)); // test-wait: legacy polling for relay archive admission
        }
        assert_eq!(app.reads.waiting.len(), 1, "the relay must reuse the one existing worker queue");
        assert_eq!(app.session.map().tiles[0].url, original.to_str().unwrap());
        client.set_nonblocking(true).unwrap();
        assert_eq!(client.peek(&mut [0u8; 1]).unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
        client.set_nonblocking(false).unwrap();
        landed(&mut app, 60);
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        let text = reply["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("replacement") || text.contains("Repaired"), "the agent's own change is reported: {text}");
        assert!(!text.contains("layersAdded") && !text.contains("first"), "the queued user read is not the agent's change: {text}");
        assert_eq!(app.session.map().tiles[0].url, replacement.to_str().unwrap());
        assert_eq!(app.session.map().layers.len(), 1);
        assert!(app.session.undo_actor().flatten().is_some_and(|actor| actor.starts_with("agent 1 (relay run ")));
        relay.stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    thread_local! {
        static TOLD: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn record(tag: &str, landed: Landed) {
        TOLD.with(|told| {
            told.borrow_mut().push(match landed {
                Landed::NotRun(why) => format!("{tag}: {why}"),
                Landed::Ran(_) => format!("{tag}: ran"),
            });
        });
    }

    fn told_running(_: &mut RustGisApp, landed: Landed) {
        record("running", landed);
    }

    fn told_waiting(_: &mut RustGisApp, landed: Landed) {
        record("waiting", landed);
    }

    #[test]
    fn stopping_ends_the_relay_batch_unrun_and_tells_the_reads_that_never_ran_in_order() {
        use std::io::{BufRead, BufReader, Write};
        let _turn = crate::fetching::turn();
        TOLD.with(|told| told.borrow_mut().clear());
        let dir = std::env::temp_dir().join(format!("rustgis-relay-read-stop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("original.pmtiles");
        let replacement = dir.join("replacement.pmtiles");
        let csv = dir.join("first.csv");
        archive(&original, "Original");
        archive(&replacement, "Replacement");
        std::fs::write(&csv, "name,x,y\nA,8,47\n").unwrap();
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("map.addTiles", json!({"url":original,"name":"Linked","overlay":true})).unwrap();
        app.reads_off_frame = true;
        // One read under way and one waiting, each with a callback.
        assert_eq!(run_then(&mut app, "layer.addData", json!({"path":csv}), Some(told_running)).unwrap()["started"], "Add Data");
        assert_eq!(run_then(&mut app, "layer.addData", json!({"path":csv}), Some(told_waiting)).unwrap()["waiting"], 1);
        // The agent's batch: two archive commands, then a command that changes the map.
        let batch = json!([
            relay_command(1, "map.repairTiles", json!({"index":0,"path":replacement})),
            relay_command(2, "map.reloadTiles", json!({"index":0})),
            relay_command(3, "layer.template", json!({"template":"points","name":"Mutated"})),
        ]);
        let (relay, _grant, client, ahead) = paused_relay(&mut app, batch);
        admitted(&mut app, ahead, relay.clone());
        assert_eq!(app.reads.waiting.len(), 2);
        let layers = app.session.map().layers.len();
        // User work said after admission belongs to the user, not to the agent's closing error.
        app.session.notify("Unrelated user notice after admission".to_string(), false);
        stop(&mut app, Duration::from_secs(10));
        assert!(!busy(&app));
        let why = tl!("RustGIS is closing").to_string();
        assert_eq!(TOLD.with(|told| told.borrow().clone()), [format!("running: {why}"), format!("waiting: {why}")]);
        let mut client = BufReader::new(client);
        let mut line = String::new();
        client.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply[0]["result"]["isError"], true, "{reply}");
        assert!(!reply[0].to_string().contains("Unrelated user notice"), "the agent's error carries only its own notices: {reply}");
        for rest in [1, 2] {
            assert_eq!(reply[rest]["error"]["message"], why.as_str(), "{reply}");
        }
        assert_eq!(app.session.map().tiles[0].url, original.to_str().unwrap(), "the repair never ran");
        assert_eq!(app.session.map().layers.len(), layers, "the batch's later commands never ran");
        assert!(app.session.map().layer_by_name("Mutated").is_none());
        assert!(relay.take_archive_read().is_none(), "the second archive command was never admitted");
        // The relay is not left holding the batch: the agent's next call is served.
        writeln!(client.get_mut(), "{}", relay_command(4, "map.tiles", json!({}))).unwrap();
        client.get_mut().flush().unwrap();
        let end = std::time::Instant::now() + Duration::from_secs(10);
        // Short waits, so each wait for the reply is followed by another poll.
        client.get_ref().set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        line.clear();
        let next = loop {
            relay.poll(&mut app.session);
            match client.read_line(&mut line) {
                Ok(n) if n > 0 => break serde_json::from_str::<Value>(&line).unwrap(),
                _ => assert!(std::time::Instant::now() < end, "the relay never answered the next call"),
            }
        };
        assert_eq!(next["result"]["isError"], false, "{next}");
        relay.stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn archive_reload_lands_once_and_stale_or_cancelled_reads_keep_retained_metadata() {
        let _turn = crate::fetching::turn();
        use rustgis_engine::render::rustgis_tiles::pmtiles;
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let dir = std::env::temp_dir().join(format!("rustgis-read-archive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.pmtiles");
        let url = path.to_str().unwrap();
        archive(&path, "Before");
        app.run("map.addTiles", json!({"url":url,"overlay":true})).unwrap();
        let retained = pmtiles::info_blocking(url, Duration::from_secs(10)).unwrap();
        let tile = app.session.map().tiles[0].clone();
        let camera = app.session.map().camera;
        archive(&path, "After");
        app.reads_off_frame = true;
        let depth = app.session.undo_depth();
        assert_eq!(app.run("map.reloadTiles", json!({"index":0})).unwrap()["started"], "Reload Data");
        assert!(Arc::ptr_eq(&pmtiles::info_blocking(url, Duration::from_secs(10)).unwrap(), &retained));
        assert_eq!(app.session.undo_depth(), depth, "validation has not installed anything");
        landed(&mut app, 60);
        let current = pmtiles::info_blocking(url, Duration::from_secs(10)).unwrap();
        assert_eq!(current.attribution(), "After");
        assert_eq!(app.session.map().tiles[0], tile);
        assert_eq!(app.session.map().camera, camera);
        assert_eq!(app.session.undo_depth(), depth + 1);

        archive(&path, "Must not land");
        app.run("map.reloadTiles", json!({"index":0})).unwrap();
        app.run("map.setTiles", json!({"index":0,"opacity":0.4})).unwrap();
        landed(&mut app, 60);
        assert_eq!(app.session.map().tiles[0].opacity, 0.4);
        assert!(Arc::ptr_eq(&pmtiles::info_blocking(url, Duration::from_secs(10)).unwrap(), &current));
        assert!(app.session.notices.iter().any(|n| n.text.contains("tile archive changed")));

        app.run("map.reloadTiles", json!({"index":0})).unwrap();
        app.reads.cancel_for_tests();
        landed(&mut app, 60);
        assert!(Arc::ptr_eq(&pmtiles::info_blocking(url, Duration::from_secs(10)).unwrap(), &current));
        app.run("map.reloadTiles", json!({"index":0})).unwrap();
        app.run("map.new", json!({"name":"Different map"})).unwrap();
        landed(&mut app, 60);
        assert!(app.session.map().tiles.is_empty());
        assert!(Arc::ptr_eq(&pmtiles::info_blocking(url, Duration::from_secs(10)).unwrap(), &current));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn archive_repair_does_not_report_success_before_validation_lands() {
        let _turn = crate::fetching::turn();
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let dir = std::env::temp_dir().join(format!("rustgis-read-archive-repair-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("original.pmtiles");
        let replacement = dir.join("replacement.pmtiles");
        archive(&original, "Original");
        archive(&replacement, "Replacement");
        app.run("map.addTiles", json!({"url":original.to_str().unwrap(),"name":"Linked","overlay":true})).unwrap();
        app.reads_off_frame = true;
        let mark = app.session.notice_mark();
        assert_eq!(
            app.run("map.repairTiles", json!({"index":0,"path":replacement.to_str().unwrap()})).unwrap()["started"],
            "Repair Data Source"
        );
        assert_eq!(app.session.map().tiles[0].url, original.to_str().unwrap());
        assert!(!app.session.notices_since(mark).iter().any(|n| n.text.contains("Repaired")));
        landed(&mut app, 60);
        assert_eq!(app.session.map().tiles[0].url, replacement.to_str().unwrap());
        assert!(app.session.notices_since(mark).iter().any(|n| n.text == "Repaired Linked"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Convert To Annotation, clicked for a layer read in place (whose view holds none of its
    /// features), reads all of its source on a thread first: every feature gets its annotation.
    #[test]
    fn labels_of_a_layer_read_in_place_convert_after_a_read_on_a_thread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("streamed.geojson");
        let point = |name: &str, x: f64| json!({"type": "Feature", "properties": {"name": name}, "geometry": {"type": "Point", "coordinates": [x, 47.0]}});
        std::fs::write(&path, json!({"type": "FeatureCollection", "features": [point("A", 8.0), point("B", 9.0)]}).to_string()).unwrap();
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.reads_off_frame = true;
        let id =
            app.run("layer.addData", json!({"path": path, "stream": true, "name": "places"})).unwrap()["layer"].as_u64().unwrap() as u32;
        assert!(app.session.map().layer(id).unwrap().dataset.features.is_empty(), "nothing read for a view yet");
        app.session.active_layer = Some(id);
        assert_eq!(app.run_by_user("labels.convert", Value::Null).unwrap()["started"], "Convert To Annotation");
        landed(&mut app, 60);
        let annotation = app.session.map().layer_by_name("places Annotation").unwrap();
        assert_eq!(annotation.dataset.features.len(), 2);
        let l = app.session.map().layer(id).unwrap();
        assert!(l.is_streamed() && !l.labels.enabled);
    }

    /// A size mapping set after a read on a thread is recorded naming its layer, as a step from
    /// the panes is, not by the id the read was bound to (#2130 review).
    #[test]
    fn a_size_mapping_read_on_a_thread_is_recorded_by_layer_name() {
        use rustgis_engine::data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value as V};
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let mut ds =
            Dataset::new("towns", GeometryType::Point, vec![Field::new("k", FieldType::Text), Field::new("n", FieldType::Integer)]);
        for (k, n) in [("a", 1), ("b", 5)] {
            ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![V::Text(k.into()), V::Int(n)]);
        }
        let id = rustgis_engine::cmd::layer::add_dataset(&mut app.session, ds);
        app.run("layer.symbology", json!({"layer": id, "type": "unique", "field": "k"})).unwrap();
        app.reads_off_frame = true;
        app.ws.recording = Some(Vec::new());
        app.run("layer.sizeBy", json!({"layer": id, "field": "n"})).unwrap();
        landed(&mut app, 60);
        assert!(app.session.map().layer(id).unwrap().size_by.is_some());
        let steps = app.ws.recording.clone().unwrap();
        let step = steps.iter().find(|s| s.command == "layer.sizeBy").expect("recorded");
        assert_eq!(step.params["layer"], json!("towns"));
    }

    /// Import QGIS Style reads its file and its raster image markers' images on a thread first
    /// (#2620): the layer is styled when they have been read, with the notes the command gives.
    #[test]
    fn a_qgis_style_is_read_on_a_thread_before_it_lands() {
        use rustgis_engine::data::{Coord, Dataset, Geometry, GeometryType};
        let dir = tempfile::tempdir().unwrap();
        let picture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../map/testdata/qgis/generated/data/picture.png");
        let path = dir.path().join("pin.qml");
        std::fs::write(
            &path,
            format!(
                r#"<qgis version="4.2.3"><renderer-v2 type="singleSymbol"><symbols><symbol type="marker" name="0" alpha="1">
                <layer class="RasterMarker" enabled="1"><Option type="Map"><Option name="imageFile" value="{}"/><Option name="size" value="6"/>
                </Option></layer></symbol></symbols></renderer-v2></qgis>"#,
                picture.display()
            ),
        )
        .unwrap();
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let mut ds = Dataset::new("towns", GeometryType::Point, vec![]);
        ds.push(Some(Geometry::Point(Coord::new(0.0, 0.0))), vec![]);
        let id = rustgis_engine::cmd::layer::add_dataset(&mut app.session, ds);
        app.reads_off_frame = true;
        let params = json!({"layer": id, "path": path});
        assert_eq!(app.run_by_user("style.importQgis", params).unwrap()["started"], "Import QGIS Style");
        assert!(
            matches!(app.session.map().layer(id).unwrap().symbol(0).marker, rustgis_engine::map::Marker::Circle),
            "styled only once it has been read"
        );
        landed(&mut app, 60);
        assert!(matches!(app.session.map().layer(id).unwrap().symbol(0).marker, rustgis_engine::map::Marker::Picture(_)));
        app.run("edit.undo", Value::Null).unwrap();
        assert!(matches!(app.session.map().layer(id).unwrap().symbol(0).marker, rustgis_engine::map::Marker::Circle));
        // A clean style says nothing; one whose image isn't a PNG or JPEG says so once, on landing.
        let said = |app: &RustGisApp| app.session.notices.iter().filter(|n| n.text.contains("isn't a PNG or JPEG")).count();
        assert_eq!(said(&app), 0);
        std::fs::write(dir.path().join("pin.gif"), b"GIF89a").unwrap();
        let gif = dir.path().join("gif.qml");
        let text = std::fs::read_to_string(&path).unwrap().replace(&picture.display().to_string(), "pin.gif");
        std::fs::write(&gif, text).unwrap();
        app.run_by_user("style.importQgis", json!({"layer": id, "path": gif})).unwrap();
        assert_eq!(said(&app), 0, "said on landing, not before");
        landed(&mut app, 60);
        assert_eq!(said(&app), 1);
    }

    /// Task steps cannot bypass the worker through direct or scripted table writes.
    #[test]
    fn a_task_refuses_table_writes_before_scanning_rows() {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let csv = format!("n\n{}", "1\n".repeat(20_000));
        app.run("layer.addDataText", json!({"fileName": "records.csv", "text": csv})).unwrap();
        app.run("edit.enable", json!({"on": true})).unwrap();
        app.reads_off_frame = true;
        for (command, params) in [
            ("edit.calculateField", json!({"table": "records", "field": "n", "expression": "n + 1"})),
            ("edit.deleteField", json!({"table": "records", "field": "n"})),
            ("script.run", json!({"script": "edit.calculateField {\"table\":\"records\",\"field\":\"n\",\"expression\":\"n + 1\"}"})),
        ] {
            app.run("task.new", json!({"name": "Edit records", "steps": [{"title": "Write table", "command": command, "params": params}]}))
                .unwrap();
            let before = serde_json::to_value(&app.session.project.tables).unwrap();
            let depth = app.session.undo_depth();
            let error = app.run_by_user("task.run", json!({"task": "Edit records"})).unwrap_err();
            assert!(error.contains("run the table edit directly"), "{error}");
            assert_eq!(serde_json::to_value(&app.session.project.tables).unwrap(), before);
            assert_eq!(app.session.undo_depth(), depth);
            app.run("task.remove", json!({"task": "Edit records"})).unwrap();
        }
    }

    /// The actual app dispatch schedules a table calculation and installs it once with Undo.
    /// A cell edited while it runs prevents stale results from overwriting the new value.
    #[test]
    fn table_calculation_runs_off_frame_and_refuses_changed_input() {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("layer.addDataText", json!({"fileName": "calc.csv", "text": "n\n1\n2\n"})).unwrap();
        app.run("edit.enable", json!({"on": true})).unwrap();
        app.reads_off_frame = true;
        let p = json!({"table": "calc", "field": "n", "expression": "n / 2"});
        let depth = app.session.undo_depth();
        assert_eq!(app.run_by_user("edit.calculateField", p.clone()).unwrap()["started"], "Calculate Field");
        assert_eq!(app.session.undo_depth(), depth);
        assert_eq!(app.session.project.tables[0].dataset.value(0, 0).as_f64(), Some(1.0));
        landed(&mut app, 60);
        assert_eq!(app.session.project.tables[0].dataset.value(0, 0).as_f64(), Some(0.0));
        assert_eq!(app.session.undo_depth(), depth + 1);
        assert!(app.session.notices.iter().any(|n| n.text.contains("fraction")));
        app.run("edit.undo", json!({})).unwrap();
        assert_eq!(app.session.project.tables[0].dataset.value(0, 0).as_f64(), Some(1.0));
        app.run_by_user("edit.calculateField", p).unwrap();
        app.session.run("edit.setValue", &json!({"table": "calc", "row": 0, "field": "n", "value": 9})).unwrap();
        let depth = app.session.undo_depth();
        landed(&mut app, 60);
        assert_eq!(app.session.project.tables[0].dataset.value(0, 0).as_f64(), Some(9.0));
        assert_eq!(app.session.undo_depth(), depth);
        assert!(app.session.notices.iter().any(|n| n.text.contains("table changed")));
    }

    /// The Script pane dispatch prepares table writes off frame and lands them as one undo step.
    #[test]
    fn a_table_script_runs_off_frame() {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("layer.addDataText", json!({"fileName": "records.csv", "text": "n\n1\n2\n"})).unwrap();
        app.reads_off_frame = true;
        let before = serde_json::to_value(&app.session.project.tables).unwrap();
        let depth = app.session.undo_depth();
        let p = json!({"script": "edit.addField {\"table\":\"records\",\"name\":\"result\",\"type\":\"long\"}\nedit.enable {\"on\":true}\nedit.calculateField {\"table\":\"records\",\"field\":\"result\",\"expression\":\"n + 1\"}"});
        assert_eq!(run(&mut app, "script.run", p.clone()).unwrap()["started"], "Run Script");
        assert_eq!(serde_json::to_value(&app.session.project.tables).unwrap(), before);
        landed(&mut app, 60);
        let data = &app.session.project.tables[0].dataset;
        assert_eq!(data.value(1, data.field_index("result").unwrap()).as_f64(), Some(3.0));
        assert_eq!(app.session.undo_depth(), depth + 1);
        app.run("edit.undo", Value::Null).unwrap();
        assert_eq!(serde_json::to_value(&app.session.project.tables).unwrap(), before);
        // A live edit invalidates the prepared script rather than rerunning its calculation.
        app.run("edit.enable", json!({"on": true})).unwrap();
        run(&mut app, "script.run", p.clone()).unwrap();
        app.session.run("edit.setValue", &json!({"table": "records", "row": 0, "field": "n", "value": 9})).unwrap();
        let current = (serde_json::to_value(&app.session.project.tables).unwrap(), app.session.undo_depth());
        landed(&mut app, 60);
        assert_eq!((serde_json::to_value(&app.session.project.tables).unwrap(), app.session.undo_depth()), current);
        assert!(app.session.notices.iter().any(|n| n.text.contains("table changed")));
        // An unsafe prefix is refused before the desktop script can reach a table write.
        let mixed = json!({"script": "project.new {}\nedit.addField {\"table\":\"records\",\"name\":\"extra\",\"type\":\"long\"}"});
        assert!(run(&mut app, "script.run", mixed).unwrap_err().contains("separate scripts"));
        assert_eq!((serde_json::to_value(&app.session.project.tables).unwrap(), app.session.undo_depth()), current);
        app.reads.fail_spawn = true;
        assert!(run(&mut app, "script.run", p).unwrap_err().contains("worker could not start"));
        assert_eq!((serde_json::to_value(&app.session.project.tables).unwrap(), app.session.undo_depth()), current);
    }

    /// A file-producing model before a table write must be refused before its file is written.
    #[test]
    fn a_table_script_refuses_a_file_model_before_any_live_line() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("model.pmtiles");
        let mut app = RustGisApp::with_sample();
        app.run("layer.addDataText", json!({"fileName": "records.csv", "text": "n\n1\n2\n"})).unwrap();
        app.run("model.new", json!({"name": "File", "steps": [{"tool": "vectorToPmtiles", "params": {"input": "Cities", "minZoom": 0, "maxZoom": 0, "output": output}}]})).unwrap();
        app.reads_off_frame = true;
        let before = (serde_json::to_value(&app.session.project.tables).unwrap(), app.session.undo_depth());
        let p = json!({"script": "model.run {\"name\":\"File\"}\nedit.addField {\"table\":\"records\",\"name\":\"extra\"}"});
        let result = run(&mut app, "script.run", p);
        if result.is_ok() {
            landed(&mut app, 60);
        }
        assert!(!output.exists(), "the model must not write before a missing table result is refused");
        assert_eq!((serde_json::to_value(&app.session.project.tables).unwrap(), app.session.undo_depth()), before);
        assert!(result.is_err() || app.session.notices.iter().any(|n| n.text.contains("separate scripts")));
    }

    /// A failed worker start refuses every registered table write without installing or undoing data.
    #[test]
    fn a_table_worker_failure_does_not_run_on_the_frame() {
        for (command, p) in [
            ("edit.setValue", json!({"table": "records", "row": 0, "field": "n", "value": 9})),
            ("edit.addFeature", json!({"table": "records", "attributes": {"n": 3}})),
            ("edit.addField", json!({"table": "records", "name": "extra", "type": "text"})),
            ("edit.deleteField", json!({"table": "records", "field": "day"})),
            ("edit.convertField", json!({"table": "records", "field": "day", "type": "date"})),
            ("edit.calculateField", json!({"table": "records", "field": "n", "expression": "n + 1"})),
        ] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            app.run("layer.addDataText", json!({"fileName": "records.csv", "text": "n,day\n1,2026-01-01\n2,2026-01-02\n"})).unwrap();
            app.run("edit.enable", json!({"on": true})).unwrap();
            app.reads_off_frame = true;
            app.reads.fail_spawn = true;
            let before = (serde_json::to_value(&app.session.project.tables).unwrap(), app.session.undo_depth());
            let error = app.run_by_user(command, p).unwrap_err();
            assert!(error.contains("worker could not start"), "{command}: {error}");
            assert_eq!((serde_json::to_value(&app.session.project.tables).unwrap(), app.session.undo_depth()), before, "{command}");
            assert!(!showing(&app));
        }
    }

    /// Waiting writes consume their predecessor's output, with one undo step per command.
    #[test]
    fn queued_table_writes_land_in_order() {
        let fixture = || {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            app.run("layer.addDataText", json!({"fileName": "records.csv", "text": "n\n1\n2\n"})).unwrap();
            app.run("edit.enable", json!({"on": true})).unwrap();
            app
        };
        let (mut app, mut expected) = (fixture(), fixture());
        app.reads_off_frame = true;
        let before = serde_json::to_value(&app.session.project.tables).unwrap();
        let depth = app.session.undo_depth();
        let commands = [
            ("edit.addFeature", json!({"table": "records", "attributes": {"n": 3}})),
            ("edit.addFeature", json!({"table": "records", "attributes": {"n": 4}})),
            ("edit.setValue", json!({"table": "records", "row": 3, "field": "n", "value": 9})),
            ("edit.addField", json!({"table": "records", "name": "extra", "type": "long"})),
            ("edit.calculateField", json!({"table": "records", "field": "extra", "expression": "n + 1"})),
        ];
        for (i, (command, p)) in commands.iter().enumerate() {
            expected.session.run(command, p).unwrap();
            let answer = app.run_by_user(command, p.clone()).unwrap();
            if i == 0 {
                assert!(answer.get("started").is_some());
            } else {
                assert_eq!(answer["waiting"], i);
            }
        }
        landed(&mut app, 60);
        assert_eq!(
            serde_json::to_value(&app.session.project.tables).unwrap(),
            serde_json::to_value(&expected.session.project.tables).unwrap()
        );
        assert_eq!(app.session.undo_depth(), depth + commands.len());
        assert!(app.session.notices.is_empty(), "{:?}", app.session.notices);
        for _ in commands {
            app.run("edit.undo", Value::Null).unwrap();
        }
        assert_eq!(serde_json::to_value(&app.session.project.tables).unwrap(), before);
        // Refreshing input must not retarget a waiting command, even if put_table keeps its id.
        for keeps_id in [false, true] {
            let mut app = fixture();
            app.reads_off_frame = true;
            let p = json!({"table": "records", "attributes": {"n": 3}});
            app.run_by_user("edit.addFeature", p.clone()).unwrap();
            app.run_by_user("edit.addFeature", p).unwrap();
            if keeps_id {
                let mut data = app.session.project.tables[0].dataset.clone();
                data.features[0].attrs[0] = rustgis_engine::data::Value::Int(7);
                app.session.project.put_table(data);
            } else {
                app.session.run("table.remove", &json!({"table": "records"})).unwrap();
                app.session.run("layer.addDataText", &json!({"fileName": "records.csv", "text": "n\n7\n"})).unwrap();
            }
            let replacement = (serde_json::to_value(&app.session.project.tables).unwrap(), app.session.undo_depth());
            landed(&mut app, 60);
            assert_eq!((serde_json::to_value(&app.session.project.tables).unwrap(), app.session.undo_depth()), replacement);
            assert!(app.session.notices.iter().any(|n| n.text.contains("table changed while the edit was waiting")));
        }
    }

    /// Landing rechecks the current editing setting before installing any queued row edit.
    #[test]
    fn table_row_writes_are_refused_if_editing_is_turned_off_before_landing() {
        for (command, p) in [
            ("edit.setValue", json!({"table": "records", "row": 0, "field": "n", "value": 9})),
            ("edit.addFeature", json!({"table": "records", "attributes": {"n": 3}})),
            ("edit.calculateField", json!({"table": "records", "field": "n", "expression": "n + 1"})),
        ] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            app.run("layer.addDataText", json!({"fileName": "records.csv", "text": "n\n1\n2\n"})).unwrap();
            app.run("edit.enable", json!({"on": true})).unwrap();
            app.reads_off_frame = true;
            let before = serde_json::to_value(&app.session.project.tables).unwrap();
            assert!(app.run_by_user(command, p).unwrap().get("started").is_some(), "{command}");
            app.run("edit.enable", json!({"on": false})).unwrap();
            let depth = app.session.undo_depth();
            let mark = app.session.notice_mark();
            landed(&mut app, 60);
            assert_eq!(serde_json::to_value(&app.session.project.tables).unwrap(), before, "{command}");
            assert_eq!(app.session.undo_depth(), depth, "{command}");
            assert!(app.session.notices_since(mark).iter().any(|n| n.text.contains("editing is turned off")), "{command}");
        }
    }

    /// Every table write uses the actual app worker path and lands the same fields, rows and
    /// notices as synchronous dispatch, as one undo step. Field edits still work with editing off.
    #[test]
    fn table_writes_run_off_frame_with_undo_and_notices() {
        use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType, Value as Attr};
        let fixture = || {
            let mut data =
                Dataset::new("records", GeometryType::None, vec![Field::new("n", FieldType::Integer), Field::new("day", FieldType::Text)]);
            data.push(None, vec![Attr::Int(1), Attr::Text("2026-01-02".into())]);
            data.push(None, vec![Attr::Int(2), Attr::Text("bad date".into())]);
            let mut project = rustgis_engine::Project::default();
            project.add_table(data);
            RustGisApp::new(rustgis_engine::Session::new(project))
        };
        for (command, p, editing) in [
            ("edit.setValue", json!({"table": "records", "row": 0, "field": "n", "value": 9.5}), true),
            ("edit.addFeature", json!({"table": "records", "attributes": {"n": 3}}), true),
            ("edit.addField", json!({"table": "records", "name": "extra", "type": "text"}), false),
            ("edit.deleteField", json!({"table": "records", "field": "day"}), false),
            ("edit.convertField", json!({"table": "records", "field": "day", "type": "date"}), false),
            ("edit.calculateField", json!({"table": "records", "field": "n", "expression": "n / 2"}), true),
        ] {
            let (mut app, mut expected) = (fixture(), fixture());
            for app in [&mut app, &mut expected] {
                app.run("edit.enable", json!({"on": editing})).unwrap();
            }
            let before = serde_json::to_value(&app.session.project.tables).unwrap();
            let depth = app.session.undo_depth();
            expected.session.run(command, &p).unwrap();
            app.reads_off_frame = true;
            assert!(app.run_by_user(command, p).unwrap().get("started").is_some(), "{command}");
            assert_eq!(serde_json::to_value(&app.session.project.tables).unwrap(), before, "not installed on this frame: {command}");
            landed(&mut app, 60);
            assert_eq!(
                serde_json::to_value(&app.session.project.tables).unwrap(),
                serde_json::to_value(&expected.session.project.tables).unwrap(),
                "{command}"
            );
            assert_eq!(app.session.notices, expected.session.notices, "{command}");
            assert_eq!(app.session.undo_depth(), depth + 1, "{command}");
            app.run("edit.undo", json!({})).unwrap();
            assert_eq!(serde_json::to_value(&app.session.project.tables).unwrap(), before, "{command}");
        }
    }

    /// Source File State hashes the file on a thread of its own and says what it found once
    /// it lands: Command Search shows no answer.
    #[test]
    fn a_source_file_state_check_runs_on_a_thread_and_says_what_it_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("places.geojson");
        publish_point(&path, 1.0);
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let id = app.run("layer.addData", json!({"path": path})).unwrap()["layer"].as_u64().unwrap();
        app.reads_off_frame = true;
        let mark = app.session.notice_mark();
        assert_eq!(app.run_by_user("source.syncState", json!({"layer": id})).unwrap()["started"], "Source File State");
        landed(&mut app, 60);
        let said: Vec<String> = app.session.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t == "places is in step with places.geojson"), "{said:?}");
    }

    #[test]
    fn relay_statistics_use_a_field_index_not_an_archive_index_while_queued() {
        use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType, Value as V};
        use std::io::{BufRead, BufReader};
        let dir = tempfile::tempdir().unwrap();
        let tiles = dir.path().join("context.pmtiles");
        archive(&tiles, "Context");
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("map.addTiles", json!({"url":tiles,"name":"Context","overlay":true})).unwrap();
        let mut ds = Dataset::new("numbers", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
        for n in [1, 2, 3] {
            ds.push(None, vec![V::Int(n)]);
        }
        let layer = rustgis_engine::cmd::layer::add_dataset(&mut app.session, ds);
        app.reads_off_frame = true;
        app.run("table.statistics", json!({"layer":layer,"index":0})).unwrap();
        let (relay, _grant, client, ahead) = paused_relay(&mut app, relay_command(1, "table.statistics", json!({"layer":layer,"index":0})));
        admitted(&mut app, ahead, relay.clone());
        assert_eq!(app.reads.waiting.len(), 1);
        // The column index happens to match this unrelated tile archive's index. Removing the
        // archive while the count waits must not invalidate the column statistics request.
        app.session.map_mut().tiles.clear();
        landed(&mut app, 60);
        let mut text = String::new();
        BufReader::new(client).read_line(&mut text).unwrap();
        let answer: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(answer["result"]["isError"], false, "{answer}");
        assert!(answer["result"]["content"][0]["text"].as_str().unwrap().contains("\"sum\":6"), "{answer}");
        relay.stop();
    }

    /// The real pane button owns the final ranking, not the read queue's admission receipt.
    #[test]
    fn similar_pane_receives_success_failure_and_cancellation() {
        use egui_kittest::kittest::Queryable;
        use rustgis_engine::data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as V};
        for outcome in ["success", "error", "cancel", "spawn", "newer", "layer", "workflow", "reopen", "closed", "error_layer"] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let mut ds = Dataset::new("numbers", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
            for n in [0, 2, 10] {
                ds.push(
                    Some(Geometry::Point(rustgis_engine::geom::Coord::new(n as f64, n as f64))),
                    vec![if n == 0 && matches!(outcome, "error" | "error_layer") { V::Null } else { V::Int(n) }],
                );
            }
            let id = rustgis_engine::cmd::layer::add_dataset(&mut app.session, ds.clone());
            ds.name = "other".into();
            rustgis_engine::cmd::layer::add_dataset(&mut app.session, ds);
            app.session.map_mut().layer_mut(id).unwrap().selection = vec![0];
            app.reads_off_frame = true;
            app.reads.fail_spawn = outcome == "spawn";
            if outcome == "workflow" {
                crate::analysispane::run_elsewhere(
                    &mut app,
                    "interpolate",
                    "analysis.interpolate",
                    json!({"layer":"numbers","field":"n","cellSize":2}),
                );
                assert!(app.workflow.job.is_some(), "the raster workflow is pending before Similar starts");
            }
            app.run("ui.pane.workflows", json!({"section":"similar"})).unwrap();
            app.workflow.layer = "numbers".into();
            // Pause only the frame's completion pass until the button and queue ownership have
            // been inspected; draw the owning pane first, then use the real app frame's
            // completion hook below.
            let allow_landing = std::rc::Rc::new(std::cell::Cell::new(false));
            let allow = allow_landing.clone();
            let mut fonts = false;
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(1000).build_ui_state(
                move |ui, app: &mut RustGisApp| {
                    if !fonts {
                        crate::theme::install_fonts(ui.ctx(), &[], None);
                        fonts = true;
                        return;
                    }
                    if allow.get() {
                        app.logic(ui.ctx());
                        app.ui(ui);
                    } else {
                        crate::analysispane::workflows(app, ui, &Tokens::light());
                    }
                },
                app,
            );
            if outcome == "workflow" {
                // This deliberately held background job shows a spinner until we land it.
                for _ in 0..4 {
                    h.step();
                }
            } else {
                h.run();
            }
            h.get_by_role_and_label(egui::accesskit::Role::Button, "Find Similar").click();
            for _ in 0..4 {
                h.step();
            }
            assert_eq!(showing(h.state()), outcome != "spawn", "{outcome}: worker admission");
            assert!(h.state().workflow.result.is_none(), "a receipt is not a ranking");
            if outcome == "reopen" {
                h.state_mut().run("ui.pane.workflows", json!({"section":"similar"})).unwrap();
            } else if outcome == "closed" {
                h.state_mut().run("ui.pane.workflows", json!({})).unwrap();
                assert_ne!(h.state().ui.right, Some(crate::RightPane::Workflows));
            }
            match outcome {
                "workflow" => {
                    let end = std::time::Instant::now() + Duration::from_secs(60);
                    while crate::analysispane::land_job(h.state_mut()) {
                        assert!(std::time::Instant::now() < end, "the raster workflow never landed");
                        std::thread::sleep(Duration::from_millis(5)); // test-wait: the real raster completion lands while Similar remains pending
                    }
                    assert!(h.state().session.map().layers.iter().any(|l| l.is_raster()));
                    assert!(h.state().workflow.result.is_none(), "another section cannot show its result in the Similar pane");
                }
                "cancel" => h.state().reads.cancel_for_tests(),
                "layer" | "error_layer" => {
                    h.state_mut().workflow.layer = "other".into();
                    h.state_mut().workflow.result = Some(json!({"kept":"other layer"}));
                }
                "newer" => {
                    // The older completion cannot erase the newer pending state or show its
                    // ranking. Keep the worker slots deterministic without timing the scan.
                    let end = std::time::Instant::now() + Duration::from_secs(60);
                    while !h.state().reads.read_finished_for_tests() {
                        assert!(std::time::Instant::now() < end);
                        std::thread::sleep(Duration::from_millis(5)); // test-wait: completion slot before testing queued pane ownership
                    }
                    h.get_by_role_and_label(egui::accesskit::Role::Button, "Find Similar").click();
                    for _ in 0..4 {
                        h.step();
                    }
                    assert_eq!(h.state().reads.waiting.len(), 1);
                    land(h.state_mut());
                    assert!(h.state().workflow.result.is_none(), "older ranking cannot replace the newer pending request");
                    h.state().reads.cancel_for_tests();
                }
                _ => {}
            }
            allow_landing.set(true);
            let end = std::time::Instant::now() + Duration::from_secs(60);
            while showing(h.state()) || !h.state().reads.waiting.is_empty() {
                assert!(std::time::Instant::now() < end, "{outcome}: pane read never landed");
                h.step();
                std::thread::sleep(Duration::from_millis(5)); // test-wait: worker completion delivered by the real app frame
            }
            if outcome == "closed" {
                assert!(h.state().workflow.result.is_some(), "closing the pane keeps its completed ranking for reopening");
                h.state_mut().run("ui.pane.workflows", json!({"section":"similar"})).unwrap();
                h.step();
            }
            // Completion used the full app frame above. Inspect the actual pane alone so its
            // diagnostic cannot be confused with the same text in another app surface.
            allow_landing.set(false);
            h.step();
            h.run();
            match outcome {
                "success" | "workflow" | "reopen" | "closed" => {
                    let answer = h.state().workflow.result.as_ref().expect("the pane shows the completed ranking");
                    assert_eq!(answer["similar"].as_array().unwrap().len(), 2);
                    assert!(answer["similar"][0]["distance"].is_number());
                    assert!(h.query_by_label_contains("distance").is_some(), "the ranking is drawn in the pane");
                    assert!(h.state().workflow.error.is_none());
                    if outcome == "success" {
                        let answer = h.state().workflow.result.clone();
                        h.step();
                        assert_eq!(h.state().workflow.result, answer, "an unchanged layer keeps its ranking");
                        h.state_mut().run("layer.remove", json!({"layer":id})).unwrap();
                        h.step();
                        assert_eq!(h.state().workflow.layer, "other");
                        assert!(h.state().workflow.result.is_none() && h.state().workflow.error.is_none());
                        assert!(h.query_by_label_contains("distance").is_none(), "a fallback layer cannot show the removed layer's rows");
                    }
                }
                "error" => {
                    assert!(h.state().workflow.result.is_none());
                    assert!(h.state().workflow.error.as_deref().is_some_and(|e| e.contains("reference row")));
                    assert!(
                        h.query_by_role_and_label(egui::accesskit::Role::Label, h.state().workflow.error.as_deref().unwrap()).is_some()
                    );
                }
                "spawn" => {
                    assert!(h.state().workflow.result.is_none());
                    assert!(h.state().workflow.error.as_deref().is_some_and(|e| e.contains("background worker could not start")));
                    assert!(
                        h.query_by_role_and_label(egui::accesskit::Role::Label, h.state().workflow.error.as_deref().unwrap()).is_some()
                    );
                }
                "cancel" | "newer" => {
                    assert!(h.state().workflow.result.is_none());
                    assert!(h.state().workflow.error.as_deref().is_some_and(|e| e.contains("cancelled")));
                    assert!(
                        h.query_by_role_and_label(egui::accesskit::Role::Label, h.state().workflow.error.as_deref().unwrap()).is_some()
                    );
                }
                "layer" | "error_layer" => {
                    if outcome == "error_layer" {
                        assert_eq!(h.state().session.notices.iter().filter(|n| n.error && n.text.contains("reference row")).count(), 1);
                    }
                    assert_eq!(h.state().workflow.result, Some(json!({"kept":"other layer"})));
                    assert!(h.state().workflow.error.is_none());
                }
                _ => panic!("unexpected Similar pane scenario"),
            }
        }
    }

    /// UI and direct relay Similar requests defer selection until the worker lands; a failed
    /// worker spawn must refuse rather than compute synchronously on the window thread.
    #[test]
    fn similarity_uses_the_read_queue_for_ui_and_relay() {
        use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType, Value as V};
        use std::io::{BufRead, BufReader};
        for relay_call in [false, true] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let mut ds = Dataset::new("numbers", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
            for n in [0, 2, 10] {
                ds.push(None, vec![V::Int(n)]);
            }
            let id = rustgis_engine::cmd::layer::add_dataset(&mut app.session, ds);
            let l = app.session.map_mut().layer_mut(id).unwrap();
            l.selection = vec![0];
            l.definition_query = Some("n >= 2".into());
            app.reads_off_frame = true;
            let p = json!({"layer":id,"fields":["n"],"count":2});
            let relay = if relay_call {
                let (relay, grant, client, ahead) = paused_relay(&mut app, relay_command(1, "data.similar", p.clone()));
                admitted(&mut app, ahead, relay.clone());
                Some((relay, grant, client))
            } else {
                assert_eq!(app.run("data.similar", p.clone()).unwrap()["started"], "Find Similar");
                None
            };
            assert_eq!(app.session.map().layer(id).unwrap().selection, vec![0]);
            landed(&mut app, 60);
            assert_eq!(app.session.map().layer(id).unwrap().selection, vec![1, 2]);
            if let Some((relay, _grant, client)) = relay {
                let mut text = String::new();
                BufReader::new(client).read_line(&mut text).unwrap();
                let answer: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(answer["result"]["isError"], false, "{answer}");
                assert!(answer["result"]["content"][0]["text"].as_str().unwrap().contains("similar"));
                relay.stop();
            }
            app.reads.fail_spawn = true;
            let before = app.session.map().layer(id).unwrap().selection.clone();
            assert!(app.run("data.similar", p).unwrap_err().contains("background worker could not start"));
            assert_eq!(app.session.map().layer(id).unwrap().selection, before);
        }
    }

    /// Topology's worker is used by both UI and scoped relay paths, with no inline spawn fallback.
    #[test]
    fn topology_uses_the_read_queue_for_ui_and_relay() {
        use std::io::{BufRead, BufReader};
        for relay_call in [false, true] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let mut ds = rustgis_engine::data::Dataset::new("points", rustgis_engine::data::GeometryType::Point, vec![]);
            ds.push(Some(rustgis_engine::data::Geometry::Point(rustgis_engine::geom::Coord::new(1.0, 2.0))), vec![]);
            rustgis_engine::cmd::layer::add_dataset(&mut app.session, ds);
            app.run("topology.rules", json!({"add":{"rule":"mustNotSelfIntersect","layer":"points"}})).unwrap();
            app.reads_off_frame = true;
            let p = json!({});
            let relay = if relay_call {
                let (relay, grant, client, ahead) = paused_relay(&mut app, relay_command(1, "topology.validate", p.clone()));
                admitted(&mut app, ahead, relay.clone());
                Some((relay, grant, client))
            } else {
                assert_eq!(app.run("topology.validate", p.clone()).unwrap()["started"], "Error Inspector");
                None
            };
            landed(&mut app, 60);
            if let Some((relay, _grant, client)) = relay {
                let mut text = String::new();
                BufReader::new(client).read_line(&mut text).unwrap();
                let answer: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(answer["result"]["isError"], false, "{answer}");
                assert!(answer["result"]["content"][0]["text"].as_str().unwrap().contains("errors"));
                relay.stop();
            }
            app.reads.fail_spawn = true;
            assert!(app.run("topology.validate", p).unwrap_err().contains("background worker could not start"));
        }
    }

    /// Statistics… counts the column on a thread of its own and keeps the answer for its window
    /// once it lands; a layer changed meanwhile is left, and nothing is kept.
    #[test]
    fn column_statistics_are_counted_on_a_thread_and_kept_for_the_window() {
        use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType, Value as V};
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let mut ds = Dataset::new("big", GeometryType::Point, vec![Field::new("n", FieldType::Integer)]);
        for i in [9_007_199_254_740_993_i64, 1, 2] {
            ds.push(None, vec![V::Int(i)]);
        }
        let id = rustgis_engine::cmd::layer::add_dataset(&mut app.session, ds);
        app.session.table_layer = Some(id);
        app.reads_off_frame = true;
        let asked = json!({"layer": id, "field": "n", "index": 0});
        assert_eq!(app.run("table.statistics", asked.clone()).unwrap()["started"], "Column Statistics");
        assert!(app.table_stats_kept().is_none(), "nothing shown before it lands");
        landed(&mut app, 60);
        let kept = app.table_stats_kept().expect("kept once landed");
        assert_eq!(kept["sum"], 9_007_199_254_740_996_i64, "exact, not rounded");
        // The layer edited while it counts: left for the user to ask again.
        app.session.table_layer = Some(id);
        assert_eq!(app.run("table.statistics", asked).unwrap()["started"], "Column Statistics");
        app.session
            .run("edit.addFeature", &json!({"layer": id, "geometry": {"type": "Point", "coordinates": [1.0, 2.0]}, "attributes": {"n": 5}}))
            .unwrap();
        let mark = app.session.notice_mark();
        landed(&mut app, 60);
        let said: Vec<String> = app.session.notices_since(mark).iter().map(|n| n.text.clone()).collect();
        assert!(said.iter().any(|t| t.contains("changed while") || t.contains("left")), "{said:?}");
    }

    /// The desktop's Add Data reads a Shapefile (in a coordinate system given, as Import
    /// Shapefile asks) and a GeoJSON with `"stream": false` on a thread of its own, recording the
    /// file with the read.
    #[test]
    fn whole_files_are_read_and_recorded_on_a_thread() {
        let dir = tempfile::tempdir().unwrap();
        let geojson = dir.path().join("places.geojson");
        publish_point(&geojson, 1.0);
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("layer.addData", json!({"path": geojson, "name": "seed"})).unwrap();
        let shp = dir.path().join("towns.shp");
        app.run("share.exportLayer", json!({"layer": "seed", "path": shp})).unwrap();
        app.reads_off_frame = true;
        for (p, name) in [
            (json!({"path": shp, "crs": "EPSG:4326", "name": "towns"}), "towns"),
            // Another system than the file's: the rows recorded are the rows as read in it.
            (json!({"path": shp, "crs": "EPSG:3857", "name": "projected"}), "projected"),
            (json!({"path": geojson, "stream": false, "name": "whole"}), "whole"),
        ] {
            assert_eq!(run(&mut app, "layer.addData", p).unwrap()["started"], "Add Data", "{name}");
            landed(&mut app, 60);
            let id = app.session.map().layer_by_name(name).unwrap().id;
            assert!(app.session.map().layer(id).unwrap().synced.is_some(), "{name}");
            if name == "projected" {
                assert_eq!(app.session.map().layer(id).unwrap().dataset.crs, rustgis_engine::data::raster::Crs::parse("EPSG:3857").ok());
            }
            app.reads_off_frame = false;
            assert_eq!(app.run("source.syncState", json!({"layer": id})).unwrap()["state"], "inSync", "{name}");
            app.reads_off_frame = true;
        }
    }

    fn reload_fixture() -> (tempfile::TempDir, RustGisApp, u32, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("streamed.geojson");
        publish_point(&path, 1.0);
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.reads_off_frame = true;
        let id = app.run("layer.addData", json!({"path": path, "stream": true})).unwrap()["layer"].as_u64().unwrap() as u32;
        (dir, app, id, path)
    }

    fn publish_point(path: &std::path::Path, x: f64) {
        std::fs::write(
            path,
            json!({
                "type": "FeatureCollection", "features": [{
                    "type": "Feature", "properties": {}, "geometry": {"type": "Point", "coordinates": [x, 47]}
                }]
            })
            .to_string(),
        )
        .unwrap();
    }

    fn source_x(app: &RustGisApp, id: u32) -> f64 {
        app.session.map().layer(id).unwrap().source.as_ref().unwrap().info().unwrap().extent.center().x
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn editable_reload_fixture() -> (tempfile::TempDir, egui_kittest::Harness<'static, RustGisApp>, u32) {
        use rustgis_engine::data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value, raster::Crs, source::gpkg};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("places.gpkg");
        let mut data = Dataset::new("places", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
        data.push(Some(Geometry::Point(Coord::new(1.0, 20.0))), vec![Value::Text("original".into())]);
        gpkg::write_in(&data, &path, Crs::LonLat).unwrap();
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let id = app.run("gpkg.addLayer", json!({"path": path, "table": "places"})).unwrap()["layer"].as_u64().unwrap() as u32;
        app.run("edit.enable", json!({"on": true})).unwrap();
        app.run("edit.setValue", json!({"layer": id, "row": 0, "field": "name", "value": "pending"})).unwrap();
        app.reads_off_frame = true;
        let mut fonts = false;
        let h = egui_kittest::Harness::builder().with_size(egui::vec2(700.0, 500.0)).with_max_steps(64).build_ui_state(
            move |ui, app: &mut RustGisApp| {
                if !fonts {
                    crate::theme::install_fonts(ui.ctx(), &[], None);
                    fonts = true;
                    return;
                }
                // Drive the real confirmation without landing reads on the harness's frames.
                // The tests hold the worker's completion slot and land explicitly below.
                if app.dialogs.open == Some("reloadData") {
                    crate::reloaddata::show(app, ui.ctx());
                }
            },
            app,
        );
        (dir, h, id)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn confirm_reload(h: &mut egui_kittest::Harness<'_, RustGisApp>, id: u32) {
        use egui_kittest::kittest::Queryable;
        h.state_mut().run("ui.reloadData", json!({"layer": id})).unwrap();
        h.run_steps(3);
        h.get_by_label("Inserts: 0; updates: 1; deletes: 0.");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Discard Edits and Reload").click();
        h.run_steps(3);
        assert_eq!(h.state().dialogs.open, None);
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn edited_value(app: &RustGisApp, id: u32) -> rustgis_engine::data::Value {
        app.session.map().layer(id).unwrap().dataset.features[0].attrs[0].clone()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn chosen_vertex_reload_fixture(off_frame: bool, dragging: bool) -> (tempfile::TempDir, RustGisApp, u32, std::path::PathBuf) {
        use rustgis_engine::data::{Coord, Dataset, Geometry, GeometryType, VertexId, raster::Crs, source::gpkg};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roads.gpkg");
        let mut data = Dataset::new("roads", GeometryType::Polyline, vec![]);
        data.push(Some(Geometry::Line(vec![vec![Coord::new(0.0, 0.0), Coord::new(1.0, 0.0), Coord::new(2.0, 1.0)]])), vec![]);
        gpkg::write_in(&data, &path, Crs::LonLat).unwrap();
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        let id = app.run("gpkg.addLayer", json!({"path": path, "table": "roads"})).unwrap()["layer"].as_u64().unwrap() as u32;
        app.run("edit.enable", json!({"on": true})).unwrap();
        app.run("tool.set", json!({"tool": "editVertices"})).unwrap();
        app.run("select.rows", json!({"layer": id, "rows": [0]})).unwrap();
        app.reads_off_frame = off_frame;
        if dragging {
            app.session.map_mut().camera.res = 1000.0;
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
            let from = crate::mapview::lonlat_to_screen(&app, rect, Coord::new(1.0, 0.0)).unwrap();
            assert!(crate::vertexedit::drag_start(&mut app, rect, from));
            assert!(app.map.vertex_drag_owner.is_some());
        } else {
            crate::vertexedit::choose(&mut app, Some((id, 0, VertexId::new(0, 0, 1))));
        }
        assert_eq!(app.map.vertex, Some((id, 0, VertexId::new(0, 0, 1))));
        (dir, app, id, path)
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn successful_reload_clears_chosen_vertex_and_drag_only_for_the_replaced_layer() {
        use rustgis_engine::data::{Coord, Dataset, Geometry, GeometryType, VertexId, raster::Crs, source::gpkg};
        for off_frame in [false, true] {
            for dragging in [false, true] {
                let (dir, mut app, id, path) = chosen_vertex_reload_fixture(off_frame, dragging);
                let replacement = Geometry::Line(vec![vec![Coord::new(10.0, 0.0), Coord::new(11.0, 0.0), Coord::new(12.0, 1.0)]]);
                let mut data = Dataset::new("roads", GeometryType::Polyline, vec![]);
                data.push(Some(replacement.clone()), vec![]);
                let fresh = dir.path().join("replacement.gpkg");
                gpkg::write_in(&data, &fresh, Crs::LonLat).unwrap();
                std::fs::copy(fresh, &path).unwrap();
                app.run("ui.reloadData", json!({"layer": id})).unwrap();
                if off_frame {
                    assert!(showing(&app));
                    assert!(app.map.vertex.is_some(), "starting a read must not clear the vertex before it succeeds");
                    assert_eq!(app.map.vertex_drag.is_some(), dragging);
                    landed(&mut app, 60);
                }
                assert!(app.map.vertex.is_none());
                assert!(app.map.vertex_drag.is_none());
                assert!(app.map.vertex_drag_owner.is_none());
                app.run("select.rows", json!({"layer": id, "rows": [0]})).unwrap();
                let undo = app.session.undo_label().map(str::to_string);
                assert!(!crate::vertexedit::delete_chosen(&mut app), "Delete has no chosen vertex on the replacement row");
                crate::vertexedit::drag_stop(&mut app);
                assert_eq!(app.session.map().layer(id).unwrap().dataset.features[0].geometry, Some(replacement));
                assert_eq!(app.session.undo_label(), undo.as_deref(), "stale Delete and release create no edit");

                let other = app.run("gpkg.addLayer", json!({"path": path, "table": "roads"})).unwrap()["layer"].as_u64().unwrap() as u32;
                let chosen = Some((other, 0, VertexId::new(0, 0, 1)));
                crate::vertexedit::choose(&mut app, chosen);
                app.run("ui.reloadData", json!({"layer": id})).unwrap();
                landed(&mut app, 60);
                assert_eq!(app.map.vertex, chosen, "reloading another layer keeps this layer's chosen vertex");
            }
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn nested_and_direct_reload_cannot_retarget_a_chosen_vertex_or_drag() {
        use rustgis_engine::data::{Coord, Dataset, Geometry, GeometryType, raster::Crs, source::gpkg};
        for dispatch in ["script", "session-delete", "session-frame", "discard"] {
            for off_frame in [false, true] {
                for dragging in [false, true] {
                    let (dir, mut app, id, path) = chosen_vertex_reload_fixture(off_frame, dragging);
                    let replacement = Geometry::Line(vec![vec![Coord::new(10.0, 0.0), Coord::new(11.0, 0.0), Coord::new(12.0, 1.0)]]);
                    let mut data = Dataset::new("roads", GeometryType::Polyline, vec![]);
                    data.push(Some(replacement.clone()), vec![]);
                    let fresh = dir.path().join("replacement.gpkg");
                    gpkg::write_in(&data, &fresh, Crs::LonLat).unwrap();
                    std::fs::copy(fresh, &path).unwrap();
                    if dispatch == "script" {
                        let script = format!("layer.reloadData {}", json!({"layer": id}));
                        app.run("script.run", json!({"script": script})).unwrap();
                        if off_frame {
                            landed(&mut app, 60);
                        }
                        assert!(app.map.vertex.is_none(), "a script's reload clears its chosen vertex on landing");
                        assert!(app.map.vertex_drag.is_none());
                    } else if dispatch == "discard" {
                        app.run("gpkg.discardEdits", json!({"layer": id})).unwrap();
                        assert!(app.map.vertex.is_none(), "discarding edits also replaces row identities");
                        assert!(app.map.vertex_drag.is_none());
                    } else {
                        app.session.run("layer.reloadData", &json!({"layer": id})).unwrap();
                    }
                    // Selection can return before the UI epilogue or the next frame; Delete must
                    // still recognize the replacement rather than editing the new row's vertex.
                    app.session.run("select.rows", &json!({"layer": id, "rows": [0]})).unwrap();
                    if dispatch == "session-frame" {
                        crate::vertexedit::cancel_stale_drag(&mut app);
                        assert!(app.map.vertex.is_none(), "the frame's ownership check sees direct session replacements");
                        assert!(app.map.vertex_drag.is_none());
                    }
                    let undo = app.session.undo_label().map(str::to_string);
                    assert!(!crate::vertexedit::delete_chosen(&mut app));
                    assert!(app.map.vertex.is_none());
                    assert!(app.map.vertex_owner.is_none());
                    assert!(app.map.vertex_drag.is_none());
                    assert!(app.map.vertex_drag_owner.is_none());
                    crate::vertexedit::drag_stop(&mut app);
                    assert_eq!(app.session.map().layer(id).unwrap().dataset.features[0].geometry, Some(replacement));
                    assert_eq!(app.session.undo_label(), undo.as_deref(), "stale Delete and release create no edit");
                }
            }
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn ordinary_vertex_move_keeps_the_chosen_vertex() {
        use rustgis_engine::data::Coord;
        for dispatch in ["ui", "session", "drag"] {
            let (_dir, mut app, id, _path) = chosen_vertex_reload_fixture(false, dispatch == "drag");
            let chosen = app.map.vertex;
            let revision = app.session.map().layer(id).unwrap().replacement_revision();
            if dispatch == "drag" {
                let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 800.0));
                let to = crate::mapview::lonlat_to_screen(&app, rect, Coord::new(1.25, 0.25)).unwrap();
                crate::vertexedit::drag_to(&mut app, rect, to);
                crate::vertexedit::drag_stop(&mut app);
            } else {
                let p = json!({"layer": id, "row": 0, "action": "move", "part": 0, "ring": 0, "index": 1, "lon": 1.25, "lat": 0.25});
                if dispatch == "ui" {
                    app.run("edit.vertex", p).unwrap();
                } else {
                    app.session.run("edit.vertex", &p).unwrap();
                }
            }
            crate::vertexedit::cancel_stale_drag(&mut app);
            assert_eq!(app.map.vertex, chosen, "ordinary moves keep the chosen vertex for {dispatch}");
            assert!(app.map.vertex_owner.is_some());
            assert_eq!(app.session.map().layer(id).unwrap().replacement_revision(), revision);
            assert!(crate::vertexedit::delete_chosen(&mut app), "the moved vertex remains usable");
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn failed_reload_retains_chosen_vertex_and_held_drag() {
        for off_frame in [false, true] {
            for dragging in [false, true] {
                let (_dir, mut app, id, path) = chosen_vertex_reload_fixture(off_frame, dragging);
                let chosen = app.map.vertex;
                let geometry = app.session.map().layer(id).unwrap().dataset.features[0].geometry.clone();
                let undo = app.session.undo_label().map(str::to_string);
                std::fs::write(path, b"not a GeoPackage").unwrap();
                let mark = app.session.notice_mark();
                let result = app.run("ui.reloadData", json!({"layer": id}));
                if off_frame {
                    assert!(result.unwrap().get("started").is_some());
                    landed(&mut app, 60);
                } else {
                    assert!(result.is_err());
                }
                assert_eq!(app.session.notices_since(mark).iter().filter(|notice| notice.error).count(), 1);
                assert_eq!(app.map.vertex, chosen);
                assert_eq!(app.map.vertex_drag.is_some(), dragging);
                assert_eq!(app.map.vertex_drag_owner.is_some(), dragging);
                assert_eq!(app.session.map().layer(id).unwrap().dataset.features[0].geometry, geometry);
                assert_eq!(app.session.undo_label(), undo.as_deref());
                assert!(crate::vertexedit::delete_chosen(&mut app), "the original chosen vertex remains usable");
                assert_ne!(app.session.map().layer(id).unwrap().dataset.features[0].geometry, geometry);
            }
        }
    }
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn queued_reload_retains_confirmed_rows_schema_and_edit_baseline() {
        use rustgis_engine::data::Value as DataValue;
        for change in ["unchanged", "value", "schema", "baseline"] {
            let (dir, mut h, id) = editable_reload_fixture();
            // A real failed read occupies the queue without introducing an unrelated undo step.
            let blocker = dir.path().join("blocked.geojson");
            std::fs::write(&blocker, b"not GeoJSON").unwrap();
            assert_eq!(run(h.state_mut(), "layer.addData", json!({"path": blocker})).unwrap()["started"], "Add Data");
            let slot = h.state().reads.job.as_ref().unwrap().slot.clone();
            let held = slot.lock().unwrap();
            confirm_reload(&mut h, id);
            assert_eq!(h.state().reads.waiting.len(), 1);
            let counts = h.state_mut().run("gpkg.pendingEdits", json!({"layer": id})).unwrap();
            match change {
                "value" => {
                    h.state_mut().run("edit.setValue", json!({"layer": id, "row": 0, "field": "name", "value": "later"})).unwrap();
                }
                "schema" => h.state_mut().session.map_mut().layer_mut(id).unwrap().dataset.fields[0].name = "renamed".into(),
                "baseline" => {
                    let edit = h.state_mut().session.map_mut().layer_mut(id).unwrap().gpkg.as_mut().unwrap();
                    // Equal baseline values with a new COW identity must also invalidate consent.
                    edit.versions = edit.versions.iter().collect();
                }
                "unchanged" => {}
                _ => panic!("unexpected reload scenario"),
            }
            assert_eq!(h.state_mut().run("gpkg.pendingEdits", json!({"layer": id})).unwrap(), counts, "{change}");
            let before = h.state().session.undo_label().map(str::to_string);
            drop(held);
            landed(h.state_mut(), 60);
            if change == "unchanged" {
                assert_eq!(edited_value(h.state(), id), DataValue::Text("original".into()));
                h.state_mut().run("edit.undo", Value::Null).unwrap();
                assert_eq!(edited_value(h.state(), id), DataValue::Text("pending".into()), "one reload undo step");
                continue;
            }
            let expected = if change == "value" { "later" } else { "pending" };
            assert_eq!(edited_value(h.state(), id), DataValue::Text(expected.into()), "{change}");
            assert_eq!(h.state().session.undo_label(), before.as_deref(), "refusal adds no reload undo step: {change}");
            assert!(
                h.state().session.notices.iter().any(|n| n.text.contains("its data or pending edits changed; confirm Reload Data again")),
                "{change}"
            );
            if change == "value" {
                h.state_mut().run("edit.undo", Value::Null).unwrap();
                assert_eq!(edited_value(h.state(), id), DataValue::Text("pending".into()), "the later edit is still the latest undo step");
                h.state_mut().run("edit.redo", Value::Null).unwrap();
                assert_eq!(edited_value(h.state(), id), DataValue::Text("later".into()));
            }
            // The rejected request does not poison a new, explicit confirmation.
            confirm_reload(&mut h, id);
            landed(h.state_mut(), 60);
            assert_eq!(edited_value(h.state(), id), DataValue::Text("original".into()), "{change}");
            h.state_mut().run("edit.undo", Value::Null).unwrap();
            assert_eq!(edited_value(h.state(), id), DataValue::Text(expected.into()), "one retry reload undo step: {change}");
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn running_reload_cannot_discard_a_later_same_count_edit() {
        use rustgis_engine::data::Value as DataValue;
        let (_dir, mut h, id) = editable_reload_fixture();
        confirm_reload(&mut h, id);
        let slot = h.state().reads.job.as_ref().unwrap().slot.clone();
        let held = slot.lock().unwrap();
        h.state_mut().run("edit.setValue", json!({"layer": id, "row": 0, "field": "name", "value": "later"})).unwrap();
        let before = h.state().session.undo_label().map(str::to_string);
        drop(held);
        landed(h.state_mut(), 60);
        assert_eq!(edited_value(h.state(), id), DataValue::Text("later".into()));
        assert_eq!(h.state().session.undo_label(), before.as_deref());
        confirm_reload(&mut h, id);
        landed(h.state_mut(), 60);
        assert_eq!(edited_value(h.state(), id), DataValue::Text("original".into()));
    }

    #[test]
    fn reload_lands_once_and_cancel_retains_the_current_layer() {
        let (_dir, mut app, id, path) = reload_fixture();
        publish_point(&path, 9.0);
        let before = app.session.undo_label().map(str::to_string);
        assert!(app.run("ui.reloadData", json!({"layer": id})).unwrap().get("started").is_some());
        assert_eq!(source_x(&app, id), 1.0, "the worker cannot install into the live session");
        assert_eq!(app.session.undo_label(), before.as_deref());
        landed(&mut app, 60);
        assert_eq!(source_x(&app, id), 9.0);
        let fresh = app.session.map().layer(id).unwrap().load_all_counted().unwrap().0;
        assert_eq!(fresh.features[0].geometry, Some(rustgis_engine::data::Geometry::Point(rustgis_engine::data::Coord::new(9.0, 47.0))));
        app.run("edit.undo", Value::Null).unwrap();
        assert_eq!(source_x(&app, id), 1.0, "one undo restores the retained configuration");

        app.run("ui.reloadData", json!({"layer": id})).unwrap();
        app.reads.job.as_ref().unwrap().control.cancel();
        landed(&mut app, 60);
        assert_eq!(source_x(&app, id), 1.0);
        assert_eq!(app.session.undo_label(), before.as_deref());
    }

    #[test]
    fn queued_and_running_reload_cannot_target_a_repaired_source() {
        let (dir, mut app, id, path) = reload_fixture();
        publish_point(&path, 9.0);
        app.run("ui.reloadData", json!({"layer": id})).unwrap();
        assert_eq!(app.run("ui.reloadData", json!({"layer": id})).unwrap()["waiting"], 1);
        let replacement = dir.path().join("replacement.geojson");
        publish_point(&replacement, 5.0);
        // A direct command can repair while the UI has old reads queued; UI repairs now
        // join that queue themselves, so use the command API to exercise stale landing.
        app.session.run("layer.repairSource", &json!({"layer": id, "path": replacement})).unwrap();
        let before = app.session.undo_label().map(str::to_string);
        landed(&mut app, 60);
        assert_eq!(source_x(&app, id), 5.0);
        let fresh = app.session.map().layer(id).unwrap().load_all_counted().unwrap().0;
        assert_eq!(fresh.features[0].geometry, Some(rustgis_engine::data::Geometry::Point(rustgis_engine::data::Coord::new(5.0, 47.0))));
        assert_eq!(app.session.undo_label(), before.as_deref());
    }

    #[test]
    fn queued_load_all_after_reload_is_refused_until_requested_for_the_new_source() {
        let (_dir, mut app, id, path) = reload_fixture();
        publish_point(&path, 9.0);
        let original = app.session.map().layer(id).unwrap().source.clone().unwrap();
        app.run("ui.reloadData", json!({"layer": id})).unwrap();
        assert_eq!(run(&mut app, "layer.loadAll", json!({"layer": id})).unwrap()["waiting"], 1);
        landed(&mut app, 60);
        let layer = app.session.map().layer(id).unwrap();
        assert!(layer.is_streamed(), "the queued Load All cannot silently retarget the new source");
        let reloaded = layer.source.clone().unwrap();
        assert!(!reloaded.same_opening(&original));
        assert_eq!(source_x(&app, id), 9.0);
        assert_eq!(app.session.undo_label(), Some("Reload Data"), "the stale Load All creates no undo step");
        assert!(app.session.notices.iter().any(|n| n.text.contains("source changed")));
        run(&mut app, "layer.loadAll", json!({"layer": id})).unwrap();
        landed(&mut app, 60);
        let layer = app.session.map().layer(id).unwrap();
        assert!(!layer.is_streamed());
        assert_eq!(
            layer.dataset.features[0].geometry,
            Some(rustgis_engine::data::Geometry::Point(rustgis_engine::data::Coord::new(9.0, 47.0)))
        );
    }
    /// Direct layer conversions use the desktop worker too, and stale rows cannot be replaced.
    #[test]
    fn date_conversion_runs_off_frame_for_layers() {
        let mut app = RustGisApp::with_sample();
        app.run("edit.addField", json!({"layer":"Cities", "name":"opened", "type":"text"})).unwrap();
        app.run("edit.setValue", json!({"layer":"Cities", "row":0, "field":"opened", "value":"03/05/2026"})).unwrap();
        app.reads_off_frame = true;
        let p = json!({"field":"opened", "type":"date", "format":"%d/%m/%Y"});
        app.session.active_layer = app.session.map().layer_by_name("Cities").map(|l| l.id);
        let depth = app.session.undo_depth();
        app.ws.recording = Some(Vec::new());
        assert_eq!(app.run_by_user("edit.convertField", p.clone()).unwrap()["started"], "Convert Field");
        assert_eq!(app.session.undo_depth(), depth);
        app.session.active_layer = app.session.map().layers.iter().find(|l| l.name != "Cities").map(|l| l.id);
        landed(&mut app, 60);
        let ds = &app.session.map().layer_by_name("Cities").unwrap().dataset;
        assert_eq!(ds.value(0, ds.field_index("opened").unwrap()).to_string(), "2026-05-03");
        assert_eq!(app.session.undo_depth(), depth + 1);
        // Its step names the layer it was asked for, not the one active when it landed.
        let steps = app.ws.recording.take().unwrap();
        assert_eq!(steps.iter().find(|s| s.command == "edit.convertField").map(|s| s.params["layer"].clone()), Some(json!("Cities")));
        app.run("edit.undo", json!({})).unwrap();
        app.session.active_layer = app.session.map().layer_by_name("Cities").map(|l| l.id);
        app.run_by_user("edit.convertField", p).unwrap();
        app.session.run("edit.setValue", &json!({"layer":"Cities", "row":0, "field":"opened", "value":"new text"})).unwrap();
        let depth = app.session.undo_depth();
        landed(&mut app, 60);
        assert_eq!(app.session.undo_depth(), depth);
        assert!(app.session.notices.iter().any(|n| n.text.contains("layer changed")));
    }
    /// A queued preview cannot silently read rows edited after the user's format choice.
    #[test]
    fn date_preview_rejects_changed_queued_rows() {
        let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
        app.run("layer.addDataText", json!({"fileName":"dates.csv", "text":"opened\n03/05/2026\n04/06/2026\n"})).unwrap();
        app.run("edit.enable", json!({"on":true})).unwrap();
        app.reads_off_frame = true;
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocked.geojson");
        std::fs::write(&blocker, b"not GeoJSON").unwrap();
        assert_eq!(run(&mut app, "layer.addData", json!({"path":blocker})).unwrap()["started"], "Add Data");
        let dataset = app.session.project.tables[0].dataset.clone();
        assert_eq!(
            run_date_preview(&mut app, json!({"table":"dates", "field":"opened", "type":"date", "format":"%d/%m/%Y"}), dataset).unwrap()["waiting"],
            1
        );
        app.session.run("edit.setValue", &json!({"table":"dates", "row":1, "field":"opened", "value":"not a date"})).unwrap();
        let depth = app.session.undo_depth();
        landed(&mut app, 60);
        let dataset = &app.session.project.tables[0].dataset;
        assert_eq!(dataset.fields[0].ty, rustgis_engine::data::FieldType::Text);
        assert_eq!(dataset.value(1, 0).to_string(), "not a date");
        assert_eq!(app.session.undo_depth(), depth);
        assert!(app.session.notices.iter().any(|n| n.text.contains("changed after its date preview")));
    }
}
