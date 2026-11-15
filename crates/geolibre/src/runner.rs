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

/// The geolibre-rust release that one-click install fetches and the download link points to. It
/// is the release the browser's runner is pinned to (`geolibre-wasm` in
/// `apps/rustgis-web/src/geolibre.rs`): move both forward together, with the user guide's link
/// (`docs/user-guide/analysis.md`), by a pull request, after checking the new runner against
/// [`SUPPORTED_FROM`]..[`SUPPORTED_BEFORE`].
pub const RELEASE: &str = "1.8.0";

/// The oldest runner version RustGIS works with: the first 1.x release, which already answers
/// `version`, `manifests` and `<tool> --k=v` as RustGIS asks them.
pub const SUPPORTED_FROM: (u64, u64, u64) = (1, 0, 0);

/// The first runner version RustGIS doesn't work with: the next major release, which may change
/// how the runner is asked for its tools or runs them. A runner from there on is never installed
/// and, when found, passed over with a note saying why ([`passed_over`]).
pub const SUPPORTED_BEFORE: (u64, u64, u64) = (2, 0, 0);

/// Where the pinned release's downloads are published.
pub fn download_url() -> String {
    format!("https://github.com/opengeos/geolibre-rust/releases/tag/v{RELEASE}")
}

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

/// Why the last runner passed over (found, or chosen with Locate…) wasn't used: its version is
/// outside the supported range. Kept while another runner is in use; cleared only when a choice
/// is made (Locate… of a supported runner, or looking again).
static PASSED_OVER: Mutex<Option<String>> = Mutex::new(None);

/// Why a GeoLibre runner that was found or chosen isn't in use, when its version is outside the
/// supported range: what was found and how to fix it, also while another runner is in use.
/// `None` when nothing was passed over.
pub fn passed_over() -> Option<String> {
    PASSED_OVER.lock().ok().and_then(|p| p.clone())
}

/// [`check_supported`]'s reason with how to fix it, for a runner found or chosen.
fn unsupported_with_fix(what: &str, version: &str) -> Result<(), String> {
    check_supported(what, version).map_err(|e| {
        format!("{e} Install the GeoLibre tools from the Tools pane (or `gp.installGeolibre`), which fetches runner {RELEASE}, or locate a runner in that range.")
    })
}

/// Records discovery's reason for passing a runner over, if it has one and no reason is kept
/// yet: a Locate… choice refused (say, the remembered one at startup) keeps its reason when
/// discovery then passes over another runner or finds a supported one. Only a choice replaces or
/// clears it ([`choose`]).
fn add_passed_over(note: Option<String>) {
    if let Ok(mut p) = PASSED_OVER.lock()
        && p.is_none()
    {
        *p = note;
    }
}

/// Records (or, with `None`, clears) the reason [`passed_over`] gives.
fn set_passed_over(note: Option<String>) {
    if let Ok(mut p) = PASSED_OVER.lock() {
        *p = note;
    }
}

/// The `major.minor.patch` a runner reports (`geolibre-cli 1.8.0`, maybe `v1.8.0` or with a
/// `-pre`/`+build` suffix), or `None` when it can't be read.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let word = text.strip_prefix("geolibre-cli")?.split_whitespace().next()?;
    let core = word.trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

/// Checks that a runner (`what`: its path, or which download), which reported `version` (what
/// [`version_of`] gives), is one RustGIS works with; the error says what was found and the range.
pub fn check_supported(what: &str, version: &str) -> Result<(), String> {
    let range = |(a, b, c): (u64, u64, u64)| format!("{a}.{b}.{c}");
    let (from, before) = (range(SUPPORTED_FROM), range(SUPPORTED_BEFORE));
    match parse_version(version) {
        Some(v) if v >= SUPPORTED_FROM && v < SUPPORTED_BEFORE => Ok(()),
        Some(_) => Err(format!(
            "{what} is {version}, which this RustGIS can't use: it works with GeoLibre runner {from} up to, but not including, {before}."
        )),
        None => Err(format!(
            "{what} reports its version as \"{version}\", which this RustGIS can't read: it works with GeoLibre runner {from} up to, but not including, {before}."
        )),
    }
}

/// The already discovered choice, without probing programs or searching the filesystem.
/// None means unavailable or not yet explicitly discovered by trusted native code.
pub fn selected() -> Option<(PathBuf, String)> {
    FOUND.lock().ok().and_then(|f| f.value.clone().flatten())
}

/// A command for the runner, without a console window on Windows.
fn command(exe: &Path) -> Command {
    #[cfg_attr(not(windows), expect(unused_mut, reason = "only Windows sets creation flags"))]
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

/// Starts `c`, waiting out a program file that is still held open for writing ("text file
/// busy"): a runner just written (installed, or replaced) can be held a moment longer by another
/// process started at the same time, which inherits the open file until it runs its own program.
/// Tried again for up to a second, unless `stop` gives a reason first (looked at before each
/// try): then nothing is started and the reason is the error. Any other failure, or a file still
/// busy after the second, is the error.
fn spawn_when_free(c: &mut Command, stop: &dyn Fn() -> Option<String>) -> Result<std::process::Child, String> {
    let start = Instant::now();
    loop {
        if let Some(why) = stop() {
            return Err(why);
        }
        match c.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && start.elapsed() < Duration::from_secs(1) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(e.to_string()),
            Ok(child) => return Ok(child),
        }
    }
}

/// Runs `c` to completion, unless `stop` gives a reason first (looked at every 20 ms): then the
/// process is killed and the reason is the error. Both pipes are drained while it runs.
fn output_until(mut c: Command, stop: &dyn Fn() -> Option<String>) -> Result<Output, String> {
    c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = spawn_when_free(&mut c, stop)?;
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
    let (probed, note) = first_supported(candidates());
    let mut found = FOUND.lock().ok()?;
    if found.generation != generation {
        return found.value.clone().flatten();
    }
    if found.value.is_none() {
        add_passed_over(note);
    }
    found.value.get_or_insert(probed).clone()
}

/// The first of `candidates` that is a runner of a supported version, and why the first runner
/// before it was passed over (outside the supported range), if one was.
fn first_supported(candidates: Vec<PathBuf>) -> (Option<(PathBuf, String)>, Option<String>) {
    let mut note = None;
    for p in candidates {
        let Some(v) = version_of(&p) else { continue };
        match unsupported_with_fix(&p.display().to_string(), &v) {
            Ok(()) => return (Some((p, v)), note),
            Err(e) => {
                log::warn!("GeoLibre runner passed over: {e}");
                note.get_or_insert(e);
            }
        }
    }
    (None, note)
}

/// Uses `exe` as the runner (`None`: look in the usual places again). Fails, keeping the
/// current choice, when `exe` isn't a GeoLibre runner or its version is outside the supported
/// range (that reason is also kept for [`passed_over`]); a choice made clears an earlier reason.
pub fn choose(exe: Option<PathBuf>) -> Result<Option<(PathBuf, String)>, String> {
    if let Some(p) = &exe {
        let version = version_of(p).ok_or_else(|| format!("{} is not a GeoLibre runner", p.display()))?;
        if let Err(e) = unsupported_with_fix(&p.display().to_string(), &version) {
            set_passed_over(Some(e.clone()));
            return Err(e);
        }
    }
    if let Ok(mut c) = CHOSEN.lock() {
        *c = exe;
    }
    if let Ok(mut f) = FOUND.lock() {
        f.generation = f.generation.saturating_add(1);
        f.value = None;
        // Cleared under the lock `find` records its note with, so a discovery that started
        // before this choice can't record a stale note after it.
        set_passed_over(None);
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

    /// Discovery passes over a runner outside the supported range for the next supported one,
    /// and says why; with only an unsupported one, there is no runner but the reason is given.
    #[test]
    fn discovery_passes_over_an_unsupported_runner_and_says_why() {
        let dir = std::env::temp_dir().join(format!("rustgis-geolibre-find-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = |name: &str, line: &str| {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n[ \"$1\" = version ] && echo '{line}' && exit 0\nexit 1\n")).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let (newer, works, other) =
            (fake("newer", "geolibre-cli 2.0.0"), fake("works", "geolibre-cli 1.8.0"), fake("other", "other 1.0.0"));
        let (found, note) = first_supported(vec![other.clone(), newer.clone(), works.clone()]);
        assert_eq!(found, Some((works.clone(), "geolibre-cli 1.8.0".to_string())));
        let note = note.unwrap();
        assert!(note.contains(&newer.display().to_string()) && note.contains("2.0.0") && note.contains("gp.installGeolibre"), "{note}");
        assert_eq!(first_supported(vec![other.clone(), works.clone()]), (Some((works.clone(), "geolibre-cli 1.8.0".to_string())), None));
        let (found, note) = first_supported(vec![newer.clone()]);
        assert!(found.is_none() && note.is_some());
        // Locate… refuses it too, keeping the current choice, and its reason stays reported after
        // discovery, both when that finds a runner to use and when it passes over another one.
        let before = selected();
        assert!(choose(Some(newer.clone())).is_err_and(|e| e.contains("2.0.0") && e.contains("locate a runner in that range")));
        assert_eq!(selected(), before);
        let older = fake("older", "geolibre-cli 0.11.0");
        add_passed_over(first_supported(vec![works.clone()]).1);
        add_passed_over(first_supported(vec![older.clone(), works.clone()]).1);
        let kept = passed_over().unwrap_or_default();
        assert!(kept.contains(&newer.display().to_string()) && !kept.contains(&older.display().to_string()), "{kept}");
        let _ = std::fs::remove_dir_all(&dir);
    }

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

#[cfg(test)]
mod portable_tests {
    use super::*;

    /// A stopped run is refused before attempting even an invalid executable. This also keeps
    /// every busy-file retry behind the same cancellation check.
    #[test]
    fn stopped_run_never_attempts_to_spawn() {
        let mut command = Command::new("");
        let error = spawn_when_free(&mut command, &|| Some("cancelled before spawn".to_string())).unwrap_err();
        assert_eq!(error, "cancelled before spawn");
    }

    /// Runner versions 1.x are used; anything else, or a version that can't be read, isn't, and
    /// the reason names the runner, its version and the supported range.
    #[test]
    fn supported_runner_versions() {
        for ok in ["geolibre-cli 1.0.0", "geolibre-cli 1.8.0", "geolibre-cli v1.8.2", "geolibre-cli 1.9.0-dev", "geolibre-cli 1.99.3+abc"] {
            assert_eq!(check_supported("r", ok), Ok(()), "{ok}");
        }
        for bad in ["geolibre-cli 2.0.0", "geolibre-cli 2.0.0-rc.1", "geolibre-cli 0.11.0"] {
            let e = check_supported("/opt/geolibre", bad).unwrap_err();
            assert!(e.contains("/opt/geolibre") && e.contains(bad) && e.contains("1.0.0") && e.contains("2.0.0"), "{e}");
        }
        for unreadable in ["geolibre-cli", "geolibre-cli 1.8", "geolibre-cli 1.8.0.1", "geolibre-cli one"] {
            assert!(check_supported("r", unreadable).unwrap_err().contains("can't read"), "{unreadable}");
        }
        assert!(download_url().ends_with(&format!("/releases/tag/v{RELEASE}")));
        let pinned = parse_version(&format!("geolibre-cli {RELEASE}")).unwrap();
        assert!(pinned >= SUPPORTED_FROM && pinned < SUPPORTED_BEFORE, "the pinned release is supported");
    }
}
