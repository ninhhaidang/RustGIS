//! The pane in use: the part of the window the user last pressed, dragged or scrolled in. A few
//! keys mean different things in different panes (Delete removes a layer in the Layers pane and
//! features on the map), and [`crate::chrome::shortcuts`] routes them by it (#2945).
//!
//! Hovering doesn't count. A dialog over the panes is none of them, and a menu or a pop-up leaves
//! the pane it was opened from in use.

use egui::{Key, LayerId, Modifiers, Order, PointerButton, Pos2, Rect};
use serde_json::Value;

use crate::keymap::Chord;
use crate::{LeftPane, RustGisApp};

/// A part of the window a key can act on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    /// The map, layout or ModelBuilder canvas in view. Also before anything was pressed, and in a
    /// shell that draws its own interface: the keys act as they do on the map.
    #[default]
    View,
    /// The tabs of the maps and layouts, above the view.
    Tabs,
    /// The layer list (the Layers pane, under its title).
    Layers,
    /// The attribute table.
    Table,
    /// The rest: the ribbon, the status bar, any other pane.
    Other,
}

/// A key pressed on this frame (not a held key's repeat), and what was so as it went down.
#[derive(Clone, Copy)]
pub struct Press {
    key: Key,
    modifiers: Modifiers,
    /// The pane in use then: a press or a wheel turn later on the frame doesn't change whose key
    /// it was.
    pub pane: Pane,
    /// Whether a click was under way: a mouse button was down as the frame began, or went down or
    /// up earlier on it. The panes take a click only as they are drawn, after the keys are read.
    pub after_click: bool,
}

/// A key's command kept until the frame's panes and dialogs are drawn
/// ([`crate::chrome::late_keys`]).
pub struct Late {
    pub id: String,
    pub params: Value,
    /// The pane in use as the key went down.
    pub pane: Pane,
    /// The shortcut pressed, or `None` for a key that is no shortcut (Backspace).
    pub chord: Option<Chord>,
}

/// Which pane is in use, and where the panes were drawn.
#[derive(Default)]
pub struct InUse {
    pane: Pane,
    /// Where each pane was drawn on the last frame, and on which layer, in drawing order: a pane
    /// drawn inside another comes after it.
    drawn: Vec<(Pane, Rect, LayerId)>,
    /// The project the pane was pressed in ([`rustgis_engine::Session::opened`]).
    project: u64,
    /// Where the pointer was when the last frame ended.
    pointer: Option<Pos2>,
    /// The mouse buttons that are down by this frame's events, a bit each (a finger is the first).
    down: u8,
    /// Whether a click was under way at some point of this frame: a button was down as it began,
    /// or went down or up on it.
    clicking: bool,
    /// The keys pressed on this frame, in order.
    presses: Vec<Press>,
    /// The keys that are down, each with the pane that was in use as it went down: a held key's
    /// repeats are that pane's.
    held: Vec<(Key, Pane)>,
    /// This frame's keys kept until its panes are drawn, and the dialogs that were up as they
    /// were pressed (a number that differs when they do).
    late: Vec<Late>,
    late_dialogs: u64,
}

impl InUse {
    /// Records where `pane` is drawn this frame, and on which layer (`Ui::layer_id`).
    pub fn drawn(&mut self, pane: Pane, layer: LayerId, rect: Rect) {
        self.drawn.push((pane, rect, layer));
    }
}

/// A mouse button's bit in [`InUse::down`].
fn bit(button: PointerButton) -> u8 {
    match button {
        PointerButton::Primary => 1,
        PointerButton::Secondary => 2,
        PointerButton::Middle => 4,
        PointerButton::Extra1 => 8,
        PointerButton::Extra2 => 16,
    }
}

/// Takes this frame's presses and scrolling: the pane under the pointer at each is the one in use
/// from there on, for the keys that follow it on the frame too. Called once a frame, before the
/// keys are read and the panes are drawn.
pub fn follow(app: &mut RustGisApp, ctx: &egui::Context) {
    let drawn = std::mem::take(&mut app.ui.in_use.drawn);
    let in_use = &mut app.ui.in_use;
    // Another project is nothing the user has worked in yet: Delete must not remove a layer of it
    // because the last project's layer list was in use.
    if in_use.project != app.session.opened {
        in_use.project = app.session.opened;
        in_use.pane = Pane::default();
    }
    // A pane closed (by a key or a command, with no press elsewhere) is no longer in use, and
    // isn't again when it is opened. Nor is one pressed where it was drawn as it closed. And no
    // pane is drawn behind the File menu's backstage, which a command can open with no press.
    let open = |pane: Pane| {
        !app.ui.backstage
            && match pane {
                Pane::Layers => app.ui.left == Some(LeftPane::Contents),
                Pane::Table => app.session.table_layer.is_some() || app.session.table_standalone.is_some(),
                _ => true,
            }
    };
    if !open(in_use.pane) {
        in_use.pane = Pane::Other;
    }
    // Where the pointer was at each press, wheel turn and zoom (not where it is by the end of the
    // frame), and for each key how many of them came before it.
    let mut pointer = in_use.pointer;
    let mut down = in_use.down;
    let mut clicked = down != 0;
    let mut worked: Vec<Pos2> = Vec::new();
    // Each key going down (with its modifiers, how many of them came before and whether a click
    // was under way) or up (`None`), in order.
    let mut keys: Vec<(Key, Option<(Modifiers, usize, bool)>)> = Vec::new();
    ctx.input(|i| {
        for e in &i.events {
            match e {
                egui::Event::PointerMoved(pos) => pointer = Some(*pos),
                egui::Event::PointerButton { pos, button, pressed, .. } => {
                    pointer = Some(*pos);
                    clicked = true;
                    if *pressed {
                        down |= bit(*button);
                        worked.push(*pos);
                    } else {
                        down &= !bit(*button);
                    }
                }
                egui::Event::MouseWheel { .. } | egui::Event::Zoom(_) => worked.extend(pointer),
                egui::Event::PointerGone => pointer = None,
                // The system took the touch away, or the window lost the keyboard (a release
                // elsewhere isn't told): no release will come.
                egui::Event::Touch { phase: egui::TouchPhase::Cancel, .. } | egui::Event::WindowFocused(false) => down = 0,
                egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => {
                    keys.push((*key, Some((*modifiers, worked.len(), clicked))));
                }
                egui::Event::Key { key, pressed: false, .. } => keys.push((*key, None)),
                _ => {}
            }
        }
    });
    in_use.pointer = pointer;
    in_use.down = down;
    in_use.clicking = clicked;
    // The pane after each of them, in turn.
    let modal = ctx.memory(|m| m.top_modal_layer());
    let mut panes = vec![in_use.pane];
    for at in worked {
        // What floats there over the window's own layer (a dialog, a menu, a pop-up), if anything.
        let over = ctx.layer_id_at(at);
        let hit = drawn
            .iter()
            .rev()
            .find(|(pane, rect, layer)| open(*pane) && rect.contains(at) && over.is_none_or(|o| o == *layer))
            .map(|(pane, ..)| *pane);
        let before = panes.last().copied().unwrap_or_default();
        panes.push(match hit {
            Some(pane) => pane,
            // A modal dialog (Command Search), wherever in the window the press lands.
            None if over.is_some() && over == modal => Pane::Other,
            // A menu or a pop-up that no pane says is its own: the pane it was opened from stays.
            None if over.is_some_and(|o| matches!(o.order, Order::Foreground | Order::Tooltip | Order::Debug)) => before,
            None => Pane::Other,
        });
    }
    in_use.pane = panes.last().copied().unwrap_or_default();
    in_use.presses.clear();
    for (key, pressed) in keys {
        in_use.held.retain(|(held, _)| *held != key);
        if let Some((modifiers, worked, after_click)) = pressed {
            let pane = panes.get(worked).copied().unwrap_or(in_use.pane);
            in_use.presses.push(Press { key, modifiers, pane, after_click });
            in_use.held.push((key, pane));
        }
    }
}

/// The pane in use.
pub fn pane(app: &RustGisApp) -> Pane {
    app.ui.in_use.pane
}

/// Whether a mouse button or a finger is down, by this frame's events.
pub fn down(app: &RustGisApp) -> bool {
    app.ui.in_use.down != 0
}

/// Whether a click was under way at some point of this frame.
pub fn clicking(app: &RustGisApp) -> bool {
    app.ui.in_use.clicking
}

/// The pane that was in use as `key`, still down, was pressed.
pub fn held_in(app: &RustGisApp, key: Key) -> Option<Pane> {
    app.ui.in_use.held.iter().find(|(held, _)| *held == key).map(|(_, pane)| *pane)
}

/// The first press of `key` on this frame that isn't a held key's repeat: with modifiers that
/// match `chord` as a shortcut's do (Shift and Alt may be held besides), or with none at all for
/// `None`. The one event says both whether a shortcut was pressed anew and when.
pub fn press(app: &RustGisApp, key: Key, chord: Option<Modifiers>) -> Option<Press> {
    let matches = |p: &&Press| p.key == key && chord.map_or(p.modifiers.is_none(), |chord| p.modifiers.matches_logically(chord));
    app.ui.in_use.presses.iter().find(matches).copied()
}

/// Keeps this frame's keys that run after its panes are drawn ([`crate::chrome::late_keys`]),
/// with `dialogs`, a number for the dialogs that are up now.
pub fn keep_late(app: &mut RustGisApp, late: Vec<Late>, dialogs: u64) {
    app.ui.in_use.late = late;
    app.ui.in_use.late_dialogs = dialogs;
}

/// The keys kept by [`keep_late`], once, and its `dialogs`.
pub fn take_late(app: &mut RustGisApp) -> (Vec<Late>, u64) {
    (std::mem::take(&mut app.ui.in_use.late), app.ui.in_use.late_dialogs)
}

#[cfg(test)]
mod tests {
    use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, vec2};
    use egui_kittest::kittest::Queryable;
    use serde_json::{Value, json};

    use super::*;

    type Window = egui_kittest::Harness<'static, RustGisApp>;

    /// The whole app as its window runs it, so the panes say where they are and the keys are
    /// read from where a frame reads them. No right pane: a layer's name is only in the list.
    /// Frames a quarter of a second long, so two clicks on a layer are never a double-click.
    fn window(mut app: RustGisApp) -> Window {
        app.ui.right_open.clear();
        app.ui.right = None;
        let mut h = egui_kittest::Harness::builder().with_size(vec2(1200.0, 900.0)).with_max_steps(64).with_step_dt(0.25).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            app,
        );
        h.run_steps(6);
        h
    }

    /// Where `pane` was drawn on the last frame.
    fn rect(h: &Window, pane: Pane) -> Rect {
        h.state().ui.in_use.drawn.iter().find(|d| d.0 == pane).map(|d| d.1).unwrap()
    }

    fn button(pos: Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    /// A press and release of the mouse at `pos`.
    fn press(h: &mut Window, pos: Pos2) {
        h.event(Event::PointerMoved(pos));
        h.step();
        h.event(button(pos, true));
        h.step();
        h.event(button(pos, false));
        h.run_steps(2);
    }

    /// Events that all arrive in one frame, as on a slow one (the harness otherwise gives each
    /// event a frame of its own); only that frame is run.
    fn one_frame(h: &mut Window, events: impl IntoIterator<Item = Event>) {
        h.input_mut().events.extend(events);
        h.step();
    }

    /// [`one_frame`], and the frames after it in which what it started ends.
    fn in_one_frame(h: &mut Window, events: impl IntoIterator<Item = Event>) {
        one_frame(h, events);
        h.run_steps(2);
    }

    /// The Delete key going down, or up.
    fn delete(pressed: bool) -> Event {
        Event::Key { key: Key::Delete, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }
    }

    /// A turn of the mouse wheel.
    fn wheel() -> Event {
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, 1.0),
            modifiers: Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        }
    }

    /// A key pressed and let go.
    fn key(h: &mut Window, key: Key) {
        h.key_press(key);
        h.run_steps(2);
    }

    fn layer_names(h: &Window) -> Vec<String> {
        h.state().session.map().layers.iter().map(|l| l.name.clone()).collect()
    }

    fn cities(h: &Window) -> (usize, usize) {
        let l = h.state().session.map().layer_by_name("Cities").unwrap();
        (l.dataset.features.len(), l.selection.len())
    }

    /// Whether a notice with exactly this text shows.
    fn says(h: &Window, text: &str) -> bool {
        h.state().map.toasts.iter().any(|t| t.text == text)
    }

    /// Features selected, then a layer clicked in the list: Delete (and Backspace) removes that
    /// layer in one Undo step and leaves the features. Once the map is scrolled, Delete is for the
    /// features again (#2945).
    #[test]
    fn delete_removes_layers_in_the_layer_list_and_features_on_the_map() {
        let mut app = RustGisApp::with_sample();
        app.run("edit.enable", json!({"on": true})).unwrap();
        app.run("select.all", json!({"layer": "Cities"})).unwrap();
        let mut h = window(app);
        let (before, all) = (layer_names(&h), cities(&h));
        assert!(all.0 > 0 && all.0 == all.1, "every city is selected: {all:?}");
        assert_eq!(pane(h.state()), Pane::View, "nothing pressed yet");

        for delete in [Key::Delete, Key::Backspace] {
            h.get_by_label("Rivers").click();
            h.run_steps(2);
            assert_eq!(pane(h.state()), Pane::Layers);
            key(&mut h, delete);
            assert!(!layer_names(&h).contains(&"Rivers".to_string()), "{delete:?} removed the layer clicked");
            assert_eq!(layer_names(&h).len(), before.len() - 1, "and only that one");
            assert_eq!(cities(&h), all, "the selected features stay");
            h.state_mut().run("edit.undo", Value::Null).unwrap();
            h.run_steps(2);
            assert_eq!(layer_names(&h), before, "one Undo brings the layer back");
        }

        // The key held: its repeats remove nothing more, though the next layer is now the active
        // one. Shift+Backspace is another key.
        h.get_by_label("Rivers").click();
        h.run_steps(2);
        h.key_down(Key::Delete);
        h.run_steps(2);
        assert_eq!(layer_names(&h).len(), before.len() - 1);
        // Down again with no release between: a repeat, as a held key sends.
        h.key_down(Key::Delete);
        h.run_steps(2);
        // Still held, and the wheel turned over the map: the key stays the layer list's, and its
        // repeats don't reach the map's selected features.
        let map = rect(&h, Pane::View).center();
        in_one_frame(&mut h, [Event::PointerMoved(map), wheel()]);
        h.key_down(Key::Delete);
        h.run_steps(2);
        assert_eq!(cities(&h), all, "a key held since the layer list was in use deletes no features");
        h.key_up(Key::Delete);
        h.get_by_label("Cities").click();
        h.run_steps(2);
        h.key_press_modifiers(Modifiers::SHIFT, Key::Backspace);
        h.run_steps(2);
        assert_eq!(layer_names(&h).len(), before.len() - 1, "one press removes one layer: {:?}", layer_names(&h));
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        h.run_steps(2);

        // A click and Delete in one frame: the key waits for the click, and removes the layer it
        // picked, not the one picked before.
        h.get_by_label("Cities").click();
        h.run_steps(2);
        let rivers = h.get_by_label("Rivers").rect().center();
        let only_rivers_went = |h: &mut Window, when: &str| {
            assert!(
                !layer_names(h).contains(&"Rivers".to_string()) && layer_names(h).len() == before.len() - 1,
                "{when}: {:?}",
                layer_names(h)
            );
            assert_eq!(cities(h), all, "{when}: the selected features stay");
            h.state_mut().run("edit.undo", Value::Null).unwrap();
            h.run_steps(2);
            assert_eq!(layer_names(h), before);
            h.get_by_label("Cities").click();
            h.run_steps(2);
        };
        let click_and_delete = [Event::PointerMoved(rivers), button(rivers, true), button(rivers, false), delete(true), delete(false)];
        in_one_frame(&mut h, click_and_delete.clone());
        only_rivers_went(&mut h, "clicked and pressed in one frame");
        // Another layer clicked on the very next frame: the key was for the first click.
        let cities_row = h.get_by_label("Cities").rect().center();
        one_frame(&mut h, click_and_delete);
        in_one_frame(&mut h, [Event::PointerMoved(cities_row), button(cities_row, true), button(cities_row, false)]);
        only_rivers_went(&mut h, "another layer clicked on the next frame");
        // The key first, then the click, in one frame: the key was for the layer picked before.
        in_one_frame(&mut h, [delete(true), delete(false), Event::PointerMoved(rivers), button(rivers, true), button(rivers, false)]);
        assert!(!layer_names(&h).contains(&"Cities".to_string()) && layer_names(&h).len() == before.len() - 1, "{:?}", layer_names(&h));
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        h.run_steps(2);
        h.get_by_label("Cities").click();
        h.run_steps(2);
        // The button still down when the key comes: what the click picks isn't known yet, so the
        // key is dropped, and nothing goes once the button is let go either.
        one_frame(&mut h, [Event::PointerMoved(rivers), button(rivers, true)]);
        // Another button pressed and let go meanwhile leaves the first one down.
        let middle = |pressed| Event::PointerButton { pos: rivers, button: PointerButton::Middle, pressed, modifiers: Modifiers::NONE };
        one_frame(&mut h, [middle(true), middle(false)]);
        in_one_frame(&mut h, [delete(true), delete(false)]);
        assert_eq!(layer_names(&h), before, "nothing is removed while the button is down");
        h.event(button(rivers, false));
        h.run_steps(3);
        assert_eq!(layer_names(&h), before, "nor once it is let go");
        // A touch the system took away: no release comes, and the keys run as before.
        let cancel = Event::Touch {
            device_id: egui::TouchDeviceId(0),
            id: egui::TouchId(0),
            phase: egui::TouchPhase::Cancel,
            pos: rivers,
            force: None,
        };
        in_one_frame(&mut h, [Event::PointerMoved(rivers), button(rivers, true), cancel, Event::PointerGone]);
        key(&mut h, Key::Delete);
        assert_eq!(layer_names(&h).len(), before.len() - 1, "the key runs though no release came: {:?}", layer_names(&h));
        h.state_mut().run("edit.undo", Value::Null).unwrap();
        h.run_steps(2);
        h.get_by_label("Cities").click();
        h.run_steps(2);
        // A click that puts the cursor in the filter box: the key that came with it is the box's.
        let filter = rect(&h, Pane::Layers).center_top() + vec2(0.0, 20.0);
        in_one_frame(&mut h, [Event::PointerMoved(filter), button(filter, true), button(filter, false), delete(true), delete(false)]);
        assert!(h.ctx.egui_wants_keyboard_input(), "the click is in the filter box");
        assert_eq!(layer_names(&h), before, "Delete in a text field removes no layer");
        h.key_press(Key::Escape);
        h.run_steps(2);

        // The wheel over the map: the map is in use, with the layer list's click still the last.
        h.get_by_label("Cities").click();
        h.run_steps(2);
        let at = rect(&h, Pane::View).center();
        h.event(Event::PointerMoved(at));
        h.step();
        h.event(wheel());
        h.run_steps(2);
        assert_eq!(pane(h.state()), Pane::View);
        // Delete, then a layer clicked, in one frame: the key was pressed on the map.
        in_one_frame(&mut h, [delete(true), delete(false), Event::PointerMoved(rivers), button(rivers, true), button(rivers, false)]);
        assert_eq!(pane(h.state()), Pane::Layers);
        assert_eq!(layer_names(&h), before, "Delete on the map removes no layer");
        assert_eq!(cities(&h), (0, 0), "it deletes the selected features");
    }

    /// F2 renames what the pane in use shows: the map or layout on the tabs and in a layout, the
    /// active layer in the layer list and on the map.
    #[test]
    fn f2_renames_the_map_or_layout_on_its_tab_and_the_layer_elsewhere() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Sheet"})).unwrap();
        app.run("layout.close", Value::Null).unwrap();
        let map = app.session.map().name.clone();
        let mut h = window(app);
        // What F2 opens, and the name it offers; the dialog is closed again.
        let f2 = |h: &mut Window| {
            key(h, Key::F2);
            let asked = (h.state().dialogs.open, h.state().dialogs.text.clone());
            h.state_mut().dialogs.open = None;
            h.run_steps(2);
            asked
        };

        // The first tab is the map in view.
        let tabs = rect(&h, Pane::Tabs);
        press(&mut h, tabs.left_center() + vec2(40.0, 0.0));
        assert_eq!(pane(h.state()), Pane::Tabs);
        assert_eq!(f2(&mut h), (Some("renameMap"), map));

        h.state_mut().run("layout.activate", json!({"name": "Sheet"})).unwrap();
        h.run_steps(2);
        assert_eq!(f2(&mut h), (Some("renameLayout"), "Sheet".to_string()), "on the tabs, with a layout in view");
        let view = rect(&h, Pane::View);
        press(&mut h, view.left_top() + vec2(12.0, 12.0));
        assert_eq!(pane(h.state()), Pane::View);
        assert_eq!(f2(&mut h), (Some("renameLayout"), "Sheet".to_string()), "in the layout");

        h.get_by_label("Rivers").click();
        h.run_steps(2);
        assert_eq!(f2(&mut h), (Some("renameLayer"), "Rivers".to_string()), "in the layer list, with a layout in view");

        h.state_mut().run("layout.close", Value::Null).unwrap();
        h.run_steps(2);
        let view = rect(&h, Pane::View);
        press(&mut h, view.left_top() + vec2(12.0, 12.0));
        assert_eq!(pane(h.state()), Pane::View);
        assert_eq!(f2(&mut h), (Some("renameLayer"), "Rivers".to_string()), "on the map");
    }

    /// Delete with nothing to act on says why, in each pane, and not twice while the notice shows.
    #[test]
    fn delete_with_nothing_to_act_on_says_why() {
        let mut app = RustGisApp::with_sample();
        app.run("edit.enable", json!({"on": false})).unwrap();
        let mut h = window(app);
        key(&mut h, Key::Delete);
        assert!(
            says(&h, "Delete: editing is turned off (Edit › Edit)"),
            "{:?}",
            h.state().map.toasts.iter().map(|t| &t.text).collect::<Vec<_>>()
        );

        h.state_mut().run("edit.enable", json!({"on": true})).unwrap();
        key(&mut h, Key::Delete);
        key(&mut h, Key::Delete);
        assert_eq!(h.state().map.toasts.iter().filter(|t| t.text == "Delete: nothing is selected").count(), 1);

        // The layer list in use, with no layer picked in it.
        h.get_by_label("Rivers").click();
        h.run_steps(2);
        h.state_mut().session.active_layer = None;
        key(&mut h, Key::Backspace);
        assert!(says(&h, "Remove: select a layer in the Layers pane first"));

        h.state_mut().run("layout.new", json!({"name": "Sheet"})).unwrap();
        h.state_mut().layout.selected = None;
        h.run_steps(2);
        let view = rect(&h, Pane::View);
        press(&mut h, view.left_top() + vec2(12.0, 12.0));
        h.state_mut().layout.selected = None;
        key(&mut h, Key::Delete);
        assert!(says(&h, "Delete Element: no element is selected"));
    }

    /// A click that asks a question (a layout tab's close button: Delete Layout?) and Delete in
    /// the same frame: the key is behind the question, and the layout's selected element stays.
    #[test]
    fn delete_with_the_click_that_asks_a_question_is_dropped() {
        let mut app = RustGisApp::with_sample();
        app.run("layout.new", json!({"name": "Sheet"})).unwrap();
        let li = app.session.active_layout.unwrap();
        let title = app.session.project.layouts[li].elements.iter().find(|e| e.name == "Title").unwrap().id;
        app.layout.selected = Some(title);
        let mut h = window(app);
        assert_eq!(h.state().layout.selected, Some(title));
        let close = h.get_by_label("Delete Layout").rect().center();
        in_one_frame(&mut h, [Event::PointerMoved(close), button(close, true), button(close, false), delete(true), delete(false)]);
        assert!(h.state().map.deleting.is_some(), "the click asked whether to delete the layout");
        assert!(h.state().session.project.layouts[li].element(title).is_some(), "Delete didn't reach the element behind the question");
    }

    /// What ends the layer list's turn without a press in another pane, and what doesn't: its
    /// own menu keeps it; a dialog, another project and the pane closing end it.
    #[test]
    fn the_layer_list_stays_in_use_only_while_the_user_is_in_it() {
        let mut h = window(RustGisApp::with_sample());
        let list = |h: &mut Window| {
            h.get_by_label("Rivers").click();
            h.run_steps(2);
            assert_eq!(pane(h.state()), Pane::Layers);
        };

        // A row of the layer's own menu, wherever the menu reaches.
        list(&mut h);
        h.get_by_label("Rivers").click_secondary();
        h.run_steps(3);
        h.get_by_label("Zoom To Layer").click();
        h.run_steps(3);
        assert_eq!(pane(h.state()), Pane::Layers, "a menu is its pane's");

        // A dialog's button.
        h.state_mut().run("ui.dialog", json!({"name": "renameLayer"})).unwrap();
        h.run_steps(3);
        h.get_by_label("Cancel").click();
        h.run_steps(3);
        assert_eq!(pane(h.state()), Pane::Other, "a dialog is no pane");

        // A press says where it was, though the pointer is over the list by the end of the frame.
        list(&mut h);
        let (map, layers) = (rect(&h, Pane::View).center(), rect(&h, Pane::Layers).center());
        in_one_frame(&mut h, [Event::PointerMoved(map), button(map, true), button(map, false), Event::PointerMoved(layers)]);
        assert_eq!(pane(h.state()), Pane::View, "pressed on the map, then moved over the list");

        // Command Search closed by a click beside it: a dialog, though the click is over the map.
        list(&mut h);
        h.state_mut().run("ui.commandSearch", Value::Null).unwrap();
        h.run_steps(3);
        press(&mut h, map);
        h.state_mut().ui.command_search = None;
        h.run_steps(2);
        assert_eq!(pane(h.state()), Pane::Other, "a modal dialog is no pane");

        list(&mut h);
        h.state_mut().run("project.new", Value::Null).unwrap();
        h.run_steps(2);
        assert_eq!(pane(h.state()), Pane::View, "another project starts on the map");

        h.state_mut().run("project.sample", Value::Null).unwrap();
        h.run_steps(3);
        list(&mut h);
        // The File menu's backstage opened by a command, with no press: it covers the list.
        h.state_mut().run("ui.backstage", json!({"open": true})).unwrap();
        h.run_steps(2);
        assert_eq!(pane(h.state()), Pane::Other, "the layer list is behind the backstage");
        h.state_mut().run("ui.backstage", json!({"open": false})).unwrap();
        h.run_steps(2);
        // So are the tabs: F2 must not rename the map behind it.
        let tabs = rect(&h, Pane::Tabs);
        press(&mut h, tabs.left_center() + vec2(40.0, 0.0));
        assert_eq!(pane(h.state()), Pane::Tabs);
        h.state_mut().run("ui.backstage", json!({"open": true})).unwrap();
        h.run_steps(2);
        assert_eq!(pane(h.state()), Pane::Other, "the tabs are behind the backstage");
        h.state_mut().run("ui.backstage", json!({"open": false})).unwrap();
        h.run_steps(2);
        list(&mut h);
        // The wheel turned where the pane was drawn as it closed: it isn't there any more.
        h.state_mut().ui.left = None;
        one_frame(&mut h, [Event::PointerMoved(layers), wheel()]);
        assert_eq!(pane(h.state()), Pane::Other, "the Layers pane closed");
        h.run_steps(2);
        assert_eq!(pane(h.state()), Pane::Other);
        h.state_mut().ui.left = Some(LeftPane::Contents);
        h.run_steps(2);
        assert_eq!(pane(h.state()), Pane::Other, "and open again, with no press in it");
    }
}
