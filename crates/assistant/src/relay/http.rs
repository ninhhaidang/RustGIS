//! MCP over HTTP ("Streamable HTTP", stateless, JSON replies) for the relay: clients that speak
//! HTTP (Claude Code's `--transport http`, others) reach the open project without
//! `rustgis-cli mcp --attach`. Each POST carries one JSON-RPC message or batch; it goes through
//! the same queue as an attached client's, runs on the app's frame and gets the server's reply.
//!
//! Safety: the listener binds 127.0.0.1 only. Every request needs `Authorization: Bearer
//! <token>`, compared in constant time; the token is stable across launches (kept in the
//! settings folder, readable by this user only; deleting the file makes a new one). A request
//! that names another host (DNS rebinding) or carries an `Origin` (a web page) is refused before
//! the token is read. Heads, bodies, waits and open connections are bounded.

use std::io::{BufRead, BufReader};
use std::path::Path;

use super::*;

/// The port the HTTP listener takes unless [`HTTP_PORT_ENV`] names another (beside the relay's).
pub const HTTP_PORT: u16 = 8768;
/// The environment variable that changes [`HTTP_PORT`].
pub const HTTP_PORT_ENV: &str = "RUSTGIS_MCP_HTTP_PORT";
/// The path clients post to (`/` works too).
pub const HTTP_PATH: &str = "/mcp";
/// The largest request head and body read.
const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 16 * 1024 * 1024;
/// How long a client may take to send its whole request (head and body), and the reply to be
/// written: a client trickling bytes can't hold a request slot past it.
const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// Requests not yet authenticated at once.
const MAX_HTTP_CONNECTIONS: usize = 8;
/// Authenticated requests in progress at once (each holds a thread while it waits for the app).
const MAX_HTTP_IN_FLIGHT: usize = 16;

/// The port the HTTP listener takes: [`HTTP_PORT_ENV`] when it holds a port number, else
/// [`HTTP_PORT`].
pub fn default_http_port() -> u16 {
    std::env::var(HTTP_PORT_ENV).ok().and_then(|v| v.trim().parse::<u16>().ok()).filter(|p| *p != 0).unwrap_or(HTTP_PORT)
}

/// The file that keeps the HTTP token, in the settings folder.
pub fn http_token_path() -> Option<PathBuf> {
    rustgis_engine::data::dirs::config_dir().map(|d| d.join("mcp-http.token"))
}

/// The HTTP token: the one kept in [`http_token_path`], or a new one written there (readable by
/// this user only) when there is none. Clients registered with it keep working across launches.
pub fn http_token() -> Result<String, String> {
    let path = http_token_path().ok_or("no settings folder for the MCP HTTP token")?;
    token_at(&path)
}

/// The token in `path`, made when the file doesn't exist. A few tries at most: another process
/// making the file at the same moment (the app, `mcp-register`) is read back, not raced forever.
fn token_at(path: &Path) -> Result<String, String> {
    for _ in 0..3 {
        // A file left empty (a write that never finished, a few seconds ago at least, so not one
        // another writer is filling now) is no token: a new one is made.
        let abandoned =
            |m: &std::fs::Metadata| m.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > Duration::from_secs(5));
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() == 0 && abandoned(&m)) {
            let _ = std::fs::remove_file(path);
        }
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            // Empty and young: another writer is filling it; wait, then read it.
            if meta.is_file() && meta.len() == 0 {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            return read_token_at(path, &meta);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let token = random_token()?;
        // Written beside it and renamed into place, so no reader ever sees half a token.
        // Unique to this process and thread, so two writers in one process don't share it.
        let thread = format!("{:?}", std::thread::current().id()).replace(|c: char| !c.is_ascii_alphanumeric(), "");
        let tmp = path.with_file_name(format!(".mcp-http.token.{}.{thread}", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        let written = o.open(&tmp).and_then(|mut f| f.write_all(format!("{token}\n").as_bytes()));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("{}: {e}", tmp.display()));
        }
        // Published only if there is none yet, in one step (a hard link fails when the name is
        // taken), so two writers never end up holding different tokens: the loser reads the
        // winner's on the next round.
        let linked = std::fs::hard_link(&tmp, path);
        match linked {
            Ok(()) => {
                let _ = std::fs::remove_file(&tmp);
                return Ok(token);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = std::fs::remove_file(&tmp);
                continue;
            }
            // A folder without hard links (FAT, some network mounts): the file is made in place
            // with create-new, also create-if-absent in one step, and what ends up there is read
            // back, so two writers still agree.
            Err(_) => {
                let _ = std::fs::remove_file(&tmp);
                let mut o = std::fs::OpenOptions::new();
                o.write(true).create_new(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
                match o.open(path) {
                    Ok(mut f) => {
                        f.write_all(format!("{token}\n").as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
                        return Ok(token);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                        // Another writer's: let it finish, then read it.
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                    Err(e) => return Err(format!("{}: {e}", path.display())),
                }
            }
        }
    }
    Err(format!("{}: the token file kept changing; try again", path.display()))
}

/// The token in an existing file: a small regular file of this user's, private to them (on Unix;
/// on Windows the user profile's permissions protect it).
fn read_token_at(path: &Path, meta: &std::fs::Metadata) -> Result<String, String> {
    if !meta.is_file() || meta.len() > 1024 {
        return Err(format!("{}: not a token file; refused", path.display()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.uid() != rustix::process::geteuid().as_raw() || meta.permissions().mode() & 0o077 != 0 {
            return Err(format!("{}: readable by others or not this user's; remove it to make a new token", path.display()));
        }
    }
    let token = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?.trim().to_string();
    if token.len() >= 32 && token.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Ok(token);
    }
    Err(format!("{}: holds no token; remove it to make a new one", path.display()))
}

impl Relay {
    /// Starts MCP over HTTP on 127.0.0.1:`port` (0 picks one) beside the relay, with the stable
    /// token ([`http_token`]). It stops with the relay. Returns the port: a listener already
    /// running keeps its own, whatever port is asked. Called from the app's thread only.
    pub fn serve_http(&self, port: u16) -> Result<u16, String> {
        self.serve_http_with(port, http_token()?)
    }

    /// The HTTP listener's port, while it runs.
    pub fn http_port(&self) -> Option<u16> {
        *lock(&self.owner.shared.http_port)
    }

    pub(super) fn serve_http_with(&self, port: u16, token: String) -> Result<u16, String> {
        self.serve_http_timed(port, token, IO_TIMEOUT)
    }

    /// [`Relay::serve_http_with`] with the time a request may take to arrive (tests).
    fn serve_http_timed(&self, port: u16, token: String, io: Duration) -> Result<u16, String> {
        let shared = Arc::clone(&self.owner.shared);
        if let Some(port) = *lock(&shared.http_port) {
            return Ok(port);
        }
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = TcpListener::bind(addr).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                format!("{addr}: the port is in use; set {HTTP_PORT_ENV} to another port")
            } else {
                format!("{addr}: {e}")
            }
        })?;
        let port = listener.local_addr().map_err(|e| format!("{addr}: {e}"))?.port();
        listener.set_nonblocking(true).map_err(|e| format!("{addr}: {e}"))?;
        let token = Arc::new(token);
        let active = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new()
            .name("rustgis-mcp-http".into())
            .spawn(move || {
                while !shared.stopped.load(SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            if active.fetch_add(1, SeqCst) >= MAX_HTTP_CONNECTIONS {
                                active.fetch_sub(1, SeqCst);
                                // Said, not dropped silently, without letting a stalled peer hold
                                // up the listener.
                                let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                                respond(
                                    &mut stream,
                                    "503 Service Unavailable",
                                    &[("Retry-After", "1")],
                                    &problem("too many requests at once"),
                                );
                                continue;
                            }
                            let (request_shared, request_token, request_active) =
                                (Arc::clone(&shared), Arc::clone(&token), Arc::clone(&active));
                            let request_in_flight = Arc::clone(&in_flight);
                            let spawned = std::thread::Builder::new().name("rustgis-mcp-http-request".into()).spawn(move || {
                                serve(
                                    stream,
                                    &request_shared,
                                    &request_token,
                                    port,
                                    io,
                                    Slot { active: request_active, held: true },
                                    &request_in_flight,
                                );
                            });
                            if let Err(e) = spawned {
                                active.fetch_sub(1, SeqCst);
                                log::warn!("MCP over HTTP: {e}");
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(STOP_POLL),
                        Err(e) => {
                            log::warn!("MCP over HTTP: {e}");
                            std::thread::sleep(STOP_POLL);
                        }
                    }
                }
            })
            .map_err(|e| format!("MCP over HTTP could not start: {e}"))?;
        *lock(&self.owner.shared.http_port) = Some(port);
        // An endpoint file already published now names the HTTP address too.
        let published = lock(&self.owner.shared.endpoint).as_ref().and_then(|p| p.file_name()).and_then(|n| n.to_str()).map(str::to_string);
        if let Some(name) = published {
            let _ = self.publish_endpoint_as(&name);
        }
        Ok(port)
    }
}

/// A request's reads under one deadline for the whole request: each read waits only for the time
/// left, so a client sending a byte at a time is cut off when it runs out.
struct Deadline {
    stream: TcpStream,
    until: Instant,
}

impl Read for Deadline {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "the request took too long"));
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

/// An HTTP response, then the connection closes.
fn respond(stream: &mut TcpStream, status: &str, extra: &[(&str, &str)], body: &str) {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(body.as_bytes())).and_then(|()| stream.flush());
}

/// A JSON error body for a refused request.
fn problem(message: &str) -> String {
    serde_json::json!({"error": message}).to_string()
}

/// A request's place among the [`MAX_HTTP_CONNECTIONS`] not yet authenticated: given back once
/// the token checks out (an authenticated request waits in the relay's bounded queue instead), or
/// when the request ends.
struct Slot {
    active: Arc<AtomicUsize>,
    held: bool,
}

impl Slot {
    fn release(&mut self) {
        if std::mem::replace(&mut self.held, false) {
            self.active.fetch_sub(1, SeqCst);
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.release();
    }
}

/// An authenticated request's place among [`MAX_HTTP_IN_FLIGHT`], given back when it ends.
struct InFlight(Arc<AtomicUsize>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, SeqCst);
    }
}

/// One HTTP request: checked, queued for the app, answered with its reply. `io` bounds the whole
/// request's arrival and the reply's writing.
fn serve(stream: TcpStream, shared: &Arc<Shared>, token: &str, port: u16, io: Duration, mut slot: Slot, in_flight: &Arc<AtomicUsize>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_write_timeout(Some(io));
    let Ok(mut out) = stream.try_clone() else { return };
    let mut input = BufReader::new(Deadline { stream, until: Instant::now() + io });
    // The head: the request line and headers, bounded.
    let mut lines = Vec::new();
    let mut read = 0;
    loop {
        let mut line = String::new();
        match (&mut input).take((MAX_HEAD - read.min(MAX_HEAD)) as u64 + 1).read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(n) => read += n,
        }
        if read > MAX_HEAD {
            return respond(&mut out, "431 Request Header Fields Too Large", &[], &problem("the request head is too large"));
        }
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let Some((request_line, header_lines)) = lines.split_first() else { return };
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let headers: HashMap<String, String> =
        header_lines.iter().filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
    let header = |k: &str| headers.get(k).map(String::as_str);
    // A header that decides the request given twice is ambiguous (which length? which host?).
    let twice =
        |k: &str| header_lines.iter().filter(|l| l.split_once(':').is_some_and(|(n, _)| n.trim().eq_ignore_ascii_case(k))).count() > 1;
    if ["content-length", "host", "authorization", "transfer-encoding"].iter().any(|k| twice(k)) {
        return respond(&mut out, "400 Bad Request", &[], &problem("a header that decides the request is given twice"));
    }
    // Another host name (DNS rebinding) or a web page's request: refused before anything else.
    let host_ok = header("host").is_some_and(|h| {
        [format!("127.0.0.1:{port}"), format!("localhost:{port}"), format!("[::1]:{port}")].iter().any(|ok| h.eq_ignore_ascii_case(ok))
    });
    if !host_ok {
        return respond(&mut out, "403 Forbidden", &[], &problem("the Host header must name this computer's loopback address and port"));
    }
    if header("origin").is_some() {
        return respond(&mut out, "403 Forbidden", &[], &problem("requests from web pages are refused"));
    }
    // The scheme's name in any case (RFC 7235), then the token.
    let given = header("authorization")
        .and_then(|a| a.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim())
        .unwrap_or("");
    if !same_token(given, token) {
        return respond(&mut out, "401 Unauthorized", &[("WWW-Authenticate", "Bearer")], &problem("a valid bearer token is required"));
    }
    // Authenticated: its place among the unauthenticated goes back, so idle connections can't
    // keep a real client out while it waits for the app; it takes one among the in-flight.
    slot.release();
    if in_flight.fetch_add(1, SeqCst) >= MAX_HTTP_IN_FLIGHT {
        in_flight.fetch_sub(1, SeqCst);
        return respond(&mut out, "503 Service Unavailable", &[("Retry-After", "1")], &problem("too many requests at once"));
    }
    let _in_flight = InFlight(Arc::clone(in_flight));
    let path = target.split('?').next().unwrap_or("");
    if path != HTTP_PATH && path != "/" {
        return respond(&mut out, "404 Not Found", &[], &problem("post MCP messages to /mcp"));
    }
    if method != "POST" {
        // Stateless: no server-sent event stream, no sessions to delete.
        return respond(&mut out, "405 Method Not Allowed", &[("Allow", "POST")], &problem("this server takes POST only"));
    }
    if header("transfer-encoding").is_some() {
        return respond(&mut out, "411 Length Required", &[], &problem("send the body with a Content-Length"));
    }
    // Digits only: no sign, nothing a parser could read two ways.
    let Some(len) =
        header("content-length").filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit())).and_then(|v| v.parse::<usize>().ok())
    else {
        return respond(&mut out, "411 Length Required", &[], &problem("send the body with a Content-Length"));
    };
    if len > MAX_BODY {
        return respond(&mut out, "413 Content Too Large", &[], &problem("the body is too large"));
    }
    // A client waiting for leave to send its body (curl does for large ones) gets it now.
    if header("expect").is_some_and(|e| e.eq_ignore_ascii_case("100-continue")) {
        let _ = out.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").and_then(|()| out.flush());
    }
    // Read as it arrives, not allocated whole before a byte comes.
    let mut body = Vec::new();
    if (&mut input).take(len as u64).read_to_end(&mut body).is_err() || body.len() != len {
        return;
    }
    let Ok(line) = String::from_utf8(body) else {
        return respond(&mut out, "400 Bad Request", &[], &problem("the body is not UTF-8"));
    };
    if shared.stopped.load(SeqCst) {
        return respond(&mut out, "503 Service Unavailable", &[], &problem("the MCP relay has stopped"));
    }
    match submit(shared, line) {
        Submitted::Reply(reply) => respond(&mut out, "200 OK", &[], &reply),
        Submitted::Accepted => respond(&mut out, "202 Accepted", &[], ""),
        Submitted::Failed(status, why) => respond(&mut out, status, &[], &problem(&why)),
    }
}

/// How a request through the queue went.
enum Submitted {
    Reply(String),
    /// A notification (or response): nothing to answer.
    Accepted,
    Failed(&'static str, String),
}

/// Queues `line` as the relay's own client and waits for the app's reply, cancelling it if the
/// app hasn't started it in time, as an attached client's request is.
fn submit(shared: &Arc<Shared>, line: String) -> Submitted {
    let expects = expects_reply(&line);
    let state = Arc::new(AtomicU8::new(PENDING));
    let (tx, rx) = sync_channel(1);
    {
        let mut queue = lock(&shared.requests);
        // Checked under the lock `shutdown` drains, as for an attached client.
        if shared.stopped.load(SeqCst) {
            return Submitted::Failed("503 Service Unavailable", "the MCP relay has stopped".into());
        }
        if queue.len() >= MAX_QUEUE {
            return Submitted::Failed("503 Service Unavailable", "the app is busy; try again".into());
        }
        queue.push_back(Request {
            plans: Vec::new(),
            approval_required: false,
            held: Arc::new(AtomicBool::new(false)),
            id: shared.next_id.fetch_add(1, SeqCst),
            decision: Arc::new(AtomicU8::new(PENDING)),
            line,
            token: shared.token.clone(),
            asked: false,
            reply: tx,
            state: Arc::clone(&state),
        });
    }
    shared.notify();
    if !expects {
        return Submitted::Accepted;
    }
    let answer = match wait_reply(&rx, shared, shared.reply_wait) {
        Err(RecvTimeoutError::Timeout) => {
            if state.compare_exchange(PENDING, CANCELLED, SeqCst, SeqCst).is_ok() {
                return Submitted::Failed("503 Service Unavailable", "the app did not run the request in time; it was cancelled".into());
            }
            wait_reply(&rx, shared, shared.reply_wait)
        }
        other => other,
    };
    match answer {
        Ok(Some(reply)) => Submitted::Reply(reply),
        Ok(None) => Submitted::Accepted,
        Err(RecvTimeoutError::Timeout) => {
            Submitted::Failed("504 Gateway Timeout", "the app is still running the request; its outcome is unknown".into())
        }
        Err(RecvTimeoutError::Disconnected) => Submitted::Failed("503 Service Unavailable", "the MCP relay has stopped".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Session {
        let mut s = Session::new(rustgis_engine::Project::default());
        s.run("project.sample", &Value::Null).unwrap();
        s
    }

    /// Sends a raw HTTP request and reads the whole response (the server closes after it).
    fn http(port: u16, request: &str) -> (u16, String) {
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.write_all(request.as_bytes()).unwrap();
        let mut text = String::new();
        let _ = c.read_to_string(&mut text);
        let status = text.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let body = text.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
        (status, body)
    }

    fn post(host: &str, auth: Option<&str>, extra: &str, body: &str) -> String {
        let auth = auth.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        format!(
            "POST /mcp HTTP/1.1\r\nHost: {host}\r\n{auth}{extra}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    /// Runs the app's frames until `done` says the response came, as the UI thread would.
    fn with_app<T: Send + 'static>(relay: &Relay, s: &mut Session, f: impl FnOnce() -> T + Send + 'static) -> T {
        let worker = std::thread::spawn(f);
        while !worker.is_finished() {
            relay.poll(s);
            std::thread::sleep(Duration::from_millis(5));
        }
        worker.join().unwrap()
    }

    /// A client with the token posts MCP messages and gets the server's replies; the command ran
    /// on the app's session. Notifications are accepted with no body.
    #[test]
    fn http_clients_with_the_token_drive_the_session() {
        let relay = Relay::start(0).unwrap();
        let port = relay.serve_http_with(0, "a".repeat(32)).unwrap();
        assert_eq!(relay.http_port(), Some(port));
        let mut s = sample();
        let layers = s.map().layers.len();
        let host = format!("127.0.0.1:{port}");
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}).to_string();
        let req = post(&host, Some(&"a".repeat(32)), "", &init);
        let (status, body) = with_app(&relay, &mut s, move || http(port, &req));
        assert_eq!(status, 200, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["serverInfo"]["name"], "rustgis", "{body}");
        let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "run_command", "arguments": {"command": "layer.addSample", "params": {"name": "countries"}}}}).to_string();
        let req = post(&host, Some(&"a".repeat(32)), "", &call);
        let (status, body) = with_app(&relay, &mut s, move || http(port, &req));
        assert_eq!(status, 200, "{body}");
        assert_eq!(s.map().layers.len(), layers + 1, "the command ran on the app's session");
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string();
        let (status, body) = http(port, &post(&host, Some(&"a".repeat(32)), "", &note));
        assert_eq!((status, body.as_str()), (202, ""));
        relay.stop();
    }

    /// Without the token, from a web page, for another host, or another method: refused, and
    /// nothing reaches the app.
    #[test]
    fn http_refuses_requests_without_the_token_or_from_elsewhere() {
        let relay = Relay::start(0).unwrap();
        let token = "b".repeat(32);
        let port = relay.serve_http_with(0, token.clone()).unwrap();
        let mut s = sample();
        let host = format!("127.0.0.1:{port}");
        let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
        assert_eq!(http(port, &post(&host, None, "", &ping)).0, 401);
        assert_eq!(http(port, &post(&host, Some(&"c".repeat(32)), "", &ping)).0, 401);
        assert_eq!(http(port, &post("evil.example:80", Some(&token), "", &ping)).0, 403, "a rebinding host");
        assert_eq!(http(port, &post(&host, Some(&token), "Origin: https://evil.example\r\n", &ping)).0, 403, "a web page");
        let get = format!("GET /mcp HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\n\r\n");
        assert_eq!(http(port, &get).0, 405);
        let elsewhere = format!("POST /other HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nContent-Length: 2\r\n\r\n{{}}");
        assert_eq!(http(port, &elsewhere).0, 404);
        let twice = format!(
            "POST /mcp HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nContent-Length: 2\r\nContent-Length: 40\r\n\r\n{{}}"
        );
        assert_eq!(http(port, &twice).0, 400, "two lengths");
        let huge =
            format!("POST /mcp HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1);
        assert_eq!(http(port, &huge).0, 413);
        assert_eq!(relay.poll(&mut s), 0, "nothing refused reached the app");
        relay.stop();
        // Stopped with the relay.
        std::thread::sleep(STOP_POLL * 4);
        assert!(
            TcpStream::connect(("127.0.0.1", port))
                .map(|mut c| {
                    let _ = c.write_all(post(&host, Some(&token), "", &ping).as_bytes());
                    let mut t = String::new();
                    let _ = c.read_to_string(&mut t);
                    t
                })
                .map_or(true, |t| !t.contains("200 OK"))
        );
    }

    /// The endpoint file names the HTTP address once HTTP starts.
    #[test]
    fn http_is_named_in_the_endpoint_file() {
        let relay = Relay::start(0).unwrap();
        let name = format!("endpoint-http-test-{}.json", std::process::id());
        let path = relay.publish_endpoint_as(&name).unwrap();
        let before: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(before["http_url"].is_null(), "{before}");
        let port = relay.serve_http_with(0, "e".repeat(32)).unwrap();
        let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["http_url"], format!("http://127.0.0.1:{port}/mcp"), "{after}");
        relay.stop();
        assert!(!path.exists());
    }

    /// A client that trickles its request is cut off at the request's deadline, not kept for as
    /// long as it sends a byte now and then.
    #[test]
    fn http_cuts_off_a_trickling_request() {
        let relay = Relay::start(0).unwrap();
        let io = Duration::from_millis(800);
        let port = relay.serve_http_timed(0, "d".repeat(32), io).unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        // A byte every 200 ms: each read alone would never time out.
        for b in "POST /mcp HTTP/1.1\r\nHost".bytes() {
            if c.write_all(&[b]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        // Cut off at the deadline: the server has closed (end of stream or a reset).
        let mut buf = [0u8; 64];
        let cut = match c.read(&mut buf) {
            Ok(0) => true,
            Err(e) => matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::BrokenPipe
            ),
            Ok(_) => false,
        };
        assert!(cut, "the trickling client was kept");
        relay.stop();
    }

    /// The token is made once and kept, readable by this user only; deleting the file makes a new one.
    #[test]
    fn http_token_is_stable_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-http.token");
        let first = token_at(&path).unwrap();
        assert_eq!(first.len(), 32);
        assert_eq!(token_at(&path).unwrap(), first, "the same token across launches");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(token_at(&path).is_err(), "a token others can read is refused");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        std::fs::remove_file(&path).unwrap();
        assert_ne!(token_at(&path).unwrap(), first, "deleting the file rotates the token");
        // A file left empty by a write that never finished is replaced, not refused.
        std::fs::write(&path, "").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
            .unwrap();
        assert_eq!(token_at(&path).unwrap().len(), 32);
    }
}
