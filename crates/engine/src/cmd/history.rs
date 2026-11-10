//! A geoprocessing run from the History pane as code (#539): a call for the `rustgis` Python
//! package, a line for the Script pane, or a `rustgis-cli` command; and the whole history as a
//! Python script.
//!
//! The parameters are as the tool ran with them, without what would give someone else the user's
//! access (`sources::redact`). Layers go by the names they had then, so a script opens the
//! project they belong to.

use serde_json::{Map, Value, json};

use super::{ps, uint};
use crate::{CmdResult, CommandSpec as C, GpRecord, Session};

pub fn commands() -> Vec<C> {
    vec![
        C::new("gp.historyCode", "Copy Run As Code", "Analyze › Geoprocessing", code)
            .roles(&[])
            .effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"run": index in the history (0 = the first run), "as"?: "python" (default)|"command" (a Script pane line)|"cli"}"#),
        C::new("gp.historyScript", "Save History As Python Script", "Analyze › Geoprocessing", script)
            .roles(&[])
            .effects(crate::policy::Effects::NONE)
            .view(),
    ]
}

/// Formats a replayable code fragment inside the guarded history projection.
fn code(s: &mut Session, p: &Value) -> CmdResult {
    let i = uint(p, "run").and_then(|i| usize::try_from(i).ok()).ok_or("run is required: a run's place in the history, from 0")?;
    let project = project_file(s);
    let r = s.project.gp_history.get_mut(i).ok_or("no run there in the history")?;
    let (text, report) = crate::durable::output(r, None, |r| {
        let (cmd, params) = replay_written(r)?;
        match ps(p, "as").unwrap_or("python") {
            "python" => Ok(python(&cmd, &params)),
            "command" => Ok(format!("{cmd} {params}")),
            "cli" => Ok(format!("rustgis-cli run --project {} --cmd {}", shell(&project), shell(&format!("{cmd}={params}")))),
            other => Err(format!("unknown kind '{other}': python, command or cli")),
        }
    })?;
    crate::durable::notify(s, &report);
    Ok(json!({"code": text}))
}

/// The history as a Python script that opens the project and runs each tool again, in order. A
/// run that failed, or can't be run again, is there as a comment.
fn script(s: &mut Session, _: &Value) -> CmdResult {
    if s.project.gp_history.is_empty() {
        return Err("the history is empty: run a tool first".into());
    }
    let name = s.project.name.clone();
    let project = project_file(s);
    let unsaved = s.path.is_none();
    let dirty = s.dirty();
    let (result, report) = crate::durable::output(&mut s.project.gp_history, None, |runs| {
        let mut text = format!(
            "# The geoprocessing history of the RustGIS project {}, run again in order.\n\
         # Layers are named as they were then, so this opens the project they belong to.\n",
            quoted(&name)
        );
        // The script opens the saved file: a layer added since, which a run may have used, isn't
        // there until the project is saved again.
        if unsaved {
            text.push_str("# The project hasn't been saved yet: save it as this file first.\n");
        } else if dirty {
            text.push_str("# The project has changes that aren't saved: save it first, so its layers are there.\n");
        }
        text.push_str(&format!("import rustgis\n\ns = rustgis.Session({})\n", quoted(&project)));
        for (n, r) in runs.iter().enumerate() {
            let when = if r.at > 0 {
                let (date, time) = rustgis_map::graphics::civil(r.at);
                format!(" ({date} {time} UTC)")
            } else {
                String::new()
            };
            text.push_str(&format!("\n# {}. {}{when}", n + 1, line(&r.tool)));
            match replay_written(r) {
                Ok((cmd, params)) if r.ok => text.push_str(&format!("\n{}\n", python(&cmd, &params))),
                Ok((cmd, params)) => {
                    // An error may quote a connection, password and all: redacted as a parameter's
                    // value is, not for its addresses only.
                    let why = r.messages.first().map(|m| line(&redacted_text(m))).unwrap_or_default();
                    text.push_str(&format!(": failed when it ran ({why})\n"));
                    for l in python(&cmd, &params).lines() {
                        text.push_str(&format!("# {l}\n"));
                    }
                }
                Err(e) => text.push_str(&format!(": {}\n", line(&e))),
            }
        }
        let file = format!("{}_history.py", file_stem(&name));
        Ok(json!({"fileName": file, "text": text}))
    })?;
    crate::durable::notify(s, &report);
    Ok(result)
}

/// The commands other than `gp.run` a run is copied as. The history comes from the project file,
/// which may say anything: only these are written into code, so a crafted file can't pass any
/// other command off as a geoprocessing run (and an id is never text to escape).
const REPLAYED: &[&str] = &["gp.bufferPoint", "gp.batch"];

/// The command that runs `r` again, with its parameters: `gp.run` with the tool, or the command
/// that ran it (a point buffered where the map was clicked, `gp.bufferPoint`).
pub fn replay(r: &GpRecord) -> Result<(String, Value), String> {
    let (id, params) = replay_written(r)?;
    let (clean, _) = crate::roles::command(&id, &params, 0);
    Ok((id, clean.unwrap_or(params)))
}

/// Formats a record already inside the durable output transaction; data stays replayable.
fn replay_written(r: &GpRecord) -> Result<(String, Value), String> {
    if let Some((cmd, params)) = &r.command {
        let Some(cmd) = REPLAYED.iter().find(|c| **c == cmd.as_str()) else {
            return Err(format!("{} can't be run again: the history names a command that isn't a geoprocessing one", r.tool));
        };
        return Ok((cmd.to_string(), params.clone()));
    }
    let id = tool_of(r).ok_or_else(|| format!("{} can't be run again: the history doesn't say which tool it was", r.tool))?;
    Ok(("gp.run".into(), json!({"tool": id, "params": r.params})))
}

/// The id of the tool `r` ran with `gp.run`: kept with the run, or, in projects from before it
/// was kept, a built-in tool found by its name. `None` for a run another command made.
pub fn tool_of(r: &GpRecord) -> Option<String> {
    if r.command.is_some() {
        return None;
    }
    if r.id.is_empty() { super::gp::tool_id(&r.tool).map(str::to_string) } else { Some(r.id.clone()) }
}

/// `v` without what would give someone else the user's access: an address's keys, tokens and
/// passwords, a connection's password, and a value under a name that says it is a secret (a
/// `password`, an `api_token`; a plain `key` when it reads as one or sits beside a web address,
/// as `sources::holds_secret` judges it), left empty. An object's keys too: the file may put
/// anything there.
pub(crate) fn redacted(v: &Value) -> Value {
    use crate::sources::looks_like_a_key;
    use rustgis_data::source::credentials::secret_key;
    match v {
        Value::String(t) => Value::String(redacted_parameter(t)),
        Value::Array(a) => Value::Array(a.iter().map(redacted).collect()),
        Value::Object(o) => {
            let beside_address = |k: &str| o.iter().any(|(sibling, v)| sibling != k && crate::project_log::address_context(sibling, v, 0));
            let secret = |k: &str, v: &Value| {
                secret_key(k)
                    && !matches!(v, Value::Bool(_) | Value::Null)
                    && (!k.eq_ignore_ascii_case("key") || v.as_str().is_some_and(looks_like_a_key) || beside_address(k))
            };
            Value::Object(
                o.iter()
                    .map(|(k, v)| (redacted_parameter(k), if secret(k, v) { Value::String(String::new()) } else { redacted(v) }))
                    .collect(),
            )
        }
        other => other.clone(),
    }
}

/// Replay parameters are data, not diagnostic payloads: preserve innocent markup and public
/// query parameters, while keeping the same conservative credential boundaries.
pub(crate) fn redacted_parameter(t: &str) -> String {
    let structured = t.trim_start().starts_with(['{', '[', '<']);
    let text = if connection(t) || (!structured && !t.contains(char::is_whitespace)) {
        crate::sources::redact(t)
    } else if structured || t.contains("://") {
        rustgis_data::diagnostics::redact_parameter(t)
    } else {
        t.to_string()
    };
    query_pairs_emptied(&private_keys_removed(&text))
}

/// `t` without its secrets. A connection is taken whole, though a libpq one (`PG:host=db
/// password=…`) has spaces in it. A single-word address keeps its nonsecret request parameters
/// for replay. In prose, an unknown URL's secret value may continue past whitespace: the strict
/// diagnostic scanner must see it before any address splitter removes its identifying key.
pub(crate) fn redacted_text(t: &str) -> String {
    let r = if connection(t) || !t.contains(char::is_whitespace) {
        crate::sources::redact(t)
    } else if t.contains("://") {
        rustgis_data::diagnostics::redact(t)
    } else {
        t.to_string()
    };
    // After `redact` too: a prefix it knows may come in a form it doesn't (GDAL's `MYSQL:`).
    query_pairs_emptied(&private_keys_removed(&r))
}

/// `t` with its secret pairs emptied (`secret_pairs_emptied`), parted first at each `&` that
/// starts another `name=` pair, as a query string's do (`a=1&password=…`, with no `://` for
/// `redact` to read it by). An `&` inside a value (`password=ab&cd`), or in a value quoted or
/// braced from its start, stays in it, so the whole value goes. A quote inside a word
/// (`O'Reilly`, `can't`) opens nothing; one that opens a value and never closes is taken for a
/// stray one, and the text is parted at every such `&`, quotes aside (the safe way).
fn query_pairs_emptied(t: &str) -> String {
    pairs_parted(t, true).or_else(|| pairs_parted(t, false)).unwrap_or_default()
}

/// [`query_pairs_emptied`]'s work, honouring quotes and braces that open a value or not: `None`
/// when one opened and never closed.
fn pairs_parted(t: &str, quotes: bool) -> Option<String> {
    let mut out = String::with_capacity(t.len());
    let mut from = 0;
    let mut open: Option<char> = None;
    // Right after a `=` (spaces aside): where a value, and so a quote or a brace, can start.
    let mut value_start = false;
    let mut chars = t.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match open {
            Some(close) if c == '\\' && close != '}' => {
                chars.next();
            }
            // ODBC writes a `}` in a braced value as `}}`.
            Some('}') if c == '}' && chars.peek().is_some_and(|&(_, n)| n == '}') => {
                chars.next();
            }
            Some(close) if c == close => open = None,
            Some(_) => {}
            None if quotes && value_start && (c == '\'' || c == '"') => open = Some(c),
            None if quotes && value_start && c == '{' => open = Some('}'),
            None if c == '&' && starts_pair(t.get(i + 1..).unwrap_or("")) => {
                out.push_str(&secret_pairs_emptied(t.get(from..i).unwrap_or("")));
                out.push('&');
                from = i + 1;
            }
            None => {}
        }
        value_start = open.is_none() && (c == '=' || (value_start && c == ' '));
    }
    if open.is_some() {
        return None;
    }
    out.push_str(&secret_pairs_emptied(t.get(from..).unwrap_or("")));
    Some(out)
}

/// Whether `s` starts with a pair's name and its `=`.
fn starts_pair(s: &str) -> bool {
    let n = s.find(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))).unwrap_or(s.len());
    n > 0 && s.get(n..).is_some_and(|r| r.trim_start().starts_with('='))
}

/// `t` without the PEM private keys pasted in it (`-----BEGIN … PRIVATE KEY-----` to its
/// `-----END … PRIVATE KEY-----`, or to the end of the text when it has none).
fn private_keys_removed(t: &str) -> String {
    const TAIL: &str = "PRIVATE KEY-----";
    let mut out = String::new();
    let mut rest = t;
    while let Some(tail) = rest.find(TAIL) {
        // Where this marker's line starts: its `-----BEGIN`.
        let begin = rest.get(..tail).and_then(|b| b.rfind("-----BEGIN")).unwrap_or(tail);
        out.push_str(rest.get(..begin).unwrap_or(""));
        let after = rest.get(tail + TAIL.len()..).unwrap_or("");
        rest = match after.find("-----END").and_then(|e| after.get(e..)?.find(TAIL).map(|t| e + t + TAIL.len())) {
            Some(end) => after.get(end..).unwrap_or(""),
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// `t` with the value of each `name=value` pair whose name says it's a secret
/// (`credentials::secret_key`; a bare `key`, `sig` or `sas` when its value reads as a key) left
/// empty, pairs being parted by spaces, commas or semicolons. A
/// value in quotes or braces (`password='a b'`, ODBC's `PWD={a;b}`) goes whole, to its closer
/// (to the end of the text when it has none).
fn secret_pairs_emptied(t: &str) -> String {
    let parts = |c: char| c.is_whitespace() || c == ',' || c == ';';
    let mut out = String::new();
    let mut rest = t;
    while !rest.is_empty() {
        let end = rest.find(parts).unwrap_or(rest.len());
        let item = rest.get(..end).unwrap_or(rest);
        // The pair's name and where its value starts in `rest`: `name=value`, or libpq's
        // `name = value`, spaces around the `=`. A space after the `=` comes before the value
        // when the next word isn't a pair of its own.
        let pair = match item.split_once('=') {
            Some((k, _)) => Some((k, k.len() + 1)),
            None => {
                let after = rest.get(end..).unwrap_or("");
                let eq = after.trim_start();
                eq.strip_prefix('=').map(|_| (item, end + (after.len() - eq.len()) + 1))
            }
        };
        let pair = pair.map(|(k, at)| {
            let value = rest.get(at..).unwrap_or("");
            let word = value.trim_start();
            let next = word.get(..word.find(parts).unwrap_or(word.len())).unwrap_or("");
            if word.len() < value.len() && !next.is_empty() && !next.contains('=') { (k, at + value.len() - word.len()) } else { (k, at) }
        });
        // A bare `key`, `sig` or `sas` is as likely a field in an expression (`key=5`): a secret
        // only when its value reads as one.
        let secret = |&(k, at): &(&str, usize)| {
            let k = k.trim();
            let value = rest.get(at..).unwrap_or("");
            let v = value.get(..value.find(parts).unwrap_or(value.len())).unwrap_or("");
            !k.is_empty()
                && rustgis_data::source::credentials::secret_key(k)
                && (!["key", "sig", "sas"].iter().any(|p| k.eq_ignore_ascii_case(p))
                    || crate::sources::looks_like_a_key(v.trim_matches(['\'', '"', '{', '}'])))
        };
        let Some((_, at)) = pair.filter(secret) else {
            // Not a secret: the item and the separator after it, as they are.
            let next = rest.get(end..).and_then(|r| r.chars().next()).map_or(end, |c| end + c.len_utf8());
            out.push_str(rest.get(..next).unwrap_or(rest));
            rest = rest.get(next..).unwrap_or("");
            continue;
        };
        // The name and the `=`, as they are (spaces before the value go: the separator follows).
        out.push_str(rest.get(..at).unwrap_or("").trim_end());
        // Past the value: to its closer when it opens with a quote or a brace, then to the next
        // separator.
        let value = rest.get(at..).unwrap_or("");
        let close = match value.chars().next() {
            Some(q @ ('\'' | '"')) => Some(q),
            Some('{') => Some('}'),
            _ => None,
        };
        let from = match close {
            Some(c) => closer(value, c).map_or(value.len(), |i| i + c.len_utf8()),
            None => 0,
        };
        let tail = value.get(from..).unwrap_or("");
        rest = tail.get(tail.find(parts).unwrap_or(tail.len())..).unwrap_or("");
    }
    out
}

/// Where the value `v` (opening with a quote or a brace) closes with `c`: past an escaped quote
/// (`\'`, as libpq writes one in a quoted value), which doesn't close it.
fn closer(v: &str, c: char) -> Option<usize> {
    let mut escaped = false;
    let mut chars = v.char_indices().skip(1).peekable();
    while let Some((i, ch)) = chars.next() {
        if escaped {
            escaped = false;
        } else if ch == '\\' && c != '}' {
            escaped = true;
        } else if ch == c {
            // ODBC writes a `}` in a braced value as `}}`: it doesn't close it.
            if c == '}' && chars.peek().is_some_and(|&(_, n)| n == '}') {
                chars.next();
                continue;
            }
            return Some(i);
        }
    }
    None
}

/// Whether `t` is a database connection `sources::redact` reads as one: PostgreSQL's (a URI, or
/// libpq's `key=value` words after `postgres:`, `postgresql:` or GDAL's `PG:`), MySQL's,
/// MariaDB's or SQL Server's.
fn connection(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    ["postgres:", "postgresql:", "pg:", "mysql:", "mariadb:", "mssql:"].iter().any(|p| lower.starts_with(p))
}

/// The project file a script opens: the one it was opened from or saved to, else one named
/// after it (to save it as, first).
fn project_file(s: &Session) -> String {
    match &s.path {
        Some(p) => p.display().to_string(),
        None => format!("{}.rgis", file_stem(&s.project.name)),
    }
}

/// `name` as a file name on every system: letters, digits, spaces, `-` and `_` kept (so a
/// project saved under its own name, "World Atlas.rgis", is the file the script opens), anything
/// else (a separator, `* ? " < > |`, a control character) `_`.
fn file_stem(name: &str) -> String {
    let stem: String = name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ') { c } else { '_' }).collect();
    let stem = stem.trim();
    if stem.is_empty() { "project".into() } else { stem.to_string() }
}

/// A call of the `rustgis` package's `Session.run`, one parameter a line (as ArcGIS Pro copies a
/// tool's Python): keywords where the names allow, `gp.run`'s tool before its parameters.
pub fn python(cmd: &str, params: &Value) -> String {
    let mut out = format!("s.run(\n    {},\n", quoted(cmd));
    let empty = Map::new();
    let o = params.as_object().unwrap_or(&empty);
    if o.keys().all(|k| identifier(k)) {
        // `tool` first: the JSON map keeps its keys sorted.
        let mut keys: Vec<&String> = o.keys().collect();
        keys.sort_by_key(|k| (k.as_str() != "tool", k.as_str() == "params"));
        for k in keys {
            if let Some(v) = o.get(k) {
                out.push_str(&format!("    {k}={},\n", py(v, 1)));
            }
        }
    } else {
        out.push_str(&format!("    {},\n", py(params, 1)));
    }
    out.push(')');
    out
}

/// `v` as a Python literal, objects one key a line at `indent` levels.
fn py(v: &Value, indent: usize) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).into(),
        Value::Number(n) => n.to_string(),
        Value::String(t) => quoted(t),
        Value::Array(a) => format!("[{}]", a.iter().map(|x| py(x, indent)).collect::<Vec<_>>().join(", ")),
        Value::Object(o) if o.is_empty() => "{}".into(),
        Value::Object(o) => {
            let pad = "    ".repeat(indent);
            let items: String = o.iter().map(|(k, x)| format!("{pad}    {}: {},\n", quoted(k), py(x, indent + 1))).collect();
            format!("{{\n{items}{pad}}}")
        }
    }
}

/// A string literal Python reads back as `t`: JSON's escapes are Python's too.
fn quoted(t: &str) -> String {
    serde_json::to_string(t).unwrap_or_else(|_| "\"\"".into())
}

/// Whether `k` can be a keyword argument: a Python identifier, not one of its reserved words.
fn identifier(k: &str) -> bool {
    const RESERVED: &[&str] = &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else",
        "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise",
        "return", "try", "while", "with", "yield",
    ];
    let mut chars = k.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !RESERVED.contains(&k)
}

/// `t` quoted for a POSIX shell: in single quotes, each of its own closing them for a moment.
fn shell(t: &str) -> String {
    format!("'{}'", t.replace('\'', "'\\''"))
}

/// `t` on one line, for a comment. What may end a line (a lone `\r` does, for Python), or any
/// other control character, becomes a space: the text comes from the project file, and a line
/// break in it would leave the rest of it outside the comment, as code.
fn line(t: &str) -> String {
    t.chars().map(|c| if c.is_control() || c == '\u{2028}' || c == '\u{2029}' { ' ' } else { c }).take(300).collect()
}
