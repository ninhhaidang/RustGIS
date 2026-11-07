//! The file bridge: for agents that can't use MCP, or when the relay's port is taken. An agent
//! writes a script (the `script.run` format: one `command-id {json}` per line, `#` comments) to
//! the command file; the app runs it on the open project and writes the outcome to the result
//! file. The files live in the relay's private folder ([`crate::relay::token_dir`]), which only
//! this user can read or write, and a marker file says the bridge is on.
//!
//! A thread watches the command file's modification time every [`POLL`] and, once it has stayed
//! the same for a tick (the agent finished writing), wakes the app; the
//! app runs the script on its own thread ([`FileBridge::poll`]), through the MCP server's `run`
//! tool, so the same refusal policy applies as to an attached agent, and the script is one undo
//! step. Whatever the file held when the bridge started is not run.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustgis_engine::Session;
use serde_json::{Value, json};

use crate::mcp::Server;

/// How often the command file is checked.
pub const POLL: Duration = Duration::from_millis(500);
/// The command file an agent writes its script to.
pub const COMMAND: &str = "bridge-command.txt";
/// The file the outcome is written to.
pub const RESULT: &str = "bridge-result.json";
/// The marker that says the bridge is on, with the files' paths.
pub const READY: &str = "bridge-ready.json";
/// The largest script read.
const MAX_SCRIPT: u64 = 1024 * 1024;
/// The bridge's name in the undo history ([`Session::actor`]).
pub const BRIDGE_ACTOR: &str = "file bridge";
/// The lock one app holds while its bridge runs, so a second app doesn't run the same scripts.
const LOCK: &str = "bridge.lock";

/// What a run of the command file came to, for the app's status bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub success: bool,
    /// The error, when it failed.
    pub error: Option<String>,
}

struct Inner {
    dir: PathBuf,
    stopped: AtomicBool,
    /// The command file changed since it was last run.
    changed: AtomicBool,
    /// The command file's [`Stamp`] when last seen.
    seen: Mutex<Option<Stamp>>,
    allow_access: AtomicBool,
    /// Scripts may run any command ([`FileBridge::set_full_access`]).
    full_access: AtomicBool,
    /// The audit log's folder, if scripts are logged ([`FileBridge::set_audit_dir`]).
    audit: Mutex<Option<PathBuf>>,
    /// The layer scripts act on when they name none, once a script chose one (as an agent on the
    /// relay keeps its own, apart from the user's selection).
    active: Mutex<Option<u32>>,
    wake: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// The locked [`LOCK`] file, held until the bridge stops (the system releases it if the app
    /// ends any other way).
    lock: Mutex<Option<std::fs::File>>,
}

/// A running file bridge. Dropping the last clone stops it and removes the marker.
#[derive(Clone)]
pub struct FileBridge {
    inner: Arc<Inner>,
    _owner: Arc<Owner>,
}

/// Stops the bridge when the last [`FileBridge`] goes.
struct Owner(Arc<Inner>);

impl Drop for Owner {
    fn drop(&mut self) {
        shutdown(&self.0);
    }
}

impl std::fmt::Debug for FileBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileBridge").field("dir", &self.inner.dir).finish()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The command file's modification time and size, if it is a regular file.
fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::symlink_metadata(path).ok().filter(|m| m.is_file())?;
    // The change time too where there is one: it moves on every write, to the nanosecond, so a
    // rewrite of the same length within the modification time's resolution is still seen.
    #[cfg(unix)]
    let changed = {
        use std::os::unix::fs::MetadataExt;
        (meta.ctime(), meta.ctime_nsec(), meta.ino())
    };
    #[cfg(not(unix))]
    let changed = (0, 0, 0);
    Some((meta.modified().ok()?, meta.len(), changed))
}

/// What says the command file changed: its modification time, size, and (on Unix) change time
/// and inode.
type Stamp = (SystemTime, u64, (i64, i64, u64));

fn shutdown(inner: &Inner) {
    if !inner.stopped.swap(true, SeqCst) {
        let _ = std::fs::remove_file(inner.dir.join(READY));
        if let Some(f) = lock(&inner.lock).take() {
            let _ = f.unlock();
        }
    }
}

impl FileBridge {
    /// Starts the bridge in the relay's private folder.
    pub fn start() -> Result<Self, String> {
        Self::start_in(crate::relay::private_dir()?)
    }

    /// Starts the bridge in `dir` (a test's folder).
    pub fn start_in(dir: PathBuf) -> Result<Self, String> {
        // One bridge per folder: a second app would run every script again on its own project.
        let lock_path = dir.join(LOCK);
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| format!("{}: {e}", lock_path.display()))?;
        match lock_file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err("another RustGIS is running the file bridge; close it to use the bridge here".into());
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(format!("{}: {e}", lock_path.display())),
        }
        let inner = Arc::new(Inner {
            lock: Mutex::new(Some(lock_file)),
            seen: Mutex::new(stamp(&dir.join(COMMAND))),
            dir,
            stopped: AtomicBool::new(false),
            changed: AtomicBool::new(false),
            allow_access: AtomicBool::new(false),
            full_access: AtomicBool::new(false),
            audit: Mutex::new(None),
            active: Mutex::new(None),
            wake: Mutex::new(None),
        });
        let started = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let ready = json!({
            "pid": std::process::id(),
            "command": inner.dir.join(COMMAND),
            "result": inner.dir.join(RESULT),
            "started_at": crate::chatgpt::rfc3339(i64::try_from(started).unwrap_or(0)),
            "version": env!("CARGO_PKG_VERSION"),
            "schema": 1,
        });
        write_atomic(&inner.dir, READY, &ready.to_string())?;
        let watch = Arc::clone(&inner);
        std::thread::Builder::new()
            .name("rustgis-file-bridge".into())
            .spawn(move || {
                // A change runs once the file has stayed the same for a tick: an agent still
                // writing it (truncated, then filled) doesn't get half its script run.
                let mut settling = None;
                while !watch.stopped.load(SeqCst) {
                    std::thread::sleep(POLL);
                    let now = stamp(&watch.dir.join(COMMAND));
                    let mut seen = lock(&watch.seen);
                    if now.is_none() || now == *seen {
                        settling = None;
                        continue;
                    }
                    if settling != Some(now) {
                        settling = Some(now);
                        continue;
                    }
                    settling = None;
                    *seen = now;
                    drop(seen);
                    watch.changed.store(true, SeqCst);
                    if let Some(wake) = lock(&watch.wake).as_ref() {
                        wake();
                    }
                }
            })
            .map_err(|e| format!("the file bridge could not start: {e}"))?;
        Ok(Self { _owner: Arc::new(Owner(Arc::clone(&inner))), inner })
    }

    /// The folder the bridge's files are in.
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// The command file an agent writes its script to.
    pub fn command_path(&self) -> PathBuf {
        self.inner.dir.join(COMMAND)
    }

    /// The file the outcome is written to.
    pub fn result_path(&self) -> PathBuf {
        self.inner.dir.join(RESULT)
    }

    /// What runs when the command file changes: the app's `request_repaint`, so a frame runs it.
    pub fn set_wake(&self, wake: Box<dyn Fn() + Send + Sync>) {
        *lock(&self.inner.wake) = Some(wake);
    }

    /// Whether the app gave the bridge its wake yet.
    pub fn has_wake(&self) -> bool {
        lock(&self.inner.wake).is_some()
    }

    /// Lets scripts use files and the network (the user turned agents on in the app).
    pub fn set_allow_access(&self, allow: bool) {
        self.inner.allow_access.store(allow, SeqCst);
    }

    /// Lets scripts run any command, as the relay's agents may with the user's full access.
    pub fn set_full_access(&self, on: bool) {
        self.inner.full_access.store(on, SeqCst);
    }

    /// Writes each script run to the audit log in `dir` ([`crate::audit`], source `file`), or to
    /// none.
    pub fn set_audit_dir(&self, dir: Option<PathBuf>) {
        *lock(&self.inner.audit) = dir;
    }

    /// Stops watching and removes the marker. Idempotent.
    pub fn stop(&self) {
        shutdown(&self.inner);
    }

    /// Runs the command file on `s` if it changed since it was last run, and writes the result
    /// file. `None` when there was nothing to run.
    pub fn poll(&self, s: &mut Session) -> Option<Outcome> {
        if self.inner.stopped.load(SeqCst) || !self.inner.changed.swap(false, SeqCst) {
            return None;
        }
        // The file the watcher saw settle, and no other: one replaced meanwhile (or while it's
        // read) is left for the watcher to settle and run once whole.
        let command = self.command_path();
        let settled = *lock(&self.inner.seen);
        if stamp(&command) != settled {
            return None;
        }
        let script = read_script(&command);
        if stamp(&command) != settled {
            return None;
        }
        // The last run's outcome goes first, so an agent waiting for this run's never reads it.
        let _ = std::fs::remove_file(self.result_path());
        let started = Instant::now();
        let mut outcome = match script {
            Ok(script) => {
                let server = Server {
                    users_project: true,
                    full_access: self.inner.full_access.load(SeqCst),
                    ..Server::new(self.inner.allow_access.load(SeqCst))
                };
                let call =
                    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "run", "arguments": {"script": script}}})
                        .to_string();
                // Like an agent on the relay: named in the undo history (so its undo takes back only
                // its own steps), with a layer of its own once a script chose one, and the user's
                // selection given back afterwards.
                let users = s.active_layer;
                let mine = lock(&self.inner.active).filter(|id| s.map().layer(*id).is_some());
                if mine.is_some() {
                    s.active_layer = mine;
                }
                let entered = s.active_layer;
                s.actor = Some(BRIDGE_ACTOR.to_string());
                let access = if server.full_access {
                    rustgis_engine::policy::AgentAccess::Full
                } else if server.allow_access {
                    rustgis_engine::policy::AgentAccess::Allow
                } else {
                    rustgis_engine::policy::AgentAccess::Refuse
                };
                let scope = rustgis_engine::policy::Scope::agent(
                    access,
                    true,
                    BRIDGE_ACTOR,
                    Some(BRIDGE_ACTOR.into()),
                    Arc::new(BridgeLive(Arc::downgrade(&self.inner))),
                );
                let text =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| server.handle_scoped(s, &call, scope, Vec::new(), "bridge")))
                        .ok()
                        .flatten();
                s.actor = None;
                if s.active_layer != entered || (crate::relay::selects_layer(&call) && crate::relay::reply_ok(text.as_deref())) {
                    *lock(&self.inner.active) = s.active_layer;
                }
                s.active_layer = users.filter(|id| s.map().layer(*id).is_some());
                // Logged as an agent's call through the relay is.
                let audit = lock(&self.inner.audit).clone();
                if let Some(dir) = audit {
                    crate::audit::record(&dir, "file", &call, text.as_deref(), started.elapsed());
                }
                let reply = text.and_then(|r| serde_json::from_str::<Value>(&r).ok()).unwrap_or(Value::Null);
                result_of(&reply, started.elapsed())
            }
            Err(e) => json!({"success": false, "result": null, "error": e, "notices": [], "duration_s": 0.0}),
        };
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        if let Some(o) = outcome.as_object_mut() {
            o.insert("timestamp".into(), json!(crate::chatgpt::rfc3339(i64::try_from(secs).unwrap_or(0))));
        }
        // A result that can't be written is said: an agent waiting for it would wait forever.
        if let Err(e) = write_atomic(&self.inner.dir, RESULT, &outcome.to_string()) {
            return Some(Outcome { success: false, error: Some(format!("the result couldn't be written: {e}")) });
        }
        Some(Outcome {
            success: outcome.get("success") == Some(&Value::Bool(true)),
            error: outcome.get("error").and_then(Value::as_str).map(str::to_string),
        })
    }
}

/// The command file's script: a regular file of at most [`MAX_SCRIPT`] bytes, as UTF-8.
fn read_script(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    // Not a link (the check), then the file opened is checked again and read to the limit only,
    // however it changes in between.
    if !std::fs::symlink_metadata(path).map_err(err)?.is_file() {
        return Err(format!("{}: not a file", path.display()));
    }
    let file = std::fs::File::open(path).map_err(err)?;
    if !file.metadata().map_err(err)?.is_file() {
        return Err(format!("{}: not a file", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_SCRIPT + 1).read_to_end(&mut bytes).map_err(err)?;
    if bytes.len() as u64 > MAX_SCRIPT {
        return Err(format!("{}: larger than {MAX_SCRIPT} bytes", path.display()));
    }
    String::from_utf8(bytes).map_err(|_| format!("{}: not UTF-8 text", path.display()))
}

/// The result file's JSON from the `run` tool's reply: `{success, result, error, notices,
/// duration_s}`. A refusal (plain text, not the tool's JSON) is the error.
fn result_of(reply: &Value, took: Duration) -> Value {
    let text = reply.pointer("/result/content/0/text").and_then(Value::as_str).unwrap_or("");
    let is_error = reply.pointer("/result/isError") == Some(&Value::Bool(true));
    match serde_json::from_str::<Value>(text) {
        Ok(v) if v.get("success").is_some() => v,
        _ => {
            let error = if text.is_empty() {
                reply.pointer("/error/message").and_then(Value::as_str).unwrap_or("the script did not run").to_string()
            } else {
                text.to_string()
            };
            json!({"success": !is_error && reply.get("error").is_none() && !text.is_empty(), "result": null, "error": error, "notices": [],
                "duration_s": (took.as_secs_f64() * 1000.0).round() / 1000.0})
        }
    }
}

/// Writes `text` to `name` in `dir` through a temporary file and a rename, so a reader never
/// sees half of it. Readable by this user only.
fn write_atomic(dir: &Path, name: &str, text: &str) -> Result<(), String> {
    use std::io::Write;
    let path = dir.join(name);
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let mut f = o.open(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    drop(f);
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

struct BridgeLive(std::sync::Weak<Inner>);
impl rustgis_engine::policy::Revocation for BridgeLive {
    fn allows(&self, e: rustgis_engine::policy::Effects) -> bool {
        use rustgis_engine::policy::Effect;
        let Some(inner) = self.0.upgrade() else {
            return false;
        };
        if inner.stopped.load(SeqCst) {
            return false;
        }
        if e.contains(Effect::Reserved) || e.contains(Effect::Automation) || e.contains(Effect::Replaces) {
            return inner.full_access.load(SeqCst);
        }
        if e.contains(Effect::Files) || e.contains(Effect::Network) || e.contains(Effect::Plugin) {
            return inner.allow_access.load(SeqCst) || inner.full_access.load(SeqCst);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::Project;

    fn sample() -> Session {
        let mut s = Session::new(Project::default());
        s.run("project.sample", &Value::Null).unwrap();
        s
    }

    /// Waits until the bridge sees a change and runs it, failing after five seconds.
    fn run_next(b: &FileBridge, s: &mut Session) -> Outcome {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(o) = b.poll(s) {
                return o;
            }
            assert!(Instant::now() < deadline, "the bridge never ran the command file");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Writes the command file so its stamp changes even within the clock's resolution.
    fn write_command(b: &FileBridge, text: &str) {
        let before = stamp(&b.command_path());
        std::fs::write(b.command_path(), text).unwrap();
        // A same-size write in the same instant would look unchanged: nudge the time.
        if stamp(&b.command_path()) == before {
            let f = std::fs::File::options().write(true).open(b.command_path()).unwrap();
            f.set_modified(SystemTime::now() + Duration::from_secs(1)).unwrap();
        }
    }

    /// A script written to the command file runs on the session as one undo step, and its
    /// outcome lands in the result file; the marker says where the files are while it runs.
    #[test]
    fn runs_the_command_file_and_writes_the_result() {
        let dir = tempfile::tempdir().unwrap();
        // What the file held before the bridge started is not run.
        std::fs::write(dir.path().join(COMMAND), "layer.addSample {\"name\": \"rivers\"}").unwrap();
        let b = FileBridge::start_in(dir.path().to_path_buf()).unwrap();
        let ready: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join(READY)).unwrap()).unwrap();
        assert_eq!(ready["pid"], std::process::id());
        assert_eq!(ready["command"], b.command_path().display().to_string());
        let mut s = sample();
        let (layers, undo) = (s.map().layers.len(), s.undo_depth());
        std::thread::sleep(POLL * 2);
        assert!(b.poll(&mut s).is_none(), "the old command file ran");

        write_command(&b, "# two layers\nlayer.addSample {\"name\": \"countries\"}\nlayer.addSample {\"name\": \"cities\"}\n");
        let o = run_next(&b, &mut s);
        assert!(o.success, "{o:?}");
        assert_eq!((s.map().layers.len(), s.undo_depth()), (layers + 2, undo + 1));
        let r: Value = serde_json::from_str(&std::fs::read_to_string(b.result_path()).unwrap()).unwrap();
        assert_eq!(r["success"], true, "{r}");
        assert!(r["timestamp"].as_str().is_some_and(|t| t.ends_with('Z')) && r["error"].is_null(), "{r}");

        // A new script takes the last outcome away until its own is written.
        std::fs::write(b.command_path(), "map.zoomIn\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !b.inner.changed.load(SeqCst) {
            assert!(Instant::now() < deadline, "the change was never seen");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(b.result_path().exists(), "kept until the run");
        assert!(run_next(&b, &mut s).success);
        // A failing line: nothing kept, the error names it.
        write_command(&b, "layer.addSample {\"name\": \"countries\"}\nmap.basemap {\"name\": \"nowhere\"}\n");
        let o = run_next(&b, &mut s);
        assert!(!o.success && o.error.as_deref().is_some_and(|e| e.starts_with("line 2")), "{o:?}");
        assert_eq!(s.map().layers.len(), layers + 2);
        let r: Value = serde_json::from_str(&std::fs::read_to_string(b.result_path()).unwrap()).unwrap();
        assert_eq!(r["success"], false, "{r}");

        b.stop();
        assert!(!dir.path().join(READY).exists(), "the marker goes when the bridge stops");
    }

    /// The bridge follows the relay's policy: files and the network only when the app allows
    /// them, and never replacing the user's project.
    #[test]
    fn scripts_follow_the_relays_refusals() {
        let dir = tempfile::tempdir().unwrap();
        let b = FileBridge::start_in(dir.path().to_path_buf()).unwrap();
        let mut s = sample();
        let layers = s.map().layers.len();
        let out = dir.path().join("out.png");
        write_command(&b, &format!("share.exportMap {}", json!({"path": out.display().to_string()})));
        let o = run_next(&b, &mut s);
        assert!(!o.success && o.error.as_deref().is_some_and(|e| e.contains("line 1")), "{o:?}");
        assert!(!out.exists());
        write_command(&b, "project.new {}");
        let o = run_next(&b, &mut s);
        assert!(!o.success && o.error.as_deref().is_some_and(|e| e.contains("reserved to the user")), "{o:?}");
        assert_eq!(s.map().layers.len(), layers);
        // Allowed by the app: the export runs.
        b.set_allow_access(true);
        write_command(&b, &format!("share.exportMap {}", json!({"path": out.display().to_string(), "width": 64, "height": 64})));
        let o = run_next(&b, &mut s);
        assert!(o.success, "{o:?}");
        assert!(out.exists());
    }

    /// One bridge per folder: a second app's bridge is refused until the first stops.
    #[test]
    fn a_second_bridge_in_the_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let first = FileBridge::start_in(dir.path().to_path_buf()).unwrap();
        let e = FileBridge::start_in(dir.path().to_path_buf()).unwrap_err();
        assert!(e.contains("another RustGIS"), "{e}");
        first.stop();
        let second = FileBridge::start_in(dir.path().to_path_buf()).unwrap();
        second.stop();
    }

    /// With an audit folder, each script run is logged with source `file`; with full access, a
    /// script may run the user's own automation.
    #[test]
    fn scripts_are_audited_and_follow_full_access() {
        let dir = tempfile::tempdir().unwrap();
        let log = tempfile::tempdir().unwrap();
        let b = FileBridge::start_in(dir.path().to_path_buf()).unwrap();
        b.set_audit_dir(Some(log.path().to_path_buf()));
        let mut s = sample();
        let reserved = "model.new {\"name\": \"audited\"}";
        write_command(&b, reserved);
        let o = run_next(&b, &mut s);
        assert!(!o.success && o.error.as_deref().is_some_and(|e| e.contains("reserved")), "{o:?}");
        b.set_full_access(true);
        write_command(&b, &format!("{reserved}\n"));
        assert!(run_next(&b, &mut s).success);
        let files: Vec<_> = std::fs::read_dir(log.path()).unwrap().flatten().collect();
        let text = std::fs::read_to_string(files[0].path()).unwrap();
        let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines.iter().all(|l| l["source"] == "file" && l["tool"] == "run"), "{text}");
        assert_eq!((lines[0]["success"].clone(), lines[1]["success"].clone()), (json!(false), json!(true)));
    }

    /// A script's steps are the bridge's in the undo history: it undoes only its own, never the
    /// user's; and the layer a script selects is the bridge's, not the user's selection.
    #[test]
    fn scripts_are_named_in_the_undo_history_and_keep_their_own_layer() {
        let dir = tempfile::tempdir().unwrap();
        let b = FileBridge::start_in(dir.path().to_path_buf()).unwrap();
        let mut s = sample();
        let id = |s: &Session, name: &str| s.map().layer_by_name(name).map(|l| l.id);
        s.run("layer.rename", &json!({"layer": "Rivers", "name": "Streams"})).unwrap();
        write_command(&b, "edit.undo");
        let o = run_next(&b, &mut s);
        assert!(!o.success && o.error.as_deref().is_some_and(|e| e.contains("the user's")), "{o:?}");
        assert!(s.map().layer_by_name("Streams").is_some(), "the user's step stays");
        // Its own layer: selecting one leaves the user's selection, and later scripts use it.
        s.active_layer = id(&s, "Countries");
        write_command(&b, "layer.select {\"layer\": \"Lakes\"}");
        assert!(run_next(&b, &mut s).success);
        assert_eq!(s.active_layer, id(&s, "Countries"), "the user's selection stays");
        write_command(&b, "layer.rename {\"name\": \"Ponds\"}");
        assert!(run_next(&b, &mut s).success);
        assert!(s.map().layer_by_name("Ponds").is_some() && s.map().layer_by_name("Countries").is_some());
        assert_eq!(s.undo_actor(), Some(Some(BRIDGE_ACTOR)));
        assert_eq!(s.actor, None);
        // Its own last step, it may undo.
        write_command(&b, "edit.undo\n");
        assert!(run_next(&b, &mut s).success);
        assert!(s.map().layer_by_name("Lakes").is_some());
    }

    /// Dropping the last handle stops the bridge too.
    #[test]
    fn dropping_the_bridge_removes_its_marker() {
        let dir = tempfile::tempdir().unwrap();
        let b = FileBridge::start_in(dir.path().to_path_buf()).unwrap();
        let again = b.clone();
        drop(b);
        assert!(dir.path().join(READY).exists());
        drop(again);
        assert!(!dir.path().join(READY).exists());
    }
}
