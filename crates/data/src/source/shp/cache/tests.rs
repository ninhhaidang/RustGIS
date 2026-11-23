#![expect(clippy::disallowed_methods, reason = "a thread the system refuses fails the test, as it should")]
use super::*;
use crate::source::FeatureSource;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Folder(PathBuf);
impl Folder {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("rustgis-cache-test-{}", unique()));
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Folder {
    fn drop(&mut self) {
        // Only the unique direct child of the test temp directory belongs to this fixture.
        if self.0.parent() == Some(std::env::temp_dir().as_path()) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
fn key(n: usize) -> String {
    format!("{n:016x}-0000000000000001")
}

#[test]
fn cache_test_index_roots_are_scoped_and_thread_local() {
    let first = Folder::new();
    let second = Folder::new();
    let original = index_dir();
    with_index_dir(first.0.clone(), || {
        assert_eq!(index_dir(), first.0);
        let outer = first.0.clone();
        std::thread::spawn(move || assert_ne!(index_dir(), outer)).join().unwrap();
        let result = std::panic::catch_unwind(|| with_index_dir(second.0.clone(), || panic!("test unwind")));
        assert!(result.is_err());
        assert_eq!(index_dir(), first.0);
    });
    assert_eq!(index_dir(), original);
}
fn ready(root: &Path, n: usize, bytes: u64, budget: u64) -> Lease {
    ready_key(root, &key(n), bytes, budget)
}
fn ready_key(root: &Path, key: &str, bytes: u64, budget: u64) -> Lease {
    let mut lease = reserve_index(root, key, budget).unwrap();
    File::options().write(true).open(lease.path()).unwrap().set_len(bytes).unwrap();
    lease.commit().unwrap();
    lease
}
/// Index `version` of source file `file`: the key's first half names the file (#1230).
fn version(file: u64, version: u64) -> String {
    format!("{file:016x}-{version:016x}")
}

/// A new index of a file replaces the idle indexes of its earlier versions; one in use stays,
/// and so do other files' indexes (#1230).
#[test]
fn cache_commit_drops_earlier_versions_of_the_same_file() {
    let root = Folder::new();
    let old = ready_key(&root.0, &version(1, 1), 10, 4096);
    let old_path = old.path().to_path_buf();
    drop(old);
    let held = ready_key(&root.0, &version(1, 2), 10, 4096);
    let other = ready_key(&root.0, &version(2, 1), 10, 4096);
    let other_path = other.path().to_path_buf();
    drop(other);
    assert!(!old_path.exists(), "the idle earlier version goes when a newer one is committed");
    let newest = ready_key(&root.0, &version(1, 3), 10, 4096);
    assert!(held.path().exists(), "an earlier version in use stays");
    assert!(other_path.exists(), "another file's index stays");
    let held_path = held.path().to_path_buf();
    drop(held);
    drop(newest);
    let newer = ready_key(&root.0, &version(1, 4), 10, 4096);
    assert!(!held_path.exists(), "released, it goes with the next version");
    assert!(!index_path(&root.0, &version(1, 3)).unwrap().exists());
    assert!(newer.path().exists());
}

/// An index unused for longer than the limit goes at the next sweep, within budget or not: its
/// file may have been deleted or moved (#1230).
#[test]
fn cache_drops_indexes_unused_too_long() {
    let root = Folder::new();
    let stale = ready(&root.0, 1, 10, 4096);
    let stale_path = stale.path().to_path_buf();
    drop(stale);
    let recent = ready(&root.0, 2, 10, 4096);
    let recent_path = recent.path().to_path_buf();
    drop(recent);
    let long_ago = SystemTime::now() - index_max_idle() - Duration::from_secs(60);
    File::options().write(true).open(lease_path(&stale_path)).unwrap().set_modified(long_ago).unwrap();
    let stats = maintain(&root.0, 4096).unwrap();
    assert_eq!(stats.removed, 1);
    assert!(!stale_path.exists() && !lease_path(&stale_path).exists());
    assert!(recent_path.exists(), "an index used recently stays");
}

/// Files a sweep never evicts (no readable lease) don't leave the folder marked, so releases
/// don't keep sweeping over them (#1230).
#[test]
fn cache_files_without_a_lease_dont_keep_releases_sweeping() {
    let root = Folder::new();
    let stray = index_path(&root.0, &key(1)).unwrap();
    std::fs::write(&stray, [0u8; 64]).unwrap();
    let stats = maintain(&root.0, 10).unwrap();
    assert!(stats.remaining_bytes > 10, "the stray file is counted");
    assert!(!over_budget(&directory_lock(&root.0).unwrap().root), "but no release can free it");
    assert!(stray.exists(), "and it is never deleted");

    // An index in use when its commit sweeps doesn't change that: releasing it can't bring the
    // cache within budget either, so the release doesn't sweep and the index stays.
    let open = ready(&root.0, 2, 5, 10);
    let open_path = open.path().to_path_buf();
    let before = sweeps();
    drop(open);
    assert_eq!(sweeps(), before, "the release doesn't sweep");
    assert!(open_path.exists(), "the released index stays until the next build");
}

/// Emptying the cache removes every index not in use, in each folder, and says what it freed
/// and what stayed in use; a missing folder is skipped (#1259).
#[test]
fn cache_empty_removes_every_index_not_in_use() {
    let (first, second) = (Folder::new(), Folder::new());
    let idle = ready(&first.0, 1, 10, 4096);
    let idle_path = idle.path().to_path_buf();
    drop(idle);
    let held = ready(&first.0, 2, 20, 4096);
    let other = ready(&second.0, 3, 30, 4096);
    let other_path = other.path().to_path_buf();
    drop(other);
    let missing = first.0.join("missing");
    let emptied = empty_folders(&[first.0.clone(), second.0.clone(), missing.clone()]);
    assert_eq!((emptied.removed, emptied.removed_bytes), (2, 40), "{emptied:?}");
    assert_eq!((emptied.in_use, emptied.in_use_bytes, emptied.kept_bytes), (1, 20, 0), "{emptied:?}");
    assert_eq!((emptied.folders, emptied.failed.len()), (2, 0), "{emptied:?}");
    assert!(!idle_path.exists() && !other_path.exists());
    assert!(held.path().exists(), "an index in use stays");
    assert!(!missing.exists(), "a missing folder isn't created");
}

/// A root that isn't a folder is reported, not skipped as a missing one is (#1259).
#[test]
fn cache_empty_reports_a_root_that_is_not_a_folder() {
    let root = Folder::new();
    let file = root.0.join("not-a-folder");
    std::fs::write(&file, b"keep").unwrap();
    let emptied = empty_folders(std::slice::from_ref(&file));
    assert_eq!((emptied.folders, emptied.failed.len()), (0, 1), "{emptied:?}");
    assert!(emptied.failed[0].contains("not a folder"), "{emptied:?}");
    assert_eq!(std::fs::read(&file).unwrap(), b"keep");
}

/// A folder the cache refuses (writable by others) is reported and doesn't stop the next one
/// from being emptied: that is where the indexes are when the first is refused (#1259).
#[cfg(unix)]
#[test]
fn cache_empty_goes_on_past_a_folder_it_refuses() {
    use std::os::unix::fs::PermissionsExt;
    let (refused, fallback) = (Folder::new(), Folder::new());
    let idle = ready(&fallback.0, 1, 10, 4096);
    let idle_path = idle.path().to_path_buf();
    drop(idle);
    std::fs::set_permissions(&refused.0, std::fs::Permissions::from_mode(0o777)).unwrap();
    let emptied = empty_folders(&[refused.0.clone(), fallback.0.clone()]);
    assert_eq!((emptied.folders, emptied.removed), (1, 1), "{emptied:?}");
    assert_eq!(emptied.failed.len(), 1, "{emptied:?}");
    assert!(emptied.failed[0].contains(&*refused.0.to_string_lossy()), "{emptied:?}");
    assert!(!idle_path.exists());
}

#[test]
fn cache_lru_touches_reopened_indexes_and_evicts_oldest_idle() {
    let root = Folder::new();
    for n in 1..=3 {
        let lease = ready(&root.0, n, 10, 4096);
        let path = lease.path().to_path_buf();
        drop(lease);
        // Used minutes apart, in order, and recently enough to stay within the idle limit.
        let used = SystemTime::now() - Duration::from_secs(600 - 60 * n as u64);
        File::options().write(true).open(lease_path(&path)).unwrap().set_modified(used).unwrap();
    }
    let hot = pin_index(&root.0, &key(1), 4096).unwrap().unwrap();
    drop(hot);
    let stats = maintain(&root.0, 20).unwrap();
    assert_eq!(stats.remaining_bytes, 20);
    assert_eq!(stats.removed, 1);
    assert!(index_path(&root.0, &key(1)).unwrap().exists());
    assert!(!index_path(&root.0, &key(2)).unwrap().exists());
    assert!(index_path(&root.0, &key(3)).unwrap().exists());
}

#[test]
fn cache_shared_readers_and_oversized_indexes_are_protected_until_release() {
    let root = Folder::new();
    let first = ready(&root.0, 1, 10, 4096);
    let second = pin_index(&root.0, &key(1), 4096).unwrap().unwrap();
    let path = first.path().to_path_buf();
    assert_eq!(maintain(&root.0, 0).unwrap().pinned_bytes, 10);
    drop(first);
    assert!(path.exists());
    assert_eq!(maintain(&root.0, 0).unwrap().remaining_bytes, 10);
    drop(second);
    assert_eq!(maintain(&root.0, 0).unwrap().remaining_bytes, 0);
    assert!(!path.exists());
    let oversized = ready(&root.0, 2, 40, 20);
    let path = oversized.path().to_path_buf();
    assert_eq!(maintain(&root.0, 20).unwrap().pinned_bytes, 40);
    assert!(path.exists());
    // A concurrent sweep can defer release cleanup on Unix as well as Windows.
    let sweep = directory_lock(&root.0).unwrap();
    drop(oversized);
    assert!(path.exists(), "release must not mutate a directory owned by a sweep");
    drop(sweep);
    wait_removed(&path);
}

/// A release sweeps the folder only when the last sweep couldn't bring it within budget because
/// of indexes in use; one within budget is left alone (#1230).
#[test]
fn cache_releases_sweep_only_a_cache_left_over_budget() {
    let root = Folder::new();
    let kept = ready(&root.0, 1, 10, 4096);
    let kept_path = kept.path().to_path_buf();
    let before = sweeps();
    drop(kept);
    assert_eq!(sweeps(), before, "a release within budget doesn't sweep");
    assert!(kept_path.exists(), "a ready index within budget stays");

    // The first's commit evicts the idle index; the second's can't evict either of these two in
    // use, so the cache stays over budget until one is released.
    let first = ready(&root.0, 2, 10, 10);
    assert!(!kept_path.exists(), "the idle index went first");
    let second = ready(&root.0, 3, 10, 10);
    let (first_path, second_path) = (first.path().to_path_buf(), second.path().to_path_buf());
    let before = sweeps();
    drop(first);
    assert_eq!(sweeps(), before + 1, "a release frees what the last sweep couldn't");
    assert!(!first_path.exists(), "the released index brought the cache back within budget");
    let before = sweeps();
    drop(second);
    assert_eq!(sweeps(), before, "within budget again, the next release doesn't sweep");
    assert!(second_path.exists());

    // A sweep that fails may have left the cache over budget: the next release sweeps again.
    let missing = root.0.join("missing");
    assert!(prune_locked(&missing, 10, None).is_err());
    assert!(over_budget(&missing));
    note_budget(&missing, false);
}

/// A failed build never reaches the sweep at commit, so its release sweeps instead: abandoned
/// builds left by a crashed process don't outlive the build that failed next to them (#1230).
#[test]
fn cache_failed_build_sweeps_abandoned_builds() {
    let root = Folder::new();
    let abandoned = root.0.join("rg-index-work-v1-1-2-3.sqlite");
    std::fs::write(&abandoned, b"partial").unwrap();
    std::fs::write(lease_path(&abandoned), b"T").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(lease_path(&abandoned), std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let failed = reserve_index(&root.0, &key(1), 4096).unwrap();
    assert!(abandoned.exists(), "reserving doesn't sweep");
    let failed_path = failed.path().to_path_buf();
    let before = sweeps();
    drop(failed);
    assert_eq!(sweeps(), before + 1, "a failed build sweeps on release");
    assert!(!abandoned.exists(), "the abandoned build is reclaimed");
    assert!(!failed_path.exists(), "the failed build removes its own index");
}

/// A reservation that fails takes no lease, so no release would sweep the folder: it sweeps
/// itself, and abandoned builds are reclaimed even though no lease is taken (#1230).
#[test]
fn cache_failed_reservation_sweeps_abandoned_builds() {
    let root = Folder::new();
    let abandoned = root.0.join("rg-index-work-v1-1-2-3.sqlite");
    std::fs::write(&abandoned, b"partial").unwrap();
    std::fs::write(lease_path(&abandoned), b"T").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(lease_path(&abandoned), std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    // A folder where the lease file should be makes the reservation fail.
    let blocked = lease_path(&index_path(&root.0, &key(1)).unwrap());
    std::fs::create_dir(&blocked).unwrap();
    let before = sweeps();
    assert!(reserve_index(&root.0, &key(1), 4096).is_err());
    assert_eq!(sweeps(), before + 1, "a failed reservation sweeps");
    assert!(!abandoned.exists(), "the abandoned build is reclaimed");
    assert!(blocked.is_dir(), "the sweep leaves what it doesn't own");
}

fn wait_removed(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while path.exists() {
        assert!(Instant::now() < deadline, "release cleanup was never retried: {}", path.display());
        std::thread::sleep(Duration::from_millis(10)); // test-wait: legacy polling for cache cleanup
    }
}

#[test]
fn cache_release_racing_a_sweep_is_retried_without_another_checkout() {
    let root = Folder::new();
    let (lease, file) = temporary_index(&root.0);
    drop(file);
    let path = lease.path().to_path_buf();
    let sweep = directory_lock(&root.0).unwrap();
    drop(lease);
    assert!(path.exists(), "a busy sweep owns directory mutations");
    drop(sweep);
    wait_removed(&path);
}

#[test]
fn cache_concurrent_builder_uses_private_index_without_replacing_reader() {
    let root = Folder::new();
    let reader = ready(&root.0, 1, 10, 4096);
    let mut private = reserve_index(&root.0, &key(1), 4096).unwrap();
    assert_ne!(reader.path(), private.path());
    File::options().write(true).open(private.path()).unwrap().set_len(30).unwrap();
    private.commit().unwrap();
    assert_eq!(std::fs::metadata(reader.path()).unwrap().len(), 10);
    let path = private.path().to_path_buf();
    drop(private);
    assert!(!path.exists());
    assert!(reader.path().exists());
}

// A process spawned on another thread holds duplicates of every descriptor until it
// execs. `try_clone` makes the same duplicate: releases must not wait for it to close.
#[test]
fn cache_releases_unlock_even_while_a_spawned_process_holds_a_duplicate() {
    let root = Folder::new();
    let (lease, file) = temporary_index(&root.0);
    drop(file);
    let path = lease.path().to_path_buf();
    let duplicate = lease.lock.as_ref().unwrap().try_clone().unwrap();
    drop(lease);
    assert!(!path.exists(), "a released temporary index is removed at once");
    assert_eq!(maintain(&root.0, 0).unwrap().remaining_bytes, 0);
    drop(duplicate);

    let lease = ready(&root.0, 1, 10, 4096);
    let shared = lease.path().to_path_buf();
    let duplicate = lease.lock.as_ref().unwrap().try_clone().unwrap();
    drop(lease);
    let rebuilt = reserve_index(&root.0, &key(1), 4096).unwrap();
    assert_eq!(rebuilt.path(), shared, "a released index is not still pinned by the duplicate");
    drop(rebuilt);
    drop(duplicate);

    let sweep = directory_lock(&root.0).unwrap();
    let duplicate = sweep.file.try_clone().unwrap();
    drop(sweep);
    assert!(directory_file(&root.0).unwrap().file.try_lock().is_ok(), "a finished sweep releases the directory");
    drop(duplicate);
}

#[test]
fn cache_cleanup_is_bounded_and_preserves_unknown_files_and_orphans_in_use() {
    let root = Folder::new();
    std::fs::write(root.0.join("customer.sqlite"), b"keep").unwrap();
    let unleased = root.0.join("rg-index-work-v1-1-2-4.sqlite");
    std::fs::write(&unleased, b"unverified").unwrap();
    assert!(!owned("rg-index-work-v1-abc.sqlite"));
    std::fs::write(root.0.join("0000000000000000.sqlite"), b"legacy index: ownership uncertain").unwrap();
    for n in 0..CANDIDATES + 3 {
        let path = index_path(&root.0, &key(n)).unwrap();
        std::fs::write(&path, [1]).unwrap();
        std::fs::write(lease_path(&path), b"C").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::set_permissions(lease_path(&path), std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    let orphan = root.0.join("rg-index-work-v1-1-2-3.lease");
    let lock = open_lock(&orphan).unwrap();
    lock.lock().unwrap();
    let stats = maintain(&root.0, 2).unwrap();
    assert_eq!(stats.remaining_bytes, 10);
    assert_eq!(stats.removed, (CANDIDATES + 3) as u64);
    assert_eq!(std::fs::read(unleased).unwrap(), b"unverified");
    assert!(orphan.exists());
    drop(lock);
    maintain(&root.0, 2).unwrap();
    assert!(!orphan.exists());
    assert_eq!(std::fs::read(root.0.join("customer.sqlite")).unwrap(), b"keep");
    assert!(root.0.join("0000000000000000.sqlite").exists());
    assert!(reserve_index(&root.0, "../customer", 2).is_err());
}

struct Process(Child, PathBuf);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn(root: &Path, mode: &str) -> Process {
    let marker = root.join(format!("ready-{}", unique()));
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "source::shp::cache::tests::cache_child_fixture", "--nocapture"])
        .env("RUSTGIS_CACHE_TEST_READY", &marker)
        .env("RUSTGIS_CACHE_TEST_ROOT", root)
        .env("RUSTGIS_CACHE_TEST_MODE", mode)
        .env("RUSTGIS_SHP_CACHE", if matches!(mode, "fallback" | "scan") { root.join("blocked") } else { root.to_path_buf() })
        .env("TMP", root)
        .env("TEMP", root)
        .env("TMPDIR", root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    Process(child, marker)
}
fn wait_ready(child: &mut Process, root: &Path) -> PathBuf {
    let marker = &child.1;
    assert!(marker.starts_with(root));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !marker.exists() {
        assert!(child.0.try_wait().unwrap().is_none(), "cache child exited before holding its lease");
        assert!(Instant::now() < deadline, "cache child did not become ready");
        std::thread::sleep(Duration::from_millis(10)); // test-wait: legacy polling for the child lease marker
    }
    PathBuf::from(std::fs::read_to_string(marker).unwrap())
}

/// Real process death releases OS leases without running any Rust destructors.
#[test]
fn cache_killed_process_is_reclaimed_at_checkout_but_live_instance_is_preserved() {
    let root = Folder::new();
    let mut live = spawn(&root.0, "temporary");
    let live_path = wait_ready(&mut live, &root.0);
    for mode in ["temporary", "building", "index"] {
        let mut dead = spawn(&root.0, mode);
        let dead_path = wait_ready(&mut dead, &root.0);
        assert!(maintain(&root.0, 0).unwrap().pinned_bytes >= 2048);
        if mode == "index" {
            let reader = pin_index(&root.0, &key(dead.0.id() as usize), 4096).unwrap().unwrap();
            assert_eq!(reader.path(), dead_path);
        }
        dead.0.kill().unwrap();
        dead.0.wait().unwrap();
        let mut cleaner = spawn(&root.0, "sweep");
        assert!(cleaner.0.wait().unwrap().success());
        if mode == "index" {
            assert!(dead_path.exists(), "a ready index below budget must survive restart");
            maintain(&root.0, 0).unwrap();
        }
        assert!(!dead_path.exists());
        assert!(live_path.exists(), "checkout deleted another running instance's temporary index");
    }
    live.0.kill().unwrap();
    live.0.wait().unwrap();
    let mut cleaner = spawn(&root.0, "sweep");
    assert!(cleaner.0.wait().unwrap().success());
    assert!(!live_path.exists());
}

#[test]
fn cache_child_fixture() {
    let Some(root) = std::env::var_os("RUSTGIS_CACHE_TEST_ROOT").map(PathBuf::from) else { return };
    let mode = std::env::var("RUSTGIS_CACHE_TEST_MODE").unwrap();
    if mode == "reject-root" {
        for result in [
            directory_lock(&root).map(|_| ()),
            pin_index(&root, &key(1), 4096).map(|_| ()),
            reserve_index(&root, &key(1), 4096).map(|_| ()),
            maintain(&root, 4096).map(|_| ()),
        ] {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        }
        return;
    }
    if mode == "sweep" {
        checkout().unwrap();
        return;
    }
    if mode == "scan" {
        std::fs::write(index_fallback_dir(), b"keep fallback").unwrap();
        let mut ds = crate::Dataset::new("scan", crate::GeometryType::Point, vec![]);
        ds.push(Some(crate::Geometry::Point(crate::Coord::new(1.0, 2.0))), vec![]);
        let path = root.join("source.shp");
        crate::io::write_path(&ds, &path).unwrap();
        let source = super::super::ShpSource::open_path(&path, "scan").unwrap();
        assert!(!source.info().indexed);
        assert!(source.info().notes.iter().any(|n| n.contains("scanning record headers")));
        for limit in [0, 1, 100] {
            let batch = source.read(&crate::source::Request { extent: Some(crate::Extent::new(0.0, 0.0, 3.0, 3.0)), limit }).unwrap();
            assert_eq!(batch.matched, 1);
            assert_eq!(batch.features.len(), limit.min(1));
        }
        let expected = source.read(&crate::source::Request::all()).unwrap();
        let mut rows = Vec::new();
        let summary = source
            .visit_chunks(None, 1, &mut |chunk| {
                rows.extend(chunk);
                Ok(())
            })
            .unwrap();
        assert!(summary.complete);
        assert!(summary.features.is_empty());
        assert_eq!(summary.matched, expected.matched);
        assert_eq!(rows, expected.features, "complete chunks must also work without a disk cache");
        assert_eq!(std::fs::read(root.join("blocked")).unwrap(), b"keep");
        assert_eq!(std::fs::read(index_fallback_dir()).unwrap(), b"keep fallback");
        return;
    }
    if mode == "fallback" {
        let mut ds = crate::Dataset::new("fallback", crate::GeometryType::Point, vec![]);
        ds.push(Some(crate::Geometry::Point(crate::Coord::new(1.0, 2.0))), vec![]);
        let path = root.join("source.shp");
        crate::io::write_path(&ds, &path).unwrap();
        let source = crate::source::open_path(&path, None).unwrap();
        assert_eq!(source.read(&crate::source::Request::all()).unwrap().features.len(), 1);
        drop(source);
        let fallback = index_fallback_dir();
        let index = std::fs::read_dir(&fallback)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("rg-index-v1-"))
                    && p.extension().is_some_and(|e| e == "sqlite")
            })
            .unwrap();
        let db = rusqlite::Connection::open(&index).unwrap();
        db.execute_batch("CREATE TABLE reuse_probe(value INTEGER)").unwrap();
        drop(db);
        drop(crate::source::open_path(&path, None).unwrap());
        let db = rusqlite::Connection::open(&index).unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM sqlite_master WHERE name='reuse_probe'", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        drop(db);
        let abandoned = fallback.join("rg-index-work-v1-1-2-3.sqlite");
        std::fs::write(&abandoned, b"partial").unwrap();
        std::fs::write(lease_path(&abandoned), b"T").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(lease_path(&abandoned), std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        checkout().unwrap();
        assert!(!abandoned.exists(), "checkout must sweep the fallback cache too");
        assert_eq!(std::fs::read(root.join("blocked")).unwrap(), b"keep");
        return;
    }
    let lease = if mode == "temporary" {
        let (lease, file) = temporary_index(&root);
        file.set_len(1024).unwrap();
        lease
    } else if mode == "index" {
        ready(&root, std::process::id() as usize, 1024, 4096)
    } else {
        let lease = reserve_index(&root, &key(std::process::id() as usize), 4096).unwrap();
        File::options().write(true).open(lease.path()).unwrap().set_len(1024).unwrap();
        lease
    };
    let marker = PathBuf::from(std::env::var_os("RUSTGIS_CACHE_TEST_READY").unwrap());
    let scratch = marker.with_extension("tmp");
    std::fs::write(&scratch, lease.path().to_string_lossy().as_bytes()).unwrap();
    std::fs::rename(scratch, marker).unwrap();
    std::thread::sleep(Duration::from_secs(60)); // test-wait: keep the fixture lease alive until its parent kills it
    drop(lease);
}

#[test]
fn cache_symlinks_never_authorize_deleting_or_modifying_their_targets() {
    let root = Folder::new();
    let other = Folder::new();
    let sentinel = other.0.join("sentinel");
    std::fs::write(&sentinel, b"private").unwrap();
    let link = root.0.join("rg-index-work-v1-1-2-3.sqlite");
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&sentinel, &link).is_ok();
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&sentinel, &link).is_ok();
    #[cfg(not(any(windows, unix)))]
    let linked = false;
    if linked {
        maintain(&root.0, 0).unwrap();
        assert!(std::fs::symlink_metadata(link).is_ok());
    }
    assert_eq!(std::fs::read(sentinel).unwrap(), b"private");
}

#[test]
fn cache_unwritable_primary_reuses_and_cleans_the_fallback_directory() {
    let root = Folder::new();
    std::fs::write(root.0.join("blocked"), b"keep").unwrap();
    let mut child = spawn(&root.0, "fallback");
    assert!(child.0.wait().unwrap().success(), "fallback cache was not reused or cleaned");
}

fn temporary_index(root: &Path) -> (Lease, File) {
    let directory = directory_lock(root).unwrap();
    create_locked(directory.root.join(format!("rg-index-work-v1-{}.sqlite", unique())), true, 4096).unwrap()
}
fn checkout() -> io::Result<()> {
    for root in [index_dir(), index_fallback_dir()] {
        if root.is_dir() {
            maintain(&root, index_budget())?;
        }
    }
    Ok(())
}

#[test]
fn cache_unwritable_locations_keep_read_only_shapefile_loading_available() {
    let root = Folder::new();
    std::fs::write(root.0.join("blocked"), b"keep").unwrap();
    let mut child = spawn(&root.0, "scan");
    assert!(child.0.wait().unwrap().success(), "scan fallback failed");
}

#[test]
fn cache_reservation_preserves_unverified_payload_even_at_the_requested_key() {
    let root = Folder::new();
    let path = index_path(&root.0, &key(17)).unwrap();
    std::fs::write(&path, b"unrelated").unwrap();
    let lease = reserve_index(&root.0, &key(17), 4096).unwrap();
    assert_ne!(lease.path(), path);
    assert_eq!(std::fs::read(&path).unwrap(), b"unrelated");
    drop(lease);
    maintain(&root.0, 0).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"unrelated");
}

/// A predictable per-UID name still needs ownership checks before any blocking lock.
#[cfg(unix)]
#[test]
fn cache_unix_unsafe_root_is_rejected_before_an_attacker_lock() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = Folder::new();
    let lock_path = root.0.join(".rustgis-cache-v1.lock");
    std::fs::write(&lock_path, b"sentinel").unwrap();
    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let lock = open_lock(&lock_path).unwrap();
    lock.lock().unwrap();
    std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o777)).unwrap();
    let mut child = spawn(&root.0, "reject-root");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "unsafe root was accepted");
            break;
        }
        assert!(Instant::now() < deadline, "unsafe root reached the attacker's blocking lock");
        std::thread::sleep(Duration::from_millis(10)); // test-wait: legacy polling for rejection of the unsafe root
    }
    assert_eq!(std::fs::read(lock_path).unwrap(), b"sentinel");
    assert_eq!(root.0.metadata().unwrap().mode() & 0o777, 0o777, "do not repair untrusted roots");
}

#[cfg(unix)]
#[test]
fn cache_unix_roots_are_private_and_legacy_owned_roots_still_work() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let folder = Folder::new();
    let root = folder.0.join("new/nested");
    drop(directory_lock(&root).unwrap());
    for dir in [&root, &folder.0.join("new")] {
        let metadata = dir.metadata().unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o700);
        assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
    }
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    let lease = ready(&root, 1, 10, 4096);
    let reopened = pin_index(&root, &key(1), 4096).unwrap().unwrap();
    assert_eq!(lease.path(), reopened.path());
    assert_eq!(index_fallback_dir().file_name().unwrap(), format!("rustgis-shp-index-{}", rustix::process::geteuid().as_raw()).as_str());
}

#[cfg(unix)]
#[test]
fn cache_unix_release_does_not_mutate_a_root_that_became_unsafe() {
    use std::os::unix::fs::PermissionsExt;
    let root = Folder::new();
    let lease = ready(&root.0, 1, 40, 20);
    let path = lease.path().to_path_buf();
    std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o777)).unwrap();
    drop(lease);
    assert!(path.exists(), "Drop must validate the same boundary as checkout");
    std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    maintain(&root.0, 20).unwrap();
    assert!(!path.exists());
}

#[cfg(unix)]
#[test]
fn cache_unix_symlink_roots_and_lock_files_are_rejected() {
    let folder = Folder::new();
    let other = Folder::new();
    let root = folder.0.join("linked");
    std::os::unix::fs::symlink(&other.0, &root).unwrap();
    for alias in [root.clone(), root.join("."), PathBuf::from(format!("{}/", root.display()))] {
        assert!(directory_lock(&alias).is_err(), "symlink root accepted: {}", alias.display());
    }
    assert!(!other.0.join(".rustgis-cache-v1.lock").exists());
    let sentinel = other.0.join("sentinel");
    std::fs::write(&sentinel, b"unchanged").unwrap();
    std::os::unix::fs::symlink(&sentinel, folder.0.join(".rustgis-cache-v1.lock")).unwrap();
    assert!(directory_lock(&folder.0).is_err());
    assert_eq!(std::fs::read(sentinel).unwrap(), b"unchanged");
}

#[cfg(unix)]
#[test]
fn cache_unix_writable_ancestors_cannot_redirect_private_roots() {
    use std::os::unix::fs::PermissionsExt;
    let folder = Folder::new();
    let root = folder.0.join("private");
    std::fs::set_permissions(&folder.0, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(directory_lock(&root).is_err());
    assert!(!root.join(".rustgis-cache-v1.lock").exists());
    std::fs::set_permissions(&folder.0, std::fs::Permissions::from_mode(0o1777)).unwrap();
    drop(directory_lock(&root).unwrap());
    std::fs::set_permissions(&folder.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    drop(directory_lock(&root).unwrap());
}

#[cfg(unix)]
#[test]
fn cache_unix_foreign_owned_roots_are_rejected() {
    let uid = rustix::process::geteuid();
    if uid.as_raw() != 0 {
        assert_eq!(validate_directory(Path::new("/")).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    } else {
        let root = Folder::new();
        rustix::fs::chown(&root.0, Some(rustix::process::Uid::from_raw(65534)), None).unwrap();
        assert_eq!(directory_lock(&root.0).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(!root.0.join(".rustgis-cache-v1.lock").exists());
    }
}

/// A hidden, mutable intermediate link must not redirect a pinned SQLite pathname.
#[cfg(unix)]
#[test]
fn cache_unix_ancestor_symlink_retargeting_cannot_redirect_a_lease() {
    let folder = Folder::new();
    let original = Folder::new();
    let replacement = Folder::new();
    let bridge = folder.0.join("bridge");
    let alias = folder.0.join("alias");
    std::os::unix::fs::symlink(&original.0, &bridge).unwrap();
    std::os::unix::fs::symlink(&bridge, &alias).unwrap();
    let root = alias.join("index");
    let reader = ready(&root, 1, 10, 4096);
    let pinned = pin_index(&root, &key(1), 4096).unwrap().unwrap();
    let replacement_root = replacement.0.join("index");
    std::fs::create_dir(&replacement_root).unwrap();
    let forged = replacement_root.join(reader.path().file_name().unwrap());
    std::fs::write(forged, b"forged cache").unwrap();
    std::fs::remove_file(&bridge).unwrap();
    std::os::unix::fs::symlink(&replacement.0, &bridge).unwrap();
    assert_eq!(std::fs::read(reader.path()).unwrap(), vec![0; 10]);
    assert_eq!(std::fs::read(pinned.path()).unwrap(), vec![0; 10]);
    assert_eq!(reader.path().parent().unwrap(), original.0.join("index").canonicalize().unwrap());
    drop(pinned);
    drop(reader);
    maintain(&original.0.join("index"), 0).unwrap();
}
