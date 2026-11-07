//! Agents on this computer (desktop): the assistant hands a task to a coding agent the user has
//! (Claude Code, Codex, Gemini CLI…) through ACP ([`rustgis_assistant::acp`]). The agent reaches
//! the open map through the app's MCP relay with a token for this run only, works in a folder of
//! its own that is kept for the user, and asks the user (through the assistant pane) before
//! acting.
//!
//! [`Run`] runs one agent on a task, passes its lines through the ACP session, and stops it (with
//! every process it started) when dropped; its token is revoked with it.

use std::io::{BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use rustgis_assistant::acp;
use rustgis_assistant::framing::{Error as FramingError, MAX_LINE, Oversize, read_message_max};

/// Windows' flag for a console program started without a console window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The most of an agent's error output kept, for a report when it fails without one.
const MAX_STDERR: usize = 4096;

/// Lines waiting for the agent to read them: room for a burst of answers to its requests.
const INPUT_QUEUE: usize = 64;

/// An agent working on a task.
pub struct Run {
    /// Where it works; kept afterwards.
    pub work: PathBuf,
    events: Arc<Mutex<Vec<acp::Event>>>,
    session: Arc<Mutex<acp::Session>>,
    stdin: SyncSender<String>,
    child: Arc<Mutex<Option<Child>>>,
    pid: u32,
    /// Its relay token: revoked when the run is dropped.
    _grant: rustgis_assistant::relay::Grant,
}

/// Queue one bounded line without ever waiting for the agent's pipe.
fn send(stdin: &SyncSender<String>, line: &str) -> Result<(), String> {
    if u64::try_from(line.len()).unwrap_or(u64::MAX) > MAX_LINE {
        return Err(crate::tl!("Agent input exceeds the protocol line limit.").into());
    }
    stdin
        .try_send(line.to_string())
        .map_err(|_| crate::tl!("Agent input queue is full or disconnected; stop the agent and try again.").into())
}

/// Observe exit while keeping the Unix leader unreaped, reserving its PID/group identity
/// until Stop can terminate surviving descendants. Other platforms keep Child's safe policy.
fn child_status(child: &mut Child) -> std::io::Result<Option<bool>> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        let pid = i32::try_from(child.id()).ok().and_then(Pid::from_raw).ok_or_else(|| std::io::Error::other("invalid child id"))?;
        waitid(WaitId::Pid(pid), WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT)
            .map(|s| s.map(|s| s.exit_status() == Some(0)))
            .map_err(std::io::Error::from)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        child.try_wait().map(|s| s.map(|s| s.success()))
    }
}

fn owns_child(child: &mut Child) -> bool {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        child_status(child).is_ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        matches!(child_status(child), Ok(None))
    }
}

/// `yyyy-mm-dd-hhmmss` (UTC) for a folder name.
fn stamp() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}-{:02}{:02}{:02}", rest / 3600, rest / 60 % 60, rest % 60)
}

/// A new folder for a task: `Documents/RustGIS/Agent work/<time>-<agent>`.
pub fn work_folder(agent: &str) -> Result<PathBuf, String> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).ok_or("no home folder")?;
    let docs = Some(home.join("Documents")).filter(|d| d.is_dir()).unwrap_or(home);
    work_folder_in(&docs.join("RustGIS").join("Agent work"), agent)
}

/// A new folder in `base` for a task: `<time>-<agent>`, numbered when another task of the same
/// agent took that name within the second.
fn work_folder_in(base: &std::path::Path, agent: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(base).map_err(|e| format!("{}: {e}", base.display()))?;
    let name = format!("{}-{agent}", stamp());
    for n in 1..1000 {
        let dir = base.join(if n == 1 { name.clone() } else { format!("{name}-{n}") });
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        }
    }
    Err(format!("{}: no free folder name for {name}", base.display()))
}

impl Run {
    /// Starts `spec` (with `command`, from [`acp::command`]) on `task`, reaching the map through
    /// `relay` with a token of its own.
    /// `cli` is `rustgis-cli`, which serves the map to the agent over MCP; `work` is the folder
    /// it works in ([`work_folder`]).
    pub fn start(
        spec: &'static acp::AgentSpec,
        command: &[String],
        task: &str,
        relay: &rustgis_assistant::relay::Relay,
        cli: &std::path::Path,
        work: PathBuf,
        auto: bool,
        ctx: egui::Context,
    ) -> Result<Run, String> {
        // Its own token: what it may do on the map is this run's, whatever other agents may.
        let grant = relay.grant_reviewed(auto)?;
        let mcp = vec![
            cli.display().to_string(),
            "mcp".into(),
            "--attach".into(),
            relay.port().to_string(),
            "--token-file".into(),
            grant.token_file().display().to_string(),
            "--yes".into(),
        ];
        let (session, init) = acp::Session::new(spec.label, task, &work.display().to_string(), mcp, auto);
        let (program, args) = command.split_first().ok_or("no command")?;
        let mut cmd = Command::new(program);
        cmd.args(args).current_dir(&work).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // Its own process group, so stopping it stops what it started (Unix).
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        // No console window on Windows: what the agent does shows in its window in RustGIS.
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, CREATE_NO_WINDOW);
        let mut child = cmd.spawn().map_err(|e| format!("{program}: {e}"))?;
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let pid = child.id();
        let (input, lines) = sync_channel::<String>(INPUT_QUEUE);
        let run = Run {
            work,
            events: Arc::default(),
            session: Arc::new(Mutex::new(session)),
            stdin: input,
            child: Arc::new(Mutex::new(Some(child))),
            pid,
            _grant: grant,
        };
        let (events, session, writer_ctx) = (run.events.clone(), run.session.clone(), ctx.clone());
        std::thread::spawn(move || {
            let Some(mut pipe) = stdin else { return };
            for line in lines {
                if pipe.write_all(line.as_bytes()).and_then(|()| pipe.write_all(b"\n")).and_then(|()| pipe.flush()).is_err() {
                    let new = session.lock().map(|mut s| s.exited(false, crate::tl!("Agent input pipe closed."))).unwrap_or_default();
                    if let Ok(mut events) = events.lock() {
                        events.extend(new);
                    }
                    writer_ctx.request_repaint();
                    break;
                }
            }
        });
        send(&run.stdin, &init)?;
        // Its last bytes of errors, decoded once it ends (a character split across reads stays whole).
        let errors: Arc<Mutex<Vec<u8>>> = Arc::default();
        if let Some(mut stderr) = stderr {
            let errors = errors.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                while let Ok(n) = stderr.read(&mut buf) {
                    let Some(piece) = buf.get(..n).filter(|p| !p.is_empty()) else { break };
                    if let Ok(mut e) = errors.lock() {
                        e.extend_from_slice(piece);
                        let over = e.len().saturating_sub(MAX_STDERR);
                        if over > 0 {
                            // Cut at the start of a character, not inside one.
                            let cut = (over..e.len()).find(|i| e.get(*i).is_some_and(|b| b & 0xC0 != 0x80)).unwrap_or(e.len());
                            e.drain(..cut);
                        }
                    }
                }
            });
        }
        let (events, session, stdin, child) = (run.events.clone(), run.session.clone(), run.stdin.clone(), run.child.clone());
        std::thread::spawn(move || {
            let push = |new: Vec<acp::Event>| {
                if !new.is_empty() {
                    if let Ok(mut e) = events.lock() {
                        e.extend(new);
                    }
                    ctx.request_repaint();
                }
            };
            if let Some(stdout) = stdout {
                // Its lines, each at most the relay's limit: an oversized or unreadable one is
                // skipped, not buffered whole.
                let mut reader = BufReader::new(stdout);
                'reading: loop {
                    let line = match read_message_max(&mut reader, MAX_LINE, Oversize::Skip) {
                        Ok(Some(line)) => line,
                        Ok(None) | Err(FramingError::Io(_)) => break,
                        Err(_) => continue,
                    };
                    let new = match session.lock() {
                        Ok(mut s) => {
                            let (out, new) = s.line(&line);
                            // Queue writes under the state lock: auto-off cannot overtake a
                            // prompt that passed readiness, nor can that prompt follow cancel.
                            for l in out {
                                if let Err(why) = send(&stdin, &l) {
                                    push(s.exited(false, &why));
                                    break 'reading;
                                }
                            }
                            new
                        }
                        Err(_) => break,
                    };
                    push(new);
                }
            }
            // Its output ended: wait for it to exit without holding the lock (Stop must not wait).
            let ok = loop {
                let status = child.lock().ok().and_then(|mut c| c.as_mut().map(child_status));
                match status {
                    Some(Ok(Some(ok))) => break ok,
                    Some(Ok(None)) => std::thread::sleep(Duration::from_millis(50)),
                    _ => break false,
                }
            };
            let why = errors.lock().map(|e| String::from_utf8_lossy(&e).into_owned()).unwrap_or_default();
            let new = session.lock().map(|mut s| s.exited(ok, &why)).unwrap_or_default();
            push(new);
        });
        Ok(run)
    }

    /// What it did and asked since last asked, including an unanswered mode-change timeout.
    pub fn events(&self) -> Vec<acp::Event> {
        let mut new = self.session.lock().map(|mut s| s.poll()).unwrap_or_default();
        let mut events = self.events.lock().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default();
        events.append(&mut new);
        events
    }

    /// Apply the current shared setting and start a follow-up under one session lock, then
    /// queue their lines in order. A reply cannot release work between those two decisions.
    pub fn follow_up(&self, task: &str, auto: bool) -> bool {
        self._grant.set_auto(auto);
        let Ok(mut s) = self.session.lock() else { return false };
        let (mut lines, mut new) = s.set_auto(auto);
        let accepted = if let Some((follow, events)) = s.follow_up(task) {
            lines.extend(follow);
            new.extend(events);
            true
        } else {
            false
        };
        for line in lines {
            if let Err(why) = send(&self.stdin, &line) {
                new.extend(s.exited(false, &why));
                if let Ok(mut events) = self.events.lock() {
                    events.extend(new);
                }
                return false;
            }
        }
        if let Ok(mut events) = self.events.lock() {
            events.extend(new);
        }
        // Even an immediate readiness refusal was accepted as a turn: its Done event tells
        // the caller why. Returning false would instead claim the thread had ended.
        accepted
    }

    fn input_failed(&self, why: &str) {
        let new = self.session.lock().map(|mut s| s.exited(false, why)).unwrap_or_default();
        if let Ok(mut events) = self.events.lock() {
            events.extend(new);
        }
    }

    /// Apply the shared approval setting immediately, forwarding cancellation and mode changes
    /// in session order. A turn already running unsafely is stopped with a visible report.
    pub fn set_auto(&self, auto: bool) {
        self._grant.set_auto(auto);
        if let Ok(mut s) = self.session.lock() {
            let (lines, mut new) = s.set_auto(auto);
            for line in lines {
                if let Err(why) = send(&self.stdin, &line) {
                    new.extend(s.exited(false, &why));
                    break;
                }
            }
            if let Ok(mut events) = self.events.lock() {
                events.extend(new);
            }
        }
    }

    /// It can't go on (it failed to start, or its process ended).
    pub fn finished(&self) -> bool {
        self.session.lock().map(|s| s.finished()).unwrap_or(true)
    }

    pub fn map_approvals(&self) -> Vec<rustgis_assistant::relay::Approval> {
        self._grant.pending()
    }

    pub fn answer_map(&self, id: u64, allow: bool) {
        self._grant.decide(id, allow);
    }

    /// Answers a permission request.
    pub fn answer(&self, p: &acp::Permission, choice: acp::Choice) {
        let line = self.session.lock().ok().map(|s| s.answer(p, choice));
        if let Some(line) = line
            && let Err(why) = send(&self.stdin, &line)
        {
            self.input_failed(&why);
        }
    }
}

/// Keep cleanup workers until application shutdown, so exit cannot abandon a cancelled agent.
fn stop_jobs() -> &'static Mutex<Vec<std::thread::JoinHandle<()>>> {
    static JOBS: OnceLock<Mutex<Vec<std::thread::JoinHandle<()>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(Vec::new()))
}

pub(crate) fn finish_stops() {
    let jobs = std::mem::take(&mut *stop_jobs().lock().unwrap_or_else(|e| e.into_inner()));
    for job in jobs {
        let _ = job.join();
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        let (pid, child, session, stdin) = (self.pid, self.child.clone(), self.session.clone(), self.stdin.clone());
        let worker = std::thread::Builder::new().name("rustgis-agent-stop".into()).spawn(move || stop_process(pid, child, session, stdin));
        match worker {
            Ok(worker) => {
                let mut jobs = stop_jobs().lock().unwrap_or_else(|e| e.into_inner());
                jobs.retain(|j| !j.is_finished());
                jobs.push(worker);
            }
            Err(_) => stop_process(self.pid, self.child.clone(), self.session.clone(), self.stdin.clone()),
        }
    }
}

fn stop_process(pid: u32, child: Arc<Mutex<Option<Child>>>, session: Arc<Mutex<acp::Session>>, stdin: SyncSender<String>) {
    let owned = child.lock().ok().and_then(|mut c| c.as_mut().map(owns_child)).unwrap_or(false);
    // An externally reaped child no longer pins the numeric process/group identity.
    if !owned {
        return;
    }
    // Asked to stop first (ACP's cancel), with a moment to act on it, then stopped regardless.
    if let Some(cancel) = session.try_lock().ok().and_then(|s| s.cancel()) {
        // Best effort: the bounded writer queue never waits for a stalled pipe.
        let _ = send(&stdin, &cancel);
        let until = std::time::Instant::now() + Duration::from_millis(200);
        while std::time::Instant::now() < until {
            let exited = child.lock().ok().and_then(|mut c| c.as_mut().map(|c| !matches!(child_status(c), Ok(None)))).unwrap_or(true);
            if exited {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    // Hold the child lock through termination: the waiter must not reap the leader
    // between checking its identity and signalling its group/tree.
    let Ok(mut children) = child.lock() else { return };
    let Some(child) = children.as_mut() else { return };
    if child.id() != pid || !owns_child(child) {
        return;
    }
    #[cfg(windows)]
    {
        let mut kill = Command::new("taskkill");
        std::os::windows::process::CommandExt::creation_flags(&mut kill, CREATE_NO_WINDOW);
        let _ = kill.args(["/T", "/F", "/PID", &pid.to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    #[cfg(unix)]
    {
        // The process group the agent leads (it was started as one), by the system call: not
        // the `kill` program, whose reading of a negative id varies (procps-ng 4.0.2 takes
        // `kill -KILL -<pid>` as every process of the user). An id of 0 is refused, so this
        // never widens to the caller's own group.
        if let Some(pgid) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) {
            let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    //! A fake ACP agent, played by this test binary: the run's processes, lines, follow-ups,
    //! permission answers and Stop, for real.
    use std::io::{BufRead, Write};
    use std::time::{Duration, Instant};

    use rustgis_assistant::acp::{AgentSpec, Choice, Event};
    use serde_json::{Value, json};

    use super::Run;

    /// The argument that makes this binary the fake agent (also a test filter matching nothing).
    const CHILD_MARK: &str = "rustgis-acp-child-mode";
    const HELD_MODES: &str = "rustgis-acp-held-modes";

    /// The fake agent: ACP over standard input and output. A task with ASK asks for permission
    /// first; one with HANG works until it's cancelled; any other is done at once.
    #[test]
    fn fake_acp_agent() {
        if !std::env::args().any(|a| a == CHILD_MARK) {
            return;
        }
        let held_modes = std::env::args().any(|a| a == HELD_MODES);
        let mut mode_reply: Option<(Value, String)> = None;
        let mut prompts = 0;
        let mut out = std::io::stdout();
        let _ = writeln!(out);
        let send = |out: &mut std::io::Stdout, v: Value| {
            let _ = writeln!(out, "{v}");
            let _ = out.flush();
        };
        let update = |u: Value| json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "fake", "update": u}});
        let say = |t: &str| update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": t}}));
        let mark = |title: &str| update(json!({"sessionUpdate": "tool_call", "toolCallId": "fixture", "title": title}));
        let mut lines = std::io::stdin().lock().lines().map_while(Result::ok);
        let mut hanging: Option<Value> = None;
        while let Some(line) = lines.next() {
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            let id = msg["id"].clone();
            match msg["method"].as_str() {
                Some("initialize") => send(
                    &mut out,
                    json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": []}}),
                ),
                Some("session/new") => {
                    let mut modes = vec![json!({"id": "agent"})];
                    if held_modes {
                        modes.push(json!({"id": "agent-full-access"}));
                    }
                    send(
                        &mut out,
                        json!({"jsonrpc": "2.0", "id": id, "result": {"sessionId": "fake", "modes": {"currentModeId": "agent", "availableModes": modes}}}),
                    );
                }
                Some("session/set_mode") if held_modes => {
                    let mode = msg["params"]["modeId"].as_str().unwrap().to_string();
                    send(&mut out, mark(&format!("held:{mode}")));
                    assert!(mode_reply.replace((id, mode)).is_none(), "mode changes must stay serialized");
                }
                Some("test/release_mode") if held_modes => {
                    let (id, mode) = mode_reply.take().unwrap();
                    send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": {}}));
                    send(&mut out, mark(&format!("released:{mode}")));
                }
                Some("test/full_mode") if held_modes => {
                    send(&mut out, update(json!({"sessionUpdate": "current_mode_update", "currentModeId": "agent-full-access"})))
                }
                Some("test/barrier") if held_modes => {
                    send(&mut out, mark(&format!("{}:{prompts}", msg["params"]["name"].as_str().unwrap())))
                }
                Some("session/cancel") => {
                    if let Some(pid) = hanging.take() {
                        send(&mut out, json!({"jsonrpc": "2.0", "id": pid, "result": {"stopReason": "cancelled"}}));
                    }
                }
                Some("session/prompt") => {
                    if held_modes {
                        prompts += 1;
                        send(&mut out, mark("prompt"));
                    }
                    let task = msg["params"]["prompt"][0]["text"].as_str().unwrap_or("").to_string();
                    if task.contains("BLOCK") {
                        send(
                            &mut out,
                            update(json!({"sessionUpdate":"tool_call","toolCallId":"blocked","title":"Blocked","status":"in_progress"})),
                        );
                        std::thread::sleep(Duration::from_secs(30));
                        continue;
                    }
                    if task.contains("HANG") {
                        send(
                            &mut out,
                            update(
                                json!({"sessionUpdate": "tool_call", "toolCallId": "h", "title": "Working forever", "status": "in_progress"}),
                            ),
                        );
                        hanging = Some(id);
                        continue;
                    }
                    if task.contains("ASK") {
                        send(
                            &mut out,
                            update(
                                json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Fetch the gauges", "status": "pending"}),
                            ),
                        );
                        send(
                            &mut out,
                            json!({"jsonrpc": "2.0", "id": 100, "method": "session/request_permission", "params": {"sessionId": "fake",
                            "toolCall": {"toolCallId": "t1", "rawInput": {"url": "https://waterservices.usgs.gov/"}},
                            "options": [{"optionId": "yes", "name": "Allow", "kind": "allow_once"}, {"optionId": "no", "name": "Reject", "kind": "reject_once"}]}}),
                        );
                        let answer = lines.by_ref().find_map(|l| serde_json::from_str::<Value>(&l).ok().filter(|v| v["id"] == 100));
                        let chosen = answer.and_then(|a| a["result"]["outcome"]["optionId"].as_str().map(str::to_string));
                        send(&mut out, say(if chosen.as_deref() == Some("yes") { "Allowed: fetched 3 gauges." } else { "Declined." }));
                    } else {
                        send(&mut out, say("Did "));
                        send(&mut out, say("the task."));
                    }
                    send(&mut out, json!({"jsonrpc": "2.0", "id": id, "result": {"stopReason": "end_turn"}}));
                }
                _ => {}
            }
        }
    }

    /// Waits for the run's events up to the end of a task (or five seconds).
    fn until_done(run: &Run) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut all = Vec::new();
        while Instant::now() < deadline {
            let new = run.events();
            let done = new.iter().any(|e| matches!(e, Event::Done { .. } | Event::Permission(_)));
            all.extend(new);
            if done {
                return all;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        all
    }

    fn report(events: &[Event]) -> Option<(String, bool)> {
        events.iter().find_map(|e| match e {
            Event::Done { report, error } => Some((report.clone(), *error)),
            _ => None,
        })
    }

    /// The actual checkbox must reach a held ACP upgrade before the next thread poll. The
    /// child's FIFO barrier counts every prompt, including events a later app poll consumes.
    #[test]
    fn local_agents_checkbox_contains_held_upgrade_and_queued_follow_up() {
        use egui_kittest::kittest::Queryable;
        use rustgis_assistant::agent::{Agent, Step};
        use rustgis_assistant::provider::Config;
        use std::sync::{Arc, Mutex};

        /// Read the real process events without draining them before the app's next poll.
        fn wait_mark(events: &Arc<Mutex<Vec<Event>>>, prefix: &str) -> String {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(title) = events.lock().unwrap().iter().find_map(|e| match e {
                    Event::Tool(title) if title == prefix || (prefix.ends_with(':') && title.starts_with(prefix)) => Some(title.clone()),
                    _ => None,
                }) {
                    return title;
                }
                assert!(Instant::now() < deadline, "fake ACP barrier timed out: {prefix}");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        /// Inject a provider reply through Agent's normal delegation queue, with no HTTP.
        fn delegate(a: &mut Agent, s: &mut rustgis_engine::Session, thread: Option<&str>) {
            let mut args = json!({"agent": "goose", "task": "Map the cities"});
            if let Some(thread) = thread {
                args["thread"] = json!(thread);
            }
            let body = json!({"choices": [{"message": {"role": "assistant", "content": null,
                "tool_calls": [{"id": "d", "type": "function", "function": {"name": "delegate", "arguments": args.to_string()}}]},
                "finish_reason": "tool_calls"}]})
            .to_string()
            .into_bytes();
            assert!(matches!(
                a.receive(a.generation(), Ok(rustgis_assistant::HttpResponse { status: 200, body, headers: vec![] }), s),
                Step::Send(_)
            ));
        }

        let relay = rustgis_assistant::relay::Relay::start(0).unwrap();
        let work = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let command = vec![
            exe.display().to_string(),
            "localagents::tests::fake_acp_agent".into(),
            CHILD_MARK.into(),
            HELD_MODES.into(),
            "--exact".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ];
        let ctx = egui::Context::default();
        let run = Run::start(
            AgentSpec::from_id("goose").unwrap(),
            &command,
            "Map the cities",
            &relay,
            &exe,
            work.path().to_path_buf(),
            true,
            ctx.clone(),
        )
        .unwrap();
        let (events, input) = (run.events.clone(), run.stdin.clone());
        let mut app = crate::RustGisApp::with_sample();
        let prefs = Arc::new(Mutex::new(Vec::new()));
        let saved = prefs.clone();
        app.services.save_pref = Some(Box::new(move |key, value| saved.lock().unwrap().push((key.to_string(), value.to_string()))));
        crate::agentthreads::insert_test_run(&mut app, run);
        let mut h = crate::assistantpane::tests::local_agents_harness(app);
        h.run_steps(3);
        wait_mark(&events, "held:agent-full-access");
        h.get_by_label("Approve agents' actions without asking").click();
        h.step();
        assert!(!h.state().assistant.agents_auto);
        assert!(prefs.lock().unwrap().contains(&("assistant_agents_auto".into(), "off".into())));
        // No agentthreads poll occurs between the real checkbox callback and this reply.
        let control =
            |method: &str, name: &str| super::send(&input, &json!({"method": method, "params": {"name": name}}).to_string()).unwrap();
        control("test/release_mode", "");
        wait_mark(&events, "released:agent-full-access");
        control("test/barrier", "after-upgrade");
        assert_eq!(wait_mark(&events, "after-upgrade:"), "after-upgrade:0");
        assert_eq!(wait_mark(&events, "held:agent"), "held:agent");
        control("test/release_mode", "");
        wait_mark(&events, "released:agent");
        control("test/barrier", "after-asking");
        assert_eq!(wait_mark(&events, "after-asking:"), "after-asking:1");
        assert!(events.lock().unwrap().iter().any(|e| matches!(e, Event::Done { error: false, .. })));
        crate::agentthreads::poll(h.state_mut(), &ctx);

        // Queue a real parent-Agent follow-up while auto is on, then change the checkbox
        // before poll consumes that queue. The Run must use the current shared flag.
        h.get_by_label("Approve agents' actions without asking").click();
        h.step();
        assert!(h.state().assistant.agents_auto);
        control("test/full_mode", "");
        control("test/barrier", "full-between-turns");
        assert_eq!(wait_mark(&events, "full-between-turns:"), "full-between-turns:1");
        let config = Config {
            provider: "openai".into(),
            base_url: "https://model.invalid/v1".into(),
            model: "fixture".into(),
            key: "synthetic-test".into(),
            account: String::new(),
            client_id: String::new(),
            session: String::new(),
            reasoning: None,
            tool_output_token_limit: rustgis_assistant::tools::DEFAULT_OUTPUT_TOKENS,
        };
        let app = h.state_mut();
        let mut a = Agent::new(config, &app.session);
        a.set_harnesses(vec![AgentSpec::from_id("goose").unwrap()]);
        a.set_agents_auto(true);
        assert!(matches!(a.ask("Map it", &app.session), Step::Send(_)));
        delegate(&mut a, &mut app.session, None);
        let first = a.take_delegations();
        assert_eq!(first[0].thread, "goose-1");
        a.thread_report("goose-1", "Done".into(), false);
        delegate(&mut a, &mut app.session, Some("goose-1"));
        app.assistant.agent = Some(a);
        h.get_by_label("Approve agents' actions without asking").click();
        h.step();
        assert!(!h.state().assistant.agents_auto);
        crate::agentthreads::poll(h.state_mut(), &ctx);
        control("test/barrier", "queued-follow-up");
        assert_eq!(wait_mark(&events, "queued-follow-up:"), "queued-follow-up:1", "a queued follow-up waits for asking mode");
        control("test/release_mode", "");
        // The response marker follows Session processing, so its generated prompt is
        // already queued before this final FIFO barrier.
        wait_mark(&events, "released:agent");
        control("test/barrier", "follow-up-done");
        assert_eq!(wait_mark(&events, "follow-up-done:"), "follow-up-done:2");
        drop(input);
        drop(h);
        super::finish_stops();
    }

    /// Two tasks of one agent within the same second get folders of their own.
    #[test]
    fn work_folders_never_collide() {
        let base = std::env::temp_dir().join(format!("rustgis-work-folders-{}", std::process::id()));
        let a = super::work_folder_in(&base, "goose").unwrap();
        let b = super::work_folder_in(&base, "goose").unwrap();
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_agent_thread_works_takes_follow_ups_asks_and_stops() {
        let relay = rustgis_assistant::relay::Relay::start(0).unwrap();
        let exe = std::env::current_exe().unwrap();
        let command: Vec<String> = [
            exe.display().to_string().as_str(),
            "localagents::tests::fake_acp_agent",
            CHILD_MARK,
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let work = std::env::temp_dir().join(format!("rustgis-acp-test-{}", std::process::id()));
        std::fs::create_dir_all(&work).unwrap();
        let spec = AgentSpec::from_id("goose").unwrap();
        let run = Run::start(spec, &command, "Say hi", &relay, &exe, work.clone(), false, egui::Context::default()).unwrap();
        // The first task: its message, whole, and its report.
        let ev = until_done(&run);
        assert!(ev.contains(&Event::Text("Did the task.".into())), "{ev:?}");
        assert_eq!(report(&ev), Some(("Did the task.".into(), false)));
        assert!(!run.finished());
        // A follow-up that asks for permission: allowed.
        assert!(run.follow_up("ASK to fetch", false));
        assert!(!run.follow_up("one at a time", false), "a working thread takes no second task");
        let ev = until_done(&run);
        assert!(ev.contains(&Event::Tool("Fetch the gauges".into())), "{ev:?}");
        let Some(Event::Permission(p)) = ev.iter().find(|e| matches!(e, Event::Permission(_))).cloned() else {
            panic!("no request: {ev:?}")
        };
        assert!(p.detail.contains("waterservices.usgs.gov"));
        run.answer(&p, Choice::Allow);
        assert_eq!(report(&until_done(&run)), Some(("Allowed: fetched 3 gauges.".into(), false)));
        // Declined.
        assert!(run.follow_up("ASK again", false));
        let Some(Event::Permission(p)) = until_done(&run).into_iter().find(|e| matches!(e, Event::Permission(_))) else {
            panic!("no request")
        };
        run.answer(&p, Choice::Reject);
        assert_eq!(report(&until_done(&run)), Some(("Declined.".into(), false)));
        // Its relay token is its own, revoked with it; Stop ends a task that never would.
        assert!(run.follow_up("HANG", false));
        std::thread::sleep(Duration::from_millis(300));
        let pid = run.pid;
        let grant_file = run._grant.token_file().to_path_buf();
        assert!(grant_file.exists());
        drop(run);
        drop(crate::agentthreads::Threads::default());
        assert!(!grant_file.exists(), "the run's relay token is revoked with it");
        let deadline = Instant::now() + Duration::from_secs(10);
        while alive(pid) {
            assert!(Instant::now() < deadline, "the agent's process outlived Stop");
            std::thread::sleep(Duration::from_millis(50));
        }
        relay.stop();
        let _ = std::fs::remove_dir_all(&work);
    }

    /// A reaped leader's old number must never target a new process group.
    #[cfg(unix)]
    #[test]
    fn dropping_a_reaped_agent_never_signals_a_reused_id() {
        use std::os::unix::process::CommandExt;
        let relay = rustgis_assistant::relay::Relay::start(0).unwrap();
        let exe = std::env::current_exe().unwrap();
        let command = vec![
            exe.display().to_string(),
            "localagents::tests::fake_acp_agent".into(),
            CHILD_MARK.into(),
            "--exact".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ];
        let work = std::env::temp_dir().join(format!("rustgis-acp-reaped-{}", std::process::id()));
        std::fs::create_dir_all(&work).unwrap();
        let mut run =
            Run::start(AgentSpec::from_id("goose").unwrap(), &command, "Hi", &relay, &exe, work.clone(), false, egui::Context::default())
                .unwrap();
        assert!(report(&until_done(&run)).is_some());
        {
            let mut children = run.child.lock().unwrap();
            let child = children.as_mut().unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
        }
        let mut other = std::process::Command::new("sleep").arg("30").process_group(0).spawn().unwrap();
        // Deterministically simulate reuse, without waiting for the OS to recycle a PID.
        run.pid = other.id();
        super::stop_process(run.pid, run.child.clone(), run.session.clone(), run.stdin.clone());
        drop(run);
        let survived = other.try_wait().unwrap().is_none();
        let _ = other.kill();
        let _ = other.wait();
        assert!(survived, "Drop signalled an unrelated process group");
        relay.stop();
        let _ = std::fs::remove_dir_all(work);
    }

    #[test]
    fn stop_does_not_wait_for_a_blocked_stdin_writer() {
        let relay = rustgis_assistant::relay::Relay::start(0).unwrap();
        let exe = std::env::current_exe().unwrap();
        let command = vec![
            exe.display().to_string(),
            "localagents::tests::fake_acp_agent".into(),
            CHILD_MARK.into(),
            "--exact".into(),
            "--nocapture".into(),
            "--test-threads=1".into(),
        ];
        let work = std::env::temp_dir().join(format!("rustgis-acp-blocked-{}", std::process::id()));
        std::fs::create_dir_all(&work).unwrap();
        let run = Run::start(
            AgentSpec::from_id("goose").unwrap(),
            &command,
            "BLOCK",
            &relay,
            &exe,
            work.clone(),
            false,
            egui::Context::default(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if run.events().iter().any(|e| matches!(e,Event::Tool(s) if s == "Blocked")) {
                break;
            }
            assert!(Instant::now() < deadline, "fake agent never blocked");
            std::thread::sleep(Duration::from_millis(20));
        }
        // Fill the writer pipe and its bounded queue while the fake agent refuses reads.
        for _ in 0..16 {
            let _ = super::send(&run.stdin, &"x".repeat(usize::try_from(super::MAX_LINE).unwrap()));
        }
        let request = r#"{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{"options":[{"optionId":"yes","kind":"allow_once"}]}}"#;
        let (_, events) = run.session.lock().unwrap().line(request);
        let Event::Permission(permission) = &events[0] else { panic!("permission") };
        let start = Instant::now();
        run.answer(permission, Choice::Allow);
        assert!(start.elapsed() < Duration::from_secs(1), "permission answer blocked on agent stdin");
        let child = run.child.clone();
        drop(run);
        super::finish_stops();
        // Another test's `finish_stops` may have taken this stop job and still be waiting on it,
        // so the agent is stopped soon rather than at once.
        let until = Instant::now() + Duration::from_secs(5);
        while child.lock().unwrap().as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < until, "agent survived Stop");
            std::thread::sleep(Duration::from_millis(20));
        }
        relay.stop();
        let _ = std::fs::remove_dir_all(work);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stop_terminates_descendants_after_leader_exit_without_reusing_its_identity() {
        let relay = rustgis_assistant::relay::Relay::start(0).unwrap();
        let base = std::env::temp_dir().join(format!("rustgis-acp-descendant-{}", std::process::id()));
        let work = super::work_folder_in(&base, "test").unwrap();
        let command = vec!["/bin/sh".into(), "-c".into(), "sleep 30 >/dev/null 2>&1 & echo $! > descendant.pid; exit 0".into()];
        let run = Run::start(
            AgentSpec::from_id("goose").unwrap(),
            &command,
            "task",
            &relay,
            &std::env::current_exe().unwrap(),
            work.clone(),
            false,
            egui::Context::default(),
        )
        .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let exited = run.child.lock().unwrap().as_mut().is_some_and(|c| super::child_status(c).unwrap().is_some());
            if run.finished() && work.join("descendant.pid").is_file() && exited {
                break;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(20));
        }
        let descendant: u32 = std::fs::read_to_string(work.join("descendant.pid")).unwrap().trim().parse().unwrap();
        assert!(alive(descendant));
        drop(run);
        super::finish_stops();
        let until = Instant::now() + Duration::from_secs(5);
        while alive(descendant) {
            assert!(Instant::now() < until, "descendant survived Stop");
            std::thread::sleep(Duration::from_millis(20));
        }
        relay.stop();
        let _ = std::fs::remove_dir_all(base);
    }

    /// Whether a process is still running.
    fn alive(pid: u32) -> bool {
        #[cfg(unix)]
        {
            std::path::Path::new(&format!("/proc/{pid}")).exists()
                && !std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.contains(") Z "))
        }
        #[cfg(windows)]
        {
            std::process::Command::new("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/NH"])
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        }
    }
}
