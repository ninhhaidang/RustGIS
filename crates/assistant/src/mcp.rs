//! A Model Context Protocol server: RustGIS for any MCP client (Claude Code, Claude Desktop,
//! Cursor, VS Code…). The client's model drives a session through the assistant's tools
//! ([`crate::tools`]), sees the map as an image and reads the built-in documentation
//! ([`crate::docs`]).
//!
//! Sans I/O: [`Server::handle`] takes one JSON-RPC message and gives back the reply, so
//! `rustgis-cli mcp` (standard input and output) and the tests drive the same code.
//!
//! The client asks its user before each tool call; the server adds its own floor. File and
//! network access is refused unless the server was started allowing it, and the user's own
//! automation (scripts, tasks, models, plugin management) is never run. Without access, the only
//! network use left is drawing web tile layers that the project the user opened already has.

use rustgis_engine::Session;
use rustgis_engine::policy::{AgentAccess, Effects, Outcome, Permit, Scope};
use serde_json::{Value, json};
use std::sync::Arc;

use crate::provider::ToolCall;
use crate::{docs, tools};

/// The protocol versions this server speaks, newest first.
pub const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// The most messages one batch may hold.
pub const MAX_BATCH: usize = 16;

/// The largest map image `view_map` draws (pixels a side).
const MAX_VIEW: f64 = 2048.0;

/// The longest a screen snapshot waits for the map to finish drawing (seconds), and the default.
pub const MAX_SNAPSHOT_WAIT: f64 = 30.0;
const SNAPSHOT_WAIT: f64 = 5.0;

/// What the model is told about the server when it connects.
const INSTRUCTIONS: &str = "RustGIS is a desktop GIS. This server holds one RustGIS project in memory and acts on it.\n\
- list_layers shows the map. view_map shows it as an image: look after changing it. snapshot adds the view's extent and scale, and can save the image.\n\
- Every action is a command: search_commands finds one, run_command runs it (navigation, layers, symbology, labels, selection, editing, layouts, export…). run runs several commands at once, as one undo step.\n\
- Geoprocessing: search_tools, then describe_tool for its parameters, then run_tool; its output is a new layer, or a standalone table.\n\
- read_features reads attribute rows of a layer or a standalone table, with a where clause.\n\
- search_docs and read_doc look things up in the RustGIS documentation.\n\
Open data with the layer.addData command ({\"path\": …}) and save with project.saveAs ({\"path\": \"*.rgis\"}); \
file and network access works only when the server was started with --yes.";

/// Added to [`INSTRUCTIONS`] on the desktop app's relay, where the session is the user's own.
const LIVE: &str = "\nThis is the project the user has open in RustGIS, and they watch the map change as you work.\n\
- Call list_layers before acting on a project you haven't seen; name layers in your commands rather than relying on the one the user has selected.\n\
- Call view_map after any visual change and check it before you say it worked.\n\
- Each command is a step in the user's own undo history; don't undo what you didn't do.\n\
- Replacing the project (project.new, project.open…) is the user's to do.\n\
- A long geoprocessing run keeps the app busy: say what you are about to run first.";

/// The server: the session's access policy. The session itself is the caller's.
#[derive(Debug, Clone)]
pub struct Server {
    /// Whether commands may read and write files, use the network and run plugin commands.
    pub allow_access: bool,
    /// The skills offered as prompts ([`crate::skills`]).
    pub skills: Vec<crate::skills::Skill>,
    /// Whether the session is the project the user has open (the desktop app's relay), so
    /// commands that replace it stay the user's. Off for `rustgis-cli mcp`'s own session.
    pub users_project: bool,
    /// Full access, which the user turned on in the desktop app: no command is refused, the
    /// user's own automation (scripts, tasks, models, plugin commands and management) included,
    /// as from a scripting console.
    pub full_access: bool,
}

impl Default for Server {
    fn default() -> Self {
        Self::new(false)
    }
}

/// A JSON-RPC error: its code and message.
type RpcError = (i64, String);

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
/// MCP's code for a resource that doesn't exist.
const RESOURCE_NOT_FOUND: i64 = -32002;

/// Binds an ingress request to its complete-message position, independently of wire ids.
/// Count batch entries before filtering; an unbatched message always uses position zero.
pub(crate) fn request_key(root: &str, position: usize) -> String {
    format!("{root}/{position}")
}

/// A relay message paused at an admitted standalone archive command. Batch positions and
/// one-use permits remain owned here; no suffix entry runs until the actual command lands.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct ArchiveBatch {
    server: Server,
    scope: Scope,
    permits: Vec<(String, Permit)>,
    request: String,
    messages: std::vec::IntoIter<Value>,
    position: usize,
    batch: bool,
    replies: Vec<Value>,
    pending: Option<(Value, tools::CommandCompletion)>,
    ahead: Option<rustgis_engine::cmd::reads::ReadAhead>,
    /// Successful explicit selections in the current uninterrupted execution segment.
    picked: bool,
}
#[cfg(not(target_arch = "wasm32"))]
pub(crate) enum ArchiveStep {
    Done(Option<String>, bool),
    Read(Box<ArchiveBatch>),
}
#[cfg(not(target_arch = "wasm32"))]
impl ArchiveBatch {
    pub(crate) fn take_read(&mut self) -> Option<rustgis_engine::cmd::reads::ReadAhead> {
        self.ahead.take()
    }
    pub(crate) fn picked(&self) -> bool {
        self.picked
    }
    /// The command's reply reports changes made from the moment it actually runs, not from its
    /// admission: the queue may run user work or other reads first.
    pub(crate) fn begin_landing(&mut self, s: &Session) {
        if let Some((_, completion)) = &mut self.pending {
            *completion = tools::CommandCompletion::capture(s);
        }
    }
    pub(crate) fn finish(mut self, s: &mut Session, result: Result<Value, String>) -> ArchiveStep {
        self.finish_pending(s, result);
        self.advance(s)
    }
    /// A changed project or map invalidates the suffix, not the command's actual result; so does
    /// the app closing. Complete every remaining request with `remaining` as its error, without
    /// dispatching even a read-only tool.
    pub(crate) fn cancel_remaining(mut self, s: &mut Session, result: Result<Value, String>, remaining: &str) -> ArchiveStep {
        self.finish_pending(s, result);
        for msg in self.messages.by_ref() {
            match request_identity(&msg) {
                Ok(Some((_, id))) => {
                    self.replies.push(error_value(&id, -32000, remaining));
                }
                Ok(None) => {}
                Err(reply) => self.replies.push(reply),
            }
        }
        self.done()
    }
    fn finish_pending(&mut self, s: &mut Session, result: Result<Value, String>) {
        self.picked = false;
        if let Some((id, completion)) = self.pending.take() {
            self.replies.push(archive_reply(id, completion.finish(s, result)));
        }
    }
    fn advance(mut self, s: &mut Session) -> ArchiveStep {
        while let Some(msg) = self.messages.next() {
            let request = request_key(&self.request, self.position);
            self.position += 1;
            let archive = (|| {
                let id = msg.get("id")?;
                if !matches!(id, Value::String(_) | Value::Number(_)) || msg.get("method").and_then(Value::as_str) != Some("tools/call") {
                    return None;
                }
                let (name, args) = tool_request(msg.get("params")?).ok()?;
                if name != "run_command"
                    || !matches!(args.get("command").and_then(Value::as_str).map(str::trim), Some("map.reloadTiles" | "map.repairTiles"))
                {
                    return None;
                }
                Some((id.clone(), ToolCall { id: request.clone(), name: name.into(), args }))
            })();
            if let Some((id, call)) = archive {
                let permit = self.permits.iter().position(|(key, _)| key == &request).map(|i| self.permits.remove(i).1);
                let completion = tools::CommandCompletion::capture(s);
                let admitted = tools::policy_call(&call)
                    .and_then(|call| call.ok_or_else(|| "missing archive adapter".to_string()))
                    .and_then(|call| s.start_scoped_archive_read(self.scope.clone(), &call, permit));
                match admitted {
                    Ok(ahead) => {
                        self.pending = Some((id, completion));
                        self.ahead = Some(ahead);
                        return ArchiveStep::Read(Box::new(self));
                    }
                    Err(error) => self.replies.push(archive_reply(id, completion.finish(s, Err(error)))),
                }
            } else if let Some(reply) = self.server.handle_value(s, &msg, &self.scope, &mut self.permits, &request) {
                if reply.get("error").is_none() && reply.pointer("/result/isError") == Some(&Value::Bool(false)) {
                    self.picked |= msg.get("params").is_some_and(|params| {
                        let Some(name) = params.get("name").and_then(Value::as_str) else { return false };
                        params.get("arguments").is_some_and(|args| tools::selects_layer(name, args))
                    });
                }
                self.replies.push(reply);
            }
        }
        self.done()
    }
    fn done(mut self) -> ArchiveStep {
        let reply = if self.replies.is_empty() {
            None
        } else if self.batch {
            Some(Value::Array(self.replies).to_string())
        } else {
            self.replies.pop().map(|reply| reply.to_string())
        };
        ArchiveStep::Done(reply, self.picked)
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn archive_reply(id: Value, result: Result<String, String>) -> Value {
    let (text, failed) = match result {
        Ok(text) => (text, false),
        Err(error) => (error, true),
    };
    json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":text}],"isError":failed}})
}

impl Server {
    /// A server with the built-in skills. `rustgis-cli mcp` and the desktop's relay replace them
    /// with the skills switched on (`skills::installed`): the user's, the plugins' and shared ones.
    pub fn new(allow_access: bool) -> Self {
        Self { allow_access, skills: crate::skills::built_in(), users_project: false, full_access: false }
    }

    /// Handles one message (a line of JSON): the reply to send, or `None` for a notification or
    /// a response.
    pub fn handle(&self, s: &mut Session, message: &str) -> Option<String> {
        self.handle_scoped(s, message, self.scope(s), Vec::new(), "")
    }
    #[cfg(not(target_arch = "wasm32"))]
    /// Desktop relay ingress; only direct archive commands pause. Scripts and other transports
    /// retain their synchronous contract, and malformed messages keep normal JSON-RPC handling.
    pub(crate) fn start_archive_batch(
        self,
        s: &mut Session,
        message: &str,
        scope: Scope,
        permits: Vec<(String, Permit)>,
        request: &str,
    ) -> ArchiveStep {
        let parsed = serde_json::from_str::<Value>(message);
        let (messages, batch) = match parsed {
            Ok(Value::Array(batch)) if !batch.is_empty() && batch.len() <= MAX_BATCH => (batch, true),
            Ok(msg) if !msg.is_array() => (vec![msg], false),
            _ => return ArchiveStep::Done(self.handle_scoped(s, message, scope, permits, request), false),
        };
        ArchiveBatch {
            server: self,
            scope,
            permits,
            request: request.into(),
            messages: messages.into_iter(),
            position: 0,
            batch,
            replies: Vec::new(),
            pending: None,
            ahead: None,
            picked: false,
        }
        .advance(s)
    }
    /// Named relay/file-bridge ingress; permits are engine plans stored when approval was shown.
    pub(crate) fn handle_scoped(
        &self,
        s: &mut Session,
        message: &str,
        scope: Scope,
        mut permits: Vec<(String, Permit)>,
        request: &str,
    ) -> Option<String> {
        let msg: Value = match serde_json::from_str(message) {
            Ok(v) => v,
            Err(e) => return Some(error(&Value::Null, PARSE_ERROR, &format!("not JSON: {e}"))),
        };
        let reply = match &msg {
            // A batch (protocol 2025-03-26): one reply per request, none when all are notifications.
            Value::Array(batch) if batch.is_empty() => error_value(&Value::Null, INVALID_REQUEST, "empty batch"),
            // Each reply is held until the batch is done, and a map image can be megabytes.
            Value::Array(batch) if batch.len() > MAX_BATCH => {
                error_value(&Value::Null, INVALID_REQUEST, &format!("a batch holds at most {MAX_BATCH} messages"))
            }
            Value::Array(batch) => {
                let replies: Vec<Value> = batch
                    .iter()
                    .enumerate()
                    .filter_map(|(position, m)| self.handle_value(s, m, &scope, &mut permits, &request_key(request, position)))
                    .collect();
                if replies.is_empty() {
                    return None;
                }
                Value::Array(replies)
            }
            _ => self.handle_value(s, &msg, &scope, &mut permits, &request_key(request, 0))?,
        };
        Some(reply.to_string())
    }

    /// Applies per-entry JSON-RPC identity/notification rules before tool dispatch.
    fn handle_value(
        &self,
        s: &mut Session,
        msg: &Value,
        scope: &Scope,
        permits: &mut Vec<(String, Permit)>,
        request: &str,
    ) -> Option<Value> {
        let (method, id) = match request_identity(msg) {
            Ok(Some(request)) => request,
            Ok(None) => return None,
            Err(reply) => return Some(reply),
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        Some(match self.call(s, method, &params, scope, permits, request) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => error_value(&id, code, &message),
        })
    }

    fn call(
        &self,
        s: &mut Session,
        method: &str,
        params: &Value,
        scope: &Scope,
        permits: &mut Vec<(String, Permit)>,
        request: &str,
    ) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(initialize(params, &self.instructions())),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_list()})),
            "tools/call" => {
                let (name, args) = tool_request(params)?;
                let permit = permits.iter().position(|(id, _)| id == request).map(|i| permits.remove(i).1);
                Ok(self.call_tool(s, name, args, scope.clone(), permit, request))
            }
            "resources/list" => Ok(json!({"resources": resource_list()})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).ok_or((INVALID_PARAMS, "uri is required".to_string()))?;
                let page =
                    uri.strip_prefix("rustgis://docs/").and_then(docs::page).ok_or((RESOURCE_NOT_FOUND, format!("no resource '{uri}'")))?;
                Ok(json!({"contents": [{"uri": uri, "mimeType": "text/markdown", "text": page.text}]}))
            }
            // Skills, as prompts: a client offers them to its user, or reads them for its model.
            "prompts/list" => Ok(
                json!({"prompts": self.skills.iter().map(|s| json!({"name": s.name, "description": s.description})).collect::<Vec<_>>()}),
            ),
            "prompts/get" => {
                let name = params.get("name").and_then(Value::as_str).ok_or((INVALID_PARAMS, "name is required".to_string()))?;
                let skill = self.skills.iter().find(|s| s.name == name).ok_or((INVALID_PARAMS, format!("no prompt '{name}'")))?;
                Ok(
                    json!({"description": skill.description, "messages": [{"role": "user", "content": {"type": "text", "text": skill.body}}]}),
                )
            }
            _ => Err((METHOD_NOT_FOUND, format!("unknown method '{method}'"))),
        }
    }

    /// Runs a tool; failures are results the model reads (`isError`), not protocol errors.
    fn call_tool(&self, s: &mut Session, name: &str, args: Value, scope: Scope, permit: Option<Permit>, request: &str) -> Value {
        let text = |t: String, is_error: bool| json!({"content": [{"type": "text", "text": t}], "isError": is_error});
        let call = ToolCall { id: request.into(), name: name.into(), args: args.clone() };
        let result = match name {
            "view_map" => {
                let result = (|| {
                    let engine = tools::policy_call(&call)?.ok_or("missing view adapter")?;
                    let owned = s.defer(scope, &engine, permit)?;
                    s.finish_deferred(owned, |s| view_map(s, &args))?
                })();
                return result.unwrap_or_else(|e| text(e, true));
            }
            "snapshot" => {
                let admitted = (|| {
                    let engine = tools::policy_call(&call)?.ok_or("missing snapshot adapter")?;
                    let owned = s.defer(scope.clone(), &engine, permit)?;
                    s.finish_deferred(owned, |s| self.snapshot(s, &args, &scope))?
                })();
                return admitted.unwrap_or_else(|e| text(e, true));
            }
            _ => tools::run(s, &call, scope, permit),
        };
        match result {
            Ok(t) => text(t, false),
            Err(e) => text(e, true),
        }
    }

    /// `snapshot` drawn offscreen: the server can't see the app's window, so a `screen` snapshot
    /// is drawn too, with a note. The desktop relay captures the window itself
    /// ([`crate::relay`]) and comes here only when the map isn't on screen.
    fn snapshot(&self, s: &mut Session, a: &Value, scope: &Scope) -> Result<Value, String> {
        let note = match a.get("mode").and_then(Value::as_str) {
            Some("screen") if self.users_project => Some(
                "the map isn't on screen in RustGIS (a layout or the catalog is shown, or the window is hidden), so it was drawn offscreen",
            ),
            Some("screen") => {
                Some("screen capture works only through the desktop app's MCP relay (rustgis-cli mcp --attach); drawn offscreen")
            }
            _ => None,
        };
        render_snapshot_checked(s, a, note, scope)
    }

    /// What the model is told about the server when it connects.
    pub fn instructions(&self) -> String {
        if self.users_project { format!("{INSTRUCTIONS}{LIVE}") } else { INSTRUCTIONS.to_string() }
    }

    /// Why this server won't run a call, if it won't. With [`Server::full_access`] there is no
    /// refusal at all, by design: the user opted in to agents doing anything they could, project
    /// commands, files and the network included. A refusal added below is lifted by it too, so a
    /// rule that must hold even then belongs before this check.
    pub fn refusal(&self, s: &Session, call: &ToolCall) -> Option<String> {
        match tools::policy_call(call) {
            Err(e) => Some(e),
            Ok(None) => None,
            Ok(Some(call)) => match s.review(&self.scope(s), &call) {
                Outcome::Allowed => None,
                Outcome::NeedsApproval(p) => Some(p.review().why.clone()),
                Outcome::Refused(e) => Some(e),
            },
        }
    }
    /// Captures this server's named ingress; nested work inherits it through Session::run.
    fn scope(&self, s: &Session) -> Scope {
        let access = if self.full_access {
            AgentAccess::Full
        } else if self.allow_access {
            AgentAccess::Allow
        } else {
            AgentAccess::Refuse
        };
        Scope::agent(access, self.users_project, "stdio", s.actor.clone(), Arc::new(FixedLive))
    }
}

/// Validates the same tool envelope for execution and reviewed batch extraction.
pub(crate) fn tool_request(params: &Value) -> Result<(&str, Value), RpcError> {
    let name = params.get("name").and_then(Value::as_str).ok_or((INVALID_PARAMS, "name is required".to_string()))?;
    let args = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return Err((INVALID_PARAMS, "arguments must be an object".into())),
    };
    if !TOOL_NAMES.contains(&name) && !tools::defs().iter().any(|d| d.name == name) {
        return Err((INVALID_PARAMS, format!("unknown tool '{name}'")));
    }
    Ok((name, args))
}

fn initialize(params: &Value, instructions: &str) -> Value {
    // Speak the client's version when we know it, else our newest.
    let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
    let version = VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}, "resources": {"listChanged": false}, "prompts": {"listChanged": false}},
        "serverInfo": {"name": "rustgis", "title": "RustGIS", "version": env!("CARGO_PKG_VERSION")},
        "instructions": instructions,
    })
}

/// The tools this server adds to the assistant's.
const TOOL_NAMES: &[&str] = &["view_map", "run", "snapshot"];

/// Tools that only look: list, search, read, view. A snapshot can be saved over a file.
fn read_only(name: &str) -> bool {
    !matches!(name, "run_command" | "run_tool" | "run" | "snapshot")
}

fn tool_list() -> Vec<Value> {
    let mut out: Vec<Value> = tools::defs()
        .into_iter()
        .map(|d| {
            let description = match d.name {
                "run_command" => "Run a RustGIS command by id with JSON parameters (see search_commands for ids and parameters). Each run is one undo step (edit.undo undoes it). Commands that read or write files or use the network run only when the server was started with --yes.".to_string(),
                "read_doc" => {
                    let pages: Vec<&str> = docs::PAGES.iter().map(|p| p.path).collect();
                    format!("Read a RustGIS documentation page or section as Markdown. Use next_offset to continue budgeted results. Pages: {}.", pages.join(", "))
                }
                _ => d.description.to_string(),
            };
            json!({"name": d.name, "description": description, "inputSchema": d.schema, "annotations": annotations(d.name)})
        })
        .collect();
    out.push(json!({
        "name": "run",
        "description": "Run several RustGIS commands in one call, as one undo step: a script with one `command-id {json}` per line (# starts a comment), e.g. `map.basemap {\"name\": \"ocean\"}`. A failing line stops the script, nothing it did to the project is kept (a file an earlier line wrote stays written), and the error names the line. Returns {success, result (the last command's), lines, layersAdded, notices, error, duration_s}. Each line follows the same rules as run_command.",
        "inputSchema": {
            "type": "object",
            "properties": {"script": {"type": "string", "description": "One command per line: id {json}"}},
            "required": ["script"]
        },
        "annotations": annotations("run"),
    }));
    out.push(json!({
        "name": "snapshot",
        "description": "The map as a PNG image with JSON metadata (mode, size, lon/lat extent, centre, scale, projection, render_settled). mode \"screen\" is exactly what the user sees in the RustGIS window, after waiting up to wait_s for drawing and tiles to finish (desktop app relay only); \"render\" draws the same layers, view and styling offscreen at max_width, which works while the window is hidden; \"auto\" (default) captures the screen when the map is on it. save_path also writes the PNG to a file (needs --yes).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "mode": {"type": "string", "enum": ["auto", "screen", "render"]},
                "max_width": {"type": "integer", "description": "render: pixels wide (64 to 2048, default 1024); the height follows the view's shape"},
                "wait_s": {"type": "number", "description": "screen: the longest to wait for the map to finish drawing (0 to 30, default 5)"},
                "overlays": {"type": "boolean", "description": "screen: show the map's controls, scale bar, north arrow and sketches (default true); render never draws them"},
                "save_path": {"type": "string", "description": "Also write the PNG here"}
            }
        },
        "annotations": annotations("snapshot"),
    }));
    out.push(json!({
        "name": "view_map",
        "description": "The map as it looks now, as a PNG image: look after changing it. The current view, at a size of your choosing (default 1024 x 640).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "width": {"type": "integer", "description": "Pixels (64 to 2048)"},
                "height": {"type": "integer", "description": "Pixels (64 to 2048)"}
            }
        },
        "annotations": annotations("view_map"),
    }));
    out
}

fn annotations(name: &str) -> Value {
    let ro = read_only(name);
    // Commands can delete features, overwrite files and replace the project.
    json!({"readOnlyHint": ro, "destructiveHint": !ro, "idempotentHint": ro, "openWorldHint": !ro})
}

fn resource_list() -> Vec<Value> {
    docs::PAGES
        .iter()
        .map(|p| {
            json!({
                "uri": format!("rustgis://docs/{}", p.path),
                "name": p.path,
                "title": p.title(),
                "description": format!("RustGIS documentation: {} ({})", p.title(), p.url()),
                "mimeType": "text/markdown",
            })
        })
        .collect()
}

fn view_map(s: &mut Session, a: &Value) -> Result<Value, String> {
    use base64::Engine;
    let size = |k: &str, default: f64| a.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(default).clamp(64.0, MAX_VIEW);
    let (w, h) = (size("width", 1024.0), size("height", 640.0));
    let png = rustgis_engine::cmd::share::map_png(s, Some(w), Some(h), None)?;
    let m = s.map();
    let c = m.center_lonlat();
    let caption = format!(
        "Map \"{}\", {} layers, centre {:.4}, {:.4} (lon, lat), scale 1:{:.0}, {w:.0} x {h:.0} px.",
        m.name,
        m.layers.len(),
        c.x,
        c.y,
        m.scale()
    );
    Ok(json!({
        "content": [
            {"type": "image", "data": base64::engine::general_purpose::STANDARD.encode(png), "mimeType": "image/png"},
            {"type": "text", "text": caption},
        ],
        "isError": false,
    }))
}

/// Where a snapshot is to be saved, if anywhere.
fn save_path(a: &Value) -> Option<&str> {
    a.get("save_path").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty())
}

/// How long a screen snapshot may wait for drawing to finish (`wait_s`, clamped).
pub fn snapshot_wait(a: &Value) -> f64 {
    a.get("wait_s").and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(SNAPSHOT_WAIT).clamp(0.0, MAX_SNAPSHOT_WAIT)
}

/// A `snapshot` drawn offscreen at `max_width` in the view's shape, with `note` saying why when it
/// was asked of the screen.
pub fn render_snapshot(s: &mut Session, a: &Value, note: Option<&str>) -> Result<Value, String> {
    render_snapshot_with(s, a, note, None)
}
pub(crate) fn render_snapshot_checked(s: &mut Session, a: &Value, note: Option<&str>, scope: &Scope) -> Result<Value, String> {
    render_snapshot_with(s, a, note, Some(scope))
}
fn render_snapshot_with(s: &mut Session, a: &Value, note: Option<&str>, scope: Option<&Scope>) -> Result<Value, String> {
    let asked = a.get("max_width").and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(1024.0).clamp(64.0, MAX_VIEW);
    // The view's shape, so the image shows what the window does: a very tall view is narrowed
    // to fit rather than squashed.
    let (vw, vh) = s.view_size;
    let shape = if vw >= 1.0 && vh >= 1.0 { vh / vw } else { 0.625 };
    let width = asked.min(MAX_VIEW / shape.max(f64::MIN_POSITIVE)).max(64.0);
    let height = (width * shape).clamp(64.0, MAX_VIEW);
    let png = rustgis_engine::cmd::share::map_png(s, Some(width), Some(height), None)?;
    // Tiles still on their way are missing from it.
    let settled = rustgis_engine::render::rustgis_tiles::cache::in_flight() == 0;
    snapshot_result_with(s, &png, "render", settled, a, note, scope)
}

/// A `snapshot` result: the image, and the view it shows as JSON. `mode` is how it was made
/// (`screen` or `render`) and `settled` whether drawing had finished. Saves the PNG to
/// `save_path` when the arguments name one (the caller checked the server allows it).
pub fn snapshot_result(s: &mut Session, png: &[u8], mode: &str, settled: bool, a: &Value, note: Option<&str>) -> Result<Value, String> {
    snapshot_result_with(s, png, mode, settled, a, note, None)
}
/// Formats a delivered screenshot inside its already resumed initiating scope.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn snapshot_result_checked(
    s: &mut Session,
    png: &[u8],
    mode: &str,
    settled: bool,
    a: &Value,
    note: Option<&str>,
    scope: &Scope,
) -> Result<Value, String> {
    snapshot_result_with(s, png, mode, settled, a, note, Some(scope))
}
/// Checks late save authority and acquires metadata through inherited dispatch.
fn snapshot_result_with(
    s: &mut Session,
    png: &[u8],
    mode: &str,
    settled: bool,
    a: &Value,
    note: Option<&str>,
    scope: Option<&Scope>,
) -> Result<Value, String> {
    use base64::Engine;
    if let Some(scope) = scope {
        scope.check_live(Effects::NONE)?;
    }
    let saved = match save_path(a) {
        Some(path) => {
            if let Some(scope) = scope {
                scope.check_live(Effects::NONE.with(rustgis_engine::policy::Effect::Files))?;
            }
            std::fs::write(path, png).map_err(|e| format!("could not save the snapshot to '{path}': {e}"))?;
            Some(path)
        }
        None => None,
    };
    // The PNG's own size, from its header.
    let dim = |at: usize| png.get(at..at + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_be_bytes);
    let info = s.run("info.project", &json!({"includeFields":false}))?;
    let meta = json!({
        "mode": mode,
        "width": dim(16),
        "height": dim(20),
        "extent": info.get("extent"),
        "center": info.get("center"),
        "scale": info.get("scale"),
        "projection": info.get("projection"),
        "render_settled": settled,
        "saved": saved,
        "note": note,
    });
    Ok(json!({
        "content": [
            {"type": "image", "data": base64::engine::general_purpose::STANDARD.encode(png), "mimeType": "image/png"},
            {"type": "text", "text": meta.to_string()},
        ],
        "isError": false,
    }))
}

/// Whether the server answers this message: a request (an id, not a response to one) or an
/// unparseable line. A transport that waits for the server's reply — the relay client — needs
/// this to know a notification gets none.
pub fn expects_reply(message: &str) -> bool {
    // A non-object is an INVALID_REQUEST the server answers (`handle_value`), so it expects a
    // reply too; an object answers when it has an id and is neither a result nor an error.
    let replies = |m: &Value| match m.as_object() {
        None => true,
        Some(o) => o.contains_key("id") && !o.contains_key("result") && !o.contains_key("error"),
    };
    match serde_json::from_str::<Value>(message) {
        Ok(Value::Array(batch)) => batch.is_empty() || batch.iter().any(replies),
        Ok(v) => replies(&v),
        // Not JSON: the server answers with a parse error.
        Err(_) => true,
    }
}

/// Applies JSON-RPC identity rules without executing a request.
fn request_identity(msg: &Value) -> Result<Option<(&str, Value)>, Value> {
    let Some(obj) = msg.as_object() else {
        return Err(error_value(&Value::Null, INVALID_REQUEST, "expected a JSON-RPC object"));
    };
    let id = obj.get("id").cloned();
    let Some(method) = obj.get("method").and_then(Value::as_str) else {
        // A response to a request of ours (we send none), or junk.
        return match id.filter(|_| !obj.contains_key("result") && !obj.contains_key("error")) {
            Some(id) => Err(error_value(&id, INVALID_REQUEST, "expected a method")),
            None => Ok(None),
        };
    };
    // Notifications (no id) get no reply.
    let Some(id) = id else { return Ok(None) };
    if !matches!(id, Value::String(_) | Value::Number(_)) {
        return Err(error_value(&Value::Null, INVALID_REQUEST, "id must be a string or a number"));
    }
    Ok(Some((method, id)))
}

/// Projects generated RPC error text while retaining the exact correlation id and code.
fn error_value(id: &Value, code: i64, message: &str) -> Value {
    let message = rustgis_engine::render::rustgis_tiles::logins::scrub_error(message);
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn error(id: &Value, code: i64, message: &str) -> String {
    error_value(id, code, message).to_string()
}

struct FixedLive;
impl rustgis_engine::policy::Revocation for FixedLive {
    fn allows(&self, _: Effects) -> bool {
        true
    }
}
