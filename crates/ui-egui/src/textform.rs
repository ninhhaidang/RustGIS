//! The text form shared by a text graphic (Add Text) and a layout's text (the Format pane, #516):
//! its text, dynamic text, size, colour, boldness, italic, alignment and letter spacing (#535),
//! laid out and limited the same in both, so the same text can look the same on the map and on
//! the page; a layout's text also has its font family (#517). It edits the values it is lent
//! and says what was settled; each caller runs its own command.

use egui::{Color32, Ui, vec2};
use rustgis_engine::map::layout::{FontFamily, MAX_LETTER_SPACING, MIN_LETTER_SPACING, TextAlign};
use rustgis_engine::render::families;

use crate::widgets::Dragged;

/// The most installed families the Font list shows at once, as the Labeling pane's; the rest are
/// found by typing part of a name.
const MAX_FONTS_SHOWN: usize = 200;

/// What the form edits. A style is left out where the text doesn't have it.
pub struct Fields<'a> {
    pub text: &'a mut String,
    pub size: &'a mut f64,
    pub color: Option<&'a mut Color32>,
    pub bold: Option<&'a mut bool>,
    pub italic: Option<&'a mut bool>,
    /// How the lines line up: within a layout text's box, or in a text graphic's block.
    pub align: Option<&'a mut TextAlign>,
    /// Points between letters, from −5 (tighter) to 50.
    pub letter_spacing: Option<&'a mut f64>,
    /// Sans-serif or serif: a layout text's (a text graphic is drawn in the map's sans); with
    /// `font_name`, the family drawn where that one isn't installed.
    pub font: Option<&'a mut FontFamily>,
    /// An installed family by name (#762), a layout text's: listed after the generic ones when
    /// `named_fonts`.
    pub font_name: Option<&'a mut Option<String>>,
    pub named_fonts: bool,
    /// The text is focused and selected whole, so typing replaces it (a text just placed).
    pub select: bool,
}

/// What the user settled this frame.
#[derive(Debug, Default)]
pub struct Settled {
    /// The text is being typed: the caller keeps it as a draft, not saved yet.
    pub typing: bool,
    /// The text is done: its field lost focus, Enter was pressed, or a dynamic text was added.
    pub text: bool,
    /// The size changed and was let go (a drag ended, a value typed in).
    pub size: bool,
    /// The colour changed: its button, for the caller to make the pick one undo step
    /// ([`crate::widgets::one_step_per_press`]).
    pub color: Option<egui::Response>,
    pub bold: bool,
    pub italic: bool,
    pub align: bool,
    /// The letter spacing changed and was let go, as the size.
    pub letter_spacing: bool,
    /// Another family was picked.
    pub font: bool,
}

/// A section of the Format pane ("Element", "Text"), folding as its other sections do (a frame's
/// Grid, a legend's Layers): open unless the user folded it, which holds for every element.
pub fn section<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    ui.add_space(4.0);
    egui::CollapsingHeader::new(tl!(title)).id_salt(("format_section", title)).default_open(true).show(ui, add).body_returned
}

/// The width the labels of a properties form take, so the controls of its grids line up in one
/// column (a longer label, in some languages, widens its own grid only).
pub const LABEL_W: f32 = 96.0;

/// The form, its fields told apart by `id` (an element, a dialog). `tokens` are the dynamic text
/// lists the text expands where it is shown, each a section of the Dynamic Text menu.
pub fn show(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, tokens: &[&[(&str, &str)]], f: Fields<'_>) -> Settled {
    let id = egui::Id::unique(id);
    let mut out = Settled::default();
    let hint = tl!("Shift+Enter starts a new line.");
    let mut edit =
        crate::dialogs::text_box(f.text).id_salt(id.with("text")).hint_text(hint).desired_rows(3).desired_width(f32::INFINITY).show(ui);
    if f.select {
        edit.response.request_focus();
        let end = f.text.chars().count();
        edit.state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(end))));
        edit.state.store(ui.ctx(), edit.response.id);
    }
    let r = edit.response;
    // Enter is done typing, as in the dialogs; Shift+Enter starts a line.
    if r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift) {
        r.surrender_focus();
        out.text = true;
    }
    out.text |= r.lost_focus();
    // The values the text can show, as the ribbon's Dynamic Text menu lists them: one line here,
    // and what each is said in words.
    ui.menu_button(tl!("Insert Dynamic Text"), |ui| {
        for (i, list) in tokens.iter().enumerate() {
            if i > 0 {
                ui.separator();
            }
            for &(token, what) in *list {
                if ui.button(format!("{}  {token}", tl!(what))).clicked() {
                    f.text.push_str(token);
                    out.text = true;
                    ui.close();
                }
            }
        }
    });
    out.typing = r.has_focus() && !out.text;
    egui::Grid::new(id.with("style")).num_columns(2).min_col_width(LABEL_W).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
        let input_label = ui.label(tl!("Size"));
        let saved = *f.size;
        let drag = Dragged::lend(ui, id.with("size"), f.size);
        let r = ui
            .add(
                egui::DragValue::new(f.size)
                    .update_while_editing(false)
                    .range(rustgis_engine::map::graphics::FONT_SIZE)
                    .speed(0.2)
                    .suffix(" pt"),
            )
            .labelled_by(input_label.id);
        // Named for assistive technology, as its label sits in the other column.
        let size = *f.size;
        r.widget_info(|| egui::WidgetInfo { label: Some(tl!("Size").to_string()), ..egui::WidgetInfo::drag_value(true, size) });
        out.size = drag.settled(ui, &r, f.size) && (*f.size - saved).abs() > 1e-6;
        ui.end_row();
        if let Some(c) = f.color {
            ui.label(tl!("Color"));
            out.color = Some(crate::widgets::color_button(ui, c)).filter(egui::Response::changed);
            ui.end_row();
        }
        if let Some(b) = f.bold {
            let input_label = ui.label(tl!("Bold"));
            let r = ui.checkbox(b, "").labelled_by(input_label.id);
            let on = *b;
            r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, tl!("Bold")));
            out.bold = r.changed();
            ui.end_row();
        }
        if let Some(it) = f.italic {
            let input_label = ui.label(tl!("Italic"));
            // Named for assistive technology, as its label sits in the other column.
            let r = ui.checkbox(it, "").labelled_by(input_label.id);
            let on = *it;
            r.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, tl!("Italic")));
            out.italic = r.changed();
            ui.end_row();
        }
        if let Some(a) = f.align {
            let input_label = ui.label(tl!("Alignment"));
            let shown = tl!(a.label());
            egui::ComboBox::from_id_salt(id.with("align"))
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    for v in TextAlign::ALL {
                        if ui.selectable_label(*a == v, tl!(v.label())).clicked() && *a != v {
                            *a = v;
                            out.align = true;
                        }
                    }
                })
                .response
                .labelled_by(input_label.id);
            ui.end_row();
        }
        if let Some(ls) = f.letter_spacing {
            let input_label = ui.label(tl!("Letter spacing"));
            let saved = *ls;
            let drag = Dragged::lend(ui, id.with("letter_spacing"), ls);
            let r = ui
                .add(
                    egui::DragValue::new(ls)
                        .update_while_editing(false)
                        .range(MIN_LETTER_SPACING..=MAX_LETTER_SPACING)
                        .speed(0.1)
                        .max_decimals(1)
                        .suffix(" pt"),
                )
                .labelled_by(input_label.id);
            let now = *ls;
            r.widget_info(|| egui::WidgetInfo {
                label: Some(tl!("Letter spacing").to_string()),
                ..egui::WidgetInfo::drag_value(true, now)
            });
            out.letter_spacing = drag.settled(ui, &r, ls) && (*ls - saved).abs() > 1e-6;
            ui.end_row();
        }
        if let Some(fam) = f.font {
            let mut no_name = None;
            let name = f.font_name.unwrap_or(&mut no_name);
            // The installed families (#762) once indexed, on a thread of their own; until then the
            // two generic ones.
            let installed = if f.named_fonts { families::installed_now() } else { None };
            if f.named_fonts && installed.is_none() {
                families::warm_up();
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            }
            let missing = name.as_deref().is_some_and(|n| installed.is_some() && families::find_ready(n).is_none());
            let shown = match name.as_deref() {
                Some(n) if missing => crate::i18n::fmt("{name} (not installed)", &[("name", n)]),
                Some(n) => n.to_string(),
                None => tl!(fam.label()).to_string(),
            };
            let input_label = ui.label(tl!("Font"));
            let search_id = id.with("font_search");
            let list = egui::ComboBox::from_id_salt(id.with("font"))
                .selected_text(shown)
                .height(320.0)
                .close_behavior(crate::labelpane::SEARCHED_LIST_CLOSES)
                .show_ui(ui, |ui| {
                    // Sans-serif and Serif first, then the installed families, found by typing.
                    let search =
                        if installed.is_some() { crate::labelpane::list_search(ui, search_id, tl!("Search fonts")) } else { String::new() };
                    for v in FontFamily::ALL {
                        if ui.selectable_label(name.is_none() && *fam == v, tl!(v.label())).clicked() {
                            if name.is_some() || *fam != v {
                                (*fam, *name) = (v, None);
                                out.font = true;
                            }
                            crate::labelpane::end_search(ui, search_id);
                        }
                    }
                    let Some(installed) = installed else { return };
                    ui.separator();
                    // A family named as a generic one ("Serif") is the generic one: not listed again.
                    let mut matching = installed.iter().filter(|f| {
                        f.listed
                            && FontFamily::from_name(&f.name).is_none()
                            && (search.is_empty() || f.name.to_lowercase().contains(&search))
                    });
                    for v in matching.by_ref().take(MAX_FONTS_SHOWN) {
                        let on = name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&v.name));
                        if ui.selectable_label(on, &v.name).clicked() {
                            if !on {
                                // Its fallback, from its class, as `layout.update` gives it.
                                (*fam, *name) = (if v.serif { FontFamily::Serif } else { FontFamily::Sans }, Some(v.name.clone()));
                                out.font = true;
                            }
                            crate::labelpane::end_search(ui, search_id);
                        }
                    }
                    let more = matching.count();
                    if more > 0 {
                        ui.weak(crate::i18n::fmt("{count} more: type part of a name", &[("count", &more.to_string())]));
                    }
                })
                .response
                .labelled_by(input_label.id);
            // Named for assistive technology, as its label sits in the other column.
            list.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, tl!("Font")));
            ui.end_row();
            // A named family is drawn in its fallback where it isn't installed (a project from
            // another computer): which one, chosen here.
            if name.is_some() {
                let input_label = ui.label(tl!("Fallback"));
                let list = egui::ComboBox::from_id_salt(id.with("fallback"))
                    .selected_text(tl!(fam.label()))
                    .show_ui(ui, |ui| {
                        for v in FontFamily::ALL {
                            if ui.selectable_label(*fam == v, tl!(v.label())).clicked() && *fam != v {
                                *fam = v;
                                out.font = true;
                            }
                        }
                    })
                    .response
                    .labelled_by(input_label.id)
                    .on_hover_text(tl!("Where the font isn't installed, the text is drawn in this family"));
                list.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, tl!("Fallback")));
                ui.end_row();
            }
            // A text not drawn as asked says so here, so the page never differs from what was
            // asked unsaid: a family this computer hasn't, or a serif where none is installed.
            let serif_drawn = *fam == FontFamily::Serif && rustgis_engine::render::fonts::serif_available();
            let note = if missing {
                Some(if serif_drawn {
                    tl!("This font isn't installed here, so the text is drawn in the serif")
                } else {
                    tl!("This font isn't installed here, so the text is drawn in the sans-serif")
                })
            } else if name.is_none() && *fam == FontFamily::Serif && !serif_drawn {
                Some(tl!("No serif font is installed, so this text is drawn in the sans-serif"))
            } else {
                None
            };
            if let Some(note) = note {
                ui.label("");
                ui.label(egui::RichText::new(note).weak().small());
                ui.end_row();
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;

    #[derive(Default)]
    struct Form {
        text: String,
        size: f64,
        color: Color32,
        bold: bool,
        italic: bool,
        align: TextAlign,
        letter_spacing: f64,
        /// Whether the style (colour, boldness, italic, alignment, letter spacing) is offered.
        styled: bool,
        settled: Vec<&'static str>,
    }

    fn harness(form: Form) -> egui_kittest::Harness<'static, Form> {
        egui_kittest::Harness::builder().build_ui_state(
            |ui, f: &mut Form| {
                // The numbers are lent saved and kept only once settled, as the pane's callers do.
                let (mut size, mut spacing) = (f.size, f.letter_spacing);
                let s = show(
                    ui,
                    "form",
                    &[rustgis_engine::map::graphics::TOKENS, rustgis_engine::map::layout::PAGE_TOKENS],
                    Fields {
                        text: &mut f.text,
                        size: &mut size,
                        color: f.styled.then_some(&mut f.color),
                        bold: f.styled.then_some(&mut f.bold),
                        italic: f.styled.then_some(&mut f.italic),
                        align: f.styled.then_some(&mut f.align),
                        letter_spacing: f.styled.then_some(&mut spacing),
                        font: None,
                        font_name: None,
                        named_fonts: false,
                        select: false,
                    },
                );
                for (on, what) in [
                    (s.text, "text"),
                    (s.size, "size"),
                    (s.color.is_some(), "color"),
                    (s.bold, "bold"),
                    (s.italic, "italic"),
                    (s.align, "align"),
                    (s.letter_spacing, "letterSpacing"),
                ] {
                    if on {
                        f.settled.push(what);
                    }
                }
                if s.size {
                    f.size = size;
                }
                if s.letter_spacing {
                    f.letter_spacing = spacing;
                }
            },
            form,
        )
    }

    /// Every token listed is in the Dynamic Text menu, said in words; picking one adds it to the
    /// text, which is then settled.
    #[test]
    fn dynamic_text_is_added_from_every_list() {
        let mut h = harness(Form { text: "Page ".into(), size: 12.0, ..Form::default() });
        h.run();
        for (item, text) in [("Page number  {page}", "Page {page}"), ("Map scale  {scale}", "Page {page}{scale}")] {
            h.get_by_label("Insert Dynamic Text").click();
            h.run();
            h.get_by_label(item).click();
            h.run();
            assert_eq!(h.state().text, text);
        }
        assert_eq!(h.state().settled, ["text", "text"]);
        assert!(h.query_by_label("Page number  {page}").is_none(), "the menu closes");
    }

    /// Colour and boldness show only where the text has them; the size stays in the one range
    /// text is drawn at.
    #[test]
    fn colour_and_bold_show_only_when_lent() {
        let mut h = harness(Form { text: "T".into(), size: 12.0, ..Form::default() });
        h.run();
        assert!(h.query_by_label("Color").is_none() && h.query_by_label("Bold").is_none());
        let mut h = harness(Form { text: "T".into(), size: 12.0, styled: true, ..Form::default() });
        h.run();
        h.get_by_role_and_label(Role::CheckBox, "Bold").click();
        h.run();
        assert!(h.state().bold);
        assert_eq!(h.state().settled, ["bold"]);
    }

    /// Italic, alignment and letter spacing show only where the text has them, under the same
    /// names for a layout's text and a text graphic (#535); a change is settled.
    #[test]
    fn italic_alignment_and_spacing_show_only_when_lent() {
        let mut h = harness(Form { text: "T".into(), size: 12.0, ..Form::default() });
        h.run();
        for name in ["Italic", "Alignment", "Letter spacing"] {
            assert!(h.query_by_label(name).is_none(), "{name} offered unlent");
        }
        let mut h = harness(Form { text: "T".into(), size: 12.0, styled: true, ..Form::default() });
        h.run();
        assert!(h.query_by_label("Alignment").is_some());
        assert!(h.query_by_role_and_label(Role::SpinButton, "Letter spacing").is_some());
        h.get_by_role_and_label(Role::CheckBox, "Italic").click();
        h.run();
        assert!(h.state().italic);
        assert_eq!(h.state().settled, ["italic"]);
    }

    /// A number dragged and let go keeps the dragged value, though the caller lends the saved one
    /// each frame and egui forgets the dragged one on release (it used to end where it started).
    #[test]
    fn a_dragged_number_is_kept_when_let_go() {
        let mut h = harness(Form { text: "T".into(), size: 12.0, styled: true, ..Form::default() });
        h.run();
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        for (name, what) in [("Letter spacing", "letterSpacing"), ("Size", "size")] {
            let at = h.get_by_role_and_label(Role::SpinButton, name).rect().center();
            h.event(egui::Event::PointerMoved(at));
            h.step();
            h.event(button(at, true));
            h.step();
            // Dragged right over several frames, then let go without moving.
            for i in 1..=4 {
                h.event(egui::Event::PointerMoved(at + vec2(10.0 * i as f32, 0.0)));
                h.step();
            }
            h.event(button(at + vec2(40.0, 0.0), false));
            h.step();
            h.run();
            assert_eq!(h.state().settled.iter().filter(|s| **s == what).count(), 1, "{name}: settled {:?}", h.state().settled);
        }
        let (spacing, size) = (h.state().letter_spacing, h.state().size);
        assert!(spacing > 1.0 && size > 14.0, "kept: spacing {spacing}, size {size}");
    }
}
