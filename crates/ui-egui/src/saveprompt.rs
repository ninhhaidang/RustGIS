//! Asks to save unsaved changes before the interface discards them (#906): quitting, closing the
//! window, or replacing the project (New, Open, the World Atlas sample, a template, a project
//! file dropped on the window). Save, Don't Save or Cancel.
//!
//! Commands never ask: `project.new` and the rest run as before for the CLI, scripts, the AI
//! Assistant and attached agents. What the user does in the interface goes through
//! [`RustGisApp::run_by_user`], which asks first. The desktop app asks, and so does a phone's
//! shell before a project is replaced; the browser warns before its page is left instead, and
//! saves by downloading, so it doesn't.

use egui::vec2;
use serde_json::Value;

use crate::RustGisApp;
use crate::theme::{Tokens, semibold};

/// The commands that replace the open project.
const REPLACES: &[&str] = &["project.new", "project.sample", "project.open", "project.openText", "project.fromTemplate"];

/// What waits for the user's answer.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Discard {
    /// A command replacing the project, with its parameters.
    Replace(String, Value),
    /// Quitting: the window closes once the answer allows it.
    Quit,
}

#[derive(Default)]
pub(crate) struct SavePromptState {
    /// What the dialog asks about, while it shows.
    pub asking: Option<Discard>,
    /// The user chose to quit: the next close request goes through.
    pub closing: bool,
    /// The project's state when asked ([`rustgis_engine::Session::state`]): Don't Save discards
    /// the changes the user was asked about, not ones an agent made while the dialog showed.
    pub asked: u64,
    /// The project changed while the dialog showed: it asks again, and says why.
    pub changed: bool,
    pending: Option<PendingSave>,
    /// The documents picked projects were copied from, by the copy's path, that a phone may write
    /// back to ([`RustGisApp::picked_document`]).
    documents: std::collections::HashMap<std::path::PathBuf, String>,
    /// Where Save puts each project ([`rustgis_engine::Session::file_identity`], which Undo puts
    /// back with it) once a save has decided it: the document a picker save wrote, which Save
    /// writes back to from then on, or `None` for the picker (a picker save the phone can't write
    /// back to, or a write-back that failed). A project with none here is written back to the
    /// document it was opened from. With each document, whether it is that one (its address found
    /// again after it moved).
    targets: std::collections::HashMap<u64, Option<(String, bool)>>,
    /// The document a picker save reported, until its result comes.
    reported: Option<String>,
}

struct PendingSave {
    snapshot: rustgis_engine::platform::SaveSnapshot,
    after: Option<Discard>,
    /// The project saved ([`rustgis_engine::Session::file_identity`]).
    project: u64,
    /// The document written back to, or `None` for a picker save.
    document: Option<String>,
    /// Whether that document is the one the project was opened from.
    opened_from: bool,
    /// What the save loses of the project's constructions, said once it is written.
    notes: Vec<rustgis_engine::data::loss::Note>,
}

/// The document the open project is written back to on Save, and whether it is the one the project
/// was opened from: where its last save decided, or else the one it was opened from. A field
/// package is never written over by Save: it goes back with Send Field Package.
fn document(app: &RustGisApp) -> Option<(String, bool)> {
    let prompt = &app.save_prompt;
    if let Some(target) = prompt.targets.get(&app.session.file_identity()) {
        return target.clone();
    }
    if app.session.opened_package().is_some() {
        return None;
    }
    // Only a project file: a package opened (`.rgispkg`) isn't written over by its project's text.
    let project = app.session.path.as_ref().filter(|p| rustgis_engine::project::is_project_file(&p.to_string_lossy()));
    project.and_then(|path| prompt.documents.get(path)).map(|document| (document.clone(), true))
}

/// Whether running command `id` from the interface asks first: it replaces the project, which
/// has unsaved changes, in the desktop app or in a shell that can confirm a save.
pub(crate) fn asks(app: &RustGisApp, id: &str) -> bool {
    REPLACES.contains(&id) && app.session.dirty() && (app.opens_files() || app.services.save_project.is_some())
}

/// Sends the same portable project text as the download path: written back to the project's
/// document (`in_place`, where it has one), or offered to the picker, which supplies the final file
/// name, so the request leaves the live project name alone.
pub(crate) fn start_project_save(app: &mut RustGisApp, in_place: bool) -> Result<(), String> {
    if app.save_prompt.pending.is_some() {
        return Err(tl!("A project save is already in progress.").into());
    }
    let (target, opened_from) = match document(app).filter(|_| in_place) {
        Some((document, opened_from)) => (Some(document), opened_from),
        None => (None, false),
    };
    let save = app.services.save_project.as_ref().ok_or(tl!("This build cannot save through the native picker."))?;
    let name = app.session.project.name.replace(['/', '\\'], "-");
    // Written back to the file it was opened from, it is named by that file; otherwise by the
    // project, as the picker offers it.
    let opened_file = app.session.path.as_deref().filter(|_| opened_from).and_then(std::path::Path::file_name);
    let file = opened_file.map_or_else(|| format!("{name}.rgis"), |f| f.to_string_lossy().into_owned());
    let notes = app.session.construction_output_notes()?;
    // Written back to the document it was opened from, its paths are written relative to the folder
    // of the copy it was opened from: a file linked beside the original (`./terrain.tif`) stays so,
    // where the original is. A picker save, and a write-back to a document saved as since (whose
    // folder isn't the copy's), keep them whole, as the download does.
    let copy_dir = app.session.path.as_deref().filter(|_| opened_from).and_then(std::path::Path::parent).map(std::path::Path::to_path_buf);
    let (text, report) = app.session.project.saved_text(copy_dir.as_deref())?;
    // Refused before it started (the phone busy with another file): nothing touched the document,
    // so Save still writes back there.
    save(&file, text.into_bytes(), target.as_deref())?;
    app.save_prompt.reported = None;
    app.save_prompt.pending = Some(PendingSave {
        snapshot: app.session.save_snapshot(),
        after: None,
        project: app.session.file_identity(),
        document: target,
        opened_from,
        notes,
    });
    report.notify(&mut app.session);
    Ok(())
}

impl RustGisApp {
    /// Consumes the native picker's result. Only the snapshot that reached the file is saved.
    /// A failed or cancelled write abandons any action that was waiting for it.
    pub fn complete_project_save(&mut self, result: Result<String, String>) {
        self.finish_project_save(result, true);
    }

    /// A save the phone turned down before touching anything (it was busy with another file):
    /// said as a failed save, while Save keeps writing back where it did.
    pub fn project_save_refused(&mut self, error: String) {
        self.finish_project_save(Err(error), false);
    }

    /// [`RustGisApp::complete_project_save`]; a failed write-back makes the next Save ask where
    /// only when it may have reached the document (`reached`).
    fn finish_project_save(&mut self, result: Result<String, String>, reached: bool) {
        let pending = self.save_prompt.pending.take();
        let reported = self.save_prompt.reported.take();
        match result {
            Ok(name) => self.session.notify(crate::i18n::fmt("Saved {name}.", &[("name", &name)]), false),
            Err(error) => {
                self.session.notify(error, true);
                // A failed save that may have written its file (a write-back that reached its
                // document, or a picker that reported where it wrote): what that loses of the
                // constructions is said too, as the file may hold it. A cancelled picker wrote nothing.
                if let Some(p) = pending.as_ref().filter(|p| (p.document.is_some() && reached) || reported.is_some()) {
                    for note in p.notes.clone() {
                        self.session.say(note);
                    }
                }
                // A document that couldn't be written back is asked for again: the next Save
                // shows the picker. A picker closed with no choice changes nothing.
                if let Some(failed) = pending.filter(|p| p.document.is_some() && reached) {
                    self.save_prompt.targets.insert(failed.project, None);
                }
                return;
            }
        }
        let Some(saved) = pending else { return };
        // Written: what it lost of the constructions is said now, and only now.
        for note in saved.notes {
            self.session.say(note);
        }
        // Saved through the picker: Save writes back to the document it wrote from now on, or
        // asks again where the phone can't write it back; never to the one before.
        // A write-back that reports its document again (iOS made its bookmark again, the file having
        // moved) writes there by the new one from now on.
        if saved.document.is_none() {
            self.save_prompt.targets.insert(saved.project, reported.map(|document| (document, false)));
        } else if let Some(fresh) = reported {
            self.save_prompt.targets.insert(saved.project, Some((fresh, saved.opened_from)));
        }
        if self.session.mark_snapshot_saved(saved.snapshot)
            && let Some(what) = saved.after
        {
            if self.session.dirty() {
                ask(self, what);
                self.save_prompt.changed = true;
            } else {
                proceed(self, what);
            }
        }
    }
}

impl RustGisApp {
    /// A picked project's copy at `path` came from `document`, which the phone may write back to:
    /// Save writes there while that copy is the open project.
    pub fn picked_document(&mut self, path: std::path::PathBuf, document: String) {
        self.save_prompt.documents.insert(path, document);
    }

    /// Whether the save under way is of the project open now: a failure's advice to Save again
    /// is for it alone.
    pub fn saving_the_open_project(&self) -> bool {
        self.save_prompt.pending.as_ref().is_some_and(|p| p.project == self.session.file_identity())
    }

    /// The save under way wrote `document`, which the phone may write back to (a picker save's, or
    /// a write-back's document found again under a new address): its
    /// [`RustGisApp::complete_project_save`] follows.
    pub fn project_document(&mut self, document: String) {
        self.save_prompt.reported = Some(document);
    }
}

/// Whether the project has unsaved changes in a shell that asks about them (the desktop app).
fn unsaved_here(app: &RustGisApp) -> bool {
    app.session.dirty() && app.opens_files()
}

/// Shows the dialog for `what`, about the project as it is now. One already showing keeps its
/// question (the dialog takes the pointer and the keys; a drop that opens a project waits with
/// the rest of its files). Quitting does replace it, as the user asked for it last.
pub(crate) fn ask(app: &mut RustGisApp, what: Discard) {
    if app.save_prompt.asking.is_none() || what == Discard::Quit {
        app.save_prompt.asking = Some(what);
        app.save_prompt.asked = app.session.state();
        app.save_prompt.changed = false;
    }
}

/// Whether the dialog is waiting for an answer: what asked it waits too (the Project tab stays
/// open, so Cancel goes back to it; it closes once the project is replaced).
pub(crate) fn waiting(app: &RustGisApp) -> bool {
    app.save_prompt.asking.is_some() || app.save_prompt.pending.as_ref().is_some_and(|p| p.after.is_some())
}

/// Whether the app ends with unsaved changes the user hasn't agreed to lose: the project has
/// some, this shell asks, and the dialog wasn't answered with Don't Save (or Save, which leaves
/// none). Only an end that skipped the close request gets here: macOS's Quit.
pub(crate) fn unanswered(app: &RustGisApp) -> bool {
    unsaved_here(app) && !app.save_prompt.closing
}

/// Each frame, before anything draws: a request to close the window (its close button, the
/// system's Quit, or `ui.quit`) waits while the project has unsaved changes, and the dialog asks.
pub(crate) fn close_requested(app: &mut RustGisApp, ctx: &egui::Context) {
    if !ctx.input(|i| i.viewport().close_requested()) || app.save_prompt.closing {
        return;
    }
    if !unsaved_here(app) {
        return;
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
    // `ui.quit` asked the shell to close: it would ask again each frame.
    app.quit_requested = false;
    ask(app, Discard::Quit);
}

/// The dialog: Save (then go on, once the project is saved), Don't Save (go on), or Cancel.
pub(crate) fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(what) = app.save_prompt.asking.clone() else { return };
    let t = Tokens::get(ctx);
    let name = &app.session.project.name;
    let name = if name.trim().is_empty() { tl_raw!("Untitled").to_string() } else { name.clone() };
    let mut answer = None;
    let frame = egui::Frame::popup(&ctx.global_style());
    let width = crate::widgets::dialog_width(ctx, 380.0, frame);
    egui::Modal::new(egui::Id::unique("save_prompt")).frame(frame).show(ctx, |ui| {
        ui.set_width(width);
        ui.label(egui::RichText::new(crate::i18n::fmt("Save changes to {name}?", &[("name", &name)])).font(semibold(15.0)));
        ui.add_space(4.0);
        ui.label(tl!("Your changes will be lost if you don't save them."));
        if app.save_prompt.changed {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tl!("The project changed while this was open: check the changes, then answer again.")).color(t.red),
            );
        }
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min).with_main_wrap(true), |ui| {
            if crate::widgets::dialog_button(ui, tl!("Save"), Some(t.accent), vec2(84.0, 28.0)).clicked() {
                answer = Some(Answer::Save);
            }
            if crate::widgets::dialog_button(ui, tl!("Don't Save"), None, vec2(84.0, 28.0)).clicked() {
                answer = Some(Answer::DontSave);
            }
            if crate::widgets::dialog_button(ui, tl!("Cancel"), None, vec2(84.0, 28.0)).clicked()
                || ui.input(|i| i.key_pressed(egui::Key::Escape))
            {
                answer = Some(Answer::Cancel);
            }
        });
    });
    let Some(answer) = answer else { return };
    // Changed since the question (an agent, the AI Assistant): Don't Save would discard changes
    // the user hasn't seen. The dialog stays and asks about the project as it is now.
    if matches!(answer, Answer::DontSave) && app.session.state() != app.save_prompt.asked {
        app.save_prompt.asked = app.session.state();
        app.save_prompt.changed = true;
        return;
    }
    app.save_prompt.asking = None;
    // The shell closes the window, or the replaced project draws, on the next frame.
    ctx.request_repaint();
    match answer {
        Answer::Save => save_then(app, what),
        Answer::DontSave => proceed(app, what),
        Answer::Cancel => {}
    }
}

#[derive(Clone, Copy)]
enum Answer {
    Save,
    DontSave,
    Cancel,
}

/// The desktop returns from Save after writing. A phone's pending save keeps the continuation.
fn save_then(app: &mut RustGisApp, what: Discard) {
    if let Err(error) = app.run("ui.save", Value::Null) {
        // A phone's native save can fail before any picker shows, and nothing else says so; the
        // desktop's save reports its own failures.
        if app.services.save_project.is_some() {
            app.session.notify(error, true);
        }
        return;
    }
    if let Some(pending) = &mut app.save_prompt.pending {
        pending.after = Some(what);
    } else if !app.session.dirty() {
        proceed(app, what);
    }
}

/// Does what waited for the answer. Quitting removes the recovery copy as a clean exit does
/// (`on_exit`); a replacement leaves no unsaved changes, so autosave removes it too.
fn proceed(app: &mut RustGisApp, what: Discard) {
    match what {
        Discard::Replace(id, params) => {
            let _ = app.run(&id, params);
            app.ui.backstage = false;
            app.map.invalidate();
        }
        Discard::Quit => {
            app.save_prompt.closing = true;
            app.quit_requested = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::PICKED;
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    use serde_json::json;
    use std::path::PathBuf;

    type Harness = egui_kittest::Harness<'static, RustGisApp>;

    /// The desktop app on the World Atlas sample, never saved, drawing its frames.
    fn launch() -> Harness {
        let mut app = RustGisApp::with_sample();
        app.start(&Default::default()).unwrap();
        egui_kittest::Harness::builder().with_size(vec2(1400.0, 860.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        )
    }

    /// A folder of its own for test `name`, emptied first.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustgis-save-prompt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// An unsaved change: the map renamed (a new project has no layers to change).
    fn edit(h: &mut Harness, name: &str) {
        h.state_mut().run("map.rename", json!({"name": name})).unwrap();
        assert!(h.state().session.dirty());
    }

    /// Runs `n` frames: a click is handled on the frame after it.
    fn frames(h: &mut Harness, n: usize) {
        for _ in 0..n {
            h.step();
        }
    }

    /// Whether the dialog shows.
    fn asking(h: &Harness) -> bool {
        h.query_by_label_contains("Save changes to").is_some()
    }

    /// Clicks the dialog's button `label` and lets the answer take effect.
    fn click(h: &mut Harness, label: &str) {
        h.get_by_role_and_label(Role::Button, label).click();
        frames(h, 3);
    }

    /// A file dropped on the window, as the desktop shell passes it: by its path.
    #[derive(Debug)]
    struct Dropped(PathBuf);

    impl egui::DroppedFile for Dropped {
        /// Where the file is.
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        /// The file's contents, read when asked.
        fn bytes(&self) -> Result<Vec<u8>, String> {
            std::fs::read(&self.0).map_err(|e| e.to_string())
        }
    }

    /// The window's close button, or the system's Quit: what eframe passes the app.
    fn close_window(h: &mut Harness) {
        h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
        h.step();
    }

    /// Whether the last frame held back the window's close (`CancelClose`).
    fn close_cancelled(h: &Harness) -> bool {
        h.output().viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.commands.contains(&egui::ViewportCommand::CancelClose))
    }

    /// A picker save of a project holding a circle says what the save loses of it (drawn as straight
    /// edges, with the code) once the save completes, or when it failed after the picker wrote; a
    /// cancelled or refused picker says nothing of it.
    #[test]
    fn a_picker_save_says_its_construction_losses_only_once_written() {
        use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType, loss::LossCode};
        let said = |h: &Harness, mark: u64| {
            h.state()
                .session
                .notices_since(mark)
                .iter()
                .filter(|n| n.text.contains("circle(s)") && n.loss() == Some(LossCode::ApproximatedCurve))
                .count()
        };
        for (finish, expected) in [("ok", 1), ("cancelled", 0), ("wrote, then failed", 1), ("refused", 0)] {
            let mut h = launch();
            h.state_mut().services.save_project = Some(Box::new(|_, _, _| Ok(())));
            let id = h.state_mut().session.map_mut().add_dataset(Dataset::new(
                "Rings",
                GeometryType::Polygon,
                vec![Field::new("n", FieldType::Text)],
            ));
            h.state_mut().session.active_layer = Some(id);
            h.state_mut().run("edit.enable", json!({"on": true})).unwrap();
            h.state_mut().run("edit.addCircle", json!({"circle": {"center": [1000.0, 1000.0], "radius": 100.0}})).unwrap();
            let mark = h.state().session.notice_mark();
            // New Project asks about the unsaved circle; its Save opens the picker.
            h.state_mut().run_by_user("project.new", Value::Null).unwrap();
            frames(&mut h, 2);
            click(&mut h, "Save");
            assert_eq!(said(&h, mark), 0, "{finish}: nothing said before the save completes");
            match finish {
                "ok" => h.state_mut().complete_project_save(Ok("World Atlas.rgis".into())),
                "cancelled" => h.state_mut().complete_project_save(Err("Picker cancelled".into())),
                "wrote, then failed" => {
                    h.state_mut().project_document("content://saved/World Atlas.rgis".into());
                    h.state_mut().complete_project_save(Err("couldn't confirm the save".into()));
                }
                _ => h.state_mut().project_save_refused("busy".into()),
            }
            assert_eq!(said(&h, mark), expected, "{finish}: {:?}", h.state().session.notices_since(mark));
        }
    }

    #[test]
    fn project_save_dialogs_fit_a_narrow_viewport() {
        let mut h = launch();
        h.state_mut().touch = Some(RustGisApp::map_view);
        h.state_mut().chrome = Some(RustGisApp::map_view);
        h.state_mut().services.download = Some(Box::new(|_, _| Ok(())));
        h.set_size(vec2(360.0, 760.0));
        edit(&mut h, "Edited");
        h.state_mut().run_by_user("project.new", Value::Null).unwrap();
        frames(&mut h, 5);
        for label in ["Save", "Don't Save", "Cancel"] {
            let button = h.get_by_role_and_label(Role::Button, label);
            assert!(h.ctx.content_rect().contains_rect(button.rect()), "{label}: {:?}", button.rect());
        }
        click(&mut h, "Cancel");
        h.state_mut().run("ui.saveAs", Value::Null).unwrap();
        for height in [760.0, 360.0] {
            h.set_size(vec2(360.0, height));
            frames(&mut h, 5);
            let label = h.get_by_label("File name");
            let save = h.get_by_role_and_label(Role::Button, "Save");
            for rect in [label.rect(), save.rect()] {
                assert!(h.ctx.content_rect().contains_rect(rect), "{rect:?} outside viewport");
            }
        }
    }

    /// Closing the window with unsaved changes waits for an answer: Cancel keeps the app open
    /// with the changes, Don't Save closes it, and so does `ui.quit` once answered.
    #[test]
    fn quitting_asks_and_each_answer_does_what_it_says() {
        let mut h = launch();
        frames(&mut h, 2);
        // Nothing unsaved: the window closes without a word.
        close_window(&mut h);
        assert!(!close_cancelled(&h) && !asking(&h));
        edit(&mut h, "Edited");
        close_window(&mut h);
        assert!(close_cancelled(&h), "the close waits");
        frames(&mut h, 2);
        assert!(asking(&h) && h.query_by_label("Save changes to World Atlas?").is_some());
        click(&mut h, "Cancel");
        assert!(!asking(&h) && !h.state().quit_requested && h.state().session.dirty());
        // Escape is Cancel too.
        close_window(&mut h);
        frames(&mut h, 2);
        h.key_press(egui::Key::Escape);
        frames(&mut h, 2);
        assert!(!asking(&h) && !h.state().quit_requested);
        // `ui.quit` asks the shell to close, which comes back as a close request: it asks once,
        // and stops asking the shell meanwhile.
        h.state_mut().run("ui.quit", Value::Null).unwrap();
        close_window(&mut h);
        assert!(close_cancelled(&h) && !h.state().quit_requested);
        frames(&mut h, 2);
        click(&mut h, "Don't Save");
        assert!(h.state().quit_requested && !asking(&h));
        close_window(&mut h);
        assert!(!close_cancelled(&h), "the window closes");
    }

    /// Save on a project never saved asks where to write it: cancelling that keeps the project
    /// and its changes; a path picked saves it, then quits.
    #[test]
    fn save_on_a_new_project_saves_as_first() {
        let dir = folder("save-as");
        let mut h = launch();
        frames(&mut h, 2);
        edit(&mut h, "Edited");
        close_window(&mut h);
        frames(&mut h, 2);
        PICKED.with(|p| *p.borrow_mut() = None);
        click(&mut h, "Save");
        assert!(!h.state().quit_requested && h.state().session.dirty() && !asking(&h), "Save As cancelled: nothing is discarded");
        let file = dir.join("atlas.rgis");
        close_window(&mut h);
        frames(&mut h, 2);
        PICKED.with(|p| *p.borrow_mut() = Some(file.display().to_string()));
        click(&mut h, "Save");
        assert!(file.exists() && !h.state().session.dirty() && h.state().quit_requested);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A save that fails discards nothing: the project and its changes stay, and it says why.
    #[test]
    fn a_failed_save_discards_nothing() {
        let dir = folder("failed");
        let mut h = launch();
        frames(&mut h, 2);
        edit(&mut h, "Edited");
        // Its file is in a folder that has gone.
        h.state_mut().session.path = Some(dir.join("gone").join("atlas.rgis"));
        h.state_mut().run_by_user("project.new", Value::Null).unwrap();
        frames(&mut h, 2);
        assert!(asking(&h));
        click(&mut h, "Save");
        let app = h.state();
        assert!(app.session.dirty() && app.session.project.name == "World Atlas" && app.session.map().name == "Edited");
        assert!(app.session.notices.iter().any(|n| n.error), "the failure is said");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The native picker answers later. Only its successful project write lets New proceed.
    #[test]
    fn native_project_save_resumes_replacement_after_success() {
        let mut h = launch();
        h.state_mut().services.open_async = Some(Box::new(|_| {}));
        h.state_mut().services.save_project = Some(Box::new(|name, bytes, _| {
            assert_eq!(name, "World Atlas.rgis");
            assert!(String::from_utf8(bytes).unwrap().contains("\"Edited\""));
            Ok(())
        }));
        edit(&mut h, "Edited");
        for result in [Err("Picker cancelled".into()), Ok("World Atlas.rgis".into())] {
            h.state_mut().run_by_user("project.new", Value::Null).unwrap();
            frames(&mut h, 2);
            click(&mut h, "Save");
            assert!(waiting(h.state()) && h.state().session.dirty());
            assert_eq!(h.state().session.project.name, "World Atlas");
            h.state_mut().complete_project_save(result);
            assert!(!waiting(h.state()));
        }
        assert!(!h.state().session.dirty());
        assert_eq!(h.state().session.project.name, "Untitled");
    }

    /// A phone save refused before its picker shows (here, one already waiting) is said, and the
    /// project stays as it was.
    #[test]
    fn native_project_save_refused_before_the_picker_is_said() {
        let mut h = launch();
        h.state_mut().services.open_async = Some(Box::new(|_| {}));
        h.state_mut().services.save_project = Some(Box::new(|_, _, _| Err("No picker".into())));
        edit(&mut h, "Edited");
        h.state_mut().run_by_user("project.new", Value::Null).unwrap();
        frames(&mut h, 2);
        let mark = h.state().session.notice_mark();
        click(&mut h, "Save");
        let app = h.state();
        assert!(!waiting(app) && app.session.dirty() && app.session.map().name == "Edited");
        assert!(app.session.notices_since(mark).iter().any(|n| n.error && n.text.contains("No picker")), "the failure is said");
    }

    /// On a phone, Save writes back to the document the project was opened from, and Save As asks
    /// where. A picker save decides where Save goes next: the document it wrote where the phone may
    /// write it back, else the picker, never the document before. A write-back that failed asks
    /// again. Each project keeps its own, through Undo across a replacement.
    #[test]
    fn native_save_writes_back_to_the_opened_document() {
        use std::{cell::RefCell, rc::Rc};
        let mut h = launch();
        let asked: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let seen = asked.clone();
        h.state_mut().services.save_project = Some(Box::new(move |_, _, document| {
            seen.borrow_mut().push(document.map(str::to_string));
            Ok(())
        }));
        let copy = std::path::PathBuf::from("/picked/1/0/site.rgis");
        h.state_mut().picked_document(copy.clone(), "content://site".into());
        h.state_mut().session.path = Some(copy.clone());
        // Saves with an edit before each, answered with `result`; a picker save may report the
        // document it wrote first.
        let save = |h: &mut Harness, id: &str, reported: Option<&str>, result: Result<String, String>| {
            edit(h, id);
            h.state_mut().run(id, Value::Null).unwrap();
            if let Some(document) = reported {
                h.state_mut().project_document(document.into());
            }
            h.state_mut().complete_project_save(result);
        };
        let last = |asked: &Rc<RefCell<Vec<Option<String>>>>| asked.borrow().last().cloned().flatten();
        save(&mut h, "ui.save", None, Ok("site.rgis".into()));
        assert_eq!(last(&asked).as_deref(), Some("content://site"));
        // The write back failed: the next Save asks where.
        save(&mut h, "ui.save", None, Err("site.rgis may be damaged".into()));
        save(&mut h, "ui.save", None, Err("no place was chosen".into()));
        assert_eq!(last(&asked), None);
        // Saved as a document the phone may write back to: Save writes there.
        save(&mut h, "ui.saveAs", Some("content://other"), Ok("other.rgis".into()));
        save(&mut h, "ui.save", None, Ok("other.rgis".into()));
        assert_eq!(last(&asked).as_deref(), Some("content://other"));
        // Saved as one it can't: Save asks, and never goes back to the one before.
        save(&mut h, "ui.saveAs", None, Ok("elsewhere.rgis".into()));
        save(&mut h, "ui.save", None, Ok("elsewhere.rgis".into()));
        assert_eq!(last(&asked), None);
        save(&mut h, "ui.saveAs", Some("content://kept"), Ok("kept.rgis".into()));
        // Another project (opened by a script's step, which Undo goes back across), saved as its
        // own document; Undo brings back the first, and its own.
        h.state_mut().run("script.run", json!({"script": "project.new {}"})).unwrap();
        assert_eq!(h.state().session.path, None);
        h.state_mut().run("ui.saveAs", Value::Null).unwrap();
        h.state_mut().project_document("content://fresh".into());
        h.state_mut().complete_project_save(Ok("fresh.rgis".into()));
        h.state_mut().run("ui.save", Value::Null).unwrap();
        h.state_mut().complete_project_save(Ok("fresh.rgis".into()));
        assert_eq!(last(&asked).as_deref(), Some("content://fresh"));
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert_eq!(h.state().session.path.as_deref(), Some(copy.as_path()));
        h.state_mut().run("ui.save", Value::Null).unwrap();
        h.state_mut().complete_project_save(Ok("kept.rgis".into()));
        assert_eq!(last(&asked).as_deref(), Some("content://kept"));
    }

    /// A write-back to the file the project was opened from is named by that file; Save As is
    /// named by the project, as the picker offers it.
    #[test]
    fn a_write_back_is_named_by_its_file() {
        use std::{cell::RefCell, rc::Rc};
        let mut h = launch();
        let names: Rc<RefCell<Vec<String>>> = Rc::default();
        let seen = names.clone();
        h.state_mut().services.save_project = Some(Box::new(move |name, _, _| {
            seen.borrow_mut().push(name.to_string());
            Ok(())
        }));
        let copy = std::path::PathBuf::from("/picked/5/0/site.rgis");
        h.state_mut().picked_document(copy.clone(), "content://site".into());
        h.state_mut().session.path = Some(copy);
        h.state_mut().run("ui.save", Value::Null).unwrap();
        h.state_mut().complete_project_save(Ok("site.rgis".into()));
        h.state_mut().run("ui.saveAs", Value::Null).unwrap();
        assert_eq!(*names.borrow(), ["site.rgis", "World Atlas.rgis"]);
    }

    /// A write-back the phone turns down while busy with another file leaves Save writing back
    /// where it did.
    #[test]
    fn a_write_back_turned_down_while_busy_keeps_its_document() {
        use std::{cell::RefCell, rc::Rc};
        let mut h = launch();
        let asked: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let seen = asked.clone();
        h.state_mut().services.save_project = Some(Box::new(move |_, _, document| {
            seen.borrow_mut().push(document.map(str::to_string));
            Ok(())
        }));
        let copy = std::path::PathBuf::from("/picked/3/0/site.rgis");
        h.state_mut().picked_document(copy.clone(), "content://site".into());
        h.state_mut().session.path = Some(copy);
        h.state_mut().run("ui.save", Value::Null).unwrap();
        h.state_mut().project_save_refused("site.rgis wasn't saved: another file is being chosen or saved.".into());
        h.state_mut().run("ui.save", Value::Null).unwrap();
        assert_eq!(*asked.borrow(), [Some("content://site".to_string()), Some("content://site".to_string())]);
    }

    /// A write-back that finds its document under a new address (iOS makes its bookmark again)
    /// writes there by the new one from then on.
    #[test]
    fn a_write_back_that_finds_its_document_again_keeps_the_new_address() {
        use std::{cell::RefCell, rc::Rc};
        let mut h = launch();
        let asked: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let seen = asked.clone();
        h.state_mut().services.save_project = Some(Box::new(move |_, _, document| {
            seen.borrow_mut().push(document.map(str::to_string));
            Ok(())
        }));
        let copy = std::path::PathBuf::from("/picked/4/0/site.rgis");
        h.state_mut().picked_document(copy.clone(), "bookmark-old".into());
        h.state_mut().session.path = Some(copy);
        h.state_mut().run("ui.save", Value::Null).unwrap();
        h.state_mut().project_document("bookmark-new".into());
        h.state_mut().complete_project_save(Ok("site.rgis".into()));
        h.state_mut().run("ui.save", Value::Null).unwrap();
        assert_eq!(*asked.borrow(), [Some("bookmark-old".to_string()), Some("bookmark-new".to_string())]);
    }

    /// A write-back the phone refuses before it starts (busy with another file) touched nothing:
    /// the next Save writes back there again.
    #[test]
    fn a_write_back_refused_before_it_starts_keeps_its_document() {
        use std::{cell::RefCell, rc::Rc};
        let mut h = launch();
        let asked: Rc<RefCell<Vec<Option<String>>>> = Rc::default();
        let seen = asked.clone();
        h.state_mut().services.save_project = Some(Box::new(move |_, _, document| {
            seen.borrow_mut().push(document.map(str::to_string));
            if document.is_some() { Err("the phone can't reach it".into()) } else { Ok(()) }
        }));
        let copy = std::path::PathBuf::from("/picked/2/0/site.rgis");
        h.state_mut().picked_document(copy.clone(), "content://site".into());
        h.state_mut().session.path = Some(copy);
        assert!(h.state_mut().run("ui.save", Value::Null).is_err());
        assert!(h.state_mut().run("ui.save", Value::Null).is_err());
        assert_eq!(*asked.borrow(), [Some("content://site".to_string()), Some("content://site".to_string())]);
    }

    /// Written back, a project names a file beside its copy as beside itself (`./…`), so the
    /// original still finds a file linked beside it; a picker save keeps the path whole.
    #[test]
    fn native_write_back_keeps_links_beside_the_project_relative() {
        use std::{cell::RefCell, rc::Rc};
        let dir = folder("write-back-links");
        let roads = dir.join("roads.geojson");
        std::fs::write(
            &roads,
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
        )
        .unwrap();
        let mut h = launch();
        let written: Rc<RefCell<Vec<(Option<String>, String)>>> = Rc::default();
        let seen = written.clone();
        h.state_mut().services.save_project = Some(Box::new(move |_, bytes, document| {
            seen.borrow_mut().push((document.map(str::to_string), String::from_utf8(bytes).unwrap()));
            Ok(())
        }));
        h.state_mut().run("layer.addData", json!({"path": roads})).unwrap();
        let copy = dir.join("site.rgis");
        h.state_mut().picked_document(copy.clone(), "content://site".into());
        h.state_mut().session.path = Some(copy);
        h.state_mut().run("ui.save", Value::Null).unwrap();
        // The original found again under a new address (iOS's bookmark made again).
        h.state_mut().project_document("content://site-moved".into());
        h.state_mut().complete_project_save(Ok("site.rgis".into()));
        h.state_mut().run("ui.save", Value::Null).unwrap();
        h.state_mut().complete_project_save(Ok("site.rgis".into()));
        h.state_mut().run("ui.saveAs", Value::Null).unwrap();
        h.state_mut().project_document("content://copy".into());
        h.state_mut().complete_project_save(Ok("copy.rgis".into()));
        // Written back to the document saved as, whose folder isn't the copy's: whole.
        h.state_mut().run("ui.save", Value::Null).unwrap();
        h.state_mut().complete_project_save(Ok("copy.rgis".into()));
        let written = written.borrow();
        let whole = serde_json::to_string(&roads.display().to_string()).unwrap();
        assert_eq!(written[0].0.as_deref(), Some("content://site"));
        assert!(written[0].1.contains("\"./roads.geojson\""), "written back with the link beside it");
        assert_eq!(written[1].0.as_deref(), Some("content://site-moved"));
        assert!(written[1].1.contains("\"./roads.geojson\""), "the original at its new address, still beside it");
        assert_eq!(written[2].0, None);
        assert!(written[2].1.contains(&whole), "a picker save keeps it whole");
        assert_eq!(written[3].0.as_deref(), Some("content://copy"));
        assert!(written[3].1.contains(&whole), "and so does Save after it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A direct Save As writes the submitted state. Any edit after submission is still unsaved.
    #[test]
    fn native_save_marks_the_written_snapshot() {
        let mut h = launch();
        h.state_mut().services.save_project = Some(Box::new(|_, _, _| Ok(())));
        edit(&mut h, "On disk");
        h.state_mut().run("ui.saveAs", Value::Null).unwrap();
        edit(&mut h, "In memory");
        h.state_mut().complete_project_save(Ok("atlas.rgis".into()));
        assert!(h.state().session.dirty());
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert_eq!(h.state().session.map().name, "On disk");
        assert!(!h.state().session.dirty());
    }

    /// New from the toolbar: Cancel keeps the project, Don't Save replaces it, and Save writes it
    /// to its file first. A project with nothing unsaved is replaced without a word, as is one
    /// undone back to the state it was saved in.
    #[test]
    fn new_asks_before_replacing_unsaved_work() {
        let dir = folder("new");
        let file = dir.join("atlas.rgis");
        let mut h = launch();
        frames(&mut h, 2);
        h.state_mut().run("project.saveAs", json!({"path": file.display().to_string()})).unwrap();
        edit(&mut h, "Edited");
        h.get_by_label("New project").click();
        frames(&mut h, 3);
        assert!(asking(&h) && h.state().session.project.name == "atlas", "it waits for the answer");
        click(&mut h, "Cancel");
        assert_eq!(h.state().session.map().name, "Edited");
        h.get_by_label("New project").click();
        frames(&mut h, 3);
        click(&mut h, "Save");
        let saved = std::fs::read_to_string(&file).unwrap();
        assert!(saved.contains("\"Edited\"") && h.state().session.project.name == "Untitled" && !asking(&h));
        // Undone back to the save: nothing is unsaved, so nothing asks.
        h.state_mut().run("project.open", json!({"path": file.display().to_string()})).unwrap();
        edit(&mut h, "Again");
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        assert!(!h.state().session.dirty());
        h.get_by_label("New project").click();
        frames(&mut h, 3);
        assert!(!asking(&h) && h.state().session.project.name == "Untitled");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The keyboard shortcut and the Project tab's cards ask as the toolbar does; once answered,
    /// the Project tab closes on the project that replaced this one.
    #[test]
    fn shortcuts_and_the_project_tab_ask_too() {
        let mut h = launch();
        frames(&mut h, 2);
        edit(&mut h, "Edited");
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        frames(&mut h, 3);
        assert!(asking(&h) && h.state().session.project.name == "World Atlas");
        // Shortcuts don't act behind the dialog: Escape answers it, not the map's tool.
        h.state_mut().run("tool.set", json!({"tool": "measure"})).unwrap();
        h.key_press(egui::Key::Escape);
        frames(&mut h, 3);
        assert!(!asking(&h) && h.state().session.tool == rustgis_engine::Tool::Measure);
        // Command Search closes on its own Escape too, leaving the tool.
        h.state_mut().run("ui.commandSearch", Value::Null).unwrap();
        frames(&mut h, 3);
        h.key_press(egui::Key::Escape);
        frames(&mut h, 3);
        assert!(h.state().ui.command_search.is_none() && h.state().session.tool == rustgis_engine::Tool::Measure);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
        frames(&mut h, 3);
        click(&mut h, "Don't Save");
        assert_eq!(h.state().session.project.name, "Untitled");
        edit(&mut h, "Edited");
        h.state_mut().run("ui.backstage", json!({"open": true})).unwrap();
        frames(&mut h, 3);
        h.get_by_role_and_label(Role::Button, "World Atlas").click();
        frames(&mut h, 3);
        assert!(asking(&h) && h.state().session.project.name == "Untitled");
        // Cancel goes back to the Project tab, not the map.
        click(&mut h, "Cancel");
        assert!(h.state().ui.backstage && h.state().session.project.name == "Untitled");
        h.get_by_role_and_label(Role::Button, "World Atlas").click();
        frames(&mut h, 3);
        click(&mut h, "Don't Save");
        assert!(h.state().session.project.name == "World Atlas" && !h.state().ui.backstage);
    }

    /// Opening a project, picked or dropped on the window, asks first; Don't Save opens it. A
    /// QGIS project, which opens as a map of this one, doesn't ask.
    #[test]
    fn open_and_drop_ask_before_replacing_unsaved_work() {
        let dir = folder("open");
        let other = dir.join("other.rgis");
        let mut h = launch();
        frames(&mut h, 2);
        h.state_mut().run("project.saveAs", json!({"path": other.display().to_string()})).unwrap();
        h.state_mut().run("project.sample", Value::Null).unwrap();
        edit(&mut h, "Edited");
        PICKED.with(|p| *p.borrow_mut() = Some(other.display().to_string()));
        h.state_mut().run_by_user("ui.open", Value::Null).unwrap();
        frames(&mut h, 2);
        assert!(asking(&h) && h.state().session.path.is_none());
        click(&mut h, "Cancel");
        assert!(h.state().session.dirty() && h.state().session.path.is_none());
        // Data dropped before a project, and a second project: the project is asked about first;
        // the rest wait, unopened (the data isn't added to the project Don't Save discards), and
        // a notice says so.
        let data = dir.join("sites.geojson");
        std::fs::write(&data, r#"{"type":"FeatureCollection","features":[]}"#).unwrap();
        let said = h.state().session.notices.len();
        for f in [data.clone(), other.clone(), other.clone()] {
            h.input_mut().dropped_files.push(std::sync::Arc::new(Dropped(f)));
        }
        frames(&mut h, 2);
        assert!(asking(&h) && h.state().session.map().layer_by_name("sites").is_none());
        assert!(h.state().session.notices[said..].iter().any(|n| n.text.starts_with("2 other dropped files weren't opened")));
        click(&mut h, "Don't Save");
        assert!(!h.state().session.dirty() && h.state().session.path.as_deref() == Some(other.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The project changed while the dialog showed (an agent's edit): Don't Save doesn't discard
    /// what the user wasn't asked about. The dialog says so and asks again; the next answer goes.
    #[test]
    fn a_change_while_asking_is_asked_about_again() {
        let mut h = launch();
        frames(&mut h, 2);
        edit(&mut h, "Edited");
        h.state_mut().run_by_user("project.new", Value::Null).unwrap();
        frames(&mut h, 2);
        h.state_mut().session.actor = Some("agent 1".into());
        h.state_mut().session.run("map.rename", &json!({"name": "By the agent"})).unwrap();
        h.state_mut().session.actor = None;
        click(&mut h, "Don't Save");
        assert!(asking(&h) && h.state().session.map().name == "By the agent");
        assert!(h.query_by_label_contains("The project changed while this was open").is_some());
        click(&mut h, "Don't Save");
        assert!(!asking(&h) && h.state().session.project.name == "Untitled");
    }

    /// Commands run directly never ask: scripts, the AI Assistant and attached agents replace the
    /// project as they always have.
    #[test]
    fn commands_run_directly_never_ask() {
        let mut h = launch();
        frames(&mut h, 2);
        edit(&mut h, "Edited");
        h.state_mut().run("project.new", Value::Null).unwrap();
        frames(&mut h, 2);
        assert!(!asking(&h) && h.state().session.project.name == "Untitled");
        // An agent's command, and a script's line.
        edit(&mut h, "Edited");
        h.state_mut().session.run("project.sample", &Value::Null).unwrap();
        assert_eq!(h.state().session.project.name, "World Atlas");
        edit(&mut h, "Edited");
        h.state_mut().run("script.run", json!({"script": "project.new"})).unwrap();
        frames(&mut h, 2);
        assert!(!asking(&h) && h.state().session.project.name == "Untitled");
        // What the user runs that doesn't replace the project doesn't ask either.
        edit(&mut h, "Edited");
        h.state_mut().run_by_user("map.zoomFull", Value::Null).unwrap();
        frames(&mut h, 2);
        assert!(!asking(&h));
    }
}
