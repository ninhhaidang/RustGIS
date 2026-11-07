//! The agents' audit log: every tool call an agent made on the open project through the desktop
//! app ([`crate::relay`]), one JSON line each, in a file a day in the app-data folder. It lets the
//! user see what an agent did, and replay or review it.
//!
//! Each line is `{ts, source, tool, command?, script?, args, success, duration_s, error}`.
//! Arguments lose passwords, keys and tokens before they are written. Files older than
//! [`KEEP_DAYS`] are removed when a new day's file starts.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// How many days of logs are kept.
pub const KEEP_DAYS: u64 = 30;

/// The most bytes of a call's arguments a line keeps.
const MAX_ARGS: usize = 8_000;

/// The folder the desktop app keeps the log in: `mcp-audit` in the app-data folder.
pub fn dir() -> Option<PathBuf> {
    rustgis_engine::data::dirs::app_data_dir().map(|d| d.join("mcp-audit"))
}

/// The day's log file in `dir` for `secs` since the Unix epoch (`2026-10-18.jsonl`).
pub fn file_for(dir: &Path, secs: i64) -> PathBuf {
    let day: String = crate::chatgpt::rfc3339(secs).chars().take(10).collect();
    dir.join(format!("{day}.jsonl"))
}

/// Appends a line for each tool call in `request` (a JSON-RPC message or batch) with how its
/// `reply` went. `source` says where it came from (`mcp`, `file`). Logging never fails the call:
/// a log that can't be written is skipped.
pub fn record(dir: &Path, source: &str, request: &str, reply: Option<&str>, took: Duration) {
    // One reading of the clock, so a call at midnight lands in one day's file.
    let secs = now();
    let lines = entries(source, request, reply, took, secs);
    if lines.is_empty() {
        return;
    }
    let path = file_for(dir, secs);
    let new_day = !path.exists();
    // The log says what agents did with the user's files and addresses: this user's alone.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut folder = std::fs::DirBuilder::new();
    folder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut folder, 0o700);
    if folder.create(dir).is_err() {
        return;
    }
    // A folder made before (by hand, or looser) is made private too, once a day.
    #[cfg(unix)]
    if new_day {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let mut o = std::fs::OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    // A log that can't be written doesn't fail the call, but it's said.
    if let Err(e) = o.open(&path).and_then(|mut file| file.write_all(text.as_bytes())) {
        log::warn!("MCP audit log {}: {e}", path.display());
        return;
    }
    if new_day {
        prune(dir, secs);
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX)).unwrap_or(0)
}

/// The log lines for the tool calls in `request`, at `secs`.
pub fn entries(source: &str, request: &str, reply: Option<&str>, took: Duration, secs: i64) -> Vec<Value> {
    let Ok(msg) = serde_json::from_str::<Value>(request) else { return Vec::new() };
    let messages = msg.as_array().map(Vec::as_slice).unwrap_or_else(|| std::slice::from_ref(&msg));
    let replies: Value = reply.and_then(|r| serde_json::from_str(r).ok()).unwrap_or(Value::Null);
    let replies = replies.as_array().map(Vec::as_slice).unwrap_or_else(|| std::slice::from_ref(&replies));
    let duration_s = (took.as_secs_f64() * 1000.0).round() / 1000.0;
    // A batch is timed as a whole: each of its lines carries that time and says so.
    let batch = msg.is_array();
    messages
        .iter()
        .filter(|m| m.get("method").and_then(Value::as_str) == Some("tools/call") && m.get("id").is_some())
        .map(|m| {
            let params = m.get("params").cloned().unwrap_or(Value::Null);
            let args =
                normalized_args(params.get("name").and_then(Value::as_str).unwrap_or(""), params.get("arguments").unwrap_or(&Value::Null));
            let r = replies.iter().find(|r| r.get("id") == m.get("id"));
            let failed = r.is_none_or(|r| r.get("error").is_some() || r.pointer("/result/isError") == Some(&Value::Bool(true)));
            let error = failed.then(|| {
                let text = r
                    .and_then(|r| r.pointer("/error/message").or_else(|| r.pointer("/result/content/0/text")))
                    .and_then(Value::as_str)
                    .unwrap_or("no reply");
                let text = redact_string(text);
                // Free text can name a credential the redaction can't find (`token abc123`):
                // such an error is withheld, not kept.
                if mentions_secret(&text) {
                    "[the error text mentions a credential; withheld]".to_string()
                } else {
                    crate::tools::clip(&text, 2000)
                }
            });
            let mut line = json!({
                "ts": crate::chatgpt::rfc3339(secs),
                "source": source,
                "tool": params.get("name"),
                "success": !failed,
                "duration_s": duration_s,
                "error": error,
            });
            if let Some(o) = line.as_object_mut() {
                if batch {
                    o.insert("batch".into(), json!(true));
                }
                for key in ["command", "script"] {
                    if let Some(v) = args.get(key).and_then(Value::as_str) {
                        o.insert(key.into(), json!(crate::tools::clip(v, MAX_ARGS)));
                    }
                }
                let text = args.to_string();
                o.insert(
                    "args".into(),
                    if text.len() <= MAX_ARGS {
                        args
                    } else {
                        json!({"cut": true, "bytes": text.len(), "text": crate::tools::clip(&text, MAX_ARGS)})
                    },
                );
            }
            line
        })
        .collect()
}

/// Arguments as the executor reads them. Only this output copy is redacted; it is never
/// supplied to the executor, the permission planner or approval equality.
fn normalized_args(tool: &str, arguments: &Value) -> Value {
    match tool {
        "run_command" | "run_tool" => {
            let params = match rustgis_engine::params_envelope(arguments.get("params")) {
                Ok(params) => rustgis_engine::log_params(&params).into_log_value(),
                Err(_) => json!("[parameters not JSON; withheld]"),
            };
            let key = if tool == "run_command" { "command" } else { "tool" };
            let mut out = serde_json::Map::new();
            let id = match arguments.get(key) {
                Some(Value::String(id)) => json!(rustgis_engine::log_identifier(id)),
                other => redact(other.cloned().unwrap_or(Value::Null), 0),
            };
            out.insert(key.into(), id);
            out.insert("params".into(), params);
            Value::Object(out)
        }
        "run" => json!({"script": redact_script(arguments.get("script").and_then(Value::as_str).unwrap_or(""))}),
        _ => redact(arguments.clone(), 0),
    }
}

/// A string argument without its secrets: an address (one word with a scheme, or a database
/// connection string) loses its password, keys and tokens; `password=…` in a keyword connection
/// string is withheld. Other text is kept as it is, not cut at a `?` or `#`.
fn redact_string(s: &str) -> String {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    // A password given as `password=…` or `password: …` (a keyword connection string, quoted or
    // not): the whole string is withheld, since a quoted value can hold spaces.
    let names_password = ["password", "passwd", "pwd"].iter().any(|k| {
        lower.match_indices(k).any(|(i, _)| lower.get(i + k.len()..).is_some_and(|rest| rest.trim_start().starts_with(['=', ':'])))
    });
    if names_password {
        return "[withheld: it gives a password]".to_string();
    }
    let schemes = ["postgres:", "postgresql:", "pg:", "mysql:", "mariadb:", "mssql:"];
    let address = |w: &str| {
        let l = w.to_ascii_lowercase();
        w.contains("://") || schemes.iter().any(|p| l.starts_with(p))
    };
    if !t.contains(char::is_whitespace) {
        return if address(t) { rustgis_engine::sources::redact(t) } else { s.to_string() };
    }
    // Text with an address in it (a query, a description): each address word that carries
    // credentials (a user and password, or a key or token in its query) loses them; the rest,
    // and the spacing, stay as they were.
    let carries = |w: &str| {
        let q = w.split_once('?').map_or("", |(_, q)| q.split('#').next().unwrap_or(q));
        let names = rustgis_engine::data::address::QueryNames::of(q);
        let secret_pair = q.split(['&', ';']).any(|pair| {
            let name = pair.split_once('=').map_or(pair, |(k, _)| k);
            rustgis_engine::data::address::name_class(name, &names) != rustgis_engine::data::address::NameClass::Plain
        });
        address(w) && (w.contains('@') || mentions_secret(w) || secret_pair)
    };
    if t.split_whitespace().any(carries) {
        let mut out = String::with_capacity(s.len());
        let mut word = String::new();
        let flush = |word: &mut String, out: &mut String| {
            if !word.is_empty() {
                out.push_str(&if carries(word) { rustgis_engine::sources::redact(word) } else { word.clone() });
                word.clear();
            }
        };
        for c in s.chars() {
            if c.is_whitespace() {
                flush(&mut word, &mut out);
                out.push(c);
            } else {
                word.push(c);
            }
        }
        flush(&mut word, &mut out);
        return out;
    }
    s.to_string()
}

/// Whether free text names a secret. Narrower than the structured-name list: a plain "key" in an error
/// ("primary key") isn't one.
fn mentions_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["password", "passwd", "pwd", "secret", "token", "apikey", "api_key", "api key", "authorization", "bearer", "credential", "cookie"]
        .iter()
        .any(|w| lower.contains(w))
}

/// A script (`run`'s, one `command-id {json}` per line) with each line's parameters redacted as
/// a call's are, line by line, so one line that doesn't parse leaves the others redacted. A line
/// whose parameters aren't JSON keeps only its command id.
fn redact_script(script: &str) -> String {
    let omitted = script.lines().skip(rustgis_engine::cmd::workflows::MAX_SCRIPT_LINES).count();
    let output = script
        .lines()
        .take(rustgis_engine::cmd::workflows::MAX_SCRIPT_LINES)
        .map(|line| {
            let line = line.trim();
            // Display only: a malformed identifier may carry JSON glued after `=`. It must
            // not be copied as an id; execution still uses the shared parser unchanged.
            let head = line.split(|c: char| c.is_whitespace() || c == '=').next().unwrap_or(line);
            let glued = line
                .get(head.len()..)
                .and_then(|r| r.strip_prefix('='))
                .is_some_and(|r| !r.is_empty() && !r.starts_with(char::is_whitespace));
            match rustgis_engine::script_line(line) {
                None => redact_string(line),
                Some(Ok((id, params))) if !glued => {
                    let id = rustgis_engine::log_identifier(&id);
                    if params.is_null() { id } else { format!("{id} {}", rustgis_engine::log_params(&params).into_log_value()) }
                }
                Some(_) => format!("{} [parameters not JSON; withheld]", rustgis_engine::log_identifier(head)),
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if omitted > 0 { format!("# [{omitted} further script line(s) not recorded]\n{output}") } else { output }
}

/// `v` without what would give someone else the user's access: values under keys that name a
/// secret, the passwords, keys and tokens in addresses ([`rustgis_engine::sources::redact`]),
/// and the same inside a script's lines.
fn redact(v: Value, depth: usize) -> Value {
    match v {
        // A long value (inline GeoJSON) is cut before it's searched, not after.
        Value::String(s) if s.len() > MAX_ARGS => Value::String(redact_string(&crate::tools::clip(&s, MAX_ARGS))),
        Value::String(s) => Value::String(redact_string(&s)),
        Value::Array(a) if depth < 16 => Value::Array(a.into_iter().map(|x| redact(x, depth + 1)).collect()),
        Value::Object(o) if depth < 16 => Value::Object(
            o.into_iter()
                .map(|(k, x)| match (rustgis_engine::data::address::log_sensitive(&k), k.as_str(), x) {
                    (true, _, _) => (k, json!("[redacted]")),
                    (false, "script", Value::String(s)) => (k, json!(redact_script(&s))),
                    (false, _, x) => (k, redact(x, depth + 1)),
                })
                .collect(),
        ),
        Value::Array(_) | Value::Object(_) => json!("[too deep]"),
        other => other,
    }
}

/// Whether `name` is a day file of the log: `YYYY-MM-DD.jsonl`, a real month and day.
fn is_day_file(name: &str) -> bool {
    let Some(day) = name.strip_suffix(".jsonl") else { return false };
    let b = day.as_bytes();
    let digits = |r: std::ops::Range<usize>| b.get(r).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
    let num = |r: std::ops::Range<usize>| day.get(r).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    b.len() == 10
        && digits(0..4)
        && b.get(4) == Some(&b'-')
        && digits(5..7)
        && b.get(7) == Some(&b'-')
        && digits(8..10)
        && (1..=12).contains(&num(5..7))
        && (1..=31).contains(&num(8..10))
}

/// Removes day files in `dir` older than [`KEEP_DAYS`] before `secs`.
fn prune(dir: &Path, secs: i64) {
    let oldest = file_for(dir, secs - i64::try_from(KEEP_DAYS * 86_400).unwrap_or(0));
    let Some(oldest) = oldest.file_name().and_then(|n| n.to_str()).map(str::to_string) else { return };
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for e in read.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        // Only our own day files: `YYYY-MM-DD.jsonl`, compared as text.
        if is_day_file(&name) && name < oldest {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_logs_the_executed_envelope_and_shared_script_grammar() {
        for line in [
            "sql.connect={\"password\":\"RGSYNTH_glued_audit\"}",
            "sql.connect={\"password\":\"RGSYNTH_glued_audit\"} junk",
            "sql.connect={\"password\":\"RGSYNTH_glued_audit\"",
        ] {
            let output = redact_script(line);
            assert!(!output.contains("RGSYNTH_glued_audit"), "{output}");
        }
        for id in [
            "sql.connect={\"password\":\"RGSYNTH_glued_audit\"}",
            "sql.connect {\"password\":\"RGSYNTH_glued_audit\"}",
            "sql.connect:{\"password\":\"RGSYNTH_glued_audit\"}",
            "sql.connect:password:RGSYNTH_glued_audit",
            "sql.connect:token:RGSYNTH_glued_audit",
            "password:RGSYNTH_glued_audit",
        ] {
            for tool in ["run_command", "run_tool"] {
                let key = if tool == "run_command" { "command" } else { "tool" };
                let mut args = json!({"params":null});
                args[key] = json!(id);
                let logged = normalized_args(tool, &args);
                assert!(!logged.to_string().contains("RGSYNTH_glued_audit"), "{tool}: {logged}");
            }
        }
        let params =
            json!({"url": "https://h.invalid/a?%74oken=RGSYNTH_audit_20261103", "pass%77ord": "RGSYNTH_option_20261103", "layer": "roads"});
        for tool in ["run_command", "run_tool"] {
            let object = json!({"command": "layer.addUrl", "tool": "buffer", "params": params});
            let mut string = object.clone();
            string["params"] = json!(params.to_string());
            let logged = normalized_args(tool, &object);
            assert_eq!(logged, normalized_args(tool, &string));
            assert_eq!(logged["params"], rustgis_engine::log_params(&params).into_log_value());
            assert!(!logged.to_string().contains("RGSYNTH_"));
            for bad in [json!("{broken RGSYNTH_bad_20261103"), json!("[1]"), json!(42)] {
                let mut bad_args = object.clone();
                bad_args["params"] = bad;
                assert_eq!(normalized_args(tool, &bad_args)["params"], "[parameters not JSON; withheld]");
            }
        }
        assert!(!redact_string("failed https://h.invalid/a?%74oken=RGSYNTH_error_20261103").contains("RGSYNTH_"));
        let script = "map.zoomIn= {}\nlayer.addUrl {broken RGSYNTH_bad_20261103\nmap.zoomOut {}";
        let logged = redact_script(script);
        assert!(logged.starts_with("map.zoomIn {}\nlayer.addUrl [parameters not JSON; withheld]\nmap.zoomOut {}"));
        assert!(!logged.contains("RGSYNTH_"));
        let comments = "# clean comment\n# https://h.invalid/a?token=RGSYNTH_comment_20261106";
        let args = normalized_args("run", &json!({"script": comments}));
        assert!(!args.to_string().contains("RGSYNTH_comment"));
        assert!(!redact_script(comments).contains("RGSYNTH_comment"));
        assert!(redact_script(comments).starts_with("# clean comment\n"));
        for url in
            ["https://h.invalid/data", "pg:host=h dbname=gis", "mariadb://h/gis", "pmtiles:https://h.invalid/a", "cog:https://h.invalid/a"]
        {
            let nested = json!({"key":"abc", "source":{"url":url}});
            assert_eq!(normalized_args("run_command", &json!({"command":"plugin.unknown", "params":nested}))["params"]["key"], "", "{url}");
        }
        let document =
            json!({"type": "FeatureCollection", "features": [], "padding": "x".repeat(250), "token": "RGSYNTH_data_20261103"}).to_string();
        let arguments = json!({"command": "layer.addDataText", "params": {"text": document}});
        let logged = normalized_args("run_command", &arguments);
        assert!(logged["params"]["text"].as_str().is_some_and(|s| s.starts_with("(a document of ")));
        assert_eq!(arguments["params"]["text"], document, "the executed data document stays intact");
    }

    /// A line for each tool call: what ran, whether it worked, how long it took; secrets in its
    /// arguments are not written.
    #[test]
    fn entries_describe_each_call_without_secrets() {
        let request = json!([
            {"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "run_command", "arguments": {"command": "layer.addData", "params": {"path": "postgresql://ann:hunter2@db.example.com/gis", "token": "abc"}}}},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "run", "arguments": {"script": "map.zoomIn"}}},
            {"jsonrpc": "2.0", "id": 3, "method": "ping"}
        ]);
        let reply = json!([
            {"jsonrpc": "2.0", "id": 1, "result": {"isError": true, "content": [{"type": "text", "text": "could not connect"}]}},
            {"jsonrpc": "2.0", "id": 2, "result": {"isError": false, "content": [{"type": "text", "text": "{}"}]}},
            {"jsonrpc": "2.0", "id": 3, "result": {}}
        ]);
        let lines = entries("mcp", &request.to_string(), Some(&reply.to_string()), Duration::from_millis(1234), 1_792_000_000);
        assert_eq!(lines.len(), 2, "{lines:?}");
        let (a, b) = (&lines[0], &lines[1]);
        assert_eq!(
            (a["tool"].clone(), a["command"].clone(), a["success"].clone()),
            (json!("run_command"), json!("layer.addData"), json!(false))
        );
        assert_eq!(a["error"], "could not connect");
        assert_eq!((a["source"].clone(), a["duration_s"].clone()), (json!("mcp"), json!(1.234)));
        assert!(a["ts"].as_str().is_some_and(|t| t.ends_with('Z')));
        let args = a["args"].to_string();
        assert!(!args.contains("hunter2") && !args.contains("abc") && args.contains("db.example.com"), "{args}");
        assert_eq!((b["script"].clone(), b["success"].clone(), b["error"].clone()), (json!("map.zoomIn"), json!(true), Value::Null));
        // No reply (the connection gave up): failed.
        let lone = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "list_layers"}});
        let lines = entries("file", &lone.to_string(), None, Duration::ZERO, 0);
        assert_eq!((lines[0]["success"].clone(), lines[0]["error"].clone()), (json!(false), json!("no reply")));
        // Not JSON: nothing to log.
        assert!(entries("mcp", "not json", None, Duration::ZERO, 0).is_empty());
        // A script's lines are redacted like a call's parameters, and a batch says so.
        assert_eq!(a["batch"], true);
        let script = json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "run", "arguments": {
            "script": "layer.addData {\"path\": \"postgresql://ann:hunter2@db.example.com/gis\"}\nsql.connect {\"password\": \"s3cret\"}"}}});
        let lines = entries("mcp", &script.to_string(), None, Duration::ZERO, 0);
        let text = lines[0].to_string();
        assert!(!text.contains("hunter2") && !text.contains("s3cret") && text.contains("db.example.com"), "{text}");
        assert!(lines[0].get("batch").is_none());
        // Keys by their words: secrets, not every key.
        for k in [
            "password",
            "api_key",
            "accessKey",
            "accessKeyId",
            "access_key_id",
            "secretAccessKey",
            "sessionToken",
            "tokens",
            "authToken",
            "client_secret",
            "key",
            "DSN",
            "accesstoken",
            "clientsecret",
            "passphrase",
            "jwt",
        ] {
            assert!(rustgis_engine::data::address::log_sensitive(k), "{k}");
        }
        for k in ["primary_key", "sort_key", "keys_column", "texas", "name"] {
            assert!(!rustgis_engine::data::address::log_sensitive(k), "{k}");
        }
        // Free text keeps its shape; a keyword connection string loses its password.
        assert_eq!(redact_string("SELECT * FROM t WHERE url = 'http://x?a=1#b'"), "SELECT * FROM t WHERE url = 'http://x?a=1#b'");
        assert!(!redact_string("host=db user=ann password=hunter2").contains("hunter2"));
        for quoted in ["host=db password='my secret'", "password = x y", "pwd: hunter2"] {
            let out = redact_string(quoted);
            assert!(!out.contains("secret'") && !out.contains("hunter2") && !out.contains(" y"), "{quoted}: {out}");
        }
        // An address inside longer text loses its password; the rest stays.
        let out = redact_string("COPY FROM postgresql://ann:hunter2@db.example.com/gis WITH x");
        assert!(!out.contains("hunter2") && out.starts_with("COPY FROM ") && out.ends_with(" WITH x"), "{out}");
        // One line that isn't JSON doesn't leave the others' secrets in the log.
        let broken = json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "run", "arguments": {
            "script": "sql.connect {\"password\": \"hunter2\"}\nmap.zoomIn {oops token=abc"}}});
        let text = entries("mcp", &broken.to_string(), None, Duration::ZERO, 0)[0].to_string();
        assert!(!text.contains("hunter2") && !text.contains("abc") && text.contains("map.zoomIn"), "{text}");
        // An error that names a credential in free text is withheld.
        let call = json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "list_layers"}});
        let err = json!({"jsonrpc": "2.0", "id": 6, "result": {"isError": true, "content": [{"type": "text", "text": "authentication failed: token abc123"}]}});
        let lines = entries("mcp", &call.to_string(), Some(&err.to_string()), Duration::ZERO, 0);
        assert!(!lines[0]["error"].as_str().unwrap_or("").contains("abc123"), "{}", lines[0]);
    }

    /// Lines go to the day's file; old days are removed, other files are left.
    #[test]
    fn records_append_to_the_days_file_and_old_days_go() {
        let dir = tempfile::tempdir().unwrap();
        let old = file_for(dir.path(), now() - 40 * 86_400);
        let recent = file_for(dir.path(), now() - 2 * 86_400);
        std::fs::write(&old, "{}\n").unwrap();
        std::fs::write(&recent, "{}\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "mine").unwrap();
        // Not a day file, though its first ten characters are digits.
        std::fs::write(dir.path().join("0000000000.jsonl"), "{}\n").unwrap();
        let call =
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "list_layers", "arguments": {}}}).to_string();
        let ok = json!({"jsonrpc": "2.0", "id": 1, "result": {"isError": false, "content": []}}).to_string();
        record(dir.path(), "mcp", &call, Some(&ok), Duration::from_millis(5));
        record(dir.path(), "mcp", &call, Some(&ok), Duration::from_millis(5));
        let today = std::fs::read_to_string(file_for(dir.path(), now())).unwrap();
        assert_eq!(today.lines().count(), 2, "{today}");
        assert!(today.lines().all(|l| serde_json::from_str::<Value>(l).is_ok_and(|v| v["tool"] == "list_layers")));
        assert!(!old.exists(), "a day past the limit is removed");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(file_for(dir.path(), now())).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "the log is this user's alone: {mode:o}");
        }
        assert!(recent.exists() && dir.path().join("notes.txt").exists());
        assert!(dir.path().join("0000000000.jsonl").exists(), "only day files are pruned");
        assert!(is_day_file("2026-10-18.jsonl") && !is_day_file("2026-13-01.jsonl") && !is_day_file("2026-10-18.json"));
    }

    /// The bounded audit copy labels omitted lines before the display's byte clipping.
    #[test]
    fn audit_script_line_limit_counts_unrecorded_lines() {
        let limit = rustgis_engine::cmd::workflows::MAX_SCRIPT_LINES;
        let clean = "# safe\n".repeat(limit);
        assert!(!redact_script(&clean).contains("not recorded"));
        let output = redact_script(&(clean + "layer.rename {\"layer\":\"RGSYNTH_omitted_line\"}"));
        assert!(output.starts_with("# [1 further script line(s) not recorded]\n"));
        assert!(!output.contains("RGSYNTH_omitted_line"));
    }
}
