//! Ribbon widgets: large and small buttons, menu buttons, groups and columns. Each control runs a
//! command by id and greys out when the command is disabled.

use egui::{Align2, CornerRadius, Rect, Response, Sense, Stroke, Ui, pos2, vec2};
use serde_json::Value;

use crate::theme::{Tokens, regular, semibold};
use crate::{RustGisApp, icons};

/// Height of the ribbon's control area (without group labels).
pub const CONTENT_H: f32 = 72.0;
pub const LABEL_H: f32 = 18.0;

/// A number its caller lends saved each frame and saves only once it is settled, kept across a
/// drag (#597, #607). egui's `DragValue` keeps the value dragged so far in its own memory and
/// forgets it when the pointer is released, a frame on which `dragged()` is false: lent the saved
/// value again, the drag would end back where it started, nothing saved. `lend` before the
/// `DragValue` and `settled` after it. The value dragged so far is kept with the saved one it was
/// dragged from, so it goes stale when that changes (another element shown).
pub(crate) struct Dragged<T> {
    key: egui::Id,
    saved: T,
}

impl<T: Copy + Default + PartialEq + Send + Sync + 'static> Dragged<T> {
    /// Lends the value dragged so far in place of the saved one, mid-drag (so it doesn't flicker
    /// back to the saved one when the pointer pauses). `key` names this number, steadily.
    pub(crate) fn lend(ui: &Ui, key: egui::Id, value: &mut T) -> Self {
        let saved = *value;
        if let Some((_, dragged)) = ui.data(|d| d.get_temp::<(T, T)>(key)).filter(|(from, _)| *from == saved) {
            *value = dragged;
        }
        Dragged { key, saved }
    }

    /// Whether the number is settled: let go after a drag (`value` is then the dragged one), typed
    /// in, or set by assistive technology. The caller still compares it with the saved one.
    pub(crate) fn settled(self, ui: &Ui, r: &Response, value: &mut T) -> bool {
        if escaped(r) {
            *value = self.saved;
            return false;
        }
        if r.dragged() {
            ui.data_mut(|d| d.insert_temp(self.key, (self.saved, *value)));
            return false;
        }
        if let Some((from, dragged)) = ui.data_mut(|d| d.remove_temp::<(T, T)>(self.key))
            && from == self.saved
        {
            *value = dragged;
        }
        r.drag_stopped() || r.lost_focus() || r.changed()
    }
}

/// Makes the command the caller runs next, for a change of `r`, part of the undo step of the
/// pointer press making it: a number dragged, or a colour picked in its picker, is one step to
/// undo however many changes it makes (#1194). A number typed into `r` is a step of its own, even
/// while a button is held elsewhere; a key pressed during a drag elsewhere doesn't split the drag.
/// Called just before the command runs, so the merge can't reach another command.
pub(crate) fn one_step_per_press(app: &mut RustGisApp, r: &Response) {
    let typed = r.has_focus() || r.lost_focus();
    let press = r.ctx.input(|i| i.pointer.press_start_time()).filter(|_| !typed);
    if let Some(t) = press {
        app.session.merge_undo(format!("press {:?} {t}", r.id));
    }
}

/// Makes a field being typed into let go of its focus, and so set what was typed, when the pointer
/// is pressed on a panel outside it, not when it is let go: the Layers pane is drawn before the
/// panes, so a click there would pick another layer before the field set a number typed for the
/// one it was typed for (#1194). A press on a popup, a menu or a list over the map (the search
/// box's) is left to egui, which lets go on the release, after the list took the click. Called
/// each frame before the panes are drawn.
pub(crate) fn let_go_on_panel_press(ctx: &egui::Context) {
    let Some(at) = ctx.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()).flatten()) else { return };
    let Some(id) = ctx.memory(|m| m.focused()) else { return };
    // A field that keeps its focus on a press elsewhere (`fieldmenu`) keeps it.
    let kept = ctx.options(|o| o.input_options.surrender_focus_on == egui::SurrenderFocusOn::Never);
    let on_field = ctx.read_response(id).is_some_and(|r| r.rect.contains(at));
    let on_panel = ctx.layer_id_at(at).is_some_and(|l| l.order == egui::Order::Background);
    if ctx.text_edit_focused() && !kept && !on_field && on_panel {
        ctx.memory_mut(|m| m.surrender_focus(id));
    }
}

/// What [`note_escape`] keeps: the widget that had the focus, and the field Escape took it from
/// with the pass it did.
type EscapeNote = (Option<egui::Id>, Option<(egui::Id, u64)>);

/// Where [`note_escape`] keeps its [`EscapeNote`].
fn escape_key() -> egui::Id {
    egui::Id::unique("escaped_field")
}

/// Notes the field Escape takes the focus from, for [`escaped`]. Called each frame before the
/// panes are drawn.
pub(crate) fn note_escape(ctx: &egui::Context) {
    let now = ctx.memory(|m| m.focused());
    let pass = ctx.cumulative_pass_nr();
    let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    ctx.data_mut(|d| {
        let (before, escaped) = d.get_temp::<EscapeNote>(escape_key()).unwrap_or_default();
        let escaped = match before {
            Some(id) if escape && now != Some(id) => Some((id, pass)),
            // The field taken back: what it sets from here on is not what Escape dropped.
            _ => escaped.filter(|(id, _)| now != Some(*id)),
        };
        d.insert_temp(escape_key(), (now, escaped));
    });
}

/// The widget that had the focus when the pass began, as [`note_escape`] noted it: egui may have
/// taken the focus from a field drawn earlier in the pass, which sets its number only in the next.
pub(crate) fn focused_at_pass_start(ctx: &egui::Context) -> Option<egui::Id> {
    ctx.data(|d| d.get_temp::<EscapeNote>(escape_key())).and_then(|(now, _)| now)
}

/// Whether a change of `r` is egui setting the text typed into it before Escape. egui sets it on a
/// lost focus unless it sees the Escape: a shortcut drawn before the field can take the key on the
/// Escape's own frame, and egui 0.37 tells a field it lost the focus for two frames, the second
/// without the Escape. The caller drops the change, so Escape brings back the value from before
/// (#1194).
pub(crate) fn escaped(r: &Response) -> bool {
    let escaped = r.ctx.data(|d| d.get_temp::<EscapeNote>(escape_key())).and_then(|(_, e)| e);
    // The Escape's pass or one of the two after it.
    escaped.is_some_and(|(id, pass)| id == r.id && (pass..=pass + 2).contains(&r.ctx.cumulative_pass_nr()))
}

/// Whether `r` changed to a value to keep: not one Escape dropped ([`escaped`]).
pub(crate) fn changed(r: &Response) -> bool {
    r.changed() && !escaped(r)
}

pub fn text_w(ui: &Ui, s: &str, font: egui::FontId) -> f32 {
    ui.ctx().fonts_mut(|f| f.layout_no_wrap(s.to_string(), font, egui::Color32::WHITE).size().x)
}

/// The shortcut of a command run without parameters (`⌘S` on a Mac, as egui reads the keys,
/// `Ctrl+S` elsewhere), or empty for none.
pub fn shortcut_text(ctx: &egui::Context, app: &RustGisApp, id: &str) -> String {
    crate::keymap::shortcut_text(ctx, app, id, &Value::Null)
}

/// Whether a command exists and is enabled now.
pub fn enabled(app: &RustGisApp, id: &str) -> bool {
    match app.session.registry.get(id) {
        Some(s) => (s.enabled)(&app.session).is_none(),
        None => id.starts_with("ui.") || rustgis_engine::plugins::command(id).is_some(),
    }
}

pub fn why_disabled(app: &RustGisApp, id: &str) -> Option<&'static str> {
    app.session.registry.get(id).and_then(|s| (s.enabled)(&app.session))
}

/// A rich tooltip: title (with the shortcut of `id` with `params`), description and why it's
/// disabled.
pub fn tooltip(app: &RustGisApp, resp: Response, title: &str, desc: &str, id: &str, params: &Value) -> Response {
    let sc = crate::keymap::shortcut_text(&resp.ctx, app, id, params);
    let why = why_disabled(app, id);
    let contents = |ui: &mut Ui| {
        ui.set_max_width(280.0);
        // Callers pass text that is already translated.
        let head = if sc.is_empty() { title.to_string() } else { format!("{title}  ({sc})") };
        ui.label(egui::RichText::new(head).font(semibold(12.5)));
        if !desc.is_empty() {
            ui.label(egui::RichText::new(desc).size(11.5).weak());
        }
        if let Some(w) = why {
            ui.label(egui::RichText::new(crate::i18n::fmt("Unavailable: {reason}", &[("reason", tl_raw!(w))])).size(11.0).italics().weak());
        }
    };
    if resp.enabled() { resp.on_hover_ui(contents) } else { resp.on_disabled_hover_ui(contents) }
}

fn bg(ui: &Ui, r: Rect, resp: &Response, checked: bool, t: &Tokens) {
    let fill = if resp.is_pointer_button_down_on() {
        t.pressed
    } else if checked {
        t.checked
    } else if resp.hovered() {
        t.hover
    } else {
        return;
    };
    ui.painter().rect_filled(r, CornerRadius::same(6), fill);
    if checked {
        ui.painter().rect_stroke(r, CornerRadius::same(6), Stroke::new(1.0, t.accent.gamma_multiply(0.55)), egui::StrokeKind::Inside);
    }
}

/// A small downward caret marking a menu.
pub fn caret(p: &egui::Painter, c: egui::Pos2, col: egui::Color32) {
    p.add(egui::Shape::convex_polygon(vec![c + vec2(-3.5, -1.8), c + vec2(3.5, -1.8), c + vec2(0.0, 2.2)], col, Stroke::NONE));
}

/// A large button: 32 px icon over a one- or two-line label. Returns the response.
pub fn big(
    ui: &mut Ui,
    app: &mut RustGisApp,
    icon: &str,
    label: &str,
    desc: &str,
    id: &str,
    params: Value,
    checked: bool,
    menu: bool,
) -> Response {
    let (label, desc) = (tl!(label), tl!(desc));
    let t = Tokens::get(ui.ctx());
    let lw = label.split('\n').map(|l| text_w(ui, l, regular(11.5)) + if menu { 9.0 } else { 0.0 }).fold(0.0, f32::max);
    let w = (lw + 14.0).max(50.0);
    let (r, resp) = ui.allocate_exact_size(vec2(w, CONTENT_H - 2.0), Sense::click());
    let on = enabled(app, id) || menu;
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, on, label.replace('\n', " ")));
    bg(ui, r, &resp, checked, &t);
    paint_big(ui, r, icon, label, on, menu, &t);
    let resp = tooltip(app, resp, &label.replace('\n', " "), desc, id, &params);
    crate::showme::ring(ui, app, r, id, &params);
    if resp.clicked() && on && !menu {
        let _ = app.run_by_user(id, params);
    }
    resp
}

/// The shared icon and label layout of large ribbon controls. A split button gives this only
/// its primary region, keeping even a long translated label clear of the adjacent arrow.
fn paint_big(ui: &Ui, r: Rect, icon: &str, label: &str, on: bool, menu: bool, t: &Tokens) {
    let ic = Rect::from_center_size(pos2(r.center().x, r.min.y + 21.0), vec2(32.0, 32.0));
    let (c, a) = if on { (t.icon, t.accent) } else { (t.text_disabled, t.text_disabled) };
    icons::paint(ui.painter(), ic, icon, c, a);
    let mut lines = label.split('\n').peekable();
    let mut y = r.min.y + 42.0;
    while let Some(l) = lines.next() {
        let col = if on { t.text } else { t.text_disabled };
        if menu && lines.peek().is_none() {
            let tw = text_w(ui, l, regular(11.5));
            let x0 = r.center().x - (tw + 9.0) / 2.0;
            ui.painter().text(pos2(x0, y), Align2::LEFT_TOP, l, regular(11.5), col);
            caret(ui.painter(), pos2(x0 + tw + 5.0, y + 7.5), col);
        } else {
            ui.painter().text(pos2(r.center().x, y), Align2::CENTER_TOP, l, regular(11.5), col);
        }
        y += 13.5;
    }
}

/// A large ribbon button with a fixed primary command and an independent alternatives menu.
///
/// Labels and descriptions are translated here, as for [`big`]. `menu_label` names the arrow
/// for assistive technology; give it a distinct name such as "Select options". `menu_enabled`
/// is independent of the primary command's availability. Both regions respect a disabled UI.
/// Menu rows should use [`menu_item`] internally, or run their action and call `ui.close()`.
/// Alternatives never change `id` or `params`. Escape/outside clicks use egui's menu dismissal.
///
/// Returns the primary and arrow responses, in that order. Both are keyboard focusable when
/// enabled and activate with Enter/Space. Enclose repeated controls in `ui.push_id` when their
/// positions in a dynamic list can change.
pub fn big_split_menu(
    ui: &mut Ui,
    app: &mut RustGisApp,
    icon: &str,
    label: &str,
    desc: &str,
    id: &str,
    params: Value,
    checked: bool,
    menu_label: &str,
    menu_desc: &str,
    menu_enabled: bool,
    menu: impl FnOnce(&mut Ui, &mut RustGisApp),
) -> (Response, Response) {
    let (label, desc, menu_label, menu_desc) = (tl!(label), tl!(desc), tl!(menu_label), tl!(menu_desc));
    let t = Tokens::get(ui.ctx());
    let w = (label.split('\n').map(|l| text_w(ui, l, regular(11.5))).fold(0.0, f32::max) + 14.0).max(50.0);
    let on = enabled(app, id);
    let (primary, arrow) = ui
        .allocate_ui_with_layout(vec2(w + 20.0, CONTENT_H - 2.0), egui::Layout::left_to_right(egui::Align::Min), |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let primary = ui.add_enabled_ui(on, |ui| ui.allocate_exact_size(vec2(w, CONTENT_H - 2.0), Sense::click()).1).inner;
            let arrow = ui.add_enabled_ui(menu_enabled, |ui| ui.allocate_exact_size(vec2(20.0, CONTENT_H - 2.0), Sense::click()).1).inner;
            (primary, arrow)
        })
        .inner;
    primary.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, primary.enabled(), label.replace('\n', " ")));
    arrow.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, arrow.enabled(), menu_label));
    let popup_id = egui::Popup::default_response_id(&arrow);
    if !arrow.enabled() {
        egui::Popup::close_id(ui.ctx(), popup_id);
    }
    if primary.enabled() {
        bg(ui, primary.rect, &primary, checked, &t);
    }
    if arrow.enabled() {
        bg(ui, arrow.rect, &arrow, egui::Popup::is_id_open(ui.ctx(), popup_id), &t);
    }
    paint_big(ui, primary.rect, icon, label, primary.enabled(), false, &t);
    let x = arrow.rect.min.x;
    ui.painter().line_segment([pos2(x, arrow.rect.min.y + 6.0), pos2(x, arrow.rect.max.y - 6.0)], Stroke::new(1.0, t.border));
    caret(ui.painter(), arrow.rect.center(), if arrow.enabled() { t.text } else { t.text_disabled });
    for resp in [&primary, &arrow] {
        if resp.has_focus() {
            ui.painter().rect_stroke(resp.rect, CornerRadius::same(6), Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
        }
    }
    let primary = tooltip(app, primary, &label.replace('\n', " "), desc, id, &params);
    let arrow = tooltip(app, arrow, menu_label, menu_desc, "ui.menu", &Value::Null);
    crate::showme::ring(ui, app, primary.rect, id, &params);
    if primary.clicked() {
        let _ = app.run_by_user(id, params);
    }
    egui::Popup::menu(&arrow).show(|ui| {
        ui.set_min_width(200.0);
        menu(ui, app);
    });
    (primary, arrow)
}

/// A small button: 18 px icon with an optional label.
pub fn small(ui: &mut Ui, app: &mut RustGisApp, icon: &str, label: &str, desc: &str, id: &str, params: Value, checked: bool) -> Response {
    let (label, desc) = (tl!(label), tl!(desc));
    let t = Tokens::get(ui.ctx());
    let tw = if label.is_empty() { 0.0 } else { text_w(ui, label, regular(11.5)) + 8.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(26.0 + tw, 22.0), Sense::click());
    let on = enabled(app, id);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, on, if label.is_empty() { desc } else { label }));
    bg(ui, r, &resp, checked, &t);
    let ic = Rect::from_center_size(pos2(r.min.x + 13.0, r.center().y), vec2(18.0, 18.0));
    let (c, a) = if on { (t.icon, t.accent) } else { (t.text_disabled, t.text_disabled) };
    icons::paint(ui.painter(), ic, icon, c, a);
    if !label.is_empty() {
        ui.painter().text(
            pos2(r.min.x + 26.0, r.center().y),
            Align2::LEFT_CENTER,
            label,
            regular(11.5),
            if on { t.text } else { t.text_disabled },
        );
    }
    let title = if label.is_empty() { desc } else { label };
    let resp = tooltip(app, resp, title, if label.is_empty() { "" } else { desc }, id, &params);
    crate::showme::ring(ui, app, r, id, &params);
    if resp.clicked() && on {
        let _ = app.run_by_user(id, params);
    }
    resp
}

/// A large button that opens a menu.
pub fn big_menu(ui: &mut Ui, app: &mut RustGisApp, icon: &str, label: &str, desc: &str, menu: impl FnOnce(&mut Ui, &mut RustGisApp)) {
    let resp = big(ui, app, icon, label, desc, "ui.menu", Value::Null, false, true);
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(200.0);
        menu(ui, app);
    });
}

/// A large button that opens a menu with editable controls (text boxes, checkboxes, sliders).
/// Clicks inside keep it open; action buttons close it themselves (`ui.close()`).
pub fn big_form_menu(ui: &mut Ui, app: &mut RustGisApp, icon: &str, label: &str, desc: &str, menu: impl FnOnce(&mut Ui, &mut RustGisApp)) {
    let resp = big(ui, app, icon, label, desc, "ui.menu", Value::Null, false, true);
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(200.0);
        menu(ui, app);
    });
}

/// A small button that opens a menu.
pub fn small_menu(ui: &mut Ui, app: &mut RustGisApp, icon: &str, label: &str, desc: &str, menu: impl FnOnce(&mut Ui, &mut RustGisApp)) {
    small_menu_with(ui, app, icon, label, desc, egui::PopupCloseBehavior::CloseOnClick, menu);
}

/// A small button that opens a menu with editable controls, kept open by clicks inside it (see
/// [`big_form_menu`]).
pub fn small_form_menu(
    ui: &mut Ui,
    app: &mut RustGisApp,
    icon: &str,
    label: &str,
    desc: &str,
    menu: impl FnOnce(&mut Ui, &mut RustGisApp),
) {
    small_menu_with(ui, app, icon, label, desc, egui::PopupCloseBehavior::CloseOnClickOutside, menu);
}

fn small_menu_with(
    ui: &mut Ui,
    app: &mut RustGisApp,
    icon: &str,
    label: &str,
    desc: &str,
    close: egui::PopupCloseBehavior,
    menu: impl FnOnce(&mut Ui, &mut RustGisApp),
) {
    let (label, desc) = (tl!(label), tl!(desc));
    let t = Tokens::get(ui.ctx());
    let tw = text_w(ui, label, regular(11.5)) + 20.0;
    let (r, resp) = ui.allocate_exact_size(vec2(26.0 + tw, 22.0), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), if label.is_empty() { desc } else { label }));
    bg(ui, r, &resp, false, &t);
    icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 13.0, r.center().y), vec2(18.0, 18.0)), icon, t.icon, t.accent);
    ui.painter().text(pos2(r.min.x + 26.0, r.center().y), Align2::LEFT_CENTER, label, regular(11.5), t.text);
    caret(ui.painter(), pos2(r.max.x - 9.0, r.center().y), t.text);
    let resp = tooltip(app, resp, label, desc, "ui.menu", &Value::Null);
    egui::Popup::menu(&resp).close_behavior(close).show(|ui| {
        ui.set_min_width(200.0);
        menu(ui, app);
    });
}

/// A frameless button as a row of a menu or list. Such a button paints nothing behind its text,
/// so the row's hover (or pressed) fill goes under it, `expand` wider than the row on each side.
pub fn row_button(ui: &mut Ui, on: bool, button: egui::Button<'_>, expand: f32) -> Response {
    let under = ui.painter().add(egui::Shape::Noop);
    let resp = ui.add_enabled(on, button);
    if on && (resp.hovered() || resp.is_pointer_button_down_on()) {
        let t = Tokens::get(ui.ctx());
        let fill = if resp.is_pointer_button_down_on() { t.pressed } else { t.hover };
        ui.painter().set(under, egui::Shape::rect_filled(resp.rect.expand2(vec2(expand, 0.0)), CornerRadius::same(5), fill));
    }
    resp
}

/// Room for a menu row's icon before its label.
const MENU_ICON_W: f32 = 22.0;

/// A menu row that runs a command (disabled when the command is).
pub fn menu_item(ui: &mut Ui, app: &mut RustGisApp, icon: &str, label: &str, id: &str, params: Value) {
    let on = enabled(app, id);
    let shortcut = crate::keymap::shortcut_text(ui.ctx(), app, id, &params);
    if menu_row(ui, icon, tl!(label), &shortcut, on).clicked() {
        let _ = app.run_by_user(id, params);
        ui.close();
    }
}

/// A menu row with an icon (empty for none) before an already translated label, and a shortcut
/// (empty for none) after it. The caller acts on a click and closes the menu.
pub fn menu_row(ui: &mut Ui, icon: &str, label: &str, shortcut: &str, on: bool) -> Response {
    let mut button = egui::Button::new(menu_text(ui, icon, label)).frame(false).min_size(vec2(0.0, 24.0));
    // Only with a shortcut: an empty right text would end the button's accessible label in a space.
    if !shortcut.is_empty() {
        button = button.right_text(shortcut);
    }
    let resp = row_button(ui, on, button, 4.0);
    menu_icon(ui, &resp, icon);
    resp
}

/// A menu row with an icon that opens a submenu of `add`'s rows.
pub fn submenu_row(ui: &mut Ui, icon: &str, label: &str, add: impl FnOnce(&mut Ui)) {
    let button = egui::Button::new(menu_text(ui, icon, label))
        .min_size(vec2(0.0, 24.0))
        .right_text(egui::containers::menu::SubMenuButton::arrow_atom(None));
    let (resp, _) = egui::containers::menu::SubMenuButton::from_button(button).ui(ui, add);
    menu_icon(ui, &resp, icon);
}

/// A menu row's label: with an icon, it starts after it, so the icon is drawn inside the row (and the menu).
fn menu_text(ui: &Ui, icon: &str, label: &str) -> egui::WidgetText {
    if icon.is_empty() {
        return label.into();
    }
    let mut job = egui::text::LayoutJob::default();
    let font = egui::TextStyle::Button.resolve(ui.style());
    job.append(label, MENU_ICON_W, egui::TextFormat::simple(font, egui::Color32::PLACEHOLDER));
    job.into()
}

/// Draws a menu row's icon in the room [`menu_text`] left for it.
fn menu_icon(ui: &Ui, resp: &Response, icon: &str) {
    if icon.is_empty() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    icons::paint(
        ui.painter(),
        Rect::from_center_size(
            resp.rect.left_center() + vec2(ui.spacing().button_padding.x + MENU_ICON_W / 2.0 - 2.0, 0.0),
            vec2(16.0, 16.0),
        ),
        icon,
        t.icon,
        t.accent,
    );
}

/// A ribbon group: controls laid out left to right over a centred label, with a separator.
pub fn group(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    let title = tl!(title);
    let t = Tokens::get(ui.ctx());
    let start = ui.cursor().min;
    let inner = ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 1.0);
        let r = ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            ui.set_min_height(CONTENT_H);
            ui.spacing_mut().item_spacing = vec2(2.0, 1.0);
            add(ui);
        });
        let lw = text_w(ui, title, regular(11.0));
        let w = r.response.rect.width().max(lw + 12.0);
        ui.allocate_exact_size(vec2(w, LABEL_H), Sense::hover());
        w
    });
    let w = inner.inner;
    ui.painter().text(
        pos2(start.x + w / 2.0, start.y + CONTENT_H + LABEL_H / 2.0 - 1.0),
        Align2::CENTER_CENTER,
        title,
        regular(11.0),
        t.group_label,
    );
    ui.add_space(6.0);
    let x = ui.cursor().min.x;
    ui.painter().line_segment([pos2(x, start.y + 6.0), pos2(x, start.y + CONTENT_H + LABEL_H - 4.0)], Stroke::new(1.0, t.border));
    ui.add_space(7.0);
}

/// A column of up to three small controls.
pub fn col(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(1.0, 1.0);
        ui.add_space(1.0);
        add(ui);
    });
}

/// A compact labelled combo for ribbons. Returns the chosen value when changed.
pub fn ribbon_combo(ui: &mut Ui, id: &str, label: &str, current: &str, options: &[String], width: f32) -> Option<String> {
    let mut out = None;
    ui.horizontal(|ui| {
        if !label.is_empty() {
            ui.label(egui::RichText::new(tl!(label)).size(11.5));
        }
        egui::ComboBox::from_id_salt(id)
            .selected_text(current)
            .width(width)
            .show_ui(ui, |ui| {
                for o in options {
                    if ui.selectable_label(o == current, o).clicked() {
                        out = Some(o.clone());
                    }
                }
            })
            .response
            .on_hover_text(tl!(label));
    });
    out
}

/// A filled action button (Run, Install…). Painted here so the label sits in the middle of the
/// button: egui places button text by the font's line metrics, which leaves some fonts low.
pub fn primary_button(ui: &mut Ui, label: &str, enabled: bool, min_size: egui::Vec2) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = semibold(13.0);
    let size = vec2(min_size.x.max(text_w(ui, label, font.clone()) + 28.0), min_size.y.max(28.0));
    let (r, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    // Screen readers (and UI tests) find it by its label.
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, label));
    let fill = if !enabled {
        t.green.gamma_multiply(0.45)
    } else if resp.is_pointer_button_down_on() {
        t.green.gamma_multiply(0.8)
    } else if resp.hovered() {
        t.green.gamma_multiply(0.9)
    } else {
        t.green
    };
    ui.painter().rect_filled(r, CornerRadius::same(6), fill);
    centred_label(ui, r, label, font, egui::Color32::WHITE);
    if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp }
}

/// A dialog button (OK, Cancel…): `fill` paints it as the default action, otherwise it takes the
/// theme's button look. Painted like [`primary_button`] so the label sits in the middle.
pub fn dialog_button(ui: &mut Ui, label: &str, fill: Option<egui::Color32>, min_size: egui::Vec2) -> Response {
    let font = if fill.is_some() { semibold(12.5) } else { regular(12.5) };
    let size = vec2(min_size.x.max(text_w(ui, label, font.clone()) + 24.0), min_size.y);
    let (r, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, label));
    let v = *ui.style().interact(&resp);
    let (bg, text) = match fill {
        Some(c) if resp.is_pointer_button_down_on() => (c.gamma_multiply(0.8), egui::Color32::WHITE),
        Some(c) if resp.hovered() => (c.gamma_multiply(0.9), egui::Color32::WHITE),
        Some(c) => (c, egui::Color32::WHITE),
        None => (v.weak_bg_fill, v.text_color()),
    };
    ui.painter().rect(r, v.corner_radius, bg, if fill.is_some() { Stroke::NONE } else { v.bg_stroke }, egui::StrokeKind::Inside);
    centred_label(ui, r, label, font, text);
    resp
}

/// Paints `label` centred in `r` on its capital letters (baseline to cap height) rather than on
/// the font's line box, whose ascent and descent are uneven in some faces.
fn centred_label(ui: &Ui, r: Rect, label: &str, font: egui::FontId, color: egui::Color32) {
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(label.to_string(), font.clone(), color);
    let cap = painter.layout_no_wrap("H".to_string(), font, color);
    let mid = cap
        .rows
        .first()
        .and_then(|row| Some((row.pos.y + row.glyphs.first()?.pos.y, row.pos.y + row.visuals.mesh_bounds.min.y)))
        .filter(|(base, top)| base > top)
        .map(|(base, top)| (base + top) / 2.0)
        .unwrap_or(galley.size().y / 2.0);
    painter.galley(pos2(r.center().x - galley.size().x / 2.0, r.center().y - mid).round(), galley, color);
}

/// A small icon button (back, close…), drawn with the app's own icons rather than a glyph the
/// interface font may lack.
pub fn icon_button(ui: &mut Ui, icon: &str, hover: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(5), t.hover);
    }
    ui.painter().rect_stroke(r, CornerRadius::same(5), Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
    icons::paint(ui.painter(), r.shrink(5.0), icon, t.text, t.accent);
    resp.on_hover_text(hover)
}

/// A search field, laid out like Command Search's (#337): the search icon, a frameless text box
/// with `hint` and, once it holds text, a ✕ that clears it, across the width it is given (the
/// caller adds margins and any separator). Escape clears the text, and on an empty box lets go
/// of the focus. Returns the text box's response, marked changed when the ✕ or Escape clear it.
pub fn search_field(ui: &mut Ui, text: &mut String, hint: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
        icons::paint(ui.painter(), r, "search", t.text_dim, t.accent);
        let clear = !text.is_empty();
        let room = ui.available_width() - if clear { 22.0 + ui.spacing().item_spacing.x } else { 0.0 };
        // The box keeps Escape (egui would drop its focus on it before the global shortcuts ran,
        // which then take the key): Escape clears the text, and on an empty box lets go.
        let keys = egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, escape: true, ..Default::default() };
        let edit = egui::TextEdit::singleline(text).hint_text(hint).frame(egui::Frame::NONE);
        let mut edit = ui.add(edit.desired_width(room).event_filter(keys));
        if edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if text.is_empty() {
                edit.surrender_focus();
            } else {
                text.clear();
                edit.mark_changed();
            }
        }
        if clear {
            let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Clear")));
            if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(5), t.hover);
            }
            icons::paint(ui.painter(), r.shrink(5.0), "close", t.text_dim, t.accent);
            if resp.on_hover_text(tl!("Clear")).clicked() {
                text.clear();
                edit.mark_changed();
            }
        }
        edit
    })
    .inner
}

/// A colour button whose picker has no "Additive" blending (#381). RustGIS symbols blend normally
/// only, and egui's additive colour is a premultiplied one with zero alpha, which unmultiplies to
/// transparent black (`to_srgba_unmultiplied`): picking it made the symbol disappear.
pub fn color_button(ui: &mut Ui, color: &mut egui::Color32) -> Response {
    egui::color_picker::color_edit_button_srgba(ui, color, egui::color_picker::Alpha::OnlyBlend)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use egui_kittest::kittest::Queryable;

    /// Opens a colour button's picker and says whether it offers additive blending.
    fn offers_additive(button: fn(&mut egui::Ui, &mut egui::Color32)) -> bool {
        let mut h = egui_kittest::Harness::builder()
            .build_ui_state(|ui, c: &mut egui::Color32| button(ui, c), egui::Color32::from_rgb(224, 105, 59));
        h.get_by_role(egui::accesskit::Role::ColorWell).click();
        h.run();
        h.query_by_label("Additive").is_some()
    }

    #[test]
    fn colour_buttons_offer_no_additive_blending() {
        // egui's own button offers it, so the check can tell; RustGIS's doesn't (#381).
        assert!(offers_additive(|ui, c| {
            ui.color_edit_button_srgba(c);
        }));
        assert!(!offers_additive(|ui, c| {
            super::color_button(ui, c);
        }));
    }

    /// A field being typed into lets go of its focus on a press on the panel outside it, but keeps
    /// it through a press on a list over it until the release, as the search box's list needs to
    /// take the click (#1194).
    #[test]
    fn a_press_on_a_panel_lets_go_of_a_field_and_one_on_a_list_waits_for_the_release() {
        use egui::accesskit::Role;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(600.0, 400.0)).build_ui_state(
            |ui, text: &mut String| {
                super::let_go_on_panel_press(ui.ctx());
                let label = ui.label("Name");
                ui.add(egui::TextEdit::singleline(text)).labelled_by(label.id);
                let _ = ui.button("Panel");
                egui::Area::new(egui::Id::unique("list")).order(egui::Order::Foreground).fixed_pos(egui::pos2(300.0, 200.0)).show(
                    ui.ctx(),
                    |ui| {
                        let _ = ui.button("Listed");
                    },
                );
            },
            String::new(),
        );
        h.run();
        let focused = |h: &egui_kittest::Harness<'_, String>| h.query_all_by_role(Role::TextInput).any(|n| n.is_focused());
        for (button, kept) in [("Listed", true), ("Panel", false)] {
            h.get_by_role(Role::TextInput).focus();
            h.run();
            assert!(focused(&h));
            let at = h.get_by_label(button).rect().center();
            h.hover_at(at);
            h.drag_at(at);
            h.step();
            h.step();
            assert_eq!(focused(&h), kept, "{button}: pressed");
            h.drop_at(at);
            h.run();
            assert!(!focused(&h), "{button}: let go");
        }
    }

    /// A number dragged and let go keeps the dragged value (#607), a decimal and a whole one
    /// alike, though its caller lends the saved one each frame and saves only what is settled, as
    /// the panes' callers do (it used to end where it started).
    #[test]
    fn a_dragged_number_is_kept_when_let_go() {
        use egui::accesskit::Role;
        #[derive(Default)]
        struct Saved {
            real: f64,
            whole: u64,
            settled: Vec<&'static str>,
        }
        let mut h = egui_kittest::Harness::builder().build_ui_state(
            |ui, s: &mut Saved| {
                let mut real = s.real;
                let drag = super::Dragged::lend(ui, egui::Id::unique("real"), &mut real);
                let r = ui.add(egui::DragValue::new(&mut real).speed(0.1)).on_hover_text("Real");
                if drag.settled(ui, &r, &mut real) && real != s.real {
                    s.real = real;
                    s.settled.push("real");
                }
                let mut whole = s.whole;
                let drag = super::Dragged::lend(ui, egui::Id::unique("whole"), &mut whole);
                let r = ui.add(egui::DragValue::new(&mut whole).speed(0.1)).on_hover_text("Whole");
                if drag.settled(ui, &r, &mut whole) && whole != s.whole {
                    s.whole = whole;
                    s.settled.push("whole");
                }
            },
            Saved { real: 1.0, whole: 2, ..Saved::default() },
        );
        h.run();
        let spins: Vec<egui::Pos2> = h.query_all_by_role(Role::SpinButton).map(|n| n.rect().center()).collect();
        assert_eq!(spins.len(), 2);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        for at in spins {
            h.event(egui::Event::PointerMoved(at));
            h.step();
            h.event(button(at, true));
            h.step();
            // Dragged right over several frames, then let go without moving.
            for i in 1..=4 {
                h.event(egui::Event::PointerMoved(at + egui::vec2(10.0 * i as f32, 0.0)));
                h.step();
            }
            h.event(button(at + egui::vec2(40.0, 0.0), false));
            h.step();
            h.run();
        }
        let s = h.state();
        assert_eq!(s.settled, ["real", "whole"], "each settled once");
        assert!(s.real > 3.0 && s.whole > 4, "kept: {} and {}", s.real, s.whole);
    }
}

#[cfg(test)]
mod split_tests;
