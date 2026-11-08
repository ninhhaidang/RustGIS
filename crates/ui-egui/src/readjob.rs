//! Add Data's whole-file reads, Load All Features and a model's tools on the desktop run on a
//! thread of their own (#1291 step 3), so a large raster, GeoJSON or table, a streamed layer
//! loaded whole, or a long model doesn't hold the window: the status bar shows the run under way,
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

/// A command asked for, with where it was asked: the project and map then open and, for Load All,
/// the layer. Another project or map open by the time it runs, or the layer gone, leaves it.
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

/// Runs `command` (`layer.addData`, `layer.loadAll`, `model.run` or `script.run`) with `p`: its
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
    let layer = (command == "layer.loadAll")
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
    if let Some((instance, _)) = &layer
        // (One whose map or project went never loads it: a new request waits behind it.)
        && app.reads.job.iter().filter(|j| j.gone.is_none()).map(|j| &j.asked).chain(&app.reads.waiting).any(|a| a.layer == Some(*instance))
    {
        return Ok(json!({"reading": instance}));
    }
    let what = match &layer {
        Some((_, name)) => name.clone(),
        // A model by its name (one drawn but not yet named has none: see `called`).
        None if command == "model.run" => p.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string(),
        None => p.get("path").and_then(Value::as_str).map(|path| file_name(path).to_string()).unwrap_or_default(),
    };
    let asked = Asked {
        command,
        params: p,
        ahead: Some(ahead),
        what,
        opened: app.session.opened,
        map: app.session.map().instance_id(),
        layer: layer.map(|(instance, _)| instance),
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
}

impl std::fmt::Display for Gone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            // Filled into a notice's template: in logical order (`tl_raw!`).
            Gone::Project => tl_raw!("another project was opened"),
            Gone::Map => tl_raw!("the map it was asked for isn't open"),
            Gone::Layer => tl_raw!("its layer was removed"),
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
    for why in [Gone::Project, Gone::Map, Gone::Layer] {
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
}
