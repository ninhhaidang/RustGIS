//! A loopback relay: an MCP client drives the session of the desktop app that is already open.
//!
//! The app starts a listener with its `ui.mcpRelay` command (off until then), and
//! `rustgis-cli mcp --attach` connects to it. The relay is the same [`crate::mcp`] protocol as
//! `rustgis-cli mcp`, one JSON-RPC message per line, so there is no second command protocol:
//! every action reaches the open [`Session`] through [`Session::run`], one undo step per command,
//! exactly as the private-session server does.
//!
//! Safety: the listener binds 127.0.0.1 only ([`Relay::bind`] refuses any other address before
//! opening a socket). Each run draws a token from the operating system's random source and writes
//! it to a private file ([`token_path`]); a connection is dropped unless its first message is the
//! relay's `rustgis/attach` carrying that token. The attach's `access` flag then decides what that
//! connection allows; the existing [`Server::refusal`] policy still runs per call.
//!
//! Limits: connections, unauthenticated connections, queued messages, the first line before the
//! token is checked, and every read and write wait are bounded. A command whose reply does not
//! come in time is cancelled before the app can run it. Stopping the relay, or dropping its last
//! owner, closes the listener and every client and removes the token file.
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rustgis_engine::Session;
use rustgis_engine::policy::{AgentAccess, Effect, Effects, Outcome, Pending, Revocation, Scope};
use serde_json::Value;

use crate::framing::{self, MAX_LINE, Oversize, read_message_max};
use crate::mcp::{Server, expects_reply};

mod http;
pub use http::{HTTP_PATH, HTTP_PORT, HTTP_PORT_ENV, default_http_port, http_token, http_token_path};

/// The port `rustgis-cli mcp --attach` connects to unless it names another. (GeoLibre's runner
/// is 8766; the relay takes the next one.)
pub const DEFAULT_PORT: u16 = 8767;

/// The environment variable that changes the relay's default port, for the app and
/// `rustgis-cli mcp --attach` alike (a second app, or a port another program holds).
pub const PORT_ENV: &str = "RUSTGIS_MCP_PORT";

/// The port the relay listens on, and `--attach` connects to, unless one is named:
/// [`PORT_ENV`] when it's set, else [`DEFAULT_PORT`]. A value that isn't a port is an error
/// naming the variable, not a quiet fall back to the default (which may be another app's).
pub fn default_port() -> Result<u16, String> {
    port_from(std::env::var(PORT_ENV).ok().as_deref())
}

/// The port [`PORT_ENV`]'s value names, or [`DEFAULT_PORT`] when it's unset or empty.
fn port_from(value: Option<&str>) -> Result<u16, String> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(DEFAULT_PORT),
        Some(v) => v
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or_else(|| format!("{PORT_ENV}={v} is not a port number (1 to 65535); fix or unset it")),
    }
}

/// The shortest and longest a client may ask a request to wait for the app (`timeout_s` in its
/// attach).
const MIN_WAIT: Duration = Duration::from_secs(5);
const MAX_WAIT: Duration = Duration::from_secs(600);
/// A request waiting while the app has run no frame this long gets an early "busy" reply instead
/// of waiting out its timeout: a modal dialog (a file picker) or a long tool holds the app.
const BUSY_AFTER: Duration = Duration::from_secs(10);
/// The error code of that early reply: apart from -32001 (the queue is full, try again soon), so
/// a client can tell a stalled app from a busy queue.
const BUSY_CODE: i64 = -32004;
/// The endpoint file the app publishes for clients and setup scripts ([`Relay::publish_endpoint`]).
pub const ENDPOINT: &str = "endpoint.json";

/// The most bytes in a line before the token is checked: an attach is far smaller than this.
const MAX_PREAUTH_LINE: u64 = 4096;
/// Open connections, authenticated or not.
const MAX_CONNECTIONS: usize = 16;
/// Connections that have not attached yet.
const MAX_PREAUTH_CONNECTIONS: usize = 4;
/// Messages waiting for the app's frame.
const MAX_QUEUE: usize = 64;
/// How long a new connection may take to send its attach.
const PREAUTH_TIMEOUT: Duration = Duration::from_secs(5);
/// How long an attached client may stay silent before the relay closes it.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// How long a reply may take to be written to a client.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a request waits for the app to run it; after that it is cancelled.
const REPLY_WAIT: Duration = Duration::from_secs(60);
/// How long a request waits for the user's approval before it is cancelled.
const APPROVAL_WAIT: Duration = Duration::from_secs(15 * 60);
/// How often the listener, a connection waiting for a reply, and a connection waiting for its
/// client's next line check whether the relay was stopped.
const STOP_POLL: Duration = Duration::from_millis(25);

/// The connection message: `{"jsonrpc":"2.0","method":"rustgis/attach","params":{"access":bool,
/// "token":"…"}}`. It carries no commands; it proves the client is this user's own (`token`,
/// read from [`token_path`]) and says what its `--yes` allows, before its first call.
const ATTACH: &str = "rustgis/attach";

/// The states of a queued request. The connection and the app race for it: the first to move
/// it out of `PENDING` decides. A cancelled request never runs.
const PENDING: u8 = 0;
const RUNNING: u8 = 1;
const CANCELLED: u8 = 2;

/// A queued message: its line, what its connection allows, where its reply goes, and its state.
struct Request {
    plans: Vec<(String, Pending)>,
    /// A positive preflight is cached while this exact message waits for a decision.
    approval_required: bool,
    /// Set while it waits for the user's approval, so its connection waits for the user
    /// ([`APPROVAL_WAIT`]) rather than for the app ([`REPLY_WAIT`]).
    held: Arc<AtomicBool>,
    id: u64,
    decision: Arc<AtomicU8>,
    line: String,
    /// The token its connection attached with and whether it asked for file and network access
    /// (`--yes`): what it may do is decided when it runs, so a revoked grant or the app's changed
    /// setting applies to requests already queued.
    token: String,
    asked: bool,
    reply: SyncSender<Option<String>>,
    state: Arc<AtomicU8>,
}

/// A RustGIS-side approval, scoped to one exact MCP message and one run's grant.
#[derive(Clone)]
pub struct Approval {
    pub id: u64,
    pub summary: String,
    /// Sanitized engine cards, including structured details for translated UI presentation.
    pub reviews: Vec<rustgis_engine::policy::Review>,
    token: String,
    state: Arc<AtomicU8>,
    decision: Arc<AtomicU8>,
}

/// State shared by the owner, the accept loop and the connections. It never refers back to the
/// owner, so dropping the last [`Relay`] is what shuts the relay down.
struct Shared {
    stopped: AtomicBool,
    requests: Mutex<VecDeque<Request>>,
    /// The run's attach token: only a connection that sends it in its first message stays open.
    token: String,
    /// The token file this run wrote; removed when the relay stops.
    token_file: PathBuf,
    /// What to run when a message is queued or a client attaches or leaves, so the UI thread
    /// draws a frame to run the message or repaint the client count.
    wake: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Open connections, by id, so stopping the relay can shut their sockets.
    streams: Mutex<HashMap<u64, TcpStream>>,
    next_id: AtomicU64,
    /// Connections that have not attached yet.
    preauth: AtomicUsize,
    /// Authenticated connections.
    attached: AtomicUsize,
    /// How long a request waits for the app before it is cancelled, unless its client asked for
    /// another time.
    reply_wait: Duration,
    /// After how long without a frame a waiting request is told the app is busy.
    busy_after: Duration,
    /// When the app last ran [`Relay::poll`].
    last_poll: Mutex<Instant>,
    /// The endpoint file this relay published, removed when it stops.
    endpoint: Mutex<Option<PathBuf>>,
    /// The port, naming the grants' token files.
    port: u16,
    /// The app lets agents attached with the relay's own token use files and the network, whether
    /// or not they asked (`--yes`): the user turned agents on in the app ([`Relay::set_allow_access`]).
    allow_access: AtomicBool,
    /// Agents attached with the relay's own token may run any command ([`Relay::set_full_access`]).
    full_access: AtomicBool,
    /// Run-scoped tokens and what each allows ([`Relay::grant`]), by token.
    grants: Mutex<HashMap<String, (bool, PathBuf)>>,
    /// Local-agent grants require RustGIS approval unless their explicit auto setting is on.
    reviewed: Mutex<HashMap<String, bool>>,
    approvals: Mutex<Vec<Approval>>,
    /// The connections attached with a grant, by connection id, so revoking it closes them.
    granted: Mutex<HashMap<u64, String>>,
    grant_ids: AtomicU64,
    /// This run's own id (random, not secret), naming its grants' files: a grant left over from an
    /// earlier run on the same port can't remove a file of this one.
    run_id: String,
    /// The port of MCP over HTTP beside the relay, while it runs ([`Relay::serve_http`]).
    http_port: Mutex<Option<u16>>,
    /// Each agent (by the token it attached with): its name in the undo history and the layer
    /// it last made active, kept apart from the user's selection.
    agents: Mutex<HashMap<String, AgentState>>,
    /// Whether the app shows the map, so a snapshot can capture the screen ([`Relay::set_screen`]).
    screen: AtomicBool,
    /// The screen snapshot the app is capturing ([`Relay::screen_request`]); nothing else runs
    /// until it is done.
    shot: Mutex<Option<Shot>>,
    /// A desktop archive command holds its complete message, including any ordered batch suffix.
    archive: Mutex<Option<Archive>>,
}

/// What the relay keeps for one agent between its calls.
struct AgentState {
    /// `agent 1 (relay run …)`, `agent 2 (…)`…, in the order they first called: the
    /// [`Session::actor`] of its steps.
    name: String,
    /// The layer its commands act on when they name none.
    active: Option<u32>,
}

/// What a screen snapshot asks of the app ([`Relay::screen_request`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenRequest {
    /// The longest to wait for the map to finish drawing.
    pub wait: Duration,
    /// Whether the map's controls, scale bar and sketches show (`overlays`, default true).
    pub overlays: bool,
}

/// A `snapshot` request waiting for the app to capture its window.
struct Shot {
    req: Request,
    scope: Scope,
    completion: rustgis_engine::policy::Deferred,
    /// The request's JSON-RPC id and the tool's arguments.
    id: Value,
    args: Value,
    /// How long the app may wait for the map to finish drawing.
    wait: Duration,
}

struct Archive {
    req: Request,
    batch: Box<crate::mcp::ArchiveBatch>,
    started: Instant,
    /// The project and selected map at this pause, after the preceding batch segment ran.
    opened: u64,
    map: u64,
}

impl Shared {
    /// Wakes the UI thread so it draws a frame to run queued messages or repaint the client
    /// count. Does nothing when the app never set a callback with [`Relay::set_wake`].
    fn notify(&self) {
        if let Some(wake) = lock(&self.wake).as_ref() {
            wake();
        }
    }

    /// Stops the listener, closes every client, drops every queued request (its connection sees
    /// the closed reply channel) and removes this run's token file. Idempotent.
    fn shutdown(&self) {
        if self.stopped.swap(true, SeqCst) {
            return;
        }
        let streams: Vec<TcpStream> = lock(&self.streams).drain().map(|(_, s)| s).collect();
        for stream in streams {
            let _ = stream.shutdown(Shutdown::Both);
        }
        lock(&self.approvals).clear();
        if let Some(shot) = lock(&self.shot).take() {
            shot.req.state.store(CANCELLED, SeqCst);
        }
        if let Some(archive) = lock(&self.archive).take() {
            archive.req.state.store(CANCELLED, SeqCst);
        }
        // Pending requests are cancelled before they are dropped, so none can run after stop.
        for req in lock(&self.requests).drain(..) {
            let _ = req.state.compare_exchange(PENDING, CANCELLED, SeqCst, SeqCst);
        }
        let _ = std::fs::remove_file(&self.token_file);
        *lock(&self.http_port) = None;
        if let Some(path) = lock(&self.endpoint).take() {
            // Only if it is still this relay's: another app may have published its own since.
            let ours = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).is_some_and(|v| {
                v.get("port").and_then(Value::as_u64) == Some(u64::from(self.port))
                    && v.get("pid").and_then(Value::as_u64) == Some(u64::from(std::process::id()))
            });
            if ours {
                let _ = std::fs::remove_file(&path);
            }
        }
        for (_, (_, file)) in lock(&self.grants).drain() {
            let _ = std::fs::remove_file(&file);
        }
        self.notify();
    }

    /// Whether a connection attached with `token` may use files and the network, given whether
    /// it asked (`--yes`): the relay's own token as it asked, or always when the app allows it; a
    /// grant as the grant says. `None` for a token that isn't (or is no longer) valid.
    fn access(&self, token: &str, asked: bool) -> Option<bool> {
        if same_token(token, &self.token) {
            return Some(asked || self.allow_access.load(SeqCst));
        }
        lock(&self.grants).iter().find(|(t, _)| same_token(token, t)).map(|(_, (access, _))| *access)
    }
}

/// A run-scoped token ([`Relay::grant`]): its file, for `rustgis-cli mcp --attach PORT
/// --token-file FILE`. Dropping it revokes the token, removes the file and closes the
/// connections that attached with it.
pub struct Grant {
    token: String,
    file: PathBuf,
    shared: std::sync::Weak<Shared>,
}

impl Grant {
    /// Updates the opt-in automatic approval policy for this run only.
    pub fn set_auto(&self, auto: bool) {
        if let Some(shared) = self.shared.upgrade()
            && let Some(value) = lock(&shared.reviewed).get_mut(&self.token)
        {
            *value = auto;
        }
    }

    pub fn pending(&self) -> Vec<Approval> {
        let Some(shared) = self.shared.upgrade() else { return Vec::new() };
        lock(&shared.approvals)
            .iter()
            .filter(|a| a.token == self.token && a.state.load(SeqCst) == PENDING && a.decision.load(SeqCst) == PENDING)
            .cloned()
            .collect()
    }

    /// The id belongs to this grant, and an expired/revoked request can never be revived.
    pub fn decide(&self, id: u64, allow: bool) {
        let Some(shared) = self.shared.upgrade() else { return };
        for a in lock(&shared.approvals).iter().filter(|a| a.token == self.token && a.id == id && a.state.load(SeqCst) == PENDING) {
            let _ = a.decision.compare_exchange(PENDING, if allow { RUNNING } else { CANCELLED }, SeqCst, SeqCst);
        }
        shared.notify();
    }

    /// The token's file, private to this user.
    pub fn token_file(&self) -> &std::path::Path {
        &self.file
    }
}

impl Drop for Grant {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
        let Some(shared) = self.shared.upgrade() else { return };
        lock(&shared.grants).remove(&self.token);
        lock(&shared.reviewed).remove(&self.token);
        lock(&shared.approvals).retain(|a| a.token != self.token);
        let ids: Vec<u64> = lock(&shared.granted).iter().filter(|(_, t)| **t == self.token).map(|(id, _)| *id).collect();
        let streams = lock(&shared.streams);
        for id in ids {
            if let Some(stream) = streams.get(&id) {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
    }
}

/// The owner of a running listener. The last [`Relay`] clone that drops stops the relay.
struct Owner {
    shared: Arc<Shared>,
    port: u16,
    /// The folder of the audit log the calls are written to ([`Relay::set_audit_dir`]).
    audit: Mutex<Option<PathBuf>>,
    /// The last tool call an attached agent ran ([`Relay::last_action`]).
    last: Mutex<Option<Action>>,
}

/// A tool call an attached agent ran, for the app to show while the user watches.
#[derive(Clone, Debug)]
pub struct Action {
    /// Short: the command id (`layer.addSample`), `gp.run <tool>` or the tool's name.
    pub label: String,
    /// The call with its parameters, cut to a line ([`crate::tools::summary`]).
    pub detail: String,
    /// Whether it succeeded.
    pub ok: bool,
    /// When it ran.
    pub at: Instant,
}

impl Drop for Owner {
    /// Stops the relay when its last owner drops: the listener closes, clients disconnect,
    /// queued requests are cancelled unrun, and the token file is removed.
    fn drop(&mut self) {
        self.shared.shutdown();
    }
}

/// A running listener. Messages queue for [`Relay::poll`], which runs them on the thread that
/// owns the [`Session`] (the app's UI thread), so the session never leaves its owner. Clones
/// share one listener: dropping a clone does nothing, and [`Relay::stop`] stops it for all.
#[derive(Clone)]
pub struct Relay {
    owner: Arc<Owner>,
}

impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay").field("port", &self.owner.port).finish()
    }
}

/// The folder that holds the run tokens. `$XDG_RUNTIME_DIR/rustgis` when that is an absolute
/// path to a folder (the per-user runtime folder); otherwise a folder named for the user in the
/// system's temporary folder. [`token_path`] and [`read_token`] use the same folder.
pub fn token_dir() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute() && p.is_dir()) {
        return runtime.join("rustgis");
    }
    let user: String = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let user = if user.is_empty() { "user".to_string() } else { user };
    std::env::temp_dir().join(format!("rustgis-mcp-{user}"))
}

/// The token file for `port`, in [`token_dir`]. The relay writes it when it starts (readable by
/// this user only) and removes it when it stops; `rustgis-cli mcp --attach` reads it through
/// [`read_token`], so no token ever travels through a command line, an argument or the clipboard.
pub fn token_path(port: u16) -> PathBuf {
    token_dir().join(format!("{port}.token"))
}

/// Reads the token the running relay on `port` wrote, for `rustgis-cli mcp --attach`. The folder
/// and file must be this user's: a regular file, not a symlink, both owned by the user running
/// this process, and private to that user (no group or other access).
pub fn read_token(port: u16) -> Result<String, String> {
    read_token_file(&token_path(port))
}

/// Reads a token file the relay wrote: its own ([`token_path`]) or a grant's
/// ([`Grant::token_file`]), with the same checks as [`read_token`].
pub fn read_token_file(path: &std::path::Path) -> Result<String, String> {
    let hint = "check the token file path (--token-file) and whether its relay or grant is still active";
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e} ({hint})", path.display()))?;
    if !meta.is_file() || meta.len() > 1024 {
        return Err(format!("{}: not a token file; refused", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| std::path::Path::new("."));
        let dir_meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let me = rustix::process::geteuid().as_raw();
        if !dir_meta.is_dir() || dir_meta.uid() != me || meta.uid() != me {
            return Err(format!("{}: not owned by this user; refused", path.display()));
        }
        if dir_meta.permissions().mode() & 0o077 != 0 || meta.permissions().mode() & 0o077 != 0 {
            return Err(format!("{}: readable by others; refused", path.display()));
        }
    }
    let token = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let token = token.trim();
    if token.is_empty() {
        return Err(format!("{}: holds no token ({hint})", path.display()));
    }
    Ok(token.to_string())
}

/// Makes sure [`token_dir`] exists as a private folder owned by the user running this process,
/// and returns it. A folder that is a symlink, is not a folder, belongs to another user or is
/// readable by anyone else is refused, not repaired. The file bridge keeps its files here too.
pub(crate) fn private_dir() -> Result<PathBuf, String> {
    let dir = token_dir();
    // Only Unix sets a mode on the builder; elsewhere it stays as created.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    }
    let meta = std::fs::symlink_metadata(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{}: not a folder; refused", dir.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(format!("{}: owned by another user; refused", dir.display()));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(format!("{}: readable by others; it must be private (mode 0700)", dir.display()));
        }
    }
    Ok(dir)
}

/// A fresh token for one relay run: 16 bytes from the operating system's random source, as 32
/// hex characters.
fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| format!("the MCP relay could not read the operating system's random source: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Writes the run's token for `port` into the private folder, readable by this user only, and
/// returns its path. A previous run's file (or a symlink in its place) is replaced, never
/// written through.
fn write_token_file(port: u16, token: &str) -> Result<PathBuf, String> {
    write_private(&format!("{port}.token"), token)
}

/// Writes `token` to the file `name` in the private folder, readable by this user only.
fn write_private(name: &str, token: &str) -> Result<PathBuf, String> {
    let dir = private_dir()?;
    let path = dir.join(name);
    // `remove_file` removes a symlink itself, not what it points to; `create_new` then never
    // follows anything that is left.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e} (a token file this user cannot remove; remove it to start the MCP relay)", path.display())),
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let file_uid = f.metadata().map_err(|e| format!("{}: {e}", path.display()))?.uid();
        if file_uid != rustix::process::geteuid().as_raw() {
            let _ = std::fs::remove_file(&path);
            return Err(format!("{}: not owned by this user; refused", path.display()));
        }
    }
    let written = f.write_all(token.as_bytes()).and_then(|()| f.write_all(b"\n"));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&path);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(path)
}

/// Locks a relay mutex, recovering the contents when a previous holder panicked: a poisoned
/// mutex would otherwise stop the listener, so the relay keeps its guarded state and runs on.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Relay {
    /// Binds `addr`. Anything but a loopback address is refused before a socket is opened, so a
    /// relay can never be reached from another computer.
    pub fn bind(addr: SocketAddr) -> Result<Self, String> {
        Self::bind_with(addr, REPLY_WAIT, BUSY_AFTER)
    }

    /// Binds 127.0.0.1:`port`; 0 asks the system for a free port.
    pub fn start(port: u16) -> Result<Self, String> {
        Self::bind(SocketAddr::from(([127, 0, 0, 1], port)))
    }

    /// [`Relay::bind`] with the time a request waits for the app before it is cancelled.
    fn bind_with(addr: SocketAddr, reply_wait: Duration, busy_after: Duration) -> Result<Self, String> {
        if !addr.ip().is_loopback() {
            return Err(format!("{addr}: the MCP relay listens on the loopback interface only"));
        }
        let listener = TcpListener::bind(addr).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                format!("{addr}: the port is in use (another RustGIS, or another program); set {PORT_ENV} to another port")
            } else {
                format!("{addr}: {e}")
            }
        })?;
        let port = listener.local_addr().map_err(|e| format!("{addr}: {e}"))?.port();
        listener.set_nonblocking(true).map_err(|e| format!("{addr}: {e}"))?;
        // The token is written before the accept loop starts, so no client can attach before it
        // exists. If anything below fails, the owner that holds the token file drops and removes it.
        let token = random_token()?;
        let token_file = write_token_file(port, &token)?;
        let shared = Arc::new(Shared {
            stopped: AtomicBool::new(false),
            requests: Mutex::new(VecDeque::new()),
            token,
            token_file,
            wake: Mutex::new(None),
            streams: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            preauth: AtomicUsize::new(0),
            attached: AtomicUsize::new(0),
            reply_wait,
            busy_after,
            last_poll: Mutex::new(Instant::now()),
            endpoint: Mutex::new(None),
            port,
            allow_access: AtomicBool::new(false),
            full_access: AtomicBool::new(false),
            grants: Mutex::new(HashMap::new()),
            reviewed: Mutex::new(HashMap::new()),
            approvals: Mutex::new(Vec::new()),
            granted: Mutex::new(HashMap::new()),
            grant_ids: AtomicU64::new(0),
            run_id: random_token()?.chars().take(8).collect(),
            http_port: Mutex::new(None),
            agents: Mutex::new(HashMap::new()),
            screen: AtomicBool::new(false),
            shot: Mutex::new(None),
            archive: Mutex::new(None),
        });
        let owner = Arc::new(Owner { shared, port, audit: Mutex::new(None), last: Mutex::new(None) });
        let accepting = Arc::clone(&owner.shared);
        std::thread::Builder::new()
            .name("rustgis-mcp-relay".into())
            .spawn(move || accept_loop(listener, accepting))
            .map_err(|e| format!("the MCP relay could not start: {e}"))?;
        Ok(Self { owner })
    }

    /// The port the listener is on (what `start(0)` chose).
    pub fn port(&self) -> u16 {
        self.owner.port
    }

    /// Whether the listener is still running (false after [`Relay::stop`] or the last owner's drop).
    pub fn is_running(&self) -> bool {
        !self.owner.shared.stopped.load(SeqCst)
    }

    /// How many authenticated clients are attached right now. Unauthenticated connections are
    /// not counted.
    pub fn attached_clients(&self) -> usize {
        self.owner.shared.attached.load(SeqCst)
    }

    /// Writes a request and how it went to the audit log, if there is one.
    fn audit(&self, line: &str, reply: Option<&str>, took: Duration) {
        // Out of the lock first, so changing the folder never waits on the disk.
        let dir = lock(&self.owner.audit).clone();
        if let Some(dir) = dir {
            crate::audit::record(&dir, "mcp", line, reply, took);
        }
    }

    /// Writes every tool call agents make to the audit log in `dir` ([`crate::audit`]), or to
    /// none. The desktop app keeps it in [`crate::audit::dir`].
    pub fn set_audit_dir(&self, dir: Option<PathBuf>) {
        *lock(&self.owner.audit) = dir;
    }

    /// Publishes where the relay is, for clients and setup scripts: [`ENDPOINT`] in the private
    /// folder, written whole (a temporary file renamed) and removed when the relay stops. It holds
    /// the host, port, process id, start time, app version, a schema number and the token file's
    /// path (not the token). Returns its path.
    pub fn publish_endpoint(&self) -> Result<PathBuf, String> {
        self.publish_endpoint_as(ENDPOINT)
    }

    /// [`Relay::publish_endpoint`] under another file name (tests, beside a running app's).
    fn publish_endpoint_as(&self, name: &str) -> Result<PathBuf, String> {
        let shared = &self.owner.shared;
        let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let text = serde_json::json!({
            "schema": 1,
            "host": "127.0.0.1",
            "port": shared.port,
            "url": format!("tcp://127.0.0.1:{}", shared.port),
            "pid": std::process::id(),
            "started_at": crate::chatgpt::rfc3339(i64::try_from(started).unwrap_or(0)),
            "version": env!("CARGO_PKG_VERSION"),
            "token_file": shared.token_file,
            "attach": format!("rustgis-cli mcp --attach {}", shared.port),
            // MCP over HTTP, once it runs (the file is written again when it starts).
            "http_url": lock(&shared.http_port).map(|p| format!("http://127.0.0.1:{p}{}", http::HTTP_PATH)),
            "http_token_file": lock(&shared.http_port).and(http_token_path()),
        })
        .to_string();
        let tmp = write_private(&format!(".{name}.{}", std::process::id()), &text)?;
        let path = tmp.with_file_name(name);
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })?;
        *lock(&shared.endpoint) = Some(path.clone());
        if shared.stopped.load(SeqCst) {
            let _ = std::fs::remove_file(&path);
            return Err("the MCP relay has stopped".into());
        }
        Ok(path)
    }

    /// Tells the relay whether the app shows the map now, so a snapshot can capture the screen.
    /// The app calls it every frame before [`Relay::poll`].
    pub fn set_screen(&self, shown: bool) {
        self.owner.shared.screen.store(shown, SeqCst);
    }

    /// A screen snapshot an agent asked for: the app captures its window, then calls
    /// [`Relay::finish_screen`].
    pub fn screen_request(&self) -> Option<ScreenRequest> {
        lock(&self.owner.shared.shot)
            .as_ref()
            .map(|shot| ScreenRequest { wait: shot.wait, overlays: shot.args.get("overlays").and_then(Value::as_bool).unwrap_or(true) })
    }

    /// Answers the screen snapshot with the map as the window showed it (a PNG), and whether the
    /// map had finished drawing; or, when it couldn't be captured, draws it offscreen with the
    /// reason as its note.
    pub fn finish_screen(&self, s: &mut Session, shot_png: Result<Vec<u8>, String>, settled: bool) {
        let Some(shot) = lock(&self.owner.shared.shot).take() else { return };
        if shot.req.state.load(SeqCst) != RUNNING {
            return;
        }
        // A last resort, as for the messages `poll` runs: a panic answers with an error instead
        // of breaking the app.
        let req = shot.req;
        let scope = shot.scope;
        let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            s.finish_deferred(shot.completion, |s| match shot_png {
                Ok(png) => crate::mcp::snapshot_result_checked(s, &png, "screen", settled, &shot.args, None, &scope),
                Err(why) => crate::mcp::render_snapshot_checked(s, &shot.args, Some(&why), &scope),
            })?
        }))
        .unwrap_or_else(|_| Err("the snapshot failed unexpectedly".into()));
        let result = answer.unwrap_or_else(|e| serde_json::json!({"content": [{"type": "text", "text": e}], "isError": true}));
        let reply = serde_json::json!({"jsonrpc": "2.0", "id": shot.id, "result": result}).to_string();
        if let Some(a) = action(&req.line, Some(&reply)) {
            *lock(&self.owner.last) = Some(a);
        }
        self.audit(&req.line, Some(&reply), Duration::ZERO);
        let _ = req.reply.send(Some(reply));
        self.owner.shared.notify();
    }
    /// Takes the already-admitted archive read once. The desktop uses its single read worker;
    /// dropping or recreating the invocation is never a native redispatch.
    pub fn take_archive_read(&self) -> Option<rustgis_engine::cmd::reads::ReadAhead> {
        lock(&self.owner.shared.archive).as_mut()?.batch.take_read()
    }

    /// Starts the reply's change report at the moment the admitted command actually runs (or is
    /// known never to run), so work the queue ran meanwhile is not attributed to the agent.
    pub fn begin_archive_landing(&self, s: &Session) {
        if let Some(archive) = lock(&self.owner.shared.archive).as_mut() {
            archive.batch.begin_landing(s);
        }
    }

    /// Completes the real archive command before running its batch suffix or issuing its reply
    /// and audit. A suffix may resume only in the project and map selected at its latest pause.
    pub fn finish_archive_read(&self, s: &mut Session, result: Result<Value, String>) {
        self.complete_archive_read(s, result, None);
    }

    /// Ends the admitted archive command for good because RustGIS is closing: `why` answers it
    /// and every remaining request of its message, none of which runs, and the slot is cleared.
    pub fn abandon_archive_read(&self, s: &mut Session, why: String) {
        self.complete_archive_read(s, Err(why.clone()), Some(&why));
    }

    /// [`Relay::finish_archive_read`], or with `abandon` set the suffix is cancelled with that error.
    fn complete_archive_read(&self, s: &mut Session, result: Result<Value, String>, abandon: Option<&str>) {
        let Some(archive) = lock(&self.owner.shared.archive).take() else { return };
        let Archive { req, batch, started, opened, map } = archive;
        // Check before entering the agent: its remembered layer ID is only map-local, and a
        // stale continuation must not change the current actor, selection or stored agent state.
        let same_context = abandon.is_none() && s.opened == opened && s.map().instance_id() == map;
        let layers = same_context.then(|| self.enter_agent(s, &req.token));
        let step = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if same_context {
                (*batch).finish(s, result)
            } else {
                (*batch).cancel_remaining(s, result, abandon.unwrap_or("archive batch context changed; remaining request cancelled"))
            }
        }))
        .unwrap_or_else(|_| crate::mcp::ArchiveStep::Done(Some(internal_error(&req.line)), false));
        let (out, picked) = match step {
            crate::mcp::ArchiveStep::Read(batch) => {
                if let Some(layers) = layers {
                    self.leave_agent(s, &req.token, layers, batch.picked());
                }
                *lock(&self.owner.shared.archive) = Some(Archive { req, batch, started, opened: s.opened, map: s.map().instance_id() });
                self.owner.shared.notify();
                return;
            }
            crate::mcp::ArchiveStep::Done(out, picked) => (out, picked),
        };
        if let Some(layers) = layers {
            self.leave_agent(s, &req.token, layers, picked);
        }
        self.audit(&req.line, out.as_deref(), started.elapsed());
        if let Some(action) = action(&req.line, out.as_deref()) {
            *lock(&self.owner.last) = Some(action);
        }
        let _ = req.reply.send(out);
        self.owner.shared.notify();
    }

    /// The last tool call an attached agent ran, if any has run since the relay started.
    pub fn last_action(&self) -> Option<Action> {
        lock(&self.owner.last).clone()
    }

    /// Stops the listener: it closes, every client is disconnected, queued messages are dropped
    /// unrun, and the token file is removed. Idempotent.
    pub fn stop(&self) {
        self.owner.shared.shutdown();
    }

    /// What runs when a message is queued, or a client attaches or leaves: [`Relay::poll`] only
    /// runs messages on a frame the UI thread draws, and in reactive mode egui draws none while
    /// the window is idle, so the app passes its `request_repaint` here.
    pub fn set_wake(&self, wake: Box<dyn Fn() + Send + Sync>) {
        *lock(&self.owner.shared.wake) = Some(wake);
    }

    /// Lets agents attached with the relay's own token use files and the network without asking
    /// (`--yes`), or goes back to what each asked. The desktop app allows it while the user has
    /// agents turned on. It applies to their next call.
    pub fn set_allow_access(&self, allow: bool) {
        self.owner.shared.allow_access.store(allow, SeqCst);
    }

    /// Lets agents attached with the relay's own token run any command, the user's own automation
    /// included (scripts, tasks, models, plugins), or goes back to the usual refusals. The user's
    /// explicit opt-in in the app; agents the app starts itself keep their grants' rules. It
    /// applies to their next call.
    pub fn set_full_access(&self, on: bool) {
        self.owner.shared.full_access.store(on, SeqCst);
    }

    /// Whether agents have full access ([`Relay::set_full_access`]).
    pub fn full_access(&self) -> bool {
        self.owner.shared.full_access.load(SeqCst)
    }

    /// A run-scoped token for an agent the app starts itself: connections that attach with it may
    /// use files and the network only if `access`. Dropping the [`Grant`] revokes it.
    pub fn grant(&self, access: bool) -> Result<Grant, String> {
        let shared = &self.owner.shared;
        let token = random_token()?;
        let n = shared.grant_ids.fetch_add(1, SeqCst);
        let file = write_private(&format!("{}-{}-{n}.token", shared.port, shared.run_id), &token)?;
        let mut grants = lock(&shared.grants);
        // Checked under the lock `shutdown` drains: a grant made as the relay stops would outlive
        // it with its file left behind.
        if shared.stopped.load(SeqCst) {
            drop(grants);
            let _ = std::fs::remove_file(&file);
            return Err("the MCP relay has stopped".into());
        }
        grants.insert(token.clone(), (access, file.clone()));
        drop(grants);
        Ok(Grant { token, file, shared: Arc::downgrade(shared) })
    }

    /// With automatic approval off, gated MCP messages attached with this grant wait for
    /// RustGIS-side approval. Neither --yes nor the global relay checkbox widens the grant.
    pub fn grant_reviewed(&self, auto: bool) -> Result<Grant, String> {
        let grant = self.grant(true)?;
        let grants = lock(&self.owner.shared.grants);
        if self.owner.shared.stopped.load(SeqCst) || !grants.contains_key(&grant.token) {
            return Err("the MCP relay has stopped".into());
        }
        lock(&self.owner.shared.reviewed).insert(grant.token.clone(), auto);
        drop(grants);
        Ok(grant)
    }

    /// Runs queued messages on `s`, returning how many completed. Direct archive commands pause
    /// at admission for the app's existing read queue: take_archive_read and finish_archive_read
    /// carry them through their real completion, holding their batch suffix and later messages.
    /// Commands retain ordinary undo bookkeeping; abandoned queued requests never run.
    pub fn poll(&self, s: &mut Session) -> usize {
        *lock(&self.owner.shared.last_poll) = Instant::now();
        // One request at a time: a screen snapshot being captured holds the rest back.
        if lock(&self.owner.shared.shot).is_some() || lock(&self.owner.shared.archive).is_some() {
            return 0;
        }
        let pending: Vec<Request> = lock(&self.owner.shared.requests).drain(..).collect();
        let mut pending = pending.into_iter();
        // The skills switched on, read once for the batch and only when a message asks for them:
        // this runs on the app's thread, and reading the skill folders is file I/O.
        let mut skills: Option<Vec<crate::skills::Skill>> = None;
        let mut ran = 0;
        lock(&self.owner.shared.approvals).retain(|a| a.state.load(SeqCst) == PENDING);
        while let Some(mut req) = pending.next() {
            if req.state.load(SeqCst) != PENDING {
                continue;
            }
            let Some(access) = self.owner.shared.access(&req.token, req.asked) else {
                req.state.store(CANCELLED, SeqCst);
                continue;
            };
            if req.decision.load(SeqCst) == CANCELLED {
                if req.state.compare_exchange(PENDING, RUNNING, SeqCst, SeqCst).is_ok() {
                    let declined = declined_reply(&req.line);
                    self.audit(&req.line, Some(&declined), Duration::ZERO);
                    let _ = req.reply.send(Some(declined));
                    ran += 1;
                }
                continue;
            }
            // Serialize policy changes with claiming the request: turning auto approval off
            // wins unless this request has already become in-flight.
            let policy = lock(&self.owner.shared.reviewed);
            let reviewed = policy.get(&req.token).copied() == Some(false);
            if reviewed && access {
                if !req.approval_required
                    && let Some((summary, plans)) = {
                        let layers = self.enter_agent(s, &req.token);
                        let scope = self.request_scope(&req, AgentAccess::Review, s.actor.clone());
                        let result = approval_plans(s, &req.line, &scope, &req.id.to_string());
                        self.leave_agent(s, &req.token, layers, false);
                        result
                    }
                {
                    req.plans = plans;
                    req.approval_required = true;
                    req.held.store(true, SeqCst);
                    lock(&self.owner.shared.approvals).push(Approval {
                        id: req.id,
                        summary,
                        reviews: req.plans.iter().map(|(_, p)| p.review().clone()).collect(),
                        token: req.token.clone(),
                        state: Arc::clone(&req.state),
                        decision: Arc::clone(&req.decision),
                    });
                }
                if req.approval_required {
                    match req.decision.load(SeqCst) {
                        PENDING => {
                            lock(&self.owner.shared.requests).push_back(req);
                            continue;
                        }
                        CANCELLED => {
                            if req.state.compare_exchange(PENDING, RUNNING, SeqCst, SeqCst).is_ok() {
                                let declined = declined_reply(&req.line);
                                self.audit(&req.line, Some(&declined), Duration::ZERO);
                                let _ = req.reply.send(Some(declined));
                                ran += 1;
                            }
                            continue;
                        }
                        _ => {}
                    }
                }
            }
            if req.state.compare_exchange(PENDING, RUNNING, SeqCst, SeqCst).is_err() {
                continue;
            }
            drop(policy);
            // Claimed first, then checked: a stop that came before the claim keeps the whole
            // batch from running (its connections are already closed).
            if self.owner.shared.stopped.load(SeqCst) {
                req.state.store(CANCELLED, SeqCst);
                continue;
            }
            // Decided now, not when it was queued; a grant revoked meanwhile cancels it.
            let Some(allow_access) = self.owner.shared.access(&req.token, req.asked) else {
                req.state.store(CANCELLED, SeqCst);
                continue;
            };
            // The session is the user's open project: commands that replace it stay theirs.
            let full_access = self.owner.shared.full_access.load(SeqCst) && same_token(&req.token, &self.owner.shared.token);
            let mut server = Server { users_project: true, full_access, ..Server::new(allow_access) };
            if asks_for_skills(&req.line) {
                server.skills = skills.get_or_insert_with(crate::skills::installed).clone();
            }
            // A snapshot of the screen: the app captures its window over the next frames
            // ([`Relay::finish_screen`]), and the rest wait their turn.
            if let Some((id, args)) = screen_call(&req.line, self.owner.shared.screen.load(SeqCst))
                && server
                    .refusal(s, &crate::provider::ToolCall { id: String::new(), name: "snapshot".into(), args: args.clone() })
                    .is_none()
            {
                let wait = Duration::from_secs_f64(crate::mcp::snapshot_wait(&args));
                let mode = if full_access {
                    AgentAccess::Full
                } else if reviewed && allow_access {
                    AgentAccess::Review
                } else if allow_access {
                    AgentAccess::Allow
                } else {
                    AgentAccess::Refuse
                };
                let scope = self.request_scope(&req, mode, None);
                let call = crate::provider::ToolCall {
                    id: crate::mcp::request_key(&req.id.to_string(), 0),
                    name: "snapshot".into(),
                    args: args.clone(),
                };
                let completion = (|| {
                    let engine = crate::tools::policy_call(&call)?.ok_or("missing snapshot adapter")?;
                    let permit = req.plans.iter().position(|(id, _)| id == &call.id).map(|i| req.plans.remove(i).1.allow());
                    s.defer(scope.clone(), &engine, permit)
                })();
                let completion = match completion {
                    Ok(c) => c,
                    Err(e) => {
                        let answer =
                            serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":e}],"isError":true}})
                                .to_string();
                        let _ = req.reply.send(Some(answer));
                        ran += 1;
                        continue;
                    }
                };
                *lock(&self.owner.shared.shot) = Some(Shot { req, scope, completion, id, args, wait });
                let mut queue = lock(&self.owner.shared.requests);
                for rest in pending.rev() {
                    queue.push_front(rest);
                }
                return ran;
            }
            // A last resort: a panic answers that message with an error instead of breaking the
            // app (Session::run catches panics inside commands; this covers the tools around it).
            // The agent works with its own active layer and is named in the undo history; the
            // user's selection is theirs again afterwards.
            let layers = self.enter_agent(s, &req.token);
            let mode = if full_access {
                AgentAccess::Full
            } else if reviewed && allow_access {
                AgentAccess::Review
            } else if allow_access {
                AgentAccess::Allow
            } else {
                AgentAccess::Refuse
            };
            let scope = self.request_scope(&req, mode, s.actor.clone());
            let permits =
                if reviewed { std::mem::take(&mut req.plans).into_iter().map(|(id, p)| (id, p.allow())).collect() } else { Vec::new() };
            let started = Instant::now();
            let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                server.start_archive_batch(s, &req.line, scope, permits, &req.id.to_string())
            }))
            .unwrap_or_else(|_| crate::mcp::ArchiveStep::Done(Some(internal_error(&req.line)), false));
            let (out, picked) = match out {
                crate::mcp::ArchiveStep::Done(out, picked) => (out, picked),
                crate::mcp::ArchiveStep::Read(batch) => {
                    self.leave_agent(s, &req.token, layers, batch.picked());
                    *lock(&self.owner.shared.archive) = Some(Archive { req, batch, started, opened: s.opened, map: s.map().instance_id() });
                    let mut queue = lock(&self.owner.shared.requests);
                    for rest in pending.rev() {
                        queue.push_front(rest);
                    }
                    return ran;
                }
            };
            self.leave_agent(s, &req.token, layers, picked);
            self.audit(&req.line, out.as_deref(), started.elapsed());
            if let Some(a) = action(&req.line, out.as_deref()) {
                *lock(&self.owner.last) = Some(a);
            }
            let _ = req.reply.send(out); // the connection may have hung up
            ran += 1;
        }
        ran
    }
}

impl Relay {
    /// Before an agent's call: names it as the session's actor and makes its own active layer
    /// (if it still exists) the session's. Returns the user's active layer.
    fn request_scope(&self, req: &Request, access: AgentAccess, actor: Option<String>) -> Scope {
        let actor = actor
            .or_else(|| lock(&self.owner.shared.agents).get(&req.token).map(|a| a.name.clone()))
            .unwrap_or_else(|| format!("relay {} request {}", self.owner.shared.run_id, req.id));
        Scope::agent(
            access,
            true,
            &actor,
            Some(actor.clone()),
            Arc::new(GrantLive { shared: Arc::downgrade(&self.owner.shared), token: req.token.clone(), asked: req.asked, access }),
        )
    }
    fn enter_agent(&self, s: &mut Session, token: &str) -> (Option<u32>, Option<u32>) {
        let mut agents = lock(&self.owner.shared.agents);
        let n = agents.len() + 1;
        // Named for this relay run too: a relay started again on the same session numbers its
        // agents from 1 again, and its `agent 1` mustn't pass for an earlier run's.
        let run = &self.owner.shared.run_id;
        let agent =
            agents.entry(token.to_string()).or_insert_with(|| AgentState { name: format!("agent {n} (relay run {run})"), active: None });
        let users = s.active_layer;
        let mine = agent.active.filter(|id| s.map().layer(*id).is_some());
        if mine.is_some() {
            s.active_layer = mine;
        }
        s.actor = Some(agent.name.clone());
        (users, s.active_layer)
    }

    /// After an agent's call: when the call chose a layer (changed the active one, or selected one
    /// with `layer.select`, even the user's), it becomes the agent's own; until then the agent's
    /// commands follow the user's selection. The user gets theirs back, if it's still there.
    fn leave_agent(&self, s: &mut Session, token: &str, (users, entered): (Option<u32>, Option<u32>), picked: bool) {
        s.actor = None;
        if let Some(agent) = lock(&self.owner.shared.agents).get_mut(token)
            && (picked || s.active_layer != entered)
        {
            agent.active = s.active_layer;
        }
        s.active_layer = users.filter(|id| s.map().layer(*id).is_some());
    }
}

/// Whether a reply (or every reply in a batch) says its call worked.
pub(crate) fn reply_ok(reply: Option<&str>) -> bool {
    let Some(v) = reply.and_then(|r| serde_json::from_str::<Value>(r).ok()) else { return false };
    let replies = v.as_array().map(Vec::as_slice).unwrap_or_else(|| std::slice::from_ref(&v));
    replies.iter().all(|r| r.get("error").is_none() && r.pointer("/result/isError") != Some(&Value::Bool(true)))
}

/// Whether a message (or a batch) selects a layer: a `layer.select` command, on its own or as a
/// line of a `run` script.
pub(crate) fn selects_layer(line: &str) -> bool {
    let Ok(msg) = serde_json::from_str::<Value>(line) else { return false };
    let messages = msg.as_array().map(Vec::as_slice).unwrap_or_else(|| std::slice::from_ref(&msg));
    messages.iter().filter_map(|m| m.get("params")).any(|params| {
        let Some(name) = params.get("name").and_then(Value::as_str) else { return false };
        params.get("arguments").is_some_and(|args| crate::tools::selects_layer(name, args))
    })
}

/// A message that is one `snapshot` call to capture the screen: `mode` `screen`, or `auto`
/// (the default) while the app shows the map. Its id and arguments.
fn screen_call(line: &str, shown: bool) -> Option<(Value, Value)> {
    let msg: Value = serde_json::from_str(line).ok()?;
    if msg.get("method").and_then(Value::as_str) != Some("tools/call") {
        return None;
    }
    let id = msg.get("id")?.clone();
    let params = msg.get("params")?;
    if params.get("name").and_then(Value::as_str) != Some("snapshot") {
        return None;
    }
    let args = match params.get("arguments") {
        None | Some(Value::Null) => Value::Object(serde_json::Map::new()),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return None,
    };
    let wanted = match args.get("mode").and_then(Value::as_str).unwrap_or("auto") {
        "screen" | "auto" => shown,
        _ => false,
    };
    wanted.then_some((id, args))
}

/// The last tool call in a message (or batch), with whether its reply says it worked.
fn action(line: &str, reply: Option<&str>) -> Option<Action> {
    let msg: Value = serde_json::from_str(line).ok()?;
    let messages = msg.as_array().map(Vec::as_slice).unwrap_or_else(|| std::slice::from_ref(&msg));
    let call = messages.iter().rev().find(|m| m.get("method").and_then(Value::as_str) == Some("tools/call") && m.get("id").is_some())?;
    let params = call.get("params")?;
    let name = params.get("name").and_then(Value::as_str)?;
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    let text = |k: &str| args.get(k).and_then(Value::as_str).map(|v| crate::tools::clip(v.trim(), 80));
    let label = match name {
        "run_command" => text("command").unwrap_or_else(|| name.to_string()),
        "run_tool" => format!("gp.run {}", text("tool").unwrap_or_default()),
        _ => crate::tools::clip(name, 80),
    };
    let detail = crate::tools::summary(&crate::provider::ToolCall { id: String::new(), name: name.to_string(), args });
    // The reply to that call: a protocol error or a tool result marked as an error failed.
    let id = call.get("id");
    let replies: Value = reply.and_then(|r| serde_json::from_str(r).ok()).unwrap_or(Value::Null);
    let replies = replies.as_array().map(Vec::as_slice).unwrap_or_else(|| std::slice::from_ref(&replies));
    let ok = replies
        .iter()
        .find(|r| r.get("id") == id)
        .is_some_and(|r| r.get("error").is_none() && r.pointer("/result/isError") != Some(&Value::Bool(true)));
    Some(Action { label, detail, ok, at: Instant::now() })
}

/// Whether a message (or a message in a batch) is `prompts/list` or `prompts/get`, the only ones
/// that read skills.
fn asks_for_skills(line: &str) -> bool {
    let prompts = |m: &Value| matches!(m.get("method").and_then(Value::as_str), Some("prompts/list" | "prompts/get"));
    match serde_json::from_str::<Value>(line) {
        Ok(Value::Array(batch)) => batch.iter().any(prompts),
        Ok(m) => prompts(&m),
        Err(_) => false,
    }
}

/// Accepts connections until the relay is stopped, then closes the listener.
fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    while !shared.stopped.load(SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => admit(stream, &shared),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(STOP_POLL),
            Err(e) => {
                log::warn!("MCP relay: {e}");
                std::thread::sleep(STOP_POLL);
            }
        }
    }
}

/// One connection's bookkeeping. Dropping it (however the connection ends) releases its place in
/// the limits, removes its socket, and repaints the client count if it had attached.
struct Slot {
    shared: Arc<Shared>,
    id: u64,
    preauth: bool,
    attached: bool,
}

impl Drop for Slot {
    /// Releases the connection's place in the limits, removes its socket, and repaints the
    /// client count if it had attached.
    fn drop(&mut self) {
        lock(&self.shared.streams).remove(&self.id);
        lock(&self.shared.granted).remove(&self.id);
        if self.preauth {
            self.shared.preauth.fetch_sub(1, SeqCst);
        }
        if self.attached {
            self.shared.attached.fetch_sub(1, SeqCst);
            self.shared.notify();
        }
    }
}

/// Starts a thread for an accepted stream, or refuses it when a limit is reached.
fn admit(stream: TcpStream, shared: &Arc<Shared>) {
    if shared.preauth.fetch_add(1, SeqCst) >= MAX_PREAUTH_CONNECTIONS {
        shared.preauth.fetch_sub(1, SeqCst);
        return;
    }
    let mut slot = Slot { shared: Arc::clone(shared), id: shared.next_id.fetch_add(1, SeqCst), preauth: true, attached: false };
    if stream.set_nonblocking(false).is_err() {
        return;
    }
    let Ok(clone) = stream.try_clone() else { return };
    {
        let mut streams = lock(&shared.streams);
        // Checked under the lock `shutdown` drains: a connection accepted as the relay stops is
        // closed here rather than outliving the stop.
        if shared.stopped.load(SeqCst) || streams.len() >= MAX_CONNECTIONS {
            return;
        }
        streams.insert(slot.id, clone);
    }
    let spawned = std::thread::Builder::new().name("rustgis-mcp-relay-connection".into()).spawn(move || connection(stream, &mut slot));
    if let Err(e) = spawned {
        log::warn!("MCP relay: {e}");
    }
}

/// A line that starts like an HTTP request (`POST / HTTP/1.1`) or response is not the JSON line
/// protocol: a web page can reach a loopback port with `fetch`, and it must get nowhere.
fn looks_like_http(line: &str) -> bool {
    const METHODS: &[&str] = &["GET ", "POST ", "PUT ", "HEAD ", "DELETE ", "OPTIONS ", "PATCH ", "CONNECT ", "TRACE "];
    line.starts_with("HTTP/") || METHODS.iter().any(|m| line.starts_with(m))
}

/// Compares two tokens without stopping at the first difference.
fn same_token(given: &str, expected: &str) -> bool {
    given.len() == expected.len() && given.bytes().zip(expected.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0
}

/// The attaching client's reads, under one deadline for the whole handshake, so a client cannot
/// hold a connection open by sending a byte at a time. Once attached (`deadline` is `None`) each
/// read waits up to [`IDLE_TIMEOUT`]. Reads wake every [`STOP_POLL`] to see whether the relay
/// was stopped: shutting a cloned socket does not wake a blocked read on every platform (not on
/// Windows), so the connection would otherwise outlive the stop by the whole idle timeout.
struct Handshake {
    stream: TcpStream,
    deadline: Option<Instant>,
    shared: Arc<Shared>,
}

impl Read for Handshake {
    /// Reads under the handshake deadline (or [`IDLE_TIMEOUT`] once attached), so a slow client
    /// cannot hold a connection open byte by byte; an expired deadline surfaces as a timeout.
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let deadline = self.deadline.unwrap_or_else(|| Instant::now() + IDLE_TIMEOUT);
        loop {
            if self.shared.stopped.load(SeqCst) {
                return Err(std::io::Error::from(std::io::ErrorKind::ConnectionAborted));
            }
            let left = deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::TimedOut))?;
            self.stream.set_read_timeout(Some(left.min(STOP_POLL)))?;
            match self.stream.read(buf) {
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                other => return other,
            }
        }
    }
}

/// One attached client: queue what it sends for the app's frame, write the app's replies back.
fn connection(stream: TcpStream, slot: &mut Slot) {
    let shared = Arc::clone(&slot.shared);
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let Ok(mut output) = stream.try_clone() else { return };
    let mut input =
        std::io::BufReader::new(Handshake { stream, deadline: Some(Instant::now() + PREAUTH_TIMEOUT), shared: Arc::clone(&shared) });
    // What the client asked for (`--yes`) and the token it attached with: what it may do is looked
    // up for each call, so the app's setting and a revoked grant apply at once.
    let mut asked = false;
    let mut token = String::new();
    // How long this client's requests wait for the app (`timeout_s` in its attach).
    let mut wait = shared.reply_wait;
    loop {
        let max = if slot.attached { MAX_LINE } else { MAX_PREAUTH_LINE };
        let line = match read_message_max(&mut input, max, Oversize::Stop) {
            Ok(Some(line)) => line,
            Ok(None) | Err(framing::Error::Io(_)) => return,
            // Too long or not UTF-8: say why, then close (nothing more of the line is read).
            Err(why) => {
                send_line(&mut output, &error_reply(&Value::Null, -32700, &why.to_string()));
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if !slot.attached && looks_like_http(&line) {
            log::warn!("MCP relay: dropped a connection that spoke HTTP, not the relay protocol");
            return;
        }
        // Before the attach the connection has proven nothing, so a bad line drops it. After
        // a valid attach the line belongs to the client: a non-JSON line is queued like any
        // other, and `Server::handle` answers the parse error while the session stays open.
        let msg = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => Some(msg),
            Err(_) if slot.attached => None,
            Err(_) => return,
        };
        if let Some(msg) = &msg {
            // The first non-empty message must be a valid attach: the run's token, or the
            // connection goes. A repeat attach still carries the token (it may update what the
            // connection allows).
            let is_attach = msg.get("method").and_then(Value::as_str) == Some(ATTACH);
            if !slot.attached || is_attach {
                let given = msg.get("params").and_then(|p| p.get("token")).and_then(Value::as_str).unwrap_or("");
                if !is_attach || shared.access(given, false).is_none() {
                    return;
                }
                token = given.to_string();
                if same_token(given, &shared.token) {
                    // Re-attached with the relay's own token: no longer a grant's connection.
                    lock(&shared.granted).remove(&slot.id);
                } else {
                    lock(&shared.granted).insert(slot.id, token.clone());
                    // A grant revoked between the check above and this insert: the revocation
                    // missed this connection, so it ends here.
                    if shared.access(given, false).is_none() {
                        return;
                    }
                }
                asked = msg.get("params").and_then(|p| p.get("access")).and_then(Value::as_bool).unwrap_or(false);
                if let Some(secs) = msg.get("params").and_then(|p| p.get("timeout_s")).and_then(Value::as_f64).filter(|s| s.is_finite()) {
                    wait = Duration::from_secs_f64(secs.clamp(MIN_WAIT.as_secs_f64(), MAX_WAIT.as_secs_f64()));
                }
                if !slot.attached {
                    slot.attached = true;
                    slot.preauth = false;
                    shared.preauth.fetch_sub(1, SeqCst);
                    shared.attached.fetch_add(1, SeqCst);
                    input.get_mut().deadline = None;
                    shared.notify();
                }
                continue;
            }
        }
        if shared.stopped.load(SeqCst) {
            return;
        }
        // A revoked grant ends the connection.
        if shared.access(&token, asked).is_none() {
            return;
        }
        let expects = expects_reply(&line);
        let id = msg.as_ref().map_or(Value::Null, request_id);
        let state = Arc::new(AtomicU8::new(PENDING));
        let held = Arc::new(AtomicBool::new(false));
        let (tx, rx) = sync_channel(1);
        let queued = {
            let mut queue = lock(&shared.requests);
            if shared.stopped.load(SeqCst) {
                return;
            }
            // Requests waiting for the user's approval don't take the room of others (reads still
            // run while the user decides), within a cap of their own.
            let waiting = queue.iter().filter(|r| r.approval_required).count();
            if queue.len() - waiting >= MAX_QUEUE || waiting >= MAX_QUEUE {
                false
            } else {
                queue.push_back(Request {
                    plans: Vec::new(),
                    approval_required: false,
                    held: Arc::clone(&held),
                    id: shared.next_id.fetch_add(1, SeqCst),
                    decision: Arc::new(AtomicU8::new(PENDING)),
                    line,
                    token: token.clone(),
                    asked,
                    reply: tx,
                    state: Arc::clone(&state),
                });
                true
            }
        };
        if !queued {
            if expects && !send_line(&mut output, &error_reply(&id, -32001, "the app is busy; try again")) {
                return;
            }
            continue;
        }
        // The app's frame runs queued messages, and egui draws no frame the UI thread isn't
        // asked for: wake it now, or the client waits until the user moves the mouse.
        shared.notify();
        if !expects {
            continue;
        }
        let asked_at = Instant::now();
        let mut answer = wait_or_busy(&rx, &shared, wait, asked_at, &state, &held);
        // The app ran no frame for a while (a dialog, a long tool): said at once, not after the
        // whole wait.
        if matches!(answer, Err(RecvTimeoutError::Timeout))
            && !held.load(SeqCst)
            && app_busy(&shared, asked_at)
            && state.compare_exchange(PENDING, CANCELLED, SeqCst, SeqCst).is_ok()
        {
            let busy = "RustGIS isn't drawing (a dialog is open, a long task is running, or its window is hidden); the request was not run, try again when it is back";
            if !send_line(&mut output, &error_reply(&id, BUSY_CODE, busy)) {
                return;
            }
            continue;
        }
        // Waiting for the user's approval: the user takes their time, within a limit of its own.
        while matches!(answer, Err(RecvTimeoutError::Timeout))
            && held.load(SeqCst)
            && state.load(SeqCst) == PENDING
            && asked_at.elapsed() < APPROVAL_WAIT
        {
            answer = wait_reply(&rx, &shared, wait.min(APPROVAL_WAIT.saturating_sub(asked_at.elapsed())));
        }
        let reply = match answer {
            Ok(reply) => reply,
            Err(RecvTimeoutError::Timeout) => {
                // Not yet running: cancel it, so the app can never run it after the client has
                // been told it failed.
                if state.compare_exchange(PENDING, CANCELLED, SeqCst, SeqCst).is_ok() {
                    // Not approved in time: this request fails, the connection stays (the bridge
                    // doesn't reconnect).
                    if held.load(SeqCst) {
                        if !send_line(&mut output, &error_reply(&id, -32002, "the request was not approved in time; it was cancelled")) {
                            return;
                        }
                        continue;
                    }
                    send_line(&mut output, &error_reply(&id, -32002, "the app did not run the command in time; it was cancelled"));
                    return;
                }
                // Already running on the app's frame: wait one more bounded interval for its reply.
                // If it still has not come, its outcome is unknown; say so instead of waiting on.
                match wait_reply(&rx, &shared, wait) {
                    Ok(reply) => reply,
                    Err(RecvTimeoutError::Timeout) => {
                        send_line(&mut output, &error_reply(&id, -32003, "the app is still running the command; its outcome is unknown"));
                        return;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            // The relay stopped, or the app dropped the request.
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match reply {
            Some(reply) => {
                if !send_line(&mut output, &reply) {
                    return;
                }
            }
            // No reply to this message.
            None => return,
        }
    }
}

/// Whether the app has run no frame since a request queued at `asked_at` for the relay's
/// `busy_after`. (An app that was idle before the request is not busy: the request wakes it.)
fn app_busy(shared: &Shared, asked_at: Instant) -> bool {
    asked_at.elapsed() >= shared.busy_after && lock(&shared.last_poll).elapsed() >= shared.busy_after
}

/// [`wait_reply`], returning early (as a timeout) when the request is still waiting to run, not
/// for the user's approval, and the app is busy ([`app_busy`]).
fn wait_or_busy(
    rx: &Receiver<Option<String>>,
    shared: &Shared,
    wait: Duration,
    asked_at: Instant,
    state: &AtomicU8,
    held: &AtomicBool,
) -> Result<Option<String>, RecvTimeoutError> {
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(RecvTimeoutError::Timeout);
        }
        match wait_reply(rx, shared, left.min(Duration::from_millis(250))) {
            Err(RecvTimeoutError::Timeout) => {}
            done => return done,
        }
        if app_busy(shared, asked_at) && state.load(SeqCst) == PENDING && !held.load(SeqCst) {
            return Err(RecvTimeoutError::Timeout);
        }
    }
}

/// Waits up to `wait` for a request's reply, checking every [`STOP_POLL`] whether the relay
/// stopped: a stop ends the wait at once as `Disconnected`, even while the app is still running
/// the request, so no connection thread outlives a stop by more than that.
fn wait_reply(rx: &Receiver<Option<String>>, shared: &Shared, wait: Duration) -> Result<Option<String>, RecvTimeoutError> {
    let deadline = Instant::now() + wait;
    loop {
        if shared.stopped.load(SeqCst) {
            return Err(RecvTimeoutError::Disconnected);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(RecvTimeoutError::Timeout);
        }
        match rx.recv_timeout(left.min(STOP_POLL)) {
            Err(RecvTimeoutError::Timeout) => {}
            done => return done,
        }
    }
}

/// Writes one line and flushes it. `false` when the client has gone.
fn send_line(output: &mut TcpStream, text: &str) -> bool {
    writeln!(output, "{text}").and_then(|()| output.flush()).is_ok()
}

/// The JSON-RPC id of a message, when it is a string or a number.
fn request_id(msg: &Value) -> Value {
    msg.get("id").filter(|id| id.is_string() || id.is_number()).cloned().unwrap_or(Value::Null)
}

/// Preflight the exact tool calls in a message, including batches. Reserved commands still go
/// through Server::refusal and remain unavailable even after approval.
fn approval_plans(s: &Session, line: &str, scope: &Scope, request: &str) -> Option<(String, Vec<(String, Pending)>)> {
    let msg: Value = serde_json::from_str(line).ok()?;
    let messages = msg.as_array().map(Vec::as_slice).unwrap_or_else(|| std::slice::from_ref(&msg));
    if messages.len() > crate::mcp::MAX_BATCH {
        return None;
    }
    let mut summaries = Vec::new();
    let mut plans = Vec::new();
    for (position, msg) in messages.iter().enumerate() {
        if msg.get("method").and_then(Value::as_str) != Some("tools/call") {
            continue;
        }
        let Some(id) = msg.get("id") else {
            continue;
        };
        if !matches!(id, Value::String(_) | Value::Number(_)) {
            continue;
        }
        let Some(params) = msg.get("params").filter(|p| p.is_object()) else { continue };
        let Ok((name, arguments)) = crate::mcp::tool_request(params) else { continue };
        let call = crate::provider::ToolCall { id: crate::mcp::request_key(request, position), name: name.into(), args: arguments };
        if let Ok(Some(engine)) = crate::tools::policy_call(&call)
            && let Outcome::NeedsApproval(p) = s.review(scope, &engine)
        {
            let review = p.review();
            summaries.push(format!("{}: {}\n{}", review.what, review.why, review.steps.join("\n")));
            plans.push((call.id, p));
        }
    }
    (!plans.is_empty()).then(|| (summaries.join("\n"), plans))
}

fn declined_reply(line: &str) -> String {
    let msg: Value = serde_json::from_str(line).unwrap_or(Value::Null);
    let denied = |msg: &Value| {
        let id = msg.get("id")?;
        let message = "The user declined this MCP request. Do not retry it.";
        if msg.get("method").and_then(Value::as_str) == Some("tools/call") {
            Some(serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"isError":true,"content":[{"type":"text","text":message}]}}))
        } else {
            Some(serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":message}}))
        }
    };
    match msg.as_array() {
        Some(batch) => Value::Array(batch.iter().filter_map(denied).collect()).to_string(),
        None => denied(&msg).unwrap_or(Value::Null).to_string(),
    }
}

/// A JSON-RPC error reply to `id`.
fn error_reply(id: &Value, code: i64, message: &str) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

/// The internal-error reply to a message that panicked, with the id the client can match it to.
fn internal_error(message: &str) -> String {
    let id = serde_json::from_str::<Value>(message).ok().map_or(Value::Null, |v| request_id(&v));
    error_reply(&id, -32603, "internal error")
}

/// The original grant and current user settings, checked on fetch workers as well as the UI.
struct GrantLive {
    shared: std::sync::Weak<Shared>,
    token: String,
    asked: bool,
    access: AgentAccess,
}
impl Revocation for GrantLive {
    fn allows(&self, effects: Effects) -> bool {
        let Some(shared) = self.shared.upgrade() else {
            return false;
        };
        if shared.stopped.load(SeqCst) {
            return false;
        }
        let Some(access) = shared.access(&self.token, self.asked) else {
            return false;
        };
        if (effects.contains(Effect::Reserved) || effects.contains(Effect::Automation) || effects.contains(Effect::Replaces))
            && (!shared.full_access.load(SeqCst) || !same_token(&self.token, &shared.token))
        {
            return false;
        }
        if effects.contains(Effect::Files) || effects.contains(Effect::Network) || effects.contains(Effect::Plugin) {
            if !access {
                return false;
            }
            if self.access == AgentAccess::Allow && lock(&shared.reviewed).get(&self.token) == Some(&false) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
#[path = "relay/archive_tests.rs"]
mod archive_tests;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    /// Only prompt requests read the skill folders (on the app's thread); a batch with one does.
    #[test]
    fn only_prompt_requests_load_skills() {
        assert!(super::asks_for_skills(r#"{"jsonrpc":"2.0","id":1,"method":"prompts/list"}"#));
        assert!(super::asks_for_skills(r#"{"jsonrpc":"2.0","id":1,"method":"prompts\/get","params":{"name":"x"}}"#));
        assert!(super::asks_for_skills(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","id":2,"method":"prompts/list"}]"#));
        for line in
            [r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#, "not json", "[1, 2]", "{}"]
        {
            assert!(!super::asks_for_skills(line), "{line}");
        }
    }

    use std::io::{BufRead, BufReader, Write};
    use std::sync::atomic::AtomicUsize;

    use rustgis_engine::{Project, Session};
    use serde_json::json;

    use super::*;

    #[test]
    fn reviewed_grants_require_exact_run_scoped_approvals() {
        let _plugins = crate::tests::PLUGIN_REGISTRY_TEST.lock().unwrap_or_else(|poison| poison.into_inner());
        let relay = Relay::start(0).unwrap();
        relay.set_allow_access(true); // The global checkbox must not bypass run-specific review.
        let grant = relay.grant_reviewed(false).unwrap();
        let other = relay.grant_reviewed(false).unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut s = sample();
        let export = |path: &std::path::Path| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"run_command","arguments":{"command":"share.exportMap","params":{"path":path,"width":64,"height":64}}}});
        let queue = |message: Value| {
            let (tx, rx) = sync_channel(1);
            let state = Arc::new(AtomicU8::new(PENDING));
            let id = relay.owner.shared.next_id.fetch_add(1, SeqCst);
            lock(&relay.owner.shared.requests).push_back(Request {
                plans: Vec::new(),
                approval_required: false,
                held: Arc::default(),
                id,
                decision: Arc::new(AtomicU8::new(PENDING)),
                line: message.to_string(),
                token: grant.token.clone(),
                asked: true,
                reply: tx,
                state: Arc::clone(&state),
            });
            (id, rx, state)
        };
        let path = root.path().join("allowed.png");
        let (id, rx, _) = queue(export(&path));
        assert_eq!(relay.poll(&mut s), 0);
        assert!(!path.exists());
        assert!(rx.try_recv().is_err());
        assert_eq!(grant.pending().len(), 1);
        assert!(grant.pending()[0].summary.contains("share.exportMap"));
        other.decide(id, true);
        assert_eq!(relay.poll(&mut s), 0, "another run cannot approve this request");
        grant.decide(id, true);
        assert_eq!(relay.poll(&mut s), 1);
        assert!(path.exists());
        let reply: Value = serde_json::from_str(&rx.recv().unwrap().unwrap()).unwrap();
        assert_ne!(reply["result"]["isError"], true, "{reply}");
        let changed = root.path().join("changed.png");
        let (id, rx, _) = queue(export(&changed));
        assert_eq!(relay.poll(&mut s), 0);
        assert!(grant.pending().iter().any(|p| p.id == id)); // Freeze the actual first card.
        s.run("layer.select", &json!({"layer":"Cities"})).unwrap();
        grant.decide(id, true);
        assert_eq!(relay.poll(&mut s), 1);
        assert!(rx.recv().unwrap().unwrap().contains("changed"));
        assert!(!changed.exists());
        // A load replaced while the actual relay card is shown cannot inherit its consent.
        let load = || {
            rustgis_engine::plugins::load(
                rustgis_engine::plugins::Package::from_zip(&rustgis_engine::plugins::testing::package("relay-policy-load", &["edit"]))
                    .unwrap(),
                "test",
            )
            .unwrap()
        };
        load();
        let (id, rx, _) = queue(
            json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"run_command","arguments":{"command":"plugin:relay-policy-load/f","params":{"op":"layer.rename","params":{"layer":"Cities","name":"stale load"}}}}}),
        );
        assert_eq!(relay.poll(&mut s), 0);
        assert!(grant.pending().iter().any(|p| p.id == id));
        rustgis_engine::plugins::unload("relay-policy-load");
        load();
        grant.decide(id, true);
        assert_eq!(relay.poll(&mut s), 1);
        assert!(rx.recv().unwrap().unwrap().contains("changed"));
        assert!(s.map().layer_by_name("stale load").is_none());
        rustgis_engine::plugins::unload("relay-policy-load");
        let denied = root.path().join("denied.png");
        let (id, rx, _) = queue(export(&denied));
        assert_eq!(relay.poll(&mut s), 0);
        grant.decide(id, false);
        grant.set_auto(true); // An explicit decline wins even if auto approval is then enabled.
        assert_eq!(relay.poll(&mut s), 1);
        assert!(rx.recv().unwrap().unwrap().contains("declined"));
        assert!(!denied.exists());
        grant.set_auto(false);
        let batch_path = root.path().join("batch.png");
        let (id, rx, _) = queue(json!([{"jsonrpc":"2.0","id":0,"method":"tools/call"},export(&batch_path)]));
        assert_eq!(relay.poll(&mut s), 0, "malformed earlier entries cannot bypass batch approval");
        assert!(grant.pending().iter().any(|p| p.id == id && p.summary.contains("share.exportMap")));
        grant.decide(id, false);
        assert_eq!(relay.poll(&mut s), 1);
        assert!(rx.recv().unwrap().unwrap().contains("declined"));
        assert!(!batch_path.exists());
        // Repeated wire ids must not let a read consume the export's approval.
        let duplicate_path = root.path().join("duplicate-id.png");
        let (id, rx, _) = queue(json!([
            {"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_layers","arguments":{}}},
            export(&duplicate_path)
        ]));
        assert_eq!(relay.poll(&mut s), 0);
        assert!(!duplicate_path.exists());
        grant.decide(id, true);
        assert_eq!(relay.poll(&mut s), 1);
        let reply: Value = serde_json::from_str(&rx.recv().unwrap().unwrap()).unwrap();
        let replies = reply.as_array().unwrap();
        assert_eq!(replies.len(), 2, "{reply}");
        assert!(replies.iter().all(|r| r["id"] == 1 && r["result"]["isError"] == false), "{reply}");
        assert!(duplicate_path.exists());
        let expired = root.path().join("expired.png");
        let (id, _, state) = queue(export(&expired));
        assert_eq!(relay.poll(&mut s), 0);
        state.store(CANCELLED, SeqCst);
        grant.decide(id, true);
        assert_eq!(relay.poll(&mut s), 0, "an expired request cannot be revived");
        assert!(!expired.exists());
        grant.set_auto(true);
        let auto = root.path().join("auto.png");
        let (_, rx, _) = queue(export(&auto));
        assert_eq!(relay.poll(&mut s), 1);
        assert!(auto.exists());
        assert!(rx.recv().unwrap().is_some());
        grant.set_auto(false);
        let revoked = root.path().join("revoked.png");
        let (_, _, state) = queue(export(&revoked));
        assert_eq!(relay.poll(&mut s), 0);
        drop(grant);
        assert_eq!(relay.poll(&mut s), 0);
        assert_eq!(state.load(SeqCst), CANCELLED);
        assert!(!revoked.exists());
        relay.stop();
    }

    fn sample() -> Session {
        let mut s = Session::new(Project::default());
        s.run("project.sample", &Value::Null).unwrap();
        s
    }

    /// Reads the one reply a request gets.
    fn reply(client: &TcpStream) -> Value {
        let mut input = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        input.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// The token the relay wrote for its port: the same file the CLI reads.
    fn token(relay: &Relay) -> String {
        read_token(relay.port()).unwrap()
    }

    /// Attaches a client the way `rustgis-cli mcp --attach` does.
    fn attach(client: &mut TcpStream, relay: &Relay, access: bool) {
        let hello = json!({"jsonrpc": "2.0", "method": "rustgis/attach", "params": {"access": access, "token": token(relay)}});
        writeln!(client, "{hello}").unwrap();
        client.flush().unwrap();
    }

    /// Lets the connection thread hand the request to the app's frame, which runs it.
    fn settle(relay: &Relay, s: &mut Session) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while relay.poll(s) == 0 {
            assert!(std::time::Instant::now() < deadline, "the relay never handed the request to the app");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Waits until `cond` holds, failing after five seconds.
    fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn add_sample_call(id: u64) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
            "name": "run_command",
            "arguments": {"command": "layer.addSample", "params": {"name": "countries"}}
        }})
    }

    /// A command sent through the relay runs on the target Session through Session::run (its
    /// undo history shows it), and a bind away from loopback is refused.
    #[test]
    fn relay_runs_a_command_on_the_target_session() {
        // The negative control: anything but loopback is refused, before any socket opens.
        for addr in ["0.0.0.0:0", "[::]:0", "192.0.2.1:8767"] {
            let a: SocketAddr = addr.parse().unwrap();
            let e = Relay::bind(a).unwrap_err();
            assert!(e.contains("loopback"), "{addr}: {e}");
        }

        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let layers = s.map().layers.len();
        let undo = s.undo_depth();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        writeln!(client, "{}", add_sample_call(1)).unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        let reply = reply(&client);
        assert_eq!(reply["id"], 1, "{reply}");
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        // The session the relay was started for: the command ran here, one undo step.
        assert_eq!(s.map().layers.len(), layers + 1, "{reply}");
        assert_eq!(s.undo_depth(), undo + 1, "one undo step, as in the private-session server");
        relay.stop();
    }

    /// A connection that doesn't start with the run's token is dropped: a self-declared access
    /// flag proves nothing, no command runs, and a line that speaks HTTP gets nowhere.
    #[test]
    fn relay_drops_connections_without_the_token() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        // A wrong token: the connection is closed and nothing was queued.
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        let bad = json!({"jsonrpc": "2.0", "method": "rustgis/attach", "params": {"access": true, "token": "not-the-token"}});
        writeln!(client, "{bad}").unwrap();
        client.flush().unwrap();
        let mut input = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        assert_eq!(input.read_line(&mut line).unwrap(), 0, "the relay kept a client without the token: {line}");
        assert_eq!(relay.poll(&mut s), 0, "a connection without the token queued a message");
        assert_eq!(relay.attached_clients(), 0);
        // A command with no attach at all: dropped the same way.
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "list_layers", "arguments": {}}});
        writeln!(client, "{call}").unwrap();
        client.flush().unwrap();
        let mut input = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        assert_eq!(input.read_line(&mut line).unwrap(), 0, "the relay kept a client that never attached: {line}");
        assert_eq!(relay.poll(&mut s), 0, "a connection that never attached queued a message");
        // An HTTP request line — a web page's fetch at the loopback port — is a protocol
        // violation: dropped, no parse-error reply to keep the exchange going.
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        writeln!(client, "POST / HTTP/1.1").unwrap();
        client.flush().unwrap();
        let mut input = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        assert_eq!(input.read_line(&mut line).unwrap(), 0, "the relay kept an HTTP request open: {line}");
        assert_eq!(relay.poll(&mut s), 0, "an HTTP request queued a message");
        relay.stop();
    }

    /// After a valid attach a non-JSON line is not a protocol violation of its own: the server
    /// answers it with a parse error and the same connection keeps serving requests.
    #[test]
    fn relay_returns_parse_error_after_attach_and_stays_usable() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        writeln!(client, "this is not JSON").unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        let parse_error = reply(&client);
        assert_eq!(parse_error["error"]["code"], -32700, "{parse_error}");
        // The same connection still answers a real request afterwards.
        let ping = json!({"jsonrpc": "2.0", "id": 7, "method": "ping"});
        writeln!(client, "{ping}").unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        let pong = reply(&client);
        assert_eq!(pong["id"], 7, "{pong}");
        relay.stop();
    }

    /// A file command still needs --yes: without the connection's access message the existing
    /// refusal policy answers it.
    #[test]
    fn relay_refuses_file_commands_without_yes() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        let out = std::env::temp_dir().join(format!("rustgis-relay-test-{}.png", std::process::id()));
        let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "run_command",
            "arguments": {"command": "share.exportMap", "params": {"path": out.display().to_string()}}
        }});
        writeln!(client, "{call}").unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        let reply = reply(&client);
        assert_eq!(reply["result"]["isError"], true, "{reply}");
        assert!(reply["result"]["content"][0]["text"].as_str().unwrap().contains("--yes"), "{reply}");
        assert!(!out.exists(), "the file command did not run");
        let _ = std::fs::remove_file(out);
        relay.stop();
    }

    /// The session behind the relay is the user's open project: an attached agent may not
    /// replace it, even with file and network access, and the project stays as it was.
    #[test]
    fn relay_keeps_project_commands_for_the_user() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let layers = s.map().layers.len();
        let undo = s.undo_depth();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, true);
        let project = json!({"version": 1}).to_string();
        let calls = [
            ("project.new", json!({})),
            ("project.sample", json!({})),
            ("project.fromTemplate", json!({"template": "blank"})),
            ("project.openText", json!({"text": project})),
            ("project.open", json!({"path": "elsewhere.rgis"})),
        ];
        for (id, (command, params)) in (10..).zip(calls) {
            let call = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
                "name": "run_command",
                "arguments": {"command": command, "params": params}
            }});
            writeln!(client, "{call}").unwrap();
            client.flush().unwrap();
            settle(&relay, &mut s);
            let reply = reply(&client);
            assert_eq!(reply["result"]["isError"], true, "{command}: {reply}");
            assert!(reply["result"]["content"][0]["text"].as_str().unwrap().contains("reserved to the user"), "{command}: {reply}");
        }
        assert_eq!(s.map().layers.len(), layers, "the user's project was replaced");
        assert_eq!(s.undo_depth(), undo, "a refused command left an undo step");
        // Undo and redo stay: the agent's own steps are the user's to watch and the agent's to take back.
        writeln!(client, "{}", add_sample_call(20)).unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        assert_eq!(reply(&client)["result"]["isError"], false);
        let undo_call = json!({"jsonrpc": "2.0", "id": 21, "method": "tools/call", "params": {
            "name": "run_command", "arguments": {"command": "edit.undo", "params": {}}
        }});
        writeln!(client, "{undo_call}").unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        let reply = reply(&client);
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        assert_eq!(s.map().layers.len(), layers, "{reply}");
        relay.stop();
    }

    fn command(id: u64, command: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "run_command", "arguments": {"command": command, "params": params}}})
    }

    /// Sends one call and gives back its reply, letting the app run it.
    fn call(client: &mut TcpStream, relay: &Relay, s: &mut Session, msg: Value) -> Value {
        writeln!(client, "{msg}").unwrap();
        client.flush().unwrap();
        settle(relay, s);
        reply(client)
    }

    /// The agent and the user each have their own active layer: what the agent selects doesn't
    /// move the user's selection, and what the user selects doesn't redirect the agent's
    /// commands that name no layer.
    #[test]
    fn relay_keeps_the_agents_active_layer_apart_from_the_users() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let id = |s: &Session, name: &str| s.map().layer_by_name(name).map(|l| l.id);
        s.active_layer = id(&s, "Countries");
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        let r = call(&mut client, &relay, &mut s, command(1, "layer.select", json!({"layer": "Cities"})));
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert_eq!(s.active_layer, id(&s, "Countries"), "the user's selection stays");
        // The user picks another layer; the agent's next un-targeted command still acts on its own.
        s.active_layer = id(&s, "Lakes");
        let r = call(&mut client, &relay, &mut s, command(2, "layer.rename", json!({"name": "Towns"})));
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert!(s.map().layer_by_name("Towns").is_some() && s.map().layer_by_name("Lakes").is_some(), "the agent renamed its own layer");
        assert_eq!(s.active_layer, id(&s, "Lakes"));
        relay.stop();

        // An agent that hasn't chosen a layer follows the user's selection.
        let relay2 = Relay::start(0).unwrap();
        let mut fresh = TcpStream::connect(("127.0.0.1", relay2.port())).unwrap();
        attach(&mut fresh, &relay2, false);
        s.active_layer = id(&s, "Towns");
        call(
            &mut fresh,
            &relay2,
            &mut s,
            json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "list_layers", "arguments": {}}}),
        );
        s.active_layer = id(&s, "Rivers");
        call(&mut fresh, &relay2, &mut s, command(10, "layer.rename", json!({"name": "Streams"})));
        assert!(s.map().layer_by_name("Streams").is_some() && s.map().layer_by_name("Towns").is_some(), "it followed the user to Rivers");
        relay2.stop();

        // A layer.select that fails chooses nothing: the agent still follows the user.
        let relay3 = Relay::start(0).unwrap();
        let mut failing = TcpStream::connect(("127.0.0.1", relay3.port())).unwrap();
        attach(&mut failing, &relay3, false);
        s.active_layer = id(&s, "Countries");
        call(&mut failing, &relay3, &mut s, command(11, "layer.select", json!({"layer": "No such layer"})));
        s.active_layer = id(&s, "Lakes");
        call(&mut failing, &relay3, &mut s, command(12, "layer.rename", json!({"name": "Ponds"})));
        assert!(s.map().layer_by_name("Ponds").is_some() && s.map().layer_by_name("Countries").is_some(), "it followed the user");
        relay3.stop();
        s.run("layer.rename", &json!({"layer": "Ponds", "name": "Lakes"})).unwrap();

        // An agent that picks the layer the user has active keeps it when the user moves on.
        let relay = Relay::start(0).unwrap();
        s.active_layer = id(&s, "Countries");
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        call(&mut client, &relay, &mut s, command(3, "layer.select", json!({"layer": "Countries"})));
        s.active_layer = id(&s, "Lakes");
        call(&mut client, &relay, &mut s, command(4, "layer.rename", json!({"name": "Nations"})));
        assert!(s.map().layer_by_name("Nations").is_some() && s.map().layer_by_name("Lakes").is_some());
        relay.stop();
    }

    /// A relay started again on the same session doesn't take an earlier run's agent steps for
    /// its own agents'.
    #[test]
    fn relay_agents_of_another_run_cant_undo_earlier_steps() {
        let mut s = sample();
        let first = Relay::start(0).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", first.port())).unwrap();
        attach(&mut client, &first, false);
        call(&mut client, &first, &mut s, add_sample_call(1));
        first.stop();
        let second = Relay::start(0).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", second.port())).unwrap();
        attach(&mut client, &second, false);
        let r = call(&mut client, &second, &mut s, command(2, "edit.undo", json!({})));
        assert_eq!(r["result"]["isError"], true, "another run's agent 1 is not this one: {r}");
        second.stop();
    }

    /// An agent undoes and redoes only its own steps, never the user's; the user's history keeps
    /// working as before.
    #[test]
    fn relay_agents_undo_only_their_own_steps() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        let undo = |id| command(id, "edit.undo", json!({}));
        // The user's step: refused to the agent.
        s.run("layer.rename", &json!({"layer": "Rivers", "name": "Streams"})).unwrap();
        let r = call(&mut client, &relay, &mut s, undo(1));
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert!(r["result"]["content"][0]["text"].as_str().is_some_and(|t| t.contains("the user's")), "{r}");
        assert!(s.map().layer_by_name("Streams").is_some(), "the user's step stays");
        // The agent's own step: undone, then redone.
        let layers = s.map().layers.len();
        call(&mut client, &relay, &mut s, add_sample_call(2));
        assert!(s.undo_actor().flatten().is_some_and(|a| a.starts_with("agent 1 (relay run ")), "{:?}", s.undo_actor());
        let r = call(&mut client, &relay, &mut s, undo(3));
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert_eq!(s.map().layers.len(), layers);
        let r = call(&mut client, &relay, &mut s, command(4, "edit.redo", json!({})));
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert_eq!(s.map().layers.len(), layers + 1);
        // A script can't undo twice past the agent's own step: undo runs alone.
        call(&mut client, &relay, &mut s, add_sample_call(7));
        let twice = json!({"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "run", "arguments": {"script": "edit.undo\nedit.undo"}}});
        let r = call(&mut client, &relay, &mut s, twice);
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert!(s.map().layer_by_name("Streams").is_some(), "the user's step stays");
        let r = call(&mut client, &relay, &mut s, command(9, "edit.undo", json!({})));
        assert_eq!(r["result"]["isError"], false, "its own step, alone: {r}");
        // A script can't slip an undo of the user's step through either.
        call(&mut client, &relay, &mut s, undo(5));
        let script =
            json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "run", "arguments": {"script": "edit.undo"}}});
        let r = call(&mut client, &relay, &mut s, script);
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert!(s.map().layer_by_name("Streams").is_some());
        // The user still undoes anything.
        assert!(s.run("edit.undo", &json!({})).is_ok());
        assert!(s.map().layer_by_name("Rivers").is_some());
        assert_eq!(s.actor, None);
        relay.stop();
    }

    /// Every tool call through the relay goes to the audit log, with how it went.
    #[test]
    fn relay_writes_each_call_to_the_audit_log() {
        let relay = Relay::start(0).unwrap();
        let log = tempfile::tempdir().unwrap();
        relay.set_audit_dir(Some(log.path().to_path_buf()));
        let mut s = sample();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        writeln!(client, "{}", add_sample_call(1)).unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        reply(&client);
        let ping = json!({"jsonrpc": "2.0", "id": 2, "method": "ping"});
        writeln!(client, "{ping}").unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        reply(&client);
        let files: Vec<_> = std::fs::read_dir(log.path()).unwrap().flatten().collect();
        assert_eq!(files.len(), 1);
        let text = std::fs::read_to_string(files[0].path()).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 1, "only tool calls are logged: {text}");
        assert_eq!(
            (lines[0]["command"].clone(), lines[0]["success"].clone(), lines[0]["source"].clone()),
            (json!("layer.addSample"), json!(true), json!("mcp"))
        );
        relay.stop();
    }

    /// A request waiting while the app runs no frame (a dialog is open) is told the app is busy
    /// well before its timeout, and never runs; an app that was merely idle before is not busy.
    #[test]
    fn relay_says_the_app_is_busy_early() {
        let relay = Relay::bind_with(SocketAddr::from(([127, 0, 0, 1], 0)), Duration::from_secs(30), Duration::from_millis(1500)).unwrap();
        let mut s = sample();
        let layers = s.map().layers.len();
        // Idle before the request: the request wakes the app, which runs it.
        std::thread::sleep(Duration::from_millis(2000));
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        writeln!(client, "{}", add_sample_call(1)).unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        assert_eq!(reply(&client)["result"]["isError"], false);
        // Now the app runs no frame: busy, early, and the request never runs.
        let asked = Instant::now();
        writeln!(client, "{}", add_sample_call(2)).unwrap();
        client.flush().unwrap();
        let r = reply(&client);
        assert!(asked.elapsed() < Duration::from_secs(10), "the busy reply took {:?}", asked.elapsed());
        assert_eq!(r["error"]["code"], BUSY_CODE, "{r}");
        assert!(r["error"]["message"].as_str().is_some_and(|m| m.contains("isn't drawing")), "{r}");
        assert_eq!(relay.poll(&mut s), 0, "a request told the app was busy never runs");
        assert_eq!(s.map().layers.len(), layers + 1);
        // The connection still works once the app is back.
        writeln!(client, "{}", add_sample_call(3)).unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        assert_eq!(reply(&client)["result"]["isError"], false);
        relay.stop();
    }

    /// A client's `timeout_s` sets how long its requests wait for the app, within the limits.
    #[test]
    fn relay_waits_as_long_as_the_client_asks() {
        let relay = Relay::bind_with(SocketAddr::from(([127, 0, 0, 1], 0)), Duration::from_secs(60), Duration::from_secs(60)).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        let hello =
            json!({"jsonrpc": "2.0", "method": "rustgis/attach", "params": {"access": false, "token": token(&relay), "timeout_s": 1}});
        writeln!(client, "{hello}").unwrap();
        writeln!(client, "{}", add_sample_call(1)).unwrap();
        client.flush().unwrap();
        let asked = Instant::now();
        let r = reply(&client);
        let took = asked.elapsed();
        // Asked for 1 s, given the shortest allowed: 5 s, not the relay's 60.
        assert!(took >= MIN_WAIT - Duration::from_millis(200) && took < Duration::from_secs(20), "{took:?}");
        assert_eq!(r["error"]["code"], -32002, "{r}");
        relay.stop();
    }

    /// The endpoint file says where the relay is and goes when it stops; the port can be named
    /// in the environment, and a busy port's error says how.
    #[test]
    fn relay_publishes_its_endpoint_and_names_the_port_variable() {
        let relay = Relay::start(0).unwrap();
        let name = format!("endpoint-test-{}.json", std::process::id());
        let path = relay.publish_endpoint_as(&name).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!((v["port"].clone(), v["pid"].clone(), v["schema"].clone()), (json!(relay.port()), json!(std::process::id()), json!(1)));
        assert_eq!(v["token_file"], token_path(relay.port()).display().to_string());
        assert!(v.get("token").is_none(), "the token stays in its own file");
        relay.stop();
        assert!(!path.exists(), "the endpoint file goes with the relay");

        assert_eq!(port_from(None), Ok(DEFAULT_PORT));
        assert_eq!(port_from(Some("  ")), Ok(DEFAULT_PORT));
        assert_eq!(port_from(Some(" 9123 ")), Ok(9123));
        for bad in ["0", "70000", "not a port"] {
            assert!(port_from(Some(bad)).is_err_and(|e| e.contains(PORT_ENV)), "{bad}");
        }
        let taken = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let e = Relay::start(taken.local_addr().unwrap().port()).unwrap_err();
        assert!(e.contains(PORT_ENV), "{e}");
    }

    /// With full access on, an agent attached with the relay's own token may run the user's own
    /// automation (a script here); without it, or through a grant, that stays refused.
    #[test]
    fn relay_full_access_lifts_the_refusals_for_its_own_token_only() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let script = |id: u64| {
            json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "run_command",
                "arguments": {"command": "script.run", "params": {"script": "layer.addSample {\"name\": \"countries\"}"}}}})
        };
        let ask = |client: &mut TcpStream, s: &mut Session, id: u64| {
            writeln!(client, "{}", script(id)).unwrap();
            client.flush().unwrap();
            settle(&relay, s);
            reply(client)
        };
        let layers = s.map().layers.len();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, true);
        let r = ask(&mut client, &mut s, 1);
        assert!(r["result"]["isError"] == true && r["result"]["content"][0]["text"].as_str().unwrap().contains("reserved"), "{r}");
        relay.set_full_access(true);
        assert!(relay.full_access());
        let r = ask(&mut client, &mut s, 2);
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert_eq!(s.map().layers.len(), layers + 1);
        // A grant (an agent the app started) keeps its own rules.
        let grant = relay.grant(true).unwrap();
        let mut other = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach_with(&mut other, &read_token_file(grant.token_file()).unwrap(), true);
        let r = ask(&mut other, &mut s, 3);
        assert_eq!(r["result"]["isError"], true, "{r}");
        relay.set_full_access(false);
        let r = ask(&mut client, &mut s, 4);
        assert_eq!(r["result"]["isError"], true, "{r}");
        relay.stop();
    }

    fn snapshot_call(id: u64, args: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "snapshot", "arguments": args}})
    }

    /// The metadata a snapshot reply carries beside its image.
    fn snapshot_meta(reply: &Value) -> Value {
        let content = reply["result"]["content"].as_array().cloned().unwrap_or_default();
        assert_eq!(content.first().map(|c| c["type"].clone()), Some(json!("image")), "{reply}");
        serde_json::from_str(content.get(1).and_then(|c| c["text"].as_str()).unwrap_or("null")).unwrap()
    }

    /// A screen snapshot is handed to the app, which captures its window; the relay runs nothing
    /// else until the app answers, then the reply carries the app's image.
    #[test]
    fn relay_hands_screen_snapshots_to_the_app() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let layers = s.map().layers.len();
        relay.set_screen(true);
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        writeln!(client, "{}", snapshot_call(1, json!({"wait_s": 2}))).unwrap();
        writeln!(client, "{}", add_sample_call(2)).unwrap();
        client.flush().unwrap();
        wait_until("the snapshot to reach the app", || {
            relay.poll(&mut s);
            relay.screen_request().is_some()
        });
        assert_eq!(relay.screen_request(), Some(ScreenRequest { wait: Duration::from_secs(2), overlays: true }));
        // The next request waits its turn.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(relay.poll(&mut s), 0);
        assert_eq!(s.map().layers.len(), layers, "nothing runs while the screen is captured");
        let png = rustgis_engine::cmd::share::map_png(&mut s, Some(80.0), Some(50.0), None).unwrap();
        relay.finish_screen(&mut s, Ok(png), true);
        assert!(relay.screen_request().is_none());
        let meta = snapshot_meta(&reply(&client));
        assert_eq!(
            (meta["mode"].clone(), meta["width"].clone(), meta["height"].clone()),
            (json!("screen"), json!(80), json!(50)),
            "{meta}"
        );
        assert_eq!(meta["render_settled"], true, "{meta}");
        assert!(meta["extent"].is_array() && meta["scale"].is_number(), "{meta}");
        settle(&relay, &mut s);
        assert_eq!(reply(&client)["id"], 2);
        assert_eq!(s.map().layers.len(), layers + 1);
        relay.stop();
    }

    /// The actual queued screen completion keeps its original grant and project generation.
    #[test]
    fn permission_policy_delayed_screen_refuses_revocation_and_replacement() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        relay.set_screen(true);
        relay.set_allow_access(true);
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("held.png");
        for (id, replace) in [(1, false), (2, true)] {
            relay.set_allow_access(true);
            writeln!(client, "{}", snapshot_call(id, json!({"save_path":path}))).unwrap();
            client.flush().unwrap();
            wait_until("the held screen request", || {
                relay.poll(&mut s);
                relay.screen_request().is_some()
            });
            if replace {
                s.run("project.new", &json!({"name":"replacement"})).unwrap();
            } else {
                relay.set_allow_access(false);
            }
            // The fallback path is also late work; it must refuse before rendering/writing.
            relay.finish_screen(&mut s, Err("screen unavailable".into()), false);
            let r = reply(&client);
            assert_eq!(r["result"]["isError"], true, "{r}");
            assert!(!path.exists());
            assert!(relay.screen_request().is_none());
            relay.finish_screen(&mut s, Ok(vec![1, 2, 3]), false);
            assert!(!path.exists(), "a replay has no owner claim");
        }
        relay.stop();
    }

    /// Without the map on screen, or when the app can't capture it, a snapshot is drawn
    /// offscreen and says so; `mode: render` never waits for the app.
    #[test]
    fn relay_draws_snapshots_offscreen_without_the_screen() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        // No map on screen: drawn at once, with a note when the screen was asked for.
        relay.set_screen(false);
        writeln!(client, "{}", snapshot_call(1, json!({"mode": "screen", "max_width": 200}))).unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        let meta = snapshot_meta(&reply(&client));
        assert_eq!((meta["mode"].clone(), meta["width"].clone()), (json!("render"), json!(200)), "{meta}");
        assert!(meta["note"].as_str().is_some_and(|n| n.contains("isn't on screen")), "{meta}");
        // On screen but asked to render: drawn offscreen, no note.
        relay.set_screen(true);
        writeln!(client, "{}", snapshot_call(2, json!({"mode": "render"}))).unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        let meta = snapshot_meta(&reply(&client));
        assert!(meta["mode"] == "render" && meta["note"].is_null(), "{meta}");
        assert!(relay.screen_request().is_none());
        // The app couldn't capture it: drawn offscreen.
        writeln!(client, "{}", snapshot_call(3, json!({}))).unwrap();
        client.flush().unwrap();
        wait_until("the snapshot to reach the app", || {
            relay.poll(&mut s);
            relay.screen_request().is_some()
        });
        relay.finish_screen(&mut s, Err("the window's screenshot didn't come back".into()), false);
        let meta = snapshot_meta(&reply(&client));
        assert_eq!(meta["mode"], "render", "{meta}");
        assert_eq!(meta["note"], "the window's screenshot didn't come back", "the note says why: {meta}");
        assert_eq!(relay.last_action().map(|a| a.label), Some("snapshot".to_string()), "a snapshot is the agent's last call too");
        relay.stop();
    }

    /// A snapshot saved to a file is refused without file access before the app is asked to
    /// capture anything.
    #[test]
    fn relay_refuses_a_saved_snapshot_without_access() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        relay.set_screen(true);
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        let out = std::env::temp_dir().join(format!("rustgis-snap-{}.png", std::process::id()));
        writeln!(client, "{}", snapshot_call(1, json!({"save_path": out.display().to_string()}))).unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        let r = reply(&client);
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert!(r["result"]["content"][0]["text"].as_str().is_some_and(|t| t.contains("--yes")), "{r}");
        assert!(relay.screen_request().is_none() && !out.exists());
        relay.stop();
    }

    /// The relay remembers the last tool call an agent ran, and whether it worked, for the app
    /// to show; an agent attached to it is told the user is watching.
    #[test]
    fn relay_records_the_agents_last_call() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        assert!(relay.last_action().is_none());
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
        writeln!(client, "{init}").unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        let r = reply(&client);
        assert!(r["result"]["instructions"].as_str().is_some_and(|i| i.contains("watch the map")), "{r}");
        assert!(relay.last_action().is_none(), "initialize is not a tool call");
        writeln!(client, "{}", add_sample_call(2)).unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        reply(&client);
        let a = relay.last_action().unwrap();
        assert_eq!(a.label, "layer.addSample");
        assert!(a.ok && a.detail.contains("countries"), "{a:?}");
        let bad = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "run_command", "arguments": {"command": "map.basemap", "params": {"name": "no-such-basemap"}}
        }});
        writeln!(client, "{bad}").unwrap();
        client.flush().unwrap();
        settle(&relay, &mut s);
        reply(&client);
        let a = relay.last_action().unwrap();
        assert_eq!(a.label, "map.basemap");
        assert!(!a.ok, "a failed call is shown as failed: {a:?}");
        relay.stop();
    }

    /// Attaches with any token, asking for file and network access or not.
    fn attach_with(client: &mut TcpStream, token: &str, access: bool) {
        let hello = json!({"jsonrpc": "2.0", "method": "rustgis/attach", "params": {"access": access, "token": token}});
        writeln!(client, "{hello}").unwrap();
        client.flush().unwrap();
    }

    /// Asks the relay to export the map to `out`; the reply.
    fn export(client: &mut TcpStream, relay: &Relay, s: &mut Session, out: &std::path::Path, id: u64) -> Value {
        let call = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
            "name": "run_command",
            "arguments": {"command": "share.exportMap", "params": {"path": out.display().to_string()}}
        }});
        writeln!(client, "{call}").unwrap();
        client.flush().unwrap();
        settle(relay, s);
        reply(client)
    }

    /// With agents turned on in the app, an attached agent may use files and the network without
    /// asking (`--yes`); turned off, what it asked for decides, as before.
    #[test]
    fn relay_app_lets_agents_use_files_without_yes() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let out = std::env::temp_dir().join(format!("rustgis-relay-app-access-{}.png", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        // Not asked, not allowed by the app: refused, pointing at --yes (as before).
        let r = export(&mut client, &relay, &mut s, &out, 1);
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert!(!out.exists());
        // The app allows it: the same connection's next call runs, without --yes.
        relay.set_allow_access(true);
        let r = export(&mut client, &relay, &mut s, &out, 2);
        assert_ne!(r["result"]["isError"], true, "{r}");
        assert!(out.exists(), "the export didn't run once the app allowed it");
        let _ = std::fs::remove_file(&out);
        // Turned off again, it's refused again; a client that asked still may.
        relay.set_allow_access(false);
        assert_eq!(export(&mut client, &relay, &mut s, &out, 3)["result"]["isError"], true);
        let mut asked = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut asked, &relay, true);
        assert_ne!(export(&mut asked, &relay, &mut s, &out, 4)["result"]["isError"], true);
        let _ = std::fs::remove_file(&out);
        relay.stop();
    }

    /// A grant is a run-scoped token with its own access; dropping it revokes the token, removes
    /// its file and closes its connections.
    #[test]
    fn relay_grants_are_scoped_and_revocable() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let out = std::env::temp_dir().join(format!("rustgis-relay-grant-{}.png", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let full = relay.grant(true).unwrap();
        let map_only = relay.grant(false).unwrap();
        let token = read_token_file(full.token_file()).unwrap();
        assert_ne!(token, read_token(relay.port()).unwrap(), "a grant has a token of its own");
        // The full grant may export, though the client didn't ask and the app's setting is off.
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach_with(&mut client, &token, false);
        assert_ne!(export(&mut client, &relay, &mut s, &out, 1)["result"]["isError"], true);
        assert!(out.exists());
        let _ = std::fs::remove_file(&out);
        // The map-only grant may not, even when the client asks.
        let mut limited = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach_with(&mut limited, &read_token_file(map_only.token_file()).unwrap(), true);
        assert_eq!(export(&mut limited, &relay, &mut s, &out, 2)["result"]["isError"], true);
        assert!(!out.exists());
        // Revoked: the file goes, the connection closes, and the token no longer attaches.
        let file = full.token_file().to_path_buf();
        drop(full);
        assert!(!file.exists());
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut line = String::new();
        assert_eq!(
            BufReader::new(client.try_clone().unwrap()).read_line(&mut line).unwrap_or(0),
            0,
            "a revoked grant's connection stayed open"
        );
        let mut again = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach_with(&mut again, &token, true);
        again.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut line = String::new();
        assert_eq!(BufReader::new(again.try_clone().unwrap()).read_line(&mut line).unwrap_or(0), 0, "a revoked token attached");
        // Stopping the relay removes the other grant's file, and refuses new grants.
        let other = map_only.token_file().to_path_buf();
        relay.stop();
        assert!(!other.exists());
        assert!(relay.grant(true).is_err(), "a grant after the relay stopped");
        // Grant files carry the run's id: another run on the same port names its own.
        wait_until("the port to close", || TcpStream::connect(("127.0.0.1", relay.port())).is_err());
        let next = Relay::start(relay.port()).unwrap();
        let fresh = next.grant(true).unwrap();
        assert_ne!(fresh.token_file(), other.as_path());
        next.stop();
    }

    /// Permission is decided when the app runs a request: one queued before its grant was revoked
    /// never runs. A connection that re-attached with the relay's own token isn't the grant's.
    #[test]
    fn relay_revocation_reaches_queued_requests_and_spares_reattached_connections() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let out = std::env::temp_dir().join(format!("rustgis-relay-queued-{}.png", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let grant = relay.grant(true).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach_with(&mut client, &read_token_file(grant.token_file()).unwrap(), false);
        let call = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "run_command",
            "arguments": {"command": "share.exportMap", "params": {"path": out.display().to_string()}}
        }});
        writeln!(client, "{call}").unwrap();
        client.flush().unwrap();
        // Queued, not yet run: the grant goes, then the app's frame comes.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while lock(&relay.owner.shared.requests).is_empty() {
            assert!(std::time::Instant::now() < deadline, "the request was never queued");
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(grant);
        assert_eq!(relay.poll(&mut s), 0, "a request queued before its grant was revoked ran");
        assert!(!out.exists());
        // Attached with a grant, then re-attached with the relay's own token: revoking the grant
        // leaves it open.
        let other = relay.grant(true).unwrap();
        let mut moved = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach_with(&mut moved, &read_token_file(other.token_file()).unwrap(), false);
        attach(&mut moved, &relay, true);
        // A round trip: the re-attach line has been handled once this is answered.
        writeln!(moved, "{}", json!({"jsonrpc": "2.0", "id": 9, "method": "tools/list"})).unwrap();
        moved.flush().unwrap();
        settle(&relay, &mut s);
        assert!(reply(&moved)["result"]["tools"].is_array());
        drop(other);
        let r = export(&mut moved, &relay, &mut s, &out, 2);
        assert_ne!(r["result"]["isError"], true, "a connection on the relay's own token was closed with the grant: {r}");
        let _ = std::fs::remove_file(&out);
        relay.stop();
    }

    /// A request waiting for the user's approval outlasts the app's reply wait: its card stays,
    /// and once allowed it runs and answers on the same connection.
    #[test]
    fn approval_held_requests_wait_for_the_user() {
        // Once allowed, the export runs on this thread while the client waits at most two reply
        // waits: render once first (fonts load on the first render), and give the run a second.
        let relay = Relay::bind_with(SocketAddr::from(([127, 0, 0, 1], 0)), Duration::from_secs(1), BUSY_AFTER).unwrap();
        let grant = relay.grant_reviewed(false).unwrap();
        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("held.png");
        let mut s = sample();
        s.run("share.exportMap", &json!({"path": root.path().join("warm.png"), "width": 64, "height": 64})).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach_with(&mut client, &read_token_file(grant.token_file()).unwrap(), true);
        let call = json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"run_command","arguments":{"command":"share.exportMap","params":{"path":out,"width":64,"height":64}}}});
        writeln!(client, "{call}").unwrap();
        client.flush().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while grant.pending().is_empty() {
            assert!(std::time::Instant::now() < deadline, "the request never asked for approval");
            relay.poll(&mut s);
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(1500));
        relay.poll(&mut s);
        let pending = grant.pending();
        assert_eq!(pending.len(), 1, "the approval expired with the app's reply wait");
        grant.decide(pending[0].id, true);
        settle(&relay, &mut s);
        let r = reply(&client);
        assert_eq!(r["id"], 4, "{r}");
        assert!(r.get("error").is_none() && out.exists(), "{r}");
        relay.stop();
    }

    /// A command whose reply times out is cancelled: the client gets an error, and the app's
    /// next frame does not run it.
    #[test]
    fn relay_cancels_a_timed_out_command_so_it_never_runs() {
        let relay = Relay::bind_with(SocketAddr::from(([127, 0, 0, 1], 0)), Duration::from_millis(100), BUSY_AFTER).unwrap();
        let mut s = sample();
        let layers = s.map().layers.len();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        writeln!(client, "{}", add_sample_call(3)).unwrap();
        client.flush().unwrap();
        // Nobody polls, so the wait runs out and the client is told.
        let timed_out = reply(&client);
        assert_eq!(timed_out["id"], 3, "{timed_out}");
        assert_eq!(timed_out["error"]["code"], -32002, "{timed_out}");
        // The app's frame now finds the queue: the cancelled command must not run.
        assert_eq!(relay.poll(&mut s), 0, "a cancelled command ran");
        assert_eq!(s.map().layers.len(), layers, "a cancelled command changed the session");
        relay.stop();
    }

    /// Stopping closes the listener and every client, removes the token, and drops the
    /// attached count; a temporary clone that drops first does not stop the relay.
    #[test]
    fn relay_stop_closes_clients_and_removes_the_token() {
        let relay = Relay::start(0).unwrap();
        let port = relay.port();
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        attach(&mut client, &relay, false);
        wait_until("the attach", || relay.attached_clients() == 1);
        assert!(token_path(port).exists(), "the token file was not written");
        drop(relay.clone());
        assert!(relay.is_running(), "a dropped temporary clone stopped the relay");
        relay.stop();
        assert!(!relay.is_running());
        assert!(!token_path(port).exists(), "the token file survived stop");
        wait_until("the attached count to fall", || relay.attached_clients() == 0);
        let mut input = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        assert_eq!(input.read_line(&mut line).unwrap(), 0, "a client stayed connected after stop: {line}");
        wait_until("the port to close", || TcpStream::connect(("127.0.0.1", port)).is_err());
    }

    /// The last owner's drop stops the relay: no thread keeps the listener, and the token goes.
    #[test]
    fn relay_last_owner_drop_closes_the_listener() {
        let relay = Relay::start(0).unwrap();
        let port = relay.port();
        let copy = relay.clone();
        drop(relay);
        assert!(copy.is_running(), "dropping one owner stopped a relay another clone still holds");
        drop(copy);
        wait_until("the port to close", || TcpStream::connect(("127.0.0.1", port)).is_err());
        assert!(!token_path(port).exists(), "the token file survived the last drop");
    }

    /// Unauthenticated connections are capped: one more than the limit is closed at once.
    #[test]
    fn relay_caps_unauthenticated_connections() {
        let relay = Relay::start(0).unwrap();
        let port = relay.port();
        let held: Vec<TcpStream> = (0..MAX_PREAUTH_CONNECTIONS).map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap()).collect();
        let extra = TcpStream::connect(("127.0.0.1", port)).unwrap();
        extra.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut line = String::new();
        let n = BufReader::new(extra).read_line(&mut line);
        assert!(matches!(n, Ok(0)), "the relay kept a connection over the unauthenticated limit: {n:?} {line}");
        drop(held);
        relay.stop();
    }

    /// A line longer than the pre-attach limit is refused without being buffered: no attach,
    /// no queued message, and the connection is closed.
    #[test]
    fn relay_refuses_an_oversized_unauthenticated_line() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        let big = vec![b'a'; 1 << 20];
        // The relay closes once the line is too long, so the rest of the write may fail.
        let _ = client.write_all(&big);
        let _ = client.write_all(b"\n");
        let mut input = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        let _ = input.read_line(&mut line);
        assert!(line.is_empty() || line.contains("-32700"), "the relay answered an oversized line: {line}");
        assert_eq!(relay.poll(&mut s), 0);
        assert_eq!(relay.attached_clients(), 0);
        relay.stop();
    }

    /// Bare relative grant paths work in a private directory and retain all permission checks.
    #[cfg(unix)]
    #[test]
    fn relative_token_file_uses_its_current_directory() {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        if std::env::var_os("RUSTGIS_TEST_RELATIVE_TOKEN").is_some() {
            let file = std::path::Path::new("grant.token");
            assert_eq!(read_token_file(file).unwrap(), "synthetic-token");
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(read_token_file(file).unwrap_err().contains("readable by others"));
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::set_permissions(".", std::fs::Permissions::from_mode(0o750)).unwrap();
            assert!(read_token_file(file).unwrap_err().contains("readable by others"));
            std::fs::set_permissions(".", std::fs::Permissions::from_mode(0o700)).unwrap();
            std::os::unix::fs::symlink("grant.token", "link.token").unwrap();
            assert!(read_token_file(std::path::Path::new("link.token")).is_err());
            let missing = read_token_file(std::path::Path::new("missing.token")).unwrap_err();
            assert!(missing.contains("--token-file") && missing.contains("relay or grant"));
            return;
        }
        // A child process isolates the current directory from tests running in parallel.
        let dir = std::env::temp_dir().join(format!("rustgis-relative-token-{}", random_token().unwrap()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(dir.join("grant.token")).unwrap();
        file.write_all(b"synthetic-token").unwrap();
        drop(file);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "relay::tests::relative_token_file_uses_its_current_directory", "--nocapture"])
            .env("RUSTGIS_TEST_RELATIVE_TOKEN", "1")
            .current_dir(&dir)
            .status()
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(status.success());
    }

    /// The token is 32 hex characters from the operating system, differs per run, and is
    /// readable by this user only; its folder is private too.
    #[test]
    fn relay_token_is_random_and_private() {
        let a = Relay::start(0).unwrap();
        let ta = token(&a);
        assert_eq!(ta.len(), 32, "{ta}");
        assert!(ta.bytes().all(|b| b.is_ascii_hexdigit()), "{ta}");
        let b = Relay::start(0).unwrap();
        assert_ne!(ta, token(&b), "two runs drew the same token");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_mode = std::fs::metadata(token_path(a.port())).unwrap().permissions().mode();
            assert_eq!(file_mode & 0o077, 0, "the token file is readable by others: {file_mode:o}");
            let dir_mode = std::fs::metadata(token_dir()).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o077, 0, "the token folder is readable by others: {dir_mode:o}");
        }
        a.stop();
        b.stop();
    }

    /// The count of authenticated clients follows attach and disconnect, and each change wakes
    /// the UI so the indicator repaints.
    #[test]
    fn relay_counts_authenticated_clients_and_wakes_on_change() {
        let relay = Relay::start(0).unwrap();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        relay.set_wake(Box::new(move || {
            counted.fetch_add(1, SeqCst);
        }));
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        // The relay changes the count, then wakes the UI: wait for both, as the test thread can
        // see the new count before the wake.
        wait_until("the attach", || relay.attached_clients() == 1);
        wait_until("the attach to wake the UI", || wakes.load(SeqCst) >= 1);
        let before = wakes.load(SeqCst);
        drop(client);
        wait_until("the disconnect", || relay.attached_clients() == 0);
        wait_until("the disconnect to wake the UI", || wakes.load(SeqCst) > before);
        relay.stop();
    }

    /// Stopping while a client waits for its reply ends the wait at once (not after the reply
    /// wait), closes the client, and the queued command never runs afterwards.
    #[test]
    fn relay_stop_ends_a_reply_wait_and_the_command_never_runs() {
        let relay = Relay::start(0).unwrap();
        let mut s = sample();
        let layers = s.map().layers.len();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        writeln!(client, "{}", add_sample_call(4)).unwrap();
        client.flush().unwrap();
        wait_until("the request to queue", || lock(&relay.owner.shared.requests).len() == 1);
        let started = Instant::now();
        relay.stop();
        client.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut line = String::new();
        let n = BufReader::new(client.try_clone().unwrap()).read_line(&mut line);
        assert!(matches!(n, Ok(0)) || n.is_err(), "a client stayed connected after stop: {n:?} {line}");
        assert!(started.elapsed() < Duration::from_secs(5), "the reply wait outlived the stop");
        assert_eq!(relay.poll(&mut s), 0, "a command ran after stop");
        assert_eq!(s.map().layers.len(), layers, "a command changed the session after stop");
    }

    /// A command the app took but did not finish in time is reported as unknown, not as
    /// cancelled, and the wait still ends.
    #[test]
    fn relay_reports_a_running_command_past_its_wait_as_unknown() {
        let relay = Relay::bind_with(SocketAddr::from(([127, 0, 0, 1], 0)), Duration::from_secs(1), BUSY_AFTER).unwrap();
        let mut client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        attach(&mut client, &relay, false);
        writeln!(client, "{}", add_sample_call(5)).unwrap();
        client.flush().unwrap();
        wait_until("the request to queue", || lock(&relay.owner.shared.requests).len() == 1);
        // The app's frame claims it and is still running it: its reply channel stays open.
        let running = lock(&relay.owner.shared.requests).pop_front().unwrap();
        assert!(running.state.compare_exchange(PENDING, RUNNING, SeqCst, SeqCst).is_ok());
        let unknown = reply(&client);
        assert_eq!(unknown["id"], 5, "{unknown}");
        assert_eq!(unknown["error"]["code"], -32003, "{unknown}");
        drop(running);
        relay.stop();
    }

    /// The attach deadline is absolute: a client that sends one byte at a time, never a whole
    /// line, is still closed once the handshake time runs out.
    #[test]
    fn relay_closes_a_trickling_handshake_at_its_deadline() {
        let relay = Relay::start(0).unwrap();
        let client = TcpStream::connect(("127.0.0.1", relay.port())).unwrap();
        let mut writer = client.try_clone().unwrap();
        let trickle = std::thread::spawn(move || {
            for _ in 0..40 {
                if writer.write_all(b" ").and_then(|()| writer.flush()).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
        });
        let started = Instant::now();
        client.set_read_timeout(Some(PREAUTH_TIMEOUT * 3)).unwrap();
        let mut line = String::new();
        let n = BufReader::new(client.try_clone().unwrap()).read_line(&mut line);
        assert!(matches!(n, Ok(0)) || n.is_err(), "the relay kept a trickling handshake: {n:?} {line}");
        assert!(started.elapsed() < PREAUTH_TIMEOUT * 2, "the handshake outlived its deadline: {:?}", started.elapsed());
        assert_eq!(relay.attached_clients(), 0);
        let _ = client.shutdown(Shutdown::Both);
        let _ = trickle.join();
        relay.stop();
    }

    /// An approval shows the whole request: padding that sorts first can't push the path out of
    /// view, and a request too long to show says so.
    #[test]
    fn approvals_show_the_whole_request() {
        let s = Session::new(Project::default());
        let call = |params: Value| {
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "run_command", "arguments": {"command": "share.exportMap", "params": params}}})
                .to_string()
        };
        let scope = Scope::agent(AgentAccess::Review, true, "review-fixture", None, std::sync::Arc::new(crate::tools::Live::default()));
        let summary = |line: &str| approval_plans(&s, line, &scope, "request").map(|(text, _)| text);
        let padded = summary(&call(json!({"a": "a".repeat(400), "path": "/home/u/.profile"}))).unwrap();
        assert!(padded.contains("/home/u/.profile"), "{padded}");
        let huge = call(json!({"a": "a".repeat(5000), "path": "/home/u/.profile"}));
        assert!(summary(&huge).is_none(), "an unshowable request is refused before approval");
        let raw: Value = serde_json::from_str(&huge).unwrap();
        let engine = crate::tools::policy_call(&crate::provider::ToolCall {
            id: "request/1".into(),
            name: "run_command".into(),
            args: raw["params"]["arguments"].clone(),
        })
        .unwrap()
        .unwrap();
        assert!(matches!(s.review(&scope, &engine), Outcome::Refused(e) if e.contains("too long to show in full")));
    }
}
