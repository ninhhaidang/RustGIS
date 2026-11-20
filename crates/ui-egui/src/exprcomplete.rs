//! A box where an expression is written, with suggestions as it is typed (#1240): the shared
//! list (`completion`) under the cursor, holding what the language takes there as the parser
//! reads it (`query::complete_at`): the dataset's fields, its functions, variables and keywords,
//! each with its help beside. It opens once a character of the word is typed, or with
//! Ctrl+Space. Nothing is listed here by hand: what the language learns is offered too. A `(`,
//! `'` or `"` typed closes itself, as the Script pane's braces do (`completion::pairs`).

use egui::text::{CCursor, CCursorRange};
use egui::widgets::text_edit::{TextEditOutput, TextEditState};
use egui::{Id, Ui};
use rustgis_engine::data::Dataset;
use rustgis_engine::data::query::{self, Call, Completion, FuncSpec, Kw, Place, VARS};

use crate::completion::{Asked, Before, GAP, Keyed, List, Offered, Side, Tip, Worked, byte_at, char_at, pairs};

/// The most fields the list holds (typing narrows them; it scrolls): a table of hundreds of
/// columns mustn't draw hundreds of rows. The functions, variables and keywords after them are
/// the language's, a few dozen, and always all there.
const MAX_FIELDS: usize = 50;

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
/// what goes there that starts with the word typed (the word typed whole isn't offered again).
/// Only once a character of the word is typed (a quote begun counts), unless `asked`
/// (Ctrl+Space).
pub(crate) fn offer_from(c: &Completion, text: &str, ds: &Dataset, asked: bool) -> Option<Offer> {
    if c.place == Place::Text || (c.word.is_empty() && !asked) {
        return None;
    }
    let p = c.prefix.as_str();
    let fits = |name: &str| starts_with(name, p) && !name.eq_ignore_ascii_case(p);
    let mut of = Vec::new();
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
    let quote = (c.place == Place::Field).then(|| text.chars().nth(c.word.start)).flatten();
    Some(Offer { start: c.word.start, end: c.word.end, quote, items, of })
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
    }
}

/// The box's state between frames.
#[derive(Clone, Default)]
struct State {
    list: List<Offer>,
    /// What was worked out to offer, and the call the cursor is in, for which cursor, text (its
    /// hash) and fields (their names' hash: another layer's offer others), whether it was asked
    /// for and whether an item was just accepted there.
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
    let out: TextEditOutput = style(egui::TextEdit::singleline(text).id(id).event_filter(filter)).show(ui);
    let visible = out.text_clip_rect;
    let hash = egui::util::hash(text.as_str());
    let fields = ds.map_or(0, |d| egui::util::hash(d.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>()));
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
        let with = (fields, here, quiet);
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
    let draw = |ui: &mut Ui, offer: &Offer, item: &str, width: f32| draw_item(ui, offer, item, ds, width);
    let clicked = state.list.show(ui, id.with("expr_list"), &out, visible, work, icon, draw);
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

/// The help of `item` of `offer` beside the list, after a gap (`draw_help`): the expression box's
/// list and the Script's show the same.
pub(crate) fn draw_item(ui: &mut Ui, offer: &Offer, item: &str, ds: Option<&Dataset>, width: f32) {
    if let Some(of) = find(offer, item) {
        ui.add_space(GAP);
        draw_help(ui, of, ds, width);
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
            ui.add_space(GAP);
            draw_help(ui, of, None, width);
        }
    };
    let describe = |i: usize| match &catalog.get(i)?.1 {
        Item::Function(f) => Some(format!("{}\n{}", f.signature, tl!(f.description))),
        Item::Variable(i, _) => VARS.get(*i).map(|v| tl!(v.description).to_string()),
        _ => None,
    };
    let items = || catalog.iter().map(|(s, _)| s.clone()).collect();
    let (_, accepted) = crate::completion::menu(ui, label, items, |_| None, describe, help);
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
/// other text, the bracket only open; a `(` already there is used, not doubled), a variable or
/// keyword as it is; as one undo step of the box, the cursor after it. Nothing when it doesn't
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
/// a name or variable (`po▮p`), or of a quoted name with its closing quote (`"Po▮p Est"`; a
/// doubled `"` is one inside the name). None of an unclosed quoted name: what follows is left.
fn word_rest(rest: &[char], quote: Option<char>) -> usize {
    let Some(q) = quote else { return rest.iter().take_while(|c| c.is_alphanumeric() || **c == '_').count() };
    let mut i = 0;
    while let Some(&c) = rest.get(i) {
        if c == q && q == '"' && rest.get(i + 1) == Some(&'"') {
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
    use rustgis_engine::data::{Field, FieldType, GeometryType};

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
            let mut state = TextEditState::load(&h.ctx, id).unwrap_or_default();
            state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(i))));
            state.store(&h.ctx, id);
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
        let mut state = TextEditState::load(&h.ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(6), CCursor::new(7))));
        state.store(&h.ctx, id);
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
        let mut state = TextEditState::load(&h.ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(start.chars().count()))));
        state.store(&h.ctx, id);
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
        let mut state = TextEditState::load(&h.ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(1))));
        state.store(&h.ctx, id);
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
