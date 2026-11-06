//! Reads the desktop app makes on a thread of its own before a command adds what they read
//! (#1291 step 3): Add Data's whole-file reads (a raster, a raster GDAL converts, a GeoJSON,
//! delimited text or spreadsheet read whole), Load All Features, and a model's or a script's
//! tools, run on a copy of the session. [`read_ahead`] takes what the read needs of the session,
//! [`ReadAhead::run`] reads on the thread, and the app then runs the command as it would have
//! with the read [`Read::placed`] in the session: the command takes it instead of reading again,
//! so what it adds, says, logs and puts in Undo is the same as when it reads here. A script's own
//! Add Data lines, and the command line, read where the command runs.

use std::any::Any;

use serde_json::Value;

use crate::Session;

/// What a read gave, for the command it was made for: kept in the session ([`Read::placed`])
/// while that command runs, and taken by the site that would have read it.
pub struct Read {
    key: String,
    value: Box<dyn Any + Send>,
}

impl Read {
    /// Puts the read in the session for the next command to take, until [`Session::drop_read`].
    pub fn placed(self, s: &mut Session) {
        s.read_ahead = Some(self);
    }
}

/// A read started for a command, to run on a thread of its own.
pub struct ReadAhead {
    /// What the read is called, for its progress and a cancelled run's error.
    pub name: &'static str,
    key: String,
    work: Box<dyn FnOnce() -> Box<dyn Any + Send> + Send>,
}

impl ReadAhead {
    /// Reads, blocking until it's done; `control` stops it and counts its progress (see
    /// [`rustgis_analysis::control`]). A read that fails gives its failure to the command, which
    /// says it as it would have; only a cancelled run (or a bug in a reader) is an error here.
    pub fn run(self, control: &std::sync::Arc<rustgis_analysis::control::Control>) -> Result<Read, String> {
        let ReadAhead { name, key, work } = self;
        let mut value = None;
        // A bug in a reader must not take the app down.
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rustgis_analysis::run_under(name, control, || {
                value = Some(work());
                Ok(())
            })
        }))
        .unwrap_or_else(|_| Err(format!("{name} failed unexpectedly")));
        res?;
        let value = value.ok_or_else(|| format!("{name} read nothing"))?;
        Ok(Read { key, value })
    }
}

/// The read `command` makes with `p`, when the app makes it on a thread of its own before the
/// command runs: `None` for one it runs as it is (a layer read in place, a web service, a file
/// the command reads differently).
pub fn read_ahead(s: &Session, command: &str, p: &Value) -> Option<ReadAhead> {
    match command {
        "layer.addData" => super::layer::add_data_ahead(p),
        "layer.loadAll" => super::layer::load_all_ahead(s, p),
        #[cfg(not(target_arch = "wasm32"))]
        "model.run" => Some(super::workflows::ahead_on_copy(s, "model.run", p)),
        #[cfg(not(target_arch = "wasm32"))]
        "script.run" => super::workflows::script_ahead(s, p),
        _ => None,
    }
}

/// `work` as a read for the command to take under `key`.
pub(crate) fn ahead<T: Send + 'static>(name: &'static str, key: String, work: impl FnOnce() -> T + Send + 'static) -> ReadAhead {
    ReadAhead { name, key, work: Box::new(move || Box::new(work()) as Box<dyn Any + Send>) }
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
