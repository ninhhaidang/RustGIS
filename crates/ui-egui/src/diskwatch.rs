//! Changed on disk (#1477): the desktop watches the files its layers and local tile archives read,
//! every few seconds and when the window regains focus, and offers Reload Data for a source that
//! changed. Only metadata is read (`rustgis_engine::observe`): scans on a thread of their own, a
//! reload, save or export's own look on the thread its read or write runs on. Reloading is
//! always the user's choice and always goes through `ui.reloadData` or `map.reloadTiles`.
//!
//! [`Monitor`] is the state machine, driven by observations and nothing else; [`Watch`] finds the
//! targets, runs the scans and shows what the monitor says. Monitor state is the app's alone: it
//! never dirties the project or adds an undo step.
use std::collections::{HashMap, HashSet};

use rustgis_engine::Session;
use rustgis_engine::data::sync::FileSync;
use rustgis_engine::observe::Observation;
use serde_json::{Value, json};

use crate::RustGisApp;
use crate::theme::Tokens;

/// How often the files are looked at while the app is open.
const EVERY: web_time::Duration = web_time::Duration::from_secs(5);
/// How long a scan may go unanswered before the user is told checks are waiting.
const STALLED: web_time::Duration = web_time::Duration::from_secs(30);

/// What one scan found at a source's linked path.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Seen {
    Present(Observation),
    Missing,
    /// Its metadata couldn't be read (permissions, a locked or odd file).
    Unreadable(String),
    /// A folder (a Zarr store, a File Geodatabase): not monitored, its timestamp says nothing of
    /// the files inside. Reload Data reads it on request.
    Folder,
}

fn look(path: &str) -> Seen {
    if std::path::Path::new(path).is_dir() {
        return Seen::Folder;
    }
    match Observation::source(path) {
        Ok(o) if o.missing() => Seen::Missing,
        Ok(o) => Seen::Present(o),
        Err(e) => Seen::Unreadable(e),
    }
}

/// A reload target: a layer, or a local archive, in one map of the open project. Layer ids are
/// map-local and reused, so a layer is known by its instance and its map's; an archive by its
/// path and which of the map's archives with that path it is (`nth`, from the top).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Target {
    Layer { map: u64, layer: u64 },
    Tiles { map: u64, url: String, nth: usize },
}

/// What the app shows for a target.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Status {
    Unchanged,
    Changed,
    Missing,
    Unreadable(String),
    /// A folder source: Reload Data only, said in the layer's details.
    Folder,
}

#[derive(Default)]
struct Source {
    /// The latest scan and its number.
    last: Option<(Seen, u64)>,
    /// What two consecutive scans agreed on: a write still under way isn't offered.
    settled: Option<Seen>,
    /// RustGIS wrote the file itself: what was there just before, the number of the first scan
    /// that started after the write, what was there just after it (RustGIS's result), and the
    /// targets whose data the write left behind (another layer on the table it wrote).
    wrote: Option<(Seen, u64, Seen, HashSet<Target>)>,
    /// The target that write left in step whatever its baseline: a publish compares the file's
    /// contents with its record before writing, so a change of metadata alone before it (the same
    /// bytes written again) doesn't keep the publishing layer marked.
    verified: Option<Target>,
}

struct Tracked {
    /// The coalescing key of the linked path; a repair changes it, and the target starts over.
    key: String,
    /// The observation the target's data was read with, as far as the app knows.
    baseline: Option<Seen>,
    /// Not Now: the observation it was given for. A newer one notifies again.
    dismissed: Option<Seen>,
    /// The first scan that may give it a baseline: after [`Monitor::rebaseline`], a scan that
    /// started before (and may have seen the file before the change) doesn't.
    from_scan: u64,
}

/// Change detection for every target, by observations alone (no clock, no files).
#[derive(Default)]
pub(crate) struct Monitor {
    sources: HashMap<String, Source>,
    targets: HashMap<Target, Tracked>,
    /// Each reload read under way and what its file held as it started.
    reading: HashMap<Target, Seen>,
}

impl Monitor {
    /// Follows the project's targets now (each with its source's key): new ones start with no
    /// baseline, gone ones are forgotten, one whose path changed starts over. `true` when a
    /// target has nothing to compare with yet, so a scan is wanted now.
    pub(crate) fn track(&mut self, found: impl IntoIterator<Item = (Target, String)>) -> bool {
        let mut seen = HashSet::new();
        for (target, key) in found {
            match self.targets.get_mut(&target) {
                Some(t) if t.key == key => {}
                _ => {
                    self.targets.insert(target.clone(), Tracked { key: key.clone(), baseline: None, dismissed: None, from_scan: 0 });
                }
            }
            seen.insert(target);
        }
        self.targets.retain(|target, _| seen.contains(target));
        let keys: HashSet<&String> = self.targets.values().map(|t| &t.key).collect();
        self.sources.retain(|key, _| keys.contains(key));
        self.targets.values().any(|t| t.baseline.is_none())
    }

    /// Forgets what `target` was read with, so the first readable scan numbered `next_scan` or
    /// later is its baseline: its data was brought in step with its file some other way, and a
    /// scan already under way may have looked before that. `true`, as a scan is then wanted.
    /// An earlier publish's acknowledgment still pending for it no longer applies: what it wrote
    /// was replaced.
    pub(crate) fn rebaseline(&mut self, target: &Target, next_scan: u64) -> bool {
        if let Some(t) = self.targets.get_mut(target) {
            t.baseline = None;
            t.dismissed = None;
            t.from_scan = next_scan;
            if let Some(source) = self.sources.get_mut(&t.key)
                && source.verified.as_ref() == Some(target)
            {
                source.verified = None;
            }
        }
        true
    }

    /// Whether a target reads `key`.
    pub(crate) fn watches(&self, key: &str) -> bool {
        self.targets.values().any(|t| t.key == key)
    }

    /// The keys to scan.
    #[cfg(test)]
    pub(crate) fn keys(&self) -> HashSet<String> {
        self.targets.values().map(|t| t.key.clone()).collect()
    }

    /// Gives a target the observation its data was read with.
    #[cfg(test)]
    pub(crate) fn baseline(&mut self, target: &Target, seen: Seen) {
        if let Some(t) = self.targets.get_mut(target) {
            t.baseline = Some(seen);
        }
    }

    /// Scan number `scan` saw `seen` at `key`.
    pub(crate) fn observed(&mut self, key: &str, seen: Seen, scan: u64) {
        if !self.targets.values().any(|t| t.key == key) {
            return;
        }
        let source = self.sources.entry(key.to_string()).or_default();
        let agreed = source.last.as_ref().is_some_and(|(last, _)| *last == seen);
        source.last = Some((seen.clone(), scan));
        if agreed {
            source.settled = Some(seen.clone());
        }
        // Self-write: acknowledged once two scans agree, the later started after it (the earlier,
        // perhaps in flight across the write, saw the same, so it is the write's result too).
        let acknowledged = match &source.wrote {
            Some((before, after, result, stale)) if agreed && scan >= *after => Some((before.clone(), result.clone(), stale.clone())),
            _ => None,
        };
        let verified = if acknowledged.is_some() {
            source.wrote = None;
            source.verified.take()
        } else {
            None
        };
        for (target, t) in self.targets.iter_mut().filter(|(_, t)| t.key == key) {
            // A target added since: the first readable scan is what it was read with (a look
            // that couldn't read the metadata says nothing about what the layer holds).
            if t.baseline.is_none() && !matches!(seen, Seen::Unreadable(_)) && scan >= t.from_scan {
                t.baseline = Some(seen.clone());
            }
            if let Some((before, result, stale)) = &acknowledged
                && (t.baseline.as_ref() == Some(before) || verified.as_ref() == Some(target))
                && !stale.contains(target)
            {
                t.baseline = Some(result.clone());
            }
            // Up to date again: a Not Now given for an earlier episode doesn't hide the next one.
            // (A file missing since it was first seen is not "up to date": its Not Now holds.)
            if agreed && matches!(seen, Seen::Present(_)) && t.baseline.as_ref() == Some(&seen) {
                t.dismissed = None;
            }
        }
    }

    fn settled(&self, t: &Tracked) -> Option<&Seen> {
        self.sources.get(&t.key).and_then(|s| s.settled.as_ref())
    }

    pub(crate) fn status(&self, target: &Target) -> Status {
        let Some(t) = self.targets.get(target) else { return Status::Unchanged };
        match self.settled(t) {
            Some(Seen::Missing) => Status::Missing,
            Some(Seen::Unreadable(e)) => Status::Unreadable(e.clone()),
            Some(Seen::Folder) => Status::Folder,
            Some(seen @ Seen::Present(_)) if t.baseline.as_ref().is_some_and(|b| b != seen) => Status::Changed,
            _ => Status::Unchanged,
        }
    }

    /// Whether the notification lists the target: it isn't up to date and Not Now wasn't
    /// chosen for what is there now.
    pub(crate) fn notifies(&self, target: &Target) -> bool {
        let Some(t) = self.targets.get(target) else { return false };
        !matches!(self.status(target), Status::Unchanged | Status::Folder) && t.dismissed.as_ref() != self.settled(t)
    }

    /// Not Now: the target stays marked, the notification leaves it until something newer.
    pub(crate) fn dismiss(&mut self, target: &Target) {
        let settled = self.targets.get(target).and_then(|t| self.settled(t)).cloned();
        if let Some(t) = self.targets.get_mut(target) {
            t.dismissed = settled;
        }
    }

    /// A reload's read starts, its file holding `seen` (looked at just now).
    pub(crate) fn reading(&mut self, target: Target, seen: Seen) {
        self.reading.insert(target, seen);
    }

    /// A read started for `target` ended without a result (it failed, or never ran).
    pub(crate) fn not_read(&mut self, target: &Target) {
        self.reading.remove(target);
    }

    /// A reload succeeded: what its file held as its read started ([`Monitor::reading`]) is
    /// acknowledged, so a change that came while it read stays pending. A reload with no such
    /// observation (run some other way) leaves the mark as it is: what it read isn't known.
    pub(crate) fn reloaded(&mut self, target: &Target) {
        let used = self.reading.remove(target);
        if let Some(t) = self.targets.get_mut(target)
            && matches!(used, Some(Seen::Present(_)))
        {
            t.baseline = used;
            t.dismissed = None;
        }
    }

    /// RustGIS wrote `key` (a GeoPackage save, an export into it); `before` is what was there
    /// just before the write and `next_scan` the number the next scan will have. Only targets
    /// whose data matched `before` take `result` (what was there just after the write) as
    /// theirs, once two scans after the write agree; a change another program makes before or
    /// after the save then still differs from what they hold. `stale` are targets whose data
    /// the write didn't refresh (another layer on the table it wrote): they stay marked.
    /// `writer` is the target whose data it did refresh (the saving layer), if any.
    pub(crate) fn wrote(&mut self, key: &str, before: Seen, result: Seen, stale: HashSet<Target>, writer: Option<&Target>, next_scan: u64) {
        if !self.watches(key) {
            return;
        }
        let source = self.sources.entry(key.to_string()).or_default();
        // A second write right after the first, with nothing between them, continues it; its
        // writer is refreshed by it, whatever the first left stale.
        let (before, stale) = match source.wrote.take() {
            Some((first_before, _, first_result, first_stale)) if first_result == before => {
                let carried = first_stale.into_iter().filter(|t| Some(t) != writer);
                (first_before, carried.chain(stale).collect())
            }
            _ => (before, stale),
        };
        source.wrote = Some((before, next_scan, result, stale));
        // Only a publish marks its writer verified, after this ([`Monitor::verified`]).
        source.verified = None;
    }

    /// The write just recorded ([`Monitor::wrote`]) checked `target`'s file against its record
    /// before writing it, so `target` takes the result whatever it held before.
    pub(crate) fn verified(&mut self, key: &str, target: &Target) {
        if let Some(source) = self.sources.get_mut(key).filter(|s| s.wrote.is_some()) {
            source.verified = Some(target.clone());
        }
    }
}

/// A path as the targets reading it are grouped: absolute, without resolving links (the linked
/// path itself is what is watched). On Linux and macOS `..` is kept: after a link to a folder it
/// leads somewhere a lexical step back wouldn't, so two spellings of one file may be watched as
/// two. (Windows resolves `..` lexically, and so does `absolute` there.)
pub(crate) fn key(path: &str) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.into());
    // Rebuilt from its components: a `.` step is dropped.
    absolute.components().collect::<std::path::PathBuf>().display().to_string()
}

/// One target as the project has it now.
struct Found {
    target: Target,
    key: String,
    path: String,
    /// The table or dataset in its container that it reads, when there is one.
    table: Option<String>,
    map: String,
    name: String,
    /// Repair Data Source can relink it (an editable GeoPackage layer without a linked file can't).
    repairable: bool,
    /// Read in place: a RustGIS write into its GeoPackage table reads it again
    /// (`gpkg::reset_sources`), so that write leaves nothing stale on it.
    streamed: bool,
    /// For a layer read whole from a Shapefile or GeoJSON file it can publish to: the record of
    /// its files at the last sync. Reload Data can't read such a layer again, so it's marked in
    /// the Layers pane and not offered in the notification.
    record: Option<FileSync>,
}

/// The file a layer read whole from a local Shapefile or GeoJSON publishes to, and its record
/// (`source.publish`): held in memory, not read in place.
fn published_file(l: &rustgis_engine::map::Layer) -> Option<(&str, &rustgis_engine::data::sync::Synced)> {
    let synced = l.synced.as_ref()?;
    // As `source.publish` takes it: a network share isn't a file on this computer.
    let path = l.dataset.source.as_deref().filter(|p| rustgis_engine::cmd::publish::local_file(p))?;
    (l.source.is_none() && l.gpkg.is_none()).then_some((path, synced))
}

/// Whether the layer publishes to a file the Layers pane marks ([`published_file`]).
pub(crate) fn publishes(l: &rustgis_engine::map::Layer) -> bool {
    published_file(l).is_some()
}

/// The linked local sources in every map: layers Reload Data reads, layers that publish to their
/// file, and local archives. Remote, embedded and other in-memory data aren't targets; a converted (GDAL, DuckDB) layer is watched at the
/// original file, not the conversion.
fn targets(s: &Session) -> Vec<Found> {
    let mut out = Vec::new();
    for m in &s.project.maps {
        for l in &m.layers {
            let target = Target::Layer { map: m.instance_id(), layer: l.instance_id() };
            if let Some((path, table, _)) = crate::reloaddata::source(l) {
                let repairable = l.linked_file().is_some();
                out.push(Found {
                    target,
                    key: key(path),
                    path: path.to_string(),
                    table: table.map(str::to_string),
                    map: m.name.clone(),
                    name: l.name.clone(),
                    repairable,
                    streamed: l.gpkg.is_none() && l.source.is_some(),
                    record: None,
                });
            } else if let Some((path, synced)) = published_file(l) {
                out.push(Found {
                    target,
                    key: key(path),
                    path: path.to_string(),
                    table: None,
                    map: m.name.clone(),
                    name: l.name.clone(),
                    repairable: false,
                    streamed: false,
                    record: Some(synced.files.clone()),
                });
            }
        }
        let mut archives: HashMap<&str, usize> = HashMap::new();
        for tile in &m.tiles {
            if let Some(url) = tile.archive_file() {
                let nth = archives.entry(url).or_default();
                let path = url.strip_prefix("file://").unwrap_or(url);
                let target = Target::Tiles { map: m.instance_id(), url: url.to_string(), nth: *nth };
                *nth += 1;
                out.push(Found {
                    target,
                    key: key(path),
                    path: path.to_string(),
                    table: None,
                    map: m.name.clone(),
                    name: tile.name.clone(),
                    repairable: true,
                    streamed: false,
                    record: None,
                });
            }
        }
    }
    out
}

/// One scan's observations by key, and each published layer's file parts that differ from its
/// record by length or presence ([`FileSync::resized`]), with the record compared.
type Scanned = crate::Slot<(Vec<(String, Seen)>, Vec<(Target, FileSync, Vec<String>)>)>;

/// The app's side of the monitor: its targets, its one scan at a time and when the next is due.
#[derive(Default)]
pub struct Watch {
    monitor: Monitor,
    found: Vec<Found>,
    opened: u64,
    revision: Option<u64>,
    scans: u64,
    scan: Option<(u64, Scanned, web_time::Instant)>,
    due: Option<web_time::Instant>,
    wanted: bool,
    focused: bool,
    /// A scan couldn't start, or one hasn't answered for [`STALLED`]: said once each.
    said_failed: bool,
    said_stalled: bool,
    /// Each published layer's file parts the latest scan found resized against the record it
    /// compared with; it counts only while the layer still has that record.
    resized: HashMap<Target, (FileSync, Vec<String>)>,
}

impl Watch {
    fn status(&self, target: &Target) -> Status {
        self.monitor.status(target)
    }
}

/// What the Layers pane says of a layer of the active map. A layer that publishes to its file is
/// changed, too, when its files' lengths or presence differ from its record: that catches a change
/// made while the project was closed, which the first scan takes as the baseline.
pub(crate) fn layer_status(app: &RustGisApp, layer: &rustgis_engine::map::Layer) -> Status {
    let target = Target::Layer { map: app.session.map().instance_id(), layer: layer.instance_id() };
    let status = app.diskwatch.status(&target);
    let resized = app
        .diskwatch
        .resized
        .get(&target)
        .is_some_and(|(record, parts)| !parts.is_empty() && layer.synced.as_ref().is_some_and(|s| s.files == *record));
    if status == Status::Unchanged && resized { Status::Changed } else { status }
}

/// Whether Reload Data can read the layer again; a layer that publishes to its file can't.
pub(crate) fn reloadable(layer: &rustgis_engine::map::Layer) -> bool {
    crate::reloaddata::source(layer).is_some()
}

/// The target of tile layer `i` of `map`: its archive and which of the map's archives with that
/// path it is.
fn tiles_target(map: &rustgis_engine::map::Map, i: usize) -> Option<Target> {
    let url = map.tiles.get(i)?.archive_file()?;
    let nth = map.tiles.iter().take(i).filter(|t| t.archive_file() == Some(url)).count();
    Some(Target::Tiles { map: map.instance_id(), url: url.to_string(), nth })
}

/// What the Layers pane says of tile layer `i` of the active map.
pub(crate) fn tiles_status(app: &RustGisApp, i: usize) -> Status {
    match tiles_target(app.session.map(), i) {
        Some(target) => app.diskwatch.status(&target),
        None => Status::Unchanged,
    }
}

/// The Layers pane's words for a status: a note after the name (`""` for none), a line for its
/// details, and how the name is tinted: `Some(true)` red, `Some(false)` amber, `None` as it is.
/// `repairable`: Repair Data Source can relink it, as the notification offers.
pub(crate) fn row_note(status: &Status, repairable: bool) -> Option<(String, String, Option<bool>)> {
    match status {
        Status::Unchanged => None,
        Status::Changed => Some((
            tl!("(changed on disk)").to_string(),
            tl!("Changed on disk: the file may have changed since it was read. Reload Data reads it again.").to_string(),
            Some(false),
        )),
        Status::Missing => Some((
            tl!("(missing on disk)").to_string(),
            if repairable {
                tl!("The file is missing. Restore it, or use Repair Data Source.")
            } else {
                tl!("The file is missing. Restore it.")
            }
            .to_string(),
            Some(true),
        )),
        Status::Unreadable(e) => Some((
            tl!("(can't be checked)").to_string(),
            crate::i18n::fmt("The file can't be checked: {error}", &[("error", e)]),
            Some(true),
        )),
        Status::Folder => Some((String::new(), tl!("Folder source — changes are not monitored. Use Reload Data.").to_string(), None)),
    }
}

/// Each frame: follows the project's targets, lands a finished scan and starts one when due.
pub(crate) fn tick(app: &mut RustGisApp, ctx: &egui::Context) {
    // A file an export under way writes into is the export's work until it lands.
    let exporting = crate::exportjob::writing(app).map(String::from);
    let w = &mut app.diskwatch;
    if w.opened != app.session.opened {
        // Another project: nothing of the old one's scans applies.
        *w = Watch { opened: app.session.opened, focused: w.focused, ..Watch::default() };
    }
    if w.revision != Some(app.session.revision) {
        w.revision = Some(app.session.revision);
        let before = std::mem::take(&mut w.found);
        w.found = targets(&app.session);
        w.wanted |= w.monitor.track(w.found.iter().map(|f| (f.target.clone(), f.key.clone())));
        // A published layer whose record changed was published, however the command reached the
        // session (the assistant's relay and file bridge run it without this watcher's hooks): the
        // file now holds what it does, so the next scan is its baseline again.
        for f in &w.found {
            if let Some(record) = &f.record
                && before.iter().any(|b| b.target == f.target && b.record.as_ref().is_some_and(|old| old != record))
            {
                w.wanted |= w.monitor.rebaseline(&f.target, w.scans + 1);
            }
        }
    }
    let focused = ctx.input(|i| i.focused);
    w.wanted |= focused && !w.focused;
    w.focused = focused;
    if let Some((scan, slot, _)) = &w.scan {
        let done = slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some((results, resized)) = done {
            let scan = *scan;
            w.scan = None;
            // A scan answered: a later stall is a new episode, said again.
            w.said_stalled = false;
            for (key, seen) in results {
                if exporting.as_deref() != Some(key.as_str()) {
                    w.monitor.observed(&key, seen, scan);
                }
            }
            // As for the observations: a file being written by RustGIS (a publish) is its write's,
            // not a change, until it lands.
            let found = &w.found;
            w.resized = resized
                .into_iter()
                .filter(|(target, _, _)| {
                    exporting.is_none() || found.iter().find(|f| f.target == *target).map(|f| f.key.as_str()) != exporting.as_deref()
                })
                .map(|(target, record, parts)| (target, (record, parts)))
                .collect();
        }
    }
    let now = web_time::Instant::now();
    // A file that never answers (a network drive gone away) holds the scan, and with it every
    // later one: said, once, rather than shown as up to date.
    let mut say = None;
    if !w.said_stalled && w.scan.as_ref().is_some_and(|(_, _, started)| now.saturating_duration_since(*started) >= STALLED) {
        w.said_stalled = true;
        say = Some(tl!("Checking linked files for changes on disk is waiting for a file that doesn't answer (a network drive?). Changes aren't reported until it does, and reloading, saving or exporting a file there may wait for it too.").to_string());
    }
    if !w.found.is_empty() {
        if w.due.is_none_or(|due| now >= due) {
            w.wanted = true;
        }
        if w.wanted && w.scan.is_none() {
            match start(w, now) {
                // Started again: a later failure is a new episode, said again.
                Ok(()) => w.said_failed = false,
                Err(e) => {
                    log::warn!("changed-on-disk scan could not start: {e}");
                    if !w.said_failed {
                        w.said_failed = true;
                        say = Some(crate::i18n::fmt(
                            "Checking linked files for changes on disk couldn't start ({error}); it is tried again every few seconds.",
                            &[("error", &e)],
                        ));
                    }
                }
            }
        }
        // A scan under way is looked at often, until it has stalled: then every few seconds.
        let stalled = w.scan.as_ref().is_some_and(|(_, _, started)| now.saturating_duration_since(*started) >= STALLED);
        let wait = if stalled {
            EVERY
        } else if w.scan.is_some() {
            web_time::Duration::from_millis(100)
        } else {
            w.due.map_or(EVERY, |due| due.saturating_duration_since(now))
        };
        ctx.request_repaint_after(wait);
    }
    if let Some(text) = say {
        app.session.notify(text, true);
    }
}

/// Starts a scan of every watched path on a thread of its own; why not, when the OS refuses one
/// (that costs this scan, not the app: the next is tried when due).
fn start(w: &mut Watch, now: web_time::Instant) -> Result<(), String> {
    // The linked path is observed as given (one per key), never a resolved copy of it.
    let mut paths: HashMap<String, String> = HashMap::new();
    for f in &w.found {
        paths.entry(f.key.clone()).or_insert_with(|| f.path.clone());
    }
    let records: Vec<(Target, String, FileSync)> =
        w.found.iter().filter_map(|f| Some((f.target.clone(), f.path.clone(), f.record.clone()?))).collect();
    let slot = Scanned::default();
    let out = slot.clone();
    let spawned = std::thread::Builder::new().name("Changed on disk".into()).spawn(move || {
        let results = paths.into_iter().map(|(key, path)| (key, look(&path))).collect();
        let resized = records
            .into_iter()
            .map(|(target, path, record)| {
                let parts = record.resized(std::path::Path::new(&path));
                (target, record, parts)
            })
            .collect();
        *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((results, resized));
    });
    w.due = Some(now + EVERY);
    w.wanted = false;
    spawned.map_err(|e| e.to_string())?;
    w.scans += 1;
    w.scan = Some((w.scans, slot, now));
    Ok(())
}

/// The target a reload command names in the active map, if it's watched.
fn target_of(app: &RustGisApp, command: &str, params: &Value, answer: Option<&Value>) -> Option<Target> {
    let map = app.session.map();
    match command {
        "layer.reloadData" | "gpkg.discardEdits" => {
            let id = answer.and_then(|a| a.get("layer")).and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok());
            let id = id.or_else(|| app.session.layer_id(params).ok())?;
            Some(Target::Layer { map: map.instance_id(), layer: map.layer(id)?.instance_id() })
        }
        "map.reloadTiles" => {
            let i = match answer.and_then(|a| a.get("index")).and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()) {
                Some(i) => i,
                None => rustgis_engine::cmd::reads::archive_target(&app.session, params)?.0,
            };
            tiles_target(map, i)
        }
        _ => None,
    }
}

/// The target a reload names and the file its read will look at, with no file access.
pub(crate) fn read_target(app: &RustGisApp, command: &str, params: &Value) -> Option<(Target, String)> {
    if !app.watch_files {
        return None;
    }
    let target = target_of(app, command, params, None)?;
    let path = app.diskwatch.found.iter().find(|f| f.target == target)?.path.clone();
    Some((target, path))
}

/// A reload reads its file here, on the UI thread (no thread of its own): what the file holds
/// now is what it reads, and what it acknowledges once it lands. The one metadata read is taken
/// here because the read itself runs here too, on the same file.
pub(crate) fn reading(app: &mut RustGisApp, command: &str, params: &Value) {
    if let Some((target, path)) = read_target(app, command, params) {
        app.diskwatch.monitor.reading(target, look(&path));
    }
}

/// A reload's read thread looked at its file (`seen`, [`observe`]) just before reading it.
pub(crate) fn read_started(app: &mut RustGisApp, target: Target, seen: Seen) {
    app.diskwatch.monitor.reading(target, seen);
}

/// One metadata read of a watched file, for a worker thread to take before or after its work.
pub(crate) fn observe(path: &str) -> Seen {
    look(path)
}

/// A reload's read failed or never ran: what it started with is forgotten.
pub(crate) fn not_read(app: &mut RustGisApp, command: &str, params: &Value) {
    if let Some(target) = target_of(app, command, params, None) {
        app.diskwatch.monitor.not_read(&target);
    }
}

/// The targets on `key` whose data a write of `table` into `path` leaves old: other layers
/// reading the table written keep its old rows; an export over a single-dataset file (a GeoJSON,
/// a Shapefile) replaces what every layer on it reads. A layer read in place from the GeoPackage
/// table written is read again by the command itself (`gpkg::reset_sources`): it holds the new
/// rows.
fn stale_for(found: &[Found], key: &str, path: &str, table: &str, writer: Option<&Target>) -> HashSet<Target> {
    let export = writer.is_none();
    let geopackage = std::path::Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("gpkg"));
    found
        .iter()
        .filter(|f| f.key == key && Some(&f.target) != writer)
        .filter(|f| !(geopackage && f.streamed))
        .filter(|f| match f.table.as_deref() {
            Some(t) => t.eq_ignore_ascii_case(table),
            None => export,
        })
        .map(|f| f.target.clone())
        .collect()
}

/// The watched file a save or export is about to write, with no file access yet: what it
/// predicts it writes is corrected from the command's answer as the write lands.
pub(crate) fn plan_write(app: &RustGisApp, command: &str, params: &Value) -> Option<Write> {
    if !app.watch_files {
        return None;
    }
    let s = &app.session;
    // The file written, the table in it, and the layer whose data the write refreshes (a save
    // keeps the saving layer's rows; an export refreshes none).
    let (path, table, writer) = match command {
        "gpkg.saveEdits" => {
            let l = s.map().layer(s.layer_id(params).ok()?)?;
            let edit = l.gpkg.as_ref()?;
            (edit.path.clone(), edit.table.clone(), Some(Target::Layer { map: s.map().instance_id(), layer: l.instance_id() }))
        }
        // Publishing replaces its layer's file: the publishing layer holds what it wrote.
        "source.publish" => {
            let l = s.map().layer(s.layer_id(params).ok()?)?;
            let (path, _) = published_file(l)?;
            (path.to_string(), String::new(), Some(Target::Layer { map: s.map().instance_id(), layer: l.instance_id() }))
        }
        // Conversion replaces a whole file; it writes no container table and refreshes no reader.
        "data.convert" => (params.get("path").and_then(Value::as_str)?.to_string(), String::new(), None),
        // An existing GeoPackage written into; a new file isn't watched yet.
        "share.exportLayer" | "share.exportTable" => {
            let path = params.get("path").and_then(Value::as_str)?.to_string();
            // Trimmed as the engine trims it.
            let named = params.get("tableName").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty());
            let table = match (named, command) {
                (Some(name), _) => name.to_string(),
                (None, "share.exportLayer") => {
                    s.layer_id(params).ok().and_then(|id| s.map().layer(id)).map(|l| l.name.clone()).unwrap_or_default()
                }
                // As the engine names it: the standalone table's own name, given by name or id.
                (None, _) => {
                    s.table_index(params).ok().and_then(|i| s.project.tables.get(i)).map(|t| t.dataset.name.clone()).unwrap_or_default()
                }
            };
            (path, table, None)
        }
        _ => return None,
    };
    let key = key(&path);
    if !app.diskwatch.monitor.watches(&key) {
        return None;
    }
    let stale = if command == "data.convert" {
        // The explicit format can differ from the suffix: even a .gpkg is replaced whole.
        app.diskwatch.found.iter().filter(|f| f.key == key).map(|f| f.target.clone()).collect()
    } else if command == "source.publish" {
        // Every other layer on the file keeps the rows it read.
        app.diskwatch.found.iter().filter(|f| f.key == key && Some(&f.target) != writer.as_ref()).map(|f| f.target.clone()).collect()
    } else {
        stale_for(&app.diskwatch.found, &key, &path, &table, writer.as_ref())
    };
    let verified = command == "source.publish";
    Some(Write { before: None, result: None, key, path, stale, writer, table, verified })
}

/// [`plan_write`] for a save or export that writes here, on the UI thread: the file is written
/// on this thread anyway, so its one metadata read just before is taken here too.
pub(crate) fn before_write(app: &RustGisApp, command: &str, params: &Value) -> Option<Write> {
    let mut write = plan_write(app, command, params)?;
    write.observe_before();
    Some(write)
}

/// A write RustGIS is about to make into a watched file ([`plan_write`]).
pub(crate) struct Write {
    key: String,
    path: String,
    /// What the file held just before the write, and just after it.
    before: Option<Seen>,
    result: Option<Seen>,
    /// Targets whose data the write doesn't refresh.
    stale: HashSet<Target>,
    /// The target whose data it does refresh (the saving layer), if any.
    writer: Option<Target>,
    /// The table written, as predicted, then as the engine named it.
    table: String,
    /// A publish: it checked the file's contents against the writer's record first.
    verified: bool,
}

impl Write {
    /// The watched file it writes.
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    /// Looks at the file just before the write (on the thread that writes it).
    pub(crate) fn observe_before(&mut self) {
        self.before = Some(look(&self.path));
    }

    /// Looks at the file just after the write (on the thread that wrote it).
    pub(crate) fn observe_after(&mut self) {
        self.result = Some(look(&self.path));
    }
}

/// A command the app ran succeeded: a reload acknowledges what it read, a save into a watched
/// file (`before`, from [`before_write`]) is RustGIS's own write.
pub(crate) fn ran(app: &mut RustGisApp, command: &str, params: &Value, answer: &Value, before: Option<Write>) {
    match command {
        "layer.reloadData" | "gpkg.discardEdits" | "map.reloadTiles" => {
            if let Some(target) = target_of(app, command, params, Some(answer)) {
                app.diskwatch.monitor.reloaded(&target);
            }
        }
        // A save that wrote nothing (no edits) isn't a write.
        "gpkg.saveEdits" if answer.get("saved").and_then(Value::as_bool) == Some(false) => {}
        "gpkg.saveEdits" | "share.exportLayer" | "share.exportTable" | "data.convert" | "source.publish" => {
            if let Some(write) = before {
                wrote(app, write, answer);
            }
        }
        _ => {}
    }
}

/// RustGIS wrote a watched file; `answer` is the command's. The table the engine actually wrote
/// (`answer.table`: it renames reserved and control-character names) decides which layers it
/// left stale. A write here (not on a thread) is looked at once more, here: it ran here.
pub(crate) fn wrote(app: &mut RustGisApp, mut write: Write, answer: &Value) {
    if let Some(table) = answer.get("table").and_then(Value::as_str)
        && !table.eq_ignore_ascii_case(&write.table)
    {
        write.table = table.to_string();
        write.stale = stale_for(&app.diskwatch.found, &write.key, &write.path, &write.table, write.writer.as_ref());
    }
    let result = write.result.take().unwrap_or_else(|| look(&write.path));
    // Not looked at before the write (its thread couldn't): nothing to tell it from an outside
    // change, so it acknowledges nothing.
    let Some(before) = write.before.take() else { return };
    // A layer read in place from the table written that has an error now (the write's own re-read
    // couldn't reopen it, or it was already failing) didn't read the new rows: it stays marked.
    let unopened: Vec<Target> = app
        .diskwatch
        .found
        .iter()
        .filter(|f| f.key == write.key && f.streamed)
        .filter(|f| match &f.target {
            Target::Layer { map, layer } => app
                .session
                .project
                .maps
                .iter()
                .find(|m| m.instance_id() == *map)
                .and_then(|m| m.layers.iter().find(|l| l.instance_id() == *layer))
                .and_then(|l| l.source.as_ref())
                // The layers the write's own re-read touched: those on the table written.
                .filter(|s| s.table.as_deref().is_none_or(|t| t.eq_ignore_ascii_case(&write.table)))
                .is_some_and(|s| s.error.is_some()),
            Target::Tiles { .. } => false,
        })
        .map(|f| f.target.clone())
        .collect();
    write.stale.extend(unopened);
    let w = &mut app.diskwatch;
    // A scan under way started before the write; the next one is the first after it.
    w.monitor.wrote(&write.key, before, result, write.stale, write.writer.as_ref(), w.scans + 1);
    if write.verified
        && let Some(writer) = &write.writer
    {
        w.monitor.verified(&write.key, writer);
    }
}

/// Activates the target's map and returns the target's layer id or tile index there.
fn place(app: &mut RustGisApp, target: &Target) -> Result<Place, String> {
    let gone = || tl!("The layer or archive is no longer in the project.").to_string();
    let map = match target {
        Target::Layer { map, .. } | Target::Tiles { map, .. } => *map,
    };
    let at = app.session.project.maps.iter().position(|m| m.instance_id() == map).ok_or_else(gone)?;
    if app.session.project.active_map != at {
        let name = app.session.project.maps.get(at).map(|m| m.name.clone()).ok_or_else(gone)?;
        app.run("map.activate", json!({"name": name}))?;
        if app.session.map().instance_id() != map {
            return Err(tl!("Another map has that name: open the map and use Reload Data from the Layers pane.").into());
        }
    }
    match target {
        Target::Layer { layer, .. } => {
            app.session.map().layers.iter().find(|l| l.instance_id() == *layer).map(|l| Place::Layer(l.id)).ok_or_else(gone)
        }
        Target::Tiles { .. } => {
            let map = app.session.map();
            (0..map.tiles.len()).find(|&i| tiles_target(map, i).as_ref() == Some(target)).map(Place::Tiles).ok_or_else(gone)
        }
    }
}

enum Place {
    Layer(u32),
    Tiles(usize),
}

fn act(app: &mut RustGisApp, target: &Target, repair: bool) {
    let place = match place(app, target) {
        Ok(place) => place,
        Err(e) => {
            app.session.notify(e, true);
            return;
        }
    };
    // Failures say why through the command's own notice; the target stays pending.
    let _ = match (place, repair) {
        (Place::Layer(id), false) => app.run("ui.reloadData", json!({"layer": id})),
        (Place::Tiles(i), false) => app.run("map.reloadTiles", json!({"index": i})),
        (Place::Layer(id), true) => app.run("ui.dialog", json!({"name": "repairSource", "layer": id})),
        (Place::Tiles(i), true) => app.run("ui.dialog", json!({"name": "repairTiles", "index": i})),
    };
}

/// The notification: each source that changed, went missing or can't be checked, with Reload
/// Data (or Repair Data Source) for each layer or archive reading it, and Not Now. It doesn't
/// block the window, and doesn't come back for an observation it was dismissed for.
pub(crate) fn show(app: &mut RustGisApp, ctx: &egui::Context) {
    let w = &app.diskwatch;
    let mut groups: Vec<(&str, &str, Status, Vec<&Found>)> = Vec::new();
    for f in w.found.iter().filter(|f| f.record.is_none() && w.monitor.notifies(&f.target)) {
        match groups.iter_mut().find(|g| g.0 == f.key) {
            Some(g) => g.3.push(f),
            None => groups.push((&f.key, &f.path, w.status(&f.target), vec![f])),
        }
    }
    if groups.is_empty() {
        return;
    }
    let t = Tokens::get(ctx);
    let mut chosen: Option<(Target, bool)> = None;
    let mut dismissed: Vec<Target> = Vec::new();
    egui::Window::new(tl!("Changed on disk"))
        .id(egui::Id::unique("changed_on_disk"))
        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -40.0))
        .collapsible(false)
        .resizable(false)
        .default_width(380.0)
        .show(ctx, |ui| {
            for (i, (_, path, status, found)) in groups.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                let file = std::path::Path::new(path).file_name().and_then(|n| n.to_str()).unwrap_or(path);
                ui.label(egui::RichText::new(file).strong()).on_hover_text(*path);
                match status {
                    Status::Changed => {
                        ui.label(tl!(
                            "The file may have changed since it was read. What the map shows may be out of date until you reload."
                        ));
                    }
                    Status::Missing => {
                        let text = if found.iter().any(|f| f.repairable) {
                            tl!("The file is missing. Restore it, or use Repair Data Source.")
                        } else {
                            tl!("The file is missing. Restore it.")
                        };
                        ui.colored_label(t.red, text);
                    }
                    Status::Unreadable(e) => {
                        ui.colored_label(t.red, crate::i18n::fmt("The file can't be checked: {error}", &[("error", e)]));
                    }
                    Status::Unchanged | Status::Folder => {}
                }
                for f in found {
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::fmt("{map} › {layer}", &[("map", &f.map), ("layer", &f.name)]));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let repair = *status == Status::Missing && f.repairable;
                            let label = if repair { tl!("Repair Data Source…") } else { tl!("Reload Data") };
                            if ui.button(label).clicked() {
                                chosen = Some((f.target.clone(), repair));
                            }
                        });
                    });
                }
                if ui
                    .button(tl!("Not Now"))
                    .on_hover_text(tl!("Keep the data shown now. The layer stays marked; a newer change is reported again."))
                    .clicked()
                {
                    dismissed.extend(found.iter().map(|f| f.target.clone()));
                }
            }
        });
    for target in &dismissed {
        app.diskwatch.monitor.dismiss(target);
    }
    if let Some((target, repair)) = chosen {
        act(app, &target, repair);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` distinct observations, of temporary files of different sizes.
    fn observations(n: usize) -> Vec<Seen> {
        // One folder per call: the tests run in parallel.
        static CALL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let call = CALL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("rustgis-diskwatch-{}-{call}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let seen = (0..n)
            .map(|i| {
                let path = dir.join(format!("{i}.fgb"));
                std::fs::write(&path, vec![0; i + 1]).unwrap();
                look(&path.display().to_string())
            })
            .collect();
        std::fs::remove_dir_all(dir).unwrap();
        seen
    }

    fn layer(n: u64) -> Target {
        Target::Layer { map: 1, layer: n }
    }

    /// A monitor with targets on `a.fgb` (layers 1 and 2) and `b.gpkg` (layer 3), all baselined at `v0`.
    fn monitor(v0: &Seen) -> (Monitor, u64) {
        let mut m = Monitor::default();
        assert!(m.track([(layer(1), "a".into()), (layer(2), "a".into()), (layer(3), "b".into())]));
        for t in [1, 2, 3] {
            m.baseline(&layer(t), v0.clone());
        }
        for scan in 1..=2 {
            m.observed("a", v0.clone(), scan);
            m.observed("b", v0.clone(), scan);
        }
        (m, 3)
    }

    #[test]
    fn a_change_is_offered_once_two_scans_agree_and_each_target_clears_alone() {
        let [v0, v1, v2] = observations(3).try_into().unwrap();
        let (mut m, scan) = monitor(&v0);
        assert_eq!(m.status(&layer(1)), Status::Unchanged);
        // A write under way: one scan sees it, the next sees it finished differently.
        m.observed("a", v1.clone(), scan);
        assert_eq!(m.status(&layer(1)), Status::Unchanged, "one scan isn't enough");
        m.observed("a", v2.clone(), scan + 1);
        assert_eq!(m.status(&layer(1)), Status::Unchanged);
        m.observed("a", v2.clone(), scan + 2);
        assert_eq!((m.status(&layer(1)), m.status(&layer(2)), m.status(&layer(3))), (Status::Changed, Status::Changed, Status::Unchanged));
        assert!(m.notifies(&layer(1)) && m.notifies(&layer(2)));
        // Reloading one target leaves the other reading the same file pending.
        m.reading(layer(1), v2.clone());
        m.reloaded(&layer(1));
        assert_eq!((m.status(&layer(1)), m.status(&layer(2))), (Status::Unchanged, Status::Changed));
    }

    #[test]
    fn a_reload_acknowledges_only_what_it_read() {
        let [v0, v1, v2, v3] = observations(4).try_into().unwrap();
        let (mut m, scan) = monitor(&v0);
        m.observed("a", v1.clone(), scan);
        m.observed("a", v1.clone(), scan + 1);
        m.reading(layer(1), v1.clone());
        // Written again while the reload read.
        m.observed("a", v2.clone(), scan + 2);
        m.observed("a", v2.clone(), scan + 3);
        m.reloaded(&layer(1));
        assert_eq!(m.status(&layer(1)), Status::Changed, "the newer change stays pending");
        // A failed reload (no `reloaded`) keeps it pending too; a later success clears it.
        m.reading(layer(1), v2.clone());
        assert_eq!(m.status(&layer(1)), Status::Changed);
        m.reading(layer(1), v2.clone());
        m.reloaded(&layer(1));
        assert_eq!(m.status(&layer(1)), Status::Unchanged);
        // Two reloads queued, the file changing between them: each acknowledges what its own
        // read started with (reads run one at a time).
        m.reading(layer(1), v2.clone());
        m.observed("a", v3.clone(), scan + 4);
        m.observed("a", v3.clone(), scan + 5);
        m.reloaded(&layer(1));
        assert_eq!(m.status(&layer(1)), Status::Changed, "the first read didn't see v3");
        m.reading(layer(1), v3.clone());
        m.reloaded(&layer(1));
        assert_eq!(m.status(&layer(1)), Status::Unchanged);
        // A reload of a change no scan has seen yet (b.gpkg now v2) acknowledges what it read,
        // not what the scans last agreed on (v0).
        m.reading(layer(3), v2.clone());
        m.reloaded(&layer(3));
        m.observed("b", v2.clone(), scan + 6);
        m.observed("b", v2.clone(), scan + 7);
        assert_eq!(m.status(&layer(3)), Status::Unchanged, "it read v2, which then settled");
        // A read started for another target isn't taken: a reload with no observation of its own
        // (run some other way) leaves the mark, since what it read isn't known.
        m.observed("a", v1.clone(), scan + 8);
        m.observed("a", v1.clone(), scan + 9);
        m.reading(layer(2), v3.clone());
        m.reloaded(&layer(1));
        assert_eq!(m.status(&layer(1)), Status::Changed);
        m.reloaded(&layer(2));
        assert_eq!(m.status(&layer(2)), Status::Changed, "layer 2's read started at v3, not v1");
        // A reload before two scans have agreed (a target just added) acknowledges what it read.
        let mut m = Monitor::default();
        m.track([(layer(9), "c".into())]);
        m.observed("c", v0.clone(), 1);
        m.observed("c", v1.clone(), 2);
        m.reading(layer(9), v1.clone());
        m.reloaded(&layer(9));
        m.observed("c", v1.clone(), 3);
        assert_eq!(m.status(&layer(9)), Status::Unchanged, "the early reload read the change");
    }

    #[test]
    fn not_now_is_for_one_observation() {
        let [v0, v1, v2] = observations(3).try_into().unwrap();
        let (mut m, scan) = monitor(&v0);
        m.observed("a", v1.clone(), scan);
        m.observed("a", v1.clone(), scan + 1);
        m.dismiss(&layer(1));
        assert!(!m.notifies(&layer(1)) && m.notifies(&layer(2)), "dismissal is per target");
        assert_eq!(m.status(&layer(1)), Status::Changed, "the layer stays marked");
        m.observed("a", v1.clone(), scan + 2);
        assert!(!m.notifies(&layer(1)), "the same observation again doesn't notify again");
        m.observed("a", v2.clone(), scan + 3);
        m.observed("a", v2.clone(), scan + 4);
        assert!(m.notifies(&layer(1)), "a newer change notifies again");
    }

    #[test]
    fn missing_and_unreadable_are_states_that_clear_on_a_good_check() {
        let [v0, v1] = observations(2).try_into().unwrap();
        let (mut m, scan) = monitor(&v0);
        m.observed("a", Seen::Missing, scan);
        m.observed("a", Seen::Missing, scan + 1);
        assert_eq!(m.status(&layer(1)), Status::Missing);
        assert!(m.notifies(&layer(1)));
        m.dismiss(&layer(1));
        assert!(!m.notifies(&layer(1)), "no repeated prompt while it stays missing");
        m.observed("a", Seen::Unreadable("locked".into()), scan + 2);
        m.observed("a", Seen::Unreadable("locked".into()), scan + 3);
        assert_eq!(m.status(&layer(1)), Status::Unreadable("locked".into()));
        // Back as it was: up to date. Back changed: changed.
        m.observed("a", v0.clone(), scan + 4);
        m.observed("a", v0.clone(), scan + 5);
        assert_eq!(m.status(&layer(1)), Status::Unchanged);
        // Missing again after it was up to date: a new episode, so Not Now from the last one
        // doesn't hide it.
        m.observed("a", Seen::Missing, scan + 4);
        m.observed("a", Seen::Missing, scan + 5);
        assert!(m.notifies(&layer(1)), "the earlier dismissal cleared once the file was back");
        m.observed("a", v0.clone(), scan + 4);
        m.observed("a", v0.clone(), scan + 5);
        m.observed("a", Seen::Missing, scan + 6);
        m.observed("a", Seen::Missing, scan + 7);
        m.observed("a", v1.clone(), scan + 8);
        m.observed("a", v1.clone(), scan + 9);
        assert_eq!(m.status(&layer(1)), Status::Changed, "reappearance is detected");
        // A source missing when its target was added changes once it appears.
        let mut m = Monitor::default();
        m.track([(layer(9), "c".into())]);
        m.observed("c", Seen::Missing, 1);
        m.observed("c", v0.clone(), 2);
        m.observed("c", v0.clone(), 3);
        assert_eq!(m.status(&layer(9)), Status::Changed);
        // Missing from the first look (a moved file at project open): Not Now holds while it
        // stays missing.
        let mut m = Monitor::default();
        m.track([(layer(8), "d".into())]);
        m.observed("d", Seen::Missing, 1);
        m.observed("d", Seen::Missing, 2);
        assert!(m.notifies(&layer(8)));
        m.dismiss(&layer(8));
        m.observed("d", Seen::Missing, 3);
        assert!(!m.notifies(&layer(8)), "no prompt again while it stays missing");
        // Unreadable at the first look (a passing metadata error at project open): once it reads,
        // that is what the layer was read with, not a change.
        let mut m = Monitor::default();
        m.track([(layer(7), "e".into())]);
        m.observed("e", Seen::Unreadable("locked".into()), 1);
        m.observed("e", v0.clone(), 2);
        m.observed("e", v0.clone(), 3);
        assert_eq!(m.status(&layer(7)), Status::Unchanged);
    }

    #[test]
    fn rustgis_own_write_is_acknowledged_once_stable_but_an_earlier_change_is_not() {
        let [v0, v1, v2, v3, v4] = observations(5).try_into().unwrap();
        let (mut m, scan) = monitor(&v0);
        // A scan started before the save sees the file mid-write; the save's result (v2) settles.
        m.wrote("b", v0.clone(), v2.clone(), HashSet::new(), None, scan + 1);
        m.observed("b", v1.clone(), scan);
        m.observed("b", v2.clone(), scan + 1);
        m.observed("b", v2.clone(), scan + 2);
        assert_eq!(m.status(&layer(3)), Status::Unchanged, "the self-save is not an external change");
        // Saved, then nothing changed on disk at all (the write was a no-op): still acknowledged,
        // and a later external change is still seen.
        m.wrote("b", v2.clone(), v2.clone(), HashSet::new(), None, scan + 3);
        m.observed("b", v2.clone(), scan + 3);
        m.observed("b", v2.clone(), scan + 4);
        m.observed("b", v3.clone(), scan + 5);
        m.observed("b", v3.clone(), scan + 6);
        assert_eq!(m.status(&layer(3)), Status::Changed);
        // A change pending before the save stays pending after it.
        m.wrote("b", v3.clone(), v1.clone(), HashSet::new(), None, scan + 7);
        m.observed("b", v1.clone(), scan + 7);
        m.observed("b", v1.clone(), scan + 8);
        assert_eq!(m.status(&layer(3)), Status::Changed);
        // An outside change no scan saw (v4), then a save at once: the save doesn't absorb it.
        let (mut m, scan) = monitor(&v0);
        m.wrote("b", v4.clone(), v1.clone(), HashSet::new(), None, scan);
        m.observed("b", v1.clone(), scan);
        m.observed("b", v1.clone(), scan + 1);
        assert_eq!(m.status(&layer(3)), Status::Changed, "the outside write is still offered");
        // RustGIS saves (v1), then another program writes (v2) before any scan, or between scans.
        for scans in [[v2.clone(), v2.clone(), v2.clone()], [v1.clone(), v2.clone(), v2.clone()]] {
            let (mut m, scan) = monitor(&v0);
            m.wrote("b", v0.clone(), v1.clone(), HashSet::new(), None, scan);
            for (n, seen) in scans.into_iter().enumerate() {
                m.observed("b", seen, scan + n as u64);
            }
            assert_eq!(m.status(&layer(3)), Status::Changed, "a change just after the save is still offered");
        }
        // Two saves in a row continue one write; another program writing between them doesn't.
        let (mut m, scan) = monitor(&v0);
        m.wrote("b", v0.clone(), v1.clone(), HashSet::new(), None, scan);
        m.wrote("b", v1.clone(), v2.clone(), HashSet::new(), None, scan);
        m.observed("b", v2.clone(), scan);
        m.observed("b", v2.clone(), scan + 1);
        assert_eq!(m.status(&layer(3)), Status::Unchanged);
        let (mut m, scan) = monitor(&v0);
        m.wrote("b", v0.clone(), v1.clone(), HashSet::new(), None, scan);
        m.wrote("b", v3.clone(), v2.clone(), HashSet::new(), None, scan);
        m.observed("b", v2.clone(), scan);
        m.observed("b", v2.clone(), scan + 1);
        assert_eq!(m.status(&layer(3)), Status::Changed, "the write between the saves is offered");
        // Another layer on the table a save wrote still holds its old rows: it stays marked,
        // while the saving layer and nothing else on the file take the result.
        let (mut m, scan) = monitor(&v0);
        m.wrote("a", v0.clone(), v1.clone(), HashSet::from([layer(2)]), Some(&layer(1)), scan);
        m.observed("a", v1.clone(), scan);
        m.observed("a", v1.clone(), scan + 1);
        assert_eq!((m.status(&layer(1)), m.status(&layer(2))), (Status::Unchanged, Status::Changed));
        // Both save the table before a scan lands: the second save's layer holds what the file
        // now has, the first's doesn't (it never read the second's rows).
        let (mut m, scan) = monitor(&v0);
        m.wrote("a", v0.clone(), v1.clone(), HashSet::from([layer(2)]), Some(&layer(1)), scan);
        m.wrote("a", v1.clone(), v2.clone(), HashSet::from([layer(1)]), Some(&layer(2)), scan);
        m.observed("a", v2.clone(), scan);
        m.observed("a", v2.clone(), scan + 1);
        assert_eq!((m.status(&layer(1)), m.status(&layer(2))), (Status::Changed, Status::Unchanged), "layer 1 hasn't read layer 2's rows");
        // A scan in flight across the write (numbered before it) sees the result; the next agrees:
        // that is RustGIS's own write, not a change, at no point in between.
        let (mut m, scan) = monitor(&v0);
        m.wrote("b", v0.clone(), v1.clone(), HashSet::new(), None, scan + 1);
        m.observed("b", v1.clone(), scan);
        assert_eq!(m.status(&layer(3)), Status::Unchanged);
        m.observed("b", v1.clone(), scan + 1);
        assert_eq!(m.status(&layer(3)), Status::Unchanged, "no false mark while the in-flight scan agrees");
    }

    #[test]
    fn removed_replaced_or_repaired_targets_start_over_and_late_scans_hit_nothing() {
        let [v0, v1] = observations(2).try_into().unwrap();
        let (mut m, scan) = monitor(&v0);
        m.observed("a", v1.clone(), scan);
        m.observed("a", v1.clone(), scan + 1);
        // Layer 1 repaired to another file, layer 2 removed, layer 4 (a new instance) added.
        assert!(m.track([(layer(1), "moved".into()), (layer(3), "b".into()), (layer(4), "a".into())]));
        assert_eq!(m.status(&layer(2)), Status::Unchanged);
        assert!(!m.notifies(&layer(2)));
        assert_eq!(m.keys(), HashSet::from(["moved".to_string(), "b".to_string(), "a".to_string()]));
        m.observed("moved", v0.clone(), scan + 2);
        m.observed("a", v1.clone(), scan + 2);
        assert_eq!(
            (m.status(&layer(1)), m.status(&layer(4))),
            (Status::Unchanged, Status::Unchanged),
            "new targets baseline at their first scan"
        );
        // A late result for a path nothing reads any more is dropped.
        m.track([(layer(3), "b".into())]);
        m.observed("a", v0.clone(), scan + 3);
        assert!(!m.keys().contains("a"));
    }

    #[test]
    fn a_scan_that_looked_before_a_rebaseline_does_not_become_the_baseline() {
        let [v0, v1] = observations(2).try_into().unwrap();
        let (mut m, scan) = monitor(&v0);
        // Started (scan `scan`) while the file still held v0; a publish then wrote v1 and the
        // layer's record changed before that scan landed.
        m.rebaseline(&layer(3), scan + 1);
        m.observed("b", v0.clone(), scan);
        assert_eq!(m.status(&layer(3)), Status::Unchanged, "no baseline yet");
        for next in [scan + 1, scan + 2] {
            m.observed("b", v1.clone(), next);
        }
        assert_eq!(m.status(&layer(3)), Status::Unchanged, "the scans after it are the baseline");
        assert_eq!(m.status(&layer(1)), Status::Unchanged, "another file's target is untouched");
    }

    #[test]
    fn a_rebaseline_drops_an_earlier_publish_still_to_be_acknowledged() {
        let [v0, v1, v2] = observations(3).try_into().unwrap();
        let (mut m, scan) = monitor(&v0);
        // Published in the app (v0 to v1), then again through the session alone (v2) before any
        // scan acknowledged the first.
        m.wrote("b", v0.clone(), v1.clone(), HashSet::new(), Some(&layer(3)), scan);
        m.verified("b", &layer(3));
        m.rebaseline(&layer(3), scan);
        for next in [scan, scan + 1] {
            m.observed("b", v2.clone(), next);
        }
        assert_eq!(m.status(&layer(3)), Status::Unchanged, "the second publish's file is the baseline");
    }

    #[test]
    fn two_tile_layers_on_one_archive_are_separate_targets() {
        use rustgis_engine::map::{Map, TileKind, TileLayer};
        let path = std::env::temp_dir().join("rustgis-diskwatch-same.pmtiles").display().to_string();
        let mut map = Map::new("m");
        map.tiles = vec![
            TileLayer::new("first", TileKind::PmTiles, path.clone()),
            TileLayer::new("web", TileKind::Raster, "https://tiles.invalid/{z}/{x}/{y}.png"),
            TileLayer::new("second", TileKind::PmTiles, path.clone()),
        ];
        let [first, web, second] = [0, 1, 2].map(|i| tiles_target(&map, i));
        assert!(web.is_none(), "a web service isn't watched");
        let (Some(first), Some(second)) = (first, second) else { panic!("local archives are targets") };
        assert_ne!(first, second);
        // The second still names the second after the web layer goes.
        map.tiles.remove(1);
        assert_eq!(tiles_target(&map, 1), Some(second));
    }

    #[test]
    fn keys_keep_parent_steps_and_hostile_paths_are_observed_without_panicking() {
        let base = std::env::temp_dir().join("rustgis-diskwatch-key");
        let plain = base.join("data.fgb").display().to_string();
        assert_eq!(key(&base.join(".").join("data.fgb").display().to_string()), key(&plain));
        // `x` may be a link to another folder: `x/..` isn't `base` unless the system says so.
        #[cfg(unix)]
        assert_ne!(key(&base.join("x").join("..").join("data.fgb").display().to_string()), key(&plain));
        for hostile in ["", "\0", "/nonexistent/\u{202e}/x.shp", "relative/../../..", &"a".repeat(5000)] {
            let _ = key(hostile);
            assert!(!matches!(look(hostile), Seen::Present(_)), "{hostile:?}");
        }
    }

    /// Runs `n` real scans to completion (the timer isn't waited for).
    fn scan(app: &mut RustGisApp, ctx: &egui::Context, n: usize) {
        for _ in 0..n {
            app.diskwatch.wanted = true;
            tick(app, ctx);
            let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while app.diskwatch.scan.is_some() {
                assert!(std::time::Instant::now() < end, "the scan never landed");
                std::thread::sleep(std::time::Duration::from_millis(5));
                tick(app, ctx);
            }
        }
    }

    #[test]
    fn the_app_marks_an_outside_change_and_clears_it_on_reload_but_not_for_its_own_save() {
        use rustgis_engine::data::{Coord, Dataset, Field, FieldType, Geometry, GeometryType, Value as V, raster::Crs, source::gpkg};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("places.gpkg");
        let write = |to: &std::path::Path, name: &str| {
            let mut data = Dataset::new("places", GeometryType::Point, vec![Field::new("name", FieldType::Text)]);
            data.push(Some(Geometry::Point(Coord::new(1.0, 20.0))), vec![V::Text(name.into())]);
            gpkg::write_in(&data, to, Crs::LonLat).unwrap();
        };
        write(&path, "original");
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        app.watch_files = true;
        let id = app.run("gpkg.addLayer", json!({"path": path, "table": "places"})).unwrap()["layer"].as_u64().unwrap() as u32;
        let target = Target::Layer { map: app.session.map().instance_id(), layer: app.session.map().layer(id).unwrap().instance_id() };
        let ctx = egui::Context::default();
        scan(&mut app, &ctx, 2);
        assert_eq!(app.diskwatch.status(&target), Status::Unchanged, "opening is not a change");
        // Another program replaces the file.
        let other = dir.path().join("other.gpkg");
        write(&other, "changed outside");
        std::fs::rename(&other, &path).unwrap();
        scan(&mut app, &ctx, 2);
        assert_eq!(app.diskwatch.status(&target), Status::Changed);
        assert!(app.diskwatch.monitor.notifies(&target));
        app.run("ui.reloadData", json!({"layer": id})).unwrap();
        scan(&mut app, &ctx, 2);
        assert_eq!(app.diskwatch.status(&target), Status::Unchanged, "the reload read it");
        // RustGIS's own save into the file.
        app.run("edit.enable", json!({"on": true})).unwrap();
        app.run("edit.setValue", json!({"layer": id, "row": 0, "field": "name", "value": "saved here"})).unwrap();
        app.run("gpkg.saveEdits", json!({"layer": id})).unwrap();
        scan(&mut app, &ctx, 3);
        assert_eq!(app.diskwatch.status(&target), Status::Unchanged, "RustGIS's own write is not an outside change");
    }

    #[test]
    fn a_scan_that_never_answers_is_said_once_and_one_that_answers_isnt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("places.geojson");
        std::fs::write(&path, r#"{"type":"FeatureCollection","features":[]}"#).unwrap();
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        app.watch_files = true;
        app.run("layer.addData", json!({"path": path, "stream": true})).unwrap();
        let ctx = egui::Context::default();
        let said = |app: &RustGisApp, mark| app.session.notices_since(mark).iter().filter(|n| n.text.contains("doesn't answer")).count();
        // Scans that answer say nothing.
        let mark = app.session.notice_mark();
        scan(&mut app, &ctx, 2);
        assert_eq!(said(&app, mark), 0);
        // A scan in flight for longer than `STALLED` whose slot never fills: said once, and no
        // second scan is started over it.
        let started = web_time::Instant::now().checked_sub(STALLED * 2).unwrap();
        app.diskwatch.scan = Some((app.diskwatch.scans + 1, Scanned::default(), started));
        let scans = app.diskwatch.scans;
        for _ in 0..3 {
            tick(&mut app, &ctx);
        }
        assert_eq!(said(&app, mark), 1);
        assert_eq!(app.diskwatch.scans, scans, "no scan starts while one is in flight");
    }

    #[test]
    fn an_export_leaves_the_layers_reading_what_it_replaced_marked() {
        use rustgis_engine::data::{Dataset, GeometryType, raster::Crs, source::gpkg};
        let dir = tempfile::tempdir().unwrap();
        let geojson = dir.path().join("places.geojson");
        std::fs::write(&geojson, r#"{"type":"FeatureCollection","features":[]}"#).unwrap();
        let package = dir.path().join("places.gpkg");
        gpkg::write_in(&Dataset::new("rows", GeometryType::Point, vec![]), &package, Crs::LonLat).unwrap();
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        app.watch_files = true;
        app.run("layer.addData", json!({"path": geojson, "stream": true})).unwrap();
        let table = app.run("gpkg.addLayer", json!({"path": package, "table": "rows"})).unwrap()["layer"].as_u64().unwrap() as u32;
        let table = Target::Layer { map: app.session.map().instance_id(), layer: app.session.map().layer(table).unwrap().instance_id() };
        let mut standalone = rustgis_engine::project::Table::new(7, Dataset::new("rows", GeometryType::None, vec![]));
        standalone.dataset.name = "rows".into();
        app.session.project.tables.push(standalone);
        tick(&mut app, &egui::Context::default());
        let stale = |app: &RustGisApp, command, params: Value| before_write(app, command, &params).map(|w| w.stale.len());
        // Over a single-dataset file: the layer reading it now shows what was there before.
        assert_eq!(stale(&app, "share.exportLayer", json!({"path": geojson})), Some(1));
        // Interop replaces a whole file in its explicit format, even with a .gpkg suffix:
        // it refreshes no layer or table, so every reader keeps its old rows.
        for path in [&geojson, &package] {
            let write = before_write(&app, "data.convert", &json!({"path": path, "format": "csv"})).unwrap();
            assert_eq!(write.stale.len(), 1);
            assert!(write.writer.is_none());
            assert!(write.table.is_empty());
        }
        // Into the GeoPackage table a layer reads, by table id or by name; another table, none.
        let by_id = before_write(&app, "share.exportTable", &json!({"table": 7, "path": package, "mode": "replaceTable"})).unwrap();
        assert!(by_id.stale.contains(&table));
        assert_eq!(stale(&app, "share.exportTable", json!({"table": "rows", "path": package, "mode": "replaceTable"})), Some(1));
        assert_eq!(
            stale(&app, "share.exportTable", json!({"table": 7, "path": package, "mode": "addTable", "tableName": "other"})),
            Some(0)
        );
        // Named with spaces the engine trims: still the table the layer reads.
        assert_eq!(
            stale(&app, "share.exportTable", json!({"table": 7, "path": package, "mode": "replaceTable", "tableName": " rows "})),
            Some(1)
        );
        // A real export: once the scans agree, the layer on the replaced table is marked, and
        // nothing marks the layer on the GeoJSON the export didn't touch.
        let ctx = egui::Context::default();
        scan(&mut app, &ctx, 2);
        let geo = app.diskwatch.found.iter().find(|f| f.key == key(&geojson.display().to_string())).map(|f| f.target.clone()).unwrap();
        app.run("share.exportTable", json!({"table": 7, "path": package, "mode": "replaceTable"})).unwrap();
        scan(&mut app, &ctx, 3);
        assert_eq!(app.diskwatch.status(&table), Status::Changed, "the layer still shows the replaced table's old rows");
        assert_eq!(app.diskwatch.status(&geo), Status::Unchanged);
    }

    fn target_of_layer(app: &RustGisApp, id: u32) -> Target {
        Target::Layer { map: app.session.map().instance_id(), layer: app.session.map().layer(id).unwrap().instance_id() }
    }

    fn layer_id(answer: Value) -> u32 {
        answer["layer"].as_u64().unwrap() as u32
    }

    #[test]
    fn a_background_export_marks_only_the_layers_it_left_stale() {
        let dir = tempfile::tempdir().unwrap();
        let geojson = dir.path().join("points.geojson");
        std::fs::write(
            &geojson,
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
        )
        .unwrap();
        let package = dir.path().join("places.gpkg");
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        let points = layer_id(app.run("layer.addData", json!({"path": geojson})).unwrap());
        for table in ["rows", "other"] {
            let mode = if package.exists() { "addTable" } else { "newFile" };
            app.run("share.exportLayer", json!({"layer": points, "path": package, "mode": mode, "tableName": table})).unwrap();
        }
        let rows = layer_id(app.run("gpkg.addLayer", json!({"path": package, "table": "rows"})).unwrap());
        let other = layer_id(app.run("gpkg.addLayer", json!({"path": package, "table": "other"})).unwrap());
        let (rows, other) = (target_of_layer(&app, rows), target_of_layer(&app, other));
        app.watch_files = true;
        let ctx = egui::Context::default();
        scan(&mut app, &ctx, 2);
        // Replaced on the export's thread, as the desktop's Export Features does.
        crate::exportjob::start(
            &mut app,
            "share.exportLayer",
            json!({"layer": points, "path": package, "mode": "replaceTable", "tableName": "rows"}),
        )
        .unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while crate::exportjob::land(&mut app) {
            assert!(std::time::Instant::now() < end, "the export never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        scan(&mut app, &ctx, 3);
        assert_eq!(app.diskwatch.status(&rows), Status::Changed, "the editable layer on the replaced table holds its old rows");
        assert_eq!(app.diskwatch.status(&other), Status::Unchanged, "RustGIS's own export isn't an outside change for another table");
    }

    /// What the Layers pane says of layer `id`'s edits, once its hash has landed.
    fn edited(app: &mut RustGisApp, ctx: &egui::Context, id: u32) -> bool {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let l = app.session.map().layer(id).unwrap().clone();
            if let Some(edited) = crate::syncmark::hashed(app, &l, ctx) {
                return edited;
            }
            assert!(std::time::Instant::now() < end, "the hash never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn status_of(app: &RustGisApp, id: u32) -> Status {
        layer_status(app, app.session.map().layer(id).unwrap())
    }

    /// A GeoJSON read whole, as the desktop's Add Data reads it, so it can publish.
    fn published_layer(dir: &std::path::Path) -> (RustGisApp, u32, std::path::PathBuf) {
        let path = dir.join("parcels.geojson");
        std::fs::write(
            &path,
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{"owner":"Smith"},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
        )
        .unwrap();
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        let id = layer_id(app.run("layer.addData", json!({"path": path, "stream": false})).unwrap());
        assert!(app.session.map().layer(id).unwrap().synced.is_some());
        app.watch_files = true;
        (app, id, path)
    }

    #[test]
    fn a_publish_clears_a_mark_left_by_the_same_bytes_written_again() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id, path) = published_layer(dir.path());
        let ctx = egui::Context::default();
        scan(&mut app, &ctx, 2);
        // Another program writes the same bytes: only its metadata changed.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, std::fs::read(&path).unwrap()).unwrap();
        scan(&mut app, &ctx, 2);
        assert_eq!(status_of(&app, id), Status::Changed);
        app.run("edit.enable", json!({"on": true})).unwrap();
        app.run("edit.setValue", json!({"layer": id, "row": 0, "field": "owner", "value": "Jones"})).unwrap();
        app.run("source.publish", json!({"layer": id})).unwrap();
        scan(&mut app, &ctx, 3);
        assert_eq!(status_of(&app, id), Status::Unchanged, "the publish checked the contents and wrote them");
    }

    #[test]
    fn a_publish_through_the_session_alone_clears_the_mark_too() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id, path) = published_layer(dir.path());
        let ctx = egui::Context::default();
        scan(&mut app, &ctx, 2);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&path, std::fs::read(&path).unwrap()).unwrap();
        scan(&mut app, &ctx, 2);
        assert_eq!(status_of(&app, id), Status::Changed);
        // As the assistant's relay runs it: the session's own dispatch, without the app's hooks.
        app.session.run("edit.enable", &json!({"on": true})).unwrap();
        app.session.run("edit.setValue", &json!({"layer": id, "row": 0, "field": "owner", "value": "Jones"})).unwrap();
        app.session.run("source.publish", &json!({"layer": id})).unwrap();
        scan(&mut app, &ctx, 3);
        assert_eq!(status_of(&app, id), Status::Unchanged, "the new record says the file was published");
    }

    #[test]
    fn a_scan_left_unread_across_a_session_publish_does_not_become_the_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id, _) = published_layer(dir.path());
        let ctx = egui::Context::default();
        scan(&mut app, &ctx, 2);
        // A scan looks at the file as it is, and its answer waits, untaken.
        app.diskwatch.wanted = true;
        tick(&mut app, &ctx);
        let slot = app.diskwatch.scan.as_ref().map(|(_, slot, _)| slot.clone()).unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while slot.lock().unwrap().is_none() {
            assert!(std::time::Instant::now() < end, "the scan never answered");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Published through the session alone, then that old answer lands with later scans.
        app.session.run("edit.enable", &json!({"on": true})).unwrap();
        app.session.run("edit.setValue", &json!({"layer": id, "row": 0, "field": "owner", "value": "Jones"})).unwrap();
        app.session.run("source.publish", &json!({"layer": id})).unwrap();
        scan(&mut app, &ctx, 3);
        assert_eq!(status_of(&app, id), Status::Unchanged, "the answer from before the publish isn't the baseline");
    }

    #[test]
    fn edits_on_a_missing_file_point_to_export_not_publish() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id, path) = published_layer(dir.path());
        let ctx = egui::Context::default();
        scan(&mut app, &ctx, 2);
        app.run("edit.enable", json!({"on": true})).unwrap();
        app.run("edit.setValue", json!({"layer": id, "row": 0, "field": "owner", "value": "Jones"})).unwrap();
        assert!(edited(&mut app, &ctx, id));
        std::fs::remove_file(&path).unwrap();
        scan(&mut app, &ctx, 2);
        assert_eq!(status_of(&app, id), Status::Missing);
        let l = app.session.map().layer(id).unwrap().clone();
        let (_, detail, _) = crate::panes::layer_note(&mut app, &l, &ctx).unwrap();
        assert!(detail.contains("Export Features"), "{detail}");
        assert!(!detail.contains("writes them to the file"), "Publish refuses a missing file: {detail}");
    }

    #[test]
    fn a_layer_on_a_network_share_is_not_marked_as_publishable() {
        let dir = tempfile::tempdir().unwrap();
        let (app, id, _) = published_layer(dir.path());
        assert!(publishes(app.session.map().layer(id).unwrap()));
        // Publish to Source refuses a share, so the Layers pane doesn't promise it.
        for share in [r"\\host\share\parcels.geojson", "//host/share/parcels.geojson"] {
            let mut l = app.session.map().layer(id).unwrap().clone();
            l.dataset.source = Some(share.into());
            assert!(!publishes(&l), "{share}");
        }
    }

    #[test]
    fn a_published_file_resized_while_the_project_was_closed_is_marked() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id, path) = published_layer(dir.path());
        // Changed before the first scan, which takes what is there as its baseline.
        std::fs::write(&path, r#"{"type":"FeatureCollection","features":[]}"#).unwrap();
        let ctx = egui::Context::default();
        scan(&mut app, &ctx, 2);
        assert_eq!(status_of(&app, id), Status::Changed, "its length differs from the record");
        assert!(!edited(&mut app, &ctx, id), "the layer holds what it read");
    }

    #[test]
    fn edits_are_marked_until_published_and_the_publish_is_not_an_outside_change() {
        let dir = tempfile::tempdir().unwrap();
        let (mut app, id, path) = published_layer(dir.path());
        let ctx = egui::Context::default();
        scan(&mut app, &ctx, 2);
        assert_eq!(status_of(&app, id), Status::Unchanged);
        assert!(!edited(&mut app, &ctx, id));
        app.run("edit.enable", json!({"on": true})).unwrap();
        app.run("edit.setValue", json!({"layer": id, "row": 0, "field": "owner", "value": "Jones"})).unwrap();
        assert!(edited(&mut app, &ctx, id), "an edit the file lacks");
        crate::exportjob::start(&mut app, "source.publish", json!({"layer": id})).unwrap();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while crate::exportjob::land(&mut app) {
            assert!(std::time::Instant::now() < end, "the publish never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(std::fs::read_to_string(&path).unwrap().contains("Jones"));
        scan(&mut app, &ctx, 3);
        assert_eq!(status_of(&app, id), Status::Unchanged, "RustGIS's own publish is not an outside change");
        assert!(!edited(&mut app, &ctx, id), "published");
        // Published where it is asked (a script), not on the export thread: the same.
        app.run("edit.setValue", json!({"layer": id, "row": 0, "field": "owner", "value": "Lee"})).unwrap();
        app.run("source.publish", json!({"layer": id})).unwrap();
        scan(&mut app, &ctx, 3);
        assert_eq!(status_of(&app, id), Status::Unchanged, "a publish run in place is RustGIS's own write too");
        assert!(!edited(&mut app, &ctx, id));
        // Another program writes the file; the notification leaves it to the Layers pane.
        std::fs::write(&path, r#"{"type":"FeatureCollection","features":[]}"#).unwrap();
        scan(&mut app, &ctx, 2);
        assert_eq!(status_of(&app, id), Status::Changed);
        assert!(app.diskwatch.found.iter().all(|f| f.record.is_some()), "offered no Reload Data");
    }

    #[test]
    fn a_queued_reload_that_is_refused_leaves_a_later_change_marked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("places.geojson");
        // Replaced whole, as an editor saving does: a read on its thread never sees half a file.
        let replace = |n: usize| {
            let features: Vec<String> = (0..n)
                .map(|i| format!(r#"{{"type":"Feature","properties":{{}},"geometry":{{"type":"Point","coordinates":[{i},0]}}}}"#))
                .collect();
            let beside = dir.path().join("places.tmp");
            std::fs::write(&beside, format!(r#"{{"type":"FeatureCollection","features":[{}]}}"#, features.join(","))).unwrap();
            std::fs::rename(&beside, dir.path().join("places.geojson")).unwrap();
        };
        replace(1);
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        app.watch_files = true;
        app.reads_off_frame = true;
        let id = layer_id(app.run("layer.addData", json!({"path": path, "stream": true})).unwrap());
        let target = target_of_layer(&app, id);
        let ctx = egui::Context::default();
        scan(&mut app, &ctx, 2);
        replace(2);
        scan(&mut app, &ctx, 2);
        assert_eq!(app.diskwatch.status(&target), Status::Changed);
        // The first reload starts on its thread; the file changes; a second waits behind it. The
        // second's read was prepared before the first replaced the source, so the engine refuses
        // it when it lands (nothing is discarded).
        app.run("ui.reloadData", json!({"layer": id})).unwrap();
        // Wait for the first read to be done on its thread before the file changes again: a read
        // that began after the change would see it, and the mark would rightly clear.
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !app.reads.read_finished_for_tests() {
            assert!(std::time::Instant::now() < end, "the first reload never finished reading");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        replace(3);
        app.run("ui.reloadData", json!({"layer": id})).unwrap();
        let land_all = |app: &mut RustGisApp| {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while crate::readjob::land(app) {
                assert!(std::time::Instant::now() < end, "the reads never landed");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        let mark = app.session.notice_mark();
        land_all(&mut app);
        assert!(app.session.notices_since(mark).iter().any(|n| n.error), "the queued reload was refused");
        scan(&mut app, &ctx, 2);
        assert_eq!(
            app.diskwatch.status(&target),
            Status::Changed,
            "the first reload started before the last change, the second never read"
        );
        // Asked again: it reads what is there now, and the mark clears.
        app.run("ui.reloadData", json!({"layer": id})).unwrap();
        land_all(&mut app);
        scan(&mut app, &ctx, 2);
        assert_eq!(app.diskwatch.status(&target), Status::Unchanged);
    }

    #[test]
    fn a_streamed_layer_with_an_error_on_the_table_written_stays_marked() {
        let dir = tempfile::tempdir().unwrap();
        let geojson = dir.path().join("points.geojson");
        std::fs::write(
            &geojson,
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
        )
        .unwrap();
        let package = dir.path().join("places.gpkg");
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        let points = layer_id(app.run("layer.addData", json!({"path": geojson})).unwrap());
        for table in ["rows", "other"] {
            let mode = if package.exists() { "addTable" } else { "newFile" };
            app.run("share.exportLayer", json!({"layer": points, "path": package, "mode": mode, "tableName": table})).unwrap();
        }
        for table in ["rows", "other"] {
            app.run("layer.addData", json!({"path": package, "table": table, "stream": true})).unwrap();
        }
        app.watch_files = true;
        scan(&mut app, &egui::Context::default(), 2);
        let write = before_write(
            &app,
            "share.exportLayer",
            &json!({"layer": points, "path": package, "mode": "replaceTable", "tableName": "rows"}),
        )
        .unwrap();
        assert!(write.stale.is_empty(), "the command re-reads layers read in place from the table it writes");
        // Every layer read in place from the file fails after the write; only those on the table
        // written are the write's to answer for.
        let map = app.session.map().instance_id();
        let mut on = |table: &str| -> Vec<Target> {
            let mut out = Vec::new();
            for l in app.session.map_mut().layers.iter_mut() {
                if let Some(s) = l.source.as_mut().filter(|s| s.table.as_deref() == Some(table)) {
                    s.error = Some("couldn't reopen".into());
                    out.push(Target::Layer { map, layer: l.instance_id() });
                }
            }
            out
        };
        let (rows, other) = (on("rows"), on("other"));
        assert!(!rows.is_empty() && !other.is_empty());
        let key = write.key.clone();
        wrote(&mut app, write, &json!({}));
        let stale = &app.diskwatch.monitor.sources[&key].wrote.as_ref().unwrap().3;
        assert!(rows.iter().all(|t| stale.contains(t)) && other.iter().all(|t| !stale.contains(t)), "{stale:?}");
    }

    #[test]
    fn an_export_under_a_reserved_table_name_marks_the_layer_on_the_table_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let geojson = dir.path().join("pts.geojson");
        std::fs::write(
            &geojson,
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","properties":{},"geometry":{"type":"Point","coordinates":[1,2]}}]}"#,
        )
        .unwrap();
        let package = dir.path().join("places.gpkg");
        let mut app = RustGisApp::new(Session::new(rustgis_engine::Project::default()));
        let points = layer_id(app.run("layer.addData", json!({"path": geojson})).unwrap());
        let first =
            app.run("share.exportLayer", json!({"layer": points, "path": package, "mode": "newFile", "tableName": "gpkg_cities"})).unwrap();
        // The engine doesn't write a reserved name as given.
        assert_eq!(first["table"], "layer_gpkg_cities");
        app.watch_files = true;
        let reader =
            app.run("gpkg.addLayer", json!({"path": package, "table": "layer_gpkg_cities"})).unwrap()["layer"].as_u64().unwrap() as u32;
        let reader = Target::Layer { map: app.session.map().instance_id(), layer: app.session.map().layer(reader).unwrap().instance_id() };
        let ctx = egui::Context::default();
        scan(&mut app, &ctx, 2);
        assert_eq!(app.diskwatch.status(&reader), Status::Unchanged);
        app.run("share.exportLayer", json!({"layer": points, "path": package, "mode": "replaceTable", "tableName": "gpkg_cities"}))
            .unwrap();
        scan(&mut app, &ctx, 3);
        assert_eq!(app.diskwatch.status(&reader), Status::Changed, "the layer still shows the replaced table's old rows");
    }
}
