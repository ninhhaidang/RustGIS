//! The copies of picked files a phone keeps (#1902). A file picked on a phone is copied into the
//! app's storage (`picked/<pick>/…`, by Android's activity and by the iOS shell), and data and
//! projects open from the copy and are edited there. Nothing removes a copy by itself: a project
//! saved on the phone names its data by the copies' paths, so a copy that looks unused may still be
//! wanted. Remove Unused Copies, in the More sheet, removes the picks the project in the window
//! doesn't name, once the user has seen how many and how much, and agreed.
//!
//! A pick's folder is kept or removed whole: a project names a Shapefile's `.shp`, not its `.dbf`,
//! nor a GeoPackage's `-wal`, so a pick is in use when the project names any file in it.
//!
//! A pick still being copied into is kept too. The phones mark a pick's folder with [`COPYING`]
//! while they copy into it, and write each copy under a `.part` name of its own, renamed once
//! whole. A mark or a `.part` file left by a copy the app was killed during stops counting after
//! [`STALE`], and its pick is judged like any other.

use rustgis_ui_egui::rustgis_engine::data::dirs;
use rustgis_ui_egui::{RustGisApp, i18n, tl};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The deepest a file of a pick is (`picked/<pick>/<source folder>/<file>`), looked for a level
/// deeper: a pick holds no deeper folders, and a link out of it is never followed.
const DEPTH: usize = 3;

/// The file the phones put in a pick's folder while they copy documents into it, for the whole
/// pick, and remove once done.
pub const COPYING: &str = ".copying";

/// How long a pick's [`COPYING`] mark or `.part` file counts after it was last written: a copy
/// writes its `.part` as it goes, but iOS may first download a document from iCloud for a while.
const STALE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// The picks the project doesn't name.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Unused {
    /// Each pick's folder (or a loose file directly in `picked/`).
    pub picks: Vec<PathBuf>,
    /// The files in them.
    pub files: usize,
    /// Their size, in bytes.
    pub bytes: u64,
    /// Entries that couldn't be read in full, kept: what isn't seen can't be judged unused.
    pub unreadable: usize,
    /// The picks among them (an entry not even listed has no path).
    pub unreadable_picks: Vec<PathBuf>,
    /// Picks being copied into (or just made, with no file yet), kept.
    pub busy_picks: Vec<PathBuf>,
}

/// What Remove Them did.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Removed {
    pub files: usize,
    pub bytes: u64,
    /// What couldn't be removed, each in its own words.
    pub failed: Vec<String>,
    /// Picks found unused when asked that the project names now (it changed since), kept.
    pub now_named: usize,
    /// Picks found unused when asked that couldn't be read in full when looked at again, kept.
    pub unreadable: usize,
    /// Picks found unused when asked that are being copied into now, kept.
    pub busy: usize,
}

/// Where the copies are, where the app has a folder of its own (a phone).
pub(crate) fn folder() -> Option<PathBuf> {
    dirs::root().map(|root| root.join("picked"))
}

/// The picks in `picked` none of whose files `project` (the project as it is saved, in JSON)
/// names, looking at each between picks for `cancelled`.
pub(crate) fn unused(picked: &Path, project: &str, cancelled: &AtomicBool) -> Result<Unused, String> {
    let entries = std::fs::read_dir(picked).map_err(|e| format!("{}: {e}", picked.display()))?;
    let mut found = Unused::default();
    let mut picks = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => picks.push(entry.path()),
            // Not seen, so not judged: counted, and said.
            Err(_) => found.unreadable += 1,
        }
    }
    picks.sort();
    for pick in picks {
        if cancelled.load(Ordering::Relaxed) {
            return Err(tl!("Stopped.").to_string());
        }
        let mut files = Vec::new();
        // A pick not read in full may hold a file the project names: kept.
        if !gather(&pick, DEPTH, &mut files) {
            found.unreadable += 1;
            found.unreadable_picks.push(pick);
            continue;
        }
        // A pick with no file is being made, and one with a recent mark or `.part` file is being
        // copied into: kept.
        if files.is_empty() || files.iter().any(|(file, _)| in_flight(file)) {
            found.busy_picks.push(pick);
            continue;
        }
        if files.iter().any(|(file, _)| named(project, file)) {
            continue;
        }
        found.files += files.len();
        found.bytes += files.iter().map(|(_, size)| size).sum::<u64>();
        found.picks.push(pick);
    }
    Ok(found)
}

/// The files at or under `path` (itself, if it is one), with their sizes, `depth` levels down at
/// most. Links aren't followed. Returns whether everything there could be read.
fn gather(path: &Path, depth: usize, files: &mut Vec<(PathBuf, u64)>) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return false };
    if meta.is_file() {
        files.push((path.to_path_buf(), meta.len()));
        return true;
    }
    // A link, or another entry that is neither file nor folder, isn't looked through, and a
    // folder too deep isn't looked into: not read in full, so its pick is kept.
    if !meta.is_dir() || depth == 0 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(path) else { return false };
    let mut complete = true;
    for entry in entries {
        match entry {
            Ok(entry) => complete &= gather(&entry.path(), depth - 1, files),
            Err(_) => complete = false,
        }
    }
    complete
}

/// Whether `file` says its pick is being copied into: a [`COPYING`] mark or a `.part` copy, written
/// in the last [`STALE`].
fn in_flight(file: &Path) -> bool {
    let marks = file.file_name().is_some_and(|n| n == COPYING) || file.extension().is_some_and(|e| e == "part");
    marks && recent(file)
}

/// Whether `file` was last written in the last [`STALE`] (or in the future, as a clock set back
/// makes it).
fn recent(file: &Path) -> bool {
    let Ok(written) = std::fs::symlink_metadata(file).and_then(|m| m.modified()) else { return false };
    std::time::SystemTime::now().duration_since(written).map_or(true, |age| age < STALE)
}

/// Whether the project's JSON names `file`, written as JSON writes it.
fn named(project: &str, file: &Path) -> bool {
    let text = file.to_string_lossy();
    let quoted = serde_json::to_string(text.as_ref()).unwrap_or_default();
    let escaped = quoted.strip_prefix('"').and_then(|q| q.strip_suffix('"')).unwrap_or(&quoted);
    !escaped.is_empty() && project.contains(escaped)
}

/// Removes the picks of `found` that `picked` still holds unused by `project` (the project as it
/// is now, looked at again: it may have changed since the user was asked), looking for
/// `cancelled` between them.
fn remove_unused(picked: &Path, found: &Unused, project: &str, cancelled: &AtomicBool) -> Result<Removed, String> {
    let now = unused(picked, project, cancelled)?;
    let still: Vec<PathBuf> = found.picks.iter().filter(|pick| now.picks.contains(pick)).cloned().collect();
    let mut removed = remove(&still, cancelled);
    // Each pick asked about that isn't removable now, counted once: not read in full, being copied
    // into, or named (what is left of those that still exist).
    let kept: Vec<&PathBuf> = found.picks.iter().filter(|pick| !now.picks.contains(pick)).collect();
    removed.unreadable = kept.iter().filter(|pick| now.unreadable_picks.contains(pick)).count();
    removed.busy = kept.iter().filter(|pick| now.busy_picks.contains(pick)).count();
    removed.now_named =
        kept.iter().filter(|pick| pick.exists() && !now.unreadable_picks.contains(pick) && !now.busy_picks.contains(pick)).count();
    Ok(removed)
}

/// Removes `picks` (folders or files), looking for `cancelled` between them.
fn remove(picks: &[PathBuf], cancelled: &AtomicBool) -> Removed {
    let (mut files, mut bytes, mut failed) = (0usize, 0u64, Vec::new());
    for pick in picks {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let mut held = Vec::new();
        gather(pick, DEPTH, &mut held);
        let gone = if pick.is_dir() { std::fs::remove_dir_all(pick) } else { std::fs::remove_file(pick) };
        // What is gone is counted, also when the removal stopped partway through a pick.
        let removed: Vec<&(PathBuf, u64)> =
            if gone.is_ok() { held.iter().collect() } else { held.iter().filter(|(file, _)| !file.exists()).collect() };
        files += removed.len();
        bytes += removed.iter().map(|(_, size)| size).sum::<u64>();
        if let Err(e) = gone {
            failed.push(format!("{}: {e}", pick.display()));
        }
    }
    Removed { files, bytes, failed, ..Removed::default() }
}

/// Where Remove Unused Copies is.
#[derive(Clone, Default)]
pub(crate) enum Job {
    #[default]
    Idle,
    /// Looking for the unused copies, off the window's thread.
    Scanning(Arc<Mutex<Option<Result<Unused, String>>>>, Arc<AtomicBool>),
    /// Found: waiting for the user to agree.
    Asking(Unused),
    /// Removing them, off the window's thread.
    Removing(Arc<Mutex<Option<Result<Removed, String>>>>, Arc<AtomicBool>),
}

fn id() -> egui::Id {
    egui::Id::unique("touch_copies")
}

pub(crate) fn job(ctx: &egui::Context) -> Job {
    ctx.data(|d| d.get_temp::<Job>(id())).unwrap_or_default()
}

fn set_job(ctx: &egui::Context, job: Job) {
    ctx.data_mut(|d| d.insert_temp(id(), job));
}

/// Starts looking for the copies the project doesn't name. The project is copied on the window's
/// thread (one step, without Cancel); it is written out and the copies looked at off it.
pub(crate) fn start(app: &mut RustGisApp, ctx: &egui::Context) {
    let Some(picked) = folder() else { return };
    let project = app.session.project.clone();
    let (result, cancelled) = (Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
    let (out, stop, wake) = (result.clone(), cancelled.clone(), ctx.clone());
    let started = std::thread::Builder::new().name("unused copies".into()).spawn(move || {
        let found = serde_json::to_string(&project).map_err(|e| e.to_string()).and_then(|text| unused(&picked, &text, &stop));
        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(found);
        wake.request_repaint();
    });
    match started {
        Ok(_) => set_job(ctx, Job::Scanning(result, cancelled)),
        Err(e) => app.session.notify(i18n::fmt("The unused copies weren't looked for: {error}", &[("error", &e.to_string())]), true),
    }
}

/// Removes the copies the user agreed to, off the window's thread: those still unused by the
/// project as it is now (copied here, one step), as it may have changed while the question was up
/// (an Undo, another project opened).
pub(crate) fn confirm(app: &mut RustGisApp, ctx: &egui::Context, found: Unused) {
    let Some(picked) = folder() else {
        set_job(ctx, Job::Idle);
        return;
    };
    let project = app.session.project.clone();
    let (result, cancelled) = (Arc::new(Mutex::new(None)), Arc::new(AtomicBool::new(false)));
    let (out, stop, wake) = (result.clone(), cancelled.clone(), ctx.clone());
    let started = std::thread::Builder::new().name("removing copies".into()).spawn(move || {
        let removed =
            serde_json::to_string(&project).map_err(|e| e.to_string()).and_then(|text| remove_unused(&picked, &found, &text, &stop));
        *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(removed);
        wake.request_repaint();
    });
    match started {
        Ok(_) => set_job(ctx, Job::Removing(result, cancelled)),
        Err(e) => {
            set_job(ctx, Job::Idle);
            app.session.notify(i18n::fmt("The unused copies weren't removed: {error}", &[("error", &e.to_string())]), true);
        }
    }
}

/// Whether unused copies are being removed now. Done once the thread has finished, though its
/// answer is only said by the touch interface (which the user may have left meanwhile).
pub(crate) fn removing(ctx: &egui::Context) -> bool {
    match job(ctx) {
        Job::Removing(result, _) => result.lock().unwrap_or_else(|e| e.into_inner()).is_none(),
        _ => false,
    }
}

/// Lets go of what was found, or stops what is running.
pub(crate) fn cancel(ctx: &egui::Context) {
    match job(ctx) {
        Job::Scanning(_, stop) | Job::Removing(_, stop) => stop.store(true, Ordering::Relaxed),
        Job::Asking(_) | Job::Idle => set_job(ctx, Job::Idle),
    }
}

/// A size for people: megabytes with one decimal, or kilobytes under a tenth of a megabyte.
pub(crate) fn size(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 0.1 {
        i18n::fmt("{n} MB", &[("n", &format!("{mb:.1}"))])
    } else {
        i18n::fmt("{n} KB", &[("n", &bytes.div_ceil(1024).to_string())])
    }
}

/// Takes what the threads found, each frame: what is found is asked about, what was removed is
/// said.
pub(crate) fn poll(app: &mut RustGisApp, ctx: &egui::Context) {
    match job(ctx) {
        Job::Idle | Job::Asking(_) => {}
        Job::Scanning(result, stop) => match result.lock().unwrap_or_else(|e| e.into_inner()).take() {
            None => ctx.request_repaint_after(std::time::Duration::from_millis(250)),
            Some(Ok(found)) if found.picks.is_empty() => {
                set_job(ctx, Job::Idle);
                let mut text = tl!("No copies of picked files are unused: the project names every one.").to_string();
                if !found.busy_picks.is_empty() {
                    text.push(' ');
                    text.push_str(&busy_said(found.busy_picks.len()));
                }
                if found.unreadable > 0 {
                    text.push(' ');
                    text.push_str(&unreadable_said(found.unreadable));
                }
                app.session.notify(text, found.unreadable > 0);
            }
            Some(Ok(found)) => set_job(ctx, Job::Asking(found)),
            Some(Err(_)) if stop.load(Ordering::Relaxed) => set_job(ctx, Job::Idle),
            Some(Err(error)) => {
                set_job(ctx, Job::Idle);
                app.session.notify(i18n::fmt("The unused copies weren't looked for: {error}", &[("error", &error)]), true);
            }
        },
        Job::Removing(result, stop) => match result.lock().unwrap_or_else(|e| e.into_inner()).take() {
            None => ctx.request_repaint_after(std::time::Duration::from_millis(250)),
            Some(Err(_)) if stop.load(Ordering::Relaxed) => set_job(ctx, Job::Idle),
            Some(Err(error)) => {
                set_job(ctx, Job::Idle);
                app.session.notify(i18n::fmt("The unused copies weren't removed: {error}", &[("error", &error)]), true);
            }
            Some(Ok(removed)) => {
                set_job(ctx, Job::Idle);
                let (count, size) = (removed.files.to_string(), size(removed.bytes));
                let template = if removed.files == 1 {
                    "Removed 1 copy of a picked file ({size})."
                } else {
                    "Removed {count} copies of picked files ({size})."
                };
                let mut text = i18n::fmt(template, &[("count", &count), ("size", &size)]);
                let mut said = |more: String| {
                    text.push(' ');
                    text.push_str(&more);
                };
                if stop.load(Ordering::Relaxed) {
                    said(tl!("Stopped before the rest.").to_string());
                }
                if removed.now_named > 0 {
                    let n = removed.now_named.to_string();
                    said(i18n::fmt("{n} kept: the project names them now.", &[("n", &n)]));
                }
                if removed.busy > 0 {
                    said(busy_said(removed.busy));
                }
                if removed.unreadable > 0 {
                    said(unreadable_said(removed.unreadable));
                }
                if let Some(first) = removed.failed.first() {
                    let (n, first) = (removed.failed.len().to_string(), first.clone());
                    said(i18n::fmt("{n} couldn't be removed, the first: {first}", &[("n", &n), ("first", &first)]));
                }
                app.session.notify(text, !removed.failed.is_empty() || removed.unreadable > 0);
            }
        },
    }
}

/// What is said of `n` picks being copied into, kept.
fn busy_said(n: usize) -> String {
    i18n::fmt("{n} picks are still being copied into and were kept.", &[("n", &n.to_string())])
}

/// What is said of `n` picks that couldn't be read in full and were kept.
fn unreadable_said(n: usize) -> String {
    i18n::fmt("{n} picks couldn't be read in full and were kept.", &[("n", &n.to_string())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pick_is_kept_whole_when_the_project_names_any_file_in_it() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        // A Shapefile the project names by its .shp (its .dbf beside it), and a GeoPackage it
        // doesn't name, from another pick; a loose file too. A path the project names is joined a
        // folder at a time, as the folder is listed: Windows lists `1\0` where `join("1/0")` keeps `1/0`.
        let shapes = picked.join("1").join("0");
        let unused_pick = picked.join("2").join("0");
        std::fs::create_dir_all(&shapes).unwrap();
        std::fs::create_dir_all(&unused_pick).unwrap();
        std::fs::write(shapes.join("roads.shp"), b"shp").unwrap();
        std::fs::write(shapes.join("roads.dbf"), b"dbf").unwrap();
        std::fs::write(unused_pick.join("parcels.gpkg"), b"0123456789").unwrap();
        std::fs::write(picked.join("loose.csv"), b"a,b").unwrap();
        let project = serde_json::json!({"layers": [{"source": shapes.join("roads.shp").to_string_lossy()}]}).to_string();
        let found = unused(&picked, &project, &AtomicBool::new(false)).unwrap();
        assert_eq!(found.picks, vec![picked.join("2"), picked.join("loose.csv")]);
        assert_eq!((found.files, found.bytes), (2, 13));
        // Removed: the named pick stays, sidecar and all.
        let removed = remove(&found.picks, &AtomicBool::new(false));
        assert_eq!((removed.files, removed.bytes, removed.failed.len()), (2, 13, 0));
        assert!(shapes.join("roads.dbf").exists());
        assert!(!picked.join("2").exists() && !picked.join("loose.csv").exists());
        // Cancelled, nothing is looked at.
        assert!(unused(&picked, &project, &AtomicBool::new(true)).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pick_the_project_names_by_the_time_of_removal_is_kept() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-late-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        for pick in ["1/0", "2/0"] {
            std::fs::create_dir_all(picked.join(pick)).unwrap();
        }
        let a = picked.join("1").join("0").join("a.gpkg");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(picked.join("2").join("0").join("b.gpkg"), b"bb").unwrap();
        let found = unused(&picked, "{}", &AtomicBool::new(false)).unwrap();
        assert_eq!(found.picks.len(), 2);
        // An Undo brings back the layer reading a.gpkg before Remove Them.
        let now = serde_json::json!({"path": a.to_string_lossy()}).to_string();
        let removed = remove_unused(&picked, &found, &now, &AtomicBool::new(false)).unwrap();
        assert_eq!((removed.files, removed.now_named), (1, 1));
        assert!(a.exists() && !picked.join("2").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pick_with_a_copy_being_written_is_kept() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-part-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        std::fs::create_dir_all(picked.join("4/0")).unwrap();
        std::fs::write(picked.join("4/0/big.gpkg.part"), b"half").unwrap();
        // Between two documents of one pick: a copy done, the next not started, the mark there.
        std::fs::create_dir_all(picked.join("5/0")).unwrap();
        std::fs::write(picked.join("5").join(COPYING), b"").unwrap();
        std::fs::write(picked.join("5/0/roads.gpkg"), b"whole").unwrap();
        let found = unused(&picked, "{}", &AtomicBool::new(false)).unwrap();
        assert!(found.picks.is_empty());
        assert_eq!(found.busy_picks, [picked.join("4"), picked.join("5")]);
        // Found unused when asked, then copied into before the removal: kept, and said as such.
        std::fs::remove_file(picked.join("5").join(COPYING)).unwrap();
        let asked = unused(&picked, "{}", &AtomicBool::new(false)).unwrap();
        assert_eq!(asked.picks, [picked.join("5")]);
        std::fs::write(picked.join("5/0/rivers.gpkg.part"), b"half").unwrap();
        let removed = remove_unused(&picked, &asked, "{}", &AtomicBool::new(false)).unwrap();
        assert_eq!((removed.files, removed.busy, removed.now_named), (0, 1, 0));
        assert!(picked.join("5/0/roads.gpkg").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pick_left_by_a_copy_cut_short_long_ago_is_judged_like_any_other() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        std::fs::create_dir_all(picked.join("6/0")).unwrap();
        let (mark, part) = (picked.join("6").join(COPYING), picked.join("6/0/big.gpkg.part"));
        std::fs::write(&mark, b"").unwrap();
        std::fs::write(&part, b"half").unwrap();
        let long_ago = std::time::SystemTime::now() - STALE - std::time::Duration::from_secs(60);
        for file in [&mark, &part] {
            std::fs::File::options().write(true).open(file).unwrap().set_modified(long_ago).unwrap();
        }
        let found = unused(&picked, "{}", &AtomicBool::new(false)).unwrap();
        assert_eq!(found.picks, [picked.join("6")]);
        assert!(found.busy_picks.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pick_with_no_file_is_kept() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        std::fs::create_dir_all(picked.join("3/0")).unwrap();
        let found = unused(&picked, "{}", &AtomicBool::new(false)).unwrap();
        assert!(found.picks.is_empty());
        assert!(picked.join("3/0").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pick_deeper_than_looked_at_is_kept() {
        let root = std::env::temp_dir().join(format!("rustgis-copies-deep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        let deep = picked.join("1/0/x/y");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("z.gpkg"), b"z").unwrap();
        let found = unused(&picked, "{}", &AtomicBool::new(false)).unwrap();
        assert_eq!((found.picks.len(), found.unreadable), (0, 1));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_pick_not_read_in_full_is_kept() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("rustgis-copies-locked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let picked = root.join("picked");
        let locked = picked.join("1/0");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("a.gpkg"), b"a").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Skipped where permissions don't hold (running as root).
        if std::fs::read_dir(&locked).is_err() {
            let found = unused(&picked, "{}", &AtomicBool::new(false)).unwrap();
            assert_eq!((found.picks.len(), found.unreadable), (0, 1));
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_path_is_matched_as_json_writes_it() {
        let file = Path::new("/data/picked/1/0/a \"quoted\" name.geojson");
        let project = serde_json::json!({"path": file.to_string_lossy()}).to_string();
        assert!(named(&project, file));
        assert!(!named(&project, Path::new("/data/picked/1/0/other.geojson")));
    }

    #[test]
    fn sizes_are_said_in_megabytes_or_kilobytes() {
        assert_eq!(size(420), "1 KB");
        assert_eq!(size(50 * 1024), "50 KB");
        assert_eq!(size(3 * 1024 * 1024 + 300 * 1024), "3.3 MB");
    }
}
