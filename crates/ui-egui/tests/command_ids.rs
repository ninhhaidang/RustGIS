//! Hand-kept lists of command ids name real commands. Dozens of places outside the registry hold
//! command ids as string literals (the routing of Command Search, `ON_A_COPY`, the policy tables,
//! the background registries, the MCP read-ahead set, the ribbon): a command renamed or removed
//! leaves them naming nothing, and nothing fails to compile. This reads the non-test source of
//! every crate and app and fails on an id-shaped literal that is not a command.
//!
//! A literal counts as an id when it is dotted words (`layer.add`) in a namespace some registered
//! command has, or `ui`. It must then be a command the engine registers (`cmd::all()`), or, for
//! `ui.*`, one the app handles: a match arm `"ui.pane.catalog" => …` in `run_ui`, which is in
//! [`DISPATCH`]. The other dotted literals in
//! those namespaces (file names, host names) are told apart by their last word ([`NOT_IDS`]) and
//! [`EXEMPT`]. A literal in a namespace no registered command has is not looked at, so removing
//! the last command of a namespace leaves its literals unchecked. This lives in the UI crate
//! because only it can see both the engine's registry and the app's own table; the scan reads the
//! files, so it needs no private access.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

/// Last words of dotted literals that are file names or host names, not ids (`layer.json`,
/// `data.seattle.gov`). The test checks that no registered id ends in one, so a command added
/// with such an id fails it; a removed command whose id ended in one is not caught, as the check
/// sees only the ids still registered. A literal that ends in one of these needs no entry in
/// [`EXEMPT`].
const NOT_IDS: &[&str] = &[
    "json", "geojson", "csv", "tif", "tiff", "parquet", "shp", "gpkg", "kml", "gpx", "svg", "png", "jpg", "pdf", "qml", "html", "txt",
    "md", "xml", "zarr", "pmtiles", "wasm", "com", "gov", "org", "net", "us",
];

/// Dotted literals in an id's namespace that are not commands and don't end in a [`NOT_IDS`] word,
/// each with why. A literal listed here that the scan no longer finds fails the test, so the list
/// stays true.
const EXEMPT: &[(&str, &str)] = &[
    ("ui.layoutNudge", "an arrow key on a selected layout element, handled where the key is read (chrome.rs), not a command"),
    ("ui.undoHistory", "the toolbar's Undo History button: a menu, not a command"),
    ("ui.stac.planet", "a Web Data preset: `ui.stac.*` is resolved by `stacui::Preset::from_id`"),
    ("ui.stac.portolan", "a Web Data preset: `ui.stac.*` is resolved by `stacui::Preset::from_id`"),
    ("ui.stac.sentinel2", "a Web Data preset: `ui.stac.*` is resolved by `stacui::Preset::from_id`"),
    ("ui.stac.usgsLidar", "a Web Data preset: `ui.stac.*` is resolved by `stacui::Preset::from_id`"),
    ("ui.stac.vantor", "a Web Data preset: `ui.stac.*` is resolved by `stacui::Preset::from_id`"),
    ("data.cityofberkeley.info", "an open-data portal's host name; `info` is not in `NOT_IDS`, because `time.info` is a command"),
    ("stac.cut_note", "a key for a widget's remembered state in the STAC pane (stacui.rs), not a command"),
];

/// The file whose match arms handle the `ui.*` commands.
const DISPATCH: &str = "crates/ui-egui/src/lib.rs";

/// The byte range of `run_ui` in the dispatch file's `text`: from its signature to the first line
/// that is only an indented closing brace (rustfmt puts a method's at four spaces), with `\n` or
/// `\r\n` line ends.
fn run_ui_range(text: &str) -> std::ops::Range<usize> {
    let Some(start) = text.find("fn run_ui(") else { panic!("{DISPATCH} has no `fn run_ui(`") };
    let ends_line = |at: usize| matches!(text.as_bytes().get(at), Some(b'\n' | b'\r'));
    let end = text[start..].match_indices("\n    }").map(|(n, m)| start + n + m.len()).find(|&at| ends_line(at));
    let Some(end) = end else { panic!("the end of `run_ui` in {DISPATCH} not found") };
    start..end
}

/// The repository's root.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `.rs` files of every crate's and app's `src`, leaving out test files and directories by name
/// (`tests.rs`, `*_tests.rs`, `tests/`); a test-only module in a file with another name
/// (`#[cfg(test)] mod legacy;`) is scanned. A directory or entry that can't be read fails the test: a
/// file left out unseen would pass it.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type().unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        if kind.is_dir() {
            if name != "tests" && name != "benches" {
                sources(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
            out.push(path);
        }
    }
}

/// A piece of Rust source that matters here: a string literal's text (undecoded), a word, or one
/// punctuation character. Comments, char literals, lifetimes and whitespace make none, and only
/// the words and punctuation [`non_test`] and [`is_arm`] look at are kept.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok<'a> {
    Str(&'a str),
    Word(&'a str),
    Punct(char),
}

/// A source token and the byte offset it starts at.
type Lexed<'a> = (usize, Tok<'a>);

/// The words [`non_test`] and [`is_arm`] look at.
const KEPT_WORDS: &[&str] = &[
    "cfg",
    "test",
    "all",
    "any",
    "if",
    "macro_rules",
    "pub",
    "mod",
    "fn",
    "use",
    "impl",
    "const",
    "static",
    "struct",
    "enum",
    "trait",
    "type",
    "async",
    "unsafe",
    "extern",
];

/// The tokens of `text`. Reads strings the way Rust does (escapes, raw strings with or without `#`,
/// byte and C strings) and skips comments (nested block comments too), so a quote inside a
/// comment, a char (`'"'`) or a raw string doesn't start or end one. It runs over every source
/// file on every test run, so it jumps over comments and strings with `str::find` (optimised
/// even in a debug build) and keeps no token it won't look at.
fn lex(text: &str) -> Vec<Lexed<'_>> {
    let b = text.as_bytes();
    let word_byte = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80;
    let (mut i, mut out) = (0, Vec::new());
    while let Some(&c) = b.get(i) {
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            i += text[i..].find('\n').unwrap_or(b.len() - i);
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 1;
            i += 2;
            while depth > 0 {
                let rest = &text[i..];
                let Some(end) = rest.find("*/") else {
                    i = b.len();
                    break;
                };
                if let Some(open) = rest[..end].find("/*") {
                    depth += 1;
                    i += open + 2;
                } else {
                    depth -= 1;
                    i += end + 2;
                }
            }
        } else if c == b'\'' {
            // A char literal (`'a'`, `'\n'`, `'"'`) or a lifetime (`'a`, whose word is lexed next).
            if b.get(i + 1) == Some(&b'\\') {
                i += 3;
                while i < b.len() && b[i] != b'\'' {
                    i += 1;
                }
                i += 1;
            } else {
                let len = text.get(i + 1..).and_then(|t| t.chars().next()).map_or(1, char::len_utf8);
                i += if b.get(i + 1 + len) == Some(&b'\'') { 2 + len } else { 1 };
            }
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'"' || c >= 0x80 {
            // A string starts at its quote or, with a prefix (`b`, `c`, `r`, `br`, `cr`, hashes), at
            // its first letter; anything else that starts like a word is one.
            let quote = c == b'"';
            let mut j = i;
            if matches!(c, b'b' | b'c') {
                j += 1;
            }
            let raw = !quote && b.get(j) == Some(&b'r');
            let mut hashes = 0;
            if raw {
                j += 1;
                while b.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
            }
            if b.get(j) == Some(&b'"') && (quote || j > i) {
                let start = j + 1;
                let mut k = start;
                // The closing quote: not one a backslash escapes (an odd run of them before it), or
                // for a raw string the first one followed by its hashes.
                let end = loop {
                    let Some(at) = text[k..].find('"').map(|n| k + n) else { break b.len() };
                    let escaped = !raw && b[start..at].iter().rev().take_while(|&&c| c == b'\\').count() % 2 == 1;
                    if !escaped && (1..=hashes).all(|n| b.get(at + n) == Some(&b'#')) {
                        break at;
                    }
                    k = at + 1;
                };
                out.push((start, Tok::Str(text.get(start..end).unwrap_or(""))));
                i = end + 1 + hashes;
            } else {
                let start = i;
                while i < b.len() && word_byte(b[i]) {
                    i += 1;
                }
                let word = text.get(start..i).unwrap_or("");
                if KEPT_WORDS.contains(&word) {
                    out.push((start, Tok::Word(word)));
                }
            }
        } else if word_byte(c) {
            while i < b.len() && word_byte(b[i]) {
                i += 1;
            }
        } else {
            if matches!(c, b'#' | b'[' | b']' | b'(' | b')' | b'{' | b'}' | b';' | b',' | b'|' | b'=' | b'>') {
                out.push((i, Tok::Punct(char::from(c))));
            }
            i += 1;
        }
    }
    out
}

/// The line (from 1) of the byte offset `at` in `text`.
fn line_of(text: &str, at: usize) -> usize {
    1 + text.as_bytes().iter().take(at).filter(|&&c| c == b'\n').count()
}

/// The length in tokens of the test attribute at `tokens[i]`: `#[test]`, `#[cfg(test)]` or
/// `#[cfg(all(test, …))]`, and 0 for anything else (`#[cfg(not(test))]` and `#[cfg(any(test, …))]`
/// also compile outside tests).
fn test_attribute(t: &[Lexed<'_>], i: usize) -> usize {
    let word = |i: usize, w: &str| matches!(t.get(i), Some((_, Tok::Word(x))) if *x == w);
    let punct = |i: usize, p: char| matches!(t.get(i), Some((_, Tok::Punct(x))) if *x == p);
    if !(punct(i, '#') && punct(i + 1, '[')) {
        return 0;
    }
    if word(i + 2, "test") && punct(i + 3, ']') {
        return 4;
    }
    if !(word(i + 2, "cfg") && punct(i + 3, '(')) {
        return 0;
    }
    if word(i + 4, "test") && punct(i + 5, ')') && punct(i + 6, ']') {
        return 7;
    }
    if !(word(i + 4, "all") && punct(i + 5, '(')) {
        return 0;
    }
    // Inside `all(`: a `test` directly in it, up to the attribute's closing bracket.
    let (mut depth, mut found) = (2, false);
    for (k, (_, tok)) in t.iter().enumerate().skip(i + 6) {
        match tok {
            Tok::Punct('(') => depth += 1,
            Tok::Punct(')') => depth -= 1,
            Tok::Punct(']') if depth == 0 => return if found { k + 1 - i } else { 0 },
            Tok::Word("test") if depth == 2 => found = true,
            _ => {}
        }
    }
    0
}

/// `tokens` without the test code in them: items under `#[cfg(test)]` or `#[test]` (a module, fn,
/// use, impl or the like, up to its closing brace or semicolon). Code after a test module stays.
fn non_test(tokens: Vec<Lexed<'_>>) -> Vec<Lexed<'_>> {
    let word = |t: &[Lexed<'_>], i: usize, w: &str| matches!(t.get(i), Some((_, Tok::Word(x))) if *x == w);
    let punct = |t: &[Lexed<'_>], i: usize, p: char| matches!(t.get(i), Some((_, Tok::Punct(x))) if *x == p);
    let items =
        ["mod", "fn", "use", "impl", "const", "static", "struct", "enum", "trait", "type", "async", "unsafe", "extern", "macro_rules"];
    let (mut kept, mut i) = (Vec::new(), 0);
    while i < tokens.len() {
        let t = &tokens[..];
        let attr = test_attribute(t, i);
        // The item after the attributes, past any more of them and a visibility such as `pub(crate)`.
        let mut j = i + attr;
        while attr > 0 && punct(t, j, '#') && punct(t, j + 1, '[') {
            let mut depth = 0;
            while j < t.len() {
                if punct(t, j, '[') {
                    depth += 1;
                } else if punct(t, j, ']') {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                j += 1;
            }
            j += 1;
        }
        if attr > 0 && word(t, j, "pub") {
            j += 1;
            if punct(t, j, '(') {
                while j < t.len() && !punct(t, j, ')') {
                    j += 1;
                }
                j += 1;
            }
        }
        if attr > 0 && items.iter().any(|k| word(t, j, k)) {
            // To the item's closing brace, or its semicolon outside any bracket (`[&str; 2]`).
            let (mut depth, mut open) = (0usize, 0usize);
            while j < t.len() {
                match t[j].1 {
                    Tok::Punct('{') => depth += 1,
                    Tok::Punct('}') => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 && open == 0 {
                            break;
                        }
                    }
                    Tok::Punct('(' | '[') => open += 1,
                    Tok::Punct(')' | ']') => open = open.saturating_sub(1),
                    Tok::Punct(';') if depth == 0 && open == 0 => break,
                    _ => {}
                }
                j += 1;
            }
            i = j + 1;
        } else {
            kept.push(tokens[i]);
            i += 1;
        }
    }
    kept
}

/// Whether the string at `tokens[at - 1]` is (one of the alternatives of) a match arm: followed,
/// through any more `| "…"` alternatives, by `=>` or an `if` guard. A list in `matches!(…)` ends
/// in a bracket and isn't one.
fn is_arm(tokens: &[Lexed<'_>], mut at: usize) -> bool {
    loop {
        match (tokens.get(at), tokens.get(at + 1)) {
            (Some((_, Tok::Punct('='))), Some((_, Tok::Punct('>')))) => return true,
            (Some((_, Tok::Word(w))), _) if *w == "if" => return guard_ends_in_arrow(tokens, at + 1),
            (Some((_, Tok::Punct('|'))), Some((_, Tok::Str(_)))) => at += 2,
            _ => return false,
        }
    }
}

/// Whether the guard that starts at `from` is followed by `=>` before the delimiter around it
/// closes: a match arm's guard, not the guard of a `matches!(id, "x" if cond)`.
fn guard_ends_in_arrow(tokens: &[Lexed<'_>], from: usize) -> bool {
    let mut open = 0usize;
    for (n, (_, tok)) in tokens.iter().enumerate().skip(from) {
        match tok {
            Tok::Punct('(' | '[' | '{') => open += 1,
            Tok::Punct(')' | ']' | '}') => {
                if open == 0 {
                    return false;
                }
                open -= 1;
            }
            Tok::Punct(';') if open == 0 => return false,
            Tok::Punct('=') if open == 0 && matches!(tokens.get(n + 1), Some((_, Tok::Punct('>')))) => return true,
            _ => {}
        }
    }
    false
}

/// An id-shaped string literal: its byte offset, its text, whether it is a match arm, and how many
/// braces were open where it stands.
type Literal<'a> = (usize, &'a str, bool, usize);

/// The id-shaped string literals in the non-test code of `text`. Id-shaped is dotted words, the
/// first a lower-case word, none empty; one ending in a dot is a prefix (`"ui.pane."`), not an id.
fn literals(text: &str) -> Vec<Literal<'_>> {
    let tokens = non_test(lex(text));
    let (mut out, mut depth) = (Vec::new(), 0usize);
    for (n, (at, tok)) in tokens.iter().enumerate() {
        let s = match *tok {
            Tok::Punct('{') => {
                depth += 1;
                continue;
            }
            Tok::Punct('}') => {
                depth = depth.saturating_sub(1);
                continue;
            }
            Tok::Str(s) => s,
            _ => continue,
        };
        let words_ok = s.split('.').all(|w| !w.is_empty() && w.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'));
        if words_ok && s.contains('.') && s.bytes().next().is_some_and(|c| c.is_ascii_lowercase()) {
            out.push((*at, s, is_arm(&tokens, n + 1), depth));
        }
    }
    out
}

/// The `ui.*` ids that are arms of the dispatch `match` within `range` of the file the `literals`
/// are from: of the arms there, those at the least depth, since an inner `match` has more braces
/// open (`let cmd = match id { "ui.copy" => "edit.copy", … }` inside an arm handles nothing).
fn dispatch_arms<'a>(literals: &[Literal<'a>], range: std::ops::Range<usize>) -> HashSet<&'a str> {
    let arms: Vec<_> = literals.iter().filter(|(at, id, arm, _)| *arm && id.starts_with("ui.") && range.contains(at)).collect();
    let top = arms.iter().map(|(_, _, _, depth)| *depth).min();
    arms.iter().filter(|(_, _, _, depth)| Some(*depth) == top).map(|(_, id, _, _)| *id).collect()
}

/// The scan reads Rust as the compiler does: a literal in a comment or after a test module, in a
/// raw string, next to a `'"'` char or a brace in a string.
#[test]
fn the_scan_reads_rust_source_not_text() {
    let text = r####"
// "a.comment" is not code, nor is /* "a.block" */ this
fn code() {
    let (a, b) = ("a.first", r"a.raw");
    let c = ('"', "a.after_char"); /* "a.in_block" */
}
#[cfg(test)]
mod tests {
    fn t() { let s = "{ a.in_tests }"; let c = '{'; }
    const X: &str = "a.test_only";
}
const LATER: &str = r#"a.after_tests"#;
#[cfg(test)]
#[allow(dead_code)]
mod more {
    const Y: &str = "a.second_attribute";
}
#[cfg(test)]
const CASES: [&str; 2] = ["a.in_array", "a.in_array_too"];
const AFTER: &str = "a.after_array";
#[cfg(all(test, not(target_arch = "wasm32")))]
mod gated { const Z: &str = "a.in_all"; }
#[cfg(test)]
macro_rules! cases { () => { "a.in_macro" }; }
macro_rules! prod { () => { "a.prod_macro" }; }
#[cfg(test)]
const SIZED: [&str; { 1 }] = ["a.sized"];
#[cfg(not(test))]
const NOT_TEST: &str = "a.not_test";
#[cfg(any(test, unix))]
const ANY: &str = "a.any_test";
#[test]
fn alone() { let _ = "a.single_test"; }
fn arm(id: &str) {
    match id { "ui.one" | "ui.two" => {} "ui.three" if true => {} _ => {} }
    let _ = matches!(id, "ui.four" | "ui.five");
    let _ = matches!(id, "ui.six" if id.len() > 1);
}
fn run(id: &str) {
    match id {
        "ui.outer" => { let c = match id { "ui.inner" => "edit.x", "ui.active" => "ui.result", _ => "" }; }
        _ => {}
    }
}
"####;
    let found = literals(text);
    let ids: Vec<&str> = found.iter().map(|(_, id, _, _)| *id).collect();
    assert_eq!(
        ids,
        [
            "a.first",
            "a.raw",
            "a.after_char",
            "a.after_tests",
            "a.after_array",
            "a.prod_macro",
            "a.not_test",
            "a.any_test",
            "ui.one",
            "ui.two",
            "ui.three",
            "ui.four",
            "ui.five",
            "ui.six",
            "ui.outer",
            "ui.inner",
            "edit.x",
            "ui.active",
            "ui.result"
        ],
        "{found:?}"
    );
    let arms: Vec<&str> = found.iter().filter(|(_, _, arm, _)| *arm).map(|(_, id, _, _)| *id).collect();
    assert_eq!(arms, ["ui.one", "ui.two", "ui.three", "ui.outer", "ui.inner", "ui.active"]);
    // The dispatch is the outer match: not the inner one, and not a string that follows `=>`.
    let mut dispatch: Vec<&str> = dispatch_arms(&found, 0..text.len()).into_iter().collect();
    dispatch.sort_unstable();
    assert_eq!(dispatch, ["ui.one", "ui.outer", "ui.three", "ui.two"]);
    assert_eq!(line_of(text, found[3].0), 12, "the line of a literal after a test module");
    // `run_ui` ends at its closing brace whether the file has LF or CRLF line ends.
    let crlf = "impl A {\r\n    fn run_ui(&mut self) {\r\n        x\r\n    }\r\n}\r\n";
    assert_eq!(&crlf[run_ui_range(crlf)], "fn run_ui(&mut self) {\r\n        x\r\n    }");
}

#[test]
fn command_ids_in_hand_kept_lists_are_registered() {
    let commands = rustgis_engine::cmd::all();
    let ids: HashSet<&str> = commands.iter().map(|c| c.id).collect();
    let mut namespaces: BTreeSet<&str> = ids.iter().filter_map(|id| id.split('.').next()).collect();
    namespaces.insert("ui");
    assert!(ids.len() > 500, "the registry has {} commands", ids.len());
    let ends_like_a_file = ids.iter().filter(|id| id.rsplit('.').next().is_some_and(|w| NOT_IDS.contains(&w))).collect::<Vec<_>>();
    assert!(ends_like_a_file.is_empty(), "registered ids end in a word NOT_IDS treats as a file or host name: {ends_like_a_file:?}");

    let root = root();
    let mut files = Vec::new();
    for group in ["crates", "apps"] {
        let packages = std::fs::read_dir(root.join(group)).unwrap_or_else(|e| panic!("{group}: {e}"));
        for package in packages {
            let src = package.unwrap_or_else(|e| panic!("{group}: {e}")).path().join("src");
            // A package without a `src` directory has no source; one that can't be read is an error.
            match std::fs::metadata(&src) {
                Ok(meta) if meta.is_dir() => sources(&src, &mut files),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => panic!("{}: {e}", src.display()),
            }
        }
    }
    files.sort();
    assert!(files.len() > 200, "the scan found {} source files", files.len());
    // Each file is read once and lexed once, whatever lists of ids it holds.
    let texts: Vec<(String, String)> = files
        .iter()
        .map(|f| {
            let text = std::fs::read_to_string(f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
            (f.strip_prefix(&root).unwrap_or(f).display().to_string().replace('\\', "/"), text)
        })
        .collect();
    let found: Vec<(&str, &str, Vec<Literal<'_>>)> =
        texts.iter().map(|(name, text)| (name.as_str(), text.as_str(), literals(text))).collect();

    // The `ui.*` commands the app handles: those with a match arm in the dispatch in `run_ui` (an arm
    // elsewhere, such as one that picks an icon or an inner `match`, handles nothing).
    let ui_arms: HashSet<&str> =
        found.iter().filter(|(file, _, _)| *file == DISPATCH).flat_map(|(_, text, l)| dispatch_arms(l, run_ui_range(text))).collect();
    assert!(ui_arms.len() > 100, "the scan found {} ui commands with an arm", ui_arms.len());

    let (mut seen, mut exempt_seen, mut dangling) = (0, HashSet::new(), Vec::new());
    for (file, text, literals) in &found {
        for (at, word, _, _) in literals {
            let namespace = word.split('.').next().unwrap_or("");
            let last = word.rsplit('.').next().unwrap_or("");
            if !namespaces.contains(namespace) || NOT_IDS.contains(&last) {
                continue;
            }
            seen += 1;
            if let Some((id, _)) = EXEMPT.iter().find(|(id, _)| id == word) {
                exempt_seen.insert(*id);
            } else if !(ids.contains(word) || ui_arms.contains(word)) {
                dangling.push(format!("{file}:{}: \"{word}\"", line_of(text, *at)));
            }
        }
    }
    assert!(seen > 2000, "the scan found {seen} command ids");
    assert!(
        dangling.is_empty(),
        "{} command ids in the source are not commands (a command renamed or removed? or add to EXEMPT with a reason):\n{}",
        dangling.len(),
        dangling.join("\n")
    );
    let stale: Vec<_> = EXEMPT.iter().filter(|(id, _)| !exempt_seen.contains(id)).map(|(id, _)| *id).collect();
    assert!(stale.is_empty(), "EXEMPT lists literals the source no longer has: {stale:?}");
}
