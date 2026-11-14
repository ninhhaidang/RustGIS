//! Edits not yet published (#1385): whether a layer read whole from a Shapefile or GeoJSON file
//! holds rows its file lacks, for the Layers pane. The layer's rows are hashed
//! (`rustgis_data::sync::rows_digest`) on a thread of their own and compared with the record of
//! the last sync; the answer is kept until the rows, fields, coordinate system or record change.
//! An edit copies the rows, so a changed layer is told by their allocation without a hash. The
//! rows hashed are held only while their hash runs. A hash whose layer changed or went away is
//! cancelled (`rows_digest` looks at Cancel between rows), and no more hashes run at once than
//! the thread setting allows (`rustgis_data::threads::max`), so opening a project of many large
//! layers doesn't start one each. Nothing is counted: the mark is a status, not a run with
//! progress. App state alone: nothing here dirties the project or adds an undo step.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use rustgis_engine::data::{Feature, Field, sync::Synced};
use rustgis_engine::map::Layer;

/// A layer's answer, or the hash under way for it.
struct Entry {
    rows: Weak<Vec<Feature>>,
    fields: Vec<Field>,
    crs: Option<rustgis_engine::data::raster::Crs>,
    synced: Synced,
    /// The rows' hash once it lands; `Err` when it couldn't be made.
    digest: Option<Result<u64, String>>,
    /// The one hash under way for the layer. One for an earlier state is cancelled and lands
    /// before another starts, so edits in quick succession never hash a layer twice at once.
    job: Option<Job>,
    /// When to try again after the OS refused a thread.
    retry: Option<web_time::Instant>,
    /// The answer for the layer's previous state, shown until this state's hash lands, so the
    /// note doesn't drop out during each edit's hash.
    shown: Option<bool>,
}

/// A hash on its thread: where it lands, its Cancel, and whether it hashes the layer as its entry
/// has it. Dropped, it is cancelled.
struct Job {
    slot: crate::Slot<Result<u64, String>>,
    stop: Arc<AtomicBool>,
    current: bool,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The hash thread's Cancel, as `rows_digest` looks at it.
struct Stop(Arc<AtomicBool>);

impl rustgis_engine::data::progress::Probe for Stop {
    fn cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    fn begin(&self, _: u64) {}
    fn advance(&self, _: u64) {}
}

impl Entry {
    /// Takes in its hash if it has landed: the answer, when it hashed the layer as the entry has
    /// it. Whether a hash landed, freeing its turn.
    fn land(&mut self) -> bool {
        let Some(job) = &self.job else { return false };
        let landed = job.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let Some(digest) = landed else { return false };
        let current = job.current;
        self.job = None;
        if current {
            self.digest = Some(digest);
        }
        true
    }

    /// Whether the layer holds edits its file lacks: its own hash's answer once that lands, else
    /// the previous state's.
    fn answer(&self) -> Option<bool> {
        match &self.digest {
            Some(Ok(digest)) => Some(*digest != self.synced.rows),
            // This state's hash failed: unknown, not the previous state's answer.
            Some(Err(_)) => None,
            None => self.shown,
        }
    }

    /// Whether it answers for `l` as it is now.
    fn of(&self, l: &Layer) -> bool {
        Weak::ptr_eq(&self.rows, &l.dataset.features.allocation_key())
            && self.fields == l.dataset.fields
            && self.crs == l.dataset.crs
            && l.synced.as_ref() == Some(&self.synced)
    }
}

/// How long after the OS refused a thread one is asked for again.
const RETRY: web_time::Duration = web_time::Duration::from_secs(5);

/// Each layer's answer, by its map's instance and its own.
#[derive(Default)]
pub(crate) struct Marks {
    entries: HashMap<(u64, u64), Entry>,
    /// The project revision the entries were last pruned at.
    revision: Option<u64>,
    /// Hashes under way, as counted once a frame ([`tick`]) and as started since.
    running: usize,
    /// The project the entries belong to (`Session::opened`).
    opened: u64,
}

/// Whether `l`, a layer of the active map, holds edits its file lacks: `None` while that isn't
/// known yet (its hash is under way, or couldn't start) or for a layer with no record. Cheap on
/// the UI thread: a hash starts only when the layer changed, on a thread of its own, and the
/// pane is drawn again when it lands.
pub(crate) fn edited(app: &mut crate::RustGisApp, l: &Layer, ctx: &egui::Context) -> Option<bool> {
    let synced = l.synced.as_ref()?;
    let id = (app.session.map().instance_id(), l.instance_id());
    if !app.syncmark.entries.get(&id).is_some_and(|e| e.of(l)) {
        // The earlier hash is stopped, and kept until it lands.
        let replaced = app.syncmark.entries.remove(&id);
        let shown = replaced.as_ref().and_then(Entry::answer);
        let earlier = replaced.and_then(|mut e| e.job.take()).map(|mut job| {
            job.stop.store(true, Ordering::Relaxed);
            job.current = false;
            job
        });
        let entry = Entry {
            rows: l.dataset.features.allocation_key(),
            fields: l.dataset.fields.clone(),
            crs: l.dataset.crs,
            synced: synced.clone(),
            digest: None,
            job: earlier,
            retry: None,
            shown,
        };
        app.syncmark.entries.insert(id, entry);
    }
    let marks = &mut app.syncmark;
    let entry = marks.entries.get_mut(&id)?;
    if entry.land() {
        marks.running = marks.running.saturating_sub(1);
    }
    let running = marks.running;
    let now = web_time::Instant::now();
    if entry.job.is_some() {
        // Looked at again soon: either it lands, or the next hash starts once it has.
        ctx.request_repaint_after(web_time::Duration::from_millis(100));
    } else if entry.digest.is_none() && entry.retry.is_none_or(|at| now >= at) {
        if running >= rustgis_engine::data::threads::max() {
            // Its turn comes when another hash lands.
            ctx.request_repaint_after(web_time::Duration::from_millis(100));
        } else {
            let (slot, stop) = (crate::Slot::default(), Arc::new(AtomicBool::new(false)));
            let (out, probe) = (slot.clone(), Stop(stop.clone()));
            let dataset = l.dataset.clone();
            let spawned = std::thread::Builder::new().name("Edits not published".into()).spawn(move || {
                let _probe = rustgis_engine::data::progress::install(Some(Arc::new(probe)));
                // A panic lands as an answer too, so it never holds the turn. Neither this nor a
                // refused thread below can be brought about in a test: `rows_digest` has no panic
                // path, and the OS refuses threads only under load.
                let digest = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rustgis_engine::data::sync::rows_digest(&dataset)))
                    .map_err(|_| "the hash stopped".to_string())
                    .and_then(|d| d.map_err(|e| e.to_string()));
                *out.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(digest);
            });
            match spawned {
                Ok(_) => {
                    entry.job = Some(Job { slot, stop, current: true });
                    entry.retry = None;
                    marks.running += 1;
                    ctx.request_repaint_after(web_time::Duration::from_millis(100));
                }
                // A thread the OS refuses costs time, not the mark: the layer stays unmarked (an
                // unknown state isn't shown as edited) and it is tried again shortly.
                Err(_) => {
                    entry.retry = Some(now + RETRY);
                    ctx.request_repaint_after(RETRY);
                }
            }
        }
    }
    entry.answer()
}

/// [`edited`] as this state's own hash answers it, `None` until that lands (not the previous
/// state's answer, which [`edited`] shows meanwhile).
#[cfg(test)]
pub(crate) fn hashed(app: &mut crate::RustGisApp, l: &Layer, ctx: &egui::Context) -> Option<bool> {
    edited(app, l, ctx);
    let e = app.syncmark.entries.get(&(app.session.map().instance_id(), l.instance_id()))?;
    match e.digest.as_ref()? {
        Ok(digest) => Some(*digest != e.synced.rows),
        Err(_) => None,
    }
}

/// Once a frame: forgets another project's answers, takes in every finished hash, whether its row is drawn or not (another map, a
/// filtered or collapsed list), so one that landed never holds a turn another layer is waiting
/// for, and counts those still running. Once per project revision, forgets layers that are gone,
/// cancelling their hashes, so the answers kept follow the project.
pub(crate) fn tick(app: &mut crate::RustGisApp) {
    let marks = &mut app.syncmark;
    // Another project: nothing of the old one's applies, and its hashes are cancelled as their
    // entries go.
    if marks.opened != app.session.opened {
        *marks = Marks { opened: app.session.opened, ..Marks::default() };
    }
    for e in marks.entries.values_mut() {
        e.land();
    }
    marks.running = marks.entries.values().filter(|e| e.job.is_some()).count();
    if marks.revision == Some(app.session.revision) {
        return;
    }
    app.syncmark.revision = Some(app.session.revision);
    let live: std::collections::HashSet<(u64, u64)> =
        app.session.project.maps.iter().flat_map(|m| m.layers.iter().map(move |l| (m.instance_id(), l.instance_id()))).collect();
    app.syncmark.entries.retain(|id, _| live.contains(id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustgis_engine::data::sync::FileSync;

    /// An entry whose hash, for the layer as it has it or an earlier state, has landed.
    fn landed(current: bool) -> Entry {
        let slot = crate::Slot::default();
        *slot.lock().unwrap() = Some(Ok(7));
        Entry {
            rows: Weak::new(),
            fields: Vec::new(),
            crs: None,
            synced: Synced { files: FileSync { files: Vec::new() }, rows: 7 },
            digest: None,
            job: Some(Job { slot, stop: Arc::new(AtomicBool::new(false)), current }),
            retry: None,
            shown: Some(true),
        }
    }

    #[test]
    fn a_landed_hash_frees_its_turn_and_answers_only_for_the_state_it_hashed() {
        let mut e = landed(true);
        e.land();
        assert!(e.job.is_none(), "no longer counted as running");
        assert_eq!(e.digest, Some(Ok(7)));
        let mut e = landed(false);
        e.land();
        assert!(e.job.is_none());
        assert_eq!(e.digest, None, "an earlier state's hash doesn't answer");
        assert_eq!(e.answer(), Some(true), "the previous answer stays shown until this state's lands");
        let mut e = landed(true);
        *e.job.as_ref().unwrap().slot.lock().unwrap() = Some(Err("the hash stopped".into()));
        e.land();
        assert_eq!(e.answer(), None, "a failed hash is unknown, not the previous state's answer");
    }
}
