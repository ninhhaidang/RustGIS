//! Colours for the Script pane (#653): comments, command ids and their JSON parameters, read by
//! the rules `script.run` reads a script by (`script_commands`, `refused_in_script`), so a line that won't run shows
//! before Run. The text itself is never changed: the colours are spans over it, byte for byte, so
//! the box's cursor, selection and undo work as in a plain one.

use std::ops::Range;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Stroke};
use rustgis_engine::cmd::workflows::{MAX_SCRIPT_LINES, refused_in_script};
use rustgis_engine::string_end;

/// What a piece of a script is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Comment,
    /// A command id that runs in a script.
    Id,
    /// A command id no command has, or one that can't run in a script.
    BadId,
    Key,
    Str,
    Num,
    /// `true`, `false`, `null`.
    Literal,
    Punct,
    /// A line past `MAX_SCRIPT_LINES`, which Run doesn't read.
    Unread,
}

/// A piece of a script: its bytes, what it is, and whether it is in parameters that don't read as
/// JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub range: Range<usize>,
    pub kind: Kind,
    pub broken: bool,
}

/// The spans of `script`, in order and covering it all. `unknown` holds the ids no command has.
pub fn spans(script: &str, unknown: &[String]) -> Vec<Span> {
    let mut out = Vec::new();
    let mut at = 0;
    for (n, line) in script.split_inclusive('\n').enumerate() {
        let end = at + line.trim_end_matches(['\n', '\r']).len();
        if n >= MAX_SCRIPT_LINES {
            // Run reads no further: the rest is one dimmed span.
            out.push(Span { range: at..script.len(), kind: Kind::Unread, broken: false });
            return out;
        }
        line_spans(script, at..end, unknown, &mut out);
        if end < at + line.len() {
            out.push(Span { range: end..at + line.len(), kind: Kind::Plain, broken: false });
        }
        at += line.len();
    }
    out
}

/// The command id of a script line, as `script_commands` reads it: its first word (to the first
/// space), less a trailing `=`. So `labels.style= {…}` names `labels.style`, while
/// `labels.style={…}` names `labels.style={…` as Run does, which no command has. None for a blank
/// or comment line.
pub fn line_id(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let id = line.split_once(char::is_whitespace).map_or(line, |(id, _)| id);
    Some(id.trim_end_matches('='))
}

fn line_spans(script: &str, range: Range<usize>, unknown: &[String], out: &mut Vec<Span>) {
    let Some(line) = script.get(range.clone()) else { return };
    let start = range.start;
    let plain = |r: Range<usize>, out: &mut Vec<Span>| {
        if !r.is_empty() {
            out.push(Span { range: r, kind: Kind::Plain, broken: false });
        }
    };
    let body = line.trim_start();
    let lead = line.len() - body.len();
    if body.is_empty() {
        plain(range, out);
        return;
    }
    if body.starts_with('#') {
        plain(start..start + lead, out);
        out.push(Span { range: start + lead..range.end, kind: Kind::Comment, broken: false });
        return;
    }
    plain(start..start + lead, out);
    let word = body.split_once(char::is_whitespace).map_or(body, |(w, _)| w);
    let id = word.trim_end_matches('=');
    // Read as `script_commands` reads it: the JSON once, and the engine's own rule for the ids a
    // script refuses.
    let rest = &body[word.len()..];
    let json_reads = rest.trim().is_empty() || serde_json::from_str::<serde_json::Value>(rest.trim()).is_ok();
    // `unknown` is sorted (`unknown_ids`), so a binary search: a script may name many.
    let bad_id = refused_in_script(id) || unknown.binary_search_by(|u| u.as_str().cmp(id)).is_ok();
    let id_at = start + lead;
    if !id.is_empty() {
        out.push(Span { range: id_at..id_at + id.len(), kind: if bad_id { Kind::BadId } else { Kind::Id }, broken: false });
    }
    if word.len() > id.len() {
        out.push(Span { range: id_at + id.len()..id_at + word.len(), kind: Kind::Punct, broken: false });
    }
    json_spans(rest, id_at + word.len(), !json_reads, out);
}

/// The spans of a line's parameters, `text` starting at byte `at` of the script. A tokenizer that
/// reads anything, JSON or not: an unclosed string runs to the end.
fn json_spans(text: &str, at: usize, broken: bool, out: &mut Vec<Span>) {
    let b = text.as_bytes();
    let mut i = 0;
    // Whether the next token is a value: after a colon, until a comma or a bracket.
    let mut value = false;
    // Where the last chain of `|` alternatives read ends, and whether a key's colon ends it: the
    // chain's other strings take the same answer, so a long chain is read once.
    let mut chain = (0, false);
    let push = |r: Range<usize>, kind: Kind, out: &mut Vec<Span>| {
        if !r.is_empty() {
            out.push(Span { range: at + r.start..at + r.end, kind, broken });
        }
    };
    while let Some(&c) = b.get(i) {
        let s = i;
        match c {
            b'"' => {
                // Past its closing quote, or to the end when nothing closes it.
                i = text.get(i + 1..).and_then(string_end).map_or(text.len(), |n| i + n + 2);
                // A key is a string with a colon after it.
                let after = text.get(i..).map(str::trim_start).unwrap_or_default();
                // (A command's parameter description marks a key that may be left out with `?`:
                // `"size"?: number`, `"layer"?,`, and alternative keys with `|`: `"lon" | "x":
                // number`. A script's JSON never has either there. Values have alternatives too,
                // `"path": "*.gif" | "*.zip"`, so `|` makes a key only where no value is due.)
                let mut key = after.starts_with(':') || after.starts_with('?');
                if !key && !value {
                    if s >= chain.0 {
                        let (end, keys) = alternatives(after);
                        // `end` is the tail of `text` past the chain: where it starts is the chain's end.
                        chain = (text.len() - end.len(), keys);
                    }
                    key = chain.1;
                }
                push(s..i, if key { Kind::Key } else { Kind::Str }, out);
            }
            b'-' | b'0'..=b'9' => {
                i += 1;
                while b.get(i).is_some_and(|&c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-')) {
                    i += 1;
                }
                push(s..i, Kind::Num, out);
            }
            b'{' | b'}' | b'[' | b']' | b':' | b',' => {
                i += 1;
                value = c == b':';
                push(s..i, Kind::Punct, out);
            }
            c if c.is_ascii_alphabetic() => {
                while b.get(i).is_some_and(u8::is_ascii_alphanumeric) {
                    i += 1;
                }
                let word = text.get(s..i).unwrap_or_default();
                push(s..i, if matches!(word, "true" | "false" | "null") { Kind::Literal } else { Kind::Plain }, out);
            }
            _ => {
                // Whitespace and anything else, to the next byte a token starts with (on a char
                // boundary, as those bytes are ASCII).
                i += 1;
                while b.get(i).is_some_and(|&c| {
                    !matches!(c, b'"' | b'-' | b'0'..=b'9' | b'{' | b'}' | b'[' | b']' | b':' | b',') && !c.is_ascii_alphabetic()
                }) {
                    i += 1;
                }
                push(s..i, Kind::Plain, out);
            }
        }
    }
}

/// The spans of a help's signature keys (` {layer?, size?}`), `text` starting at byte `at`: each
/// name a key, as the same key is in the script; brackets and commas punctuation; spaces and the
/// `?` that marks a key optional plain. That `?` ends a name (`signature` writes `name?`), so a
/// `?` inside one (a plugin's `{"a?b": text}`) stays part of the key.
fn signature_spans(text: &str, at: usize, out: &mut Vec<Span>) {
    let kind = |c: char, next: Option<char>| match c {
        '{' | '}' | '[' | ']' | ',' => Kind::Punct,
        '?' if matches!(next, Some(',' | '}') | None) => Kind::Plain,
        c if c.is_whitespace() => Kind::Plain,
        _ => Kind::Key,
    };
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let (range, k) = (at + i..at + i + c.len_utf8(), kind(c, chars.peek().map(|&(_, n)| n)));
        match out.last_mut() {
            // A run of one kind is one span.
            Some(last) if last.kind == k && last.range.end == range.start => last.range.end = range.end,
            _ => out.push(Span { range, kind: k, broken: false }),
        }
    }
}

/// Reads the `| "…"` alternatives at the start of `after` (the text after a string). Returns the
/// text past them, and whether a key's colon (or `?`) follows them: true for ` | "x": number`
/// after `"lon"`, false for ` | "*.zip" (PNG frames)`.
fn alternatives(mut after: &str) -> (&str, bool) {
    while let Some(rest) = after.strip_prefix('|').map(str::trim_start) {
        // Past the next string's closing quote; a string nothing closes ends them.
        let Some(next) = rest.strip_prefix('"').and_then(|r| r.get(string_end(r)? + 1..)) else { return (after, false) };
        after = next.trim_start();
    }
    (after, after.starts_with(':') || after.starts_with('?'))
}

/// The colours of a script, one per kind. The theme's text, dimmed text and accent, and colours
/// of the script's own for the rest: the theme's green and amber are made for icons and marks and
/// read poorly as small text on the light theme's white, and comments are a colour of their own,
/// not the dimmed one that reads as disabled. Keys take a teal of their own, apart from the
/// strings they are written as. Every one is at least 4.5:1 on the box (WCAG AA).
#[derive(Clone, Copy, Hash)]
pub struct Palette {
    pub text: Color32,
    /// Punctuation.
    pub dim: Color32,
    pub comment: Color32,
    pub id: Color32,
    pub bad: Color32,
    pub key: Color32,
    pub string: Color32,
    pub number: Color32,
}

impl Palette {
    pub fn of(t: &crate::theme::Tokens) -> Self {
        let rgb = Color32::from_rgb;
        let (comment, bad, key, string, number) = if t.dark {
            (rgb(0x7F, 0xBF, 0x8F), rgb(0xF0, 0x6A, 0x6A), rgb(0x5E, 0xD3, 0xE6), rgb(0xE0, 0xA0, 0x60), rgb(0xC6, 0xA0, 0xF6))
        } else {
            (rgb(0x2E, 0x7D, 0x4F), rgb(0xC6, 0x28, 0x28), rgb(0x0F, 0x76, 0x6E), rgb(0x8A, 0x4B, 0x08), rgb(0x7B, 0x3F, 0xB5))
        };
        Palette { text: t.text, dim: t.text_dim, comment, id: t.accent_text, bad, key, string, number }
    }
}

/// `script` laid out with its colours.
pub fn job(script: &str, unknown: &[String], palette: Palette, font: &FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    for s in spans(script, unknown) {
        append(&mut job, script, &s, palette, font);
    }
    job
}

/// A command's signature or one of its parameters, for the Script suggestions' help (#662),
/// coloured as the script colours a command and its JSON, so the help reads as what's typed.
/// `id` leads, in the id's colour, when given, and `text` is then the signature's keys
/// (` {layer?, size?}`); otherwise a parameter's description.
pub fn help_job(id: Option<&str>, text: &str, palette: Palette, font: &FontId) -> LayoutJob {
    let full = format!("{}{text}", id.unwrap_or_default());
    let at = full.len() - text.len();
    let mut spans = Vec::new();
    if at > 0 {
        spans.push(Span { range: 0..at, kind: Kind::Id, broken: false });
    }
    if id.is_some() {
        signature_spans(text, at, &mut spans);
    } else {
        json_spans(text, at, false, &mut spans);
    }
    let mut job = LayoutJob::default();
    for s in &spans {
        append(&mut job, &full, s, palette, font);
    }
    job
}

/// `s` of `text` appended to `job` in its colour.
fn append(job: &mut LayoutJob, text: &str, s: &Span, palette: Palette, font: &FontId) {
    let color = match s.kind {
        Kind::Plain => palette.text,
        Kind::Key => palette.key,
        Kind::Comment => palette.comment,
        Kind::Punct | Kind::Unread => palette.dim,
        Kind::Id => palette.id,
        Kind::BadId => palette.bad,
        Kind::Str => palette.string,
        Kind::Num | Kind::Literal => palette.number,
    };
    // What won't run is underlined too, not told by its colour alone (WCAG 1.4.1).
    let underline = if s.broken || s.kind == Kind::BadId { Stroke::new(1.0, palette.bad) } else { Stroke::NONE };
    let format = TextFormat { font_id: font.clone(), color, underline, italics: s.kind == Kind::Comment, ..Default::default() };
    job.append(text.get(s.range.clone()).unwrap_or_default(), 0.0, format);
}

/// Works out a script's colours once per change of the script (or theme, or of the ids no
/// command has), not every frame (egui's frame cache).
#[derive(Default)]
pub struct Highlighter;

impl egui::cache::ComputerMut<(&str, &[String], Palette, &FontId), LayoutJob> for Highlighter {
    fn compute(&mut self, (script, unknown, palette, font): (&str, &[String], Palette, &FontId)) -> LayoutJob {
        job(script, unknown, palette, font)
    }
}

pub type Cache = egui::cache::FrameCache<LayoutJob, Highlighter>;

/// `unknown_ids`, worked out again only when the script or the loaded plugins change (a plugin
/// turned on or off shows at once); otherwise one hash of the script per frame. The built-in
/// commands don't change while the app runs.
pub fn unknown_ids_cached(ui: &egui::Ui, session: &rustgis_engine::Session, script: &str) -> std::sync::Arc<Vec<String>> {
    type Kept = (u64, u64, std::sync::Arc<Vec<String>>);
    let key = egui::Id::unique("script_unknown_ids");
    let (text, plugins) = (egui::util::hash(script), rustgis_engine::plugins::revision());
    if let Some((t, p, ids)) = ui.data(|d| d.get_temp::<Kept>(key))
        && (t, p) == (text, plugins)
    {
        return ids;
    }
    let ids = std::sync::Arc::new(unknown_ids(session, script));
    ui.data_mut(|d| d.insert_temp::<Kept>(key, (text, plugins, ids.clone())));
    ids
}

/// The ids `script`'s lines name (as far as Run reads) that no command has, built-in or a
/// plugin's, each once and sorted.
pub fn unknown_ids(session: &rustgis_engine::Session, script: &str) -> Vec<String> {
    let known = |id: &str| {
        session.registry.get(id).is_some()
            || (id.starts_with(rustgis_engine::plugins::PREFIX) && rustgis_engine::plugins::command(id).is_some())
    };
    let lines = script.lines().take(MAX_SCRIPT_LINES);
    let mut out: Vec<String> = lines.filter_map(line_id).filter(|id| !known(id)).map(String::from).collect();
    out.sort();
    out.dedup();
    out
}

/// The text box's layout of `script`: its colours (worked out once per change), wrapped at `wrap`.
///
/// Each frame costs a hash of the script (the cache key) and a clone of the kept `LayoutJob`, by
/// design: `layout_job` takes the job by value, and egui keeps the laid-out galleys itself. Keeping
/// our own `Galley` instead would skip that clone but outlive egui's font atlas: after the atlas is
/// rebuilt (a language with a CJK font, a change of UI zoom) it would draw stale glyphs, where
/// egui's own galley cache is cleared. egui_extras' highlighter works the same way.
pub fn layout(ui: &egui::Ui, script: &str, unknown: &[String], palette: Palette, wrap: f32) -> std::sync::Arc<egui::Galley> {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let mut job = ui.memory_mut(|m| m.caches.cache::<Cache>().get((script, unknown, palette, &font)).clone());
    job.wrap.max_width = wrap;
    ui.fonts_mut(|f| f.layout_job(job))
}

#[cfg(test)]
mod tests {
    use super::{Kind, line_id, spans};

    /// The pieces of `s` with their kinds, broken JSON marked with `!`.
    fn kinds(s: &str, unknown: &[&str]) -> Vec<(String, Kind, bool)> {
        let unknown: Vec<String> = unknown.iter().map(|u| u.to_string()).collect();
        let sp = spans(s, &unknown);
        // The spans cover the text, in order, with nothing left out or repeated.
        let mut at = 0;
        for x in &sp {
            assert_eq!(x.range.start, at, "{s:?}: {sp:?}");
            at = x.range.end;
        }
        assert_eq!(at, s.len(), "{s:?}");
        sp.into_iter().filter(|x| !s[x.range.clone()].trim().is_empty()).map(|x| (s[x.range].to_string(), x.kind, x.broken)).collect()
    }

    #[test]
    fn a_command_and_its_json_are_told_apart() {
        assert_eq!(
            kinds(r##"labels.style {"size": 12.5, "bold": true, "color": "#5A1E0A", "x": null}"##, &[]),
            [
                ("labels.style", Kind::Id, false),
                ("{", Kind::Punct, false),
                (r#""size""#, Kind::Key, false),
                (":", Kind::Punct, false),
                ("12.5", Kind::Num, false),
                (",", Kind::Punct, false),
                (r#""bold""#, Kind::Key, false),
                (":", Kind::Punct, false),
                ("true", Kind::Literal, false),
                (",", Kind::Punct, false),
                (r#""color""#, Kind::Key, false),
                (":", Kind::Punct, false),
                (r##""#5A1E0A""##, Kind::Str, false),
                (",", Kind::Punct, false),
                (r#""x""#, Kind::Key, false),
                (":", Kind::Punct, false),
                ("null", Kind::Literal, false),
                ("}", Kind::Punct, false),
            ]
            .map(|(a, k, b)| (a.to_string(), k, b))
        );
    }

    #[test]
    fn comments_blank_lines_and_bare_commands() {
        let s = "# zoom\n\n   # indented\r\nmap.zoomFull\n";
        assert_eq!(
            kinds(s, &[]),
            [("# zoom", Kind::Comment, false), ("# indented", Kind::Comment, false), ("map.zoomFull", Kind::Id, false)].map(|(a, k, b)| (
                a.to_string(),
                k,
                b
            ))
        );
    }

    #[test]
    fn lines_that_wont_run_are_marked() {
        // No such command; one that can't run in a script; JSON that doesn't read.
        assert_eq!(kinds("nope.cmd {}", &["nope.cmd"])[0], ("nope.cmd".to_string(), Kind::BadId, false));
        for id in ["ui.pane.labeling", "script.run", "notebook.run"] {
            assert_eq!(kinds(&format!("{id} {{}}"), &[])[0].1, Kind::BadId, "{id}");
        }
        let k = kinds(r#"labels.style {"size": }"#, &[]);
        assert_eq!(k[0].1, Kind::Id, "the id is fine");
        assert!(k[1..].iter().all(|x| x.2), "the parameters are marked: {k:?}");
        // An unclosed string runs to the end, and the next line is read on its own.
        let k = kinds("labels.style {\"a\": \"x\nmap.zoomFull", &[]);
        assert!(k.iter().any(|x| x.0 == "\"x" && x.1 == Kind::Str && x.2));
        assert_eq!(k.last().unwrap(), &("map.zoomFull".to_string(), Kind::Id, false));
    }

    #[test]
    fn strings_escapes_nesting_and_unicode() {
        let k = kinds(r#"select.byAttributes {"where": "name = \"Bogotá\" 🌎", "o": {"k": [1, -2e3]}}"#, &[]);
        assert!(k.contains(&(r#""name = \"Bogotá\" 🌎""#.to_string(), Kind::Str, false)), "{k:?}");
        assert!(k.contains(&(r#""k""#.to_string(), Kind::Key, false)), "a nested key: {k:?}");
        assert!(k.contains(&("-2e3".to_string(), Kind::Num, false)));
    }

    /// The id is the first word as Run reads it (`script_commands`): a trailing `=` is dropped, but
    /// `id={json}` with no space is one word, which no command has.
    #[test]
    fn an_id_is_the_first_word_as_run_reads_it() {
        let k = kinds(r#"labels.style= {"size": 12}"#, &[]);
        assert_eq!(k[..2], [("labels.style".to_string(), Kind::Id, false), ("=".to_string(), Kind::Punct, false)]);
        let glued = r#"labels.style={"size": 12}"#;
        assert_eq!(line_id(glued), Some(r#"labels.style={"size":"#));
        assert_eq!(kinds(glued, &[r#"labels.style={"size":"#])[0].1, Kind::BadId);
        assert_eq!((line_id("  # x"), line_id(" "), line_id("map.zoomFull")), (None, None, Some("map.zoomFull")));
    }

    #[test]
    fn junk_never_panics() {
        for s in [
            "\"",
            "{",
            "x \u{0}\u{7f}\"\\",
            "a \"\\",
            "é",
            "\r\n\r",
            "id -",
            "id 1e",
            "id \"\\u12",
            "id \"\\é\"",
            "id \"\\🌎",
            "= {}",
            "=== x",
        ] {
            kinds(s, &[]);
        }
        let long = "map.zoomFull {}\n".repeat(10_000);
        assert_eq!(spans(&long, &[]).len(), 10_000 * 5, "id, space, two braces, line end");
    }

    /// The ids no command has: built-in ones and plugins' are known, each unknown one listed once.
    #[test]
    fn unknown_ids_are_the_ones_no_command_has() {
        let app = crate::RustGisApp::with_sample();
        let script = "nope.cmd {}\nmap.zoomFull\n# x.y\nnope.cmd\nplugin:none/cmd {}\nlabels.style= {}\nlabels.style={}";
        // `labels.style={}` is one word, as Run reads it: no command has that id.
        assert_eq!(super::unknown_ids(&app.session, script), ["labels.style={}", "nope.cmd", "plugin:none/cmd"]);
    }

    /// The Script pane draws its coloured box, and the text in it is the script as typed.
    #[test]
    fn the_script_pane_keeps_the_text_it_colours() {
        use egui_kittest::kittest::Queryable;
        let mut app = crate::RustGisApp::with_sample();
        let script = "# zoom\nnope.cmd {\"a\": \"é\"}\nlabels.style {\"size\": }\n";
        app.workflow.script = script.into();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(500.0, 400.0)).build_ui_state(
            |ui, app: &mut crate::RustGisApp| crate::analysispane::script_pane(app, ui, &crate::theme::Tokens::light()),
            app,
        );
        h.run();
        assert_eq!(h.get_by_role(egui::accesskit::Role::MultilineTextInput).value().as_deref(), Some(script));
        h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
        h.run();
        h.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text("x");
        h.run();
        assert_eq!(h.state().workflow.script.chars().filter(|c| *c == 'x').count(), 1, "typing still edits it");
    }

    /// Every colour of the script reads on its box, in both themes: at least 4.5:1 (WCAG AA for
    /// small text).
    #[test]
    fn every_colour_reads_on_the_box() {
        fn luminance(c: egui::Color32) -> f64 {
            let f = |v: u8| {
                let v = f64::from(v) / 255.0;
                if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        }
        let contrast = |a, b| {
            let (x, y) = (luminance(a), luminance(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        };
        for t in [crate::theme::Tokens::light(), crate::theme::Tokens::dark()] {
            let p = super::Palette::of(&t);
            for (name, c) in [
                ("text", p.text),
                ("dim", p.dim),
                ("comment", p.comment),
                ("id", p.id),
                ("bad", p.bad),
                ("key", p.key),
                ("string", p.string),
                ("number", p.number),
            ] {
                let r = contrast(c, t.input);
                assert!(r >= 4.5, "{name} on the {} theme: {r:.2}", if t.dark { "dark" } else { "light" });
            }
        }
    }

    /// A long script scrolls in its box rather than pushing Run out of the pane.
    #[test]
    fn a_long_script_keeps_run_in_view() {
        use egui_kittest::kittest::Queryable;
        let mut app = crate::RustGisApp::with_sample();
        app.workflow.script = "map.zoomFull\n".repeat(300);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(500.0, 700.0)).build_ui_state(
            |ui, app: &mut crate::RustGisApp| crate::analysispane::script_pane(app, ui, &crate::theme::Tokens::light()),
            app,
        );
        h.run();
        let run = h.get_by_label("Run").rect();
        assert!(run.max.y < 700.0, "Run is in view: {run:?}");
    }

    /// A short script's box stays small: it grows with the script, and only a long one meets the
    /// limit (see `a_long_script_keeps_run_in_view`).
    #[test]
    fn a_short_script_keeps_a_small_box() {
        use egui_kittest::kittest::Queryable;
        let mut app = crate::RustGisApp::with_sample();
        app.workflow.script = "map.zoomFull".into();
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(500.0, 700.0)).build_ui_state(
            |ui, app: &mut crate::RustGisApp| crate::analysispane::script_pane(app, ui, &crate::theme::Tokens::light()),
            app,
        );
        h.run();
        let text = h.get_by_role(egui::accesskit::Role::MultilineTextInput).rect();
        assert!(text.height() < 300.0, "about ten lines, not the whole pane: {text:?}");
    }

    /// What won't run is underlined, not told by its colour alone: a bad id and broken JSON.
    #[test]
    fn what_wont_run_is_underlined() {
        let palette = super::Palette::of(&crate::theme::Tokens::light());
        let font = egui::FontId::monospace(12.0);
        let script = "nope.cmd {}\nmap.zoomFull {\"a\": }";
        let job = super::job(script, &["nope.cmd".to_string()], palette, &font);
        let underlined = |text: &str| {
            job.sections
                .iter()
                .filter(|s| &script[s.byte_range.start.0..s.byte_range.end.0] == text)
                .all(|s| s.format.underline.width > 0.0)
        };
        assert!(underlined("nope.cmd") && underlined("\"a\""), "the bad id and the broken JSON");
        assert!(!underlined("map.zoomFull"), "a good id isn't");
    }

    /// A run's results, however long, stay in view under the buttons, in a scroll of their own. The
    /// line above them, the run and when it started, too.
    #[test]
    fn long_results_stay_in_view() {
        use egui_kittest::kittest::Queryable;
        let mut app = crate::RustGisApp::with_sample();
        app.workflow.script = "map.zoomFull\n".repeat(300);
        app.workflow.script_out = (0..12).map(|i| (format!("line {i}: {}", "a long error ".repeat(30)), true)).collect();
        app.workflow.script_last = Some(crate::analysispane::ScriptRun { n: 1, secs: 0.5, at: 1_800_000_000, ..Default::default() });
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(500.0, 700.0)).build_ui_state(
            |ui, app: &mut crate::RustGisApp| crate::analysispane::script_pane(app, ui, &crate::theme::Tokens::light()),
            app,
        );
        h.run();
        assert!(h.get_by_label("Run").rect().max.y < 700.0);
        let first = h.get_by_label_contains("line 0:").rect();
        assert!(first.min.y < 700.0, "the first result shows: {first:?}");
        let run = h.get_by_label_contains("Run #1 (").rect();
        assert!(run.max.y < first.min.y, "the run above them");
        // Scrolled down, the results move under the run's line, which stays where it was: it's
        // not in their scroll.
        h.event(egui::Event::PointerMoved(first.center()));
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -100_000.0),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        });
        // egui eases a wheel's scroll over frames, asking for a repaint until it's done.
        h.run_steps(60);
        assert!(h.get_by_label_contains("line 0:").rect().min.y < first.min.y, "the results scrolled");
        assert_eq!(h.get_by_label_contains("Run #1 (").rect(), run, "the run's line didn't move");
    }

    /// The kept unknown ids follow the script: a change is read again, the same script isn't.
    #[test]
    fn kept_unknown_ids_follow_the_script() {
        let app = crate::RustGisApp::with_sample();
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(Default::default(), |ui| {
            let first = super::unknown_ids_cached(ui, &app.session, "nope.a {}");
            assert_eq!(*first, ["nope.a"]);
            let again = super::unknown_ids_cached(ui, &app.session, "nope.a {}");
            assert!(std::sync::Arc::ptr_eq(&first, &again), "the same script is kept, not read again");
            assert_eq!(*super::unknown_ids_cached(ui, &app.session, "nope.b {}\nmap.zoomFull"), ["nope.b"]);
        });
        // No painter here: the frame's textures are let go, as egui asks.
        out.textures_delta.clear();
    }

    /// A pane too short for the box's first lines and the buttons scrolls as a whole, the box
    /// kept at its first lines; a tall one fits the box to it (`a_long_script_keeps_run_in_view`).
    #[test]
    fn a_short_pane_scrolls_as_a_whole() {
        use egui_kittest::kittest::Queryable;
        let mut app = crate::RustGisApp::with_sample();
        app.workflow.script = "map.zoomFull\n".repeat(300);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(500.0, 150.0)).build_ui_state(
            |ui, app: &mut crate::RustGisApp| crate::analysispane::script_pane(app, ui, &crate::theme::Tokens::light()),
            app,
        );
        h.run();
        // The text's own rect is its whole content (it scrolls inside the box): Run, just under the
        // box, says how tall the box is.
        let (text, run) = (h.get_by_role(egui::accesskit::Role::MultilineTextInput).rect(), h.get_by_label("Run").rect());
        assert!(run.min.y - text.min.y < 260.0, "the box keeps about its first lines: {text:?}, Run at {run:?}");
    }

    /// Past the lines Run reads, nothing is coloured or looked up: those lines won't run.
    #[test]
    fn lines_run_doesnt_read_are_dimmed() {
        let max = rustgis_engine::cmd::workflows::MAX_SCRIPT_LINES;
        let script = format!("{}nope.cmd {{}}\n", "map.zoomFull\n".repeat(max));
        let sp = spans(&script, &[]);
        let last = sp.last().unwrap();
        assert_eq!((last.kind, &script[last.range.clone()]), (Kind::Unread, "nope.cmd {}\n"));
        let app = crate::RustGisApp::with_sample();
        assert!(super::unknown_ids(&app.session, &script).is_empty(), "an unread line's id isn't looked up");
    }

    /// The box keeps its focus and text when the pane is resized from fitting the box to scrolling
    /// as a whole: one id either way.
    #[test]
    fn resizing_the_pane_keeps_the_box_focused() {
        use egui_kittest::kittest::Queryable;
        let mut app = crate::RustGisApp::with_sample();
        app.workflow.script = "map.zoomFull\n".repeat(30);
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(500.0, 700.0)).build_ui_state(
            |ui, app: &mut crate::RustGisApp| crate::analysispane::script_pane(app, ui, &crate::theme::Tokens::light()),
            app,
        );
        h.run();
        h.get_by_role(egui::accesskit::Role::MultilineTextInput).focus();
        h.run();
        h.set_size(egui::vec2(500.0, 150.0));
        h.run();
        assert!(h.get_by_role(egui::accesskit::Role::MultilineTextInput).is_focused(), "still focused once the pane scrolls");
        h.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text("z");
        h.run();
        assert!(h.state().workflow.script.contains('z'));
    }

    /// The help's signature and parameters take the script's colours: the id, keys (`?` marks
    /// one that may be left out) and string values.
    #[test]
    fn help_reads_in_the_scripts_colours() {
        let palette = super::Palette::of(&crate::theme::Tokens::light());
        let font = egui::FontId::monospace(12.0);
        // The colour where `piece` starts (egui joins neighbouring pieces of one format).
        let colour = |job: &egui::text::LayoutJob, piece: &str| {
            let at = job.text.find(piece)?;
            job.sections.iter().find(|s| (s.byte_range.start.0..s.byte_range.end.0).contains(&at)).map(|s| s.format.color)
        };
        let sig = super::help_job(Some("map.body"), " {name?, basemap?}", palette, &font);
        assert_eq!(colour(&sig, "map.body"), Some(palette.id));
        assert_eq!(colour(&sig, "name"), Some(palette.key), "the signature's keys are keys");
        assert_eq!(colour(&sig, "basemap"), Some(palette.key));
        assert_eq!(colour(&sig, "?"), Some(palette.text));
        assert_eq!(colour(&sig, "{"), Some(palette.dim));
        assert_eq!(colour(&sig, ","), Some(palette.dim));
        // A `?` inside a name (a plugin's key) is part of the key; the one after it marks it optional.
        let odd = super::help_job(Some("plugin:x/y"), " {a?b, c?}", palette, &font);
        assert_eq!(colour(&odd, "?b"), Some(palette.key));
        assert_eq!(colour(&odd, "?}"), Some(palette.text));
        let p = super::help_job(None, r#""name"?: "earth"|"moon" (map.bodies lists them)"#, palette, &font);
        assert_eq!(colour(&p, r#""name""#), Some(palette.key), "a key, `?` and all");
        assert_eq!(colour(&p, r#""moon""#), Some(palette.string));
        assert_eq!(colour(&p, "lists"), Some(palette.text), "a description's words stay text");
    }

    /// Keys have a colour of their own, apart from strings and plain text, in both themes.
    #[test]
    fn keys_have_their_own_colour() {
        for t in [crate::theme::Tokens::light(), crate::theme::Tokens::dark()] {
            let p = super::Palette::of(&t);
            assert!(p.key != p.text && p.key != p.string && p.key != p.comment && p.key != p.id);
        }
        let font = egui::FontId::monospace(12.0);
        let palette = super::Palette::of(&crate::theme::Tokens::light());
        let script = r#"labels.style {"size": 12, "halo": {"width": 1}}"#;
        let job = super::job(script, &[], palette, &font);
        let colour = |piece: &str| {
            let at = script.find(piece)?;
            job.sections.iter().find(|s| (s.byte_range.start.0..s.byte_range.end.0).contains(&at)).map(|s| s.format.color)
        };
        assert_eq!(colour(r#""size""#), Some(palette.key));
        assert_eq!(colour(r#""width""#), Some(palette.key), "a nested key too");
    }

    /// A description's alternatives: keys (`"lon" | "x": number`) are keys, each of them, and
    /// values (`"path": "*.gif" | "*.zip"`) stay strings, even when a key follows them.
    #[test]
    fn alternative_keys_are_keys_and_alternative_values_strings() {
        // The quoted pieces of `desc` and whether each reads as a key.
        let keys = |desc: &str| {
            let mut sp = Vec::new();
            super::json_spans(desc, 0, false, &mut sp);
            sp.into_iter()
                .filter(|s| matches!(s.kind, Kind::Key | Kind::Str))
                .map(|s| (desc[s.range].to_string(), s.kind == Kind::Key))
                .collect::<Vec<_>>()
        };
        let q = |pieces: &[(&str, bool)]| pieces.iter().map(|&(p, k)| (format!("\"{p}\""), k)).collect::<Vec<_>>();
        // map.goToXY
        assert_eq!(
            keys(r#"{"lon" | "x": number, "lat" | "y": number, "scale"?: number}"#),
            q(&[("lon", true), ("x", true), ("lat", true), ("y", true), ("scale", true)])
        );
        // map.save, time.export: the values' alternatives.
        assert_eq!(
            keys(r#"{"path"?: "*.model.json" | "*.rgistool", "text"?: file JSON}"#),
            q(&[("path", true), ("*.model.json", false), ("*.rgistool", false), ("text", true)])
        );
        assert_eq!(
            keys(r#"{"path": "*.gif" | "*.zip" (PNG frames), "width"?}"#),
            q(&[("path", true), ("*.gif", false), ("*.zip", false), ("width", true)])
        );
        // raster.zarrInfo, map.import: a value, then a key as the other choice.
        assert_eq!(
            keys(r#"{"path": "a Zarr store's folder" | "url": "https://…"}"#),
            q(&[("path", true), ("a Zarr store's folder", false), ("url", true), ("https://…", false)])
        );
        assert_eq!(
            keys(r#"{"path": "*.rgismap|*.rgis" | "text": map file JSON | "data": base64 file}"#),
            q(&[("path", true), ("*.rgismap|*.rgis", false), ("text", true), ("data", true)])
        );
        // Three choices, and a chain that ends at a comma before one that ends at a key.
        assert_eq!(
            keys(r#"{"a" | "b" | "c": number, "d" | "e", "f" | "g": number}"#),
            q(&[("a", true), ("b", true), ("c", true), ("d", false), ("e", false), ("f", true), ("g", true)])
        );
        // Unclosed: no key out of a string nothing closes.
        assert_eq!(keys(r#""a" | "b"#), [(r#""a""#.to_string(), false), (r#""b"#.to_string(), false)]);
    }
}
