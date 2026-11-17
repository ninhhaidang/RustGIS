//! What the Android and iOS apps share: the desktop front end in a window of the phone's, with
//! the phone ([`Phone`], a `rustgis_engine::platform::Platform`) standing where the browser
//! stands for the web app. Each system's module makes its [`Phone`] and calls [`run`].

use eframe::egui;
use rustgis_engine::platform::{Event, Platform};
use rustgis_ui_egui::{Inbox, RustGisApp, i18n, tl};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// How long the window keeps looking for the soft keyboard's room after a field last wanted
/// it: long enough for the keyboard to finish closing.
const KEYBOARD_SETTLES: std::time::Duration = std::time::Duration::from_millis(1500);

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

    /// Whether the soft keyboard's Return was pressed since the last frame, where the system
    /// passes it on as no key egui knows (iOS). The shell gives egui an Enter for it.
    fn take_return(&self) -> bool {
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
    /// The file `name` is over the size read into memory (`rustgis_data::io::MAX_BYTES`), by
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
                Event::Picked { purpose, name, path } => {
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
                Event::Failed(text) => self.app.session.notify(text, true),
            }
        }
        for problem in self.problems.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
            let text = match problem {
                Problem::ShortcutTooLarge { name } => {
                    let limit = rustgis_ui_egui::SHORTCUT_FILE_MAX_BYTES.to_string();
                    i18n::fmt("{name} wasn't opened: a shortcut file is at most {limit} bytes.", &[("name", &name), ("limit", &limit)])
                }
                Problem::TooLarge { name, together } => {
                    let limit = (rustgis_engine::data::io::MAX_BYTES >> 20).to_string();
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
            None
        };
    }

    /// Ends a request for files that got none (`purpose`): a dialog waiting for its answer
    /// (Import DXF) is answered empty, and stops waiting.
    fn end(&self, purpose: Option<String>) {
        if let Some(purpose) = purpose.filter(|p| p.starts_with("dxf:")) {
            self.inbox.lock().unwrap_or_else(|e| e.into_inner()).push((crate::mark(&purpose), Vec::new()));
        }
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
        let retire: Vec<_> =
            reads.iter().filter(|(purpose, _)| purpose.starts_with("dxf:")).map(|(purpose, _)| crate::mark(purpose)).collect();
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

/// A worker that cannot start answers its DXF requests just as a cancelled picker does.
fn retire_unread(inbox: &Inbox, marks: Vec<String>) {
    inbox.lock().unwrap_or_else(|e| e.into_inner()).extend(marks.into_iter().map(|mark| (mark, Vec::new())));
}

/// Reads one folder's files of a pick (name, copy) into `inbox`, marked for the dialog that
/// asked, and removes the copies: their bytes are all that was wanted of them. `held` counts the
/// bytes the pick has read so far, across its folders.
fn read_batch(purpose: &str, files: Vec<(String, PathBuf)>, inbox: &Inbox, problems: &Problems, held: &mut u64) {
    let mark = crate::mark(purpose);
    let say = |problem| problems.lock().unwrap_or_else(|e| e.into_inner()).push(problem);
    let mut read = Vec::new();
    for (name, path) in files {
        let shortcut = mark == rustgis_ui_egui::SHORTCUT_PICK || (mark.is_empty() && name.to_ascii_lowercase().ends_with(".rgisshortcuts"));
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if shortcut && size > rustgis_ui_egui::SHORTCUT_FILE_MAX_BYTES as u64 {
            say(Problem::ShortcutTooLarge { name });
        } else if held.saturating_add(size) > rustgis_engine::data::io::MAX_BYTES {
            // Refused before it is read: no more than the cap is held in memory at once, in
            // one file or in the files picked together.
            say(Problem::TooLarge { name, together: size <= rustgis_engine::data::io::MAX_BYTES });
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
    let (read, found) = rustgis_engine::data::io::gather_shapefiles(read);
    for text in found {
        say(Problem::Shapefile(text));
    }
    // A DXF request with nothing read is ended, as a cancelled one is: Import DXF stops waiting.
    if mark.starts_with(rustgis_ui_egui::DXF_PICK) && read.is_empty() {
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
        // A DXF request whose picker didn't show is ended now: Import DXF stops waiting.
        if let Some(purpose) = ended.filter(|p| p.starts_with("dxf:")) {
            answers.lock().unwrap_or_else(|e| e.into_inner()).push((crate::mark(&purpose), Vec::new()));
        }
    }));
    let saver = phone.clone();
    app.services.download = Some(Box::new(move |name: &str, bytes: Vec<u8>| saver.save_file(name, bytes)));
    app.services.inbox = Some(inbox.clone());
    let (camera, photographing) = (phone.clone(), asked.clone());
    app.services.take_photo = Some(Box::new(move || {
        camera.take_photo()?;
        // Its cancel, if it comes, is the camera's ([`crate::Requests::cancelled`]).
        photographing.borrow_mut().photo_asked();
        Ok(())
    }));
}

impl<P: Phone> eframe::App for App<P> {
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
        // A second picker asked for while one was up was refused: what the front end kept for
        // the first is put back, so its file is read as it was asked for.
        if self.refused_picker.replace(false)
            && let Some(kept) = self.kept.borrow().clone()
        {
            kept.restore(&mut self.app);
        }
        // Something of the phone's is still to arrive (a picked file, a position): look again.
        // A picker or a save asked for in this frame counts too: the poll was set before it.
        if self.poll.is_none() && self.phone.as_ref().is_some_and(|phone| phone.busy()) {
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
            for pressed in [true, false] {
                raw_input.events.push(egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
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

/// Makes the front end with the sample project, ready for a phone: commands run on the window's
/// thread don't hold it, and `folders` (whether the system gave the app a folder of its own) is
/// said when it is false.
pub fn front_end(folders: bool) -> RustGisApp {
    let mut app = RustGisApp::with_sample();
    if !folders {
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
        Box::new(move |_cc| {
            Ok(Box::new(App {
                app,
                poll: None,
                locating: false,
                keeping: false,
                typing: None,
                keyboard: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::cell::Cell;

    #[derive(Default)]
    struct FakePhone(Cell<bool>);
    impl Platform for FakePhone {
        fn pick_files(&self, _: &str) -> Result<(), String> {
            self.0.set(true);
            Ok(())
        }
        fn save_file(&self, _: &str, _: Vec<u8>) -> Result<(), String> {
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
            Vec::new()
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
}
