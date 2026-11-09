//! Process plugins (desktop and command line): a program on the user's computer, not sandboxed,
//! that speaks the plugin ABI over its standard input and output, one JSON object per line.
//!
//! For each command or tool call RustGIS starts the program in the plugin's folder and writes
//! the request (the same object a WebAssembly module gets) as one line. The program answers with
//! lines of its own:
//!
//! - `{"call": {"op": "...", "params": {...}}}`: a host request, answered with one line,
//!   `{"ok": value}` or `{"error": "text"}`.
//! - `{"log": "text"}`: a log line.
//! - `{"progress": "text"}`: what it's doing now, shown while a tool runs and not kept.
//! - `{"ok": value}` or `{"error": "text"}`: the result. RustGIS then closes the program's input
//!   and waits briefly for it to exit before stopping it.
//!
//! Other output lines are logged and skipped (libraries print), and so is what it writes to its
//! error output.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel, sync_channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::runtime::Host;

/// How long a command may run; tools run until they finish (a large dataset can take a while).
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a program has to exit after answering, before it's stopped.
pub(crate) const EXIT_GRACE: Duration = Duration::from_secs(3);
/// The longest line read from a program (a reply with large layers).
pub(crate) const MAX_LINE: usize = 256 * 1024 * 1024;
/// The most host requests one call may make.
pub(crate) const MAX_HOST_CALLS: u32 = 100_000;
/// The most lines of its own output a program may log in one call.
const MAX_LOGGED: usize = 200;
/// The longest line of a program's error output kept (a longer one is split).
const MAX_ERROR_LINE: usize = 64 * 1024;
/// How often a call waiting on the program checks whether it was cancelled.
const CANCEL_POLL: Duration = Duration::from_millis(100);
/// The error of a call cancelled through its host.
pub const CANCELLED: &str = "it was cancelled";

/// How to start a process plugin.
#[derive(Clone, Debug)]
pub struct Spec {
    /// The program: a bare name (found on `PATH`) or a path.
    pub program: String,
    pub args: Vec<String>,
    /// The plugin's folder: where it runs, and what relative program paths are in.
    pub dir: PathBuf,
    /// Environment variables for the program (tests only: a manifest can't set them).
    pub(crate) env: Vec<(String, String)>,
    /// For a plugin with a Python environment (`"python"`): the check that it's ready.
    pub env_check: Option<crate::pyenv::EnvCheck>,
    /// Whether its commands go to one program kept running (`"session"`, see [`crate::session`]).
    pub session: bool,
}

impl Spec {
    /// The spec for the manifest's `run` on this platform, in `dir`.
    pub fn new(argv: &[String], dir: &Path) -> Result<Spec, String> {
        let (program, args) = argv.split_first().ok_or("'run' names no program")?;
        // Absolute: the program starts in the folder, so a relative folder would be followed
        // twice (`./plugin/plugin/bin/tool`).
        let dir = std::path::absolute(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(Spec { program: program.clone(), args: args.to_vec(), dir, env: Vec::new(), env_check: None, session: false })
    }

    /// The program to start: a path relative to the plugin's folder, or a name for `PATH`.
    fn program_path(&self) -> PathBuf {
        let p = Path::new(&self.program);
        if p.is_absolute() || !self.program.contains(['/', '\\']) { p.to_path_buf() } else { self.dir.join(p) }
    }

    fn command(&self) -> Command {
        let mut c = Command::new(self.program_path());
        c.args(&self.args).current_dir(&self.dir).envs(self.env.iter().map(|(k, v)| (k, v)));
        // A group of its own, so stopping it stops what it started too (workers, a wrapper's
        // program), and their pipes close.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            c.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c.creation_flags(CREATE_NO_WINDOW);
        }
        c
    }
}

/// What the program sent: a line of its output, or the end of it.
pub(crate) enum Line {
    Text(Vec<u8>),
    End,
    TooLong,
}

/// Reads the program's output lines on a thread, so waiting for one can time out. A few lines
/// at most wait to be handled: a program printing faster than RustGIS reads is made to wait
/// rather than filling memory.
fn lines(out: impl Read + Send + 'static, wake: Option<crate::session::Wake>) -> Receiver<Line> {
    let (tx, rx) = sync_channel(2);
    std::thread::spawn(move || {
        let mut r = BufReader::new(out);
        loop {
            let mut buf = Vec::new();
            match (&mut r).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Line::End);
                    wake.as_ref().inspect(|w| w());
                    return;
                }
                Ok(_) if buf.len() > MAX_LINE => {
                    let _ = tx.send(Line::TooLong);
                    wake.as_ref().inspect(|w| w());
                    return;
                }
                Ok(_) => {
                    if tx.send(Line::Text(buf)).is_err() {
                        return;
                    }
                    // A session's line can arrive while the app is idle: it is asked to look.
                    wake.as_ref().inspect(|w| w());
                }
            }
        }
    });
    rx
}

/// The last lines of the program's error output, for the log and for an error message.
pub(crate) type Kept = std::sync::Arc<std::sync::Mutex<ErrorLines>>;

/// The last lines of a program's error output, and how many earlier ones weren't kept.
#[derive(Debug, Default)]
pub(crate) struct ErrorLines {
    pub lines: std::collections::VecDeque<String>,
    pub dropped: usize,
    /// Whether the error output has ended (everything holding it open has closed it).
    pub done: bool,
}

/// The most lines of a program's error output kept until they're read.
const KEPT_ERROR_LINES: usize = 20;

/// Keeps the program's error output as it comes; `wake` is called as each line arrives (a
/// session's, so it is logged while the app is idle). Read, never joined: a process the program
/// started can hold the pipe open after the program itself has exited.
pub(crate) fn errors(err: impl Read + Send + 'static, wake: Option<crate::session::Wake>) -> Kept {
    let kept = Kept::default();
    let k = kept.clone();
    std::thread::spawn(move || {
        let mut r = BufReader::new(err);
        loop {
            // Bounded: endless output without a line end is read in pieces.
            let mut buf = Vec::new();
            match (&mut r).take(MAX_ERROR_LINE as u64).read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => {
                    if let Ok(mut k) = k.lock() {
                        k.done = true;
                    }
                    break;
                }
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf).trim_end().to_string();
            let Ok(mut k) = k.lock() else { break };
            k.lines.push_back(line.chars().take(1_000).collect::<String>());
            if k.lines.len() > KEPT_ERROR_LINES {
                k.lines.pop_front();
                k.dropped += 1;
            }
            drop(k);
            wake.as_ref().inspect(|w| w());
        }
    });
    kept
}

/// Stops the program, and what it started, if it hasn't exited on its own within `grace`.
pub(crate) fn finish(child: &mut Child, grace: Duration) {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if start.elapsed() < grace => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                // Before the program is reaped, so its id (its group's) can't have been reused.
                kill_tree(child);
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

/// Whether the program has exited. On Unix it isn't reaped yet: until it is, its id can't be
/// reused, so its process group can still be stopped safely ([`end`]).
pub(crate) fn exited(child: &mut Child) -> bool {
    #[cfg(unix)]
    {
        use rustix::io::Errno;
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        loop {
            match waitid(WaitId::Pid(Pid::from_child(child)), options) {
                Ok(found) => return found.is_some(),
                Err(Errno::INTR) => continue,
                // Reaped already (by its own status check, or by something else when the app
                // reaps every child or ignores SIGCHLD): gone either way.
                // (Not waited on again: its id may be another child's by now.)
                Err(Errno::CHILD) => return true,
                // Not known to have exited: asking `try_wait` would reap it, so it counts as running.
                Err(_) => return false,
            }
        }
    }
    #[cfg(not(unix))]
    matches!(child.try_wait(), Ok(Some(_)))
}

/// Ends a program kept running: gives it `grace` to exit, stops what it left running, then stops
/// it if it's still running and reaps it. Returns its exit status when it exited by itself (and
/// RustGIS reaped it), and whether it had to be stopped.
///
/// On Unix what it left running is its process group, stopped before the program is reaped so
/// the group's id can't belong to anything else yet. On Windows a process tree can't be found
/// from a program that has exited, so only a running one's is stopped.
pub(crate) fn end(child: &mut Child, grace: Duration) -> (Option<std::process::ExitStatus>, bool) {
    let start = Instant::now();
    while !exited(child) && start.elapsed() < grace {
        std::thread::sleep(Duration::from_millis(10));
    }
    let by_itself = exited(child);
    // Reaped already (by something else: an embedding host reaping every child, SIGCHLD
    // ignored): its id may be another process's by now, so nothing is signalled by it.
    #[cfg(unix)]
    let reaped = {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
        matches!(waitid(WaitId::Pid(Pid::from_child(child)), options), Err(rustix::io::Errno::CHILD))
    };
    #[cfg(not(unix))]
    let reaped = false;
    #[cfg(unix)]
    if !reaped {
        let _ = rustix::process::kill_process_group(rustix::process::Pid::from_child(child), rustix::process::Signal::KILL);
    }
    // Reaped elsewhere, it's gone: waiting on its id could wait on another child that has it now.
    if reaped {
        return (None, false);
    }
    if !by_itself {
        kill_tree(child);
        let _ = child.kill();
    }
    let status = child.wait().ok();
    (status.filter(|_| by_itself), !by_itself)
}

/// Stops the processes a program started: its process group (Unix), its process tree (Windows).
fn kill_tree(child: &Child) {
    #[cfg(unix)]
    {
        let _ = rustix::process::kill_process_group(rustix::process::Pid::from_child(child), rustix::process::Signal::KILL);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
}

/// The program's input: lines written on a thread, so a program that doesn't read can't block
/// RustGIS (the timeout still applies). Dropping it closes the input.
pub(crate) struct Writer {
    tx: Sender<Vec<u8>>,
    /// Lines sent and not yet written: how far behind the program is in reading.
    backlog: Arc<AtomicUsize>,
    /// Their bytes.
    bytes: Arc<AtomicUsize>,
}

/// How many lines may wait for the program to read them before a session's events are held
/// (`session::event`); when it catches up below this, the app is asked to send them.
pub(crate) const HOLD_EVENTS_AT: usize = 16;

impl Writer {
    /// Queues `v` as one line.
    pub(crate) fn send(&self, v: &Value) -> Result<(), String> {
        self.send_line(v.to_string().into_bytes())
    }

    /// Queues `line`, JSON already written out, as one line.
    pub(crate) fn send_line(&self, line: Vec<u8>) -> Result<(), String> {
        self.backlog.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(line.len(), Ordering::Relaxed);
        self.tx.send(line).map_err(|_| "it stopped reading".to_string())
    }

    /// Lines waiting for the program to read them.
    pub(crate) fn backlog(&self) -> usize {
        self.backlog.load(Ordering::Relaxed)
    }

    /// Bytes waiting for the program to read them.
    pub(crate) fn backlog_bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }
}

/// `wake` is called when the program, behind in reading, catches up below [`HOLD_EVENTS_AT`].
fn writer(mut stdin: impl Write + Send + 'static, wake: Option<crate::session::Wake>) -> Writer {
    let (tx, rx) = channel::<Vec<u8>>();
    let backlog = Arc::new(AtomicUsize::new(0));
    let bytes = Arc::new(AtomicUsize::new(0));
    let (left, left_bytes) = (backlog.clone(), bytes.clone());
    std::thread::spawn(move || {
        for mut line in rx {
            let n = line.len();
            line.push(b'\n');
            if stdin.write_all(&line).and_then(|_| stdin.flush()).is_err() {
                return;
            }
            left_bytes.fetch_sub(n, Ordering::Relaxed);
            if left.fetch_sub(1, Ordering::Relaxed) == HOLD_EVENTS_AT {
                wake.as_ref().inspect(|w| w());
            }
        }
    });
    Writer { tx, backlog, bytes }
}

/// A program started, with its input, its output lines and its error output.
pub(crate) struct Started {
    pub child: Child,
    pub stdin: Writer,
    pub rx: Receiver<Line>,
    pub errs: Kept,
}

/// Starts the program, its pipes connected; `wake` is called as each line of its output arrives.
pub(crate) fn spawn(spec: &Spec, wake: Option<crate::session::Wake>) -> Result<Started, String> {
    if let Some(check) = spec.env_check.as_ref().filter(|c| !c.ready()) {
        return Err(format!(
            "its Python environment isn't ready: prepare it in Plugins › Plugin Manager › Installed, or with rustgis-cli plugins prepare {}",
            check.id
        ));
    }
    let mut child = spec
        .command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't start {} ({e})", spec.program))?;
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        finish(&mut child, Duration::ZERO);
        return Err("couldn't connect to it".into());
    };
    Ok(Started { child, errs: errors(stderr, wake.clone()), rx: lines(stdout, wake.clone()), stdin: writer(stdin, wake) })
}

/// Runs one call: starts the program, sends `request`, answers its host requests through
/// `host`, and returns its `ok` value or its error. `timeout` (none for tools) stops it.
pub fn call(spec: &Spec, host: &mut dyn Host, request: &Value, timeout: Option<Duration>) -> Result<Value, String> {
    let Started { mut child, stdin, rx, errs } = spawn(spec, None)?;
    let res = converse(&stdin, &rx, host, request, timeout);
    // Its input closed, a program that answered exits; one that didn't is stopped. Its output
    // isn't read any more: one still printing gets a broken pipe rather than waiting on a full
    // queue until it's stopped.
    drop(rx);
    drop(stdin);
    finish(&mut child, if res.is_ok() { EXIT_GRACE } else { Duration::ZERO });
    // A moment for what a failing program wrote last to arrive.
    if res.is_err() {
        std::thread::sleep(Duration::from_millis(20));
    }
    let stderr: Vec<String> = errs.lock().map(|k| k.lines.iter().cloned().collect()).unwrap_or_default();
    for line in stderr.iter().filter(|l| !l.trim().is_empty()) {
        log::info!("plugin {}: {line}", spec.program);
    }
    res.map_err(|e| match stderr.iter().rev().find(|l| !l.trim().is_empty()) {
        Some(last) if e.contains("without answering") => format!("{e}: {last}"),
        _ => e,
    })
}

fn converse(stdin: &Writer, rx: &Receiver<Line>, host: &mut dyn Host, request: &Value, timeout: Option<Duration>) -> Result<Value, String> {
    stdin.send(request)?;
    let deadline = timeout.map(|t| Instant::now() + t);
    let (mut calls, mut logged) = (0u32, 0usize);
    loop {
        // Waits a little at a time, to notice a cancel (the program is then stopped by the
        // caller, as on a timeout).
        let line = loop {
            if host.cancelled() {
                return Err(CANCELLED.into());
            }
            // Checked whatever it sends: a program that keeps talking still runs out of time.
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err("it ran too long and was stopped".into());
            }
            let wait = deadline.map_or(CANCEL_POLL, |d| d.saturating_duration_since(Instant::now()).min(CANCEL_POLL));
            match rx.recv_timeout(wait) {
                Ok(l) => break l,
                Err(RecvTimeoutError::Timeout) if deadline.is_some_and(|d| Instant::now() >= d) => {
                    return Err("it ran too long and was stopped".into());
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break Line::End,
            }
        };
        let bytes = match line {
            Line::Text(b) => b,
            Line::End => return Err("it exited without answering".into()),
            Line::TooLong => return Err(format!("it sent a line longer than {} MB", MAX_LINE / (1024 * 1024))),
        };
        // Lines that aren't protocol messages are other output (a library printing, even a JSON
        // object of its own): logged and skipped.
        let msg = match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(m)) if ["call", "log", "progress", "error", "ok"].iter().any(|k| m.contains_key(*k)) => m,
            _ => {
                if logged < MAX_LOGGED {
                    logged += 1;
                    let text = String::from_utf8_lossy(&bytes);
                    let text = text.trim_end();
                    if !text.is_empty() {
                        log::info!("plugin output: {}", text.chars().take(1_000).collect::<String>());
                    }
                }
                continue;
            }
        };
        if let Some(c) = msg.get("call") {
            calls += 1;
            if calls > MAX_HOST_CALLS {
                return Err("it made too many requests".into());
            }
            let op = c.get("op").and_then(Value::as_str).unwrap_or_default();
            let params = c.get("params").cloned().unwrap_or(Value::Null);
            let reply = match host.call(op, &params) {
                Ok(v) => json!({"ok": v}),
                Err(e) => json!({"error": e}),
            };
            stdin.send(&reply)?;
        } else if let Some(text) = msg.get("log") {
            if logged < MAX_LOGGED {
                logged += 1;
                host.log(&text.as_str().map(str::to_string).unwrap_or_else(|| text.to_string()));
            }
        } else if let Some(text) = msg.get("progress") {
            host.progress(&text.as_str().map(str::to_string).unwrap_or_else(|| text.to_string()));
        } else if let Some(e) = msg.get("error") {
            return Err(e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string()));
        } else if let Some(v) = msg.get("ok") {
            return Ok(v.clone());
        }
    }
}
