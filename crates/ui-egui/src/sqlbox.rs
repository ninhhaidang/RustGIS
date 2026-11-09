//! A SQL `WHERE` box (#618): the clause a label (or later, a filter) applies to, written as a draft
//! and applied in one go, as ArcGIS Pro's SQL query of a label class. It holds only the draft (in
//! egui's memory, one per `slot`); the clause itself lives in the project, so undo, scripts and the
//! Assistant changing it show here.

use egui::{RichText, Ui};
use rustgis_engine::data::Dataset;

use crate::theme::Tokens;

/// What the box asks for this frame.
#[derive(Debug, PartialEq)]
pub enum Apply {
    /// Use this clause.
    Clause(String),
    /// No clause: every feature.
    Clear,
}

/// The draft of one owner's clause (a layer's label, for now).
#[derive(Clone)]
struct Draft {
    owner: egui::Id,
    /// The clause in the project when the draft last followed it.
    base: Option<String>,
    text: String,
    /// Shown as a box rather than "There is no expression defined.".
    open: bool,
    /// The box takes the keyboard on the next frame (just opened).
    focus: bool,
    /// The last Check: the text it checked, the caller's `version` then, and how many of how many
    /// features match, or why the text doesn't read. Shown while both are unchanged.
    check: Option<(String, u64, Result<(usize, usize), String>)>,
}

/// The longest list of fields offered as chips (a wider table is typed).
const MAX_CHIPS: usize = 40;

/// The box in `slot` for `owner`'s clause, `stored` in the project, over the fields of `ds`.
/// `count` runs only on Check: how many features match a clause that reads, of how many; its
/// answer shows while `version` (whatever the count depends on: the project's revision, the
/// scale) is the same. A draft is kept while the same owner
/// is shown in the slot and dropped for another; it follows the stored clause until it is edited,
/// and then says it isn't applied.
pub fn show(
    ui: &mut Ui,
    slot: impl std::hash::Hash + std::fmt::Debug,
    owner: egui::Id,
    version: u64,
    stored: Option<&str>,
    ds: &Dataset,
    count: impl FnOnce(&str) -> Result<(usize, usize), String>,
) -> Option<Apply> {
    let key = egui::Id::unique(("sqlbox_draft", slot));
    let t = &Tokens::get(ui.ctx());
    let stored = stored.map(str::trim).filter(|w| !w.is_empty());
    let fresh = || Draft {
        owner,
        base: stored.map(String::from),
        text: stored.unwrap_or_default().to_string(),
        open: stored.is_some(),
        focus: false,
        check: None,
    };
    let mut d = ui.data(|m| m.get_temp::<Draft>(key)).filter(|d| d.owner == owner).unwrap_or_else(fresh);
    // Changed underneath (undo, a script): an untouched draft follows it, an edited one is kept.
    if d.base.as_deref() != stored {
        if d.text.trim() == d.base.as_deref().unwrap_or_default() {
            d.text = stored.unwrap_or_default().to_string();
            d.open = stored.is_some();
        }
        d.base = stored.map(String::from);
    }
    let mut apply = None;
    if !d.open {
        ui.label(RichText::new(tl!("There is no expression defined.")).italics().color(t.text_dim));
        if ui.button(tl!("+ New expression")).clicked() {
            (d.open, d.focus) = (true, true);
        }
    } else {
        apply = editor(ui, t, &mut d, version, stored, ds, count);
    }
    ui.data_mut(|m| m.insert_temp(key, d));
    apply
}

fn editor(
    ui: &mut Ui,
    t: &Tokens,
    d: &mut Draft,
    version: u64,
    stored: Option<&str>,
    ds: &Dataset,
    count: impl FnOnce(&str) -> Result<(usize, usize), String>,
) -> Option<Apply> {
    let id = ui.make_persistent_id("sqlbox_text");
    // Taken before the box sees it, so it applies rather than breaking the line.
    let ctrl_enter = ui.memory(|m| m.has_focus(id)) && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
    let edit = egui::TextEdit::multiline(&mut d.text)
        .id(id)
        .hint_text(tl!("e.g. pop_max > 1000000"))
        .desired_rows(3)
        // No longer than a clause can be, so a paste isn't read again each frame only to be refused.
        .char_limit(rustgis_engine::data::query::MAX_LEN)
        .desired_width(f32::INFINITY)
        .font(egui::TextStyle::Monospace);
    let r = ui.add(edit);
    // A new box takes the keyboard, so the clause can be typed at once.
    if std::mem::take(&mut d.focus) {
        r.request_focus();
    }
    chips(ui, &mut d.text, ds);
    let text = d.text.trim().to_string();
    // Live syntax and field checks must not scan geometry or invalidate a retained clause.
    // Check and the command handling Apply perform the full data validation.
    let reads = if text.is_empty() { Ok(()) } else { rustgis_engine::data::query::parse_fields(&text, ds).map(|_| ()) };
    let changed = text != stored.unwrap_or_default();
    let mut apply = None;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!text.is_empty(), egui::Button::new(tl!("Check")))
            .on_hover_text(tl!("Check the clause, and count the features it takes."))
            .clicked()
        {
            let result = match &reads {
                Ok(()) => count(&text),
                Err(e) => Err(e.clone()),
            };
            d.check = Some((text.clone(), version, result));
        }
        let can_apply = changed && reads.is_ok();
        let hover = if text.is_empty() { tl!("Remove the clause.") } else { tl!("Label only the features that match.") };
        if ui.add_enabled(can_apply, egui::Button::new(tl!("Apply"))).on_hover_text(hover).clicked() || (ctrl_enter && can_apply) {
            apply = Some(if text.is_empty() { Apply::Clear } else { Apply::Clause(text.clone()) });
        }
        if ui
            .add_enabled(stored.is_some() || !text.is_empty(), egui::Button::new(tl!("Clear")))
            .on_hover_text(tl!("Remove the clause."))
            .clicked()
        {
            d.text.clear();
            d.open = false;
            d.check = None;
            if stored.is_some() {
                apply = Some(Apply::Clear);
            }
        }
    });
    if apply == Some(Apply::Clear) {
        d.open = false;
    }
    match (&reads, &d.check) {
        (Err(e), _) => {
            let said = if stored == Some(text.as_str()) {
                crate::i18n::fmt("This clause no longer reads, so no feature is labelled: {error}", &[("error", e)])
            } else {
                crate::i18n::fmt("Invalid: {error}", &[("error", e)])
            };
            ui.label(RichText::new(said).small().color(t.red));
        }
        (Ok(()), Some((checked, at, Ok((n, of))))) if *checked == text && *at == version => {
            let said = crate::i18n::fmt("Valid: {n} of {of} features match", &[("n", &n.to_string()), ("of", &of.to_string())]);
            ui.label(RichText::new(said).small().color(t.green));
        }
        (Ok(()), Some((checked, at, Err(e)))) if *checked == text && *at == version => {
            ui.label(RichText::new(crate::i18n::fmt("Invalid: {error}", &[("error", e)])).small().color(t.red));
        }
        _ => {}
    }
    if changed && d.open {
        ui.label(RichText::new(tl!("Not applied yet.")).small().color(t.text_dim));
    }
    apply
}

/// The layer's fields and the common operators, each added to the clause where it ends.
fn chips(ui: &mut Ui, text: &mut String, ds: &Dataset) {
    let add = |text: &mut String, word: &str| {
        if !text.is_empty() && !text.ends_with(' ') {
            text.push(' ');
        }
        text.push_str(word);
        text.push(' ');
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(tl!("Fields:")).small().weak());
        for f in ds.fields.iter().take(MAX_CHIPS) {
            if ui.small_button(&f.name).clicked() {
                add(text, &field_ref(&f.name));
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        for op in ["=", "<>", ">", "<", ">=", "<=", "LIKE", "IN", "IS NULL", "AND", "OR", "NOT"] {
            if ui.small_button(op).clicked() {
                add(text, op);
            }
        }
    });
}

/// A field as a clause names it: as it is when it reads as a plain name, else in double quotes.
fn field_ref(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    // The words the clause language reads itself (`rustgis_data::query`), values included.
    let keyword = ["AND", "OR", "NOT", "LIKE", "IN", "IS", "NULL", "TRUE", "FALSE"].iter().any(|k| k.eq_ignore_ascii_case(name));
    if plain && !keyword { name.to_string() } else { format!("\"{}\"", name.replace('"', "\"\"")) }
}

#[cfg(test)]
mod tests {
    use super::field_ref;

    #[test]
    fn a_field_is_quoted_only_when_a_clause_needs_it() {
        assert_eq!(field_ref("pop_max"), "pop_max");
        assert_eq!(field_ref("Name 2"), "\"Name 2\"");
        assert_eq!(field_ref("2020"), "\"2020\"");
        assert_eq!(field_ref("in"), "\"in\"");
        assert_eq!(field_ref("True"), "\"True\"");
        assert_eq!(field_ref("año"), "año");
        assert_eq!(field_ref("a\"b"), "\"a\"\"b\"");
    }
}
