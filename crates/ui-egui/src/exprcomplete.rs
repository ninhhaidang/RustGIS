//! A box where an expression is written, with suggestions as it is typed (#1240): the shared
//! list (`completion`) under the cursor, holding what the language takes there as the parser
//! reads it (`query::complete_at`): the dataset's fields, its functions, variables and keywords,
//! each with its help beside, and where a value compared with a field goes, that field's values.
//! It opens once a character of the word is typed, or with Ctrl+Space. Nothing is listed here by
//! hand: what the language learns is offered too. A `(`, `'` or `"` typed closes itself, as the
//! Script pane's braces do (`completion::pairs`).

use egui::text::{CCursor, CCursorRange};
use egui::widgets::text_edit::{TextEditOutput, TextEditState};
use egui::{Id, Ui};
use rustgis_engine::data::Dataset;
use rustgis_engine::data::query::{self, Call, Completion, FuncSpec, Kw, Place, VARS};

use crate::completion::{Asked, Before, Keyed, List, Offered, Side, Tip, Worked, byte_at, char_at, pairs};

/// The most fields the list holds (typing narrows them; it scrolls): a table of hundreds of
/// columns mustn't draw hundreds of rows. The functions, variables and keywords after them are
/// the language's, a few dozen, and always all there.
const MAX_FIELDS: usize = 50;

/// The most values of a field the list holds, as of fields: typing narrows them.
const MAX_VALUES: usize = 50;

/// How many lines a call's signature is first guessed to take (the signature, and the
/// description on up to three): where it goes before it's drawn once.
pub(crate) const TIP_ROWS: usize = 4;

/// What an item stands for.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    /// A field, by its name: what's written is what was offered, whatever the data does
    /// meanwhile.
    Field(String),
    Function(FuncSpec),
    /// A variable (its entry in `VARS`) by the name it was matched by.
    Variable(usize, &'static str),
    Keyword(Kw),
    /// A value of a field, as the expression writes it (`'Paris'`, `10`), the field's name, and
    /// what it was read from.
    Value(String, String, ValuesRead),
}

/// Which features a field's values were read from, for the help to say when it isn't all of the
/// layer's (`query::field_values`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct ValuesRead {
    read: usize,
    of: usize,
    /// A streamed layer's: its dataset holds the features read for the view.
    view: bool,
}

impl ValuesRead {
    /// What the help says of where the values come from; None when they are all the layer's.
    fn note(self) -> Option<String> {
        let (read, of) = (self.read.to_string(), self.of.to_string());
        let args = [("read", read.as_str()), ("of", of.as_str())];
        match (self.view, self.read < self.of) {
            (false, false) => None,
            (false, true) => Some(crate::i18n::fmt("Values of {read} of {of} features, evenly spread", &args)),
            (true, false) => Some(crate::i18n::fmt("Values of the {of} features read for the view", &args)),
            (true, true) => Some(crate::i18n::fmt("Values of {read} of the {of} features read for the view, evenly spread", &args)),
        }
    }
}

/// What to offer at a cursor: the word it replaces (chars), the quote a field name was begun
/// with, and the items, as the list shows them and as what they stand for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Offer {
    start: usize,
    end: usize,
    quote: Option<char>,
    items: Vec<String>,
    of: Vec<Item>,
    /// The fields it was worked out over, as their rows say them (`fields_key`): the same items
    /// over a field whose type or alias changed are another offer, whose rows say something else.
    fields: u64,
}

/// What a dataset's fields say in a list's rows and help: each one's name, type and alias. An
/// offer worked out over them is kept while it's the same (`Worked`), and its rows' descriptions
/// while the offer is (`completion::List`).
pub(crate) fn fields_key(ds: &Dataset) -> u64 {
    egui::util::hash(ds.fields.iter().map(|f| (f.name.as_str(), f.ty.label(), f.alias.as_deref())).collect::<Vec<_>>())
}

impl Offered for Offer {
    fn items(&self) -> &[String] {
        &self.items
    }

    fn end(&self) -> usize {
        self.end
    }
}

impl Offer {
    /// Where the word it replaces starts (a char index of the expression).
    pub(crate) fn start(&self) -> usize {
        self.start
    }
}

/// A field as an expression names it: as it is when it reads as a plain name, else in double
/// quotes (a space, a leading digit, a keyword of the language).
pub(crate) fn field_ref(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    let keyword = Kw::ALL.iter().any(|k| k.word().eq_ignore_ascii_case(name));
    if plain && !keyword { name.to_string() } else { format!("\"{}\"", name.replace('"', "\"\"")) }
}

/// Whether `name` starts with `prefix`, letter case aside.
fn starts_with(name: &str, prefix: &str) -> bool {
    name.to_lowercase().starts_with(&prefix.to_lowercase())
}

/// What to offer in `text`, an expression over `ds`, where `c` (`complete_at`) says what goes:
/// the values of the field compared with there first (`values`), then what goes there that
/// starts with the word typed (the word typed whole isn't offered again).
/// Only once a character of the word is typed (a quote begun counts), unless `asked`
/// (Ctrl+Space).
pub(crate) fn offer_from(c: &Completion, text: &str, ds: &Dataset, asked: bool) -> Option<Offer> {
    let mut of = values(c, text, ds, asked);
    if (c.place == Place::Text && of.is_empty()) || (c.word.is_empty() && !asked) {
        return None;
    }
    let p = c.prefix.as_str();
    let fits = |name: &str| starts_with(name, p) && !name.eq_ignore_ascii_case(p);
    if c.operand {
        // A field another one shadows (`name` after `Name`: the expression reads the first, letter
        // case aside) isn't offered: written, it would read the other. Looked for among the
        // fields that fit only, as each look goes through the fields.
        let mut fields: Vec<&str> = ds
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| fits(&f.name))
            .filter(|(i, f)| ds.field_index(&f.name) == Some(*i))
            .map(|(_, f)| f.name.as_str())
            .collect();
        fields.sort_by_key(|n| n.to_lowercase());
        fields.truncate(MAX_FIELDS);
        of.extend(fields.into_iter().map(|n| Item::Field(n.to_string())));
    }
    if c.place == Place::Code {
        if c.operand {
            of.extend(query::functions().filter(|f| fits(f.name)).map(Item::Function));
            // A variable once, by the first of its names that fits.
            for (i, v) in VARS.iter().enumerate() {
                if let Some(name) = v.names.iter().find(|n| fits(n)) {
                    of.push(Item::Variable(i, name));
                }
            }
        }
        of.extend(c.keywords.iter().filter(|k| fits(k.word())).map(|k| Item::Keyword(*k)));
    }
    if of.is_empty() {
        return None;
    }
    let items = of.iter().map(shown).collect();
    let quote = (c.place != Place::Code).then(|| text.chars().nth(c.word.start)).flatten();
    Some(Offer { start: c.word.start, end: c.word.end, quote, items, of, fields: fields_key(ds) })
}

/// The values of the field a value typed at `c` is compared with (`name = '▮`, `pop IN (▮`),
/// as the expression writes them, first in the list: inside a text, a text, date or date-time
/// field's that start with what's typed (letter case aside, and not what's typed whole); where a
/// value goes with no word typed, when the list is asked for (Ctrl+Space), any field's (a number
/// as it is). None in a field's name, nor of a value compared with no field (`upper(name) = '`).
/// Read again whenever the text, the cursor or the rows change, from a bounded sample
/// (`query::VALUES_READ`).
fn values(c: &Completion, text: &str, ds: &Dataset, asked: bool) -> Vec<Item> {
    use rustgis_engine::data::FieldType;
    let Some((i, field)) = c.field.and_then(|i| Some((i, ds.fields.get(i)?))) else { return Vec::new() };
    let quoted = matches!(field.ty, FieldType::Text | FieldType::Date | FieldType::DateTime);
    let wanted = match c.place {
        Place::Text => quoted,
        // Not just before a value already written (`pop = ▮1.5`, `name = ▮'Lima'`): only its
        // start would be replaced (`3.5`, `'Paris''Lima'`).
        Place::Code => {
            let next = text.chars().nth(c.word.end);
            c.operand && c.word.is_empty() && asked && next.is_none_or(|n| n.is_whitespace() || matches!(n, ')' | ','))
        }
        Place::Field => false,
    };
    if !wanted {
        return Vec::new();
    }
    let got = query::field_values(ds, i, query::VALUES_READ);
    let read = ValuesRead { read: got.read, of: got.of, view: ds.windowed };
    let lower = c.prefix.to_lowercase();
    got.values
        .into_iter()
        // Inside a text only a value written in quotes (a number in a text field of a hostile
        // file would be written bare over the quote begun).
        .filter(|v| {
            let typed = v.written.starts_with('\'') && v.text.to_lowercase().starts_with(&lower) && v.text != c.prefix;
            c.place == Place::Code || typed
        })
        // In a `LIKE` pattern a `%` or `_` matches more than itself, and the language has no
        // escape: such a value would pick others too.
        .filter(|v| !(c.pattern && v.text.contains(['%', '_'])))
        .take(MAX_VALUES)
        .map(|v| Item::Value(v.written, field.name.clone(), read))
        .collect()
}

/// An item as the list shows it, each apart from the others, as it is picked by what it shows:
/// a field as the expression names it (`"NOT"`, `"$area"` and `"Pop Est"` in quotes, so a field
/// never reads as a keyword or variable), a function with its brackets.
fn shown(item: &Item) -> String {
    match item {
        Item::Field(name) => field_ref(name),
        Item::Function(f) => format!("{}()", f.name),
        Item::Variable(_, name) => name.to_string(),
        Item::Keyword(k) => k.word().to_string(),
        Item::Value(written, ..) => written.clone(),
    }
}

/// The box's state between frames.
#[derive(Clone, Default)]
struct State {
    list: List<Offer>,
    /// What was worked out to offer, and the call the cursor is in, for which cursor, text (its
    /// hash) and source (its fields' names' hash: another layer's offer others), whether it was
    /// asked for and whether an item was just accepted there. Worked out again when [`rows`]
    /// changes.
    ///
    /// [`rows`]: Self::rows
    worked: Option<Worked<(u64, bool, bool), (Option<Offer>, Option<Call>)>>,
    /// The signature of the call the cursor is in, floating by it while the list is closed, for
    /// the call's `(` (where it starts).
    tip: Tip<usize>,
    /// Where Ctrl+Space asked for the list: it shows there with no word typed.
    asked: Asked,
    /// Where an item was just accepted (the text's hash and the cursor): nothing is offered there
    /// (`pop` taken mustn't offer `"Pop Est"`, for Enter to take next), until the text or the
    /// cursor moves on, or Ctrl+Space asks.
    accepted: Option<(u64, usize)>,
    /// The rows [`worked`](Self::worked) read the values from, held: an edit then moves them
    /// (`Arc::make_mut` doesn't change rows in place while one is held), as do a streamed
    /// layer's view read and another layer, and the values offered are read again.
    rows: Option<std::sync::Weak<Vec<rustgis_engine::data::Feature>>>,
}

fn load(ui: &Ui, id: Id) -> State {
    ui.data(|d| d.get_temp::<State>(id.with("expr_complete"))).unwrap_or_default()
}

fn store(ui: &Ui, id: Id, state: State) {
    ui.data_mut(|d| d.insert_temp(id.with("expr_complete"), state));
}

/// Whether the [`expr_edit`] box `id` shows its suggestion list: the word there is still being
/// chosen (`pop_`, with `pop_est` offered, isn't a missing field yet).
pub(crate) fn offering(ui: &Ui, id: Id) -> bool {
    // Read in place: `load` would copy the whole state (the offer's lists) for one flag.
    ui.data_mut(|d| d.get_temp_mut_or_default::<State>(id.with("expr_complete")).list.is_open())
}

/// A single-line box for an expression over `ds` (none: no suggestions nor signature, as both
/// come from reading the expression against its fields), its id `id`, styled by
/// `style` (hint, font, width…), with the suggestion list, and the signature of the call the
/// cursor is in floating over it while no list shows, so what goes under the box (its error, its
/// help) stays in view; under it only when there's no room over it. Returns the box's response.
pub(crate) fn expr_edit(
    ui: &mut Ui,
    id: Id,
    text: &mut String,
    ds: Option<&Dataset>,
    style: impl for<'t> FnOnce(egui::TextEdit<'t>) -> egui::TextEdit<'t>,
) -> egui::Response {
    expr_edit_lines(ui, id, text, ds, |text| style(egui::TextEdit::singleline(text)))
}

/// A box of several lines for an expression, as [`expr_edit`] is one of a line: `build` makes the
/// box from the text (`TextEdit::multiline`, a dialog's `text_box`), styled. The list shows under
/// the cursor's line and the signature over the line of its call's `(`, so what goes under the box
/// stays in view. While the list is open, Enter and Tab take its item, not a new line or the
/// dialog's button, and Escape closes it, not the dialog or the box.
pub(crate) fn expr_edit_lines(
    ui: &mut Ui,
    id: Id,
    text: &mut String,
    ds: Option<&Dataset>,
    build: impl for<'t> FnOnce(&'t mut String) -> egui::TextEdit<'t>,
) -> egui::Response {
    let mut state = load(ui, id);
    // The list's keys, taken before the box sees them: Ctrl+Space asks for the list.
    if state.asked.keys(ui, id, text) {
        state.list.reopen();
    }
    if state.worked.as_ref().is_some_and(|w| w.text != egui::util::hash(text.as_str())) {
        state.list.forget();
    }
    let picked = state.list.keys(ui, id);
    if let Some((offer, item)) = &picked {
        state.accepted = accept(ui, id, text, offer, item);
        state.worked = None;
    }
    // No list open: Escape closes the signature.
    if !state.list.is_open() {
        state.tip.keys(ui, id);
    }
    // What was there before the box takes the keys, for a `(` or quote typed to close itself.
    let before = crate::completion::before(ui, id, text);
    let open = state.list.is_open();
    let escape = open || state.tip.is_shown();
    let filter = egui::EventFilter { tab: open, horizontal_arrows: true, vertical_arrows: true, escape };
    let out: TextEditOutput = build(text).id(id).event_filter(filter).show(ui);
    let visible = out.text_clip_rect;
    let hash = egui::util::hash(text.as_str());
    let rows = ds.map(|d| d.features.allocation_key());
    let same = match (&state.rows, &rows) {
        (Some(was), Some(is)) => was.ptr_eq(is),
        (was, is) => was.is_none() && is.is_none(),
    };
    if !same {
        state.worked = None;
    }
    state.rows = rows;
    let source = ds.map_or(0, fields_key);
    let worked = &mut state.worked;
    let asked = &mut state.asked;
    let accepted = &mut state.accepted;
    let work = |at: Option<usize>| {
        let here = asked.here(hash, at);
        // Just accepted here, and not asked since.
        if at.map(|at| (hash, at)) != *accepted {
            *accepted = None;
        }
        let quiet = accepted.is_some() && !here;
        let with = (source, here, quiet);
        let (offer, _) = Worked::get(worked, at, hash, &with).unwrap_or_else(|| {
            let c = at.zip(ds).and_then(|(at, ds)| Some((query::complete_at(text.as_str(), at, ds)?, ds)));
            let offer = c.as_ref().filter(|_| !quiet).and_then(|(c, ds)| offer_from(c, text.as_str(), ds, here));
            let value = (offer, c.and_then(|(c, _)| c.call));
            *worked = Some(Worked { at, text: hash, with, value: value.clone() });
            value
        });
        offer
    };
    let icon = |offer: &Offer, item: &str| names_field(offer, item).then_some("field");
    let describe = |offer: &Offer, item: &str| describe_item(offer, item, ds);
    let draw = |ui: &mut Ui, offer: &Offer, item: &str, width: f32| draw_item(ui, offer, item, ds, width);
    // What a row says is read from the offer, which holds its fields (`fields_key`): no other key.
    let clicked = state.list.show(ui, id.with("expr_list"), &out, visible, work, icon, |_| 0, describe, draw);
    // The call the cursor is in, kept with the offer `work` worked out for the list this frame
    // (it works one out for the cursor whenever the box has the focus). Shown while no list is open: one open only hides
    // it, so what Escape closed stays closed until the cursor leaves the call.
    let focused = out.response.response.has_focus() && ui.input(|i| i.focused);
    let cursor = out.cursor_range.filter(|r| focused && r.is_empty()).map(|r| r.primary.index.0);
    let call = state.worked.as_ref().filter(|w| cursor.is_some() && w.at == cursor && w.text == hash).and_then(|w| w.value.1);
    let at = cursor.filter(|_| !state.list.is_open());
    // Kept by its `(`, and anchored there; over the line, as under the box are its error and help.
    let about = call.map(|c| (c.open, c.open));
    state.tip.show(ui, id.with("expr_tip"), &out, visible, at, about, TIP_ROWS, Side::Above, |ui, width| {
        if let Some(call) = &call {
            draw_call(ui, call, width);
        }
    });
    store(ui, id, state);
    if let Some((offer, item)) = clicked {
        let accepted = accept(ui, id, text, &offer, &item);
        let mut state = load(ui, id);
        state.accepted = accepted;
        state.worked = None;
        store(ui, id, state);
    } else {
        pairs(ui, before, &out, text, &['(', ')', '\'', '"'], pair);
    }
    out.response.response
}

/// What may follow a `(` or quote that closes itself: nothing, a space, a `)` or a `,`.
fn closes(c: char) -> bool {
    c.is_whitespace() || matches!(c, ')' | ',')
}

/// What may come before a quote that closes itself, a space aside: where a value can start.
const OPENS: &[char] = &['(', ',', '=', '<', '>', '+', '-', '*', '/', '%', '^', '|'];

/// The quote char `at` of `text` is inside of: `'` a text's, `"` or `!` a field name's; None
/// in the code. A doubled `'` or `"` in one is part of it, as the expression reads it (it ends
/// and starts again), and `!=` is an operator, not a name's `!`.
fn quoted_at(text: &str, at: usize) -> Option<char> {
    let cs: Vec<char> = text.chars().collect();
    let mut quote = None;
    for (i, &c) in cs.iter().enumerate().take(at) {
        quote = match quote {
            None if c == '\'' || c == '"' => Some(c),
            None if c == '!' && cs.get(i + 1) != Some(&'=') => Some('!'),
            Some(q) if c == q => None,
            q => q,
        };
    }
    quote
}

/// The expression once the box's edit is followed through (`completion::pairs`): the edit read
/// from `text` as the box left it, what was there `before`, and the cursor `now` (a char index).
/// A `(` typed closes itself (`(|)`) when nothing, a space, a `)` or a `,` follows it; a `'` or
/// `"` too (`'|'`), when it also comes where a value can start (the start, a space, `(`, `,` or
/// an operator); a `)` typed right before a `)` steps over it rather than add one, and so does a
/// quote typed before the one that ends its text or name; Backspace in an empty `(|)`, `'|'` or
/// `"|"` deletes both. Not in a text or a field's name, nor past the longest expression read
/// (`MAX_LEN`, as the box takes no more), and only for the keys that make those edits (`keyed`).
/// The cursor stays where the box put it. None to leave the edit as it is.
fn pair(text: &str, before: Before, now: usize, keyed: Keyed) -> Option<String> {
    let Before { at, prev, len } = before;
    let plain = |i: usize| quoted_at(text, i).is_none();
    let (typed, next) = (char_at(text, at), char_at(text, now));
    let typed_one = keyed == Keyed::Pair && text.len() == len + 1 && now == at + 1;
    let one_deleted = keyed == Keyed::Backspace && text.len() + 1 == len && now + 1 == at && plain(now);
    let cut = byte_at(text, if one_deleted { now } else { at })?;
    let (b, rest) = (text.get(..cut)?, text.get(cut..)?);
    let opens = |c: char| c.is_whitespace() || OPENS.contains(&c);
    let new = match (typed, next) {
        (Some('('), next) if typed_one && plain(at) && next.is_none_or(closes) => Some(format!("{b}(){}", rest.get(1..)?)),
        (Some(q @ ('\'' | '"')), next) if typed_one && plain(at) && next.is_none_or(closes) && prev.is_none_or(opens) => {
            Some(format!("{b}{q}{q}{}", rest.get(1..)?))
        }
        // The `)` typed goes: the cursor, after it, is after the one that was there.
        (Some(')'), Some(')')) if typed_one && plain(at) => Some(format!("{b}{}", rest.get(1..)?)),
        // So does a quote that ends its text or name (in it before, out of it after).
        (Some(q @ ('\'' | '"')), Some(n)) if typed_one && q == n && quoted_at(text, at) == Some(q) && plain(now) => {
            Some(format!("{b}{}", rest.get(1..)?))
        }
        // The closer that followed the deleted opener, now at the cursor.
        (_, Some(next)) if one_deleted && matches!((prev, next), (Some('('), ')') | (Some('\''), '\'') | (Some('"'), '"')) => {
            Some(format!("{b}{}", rest.get(1..)?))
        }
        _ => None,
    };
    new.filter(|t| t.chars().count() <= query::MAX_LEN)
}

/// The span (bytes) of argument `arg` (from 0) in a function's `signature` (`round(x[, places])`):
/// its name, or the `…` that stands for the rest (`concat(a, b, …)`); None past the last.
fn arg_range(signature: &str, arg: usize) -> Option<std::ops::Range<usize>> {
    let open = signature.find('(')? + 1;
    let mut names: Vec<std::ops::Range<usize>> = Vec::new();
    let mut name: Option<usize> = None;
    for (i, c) in signature.get(open..)?.char_indices().map(|(i, c)| (open + i, c)) {
        let word = c.is_alphanumeric() || c == '_';
        match (name, word) {
            (None, true) => name = Some(i),
            (Some(start), false) => {
                names.push(start..i);
                name = None;
            }
            _ => {}
        }
        if c == '…' {
            names.push(i..i + c.len_utf8());
        }
    }
    // A name that ends the signature (none does: each ends in `)`).
    if let Some(start) = name {
        names.push(start..signature.len());
    }
    let rest = names.last().filter(|r| signature.get((*r).clone()) == Some("…"));
    names.get(arg).or(rest).cloned()
}

/// A function's `signature` coloured as the list's help colours one (`scriptcolor::expr_job`),
/// the argument at the byte range `arg` marked in `mark`'s background and text colours. Laid out in
/// three parts, so the argument is a part of its own: a layout joins neighbours of one format
/// (`places` with the space before it).
fn signature_job(
    signature: &str,
    arg: Option<std::ops::Range<usize>>,
    palette: crate::scriptcolor::Palette,
    font: &egui::FontId,
    mark: (egui::Color32, egui::Color32),
) -> egui::text::LayoutJob {
    let parts = arg.and_then(|r| Some((signature.get(..r.start)?, signature.get(r.clone())?, signature.get(r.end..)?)));
    let Some((before, marked, after)) = parts else { return crate::scriptcolor::expr_job(signature, palette, font) };
    let mut job = egui::text::LayoutJob::default();
    for (text, marked) in [(before, false), (marked, true), (after, false)] {
        let part = crate::scriptcolor::expr_job(text, palette, font);
        for s in &part.sections {
            let mut format = s.format.clone();
            if marked {
                (format.background, format.color) = mark;
            }
            job.append(part.text.get(s.byte_range.start.0..s.byte_range.end.0).unwrap_or_default(), 0.0, format);
        }
    }
    job
}

/// The signature of the function the cursor is in a call of, coloured as the list's help colours
/// one, the argument the cursor is in marked, and what the function gives.
pub(crate) fn draw_call(ui: &mut Ui, call: &Call, width: f32) {
    let palette = crate::scriptcolor::Palette::of(&crate::theme::Tokens::get(ui.ctx()));
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let mark = (ui.visuals().selection.bg_fill, ui.visuals().selection.stroke.color);
    let mut job = signature_job(call.spec.signature, arg_range(call.spec.signature, call.arg), palette, &mono, mark);
    job.wrap.max_width = width;
    ui.vertical(|ui| {
        ui.set_max_width(width);
        ui.label(job);
        ui.label(tl!(call.spec.description));
    });
}

/// What `item` (as shown) of `offer` stands for.
fn find<'a>(offer: &'a Offer, item: &str) -> Option<&'a Item> {
    offer.items.iter().position(|i| i == item).and_then(|i| offer.of.get(i))
}

/// Whether `item` of `offer` is a field, shown after a field's icon.
pub(crate) fn names_field(offer: &Offer, item: &str) -> bool {
    matches!(find(offer, item), Some(Item::Field(_)))
}

/// The help of `item` of `offer` beside the list (`draw_help`), in the help's box: the expression
/// box's list and the Script's show the same.
pub(crate) fn draw_item(ui: &mut Ui, offer: &Offer, item: &str, ds: Option<&Dataset>, width: f32) {
    if let Some(of) = find(offer, item) {
        draw_help(ui, of, ds, width);
    }
}

/// What the help of `item` of `offer` says, for a screen reader (`describe`): the expression
/// box's list and the Script's say the same.
pub(crate) fn describe_item(offer: &Offer, item: &str, ds: Option<&Dataset>) -> Option<String> {
    find(offer, item).map(|of| spoken_item(of, ds))
}

/// What the help beside the list says about `item` (`draw_help`), as text for a screen reader:
/// the signature and what a function gives (the example left out, as code to read); a field's
/// type and alias; what a variable reads or a keyword does; whose value a value is.
fn spoken_item(item: &Item, ds: Option<&Dataset>) -> String {
    match item {
        Item::Function(f) => crate::completion::spoken(f.signature, Some(tl!(f.description))),
        Item::Field(name) => {
            let f = ds.and_then(|d| d.fields.iter().find(|f| f.name == *name));
            let ty = f.map(|f| tl!(f.ty.label()).to_string());
            let alias = f.and_then(|f| f.alias.clone()).filter(|a| !a.is_empty());
            match (ty, alias) {
                (Some(ty), alias) => crate::completion::spoken(&ty, alias.as_deref()),
                (None, alias) => alias.unwrap_or_default(),
            }
        }
        Item::Variable(i, _) => VARS.get(*i).map(|v| tl!(v.description).to_string()).unwrap_or_default(),
        Item::Keyword(k) => tl!(k.description()).to_string(),
        Item::Value(_, field, read) => {
            let of = crate::i18n::fmt("A value of {field}", &[("field", &field_ref(field))]);
            crate::completion::spoken(&of, read.note().as_deref())
        }
    }
}

/// The help beside the list: a function's signature, coloured as the Script colours one, what
/// it gives and an example; a field's type and alias; what a variable reads or a keyword does.
fn draw_help(ui: &mut Ui, item: &Item, ds: Option<&Dataset>, width: f32) {
    let palette = crate::scriptcolor::Palette::of(&crate::theme::Tokens::get(ui.ctx()));
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let code = |ui: &mut Ui, text: &str| {
        let mut job = crate::scriptcolor::expr_job(text, palette, &mono);
        job.wrap.max_width = width;
        ui.label(job);
    };
    ui.vertical(|ui| {
        ui.set_max_width(width);
        match item {
            Item::Function(f) => {
                code(ui, f.signature);
                ui.label(tl!(f.description));
                ui.label(egui::RichText::new(f.example).monospace().small().weak());
            }
            Item::Field(name) => {
                code(ui, &field_ref(name));
                if let Some(f) = ds.and_then(|d| d.fields.iter().find(|f| f.name == *name)) {
                    ui.label(tl!(f.ty.label()));
                    if let Some(alias) = f.alias.as_deref().filter(|a| !a.is_empty()) {
                        ui.label(egui::RichText::new(alias).small());
                    }
                }
            }
            Item::Variable(i, name) => {
                code(ui, name);
                if let Some(v) = VARS.get(*i) {
                    ui.label(tl!(v.description));
                }
            }
            Item::Keyword(k) => {
                code(ui, k.word());
                ui.label(tl!(k.description()));
            }
            Item::Value(written, field, read) => {
                code(ui, written);
                ui.label(crate::i18n::fmt("A value of {field}", &[("field", &field_ref(field))]));
                if let Some(note) = read.note() {
                    ui.label(egui::RichText::new(note).small().weak());
                }
            }
        }
    });
}

/// What a menu beside an expression lists: the language's functions, or its variables.
#[derive(Clone, Copy)]
pub(crate) enum Menu {
    Functions,
    Variables,
}

/// The entries of `menu`, each as the list shows it: the language's own catalogs, as the parser
/// reads them (one added to the language is listed with no change). Worked out once.
fn menu_items(menu: Menu) -> &'static [(String, Item)] {
    static FUNCTIONS: std::sync::OnceLock<Vec<(String, Item)>> = std::sync::OnceLock::new();
    static VARIABLES: std::sync::OnceLock<Vec<(String, Item)>> = std::sync::OnceLock::new();
    let (cell, items): (_, fn() -> Vec<Item>) = match menu {
        Menu::Functions => (&FUNCTIONS, || query::functions().map(Item::Function).collect()),
        Menu::Variables => {
            (&VARIABLES, || VARS.iter().enumerate().filter_map(|(i, v)| Some(Item::Variable(i, v.names.first().copied()?))).collect())
        }
    };
    cell.get_or_init(|| items().into_iter().map(|i| (shown(&i), i)).collect())
}

/// A button `label` that opens the list of `menu`, its entries shown as the suggestion list shows
/// them (`round()`, `$area`) and with its help, in the list every box's suggestions show in
/// (`completion::menu`). Returns what to add for the one accepted: a function open (`round(`),
/// unless it takes no arguments (`pi()`); a variable as it is.
pub(crate) fn menu(ui: &mut Ui, label: impl Into<egui::WidgetText>, menu: Menu) -> Option<String> {
    let catalog = menu_items(menu);
    let find = |item: &str| catalog.iter().find(|(s, _)| s == item).map(|(_, i)| i);
    let help = |ui: &mut Ui, item: &str, width: f32| {
        if let Some(of) = find(item) {
            draw_help(ui, of, None, width);
        }
    };
    let describe = |i: usize| catalog.get(i).map(|(_, item)| spoken_item(item, None));
    let items = || catalog.iter().map(|(s, _)| s.clone()).collect();
    let (_, accepted) = crate::completion::menu(ui, label, items, |_| None, describe, true, help);
    find(&accepted?).map(|item| match item {
        Item::Function(f) if f.max > 0 => format!("{}(", f.name),
        item => shown(item),
    })
}

/// The Functions and Variables menus beside a clause's buttons (`menu`). Returns what to add for
/// the entry accepted in either.
pub(crate) fn menus(ui: &mut Ui) -> Option<String> {
    let functions = menu(ui, tl!("Functions ▾"), Menu::Functions);
    let variables = menu(ui, tl!("Variables ▾"), Menu::Variables);
    functions.or(variables)
}

/// Puts `item` in place of the offer's word in `text`, the rest of the word after the cursor
/// too: a field as the expression names it (in the quote it was begun with; a `!` name holding a
/// `!` in double quotes, as `!` has no escape), a function with its brackets, the cursor inside
/// them as a `(` typed closes itself (`pair`), after them when it takes no arguments (before
/// other text, the bracket only open; a `(` already there is used, not doubled), a variable,
/// keyword or value as it is (a value in its quotes, over the text begun and its closing quote);
/// as one undo step of the box, the cursor after it. Nothing when it doesn't
/// fit (`written`). Either way the box keeps the focus (a click on the list took it), so the list
/// stays open when nothing was put. Returns where it was put: the new text's hash and the cursor.
fn accept(ui: &Ui, id: Id, text: &mut String, offer: &Offer, item: &str) -> Option<(u64, usize)> {
    ui.memory_mut(|m| m.request_focus(id));
    let (new, at) = written(text, offer, item)?;
    let mut state = TextEditState::load(ui.ctx(), id).unwrap_or_default();
    let mut undoer = state.undoer();
    let was = state.cursor.char_range().unwrap_or(CCursorRange::one(CCursor::new(offer.end)));
    undoer.add_undo(&(was, text.clone()));
    state.set_undoer(undoer);
    *text = new;
    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(at))));
    state.store(ui.ctx(), id);
    Some((egui::util::hash(text.as_str()), at))
}

/// How many of `rest`, the text after the cursor, are still the word being replaced: the rest of
/// a name or variable (`po▮p`), or of a quoted name or text with its closing quote
/// (`"Po▮p Est"`, `'Pa▮ris'`; a doubled `"` or `'` is one inside it). None of an unclosed quoted
/// name or text: what follows is left.
fn word_rest(rest: &[char], quote: Option<char>) -> usize {
    let Some(q) = quote else { return rest.iter().take_while(|c| c.is_alphanumeric() || **c == '_').count() };
    let mut i = 0;
    while let Some(&c) = rest.get(i) {
        if c == q && q != '!' && rest.get(i + 1) == Some(&q) {
            i += 2;
        } else if c == q {
            return i + 1;
        } else {
            i += 1;
        }
    }
    0
}

/// `text` with `item` of `offer` accepted (`accept`), and where the cursor goes (chars). None
/// past the longest expression read (`MAX_LEN`, as the box takes no more): nothing is put in
/// part, as a call left open there couldn't be closed.
pub(crate) fn written(text: &str, offer: &Offer, item: &str) -> Option<(String, usize)> {
    let of = find(offer, item)?;
    let chars: Vec<char> = text.chars().collect();
    let (before, rest) = (chars.get(..offer.start)?, chars.get(offer.end..)?);
    let mut after = rest.get(word_rest(rest, offer.quote)..).unwrap_or_default();
    // Chars of what's put the cursor goes back over: into a function's brackets.
    let mut into_brackets = 0;
    let put = match of {
        Item::Field(name) => match offer.quote.filter(|q| *q != '!' || !name.contains('!')) {
            Some(q) => {
                let name = if q == '"' { name.replace('"', "\"\"") } else { name.to_string() };
                format!("{q}{name}{q}")
            }
            None => field_ref(name),
        },
        Item::Function(f) if after.first() == Some(&'(') => {
            after = after.get(1..).unwrap_or_default();
            format!("{}(", f.name)
        }
        Item::Function(f) if f.max == 0 => format!("{}()", f.name),
        Item::Function(f) if after.first().is_none_or(|c| closes(*c)) => {
            into_brackets = 1;
            format!("{}()", f.name)
        }
        Item::Function(f) => format!("{}(", f.name),
        Item::Variable(_, name) => name.to_string(),
        Item::Keyword(k) => k.word().to_string(),
        Item::Value(written, ..) => written.clone(),
    };
    if before.len() + put.chars().count() + after.len() > query::MAX_LEN {
        return None;
    }
    let mut new: String = before.iter().collect();
    new.push_str(&put);
    new.extend(after);
    Some((new, (offer.start + put.chars().count()).saturating_sub(into_brackets)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};
    use rustgis_engine::data::{Field, FieldType, GeometryType, Value};

    fn ds() -> Dataset {
        Dataset::new(
            "t",
            GeometryType::Polygon,
            vec![Field::new("name", FieldType::Text), Field::new("pop", FieldType::Integer), Field::new("Pop Est", FieldType::Integer)],
        )
    }

    /// A harness with an expression box over `data`, its id `id`, holding `text`.
    fn boxed(id: Id, data: Option<Dataset>, text: &str) -> egui_kittest::Harness<'static, String> {
        egui_kittest::Harness::new_ui_state(
            move |ui, text: &mut String| {
                let label = ui.label("Expression");
                let _ = expr_edit(ui, id, text, data.as_ref(), |e| e).labelled_by(label.id);
            },
            text.to_string(),
        )
    }

    /// The primary button `pressed`, or let go, at `at`.
    fn button(at: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    /// What is offered at char `at` of `text`.
    fn offer_at(text: &str, at: usize, ds: &Dataset, asked: bool) -> Option<Offer> {
        offer_from(&query::complete_at(text, at, ds)?, text, ds, asked)
    }

    /// The items offered at the end of `text`.
    fn items(text: &str, asked: bool) -> Vec<String> {
        offer_at(text, text.chars().count(), &ds(), asked).map(|o| o.items).unwrap_or_default()
    }

    /// [`ds`] with rows: names (one with a `'`, one null) and numbers.
    fn places() -> Dataset {
        let mut d = ds();
        for (name, pop) in [("Paris", 10), ("Pamplona", 3), ("O'Brien", 10), ("Lima", 25)] {
            d.push(None, vec![Value::Text(name.into()), Value::Int(pop)]);
        }
        d.push(None, vec![Value::Null, Value::Null]);
        d
    }

    /// The items offered over [`places`] at the end of `text`.
    fn offered(text: &str, asked: bool) -> Vec<String> {
        offer_at(text, text.chars().count(), &places(), asked).map(|o| o.items).unwrap_or_default()
    }

    /// Where a value compared with a field goes, the field's values are offered as the expression
    /// writes them: inside a text, a text field's that start with what's typed; with Ctrl+Space
    /// where a value goes, any field's, first. Not a number's inside a text, nor with no field
    /// compared, nor null.
    #[test]
    fn a_fields_values_are_offered_where_one_is_compared_with_it() {
        assert_eq!(offered("name = '", false), ["'Lima'", "'O''Brien'", "'Pamplona'", "'Paris'"]);
        assert_eq!(offered("name = 'pa", false), ["'Pamplona'", "'Paris'"], "letter case aside");
        assert_eq!(offered("name LIKE 'Pa", false), ["'Pamplona'", "'Paris'"]);
        let mut wild = places();
        for name in ["Route_66", "100%", "Route 66"] {
            wild.push(None, vec![Value::Text(name.into()), Value::Null]);
        }
        let like = |text: &str| offer_at(text, text.chars().count(), &wild, false).map(|o| o.items).unwrap_or_default();
        assert_eq!(like("name LIKE 'R"), ["'Route 66'"], "a `_` in a pattern matches more");
        assert_eq!(like("name ILIKE '1"), Vec::<String>::new(), "so does a `%`");
        assert_eq!(like("name = 'R"), ["'Route 66'", "'Route_66'"], "not in a comparison");
        assert_eq!(like("name IN ('R"), ["'Route 66'", "'Route_66'"]);
        assert_eq!(offered("name IN ('Lima', 'O''", false), ["'O''Brien'"], "a doubled quote is one");
        assert!(offered("name = 'Paris", false).is_empty(), "not what's typed whole");
        assert!(offered("name = 'x", false).is_empty());
        assert!(offered("pop = '", false).is_empty(), "a number isn't written in quotes");
        assert!(offered("upper(name) = '", false).is_empty(), "compared with no field");
        assert!(offered("'", false).is_empty(), "compared with nothing");
        assert!(offered("pop = ", false).is_empty(), "nothing typed, not asked");
        let asked = offered("pop = ", true);
        assert_eq!(asked.get(..3), Some(&["3".to_string(), "10".to_string(), "25".to_string()][..]), "{asked:?}");
        assert!(asked.contains(&"pop".to_string()) && asked.contains(&"round()".to_string()), "then the rest");
        assert_eq!(offered("name IN ('Lima', ", true).first().map(String::as_str), Some("'Lima'"));
        assert!(!offered("pop = p", true).contains(&"3".to_string()), "a word typed is no value");
        for (text, at) in [("pop = 1.5", 6), ("name = 'Lima'", 7), ("pop IN (3, 10)", 8)] {
            let items = offer_at(text, at, &places(), true).map(|o| o.items).unwrap_or_default();
            assert!(!items.iter().any(|i| i == "3" || i == "'Paris'"), "not before a value written: {text}");
        }
        assert_eq!(offer_at("pop IN (3, )", 11, &places(), true).unwrap().items.first().map(String::as_str), Some("3"));
        // Of a layer of no rows, none.
        assert_eq!(items("name = '", false), Vec::<String>::new());
        // A number in a text field (a hostile file) isn't written bare over the quote begun.
        let mut odd = places();
        odd.push(None, vec![Value::Int(7)]);
        assert!(offer_at("name = '", 8, &odd, false).unwrap().items.iter().all(|i| i.starts_with('\'')));
    }

    /// The values offered are the rows' as they are now: other rows of the same fields (another
    /// layer, an edit, a streamed layer's view read) are read again, with the text and cursor
    /// unchanged.
    #[test]
    fn values_follow_the_rows_they_are_read_from() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let id = Id::unique("expr");
        let mut h = egui_kittest::Harness::new_ui_state(
            move |ui, (text, data): &mut (String, Dataset)| {
                let label = ui.label("Expression");
                let _ = expr_edit(ui, id, text, Some(data), |e| e).labelled_by(label.id);
            },
            ("name = 'P".to_string(), places()),
        );
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        h.run();
        h.event(egui::Event::Text("i".into()));
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, "'Pisa'").is_none());
        let mut other = ds();
        other.push(None, vec![Value::Text("Pisa".into())]);
        h.state_mut().1 = other;
        h.run();
        assert_eq!(h.state().0, "name = 'Pi");
        assert!(h.query_by_role_and_label(Role::Button, "'Pisa'").is_some(), "the new rows' values");
        // Edited where they are, nothing else holding them.
        h.state_mut().1.push(None, vec![Value::Text("Pistoia".into())]);
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, "'Pistoia'").is_some(), "the edited rows' values");
    }

    /// Accepting a value writes it in its quotes over the text begun and the quote that closed
    /// itself; a number as it is.
    #[test]
    fn accepting_a_value_writes_it_whole() {
        for (text, item, want) in [
            ("name = 'Pa▮'", "'Paris'", "name = 'Paris'▮"),
            ("name = 'Pa▮", "'Paris'", "name = 'Paris'▮"),
            ("name = 'Pa▮ris' AND pop > 1", "'Pamplona'", "name = 'Pamplona'▮ AND pop > 1"),
            ("name = 'O''▮'", "'O''Brien'", "name = 'O''Brien'▮"),
            ("name IN ('Lima', '▮')", "'Paris'", "name IN ('Lima', 'Paris'▮)"),
        ] {
            let at = text.chars().position(|c| c == '▮').unwrap();
            let text = text.replace('▮', "");
            let offer = offer_at(&text, at, &places(), false).unwrap();
            let (new, cursor) = written(&text, &offer, item).unwrap();
            let mut got: Vec<char> = new.chars().collect();
            got.insert(cursor, '▮');
            assert_eq!(got.into_iter().collect::<String>(), want, "{text}");
        }
        let text = "pop > ";
        let offer = offer_at(text, 6, &places(), true).unwrap();
        assert_eq!(written(text, &offer, "25"), Some(("pop > 25".to_string(), 8)));
    }

    /// Typed in the box: a `'` after `=` closes itself, the text typed in it lists the field's
    /// values, and Enter writes the one highlighted over the text and its closing quote.
    #[test]
    fn a_value_typed_in_the_box_is_taken_from_the_list() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let id = Id::unique("expr");
        let mut h = boxed(id, Some(places()), "");
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        h.run();
        for typed in ["name = ", "'", "P", "a", "r"] {
            h.event(egui::Event::Text(typed.into()));
            h.run();
        }
        assert_eq!(h.state(), "name = 'Par'", "the quote closed itself");
        assert!(h.query_by_role_and_label(Role::Button, "'Paris'").is_some(), "offered");
        assert!(h.query_by_role_and_label(Role::Button, "'Pamplona'").is_none(), "narrowed");
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state(), "name = 'Paris'");
        assert!(h.query_by_role_and_label(Role::Button, "'Paris'").is_none(), "closed once taken");
    }

    /// The help says when the values aren't all of the layer's: of a sample, or of a streamed
    /// layer's view.
    #[test]
    fn a_values_help_says_what_they_were_read_from() {
        let note = |read, of, view| ValuesRead { read, of, view }.note();
        assert_eq!(note(4, 4, false), None);
        assert_eq!(note(10_000, 25_000, false).as_deref(), Some("Values of 10000 of 25000 features, evenly spread"));
        assert_eq!(note(40, 40, true).as_deref(), Some("Values of the 40 features read for the view"));
        assert_eq!(note(10_000, 30_000, true).as_deref(), Some("Values of 10000 of the 30000 features read for the view, evenly spread"));
        let mut streamed = places();
        streamed.windowed = true;
        let offer = offer_at("name = '", 8, &streamed, false).unwrap();
        assert!(matches!(offer.of.first(), Some(Item::Value(_, field, ValuesRead { read: 5, of: 5, view: true })) if field == "name"));
    }

    fn catalog_harness(size: egui::Vec2) -> egui_kittest::Harness<'static, ()> {
        egui_kittest::Harness::builder().with_size(size).allow_missing_glyphs().build_ui(|ui| {
            let _ = menus(ui);
        })
    }

    /// A function is added open, ready for its arguments, unless it takes none: then it's added
    /// whole, `pi()`.
    #[test]
    fn a_function_without_arguments_is_added_whole() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        for (f, end) in [(query::functions().find(|f| f.max == 0).unwrap(), "()"), (query::functions().find(|f| f.max > 0).unwrap(), "(")] {
            let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(720.0, 600.0)).allow_missing_glyphs().build_ui_state(
                |ui, got: &mut Option<String>| {
                    if let Some(item) = menus(ui) {
                        *got = Some(item);
                    }
                },
                None,
            );
            h.run();
            h.get_by_role_and_label(Role::Button, "Functions ▾").click();
            h.run();
            // Highlighted from the keyboard: the row may lie past the ten in view.
            let at = menu_items(Menu::Functions).iter().position(|(s, _)| *s == format!("{}()", f.name)).unwrap();
            for _ in 0..at {
                h.key_press(Key::ArrowDown);
                h.run();
            }
            h.key_press(Key::Enter);
            h.run();
            assert_eq!(h.state().as_deref(), Some(format!("{}{end}", f.name).as_str()), "{}", f.name);
        }
    }

    #[test]
    fn catalog_rows_describe_functions_and_variables_to_accessibility_clients() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::{NodeT, Queryable};
        let mut h = catalog_harness(egui::vec2(720.0, 600.0));
        h.run();
        h.get_by_role_and_label(Role::Button, "Functions ▾").click();
        h.run();
        let f = query::functions().find(|f| f.name == "round").unwrap();
        let row = h.get_by_role_and_label(Role::Button, "round()");
        let node = row.accesskit_node();
        let description = node.description().unwrap();
        assert!(description.contains(f.signature));
        assert!(description.contains(tl!(f.description)));
        h.key_press(Key::Escape);
        h.run();
        h.get_by_role_and_label(Role::Button, "Variables ▾").click();
        h.run();
        let v = &VARS[0];
        assert_eq!(h.get_by_role_and_label(Role::Button, v.names[0]).accesskit_node().description(), Some(tl!(v.description).to_string()));
    }

    /// The help is the highlighted entry's, the one under the pointer's while the pointer moves,
    /// and the highlighted one's again once a key moves the highlight.
    #[test]
    fn catalog_help_follows_hover_and_then_keyboard_navigation() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut h = catalog_harness(egui::vec2(720.0, 600.0));
        h.run();
        h.get_by_role_and_label(Role::Button, "Functions ▾").click();
        h.run();
        let fs: Vec<_> = query::functions().take(2).collect();
        let abs = query::functions().find(|f| f.name == "abs").unwrap();
        assert!(h.query_by_label(fs[0].signature).is_some(), "the first one is highlighted");
        // Twice: a first move has no motion to follow.
        let at = h.get_by_role_and_label(Role::Button, "abs()").rect().center();
        h.hover_at(at - egui::vec2(0.0, 2.0));
        h.run();
        h.hover_at(at);
        h.run();
        assert!(h.query_by_label(abs.signature).is_some(), "the one under the pointer");
        assert!(h.query_by_label(fs[0].signature).is_none());
        h.key_press(Key::ArrowDown);
        h.run();
        assert!(h.query_by_label(fs[1].signature).is_some(), "the keyboard's highlight wins over the stationary pointer");
        assert!(h.query_by_label(abs.signature).is_none());
    }

    #[test]
    fn catalog_help_can_be_scrolled_into_a_short_viewport() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::{NodeT, Queryable};
        let mut h = catalog_harness(egui::vec2(420.0, 220.0));
        h.run();
        h.get_by_role_and_label(Role::Button, "Functions ▾").click();
        h.run();
        let list = h.get_by_role_and_label(Role::Button, "if()").rect();
        h.hover_at(list.center());
        for _ in 0..3 {
            h.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -1000.0),
                modifiers: Modifiers::NONE,
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
            });
            h.run();
        }
        // Scrolling can move another function under the pointer; its help must be reachable too.
        let help = query::functions()
            .find_map(|f| h.query_all_by_label(f.example).find(|n| n.accesskit_node().role() == Role::Label))
            .unwrap()
            .rect();
        assert!(h.ctx.content_rect().contains_rect(help), "the help is reachable inside the viewport: {help:?}");
    }

    #[test]
    fn a_field_is_quoted_only_when_an_expression_needs_it() {
        assert_eq!(field_ref("pop_max"), "pop_max");
        assert_eq!(field_ref("Name 2"), "\"Name 2\"");
        assert_eq!(field_ref("2020"), "\"2020\"");
        assert_eq!(field_ref("in"), "\"in\"");
        assert_eq!(field_ref("True"), "\"True\"");
        // Every keyword of the language, not only the ones a clause starts with.
        assert_eq!(field_ref("case"), "\"case\"");
        assert_eq!(field_ref("año"), "año");
        assert_eq!(field_ref("a\"b"), "\"a\"\"b\"");
    }

    /// A word begun: the fields, functions and variables it starts, letter case aside, fields
    /// first; the keywords where a keyword goes.
    #[test]
    fn a_word_begun_offers_what_it_starts() {
        assert_eq!(items("po", false), ["pop", "\"Pop Est\""]);
        assert_eq!(items("ro", false), ["round()"]);
        assert_eq!(items("$ar", false), ["$area"]);
        assert_eq!(items("pop > 1 a", false), ["AND"]);
        assert!(items("pop > 1 AND ", false).is_empty(), "no word typed");
        assert!(items("pop = 'po", false).is_empty(), "inside text");
        assert!(items("name", false).is_empty(), "the field typed whole");
    }

    /// Ctrl+Space with no word typed offers everything that goes there.
    #[test]
    fn asked_with_no_word_offers_all_that_goes_there() {
        let all = items("", true);
        assert_eq!(all.first().map(String::as_str), Some("name"), "fields first");
        assert!(all.contains(&"round()".to_string()) && all.contains(&"$area".to_string()) && all.contains(&"CASE".to_string()));
        assert!(!all.contains(&"AND".to_string()), "no AND where a value goes");
        assert!(items("", false).is_empty(), "not unasked");
    }

    /// In a quoted field name, only fields.
    #[test]
    fn a_quoted_name_offers_only_fields() {
        let o = offer_at("\"Po", 3, &ds(), false).unwrap();
        assert_eq!((o.items, o.quote, o.start), (vec!["pop".to_string(), "\"Pop Est\"".to_string()], Some('"'), 0));
        assert!(items("\"ro", false).is_empty(), "no function in a field's quotes");
    }

    /// A field that reads like a keyword or a variable is shown quoted, so each item is picked
    /// as what it is: Enter on `NOT` writes the keyword, on `"NOT"` the field.
    #[test]
    fn a_field_never_reads_as_a_keyword_or_variable() {
        let ds = Dataset::new("t", GeometryType::Point, vec![Field::new("NOT", FieldType::Text), Field::new("$area", FieldType::Double)]);
        let o = offer_at("no", 2, &ds, false).unwrap();
        assert_eq!(o.items, ["\"NOT\"", "NOT"]);
        assert_eq!(find(&o, "NOT"), Some(&Item::Keyword(Kw::Not)));
        assert_eq!(find(&o, "\"NOT\""), Some(&Item::Field("NOT".into())));
        let o = offer_at("$ar", 3, &ds, false).unwrap();
        assert_eq!(o.items, ["\"$area\"", "$area"]);
    }

    /// A field another one shadows, letter case aside, isn't offered: the expression would read
    /// the first.
    #[test]
    fn a_shadowed_field_is_not_offered() {
        let ds = Dataset::new("t", GeometryType::Point, vec![Field::new("Name", FieldType::Text), Field::new("name", FieldType::Text)]);
        assert_eq!(offer_at("na", 2, &ds, false).map(|o| o.items), Some(vec!["Name".to_string()]));
    }

    /// Accepting closes the list where it was put, though more is on offer there (`pop` taken,
    /// `"Pop Est"` still starts with it): Enter is the dialog's again. Ctrl+Space shows it.
    #[test]
    fn accepting_leaves_the_list_closed_until_asked() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let id = Id::unique("expr");
        let mut h = boxed(id, Some(ds()), "");
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        h.run();
        h.event(egui::Event::Text("po".into()));
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, "pop").is_some(), "offered");
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state(), "pop");
        assert!(h.query_by_role_and_label(Role::Button, "\"Pop Est\"").is_none(), "closed where it was put");
        h.key_press_modifiers(Modifiers::CTRL, Key::Space);
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, "\"Pop Est\"").is_some(), "asked for");
    }

    /// The argument a signature marks: its name, `…` for the rest, none past the last.
    #[test]
    fn a_signature_marks_the_argument_the_cursor_is_in() {
        fn at(sig: &str, arg: usize) -> Option<&str> {
            arg_range(sig, arg).and_then(|r| sig.get(r))
        }
        assert_eq!(at("round(x[, places])", 0), Some("x"));
        assert_eq!(at("round(x[, places])", 1), Some("places"));
        assert_eq!(at("round(x[, places])", 2), None);
        assert_eq!(at("area([unit])", 0), Some("unit"));
        assert_eq!(at("coalesce(a, b, …)", 1), Some("b"));
        assert_eq!(at("coalesce(a, b, …)", 2), Some("…"));
        assert_eq!(at("coalesce(a, b, …)", 7), Some("…"));
        assert_eq!(at("pi()", 0), None);
        // Every function's signature names as many arguments as it takes, or ends in `…`.
        for f in query::functions() {
            for arg in 0..f.max.min(4) {
                assert!(at(f.signature, arg).is_some(), "{} argument {arg}", f.signature);
            }
        }
    }

    /// The argument is marked whole, and only it, though a layout joins neighbours of one format.
    #[test]
    fn a_signature_job_marks_the_argument_alone() {
        let palette = crate::scriptcolor::Palette::of(&crate::theme::Tokens::light());
        let font = egui::FontId::monospace(12.0);
        let mark = (egui::Color32::YELLOW, egui::Color32::BLACK);
        for (sig, arg, want) in [("round(x[, places])", 1, "places"), ("round(x[, places])", 0, "x"), ("concat(a, b, …)", 5, "…")] {
            let job = signature_job(sig, arg_range(sig, arg), palette, &font, mark);
            assert_eq!(job.text, sig);
            let marked: String = job
                .sections
                .iter()
                .filter(|s| s.format.background == mark.0)
                .filter_map(|s| job.text.get(s.byte_range.start.0..s.byte_range.end.0))
                .collect();
            assert_eq!(marked, want, "{sig} argument {arg}");
            // The function's name keeps its colour: the `(` after it is in the same part.
            assert_eq!(job.sections.first().map(|s| s.format.color), Some(palette.id), "{sig}");
        }
        let job = signature_job("pi()", None, palette, &font, mark);
        assert!(job.sections.iter().all(|s| s.format.background != mark.0), "none past the last");
    }

    /// In a call, its signature floats by the cursor while no list shows; Escape closes it until
    /// the cursor leaves that call.
    #[test]
    fn a_call_shows_its_signature_until_escape() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let id = Id::unique("expr");
        let mut h = boxed(id, Some(ds()), "");
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        h.run();
        let round = |h: &egui_kittest::Harness<'_, String>| h.query_by_role_and_label(Role::Label, "round(x[, places])").is_some();
        h.event(egui::Event::Text("round(pop, ".into()));
        h.run();
        assert!(round(&h), "the signature shown");
        h.key_press(Key::Escape);
        h.run();
        assert!(!round(&h), "closed");
        assert!(h.ctx.memory(|m| m.has_focus(id)), "the box keeps the focus");
        h.event(egui::Event::Text("2".into()));
        h.run();
        assert!(!round(&h), "not again in the same call");
        h.event(egui::Event::Text(") + round(".into()));
        h.run();
        assert!(round(&h), "another call shows it again");
        assert_eq!(h.state(), "round(pop, 2) + round(");
    }

    /// The signature closed with Escape shows again in another call, and back in the first,
    /// though the cursor went straight from one to the other; not after the window is left and
    /// come back to with the cursor where it was, nor after text selected in the call.
    #[test]
    fn a_closed_signature_shows_again_in_another_call_only() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let id = Id::unique("expr");
        let mut h = boxed(id, Some(ds()), "round(1) + abs(2)");
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        let at = |h: &mut egui_kittest::Harness<'_, String>, i: usize| {
            crate::widgets::set_cursor_range(&h.ctx, id, CCursorRange::one(CCursor::new(i)));
            h.run();
        };
        let shows = |h: &egui_kittest::Harness<'_, String>, sig: &str| h.query_by_role_and_label(Role::Label, sig).is_some();
        let (round, abs) = ("round(x[, places])", "abs(x)");
        at(&mut h, 6);
        assert!(shows(&h, round));
        h.key_press(Key::Escape);
        h.run();
        assert!(!shows(&h, round), "closed");
        h.input_mut().focused = false;
        h.run();
        h.input_mut().focused = true;
        h.run();
        assert!(!shows(&h, round), "still closed: the cursor didn't move");
        at(&mut h, 15);
        assert!(shows(&h, abs), "another call");
        at(&mut h, 6);
        assert!(shows(&h, round), "back in the first");
        // Text selected in the call, then the cursor back in it: still closed.
        h.key_press(Key::Escape);
        h.run();
        crate::widgets::set_cursor_range(&h.ctx, id, CCursorRange::two(CCursor::new(6), CCursor::new(7)));
        h.run();
        at(&mut h, 7);
        assert!(!shows(&h, round), "a selection in the call keeps it closed");
    }

    /// Many fields: at most `MAX_FIELDS` of them, and the language's items still after them.
    #[test]
    fn many_fields_leave_room_for_the_language() {
        let fields = (0..80).map(|i| Field::new(format!("f{i:02}"), FieldType::Integer)).collect();
        let ds = Dataset::new("t", GeometryType::Point, fields);
        let o = offer_at("", 0, &ds, true).unwrap();
        assert_eq!(o.of.iter().filter(|i| matches!(i, Item::Field(_))).count(), MAX_FIELDS);
        for want in ["round()", "$area", "CASE"] {
            assert!(o.items.iter().any(|i| i == want), "{want}");
        }
    }

    /// What accepting writes, and where the cursor goes: `▮` marks it, in `text` (before) and in
    /// what's expected (after).
    #[test]
    fn accepting_writes_the_item_as_the_expression_needs_it() {
        let ds =
            Dataset::new("t", GeometryType::Point, vec![Field::new("Pop Est", FieldType::Integer), Field::new("Area!", FieldType::Double)]);
        for (text, item, want) in [
            ("Pop▮", "\"Pop Est\"", "\"Pop Est\"▮"),
            ("\"Pop▮\" > 1", "\"Pop Est\"", "\"Pop Est\"▮ > 1"),
            ("!Pop▮", "\"Pop Est\"", "!Pop Est!▮"),
            // `!` has no escape: a name holding one goes in double quotes.
            ("!Ar▮", "\"Area!\"", "\"Area!\"▮"),
            ("!Ar▮!", "\"Area!\"", "\"Area!\"▮"),
            // The rest of the word after the cursor is replaced too.
            ("Po▮p", "\"Pop Est\"", "\"Pop Est\"▮"),
            ("\"Po▮p E\" > 1", "\"Pop Est\"", "\"Pop Est\"▮ > 1"),
            ("\"Po▮p \"\"E\" > 1", "\"Pop Est\"", "\"Pop Est\"▮ > 1"),
            ("ro▮und(2)", "round()", "round(▮2)"),
            // With no closing quote, what follows is the rest of the expression: it stays.
            ("\"Po▮ > 1", "\"Pop Est\"", "\"Pop Est\"▮ > 1"),
            // The brackets closed as a `(` typed closes itself, the cursor inside.
            ("ro▮", "round()", "round(▮)"),
            ("ro▮ + 1", "round()", "round(▮) + 1"),
            ("abs(ro▮)", "round()", "abs(round(▮))"),
            ("ro▮(2)", "round()", "round(▮2)"),
            ("p▮", "pi()", "pi()▮"),
        ] {
            let at = text.chars().position(|c| c == '▮').unwrap();
            let text = text.replace('▮', "");
            let offer = offer_at(&text, at, &ds, false).unwrap();
            let (new, cursor) = written(&text, &offer, item).unwrap();
            let mut got: Vec<char> = new.chars().collect();
            got.insert(cursor, '▮');
            assert_eq!(got.into_iter().collect::<String>(), want, "{text}");
        }
    }

    /// At the longest expression read, an item that fits whole is written, and one a char
    /// longer isn't, nor any part of it: a function, a field, a keyword.
    #[test]
    fn accepting_stays_within_the_longest_expression() {
        let accepted = |pad: usize, typed: &str, item: &str| {
            let text = format!("{}{typed}", "x".repeat(pad));
            let offer = offer_at(&text, text.chars().count(), &ds(), false).unwrap();
            written(&text, &offer, item).map(|(new, _)| new.chars().skip(pad).collect::<String>())
        };
        for (typed, item, want) in
            [(" + ro", "round()", " + round()"), (" + Po", "\"Pop Est\"", " + \"Pop Est\""), (" > 1 a", "AND", " > 1 AND")]
        {
            let pad = query::MAX_LEN - want.chars().count();
            assert_eq!(accepted(pad, typed, item).as_deref(), Some(want), "{item} fits whole");
            assert_eq!(accepted(pad + 1, typed, item), None, "{item} a char too long");
        }
    }

    /// Clicks the list's `item`: pressed, and let go two frames later, as a real click is, as the
    /// Script pane's `a_click_accepts_an_item` does.
    fn click(h: &mut egui_kittest::Harness<'_, String>, item: &str) {
        use egui_kittest::kittest::Queryable;
        let at = h.get_by_role_and_label(egui::accesskit::Role::Button, item).rect().center();
        h.event(egui::Event::PointerMoved(at));
        h.step();
        h.event(button(at, true));
        h.step();
        h.step();
        h.event(button(at, false));
        h.run();
    }

    /// A field's type changed while its list is open: the row says the new one, as its help does
    /// (CodeRabbit on #2654), the same items over other fields being another offer.
    #[test]
    fn a_row_says_a_field_s_new_type_while_its_list_is_open() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let id = Id::unique("expr");
        let mut h = egui_kittest::Harness::new_ui_state(
            move |ui, (text, data): &mut (String, Dataset)| {
                let label = ui.label("Expression");
                let _ = expr_edit(ui, id, text, Some(&*data), |e| e).labelled_by(label.id);
            },
            (String::new(), ds()),
        );
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        h.run();
        h.event(egui::Event::Text("po".into()));
        h.run();
        let said = |h: &egui_kittest::Harness<'_, (String, Dataset)>| {
            let node = h.get_by_label("Expression");
            node.accesskit_node().active_descendant().and_then(|row| row.description())
        };
        assert_eq!(said(&h).as_deref(), Some(tl!(FieldType::Integer.label())));
        if let Some(f) = h.state_mut().1.fields.iter_mut().find(|f| f.name == "pop") {
            f.ty = FieldType::Text;
            f.alias = Some("People".into());
        }
        h.run();
        let now = said(&h).unwrap_or_default();
        assert_eq!(now, format!("{}\nPeople", tl!(FieldType::Text.label())), "the new type and alias");
    }

    /// While a list is open the box keeps the focus, and its highlighted row is the box's active
    /// descendant, with what its help says: a screen reader reads the entry the highlight is on,
    /// and follows it (#2619). A function says its signature and what it gives, a field its type.
    /// Closed, the box names no row.
    #[test]
    fn a_screen_reader_reads_the_highlighted_entry() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let id = Id::unique("expr");
        let mut h = boxed(id, Some(ds()), "");
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        h.run();
        let read = |h: &egui_kittest::Harness<'_, String>| {
            let node = h.get_by_label("Expression");
            node.accesskit_node().active_descendant().map(|row| (row.label().unwrap_or_default(), row.description().unwrap_or_default()))
        };
        h.event(egui::Event::Text("ro".into()));
        h.run();
        let (label, said) = read(&h).expect("a row named");
        assert_eq!(label, "round()");
        assert!(said.starts_with("round("), "its signature: {said}");
        assert!(h.ctx.memory(|m| m.has_focus(id)), "the box keeps the focus");
        h.key_press(egui::Key::Backspace);
        h.key_press(egui::Key::Backspace);
        h.event(egui::Event::Text("po".into()));
        h.run();
        let (first, said) = read(&h).expect("a row named");
        assert_eq!(first, "pop");
        assert_eq!(said, tl!(FieldType::Integer.label()), "a field's type");
        h.key_press(egui::Key::ArrowDown);
        h.run();
        let (second, _) = read(&h).expect("a row named");
        assert_eq!(second, "\"Pop Est\"", "it follows the highlight");
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(read(&h), None, "closed: no row named");
    }

    /// A function clicked in the list is written, and the box keeps the focus (the list is under
    /// it, so the button let go there is a click outside it): what's typed next goes in its
    /// brackets.
    #[test]
    fn a_click_accepts_and_the_box_keeps_the_focus() {
        let id = Id::unique("expr");
        let mut h = boxed(id, Some(ds()), "");
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        h.run();
        h.event(egui::Event::Text("ro".into()));
        h.run();
        click(&mut h, "round()");
        assert!(h.ctx.memory(|m| m.has_focus(id)), "the box keeps the focus");
        h.event(egui::Event::Text("1".into()));
        h.run();
        assert_eq!(h.state(), "round(1)");
    }

    /// A box inside a menu (the ribbon's Display Filters): a click in the box brings the menu,
    /// a foreground area as the list is, to the top. The list stays drawn over it, and a click
    /// there still reaches the list.
    #[test]
    fn the_list_stays_over_a_menu_its_box_is_in() {
        use egui_kittest::kittest::Queryable;
        let id = Id::unique("expr");
        let menu = Id::unique("menu");
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(600.0, 400.0)).build_ui_state(
            move |ui, text: &mut String| {
                egui::Area::new(menu).order(egui::Order::Foreground).fixed_pos(egui::pos2(20.0, 20.0)).show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_min_size(egui::vec2(400.0, 250.0));
                        let label = ui.label("Filter");
                        let _ = expr_edit(ui, id, text, Some(&ds()), |e| e).labelled_by(label.id);
                    });
                });
            },
            String::new(),
        );
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        h.run();
        h.event(egui::Event::Text("po".into()));
        h.run();
        let at = h.ctx.read_response(id).expect("the box").rect.center();
        h.event(egui::Event::PointerMoved(at));
        h.event(button(at, true));
        h.step();
        h.event(button(at, false));
        h.run();
        let item = h.get_by_role_and_label(egui::accesskit::Role::Button, "pop").rect().center();
        let menu_layer = egui::LayerId::new(egui::Order::Foreground, menu);
        assert_ne!(h.ctx.layer_id_at(item), Some(menu_layer), "the list is drawn over the menu");
        click(&mut h, "pop");
        assert_eq!(h.state(), "pop");
    }

    /// A click whose press and release come in one frame (a quick tap, a slow frame) reaches the
    /// item too, and the box keeps the focus. (kittest's `click()` gives each its own frame.)
    #[test]
    fn a_click_in_one_frame_accepts_too() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let id = Id::unique("expr");
        let mut h = boxed(id, Some(ds()), "");
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        h.run();
        h.event(egui::Event::Text("ro".into()));
        h.run();
        let at = h.get_by_role_and_label(Role::Button, "round()").rect().center();
        h.event(egui::Event::PointerMoved(at));
        h.run();
        h.input_mut().events.extend([button(at, true), button(at, false)]);
        h.run();
        assert!(h.ctx.memory(|m| m.has_focus(id)), "the box keeps the focus");
        h.event(egui::Event::Text("1".into()));
        h.run();
        assert_eq!(h.state(), "round(1)");
    }

    /// In the box, at the longest expression read: Enter on a function that doesn't fit writes
    /// nothing, and the list stays open; so does a click on it, the box keeping the focus.
    #[test]
    fn accepting_what_does_not_fit_leaves_the_list_open() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let id = Id::unique("expr");
        // `ro` typed ends it at the longest: `round()` doesn't fit.
        let start = format!("{} + r", "x".repeat(query::MAX_LEN - 5));
        let mut h = boxed(id, Some(ds()), &start);
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        crate::widgets::set_cursor_range(&h.ctx, id, CCursorRange::one(CCursor::new(start.chars().count())));
        h.run();
        h.event(egui::Event::Text("o".into()));
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, "round()").is_some(), "offered");
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state().chars().count(), query::MAX_LEN, "nothing written");
        assert!(h.state().ends_with(" + ro"));
        assert!(h.query_by_role_and_label(Role::Button, "round()").is_some(), "the list still open");
        click(&mut h, "round()");
        assert!(h.state().ends_with(" + ro") && h.state().chars().count() == query::MAX_LEN, "nothing written on a click");
        assert!(h.ctx.memory(|m| m.has_focus(id)), "the box keeps the focus");
        assert!(h.query_by_role_and_label(Role::Button, "round()").is_some(), "the list still open after a click");
    }

    /// What `pair` makes of `ins` typed at char `at` of `old` (`▮` there in the messages).
    fn typed(old: &str, at: usize, ins: &str) -> Option<String> {
        let b = byte_at(old, at).unwrap();
        let text = format!("{}{ins}{}", &old[..b], &old[b..]);
        let before = Before { at, prev: at.checked_sub(1).and_then(|i| char_at(old, i)), len: old.len() };
        let keyed = if ins.chars().count() == 1 { Keyed::Pair } else { Keyed::Other };
        pair(&text, before, at + ins.chars().count(), keyed)
    }

    /// What `pair` makes of Backspace at char `at` of `old`.
    fn backspace(old: &str, at: usize) -> Option<String> {
        let b = byte_at(old, at - 1).unwrap();
        let e = byte_at(old, at).unwrap();
        let text = format!("{}{}", &old[..b], &old[e..]);
        let before = Before { at, prev: char_at(old, at - 1), len: old.len() };
        pair(&text, before, at - 1, Keyed::Backspace)
    }

    /// `(` closes itself before nothing, a space, a `)` or a `,`; not before other text, in a
    /// text or a field's name, nor for a paste; `)` steps over a `)`; Backspace in an empty `()`
    /// deletes both.
    #[test]
    fn a_bracket_typed_closes_itself() {
        assert_eq!(typed("round", 5, "(").as_deref(), Some("round()"));
        assert_eq!(typed("abs() + 1", 4, "(").as_deref(), Some("abs(()) + 1"), "before a closing bracket");
        assert_eq!(typed("concat(a, )", 10, "(").as_deref(), Some("concat(a, ())"), "an argument");
        assert_eq!(typed("x pop", 2, "("), None, "before other text");
        assert_eq!(typed("name = 'a ", 10, "("), None, "in a text");
        assert_eq!(typed("\"Pop ", 5, "("), None, "in a field's name");
        assert_eq!(typed("!Pop ", 5, "("), None, "in a `!` name");
        assert_eq!(typed("pop != ", 7, "(").as_deref(), Some("pop != ()"), "`!=` is no name");
        assert_eq!(typed("ñandú", 5, "(").as_deref(), Some("ñandú()"), "chars, not bytes");
        assert_eq!(typed("round", 5, "(("), None, "more than the bracket typed");
        let long = "x".repeat(query::MAX_LEN - 1);
        assert_eq!(typed(&long, long.len(), "("), None, "not past the longest expression");
        let short = "x".repeat(query::MAX_LEN - 2);
        assert!(typed(&short, short.len(), "(").is_some(), "up to it");
        assert_eq!(typed("round(1)", 7, ")").as_deref(), Some("round(1)"), "stepped over");
        assert_eq!(typed("'a)'", 2, ")"), None, "not over one in a text");
        assert_eq!(backspace("round()", 6).as_deref(), Some("round"));
        assert_eq!(backspace("round(1)", 6), None, "not empty");
        let before = Before { at: 5, prev: Some('d'), len: 5 };
        assert_eq!(pair("round(", before, 6, Keyed::Other), None, "a paste, an undo");
    }

    /// A `'` or `"` closes itself where a value can start: not after a word or a quote; one typed
    /// before the quote that ends its text or name steps over it, not an escaped one; Backspace in
    /// an empty `''` or `""` deletes both, not after a text's closing quote.
    #[test]
    fn a_quote_works_as_a_bracket() {
        assert_eq!(typed("", 0, "'").as_deref(), Some("''"), "the start");
        assert_eq!(typed("name = ", 7, "'").as_deref(), Some("name = ''"));
        assert_eq!(typed("name=", 5, "'").as_deref(), Some("name=''"), "right after an operator");
        assert_eq!(typed("upper()", 6, "'").as_deref(), Some("upper('')"), "in a call");
        assert_eq!(typed("concat(a,)", 9, "\"").as_deref(), Some("concat(a,\"\")"), "a field's name");
        assert_eq!(typed("x ab", 2, "'"), None, "before other text");
        assert_eq!(typed("ab", 2, "'"), None, "after a word");
        assert_eq!(typed("''", 2, "'"), None, "after a quote");
        assert_eq!(typed("'ab'", 3, "'").as_deref(), Some("'ab'"), "stepped over");
        assert_eq!(typed("\"ab\"", 3, "\"").as_deref(), Some("\"ab\""), "a name's too");
        assert_eq!(typed("'ab\"", 3, "\""), None, "not another quote's");
        assert_eq!(typed("'a''b'", 3, "'"), None, "not an escaped one");
        assert_eq!(backspace("name = ''", 8).as_deref(), Some("name = "));
        assert_eq!(backspace("\"\"", 1).as_deref(), Some(""));
        assert_eq!(backspace("'a''", 3), None, "a text's closing quote");
    }

    /// In the box: `(` closes itself, the cursor inside, `)` steps over, Backspace deletes both;
    /// a function taken from the list has its brackets closed too.
    #[test]
    fn brackets_close_themselves_in_the_box() {
        let id = Id::unique("expr");
        let mut h = boxed(id, Some(ds()), "");
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        h.run();
        for c in ["abs", "(", "p", "o", "p", ")"] {
            h.event(egui::Event::Text(c.into()));
            h.run();
        }
        assert_eq!(h.state(), "abs(pop)", "`)` stepped over");
        h.key_press(Key::ArrowLeft);
        h.run();
        h.event(egui::Event::Text(" + ro".into()));
        h.run();
        h.key_press(Key::Enter);
        h.run();
        h.event(egui::Event::Text("1".into()));
        h.run();
        assert_eq!(h.state(), "abs(pop + round(1))", "the function's brackets closed, the cursor inside");
        let mut h = boxed(id, None, "x");
        h.run();
        h.ctx.memory_mut(|m| m.request_focus(id));
        crate::widgets::set_cursor_range(&h.ctx, id, CCursorRange::one(CCursor::new(1)));
        h.run();
        h.event(egui::Event::Text("(".into()));
        h.run();
        h.key_press(Key::Backspace);
        h.run();
        assert_eq!(h.state(), "x", "Backspace in `()` deletes both");
        // As Wayland types with an input method running (ibus, fcitx): the `(` committed by it,
        // after an empty composition.
        h.event(egui::Event::Ime(egui::ImeEvent::Preedit { text: String::new(), active_range_chars: None }));
        h.event(egui::Event::Ime(egui::ImeEvent::Commit("(".into())));
        h.run();
        assert_eq!(h.state(), "x()", "an input method's `(` closes itself too");
        h.event(egui::Event::Ime(egui::ImeEvent::Commit(")".into())));
        h.run();
        assert_eq!(h.state(), "x()", "and its `)` steps over");
    }
}
