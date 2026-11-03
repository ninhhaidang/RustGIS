//! Keyboard shortcuts: one map from key combinations to commands. It drives the key handling
//! ([`crate::chrome::shortcuts`]), the shortcut shown in tooltips, menu rows and Command Search,
//! and the Help › Shortcuts window.
//!
//! A key combination is written `Mod+Shift+Z`: `Mod` is Ctrl, or ⌘ on macOS, so one spelling
//! works on every platform and in the browser.

use egui::Key;
use rustgis_engine::Registry;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::RustGisApp;

pub(crate) const MAX_PREF_BYTES: usize = 100_000;
const MAX_PREF_CHANGES: usize = 1_000;
const MAX_COMMAND_LEN: usize = 200;

/// The default shortcuts: (keys, command, parameters as JSON, or empty for none). Clipboard
/// keys are not here: they arrive as copy, cut and paste events, not key presses.
const DEFAULTS: &[(&str, &str, &str)] = &[
    ("Mod+K", "ui.commandSearch", ""),
    ("Mod+N", "project.new", ""),
    ("Mod+O", "ui.open", ""),
    ("Mod+S", "ui.save", ""),
    ("Mod+Shift+S", "ui.saveAs", ""),
    ("Mod+Z", "edit.undo", ""),
    ("Mod+Y", "edit.redo", ""),
    ("Mod+Shift+Z", "edit.redo", ""),
    ("Mod+T", "table.open", ""),
    ("Mod+F1", "ui.ribbon", ""),
    ("Mod+Equals", "map.zoomIn", ""),
    ("Mod+Plus", "map.zoomIn", ""),
    ("Mod+Minus", "map.zoomOut", ""),
    ("Home", "map.zoomFull", ""),
    ("Escape", "tool.set", r#"{"tool":"explore"}"#),
    ("Delete", "edit.delete", ""),
    ("Mod+D", "layout.duplicateElement", ""),
    // The active layer in the Layers pane's outline, as a word processor's outline keys (#836).
    ("Alt+Shift+ArrowRight", "layer.indent", ""),
    ("Alt+Shift+ArrowLeft", "layer.outdent", ""),
    ("Alt+Shift+ArrowUp", "layer.moveUp", ""),
    ("Alt+Shift+ArrowDown", "layer.moveDown", ""),
];

/// The clipboard keys: (keys, command). They arrive as copy, cut and paste events rather than key
/// presses, so they are shown but never bound.
pub const CLIPBOARD: &[(&str, &str)] = &[("Mod+C", "edit.copy"), ("Mod+X", "edit.cut"), ("Mod+V", "edit.paste")];

/// The map tools a shortcut can pick (`tool.set` with this tool): (tool, label).
pub const MAP_TOOLS: &[(&str, &str)] = &[
    ("explore", "Explore"),
    ("select", "Select"),
    ("selectPolygon", "Select by Polygon"),
    ("selectLasso", "Select by Lasso"),
    ("selectRadius", "Select by Radius"),
    ("zoomIn", "Zoom In"),
    ("measure", "Measure Distance"),
    ("measureArea", "Measure Area"),
    ("createPoint", "Create Point"),
    ("createLine", "Create Line"),
    ("createPolygon", "Create Polygon"),
    ("editVertices", "Edit Vertices"),
    ("cutPolygon", "Cut Polygon"),
    ("reshape", "Reshape"),
];

/// Labels of the `ui.*` commands that have a shortcut but no entry in Command Search.
const UI_LABELS: &[(&str, &str)] = &[
    ("ui.commandSearch", "Command Search"),
    ("ui.open", "Open Project…"),
    ("ui.save", "Save Project"),
    ("ui.saveAs", "Save Project As…"),
    ("ui.ribbon", "Collapse or Expand the Ribbon"),
];

/// A key with its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    /// Ctrl, or ⌘ on macOS.
    pub command: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

impl Chord {
    /// Whether a focused text field takes this chord for itself: egui's undo (Mod+Z) and redo
    /// (Mod+Y, Mod+Shift+Z), matched as egui matches them, an extra Alt or Shift aside.
    pub fn is_text_undo(self) -> bool {
        self.command && matches!(self.key, Key::Z | Key::Y)
    }

    /// Reads `Mod+Shift+Z`, `Ctrl+T`, `Home`, `F5`, `Mod++`. `Ctrl`, `Cmd` and `⌘` all mean
    /// `Mod`; letters may be lower case. `None` for anything else.
    pub fn parse(s: &str) -> Option<Chord> {
        let s = s.trim();
        let (mods, key) = match s.strip_suffix('+') {
            // `Mod++` and `+`: the key is the plus sign.
            Some(rest) if rest.is_empty() || rest.ends_with('+') => (rest.trim_end_matches('+'), "+"),
            _ => s.rsplit_once('+').unwrap_or(("", s)),
        };
        let key = key.trim();
        let key = Key::from_name(key).or_else(|| Key::from_name(&key.to_ascii_uppercase()))?;
        let mut chord = Chord { command: false, alt: false, shift: false, key };
        for m in mods.split('+').map(str::trim).filter(|m| !m.is_empty()) {
            match m.to_ascii_lowercase().as_str() {
                "mod" | "ctrl" | "control" | "cmd" | "command" | "⌘" => chord.command = true,
                "alt" | "option" | "opt" | "⌥" => chord.alt = true,
                "shift" | "⇧" => chord.shift = true,
                _ => return None,
            }
        }
        Some(chord)
    }

    /// The modifiers to match, for egui.
    pub fn modifiers(self) -> egui::Modifiers {
        let mut m = egui::Modifiers::NONE;
        if self.command {
            m |= egui::Modifiers::COMMAND;
        }
        if self.alt {
            m |= egui::Modifiers::ALT;
        }
        if self.shift {
            m |= egui::Modifiers::SHIFT;
        }
        m
    }

    /// As shown to the user: `⇧⌘S` on a Mac (`mac`, as egui reads the keys: `ctx.os().is_mac()`, a
    /// browser's too), `Ctrl+Shift+S` elsewhere.
    pub fn text(self, mac: bool) -> String {
        let key = match self.key {
            Key::Escape => "Esc",
            Key::Plus | Key::Minus | Key::Equals => self.key.symbol_or_name(),
            k => k.name(),
        };
        if mac {
            let mut s = String::new();
            for (on, sym) in [(self.alt, "⌥"), (self.shift, "⇧"), (self.command, "⌘")] {
                if on {
                    s.push_str(sym);
                }
            }
            s + key
        } else {
            let mut s = String::new();
            for (on, name) in [(self.command, "Ctrl+"), (self.alt, "Alt+"), (self.shift, "Shift+")] {
                if on {
                    s.push_str(name);
                }
            }
            s + key
        }
    }

    /// [`Chord::text`] for `ctx`'s keys, Option spelled out (`Opt+⇧Right`) where the interface
    /// faces have no "⌥" (egui's own have none, so a browser without the system's faces).
    pub fn shown(self, ctx: &egui::Context) -> String {
        let text = self.text(ctx.os().is_mac());
        if !text.contains('⌥') {
            return text;
        }
        let ids = [egui::FontId::monospace(12.0), egui::FontId::proportional(12.0)];
        if ctx.fonts_mut(|f| ids.iter().all(|id| f.has_glyphs(id, "⌥"))) { text } else { text.replacen('⌥', "Opt+", 1) }
    }
}

impl std::fmt::Display for Chord {
    /// The portable spelling [`Chord::parse`] reads: `Mod+Shift+Z`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (on, name) in [(self.command, "Mod+"), (self.alt, "Alt+"), (self.shift, "Shift+")] {
            if on {
                f.write_str(name)?;
            }
        }
        f.write_str(self.key.name())
    }
}

/// A key combination that runs a command.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub chord: Chord,
    pub command: String,
    /// `Value::Null` for none.
    pub params: Value,
}

impl Binding {
    /// Whether this runs `command` with `params` (`null` and `{}` are both no parameters).
    pub fn runs(&self, command: &str, params: &Value) -> bool {
        let none = |v: &Value| v.is_null() || v.as_object().is_some_and(|o| o.is_empty());
        self.command == command && (self.params == *params || (none(&self.params) && none(params)))
    }
}

/// One user change to the defaults, stored in preferences.
#[derive(Serialize, Deserialize)]
pub(crate) struct Change {
    pub(crate) keys: String,
    pub(crate) command: String,
    #[serde(default)]
    pub(crate) params: Value,
    #[serde(default)]
    pub(crate) remove: bool,
}

/// Every shortcut.
#[derive(Clone, Debug, PartialEq)]
pub struct Keymap {
    pub bindings: Vec<Binding>,
}

impl Default for Keymap {
    fn default() -> Self {
        let bindings = DEFAULTS
            .iter()
            .filter_map(|(keys, command, params)| {
                Some(Binding {
                    chord: Chord::parse(keys)?,
                    command: (*command).to_string(),
                    params: serde_json::from_str(params).unwrap_or(Value::Null),
                })
            })
            .collect();
        Keymap { bindings }
    }
}

impl Keymap {
    /// The defaults, with the shortcut a command declares itself ([`CommandSpec::key`]) where
    /// its key is free.
    ///
    /// [`CommandSpec::key`]: rustgis_engine::CommandSpec::key
    pub fn new(registry: &Registry) -> Self {
        let mut map = Keymap::default();
        for spec in &registry.commands {
            if let Some(chord) = Chord::parse(spec.shortcut)
                && map.command_for(chord).is_none()
                && !is_clipboard(chord)
            {
                map.bindings.push(Binding { chord, command: spec.id.to_string(), params: Value::Null });
            }
        }
        map
    }

    /// Apply stored user changes to this version's defaults. Returns false if any of them could
    /// not be read: the valid ones still apply, but the stored text must not be overwritten.
    pub fn apply_preferences(&mut self, text: &str) -> bool {
        if text.len() > MAX_PREF_BYTES {
            return false;
        }
        let Ok(changes) = serde_json::from_str::<Vec<Value>>(text) else { return false };
        if changes.len() > MAX_PREF_CHANGES {
            return false;
        }
        let mut readable = true;
        for record in changes {
            let Ok(change) = serde_json::from_value::<Change>(record) else {
                readable = false;
                continue;
            };
            let Some(chord) = Chord::parse(&change.keys) else {
                readable = false;
                continue;
            };
            if is_clipboard(chord) || change.command.is_empty() || change.command.len() > MAX_COMMAND_LEN {
                readable = false;
                continue;
            }
            if change.remove {
                self.bindings.retain(|b| b.chord != chord || !b.runs(&change.command, &change.params));
            } else {
                self.bindings.retain(|b| b.chord != chord);
                self.bindings.push(Binding { chord, command: change.command, params: change.params });
            }
        }
        readable
    }

    /// Store differences only; return None if they would exceed the load limits.
    pub fn preferences(&self, defaults: &Keymap) -> Option<String> {
        let mut changes = Vec::new();
        for b in &defaults.bindings {
            if !self.bindings.contains(b) {
                changes.push(Change { keys: b.chord.to_string(), command: b.command.clone(), params: b.params.clone(), remove: true });
            }
        }
        for b in &self.bindings {
            if !defaults.bindings.contains(b) {
                changes.push(Change { keys: b.chord.to_string(), command: b.command.clone(), params: b.params.clone(), remove: false });
            }
        }
        if changes.len() > MAX_PREF_CHANGES || changes.iter().any(|c| c.command.is_empty() || c.command.len() > MAX_COMMAND_LEN) {
            return None;
        }
        serde_json::to_string(&changes).ok().filter(|text| text.len() <= MAX_PREF_BYTES)
    }

    /// Assign a chord, replacing its previous assignment. Clipboard chords are reserved.
    pub fn assign(&mut self, chord: Chord, command: &str, params: Value) -> bool {
        if is_clipboard(chord) || command.is_empty() {
            return false;
        }
        self.bindings.retain(|b| b.chord != chord);
        self.bindings.push(Binding { chord, command: command.to_owned(), params });
        true
    }

    /// Remove a particular chord.
    pub fn remove(&mut self, chord: Chord) {
        self.bindings.retain(|b| b.chord != chord);
    }

    /// The binding a key combination runs.
    pub fn command_for(&self, chord: Chord) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.chord == chord)
    }

    /// The keys that run `command` with `params`, in order.
    pub fn keys_for<'a>(&'a self, command: &'a str, params: &'a Value) -> impl Iterator<Item = Chord> + 'a {
        self.bindings.iter().filter(move |b| b.runs(command, params)).map(|b| b.chord)
    }

    /// The bindings in the order to match key presses: those with more modifiers first, since
    /// egui ignores an extra Shift or Alt (Ctrl+Shift+Z would otherwise run Ctrl+Z's command).
    pub fn match_order(&self) -> Vec<&Binding> {
        let mut order: Vec<&Binding> = self.bindings.iter().collect();
        order.sort_by_key(|b| std::cmp::Reverse(u8::from(b.chord.shift) + u8::from(b.chord.alt)));
        order
    }

    /// The bindings grouped by what they run, in order of first appearance: (command, params,
    /// keys).
    pub fn grouped(&self) -> Vec<(&str, &Value, Vec<Chord>)> {
        let mut groups: Vec<(&str, &Value, Vec<Chord>)> = Vec::new();
        for b in &self.bindings {
            match groups.iter_mut().find(|(c, p, _)| b.runs(c, p)) {
                Some((_, _, keys)) => keys.push(b.chord),
                None => groups.push((b.command.as_str(), &b.params, vec![b.chord])),
            }
        }
        groups
    }
}

/// A key as a browser's keyboard event names it: by its `key` (`d`, `=`, `+`), else by its
/// `code` (`KeyD`, `Digit1`, `Equal`, `Minus`), which stays put when Alt (⌥) changes the
/// character a Mac types.
pub fn browser_key(key: &str, code: &str) -> Option<Key> {
    let by_name = |s: &str| Key::from_name(s).or_else(|| Key::from_name(&s.to_ascii_uppercase()));
    by_name(key).or_else(|| match code {
        "Equal" => Some(Key::Equals),
        "Minus" => Some(Key::Minus),
        c => by_name(c.strip_prefix("Key").or_else(|| c.strip_prefix("Digit")).unwrap_or(c)),
    })
}

/// Whether a key combination is one of the [`CLIPBOARD`] keys.
pub fn is_clipboard(chord: Chord) -> bool {
    CLIPBOARD.iter().any(|(keys, _)| Chord::parse(keys) == Some(chord))
}

/// The first shortcut of `command` with `params` as shown to the user (a clipboard command's
/// fixed key included), or empty for none.
pub fn shortcut_text(ctx: &egui::Context, app: &RustGisApp, command: &str, params: &Value) -> String {
    let clipboard = || CLIPBOARD.iter().find(|(_, c)| *c == command).and_then(|(keys, _)| Chord::parse(keys));
    app.ui.keymap.keys_for(command, params).next().or_else(clipboard).map(|c| c.shown(ctx)).unwrap_or_default()
}

/// What a binding runs, translated: the command's label, or the map tool it picks.
pub fn label(app: &RustGisApp, command: &str, params: &Value) -> String {
    if command == "tool.set"
        && let Some(tool) = params.get("tool").and_then(Value::as_str)
    {
        let name = MAP_TOOLS.iter().find(|(code, _)| *code == tool).map_or(tool, |(_, label)| tl!(label));
        return crate::i18n::fmt("Map Tool: {tool}", &[("tool", name)]);
    }
    if let Some(spec) = app.session.registry.get(command) {
        return tl!(spec.label).to_string();
    }
    if let Some(c) = rustgis_engine::plugins::command(command) {
        return c.plugin.manifest.translate(app.lang().code(), &c.decl.label).to_string();
    }
    let ui = UI_LABELS.iter().copied().chain(crate::chrome::UI_COMMANDS.iter().map(|&(id, label, _)| (id, label)));
    for (id, label) in ui {
        if id == command {
            return tl!(label).to_string();
        }
    }
    command.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn over_limit_preferences_are_rejected_without_partial_changes() {
        let defaults = Keymap::default();
        let too_many =
            serde_json::to_string(&vec![serde_json::json!({"keys":"D","command":"select.clear"}); MAX_PREF_CHANGES + 1]).unwrap();
        let too_large = " ".repeat(MAX_PREF_BYTES + 1);
        for saved in [&too_many, &too_large] {
            let mut keymap = defaults.clone();
            assert!(!keymap.apply_preferences(saved));
            assert_eq!(keymap, defaults);
        }
        let mut changed = defaults.clone();
        changed.assign(Chord::parse("D").unwrap(), "test.command", Value::String("x".repeat(MAX_PREF_BYTES)));
        assert!(changed.preferences(&defaults).is_none(), "never save a value that cannot be loaded");
        let mut long_id = defaults.clone();
        long_id.assign(Chord::parse("D").unwrap(), &"x".repeat(MAX_COMMAND_LEN + 1), Value::Null);
        assert!(long_id.preferences(&defaults).is_none(), "never save a command id that loading rejects");
    }

    #[test]
    fn removed_registry_shortcut_stays_removed_after_preference_reload() {
        let registry =
            Registry { commands: vec![rustgis_engine::CommandSpec::new("test.action", "Test", "", |_, _| Ok(Value::Null)).key("F8")] };
        let defaults = Keymap::new(&registry);
        let chord = Chord::parse("F8").unwrap();
        assert_eq!(defaults.command_for(chord).unwrap().command, "test.action");
        let mut changed = defaults.clone();
        changed.remove(chord);
        let saved = changed.preferences(&defaults).unwrap();
        let mut reloaded = Keymap::new(&registry);
        reloaded.apply_preferences(&saved);
        assert!(reloaded.command_for(chord).is_none());
    }

    #[test]
    fn shortcut_preferences_round_trip_and_keep_new_defaults() {
        let defaults = Keymap::default();
        let mut changed = defaults.clone();
        let table = Chord::parse("Mod+T").unwrap();
        changed.remove(table);
        let d = Chord::parse("D").unwrap();
        assert!(changed.assign(d, "tool.set", serde_json::json!({"tool": "select"})));
        assert!(changed.assign(table, "select.clear", Value::Null));
        assert!(!changed.assign(Chord::parse("Mod+C").unwrap(), "select.clear", Value::Null));
        let saved = changed.preferences(&defaults).unwrap();
        let mut newer = defaults.clone();
        let fresh = Chord::parse("F8").unwrap();
        assert!(newer.assign(fresh, "new.command", Value::Null));
        newer.apply_preferences(&saved);
        assert_eq!(newer.command_for(d).unwrap().params, serde_json::json!({"tool": "select"}));
        assert_eq!(newer.command_for(table).unwrap().command, "select.clear");
        assert_eq!(newer.command_for(fresh).unwrap().command, "new.command");
        newer.apply_preferences("not json");
        assert_eq!(newer.command_for(table).unwrap().command, "select.clear");
    }

    #[test]
    fn malformed_preference_records_do_not_hide_valid_bindings() {
        let mut keymap = Keymap::default();
        let readable =
            keymap.apply_preferences(r#"[{"keys":"D","command":"select.clear"},{"keys":42},{"keys":"W","command":"table.open"}]"#);
        assert!(!readable, "an unreadable record blocks saving over the stored text");
        assert_eq!(keymap.command_for(Chord::parse("D").unwrap()).unwrap().command, "select.clear");
        assert_eq!(keymap.command_for(Chord::parse("W").unwrap()).unwrap().command, "table.open");
    }

    /// Undo and redo are a text field's own, an extra Alt or Shift aside; nothing else is.
    #[test]
    fn text_undo_chords_are_undo_and_redo() {
        let is = |s: &str| Chord::parse(s).unwrap().is_text_undo();
        assert!(is("Mod+Z") && is("Mod+Shift+Z") && is("Mod+Y") && is("Mod+Alt+Z"));
        assert!(!is("Mod+S") && !is("Z") && !is("Mod+K"));
    }

    #[test]
    fn browser_keys_read_by_key_or_code() {
        assert_eq!(browser_key("d", "KeyD"), Some(Key::D));
        assert_eq!(browser_key("D", "KeyD"), Some(Key::D));
        assert_eq!(browser_key("=", "Equal"), Some(Key::Equals));
        assert_eq!(browser_key("+", "Equal"), Some(Key::Plus));
        assert_eq!(browser_key("-", "Minus"), Some(Key::Minus));
        // ⌥D types ∂ on a Mac: the key's code still says D.
        assert_eq!(browser_key("∂", "KeyD"), Some(Key::D));
        assert_eq!(browser_key("¡", "Digit1"), Some(Key::Num1));
        assert_eq!(browser_key("Dead", "Unidentified"), None);
    }

    /// In a browser, the app's Ctrl (⌘) shortcuts are kept from the browser (⌘D would bookmark
    /// the page), the clipboard's and keys the app doesn't use are left to it.
    #[test]
    fn the_app_takes_its_shortcuts_from_the_browser() {
        let app = crate::RustGisApp::with_sample();
        let keys = app.command_keys();
        let takes = |key: &str, code: &str, cmd: bool, alt: bool, shift: bool| {
            crate::RustGisApp::takes_browser_key(&keys, key, code, cmd, alt, shift)
        };
        assert!(takes("d", "KeyD", true, false, false), "⌘D: duplicate, not bookmark");
        assert!(takes("y", "KeyY", true, false, false), "⌘Y: redo, not history");
        assert!(takes("k", "KeyK", true, false, false), "Ctrl+K: command search, not the address bar");
        assert!(takes("=", "Equal", true, false, false) && takes("+", "Equal", true, false, true), "⌘= and ⌘+ zoom the map, not the page");
        assert!(takes("z", "KeyZ", true, false, true), "⌘⇧Z: redo");
        // Without Ctrl (⌘), and keys the app leaves alone, go to the browser.
        assert!(!takes("d", "KeyD", false, false, false));
        assert!(!takes("r", "KeyR", true, false, false), "⌘R reloads");
        assert!(!takes("l", "KeyL", true, false, false), "⌘L: the address bar");
        // Copy, cut and paste must reach the page as the browser's clipboard events.
        for (k, c) in [("c", "KeyC"), ("x", "KeyX"), ("v", "KeyV")] {
            assert!(!takes(k, c, true, false, false), "⌘{k}");
        }
    }

    /// The web app rebuilds its list of taken keys only when the fingerprint moves: it must move
    /// when a shortcut is rebound.
    #[test]
    fn the_shortcuts_fingerprint_follows_the_keymap() {
        let mut app = crate::RustGisApp::with_sample();
        let before = app.shortcuts_fingerprint();
        assert_eq!(before, app.shortcuts_fingerprint(), "stable while nothing changes");
        let mod_d = Chord::parse("Mod+D");
        let Some(b) = app.ui.keymap.bindings.iter_mut().find(|b| Some(b.chord) == mod_d) else {
            panic!("Mod+D is bound");
        };
        b.chord = Chord { command: true, alt: false, shift: false, key: Key::J };
        assert_ne!(before, app.shortcuts_fingerprint());
        assert!(crate::RustGisApp::takes_browser_key(&app.command_keys(), "j", "KeyJ", true, false, false));
        // The same key given another command, or other parameters, changes it too.
        let moved = app.shortcuts_fingerprint();
        let j = Chord { command: true, alt: false, shift: false, key: Key::J };
        app.ui.keymap.assign(j, "select.clear", Value::Null);
        let other_command = app.shortcuts_fingerprint();
        assert_ne!(moved, other_command);
        // Assigning moves the binding to the end, so a second command for the same key, in the
        // same place, isolates the command.
        app.ui.keymap.assign(j, "select.all", Value::Null);
        assert_ne!(other_command, app.shortcuts_fingerprint());
        app.ui.keymap.assign(j, "tool.set", serde_json::json!({"tool": "select"}));
        let with_params = app.shortcuts_fingerprint();
        app.ui.keymap.assign(j, "tool.set", serde_json::json!({"tool": "measure"}));
        assert_ne!(with_params, app.shortcuts_fingerprint());
    }

    #[test]
    fn chords_read_and_write() {
        let c = Chord::parse("Mod+Shift+Z");
        assert_eq!(c, Some(Chord { command: true, alt: false, shift: true, key: Key::Z }));
        assert_eq!(c.map(|c| c.to_string()).as_deref(), Some("Mod+Shift+Z"));
        assert_eq!(Chord::parse("ctrl+t").map(|c| c.to_string()).as_deref(), Some("Mod+T"));
        assert_eq!(Chord::parse("Mod++").map(|c| (c.command, c.key)), Some((true, Key::Plus)));
        assert_eq!(Chord::parse("+").map(|c| (c.command, c.key)), Some((false, Key::Plus)));
        assert_eq!(Chord::parse("Mod+-").map(|c| c.key), Some(Key::Minus));
        assert_eq!(Chord::parse("d").map(|c| c.key), Some(Key::D));
        assert_eq!(Chord::parse("Esc").map(|c| c.key), Some(Key::Escape));
        for bad in ["", "Mod+", "Hyper+X", "Mod+Shift", "Mod+NoSuchKey"] {
            assert_eq!(Chord::parse(bad), None, "{bad}");
        }
        // Every spelling written out reads back the same.
        for b in Keymap::default().bindings {
            assert_eq!(Chord::parse(&b.chord.to_string()), Some(b.chord));
        }
    }

    /// Shown as egui reads the keys, by the OS it tells (a browser's from its user agent), not the
    /// one the app was built for: a web app on a Mac shows ⌘ (#799).
    #[test]
    fn chords_show_the_keys_of_the_os_egui_reads() {
        let c = Chord::parse("Mod+Shift+S");
        assert_eq!(c.map(|c| c.text(true)).as_deref(), Some("⇧⌘S"));
        assert_eq!(c.map(|c| c.text(false)).as_deref(), Some("Ctrl+Shift+S"));
        let app = RustGisApp::with_sample();
        let ctx = egui::Context::default();
        use egui::os::OperatingSystem as Os;
        for (os, undo) in [(Os::Mac, "⌘Z"), (Os::IOS, "⌘Z"), (Os::Windows, "Ctrl+Z")] {
            ctx.set_os(os);
            assert_eq!(shortcut_text(&ctx, &app, "edit.undo", &Value::Null), undo, "{os:?}");
        }
    }

    #[test]
    fn every_default_parses_once() {
        let map = Keymap::default();
        assert_eq!(map.bindings.len(), DEFAULTS.len(), "a default failed to parse");
        let mut seen = std::collections::HashSet::new();
        for b in &map.bindings {
            assert!(seen.insert(b.chord), "{} is bound twice", b.chord);
        }
    }

    #[test]
    fn more_modifiers_match_first() {
        let map = Keymap::default();
        let order = map.match_order();
        let pos = |keys: &str| Chord::parse(keys).and_then(|c| order.iter().position(|b| b.chord == c));
        assert!(pos("Mod+Shift+Z") < pos("Mod+Z"));
        assert!(pos("Mod+Shift+S") < pos("Mod+S"));
    }

    #[test]
    fn clipboard_keys_stay_events() {
        // Copy, Cut and Paste declare Mod+C/X/V: binding them too would run each twice.
        let app = RustGisApp::with_sample();
        assert!(app.ui.keymap.bindings.iter().all(|b| !is_clipboard(b.chord)));
        assert!(!shortcut_text(&egui::Context::default(), &app, "edit.copy", &Value::Null).is_empty(), "but Copy still shows its key");
        // A command's own key joins the defaults only where it is free.
        assert_eq!(app.ui.keymap.keys_for("layer.remove", &Value::Null).count(), 0, "Delete deletes features");
    }

    #[test]
    fn redo_has_two_keys() {
        let map = Keymap::default();
        let keys: Vec<String> = map.keys_for("edit.redo", &Value::Null).map(|c| c.to_string()).collect();
        assert_eq!(keys, ["Mod+Y", "Mod+Shift+Z"]);
        let explore = serde_json::json!({"tool": "explore"});
        assert_eq!(map.keys_for("tool.set", &explore).count(), 1);
        assert_eq!(map.keys_for("tool.set", &serde_json::json!({"tool": "select"})).count(), 0);
        // `{}` and `null` are both no parameters.
        assert_eq!(map.keys_for("table.open", &serde_json::json!({})).count(), 1);
    }
}
