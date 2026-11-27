//! Find and replace in the Script pane (#2996): what the find bar looks for in the script, and the
//! text a replace leaves. Positions are characters, as the box's cursor counts them. The matching
//! is plain text (no patterns), and worked out apart from the box so it is tested on its own.

use std::ops::Range;

/// The most matches kept: a script this long with this many hits is not read by eye, and the
/// highlight is laid out each time the text changes.
pub const MAX_MATCHES: usize = 10_000;

/// Where `query` is in a script.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Found {
    /// Each match, in characters, in order and apart from each other.
    pub ranges: Vec<Range<usize>>,
    /// More than [`MAX_MATCHES`] hits: the rest are not kept.
    pub capped: bool,
}

/// Every place `query` is in `text`, from the start, a match taken before the next is looked for
/// (so `aa` is twice in `aaaa`). `case`: capitals count; otherwise a letter is its lower case.
/// `whole`: only a match with no letter, digit or `_` against it on either side (a whole word, as a
/// code editor's Match Whole Word takes it). An empty query is nowhere.
pub fn matches(text: &str, query: &str, case: bool, whole: bool) -> Found {
    // A letter whose lower case is several characters (`İ`) stays as it is, so it matches only
    // itself and the positions stay the text's.
    let fold = |c: char| {
        let mut lower = c.to_lowercase();
        match (lower.next(), lower.next()) {
            (Some(l), None) if !case => l,
            _ => c,
        }
    };
    let query: Vec<char> = query.chars().map(fold).collect();
    let mut found = Found::default();
    if query.is_empty() {
        return found;
    }
    let word = |c: Option<char>| c.map(fold).is_some_and(|c| c.is_alphanumeric() || c == '_');
    // Knuth, Morris and Pratt (1977): each character of the text is read once, whatever the two
    // hold, so a long query against a long run of one letter costs no more than any other.
    // `back[k]`: how much of the query still matches when its character after `k + 1` doesn't.
    let mut back = Vec::with_capacity(query.len());
    let mut matched = 0;
    for (k, c) in query.iter().enumerate() {
        while matched > 0 && query.get(matched) != Some(c) {
            matched = back.get(matched - 1).copied().unwrap_or(0);
        }
        matched += usize::from(k > 0 && query.get(matched) == Some(c));
        back.push(matched);
    }
    let mut matched = 0;
    // The script is read as it goes, never copied (the box takes any size): `rest` is read once, and
    // `behind`, which only moves forward, gives the character before a match.
    let mut rest = text.chars().enumerate().peekable();
    let (mut behind, mut passed) = (text.chars(), 0);
    while let Some((i, c)) = rest.next() {
        let c = &fold(c);
        while matched > 0 && query.get(matched) != Some(c) {
            matched = back.get(matched - 1).copied().unwrap_or(0);
        }
        matched += usize::from(query.get(matched) == Some(c));
        if matched < query.len() {
            continue;
        }
        let at = i + 1 - query.len();
        let before = at.checked_sub(1).filter(|_| whole).and_then(|b| {
            let c = behind.nth(b.saturating_sub(passed));
            passed = b + 1;
            c
        });
        if !whole || (!word(before) && !word(rest.peek().map(|&(_, c)| c))) {
            if found.ranges.len() == MAX_MATCHES {
                found.capped = true;
                break;
            }
            found.ranges.push(at..i + 1);
            // The next is looked for after this one.
            matched = 0;
        } else {
            // Not a whole word: one that overlaps it may still be.
            matched = back.last().copied().unwrap_or(0);
        }
    }
    found
}

/// The match after (or, `back`, before) `current` of `len`, going round at the ends; the first
/// (the last, `back`) when none is current. None when there are no matches.
pub fn step(len: usize, current: Option<usize>, back: bool) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(match current.map(|c| c.min(last)) {
        None => {
            if back {
                last
            } else {
                0
            }
        }
        Some(c) => (c + if back { last } else { 1 }) % len,
    })
}

/// The index of the first match at or after the character `from`, else the first (it goes round).
pub fn first_from(found: &Found, from: usize) -> Option<usize> {
    let at = found.ranges.partition_point(|r| r.start < from);
    (!found.ranges.is_empty()).then_some(if at < found.ranges.len() { at } else { 0 })
}

/// `text` with the characters of each of `ranges` (in order, apart from each other) replaced by
/// `with`.
pub fn replace(text: &str, ranges: &[Range<usize>], with: &str) -> String {
    /// `s` cut after its first `n` characters.
    fn after(s: &str, n: usize) -> (&str, &str) {
        let byte = s.char_indices().nth(n).map_or(s.len(), |(byte, _)| byte);
        s.split_at_checked(byte).unwrap_or((s, ""))
    }
    let mut out = String::with_capacity(text.len());
    // What is left of the text, and the character it starts at.
    let (mut rest, mut at) = (text, 0);
    for r in ranges {
        let (kept, from) = after(rest, r.start.saturating_sub(at));
        out.push_str(kept);
        out.push_str(with);
        rest = after(from, r.end.saturating_sub(r.start)).1;
        at = r.end.max(r.start);
    }
    out.push_str(rest);
    out
}

/// `new` in place of `script`, as one step of the box's Undo; the same text records none.
fn write(ctx: &egui::Context, box_id: egui::Id, script: &mut String, new: String) {
    if new != *script {
        crate::analysispane::keep_for_undo_of(ctx, box_id, script);
        *script = new;
    }
}

/// The most characters of a query, and of a replacement: what each field takes, and so of a
/// selection that fills the query when the bar opens (a longer one is a passage, not a word to look
/// for). Replace All writes the replacement once per match, up to [`MAX_MATCHES`].
const FIELD_MAX: usize = 200;

/// A field of the bar: its text, never over [`FIELD_MAX`], and what the last write of it left out.
#[derive(Clone, Default)]
struct Field {
    text: String,
    left_out: usize,
}

impl Field {
    /// Before the field `id` is drawn: what is pasted or typed into it is cut to the most it could
    /// take, so a huge clipboard is never laid out in it. How many characters that dropped, for
    /// [`Self::written`].
    fn cut_incoming(ui: &egui::Ui, id: egui::Id) -> usize {
        if !ui.memory(|m| m.has_focus(id)) {
            return 0;
        }
        ui.input_mut(|i| {
            let mut dropped = 0;
            for event in &mut i.events {
                if let egui::Event::Paste(text) | egui::Event::Text(text) = event
                    && let Some((end, _)) = text.char_indices().nth(FIELD_MAX)
                {
                    dropped += text.get(end..).map_or(0, |rest| rest.chars().count());
                    text.truncate(end);
                }
            }
            dropped
        })
    }

    /// After any write of the text (typed, pasted or filled from a selection): cut to what the field
    /// takes, keeping how many characters that left out: a paste's own count, one more for each key
    /// typed while the field stays over, none once a write fits. Given back, for a notice, as the
    /// field goes from fitting to cut and for each paste; not for those keys.
    fn written(&mut self, dropped: usize) -> Option<usize> {
        let before = self.left_out;
        let mut now = dropped;
        if let Some((end, _)) = self.text.char_indices().nth(FIELD_MAX) {
            now += self.text.get(end..).map_or(0, |rest| rest.chars().count());
            self.text.truncate(end);
        }
        self.left_out = if now == 1 { before + 1 } else { now };
        (now > 1 || (now == 1 && before == 0)).then_some(self.left_out)
    }
}

/// Which field, to say what it left out.
#[derive(Clone, Copy)]
enum Which {
    Find,
    Replace,
}

/// What a field left out of a longer text, as the bar and Messages say it.
fn left_out(which: Which, n: usize) -> String {
    let (max, n) = (FIELD_MAX.to_string(), n.to_string());
    let args = [("max", max.as_str()), ("n", n.as_str())];
    // Each sentence a literal where it is formatted, for the translators to find.
    match which {
        Which::Find => crate::i18n::fmt("The Find field takes {max} characters: {n} left out", &args),
        Which::Replace => crate::i18n::fmt("The Replace field takes {max} characters: {n} left out", &args),
    }
}

/// The find bar's state, kept with the Script pane's (and started over with it for another
/// project): it isn't saved with the project.
#[derive(Default)]
pub struct Bar {
    open: bool,
    query: Field,
    replace: Field,
    case: bool,
    /// Only whole words match.
    whole: bool,
    /// The current match is the first from this character: where the cursor was when the bar
    /// opened or the script was last edited, the match last stepped to, or the end of a
    /// replacement.
    anchor: usize,
    /// Scroll the current match into view when it is next painted.
    scroll: bool,
    /// Put the focus in the query field, its text selected.
    focus_query: bool,
    /// The widget of the bar (a field, a button, a toggle) the keyboard was on when the bar was
    /// last drawn, with that pass's number, none when it was elsewhere: where the bar's keys work,
    /// besides the box.
    focus: Option<(egui::Id, u64)>,
    /// The Replace row is shown.
    replace_open: bool,
    /// The matches [`Bar::refresh`] last worked out: read only after it.
    found: Found,
    /// What `found` was worked out for: the script's hash, the query and the toggles.
    key: Option<(u64, String, bool, bool)>,
}

impl Bar {
    /// Whether the bar shows, so F3, Shift+F3 and Escape are its keys.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the keyboard was on a widget of the bar in the pass before `pass`: the app's
    /// shortcuts then leave Escape to the bar, though egui has taken the focus away by now. Stale
    /// when the bar isn't drawn (its pane closed), so nothing is held off by a bar not shown.
    pub(crate) fn had_focus(&self, pass: u64) -> bool {
        self.focus.is_some_and(|(_, p)| p + 1 == pass)
    }

    /// The matches of `script`, worked out again if it, the query or a toggle changed since:
    /// called wherever the matches are used, so they are never of an older text. `cursor`: the
    /// box's cursor, where the current match is picked from once the script is edited.
    fn refresh(&mut self, script: &str, cursor: usize) {
        let key = (egui::util::hash(script), self.query.text.clone(), self.case, self.whole);
        if self.key.as_ref() != Some(&key) {
            // The text changed under the bar (typed in the box): the matches shifted.
            if self.key.as_ref().is_some_and(|old| old.0 != key.0) {
                self.anchor = cursor;
            }
            self.found = matches(script, &self.query.text, self.case, self.whole);
            self.key = Some(key);
        }
    }

    /// The match the next and previous go from, an index of `found`.
    fn current(&self) -> Option<usize> {
        first_from(&self.found, self.anchor)
    }

    /// The bar closed. What it found goes with it (up to 10,000 ranges), and is worked out again on
    /// opening.
    fn close(&mut self) {
        self.open = false;
        self.focus = None;
        self.found = Found::default();
        self.key = None;
    }

    /// The bar moved to the match `at` of the found ones.
    fn go(&mut self, at: Option<usize>) {
        if let Some(r) = at.and_then(|i| self.found.ranges.get(i)) {
            self.anchor = r.start;
            self.scroll = true;
        }
    }
}

/// What a key or a button of the box and its bar does.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    Find,
    FindAndReplace,
    Next,
    Previous,
    Replace,
    ReplaceAll,
    Close,
}

/// A shortcut of the box and its bar: Ctrl or ⌘ held (`command`), Alt and Shift, as given, and the
/// key.
#[derive(Clone, Copy, PartialEq)]
struct Chord {
    command: bool,
    alt: bool,
    shift: bool,
    key: egui::Key,
    does: Action,
}

const fn chord(command: bool, alt: bool, shift: bool, key: egui::Key, does: Action) -> Chord {
    Chord { command, alt, shift, key, does }
}

/// The shortcuts of a code editor, the one list the box takes its keys from and the web app keeps
/// from the browser: Ctrl+F (⌘F) finds; Ctrl+H, ⌥⌘F on a Mac (where ⌘H hides the app), finds with
/// the Replace row; F3 and Shift+F3 step. Alt and Shift are as listed: elsewhere than a Mac,
/// Ctrl+Alt is AltGr, and AltGr+F types a letter on many layouts.
fn chords(mac: bool) -> [Chord; 4] {
    use egui::Key;
    let replace = if mac {
        chord(true, true, false, Key::F, Action::FindAndReplace)
    } else {
        chord(true, false, false, Key::H, Action::FindAndReplace)
    };
    [
        chord(true, false, false, Key::F, Action::Find),
        replace,
        chord(false, false, true, Key::F3, Action::Previous),
        chord(false, false, false, Key::F3, Action::Next),
    ]
}

impl Chord {
    fn is_pressed_with(&self, m: egui::Modifiers) -> bool {
        m.command == self.command && m.alt == self.alt && m.shift == self.shift
    }

    /// Takes this chord's presses out of the frame's events, so nothing else sees them.
    fn take(&self, ui: &egui::Ui) -> bool {
        ui.input_mut(|i| {
            let before = i.events.len();
            i.events.retain(|e| {
                !matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. }
                    if *key == self.key && self.is_pressed_with(*modifiers))
            });
            before != i.events.len()
        })
    }
}

/// The keyboard is in the Script box or on a widget of its bar: where the shortcuts work.
fn here(ctx: &egui::Context, box_id: egui::Id, bar: &Bar) -> bool {
    ctx.memory(|m| m.focused()).is_some_and(|f| f == box_id || bar.focus.is_some_and(|(w, _)| w == f))
}

/// The bar closed from itself (`select`: the cursor goes where the search was, the current match
/// selected, in the box) or from the box, whose cursor stays; the keyboard goes to the box.
fn dismiss(ctx: &egui::Context, box_id: egui::Id, bar: &mut Bar, select: bool) {
    if let Some(r) = bar.current().and_then(|i| bar.found.ranges.get(i)).filter(|_| select) {
        use egui::text::{CCursor, CCursorRange};
        crate::widgets::set_cursor_range(ctx, box_id, CCursorRange::two(CCursor::new(r.start), CCursor::new(r.end)));
    }
    bar.close();
    ctx.memory_mut(|m| m.request_focus(box_id));
}

/// Escape closes an open bar, from the box (after a list or the help over it took its own, read
/// before this) or from any widget of the bar, which egui has taken the focus off by now.
fn escape(ui: &egui::Ui, box_id: egui::Id, script: &str, bar: &mut Bar) {
    let focused = ui.memory(|m| m.focused());
    let boxed = focused == Some(box_id);
    let barred = bar.focus.is_some_and(|(w, _)| focused.is_none_or(|f| f == w));
    if bar.open && (boxed || barred) && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        bar.refresh(script, bar.anchor);
        dismiss(ui.ctx(), box_id, bar, !boxed);
    }
}

/// The chords the box and its bar take now: the next and previous match only with the bar `open`.
fn taken_now(mac: bool, open: bool) -> impl Iterator<Item = Chord> {
    chords(mac).into_iter().filter(move |c| open || !matches!(c.does, Action::Next | Action::Previous))
}

/// Before the box: the shortcuts pressed in the box or the bar ([`chords`]). Finding opens the bar,
/// with the selection in its field; the next and previous match move an open one; Escape closes it.
pub fn keys(ui: &egui::Ui, box_id: egui::Id, script: &str, bar: &mut Bar) {
    escape(ui, box_id, script, bar);
    if !here(ui.ctx(), box_id, bar) {
        return;
    }
    for chord in taken_now(ui.ctx().os().is_mac(), bar.open) {
        if !chord.take(ui) {
            continue;
        }
        let stepping = matches!(chord.does, Action::Next | Action::Previous);
        let range = crate::widgets::cursor_range(ui.ctx(), box_id);
        if stepping {
            bar.refresh(script, range.map_or(bar.anchor, |r| r.primary.index.0));
            bar.go(step(bar.found.ranges.len(), bar.current(), chord.does == Action::Previous));
            continue;
        }
        if let Some(r) = range.filter(|_| ui.memory(|m| m.has_focus(box_id))) {
            let (a, b) = (r.primary.index.0.min(r.secondary.index.0), r.primary.index.0.max(r.secondary.index.0));
            // No more of a selection than tells whether it fits the field.
            let picked: String = script.chars().skip(a).take((b - a).min(FIELD_MAX + 1)).collect();
            let word = !picked.is_empty() && picked.chars().count() <= FIELD_MAX && !picked.contains(['\n', '\r']);
            // The same query keeps what it left out of a longer one.
            if word && bar.query.text != picked {
                bar.query.text = picked;
                bar.query.written(0);
            }
            bar.anchor = a;
        }
        bar.open = true;
        bar.replace_open |= chord.does == Action::FindAndReplace;
        bar.scroll = true;
        bar.focus_query = true;
    }
}

/// The chords the Script box and its bar take at one moment, none while neither has the focus:
/// what the web app keeps from the browser's own Find and History.
#[derive(Clone, Default, PartialEq)]
pub struct FindChords(Vec<Chord>);

impl FindChords {
    /// Whether a browser key press (its `key` and `code`, Ctrl or ⌘ held, Alt, Shift) is one of
    /// them, read as the box reads it ([`Chord::is_pressed_with`]).
    pub fn takes(&self, key: &str, code: &str, command: bool, alt: bool, shift: bool) -> bool {
        let held = egui::Modifiers { command, alt, shift, ..egui::Modifiers::default() };
        crate::keymap::browser_key(key, code).is_some_and(|k| self.0.iter().any(|c| c.key == k && c.is_pressed_with(held)))
    }
}

/// [`FindChords`] as they are now.
pub fn browser_keys(ctx: &egui::Context, bar: &Bar) -> FindChords {
    if !here(ctx, egui::Id::unique("script_text"), bar) {
        return FindChords::default();
    }
    FindChords(taken_now(ctx.os().is_mac(), bar.open).collect())
}

/// The room the bar leaves at the box's edges, and the widths it takes: what the count takes, the
/// toggle that shows Replace, and the least and most the bar is wide (egui keeps it on screen when
/// the box is narrower).
const EDGE: f32 = 8.0;
const COUNT_WIDTH: f32 = 52.0;
const TOGGLE_WIDTH: f32 = 16.0;
const MIN_BAR: f32 = 230.0;
const MAX_BAR: f32 = 380.0;
const GAP: f32 = 4.0;
/// The least a field is wide: its margins and the toggles inside it, and some forty points of text.
const MIN_FIELD: f32 = 100.0;

/// The toggles inside the right end of the query field `input`, as in a code editor: Match Case
/// (`Aa`) and Match Whole Word (`ab` under a bracket). Each is `on` or off; the one clicked is
/// returned.
fn field_toggles(ui: &mut egui::Ui, id: egui::Id, input: egui::Rect, case: bool, whole: bool) -> Option<Toggle> {
    let t = crate::theme::Tokens::get(ui.ctx());
    let mut clicked = None;
    let toggles = [(Toggle::Word, whole, tl!("Match Whole Word")), (Toggle::Case, case, tl!("Match Case"))];
    for (k, (toggle, on, label)) in toggles.into_iter().enumerate() {
        let r =
            egui::Rect::from_min_size(egui::pos2(input.right() - 26.0 - 24.0 * k as f32, input.center().y - 9.0), egui::vec2(22.0, 18.0));
        let resp = ui.interact(r, id.with(k), egui::Sense::click());
        if on {
            ui.painter().rect_filled(r, egui::CornerRadius::same(3), t.accent.gamma_multiply(0.3));
            ui.painter().rect_stroke(r, egui::CornerRadius::same(3), egui::Stroke::new(1.0, t.accent), egui::StrokeKind::Inside);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, egui::CornerRadius::same(3), t.hover);
        }
        let ink = if on { t.text } else { t.text_dim };
        let c = r.center();
        match toggle {
            Toggle::Case => {
                ui.painter().text(c, egui::Align2::CENTER_CENTER, "Aa", egui::FontId::proportional(12.0), ink);
            }
            Toggle::Word => {
                ui.painter().text(c - egui::vec2(0.0, 1.5), egui::Align2::CENTER_CENTER, "ab", egui::FontId::proportional(11.0), ink);
                // A bracket under the word, as a code editor draws it.
                let (y, stroke) = (c.y + 6.0, egui::Stroke::new(1.2, ink));
                let (l, rr) = (c.x - 7.0, c.x + 7.0);
                ui.painter().line_segment([egui::pos2(l, y - 2.5), egui::pos2(l, y)], stroke);
                ui.painter().line_segment([egui::pos2(l, y), egui::pos2(rr, y)], stroke);
                ui.painter().line_segment([egui::pos2(rr, y), egui::pos2(rr, y - 2.5)], stroke);
            }
        }
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, on, label.to_string()));
        if resp.on_hover_text(label).clicked() {
            clicked = Some(toggle);
        }
    }
    clicked
}

/// A toggle inside the query field.
#[derive(Clone, Copy)]
enum Toggle {
    Case,
    Word,
}

/// The find bar, drawn over the top right of the box (`area`, as laid out), as in a code editor: a
/// field with Match case inside it, the count and the previous, next and close buttons; and,
/// under the arrow at its left, a field for what to replace with and the Replace and Replace All
/// buttons. Does what it's asked: a replace changes `script` as one undo step of the box, which
/// `running` forbids. Returns what to say in Messages: what a field left out of a longer text, or
/// why a replace was refused.
pub fn show(ui: &mut egui::Ui, box_id: egui::Id, script: &mut String, running: bool, area: egui::Rect, bar: &mut Bar) -> Vec<String> {
    use egui::text::{CCursor, CCursorRange};
    let (query_id, replace_id) = (box_id.with("find_query"), box_id.with("find_replace"));
    // What a field lost this frame, for Messages too.
    let mut said = Vec::new();
    // Over the box only while it shows: scrolled out of the pane, the bar would float over what is
    // there instead.
    let seen = ui.clip_rect();
    if !bar.open || !seen.intersects(area) {
        bar.focus = None;
        return said;
    }
    let cursor = crate::widgets::cursor_range(ui.ctx(), box_id).map_or(bar.anchor, |r| r.primary.index.0);
    bar.refresh(script, cursor);
    let total = bar.found.ranges.len();
    let capped = bar.found.capped;
    let width = (area.width() - 2.0 * EDGE).clamp(MIN_BAR, MAX_BAR);
    let pos = egui::pos2((area.right() - EDGE - width).max(area.left()), area.top().max(seen.top()) + EDGE);
    // The popup's own margin (`Frame::popup`) and its stroke.
    let margin = 6.0;
    // The fields take what the count and buttons leave, but never less than can be read: what then
    // doesn't fit wraps below them.
    let left = width - 2.0 * margin - 2.0 - TOGGLE_WIDTH - COUNT_WIDTH - 3.0 * crate::widgets::FLAT_BUTTON - 5.0 * GAP;
    let field = left.max(MIN_FIELD);
    let mut todo = None;
    // A widget of the bar was used, or a field changed.
    let (mut used, mut edited) = (false, false);
    let mut layer = None;
    crate::completion::popup(ui, box_id.with("find_area"), pos, area.height(), |ui| {
        ui.set_width(width - 2.0 * margin - 2.0);
        ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
        layer = Some(ui.layer_id());
        ui.horizontal_top(|ui| {
            // The arrow that shows or hides the Replace row, as tall as the rows it covers.
            let tall = if bar.replace_open { 2.0 * crate::widgets::FLAT_BUTTON + GAP } else { crate::widgets::FLAT_BUTTON };
            let (r, toggle) = ui.allocate_exact_size(egui::vec2(TOGGLE_WIDTH, tall), egui::Sense::click());
            let t = crate::theme::Tokens::get(ui.ctx());
            if toggle.hovered() {
                ui.painter().rect_filled(r, egui::CornerRadius::same(3), t.hover);
            }
            let icon = if bar.replace_open { "chevron_down" } else { "chevron_right" };
            let box_ =
                egui::Rect::from_center_size(r.center_top() + egui::vec2(0.0, crate::widgets::FLAT_BUTTON / 2.0), egui::vec2(14.0, 14.0));
            crate::icons::paint(ui.painter(), box_, icon, t.text, t.accent);
            toggle.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, bar.replace_open, tl!("Toggle Replace")));
            if toggle.on_hover_text(tl!("Toggle Replace")).clicked() {
                bar.replace_open = !bar.replace_open;
                used = true;
            }
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    let dropped = Field::cut_incoming(ui, query_id);
                    let query = ui.add(
                        egui::TextEdit::singleline(&mut bar.query.text)
                            .id(query_id)
                            .desired_width(field)
                            .margin(egui::Margin { left: 6, right: 54, top: 3, bottom: 3 })
                            .hint_text(tl!("Find")),
                    );
                    let toggled = field_toggles(ui, query_id.with("toggle"), query.rect, bar.case, bar.whole);
                    match toggled {
                        Some(Toggle::Case) => bar.case = !bar.case,
                        Some(Toggle::Word) => bar.whole = !bar.whole,
                        None => {}
                    }
                    bar.scroll |= toggled.is_some();
                    used |= toggled.is_some();
                    if std::mem::take(&mut bar.focus_query) {
                        ui.memory_mut(|m| m.request_focus(query_id));
                        let end = CCursor::new(bar.query.text.chars().count());
                        crate::widgets::set_cursor_range(ui.ctx(), query_id, CCursorRange::two(CCursor::new(0), end));
                    }
                    if query.changed() {
                        (bar.scroll, edited) = (true, true);
                        said.extend(bar.query.written(dropped).map(|n| left_out(Which::Find, n)));
                    }
                    let shift = ui.input(|i| i.modifiers.shift);
                    if query.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        todo = Some(if shift { Action::Previous } else { Action::Next });
                    }
                    let place = match (bar.query.text.is_empty(), bar.current()) {
                        (true, _) => String::new(),
                        (false, _) if total == 0 => tl!("No results").to_string(),
                        (false, current) => {
                            let (n, all) = (current.map_or(0, |c| c + 1).to_string(), format!("{total}{}", if capped { "+" } else { "" }));
                            crate::i18n::fmt("{n} of {all}", &[("n", &n), ("all", &all)])
                        }
                    };
                    ui.add_sized(
                        [COUNT_WIDTH, crate::widgets::FLAT_BUTTON],
                        egui::Label::new(egui::RichText::new(place).small().weak()).truncate(),
                    );
                    let (prev, next) = (tl!("Previous match (Shift+Enter)").to_string(), tl!("Next match (Enter)").to_string());
                    if crate::widgets::flat_icon_button(ui, "up", &prev, &prev, total > 0).clicked() {
                        todo = Some(Action::Previous);
                    }
                    if crate::widgets::flat_icon_button(ui, "down", &next, &next, total > 0).clicked() {
                        todo = Some(Action::Next);
                    }
                    let close = tl!("Close (Esc)").to_string();
                    if crate::widgets::flat_icon_button(ui, "close", &close, &close, true).clicked() {
                        todo = Some(Action::Close);
                    }
                });
                if bar.replace_open {
                    ui.horizontal_wrapped(|ui| {
                        let dropped = Field::cut_incoming(ui, replace_id);
                        let replace = ui.add(
                            egui::TextEdit::singleline(&mut bar.replace.text)
                                .id(replace_id)
                                .desired_width(field)
                                .margin(egui::Margin { left: 6, right: 6, top: 3, bottom: 3 })
                                .hint_text(tl!("Replace")),
                        );
                        // Under the count, the Replace buttons: one match, and all of them.
                        ui.add_space(COUNT_WIDTH + GAP);
                        if replace.changed() {
                            edited = true;
                            said.extend(bar.replace.written(dropped).map(|n| left_out(Which::Replace, n)));
                        }
                        let can = !running && bar.current().is_some();
                        let wait = tl!("Wait for the run to finish").to_string();
                        let (one_say, all_say) = (tl!("Replace (Enter)").to_string(), tl!("Replace All").to_string());
                        // Off, the hover says why: a run, or nothing to replace.
                        let nothing = tl!("Nothing to replace").to_string();
                        let one_hover = if running {
                            &wait
                        } else if !can {
                            &nothing
                        } else {
                            &one_say
                        };
                        let all_hover = if running {
                            wait.clone()
                        } else if capped {
                            crate::i18n::fmt("Too many matches to replace at once (more than {n})", &[("n", &MAX_MATCHES.to_string())])
                        } else if total == 0 {
                            nothing.clone()
                        } else {
                            tl!("Replace every match (one step of Undo)").to_string()
                        };
                        if crate::widgets::flat_icon_button(ui, "replace", &one_say, one_hover, can).clicked() {
                            todo = Some(Action::Replace);
                        } else if replace.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            // Made only when there is one to make.
                            used = true;
                            if can {
                                todo = Some(Action::Replace);
                            }
                        }
                        if crate::widgets::flat_icon_button(ui, "replace_all", &all_say, &all_hover, !running && total > 0 && !capped)
                            .clicked()
                        {
                            todo = Some(Action::ReplaceAll);
                        }
                    });
                }
                // Each note with its field: the Replace one only while its row shows.
                let replace = bar.replace_open.then_some(&bar.replace);
                let notes = [(Which::Find, Some(&bar.query)), (Which::Replace, replace)];
                for (which, field) in notes {
                    let Some(n) = field.map(|f| f.left_out).filter(|n| *n > 0) else { continue };
                    let cut = left_out(which, n);
                    ui.label(egui::RichText::new(cut).small().color(ui.visuals().warn_fg_color));
                }
            });
        });
    });
    // The widget of the bar with the keyboard, asked of the bar's layer. Before the focus goes back
    // to a field below: reading a widget's response after that would take it off again on a click.
    let focused = ui.memory(|m| m.focused());
    let mut focus = focused.filter(|f| ui.ctx().read_response(*f).is_some_and(|r| Some(r.layer_id) == layer));
    // As in a code editor, the keyboard stays in the bar after any of it was used: a click takes
    // the focus from a field, and so does Enter. Asked here, after the fields are drawn.
    if (used || todo.is_some()) && todo != Some(Action::Close) {
        let replacing = matches!(todo, Some(Action::Replace | Action::ReplaceAll));
        let from_replace = replacing || ui.memory(|m| m.had_focus_last_frame(replace_id));
        let field = if from_replace && bar.replace_open { replace_id } else { query_id };
        ui.memory_mut(|m| m.request_focus(field));
        focus = Some(field);
    }
    // What was typed in the fields this frame, before it is acted on.
    bar.refresh(script, cursor);
    match todo {
        Some(Action::Next) => bar.go(step(bar.found.ranges.len(), bar.current(), false)),
        Some(Action::Previous) => bar.go(step(bar.found.ranges.len(), bar.current(), true)),
        Some(Action::Replace) => {
            if let Some(r) = bar.current().and_then(|i| bar.found.ranges.get(i)).cloned().filter(|_| !running) {
                write(ui.ctx(), box_id, script, replace(script, std::slice::from_ref(&r), &bar.replace.text));
                let after = r.start + bar.replace.text.chars().count();
                crate::widgets::set_cursor_range(ui.ctx(), box_id, CCursorRange::one(CCursor::new(after)));
                bar.anchor = after;
                bar.scroll = true;
            }
        }
        Some(Action::ReplaceAll) => {
            if !running && !bar.found.capped && !bar.found.ranges.is_empty() {
                let new = replace(script, &bar.found.ranges, &bar.replace.text);
                // It may not grow the script past what a script file may be: a run adds up to the
                // matches times the replacement. One already larger may still shrink.
                if new.len() > crate::SCRIPT_FILE_MAX_BYTES && new.len() > script.len() {
                    said.push(tl!("Replace All would make the script larger than 1 MB: nothing was replaced").to_string());
                } else {
                    write(ui.ctx(), box_id, script, new);
                    crate::widgets::set_cursor_range(ui.ctx(), box_id, CCursorRange::one(CCursor::new(0)));
                    bar.anchor = 0;
                    bar.scroll = true;
                }
            }
        }
        Some(Action::Close) => dismiss(ui.ctx(), box_id, bar, true),
        _ => {}
    }
    if todo.is_some() || edited || used {
        // The box was painted before the bar: it shows what changed on the next frame.
        ui.ctx().request_repaint();
    }
    // Where the keyboard is now, for the next frame's keys.
    let pass = ui.ctx().cumulative_pass_nr();
    bar.focus = focus.filter(|_| bar.open).map(|f| (f, pass));
    said
}

/// The fill of a match, and of the current one: amber over the text, translucent, as the stop's
/// mark is, the current one stronger and outlined too, so it isn't told by its colour alone.
const MATCH: egui::Color32 = egui::Color32::from_rgba_premultiplied(90, 70, 0, 90);
const CURRENT: egui::Color32 = egui::Color32::from_rgba_premultiplied(160, 80, 0, 170);

/// Paints the matches over the rows of the laid-out box that show, and, once asked, brings the
/// current one into view in the enclosing scroll. `script`: the text the box has just laid out.
pub fn paint(ui: &egui::Ui, out: &egui::text_edit::TextEditOutput, script: &str, bar: &mut Bar) {
    use egui::text::{CCursor, CharIndex};
    if !bar.open {
        return;
    }
    bar.refresh(script, out.cursor_range.map_or(bar.anchor, |r| r.primary.index.0));
    let galley = &out.galley;
    let origin = crate::widgets::text_origin(out);
    let painter = ui.painter().with_clip_rect(out.text_clip_rect.intersect(ui.clip_rect()));
    let seen = painter.clip_rect();
    let current = bar.current();
    let ranges = &bar.found.ranges;
    let mut start = 0;
    for row in &galley.rows {
        let shown_chars = row.glyphs.len();
        let y = row.rect().translate(origin).y_range();
        if y.max >= seen.top() && y.min <= seen.bottom() {
            // The first match that doesn't end before this row, then each one that starts in it.
            let first = ranges.partition_point(|r| r.end <= start);
            for (i, r) in ranges.iter().enumerate().skip(first).take_while(|(_, r)| r.start < start + shown_chars) {
                let (a, b) = (r.start.max(start) - start, r.end.min(start + shown_chars) - start);
                let x = |c: usize| row.pos.x + row.x_offset(CharIndex(c)) + origin.x;
                let rect = egui::Rect::from_x_y_ranges(x(a)..=x(b), y);
                if current == Some(i) {
                    painter.rect_filled(rect, 2.0, CURRENT);
                    painter.rect_stroke(rect, 2.0, egui::Stroke::new(1.0, CURRENT), egui::StrokeKind::Outside);
                } else {
                    painter.rect_filled(rect, 2.0, MATCH);
                }
            }
        }
        start += row.char_count_including_newline().0;
    }
    if std::mem::take(&mut bar.scroll)
        && let Some(r) = bar.current().and_then(|i| ranges.get(i))
    {
        let at = egui::text::CCursor { prefer_next_row: true, ..CCursor::new(r.start) };
        ui.scroll_to_rect(galley.pos_from_cursor(at).translate(origin), Some(egui::Align::Center));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, query: &str, case: bool) -> Vec<(usize, usize)> {
        matches(text, query, case, false).ranges.into_iter().map(|r| (r.start, r.end)).collect()
    }

    #[test]
    fn the_matches_are_of_the_text_the_query_and_the_toggles_as_they_are() {
        let mut bar = Bar { open: true, ..Bar::default() };
        bar.query.text = "cat".into();
        let count = |bar: &mut Bar, script: &str| {
            bar.refresh(script, 0);
            bar.found.ranges.len()
        };
        assert_eq!(count(&mut bar, "cat Cat cats"), 3);
        assert_eq!(count(&mut bar, "cat"), 1, "the script changed");
        bar.query.text.push('s');
        assert_eq!(count(&mut bar, "cat Cat cats"), 1, "the query changed");
        bar.query.text.pop();
        bar.case = true;
        assert_eq!(count(&mut bar, "cat Cat cats"), 2, "Match Case");
        bar.whole = true;
        assert_eq!(count(&mut bar, "cat Cat cats"), 1, "Match Whole Word");
    }

    #[test]
    fn a_field_cuts_what_it_cannot_take_and_says_it_once() {
        let mut field = Field { text: "é".repeat(FIELD_MAX), ..Field::default() };
        assert_eq!((field.written(0), field.left_out), (None, 0), "what fits says nothing");
        field.text.push_str("ab");
        assert_eq!((field.written(0), field.text.chars().count()), (Some(2), FIELD_MAX));
        field.text.push('c');
        assert_eq!((field.written(0), field.left_out), (None, 3), "a key typed while over adds to the note, Messages was told");
        field.text.pop();
        assert_eq!((field.written(0), field.left_out), (None, 0), "a change that fits takes the note away");
        // The pop left room for one: two more are over again by one.
        field.text.push_str("cd");
        assert_eq!(field.written(0), Some(1), "cut again after fitting: said again");
        // Another long paste over a field already cut leaves out more than a keystroke does.
        field.text.push_str(&"x".repeat(30));
        assert_eq!(field.written(0), Some(30), "a paste's cut is said, whatever was cut before");
    }

    /// A paste bound for a focused field is cut before the field sees it, so none of the rest is
    /// ever laid out; one bound elsewhere is left whole.
    #[test]
    fn a_paste_is_cut_before_the_field_takes_it() {
        let (ctx, id) = (egui::Context::default(), egui::Id::unique("field"));
        for focused in [true, false] {
            let input = egui::RawInput { events: vec![egui::Event::Paste("é".repeat(1000))], ..Default::default() };
            let mut got = None;
            let mut frame = ctx.run_ui(input, |ui| {
                ui.memory_mut(|m| if focused { m.request_focus(id) } else { m.surrender_focus(id) });
                let dropped = Field::cut_incoming(ui, id);
                let kept =
                    ui.input(|i| i.events.iter().find_map(|e| if let egui::Event::Paste(t) = e { Some(t.chars().count()) } else { None }));
                got = Some((dropped, kept));
            });
            // Nothing is painted here: the font textures are let go.
            frame.textures_delta.clear();
            assert_eq!(got, Some(if focused { (800, Some(FIELD_MAX)) } else { (0, Some(1000)) }), "focused: {focused}");
        }
    }

    #[test]
    fn the_chords_are_one_list_for_the_box_and_the_browser() {
        use egui::{Key, Modifiers};
        let command = Modifiers { command: true, ctrl: true, ..Modifiers::default() };
        let altgr = Modifiers { alt: true, ..command };
        for mac in [false, true] {
            let does = |key: Key, m: Modifiers| chords(mac).iter().find(|c| c.key == key && c.is_pressed_with(m)).map(|c| c.does);
            assert_eq!(does(Key::F, command), Some(Action::Find));
            assert_eq!(does(Key::F, command | Modifiers::SHIFT), None, "Ctrl+Shift+F is not this box's");
            assert_eq!(does(Key::H, command | Modifiers::SHIFT), None);
            assert_eq!(does(Key::F, altgr), mac.then_some(Action::FindAndReplace), "AltGr types a letter");
            assert_eq!(does(Key::H, command), (!mac).then_some(Action::FindAndReplace), "⌘H hides the app");
            assert_eq!(does(Key::H, altgr), None);
            assert_eq!(does(Key::F3, Modifiers::NONE), Some(Action::Next));
            assert_eq!(does(Key::F3, Modifiers::SHIFT), Some(Action::Previous));
            assert_eq!(does(Key::F3, command), None);
            assert_eq!(does(Key::F, Modifiers::NONE), None, "typing an f");
        }
    }

    /// Every text of up to seven of `a`, `b` and a space against every query of up to four: the
    /// matches are those of trying each place in turn, the plainest way to say what they are.
    #[test]
    fn matches_are_those_of_trying_every_place() {
        let words = |most: usize| {
            let mut all = vec![String::new()];
            let mut last = all.clone();
            for _ in 0..most {
                last = last.iter().flat_map(|w| ['a', 'b', ' '].map(|c| format!("{w}{c}"))).collect();
                all.extend(last.clone());
            }
            all
        };
        let tried = |text: &str, query: &str, whole: bool| {
            let (t, q): (Vec<char>, Vec<char>) = (text.chars().collect(), query.chars().collect());
            let word = |i: usize| t.get(i).is_some_and(|c| c.is_alphanumeric());
            let (mut at, mut found) = (0, Vec::new());
            while !q.is_empty() && at + q.len() <= t.len() {
                let apart = !whole || (!at.checked_sub(1).is_some_and(word) && !word(at + q.len()));
                if apart && t.get(at..at + q.len()) == Some(&q[..]) {
                    found.push(at..at + q.len());
                    at += q.len();
                } else {
                    at += 1;
                }
            }
            found
        };
        let texts = words(7);
        for query in words(4) {
            for text in &texts {
                for whole in [false, true] {
                    assert_eq!(matches(text, &query, true, whole).ranges, tried(text, &query, whole), "{text:?} {query:?} {whole}");
                }
            }
        }
    }

    #[test]
    fn an_empty_query_is_nowhere() {
        assert_eq!(at("abc", "", true), []);
        assert_eq!(at("", "a", true), []);
    }

    #[test]
    fn matches_come_in_order_and_do_not_overlap() {
        assert_eq!(at("map.zoomFull\nmap.zoomIn", "map.", true), [(0, 4), (13, 17)]);
        assert_eq!(at("aaaa", "aa", true), [(0, 2), (2, 4)]);
    }

    #[test]
    fn case_counts_only_when_asked() {
        assert_eq!(at("Cities cities CITIES", "cities", false), [(0, 6), (7, 13), (14, 20)]);
        assert_eq!(at("Cities cities CITIES", "cities", true), [(7, 13)]);
    }

    #[test]
    fn a_whole_word_has_no_letter_digit_or_underscore_against_it() {
        let whole = |text: &str, query: &str| -> Vec<(usize, usize)> {
            matches(text, query, true, true).ranges.into_iter().map(|r| (r.start, r.end)).collect()
        };
        // Punctuation, spaces and the ends of the text are boundaries; letters, digits and `_` are not.
        assert_eq!(whole("city, (city) city_x xcity city2 city", "city"), [(0, 4), (7, 11), (32, 36)]);
        // A word that is not ASCII, and a query that is not a word at all.
        assert_eq!(whole("école écoles", "école"), [(0, 5)]);
        assert_eq!(whole("a.b a.bc", "a.b"), [(0, 3)]);
        // Without it, every one counts.
        assert_eq!(matches("city cityx", "city", true, false).ranges.len(), 2);
        // A refused place doesn't hide the next one inside it.
        assert_eq!(whole("aaa a", "a"), [(4, 5)]);
    }

    #[test]
    fn positions_are_characters_not_bytes() {
        // "é" is two bytes, one character; the match after it is at 3, not 4.
        assert_eq!(at("é\"ab\"", "ab", true), [(2, 4)]);
        assert_eq!(at("ÉCOLE école", "école", false), [(0, 5), (6, 11)]);
    }

    #[test]
    fn a_letter_whose_lower_case_is_longer_keeps_the_positions() {
        // "İ" lower-cases to two characters; folding must not shift what comes after it.
        assert_eq!(at("İx x", "x", false), [(1, 2), (3, 4)]);
        // And it is not an `i`: it matches itself only.
        assert_eq!(at("İ i", "i", false), [(2, 3)]);
        assert_eq!(at("İ i", "İ", false), [(0, 1)]);
    }

    #[test]
    fn only_the_first_matches_are_kept() {
        let text = "a".repeat(MAX_MATCHES + 5);
        let found = matches(&text, "a", true, false);
        assert_eq!(found.ranges.len(), MAX_MATCHES);
        assert!(found.capped);
        assert!(!matches("aaa", "a", true, false).capped);
    }

    #[test]
    fn stepping_goes_round() {
        assert_eq!(step(0, None, false), None);
        assert_eq!(step(3, None, false), Some(0));
        assert_eq!(step(3, None, true), Some(2));
        assert_eq!(step(3, Some(2), false), Some(0));
        assert_eq!(step(3, Some(0), true), Some(2));
        assert_eq!(step(3, Some(1), false), Some(2));
        assert_eq!(step(3, Some(1), true), Some(0));
        // A current one past the end (the text got shorter) is taken as the last.
        assert_eq!(step(2, Some(9), false), Some(0));
    }

    #[test]
    fn the_first_match_from_a_cursor() {
        let found = matches("ab ab ab", "ab", true, false);
        assert_eq!(first_from(&found, 0), Some(0));
        assert_eq!(first_from(&found, 1), Some(1));
        assert_eq!(first_from(&found, 6), Some(2));
        // Past the last: round to the first.
        assert_eq!(first_from(&found, 7), Some(0));
        assert_eq!(first_from(&matches("x", "y", true, false), 0), None);
    }

    #[test]
    fn a_replace_leaves_the_rest_alone() {
        assert_eq!(replace("a Cities b", std::slice::from_ref(&(2..8)), "Towns"), "a Towns b");
        assert_eq!(replace("é Cities", std::slice::from_ref(&(2..8)), "Ü"), "é Ü");
        // An empty range is an insertion.
        assert_eq!(replace("abc", std::slice::from_ref(&(1..1)), "X"), "aXbc");
        // A range past the end is cut to it, not a panic.
        assert_eq!(replace("abc", std::slice::from_ref(&(2..9)), "X"), "abX");
        assert_eq!(replace("abc", std::slice::from_ref(&(7..9)), "X"), "abcX");
    }

    #[test]
    fn replace_all_takes_every_match_and_not_what_it_adds() {
        let text = "Cities, Cities\nCities";
        let found = matches(text, "Cities", true, false);
        assert_eq!(replace(text, &found.ranges, "Cities2"), "Cities2, Cities2\nCities2");
        assert_eq!(replace(text, &found.ranges, ""), ", \n");
        assert_eq!(replace("é a é", &matches("é a é", "é", true, false).ranges, "ee"), "ee a ee");
        assert_eq!(replace("abc", &[], "X"), "abc");
    }
}
