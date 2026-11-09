//! Add Data's whole-file reads, layer reload, Load All Features and a model's tools on the desktop
//! run on a thread of their own (#1291 step 3), so a large raster, GeoJSON or table, a streamed
//! layer loaded whole, or a long model doesn't hold the window: the status bar shows the run under way,
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
    reload: Option<crate::reloaddata::Form>,
    /// Given what the command answered when it runs, or why it didn't (cancelled, or left).
    then: Option<Then>,
}

/// What a caller does once the command lands (ModelBuilder shows its run).
pub type Then = fn(&mut RustGisApp, Landed);

pub use crate::Landed;

/// A read under way: what it is for, its Cancel and progress, and what it read once done.
struct ReadJob {
    asked: Asked,
    /// What it was for went while it read: cancelled, it keeps its place until its thread ends
    /// (a reader that doesn't stop for Cancel still runs), so reads still go one at a time.
    gone: Option<Gone>,
    name: &'static str,
    control: Arc<Control>,
    slot: crate::Slot<Result<reads::Read, String>>,
}

/// Runs `command` (`layer.addData`, `layer.loadAll`, `layer.reloadData`, `model.run` or `script.run`) with `p`: its
/// read on a thread of its own when the desktop app makes it so ([`reads::read_ahead`]), after
/// any under way; here and now otherwise, as the command does. While one is under way or waiting
/// it answers that, not what the command adds, which comes as it lands.
pub fn run(app: &mut RustGisApp, command: &'static str, p: Value) -> Result<Value, String> {
    run_then(app, command, p, None)
}

/// [`run`], then `then` with what the command answered once it ran, here or as it lands, or why
/// it never ran.
pub fn run_then(app: &mut RustGisApp, command: &'static str, mut p: Value, then: Option<Then>) -> Result<Value, String> {
    if !app.reads_off_frame {
        let r = app.run(command, p);
        return answered(app, then, r);
    }
    let layer = matches!(command, "layer.loadAll" | "layer.reloadData")
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
    let ahead = match reads::read_ahead(&mut app.session, command, &p) {
        Ok(ahead) => ahead,
        Err(error) => {
            app.command_failed(command, &error, notice_mark);
            not_run(app, then, error.clone());
            return Err(error);
        }
    };
    // Load All asked again for a layer it is reading already.
    if command == "layer.loadAll"
        && let Some((instance, _)) = &layer
        // (One whose map or project went never loads it: a new request waits behind it.)
        && app.reads.job.iter().filter(|j| j.gone.is_none()).map(|j| &j.asked).chain(&app.reads.waiting)
            .any(|a| a.command == command && a.layer == Some(*instance))
    {
        return Ok(json!({"reading": instance}));
    }
    let what = match &layer {
        Some((_, name)) => name.clone(),
        // A model by its name (one drawn but not yet named has none: see `called`).
        None if command == "model.run" => p.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string(),
        None => p.get("path").and_then(Value::as_str).map(|path| file_name(path).to_string()).unwrap_or_default(),
    };
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
        reload,
        then,
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

/// What the command answered, `r`, given to `then` first.
fn answered(app: &mut RustGisApp, then: Option<Then>, r: Result<Value, String>) -> Result<Value, String> {
    if let Some(then) = then {
        then(app, Landed::Ran(r.clone()));
    }
    r
}

/// Why the command never ran, given to `then`.
fn not_run(app: &mut RustGisApp, then: Option<Then>, why: String) {
    if let Some(then) = then {
        then(app, Landed::NotRun(why));
    }
}

/// `asked` left for `why`: its `then` told now, and kept in `left` for the notice.
fn leave(app: &mut RustGisApp, left: &mut Vec<(String, Gone)>, asked: &Asked, why: Gone) {
    let what = called(asked.command, &asked.what).to_string();
    not_run(app, asked.then, crate::i18n::fmt("{what} was left: {why}", &[("what", &what), ("why", &why.to_string())]));
    left.push((what, why));
}

/// What a command asked for is called in a notice: a script, or an unnamed model, by the
/// command's name.
fn called<'a>(command: &str, what: &'a str) -> &'a str {
    match (what.is_empty(), command) {
        (false, _) => what,
        (true, "script.run") => tl_raw!("Run Script"),
        (true, _) => tl_raw!("Run Model"),
    }
}

/// A path's last part, as a notice names the file.
fn file_name(path: &str) -> &str {
    std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path)
}

/// Whether a read is under way or waiting.
fn busy(app: &RustGisApp) -> bool {
    app.reads.job.is_some() || !app.reads.waiting.is_empty()
}

/// Why a command asked for can't run now: what was open when it was asked isn't any more.
#[derive(Clone, Copy, PartialEq)]
enum Gone {
    Project,
    Map,
    Layer,
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
    let name = ahead.name;
    let control = Control::stoppable();
    if !ahead.reads() {
        let read = ahead.run(&control);
        return complete_here(app, asked, p, read);
    }
    let (ctl, slot) = (control.clone(), crate::Slot::default());
    let out = slot.clone();
    // Retain the admitted operation if the OS cannot start its worker: the synchronous
    // fallback must use that same owner, rather than dispatching a new native User command.
    let held = crate::Slot::default();
    *held.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ahead);
    let work = held.clone();
    let diagnostics = app.session.diagnostics.capture();
    let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
        let _diagnostics = diagnostics.enter();
        let ahead = work.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let read = ahead.ok_or_else(|| tl!("This read was already started").to_string()).and_then(|ahead| ahead.run(&ctl));
        *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(read);
    });
    if let Err(e) = spawned {
        log::warn!("{name} reads here: its thread didn't start: {e}");
        let read = held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| tl!("This read was already started").to_string())
            .and_then(|ahead| ahead.run(&control));
        return complete_here(app, asked, p, read);
    }
    app.reads.job = Some(ReadJob { asked, gone: None, name, control, slot });
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
            not_run(app, asked.then, error.clone());
            return Err(error);
        }
    };
    let r = app.run_read(asked.command, p, read);
    answered(app, asked.then, r)
}

/// Runs the command a read was for once it's done (or says why it wasn't), then starts the next
/// waiting: called every frame. `true` while one is under way or waiting.
pub fn land(app: &mut RustGisApp) -> bool {
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
                    // Cancelled (once it had read too): it adds nothing, and nor does what waits.
                    let said = if job.asked.what.is_empty() {
                        crate::i18n::fmt("{tool} was cancelled", &[("tool", tl_raw!(job.name))])
                    } else {
                        crate::i18n::fmt("{tool} of {what} was cancelled", &[("tool", tl_raw!(job.name)), ("what", &job.asked.what)])
                    };
                    app.session.notify(said.clone(), false);
                    not_run(app, job.asked.then, said);
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
                        not_run(app, asked.then, left_too.clone());
                    }
                } else {
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
    for why in [Gone::Project, Gone::Map, Gone::Layer, Gone::Source, Gone::Data] {
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
pub fn stop(app: &mut RustGisApp, wait: std::time::Duration) {
    app.reads.waiting.clear();
    let Some(job) = app.reads.job.take() else { return };
    job.control.cancel();
    let end = std::time::Instant::now() + wait;
    while job.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_none() && std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(10));
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Lands every read under way and waiting, failing past `secs`.
    fn landed(app: &mut RustGisApp, secs: u64) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        while land(app) {
            assert!(std::time::Instant::now() < end, "the reads never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
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
        app.run("layer.repairSource", json!({"layer": id, "path": replacement})).unwrap();
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
}
