//! Reads the desktop app makes on a thread of its own before a command adds what they read
//! (#1291 step 3): Add Data's whole-file reads (a raster, a raster GDAL converts, a GeoJSON,
//! delimited text or spreadsheet read whole, a ZIP or CityJSON/CityGML model's layers, the opening
//! of a large Shapefile or GeoJSON file read in place, #2446), Repair/Reload Data, Load All
//! Features, Convert To Annotation's read of a layer read in place, native models or scripts, the
//! Error Inspector's topology check (#1291), Source File State's hashing of a layer's file, Column Statistics' count of a column and Import QGIS Style's file and images. [`read_ahead`]
//! takes what the read needs of the session, [`ReadAhead::run`] reads on the thread, and [`read_land`]
//! runs the admitted command under its initiating owner: the command takes
//! it instead of reading again, so what it adds, says, logs and puts in Undo is the same as when
//! it reads here. A script or the command line reads where the command runs.

use std::any::Any;

use serde_json::Value;

use crate::policy::{Authorized, CommandOp, Continuation};
use crate::{CmdResult, Session};

/// A prepared value accessible only while its original admitted command runs.
pub(crate) struct Ready {
    key: String,
    value: Box<dyn Any + Send>,
}

/// What a read gave, together with the command and authority that alone may consume it.
pub struct Read {
    pub(crate) op: Authorized<CommandOp>,
    pub(crate) owner: Continuation,
    pub(crate) source: Option<rustgis_map::source::SourceLayer>,
    pub(crate) ready: Option<Ready>,
    pub(crate) inputs: Vec<rustgis_data::provenance::Fingerprint>,
    /// History set aside to make room for an edit's copy, and the copy counted as held until it
    /// lands (#2431).
    pub(crate) room: Option<crate::RoomMade>,
    pub(crate) reservation: Option<crate::undo_memory::Reservation>,
}

/// A reader chosen only after the command and its original input have been admitted.
pub(crate) struct ReadWork {
    name: &'static str,
    key: String,
    work: Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>,
}

#[cfg(not(target_arch = "wasm32"))]
impl ReadWork {
    /// Lets an alternate file pick consume the same prepared value without choosing its
    /// reader again when the result lands (a DBF pick routed to its companion Shapefile).
    pub(crate) fn with_key(mut self, key: String) -> Self {
        self.key = key;
        self
    }
}

/// A read started for a command, to run on a thread of its own.
pub struct ReadAhead {
    /// What the read is called, for its progress and a cancelled run's error.
    pub name: &'static str,
    op: Authorized<CommandOp>,
    owner: Continuation,
    source: Option<rustgis_map::source::SourceLayer>,
    work: Option<ReadWork>,
    capture_inputs: bool,
    /// A table write retains its admitted project and table identity across queue waits.
    table: Option<(u64, u32, u64)>,
    preview: Option<rustgis_data::Dataset>,
    /// Set when the edit starts ([`Self::refresh_table_input`]), carried to its landing.
    room: Option<crate::RoomMade>,
    reservation: Option<crate::undo_memory::Reservation>,
    /// The rows a layer edit's worker copies, as they were when it was prepared.
    rows: Option<std::sync::Weak<Vec<rustgis_data::Feature>>>,
}

impl ReadAhead {
    /// The immutable registered invocation retained by this read, for queue presentation only.
    pub fn command(&self) -> Option<(&'static str, &Value)> {
        match self.op.op() {
            CommandOp::Registered { spec, params } => Some((spec.id, params)),
            _ => None,
        }
    }
    /// Whether this command has a read to make before it lands; otherwise landing runs it inline.
    pub fn reads(&self) -> bool {
        self.work.is_some()
    }

    /// Keeps the rows approved in a UI preview across a queue wait. Ordinary table writes
    /// still refresh their input when they start.
    pub fn keep_preview(&mut self, dataset: rustgis_data::Dataset) {
        self.preview = Some(dataset);
    }

    /// Refreshes a table write's input immediately before its worker starts, so queued writes
    /// see earlier writes' output. The admitted operation, owner and table contents identity stay fixed;
    /// landing still rejects changes made while the worker runs. Other reads keep their input.
    ///
    /// An edit's copy is checked against memory here, with the session's history as it is now and
    /// the copies of edits still running counted (#2431): refused when it can't fit, or with
    /// older history set aside for it, freed on the worker just before it copies.
    pub fn refresh_table_input(self, s: &mut Session) -> Result<Self, String> {
        self.owner.check()?;
        let mut ahead = self.refresh_input(s)?;
        ahead.make_room(s)?;
        Ok(ahead)
    }

    fn make_room(&mut self, s: &mut Session) -> Result<(), String> {
        let CommandOp::Registered { spec, params } = self.op.op() else { return Ok(()) };
        s.reclaim_rooms();
        let table = s.named_table(spec, params);
        let layer = s.layer_id(params).ok().or(s.active_layer);
        let pending = s.room_pending();
        let Some(room) = s.room_for_edit(spec.id, spec.mutates, spec.label, layer, table)? else { return Ok(()) };
        if pending && room.count > 0 {
            // One edit's history out at a time, so each goes back in its place.
            s.put_back(&room, true, true);
            return Err(format!(
                "{}: not enough memory to start it while another edit is running; run it again once that one has finished",
                spec.label
            ));
        }
        self.reservation = Some(s.undo_memory.reserve(room.copy));
        s.remember_room(&room);
        self.room = Some(room);
        Ok(())
    }

    fn refresh_input(mut self, s: &Session) -> Result<Self, String> {
        let CommandOp::Registered { spec, params } = self.op.op() else { return Err("not a registered read command".into()) };
        if let Some(preview) = &self.preview {
            let dataset = if let Some(table) = s.named_table(spec, params) {
                &s.project.table(table).ok_or("no table")?.dataset
            } else {
                &s.map().layer(s.layer_id(params)?).ok_or("no layer")?.dataset
            };
            if dataset.fields != preview.fields || !dataset.features.shares(&preview.features) {
                return Err("the data changed after its date preview: choose the format again".into());
            }
        }
        if let Some(rows) = &self.rows {
            let now = s.layer_id(params).ok().and_then(|id| s.map().layer(id)).map(|l| l.dataset.features.allocation_key());
            if !now.is_some_and(|now| now.ptr_eq(rows)) {
                return Err("the layer changed while the edit was waiting: run the edit again".into());
            }
        }
        let Some((opened, table, instance)) = self.table else { return Ok(self) };
        if s.opened != opened
            || s.named_table(spec, params) != Some(table)
            || s.project.table(table).is_none_or(|t| t.instance_id() != instance)
        {
            return Err("the table changed while the edit was waiting: run the edit again".into());
        }
        if self.preview.is_some() {
            return Ok(self);
        }
        self.work = super::edit::table_ahead(s, spec.id, params)?;
        Ok(self)
    }

    /// [`Self::run`], called on the read's own thread (which the front end starts; never the
    /// window's): a file whose coordinate system needs a datum grid RustGIS doesn't have first
    /// waits for it, when its user downloads them ([`crate::grids`]), so its features land where
    /// the grid puts them.
    pub fn run_waiting_for_grids(self, control: &std::sync::Arc<rustgis_analysis::control::Control>) -> Result<Read, String> {
        rustgis_data::raster::crs::waiting_for_grids(|| self.run(control))
    }

    /// Reads, blocking until it's done; `control` stops it and counts its progress (see
    /// [`rustgis_analysis::control`]). A read that fails gives its failure to the command, which
    /// says it as it would have; only a cancelled run (or a bug in a reader) is an error here.
    pub fn run(self, control: &std::sync::Arc<rustgis_analysis::control::Control>) -> Result<Read, String> {
        let inputs = self.capture_inputs.then(rustgis_data::provenance::Capture::start);
        let ReadAhead { name, op, owner, source, work, room, reservation, .. } = self;
        owner.check()?;
        let current = owner.clone();
        let still = std::sync::Arc::new(move || current.check().is_ok());
        let mut ready = None;
        // A bug in a reader must not take the app down.
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::render::rustgis_tiles::cache::with_still(still, || {
                rustgis_analysis::run_under(name, control, || {
                    ready = work.map(|ReadWork { key, work, .. }| {
                        // History set aside for this edit is freed just before it copies.
                        let value = match &room {
                            Some(room) => rustgis_data::before_rows_copy(room.free_on_copy(), work).0,
                            None => work(),
                        };
                        Ready { key, value }
                    });
                    Ok(())
                })
            })
        }))
        .unwrap_or_else(|_| Err(format!("{name} failed unexpectedly")));
        res?;
        Ok(Read {
            op,
            owner,
            source,
            ready,
            inputs: inputs.as_ref().map_or_else(Vec::new, rustgis_data::provenance::Capture::inputs),
            room,
            reservation,
        })
    }
}

/// The read `command` makes with `p`, when the app makes it on a thread of its own before the
/// command runs. Admission precedes all preparation; even a command without an off-thread read
/// keeps its original operation for synchronous landing.
pub fn read_ahead(s: &mut Session, command: &str, p: &Value) -> Result<ReadAhead, String> {
    s.start_read(command, p)
}

/// The commands whose read the desktop app makes on a thread of its own before they land
/// ([`read_ahead`]), each with what its read is called, besides the table edits
/// (`edit::TABLE_EDITS`). The cancel tests (`tests/cancellation.rs`) exercise every one, or name
/// why not.
pub(crate) const IN_BACKGROUND: &[(&str, &str)] = &[
    ("service.layers", "List Service Layers"),
    ("service.add", "Add Service Layer"),
    ("layer.addData", "Add Data"),
    ("layer.loadAll", "Load All Features"),
    ("labels.convert", "Convert To Annotation"),
    ("layer.repairSource", "Repair Data Source"),
    ("layer.reloadData", "Reload Data"),
    ("model.run", "Run Model"),
    ("script.run", "Run Script"),
    ("map.reloadTiles", "Reload Data"),
    ("map.repairTiles", "Repair Data Source"),
    ("topology.validate", "Error Inspector"),
    ("source.syncState", "Source File State"),
    ("raster.imageGrid", "Read Original Image Grid"),
    ("edit.previewShapeConversion", "Preview Shape Conversion"),
    ("table.statistics", "Column Statistics"),
    ("data.similar", "Find Similar"),
    ("layer.sizeBy", "Size by Field"),
    ("offline.keepRows", "Keep Field Package Rows"),
    ("style.importQgis", "Import QGIS Style"),
];

/// What `command`'s read is called, when it is one the app makes before the command lands
/// ([`IN_BACKGROUND`] or a table edit).
pub(crate) fn in_background(command: &str) -> Option<&'static str> {
    IN_BACKGROUND
        .iter()
        .find(|(c, _)| *c == command)
        .map(|(_, name)| *name)
        .or_else(|| super::edit::table_edit_in_background(command).map(|_| "Edit Table"))
}

/// Why `command` can't be read ahead.
pub(crate) fn not_in_background(command: &str) -> String {
    format!("{command} doesn't prepare background work")
}

/// Picks work only for an already admitted command, retaining its exact streamed-source opening.
pub(crate) fn prepare(s: &mut Session, op: Authorized<CommandOp>, owner: Continuation) -> Result<ReadAhead, String> {
    let CommandOp::Registered { spec, params } = op.op() else { return Err("not a registered read command".into()) };
    let name = in_background(spec.id).ok_or_else(|| not_in_background(spec.id))?;
    let (work, source) = match spec.id {
        "service.layers" => {
            #[cfg(not(target_arch = "wasm32"))]
            let work = super::service::listing_ahead(s, params)?;
            #[cfg(target_arch = "wasm32")]
            let work = None;
            (work, None)
        }
        "service.add" => {
            #[cfg(not(target_arch = "wasm32"))]
            let work = super::service::adding_ahead(s, params)?;
            #[cfg(target_arch = "wasm32")]
            let work = None;
            (work, None)
        }
        "layer.addData" => (super::layer::add_data_ahead(params), None),
        "layer.loadAll" => {
            let layer = s.map().layer(s.layer_id(params)?).ok_or("no such layer")?;
            (super::layer::load_all_ahead(s, params), layer.source.clone())
        }
        "labels.convert" => {
            let layer = s.map().layer(s.layer_id(params)?).ok_or("no such layer")?;
            // Nothing to convert: the command says so at once, with no read.
            let work = super::labels::has_label_text(layer, s.map().scale()).then(|| super::whole::ahead(s, params, name)).flatten();
            (work, layer.source.clone())
        }
        "layer.repairSource" => {
            #[cfg(not(target_arch = "wasm32"))]
            let work = super::repair::repair_ahead(s, params);
            #[cfg(target_arch = "wasm32")]
            let work = None;
            (work, None)
        }
        "layer.reloadData" => {
            #[cfg(not(target_arch = "wasm32"))]
            let work = super::repair::reload_ahead(s, params);
            #[cfg(target_arch = "wasm32")]
            let work = None;
            (work, None)
        }
        "model.run" => {
            // A copied session has native authority. Scoped callers keep their admitted
            // command and execute on the original session instead of widening into that copy.
            #[cfg(not(target_arch = "wasm32"))]
            let work = owner.native_tool_owner().then(|| super::workflows::ahead_on_copy(s, "model.run", params));
            #[cfg(target_arch = "wasm32")]
            let work = None;
            (work, None)
        }
        "script.run" => {
            #[cfg(not(target_arch = "wasm32"))]
            let work = super::workflows::script_ahead(s, params, owner.native_tool_owner())?;
            #[cfg(target_arch = "wasm32")]
            let work = None;
            (work, None)
        }
        "map.reloadTiles" | "map.repairTiles" => {
            #[cfg(not(target_arch = "wasm32"))]
            let work = Some(super::repair::archive_ahead(s, spec.id, params));
            #[cfg(target_arch = "wasm32")]
            let work = None;
            (work, None)
        }
        id if super::edit::table_edit_in_background(id).is_some() => (super::edit::table_ahead(s, spec.id, params)?, None),
        "topology.validate" => (super::design::topology_ahead(s, params), None),
        "source.syncState" => (super::publish::sync_ahead(s, params), None),
        "raster.imageGrid" => (crate::image_grid::ahead(s, params), None),
        "edit.previewShapeConversion" => (crate::shape_conversion::ahead(s, params), None),
        "table.statistics" => (super::table_stats::stats_ahead(s, params), None),
        "data.similar" => (super::similar::ahead(s, params), None),
        "layer.sizeBy" => (super::layer::size_by_ahead(s, params), None),
        "offline.keepRows" => (super::field_changes::keep_ahead(s), None),
        "style.importQgis" => (super::qgis::style_ahead(s, params), None),
        // Listed in IN_BACKGROUND without a read here.
        id => return Err(not_in_background(id)),
    };
    let name = work.as_ref().map_or(name, |work| work.name);
    let table = super::edit::table_edit_in_background(spec.id)
        .and_then(|_| s.named_table(spec, params))
        .and_then(|id| s.project.table(id).map(|t| (s.opened, id, t.instance_id())));
    // A layer edit's worker copies the rows taken here: they must still be the layer's when it
    // starts, so the memory checked then is what it copies (#2431).
    let rows = (work.is_some() && table.is_none() && spec.mutates && crate::edits_features(spec.id))
        .then(|| s.layer_id(params).ok().and_then(|id| s.map().layer(id)).map(|l| l.dataset.features.allocation_key()))
        .flatten();
    Ok(ReadAhead { name, op, owner, source, work, table, preview: None, capture_inputs: s.log.on(), room: None, reservation: None, rows })
}

/// Consumes a completed read under its initiating owner, with ordinary command history and undo.
pub fn read_land(s: &mut Session, read: Read) -> CmdResult {
    s.finish_read(read)
}

/// Captures the archive selected now, so a queued UI read cannot retarget an index or name.
#[cfg(not(target_arch = "wasm32"))]
pub fn archive_target(s: &Session, p: &Value) -> Option<(usize, rustgis_map::TileLayer)> {
    let i = super::tiles::find(s, p).ok()??;
    let tile = s.map().tiles.get(i)?;
    tile.archive_file()?;
    Some((i, tile.clone()))
}

/// `work` as a read for the command to take under `key`.
pub(crate) fn ahead<T: Send + 'static>(name: &'static str, key: String, work: impl FnOnce() -> T + Send + 'static) -> ReadWork {
    ReadWork { name, key, work: Box::new(move || Box::new(work()) as Box<dyn Any + Send>) }
}

#[cfg(not(target_arch = "wasm32"))]
/// Makes one script line's prepared result available only to its ordinary admitted invocation.
/// Restore the enclosing read on every path, including a panic raised again for its caller.
pub(crate) fn taking<T: Send + 'static>(s: &mut Session, key: &str, value: T, run: impl FnOnce(&mut Session) -> CmdResult) -> CmdResult {
    let previous = s.read_ahead.replace(Ready { key: key.into(), value: Box::new(value) });
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(s)));
    s.read_ahead = previous;
    result.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// The read placed for `key`, taken: `None` when none was, so the caller reads here.
pub(crate) fn take<T: 'static>(s: &mut Session, key: &str) -> Option<T> {
    if s.read_ahead.as_ref().is_none_or(|r| r.key != key) {
        return None;
    }
    let read = s.read_ahead.take()?;
    read.value.downcast::<T>().ok().map(|v| *v)
}

/// Borrows prepared metadata during its admitted command, without consuming the value.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn peek<'a, T: 'static>(s: &'a Session, key: &str) -> Option<&'a T> {
    s.read_ahead.as_ref().filter(|r| r.key == key)?.value.downcast_ref()
}

/// What `read` prepared, when it is a `T`, for a test to look at before it lands.
#[cfg(test)]
pub(crate) fn prepared<T: 'static>(read: &Read) -> Option<&T> {
    read.ready.as_ref()?.value.downcast_ref::<T>()
}

/// Whether a read is placed for `key`, without taking it.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn placed_for(s: &Session, key: &str) -> bool {
    s.read_ahead.as_ref().is_some_and(|r| r.key == key)
}
