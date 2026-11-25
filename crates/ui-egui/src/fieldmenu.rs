//! A text field's right-click menu, as desktop systems give every text box: Undo, Cut, Copy,
//! Paste, Delete and Select All. One menu for every text field in the app: a right click (or
//! a long touch) on a field opens it, and the row picked is played to the field as its keys would
//! be, so the field cuts, copies, pastes and undoes as it does from the keyboard.

use egui::text::CCursorRange;
use egui::text_edit::TextEditState;
use egui::{Context, Event, Id, Key, Modifiers, PointerButton, Popup, SurrenderFocusOn, Ui};

use crate::keymap::Chord;
use crate::widgets::menu_row;
use crate::{RustGisApp, Slot};

/// A row of the menu.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edit {
    Undo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
}

/// A text field and its selection (or caret).
type Field = (Id, Option<CCursorRange>);

/// The field the menu is open on, with its selection then; the row picked in it (played on the
/// next frame); the selection of a field pressed with the right button (which egui replaces with
/// the caret); when a click elsewhere takes a field's focus away, while the menu keeps it; and
/// the field a paste is on its way to, with the clipboard's text when the browser has read it.
#[derive(Default)]
pub struct FieldMenu {
    field: Option<Field>,
    picked: Option<(Field, Edit)>,
    kept: Option<(Id, CCursorRange)>,
    surrender: Option<SurrenderFocusOn>,
    pasting: Option<(Field, Slot<Result<String, String>>)>,
}

/// At the start of a frame, before the shortcuts and the fields read the keys: Escape closes an
/// open menu and is the field's no more (the search box would clear on it); a field pressed with
/// the right button has its selection noted; the row picked in a field's menu is played to the
/// field.
pub fn play(app: &mut RustGisApp, ctx: &Context) {
    // The clipboard's text the browser read for Paste, pasted into its field; or why not.
    let landed =
        app.ui.field_menu.pasting.as_ref().and_then(|(field, slot)| Some((*field, slot.lock().unwrap_or_else(|e| e.into_inner()).take()?)));
    if let Some((field, read)) = landed {
        app.ui.field_menu.pasting = None;
        match read {
            // Pasted where the user still is, which may be seconds later (the browser asked
            // first): into the field as it is now, or, with nothing focused (the browser's prompt
            // took the focus, or a click on the map or an empty part of a pane did), into the
            // field Paste was picked in, as the menu left it. Moved on to another field, the text
            // isn't pasted there.
            Ok(text) => match ctx.memory(|m| m.focused()) {
                Some(id) if id == field.0 => ctx.input_mut(|i| i.events.push(Event::Paste(text))),
                None => {
                    refocus(ctx, field);
                    ctx.input_mut(|i| i.events.push(Event::Paste(text)));
                }
                Some(_) => {
                    let keys = Chord::parse("Mod+V").map(|c| c.text(ctx.os().is_mac())).unwrap_or_default();
                    app.session.notify(
                        crate::i18n::fmt(
                            "The clipboard came after you moved on, so nothing was pasted: paste with {keys}.",
                            &[("keys", &keys)],
                        ),
                        false,
                    );
                }
            },
            Err(e) => {
                let keys = Chord::parse("Mod+V").map(|c| c.text(ctx.os().is_mac())).unwrap_or_default();
                app.session.notify(
                    crate::i18n::fmt(
                        "The browser didn't give RustGIS the clipboard ({why}): paste with {keys}.",
                        &[("why", &e), ("keys", &keys)],
                    ),
                    false,
                );
            }
        }
    }
    let menu = &mut app.ui.field_menu;
    // While the menu is open, the field keeps its focus when the menu is clicked: one that saves
    // and closes when it loses the focus (a table cell, a name being edited) stays open for the
    // row picked.
    ctx.options_mut(|o| {
        let on = &mut o.input_options.surrender_focus_on;
        match (menu.field.is_some(), menu.surrender) {
            (true, None) => menu.surrender = Some(std::mem::replace(on, SurrenderFocusOn::Never)),
            (false, Some(was)) => {
                *on = was;
                menu.surrender = None;
            }
            _ => {}
        }
    });
    if let Some(field) = menu.field
        && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape))
    {
        // egui took the field's focus away on Escape before this pass; it was the menu's.
        Popup::close_id(ctx, field.0.with("field_menu"));
        refocus(ctx, field);
        menu.field = None;
    }
    if ctx.input(|i| i.pointer.button_pressed(PointerButton::Secondary)) {
        let hovered: Vec<Id> = ctx.interaction_snapshot(|s| s.hovered.iter().copied().collect());
        menu.kept = hovered.into_iter().find_map(|id| {
            let range = crate::widgets::cursor_range(ctx, id)?;
            (!range.is_empty()).then_some((id, range))
        });
    }
    let Some((field, edit)) = menu.picked.take() else { return };
    refocus(ctx, field);
    // In the browser, the page reads the clipboard itself (the browser may ask first), and the
    // text is pasted when it comes.
    if edit == Edit::Paste
        && let Some(read) = &app.services.read_clipboard
    {
        let slot = Slot::default();
        read(slot.clone());
        app.ui.field_menu.pasting = Some((field, slot));
        return;
    }
    let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
    let event = match edit {
        Edit::Undo => key(Key::Z, Modifiers::COMMAND),
        Edit::Cut => Event::Cut,
        Edit::Copy => Event::Copy,
        Edit::Delete => key(Key::Delete, Modifiers::NONE),
        Edit::SelectAll => key(Key::A, Modifiers::COMMAND),
        // The system clipboard is read by the window, which sends what it holds to the focused
        // field on the next frame, as Ctrl+V does.
        Edit::Paste => {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            // The window doesn't redraw for it by itself.
            ctx.request_repaint();
            return;
        }
    };
    ctx.input_mut(|i| i.events.push(event));
}

/// Gives `field` the focus (back, after Escape), with its selection.
fn refocus(ctx: &Context, (id, range): Field) {
    if let Some(mut state) = TextEditState::load(ctx, id) {
        state.cursor.set_char_range(range);
        state.store(ctx, id);
    }
    ctx.memory_mut(|m| m.request_focus(id));
}

/// Opens the menu on a text field right-clicked this frame and shows it while it's open: after
/// every field is drawn.
///
/// A field takes the focus when pressed with any button; a number field (`DragValue`) only when
/// it's being typed in, so it has the menu then. A field with a right-click menu of its own keeps
/// that one, and one in a popup (a ribbon button's form) has none: egui keeps one popup open, and
/// the menu would close the form.
pub fn show(app: &mut RustGisApp, ctx: &Context) {
    keep_selection(app, ctx);
    let (clicked, touched) = ctx.interaction_snapshot(|s| (s.clicked, s.long_touched));
    let right = clicked.filter(|_| ctx.input(|i| i.pointer.secondary_clicked())).or(touched);
    let ours = app.ui.field_menu.field.is_some_and(|(f, _)| Popup::is_id_open(ctx, f.with("field_menu")));
    if let Some(id) = right
        && ctx.memory(|m| m.has_focus(id))
        && let Some(state) = TextEditState::load(ctx, id)
        && ctx.read_response(id).is_some()
        && (ours || !Popup::is_any_open(ctx))
    {
        app.ui.field_menu.field = Some((id, state.cursor.char_range()));
    }
    let Some(field @ (id, range)) = app.ui.field_menu.field else { return };
    let Some(resp) = ctx.read_response(id) else {
        app.ui.field_menu.field = None;
        return;
    };
    let selected = range.is_some_and(|r| !r.is_empty());
    let mac = ctx.os().is_mac();
    // In the browser, only where the page can read the clipboard (over HTTPS).
    let paste = !cfg!(target_arch = "wasm32") || app.services.read_clipboard.is_some();
    let shown = Popup::context_menu(&resp).id(id.with("field_menu")).show(|ui| rows(ui, selected, paste, mac));
    let menu_rect = shown.as_ref().map(|s| s.response.rect);
    if let Some(edit) = shown.and_then(|s| s.inner) {
        app.ui.field_menu.picked = Some((field, edit));
        ctx.request_repaint();
    } else if !Popup::is_id_open(ctx, id.with("field_menu")) {
        // Closed by a click away from the field and the menu (one in the menu, on a greyed row
        // say, closes it too): the field gives up the focus that click would have taken (and
        // saves, if it does on losing it).
        let inside = |p| resp.rect.contains(p) || menu_rect.is_some_and(|r| r.contains(p));
        let away = ctx.input(|i| i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| !inside(p)));
        if away {
            ctx.memory_mut(|m| m.surrender_focus(id));
        }
        app.ui.field_menu.field = None;
    }
}

/// A field pressed with the right button within its selection (or at either end) keeps it, as in
/// Windows: egui put the caret where it was pressed. Pressed elsewhere, the caret stays there.
fn keep_selection(app: &mut RustGisApp, ctx: &Context) {
    let Some((id, kept)) = app.ui.field_menu.kept.take() else { return };
    let Some(mut state) = TextEditState::load(ctx, id) else { return };
    let Some(now) = state.cursor.char_range() else { return };
    let [from, to] = kept.sorted_cursors();
    if now.is_empty() && (from.index..=to.index).contains(&now.primary.index) {
        state.cursor.set_char_range(Some(kept));
        state.store(ctx, id);
    }
}

/// The menu's rows, with their keys; cutting, copying and deleting need a selection, pasting a
/// clipboard the app can read. Returns the row clicked.
fn rows(ui: &mut Ui, selected: bool, paste: bool, mac: bool) -> Option<Edit> {
    let keys = |k: &str| Chord::parse(k).map(|c| c.text(mac)).unwrap_or_default();
    let mut picked = None;
    let mut row = |ui: &mut Ui, icon: &str, label: &str, k: &str, on: bool, edit: Edit| {
        if menu_row(ui, icon, label, &keys(k), on).clicked() {
            picked = Some(edit);
        }
    };
    row(ui, "undo", tl!("Undo"), "Mod+Z", true, Edit::Undo);
    ui.separator();
    row(ui, "cut", tl!("Cut"), "Mod+X", selected, Edit::Cut);
    row(ui, "copy", tl!("Copy"), "Mod+C", selected, Edit::Copy);
    row(ui, "paste", tl!("Paste"), "Mod+V", paste, Edit::Paste);
    row(ui, "delete", tl!("Delete"), "Delete", selected, Edit::Delete);
    ui.separator();
    row(ui, "", tl!("Select All"), "Mod+A", true, Edit::SelectAll);
    picked
}

#[cfg(test)]
mod tests {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;

    use super::*;

    fn harness() -> egui_kittest::Harness<'static, RustGisApp> {
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 1000.0)).with_max_steps(64).build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
                app.ui(ui);
            },
            RustGisApp::with_sample(),
        );
        h.run_steps(4);
        h
    }

    const SEARCH: &str = "Search the map";

    fn search(h: &egui_kittest::Harness<'_, RustGisApp>) -> String {
        h.state().ui.search.query.clone()
    }

    /// Right-clicks the search box and picks `row` in its menu: the text it put on the clipboard.
    fn pick(h: &mut egui_kittest::Harness<'_, RustGisApp>, row: &str) -> Vec<String> {
        h.get_by_role_and_label(Role::TextInput, SEARCH).click_secondary();
        h.run_steps(2);
        h.get_by_label(row).click();
        let mut copied = Vec::new();
        for _ in 0..4 {
            h.step();
            for c in &h.output().platform_output.commands {
                if let egui::OutputCommand::CopyText(t) = c {
                    copied.push(t.clone());
                }
            }
        }
        copied
    }

    /// A text field's right-click menu selects, copies, cuts, deletes and undoes as its keys do;
    /// without a selection, only Undo, Paste and Select All can be picked.
    #[test]
    fn a_text_fields_menu_edits_it_as_its_keys_do() {
        let mut h = harness();
        // Not focused before: the right press focuses it.
        h.state_mut().ui.search.query = "Zurich".into();
        h.run_steps(2);
        h.get_by_role_and_label(Role::TextInput, SEARCH).click_secondary();
        h.run_steps(2);
        for row in ["Undo Ctrl+Z", "Cut Ctrl+X", "Copy Ctrl+C", "Paste Ctrl+V", "Delete Delete", "Select All Ctrl+A"] {
            assert!(h.query_by_label(row).is_some(), "{row}");
        }
        // Escape closes it; the search box, which clears on Escape, keeps its text.
        h.key_press(Key::Escape);
        h.run_steps(2);
        assert!(h.query_by_label("Select All Ctrl+A").is_none());
        assert_eq!(search(&h), "Zurich");

        pick(&mut h, "Select All Ctrl+A");
        assert!(h.get_by_role_and_label(Role::TextInput, SEARCH).is_focused());
        // Right-clicked inside the selection, it keeps it, Escape too.
        h.get_by_role_and_label(Role::TextInput, SEARCH).click_secondary();
        h.run_steps(2);
        h.key_press(Key::Escape);
        h.step();
        h.run_steps(2);
        assert_eq!(search(&h), "Zurich");
        assert_eq!(pick(&mut h, "Copy Ctrl+C"), ["Zurich"]);
        assert_eq!(search(&h), "Zurich");
        assert_eq!(pick(&mut h, "Cut Ctrl+X"), ["Zurich"]);
        assert_eq!(search(&h), "");
        pick(&mut h, "Undo Ctrl+Z");
        assert_eq!(search(&h), "Zurich");
        pick(&mut h, "Select All Ctrl+A");
        pick(&mut h, "Delete Delete");
        assert_eq!(search(&h), "");
    }

    /// Paste asks the window for the clipboard and keeps the field focused for what it sends.
    #[test]
    fn paste_puts_the_clipboard_into_the_field() {
        let mut h = harness();
        h.get_by_role_and_label(Role::TextInput, SEARCH).click_secondary();
        h.run_steps(2);
        h.get_by_label("Paste Ctrl+V").click();
        let mut asked = false;
        for _ in 0..3 {
            h.step();
            asked |=
                h.output().viewport_output.values().any(|v| v.commands.iter().any(|c| matches!(c, egui::ViewportCommand::RequestPaste)));
        }
        assert!(asked, "the window is asked for the clipboard");
        assert!(h.get_by_role_and_label(Role::TextInput, SEARCH).is_focused());
        h.event(Event::Paste("Bern".into()));
        h.run_steps(2);
        assert_eq!(search(&h), "Bern");
    }

    /// Where the page reads the clipboard itself (the browser), Paste asks it and pastes what it
    /// reads into the field; what it can't read is said, the field left as it was.
    #[test]
    fn paste_reads_the_clipboard_where_the_page_reads_it() {
        let mut h = harness();
        let asked: Slot<Slot<Result<String, String>>> = Slot::default();
        let ask = asked.clone();
        h.state_mut().services.read_clipboard = Some(Box::new(move |slot| *ask.lock().unwrap() = Some(slot)));
        let paste = |h: &mut egui_kittest::Harness<'_, RustGisApp>| {
            h.get_by_role_and_label(Role::TextInput, SEARCH).click_secondary();
            h.run_steps(2);
            h.get_by_label("Paste Ctrl+V").click();
            h.run_steps(2);
            asked.lock().unwrap().take().expect("the page is asked for the clipboard")
        };
        // It comes later (the browser may ask the user first).
        let slot = paste(&mut h);
        assert_eq!(search(&h), "");
        *slot.lock().unwrap() = Some(Ok("Winterthur".into()));
        h.run_steps(3);
        assert_eq!(search(&h), "Winterthur");
        assert!(h.get_by_role_and_label(Role::TextInput, SEARCH).is_focused());
        // With nothing focused when it comes (the browser's prompt took the focus): into the
        // field, over the selection the menu was opened on.
        pick(&mut h, "Select All Ctrl+A");
        let slot = paste(&mut h);
        h.ctx.memory_mut(|m| m.focused().map(|id| m.surrender_focus(id)));
        h.run_steps(2);
        *slot.lock().unwrap() = Some(Ok("Bern".into()));
        h.run_steps(3);
        assert_eq!(search(&h), "Bern");
        // Refused: said, and nothing pasted.
        let slot = paste(&mut h);
        *slot.lock().unwrap() = Some(Err("NotAllowedError".into()));
        h.run_steps(3);
        assert_eq!(search(&h), "Bern");
        let said = h.state().session.notices.iter().any(|n| n.text.contains("NotAllowedError") && n.text.contains("Ctrl+V"));
        assert!(said, "{:?}", h.state().session.notices.iter().map(|n| &n.text).collect::<Vec<_>>());
    }

    /// A page with the fields the app's menu has to live with: one that saves when it loses the
    /// focus (as a table cell does), a number field, and one in a button's popup (as a ribbon
    /// button's form).
    struct Page {
        app: RustGisApp,
        name: String,
        saved: Vec<String>,
        number: f64,
        code: String,
    }

    fn page() -> egui_kittest::Harness<'static, Page> {
        let page = Page { app: RustGisApp::with_sample(), name: "Bern".into(), saved: Vec::new(), number: 5.0, code: "2056".into() };
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(600.0, 400.0)).build_ui_state(
            |ui, p: &mut Page| {
                let ctx = ui.ctx().clone();
                play(&mut p.app, &ctx);
                let label = ui.label("Name");
                if ui.add(egui::TextEdit::singleline(&mut p.name)).labelled_by(label.id).lost_focus() {
                    p.saved.push(p.name.clone());
                }
                let label = ui.label("Number");
                ui.add(egui::DragValue::new(&mut p.number)).labelled_by(label.id);
                let _ = ui.button("Elsewhere");
                let form = ui.button("Form");
                // As a ribbon button's form: it stays open for clicks in it.
                Popup::menu(&form).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                    let label = ui.label("Code");
                    ui.add(egui::TextEdit::singleline(&mut p.code)).labelled_by(label.id);
                });
                show(&mut p.app, &ctx);
            },
            page,
        );
        h.run_steps(2);
        h
    }

    fn right_click(h: &mut egui_kittest::Harness<'_, Page>, role: Role, label: &str) {
        h.get_by_role_and_label(role, label).click_secondary();
        h.run_steps(2);
    }

    /// A field that saves when it loses the focus keeps it while its menu is used, so the row
    /// picked edits it; a click away from the menu then takes the focus as a click would.
    #[test]
    fn a_field_that_saves_on_losing_focus_keeps_it_for_the_menu() {
        let mut h = page();
        right_click(&mut h, Role::TextInput, "Name");
        h.get_by_label("Select All Ctrl+A").click();
        h.run_steps(3);
        right_click(&mut h, Role::TextInput, "Name");
        h.get_by_label("Cut Ctrl+X").click();
        h.run_steps(3);
        assert_eq!(h.state().name, "");
        assert!(h.state().saved.is_empty(), "{:?}", h.state().saved);
        assert!(h.get_by_role_and_label(Role::TextInput, "Name").is_focused());
        // A greyed row clicked (nothing is selected) closes the menu; the field stays open.
        right_click(&mut h, Role::TextInput, "Name");
        h.get_by_label("Copy Ctrl+C").click();
        h.run_steps(3);
        assert!(h.query_by_label("Undo Ctrl+Z").is_none());
        assert!(h.state().saved.is_empty(), "{:?}", h.state().saved);
        assert!(h.get_by_role_and_label(Role::TextInput, "Name").is_focused());
        // Clicked away while the menu is open: the menu closes and the field saves.
        right_click(&mut h, Role::TextInput, "Name");
        assert!(h.query_by_label("Undo Ctrl+Z").is_some());
        h.get_by_label("Elsewhere").click();
        h.run_steps(3);
        assert!(h.query_by_label("Undo Ctrl+Z").is_none());
        assert_eq!(h.state().saved, [""]);
        assert!(!h.get_by_role_and_label(Role::TextInput, "Name").is_focused());
    }

    /// A number field has the menu only while it's typed in.
    #[test]
    fn a_number_field_has_the_menu_while_typed_in() {
        let mut h = page();
        let field = h.get_by_role_and_label(Role::SpinButton, "Number").rect();
        right_click(&mut h, Role::SpinButton, "Number");
        assert!(h.query_by_label("Undo Ctrl+Z").is_none());
        h.get_by_role_and_label(Role::SpinButton, "Number").click();
        h.run_steps(2);
        h.event(Event::PointerMoved(field.center()));
        h.event(Event::PointerButton { pos: field.center(), button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE });
        h.event(Event::PointerButton { pos: field.center(), button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE });
        h.run_steps(2);
        assert!(h.query_by_label("Undo Ctrl+Z").is_some());
    }

    /// A field in a popup has no menu: the popup would close for it.
    #[test]
    fn a_field_in_a_popup_keeps_the_popup() {
        let mut h = page();
        h.get_by_label("Form").click();
        h.run_steps(2);
        right_click(&mut h, Role::TextInput, "Code");
        assert!(h.query_by_label("Undo Ctrl+Z").is_none());
        assert!(h.query_by_role_and_label(Role::TextInput, "Code").is_some(), "the popup stays open");
    }
}
