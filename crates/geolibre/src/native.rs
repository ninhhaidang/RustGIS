//! Running a prepared [`Job`] natively: stage its files in a fresh scratch folder, run the
//! `geolibre` program there, and read the new files back.

use std::path::{Path, PathBuf};

use rustgis_analysis::ToolResult;

use crate::{Job, finish, runner};

/// A scratch folder for one run, removed afterwards. It holds copies of the user's layers, so on
/// Unix it and the files staged in it are private to the user (`0700`, `0600`) whatever the
/// umask: the system's temporary folder is shared by every user there. Windows keeps `%TEMP%`
/// in the user's profile by default.
struct Scratch(PathBuf);

impl Scratch {
    /// A new, empty folder: never one that already exists (left over, or made by someone else
    /// on a shared machine), so no old file can pass for a tool's output.
    fn new() -> Result<Scratch, String> {
        Scratch::new_in(&std::env::temp_dir())
    }

    /// [`Scratch::new`] in `parent`: a new folder made in one step, which fails rather than use one
    /// already there (the next name is tried).
    fn new_in(parent: &Path) -> Result<Scratch, String> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let mut last = None;
        for _ in 0..16 {
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = parent.join(format!("rustgis-gp-{}-{nanos:x}-{n}", std::process::id()));
            #[cfg_attr(not(unix), expect(unused_mut, reason = "only Unix sets a mode on the builder"))]
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            match builder.create(&dir) {
                Ok(()) => return Ok(Scratch(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
                Err(e) => return Err(format!("can't create a scratch folder: {e}")),
            }
        }
        Err(format!("can't create a scratch folder: {}", last.map(|e| e.to_string()).unwrap_or_default()))
    }

    /// Writes a staged input as a new file, readable only by the user on Unix.
    fn stage(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        use std::io::Write as _;
        let path = self.0.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(&path).and_then(|mut f| f.write_all(bytes)).map_err(|e| format!("can't write {}: {e}", path.display()))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The files in `dir` that aren't staged inputs, by file name.
fn new_files(dir: &Path, job: &Job) -> Vec<(String, Vec<u8>)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if job.files.iter().any(|(f, _)| *f == name) {
                return None;
            }
            std::fs::read(e.path()).ok().map(|b| (name, b))
        })
        .collect()
}

pub(crate) fn run(job: &Job) -> Result<ToolResult, String> {
    run_pinned(job, &runner::ResolvedRunner::capture()?, &|| Ok(()))
}

/// Stages only after the captured runner and live caller are still valid.
pub(crate) fn run_pinned(job: &Job, pin: &runner::ResolvedRunner, live: &dyn Fn() -> Result<(), String>) -> Result<ToolResult, String> {
    pin.check()?;
    live()?;
    let scratch = Scratch::new()?;
    for (name, bytes) in &job.files {
        scratch.stage(name, bytes)?;
    }
    let lines = runner::run_pinned(pin, &job.tool, &job.argv(&scratch.0.display().to_string()), &scratch.0, live)?;
    let files = new_files(&scratch.0, job);
    Ok(finish(job, &files, &lines, Some(&scratch.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of the test's own under the system's temporary folder, removed afterwards.
    struct Parent(PathBuf);

    impl Parent {
        fn new(name: &str) -> Parent {
            let dir = std::env::temp_dir().join(format!("rustgis-gp-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a temporary folder");
            Parent(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Parent {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Other users can't list the scratch folder or read what is staged in it, even where the
    /// umask would leave new files readable by everyone.
    #[cfg(unix)]
    #[test]
    fn scratch_folders_and_staged_inputs_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let parent = Parent::new("private");
        // A shared parent, as /tmp is: what the folder allows is then what others get.
        std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o777)).expect("a shared parent");
        let scratch = Scratch::new_in(parent.path()).expect("a scratch folder");
        scratch.stage("roads.geojson", b"{}").expect("a staged input");
        let mode = |p: &Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(&scratch.0), 0o700, "the scratch folder");
        assert_eq!(mode(&scratch.0.join("roads.geojson")), 0o600, "a staged input");
        assert_eq!(std::fs::read(scratch.0.join("roads.geojson")).expect("read back"), b"{}");
    }

    /// A staged input never writes through a file that is already there.
    #[test]
    fn staging_refuses_a_file_that_already_exists() {
        let parent = Parent::new("exists");
        let scratch = Scratch::new_in(parent.path()).expect("a scratch folder");
        std::fs::write(scratch.0.join("taken.csv"), b"old").expect("an existing file");
        assert!(scratch.stage("taken.csv", b"new").is_err());
        assert_eq!(std::fs::read(scratch.0.join("taken.csv")).expect("read back"), b"old");
    }

    /// The folder is removed with what it holds when the run ends.
    #[test]
    fn a_scratch_folder_is_removed_afterwards() {
        let parent = Parent::new("removed");
        let scratch = Scratch::new_in(parent.path()).expect("a scratch folder");
        scratch.stage("a.tif", b"x").expect("a staged input");
        let dir = scratch.0.clone();
        drop(scratch);
        assert!(!dir.exists());
    }
}
