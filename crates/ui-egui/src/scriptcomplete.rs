//! Autocomplete for the Script pane (#662): a floating list of suggestions just under the cursor,
//! as in a code editor. ↑/↓ move the highlight, Enter or Tab accept it, Escape closes the list, or
//! a click accepts an item; Ctrl+Space opens it with nothing typed yet. What to offer at a cursor
//! (`offer_at`) is worked out apart from the box and tested on its own: command ids (a line's
//! first word), a command's parameter keys in its `{…}` and in its second-level objects
//! (`key_at`), and the ids some values take: tools for a `gp.run` line's `"tool"`, commands for a
//! `task.addStep` line's `"command"` (`value_at`), whose own keys then go in its `"params": {…}`,
//! and what the project holds: layers for a `"layer"`, the line's layer's fields for a `"field"`
//! and the keys described as taking fields (in their arrays too, #846), maps, layouts, tables and
//! label classes, and in a `gp.run` line's `"params"` what the tool's parameter takes by its kind
//! (`tool_value`), as the tool's form offers it. Typing aids beside it: a command's parameters
//! added on accept (`template`), and braces, brackets and quotes that close themselves (`brace`).
//! Quoted text is read by one rule everywhere (`rustgis_engine::Quotes`), the one the colours use.

use egui::text::{CCursor, CCursorRange};
use egui::widgets::text_edit::{TextEditOutput, TextEditState};
use egui::{Id, Ui};
use rustgis_engine::Quotes;
use rustgis_engine::cmd::workflows::refused_in_script;

use crate::completion::{Asked, Before, Keyed, List, Offered, Side, Tip, Worked, byte_at, char_at, pairs};

/// The fewest characters typed before ids are offered (one would offer most of them).
const MIN_TYPED: usize = 2;

/// What to offer at a cursor: the word being typed (char indexes, `end` at the cursor) and what
/// can replace it: command ids (`of` None), or what `of` says.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub start: usize,
    pub end: usize,
    pub items: Vec<String>,
    pub of: Option<Of>,
}

/// What an offer holds besides the command ids that start a line: a command's parameter keys (by
/// its id), the keys of the second-level object of a command's entry `key`, a geoprocessing
/// tool's parameter keys (by its id) in a `gp.run` line's `"params"`, or the ids a value takes:
/// the tools for a `gp.run` line's `"tool"`, the commands for a `task.addStep` line's
/// `"command"`; or what the project holds for a value: the map's layers for a `"layer"` (and the
/// tables, for a tool's parameter that takes any layer), the fields of a layer (by its id) for a
/// `"field"`, of a standalone table (by its id) for one a `layer.geocode` line names, maps,
/// layouts, tables or a layer's label classes; the fields of a tool's input (by the name its line
/// gives) for a tool's field parameter, a tool's parameter's choices; or what an expression box
/// offers in the string of a key that takes an expression (#1240).
#[derive(Clone, Debug, PartialEq)]
pub enum Of {
    CommandKeys(String),
    SubKeys { id: String, key: String },
    ToolKeys(String),
    ToolIds,
    CommandIds,
    Layers,
    Fields(u32),
    TableFields(u32),
    Maps,
    Layouts,
    Tables,
    Classes(u32),
    ToolFields(String),
    Choices,
    Expression(Box<InExpression>),
}

impl Of {
    /// Whether its items are a value's (written as a JSON string's text), not keys.
    fn is_value(&self) -> bool {
        matches!(
            self,
            Of::ToolIds
                | Of::CommandIds
                | Of::Layers
                | Of::Fields(_)
                | Of::TableFields(_)
                | Of::Maps
                | Of::Layouts
                | Of::Tables
                | Of::Classes(_)
                | Of::ToolFields(_)
                | Of::Choices
        )
    }
}

/// The ids that complete the command id being typed at char `at` of `script`: the cursor at the
/// end of a line's first word (comments aside), at least `MIN_TYPED` characters typed. `ids` are
/// the commands a script can run. Case aside, an id equal to the word isn't offered again.
#[cfg(test)]
pub fn offer(script: &str, at: usize, ids: &[String]) -> Option<Offer> {
    let (start, word) = word_at(script, at, false)?;
    complete(start, at, &word, ids)
}

/// The command id being typed at char `at` of `script`, and where it starts (a char index): the
/// cursor at the end of a line's first word (comments aside), at least `MIN_TYPED` characters;
/// when `asked` (Ctrl+Space), any word, empty on a blank line. It reads the cursor's line only,
/// so working an offer out costs no more for a long script.
pub fn word_at(script: &str, at: usize, asked: bool) -> Option<(usize, String)> {
    let (before, after) = script.split_at(byte_at(script, at)?);
    // The cursor ends the word: no word character right after it.
    if after.chars().next().is_some_and(|c| !c.is_whitespace()) {
        return None;
    }
    let line = before.rsplit('\n').next().unwrap_or_default();
    let word = line.trim_start();
    let fewest = if asked { 0 } else { MIN_TYPED };
    if word.chars().count() < fewest || word.chars().any(char::is_whitespace) || word.starts_with('#') {
        return None;
    }
    Some((at - word.chars().count(), word.to_string()))
}

/// The command whose parameters a cursor is in (`hint_at`): its line's start and its `{` (char
/// indexes), and its id.
#[derive(Clone, Debug, PartialEq)]
pub struct Hint {
    pub line: usize,
    pub brace: usize,
    pub id: String,
}

/// The command whose parameters the cursor at char `at` is in: the cursor inside its line's
/// `{…}` (quoted text aside: a `}` in a string doesn't close it). None outside the braces, or on
/// a line with no id.
pub fn hint_at(script: &str, at: usize) -> Option<Hint> {
    let before = script.get(..byte_at(script, at)?)?;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let line = before.get(line_start..)?;
    let id = crate::scriptcolor::line_id(line)?;
    let brace = line.find('{')?;
    let (mut depth, mut q) = (0usize, Quotes::default());
    for c in line.get(brace..)?.chars() {
        if !q.read(c) {
            continue;
        }
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    let chars = |s: Option<&str>| s.map_or(0, |s| s.chars().count());
    let line_chars = chars(before.get(..line_start));
    (depth > 0).then(|| Hint { line: line_chars, brace: line_chars + chars(line.get(..brace)), id: id.to_string() })
}

/// The ids in `ids` (the commands a script can run, `script_ids`) that complete `word`, typed
/// from char `start` to `end`, case aside; the id typed whole isn't offered again. All of them,
/// sorted: the list shows ten at a time and scrolls (`completion`).
fn complete(start: usize, end: usize, word: &str, ids: &[String]) -> Option<Offer> {
    let mut items: Vec<String> = ids.iter().filter(|id| starts_with_ci(id, word) && !id.eq_ignore_ascii_case(word)).cloned().collect();
    items.sort();
    (!items.is_empty()).then_some(Offer { start, end, items, of: None })
}

/// Whether `text` starts with `word`, case aside (ASCII: ids and keys).
fn starts_with_ci(text: &str, word: &str) -> bool {
    text.get(..word.len()).is_some_and(|p| p.eq_ignore_ascii_case(word))
}

/// Where a line's object leaves off at its end (`text`, from its `{`): where a quoted key being
/// typed starts (after its quote, a char index of `text`; None when the end isn't in one), the
/// second-level object the end is in (`nested`), where a top-level quoted value being typed
/// starts, with its key and whether it's right in that key's array (`"fields": ["a", "|`), and
/// the top-level quoted values read whole, each with its key and whether in its array, in order.
struct Scan {
    start: Option<usize>,
    nested: Option<Nested>,
    value: Option<(usize, String, bool)>,
    values: Vec<(String, String, bool)>,
    // Top-level strings and numeric table IDs, in order; command value_of stays string-only.
    inputs: Vec<(String, serde_json::Value)>,
}

/// A second-level object: the value of a top-level key (`"add": {…}`), or one of the objects in
/// its array (`"filters": [{…}, {…}]`). Its `{` (a char index) and that key.
#[derive(Clone, Debug, PartialEq)]
pub struct Nested {
    pub brace: usize,
    pub key: String,
}

/// Reads `text`, a line's object from its `{`, to its end: a key starts an entry right after a
/// `{` or a `,`, at the top level or at the top of a second-level object; not in a value, nor
/// deeper. A top-level value follows its key and colon.
fn scan(text: &str) -> Scan {
    let chars: Vec<char> = text.chars().collect();
    let (mut depth, mut q, mut entry, mut start) = (0usize, Quotes::default(), false, None);
    // The top-level key just read, the one whose colon was just read (its value comes next),
    // where the key being read starts, and the top-level value being read.
    let (mut named, mut valued, mut from, mut value) = (None::<String>, None::<String>, None, None::<(usize, String, bool)>);
    // The key whose array the text is in (an array of objects), and the second-level object the
    // text is in, with its depth: anywhere in it, deeper levels included.
    let (mut array, mut nested) = (None::<String>, None::<(Nested, usize)>);
    let mut values = Vec::new();
    let mut inputs = Vec::new();
    for (i, &c) in chars.iter().enumerate() {
        let was_quoted = q.quoted();
        q.read(c);
        if was_quoted {
            // Its closing quote.
            if !q.quoted() {
                if let Some((v, key, listed)) = value.take() {
                    let text: String = chars.get(v..i).unwrap_or_default().iter().collect();
                    if !listed {
                        inputs.push((key.clone(), serde_json::Value::String(text.clone())));
                    }
                    values.push((key, text, listed));
                }
                start = None;
                named = from.take().and_then(|f| chars.get(f..i)).map(|k| k.iter().collect());
            }
            continue;
        }
        let top = depth == 1 || nested.as_ref().is_some_and(|(_, d)| *d == depth);
        match c {
            // An opening quote.
            '"' => {
                start = (top && entry).then_some(i + 1);
                from = (depth == 1 && entry).then_some(i + 1);
                // A top-level key's value, or one right in its array.
                value = match (valued.take(), &array) {
                    (Some(key), _) if depth == 1 => Some((i + 1, key, false)),
                    (_, Some(key)) if depth == 2 => Some((i + 1, key.clone(), true)),
                    _ => None,
                };
                entry = false;
            }
            '{' | '[' | '(' => {
                depth += 1;
                // A top-level key's object or array, or an object right in that array.
                let key = match (c, depth) {
                    ('{', 2) => valued.clone(),
                    ('{', 3) => array.clone(),
                    _ => None,
                };
                if c == '[' && depth == 2 {
                    array = valued.clone();
                }
                if let Some(key) = key {
                    nested = Some((Nested { brace: i, key }, depth));
                }
                entry = c == '{' && (depth == 1 || nested.as_ref().is_some_and(|(n, _)| n.brace == i));
            }
            '}' | ']' | ')' => {
                depth = depth.saturating_sub(1);
                if nested.as_ref().is_some_and(|(_, d)| depth < *d) {
                    nested = None;
                }
                if depth < 2 {
                    array = None;
                }
                entry = false;
            }
            ',' => entry = top,
            ':' => (valued, entry) = (named.take().filter(|_| depth == 1), false),
            c if c.is_whitespace() => {}
            _ => {
                if depth == 1
                    && c.is_ascii_digit()
                    && let Some(key) = valued.as_ref()
                {
                    let token: String =
                        chars.get(i..).unwrap_or_default().iter().take_while(|c| !c.is_whitespace() && !matches!(c, ',' | '}')).collect();
                    if let Ok(id) = serde_json::from_str::<u32>(&token) {
                        inputs.push((key.clone(), serde_json::json!(id)));
                    }
                }
                entry = false;
            }
        }
        // A key's name lasts up to its colon, and from there up to its value.
        if !c.is_whitespace() {
            if c != '"' {
                named = None;
            }
            if c != ':' {
                valued = None;
            }
        }
    }
    Scan { start, nested: nested.map(|(n, _)| n), value, values, inputs }
}

/// The line's object from its `{` up to the cursor at char `at` of `script`, read (`scan`), with
/// the command's line (`hint_at`); None when what follows the cursor doesn't end a word.
fn scan_at(script: &str, at: usize) -> Option<(Hint, Scan, &str)> {
    let h = hint_at(script, at)?;
    let text = script.get(byte_at(script, h.brace)?..byte_at(script, at)?)?;
    let next = script.get(byte_at(script, at)?..)?.chars().next();
    if next.is_some_and(|c| !c.is_whitespace() && !matches!(c, '"' | ',' | '}')) {
        return None;
    }
    let scan = scan(text);
    Some((h, scan, text))
}

/// The word of `text` from char `start` to its end, when it's one: no space, no escape.
fn word_from(text: &str, start: usize) -> Option<String> {
    let word: String = text.chars().skip(start).collect();
    (!word.chars().any(|c| c.is_whitespace() || c == '\\')).then_some(word)
}

/// A key being typed (`key_at`): the command's line, where the word starts (after its quote, a
/// char index of the script), the word, and the second-level object it's in (its `{` a char
/// index of the script; None at the top level).
#[derive(Clone, Debug, PartialEq)]
pub struct KeyAt {
    pub hint: Hint,
    pub start: usize,
    pub word: String,
    pub nested: Option<Nested>,
}

/// The key being typed at char `at` of `script`: the cursor in a quoted word that starts an entry
/// of its line's object, right after its `{` or a `,`, at the top level or at the top of a
/// second-level object (not a value, not deeper).
pub fn key_at(script: &str, at: usize) -> Option<KeyAt> {
    let (hint, Scan { start, nested, .. }, text) = scan_at(script, at)?;
    let start = start?;
    let word = word_from(text, start)?;
    let nested = nested.map(|n| Nested { brace: hint.brace + n.brace, key: n.key });
    Some(KeyAt { start: hint.brace + start, word, nested, hint })
}

/// A top-level value being typed (`value_at`): the command's line, where the word starts (after
/// its quote, a char index of the script), the word, its key, and whether it's in that key's array.
#[derive(Clone, Debug, PartialEq)]
pub struct ValueAt {
    pub hint: Hint,
    pub start: usize,
    pub word: String,
    pub key: String,
    pub listed: bool,
}

/// The top-level value being typed at char `at` of `script`: the cursor in a quoted word right
/// after a key and its colon, or right in that key's array.
pub fn value_at(script: &str, at: usize) -> Option<ValueAt> {
    let (hint, Scan { value, .. }, text) = scan_at(script, at)?;
    let (start, key, listed) = value?;
    let word = word_from(text, start)?;
    Some(ValueAt { start: hint.brace + start, word, key, listed, hint })
}

/// The top-level key of the second-level object of line `h`'s object that the cursor at char
/// `at` of `script` is in, anywhere in it (deeper levels included); None outside one.
fn nested_at(script: &str, at: usize, h: &Hint) -> Option<String> {
    scan(script.get(byte_at(script, h.brace)?..byte_at(script, at)?)?).nested.map(|n| n.key)
}

/// The top-level `key` of a line's object (`line`, from its `{`), as a string, its escapes read
/// (`"Big \"cities\""` as `Big "cities"`): the tool a `gp.run` line runs, the command a
/// `task.addStep` line adds, before or after its `"params"`, or a layer's or table's name; the
/// last one given, as Run reads it. None without one, or when it isn't a string or its escapes
/// aren't JSON's.
pub fn value_of(line: &str, key: &str) -> Option<String> {
    let (_, raw, _) = scan(line).values.into_iter().rev().find(|(k, _, listed)| k == key && !listed)?;
    text_of(&raw)
}

/// What a string's text (`raw`, between its quotes, as written) holds, its escapes read; None
/// for one that isn't JSON's.
fn text_of(raw: &str) -> Option<String> {
    unescape(&raw.chars().collect::<Vec<_>>()).map(|(text, _)| text)
}

/// The rows a tool's input (`input`, as `scan` reads it) names: a layer or a table by its name,
/// escapes read, or a standalone table by its id; with the fields' `Of`.
fn input_data(session: &rustgis_engine::Session, input: serde_json::Value) -> Option<(&rustgis_engine::data::Dataset, Of)> {
    match input {
        serde_json::Value::String(raw) => {
            let name = text_of(&raw)?;
            Some((crate::panes::input_dataset(session, &name)?, Of::ToolFields(name)))
        }
        serde_json::Value::Number(id) => {
            let id = u32::try_from(id.as_u64()?).ok()?;
            Some((&session.project.table(id)?.dataset, Of::TableFields(id)))
        }
        _ => None,
    }
}

/// Line `h`'s object, from its `{` to the line's end.
fn line_of<'a>(script: &'a str, h: &Hint) -> Option<&'a str> {
    script.get(byte_at(script, h.brace)?..)?.split('\n').next()
}

/// The keys of the parameters `desc` describes, entry by entry, each with its alternatives
/// (`"lon" | "x"`), and the entry's text as described.
fn entries(desc: &str) -> Vec<(Vec<String>, String)> {
    let params = rustgis_engine::param_list(desc).unwrap_or_default();
    params
        .iter()
        .map(|p| {
            let entry = format!("{{{}}}", p.text);
            let keys = rustgis_engine::param_keys(&entry).filter(|k| !k.is_empty()).unwrap_or(vec![p.key]);
            (keys.into_iter().map(str::to_string).collect(), p.text.to_string())
        })
        .collect()
}

/// The keys of command `id`'s parameters (`desc`) that complete `word`, typed from char `start`
/// to `end`, case aside, in the order described; an entry already given on the line (`line`, from
/// its `{`: a script's command is one line) isn't offered, nor its alternatives. `line` comes
/// without the word being typed, so that key, closed or not, doesn't count as given (a key already
/// closed, `"lat|": 5`, can still be changed), while the same key given elsewhere does.
fn complete_keys(id: &str, desc: &str, line: &str, start: usize, end: usize, word: &str) -> Option<Offer> {
    let entries = entries(desc).into_iter().map(|(keys, _)| keys).collect();
    offer_keys(Of::CommandKeys(id.to_string()), entries, line, start, end, word)
}

/// The keys of `entries` (each entry's alternatives together) that complete `word`, as
/// `complete_keys` offers them; `line` is the object they go in, from its `{`.
fn offer_keys(of: Of, entries: Vec<Vec<String>>, line: &str, start: usize, end: usize, word: &str) -> Option<Offer> {
    let given = rustgis_engine::param_keys(line).unwrap_or_default();
    let items: Vec<String> = entries
        .into_iter()
        .filter(|keys| !keys.iter().any(|k| given.contains(&k.as_str())))
        .flatten()
        .filter(|k| starts_with_ci(k, word))
        .collect();
    (!items.is_empty()).then_some(Offer { start, end, items, of: Some(of) })
}

/// What a command's naming key gives (`named`): a geoprocessing tool, or a command.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Named {
    Tool,
    Command,
}

/// The top-level key whose value names what command `id`'s `"params": {…}` hold the parameters
/// of (`gp.run`'s `"tool"`, `task.addStep`'s `"command"`), and what it names; None for a command
/// without one.
fn named(id: &str) -> Option<(&'static str, Named)> {
    match id {
        "gp.run" => Some(("tool", Named::Tool)),
        "task.addStep" => Some(("command", Named::Command)),
        _ => None,
    }
}

/// What line `h`'s `"params"` hold the parameters of: the tool or command its naming key gives
/// (`named`), as written there.
fn named_in(script: &str, h: &Hint) -> Option<(Named, String)> {
    let (key, what) = named(&h.id)?;
    Some((what, value_of(line_of(script, h)?, key)?))
}

/// The layer a line's object (`line`, from its `{`) acts on, as Run finds it: by its `"layer"`
/// (a name, escapes read, or an id written as text), the active layer without one. None for a
/// layer that isn't there, or one given otherwise (a number): not the active layer then.
fn line_layer<'a>(session: &'a rustgis_engine::Session, line: &str) -> Option<&'a rustgis_engine::map::Layer> {
    let given = rustgis_engine::param_keys(line).is_some_and(|keys| keys.contains(&"layer"));
    let p = match value_of(line, "layer") {
        Some(name) => serde_json::json!({ "layer": name }),
        None if given => return None,
        None => serde_json::json!({}),
    };
    session.map().layer(session.layer_id(&p).ok()?)
}

/// The rows command `id`'s line (`line`, from its `{`) reads: a standalone table's, where the
/// line gives the `"table"` the command reads instead (`layer.geocode`, `edit.calculateField`; by
/// its name, as `line_layer` reads a layer), else the line's layer's; with the fields' `Of`.
fn line_data<'a>(session: &'a rustgis_engine::Session, id: &str, line: &str) -> Option<(&'a rustgis_engine::data::Dataset, Of)> {
    let table = takes(session, id, "table") && rustgis_engine::param_keys(line).is_some_and(|keys| keys.contains(&"table"));
    if table {
        let t = crate::panes::input_table(session, &value_of(line, "table")?)?;
        return Some((&t.dataset, Of::TableFields(t.id)));
    }
    let l = line_layer(session, line)?;
    Some((&l.dataset, Of::Fields(l.id)))
}

/// The rows whose fields a list of `of` offers: a layer's, a standalone table's, or a tool's
/// input's (by its name). None for another list.
fn data_of<'a>(session: &'a rustgis_engine::Session, of: &Of) -> Option<&'a rustgis_engine::data::Dataset> {
    match of {
        Of::Fields(id) => Some(&session.map().layer(*id)?.dataset),
        Of::TableFields(id) => Some(&session.project.tables.iter().find(|t| t.id == *id)?.dataset),
        Of::ToolFields(input) => crate::panes::input_dataset(session, input),
        _ => None,
    }
}

/// The text of a JSON string from its first char (`raw`, after its opening quote) to its closing
/// quote, or the line's end while it's being typed: what it holds, escapes read (`\"` as `"`), and
/// where in `raw` each of its chars starts, its end last. None for an escape that isn't JSON's or
/// isn't finished (`\` just typed), or a `\u` that names no char.
fn unescape(raw: &[char]) -> Option<(String, Vec<usize>)> {
    let (mut text, mut at, mut i) = (String::new(), Vec::new(), 0);
    // Four hex digits, and nothing else (`from_str_radix` would take a sign).
    let hex = |i: usize| -> Option<u32> {
        let digits = raw.get(i..i + 4)?;
        digits.iter().try_fold(0, |n, c| Some(n * 16 + c.to_digit(16)?))
    };
    while let Some(&c) = raw.get(i) {
        if c == '"' || c == '\n' {
            break;
        }
        at.push(i);
        if c != '\\' {
            text.push(c);
            i += 1;
            continue;
        }
        let (c, len) = match raw.get(i + 1)? {
            '"' => ('"', 2),
            '\\' => ('\\', 2),
            '/' => ('/', 2),
            'b' => ('\u{8}', 2),
            'f' => ('\u{c}', 2),
            'n' => ('\n', 2),
            'r' => ('\r', 2),
            't' => ('\t', 2),
            'u' => {
                let high = hex(i + 2)?;
                // A char past the first plane is written as two: its high and low surrogates.
                if (0xD800..0xDC00).contains(&high) && raw.get(i + 6..i + 8) == Some(&['\\', 'u']) {
                    let low = hex(i + 8).filter(|low| (0xDC00..0xE000).contains(low))?;
                    (char::from_u32(0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00))?, 12)
                } else {
                    (char::from_u32(high)?, 6)
                }
            }
            _ => return None,
        };
        text.push(c);
        i += len;
    }
    at.push(i);
    Some((text, at))
}

/// An expression being typed in a JSON string (`expression_at`): what the expression box's list
/// offers there (`exprcomplete`), where the string's text starts (a char index of the script),
/// and the layer's or table's fields it names (an `Of` of fields), for their help.
#[derive(Clone, Debug, PartialEq)]
pub struct InExpression {
    offer: crate::exprcomplete::Offer,
    text: usize,
    data: Option<Of>,
}

/// An expression the cursor is in (`expression_in`): where its string's text starts (a char
/// index of the script), what it holds, escapes read, and where each of its chars starts in the
/// script from there (`unescape`), the cursor in its chars, and the layer's or table's rows it's
/// over, if any.
struct ExpressionRead<'a> {
    start: usize,
    text: String,
    places: Vec<usize>,
    cursor: usize,
    data: Option<(&'a rustgis_engine::data::Dataset, Of)>,
}

impl ExpressionRead<'_> {
    /// What the expression's reading says goes at its cursor (`query::complete_at`), given to
    /// `then` with the fields it's over: none (the language's own words) without rows.
    fn read<T>(
        &self,
        then: impl FnOnce(rustgis_engine::data::query::Completion, &rustgis_engine::data::Dataset) -> Option<T>,
    ) -> Option<T> {
        let none = rustgis_engine::data::Dataset::new("", rustgis_engine::data::GeometryType::Polygon, Vec::new());
        let ds = self.data.as_ref().map_or(&none, |(d, _)| d);
        then(rustgis_engine::data::query::complete_at(&self.text, self.cursor, ds)?, ds)
    }
}

/// What an expression box offers (#1240) for the expression being typed at char `at` of
/// `script` (`expression_in`); with no word typed, only when `asked` (Ctrl+Space), as the box.
fn expression_at(script: &str, at: usize, session: &rustgis_engine::Session, asked: bool) -> Option<Offer> {
    let e = expression_in(script, at, &hint_at(script, at)?, session)?;
    let offer = e.read(|completion, ds| crate::exprcomplete::offer_from(&completion, &e.text, ds, asked))?;
    let items = offer.items().to_vec();
    let from = e.start + e.places.get(offer.start())?;
    let of = Of::Expression(Box::new(InExpression { offer, text: e.start, data: e.data.map(|(_, of)| of) }));
    Some(Offer { start: from, end: at, items, of: Some(of) })
}

/// The call of a function the cursor at char `at` of `script` is in, in an expression in line
/// `h` (`expression_in`), the innermost, and where its `(` is (a char index of the script): its
/// signature floats there (`hint`), as in an expression box.
fn call_at(script: &str, at: usize, h: &Hint, session: &rustgis_engine::Session) -> Option<(rustgis_engine::data::query::Call, usize)> {
    let e = expression_in(script, at, h, session)?;
    let call = e.read(|completion, _| completion.call)?;
    Some((call, e.start + e.places.get(call.open)?))
}

/// The expression the cursor at char `at` of `script` is in (#1240), in line `h`: in the string of a
/// command's key that takes one (its type `sql` or `expression`, `rustgis_engine::expression_keys`),
/// of a `gp.run` line's tool's parameter that does (`ParamKind::Expression`), or of a
/// `task.addStep` line's command's key that does, in its `"params"`; anywhere in it. Over the
/// fields of the line's (or the step's) layer or table (`line_data`), or of the tool's input;
/// without one, the language's own words.
fn expression_in<'a>(script: &str, at: usize, h: &Hint, session: &'a rustgis_engine::Session) -> Option<ExpressionRead<'a>> {
    let line = line_of(script, h)?;
    let before = scan(script.get(byte_at(script, h.brace)?..byte_at(script, at)?)?);
    let (start, data) = match before.value {
        Some((start, key, false)) => {
            if !takes_expression(session, &h.id, &key)? {
                return None;
            }
            (h.brace + start, line_data(session, &h.id, line))
        }
        Some(_) => return None,
        // In a `gp.run` or `task.addStep` line's `"params"`.
        None => params_expression_at(script, at, h, before.nested.filter(|n| n.key == "params")?.brace, session)?,
    };
    // From the string's text to its line's end, where `unescape` stops anyway: not the whole script.
    let raw: Vec<char> = script.get(byte_at(script, start)?..)?.split('\n').next()?.chars().collect();
    let (text, places) = unescape(&raw)?;
    // The cursor in the middle of an escape (`\|"`) is in no char of the text.
    let cursor = places.iter().position(|p| start + p == at)?;
    Some(ExpressionRead { start, text, places, cursor, data })
}

/// Where the expression being typed at char `at` of `script` starts (a char index), in the
/// `"params"` of line `h` (`gp.run`, `task.addStep`) whose `{` is char `brace` of its object: the
/// string of a parameter of kind `Expression` of the tool it runs, over the rows of the tool's
/// input given before or after it, as for its fields; or of a key of the command the step adds
/// that takes one, over the step's layer or table, read from its `"params"` as from a line. With
/// no rows to read (no input, a tool with no layer parameter), none: the language's own words.
fn params_expression_at<'a>(
    script: &str,
    at: usize,
    h: &Hint,
    brace: usize,
    session: &'a rustgis_engine::Session,
) -> Option<(usize, Option<(&'a rustgis_engine::data::Dataset, Of)>)> {
    let line = script.get(byte_at(script, h.brace + brace)?..)?.split('\n').next()?;
    let (before, after) = line.split_at(byte_at(line, at.checked_sub(h.brace + brace)?)?);
    let (start, key, false) = scan(before).value? else {
        return None;
    };
    // The `"params"` object alone, up to its `}`: an object after it on the line (a later key's)
    // isn't read as giving the layer.
    let params = format!("{before}{}", object_rest(after, false));
    let params = params.as_str();
    let data = match named_in(script, h)? {
        (Named::Tool, name) => {
            let tool = rustgis_engine::analysis::tool(&name)?;
            tool.params.iter().find(|p| p.name == key && p.kind == rustgis_engine::analysis::ParamKind::Expression)?;
            let source = crate::panes::layer_for_param(tool, &key).map(|p| p.name);
            let input = source.and_then(|source| scan(params).inputs.into_iter().rev().find(|(k, _)| k == source));
            input.and_then(|(_, input)| input_data(session, input))
        }
        (Named::Command, name) => {
            if !takes_expression(session, &name, &key)? {
                return None;
            }
            line_data(session, &name, params)
        }
    };
    Some((h.brace + brace + start, data))
}

/// Whether command `id`'s `key` takes an expression (`rustgis_engine::expression_keys`); None for
/// a command that isn't there.
fn takes_expression(session: &rustgis_engine::Session, id: &str, key: &str) -> Option<bool> {
    Some(rustgis_engine::expression_keys(&params_of(session, id)?).contains(&key))
}

/// `script` with `item` of the expression's list accepted (`exprcomplete::written`), and where the
/// cursor goes (chars). Only what it changes is written again, escaped as a JSON string's text
/// (`"Pop Est"` as `\"Pop Est\"`): the rest of the string keeps its spelling. None when the
/// expression box wouldn't take it.
fn expression_written(script: &str, e: &InExpression, item: &str) -> Option<(String, usize)> {
    let chars: Vec<char> = script.chars().collect();
    let (old, places) = unescape(chars.get(e.text..)?)?;
    let (new, cursor) = crate::exprcomplete::written(&old, &e.offer, item)?;
    let (old, new): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let same = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let tail = old.get(same..)?.iter().rev().zip(new.get(same..)?.iter().rev()).take_while(|(a, b)| a == b).count();
    let (old_tail, new_tail) = (old.len() - tail, new.len() - tail);
    let put = json_text(&new.get(same..new_tail)?.iter().collect::<String>());
    let (head, rest) = (*places.get(same)?, *places.get(old_tail)?);
    let mut out: String = chars.get(..e.text + head)?.iter().collect();
    out.push_str(&put);
    out.extend(chars.get(e.text + rest..)?);
    // The cursor: in the text kept before, in what's put, or in the text kept after.
    let to = if cursor <= same {
        *places.get(cursor)?
    } else if cursor <= new_tail {
        head + json_text(&new.get(same..cursor)?.iter().collect::<String>()).chars().count()
    } else {
        head + put.chars().count() + places.get(old_tail + cursor - new_tail)? - rest
    };
    Some((out, e.text + to))
}

/// What to offer at char `at` of `script`: the command ids for a line's first word, the keys of
/// the line's command for a key being typed in its `{…}`, and in its second-level objects the
/// keys their entries describe; in a `gp.run` line the tool ids for its `"tool"` and the tool's
/// keys and values (`tool_value`) in its `"params"`, in a `task.addStep` line the command ids for
/// its `"command"` and that command's keys and values (`step_value`) in its `"params"`; in the
/// string of a key that takes an expression, what an expression box offers there
/// (`expression_at`). When `asked` (Ctrl+Space), a line's first word and an expression's word are
/// offered with nothing typed yet; keys and values are anyway.
fn offer_at(script: &str, at: usize, ids: &[String], session: &rustgis_engine::Session, asked: bool) -> Option<Offer> {
    if let Some((start, word)) = word_at(script, at, asked) {
        return complete(start, at, &word, ids);
    }
    if let Some(offer) = expression_at(script, at, session, asked) {
        return Some(offer);
    }
    if let Some(ValueAt { hint, start, word, key, listed }) = value_at(script, at) {
        if let Some((_, what)) = named(&hint.id).filter(|(names, _)| !listed && key == *names) {
            return match what {
                Named::Tool => value_ids(start, at, &word, rustgis_engine::analysis::tools().map(|t| t.id), Of::ToolIds),
                // Those `task.addStep` takes, by its own rule (not a script's: it adds the step, it
                // doesn't run it).
                Named::Command => {
                    let steps =
                        session.registry.commands.iter().map(|c| c.id).filter(|id| rustgis_engine::cmd::items::can_be_step(session, id));
                    value_ids(start, at, &word, steps, Of::CommandIds)
                }
            };
        }
        // The line's object without the word being typed, as `complete_keys` reads it.
        let after = object_rest(script.get(byte_at(script, at)?..)?.split('\n').next()?, false);
        let without = format!("{}{after}", script.get(byte_at(script, hint.brace)?..byte_at(script, start)?)?);
        let value = TypedValue { start, end: at, word: &word, key: &key, listed };
        return command_value(session, &hint.id, line_of(script, &hint)?, &without, value);
    }
    if let Some(offer) = step_value(script, at, session) {
        return Some(offer);
    }
    if let Some(offer) = tool_value(script, at, session) {
        return Some(offer);
    }
    let KeyAt { hint: h, start, word, nested } = key_at(script, at)?;
    let after = object_rest(script.get(byte_at(script, at)?..)?.split('\n').next()?, false);
    // The object the key goes in, from its `{` to its `}`, the word being typed taken out.
    let brace = nested.as_ref().map_or(h.brace, |n| n.brace);
    let object = format!("{}{after}", script.get(byte_at(script, brace)?..byte_at(script, start)?)?);
    let Some(nested) = nested else {
        return complete_keys(&h.id, &params_of(session, &h.id)?, &object, start, at, &word);
    };
    if nested.key == "params" && named(&h.id).is_some() {
        return match named_in(script, &h)? {
            (Named::Tool, name) => {
                let tool = rustgis_engine::analysis::tool(&name)?;
                let entries = tool.params.iter().map(|p| vec![p.name.to_string()]).collect();
                offer_keys(Of::ToolKeys(tool.id.to_string()), entries, &object, start, at, &word)
            }
            (Named::Command, name) => complete_keys(&name, &params_of(session, &name)?, &object, start, at, &word),
        };
    }
    let entries = entries(&sub_desc(&params_of(session, &h.id)?, &nested.key)?).into_iter().map(|(keys, _)| keys).collect();
    offer_keys(Of::SubKeys { id: h.id, key: nested.key }, entries, &object, start, at, &word)
}

/// A value being typed in a command's object (`value_at`, `step_value`): where its word starts
/// and ends (char indexes of the script), the word, its key, and whether it's in that key's array.
struct TypedValue<'a> {
    start: usize,
    end: usize,
    word: &'a str,
    key: &'a str,
    listed: bool,
}

/// What command `id` takes for the value being typed (`v`) in its object (`object`, from its `{`;
/// `without`, the same without the word being typed): the layers for a `"layer"`, the maps, the
/// layouts, the tables, the layer's label classes, or the fields of the object's layer or table
/// (`line_data`), as for a line of that command or a step that adds it (#1240).
fn command_value(session: &rustgis_engine::Session, id: &str, object: &str, without: &str, v: TypedValue) -> Option<Offer> {
    let TypedValue { start, end, word, key, listed } = v;
    // In an array only a key's whose type is a list of fields (#846): another's, a layer's
    // or a tool's, is turned down by Run.
    let field = field_type(session, id, key);
    if listed && !field.is_some_and(|(_, list)| list) {
        return None;
    }
    // Only where the command takes that key: a value for one Run turns down is no help.
    if !takes(session, id, key) {
        return None;
    }
    match key {
        "layer" => {
            // Those Run finds by their name (`Map::layer_by_name`): of two named alike, case
            // aside, the first.
            let map = session.map();
            let names = map.layers.iter().filter(|l| map.layer_by_name(&l.name).is_some_and(|f| f.id == l.id)).map(|l| l.name.as_str());
            value_ids(start, end, word, names, Of::Layers)
        }
        "map" => {
            let mut seen = std::collections::HashSet::new();
            let names = session.project.maps.iter().map(|m| m.name.as_str()).filter(|n| seen.insert(n.to_ascii_lowercase()));
            value_ids(start, end, word, names, Of::Maps)
        }
        "layout" => {
            let mut seen = std::collections::HashSet::new();
            let names = session.project.layouts.iter().map(|l| l.name.as_str()).filter(|n| seen.insert(n.to_ascii_lowercase()));
            value_ids(start, end, word, names, Of::Layouts)
        }
        "table" => {
            // Exact duplicates are removed by value_ids; distinct case variants each
            // resolve to their own table, since find_table prefers an exact name.
            let names = session.project.tables.iter().map(|t| t.name.as_str());
            value_ids(start, end, word, names, Of::Tables)
        }
        "class" => {
            let l = line_layer(session, object)?;
            let without_default = matches!(id, "labels.class.remove" | "labels.class.move");
            let mut seen = std::collections::HashSet::new();
            let names = l
                .labels
                .in_order()
                .filter(|c| !without_default || !c.name.eq_ignore_ascii_case(&l.labels.default.name))
                .map(|c| c.name.as_str())
                .filter(|n| seen.insert(n.to_ascii_lowercase()));
            value_ids(start, end, word, names, Of::Classes(l.id))
        }
        // A `"field"`, and the keys whose type is a field or fields (#846) in a command that
        // takes a `"layer"`: another's are those of the layers it names otherwise
        // (`data.relate`'s `"origin"`), or of a report's.
        key if key == "field" || (field.is_some() && takes(session, id, "layer")) => {
            let numeric = field.is_some_and(|(numeric, _)| numeric);
            let (d, of) = line_data(session, id, object)?;
            // Not those its array already gives, before or after it: twice is the same work twice.
            let given = scan(without).values;
            let names = d
                .fields
                .iter()
                .filter(|f| {
                    (!numeric || f.ty.is_numeric())
                        && !given.iter().any(|(k, v, _)| k == key && text_of(v).as_deref() == Some(f.name.as_str()))
                })
                .map(|f| f.name.as_str());
            value_ids(start, end, word, names, of)
        }
        _ => None,
    }
}

/// What a `task.addStep` line's command takes for the value being typed in its `"params"`
/// (#1240), as in a line of that command (`command_value`): over the `"params"` object alone, up
/// to its `}`, as `params_expression_at` reads it, so the layer or table is the step's, not the
/// line's.
fn step_value(script: &str, at: usize, session: &rustgis_engine::Session) -> Option<Offer> {
    let (h, Scan { nested, .. }, _) = scan_at(script, at)?;
    let brace = h.brace + nested.filter(|n| n.key == "params")?.brace;
    let (Named::Command, name) = named_in(script, &h)? else {
        return None;
    };
    let line = script.get(byte_at(script, brace)?..)?.split('\n').next()?;
    let (before, after) = line.split_at(byte_at(line, at.checked_sub(brace)?)?);
    let (start, key, listed) = scan(before).value?;
    let word = word_from(before, start)?;
    // To the `}` of `"params"`, past the array the word is in: a later key (`"layer"`) still counts.
    let rest = object_rest(after, listed);
    let params = format!("{before}{rest}");
    let without = format!("{}{rest}", before.chars().take(start).collect::<String>());
    let value = TypedValue { start: brace + start, end: at, word: &word, key: &key, listed };
    command_value(session, &name, &params, &without, value)
}

/// What a `gp.run` line's tool takes for the value being typed in its `"params"` (#788), by the
/// kind of the tool's parameter of that key, as the tool's form offers it (`takes_layer`,
/// `takes_field`): the layers of its kind, then the tables for one that takes any layer; the
/// fields it takes of the layer or table named by the parameter whose fields it lists (the
/// input `fieldlists::input` gives for its kind, else `panes::layer_for_param`, before or after
/// it); or its choices. None for another kind: a number, a bool, a path, a text, several layers in
/// one string.
fn tool_value(script: &str, at: usize, session: &rustgis_engine::Session) -> Option<Offer> {
    use crate::panes::{takes_field, takes_layer};
    use rustgis_engine::analysis::ParamKind as K;
    let (h, Scan { nested, .. }, _) = scan_at(script, at)?;
    let brace = h.brace + nested.filter(|n| n.key == "params")?.brace;
    let (Named::Tool, name) = named_in(script, &h)? else {
        return None;
    };
    let tool = rustgis_engine::analysis::tool(&name)?;
    // The `"params"` object, read as a line's object from its `{`.
    let text = script.get(byte_at(script, brace)?..byte_at(script, at)?)?;
    // Field lists accept a single name as well as names within an array.
    let (start, key, in_array) = scan(text).value?;
    let p = tool.params.iter().find(|p| p.name == key)?;
    if in_array && !matches!(p.kind, K::Fields { .. }) {
        return None;
    }
    // A text list of field names may hold spaces after its separators.
    let word = if p.kind == K::FieldNames {
        let word: String = text.chars().skip(start).collect();
        if word.contains('\\') {
            return None;
        }
        word
    } else {
        word_from(text, start)?
    };
    let start = brace + start;
    let map = session.map();
    match p.kind {
        K::Layer | K::PolygonLayer | K::PointLayer | K::RasterLayer | K::VectorLayer => {
            // Those Run finds by their name, as for a `"layer"`; then the tables no layer's name
            // hides, as the form lists them, of two named alike the one Run finds.
            let layers = map.layers.iter().filter(|l| map.layer_by_name(&l.name).is_some_and(|f| f.id == l.id) && takes_layer(p.kind, l));
            let all = &session.project.tables;
            let tables = all.iter().enumerate().filter(|(i, t)| {
                p.kind == K::Layer && map.layer_by_name(&t.name).is_none() && rustgis_engine::project::find_table(all, &t.name) == Some(*i)
            });
            let names = layers.map(|l| l.name.as_str()).chain(tables.map(|(_, t)| t.name.as_str()));
            value_ids(start, at, &word, names, Of::Layers)
        }
        K::Field | K::FieldNames | K::NumericField | K::CoordinateField | K::Fields { .. } => {
            let after = object_rest(script.get(byte_at(script, at)?..)?.split('\n').next()?, in_array);
            let source = crate::fieldlists::input(p.kind).or_else(|| crate::panes::layer_for_param(tool, p.name).map(|p| p.name))?;
            let whole = format!("{text}{after}");
            let input = scan(&whole).inputs.into_iter().rev().find(|(key, _)| key == source)?.1;
            let (d, of) = input_data(session, input)?;
            let without_word = format!("{}{after}", script.get(byte_at(script, brace)?..byte_at(script, start)?)?);
            // Scan retains raw JSON string contents; decode before comparing with dataset names.
            let given: std::collections::HashSet<String> = scan(&without_word)
                .values
                .into_iter()
                .filter(|(k, _, listed)| *listed && k == &key)
                .filter_map(|(_, value, _)| serde_json::from_str::<String>(&format!("\"{value}\"")).ok())
                .map(|name| name.trim().to_ascii_lowercase())
                .collect();
            let names = d
                .fields
                .iter()
                .enumerate()
                .filter(|(i, f)| takes_field(p.kind, d, *i) && (!in_array || !given.contains(&f.name.to_ascii_lowercase())))
                .map(|(_, f)| f.name.as_str());
            let (start, word) = if p.kind == K::FieldNames {
                let (off, suffix) = list_suffix(&word);
                (start + off, suffix)
            } else {
                (start, word)
            };
            value_ids(start, at, &word, names, of)
        }
        K::Choice(choices) => value_ids(start, at, &word, choices.iter().copied(), Of::Choices),
        _ => None,
    }
}

/// The name a `Fields` completion replaces: the list's last entry, after any `,`/`;` and the
/// spaces following it, with the character offset it begins at, so earlier entries stay typed.
fn list_suffix(word: &str) -> (usize, String) {
    let cut = word.char_indices().filter(|(_, c)| matches!(c, ',' | ';')).map(|(i, c)| i + c.len_utf8()).next_back().unwrap_or(0);
    let (head, tail) = word.split_at(cut);
    let spaces = tail.len() - tail.trim_start().len();
    (head.chars().count() + tail[..spaces].chars().count(), tail.trim_start().to_string())
}

/// What follows a cursor in a key (`after`, to the line's end) up to the `}` that closes the key's
/// object, or all of it when nothing closes it: an object after it on the line (the next one in
/// its array, a later key's) isn't read as holding its keys.
fn object_rest(after: &str, in_array: bool) -> &str {
    // Inside quotes in the object, and optionally its field-name array.
    let (mut depth, mut q) = (if in_array { 2usize } else { 1usize }, Quotes::in_string());
    for (i, c) in after.char_indices() {
        if !q.read(c) {
            continue;
        }
        match c {
            '{' | '[' | '(' => depth += 1,
            '}' | ']' | ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return after.get(..i + c.len_utf8()).unwrap_or(after);
                }
            }
            _ => {}
        }
    }
    after
}

/// The description of the second-level object entry `key` of parameter description `desc` takes:
/// its `{…}`, or the `{…}` in its `[…]` (`"filters": [{"name", "where"}]`). None for an entry
/// that doesn't describe one.
fn sub_desc(desc: &str, key: &str) -> Option<String> {
    let (_, text) = entries(desc).into_iter().find(|(keys, _)| keys.iter().any(|k| k == key))?;
    let value = text.split_once(':')?.1.trim_start();
    let value = value.strip_prefix('[').map_or(value, str::trim_start);
    value.starts_with('{').then(|| value.to_string())
}

/// The ids of `ids` that complete `word`, typed from char `start` to `end`, case aside, each
/// once: the tools, commands, layers, tables, fields or choices a value takes (`of`), offered
/// from its quote, all of them (the list scrolls). Commands sorted, as a line's
/// first word offers them; the rest in their order: tools with the built-in ones first (an
/// external one with the same id isn't the one Run finds), layers top first (then tables), fields
/// as the layer's table has them, choices as the tool lists them.
fn value_ids<'a>(start: usize, end: usize, word: &str, ids: impl Iterator<Item = &'a str>, of: Of) -> Option<Offer> {
    let mut seen = std::collections::HashSet::new();
    let mut items: Vec<String> = ids.filter(|id| starts_with_ci(id, word) && seen.insert(*id)).map(str::to_string).collect();
    if of == Of::CommandIds {
        items.sort();
    }
    (!items.is_empty()).then_some(Offer { start, end, items, of: Some(of) })
}

/// What the help beside a list says about item `item` of `of`: for a key, the line describing its
/// parameter (`"size"?: number`) and a longer help under it (a tool's, when it has one); for a
/// layer, what it holds, for a table its rows; for a field, its type and its alias.
fn item_help(session: &rustgis_engine::Session, of: &Of, item: &str) -> Option<(String, Option<String>)> {
    let described = |desc: &str| entries(desc).into_iter().find(|(keys, _)| keys.iter().any(|k| k == item)).map(|(_, text)| (text, None));
    let field = |d: &rustgis_engine::data::Dataset| {
        d.fields.iter().find(|f| f.name == item).map(|f| (tl!(f.ty.label()).to_string(), f.alias.clone()))
    };
    match of {
        Of::CommandKeys(id) => described(&params_of(session, id)?),
        Of::SubKeys { id, key: entry } => described(&sub_desc(&params_of(session, id)?, entry)?),
        Of::ToolKeys(id) => {
            let p = rustgis_engine::analysis::tool(id)?.params.iter().find(|p| p.name == item)?;
            Some((param_line(p), (!p.help.is_empty()).then(|| tl!(p.help).to_string())))
        }
        Of::Layers => {
            let Some(l) = session.map().layer_by_name(item) else {
                // A table, after the layers (a tool's parameter that takes any layer).
                let t = crate::panes::input_table(session, item)?;
                return Some((crate::i18n::fmt("Table · {n} rows", &[("n", &t.dataset.features.len().to_string())]), None));
            };
            if l.is_raster() {
                return Some((tl!("Raster").to_string(), None));
            }
            let text = crate::i18n::fmt(
                "{type} · {n} features · {fields} fields",
                &[
                    ("type", tl_raw!(l.geometry_type().label())),
                    ("n", &l.dataset.features.len().to_string()),
                    ("fields", &l.dataset.fields.len().to_string()),
                ],
            );
            Some((text, None))
        }
        Of::Fields(_) | Of::TableFields(_) | Of::ToolFields(_) => field(data_of(session, of)?),
        Of::Maps => {
            let m = session.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(item))?;
            Some((crate::i18n::fmt("{n} layers", &[("n", &m.layers.len().to_string())]), None))
        }
        Of::Layouts => {
            let l = session.project.layouts.iter().find(|l| l.name.eq_ignore_ascii_case(item))?;
            Some((crate::i18n::fmt("{width} × {height} mm", &[("width", &l.width.to_string()), ("height", &l.height.to_string())]), None))
        }
        Of::Tables => {
            let t = session.project.tables.get(rustgis_engine::project::find_table(&session.project.tables, item)?)?;
            Some((crate::i18n::fmt("{n} rows", &[("n", &t.features.len().to_string())]), None))
        }
        Of::Classes(id) => {
            let labels = &session.map().layer(*id)?.labels;
            let c = if labels.default.name.eq_ignore_ascii_case(item) { &labels.default } else { labels.class(item)? };
            Some((c.where_.clone().filter(|w| !w.trim().is_empty()).unwrap_or_else(|| tl!("All features").to_string()), None))
        }
        // An expression's items: the expression box's own help (`show`).
        Of::ToolIds | Of::CommandIds | Of::Choices | Of::Expression(_) => None,
    }
}

/// The icon beside item `item` of a list of `of`: a layer's kind, as the Layers pane shows it, or
/// a field's; none for ids and keys.
fn item_icon(session: &rustgis_engine::Session, of: Option<&Of>, item: &str) -> Option<&'static str> {
    use rustgis_engine::data::GeometryType;
    match of? {
        Of::Layers => {
            let Some(l) = session.map().layer_by_name(item) else {
                return crate::panes::input_table(session, item).map(|_| "table");
            };
            Some(match l.geometry_type() {
                _ if l.is_raster() => "raster_layer",
                GeometryType::Point => "point_layer",
                GeometryType::Polyline => "line_layer",
                _ => "polygon_layer",
            })
        }
        Of::Fields(_) | Of::TableFields(_) | Of::ToolFields(_) => Some("field"),
        Of::Expression(e) => crate::exprcomplete::names_field(&e.offer, item).then_some("field"),
        Of::Maps => Some("map"),
        Of::Layouts => Some("new_layout"),
        Of::Tables => Some("table"),
        Of::Classes(_) => Some("label"),
        _ => None,
    }
}

/// `text` as it goes between a JSON string's quotes (`Mi "capa"` as `Mi \"capa\"`).
fn json_text(text: &str) -> String {
    let quoted = serde_json::to_string(text).unwrap_or_default();
    quoted.get(1..quoted.len().saturating_sub(1)).unwrap_or_default().to_string()
}

/// A tool's parameter as the help shows it, as a command's parameters are described: its key
/// (`?` when it may be left out), what it takes, its name and its default
/// (`"units"?: "Kilometers" | "Miles" — Units, default "Kilometers"`).
fn param_line(p: &rustgis_engine::analysis::ParamSpec) -> String {
    use rustgis_engine::analysis::ParamKind as K;
    // In logical order (`tl_raw!`): `fmt` puts the finished line in display form.
    let choices;
    let takes = match p.kind {
        K::Layer => tl_raw!("layer"),
        K::PolygonLayer => tl_raw!("polygon layer"),
        K::PointLayer => tl_raw!("point layer"),
        K::RasterLayer => tl_raw!("raster layer"),
        K::VectorLayer => tl_raw!("vector layer"),
        K::RasterLayers => tl_raw!("raster layers (names separated by ;)"),
        K::VectorLayers => tl_raw!("vector layers (names separated by ;)"),
        K::Field => tl_raw!("a field of the associated input"),
        K::FieldNames => tl_raw!("field names (separated by commas)"),
        K::NumericField => tl_raw!("a numeric field of the associated input"),
        K::CoordinateField => tl_raw!("a field of the associated input holding coordinates"),
        K::Fields { .. } => tl_raw!("ordered field names"),
        K::FieldStatistics { .. } => tl_raw!("field and statistic pairs"),
        K::Number => tl_raw!("number"),
        K::Text => tl_raw!("text"),
        K::Expression => tl_raw!("expression"),
        K::Bool => tl_raw!("bool"),
        K::Path => tl_raw!("file path"),
        K::SaveFile { .. } if cfg!(target_arch = "wasm32") => tl_raw!("File name"),
        K::SaveFile { .. } => tl_raw!("file path"),
        K::Choice(options) => {
            choices = options.iter().map(|o| format!("\"{o}\"")).collect::<Vec<_>>().join(" | ");
            choices.as_str()
        }
    };
    let key = format!("\"{}\"{}", p.name, if p.required { "" } else { "?" });
    let args = [("key", key.as_str()), ("takes", takes), ("label", tl_raw!(p.label)), ("default", p.default)];
    if p.default.is_empty() {
        crate::i18n::fmt("{key}: {takes} — {label}", &args)
    } else {
        crate::i18n::fmt("{key}: {takes} — {label}, default {default}", &args)
    }
}

/// The help for tool `t`, as `help` gives a command's: its signature, name, summary, parameters
/// one per line (`param_line`), and its toolbox.
fn tool_help(t: &rustgis_engine::analysis::ToolSpec) -> Help {
    Help {
        id: t.id.to_string(),
        keys: signature(t.params.iter().map(|p| (p.name, !p.required))),
        label: tl!(t.name).to_string(),
        about: tl!(t.summary).to_string(),
        params: t.params.iter().map(param_line).collect(),
        location: crate::i18n::location(t.toolbox),
    }
}

/// The help by the cursor at char `at` of `script`, in line `h`'s braces: in a `gp.run` line's
/// `"params"`, the tool's it runs, in a `task.addStep` line's, the command's it adds (when the
/// line names one there is); elsewhere the line's command's.
fn help_at(session: &rustgis_engine::Session, script: &str, at: usize, h: &Hint) -> Option<Help> {
    let inner = || match named_in(script, h).filter(|_| nested_at(script, at, h).as_deref() == Some("params"))? {
        (Named::Tool, name) => rustgis_engine::analysis::tool(&name).map(tool_help),
        (Named::Command, name) => help(session, &name),
    };
    inner().or_else(|| help(session, &h.id))
}

/// `script_ids`, kept until the loaded plugins or the session's commands change (the built-in
/// commands don't while the app runs; their count tells another session's registry apart).
fn script_ids_cached(ui: &Ui, session: &rustgis_engine::Session) -> std::sync::Arc<Vec<String>> {
    type Kept = ((u64, usize), std::sync::Arc<Vec<String>>);
    let key = Id::unique("script_complete_ids");
    let now = (rustgis_engine::plugins::revision(), session.registry.commands.len());
    if let Some((was, ids)) = ui.data(|d| d.get_temp::<Kept>(key))
        && was == now
    {
        return ids;
    }
    let ids = std::sync::Arc::new(script_ids(session));
    ui.data_mut(|d| d.insert_temp::<Kept>(key, (now, ids.clone())));
    ids
}

/// The ids a script can run: the built-in commands and enabled plugins' (`plugin:…`), less those a
/// script refuses.
pub fn script_ids(session: &rustgis_engine::Session) -> Vec<String> {
    let builtin = session.registry.commands.iter().map(|c| c.id.to_string());
    let plugins = rustgis_engine::plugins::commands().into_iter().map(|c| c.id);
    builtin.chain(plugins).filter(|id| !refused_in_script(id)).collect()
}

/// What the help beside the list says about a command (#662), most useful first: its signature
/// (required keys bare, optional ones with `?`), its name and help (a plugin's command has a help
/// text; a built-in one, its label), its parameters one per line as described, and last where it
/// sits in the interface.
#[derive(Clone, Debug, PartialEq)]
pub struct Help {
    pub id: String,
    /// The keys after the id, as the signature reads (`labels.style` then ` {layer?, size?}`);
    /// empty for a command with none.
    pub keys: String,
    pub label: String,
    pub about: String,
    pub params: Vec<String>,
    pub location: String,
}

impl Help {
    /// What the help says, for a screen reader: the command's name and what it does (the
    /// signature and parameters left out, as code to read).
    fn spoken(&self) -> String {
        crate::completion::spoken(&self.label, Some(&self.about))
    }
}

/// The help for command `id`, as Command Search names and places it (translated); None for an id
/// no command has.
pub fn help(session: &rustgis_engine::Session, id: &str) -> Option<Help> {
    let lang = crate::i18n::current().code();
    let (label, about, location, desc) = match session.registry.get(id) {
        Some(c) => (tl!(c.label).to_string(), String::new(), crate::i18n::location(c.location), c.params.to_string()),
        None => {
            let c = rustgis_engine::plugins::command(id)?;
            let m = &c.plugin.manifest;
            let location = format!("{} › {}", tl!("Plugins"), m.translate(lang, &m.name));
            (m.translate(lang, &c.decl.label).to_string(), m.translate(lang, &c.decl.help).to_string(), location, c.decl.params.clone())
        }
    };
    let params = rustgis_engine::param_list(&desc).unwrap_or_default();
    let keys = signature(params.iter().map(|p| (p.key, p.optional)));
    Some(Help { id: id.to_string(), keys, label, about, params: params.iter().map(|p| p.text.to_string()).collect(), location })
}

/// The keys after an id in a help's signature, each with `?` when it may be left out
/// (` {layer?, size?}`); empty for none.
fn signature<'a>(keys: impl Iterator<Item = (&'a str, bool)>) -> String {
    let names: Vec<String> = keys.map(|(key, optional)| format!("{key}{}", if optional { "?" } else { "" })).collect();
    if names.is_empty() { String::new() } else { format!(" {{{}}}", names.join(", ")) }
}

/// The parameter description of command `id`: a built-in command's, or an enabled plugin's.
fn params_of(session: &rustgis_engine::Session, id: &str) -> Option<String> {
    match session.registry.get(id) {
        Some(c) => Some(c.params.to_string()),
        None => rustgis_engine::plugins::command(id).map(|c| c.decl.params),
    }
}

/// Whether command `id`'s parameter description names top-level key `key`.
fn takes(session: &rustgis_engine::Session, id: &str, key: &str) -> bool {
    params_of(session, id).is_some_and(|desc| rustgis_engine::param_keys(&desc).is_some_and(|keys| keys.contains(&key)))
}

/// Whether key `key` of command `id` takes fields, as its description's type says: `field`,
/// `[fields]`, `[field, …]`, `field | [fields]`…; with whether only numeric ones
/// (`[numeric fields]`) and whether in a list (a `[`). None for any other type
/// (`[related fields]`, `[names]`).
fn field_type(session: &rustgis_engine::Session, id: &str, key: &str) -> Option<(bool, bool)> {
    let (_, text) = entries(&params_of(session, id)?).into_iter().find(|(keys, _)| keys.iter().any(|k| k == key))?;
    entry_field_type(&text)
}

/// [`field_type`] read from a parameter's entry (`"fields"?: [numeric fields]`): its type, up to
/// a note in parentheses (`field (metres)`), so a note's brackets aren't read as a list.
fn entry_field_type(entry: &str) -> Option<(bool, bool)> {
    let ty = entry.split_once(':')?.1.split('(').next()?.trim_start();
    let first = ty.trim_start_matches('[');
    let numeric = first.starts_with("numeric field");
    (numeric || first.starts_with("field")).then_some((numeric, ty.contains('[')))
}

/// What accepting a command whose parameters `desc` describes puts after its id, and where the
/// cursor goes in it (chars): its required keys, each with its value to type, the cursor at the
/// first (` {"lon": |, "lat": }`); empty braces when every key may be left out (` {|}`); nothing
/// for a command with no parameters, or a description that isn't an object (no keys to start
/// from).
pub fn template(desc: &str) -> (String, usize) {
    let params = rustgis_engine::param_list(desc).unwrap_or_default();
    if params.is_empty() {
        return (String::new(), 0);
    }
    let required: Vec<String> = params.iter().filter(|p| !p.optional).map(|p| format!("\"{}\": ", p.key)).collect();
    let Some(first) = required.first() else { return (" {}".to_string(), 2) };
    // After ` {` and the first key.
    let at = 2 + first.chars().count();
    (format!(" {{{}}}", required.join(", ")), at)
}

/// Draws a key's parameter `text` beside the list, coloured as the script is, and its longer help
/// (`about`, may be empty) under it.
fn draw_key_help(ui: &mut Ui, text: &str, about: Option<&str>, width: f32) {
    let palette = crate::scriptcolor::Palette::of(&crate::theme::Tokens::get(ui.ctx()));
    let mut job = crate::scriptcolor::help_job(None, text, palette, &egui::TextStyle::Monospace.resolve(ui.style()));
    job.wrap.max_width = width;
    ui.vertical(|ui| {
        ui.set_max_width(width);
        ui.label(job);
        if let Some(about) = about {
            ui.label(egui::RichText::new(about).small());
        }
    });
}

/// Draws `help` beside the list: the signature and parameters coloured as the script is, in its
/// monospace font (smaller for the parameters).
fn draw_help(ui: &mut Ui, help: &Help, width: f32) {
    let palette = crate::scriptcolor::Palette::of(&crate::theme::Tokens::get(ui.ctx()));
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let small = egui::FontId::monospace(mono.size * 0.85);
    let coloured = |id: Option<&str>, text: &str, font: &egui::FontId| {
        let mut job = crate::scriptcolor::help_job(id, text, palette, font);
        job.wrap.max_width = width;
        job
    };
    ui.vertical(|ui| {
        ui.set_max_width(width);
        ui.label(coloured(Some(&help.id), &help.keys, &mono));
        ui.label(&help.label);
        if !help.about.is_empty() {
            ui.label(egui::RichText::new(&help.about).small());
        }
        if !help.params.is_empty() {
            ui.add_space(4.0);
            for p in &help.params {
                ui.label(coloured(None, p, &small));
            }
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new(&help.location).small().weak());
    });
}

/// The list's state between frames, kept in one place.
#[derive(Clone, Default)]
struct State {
    /// The list shown, its keys and how it's drawn (`completion`).
    list: List<Offer>,
    /// What was worked out to offer, and for which cursor, script (its hash: one replaced from
    /// outside the box, as long as before, still tells), plugins (their revision: a plugin
    /// turned off mustn't stay on offer, to be accepted) and project (the session's revision and
    /// active layer: the layers and fields offered). While none changes (the cursor
    /// blinking, the pointer moving) it isn't worked out again. Kept whether the list shows or not.
    /// The hash is taken each frame the box has focus: the length alone misses a script replaced by
    /// one as long, and a change made by code comes with no input event to wait for. It reads the
    /// text once, as egui's own layout already does each frame (the layouter's cache key, the
    /// galley cache), so it adds a constant, not a new order of cost. And whether it was asked for.
    worked: Option<Worked<(u64, u64, Option<u32>, bool), Option<Offer>>>,
    /// Where Ctrl+Space asked for the list: it shows there with nothing typed.
    asked: Asked,
    /// The help by the cursor (`hint`).
    hint: HintState,
}

impl Offered for Offer {
    fn items(&self) -> &[String] {
        &self.items
    }

    fn end(&self) -> usize {
        self.end
    }

    /// A list of choices (a parameter's allowed values) has no help (`item_help`); every other
    /// list's items have one.
    fn has_help(&self) -> bool {
        !matches!(self.of, Some(Of::Choices))
    }
}

/// The help shown by the cursor while it's in a line's `{…}`.
#[derive(Clone, Default)]
struct HintState {
    /// Where the command's help floats, for the command whose braces the cursor is in
    /// (`completion::Tip`).
    tip: Tip<Hint>,
    /// Where a function's signature floats instead, for the call the cursor is in (its `(`, a
    /// char index of the script). Each has what Escape closed of its own: closing one doesn't
    /// close the other, and going through a call doesn't bring back a command's help closed.
    call: Tip<usize>,
    /// What it was worked out to show, for which cursor, script (its hash), language (the help is
    /// translated), plugins' revision (a plugin's command comes and goes with its plugin) and
    /// project (the session's revision and active layer: the fields an expression is read over):
    /// not again each frame the cursor blinks.
    worked: Option<Worked<(&'static str, u64, u64, Option<u32>), Option<Shown>>>,
}

/// What the help by the cursor shows: the help of the command whose braces it's in (a command's
/// or tool's), or the signature of the call of a function it's in, in an expression there, with
/// the command whose braces it's in and where that call's `(` is (a char index of the script).
#[derive(Clone)]
enum Shown {
    Help(Hint, Help),
    Call(Hint, usize, rustgis_engine::data::query::Call),
}

fn load(ui: &Ui, box_id: Id) -> State {
    ui.data(|d| d.get_temp::<State>(box_id.with("complete"))).unwrap_or_default()
}

fn store(ui: &Ui, box_id: Id, state: State) {
    ui.data_mut(|d| d.insert_temp(box_id.with("complete"), state));
}

/// The keys the box keeps while it has focus: code_editor's (the arrows, Tab to indent) and, with
/// the list or the help by the cursor open, Escape, which egui would otherwise take as "drop the
/// focus" before `keys` can read it as "close it".
pub fn filter(ui: &Ui, box_id: Id) -> egui::EventFilter {
    let state = load(ui, box_id);
    egui::EventFilter {
        tab: true,
        horizontal_arrows: true,
        vertical_arrows: true,
        escape: state.list.is_open() || state.hint.tip.is_shown() || state.hint.call.is_shown(),
    }
}

/// Closes the list as Escape does: not shown again until what's typed changes.
pub fn close(ui: &Ui, box_id: Id) {
    let mut state = load(ui, box_id);
    if state.list.close() {
        store(ui, box_id, state);
    }
}

/// Before the box sees the keys: with the box focused, Ctrl+Space asks for the list at the cursor
/// (with nothing typed, or one Escape closed); with the list open, its keys (`List::keys`);
/// with the help by the cursor shown instead, Escape closes it. Returns the item to accept, if
/// any. A list worked out for another text than `script` (changed from outside the box since) is
/// dropped, not acted on: its word could be elsewhere now.
pub fn keys(ui: &Ui, box_id: Id, script: &str) -> Option<(Offer, String)> {
    let mut state = load(ui, box_id);
    if state.asked.keys(ui, box_id, script) {
        state.list.reopen();
        store(ui, box_id, state);
        return None;
    }
    // The help by the cursor (no list open): Escape closes it.
    if !state.list.is_open() && (state.hint.tip.keys(ui, box_id) || state.hint.call.keys(ui, box_id)) {
        store(ui, box_id, state);
        return None;
    }
    if !state.list.is_open() {
        return None;
    }
    if state.worked.as_ref().is_none_or(|w| w.text != egui::util::hash(script)) {
        state.list.forget();
        store(ui, box_id, state);
        return None;
    }
    let picked = state.list.keys(ui, box_id);
    store(ui, box_id, state);
    picked
}

/// After the box: offers for the word at its cursor, in the list `completion` draws just under
/// it, each layer or field after its icon, the help of the item highlighted beside it. `visible`
/// is the part of the box that shows (`List::show`). Returns an item clicked, to accept.
pub fn show(
    ui: &Ui,
    box_id: Id,
    out: &TextEditOutput,
    visible: egui::Rect,
    script: &str,
    session: &rustgis_engine::Session,
) -> Option<(Offer, String)> {
    let mut state = load(ui, box_id);
    let worked = &mut state.worked;
    let asked = &mut state.asked;
    let work = |at: Option<usize>| {
        let text = egui::util::hash(script);
        let here = asked.here(text, at);
        let with = (rustgis_engine::plugins::revision(), session.revision, session.active_layer, here);
        Worked::get(worked, at, text, &with).unwrap_or_else(|| {
            let offer = at.and_then(|at| offer_at(script, at, &script_ids_cached(ui, session), session, here));
            *worked = Some(Worked { at, text, with, value: offer.clone() });
            offer
        })
    };
    // A layer's kind or a field, before its name, as the Layers pane shows it.
    let icon = |offer: &Offer, item: &str| item_icon(session, offer.of.as_ref(), item);
    // Worked out each frame the list shows, for one command or tool: a lookup and a short
    // description split. Kept, it would need the id and the language as its key (the help is
    // translated), for next to nothing saved. It goes in the help's box beside the list, after the
    // gap the list leaves (`completion::rows`).
    let draw = |ui: &mut Ui, offer: &Offer, item: &str, width: f32| match &offer.of {
        // A tool: its help, as a command's.
        Some(Of::ToolIds) => {
            if let Some(t) = rustgis_engine::analysis::tool(item) {
                draw_help(ui, &tool_help(t), width);
            }
        }
        // A command, starting a line or for a value.
        None | Some(Of::CommandIds) => {
            if let Some(help) = help(session, item) {
                draw_help(ui, &help, width);
            }
        }
        // In an expression: as the expression box shows it, a field's over its layer or table.
        Some(Of::Expression(e)) => {
            let ds = e.data.as_ref().and_then(|of| data_of(session, of));
            crate::exprcomplete::draw_item(ui, &e.offer, item, ds, width);
        }
        // A key: the line describing its parameter; a layer or a field: what it holds.
        Some(of) => {
            if let Some((text, about)) = item_help(session, of, item) {
                draw_key_help(ui, &text, about.as_deref(), width);
            }
        }
    };
    // What the help says, for a screen reader (`List::show`): the same sources as `draw`.
    let describe = |offer: &Offer, item: &str| match &offer.of {
        Some(Of::ToolIds) => rustgis_engine::analysis::tool(item).map(|t| tool_help(t).spoken()),
        None | Some(Of::CommandIds) => help(session, item).map(|h| h.spoken()),
        Some(Of::Expression(e)) => {
            let ds = e.data.as_ref().and_then(|of| data_of(session, of));
            crate::exprcomplete::describe_item(&e.offer, item, ds)
        }
        Some(of) => item_help(session, of, item).map(|(text, about)| crate::completion::spoken(&text, about.as_deref())),
    };
    let said = |offer: &Offer| rows_key(session, offer);
    let clicked = state.list.show(ui, box_id.with("complete_list"), out, visible, work, icon, said, describe, draw);
    store(ui, box_id, state);
    clicked
}

/// A key of what the rows of `offer` say is read from (`List::show`'s `said`): the project, as the
/// offer's own key has it (`show`'s `with`: plugins, the session's revision, the active layer), and
/// for a list of layers each one's count of features, which a streamed layer's read for the view
/// changes with no command, so with no revision.
fn rows_key(session: &rustgis_engine::Session, offer: &Offer) -> u64 {
    let mut key = egui::util::hash((rustgis_engine::plugins::revision(), session.revision, session.active_layer));
    if matches!(offer.of, Some(Of::Layers)) {
        for l in &session.map().layers {
            key = egui::util::hash((key, l.id, l.dataset.features.len()));
        }
    }
    key
}

/// After the list: with no list open, the help of the command whose parameters the cursor is in
/// (its line's `{…}`), floating by the cursor, so the signature and parameters stay in view while
/// they're typed; in the call of a function in an expression there (`call_at`), the function's
/// signature instead, its argument at the cursor marked, as an expression box shows it (#1240).
/// Not with text selected, a cursor out of view, the window left, or once Escape closed it (until
/// the cursor leaves those braces, or that call).
pub fn hint(ui: &Ui, box_id: Id, out: &TextEditOutput, visible: egui::Rect, script: &str, session: &rustgis_engine::Session) {
    let mut state = load(ui, box_id);
    let focused = out.response.response.has_focus() && ui.input(|i| i.focused);
    let at = out.cursor_range.filter(|r| focused && !state.list.is_open() && r.is_empty()).map(|r| r.primary.index.0);
    // Its own hash of the script, not the list's: it mustn't depend on how `show` ran this frame.
    let text = egui::util::hash(script);
    // The help is translated, a plugin's command comes and goes with its plugin, and an
    // expression is read over the project's fields.
    let with = (crate::i18n::current().code(), rustgis_engine::plugins::revision(), session.revision, session.active_layer);
    let kept = Worked::get(&state.hint.worked, at, text, &with);
    let found = match kept {
        Some(found) => found,
        None => {
            let found = at.and_then(|at| {
                let h = hint_at(script, at)?;
                if let Some((call, open)) = call_at(script, at, &h, session) {
                    return Some(Shown::Call(h, open, call));
                }
                let help = help_at(session, script, at, &h)?;
                Some(Shown::Help(h, help))
            });
            state.hint.worked = Some(Worked { at, text, with, value: found.clone() });
            found
        }
    };
    let (help, call) = match found {
        Some(Shown::Help(h, help)) => (Some((h, help)), None),
        Some(Shown::Call(h, open, call)) => {
            // In a call its command's help is hidden, not left: what Escape closed of it stays
            // closed while the cursor is still in that command's braces (`Tip::hide`).
            state.hint.tip.hide(Some(&h));
            (None, Some((open, call)))
        }
        None => (None, None),
    };
    // Anchored at the line's `{`, the signature at the call's `(`.
    if call.is_none() {
        let rows = help.as_ref().map_or(0, |(_, help)| help.params.len() + 3);
        let about = help.as_ref().map(|(h, _)| (h.clone(), h.brace));
        state.hint.tip.show(ui, box_id.with("complete_hint"), out, visible, at, about, rows, Side::Below, |ui, width| {
            if let Some((_, help)) = &help {
                draw_help(ui, help, width);
            }
        });
    }
    let about = call.as_ref().map(|(open, _)| (*open, *open));
    let rows = crate::exprcomplete::TIP_ROWS;
    state.hint.call.show(ui, box_id.with("complete_call"), out, visible, at, about, rows, Side::Below, |ui, width| {
        if let Some((_, call)) = &call {
            crate::exprcomplete::draw_call(ui, call, width);
        }
    });
    store(ui, box_id, state);
}

/// Puts `item` in place of the offer's word in `script`, with its parameters' `template` when
/// nothing follows the word on its line (a line that has parameters keeps them: only its id
/// changes), the cursor where the first value goes; as one undo step of the box (its own undo
/// takes the pick back), and gives the box the focus again.
pub fn accept(ui: &Ui, box_id: Id, script: &mut String, offer: &Offer, item: &str, session: &rustgis_engine::Session) {
    let Some((new, cursor)) = accepted(script, offer, item, session) else { return };
    let mut state = TextEditState::load(ui.ctx(), box_id).unwrap_or_default();
    let mut undoer = state.undoer();
    let was = state.cursor.char_range().unwrap_or(CCursorRange::one(CCursor::new(offer.end)));
    undoer.add_undo(&(was, script.clone()));
    state.set_undoer(undoer);
    *script = new;
    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(cursor))));
    state.store(ui.ctx(), box_id);
    ui.memory_mut(|m| m.request_focus(box_id));
    // What was worked out was for the text before the pick.
    let mut complete = load(ui, box_id);
    complete.worked = None;
    store(ui, box_id, complete);
}

/// `script` with `item` of `offer` accepted (`accept`), and where the cursor goes (chars). An
/// expression's item as the expression box puts it (`expression_written`).
fn accepted(script: &str, offer: &Offer, item: &str, session: &rustgis_engine::Session) -> Option<(String, usize)> {
    if let Some(Of::Expression(e)) = &offer.of {
        return expression_written(script, e, item);
    }
    let chars: Vec<char> = script.chars().collect();
    let (before, mut after) = (chars.get(..offer.start)?, chars.get(offer.end..)?);
    let value = offer.of.as_ref().is_some_and(Of::is_value);
    // A value as a JSON string holds it (a layer named `Mi "capa"` as `Mi \"capa\"`).
    let item = if value { json_text(item) } else { item.to_string() };
    let (tail, at) = match &offer.of {
        // A value: closed, the cursor after its quote (one already there is kept).
        Some(_) if value => (if after.first() == Some(&'"') { String::new() } else { "\"".to_string() }, 1),
        // A key: closed, its colon and a space after it, ready for the value; a closing quote
        // already there is taken in, and a key that has its colon keeps it.
        Some(_) => {
            let quoted = after.first() == Some(&'"');
            let colon = quoted && after.get(1..).is_some_and(|a| a.iter().find(|c| !c.is_whitespace()) == Some(&':'));
            if quoted && !colon {
                after = after.get(1..).unwrap_or_default();
            }
            // With its colon, the cursor goes after the quote already there.
            if colon { (String::new(), 1) } else { ("\": ".to_string(), 3) }
        }
        // A command starting a line.
        None if after.iter().take_while(|c| **c != '\n').all(|c| c.is_whitespace()) => {
            params_of(session, &item).map(|d| template(&d)).unwrap_or_default()
        }
        None => Default::default(),
    };
    let mut new: String = before.iter().collect();
    new.push_str(&item);
    new.push_str(&tail);
    new.extend(after);
    Some((new, offer.start + item.chars().count() + at))
}

/// Whether the end of `line` is inside a string.
fn in_string(line: &str) -> bool {
    let mut q = Quotes::default();
    for c in line.chars() {
        q.read(c);
    }
    q.quoted()
}

/// The closing character of an opening brace, bracket or quote.
fn closer(open: char) -> Option<char> {
    match open {
        '{' => Some('}'),
        '[' => Some(']'),
        '"' => Some('"'),
        _ => None,
    }
}

/// The script once the box's edit is followed through, the edit read from `script` as the box
/// left it, what was there `before`, and the cursor `now` (a char index): a `{` or `[` typed
/// closes itself (`{|}`, `[|]`) when nothing, a space or a closing `}`, `]` or `,` follows it; a
/// `"` too (`"|"`), when it also comes where a string can start (a line's start, a space, `{`,
/// `[`, `,` or `:`); a `}` or `]` typed right before the same one steps over it rather than add
/// one, and so does a `"` that ends a string; Backspace in an empty `{|}`, `[|]` or `"|"` deletes
/// both. Not in a string or a comment line, and only for the keys that make those edits
/// (`keyed`): an undo can bring a `{` back as typing one would. The cursor stays where the box put
/// it in each case. None to leave the edit as it is.
fn brace(script: &str, before: Before, now: usize, keyed: Keyed) -> Option<String> {
    let Before { at, prev, len } = before;
    // Outside a comment line and a string, up to char `i`.
    let plain = |i: usize| {
        let line = byte_at(script, i).and_then(|b| script.get(..b)).map(|b| b.rsplit('\n').next().unwrap_or_default());
        line.is_some_and(|l| !l.trim_start().starts_with('#') && !in_string(l))
    };
    let (typed, next) = (char_at(script, at), char_at(script, now));
    let typed_one = keyed == Keyed::Pair && script.len() == len + 1 && now == at + 1;
    let one_deleted = keyed == Keyed::Backspace && script.len() + 1 == len && now + 1 == at && plain(now);
    let cut = byte_at(script, if one_deleted { now } else { at })?;
    let (b, rest) = (script.get(..cut)?, script.get(cut..)?);
    // What may follow an opener that closes itself, and what may come before a quote that does.
    let closes = |c: char| c.is_whitespace() || matches!(c, '}' | ']' | ',');
    let opens = |c: char| c.is_whitespace() || matches!(c, '{' | '[' | ',' | ':');
    match (typed, next) {
        (Some(open @ ('{' | '[')), next) if typed_one && plain(at) && next.is_none_or(closes) => {
            Some(format!("{b}{open}{}{}", closer(open)?, rest.get(1..)?))
        }
        (Some('"'), next) if typed_one && plain(at) && next.is_none_or(closes) && prev.is_none_or(opens) => {
            Some(format!("{b}\"\"{}", rest.get(1..)?))
        }
        // The `}` or `]` typed goes: the cursor, after it, is after the one that was there.
        (Some(close @ ('}' | ']')), Some(next)) if typed_one && plain(at) && close == next => Some(format!("{b}{}", rest.get(1..)?)),
        // So does a `"` that ends a string (in one before it, out of it after): not an escaped one.
        (Some('"'), Some('"')) if typed_one && !plain(at) && plain(now) => Some(format!("{b}{}", rest.get(1..)?)),
        // The closer that followed the deleted opener, now at the cursor.
        (_, Some(next)) if one_deleted && prev.and_then(closer) == Some(next) => Some(format!("{b}{}", rest.get(1..)?)),
        _ => None,
    }
}

/// After the box: its edit followed through as `brace` says, from what was there `before` it
/// took the keys (`completion::pairs`).
pub fn braces(ui: &Ui, before: Option<Before>, out: &TextEditOutput, script: &mut String) {
    pairs(ui, before, out, script, &['{', '}', '[', ']', '"'], brace);
}

#[cfg(test)]
mod tests {
    use super::{Offer, offer};

    /// What `offer_at` offers where Ctrl+Space didn't ask.
    fn unasked(script: &str, at: usize, ids: &[String], session: &rustgis_engine::Session) -> Option<Offer> {
        super::offer_at(script, at, ids, session, false)
    }

    /// The ids as the pane has them (`script_ids`): those a script refuses already left out.
    fn ids() -> Vec<String> {
        ["labels.style", "labels.placement", "labels.class.add", "map.zoomFull", "plugin:demo/hello"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn items(o: Option<Offer>) -> Vec<String> {
        o.map(|o| o.items).unwrap_or_default()
    }

    #[test]
    fn a_lines_first_word_offers_the_ids_it_starts() {
        let s = "map.zoomFull\n  labels.";
        let o = offer(s, s.chars().count(), &ids()).unwrap();
        assert_eq!(o.items, ["labels.class.add", "labels.placement", "labels.style"]);
        assert_eq!((o.start, o.end), (15, 22), "the word, after the indent");
        assert_eq!(items(offer("LABELS.ST", 9, &ids())), ["labels.style"], "case aside");
        assert_eq!(items(offer("plu", 3, &ids())), ["plugin:demo/hello"], "plugins' too");
    }

    #[test]
    fn only_a_lines_first_word_at_the_cursor_is_completed() {
        let none = |s: &str, at: usize| offer(s, at, &ids()).is_none();
        assert!(none("labels.style {\"la", 17), "in the JSON");
        assert!(none("# labels", 8), "in a comment");
        assert!(none("l", 1), "one character");
        assert!(none("labels.st", 4), "the cursor mid-word");
        assert!(none("labels.style", 12), "the id typed whole");
        assert!(none("ui.pa", 5) && none("script.r", 8), "ids a script refuses aren't among them");
        assert!(none("nothing", 7), "no match");
        assert!(none("map", 99), "a cursor past the text");
    }

    #[test]
    fn char_indexes_with_multibyte_text_and_crlf() {
        let s = "# Bogotá 🌎\r\nlabels.pl";
        let at = s.chars().count();
        let o = offer(s, at, &ids()).unwrap();
        assert_eq!(o.items, ["labels.placement"]);
        let start: String = s.chars().skip(o.start).take(o.end - o.start).collect();
        assert_eq!(start, "labels.pl");
    }

    /// Every command that completes a line's first word is offered, not the first ten: the list
    /// shows ten at a time and scrolls to the rest.
    #[test]
    fn every_matching_command_is_offered() {
        let many: Vec<String> = (0..30).map(|i| format!("map.cmd{i:02}")).collect();
        assert_eq!(items(offer("map.cm", 6, &many)), many);
    }

    /// The pane with an empty script, its box focused.
    fn pane() -> egui_kittest::Harness<'static, crate::RustGisApp> {
        use egui_kittest::kittest::Queryable;
        // Without the interface fonts (`theme::install_fonts`) egui's faces have no "→".
        let mut h = egui_kittest::Harness::builder().allow_missing_glyphs().with_size(egui::vec2(500.0, 700.0)).build_ui_state(
            |ui, app: &mut crate::RustGisApp| crate::analysispane::script_pane(app, ui, &crate::theme::Tokens::light()),
            crate::RustGisApp::with_sample(),
        );
        h.run();
        h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
        h.run();
        h
    }

    fn type_in(h: &mut egui_kittest::Harness<'static, crate::RustGisApp>, text: &str) {
        h.event(egui::Event::Text(text.into()));
        h.run();
    }

    /// Typing a line's first word opens the list; Enter accepts the highlighted id, with no line
    /// break, and the list closes.
    #[test]
    fn enter_accepts_the_highlighted_id() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "labels.sty");
        h.get_by_label("labels.style");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, "labels.style {}", "with its parameters' braces");
        assert!(h.query_by_label("labels.style").is_none(), "the list closed");
    }

    /// The highlighted entry is the box's active descendant, with what its help says, as the
    /// highlight moves (#2619): a command its name and what it does, a field in an expression its
    /// type (as the expression box says it).
    #[test]
    fn a_screen_reader_reads_the_highlighted_entry() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let read = |h: &egui_kittest::Harness<'_, crate::RustGisApp>| {
            let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
            node.accesskit_node().active_descendant().map(|row| (row.label().unwrap_or_default(), row.description().unwrap_or_default()))
        };
        let mut h = pane();
        type_in(&mut h, "labels.sty");
        let (label, said) = read(&h).expect("a row named");
        assert_eq!(label, "labels.style");
        assert_eq!(said, super::help(&h.state().session, "labels.style").unwrap().spoken());
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(read(&h), None, "closed: no row named");
        let mut h = pane();
        type_in(&mut h, r#"select.byAttributes {"layer": "Countries", "where": "\"pop_"#);
        let (label, said) = read(&h).expect("a row named");
        assert_eq!(label, "pop_est");
        assert!(!said.is_empty(), "the field's type");
    }

    /// The key of what a list's rows say (`rows_key`): a layer list's moves with a layer's count
    /// of features, read with no command (a streamed layer's view); a command list's doesn't, so its
    /// rows aren't read again; every list's moves with the session's revision (a command run).
    #[test]
    fn a_list_s_rows_are_keyed_by_what_they_read() {
        use super::{Of, Offer, rows_key};
        let mut app = crate::RustGisApp::with_sample();
        let offer = |of: Of| Offer { start: 0, end: 0, items: Vec::new(), of: Some(of) };
        let (layers, commands, tables) = (offer(Of::Layers), offer(Of::CommandIds), offer(Of::Tables));
        let s = &mut app.session;
        let was = [rows_key(s, &layers), rows_key(s, &commands), rows_key(s, &tables)];
        let id = s.map().layer_by_name("Cities").map(|l| l.id);
        if let Some(l) = id.and_then(|id| s.map_mut().layer_mut(id)) {
            l.dataset.features.pop();
        }
        assert_ne!(rows_key(s, &layers), was[0], "a layer's count, with no command");
        assert_eq!(rows_key(s, &commands), was[1], "a command list: no count in its key");
        assert_eq!(rows_key(s, &tables), was[2]);
        s.revision = s.revision.wrapping_add(1);
        assert_ne!(rows_key(s, &tables), was[2], "a command run");
        assert_ne!(rows_key(s, &commands), was[1]);
    }

    /// A layer's count of features changed while its list is open: the row says the new count, as
    /// its help does, after a command and after a read with no command (`rows_key`).
    #[test]
    fn a_row_says_a_layer_s_new_count_while_its_list_is_open() {
        use egui_kittest::kittest::{NodeT, Queryable};
        let said = |h: &egui_kittest::Harness<'_, crate::RustGisApp>| {
            let node = h.get_by_role(egui::accesskit::Role::MultilineTextInput);
            node.accesskit_node().active_descendant().and_then(|row| Some((row.label()?, row.description()?)))
        };
        let count = |h: &egui_kittest::Harness<'_, crate::RustGisApp>| {
            h.state().session.map().layer_by_name("Cities").map_or(0, |l| l.dataset.features.len())
        };
        let mut h = pane();
        type_in(&mut h, r#"labels.style {"layer": "Citi"#);
        let (label, before) = said(&h).expect("a row named");
        assert_eq!(label, "Cities");
        let n = count(&h);
        assert!(before.contains(&format!(" {n} features")), "{before}");
        // Some cities deleted as the app does it, by commands (which move the session's revision).
        let session = &mut h.state_mut().session;
        let revision = session.revision;
        let ran = [
            ("select.byAttributes", serde_json::json!({"layer": "Cities", "where": "pop_max > 10000000"})),
            ("edit.enable", serde_json::json!({"on": true})),
            ("edit.delete", serde_json::json!({})),
        ]
        .into_iter()
        .all(|(id, params)| session.run(id, &params).is_ok());
        assert!(ran && session.revision != revision, "deleted by commands");
        h.run();
        let left = count(&h);
        assert!(left < n, "some deleted: {left} of {n}");
        let (_, after) = said(&h).expect("still open");
        assert!(after.contains(&format!(" {left} features")) && !after.contains(&format!(" {n} features")), "the new count: {after}");
        // A streamed layer's read as the map is drawn: no command, the revision kept.
        let session = &mut h.state_mut().session;
        let id = session.map().layer_by_name("Cities").map(|l| l.id);
        if let Some(l) = id.and_then(|id| session.map_mut().layer_mut(id)) {
            l.dataset.features.pop();
        }
        h.run();
        let (_, read) = said(&h).expect("still open");
        assert!(read.contains(&format!(" {} features", left - 1)), "the count read with the view: {read}");
    }

    /// In an expression's string (#1240) the list holds the line's layer's fields, Enter puts the
    /// one highlighted, a field begun with an escaped quote closed with one, and the list closes.
    #[test]
    fn enter_accepts_a_field_in_an_expression() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        let head = r#"select.byAttributes {"layer": "Countries", "where": "#;
        type_in(&mut h, &format!(r#"{head}"\"pop_"#));
        assert!(h.query_all_by_label("pop_est").next().is_some(), "the field listed (and its help beside)");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, format!(r#"{head}"\"pop_est\""#));
        assert!(h.query_by_label("pop_est").is_none(), "the list closed");
    }

    /// The arrows move the highlight; Tab accepts it too, inserting no tab.
    #[test]
    fn the_arrows_move_and_tab_accepts() {
        let mut h = pane();
        type_in(&mut h, "labels.");
        h.key_press(egui::Key::ArrowDown);
        h.run();
        h.key_press(egui::Key::Tab);
        h.run();
        let session = &h.state().session;
        let second = super::offer("labels.", 7, &super::script_ids(session)).unwrap().items[1].clone();
        let (template, _) = super::template(&super::params_of(session, &second).unwrap());
        assert_eq!(h.state().workflow.script, format!("{second}{template}"), "the second item, and no tab");
    }

    /// Escape closes the list without touching the text, and it stays closed for that word.
    #[test]
    fn escape_closes_the_list_for_that_word() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "labels.sty");
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_label("labels.style").is_none());
        assert_eq!(h.state().workflow.script, "labels.sty");
        h.run();
        assert!(h.query_by_label("labels.style").is_none(), "not opened again");
        type_in(&mut h, "l");
        h.get_by_label("labels.style");
    }

    /// Ctrl+Enter (⌘+Enter) runs the script (#773): no line break, and the box keeps the focus.
    /// The line above the results tells the run's number, so a run made again shows, how long it
    /// took and when it started.
    #[test]
    fn ctrl_enter_runs_the_script() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "map.zoomFull");
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
        h.run();
        let out = &h.state().workflow.script_out;
        assert!(out.iter().any(|(line, err)| !err && line.starts_with("map.zoomFull")), "{out:?}");
        assert_eq!(h.state().workflow.script, "map.zoomFull", "no line break");
        assert!(h.ctx.memory(|m| m.has_focus(egui::Id::unique("script_text"))));
        h.get_by_label_contains("Run #1 (");
        h.get_by_label_contains(" UTC");
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
        h.run();
        h.get_by_label_contains("Run #2 (");
    }

    /// A key held down runs the script once: its repeats are taken (no line break), not run.
    /// Presses with no release between them, as a held key sends: egui marks all but the first
    /// as repeats itself, whatever the event says.
    #[test]
    fn a_held_ctrl_enter_runs_once() {
        let mut h = pane();
        type_in(&mut h, "map.zoomFull");
        for _ in 0..3 {
            h.event(egui::Event::Key {
                key: egui::Key::Enter,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
                physical_key: None,
            });
            h.run();
        }
        assert_eq!(h.state().workflow.script_last.as_ref().map(|r| r.n), Some(1), "the repeats don't run it");
        assert_eq!(h.state().workflow.script, "map.zoomFull", "nor break the line");
    }

    /// With the list open, Ctrl+Enter runs the script and closes the list, as Escape does: it
    /// accepts nothing, and the list isn't opened again for that word.
    #[test]
    fn ctrl_enter_closes_the_list() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "labels.sty");
        h.get_by_label("labels.style");
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
        h.run();
        assert!(h.state().workflow.script_last.is_some(), "it ran");
        assert_eq!(h.state().workflow.script, "labels.sty", "nothing accepted");
        h.run();
        assert!(h.query_by_label("labels.style").is_none(), "closed");
    }

    /// Ctrl+Enter once the box has lost the focus runs nothing.
    #[test]
    fn ctrl_enter_elsewhere_runs_nothing() {
        let mut h = pane();
        type_in(&mut h, "map.zoomFull");
        h.ctx.memory_mut(|m| m.surrender_focus(egui::Id::unique("script_text")));
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
        h.run();
        assert!(h.state().workflow.script_last.is_none());
    }

    /// A click accepts an item, and the box keeps the focus. Pressed and let go in frames of their
    /// own, as a real click is (`click()` does both in one frame, which hid that the press takes
    /// the focus from the box first).
    #[test]
    fn a_click_accepts_an_item() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "map.zoomF");
        let at = h.get_by_role_and_label(egui::accesskit::Role::Button, "map.zoomFull").rect().center();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        h.event(egui::Event::PointerMoved(at));
        h.step();
        h.event(button(true));
        h.step();
        h.step();
        h.event(button(false));
        h.run();
        assert_eq!(h.state().workflow.script, "map.zoomFull");
        assert!(h.get_by_role(egui::accesskit::Role::MultilineTextInput).is_focused());
    }

    /// The box's own undo takes a pick back.
    #[test]
    fn a_pick_is_one_undo_step_of_the_box() {
        let mut h = pane();
        type_in(&mut h, "labels.sty");
        h.key_press(egui::Key::Enter);
        h.run();
        let state = egui::widgets::text_edit::TextEditState::load(&h.ctx, egui::Id::unique("script_text")).unwrap();
        let now = (state.cursor.char_range().unwrap(), h.state().workflow.script.clone());
        let mut undoer = state.undoer();
        let back = undoer.undo(&now).map(|(_, text)| text.clone());
        assert_eq!(back.as_deref(), Some("labels.sty"), "the id and its template together");
    }

    /// The template: the required keys, the cursor at the first one's value; empty braces when
    /// every key may be left out; nothing with no keys.
    #[test]
    fn the_template_starts_at_the_first_required_key() {
        use super::template;
        let go_to = r#"{"lon" | "x": number, "lat" | "y": number, "scale"?: number}"#;
        assert_eq!(template(go_to), (r#" {"lon": , "lat": }"#.to_string(), 9), "every required key, the first of its alternatives");
        assert_eq!(template(r#"{"size"?: number, "field": string}"#), (r#" {"field": }"#.to_string(), 11), "not the first key");
        assert_eq!(template(r#"{"size"?: number}"#), (" {}".to_string(), 2));
        assert_eq!(template("{}"), (String::new(), 0));
        assert_eq!(template("a path"), (String::new(), 0), "a description that isn't an object");
        assert_eq!(template(r#"{"größe": number}"#), (r#" {"größe": }"#.to_string(), 11), "chars, not bytes");
    }

    /// Accepting a command with a required key puts the cursor where its value goes, and the
    /// help by the cursor shows at once; a line further on after multibyte text.
    #[test]
    fn accepting_leaves_the_cursor_at_the_first_value() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "# ñandú\nmap.goToX");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, "# ñandú\nmap.goToXY {\"lon\": , \"lat\": }");
        let help = super::help(&h.state().session, "map.goToXY").unwrap();
        h.get_by_label(&format!("{}{}", help.id, help.keys));
        type_in(&mut h, "2");
        assert_eq!(h.state().workflow.script, "# ñandú\nmap.goToXY {\"lon\": 2, \"lat\": }");
    }

    /// A command with no parameters gets nothing after its id; a line that already has
    /// parameters keeps them, and only its id changes.
    #[test]
    fn a_line_with_parameters_keeps_them() {
        let mut h = pane();
        type_in(&mut h, "map.zoomF");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, "map.zoomFull");
        let mut h = pane();
        type_in(&mut h, r#"map.goTo {"lat": 1}"#);
        for _ in 0..r#" {"lat": 1}"#.len() {
            h.key_press(egui::Key::ArrowLeft);
        }
        type_in(&mut h, "X");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, r#"map.goToXY {"lat": 1}"#);
    }

    /// Moving the cursor, with no text changed, works the list out again: into the word it
    /// closes, back to its end it opens.
    #[test]
    fn moving_the_cursor_opens_and_closes_the_list() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "labels.sty");
        h.get_by_label("labels.style");
        h.key_press(egui::Key::ArrowLeft);
        h.run();
        assert!(h.query_by_label("labels.style").is_none(), "mid-word");
        h.key_press(egui::Key::ArrowRight);
        h.run();
        h.get_by_label("labels.style");
    }

    /// A cursor scrolled out of the box's view keeps no list open: Enter breaks the line rather
    /// than accept a suggestion no one sees.
    #[test]
    fn a_hidden_cursor_keeps_no_list() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        // Pasted at once, the box can't scroll all the way to the cursor on that frame (its scroll
        // is bounded by the content it had before), so one more key is typed, as a person would:
        // that scrolls the box to the cursor with the whole text in it. Scrolls are eased.
        h.event(egui::Event::Text(format!("{}labels.st", "map.zoomFull\n".repeat(200))));
        h.run_steps(60);
        h.event(egui::Event::Text("y".into()));
        h.run_steps(60);
        h.get_by_label("labels.style");
        // Scroll the box back to its top, away from the cursor: the pointer low in the pane, clear
        // of the list and its help above the cursor (which a command with many parameters makes
        // tall), so the wheel scrolls the box rather than the list.
        let at = h.get_by_role(egui::accesskit::Role::MultilineTextInput).rect().center();
        h.event(egui::Event::PointerMoved(egui::pos2(at.x, 640.0)));
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 100_000.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        });
        // egui eases a wheel's scroll over frames, asking for a repaint until it's done.
        h.run_steps(60);
        assert!(h.query_by_label("labels.style").is_none(), "no list for a hidden cursor");
        h.key_press(egui::Key::Enter);
        // The box scrolls back to its cursor, eased too.
        h.run_steps(60);
        assert!(h.state().workflow.script.ends_with("labels.sty\n"), "Enter broke the line");
    }

    /// The ids the pane offers leave out what a script refuses (the one rule, `refused_in_script`).
    #[test]
    fn script_ids_leave_out_what_a_script_refuses() {
        let app = crate::RustGisApp::with_sample();
        let ids = super::script_ids(&app.session);
        assert!(ids.iter().any(|i| i == "map.zoomFull"));
        assert!(!ids.iter().any(|i| rustgis_engine::cmd::workflows::refused_in_script(i)), "no script.*, ui.*, notebook.run");
    }

    /// With text selected there's no list: Enter and Tab act on the selection as usual.
    #[test]
    fn a_selection_shows_no_list() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "labels.sty");
        h.get_by_label("labels.style");
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowLeft);
        h.run();
        assert!(h.query_by_label("labels.style").is_none(), "a selection closes it");
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, "labels.st\n", "Enter replaced the selection");
    }

    /// Leaving the window closes the list, and coming back doesn't open it again until what's
    /// typed changes (the box keeps its focus meanwhile, as egui keeps a widget's).
    #[test]
    fn leaving_the_window_closes_the_list() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "labels.sty");
        h.get_by_label("labels.style");
        // As eframe tells it: `RawInput::focused`, which `input.focused` comes from (the
        // `WindowFocused` event alone doesn't change it).
        h.input_mut().focused = false;
        h.event(egui::Event::WindowFocused(false));
        h.run();
        assert!(h.query_by_label("labels.style").is_none(), "closed while away");
        h.input_mut().focused = true;
        h.event(egui::Event::WindowFocused(true));
        h.run();
        assert!(h.query_by_label("labels.style").is_none(), "not opened again on return");
        type_in(&mut h, "l");
        h.get_by_label("labels.style");
    }

    /// A script replaced from outside the box by one as long, the cursor where it was, is worked
    /// out again: the offer is kept by the text's hash, not its length.
    #[test]
    fn a_script_replaced_as_long_is_worked_out_again() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "labels.sty");
        h.get_by_label("labels.style");
        // Same length, same cursor (10 chars): another prefix, as History's "copy to script" could.
        h.state_mut().workflow.script = "map.zoomFu".into();
        h.run();
        assert!(h.query_by_label("labels.style").is_none(), "the old offer isn't reused");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "map.zoomFull");
    }

    /// A list left open over a script changed from outside the box isn't acted on: Enter doesn't
    /// put an id where the old word was.
    #[test]
    fn keys_drop_a_list_for_another_text() {
        let mut h = pane();
        type_in(&mut h, "labels.sty");
        // Changed under the open list, the box still focused (no code does this today); longer
        // than the old word, so a stale pick could still be spliced in.
        h.state_mut().workflow.script = "# a longer note".into();
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(!h.state().workflow.script.contains("labels.style"), "{:?}", h.state().workflow.script);
    }

    /// The help puts the signature first, then the name, the parameters one per line and the
    /// location last.
    #[test]
    fn help_shows_the_signature_first() {
        let app = crate::RustGisApp::with_sample();
        let h = super::help(&app.session, "labels.style").unwrap();
        assert!(h.keys.starts_with(" {layer?, class?, size?"), "{}", h.keys);
        assert_eq!(h.label, "Text Symbol");
        assert!(h.params.iter().any(|p| p == r#""size"?: number"#), "{:?}", h.params);
        assert_eq!(h.location, "Labeling › Text Symbol");
        // Required keys go bare.
        assert!(super::help(&app.session, "labels.field").unwrap().keys.contains("field}"));
        // No parameters: the id alone.
        let z = super::help(&app.session, "map.zoomFull").unwrap();
        assert_eq!((z.keys.as_str(), z.params.len()), ("", 0));
        assert!(super::help(&app.session, "nope.cmd").is_none());
    }

    /// The help shows beside the list, for the highlighted item, and follows the arrows.
    #[test]
    fn the_help_follows_the_highlight() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "labels.");
        let first = super::offer("labels.", 7, &super::script_ids(&h.state().session)).unwrap().items;
        let sig = |id: &str| super::help(&h.state().session, id).map(|h| format!("{}{}", h.id, h.keys)).unwrap();
        let (one, two) = (sig(&first[0]), sig(&first[1]));
        h.get_by_label(&one);
        h.key_press(egui::Key::ArrowDown);
        h.run();
        assert!(h.query_by_label(&one).is_none());
        h.get_by_label(&two);
    }

    /// With the pointer resting on the list, the arrows still move the help with the highlight;
    /// moving the pointer gives it back to the item under it.
    #[test]
    fn the_last_to_move_leads_the_help() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "labels.");
        let items = super::offer("labels.", 7, &super::script_ids(&h.state().session)).unwrap().items;
        let sig = |h: &egui_kittest::Harness<'static, crate::RustGisApp>, id: &str| {
            super::help(&h.state().session, id).map(|x| format!("{}{}", x.id, x.keys)).unwrap()
        };
        // The pointer over the third item: the help follows it.
        let third = h.get_by_role_and_label(egui::accesskit::Role::Button, &items[2]).rect().center();
        // From somewhere first: egui's first pointer position has no delta to move by.
        h.event(egui::Event::PointerMoved(egui::pos2(1.0, 1.0)));
        h.run();
        h.event(egui::Event::PointerMoved(third));
        h.run();
        h.get_by_label(&sig(&h, &items[2]));
        // ↓ with the pointer still: the help follows the highlight (the second item).
        h.key_press(egui::Key::ArrowDown);
        h.run();
        h.get_by_label(&sig(&h, &items[1]));
    }

    /// In a window too narrow for the help beside the list, the help goes under it: neither is
    /// cut off at the edge.
    #[test]
    fn a_narrow_window_puts_the_help_under_the_list() {
        use egui_kittest::kittest::Queryable;
        let width = 380.0;
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(width, 700.0)).build_ui_state(
            |ui, app: &mut crate::RustGisApp| crate::analysispane::script_pane(app, ui, &crate::theme::Tokens::light()),
            crate::RustGisApp::with_sample(),
        );
        h.run();
        h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
        h.run();
        h.event(egui::Event::Text("labels.sty".into()));
        h.run();
        let item = h.get_by_role_and_label(egui::accesskit::Role::Button, "labels.style").rect();
        let help = super::help(&h.state().session, "labels.style").unwrap();
        let sig = h.get_by_label(&format!("{}{}", help.id, help.keys)).rect();
        assert!(sig.min.y > item.max.y, "under the list: {sig:?} {item:?}");
        assert!(sig.max.x <= width + 0.5, "within the window: {sig:?}");
    }

    /// In a short window the list and help scroll within the room by the cursor: the popup stays
    /// on screen, and ↓ to the last item scrolls it into view.
    #[test]
    fn a_short_window_scrolls_the_list() {
        use egui_kittest::kittest::Queryable;
        let (width, height) = (380.0, 300.0);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(width, height)).build_ui_state(
            |ui, app: &mut crate::RustGisApp| crate::analysispane::script_pane(app, ui, &crate::theme::Tokens::light()),
            crate::RustGisApp::with_sample(),
        );
        h.run();
        h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
        h.run();
        h.event(egui::Event::Text("labels.".into()));
        h.run();
        let items = super::offer("labels.", 7, &super::script_ids(&h.state().session)).unwrap().items;
        for _ in 1..items.len() {
            h.key_press(egui::Key::ArrowDown);
            h.run_steps(10);
        }
        let last = h.get_by_role_and_label(egui::accesskit::Role::Button, items.last().unwrap()).rect();
        assert!(last.min.y >= 0.0 && last.max.y <= height, "the last item scrolled into view: {last:?}");
    }

    /// A list that was short once (a single item) grows back for a longer one: egui lays an area
    /// out in last frame's size, and the list's scroll would keep it short, scrolling.
    #[test]
    fn a_short_list_grows_back() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, "labels.sty");
        h.get_by_label("labels.style");
        for _ in 0.."sty".len() {
            h.key_press(egui::Key::Backspace);
            h.run();
        }
        let items = super::offer("labels.", 7, &super::script_ids(&h.state().session)).unwrap().items;
        assert!(items.len() > 3, "a longer list: {items:?}");
        let state = h.ctx.data(|d| d.get_temp::<super::State>(egui::Id::unique("script_text").with("complete"))).unwrap();
        let shown = state.list.drawn().unwrap();
        // The last row the list shows at once (it scrolls past `MAX_ROWS`): one further down would
        // be clipped to the list, its rect no measure of the popup.
        let last = &items[items.len().min(crate::completion::MAX_ROWS) - 1];
        let last = h.get_by_role_and_label(egui::accesskit::Role::Button, last).rect();
        assert!(last.max.y <= shown.max.y, "every row in view, no scroll: {last:?} in {shown:?}");
    }

    /// What `brace` makes of `ins` typed at char `at` of `old`, as the box would leave it.
    fn typed(old: &str, at: usize, ins: &str) -> Option<String> {
        let b = super::byte_at(old, at).unwrap();
        let script = format!("{}{ins}{}", &old[..b], &old[b..]);
        let before = super::Before { at, prev: at.checked_sub(1).and_then(|i| super::char_at(old, i)), len: old.len() };
        super::brace(&script, before, at + ins.chars().count(), super::Keyed::Pair)
    }

    /// What `brace` makes of Backspace at char `at` of `old`.
    fn backspace(old: &str, at: usize) -> Option<String> {
        let (b, e) = (super::byte_at(old, at - 1).unwrap(), super::byte_at(old, at).unwrap());
        let script = format!("{}{}", &old[..b], &old[e..]);
        let before = super::Before { at, prev: super::char_at(old, at - 1), len: old.len() };
        super::brace(&script, before, at - 1, super::Keyed::Backspace)
    }

    /// `{` closes itself before nothing, a space or a closing `}`, `]` or `,`, not before other
    /// text, in a string or in a comment line, nor for a paste.
    #[test]
    fn a_brace_typed_closes_itself() {
        let end = |s: &str| typed(s, s.chars().count(), "{");
        assert_eq!(end("labels.style ").as_deref(), Some("labels.style {}"));
        assert_eq!(end("").as_deref(), Some("{}"));
        assert_eq!(typed(r#"x {"a": }"#, 8, "{").as_deref(), Some(r#"x {"a": {}}"#), "before a closing brace");
        assert_eq!(typed("x [, ]", 3, "{").as_deref(), Some("x [{}, ]"), "before a comma");
        assert_eq!(typed("x  \r\ny", 2, "{").as_deref(), Some("x {} \r\ny"), "before a space");
        assert_eq!(typed("x \r\n", 2, "{").as_deref(), Some("x {}\r\n"), "at a CRLF line's end");
        assert_eq!(typed("x ab", 2, "{"), None, "before other text");
        assert_eq!(end(r#"x {"where": "a "#), None, "in a string");
        assert_eq!(end(r#"x {"where": "a \" "#), None, "an escaped quote doesn't end it");
        assert_eq!(end(r#"x \"a "#), None, "a backslash outside a string escapes nothing (#741)");
        assert_eq!(end("  # x "), None, "in a comment line");
        assert_eq!(end("ñandú ").as_deref(), Some("ñandú {}"), "chars, not bytes");
        assert_eq!(typed("x ", 2, "{{"), None, "more than the brace typed");
        let before = super::Before { at: 2, prev: Some(' '), len: 2 };
        assert_eq!(super::brace("x {", before, 3, super::Keyed::Other), None, "a paste, an undo");
        let before = super::Before { at: 3, prev: Some('{'), len: 4 };
        assert_eq!(super::brace("x }", before, 2, super::Keyed::Other), None, "a cut, an undo");
    }

    /// `[` closes itself as `{` does; `]` steps over a `]`, not a `}` (nor `}` a `]`); Backspace
    /// in an empty `[]` deletes both, not in `[}`.
    #[test]
    fn a_bracket_works_as_a_brace() {
        assert_eq!(typed("x ", 2, "[").as_deref(), Some("x []"));
        assert_eq!(typed(r#"x {"a": }"#, 8, "[").as_deref(), Some(r#"x {"a": []}"#), "before a closing brace");
        assert_eq!(typed("x []", 3, "{").as_deref(), Some("x [{}]"), "a brace in a bracket");
        assert_eq!(typed("x []", 3, "[").as_deref(), Some("x [[]]"), "before a closing bracket");
        assert_eq!(typed("x ab", 2, "["), None, "before other text");
        assert_eq!(typed(r#"x {"w": "a "#, 11, "["), None, "in a string");
        assert_eq!(typed("x [1]", 4, "]").as_deref(), Some("x [1]"), "stepped over");
        assert_eq!(typed("x [1}", 4, "]"), None, "not over a brace");
        assert_eq!(typed("x {1]", 4, "}"), None, "a brace not over a bracket");
        assert_eq!(backspace("x []", 3).as_deref(), Some("x "));
        assert_eq!(backspace("x [}", 3), None, "not a pair");
        assert_eq!(backspace("x {]", 3), None, "not a pair either");
    }

    /// `"` closes itself as `{` does, where a string can start: not after a word, a quote or a
    /// closing brace; one typed before the quote that ends its string steps over it, not an
    /// escaped one; Backspace in an empty `""` deletes both, not after a string's closing quote.
    #[test]
    fn a_quote_works_as_a_brace() {
        assert_eq!(typed("x {}", 3, "\"").as_deref(), Some(r#"x {""}"#), "before a closing brace");
        assert_eq!(typed(r#"x {"a": }"#, 8, "\"").as_deref(), Some(r#"x {"a": ""}"#), "a value");
        assert_eq!(typed(r#"x {"a":}"#, 7, "\"").as_deref(), Some(r#"x {"a":""}"#), "right after a colon");
        assert_eq!(typed("x [1, ]", 6, "\"").as_deref(), Some(r#"x [1, ""]"#), "before a closing bracket");
        assert_eq!(typed("", 0, "\"").as_deref(), Some(r#""""#), "the script's start");
        assert_eq!(typed("x\n", 2, "\"").as_deref(), Some("x\n\"\""), "a line's start");
        assert_eq!(typed("ñ ", 2, "\"").as_deref(), Some(r#"ñ """#), "chars, not bytes");
        assert_eq!(typed("x { : 1}", 4, "\""), None, "before a colon");
        assert_eq!(typed("x ab", 2, "\""), None, "before other text");
        assert_eq!(typed("x ab", 4, "\""), None, "after a word");
        assert_eq!(typed(r#"x """#, 4, "\""), None, "after a quote");
        assert_eq!(typed("x {}", 4, "\""), None, "after a closing brace");
        assert_eq!(typed("# x ", 4, "\""), None, "in a comment line");
        assert_eq!(typed(r#"x {"a"}"#, 5, "\"").as_deref(), Some(r#"x {"a"}"#), "stepped over");
        assert_eq!(typed(r#"x {"a\"}"#, 6, "\""), None, "an escaped quote");
        assert_eq!(typed(r#"x {"ab"#, 5, "\""), None, "in a string, before a letter");
        assert_eq!(typed(r#"# "a""#, 4, "\""), None, "not stepped over in a comment line");
        assert_eq!(backspace(r#"x """#, 3).as_deref(), Some("x "));
        assert_eq!(backspace(r#"x {"a": ""}"#, 9).as_deref(), Some(r#"x {"a": }"#), "an empty value");
        assert_eq!(backspace(r#"x "a"""#, 5), None, "a string's closing quote");
    }

    /// `}` typed right before a `}` steps over it; Backspace in an empty `{}` deletes both.
    #[test]
    fn a_closing_brace_steps_over_and_backspace_deletes_both() {
        assert_eq!(typed(r#"x {"a": 1}"#, 9, "}").as_deref(), Some(r#"x {"a": 1}"#), "stepped over");
        assert_eq!(typed(r#"x {"a": 1"#, 9, "}"), None, "no brace to step over");
        assert_eq!(typed(r#"x {"a": "1}"#, 10, "}"), None, "in a string");
        assert_eq!(backspace("x {}", 3).as_deref(), Some("x "));
        assert_eq!(backspace("ñ {}\r\n", 3).as_deref(), Some("ñ \r\n"), "chars, not bytes");
        assert_eq!(backspace("x {a}", 3), None, "not empty");
        assert_eq!(backspace("x (}", 3), None, "not a brace deleted");
        assert_eq!(backspace(r#"x "{}"#, 4), None, "in a string");
    }

    /// In the pane: `{` closes itself with the cursor inside, `}` steps over, Backspace deletes
    /// both, and the box's undo takes the brace back with what was typed with it.
    #[test]
    fn braces_in_the_box() {
        let mut h = pane();
        type_in(&mut h, "labels.style ");
        type_in(&mut h, "{");
        assert_eq!(h.state().workflow.script, "labels.style {}");
        type_in(&mut h, "}");
        type_in(&mut h, "!");
        assert_eq!(h.state().workflow.script, "labels.style {}!", "the cursor stepped over the brace");
        let mut h = pane();
        type_in(&mut h, "labels.style ");
        type_in(&mut h, "{");
        h.key_press(egui::Key::Backspace);
        h.run();
        assert_eq!(h.state().workflow.script, "labels.style ");
        type_in(&mut h, "{");
        let state = egui::widgets::text_edit::TextEditState::load(&h.ctx, egui::Id::unique("script_text")).unwrap();
        let now = (state.cursor.char_range().unwrap(), h.state().workflow.script.clone());
        let back = state.undoer().undo(&now).map(|(_, text)| text.clone());
        assert!(back.is_some_and(|t| !t.contains('{')), "no lone brace left by undo");
        let mut h = pane();
        h.event(egui::Event::Paste("labels.style {".into()));
        h.run();
        assert_eq!(h.state().workflow.script, "labels.style {", "a paste is left as it is");
        let mut h = pane();
        type_in(&mut h, r#"layer.displayFilters {"filters": "#);
        type_in(&mut h, "[");
        type_in(&mut h, "{");
        assert_eq!(h.state().workflow.script, r#"layer.displayFilters {"filters": [{}]"#, "a bracket closes itself too");
        type_in(&mut h, "}");
        type_in(&mut h, "]");
        type_in(&mut h, "!");
        assert_eq!(h.state().workflow.script, r#"layer.displayFilters {"filters": [{}]!"#, "and is stepped over");
    }

    /// As Wayland types with an input method running (ibus, fcitx): a `{` committed by it, after
    /// an empty composition, closes itself, and its `}` steps over.
    #[test]
    fn braces_from_an_input_method() {
        let mut h = pane();
        type_in(&mut h, "labels.style ");
        h.event(egui::Event::Ime(egui::ImeEvent::Preedit { text: String::new(), active_range_chars: None }));
        h.event(egui::Event::Ime(egui::ImeEvent::Commit("{".into())));
        h.run();
        assert_eq!(h.state().workflow.script, "labels.style {}");
        h.event(egui::Event::Ime(egui::ImeEvent::Commit("}".into())));
        h.run();
        type_in(&mut h, "!");
        assert_eq!(h.state().workflow.script, "labels.style {}!", "the cursor stepped over the brace");
    }

    /// In the pane: a quote closes itself and the lists still open in it; accepting a key takes
    /// in its closing quote, accepting a tool keeps it, and a quote typed before it steps over.
    #[test]
    fn quotes_in_the_box() {
        let mut h = pane();
        type_in(&mut h, "map.goToXY ");
        for c in ["{", "\"", "l", "a"] {
            type_in(&mut h, c);
        }
        assert_eq!(h.state().workflow.script, r#"map.goToXY {"la"}"#);
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, r#"map.goToXY {"lat": }"#, "the key took in its quote");
        let mut h = pane();
        type_in(&mut h, "gp.run ");
        for c in ["{", "\"", "tool", "\"", ": ", "\"", "buffe"] {
            type_in(&mut h, c);
        }
        assert_eq!(h.state().workflow.script, r#"gp.run {"tool": "buffe"}"#, "the key's quote stepped over");
        h.key_press(egui::Key::Enter);
        h.run();
        type_in(&mut h, "!");
        assert_eq!(h.state().workflow.script, r#"gp.run {"tool": "buffer"!}"#, "the tool kept its quote");
        let mut h = pane();
        type_in(&mut h, r#"labels.style {"layer": "#);
        type_in(&mut h, "\"");
        h.key_press(egui::Key::Backspace);
        h.run();
        assert_eq!(h.state().workflow.script, r#"labels.style {"layer": "#, "an empty pair deleted");
    }

    /// A key is being typed in a quoted word that starts an entry of the line's object: right
    /// after its `{` or a `,`, at the top level (second-level objects: `key_at_finds_a_key_in_a_
    /// second_level_object`); not in a value, deeper, or in a comment.
    #[test]
    fn key_at_finds_a_key_being_typed() {
        let at = |s: &str| super::key_at(s, s.chars().count()).map(|k| (k.hint.id, k.start, k.word));
        assert_eq!(at(r#"labels.style {""#), Some(("labels.style".into(), 15, String::new())));
        assert_eq!(at(r#"labels.style {"layer": "Cities", "si"#).map(|k| k.2), Some("si".into()), "after a comma");
        assert_eq!(at(r#"x.y {"w": "a\"b", ""#).map(|k| k.2), Some(String::new()), "an escaped quote in a value");
        assert_eq!(at("# ñ\r\nlabels.style {\"la").map(|k| (k.1, k.2)), Some((20, "la".into())), "chars, CRLF");
        assert_eq!(at(r#"labels.style {"where": "po"#), None, "a value");
        assert_eq!(at(r#"x.y {"a": {"b": {""#), None, "deeper than a second-level object");
        assert_eq!(at(r#"x.y {"a": "#), None, "no quote");
        assert_eq!(at(r#"x.y {"a" "#), None, "past the key");
        assert_eq!(at(r#"# x.y {""#), None, "a comment");
        let s = r#"labels.style {"layer"}"#;
        assert_eq!(super::key_at(s, 20).map(|k| k.word), Some("layer".into()), "a closing quote after the cursor");
        assert!(super::key_at(s, 18).is_none(), "a letter after the cursor");
    }

    /// The keys offered: the command's, in the order described, those typed so far, case aside;
    /// not an entry already given, nor its alternatives.
    #[test]
    fn keys_offered_leave_out_those_given() {
        let desc = r#"{"lon" | "x": number, "lat" | "y": number, "scale"?: number}"#;
        let keys = |line: &str, word: &str| super::complete_keys("map.goToXY", desc, line, 0, 0, word).map(|o| o.items).unwrap_or_default();
        assert_eq!(keys(r#"{""#, ""), ["lon", "x", "lat", "y", "scale"]);
        assert_eq!(keys(r#"{"lon": 2, ""#, ""), ["lat", "y", "scale"]);
        assert_eq!(keys(r#"{"x": 2, ""#, ""), ["lat", "y", "scale"], "an alternative given");
        assert_eq!(keys(r#"{""#, "S"), ["scale"], "case aside");
        assert!(keys(r#"{""#, "zz").is_empty());
        assert_eq!(keys(r#"{"lat": 5}"#, ""), ["lon", "x", "scale"], "a key given");
        let o = super::complete_keys("map.goToXY", desc, r#"{""#, 0, 0, "").unwrap();
        assert_eq!(o.of, Some(super::Of::CommandKeys("map.goToXY".into())));
    }

    /// The key under the cursor, closed or not, doesn't count as given; the same key given
    /// elsewhere on the line does.
    #[test]
    fn the_key_being_typed_is_not_given() {
        let app = crate::RustGisApp::with_sample();
        let keys = |s: &str, at: usize| unasked(s, at, &[], &app.session).map(|o| o.items).unwrap_or_default();
        let s = r#"map.goToXY {"lat": 5}"#;
        assert_eq!(keys(s, 16), ["lat"], "a closed key can be changed");
        let s = r#"map.goToXY {"lat": 5, "lat": 6}"#;
        assert!(keys(s, 26).is_empty(), "the same key closed elsewhere");
        let s = r#"map.goToXY {"lat": 5, "lat"#;
        assert!(keys(s, s.chars().count()).is_empty(), "the same key, the cursor's not closed");
    }

    /// In the pane: a quote in a command's braces offers its keys, the help beside shows the
    /// highlighted key's parameter, and accepting closes the key with its colon.
    #[test]
    fn a_key_is_offered_and_accepted() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, r#"labels.style {""#);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "layer");
        type_in(&mut h, "si");
        h.get_by_label(r#""size"?: number"#);
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, r#"labels.style {"size": "#);
    }

    /// A closing quote already there is taken in; a key that has its colon keeps it.
    #[test]
    fn accepting_a_key_takes_in_what_follows() {
        let mut h = pane();
        type_in(&mut h, r#"map.goToXY {"la"}"#);
        for _ in 0..2 {
            h.key_press(egui::Key::ArrowLeft);
            h.run();
        }
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, r#"map.goToXY {"lat": }"#);
        let mut h = pane();
        type_in(&mut h, r#"map.goToXY {"la": 5}"#);
        for _ in 0..5 {
            h.key_press(egui::Key::ArrowLeft);
            h.run();
        }
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, r#"map.goToXY {"lat": 5}"#);
        type_in(&mut h, "!");
        assert_eq!(h.state().workflow.script, r#"map.goToXY {"lat"!: 5}"#, "the cursor after the key's quote");
    }

    /// The cursor is in a command's parameters inside its line's `{…}`: not before the brace, not
    /// past the closing one, and a `}` in a string doesn't close it.
    #[test]
    fn hint_at_finds_the_command_whose_braces_hold_the_cursor() {
        use super::hint_at;
        let at = |s: &str| hint_at(s, s.chars().count()).map(|h| (h.line, h.id));
        assert_eq!(at(r#"labels.style {"size": 1"#), Some((0, "labels.style".into())));
        assert_eq!(at("map.zoomFull\nlabels.style {"), Some((13, "labels.style".into())), "its line's start");
        assert_eq!(hint_at("map.zoomFull\nlabels.style {", 27).map(|h| h.brace), Some(26), "its brace");
        assert_eq!(at(r#"labels.style {"x": "a}b", "#), Some((0, "labels.style".into())), "a brace in a string");
        assert_eq!(at(r#"labels.style {"size": 1}"#), None, "past the closing brace");
        assert_eq!(at("labels.style "), None, "before the brace");
        assert_eq!(at("# labels.style {"), None, "a comment");
        assert_eq!(at(r#"labels.style {"a": {"b": 1"#), Some((0, "labels.style".into())), "nested");
        assert!(hint_at("x", 99).is_none(), "a cursor past the text");
    }

    /// Typing a command's parameters keeps its help by the cursor; past the closing brace it's
    /// gone, and Escape closes it until the cursor leaves the braces (a list opening over it
    /// doesn't).
    #[test]
    fn the_help_stays_by_the_cursor_in_the_braces() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        let help = super::help(&h.state().session, "labels.style").unwrap();
        let sig = format!("{}{}", help.id, help.keys);
        type_in(&mut h, "labels.style {");
        h.get_by_label(&sig);
        type_in(&mut h, r#""size": 12"#);
        h.get_by_label(&sig);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_label(&sig).is_none(), "Escape closes it");
        assert!(h.get_by_role(egui::accesskit::Role::MultilineTextInput).is_focused(), "and the box keeps the focus");
        type_in(&mut h, ", ");
        assert!(h.query_by_label(&sig).is_none(), "not again in the same braces");
        // A list opening over it, and Escape closing the list: still closed.
        type_in(&mut h, "\"");
        h.get_by_role_and_label(egui::accesskit::Role::Button, "layer");
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "layer").is_none(), "the list closed");
        assert!(h.query_by_label(&sig).is_none(), "not again once the list closes");
        type_in(&mut h, "x\": 1");
        type_in(&mut h, "}");
        assert!(h.query_by_label(&sig).is_none(), "past the closing brace");
        type_in(&mut h, "\nlabels.style {");
        h.get_by_label(&sig);
    }

    /// The help by the cursor stays put sideways while the parameters are typed: it's anchored at
    /// the line's `{`, not at the moving cursor.
    #[test]
    fn the_help_by_the_cursor_doesnt_slide() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        let help = super::help(&h.state().session, "labels.style").unwrap();
        let sig = format!("{}{}", help.id, help.keys);
        type_in(&mut h, "labels.style {");
        let first = h.get_by_label(&sig).rect().left();
        type_in(&mut h, r#""size": 12, "bold": true"#);
        assert!((h.get_by_label(&sig).rect().left() - first).abs() < 0.5, "it didn't move with the cursor");
    }

    /// In a call in an expression (#1240) the function's signature shows instead of the command's
    /// help, the innermost call's; Escape closes it until the cursor leaves the call, and out of
    /// it the command's help is back.
    #[test]
    fn a_call_in_an_expression_shows_its_signature() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        let help = super::help(&h.state().session, "select.byAttributes").unwrap();
        let sig = format!("{}{}", help.id, help.keys);
        let shows =
            |h: &egui_kittest::Harness<'static, crate::RustGisApp>, label: &str| h.query_by_role_and_label(Role::Label, label).is_some();
        let round = "round(x[, places])";
        type_in(&mut h, r#"select.byAttributes {"layer": "Countries", "where": "round(pop_est, "#);
        assert!(shows(&h, round), "the signature");
        assert!(h.query_by_label(&sig).is_none(), "not the command's help too");
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!shows(&h, round), "Escape closes it");
        assert!(h.query_by_label(&sig).is_none(), "and the command's help doesn't take its place");
        type_in(&mut h, "2)");
        h.get_by_label(&sig);
        type_in(&mut h, " + round(abs(");
        assert!(shows(&h, "abs(x)"), "the innermost call's");
        assert!(!shows(&h, round));
    }

    /// A command's help closed with Escape stays closed through a call in its expression: the
    /// signature shows in the call, and out of it the help doesn't come back, as the cursor never
    /// left the braces. A call in another command's braces is leaving them.
    #[test]
    fn a_closed_commands_help_stays_closed_through_a_call() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        let help = super::help(&h.state().session, "select.byAttributes").unwrap();
        let sig = format!("{}{}", help.id, help.keys);
        type_in(&mut h, r#"select.byAttributes {"layer": "Countries", "where": "pop_est > "#);
        h.get_by_label(&sig);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_label(&sig).is_none(), "closed");
        type_in(&mut h, "round(");
        assert!(h.query_by_role_and_label(Role::Label, "round(x[, places])").is_some(), "the signature in the call");
        type_in(&mut h, "1)");
        assert!(h.query_by_label(&sig).is_none(), "still closed out of the call");
        type_in(&mut h, "\"}\nselect.byAttributes {");
        h.get_by_label(&sig);
        // Closed in one command, then straight into a call in another, and back: shown again, as
        // the cursor left its braces.
        let mut h = pane();
        let help = super::help(&h.state().session, "labels.style").unwrap();
        let styled = format!("{}{}", help.id, help.keys);
        type_in(&mut h, r#"labels.style {"size": 1, "#);
        h.get_by_label(&styled);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_label(&styled).is_none(), "closed");
        type_in(&mut h, "\nselect.byAttributes {\"layer\": \"Countries\", \"where\": \"round(");
        assert!(h.query_by_role_and_label(Role::Label, "round(x[, places])").is_some(), "another command's call");
        // Straight back into the first command's braces (a click there).
        let id = egui::Id::unique("script_text");
        let mut state = egui::widgets::text_edit::TextEditState::load(&h.ctx, id).unwrap_or_default();
        let back = egui::text::CCursorRange::one(egui::text::CCursor::new("labels.style {".chars().count()));
        state.cursor.set_char_range(Some(back));
        state.store(&h.ctx, id);
        h.run();
        h.get_by_label(&styled);
    }

    /// Ctrl+Space opens the list with nothing typed: a line's first word, an expression's; and
    /// one Escape closed. Typing on leaves it to what's typed again.
    #[test]
    fn ctrl_space_asks_for_the_list() {
        use egui::accesskit::Role;
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        let ask = |h: &mut egui_kittest::Harness<'static, crate::RustGisApp>| {
            h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Space);
            h.run();
        };
        let mut ids = super::script_ids(&h.state().session);
        ids.sort();
        ask(&mut h);
        h.get_by_role_and_label(Role::Button, &ids[0]);
        assert_eq!(h.state().workflow.script, "", "nothing typed");
        type_in(&mut h, "x");
        assert!(h.query_by_role_and_label(Role::Button, &ids[0]).is_none(), "typed on: as unasked");
        let mut h = pane();
        let s = r#"select.byAttributes {"layer": "Countries", "where": ""#;
        type_in(&mut h, s);
        let first = super::offer_at(s, s.chars().count(), &[], &h.state().session, true).unwrap().items[0].clone();
        assert!(h.query_by_role_and_label(Role::Button, &first).is_none(), "an expression unasked");
        ask(&mut h);
        h.get_by_role_and_label(Role::Button, &first);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.query_by_role_and_label(Role::Button, &first).is_none(), "closed");
        ask(&mut h);
        h.get_by_role_and_label(Role::Button, &first);
    }

    /// Asked for (Ctrl+Space), a line's first word and an expression's word are offered with
    /// nothing typed; not where no list goes.
    #[test]
    fn asked_offers_with_nothing_typed() {
        let app = crate::RustGisApp::with_sample();
        let session = &app.session;
        let mut ids = super::script_ids(session);
        ids.sort();
        let asked = |s: &str| super::offer_at(s, s.chars().count(), &ids, session, true).map(|o| o.items).unwrap_or_default();
        let plain = |s: &str| unasked(s, s.chars().count(), &ids, session).map(|o| o.items).unwrap_or_default();
        assert_eq!(asked(""), ids, "an empty script: every id");
        assert_eq!(asked("map.zoomFull\n  "), ids, "an empty line");
        assert!(plain("").is_empty());
        let m = asked("m");
        assert!(!m.is_empty() && m.iter().all(|i| i.starts_with('m')), "one letter: those it starts");
        assert!(plain("m").is_empty());
        let s = r#"select.byAttributes {"layer": "Countries", "where": ""#;
        assert!(asked(s).iter().any(|i| i == "pop_est") && asked(s).iter().any(|i| i == "upper()"), "an expression's all");
        assert!(plain(s).is_empty());
        assert!(asked(&format!("{s}pop_est > ")).iter().any(|i| i == "pop_est"), "after an operator");
        for none in ["# ", "map.zoomFull ", "select.byAttributes {", r#"layer.rename {"layer": "Countries", "name": ""#] {
            assert!(asked(none).is_empty(), "{none}");
        }
    }

    /// The call the cursor is in, in an expression (#1240): its function, its argument and where
    /// its `(` is in the script, past escapes; the innermost; in a tool's or a step's
    /// `"params"`. None out of a call, in another key's string or in a comment.
    #[test]
    fn call_at_finds_the_call_in_an_expression() {
        let app = crate::RustGisApp::with_sample();
        let call = |s: &str| {
            let at = s.chars().count();
            let (c, open) = super::call_at(s, at, &super::hint_at(s, at)?, &app.session)?;
            Some((c.spec.signature, c.arg, open))
        };
        let open = |s: &str| s.chars().count() - s.chars().rev().position(|c| c == '(').unwrap() - 1;
        let s = r#"select.byAttributes {"layer": "Countries", "where": "round(pop_est, "#;
        assert_eq!(call(s), Some(("round(x[, places])", 1, open(s))));
        let s = r#"select.byAttributes {"layer": "Countries", "where": "round(abs("#;
        assert_eq!(call(s), Some(("abs(x)", 0, open(s))), "the innermost");
        let s = r#"select.byAttributes {"layer": "Countries", "where": "\"name\" = 'é' OR round("#;
        assert_eq!(call(s), Some(("round(x[, places])", 0, open(s))), "past escapes and wide chars");
        let s = r#"gp.run {"tool": "selectByAttributes", "params": {"input": "Countries", "where": "round("#;
        assert_eq!(call(s).map(|c| c.0), Some("round(x[, places])"), "a tool's");
        let s = r#"task.addStep {"command": "select.byAttributes", "params": {"layer": "Countries", "where": "round("#;
        assert_eq!(call(s).map(|c| c.0), Some("round(x[, places])"), "a step's");
        for none in [
            r#"select.byAttributes {"layer": "Countries", "where": "round(1) "#,
            r#"layer.rename {"layer": "Countries", "name": "round("#,
            r#"# select.byAttributes {"where": "round("#,
        ] {
            assert_eq!(call(none), None, "{none}");
        }
    }

    /// In a second-level object (a top-level key's `{…}`, or an object in its `[…]`), a key starts
    /// an entry at its top, as at the line's top level; not deeper, in a value, or once the object
    /// is closed.
    #[test]
    fn key_at_finds_a_key_in_a_second_level_object() {
        let at = |s: &str| super::key_at(s, s.chars().count()).map(|k| (k.start, k.word, k.nested.map(|n| (n.brace, n.key))));
        let s = r#"gp.run {"tool": "buffer", "params": {"di"#;
        assert_eq!(at(s), Some((38, "di".into(), Some((36, "params".into())))));
        let nested = |s: &str| at(s).and_then(|k| k.2);
        assert_eq!(nested(r#"gp.run {"params" : {"input": "Cities", ""#), Some((19, "params".into())), "after a comma, spaced colon");
        assert_eq!(nested(r#"topology.rules {"add": {""#).map(|n| n.1), Some("add".into()), "any key's object");
        let s = r#"layer.displayFilters {"filters": [{"name": "a"}, {""#;
        assert_eq!(nested(s), Some((49, "filters".into())), "the array's second object");
        assert_eq!(at(r#"gp.run {"tool": "buffer", "#), None, "no quote");
        assert_eq!(at(r#"gp.run {"tool": "buffer", ""#).map(|k| k.2), Some(None), "the top level");
        assert_eq!(at(r#"gp.run {"params": {"a": {""#), None, "deeper");
        assert_eq!(at(r#"x.y {"a": [[{""#), None, "an array's array");
        assert_eq!(at(r#"x.y {"a": [{"b": [{""#), None, "an array in the array's object");
        assert_eq!(at(r#"gp.run {"params": {"a": "b"#), None, "a value");
        assert_eq!(at(r#"gp.run {"params": {}, ""#).map(|k| k.2), Some(None), "closed");
        assert_eq!(at(r#"x.y {"filters": [{}], ""#).map(|k| k.2), Some(None), "the array closed");
        assert_eq!(at(r#"gp.run {"x": "params", "y" {""#), None, "no colon");
    }

    /// The tool a line runs (its top-level `"tool"`), wherever it's given, the last one as Run
    /// reads it, its escapes read; not a nested one, a value named "tool", or one whose escapes
    /// aren't JSON's.
    #[test]
    fn value_of_reads_the_lines_tool() {
        let tool_of = |line: &str| super::value_of(line, "tool");
        assert_eq!(tool_of(r#"{"tool": "buffer", "params": {}}"#).as_deref(), Some("buffer"));
        assert_eq!(tool_of(r#"{"params": {"input": "a"}, "tool" : "clip"}"#).as_deref(), Some("clip"), "after params");
        assert_eq!(tool_of(r#"{"tool": "a", "tool": "b"}"#).as_deref(), Some("b"), "the last one");
        assert_eq!(tool_of(r#"{"params": {"tool": "buffer"}}"#), None, "nested");
        assert_eq!(tool_of(r#"{"x": "tool", "y": "buffer"}"#), None, "a value");
        assert_eq!(tool_of(r#"{"tool": 5}"#), None, "not a string");
        assert_eq!(tool_of(r#"{"tool": "a\"b"}"#).as_deref(), Some("a\"b"), "escapes read");
        assert_eq!(tool_of(r#"{"tool": "a\qb"}"#), None, "not JSON's");
        assert_eq!(tool_of(r#"{"tool": "buf"#), None, "being typed");
        assert_eq!(tool_of(r#"{"tool""#), None);
        assert_eq!(tool_of(""), None);
    }

    /// A JSON string's text is read with its escapes, each char placed where it starts in the
    /// string, up to its closing quote or the line's end; an escape not finished or not JSON's
    /// reads as nothing.
    #[test]
    fn a_strings_text_is_read_with_its_escapes() {
        let read = |s: &str| super::unescape(&s.chars().collect::<Vec<_>>());
        assert_eq!(read(r#"pop > 1"#), Some(("pop > 1".to_string(), (0..=7).collect())));
        assert_eq!(read(r#"\"Pop Est\" = 1", "mode""#).map(|(t, _)| t), Some("\"Pop Est\" = 1".to_string()), "to its quote");
        assert_eq!(read(r#"a\\b"#), Some(("a\\b".to_string(), vec![0, 1, 3, 4])));
        // Escapes built apart, so no tool reading this file takes them for its own.
        let escaped = ["", "u00e9", "ud83d", "ude00x"].join("\\");
        assert_eq!(read(&escaped), Some(("é😀x".to_string(), vec![0, 6, 18, 19])), "a surrogate pair is one char");
        assert_eq!(read("a\nb").map(|(t, _)| t), Some("a".to_string()), "to the line's end");
        assert_eq!(read(r#"a\"#), None, "an escape not finished");
        assert_eq!(read(r#"\u00"#), None);
        assert_eq!(read(r#"\x"#), None, "not JSON's");
        assert_eq!(read(r#"\udc00"#), None, "a lone surrogate");
        assert_eq!(read(&["", "ud83d", "ue000"].join("\\")), None, "a high surrogate without its low one");
        assert_eq!(read(&["", "u+041"].join("\\")), None, "a sign is no hex digit");
    }

    /// In the string of a key that takes an expression (#1240), what an expression box offers
    /// there: the fields of the line's layer (the active one without a `"layer"`), of a `gp.run`
    /// tool's input given before or after, the language's words; with the cursor anywhere in it.
    /// Nothing in another key's string, in quoted text, or in a nested `where`.
    #[test]
    fn an_expressions_words_are_offered_in_its_string() {
        let app = crate::RustGisApp::with_sample();
        let session = &app.session;
        let at = |s: &str, at: usize| unasked(s, at, &[], session);
        let end = |s: &str| at(s, s.chars().count());
        let items =
            |o: Option<super::Offer>| o.filter(|o| matches!(o.of, Some(super::Of::Expression(_)))).map(|o| o.items).unwrap_or_default();
        let has = |o: Option<super::Offer>, item: &str| items(o).iter().any(|i| i == item);
        let s = r#"select.byAttributes {"layer": "Countries", "where": "pop_"#;
        let o = end(s).unwrap();
        assert_eq!((o.start, o.end), (s.chars().count() - 4, s.chars().count()), "the word being typed");
        assert!(has(Some(o), "pop_est"));
        assert!(has(end(r#"select.byAttributes {"layer": "Cities", "where": "pop_"#), "pop_max"), "the line's layer");
        let active = session.map().layer(session.active_layer.unwrap()).unwrap();
        let field = &active.dataset.fields.first().unwrap().name;
        let typed: String = field.chars().take(2).collect();
        assert!(has(end(&format!(r#"select.byAttributes {{"where": "{typed}"#)), field), "the active layer's");
        assert!(has(end(r#"select.byAttributes {"layer": "Nope", "where": "up"#), "upper()"), "no layer: the language's words");
        assert!(items(end(r#"select.byAttributes {"layer": "Nope", "where": "pop_"#)).is_empty(), "and no field");
        assert!(has(end(r#"edit.calculateField {"layer": "Countries", "field": "x", "expression": "up"#), "upper()"));
        // In the middle: what follows the cursor, in the string and after it, is left.
        let s = r#"select.byAttributes {"layer": "Countries", "where": "pop_ > 1 AND x", "mode": "new"}"#;
        assert!(has(at(s, s.find("_ >").unwrap() + 1), "pop_est"), "mid string");
        assert!(has(end(r#"select.byAttributes {"layer": "Countries", "where": "pop_est > 1 AND pop_"#), "pop_est"), "after an operator");
        // A field begun with an escaped quote.
        assert!(has(end(r#"select.byAttributes {"layer": "Countries", "where": "\"pop_"#), "pop_est"));
        // A tool's expression, over its input, given before or after.
        assert!(has(end(r#"gp.run {"tool": "selectByAttributes", "params": {"input": "Countries", "where": "pop_"#), "pop_est"));
        let s = r#"gp.run {"tool": "selectByAttributes", "params": {"where": "pop_"#;
        assert!(has(at(&format!(r#"{s}", "input": "Cities"}}}}"#), s.chars().count()), "pop_max"), "input after");
        // A step's command's expression, over the step's layer, given before or after it, the
        // command too; the active layer's without one.
        let step = r#"task.addStep {"title": "t", "command": "select.byAttributes", "params": {"#;
        assert!(has(end(&format!(r#"{step}"layer": "Countries", "where": "pop_"#)), "pop_est"), "a step's expression");
        let s = r#"task.addStep {"title": "t", "params": {"where": "pop_"#;
        let line = format!(r#"{s}", "layer": "Cities"}}, "command": "select.byAttributes"}}"#);
        assert!(has(at(&line, s.chars().count()), "pop_max"), "layer and command after");
        assert!(has(end(&format!(r#"{step}"where": "{typed}"#)), field), "a step's active layer");
        // A later object on the line isn't read as the `"params"`' layer or input.
        let s = r#"task.addStep {"command": "select.byAttributes", "params": {"layer": "Countries", "where": "pop_"#;
        assert!(has(at(&format!(r#"{s}"}}, "x": {{"layer": "Cities"}}}}"#), s.chars().count()), "pop_est"), "a step's params alone");
        let s = r#"gp.run {"tool": "selectByAttributes", "params": {"input": "Countries", "where": "pop_"#;
        assert!(has(at(&format!(r#"{s}"}}, "x": {{"input": "Cities"}}}}"#), s.chars().count()), "pop_est"), "a tool's params alone");
        let plain = r#"task.addStep {"command": "layer.rename", "params": {"layer": "Countries", "name": "pop_"#;
        assert!(items(end(plain)).is_empty(), "a step's plain text");
        // Not here.
        assert!(items(end(r#"layer.rename {"layer": "Countries", "name": "pop_"#)).is_empty(), "another key");
        assert!(items(end(r#"select.byAttributes {"layer": "Countries", "where": "name = 'pop_"#)).is_empty(), "quoted text");
        assert!(items(end(r#"select.byAttributes {"layer": "Countries", "where": "#)).is_empty(), "nothing typed");
        assert!(items(end(r#"select.byAttributes {"layer": "Countries", "where": "a \"#)).is_empty(), "an escape begun");
        assert!(items(end(r#"layer.displayFilters {"layer": "Countries", "filters": [{"where": "pop_"#)).is_empty(), "nested");
        assert!(items(end(r#"gp.run {"tool": "buffer", "params": {"input": "Countries", "output": "pop_"#)).is_empty(), "a tool's text");
    }

    /// A layer named with escapes (`"Big \"cities\""`) is read as its name wherever the Script
    /// reads one: a command's `"layer"`, for its fields and its expressions, and a `gp.run` tool's
    /// input, for its fields and its expressions alike.
    #[test]
    fn escaped_names_give_their_fields() {
        let mut app = crate::RustGisApp::with_sample();
        app.run("layer.rename", serde_json::json!({"layer": "Cities", "name": "Big \"cities\""})).unwrap();
        let id = app.session.map().layer_by_name("Big \"cities\"").unwrap().id;
        let end = |s: &str| unasked(s, s.chars().count(), &[], &app.session).unwrap();
        let o = end(r#"select.byAttributes {"layer": "Big \"cities\"", "where": "pop_"#);
        assert!(matches!(&o.of, Some(super::Of::Expression(e)) if e.data == Some(super::Of::Fields(id))), "a command's expression");
        assert!(o.items.iter().any(|i| i == "pop_max"));
        let o = end(r#"task.addStep {"command": "select.byAttributes", "params": {"layer": "Big \"cities\"", "where": "pop_"#);
        assert!(matches!(&o.of, Some(super::Of::Expression(e)) if e.data == Some(super::Of::Fields(id))), "a step's expression");
        let o = end(r#"labels.field {"layer": "Big \"cities\"", "field": "pop"#);
        assert_eq!((o.items, o.of), (vec!["pop_max".to_string()], Some(super::Of::Fields(id))), "a command's field");
        let o = end(r#"gp.run {"tool": "selectByAttributes", "params": {"input": "Big \"cities\"", "where": "pop_"#);
        assert!(matches!(&o.of, Some(super::Of::Expression(e)) if e.data == Some(super::Of::ToolFields("Big \"cities\"".into()))));
        assert!(o.items.iter().any(|i| i == "pop_max"), "a tool's expression");
        let o = end(r#"gp.run {"tool": "summaryStatistics", "params": {"input": "Big \"cities\"", "field": "pop"#);
        assert_eq!((o.items, o.of), (vec!["pop_max".to_string()], Some(super::Of::ToolFields("Big \"cities\"".into()))), "a tool's field");
    }

    /// An expression's item goes in as the expression box puts it, escaped as the string's text:
    /// a field begun with a quote closed with one (`\"pop_est\"`), a function with its brackets and
    /// the cursor between them; the rest of the string keeps its spelling (an `\u` escape).
    #[test]
    fn an_expressions_item_is_put_escaped() {
        let app = crate::RustGisApp::with_sample();
        let put = |s: &str, item: &str| {
            let offer = unasked(s, s.chars().count(), &[], &app.session).unwrap();
            let (new, at) = super::accepted(s, &offer, item, &app.session).unwrap();
            let chars: Vec<char> = new.chars().collect();
            format!("{}|{}", chars[..at].iter().collect::<String>(), chars[at..].iter().collect::<String>())
        };
        let head = r#"select.byAttributes {"layer": "Countries", "where": "#;
        assert_eq!(put(&format!(r#"{head}"pop_"#), "pop_est"), format!(r#"{head}"pop_est|"#));
        assert_eq!(put(&format!(r#"{head}"\"pop_"#), "pop_est"), format!(r#"{head}"\"pop_est\"|"#), "its quote closed, escaped");
        let e = ["", "u00e9"].join("\\");
        assert_eq!(put(&format!(r#"{head}"name = '{e}' OR up"#), "upper()"), format!(r#"{head}"name = '{e}' OR upper(|)"#));
        let s = format!(r#"{head}"pop_ > 1", "mode": "new"}}"#);
        let offer = unasked(&s, head.chars().count() + 5, &[], &app.session).unwrap();
        let (new, _) = super::accepted(&s, &offer, "pop_est", &app.session).unwrap();
        assert_eq!(new, format!(r#"{head}"pop_est > 1", "mode": "new"}}"#), "mid string: the rest is left");
    }

    /// In the string of an expression, a text compared with a field of the line's layer lists
    /// that field's values, as an expression box does, and the one taken goes in with its quotes.
    #[test]
    fn a_fields_values_are_offered_in_an_expressions_string() {
        let app = crate::RustGisApp::with_sample();
        let layer = app.session.map().layer_by_name("Countries").unwrap();
        let ds = &layer.dataset;
        let (i, field) = ds.fields.iter().enumerate().find(|(_, f)| f.ty == rustgis_engine::data::FieldType::Text).unwrap();
        let value = rustgis_engine::data::query::field_values(ds, i, 100)
            .values
            .into_iter()
            .find(|v| v.text.chars().all(char::is_alphanumeric))
            .unwrap();
        let (value, name) = (value.text, crate::exprcomplete::field_ref(&field.name).replace('"', "\\\""));
        let typed: String = value.chars().take(1).collect();
        let s = format!(r#"select.byAttributes {{"layer": "Countries", "where": "{name} = '{typed}"#);
        let offer = unasked(&s, s.chars().count(), &[], &app.session).unwrap();
        assert!(offer.items.iter().any(|v| *v == format!("'{value}'")), "{:?}", offer.items);
        let (new, _) = super::accepted(&s, &offer, &format!("'{value}'"), &app.session).unwrap();
        assert_eq!(new, format!(r#"select.byAttributes {{"layer": "Countries", "where": "{name} = '{value}'"#));
        // A value holding a `"` and a `'` goes in escaped as the string's text.
        let field = field.name.clone();
        let mut app = app;
        app.run("edit.setValue", serde_json::json!({"layer": "Countries", "row": 0, "field": field, "value": "Say \"hi\" o'k"})).unwrap();
        let s = format!(r#"select.byAttributes {{"layer": "Countries", "where": "{name} = 'Say"#);
        let offer = unasked(&s, s.chars().count(), &[], &app.session).unwrap();
        let item = "'Say \"hi\" o''k'";
        assert!(offer.items.iter().any(|v| v == item), "{:?}", offer.items);
        let (new, _) = super::accepted(&s, &offer, item, &app.session).unwrap();
        assert_eq!(new, format!(r#"select.byAttributes {{"layer": "Countries", "where": "{name} = 'Say \"hi\" o''k'"#));
    }

    /// In a `gp.run` line's `"params"`, the keys of the tool it runs (by id or name, case aside,
    /// as Run finds it), less those that object has; none without a known tool, nor for another
    /// command's `"params"`.
    #[test]
    fn a_tools_keys_are_offered_in_params() {
        let app = crate::RustGisApp::with_sample();
        let keys = |s: &str| unasked(s, s.chars().count(), &[], &app.session).map(|o| o.items).unwrap_or_default();
        assert_eq!(keys(r#"gp.run {"tool": "buffer", "params": {""#), ["input", "distance", "units", "dissolve", "output"]);
        assert_eq!(keys(r#"gp.run {"params": {"input": "Cities", "d"#), Vec::<String>::new(), "no tool yet");
        assert_eq!(keys(r#"gp.run {"tool": "Buffer", "params": {"input": "Cities", "d"#), ["distance", "dissolve"], "by name");
        assert_eq!(keys(r#"gp.run {"tool": "nope", "params": {""#), Vec::<String>::new(), "an unknown tool");
        assert_eq!(keys(r#"task.addStep {"command": "x", "params": {""#), Vec::<String>::new(), "another command");
        assert!(keys(r#"gp.run {"tool": "buffer", "params": {"output": "a", "o"#).is_empty(), "given in params");
        assert_eq!(keys(r#"gp.run {"output": 1, "tool": "buffer", "params": {"o"#), ["output"], "given at the top level");
        let s = r#"gp.run {"params": {""#;
        let o = unasked(&format!(r#"{s}"}}, "tool": "buffer"}}"#), s.chars().count(), &[], &app.session).unwrap();
        assert_eq!(o.items.len(), 5, "the tool after params");
        assert_eq!(o.of, Some(super::Of::ToolKeys("buffer".into())));
        let s = r#"gp.run {"tool": "buffer", "params": {"distance": 5, "distance"}}"#;
        assert!(keys(&s[..s.len() - 2]).is_empty(), "the same key given before in params");
        let s = r#"gp.run {"params": {""#;
        let o = unasked(&format!(r#"{s}"}}, "output": 1, "tool": "buffer"}}"#), s.chars().count(), &[], &app.session).unwrap();
        assert!(o.items.iter().any(|k| k == "output"), "a top-level key after params isn't given in it");
        let o = unasked(&format!(r#"{s}"}}, "x": {{"output": 1}}, "tool": "buffer"}}"#), s.chars().count(), &[], &app.session);
        assert!(o.is_some_and(|o| o.items.iter().any(|k| k == "output")), "nor a key of a later object");
        let o = unasked(&format!("{s}\n\"tool\": \"buffer\"}}}}"), s.chars().count(), &[], &app.session);
        assert!(o.is_none(), "a tool on another line: a command is one line");
    }

    /// A tool's parameter reads as a command's is described: its key, what it takes, its name, and
    /// its default when it has one.
    #[test]
    fn a_tools_parameter_reads_as_described() {
        let buffer = rustgis_engine::analysis::tool("buffer").unwrap();
        let line = |name: &str| super::param_line(buffer.params.iter().find(|p| p.name == name).unwrap());
        assert_eq!(line("input"), r#""input": layer — Input Features"#);
        assert_eq!(line("distance"), r#""distance": number — Distance, default 100"#);
        assert_eq!(line("dissolve"), r#""dissolve"?: "None" | "All" — Dissolve, default "None""#);
        let help = super::tool_help(buffer);
        assert_eq!(help.keys, " {input, distance, units?, dissolve?, output?}");
        assert_eq!((help.id.as_str(), help.label.as_str(), help.params.len()), ("buffer", "Buffer", 5));
    }

    /// In the pane: in a `gp.run` line's `"params"`, a quote offers the tool's keys with the
    /// highlighted one's parameter beside, accepting closes it with its colon, and the help by
    /// the cursor is the tool's there, `gp.run`'s elsewhere in the line.
    #[test]
    fn a_tools_key_is_offered_and_accepted() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, r#"gp.run {"tool": "buffer", "#);
        let tool = super::tool_help(rustgis_engine::analysis::tool("buffer").unwrap());
        let sig = format!("{}{}", tool.id, tool.keys);
        let run = super::help(&h.state().session, "gp.run").unwrap();
        h.get_by_label(&format!("{}{}", run.id, run.keys));
        assert!(h.query_by_label(&sig).is_none(), "gp.run's help at the top level");
        type_in(&mut h, r#""params": {"#);
        h.get_by_label(&sig);
        type_in(&mut h, r#""di"#);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "dissolve");
        h.get_by_label(r#""distance": number — Distance, default 100"#);
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, r#"gp.run {"tool": "buffer", "params": {"distance": "#);
    }

    /// A top-level value is being typed: the cursor in a quoted word right after a key and its
    /// colon; not a key, a nested value, an unquoted one, or one already closed.
    #[test]
    fn value_at_finds_a_value_being_typed() {
        let at = |s: &str| super::value_at(s, s.chars().count()).map(|v| (v.hint.id, v.start, v.word, v.key));
        assert_eq!(at(r#"gp.run {"tool": "bu"#), Some(("gp.run".into(), 17, "bu".into(), "tool".into())));
        assert_eq!(at(r#"gp.run {"params": {}, "tool" : ""#).map(|v| (v.2, v.3)), Some((String::new(), "tool".into())), "after params");
        assert_eq!(at(r#"gp.run {"tool": "a", "x"#), None, "a key");
        assert_eq!(at(r#"gp.run {"params": {"tool": ""#), None, "nested");
        assert_eq!(at(r#"gp.run {"tool": 5"#), None, "not quoted");
        assert_eq!(at(r#"gp.run {"tool" ""#), None, "no colon");
        assert_eq!(at(r#"gp.run {"tool": "a" "#), None, "closed");
        let s = r#"gp.run {"tool": "bu"}"#;
        assert_eq!(super::value_at(s, 19).map(|v| v.word), Some("bu".into()), "a closing quote after the cursor");
        assert!(super::value_at(s, 18).is_none(), "a letter after the cursor");
    }

    /// In a `gp.run` line's `"tool"`, the tool ids that start with what's typed, case aside; no
    /// value is offered for another key or another command.
    #[test]
    fn tool_ids_are_offered_for_gp_runs_tool() {
        let app = crate::RustGisApp::with_sample();
        let offer = |s: &str| unasked(s, s.chars().count(), &[], &app.session);
        let o = offer(r#"gp.run {"tool": "BUF"#).unwrap();
        assert_eq!((o.items.first().map(String::as_str), o.of), (Some("buffer"), Some(super::Of::ToolIds)));
        assert!(o.items.iter().all(|id| id.to_lowercase().starts_with("buf")));
        let all = offer(r#"gp.run {"tool": ""#).map(|o| o.items).unwrap_or_default();
        let ids: std::collections::HashSet<&str> = rustgis_engine::analysis::tools().map(|t| t.id).collect();
        assert_eq!(all.len(), ids.len(), "every tool, once");
        assert!(offer(r#"gp.run {"tool": "zzz"#).is_none());
        assert!(offer(r#"labels.style {"where": ""#).is_none(), "another key's value");
        assert!(offer(r#"task.addStep {"tool": ""#).is_none(), "another command");
    }

    /// A `"layer"` value offers the map's layers, top first, case aside; a `"field"` value the
    /// fields of the line's layer (before or after it), or the active layer's without one, as Run
    /// finds it. Only in a command that takes the key; nothing for a layer that isn't there or
    /// isn't given by name, nor at the second level.
    #[test]
    fn layers_and_fields_are_offered_for_their_values() {
        let mut app = crate::RustGisApp::with_sample();
        let offer = |app: &crate::RustGisApp, s: &str| unasked(s, s.chars().count(), &[], &app.session);
        let names: Vec<String> = app.session.map().layers.iter().map(|l| l.name.clone()).collect();
        let o = offer(&app, r#"labels.style {"layer": ""#).unwrap();
        assert_eq!((o.items, o.of), (names, Some(super::Of::Layers)));
        assert_eq!(offer(&app, r#"labels.field {"layer": "ci"#).unwrap().items, ["Cities"], "case aside");
        assert!(offer(&app, r#"map.goToXY {"layer": ""#).is_none(), "a command that doesn't take a layer");
        assert!(offer(&app, r#"labels.style {"layer": "Cities", "field": ""#).is_none(), "nor a field");
        assert!(offer(&app, r#"labels.field {"layer": "zz"#).is_none());
        let cities = app.session.map().layer_by_name("Cities").unwrap();
        let (id, fields): (u32, Vec<String>) = (cities.id, cities.dataset.fields.iter().map(|f| f.name.clone()).collect());
        let o = offer(&app, r#"labels.field {"layer": "cities", "field": ""#).unwrap();
        assert_eq!((o.items, o.of), (fields.clone(), Some(super::Of::Fields(id))));
        let s = r#"labels.field {"field": ""#;
        let o = unasked(&format!(r#"{s}", "layer": "Cities"}}"#), s.chars().count(), &[], &app.session).unwrap();
        assert_eq!(o.items, fields, "the layer after the field");
        assert!(offer(&app, r#"labels.field {"layer": "Nowhere", "field": ""#).is_none(), "a layer that isn't there");
        assert!(offer(&app, r#"labels.field {"layer": 99, "field": ""#).is_none(), "a layer given as a number");
        assert!(offer(&app, r#"labels.field {"layer": "a\"b", "field": ""#).is_none(), "an escaped name that isn't there");
        app.session.active_layer = Some(id);
        assert_eq!(offer(&app, r#"labels.field {"field": ""#).unwrap().items, fields, "the active layer");
        app.session.active_layer = None;
        assert!(offer(&app, r#"labels.field {"field": ""#).is_none(), "no active layer");
        assert!(offer(&app, r#"gp.run {"params": {"layer": ""#).is_none(), "in params, without a tool");
        // Two layers named alike but for case: the first, the one Run finds by either name.
        let other = app.session.map().layers.iter().position(|l| l.name != "Cities").unwrap();
        app.session.map_mut().layers[other].name = "CITIES".into();
        let cities = app.session.map().layers.iter().filter(|l| l.name.eq_ignore_ascii_case("cities")).count();
        assert_eq!(cities, 2);
        assert_eq!(offer(&app, r#"labels.style {"layer": "ci"#).unwrap().items.len(), 1, "once, case aside");
    }

    /// A key whose description gives a field as its type offers the line's layer's fields, in a
    /// string or right in its array (#846); the numeric ones where it says so; a `layer.geocode`
    /// line's table's where it names one. Not for a command that takes no `"layer"` (its fields
    /// are another layer's), nor for a type that isn't a field.
    #[test]
    fn keys_that_take_fields_offer_them_in_arrays_too() {
        use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType};
        let mut app = crate::RustGisApp::with_sample();
        let offer = |app: &crate::RustGisApp, s: &str| unasked(s, s.chars().count(), &[], &app.session);
        let cities = app.session.map().layer_by_name("Cities").unwrap();
        let id = cities.id;
        let fields: Vec<String> = cities.dataset.fields.iter().map(|f| f.name.clone()).collect();
        let numeric: Vec<String> = cities.dataset.fields.iter().filter(|f| f.ty.is_numeric()).map(|f| f.name.clone()).collect();
        assert!(!numeric.is_empty() && numeric.len() < fields.len());
        let o = offer(&app, r#"data.explore {"layer": "Cities", "fields": [""#).unwrap();
        assert_eq!((o.items, o.of), (numeric.clone(), Some(super::Of::Fields(id))), "[numeric fields]");
        let s = format!(r#"data.explore {{"layer": "Cities", "fields": ["{}", ""#, numeric[0]);
        assert_eq!(offer(&app, &s).unwrap().items, numeric[1..], "not one given before");
        let s = r#"data.explore {"layer": "Cities", "fields": [""#;
        let line = format!(r#"{s}", "{}"]}}"#, numeric[1]);
        let o = unasked(&line, s.chars().count(), &[], &app.session).unwrap();
        assert!(!o.items.contains(&numeric[1]) && o.items.contains(&numeric[0]), "nor one given after");
        let s = format!(r#"data.explore {{"layer": "Cities", "fields": ["{}"#, numeric[0]);
        assert_eq!(offer(&app, &s).unwrap().items[0], numeric[0], "the one being typed stays");
        assert_eq!(offer(&app, r#"share.locator {"layer": "Cities", "fields": [""#).unwrap().items, fields, "[fields searched]");
        assert_eq!(offer(&app, r#"data.convert {"layer": "Cities", "nameField": ""#).unwrap().items, fields, "field");
        assert_eq!(offer(&app, r#"layer.geocode {"layer": "Cities", "address": [""#).unwrap().items, fields, "field | [fields]");
        assert!(offer(&app, r#"data.relate {"origin": "Countries", "originKey": ""#).is_none(), "no layer: another's fields");
        assert!(offer(&app, r#"report.subreport {"fields": [""#).is_none(), "[related fields]");
        assert!(offer(&app, r#"data.contingentValues {"layer": "Cities", "combos": [[""#).is_none(), "deeper in an array");
        // In an array only a list of fields: Run turns down an array for another key.
        assert!(offer(&app, r#"labels.field {"layer": "Cities", "field": [""#).is_none(), "one field, not a list");
        assert!(offer(&app, r#"labels.style {"layer": [""#).is_none(), "a layer");
        assert!(offer(&app, r#"gp.run {"tool": [""#).is_none(), "a tool");
        assert!(offer(&app, r#"gp.run {"tool": "buffer", "params": {"input": [""#).is_none(), "a tool's parameter");
        assert!(offer(&app, r#"labels.field {"layer": ["Cities"], "field": ""#).is_none(), "nor is a layer given in one the line's");
        let table =
            app.session.project.add_table(Dataset::new("Addresses", GeometryType::None, vec![Field::new("street", FieldType::Text)]));
        let o = offer(&app, r#"layer.geocode {"table": "addresses", "address": ""#).unwrap();
        assert_eq!((o.items, o.of), (vec!["street".to_string()], Some(super::Of::TableFields(table))), "a table's");
        let s = super::item_help(&app.session, &super::Of::TableFields(table), "street").map(|h| h.0);
        assert_eq!(s.as_deref(), Some(FieldType::Text.label()));
        assert_eq!(super::item_icon(&app.session, Some(&super::Of::TableFields(table)), "street"), Some("field"));
        assert!(offer(&app, r#"layer.geocode {"table": "Nowhere", "address": ""#).is_none(), "a table that isn't there");
        assert!(offer(&app, r#"layer.geocode {"table": 7, "address": ""#).is_none(), "a table given as a number: not the layer's");
        // A name given in the array with escapes is the field it names: not offered again.
        let fields = vec![Field::new("a\"b", FieldType::Text), Field::new("c", FieldType::Text)];
        app.session.project.add_table(Dataset::new("Quoted", GeometryType::None, fields));
        let o = offer(&app, r#"layer.geocode {"table": "Quoted", "address": ["a\"b", ""#).unwrap();
        assert_eq!(o.items, ["c"], "an escaped name given");
    }

    /// A parameter's type says whether it takes fields, only numeric ones, in a list; a note in
    /// parentheses isn't part of it.
    #[test]
    fn an_entrys_type_says_whether_it_takes_fields() {
        let ty = super::entry_field_type;
        assert_eq!(ty(r#""field": field"#), Some((false, false)));
        assert_eq!(ty(r#""sort"?: field|null"#), Some((false, false)));
        assert_eq!(ty(r#""fields"?: [numeric fields]"#), Some((true, true)));
        assert_eq!(ty(r#""address": field | [fields] (joined with commas)"#), Some((false, true)));
        assert_eq!(ty(r#""fields"?: [fields searched] (default: text fields)"#), Some((false, true)));
        assert_eq!(ty(r#""field"?: numeric field (metres)"#), Some((true, false)));
        assert_eq!(ty(r#""name": field (the label [optional])"#), Some((false, false)), "a note's brackets");
        assert_eq!(ty(r#""fields"?: [related fields]"#), None);
        assert_eq!(ty(r#""fields"?: [names] (default: all)"#), None);
        assert_eq!(ty(r#""layer"?"#), None, "no type");
    }

    /// Beside a layer, what it holds and its kind's icon, as the Layers pane shows it; beside a
    /// field, its type and the field icon; no icon for ids.
    #[test]
    fn layers_and_fields_have_help_and_icons() {
        use super::{Of, item_help, item_icon};
        let app = crate::RustGisApp::with_sample();
        let s = &app.session;
        let cities = s.map().layer_by_name("Cities").unwrap();
        let (text, _) = item_help(s, &Of::Layers, "Cities").unwrap();
        assert!(text.contains(&cities.dataset.features.len().to_string()), "{text}");
        assert_eq!(item_icon(s, Some(&Of::Layers), "Cities"), Some("point_layer"));
        assert_eq!(item_icon(s, Some(&Of::Layers), "Rivers"), Some("line_layer"));
        assert_eq!(item_icon(s, Some(&Of::Layers), "Countries"), Some("polygon_layer"));
        let f = &cities.dataset.fields[0];
        assert_eq!(item_help(s, &Of::Fields(cities.id), &f.name).map(|h| h.0), Some(f.ty.label().to_string()));
        assert_eq!(item_icon(s, Some(&Of::Fields(cities.id)), &f.name), Some("field"));
        assert_eq!(item_icon(s, Some(&Of::ToolIds), "buffer"), None);
    }

    /// In a `gp.run` line's `"params"`, a value offers what the tool's parameter of its key takes,
    /// by its kind, as the tool's form offers it (#788): the layers of a polygon parameter, every
    /// layer then the tables for any layer, the numeric fields of the input for a numeric field (the
    /// input before or after it, a table's too), a choice's choices in order; nothing for a
    /// number, nor in another command's `"params"`.
    #[test]
    fn a_tools_values_are_offered_by_kind() {
        use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType};
        let mut app = crate::RustGisApp::with_sample();
        let offer = |app: &crate::RustGisApp, s: &str| unasked(s, s.chars().count(), &[], &app.session);
        let names = |app: &crate::RustGisApp, keep: fn(&rustgis_engine::map::Layer) -> bool| -> Vec<String> {
            app.session.map().layers.iter().filter(|l| keep(l)).map(|l| l.name.clone()).collect()
        };
        let polygons = names(&app, |l| l.geometry_type() == GeometryType::Polygon);
        assert!(polygons.iter().any(|n| n == "Countries") && !polygons.iter().any(|n| n == "Cities"));
        let o = offer(&app, r#"gp.run {"tool": "dissolve", "params": {"input": ""#).unwrap();
        assert_eq!((o.items, o.of), (polygons, Some(super::Of::Layers)), "a polygon layer");
        // A table after the layers; one named as a layer isn't the one Run finds, so not offered.
        app.session.project.add_table(Dataset::new("Stats", GeometryType::None, vec![Field::new("total", FieldType::Integer)]));
        app.session.project.add_table(Dataset::new("Cities", GeometryType::None, vec![]));
        let mut any = names(&app, |_| true);
        any.push("Stats".into());
        assert_eq!(offer(&app, r#"gp.run {"tool": "buffer", "params": {"input": ""#).unwrap().items, any, "any layer, then the tables");
        assert_eq!(
            offer(&app, r#"gp.run {"tool": "Buffer", "params": {"input": "st"#).unwrap().items,
            ["Stats"],
            "the tool by name, case aside"
        );
        let cities = app.session.map().layer_by_name("Cities").unwrap();
        let fields: Vec<String> = cities.dataset.fields.iter().map(|f| f.name.clone()).collect();
        let numeric: Vec<String> = cities.dataset.fields.iter().filter(|f| f.ty.is_numeric()).map(|f| f.name.clone()).collect();
        assert!(!numeric.is_empty() && numeric.len() < fields.len());
        let o = offer(&app, r#"gp.run {"tool": "summaryStatistics", "params": {"input": "Cities", "field": ""#).unwrap();
        assert_eq!((o.items, o.of), (numeric.clone(), Some(super::Of::ToolFields("Cities".into()))), "a numeric field");
        let o = offer(&app, r#"gp.run {"tool": "summaryStatistics", "params": {"input": "Cities", "case": ""#).unwrap();
        assert_eq!(o.items, fields, "any field");
        let o = offer(&app, r#"gp.run {"tool": "summaryStatistics", "params": {"input": "Cities", "case": [""#).unwrap();
        assert_eq!(o.items, fields, "field-list array values");
        let o = offer(&app, r#"gp.run {"tool": "pivotTable", "params": {"input": "Cities", "fields": [""#).unwrap();
        assert_eq!(o.items, fields, "pivot row fields");
        let o = offer(&app, r#"gp.run {"tool":"summaryStatistics","params":{"input":"Cities","case":[" NAME ",""#).unwrap();
        assert!(
            !o.items.iter().any(|f| f.eq_ignore_ascii_case("name")),
            "previous array entry excluded with execution's trimming and casing"
        );
        let before = r#"gp.run {"tool":"pivotTable","params":{"input":"Cities","fields":[""#;
        let o = unasked(&format!(r#"{before}","name"]}}}}"#), before.chars().count(), &[], &app.session).unwrap();
        assert!(!o.items.iter().any(|f| f.eq_ignore_ascii_case("name")), "later array entry excluded");
        let id = app.session.project.tables[0].id;
        for tool in ["summaryStatistics", "pivotTable"] {
            let key = if tool == "summaryStatistics" { "case" } else { "fields" };
            let text = format!(r#"gp.run {{"tool":"{tool}","params":{{"input":{id},"{key}":[""#);
            let o = offer(&app, &text).unwrap();
            assert_eq!((o.items, o.of), (vec!["total".to_string()], Some(super::Of::TableFields(id))), "numeric ID before another key");
        }
        let before = r#"gp.run {"tool":"summaryStatistics","params":{"case":[""#;
        let o = unasked(&format!(r#"{before}"],"input":{id}}}}}"#), before.chars().count(), &[], &app.session).unwrap();
        assert_eq!(
            (o.items, o.of),
            (["total".to_string()].to_vec(), Some(super::Of::TableFields(id))),
            "numeric standalone table ID after array"
        );
        let s = r#"gp.run {"tool": "summaryStatistics", "params": {"case": [""#;
        let o = unasked(&format!(r#"{s}"], "input": "Cities"}}}}"#), s.chars().count(), &[], &app.session).unwrap();
        assert_eq!(o.items, fields, "the input after a field array");
        let s = r#"gp.run {"tool": "summaryStatistics", "params": {"field": ""#;
        let o = unasked(&format!(r#"{s}", "input": "Cities"}}}}"#), s.chars().count(), &[], &app.session).unwrap();
        assert_eq!(o.items, numeric, "the input after the field");
        let o = offer(&app, r#"gp.run {"tool": "summaryStatistics", "params": {"input": "Stats", "field": ""#).unwrap();
        assert_eq!(o.items, ["total"], "a table's fields");
        assert!(offer(&app, r#"gp.run {"tool": "summaryStatistics", "params": {"field": ""#).is_none(), "no input");
        assert!(offer(&app, r#"gp.run {"tool": "summaryStatistics", "params": {"input": "Nowhere", "field": ""#).is_none());
        let o = offer(&app, r#"gp.run {"tool": "buffer", "params": {"units": ""#).unwrap();
        // Mere choices: no help, and no room kept for one beside them (#2618).
        assert!(!crate::completion::Offered::has_help(&o), "choices have no help");
        assert!(
            crate::completion::Offered::has_help(&offer(&app, r#"gp.run {"tool": "buffer", "params": {"input": ""#).unwrap()),
            "layers do"
        );
        assert_eq!((o.items, o.of), (["Kilometers", "Meters", "Miles", "Feet"].map(String::from).to_vec(), Some(super::Of::Choices)));
        assert_eq!(offer(&app, r#"gp.run {"tool": "buffer", "params": {"units": "mi"#).unwrap().items, ["Miles"], "case aside");
        assert!(offer(&app, r#"gp.run {"tool": "buffer", "params": {"distance": ""#).is_none(), "a number");
        assert!(offer(&app, r#"gp.run {"tool": "buffer", "params": {"nope": ""#).is_none(), "a key the tool doesn't take");
        assert!(offer(&app, r#"gp.run {"tool": "nope", "params": {"input": ""#).is_none(), "a tool that isn't there");
        assert!(offer(&app, r#"gp.run {"tool": "buffer", "params": {"input": {"x": ""#).is_none(), "deeper");
        assert!(offer(&app, r#"task.addStep {"command": "buffer", "params": {"input": ""#).is_none(), "another command's params");
        app.session.project.tables[0].dataset.fields.push(Field::new("a\"b", FieldType::Text));
        let quoted = serde_json::to_string("a\"b").unwrap();
        let text = format!(r#"gp.run {{"tool":"pivotTable","params":{{"input":{id},"fields":[{quoted},""#);
        assert_eq!(offer(&app, &text).unwrap().items, ["total"], "escaped name before cursor excluded");
        let before = format!(r#"gp.run {{"tool":"pivotTable","params":{{"input":{id},"fields":[""#);
        let text = format!(r#"{before}",{quoted}]}}}}"#);
        assert_eq!(
            unasked(&text, before.chars().count(), &[], &app.session).unwrap().items,
            ["total"],
            "escaped name after cursor excluded"
        );
        let unicode = r#""to\u0074al""#;
        let text = format!(r#"gp.run {{"tool":"pivotTable","params":{{"input":{id},"fields":[{unicode},""#);
        assert_eq!(offer(&app, &text).unwrap().items, ["a\"b"], "Unicode escape decoded before comparison");
    }

    /// A `Fields` completion replaces only the list's last name, so earlier names stay typed.
    #[test]
    fn a_field_list_completes_only_its_last_name() {
        assert_eq!(super::list_suffix("name, pop"), (6, "pop".into()));
        assert_eq!(super::list_suffix("name;pop"), (5, "pop".into()));
        assert_eq!(super::list_suffix("name, "), (6, String::new()));
        assert_eq!(super::list_suffix("pop"), (0, "pop".into()));
    }

    /// Beside a table, its rows and the table icon; beside a tool's field, its type and the field
    /// icon; no help for a choice.
    #[test]
    fn a_tools_values_have_help_and_icons() {
        use super::{Of, item_help, item_icon};
        use rustgis_engine::data::{Dataset, Field, FieldType, GeometryType};
        let mut app = crate::RustGisApp::with_sample();
        app.session.project.add_table(Dataset::new("Stats", GeometryType::None, vec![Field::new("total", FieldType::Integer)]));
        let s = &app.session;
        assert_eq!(item_help(s, &Of::Layers, "Stats").map(|h| h.0), Some("Table · 0 rows".into()));
        assert_eq!(item_icon(s, Some(&Of::Layers), "Stats"), Some("table"));
        let of = Of::ToolFields("Stats".into());
        assert_eq!(item_help(s, &of, "total").map(|h| h.0), Some(FieldType::Integer.label().to_string()));
        assert_eq!(item_icon(s, Some(&of), "total"), Some("field"));
        assert_eq!(item_help(s, &Of::Choices, "Miles"), None);
    }

    /// In the pane: accepting a choice closes it, the cursor after its quote.
    #[test]
    fn a_tools_choice_is_accepted() {
        let mut h = pane();
        type_in(&mut h, r#"gp.run {"tool": "buffer", "params": {"units": "Mi"#);
        h.key_press(egui::Key::Enter);
        h.run();
        type_in(&mut h, "}");
        assert_eq!(h.state().workflow.script, r#"gp.run {"tool": "buffer", "params": {"units": "Miles"}"#);
    }

    /// In the pane: accepting a layer closes its name, written as a JSON string holds it.
    #[test]
    fn a_layer_is_accepted_as_json_writes_it() {
        let mut h = pane();
        h.state_mut().session.map_mut().layers[0].name = r#"Big "lakes""#.into();
        type_in(&mut h, r#"labels.style {"layer": "Big"#);
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, r#"labels.style {"layer": "Big \"lakes\"""#);
    }

    fn named_objects(app: &mut crate::RustGisApp) {
        use rustgis_engine::data::{Dataset, GeometryType};
        app.session.map_mut().name = "Atlas".into();
        app.session.project.layouts.push(rustgis_engine::map::layout::Layout::new("Sheet", 297.0, 210.0));
        let mut dataset = Dataset::new("Stats", GeometryType::None, vec![]);
        dataset.push(None, vec![]);
        dataset.push(None, vec![]);
        app.session.project.tables.push(rustgis_engine::project::Table::new(1, dataset));
        app.session
            .run("labels.class.add", &serde_json::json!({"layer": "Cities", "name": "Large", "where": "pop_max > 1000000"}))
            .unwrap();
        app.session.active_layer = Some(app.session.map().layer_by_name("Cities").unwrap().id);
    }

    #[test]
    fn project_names_are_offered_only_for_commands_that_take_them() {
        use super::Of;
        let mut app = crate::RustGisApp::with_sample();
        named_objects(&mut app);
        let offer = |app: &crate::RustGisApp, text: &str| unasked(text, text.chars().count(), &[], &app.session);
        for (text, name, of) in [
            (r#"bookmark.add {"map": "at"#, "Atlas", Of::Maps),
            (r#"layout.page {"layout": "sh"#, "Sheet", Of::Layouts),
            (r#"table.remove {"table": "st"#, "Stats", Of::Tables),
        ] {
            let o = offer(&app, text).unwrap();
            assert_eq!((o.items, o.of), (vec![name.to_string()], Some(of)));
        }
        for key in ["map", "layout", "table", "class"] {
            let text = format!("map.goToXY {{\"{key}\": \"");
            assert!(offer(&app, &text).is_none(), "a key the command doesn't take: {key}");
        }
        app.session.project.layouts.clear();
        app.session.project.tables.clear();
        assert!(offer(&app, r#"layout.page {"layout": ""#).is_none());
        assert!(offer(&app, r#"table.remove {"table": ""#).is_none());
    }

    #[test]
    fn project_name_duplicates_follow_the_commands_lookup_rules() {
        let mut app = crate::RustGisApp::with_sample();
        named_objects(&mut app);
        let mut map = app.session.map().clone();
        map.name = "ATLAS".into();
        app.session.project.maps.push(map);
        app.session.project.layouts.push(rustgis_engine::map::layout::Layout::new("SHEET", 1.0, 1.0));
        let mut table = app.session.project.tables[0].clone();
        table.id = 2;
        app.session.project.tables.push(table.clone());
        table.id = 3;
        table.name = "STATS".into();
        app.session.project.tables.push(table);
        let names = |text: &str| unasked(text, text.chars().count(), &[], &app.session).unwrap().items;
        assert_eq!(names(r#"bookmark.add {"map": ""#), ["Atlas"]);
        assert_eq!(names(r#"layout.page {"layout": ""#), ["Sheet"]);
        // Tables prefer an exact name: these two spellings each address a different table.
        assert_eq!(names(r#"table.remove {"table": ""#), ["Stats", "STATS"]);
    }

    #[test]
    fn class_values_follow_the_lines_layer_and_leave_default_out_when_refused() {
        let mut app = crate::RustGisApp::with_sample();
        named_objects(&mut app);
        app.session.run("labels.class.rename", &serde_json::json!({"layer": "Cities", "class": "Default", "name": "Base"})).unwrap();
        let offer = |app: &crate::RustGisApp, text: &str| unasked(text, text.chars().count(), &[], &app.session);
        assert_eq!(offer(&app, r#"labels.style {"class": ""#).unwrap().items, ["Large", "Base"]);
        assert_eq!(offer(&app, r#"labels.style {"layer": "Cities", "class": "la"#).unwrap().items, ["Large"]);
        let text = r#"labels.style {"class": ""#;
        let later = format!(r#"{text}", "layer": "Countries"}}"#);
        assert_eq!(unasked(&later, text.chars().count(), &[], &app.session).unwrap().items, ["Default"]);
        for id in ["labels.class.remove", "labels.class.move"] {
            let text = format!("{id} {{\"class\": \"");
            assert_eq!(offer(&app, &text).unwrap().items, ["Large"]);
            assert!(app.session.run(id, &serde_json::json!({"layer": "Cities", "class": "Base", "to": 0})).is_err());
        }
        assert!(offer(&app, r#"labels.style {"layer": "Missing", "class": ""#).is_none());
        app.session.active_layer = None;
        assert!(offer(&app, r#"labels.style {"class": ""#).is_none());
    }

    #[test]
    fn project_values_have_their_own_help_and_icons() {
        use super::{Of, item_help, item_icon};
        let mut app = crate::RustGisApp::with_sample();
        named_objects(&mut app);
        let s = &app.session;
        let city = s.map().layer_by_name("Cities").unwrap().id;
        for (of, name, text, icon) in [
            (Of::Maps, "Atlas", "4 layers", "map"),
            (Of::Layouts, "Sheet", "297 × 210 mm", "new_layout"),
            (Of::Tables, "Stats", "2 rows", "table"),
            (Of::Classes(city), "Large", "pop_max > 1000000", "label"),
            (Of::Classes(city), "Default", "All features", "label"),
        ] {
            assert_eq!(item_help(s, &of, name).unwrap().0, text);
            assert_eq!(item_icon(s, Some(&of), name), Some(icon));
        }
    }

    #[test]
    fn project_values_are_accepted_as_json_strings_in_the_pane() {
        for (id, key, prefix) in [
            ("bookmark.add", "map", "Atlas"),
            ("layout.page", "layout", "Sheet"),
            ("table.remove", "table", "Stats"),
            ("labels.style", "class", "Large"),
        ] {
            let mut h = pane();
            named_objects(h.state_mut());
            let name = format!("{prefix} \"北\"");
            let s = &mut h.state_mut().session;
            match key {
                "map" => s.map_mut().name = name.clone(),
                "layout" => s.project.layouts[0].name = name.clone(),
                "table" => s.project.tables[0].name = name.clone(),
                _ => {
                    let id = s.active_layer.unwrap();
                    s.map_mut().layer_mut(id).unwrap().labels.classes[0].name = name.clone();
                }
            }
            type_in(&mut h, &format!("{id} {{\"{key}\": \"{prefix}"));
            h.key_press(egui::Key::Enter);
            h.run();
            let (_, params) = h.state().workflow.script.split_once(' ').unwrap();
            let params: serde_json::Value = serde_json::from_str(&format!("{params}}}")).unwrap();
            assert_eq!(params[key], name);
        }
    }

    /// In the pane: a quote after `"tool": ` in a `gp.run` line offers the tools with the
    /// highlighted one's help beside; accepting closes the id (a quote already there is kept),
    /// the cursor after it.
    #[test]
    fn a_tool_id_is_offered_and_accepted() {
        use egui_kittest::kittest::Queryable;
        let mut h = pane();
        type_in(&mut h, r#"gp.run {"tool": "buf"#);
        let tool = super::tool_help(rustgis_engine::analysis::tool("buffer").unwrap());
        h.get_by_label(&format!("{}{}", tool.id, tool.keys));
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, r#"gp.run {"tool": "buffer""#);
        let mut h = pane();
        type_in(&mut h, r#"gp.run {"tool": "buf"}"#);
        for _ in 0..2 {
            h.key_press(egui::Key::ArrowLeft);
            h.run();
        }
        h.key_press(egui::Key::Enter);
        h.run();
        type_in(&mut h, ",");
        assert_eq!(h.state().workflow.script, r#"gp.run {"tool": "buffer",}"#, "the cursor after the quote");
    }

    /// In a second-level object, the keys its entry describes (`"add": {…}`, each object of
    /// `"filters": [{…}]`), less those that object has, with the key's line beside; none for an
    /// entry that describes none.
    #[test]
    fn a_second_level_objects_keys_are_offered() {
        let app = crate::RustGisApp::with_sample();
        let offer = |s: &str| unasked(s, s.chars().count(), &[], &app.session);
        let keys = |s: &str| offer(s).map(|o| o.items).unwrap_or_default();
        assert_eq!(keys(r#"topology.rules {"add": {""#), ["rule", "layer", "by"]);
        assert_eq!(keys(r#"topology.rules {"add": {"rule": "x", ""#), ["layer", "by"]);
        assert_eq!(keys(r#"layer.displayFilters {"filters": [{"name": "a", ""#), ["minScale", "maxScale", "where"]);
        let s = r#"layer.displayFilters {"filters": [{"name": "a"}, {""#;
        assert_eq!(keys(s), ["name", "minScale", "maxScale", "where"], "another object: nothing given in it");
        let s = r#"layer.displayFilters {"filters": [{"na"#;
        let later = unasked(&format!(r#"{s}"}}, {{"name": "b"}}]}}"#), s.chars().count(), &[], &app.session);
        assert_eq!(later.map(|o| o.items), Some(vec!["name".to_string()]), "a later object of the array isn't this one");
        assert!(keys(r#"topology.rules {"remove": {""#).is_empty(), "an entry that describes none");
        assert!(keys(r#"topology.rules {"nope": {""#).is_empty(), "no such entry");
        let of = offer(r#"topology.rules {"add": {""#).and_then(|o| o.of).unwrap();
        assert_eq!(of, super::Of::SubKeys { id: "topology.rules".into(), key: "add".into() });
        let (line, about) = super::item_help(&app.session, &of, "rule").unwrap();
        assert!(line.starts_with(r#""rule": "mustNotOverlap""#), "{line}");
        assert_eq!(about, None);
    }

    /// In a `task.addStep` line, `"command": "` offers the commands a script can run, its
    /// `"params"` that command's keys (the command before or after them), and the help by the
    /// cursor there is that command's.
    #[test]
    fn a_steps_command_and_its_keys_are_offered() {
        let app = crate::RustGisApp::with_sample();
        let ids = super::script_ids(&app.session);
        let offer = |s: &str| unasked(s, s.chars().count(), &ids, &app.session);
        let o = offer(r#"task.addStep {"command": "LABELS.ST"#).unwrap();
        assert_eq!((o.items, o.of), (vec!["labels.style".to_string()], Some(super::Of::CommandIds)));
        assert!(offer(r#"task.addStep {"command": "ui."#).is_none(), "no interface command");
        assert!(offer(r#"task.addStep {"command": "task."#).is_none(), "no task command: a step doesn't run a task");
        let o = offer(r#"task.addStep {"command": "script."#).map(|o| o.items).unwrap_or_default();
        assert!(o.iter().any(|id| id == "script.run"), "what task.addStep takes, not what a script runs: {o:?}");
        let all = offer(r#"task.addStep {"command": ""#).map(|o| o.items).unwrap_or_default();
        assert!(all.len() > 10 && all.is_sorted(), "every command, sorted as a line's first word offers them");
        assert!(offer(r#"task.addStep {"title": "labels.st"#).is_none(), "another key's value");
        let keys = |s: &str| offer(s).map(|o| o.items).unwrap_or_default();
        assert_eq!(keys(r#"task.addStep {"command": "labels.style", "params": {"si"#), ["size"]);
        let s = r#"task.addStep {"params": {"si"#;
        let after = unasked(&format!(r#"{s}"}}, "command": "labels.style"}}"#), s.chars().count(), &ids, &app.session);
        assert_eq!(after.map(|o| o.items), Some(vec!["size".to_string()]), "the command after params");
        assert!(keys(r#"task.addStep {"params": {""#).is_empty(), "no command");
        assert!(keys(r#"task.addStep {"command": "nope", "params": {""#).is_empty(), "an unknown command");
        let help = |s: &str| {
            super::hint_at(s, s.chars().count()).and_then(|h| super::help_at(&app.session, s, s.chars().count(), &h)).map(|h| h.id)
        };
        assert_eq!(help(r#"task.addStep {"command": "labels.style", "params": {"#).as_deref(), Some("labels.style"));
        assert_eq!(help(r#"task.addStep {"command": "labels.style", "#).as_deref(), Some("task.addStep"), "outside params");
    }

    /// In the pane: a step's command is offered and closed on accept, then its keys in `params`.
    #[test]
    fn a_steps_command_then_its_key_is_accepted() {
        let mut h = pane();
        type_in(&mut h, r#"task.addStep {"command": "labels.st"#);
        h.key_press(egui::Key::Enter);
        h.run();
        type_in(&mut h, r#", "params": {"si"#);
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, r#"task.addStep {"command": "labels.style", "params": {"size": "#);
    }

    /// In a `task.addStep` line's `"params"`, the values its command takes are offered as in a
    /// line of that command (#1240): its layers, project names, label classes and fields, over the
    /// step's layer or table, given in its `"params"` or the active one; the command before or
    /// after them. Nothing without a command, for an unknown one, or for a key it doesn't take.
    #[test]
    fn a_steps_values_are_offered_as_its_commands() {
        use super::Of;
        let mut app = crate::RustGisApp::with_sample();
        named_objects(&mut app);
        let offer = |app: &crate::RustGisApp, s: &str| unasked(s, s.chars().count(), &[], &app.session);
        let o = offer(&app, r#"task.addStep {"command": "labels.field", "params": {"layer": "ci"#).unwrap();
        assert_eq!((o.items, o.of), (vec!["Cities".to_string()], Some(Of::Layers)));
        let s = r#"task.addStep {"params": {"layer": "ci"#;
        let after = unasked(&format!(r#"{s}"}}, "command": "labels.field"}}"#), s.chars().count(), &[], &app.session);
        assert_eq!(after.map(|o| o.items), Some(vec!["Cities".to_string()]), "the command after params");
        assert!(offer(&app, r#"task.addStep {"params": {"layer": ""#).is_none(), "no command");
        assert!(offer(&app, r#"task.addStep {"command": "nope", "params": {"layer": ""#).is_none(), "an unknown command");
        let s = r#"task.addStep {"command": "map.goToXY", "params": {"layer": ""#;
        assert!(offer(&app, s).is_none(), "a key the command doesn't take");
        for (text, name, of) in [
            (r#"task.addStep {"command": "bookmark.add", "params": {"map": "at"#, "Atlas", Of::Maps),
            (r#"task.addStep {"command": "layout.page", "params": {"layout": "sh"#, "Sheet", Of::Layouts),
            (r#"task.addStep {"command": "table.remove", "params": {"table": "st"#, "Stats", Of::Tables),
        ] {
            let o = offer(&app, text).unwrap();
            assert_eq!((o.items, o.of), (vec![name.to_string()], Some(of)));
        }
        // Fields and classes: of the layer the step's `"params"` give, else the active one (Cities).
        let cities = app.session.map().layer_by_name("Cities").unwrap();
        let (id, fields): (u32, Vec<String>) = (cities.id, cities.dataset.fields.iter().map(|f| f.name.clone()).collect());
        let numeric: Vec<String> = cities.dataset.fields.iter().filter(|f| f.ty.is_numeric()).map(|f| f.name.clone()).collect();
        let o = offer(&app, r#"task.addStep {"command": "labels.field", "params": {"field": ""#).unwrap();
        assert_eq!((o.items, o.of), (fields, Some(Of::Fields(id))), "the active layer");
        let countries = app.session.map().layer_by_name("Countries").unwrap().id;
        let o = offer(&app, r#"task.addStep {"command": "labels.field", "params": {"layer": "Countries", "field": ""#).unwrap();
        assert_eq!(o.of, Some(Of::Fields(countries)), "the step's layer");
        let s = r#"task.addStep {"command": "labels.field", "params": {"field": ""#;
        let o = unasked(&format!(r#"{s}", "layer": "Countries"}}}}"#), s.chars().count(), &[], &app.session).unwrap();
        assert_eq!(o.of, Some(Of::Fields(countries)), "the step's layer after the field");
        let s = r#"task.addStep {"layer": "Countries", "command": "labels.field", "params": {"field": ""#;
        assert_eq!(offer(&app, s).unwrap().of, Some(Of::Fields(id)), "not the line's: a step's layer is in its params");
        let s = r#"task.addStep {"command": "labels.field", "params": {"field": ""#;
        let o = unasked(&format!(r#"{s}"}}, "x": {{"layer": "Countries"}}}}"#), s.chars().count(), &[], &app.session).unwrap();
        assert_eq!(o.of, Some(Of::Fields(id)), "nor a later object's");
        assert!(offer(&app, r#"task.addStep {"command": "labels.field", "params": {"layer": "Nowhere", "field": ""#).is_none());
        let s = format!(r#"task.addStep {{"command": "data.explore", "params": {{"fields": ["{}", ""#, numeric[0]);
        assert_eq!(offer(&app, &s).unwrap().items, numeric[1..], "[numeric fields], not one given before");
        let s = r#"task.addStep {"command": "data.explore", "params": {"fields": [""#;
        let o = unasked(&format!(r#"{s}"], "layer": "Countries"}}}}"#), s.chars().count(), &[], &app.session).unwrap();
        assert_eq!(o.of, Some(Of::Fields(countries)), "the step's layer after the array");
        assert!(offer(&app, r#"task.addStep {"command": "labels.style", "params": {"layer": [""#).is_none(), "a layer in an array");
        let table = app.session.project.add_table(rustgis_engine::data::Dataset::new(
            "Addresses",
            rustgis_engine::data::GeometryType::None,
            vec![rustgis_engine::data::Field::new("street", rustgis_engine::data::FieldType::Text)],
        ));
        let o = offer(&app, r#"task.addStep {"command": "layer.geocode", "params": {"table": "addresses", "address": [""#).unwrap();
        assert_eq!((o.items, o.of), (vec!["street".to_string()], Some(Of::TableFields(table))), "the step's table");
        let o = offer(&app, r#"task.addStep {"command": "labels.style", "params": {"class": "la"#).unwrap();
        assert_eq!((o.items, o.of), (vec!["Large".to_string()], Some(Of::Classes(id))));
        let s = r#"task.addStep {"command": "labels.style", "params": {"layer": "Countries", "class": "la"#;
        assert!(offer(&app, s).is_none(), "the step's layer's classes");
        let o = offer(&app, r#"task.addStep {"command": "labels.class.remove", "params": {"class": ""#).unwrap();
        assert_eq!(o.items, ["Large"], "without Default, which it can't remove");
    }

    /// In the pane: a layer accepted in a step's `"params"` is closed, as in a line.
    #[test]
    fn a_steps_layer_is_accepted() {
        let mut h = pane();
        type_in(&mut h, r#"task.addStep {"command": "labels.field", "params": {"layer": "Citi"#);
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().workflow.script, r#"task.addStep {"command": "labels.field", "params": {"layer": "Cities""#);
    }

    /// What follows a key's cursor, up to the end of its object: past the key's closing quote,
    /// nested braces and strings with braces in them; all of it when nothing closes it.
    #[test]
    fn object_rest_stops_at_the_keys_object() {
        use super::object_rest;
        assert_eq!(object_rest(r#"me": "a"}, {"name": "b"}]}"#, false), r#"me": "a"}"#);
        assert_eq!(
            object_rest(r#"": {"b": [1]}, "c": "}"}, "d": 1}"#, false),
            r#"": {"b": [1]}, "c": "}"}"#,
            "nested, and a brace in a string"
        );
        assert_eq!(object_rest(r#"a\"b": 1}"#, false), r#"a\"b": 1}"#, "an escaped quote in the key");
        assert_eq!(object_rest(r#"": 1, "b""#, false), r#"": 1, "b""#, "not closed");
        assert_eq!(object_rest("", false), "");
    }
}
