//! Source guardrails for tests, without imposing timer restrictions on production code.
//!
//! This is a spelling check, not Rust name resolution: timer calls (including imported names)
//! need `// test-wait: <reason>` on the call's line. Signal waits with a deadline, such as
//! `recv_timeout` and `Condvar::wait_timeout`, are encouraged and are not timer waits.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const TIMERS: &[&str] = &["sleep", "sleep_until", "park_timeout", "park_timeout_ms", "interval", "interval_at"];
/// Timer types constructed with `::new(..)`.
const TIMER_TYPES: &[&str] = &["Delay", "TimeoutFuture"];

/// A token retains its byte offset so diagnostics and exceptions refer to the original line.
#[derive(Clone, Copy)]
struct Token<'a> {
    text: &'a str,
    at: usize,
}

/// The 1-based line of a byte offset, from the file's newline offsets.
fn line_of(newlines: &[usize], at: usize) -> usize {
    newlines.partition_point(|&n| n < at) + 1
}

/// Skip comments and keep literals whole: examples, URLs, braces and embedded scripts are data.
/// Also returns the newline offsets, so lines are looked up without rescanning the file.
fn tokens(source: &str) -> (Vec<Token<'_>>, BTreeSet<usize>, Vec<usize>) {
    let b = source.as_bytes();
    let newlines: Vec<usize> = source.match_indices('\n').map(|(n, _)| n).collect();
    let mut out = Vec::new();
    let mut reasons = BTreeSet::new();
    let mut at = 0;
    while at < b.len() {
        let start = at;
        if b.get(at..at + 2) == Some(b"//") {
            let end = source[at..].find('\n').map_or(b.len(), |n| at + n);
            if source[at + 2..end].trim().strip_prefix("test-wait:").is_some_and(|s| !s.trim().is_empty()) {
                reasons.insert(line_of(&newlines, at));
            }
            at = end;
            continue;
        }
        if b.get(at..at + 2) == Some(b"/*") {
            at += 2;
            let mut depth = 1;
            while at < b.len() && depth > 0 {
                match b.get(at..at + 2) {
                    Some(b"/*") => {
                        depth += 1;
                        at += 2;
                    }
                    Some(b"*/") => {
                        depth -= 1;
                        at += 2;
                    }
                    _ => at += 1,
                }
            }
            continue;
        }
        // Raw strings, including byte and C strings; hashes must match at the closing quote.
        let raw = if matches!(b.get(at), Some(b'b' | b'c')) { at + 1 } else { at };
        if b.get(raw) == Some(&b'r') {
            let mut quote = raw + 1;
            while b.get(quote) == Some(&b'#') {
                quote += 1;
            }
            if b.get(quote) == Some(&b'"') {
                let close = format!("\"{}", "#".repeat(quote - raw - 1));
                at = source[quote + 1..].find(&close).map_or(b.len(), |n| quote + 1 + n + close.len());
                out.push(Token { text: &source[start..at], at: start });
                continue;
            }
        }
        // A lifetime starts with a quote too; only a closed character is a literal.
        let character = b.get(at) == Some(&b'\'')
            && source[at + 1..].chars().next().is_some_and(|c| c == '\\' || b.get(at + 1 + c.len_utf8()) == Some(&b'\''));
        if b.get(at) == Some(&b'"') || character {
            let quote = b.get(at).copied();
            at += 1;
            while at < b.len() {
                let c = b.get(at).copied();
                at += 1;
                if c == Some(b'\\') {
                    at = (at + 1).min(b.len());
                } else if c == quote {
                    break;
                }
            }
        } else if b.get(at).is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_') {
            at += 1;
            while b.get(at).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                at += 1;
            }
        } else {
            at += source[at..].chars().next().map_or(1, char::len_utf8);
            if b.get(start).is_some_and(u8::is_ascii_whitespace) {
                continue;
            }
        }
        out.push(Token { text: &source[start..at], at: start });
    }
    (out, reasons, newlines)
}

/// Split attribute words at top-level commas.
fn top_level<'a>(words: &[&'a str]) -> Vec<Vec<&'a str>> {
    let mut parts = vec![Vec::new()];
    let mut depth = 0usize;
    for word in words {
        match *word {
            "(" | "[" => depth += 1,
            ")" | "]" => depth = depth.saturating_sub(1),
            "," if depth == 0 => {
                parts.push(Vec::new());
                continue;
            }
            _ => {}
        }
        if let Some(part) = parts.last_mut() {
            part.push(*word);
        }
    }
    parts.retain(|p| !p.is_empty());
    parts
}

/// Whether a `cfg` predicate holds only in test builds: `test`, an `all` with such a part, or
/// an `any` whose every part is one. `not(..)` and other options never imply it.
fn requires_test(predicate: &[&str]) -> bool {
    let inner = || predicate.get(2..predicate.len().saturating_sub(1)).unwrap_or_default();
    match predicate {
        ["test"] => true,
        ["all", "(", .., ")"] => top_level(inner()).iter().any(|p| requires_test(p)),
        ["any", "(", .., ")"] => {
            let parts = top_level(inner());
            !parts.is_empty() && parts.iter().all(|p| requires_test(p))
        }
        _ => false,
    }
}

/// Whether an attribute's words make the next item test-only (`cfg`) or a test: `test`, a
/// runtime's test attribute (`tokio::test(..)`, `wasm_bindgen_test`), or `test` through `cfg_attr`.
fn marks_test(words: &[&str]) -> bool {
    let args = || words.get(2..words.len().saturating_sub(1)).unwrap_or_default();
    let path: Vec<_> = words.iter().take_while(|w| **w != "(").collect();
    if path.last().is_some_and(|w| **w == "test" || w.ends_with("_test"))
        && path.iter().all(|w| w.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':'))
    {
        return true;
    }
    match words {
        ["cfg", "(", .., ")"] => requires_test(args()),
        ["cfg_attr", "(", .., ")"] => top_level(args()).iter().skip(1).any(|a| a.as_slice() == ["test"]),
        _ => false,
    }
}

/// Contents of a plain or raw string literal, for ignore reasons and module paths.
fn string_contents(text: &str) -> Option<&str> {
    if let Some(text) = text.strip_prefix('"') {
        return text.strip_suffix('"');
    }
    let raw = text.strip_prefix('r')?;
    let hashes = raw.len() - raw.trim_start_matches('#').len();
    raw.get(hashes..)?.strip_prefix('"')?.strip_suffix(&format!("\"{}", "#".repeat(hashes)))
}

/// Find test-only modules and test bodies using balanced delimiters, independent of formatting.
/// Follow external modules and `include!` as well, so helpers needn't have a special filename.
fn scan(path: &Path, source: &str, whole_test: bool) -> (Vec<String>, Vec<PathBuf>) {
    let (tokens, reasons, newlines) = tokens(source);
    let mut errors = Vec::new();
    let mut modules = Vec::new();
    let parent = path.parent().unwrap_or(Path::new(""));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    let dir = if matches!(stem, "lib" | "main" | "mod") { parent.to_path_buf() } else { parent.join(stem) };
    let mut scopes = vec![(whole_test, dir)];
    let mut pending_module = None;
    let mut pending_test = false;
    let mut module_path = None;
    let mut aliases: BTreeSet<&str> = TIMERS.iter().copied().collect();
    let mut types: BTreeSet<&str> = TIMER_TYPES.iter().copied().collect();
    for w in tokens.windows(3) {
        if let [name, as_, alias] = w
            && as_.text == "as"
        {
            if aliases.contains(name.text) {
                aliases.insert(alias.text);
            }
            if types.contains(name.text) {
                types.insert(alias.text);
            }
        }
    }
    // `(`/`[` depth outside attributes, so a `;` in a signature (`-> [u8; 4]`) doesn't end the item.
    let mut nesting = 0usize;
    let mut i = 0;
    while let Some(t) = tokens.get(i) {
        let next = |n: usize| tokens.get(i + n).map_or("", |t| t.text);
        let in_test = scopes.last().is_some_and(|s| s.0) || pending_test;
        let line = || line_of(&newlines, t.at);
        let inner = next(1) == "!";
        if t.text == "#" && next(if inner { 2 } else { 1 }) == "[" {
            let start = i + if inner { 3 } else { 2 };
            let mut end = start;
            let mut depth = 1;
            while let Some(t) = tokens.get(end) {
                if t.text == "[" {
                    depth += 1;
                }
                if t.text == "]" {
                    depth -= 1;
                }
                if depth == 0 {
                    break;
                }
                end += 1;
            }
            let attr = tokens.get(start..end).unwrap_or_default();
            let words: Vec<_> = attr.iter().map(|t| t.text).collect();
            // Also catches ignore nested in cfg_attr, with either spelling rejected unless
            // it supplies a nonempty string reason.
            for (j, word) in words.iter().enumerate() {
                if *word == "ignore" {
                    let reason = words.get(j + 2).copied().unwrap_or_default();
                    if words.get(j + 1) != Some(&"=") || string_contents(reason).is_none_or(|s| s.trim().is_empty()) {
                        errors.push(format!(
                            "{}:{}: use #[ignore = \"reason\"] (external requirement, or quarantine issue)",
                            path.display(),
                            line()
                        ));
                    }
                }
            }
            pending_test |= marks_test(&words);
            if words.first() == Some(&"path") {
                module_path = words.get(2).and_then(|s| string_contents(s)).map(str::to_string);
            }
            if inner && pending_test {
                if let Some(scope) = scopes.last_mut() {
                    scope.0 = true;
                }
                pending_test = false;
            }
            i = end + 1;
            continue;
        }
        let timer = (aliases.contains(t.text) && next(1) == "(")
            || (types.contains(t.text) && next(1) == ":" && next(2) == ":" && next(3) == "new" && next(4) == "(");
        if in_test && timer && !reasons.contains(&line()) {
            errors.push(format!("{}:{}: timer wait in test: wait on the job's completion or progress signal, not on time; if time is essential, add // test-wait: <reason> on this line", path.display(), line()));
        }
        if t.text == "mod" {
            pending_module = Some(next(1).to_string());
            if in_test && next(2) == ";" {
                if let Some(p) = &module_path {
                    let base = if scopes.len() > 1 { scopes.last().map_or(parent, |s| s.1.as_path()) } else { parent };
                    modules.push(base.join(p));
                } else if let Some((_, dir)) = scopes.last() {
                    let name = next(1);
                    modules.extend([dir.join(format!("{name}.rs")), dir.join(name).join("mod.rs")].into_iter().filter(|p| p.is_file()));
                }
            }
        }
        if in_test && t.text == "include" && next(1) == "!" && next(2) == "(" {
            let literal = next(3);
            if let Some(literal) = string_contents(literal) {
                modules.push(parent.join(literal));
            }
        }
        match t.text {
            "{" => {
                let dir = scopes.last().map(|s| s.1.clone()).unwrap_or_default();
                let dir = pending_module.take().map_or_else(|| dir.clone(), |name| dir.join(name));
                scopes.push((in_test, dir));
                pending_test = false;
                module_path = None;
            }
            "}" => {
                scopes.pop();
                pending_test = false;
            }
            "(" | "[" => nesting += 1,
            ")" | "]" => nesting = nesting.saturating_sub(1),
            ";" => {
                if nesting == 0 {
                    pending_test = false;
                }
                pending_module = None;
                module_path = None;
            }
            _ => {}
        }
        i += 1;
    }
    (errors, modules)
}

/// Scan repository Rust sources, including untracked new tests, without traversing build output.
pub fn run(root: &Path) -> Result<(), String> {
    // Match canonical module references, including Windows verbatim paths and roots with `..`.
    let canonical = root.canonicalize().map_err(|e| format!("{}: {e}", root.display()))?;
    let root = canonical.as_path();
    /// Read source files only; symlinks, hidden directories and build output are left out.
    fn walk(dir: &Path, files: &mut BTreeSet<PathBuf>) -> Result<(), String> {
        for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() && entry.file_name() != "target" && !entry.file_name().to_string_lossy().starts_with('.') {
                walk(&path, files)?;
            } else if kind.is_file() && path.extension().is_some_and(|s| s == "rs") {
                files.insert(path);
            }
        }
        Ok(())
    }
    let mut files = BTreeSet::new();
    walk(root, &mut files)?;
    let mut tests = BTreeSet::new();
    let mut errors = Vec::new();
    // Discover test-only external modules before collecting errors, to scan each source once
    // with its final context; every newly discovered module is followed transitively.
    for path in &files {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let whole = path.strip_prefix(root).unwrap_or(path).components().any(|c| c.as_os_str() == "tests");
        if whole {
            tests.insert(path.clone());
        }
        for module in scan(path, &text, whole).1 {
            tests.insert(module.canonicalize().map_err(|e| format!("{}: {e}", module.display()))?);
        }
    }
    let mut visited = BTreeSet::new();
    while let Some(path) = tests.difference(&visited).next().cloned() {
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        for module in scan(&path, &text, true).1 {
            tests.insert(module.canonicalize().map_err(|e| format!("{}: {e}", module.display()))?);
        }
        visited.insert(path);
    }
    // Included sources needn't end in `.rs`, so the discovered test sources are checked too.
    for path in files.union(&tests) {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let relative = path.strip_prefix(root).unwrap_or(path);
        errors.extend(scan(relative, &text, tests.contains(path)).0);
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    println!("test-hygiene: ok");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_waits_need_a_reason_on_the_call_line() {
        for call in [
            "std::thread::sleep(d)",
            "thread :: sleep (d)",
            "sleep(d)",
            "tokio::time::sleep_until(t)",
            "std::thread::park_timeout(d)",
            "interval(d)",
            "futures_timer::Delay::new(d)",
            "gloo_timers::future::TimeoutFuture::new(ms)",
        ] {
            let text = format!("#[test] fn check() {{ {call}; }}");
            let errors = scan(Path::new("src/lib.rs"), &text, false).0;
            assert_eq!(errors.len(), 1, "{text}: {errors:?}");
            assert!(errors[0].contains("completion or progress signal"));
            assert!(scan(Path::new("src/lib.rs"), &format!("{text} // test-wait: exercises timer expiry"), false).0.is_empty());
            assert_eq!(scan(Path::new("src/lib.rs"), &format!("// test-wait: wrong line\n{text}"), false).0.len(), 1);
            assert_eq!(scan(Path::new("src/lib.rs"), &format!("{text} // test-wait:  "), false).0.len(), 1);
        }
        let text = "use std::thread::{sleep as pause}; #[test] fn check() { pause(d); }";
        assert_eq!(scan(Path::new("src/lib.rs"), text, false).0.len(), 1);
        let text = "use futures_timer::Delay as Pause; #[test] fn check() { Pause::new(d); }";
        assert_eq!(scan(Path::new("src/lib.rs"), text, false).0.len(), 1);
        assert!(scan(Path::new("tests/helper.rs"), "rx.recv_timeout(d); cv.wait_timeout(lock, d);", true).0.is_empty());
    }

    #[test]
    fn scopes_keep_production_waits_out_and_test_helpers_in() {
        let text = r#"
            fn production() { std::thread::sleep(d); }
            #[cfg(test)] mod checks { fn helper() { sleep(d); } }
            fn more_production() { std::thread::sleep(d); }
            #[cfg(all(test, not(target_arch = "wasm32")))] fn helper() { sleep(d); }
            #[cfg(not(test))] fn native() { sleep(d); }
            #[cfg(any(test, unix))] fn unix_or_test() { sleep(d); }
            #[cfg(any(test, all(test, unix)))] fn only_test() { sleep(d); }
            #[cfg_attr(test, test)] fn conditional() { sleep(d); }
            #[cfg_attr(test, allow(dead_code))] fn allowed() { sleep(d); }
            #[tokio::test(flavor = "multi_thread")] async fn runtime() { sleep(d); }
            #[wasm_bindgen_test] fn browser() { sleep(d); }
            #[test] fn array() -> [u8; 2] { sleep(d); [0; 2] }
            #[inline] fn inlined() { sleep(d); }
        "#;
        assert_eq!(scan(Path::new("src/lib.rs"), text, false).0.len(), 7);
        assert_eq!(scan(Path::new("tests/helper.rs"), "fn helper() { sleep(d); }", true).0.len(), 1);
        assert_eq!(scan(Path::new("src/lib.rs"), "#![cfg(test)] fn helper() { sleep(d); }", false).0.len(), 1);
    }

    #[test]
    fn examples_and_comments_cannot_trigger_or_supply_exceptions() {
        let text = r###"
            #[test] fn check() {
                let _ = "sleep(d); #[ignore] // test-wait: not a comment";
                let _ = r##"sleep(d); #[ignore]"##;
                let _ = b"sleep(d)";
                let _ = '"'; let _ = '{'; let _ = '\'';
                /* nested /* sleep(d); */ #[ignore] */
                // sleep(d); #[ignore]
                sleep(d); let _ = "// test-wait: not a comment";
            }
        "###;
        let errors = scan(Path::new("src/lib.rs"), text, false).0;
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].starts_with("src/lib.rs:9:"));
    }

    #[test]
    fn ignores_need_nonempty_reasons_including_conditional_attributes() {
        for attr in ["#[ignore]", "# [ ignore ]", "#[ignore = \"\"]", "#[ignore = \"  \" ]", "#[cfg_attr(windows, ignore)]"] {
            assert_eq!(scan(Path::new("test.rs"), attr, false).0.len(), 1, "{attr}");
        }
        for attr in [
            "#[ignore = \"needs a live PostGIS\"]",
            r##"#[ignore = r#"needs network"#]"##,
            "#[cfg_attr(windows, ignore = \"quarantined: #123\")]",
        ] {
            assert!(scan(Path::new("test.rs"), attr, false).0.is_empty(), "{attr}");
        }
    }

    /// External helpers inherit their test context even when their names don't say "test".
    #[test]
    fn follows_external_modules_and_includes_transitively() {
        let dir = std::env::temp_dir().join(format!("rustgis-test-hygiene-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("checks/nested")).unwrap();
        std::fs::write(dir.join("lib.rs"), "#[cfg(test)] mod checks { mod helper; }").unwrap();
        std::fs::write(dir.join("checks/helper.rs"), "#[path = \"nested/worker.rs\"] mod worker;").unwrap();
        std::fs::write(dir.join("checks/nested/worker.rs"), "include!(\"body.rs\"); include!(\"cases.inc\");").unwrap();
        let body = dir.join("checks/nested/body.rs");
        std::fs::write(&body, "fn helper() { std::thread::sleep(d); }").unwrap();
        let cases = dir.join("checks/nested/cases.inc");
        std::fs::write(&cases, "fn case() { sleep(d); }").unwrap();
        let error = run(&dir).unwrap_err();
        assert!(error.contains("cases.inc:1: timer wait"), "{error}");
        std::fs::write(&cases, "fn case() {}").unwrap();
        let error = run(&dir.join("checks/..")).unwrap_err();
        assert!(error.contains("body.rs:1: timer wait"), "{error}");
        std::fs::write(&body, "fn helper() { std::thread::sleep(d); } // test-wait: simulated slow worker").unwrap();
        run(&dir).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    // CI runs the workspace tests even when it doesn't invoke `cargo xtask ci`.
    #[test]
    fn repository_tests_follow_the_guardrails() {
        if let Err(error) = run(&crate::root()) {
            panic!("{error}");
        }
    }
}
