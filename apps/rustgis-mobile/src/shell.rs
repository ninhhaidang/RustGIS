//! What the Android and iOS apps share: the desktop front end in a window of the phone's, with
//! the phone ([`Phone`], a `rustgis_engine::platform::Platform`) standing where the browser
//! stands for the web app. Each system's module makes its [`Phone`] and calls [`run`].

use eframe::egui;
use rustgis_engine::platform::{Event, Platform};
use rustgis_ui_egui::{FolderStore, Inbox, RustGisApp, i18n, tl};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// How long the window keeps looking for the soft keyboard's room after a field last wanted
/// it: long enough for the keyboard to finish closing.
const KEYBOARD_SETTLES: std::time::Duration = std::time::Duration::from_millis(1500);

/// The memory the limits plan with where a phone's system doesn't say what it allows: 1.5 GiB,
/// what a small phone can give an app.
pub const MEMORY_UNSAID: u64 = 1536 << 20;

/// Says, in the window, that the limits plan with [`MEMORY_UNSAID`].
pub fn say_memory_unsaid(app: &mut RustGisApp) {
    app.session.warn(tl!("This device didn't say how much memory RustGIS may use, so its limits plan for 1.5 GiB."));
}

/// What the shell asks of a phone beyond [`Platform`].
pub trait Phone: Platform {
    /// The room the system's bars, a camera cutout and the soft keyboard take at the window's
    /// edges, in points, and whether the keyboard is up where the system says. `None` when the
    /// phone can't say, which leaves egui's last answer in place.
    fn insets(&self, pixels_per_point: f32) -> Option<(egui::SafeAreaInsets, Option<bool>)>;

    /// Draws the system bars' icons dark (a `light` ground under them) or light (a dark one).
    fn bars_light(&self, light: bool, frame: &eframe::Frame);

    /// Whether something asked for is still to be reported: the window keeps asking while it is.
    fn busy(&self) -> bool;

    /// Whether a picker (or the camera) is on the screen and hasn't answered: a picker asked for
    /// now is refused. A phone that wakes the window when one answers leaves it out of
    /// [`Phone::busy`], so the window doesn't redraw behind it. The default, [`Phone::busy`],
    /// is for a phone that can't tell.
    fn picking(&self) -> bool {
        self.busy()
    }

    /// How often (milliseconds) a window with nothing else to look for looks again, for a phone
    /// that reports only when asked and can't wake the window (Android's memory). `None`, the
    /// default: the phone wakes it.
    fn idle_look(&self) -> Option<u64> {
        None
    }

    /// Whether the soft keyboard's Return was pressed since the last frame, where the system
    /// passes it on as no key egui knows (iOS). The shell gives egui an Enter for it.
    fn take_return(&self) -> bool {
        false
    }

    /// Puts `text`, copied in the window, on the system's clipboard too, where other apps can
    /// paste it; an error says why it couldn't be asked. The default does nothing: the window's
    /// own clipboard keeps it (iOS).
    fn copy_text(&self, text: &str) -> Result<(), String> {
        let _ = text;
        Ok(())
    }

    /// Asks the system for its Back (`on`), or leaves it to the system, where it keeps Back from
    /// an app that doesn't ask (Android 16 on). Where Back comes as a key, or there is none (iOS),
    /// this does nothing.
    fn takes_back(&self, _on: bool) {}

    /// Whether the system's Back asked for came since the last frame, where it comes as no key egui
    /// knows (Android 16 on). The shell gives egui the Back key for it.
    fn take_back(&self) -> bool {
        false
    }
}

/// What a service couldn't do, kept until the next frame says it in the window (a service has
/// no session to tell).
type Refusals = Rc<RefCell<Vec<String>>>;

/// The requests for files waited on ([`crate::Requests`]).
type Asked = Rc<RefCell<crate::Requests>>;

/// What the front end keeps for the picker that is up (Add Delimited Text's options, the command
/// a picked project opens with, a picture's corners): a request the phone refuses while it is
/// up resets these before it is refused, so they are put back after the frame.
#[derive(Clone)]
struct Kept {
    csv: Option<serde_json::Value>,
    shapefile: Option<serde_json::Value>,
    open: Option<&'static str>,
    picture: Option<Vec<rustgis_engine::geom::Coord>>,
    layout_picture: bool,
}

impl Kept {
    fn of(ui: &rustgis_ui_egui::UiState) -> Kept {
        Kept {
            csv: ui.pending_csv.clone(),
            shapefile: ui.pending_shapefile.clone(),
            open: ui.pending_open,
            picture: ui.pending_picture.clone(),
            layout_picture: ui.pending_layout_picture,
        }
    }

    fn restore(self, app: &mut RustGisApp) {
        let ui = &mut app.ui;
        ui.pending_csv = self.csv;
        ui.pending_shapefile = self.shapefile;
        ui.pending_open = self.open;
        ui.pending_picture = self.picture;
        ui.pending_layout_picture = self.layout_picture;
    }
}

/// The app: the desktop front end, with the files to open once the window runs.
struct App<P: Phone> {
    app: RustGisApp,
    /// The files pushed to the app's folder ([`pushed`]), until the first frame takes them.
    pushed: Vec<PathBuf>,
    /// How soon (milliseconds) the window draws again to take what is still to arrive from
    /// the phone; `None` when nothing is.
    poll: Option<u64>,
    /// Whether the phone was last asked for positions (`RustGisApp::locate`, as last seen).
    locating: bool,
    /// Whether the phone was last asked to keep positions coming out of sight
    /// (`RustGisApp::keep_locating`, as last seen).
    keeping: bool,
    /// Whether the soft keyboard was up, where the phone says.
    keyboard: Option<bool>,
    /// Whether the interface has something Back lowers (`rustgis_ui_touch::takes_back`), as
    /// the phone was last told.
    back: bool,
    /// Until when the window looks again for the soft keyboard's room: a moment after the
    /// last frame a field wanted the keyboard.
    typing: Option<std::time::Instant>,
    /// Whether the system bars' icons were last asked to be dark (a light ground); `None`
    /// before the first frame.
    bars_light: Option<bool>,
    /// The phone, where it can be reached.
    phone: Option<Rc<P>>,
    /// Picked files the front end reads as bytes (`Services::inbox`).
    inbox: Inbox,
    refusals: Refusals,
    /// The requests for files waited on.
    asked: Asked,
    /// What the front end kept for that picker, as the frame began ([`Kept`]).
    kept: Rc<RefCell<Option<Kept>>>,
    /// Whether a picker was refused in this frame because one was up.
    refused_picker: Rc<std::cell::Cell<bool>>,
    /// Files picked for a dialog (purpose, name, copy), until their picker has answered in
    /// full: a Shapefile's parts arrive one by one and are read together.
    waiting: Vec<(String, String, PathBuf)>,
    /// What reading them off the window's thread found, until the next frame says it.
    problems: Problems,
    /// The batches being read.
    reading: Arc<AtomicUsize>,
}

/// Why a picked file didn't reach the dialog that asked for it.
enum Problem {
    /// The shortcut file `name` is over `SHORTCUT_FILE_MAX_BYTES`.
    ShortcutTooLarge { name: String },
    /// The script file `name`, picked for the Script pane's Load…, is over `SCRIPT_FILE_MAX_BYTES`.
    ScriptTooLarge { name: String },
    /// The image picked for a marker's «Image…» is over `MARKER_FILE_MAX_BYTES`.
    MarkerTooLarge,
    /// The pictures `names`, picked for the AI Assistant's message, are over
    /// `ASSISTANT_FILE_MAX_BYTES`: said once, with how many.
    PicturesTooLarge { names: Vec<String> },
    /// The pictures `names`, picked for the AI Assistant's message, are past what a message takes.
    PicturesLeftOut { names: Vec<String> },
    /// The file `name` is over the size a pick reads into memory ([`PICK_MAX_BYTES`]), by
    /// itself or `together` with the files picked with it and already read.
    TooLarge { name: String, together: bool },
    /// The copy of the file `name` couldn't be read.
    Unread { name: String, error: String },
    /// What gathering a Shapefile's parts found, in its own words.
    Shapefile(String),
}

type Problems = Arc<Mutex<Vec<Problem>>>;

impl<P: Phone> App<P> {
    /// Takes what the phone did since the last frame: picked files are opened, the rest is
    /// said in the window.
    fn take_phone_events(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        for text in self.refusals.borrow_mut().drain(..) {
            self.app.session.notify(text, true);
        }
        let Some(phone) = self.phone.clone() else { return };
        // Positions out of sight follow what the interface asks for (a track being recorded).
        if self.app.keep_locating != self.keeping {
            self.keeping = self.app.keep_locating;
            if let Err(e) = phone.keep_locating(self.keeping) {
                self.app.session.notify(e, true);
            }
        }
        // Positioning follows what the interface asks for; off, the last position is let go.
        if self.app.locate != self.locating {
            self.locating = self.app.locate;
            if let Err(e) = phone.locate(self.locating) {
                self.app.session.notify(e, true);
                self.app.locate = false;
                self.locating = false;
            }
            if !self.locating {
                self.app.position = None;
            }
        }
        let events = phone.take_events();
        // Mirrors EventBuffer.PER_FRAME in the Android bridge: a full batch may leave more queued.
        if events.len() >= 512 {
            ctx.request_repaint();
        }
        for event in events {
            match event {
                Event::Picked { purpose, name, path, document } => {
                    // A project the phone may write back to: Save writes there once it is open.
                    if let Some(document) = document.filter(|_| purpose == "project") {
                        self.app.picked_document(path.clone(), document);
                    }
                    // A file answers its own request; one of an earlier request (still being
                    // copied when another was asked for) leaves the later one waiting.
                    self.asked.borrow_mut().picked(&purpose);
                    self.waiting.push((purpose, name, path));
                }
                // The picker on the screen closed with no choice: its request is ended.
                Event::Cancelled => {
                    let ended = self.asked.borrow_mut().cancelled();
                    self.end(ended);
                }
                // A request whose every copy failed, by name.
                Event::Ended { purpose } => {
                    let ended = self.asked.borrow_mut().ended(&purpose);
                    self.end(ended);
                }
                Event::Photo { path, .. } => {
                    self.asked.borrow_mut().photo_taken();
                    self.app.photos.push(path);
                }
                Event::Saved { name } => self.app.session.notify(i18n::fmt("Saved {name}.", &[("name", &name)]), false),
                Event::ProjectDocument(document) => self.app.project_document(document),
                Event::ProjectSave(result) => self.app.complete_project_save(result),
                Event::WriteBackFailed { name, reason, kept } => {
                    // Save again only helps while the project saved is the one open.
                    let open = self.app.saving_the_open_project();
                    let damaged = kept.is_some();
                    let text = write_back_failure(&name, &reason, kept.as_deref(), open);
                    // Turned down while busy with another file: the document wasn't touched.
                    if reason == "code:busy" {
                        self.app.project_save_refused(text);
                    } else {
                        self.app.complete_project_save(Err(text));
                    }
                    // The file may hold part of the project: what is open no longer matches any
                    // saved file, so it counts as unsaved (and asks before it is closed).
                    if damaged && open {
                        self.app.session.mark_unsaved();
                    }
                }
                Event::Position(fix) if self.locating => self.app.report_position(fix),
                // A fix that was on its way when positioning was turned off.
                Event::Position(_) => {}
                // The system ended positioning itself (its reason is a failure beside this): the
                // interface goes back to offering to show the position.
                Event::Stopped => {
                    self.app.locate = false;
                    self.locating = false;
                    self.app.position = None;
                }
                Event::LowMemory => self.app.release_memory(),
                // The Capture form takes it (`rustgis_ui_touch`).
                Event::Typed(typed) => {
                    if let rustgis_engine::platform::Typed::Closed { failed: Some(error), .. } = &typed {
                        self.app.session.notify(i18n::fmt("The text box couldn't be opened: {error}", &[("error", error)]), true);
                    }
                    self.app.typed.push(typed);
                }
                Event::CopyFailed(error) => copy_failed(&mut self.app, &error),
                Event::Failed(text) => self.app.session.notify(text, true),
            }
        }
        for problem in self.problems.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
            let text = match problem {
                Problem::ShortcutTooLarge { name } => {
                    let limit = rustgis_ui_egui::SHORTCUT_FILE_MAX_BYTES.to_string();
                    i18n::fmt("{name} wasn't opened: a shortcut file is at most {limit} bytes.", &[("name", &name), ("limit", &limit)])
                }
                Problem::ScriptTooLarge { name } => i18n::fmt("{file} is larger than 1 MB", &[("file", &name)]),
                Problem::MarkerTooLarge => rustgis_ui_egui::marker_file_too_large().to_string(),
                Problem::PicturesTooLarge { names } => {
                    let n = names.len();
                    let first = names.into_iter().next().unwrap_or_default();
                    let why = rustgis_ui_egui::rustgis_assistant::attach::too_large(first);
                    if n == 1 {
                        i18n::note(&why)
                    } else {
                        // In logical order inside the count's message, put in display form once.
                        i18n::fmt(
                            "{n} pictures weren't attached, such as {first}",
                            &[("n", &n.to_string()), ("first", &i18n::note_raw(&why))],
                        )
                    }
                }
                Problem::PicturesLeftOut { names } => i18n::fmt(
                    "{names} weren't attached: a message takes at most {n} pictures",
                    &[("names", &names.join(", ")), ("n", &rustgis_ui_egui::rustgis_assistant::attach::MAX_PER_MESSAGE.to_string())],
                ),
                Problem::TooLarge { name, together } => {
                    let limit = (PICK_MAX_BYTES >> 20).to_string();
                    let template = if together {
                        "{name} wasn't opened: with the files picked with it, it is over {limit} MiB."
                    } else {
                        "{name} wasn't opened: it is over {limit} MiB."
                    };
                    i18n::fmt(template, &[("name", &name), ("limit", &limit)])
                }
                Problem::Unread { name, error } => i18n::fmt("{name} wasn't opened: {error}", &[("name", &name), ("error", &error)]),
                Problem::Shapefile(text) => text,
            };
            self.app.session.notify(text, true);
        }
        // A picked file is copied on a thread of the phone's: look again until it has arrived.
        let busy = phone.busy();
        // The request waited on has been answered in full once the phone, busy with it, no
        // longer is: with no file picked (each copy failed, said), it is ended.
        let ended = self.asked.borrow_mut().frame(busy || phone.picking());
        self.end(ended);
        // Held while unused copies are being removed: a project picked could name one of them.
        if !busy && !self.waiting.is_empty() && !rustgis_ui_touch::removing_copies(ctx) {
            self.open_waiting(ctx, raw_input);
        }
        // Positions arrive between frames too: looked for while they are wanted. Asked for from
        // inside the frame (`ui`): asked from here, before it, it isn't kept. Soon for what is
        // quick (a file being copied or read); once a second for positions, which come no
        // faster: redrawing the map more often than that would only cost battery.
        self.poll = if busy || self.reading.load(Ordering::Relaxed) > 0 {
            Some(250)
        } else if self.locating {
            Some(1000)
        } else {
            phone.idle_look()
        };
    }

    /// Ends a request for files that got none (`purpose`), as [`answer_ended`] does.
    fn end(&self, purpose: Option<String>) {
        answer_ended(&self.inbox, purpose);
    }

    /// Opens the picked files, now that every picker has answered in full, what was picked
    /// together kept together (its copies share a folder).
    ///
    /// Data and projects open by path, as files dropped on the desktop window do: a
    /// GeoPackage is then edited in place, in its copy, and a file read with the one beside
    /// it finds it. Every other picker's files go where the browser's do, to the dialog that
    /// asked: read into the inbox off the window's thread (a file can be large), a Shapefile's
    /// loose parts gathered into one layer. That read has no Cancel: each file is one read,
    /// and the dialogs' files are small beside data.
    fn open_waiting(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let mut batches: Vec<(Option<PathBuf>, String, Vec<(String, PathBuf)>)> = Vec::new();
        for (purpose, name, path) in self.waiting.drain(..) {
            let folder = path.parent().map(PathBuf::from);
            match batches.iter_mut().find(|(f, _, _)| *f == folder) {
                Some((_, _, files)) => files.push((name, path)),
                None => batches.push((folder, purpose, vec![(name, path)])),
            }
        }
        // The dialogs' batches are read on one thread, one after another, so the cap on what is
        // held in memory holds for the whole pick, from however many folders.
        let mut reads = Vec::new();
        for (_, purpose, files) in batches {
            if purpose == "data" || purpose == "project" {
                // A Shapefile's or a MapInfo file's parts picked with it are read by it, from
                // beside it: opened on their own they would each be refused.
                let paths = crate::without_sidecars(files.into_iter().map(|(_, path)| path).collect());
                raw_input.dropped_files.extend(paths.into_iter().map(|path| Arc::new(Pushed(path)) as _));
                continue;
            }
            reads.push((purpose, files));
        }
        if reads.is_empty() {
            return;
        }
        let (inbox, problems, reading, ctx) = (self.inbox.clone(), self.problems.clone(), self.reading.clone(), ctx.clone());
        let unread: Vec<(String, PathBuf)> = reads.iter().flat_map(|(_, files)| files.clone()).collect();
        let retire: Vec<_> = reads.iter().filter(|(purpose, _)| waits(purpose)).map(|(purpose, _)| crate::mark(purpose)).collect();
        let started = start_reader(&self.inbox, &self.reading, retire, || {
            std::thread::Builder::new().name("picked files".into()).spawn(move || {
                let mut held = 0u64;
                for (purpose, files) in reads {
                    read_batch(&purpose, files, &inbox, &problems, &mut held);
                }
                reading.fetch_sub(1, Ordering::Relaxed);
                ctx.request_repaint();
            })
        });
        if let Err(e) = started {
            let error = e.to_string();
            // Nothing will read these copies now: they go, as they would have once read.
            for (name, path) in unread {
                remove_copy(&path);
                self.app.session.notify(i18n::fmt("{name} wasn't opened: {error}", &[("name", &name), ("error", &error)]), true);
            }
        }
    }
}

/// Balance admission and retire waiting dialogs even when the OS refuses a worker.
fn start_reader<T>(
    inbox: &Inbox,
    reading: &AtomicUsize,
    retire: Vec<String>,
    spawn: impl FnOnce() -> std::io::Result<T>,
) -> std::io::Result<T> {
    reading.fetch_add(1, Ordering::Relaxed);
    match spawn() {
        Ok(worker) => Ok(worker),
        Err(error) => {
            reading.fetch_sub(1, Ordering::Relaxed);
            retire_unread(inbox, retire);
            Err(error)
        }
    }
}

/// A worker that cannot start answers its waiting requests ([`waits`]) just as a cancelled
/// picker does.
fn retire_unread(inbox: &Inbox, marks: Vec<String>) {
    inbox.lock().unwrap_or_else(|e| e.into_inner()).extend(marks.into_iter().map(|mark| (mark, Vec::new())));
}

/// The most a pick reads into memory, in one file or the files picked together: fixed at the
/// size every computer reads ([`rustgis_engine::data::io::BYTES_FLOOR`]), whatever the phone's
/// memory. A phone reports all its memory, but an app may use much less of it before the system
/// stops it, so the desktop's share of the memory isn't a phone's (#2408).
const PICK_MAX_BYTES: u64 = rustgis_engine::data::io::BYTES_FLOOR;

/// Reads one folder's files of a pick (name, copy) into `inbox`, marked for the dialog that
/// asked, and removes the copies: their bytes are all that was wanted of them. `held` counts the
/// bytes the pick has read so far, across its folders.
fn read_batch(purpose: &str, mut files: Vec<(String, PathBuf)>, inbox: &Inbox, problems: &Problems, held: &mut u64) {
    let mark = crate::mark(purpose);
    let say = |problem| problems.lock().unwrap_or_else(|e| e.into_inner()).push(problem);
    // No more of the assistant's pictures are read than a message takes; the rest are said.
    let most = rustgis_ui_egui::rustgis_assistant::attach::MAX_PER_MESSAGE;
    if mark == rustgis_ui_egui::ASSISTANT_PICK && files.len() > most {
        let left: Vec<(String, PathBuf)> = files.drain(most..).collect();
        for (_, path) in &left {
            remove_copy(path);
        }
        say(Problem::PicturesLeftOut { names: left.into_iter().map(|(name, _)| name).collect() });
    }
    let max = PICK_MAX_BYTES;
    let mut read = Vec::new();
    let mut too_large = Vec::new();
    for (name, path) in files {
        let shortcut = mark == rustgis_ui_egui::SHORTCUT_PICK || (mark.is_empty() && name.to_ascii_lowercase().ends_with(".rgisshortcuts"));
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if shortcut && size > rustgis_ui_egui::SHORTCUT_FILE_MAX_BYTES as u64 {
            say(Problem::ShortcutTooLarge { name });
        } else if mark == rustgis_ui_egui::SCRIPT_PICK && size > rustgis_ui_egui::SCRIPT_FILE_MAX_BYTES as u64 {
            // Refused before it is read, as the pane would refuse it after.
            say(Problem::ScriptTooLarge { name });
        } else if mark.starts_with(rustgis_ui_egui::MARKER_PICK) && size > rustgis_ui_egui::MARKER_FILE_MAX_BYTES as u64 {
            say(Problem::MarkerTooLarge);
        } else if mark == rustgis_ui_egui::ASSISTANT_PICK && size > rustgis_ui_egui::ASSISTANT_FILE_MAX_BYTES as u64 {
            too_large.push(name);
        } else if held.saturating_add(size) > max {
            // Refused before it is read: no more than the cap is held in memory at once, in
            // one file or in the files picked together.
            say(Problem::TooLarge { name, together: size <= max });
        } else {
            match std::fs::read(&path) {
                Ok(bytes) => {
                    *held += bytes.len() as u64;
                    read.push((name, bytes));
                }
                Err(e) => say(Problem::Unread { name, error: e.to_string() }),
            }
        }
        remove_copy(&path);
    }
    if !too_large.is_empty() {
        say(Problem::PicturesTooLarge { names: too_large });
    }
    let (read, found) = rustgis_engine::data::io::gather_shapefiles(read);
    for text in found {
        say(Problem::Shapefile(text));
    }
    // A waiting request with nothing read is ended, as a cancelled one is: Import DXF, or a
    // marker's «Image…», stops waiting.
    if waits(purpose) && read.is_empty() {
        inbox.lock().unwrap_or_else(|e| e.into_inner()).push((mark.clone(), Vec::new()));
    }
    inbox.lock().unwrap_or_else(|e| e.into_inner()).extend(read.into_iter().map(|(name, bytes)| (format!("{mark}{name}"), bytes)));
}

/// Removes a picked file's copy, and its folders once they are empty: the one for its source
/// folder, and the one for its pick.
fn remove_copy(path: &Path) {
    let _ = std::fs::remove_file(path);
    for folder in path.ancestors().skip(1).take(2) {
        // Not removed while it holds anything (another copy).
        let _ = std::fs::remove_dir(folder);
    }
}

/// Whether a request for files (`purpose`) has something waiting on its answer, which an empty
/// one ends: Import DXF (`dxf:<id>`) or a marker's «Image…» (`marker:<id>`).
fn waits(purpose: &str) -> bool {
    purpose.starts_with("dxf:") || purpose.starts_with("marker:")
}

/// Ends a request for files that got none (`purpose`): a dialog or marker waiting for its answer
/// (Import DXF, a marker's «Image…») is answered empty in `inbox`, and stops waiting.
fn answer_ended(inbox: &Inbox, purpose: Option<String>) {
    if let Some(purpose) = purpose.filter(|p| waits(p)) {
        inbox.lock().unwrap_or_else(|e| e.into_inner()).push((crate::mark(&purpose), Vec::new()));
    }
}

/// What the user is told when project `name` couldn't be written back: why ([`write_back_reason`]),
/// whether the file was left as it was or may be damaged (the project as saved `kept` there), and,
/// while the project is still `open`, that Save again asks where.
fn write_back_failure(name: &str, reason: &str, kept: Option<&Path>, open: bool) -> String {
    // Turned down while busy: nothing was touched, and Save writes back there once it is done.
    if reason == "code:busy" {
        let text = if open {
            "{name} wasn't saved: another file is being chosen or saved. Save again once it is done."
        } else {
            "{name} wasn't saved: another file is being chosen or saved."
        };
        return i18n::fmt(text, &[("name", name)]);
    }
    let reason = write_back_reason(reason);
    match (kept, open) {
        (None, true) => i18n::fmt(
            "{name} wasn't saved where it was opened from: {reason}. The file wasn't changed: Save again to choose where to save it.",
            &[("name", name), ("reason", &reason)],
        ),
        (None, false) => i18n::fmt(
            "{name} wasn't saved where it was opened from: {reason}. The file wasn't changed.",
            &[("name", name), ("reason", &reason)],
        ),
        (Some(kept), true) => i18n::fmt(
            "{name} may be damaged where it was opened from: {reason}. The project is still open: Save again to choose where to save it. A copy of it as saved is kept at {kept}.",
            &[("name", name), ("reason", &reason), ("kept", &kept.display().to_string())],
        ),
        (Some(kept), false) => i18n::fmt(
            "{name} may be damaged where it was opened from: {reason}. A copy of it as saved is kept at {kept}.",
            &[("name", name), ("reason", &reason), ("kept", &kept.display().to_string())],
        ),
    }
}

/// Why a project couldn't be written back, in the user's language: the app's codes for its own
/// reasons (`DocumentWrite.java`), or the system's text as it gave it.
fn write_back_reason(reason: &str) -> String {
    match reason {
        "code:busy" => tl!("another file is being chosen or saved").to_string(),
        "code:storage" => tl!("the app's storage can't be written to").to_string(),
        "code:unwritable" => tl!("the file can't be written to").to_string(),
        "code:stalled" => tl!("the file took no more bytes").to_string(),
        "code:short" => tl!("the file doesn't hold all of the project after writing it").to_string(),
        "code:unfinished" => tl!("the app ended before it was written").to_string(),
        other => other.to_string(),
    }
}

/// Gives the front end the phone's file picker, the way the browser's shell gives its own:
/// Open and Add Data ask through `open_async`, and what is saved or exported is offered
/// through `download`.
fn use_picker<P: Phone + 'static>(
    app: &mut RustGisApp,
    phone: &Rc<P>,
    inbox: &Inbox,
    refusals: &Refusals,
    asked: &Asked,
    refused_picker: &Rc<std::cell::Cell<bool>>,
    kept: &Rc<RefCell<Option<Kept>>>,
) {
    let (picker, refused, asking, answers) = (phone.clone(), refusals.clone(), asked.clone(), inbox.clone());
    let candidate = Rc::new(RefCell::new(None));
    let snapshot = candidate.clone();
    app.services.open_options = Some(Box::new(move |ui| {
        *snapshot.borrow_mut() = Some(Kept::of(ui));
    }));
    let admitted = kept.clone();
    let refused_while_up = refused_picker.clone();
    app.services.open_async = Some(Box::new(move |purpose: &str| {
        log::info!("asking for files: {purpose}");
        // Refused only where the phone refuses it: an error now (iOS), or a picker on the screen
        // already (Android says why a moment later). Accepted, it is the request from now on,
        // even while the files picked before it are still being copied.
        let up = picker.picking();
        let refused_here = match picker.pick_files(purpose) {
            Ok(()) => up,
            Err(e) => {
                log::warn!("{e}");
                refused.borrow_mut().push(e);
                true
            }
        };
        let options = candidate.borrow_mut().take();
        if !refused_here {
            *admitted.borrow_mut() = options;
        }
        let ended = asking.borrow_mut().asked(purpose, refused_here);
        // Refused because another picker was up: the shell puts back what the front end kept
        // for that one ([`Kept`]).
        if refused_here && up {
            refused_while_up.set(true);
        }
        // A request whose picker didn't show is ended now: Import DXF, or a marker's «Image…»,
        // stops waiting.
        answer_ended(&answers, ended);
    }));
    let saver = phone.clone();
    app.services.download = Some(Box::new(move |name: &str, bytes: Vec<u8>| saver.save_file(name, bytes, false)));
    let saver = phone.clone();
    app.services.save_project = Some(Box::new(move |name: &str, bytes: Vec<u8>, document: Option<&str>| match document {
        Some(document) => saver.write_back(document, name, bytes),
        None => saver.save_file(name, bytes, true),
    }));
    app.services.inbox = Some(inbox.clone());
    let (camera, photographing) = (phone.clone(), asked.clone());
    app.services.take_photo = Some(Box::new(move || {
        camera.take_photo()?;
        // Its cancel, if it comes, is the camera's ([`crate::Requests::cancelled`]).
        photographing.borrow_mut().photo_asked();
        Ok(())
    }));
    // Where the phone has text fields of its own, the touch interface's fields open one.
    if phone.enters_text() {
        let typist = phone.clone();
        app.services.enter_text = Some(Box::new(move |entry| typist.enter_text(entry)));
    }
}

impl<P: Phone> eframe::App for App<P> {
    /// When the event loop exits, give the recovery writer a short chance to finish the latest
    /// edit. The next launch offers the last complete copy, if one exists.
    fn on_exit(&mut self) {
        self.app.finish_recovery_copy();
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // The GPU map backend draws inside egui's wgpu pass; hand over the render state.
        self.app.set_render_state(frame.wgpu_render_state().cloned());
        self.app.logic(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // The window runs under the system's bars; the interface keeps to what they leave
        // (`insets`), over a ground of its own colour so the bars read as part of it.
        let ctx = ui.ctx().clone();
        let tokens = rustgis_ui_egui::theme::Tokens::get(&ctx);
        // The bars' icons are drawn to be read on that ground, whichever theme it is.
        if self.bars_light != Some(!tokens.dark) {
            self.bars_light = Some(!tokens.dark);
            if let Some(phone) = &self.phone {
                phone.bars_light(!tokens.dark, frame);
            }
        }
        ui.painter().rect_filled(ctx.viewport_rect(), 0.0, tokens.pane);
        ui.scope_builder(egui::UiBuilder::new().max_rect(ctx.content_rect()), |ui| self.app.ui(ui));
        // Text copied in the window goes on the system's clipboard as well, for other apps.
        if let Some(phone) = &self.phone {
            let copied: Vec<String> = ctx.output(|o| {
                o.commands
                    .iter()
                    .filter_map(|c| match c {
                        egui::OutputCommand::CopyText(text) => Some(text.clone()),
                        _ => None,
                    })
                    .collect()
            });
            copy_out(&mut self.app, phone.as_ref(), copied);
        }
        let back = rustgis_ui_touch::takes_back(&self.app, &ctx);
        if back != self.back {
            self.back = back;
            if let Some(phone) = &self.phone {
                phone.takes_back(back);
            }
        }
        // A second picker asked for while one was up was refused: what the front end kept for
        // the first is put back, so its file is read as it was asked for.
        if self.refused_picker.replace(false)
            && let Some(kept) = self.kept.borrow().clone()
        {
            kept.restore(&mut self.app);
        }
        // Something of the phone's is still to arrive (a picked file, a position): look again.
        // A picker or a save asked for in this frame counts too: the poll was set before it.
        if self.poll.is_none_or(|wait| wait > 250) && self.phone.as_ref().is_some_and(|phone| phone.busy()) {
            self.poll = Some(250);
        }
        if let Some(wait) = self.poll {
            ctx.request_repaint_after(std::time::Duration::from_millis(wait));
        }
        // The soft keyboard comes and goes without waking the app: while a field wants it,
        // and a moment after, the window looks again for the room it takes (`insets`).
        let now = std::time::Instant::now();
        if ctx.egui_wants_keyboard_input() {
            self.typing = Some(now + KEYBOARD_SETTLES);
        }
        if self.typing.is_some_and(|until| now < until) {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        } else {
            self.typing = None;
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let room = self.phone.as_deref().and_then(|phone| phone.insets(ctx.pixels_per_point()));
        if let Some((edges, _)) = room {
            raw_input.safe_area_insets = Some(edges);
        }
        // The soft keyboard closed (its own button, or Back on Android) while a field still has
        // the focus: typing is over, so the field lets it go. Kept, the interface would stay
        // laid out for typing until the next tap, and move what that tap was aimed at. iOS and
        // Android 11 and later say when the keyboard goes. Before, Android doesn't report it at
        // all (nor its room), and the field keeps the focus until the next tap.
        if let Some((_, keyboard)) = room {
            if self.keyboard == Some(true) && keyboard == Some(false) && ctx.egui_wants_keyboard_input() {
                ctx.memory_mut(|m| m.stop_text_input());
            }
            self.keyboard = keyboard;
        }
        self.take_phone_events(ctx, raw_input);
        // The soft keyboard's Return, as the Enter key egui knows: each field does with it what
        // it does with Enter.
        if self.phone.as_ref().is_some_and(|phone| phone.take_return()) {
            press(raw_input, egui::Key::Enter);
        }
        // The system's Back, as the key older Androids send for it.
        if self.back && self.phone.as_ref().is_some_and(|phone| phone.take_back()) {
            press(raw_input, egui::Key::BrowserBack);
        }
        if !self.phone.as_ref().is_some_and(|phone| phone.picking()) {
            self.kept.borrow_mut().take();
        }
        // The pushed files open as files dropped on the desktop window do: once the window
        // runs, read off its thread with progress and Cancel, each failure said in the window.
        // Opened before the event loop, a large one would hold the app's start.
        // Held, as picked files are, while unused copies are being removed.
        let pushed = if rustgis_ui_touch::removing_copies(ctx) { Vec::new() } else { std::mem::take(&mut self.pushed) };
        for path in pushed {
            raw_input.dropped_files.push(Arc::new(Pushed(path)));
        }
        self.app.drop_stale_clicks(raw_input);
    }
}

/// Gives egui a press and release of `key`.
fn press(raw_input: &mut egui::RawInput, key: egui::Key) {
    for pressed in [true, false] {
        raw_input.events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE });
    }
}

/// A pushed or picked file, handed to the front end as a file dropped on the window: by its path.
#[derive(Debug)]
struct Pushed(PathBuf);

impl egui::DroppedFile for Pushed {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// The files and folders in `dir` (a folder of the app's that files can be put in from a
/// computer: a way in for tests, beside the file picker), in name order, at most one project. A
/// folder or an entry that can't be read is said in the window. `None` is said too: the system
/// gave no such folder.
pub fn pushed(dir: Option<PathBuf>, app: &mut RustGisApp) -> Vec<PathBuf> {
    let Some(dir) = dir else {
        app.session.warn(tl!("This device gives RustGIS no folder for files put on it: there are no pushed files to open."));
        return Vec::new();
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) => {
            let (dir, error) = (dir.display().to_string(), e.to_string());
            app.session.notify(i18n::fmt("The files in {dir} weren't opened: {error}", &[("dir", &dir), ("error", &error)]), true);
            return Vec::new();
        }
    };
    let mut files = Vec::new();
    let mut unread = 0usize;
    let mut first = None;
    for entry in entries {
        match entry {
            // Folders too (a Zarr store is one): what the front end can't open, it says.
            Ok(entry) => files.push(entry.path()),
            Err(e) => {
                unread += 1;
                first.get_or_insert(e);
            }
        }
    }
    if let Some(e) = first {
        let (count, dir, error) = (unread.to_string(), dir.display().to_string(), e.to_string());
        app.session.notify(
            i18n::fmt(
                "{count} entries of {dir} couldn't be read and weren't opened: {error}",
                &[("count", &count), ("dir", &dir), ("error", &error)],
            ),
            true,
        );
    }
    files.sort();
    // One project: each one opened would replace the one before it, and nothing would say so.
    let mut projects = 0usize;
    files.retain(|f| {
        let project = rustgis_engine::project::open_command(&f.display().to_string()) != "layer.addData";
        projects += usize::from(project);
        !project || projects == 1
    });
    if projects > 1 {
        app.session.notify(
            i18n::fmt(
                "{count} other projects in {dir} weren't opened: only the first project is.",
                &[("count", &(projects - 1).to_string()), ("dir", &dir.display().to_string())],
            ),
            false,
        );
    }
    files
}

/// Makes the front end with the sample project and recovery in the phone's private storage.
pub fn front_end(root: Option<&Path>) -> RustGisApp {
    // Datum grids: the app's own folder, where downloaded ones are kept (#2032 A4);
    // a phone has no PROJ install to share.
    if root.is_some() {
        rustgis_engine::data::raster::crs::use_grid_folders();
    }
    let mut app = RustGisApp::with_sample();
    if let Some(root) = root {
        match FolderStore::open(&root.join("recovery")) {
            Ok(store) => app = app.with_edit_recovery(Box::new(store)),
            Err(e) => app.session.notify(i18n::fmt("Autosave couldn't keep a recovery copy: {error}", &[("error", &e)]), true),
        }
    } else {
        app.session.warn(tl!("This device gives RustGIS no folder of its own: settings, the tile cache and recovery copies are off."));
    }
    // As on the desktop: commands run on the window's thread must not hold it.
    app.session.in_place_wait = false;
    app.session.on_window_thread = true;
    app.reads_off_frame = true;
    if let Err(e) = app.start(&rustgis_ui_egui::Start { blank: Some(false), ..Default::default() }) {
        log::warn!("start-up: {e}");
        app.session.notify(e, true);
    }
    app
}

/// Runs `app` in the phone's window with the touch interface: `phone` (or why it can't be
/// reached), the files `pushed` to open once the window is up, and the window's `options`.
/// Returns when the window is gone (Android); on iOS it never returns.
pub fn run<P: Phone + 'static>(
    mut app: RustGisApp,
    phone: Result<P, String>,
    pushed: Vec<PathBuf>,
    options: eframe::NativeOptions,
) -> eframe::Result {
    let (inbox, refusals, asked) = (Inbox::default(), Refusals::default(), Asked::default());
    let refused_picker = Rc::new(std::cell::Cell::new(false));
    let kept = Rc::new(RefCell::new(None));
    let phone = match phone {
        Ok(phone) => {
            let phone = Rc::new(phone);
            use_picker(&mut app, &phone, &inbox, &refusals, &asked, &refused_picker, &kept);
            Some(phone)
        }
        Err(e) => {
            app.session.warn(i18n::fmt("{error}. Every button that asks for a file does nothing.", &[("error", &e)]));
            None
        }
    };
    // A phone gets the touch interface; its More sheet has the desktop one, whose View tab has
    // this one again.
    app.chrome = Some(rustgis_ui_touch::chrome);
    app.touch = app.chrome;
    app.session.mark_saved();
    eframe::run_native(
        "RustGIS",
        options,
        Box::new(move |cc| {
            app.enable_native_font_downloads(&cc.egui_ctx);
            Ok(Box::new(App {
                app,
                poll: None,
                locating: false,
                keeping: false,
                typing: None,
                keyboard: None,
                back: false,
                bars_light: None,
                pushed,
                phone,
                inbox,
                refusals,
                asked,
                kept,
                refused_picker,
                waiting: Vec::new(),
                problems: Problems::default(),
                reading: Arc::default(),
            }))
        }),
    )
}

/// Puts the text copied in the window on the system's clipboard; a copy the phone couldn't be
/// asked to take is said, as one it refused is ([`Event::CopyFailed`]).
fn copy_out(app: &mut RustGisApp, phone: &impl Phone, copied: Vec<String>) {
    for text in copied {
        if let Err(error) = phone.copy_text(&text) {
            copy_failed(app, &error);
        }
    }
}

/// Says that text copied in the window didn't reach the system's clipboard.
fn copy_failed(app: &mut RustGisApp, error: &str) {
    app.session.notify(
        i18n::fmt("The text copied wasn't put on the system's clipboard, so other apps can't paste it: {error}", &[("error", error)]),
        true,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::cell::Cell;

    /// A phone that is busy once asked for a file, and reports the events put in it.
    #[derive(Default)]
    struct FakePhone(Cell<bool>, RefCell<Vec<Event>>);
    impl Platform for FakePhone {
        fn pick_files(&self, _: &str) -> Result<(), String> {
            self.0.set(true);
            Ok(())
        }
        fn save_file(&self, _: &str, _: Vec<u8>, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn locate(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn keep_locating(&self, _: bool) -> Result<(), String> {
            Ok(())
        }
        fn take_photo(&self) -> Result<(), String> {
            Ok(())
        }
        fn take_events(&self) -> Vec<Event> {
            self.1.take()
        }
    }
    impl Phone for FakePhone {
        fn insets(&self, _: f32) -> Option<(egui::SafeAreaInsets, Option<bool>)> {
            None
        }
        fn bars_light(&self, _: bool, _: &eframe::Frame) {}
        fn busy(&self) -> bool {
            self.0.get()
        }
        fn idle_look(&self) -> Option<u64> {
            Some(5000)
        }
        fn copy_text(&self, text: &str) -> Result<(), String> {
            if text == "refused" { Err("no clipboard".into()) } else { Ok(()) }
        }
    }

    /// The phone's low-memory warnings reach the front end: the tile cache and the drawing copy
    /// of a layer switched off go (a drawn layer's stays), it is said once however often it
    /// comes, and an idle window looks again as often as the phone asks.
    #[test]
    fn a_low_memory_warning_drops_caches_and_is_said_once() {
        use rustgis_engine::render::rustgis_tiles::cache;

        let mut app = RustGisApp::with_sample();
        let layers = &mut app.session.map_mut().layers;
        assert!(layers.len() >= 2);
        layers[0].visible = false;
        layers[1].visible = true;
        for layer in layers.iter_mut().take(2) {
            layer.projected();
            assert!(layer.is_cached("mercator"));
        }
        let (clears, mark) = (cache::clears(), app.session.notice_mark());
        let phone = Rc::new(FakePhone::default());
        phone.1.borrow_mut().extend([Event::LowMemory, Event::LowMemory]);
        let mut window = App {
            app,
            pushed: Vec::new(),
            poll: None,
            locating: false,
            keeping: false,
            keyboard: None,
            typing: None,
            back: false,
            bars_light: None,
            phone: Some(phone),
            inbox: Inbox::default(),
            refusals: Refusals::default(),
            asked: Asked::default(),
            kept: Rc::default(),
            refused_picker: Rc::default(),
            waiting: Vec::new(),
            problems: Problems::default(),
            reading: Arc::default(),
        };
        window.take_phone_events(&egui::Context::default(), &mut egui::RawInput::default());
        assert!(cache::clears() > clears);
        let layers = &window.app.session.map().layers;
        assert_eq!([layers[0].is_cached("mercator"), layers[1].is_cached("mercator")], [false, true]);
        let said = window.app.session.notices_since(mark).iter().filter(|n| n.text.contains("low on memory")).count();
        assert_eq!(said, 1);
        assert_eq!(window.poll, Some(5000));
    }

    /// A copy the phone couldn't be asked to take is said once; one it took says nothing.
    #[test]
    fn a_copy_that_cannot_reach_the_clipboard_is_said() {
        let mut app = RustGisApp::with_sample();
        let before = app.session.notice_mark();
        copy_out(&mut app, &FakePhone::default(), vec!["taken".into()]);
        assert!(app.session.notices_since(before).is_empty());
        copy_out(&mut app, &FakePhone::default(), vec!["refused".into(), "taken".into()]);
        let said = app.session.notices_since(before);
        assert_eq!(said.len(), 1);
        assert!(said[0].text.contains("system's clipboard") && said[0].text.contains("no clipboard"));
    }

    #[test]
    fn same_frame_refused_picker_restores_the_options_of_the_admitted_request() {
        let mut app = RustGisApp::with_sample();
        let phone = Rc::new(FakePhone::default());
        let (inbox, refusals, asked) = (Inbox::default(), Refusals::default(), Asked::default());
        let refused = Rc::new(Cell::new(false));
        let kept = Rc::new(RefCell::new(None));
        use_picker(&mut app, &phone, &inbox, &refusals, &asked, &refused, &kept);
        let options = json!({"x": "longitude", "y": "latitude"});
        app.run("ui.addDelimited", options.clone()).unwrap();
        app.run("ui.addData", Value::Null).unwrap();
        assert!(refused.replace(false));
        kept.borrow().clone().unwrap().restore(&mut app);
        assert_eq!(app.ui.pending_csv, Some(options));
        // Refusing DXF while the accepted picker is up retires exactly its own request.
        app.run("ui.addDxf", Value::Null).unwrap();
        assert_eq!(inbox.lock().unwrap().len(), 1);
        assert!(inbox.lock().unwrap()[0].0.starts_with(rustgis_ui_egui::DXF_PICK));
        // So does a marker's «Image…» refused the same way: the marker stops waiting.
        (app.services.open_async.as_ref().unwrap())("marker:9");
        assert_eq!(inbox.lock().unwrap().last().unwrap().0, format!("{}9:", rustgis_ui_egui::MARKER_PICK));
    }
    /// A failed write-back says whether the file was changed, and offers Save again only while the
    /// project saved is still open; the app's own reasons are worded, the system's passed on.
    #[test]
    fn a_failed_write_back_is_worded_by_what_happened() {
        let kept = Path::new("/data/saved/1/site.rgis");
        let said = |kept: Option<&Path>, open| write_back_failure("site.rgis", "code:unwritable", kept, open);
        assert_eq!(
            said(None, true),
            "site.rgis wasn't saved where it was opened from: the file can't be written to. The file wasn't changed: Save again to choose where to save it."
        );
        assert!(said(None, false).ends_with("The file wasn't changed.") && !said(None, false).contains("Save again"));
        assert!(said(Some(kept), true).contains("may be damaged") && said(Some(kept), true).contains("Save again"));
        assert!(said(Some(kept), true).ends_with("kept at /data/saved/1/site.rgis."));
        assert!(said(Some(kept), false).contains("may be damaged") && !said(Some(kept), false).contains("Save again"));
        let busy = write_back_failure("site.rgis", "code:busy", None, true);
        assert_eq!(busy, "site.rgis wasn't saved: another file is being chosen or saved. Save again once it is done.");
        assert_eq!(
            write_back_failure("site.rgis", "code:busy", None, false),
            "site.rgis wasn't saved: another file is being chosen or saved."
        );
        for (code, worded) in [
            ("code:busy", "another file is being chosen or saved"),
            ("code:storage", "the app's storage can't be written to"),
            ("code:stalled", "the file took no more bytes"),
            ("code:short", "the file doesn't hold all of the project after writing it"),
            ("code:unfinished", "the app ended before it was written"),
            ("Permission denied", "Permission denied"),
        ] {
            assert_eq!(write_back_reason(code), worded);
        }
    }

    #[test]
    fn a_worker_refused_before_reading_retires_its_dxf_answer() {
        let inbox = Inbox::default();
        let reading = AtomicUsize::new(0);
        let result: std::io::Result<()> =
            start_reader(&inbox, &reading, vec![crate::mark("dxf:7")], || Err(std::io::Error::other("injected worker refusal")));
        assert!(result.is_err());
        assert_eq!(reading.load(Ordering::Relaxed), 0);
        assert_eq!(*inbox.lock().unwrap(), vec![(crate::mark("dxf:7"), Vec::<u8>::new())]);
    }

    /// A script picked for Load… over the pane's limit is refused before it is read, naming it; one
    /// within it reaches the inbox marked for the pane.
    #[test]
    fn a_script_over_the_limit_is_refused_before_it_is_read() {
        let dir = std::env::temp_dir().join(format!("rustgis-mobile-script-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (big, small) = (dir.join("big.txt"), dir.join("small.txt"));
        std::fs::write(&big, vec![b'#'; rustgis_ui_egui::SCRIPT_FILE_MAX_BYTES + 1]).unwrap();
        std::fs::write(&small, "map.zoomFull").unwrap();
        let (inbox, problems) = (Inbox::default(), Problems::default());
        let files = vec![("big.txt".to_string(), big), ("small.txt".to_string(), small)];
        read_batch("script", files, &inbox, &problems, &mut 0);
        let said = problems.lock().unwrap();
        assert!(matches!(said.as_slice(), [Problem::ScriptTooLarge { name }] if name == "big.txt"));
        let names: Vec<String> = inbox.lock().unwrap().iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names, [format!("{}small.txt", rustgis_ui_egui::SCRIPT_PICK)]);
        // The copies are gone, read or not, and their folder with them (`remove_copy`).
        assert!(!dir.exists());
    }

    /// An image picked for a marker over the marker's limit is refused before it is read; one
    /// within it reaches the inbox marked for the marker, and a pick with nothing read ends the
    /// marker's wait.
    #[test]
    fn a_marker_image_over_the_limit_is_refused_before_it_is_read() {
        let dir = std::env::temp_dir().join(format!("rustgis-mobile-marker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (big, small) = (dir.join("big.png"), dir.join("small.png"));
        std::fs::write(&big, vec![0; rustgis_ui_egui::MARKER_FILE_MAX_BYTES + 1]).unwrap();
        std::fs::write(&small, b"png").unwrap();
        let (inbox, problems) = (Inbox::default(), Problems::default());
        let files = vec![("big.png".to_string(), big), ("small.png".to_string(), small)];
        read_batch("marker:1", files, &inbox, &problems, &mut 0);
        assert!(matches!(problems.lock().unwrap().as_slice(), [Problem::MarkerTooLarge]));
        let names: Vec<String> = inbox.lock().unwrap().iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names, [format!("{}1:small.png", rustgis_ui_egui::MARKER_PICK)]);
        // Only the large one: nothing read, so the request is answered empty and stops waiting.
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big2.png");
        std::fs::write(&big, vec![0u8; rustgis_ui_egui::MARKER_FILE_MAX_BYTES + 1]).unwrap();
        let inbox = Inbox::default();
        read_batch("marker:2", vec![("big2.png".to_string(), big)], &inbox, &problems, &mut 0);
        assert_eq!(*inbox.lock().unwrap(), vec![(format!("{}2:", rustgis_ui_egui::MARKER_PICK), Vec::<u8>::new())]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A marker's request ended with no file (a cancelled picker) is answered empty with its
    /// mark, so the marker stops waiting; a request nothing waits on gets no answer.
    #[test]
    fn an_ended_marker_request_is_answered_empty() {
        let inbox = Inbox::default();
        answer_ended(&inbox, Some("marker:3".into()));
        answer_ended(&inbox, Some("data".into()));
        assert_eq!(*inbox.lock().unwrap(), vec![(format!("{}3:", rustgis_ui_egui::MARKER_PICK), Vec::<u8>::new())]);
    }
}
