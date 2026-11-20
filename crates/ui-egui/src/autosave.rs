//! Autosave and crash recovery (#873). While the project has unsaved changes, a recovery copy of
//! it is kept through [`Services::recovery`](crate::Services). Desktop writes it at intervals;
//! phones group foreground edits into checkpoints and keep a final copy on exit. Saving or
//! starting another project clears this run's copy. A copy left at start is offered back
//! ([`offer`]).
//!
//! Native apps keep the copy in a folder ([`FolderStore`]); a shell without a store (the
//! browser, for now) has no autosave.

use egui::vec2;
use rustgis_engine::Project;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use web_time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::RustGisApp;
use crate::theme::{Tokens, semibold};

/// The preference the interval is kept under, in minutes (`0` is off).
pub(crate) const MINUTES_PREF: &str = "autosave_minutes";
/// The interval until the user picks one.
pub(crate) const DEFAULT_MINUTES: u32 = 5;
/// The longest interval `ui.autosave` takes.
const MAX_MINUTES: u64 = 60;
/// Minimum time between foreground phone checkpoint submissions.
const PHONE_INTERVAL: Duration = Duration::from_secs(2);
/// Wait longer before retrying a failed phone copy of the same state.
const PHONE_RETRY_INTERVAL: Duration = Duration::from_secs(30);
/// Desktop can wait for the recovery writer during its clean exit.
const DESKTOP_FLUSH_WAIT: Duration = Duration::from_secs(30);
/// Leave time for the rest of teardown within Android's five-second input timeout.
const PHONE_FLUSH_WAIT: Duration = Duration::from_secs(2);
/// The intervals Settings › Autosave offers (`0` is off).
pub(crate) const CHOICES: [u32; 7] = [0, 1, 2, 5, 10, 15, 30];

/// What a recovery copy holds beside the project: what the recovery dialog says about it and
/// where saving the recovered work writes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovery {
    /// The project's name.
    #[serde(default)]
    pub name: String,
    /// The file the project was opened from or saved to, if any.
    #[serde(default)]
    pub path: Option<String>,
    /// The package this project was opened from, independently of the path Save uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// When the copy was made, in seconds since 1970 (UTC).
    #[serde(default)]
    pub time: u64,
    /// Kept as the app was quit without its question about unsaved changes answered (macOS's
    /// Quit, which closes without asking the window), not left by a run that didn't close.
    /// Copies written before it was known read as `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub quit: bool,
    /// Counted output notices shown when this copy is recovered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
}

/// Where a shell keeps the recovery copy. Writing and clearing may happen in the background, but
/// in the order they were asked for: a copy cleared after a save is never written again by an
/// earlier, slower write.
pub trait RecoveryStore {
    /// Replaces this run's copy with `project` (a copy of the open one) and `about` it.
    fn write(&self, project: Project, about: Recovery);
    /// Checkpoints a completed edit. An async store can replace a checkpoint still waiting
    /// for the worker with this newer one, which also contains the earlier edits.
    fn checkpoint(&self, project: Project, about: Recovery) {
        self.write(project, about);
    }
    /// Removes this run's copy (the work was saved, or replaced, or the app closes cleanly).
    fn clear(&self);
    /// Waits for queued work to finish with the desktop's clean-exit budget.
    fn flush(&self) {
        self.flush_for(DESKTOP_FLUSH_WAIT);
    }
    /// Waits for queued work to finish, up to the caller's timeout.
    fn flush_for(&self, timeout: Duration);
    /// The copy an earlier run left that is offered now: the newest of those waiting to be
    /// recovered or discarded (runs that crashed again after Later each leave one).
    fn pending(&self) -> Option<Recovery>;
    /// How many copies are waiting, the offered one included.
    fn waiting(&self) -> usize;
    /// The project text of the offered copy.
    fn read_pending(&self) -> Result<String, String>;
    /// Forgets the offered copy; the next one waiting is offered.
    fn discard_pending(&self);
    /// Makes the offered copy this run's copy, once it was recovered: it is kept, unchanged,
    /// until the recovered work is saved or changes again.
    fn adopt_pending(&self);
    /// Why the last copy couldn't be written, once.
    fn take_error(&self) -> Option<String>;
    /// Gives the store a way to wake the interface when a copy fails in the background, so the
    /// failure is told on an idle window too. A store that fails only when asked needs none.
    fn set_wake(&self, _wake: Box<dyn Fn() + Send + Sync>) {}
}

/// The app's autosave state.
pub struct AutosaveState {
    /// Minutes between copies; `0` is off.
    pub(crate) minutes: u32,
    /// Use the phone's foreground checkpoint policy in place of the desktop interval.
    pub(crate) phone: bool,
    /// The session revision the last copy was made at, while one is kept.
    written: Option<u64>,
    /// The last copy failed in the background: due again at the next interval, unchanged or not.
    /// Kept apart from `written`, so a save still removes the earlier copy the failure left.
    failed: bool,
    /// When the last copy was submitted (or adopted on desktop).
    last: Option<Instant>,
    /// The copy an earlier run left, while the user hasn't recovered or discarded it.
    offer: Option<Recovery>,
    /// How many more copies wait behind the offered one.
    more: usize,
    /// Whether the offered copy's file changed on disk after the copy was made.
    changed_since: bool,
    /// The dialog was put off until the next start (Later, or other copies after a recovery).
    later: bool,
    /// Why recovering the offered copy failed, shown in the dialog.
    offer_error: Option<String>,
    /// The last write error told, so a failing folder isn't reported at every interval.
    told: Option<String>,
    /// The store was given the interface's wake.
    wake_set: bool,
}

impl Default for AutosaveState {
    /// Every few minutes ([`DEFAULT_MINUTES`]), nothing written yet and nothing offered.
    fn default() -> Self {
        AutosaveState {
            minutes: DEFAULT_MINUTES,
            phone: false,
            written: None,
            failed: false,
            last: None,
            offer: None,
            more: 0,
            changed_since: false,
            later: false,
            offer_error: None,
            told: None,
            wake_set: false,
        }
    }
}

/// Seconds since 1970, now.
fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The open project's description for its recovery copy.
/// A file name that isn't Unicode (legal on Unix) can't be kept exactly in the copy's description:
/// the project is described as untitled then, so Save asks where to write rather than writing to
/// a lossy spelling of the name. A package path that cannot be stored is reported when
/// recovering the copy.
fn about(app: &RustGisApp) -> Recovery {
    let path = app.session.path.as_ref().and_then(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()).to_str().map(str::to_string));
    let opened_package = app.session.opened_package();
    let package = opened_package.and_then(std::path::Path::to_str).map(str::to_string);
    let notices = if opened_package.is_some() && package.is_none() {
        vec![tl!("The recovery copy could not keep the opened package's path because it is not UTF-8.").to_string()]
    } else {
        Vec::new()
    };
    Recovery { name: app.session.project.name.clone(), path, package, time: now_secs(), quit: false, notices }
}

/// Reads the interval preference and the copy an earlier run left (from [`RustGisApp::with_services`]).
pub(crate) fn load(app: &mut RustGisApp) {
    let minutes = app.services.load_pref.as_ref().and_then(|load| load(MINUTES_PREF)).and_then(|v| v.trim().parse::<u32>().ok());
    // A stored value out of range (edited by hand) means the default.
    app.autosave.minutes = minutes.filter(|m| u64::from(*m) <= MAX_MINUTES).unwrap_or(DEFAULT_MINUTES);
    next_offer(app);
}

/// How many copies wait to be offered, the one offered now included.
fn waiting(app: &RustGisApp) -> usize {
    app.autosave.offer.as_ref().map_or(0, |_| app.autosave.more + 1)
}

/// Offers the next copy waiting in the store, if any.
pub(crate) fn next_offer(app: &mut RustGisApp) {
    let store = app.services.recovery.as_ref();
    let pending = store.and_then(|s| s.pending());
    app.autosave.more = store.map_or(0, |s| s.waiting()).saturating_sub(1);
    app.autosave.changed_since = pending.as_ref().is_some_and(changed_since);
    app.autosave.offer = pending;
    app.autosave.offer_error = None;
}

/// Whether the copy's file was changed on disk after the copy was made.
fn changed_since(r: &Recovery) -> bool {
    let Some(path) = &r.path else { return false };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        modified.and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).is_some_and(|d| d.as_secs() > r.time)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        false
    }
}

/// Keeps the recovery copy in step with the project. Desktop uses the configured interval.
/// Phones submit the first edit immediately and group later edits within the submission interval.
pub(crate) fn tick(app: &mut RustGisApp, ctx: &egui::Context) {
    tick_at(app, Some(ctx), Instant::now());
}

/// Updates the copy at `now`. Without a frame context the phone is exiting, so skip the delay.
fn tick_at(app: &mut RustGisApp, ctx: Option<&egui::Context>, now: Instant) {
    let Some(store) = &app.services.recovery else { return };
    if !app.autosave.wake_set
        && let Some(ctx) = ctx
    {
        let ctx = ctx.clone();
        store.set_wake(Box::new(move || ctx.request_repaint()));
        app.autosave.wake_set = true;
    }
    if let Some(e) = store.take_error() {
        // The work is due again, at the next interval.
        app.autosave.failed = true;
        if app.autosave.told.as_ref() != Some(&e) {
            app.autosave.told = Some(e.clone());
            let text = crate::i18n::fmt("Autosave couldn't keep a recovery copy: {error}", &[("error", &e)]);
            app.session.notify(text, true);
        }
    }
    let phone = app.autosave.phone;
    if (!phone && app.autosave.minutes == 0) || !app.session.dirty() {
        if app.autosave.written.take().is_some() {
            store.clear();
        }
        app.autosave.failed = false;
        return;
    }
    // View commands advance revision too. On the phone only new undoable project states
    // (including Undo and Redo) need another copy.
    let state = if phone { app.session.state() } else { app.session.revision };
    let changed = app.autosave.written != Some(state);
    if !changed && !app.autosave.failed {
        return;
    }
    if let Some(ctx) = ctx {
        let interval = match (phone, changed) {
            (true, true) => PHONE_INTERVAL,
            (true, false) => PHONE_RETRY_INTERVAL,
            (false, _) => Duration::from_secs(u64::from(app.autosave.minutes) * 60),
        };
        let remaining = app.autosave.last.map_or(Duration::ZERO, |last| (last + interval).saturating_duration_since(now));
        if !remaining.is_zero() {
            ctx.request_repaint_after(remaining);
            return;
        }
    }
    // Cheap: layers' features (`Features`), raster pixels and embedded bytes are `Arc`s shared
    // with the copy, as they are with the undo snapshot every edit already takes; only the
    // project's own settings are copied here. Serializing and writing happen on the store's thread.
    if phone {
        store.checkpoint(app.session.project.clone(), about(app));
    } else {
        store.write(app.session.project.clone(), about(app));
    }
    app.autosave.written = Some(state);
    app.autosave.failed = false;
    app.autosave.last = Some(now);
}

/// Submits any remaining phone edit immediately. If the wait expires, the latest copy may still
/// be queued or writing. The last complete copy, if any, stays available for the next launch.
pub(crate) fn finish(app: &mut RustGisApp) {
    if app.autosave.phone {
        tick_at(app, None, Instant::now());
    }
    if let Some(store) = &app.services.recovery {
        store.flush_for(PHONE_FLUSH_WAIT);
    }
}

/// A clean exit: this run's copy goes, and what was asked of the store is done before the
/// process ends. The copy an earlier run left stays until it is recovered or discarded.
///
/// Unless the work in it would be lost unasked: the app was quit with unsaved changes and the
/// save prompt never answered (macOS's Quit, which ends the app without asking the window to
/// close). Then a last copy is written and kept, and the next start offers it back. With
/// autosave off there is no copy to keep.
pub(crate) fn end(app: &mut RustGisApp) {
    if let Some(store) = &app.services.recovery {
        if app.autosave.minutes > 0 && crate::saveprompt::unanswered(app) {
            store.write(app.session.project.clone(), Recovery { quit: true, ..about(app) });
        } else {
            store.clear();
        }
        store.flush();
    }
    app.autosave.written = None;
}

/// `ui.autosave`: sets the interval (`minutes`, 0 to 60; `0` is off) and says what autosave
/// does: the interval, and the copy waiting to be recovered.
pub(crate) fn command(app: &mut RustGisApp, p: &Value) -> Result<Value, String> {
    if let Some(m) = p.get("minutes") {
        if app.autosave.phone {
            return Err("the phone keeps a recovery copy after edits".into());
        }
        if app.services.recovery.is_none() {
            return Err("autosave needs the desktop app".into());
        }
        let m = m.as_u64().filter(|m| *m <= MAX_MINUTES).ok_or("minutes is a whole number from 0 (off) to 60")?;
        app.autosave.minutes = u32::try_from(m).unwrap_or(DEFAULT_MINUTES);
        if let Some(save) = &app.services.save_pref {
            save(MINUTES_PREF, &m.to_string());
        }
    }
    Ok(json!({
        "available": app.services.recovery.is_some(),
        "minutes": app.autosave.minutes,
        "pending": app.autosave.offer.as_ref().map(|r| json!({"name": r.name, "path": r.path, "time": r.time})),
    }))
}

/// `ui.recover`: opens the copy an earlier run left as the project, unsaved, with its original
/// file (so Save writes there); `{"discard": true}` deletes it instead.
pub(crate) fn recover(app: &mut RustGisApp, p: &Value) -> Result<Value, String> {
    let discard = match p.get("discard") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("discard is true or false".into()),
    };
    let none = "there is no unsaved work to recover";
    let (Some(store), Some(offered)) = (&app.services.recovery, app.autosave.offer.clone()) else { return Err(none.into()) };
    if discard {
        store.discard_pending();
        next_offer(app);
        return Ok(json!({"discarded": true, "waiting": waiting(app)}));
    }
    // Opening it replaces the open project: never one with work of its own not saved (after
    // Later), which it would lose with its recovery copy.
    if app.session.dirty() {
        return Err("the open project has unsaved changes: save them before recovering other work".into());
    }
    let text = store.read_pending()?;
    app.run("project.openText", json!({"text": text, "package": offered.package.is_some()}))?;
    app.session.restore_recovery_paths(offered.path.as_ref().map(std::path::PathBuf::from), offered.package.map(std::path::PathBuf::from));
    // The copy becomes this run's as it is: a crash now loses nothing, and nothing is written.
    if let Some(store) = &app.services.recovery {
        store.adopt_pending();
    }
    app.autosave.written = Some(if app.autosave.phone { app.session.state() } else { app.session.revision });
    // The adopted copy is already on disk. On the phone, only submissions throttle new edits.
    if !app.autosave.phone {
        app.autosave.last = Some(Instant::now());
    }
    next_offer(app);
    let text = crate::i18n::fmt("Recovered unsaved work in {name}. Save to keep it.", &[("name", &app.session.project.name)]);
    app.session.notify(text, false);
    for notice in offered.notices {
        app.session.notify(notice, false);
    }
    // Other copies wait for the next start: recovering one now would replace this work.
    let waiting = waiting(app);
    if waiting > 0 {
        app.autosave.later = true;
        let text =
            crate::i18n::fmt("Other copies of unsaved work will be offered at the next start ({n}).", &[("n", &waiting.to_string())]);
        app.session.notify(text, false);
    }
    Ok(json!({"name": app.session.project.name, "path": offered.path, "waiting": waiting}))
}

/// The dialog's second line: what was kept, and how long ago (`then`, seconds since 1970).
/// Whole sentences, one per unit, so each translates as a sentence; only data is filled in.
fn kept_line(name: &str, then: u64) -> String {
    let secs = now_secs().saturating_sub(then);
    let (template, n) = match secs {
        0..60 => ("A copy of {name} with unsaved changes was kept less than a minute ago.", 0),
        60..120 => ("A copy of {name} with unsaved changes was kept a minute ago.", 1),
        120..7_200 => ("A copy of {name} with unsaved changes was kept {n} minutes ago.", secs / 60),
        7_200..172_800 => ("A copy of {name} with unsaved changes was kept {n} hours ago.", secs / 3_600),
        _ => ("A copy of {name} with unsaved changes was kept {n} days ago.", secs / 86_400),
    };
    crate::i18n::fmt(template, &[("name", name), ("n", &n.to_string())])
}

/// The recovery dialog, at start while a copy an earlier run left is waiting: Recover, Discard,
/// or Later (it is offered again at the next start).
pub(crate) fn offer(app: &mut RustGisApp, ctx: &egui::Context) {
    if app.autosave.later {
        return;
    }
    let Some(r) = app.autosave.offer.clone() else { return };
    let t = Tokens::get(ctx);
    let name = if r.name.trim().is_empty() { tl_raw!("Untitled").to_string() } else { r.name.clone() };
    let file = r.path.as_deref().map(|p| std::path::Path::new(p).file_name().map_or(p.to_string(), |n| n.to_string_lossy().into_owned()));
    let mut decision = None;
    let frame = egui::Frame::popup(&ctx.global_style());
    let width = crate::widgets::dialog_width(ctx, 420.0, frame);
    let height = (ctx.content_rect().height() - frame.total_margin().sum().y - 16.0).max(0.0);
    let contents = |ui: &mut egui::Ui| {
        ui.label(egui::RichText::new(tl!("Recover unsaved work?")).font(semibold(15.0)));
        ui.add_space(4.0);
        // On a phone the OS can stop a backgrounded app as part of normal use.
        if !app.autosave.phone {
            ui.label(if r.quit {
                tl!("RustGIS was quit last time without saving your changes.")
            } else {
                tl!("RustGIS didn't close properly last time.")
            });
        }
        ui.label(kept_line(&name, r.time));
        ui.add_space(4.0);
        match &file {
            _ if app.autosave.phone => {
                ui.label(tl!("It opens unsaved. Save to keep it."));
            }
            Some(f) => {
                ui.label(crate::i18n::fmt("It opens unsaved; saving replaces {file}.", &[("file", f)]));
                if app.autosave.changed_since {
                    ui.label(
                        egui::RichText::new(crate::i18n::fmt("{file} has changed since this copy was made.", &[("file", f)])).color(t.red),
                    );
                }
            }
            None => {
                ui.label(tl!("It was never saved: it opens unsaved, and Save asks where to write it."));
            }
        }
        if app.autosave.more > 0 {
            ui.label(crate::i18n::fmt("Older copies waiting: {n}.", &[("n", &app.autosave.more.to_string())]));
        }
        // Recovering replaces the open project: not one with unsaved work of its own (an edit
        // made before answering). Recover comes back once that work is saved.
        let blocked = app.session.dirty();
        if blocked {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tl!("The open project has unsaved changes: save them, then recover this copy.")).color(t.text_dim),
            );
        }
        if let Some(e) = &app.autosave.offer_error {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(crate::i18n::fmt("It didn't open: {error}", &[("error", e)])).color(t.red));
        }
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min).with_main_wrap(true), |ui| {
            // A copy that didn't open isn't offered again; Discard and Later remain.
            if app.autosave.offer_error.is_none()
                && !blocked
                && crate::widgets::dialog_button(ui, tl!("Recover"), Some(t.accent), vec2(84.0, 28.0)).clicked()
            {
                decision = Some("recover");
            }
            if crate::widgets::dialog_button(ui, tl!("Discard"), None, vec2(84.0, 28.0)).clicked() {
                decision = Some("discard");
            }
            if crate::widgets::dialog_button(ui, tl!("Later"), None, vec2(84.0, 28.0)).clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Escape))
            {
                decision = Some("later");
            }
        });
    };
    egui::Modal::new(egui::Id::unique("autosave_recover")).frame(frame).show(ctx, |ui| {
        ui.set_width(width);
        ui.set_max_height(height);
        egui::ScrollArea::vertical().min_scrolled_height(0.0).auto_shrink([false, true]).show(ui, contents);
    });
    match decision {
        Some("recover") => {
            if let Err(e) = app.run("ui.recover", Value::Null) {
                app.autosave.offer_error = Some(e);
            }
        }
        Some("discard") => {
            let _ = app.run("ui.recover", json!({"discard": true}));
        }
        Some(_) => app.autosave.later = true,
        None => {}
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use folder::FolderStore;

/// The desktop's store: one file for this run's copy and one for each copy earlier runs left,
/// in a folder of their own, written by a background thread.
#[cfg(not(target_arch = "wasm32"))]
mod folder {
    use std::io::{BufRead, Read};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use rustgis_engine::Project;

    use super::{Recovery, RecoveryStore};

    /// This run's copy.
    const LIVE: &str = "autosave.recovery";
    /// Copies earlier runs left are `recovered-<n>.recovery`.
    const PENDING_PREFIX: &str = "recovered-";
    const EXTENSION: &str = ".recovery";
    /// The most copies kept waiting: the oldest beyond these go.
    const MAX_WAITING: usize = 5;
    /// The first line of a copy, describing it, is at most this long.
    const MAX_HEADER: u64 = 64 * 1024;

    type Snapshot = (Box<Project>, Recovery);
    type Pending = Arc<Mutex<Option<Snapshot>>>;

    /// What the background thread is asked to do, in order.
    enum Job {
        Write(Box<Project>, Recovery),
        Checkpoint(Pending),
        Clear,
        Remove(PathBuf),
        Adopt(PathBuf),
        Flush(Sender<()>),
    }

    /// A phone can update the latest queued checkpoint, until the worker takes it or another
    /// operation is queued. Save, discard, adoption and flush keep their FIFO order.
    struct Queue {
        jobs: Sender<Job>,
        latest: Option<Pending>,
    }

    impl Queue {
        /// Ends coalescing across another operation. A save invalidates a checkpoint the
        /// worker hasn't started yet.
        fn send(&mut self, job: Job) {
            if let Some(slot) = self.latest.take()
                && matches!(job, Job::Clear)
            {
                slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            }
            self.enqueue(job);
        }

        /// Holds just the latest waiting checkpoint. The worker can drain it even when the
        /// window stops drawing because the app went to the background.
        fn checkpoint(&mut self, snapshot: Snapshot) {
            if let Some(slot) = &self.latest {
                let mut next = slot.lock().unwrap_or_else(|e| e.into_inner());
                if next.is_some() {
                    *next = Some(snapshot);
                    return;
                }
            }
            let slot = Arc::new(Mutex::new(Some(snapshot)));
            self.latest = Some(slot.clone());
            self.enqueue(Job::Checkpoint(slot));
        }

        /// A stopped worker cannot keep the copy.
        fn enqueue(&self, job: Job) {
            if self.jobs.send(job).is_err() {
                log::warn!("autosave stopped: its thread is gone");
            }
        }
    }

    /// A recovery folder. A copy is one file: a line of JSON describing it ([`Recovery`]), then the
    /// project's saved text. One file written atomically means the description always belongs to
    /// the project after it, whenever the app stops.
    pub struct FolderStore {
        jobs: Mutex<Queue>,
        /// The copies earlier runs left, newest first.
        waiting: Mutex<Vec<(PathBuf, Recovery)>>,
        error: Arc<Mutex<Option<String>>>,
        /// Called by the background thread after a failure: the interface's repaint.
        wake: Wake,
    }

    /// The interface's wake, shared with the background thread; none until the app sets it.
    type Wake = Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>;

    impl FolderStore {
        /// Takes `dir` as the recovery folder for this run, which must be the only one using it
        /// (the desktop app holds the run marker). A copy the last run left there joins those
        /// waiting to be offered; none is ever replaced by another, and only the oldest beyond
        /// [`MAX_WAITING`] are removed.
        pub fn open(dir: &Path) -> Result<FolderStore, String> {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            // The copies hold the project's data: this user's only.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
                    log::warn!("autosave: could not make {} private: {e}", dir.display());
                }
            }
            let live = dir.join(LIVE);
            if live.is_file() {
                let set_aside = unused_pending_name(dir);
                std::fs::rename(&live, &set_aside).map_err(|e| format!("{}: {e}", live.display()))?;
            }
            let mut waiting: Vec<(PathBuf, Recovery)> = std::fs::read_dir(dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|p| p.is_file() && is_pending(p))
                .map(|p| {
                    let about = describe(&p);
                    (p, about)
                })
                .collect();
            // Newest first; copies of the same second by name, the later set aside first.
            waiting.sort_by(|a, b| b.1.time.cmp(&a.1.time).then_with(|| b.0.cmp(&a.0)));
            for (old, _) in waiting.drain(MAX_WAITING.min(waiting.len())..) {
                log::warn!("autosave: removing {}, the oldest of more than {MAX_WAITING} copies", old.display());
                remove(&old);
            }
            let (jobs, rx) = channel();
            let error: Arc<Mutex<Option<String>>> = Arc::default();
            let wake: Wake = Arc::default();
            let (worker_dir, worker_error, worker_wake) = (dir.to_path_buf(), error.clone(), wake.clone());
            std::thread::Builder::new()
                .name("autosave".into())
                .spawn(move || work(&worker_dir, &rx, &worker_error, &worker_wake))
                .map_err(|e| format!("could not start autosave: {e}"))?;
            Ok(FolderStore { jobs: Mutex::new(Queue { jobs, latest: None }), waiting: Mutex::new(waiting), error, wake })
        }

        /// Queues the request on the store's worker.
        fn send(&self, job: Job) {
            self.jobs.lock().unwrap_or_else(|e| e.into_inner()).send(job);
        }

        /// The copies waiting, locked.
        fn list(&self) -> std::sync::MutexGuard<'_, Vec<(PathBuf, Recovery)>> {
            self.waiting.lock().unwrap_or_else(|e| e.into_inner())
        }

        /// Takes the offered copy off the list, for `job` to do with its file.
        fn take_offered(&self, job: fn(PathBuf) -> Job) {
            let mut waiting = self.list();
            if waiting.is_empty() {
                return;
            }
            let (path, _) = waiting.remove(0);
            drop(waiting);
            self.send(job(path));
        }
    }

    /// Whether `path` is a copy an earlier run left.
    fn is_pending(path: &Path) -> bool {
        path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(PENDING_PREFIX) && n.ends_with(EXTENSION))
    }

    /// A name for a copy set aside that no other copy has: `recovered-<n>.recovery`, one past the
    /// highest number there.
    fn unused_pending_name(dir: &Path) -> PathBuf {
        let highest = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                name.strip_prefix(PENDING_PREFIX)?.strip_suffix(EXTENSION)?.parse::<u64>().ok()
            })
            .max();
        let n = highest.map_or(1, |h| h.saturating_add(1));
        dir.join(format!("{PENDING_PREFIX}{n}{EXTENSION}"))
    }

    /// A copy's description, from its first line; a copy whose line doesn't read (cut short,
    /// garbled) is still offered, as an untitled project of its file's time.
    fn describe(path: &Path) -> Recovery {
        let mut line = String::new();
        let read = std::fs::File::open(path).and_then(|f| std::io::BufReader::new(f.take(MAX_HEADER)).read_line(&mut line));
        if read.is_ok()
            && let Ok(r) = serde_json::from_str::<Recovery>(line.trim_end())
        {
            return r;
        }
        let time = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        Recovery { time, ..Recovery::default() }
    }

    /// How the background thread writes a copy (a test can make one fail).
    type WriteCopy = fn(&Path, Project, &Recovery) -> Result<(), String>;

    /// The background thread: does what it is asked in order, writing copies with [`write`].
    fn work(dir: &Path, rx: &Receiver<Job>, error: &Mutex<Option<String>>, wake: &Wake) {
        work_with(dir, rx, error, wake, write);
    }

    /// [`work`] with its writer. Each job runs guarded: a panic is told as a failure and the
    /// thread goes on. A write followed by another is skipped, as only the latest copy matters,
    /// but kept until the next one is done: if that one fails or panics (a project grown past the
    /// size a copy may have), the skipped one is written instead, before any later job, so the
    /// folder holds the newest copy that could be written.
    fn work_with(dir: &Path, rx: &Receiver<Job>, error: &Mutex<Option<String>>, wake: &Wake, write: WriteCopy) {
        let fail = |e: String| {
            log::warn!("autosave: {e}");
            *error.lock().unwrap_or_else(|p| p.into_inner()) = Some(e);
            if let Some(wake) = wake.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                wake();
            }
        };
        // A copy whose writing panics fails like any other, so the skipped one still stands in.
        let write = |project: Project, about: &Recovery| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| write(dir, project, about)))
                .unwrap_or_else(|_| Err("writing the copy failed unexpectedly".into()))
        };
        let mut next = rx.recv().ok();
        let mut skipped: Option<(Box<Project>, Recovery)> = None;
        // Recovered copies that couldn't become this run's copy: removed once a newer copy is
        // written or the copy is cleared (until then, a crash leaves them to be offered again).
        let mut leftover: Vec<PathBuf> = Vec::new();
        while let Some(job) = next.take() {
            if let Job::Write(..) = job
                && let Ok(later) = rx.try_recv()
            {
                match later {
                    // Removed as soon as written (a save, a clean exit): neither copy is made, so
                    // an exit doesn't wait for one.
                    Job::Clear => {
                        skipped = None;
                        next = Some(later);
                        continue;
                    }
                    // One copy is held back at a time: with one held, this write is done (falling
                    // back to the held one) before the next, so a viable older copy is never dropped.
                    Job::Write(..) if skipped.is_none() => {
                        if let Job::Write(project, about) = job {
                            skipped = Some((project, about));
                        }
                        next = Some(later);
                        continue;
                    }
                    other => next = Some(other),
                }
            }
            // A panic in a job (serializing a hostile project, say) is told like a failure and the
            // thread goes on: autosave must not stop without a word (see `Session::run`).
            let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
                Job::Write(project, about) => {
                    let earlier = skipped.take();
                    let written = match write(*project, &about) {
                        Ok(()) => true,
                        Err(e) => {
                            let fallback = earlier.map(|(project, about)| write(*project, &about));
                            if let Some(Err(again)) = &fallback {
                                log::warn!("autosave: the copy before it failed too: {again}");
                            }
                            fail(e);
                            matches!(fallback, Some(Ok(())))
                        }
                    };
                    if written {
                        leftover.drain(..).for_each(|p| remove(&p));
                    }
                }
                Job::Checkpoint(slot) => {
                    let snapshot = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
                    if let Some((project, about)) = snapshot {
                        match write(*project, &about) {
                            Ok(()) => leftover.drain(..).for_each(|p| remove(&p)),
                            Err(e) => fail(e),
                        }
                    }
                }
                Job::Clear => {
                    remove(&dir.join(LIVE));
                    leftover.drain(..).for_each(|p| remove(&p));
                }
                Job::Remove(path) => remove(&path),
                Job::Adopt(path) => {
                    if let Err(e) = std::fs::rename(&path, dir.join(LIVE)) {
                        leftover.push(path);
                        fail(format!("could not keep the recovered copy: {e}"));
                    }
                }
                Job::Flush(done) => {
                    let _ = done.send(());
                }
            }));
            if done.is_err() {
                fail("autosave failed unexpectedly; it goes on with the next copy".into());
            }
            if next.is_none() {
                next = rx.recv().ok();
            }
        }
    }

    /// Writes this run's copy: the description line, then the project's text, in one atomic
    /// write, so a stop part way leaves the previous copy whole.
    fn write(dir: &Path, project: Project, about: &Recovery) -> Result<(), String> {
        let (text, report) = project.recovery_text_with_report()?;
        let mut about = about.clone();
        about.notices.extend(report.notices());
        let header = serde_json::to_string(&about).map_err(|e| e.to_string())?;
        use std::io::Write as _;
        rustgis_engine::data::io::replace_atomic_with(&dir.join(LIVE), |out| -> std::io::Result<()> {
            writeln!(out, "{header}")?;
            out.write_all(text.as_bytes())
        })
        .map_err(|e| e.to_string())
    }

    /// Removes a copy; one already gone is no error.
    fn remove(path: &Path) {
        if let Err(e) = std::fs::remove_file(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("autosave: could not remove {}: {e}", path.display());
        }
    }

    impl RecoveryStore for FolderStore {
        /// Serialized and written by the background thread; a newer copy queued behind it wins.
        fn write(&self, project: Project, about: Recovery) {
            self.send(Job::Write(Box::new(project), about));
        }

        /// Replaces the checkpoint waiting behind any current one, avoiding full rewrites
        /// of superseded edits and bounding the number of snapshots held.
        fn checkpoint(&self, project: Project, about: Recovery) {
            self.jobs.lock().unwrap_or_else(|e| e.into_inner()).checkpoint((Box::new(project), about));
        }

        /// Done after any copy queued before it, so a slow write can't bring the copy back.
        fn clear(&self) {
            self.send(Job::Clear);
        }

        /// Waits for queued work up to the caller's deadline. If the process ends first, the last
        /// complete copy still on disk is offered at the next start.
        fn flush_for(&self, timeout: Duration) {
            let (done, wait) = channel();
            self.send(Job::Flush(done));
            if wait.recv_timeout(timeout).is_err() {
                log::warn!("autosave: the recovery writer did not finish flushing at exit");
            }
        }

        /// The newest waiting, as [`FolderStore::open`] found them.
        fn pending(&self) -> Option<Recovery> {
            self.list().first().map(|(_, about)| about.clone())
        }

        /// As many as [`FolderStore::open`] found, less those recovered or discarded since.
        fn waiting(&self) -> usize {
            self.list().len()
        }

        /// Read here, on the caller's thread (the user is waiting for it), at most a project's
        /// largest size; a copy cut short or not text is refused with why.
        fn read_pending(&self) -> Result<String, String> {
            let path = self.list().first().map(|(p, _)| p.clone()).ok_or("there is no unsaved work to recover")?;
            let mut bytes = Vec::new();
            let max = rustgis_engine::project::max_bytes() + MAX_HEADER;
            std::fs::File::open(&path)
                .and_then(|f| f.take(max + 1).read_to_end(&mut bytes))
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if bytes.len() as u64 > max {
                let mib = ((max - MAX_HEADER) >> 20).to_string();
                return Err(crate::i18n::fmt("the recovery copy is too large (maximum {mib} MiB on this computer)", &[("mib", &mib)]));
            }
            // The description line goes in place: a copy near the size limit isn't held twice.
            let mut text = String::from_utf8(bytes).map_err(|_| "the recovery copy isn't text".to_string())?;
            let header = text.find('\n').ok_or("the recovery copy is incomplete")?;
            text.drain(..=header);
            Ok(text)
        }

        /// The offered copy's file goes after what is queued before it; the next one is offered.
        fn discard_pending(&self) {
            self.take_offered(Job::Remove);
        }

        /// Renamed into this run's place, unchanged: nothing is written again. A rename that
        /// fails is told as a failed write is.
        fn adopt_pending(&self) {
            self.take_offered(Job::Adopt);
        }

        /// The background thread's last failure, taken so it is told once.
        fn take_error(&self) -> Option<String> {
            self.error.lock().unwrap_or_else(|e| e.into_inner()).take()
        }

        /// Kept for the background thread, which calls it after each failure.
        fn set_wake(&self, wake: Box<dyn Fn() + Send + Sync>) {
            *self.wake.lock().unwrap_or_else(|e| e.into_inner()) = Some(wake);
        }
    }

    #[cfg(test)]
    mod tests {
        use std::sync::mpsc::channel;
        use std::sync::{Arc, Mutex};

        use rustgis_engine::{Project, Session};

        use super::{FolderStore, Job, LIVE, Queue, Recovery, work_with};

        /// Writes copies as the store does, except projects named "newer…", which are refused
        /// as too large.
        fn newer_fails(dir: &std::path::Path, project: Project, about: &Recovery) -> Result<(), String> {
            if project.name.starts_with("newer") {
                return Err("the project is too large to keep a recovery copy".into());
            }
            super::write(dir, project, about)
        }

        /// The jobs given, in order, done by the background thread's loop with [`newer_fails`]:
        /// what the folder holds then, and the error told.
        fn run(dir: &std::path::Path, jobs: Vec<Job>) -> (Option<String>, Option<String>) {
            let (tx, rx) = channel();
            for job in jobs {
                tx.send(job).unwrap();
            }
            drop(tx);
            let error = Mutex::new(None);
            work_with(dir, &rx, &error, &Arc::default(), newer_fails);
            (std::fs::read_to_string(dir.join(LIVE)).ok(), error.into_inner().unwrap())
        }

        /// Writes copies as the store does, except a project named "boom", whose copy panics.
        fn boom_panics(dir: &std::path::Path, project: Project, about: &Recovery) -> Result<(), String> {
            if project.name == "boom" {
                panic!("a hostile project");
            }
            super::write(dir, project, about)
        }

        /// A job that panics is told as a failure and the thread goes on with the next one:
        /// autosave doesn't stop without a word.
        #[test]
        fn a_panicking_copy_is_told_and_the_next_one_still_written() {
            let dir = std::env::temp_dir().join(format!("rustgis-autosave-panic-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let copy = |name: &str| Job::Write(Box::new(Project::new(name)), Recovery { name: name.into(), ..Recovery::default() });
            let (tx, rx) = channel();
            // A removal of another file between keeps the two copies from being merged into one.
            for job in [copy("boom"), Job::Remove(dir.join("unrelated")), copy("after")] {
                tx.send(job).unwrap();
            }
            drop(tx);
            let error = Mutex::new(None);
            work_with(&dir, &rx, &error, &Arc::default(), boom_panics);
            assert!(std::fs::read_to_string(dir.join(LIVE)).unwrap().contains("\"after\""));
            assert!(error.into_inner().unwrap().unwrap().contains("unexpectedly"));
            // A copy skipped for one that panics is written instead.
            std::fs::remove_file(dir.join(LIVE)).unwrap();
            let (tx, rx) = channel();
            for job in [copy("older"), copy("boom")] {
                tx.send(job).unwrap();
            }
            drop(tx);
            let error = Mutex::new(None);
            work_with(&dir, &rx, &error, &Arc::default(), boom_panics);
            assert!(std::fs::read_to_string(dir.join(LIVE)).unwrap().contains("\"older\""));
            assert!(error.into_inner().unwrap().unwrap().contains("unexpectedly"));
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// A recovered copy that can't become this run's copy is still removed by the next clear,
        /// so a save or a clean exit doesn't leave it to be offered again; a write followed at once
        /// by a clear isn't made at all.
        #[test]
        fn a_copy_that_could_not_be_adopted_is_cleared_and_a_doomed_write_skipped() {
            let dir = std::env::temp_dir().join(format!("rustgis-autosave-adopt-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let pending = dir.join("recovered-1.recovery");
            std::fs::write(&pending, "{}\n{}").unwrap();
            // A folder where this run's copy goes: the rename can't replace it.
            std::fs::create_dir_all(dir.join(LIVE).join("blocked")).unwrap();
            let (held, error) = run(&dir, vec![Job::Adopt(pending.clone()), Job::Clear]);
            assert!(held.is_none() && error.unwrap().contains("recovered copy"));
            assert!(!pending.exists(), "removed by the clear");
            std::fs::remove_dir_all(dir.join(LIVE)).unwrap();
            // Written, then cleared at once: skipped, so nothing is written (and nothing waits).
            // A copy the writer would refuse: had it been attempted, its failure would be told.
            let copy = Job::Write(Box::new(Project::new("newer, doomed")), Recovery::default());
            let (held, error) = run(&dir, vec![copy, Job::Clear]);
            assert!(held.is_none() && error.is_none());
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn phone_checkpoints_coalesce_while_waiting_and_stop_at_a_clear() {
            let (jobs, rx) = channel();
            let mut queue = Queue { jobs, latest: None };
            let checkpoint = |name| (Box::new(Project::new(name)), Recovery::default());
            let take = || {
                let Job::Checkpoint(slot) = rx.try_recv().unwrap() else { panic!("expected a checkpoint") };
                slot.lock().unwrap().take().map(|(project, _)| project.name)
            };

            queue.checkpoint(checkpoint("first"));
            assert_eq!(take().as_deref(), Some("first"));
            queue.checkpoint(checkpoint("second"));
            queue.checkpoint(checkpoint("third"));
            assert_eq!(take().as_deref(), Some("third"));
            assert!(rx.try_recv().is_err(), "only one write waits");

            queue.checkpoint(checkpoint("before save"));
            queue.send(Job::Clear);
            queue.checkpoint(checkpoint("after save"));
            assert!(take().is_none(), "save invalidated the waiting copy");
            assert!(matches!(rx.try_recv(), Ok(Job::Clear)));
            assert_eq!(take().as_deref(), Some("after save"));
        }

        /// A stalled worker cannot keep Back waiting past the phone's exit budget.
        #[test]
        fn phone_exit_leaves_the_last_complete_copy_when_the_writer_stalls() {
            use std::time::Duration;

            let dir = tempfile::tempdir().unwrap();
            let previous = Recovery { name: "previous edit".into(), ..Recovery::default() };
            super::write(dir.path(), Project::new("previous edit"), &previous).unwrap();
            let (jobs, rx) = channel();
            // Hold the worker's receiver without processing any jobs.
            let store = FolderStore {
                jobs: Mutex::new(Queue { jobs, latest: None }),
                waiting: Mutex::default(),
                error: Arc::default(),
                wake: Arc::default(),
            };
            let mut app = crate::RustGisApp::new(Session::new(Project::default())).with_edit_recovery(Box::new(store));
            app.session.mark_unsaved();
            let (exited, exit) = channel();
            let stalled = std::thread::spawn(move || {
                // test-wait: watchdog releases a stalled flush if the phone exceeds its exit budget.
                let timely = exit.recv_timeout(Duration::from_secs(4)).is_ok();
                drop(rx);
                timely
            });
            app.finish_recovery_copy();
            let _ = exited.send(());
            assert!(stalled.join().unwrap(), "exit kept waiting for the stalled writer");
            assert_eq!(super::describe(&dir.path().join(LIVE)).name, previous.name);
        }

        /// A skipped copy is written when the newer one that replaced it fails, and
        /// still in order: a removal queued after both removes it.
        #[test]
        fn a_skipped_copy_stands_in_for_a_newer_one_that_failed() {
            let dir = std::env::temp_dir().join(format!("rustgis-autosave-skip-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let copy = |name: &str| Job::Write(Box::new(Project::new(name)), Recovery { name: name.into(), ..Recovery::default() });
            let (held, error) = run(&dir, vec![copy("older"), copy("newer")]);
            assert!(held.unwrap().contains("\"older\""), "the skipped copy is kept");
            assert!(error.unwrap().contains("too large"), "the newer one's failure is told");
            let (held, _) = run(&dir, vec![copy("older"), copy("newer"), Job::Clear]);
            assert!(held.is_none(), "removed after, as asked");
            // Three queued: the two newer ones fail, the oldest is still written.
            let (held, error) = run(&dir, vec![copy("older"), copy("newer"), copy("newer still")]);
            assert!(held.unwrap().contains("\"older\""), "the viable copy is kept");
            assert!(error.is_some());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use rustgis_engine::{Project, Session};
    use serde_json::json;

    use super::{Duration, Instant, PHONE_INTERVAL, Recovery, RecoveryStore, kept_line, now_secs, tick, tick_at};
    use crate::{RustGisApp, Services};

    /// A store that counts the copies and removals asked of it and fails the next copy when
    /// told to.
    #[derive(Clone, Default)]
    struct Counting(Rc<RefCell<(usize, Option<String>, usize)>>);

    impl RecoveryStore for Counting {
        /// Counts the copy; a failure set by the test is reported as the worker's would be.
        fn write(&self, _: Project, _: Recovery) {
            self.0.borrow_mut().0 += 1;
        }
        /// Counts the removal.
        fn clear(&self) {
            self.0.borrow_mut().2 += 1;
        }
        /// Nothing is in flight.
        fn flush_for(&self, _: Duration) {}
        /// Nothing waits.
        fn pending(&self) -> Option<Recovery> {
            None
        }
        /// Nothing waits.
        fn waiting(&self) -> usize {
            0
        }
        /// Nothing waits.
        fn read_pending(&self) -> Result<String, String> {
            Err("none".into())
        }
        /// Nothing waits.
        fn discard_pending(&self) {}
        /// Nothing waits.
        fn adopt_pending(&self) {}
        /// The failure set by the test, once.
        fn take_error(&self) -> Option<String> {
            self.0.borrow_mut().1.take()
        }
    }

    /// A copy that failed in the background is made again at the next interval,
    /// though the project hasn't changed since, and the failure is told once.
    #[test]
    fn a_failed_copy_is_retried() {
        let store = Counting::default();
        let mut app = RustGisApp::new(Session::new(Project::default()))
            .with_services(Services { recovery: Some(Box::new(store.clone())), ..Default::default() });
        let ctx = egui::Context::default();
        app.run("layer.template", json!({"template": "points", "name": "Unsaved"})).unwrap();
        tick(&mut app, &ctx);
        assert_eq!(store.0.borrow().0, 1);
        // Unchanged: nothing more, even once the interval has passed.
        app.autosave.last = None;
        tick(&mut app, &ctx);
        assert_eq!(store.0.borrow().0, 1);
        // The write failed: told, and made again once the interval has passed.
        app.autosave.last = Some(web_time::Instant::now());
        store.0.borrow_mut().1 = Some("disk full".into());
        tick(&mut app, &ctx);
        assert!(app.session.notices.iter().any(|n| n.error && n.text.contains("disk full")));
        assert_eq!(store.0.borrow().0, 1, "not before the interval");
        app.autosave.last = None;
        tick(&mut app, &ctx);
        assert_eq!(store.0.borrow().0, 2);
        store.0.borrow_mut().1 = Some("disk full".into());
        let told = app.session.notices.len();
        tick(&mut app, &ctx);
        assert_eq!(app.session.notices.len(), told, "the same failure is told once");
        // Saved after a failure: the copy an earlier write left is still removed.
        app.run(
            "project.saveAs",
            json!({"path": std::env::temp_dir().join(format!("rustgis-retry-{}.rgis", std::process::id())).display().to_string()}),
        )
        .unwrap();
        tick(&mut app, &ctx);
        assert_eq!(store.0.borrow().2, 1, "removed once saved");
        let _ = std::fs::remove_file(std::env::temp_dir().join(format!("rustgis-retry-{}.rgis", std::process::id())));
    }

    #[test]
    fn phone_submits_one_edit_immediately_and_clears_after_save() {
        let store = Counting::default();
        let mut app = RustGisApp::new(Session::new(Project::default())).with_edit_recovery(Box::new(store.clone()));
        let ctx = egui::Context::default();
        let start = Instant::now();
        tick_at(&mut app, Some(&ctx), start);
        assert_eq!(store.0.borrow().0, 0, "a clean project needs no copy");

        app.run("layer.template", json!({"template": "points", "name": "Sites"})).unwrap();
        tick_at(&mut app, Some(&ctx), start);
        assert_eq!(store.0.borrow().0, 1, "the edit submits in the same frame");

        app.session.mark_saved();
        tick_at(&mut app, Some(&ctx), start);
        assert_eq!(store.0.borrow().2, 1, "an explicit save clears the copy");
    }

    #[test]
    fn phone_submits_a_burst_once_immediately_and_once_at_the_deadline() {
        let store = Counting::default();
        let mut app = RustGisApp::new(Session::new(Project::default())).with_edit_recovery(Box::new(store.clone()));
        let ctx = egui::Context::default();
        let start = Instant::now();
        app.run("layer.template", json!({"template": "points", "name": "Sites"})).unwrap();
        tick_at(&mut app, Some(&ctx), start);
        assert_eq!(store.0.borrow().0, 1);

        for millis in [500, 1_000, 1_999] {
            app.run("edit.addFeature", json!({"layer": "Sites", "geometry": {"type": "Point", "coordinates": [10, 5]}})).unwrap();
            tick_at(&mut app, Some(&ctx), start + Duration::from_millis(millis));
            assert_eq!(store.0.borrow().0, 1, "still within the interval");
        }
        let latest = app.session.state();
        tick_at(&mut app, Some(&ctx), start + PHONE_INTERVAL);
        assert_eq!(store.0.borrow().0, 2, "the last edit did not move the deadline");
        assert_eq!(app.autosave.written, Some(latest));
        app.run("tool.set", json!({"tool": "select"})).unwrap();
        tick_at(&mut app, Some(&ctx), start + 2 * PHONE_INTERVAL);
        assert_eq!(store.0.borrow().0, 2, "a view change needs no copy");
    }

    #[test]
    fn phone_exit_writes_the_edit_still_within_the_interval() {
        let dir = tempfile::tempdir().unwrap();
        let mut app =
            RustGisApp::new(Session::new(Project::default())).with_edit_recovery(Box::new(super::FolderStore::open(dir.path()).unwrap()));
        let ctx = egui::Context::default();
        let start = Instant::now();
        app.run("layer.template", json!({"template": "points", "name": "Sites"})).unwrap();
        tick_at(&mut app, Some(&ctx), start);
        app.run("edit.addFeature", json!({"layer": "Sites", "geometry": {"type": "Point", "coordinates": [10, 5]}})).unwrap();
        tick_at(&mut app, Some(&ctx), start + Duration::from_secs(1));
        assert_ne!(app.autosave.written, Some(app.session.state()));

        app.finish_recovery_copy();
        drop(app);

        let mut restarted =
            RustGisApp::new(Session::new(Project::default())).with_edit_recovery(Box::new(super::FolderStore::open(dir.path()).unwrap()));
        restarted.run("ui.recover", json!(null)).unwrap();
        assert_eq!(restarted.session.map().layers[0].dataset.features.len(), 1);
        restarted.services.recovery.as_ref().unwrap().flush();
    }

    /// Capture and checkpoint in one UI frame. With no exit hook or later frame, the next
    /// run can return the point and its photo to the home project once the writer has finished.
    #[test]
    fn phone_offers_a_captured_point_and_can_bring_the_recovered_package_back() {
        use rustgis_engine::data::zip;
        use rustgis_engine::map::{TileKind, TileLayer};

        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let basemap = dir.join("site.mbtiles");
        std::fs::write(&basemap, b"basemap bytes").unwrap();
        let sent = dir.join("Survey.rgisfield");
        let photo = dir.join("site.jpg");
        std::fs::write(&photo, b"photo bytes").unwrap();

        let mut home = Session::new(Project::new("Home"));
        home.path = Some(dir.join("Home.rgis"));
        home.run("layer.template", &json!({"template": "points", "name": "Sites"})).unwrap();
        home.map_mut().tiles.push(TileLayer::new("Site", TileKind::MbTiles, basemap.to_str().unwrap()));
        home.run("offline.package", &json!({"path": sent, "name": "Survey", "extent": [-20, -40, 60, 40]})).unwrap();

        let files = zip::read(&std::fs::read(&sent).unwrap()).unwrap();
        // Model the shell after opening the archive: the engine resolves unpacked files
        // to absolute paths. Use the test's own folder in place of the app's data directory.
        let unpacked = dir.join("field");
        for (name, bytes) in &files {
            let path = unpacked.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        let mut project = Project::load(&unpacked.join("Survey.rgis")).unwrap();
        let origin = project.field_origin.clone().unwrap();
        let expected_basemap = project.maps[0].tiles[0].url.clone();
        assert_eq!(std::path::Path::new(&expected_basemap), unpacked.join("basemaps/site.mbtiles"));
        // Simulate the camera taking a photo in the private folder before the edit.
        project.maps[0].layers[0].dataset.fields.push(rustgis_engine::data::Field::new("photos", rustgis_engine::data::FieldType::Text));
        let mut session = Session::new(project);
        session.restore_recovery_paths(None, Some(sent.clone()));
        session.mark_saved();
        let recovery = dir.join("recovery");
        let mut app = RustGisApp::new(session).with_edit_recovery(Box::new(super::FolderStore::open(&recovery).unwrap()));
        app.fonts_frames = 2;
        app.chrome = Some(|app, _| {
            if !app.session.dirty() {
                app.run_by_user("edit.enable", json!({"on": true})).unwrap();
                let photo = app.session.opened_package().unwrap().with_file_name("site.jpg");
                app.run_by_user(
                    "edit.addFeature",
                    json!({"layer": "Sites", "geometry": {"type": "Point", "coordinates": [10, 5]}, "attributes": {"photos": photo}}),
                )
                .unwrap();
            }
        });
        let ctx = egui::Context::default();
        let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| app.ui(ui));
        frame.textures_delta.clear();
        assert_eq!(app.autosave.written, Some(app.session.state()), "the editing frame submits the copy");
        app.services.recovery.as_ref().unwrap().flush();
        drop(app);

        let mut restarted =
            RustGisApp::new(Session::new(Project::default())).with_edit_recovery(Box::new(super::FolderStore::open(&recovery).unwrap()));
        assert_eq!(restarted.run("ui.autosave", json!(null)).unwrap()["pending"]["name"], "Survey");
        restarted.run("ui.recover", json!(null)).unwrap();
        assert!(restarted.session.dirty() && restarted.session.path.is_none());
        assert_eq!(restarted.session.opened_package(), Some(sent.as_path()));
        assert_eq!(restarted.session.project.field_origin.as_ref(), Some(&origin));
        assert_eq!(restarted.session.map().tiles[0].url, expected_basemap);

        let back = dir.join("Return.rgisfield");
        restarted.session.run("offline.package", &json!({"path": back})).unwrap();
        // The home device can see only the returned package, so it has to unpack the photo.
        std::fs::remove_file(photo).unwrap();
        let result = home.run("offline.bringBack", &json!({"path": back})).unwrap();
        assert_eq!(result["added"], 1);
        assert_eq!(result["photos"], 1);
        assert_eq!(std::fs::read(dir.join("Survey photos/site.jpg")).unwrap(), b"photo bytes");
        restarted.services.recovery.as_ref().unwrap().flush();
    }

    /// Hidden fill settings are ordinary project data and survive package recovery.
    #[test]
    fn phone_recovers_a_field_packages_hidden_fill() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::new(Project::new("Survey"));
        session.map_mut().name = "Survey (offline)".into();
        session.run("layer.template", &json!({"template": "polygons", "name": "Parcels"})).unwrap();
        session.run("layer.color", &json!({"layer": "Parcels", "color": "#33669964"})).unwrap();
        let parcels = session.map().layer_by_name("Parcels").unwrap();
        let renderer = parcels.renderer.clone();
        let layer = parcels.id;
        session.active_layer = Some(layer);
        session.restore_recovery_paths(None, Some(dir.path().join("Survey.rgisfield")));
        let mut app = RustGisApp::new(session).with_edit_recovery(Box::new(super::FolderStore::open(dir.path()).unwrap()));
        app.run("layer.noFill", json!({"layer": "Parcels"})).unwrap();
        app.finish_recovery_copy();
        drop(app);

        let mut restarted =
            RustGisApp::new(Session::new(Project::default())).with_edit_recovery(Box::new(super::FolderStore::open(dir.path()).unwrap()));
        restarted.run("ui.recover", json!(null)).unwrap();
        restarted.session.active_layer = Some(layer);
        let mark = restarted.session.notice_mark();
        restarted.run("layer.restoreFill", json!({"layer": "Parcels"})).unwrap();
        assert!(restarted.session.notices_since(mark).is_empty());
        assert_eq!(restarted.session.map().layer_by_name("Parcels").unwrap().renderer, renderer);
        restarted.services.recovery.as_ref().unwrap().flush();
    }

    #[test]
    fn recovery_actions_are_reachable_after_scrolling_a_long_diagnostic() {
        use egui_kittest::kittest::Queryable;

        let mut app = RustGisApp::new(Session::new(Project::default()));
        app.autosave.offer = Some(Recovery { name: "Survey".into(), time: now_secs(), ..Recovery::default() });
        app.autosave.offer_error = Some(
            (1..=12)
                .map(|line| format!("Diagnostic line {line} of 12 for the copy that was kept with unsaved changes."))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(360.0, 360.0))
            .allow_missing_glyphs()
            .with_context_setup(|ctx| crate::theme::install_fonts(ctx, &[], None))
            .build_ui_state(|ui, app| super::offer(app, ui.ctx()), app);
        h.run();
        h.hover_at(egui::pos2(180.0, 240.0));
        h.run();
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -1000.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        });
        h.run();

        for label in ["Discard", "Later"] {
            let button = h.get_by_role_and_label(egui::Role::Button, label);
            assert!(h.ctx.content_rect().contains_rect(button.rect()), "{label} is outside the viewport");
        }
        h.get_by_role_and_label(egui::Role::Button, "Later").click();
        h.run();
        assert!(h.state().autosave.later);
    }

    /// The dialog's age reads as a whole sentence in every range: never "1 minutes ago".
    #[test]
    fn the_copy_s_age_reads_naturally() {
        let now = now_secs();
        for (ago, says) in [
            (5, "less than a minute ago"),
            (90, "a minute ago"),
            (150, "2 minutes ago"),
            (3 * 3_600, "3 hours ago"),
            (3 * 86_400, "3 days ago"),
        ] {
            let line = kept_line("Parcels", now - ago);
            assert!(line.contains(says) && line.contains("Parcels"), "{ago}: {line}");
        }
        assert!(!kept_line("Parcels", now - 61).contains("1 minutes"));
    }
}
