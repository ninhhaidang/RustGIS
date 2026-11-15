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
}

/// Whether running command `id` from the interface asks first: it replaces the project, which
/// has unsaved changes, in the desktop app or on a phone.
///
/// A phone's shell saves through the system's picker, as the browser saves by downloading, but
/// it has no page to warn before leaving: so it asks too. Its Save starts the save and leaves
/// the project as it is (the picker answers later, and the dialog reads the result at once);
/// Don't Save and Cancel are the desktop's.
pub(crate) fn asks(app: &RustGisApp, id: &str) -> bool {
    REPLACES.contains(&id) && app.session.dirty() && (app.opens_files() || cfg!(any(target_os = "android", target_os = "ios")))
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
    app.save_prompt.asking.is_some()
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
    egui::Modal::new(egui::Id::unique("save_prompt")).show(ctx, |ui| {
        ui.set_width(380.0);
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
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
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
        // Save As when the project has no file. A save that fails says why (its notice), and one
        // cancelled leaves the changes unsaved: either way nothing is discarded. This reads the
        // result at once: on the desktop, the only shell that asks (`asks`), `ui.save` writes the
        // file, and Save As's picker returns, before it does. A shell that saved later (the
        // browser's download dialog) would have to go on from there instead.
        Answer::Save => {
            if app.run("ui.save", Value::Null).is_ok() && !app.session.dirty() {
                proceed(app, what);
            }
        }
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
