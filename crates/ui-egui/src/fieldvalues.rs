//! Select By Attributes' Values list: the distinct values of a field, to pick from. A user who
//! clicked a field and wonders what it holds sees them without opening the table, and a click
//! writes the comparison, the value quoted as the expression needs it.
//!
//! The values are what the completion offers after `"field" = '` (`query::field_values`): read
//! from an evenly spread sample of at most `query::VALUES_READ` features, a streamed layer's from
//! the features in view. They are read once, when the list is opened for a field, and kept while
//! the dialog stays on that layer and field and the layer's rows don't change, and not read
//! again while the list stays open: it changes only as it opens.

use std::rc::Rc;
use std::sync::Weak;

use egui::Ui;
use egui::widgets::text_edit::TextEditState;
use rustgis_engine::data::query::{self, Completion, Kw, Place};
use rustgis_engine::data::{Dataset, Feature};

use crate::exprcomplete::{ValuesRead, field_ref, fields_key};

/// The Values list between frames, held by the dialog (`Dialogs::values`): which field it lists,
/// and the values read for it.
#[derive(Default)]
pub struct Form {
    /// The layer the field belongs to, by the name the dialog has it under.
    layer: String,
    /// The field last clicked, or the cursor was last in, as the layer names it.
    field: Option<String>,
    read: Option<Read>,
    /// Where the cursor was (a char index) when `field` came from it, and the text it was in: a
    /// value picked goes there while the text is as it was. A field clicked has none.
    cursor: Option<(usize, String)>,
    /// The frame the list was last drawn in: drawn the frame before, it is still the one open.
    drawn: Option<u64>,
    /// Why there is no list to show, for the dialog to say under the buttons.
    said: Option<String>,
    /// How many times the values were read: once per list opened, not once per frame.
    #[cfg(test)]
    pub(crate) reads: usize,
}

/// The values of a field, as read, and the rows and fields they were read from: another set of
/// rows (an edit, a reload, a streamed layer's next view) or another type of field reads them
/// again.
struct Read {
    rows: Weak<Vec<Feature>>,
    fields: u64,
    /// Which features they were read from, and how many have a value not listed.
    from: ValuesRead,
    /// What the list shows for each value: as the expression writes it. Shared with the list
    /// each frame it is open, not copied.
    written: Rc<[String]>,
}

impl Form {
    /// Follows the layer the dialog is on: the list is of one layer's field, so another layer
    /// has none until a field of it is chosen.
    pub(crate) fn follow(&mut self, layer: &str) {
        if self.layer != layer {
            self.layer = layer.to_string();
            self.field = None;
            self.read = None;
            self.cursor = None;
            self.said = None;
        }
    }

    /// Lists `field` from now on, as one clicked: a value picked is added at the end. Another
    /// field drops the values read for the last.
    pub(crate) fn choose(&mut self, field: &str) {
        self.cursor = None;
        if self.field.as_deref() != Some(field) {
            self.field = Some(field.to_string());
            self.read = None;
            self.said = None;
        }
    }

    /// Lists the field the box `id` (holding `text`, with the focus) has its cursor where a value
    /// compared with one of `ds`'s goes (`name = ▮`, `name IN ('a', ▮`): a field typed is a field
    /// chosen, as `query::complete_at` reads it.
    pub(crate) fn follow_box(&mut self, ui: &Ui, id: egui::Id, text: &str, ds: &Dataset) {
        let cursor = TextEditState::load(ui.ctx(), id).and_then(|s| s.cursor.char_range()).map(|r| r.primary.index.0);
        let here = cursor.and_then(|at| Some((at, query::complete_at(text, at, ds)?)));
        if here.as_ref().is_some_and(|(_, c)| c.compared && c.field.is_none()) {
            // Compared with something computed (`upper(name) = ▮`): no field's values to list,
            // not the one chosen before.
            self.field = None;
            self.read = None;
            self.cursor = None;
            return;
        }
        let at = here.and_then(|(at, c)| Some((at, c.field?)));
        let Some((at, field)) = at.and_then(|(at, i)| Some((at, ds.fields.get(i)?))) else {
            // The cursor is not where a value goes: a value picked goes at the end.
            self.cursor = None;
            return;
        };
        self.choose(&field.name);
        // Where the field came from: a value picked goes there (a clause with a gap in it is
        // filled, not added to).
        if self.cursor.as_ref().is_none_or(|(was, held)| *was != at || held != text) {
            self.cursor = Some((at, text.to_string()));
        }
    }

    /// The values of field `field` of `ds`, as written, and where they were read from. Read as
    /// the list opens, unless held from the rows and fields `ds` has now; then kept while it stays
    /// open (`frame`: it was drawn in the frame before), however the rows change under it, so
    /// what is highlighted and what a screen reader was told stay what Enter takes. Says why there
    /// is nothing to list when there is none.
    fn list(&mut self, ds: &Dataset, field: usize, frame: u64) -> (Rc<[String]>, ValuesRead) {
        let open = self.drawn.is_some_and(|was| was + 1 >= frame);
        self.drawn = Some(frame);
        if !open {
            let (rows, fields) = (ds.features.allocation_key(), fields_key(ds));
            let held = |r: &Read| r.fields == fields && r.rows.ptr_eq(&rows) && r.from.view == ds.windowed;
            if !self.read.as_ref().is_some_and(held) {
                self.read = None;
            }
        }
        #[cfg(test)]
        let reads = &mut self.reads;
        let read = self.read.get_or_insert_with(|| {
            #[cfg(test)]
            {
                *reads += 1;
            }
            let got = query::field_values(ds, field, query::VALUES_READ);
            let from = ValuesRead::of(&got, ds);
            let written = got.values.into_iter().map(|v| v.written).collect();
            Read { rows: ds.features.allocation_key(), fields: fields_key(ds), from, written }
        });
        let name = ds.fields.get(field).map(|f| f.name.as_str()).unwrap_or_default();
        self.said = read.written.is_empty().then(|| {
            // The parts in logical order, and the whole put in display form once.
            let field = field_ref(name);
            let none = crate::i18n::fmt_raw("{field} has no values to list", &[("field", &field)]);
            match read.from.left_out {
                0 => crate::i18n::display(&none),
                _ => crate::i18n::fmt("{none}. {left_out}", &[("none", &none), ("left_out", &read.from.left_out_note())]),
            }
        });
        (Rc::clone(&read.written), read.from)
    }

    /// Adds the value picked to `text`: at the cursor the field came from while the text is as it
    /// was then, else at the end. When it can't be added, the text stays as it is and the box
    /// says why.
    pub(crate) fn accept(&mut self, text: &mut String, field: &str, value: &str, ds: &Dataset) {
        let at = self.cursor.as_ref().filter(|(_, held)| held == text).map(|(at, _)| *at);
        // The list closed with the pick: opened again, even in the next frame, it is a new one.
        self.drawn = None;
        self.said = insert(text, field, value, ds, at).err();
        if self.said.is_none() {
            self.cursor = None;
        }
    }
}

/// The Values button, beside a clause's field buttons: opens the list of the chosen field's
/// values. Returns the field and the value accepted, as the expression writes it (`insert`
/// puts them in the clause). Without a field chosen, says to choose one.
pub(crate) fn button(ui: &mut Ui, form: &mut Form, ds: Option<&Dataset>) -> Option<(String, String)> {
    let label = tl!("Values ▾");
    let chosen = ds.zip(form.field.clone()).and_then(|(ds, name)| Some((ds, ds.fields.iter().position(|f| f.name == name)?, name)));
    let Some((ds, field, name)) = chosen else {
        if ui.button(label).clicked() {
            form.said = Some(tl!("Click a field, or put the cursor in one, to list its values").to_string());
        }
        return None;
    };
    let frame = ui.ctx().cumulative_frame_nr();
    let accepted = crate::exprcomplete::values_menu(ui, label, &name, || form.list(ds, field, frame));
    accepted.map(|value| (name, value))
}

/// What the dialog says under its buttons about the list, when there is nothing to list.
pub(crate) fn said(ui: &mut Ui, form: &Form) {
    if let Some(said) = &form.said {
        ui.label(egui::RichText::new(said).small().weak());
    }
}

/// Adds `value` (as the expression writes it) of `field` to the clause `text`, at char `at` (the
/// cursor the field came from; the end without one), where `query::complete_at` says the clause
/// stands there (`ds` has the fields). What follows `at` is kept.
/// - where a value compared with something goes (`name = `, `pop >`, `upper(name) =`, `name IN (`,
///   `pop BETWEEN `), only the value, which `BETWEEN` takes for both ends (`BETWEEN 10 AND 10`, to
///   edit) and a bracket left open (`IN (`) is closed after;
/// - after a bare `IN` (a list is next), the value as a list of one (`IN ('Asia')`);
/// - after the field itself, as its button writes it (`continent `), what goes after it
///   (`= 'Asia'`);
/// - after the first end of a `BETWEEN` (`pop_est BETWEEN 1 `), `AND` and the value;
/// - after a whole clause (`pop_est > 1 `), the comparison joined to it with `AND`;
/// - anywhere else, the comparison with the field (`"Pop Est" = 10`).
///
/// A text or a quoted name begun and left open (`name = 'As`) is replaced by what is added, to its
/// closing quote, as the completion does; a clause that can't be read at all gets the comparison,
/// and the box says why it doesn't read.
///
/// Nothing is added, and the reason is returned, when the value has a `%` or `_` where a `LIKE`
/// pattern goes (it would match more than itself), a value follows the cursor already, an operator
/// follows it where more than the value would be added, a quote at the cursor is never closed and
/// holds the rest of the clause, or the clause would pass `query::MAX_LEN`.
pub(crate) fn insert(text: &mut String, field: &str, value: &str, ds: &Dataset, at: Option<usize>) -> Result<(), String> {
    let split = at.and_then(|at| text.char_indices().nth(at)).map_or(text.len(), |(i, _)| i);
    let (before, mut after) = text.split_at(split);
    let mut head = before.to_string();
    let mut here = at_end(&head, ds);
    if let Some(open) = here.as_ref().filter(|c| c.place != Place::Code) {
        let keep = head.char_indices().nth(open.word.start).map_or(head.len(), |(i, _)| i);
        // What is left of the text, up to its closing quote, goes with what was typed of it.
        if let Some(quote) = head.get(keep..).unwrap_or_default().chars().next().filter(|q| matches!(q, '\'' | '"' | '!')) {
            after = match after_quote(after, quote) {
                Some(rest) => rest,
                None if after.trim().is_empty() => "",
                // Never closed, it holds the rest of the clause, which would go with it.
                None => {
                    return Err(tl!(
                        "Not added: the quote at the cursor is never closed, so the rest of the clause is in it. Close it first"
                    )
                    .to_string());
                }
            };
        }
        head.truncate(keep);
        here = at_end(&head, ds);
    }
    let next = query::follows(after);
    if next == query::Follows::Value {
        return Err(tl!("Not added: a value follows the cursor already. Put the cursor where none is, or delete that one").to_string());
    }
    if !here.as_ref().is_some_and(|c| c.operand || c.symbols.contains(&"=")) && ends_with_field(&head, field) {
        // `pop_est > continent ▮`: the field is the value of a comparison already.
        return Err(crate::i18n::fmt(
            "Not added: {field} is already a value here. Type an operator after it, or put the cursor after one",
            &[("field", &field_ref(field))],
        ));
    }
    if here.as_ref().is_some_and(|c| c.pattern) && value.contains(['%', '_']) {
        return Err(crate::i18n::fmt("{value} has a % or _, which matches more than itself after LIKE: use = for it", &[("value", value)]));
    }
    let alone = here.as_ref().is_some_and(|c| c.operand && c.compared);
    // `continent▮ = 'Old'`, `continent = ▮ != 'Old'`: what follows would compare what is added.
    // A value alone may go before an operator that makes a value of it (`pop_est > ▮ * 2`).
    if next == query::Follows::Comparison || (!alone && next == query::Follows::Operator) {
        return Err(tl!("Not added: an operator follows the cursor. Put the cursor after it, where its value goes").to_string());
    }
    let put = match &here {
        _ if alone => value.to_string(),
        Some(c) if !c.operand && c.symbols == ["("] => format!("({value})"),
        Some(c) if !c.operand && c.symbols.contains(&"=") && ends_with_field(&head, field) => format!("= {value}"),
        // `pop_est BETWEEN 1 ▮`: the second end.
        Some(c) if !c.operand && c.keywords == [Kw::And] => format!("AND {value}"),
        // After a whole clause (`pop_est > 1 ▮`, `(pop_est > 1) ▮`), the comparison joined to it.
        Some(c) if !c.operand && c.keywords.contains(&Kw::Or) && query::parse_fields(&head, ds).is_ok_and(|e| is_condition(&e)) => {
            format!("AND {} = {value}", field_ref(field))
        }
        _ => format!("{} = {value}", field_ref(field)),
    };
    crate::dialogs::add_word(&mut head, &put);
    // What was after it, from its first word.
    let after = after.trim_start();
    let join = |head: &str| match after.starts_with([')', ',']) {
        true => format!("{}{after}", head.trim_end()),
        false => format!("{head}{after}"),
    };
    // What the value left to finish (the end of `head` is where it was added), in the order the
    // clause asks for it: `BETWEEN 10` needs `AND` and a second end, which `BETWEEN (10` needs once
    // its bracket is closed. Each bracket left open is closed after the value, unless what follows
    // closes it (`(continent = ▮ AND pop_est > 1)`) or the value goes on in it
    // (`IN (▮, 'Africa')`); what follows doesn't otherwise count (an unfinished clause after it
    // isn't a bracket left open).
    let opened = |text: &str| query::brackets(text).unwrap_or_default();
    // Before an operator that goes on with it (`BETWEEN ▮ * 2`, `IN (▮ * 2`), the value isn't
    // finished, so nothing is added after it.
    let goes_on = next == query::Follows::Operator;
    let mut missing = match after.starts_with(',') || goes_on {
        true => 0,
        // What follows that can't be read is kept as it is, its brackets not guessed at.
        false => query::brackets(after).map_or(0, |(_, closed)| opened(&head).0.saturating_sub(closed)),
    };
    let mut second_end = !goes_on && !starts_with_word(after, "and");
    loop {
        let then = at_end(&head, ds);
        if second_end && then.as_ref().is_some_and(|c| !c.operand && c.keywords == [Kw::And]) {
            crate::dialogs::add_word(&mut head, &format!("AND {value}"));
            second_end = false;
        } else if missing > 0 && then.is_some_and(|c| c.symbols.contains(&")")) {
            head.pop();
            head.push_str(") ");
            missing -= 1;
        } else {
            break;
        }
    }
    let done = join(&head);
    if done.chars().count() > query::MAX_LEN {
        return Err(crate::i18n::fmt(
            "Not added: the clause would be longer than {max} characters",
            &[("max", &query::MAX_LEN.to_string())],
        ));
    }
    // What these rules miss: a clause that read as far as it went doesn't stop reading.
    let reads = |text: &str| query::complete_at(text, text.chars().count(), ds).is_some();
    if reads(text) && !reads(&done) {
        return Err(tl!("Not added: the clause wouldn't read with it there. Put the cursor where a value or a comparison goes").to_string());
    }
    *text = done;
    Ok(())
}

/// Whether `e` is a condition, true or false, which `AND` can join another to (`pop_est > 1`),
/// and not a value that a comparison could still follow (`upper(name)`, `pop_est + 1`).
fn is_condition(e: &query::Expr) -> bool {
    use query::Expr as E;
    !matches!(e, E::Field(_) | E::Lit(_) | E::Var(_) | E::Call(..) | E::Concat(_) | E::Chain(..) | E::Case { .. })
        || matches!(e, E::Lit(rustgis_engine::data::Value::Bool(_)))
}

/// What is left of `text` after the quoted text or name that began before it, up to its closing
/// quote (a doubled `'` or `"` is that quote in it; `!` has no such escape, as the lexer reads
/// it): `None` when the quote is never closed.
fn after_quote(text: &str, quote: char) -> Option<&str> {
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == quote {
            if quote != '!' && chars.next_if(|&(_, next)| next == quote).is_some() {
                continue;
            }
            return Some(text.get(i + c.len_utf8()..).unwrap_or_default());
        }
    }
    None
}

/// Whether `text` starts with the word `word`, letter case aside (not a longer name that starts
/// with it).
fn starts_with_word(text: &str, word: &str) -> bool {
    text.get(..word.len()).is_some_and(|start| start.eq_ignore_ascii_case(word))
        && !text.get(word.len()..).unwrap_or_default().starts_with(|c: char| c.is_alphanumeric() || c == '_')
}

/// Whether `text` (trailing space aside) ends with `field` as its button writes it, and not as the
/// end of a longer name: what a click on the field's button leaves, to go on from.
fn ends_with_field(text: &str, field: &str) -> bool {
    let (text, written) = (text.trim_end(), field_ref(field));
    let (n, len) = (written.chars().count(), text.chars().count());
    let Some(start) = len.checked_sub(n) else { return false };
    let at = text.char_indices().nth(start).map_or(text.len(), |(i, _)| i);
    text.get(at..).is_some_and(|end| end.eq_ignore_ascii_case(&written))
        && (written.starts_with('"')
            || !text.get(..at).unwrap_or_default().ends_with(|c: char| c.is_alphanumeric() || matches!(c, '_' | '@' | '$' | '.')))
}

/// What may go at the end of `text`, where a value is added: read with a space after it, so a
/// name or keyword typed last (`IN`) is a word complete, not one being typed.
fn at_end(text: &str, ds: &Dataset) -> Option<Completion> {
    let text = format!("{} ", text.trim_end());
    query::complete_at(&text, text.chars().count(), ds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn countries() -> Dataset {
        let app = crate::RustGisApp::with_sample();
        let layer = app.session.map().layer_by_name("Countries");
        layer.expect("the sample has Countries").dataset.clone()
    }

    /// The value goes where a value compared with a field goes, a list of one after a bare `IN`,
    /// and the comparison anywhere else; each result is the exact text, and reads on Countries.
    /// Every case is checked before the test fails, so one run shows them all.
    #[test]
    fn a_value_is_added_where_the_clause_takes_it_and_the_clause_still_reads() {
        let ds = countries();
        // (the clause, the field listed, the value listed, what the clause becomes)
        let cases = [
            // A clause starting, or a new one: the comparison, the field quoted if it needs it.
            ("", "continent", "'Asia'", "continent = 'Asia' "),
            ("continent = 'Asia' AND ", "pop_est", "10", "continent = 'Asia' AND pop_est = 10 "),
            ("continent = 'Asia' AND", "pop_est", "10", "continent = 'Asia' AND pop_est = 10 "),
            // After a comparison operator: the value alone, whatever field was listed.
            ("continent = ", "continent", "'Asia'", "continent = 'Asia' "),
            ("continent =", "continent", "'Asia'", "continent = 'Asia' "),
            ("pop_est >=", "pop_est", "10", "pop_est >= 10 "),
            ("continent LIKE", "continent", "'Asia'", "continent LIKE 'Asia' "),
            // A value with a quote is written as given (the list doubles it).
            ("", "name", "'O''Brien'", "name = 'O''Brien' "),
            ("name = ", "name", "'O''Brien'", "name = 'O''Brien' "),
            // In a list: the value, and the list closed.
            ("continent IN (", "continent", "'Asia'", "continent IN ('Asia') "),
            ("continent IN ('Asia',", "continent", "'Africa'", "continent IN ('Asia', 'Africa') "),
            // In a bracket where the value goes: the value, and the bracket closed.
            ("continent = (", "continent", "'Asia'", "continent = ('Asia') "),
            ("pop_est > ((", "pop_est", "10", "pop_est > ((10)) "),
            // After a bare `IN` or `NOT IN`, a list is next.
            ("continent IN", "continent", "'Asia'", "continent IN ('Asia') "),
            ("continent not in ", "continent", "'Asia'", "continent not in ('Asia') "),
            // `BETWEEN` takes both ends: the value for each, to edit, or the upper end after `AND`.
            ("pop_est BETWEEN", "pop_est", "10", "pop_est BETWEEN 10 AND 10 "),
            ("pop_est BETWEEN 1 AND", "pop_est", "10", "pop_est BETWEEN 1 AND 10 "),
            ("(pop_est BETWEEN ", "pop_est", "10", "(pop_est BETWEEN 10 AND 10) "),
            ("pop_est BETWEEN (", "pop_est", "10", "pop_est BETWEEN (10) AND 10 "),
            ("pop_est BETWEEN 1 ", "pop_est", "10", "pop_est BETWEEN 1 AND 10 "),
            // After a whole clause, the comparison joined to it.
            ("pop_est > 1 ", "continent", "'Asia'", "pop_est > 1 AND continent = 'Asia' "),
            ("pop_est > 1", "continent", "'Asia'", "pop_est > 1 AND continent = 'Asia' "),
            ("(pop_est > 1) ", "continent", "'Asia'", "(pop_est > 1) AND continent = 'Asia' "),
            // A text begun is replaced by the value.
            ("continent = 'As", "continent", "'Asia'", "continent = 'Asia' "),
            // The field's button was clicked, then Values: the field is there, so only what
            // follows it is added.
            ("continent ", "continent", "'Asia'", "continent = 'Asia' "),
            ("continent", "continent", "'Asia'", "continent = 'Asia' "),
            ("pop_est > 1 AND continent ", "continent", "'Asia'", "pop_est > 1 AND continent = 'Asia' "),
            ("CONTINENT ", "continent", "'Asia'", "CONTINENT = 'Asia' "),
            // A computed left side has no field to repeat: only the value.
            ("upper(name) = ", "name", "'PARIS'", "upper(name) = 'PARIS' "),
            ("upper(name) LIKE ", "name", "'PARIS'", "upper(name) LIKE 'PARIS' "),
            ("pop_est + 1 BETWEEN ", "pop_est", "10", "pop_est + 1 BETWEEN 10 AND 10 "),
        ];
        let mut wrong = Vec::new();
        for (clause, field, value, expected) in cases {
            let mut text = clause.to_string();
            insert(&mut text, field, value, &ds, None).unwrap();
            if text != expected {
                wrong.push(format!("`{clause}` + {value} wrote `{text}`, not `{expected}`"));
            }
            if let Err(e) = query::select(&text, &ds) {
                wrong.push(format!("`{clause}` + {value} wrote `{text}`, which doesn't read: {e}"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        // A clause that doesn't read at all gets the comparison, and the box says why.
        let mut text = "pop_est > 1 ) AND".to_string();
        insert(&mut text, "continent", "'Asia'", &ds, None).unwrap();
        assert_eq!(text, "pop_est > 1 ) AND continent = 'Asia' ");
    }

    /// With the cursor where the field came from, the value goes there and the text after it is
    /// kept: a gap in the middle of a clause is filled, not the end of it added to.
    #[test]
    fn a_value_goes_at_the_cursor_the_field_came_from() {
        let ds = countries();
        // (the text, the cursor in chars, the field listed, the value, what the text becomes)
        let cases = [
            ("continent =  AND pop_est > 1", 12, "continent", "'Asia'", "continent = 'Asia' AND pop_est > 1"),
            ("continent IN ( AND pop_est > 1", 14, "continent", "'Asia'", "continent IN ('Asia') AND pop_est > 1"),
            ("(continent = ) AND pop_est > 1", 13, "continent", "'Asia'", "(continent = 'Asia') AND pop_est > 1"),
            ("continent IN (, 'Africa')", 14, "continent", "'Asia'", "continent IN ('Asia', 'Africa')"),
            // `BETWEEN`'s upper end is already there.
            ("pop_est BETWEEN  AND 5", 16, "pop_est", "10", "pop_est BETWEEN 10 AND 5"),
            // A text begun is replaced as far as its closing quote, not just up to the cursor.
            ("continent = 'As' AND pop_est > 1", 15, "continent", "'Asia'", "continent = 'Asia' AND pop_est > 1"),
            ("continent = 'Af' AND name = 'x'", 15, "continent", "'Asia'", "continent = 'Asia' AND name = 'x'"),
            // The cursor at the end is the end; past it is the end too.
            ("continent = ", 12, "continent", "'Asia'", "continent = 'Asia' "),
            ("continent = ", 99, "continent", "'Asia'", "continent = 'Asia' "),
        ];
        let mut wrong = Vec::new();
        for (clause, at, field, value, expected) in cases {
            let mut text = clause.to_string();
            insert(&mut text, field, value, &ds, Some(at)).unwrap();
            if text != expected {
                wrong.push(format!("`{clause}` at {at} + {value} wrote `{text}`, not `{expected}`"));
            }
            if let Err(e) = query::select(&text, &ds) {
                wrong.push(format!("`{clause}` at {at} + {value} wrote `{text}`, which doesn't read: {e}"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// Brackets: each left open is closed, however many, but not one that what follows the value
    /// closes, and not for an unfinished clause after it.
    #[test]
    fn every_bracket_left_open_is_closed_and_no_other() {
        let ds = countries();
        let mut text = "((continent = ".to_string();
        insert(&mut text, "continent", "'Asia'", &ds, None).unwrap();
        assert_eq!(text, "((continent = 'Asia')) ");
        assert!(query::select(&text, &ds).is_ok());
        // The list is closed; the comparison after it is unfinished, and stays so.
        let mut text = "continent IN () AND pop_est > ".to_string();
        insert(&mut text, "continent", "'Asia'", &ds, Some(14)).unwrap();
        assert_eq!(text, "continent IN ('Asia') AND pop_est > ");
        // What follows closes some of them, or all, later on: only the rest are closed. A bracket
        // in a text isn't one.
        for (clause, at, expected) in [
            ("((continent = )", 14, "((continent = 'Asia'))"),
            ("(continent =  AND pop_est > 1)", 13, "(continent = 'Asia' AND pop_est > 1)"),
            ("((continent =  AND pop_est > 1)", 14, "((continent = 'Asia') AND pop_est > 1)"),
            ("((continent =  AND name <> ')')", 14, "((continent = 'Asia') AND name <> ')')"),
            ("(name <> '(' AND continent = ", 29, "(name <> '(' AND continent = 'Asia') "),
            ("(continent =  AND !tag)! = 1", 13, "(continent = 'Asia') AND !tag)! = 1"),
            ("(continent =  AND \"a)\" = 1)", 13, "(continent = 'Asia' AND \"a)\" = 1)"),
        ] {
            let mut text = clause.to_string();
            insert(&mut text, "continent", "'Asia'", &ds, Some(at)).unwrap();
            assert_eq!(text, expected, "{clause}");
        }
        // What follows goes on with the value (`* 2`): nothing is finished before it.
        for (clause, at, expected) in
            [("pop_est BETWEEN  * 2 AND 50", 16, "pop_est BETWEEN 10 * 2 AND 50"), ("pop_est IN ( * 2", 12, "pop_est IN (10 * 2")]
        {
            let mut text = clause.to_string();
            insert(&mut text, "pop_est", "10", &ds, Some(at)).unwrap();
            assert_eq!(text, expected, "{clause}");
        }
        // What follows doesn't read (`#`): its bracket is kept, and none added.
        let mut text = "(continent = ) AND name = 'x' #".to_string();
        insert(&mut text, "continent", "'Asia'", &ds, Some(13)).unwrap();
        assert_eq!(text, "(continent = 'Asia') AND name = 'x' #");
    }

    /// The note under the buttons is put in display form once, as a whole: a field named in
    /// right-to-left letters starts the line, which then reads as `i18n::fmt` writes it, its
    /// Latin runs (and the limit, `100`) reversed once, not twice.
    #[test]
    fn the_note_of_values_not_listed_is_put_in_display_form_once() {
        use rustgis_engine::data::{Field, FieldType, GeometryType, Value};
        let mut ds = Dataset::new("t", GeometryType::Point, vec![Field::new("نام", FieldType::Text)]);
        ds.push(None, vec![Value::Text("x".repeat(query::VALUE_CHARS + 1))]);
        let mut form = Form::default();
        form.list(&ds, 0, 1);
        let logical = "نام has no values to list. The value of 1 feature isn't listed: longer than 100 characters, with a line break or other control character, or too large";
        assert_eq!(form.said.as_deref(), Some(crate::i18n::display(logical).as_str()));
        assert_ne!(crate::i18n::display(logical), logical, "the line is laid out back to front");
    }

    /// A name begun in `!` at the cursor is replaced to its closing `!`, which has no doubling.
    #[test]
    fn a_name_begun_in_bangs_is_replaced_to_its_close() {
        let ds = countries();
        for (clause, at, expected) in [
            ("continent = !Af!", 15, "continent = 'Asia' "),
            ("continent = !Af! AND pop_est > 1", 15, "continent = 'Asia' AND pop_est > 1"),
        ] {
            let mut text = clause.to_string();
            insert(&mut text, "continent", "'Asia'", &ds, Some(at)).unwrap();
            assert_eq!(text, expected, "{clause}");
        }
    }

    /// What would stand beside a value or a field already there, after an operator, or in a quote
    /// that holds the rest is not written: the draft stays, and the box says why.
    #[test]
    fn a_value_is_not_written_beside_another() {
        let ds = countries();
        // (the text, the cursor in chars, the field)
        for (clause, at, field) in [
            ("continent = 'Africa' AND pop_est > 1", Some(12), "continent"),
            ("continent =  'Africa'", Some(12), "continent"),
            ("pop_est > 5", Some(10), "pop_est"),
            ("continent = name", Some(12), "continent"),
            ("pop_est > -5", Some(10), "pop_est"),
            ("pop_est > +5", Some(10), "pop_est"),
            ("continent = !name!", Some(12), "continent"),
            ("continent =  'Afr", Some(12), "continent"),
            // An operator after the cursor, where more than the value would go.
            ("continent = 'Old'", Some(9), "continent"),
            ("continent IN ('Old')", Some(9), "continent"),
            ("continent =  != 'Old'", Some(12), "continent"),
            // A value after the cursor, though something later doesn't read.
            ("continent =  'Africa' AND pop_est > #", Some(12), "continent"),
            ("continent =  'Africa'#", Some(12), "continent"),
            // Nothing above refuses it, but the clause would stop reading there.
            ("(pop_est > 1", None, "continent"),
            // A quote never closed holds the rest of the clause.
            ("continent = 'As AND pop_est > 1", Some(15), "continent"),
            // The field's button clicked after an operator: the field is the value already.
            ("pop_est > continent ", None, "continent"),
            ("name = continent", None, "continent"),
        ] {
            let mut text = clause.to_string();
            let said = insert(&mut text, field, "'Asia'", &ds, at).expect_err(clause);
            assert_eq!(text, clause, "left as it was");
            assert!(said.starts_with("Not added"), "{said}");
        }
        // Words that come after a value leave the gap open to fill.
        for (clause, at) in [("continent =  AND pop_est > 1", 12), ("continent =  OR pop_est > 1", 12)] {
            let mut text = clause.to_string();
            insert(&mut text, "continent", "'Asia'", &ds, Some(at)).unwrap();
            assert!(query::select(&text, &ds).is_ok(), "{text}");
        }
    }

    /// The list is held while it stays open, but a pick closes it: opened again, even in the next
    /// frame, it reads the rows as they are then.
    #[test]
    fn a_pick_ends_the_list_held() {
        let ds = countries();
        let field = ds.fields.iter().position(|f| f.name == "continent").unwrap();
        // Rows of their own: one more feature.
        let mut other = ds.clone();
        other.push(None, vec![rustgis_engine::data::Value::Null; ds.fields.len()]);
        assert!(!other.features.allocation_key().ptr_eq(&ds.features.allocation_key()));
        let mut form = Form::default();
        form.choose("continent");
        let _ = form.list(&ds, field, 1);
        let _ = form.list(&other, field, 2);
        assert_eq!(form.reads, 1, "held while open, whatever the rows");
        let mut text = String::new();
        form.accept(&mut text, "continent", "'Asia'", &ds);
        let _ = form.list(&other, field, 3);
        assert_eq!(form.reads, 2, "opened again in the next frame: read again");
    }

    /// The cursor remembered is the one the field came from, while the text is as it was: typed in
    /// since, the cursor says nothing and the value goes at the end; a field clicked is the end.
    #[test]
    fn the_cursor_is_kept_only_while_the_text_is_as_it_was() {
        let ds = countries();
        let text = "continent =  AND pop_est > 1";
        let mut form = Form { cursor: Some((12, text.to_string())), ..Form::default() };
        let mut now = text.to_string();
        form.accept(&mut now, "continent", "'Asia'", &ds);
        assert_eq!(now, "continent = 'Asia' AND pop_est > 1", "at the cursor");
        assert!(form.cursor.is_none(), "used");
        // Typed since: the cursor is not where it was.
        form.cursor = Some((12, text.to_string()));
        let mut typed = format!("{text} OR ");
        form.accept(&mut typed, "continent", "'Asia'", &ds);
        assert_eq!(typed, "continent =  AND pop_est > 1 OR continent = 'Asia' ", "at the end");
        // A field clicked leaves the cursor behind.
        form.cursor = Some((12, text.to_string()));
        form.choose("name");
        assert!(form.cursor.is_none());
    }

    /// A value with a `%` or `_` after `LIKE` would match more than itself: nothing is written, and
    /// it says why. Anywhere else such a value is written as any other.
    #[test]
    fn a_wildcard_value_is_not_written_after_like() {
        let ds = countries();
        for (clause, value) in [("name LIKE ", "'a_b'"), ("name NOT LIKE ", "'100%'"), ("name ILIKE", "'x_'"), ("name LIKE 'a", "'a_b'")] {
            let mut text = clause.to_string();
            let said = insert(&mut text, "name", value, &ds, None).expect_err(clause);
            assert_eq!(text, clause, "the draft is left as it was");
            assert!(said.contains(value) && said.contains('%') && said.contains('='), "{said}");
        }
        for (clause, value, expected) in [
            ("name = ", "'a_b'", "name = 'a_b' "),
            ("name IN (", "'a_b'", "name IN ('a_b') "),
            ("name LIKE ", "'abc'", "name LIKE 'abc' "),
            ("", "'a_b'", "name = 'a_b' "),
        ] {
            let mut text = clause.to_string();
            insert(&mut text, "name", value, &ds, None).unwrap();
            assert_eq!(text, expected);
        }
    }

    /// An insertion that would take the draft past `query::MAX_LEN` leaves it as it is and says so;
    /// one that reaches the limit exactly is written.
    #[test]
    fn a_value_that_would_pass_the_length_limit_is_not_written() {
        let ds = countries();
        let fits = format!("{}continent = ", " ".repeat(query::MAX_LEN - 19));
        let mut text = fits.clone();
        insert(&mut text, "continent", "'Asia'", &ds, None).unwrap();
        assert_eq!(text.chars().count(), query::MAX_LEN);
        assert!(text.ends_with("continent = 'Asia' "));
        let full = format!(" {fits}");
        let mut text = full.clone();
        let said = insert(&mut text, "continent", "'Asia'", &ds, None).expect_err("one char too many");
        assert_eq!(text, full, "left as it was");
        assert!(said.contains(&query::MAX_LEN.to_string()), "{said}");
        // The same with the cursor in the middle.
        let mut text = format!("{full} AND x");
        assert!(insert(&mut text, "continent", "'Asia'", &ds, Some(full.chars().count())).is_err());
    }

    /// The field a value typed at the cursor is compared with (what `follow_box` lists) is the
    /// one `complete_at` reads there, wherever in the clause; none outside a comparison.
    #[test]
    fn the_field_listed_is_the_one_the_value_at_the_cursor_is_compared_with() {
        let ds = countries();
        let of = |text: &str, cursor: usize| {
            query::complete_at(text, cursor, &ds).and_then(|c| c.field).and_then(|i| ds.fields.get(i)).map(|f| f.name.clone())
        };
        assert_eq!(of("continent = ", 12).as_deref(), Some("continent"));
        assert_eq!(of("pop_est > 1 AND continent IN ('a', ", 36).as_deref(), Some("continent"));
        assert_eq!(of("continent = 'x' AND pop_est > 1", 12).as_deref(), Some("continent"), "the text before the cursor only");
        assert_eq!(of("continent", 9), None, "no operator yet");
    }
}
