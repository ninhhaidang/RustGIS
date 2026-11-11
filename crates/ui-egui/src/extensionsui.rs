//! The Extensions window (Plugins › Extensions, #1752): every extension with a switch to turn it
//! on or off, and where to find it when it's on. What the user chose is the `extensions`
//! setting: read at start, written whenever it changes (by the window, command search, a script
//! or the command line in the app alike).

use egui::{Align2, vec2};
use serde_json::json;

use crate::RustGisApp;
use crate::theme::Tokens;
use rustgis_engine::extensions;

/// The setting the choices are kept in.
pub const SETTING: &str = "extensions";

/// The Extensions window's state.
#[derive(Default)]
pub struct ExtensionsState {
    /// Whether the window is open.
    pub open: bool,
    /// The last thing a switch said (true: done), shown at the top.
    pub message: Option<(bool, String)>,
    /// The choices last written to the setting (or read from it at start).
    saved: Option<extensions::State>,
}

/// Takes the saved choices (at start).
pub fn load(app: &mut RustGisApp) {
    let text = app.services.load_pref.as_ref().and_then(|load| load(SETTING));
    app.session.load_extensions(text.as_deref());
    app.extensions.saved = Some(app.session.extensions().clone());
}

/// Writes the choices when they changed since they were last written.
pub fn persist(app: &mut RustGisApp) {
    let now = app.session.extensions();
    if app.extensions.saved.as_ref() == Some(now) {
        return;
    }
    let now = now.clone();
    // A setting that couldn't be read is never written over (said when it was read).
    if let (Some(save), Some(text)) = (&app.services.save_pref, now.to_setting()) {
        save(SETTING, &text);
    }
    app.extensions.saved = Some(now);
}

/// Turns extension `id` on or off from the window, saying what happened.
fn set(app: &mut RustGisApp, id: &str, on: bool) {
    match app.run("extension.enable", json!({"id": id, "enabled": on})) {
        Ok(_) => {
            let name = extensions::get(id).map_or(id, |e| e.name);
            let text = if on {
                crate::i18n::fmt("{name} is on.", &[("name", tl_raw!(name))])
            } else {
                crate::i18n::fmt("{name} is off: its tab and commands are hidden.", &[("name", tl_raw!(name))])
            };
            app.extensions.message = Some((true, text));
        }
        Err(e) => app.extensions.message = Some((false, e)),
    }
}

/// The window, while it's open.
pub fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    if !app.extensions.open {
        return;
    }
    let t = Tokens::get(ctx);
    let mut open = true;
    let screen = ctx.content_rect();
    egui::Window::new(tl!("Extensions"))
        .id(egui::Id::unique("extensions_window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(vec2(480.0_f32.min(screen.width() - 32.0), 320.0_f32.min(screen.height() - 80.0)))
        .pivot(Align2::CENTER_CENTER)
        .default_pos(screen.center())
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(tl!(
                    "Extensions are optional parts of RustGIS: nothing to download. Turn one off to hide its tab and commands."
                ))
                .size(12.0)
                .color(t.text_dim),
            );
            if let Some((ok, text)) = app.extensions.message.clone() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(text).size(12.0).color(if ok { t.green } else { t.red }));
            }
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
                for e in extensions::ALL {
                    ui.push_id(e.id, |ui| row(app, ui, &t, e));
                    ui.separator();
                }
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tl!("Add more with plugins:")).size(12.0).color(t.text_dim));
                if ui.link(tl!("Plugin Manager")).clicked() {
                    let _ = app.run("ui.plugins", json!({"open": true}));
                }
            });
        });
    if !open {
        app.extensions.open = false;
        app.extensions.message = None;
    }
}

/// One extension: its switch, name and description, and where it is.
fn row(app: &mut RustGisApp, ui: &mut egui::Ui, t: &Tokens, e: &extensions::Extension) {
    let was = app.session.extensions().is_on(e.id);
    let mut on = was;
    ui.horizontal(|ui| {
        ui.checkbox(&mut on, egui::RichText::new(tl_raw!(e.name)).strong().size(13.5));
        if let (true, Some(caption)) = (was, e.tab) {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = crate::i18n::fmt("Open the {tab} tab", &[("tab", tl_raw!(caption))]);
                if ui.link(label).clicked() {
                    let _ = app.run("ui.tab", json!({"tab": extensions::tab_id(e.id)}));
                }
            });
        }
    });
    ui.label(egui::RichText::new(tl_raw!(e.description)).size(12.0));
    let place = match e.tab {
        Some(caption) => crate::i18n::fmt("On its own tab, {tab}.", &[("tab", tl_raw!(caption))]),
        None => tl!("On the Plugins tab.").to_string(),
    };
    let count = e.commands.len().to_string();
    let said = crate::i18n::fmt("{place} {count} commands.", &[("place", &place), ("count", &count)]);
    ui.label(egui::RichText::new(said).size(11.5).color(t.text_dim));
    if on != was {
        set(app, e.id, on);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// An app whose settings live in `prefs`.
    fn app_with_prefs(prefs: &Arc<Mutex<Vec<(String, String)>>>) -> RustGisApp {
        let mut app = RustGisApp::with_sample();
        let (load, save) = (Arc::clone(prefs), Arc::clone(prefs));
        app.services.load_pref =
            Some(Box::new(move |k: &str| load.lock().ok().and_then(|p| p.iter().rev().find(|(key, _)| key == k).map(|(_, v)| v.clone()))));
        app.services.save_pref = Some(Box::new(move |k: &str, v: &str| {
            if let Ok(mut p) = save.lock() {
                p.push((k.to_string(), v.to_string()));
            }
        }));
        app
    }

    #[test]
    fn turning_imagery_off_hides_its_tab_sections_and_commands_and_is_saved() {
        let prefs = Arc::new(Mutex::new(vec![(SETTING.to_string(), r#"{"imagery": true, "from-newer": true}"#.to_string())]));
        let mut app = app_with_prefs(&prefs);
        load(&mut app);
        let tab = extensions::tab_id("imagery");
        assert!(crate::ribbon::tab_exists(&app, &tab));
        app.run("ui.tab", json!({"tab": tab})).unwrap();
        app.run("ui.pane.workflows", json!({"section": "ortho"})).unwrap();

        app.run("ui.extensions", json!({"open": true})).unwrap();
        assert!(app.extensions.open);
        set(&mut app, "imagery", false);
        assert!(app.extensions.message.as_ref().is_some_and(|(ok, _)| *ok));
        assert!(!crate::ribbon::tab_exists(&app, &tab));
        // A refused section leaves a pick started elsewhere alone.
        app.workflow.pick = Some(crate::analysispane::Pick { purpose: "test", needed: 1, points: Vec::new() });
        let why = app.run("ui.pane.workflows", json!({"section": "ortho"})).unwrap_err();
        assert!(app.workflow.pick.is_some());
        assert!(why.contains("Imagery extension, which is off"), "{why}");
        assert!(!crate::analysispane::section_shown(&app.session, "classify"));
        assert!(crate::analysispane::section_shown(&app.session, "rasterFunctions"));
        let why = app.run("raster.classify", json!({})).unwrap_err();
        assert!(why.contains("Imagery extension, which is off"), "{why}");

        // Saved with what a newer version wrote, and read again at the next start.
        persist(&mut app);
        let saved = prefs.lock().unwrap().iter().rev().find(|(k, _)| k == SETTING).map(|(_, v)| v.clone()).unwrap();
        assert!(saved.contains("\"imagery\":false") && saved.contains("\"from-newer\":true"), "{saved}");
        let mut again = app_with_prefs(&prefs);
        load(&mut again);
        assert!(!again.session.extensions().is_on("imagery"));
        assert!(!crate::ribbon::tab_exists(&again, &tab));

        // On again: the tab is back.
        again.run("extension.enable", json!({"id": "imagery"})).unwrap();
        assert!(crate::ribbon::tab_exists(&again, &tab));
        assert!(again.run("ui.pane.workflows", json!({"section": "ortho"})).is_ok());
    }

    #[test]
    fn an_unreadable_setting_is_said_and_nothing_is_off() {
        let prefs = Arc::new(Mutex::new(vec![(SETTING.to_string(), "not json".to_string())]));
        let mut app = app_with_prefs(&prefs);
        load(&mut app);
        // At its default.
        assert!(!app.session.extensions().is_on("imagery"));
        assert!(app.session.notices.iter().any(|n| n.text.contains("extensions setting isn't a JSON object")));
        // Not written over by a change.
        set(&mut app, "imagery", true);
        persist(&mut app);
        assert!(prefs.lock().unwrap().iter().all(|(_, v)| v == "not json"));
        // A clean setting says nothing.
        let prefs = Arc::new(Mutex::new(vec![(SETTING.to_string(), r#"{"imagery": true}"#.to_string())]));
        let mut app = app_with_prefs(&prefs);
        load(&mut app);
        assert!(!app.session.notices.iter().any(|n| n.text.contains("extensions setting")));
    }

    #[test]
    fn the_app_starts_with_imagery_off_until_it_is_turned_on() {
        let prefs = Arc::new(Mutex::new(Vec::new()));
        let mut app = app_with_prefs(&prefs);
        load(&mut app);
        let tab = extensions::tab_id("imagery");
        assert!(!crate::ribbon::tab_exists(&app, &tab));
        assert!(app.run("raster.classify", json!({})).is_err_and(|e| e.contains("Imagery extension, which is off")));
        // Nothing chosen yet: nothing written.
        persist(&mut app);
        assert!(prefs.lock().unwrap().is_empty());
        set(&mut app, "imagery", true);
        assert!(crate::ribbon::tab_exists(&app, &tab));
        persist(&mut app);
        assert!(prefs.lock().unwrap().iter().any(|(k, v)| k == SETTING && v.contains("\"imagery\":true")));
    }

    #[test]
    fn show_me_opens_an_extensions_tab_for_its_commands() {
        let mut app = RustGisApp::with_sample();
        let step = |command: &str| rustgis_engine::project::TaskStep {
            title: String::new(),
            instructions: String::new(),
            command: command.into(),
            params: json!({}),
        };
        crate::showme::start(&mut app, &step("raster.classify"));
        assert_eq!(app.ui.tab, "extension:imagery");
        // While the extension is off its tab isn't there to open.
        app.run("ui.tab", json!({"tab": "Home"})).unwrap();
        app.run("extension.disable", json!({"id": "imagery"})).unwrap();
        app.ui.show_me = None;
        let said = crate::showme::start(&mut app, &step("raster.classify"));
        assert_eq!(app.ui.tab, "Home");
        assert!(said.contains("Imagery extension, which is off"), "{said}");
        assert!(app.ui.show_me.is_none());
    }

    #[test]
    fn an_extensions_tab_comes_after_help() {
        let app = RustGisApp::with_sample();
        let ids: Vec<String> = crate::ribbon::all_tabs(&app).into_iter().map(|(id, _)| id).collect();
        let at = |t: &str| ids.iter().position(|i| i == t);
        assert!(at("extension:imagery") > at("Help") && at("Help").is_some(), "{ids:?}");
    }

    #[test]
    fn the_legacy_imagery_tab_name_opens_the_extension_tab() {
        assert_eq!(crate::ribbon::current_tab("Imagery"), "extension:imagery");
        assert_eq!(crate::ribbon::current_hidden_tabs(vec!["extension:imagery".into()]), vec!["extension:imagery".to_string()]);
    }
}
