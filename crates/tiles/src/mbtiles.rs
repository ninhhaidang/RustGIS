//! MBTiles archives as tile layers: a tile's address is `mbtiles:{z}/{x}/{y}@<archive>`, the
//! archive being a file path on the desktop (read through SQLite on the fetch workers, a tile at a
//! time) or `upload:<name>`, a file read into memory with [`register`] (a browser upload).
//!
//! The reading itself is `rustgis_data::mbtiles`; this is addresses, the archives in use and
//! answering the cache.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rustgis_data::mbtiles::{Archive, Info};
use web_time::{Duration, Instant};

use crate::cache::Done;

/// The scheme of a tile address read from an MBTiles archive.
pub const SCHEME: &str = "mbtiles:";
/// The prefix of an archive held in memory.
pub const UPLOAD: &str = "upload:";
/// Archives held in memory at most.
const MAX_UPLOADS: usize = 16;
/// A file that couldn't be read is tried again after this long.
const RETRY: Duration = Duration::from_secs(60);
/// Counts clears and per-file forgets, so a completion can tell whether one happened since it started.
static EPOCH: AtomicU64 = AtomicU64::new(0);
/// The epoch of the last clear of every file's metadata.
static CLEARED: AtomicU64 = AtomicU64::new(0);
/// Files [`forgotten`] keeps at most; their paths come from command parameters.
pub(crate) const MAX_FORGOTTEN: usize = 64;
/// The epoch [`forgotten`] was last emptied at.
static EMPTIED: AtomicU64 = AtomicU64::new(0);
/// A pending OS read stops being presented as normal loading after this long.
#[cfg(not(target_arch = "wasm32"))]
const READ_DEADLINE: Duration = Duration::from_secs(30);

/// The template for an archive's tiles.
pub fn template(archive: &str) -> String {
    format!("{SCHEME}{{z}}/{{x}}/{{y}}@{archive}")
}

/// Whether an address is a tile read from an MBTiles archive.
pub fn is_tile(url: &str) -> bool {
    url.starts_with(SCHEME)
}

/// Whether an archive address names an archive held in memory.
pub fn is_upload(archive: &str) -> bool {
    archive.starts_with(UPLOAD)
}

/// Whether an archive may be read: one held in memory, or the full path of an `.mbtiles` file on
/// this computer (not a network share).
pub fn check(archive: &str) -> Result<(), String> {
    if let Some(name) = archive.strip_prefix(UPLOAD) {
        return if name.is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
            Err("an uploaded MBTiles file needs a name".into())
        } else {
            Ok(())
        };
    }
    let p = archive.strip_prefix("file://").unwrap_or(archive);
    let drive = p.as_bytes().get(1) == Some(&b':') && p.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    let share = p.starts_with("\\\\") || p.starts_with("//") || (archive.starts_with("file://") && !p.starts_with('/'));
    if share || !(p.starts_with('/') || drive) || p.chars().any(char::is_control) || p.len() > 4096 {
        return Err(format!("'{p}': an MBTiles file is given by its full path on this computer"));
    }
    if !p.to_ascii_lowercase().ends_with(".mbtiles") {
        return Err(format!("'{p}' isn't an .mbtiles file"));
    }
    Ok(())
}

fn uploads() -> &'static Mutex<Vec<(String, Arc<Archive>)>> {
    static U: OnceLock<Mutex<Vec<(String, Arc<Archive>)>>> = OnceLock::new();
    U.get_or_init(Mutex::default)
}

/// Reads an MBTiles file's bytes into memory as `upload:<name>`, and returns that address. The
/// archive replaces one of the same name; the oldest of too many is forgotten.
pub fn register(name: &str, bytes: &[u8]) -> Result<String, String> {
    let archive = rustgis_data::mbtiles::read_bytes(bytes).map_err(|e| format!("{name}: {e}"))?;
    let address = format!("{UPLOAD}{}", name.chars().filter(|c| !c.is_control()).take(512).collect::<String>());
    check(&address)?;
    let mut all = uploads().lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(a, _)| *a != address);
    if all.len() >= MAX_UPLOADS {
        all.remove(0);
    }
    all.push((address.clone(), Arc::new(archive)));
    drop(all);
    // Tiles of an earlier upload of the same name are stale.
    crate::source::forget();
    Ok(address)
}

fn upload(archive: &str) -> Option<Arc<Archive>> {
    uploads().lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(a, _)| a == archive).map(|(_, a)| a.clone())
}

fn infos() -> &'static Mutex<HashMap<String, Result<Arc<Info>, (String, Instant)>>> {
    static I: OnceLock<Mutex<HashMap<String, Result<Arc<Info>, (String, Instant)>>>> = OnceLock::new();
    I.get_or_init(Mutex::default)
}

/// When each file was last forgotten by itself. Taken after [`infos`] and the cache's lock where
/// they are held. Emptied when full ([`forget_archive`]): reads asked for before that are read again.
fn forgotten() -> &'static Mutex<HashMap<String, u64>> {
    static F: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    F.get_or_init(Mutex::default)
}

/// The epoch now, to tell later whether a file was forgotten since ([`forgotten_since`]).
pub(crate) fn epoch() -> u64 {
    EPOCH.load(Ordering::SeqCst)
}

/// Whether a tile address is of a file forgotten after `epoch`.
pub(crate) fn forgotten_since(url: &str, epoch: u64) -> bool {
    parse_url(url)
        .is_some_and(|(_, _, _, archive)| forgotten().lock().unwrap_or_else(|e| e.into_inner()).get(archive).is_some_and(|at| *at > epoch))
}

/// How many files [`forgotten`] holds.
#[cfg(test)]
pub(crate) fn forgotten_len() -> usize {
    forgotten().lock().unwrap_or_else(|e| e.into_inner()).len()
}

/// Whether a tile address was asked for at `epoch`, before [`forgotten`] last lost track of what it held.
pub(crate) fn asked_before_emptied(url: &str, epoch: u64) -> bool {
    is_tile(url) && epoch < EMPTIED.load(Ordering::SeqCst)
}

/// Forgets the files' metadata (with the tile cache), so they are read again.
pub(crate) fn forget() {
    let mut all = infos().lock().unwrap_or_else(|e| e.into_inner());
    CLEARED.store(EPOCH.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
    all.clear();
    drop(all);
    #[cfg(not(target_arch = "wasm32"))]
    rustgis_data::mbtiles::close_all();
}

/// Forgets what was read of one file (its metadata and its connections) with every file's resolved
/// styles, made again when next drawn, and won't keep a read of it under way: the file may be
/// replaced next. Reads of other files are kept, as they may be framing a map, until the record of
/// forgotten files fills up ([`MAX_FORGOTTEN`]).
/// [`cache::forget`](crate::cache::forget) asks this of any address, so others are left alone.
pub(crate) fn forget_archive(archive: &str) {
    if check(archive).is_err() {
        return;
    }
    let mut all = infos().lock().unwrap_or_else(|e| e.into_inner());
    let epoch = EPOCH.fetch_add(1, Ordering::SeqCst) + 1;
    let mut gone = forgotten().lock().unwrap_or_else(|e| e.into_inner());
    if gone.len() >= MAX_FORGOTTEN && !gone.contains_key(archive) {
        gone.clear();
        EMPTIED.store(epoch, Ordering::SeqCst);
        CLEARED.store(epoch, Ordering::SeqCst);
    }
    gone.insert(archive.to_string(), epoch);
    drop(gone);
    all.remove(archive);
    drop(all);
    crate::source::forget();
    #[cfg(not(target_arch = "wasm32"))]
    rustgis_data::mbtiles::close(file_of(archive));
}

/// An archive's metadata.
pub fn info(archive: &str) -> Result<Arc<Info>, String> {
    crate::cache::check_still(&crate::cache::current_still())?;
    check(archive)?;
    if is_upload(archive) {
        return upload(archive)
            .map(|a| Arc::new(a.info.clone()))
            .ok_or_else(|| format!("{}: add the MBTiles file again (uploads aren't kept)", archive.trim_start_matches(UPLOAD)));
    }
    if let Some(result) = cached_info(archive) {
        return result;
    }
    let epoch = EPOCH.load(Ordering::SeqCst);
    let r = read_info(archive);
    crate::cache::check_still(&crate::cache::current_still())?;
    remember_info(archive, &r, epoch);
    r
}

/// Keeps a completion only if neither its file nor every file was forgotten since its read started
/// (a full clear, or the record of forgotten files emptied); check and insert share the same lock
/// as the clear, so an old completion cannot repopulate the new cache.
fn remember_info(archive: &str, result: &Result<Arc<Info>, String>, epoch: u64) {
    if crate::cache::check_still(&crate::cache::current_still()).is_err() {
        return;
    }
    let mut all = infos().lock().unwrap_or_else(|e| e.into_inner());
    let forgotten_at = forgotten().lock().unwrap_or_else(|e| e.into_inner()).get(archive).copied().unwrap_or(0);
    if epoch < CLEARED.load(Ordering::SeqCst).max(forgotten_at) {
        return;
    }
    if all.len() >= 64 {
        all.clear();
    }
    all.insert(archive.to_string(), result.clone().map_err(|e| (e, Instant::now())));
}

/// An existing read has its own deadline. An unadmitted read reports saturation once a worker
/// has stalled; workers keep their slots until the OS returns, never spawning unbounded threads.
#[cfg(not(target_arch = "wasm32"))]
fn metadata_stalled(reads: &HashMap<String, Instant>, archive: &str) -> bool {
    reads.get(archive).map_or_else(
        || reads.len() >= 8 && reads.values().any(|start| start.elapsed() >= READ_DEADLINE),
        |start| start.elapsed() >= READ_DEADLINE,
    )
}

/// A caller's wait cannot outlive the pending read's remaining deadline.
#[cfg(not(target_arch = "wasm32"))]
fn metadata_wait(start: Instant, timeout: Duration) -> Duration {
    timeout.min(READ_DEADLINE.saturating_sub(start.elapsed()))
}

/// Cached metadata, retaining failed reads only for the retry interval.
fn cached_info(archive: &str) -> Option<Result<Arc<Info>, String>> {
    infos().lock().unwrap_or_else(|e| e.into_inner()).get(archive).and_then(|r| match r {
        Ok(info) => Some(Ok(info.clone())),
        Err((error, at)) if at.elapsed() < RETRY => Some(Err(error.clone())),
        Err(_) => None,
    })
}

/// The metadata workers' shared pending reads and completion signal.
#[cfg(not(target_arch = "wasm32"))]
fn metadata_reads() -> &'static (Mutex<HashMap<String, Instant>>, std::sync::Condvar) {
    static LOADING: OnceLock<(Mutex<HashMap<String, Instant>>, std::sync::Condvar)> = OnceLock::new();
    LOADING.get_or_init(|| (Mutex::new(HashMap::new()), std::sync::Condvar::new()))
}

/// Metadata without file I/O on the caller: local reads run in at most eight workers. A caller
/// may wait up to `timeout`; otherwise `None` means pending. Completion wakes the map. Uploads
/// are already in memory. At capacity a read remains pending until a later poll can start it.
/// A read still pending after 30 seconds reports an error. A stalled OS read keeps its worker
/// slot until it answers; those slots are never recycled into more potentially blocked threads.
pub fn info_ready(archive: &str, timeout: Duration) -> Result<Option<Arc<Info>>, String> {
    crate::cache::check_still(&crate::cache::current_still())?;
    check(archive)?;
    if is_upload(archive) {
        return info(archive).map(Some);
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = timeout;
        info(archive).map(Some)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let (pending, ready) = metadata_reads();
        let mut all = pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(result) = cached_info(archive) {
            return result.map(Some);
        }
        if metadata_stalled(&all, archive) {
            return Err(format!(
                "{archive}: MBTiles metadata read exceeded 30 seconds or all eight workers are occupied by stalled storage; retry after the storage responds"
            ));
        }
        if !all.contains_key(archive) {
            if all.len() >= 8 {
                return Ok(None);
            }
            all.insert(archive.to_string(), Instant::now());
            let address = archive.to_string();
            let still = crate::cache::current_still();
            let spawned = std::thread::Builder::new().name("rustgis-mbtiles-info".into()).spawn(move || {
                crate::cache::with_optional_still(still, || {
                    let _ = info(&address);
                });
                pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&address);
                ready.notify_all();
                crate::cache::notify();
            });
            if let Err(error) = spawned {
                all.remove(archive);
                return Err(format!("could not start an MBTiles metadata read: {error}"));
            }
        }
        if !timeout.is_zero() {
            let wait = all.get(archive).map_or(Duration::ZERO, |start| metadata_wait(*start, timeout));
            let (reads, _) = ready.wait_timeout_while(all, wait, |all| all.contains_key(archive)).unwrap_or_else(|e| e.into_inner());
            all = reads;
        }
        drop(all);
        crate::cache::check_still(&crate::cache::current_still())?;
        let all = pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(result) = cached_info(archive) {
            return result.map(Some);
        }
        if metadata_stalled(&all, archive) {
            return Err(format!("{archive}: MBTiles metadata read exceeded 30 seconds; retry after the storage responds"));
        }
        Ok(None)
    }
}

/// The file an archive address names.
#[cfg(not(target_arch = "wasm32"))]
fn file_of(archive: &str) -> &std::path::Path {
    std::path::Path::new(archive.strip_prefix("file://").unwrap_or(archive))
}

/// Closes the connections kept to files none of `archives` names, so another program can move or
/// delete a file once no layer reads it.
#[cfg(not(target_arch = "wasm32"))]
pub fn close_unused<'a>(archives: impl Iterator<Item = &'a str>) {
    let kept: Vec<&std::path::Path> = archives.map(file_of).collect();
    rustgis_data::mbtiles::close_unless(|file| kept.contains(&file));
}

#[cfg(not(target_arch = "wasm32"))]
fn read_info(archive: &str) -> Result<Arc<Info>, String> {
    rustgis_data::mbtiles::open_path(file_of(archive)).map(Arc::new)
}

#[cfg(target_arch = "wasm32")]
fn read_info(archive: &str) -> Result<Arc<Info>, String> {
    Err(format!("{archive}: the web app reads MBTiles files added to it (Add Data), not paths"))
}

/// `mbtiles:z/x/y@archive` → its parts.
fn parse_url(url: &str) -> Option<(u8, u32, u32, &str)> {
    let rest = url.strip_prefix(SCHEME)?;
    let (zxy, archive) = rest.split_once('@')?;
    let mut p = zxy.split('/');
    let z: u8 = p.next()?.parse().ok()?;
    let x: u32 = p.next()?.parse().ok()?;
    let y: u32 = p.next()?.parse().ok()?;
    (z <= 30 && p.next().is_none() && !archive.is_empty()).then_some((z, x, y, archive))
}

/// Fetches a tile address: its bytes, or none for a tile the archive doesn't have. A file's tile
/// is read on the fetch workers, never on the frame.
pub(crate) fn fetch(url: &str, done: Done) {
    let Some((z, x, y, archive)) = parse_url(url) else {
        done(Err(format!("{url}: not an MBTiles tile address")));
        return;
    };
    if let Err(e) = check(archive) {
        done(Err(e));
        return;
    }
    if is_upload(archive) {
        match upload(archive) {
            Some(a) => done(Ok(a.tile(z, x, y).map(|t| t.to_vec()).unwrap_or_default())),
            None => done(Err(format!("{archive}: no such upload"))),
        }
        return;
    }
    crate::cache::fetch_worker(url.to_string(), done);
}

/// Reads a tile from a file (on a fetch worker).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_local(url: &str) -> Result<Vec<u8>, String> {
    let (z, x, y, archive) = parse_url(url).ok_or_else(|| format!("{url}: not an MBTiles tile address"))?;
    check(archive)?;
    Ok(rustgis_data::mbtiles::tile_path(file_of(archive), z, x, y)?.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reentrant authority check may clear metadata; the old epoch must still reject its result.
    #[test]
    fn metadata_completion_checks_still_outside_the_cache_lock() {
        let _turn = crate::cache::turn();
        let available = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = available.clone();
        let still: crate::cache::Still = Arc::new(move || {
            let free = infos().try_lock().is_ok();
            seen.store(free, Ordering::SeqCst);
            if free {
                forget();
            }
            true
        });
        let epoch = EPOCH.load(Ordering::SeqCst);
        let address = "/reentrant-cache-completion.mbtiles";
        crate::cache::with_still(still, || remember_info(address, &Err("old completion".into()), epoch));
        assert!(available.load(Ordering::SeqCst), "Still ran with the metadata cache locked");
        assert!(cached_info(address).is_none(), "a clear during the predicate invalidates the earlier completion");
    }

    /// Both preflight and post-wait authority checks permit metadata reentry without a lock cycle.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn metadata_ready_checks_still_outside_the_pending_lock() {
        let _turn = crate::cache::turn();
        let origin = std::thread::current().id();
        let available = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (free, count) = (available.clone(), checks.clone());
        let still: crate::cache::Still = Arc::new(move || {
            if std::thread::current().id() != origin {
                return false;
            }
            count.fetch_add(1, Ordering::SeqCst);
            free.fetch_and(metadata_reads().0.try_lock().is_ok(), Ordering::SeqCst);
            true
        });
        let _ = crate::cache::with_still(still, || info_ready("/reentrant-pending-read.mbtiles", Duration::ZERO));
        assert!(checks.load(Ordering::SeqCst) >= 2, "the post-wait check must execute");
        assert!(available.load(Ordering::SeqCst), "Still ran with pending metadata reads locked");
    }

    /// The metadata worker inherits its initiating transport context instead of reading at root.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn metadata_worker_keeps_still_and_drops_revoked_completion() {
        let _turn = crate::cache::turn();
        let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker = checks.clone();
        let still: crate::cache::Still = Arc::new(move || {
            if std::thread::current().name() == Some("rustgis-mbtiles-info") {
                worker.fetch_add(1, Ordering::SeqCst);
                return false;
            }
            true
        });
        let address = "/revoked-metadata-worker.mbtiles";
        let got = crate::cache::with_still(still, || info_ready(address, Duration::from_secs(10)));
        assert!(matches!(got, Ok(None)), "revoked work must not populate metadata");
        assert!(checks.load(Ordering::SeqCst) > 0, "the real worker checked the inherited context");
        assert!(cached_info(address).is_none());
        assert!(crate::cache::current_still().is_none());
    }

    #[test]
    fn cleared_metadata_rejects_an_older_completion() {
        let _turn = crate::cache::turn();
        let epoch = EPOCH.load(Ordering::SeqCst);
        forget();
        remember_info("/old-completion.mbtiles", &Err("obsolete read".into()), epoch);
        assert!(cached_info("/old-completion.mbtiles").is_none());
        remember_info("/fresh-completion.mbtiles", &Err("current read".into()), EPOCH.load(Ordering::SeqCst));
        assert!(cached_info("/fresh-completion.mbtiles").is_some());
    }

    /// The metadata of a read under way when its file was forgotten isn't kept; a read of another
    /// file is, and an address that is no MBTiles file changes nothing.
    #[test]
    fn a_forgotten_file_rejects_only_its_own_older_completion() {
        let _turn = crate::cache::turn();
        let epoch = EPOCH.load(Ordering::SeqCst);
        forget_archive("/forgotten-while-read.mbtiles");
        remember_info("/forgotten-while-read.mbtiles", &Err("old file".into()), epoch);
        assert!(cached_info("/forgotten-while-read.mbtiles").is_none());
        // Tried several times: another test's clear may fall in between, but not every time.
        assert!((0..100).any(|_| {
            let epoch = EPOCH.load(Ordering::SeqCst);
            forget_archive("/forgotten-while-read.mbtiles");
            forget_archive("https://example.com/not-an-archive");
            remember_info("/read-meanwhile.mbtiles", &Err("another file".into()), epoch);
            cached_info("/read-meanwhile.mbtiles").is_some()
        }));
        assert!(!forgotten().lock().unwrap_or_else(|e| e.into_inner()).contains_key("https://example.com/not-an-archive"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn stalled_metadata_reports_errors_without_recycling_worker_slots() {
        let old = Instant::now().checked_sub(READ_DEADLINE + Duration::from_secs(1)).expect("an earlier instant");
        let mut reads: HashMap<String, Instant> = (0..8).map(|i| (format!("/{i}.mbtiles"), Instant::now())).collect();
        assert!(!metadata_stalled(&reads, "/next.mbtiles"), "busy workers initially remain pending");
        assert!(metadata_wait(Instant::now(), Duration::from_secs(60)) <= READ_DEADLINE);
        assert_eq!(metadata_wait(old, Duration::from_secs(60)), Duration::ZERO);
        reads.insert("/0.mbtiles".into(), old);
        assert!(metadata_stalled(&reads, "/0.mbtiles"), "the stalled read reports its deadline");
        assert!(metadata_stalled(&reads, "/next.mbtiles"), "a saturated stalled pool is an error, not endless loading");
        assert!(!metadata_stalled(&reads, "/1.mbtiles"), "a healthy admitted read retains its own deadline");
        reads.remove("/0.mbtiles");
        assert!(!metadata_stalled(&reads, "/next.mbtiles"), "capacity can recover when the OS returns");
    }

    #[test]
    fn addresses() {
        assert_eq!(parse_url("mbtiles:3/1/2@/data/a.mbtiles"), Some((3, 1, 2, "/data/a.mbtiles")));
        assert_eq!(parse_url("mbtiles:3/1/2@upload:a b.mbtiles"), Some((3, 1, 2, "upload:a b.mbtiles")));
        for bad in
            ["mbtiles:3/1@/a.mbtiles", "mbtiles:99/1/2@/a.mbtiles", "mbtiles:1/2/3/4@/a.mbtiles", "mbtiles:1/2/3@", "pmtiles:1/2/3@/a"]
        {
            assert_eq!(parse_url(bad), None, "{bad}");
        }
        assert!(check("/data/a.mbtiles").is_ok());
        assert!(check("C:\\data\\a.MBTiles").is_ok());
        assert!(check("upload:a.mbtiles").is_ok());
        for bad in
            ["a.mbtiles", "/data/a.pmtiles", "\\\\server\\share\\a.mbtiles", "file://server/a.mbtiles", "upload:", "https://x/a.mbtiles"]
        {
            assert!(check(bad).is_err(), "{bad}");
        }
        assert_eq!(template("upload:x"), "mbtiles:{z}/{x}/{y}@upload:x");
    }

    #[test]
    fn uploads_answer_tiles() {
        assert!(register("junk.mbtiles", b"nope").is_err());
        assert!(info("upload:never.mbtiles").is_err());
        let (tx, rx) = std::sync::mpsc::channel();
        fetch("mbtiles:0/0/0@upload:never.mbtiles", Box::new(move |r| tx.send(r.is_err()).unwrap_or_default()));
        assert_eq!(rx.recv().ok(), Some(true));
    }
}
