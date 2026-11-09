//! Reads the desktop app makes on a thread of its own before a command adds what they read
//! (#1291 step 3): Add Data's whole-file reads (a raster, a raster GDAL converts, a GeoJSON,
//! delimited text or spreadsheet read whole), Reload Data, Load All Features and native models or scripts. [`read_ahead`]
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
}

/// A reader chosen only after the command and its original input have been admitted.
pub(crate) struct ReadWork {
    name: &'static str,
    key: String,
    work: Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>,
}

/// A read started for a command, to run on a thread of its own.
pub struct ReadAhead {
    /// What the read is called, for its progress and a cancelled run's error.
    pub name: &'static str,
    op: Authorized<CommandOp>,
    owner: Continuation,
    source: Option<rustgis_map::source::SourceLayer>,
    work: Option<ReadWork>,
}

impl ReadAhead {
    /// Whether this command has a read to make before it lands; otherwise landing runs it inline.
    pub fn reads(&self) -> bool {
        self.work.is_some()
    }

    /// Reads, blocking until it's done; `control` stops it and counts its progress (see
    /// [`rustgis_analysis::control`]). A read that fails gives its failure to the command, which
    /// says it as it would have; only a cancelled run (or a bug in a reader) is an error here.
    pub fn run(self, control: &std::sync::Arc<rustgis_analysis::control::Control>) -> Result<Read, String> {
        let ReadAhead { name, op, owner, source, work } = self;
        owner.check()?;
        let current = owner.clone();
        let still = std::sync::Arc::new(move || current.check().is_ok());
        let mut ready = None;
        // A bug in a reader must not take the app down.
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::render::rustgis_tiles::cache::with_still(still, || {
                rustgis_analysis::run_under(name, control, || {
                    ready = work.map(|ReadWork { key, work, .. }| Ready { key, value: work() });
                    Ok(())
                })
            })
        }))
        .unwrap_or_else(|_| Err(format!("{name} failed unexpectedly")));
        res?;
        Ok(Read { op, owner, source, ready })
    }
}

/// The read `command` makes with `p`, when the app makes it on a thread of its own before the
/// command runs. Admission precedes all preparation; even a command without an off-thread read
/// keeps its original operation for synchronous landing.
pub fn read_ahead(s: &mut Session, command: &str, p: &Value) -> Result<ReadAhead, String> {
    s.start_read(command, p)
}

/// Picks work only for an already admitted command, retaining its exact streamed-source opening.
pub(crate) fn prepare(s: &Session, op: Authorized<CommandOp>, owner: Continuation) -> Result<ReadAhead, String> {
    let CommandOp::Registered { spec, params } = op.op() else { return Err("not a registered read command".into()) };
    let (name, work, source) = match spec.id {
        "layer.addData" => ("Add Data", super::layer::add_data_ahead(params), None),
        "layer.loadAll" => {
            let layer = s.map().layer(s.layer_id(params)?).ok_or("no such layer")?;
            ("Load All Features", super::layer::load_all_ahead(s, params), layer.source.clone())
        }
        "layer.reloadData" => {
            #[cfg(not(target_arch = "wasm32"))]
            let work = super::repair::reload_ahead(s, params);
            #[cfg(target_arch = "wasm32")]
            let work = None;
            ("Reload Data", work, None)
        }
        "model.run" => {
            // A copied session has native authority. Scoped callers keep their admitted
            // command and execute on the original session instead of widening into that copy.
            #[cfg(not(target_arch = "wasm32"))]
            let work = owner.native_tool_owner().then(|| super::workflows::ahead_on_copy(s, "model.run", params));
            #[cfg(target_arch = "wasm32")]
            let work = None;
            ("Run Model", work, None)
        }
        "script.run" => {
            #[cfg(not(target_arch = "wasm32"))]
            let work = if owner.native_tool_owner() { super::workflows::script_ahead(s, params) } else { None };
            #[cfg(target_arch = "wasm32")]
            let work = None;
            ("Run Script", work, None)
        }
        _ => return Err("only Add Data, Reload Data, Load All, models and scripts can prepare background work".into()),
    };
    let name = work.as_ref().map_or(name, |work| work.name);
    Ok(ReadAhead { name, op, owner, source, work })
}

/// Consumes a completed read under its initiating owner, with ordinary command history and undo.
pub fn read_land(s: &mut Session, read: Read) -> CmdResult {
    s.finish_read(read)
}

/// `work` as a read for the command to take under `key`.
pub(crate) fn ahead<T: Send + 'static>(name: &'static str, key: String, work: impl FnOnce() -> T + Send + 'static) -> ReadWork {
    ReadWork { name, key, work: Box::new(move || Box::new(work()) as Box<dyn Any + Send>) }
}

/// The read placed for `key`, taken: `None` when none was, so the caller reads here.
pub(crate) fn take<T: 'static>(s: &mut Session, key: &str) -> Option<T> {
    if s.read_ahead.as_ref().is_none_or(|r| r.key != key) {
        return None;
    }
    let read = s.read_ahead.take()?;
    read.value.downcast::<T>().ok().map(|v| *v)
}

/// Whether a read is placed for `key`, without taking it.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn placed_for(s: &Session, key: &str) -> bool {
    s.read_ahead.as_ref().is_some_and(|r| r.key == key)
}
