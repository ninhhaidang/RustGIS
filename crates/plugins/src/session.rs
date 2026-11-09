//! Session process plugins (`"session": true`, desktop and command line): one program kept
//! running between the plugin's commands, so it can ask RustGIS for things at any time and hear
//! when the map changes. A linked view in a window of its own (a 3D globe) is one.
//!
//! RustGIS starts the program the first time one of the plugin's commands runs, in the plugin's
//! folder, and writes a first line `{"session": {"abi", "plugin", "rustgis", "lang"}}`. From
//! then on the lines go both ways, one JSON object each:
//!
//! - RustGIS to the program: `{"command": request, "id": n}`, a command to run (the request a
//!   per-call plugin gets), answered with `{"ok": value, "id": n}` or `{"error": "text", "id": n}`;
//!   the reply to one of its host requests, `{"ok": value}` or `{"error": "text"}` with the
//!   request's `id` when it gave one; and `{"event": "view" | "layers" | "selection" | "project",
//!   ...}` when the map changes or another project opens.
//! - The program to RustGIS: `{"call": {"op", "params"}, "id": n}` (the `id` optional), `log`
//!   and `progress` lines, and its answers to commands.
//!
//! The channel is the program's own input and output: no port, no token, nothing another program
//! can join. Its requests go through the same [`Host`] checks as a command's, within the plugin's
//! permissions. The program is stopped, with what it started, when the plugin is turned off,
//! removed or updated and when RustGIS closes ([`stop`], [`stop_all`]); its input closing means
//! it is being stopped (RustGIS closing or gone, however it ended; the plugin turned off, removed
//! or updated; the program failed) and is its sign to exit. A program that exits on its own (its window
//! closed) is started again by the plugin's next command; on Unix what it left running is stopped.
//!
//! The plugin's tools still start the program once per run, as a per-call process plugin's do.

use std::process::{Child, ExitStatus};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::process::{self, Kept, Line, Spec, Started, Writer};
use crate::runtime::Host;

/// Asks the app to look at its sessions (a repaint): a line arrived while it was idle.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The most lines of one program handled in one [`poll`]; the rest wait for the next.
pub const POLL_BATCH: usize = 64;
/// Why a program was stopped when the answers it hadn't read passed [`MAX_QUEUED_BYTES`].
const STALLED: &str = "the answers waiting for it to read passed 256 MB";
/// How long a program has to exit when its plugin is turned off, removed or updated: the app
/// waits, so a program that ignores its closed input holds it this long at most.
const STOP_GRACE: Duration = Duration::from_secs(1);
/// How long a program whose output ended has to finish exiting before it's stopped.
const END_GRACE: Duration = Duration::from_millis(200);
/// The most lines of a program's other output and `log` lines logged per [`LOG_WINDOW`].
const MAX_LOGGED: usize = 200;
const LOG_WINDOW: Duration = Duration::from_secs(60);
/// The most bytes of replies that may wait for the program to read them: one that keeps asking
/// without reading the answers is stopped rather than fill memory.
const MAX_QUEUED_BYTES: usize = 256 * 1024 * 1024;
/// How often a command waiting on the program checks whether it was cancelled.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// A program kept running for its plugin.
struct Running {
    plugin: String,
    program: String,
    child: Child,
    stdin: Writer,
    rx: Receiver<Line>,
    errs: Kept,
    /// The next command's `id`.
    next_id: u64,
    /// Lines logged since the window began: a program printing endlessly can't flood the log.
    logged: (Instant, usize),
    /// Events held while the program is behind in reading: the latest of each kind.
    held: Vec<Value>,
    /// How many polls have found it exited with its output still coming: something it left
    /// running keeps writing. At [`DRAIN_BATCHES`] it's stopped.
    drained: usize,
    /// Writing to it failed: it closed its input (stopped reading) while it may still run.
    input_closed: bool,
    /// The last line of its error output logged so far.
    last_stderr: Option<String>,
}

impl Running {
    /// Whether one more line may be logged now; says once when lines stop being logged.
    fn may_log(&mut self) -> bool {
        if self.logged.0.elapsed() > LOG_WINDOW {
            self.logged = (Instant::now(), 0);
        }
        self.logged.1 += 1;
        if self.logged.1 == MAX_LOGGED + 1 {
            log::info!("{}: more output than {MAX_LOGGED} lines a minute; the rest isn't logged", self.plugin);
        }
        self.logged.1 <= MAX_LOGGED
    }
}

static RUNNING: Mutex<Vec<Running>> = Mutex::new(Vec::new());
/// How programs found ended by a command (not by [`poll`]) ended: for the app to say ([`take_ended`]).
static ENDED: Mutex<Vec<(String, Ended)>> = Mutex::new(Vec::new());
/// The most [`poll`] batches read of a program found exited before it is stopped: what it sent
/// before exiting is finite, but what it left running may keep its output open and writing.
pub const DRAIN_BATCHES: usize = 256;

/// Keeps how `plugin`'s program ended when it failed, for [`take_ended`].
fn note_ended(plugin: &str, ended: Ended) {
    if (ended.failed || ended.unread)
        && let Ok(mut v) = ENDED.lock()
    {
        // One kept a plugin: the latest.
        v.retain(|(p, _)| p != plugin);
        v.push((plugin.to_string(), ended));
    }
}

/// How `plugin`'s program ended, if a command found it failed (and started it again) since
/// the last call: the app says so as it does for one [`poll`] finds.
pub fn take_ended(plugin: &str) -> Option<Ended> {
    let mut v = ENDED.lock().ok()?;
    let i = v.iter().position(|(p, _)| p == plugin)?;
    Some(v.swap_remove(i).1)
}

/// Whether `plugin`'s program is in the list but has exited: what it sent last is still to be
/// read ([`poll`]).
pub fn has_exited(plugin: &str) -> bool {
    RUNNING.lock().is_ok_and(|mut v| v.iter_mut().any(|r| r.plugin == plugin && process::exited(&mut r.child)))
}
static WAKE: Mutex<Option<Wake>> = Mutex::new(None);

/// Sets what a session's line arriving calls, for programs started from now on.
pub fn set_wake(wake: Wake) {
    if let Ok(mut w) = WAKE.lock() {
        *w = Some(wake);
    }
}

/// What a new program's lines call as they arrive ([`set_wake`]).
fn wake() -> Option<Wake> {
    // What the app set when it's called, not when the program started: one started before the
    // app's first frame wakes it too. At most one asked for until the app polls ([`polled`]):
    // a program printing line after line asks for one frame, not one a line.
    Some(Arc::new(|| {
        if !WOKEN.swap(true, std::sync::atomic::Ordering::Relaxed)
            && let Some(w) = WAKE.lock().ok().and_then(|w| w.clone())
        {
            w();
        }
    }))
}

/// Whether a wake was asked for since the app last polled.
static WOKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The app is about to poll: the next line asks for a frame again.
pub fn polled() {
    WOKEN.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// The plugins whose programs are running.
pub fn running() -> Vec<String> {
    RUNNING.lock().map(|v| v.iter().map(|r| r.plugin.clone()).collect()).unwrap_or_default()
}

/// Whether `plugin`'s program is running.
pub fn is_running(plugin: &str) -> bool {
    RUNNING.lock().is_ok_and(|v| v.iter().any(|r| r.plugin == plugin))
}

/// Takes a running program out of the list while it's talked to: what its requests run can't
/// reach the list's lock.
fn take(plugin: &str) -> Option<Running> {
    let mut v = RUNNING.lock().ok()?;
    let i = v.iter().position(|r| r.plugin == plugin)?;
    Some(v.swap_remove(i))
}

/// Puts a program back in the list after it was talked to; one that can't be kept is stopped.
fn put(r: Running) {
    match RUNNING.lock() {
        Ok(mut v) => v.push(r),
        // Not kept, so not left running either.
        Err(_) => {
            shut(r, Duration::ZERO);
        }
    }
}

/// Starts `plugin`'s program and greets it with the session's first line.
fn start(plugin: &str, spec: &Spec) -> Result<Running, String> {
    let Started { child, stdin, rx, errs } = process::spawn(spec, wake())?;
    let r = Running {
        plugin: plugin.to_string(),
        program: spec.program.clone(),
        child,
        stdin,
        rx,
        errs,
        next_id: 1,
        logged: (Instant::now(), 0),
        held: Vec::new(),
        drained: 0,
        input_closed: false,
        last_stderr: None,
    };
    let hello = json!({"session": {"abi": crate::manifest::ABI, "plugin": plugin, "rustgis": crate::manifest::RUSTGIS_VERSION, "lang": crate::language()}});
    if let Err(e) = r.stdin.send(&hello) {
        shut(r, Duration::ZERO);
        return Err(e);
    }
    Ok(r)
}

/// Closes the program's input, gives it `grace` to exit, then stops it with what it started.
/// Returns its exit status when it exited by itself, whether it had to be stopped, and the last
/// line of its error output.
fn shut(mut r: Running, grace: Duration) -> (Option<ExitStatus>, bool, Option<String>) {
    log_errors(&mut r);
    let Running { mut child, stdin, rx, errs, program, last_stderr, .. } = r;
    drop(stdin);
    drop(rx);
    let (status, stopped) = process::end(&mut child, grace);
    // What it wrote as it ended: until its error output has ended, or a moment at most, as
    // something holding the pipe open (which `end` couldn't stop) mustn't hold this.
    let until = Instant::now() + END_GRACE;
    while !errs.lock().is_ok_and(|k| k.done) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
    }
    let last = drain_errors(&errs, &program).or(last_stderr);
    (status, stopped, last)
}

/// Logs the error output the program wrote since the last call, as it runs.
fn log_errors(r: &mut Running) {
    let (lines, dropped) = r.errs.lock().map(|mut k| (k.lines.drain(..).collect(), std::mem::take(&mut k.dropped))).unwrap_or_default();
    let lines: Vec<String> = lines;
    if dropped > 0 && r.may_log() {
        log::info!("plugin {}: {dropped} earlier lines of its error output weren't kept", r.program);
    }
    for line in lines.into_iter().filter(|l| !l.trim().is_empty()) {
        if r.may_log() {
            log::info!("plugin {}: {line}", r.program);
        }
        r.last_stderr = Some(line);
    }
}

/// Logs what is left of a program's error output; returns its last line.
fn drain_errors(errs: &Kept, program: &str) -> Option<String> {
    let (lines, dropped) = errs.lock().map(|mut k| (k.lines.drain(..).collect(), std::mem::take(&mut k.dropped))).unwrap_or_default();
    let lines: Vec<String> = lines;
    if dropped > 0 {
        log::info!("plugin {program}: {dropped} earlier lines of its error output weren't kept");
    }
    for line in lines.iter().filter(|l| !l.trim().is_empty()) {
        log::info!("plugin {program}: {line}");
    }
    lines.into_iter().rev().find(|l| !l.trim().is_empty())
}

/// Stops `plugin`'s program, if it's running.
pub fn stop(plugin: &str) {
    if let Some(r) = take(plugin) {
        shut(r, STOP_GRACE);
    }
}

/// Stops every program (RustGIS is closing): their inputs close together, and they share the
/// time to exit.
pub fn stop_all() {
    let all: Vec<Running> = RUNNING.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
    let mut waiting = Vec::new();
    for r in all {
        let Running { child, stdin, rx, errs, program, .. } = r;
        drop(stdin);
        drop(rx);
        waiting.push((child, errs, program));
    }
    let end = Instant::now() + process::EXIT_GRACE;
    for (mut child, errs, program) in waiting {
        process::end(&mut child, end.saturating_duration_since(Instant::now()));
        drain_errors(&errs, &program);
    }
}

/// A protocol line from the program, or `None` for other output (logged, within limits).
fn message(r: &mut Running, bytes: &[u8]) -> Option<Map<String, Value>> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(m)) if ["call", "log", "progress", "error", "ok"].iter().any(|k| m.contains_key(*k)) => Some(m),
        _ => {
            let text = String::from_utf8_lossy(bytes);
            let text = text.trim_end();
            if !text.is_empty() && r.may_log() {
                log::info!("plugin output: {}", text.chars().take(1_000).collect::<String>());
            }
            None
        }
    }
}

/// A message's text: a string as it is, anything else as JSON.
fn text(v: &Value) -> String {
    v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())
}

/// Handles a line that isn't an answer to a command: a host request (answered), a log or
/// progress line. Returns the answer's `id` and value when it is one.
fn handle(m: &Map<String, Value>, host: &mut dyn Host, r: &mut Running) -> Result<Option<(Value, Result<Value, String>)>, String> {
    let id = m.get("id").cloned().unwrap_or(Value::Null);
    if let Some(c) = m.get("call") {
        let op = c.get("op").and_then(Value::as_str).unwrap_or_default();
        let params = c.get("params").cloned().unwrap_or(Value::Null);
        // A request that fails unexpectedly is an error answer, as in a WebAssembly plugin: the
        // program, out of the list while it's talked to, is never left running untracked.
        let answer = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.call(op, &params)))
            .unwrap_or_else(|_| Err(format!("'{op}' failed unexpectedly")));
        let mut reply = match answer {
            Ok(v) => json!({"ok": v}),
            Err(e) => json!({"error": e}),
        };
        if !id.is_null()
            && let Some(o) = reply.as_object_mut()
        {
            o.insert("id".into(), id);
        }
        // An answer too large to send at all is refused to it, whether it reads or not; one
        // that would only overflow what it hasn't read yet means it stopped reading.
        // Written out once: measured, then sent as it is (an answer can be hundreds of MB).
        let mut line = reply.to_string().into_bytes();
        let size = line.len();
        if size > MAX_QUEUED_BYTES {
            let id = reply.get("id").cloned();
            reply = json!({"error": format!("the answer is larger than {} MB", MAX_QUEUED_BYTES / (1024 * 1024))});
            if let (Some(id), Some(o)) = (id, reply.as_object_mut()) {
                o.insert("id".into(), id);
            }
            line = reply.to_string().into_bytes();
        }
        // What is sent, refusal or not, must fit with what it hasn't read yet.
        if r.stdin.backlog_bytes().saturating_add(line.len()) > MAX_QUEUED_BYTES {
            return Err(STALLED.into());
        }
        r.stdin.send_line(line)?;
    } else if let Some(t) = m.get("log") {
        if r.may_log() {
            host.log(&text(t));
        }
    } else if let Some(t) = m.get("progress") {
        host.progress(&text(t));
    } else if let Some(e) = m.get("error") {
        return Ok(Some((id, Err(text(e)))));
    } else if let Some(v) = m.get("ok") {
        return Ok(Some((id, Ok(v.clone()))));
    }
    Ok(None)
}

/// Runs a command of `plugin` in its program, starting it if it isn't running: sends `request`,
/// answers its host requests through `host`, and returns its answer. A program that doesn't
/// answer within `timeout` is stopped.
pub fn command(plugin: &str, spec: &Spec, host: &mut dyn Host, request: &Value, timeout: Duration) -> Result<Value, String> {
    let mut r = match take(plugin) {
        Some(mut r) => {
            if !process::exited(&mut r.child) {
                r
            } else {
                // It exited on its own (its window closed): what it sent last is handled first,
                // then it is started again.
                put(r);
                // All of it, a batch at a time; then how it ended is kept for the app to say.
                for _ in 0..DRAIN_BATCHES {
                    let polled = poll(plugin, host);
                    if let Some(ended) = polled.ended {
                        note_ended(plugin, ended);
                        break;
                    }
                }
                // Still output past that: what it left running keeps writing. Stopped, and its
                // exit status says whether it failed.
                if let Some(r) = take(plugin) {
                    log::warn!("{plugin}: its output was still coming after it exited; the rest wasn't read");
                    let (status, _, last_error) = shut(r, Duration::ZERO);
                    // No status (stopped here, or reaped elsewhere) isn't a failure: only its own exit says so.
                    note_ended(plugin, Ended { failed: status.is_some_and(|s| !s.success()), last_error, unread: true, reason: None });
                }
                start(plugin, spec)?
            }
        }
        None => start(plugin, spec)?,
    };
    let id = r.next_id;
    r.next_id += 1;
    if let Err(e) = r.stdin.send(&json!({"command": request, "id": id})) {
        shut(r, Duration::ZERO);
        return Err(e);
    }
    let deadline = Instant::now() + timeout;
    let mut calls = 0u32;
    let res: Result<(), String> = loop {
        if host.cancelled() {
            break Err(process::CANCELLED.to_string());
        }
        // Checked whatever it sends: a program that keeps talking still runs out of time.
        if Instant::now() >= deadline {
            break Err("it ran too long and was stopped".to_string());
        }
        let line = match r.rx.recv_timeout(deadline.saturating_duration_since(Instant::now()).min(CANCEL_POLL)) {
            Ok(l) => l,
            Err(RecvTimeoutError::Timeout) if Instant::now() >= deadline => break Err("it ran too long and was stopped".to_string()),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => Line::End,
        };
        let bytes = match line {
            Line::Text(b) => b,
            Line::End => break Err("it exited without answering".to_string()),
            Line::TooLong => break Err(format!("it sent a line longer than {} MB", process::MAX_LINE / (1024 * 1024))),
        };
        let Some(m) = message(&mut r, &bytes) else { continue };
        if m.contains_key("call") {
            calls += 1;
            if calls > process::MAX_HOST_CALLS {
                break Err("it made too many requests".to_string());
            }
        }
        match handle(&m, host, &mut r) {
            Err(e) => break Err(e),
            Ok(Some((answered, res))) if answered == json!(id) => {
                put(r);
                return res;
            }
            // Not this command's: an answer it had given up on.
            Ok(Some(_)) => {
                if r.may_log() {
                    log::info!("{plugin}: an answer to no command it was running");
                }
            }
            Ok(None) => {}
        }
    };
    // It stopped answering: stopped, so the next command starts it afresh.
    let err = res.err().unwrap_or_default();
    match shut(r, Duration::ZERO) {
        (_, _, Some(last)) if err.contains("without answering") => Err(format!("{err}: {last}")),
        _ => Err(err),
    }
}

/// What [`poll`] found.
#[derive(Debug, Default, PartialEq)]
pub struct Polled {
    /// Lines handled.
    pub handled: usize,
    /// The program exited, or was stopped for what it sent.
    pub ended: Option<Ended>,
}

/// How a session's program ended.
#[derive(Debug, PartialEq)]
pub struct Ended {
    /// Whether it failed: exited with an error, or was stopped. Exiting by itself (its window
    /// closed) isn't failing.
    pub failed: bool,
    /// The last line of its error output.
    pub last_error: Option<String>,
    /// Whether what it sent after exiting wasn't all read: something it left running kept
    /// writing past [`DRAIN_BATCHES`], and its requests from then on weren't answered.
    pub unread: bool,
    /// Why RustGIS stopped it, when it did for what it sent.
    pub reason: Option<String>,
}

/// Answers what `plugin`'s program sent since the last call, at most [`POLL_BATCH`] lines,
/// without waiting for more. Each host request goes to `host`.
pub fn poll(plugin: &str, host: &mut dyn Host) -> Polled {
    let Some(mut r) = take(plugin) else { return Polled::default() };
    log_errors(&mut r);
    let mut out = Polled::default();
    // Whether it ended, and whether it was stopped for what it sent.
    let mut ended = None;
    let mut reason = None;
    // It exited: what it wrote last may still be on its way from the pipe, and is read first.
    // Checked first too: what it left running may keep its output flowing, so the channel never
    // runs empty to say so.
    let mut gone = process::exited(&mut r.child).then(|| Instant::now() + END_GRACE);
    for _ in 0..POLL_BATCH {
        let line = if let Some(until) = gone {
            // One moment for all of it, not one a line.
            r.rx.recv_timeout(until.saturating_duration_since(Instant::now())).ok()
        } else {
            match r.rx.try_recv() {
                Ok(l) => Some(l),
                // Nothing new: it may have exited, its output held open by what it started.
                Err(TryRecvError::Empty) => {
                    if process::exited(&mut r.child) {
                        gone = Some(Instant::now() + END_GRACE);
                        continue;
                    }
                    break;
                }
                Err(TryRecvError::Disconnected) => None,
            }
        };
        let bytes = match line {
            Some(Line::Text(b)) => b,
            None | Some(Line::End) => {
                ended = Some(false);
                break;
            }
            Some(Line::TooLong) => {
                let why = format!("it sent a line longer than {} MB", process::MAX_LINE / (1024 * 1024));
                log::warn!("{plugin}: {why}, and was stopped");
                reason = Some(why);
                ended = Some(true);
                break;
            }
        };
        out.handled += 1;
        let Some(m) = message(&mut r, &bytes) else { continue };
        match handle(&m, host, &mut r) {
            Ok(None) => {}
            Ok(Some(_)) => {
                if r.may_log() {
                    log::info!("{plugin}: an answer to no command it was running");
                }
            }
            // The answers it hasn't read passed the limit: stopped.
            Err(e) if e == STALLED => {
                log::warn!("{plugin}: {e}, and was stopped");
                reason = Some(e);
                ended = Some(true);
                break;
            }
            // Exited, it can't be answered; what it asked was done.
            Err(_) if gone.is_some() => {}
            // It can't be written to: it's exiting, and what it sent last is read first.
            Err(_) => {
                r.input_closed = true;
                gone = Some(Instant::now() + END_GRACE);
            }
        }
    }
    // Exited, and its output still coming after this batch: something it left running keeps
    // writing. Poll after poll, that's bounded too: past it, stopped with what it left running.
    if ended.is_none() && gone.is_some() {
        r.drained += 1;
        if r.drained >= DRAIN_BATCHES {
            log::warn!("{plugin}: its output was still coming after it exited; the rest wasn't read");
            let (status, _, last_error) = shut(r, Duration::ZERO);
            out.ended = Some(Ended { failed: status.is_some_and(|s| !s.success()), last_error, unread: true, reason: None });
            return out;
        }
    }
    // It stopped reading while it still runs: nothing RustGIS says reaches it. Stopped, and said.
    if ended.is_none() && r.input_closed && !process::exited(&mut r.child) {
        reason = Some("it stopped reading its input while running".to_string());
        ended = Some(true);
    }
    let Some(stopped) = ended else {
        flush(&mut r);
        put(r);
        // A full batch may have left more: another frame for them, not whenever one comes.
        if out.handled >= POLL_BATCH
            && let Some(w) = wake()
        {
            w();
        }
        return out;
    };
    // Its own exit status says whether it failed; one stopped for what it sent did. Its output
    // ends as it exits: a moment for the exit to land, on the app's thread. One still running
    // after it is stopped, but closing its output was its own doing: no failure.
    let input_closed = r.input_closed;
    let (status, had_to_stop, last_error) = shut(r, if stopped { Duration::ZERO } else { END_GRACE });
    // Its output closed but it didn't exit: it was stopped, and that's said. (No status of its
    // own isn't enough: something else may have reaped it after a clean exit.)
    if !stopped && had_to_stop && reason.is_none() {
        reason = Some(
            if input_closed { "it stopped reading its input while running" } else { "it closed its output but kept running" }.to_string(),
        );
    }
    out.ended = Some(Ended { failed: stopped || status.is_some_and(|s| !s.success()), last_error, unread: false, reason });
    out
}

/// Tells `plugin`'s program what changed: `event` is `{"event": name, ...}`. While the program
/// is behind in reading, events wait, the latest of each kind replacing an earlier one, so a
/// program that stalls while the map moves doesn't fill memory. One that exited is found out by
/// the next [`poll`].
pub fn event(plugin: &str, event: &Value) {
    if let Ok(mut v) = RUNNING.lock()
        && let Some(r) = v.iter_mut().find(|r| r.plugin == plugin)
    {
        r.held.retain(|e| e.get("event") != event.get("event"));
        r.held.push(event.clone());
        flush(r);
    }
}

/// Sends the events held for the program, unless it's still behind in reading.
fn flush(r: &mut Running) {
    if r.stdin.backlog() < process::HOLD_EVENTS_AT {
        for e in std::mem::take(&mut r.held) {
            if r.stdin.send(&e).is_err() {
                r.input_closed = true;
            }
        }
    }
}
