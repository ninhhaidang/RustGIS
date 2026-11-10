//! The tools the model calls, and what they do to the session.
//!
//! Every RustGIS action is a command, so a handful of tools reach all of them: the model finds a
//! command or geoprocessing tool by searching, then runs it by id. Runs go through
//! [`Session::run`], so each is one undo step and is restored if it fails. Commands that reach
//! beyond the project (files, the network, the user's own automation) need the user's approval
//! first ([`gate`]).

use rustgis_engine::policy::{Call, Outcome, Permit, Scope};
use rustgis_engine::{Session, analysis, project};
use serde_json::{Value, json};

use crate::provider::{ToolCall, ToolDef};

/// Default per-tool token estimate, matching Codex's default output budget.
pub const DEFAULT_OUTPUT_TOKENS: usize = 10_000;
/// Absolute per-tool ceiling, even when the context window is very large.
pub const MAX_OUTPUT_TOKENS: usize = 24_000;
/// Default byte budget: like Codex, estimate one token per four UTF-8 bytes.
pub const MAX_RESULT: usize = DEFAULT_OUTPUT_TOKENS * 4;

/// Convert a requested token estimate to the shared, clamped serialized byte budget.
pub fn output_bytes(tokens: usize) -> usize {
    tokens.clamp(1, MAX_OUTPUT_TOKENS) * 4
}

/// Shared tool definitions for the in-app assistant and MCP clients.
pub fn defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "list_layers",
            description: "Everything about the open project in one call: its path and unsaved changes, the view (centre, lon/lat extent, scale, projection), the active layer, its layers in table-of-contents order, top first (id, name, group, type, source, lon/lat extent, feature count, selection, definition query, renderer, labels, fields, visibility), its web tile layers (XYZ, WMS, vector tiles; index 0 on top, each with the layer it's drawn over, changed with map.setTiles), basemap, tables and layouts with their map frames.",
            schema: json!({
                "type": "object",
                "properties": {
                    "name_filter": {"type": "string", "description": "Only the layers and tables whose name holds this (ignoring case)"},
                    "include_fields": {"type": "boolean", "description": "List each layer's fields (default true); false keeps a large project short"}
                }
            }),
        },
        ToolDef {
            name: "search_commands",
            description: "Search RustGIS commands (map navigation, layers, symbology, labels, selection, editing, layouts, export…) by words. Returns each command's id, label, ribbon location and parameters.",
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Words to look for, e.g. \"basemap\", \"label field\", \"zoom layer\""},
                    "limit": {"type": "integer", "description": "At most this many (default 12)"}
                },
                "required": ["query"]
            }),
        },
        ToolDef {
            name: "run_command",
            description: "Run a RustGIS command by id with JSON parameters (see search_commands for ids and parameters). Each run is one undo step. Commands that read or write files, use the network or replace the project ask the user first.",
            schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Command id, e.g. map.goToXY"},
                    "params": {"type": "object", "description": "The command's parameters"}
                },
                "required": ["command"]
            }),
        },
        ToolDef {
            name: "search_tools",
            description: "Search geoprocessing tools (buffer, clip, dissolve, spatial join, statistics, raster analysis…) by words. Returns each tool's id, name, toolbox and summary.",
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Words to look for"},
                    "limit": {"type": "integer", "description": "At most this many (default 12)"}
                },
                "required": ["query"]
            }),
        },
        ToolDef {
            name: "describe_tool",
            description: "A geoprocessing tool's parameters: name, kind, whether required, default and help.",
            schema: json!({
                "type": "object",
                "properties": {"tool": {"type": "string", "description": "Tool id"}},
                "required": ["tool"]
            }),
        },
        ToolDef {
            name: "run_tool",
            description: "Run a geoprocessing tool. Its output is added to the map as a new layer, or to the project as a standalone table (for example Summary Statistics), whose name the result gives (use it as the input of a next step; read a table with read_features). One undo step.",
            schema: json!({
                "type": "object",
                "properties": {
                    "tool": {"type": "string", "description": "Tool id"},
                    "params": {"type": "object", "description": "The tool's parameters by name; layers by name"}
                },
                "required": ["tool"]
            }),
        },
        ToolDef {
            name: "read_features",
            description: "Read a vector layer's or a standalone table's attribute rows, optionally only those matching a where clause (SQL-like: pop_max > 1000000 AND continent = 'Asia'). Give layer or table. Returns the number matching and up to `limit` rows, each with its row number (_row; a layer's for select.rows) and a layer's location (_lon, _lat).",
            schema: json!({
                "type": "object",
                "properties": {
                    "layer": {"type": "string", "description": "Layer name or id (or a standalone table's name, when no layer has it)"},
                    "table": {"type": "string", "description": "A standalone table's name or id (an id as text, \"7\"), such as a tool's output table (read instead of layer)"},
                    "where": {"type": "string", "description": "A where clause; empty for all rows"},
                    "fields": {"type": "array", "items": {"type": "string"}, "description": "Only these fields (default all)"},
                    "order_by": {"type": "string", "description": "Sort by this field"},
                    "descending": {"type": "boolean"},
                    "selected": {"type": "boolean", "description": "Only the selected rows"},
                    "limit": {"type": "integer", "description": "At most this many rows (default 20, at most 200)"}
                }
            }),
        },
        ToolDef {
            name: "search_docs",
            description: "Search the RustGIS documentation (user guide, command line, plugins, installation…) by words. Returns the best matching sections: page, section, address and a snippet; read_doc reads one.",
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Words to look for, e.g. \"graduated symbols\", \"vector tiles\""},
                    "limit": {"type": "integer", "description": "At most this many (default 8)"}
                },
                "required": ["query"]
            }),
        },
        ToolDef {
            name: "read_doc",
            description: "Read a RustGIS documentation page as Markdown, or one section of it (page paths come from search_docs). Long pages read best one section at a time. Results are budgeted; continue at next_offset when present.",
            schema: json!({
                "type": "object",
                "properties": {
                    "page": {"type": "string", "description": "Page path, e.g. user-guide/cartography.md"},
                    "section": {"type": "string", "description": "A section heading (default the whole page)"},
                    "offset": {"type": "integer", "minimum": 1, "description": "First line, 1-based (default 1; use next_offset to continue)"},
                    "limit": {"type": "integer", "description": "Maximum lines (default 2000)"}
                },
                "required": ["page"]
            }),
        },
    ]
}

/// The command a call runs and its parameters (`gp.run` for a tool), if it runs one.
/// Use execution's strict envelope parser for policy too: malformed input has no runnable target.
pub fn target(call: &ToolCall) -> Option<(String, Value)> {
    match call.name.as_str() {
        "run_command" => Some((str_arg(&call.args, "command").unwrap_or_default(), params_arg(&call.args).ok()?)),
        "run_tool" => Some((
            "gp.run".to_string(),
            json!({"tool": str_arg(&call.args, "tool").unwrap_or_default(), "params": params_arg(&call.args).ok()?}),
        )),
        _ => None,
    }
}

/// Whether a successfully executed tool explicitly chose a layer, even if it was already active.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn selects_layer(name: &str, args: &Value) -> bool {
    match name {
        "run_command" => args.get("command").and_then(Value::as_str).map(str::trim) == Some("layer.select"),
        "run" => args.get("script").and_then(Value::as_str).is_some_and(|script| {
            rustgis_engine::cmd::workflows::script_commands(script).is_ok_and(|lines| lines.iter().any(|(_, id, _)| id == "layer.select"))
        }),
        _ => false,
    }
}

/// The executor's strict envelope is the only input to engine planning.
pub fn policy_call(call: &ToolCall) -> Result<Option<Call>, String> {
    let c = match call.name.as_str() {
        "run_command" => Some(Call::command(&str_arg(&call.args, "command").ok_or("command is required")?, params_arg(&call.args)?)),
        "run_tool" => Some(Call::tool(&str_arg(&call.args, "tool").ok_or("tool is required")?, params_arg(&call.args)?)),
        "run" => Some(Call::script(call.args.get("script").and_then(Value::as_str).ok_or("script is required")?)),
        "snapshot" | "view_map" => {
            let files = call.name == "snapshot" && call.args.get("save_path").and_then(Value::as_str).is_some_and(|p| !p.trim().is_empty());
            let effects = if files {
                rustgis_engine::policy::Effects::NONE.with(rustgis_engine::policy::Effect::Files)
            } else {
                rustgis_engine::policy::Effects::NONE
            };
            Some(Call::adapter(if call.name == "snapshot" { "snapshot" } else { "view_map" }, effects, call.args.clone()))
        }
        _ => None,
    };
    Ok(c.map(|c| c.with_request(&call.id)))
}
/// A mapping of the engine outcome for callers that display the reason only.
pub fn gate(s: &Session, call: &ToolCall) -> Option<String> {
    let call = match policy_call(call) {
        Ok(Some(call)) => call,
        Ok(None) => return None,
        Err(error) => return Some(error),
    };
    let scope = Scope::assistant(0, std::sync::Arc::new(Live::default()));
    match s.review(&scope, &call) {
        Outcome::Allowed => None,
        Outcome::NeedsApproval(p) => Some(p.review().why.clone()),
        Outcome::Refused(e) => Some(e),
    }
}
/// A revocable assistant lifetime. A stopped lifetime is never re-enabled.
pub(crate) struct Live(pub(crate) std::sync::atomic::AtomicBool);
impl Default for Live {
    fn default() -> Self {
        Self(std::sync::atomic::AtomicBool::new(true))
    }
}
impl rustgis_engine::policy::Revocation for Live {
    fn allows(&self, _: rustgis_engine::policy::Effects) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}
/// A one-line description of a call for the transcript and the approval prompt.
pub fn summary(call: &ToolCall) -> String {
    let short = |v: Value| {
        let t = v.to_string();
        if t.chars().count() > 300 { format!("{}…", t.chars().take(300).collect::<String>()) } else { t }
    };
    match call.name.as_str() {
        "run_command" => format!("{} {}", str_arg(&call.args, "command").unwrap_or_default(), short(obj_arg(&call.args, "params"))),
        "run_tool" => format!(
            "gp.run {}",
            short(json!({"tool": str_arg(&call.args, "tool").unwrap_or_default(), "params": obj_arg(&call.args, "params")}))
        ),
        "delegate" => format!("Delegated: {}", str_arg(&call.args, "task").unwrap_or_default().chars().take(300).collect::<String>()),
        other => format!("{other} {}", short(call.args.clone())),
    }
}

/// Runs a call; `Err` is a result the model is told failed.
pub fn run(s: &mut Session, call: &ToolCall, scope: Scope, permit: Option<Permit>) -> Result<String, String> {
    run_with_budget(s, call, DEFAULT_OUTPUT_TOKENS, scope, permit)
}

/// Runs a call with a per-result token estimate, capped by the absolute ceiling.
pub fn run_with_budget(s: &mut Session, call: &ToolCall, tokens: usize, scope: Scope, permit: Option<Permit>) -> Result<String, String> {
    // Script failures are structured JSON: clean their error field before serialization.
    if call.name == "run" {
        return run_script(s, call, tokens, scope, permit);
    }
    run_with_budget_inner(s, call, tokens, scope, permit).map_err(|e| public_error(&e))
}

/// Adapter-owned presentation state for one already-admitted archive command. Execution
/// authority stays in the engine's ReadAhead/Read, never in this result formatter.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct CommandCompletion {
    before: Vec<(String, Vec<u32>)>,
    mark: u64,
}
#[cfg(not(target_arch = "wasm32"))]
impl CommandCompletion {
    pub(crate) fn capture(s: &Session) -> Self {
        Self { before: layer_snap(s), mark: s.notice_mark() }
    }
    pub(crate) fn finish(self, s: &mut Session, result: Result<Value, String>) -> Result<String, String> {
        result
            .map(|value| command_result(s, value, &self.before, self.mark, DEFAULT_OUTPUT_TOKENS))
            .map_err(|e| public_error(&error_with_notices(e, s, self.mark)))
    }
}

/// Retains the initiating scope and one-use permit while assembling ordinary tool results.
fn run_with_budget_inner(s: &mut Session, call: &ToolCall, tokens: usize, scope: Scope, permit: Option<Permit>) -> Result<String, String> {
    if matches!(call.name.as_str(), "run_command" | "run_tool") {
        if call.name == "run_command" && str_arg(&call.args, "command").is_some_and(|id| id.starts_with("ui.")) {
            return Err("ui.* commands only open panes and dialogs; the assistant can't use them".into());
        }
        let engine = policy_call(call)?.ok_or("missing executable call")?;
        let (before, mark) = (layer_snap(s), s.notice_mark());
        let result = match permit {
            Some(p) => s.run_permitted(scope, p, &engine),
            None => s.run_scoped(scope, &engine),
        }
        .map_err(|e| error_with_notices(e, s, mark))?;
        return Ok(command_result(s, result, &before, mark, tokens));
    }
    if permit.is_some() {
        return Err("this approval is for an executable call".into());
    }
    s.with_scope(scope, |s| run_read(s, call, tokens))?
}

fn run_read(s: &mut Session, call: &ToolCall, tokens: usize) -> Result<String, String> {
    let max_bytes = output_bytes(tokens);
    let a = &call.args;
    let v = match call.name.as_str() {
        "list_layers" => s.run(
            "info.project",
            &json!({"nameFilter": str_arg(a, "name_filter"), "includeFields": a.get("include_fields").and_then(Value::as_bool).unwrap_or(true)}),
        )?,
        "search_commands" => search_commands(s, a)?,
        "search_tools" => search_tools(a)?,
        "describe_tool" => describe_tool(a)?,
        "read_features" => read_features(s, a)?,
        "search_docs" => crate::docs::search(&str_arg(a, "query").unwrap_or_default(), limit_arg(a, 8, 30))?,
        "read_doc" => {
            return crate::docs::read_with_budget(
                &str_arg(a, "page").unwrap_or_default(),
                str_arg(a, "section").as_deref(),
                match a.get("offset") {
                    None => 1,
                    Some(v) => v.as_u64().filter(|n| *n > 0).ok_or("offset must be a positive integer (1-based line)")?,
                },
                limit_arg(a, 2000, 2000),
                max_bytes,
            )
            .map(|v| v.to_string());
        }
        // Fails the way a bug in RustGIS would, for the agent's panic guard.
        #[cfg(test)]
        #[allow(clippy::panic)]
        "test_panic" => panic!("a test tool panicked"),
        other => return Err(format!("unknown tool '{other}'")),
    };
    if call.name == "read_features" {
        Ok(public_envelope(s, v, tokens))
    } else {
        let mut withheld = Withheld::default();
        let v = public_metadata(v, 0, &mut withheld);
        Ok(finish_result(s, v, tokens, withheld))
    }
}

/// How deep a result is searched for runtime login addresses; what is deeper is withheld.
const MAX_DEPTH: usize = 16;

/// `text` with every runtime login wrapper (`rustgis-login:…`, alone or after a prefix such as
/// `pmtiles:…@`) replaced by its plain address. Commands return the bound addresses the app needs
/// to use a connection's login; the model and MCP clients get only the public ones. Active
/// raw secrets are scrubbed too, including an echo with no wrapper in the result.
fn public_text(text: String) -> String {
    rustgis_engine::render::rustgis_tiles::logins::scrub(&text)
}

/// Generated errors may quote arbitrary addresses; successful values remain data.
fn public_error(text: &str) -> String {
    rustgis_engine::render::rustgis_tiles::logins::scrub_error(text)
}

/// Publishes a completed result, reporting withheld subtrees before enforcing the response budget.
#[cfg(test)]
fn public_result(s: &mut Session, value: Value, tokens: usize) -> String {
    let mut withheld = Withheld::default();
    let value = public_addresses(value, 0, &mut withheld);
    finish_result(s, value, tokens, withheld)
}

/// These fields belong to the adapter's response schema; only their values contain data.
fn public_envelope(s: &mut Session, value: Value, tokens: usize) -> String {
    let mut withheld = Withheld::default();
    let value = match value {
        Value::Object(o) => Value::Object(o.into_iter().map(|(key, value)| (key, public_addresses(value, 1, &mut withheld))).collect()),
        value => public_addresses(value, 0, &mut withheld),
    };
    finish_result(s, value, tokens, withheld)
}

/// Generated search, tool and project metadata has a fixed schema at every level. Its
/// strings may hold private data, but its keys cannot be login values. Row/property maps
/// and arbitrary command results use `public_addresses` instead.
fn public_metadata(value: Value, depth: usize, withheld: &mut Withheld) -> Value {
    match value {
        Value::String(text) => Value::String(public_text(text)),
        Value::Array(a) if depth < MAX_DEPTH => Value::Array(a.into_iter().map(|v| public_metadata(v, depth + 1, withheld)).collect()),
        Value::Object(o) if depth < MAX_DEPTH => {
            Value::Object(o.into_iter().map(|(key, v)| (key, public_metadata(v, depth + 1, withheld))).collect())
        }
        Value::Array(_) | Value::Object(_) => {
            withheld.depth = withheld.depth.saturating_add(1);
            json!("[too deep]")
        }
        other => other,
    }
}

/// Cleans the arbitrary command payload before adding adapter-owned notices/layer metadata.
fn command_result(s: &mut Session, value: Value, before: &[(String, Vec<u32>)], mark: u64, tokens: usize) -> String {
    let mut withheld = Withheld::default();
    let value = public_addresses(value, 0, &mut withheld);
    let value = with_notices(with_new_layers(value, s, before), s, mark);
    finish_result(s, value, tokens, withheld)
}

/// Publishes counted projection losses without sending the completed envelope through a second redactor.
fn finish_result(s: &mut Session, mut value: Value, tokens: usize, withheld: Withheld) -> String {
    if withheld.depth > 0 || withheld.keys > 0 {
        let mark = s.notice_mark();
        if withheld.depth > 0 {
            s.notify(format!("Tool result: {} subtrees at nesting depth {MAX_DEPTH} were withheld.", withheld.depth), false);
        }
        if withheld.keys > 0 {
            s.notify(format!("Tool result: {} object(s) were withheld because redacted field names coincide.", withheld.keys), false);
        }
        if let Some(notices) = value.get_mut("notices").and_then(Value::as_array_mut) {
            notices.extend(s.notices_since(mark).iter().map(|n| json!(n.text)));
        } else {
            value = with_notices(value, s, mark);
        }
    }
    bounded_json(&value, tokens)
}

/// Counts output withheld for depth limits or field names that collide after redaction.
#[derive(Default)]
struct Withheld {
    depth: usize,
    keys: usize,
}

/// `v` with string values and field names made public ([`public_text`]), to a depth of
/// [`MAX_DEPTH`]. An object whose public field names collide is withheld as a whole.
fn public_addresses(v: Value, depth: usize, withheld: &mut Withheld) -> Value {
    match v {
        Value::String(s) => Value::String(public_text(s)),
        Value::Array(a) if depth < MAX_DEPTH => Value::Array(a.into_iter().map(|x| public_addresses(x, depth + 1, withheld)).collect()),
        Value::Object(o) if depth < MAX_DEPTH => {
            let mut public = serde_json::Map::new();
            for (key, value) in o {
                let key = public_text(key);
                if public.contains_key(&key) {
                    withheld.keys = withheld.keys.saturating_add(1);
                    return json!("[redacted field names coincide]");
                }
                public.insert(key, value);
            }
            Value::Object(public.into_iter().map(|(key, value)| (key, public_addresses(value, depth + 1, withheld))).collect())
        }
        Value::Array(_) | Value::Object(_) => {
            withheld.depth = withheld.depth.saturating_add(1);
            json!("[too deep]")
        }
        other => other,
    }
}

/// Serialize complete JSON. Oversized results become an explicit text preview envelope;
/// the excerpt is a JSON string, so quotes, escapes and UTF-8 can never break the response.
pub fn bounded_json(value: &Value, tokens: usize) -> String {
    let text = value.to_string();
    let max = output_bytes(tokens);
    if text.len() <= max {
        return text;
    }
    // Preserve the error reason before spending a small budget on preview metadata.
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return bounded_string(&format!("error: {error}"), max);
    }
    let chars: Vec<char> = text.chars().collect();
    let envelope = |keep: usize| {
        let head: String = chars.iter().take(keep / 2).collect();
        let tail: String = chars.iter().skip(chars.len().saturating_sub(keep - keep / 2)).collect();
        json!({"truncated": true, "original_bytes": text.len(),
            "output": format!("{head}\n… output truncated; request fewer rows or a narrower query …\n{tail}")})
        .to_string()
    };
    let (mut low, mut high) = (0, chars.len());
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if envelope(mid).len() <= max {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    let result = envelope(low);
    if result.len() <= max {
        return result;
    }
    // A tiny budget cannot hold the normal wrapper. Keep minimal truncation metadata,
    // or an abbreviated marker when even that cannot fit.
    let minimal = json!({"truncated": true}).to_string();
    if minimal.len() <= max { minimal } else { bounded_string("truncated", max) }
}

/// Fit a diagnostic prefix inside a JSON string, including quotes and escaping.
fn bounded_string(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let encoded = |keep: usize| json!(chars.iter().take(keep).collect::<String>()).to_string();
    let (mut low, mut high) = (0, chars.len());
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if encoded(mid).len() <= max {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    encoded(low)
}

/// `text`, cut to `max` characters with a note when longer.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}… (cut: {} characters in all)", text.chars().take(max).collect::<String>(), text.chars().count())
}

fn str_arg(a: &Value, k: &str) -> Option<String> {
    match a.get(k)? {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The `params` a command or tool runs with: an object, or JSON text of one (some models send
/// that). Anything else is an error: running with defaults instead could act on the wrong layer.
fn params_arg(a: &Value) -> Result<Value, String> {
    rustgis_engine::params_envelope(a.get("params"))
}

/// An object parameter, which some models send as JSON text (leniently, for display only).
fn obj_arg(a: &Value, k: &str) -> Value {
    match a.get(k) {
        Some(Value::String(t)) if !t.trim().is_empty() => serde_json::from_str(t).unwrap_or(Value::Null),
        Some(v @ Value::Object(_)) => v.clone(),
        _ => json!({}),
    }
}

fn limit_arg(a: &Value, default: usize, max: usize) -> usize {
    a.get("limit").and_then(Value::as_u64).map_or(default, |n| usize::try_from(n).unwrap_or(max)).clamp(1, max)
}

/// Every map's name and layer ids, for spotting the layers a command actually added.
fn layer_snap(s: &Session) -> Vec<(String, Vec<u32>)> {
    s.project.maps.iter().map(|m| (m.name.clone(), m.layers.iter().map(|l| l.id).collect())).collect()
}

/// A run's result, with the layers it added named (the next step's inputs).
///
/// Layers are compared with the same map's layers before the run, so switching or deleting maps
/// does not report the newly visible map's layers as `layersAdded` (#212). Map names are unique, so
/// the map is found by name, or at the same place when only its name changed (a rename).
fn with_new_layers(r: Value, s: &Session, before: &[(String, Vec<u32>)]) -> Value {
    let m = s.map();
    let active = s.project.active_map;
    // A rename changes only the active map's name; every other map keeps its own.
    let renamed = before.len() == s.project.maps.len()
        && s.project.maps.iter().zip(before).enumerate().all(|(i, (now, (was, _)))| i == active || now.name == *was);
    let same =
        before.iter().find(|(name, _)| name.eq_ignore_ascii_case(&m.name)).or_else(|| if renamed { before.get(active) } else { None });
    let had = same.map_or(&[][..], |(_, ids)| ids.as_slice());
    let added: Vec<String> = m.layers.iter().filter(|l| !had.contains(&l.id)).map(|l| public_text(l.name.clone())).collect();
    if added.is_empty() {
        return if r.is_null() { json!({"ok": true}) } else { r };
    }
    json!({"result": r, "layersAdded": added})
}

/// Runs a script (`script.run`'s format: one `command-id {json}` per line, `#` comments) in one
/// call and one undo step. A failing line stops the script and nothing it did to the project is
/// kept (files it wrote stay). The answer is `{success, result, lines, layersAdded, notices,
/// error, duration_s}`, where `result`
/// is the last command's; a failure is an `Err` holding the same JSON.
fn run_script(s: &mut Session, call: &ToolCall, tokens: usize, scope: Scope, permit: Option<Permit>) -> Result<String, String> {
    let a = &call.args;
    // Untrimmed, so errors name the same lines the caller wrote.
    let script = a
        .get("script")
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .ok_or("script is required: one `command-id {json}` per line")?;
    let start = web_time::Instant::now();
    let (before, mark) = (layer_snap(s), s.notice_mark());
    let engine = Call::script(script).with_request(&call.id);
    let r = match permit {
        Some(p) => s.run_permitted(scope, p, &engine),
        None => s.run_scoped(scope, &engine),
    };
    let secs = (start.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
    let notices: Vec<&str> = s.notices_since(mark).iter().map(|n| n.text.as_str()).collect();
    match r {
        Ok(v) => {
            let ran = v.get("ran").and_then(Value::as_array);
            let last = ran.and_then(|r| r.last()).and_then(|l| l.get("result")).cloned().unwrap_or(Value::Null);
            let added = with_new_layers(json!({}), s, &before).get("layersAdded").cloned().unwrap_or_else(|| json!([]));
            let out = json!({"success": true, "result": last, "lines": ran.map_or(0, Vec::len), "layersAdded": added,
                "notices": notices, "error": null, "duration_s": secs});
            Ok(public_envelope(s, out, tokens))
        }
        Err(e) => {
            let e = public_error(&e);
            // Nothing was kept, so no line ran and no layer was added.
            // The notice of the failure is the error redacted: compared as such, it isn't repeated.
            let told = rustgis_engine::data::diagnostics::redact(&e);
            let said: Vec<&&str> = notices.iter().filter(|t| **t != e && **t != told).collect();
            let out = json!({"success": false, "result": null, "lines": 0, "layersAdded": [], "notices": said,
                "error": clip(&e, 8000), "duration_s": secs});
            Err(public_envelope(s, out, tokens))
        }
    }
}

/// A run's result with what it said beside it (a notice that only the first tables of a GeoPackage
/// were added, a file's warnings), which the status bar shows in the app.
fn with_notices(r: Value, s: &Session, mark: u64) -> Value {
    let said: Vec<String> = s.notices_since(mark).iter().map(|n| public_text(n.text.clone())).collect();
    if said.is_empty() {
        return r;
    }
    match r {
        Value::Object(mut o) if !o.contains_key("notices") => {
            o.insert("notices".into(), json!(said));
            Value::Object(o)
        }
        other => json!({"result": other, "notices": said}),
    }
}

/// A failed run's error with what the command said before failing (a GDAL layer that couldn't be
/// converted).
fn error_with_notices(e: String, s: &Session, mark: u64) -> String {
    let told = rustgis_engine::data::diagnostics::redact(&e);
    let said: Vec<&str> = s.notices_since(mark).iter().map(|n| n.text.as_str()).filter(|t| *t != e && *t != told).collect();
    // Preserve the original 8,000-character diagnostic assembly cap for MCP errors;
    // the agent additionally applies its serialized output budget.
    if said.is_empty() { e } else { clip(&format!("{e}\n{}", said.join("\n")), 8000) }
}

/// How well `words` match `hay` (all lower case): matched words, with whole-id hits counting more.
fn score(words: &[String], fields: &[&str]) -> usize {
    words
        .iter()
        .map(|w| {
            fields.iter().enumerate().map(|(i, f)| if f.contains(w.as_str()) { if i == 0 { 3 } else { 2 } } else { 0 }).max().unwrap_or(0)
        })
        .sum()
}

/// The query's words (plural `s` dropped), or an error when it has none.
fn words(a: &Value) -> Result<Vec<String>, String> {
    let words: Vec<String> = str_arg(a, "query")
        .unwrap_or_default()
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .map(|w| if w.len() > 3 { w.trim_end_matches('s') } else { w })
        .filter(|w| w.len() > 1)
        .map(str::to_string)
        .take(12)
        .collect();
    if words.is_empty() {
        return Err("query needs at least one word of two or more letters, e.g. \"basemap\"".into());
    }
    Ok(words)
}

fn search_commands(s: &Session, a: &Value) -> Result<Value, String> {
    let words = words(a)?;
    let limit = limit_arg(a, 12, 40);
    let mut hits: Vec<(usize, Value)> = s
        .registry
        .commands
        .iter()
        // Internal plumbing the model has no use for.
        .filter(|c| !matches!(c.id, "gp.finish" | "info.commands"))
        .filter_map(|c| {
            let (id, label, loc, params) =
                (c.id.to_lowercase(), c.label.to_lowercase(), c.location.to_lowercase(), c.params.to_lowercase());
            let sc = score(&words, &[&id, &label, &loc, &params]);
            (sc > 0).then(|| {
                (sc, json!({"id": c.id, "label": c.label, "location": c.location, "params": c.params, "changesProject": c.mutates}))
            })
        })
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    let total = hits.len();
    Ok(json!({"matches": total, "commands": hits.into_iter().take(limit).map(|h| h.1).collect::<Vec<_>>()}))
}

fn search_tools(a: &Value) -> Result<Value, String> {
    let words = words(a)?;
    let limit = limit_arg(a, 12, 40);
    let mut hits: Vec<(usize, Value)> = analysis::published_tools()
        .filter_map(|t| {
            let (id, name, tb, summary) = (t.id.to_lowercase(), t.name.to_lowercase(), t.toolbox.to_lowercase(), t.summary.to_lowercase());
            let sc = score(&words, &[&id, &name, &tb, &summary]);
            (sc > 0).then(|| (sc, json!({"id": t.id, "name": t.name, "toolbox": t.toolbox, "summary": clip(t.summary, 200)})))
        })
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.0));
    let total = hits.len();
    Ok(json!({"matches": total, "tools": hits.into_iter().take(limit).map(|h| h.1).collect::<Vec<_>>()}))
}

fn describe_tool(a: &Value) -> Result<Value, String> {
    let id = str_arg(a, "tool").ok_or("tool is required")?;
    let t = analysis::resolve_published(&id)
        .map(|r| r.spec())
        .ok_or_else(|| format!("no published tool '{id}'; open the tools or load the suite first"))?;
    let params: Vec<Value> = t
        .params
        .iter()
        .map(|p| {
            let kind = match p.kind {
                analysis::ParamKind::Choice(c) => json!({"choice": c}),
                k => json!(format!("{k:?}")),
            };
            json!({"name": p.name, "label": p.label, "kind": kind, "required": p.required, "default": p.default, "help": clip(p.help, 300)})
        })
        .collect();
    Ok(json!({"id": t.id, "name": t.name, "toolbox": t.toolbox, "summary": t.summary, "params": params}))
}

/// The rows `read_features` reads: a vector layer's or a standalone table's.
struct RowSource<'a> {
    /// The reply's key for the name: `layer` or `table`.
    kind: &'static str,
    name: &'a str,
    ds: &'a rustgis_engine::data::Dataset,
    /// The layer's selected rows; a standalone table has no selection.
    selection: Option<&'a [usize]>,
}

/// Resolves `read_features`' `table` (a standalone table's name or id) or `layer` (a layer's name
/// or id). A `layer` text that names no layer but a standalone table reads the table: a tool's
/// output table comes back by name, and a model passes that name as `layer` as readily as `table`.
/// Layers come first, so a layer and a table sharing a name read the layer; that fallback matches
/// table names only, by [`project::find_table`]'s rule (exact case first), not ids. A `table` given
/// as null or empty text counts as absent, since models send unused optional arguments that way;
/// otherwise it is read instead of `layer`.
fn row_source<'a>(s: &'a Session, a: &Value) -> Result<RowSource<'a>, String> {
    let table = |i: usize| {
        s.project
            .tables
            .get(i)
            .map(|t| RowSource { kind: "table", name: &t.name, ds: &t.dataset, selection: None })
            .ok_or_else(|| "no such table".to_string())
    };
    let given = |v: &&Value| !v.is_null() && v.as_str().is_none_or(|t| !t.trim().is_empty());
    if let Some(t) = a.get("table").filter(given) {
        return table(s.table_index(&json!({"table": t}))?);
    }
    let layer = a.get("layer").cloned().ok_or("layer (or table) is required")?;
    let id = match s.layer_id(&json!({"layer": layer})) {
        Ok(id) => id,
        Err(e) => {
            // Untrimmed, as `table` and layer names are matched: `stats` and `stats ` can both exist.
            let Some(n) = layer.as_str() else { return Err(e) };
            let tables = &s.project.tables;
            // `find_table` falls back to an id; keep only a name match, so `layer: "7"` can't read table 7.
            let by_name = project::find_table(tables, n).filter(|&i| tables.get(i).is_some_and(|t| t.name.eq_ignore_ascii_case(n)));
            return match by_name {
                Some(i) => table(i),
                None => Err(format!("no layer or standalone table named '{n}'")),
            };
        }
    };
    let l = s.map().layer(id).ok_or("no such layer")?;
    if l.is_raster() {
        return Err(format!("{} is a raster layer; it has no rows (try the raster.info command)", l.name));
    }
    Ok(RowSource { kind: "layer", name: &l.name, ds: &l.dataset, selection: Some(&l.selection) })
}

/// `read_features`: the rows of a layer or standalone table ([`row_source`]) that match `where` and
/// `selected`, ordered by `order_by`, as `{layer | table, total, matched, rows}`. Only the first
/// `limit` rows are returned, each with its `_row`, the chosen `fields` and a layer feature's
/// `_lon`/`_lat`.
fn read_features(s: &Session, a: &Value) -> Result<Value, String> {
    let src = row_source(s, a)?;
    let (name, ds) = (src.name, src.ds);
    let mut rows: Vec<usize> = if a.get("selected").and_then(Value::as_bool) == Some(true) {
        let sel = src.selection.ok_or_else(|| format!("{name} is a standalone table; it has no selection"))?;
        sel.iter().copied().filter(|&r| r < ds.features.len()).collect()
    } else {
        (0..ds.features.len()).collect()
    };
    if let Some(w) = str_arg(a, "where").filter(|w| !w.is_empty()) {
        let hits: std::collections::HashSet<usize> = rustgis_engine::data::query::select(&w, ds)?.into_iter().collect();
        rows.retain(|r| hits.contains(r));
    }
    let fields: Vec<usize> = match a.get("fields").and_then(Value::as_array) {
        Some(names) if !names.is_empty() => {
            let mut v = Vec::new();
            for n in names {
                let n = n.as_str().ok_or("fields must be field names (strings)")?;
                let i =
                    ds.fields.iter().position(|f| f.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("{name} has no field '{n}'"))?;
                v.push(i);
            }
            v
        }
        _ => (0..ds.fields.len()).collect(),
    };
    if let Some(by) = str_arg(a, "order_by").filter(|f| !f.is_empty()) {
        // SQL habits: `pop_max DESC`.
        let lower = by.to_ascii_lowercase();
        let (by, desc) = if let Some(f) = lower.strip_suffix(" desc") {
            (by.get(..f.len()).unwrap_or("").trim().to_string(), true)
        } else if let Some(f) = lower.strip_suffix(" asc") {
            (by.get(..f.len()).unwrap_or("").trim().to_string(), false)
        } else {
            (by, a.get("descending").and_then(Value::as_bool) == Some(true))
        };
        let fi = ds.fields.iter().position(|f| f.name.eq_ignore_ascii_case(&by)).ok_or_else(|| format!("{name} has no field '{by}'"))?;
        // A total order (a partial one can make `sort_by` panic): numbers, then text, then empty
        // values.
        let key = |r: usize| -> (u8, f64, String) {
            match ds.features.get(r).and_then(|f| f.attrs.get(fi)) {
                Some(rustgis_engine::data::Value::Null) | None => (2, 0.0, String::new()),
                Some(v) => match v.as_f64() {
                    Some(x) => (0, x, String::new()),
                    None => (1, 0.0, v.to_string().to_lowercase()),
                },
            }
        };
        let mut keyed: Vec<(usize, (u8, f64, String))> = rows.iter().map(|&r| (r, key(r))).collect();
        // Descending reverses within numbers and within text; empty values stay last.
        keyed.sort_by(|(_, a), (_, b)| {
            let within = a.1.total_cmp(&b.1).then_with(|| a.2.cmp(&b.2));
            a.0.cmp(&b.0).then(if desc { within.reverse() } else { within })
        });
        rows = keyed.into_iter().map(|(r, _)| r).collect();
    }
    let limit = limit_arg(a, 20, 200);
    let out: Vec<Value> = rows
        .iter()
        .take(limit)
        .filter_map(|&r| {
            let f = ds.features.get(r)?;
            let mut o = serde_json::Map::new();
            o.insert("_row".into(), json!(r));
            // Where it is (its extent's centre), for zooming or going to it.
            if let Some(g) = &f.geometry {
                let c = g.extent().center();
                if c.x.is_finite() && c.y.is_finite() {
                    o.insert("_lon".into(), json!((c.x * 1e5).round() / 1e5));
                    o.insert("_lat".into(), json!((c.y * 1e5).round() / 1e5));
                }
            }
            for &fi in &fields {
                let (Some(field), Some(v)) = (ds.fields.get(fi), f.attrs.get(fi)) else { continue };
                let v = match v {
                    rustgis_engine::data::Value::Null => Value::Null,
                    rustgis_engine::data::Value::Int(i) => json!(i),
                    rustgis_engine::data::Value::Float(x) => json!(x),
                    rustgis_engine::data::Value::Bool(b) => json!(b),
                    rustgis_engine::data::Value::Text(t) => json!(clip(t, 200)),
                };
                o.insert(field.name.clone(), v);
            }
            Some(Value::Object(o))
        })
        .collect();
    let mut reply = serde_json::Map::new();
    reply.insert(src.kind.into(), json!(name));
    reply.insert("total".into(), json!(ds.features.len()));
    reply.insert("matched".into(), json!(rows.len()));
    reply.insert("rows".into(), Value::Array(out));
    Ok(Value::Object(reply))
}

/// The map as the model sees it at the start of a request: one line per layer.
pub fn context(s: &Session) -> String {
    let m = s.map();
    let c = m.center_lonlat();
    let mut out = format!(
        "Map \"{}\": basemap {}, projection {}, centre {:.4}, {:.4} (lon, lat), scale 1:{:.0}.",
        m.name,
        m.basemap.code(),
        m.projection.code(),
        c.x,
        c.y,
        m.scale()
    );
    if let Some(l) = s.active_layer.and_then(|id| m.layer(id)) {
        out.push_str(&format!(" Selected in Layers: \"{}\".", l.name));
    }
    if m.layers.is_empty() {
        out.push_str("\nNo layers.");
    } else {
        out.push_str("\nLayers (top first):");
    }
    for l in m.layers.iter().take(60) {
        let mut line = if l.is_raster() {
            format!("\n- \"{}\" (id {}, raster", l.name, l.id)
        } else {
            format!("\n- \"{}\" (id {}, {}, {} features", l.name, l.id, l.geometry_type().label(), l.dataset.features.len())
        };
        if !l.visible {
            line.push_str(", hidden");
        }
        let groups = m.groups_of(l.id);
        if !groups.is_empty() {
            // Outermost first: "Base map › Hydrography".
            let path: Vec<&str> = groups.iter().rev().map(|g| g.name.as_str()).collect();
            let hidden = if groups.iter().all(|g| g.visible) { "" } else { " (hidden)" };
            line.push_str(&format!(", in group \"{}\"{hidden}", path.join(" › ")));
        }
        if !l.selection.is_empty() {
            line.push_str(&format!(", {} selected", l.selection.len()));
        }
        line.push(')');
        if !l.dataset.fields.is_empty() {
            let fields: Vec<String> = l.dataset.fields.iter().take(40).map(|f| format!("{}:{}", f.name, f.ty.label())).collect();
            line.push_str(&format!(" fields: {}", fields.join(", ")));
        }
        out.push_str(&clip(&line, 1500));
    }
    if m.layers.len() > 60 {
        out.push_str(&format!("\n… and {} more (list_layers lists them all).", m.layers.len() - 60));
    }
    if !m.tiles.is_empty() {
        out.push_str("\nWeb tile layers (top first, index for map.setTiles), each drawn over the layer it names, or under them all:");
    }
    let slots = m.tile_slots();
    for (i, t) in m.tiles.iter().enumerate() {
        // The name is clipped, not the line: the index and flags after it are what the model needs.
        let mut line = format!("\n- \"{}\" (index {i}, {}", clip(&t.name, 200), t.kind.code());
        if t.overlay {
            line.push_str(", overlay");
        }
        match slots.get(i).copied().flatten().and_then(|id| m.layer(id)) {
            Some(l) => line.push_str(&format!(", over \"{}\"", clip(&l.name, 200))),
            None => line.push_str(", under the layers"),
        }
        if !t.visible {
            line.push_str(", hidden");
        } else if let Some(e) = &t.error {
            line.push_str(&format!(", unavailable: {e}"));
        } else if !t.draws() {
            line.push_str(", hidden by the time slider (no date in its window)");
        }
        if t.opacity < 1.0 {
            line.push_str(&format!(", opacity {:.2}", t.opacity));
        }
        line.push(')');
        out.push_str(&line);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    /// Returned tool and fetch errors are cleaned before active patterns can damage URLs;
    /// successful values and the original call remain outside the generated-error projection.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn generated_tool_and_fetch_errors_clean_addresses_before_active_patterns() {
        const TEST: &str = "tools::tests::generated_tool_and_fetch_errors_clean_addresses_before_active_patterns";
        if std::env::var("RUSTGIS_ISOLATED_TOOL_ERRORS").as_deref() != Ok(TEST) {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST])
                .env("RUSTGIS_ISOLATED_TOOL_ERRORS", TEST)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed; 0 failed"),
                "{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use rustgis_engine::render::rustgis_tiles::{failure::Failure, logins};
        let mut s = Session::new(rustgis_engine::Project::default());
        let url = "https://returned-error.invalid/a?token=RGSYNTH_returned_error&layer=roads";
        for active in [false, true] {
            let binding = active.then(|| {
                logins::activate(
                    "https://returned-error.invalid",
                    "synthetic-returned-error",
                    logins::Method::Header("X-Key".into()),
                    "https",
                    false,
                )
                .unwrap()
            });
            for (name, args) in [
                ("read_features", json!({"layer":url})),
                ("describe_tool", json!({"tool":url})),
                (url, json!({})),
                ("run", json!({"script":url})),
            ] {
                let call = ToolCall { id: "error".into(), name: name.into(), args };
                let original = call.clone();
                let error = run_with_budget(&mut s, &call, DEFAULT_OUTPUT_TOKENS, Scope::peer(), None).unwrap_err();
                assert!(!error.contains("RGSYNTH_returned_error"), "{name}, active={active}: {error}");
                assert_eq!(call, original);
                if name == "run" {
                    let reply: Value = serde_json::from_str(&error).unwrap();
                    assert_eq!(reply["success"], false);
                    assert!(reply["error"].as_str().is_some());
                }
            }
            let address = binding.as_ref().map_or_else(|| url.to_string(), |binding| logins::bind(binding, url).unwrap());
            let failure = Failure::new(&address, format!("failed to fetch {address}"));
            assert!(!failure.message.contains("RGSYNTH_returned_error"));
            if let Some(binding) = binding {
                assert!(logins::deactivate(&binding));
            }
        }
        let clean = ToolCall { id: "clean".into(), name: "read_features".into(), args: json!({"layer":"ordinary missing layer"}) };
        assert!(
            run_with_budget(&mut s, &clean, DEFAULT_OUTPUT_TOKENS, Scope::peer(), None).unwrap_err().contains("ordinary missing layer")
        );
        assert_eq!(Failure::new("https://clean.invalid/a?layer=roads", "HTTP 403").message, "HTTP 403");
        assert_eq!(logins::scrub_error("failed https://clean.invalid/a?layer=roads"), "failed https://clean.invalid/a?layer=roads");
    }
    use super::*;
    use rustgis_engine::render::rustgis_tiles::logins;

    #[test]
    fn results_carry_plain_addresses_not_login_wrappers() {
        let mut s = Session::new(rustgis_engine::Project::default());
        let origin = "https://assistant-login757.example.org";
        let definition = json!({"kind":"wmts", "address": format!("{origin}/tiles"), "auth":"header", "keyName":"X-Key"});
        let id = s.run("connection.new", &definition).unwrap()["id"].as_str().unwrap().to_string();
        s.run("connection.login", &json!({"id": id, "apiKey": "assistant-secret-757"})).unwrap();
        let plain = format!("{origin}/tiles/{{z}}/{{x}}/{{y}}.png");
        let tile = rustgis_engine::service_logins::request(&mut s, Some(&id), &plain).unwrap();
        assert!(tile.starts_with(rustgis_engine::data::login_address::PREFIX));
        // The app's command result keeps the bound address it reads through; the model's doesn't.
        let call = |command: &str, params: Value| ToolCall {
            id: String::new(),
            name: "run_command".into(),
            args: json!({"command": command, "params": params}),
        };
        let scope = Scope::assistant(757, std::sync::Arc::new(Live::default()));
        let add = call("map.addTiles", json!({"url": tile, "name": "T"}));
        let pending = match s.review(&scope, &policy_call(&add).unwrap().unwrap()) {
            Outcome::NeedsApproval(pending) => Some(pending),
            _ => None,
        }
        .unwrap();
        let added = run_with_budget(&mut s, &add, DEFAULT_OUTPUT_TOKENS, scope.clone(), Some(pending.allow())).unwrap();
        let listed = run_with_budget(&mut s, &call("map.tiles", json!({})), DEFAULT_OUTPUT_TOKENS, scope, None).unwrap();
        assert!(s.map().tiles[0].url.starts_with("rustgis-login:"));
        for text in [&added, &listed] {
            assert!(!text.contains("rustgis-login:") && !text.contains("assistant-secret-757"), "{text}");
        }
        assert!(listed.contains(&plain), "the model still learns the layer's address: {listed}");
    }

    /// The known response schema survives a secret that happens to name one of its fields.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn run_envelope_keys_are_not_data_derived_login_values() {
        if std::env::var_os("RUSTGIS_TEST_RUN_ENVELOPE").is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tools::tests::run_envelope_keys_are_not_data_derived_login_values"])
                .env("RUSTGIS_TEST_RUN_ENVELOPE", "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed; 0 failed"),
                "{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let mut s = Session::new(rustgis_engine::Project::default());
        let id = s
            .run("connection.new", &json!({"kind":"wms", "address":"https://envelope.invalid/wms", "auth":"basic", "user":"reader"}))
            .unwrap()["id"]
            .clone();
        s.run("connection.login", &json!({"id":id, "password":"success"})).unwrap();
        let scope = Scope::assistant(152140, std::sync::Arc::new(Live::default()));
        for (script, success) in [("map.zoomIn {}", true), ("not.a.command {}", false)] {
            let call = ToolCall { id: "envelope".into(), name: "run".into(), args: json!({"script":script}) };
            let text = run(&mut s, &call, scope.clone(), None).unwrap_or_else(|e| e);
            let out: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(out.get("success"), Some(&json!(success)), "{text}");
            assert!(out.get("result").is_some() && out.get("notices").is_some() && out.get("duration_s").is_some());
        }
        let data = public_addresses(json!({"success":"private field"}), 0, &mut Withheld::default());
        assert_eq!(data["[redacted]"], "private field", "the same spelling in dynamic data is still scrubbed");
        s.run("connection.login", &json!({"id":id, "password":"id"})).unwrap();
        for (name, args, array) in
            [("search_commands", json!({"query":"zoom"}), "commands"), ("search_tools", json!({"query":"buffer"}), "tools")]
        {
            let call = ToolCall { id: "metadata".into(), name: name.into(), args };
            let text = run(&mut s, &call, scope.clone(), None).unwrap();
            let out: Value = serde_json::from_str(&text).unwrap();
            let entries = out.get(array).and_then(Value::as_array).unwrap();
            assert!(!entries.is_empty(), "{text}");
            assert!(entries.iter().all(|entry| entry.get("id").is_some()), "fixed nested schema keys survive: {text}");
        }
        let data = public_addresses(json!({"id":"private field"}), 0, &mut Withheld::default());
        assert_eq!(data["[redacted]"], "private field");
    }

    #[test]
    fn wrappers_after_a_prefix_are_made_plain_too() {
        let origin = "https://assistant-login.example";
        let binding = logins::activate(origin, "assistant", logins::Method::Header("X-Key".into()), "assistant-secret", false).unwrap();
        let archive = logins::bind(&binding, &format!("{origin}/a.pmtiles")).unwrap();
        let result = json!({"layers": [{"next": {"request": {"url": format!("pmtiles:x@{archive}")}}}], "count": 2});
        let v = public_addresses(result, 0, &mut Withheld::default());
        assert_eq!(v["layers"][0]["next"]["request"]["url"], format!("pmtiles:x@{origin}/a.pmtiles"));
        assert_eq!(v["count"], 2);
        assert!(logins::deactivate(&binding));
    }

    #[test]
    fn too_deep_results_report_each_withheld_subtree_without_losing_success() {
        let mut s = Session::new(rustgis_engine::Project::default());
        let mut deep = json!({"value": "rustgis-login:leak"});
        for _ in 0..MAX_DEPTH {
            deep = json!([deep]);
        }
        let result = json!({"success": true, "left": deep.clone(), "right": deep, "notices": ["existing warning"]});
        let out: Value = serde_json::from_str(&public_result(&mut s, result, DEFAULT_OUTPUT_TOKENS)).unwrap();
        assert_eq!(out["success"], true, "withholding output must not report an executed command as failed");
        assert!(!out.to_string().contains("rustgis-login:"));
        assert_eq!(out["notices"][0], "existing warning");
        let notice = out["notices"][1].as_str().unwrap();
        let counts: Vec<usize> = notice.split_whitespace().filter_map(|word| word.parse().ok()).collect();
        assert_eq!(counts, [2, MAX_DEPTH], "the notice reports truncated subtrees, not an unbounded count of their contents");
        assert_eq!(s.notices_since(0).len(), 1, "the app also receives one aggregate loss notice");
    }

    #[test]
    fn successful_results_scrub_active_raw_service_keys_without_a_wrapper() {
        let mut s = Session::new(rustgis_engine::Project::default());
        let id = s
            .run(
                "connection.new",
                &json!({"kind":"wms","address":"https://echo-r15.example.invalid/wms","auth":"header","keyName":"X-Key"}),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        s.run("connection.login", &json!({"id":id,"apiKey":"fake001"})).unwrap();
        let result = json!({"success":true,"echo":"server said fake001","clean":"ordinary café text"});
        let out: Value = serde_json::from_str(&public_result(&mut s, result, DEFAULT_OUTPUT_TOKENS)).unwrap();
        assert_eq!(out["success"], true);
        assert_eq!(out["echo"], "server said [redacted]");
        assert_eq!(out["clean"], "ordinary café text");
        assert!(s.notices_since(0).is_empty());
        // A generated row uses imported field names as object keys. Keep the stored data,
        // but never send a field repeating an active key to the model in plain text.
        let source = json!({"type":"FeatureCollection","features":[{
            "type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},
            "properties":{"fake001":"retained value","clean":"ordinary café text"}
        }]});
        s.run("layer.addDataText", &json!({"fileName":"echo-fields.geojson","text":source.to_string()})).unwrap();
        let name = s.map().layers.last().unwrap().name.clone();
        let call = ToolCall { id: "echo-fields".into(), name: "read_features".into(), args: json!({"layer":name,"limit":1}) };
        let scope = Scope::assistant(9035, std::sync::Arc::new(Live::default()));
        let mark = s.notice_mark();
        let text = run_with_budget(&mut s, &call, DEFAULT_OUTPUT_TOKENS, scope, None).unwrap();
        assert!(!text.contains("fake001"), "generated object keys must be scrubbed too: {text}");
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["rows"][0]["[redacted]"], "retained value");
        assert_eq!(out["rows"][0]["clean"], "ordinary café text");
        assert!(s.map().layers.last().unwrap().dataset.fields.iter().any(|f| f.name == "fake001"), "stored data stays unchanged");
        assert!(s.notices_since(mark).is_empty());
        // Two keys that become equal must not silently overwrite one value with the other.
        let mark = s.notice_mark();
        let collision = json!({"success":true,"result":{"fake001":"first value","[redacted]":"second value"}});
        let text = public_result(&mut s, collision, DEFAULT_OUTPUT_TOKENS);
        assert!(!text.contains("fake001"));
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["success"], true);
        assert!(out["result"].is_string(), "ambiguous redacted objects are explicitly withheld");
        assert_eq!(s.notices_since(mark).len(), 1);
        assert!(s.notices_since(mark)[0].text.contains("field names"));
    }

    #[test]
    fn clean_nested_results_need_no_withholding_notice() {
        let mut s = Session::new(rustgis_engine::Project::default());
        let result = json!({"result": {"nested": [{"value": "complete"}]}, "success": true});
        let out: Value = serde_json::from_str(&public_result(&mut s, result.clone(), DEFAULT_OUTPUT_TOKENS)).unwrap();
        assert_eq!(out, result);
        assert!(s.notices_since(0).is_empty());
    }
}
