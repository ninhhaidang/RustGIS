//! Budget for feature allocations retained by live data, Undo/Redo and queued readers.
use crate::{Project, Step, notice_text::NoticeText};
use rustgis_data::{Coord, Feature, Features, Geometry, Value};
use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

type Key = Weak<Vec<Feature>>;

/// The allocation identities in a snapshot. The project keeps the corresponding rows alive.
pub(crate) type Allocations = Vec<Key>;

/// Weak registrations let the budget count a queued reader without keeping its rows alive.
#[derive(Clone, Default)]
pub(crate) struct Readers(Arc<Mutex<Vec<Weak<Allocations>>>>);

/// Kept beside a reader's strong row snapshots, through queueing, execution and replay.
#[derive(Clone)]
pub(crate) struct ReadRows {
    _allocations: Arc<Allocations>,
}

impl Readers {
    /// Records identities only; preparing a reader never visits feature contents.
    pub(crate) fn retain(&self, allocations: Allocations) -> ReadRows {
        let allocations = Arc::new(allocations);
        let mut readers = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        readers.retain(|reader| reader.strong_count() > 0);
        readers.push(Arc::downgrade(&allocations));
        ReadRows { _allocations: allocations }
    }

    fn allocations(&self) -> Allocations {
        let mut readers = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        readers.retain(|reader| reader.strong_count() > 0);
        readers.iter().filter_map(Weak::upgrade).flat_map(|rows| rows.as_ref().clone()).collect()
    }
}

pub(crate) struct Budget {
    pub(crate) limit: usize,
    // Command dispatch also runs post-command maintenance, which can record a second step.
    pub(crate) deferred: bool,
    // Dropping steps is said once a project: past the budget every further edit drops one, and
    // a message each time would only repeat the first. Another project says it again.
    said: bool,
    /// The computer's memory the check before an edit plans with ([`rustgis_data::memory::physical`];
    /// tests pass fixed amounts). `None`: not known, as in the browser, and nothing is checked.
    pub(crate) memory: Option<u64>,
    // Dropping steps to make room for an edit is said once a project, as `said` is.
    room_said: bool,
    // The copies of background edits not landed yet ([`Budget::reserve`]).
    in_flight: Arc<AtomicU64>,
    // This session's part of what the process holds ([`Budget::publish`]).
    holding: rustgis_data::memory::Holding,
    /// What the computer has free now ([`rustgis_data::memory::available`]; tests pass fixed
    /// amounts). `None`: not known, and only [`Self::memory`] bounds an edit's copy.
    pub(crate) free: fn() -> Option<u64>,
    // Keeping the weak key prevents address reuse while its estimate is cached, without keeping
    // the rows themselves alive. Copy-on-write gives changed rows a different key.
    estimates: HashMap<usize, (Key, usize)>,
    readers: Readers,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            limit: rustgis_data::memory::undo_history(),
            deferred: false,
            said: false,
            memory: rustgis_data::memory::physical(),
            room_said: false,
            in_flight: Arc::default(),
            holding: rustgis_data::memory::Holding::default(),
            // Unit tests here don't depend on what else runs; they set it where they test it.
            free: if cfg!(test) { || None } else { rustgis_data::memory::available },
            estimates: HashMap::new(),
            readers: Readers::default(),
        }
    }
}

impl Budget {
    pub(crate) fn clear(&mut self) {
        let retained: HashSet<_> = self.readers.allocations().iter().map(|key| key.as_ptr() as usize).collect();
        // Clearing history must not discard the cached measurement of reader-only old rows.
        self.estimates.retain(|id, (key, _)| retained.contains(id) && key.strong_count() > 0);
        self.said = false;
        self.room_said = false;
    }

    /// Keep a contiguous history around the current state. Count a shared allocation only at its
    /// highest-priority owner, so the estimate falls only once its last snapshot is evicted.
    pub(crate) fn trim(&mut self, live: &Project, undo: &mut Vec<Step>, redo: &mut Vec<Step>) -> Option<NoticeText> {
        let mut seen: HashSet<_> = rows(live).map(|features| features.allocation_key().as_ptr() as usize).collect();
        seen.extend(self.readers.allocations().iter().map(|key| key.as_ptr() as usize));
        let mut used = 0usize;
        let undo_protected = usize::from(!undo.is_empty());
        let mut account = |step: &Step| {
            used = used.saturating_add(self.private_bytes(&step.8, &mut seen));
            used <= self.limit
        };

        // Work in reverse eviction order. Keep the newest undo even if it alone exceeds the
        // limit, then the nearest redos, then as many older undos as will fit.
        if let Some(newest) = undo.last() {
            account(newest);
        }
        let redo_keep = redo.iter().rev().take_while(|step| account(step)).count();
        let undo_keep = if redo_keep == redo.len() {
            undo_protected + undo.iter().rev().skip(undo_protected).take_while(|step| account(step)).count()
        } else {
            undo_protected
        };

        let undo_drop = undo.len() - undo_keep;
        let redo_drop = redo.len() - redo_keep;
        undo.drain(..undo_drop);
        redo.drain(..redo_drop);
        self.estimates.retain(|id, (key, _)| seen.contains(id) && key.strong_count() > 0);
        if undo_drop + redo_drop == 0 || std::mem::replace(&mut self.said, true) {
            return None;
        }
        let template = "Undo history is limited by memory: older steps are dropped as you edit, keeping Undo and Redo to about {limit}. The newest step is always kept.";
        Some(NoticeText::format(template, [("limit", size_said(self.limit as u64).into())]))
    }

    /// What the session holds (#2431 fix 3): the rows of the open project and those only Undo and
    /// Redo or queued readers keep, each allocation counted once however many snapshots share it,
    /// and the pixels and embedded files of their rasters likewise. Rows are estimated as for the budget, from a
    /// cache, so this visits allocations, not rows.
    pub(crate) fn held(&mut self, live: &Project, undo: &[Step], redo: &[Step]) -> u64 {
        let mut seen = HashSet::new();
        let mut bytes = self.private_bytes(&allocations(live), &mut seen);
        for step in undo.iter().chain(redo) {
            bytes = bytes.saturating_add(self.private_bytes(&step.8, &mut seen));
        }
        bytes = bytes.saturating_add(self.private_bytes(&self.readers.allocations(), &mut seen));
        let projects = std::iter::once(live).chain(undo.iter().chain(redo).map(|step| &step.0));
        (bytes as u64).saturating_add(raster_bytes(projects))
    }

    /// Says what the session holds ([`Self::held`]) to the process-wide figure the memory limits
    /// plan with ([`rustgis_data::memory::planned`], #2431 fix 3), after each command. Copies still
    /// being made count through their [`Reservation`]s. Nothing is measured where the memory isn't
    /// known (the browser), as no limit follows it there.
    pub(crate) fn publish(&mut self, live: &Project, undo: &[Step], redo: &[Step]) {
        if self.memory.is_some() {
            let held = self.held(live, undo, redo);
            self.holding.set(held);
            // Estimates of rows nothing holds any more go, as `trim` does, which runs only when the
            // history moves: a command that replaces rows without a step would otherwise leave them.
            self.estimates.retain(|_, (key, _)| key.strong_count() > 0);
        }
    }

    /// Makes room for an edit that copies `copy` bytes of rows (#2431 fix 3): when what the session
    /// holds, the copies of edits still running in the background and this copy pass the memory
    /// it plans to use ([`rustgis_data::memory::usable`]), or the copy passes what the computer has
    /// free now less [`rustgis_data::memory::margin`] (other programs use the rest), history is set
    /// aside until they fit. What is set aside counts as free for the copy, before the margin is
    /// taken off: its memory goes back to RustGIS's allocator, which the copy then reuses, not to
    /// the system. Copies of other edits still running count against what is free only once they
    /// have allocated it. Redo is set aside first, farthest first, as the edit clears it anyway,
    /// then Undo, oldest first. What is set aside is freed just before the edit copies, or given
    /// back if it never does ([`Session::put_back`](crate::Session)), and counted as held until
    /// then. `Err` gives what is held and the memory planned for, with the history left as it is,
    /// when the copy wouldn't fit even with the history gone.
    pub(crate) fn make_room(
        &mut self,
        live: &Project,
        undo: &mut Vec<Step>,
        redo: &mut Vec<Step>,
        copy: u64,
    ) -> Result<Option<Aside>, NoRoom> {
        let Some(memory) = self.memory else { return Ok(None) };
        let Some(usable) = rustgis_data::memory::usable(Some(memory)) else { return Ok(None) };
        let running = self.in_flight.load(Ordering::Relaxed);
        let free = (self.free)();
        let margin = rustgis_data::memory::margin(memory);
        let before = self.held(live, undo, redo);
        let planned = |held: u64| held.saturating_add(running).saturating_add(copy) <= usable;
        // What is set aside is added to what is free before the margin is taken off, so it
        // doesn't make up for less free than the margin.
        let fits = |held: u64| {
            planned(held) && free.is_none_or(|free| copy <= free.saturating_add(before.saturating_sub(held)).saturating_sub(margin))
        };
        if fits(before) {
            return Ok(None);
        }
        let open = self.held(live, &[], &[]);
        if !fits(open) {
            // Said by what refuses it: the memory RustGIS plans to use, or what is free now.
            let free = if planned(open) { free } else { None };
            return Err(NoRoom { held: open.saturating_add(running), usable, free });
        }
        let mut aside = Aside::default();
        while !fits(self.held(live, undo, redo)) {
            if !redo.is_empty() {
                aside.redo.push(redo.remove(0));
            } else if undo.is_empty() {
                break;
            } else {
                aside.undo.push(undo.remove(0));
            }
        }
        let after = self.held(live, undo, redo);
        aside.held = Some(self.reserve(before.saturating_sub(after)));
        Ok(Some(aside).filter(|aside| aside.len() > 0))
    }

    /// Whether dropping steps to make room is to be said: once a project.
    pub(crate) fn say_room(&mut self) -> bool {
        !std::mem::replace(&mut self.room_said, true)
    }

    /// Counts `bytes` as held until the reservation is dropped: the copy an edit running in the
    /// background makes, until it lands (and the project holds it) or is abandoned.
    pub(crate) fn reserve(&self, bytes: u64) -> Reservation {
        self.in_flight.fetch_add(bytes, Ordering::Relaxed);
        Reservation { in_flight: Arc::clone(&self.in_flight), bytes, _held: rustgis_data::memory::Holding::of(bytes) }
    }

    /// Shared with script workers, so their retained inputs stay in the initiating budget.
    pub(crate) fn readers(&self) -> Readers {
        self.readers.clone()
    }

    /// Joins the initiating session's registry without charging a shared copy independently.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn use_readers(&mut self, readers: Readers) {
        self.readers = readers;
    }

    /// The estimated size of `features`' rows: what the first write to them copies while a
    /// snapshot shares them.
    pub(crate) fn rows_bytes(&mut self, features: &Features) -> u64 {
        self.private_bytes(&vec![features.allocation_key()], &mut HashSet::new()) as u64
    }

    /// Measure only history-owned allocations with no cached estimate. The other work visits
    /// cached allocation identities, independent of the number of rows.
    fn private_bytes(&mut self, allocations: &Allocations, seen: &mut HashSet<usize>) -> usize {
        allocations.iter().fold(0, |sum, key| {
            let id = key.as_ptr() as usize;
            if !seen.insert(id) {
                return sum;
            }
            let (_, bytes) = self.estimates.entry(id).or_insert_with(|| {
                let bytes = key.upgrade().as_deref().map_or(0, estimate);
                (key.clone(), bytes)
            });
            sum.saturating_add(*bytes)
        })
    }
}

/// Undo and Redo steps set aside to make room for an edit ([`Budget::make_room`]), oldest first.
#[derive(Default)]
pub(crate) struct Aside {
    pub(crate) undo: Vec<Step>,
    pub(crate) redo: Vec<Step>,
    /// The steps' own memory, still held until they are freed or put back: counted by the checks
    /// of other edits meanwhile.
    held: Option<Reservation>,
}

impl Aside {
    pub(crate) fn len(&self) -> usize {
        self.undo.len() + self.redo.len()
    }
}

/// Steps set aside, shared with the hook that frees them just before the edit copies rows, on
/// whichever thread it runs.
pub(crate) type AsideSlot = Arc<Mutex<Option<Aside>>>;

/// The hook [`rustgis_data::before_rows_copy`] calls: frees the steps in `slot`.
pub(crate) fn free_on_copy(slot: &AsideSlot) -> Box<dyn FnOnce()> {
    let slot = Arc::clone(slot);
    Box::new(move || drop(slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()))
}

/// Why an edit's copy can't fit even with no history ([`Budget::make_room`]): what RustGIS holds
/// (the copies still being made included) and the memory it plans to use, and what the computer
/// has free when that is what refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NoRoom {
    pub(crate) held: u64,
    pub(crate) usable: u64,
    pub(crate) free: Option<u64>,
}

/// A background edit's copy, counted as held while it runs ([`Budget::reserve`]).
pub struct Reservation {
    in_flight: Arc<AtomicU64>,
    bytes: u64,
    // Counted in what the process holds too, for the limits (#2431), until dropped.
    _held: rustgis_data::memory::Holding,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // Only what `reserve` added is taken away, so this never goes below zero.
        self.in_flight.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// The pixels and embedded files of the rasters in `projects`, each shared allocation once.
fn raster_bytes<'a>(projects: impl Iterator<Item = &'a Project>) -> u64 {
    let mut seen = HashSet::new();
    let mut bytes = 0u64;
    let rasters = projects
        .flat_map(|project| project.maps.iter().chain(project.offline_areas.iter().map(|area| &area.data)))
        .flat_map(|map| map.layers.iter())
        .filter_map(|layer| layer.raster.as_ref());
    for raster in rasters {
        if let Some(data) = raster.data.as_ref().filter(|data| seen.insert(std::sync::Arc::as_ptr(data) as usize)) {
            let pixels = data.bands.iter().fold(0usize, |n, band| n.saturating_add(band.data.capacity().saturating_mul(size_of::<f32>())));
            bytes = bytes.saturating_add(pixels as u64);
        }
        if let Some(file) = raster.embedded.as_ref().filter(|file| seen.insert(std::sync::Arc::as_ptr(file) as usize)) {
            bytes = bytes.saturating_add(file.capacity() as u64);
        }
    }
    bytes
}

/// A size as notices say it: GiB or MiB to one decimal, or bytes.
pub(crate) fn size_said(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GiB", bytes as f64 / (1_u64 << 30) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.1} MiB", bytes as f64 / (1_u64 << 20) as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Records identities once when the snapshot enters history or has a reservation removed.
pub(crate) fn allocations(project: &Project) -> Allocations {
    rows(project).map(Features::allocation_key).collect()
}

fn rows(project: &Project) -> impl Iterator<Item = &Features> {
    project
        .maps
        .iter()
        .chain(project.offline_areas.iter().map(|area| &area.data))
        .flat_map(|map| map.layers.iter().map(|layer| &layer.dataset.features))
        .chain(project.tables.iter().map(|table| &table.dataset.features))
        .filter(|features| features.capacity() > 0)
}

/// Count the row vector's capacity and extrapolate the heap storage of up to 64 evenly spaced
/// rows, each heap allocation as a block of the system allocator ([`block`]).
fn estimate(features: &Vec<Feature>) -> usize {
    let stride = features.len().div_ceil(64).max(1);
    let (sum, count) =
        features.iter().step_by(stride).fold((0usize, 0usize), |(sum, count), row| (sum.saturating_add(row_bytes(row)), count + 1));
    block(features.capacity().saturating_mul(size_of::<Feature>()))
        .saturating_add(sum.div_ceil(count.max(1)).saturating_mul(features.len()))
}

/// A row's heap storage, block by block: its attribute list, each text, and its geometry's parts.
fn row_bytes(row: &Feature) -> usize {
    let attrs = row.attrs.iter().fold(block(row.attrs.capacity().saturating_mul(size_of::<Value>())), |n, v| {
        n.saturating_add(match v {
            Value::Text(s) => block(s.capacity()),
            _ => 0,
        })
    });
    let ring = |r: &Vec<Coord>| block(r.capacity().saturating_mul(size_of::<Coord>()));
    let parts =
        |p: &Vec<Vec<Coord>>| p.iter().fold(block(p.capacity().saturating_mul(size_of::<Vec<Coord>>())), |n, r| n.saturating_add(ring(r)));
    let geometry = match &row.geometry {
        None | Some(Geometry::Point(_)) => 0,
        Some(Geometry::MultiPoint(p)) => ring(p),
        Some(Geometry::Line(p)) => parts(p),
        Some(Geometry::Polygon(p)) => {
            p.iter().fold(block(p.capacity().saturating_mul(size_of::<Vec<Vec<Coord>>>())), |n, r| n.saturating_add(parts(r)))
        }
    };
    attrs.saturating_add(geometry)
}

/// What an allocation of `bytes` takes from the system allocator: a header and 16-byte rounding,
/// 32 bytes at the least (glibc's malloc; others are alike). It is most of a short text's cost:
/// a layer of short texts measured about 0.74 GB a copy where its bytes alone came to 0.54
/// (#2431). Nothing is allocated for none.
fn block(bytes: usize) -> usize {
    if bytes == 0 { 0 } else { bytes.saturating_add(8 + 15).max(32) & !15 }
}

#[cfg(test)]
mod tests {
    use crate::Session;
    use rustgis_data::{Dataset, Field, FieldType, GeometryType, Value};
    use serde_json::json;

    fn session(text_len: usize) -> Session {
        let mut session = Session::new(crate::Project::default());
        let mut data = Dataset::new("rows", GeometryType::Point, vec![Field::new("text", FieldType::Text)]);
        data.push(Some(rustgis_data::Geometry::Point(rustgis_geom::Coord::new(0.0, 0.0))), vec![Value::Text("a".repeat(text_len))]);
        session.map_mut().add_layer(rustgis_map::Layer::new(0, data));
        session.undo_memory.limit = 2 << 10;
        // What the computer has free is left out unless a test sets it, so tests don't depend on
        // what else runs.
        session.undo_memory.free = || None;
        session
    }

    fn edit(session: &mut Session, text: &str) {
        session.run("edit.setValue", &json!({"layer": "rows", "row": 0, "field": "text", "value": text})).unwrap();
    }

    fn value(session: &Session) -> &Value {
        &session.map().layer_by_name("rows").unwrap().dataset.features[0].attrs[0]
    }

    #[test]
    fn shared_snapshots_are_counted_once_and_dropping_is_said_once() {
        let mut session = session(1024);
        let mark = session.notice_mark();
        for visible in [false, true] {
            session.run("layer.visibility", &json!({"layer": "rows", "visible": visible})).unwrap();
        }
        let b = "b".repeat(1024);
        let c = "c".repeat(1024);
        edit(&mut session, &b);
        assert_eq!(session.undo_depth(), 3);
        assert!(session.notices_since(mark).is_empty());

        // Both visibility steps and the first edit own the same initial rows. All three have
        // to go before the second edit fits, while the remaining step spans b to c.
        edit(&mut session, &c);
        let notices = session.notices_since(mark);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].text.contains("older steps are dropped"), "{notices:?}");
        assert_eq!(session.undo_depth(), 1);

        let mark = session.notice_mark();
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text(b.clone()));
        session.run("edit.redo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text(c.clone()));
        assert!(session.notices_since(mark).is_empty());

        // Past the budget each edit drops a step again, without saying it again.
        let mark = session.notice_mark();
        edit(&mut session, &b);
        assert_eq!(session.undo_depth(), 1);
        assert!(session.notices_since(mark).is_empty());

        // Another project's first drop is said again.
        session.clear_history();
        edit(&mut session, &c);
        let mark = session.notice_mark();
        edit(&mut session, &b);
        assert_eq!(session.notices_since(mark).len(), 1);
    }

    #[test]
    fn keeps_the_newest_undo_even_when_its_rows_exceed_the_budget() {
        let mut session = session(4096);
        edit(&mut session, "small");
        assert_eq!(session.undo_depth(), 1);
        assert!(session.notices.is_empty());
        edit(&mut session, "smaller");
        assert_eq!(session.undo_depth(), 1);
        assert_eq!(session.notices.len(), 1);
        assert!(session.notices[0].text.contains("older steps are dropped"));
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text("small".into()));
    }

    #[test]
    fn cuts_the_farthest_redo_when_undo_moves_large_live_rows_into_history() {
        let mut session = session(1);
        let b = "b".repeat(1200);
        let c = "c".repeat(1200);
        edit(&mut session, &b);
        edit(&mut session, &c);
        let mark = session.notice_mark();
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text(b.clone()));
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text("a".into()));
        let notices = session.notices_since(mark);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].text.contains("older steps are dropped"));
        session.run("edit.redo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text(b));
        assert!(!session.can_redo(), "only the nearer redo fits");
    }

    /// Memory planned so the open rows and one copy of them fit with `spare` bytes over,
    /// without the history.
    fn room_for_one_copy(session: &mut Session, spare: u64) -> u64 {
        let rows = session.map().layer_by_name("rows").unwrap().dataset.features.clone();
        let copy = session.undo_memory.rows_bytes(&rows);
        let open = session.undo_memory.held(&session.project, &[], &[]);
        let target = open + copy + spare;
        // `usable` leaves a quarter: the smallest memory whose three quarters reach the target.
        let memory = (target * 4).div_ceil(3) + 4;
        assert!(rustgis_data::memory::usable(Some(memory)).unwrap() >= target);
        memory
    }

    #[test]
    fn an_edit_that_fits_beside_the_history_drops_nothing() {
        let mut session = session(4096);
        session.undo_memory.limit = 1 << 30;
        session.undo_memory.memory = Some(1 << 40);
        let mark = session.notice_mark();
        edit(&mut session, "b");
        edit(&mut session, "c");
        assert_eq!(session.undo_depth(), 2);
        assert!(session.notices_since(mark).is_empty());
    }

    #[test]
    fn older_steps_make_room_for_an_edit_and_that_is_said_once() {
        let mut session = session(4096);
        session.undo_memory.limit = 1 << 30;
        session.undo_memory.memory = None;
        let (b, c) = ("b".repeat(4096), "c".repeat(4096));
        edit(&mut session, &b);
        edit(&mut session, &c);
        assert_eq!(session.undo_depth(), 2);
        let memory = room_for_one_copy(&mut session, 64);
        session.undo_memory.memory = Some(memory);
        let mark = session.notice_mark();
        edit(&mut session, "d");
        let notices = session.notices_since(mark);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert!(notices[0].text.contains("dropped to make room for edits (2 before editing rows)"), "{notices:?}");
        assert_eq!(value(&session), &Value::Text("d".into()));
        // The step the edit made undoes it.
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text(c.clone()));
        session.run("edit.redo", &json!({})).unwrap();

        // Dropping again says nothing more in the same project.
        edit(&mut session, &b);
        let mark = session.notice_mark();
        session.undo_memory.memory = Some(room_for_one_copy(&mut session, 64));
        edit(&mut session, &c);
        assert!(session.notices_since(mark).is_empty());
        assert_eq!(session.undo_depth(), 1, "the older steps went");
        assert_eq!(value(&session), &Value::Text(c));
    }

    #[test]
    fn an_edit_that_cannot_fit_is_refused_and_changes_nothing() {
        let mut session = session(4096);
        session.undo_memory.limit = 1 << 30;
        session.undo_memory.memory = None;
        edit(&mut session, "b");
        let state = session.state;
        session.undo_memory.memory = Some(4);
        let mark = session.notice_mark();
        let err = session.run("edit.setValue", &json!({"layer": "rows", "row": 0, "field": "text", "value": "c"})).unwrap_err();
        assert!(err.contains("not enough memory to edit rows: the edit copies its rows"), "{err}");
        assert_eq!(value(&session), &Value::Text("b".into()));
        assert_eq!(session.undo_depth(), 1, "the history is kept");
        assert_eq!(session.state, state);
        let notices = session.notices_since(mark);
        assert_eq!(notices.len(), 1, "{notices:?}");
        // Commands that don't edit features still run.
        session.run("layer.visibility", &json!({"layer": "rows", "visible": false})).unwrap();
        // Unknown memory (the browser) checks nothing.
        session.undo_memory.memory = None;
        edit(&mut session, "c");
        assert_eq!(value(&session), &Value::Text("c".into()));
    }

    #[test]
    fn queued_readers_keep_one_cached_allocation_after_history_is_cleared() {
        let mut session = session(4096);
        session.undo_memory.memory = None;
        let first = session.map().layer_by_name("rows").unwrap().dataset.clone();
        let second = first.clone();
        let key = first.features.allocation_key();
        let size = session.undo_memory.rows_bytes(&first.features);
        let readers = session.undo_memory.readers();
        let one = readers.retain(vec![key.clone()]);
        let two = readers.retain(vec![key.clone(), key.clone()]);
        assert_eq!(session.undo_memory.held(&session.project, &[], &[]), size, "live and duplicate readers share rows");
        edit(&mut session, &"b".repeat(4096));
        session.clear_history();
        let live = session.map().layer_by_name("rows").unwrap().dataset.features.clone();
        let live_size = session.undo_memory.rows_bytes(&live);
        assert!(session.undo_memory.estimates.contains_key(&(key.as_ptr() as usize)), "reader-only size stays cached");
        assert_eq!(session.undo_memory.held(&session.project, &[], &[]), size + live_size);
        drop((one, first));
        assert_eq!(session.undo_memory.held(&session.project, &[], &[]), size + live_size, "another reader still owns the old rows");
        drop((two, second));
        assert_eq!(session.undo_memory.held(&session.project, &[], &[]), live_size);
    }

    #[test]
    fn queued_reader_rows_cannot_be_evicted_to_admit_an_edit() {
        let mut session = session(4096);
        session.undo_memory.memory = None;
        let snapshot = session.map().layer_by_name("rows").unwrap().dataset.clone();
        let retained = session.undo_memory.readers().retain(vec![snapshot.features.allocation_key()]);
        edit(&mut session, &"b".repeat(4096));
        session.clear_history();
        let live = session.map().layer_by_name("rows").unwrap().dataset.features.clone();
        let size = session.undo_memory.rows_bytes(&live);
        // Room for current rows and their copy, but not the old rows the reader still needs.
        session.undo_memory.memory = Some(((2 * size + 64) * 4).div_ceil(3) + 4);
        let p = json!({"layer":"rows", "row":0, "field":"text", "value":"c"});
        let error = session.run("edit.setValue", &p).unwrap_err();
        assert!(error.contains("not enough memory to edit rows"), "{error}");
        assert_eq!(value(&session), &Value::Text("b".repeat(4096)));
        assert_eq!(session.undo_depth(), 0);
        drop((retained, snapshot, live));
        session.run("edit.setValue", &p).unwrap();
        assert_eq!(value(&session), &Value::Text("c".into()));
    }

    #[test]
    fn redo_goes_before_undo_to_make_room() {
        let mut session = session(4096);
        session.undo_memory.limit = 1 << 30;
        session.undo_memory.memory = None;
        for text in ["b", "c", "d"] {
            edit(&mut session, &text.repeat(4096));
        }
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!((session.undo_depth(), session.can_redo()), (2, true));
        // Room for the open rows, the Undo steps and one copy: only Redo has to go.
        let rows = session.map().layer_by_name("rows").unwrap().dataset.features.clone();
        let copy = session.undo_memory.rows_bytes(&rows);
        drop(rows);
        let held = session.undo_memory.held(&session.project, &session.undo, &[]);
        session.undo_memory.memory = Some(((held + copy + 64) * 4).div_ceil(3) + 4);
        let mark = session.notice_mark();
        edit(&mut session, "e");
        let notices = session.notices_since(mark);
        assert!(notices.iter().any(|n| n.text.contains("(1 before editing rows)")), "{notices:?}");
        assert_eq!(session.undo_depth(), 3, "every Undo step is kept");
    }

    /// A session with a table of one long text, added and edited twice: three Undo steps, two of
    /// them holding their own rows.
    #[cfg(not(target_arch = "wasm32"))]
    fn table_session() -> Session {
        let mut session = Session::new(crate::Project::default());
        let text = format!("n,t\n1,{}\n", "a".repeat(4096));
        session.run("layer.addDataText", &json!({"fileName": "calc.csv", "text": text})).unwrap();
        session.undo_memory.limit = 1 << 30;
        session.undo_memory.memory = None;
        session.undo_memory.free = || None;
        for t in ["b", "c"] {
            session.run("edit.setValue", &json!({"table": "calc", "row": 0, "field": "t", "value": t.repeat(4096)})).unwrap();
        }
        session
    }

    /// Memory planned so the open rows, the copies in flight and one copy of the table fit with a
    /// little over, without the history.
    #[cfg(not(target_arch = "wasm32"))]
    fn room_for_one_table_copy(session: &mut Session) -> u64 {
        let rows = session.project.tables[0].dataset.features.clone();
        let copy = session.undo_memory.rows_bytes(&rows);
        drop(rows);
        let open =
            session.undo_memory.held(&session.project, &[], &[]) + session.undo_memory.in_flight.load(std::sync::atomic::Ordering::Relaxed);
        ((open + copy + 64) * 4).div_ceil(3) + 4
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_table_edit_run_in_the_background_is_checked_when_it_starts() {
        use crate::cmd::reads::read_ahead;
        let mut session = table_session();
        let p = json!({"table": "calc", "row": 0, "field": "n", "value": 3});
        session.undo_memory.memory = Some(4);
        let ahead = read_ahead(&mut session, "edit.setValue", &p).ok().unwrap();
        let err = ahead.refresh_table_input(&mut session).err().unwrap();
        assert!(err.contains("not enough memory to edit calc"), "{err}");
        assert_eq!(session.undo_depth(), 3, "a refused edit keeps the history");

        // Room for one copy: the history is set aside when the edit starts.
        session.undo_memory.memory = Some(room_for_one_table_copy(&mut session));
        let first = read_ahead(&mut session, "edit.setValue", &p).ok().unwrap().refresh_table_input(&mut session).ok().unwrap();
        assert_eq!(session.undo_depth(), 0);
        // A second edit starting while the first one's copy is in flight sees that copy.
        let second = read_ahead(&mut session, "edit.setValue", &p).ok().unwrap();
        let err = second.refresh_table_input(&mut session).err().unwrap();
        assert!(err.contains("not enough memory to edit calc"), "{err}");

        // Cancelled before it ran: its history comes back at the next check, as nothing else changed.
        drop(first);
        session.undo_memory.memory = Some(1 << 40);
        let ahead = read_ahead(&mut session, "edit.setValue", &p).ok().unwrap().refresh_table_input(&mut session).ok().unwrap();
        assert_eq!(session.undo_depth(), 3, "the history set aside comes back");
        assert_eq!(
            session.undo_memory.in_flight.load(std::sync::atomic::Ordering::Relaxed),
            session.undo_memory.rows_bytes(&session.project.tables[0].dataset.features.clone())
        );
        drop(ahead);
        assert_eq!(session.undo_memory.in_flight.load(std::sync::atomic::Ordering::Relaxed), 0, "the reservation ends with the edit");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_background_table_edit_frees_the_history_when_it_copies_and_says_so() {
        use crate::cmd::reads::read_ahead;
        let mut session = table_session();
        let p = json!({"table": "calc", "row": 0, "field": "t", "value": "d"});
        session.undo_memory.memory = Some(room_for_one_table_copy(&mut session));
        let mark = session.notice_mark();
        let ahead = read_ahead(&mut session, "edit.setValue", &p).ok().unwrap().refresh_table_input(&mut session).ok().unwrap();
        let read = ahead.run(&rustgis_analysis::control::Control::stoppable()).ok().unwrap();
        session.finish_read(read).unwrap();
        assert_eq!(session.project.tables[0].dataset.features[0].attrs[1], Value::Text("d".into()));
        let notices = session.notices_since(mark);
        assert!(notices.iter().any(|n| n.text.contains("(3 before editing calc)")), "{notices:?}");
        assert_eq!(session.undo_depth(), 1, "the edit's own step");
        assert_eq!(session.undo_memory.in_flight.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_second_background_edit_needing_history_waits_for_the_first() {
        use crate::cmd::reads::read_ahead;
        let mut session = table_session();
        // A small table to edit: its copy is far smaller than a step of `calc`'s rows.
        session.run("layer.addDataText", &json!({"fileName": "tiny.csv", "text": "n\n1\n"})).unwrap();
        let p = json!({"table": "tiny", "row": 0, "field": "n", "value": 3});
        let tiny = session.project.tables.iter().find(|t| t.dataset.name == "tiny").unwrap().dataset.features.clone();
        let small = session.undo_memory.rows_bytes(&tiny);
        drop(tiny);
        let rows = session.undo_memory.rows_bytes(&session.project.tables[0].dataset.features.clone());
        let open = session.undo_memory.held(&session.project, &[], &[]);
        // Room for the open rows, one step of `calc`'s rows (a step takes a little more than the
        // rows) and two small copies: the first edit sets aside the oldest step with rows; the
        // second, with the first's copy and steps still counted, would need the next one.
        session.undo_memory.memory = Some(((open + rows + 2 * small + 1024) * 4).div_ceil(3) + 4);
        let first = read_ahead(&mut session, "edit.setValue", &p).ok().unwrap().refresh_table_input(&mut session).ok().unwrap();
        let (depth, in_flight) = (session.undo_depth(), session.undo_memory.in_flight.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(depth, 2, "calc's addition and first edit are aside");
        assert!(in_flight > small, "the steps aside are counted with the copy: {in_flight}");
        let err = read_ahead(&mut session, "edit.setValue", &p).ok().unwrap().refresh_table_input(&mut session).err().unwrap();
        assert!(err.contains("while another edit is running"), "{err}");
        assert_eq!(session.undo_depth(), depth, "what the second set aside is back");
        assert_eq!(session.undo_memory.in_flight.load(std::sync::atomic::Ordering::Relaxed), in_flight);
        drop(first);
        session.reclaim_rooms();
        assert_eq!(session.undo_depth(), 4);
        assert_eq!(session.undo_memory.in_flight.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_layer_edit_waiting_in_the_background_is_refused_once_its_layer_changed() {
        use crate::cmd::reads::read_ahead;
        let mut session = Session::new(crate::Project::default());
        let text = json!({"type": "FeatureCollection", "features": [{"type": "Feature", "properties": {"day": "01/02/2024"},
            "geometry": {"type": "Point", "coordinates": [10.01, 45.01]}}]})
        .to_string();
        session.run("layer.addDataText", &json!({"fileName": "dated.geojson", "text": text})).unwrap();
        session.run("edit.enable", &json!({"on": true})).unwrap();
        let p = json!({"layer": "dated", "field": "day", "type": "date", "format": "%m/%d/%Y"});
        let ahead = read_ahead(&mut session, "edit.convertField", &p).ok().unwrap();
        session.run("edit.setValue", &json!({"layer": "dated", "row": 0, "field": "day", "value": "02/03/2024"})).unwrap();
        let err = ahead.refresh_table_input(&mut session).err().unwrap();
        assert!(err.contains("the layer changed while the edit was waiting"), "{err}");
        // Unchanged, it starts.
        let ahead = read_ahead(&mut session, "edit.convertField", &p).ok().unwrap();
        assert!(ahead.refresh_table_input(&mut session).is_ok());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_cancelled_background_edit_gives_its_history_back_or_says_it_went() {
        use crate::cmd::reads::read_ahead;
        let p = json!({"table": "calc", "row": 0, "field": "n", "value": 3});

        // Cancelled before copying, nothing else done: Undo has its steps again at once.
        let mut session = table_session();
        session.undo_memory.memory = Some(room_for_one_table_copy(&mut session));
        let ahead = read_ahead(&mut session, "edit.setValue", &p).ok().unwrap().refresh_table_input(&mut session).ok().unwrap();
        assert_eq!(session.undo_depth(), 0);
        drop(ahead);
        let mark = session.notice_mark();
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(session.undo_depth(), 2, "three came back, one was undone");
        assert!(session.notices_since(mark).is_empty());

        // The project changed meanwhile: the steps can't go back in place, and that is said.
        let mut session = table_session();
        session.undo_memory.memory = Some(room_for_one_table_copy(&mut session));
        let ahead = read_ahead(&mut session, "edit.setValue", &p).ok().unwrap().refresh_table_input(&mut session).ok().unwrap();
        session.undo_memory.memory = None;
        session.run("layer.addDataText", &json!({"fileName": "other.csv", "text": "a\n1\n"})).unwrap();
        drop(ahead);
        let mark = session.notice_mark();
        session.run("layer.addDataText", &json!({"fileName": "third.csv", "text": "a\n1\n"})).unwrap();
        let notices = session.notices_since(mark);
        assert!(notices.iter().any(|n| n.text.contains("background edit of calc is dropped (3 Undo or Redo steps)")), "{notices:?}");
        assert!(!notices.iter().any(|n| n.text.contains("make room")), "{notices:?}");
        assert_eq!(session.undo_depth(), 2, "only the two later steps");
    }

    #[test]
    fn an_edit_of_a_row_past_the_end_keeps_the_history_set_aside_for_it() {
        let mut session = session(4096);
        session.undo_memory.limit = 1 << 30;
        session.undo_memory.memory = None;
        for text in ["b", "c"] {
            edit(&mut session, &text.repeat(4096));
        }
        session.run("edit.undo", &json!({})).unwrap();
        session.undo_memory.memory = Some(room_for_one_copy(&mut session, 64));
        let mark = session.notice_mark();
        let err = session.run("edit.setValue", &json!({"layer": "rows", "row": 5, "field": "text", "value": "x"})).unwrap_err();
        assert!(err.contains("no such row"), "{err}");
        assert_eq!((session.undo_depth(), session.can_redo()), (1, true), "Undo and Redo are kept");
        assert!(!session.notices_since(mark).iter().any(|n| n.text.contains("make room")));
    }

    #[test]
    fn a_failed_edit_keeps_the_history_set_aside_for_it() {
        let mut session = session(4096);
        session.undo_memory.limit = 1 << 30;
        session.undo_memory.memory = None;
        for text in ["b", "c"] {
            edit(&mut session, &text.repeat(4096));
        }
        session.undo_memory.memory = Some(room_for_one_copy(&mut session, 64));
        let mark = session.notice_mark();
        // An unknown field fails before anything is written.
        assert!(session.run("edit.setValue", &json!({"layer": "rows", "row": 0, "field": "nope", "value": "x"})).is_err());
        assert_eq!(session.undo_depth(), 2, "nothing was copied, so nothing was dropped");
        assert!(!session.notices_since(mark).iter().any(|n| n.text.contains("make room")));
        // The same edit with a real field copies, and drops them.
        edit(&mut session, "d");
        assert_eq!(session.undo_depth(), 1);
        assert!(session.notices_since(mark).iter().any(|n| n.text.contains("(2 before editing rows)")));
    }

    #[test]
    fn shape_edits_that_change_nothing_keep_the_history_set_aside_for_them() {
        let mut session = session(4096);
        session.undo_memory.limit = 1 << 30;
        session.undo_memory.memory = None;
        // A selection with no shape, so Move changes nothing.
        let id = session.map().layer_by_name("rows").unwrap().id;
        let layer = session.map_mut().layer_mut(id).unwrap();
        layer.dataset.features[0].geometry = None;
        layer.selection = vec![0];
        session.active_layer = Some(id);
        for text in ["b", "c"] {
            edit(&mut session, &text.repeat(4096));
        }
        session.undo_memory.memory = Some(room_for_one_copy(&mut session, 64));
        let mark = session.notice_mark();
        let point = json!({"type": "Point", "coordinates": [1.0, 1.0]});
        let err = session.run("edit.setGeometry", &json!({"layer": "rows", "row": 5, "geometry": point})).unwrap_err();
        assert!(err.contains("no such row"), "{err}");
        assert_eq!(session.undo_depth(), 2, "Set Geometry of a row past the end keeps the history");
        let err = session.run("edit.move", &json!({"dx": 1.0, "dy": 1.0})).unwrap_err();
        assert!(err.contains("nothing selected"), "{err}");
        assert_eq!(session.undo_depth(), 2, "Move that changes nothing keeps the history");
        assert!(!session.notices_since(mark).iter().any(|n| n.text.contains("make room")));
    }

    /// After each command a session says what it holds to the process-wide figure the memory
    /// limits plan with, and takes it back when dropped; a background copy counts until it is
    /// dropped; where the memory isn't known nothing is measured (#2431).
    #[test]
    fn a_session_says_what_it_holds_for_the_limits() {
        let mut session = session(4096);
        session.undo_memory.memory = Some(1 << 40);
        edit(&mut session, "b");
        let held = session.undo_memory.held(&session.project, &session.undo, &session.redo);
        assert!(held > 4096, "{held}");
        assert_eq!(session.undo_memory.holding.bytes(), held);
        let copy = session.undo_memory.reserve(1 << 20);
        assert_eq!(copy._held.bytes(), 1 << 20, "a copy being made is counted");
        drop(copy);
        // Unknown memory: what was said stays as it was, and nothing is measured.
        let mut unknown = self::session(4096);
        unknown.undo_memory.memory = None;
        edit(&mut unknown, "c");
        assert_eq!(unknown.undo_memory.holding.bytes(), 0);
    }

    /// The computer's free memory bounds an edit's copy too, when other programs use most of it
    /// (#2431): history set aside counts as free for the copy, as the copy reuses its memory; an
    /// edit whose copy doesn't fit in what is free even then is refused, saying so.
    #[test]
    fn an_edit_fits_in_what_the_computer_has_free() {
        let mut session = session(4096);
        session.undo_memory.limit = 1 << 30;
        session.undo_memory.memory = None;
        for text in ["b", "c"] {
            edit(&mut session, &text.repeat(4096));
        }
        // Plenty planned for (1 TiB), but only the margin and a little free.
        session.undo_memory.memory = Some(1 << 40);
        session.undo_memory.free = || Some(rustgis_data::memory::margin(1 << 40) + 64);
        let mark = session.notice_mark();
        edit(&mut session, "d");
        assert!(session.undo_depth() < 3, "older steps were dropped for the copy");
        assert!(session.notices_since(mark).iter().any(|n| n.text.contains("make room")));
        // Nothing free beyond the margin and no history to set aside: refused, saying why.
        session.clear_history();
        session.undo_memory.free = || Some(rustgis_data::memory::margin(1 << 40));
        let err = session.run("edit.setValue", &json!({"layer": "rows", "row": 0, "field": "text", "value": "e"})).unwrap_err();
        assert!(err.contains("the computer has only about") && err.contains("as other programs"), "{err}");
        assert_eq!(value(&session), &Value::Text("d".into()), "nothing changed");
        // Less free than the margin: steps set aside make room only beyond it, so an edit that
        // fits in them alone is refused, with the history left as it is.
        for text in ["f", "g"] {
            session.undo_memory.memory = None;
            edit(&mut session, &text.repeat(4096));
        }
        session.undo_memory.memory = Some(1 << 40);
        session.undo_memory.free = || Some(rustgis_data::memory::margin(1 << 40) - (1 << 20));
        let depth = session.undo_depth();
        let err = session.run("edit.setValue", &json!({"layer": "rows", "row": 0, "field": "text", "value": "h"})).unwrap_err();
        assert!(err.contains("the computer has only about"), "{err}");
        assert_eq!(session.undo_depth(), depth, "the history is left as it is");
        assert_eq!(value(&session), &Value::Text("g".repeat(4096)), "nothing changed");
    }

    /// Undo and Redo copy no rows and aren't checked (they are registered as `.view()`, so they
    /// don't count as edits): they run when even one copy of the active layer doesn't fit, as the
    /// way back from an edit (#2431).
    #[test]
    fn undo_and_redo_run_when_an_edit_would_not_fit() {
        let mut session = session(4096);
        session.undo_memory.limit = 1 << 30;
        session.undo_memory.memory = None;
        for text in ["b", "c"] {
            edit(&mut session, &text.repeat(4096));
        }
        session.active_layer = Some(session.map().layer_by_name("rows").unwrap().id);
        session.undo_memory.memory = Some(4);
        assert!(session.run("edit.setValue", &json!({"layer": "rows", "row": 0, "field": "text", "value": "d"})).is_err());
        session.run("edit.undo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text("b".repeat(4096)));
        session.run("edit.redo", &json!({})).unwrap();
        assert_eq!(value(&session), &Value::Text("c".repeat(4096)));
        assert_eq!((session.undo_depth(), session.can_redo()), (2, false));
    }

    #[test]
    fn paste_special_is_not_measured_by_the_active_layer() {
        let mut session = session(4096);
        session.undo_memory.memory = Some(4);
        let id = Some(session.map().layer_by_name("rows").unwrap().id);
        assert!(matches!(session.room_for_edit("edit.pasteSpecial", true, "Paste Special", id, None), Ok(None)));
        assert!(session.room_for_edit("edit.paste", true, "Paste", id, None).is_err());
        // Editing settings copy no rows.
        for command in ["edit.elevation", "edit.groundToGrid"] {
            assert!(matches!(session.room_for_edit(command, true, "Settings", id, None), Ok(None)), "{command}");
        }
    }
}
