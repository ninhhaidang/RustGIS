//! The session log (#797): what was run in a session ([`SessionLog`]), which commands go in and
//! what of them is kept. It is held in memory only: a saved or shared project carries none of it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CmdResult, Session};

/// A redacted output projection. Never use it as executable parameters or approval identity.
pub struct LogCopy(Value);

impl LogCopy {
    /// Consumes the output-only wrapper for serialization.
    pub fn into_log_value(self) -> Value {
        self.0
    }
}

/// The session log's bounded, document-aware parameter policy, shared with the agent audit.
/// The source value is never mutated and remains the only input to execution and planning.
pub fn log_params(params: &Value) -> LogCopy {
    LogCopy(kept_params(params))
}

/// Cleans a structural command/tool identifier for display and audit output only.
/// The original identifier remains the sole input to lookup, execution and approval.
pub fn log_identifier(id: &str) -> String {
    let public = crate::roles::identifier(id, &mut crate::durable::Report::default());
    crate::durable::generated_text(public.as_deref().unwrap_or(id))
}

/// Now, in milliseconds since 1970 (UTC).
pub(crate) fn now_ms() -> u64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Whether command `id`, run by itself with `params`, goes into the log: one that changes the
/// project (all but those built with `.view()`; a plugin's when its manifest asks for the Edit
/// permission), undo and redo, a save, the log's own clearing and switching, and a command whose
/// parameters name a file, folder, address or connection (an export), as the plugin permission
/// gate reads them. Reading and looking around (listing, describing, moving the view) is left
/// out: agents, scripts and plugins call those hundreds of times, and they would push the changes
/// out of the log.
pub(crate) fn is_logged(s: &Session, id: &str, params: &Value) -> bool {
    // Making or opening a project from text names no place: it is logged as the open from a file is.
    if matches!(
        id,
        "edit.undo"
            | "edit.redo"
            | "project.save"
            | "project.clearLog"
            | "project.logCommands"
            | "project.new"
            | "project.sample"
            | "project.openText"
    ) {
        return true;
    }
    let mutates = if id.starts_with(rustgis_plugins::PREFIX) {
        rustgis_plugins::command(id).is_some_and(|c| c.mutates())
    } else {
        match s.registry.get(id) {
            Some(spec) => spec.mutates,
            None => return false,
        }
    };
    mutates || crate::cmd::items::names_place(params, crate::cmd::plugins::PLACE_KEYS, 0)
}

/// `params` as the log keeps them: a document given as text (a map, a project, a capabilities
/// answer, GeoJSON) as its size only, since one may hold secrets no redaction finds in it (an S3
/// source's token inside a map's JSON); the rest clipped (so a long script costs little), then
/// without secrets (as History's Copy as code takes them out). A secret cut short is still taken
/// out: what is left of it follows its name. Within [`LOG_PARAMS_BYTES`] of JSON in three tries,
/// each giving up more: texts at that bound and lists at their first [`LOG_LIST_ITEMS`]; then
/// texts at 512 characters; then the first keys with their short values only.
pub(crate) fn kept_params(params: &Value) -> Value {
    let v = redacted(&clip(params, LOG_PARAMS_BYTES, LOG_LIST_ITEMS, 0));
    if serde_json::to_vec(&v).map_or(0, |b| b.len()) <= LOG_PARAMS_BYTES {
        return v;
    }
    // Texts shorter; lists as they are (already cut, their "… N items in all" kept).
    let clipped = clip(&v, 512, usize::MAX, 0);
    if serde_json::to_vec(&clipped).map_or(0, |b| b.len()) <= LOG_PARAMS_BYTES {
        return clipped;
    }
    // Still too large (many keys, long lists): its first keys with their short values only (a
    // number, a flag, a text of up to 64 characters with no brace), the rest as "…", as many as
    // fit within the bound. Nothing in it is cut JSON, so it reads back as it was written.
    let short = |v: &Value| match v {
        Value::String(t) if t.chars().nth(64).is_none() && !t.contains('{') => v.clone(),
        Value::Number(_) | Value::Bool(_) | Value::Null => v.clone(),
        _ => Value::String("…".into()),
    };
    match clipped {
        Value::Object(o) => {
            // Each key and value is short, but forty of them can still add up (escaped or
            // multi-byte text): each pair counts its bytes, a colon and a comma. Room is kept for
            // the note of how many keys there were, when some are left out.
            let mut room = LOG_PARAMS_BYTES.saturating_sub(2 + 48);
            let mut out = serde_json::Map::new();
            for (k, v) in o.iter().take(40) {
                let (k, v): (String, Value) = (k.chars().take(64).collect(), short(v));
                // Two keys alike in their first 64 characters: the first stays.
                if out.contains_key(&k) {
                    continue;
                }
                let size = serde_json::to_vec(&k)
                    .map_or(usize::MAX, |b| b.len())
                    .saturating_add(serde_json::to_vec(&v).map_or(usize::MAX, |b| b.len()))
                    .saturating_add(2);
                if size > room {
                    break;
                }
                room -= size;
                out.insert(k, v);
            }
            if out.len() < o.len() {
                out.insert("…".into(), Value::String(format!("{} keys in all", o.len())));
            }
            Value::Object(out)
        }
        other => short(&other),
    }
}

/// An error as the log keeps it: clipped to [`LOG_ERROR_CHARS`] before its secrets are taken out,
/// so a long one costs little (a secret cut short still follows its name).
fn kept_error(e: &str) -> String {
    let e = if e.chars().nth(LOG_ERROR_CHARS).is_some() {
        format!("{}…", e.chars().take(LOG_ERROR_CHARS).collect::<String>())
    } else {
        e.to_string()
    };
    crate::cmd::history::redacted_text(&sql_secrets_removed(&e))
}

/// `v` without secrets: as History's Copy as code takes them out (`history::redacted`), and,
/// first, inside the JSON a text holds (a script's command lines: `s3.connect {"secret": …}`),
/// whose objects are redacted in place.
/// A bounded review's display projection, without the log's clipping. Never used for
/// execution, consent equality or provenance; the policy retains its private original.
pub(crate) fn review_params(v: &Value) -> Result<Value, String> {
    review_value(v, 0)
}

/// Projects bounded review data recursively, including JSON held inside nested script strings.
/// A card that cannot show its structure safely is refused; log/diagnostic clipping is separate.
fn review_value(v: &Value, depth: usize) -> Result<Value, String> {
    if depth > 32 {
        return Err("too deeply nested to show in full; ask for a smaller request".into());
    }
    Ok(match v {
        Value::Object(o) => {
            let mut out = serde_json::Map::new();
            for (key, value) in o {
                let sensitive =
                    rustgis_data::address::normalized_name(key).as_deref() != Some("key") && rustgis_data::address::log_sensitive(key);
                let Value::String(public_key) = review_value(&Value::String(key.clone()), depth + 1)? else {
                    return Err("a review field name must be text".into());
                };
                let value = if sensitive { Value::String(String::new()) } else { review_value(value, depth + 1)? };
                if out.insert(public_key, value).is_some() {
                    return Err("redacted field names cannot be distinguished; simplify the request".into());
                }
            }
            crate::cmd::history::redacted(&Value::Object(out))
        }
        Value::Array(a) => Value::Array(a.iter().map(|v| review_value(v, depth + 1)).collect::<Result<_, _>>()?),
        Value::String(text) => {
            let mut out = String::new();
            let mut rest = text.as_str();
            while let Some(at) = rest.find(['{', '[']) {
                let (before, from) = rest.split_at(at);
                out.push_str(before);
                let mut values = serde_json::Deserializer::from_str(from).into_iter::<Value>();
                match values.next() {
                    Some(Ok(value @ (Value::Object(_) | Value::Array(_)))) => {
                        let value = review_value(&value, depth + 1)?;
                        out.push_str(&value.to_string());
                        rest = from.get(values.byte_offset()..).unwrap_or("");
                    }
                    _ if from.get(1..).is_some_and(|r| r.trim_start().starts_with(['"', '\''])) || last_secret_name(from).is_some() => {
                        return Err("structured text cannot be safely shown in full; simplify the request".into());
                    }
                    _ => {
                        let first = from.chars().next().ok_or("empty structured text")?;
                        out.push(first);
                        rest = from.get(first.len_utf8()..).unwrap_or("");
                    }
                }
            }
            out.push_str(rest);
            Value::String(crate::cmd::history::redacted_text(&sql_secrets_removed(&out)))
        }
        other => other.clone(),
    })
}

fn redacted(v: &Value) -> Value {
    crate::cmd::history::redacted(&embedded_redacted(v, 0))
}

/// A bounded log-only context check for a short key beside nested connection settings.
/// User data/documents do not make an ordinary join or lookup key a credential.
pub(crate) fn address_context(key: &str, v: &Value, depth: usize) -> bool {
    if depth >= 8 || matches!(key, "data" | "text" | "values" | "attributes" | "properties" | "features" | "geometry") {
        return false;
    }
    match v {
        Value::String(s) => {
            let s = s.trim_start();
            [
                "http://",
                "https://",
                "s3://",
                "rustgis-login:",
                "rustgis-anonymous:",
                "rustgis-s3:",
                "postgres:",
                "postgresql:",
                "pg:",
                "mysql:",
                "mariadb:",
                "mssql:",
                "pmtiles:",
                "cog:",
            ]
            .iter()
            .any(|prefix| s.get(..prefix.len()).is_some_and(|head| head.eq_ignore_ascii_case(prefix)))
        }
        Value::Array(values) => values.iter().take(LOG_LIST_ITEMS).any(|v| address_context("", v, depth + 1)),
        Value::Object(values) => values.iter().any(|(key, v)| address_context(key, v, depth + 1)),
        _ => false,
    }
}

/// Scrubs bounded nested log parameters and JSON embedded in text before the log retains them.
fn embedded_redacted(v: &Value, depth: usize) -> Value {
    match v {
        Value::String(t) => {
            let t = if t.contains('{') { json_in_text_redacted(t) } else { t.clone() };
            Value::String(sql_secrets_removed(&t))
        }
        Value::Array(a) if depth < 8 => Value::Array(a.iter().map(|x| embedded_redacted(x, depth + 1)).collect()),
        Value::Object(o) if depth < 8 => Value::Object(
            o.iter()
                .map(|(k, x)| {
                    // Preserve History's plain-key refinement; the log-only fields share diagnostics'
                    // canonical list. Clipping already bounds recursion and text before this pass.
                    let sensitive =
                        rustgis_data::address::normalized_name(k).as_deref() != Some("key") && rustgis_data::address::log_sensitive(k);
                    (k.clone(), if sensitive { Value::String(String::new()) } else { embedded_redacted(x, depth + 1) })
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// `t` with each JSON object it holds (from a `{` to where the object ends) redacted. Texts are
/// clipped before this, so trying each `{` stays cheap; the secret names are looked for once.
pub(crate) fn json_in_text_redacted(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let last_secret = last_secret_name(t);
    let mut rest = t;
    while let Some(at) = rest.find('{') {
        // Where this `{` is in `t`: a secret's name after it is one at a later offset.
        let brace = t.len() - rest.len() + at;
        let (before, from) = rest.split_at(at);
        out.push_str(before);
        let mut objects = serde_json::Deserializer::from_str(from).into_iter::<Value>();
        match objects.next() {
            Some(Ok(v @ Value::Object(_))) => {
                let end = objects.byte_offset();
                out.push_str(&serde_json::to_string(&crate::cmd::history::redacted(&v)).unwrap_or_default());
                rest = from.get(end..).unwrap_or("");
            }
            // Cut short by the clip, or an object that doesn't parse (a trailing comma, single
            // quotes) though a key starts it, or whose keys aren't quoted but one is a secret's
            // (`{secretAccessKey: …}`): what follows its `{` may hold a secret, so none of it
            // stays. Text such as `{name}` or `{x:.2f}` stays as it is.
            Some(Err(e))
                if e.is_eof()
                    || from.get(1..).is_some_and(|r| r.trim_start().starts_with(['"', '\'']))
                    || last_secret.is_some_and(|s| s > brace) =>
            {
                out.push('…');
                return out;
            }
            _ => {
                out.push('{');
                rest = from.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
    out
}

/// `t` with each SQL `CREATE … SECRET`'s options (`CREATE SECRET s3 (TYPE s3, KEY_ID 'a',
/// SECRET 'b')`, DuckDB's form, `OR REPLACE` and `PERSISTENT` too) as `(…)`: their values are
/// written as plain words, which no name or address form finds. With no `)`, the rest goes.
pub(crate) fn sql_secrets_removed(t: &str) -> String {
    // ASCII lowercase keeps every byte where it was.
    let lower = t.to_ascii_lowercase();
    let mut out = String::with_capacity(t.len());
    let mut from = 0;
    while let Some(at) = lower.get(from..).and_then(|r| r.find("create")).map(|i| i + from) {
        let after = at + "create".len();
        // `create` as a word of its own (not `recreate` or `created`).
        let word = lower.get(..at).and_then(|b| b.chars().next_back()).is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
            && lower.get(after..).is_some_and(|r| r.starts_with(char::is_whitespace));
        if !word {
            out.push_str(t.get(from..after).unwrap_or(""));
            from = after;
            continue;
        }
        // The statement's head, to its `(`: `create [or replace] [persistent] secret [name] (`.
        let Some(open) = lower.get(after..).and_then(|r| r.find(['(', ';'])).map(|i| i + after) else { break };
        let head = lower.get(after..open).unwrap_or("");
        // The object's kind comes first (`SECRET`, after `OR REPLACE`, `PERSISTENT` or
        // `TEMPORARY`): a table named `secret` isn't one.
        let kind = head.split_whitespace().find(|w| !matches!(*w, "or" | "replace" | "persistent" | "temporary" | "temp"));
        if !lower.get(open..).is_some_and(|r| r.starts_with('(')) || kind != Some("secret") {
            out.push_str(t.get(from..after).unwrap_or(""));
            from = after;
            continue;
        }
        out.push_str(t.get(from..open).unwrap_or(""));
        out.push_str("(…");
        // Its `)`, quotes and nested brackets aside.
        let (mut depth, mut quote, mut close) = (0usize, None, None);
        for (i, c) in t.get(open..).unwrap_or("").char_indices() {
            match (quote, c) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '\'' | '"') => quote = Some(c),
                (None, '(') => depth += 1,
                (None, ')') => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        close = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        match close {
            Some(c) => {
                out.push(')');
                from = c + 1;
            }
            None => return out,
        }
    }
    out.push_str(t.get(from..).unwrap_or(""));
    out
}

/// Where in `t` the last secret's name (`credentials::secret_key`) written as a key, quoted or
/// not, before a `:` or `=` (`secretAccessKey: "…"`, `"token": …`, `token = …`) starts: one pass
/// over the text.
fn last_secret_name(t: &str) -> Option<usize> {
    let name = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.');
    let mut last = None;
    let mut rest = t;
    while let Some(at) = rest.find(name) {
        let from = rest.get(at..).unwrap_or("");
        let end = from.find(|c: char| !name(c)).unwrap_or(from.len());
        let (key, after) = from.split_at(end);
        let separator = after.trim_start_matches(['"', '\'']).trim_start();
        if separator.starts_with([':', '=']) && rustgis_data::source::credentials::secret_key(key) {
            last = Some(t.len() - from.len());
        }
        rest = after;
    }
    last
}

/// The longest text kept as it is that reads as a document (it starts with `{`, `[` or `<`).
const DOCUMENT_CHARS: usize = 200;

/// Whether `t`, past [`DOCUMENT_CHARS`], is a file's contents in base64 (`layer.addDataBytes`'s
/// `data`, or a `data:` address): only base64's characters after any `data:…;base64,` head. A
/// secret's name can't be seen in it, so none of it is kept.
fn is_base64(t: &str) -> bool {
    let body = t.strip_prefix("data:").and_then(|r| r.split_once(";base64,")).map_or(t, |(_, b)| b);
    body.len() > DOCUMENT_CHARS
        && body.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_' | b'\n' | b'\r'))
}

/// `v` with each text cut to `chars` characters (a key to 200), a text that reads as a document
/// (JSON or XML past [`DOCUMENT_CHARS`]) or as a file's contents in base64 given as its size, and
/// each list cut to its first `items` (and how many there were); nesting past eight levels is
/// left out.
fn clip(v: &Value, chars: usize, items: usize, depth: usize) -> Value {
    match v {
        // A byte-order mark before it is still a document's start.
        Value::String(t)
            if t.trim_start_matches('\u{feff}').trim_start().starts_with(['{', '[', '<']) && t.chars().nth(DOCUMENT_CHARS).is_some() =>
        {
            Value::String(format!("(a document of {} characters)", t.chars().count()))
        }
        Value::String(t) if is_base64(t) => Value::String(format!("(file contents of {} characters, base64)", t.len())),
        Value::String(t) if t.chars().nth(chars).is_some() => Value::String(format!("{}…", t.chars().take(chars).collect::<String>())),
        Value::Array(a) if depth < 8 => {
            let mut out: Vec<Value> = a.iter().take(items).map(|x| clip(x, chars, items, depth + 1)).collect();
            if a.len() > items {
                out.push(Value::String(format!("… {} items in all", a.len())));
            }
            Value::Array(out)
        }
        Value::Object(o) if depth < 8 => {
            // Two keys alike in their first 200 characters: the first stays. A key cut short may
            // hide a secret's name past the cut (redaction reads the names after this): its value
            // goes.
            let mut out = serde_json::Map::new();
            for (k, x) in o {
                let value = || if k.chars().nth(200).is_some() { Value::String("…".into()) } else { clip(x, chars, items, depth + 1) };
                out.entry(k.chars().take(200).collect::<String>()).or_insert_with(value);
            }
            Value::Object(out)
        }
        Value::Array(_) | Value::Object(_) => Value::String("…".into()),
        other => other.clone(),
    }
}

/// The entry of command `id`, which started at `at` and took `took`; its error without secrets,
/// clipped to [`LOG_ERROR_CHARS`].
pub(crate) fn entry(id: &str, params: Value, res: &CmdResult, at: u64, took: web_time::Duration) -> LogEntry {
    let error = res.as_ref().err().map(|e| kept_error(e));
    LogEntry {
        at,
        command: id.chars().take(200).collect(),
        params,
        ok: res.is_ok(),
        error,
        millis: u64::try_from(took.as_millis()).unwrap_or(u64::MAX),
    }
}

/// Most entries the log keeps; past it, the oldest go first.
pub const LOG_LIMIT: usize = 10_000;
/// The most entries of plugins' commands the log keeps, their oldest going first past it: a
/// plugin run many times can't push the user's own entries out of the log.
pub const LOG_PLUGIN_LIMIT: usize = 1_000;
/// Most bytes of JSON an entry's parameters keep (a long text is cut, a long list keeps its first
/// [`LOG_LIST_ITEMS`]), and most characters of its error.
pub const LOG_PARAMS_BYTES: usize = 4096;
pub const LOG_LIST_ITEMS: usize = 100;
pub const LOG_ERROR_CHARS: usize = 500;

/// One command run in the session: when it started (UTC, milliseconds since 1970), its id, its
/// parameters (secrets taken out, large values clipped), whether it succeeded, its error, and how
/// long it took.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub at: u64,
    pub command: String,
    #[serde(default)]
    pub params: serde_json::Value,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub millis: u64,
}

impl LogEntry {
    /// When it started, as `2026-10-30 14:05:09` (UTC).
    pub fn time_utc(&self) -> String {
        let (date, time) = rustgis_map::graphics::civil(self.at / 1000);
        format!("{date} {time}:{:02}", (self.at / 1000) % 60)
    }
}

/// The session's log (#797): one entry for each command run in the session, from any front end
/// and in whatever project was open, oldest first. It sits outside the project and its undo
/// history: undoing a command doesn't take its entry back, and saving, sharing or packaging a
/// project carries none of it. It lasts until RustGIS closes, or until it is cleared.
#[derive(Debug, Default)]
pub struct SessionLog {
    /// Logging turned off (`project.logCommands`); on by default.
    off: bool,
    /// Oldest first; the oldest go first past [`LOG_LIMIT`], without moving the rest.
    entries: std::collections::VecDeque<LogEntry>,
    /// How many of `entries` are plugins' commands', kept in step with them so a push doesn't
    /// count them.
    plugin_entries: usize,
    /// Counts each change to `entries` (an entry added, the log cleared), so a view of it knows
    /// when to read it again ([`SessionLog::changes`]).
    changes: u64,
    /// How many entries were dropped since the log was last cleared, so a view or a saved file
    /// can say so ([`SessionLog::dropped_said`]): the oldest, past [`LOG_LIMIT`], and older plugin
    /// commands' (newer than some of the user's still kept), past [`LOG_PLUGIN_LIMIT`].
    dropped: u64,
    dropped_plugin: u64,
}

impl SessionLog {
    pub fn on(&self) -> bool {
        !self.off
    }

    pub fn set_on(&mut self, on: bool) {
        self.off = !on;
    }

    /// Adds `e` when logging is on, the oldest entries dropped past [`LOG_LIMIT`], and a plugin's
    /// command's past [`LOG_PLUGIN_LIMIT`] the oldest of those; turning the log off
    /// (`project.logCommands`), and clearing it while off, are recorded too, so the log says why
    /// it stops or is empty.
    pub fn push(&mut self, e: LogEntry) {
        if self.off && !matches!(e.command.as_str(), "project.logCommands" | "project.clearLog") {
            return;
        }
        let plugin = |e: &LogEntry| e.command.starts_with(rustgis_plugins::PREFIX);
        if plugin(&e)
            && self.plugin_entries >= LOG_PLUGIN_LIMIT
            && let Some(oldest) = self.entries.iter().position(plugin)
        {
            self.entries.remove(oldest);
            self.plugin_entries = self.plugin_entries.saturating_sub(1);
            self.dropped_plugin = self.dropped_plugin.saturating_add(1);
        }
        if plugin(&e) {
            self.plugin_entries += 1;
        }
        self.changes = self.changes.wrapping_add(1);
        self.entries.push_back(e);
        while self.entries.len() > LOG_LIMIT {
            if self.entries.pop_front().is_some_and(|x| plugin(&x)) {
                self.plugin_entries = self.plugin_entries.saturating_sub(1);
            }
            self.dropped = self.dropped.saturating_add(1);
        }
    }

    /// Adds `e` as part of the last entry when that is the same command, run well: a step merged
    /// into the last undo step ([`crate::Session::merge_undo`]). The entry keeps when the first
    /// started, takes the last one's parameters, and adds up how long they took.
    pub fn join_last(&mut self, mut e: LogEntry) {
        if let Some(last) = self.entries.back_mut().filter(|l| l.ok && l.command == e.command) {
            e.at = last.at;
            e.millis = last.millis.saturating_add(e.millis);
            *last = e;
            // The newest entry changed: a view of the log reads it again.
            self.changes = self.changes.wrapping_add(1);
            return;
        }
        self.push(e);
    }

    /// The entries, oldest first.
    pub fn entries(&self) -> impl ExactSizeIterator<Item = &LogEntry> {
        self.entries.iter()
    }

    /// The last `limit` entries that `keep` keeps, oldest first, with how many it keeps and how
    /// many there are: only those are copied.
    pub fn select(&self, keep: impl Fn(&LogEntry) -> bool, limit: usize) -> (Vec<LogEntry>, usize, usize) {
        let matching: Vec<&LogEntry> = self.entries.iter().filter(|e| keep(e)).collect();
        let tail = matching.get(matching.len().saturating_sub(limit)..).unwrap_or_default().iter().map(|e| (*e).clone()).collect();
        (tail, matching.len(), self.entries.len())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// How many times the entries have changed (one added, the log cleared): the same number
    /// means the same entries.
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// How many entries were dropped past the log's limits since it was last cleared.
    pub fn dropped(&self) -> u64 {
        self.dropped.saturating_add(self.dropped_plugin)
    }

    /// What was dropped past the log's limits since it was last cleared, by kind, as the pane, an
    /// export and a download say it; `None` when nothing was.
    pub fn dropped_said(&self) -> Option<String> {
        let mut said = Vec::new();
        if self.dropped > 0 {
            said.push(format!("{} oldest entries were dropped past the log's limit of {LOG_LIMIT}", self.dropped));
        }
        if self.dropped_plugin > 0 {
            said.push(format!(
                "{} older plugin command entries were dropped past the {LOG_PLUGIN_LIMIT} kept for plugins",
                self.dropped_plugin
            ));
        }
        (!said.is_empty()).then(|| said.join("; "))
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Writes the log to `w`, oldest first: as CSV (`csv`: a header, then one row an entry, a
    /// readable UTC time first and the parameters as a JSON column) or as JSON Lines (one entry a
    /// line). Entry by entry, so a long log isn't held as text first.
    pub fn write(&self, w: &mut dyn std::io::Write, csv: bool) -> std::io::Result<()> {
        if csv {
            w.write_all(b"time_utc,at,command,ok,millis,error,params\n")?;
        }
        self.entries.iter().try_for_each(|e| write_entry(w, e, csv))
    }

    /// Empties the log; how many entries it had.
    pub fn clear(&mut self) -> usize {
        self.plugin_entries = 0;
        self.dropped = 0;
        self.dropped_plugin = 0;
        self.changes = self.changes.wrapping_add(1);
        std::mem::take(&mut self.entries).len()
    }
}

/// A CSV cell: quoted, and kept from being read as a formula where it starts as one does (`=`,
/// `+`, `-`, `@`, or a tab or carriage return before them), as spreadsheets would.
fn csv_cell(t: &str) -> String {
    let lead = if t.starts_with(['=', '+', '-', '@', '\t', '\r']) { "'" } else { "" };
    format!("\"{lead}{}\"", t.replace('"', "\"\""))
}

/// Entry `e` as a CSV row (`csv`) or a JSON line, written to `w`.
fn write_entry(w: &mut dyn std::io::Write, e: &LogEntry, csv: bool) -> std::io::Result<()> {
    if csv {
        writeln!(
            w,
            "{},{},{},{},{},{},{}",
            e.time_utc(),
            e.at,
            csv_cell(&e.command),
            e.ok,
            e.millis,
            csv_cell(e.error.as_deref().unwrap_or("")),
            csv_cell(&e.params.to_string())
        )
    } else {
        serde_json::to_writer(&mut *w, e).map_err(std::io::Error::other)?;
        w.write_all(b"\n")
    }
}
