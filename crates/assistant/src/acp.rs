//! Agents on this computer, through the Agent Client Protocol ([ACP](https://agentclientprotocol.com)):
//! the assistant can hand a task to a coding agent the user already has (Claude Code, Codex,
//! Gemini CLI, …) with `delegate`. RustGIS is the ACP client, as an editor is. It starts the
//! agent, gives it RustGIS's MCP server (the app's relay, with a token for this run), sends the
//! task, shows what the agent does, and asks the user whenever the agent asks for permission.
//!
//! Plain data here: which agents there are, how to start them, and an ACP session as a state
//! machine ([`Session`]) that turns the agent's lines into lines to send back and [`Event`]s. A
//! session is a thread: after a task it waits, and takes follow-up tasks with what it did in its
//! context ([`Session::follow_up`]).
//! The desktop app runs the process ([`crate::agent::Step::Harness`]); the browser has none.

use serde_json::{Value, json};

/// The ACP major version RustGIS speaks.
pub const PROTOCOL_VERSION: u64 = 1;

/// An agent RustGIS can start: natively in ACP mode, or through an ACP adapter.
#[derive(Debug, PartialEq, Eq)]
pub struct AgentSpec {
    /// The id the model names it by.
    pub id: &'static str,
    pub label: &'static str,
    /// The agent's own program on the PATH: present when the user has the agent.
    pub program: &'static str,
    /// Arguments that start it in ACP mode (a native ACP agent).
    pub acp_args: &'static [&'static str],
    /// The ACP adapter it needs instead: a program, or the npm package `npx` runs (pinned to a
    /// version, so nothing newer than what RustGIS was released with runs unasked).
    pub adapter: Option<Adapter>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Adapter {
    pub program: &'static str,
    pub package: &'static str,
}

/// The agents offered, in order.
pub const AGENTS: &[AgentSpec] = &[
    AgentSpec {
        id: "claude-code",
        label: "Claude Code",
        program: "claude",
        acp_args: &[],
        adapter: Some(Adapter { program: "claude-agent-acp", package: "@agentclientprotocol/claude-agent-acp@0.92.0" }),
    },
    AgentSpec {
        id: "codex",
        label: "Codex",
        program: "codex",
        acp_args: &[],
        adapter: Some(Adapter { program: "codex-acp", package: "@agentclientprotocol/codex-acp@2.2.2" }),
    },
    AgentSpec { id: "gemini", label: "Gemini CLI", program: "gemini", acp_args: &["--acp"], adapter: None },
    AgentSpec { id: "copilot", label: "GitHub Copilot CLI", program: "copilot", acp_args: &["--acp"], adapter: None },
    AgentSpec { id: "cursor", label: "Cursor", program: "cursor-agent", acp_args: &["acp"], adapter: None },
    AgentSpec { id: "opencode", label: "OpenCode", program: "opencode", acp_args: &["acp"], adapter: None },
    AgentSpec { id: "goose", label: "goose", program: "goose", acp_args: &["acp"], adapter: None },
];

impl AgentSpec {
    pub fn from_id(id: &str) -> Option<&'static AgentSpec> {
        AGENTS.iter().find(|a| a.id == id)
    }
}

/// What the agent is told before the task.
const BRIEF: &str = "You are working for RustGIS's AI assistant, on the map the user has open in RustGIS. RustGIS's MCP \
server `rustgis` acts on that map: list_layers, read_features, view_map, search_commands, run_command, search_tools, \
describe_tool, run_tool, search_docs, read_doc. Use your own tools for what RustGIS can't do (calling a web API, running a \
script). Save what you make in your working folder as files RustGIS reads (GeoJSON, CSV with coordinates, GeoPackage, \
GeoTIFF) and add them to the map with run_command layer.addData {\"path\": …}, so the user sees them. The user approves \
your actions in RustGIS. End with a short report of what you did and found (layer names, numbers, files): your last \
message is all the assistant sees.\n\nTask: ";

/// What a follow-up task is introduced with.
const FOLLOW_UP: &str = "Follow-up task from RustGIS's assistant (the same map; end with a short report again):\n\n";

/// Something the agent did or asked, for the app to show or answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// What it said (a whole message, not its pieces).
    Text(String),
    /// A tool it started (its title).
    Tool(String),
    /// Its plan: the entries.
    Plan(Vec<String>),
    /// It asks to do something: answer with [`Session::answer`].
    Permission(Permission),
    /// It finished a task: its report, and whether it failed. The thread then waits for a
    /// follow-up, unless the agent can't go on ([`Session::finished`]).
    Done { report: String, error: bool },
    /// Its process ended while it waited for a follow-up.
    Closed,
}

/// A permission request.
#[derive(Clone, Debug, PartialEq)]
pub struct Permission {
    /// The request's JSON-RPC id, to answer it.
    pub id: Value,
    /// What it wants to do.
    pub title: String,
    /// The tool's input in full, or a note that it is too long to show ([`MAX_DETAIL`]).
    pub detail: String,
    /// Whether `detail` is the whole input: one too long to show can only be declined (a cut
    /// text could hide the command or address approved).
    pub complete: bool,
    /// Whether "always allow" is one of the options.
    pub can_always: bool,
    /// Whether a once-only approval can be selected without granting persistence.
    pub can_once: bool,
    options: Vec<(String, String)>,
}

/// The user's answer to a [`Permission`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Allow,
    AllowAlways,
    Reject,
    /// The run is stopping.
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Initializing,
    Starting,
    Working,
    /// Between tasks: ready for a follow-up.
    Idle,
    /// It can't go on (it failed to start, or its process ended).
    Finished,
}

/// The initial request ids; later prompts and mode changes share a monotonic allocator.
const INIT: u64 = 0;
const NEW: u64 = 1;
const PROMPT: u64 = 2;

/// The agents' modes that act without asking (Codex, Claude Code, others').
const FULL_MODES: &[&str] = &["agent-full-access", "bypassPermissions", "full-access", "yolo"];
/// Their modes that work but ask before acting, in order of preference.
const ASKING_MODES: &[&str] = &["agent", "default", "ask", "code", "workspace-write"];
/// Their modes that can't do the work (read only, planning).
const IDLE_MODES: &[&str] = &["read-only", "plan"];

/// What the advertised, confirmed mode lets the agent do. Missing or malformed mode data
/// never establishes that the agent asks before acting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModeClass {
    Asking,
    Bypass,
    Idle,
    Unknown,
    None,
}

/// Readiness for the latest automatic-approval setting, rather than for an earlier request.
enum Ready {
    Now,
    Switch(String),
    Refuse,
}

/// Only a response with this identity can confirm this mode change.
#[derive(Debug)]
struct Negotiation {
    id: u64,
    mode: String,
    started: web_time::Instant,
}

/// One task's ACP session with an agent.
#[derive(Debug)]
pub struct Session {
    label: String,
    task: String,
    cwd: String,
    mcp: Vec<String>,
    phase: Phase,
    session_id: Option<String>,
    /// The id of the task's `session/prompt`: each follow-up gets the next one.
    prompt_id: u64,
    next_id: u64,
    /// A cancelled turn still owns its prompt until the agent answers it.
    stop_reason: Option<String>,
    /// The message being streamed, and the last whole one.
    text: String,
    last: String,
    /// Tool titles by id, for permission requests that name only the id.
    tools: Vec<(String, String)>,
    /// The user lets the agent act without asking: its full-access mode, when it has one.
    auto: bool,
    /// The agent's modes, and the one it's in.
    modes: Vec<String>,
    mode: Option<String>,
    /// The task held until its current mode negotiation succeeds.
    queued: Option<String>,
    asked_mode: Option<Negotiation>,
}

/// The longest message kept from an agent (bytes): the rest is cut.
pub const MAX_TEXT: usize = 1024 * 1024;
/// The most tool titles remembered (for permission requests that name only a call's id).
const MAX_TOOLS: usize = 1000;
/// The most entries of an agent's plan kept.
const MAX_PLAN_ENTRIES: usize = 1000;
/// Agent-controlled display labels stay short enough to read in permission cards.
const MAX_TITLE: usize = 200;
/// The most of a tool's input a permission card shows.
pub const MAX_DETAIL: usize = 4096;
const MAX_TOOL_ID: usize = 1024;

fn rpc(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn label(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

fn short(v: Option<&Value>, max: usize) -> String {
    match v {
        Some(Value::String(s)) => label(s, max),
        Some(v) => label(&v.to_string(), max),
        None => String::new(),
    }
}

impl Session {
    /// A session for `task`, working in `cwd`, with RustGIS's MCP server started by `mcp`
    /// (program and arguments), and the first line to send (`initialize`).
    /// `auto`: the user lets agents act without asking (their full-access mode).
    pub fn new(label: &str, task: &str, cwd: &str, mcp: Vec<String>, auto: bool) -> (Session, String) {
        let s = Session {
            label: label.into(),
            task: task.into(),
            cwd: cwd.into(),
            mcp,
            phase: Phase::Initializing,
            session_id: None,
            prompt_id: PROMPT,
            next_id: PROMPT + 1,
            stop_reason: None,
            text: String::new(),
            last: String::new(),
            tools: Vec::new(),
            auto,
            modes: Vec::new(),
            mode: None,
            queued: None,
            asked_mode: None,
        };
        let init = rpc(
            INIT,
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                // RustGIS reads and writes no files and runs no terminals for the agent: it uses its own.
                "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
                "clientInfo": {"name": "rustgis", "title": "RustGIS", "version": env!("CARGO_PKG_VERSION")}
            }),
        );
        (s, init)
    }

    /// One line from the agent: the lines to send back, and what to show or answer.
    pub fn line(&mut self, line: &str) -> (Vec<String>, Vec<Event>) {
        let (mut out, mut events) = (Vec::new(), Vec::new());
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else { return (out, events) };
        let method = msg.get("method").and_then(Value::as_str);
        match (method, msg.get("id")) {
            // A request from the agent.
            (Some(method), Some(id)) => {
                if method == "session/request_permission" {
                    events.extend(self.flush());
                    events.push(Event::Permission(self.permission(id.clone(), msg.get("params").unwrap_or(&Value::Null))));
                } else {
                    // RustGIS offers no files or terminals (see `initialize`).
                    out.push(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("RustGIS doesn't provide {method}")}}).to_string());
                }
            }
            (Some("session/update"), None) => events.extend(self.update(msg.pointer("/params/update").unwrap_or(&Value::Null), &mut out)),
            // A response to one of RustGIS's requests.
            (None, Some(id)) => self.response(id.as_u64(), &msg, &mut out, &mut events),
            _ => {}
        }
        (out, events)
    }

    /// Accept only the current negotiation or turn reply; a failed downgrade never releases work.
    fn response(&mut self, id: Option<u64>, msg: &Value, out: &mut Vec<String>, events: &mut Vec<Event>) {
        // A stale response cannot change the confirmed mode or release a held prompt.
        if self.asked_mode.as_ref().is_some_and(|asked| Some(asked.id) == id) {
            let Some(asked) = self.asked_mode.take() else { return };
            if msg.get("error").is_none() && msg.get("result").is_some_and(Value::is_object) {
                self.mode = Some(asked.mode);
                out.extend(self.release_prompt(false, events));
            } else {
                // An invalid response doesn't establish whether the requested change was
                // applied. In particular, don't retain an old asking mode after an upgrade.
                if !msg.get("error").is_some_and(Value::is_object) {
                    self.mode = None;
                }
                if self.queued.is_some() {
                    if self.auto {
                        // Upgrading is best effort: automatic approval still permits work.
                        // Downgrading with auto off is never best effort.
                        out.extend(self.release_prompt(false, events));
                    } else {
                        events.extend(self.refuse_mode("the agent refused the asking mode"));
                    }
                }
            }
            return;
        }
        // Ignore stale or unsolicited responses before interpreting success or failure.
        let expected = match self.phase {
            Phase::Initializing => id == Some(INIT),
            Phase::Starting => id == Some(NEW),
            Phase::Working => id == Some(self.prompt_id) && self.queued.is_none(),
            _ => false,
        };
        if !expected {
            return;
        }
        if let Some(e) = msg.get("error") {
            let why = e.get("message").and_then(Value::as_str).unwrap_or("an error").to_string();
            let explicit = why.to_lowercase();
            let auth = explicit.starts_with("authentication required")
                || explicit.starts_with("not authenticated")
                || explicit.starts_with("sign in required")
                || e.pointer("/data/type").and_then(Value::as_str) == Some("auth_required")
                || e.pointer("/data/code").and_then(Value::as_str) == Some("auth_required");
            let report = if auth {
                format!("{} needs you to sign in first: open it once in a terminal and sign in, then try again ({why}).", self.label)
            } else {
                format!("{}: {why}", self.label)
            };
            // A task that failed leaves the thread open; failing to start ends it.
            if self.phase == Phase::Working {
                events.extend(self.flush());
                events.extend(self.turn_done(report, true));
            } else {
                events.extend(self.finish_with(report, true));
            }
            return;
        }
        let result = msg.get("result").unwrap_or(&Value::Null);
        match (id, self.phase) {
            (Some(INIT), Phase::Initializing) => {
                let version = result.get("protocolVersion").and_then(Value::as_u64).unwrap_or(0);
                if version != PROTOCOL_VERSION {
                    let report = format!("{} speaks ACP version {version}; RustGIS speaks version {PROTOCOL_VERSION}.", self.label);
                    events.extend(self.finish_with(report, true));
                    return;
                }
                self.phase = Phase::Starting;
                let (command, args) = self.mcp.split_first().map_or(("", &[][..]), |(c, a)| (c.as_str(), a));
                out.push(rpc(
                    NEW,
                    "session/new",
                    json!({"cwd": self.cwd, "mcpServers": [{"name": "rustgis", "command": command, "args": args, "env": []}]}),
                ));
            }
            (Some(NEW), Phase::Starting) => {
                let Some(sid) = result.get("sessionId").and_then(Value::as_str) else {
                    events.extend(self.finish_with(format!("{} started no session.", self.label), true));
                    return;
                };
                self.session_id = Some(sid.to_string());
                self.phase = Phase::Working;
                self.modes = result
                    .pointer("/modes/availableModes")
                    .and_then(Value::as_array)
                    .and_then(|modes| modes.iter().map(|m| m.get("id")?.as_str().map(str::to_string)).collect::<Option<Vec<_>>>())
                    .unwrap_or_default();
                self.mode = result.pointer("/modes/currentModeId").and_then(Value::as_str).map(str::to_string);
                let prompt = rpc(
                    PROMPT,
                    "session/prompt",
                    json!({"sessionId": sid, "prompt": [{"type": "text", "text": format!("{BRIEF}{}", self.task)}]}),
                );
                self.queued = Some(prompt);
                out.extend(self.release_prompt(true, events));
            }
            (Some(id), Phase::Working) if id == self.prompt_id => {
                let stop = result.get("stopReason").and_then(Value::as_str).unwrap_or("end_turn");
                events.extend(self.flush());
                let (report, error) = if let Some(why) = self.stop_reason.take() {
                    (why, true)
                } else {
                    match stop {
                        "end_turn" => (self.last.clone(), false),
                        "cancelled" => (format!("{} was stopped.", self.label), true),
                        "refusal" => (format!("{} refused the task. {}", self.label, self.last), true),
                        _ => (format!("{} stopped early ({stop}). {}", self.label, self.last), true),
                    }
                };
                events.extend(self.turn_done(report, error));
            }
            _ => {}
        }
    }

    /// Classify only confirmed mode data from a valid advertised mode list.
    fn mode_class(&self) -> ModeClass {
        let Some(mode) = self.mode.as_deref().filter(|_| !self.modes.is_empty()) else { return ModeClass::None };
        if ASKING_MODES.contains(&mode) {
            ModeClass::Asking
        } else if FULL_MODES.contains(&mode) {
            ModeClass::Bypass
        } else if IDLE_MODES.contains(&mode) {
            ModeClass::Idle
        } else {
            ModeClass::Unknown
        }
    }

    /// Recompute readiness under the current setting. Automatic upgrades are attempted only
    /// before a task, so a refused optional upgrade doesn't prevent an auto-on task running.
    fn ready(&self, upgrade: bool) -> Ready {
        let offered = |modes: &[&str]| modes.iter().find(|m| self.modes.iter().any(|x| x == **m)).map(|m| (*m).to_string());
        if self.auto {
            return match offered(FULL_MODES).filter(|m| upgrade && Some(m.as_str()) != self.mode.as_deref()) {
                Some(mode) => Ready::Switch(mode),
                None => Ready::Now,
            };
        }
        if self.mode_class() == ModeClass::Asking {
            return Ready::Now;
        }
        match offered(ASKING_MODES) {
            Some(mode) => Ready::Switch(mode),
            None if self.mode_class() == ModeClass::Idle => Ready::Now,
            None => Ready::Refuse,
        }
    }

    /// Allocate distinct JSON-RPC ids for prompts and mode changes; exhaustion fails closed.
    fn request_id(&mut self) -> Option<u64> {
        let id = self.next_id;
        self.next_id = id.checked_add(1)?;
        Some(id)
    }

    /// Ask for a mode under a new identity, with at most one change in flight. A sent
    /// change can still be applied by the agent even if its response is ignored locally.
    fn switch_mode(&mut self, mode: String) -> Option<String> {
        if self.asked_mode.is_some() {
            return None;
        }
        let sid = self.session_id.clone()?;
        let id = self.request_id()?;
        self.asked_mode = Some(Negotiation { id, mode: mode.clone(), started: web_time::Instant::now() });
        Some(rpc(id, "session/set_mode", json!({"sessionId": sid, "modeId": mode})))
    }

    /// Only a fresh readiness check releases work. Even an already-safe current mode waits
    /// while a switch is in flight: that switch could still put the agent in bypass mode.
    fn release_prompt(&mut self, upgrade: bool, events: &mut Vec<Event>) -> Vec<String> {
        if self.queued.is_none() || self.asked_mode.is_some() {
            return Vec::new();
        }
        match self.ready(upgrade) {
            Ready::Now => self.queued.take().into_iter().collect(),
            Ready::Switch(mode) => match self.switch_mode(mode) {
                Some(line) => vec![line],
                None => {
                    events.extend(self.refuse_mode("a mode request could not be started"));
                    Vec::new()
                }
            },
            Ready::Refuse => {
                let why = if self.modes.is_empty() {
                    "the agent advertised no modes, so RustGIS can't confirm that it asks before acting"
                } else {
                    "RustGIS can't confirm that the agent asks before acting"
                };
                events.extend(self.refuse_mode(why));
                Vec::new()
            }
        }
    }

    /// Refuse this held task visibly, retaining the session for a safely negotiated follow-up.
    fn refuse_mode(&mut self, why: &str) -> Vec<Event> {
        self.queued = None;
        let mode = self.mode.as_deref().unwrap_or("unreported");
        self.turn_done(
            format!("{}: {why} (mode: {mode}). Change the agent's mode, or turn automatic approval on, then try again.", self.label),
            true,
        )
    }

    /// Cancel a running turn whose mode no longer meets the setting, and negotiate asking.
    /// Cancellation can't recall actions already performed, so keep waiting for the turn's reply.
    fn contain_running(&mut self, why: &str, events: &mut Vec<Event>) -> Vec<String> {
        if self.auto
            || self.phase != Phase::Working
            || self.queued.is_some()
            || self.stop_reason.is_some()
            || matches!(self.mode_class(), ModeClass::Asking | ModeClass::Idle)
        {
            return Vec::new();
        }
        let mut out: Vec<String> = self.cancel().into_iter().collect();
        let report = format!("{} was stopped: {why}.", self.label);
        self.stop_reason = Some(report.clone());
        events.extend(self.flush());
        events.push(Event::Text(format!("Stopping {}: {why}.", self.label)));
        // Do not overlap mode changes: an earlier change could otherwise apply after
        // the downgrade. Held follow-ups wait for it and then check readiness again.
        if self.asked_mode.is_none()
            && let Ready::Switch(mode) = self.ready(false)
        {
            out.extend(self.switch_mode(mode));
        }
        out
    }

    /// Apply automatic approval immediately. Turning it off cancels an unsafe running turn;
    /// held work is re-negotiated and never released by an earlier upgrade's response.
    /// Callers send the lines in order and display the returned events.
    pub fn set_auto(&mut self, auto: bool) -> (Vec<String>, Vec<Event>) {
        if self.auto == auto {
            return (Vec::new(), Vec::new());
        }
        self.auto = auto;
        let mut events = Vec::new();
        if self.queued.is_some() {
            let out = self.release_prompt(true, &mut events);
            return (out, events);
        }
        let out = self.contain_running("automatic approval was turned off", &mut events);
        (out, events)
    }

    /// Retire a session after an unanswered negotiation. A timed-out request can still
    /// change the agent's mode later, so this session must never receive another task.
    pub fn poll(&mut self) -> Vec<Event> {
        if !self.asked_mode.as_ref().is_some_and(|a| a.started.elapsed() >= web_time::Duration::from_secs(60)) {
            return Vec::new();
        }
        self.mode = None;
        self.finish_with(format!("{}: the mode request timed out. Start a new agent thread before trying again.", self.label), true)
    }

    /// Track mode notifications as well as replies: an auto-off turn entering an unsafe mode
    /// must be cancelled even when RustGIS didn't request that change.
    fn update(&mut self, u: &Value, out: &mut Vec<String>) -> Vec<Event> {
        let s = |k: &str| u.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        match u.get("sessionUpdate").and_then(Value::as_str) {
            Some("current_mode_update") => {
                self.mode = u.get("currentModeId").and_then(Value::as_str).map(str::to_string);
                let mut events = Vec::new();
                out.extend(self.contain_running("the agent entered a mode that doesn't confirm approval", &mut events));
                events
            }
            Some("agent_message_chunk") => {
                if let Some(t) = u.pointer("/content/text").and_then(Value::as_str) {
                    // A message past MAX_TEXT is cut there: the agent's output is untrusted.
                    let room = MAX_TEXT.saturating_sub(self.text.len());
                    let mut end = t.len().min(room);
                    while !t.is_char_boundary(end) {
                        end -= 1;
                    }
                    self.text.push_str(t.get(..end).unwrap_or(""));
                }
                Vec::new()
            }
            Some("tool_call") => {
                let mut events = self.flush();
                let title = short(u.get("title"), MAX_TITLE);
                let title = if title.is_empty() { short(u.get("kind"), MAX_TITLE) } else { title };
                // Only the latest titles are kept: a long run's calls don't pile up.
                if let Some(id) = u.get("toolCallId").and_then(Value::as_str).filter(|id| id.len() <= MAX_TOOL_ID) {
                    if self.tools.len() >= MAX_TOOLS {
                        self.tools.remove(0);
                    }
                    self.tools.push((id.to_string(), title.clone()));
                }
                events.push(Event::Tool(title));
                events
            }
            Some("tool_call_update") if s("status") == "failed" => {
                let title = self.tool_title(&s("toolCallId"));
                vec![Event::Tool(format!("{title} ✗"))]
            }
            Some("plan") => {
                let entries = u.get("entries").and_then(Value::as_array).into_iter().flatten();
                vec![Event::Plan(
                    entries
                        .take(MAX_PLAN_ENTRIES)
                        .filter_map(|e| e.get("content").and_then(Value::as_str).map(|s| label(s, MAX_TITLE)))
                        .collect(),
                )]
            }
            _ => Vec::new(),
        }
    }

    fn tool_title(&self, id: &str) -> String {
        self.tools.iter().rev().find(|(i, _)| i == id).map(|(_, t)| t.clone()).unwrap_or_else(|| label(id, MAX_TITLE))
    }

    fn permission(&self, id: Value, p: &Value) -> Permission {
        let call = p.get("toolCall").unwrap_or(&Value::Null);
        let title = if call.get("title").and_then(Value::as_str).is_some() {
            short(call.get("title"), MAX_TITLE)
        } else {
            self.tool_title(call.get("toolCallId").and_then(Value::as_str).unwrap_or(""))
        };
        let options: Vec<(String, String)> = p
            .get("options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|o| Some((o.get("optionId")?.as_str()?.to_string(), o.get("kind")?.as_str()?.to_string())))
            .collect();
        let raw = match call.get("rawInput") {
            None | Some(Value::Null) => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
        };
        let complete = raw.len() <= MAX_DETAIL;
        let detail = if complete { raw } else { format!("(an input too long to show in full: {} bytes; decline it)", raw.len()) };
        Permission {
            id,
            title,
            detail,
            complete,
            can_always: options.iter().any(|(_, k)| k == "allow_always"),
            can_once: options.iter().any(|(_, k)| k == "allow_once"),
            options,
        }
    }

    /// The reply to a permission request.
    pub fn answer(&self, p: &Permission, choice: Choice) -> String {
        let pick = |kinds: &[&str]| kinds.iter().find_map(|k| p.options.iter().find(|(_, kind)| kind == k)).map(|(id, _)| id.clone());
        let option = match choice {
            Choice::Allow => pick(&["allow_once"]),
            Choice::AllowAlways => pick(&["allow_always", "allow_once"]),
            Choice::Reject => pick(&["reject_once", "reject_always"]),
            Choice::Cancelled => None,
        };
        let outcome = match option {
            Some(id) => json!({"outcome": "selected", "optionId": id}),
            None => json!({"outcome": "cancelled"}),
        };
        json!({"jsonrpc": "2.0", "id": p.id, "result": {"outcome": outcome}}).to_string()
    }

    /// The notification that stops the agent's turn, once a session has started.
    pub fn cancel(&self) -> Option<String> {
        let sid = self.session_id.as_ref().filter(|_| self.phase == Phase::Working)?;
        Some(json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": sid}}).to_string())
    }

    /// A follow-up task for the thread after its last prompt finishes. It uses the same
    /// readiness gate as the first prompt; a refusal is returned as an event with no prompt.
    pub fn follow_up(&mut self, task: &str) -> Option<(Vec<String>, Vec<Event>)> {
        let sid = self.session_id.clone().filter(|_| self.phase == Phase::Idle)?;
        let id = self.request_id()?;
        self.phase = Phase::Working;
        self.prompt_id = id;
        self.last.clear();
        let prompt = rpc(
            self.prompt_id,
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type": "text", "text": format!("{FOLLOW_UP}{task}")}]}),
        );
        self.queued = Some(prompt);
        let mut events = Vec::new();
        let out = self.release_prompt(true, &mut events);
        Some((out, events))
    }

    /// The agent's process ended: its report, if it was working, or that the thread closed.
    pub fn exited(&mut self, ok: bool, errors: &str) -> Vec<Event> {
        match self.phase {
            Phase::Finished => return Vec::new(),
            Phase::Idle => {
                self.phase = Phase::Finished;
                return vec![Event::Closed];
            }
            _ => {}
        }
        let mut events = self.flush();
        let why = errors.trim();
        let report = if why.is_empty() {
            format!("{} stopped before finishing{}.", self.label, if ok { "" } else { " (it failed)" })
        } else {
            format!("{} stopped before finishing: {why}", self.label)
        };
        events.extend(self.finish_with(report, true));
        events
    }

    fn flush(&mut self) -> Vec<Event> {
        let t = std::mem::take(&mut self.text);
        if t.trim().is_empty() {
            return Vec::new();
        }
        self.last = t.trim().to_string();
        vec![Event::Text(self.last.clone())]
    }

    /// A task ended; the thread waits for a follow-up.
    fn turn_done(&mut self, report: String, error: bool) -> Vec<Event> {
        self.phase = Phase::Idle;
        self.queued = None;
        self.stop_reason = None;
        let report = if report.trim().is_empty() { format!("{} finished without a report.", self.label) } else { report };
        vec![Event::Done { report, error }]
    }

    /// End this session permanently, discarding held work and pending negotiations so
    /// late replies cannot revive it or authorize a follow-up.
    fn finish_with(&mut self, report: String, error: bool) -> Vec<Event> {
        self.phase = Phase::Finished;
        self.queued = None;
        self.asked_mode = None;
        self.stop_reason = None;
        let report = if report.trim().is_empty() { format!("{} finished without a report.", self.label) } else { report };
        vec![Event::Done { report, error }]
    }

    /// It can't go on: no follow-ups.
    pub fn finished(&self) -> bool {
        self.phase == Phase::Finished
    }

    /// It is between tasks, ready for a follow-up.
    pub fn idle(&self) -> bool {
        self.phase == Phase::Idle
    }
}

/// Where a program is on the PATH (with Windows' extensions: `.exe`, `.cmd`, `.bat`), if anywhere.
#[cfg(not(target_arch = "wasm32"))]
pub fn find_program(program: &str) -> Option<std::path::PathBuf> {
    find_program_in(program, &std::env::var_os("PATH")?, &std::env::current_dir().ok()?)
}

#[cfg(not(target_arch = "wasm32"))]
fn find_program_in(program: &str, path: &std::ffi::OsStr, cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
    std::env::split_paths(path).filter(|p| !p.as_os_str().is_empty()).find_map(|dir| {
        let dir = if dir.is_absolute() { dir } else { cwd.join(dir) };
        exts.iter().map(|e| dir.join(format!("{program}{e}"))).find(|p| p.is_absolute() && runnable(p))
    })
}

/// A file the shell would run: on Unix, one with an execute bit.
#[cfg(not(target_arch = "wasm32"))]
fn runnable(p: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// How to start `spec` in ACP mode on this computer: the agent's program in ACP mode, its
/// adapter, or `npx` running the adapter's package. `None` when the agent isn't installed, or
/// it needs an adapter and neither the adapter nor `npx` (Node.js) is found.
#[cfg(not(target_arch = "wasm32"))]
pub fn command(spec: &AgentSpec) -> Option<Vec<String>> {
    let program = find_program(spec.program)?;
    match &spec.adapter {
        None => Some(std::iter::once(program.display().to_string()).chain(spec.acp_args.iter().map(|a| a.to_string())).collect()),
        Some(a) => match find_program(a.program) {
            Some(p) => Some(vec![p.display().to_string()]),
            None => find_program("npx").map(|npx| vec![npx.display().to_string(), "-y".into(), a.package.into()]),
        },
    }
}

/// The agents installed here that can be started (with how), and those installed that need
/// Node.js for their adapter.
pub type Installed = (Vec<(&'static AgentSpec, Vec<String>)>, Vec<&'static AgentSpec>);

/// The agents installed here ([`Installed`]): a search of `PATH`, so callers keep the answer.
#[cfg(not(target_arch = "wasm32"))]
pub fn installed() -> Installed {
    let (mut ready, mut need_node) = (Vec::new(), Vec::new());
    for spec in AGENTS {
        match command(spec) {
            Some(c) => ready.push((spec, c)),
            None if find_program(spec.program).is_some() => need_node.push(spec),
            None => {}
        }
    }
    (ready, need_node)
}

#[cfg(test)]
mod mode_tests {
    use super::*;

    #[test]
    fn a_timed_out_mode_request_retires_the_session_even_between_turns() {
        for held in [true, false] {
            let (mut s, _) = Session::new("Agent", "Map it", "/w", vec![], !held);
            s.line(r#"{"id":0,"result":{"protocolVersion":1}}"#);
            s.line(r#"{"id":1,"result":{"sessionId":"s","modes":{"currentModeId":"agent-full-access","availableModes":[{"id":"agent"},{"id":"agent-full-access"}]}}}"#);
            if !held {
                s.set_auto(false);
                s.line(r#"{"id":2,"result":{"stopReason":"cancelled"}}"#);
                assert!(s.idle());
            }
            let asked = s.asked_mode.as_mut().unwrap();
            let id = asked.id;
            asked.started = asked.started.checked_sub(web_time::Duration::from_secs(61)).unwrap();
            let events = s.poll();
            assert!(events.iter().any(|e| matches!(e, Event::Done { error: true, report } if report.contains("timed out"))));
            assert!(s.finished() && s.follow_up("Try again").is_none());
            assert_eq!(s.line(&json!({"id": id, "result": {}}).to_string()), (vec![], vec![]));
            assert!(s.poll().is_empty());
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod lookup_tests {
    #[test]
    fn relative_path_entries_are_resolved_before_the_agents_work_directory_changes() {
        let root = tempfile::tempdir().unwrap();
        let tools = root.path().join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        let file = tools.join(if cfg!(windows) { "test-agent.exe" } else { "test-agent" });
        std::fs::write(&file, b"program").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Not executable: not found, as the shell wouldn't run it.
            std::fs::set_permissions(&file, PermissionsExt::from_mode(0o644)).unwrap();
            let path = std::env::join_paths([tools.as_path()]).unwrap();
            assert!(super::find_program_in("test-agent", &path, root.path()).is_none());
            std::fs::set_permissions(&file, PermissionsExt::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([std::path::Path::new(""), std::path::Path::new("tools")]).unwrap();
        let found = super::find_program_in("test-agent", &path, root.path()).unwrap();
        assert!(found.is_absolute());
        assert_eq!(std::fs::canonicalize(&found).unwrap(), std::fs::canonicalize(file).unwrap());
        let empty = std::env::join_paths([std::path::Path::new("")]).unwrap();
        let current = root.path().join(if cfg!(windows) { "dot-only.exe" } else { "dot-only" });
        std::fs::write(&current, b"program").unwrap();
        // Executable, so only the empty PATH entry keeps it from being found.
        #[cfg(unix)]
        std::fs::set_permissions(&current, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert!(super::find_program_in("dot-only", &empty, root.path()).is_none());
    }
}
