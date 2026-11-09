//! What the providers that run another program (GDAL's `ogr2ogr`, DuckDB) share: finding the
//! program, running it with a time limit, and the cache folder their conversions are kept in.

use crate::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A program: in the folder `env` names, else on the `PATH`.
pub(crate) fn program(env: &str, name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let dirs: Vec<PathBuf> = std::env::var_os(env)
        .map(PathBuf::from)
        .into_iter()
        .chain(std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default())
        .collect();
    // Only absolute folders: an empty or relative PATH entry would look in the current folder.
    dirs.into_iter().filter(|d| d.is_absolute()).map(|d| d.join(&exe)).find(|p| p.is_file())
}

/// A folder in the app's cache folder (or the one `env` names, used as is).
pub(crate) fn cache_dir(env: &str, sub: &str) -> Option<PathBuf> {
    if let Some(d) = std::env::var_os(env).filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    crate::dirs::cache_dir().map(|b| b.join(sub))
}

const RELOAD_PREFIX: &str = "reload-";
const RELOAD_OWNER_MARKER: &[u8] = b"RustGIS reload workspace v1\n";

/// A private conversion directory and its live-owner lock. Close the lock before deleting the
/// directory (including on Windows); concrete readers retain this owner until their last read.
#[derive(Debug)]
pub struct ReloadWorkspace {
    _lock: std::fs::File,
    dir: tempfile::TempDir,
}

impl ReloadWorkspace {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Whether a saved conversion belongs to a disposable workspace rather than a durable cache.
    pub fn contains_file(path: &Path) -> bool {
        path.parent().and_then(Path::file_name).is_some_and(|name| name.to_string_lossy().starts_with(RELOAD_PREFIX))
    }
}

/// Reclaims a bounded number of crash-abandoned conversions before allocating a new workspace.
pub(crate) fn reload_workspace(cache: &Path) -> std::io::Result<std::sync::Arc<ReloadWorkspace>> {
    use std::io::{Read, Write};

    std::fs::create_dir_all(cache)?;
    if let Ok(entries) = std::fs::read_dir(cache) {
        for entry in entries.take(4096).flatten() {
            if !entry.file_name().to_string_lossy().starts_with(RELOAD_PREFIX) || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let path = entry.path();
            let age = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
            // The creator installs owner.lock before locking it. A collector must not claim
            // that unlocked file during installation; an unknown age is not safe to reclaim.
            if !age.is_some_and(|age| age >= std::time::Duration::from_secs(60)) {
                continue;
            }
            let lock_path = path.join("owner.lock");
            let mut lock = match std::fs::File::options().read(true).write(true).open(&lock_path) {
                Ok(lock) => lock,
                Err(_) => continue,
            };
            // A matching name and age alone do not establish ownership in a shared cache.
            // Read through the same handle we will lock, before Windows locks prevent reads.
            let mut marker = [0; RELOAD_OWNER_MARKER.len()];
            if lock.read_exact(&mut marker).is_err() || marker.as_slice() != RELOAD_OWNER_MARKER {
                continue;
            }
            if lock.try_lock().is_ok() {
                // Windows cannot remove an open lock file. No writer claims an existing
                // workspace; once unlocked, only other collectors can race its removal.
                drop(lock);
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }
    let dir = tempfile::Builder::new().prefix(RELOAD_PREFIX).tempdir_in(cache)?;
    let mut lock = std::fs::File::options().read(true).write(true).create_new(true).open(dir.path().join("owner.lock"))?;
    lock.try_lock().map_err(std::io::Error::other)?;
    lock.write_all(RELOAD_OWNER_MARKER)?;
    writeln!(lock, "{}", std::process::id())?;
    Ok(std::sync::Arc::new(ReloadWorkspace { _lock: lock, dir }))
}

/// Runs a program, reading its output as it comes (so a chatty one can't block on a full pipe) and
/// killing it if it runs past `timeout`. `what` names it in the timeout error.
pub(crate) fn output(c: Command, timeout: std::time::Duration, what: &str) -> std::io::Result<std::process::Output> {
    output_with_budget(c, timeout, what, None)
}

/// Like [`output`], optionally monitoring the total file size in a private workspace. Scratch
/// files stay there too. This is checked every 50 ms, not an OS quota: writes can overshoot
/// between checks. On a limit, timeout or monitoring error the child is killed and reaped.
/// Error paths deliberately do not join the reader threads: a surviving grandchild can inherit
/// their pipe handles, and waiting for EOF would defeat the timeout. Such readers remain detached
/// until the inherited handles close. Normal completion still joins both readers.
pub(crate) fn output_with_budget(
    mut c: Command,
    timeout: std::time::Duration,
    what: &str,
    workspace: Option<(&Path, u64)>,
) -> std::io::Result<std::process::Output> {
    if let Some((dir, max)) = workspace {
        check_workspace(dir, max, what)?;
        for key in ["CPL_TMPDIR", "TMPDIR", "TMP", "TEMP"] {
            c.env(key, dir);
        }
    }
    use std::io::Read;
    use std::process::Stdio;
    let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let drain = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = std::time::Instant::now();
    let status = loop {
        if let Some((dir, max)) = workspace
            && let Err(e) = check_workspace(dir, max, what)
        {
            break Err(e);
        }
        match child.try_wait() {
            Ok(Some(st)) => {
                // The child can finish writing between the scan and try_wait.
                break workspace.map_or(Ok(st), |(dir, max)| check_workspace(dir, max, what).map(|()| st));
            }
            Ok(None) => {}
            Err(e) => break Err(e),
        }
        if start.elapsed() > timeout {
            break Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("{what} ran for more than {} minutes", timeout.as_secs() / 60),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let status = match status {
        Ok(status) => status,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            // Do not wait for EOF on error: killing the direct child need not close inherited
            // pipe handles in its descendants. Preserve a prompt return from the timeout/limit.
            return Err(e);
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    Ok(std::process::Output { status, stdout, stderr })
}

/// Counts files without following symlinks, including nested GDAL scratch directories. Bound
/// traversal too, so a malformed workspace can't keep the process monitor busy indefinitely.
fn check_workspace(dir: &Path, max: u64, what: &str) -> std::io::Result<()> {
    fn scan(dir: &Path, depth: usize, left: &mut usize, size: &mut u64, max: u64) -> std::io::Result<bool> {
        if depth > 16 {
            return Err(std::io::Error::other("temporary workspace is nested too deeply"));
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && depth > 0 => return Ok(false),
            Err(e) => return Err(e),
        };
        for entry in entries {
            if *left == 0 {
                return Err(std::io::Error::other("too many files in temporary workspace"));
            }
            *left -= 1;
            let entry = entry?;
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if meta.is_file() {
                *size = size.saturating_add(meta.len());
                if *size > max {
                    return Ok(true);
                }
            } else if meta.is_dir() && scan(&entry.path(), depth + 1, left, size, max)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    if scan(dir, 0, &mut 4096, &mut 0, max)? {
        return Err(std::io::Error::other(format!("{what}: temporary workspace exceeds its {max}-byte limit; conversion stopped")));
    }
    Ok(())
}

/// A file's size and modification time; for a folder (a File Geodatabase), those of its files,
/// so an edit inside it is noticed.
pub(crate) fn stamp(p: &Path) -> Result<(u64, u128)> {
    let m = std::fs::metadata(p)?;
    let t = |m: &std::fs::Metadata| {
        m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).unwrap_or(0)
    };
    if !m.is_dir() {
        return Ok((m.len(), t(&m)));
    }
    let (mut len, mut newest) = (0u64, t(&m));
    for e in std::fs::read_dir(p)?.flatten().take(100_000) {
        if let Ok(em) = e.metadata() {
            len = len.saturating_add(em.len());
            newest = newest.max(t(&em));
        }
    }
    Ok((len, newest))
}

/// A 64-bit FNV-1a hash: stable across Rust releases, unlike `DefaultHasher`.
pub(crate) fn fnv1a(b: &[u8]) -> u64 {
    b.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &x| (h ^ u64::from(x)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// Marks a cache file as used now, so pruning removes what isn't used rather than what is old.
pub(crate) fn touch(p: &Path) {
    if let Ok(f) = std::fs::File::options().write(true).open(p) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
}

/// A file removed when dropped (unless taken out first).
pub(crate) struct TempFile(pub(crate) Option<PathBuf>);

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Removes the files in `dir` that `ours` claims and that weren't used for `keep` (best effort).
/// Nothing else the folder holds is touched.
pub(crate) fn prune(dir: &Path, ours: fn(&str) -> bool, keep: std::time::Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = std::time::SystemTime::now();
    for e in entries.flatten().take(100_000) {
        if !e.file_name().to_str().is_some_and(ours) || !e.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let old = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age > keep);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn age_directory(path: &Path, seconds: u64) {
        let mut options = std::fs::File::options();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.access_mode(0x0100).custom_flags(0x0200_0000); // FILE_WRITE_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS.
        }
        options
            .open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(seconds)))
            .unwrap();
    }

    #[test]
    fn reload_collection_preserves_a_new_unlocked_owner_lock_then_reclaims_it_after_grace() {
        let cache = tempfile::tempdir().unwrap();
        let abandoned = cache.path().join("reload-installing");
        std::fs::create_dir(&abandoned).unwrap();
        std::fs::write(abandoned.join("owner.lock"), RELOAD_OWNER_MARKER).unwrap();
        std::fs::write(abandoned.join("converted.fgb"), b"saved conversion").unwrap();
        let fresh = reload_workspace(cache.path()).unwrap();
        assert!(abandoned.exists(), "the creator has 60 seconds to lock its newly installed lock file");
        age_directory(&abandoned, 61);
        let next = reload_workspace(cache.path()).unwrap();
        assert!(!abandoned.exists(), "an old unlocked workspace is crash-abandoned");
        assert!(fresh.path().exists() && next.path().exists(), "live owners remain untouched");
    }

    #[test]
    fn reload_collection_keeps_old_unlocked_foreign_owner_locks_intact() {
        let cache = tempfile::tempdir().unwrap();
        let foreign = cache.path().join("reload-foreign");
        std::fs::create_dir(&foreign).unwrap();
        let lock_path = foreign.join("owner.lock");
        std::fs::write(&lock_path, b"another application's workspace\n").unwrap();
        let contents = foreign.join("not-rustgis.txt");
        std::fs::write(&contents, b"unrelated data").unwrap();
        age_directory(&foreign, 3601);

        let live = reload_workspace(cache.path()).unwrap();

        assert_eq!(std::fs::read(contents).unwrap(), b"unrelated data");
        assert_eq!(std::fs::read(lock_path).unwrap(), b"another application's workspace\n");
        // Read through the owner's own handle: on Windows its lock blocks reads through any other.
        let mut marker = Vec::new();
        let mut lock = &live._lock;
        std::io::Seek::seek(&mut lock, std::io::SeekFrom::Start(0)).unwrap();
        std::io::Read::read_to_end(&mut lock, &mut marker).unwrap();
        assert!(marker.starts_with(RELOAD_OWNER_MARKER));
        assert_eq!(&marker[RELOAD_OWNER_MARKER.len()..], format!("{}\n", std::process::id()).as_bytes());
    }

    #[test]
    fn reload_collection_keeps_old_lockless_directories_intact_and_locked_workspaces() {
        let cache = tempfile::tempdir().unwrap();
        let missing = cache.path().join("reload-no-lock");
        std::fs::create_dir(&missing).unwrap();
        let contents = missing.join("not-rustgis.txt");
        std::fs::write(&contents, b"unrelated data").unwrap();
        age_directory(&missing, 61);
        let live = reload_workspace(cache.path()).unwrap();
        assert_eq!(std::fs::read(&contents).unwrap(), b"unrelated data");
        assert!(!missing.join("owner.lock").exists(), "a collector must not claim an unowned directory");
        age_directory(live.path(), 3601);
        age_directory(&missing, 3601);
        let _next = reload_workspace(cache.path()).unwrap();
        assert_eq!(std::fs::read(&contents).unwrap(), b"unrelated data", "age does not establish RustGIS ownership");
        assert!(!missing.join("owner.lock").exists(), "old lockless directories must remain untouched");
        assert!(live.path().exists(), "even an old workspace belongs to its locked owner");
    }

    #[test]
    fn workspace_budget_counts_source_output_and_nested_scratch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("source.jp2"), [0; 64]).unwrap();
        std::fs::write(dir.path().join("output.tif"), [0; 32]).unwrap();
        let scratch = dir.path().join("scratch");
        std::fs::create_dir(&scratch).unwrap();
        let overview = scratch.join("overview.tmp");
        std::fs::write(&overview, [0; 32]).unwrap();
        assert!(check_workspace(dir.path(), 128, "GDAL").is_ok(), "the exact limit is allowed");
        std::fs::write(&overview, [0; 33]).unwrap();
        assert!(check_workspace(dir.path(), 128, "GDAL").is_err(), "scratch counts towards the aggregate limit");
    }

    #[test]
    fn workspace_budget_stops_child_and_cleans_up() {
        const WORKER: &str = "RUSTGIS_WORKSPACE_BUDGET_WORKER";
        if let Some(root) = std::env::var_os(WORKER) {
            let root = PathBuf::from(root);
            let work = root.join("work");
            let alive = std::fs::File::create(root.join("alive.lock")).unwrap();
            alive.lock().unwrap();
            for key in ["CPL_TMPDIR", "TMPDIR", "TMP", "TEMP"] {
                assert_eq!(std::env::var_os(key).as_deref(), Some(work.as_os_str()));
            }
            let scratch = work.join("scratch");
            std::fs::create_dir(&scratch).unwrap();
            std::fs::write(scratch.join("overview.tmp"), [0; 129]).unwrap();
            // Hold the lock until normal completion; killing/reaping must release it.
            std::thread::sleep(std::time::Duration::from_secs(3));
            std::fs::write(root.join("finished"), b"conversion was allowed to finish").unwrap();
            drop(alive);
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("source.jp2"), [0; 64]).unwrap();
        std::fs::write(work.join("output.tmp.tif"), [0; 64]).unwrap();
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args(["--exact", "source::external::tests::workspace_budget_stops_child_and_cleans_up", "--nocapture"]).env(WORKER, dir.path());
        let e = output_with_budget(c, std::time::Duration::from_secs(10), "GDAL", Some((&work, 256))).unwrap_err();
        assert!(e.to_string().contains("temporary workspace exceeds its 256-byte limit"), "{e}");
        assert!(!dir.path().join("finished").exists(), "oversized conversion was stopped before finishing");
        let alive = std::fs::File::open(dir.path().join("alive.lock")).unwrap();
        assert!(alive.try_lock().is_ok(), "the oversized writer must no longer hold its file lock");
        drop(alive);
        let path = dir.path().to_path_buf();
        drop(dir);
        assert!(!path.exists(), "the source, output and scratch are removed after stopping the child");
    }

    #[cfg(unix)]
    #[test]
    fn timeout_returns_while_a_grandchild_holds_the_pipes() {
        use std::time::{Duration, Instant};
        for budgeted in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let ready = dir.path().join("ready");
            let release = dir.path().join("release");
            let finished = dir.path().join("finished");
            let mut c = Command::new("/bin/sh");
            // The grandchild retains both pipes until released. Its fallback bounds its
            // lifetime even if the test fails before releasing it.
            c.args(["-c", "(echo ready > \"$1\"; i=0; while [ ! -e \"$2\" ] && [ \"$i\" -lt 500 ]; do sleep 0.01; i=$((i+1)); done; echo done > \"$3\") & wait", "pipe-holder"])
                .arg(&ready).arg(&release).arg(&finished);
            let work = dir.path().to_path_buf();
            let (tx, rx) = std::sync::mpsc::channel();
            let runner = std::thread::spawn(move || {
                let workspace = budgeted.then_some((work.as_path(), 1024));
                let result = output_with_budget(c, Duration::from_millis(250), "pipe-holder", workspace);
                let _ = tx.send(result);
            });
            let start = Instant::now();
            while !ready.exists() && start.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let result = rx.recv_timeout(Duration::from_secs(5));
            let held_pipes = ready.exists() && !finished.exists();
            // Release before asserting, so the failing-before case also cleans up.
            std::fs::write(&release, b"release inherited pipes").unwrap();
            runner.join().unwrap();
            let start = Instant::now();
            while !finished.exists() && start.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(finished.exists(), "the pipe-holder must finish after release");
            assert!(held_pipes, "the grandchild must retain its pipes until explicitly released");
            let e = result.expect("timeout must return without waiting for the grandchild").unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::TimedOut, "{e}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_budget_does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("unrelated"), [0; 129]).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert!(check_workspace(dir.path(), 0, "GDAL").is_ok());
    }
}
