//! macOS: the native menu bar (#1077), beside the ribbon rather than a copy of it. Each item runs
//! a command the app already has, so the menus add no behaviour of their own: File, View and Help
//! items run their commands as the ribbon does (asking to save first when one would replace the
//! project), the Edit items work as their keys do (on a focused text field, ModelBuilder's canvas,
//! or the project), and Quit RustGIS asks to save unsaved changes. The shortcuts shown are the
//! keymap's, and items grey out as their commands do; Undo and Redo don't, since a text field
//! undoes its own text when the project has nothing to undo.
//!
//! The Edit items are the app's own, not macOS's (`copy:`, `paste:`…), which egui's window doesn't
//! answer: muda leaves them enabled, so they would take ⌘C and ⌘V from egui. While a native panel
//! (a file dialog) has the keys, the items stay enabled but a choice is dropped, so it doesn't
//! reach the app once the panel closes; the panel's own text fields get no Edit items, as before
//! the menu bar.
//!
//! Built with `muda`; winit's own minimal menu is turned off where the event loop is made.

use std::sync::{Arc, Mutex};

use egui::{Key, KeyboardShortcut, Modifiers};
use muda::accelerator::{Accelerator, Code, Modifiers as MenuModifiers};
use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use rustgis_ui_egui::RustGisApp;
use rustgis_ui_egui::i18n::{self, Lang};
use serde_json::Value;

/// What an item of the app's own does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    /// Runs a command with these parameters (JSON, or empty for none), as the ribbon does.
    Command(&'static str, &'static str),
    /// Undo or Redo: as its key would when typed (a focused text field or ModelBuilder's canvas
    /// takes the Mac's, `fallback`; the keymap the one it gives the command), else runs the
    /// command. Shows the keymap's key, else `fallback` when no other command has it.
    Keys {
        command: &'static str,
        fallback: (Key, bool),
    },
    Cut,
    Copy,
    Paste,
    /// Selects all of a focused text field (⌘A, as egui reads it).
    SelectAll,
    /// Closes the window: the save prompt asks first.
    CloseWindow,
}

/// One row of a menu.
#[derive(Clone, Copy, Debug)]
enum Entry {
    /// An item of the app's own: its label, what it does, and a shortcut it always has.
    Item(&'static str, Action, Option<(Key, bool)>),
    /// One of macOS's own items, which act on the app or the window.
    Std(Std),
    Separator,
}

#[derive(Clone, Copy, Debug)]
enum Std {
    Services,
    Hide,
    HideOthers,
    ShowAll,
    Minimize,
    Zoom,
    FullScreen,
    BringAllToFront,
}

use Action::*;
use Entry::{Item, Separator};

/// The menus, in order: (title, rows). Titles and labels are translated; the first menu's title
/// is replaced by the app's name. They are catalog keys translated in the menu's language
/// ([`label`]), not `tl!` (a `const` can't call it), as keymap.rs's `UI_LABELS` are: new ones go
/// into the catalogs by hand (docs/i18n.md).
const MENUS: &[(&str, &[Entry])] = &[
    (
        "RustGIS",
        &[
            Item("About RustGIS", Command("ui.about", ""), None),
            Item("Check for Updates…", Command("ui.checkUpdate", ""), None),
            Separator,
            Item("Settings…", Command("ui.tab", r#"{"tab":"Settings"}"#), Some((Key::Comma, false))),
            Separator,
            Entry::Std(Std::Services),
            Separator,
            Entry::Std(Std::Hide),
            Entry::Std(Std::HideOthers),
            Entry::Std(Std::ShowAll),
            Separator,
            Item("Quit RustGIS", Command("ui.quit", ""), Some((Key::Q, false))),
        ],
    ),
    (
        "File",
        &[
            Item("New Project", Command("project.new", ""), None),
            Item("Open…", Command("ui.open", ""), None),
            Separator,
            Item("Add Data…", Command("ui.addData", ""), None),
            Separator,
            Item("Save", Command("ui.save", ""), None),
            Item("Save As…", Command("ui.saveAs", ""), None),
            Separator,
            Item("Close Window", CloseWindow, Some((Key::W, false))),
        ],
    ),
    (
        "Edit",
        &[
            Item("Undo", Keys { command: "edit.undo", fallback: (Key::Z, false) }, None),
            Item("Redo", Keys { command: "edit.redo", fallback: (Key::Z, true) }, None),
            Separator,
            Item("Cut", Cut, Some((Key::X, false))),
            Item("Copy", Copy, Some((Key::C, false))),
            Item("Paste", Paste, Some((Key::V, false))),
            Item("Select All", SelectAll, Some((Key::A, false))),
        ],
    ),
    (
        "View",
        &[
            Item("Layers", Command("ui.pane.contents", ""), None),
            Item("Browser", Command("ui.pane.catalog", ""), None),
            Item("Data Sources", Command("ui.pane.sources", ""), None),
            Separator,
            Item("Tools", Command("ui.pane.geoprocessing", ""), None),
            Item("Symbology", Command("ui.pane.symbology", ""), None),
            Item("Labeling", Command("ui.pane.labeling", ""), None),
            Item("Layer Properties", Command("ui.pane.layerProperties", ""), None),
            Item("Bookmarks", Command("ui.pane.bookmarks", ""), None),
            Item("History", Command("ui.pane.history", ""), None),
            Item("AI Assistant", Command("ui.pane.assistant", ""), None),
            Separator,
            Item("Command Search", Command("ui.commandSearch", ""), None),
            Item("Collapse or Expand the Ribbon", Command("ui.ribbon", ""), None),
            Separator,
            Entry::Std(Std::FullScreen),
        ],
    ),
    ("Window", &[Entry::Std(Std::Minimize), Entry::Std(Std::Zoom), Separator, Entry::Std(Std::BringAllToFront)]),
    (
        "Help",
        &[
            Item("RustGIS Help", Command("ui.learn", ""), None),
            Item("Keyboard Shortcuts", Command("ui.shortcuts", ""), None),
            Separator,
            Item("Report an Issue", Command("ui.learn", r#"{"url":"https://github.com/opengeos/RustGIS/issues/new/choose"}"#), None),
        ],
    ),
];

/// One of the app's own items in the menu bar.
struct Built {
    id: String,
    action: Action,
    /// The shortcut it shows: a changed keymap shows another, and builds the bar again.
    shortcut: Option<KeyboardShortcut>,
    item: MenuItem,
    enabled: bool,
}

/// The menu bar in use, and what its items do.
pub struct MenuBar {
    /// Kept alive: AppKit holds the menu, muda the items' actions.
    _menu: Menu,
    items: Vec<Built>,
    /// The language and shortcuts it was built with.
    built_for: Signature,
    /// The ids of items chosen and not acted on yet (muda calls back on the main thread).
    chosen: Arc<Mutex<Vec<String>>>,
}

/// The shortcut an item shows: one the item always has, else the keymap's first for its command
/// with ⌘ (a menu shortcut without one would take the key from text fields), redo preferring
/// ⇧⌘Z as Mac apps do.
fn shortcut(app: &RustGisApp, action: Action, fixed: Option<(Key, bool)>) -> Option<KeyboardShortcut> {
    let chord = |(key, shift): (Key, bool)| {
        KeyboardShortcut::new(if shift { Modifiers::COMMAND | Modifiers::SHIFT } else { Modifiers::COMMAND }, key)
    };
    if let Some(f) = fixed {
        return Some(chord(f));
    }
    let command = match action {
        Command(id, "") => id,
        // The keymap's key for the command, else the Mac's own, unless the keymap gives that to
        // another command (the item presses its key, so it would run that one).
        Keys { command, fallback } => {
            let keys: Vec<_> = app.shortcuts_of(command).into_iter().filter(|k| k.modifiers.command).collect();
            let own = chord(fallback);
            if keys.is_empty() {
                return app.shortcut_command(own).is_none_or(|c| c == command).then_some(own);
            }
            return keys.iter().find(|k| **k == own).or(keys.first()).copied();
        }
        _ => return None,
    };
    // ⌘Z, ⇧⌘Z and ⌘Y are a text field's own: a menu key would take them from it.
    app.shortcuts_of(command).into_iter().find(|k| k.modifiers.command && !matches!(k.logical_key, Key::Z | Key::Y))
}

/// Whether an item's command belongs to an extension that is off: the menus leave it out.
fn hidden(app: &RustGisApp, action: Action) -> bool {
    matches!(action, Command(id, _) if app.command_hidden(id))
}

/// The app's own items shown now, in order, with their shortcuts.
fn own_items(app: &RustGisApp) -> impl Iterator<Item = (String, &'static str, Action, Option<KeyboardShortcut>)> + '_ {
    MENUS.iter().enumerate().flat_map(move |(m, (_, rows))| {
        rows.iter().enumerate().filter_map(move |(r, row)| match *row {
            Item(label, action, fixed) if !hidden(app, action) => {
                Some((format!("rustgis.{m}.{r}"), label, action, shortcut(app, action, fixed)))
            }
            _ => None,
        })
    })
}

/// muda's accelerator for a shortcut, `None` for a key it has no code for.
fn accelerator(k: KeyboardShortcut) -> Option<Accelerator> {
    let mut mods = MenuModifiers::empty();
    if k.modifiers.command || k.modifiers.mac_cmd {
        mods |= MenuModifiers::META;
    }
    if k.modifiers.shift {
        mods |= MenuModifiers::SHIFT;
    }
    if k.modifiers.alt {
        mods |= MenuModifiers::ALT;
    }
    if k.modifiers.ctrl && !k.modifiers.command {
        mods |= MenuModifiers::CONTROL;
    }
    Some(Accelerator::new(mods, code(k.logical_key)?))
}

/// The key code of an egui key, for the keys shortcuts use.
fn code(key: Key) -> Option<Code> {
    use Key as K;
    Some(match key {
        K::A => Code::KeyA,
        K::B => Code::KeyB,
        K::C => Code::KeyC,
        K::D => Code::KeyD,
        K::E => Code::KeyE,
        K::F => Code::KeyF,
        K::G => Code::KeyG,
        K::H => Code::KeyH,
        K::I => Code::KeyI,
        K::J => Code::KeyJ,
        K::K => Code::KeyK,
        K::L => Code::KeyL,
        K::M => Code::KeyM,
        K::N => Code::KeyN,
        K::O => Code::KeyO,
        K::P => Code::KeyP,
        K::Q => Code::KeyQ,
        K::R => Code::KeyR,
        K::S => Code::KeyS,
        K::T => Code::KeyT,
        K::U => Code::KeyU,
        K::V => Code::KeyV,
        K::W => Code::KeyW,
        K::X => Code::KeyX,
        K::Y => Code::KeyY,
        K::Z => Code::KeyZ,
        K::Num0 => Code::Digit0,
        K::Num1 => Code::Digit1,
        K::Num2 => Code::Digit2,
        K::Num3 => Code::Digit3,
        K::Num4 => Code::Digit4,
        K::Num5 => Code::Digit5,
        K::Num6 => Code::Digit6,
        K::Num7 => Code::Digit7,
        K::Num8 => Code::Digit8,
        K::Num9 => Code::Digit9,
        K::Comma => Code::Comma,
        K::Period => Code::Period,
        K::Minus => Code::Minus,
        K::Equals => Code::Equal,
        // A key code is a key, not a character: + is ⇧= on some layouts and its own key on others.
        K::Plus => return None,
        K::Slash => Code::Slash,
        K::Backslash => Code::Backslash,
        K::Semicolon => Code::Semicolon,
        K::OpenBracket => Code::BracketLeft,
        K::CloseBracket => Code::BracketRight,
        K::Backtick => Code::Backquote,
        K::ArrowLeft => Code::ArrowLeft,
        K::ArrowRight => Code::ArrowRight,
        K::ArrowUp => Code::ArrowUp,
        K::ArrowDown => Code::ArrowDown,
        K::Home => Code::Home,
        K::End => Code::End,
        K::Enter => Code::Enter,
        K::F1 => Code::F1,
        K::F2 => Code::F2,
        K::F3 => Code::F3,
        K::F4 => Code::F4,
        K::F5 => Code::F5,
        K::F6 => Code::F6,
        K::F7 => Code::F7,
        K::F8 => Code::F8,
        K::F9 => Code::F9,
        K::F10 => Code::F10,
        K::F11 => Code::F11,
        K::F12 => Code::F12,
        _ => return None,
    })
}

/// Whether an item can be chosen now: as its command can, and none of the app's (Close Window
/// either) while `blocked`. The Edit items stay: a dialog's text field takes them.
fn enabled_now(app: &RustGisApp, action: Action, blocked: bool) -> bool {
    match action {
        Command(id, _) => !blocked && app.command_enabled(id),
        CloseWindow => !blocked,
        _ => true,
    }
}

/// What the items show and the language: a change builds the bar again.
#[derive(Clone, Debug, PartialEq)]
pub struct Signature(Lang, Vec<Option<KeyboardShortcut>>);

/// The menu bar's signature for the app now.
pub fn signature(app: &RustGisApp) -> Signature {
    Signature(app.chosen_lang(), own_items(app).map(|(.., k)| k).collect())
}

/// A label in the app's language, in logical order (AppKit lays out right-to-left text itself).
fn label(lang: Lang, s: &str) -> &str {
    i18n::tr_raw(lang, s)
}

fn std_item(s: Std, lang: Lang) -> PredefinedMenuItem {
    let t = |s: &str| Some(label(lang, s).to_string());
    match s {
        Std::Services => PredefinedMenuItem::services(t("Services").as_deref()),
        Std::Hide => PredefinedMenuItem::hide(t("Hide RustGIS").as_deref()),
        Std::HideOthers => PredefinedMenuItem::hide_others(t("Hide Others").as_deref()),
        Std::ShowAll => PredefinedMenuItem::show_all(t("Show All").as_deref()),
        Std::Minimize => PredefinedMenuItem::minimize(t("Minimize").as_deref()),
        Std::Zoom => PredefinedMenuItem::maximize(t("Zoom").as_deref()),
        Std::FullScreen => PredefinedMenuItem::fullscreen(t("Enter Full Screen").as_deref()),
        Std::BringAllToFront => PredefinedMenuItem::bring_all_to_front(t("Bring All to Front").as_deref()),
    }
}

impl MenuBar {
    /// Builds the menu bar for the app's language and keymap and makes it the app's. Call on the
    /// main thread, once the app has started.
    pub fn new(app: &RustGisApp, chosen: Arc<Mutex<Vec<String>>>) -> Result<MenuBar, String> {
        let lang = app.chosen_lang();
        let menu = Menu::new();
        let mut own = own_items(app);
        let mut items = Vec::new();
        let (mut window, mut help) = (None, None);
        for (title, rows) in MENUS {
            let sub = Submenu::new(label(lang, title), true);
            for row in *rows {
                match *row {
                    Item(_, action, _) if hidden(app, action) => {}
                    Item(..) => {
                        let (id, text, action, shortcut) = own.next().ok_or("the menu's items are out of step")?;
                        let enabled = enabled_now(app, action, false);
                        let item = MenuItem::with_id(id.clone(), label(lang, text), enabled, shortcut.and_then(accelerator));
                        sub.append(&item).map_err(|e| e.to_string())?;
                        items.push(Built { id, action, shortcut, item, enabled });
                    }
                    Entry::Std(s) => sub.append(&std_item(s, lang)).map_err(|e| e.to_string())?,
                    Separator => sub.append(&PredefinedMenuItem::separator()).map_err(|e| e.to_string())?,
                }
            }
            menu.append(&sub).map_err(|e| e.to_string())?;
            match *title {
                "Window" => window = Some(sub),
                "Help" => help = Some(sub),
                _ => {}
            }
        }
        menu.init_for_nsapp();
        // Once the bar is the app's, as muda asks: macOS lists the windows in the Window menu
        // and adds its search field, which finds any menu item, to the Help menu.
        if let Some(w) = window {
            w.set_as_windows_menu_for_nsapp();
        }
        if let Some(h) = help {
            h.set_as_help_menu_for_nsapp();
        }
        Ok(MenuBar { _menu: menu, items, built_for: signature(app), chosen })
    }

    /// What it was built for.
    pub fn built_for(&self) -> &Signature {
        &self.built_for
    }

    /// Greys out the items whose commands can't run now, and all of them while `blocked` (a
    /// dialog such as Save changes? has the keys), and back.
    pub fn follow(&mut self, app: &RustGisApp, blocked: bool) {
        for b in &mut self.items {
            let now = enabled_now(app, b.action, blocked);
            if now != b.enabled {
                b.item.set_enabled(now);
                b.enabled = now;
            }
        }
    }

    /// What the items chosen since the last call do, with the shortcuts they show, in order.
    pub fn take_chosen(&self) -> Vec<(Action, Option<KeyboardShortcut>)> {
        let ids = std::mem::take(&mut *self.chosen.lock().unwrap_or_else(|e| e.into_inner()));
        ids.iter().filter_map(|id| self.items.iter().find(|b| b.id == *id).map(|b| (b.action, b.shortcut))).collect()
    }
}

/// The text on the system clipboard, for Paste, as egui-winit pastes it (Windows line endings
/// as \n).
pub fn clipboard_text() -> Option<String> {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::NSString;
    // NSPasteboardTypeString's value, so no extern static is read.
    let text = NSPasteboard::generalPasteboard().stringForType(&NSString::from_str("public.utf8-plain-text"))?;
    Some(text.to_string().replace("\r\n", "\n")).filter(|t| !t.is_empty())
}

/// The key combinations the menu bar keeps: its items' fixed ones and macOS's own (Hide, Hide
/// Others, Minimize). A shortcut on one would never reach the app.
pub fn reserved() -> Vec<KeyboardShortcut> {
    let cmd = |m: Modifiers, key: Key| KeyboardShortcut::new(Modifiers::COMMAND | m, key);
    let mut keys: Vec<KeyboardShortcut> = MENUS
        .iter()
        .flat_map(|(_, rows)| rows.iter())
        .filter_map(|row| match *row {
            Item(_, _, Some((key, shift))) => Some(cmd(if shift { Modifiers::SHIFT } else { Modifiers::NONE }, key)),
            _ => None,
        })
        .collect();
    keys.extend([cmd(Modifiers::NONE, Key::H), cmd(Modifiers::ALT, Key::H), cmd(Modifiers::NONE, Key::M)]);
    keys
}

/// Whether a native panel (a file dialog) has the keys rather than the app's window.
fn panel_has_keys() -> bool {
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::NSApplication;
    let Some(mtm) = MainThreadMarker::new() else { return false };
    let app = NSApplication::sharedApplication(mtm);
    match (app.keyWindow(), app.mainWindow()) {
        (Some(key), Some(main)) => !std::ptr::eq(Retained::as_ptr(&key), Retained::as_ptr(&main)),
        // Without both, it can't be told: the choice is kept rather than lost.
        _ => false,
    }
}

/// Sends each item chosen to `chosen` and wakes the app (egui draws only on events). muda calls
/// back on the main thread while AppKit handles the choice.
pub fn listen(chosen: Arc<Mutex<Vec<String>>>, ctx: egui::Context) {
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if panel_has_keys() {
            return;
        }
        chosen.lock().unwrap_or_else(|e| e.into_inner()).push(e.id.0);
        ctx.request_repaint();
    }));
}

/// The command and parameters an item runs (Undo and Redo outside a text field).
pub fn command_of(action: Action) -> Option<(&'static str, Value)> {
    match action {
        Command(id, params) => {
            Some((id, if params.is_empty() { Value::Null } else { serde_json::from_str(params).unwrap_or(Value::Null) }))
        }
        Keys { command, .. } => Some((command, Value::Null)),
        _ => None,
    }
}

/// The key Undo or Redo presses in a focused text field: the Mac's own, which the field takes
/// (the keymap leaves ⌘Z and ⇧⌘Z to a focused field, whatever they're bound to).
pub fn text_key(action: Action) -> Option<KeyboardShortcut> {
    match action {
        Keys { fallback: (key, shift), .. } => {
            Some(KeyboardShortcut::new(if shift { Modifiers::COMMAND | Modifiers::SHIFT } else { Modifiers::COMMAND }, key))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::{Project, Session};

    /// Every item runs a command that exists, with parameters that parse; a fixed shortcut isn't
    /// one the keymap gives another command; every shortcut shown has ⌘ and a key code.
    #[test]
    fn items_name_commands_that_exist() {
        let mut app = RustGisApp::new(Session::new(Project::default()));
        // Those that open a native file dialog, quit or go online aren't run here.
        const NOT_RUN: &[&str] = &["ui.open", "ui.save", "ui.saveAs", "ui.addData", "ui.quit", "ui.checkUpdate"];
        let mut seen = 0;
        for (_, rows) in MENUS {
            for row in *rows {
                let Item(label, action, fixed) = *row else { continue };
                if let Some((id, params)) = command_of(action) {
                    if let Command(_, p) = action {
                        assert!(p.is_empty() || serde_json::from_str::<Value>(p).is_ok(), "{label}: {p}");
                    }
                    if !id.starts_with("ui.") {
                        assert!(app.session.registry.get(id).is_some(), "{label}: no command {id}");
                    } else if !NOT_RUN.contains(&id) {
                        // A ui.* command the app doesn't know says so.
                        if let Err(e) = app.run(id, params) {
                            assert!(!e.starts_with("unknown command"), "{label}: {e}");
                        }
                    }
                    seen += 1;
                }
                if let Keys { command, .. } = action {
                    assert!(app.session.registry.get(command).is_some(), "{label}: no command {command}");
                }
                if let Some(k) = shortcut(&app, action, fixed) {
                    assert!(k.modifiers.command, "{label}: a menu shortcut needs ⌘");
                    assert!(accelerator(k).is_some(), "{label}: no key code for {:?}", k.logical_key);
                }
                if let Some((key, shift)) = fixed {
                    let k = KeyboardShortcut::new(if shift { Modifiers::COMMAND | Modifiers::SHIFT } else { Modifiers::COMMAND }, key);
                    let taken = app.shortcut_command(k);
                    assert!(taken.is_none(), "{label}: the keymap gives {key:?} to {taken:?}");
                }
            }
        }
        assert!(seen > 10);
    }

    /// The shortcuts shown are the keymap's: Save, Open and New have theirs, Redo the Mac's ⇧⌘Z.
    #[test]
    fn shortcuts_follow_the_keymap() {
        let app = RustGisApp::new(Session::new(Project::default()));
        let k = |a: Action| shortcut(&app, a, None).map(|k| (k.logical_key, k.modifiers.shift));
        assert_eq!(k(Command("ui.save", "")), Some((Key::S, false)));
        assert_eq!(k(Command("ui.saveAs", "")), Some((Key::S, true)));
        assert_eq!(k(Command("project.new", "")), Some((Key::N, false)));
        assert_eq!(k(Command("ui.open", "")), Some((Key::O, false)));
        assert_eq!(k(UNDO), Some((Key::Z, false)));
        assert_eq!(k(REDO), Some((Key::Z, true)));
        // A command whose shortcut has no ⌘ (Home zooms to the full extent) shows none.
        assert_eq!(k(Command("map.zoomFull", "")), None);
        assert_eq!(command_of(Command("ui.tab", r#"{"tab":"Settings"}"#)).map(|c| c.1["tab"].clone()), Some(Value::from("Settings")));
    }

    /// The AI Assistant's item leaves the menus while its extension is off, and the bar is built
    /// again for it.
    #[test]
    fn the_items_of_an_extension_that_is_off_are_left_out() {
        let mut app = RustGisApp::new(Session::new(Project::default()));
        let offered = |app: &RustGisApp| own_items(app).any(|(_, label, ..)| label == "AI Assistant");
        assert!(offered(&app));
        let before = signature(&app);
        assert!(app.run("extension.disable", serde_json::json!({"id": "assistant"})).is_ok());
        assert!(!offered(&app));
        assert_ne!(signature(&app), before);
    }

    const UNDO: Action = Keys { command: "edit.undo", fallback: (Key::Z, false) };
    const REDO: Action = Keys { command: "edit.redo", fallback: (Key::Z, true) };

    /// A shortcut rebound in the keymap moves to its new item (the bar is built again), and Undo
    /// doesn't press ⌘Z once the keymap gives it to another command.
    #[test]
    fn rebinding_moves_the_shortcut() {
        let mut app = RustGisApp::new(Session::new(Project::default()));
        let shown = |app: &RustGisApp| own_items(app).map(|(_, l, _, k)| (l, k)).collect::<Vec<_>>();
        let before = shown(&app);
        assert!(app.ui.keymap.apply_preferences(r#"[{"keys": "Mod+J", "command": "ui.pane.history"}]"#));
        let history = shown(&app);
        assert_ne!(history, before);
        let j = |items: &[(&str, Option<KeyboardShortcut>)], l: &str| {
            items.iter().find(|(x, _)| *x == l).and_then(|(_, k)| *k).map(|k| k.logical_key)
        };
        assert_eq!(j(&history, "History"), Some(Key::J));
        // The same chord to another command: the list of chords is the same, the items aren't.
        assert!(app.ui.keymap.apply_preferences(r#"[{"keys": "Mod+J", "command": "ui.pane.bookmarks"}]"#));
        let bookmarks = shown(&app);
        assert_eq!((j(&bookmarks, "History"), j(&bookmarks, "Bookmarks")), (None, Some(Key::J)));
        // ⌘Z zooms in now: Undo shows no shortcut rather than pressing it.
        assert!(app.ui.keymap.apply_preferences(r#"[{"keys": "Mod+Z", "command": "map.zoomIn"}]"#));
        assert_eq!(shortcut(&app, UNDO, None), None);
        // Redo keeps ⇧⌘Z.
        assert_eq!(shortcut(&app, REDO, None).map(|k| (k.logical_key, k.modifiers.shift)), Some((Key::Z, true)));
        // Undo still runs edit.undo outside a text field, and presses ⌘Z (the field's) in one,
        // never the command ⌘Z is bound to.
        assert_eq!(command_of(UNDO).map(|c| c.0), Some("edit.undo"));
        assert_eq!(text_key(UNDO).map(|k| (k.logical_key, k.modifiers.shift)), Some((Key::Z, false)));
        // A menu command rebound to ⌘Z doesn't show it: a text field keeps its undo.
        assert!(app.ui.keymap.apply_preferences(r#"[{"keys": "Mod+Z", "command": "ui.pane.history"}]"#));
        assert_eq!(shortcut(&app, Command("ui.pane.history", ""), None), None);
        // The bar's signature follows: a new build for the changed shortcuts.
        let before = signature(&app);
        assert!(app.ui.keymap.apply_preferences(r#"[{"keys": "Mod+J", "command": "ui.pane.history"}]"#));
        assert_ne!(signature(&app), before);
    }

    /// Behind a dialog, the app's items grey out, Close Window too; the Edit items stay for a
    /// dialog's text field. ⌘+ gets no menu key (a key code can't say "+").
    #[test]
    fn items_grey_out_behind_a_dialog() {
        let app = RustGisApp::new(Session::new(Project::default()));
        assert!(enabled_now(&app, Command("ui.about", ""), false));
        assert!(!enabled_now(&app, Command("ui.about", ""), true));
        assert!(!enabled_now(&app, CloseWindow, true));
        assert!(enabled_now(&app, Paste, true));
        assert!(enabled_now(&app, UNDO, true));
        assert!(accelerator(KeyboardShortcut::new(Modifiers::COMMAND, Key::Plus)).is_none());
        assert!(accelerator(KeyboardShortcut::new(Modifiers::COMMAND, Key::Equals)).is_some());
    }
}
