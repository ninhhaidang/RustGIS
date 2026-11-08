//! User customization of keyboard shortcuts.
use crate::{
    RustGisApp,
    keymap::{self, Chord, Keymap},
};
use egui::{Context, Event};
use serde_json::{Value, json};

const PREF: &str = "shortcut_changes";

#[derive(Clone)]
struct Target {
    command: String,
    params: Value,
}

#[derive(Default)]
pub struct Editor {
    pub open: bool,
    pub filter: String,
    capture: Option<Target>,
    pending: Option<(Chord, Target)>,
    notice_clipboard: bool,
    /// A key the menu bar keeps was pressed.
    notice_reserved: bool,
    pub(crate) import: Option<crate::shortcutfile::Preview>,
}

impl Editor {
    /// Hold off global shortcuts while capturing a key or confirming a change.
    pub fn capturing(&self) -> bool {
        self.capture.is_some() || self.pending.is_some() || self.import.is_some()
    }
}

/// Persist only valid differences, preserving unreadable preferences until an explicit reset.
pub(crate) fn save(app: &mut RustGisApp) {
    if app.ui.shortcut_preferences_blocked {
        return;
    }
    let defaults = Keymap::new(&app.session.registry);
    if let Some(changes) = app.ui.keymap.preferences(&defaults) {
        app.ui.shortcut_preferences_full = false;
        if let Some(save) = &app.services.save_pref {
            save(PREF, &changes);
        }
    } else {
        // Too many changes to store: the saved ones stay as they are, and these apply to this session.
        app.ui.shortcut_preferences_full = true;
    }
}

pub fn load(app: &mut RustGisApp) {
    if let Some(changes) = app.services.load_pref.as_ref().and_then(|load| load(PREF)) {
        app.ui.shortcut_preferences_blocked = !app.ui.keymap.apply_preferences(&changes);
    }
    // A stored shortcut on a key the menu bar keeps would never run.
    let kept: Vec<Chord> = app.ui.keymap.bindings.iter().map(|b| b.chord).filter(|c| reserved(app, *c)).collect();
    app.ui.keymap.bindings.retain(|b| !kept.contains(&b.chord));
}

/// Whether the native menu bar keeps a key combination for itself.
fn reserved(app: &RustGisApp, chord: Chord) -> bool {
    app.ui.reserved_shortcuts.iter().any(|k| {
        (k.modifiers.command || k.modifiers.mac_cmd) == chord.command
            && k.modifiers.alt == chord.alt
            && k.modifiers.shift == chord.shift
            && k.logical_key == chord.key
    })
}

fn assign(app: &mut RustGisApp, editor: &mut Editor, chord: Chord, target: Target) {
    if keymap::is_clipboard(chord) {
        editor.notice_clipboard = true;
    } else if reserved(app, chord) {
        editor.notice_reserved = true;
    } else if app.ui.keymap.command_for(chord).is_some_and(|b| !b.runs(&target.command, &target.params)) {
        editor.pending = Some((chord, target));
    } else if app.ui.keymap.assign(chord, &target.command, target.params) {
        save(app);
    }
}

/// UI commands that need parameters only their dialog supplies, so a bare shortcut would fail.
const NEEDS_DIALOG: &[&str] = &["ui.importShapefile"];

fn rows(app: &RustGisApp, web: bool) -> Vec<(String, Value, String)> {
    let mut rows = Vec::new();
    for c in &app.session.registry.commands {
        if !c.needs_input() && !matches!(c.id, "edit.copy" | "edit.cut" | "edit.paste") {
            rows.push((c.id.to_string(), Value::Null, tl!(c.label).to_string()));
        }
    }
    for &(id, label, _) in crate::chrome::UI_COMMANDS {
        if !NEEDS_DIALOG.contains(&id) {
            rows.push((id.to_string(), Value::Null, tl!(label).to_string()));
        }
    }
    for c in rustgis_engine::plugins::commands() {
        let label = c.plugin.manifest.translate(app.lang().code(), &c.decl.label).to_string();
        rows.push((c.id, Value::Null, label));
    }
    for &(tool, label) in keymap::MAP_TOOLS {
        rows.push(("tool.set".to_string(), json!({"tool": tool}), crate::i18n::fmt("Map Tool: {tool}", &[("tool", tl_raw!(label))])));
    }
    for (command, params, _) in app.ui.keymap.grouped() {
        if !rows.iter().any(|(c, p, _)| c == command && p == params) {
            rows.push((command.to_string(), params.clone(), keymap::label(app, command, params)));
        }
    }
    rows.retain(|(id, _, _)| crate::chrome::command_available(id, web));
    let mut seen = std::collections::HashSet::new();
    rows.retain(|(command, params, _)| seen.insert((command.clone(), params.to_string())));
    rows.sort_by_cached_key(|a| a.2.to_lowercase());
    rows
}

/// Whether Command Search can open a matching row in the editor.
pub fn can_customize(app: &RustGisApp, id: &str) -> bool {
    if !crate::chrome::command_available(id, cfg!(target_arch = "wasm32"))
        || matches!(id, "edit.copy" | "edit.cut" | "edit.paste")
        || NEEDS_DIALOG.contains(&id)
    {
        return false;
    }
    crate::chrome::UI_COMMANDS.iter().any(|(command, _, _)| *command == id)
        || app.session.registry.commands.iter().any(|c| c.id == id && !c.needs_input())
        || rustgis_engine::plugins::command(id).is_some()
        || app.ui.keymap.bindings.iter().any(|b| b.command == id)
}

/// The chord a key press assigns. On a Mac, Ctrl alone is not one: ⌘ is the command key there
/// and Ctrl is a separate modifier, so Ctrl+T and Ctrl+⌘+T are skipped rather than stored as T
/// and ⌘T. Elsewhere `mac_cmd` is never set and `command` mirrors Ctrl.
fn captured_chord(event: &Event) -> Option<Chord> {
    match event {
        Event::Key { key, pressed: true, repeat: false, modifiers, .. }
            if !(modifiers.ctrl && (!modifiers.command || modifiers.mac_cmd)) =>
        {
            Some(Chord { command: modifiers.command, alt: modifiers.alt, shift: modifiers.shift, key: *key })
        }
        _ => None,
    }
}

/// Draws the editor after the normal map and dialogs.
pub fn show(app: &mut RustGisApp, ctx: &Context) {
    let mut editor = std::mem::take(&mut app.ui.shortcut_editor);
    let was_capturing = editor.capture.is_some();
    if !editor.open {
        app.ui.shortcut_editor = editor;
        return;
    }
    let mut open = true;
    let mut remove = None;
    let mut restore = None;
    let mut reset_all = false;
    let mut assign_escape = false;
    let mut file_command = None;
    let defaults = Keymap::new(&app.session.registry);
    let rows = rows(app, cfg!(target_arch = "wasm32"));
    egui::Window::new(tl!("Keyboard Shortcuts")).id(egui::Id::unique("shortcut_editor")).open(&mut open).default_width(620.0).default_height(500.0).resizable(true).show(
        ctx,
        |ui| {
            ui.horizontal(|ui| {
                let label = ui.label(tl!("Search commands"));
                ui.add_enabled(!editor.capturing(), egui::TextEdit::singleline(&mut editor.filter)).labelled_by(label.id);
                if ui.button(tl!("Reset All")).clicked() {
                    reset_all = true;
                }
            });
            ui.horizontal(|ui| {
                if ui.add_enabled(!editor.capturing(), egui::Button::new(tl!("Import…"))).clicked() {
                    file_command = Some("ui.shortcuts.import");
                }
                if ui.add_enabled(!editor.capturing(), egui::Button::new(tl!("Export…"))).clicked() {
                    file_command = Some("ui.shortcuts.export");
                }
            });
            if app.ui.shortcut_preferences_blocked {
                ui.label(tl!("Saved shortcuts are unreadable or exceed the limit. Changes apply only to this session; Reset All replaces the saved shortcuts."));
            } else if app.ui.shortcut_preferences_full {
                ui.label(tl!("Too many shortcut changes to save. Changes apply only to this session."));
            }
            if cfg!(target_arch = "wasm32") {
                ui.label(tl!("Your browser may reserve some shortcuts."));
            }
            ui.label(tl!("While typing, shortcuts without Ctrl/⌘ are paused. First-person navigation keeps Escape, Home and Delete."));
            if editor.notice_clipboard {
                ui.label(tl!("Copy, Cut and Paste keys cannot be reassigned."));
            }
            if editor.notice_reserved {
                ui.label(tl!("The menu bar keeps that key: choose another."));
            }
            if editor.capture.is_some() {
                ui.horizontal(|ui| {
                    ui.label(tl!("Press a key combination to assign it. Escape cancels."));
                    assign_escape = ui.button(tl!("Assign Escape")).clicked();
                    if ui.button(tl!("Cancel")).clicked() {
                        editor.capture = None;
                    }
                });
            }
            let filter = editor.filter.to_lowercase();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for (command, params, label) in &rows {
                    if !filter.is_empty() && !label.to_lowercase().contains(&filter) && !command.to_lowercase().contains(&filter) {
                        continue;
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.label(label);
                        for chord in app.ui.keymap.keys_for(command, params).collect::<Vec<_>>() {
                            if ui
                                .small_button(format!("{} ×", chord.shown(ctx)))
                                .on_hover_text(tl!("Remove shortcut"))
                                .clicked()
                            {
                                remove = Some(chord);
                            }
                        }
                        if ui
                            .small_button(tl!("Add…"))
                            .on_hover_text(tl!("Tab, Enter, Space and arrow key bindings can override keyboard navigation."))
                            .clicked()
                        {
                            editor.notice_clipboard = false;
                            editor.notice_reserved = false;
                            editor.capture = Some(Target { command: command.clone(), params: params.clone() });
                        }
                        for chord in defaults.keys_for(command, params) {
                            if app.ui.keymap.command_for(chord).is_none_or(|b| !b.runs(command, params))
                                && ui.small_button(crate::i18n::fmt("Reset {key}", &[("key", &chord.shown(ctx))])).clicked()
                            {
                                restore = Some((chord, Target { command: command.clone(), params: params.clone() }));
                            }
                        }
                    });
                    ui.separator();
                }
            });
        },
    );
    if !open {
        editor = Editor::default();
    }
    if reset_all {
        app.ui.keymap = defaults;
        app.ui.shortcut_preferences_blocked = false;
        editor.capture = None;
        editor.pending = None;
        editor.notice_clipboard = false;
        editor.notice_reserved = false;
        save(app);
    }
    if let Some(chord) = remove {
        app.ui.keymap.remove(chord);
        save(app);
    }
    if let Some((chord, target)) = restore {
        assign(app, &mut editor, chord, target);
    }
    if assign_escape {
        if let Some(target) = editor.capture.take()
            && let Some(chord) = Chord::parse("Escape")
        {
            assign(app, &mut editor, chord, target);
        }
    } else if was_capturing && editor.capture.is_some() {
        let chord = ctx.input(|i| i.events.iter().find_map(captured_chord));
        if let Some(chord) = chord
            && let Some(target) = editor.capture.take()
        {
            if chord == (Chord { command: false, alt: false, shift: false, key: egui::Key::Escape }) {
                editor.capture = None;
            } else {
                assign(app, &mut editor, chord, target);
            }
        }
    }
    if let Some((chord, target)) = editor.pending.clone() {
        let existing = app.ui.keymap.command_for(chord).map(|b| keymap::label(app, &b.command, &b.params)).unwrap_or_default();
        egui::Modal::new(egui::Id::unique("shortcut_conflict")).show(ctx, |ui| {
            ui.heading(tl!("Shortcut already assigned"));
            ui.label(crate::i18n::fmt("{key} is used by {command}.", &[("key", &chord.shown(ctx)), ("command", &existing)]));
            ui.horizontal(|ui| {
                if ui.button(tl!("Replace")).clicked() {
                    if app.ui.keymap.assign(chord, &target.command, target.params.clone()) {
                        save(app);
                    }
                    editor.pending = None;
                }
                if ui.button(tl!("Cancel")).clicked() {
                    editor.pending = None;
                }
            });
        });
    }
    let mut confirm_import = None;
    let mut cancel_import = false;
    if let Some(preview) = &editor.import {
        egui::Modal::new(egui::Id::unique("shortcut_import")).show(ctx, |ui| {
            ui.heading(tl!("Import Keyboard Shortcuts"));
            ui.label(crate::i18n::fmt(
                "Bindings: {bindings} · Conflicts: {conflicts} · Unknown commands: {unknown}",
                &[
                    ("bindings", &preview.summary.get("bindings").unwrap_or(&Value::Null).to_string()),
                    ("conflicts", &preview.summary.get("conflicts").unwrap_or(&Value::Null).to_string()),
                    ("unknown", &preview.summary.get("unknown_commands").unwrap_or(&Value::Null).to_string()),
                ],
            ));
            ui.label(tl!("Imported bindings replace conflicting keys. Unknown commands are kept for later use."));
            if app.ui.shortcut_preferences_blocked {
                ui.label(tl!("Reset unreadable saved shortcuts before importing."));
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(!app.ui.shortcut_preferences_blocked, egui::Button::new(tl!("Import"))).clicked() {
                    confirm_import = Some(preview.text.clone());
                }
                if ui.button(tl!("Cancel")).clicked() {
                    cancel_import = true;
                }
            });
        });
    }
    if cancel_import {
        editor.import = None;
    }
    app.ui.shortcut_editor = editor;
    let result = if let Some(text) = confirm_import {
        app.run("ui.shortcuts.import", json!({"text":text, "confirm":true}))
    } else if let Some(id) = file_command {
        app.run(id, Value::Null)
    } else {
        Ok(Value::Null)
    };
    if let Err(error) = result {
        app.session.notify(error, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::{Project, Session};

    #[test]
    fn web_rows_exclude_desktop_commands_even_with_existing_bindings() {
        let mut app = RustGisApp::new(Session::new(Project::default()));
        for id in ["ui.checkUpdate", "ui.mcpRelay", "ui.themeLoad", "ui.themeReload"] {
            app.ui.keymap.assign(Chord::parse("F8").unwrap(), id, Value::Null);
            assert!(!crate::chrome::command_available(id, true));
            assert!(!rows(&app, true).iter().any(|(command, _, _)| command == id));
            assert!(rows(&app, false).iter().any(|(command, _, _)| command == id));
        }
        assert!(crate::chrome::command_available("ui.theme", true));
    }

    #[test]
    fn conflicting_and_clipboard_keys_are_not_changed_without_confirmation() {
        let mut app = RustGisApp::new(Session::new(Project::default()));
        let mut editor = Editor::default();
        let target = Target { command: "select.clear".into(), params: Value::Null };
        let chord = Chord::parse("Mod+T").unwrap();
        let before = app.ui.keymap.command_for(chord).unwrap().command.clone();
        assign(&mut app, &mut editor, chord, target.clone());
        assert!(editor.pending.is_some());
        assert_eq!(app.ui.keymap.command_for(chord).unwrap().command, before);
        editor.pending = None;
        let clipboard = Chord::parse("Mod+C").unwrap();
        assign(&mut app, &mut editor, clipboard, target);
        assert!(editor.notice_clipboard);
        assert!(app.ui.keymap.command_for(clipboard).is_none());
        // A key the menu bar keeps (macOS's ⌘Q) is refused too.
        app.ui.reserved_shortcuts = vec![egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Q)];
        let quit = Chord::parse("Mod+Q").unwrap();
        assign(&mut app, &mut editor, quit, Target { command: "select.clear".into(), params: Value::Null });
        assert!(editor.notice_reserved);
        assert!(app.ui.keymap.command_for(quit).is_none());
        // A stored shortcut on it is left out when the shortcuts load.
        let stored = std::rc::Rc::new(r#"[{"keys": "Mod+Q", "command": "select.clear"}, {"keys": "Mod+J", "command": "select.clear"}]"#);
        app.services.load_pref = Some(Box::new(move |key| (key == PREF).then(|| stored.to_string())));
        load(&mut app);
        assert!(app.ui.keymap.command_for(quit).is_none());
        assert!(app.ui.keymap.command_for(Chord::parse("Mod+J").unwrap()).is_some());
        // ⌃T (Ctrl without ⌘) isn't the bare T.
        assert!(app.shortcut_command(egui::KeyboardShortcut::new(egui::Modifiers::CTRL, egui::Key::T)).is_none());
    }

    #[test]
    fn capture_takes_the_command_key_not_a_bare_ctrl() {
        let press =
            |modifiers: egui::Modifiers| Event::Key { key: egui::Key::T, physical_key: None, pressed: true, repeat: false, modifiers };
        assert_eq!(captured_chord(&press(egui::Modifiers::COMMAND)), Chord::parse("Mod+T"));
        // On a Mac, Ctrl+T is Ctrl alone: it is skipped, not stored as a bare T.
        assert_eq!(captured_chord(&press(egui::Modifiers::CTRL)), None);
    }

    #[test]
    fn search_customize_requires_an_editor_row() {
        let mut app = RustGisApp::new(Session::new(Project::default()));
        assert!(can_customize(&app, "ui.theme"));
        assert!(!can_customize(&app, "layer.addData"));
        assert!(!can_customize(&app, "edit.copy"));
        assert!(!can_customize(&app, "ui.identify"));
        assert!(app.ui.keymap.assign(Chord::parse("D").unwrap(), "layer.addData", json!({"path": "example.geojson"})));
        assert!(can_customize(&app, "layer.addData"));
    }

    /// Command Search's menu entry and the editor's rows must agree: every row can be customized,
    /// and every command that can be customized has a row. Plugins are the process's, and other
    /// tests load and unload theirs meanwhile, so of the plugins' commands only those of this test's
    /// own plugin are compared.
    #[test]
    fn search_customize_agrees_with_the_editor_rows() {
        use rustgis_engine::plugins;
        let pkg = plugins::Package::from_zip(&plugins::testing::package("ui-shortcut-rows", &["edit"])).unwrap();
        plugins::load(pkg, "test").unwrap();
        let ours = |id: &str| !id.starts_with(plugins::PREFIX) || id.starts_with("plugin:ui-shortcut-rows/");
        let app = RustGisApp::new(Session::new(Project::default()));
        let ids: std::collections::HashSet<String> = rows(&app, false).into_iter().map(|(id, _, _)| id).filter(|id| ours(id)).collect();
        assert!(ids.iter().any(|id| id.starts_with("plugin:ui-shortcut-rows/")), "its plugin's commands have rows");
        for id in &ids {
            assert!(can_customize(&app, id), "{id} has an editor row but Command Search cannot customize it");
        }
        let registry = app.session.registry.commands.iter().map(|c| c.id.to_string());
        let ui = crate::chrome::UI_COMMANDS.iter().map(|&(id, _, _)| id.to_string());
        let plugin_commands = plugins::commands().into_iter().map(|c| c.id);
        let bound = app.ui.keymap.bindings.iter().map(|b| b.command.clone());
        for id in registry.chain(ui).chain(plugin_commands).chain(bound).filter(|id| ours(id)) {
            assert_eq!(can_customize(&app, &id), ids.contains(&id), "{id}");
        }
        plugins::unload("ui-shortcut-rows");
    }
}
