//! Python bindings to the RustGIS engine: the native half of the `rustgis` package on PyPI.
//!
//! The surface is deliberately small. Everything RustGIS does is a command with an id and JSON
//! parameters, so the module exposes `run(id, params_json) -> result_json` and leaves the Pythonic
//! API (keyword arguments, `geopandas`, Jupyter display) to `python/rustgis/__init__.py`. The few
//! things that aren't commands are here too: the map as PNG bytes, a layer as GeoJSON, and the
//! notices commands leave behind.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use rustgis_engine::{Project, Session};
use serde_json::Value;
use std::io::Write;
use std::sync::{Arc, Mutex};

#[cfg(feature = "test-stores")]
mod test_support;

create_exception!(_rustgis, RustGISError, PyException, "A RustGIS command or file operation failed.");

/// A layer as Python names it: by id (a `Layer` keeps its id), or by name.
#[derive(FromPyObject)]
enum LayerRef {
    Id(u32),
    Name(String),
}

/// Engine commands also change process-global connection activations. Before releasing the GIL,
/// Python implicitly serialized these calls across sessions; retain that ordering without keeping
/// unrelated Python code waiting. Always acquire this before a session lock, and release both
/// before calling Python (including diagnostic logging).
static NATIVE_OPERATIONS: Mutex<()> = Mutex::new(());

fn with_native<T: Send>(py: Python<'_>, operation: impl FnOnce() -> T + Send) -> T {
    py.detach(move || {
        let _native = NATIVE_OPERATIONS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        operation()
    })
}

/// The store `use_saved_secrets` reads: the system keychain. A build for the Python tests
/// (`--features test-stores`) reads the store the tests installed instead, and never the
/// user's keychain.
#[cfg(not(feature = "test-stores"))]
fn saved_store() -> impl rustgis_engine::data::secrets::SecretStore + 'static {
    rustgis_keychain::Keychain::default()
}

#[cfg(feature = "test-stores")]
fn saved_store() -> impl rustgis_engine::data::secrets::SecretStore + 'static {
    test_support::installed()
}

/// Tells the Python user something went on without a saved secret, through the `rustgis` logger
/// at WARNING. Never fails and never raises: the caller already has an operation result that a
/// diagnostic must not replace, so a logging hook that raises (or no usable `logging`) falls back
/// to stderr. Logging, unlike `warnings`, doesn't drop a repeated message from the same place, so
/// every session that has something to say says it.
fn report(py: Python<'_>, messages: Vec<String>) {
    for message in messages {
        let logged = py
            .import("logging")
            .and_then(|logging| logging.call_method1("getLogger", ("rustgis",)))
            .and_then(|logger| logger.call_method1("warning", ("%s", message.as_str())));
        if logged.is_err() {
            let _ = writeln!(std::io::stderr(), "rustgis: warning: {message}");
        }
    }
}

/// One RustGIS session: a project with its maps, layers, undo history and selection.
///
/// Usable from any thread (Jupyter delivers a widget's messages on one of its own). Native calls
/// take turns across all sessions because connection activations are process-global. The work
/// and lock waits release the GIL, so unrelated Python threads keep running during a long call.
#[pyclass(frozen, module = "rustgis._rustgis")]
struct Engine {
    /// Only ever locked with the GIL released: a thread that waited for the lock while holding the
    /// GIL would deadlock with the one that has it and needs the GIL to finish.
    session: Mutex<Session>,
    /// What the session's store reported (the first failure of the keychain, a preferences file
    /// that couldn't be read), waiting to be logged by the call that is running or the next one.
    diagnostics: Arc<Mutex<Vec<String>>>,
}

impl Engine {
    fn with_session<T: Send>(&self, py: Python<'_>, operation: impl FnOnce(&mut Session) -> T + Send) -> T {
        with_native(py, || {
            let mut session = self.session.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            operation(&mut session)
        })
    }

    /// Logs what the session's store reported since the last call. Done after the operation, with
    /// its result in hand, by every method that can read the store.
    fn flush(&self, py: Python<'_>) {
        let messages = std::mem::take(&mut *self.diagnostics.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        if !messages.is_empty() {
            report(py, messages);
        }
    }
}

#[pymethods]
impl Engine {
    #[new]
    #[pyo3(signature = (use_saved_secrets=false))]
    fn new(py: Python<'_>, use_saved_secrets: bool) -> Self {
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let sink = diagnostics.clone();
        // Opting in reads the preferences file, so it runs without the GIL; it reads no secret.
        let session = with_native(py, move || {
            let mut session = Session::new(Project::default());
            if use_saved_secrets {
                let _ = rustgis_engine::prefs::opt_in(&mut session, saved_store(), move |message| {
                    sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(message);
                });
            }
            session
        });
        let engine = Engine { session: Mutex::new(session), diagnostics };
        engine.flush(py);
        engine
    }

    /// Runs command `id` with `params` (JSON text) and returns its result as JSON text.
    fn run(&self, py: Python<'_>, id: &str, params: &str) -> PyResult<String> {
        let params: Value = if params.trim().is_empty() {
            Value::Null
        } else {
            serde_json::from_str(params).map_err(|e| PyValueError::new_err(format!("parameters aren't JSON: {e}")))?
        };
        let result =
            self.with_session(py, |session| session.run(id, &params).map(|value| value.to_string()).map_err(|e| format!("{id}: {e}")));
        self.flush(py);
        result.map_err(RustGISError::new_err)
    }

    /// The map as a PNG: `width` and `height` in pixels (the view's own size when left out) at
    /// `dpi` (96 when left out).
    #[pyo3(signature = (width=None, height=None, dpi=None))]
    fn render_png<'py>(&self, py: Python<'py>, width: Option<f64>, height: Option<f64>, dpi: Option<f64>) -> PyResult<Bound<'py, PyBytes>> {
        let result = self.with_session(py, |session| {
            session.follow_style_view();
            rustgis_engine::cmd::share::map_png(session, width, height, dpi)
        });
        self.flush(py);
        result.map(|png| PyBytes::new(py, &png)).map_err(RustGISError::new_err)
    }

    /// Layer `layer` of the active map as GeoJSON text: an id, or a name (a name that is a number
    /// is read as an id after the names). Its features as they are in memory (a large file read
    /// in place holds the features of the last view; run `layer.loadAll` first for all of them),
    /// or only its selected ones when `selected`.
    #[pyo3(signature = (layer, selected=false))]
    fn layer_geojson(&self, py: Python<'_>, layer: LayerRef, selected: bool) -> PyResult<String> {
        let result = self.with_session(py, |session| {
            let map = session.map();
            let found = match &layer {
                LayerRef::Id(id) => map.layer(*id).ok_or_else(|| format!("no layer with id {id}")),
                LayerRef::Name(name) => map
                    .layer_by_name(name)
                    .or_else(|| name.trim().parse::<u32>().ok().and_then(|id| map.layer(id)))
                    .ok_or_else(|| format!("no layer named '{name}'")),
            }?;
            if !selected {
                return Ok(rustgis_engine::data::io::to_geojson(&found.dataset).to_string());
            }
            let mut ds = rustgis_engine::data::Dataset::new(found.name.clone(), found.geometry_type(), found.dataset.fields.clone());
            for f in found.selection.iter().filter_map(|r| found.dataset.features.get(*r)) {
                ds.push(f.geometry.clone(), f.attrs.clone());
            }
            Ok::<String, String>(rustgis_engine::data::io::to_geojson(&ds).to_string())
        });
        self.flush(py);
        result.map_err(RustGISError::new_err)
    }

    /// The project as `.rgis` JSON text, as `project.saveAs` writes it (what `project.openText`
    /// opens).
    fn project_json(&self, py: Python<'_>) -> PyResult<String> {
        let result = self.with_session(py, |session| serde_json::to_string(&session.project).map_err(|e| e.to_string()));
        self.flush(py);
        result.map_err(RustGISError::new_err)
    }

    /// The notices commands have given since the last call (a file's warnings, a tool's
    /// summary): `(text, is_error)` pairs, oldest first.
    fn take_notices(&self, py: Python<'_>) -> Vec<(String, bool)> {
        let notices: Vec<(String, bool)> = self.with_session(py, |session| session.notices.drain(..).map(|n| (n.text, n.error)).collect());
        self.flush(py);
        notices
    }
}

/// Loads the plugins installed for the desktop app, so their commands and tools run here too.
/// Returns the problems found, one message per plugin that couldn't load.
#[pyfunction]
fn load_plugins(py: Python<'_>) -> Vec<String> {
    // Reads and compiles the installed plugins' files: no reason to hold the GIL for it.
    with_native(py, rustgis_engine::cmd::plugins::load_installed)
}

#[pymodule]
fn _rustgis(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    m.add("RustGISError", m.py().get_type::<RustGISError>())?;
    m.add_class::<Engine>()?;
    m.add_function(wrap_pyfunction!(load_plugins, m)?)?;
    #[cfg(feature = "test-stores")]
    test_support::register(m)?;
    Ok(())
}
