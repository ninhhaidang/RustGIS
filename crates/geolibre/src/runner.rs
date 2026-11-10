//! The `geolibre` runner: a separate executable (an optional download) that runs the tools.
//!
//! RustGIS doesn't link the suite (it would triple the app's size). It runs the runner as a
//! subprocess instead: `geolibre manifests` for the tool forms, `geolibre <tool> --k=v …` for a
//! run. A tool that crashes or panics takes only the subprocess down.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

/// Where releases with the runner are published.
pub const DOWNLOAD_URL: &str = "https://github.com/opengeos/geolibre-rust/releases/latest";

/// The environment variable that names the runner, ahead of every other place.
pub const ENV: &str = "RUSTGIS_GEOLIBRE";

pub(crate) fn exe_name() -> &'static str {
    if cfg!(windows) { "geolibre.exe" } else { "geolibre" }
}

/// A runner the user chose (Locate…), ahead of the usual places.
static CHOSEN: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The runner found, with its version, cached until the choice changes.
struct Found {
    generation: u64,
    value: Option<Option<(PathBuf, String)>>,
}
static FOUND: Mutex<Found> = Mutex::new(Found { generation: 0, value: None });

/// The already discovered choice, without probing programs or searching the filesystem.
/// None means unavailable or not yet explicitly discovered by trusted native code.
pub fn selected() -> Option<(PathBuf, String)> {
    FOUND.lock().ok().and_then(|f| f.value.clone().flatten())
}

/// A command for the runner, without a console window on Windows.
fn command(exe: &Path) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Runs `c` to completion, or kills it after `limit`. For the quick queries (version,
/// manifests): a program that only happens to be called `geolibre` mustn't hang the app. Tool
/// runs have no limit, since a large raster can legitimately take a long time.
fn output_within(c: Command, limit: Duration) -> Option<Output> {
    let start = Instant::now();
    output_until(c, &|| (start.elapsed() >= limit).then(|| "timed out".to_string())).ok()
}

/// Runs `c` to completion, unless `stop` gives a reason first (looked at every 20 ms): then the
/// process is killed and the reason is the error. Both pipes are drained while it runs.
fn output_until(mut c: Command, stop: &dyn Fn() -> Option<String>) -> Result<Output, String> {
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
    // Drain both pipes on threads while the process runs: a full pipe (large manifests, or
    // many warnings) would otherwise block it until the time limit.
    fn drain(mut pipe: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    }
    let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
        // Not expected with piped output, but never leave the process running.
        let _ = child.kill();
        let _ = child.wait();
        return Err("the program's output couldn't be read".into());
    };
    let (out_reader, err_reader) = (drain(out), drain(err));
    let status = loop {
        let why = match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => stop(),
            Err(e) => Some(e.to_string()),
        };
        if let Some(why) = why {
            let _ = child.kill();
            let _ = child.wait();
            return Err(why);
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out_reader.join().map_err(|_| "the program's output couldn't be read".to_string())?;
    let stderr = err_reader.join().unwrap_or_default();
    Ok(Output { status, stdout, stderr })
}

/// The runner's version, if `exe` is a GeoLibre runner (not some other program called geolibre).
pub fn version_of(exe: &Path) -> Option<String> {
    if !exe.is_file() {
        return None;
    }
    let mut c = command(exe);
    c.arg("version");
    let out = output_within(c, Duration::from_secs(10))?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && text.starts_with("geolibre-cli")).then_some(text)
}

/// The places a runner is looked for, in order.
fn candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(p) = CHOSEN.lock()
        && let Some(p) = p.clone()
    {
        v.push(p);
    }
    if let Some(p) = std::env::var_os(ENV) {
        v.push(PathBuf::from(p));
    }
    // Where one-click install puts it.
    if let Some(dir) = crate::install::install_dir() {
        v.push(dir.join(exe_name()));
    }
    // Beside the app (a bundle that ships it), or in a `geolibre` folder there.
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        v.push(dir.join(exe_name()));
        v.push(dir.join("geolibre").join(exe_name()));
    }
    if let Some(path) = std::env::var_os("PATH") {
        // Absolute entries only: an empty or relative one would mean the current folder.
        v.extend(std::env::split_paths(&path).filter(|d| d.is_absolute()).map(|d| d.join(exe_name())));
    }
    v
}

/// The runner to use and its version, or `None` when there isn't one.
///
/// The answer is kept until the choice changes (Locate…). Probing runs without holding the
/// cache's lock, so a slow candidate never blocks another caller.
pub fn find() -> Option<(PathBuf, String)> {
    let generation = {
        let found = FOUND.lock().ok()?;
        if let Some(value) = &found.value {
            return value.clone();
        }
        found.generation
    };
    let probed = candidates().into_iter().find_map(|p| version_of(&p).map(|v| (p, v)));
    let mut found = FOUND.lock().ok()?;
    if found.generation != generation {
        return found.value.clone().flatten();
    }
    found.value.get_or_insert(probed).clone()
}

/// Uses `exe` as the runner (`None`: look in the usual places again). Fails, keeping the
/// current choice, when `exe` isn't a GeoLibre runner.
pub fn choose(exe: Option<PathBuf>) -> Result<Option<(PathBuf, String)>, String> {
    if let Some(p) = &exe
        && version_of(p).is_none()
    {
        return Err(format!("{} is not a GeoLibre runner", p.display()));
    }
    if let Ok(mut c) = CHOSEN.lock() {
        *c = exe;
    }
    if let Ok(mut f) = FOUND.lock() {
        f.generation = f.generation.saturating_add(1);
        f.value = None;
    }
    Ok(find())
}

/// Every tool's manifest, from `geolibre manifests`.
pub fn manifests() -> Result<Vec<Value>, String> {
    let (exe, _) = find().ok_or("the GeoLibre runner isn't installed")?;
    let mut c = command(&exe);
    c.arg("manifests");
    let out = output_within(c, Duration::from_secs(60)).ok_or_else(|| format!("{}: no answer to `manifests`", exe.display()))?;
    if !out.status.success() {
        return Err(format!("{}: {}", exe.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    match serde_json::from_slice::<Value>(&out.stdout) {
        Ok(Value::Array(v)) => Ok(v),
        _ => Err(format!("{}: unexpected manifests output", exe.display())),
    }
}

/// A chosen native runner. Captures path/version, not an attestation of executable bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedRunner {
    exe: PathBuf,
    version: String,
}
impl ResolvedRunner {
    /// Captures the selected executable path and reported version without permitting mutation.
    pub fn capture() -> Result<Self, String> {
        let (exe, version) = selected().ok_or("the GeoLibre runner is not published; open the tools or locate the runner first")?;
        Ok(Self { exe, version })
    }
    /// Rejects a changed runner choice before preparing or starting another stage.
    pub fn check(&self) -> Result<(), String> {
        if selected().as_ref() != Some(&(self.exe.clone(), self.version.clone())) {
            Err("the GeoLibre runner changed before this stage started".into())
        } else {
            Ok(())
        }
    }
}

/// Runs one tool in `dir` with `argv` (`--name=value` strings); returns the lines it printed.
/// The error carries what the tool wrote to stderr.
pub fn run(id: &str, argv: &[String], dir: &Path) -> Result<Vec<String>, String> {
    let _ = find(); // Explicit legacy native execution may discover; pinned policy execution never does.
    run_pinned(&ResolvedRunner::capture()?, id, argv, dir, &|| Ok(()))
}

/// Checks the captured choice and caller's live predicate immediately before spawning it.
pub(crate) fn run_pinned(
    pin: &ResolvedRunner,
    id: &str,
    argv: &[String],
    dir: &Path,
    live: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<String>, String> {
    pin.check()?;
    let exe = &pin.exe;
    let mut c = command(exe);
    c.current_dir(dir).arg(id).args(argv);
    live()?;
    pin.check()?;
    let stop = || stop_reason(live);
    let out = output_until(c, &stop).map_err(|e| if e == "cancelled" || e == CLOSED { e } else { format!("{}: {e}", exe.display()) })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        let err = if err.is_empty() { "the tool stopped unexpectedly" } else { err };
        return Err(err.lines().take(20).collect::<Vec<_>>().join("\n"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect())
}

/// Why a program started for a project is stopped once another is open.
const CLOSED: &str = "its project was closed while the tool ran";

/// Why a running program is stopped rather than waited for (#1562): its run was cancelled (the
/// Cancel of a model or script run in the background), the project it was started for is no
/// longer the one open (the predicate a background read installs), or `live` fails.
fn stop_reason(live: &dyn Fn() -> Result<(), String>) -> Option<String> {
    if rustgis_analysis::control::current().is_some_and(|c| c.is_cancelled()) {
        return Some("cancelled".to_string());
    }
    if rustgis_data::source::remote::check_still(&rustgis_data::source::remote::current_still()).is_err() {
        return Some(CLOSED.to_string());
    }
    live().err()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A program asked to stop is killed at once, not waited for (#1562): a GeoLibre step of a
    /// model run in the background stops with its Cancel.
    #[test]
    fn a_program_asked_to_stop_is_killed() {
        let mut c = Command::new("sleep");
        c.arg("30");
        let start = Instant::now();
        let e = output_until(c, &|| Some("cancelled".to_string())).unwrap_err();
        assert_eq!(e, "cancelled");
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        // One that ends first gives its output.
        let mut c = Command::new("echo");
        c.arg("done");
        let out = output_until(c, &|| None).unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "done");
    }

    /// A program is stopped once its run is cancelled or its project closed, without a GeoLibre
    /// runner: the path from the run's control and the installed predicate to the kill.
    #[test]
    fn a_cancelled_run_or_a_closed_project_stops_the_program() {
        use rustgis_analysis::control::Control;
        use rustgis_data::source::remote::with_still;
        let fine = || Ok(());
        assert_eq!(stop_reason(&fine), None);
        assert_eq!(stop_reason(&|| Err("gone".to_string())), Some("gone".to_string()));
        let control = Control::new();
        let run = rustgis_analysis::run_under("Tool", &control, || {
            assert_eq!(stop_reason(&fine), None);
            control.cancel();
            let started = std::time::Instant::now();
            let mut c = Command::new("sleep");
            c.arg("30");
            let res = output_until(c, &|| stop_reason(&fine));
            assert!(started.elapsed() < Duration::from_secs(10), "the program was waited for");
            assert_eq!(res.err().as_deref(), Some("cancelled"));
            Ok(())
        });
        assert!(run.is_err(), "the run ends cancelled");
        let closed = with_still(std::sync::Arc::new(|| false), || stop_reason(&fine));
        assert_eq!(closed.as_deref(), Some(CLOSED), "a program started for a project no longer open is stopped, and says so");
        assert_eq!(with_still(std::sync::Arc::new(|| true), || stop_reason(&fine)), None);
    }
}
