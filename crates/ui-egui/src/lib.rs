//! L6: the RustGIS desktop front end, built around a ribbon (Home, Data, Style, Labeling, Edit,
//! Analyze, View, Publish), the Layers and Browser panes, a Tools pane, the map view, the
//! attribute table and the status bar.
//!
//! The UI is thin: it reads [`Session`] state and runs commands by id ([`RustGisApp::run`]).
//! UI-only commands (`ui.*`: panes, dialogs, theme) are handled here.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[macro_use]
pub mod i18n;
#[cfg(not(target_arch = "wasm32"))]
mod agentthreads;
mod analysispane;
mod approvalcard;
mod arcgisui;
#[cfg(not(target_arch = "wasm32"))]
mod assistantkeys;
mod assistantpane;
mod autosave;
mod backstage;
mod bookmarkpane;
mod catalogsui;
mod chrome;
mod comets;
mod connectiontree;
mod connectionui;
#[cfg(not(target_arch = "wasm32"))]
mod connectui;
mod coordreadout;
mod cswui;
mod cubesui;
mod diagnosticlog;
mod dialogs;
mod docpanes;
mod earthaccessui;
mod editpanes;
mod embeddingsui;
#[cfg(not(target_arch = "wasm32"))]
mod exportjob;
mod failureui;
mod fieldlists;
mod fieldmenu;
mod fill_editor;
mod geocodeui;
mod geolibreui;
mod geometrycalc;
mod globe_drape;
mod graphicformat;
pub mod icons;
mod keymap;
mod labelfonts;
mod labelpane;
mod layerchooser;
mod layoutview;
#[cfg(not(target_arch = "wasm32"))]
mod localagents;
mod mapgpu;
mod mapmenu;
mod mapview;
mod measure_units;
mod modelbuilder;
mod navui;
mod nldiui;
mod opendataui;
mod panes;
mod pluginpanel;
mod pluginstart;
mod pluginsui;
mod portalsui;
#[cfg(not(target_arch = "wasm32"))]
mod readjob;

/// How a command asked for ended ([`readjob::run_then`] on the desktop; the browser runs it where
/// it's asked): what it answered when it ran, or why it never ran (cancelled, or left), which is
/// said already.
pub enum Landed {
    Ran(Result<Value, String>),
    NotRun(String),
}
#[cfg(not(target_arch = "wasm32"))]
mod relayshot;
#[cfg(not(target_arch = "wasm32"))]
mod reloaddata;
mod repairsource;
mod reportpane;
mod ribbon;
mod s3ui;
mod saveprompt;
mod scriptcolor;
mod scriptcomplete;
mod searchbox;
mod serviceui;
mod sessionlog;
mod shortcutfile;
mod shortcutui;
mod showme;
mod sourcespane;
mod sqlbox;
mod sqlpane;
mod stacui;
mod table;
mod textform;
pub mod theme;
mod threadsui;
mod tilelayer;
mod tilelegend;
mod tilt_drape;
mod tilt_settle;
mod timebar;
mod update;
mod vertexedit;
mod webdataui;
mod widgets;
mod workspacepanes;
mod zarrui;

use rustgis_engine::cmd::service_added::Removal;
use rustgis_engine::render::rustgis_tiles::failure::Failure;
use rustgis_engine::{Project, Session};
use serde_json::{Value, json};

#[cfg(not(target_arch = "wasm32"))]
pub use autosave::FolderStore;
pub use autosave::{Recovery, RecoveryStore};
pub use chrome::{DoubleClick, TitlebarMetrics};
pub use coordreadout::CoordReadout;
pub use rustgis_assistant;
pub use rustgis_engine;
pub use widgets::big_split_menu;

/// Reconciles only command-result wrappers, not parameters or other arbitrary JSON.
/// Script and model results retain their structure while file results become completed.
fn complete_gp_file(result: Result<Value, String>, id: u64, completed: &Result<Value, String>) -> Result<Value, String> {
    fn visit(result: &mut Value, id: u64, completed: &Result<Value, String>) -> Result<(), String> {
        if result.get("download").and_then(Value::as_u64) == Some(id) {
            *result = completed.as_ref().map_err(Clone::clone)?.clone();
            return Ok(());
        }
        if let Some(ran) = result.get_mut("ran").and_then(Value::as_array_mut) {
            for step in ran {
                visit(step, id, completed)?;
            }
        }
        if let Some(result) = result.get_mut("result") {
            visit(result, id, completed)?;
        }
        Ok(())
    }
    let mut result = result?;
    visit(&mut result, id, completed)?;
    Ok(result)
}

/// Files read asynchronously (browser uploads, drops) waiting to be opened: (file name, bytes).
pub type Inbox = std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<u8>)>>>;

/// Starts the name of a file the web shell's `"text"` picker (Add Delimited Text) put in the
/// inbox, so the dialog's options go with that file and no other (a drop, another picker).
pub const DELIMITED_PICK: &str = "\u{1}delimited:";

/// Marks files returned by the source-CRS picker, so its override never applies to a drop or
/// another picker. Several selected Shapefiles share the same import options.
pub const SHAPEFILE_PICK: &str = "\u{1}shapefile:";

/// The `open_async` purpose for a tool's file parameter, followed by the extensions it offers
/// (`toolfile:nc,nc4`; none for any file).
pub const TOOL_FILE_PURPOSE: &str = "toolfile:";

/// Marks a returned shortcut picker file; cancellation leaves no pending routing state.
pub const SHORTCUT_PICK: &str = "\u{1}shortcuts:";

/// Maximum portable shortcut document size: saved overrides plus room for version metadata.
pub const SHORTCUT_FILE_MAX_BYTES: usize = keymap::MAX_PREF_BYTES + 100;

/// Starts the name of a file the web shell's tool file picker put in the inbox: it goes to the
/// Tools pane's file parameter, not onto the map.
pub const TOOL_FILE_PICK: &str = "\u{1}toolfile:";

/// The preference the default raster style (`raster.setDefaultStyle`) is kept under.
const RASTER_DEFAULTS_PREF: &str = "raster_default_style";
/// The preference holding the favourite arrangement of publication layouts (`layout.arrangement`).
const LAYOUT_FAVOURITE_PREF: &str = "layout_favourite_arrangement";

/// A result an asynchronous service fills in when it's done (the app polls it each frame).
pub type Slot<T> = std::sync::Arc<std::sync::Mutex<Option<T>>>;

/// One window is read off the frame at a time, however many have started: each may hold 256 MB
/// and more. A read keeps its turn until the frame has taken what it read, so a frame that stalls
/// holds one window, not all of them. Those that wait for their command on the frame don't wait
/// for this.
#[cfg(not(target_arch = "wasm32"))]
static WINDOW_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// How many windows may be under way off the frame, being read or waiting for their turn: each
/// holds a thread.
#[cfg(not(target_arch = "wasm32"))]
const MAX_WINDOW_READS: usize = 16;

/// See [`UiState::window_reads`].
#[cfg(not(target_arch = "wasm32"))]
type WindowRead =
    (u64, String, Value, Slot<Result<rustgis_engine::cmd::layer::CogWindow, String>>, std::sync::Arc<std::sync::atomic::AtomicBool>);

/// A window's turn to be read off the frame, or `None` once `left` says its project was closed:
/// one waiting for its turn then gives up its place at once, without being read.
#[cfg(not(target_arch = "wasm32"))]
fn window_turn(left: &std::sync::atomic::AtomicBool) -> Option<std::sync::MutexGuard<'static, ()>> {
    loop {
        if left.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        match WINDOW_TURN.try_lock() {
            Ok(turn) => return Some(turn),
            Err(std::sync::TryLockError::Poisoned(e)) => return Some(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
}

/// A shell's overrides of the Settings › Start-up preferences for one launch.
#[derive(Clone, Debug, Default)]
pub struct Start {
    /// Begin with an empty project (`Some(true)`) or the sample (`Some(false)`).
    pub blank: Option<bool>,
    /// A projection code: `EPSG:3010`, `ESRI:102033`, a preset id, `mercator` or `globe`.
    pub projection: Option<String>,
    /// The extent to open on, as `minLon,minLat,maxLon,maxLat`.
    pub extent: Option<String>,
    /// The last run didn't close cleanly (a crash, a kill, a power cut): don't open a Start With
    /// project, which may be what crashed (one that crashes every launch would leave no way into
    /// Settings to change it).
    pub after_crash: bool,
}

/// What RustGIS opens with: Settings › Start-up › Start With (#377).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum StartWith {
    /// The World Atlas sample.
    #[default]
    Sample,
    /// An empty project.
    Blank,
    /// The project last opened or saved on this computer (the desktop app).
    Last,
    /// A project file the user picked (the desktop app).
    File(String),
}

impl StartWith {
    /// As the `start_project` preference stores it.
    fn pref(&self) -> String {
        match self {
            StartWith::Sample => "sample".into(),
            StartWith::Blank => "blank".into(),
            StartWith::Last => "last".into(),
            StartWith::File(p) => format!("file:{p}"),
        }
    }

    /// From the `start_project` preference; anything else is the sample.
    fn from_pref(v: &str) -> StartWith {
        match v {
            "blank" => StartWith::Blank,
            "last" => StartWith::Last,
            _ => v.strip_prefix("file:").map(str::trim).filter(|p| !p.is_empty()).map_or(StartWith::Sample, |p| StartWith::File(p.into())),
        }
    }

    /// As `ui.startup` takes and reports it: `"sample"`, `"blank"`, `"last"` or `{"path": …}`.
    pub fn json(&self) -> Value {
        match self {
            StartWith::File(p) => serde_json::json!({"path": p}),
            other => Value::String(other.pref()),
        }
    }

    /// A choice that opens a file: the desktop app's only.
    fn opens_a_file(&self) -> bool {
        matches!(self, StartWith::Last | StartWith::File(_))
    }

    /// From `ui.startup`'s `project`: `"sample"`, `"blank"`, `"last"` or `{"path": …}`, a project
    /// file that is there, kept by its full path.
    pub fn from_json(v: &Value) -> Result<StartWith, String> {
        match v {
            Value::String(s) if s == "sample" => Ok(StartWith::Sample),
            Value::String(s) if s == "blank" => Ok(StartWith::Blank),
            Value::String(s) if s == "last" => Ok(StartWith::Last),
            Value::Object(_) => {
                let path =
                    v.get("path").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()).ok_or("project's path is required")?;
                if !rustgis_engine::project::is_project_file(path) {
                    return Err(format!("{path} isn't a RustGIS project file"));
                }
                let full = std::path::absolute(path).map_err(|e| format!("{path}: {e}"))?;
                if !full.is_file() {
                    return Err(format!("no project at {}", full.display()));
                }
                Ok(StartWith::File(full.display().to_string()))
            }
            _ => Err("project is \"sample\", \"blank\", \"last\" or {\"path\": a project file}".into()),
        }
    }
}

/// Reads `minLon,minLat,maxLon,maxLat` (commas or spaces) into a valid longitude/latitude extent.
pub fn parse_extent(text: &str) -> Option<[f64; 4]> {
    let n: Vec<f64> = text.split([',', ' ']).filter(|x| !x.is_empty()).map(|x| x.trim().parse().ok()).collect::<Option<_>>()?;
    let e = rustgis_engine::cmd::navigate::lonlat_extent(Some(&serde_json::json!(n)))?;
    Some([e.min_x, e.min_y, e.max_x, e.max_y])
}

/// Platform hooks. The desktop app leaves them empty and uses native file dialogs; the web app
/// supplies an asynchronous file picker (results arrive in `inbox`) and browser downloads.
#[derive(Default)]
pub struct Services {
    /// Opens a file picker for `"data"`, `"project"`, or `"shapefile"`; results land in `inbox`.
    /// Shapefile picks carry [`SHAPEFILE_PICK`] after sidecars are gathered into ZIPs.
    pub open_async: Option<Box<dyn Fn(&str)>>,
    /// Offers bytes as a download named `file_name`; success means submission to the host's
    /// download mechanism. Browsers do not expose confirmation that the file was saved.
    pub download: Option<Box<dyn Fn(&str, Vec<u8>) -> Result<(), String>>>,
    pub inbox: Option<Inbox>,
    /// Reads a saved preference (`language`, …).
    pub load_pref: Option<Box<dyn Fn(&str) -> Option<String>>>,
    /// Saves a preference.
    pub save_pref: Option<Box<dyn Fn(&str, &str)>>,
    /// The system's secret store (see `docs/architecture.md` › Secrets): the desktop's keychain.
    /// `None` in the browser and on the command line, where secrets last for the session only.
    pub secrets: Option<std::sync::Arc<dyn rustgis_engine::data::secrets::SecretStore>>,
    /// Downloads a font by URL (web); the bytes land in `font_inbox`.
    pub fetch_font: Option<Box<dyn Fn(&str)>>,
    pub font_inbox: Option<Inbox>,
    /// The browser's GeoLibre runner: lists the tools' manifests (a JSON array) into the slot.
    pub geolibre_manifests: Option<Box<dyn Fn(Slot<Result<String, String>>)>>,
    /// …and runs one tool in its in-memory `/work` folder, putting the new files and printed
    /// lines in the slot. The desktop app runs tools through the `geolibre` program instead.
    pub geolibre_run: Option<Box<dyn Fn(rustgis_engine::cmd::gp::ExternalRun, Slot<rustgis_engine::cmd::gp::ExternalOutput>)>>,
    /// Downloads a URL into the slot (the browser's fetch; the desktop downloads on a thread).
    pub fetch: Option<Box<dyn Fn(&str, Slot<Result<Vec<u8>, String>>)>>,
    /// Makes an HTTP request (the AI assistant's calls to its model) and puts the response, of any
    /// status, in the slot (the browser's fetch; the desktop sends it on a thread).
    pub http: Option<Box<dyn Fn(rustgis_assistant::HttpRequest, Slot<Result<rustgis_assistant::HttpResponse, rustgis_assistant::Error>>)>>,
    /// Where autosave keeps the recovery copy of unsaved work (the desktop's recovery folder).
    /// `None` turns autosave off: the browser for now, and a second copy of the desktop app
    /// running alongside the first, which mustn't touch its copy.
    pub recovery: Option<Box<dyn RecoveryStore>>,
    /// Reads the system clipboard's text into the slot (the browser's `navigator.clipboard`,
    /// which may ask the user first), for a text field's Paste; `None` where the window sends
    /// it (the desktop) or the browser can't read it (a page not served over HTTPS).
    pub read_clipboard: Option<Box<dyn Fn(Slot<Result<String, String>>)>>,
    /// Signs in to an ArcGIS portal (its address and an OAuth client id, either blank for ArcGIS
    /// Online and RustGIS's own app) in a pop-up window, putting who signed in, or why not, in the
    /// slot (the browser; the desktop runs `arcgis.signIn`).
    pub arcgis_sign_in: Option<Box<dyn Fn(String, String, Slot<Result<String, String>>)>>,
}

/// A new private folder for one print job: unique, created fresh (never an existing path) and,
/// on Unix, readable only by this user. Earlier jobs' folders from this run are removed first.
#[cfg(not(target_arch = "wasm32"))]
fn print_dir() -> Result<std::path::PathBuf, String> {
    let tmp = std::env::temp_dir();
    let prefix = format!("rustgis-print-{}-", std::process::id());
    if let Ok(rd) = std::fs::read_dir(&tmp) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) && e.file_type().is_ok_and(|t| t.is_dir()) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = tmp.join(format!("{prefix}{nanos}"));
    // Only Unix sets a mode on it.
    #[allow(unused_mut)]
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

#[cfg(target_arch = "wasm32")]
fn print_dir() -> Result<std::path::PathBuf, String> {
    Err("printing needs the desktop app".into())
}

/// Opens a file with the system's default application (native only).
#[cfg(not(target_arch = "wasm32"))]
fn open_with_system(path: &std::path::Path) -> Result<(), String> {
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program).arg(path).spawn().map(|_| ()).map_err(|e| format!("could not open {}: {e}", path.display()))
}

#[cfg(target_arch = "wasm32")]
fn open_with_system(_: &std::path::Path) -> Result<(), String> {
    Err("printing needs the desktop app".into())
}

/// A map or feature service Add From Service connects to: its capabilities arrive in the
/// background, then its layers are listed.
pub struct ServiceConn {
    pub url: String,
    /// The saved connection captured when browsing began, never the current dialog selection.
    pub connection: Option<String>,
    /// `wms`, `wmts`, `wfs`, `ogcapi`, `arcgis`, or `auto`.
    pub kind: &'static str,
    pub slot: Slot<Result<Vec<u8>, Failure>>,
    /// The capabilities document, once it has arrived.
    pub text: Option<String>,
    /// Additional capabilities documents, keyed by their requested address.
    pub documents: Vec<Value>,
    /// The capabilities request currently in flight (independent of the entered address).
    pub pending: Option<String>,
    /// What `service.layers` made of it.
    pub listing: Option<Value>,
    /// The listing as Add From Service shows it, worked out when the session revision, the map or
    /// the filter changed (see `dialogs::refresh_added`).
    pub added: Option<ServiceAdded>,
    pub error: Option<String>,
    /// A capabilities request that failed, with what the server said (#509).
    pub failure: Option<Failure>,
}

/// Add From Service's rows: the layers of a listing that match the filter, and for each the
/// command that removes it when it is on the map.
pub struct ServiceAdded {
    /// The session revision this was worked out for.
    pub revision: u64,
    /// A fingerprint of the map's layers and tile layers this was worked out for
    /// (`dialogs::map_stamp`), which the UI can change without a new revision.
    pub map: u64,
    /// The listing filter this was worked out with.
    pub filter: String,
    /// How many layers match the filter (the first `dialogs::SHOWN_LAYERS` are shown).
    pub matched: usize,
    /// Each shown row: its place in the listing, and the command that removes it when it is on
    /// the map (`None`: the row says Add).
    pub rows: Vec<(usize, Option<Removal>)>,
}

/// How Add From Service's list is browsed.
#[derive(Default)]
pub struct ServiceBrowse {
    /// Words the listed layers' titles or names contain.
    pub filter: String,
    /// The date time-enabled layers are added at (their default when empty).
    pub time: String,
    /// Time-enabled layers are added at a fixed date (`time`, or their default when empty) rather
    /// than following the time slider, as they do by default.
    pub fixed: bool,
    /// Feature layers are asked only for the features crossing the map's view (Only the map's
    /// area), not whole.
    pub area: bool,
    /// The directories (and their types) opened before the one shown, the last most recent.
    pub back: Vec<(String, &'static str)>,
}

/// Reading an OGC API layer's tile template from its tileset list (and tileset), off the frame.
pub struct TilesResolve {
    /// `service.add`'s parameters, given the template as `"tiles"` once read.
    pub params: Value,
    /// The tileset list, whose safe query options the template may inherit; the document being read; how many have
    /// been.
    pub list: String,
    pub doc: String,
    /// Runtime discovery lineage: once a document or template crosses origins, later hops
    /// stay anonymous even if they return to a saved connection's origin.
    pub crossed_origin: bool,
    pub read: u8,
    /// Destinations that lost inherited query settings across both document reads.
    pub withheld_addresses: u32,
    pub vector: bool,
    /// The usual-path template, when the advertised one can't be read.
    pub fallback: String,
    pub slot: Slot<Result<Vec<u8>, String>>,
}

/// A service layer waiting on a document before it is added. `dialog` tells whether the add came
/// from the add-service dialog (its result shows there) or from the Connections tree (a notice);
/// `origin` is the connection a tree add is for, named in Recent once the layer is added.
/// `opened` binds every follow-up and final addition to the project that started discovery.
pub enum ServiceStep {
    /// An OGC API layer whose advertised tile template is being read.
    Tiles { resolve: TilesResolve, dialog: bool, origin: Option<String>, opened: u64 },
    /// A WCS coverage whose description is being read before it is asked for: `service.coverage`'s
    /// parameters and the description on its way.
    Coverage { params: Value, slot: Slot<Result<Vec<u8>, String>>, dialog: bool, origin: Option<String>, opened: u64 },
}

impl ServiceStep {
    /// The dialog's progress text is shared with its writer, so a retired step clears only
    /// its own message even when another project is reading a service with the same name.
    fn pending_message(&self) -> Option<String> {
        match self {
            Self::Tiles { resolve, dialog, .. } => {
                dialog.then(|| serviceui::reading_tileset(resolve.params.get("name").and_then(Value::as_str).unwrap_or("")))
            }
            Self::Coverage { params, dialog, .. } => {
                dialog.then(|| serviceui::describing(params.get("name").and_then(Value::as_str).unwrap_or("")))
            }
        }
    }
}

/// Fetches an address into a slot, off the frame.
pub(crate) fn fetch_into(url: &str) -> Slot<Result<Vec<u8>, String>> {
    fetch_into_limited(url, rustgis_engine::render::rustgis_tiles::cache::MAX_DOWNLOAD)
}

/// Like [`fetch_into`], with what is known of a failure (#509).
pub(crate) fn fetch_detailed_into(url: &str) -> Slot<Result<Vec<u8>, Failure>> {
    let slot: Slot<Result<Vec<u8>, Failure>> = Slot::default();
    let land = slot.clone();
    rustgis_engine::render::rustgis_tiles::cache::download_detailed(
        url,
        rustgis_engine::render::rustgis_tiles::cache::MAX_DOWNLOAD,
        Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)),
    );
    slot
}

pub(crate) fn fetch_into_limited(url: &str, max: usize) -> Slot<Result<Vec<u8>, String>> {
    let slot: Slot<Result<Vec<u8>, String>> = Slot::default();
    let land = slot.clone();
    rustgis_engine::render::rustgis_tiles::cache::download_limited(
        url,
        max,
        Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)),
    );
    slot
}

/// How the Layers pane lists the map's layers (#301).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayersView {
    /// In drawing order, top first; the only view that reorders layers.
    #[default]
    DrawingOrder,
    /// Grouped under the file, web address or database each layer comes from.
    DataSource,
    /// Selectable and not selectable layers, with their selected feature counts.
    Selection,
}

/// Retains the GUI's legacy XML decoding while refusing malformed JSON text.
pub(crate) fn service_text(bytes: Vec<u8>) -> Result<String, std::string::FromUtf8Error> {
    String::from_utf8(bytes).or_else(|error| {
        let bytes = error.as_bytes();
        let without_bom = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
        if without_bom.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'<') {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        } else {
            Err(error)
        }
    })
}

/// Panes on the left of the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeftPane {
    Contents,
    Catalog,
    Sources,
}

/// Panes on the right of the map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightPane {
    Geoprocessing,
    Symbology,
    History,
    Details,
    Editing,
    Design,
    Workflows,
    Script,
    Tasks,
    Jobs,
    Profile,
    /// A Zarr variable's time series at points clicked on the map (#555).
    TimeSeries,
    Diagnostics,
    /// Session-local troubleshooting messages, separate from the performance monitor.
    DiagnosticLog,
    Infographics,
    Report,
    Presentation,
    Notebook,
    Assistant,
    Shares,
    Sql,
    Directions,
    Bookmarks,
    /// The selected layout element's properties (#516).
    Format,
    /// The selected feature layer's labels (#617).
    Labeling,
    /// A Home › Add › Web Data service: the one `UiState::web_data` names.
    WebData,
    /// A plugin's panel: the one `UiState::plugin_panel` names.
    Plugin,
}

/// A Unique Values field choice scoped to the layer and renderer that opened it.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingUniqueSymbology {
    project: u64,
    map: usize,
    layer: u32,
    renderer: std::sync::Arc<rustgis_engine::map::Renderer>,
    pub field: String,
}

impl PendingUniqueSymbology {
    fn new(session: &Session, layer: &rustgis_engine::map::Layer, field: String) -> Self {
        Self {
            project: session.opened,
            map: session.project.active_map,
            layer: layer.id,
            renderer: std::sync::Arc::new(layer.renderer.clone()),
            field,
        }
    }

    fn is_current(&self, session: &Session) -> bool {
        self.project == session.opened
            && self.map == session.project.active_map
            && Some(self.layer) == session.active_layer
            && session.map().layer(self.layer).is_some_and(|layer| layer.renderer == *self.renderer)
    }
}

pub struct UiState {
    pub tab: String,
    pub backstage: bool,
    pub backstage_page: &'static str,
    pub left: Option<LeftPane>,
    /// The right pane in view: one of `right_open`, or none. Changed through `open_right`,
    /// `close_right` and `toggle_right`, which keep the two in step.
    pub right: Option<RightPane>,
    /// The right panes open as tabs, in the order they were opened (#303).
    pub right_open: Vec<RightPane>,
    /// The Diagnostics pane's filter (it also picks what **Copy diagnostics** copies).
    pub diagnostic_filter: rustgis_engine::data::diagnostics::Filter,
    diagnostic_snapshot: Option<(u64, Vec<rustgis_engine::data::diagnostics::Record>)>,
    /// The Web Data service the Web Data pane shows (a `ui.dialog` name, see
    /// `panes::web_data_name`).
    pub web_data: &'static str,
    pub dark: bool,
    /// The theme file in use, by name. Empty follows `dark` and the system.
    pub theme_name: String,
    /// The interface scale, 0.8 to 2.0, applied as egui's zoom factor.
    pub interface_scale: f32,
    /// Live measurement and map scale-bar units, chosen in Home › Measure.
    pub measurement_imperial: bool,
    /// The theme follows the system's light or dark setting until the user picks one.
    pub dark_follows_system: bool,
    /// Interface language preference: a language code or `auto`.
    pub language: String,
    /// Worldview preference for the built-in countries and world outline: `auto`, `default` or
    /// `china`.
    pub worldview: String,
    /// Start-up preference: what RustGIS opens with.
    pub start_with: StartWith,
    /// Whether a file holding several layers asks which to add (#934), or adds them all.
    pub ask_layers: bool,
    /// Key combinations the native menu bar keeps for itself (macOS: Quit, Close Window, Hide…):
    /// a shortcut on one would never reach the app, so the shortcut editor refuses them.
    pub reserved_shortcuts: Vec<egui::KeyboardShortcut>,
    /// Files whose layers are being listed or chosen, the first one shown.
    pub choosing: Vec<layerchooser::Chooser>,
    /// The project last opened or saved on this computer, for Start With › Last Project.
    pub last_project: Option<String>,
    /// Start-up preference: the projection the first map starts in (a [`Projection`] code);
    /// empty for Web Mercator.
    ///
    /// [`Projection`]: rustgis_engine::map::Projection
    pub start_projection: String,
    /// The EPSG or ESRI code typed in Settings › Start-up › Projection.
    pub start_projection_input: String,
    /// Start-up preference: the extent the map opens on, `[minLon, minLat, maxLon, maxLat]`;
    /// `None` for the full extent.
    pub start_extent: Option<[f64; 4]>,
    /// The start extent, fitted again once the map view has its real size.
    pub start_view: Option<[f64; 4]>,
    /// The ribbon shows only its tabs; clicking a tab opens it until the next click elsewhere.
    pub ribbon_minimized: bool,
    /// A minimized ribbon temporarily opened from its tab strip.
    pub ribbon_peek: bool,
    pub command_search: Option<String>,
    /// A ribbon button being shown by a task step's Show Me.
    pub show_me: Option<showme::ShowMe>,
    pub about: bool,
    pub shortcuts: bool,
    /// The keyboard shortcuts.
    pub keymap: keymap::Keymap,
    pub(crate) shortcut_editor: shortcutui::Editor,
    /// Preserve unreadable saved shortcuts until the user explicitly resets them.
    pub(crate) shortcut_preferences_blocked: bool,
    /// The shortcut changes are too many to save: they apply only to this session.
    pub(crate) shortcut_preferences_full: bool,
    pub tab_strip_rect: Option<egui::Rect>,
    /// Where the left and right panes were drawn on the last frame, while they show.
    pub pane_rects: [Option<egui::Rect>; 2],
    /// Tabs hidden from the ribbon (Settings › Customize › Ribbon).
    pub hidden_tabs: Vec<String>,
    /// Extra Quick Access Toolbar commands.
    pub quick_access: Vec<String>,
    /// The native title bar the header is drawn into; set by the desktop app on macOS.
    pub titlebar: Option<TitlebarMetrics>,
    /// Saved pane arrangements: (name, left pane, right pane in view, ribbon tab, right panes open).
    pub pane_sets: Vec<(String, Option<LeftPane>, Option<RightPane>, String, Vec<RightPane>)>,
    /// The Layers pane's name filter (#300) and the map it was typed for: another map in view
    /// starts unfiltered. Not saved.
    pub layer_filter: String,
    pub layer_filter_map: String,
    /// The active layer the filter hid (so it stopped being active), selected again once the
    /// filter shows it, unless another layer was picked meanwhile.
    pub layer_filter_hid: Option<u32>,
    /// The Layers pane's layer selection as the frame began (the rows tint from it), the layers its
    /// rows list in the order drawn (what a Shift+click spans), the row a Shift+click spans from,
    /// and the one it was made on, resolved once the rows are all drawn.
    pub layer_picked: Vec<u32>,
    pub layer_rows: Vec<u32>,
    pub layer_anchor: Option<u32>,
    pub layer_range: Option<u32>,
    /// How the Layers pane lists the layers (#301). Kept for the session, not saved.
    pub layers_view: LayersView,
    /// The command an item picked in the browser opens with (web).
    pub pending_open: Option<&'static str>,
    /// The file the project was last saved or opened as (web), which Save downloads again.
    pub web_file: Option<String>,
    /// Add Delimited Text, for the file being picked (web): `null` opens its dialog for the file,
    /// a script's options are read with it.
    pub pending_csv: Option<Value>,
    /// CRS override for the next Shapefile picked by Import Shapefile with Projection.
    pub pending_shapefile: Option<Value>,
    /// Data downloading from web addresses (Home › Add › Add Data › From URL): the address, the parameters to
    /// add it with, and where the download lands.
    pub downloads: Vec<(u64, String, Value, Slot<Result<Vec<u8>, String>>)>,
    /// A project downloading from its web address (`ui.openUrl`, a `?project=` link): the
    /// session's download id, the address and where the download lands. A later one replaces it;
    /// opening another project meanwhile drops it.
    pub project_download: Option<(u64, String, Slot<Result<Vec<u8>, String>>)>,
    /// The SQL pane's editor, results and history.
    pub sql: sqlpane::State,
    /// GeoTIFFs, FlatGeobufs and GeoParquets by address whose headers are being read: those
    /// that can be read in place are added so, others downloaded (the `ui.addUrl` parameters,
    /// and the answer).
    pub cog_probes: Vec<(Value, Option<u64>, Slot<Result<bool, String>>)>,
    /// Windows of cloud-optimised GeoTIFFs (`"bbox"`) being read on threads: the session's
    /// download id, the address, the parameters to add it with, where the read lands, and whether
    /// its project was closed.
    #[cfg(not(target_arch = "wasm32"))]
    pub window_reads: Vec<WindowRead>,
    /// Service layers whose features are being read page by page (an ArcGIS layer's description
    /// first).
    pub service_pages: Vec<serviceui::PageRead>,
    /// A link's data (`?data=`) has zoomed the map: the first of its files to land with something
    /// to show does, whichever it is.
    pub link_zoomed: bool,
    /// The service Add From Service is connected (or connecting) to.
    pub service: Option<ServiceConn>,
    /// Add From Service's list: its filter, a time-enabled layer's date, and the directories
    /// opened on the way to the service shown (Back returns to them).
    pub service_browse: ServiceBrowse,
    /// Service layers added from the dialog or the Connections tree that wait on a document
    /// (a tileset list, a coverage's description) before they are added.
    pub service_steps: Vec<ServiceStep>,
    /// The Web Data panels of single services (NASA Earthdata, EnviroAtlas, FEMA NFHL, The
    /// National Map, Fields of the World): their documents, searches and choices.
    pub web_services: webdataui::State,
    pub service_import: Option<serviceui::Import>,
    pub service_group_name: String,
    /// Add From Service › Catalogue: the search and the records found.
    pub csw: cswui::State,
    /// STAC Search: its choices, results and the request under way.
    pub stac: stacui::State,
    /// WMS and WMTS layers' legend pictures, as the Layers pane shows them (#942).
    pub tile_legends: tilelegend::Legends,
    /// A text field's right-click menu: the field it's open on, and the row picked.
    pub field_menu: fieldmenu::FieldMenu,
    /// S3…: the connection form, the bucket browsed and the listing under way.
    pub s3: s3ui::State,
    /// New/Edit Connection: the form and the Test under way.
    pub connection: connectionui::State,
    /// Choose S3 connection: the saved connection picked for each bucket the project's S3
    /// sources wait on, for the project open.
    pub s3_chooser: connectionui::Chooser,
    /// The datasets saved connections list under Data Sources › Connections, and the selection.
    pub connection_tree: connectiontree::State,
    /// OpenStreetMap and Boundaries: their choices and the request under way.
    pub portals: portalsui::State,
    /// Overture Maps and Source Cooperative: choices, results and the request under way.
    pub catalogs: catalogsui::State,
    /// Search Open Data: the portal, the search under way and the datasets found.
    pub opendata: opendataui::State,
    /// Satellite Embeddings: the dataset, its choices, the read under way and the tiles found.
    pub embeddings: embeddingsui::State,
    /// Data Cubes: the filters, the period, and what the last action said.
    pub cubes: cubesui::State,
    /// The History pane: the tools run or the session log, and the log's filters (#797).
    pub session_log: sessionlog::State,
    /// NASA Earthaccess: the dataset, the granules found and the request under way.
    pub earthaccess: earthaccessui::State,
    /// Add From Service › Sign in to ArcGIS.
    pub arcgis: arcgisui::State,
    /// USGS NLDI: the point traced, its COMID, the navigation and the request under way.
    pub nldi: nldiui::State,
    /// Find Place searches and Geocode Addresses runs under way.
    pub geocode: geocodeui::State,
    /// The Directions pane: places, routes and a simulated drive.
    pub nav: navui::State,
    /// The Bookmarks pane: its filter and the bookmark being edited.
    pub bookmarks: bookmarkpane::State,
    /// The plugin panel pane: whose panel it shows, and a slider being dragged.
    pub plugin_panel: pluginpanel::State,
    /// Plugin commands the user asked for, waiting for the window to show they started.
    plugin_start: pluginstart::State,
    /// The search box at the foot of the left pane.
    pub search: searchbox::State,
    /// The Data Sources pane's boxes, and the sources' generation last kept in the preferences.
    pub sources: sourcespane::State,
    pub sources_saved: u64,
    pub connections_saved: u64,
    /// The default raster style last kept in the preferences.
    pub raster_defaults_saved: rustgis_engine::map::RasterDefaults,
    /// The favourite arrangement last saved, to save it again only when it changes.
    pub layout_favourite_saved: Value,
    /// Home › Add › Add Data › Zarr…: its store, choices and fetches.
    pub zarr: zarrui::State,
    /// Independent Zarr reads for imported GeoLibre layers; does not share the picker state.
    pub geolibre_zarr: Vec<geolibreui::Read>,
    /// A layout picture waiting for its image file (web).
    pub pending_layout_picture: bool,
    /// The corners of a picture graphic waiting for its image file.
    pub pending_picture: Option<Vec<rustgis_engine::geom::Coord>>,
    /// The value picked under Edit › Dynamic Text (a token such as `{scale}`): the text tool's
    /// dialog starts with it. Cleared when the tool changes.
    pub dynamic_text: Option<&'static str>,
    /// Text to put on the system clipboard at the end of the frame.
    pub copy_out: Option<String>,
    /// A web page to open at the end of the frame.
    pub open_url: Option<String>,
    /// An image to put on the system clipboard at the end of the frame.
    pub copy_image: Option<egui::ColorImage>,
    /// The scale being typed into the status bar, while its box has focus.
    pub scale_text: Option<String>,
    /// A field chosen in the Symbology pane before applying Unique Values.
    pub symbology_pending: Option<PendingUniqueSymbology>,
}

impl UiState {
    /// Opens right pane `pane` as a tab (after the others, if it isn't one yet) and shows it.
    pub fn open_right(&mut self, pane: RightPane) {
        if !self.right_open.contains(&pane) {
            self.right_open.push(pane);
        }
        self.right = Some(pane);
    }

    /// Closes right pane `pane`'s tab. Closing the one in view shows its neighbour (the next tab,
    /// else the one before); closing the last hides the right side.
    pub fn close_right(&mut self, pane: RightPane) {
        let Some(i) = self.right_open.iter().position(|p| *p == pane) else {
            if self.right == Some(pane) {
                self.right = None;
            }
            return;
        };
        self.right_open.remove(i);
        if self.right == Some(pane) {
            self.right = self.right_open.get(i).or_else(|| self.right_open.last()).copied();
        }
    }

    /// A ribbon toggle: closes `pane` when it is in view, and otherwise opens or shows it (a toggle
    /// never hides the other panes open).
    pub fn toggle_right(&mut self, pane: RightPane) {
        if self.right == Some(pane) { self.close_right(pane) } else { self.open_right(pane) }
    }
}

impl Default for UiState {
    /// Starts on Home with no pending picker or download, keeping transient imports out of saved state.
    fn default() -> Self {
        UiState {
            tab: "Home".into(),
            backstage: false,
            backstage_page: "open",
            left: Some(LeftPane::Contents),
            right: None,
            right_open: Vec::new(),
            diagnostic_filter: rustgis_engine::data::diagnostics::Filter::All,
            diagnostic_snapshot: None,
            web_data: "opendata",
            dark: false,
            theme_name: String::new(),
            interface_scale: 1.0,
            measurement_imperial: false,
            dark_follows_system: true,
            language: i18n::AUTO.into(),
            worldview: i18n::AUTO.into(),
            start_with: StartWith::Sample,
            ask_layers: true,
            reserved_shortcuts: Vec::new(),
            choosing: Vec::new(),
            last_project: None,
            start_projection: String::new(),
            start_projection_input: String::new(),
            start_extent: None,
            start_view: None,
            ribbon_minimized: false,
            ribbon_peek: false,
            command_search: None,
            show_me: None,
            about: false,
            shortcuts: false,
            keymap: keymap::Keymap::default(),
            shortcut_editor: shortcutui::Editor::default(),
            shortcut_preferences_blocked: false,
            shortcut_preferences_full: false,
            tab_strip_rect: None,
            pane_rects: [None; 2],
            copy_out: None,
            open_url: None,
            copy_image: None,
            pending_picture: None,
            pending_layout_picture: false,
            pending_open: None,
            web_file: None,
            pending_csv: None,
            pending_shapefile: None,
            downloads: Vec::new(),
            project_download: None,
            sql: Default::default(),
            cog_probes: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            window_reads: Vec::new(),
            service_pages: Vec::new(),
            link_zoomed: false,
            service: None,
            service_browse: ServiceBrowse::default(),
            service_steps: Vec::new(),
            web_services: Default::default(),
            service_import: None,
            service_group_name: "My OGC Layer".into(),
            csw: Default::default(),
            stac: Default::default(),
            tile_legends: Default::default(),
            field_menu: Default::default(),
            s3: Default::default(),
            connection: Default::default(),
            s3_chooser: Default::default(),
            connection_tree: Default::default(),
            portals: Default::default(),
            catalogs: Default::default(),
            opendata: Default::default(),
            embeddings: Default::default(),
            cubes: Default::default(),
            session_log: Default::default(),
            earthaccess: Default::default(),
            arcgis: Default::default(),
            nldi: Default::default(),
            geocode: Default::default(),
            nav: Default::default(),
            bookmarks: Default::default(),
            plugin_panel: Default::default(),
            plugin_start: Default::default(),
            search: Default::default(),
            sources: Default::default(),
            sources_saved: 0,
            connections_saved: 0,
            raster_defaults_saved: Default::default(),
            layout_favourite_saved: Value::Null,
            zarr: Default::default(),
            geolibre_zarr: Vec::new(),
            hidden_tabs: Vec::new(),
            quick_access: Vec::new(),
            titlebar: None,
            pane_sets: Vec::new(),
            layer_filter: String::new(),
            layer_filter_map: String::new(),
            layer_filter_hid: None,
            layer_picked: Vec::new(),
            layer_rows: Vec::new(),
            layer_anchor: None,
            layer_range: None,
            layers_view: LayersView::DrawingOrder,
            dynamic_text: None,
            scale_text: None,
            symbology_pending: None,
        }
    }
}

/// Native UI dispatch choices share one pre/post lifecycle; command JSON cannot select them.
enum AppDispatch {
    Direct,
    Prepared(Box<rustgis_engine::policy::PreparedCommand>),
    InPlace(u64),
    Download(u64),
    #[cfg(not(target_arch = "wasm32"))]
    Read(Box<rustgis_engine::cmd::reads::Read>),
    ResolvedTiles {
        template: String,
        crossed_origin: bool,
    },
}

/// The resolution of a layout copied to the clipboard.
const CAPTURE_DPI: f64 = 192.0;
/// The most pixels a layout copied to the clipboard has (a page above it is copied at a lower dpi).
const CAPTURE_PIXELS: f64 = 16_000_000.0;
/// The lowest dpi a layout is copied at: a page that would need less is an error.
const CAPTURE_MIN_DPI: f64 = 36.0;

pub struct RustGisApp {
    pub session: Session,
    pub ui: UiState,
    pub map: mapview::MapView,
    pub dialogs: dialogs::Dialogs,
    pub gp: panes::GpForm,
    pub table: table::TableState,
    pub layout: layoutview::LayoutView,
    /// The text graphic the Format pane edits (#516).
    pub(crate) graphic_format: graphicformat::GraphicFormat,
    pub forms: editpanes::Forms,
    pub workflow: analysispane::Workflow,
    /// An export writing its file on its own thread (see [`exportjob`]).
    #[cfg(not(target_arch = "wasm32"))]
    export_job: Option<exportjob::ExportJob>,
    /// Add Data's and Load All's reads on their own thread, and those waiting (see [`readjob`]).
    #[cfg(not(target_arch = "wasm32"))]
    reads: readjob::Reads,
    /// Add Data and Load All Features read on a thread of their own (the desktop app; see
    /// [`readjob`]). Off, as for tests and the examples, they read as the command runs.
    pub reads_off_frame: bool,
    /// ModelBuilder's view: the model on its canvas.
    pub mb: modelbuilder::ModelBuilder,
    pub ws: workspacepanes::WorkspaceState,
    /// The time slider bar.
    pub time: timebar::TimeBarState,
    /// The Plugin Manager and plugin downloads.
    pub plugins: pluginsui::PluginsState,
    /// Help › Check for Updates.
    pub update: update::UpdateState,
    pub reports: reportpane::ReportState,
    pub docs: docpanes::DocState,
    /// The AI Assistant's conversation and settings.
    pub assistant: assistantpane::AssistantState,
    /// "Let your Agents Control RustGIS" (desktop).
    #[cfg(not(target_arch = "wasm32"))]
    pub connect: connectui::ConnectState,
    fonts_frames: u32,
    /// When this frame's `logic` began, for the double-click delay `ui` sets when it ends.
    frame_start: Option<web_time::Instant>,
    applied_theme: Option<(String, bool)>,
    /// The themes the app can offer: built in, then the ones in the user's themes folder.
    pub themes: Vec<theme::Theme>,
    /// The theme files that did not load, with the reason.
    pub theme_errors: Vec<String>,
    /// What the reader changed in the loaded theme files, as the menu lists it.
    pub theme_notes: Vec<String>,
    themes_stamp: u64,
    /// Changes on every folder read, including explicit reloads with unchanged metadata.
    themes_revision: u64,
    themes_checked: f64,

    sent_title: String,
    pub quit_requested: bool,
    logo: Option<egui::TextureHandle>,
    pub services: Services,
    /// Autosave's interval, its last copy, and the copy an earlier run left (#873).
    pub(crate) autosave: autosave::AutosaveState,
    /// The question asked before unsaved changes are discarded, and a quit the user agreed to.
    pub(crate) save_prompt: saveprompt::SavePromptState,
    /// The MCP relay for `rustgis-cli mcp --attach`: a loopback listener on the session this app
    /// already has open, started by the `ui.mcpRelay` command and off until then.
    #[cfg(not(target_arch = "wasm32"))]
    pub relay: Option<rustgis_assistant::relay::Relay>,
    /// The file bridge for agents that can't use MCP: on while agents are turned on, with or
    /// without the relay ([`connectui`]).
    #[cfg(not(target_arch = "wasm32"))]
    pub file_bridge: Option<rustgis_assistant::filebridge::FileBridge>,
    /// A screen snapshot an agent attached through the relay asked for, while it is captured.
    #[cfg(not(target_arch = "wasm32"))]
    relay_shot: Option<relayshot::Shot>,
    /// Where the relay snapshot's window screenshot lands.
    #[cfg(not(target_arch = "wasm32"))]
    relay_arrived: relayshot::Arrived,
    /// The frame's `egui::Context`, kept so `ui.mcpRelay` can hand the relay its wake callback:
    /// a queued message asks for a repaint, or it waits for the user to move the mouse.
    #[cfg(not(target_arch = "wasm32"))]
    frame_ctx: Option<egui::Context>,
    /// Set when `ui.mcpRelay` started the relay before the first frame, where there is no context
    /// to give its wake callback yet: `logic` installs the wake as soon as a context exists.
    #[cfg(not(target_arch = "wasm32"))]
    relay_wake_pending: bool,
    /// Whether session plugins' programs ask for a repaint when they send a line.
    #[cfg(not(target_arch = "wasm32"))]
    plugin_wake_set: bool,
    /// Extra font faces installed with the interface fonts: (name, face, script). The current
    /// language's face comes right after the interface font (before it for Arabic script), so its
    /// glyph shapes win; the others are last-resort fallbacks. Faces are shared, so reinstalling
    /// copies nothing.
    extra_fonts: Vec<(String, std::sync::Arc<egui::FontData>, Option<rustgis_engine::render::fonts::ScriptFont>)>,
    /// The script whose face was put first at the last font install.
    installed_region: Option<rustgis_engine::render::fonts::ScriptFont>,
    fonts_dirty: bool,
    fonts_requested: Vec<String>,
    /// Downloads that failed; cleared when a language is picked, which retries them.
    fonts_failed: Vec<String>,
    /// Fonts were installed this frame: `set_fonts` only takes effect on the next one.
    fonts_pending: bool,
    /// Whether the installed font families were indexed by a frame (see `families::warm_up`).
    label_fonts_indexed: bool,
    /// The language the interface is drawn in. It trails the chosen one until a font for its
    /// script is active, so switching to Chinese never draws a frame of missing-glyph boxes.
    shown_lang: i18n::Lang,
    /// The project path last seen, so a change (whoever opened or saved it) updates Last Project.
    seen_project: Option<std::path::PathBuf>,
}

impl RustGisApp {
    /// The layer to export: `layer` (an id or name, as commands take it; the dialog's is checked
    /// against its `instance`, so a reused id isn't taken for it), else the active layer.
    fn export_target(&self, p: &Value) -> Result<u32, String> {
        let gone = || tl!("the layer Export Features was opened on is gone").to_string();
        match p.get("layer") {
            None | Some(Value::Null) => {
                return self.session.active_layer.ok_or_else(|| tl!("select a layer in the Layers pane first").to_string());
            }
            Some(Value::Number(_) | Value::String(_)) => {}
            // Not taken for the active layer: with replaceTable that would overwrite a table.
            Some(other) => {
                return Err(crate::i18n::fmt("\"layer\" must be a layer's id or name, not {value}", &[("value", &other.to_string())]));
            }
        }
        let id = self.session.layer_id(p)?;
        let l = self.session.map().layer(id).ok_or_else(gone)?;
        match p.get("instance") {
            None | Some(Value::Null) => Ok(id),
            Some(v) => match v.as_u64() {
                Some(instance) if instance == l.instance_id() => Ok(id),
                Some(_) => Err(gone()),
                None => Err(crate::i18n::fmt("\"instance\" must be a layer's instance number, not {value}", &[("value", &v.to_string())])),
            },
        }
    }

    /// The Ctrl (⌘) shortcuts the keymap takes, as (Alt, Shift, key), the clipboard's left out:
    /// in a browser, what the web app keeps from the browser's own shortcuts (⌘D bookmarking
    /// the page, ⌘Y opening its history, ⌘= zooming it), as design tools in the browser do.
    pub fn command_keys(&self) -> Vec<(bool, bool, egui::Key)> {
        let mut keys: Vec<(bool, bool, egui::Key)> = self
            .ui
            .keymap
            .bindings
            .iter()
            .map(|b| b.chord)
            .filter(|c| c.command && !keymap::is_clipboard(*c))
            .map(|c| (c.alt, c.shift, c.key))
            .collect();
        keys.sort_by_key(|k| (k.0, k.1, k.2 as u32));
        keys.dedup();
        keys
    }

    /// The keyboard shortcuts of a command run without parameters, in the keymap's order, for a
    /// native menu to show (the macOS menu bar).
    pub fn shortcuts_of(&self, command: &str) -> Vec<egui::KeyboardShortcut> {
        self.ui.keymap.keys_for(command, &Value::Null).map(|c| egui::KeyboardShortcut::new(c.modifiers(), c.key)).collect()
    }

    /// The command a keyboard shortcut runs, if the keymap gives it one.
    pub fn shortcut_command(&self, shortcut: egui::KeyboardShortcut) -> Option<String> {
        let m = shortcut.modifiers;
        // A chord has no Ctrl apart from ⌘ (the Mac's Ctrl isn't one): ⌃T isn't the bare T.
        if m.ctrl && !m.command {
            return None;
        }
        let chord = keymap::Chord { command: m.command || m.mac_cmd, alt: m.alt, shift: m.shift, key: shortcut.logical_key };
        self.ui.keymap.command_for(chord).map(|b| b.command.clone())
    }

    /// Whether the keyboard shortcut editor is waiting for a key combination to record.
    pub fn capturing_shortcut(&self) -> bool {
        self.ui.shortcut_editor.capturing()
    }

    /// Whether a command exists and can run now: what the ribbon greys out.
    pub fn command_enabled(&self, command: &str) -> bool {
        widgets::enabled(self, command)
    }

    /// A fingerprint of the keymap's key combinations and what they run: cheap to take every
    /// frame (no allocation), it changes when a shortcut is added, removed or rebound (to
    /// another key or another command), so a caller can rebuild [`RustGisApp::command_keys`]
    /// only then.
    pub fn shortcuts_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for b in &self.ui.keymap.bindings {
            b.chord.hash(&mut h);
            // And what it runs: a key rebound to another command changes it too.
            b.command.hash(&mut h);
            if !b.params.is_null() {
                // Written straight into the hash: no allocation.
                struct Feed<'a>(&'a mut std::collections::hash_map::DefaultHasher);
                impl std::io::Write for Feed<'_> {
                    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                        self.0.write(buf);
                        Ok(buf.len())
                    }
                    fn flush(&mut self) -> std::io::Result<()> {
                        Ok(())
                    }
                }
                let _ = serde_json::to_writer(Feed(&mut h), &b.params);
            }
        }
        self.ui.keymap.bindings.len().hash(&mut h);
        h.finish()
    }

    /// Whether a browser key press (its `key` and `code`, Ctrl or ⌘ held, Alt, Shift) is one of
    /// [`RustGisApp::command_keys`]: as egui matches a shortcut, an extra Alt or Shift doesn't
    /// stop it (⌘+ is ⌘⇧= on most keyboards).
    pub fn takes_browser_key(keys: &[(bool, bool, egui::Key)], key: &str, code: &str, command: bool, alt: bool, shift: bool) -> bool {
        command
            && keymap::browser_key(key, code).is_some_and(|k| keys.iter().any(|&(a, s, bound)| bound == k && (!a || alt) && (!s || shift)))
    }

    /// egui's wgpu render state from the host (`frame.wgpu_render_state()`), feeding the GPU
    /// map backend. Cheap to call every frame: the state is Arc-backed.
    pub fn set_render_state(&mut self, rs: Option<egui_wgpu::RenderState>) {
        self.map.set_render_state(rs);
    }

    /// The app around `session`, with no platform hooks (see [`Self::with_services`]).
    pub fn new(session: Session) -> Self {
        // The installed fonts a label can be drawn in, indexed while the app starts.
        rustgis_engine::render::families::warm_up();
        let mut s = session;
        // A time slider move doesn't wait for a Zarr step to be read (#1291 step 3).
        s.zarr_off_frame = cfg!(not(target_arch = "wasm32"));
        if s.active_layer.is_none() {
            s.active_layer = s.map().layers.first().map(|l| l.id);
        }
        // The favourite arrangement counts as saved until a command changes it, so one never set
        // isn't written to the preferences.
        let ui =
            UiState { keymap: keymap::Keymap::new(&s.registry), layout_favourite_saved: s.layout_favourite.clone(), ..UiState::default() };
        RustGisApp {
            session: s,
            ui,
            map: mapview::MapView::default(),
            dialogs: dialogs::Dialogs::default(),
            gp: panes::GpForm::default(),
            table: table::TableState::default(),
            layout: layoutview::LayoutView::default(),
            graphic_format: graphicformat::GraphicFormat::default(),
            forms: editpanes::Forms { distance_units: "Meters".into(), ..editpanes::Forms::default() },
            workflow: analysispane::Workflow::default(),
            #[cfg(not(target_arch = "wasm32"))]
            export_job: None,
            #[cfg(not(target_arch = "wasm32"))]
            reads: readjob::Reads::default(),
            reads_off_frame: false,
            mb: modelbuilder::ModelBuilder::default(),
            ws: workspacepanes::WorkspaceState::default(),
            time: timebar::TimeBarState::default(),
            plugins: pluginsui::PluginsState::default(),
            update: update::UpdateState::default(),
            reports: reportpane::ReportState::default(),
            docs: docpanes::DocState::default(),
            assistant: assistantpane::AssistantState::default(),
            fonts_frames: 0,
            frame_start: None,
            applied_theme: None,
            themes: theme::Theme::builtin(),
            theme_errors: Vec::new(),
            theme_notes: Vec::new(),
            themes_stamp: 0,
            themes_revision: 0,
            themes_checked: 0.0,

            sent_title: String::new(),
            quit_requested: false,
            logo: None,
            services: Services::default(),
            #[cfg(not(target_arch = "wasm32"))]
            relay: None,
            #[cfg(not(target_arch = "wasm32"))]
            file_bridge: None,
            #[cfg(not(target_arch = "wasm32"))]
            relay_shot: None,
            #[cfg(not(target_arch = "wasm32"))]
            relay_arrived: relayshot::Arrived::default(),
            #[cfg(not(target_arch = "wasm32"))]
            connect: Default::default(),
            #[cfg(not(target_arch = "wasm32"))]
            frame_ctx: None,
            #[cfg(not(target_arch = "wasm32"))]
            relay_wake_pending: false,
            #[cfg(not(target_arch = "wasm32"))]
            plugin_wake_set: false,
            extra_fonts: Vec::new(),
            installed_region: None,
            fonts_dirty: false,
            fonts_requested: Vec::new(),
            fonts_failed: Vec::new(),
            fonts_pending: false,
            label_fonts_indexed: false,
            shown_lang: i18n::Lang::EN,
            seen_project: None,
            autosave: Default::default(),
            save_prompt: Default::default(),
        }
    }

    /// Installs the shell's platform hooks and reads what they keep: preferences, saved
    /// connections and the recovery copy an earlier run left.
    pub fn with_services(mut self, services: Services) -> Self {
        // Saved database passwords, S3 keys and the remembered Earthdata Login token use the same
        // store, installed before prefs.
        self.session.database_logins.set_store(services.secrets.clone());
        rustgis_engine::render::rustgis_tiles::earthdata::set_secret_store(services.secrets.clone());
        self.services = services;
        shortcutui::load(&mut self);
        sourcespane::load_connections(&mut self);
        autosave::load(&mut self);
        if let Some(lang) = self.services.load_pref.as_ref().and_then(|load| load("language")) {
            self.ui.language = lang;
        }
        // Theme readers and startup notices keep translated text, so select the language before reading.
        i18n::set_current(self.chosen_lang());
        // After the language, so a notice about the saved value is in it.
        threadsui::load(&mut self);
        // A stored value that is neither `auto` nor a known worldview means `auto`.
        if let Some(w) = self.services.load_pref.as_ref().and_then(|load| load("worldview"))
            && rustgis_engine::sample::Worldview::from_code(&w).is_some()
        {
            self.ui.worldview = w.trim().to_ascii_lowercase();
        }
        self.apply_worldview();
        self.ui.start_with =
            self.services.load_pref.as_ref().and_then(|load| load("start_project")).map_or(StartWith::Sample, |v| StartWith::from_pref(&v));
        // A shell without file paths (the web app) can't open a file at start.
        if self.ui.start_with.opens_a_file() && !self.opens_files() {
            self.ui.start_with = StartWith::Sample;
        }
        self.ui.last_project = self.services.load_pref.as_ref().and_then(|load| load("last_project")).filter(|p| !p.trim().is_empty());
        self.ui.ask_layers = self.services.load_pref.as_ref().and_then(|load| load("ask_file_layers")).is_none_or(|v| v != "off");
        // A stored projection this version can't use means Web Mercator.
        if let Some(code) = self.services.load_pref.as_ref().and_then(|load| load("start_projection"))
            && let Ok(p) = rustgis_engine::map::Projection::from_code(&code)
        {
            self.ui.start_projection = p.code();
        }
        self.ui.start_extent = self.services.load_pref.as_ref().and_then(|load| load("start_extent")).and_then(|v| parse_extent(&v));
        // How new single-band rasters are drawn (Save as Default in the raster Symbology pane).
        if let Some(d) = self.services.load_pref.as_ref().and_then(|load| load(RASTER_DEFAULTS_PREF))
            && let Ok(d) = serde_json::from_str::<rustgis_engine::map::RasterDefaults>(&d)
        {
            self.session.raster_defaults = d.sanitized();
        }
        self.ui.raster_defaults_saved = self.session.raster_defaults.clone();
        // The favourite arrangement of publication layouts; one no longer valid is passed over,
        // said, and classic used.
        if let Some(text) = self.services.load_pref.as_ref().and_then(|load| load(LAYOUT_FAVOURITE_PREF)) {
            let read = serde_json::from_str::<Value>(&text)
                .map_err(|e| e.to_string())
                .and_then(|v| rustgis_engine::cmd::layout::parse_arrangement(&v).map(|_| v));
            match read {
                // A saved null is the default, named.
                Ok(v) if v.is_null() => self.session.layout_favourite = Value::String("classic".into()),
                Ok(v) => self.session.layout_favourite = v,
                Err(why) => {
                    self.session.layout_favourite = Value::String("classic".into());
                    self.session.notify(
                        crate::i18n::fmt(
                            "The saved favourite arrangement of publication layouts can't be used ({why}), so \"classic\" is used.",
                            &[("why", &why)],
                        ),
                        false,
                    );
                }
            }
        }
        self.ui.layout_favourite_saved = self.session.layout_favourite.clone();
        self.forms.distance_units = self
            .services
            .load_pref
            .as_ref()
            .and_then(|load| load(editpanes::DISTANCE_UNITS_PREF))
            .and_then(|value| {
                editpanes::DISTANCE_UNITS.iter().find(|unit| unit.eq_ignore_ascii_case(value.trim())).map(|unit| (*unit).to_owned())
            })
            .unwrap_or_else(|| "Meters".into());
        self.ui.measurement_imperial =
            self.services.load_pref.as_ref().and_then(|load| load("measurement_units")).as_deref() == Some("imperial");
        // The pointer's coordinate format; a missing one reads as decimal degrees. One this
        // version doesn't know (a newer RustGIS may have saved it) is said and read as decimal
        // degrees too. It stays saved until the user picks a format (`ui.coordinateFormat`).
        if let Some(code) = self.services.load_pref.as_ref().and_then(|load| load(coordreadout::PREF)) {
            match CoordReadout::from_code(&code) {
                Some(format) => self.map.coord_format = format,
                None => {
                    self.session
                        .notify(tl!("The saved coordinate format isn't known to this version, so decimal degrees are shown."), false);
                }
            }
        }
        // The GPU map backend: RUSTGIS_GPU forces it on, else the saved preference.
        self.map.gpu_enabled = std::env::var_os("RUSTGIS_GPU").is_some()
            || self.services.load_pref.as_ref().and_then(|load| load("ui.map.gpu")).as_deref() == Some("1");
        if let Some(m) = self.services.load_pref.as_ref().and_then(|load| load("ribbon_minimized")) {
            self.ui.ribbon_minimized = m == "true";
        }
        let list = |s: Option<String>| -> Vec<String> {
            s.map(|v| v.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()).unwrap_or_default()
        };
        // take_saved_look finds a saved theme only once the folder is read.
        self.load_themes();
        self.take_saved_look();
        self.ui.hidden_tabs = ribbon::current_hidden_tabs(list(self.services.load_pref.as_ref().and_then(|load| load("hidden_tabs"))));
        self.ui.quick_access = list(self.services.load_pref.as_ref().and_then(|load| load("quick_access")));
        // The browser's GeoLibre tools, loaded in an earlier visit: load them again (the runner
        // comes from the browser's cache).
        if self.services.load_pref.as_ref().and_then(|load| load("geolibre_web")).as_deref() == Some("on") {
            panes::start_geolibre_load(&mut self);
        }
        // Plugins the web app installed in earlier visits (the desktop loads its plugins folder).
        // Their fetches (the blocklist, each plugin) are this session's: a failure is recorded
        // in its Diagnostics.
        {
            let _diagnostics = self.session.diagnostics.enter();
            pluginsui::restore(&mut self);
        }
        // A GeoLibre runner located earlier (Locate… in the Tools pane).
        if let Some(path) = self.services.load_pref.as_ref().and_then(|load| load("geolibre_runner")).filter(|p| !p.is_empty())
            && let Err(e) = self.session.run("gp.setGeolibre", &json!({"path": path}))
        {
            log::warn!("{e}");
        }
        self
    }

    /// The language the interface is drawn in now (see [`Self::chosen_lang`]).
    pub fn lang(&self) -> i18n::Lang {
        self.shown_lang
    }

    /// The language picked in Settings › Language (or the system's, for `auto`).
    pub fn chosen_lang(&self) -> i18n::Lang {
        i18n::Lang::from_pref(&self.ui.language)
    }

    /// Whether the interface can be drawn in `lang` this frame: its script needs no extra font, or
    /// that font is installed and active (fonts installed this very frame aren't yet). A web app
    /// waits for the download; a failed one keeps the previous language. A desktop without the
    /// font has nothing to wait for.
    fn can_draw(&self, lang: i18n::Lang) -> bool {
        let Some(region) = Self::script_region(lang.script()) else { return true };
        let have = self.extra_fonts.iter().any(|(_, _, r)| *r == Some(region));
        let can_fetch = self.services.fetch_font.is_some() && rustgis_engine::render::fonts::script_font_url(region).is_some();
        (have || !can_fetch) && !self.fonts_pending && self.installed_region == Some(region)
    }

    /// On the desktop, an installed CJK face as a last-resort fallback, so CJK text (language names
    /// in the picker, place names in data) renders whatever the interface language. The bytes are
    /// borrowed from the font cache, not copied.
    fn add_cjk_fallback(&mut self) {
        use rustgis_engine::render::fonts::{CjkRegion, cjk_font};
        if self.extra_fonts.iter().any(|(n, _, _)| n == "cjk-fallback") {
            return;
        }
        let found = [CjkRegion::SimplifiedChinese, CjkRegion::Japanese, CjkRegion::Korean].into_iter().find_map(cjk_font);
        if let Some((bytes, index)) = found {
            let mut face = egui::FontData::from_static(bytes);
            face.index = *index;
            self.extra_fonts.push(("cjk-fallback".into(), std::sync::Arc::new(face), None));
        }
    }

    /// On the desktop, an installed symbol face as a last-resort fallback, so marks the interface
    /// faces lack (Segoe UI has no "✓") don't draw as boxes. The bytes are borrowed from the font
    /// cache, not copied.
    fn add_symbol_fallback(&mut self) {
        if self.extra_fonts.iter().any(|(n, _, _)| n == "symbol-fallback") {
            return;
        }
        if let Some((bytes, index)) = rustgis_engine::render::fonts::symbol_font() {
            let mut face = egui::FontData::from_static(bytes);
            face.index = *index;
            self.extra_fonts.push(("symbol-fallback".into(), std::sync::Arc::new(face), None));
        }
    }

    /// The script face the current language needs, if any.
    fn current_region(&self) -> Option<rustgis_engine::render::fonts::ScriptFont> {
        Self::script_region(self.chosen_lang().script())
    }

    /// The face a script needs beyond the interface fonts, if any.
    fn script_region(script: i18n::Script) -> Option<rustgis_engine::render::fonts::ScriptFont> {
        use rustgis_engine::render::fonts::{CjkRegion, ScriptFont};
        match script {
            i18n::Script::Cjk(i18n::CjkFont::SimplifiedChinese) => Some(ScriptFont::Cjk(CjkRegion::SimplifiedChinese)),
            i18n::Script::Cjk(i18n::CjkFont::Japanese) => Some(ScriptFont::Cjk(CjkRegion::Japanese)),
            i18n::Script::Cjk(i18n::CjkFont::Korean) => Some(ScriptFont::Cjk(CjkRegion::Korean)),
            i18n::Script::Arabic => Some(ScriptFont::Arabic),
            i18n::Script::Bengali => Some(ScriptFont::Bengali),
            i18n::Script::Lao => Some(ScriptFont::Lao),
            i18n::Script::Alphabetic => None,
        }
    }

    /// Makes sure the current language's script has a font: an installed face on the desktop, a
    /// download on the web.
    fn ensure_script_font(&mut self) {
        use rustgis_engine::render::fonts::{script_font, script_font_url};
        // Switching between script languages reorders the faces already loaded.
        if self.current_region() != self.installed_region {
            self.fonts_dirty = true;
        }
        let Some(region) = self.current_region() else { return };
        let name = format!("script-{region:?}");
        if self.extra_fonts.iter().any(|(n, _, _)| *n == name) || self.fonts_requested.contains(&name) || self.fonts_failed.contains(&name)
        {
            return;
        }
        self.fonts_requested.push(name.clone());
        if let Some((bytes, index)) = script_font(region) {
            let mut face = egui::FontData::from_static(bytes);
            face.index = *index;
            self.extra_fonts.push((name, std::sync::Arc::new(face), Some(region)));
            self.fonts_dirty = true;
        } else if let (Some(fetch), Some(url)) = (&self.services.fetch_font, script_font_url(region)) {
            fetch(url);
        } else {
            log::warn!("no {region:?} font installed; text in that script may not display");
        }
    }

    /// Adds the waiting picture graphic with its image (`path` or base64 `image`).
    fn add_picture(&mut self, mut p: Value) -> Result<Value, String> {
        let pts = self.ui.pending_picture.take().ok_or("drag the picture's rectangle on the map first")?;
        p["shape"] = json!("picture");
        p["points"] = json!(pts.iter().map(|c| [c.x, c.y]).collect::<Vec<_>>());
        let r = self.run("graphics.add", p);
        if r.is_ok() && !self.session.keep_tool {
            self.session.tool = rustgis_engine::Tool::Explore;
        }
        r
    }

    /// A layout as PDF or PNG bytes, guarded like `Session::run`.
    fn layout_bytes(&mut self, li: usize, pdf: bool, dpi: f64) -> Result<Vec<u8>, String> {
        let session = &mut self.session;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rustgis_engine::cmd::layout::layout_bytes(session, li, pdf, dpi, true)
        }))
        .unwrap_or_else(|_| Err("the layout export failed unexpectedly".into()));
        if let Err(e) = &r {
            self.session.notify(e.clone(), true);
        }
        r
    }

    /// Opens a file by name and contents: a `.rgis` project, a raster, or vector data.
    /// Failures are reported to the user (command errors through `Session::run`, others here).
    pub fn open_bytes(&mut self, name: &str, bytes: &[u8]) -> Result<Value, String> {
        self.open_bytes_with(name, bytes, None)
    }

    /// Opens a file from the inbox: one the `"text"` picker marked ([`DELIMITED_PICK`]) opens Add
    /// Delimited Text for it, or is read with a script's options; one the tool file picker marked
    /// ([`TOOL_FILE_PICK`]) goes to the Tools pane. Source-CRS picks carry [`SHAPEFILE_PICK`];
    /// shortcut picks carry [`SHORTCUT_PICK`] and open the validated merge preview.
    pub fn open_picked(&mut self, name: &str, bytes: &[u8]) -> Result<Value, String> {
        if name.starts_with(SHORTCUT_PICK) {
            return shortcutfile::picked(self, bytes);
        }
        if let Some(name) = name.strip_prefix(TOOL_FILE_PICK) {
            panes::received_file(self, name, bytes);
            return Ok(Value::Null);
        }
        if let Some(name) = name.strip_prefix(SHAPEFILE_PICK) {
            if !name.to_ascii_lowercase().ends_with(".zip") {
                let error = tl!("Choose a zipped Shapefile, or pick its .shp and .dbf together.").to_string();
                self.session.notify(error.clone(), true);
                return Err(error);
            }
            let Some(options) = self.ui.pending_shapefile.clone() else {
                let error = tl!("Choose the source coordinate system before importing").to_string();
                self.session.notify(error.clone(), true);
                return Err(error);
            };
            return self.open_bytes_with(name, bytes, Some(options));
        }
        match name.strip_prefix(DELIMITED_PICK) {
            Some(name) => match self.ui.pending_csv.take() {
                Some(Value::Null) => dialogs::open_csv(self, name, dialogs::CsvSource::Bytes(bytes.to_vec())),
                csv => self.open_bytes_with(name, bytes, csv),
            },
            None => self.open_bytes(name, bytes),
        }
    }

    /// Opens file bytes with picker options, forwarding source CRS overrides before ZIP parsing.
    fn open_bytes_with(&mut self, name: &str, bytes: &[u8], csv: Option<Value>) -> Result<Value, String> {
        let lower = name.to_ascii_lowercase();
        // A plugin package: ask before installing it.
        if lower.ends_with(".rgplugin") {
            pluginsui::received(self, bytes.to_vec(), None);
            return Ok(Value::Null);
        }
        // Project items picked or dropped in the browser.
        let item = [
            (".geolibre", "project.importGeolibre"),
            (".geolibre.json", "project.importGeolibre"),
            (".rgismap", "map.import"),
            (".rgismpk", "map.import"),
            (".rgislyr", "layer.addLayerFile"),
            (".rgisstyle", "style.add"),
            (".rgisramp", "ramp.add"),
            (".cpt", "ramp.add"),
            (".rgistemplate", "project.fromTemplate"),
            (".rgistask", "task.import"),
            (".rgisbookmarks", "bookmark.import"),
            (".rgisshortcuts", "ui.shortcuts.import"),
            (".rgisreport", "report.open"),
            (".rgisrpttemplate", "report.open"),
            (".rgispres", "presentation.import"),
            (".rgisnb", "notebook.open"),
            (".ipynb", "notebook.open"),
            (".rgisdlpk", "dl.import"),
            (".model.json", "model.import"),
            (".rgistool", "model.import"),
        ]
        .iter()
        .find(|(e, _)| lower.ends_with(e))
        .map(|(_, c)| *c);
        if let Some(cmd) = self.ui.pending_open.filter(|c| c.ends_with(".thumbnail")) {
            use base64::Engine;
            self.ui.pending_open = None;
            return self.run(cmd, json!({"image": base64::engine::general_purpose::STANDARD.encode(bytes)}));
        }
        if let Some(cmd) = self.ui.pending_open.take().or(item) {
            if cmd == "ui.shortcuts.import" {
                return shortcutfile::picked(self, bytes);
            }
            use base64::Engine;
            let mut p = json!({"data": base64::engine::general_purpose::STANDARD.encode(bytes)});
            // A colour ramp file's name tells its format and names its ramp.
            if cmd == "ramp.add" {
                p["file"] = json!(name);
            }
            let r = self.run(cmd, p);
            self.map.invalidate();
            // A model file opens in ModelBuilder.
            if cmd == "model.import"
                && let Ok(v) = &r
            {
                modelbuilder::imported(self, v);
            }
            return r;
        }
        if lower.ends_with(".rgispkg") {
            let text = rustgis_engine::cmd::items::package_text(bytes)?;
            let r = self.run("project.openText", json!({"text": text, "package": true}));
            self.map.invalidate();
            return r;
        }
        if lower.ends_with(".rgislayout") {
            let text = std::str::from_utf8(bytes).map_err(|_| "not a RustGIS layout".to_string())?;
            return self.run("layout.import", json!({"text": text}));
        }
        // A layout picture needs the layout still open.
        if self.session.active_layout.is_none() {
            self.ui.pending_layout_picture = false;
        }
        if self.ui.pending_layout_picture && [".png", ".jpg", ".jpeg"].iter().any(|e| lower.ends_with(e)) {
            use base64::Engine;
            self.ui.pending_layout_picture = false;
            let image = base64::engine::general_purpose::STANDARD.encode(bytes);
            return self.run("layout.addElement", json!({"kind": "picture", "image": image}));
        }
        if self.ui.pending_picture.is_some() && [".png", ".jpg", ".jpeg"].iter().any(|e| lower.ends_with(e)) {
            use base64::Engine;
            return self.add_picture(json!({"image": base64::engine::general_purpose::STANDARD.encode(bytes)}));
        }
        if rustgis_engine::data::raster::is_raster_name(name)
            || rustgis_engine::data::source::is_source_name(name)
            || rustgis_engine::data::io::is_multi_layer_name(name)
            || rustgis_engine::data::sheets::is_sheet_name(name)
        {
            use base64::Engine;
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
            let mut p = json!({"fileName": name, "data": data});
            // A picked spreadsheet or Shapefile ZIP takes its import options.
            if let Some(Value::Object(opts)) = csv.filter(|_| rustgis_engine::data::sheets::is_sheet_name(name) || lower.ends_with(".zip"))
            {
                for (k, v) in opts {
                    p[k] = v;
                }
            }
            return self.run("layer.addDataBytes", p);
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            let e = crate::i18n::fmt("{name} is not a text file: RustGIS opens GeoJSON, CSV, GeoTIFF and .rgis here", &[("name", name)]);
            self.session.notify(e.clone(), true);
            return Err(e);
        };
        if rustgis_engine::project::is_project_file(name) {
            let r = self.run("project.openText", json!({"text": text, "fileName": name}));
            if r.is_ok() {
                self.ui.web_file = Some(format!("{}.rgis", self.session.project.name));
            }
            self.map.invalidate();
            r
        } else {
            let mut p = json!({"fileName": name, "text": text});
            if let Some(Value::Object(opts)) = csv.filter(|_| [".csv", ".tsv", ".txt"].iter().any(|e| lower.ends_with(e))) {
                for (k, v) in opts {
                    p[k] = v;
                }
            }
            self.run("layer.addDataText", p)
        }
    }

    /// Downloads the project as `file` (web), naming the project after it as Save As does
    /// natively. A name without the `.rgis` extension gets it.
    pub(crate) fn save_download(&mut self, file: &str) -> Result<(), String> {
        let file = file.trim();
        let stem = if file.to_ascii_lowercase().ends_with(".rgis") { file.get(..file.len() - 5).unwrap_or(file) } else { file }.trim();
        if stem.is_empty() || stem.contains(['/', '\\']) {
            return Err(tl!("type a file name").into());
        }
        if self.services.download.is_none() {
            return Err(tl!("this build saves projects with Save As").into());
        }
        let stem: String = stem.chars().take(200).collect();
        // Not an undoable edit: saving names the project, as `project.saveAs` does, once the
        // file is written.
        let old = std::mem::replace(&mut self.session.project.name, stem.clone());
        let (text, report) = match self.session.project.saved_text(None) {
            Ok(saved) => saved,
            Err(e) => {
                self.session.project.name = old;
                return Err(e.to_string());
            }
        };
        let file = format!("{stem}.rgis");
        if let Err(e) = self.services.download.as_ref().ok_or(tl!("no download service"))?(&file, text.into_bytes()) {
            self.session.project.name = old;
            return Err(e);
        }
        self.session.notify(crate::i18n::fmt("Downloaded {name}", &[("name", &file)]), false);
        for notice in report.notices() {
            self.session.notify(notice, false);
        }
        self.session.mark_saved();
        self.ui.web_file = Some(file);
        Ok(())
    }

    /// Offers bytes as a download (web), and says whether it was delivered (a failure is
    /// notified).
    pub(crate) fn download_bytes(&mut self, name: &str, bytes: Vec<u8>) -> bool {
        self.download(name, bytes)
    }

    /// Offers bytes as a download (web) and reports whether a download hook exists.
    fn download(&mut self, name: &str, bytes: Vec<u8>) -> bool {
        match &self.services.download {
            Some(d) => {
                if let Err(e) = d(name, bytes) {
                    self.session.notify(e, true);
                    return false;
                }
                self.session.notify(crate::i18n::fmt("Downloaded {name}", &[("name", name)]), false);
                true
            }
            None => false,
        }
    }

    /// Reports acknowledgment failures even when the engine refuses before command dispatch.
    fn acknowledge_gp_file(&mut self, id: u64, error: Option<&str>) -> Result<Value, String> {
        let mark = self.session.notice_mark();
        let completed = self.session.acknowledge_gp_file(id, error);
        if let Err(error) = &completed {
            self.command_failed("gp.fileDelivered", error, mark);
        }
        completed
    }

    /// Delivers exports from all command entry points, including scripts and the assistant.
    /// The engine records success only after the service submits the file to the host.
    fn deliver_gp_files(&mut self, mut requested: Result<Value, String>) -> Result<Value, String> {
        for (id, name, bytes) in self.session.take_gp_files() {
            let delivered = match &self.services.download {
                Some(download) => download(&name, bytes),
                None => Err(tl!("this build has no download service").into()),
            };
            // gp.fileDelivered reports delivery failures from its history record. The app also
            // reports acknowledgment failures before dispatch, without repeating engine notices.
            let completed = self.acknowledge_gp_file(id, delivered.as_ref().err().map(String::as_str));
            let result = delivered.and(completed);
            self.gp.last = Some(match &result {
                Ok(_) => (true, self.session.project.gp_history.last().map(|h| h.messages.clone()).unwrap_or_default()),
                Err(error) => (false, vec![error.clone()]),
            });
            requested = complete_gp_file(requested, id, &result);
        }
        requested
    }

    /// The RustGIS logo as a texture (rasterised once from `rustgis_render::logo`).
    pub fn logo(&mut self, ctx: &egui::Context) -> Option<egui::TextureId> {
        if self.logo.is_none()
            && let Some(pm) = rustgis_engine::render::logo::pixmap(256)
        {
            let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
            self.logo = Some(ctx.load_texture("rustgis-logo", img, egui::TextureOptions::LINEAR));
        }
        self.logo.as_ref().map(|t| t.id())
    }

    /// Paints the logo into `rect`.
    pub fn paint_logo(&mut self, ui: &egui::Ui, rect: egui::Rect) {
        if let Some(id) = self.logo(ui.ctx()) {
            ui.painter().image(id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
    }

    /// The sample "World Atlas" project.
    pub fn with_sample() -> Self {
        // Tests don't depend on the machine's time zone and locale.
        rustgis_engine::sample::apply_worldview(if cfg!(test) { "default" } else { i18n::AUTO });
        let mut s = Session::new(Project::default());
        let _ = s.run("project.sample", &Value::Null);
        s.mark_saved();
        RustGisApp::new(s)
    }

    /// Applies the worldview preference; an untouched sample project's countries follow it.
    fn apply_worldview(&mut self) {
        use rustgis_engine::sample;
        let before = sample::worldview();
        // Tests don't depend on the machine's time zone and locale.
        let pref = if cfg!(test) && self.ui.worldview == i18n::AUTO { "default" } else { self.ui.worldview.as_str() };
        let after = sample::apply_worldview(pref);
        if after != before {
            sample::swap_countries(&mut self.session, before, after);
            self.map.invalidate();
        }
    }

    /// Reloads available theme files from the user folder, retaining built-ins when it is absent.
    fn load_themes(&mut self) {
        self.load_themes_from(theme::Theme::user_dir().as_deref());
    }

    /// Reads the folder; the stamp is taken first, so a save during the read is picked up next time.
    fn load_themes_from(&mut self, dir: Option<&std::path::Path>) {
        self.themes_stamp = theme::Theme::folder_signature(dir);
        self.themes_revision = self.themes_revision.wrapping_add(1);
        let (themes, errors) = theme::Theme::available_in(dir);
        let notes: Vec<String> = themes.iter().flat_map(|t| t.notes.iter().cloned()).collect();
        // Said before the new lists are kept, so only what the last read did not report is said.
        self.announce_theme_errors(&errors);
        let fresh: Vec<String> = notes.iter().filter(|note| !self.theme_notes.contains(note)).cloned().collect();
        self.announce_theme_notes(&fresh);
        self.themes = themes;
        self.theme_errors = errors;
        self.theme_notes = notes;
        // The choice outlives the file: a theme that stopped loading keeps its name.
        self.sync_named_brightness();
        // Reset so a file that reads differently is applied again on the next frame.
        self.applied_theme = None;
    }

    /// A refused file is said once, with a count; the menu keeps the reason for each.
    fn announce_theme_errors(&mut self, errors: &[String]) {
        let fresh = errors.iter().filter(|e| !self.theme_errors.contains(*e)).count();
        if fresh == 0 {
            return;
        }
        let text = if fresh == 1 {
            tl!("A theme file was refused. Open Settings > Theme for the reason.").to_string()
        } else {
            i18n::fmt("{n} theme files were refused. Open Settings > Theme for the reason.", &[("n", &fresh.to_string())])
        };
        self.session.notify(text, true);
    }

    /// Reports this read's adjustments; folder refreshes filter unchanged notes before calling.
    fn announce_theme_notes(&mut self, notes: &[String]) {
        if !notes.is_empty() {
            let details = notes.iter().take(8).map(String::as_str).collect::<Vec<_>>().join("; ");
            self.session.notify(
                i18n::fmt(
                    "{n} theme adjustment notice(s): {details}. Open Settings > Theme for all details.",
                    &[("n", &notes.len().to_string()), ("details", &details)],
                ),
                false,
            );
        }
    }

    /// A file saved as the other brightness takes the interface with it, so name and look agree.
    fn sync_named_brightness(&mut self) {
        if self.ui.theme_name.is_empty() {
            return;
        }
        if let Some(dark) = self.themes.iter().find(|t| t.name == self.ui.theme_name).map(|t| t.dark) {
            if self.ui.dark != dark {
                self.ui.dark = dark;
                if let Some(save) = &self.services.save_pref {
                    save("dark", if dark { "true" } else { "false" });
                }
            }
            // The loaded theme owns its brightness; following the platform would overwrite it.
            self.ui.dark_follows_system = false;
        }
    }

    /// Reloads when file names, sizes or modification times change, at most once a second.
    /// A save preserving size and timestamp needs Reload Themes.
    fn refresh_themes(&mut self, ctx: &egui::Context) {
        let Some(dir) = theme::Theme::user_dir() else { return };
        let now = ctx.input(|i| i.time);
        if now >= self.themes_checked + 1.0 {
            self.themes_checked = now;
            if theme::Theme::folder_signature(Some(&dir)) != self.themes_stamp {
                self.load_themes();
            }
        }
        // Zero means no readable folder. Without one, let the app sleep until another event.
        if self.themes_stamp != 0 {
            ctx.request_repaint_after(web_time::Duration::from_secs(1));
        }
    }

    /// The theme drawn: the one named while it loads, else the nearest of the brightness in force.
    fn drawn(&self) -> Option<&theme::Theme> {
        self.themes.iter().find(|t| t.name == self.ui.theme_name).or_else(|| self.themes.iter().find(|t| t.dark == self.ui.dark))
    }

    /// The theme in use: the one named, else the first loaded theme of the current brightness,
    /// which can be a user theme, else the built-in of that brightness.
    pub fn current_theme(&self) -> theme::Theme {
        self.drawn().cloned().unwrap_or_else(|| {
            let builtin = theme::Theme::builtin();
            let wanted = builtin.iter().find(|t| t.dark == self.ui.dark).or_else(|| builtin.first());
            wanted.cloned().unwrap_or(theme::Theme {
                name: "Light".to_string(),
                dark: false,
                tokens: theme::Tokens::light(),
                metrics: theme::Metrics::standard(),
                notes: Vec::new(),
            })
        })
    }

    /// A saved theme name wins; one that no longer loads follows the system.
    fn take_saved_look(&mut self) {
        let pref = |app: &Self, key: &str| -> Option<String> {
            app.services.load_pref.as_ref().and_then(|load| load(key)).filter(|v| !v.is_empty())
        };
        let wanted = pref(self, "theme");
        let picked = wanted.as_ref().and_then(|name| self.themes.iter().find(|t| t.name == *name).cloned());
        if let Some(picked) = picked {
            self.ui.theme_name = picked.name;
            self.ui.dark = picked.dark;
            self.ui.dark_follows_system = false;
        } else {
            // A saved theme keeps its name until its file loads, so a fixed file is drawn again.
            self.ui.theme_name = wanted.unwrap_or_default();
            if let Some(dark) = pref(self, "dark") {
                // "auto" was Follow the System; a toggle stores the brightness it left.
                self.ui.dark_follows_system = dark == "auto";
                self.ui.dark = if self.ui.dark_follows_system {
                    pref(self, "dark_fallback").map_or(self.ui.dark, |value| value == "true")
                } else {
                    dark == "true"
                };
            }
        }
        let saved = pref(self, "ui_scale");
        let scale = saved.as_deref().and_then(|v| v.parse::<f32>().ok());
        self.ui.interface_scale = scale.map_or(1.0, theme::clamp_scale);
        if saved.is_some() && scale.is_none_or(|factor| factor != self.ui.interface_scale) {
            i18n::set_current(self.chosen_lang());
            self.session.notify(
                i18n::fmt("Interface scale held to {percent}%", &[("percent", &format!("{:.0}", self.ui.interface_scale * 100.0))]),
                false,
            );
        }
    }

    /// Selects a loaded theme and saves its name and brightness; unknown names leave the choice unchanged.
    fn use_theme(&mut self, name: &str) -> Result<(), String> {
        let Some(picked) = self.themes.iter().find(|t| t.name == name).cloned() else {
            return Err(i18n::fmt("No theme is named {name}.", &[("name", &theme::clip(name))]));
        };
        self.ui.theme_name = picked.name.clone();
        self.ui.dark = picked.dark;
        self.ui.dark_follows_system = false;
        if let Some(save) = &self.services.save_pref {
            save("theme", &picked.name);
            save("dark", if picked.dark { "true" } else { "false" });
        }
        Ok(())
    }

    /// Validates a bounded theme file, preserves its source in the user folder, then selects it.
    fn load_theme_file(&mut self, path: &std::path::Path) -> Result<(), String> {
        let said = |reason: &str| i18n::fmt("{path}: {reason}", &[("path", &path.display().to_string()), ("reason", reason)]);
        let text = match theme::read_text(path) {
            Ok(text) => text,
            Err(theme::Over::TooBig) => return Err(theme::too_big(path)),
            Err(theme::Over::Reason(reason)) => return Err(said(&reason)),
        };
        let stem = path.file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let theme = theme::Theme::parse(&stem, &text).map_err(|e| said(&e))?;
        // Keep the original file text. Remember its notes before reloading so they are announced once.
        let kept = theme.save_user(&text)?;
        self.announce_theme_notes(&theme.notes);
        for note in &theme.notes {
            if !self.theme_notes.contains(note) {
                self.theme_notes.push(note.clone());
            }
        }
        self.load_themes();
        self.use_theme(&theme.name)?;
        let kept = kept.display().to_string();
        let name = theme.name.clone();
        let text = i18n::fmt("Theme {name} loaded and kept at {path}", &[("name", &name), ("path", &kept)]);
        self.session.notify(text, false);
        Ok(())
    }

    /// Sets up the project the app opens with, from the Settings › Start-up preferences; a shell
    /// passes its own overrides (the web app's `?blank`, `?projection=` and `?extent=`). Call it
    /// once, after [`Self::with_services`] and before opening files. The start is not an edit to
    /// undo or save.
    pub fn start(&mut self, overrides: &Start) -> Result<(), String> {
        // Notices written here are shown as written: in the chosen language, not the first
        // frame's default.
        i18n::set_current(self.chosen_lang());
        // A shell's `blank` wins (a file on the command line, `--sample`, the web app's `?blank`).
        let opened = match (overrides.blank, self.ui.start_with.clone()) {
            (Some(true), _) | (None, StartWith::Blank) => {
                self.run("project.new", Value::Null)?;
                false
            }
            (Some(false), _) | (None, StartWith::Sample) => false,
            // Only when there is a project to skip: Last Project with none yet starts the sample anyway.
            (None, StartWith::Last | StartWith::File(_))
                if overrides.after_crash && (self.ui.last_project.is_some() || self.ui.start_with != StartWith::Last) =>
            {
                let text = i18n::fmt(
                    "RustGIS didn't close properly last time, so it started with the World Atlas instead of your start-up project. Open the project from the Project menu when you're ready.",
                    &[],
                );
                self.session.notify(text, true);
                false
            }
            (None, StartWith::Last) => match self.ui.last_project.clone() {
                Some(path) => self.open_start_project(&path),
                None => false,
            },
            (None, StartWith::File(path)) => self.open_start_project(&path),
        };
        // A project opened at start keeps its own projection and view: the Start-up projection and
        // extent are for the sample and a blank map. A shell's own still apply.
        let code = overrides.projection.clone().unwrap_or_else(|| if opened { String::new() } else { self.ui.start_projection.clone() });
        let mut result = Ok(());
        let mut projected = false;
        if !code.trim().is_empty() {
            match self.run("map.projection", serde_json::json!({"type": code})) {
                Ok(_) => projected = true,
                Err(e) => result = Err(e),
            }
        }
        let extent = match &overrides.extent {
            Some(text) => match parse_extent(text) {
                Some(e) => Some(e),
                None => {
                    result = result.and(Err(format!("extent '{text}' is minLon,minLat,maxLon,maxLat")));
                    self.ui.start_extent
                }
            },
            None if opened => None,
            None => self.ui.start_extent,
        };
        if let Some(e) = extent {
            let _ = self.run("map.zoomToExtent", serde_json::json!({"extent": e}));
            self.ui.start_view = Some(e);
        } else if projected {
            let _ = self.run("map.zoomFull", Value::Null);
        }
        self.session.clear_history();
        self.map.invalidate();
        result
    }

    /// Opens the Start With project. One that doesn't open (moved, deleted, unreadable) leaves
    /// what the app started with and says why; the next start tries again.
    fn open_start_project(&mut self, path: &str) -> bool {
        // A preference edited by hand can name anything: only a project file is opened.
        if !rustgis_engine::project::is_project_file(path) {
            let text = i18n::fmt("The start-up project {path} isn't a RustGIS project file.", &[("path", path)]);
            self.session.notify(text, true);
            return false;
        }
        match self.run("project.open", serde_json::json!({"path": path})) {
            Ok(_) => true,
            Err(e) => {
                let text = i18n::fmt("The start-up project {path} didn't open: {error}", &[("path", path), ("error", &e)]);
                self.session.notify(text, true);
                false
            }
        }
    }

    /// A clean exit (the desktop app's `on_exit`): the recovery copy of this run's unsaved work
    /// is removed, as recovery is for a run that didn't close. Call it once, as the app closes.
    pub fn end_autosave(&mut self) {
        autosave::end(self);
    }

    /// The app closing (the desktop app's `on_exit`): an export still writing is cancelled and
    /// given a moment to stop, so it leaves no temporary file beside its destination.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn end_exports(&mut self) {
        exportjob::stop(self, std::time::Duration::from_secs(5));
        readjob::stop(self, std::time::Duration::from_secs(5));
    }

    /// Whether this shell opens files by path (the desktop app; the web app picks them in the
    /// browser instead).
    pub(crate) fn opens_files(&self) -> bool {
        !cfg!(target_arch = "wasm32") && self.services.open_async.is_none()
    }

    /// Settings › Start-up › Last Project: once the open project's file changes, whoever opened
    /// or saved it (the user, the assistant, an attached MCP client), it's the last project, by
    /// its full path (a relative one would depend on where the next start runs from).
    fn note_last_project(&mut self) {
        if self.session.path == self.seen_project {
            return;
        }
        self.seen_project = self.session.path.clone();
        let Some(p) = self.session.path.as_ref().filter(|_| self.opens_files()) else { return };
        let path = std::path::absolute(p).unwrap_or_else(|_| p.clone()).display().to_string();
        if !rustgis_engine::project::is_project_file(&path) || self.ui.last_project.as_deref() == Some(path.as_str()) {
            return;
        }
        if let Some(save) = &self.services.save_pref {
            save("last_project", &path);
        }
        self.ui.last_project = Some(path);
    }

    /// Runs a command the user asked for in the interface (a button, a menu, a shortcut, Command
    /// Search, a file dropped on the window): one that would replace a project with unsaved
    /// changes asks to save them first, and runs once answered. Scripts, the AI Assistant and
    /// attached agents run commands without asking ([`Self::run`]).
    pub fn run_by_user(&mut self, id: &str, params: Value) -> Result<Value, String> {
        if saveprompt::asks(self, id) {
            saveprompt::ask(self, saveprompt::Discard::Replace(id.to_string(), params));
            return Ok(Value::Null);
        }
        // A plugin's may wait for a server: the window says so first (#1319).
        if id.starts_with(rustgis_engine::plugins::PREFIX) {
            let notice_mark = self.session.notice_mark();
            pluginstart::ask(self, id, params).inspect_err(|e| self.command_failed(id, e, notice_mark))?;
            return Ok(Value::Null);
        }
        self.run(id, params)
    }

    /// Imports a peer's project without going through User command dispatch or restore memory.
    pub fn open_peer_text(&mut self, text: &str) -> Result<(), String> {
        let report = self.session.open_peer_text(rustgis_engine::Peer::Embedder, text)?;
        assistantpane::peer_replaced(self);
        self.ui.web_file = None;
        self.map.deleting = None;
        self.retire_cog_probes();
        if let Some(steps) = self.ws.recording.take() {
            self.session.notify(
                crate::i18n::fmt(
                    "Recording stopped: another project opened, and its {n} steps were for the previous one.",
                    &[("n", &steps.len().to_string())],
                ),
                true,
            );
        }
        layoutview::follow_project(self);
        for notice in report.notices() {
            self.session.notify(notice, false);
        }
        Ok(())
    }

    /// Reports pending header reads retired by either ordinary or peer project replacement.
    fn retire_cog_probes(&mut self) {
        for (p, _, _) in std::mem::take(&mut self.ui.cog_probes) {
            let url = p.get("url").and_then(Value::as_str).unwrap_or("");
            let url = serviceui::address_in_message(rustgis_engine::data::s3_address::inner(url));
            let why = i18n::fmt("{url}: its project was closed while it was read, so it wasn't added", &[("url", &url)]);
            self.landed_in_dialog(&p, Err(&why));
            self.session.notify(why, false);
        }
    }

    /// Runs a command by id: `ui.*` here, everything else in the engine.
    pub fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        self.run_with_prepared(id, params, AppDispatch::Direct)
    }

    /// Reports failures before engine dispatch as well as execution, without repeating its notice.
    fn command_failed(&mut self, id: &str, error: &str, notice_mark: u64) {
        let text = rustgis_engine::Session::redacted(error);
        log::info!("{id}: {}", text.as_str());
        if !self.session.notices_since(notice_mark).iter().any(|notice| notice.error && notice.text == text.as_str()) {
            self.session.notify_redacted(text, true);
        }
    }

    /// Runs an engine-admitted delayed command with the app's usual delivery and recording.
    fn run_prepared(&mut self, id: &str, params: Value, prepared: rustgis_engine::policy::PreparedCommand) -> Result<Value, String> {
        self.run_with_prepared(id, params, AppDispatch::Prepared(Box::new(prepared)))
    }

    /// Lands an admitted read through the same notices, recording and preferences as a command.
    #[cfg(not(target_arch = "wasm32"))]
    fn run_read(&mut self, id: &str, params: Value, read: rustgis_engine::cmd::reads::Read) -> Result<Value, String> {
        self.run_with_prepared(id, params, AppDispatch::Read(Box::new(read)))
    }

    /// Keeps direct, prepared and resolved-service dispatch within the same UI lifecycle.
    fn run_with_prepared(&mut self, id: &str, params: Value, dispatch: AppDispatch) -> Result<Value, String> {
        let _diagnostics = self.session.diagnostics.enter();
        if !matches!(id, "ui.menu" | "ui.ribbon" | "ui.language" | "ui.worldview") {
            self.ui.ribbon_peek = false;
        }
        // Read before the command runs: removing or renaming the active layer changes it.
        let active = if self.ws.recording.is_some() { self.layer_acted_on(id, &params) } else { None };
        // A step being edited is known by its place in the task: Undo or Redo of a move or a
        // deletion would leave that place naming another step.
        // What is compared: the task's length and the step at the edit's place (its words and
        // command, not its parameters, which may be large).
        let task_steps = |app: &Self| {
            let task = app.session.project.tasks.iter().find(|t| t.name == app.ws.task)?;
            let at = app.ws.task_edit.as_ref().and_then(|e| task.steps.get(e.0));
            Some((task.steps.len(), at.map(|s| (s.title.clone(), s.instructions.clone(), s.command.clone()))))
        };
        let steps_before = (matches!(id, "edit.undo" | "edit.redo") && self.ws.task_edit.is_some()).then(|| task_steps(self)).flatten();
        // Element ids are per layout: a selection made in one layout must not reach an element
        // of another, so it ends when the open layout changes (or layouts come and go).
        let open_layout = |app: &Self| (app.session.active_layout, app.session.project.layouts.len());
        let layout_before = open_layout(self);
        let notice_mark = self.session.notice_mark();
        // A plugin's command that adds data by address doesn't freeze the app: the download is
        // left for the frame loop (`ui.addUrl`), as in the browser.
        let in_place = matches!(&dispatch, AppDispatch::InPlace(_));
        let resolved_lineage = match &dispatch {
            AppDispatch::ResolvedTiles { crossed_origin, .. } => Some(*crossed_origin),
            _ => None,
        };
        let mut r = match dispatch {
            AppDispatch::Prepared(prepared) => self.session.run_prepared(*prepared),
            AppDispatch::InPlace(id) => self.session.add_in_place_for(id, &params),
            AppDispatch::Download(id) => self.session.complete_download(id, &params),
            #[cfg(not(target_arch = "wasm32"))]
            AppDispatch::Read(read) => rustgis_engine::cmd::reads::read_land(&mut self.session, *read),
            AppDispatch::ResolvedTiles { template, crossed_origin } => {
                rustgis_engine::cmd::service::add_resolved_tiles(&mut self.session, &params, &template, crossed_origin)
            }
            AppDispatch::Direct if id.starts_with("ui.") => self.run_ui(id, &params),
            AppDispatch::Direct if id.starts_with(rustgis_engine::plugins::PREFIX) => self.session.run_leaving_downloads(id, &params),
            AppDispatch::Direct => self.session.run(id, &params),
        };
        r = self.deliver_gp_files(r);
        if r.is_ok() && id == "tool.set" {
            self.map.cancel_sketch();
        }
        // Nested commands (including scripts) can replace rows too; never retarget a chosen
        // vertex or held gesture to the replacement feature at the same row.
        crate::vertexedit::cancel_stale_drag(self);
        if let Err(e) = &r {
            self.command_failed(id, e, notice_mark);
        }
        // Commands can replace the project, switch maps or change the renderer while the pane
        // is closed. A field picked for the old context must not overwrite the new renderer.
        if self.ui.symbology_pending.as_ref().is_some_and(|pending| !pending.is_current(&self.session)) {
            self.ui.symbology_pending = None;
        }
        // A project replacing this one hasn't been saved in the browser yet (opening a file
        // names it again).
        if r.is_ok() && matches!(id, "project.new" | "project.sample" | "project.open" | "project.openText" | "project.fromTemplate") {
            self.ui.web_file = None;
            // A deletion asked for in the old project never reaches a namesake in the new one.
            self.map.deleting = None;
            // A recording's steps name the old project's layers: it ends here, and says so.
            if let Some(steps) = self.ws.recording.take() {
                let n = steps.len().to_string();
                let text = crate::i18n::fmt(
                    "Recording stopped: another project opened, and its {n} steps were for the previous one.",
                    &[("n", &n)],
                );
                self.session.notify(text, true);
            }
            // A probe still under way is for an address added to the old project: dropped, so
            // its layer never lands in the new one, and said, as a download landing late is. A plugin
            // command waiting for it went with the old project's waits.
            self.retire_cog_probes();
        }
        // Fields and layers can go while the table is closed: its column views follow them.
        table::prune_views(self);
        // Undo and redo can move vertices: an index chosen before may now name another vertex.
        // A step moved or deleted, from the pane, Command Search, the assistant or a script: the
        // step being edited (and the current one) follow it, so Save writes where it was typed.
        if r.is_ok() && matches!(id, "task.moveStep" | "task.removeStep") {
            let tasks = &self.session.project.tasks;
            // The task it acted on, as the command reads it: named, else the last one.
            let acted = match params.get("task").and_then(Value::as_str) {
                Some(n) => tasks.iter().find(|t| t.name.eq_ignore_ascii_case(n)),
                None => tasks.last(),
            };
            let index = |k: &str| params.get(k).and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok());
            if acted.is_some_and(|t| t.name == self.ws.task)
                && let Some(step) = index("step")
            {
                match index("to") {
                    Some(to) if id == "task.moveStep" => workspacepanes::follow_move(self, step, to),
                    _ if id == "task.removeStep" => workspacepanes::follow_delete(self, step),
                    _ => {}
                }
            }
        }
        if open_layout(self) != layout_before {
            self.layout.selected = None;
        }
        // A project this command opened: a layout element's selection and the text typed into its
        // fields stay with the old one.
        layoutview::follow_project(self);
        if r.is_ok() && matches!(id, "edit.undo" | "edit.redo") {
            crate::vertexedit::choose(self, None);
            if let Some(before) = steps_before
                && task_steps(self).is_none_or(|now| now != before)
            {
                self.ws.task_edit = None;
                self.session.notify(tl!("The task's steps changed, so the step being edited was closed: edit it again."), false);
            }
            let n = self.session.project.tasks.iter().find(|t| t.name == self.ws.task).map_or(0, |t| t.steps.len());
            self.ws.task_step = self.ws.task_step.min(n.saturating_sub(1));
        }
        // Choosing a tool, Text included, drops a value picked under Dynamic Text
        // (`ui.dynamicText` picks it after choosing the tool).
        if r.is_ok() && id == "tool.set" {
            self.ui.dynamic_text = None;
        }
        if r.is_ok() {
            if resolved_lineage == Some(true) {
                if self.ws.recording.is_some() {
                    self.session.notify(
                        tl!(
                            "This service layer isn't recorded: its tile discovery crossed origins, and a task could use a different login."
                        ),
                        false,
                    );
                }
            } else if resolved_lineage == Some(false) {
                // A saved connection can replay its same-origin discovery with its current login;
                // runtime bearer wrappers themselves must never become recorded parameters.
                let mut replay = params.clone();
                if params.get("connection").and_then(Value::as_str).is_some_and(|id| !id.is_empty()) {
                    for key in ["url", "source_url", "tiles"] {
                        if let Some(url) = params.get(key).and_then(Value::as_str) {
                            replay[key] = json!(rustgis_engine::render::rustgis_tiles::logins::inner(url));
                        }
                    }
                }
                self.record_step(id, &replay, active);
            } else if in_place {
                // The request number correlates this delivery; replay starts a fresh request.
                let mut replay = params.clone();
                if let Some(object) = replay.as_object_mut() {
                    object.remove("request");
                }
                self.record_step(id, &replay, active);
            } else {
                self.record_step(id, &params, active);
            }
        }
        self.note_last_project();
        // Sources added, saved or made favourites are kept with the preferences.
        sourcespane::save(self);
        sourcespane::save_connections(self);
        self.save_raster_defaults();
        self.save_layout_favourite();
        r
    }

    /// The name of the active layer, when command `id` would act on it for want of a `layer`
    /// (an `enabled` predicate gets the session shared, `&Session`, so asking twice changes
    /// nothing; what it says when disabled doesn't matter here):
    /// it takes a `layer` and needs an active one to be enabled, or is one of those that
    /// default to it while enabled without one. Elsewhere `layer` names something else (a file's
    /// layer for Add Data, a service's), and a layer the command names is its own.
    /// Also whether another layer has that name, so the step would replay on the first of them.
    fn layer_acted_on(&mut self, id: &str, params: &Value) -> Option<(String, bool)> {
        // Terrain with nothing named, while none is on, takes the active raster: named, so the
        // step replays on that DEM (a change of exaggeration on a terrain already on names none).
        if id == "map.terrain" {
            let named = ["layer", "tiles"].iter().any(|k| params.get(*k).is_some_and(|v| !v.is_null()))
                || params.get("off").and_then(serde_json::Value::as_bool) == Some(true);
            let map = self.session.map();
            if named || map.view.terrain.is_some() {
                return None;
            }
            let name = self.session.active_layer.and_then(|l| map.layer(l)).filter(|l| l.raster.is_some()).map(|l| l.name.clone())?;
            let shared = map.layers.iter().filter(|l| l.name.eq_ignore_ascii_case(&name)).count() > 1;
            return Some((name, shared));
        }
        let spec = self.session.registry.get(id).filter(|c| c.mutates && params.get("layer").is_none_or(Value::is_null)).copied()?;
        if !spec.param_keys().is_some_and(|k| k.contains(&"layer")) {
            return None;
        }
        let defaults = DEFAULTS_TO_ACTIVE_LAYER.contains(&id);
        let active = self.session.active_layer.take();
        let needs = (spec.enabled)(&self.session).is_some();
        self.session.active_layer = active;
        if !(defaults || needs && (spec.enabled)(&self.session).is_none()) {
            return None;
        }
        let map = self.session.map();
        let name = active.and_then(|l| map.layer(l)).map(|l| l.name.clone())?;
        let shared = map.layers.iter().filter(|l| l.name.eq_ignore_ascii_case(&name)).count() > 1;
        Some((name, shared))
    }

    /// Applies the engine-held background tool owner, then records the completed operation.
    pub(crate) fn finish_gp(&mut self, id: u64) -> rustgis_engine::CmdResult {
        let result = self.session.finish_gp(id);
        if result.is_ok() {
            self.record_step("gp.finish", &json!({"id":id}), None);
        }
        result
    }

    /// Applies a workflow's admitted raster completion and records its replayable command.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn finish_raster(&mut self, command: &str, id: u64, params: &Value) -> rustgis_engine::CmdResult {
        let result = self.session.finish_raster(command, id, params);
        if result.is_ok() {
            let mut recorded = params.clone();
            recorded["ran"] = json!(id);
            self.record_step(command, &recorded, None);
        }
        result
    }

    /// Record Task: adds a command that ran from the app and changed the project as a step. Not
    /// panes and dialogs (`ui.*`), tasks themselves, or what only moves the view. A colour, outline
    /// or transparency set again on the same layer (dragged in its picker) updates the step it just
    /// made.
    /// `active` is the active layer's name from [`Self::layer_acted_on`], read before it ran.
    fn record_step(&mut self, id: &str, params: &Value, active: Option<(String, bool)>) {
        if self.ws.recording.is_none() {
            return;
        }
        // A tool run in the background is applied by `gp.finish`, whose run id means nothing to a
        // replay: the step is the run itself, `gp.run` with its tool and parameters, from the
        // history entry it has just added.
        let finished;
        let (id, params) = if id == "gp.finish" {
            let Some(h) = self.session.project.gp_history.last().filter(|h| !h.id.is_empty()) else { return };
            finished = json!({"tool": h.id, "params": h.params});
            ("gp.run", &finished)
        } else if rustgis_engine::cmd::workflows::runs_in_background(id).is_some() && params.get("ran").is_some() {
            // A raster an Analysis workflow made on a thread (`ran`): the step is the command
            // itself, which a replay runs again from its parameters.
            let mut again = params.clone();
            if let Some(o) = again.as_object_mut() {
                o.remove("ran");
            }
            finished = again;
            (id, &finished)
        } else if id == "layer.addDownload" && params.get("window").is_some_and(Value::is_object) {
            // A window read on a thread: the step is the command that reads it, which a replay
            // runs again from its address.
            let mut again = params.clone();
            if let Some(o) = again.as_object_mut() {
                let window = o.remove("window");
                o.remove("id");
                o.remove("request");
                if let Some(Value::Object(window)) = window {
                    o.extend(window);
                }
            }
            finished = again;
            ("layer.addUrl", &finished)
        } else {
            (id, params)
        };
        let Some(steps) = self.ws.recording.as_mut() else { return };
        // Undo doesn't take back a recorded step (the undo history and the recording don't line
        // up step for step): say how to.
        if id == "edit.undo" && !steps.is_empty() {
            if !self.ws.recording_undo_said {
                self.ws.recording_undo_said = true;
                self.session.notify(tl!("Undo doesn't remove the recorded step: delete it in Edit Steps once you stop recording."), false);
            }
            return;
        }
        // Not steps of an exercise: panes, tasks, opening or saving the project, undo and redo,
        // and picking the layer to work on (a click in the Layers pane).
        if id.starts_with("ui.")
            || id.starts_with("task.")
            || id.starts_with("project.")
            || matches!(id, "edit.undo" | "edit.redo" | "layer.select")
        {
            return;
        }
        let Some(spec) = self.session.registry.get(id).filter(|c| c.mutates).copied() else { return };
        // Downloaded data is kept for this session only, and adding it uses it up: a task
        // couldn't add it again.
        // A result handed over by the app (`ran`: a summary worked out on a thread) is used up
        // the same way.
        if id == "layer.addDownload" || (matches!(id, "layer.addZarr" | "cube.trend") && params.get("ran").is_some_and(|r| !r.is_null())) {
            let text = crate::i18n::fmt(
                "{command} isn't recorded: the downloaded data is kept for this session only, so a task couldn't add it again.",
                &[("command", tl_raw!(spec.label))],
            );
            self.session.notify(text, false);
            return;
        }
        // A task is saved with the project and shared as a file: a command holding a password,
        // key or token (a database connection, a signed address) isn't recorded.
        if rustgis_engine::sources::holds_secret(params) {
            let text = crate::i18n::fmt(
                "{command} isn't recorded: its parameters may hold a password, key or token (one is named like it, an address carries one, or they nest too deep to check), and a task would keep it.",
                &[("command", tl_raw!(spec.label))],
            );
            self.session.notify(text, false);
            return;
        }
        // A task keeps its steps' parameters in the project: one carrying megabytes (a file's
        // contents added from the app) would swell every save of it.
        if serde_json::to_vec(params).map_or(0, |b| b.len()) > MAX_STEP_BYTES {
            let text = crate::i18n::fmt(
                "{command} isn't recorded: its parameters are over 4 MB, too much for a task to keep. Add that data to the project before recording.",
                &[("command", tl_raw!(spec.label))],
            );
            self.session.notify(text, false);
            return;
        }
        // A plain `key` is kept (a field or a lookup key, as often as not), but it might be an
        // access key: said once a recording.
        if rustgis_engine::sources::holds_plain_key(params) && !self.ws.recording_key_said {
            self.ws.recording_key_said = true;
            let text = crate::i18n::fmt(
                "{command}'s step keeps its key parameter. If that is an access key, delete the step (Edit Steps) before sharing the task.",
                &[("command", tl_raw!(spec.label))],
            );
            self.session.notify(text, false);
        }
        // The ribbon, the symbol editor and the panes run a command on the active layer without
        // naming it: the step names it (`active`, from `layer_acted_on`), so it replays on that
        // layer, not on whichever is active then, and a colour dragged merges into one step.
        let mut params = params.clone();
        if let Some((name, shared)) = active {
            // Steps name layers, and a name two layers share replays on the first: say so, once
            // a recording for each name.
            if shared && !self.ws.recording_shared_said.contains(&name) {
                let text = crate::i18n::fmt(
                    "Two layers are named {name}: the step names it, so it replays on the first of them. Rename one to tell them apart.",
                    &[("name", &name)],
                );
                self.session.notify(text, false);
                self.ws.recording_shared_said.push(name.clone());
            }
            if params.is_null() {
                params = json!({});
            }
            if let Some(object) = params.as_object_mut() {
                object.insert("layer".into(), json!(name));
            }
        }
        let params = &params;
        // Only a setting adjusted again and again on one layer (a colour dragged in its picker)
        // updates its step; anything else, a second selection on the same layer included, is a
        // step of its own.
        if let Some(last) = steps.last_mut()
            && matches!(id, "layer.color" | "layer.outline" | "layer.lineStrokes" | "layer.fillLayers" | "layer.transparency")
            && last.command == id
            && params.get("layer").is_some()
            && last.params.get("layer") == params.get("layer")
            && ["value", "index", "other"].iter().all(|k| last.params.get(*k) == params.get(*k))
        {
            last.params = params.clone();
            return;
        }
        // A tool's step is titled with the tool's name ("Buffer"), not "Run Tool".
        let title = match params.get("tool").and_then(Value::as_str).and_then(rustgis_engine::analysis::tool) {
            Some(tool) if id == "gp.run" => tl!(tool.name).to_string(),
            _ => tl!(spec.label).to_string(),
        };
        if steps.len() >= 200 {
            // A task holds at most 200 steps: say so once, when the first one is left out.
            if !self.ws.recording_full {
                self.ws.recording_full = true;
                self.session
                    .notify(tl!("A task holds at most 200 steps: stop recording to make it, then record the rest as another task."), true);
            }
            return;
        }
        steps.push(rustgis_engine::project::TaskStep {
            title,
            instructions: String::new(),
            command: id.to_string(),
            params: params.clone(),
        });
    }

    /// Saves the favourite arrangement of publication layouts when `layout.arrangement` changed it.
    fn save_layout_favourite(&mut self) {
        if self.session.layout_favourite == self.ui.layout_favourite_saved {
            return;
        }
        self.ui.layout_favourite_saved = self.session.layout_favourite.clone();
        if let Some(save) = &self.services.save_pref {
            save(LAYOUT_FAVOURITE_PREF, &self.session.layout_favourite.to_string());
        }
    }

    /// Keeps the default raster style with the preferences when it changed.
    fn save_raster_defaults(&mut self) {
        if self.session.raster_defaults == self.ui.raster_defaults_saved {
            return;
        }
        self.ui.raster_defaults_saved = self.session.raster_defaults.clone();
        if let (Some(save), Ok(text)) = (&self.services.save_pref, serde_json::to_string(&self.session.raster_defaults)) {
            save(RASTER_DEFAULTS_PREF, &text);
        }
    }

    /// Runs a `ui.*` command: panes, dialogs and the app's own settings.
    fn run_ui(&mut self, id: &str, p: &Value) -> Result<Value, String> {
        // A pane's command toggles it; `{"close": true}` closes it, in view or not (a tab's ✕),
        // whatever else the command is given (SQL to run, a question to ask).
        if p.get("close").and_then(Value::as_bool) == Some(true) && panes::close_pane_of(self, id) {
            return Ok(Value::Null);
        }
        let toggle_right = |ui: &mut UiState, pane: RightPane| ui.toggle_right(pane);
        match id {
            "ui.reloadData" => {
                #[cfg(not(target_arch = "wasm32"))]
                return reloaddata::request(self, p);
                #[cfg(target_arch = "wasm32")]
                return Err(tl!("Reload Data from local files requires the desktop app.").into());
            }
            "ui.pane.contents" => self.ui.left = if self.ui.left == Some(LeftPane::Contents) { None } else { Some(LeftPane::Contents) },
            "ui.pane.catalog" => self.ui.left = if self.ui.left == Some(LeftPane::Catalog) { None } else { Some(LeftPane::Catalog) },
            "ui.pane.sources" => self.ui.left = if self.ui.left == Some(LeftPane::Sources) { None } else { Some(LeftPane::Sources) },
            "ui.pane.geoprocessing" => toggle_right(&mut self.ui, RightPane::Geoprocessing),
            "ui.pane.symbology" => {
                // Showing its tab already open needs no layer.
                if self.session.active_layer.is_none() && !self.ui.right_open.contains(&RightPane::Symbology) {
                    return Err("select a layer in the Layers pane first".into());
                }
                toggle_right(&mut self.ui, RightPane::Symbology)
            }
            "ui.pane.history" => toggle_right(&mut self.ui, RightPane::History),
            "ui.pane.details" => toggle_right(&mut self.ui, RightPane::Details),
            "ui.pane.editing" => toggle_right(&mut self.ui, RightPane::Editing),
            "ui.pane.design" => toggle_right(&mut self.ui, RightPane::Design),
            "ui.pane.script" => toggle_right(&mut self.ui, RightPane::Script),
            // The Directions pane; with "from"/"to" (and "via", "mode"), opened on that route.
            "ui.pane.directions" if p.get("from").is_some() || p.get("to").is_some() => navui::open_with(self, p),
            "ui.pane.directions" => toggle_right(&mut self.ui, RightPane::Directions),
            "ui.pane.webData" => toggle_right(&mut self.ui, RightPane::WebData),
            "ui.pane.bookmarks" => toggle_right(&mut self.ui, RightPane::Bookmarks),
            "ui.pane.format" => toggle_right(&mut self.ui, RightPane::Format),
            // The Labeling pane, on `{"layer": id}` when given (a layer's right-click menu).
            "ui.pane.labeling" => {
                // The layer given, else the selected one; checked before it is selected, so a
                // refusal leaves the selection as it was.
                let given = match p.get("layer").filter(|l| !l.is_null()) {
                    Some(_) => Some(self.session.layer_id(p)?),
                    None => None,
                };
                // Showing its tab already open needs no layer.
                if given.is_some() || !self.ui.right_open.contains(&RightPane::Labeling) {
                    match given.or(self.session.active_layer).and_then(|id| self.session.map().layer(id)) {
                        None => return Err("select a layer in the Layers pane first".into()),
                        Some(l) if l.is_raster() => return Err("labels are drawn for feature layers, not rasters".into()),
                        Some(_) => {}
                    }
                }
                if let Some(id) = given {
                    self.session.run("layer.select", &json!({"layer": id}))?;
                }
                // A layer's menu asks for that layer's labels: shown, even when already in view.
                if given.is_some() { self.ui.open_right(RightPane::Labeling) } else { toggle_right(&mut self.ui, RightPane::Labeling) }
            }
            // A plugin's panel: `{"id": plugin}`.
            "ui.pane.plugin" => return pluginpanel::open(self, p),
            // The SQL pane; with "sql" (and "database"), opened on that query, run with "run".
            "ui.pane.sql" => match p.get("sql").and_then(Value::as_str) {
                Some(sql) => {
                    self.ui.open_right(RightPane::Sql);
                    self.ui.sql.text = sql.chars().take(12_000).collect();
                    if let Some(d) = p.get("database").and_then(Value::as_str) {
                        self.ui.sql.database = d.to_string();
                    }
                    if p.get("run").and_then(Value::as_bool) == Some(true) {
                        sqlpane::run_now(self);
                    }
                }
                None => toggle_right(&mut self.ui, RightPane::Sql),
            },
            // Opens the Workflows pane at a section (staying open when switching sections).
            "ui.pane.workflows" => {
                let section = p.get("section").and_then(Value::as_str);
                // A map pick started in another section (or before the pane closes) ends here.
                self.workflow.pick = None;
                match section.and_then(|s| analysispane::SECTIONS.iter().find(|x| x.0 == s)) {
                    Some((id, _)) => {
                        self.workflow.section = id;
                        self.workflow.result = None;
                        self.workflow.error = None;
                        self.ui.open_right(RightPane::Workflows);
                    }
                    None => toggle_right(&mut self.ui, RightPane::Workflows),
                }
            }
            "ui.gp.open" => {
                let tool = p.get("tool").and_then(Value::as_str).and_then(rustgis_engine::analysis::tool);
                self.gp.open(tool.map(|t| t.id), &self.session);
                // The values to start from, by parameter name (a run opened again from the History
                // pane, #539): over the form's own.
                if let Some(given) = p.get("values").and_then(Value::as_object) {
                    self.gp.fill(given);
                }
                // A standalone table to run the tool on, by its id.
                if let Some(id) = p.get("table").and_then(Value::as_u64).and_then(|id| u32::try_from(id).ok()) {
                    self.gp.pick_table(id, &self.session);
                }
                self.ui.open_right(RightPane::Geoprocessing);
            }
            "ui.language" => {
                let code = p.get("code").and_then(Value::as_str).unwrap_or(i18n::AUTO);
                if code != i18n::AUTO && i18n::Lang::from_code(code).is_none() {
                    return Err(format!("unknown language '{code}'"));
                }
                self.ui.language = code.to_ascii_lowercase();
                self.fonts_failed.clear();
                if let Some(save) = &self.services.save_pref {
                    save("language", &self.ui.language);
                }
                self.map.invalidate();
            }
            "ui.worldview" => {
                let code = p.get("worldview").and_then(Value::as_str).unwrap_or(i18n::AUTO).to_ascii_lowercase();
                if code != i18n::AUTO && rustgis_engine::sample::Worldview::from_code(&code).is_none() {
                    return Err(format!("unknown worldview '{code}'"));
                }
                self.ui.worldview = code;
                if let Some(save) = &self.services.save_pref {
                    save("worldview", &self.ui.worldview);
                }
                self.apply_worldview();
            }
            "ui.autosave" => return autosave::command(self, p),
            "ui.threads" => return threadsui::command(self, p),
            // Adds a file as a drop or Add Data does: one holding several layers asks which (#934).
            "ui.addFile" => {
                let path = p.get("path").and_then(Value::as_str).map(str::trim).filter(|p| !p.is_empty()).ok_or("path is required")?;
                return layerchooser::add_file(self, path);
            }
            // Whether a file holding several layers asks which to add.
            "ui.askLayers" => {
                // Without `on` it toggles; a value that isn't true or false is refused.
                self.ui.ask_layers = match p.get("on") {
                    None | Some(Value::Null) => !self.ui.ask_layers,
                    Some(v) => v.as_bool().ok_or("on must be true or false")?,
                };
                if let Some(save) = &self.services.save_pref {
                    save("ask_file_layers", if self.ui.ask_layers { "on" } else { "off" });
                }
                return Ok(json!({"on": self.ui.ask_layers}));
            }
            "ui.recover" => return autosave::recover(self, p),
            "ui.startup" => {
                // `project` (#377) or, as before, `blank`; `project` wins when both are given.
                let start_with = match p.get("project") {
                    None => p.get("blank").and_then(Value::as_bool).map(|b| if b { StartWith::Blank } else { StartWith::Sample }),
                    Some(v) => {
                        // A shell without file paths refuses a file before looking at one.
                        if (v.as_str() == Some("last") || v.is_object()) && !self.opens_files() {
                            return Err("starting with a project file needs the desktop app".into());
                        }
                        Some(StartWith::from_json(v)?)
                    }
                };
                // Validate before changing anything; `mercator` is stored as the default.
                let projection = match p.get("projection").and_then(Value::as_str) {
                    Some(code) => Some(rustgis_engine::map::Projection::from_code(code)?),
                    None => None,
                };
                // `"extent": "view"` takes the map's view, `null` the full extent.
                let extent = match p.get("extent") {
                    None => None,
                    Some(Value::Null) => Some(None),
                    Some(Value::String(v)) if v == "view" => {
                        let r = self.session.run("map.extent", &Value::Null)?;
                        Some(Some(rustgis_engine::cmd::navigate::lonlat_extent(r.get("extent")).ok_or("the view has no extent")?))
                    }
                    Some(v) => Some(Some(
                        rustgis_engine::cmd::navigate::lonlat_extent(Some(v))
                            .ok_or("extent is [minLon, minLat, maxLon, maxLat], \"view\" or null")?,
                    )),
                };
                if let Some(e) = extent {
                    self.ui.start_extent = e.map(|e| [e.min_x, e.min_y, e.max_x, e.max_y]);
                    if let Some(save) = &self.services.save_pref {
                        let text = self.ui.start_extent.map(|e| e.map(|x| format!("{x:.6}")).join(",")).unwrap_or_default();
                        save("start_extent", &text);
                    }
                }
                if let Some(w) = start_with {
                    if let Some(save) = &self.services.save_pref {
                        save("start_project", &w.pref());
                    }
                    self.ui.start_with = w;
                }
                if let Some(proj) = projection {
                    self.ui.start_projection =
                        if proj == rustgis_engine::map::Projection::WebMercator { String::new() } else { proj.code() };
                    if let Some(save) = &self.services.save_pref {
                        save("start_projection", &self.ui.start_projection);
                    }
                }
                return Ok(serde_json::json!({
                    "blank": self.ui.start_with == StartWith::Blank,
                    "project": self.ui.start_with.json(),
                    "lastProject": self.ui.last_project,
                    "projection": if self.ui.start_projection.is_empty() { "mercator" } else { self.ui.start_projection.as_str() },
                    "extent": self.ui.start_extent,
                }));
            }
            "ui.ribbon" => {
                self.ui.ribbon_minimized = p.get("minimized").and_then(Value::as_bool).unwrap_or(!self.ui.ribbon_minimized);
                self.ui.ribbon_peek = false;
                if let Some(save) = &self.services.save_pref {
                    save("ribbon_minimized", if self.ui.ribbon_minimized { "true" } else { "false" });
                }
            }
            // The theme list in Settings is the one control for brightness; this picks its entry.
            "ui.theme" => {
                if !p.is_null() && !p.as_object().is_some_and(|params| params.keys().all(|key| key == "dark")) {
                    return Err(i18n::fmt("ui.theme accepts only the dark parameter", &[]));
                }
                let dark = match p.get("dark") {
                    None => !self.ui.dark,
                    Some(Value::Bool(dark)) => *dark,
                    Some(_) => return Err(i18n::fmt("ui.theme needs true or false at dark", &[])),
                };
                // The brightness asked for wins: a user file may rework Light or Dark.
                self.ui.dark = dark;
                let wanted = if dark { "Dark" } else { "Light" };
                // The name is the theme that will be drawn, so the button and the menu agree.
                self.ui.theme_name = self
                    .themes
                    .iter()
                    .find(|t| t.name == wanted && t.dark == dark)
                    .or_else(|| self.themes.iter().find(|t| t.dark == dark))
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                self.ui.dark_follows_system = false;
                if let Some(save) = &self.services.save_pref {
                    save("theme", &self.ui.theme_name);
                    save("dark", if dark { "true" } else { "false" });
                }
            }
            // Picks a theme by name; no name follows the system again.
            "ui.themeFile" => {
                if !p.is_null() && !p.as_object().is_some_and(|params| params.keys().all(|key| key == "name")) {
                    return Err(i18n::fmt("ui.themeFile accepts only the name parameter", &[]));
                }
                let name = match p.get("name") {
                    Some(given) => given.as_str().ok_or_else(|| i18n::fmt("ui.themeFile needs a name string", &[]))?.trim().to_string(),
                    None => String::new(),
                };
                if name.is_empty() {
                    self.ui.theme_name.clear();
                    self.ui.dark_follows_system = true;
                    if let Some(save) = &self.services.save_pref {
                        save("theme", "");
                        save("dark_fallback", if self.ui.dark { "true" } else { "false" });
                        save("dark", "auto");
                    }
                } else {
                    self.use_theme(&name)?;
                }
            }
            "ui.themeLoad" => {
                if cfg!(target_arch = "wasm32") {
                    return Err(i18n::fmt("Loading a theme file needs the desktop app", &[]));
                }
                let Some(path) = dialogs::pick_open(&["json"], tl!("A theme file")) else { return Ok(Value::Null) };
                self.load_theme_file(std::path::Path::new(&path))?;
            }
            "ui.scale" => {
                if !p.is_null() && !p.as_object().is_some_and(|params| params.keys().all(|key| key == "factor")) {
                    return Err(i18n::fmt("ui.scale accepts only the factor parameter", &[]));
                }
                let factor = match p.get("factor") {
                    Some(given) => given
                        .as_f64()
                        .ok_or_else(|| i18n::fmt("ui.scale needs a number at factor", &[]))?
                        .clamp(f64::from(f32::MIN), f64::from(f32::MAX)) as f32,
                    None => 1.0,
                };
                self.ui.interface_scale = theme::clamp_scale(factor);
                if self.ui.interface_scale != factor {
                    self.session.notify(
                        i18n::fmt("Interface scale held to {percent}%", &[("percent", &format!("{:.0}", self.ui.interface_scale * 100.0))]),
                        false,
                    );
                }
                if let Some(save) = &self.services.save_pref {
                    save("ui_scale", &self.ui.interface_scale.to_string());
                }
            }
            "ui.themeReload" => self.load_themes(),
            "ui.measurementUnits" => {
                let imperial = match p.get("units") {
                    None => !self.ui.measurement_imperial,
                    Some(Value::String(v)) if v.eq_ignore_ascii_case("metric") => false,
                    Some(Value::String(v)) if v.eq_ignore_ascii_case("imperial") => true,
                    _ => return Err(tl!("Measurement units must be metric or imperial").into()),
                };
                self.ui.measurement_imperial = imperial;
                if let Some(save) = &self.services.save_pref {
                    save("measurement_units", if imperial { "imperial" } else { "metric" });
                }
            }
            "ui.coordinateFormat" => {
                let format = match p.get("format") {
                    None | Some(Value::Null) => self.map.coord_format.next(),
                    Some(Value::String(v)) => CoordReadout::from_code(v)
                        .ok_or_else(|| tl!("The coordinate format must be decimal, dms, utm or mgrs").to_string())?,
                    _ => return Err(tl!("The coordinate format must be decimal, dms, utm or mgrs").into()),
                };
                self.map.coord_format = format;
                if let Some(save) = &self.services.save_pref {
                    save(coordreadout::PREF, format.code());
                }
            }
            "ui.moveSharedVertices" => {
                let on = match p.get("on") {
                    None => !self.map.move_shared_vertices,
                    Some(Value::Bool(on)) => *on,
                    _ => return Err(tl!("Shared vertices must be on or off").into()),
                };
                if on != self.map.move_shared_vertices {
                    self.map.cancel_vertex_drag();
                    self.map.move_shared_vertices = on;
                }
                return Ok(serde_json::json!({"on": on}));
            }
            "ui.traceEdges" => {
                let on = match p.get("on") {
                    None => !self.map.trace_edges,
                    Some(Value::Bool(on)) => *on,
                    _ => return Err(tl!("Trace must be on or off").into()),
                };
                self.map.trace_edges = on;
                return Ok(serde_json::json!({"on": on}));
            }
            "ui.map.gpu" => {
                self.map.gpu_enabled = p.get("on").and_then(Value::as_bool).unwrap_or(!self.map.gpu_enabled);
                // `"remember": false` turns it on or off for this visit only (a `?gpu` link).
                if p.get("remember").and_then(Value::as_bool) != Some(false)
                    && let Some(save) = &self.services.save_pref
                {
                    save("ui.map.gpu", if self.map.gpu_enabled { "1" } else { "0" });
                }
                self.map.invalidate();
            }
            "ui.commandSearch" => self.ui.command_search = Some(String::new()),
            "ui.about" => self.ui.about = true,
            "ui.checkUpdate" => self.update.check(),
            // The MCP relay for `rustgis-cli mcp --attach`: starts a loopback listener off until
            // asked. Running the command again reports its address; the app owns its lifetime.
            #[cfg(not(target_arch = "wasm32"))]
            "ui.mcpRelay" => {
                if p.get("stop").and_then(Value::as_bool).unwrap_or(false) {
                    // Idempotent: stopping an already stopped relay is not an error.
                    connectui::stopped(self);
                    if let Some(relay) = self.relay.take() {
                        relay.stop();
                        self.session.notify(tl!("MCP relay stopped").to_string(), false);
                    }
                } else if let Some(relay) = &self.relay {
                    // A named port is a request to listen there; silently ignoring it would hide
                    // the mistake, so it is refused with the way out instead of rebinding.
                    if let Some(asked) = p.get("port").and_then(Value::as_u64).and_then(|n| u16::try_from(n).ok())
                        && asked != relay.port()
                    {
                        return Err(i18n::fmt(
                            "MCP relay is already listening on 127.0.0.1:{port}; stop it first to use port {asked}",
                            &[("port", &relay.port().to_string()), ("asked", &asked.to_string())],
                        ));
                    }
                    let text = i18n::fmt("MCP relay listening on 127.0.0.1:{port}", &[("port", &relay.port().to_string())]);
                    self.session.notify(text, false);
                } else {
                    // A port asked for that isn't one is an error, as RUSTGIS_MCP_PORT's is.
                    let port = match p.get("port") {
                        None | Some(Value::Null) => rustgis_assistant::relay::default_port()?,
                        Some(v) => v.as_u64().and_then(|n| u16::try_from(n).ok()).ok_or_else(|| {
                            i18n::fmt("ui.mcpRelay: port {port} is not a port number (0 to 65535)", &[("port", &v.to_string())])
                        })?,
                    };
                    let relay = rustgis_assistant::relay::Relay::start(port)?;
                    // Where it is, for clients and setup scripts; the relay works without it.
                    if let Err(e) = relay.publish_endpoint() {
                        log::warn!("MCP relay: could not publish its endpoint file: {e}");
                    }
                    // With agents turned on in the assistant settings, they may use files and the
                    // network without asking (--yes); otherwise what each asks for decides.
                    relay.set_allow_access(connectui::enabled(self));
                    // What agents do to the user's project is kept, for review.
                    relay.set_audit_dir(rustgis_assistant::audit::dir());
                    relay.set_full_access(connectui::full_access(self));
                    // The relay only queues; the frame runs the messages, so a queued message
                    // asks for the frame that will run it. Before the first frame there is no
                    // context yet; `logic` gives the relay its wake once one exists.
                    if let Some(ctx) = &self.frame_ctx {
                        let ctx = ctx.clone();
                        relay.set_wake(Box::new(move || ctx.request_repaint()));
                    } else {
                        self.relay_wake_pending = true;
                    }
                    let text = i18n::fmt("MCP relay listening on 127.0.0.1:{port}", &[("port", &relay.port().to_string())]);
                    self.session.notify(text, false);
                    self.relay = Some(relay);
                }
            }
            "ui.plugins" => {
                self.plugins.open = p.get("open").and_then(Value::as_bool).unwrap_or(!self.plugins.open);
                if let Some(url) = p.get("registry").and_then(Value::as_str) {
                    self.plugins.url = url.to_string();
                    self.plugins.set_registry(None);
                }
                if let Some(filter) = p.get("filter").and_then(Value::as_str) {
                    self.plugins.filter = filter.chars().take(200).collect();
                }
                if let Some(tab) = p.get("tab").and_then(Value::as_str) {
                    self.plugins.tab = match tab {
                        "browse" => pluginsui::Tab::Browse,
                        "settings" => pluginsui::Tab::Settings,
                        _ => pluginsui::Tab::Installed,
                    };
                }
            }
            "ui.installPlugin" => pluginsui::install_from_file(self, p.get("path").and_then(Value::as_str)),
            // What a click with Explore does: identify there and show the popup.
            "ui.identify" => {
                // The engine checks the coordinates; the popup reuses them.
                let (lon, lat) = (p.get("lon").cloned().unwrap_or(Value::Null), p.get("lat").cloned().unwrap_or(Value::Null));
                let v = self.session.run("identify", &json!({"lon": lon, "lat": lat, "tolerancePx": 5}))?;
                if let (false, Some(lon), Some(lat)) = (v.is_null(), lon.as_f64(), lat.as_f64()) {
                    let map = self.session.project.active_map;
                    self.map.popup = Some(mapview::Popup::new(v, rustgis_engine::geom::Coord::new(lon, lat), map, self.session.opened));
                } else {
                    self.map.popup = None;
                }
            }
            "ui.shortcuts.set" | "ui.shortcuts.reset" | "ui.shortcuts.export" | "ui.shortcuts.import" => {
                return shortcutfile::run(self, id, p);
            }
            "ui.shortcuts" => self.ui.shortcuts = true,
            "ui.customizeShortcuts" => self.ui.shortcut_editor.open = true,
            // Copy, cut and copy path also put their text on the system clipboard.
            "ui.copy" | "ui.cut" | "ui.copyPath" => {
                let cmd = match id {
                    "ui.copy" => "edit.copy",
                    "ui.cut" => "edit.cut",
                    _ => "layer.copyPath",
                };
                let v = self.run(cmd, p.clone())?;
                if let Some(t) = v.get("text").and_then(Value::as_str) {
                    self.ui.copy_out = Some(t.to_string());
                }
                if id == "ui.copyPath" {
                    self.session
                        .notify(i18n::fmt("Copied {path}", &[("path", v.get("text").and_then(Value::as_str).unwrap_or(""))]), false);
                }
                return Ok(v);
            }
            "ui.backstage" => self.ui.backstage = p.get("open").and_then(Value::as_bool).unwrap_or(!self.ui.backstage),
            "ui.tab" => {
                if let Some(t) = p.get("tab").and_then(Value::as_str) {
                    self.ui.tab = ribbon::current_tab(t).to_string();
                }
            }
            // Delimited text: the file, then the dialog to pick its columns. With options (a
            // script's), the file is read with them instead.
            "ui.addDelimited" => {
                let opts = p.is_object().then(|| p.clone());
                if let Some(open) = &self.services.open_async {
                    self.ui.pending_layout_picture = false;
                    self.ui.pending_picture = None;
                    self.ui.pending_open = None;
                    // `null`: the dialog, for the file picked.
                    self.ui.pending_csv = Some(opts.unwrap_or(Value::Null));
                    open("text");
                } else if let Some(path) =
                    dialogs::pick_open(&["csv", "tsv", "txt", "xlsx", "xlsm", "xlsb", "xls", "ods"], tl!("Delimited text and spreadsheets"))
                {
                    return match opts {
                        Some(mut q) => {
                            q["path"] = json!(path);
                            self.run("layer.addData", q)
                        }
                        None => dialogs::open_csv(self, &path.clone(), dialogs::CsvSource::Path(path)),
                    };
                }
            }
            // A project by its web address (a `?project=` link): downloaded off the frame, opened
            // when it lands: native projects replace it; GeoLibre imports add a map (see `poll_downloads`).
            "ui.openUrl" => {
                let url = p.get("url").and_then(Value::as_str).map(str::trim).filter(|u| !u.is_empty()).ok_or("url is required")?;
                let url = rustgis_engine::map::geolibre::download_url(url);
                let url = url.as_str();
                let slot: Slot<Result<Vec<u8>, String>> = Slot::default();
                let land = slot.clone();
                rustgis_engine::render::rustgis_tiles::cache::download_limited(
                    url,
                    if rustgis_engine::map::geolibre::is_project_file(url) {
                        rustgis_engine::map::geolibre::MAX_TEXT
                    } else {
                        rustgis_engine::render::rustgis_tiles::cache::MAX_DOWNLOAD
                    },
                    Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)),
                );
                self.session.notify(i18n::fmt("Opening {url}…", &[("url", url)]), false);
                let id = self.session.download_started();
                if let Some((old, ..)) = self.ui.project_download.replace((id, url.to_string(), slot)) {
                    self.session.download_failed(old);
                }
            }
            // A STAC item's or collection's address (a `?stac=` link): STAC Search opens on it,
            // and an item's default asset is added.
            "ui.stacLink" => {
                let url = p.get("url").and_then(Value::as_str).map(str::trim).filter(|u| !u.is_empty()).ok_or("url is required")?;
                stacui::open_link(self, url)?;
            }
            // STAC Search (`ui.stac`), or a Web Data service that is STAC Search on one API or
            // catalog (`ui.stac.planet`, `ui.stac.portolan`, `ui.stac.sentinel2`, `ui.stac.vantor`,
            // `ui.stac.usgsLidar`); `{"search": true}` finds items at once.
            "ui.stac" => stacui::open_preset(self, None, p)?,
            other if other.starts_with("ui.stac.") => {
                let preset =
                    other.strip_prefix("ui.stac.").and_then(stacui::Preset::from_id).ok_or_else(|| format!("unknown command '{other}'"))?;
                stacui::open_preset(self, Some(preset), p)?;
            }
            // Search Open Data: the dialog, then (with a query) the search, off the frame.
            "ui.searchOpenData" => {
                self.run("ui.dialog", json!({"name": "opendata"}))?;
                opendataui::start(self, p);
            }
            // Data by its web address: the dialog, then (with an address) the download, off the
            // frame; it's added when it lands. A PMTiles archive is a tile layer, never downloaded.
            "ui.addUrl" => return self.add_url(p, None, None),
            "ui.connection" => {
                if let Some(id) = p.get("id").and_then(Value::as_str) {
                    connectionui::open_edit(self, id)?;
                } else {
                    connectionui::open_new(self, p.get("kind").and_then(Value::as_str).unwrap_or("service"))?;
                }
                if p.get("test").and_then(Value::as_bool) == Some(true) {
                    connectionui::start_test(self);
                }
            }
            // Choose S3 connection: the saved connection that reads the project's S3 sources
            // waiting for a choice (`connection.resolveS3` does it).
            "ui.s3Connections" => connectionui::open_chooser(self)?,
            // A map or feature service: the dialog, then (with an address) its capabilities,
            // fetched off the frame; the dialog lists its layers when they land.
            "ui.importService" => serviceui::start(self, p, false)?,
            "ui.refreshService" => serviceui::start(self, p, true)?,
            "ui.addService" => {
                let Some(url) = p.get("url").and_then(Value::as_str).map(str::trim).filter(|u| !u.is_empty()) else {
                    return self.run("ui.dialog", json!({"name": "addService"}));
                };
                let connection = p
                    .get("connection")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| serviceui::connection_for(self, self.dialogs.connection.as_deref(), url).map(str::to_string));
                // A catalogue: the dialog's search, run for the words given.
                if p.get("type").and_then(Value::as_str) == Some("csw") {
                    if self.dialogs.open != Some("addService") {
                        self.run("ui.dialog", json!({"name": "addService"}))?;
                    }
                    self.dialogs.mode = "csw";
                    self.dialogs.connection = connection;
                    self.ui.csw.url = url.to_string();
                    self.ui.csw.query = p.get("query").and_then(Value::as_str).unwrap_or("").to_string();
                    self.ui.csw.use_view = p.get("bbox").and_then(Value::as_str) == Some("view");
                    cswui::search(self, 1);
                    return Ok(Value::Null);
                }
                let kind: &'static str = match p.get("type").and_then(Value::as_str).unwrap_or("auto") {
                    "wms" => "wms",
                    "wmts" => "wmts",
                    "wfs" => "wfs",
                    "wcs" => "wcs",
                    "ogcapi" => "ogcapi",
                    "arcgis" => "arcgis",
                    _ => "auto",
                };
                // Opened from a menu (Home › Add › Web Data): the dialog with it.
                if self.dialogs.open != Some("addService") {
                    self.run("ui.dialog", json!({"name": "addService"}))?;
                }
                // The Type shown is the connection's.
                self.dialogs.mode = kind;
                // Another service: its own list, unfiltered (Open keeps the way back).
                self.ui.service_browse.filter.clear();
                self.ui.service_browse.back.clear();
                let mut source = json!({"url":url});
                if let Some(connection) = &connection {
                    source["connection"] = json!(connection);
                }
                let bound =
                    rustgis_engine::service_logins::request_params(&mut self.session, &source).map_err(|e| connectionui::translated(&e))?;
                let resolved_connection = bound.get("connection").and_then(Value::as_str).map(str::to_string);
                let url = bound.get("url").and_then(Value::as_str).unwrap_or(url);
                let caps = rustgis_engine::data::ogc::capabilities_url(url, (kind != "auto").then_some(kind));
                // A refusal is told with what the server said (#509).
                let slot = fetch_detailed_into(&caps);
                // The dialog shows the address connected to, however the connection was asked for.
                self.dialogs.text = rustgis_engine::data::login_address::inner(url).to_string();
                self.dialogs.connection = connection;
                self.ui.service = Some(ServiceConn {
                    url: url.to_string(),
                    connection: resolved_connection,
                    kind,
                    slot,
                    text: None,
                    documents: Vec::new(),
                    pending: Some(caps),
                    listing: None,
                    added: None,
                    error: None,
                    failure: None,
                });
            }
            "ui.importShapefile" => {
                let text = p.get("crs").and_then(Value::as_str).ok_or(tl!("Source coordinate system is required"))?;
                rustgis_engine::data::raster::Crs::parse(text)?;
                if let Some(open) = &self.services.open_async {
                    self.ui.pending_layout_picture = false;
                    self.ui.pending_picture = None;
                    self.ui.pending_open = None;
                    self.ui.pending_csv = None;
                    self.ui.pending_shapefile = Some(json!({"crs": text}));
                    open("shapefile");
                } else if let Some(path) = dialogs::pick_open(&["shp", "zip"], tl!("Shapefile")) {
                    return self.run("layer.addData", json!({"path": path, "crs": text}));
                }
            }
            "ui.addData" => {
                if let Some(open) = &self.services.open_async {
                    // A picker cancelled earlier mustn't claim this file.
                    self.ui.pending_layout_picture = false;
                    self.ui.pending_picture = None;
                    self.ui.pending_open = None;
                    self.ui.pending_csv = None;
                    open("data");
                } else if let Some(path) = dialogs::pick_open(
                    &[
                        &[
                            "geojson",
                            "json",
                            "csv",
                            "tsv",
                            "xlsx",
                            "xlsm",
                            "xlsb",
                            "xls",
                            "ods",
                            "rss",
                            "georss",
                            "atom",
                            "xml",
                            "landxml",
                            "jpg",
                            "jpeg",
                            "shp",
                            "zip",
                            "gpkg",
                            "parquet",
                            "geoparquet",
                            "fgb",
                            "duckdb",
                            "ddb",
                            "pmtiles",
                            "mbtiles",
                            "kml",
                            "kmz",
                            "gpx",
                            "gml",
                            "topojson",
                            "dxf",
                            "dwg",
                            // A File Geodatabase is a folder: any of its tables adds it.
                            "gdbtable",
                            "tab",
                            "mif",
                            "osm",
                            "pbf",
                            "las",
                            "laz",
                            "tif",
                            "tiff",
                            "asc",
                        ][..],
                        // Rasters GDAL reads (every one it is asked to read).
                        #[cfg(not(target_arch = "wasm32"))]
                        rustgis_engine::data::raster::gdal::EXTENSIONS,
                    ]
                    .concat(),
                    tl!("Vector and raster data"),
                ) {
                    return layerchooser::add_file(self, &path);
                }
            }
            "ui.open" => {
                if let Some(open) = &self.services.open_async {
                    // A picker cancelled earlier mustn't claim this file.
                    self.ui.pending_layout_picture = false;
                    self.ui.pending_picture = None;
                    self.ui.pending_open = None;
                    open("project");
                    self.ui.backstage = false;
                } else if let Some(path) = dialogs::pick_open(
                    &["rgis", "rgisx", "rgispkg", "qgs", "qgz", "geolibre", "json"],
                    tl!("Projects and JSON data (RustGIS, QGIS, GeoLibre)"),
                ) {
                    // A QGIS project opens as a new map of this one.
                    let lower = path.to_ascii_lowercase();
                    let cmd = if lower.ends_with(".qgs") || lower.ends_with(".qgz") {
                        "project.importQgis"
                    } else {
                        rustgis_engine::project::open_command(&path)
                    };
                    let r = self.run_by_user(cmd, json!({"path": path}));
                    // Asking to save first: the Project tab stays until the answer, so Cancel
                    // goes back to it.
                    if !saveprompt::waiting(self) {
                        self.ui.backstage = false;
                        self.map.invalidate();
                    }
                    return r;
                }
            }
            // The browser can't write a file in place: Save downloads the project again under the
            // file name it was saved or opened as, and Save As (or a first Save) asks for one.
            "ui.save" | "ui.saveAs" if self.services.download.is_some() => {
                let name = format!("{}.rgis", self.session.project.name);
                if id == "ui.save" && self.ui.web_file.as_deref() == Some(name.as_str()) {
                    // The keyboard caller discards the result, so the failure is shown here too.
                    if let Err(error) = self.save_download(&name) {
                        self.session.notify(error.clone(), true);
                        return Err(error);
                    }
                } else {
                    self.dialogs.open("saveProject", &self.session)?;
                }
            }
            "ui.save" => {
                if self.session.path.is_some() {
                    return self.run("project.save", Value::Null);
                }
                return self.run("ui.saveAs", Value::Null);
            }
            "ui.saveAs" => {
                let name = format!("{}.rgis", self.session.project.name);
                if let Some(path) = dialogs::pick_save(&name, &["rgis"], tl!("RustGIS project")) {
                    return self.run("project.saveAs", json!({"path": path}));
                }
            }
            // Layouts: export (PDF/PNG/SVG), print, save and import layout files, draw frames.
            "ui.exportLayout" | "ui.printLayout" => {
                let li = self.session.active_layout.ok_or("open a layout first")?;
                let name = self.session.project.layouts.get(li).map(|l| l.name.clone()).unwrap_or_default();
                let print = id == "ui.printLayout";
                let pdf_only = print || p.get("format").and_then(Value::as_str) == Some("pdf");
                if self.services.download.is_some() {
                    let bytes = self.layout_bytes(li, true, if print { 300.0 } else { 150.0 })?;
                    self.download(&format!("{name}.pdf"), bytes);
                } else if print {
                    // Printing goes through the system's PDF viewer, which owns the printers.
                    let bytes = self.layout_bytes(li, true, 300.0)?;
                    let safe: String =
                        name.chars().map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_') { c } else { '_' }).collect();
                    let path = print_dir()?.join(format!("{safe}.pdf"));
                    rustgis_engine::data::io::write_atomic(&path, &bytes).map_err(|e| e.to_string())?;
                    open_with_system(&path)?;
                    self.session.notify(i18n::fmt("Opened {name} for printing", &[("name", &path.display().to_string())]), false);
                } else {
                    let (exts, kinds): (&[&str], _) =
                        if pdf_only { (&["pdf"], tl!("PDF")) } else { (&["pdf", "png", "svg"], tl!("PDF, PNG or SVG")) };
                    if let Some(path) = dialogs::pick_save(&format!("{name}.pdf"), exts, kinds) {
                        // Drawn on a thread of its own, with progress and Cancel in the status bar.
                        #[cfg(not(target_arch = "wasm32"))]
                        return exportjob::start(self, "layout.export", json!({"layout": li, "path": path, "dpi": 200}));
                        #[cfg(target_arch = "wasm32")]
                        return self.run("layout.export", json!({"layout": li, "path": path, "dpi": 200}));
                    }
                }
            }
            "ui.newReport" => {
                let r = self.run("report.new", Value::Null);
                if r.is_ok() {
                    self.ui.open_right(RightPane::Report);
                }
                return r;
            }
            "ui.reportLayoutFile" => {
                if let Some(open) = &self.services.open_async {
                    self.ui.pending_open = Some("report.existingTemplate");
                    open("item");
                } else if let Some(path) = dialogs::pick_open(&["rgislayout"], tl!("RustGIS layout")) {
                    return self.run("report.existingTemplate", json!({"path": path}));
                }
            }
            // Reports: export as PDF (or PNG, the first page).
            "ui.exportReport" => {
                let i = rustgis_engine::cmd::report::index(&self.session, p)?;
                let name = self.session.project.reports.get(i).map(|r| r.name.clone()).unwrap_or_default();
                if self.services.download.is_some() {
                    let session = &mut self.session;
                    let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        rustgis_engine::cmd::report::report_bytes(session, i, 150.0, true)
                    }))
                    .unwrap_or_else(|_| Err("the report export failed unexpectedly".into()))
                    .inspect_err(|e| self.session.notify(e.clone(), true))?
                    .0;
                    self.download(&format!("{name}.pdf"), bytes);
                } else if let Some(path) = dialogs::pick_save(&format!("{name}.pdf"), &["pdf", "png"], tl!("PDF or PNG")) {
                    return self.run("report.export", json!({"report": i, "path": path, "dpi": 200}));
                }
            }
            "ui.saveLayout" => {
                let li = self.session.active_layout.ok_or("open a layout first")?;
                let l = self.session.project.layouts.get(li).cloned().ok_or("open a layout first")?;
                if self.services.download.is_some() {
                    let text = serde_json::to_string_pretty(&l).map_err(|e| e.to_string())?;
                    self.download(&format!("{}.rgislayout", l.name), text.into_bytes());
                } else if let Some(path) = dialogs::pick_save(&format!("{}.rgislayout", l.name), &["rgislayout"], tl!("RustGIS layout")) {
                    return self.run("layout.save", json!({"layout": li, "path": path}));
                }
            }
            "ui.importLayout" => {
                if let Some(open) = &self.services.open_async {
                    // A picker cancelled earlier mustn't claim this file.
                    self.ui.pending_layout_picture = false;
                    self.ui.pending_picture = None;
                    self.ui.pending_open = None;
                    open("layout");
                } else if let Some(path) = dialogs::pick_open(&["rgislayout"], tl!("RustGIS layout")) {
                    return self.run("layout.import", json!({"path": path}));
                }
            }
            "ui.layoutPicture" => {
                let li = self.session.active_layout.ok_or("open a layout first")?;
                if let Some(open) = &self.services.open_async {
                    self.ui.pending_layout_picture = true;
                    self.ui.pending_picture = None;
                    self.ui.pending_open = None;
                    open("picture");
                } else if let Some(path) = dialogs::pick_open(&["png", "jpg", "jpeg"], tl!("Pictures")) {
                    let v = self.run("layout.addElement", json!({"layout": li, "kind": "picture", "path": path}))?;
                    self.layout.selected = v["element"].as_u64().map(|v| v as u32);
                }
            }
            // Project items: save (a file dialog, or a download on the web) and open.
            "ui.saveItem" => {
                let kind = p.get("kind").and_then(Value::as_str).ok_or("kind is required")?;
                let session = &mut self.session;
                let (name, bytes) =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rustgis_engine::cmd::items::item_bytes(session, kind, p)))
                        .unwrap_or_else(|_| Err("saving failed unexpectedly".into()))
                        .inspect_err(|e| self.session.notify(e.clone(), true))?;
                if self.services.download.is_some() {
                    self.download(&name, bytes);
                } else {
                    let (ext, label) =
                        rustgis_engine::cmd::items::ITEMS.iter().find(|i| i.0 == kind).map_or(("json", "File"), |i| (i.1, i.2));
                    if let Some(path) = dialogs::pick_save(&name, &[ext], tl!(label)) {
                        rustgis_engine::data::io::write_atomic(std::path::Path::new(&path), &bytes).map_err(|e| e.to_string())?;
                        self.session.notify(i18n::fmt("Saved {name}", &[("name", &path)]), false);
                    }
                }
            }
            "ui.openItem" => {
                let kind = p.get("kind").and_then(Value::as_str).ok_or("kind is required")?;
                let (cmd, exts, label): (&str, &[&str], &str) = match kind {
                    "map" => ("map.import", &["rgismap", "rgismpk", "rgis", "rgisx", "rgispkg"], "RustGIS maps and projects"),
                    "layer" => ("layer.addLayerFile", &["rgislyr"], "RustGIS layer file"),
                    "styles" => ("style.add", &["rgisstyle"], "RustGIS styles"),
                    "ramps" => ("ramp.add", &["rgisramp", "xml", "qml", "txt", "clr", "cpt"], "Colour ramps (RustGIS, QGIS, GDAL, GMT)"),
                    "importStyles" => ("style.import", &["rgislyr", "rgismap", "rgis", "rgisx"], "RustGIS layers, maps and projects"),
                    "geolibreProject" => ("project.importGeolibre", &["geolibre", "json"], "GeoLibre project"),
                    "qgisProject" => ("project.importQgis", &["qgs", "qgz"], "QGIS project"),
                    "qgisStyle" => ("style.importQgis", &["qml"], "QGIS layer style"),
                    "template" => ("project.fromTemplate", &["rgistemplate"], "RustGIS project template"),
                    "task" => ("task.import", &["rgistask"], "RustGIS task"),
                    "bookmarks" => ("bookmark.import", &["rgisbookmarks"], "RustGIS bookmarks"),
                    "report" => ("report.open", &["rgisreport", "rgisrpttemplate"], "RustGIS report or report template"),
                    "presentation" => ("presentation.import", &["rgispres"], "RustGIS presentation"),
                    "notebook" => ("notebook.open", &["rgisnb", "ipynb"], "RustGIS or Jupyter notebook"),
                    "dlPackage" => ("dl.import", &["rgisdlpk"], "RustGIS deep learning package"),
                    k => return Err(format!("unknown item '{k}'")),
                };
                if let Some(open) = &self.services.open_async {
                    self.ui.pending_open = Some(cmd);
                    open(match kind {
                        "ramps" => "ramp",
                        "geolibreProject" => "project",
                        "qgisProject" => "qgisProject",
                        _ => "item",
                    });
                } else if let Some(path) = dialogs::pick_open(exts, tl!(label)) {
                    let r = self.run_by_user(cmd, json!({"path": path}));
                    self.map.invalidate();
                    return r;
                }
            }
            "ui.addFolder" => {
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    return self.run("project.addFolder", json!({"path": dir.display().to_string()}));
                }
                #[cfg(target_arch = "wasm32")]
                return Err("folder connections need the desktop app".into());
            }
            "ui.captureMap" => {
                let session = &mut self.session;
                let png =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rustgis_engine::cmd::share::map_png_fitted(session, 192.0)))
                        .unwrap_or_else(|_| Err("the capture failed unexpectedly".into()))?;
                let pm = rustgis_engine::render::decode_png(&png).ok_or("could not draw the map")?;
                let size = [pm.width() as usize, pm.height() as usize];
                self.ui.copy_image = Some(egui::ColorImage::from_rgba_premultiplied(size, pm.data()));
                self.session.notify(tl!("Copied the map to the clipboard"), false);
            }
            // Copies the open layout's page as an image: at 192 dpi, lower for a page that would
            // pass the app's own pixel budget or the renderer's largest side (and says so). A page
            // that would be copied below 36 dpi is an error.
            "ui.captureLayout" => {
                let li = self.session.active_layout.ok_or_else(|| tl!("open a layout first").to_string())?;
                // Text still being typed into the Format pane is part of the page.
                layoutview::commit_drafts(self);
                let (w_mm, h_mm) = self
                    .session
                    .project
                    .layouts
                    .get(li)
                    .map(|l| (l.width, l.height))
                    .ok_or_else(|| tl!("open a layout first").to_string())?;
                let area = (w_mm / 25.4) * (h_mm / 25.4);
                let side = rustgis_engine::render::layout::MAX_SIDE * 25.4 / w_mm.max(h_mm);
                let dpi = CAPTURE_DPI.min((CAPTURE_PIXELS / area).sqrt()).min(side);
                if dpi < CAPTURE_MIN_DPI {
                    return Err(tl!("the page is too large to copy to the clipboard: export it instead").into());
                }
                let session = &mut self.session;
                let png = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    rustgis_engine::cmd::layout::layout_bytes(session, li, false, dpi, false)
                }))
                .unwrap_or_else(|_| Err(tl!("the capture failed unexpectedly").into()))?;
                let pm = rustgis_engine::render::decode_png(&png).ok_or_else(|| tl!("could not draw the layout").to_string())?;
                let size = [pm.width() as usize, pm.height() as usize];
                self.ui.copy_image = Some(egui::ColorImage::from_rgba_premultiplied(size, pm.data()));
                // The dpi reported is the one the image has, on its smaller scale (a whole pixel
                // short is only rounding).
                let (px_w, px_h) = (f64::from(pm.width()), f64::from(pm.height()));
                let short = (w_mm / 25.4 * dpi).floor() - px_w > 1.0 || (h_mm / 25.4 * dpi).floor() - px_h > 1.0;
                if dpi < CAPTURE_DPI || short {
                    let got = (px_w * 25.4 / w_mm).min(px_h * 25.4 / h_mm).floor();
                    self.session.notify(
                        i18n::fmt(
                            "Copied the layout to the clipboard at {dpi} dpi, not {wanted}: the page is too large for a larger image",
                            &[("dpi", &format!("{got:.0}")), ("wanted", &format!("{CAPTURE_DPI:.0}"))],
                        ),
                        false,
                    );
                } else {
                    self.session.notify(tl!("Copied the layout to the clipboard"), false);
                }
            }
            "ui.printMap" => {
                let session = &mut self.session;
                let pdf = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rustgis_engine::cmd::share::map_pdf(session, 300.0)))
                    .unwrap_or_else(|_| Err("printing failed unexpectedly".into()))
                    .inspect_err(|e| self.session.notify(e.clone(), true))?;
                let name = format!("{}.pdf", self.session.map().name);
                if self.services.download.is_some() {
                    self.download(&name, pdf);
                } else {
                    let safe: String = self
                        .session
                        .map()
                        .name
                        .chars()
                        .map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_') { c } else { '_' })
                        .collect();
                    let path = print_dir()?.join(format!("{safe}.pdf"));
                    rustgis_engine::data::io::write_atomic(&path, &pdf).map_err(|e| e.to_string())?;
                    open_with_system(&path)?;
                    self.session.notify(i18n::fmt("Opened {name} for printing", &[("name", &path.display().to_string())]), false);
                }
            }
            "ui.pane.tasks" => toggle_right(&mut self.ui, RightPane::Tasks),
            // Show Me for a task step (`{"command", "params"?}`): where its command is.
            "ui.taskShowMe" => {
                let st: rustgis_engine::project::TaskStep = serde_json::from_value(json!({
                    "title": p.get("title").cloned().unwrap_or(json!("")),
                    "command": p.get("command").cloned().unwrap_or(Value::Null),
                    "params": p.get("params").cloned().unwrap_or(Value::Null),
                }))
                .map_err(|e| format!("a step needs a command: {e}"))?;
                return Ok(json!({"said": showme::start(self, &st)}));
            }
            // Record Task: starts recording, or stops and makes the task (named `name`, else
            // "Recorded Task"). Says whether it is recording and how many steps it has.
            "ui.taskRecord" => match self.ws.recording.take() {
                None => {
                    self.ws.recording = Some(Vec::new());
                    self.ws.recording_full = false;
                    self.ws.recording_undo_said = false;
                    self.ws.recording_shared_said.clear();
                    self.ws.recording_key_said = false;
                    self.ws.record_name.clear();
                    return Ok(json!({"recording": true, "steps": 0}));
                }
                Some(steps) if steps.is_empty() => {
                    self.session.notify(tl!("Nothing was recorded, so no task was made."), false);
                    return Ok(json!({"recording": false, "steps": 0}));
                }
                Some(steps) => {
                    let name = p.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Recorded Task");
                    let n = steps.len();
                    let r = match self.session.run("task.new", &json!({"name": name, "steps": &steps})) {
                        Ok(r) => r,
                        Err(e) => {
                            // The exercise stays recorded: stopping again tries again.
                            self.ws.recording = Some(steps);
                            return Err(e);
                        }
                    };
                    if let Some(task) = r.get("task").and_then(Value::as_str) {
                        self.ws.task = task.to_string();
                        self.ws.task_step = 0;
                        self.ws.task_edit = None;
                        self.ws.task_editing = true;
                    }
                    self.ui.right = Some(RightPane::Tasks);
                    return Ok(json!({"recording": false, "steps": n, "task": r.get("task")}));
                }
            },
            "ui.pane.jobs" => toggle_right(&mut self.ui, RightPane::Jobs),
            "ui.pane.profile" => toggle_right(&mut self.ui, RightPane::Profile),
            "ui.pane.timeSeries" => toggle_right(&mut self.ui, RightPane::TimeSeries),
            "ui.pane.diagnostics" => toggle_right(&mut self.ui, RightPane::Diagnostics),
            "ui.pane.diagnosticLog" => toggle_right(&mut self.ui, RightPane::DiagnosticLog),
            "ui.pane.infographics" => toggle_right(&mut self.ui, RightPane::Infographics),
            "ui.pane.report" => toggle_right(&mut self.ui, RightPane::Report),
            "ui.pane.presentation" => toggle_right(&mut self.ui, RightPane::Presentation),
            "ui.pane.notebook" => toggle_right(&mut self.ui, RightPane::Notebook),
            // `{"ask": text}` opens the pane and sends the text.
            "ui.pane.assistant" => match p.get("ask").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()) {
                Some(text) => {
                    self.ui.open_right(RightPane::Assistant);
                    self.assistant.queued = Some(text.to_string());
                }
                None => toggle_right(&mut self.ui, RightPane::Assistant),
            },
            "ui.pane.shares" => toggle_right(&mut self.ui, RightPane::Shares),
            // Share As: to a file (recorded under Status › Jobs), or a download in the browser.
            "ui.share" => {
                let kind = p.get("kind").and_then(Value::as_str).ok_or("kind is required")?;
                let cmd = match kind {
                    "webMap" => "share.webMap",
                    "webLayer" => "share.webLayer",
                    "webStyle" => "share.webStyle",
                    "locator" => "share.locator",
                    "geodataService" => "share.geodataService",
                    "webTool" => "share.webTool",
                    "webScene" => "share.webScene",
                    "linkChart" => "share.linkChart",
                    k => return Err(format!("unknown share '{k}'")),
                };
                if self.services.download.is_some() {
                    return self.run("ui.saveItem", json!({"kind": kind}));
                }
                let (ext, label) = rustgis_engine::cmd::sharing::KINDS.iter().find(|k| k.0 == kind).map_or(("bin", "File"), |k| (k.1, k.2));
                let name = match kind {
                    "webLayer" | "webStyle" | "locator" => {
                        self.session.active_layer.and_then(|id| self.session.map().layer(id)).map(|l| l.name.clone())
                    }
                    _ => Some(self.session.map().name.clone()),
                }
                .unwrap_or_else(|| "shared".into());
                if let Some(path) = dialogs::pick_save(&format!("{name}.{ext}"), &[ext], tl!(label)) {
                    return self.run(cmd, json!({"path": path}));
                }
            }
            "ui.newPresentation" | "ui.newNotebook" => {
                let (cmd, pane) = if id == "ui.newPresentation" {
                    ("presentation.new", RightPane::Presentation)
                } else {
                    ("notebook.new", RightPane::Notebook)
                };
                let r = self.run(cmd, Value::Null);
                if r.is_ok() {
                    self.ui.open_right(pane);
                }
                return r;
            }
            "ui.presentationPicture" => {
                if let Some(open) = &self.services.open_async {
                    self.ui.pending_layout_picture = false;
                    self.ui.pending_picture = None;
                    self.ui.pending_open = Some("presentation.imagePage");
                    open("picture");
                } else if let Some(path) = dialogs::pick_open(&["png", "jpg", "jpeg"], tl!("Pictures")) {
                    return self.run("presentation.imagePage", json!({"path": path}));
                }
            }
            "ui.presentationVideo" => {
                // In the browser a video is a web address, typed in the pane.
                if self.services.open_async.is_some() {
                    self.ui.open_right(RightPane::Presentation);
                } else if let Some(path) = dialogs::pick_open(&["mp4", "webm", "ogv", "mov"], tl!("Videos")) {
                    return self.run("presentation.videoPage", json!({"source": path}));
                }
            }
            "ui.exportPresentation" => {
                let i = rustgis_engine::cmd::presentation::index(&self.session, p)?;
                let name = self.session.project.presentations.get(i).map(|r| r.name.clone()).unwrap_or_default();
                if self.services.download.is_some() {
                    let session = &mut self.session;
                    let bytes = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        rustgis_engine::cmd::presentation::export_bytes(session, i, "html", 110.0)
                    }))
                    .unwrap_or_else(|_| Err("the presentation export failed unexpectedly".into()))
                    .inspect_err(|e| self.session.notify(e.clone(), true))?
                    .0;
                    self.download(&format!("{name}.html"), bytes);
                } else if let Some(path) =
                    dialogs::pick_save(&format!("{name}.html"), &["html", "pdf", "png"], tl!("Slideshow, PDF or PNG"))
                {
                    return self.run("presentation.export", json!({"presentation": i, "path": path}));
                }
            }
            "ui.modelBuilder" => modelbuilder::command(self, p)?,
            "ui.importModel" => modelbuilder::import(self),
            "ui.catalogView" => self.ws.catalog_view = p.get("on").and_then(Value::as_bool).unwrap_or(!self.ws.catalog_view),
            "ui.itemPopups" => self.ws.item_popups = p.get("on").and_then(Value::as_bool).unwrap_or(!self.ws.item_popups),
            "ui.linkCursors" => self.ws.link_cursors = p.get("on").and_then(Value::as_bool).unwrap_or(!self.ws.link_cursors),
            // A second map beside the active one (for linked views and cursors).
            "ui.splitView" => {
                // On while its map is there: a name left by a deleted or renamed map counts as off.
                let on = mapview::split_map(self).is_some();
                let (maps, active) = (&self.session.project.maps, self.session.project.active_map);
                self.ws.split = match (on, p.get("map").and_then(Value::as_str)) {
                    (_, Some(n)) => {
                        Some(maps.iter().find(|m| m.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no map '{n}'"))?.name.clone())
                    }
                    (true, None) => None,
                    (false, None) => Some(
                        maps.iter()
                            .enumerate()
                            .find(|(i, _)| *i != active)
                            .map(|(_, m)| m.name.clone())
                            .ok_or("add a second map first (Data › New Map)")?,
                    ),
                };
            }
            "ui.paneSet" => {
                if let Some(n) = p.get("save").and_then(Value::as_str) {
                    self.ui.pane_sets.retain(|x| x.0 != n);
                    let open = self.ui.right_open.clone();
                    self.ui.pane_sets.push((n.to_string(), self.ui.left, self.ui.right, self.ui.tab.clone(), open));
                } else if let Some(n) = p.get("load").and_then(Value::as_str) {
                    let set = self.ui.pane_sets.iter().find(|x| x.0 == n).cloned().ok_or_else(|| format!("no pane set '{n}'"))?;
                    (self.ui.left, self.ui.right, self.ui.tab) = (set.1, set.2, ribbon::current_tab(&set.3).to_string());
                    self.ui.right_open = set.4;
                    // The pane in view is one of the tabs.
                    if let Some(pane) = set.2 {
                        self.ui.open_right(pane);
                    }
                } else {
                    return Err("save or load a pane set by name".into());
                }
            }
            "ui.learn" => {
                // Only web pages: the URL can come from a script or command search.
                let url = p.get("url").and_then(Value::as_str).unwrap_or("https://rustgis.app/user-guide/").to_string();
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err("ui.learn opens web pages (http or https) only".into());
                }
                self.ui.open_url = Some(url);
            }
            "ui.customizeRibbon" => {
                let tab = p.get("tab").and_then(Value::as_str).ok_or("tab is required")?.to_string();
                if ribbon::FIXED_TABS.contains(&tab.as_str()) {
                    return Err(format!("the {tab} tab always shows"));
                }
                if self.ui.hidden_tabs.contains(&tab) {
                    self.ui.hidden_tabs.retain(|t| *t != tab);
                } else {
                    self.ui.hidden_tabs.push(tab);
                }
                if let Some(save) = &self.services.save_pref {
                    save("hidden_tabs", &self.ui.hidden_tabs.join(","));
                }
            }
            "ui.quickAccess" => {
                let id = p.get("command").and_then(Value::as_str).ok_or("command is required")?.to_string();
                if self.ui.quick_access.contains(&id) {
                    self.ui.quick_access.retain(|c| *c != id);
                } else if self.session.registry.get(&id).is_some() || id.starts_with("ui.") {
                    self.ui.quick_access.push(id);
                } else {
                    return Err(format!("unknown command '{id}'"));
                }
                if let Some(save) = &self.services.save_pref {
                    save("quick_access", &self.ui.quick_access.join(","));
                }
            }
            "ui.importThumbnail" => {
                let cmd = if p.get("map").and_then(Value::as_bool) == Some(true) { "map.thumbnail" } else { "project.thumbnail" };
                if let Some(open) = &self.services.open_async {
                    self.ui.pending_open = Some(cmd);
                    open("picture");
                } else if let Some(path) = dialogs::pick_open(&["png", "jpg", "jpeg"], tl!("Pictures")) {
                    return self.run(cmd, json!({"path": path}));
                }
            }
            "ui.timeSlider" => return timebar::toggle(self),
            "ui.timePlay" => return timebar::play(self),
            "ui.timeExport" => return timebar::export(self),
            "ui.animationPlay" => {
                if self.session.map().animation.len() < 2 {
                    return Err("add at least two keyframes".into());
                }
                self.ws.playing = if self.ws.playing.is_some() { None } else { Some((0.0, 0)) };
            }
            "ui.exportAnimation" => {
                if self.services.download.is_some() {
                    return Err("exporting animations needs the desktop app".into());
                }
                let name = format!("{} animation.zip", self.session.map().name);
                if let Some(path) = dialogs::pick_save(&name, &["zip"], tl!("PNG frames (zip)")) {
                    return self.run("animation.export", json!({"path": path}));
                }
            }
            "ui.magnifier" => self.map.magnifier = p.get("on").and_then(Value::as_bool).unwrap_or(!self.map.magnifier),
            "ui.layoutSelect" => self.layout.selected = p.get("element").and_then(Value::as_u64).map(|v| v as u32),
            "ui.layoutTool" => {
                self.layout.tool = match p.get("shape").and_then(Value::as_str) {
                    Some("polygon") => Some("polygon"),
                    Some("lasso") => Some("lasso"),
                    _ => None,
                };
            }
            "ui.exportMap" if self.services.download.is_some() => {
                // The same last-resort panic guard `Session::run` gives commands.
                let session = &mut self.session;
                let png =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rustgis_engine::cmd::share::map_png_fitted(session, 192.0)))
                        .unwrap_or_else(|_| Err("Export Map failed unexpectedly".into()));
                let png = match png {
                    Ok(p) => p,
                    Err(e) => {
                        self.session.notify(e.clone(), true);
                        return Err(e);
                    }
                };
                let name = format!("{}.png", self.session.map().name);
                self.download(&name, png);
            }
            // Without a format, the Export Features dialog asks for one.
            "ui.exportLayer" if p.get("format").and_then(Value::as_str).is_none() => {
                self.dialogs.open("exportLayer", &self.session)?;
            }
            "ui.exportLayer" if self.services.download.is_some() => {
                let id = self.export_target(p)?;
                let l = self.session.map().layer(id).ok_or_else(|| tl!("select a layer in the Layers pane first").to_string())?;
                // Any format the dialog offers here (`dialogs::export_formats`): not GeoPackage or
                // FlatGeobuf, which the data crate refuses in the browser.
                let ext = p.get("format").and_then(Value::as_str).unwrap_or("geojson").to_ascii_lowercase();
                rustgis_engine::cmd::share::gpkg_table(p, &ext)?;
                // A streamed layer exports its whole source, not the features read for the view.
                let whole = if l.is_streamed() { Some(l.load_all()?) } else { None };
                // In the layer's own coordinate system, as share.exportLayer writes a file.
                let ds = whole.as_ref().unwrap_or(&l.dataset);
                use rustgis_engine::data::io;
                let crs = io::default_export_crs(ds, &ext);
                let written = io::write_bytes_in(ds, &ext, crs).map_err(|e| e.to_string());
                let (bytes, left_out) = match written {
                    Ok(w) => w,
                    Err(e) => {
                        self.session.notify(e.clone(), true);
                        return Err(e);
                    }
                };
                let ext = if ext == "shp" { "zip".to_string() } else { ext };
                let name = format!("{}.{ext}", l.name);
                let notes = rustgis_engine::data::export_notes([(ds, l.extent())], &name, crs);
                let mut dbf_notes = Vec::new();
                if ext == "zip" {
                    dbf_notes.extend(io::shapefile_names_note(&ds.fields, &name));
                    dbf_notes.extend(io::shapefile_values_notes(ds, &name));
                }
                if matches!(ext.as_str(), "parquet" | "geoparquet") {
                    dbf_notes.extend(rustgis_engine::data::source::parquet::values_note(ds, &name));
                }
                if left_out > 0 {
                    self.session.notify(format!("{name}: {left_out} features without drawable geometry were left out"), false);
                }
                if ext == "zip" && rustgis_engine::data::raster::wkt::to_wkt(crs).is_none() {
                    self.session.notify(format!("{name}: no .prj written, as {} has no WKT form", crs.label()), false);
                }
                if self.download(&name, bytes) {
                    for note in notes.into_iter().chain(dbf_notes) {
                        self.session.notify(note, false);
                    }
                }
            }
            "ui.exportRaster" if self.services.download.is_some() => {
                // The web app: encode once and download. A tile layer is fetched on a thread of
                // its own, which the browser hasn't: the export says so.
                if p.get("tiles").is_some_and(|t| !t.is_null()) || self.session.selected_tile().is_some() {
                    let tiles = p.get("tiles").cloned().unwrap_or(Value::Null);
                    return self.run("share.exportRaster", json!({"path": "tiles.tif", "tiles": tiles}));
                }
                match rustgis_engine::cmd::raster::geotiff_bytes(&mut self.session, &Value::Null) {
                    Ok((name, bytes)) => {
                        self.download(&format!("{name}.tif"), bytes);
                    }
                    Err(e) => {
                        self.session.notify(e.clone(), true);
                        return Err(e);
                    }
                }
            }
            "ui.exportRaster" => {
                // A tile layer (the one its menu names, else the one picked) is written over the
                // view at the zoom it shows.
                // A tile layer asked for by name or index is that one or none, never another.
                let tile = match p.get("tiles").filter(|t| !t.is_null()) {
                    Some(v) => {
                        let all = &self.session.map().tiles;
                        let named = v.as_str().and_then(|n| all.iter().position(|t| t.name.eq_ignore_ascii_case(n.trim())));
                        match v.as_u64().and_then(|i| usize::try_from(i).ok()).or(named).filter(|i| *i < all.len()) {
                            Some(i) => Some(i),
                            None => {
                                let why = tl!("No such tile layer").to_string();
                                self.session.notify(why.clone(), true);
                                return Err(why);
                            }
                        }
                    }
                    None => self.session.selected_tile(),
                };
                let tiles = self.session.map().tiles.get(tile.unwrap_or(usize::MAX)).map(|t| t.name.clone());
                let name = match &tiles {
                    Some(t) => format!("{t}.tif"),
                    None => self
                        .session
                        .active_layer
                        .and_then(|id| self.session.map().layer(id))
                        .map(|l| format!("{}.tif", l.name))
                        .unwrap_or_default(),
                };
                // Neither a tile layer nor a raster: said before the save dialog, not after it.
                let raster = self.session.active_layer.and_then(|id| self.session.map().layer(id)).is_some_and(|l| l.is_raster());
                if tiles.is_none() && !raster {
                    let why = tl!("Select a raster or tile layer in the Layers pane first").to_string();
                    self.session.notify(why.clone(), true);
                    return Err(why);
                }
                if let Some(path) = dialogs::pick_save(&name, &["tif", "tiff"], tl!("GeoTIFF")) {
                    let params = match tile.filter(|_| tiles.is_some()) {
                        Some(i) => json!({"path": path, "tiles": i}),
                        None => json!({"path": path}),
                    };
                    // Written on a thread of its own, with progress and Cancel in the status bar.
                    #[cfg(not(target_arch = "wasm32"))]
                    return exportjob::start(self, "share.exportRaster", params);
                    #[cfg(target_arch = "wasm32")]
                    return self.run("share.exportRaster", params);
                }
            }
            "ui.exportMap" => {
                let name = format!("{}.png", self.session.map().name);
                // A .tif is the view placed on the ground (a GeoTIFF), without the scale bar.
                if let Some(path) = dialogs::pick_save(&name, &["png", "tif", "tiff"], tl!("PNG image or GeoTIFF")) {
                    // 192 DPI, or the most a view too large for it holds (the export says so).
                    let args = json!({"path": path, "dpi": 192, "fit": true});
                    // Drawn on a thread of its own, with progress and Cancel in the status bar;
                    // here while a style added with its own view loads, as the command waits for
                    // it to move the map there first.
                    #[cfg(not(target_arch = "wasm32"))]
                    if self.session.style_view.is_none() {
                        return exportjob::start(self, "share.exportMap", args);
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    exportjob::idle(self)?;
                    return self.run("share.exportMap", args);
                }
            }
            "ui.exportLayer" => {
                let ext = p.get("format").and_then(Value::as_str).unwrap_or("geojson").to_ascii_lowercase();
                let label = dialogs::EXPORT_FORMATS
                    .iter()
                    .find(|(_, e)| *e == ext)
                    .map(|(l, _)| *l)
                    .ok_or_else(|| format!("unknown format '{ext}'"))?;
                // Into a GeoPackage that exists: picked to open, as it is kept, not replaced.
                // Checked as the command checks them, before a file is asked for; then passed on.
                let gpkg = rustgis_engine::cmd::share::gpkg_table(p, &ext)?;
                let id = self.export_target(p)?;
                let with_table = |mut args: Value| {
                    for key in ["mode", "tableName"] {
                        if let Some(v) = p.get(key) {
                            args[key] = v.clone();
                        }
                    }
                    args
                };
                if gpkg.mode != rustgis_engine::data::io::GpkgMode::NewFile {
                    if let Some(path) = dialogs::pick_open(&["gpkg"], label) {
                        let args = with_table(json!({"layer": id, "path": path}));
                        #[cfg(not(target_arch = "wasm32"))]
                        return exportjob::start(self, "share.exportLayer", args);
                        #[cfg(target_arch = "wasm32")]
                        return self.run("share.exportLayer", args);
                    }
                    return Ok(Value::Null);
                }
                let name = self.session.map().layer(id).map(|l| format!("{}.{ext}", l.name)).unwrap_or_default();
                if let Some(path) = dialogs::pick_save(&name, &[ext.as_str()], label) {
                    // The format chosen decides: its extension is added unless it was typed (a dot
                    // in the name, as in "U.S. Cities", isn't taken for one).
                    let typed = std::path::Path::new(&path).extension().is_some_and(|e| e.eq_ignore_ascii_case(&ext));
                    let path = if typed { path } else { format!("{path}.{ext}") };
                    // Written on a thread of its own, with progress and Cancel in the status bar.
                    let args = with_table(json!({"layer": id, "path": path}));
                    #[cfg(not(target_arch = "wasm32"))]
                    return exportjob::start(self, "share.exportLayer", args);
                    #[cfg(target_arch = "wasm32")]
                    return self.run("share.exportLayer", args);
                }
            }
            // Add Zarr Variable, connected to a store (a link, a script).
            "ui.addZarr" => {
                self.dialogs.open("zarr", &self.session)?;
                if let Some(a) = p.get("address").and_then(Value::as_str) {
                    self.ui.zarr.address = a.to_string();
                    self.ui.zarr.title = None;
                    zarrui::connect(self);
                }
            }
            // Edit › Dynamic Text: the text tool, the text it places starting with one of the map's
            // values.
            "ui.dynamicText" => {
                let text = p.get("text").and_then(Value::as_str).unwrap_or("");
                let token = rustgis_engine::map::graphics::TOKENS
                    .iter()
                    .map(|(t, _)| *t)
                    .find(|t| *t == text)
                    .ok_or_else(|| format!("no dynamic text '{text}' (one of {{map}}, {{scale}}, {{date}}, …)"))?;
                self.run("tool.set", json!({"tool": "graphicText"}))?;
                self.ui.dynamic_text = Some(token);
            }
            // A text graphic has no dialog any more (#516): by its layer and row it opens in the
            // Format pane; at `lon`/`lat` it is placed there (with `text`, else "Text") and opens.
            "ui.dialog" if p.get("name").and_then(Value::as_str) == Some("graphicText") => {
                let num = |k: &str| p.get(k).and_then(Value::as_f64);
                match (p.get("layer").and_then(Value::as_u64), p.get("row").and_then(Value::as_u64), num("lon"), num("lat")) {
                    (Some(layer), Some(row), ..) => {
                        let (layer, row) = (u32::try_from(layer).unwrap_or(u32::MAX), usize::try_from(row).unwrap_or(usize::MAX));
                        graphicformat::edit(self, layer, row)?;
                    }
                    (.., Some(lon), Some(lat)) => {
                        let text: Option<String> = p.get("text").and_then(Value::as_str).map(|t| t.chars().take(2000).collect());
                        graphicformat::place(self, lon, lat, text.as_deref())?;
                    }
                    _ => return Err("give the text's layer and row, or the lon and lat to place one".into()),
                }
            }
            "ui.dialog" => {
                let which = p.get("name").and_then(Value::as_str).unwrap_or("");
                if which == "calculateGeometry" {
                    let id = self.session.layer_id(p)?;
                    let form = geometrycalc::Form::new(&self.session, id)?;
                    self.dialogs = dialogs::Dialogs { open: Some("calculateGeometry"), geometry: Some(form), ..Default::default() };
                    return Ok(Value::Null);
                }
                if matches!(which, "repairSource" | "repairTiles") {
                    let form = (|| {
                        let target = if which == "repairSource" {
                            repairsource::Target::Layer(self.session.layer_id(p)?)
                        } else {
                            let tiles = &self.session.map().tiles;
                            let index = if let Some(index) = p.get("index") {
                                index
                                    .as_u64()
                                    .and_then(|i| usize::try_from(i).ok())
                                    .ok_or_else(|| tl!("Give a valid tile archive index.").to_string())?
                            } else {
                                tiles
                                    .iter()
                                    .position(|t| t.archive_file().is_some() && t.error.is_some())
                                    .or_else(|| tiles.iter().position(|t| t.archive_file().is_some()))
                                    .ok_or_else(|| tl!("The map has no tile archive read from a file.").to_string())?
                            };
                            repairsource::Target::Tiles(index)
                        };
                        repairsource::Form::new(&self.session, target)
                    })();
                    let form = match form {
                        Ok(form) => form,
                        Err(e) => {
                            self.session.notify(e.clone(), true);
                            return Err(e);
                        }
                    };
                    self.dialogs = dialogs::Dialogs { open: Some("repairSource"), repair: Some(form), ..Default::default() };
                    return Ok(Value::Null);
                }
                // A Web Data service opens in the right pane, beside the map it searches.
                if let Some(name) = panes::web_data_name(which) {
                    // A Web Data panel opened on a search.
                    if let Some(words) = p.get("search").and_then(Value::as_str) {
                        webdataui::set_search(self, which, words);
                    }
                    // Natural Earth is Source Cooperative's browser locked to its product; Source
                    // Cooperative opened after it starts from the products again.
                    // A panel reopened asks again for what failed.
                    webdataui::forget_failed(self);
                    if which == "naturalEarth" {
                        catalogsui::natural_earth(self);
                    } else if which == "sourcecoop" && self.ui.catalogs.locked {
                        catalogsui::unlock(self);
                    }
                    self.ui.web_data = name;
                    self.ui.open_right(RightPane::WebData);
                    // Satellite Embeddings opens on a dataset, when given one.
                    if name == "embeddings"
                        && let Some(d) = p.get("dataset").and_then(Value::as_str)
                    {
                        embeddingsui::pick(self, d)?;
                    }
                    return Ok(Value::Null);
                }
                self.dialogs.open(which, &self.session)?;
                // Rename Map / Rename Layout remember which one (`target`, by name; default the
                // one in view), so renaming another from the Browser leaves the view alone.
                if matches!(which, "renameMap" | "renameLayout") {
                    let target = p.get("target").and_then(Value::as_str);
                    let pr = &self.session.project;
                    let found = if which == "renameMap" {
                        match target {
                            Some(n) => pr.maps.iter().find(|m| m.name.eq_ignore_ascii_case(n)),
                            None => pr.maps.get(pr.active_map),
                        }
                        .map(|m| m.name.clone())
                    } else {
                        match target {
                            Some(n) => pr.layouts.iter().find(|l| l.name.eq_ignore_ascii_case(n)),
                            None => self.session.active_layout.and_then(|i| pr.layouts.get(i)),
                        }
                        .map(|l| l.name.clone())
                    };
                    let Some(name) = found else {
                        self.dialogs.open = None;
                        // With no target, only Rename Layout can miss: no layout is open.
                        return Err(match target {
                            Some(n) => i18n::fmt("Nothing named {name} to rename", &[("name", n)]),
                            None => tl!("open a layout first").to_string(),
                        });
                    };
                    self.dialogs.a = name.clone();
                    self.dialogs.text = name;
                }
            }
            // Picks the image for a picture graphic whose corners were just dragged.
            "ui.addPicture" => {
                if self.ui.pending_picture.is_none() {
                    return Err("drag the picture's rectangle on the map first".into());
                }
                if let Some(open) = &self.services.open_async {
                    self.ui.pending_layout_picture = false;
                    self.ui.pending_open = None;
                    open("picture");
                } else if let Some(path) = dialogs::pick_open(&["png", "jpg", "jpeg"], tl!("Pictures")) {
                    return self.add_picture(json!({"path": path}));
                } else {
                    self.ui.pending_picture = None;
                }
            }
            "ui.quit" => self.quit_requested = true,
            // Buttons that act through their own click handler (menus, layout inserts).
            "ui.menu" => {}
            other => return Err(format!("unknown command '{other}'")),
        }
        Ok(Value::Null)
    }

    /// Adds the data downloads that have landed (Home › Add › Add Data › From URL). The bytes go to the session
    /// as they are; a download of a project that has since been closed is dropped.
    fn poll_downloads(&mut self) {
        let landed = self.ui.project_download.as_ref().and_then(|(id, url, slot)| {
            let r = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
            r.map(|r| (*id, url.clone(), r))
        });
        if let Some((id, url, r)) = landed {
            self.ui.project_download = None;
            // Another project opened while this one downloaded: it stays.
            if !self.session.download_done(id) {
                if r.is_ok() {
                    self.session.notify(
                        i18n::fmt("{url}: downloaded after another project was opened, so it wasn't opened", &[("url", &url)]),
                        false,
                    );
                }
                return self.poll_data_downloads();
            }
            if let Err(e) = r.and_then(|bytes| self.open_project_bytes(project_file_name(&url), &bytes)) {
                self.session.notify(if e.starts_with(&url) { e } else { format!("{url}: {e}") }, true);
            }
        }
        self.poll_data_downloads();
    }

    /// Opens a downloaded project: a `.rgispkg` package, or project JSON named after its file (or
    /// keeping its own name when the address has none). Unlike [`Self::open_bytes`], it never takes
    /// a picker's pending item: the download isn't the file the user is picking.
    fn open_project_bytes(&mut self, file: Option<&str>, bytes: &[u8]) -> Result<Value, String> {
        if file.is_some_and(rustgis_engine::map::geolibre::is_project_file) {
            let text = std::str::from_utf8(bytes).map_err(|_| tl!("Not a GeoLibre project").to_string())?;
            let result = self.run("project.importGeolibre", json!({"text":text}));
            self.map.invalidate();
            return result;
        }
        let package = file.is_some_and(|f| f.to_ascii_lowercase().ends_with(".rgispkg"));
        let text = if package {
            rustgis_engine::cmd::items::package_text(bytes)?
        } else {
            std::str::from_utf8(bytes).map_err(|_| "not a RustGIS project".to_string())?.to_string()
        };
        let mut p = json!({"text": text, "package": package});
        if let Some(f) = file.filter(|_| !package) {
            p["fileName"] = json!(f);
        }
        let r = self.run("project.openText", p);
        if r.is_ok() && !package {
            self.ui.web_file = Some(format!("{}.rgis", self.session.project.name));
        }
        self.map.invalidate();
        r
    }

    /// Consumes completed probes and downloads on the app frame, finalizing imports in their
    /// reserved map or discarding failed reservations even when another map is active.
    /// A native dialog or an engine-issued deferred request. The claim rides outside JSON.
    fn add_url(&mut self, p: &Value, request: Option<&rustgis_engine::UrlRequest>, existing: Option<u64>) -> Result<Value, String> {
        let claim = match request {
            Some(request) => Some(self.session.download_started_for(request)?),
            None => existing,
        };
        if let Some(id) = claim {
            self.session.download_continues(id)?;
        }
        let still = claim.and_then(|id| self.session.download_still(id));
        rustgis_engine::render::rustgis_tiles::cache::with_optional_still(still, || {
            if p.get("geolibreZarr").and_then(Value::as_bool) == Some(true) {
                if let Err(e) = geolibreui::start(self, p, claim) {
                    rustgis_engine::cmd::geolibre::discard_pending(&mut self.session, p);
                    if let Some(id) = claim {
                        self.session.download_failed(id);
                    }
                    return Err(e);
                }
                return Ok(Value::Null);
            }
            // The connection a Connections tree add is for (named in Recent once it is added).
            let origin = connectiontree::take_origin(self);
            let Some(raw_url) = p.get("url").and_then(Value::as_str).filter(|u| !u.trim().is_empty()) else {
                return self.run("ui.dialog", json!({"name": "addUrl"}));
            };
            let url = raw_url.trim();
            // Claimed work keeps its exact admitted address and connection. Only direct native
            // UI ingress chooses a login here; a continuation never acquires a new default.
            let bound = if claim.is_some() {
                std::borrow::Cow::Borrowed(p)
            } else {
                let mut normalized = std::borrow::Cow::Borrowed(p);
                if url.len() != raw_url.len() {
                    normalized.to_mut()["url"] = json!(url);
                }
                std::borrow::Cow::Owned(
                    rustgis_engine::service_logins::request_params(&mut self.session, normalized.as_ref())
                        .map_err(|e| connectionui::translated(&e))?
                        .into_owned(),
                )
            };
            let p = bound.as_ref();
            let url = rustgis_engine::data::login_address::inner(p.get("url").and_then(Value::as_str).unwrap_or(url));
            // A window of a cloud-optimised GeoTIFF is read off the frame (below), with no
            // probe: it is refused unless it is one. In the browser the command refuses it.
            let window = cfg!(not(target_arch = "wasm32")) && p.get("bbox").is_some_and(|v| !v.is_null());
            if !window && url.to_ascii_lowercase().split(['?', '#']).next().is_some_and(|u| u.ends_with(".pmtiles")) {
                let before = self.session.sources.generation;
                let result = if let Some(id) = claim {
                    self.run_with_prepared("layer.addUrl", p.clone(), AppDispatch::InPlace(id))
                } else {
                    self.run("layer.addUrl", p.clone())
                };
                if result.is_ok() {
                    connectiontree::name_recent(self, before, origin.as_deref());
                } else if let Err(error) = &result {
                    self.session.request_failed(p, error);
                }
                return result;
            }
            let address = p.get("url").and_then(Value::as_str).unwrap_or(url).to_string();
            // A GeoTIFF, FlatGeobuf or GeoParquet: its header (or footer) first, off the
            // frame; one that can be read in place is, another is downloaded (see
            // `poll_downloads`). An address without a file name (an API answering with a
            // file, such as Vantor's `…/geotiff?…`) is probed as a GeoTIFF too, and downloaded
            // as before when it doesn't answer as one.
            let remote = rustgis_engine::data::source::is_remote_name(url);
            let tiff = rustgis_engine::cmd::layer::is_tiff_url(url);
            // A scan (`"as"`, a plugin's or a script's) keeps only the pixels: downloaded
            // whole.
            let scan = p.get("as").and_then(Value::as_str) == Some("scan");
            let unnamed = !remote
                && !tiff
                && rustgis_engine::cmd::layer::is_unnamed_url(url)
                && ["service", "coverage"].iter().all(|k| p.get(*k).and_then(Value::as_bool) != Some(true));
            if !window && (remote || tiff || unnamed) && !scan && p.get("whole").and_then(Value::as_bool) != Some(true) {
                let slot: Slot<Result<bool, String>> = Slot::default();
                let land = slot.clone();
                if remote {
                    rustgis_engine::data::source::open_url(
                        &address,
                        Box::new(move |r| {
                            let r = match r {
                                Ok(_) => Ok(true),
                                Err(e) if e.to_string().contains(rustgis_engine::data::source::remote::READ_WHOLE) => Ok(false),
                                Err(e) => Err(e.to_string()),
                            };
                            *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                        }),
                    );
                } else {
                    rustgis_engine::render::rustgis_tiles::cog::probe(
                        &address,
                        Box::new(move |r| {
                            let r = if unnamed { Ok(r.unwrap_or(false)) } else { r };
                            *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                        }),
                    );
                }
                // An address read through a saved S3 connection is said as the address it wraps.
                let shown = rustgis_engine::data::s3_address::inner(url);
                self.session.notify(i18n::fmt("Reading {url}…", &[("url", shown)]), false);
                let mut probe = p.clone();
                probe["url"] = json!(address);
                if let Some(o) = probe.as_object_mut() {
                    o.remove(connectiontree::ORIGIN);
                    if let Some(origin) = &origin {
                        o.insert(connectiontree::ORIGIN.into(), json!(origin));
                    }
                }
                self.ui.cog_probes.push((probe, claim, slot));
                return Ok(Value::Null);
            }
            let url = address;
            // The request receipt is correlation only; its admitted claim stays outside JSON.
            let mut q = json!({"url": url});
            for k in ["name", "layer", "crs", "connection"] {
                if let Some(v) = p.get(k).filter(|v| v.as_str().is_some_and(|s| !s.trim().is_empty())) {
                    q[k] = v.clone();
                }
            }
            // How the bytes are read, as `layer.addData` takes it (a command asking by
            // address, such as a plugin's: the dialog gives none of these).
            for k in ["x", "y", "wkt", "delimiter", "paperSpace", "stream", "as"] {
                if let Some(v) = p.get(k).filter(|v| !v.is_null()) {
                    q[k] = v.clone();
                }
            }
            if claim.is_some() {
                // A claimed request keeps its original JSON value through completion.
                if let Some(zoom) = p.get("zoom") {
                    q["zoom"] = zoom.clone();
                }
            } else if let Some(zoom) = p.get("zoom").filter(|v| !v.is_null()) {
                // Read as the engine reads a true/false parameter; a value it can't read goes on
                // as given, for the command adding the download to read as false and say so (#1600).
                q["zoom"] = rustgis_engine::cmd::bool_value(zoom).map_or_else(|| zoom.clone(), |z| json!(z));
            }
            if let Some(number) = p.get("request").filter(|v| v.is_u64()) {
                q["request"] = number.clone();
            }
            for k in ["service", "coverage", "anonymous"] {
                if p.get(k).and_then(Value::as_bool) == Some(true) {
                    q[k] = json!(true);
                }
            }
            for key in ["geolibreProject", "geolibreLayer"] {
                if let Some(value) = p.get(key) {
                    q[key] = value.clone();
                }
            }
            // A service layer's bounds, which its features' axes are checked against.
            if let Some(b) = p.get("bounds").filter(|b| b.is_array()) {
                q["bounds"] = b.clone();
            }
            if let Some(origin) = &origin {
                q[connectiontree::ORIGIN] = json!(origin);
            }
            #[cfg(not(target_arch = "wasm32"))]
            if window {
                return self.start_window_read(p, url, q, claim);
            }
            let shown = rustgis_engine::data::s3_address::inner(&url);
            self.session.notify(i18n::fmt("Downloading {url}…", &[("url", shown)]), false);
            let id = claim.unwrap_or_else(|| self.session.download_started());
            // A service's features: read page by page from the start, off the frame (an ArcGIS
            // layer's description first).
            if q.get("service").and_then(Value::as_bool) == Some(true) {
                let origin = connectiontree::take_origin_key(&mut q);
                serviceui::read_pages(self, serviceui::Landed { id, url, params: q, origin });
                return Ok(Value::Null);
            }
            let slot: Slot<Result<Vec<u8>, String>> = Slot::default();
            let land = slot.clone();
            // A Shapefile address brings its companion parts under the same download owner.
            rustgis_engine::cmd::layer::download_data(
                &url,
                if q.get("geolibreProject").and_then(Value::as_bool) == Some(true) {
                    rustgis_engine::map::geolibre::MAX_TEXT
                } else {
                    rustgis_engine::render::rustgis_tiles::cache::MAX_DOWNLOAD
                },
                Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)),
            );
            self.ui.downloads.push((id, url, q, slot));

            Ok(Value::Null)
        })
    }

    fn poll_data_downloads(&mut self) {
        // GeoTIFFs, FlatGeobufs and GeoParquets whose headers are in: read in place, or
        // downloaded whole.
        let mut probed = Vec::new();
        self.ui.cog_probes.retain(|(p, claim, slot)| match slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
            Some(r) => {
                probed.push((p.clone(), *claim, r));
                false
            }
            None => true,
        });
        for (mut p, claim, r) in probed {
            if let Some(id) = claim
                && let Err(e) = self.session.download_continues(id)
            {
                self.session.request_failed(&p, &e);
                self.landed_in_dialog(&p, Err(&e));
                self.session.notify(e, true);
                continue;
            }
            let origin = connectiontree::take_origin_key(&mut p);
            let url = p.get("url").and_then(Value::as_str).unwrap_or("").to_string();
            match r {
                Ok(true) => {
                    // A GeoTIFF by its first bytes, whatever its address is named.
                    if !rustgis_engine::data::source::is_remote_name(&url) {
                        p["cog"] = json!(true);
                    }
                    let before = self.session.sources.generation;
                    let pending = p.clone();
                    let added = if let Some(id) = claim {
                        self.run_with_prepared("layer.addUrl", p, AppDispatch::InPlace(id))
                    } else {
                        self.run("layer.addUrl", p)
                    };
                    match added {
                        Ok(_) => {
                            connectiontree::name_recent(self, before, origin.as_deref());
                            self.map.invalidate();
                            self.landed_in_dialog(&pending, Ok(()));
                        }
                        Err(e) => {
                            self.session.request_failed(&pending, &e);
                            self.landed_in_dialog(&pending, Err(&e));
                        }
                    }
                }
                Ok(false) => {
                    p["whole"] = json!(true);
                    let pending = p.clone();
                    // Downloaded whole: the line waits for the download (`add_download`).
                    let notice_mark = self.session.notice_mark();
                    let started = if claim.is_some() {
                        self.add_url(&p, None, claim)
                    } else {
                        connectiontree::run_add_url(self, p, origin.as_deref())
                    };
                    if let Err(e) = started {
                        self.session.request_failed(&pending, &e);
                        self.landed_in_dialog(&pending, Err(&e));
                        self.command_failed("layer.addUrl", &e, notice_mark);
                    }
                }
                Err(e) => {
                    // A waiting command's error names the address, as the notice does.
                    // Redacted whole, as the notice is: a key with a space in it isn't left half there.
                    let e = if e.starts_with(&url) { e } else { format!("{url}: {e}") };
                    let e = rustgis_engine::Session::redacted(&e).into_string();
                    self.session.request_failed(&p, &e);
                    if let Some(id) = claim {
                        self.session.download_failed(id);
                    }
                    self.landed_in_dialog(&p, Err(&e));
                    self.session.notify(e, true);
                }
            }
        }
        let mut landed = Vec::new();
        self.ui.downloads.retain(|(id, url, p, slot)| match slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
            Some(r) => {
                landed.push((*id, url.clone(), p.clone(), r));
                false
            }
            None => true,
        });
        for (id, url, mut p, r) in landed {
            let origin = connectiontree::take_origin_key(&mut p);
            match r {
                Ok(bytes) => self.add_landed(serviceui::Landed { id, url, params: p, origin }, bytes),
                Err(e) => self.landed_failed(id, &url, &p, e),
            }
        }
        // Windows read on threads, added as a download is.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut read = Vec::new();
            let session = &self.session;
            self.ui.window_reads.retain(|(id, url, p, slot, left)| match slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
                Some(r) => {
                    read.push((*id, url.clone(), p.clone(), r));
                    false
                }
                None => {
                    left.store(!session.download_under_way(*id), std::sync::atomic::Ordering::Relaxed);
                    true
                }
            });
            for (id, url, mut p, r) in read {
                let origin = connectiontree::take_origin_key(&mut p);
                match r {
                    Ok(window) => {
                        let current = self.session.download_landed_window(id, window);
                        self.add_download(serviceui::Landed { id, url, params: p, origin }, current);
                    }
                    Err(e) => self.landed_failed(id, &url, &p, e),
                }
            }
        }
    }

    /// Says why a download or a window's read failed, and answers the command waiting for it.
    fn landed_failed(&mut self, id: u64, url: &str, p: &Value, e: String) {
        self.session.request_failed(p, &e);
        self.session.download_failed(id);
        rustgis_engine::cmd::geolibre::discard_pending(&mut self.session, p);
        // Named without the address's keys, token or signature.
        let e = e.strip_prefix(url).map(|r| r.trim_start_matches(": ").to_string()).unwrap_or(e);
        let why = rustgis_engine::sources::redact_text(&e);
        // An error that names its own address (a Shapefile's part) isn't named again.
        let named = why.split_once(": ").is_some_and(|(head, _)| head.contains("://") && !head.contains(' '));
        let why = if named { why } else { format!("{}: {why}", rustgis_engine::sources::redact(url)) };
        self.landed_in_dialog(p, Err(&why));
        self.session.notify(why, true);
    }

    /// Starts reading a window of a cloud-optimised GeoTIFF (`p`, a `ui.addUrl` with `"bbox"`) on a
    /// thread, to add it with `q` when it lands (#1330). The command line and scripts wait for it
    /// in `layer.addUrl` instead. What's wrong with what was asked is refused here, on the frame.
    #[cfg(not(target_arch = "wasm32"))]
    fn start_window_read(&mut self, p: &Value, url: String, mut q: Value, claim: Option<u64>) -> Result<Value, String> {
        let asked = rustgis_engine::cmd::layer::window_asked(&self.session, p).and_then(|asked| {
            if self.ui.window_reads.len() >= MAX_WINDOW_READS {
                return Err(i18n::fmt(
                    "{url}: {count} windows are being read already: ask again once one has been added",
                    &[("url", &rustgis_engine::sources::redact(&url)), ("count", &MAX_WINDOW_READS.to_string())],
                ));
            }
            Ok(asked)
        });
        // A refused window answers the command waiting for it and gives back the download it
        // was claimed under.
        let asked = asked.inspect_err(|e| {
            self.session.request_failed(p, e);
            if let Some(id) = claim {
                self.session.download_failed(id);
            }
        })?;
        // What a task needs to read it again, as the download itself can't be (`record_step`).
        let mut window = json!({});
        for k in ["bbox", "cellSize", "cog"] {
            if let Some(v) = p.get(k).filter(|v| !v.is_null()) {
                window[k] = v.clone();
            }
        }
        q["window"] = window;
        let slot: Slot<Result<rustgis_engine::cmd::layer::CogWindow, String>> = Slot::default();
        let land = slot.clone();
        let left = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let leaving = left.clone();
        // Its requests stay in the session's diagnostics.
        let diagnostics = self.session.diagnostics.capture();
        let spawned = std::thread::Builder::new().name("cog-window".into()).spawn(move || {
            let _diagnostics = diagnostics.enter();
            let turn = window_turn(&leaving);
            let read = if turn.is_some() {
                // On a thread no command's catch reaches: a bug must not leave it running forever.
                let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rustgis_engine::cmd::layer::read_cog_window(&asked)));
                read.unwrap_or_else(|_| Err(tl!("Reading the window failed unexpectedly").to_string()))
            } else {
                Err(tl!("given up before its read began: its project was closed, or more than 64 downloads started after it").to_string())
            };
            *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(read);
            // The next read waits for the frame to take this one, or for the app to be gone.
            while std::sync::Arc::strong_count(&land) > 1 && land.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            drop(turn);
        });
        if let Err(e) = spawned {
            // Answered and given back as a refused window is.
            let e = i18n::fmt(
                "{url}: its read didn't start: {error}",
                &[("url", &rustgis_engine::sources::redact(&url)), ("error", &e.to_string())],
            );
            self.session.request_failed(p, &e);
            if let Some(id) = claim {
                self.session.download_failed(id);
            }
            return Err(e);
        }
        // Named redacted, and by the address an S3 connection's wraps.
        let shown = rustgis_engine::sources::redact(rustgis_engine::data::s3_address::inner(&url));
        self.session.notify(i18n::fmt("Reading {url}…", &[("url", &shown)]), false);
        let id = claim.unwrap_or_else(|| self.session.download_started());
        self.ui.window_reads.push((id, url, q, slot, left));
        Ok(Value::Null)
    }

    /// Adds a download that landed (`layer.addDownload`), named in Recent after the connection it
    /// came from; one whose project was closed meanwhile is said and dropped.
    pub(crate) fn add_landed(&mut self, landed: serviceui::Landed, bytes: Vec<u8>) {
        let current = self.session.download_landed(landed.id, bytes);
        self.add_download(landed, current);
    }

    /// Adds a service layer's features that landed read already (its protocol buffer pages,
    /// joined), as [`Self::add_landed`] adds a download.
    pub(crate) fn add_landed_read(
        &mut self,
        landed: serviceui::Landed,
        dataset: rustgis_engine::data::Dataset,
        report: rustgis_engine::data::ReadReport,
    ) {
        let current = self.session.download_landed_read(landed.id, dataset, report);
        self.add_download(landed, current);
    }

    /// Adds a download handed to the session (`current`: it still belongs to this project).
    fn add_download(&mut self, landed: serviceui::Landed, current: bool) {
        let serviceui::Landed { id, url, params: mut p, origin } = landed;
        if !current {
            let shown = serviceui::address_in_message(&url);
            let why = i18n::fmt("{url}: downloaded after its project was closed, so it wasn't added", &[("url", &shown)]);
            self.session.request_failed(&p, &why);
            self.landed_in_dialog(&p, Err(&why));
            self.session.notify(why, false);
            return;
        }
        // Named redacted: a notice with a key in it loses what follows it.
        let shown = rustgis_engine::sources::redact(&url);
        p["id"] = json!(id);
        p["url"] = json!(url);
        let zoom = p.get("zoom").and_then(rustgis_engine::cmd::bool_value) == Some(true);
        if zoom && self.ui.link_zoomed {
            p["zoom"] = json!(false);
        }
        let before = self.session.sources.generation;
        let pending = p.clone();
        match self.run_with_prepared("layer.addDownload", p, AppDispatch::Download(id)) {
            Ok(r) => {
                connectiontree::name_recent(self, before, origin.as_deref());
                if r.get("zoomed").and_then(Value::as_bool) == Some(true) {
                    self.ui.link_zoomed = true;
                }
                self.map.invalidate();
                self.landed_in_dialog(&pending, Ok(()));
            }
            Err(e) => {
                self.session.request_failed(&pending, &e);
                rustgis_engine::cmd::geolibre::discard_pending(&mut self.session, &pending);
                // Running it said the error already: again only to name the address it lacks. An
                // error may name it redacted.
                let named = e.starts_with(&url) || e.starts_with(&shown);
                let e = if named { e } else { format!("{shown}: {e}") };
                self.landed_in_dialog(&pending, Err(&e));
            }
        }
    }

    /// Says in the add-service dialog's line how a download it showed on its way went (#1453):
    /// the layer added, or why not, scrubbed and redacted as the notices are (a login's address or
    /// a token in the URL never shows). Only while the line still says that layer is downloading,
    /// so a line another dialog or a later add has taken over is left alone; the notices say it
    /// either way.
    pub(crate) fn landed_in_dialog(&mut self, params: &Value, r: Result<(), &str>) {
        let Some(name) = params.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()) else {
            return;
        };
        if self.dialogs.info.as_deref() != Some(serviceui::downloading(name).as_str()) {
            return;
        }
        match r {
            // As a layer added at once says it.
            Ok(()) => {
                self.dialogs.info = Some(serviceui::added(name));
                self.dialogs.error = None;
            }
            Err(e) => {
                self.dialogs.info = None;
                self.dialogs.error = Some(connectionui::translated(rustgis_engine::Session::redacted(e).as_str()));
            }
        }
    }

    /// Reports query losses once when discovery finishes, including a refused next document.
    fn notice_service_query_loss(&mut self, count: u32) {
        if count > 0 {
            self.session.notify(i18n::fmt(rustgis_engine::data::ogc::QUERY_WITHHELD_NOTICE, &[("n", &count.to_string())]), false);
        }
    }

    /// Tells how a service layer added outside the add-service dialog (or in it) went: in the
    /// dialog's own line, or as a notice for the Connections tree.
    /// `shown` is the line the step put in the dialog: the dialog is told only while it still
    /// shows that line (a failure clears it), so a late step never overwrites a newer add's line
    /// and is told as a notice instead.
    fn report_step(&mut self, dialog: bool, shown: Option<&str>, r: Result<String, String>) {
        let in_dialog = dialog && self.dialogs.open == Some("addService") && shown.is_some() && self.dialogs.info.as_deref() == shown;
        match (in_dialog, r) {
            (true, Ok(m)) => self.dialogs.info = Some(m),
            (true, Err(e)) => {
                self.dialogs.info = None;
                self.dialogs.error = Some(connectionui::translated(&e));
            }
            (false, Ok(m)) => self.session.notify(m, false),
            (false, Err(e)) => self.session.notify(connectionui::translated(&e), true),
        }
    }

    /// Refuses answers from a replaced project and reports the retired read without starting follow-up work.
    fn service_read_current(&mut self, opened: u64, address: &str, pending: Option<&str>) -> bool {
        if opened == self.session.opened {
            return true;
        }
        let url = serviceui::address_in_message(address);
        let why = i18n::fmt("{url}: its project was closed while it was read, so it wasn't added", &[("url", &url)]);
        if pending.is_some() && self.dialogs.info.as_deref() == pending {
            self.dialogs.info = None;
        }
        self.session.notify(why, false);
        false
    }

    /// Moves the service layers waiting on a document along: an OGC API layer's tile template
    /// (read from its tileset list, then the tileset when the list doesn't carry it; the layer is
    /// added with the template read, or the usual path) and a WCS coverage's description (the
    /// coverage is then asked for the view and downloaded as From URL… does).
    fn poll_service_steps(&mut self) {
        use rustgis_engine::data::ogc::{TilesStep, resolve_tiles};
        let steps = std::mem::take(&mut self.ui.service_steps);
        // A read in the new project can display the same title as a retired read. Preserve
        // progress belonging to that live generation, even before its answer has landed.
        let current: std::collections::HashSet<String> = steps
            .iter()
            .filter(|step| match step {
                ServiceStep::Tiles { opened, .. } | ServiceStep::Coverage { opened, .. } => *opened == self.session.opened,
            })
            .filter_map(ServiceStep::pending_message)
            .collect();
        for step in steps {
            let pending = step.pending_message();
            match step {
                ServiceStep::Tiles { mut resolve, dialog, origin, opened } => {
                    if !self.service_read_current(opened, &resolve.doc, pending.as_deref().filter(|message| !current.contains(*message))) {
                        continue;
                    }
                    let Some(r) = resolve.slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
                        self.ui.service_steps.push(ServiceStep::Tiles { resolve, dialog, origin, opened });
                        continue;
                    };
                    let next = r.ok().and_then(|body| resolve_tiles(&resolve.list, &resolve.doc, &body, resolve.vector));
                    let next = next.map(|resolved| {
                        resolve.withheld_addresses = resolve.withheld_addresses.saturating_add(resolved.withheld_addresses);
                        resolved.step
                    });
                    let template = match next {
                        Some(TilesStep::Next(next)) if resolve.read < 2 => {
                            resolve.read += 1;
                            // The resolver already carried the current document's binding, or
                            // its absence. A saved connection must not restore it after a hop.
                            match rustgis_engine::render::rustgis_tiles::address::check(&next, Some(&resolve.doc)) {
                                Ok(()) => {
                                    resolve.crossed_origin |= rustgis_engine::render::rustgis_tiles::logins::origin(&resolve.doc)
                                        != rustgis_engine::render::rustgis_tiles::logins::origin(&next);
                                    resolve.doc = next;
                                    resolve.slot = fetch_into(&resolve.doc);
                                }
                                Err(e) => {
                                    self.notice_service_query_loss(resolve.withheld_addresses);
                                    self.report_step(dialog, pending.as_deref(), Err(e));
                                    continue;
                                }
                            }
                            self.ui.service_steps.push(ServiceStep::Tiles { resolve, dialog, origin, opened });
                            continue;
                        }
                        Some(TilesStep::Template(t)) => t,
                        _ => rustgis_engine::render::rustgis_tiles::logins::inherit(
                            &resolve.doc,
                            rustgis_engine::render::rustgis_tiles::logins::inner(&resolve.fallback),
                        ),
                    };
                    resolve.crossed_origin |= rustgis_engine::render::rustgis_tiles::logins::origin(&resolve.doc)
                        != rustgis_engine::render::rustgis_tiles::logins::origin(&template);
                    self.notice_service_query_loss(resolve.withheld_addresses);
                    let mut p = resolve.params;
                    p["tiles"] = json!(template);
                    let title = p["name"].as_str().unwrap_or("").to_string();
                    let before = self.session.sources.generation;
                    let r = self.run_with_prepared(
                        "service.add",
                        p,
                        AppDispatch::ResolvedTiles { template, crossed_origin: resolve.crossed_origin },
                    );
                    if r.is_ok() {
                        connectiontree::name_recent(self, before, origin.as_deref());
                    }
                    let r = r.map(|_| serviceui::added(&title));
                    self.report_step(dialog, pending.as_deref(), r);
                    self.map.invalidate();
                }
                ServiceStep::Coverage { mut params, slot, dialog, origin, opened } => {
                    if !self.service_read_current(
                        opened,
                        params.get("url").and_then(Value::as_str).unwrap_or(""),
                        pending.as_deref().filter(|message| !current.contains(*message)),
                    ) {
                        continue;
                    }
                    let Some(r) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
                        self.ui.service_steps.push(ServiceStep::Coverage { params, slot, dialog, origin, opened });
                        continue;
                    };
                    let title = params["name"].as_str().unwrap_or("").to_string();
                    let r = r.and_then(|bytes| {
                        params["describe"] = json!(String::from_utf8_lossy(&bytes));
                        // Not `self.run`: an error belongs where the step reports, not the notices.
                        let c = self.session.run("service.coverage", &params)?;
                        let connection = params.as_object_mut().and_then(|p| p.remove("connection"));
                        let get = c.get("url").and_then(Value::as_str).unwrap_or("").to_string();
                        let mut p = json!({"url": get, "name": title, "coverage": true});
                        if params.get("anonymous").and_then(Value::as_bool) == Some(true)
                            || c.get("anonymous").and_then(Value::as_bool) == Some(true)
                        {
                            p["anonymous"] = json!(true);
                        } else if let Some(connection) = connection {
                            p["connection"] = connection;
                        }
                        connectiontree::run_add_url(self, p, origin.as_deref())
                    });
                    self.report_step(dialog, pending.as_deref(), r.map(|_| serviceui::downloading(&title)));
                }
            }
        }
    }

    /// Lists a service's layers once its capabilities have arrived.
    fn poll_service(&mut self) {
        self.poll_service_steps();
        let Some(conn) = &mut self.ui.service else { return };
        let Some(r) = conn.slot.lock().unwrap_or_else(|e| e.into_inner()).take() else { return };
        let document = conn.pending.take().unwrap_or_else(|| conn.url.clone());
        match r {
            Ok(bytes) => {
                let text = match service_text(bytes) {
                    Ok(text) => text,
                    Err(_) => {
                        conn.error = Some(tl!("The service's answer isn't UTF-8 text").into());
                        return;
                    }
                };
                if conn.text.is_none() {
                    conn.text = Some(text);
                } else {
                    conn.documents.push(json!({"url": document, "text": text}));
                }
                let mut p = json!({"url": conn.url, "text": conn.text, "documents": conn.documents});
                if conn.kind != "auto" {
                    p["type"] = json!(conn.kind);
                }
                if let Some(connection) = &conn.connection {
                    p["connection"] = json!(connection);
                }
                // Not `self.run`: an error belongs in the dialog, not the notices.
                match self.session.run("service.layers", &p) {
                    Ok(v) => {
                        conn.pending = v.get("next").and_then(Value::as_str).map(str::to_string);
                        if let Some(next) = &conn.pending {
                            conn.slot = fetch_detailed_into(next);
                        }
                        conn.listing = Some(v);
                        conn.added = None;
                    }
                    Err(e) => conn.error = Some(e),
                }
            }
            Err(f) => conn.failure = Some(f),
        }
    }

    /// Per-frame work that doesn't draw (theme, fonts, files that arrived).
    pub fn logic(&mut self, ctx: &egui::Context) {
        let _diagnostics = self.session.diagnostics.enter();
        // A pass egui asks to repeat is the same frame: `ui` sets the delay once, from the first pass.
        if ctx.current_pass_index() == 0 {
            self.frame_start = Some(web_time::Instant::now());
        }
        // The MCP relay: what attached agents sent runs here, on this session, before the frame
        // draws (Session lives on this thread; the listener only queues messages).
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.frame_ctx = Some(ctx.clone());
            // Agents the user turned on last time: the relay starts with the app.
            connectui::resume(self);
            if let Some(relay) = &self.relay {
                // A relay started before the first frame has no wake yet; now there is a context
                // to ask for repaints with.
                if self.relay_wake_pending {
                    let ctx = ctx.clone();
                    relay.set_wake(Box::new(move || ctx.request_repaint()));
                    self.relay_wake_pending = false;
                }
                // A snapshot can capture the screen while the map is on it.
                relay.set_screen(relayshot::map_shown(self, ctx));
                // Only a request that changed the project ends a label move: a read-only one leaves it.
                let state = self.session.state();
                if relay.poll(&mut self.session) > 0 && self.session.state() != state {
                    self.map.cancel_label_drag();
                }
            }
            relayshot::drive(self, ctx);
            // Session plugins' programs: what they asked, and what changed on the map for them.
            if !self.plugin_wake_set {
                let ctx = ctx.clone();
                rustgis_engine::plugins::session::set_wake(std::sync::Arc::new(move || ctx.request_repaint()));
                self.plugin_wake_set = true;
            }
            let state = self.session.state();
            if self.session.poll_plugin_sessions() > 0 && self.session.state() != state {
                self.map.cancel_label_drag();
            }
            // A program that exits without a last line (something it left running holds its output
            // open) wakes nothing: while one runs, the app looks at least once a second.
            if !rustgis_engine::plugins::session::running().is_empty() {
                ctx.request_repaint_after(std::time::Duration::from_secs(1));
            }
            // A script an agent wrote to the file bridge.
            if let Some(bridge) = self.file_bridge.clone() {
                if !bridge.has_wake() {
                    let ctx = ctx.clone();
                    bridge.set_wake(Box::new(move || ctx.request_repaint()));
                }
                // A script may replace the features a label move is for: it ends the move.
                let ran = bridge.poll(&mut self.session);
                if ran.is_some() {
                    self.map.cancel_label_drag();
                }
                match ran {
                    Some(o) if o.success => self.session.notify(tl!("File bridge: the script ran").to_string(), false),
                    Some(o) => {
                        let text = i18n::fmt("File bridge: {error}", &[("error", o.error.as_deref().unwrap_or(""))]);
                        self.session.notify(text, true);
                    }
                    None => {}
                }
            }
        }
        // A project an agent or a script opened, before the frame draws the old one's fields.
        layoutview::follow_project(self);
        // A project's S3 sources waiting for a choice of saved connection: asked once.
        connectionui::follow_project(self);
        // Fonts installed last frame are active now.
        self.fonts_pending = false;
        i18n::set_current(self.lang());
        rustgis_engine::plugins::set_language(self.lang().code());
        panes::poll_geolibre(self, ctx);
        panes::poll_plugin_run(self, ctx);
        #[cfg(not(target_arch = "wasm32"))]
        panes::poll_tool_run(self, ctx);
        self.poll_downloads();
        let _ = self.deliver_gp_files(Ok(Value::Null));
        // A value picked under Dynamic Text lasts while the text tool does, however the tool
        // changes (Esc, another tool, a text placed without Keep Tool, a command).
        if self.session.tool != rustgis_engine::Tool::Graphic("text") {
            self.ui.dynamic_text = None;
        }
        pluginstart::run_next(self, ctx);
        // Addresses commands asked for where they couldn't download themselves (the browser):
        // fetched off the frame and added when they land, as Home › Add › Add Data › From URL does.
        for request in self.session.take_url_requests() {
            let notice_mark = self.session.notice_mark();
            if let Err(e) = self.add_url(request.params(), Some(&request), None) {
                self.command_failed("layer.addUrl", &e, notice_mark);
            }
        }
        // Plugin commands that waited for `host.fetch` answers (the browser) run again once
        // they are in; their errors are notices already.
        pluginstart::run_again(self, ctx);
        // A raster read in place whose overview couldn't be read is read again once its wait is over.
        self.session.read_in_place_again();
        // Rasters read in place whose pixels landed since (the browser) say what they lost.
        self.session.notice_landed_rasters();
        // A frame may not come until the user moves: look again while commands wait.
        if self.session.plugin_retries_waiting() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // A window's read lands after its last tile, with nothing to wake the frame.
        #[cfg(not(target_arch = "wasm32"))]
        if !self.ui.window_reads.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // Zarr steps the time slider moved to, read off the frame, show once they land.
        if self.session.follow_time_landed() {
            self.map.invalidate();
            // A layout page showing the map draws the step too.
            self.layout.redraw();
        }
        // A style added with `"view": true` moves the map once it has loaded.
        if self.session.style_view.is_some() {
            if self.session.follow_style_view() {
                self.map.invalidate();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        sqlpane::poll(self, ctx);
        self.poll_service();
        serviceui::poll(self, ctx);
        serviceui::poll_pages(self, ctx);
        // Sources the AI Assistant or a background add brought in (not through `run`) are kept
        // too: a generation compared each frame. So is a default raster style it set.
        sourcespane::save(self);
        sourcespane::save_connections(self);
        self.save_raster_defaults();
        self.save_layout_favourite();
        cswui::poll(self);
        stacui::poll(self, ctx);
        s3ui::poll(self, ctx);
        connectionui::poll(self, ctx);
        connectiontree::poll(self, ctx);
        portalsui::poll(self, ctx);
        catalogsui::poll(self, ctx);
        opendataui::poll(self, ctx);
        embeddingsui::poll(self, ctx);
        earthaccessui::poll(self, ctx);
        nldiui::poll(self, ctx);
        geocodeui::poll(self, ctx);
        navui::poll(self, ctx);
        zarrui::poll(self, ctx);
        geolibreui::poll(self, ctx);
        pluginsui::poll(self, ctx);
        assistantpane::poll(self, ctx);
        // A project the MCP relay, the assistant or a plugin opened or saved this frame (they run
        // on the session directly) becomes Last Project now, not a frame later the app may not
        // reach.
        self.note_last_project();
        saveprompt::close_requested(self, ctx);
        autosave::tick(self, ctx);
        self.ensure_script_font();
        // Fonts downloaded by the web app.
        let fonts: Vec<(String, Vec<u8>)> = match &self.services.font_inbox {
            Some(inbox) => std::mem::take(&mut *inbox.lock().unwrap_or_else(|e| e.into_inner())),
            None => Vec::new(),
        };
        for (url, bytes) in fonts {
            use rustgis_engine::render::fonts::{CjkRegion, ScriptFont, script_font_url, valid_font};
            let region = [CjkRegion::SimplifiedChinese, CjkRegion::Japanese, CjkRegion::Korean]
                .map(ScriptFont::Cjk)
                .into_iter()
                .chain([ScriptFont::Arabic, ScriptFont::Bengali, ScriptFont::Lao])
                .find(|r| script_font_url(*r) == Some(url.as_str()));
            if bytes.is_empty() || !valid_font(&bytes, 0) {
                // A failed download: say so; picking the language again retries.
                log::error!("{url}: no usable font downloaded");
                // Name the language whose font failed (not the current one), in English: its own
                // script has no font yet.
                let language = i18n::Lang::all()
                    .find(|l| region.is_some_and(|r| Self::script_region(l.script()) == Some(r)))
                    .unwrap_or(self.chosen_lang());
                let msg = i18n::fmt(
                    "Couldn't download the font for {language}; pick the language again to retry.",
                    &[("language", language.english())],
                );
                self.session.notify(msg, true);
                if let Some(r) = region {
                    let name = format!("script-{r:?}");
                    self.fonts_requested.retain(|n| *n != name);
                    self.fonts_failed.push(name);
                }
                continue;
            }
            let name = format!("web-{}", url.rsplit('/').next().unwrap_or("font"));
            self.extra_fonts.retain(|(n, _, _)| *n != name);
            self.extra_fonts.push((name, std::sync::Arc::new(egui::FontData::from_owned(bytes)), region));
            self.fonts_dirty = true;
        }
        if self.fonts_dirty {
            self.fonts_dirty = false;
            self.installed_region = self.current_region();
            theme::install_fonts(ctx, &self.extra_fonts, self.installed_region);
            self.fonts_pending = true;
            self.map.invalidate();
        }
        // The label fonts the last frame's labels asked for (after the interface fonts, which
        // would replace them). Not in a frame that set the interface fonts: those take effect on
        // the next, and setting fonts now, from the definitions in effect, would undo them.
        if self.fonts_pending {
            ctx.request_repaint();
        } else {
            labelfonts::install_pending(ctx);
        }
        // Labels and layout texts in a named family draw in the label font, or the text's
        // fallback, until the installed fonts are indexed (`families::warm_up`); then the map and
        // the layout page are drawn again in theirs.
        if !self.label_fonts_indexed {
            if rustgis_engine::render::families::installed_now().is_some() {
                self.label_fonts_indexed = true;
                self.map.invalidate();
                self.layout.redraw();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }
        // Files from the async picker or browser drops.
        let arrived: Vec<(String, Vec<u8>)> = match &self.services.inbox {
            Some(inbox) => std::mem::take(&mut *inbox.lock().unwrap_or_else(|e| e.into_inner())),
            None => Vec::new(),
        };
        for (name, bytes) in arrived {
            let _ = self.open_picked(&name, &bytes);
        }
        // Files dropped on the desktop window arrive as paths. (The web shell reads browser drops
        // asynchronously and passes them through the inbox instead.)
        if !cfg!(target_arch = "wasm32") {
            let mut dropped = ctx.input_mut(|i| std::mem::take(&mut i.raw.dropped_files));
            // A project first, so the data dropped with it goes into that project, not the one
            // it replaces. While the save prompt asks about replacing this one, the rest wait.
            dropped.sort_by_key(|f| rustgis_engine::project::open_command(&f.path().display().to_string()) != "project.open");
            let mut held = 0usize;
            for f in dropped {
                let p = f.path().display().to_string();
                if p.is_empty() {
                    continue;
                }
                if saveprompt::waiting(self) {
                    held += 1;
                    continue;
                }
                if p.to_ascii_lowercase().ends_with(".rgplugin") {
                    match std::fs::read(f.path()) {
                        Ok(bytes) => pluginsui::received(self, bytes, None),
                        Err(e) => self.session.notify(format!("{p}: {e}"), true),
                    }
                    continue;
                }
                let _ = layerchooser::add_file(self, &p);
                self.map.invalidate();
            }
            if held == 1 {
                self.session.notify(
                    tl!("Another dropped file wasn't opened: drop it again once you've answered about the unsaved changes."),
                    false,
                );
            } else if held > 1 {
                let text = crate::i18n::fmt(
                    "{n} other dropped files weren't opened: drop them again once you've answered about the unsaved changes.",
                    &[("n", &held.to_string())],
                );
                self.session.notify(text, false);
            }
        }
        if self.fonts_frames == 0 {
            self.add_cjk_fallback();
            self.add_symbol_fallback();
            self.installed_region = self.current_region();
            theme::install_fonts(ctx, &self.extra_fonts, self.installed_region);
            self.fonts_pending = true;
        }
        // Switch the drawn language once its font can draw it; until then keep the previous one.
        let chosen = self.chosen_lang();
        if self.shown_lang != chosen {
            if self.can_draw(chosen) {
                self.shown_lang = chosen;
                self.map.invalidate();
            } else if self.fonts_pending {
                ctx.request_repaint();
            }
        }
        self.refresh_themes(ctx);
        if self.ui.dark_follows_system
            && let Some(system) = ctx.system_theme()
        {
            self.ui.dark = system == egui::Theme::Dark;
        }
        widgets::let_go_on_panel_press(ctx);
        widgets::note_escape(ctx);
        if self.applied_theme.as_ref() != Some(&(self.ui.theme_name.clone(), self.ui.dark)) {
            let chosen = self.current_theme();
            theme::apply(ctx, &chosen.tokens, &chosen.metrics);
            self.applied_theme = Some((self.ui.theme_name.clone(), self.ui.dark));
            self.map.invalidate();
        }
        // egui's own zoom keys would change the zoom behind the Interface Scale menu.
        ctx.options_mut(|o| o.zoom_with_keyboard = false);
        // The menu is the only control, so a zoom from anywhere else is put back the same frame.
        if (ctx.zoom_factor() - self.ui.interface_scale).abs() > 0.001 {
            ctx.set_zoom_factor(self.ui.interface_scale);
            self.map.invalidate();
        }
    }

    /// The Theme button's entry: the drawn name, or Follow the System where the platform reports one.
    pub fn theme_shown(&self, system_reports: bool) -> (bool, String) {
        let follows = self.ui.theme_name.is_empty() && self.ui.dark_follows_system && system_reports;
        let name = if follows {
            tl!("Follow the System").to_string()
        } else if let Some(drawn) = self.drawn() {
            // The drawn theme is named: a file that stopped loading is not.
            theme::label(&drawn.name)
        } else if self.ui.dark {
            theme::label("Dark")
        } else {
            theme::label("Light")
        };
        (follows, name)
    }

    /// A number that changes when the theme files are read again, for a caller that caches a look.
    pub fn theme_revision(&self) -> u64 {
        self.themes_revision
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let _diagnostics = self.session.diagnostics.enter();
        let ctx = ui.ctx().clone();
        // Fonts installed by `logic` take effect on the next frame.
        if self.fonts_frames < 2 {
            self.fonts_frames += 1;
            ctx.request_repaint();
            return;
        }
        i18n::set_current(self.lang());
        let t = theme::Tokens::get(&ctx);
        // Before the shortcuts and the fields: the row picked in a text field's menu, as its keys.
        fieldmenu::play(self, &ctx);
        chrome::shortcuts(self, &ctx);
        panes::follow_layer_filter(self);
        // A Zarr summary, a trend, a workflow, an export or a read running on its thread lands
        // whatever is in view, the File menu's backstage too, so a project saved there holds it
        // (all are looked at: `|`, not `||`), and the frames go on while one runs so it's taken
        // when it ends.
        #[cfg(not(target_arch = "wasm32"))]
        if workspacepanes::land_summary(self)
            | analysispane::land_trend(self)
            | analysispane::land_job(self)
            | exportjob::land(self)
            | readjob::land(self)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        if self.ui.backstage {
            backstage::show(self, ui);
        } else {
            chrome::header(self, ui);
            ribbon::show(self, ui);
            chrome::status_bar(self, ui);
            panes::show(self, ui);
            table::show(self, ui);
            timebar::show(self, ui);
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.canvas)).show(ui, |ui| {
                mapview::show(self, ui);
            });
        }
        // Text typed into a layout element's field, or a text graphic's, the Format pane no
        // longer shows.
        layoutview::save_drafts(self);
        graphicformat::save_draft(self);
        dialogs::show(self, &ctx);
        layerchooser::show(self, &ctx);
        analysispane::script_details(self, &ctx);
        mapview::confirm_delete(self, &ctx);
        autosave::offer(self, &ctx);
        saveprompt::show(self, &ctx);
        pluginsui::show(self, &ctx);
        update::show(self, &ctx);
        showme::tick(self, &ctx);
        chrome::command_search(self, &ctx);
        shortcutui::show(self, &ctx);
        chrome::about(self, &ctx);
        #[cfg(not(target_arch = "wasm32"))]
        agentthreads::windows(self, &ctx);
        chrome::notices(self, &ctx);
        // After every field is drawn: one right-clicked opens its menu.
        fieldmenu::show(self, &ctx);
        if let Some(text) = self.ui.copy_out.take() {
            ctx.copy_text(text);
        }
        if let Some(img) = self.ui.copy_image.take() {
            ctx.copy_image(img);
        }
        if let Some(url) = self.ui.open_url.take() {
            ctx.open_url(egui::OpenUrl::new_tab(url));
        }
        let title = format!("{}{} - RustGIS", self.session.project.name, if self.session.dirty() { " •" } else { "" });
        if title != self.sent_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.sent_title = title;
        }
        pluginstart::frame_drawn(self, &ctx);
        // egui times clicks by the frame they arrive in: at a slow frame rate the two clicks of a
        // double-click land too far apart for its default delay (#1321). It reads the delay when
        // the next frame begins, before that frame's `logic`, so it's set here, from this frame's
        // work so far (a map drawn on the CPU) and from egui's `stable_dt`. That is the last
        // frame's real length only when the frame before it asked for an immediate repaint, and
        // egui's `predicted_dt` otherwise, so a frame slow only in painting can go unseen.
        if let Some(start) = self.frame_start.take() {
            let dt = ctx.input(|i| i.stable_dt).max(start.elapsed().as_secs_f32());
            ctx.options_mut(|o| o.input_options.max_double_click_delay = double_click_delay(dt));
        }
    }
}

/// The file name at the end of a project's web address, when it names a project or a project
/// package; the project is then opened as that file would be.
fn project_file_name(url: &str) -> Option<&str> {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    (rustgis_engine::project::opens_as_project(last) || rustgis_engine::map::geolibre::is_project_file(last)).then_some(last)
}

/// The longest gap between the two clicks of a double-click, in seconds, for frames `dt` seconds
/// long. egui times a click by the frame it arrives in, so a gap of 0.3 s can be measured as up to
/// two frames more. At least 0.5 s, the Windows default; at most 1.2 s, so two separate clicks
/// still read as two.
fn double_click_delay(dt: f32) -> f64 {
    (0.3 + 2.0 * f64::from(dt)).clamp(0.5, 1.2)
}

/// A test's click whose press and release arrive in one frame, as they do inside a slow one, `gap`
/// seconds on egui's clock after the frame before.
#[cfg(test)]
pub(crate) fn click_in_one_frame(h: &mut egui_kittest::Harness<'_, RustGisApp>, at: egui::Pos2, gap: f64) {
    let time = h.ctx.input(|i| i.time) + gap;
    let button =
        |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    let input = h.input_mut();
    input.time = Some(time);
    input.events.extend([egui::Event::PointerMoved(at), button(true), button(false)]);
    h.step();
    h.input_mut().time = None;
}

/// Runs a test that owns process-wide accounts/registrations in its own executable process.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn isolate_account_test(name: &str) -> bool {
    if std::env::var("RUSTGIS_ACCOUNT_SCENARIO").as_deref() == Ok(name) {
        return false;
    }
    let output = std::env::current_exe().and_then(|executable| {
        std::process::Command::new(executable).args(["--exact", name]).env("RUSTGIS_ACCOUNT_SCENARIO", name).output()
    });
    assert!(output.is_ok(), "could not start isolated account scenario: {:?}", output.as_ref().err());
    let Ok(output) = output else { return true };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut summaries = stdout.lines().filter(|line| line.starts_with("test result:"));
    let ran_one = summaries.next().is_some_and(|line| line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;"))
        && summaries.next().is_none();
    assert!(
        output.status.success() && ran_one,
        "isolated account scenario failed or ran no test:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

/// Tests that fetch through the tile cache take turns: it has one fetcher for the whole process,
/// and a test that replaces it (`set_fetcher`) would answer another's downloads. So do tests that
/// log in to a connection, as a login replaced or ended empties the cache (`cache::clear`), and
/// tests that wait for what it reads (tiles, an archive's header), which that would drop.
#[cfg(test)]
pub(crate) mod fetching {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};

    static TURN: Mutex<()> = Mutex::new(());

    /// A test's turn: when it ends, even by failing, addresses are fetched natively again.
    pub(crate) struct Turn {
        _held: MutexGuard<'static, ()>,
    }

    impl Drop for Turn {
        fn drop(&mut self) {
            rustgis_engine::render::rustgis_tiles::cache::clear_fetcher();
        }
    }

    /// Waits for the tests fetching before this one to end.
    pub(crate) fn turn() -> Turn {
        Turn { _held: TURN.lock().unwrap_or_else(|e| e.into_inner()) }
    }

    /// A slow server of a file by range ([`slow_ranges`]).
    pub(crate) struct SlowRanges {
        pub origin: String,
        /// While unset, requests are held.
        pub open: Arc<AtomicBool>,
        /// How many requests it has held.
        pub held: Arc<AtomicUsize>,
    }

    /// Serves `file` by range, holding requests while `open` is unset. A frame waiting for one
    /// shows as a slow frame, not as a test that hangs: after 20 s it is answered anyway.
    pub(crate) fn slow_ranges(file: Vec<u8>) -> SlowRanges {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (open, held) = (Arc::new(AtomicBool::new(true)), Arc::new(AtomicUsize::new(0)));
        let (shut, count, file) = (open.clone(), held.clone(), Arc::new(file));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let (Ok(mut stream), shut, count, file) = (stream, shut.clone(), count.clone(), file.clone()) else { continue };
                std::thread::spawn(move || {
                    let mut request = [0u8; 4096];
                    let read = stream.read(&mut request).unwrap_or(0);
                    if !shut.load(Ordering::Relaxed) {
                        count.fetch_add(1, Ordering::Relaxed);
                        let start = std::time::Instant::now();
                        while !shut.load(Ordering::Relaxed) && start.elapsed() < std::time::Duration::from_secs(20) {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                    }
                    let request = String::from_utf8_lossy(&request[..read]).to_ascii_lowercase();
                    let range = request.lines().find_map(|l| l.strip_prefix("range: bytes=")).and_then(|r| {
                        let (a, b) = r.trim().split_once('-')?;
                        Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?.min(file.len().saturating_sub(1))))
                    });
                    let (status, part) = match range {
                        Some((a, b)) if a <= b => ("206 Partial Content", &file[a..=b]),
                        _ => ("200 OK", &file[..]),
                    };
                    let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", part.len());
                    let _ = stream.write_all(part);
                });
            }
        });
        SlowRanges { origin, open, held }
    }

    /// Waits at most ten seconds for a fixture's request, then restores blocking reads so
    /// its existing stream timeout remains effective. Missing sends fail instead of hanging CI.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn accept(server: &std::net::TcpListener) -> std::io::Result<std::net::TcpStream> {
        server.set_nonblocking(true)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match server.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false)?;
                    return Ok(stream);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no request reached the test server"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod start_with_tests {
    use super::StartWith;
    use serde_json::json;

    #[test]
    fn start_with_reads_back_what_it_stores() {
        for w in [StartWith::Sample, StartWith::Blank, StartWith::Last, StartWith::File("C:/maps/a.rgis".into())] {
            assert_eq!(StartWith::from_pref(&w.pref()), w);
        }
        // Anything else is the sample; a stored path is trimmed.
        for v in ["", "nope", "file:", "file:   "] {
            assert_eq!(StartWith::from_pref(v), StartWith::Sample, "{v:?}");
        }
        assert_eq!(StartWith::from_pref("file:  /a/b.rgis "), StartWith::File("/a/b.rgis".into()));
    }

    #[test]
    fn start_with_takes_a_named_choice_or_a_project_that_is_there() {
        for (v, w) in [(json!("sample"), StartWith::Sample), (json!("blank"), StartWith::Blank), (json!("last"), StartWith::Last)] {
            assert_eq!(StartWith::from_json(&v), Ok(w));
        }
        /// Removes the folder when the test ends, a failed assertion included.
        struct TempDir(std::path::PathBuf);
        impl Drop for TempDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = std::env::temp_dir().join(format!("rustgis-from-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _cleanup = TempDir(dir.clone());
        let project = dir.join("p.rgis");
        std::fs::write(&project, "{}").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let shown = |name: &str| dir.join(name).display().to_string();
        assert_eq!(StartWith::from_json(&json!({"path": shown("p.rgis")})), Ok(StartWith::File(shown("p.rgis"))));
        for bad in [
            json!(null),
            json!(5),
            json!("nope"),
            json!({}),
            json!({"path": " "}),
            json!({"path": shown("notes.txt")}),
            json!({"path": shown("missing.rgis")}),
        ] {
            assert!(StartWith::from_json(&bad).is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod service_text_tests {
    #[test]
    fn legacy_xml_decoding_does_not_relax_json_utf8() {
        for bytes in [b" \n<Title>Caf\xe9</Title>".as_slice(), b"\xef\xbb\xbf \n<Title>Caf\xe9</Title>".as_slice()] {
            assert!(super::service_text(bytes.to_vec()).is_ok_and(|s| s.contains("Caf\u{fffd}")));
        }
        for bytes in [b"{\"title\":\"Caf\xe9\"}".as_slice(), b"\xef\xbb\xbf{\"title\":\"Caf\xe9\"}".as_slice()] {
            assert!(super::service_text(bytes.to_vec()).is_err());
        }
        let valid = "{\"title\":\"Café\"}";
        assert!(super::service_text(valid.as_bytes().to_vec()).is_ok_and(|text| text == valid));
    }
}

/// The most a recorded step's parameters may hold, serialized.
const MAX_STEP_BYTES: usize = 4 << 20;

/// Commands that act on the active layer when given no `layer`, though they are enabled without
/// one (so [`RustGisApp::layer_acted_on`] can't tell from their `enabled`). A command added with
/// an optional `layer` that defaults to the active layer goes here, so a recorded step names it;
/// `record_tests::every_optional_layer_is_classified` fails until it is placed here or there.
const DEFAULTS_TO_ACTIVE_LAYER: &[&str] = &[
    "select.byAttributes",
    "select.rows",
    "select.layerFromSelection",
    "data.similar",
    "layer.geocode",
    "layer.repairSource",
    "layer.reloadData",
    "report.new",
    "report.open",
    "share.webLayer",
    "share.webStyle",
    "share.locator",
    "pg.saveEdits",
    "pg.discardEdits",
    "gpkg.saveEdits",
    "gpkg.discardEdits",
    "zarr.reduce",
    "cube.trend",
    "cube.reduce",
];

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod record_tests {
    use super::{DEFAULTS_TO_ACTIVE_LAYER, RustGisApp};
    use serde_json::json;

    /// Terrain run with nothing named (from command search) records the DEM it took, so the
    /// step replays on it; a change of exaggeration on a terrain already on records none.
    #[test]
    fn a_terrain_step_names_the_dem_it_took() {
        let mut app = RustGisApp::with_sample();
        let grid = "bmNvbHMgMgpucm93cyAyCnhsbGNvcm5lciAwCnlsbGNvcm5lciAwCmNlbGxzaXplIDEKTk9EQVRBX3ZhbHVlIC05OTk5CjEgMgozIDQK";
        app.session.run("layer.addDataBytes", &json!({"fileName": "grid.asc", "data": grid})).unwrap();
        let dem = app.session.map().layers.iter().find(|l| l.raster.is_some()).map(|l| (l.id, l.name.clone())).unwrap();
        app.session.active_layer = Some(dem.0);
        app.ws.recording = Some(Vec::new());
        app.run("map.terrain", serde_json::Value::Null).unwrap();
        app.run("map.terrain", json!({"exaggeration": 2})).unwrap();
        let steps = app.ws.recording.clone().unwrap();
        let layers: Vec<Option<&serde_json::Value>> =
            steps.iter().filter(|s| s.command == "map.terrain").map(|s| s.params.get("layer")).collect();
        assert_eq!(layers, [Some(&json!(dem.1)), None]);
    }

    /// Every command with an optional `layer` either needs an active layer to be enabled, defaults
    /// to it (`DEFAULTS_TO_ACTIVE_LAYER`), or reads `layer` as something else: a recorded step
    /// names the active layer for the first two only.
    #[test]
    fn every_optional_layer_is_classified() {
        // `layer` is a file's or a service's, the graphics layer drawn on (the active one only if
        // it is a graphics layer), a layout element's, or the one a report or web layer had.
        // Needs a layer read in place from its file, which the sample has none of; it acts on it.
        const NEEDS_A_STREAMED_LAYER: &[&str] = &["layer.loadAll"];
        const SOMETHING_ELSE: &[&str] = &[
            // The DEM a terrain takes its heights from.
            "map.terrain",
            "service.refresh",
            "service.group",
            "layer.addData",
            "layer.addDatabase",
            "layer.addUrl",
            "layer.addDownload",
            "layer.addDataBytes",
            "layer.addDataText",
            "portal.add",
            "graphics.add",
            "layout.addElement",
            "layout.update",
            "layout.frameZoom",
            "report.update",
            "share.replaceWebLayer",
        ];
        let mut app = RustGisApp::with_sample();
        // A raster too, for the commands that need one active (a 2×2 ASCII grid).
        let grid = "bmNvbHMgMgpucm93cyAyCnhsbGNvcm5lciAwCnlsbGNvcm5lciAwCmNlbGxzaXplIDEKTk9EQVRBX3ZhbHVlIC05OTk5CjEgMgozIDQK";
        app.session.run("layer.addDataBytes", &json!({"fileName": "grid.asc", "data": grid})).unwrap();
        let layers: Vec<u32> = app.session.map().layers.iter().map(|l| l.id).collect();
        // As `layer_acted_on` tells, whatever its message says: enabled with some layer active,
        // and not with none.
        let mut needs_active = |c: &rustgis_engine::CommandSpec| {
            app.session.active_layer = None;
            if (c.enabled)(&app.session).is_none() {
                return false;
            }
            let some = layers.iter().any(|&l| {
                app.session.active_layer = Some(l);
                (c.enabled)(&app.session).is_none()
            });
            app.session.active_layer = None;
            some
        };
        let unplaced: Vec<_> = rustgis_engine::cmd::all()
            .into_iter()
            .filter(|c| c.mutates && c.params.contains(r#""layer"?"#))
            .filter(|c| !needs_active(c))
            .filter(|c| ![DEFAULTS_TO_ACTIVE_LAYER, SOMETHING_ELSE, NEEDS_A_STREAMED_LAYER].iter().any(|list| list.contains(&c.id)))
            .map(|c| c.id)
            .collect();
        assert!(unplaced.is_empty(), "say whether these act on the active layer: {unplaced:?}");
    }

    /// Undo or Redo that moves a task's steps closes the step being edited, so Save can't write
    /// its text into another step; one that leaves the steps alone keeps it open.
    #[test]
    fn undoing_a_step_move_closes_the_step_being_edited() {
        let mut app = RustGisApp::with_sample();
        let step = |t: &str| json!({"title": t, "command": "map.zoomFull", "params": {}});
        app.run("task.new", json!({"name": "Lesson", "steps": [step("One"), step("Two")]})).unwrap();
        app.ws.task = "Lesson".into();
        app.run("layer.color", json!({"layer": "Lakes", "color": "#123456"})).unwrap();
        app.ws.task_edit = Some((1, "Two".into(), "Typed".into()));
        app.run("edit.undo", json!(null)).unwrap();
        assert!(app.ws.task_edit.is_some(), "a colour undone leaves the steps alone");
        // Run from anywhere (here, as Command Search would), the move takes the edit with it.
        app.run("task.moveStep", json!({"task": "Lesson", "step": 1, "to": 0})).unwrap();
        assert_eq!(app.ws.task_edit.as_ref().map(|e| e.0), Some(0));
        app.run("edit.undo", json!(null)).unwrap();
        assert!(app.ws.task_edit.is_none(), "the edit's place now names another step");
        assert!(app.session.notices.iter().any(|n| n.text.contains("step being edited was closed")));
    }

    /// A step whose parameters would swell the task isn't recorded, and that is said.
    #[test]
    fn a_step_over_the_size_limit_isnt_recorded() {
        let mut app = RustGisApp::with_sample();
        app.ws.recording = Some(Vec::new());
        let big = "A".repeat(super::MAX_STEP_BYTES + 1);
        app.record_step("layer.addDataBytes", &json!({"fileName": "big.geojson", "data": big}), None);
        assert_eq!(app.ws.recording.as_ref().map(Vec::len), Some(0));
        assert!(app.session.notices.iter().any(|n| n.text.contains("over 4 MB")));
        app.record_step("layer.addDataBytes", &json!({"fileName": "small.geojson", "data": "AAAA"}), None);
        assert_eq!(app.ws.recording.as_ref().map(Vec::len), Some(1));
    }

    /// A raster a workflow made on a thread is recorded as its command without `ran`, so a replay
    /// works it out again (#1291 step 3).
    #[test]
    fn a_raster_made_on_a_thread_is_recorded_as_its_command() {
        let mut app = RustGisApp::with_sample();
        for id in ["analysis.viewshed", "analysis.interpolate", "raster.classify", "dl.classify", "raster.function", "imagery.mosaic"] {
            app.ws.recording = Some(Vec::new());
            app.record_step(id, &json!({"layer": "Cities", "ran": 7}), None);
            let steps = app.ws.recording.as_deref().unwrap_or_default();
            assert_eq!(
                steps.iter().map(|s| (s.command.as_str(), s.params.clone())).collect::<Vec<_>>(),
                [(id, json!({"layer": "Cities"}))]
            );
        }
    }

    /// A background run's `gp.finish` is recorded as the `gp.run` it finished.
    #[test]
    fn a_tool_finished_in_the_background_is_recorded_as_its_run() {
        let mut app = RustGisApp::with_sample();
        app.ws.recording = Some(Vec::new());
        app.session.project.gp_history.push(rustgis_engine::GpRecord {
            tool: "Buffer".into(),
            id: "buffer".into(),
            params: json!({"input": "Cities", "distance": 50}),
            command: None,
            at: 0,
            ok: true,
            messages: Vec::new(),
            millis: 0,
            outputs: Vec::new(),
        });
        app.record_step("gp.finish", &json!({"id": 7}), None);
        let steps = app.ws.recording.as_deref().unwrap_or_default();
        assert_eq!(
            steps.iter().map(|s| (s.command.as_str(), s.params.clone())).collect::<Vec<_>>(),
            [("gp.run", json!({"tool": "buffer", "params": {"input": "Cities", "distance": 50}}))]
        );
    }

    /// A step keeping a plain `key` is recorded, and that is said once a recording.
    #[test]
    fn a_kept_key_is_said_once() {
        let mut app = RustGisApp::with_sample();
        app.ws.recording = Some(Vec::new());
        app.record_step("layer.color", &json!({"layer": "Lakes", "color": "#123456", "key": "a"}), None);
        app.record_step("layer.color", &json!({"layer": "Rivers", "color": "#123456", "key": "b"}), None);
        assert_eq!(app.ws.recording.as_ref().map(Vec::len), Some(2));
        assert_eq!(app.session.notices.iter().filter(|n| n.text.contains("keeps its key parameter")).count(), 1);
    }

    /// A step naming a layer whose name another layer shares says so, once for the name.
    #[test]
    fn a_shared_layer_name_is_said_once() {
        let mut app = RustGisApp::with_sample();
        let rivers = app.session.map().layer_by_name("Rivers").unwrap().id;
        app.session.map_mut().layers.iter_mut().find(|l| l.id == rivers).unwrap().name = "Lakes".into();
        app.run("layer.select", json!({"layer": rivers})).unwrap();
        app.run("ui.taskRecord", json!({})).unwrap();
        let said = |app: &RustGisApp| app.session.notices.iter().filter(|n| n.text.contains("Two layers are named Lakes")).count();
        app.run("layer.color", json!({"color": "#123456"})).unwrap();
        app.run("layer.visibility", json!({})).unwrap();
        assert_eq!(app.ws.recording.as_ref().unwrap()[0].params["layer"], json!("Lakes"));
        assert_eq!(said(&app), 1);
    }

    /// The actual URL completion receiver says when a successful download cannot be recorded.
    /// Failed and revoked deliveries neither add a layer nor claim a recording omission.
    #[test]
    fn downloaded_data_isnt_recorded() {
        use rustgis_engine::{policy, render::rustgis_tiles::cache};
        use std::sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        };
        struct Live(AtomicBool);
        impl policy::Revocation for Live {
            fn allows(&self, _: policy::Effects) -> bool {
                self.0.load(Ordering::SeqCst)
            }
        }
        let _turn = crate::fetching::turn();
        for outcome in ["added", "transport failed", "decode failed", "revoked"] {
            let held = Arc::new(Mutex::new(Vec::<cache::Done>::new()));
            let callbacks = held.clone();
            cache::set_fetcher(move |_, done| callbacks.lock().unwrap().push(done));
            let mut app = RustGisApp::with_sample();
            app.ws.recording = Some(Vec::new());
            let params = json!({"url":"https://recording.invalid/places.geojson", "name":"Recorded download"});
            let live = Arc::new(Live(AtomicBool::new(true)));
            if outcome == "revoked" {
                let scope = policy::Scope::agent(policy::AgentAccess::Allow, false, "record-download", None, live.clone());
                app.session.with_scope(scope, |s| s.request_url(&params)).unwrap().unwrap();
                let request = app.session.take_url_requests().pop().unwrap();
                app.add_url(request.params(), Some(&request), None).unwrap();
            } else {
                app.run("ui.addUrl", params).unwrap();
            }
            assert_eq!(app.ui.downloads.len(), 1, "the real UI download is pending");
            assert_eq!(held.lock().unwrap().len(), 1);
            assert!(app.ws.recording.as_ref().unwrap().is_empty());
            let mark = app.session.notice_mark();
            live.0.store(outcome != "revoked", Ordering::SeqCst);
            let result = match outcome {
                "transport failed" => Err("synthetic download failure".to_string()),
                "decode failed" => Ok(b"not a GeoJSON document".to_vec()),
                _ => Ok(br#"{"type":"Point","coordinates":[1,2]}"#.to_vec()),
            };
            held.lock().unwrap().pop().unwrap()(result);
            app.poll_data_downloads();
            let succeeded = outcome == "added";
            assert!(app.ui.downloads.is_empty());
            assert_eq!(app.session.map().layer_by_name("Recorded download").is_some(), succeeded);
            assert!(app.ws.recording.as_ref().unwrap().is_empty());
            let notices = app.session.notices_since(mark);
            let omitted = notices.iter().filter(|n| n.text.contains("isn't recorded") && n.text.contains("session only")).count();
            assert_eq!(omitted, usize::from(succeeded), "{outcome}: {notices:?}");
            assert_eq!(notices.iter().filter(|n| n.error).count(), usize::from(!succeeded), "{outcome}: {notices:?}");
        }
    }

    /// A window read off the frame is a step all the same: the command that reads it, with its box,
    /// not the download that can't be added again (#1330).
    #[test]
    fn a_window_read_off_the_frame_is_recorded_as_the_command_reading_it() {
        let mut app = RustGisApp::with_sample();
        app.ws.recording = Some(Vec::new());
        let window = json!({"bbox": "view", "cellSize": 30, "cog": true});
        let p = json!({"id": 1, "url": "https://example.com/data", "name": "W", "request": 7, "window": window});
        app.record_step("layer.addDownload", &p, None);
        let steps = app.ws.recording.as_ref().unwrap();
        assert_eq!(steps.len(), 1, "{:?}", app.session.notices);
        assert_eq!(steps[0].command, "layer.addUrl");
        assert_eq!(steps[0].params, json!({"url": "https://example.com/data", "name": "W", "bbox": "view", "cellSize": 30, "cog": true}));
    }

    /// `ui.addUrl` with `"bbox"`: a slow server doesn't hold the
    /// frame, and the layer lands when the read does (#1330).
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_window_is_read_off_the_frame_and_added_when_it_lands() {
        use std::sync::atomic::Ordering;
        let _turn = crate::fetching::turn();
        // With the header in hand only the window's tiles wait: those a read on the frame would hold
        // it for, for the server's 20 s. Without it, the header waits first.
        for header_read in [true, false] {
            let slow = crate::fetching::slow_ranges(rustgis_engine::data::raster::cog::example(1024, 256));
            let mut h = egui_kittest::Harness::builder().build_ui_state(
                |ui, app: &mut RustGisApp| {
                    let ctx = ui.ctx().clone();
                    app.logic(&ctx);
                },
                RustGisApp::with_sample(),
            );
            let url = format!("{}/scene.tif", slow.origin);
            if header_read {
                rustgis_engine::render::rustgis_tiles::cog::info_blocking(&url).unwrap();
            }
            slow.open.store(false, Ordering::Relaxed);
            let p = json!({"url": url, "bbox": [10.25, 45.5, 10.5, 45.75], "name": "Window"});
            let asked = std::time::Instant::now();
            h.state_mut().run("ui.addUrl", p).unwrap();
            assert!(asked.elapsed() < std::time::Duration::from_secs(5), "asking for it held the frame");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while slow.held.load(Ordering::Relaxed) == 0 {
                assert!(std::time::Instant::now() < deadline, "no request held");
                let frame = std::time::Instant::now();
                h.step();
                assert!(frame.elapsed() < std::time::Duration::from_secs(5), "the read held the frame");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let frame = std::time::Instant::now();
            h.step();
            assert!(frame.elapsed() < std::time::Duration::from_secs(5), "the read held the frame");
            assert!(h.state().session.map().layer_by_name("Window").is_none());
            assert_eq!(h.state().ui.window_reads.len(), 1);
            slow.open.store(true, Ordering::Relaxed);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while h.state().session.map().layer_by_name("Window").is_none() {
                assert!(std::time::Instant::now() < deadline, "timed out waiting: {:?}", h.state().session.notices);
                h.step();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let app = h.state();
            assert!(app.ui.window_reads.is_empty());
            let data = app.session.map().layer_by_name("Window").and_then(|l| l.raster.as_ref()?.data.clone()).unwrap();
            assert_eq!((data.width, data.height), (256, 256));
            assert!(app.session.notices.iter().any(|n| n.text.starts_with("Window: a 256 × 256 window")), "{:?}", app.session.notices);
        }
    }

    /// Windows under way are bounded, as each holds a thread (#1330).
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn too_many_windows_under_way_are_refused() {
        let mut app = RustGisApp::with_sample();
        for _ in 0..crate::MAX_WINDOW_READS {
            app.ui.window_reads.push((0, String::new(), serde_json::Value::Null, Default::default(), Default::default()));
        }
        let e = app.run("ui.addUrl", json!({"url": "https://x.org/a.tif?token=hidden-1330", "bbox": [1, 2, 3, 4]})).unwrap_err();
        assert!(e.contains("x.org/a.tif") && e.contains("16 windows") && !e.contains("hidden-1330"), "{e}");
        assert_eq!(app.ui.window_reads.len(), crate::MAX_WINDOW_READS);
    }

    /// A window still waiting for its turn when its project closes gives up its place at once, and
    /// isn't read (#1330).
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_window_waiting_when_its_project_closes_isnt_read() {
        use std::sync::atomic::Ordering;
        let _turn = crate::fetching::turn();
        let slow = crate::fetching::slow_ranges(rustgis_engine::data::raster::cog::example(1024, 256));
        slow.open.store(false, Ordering::Relaxed);
        let mut app = RustGisApp::with_sample();
        // Another window's read has the turn, and keeps it throughout.
        let reading = crate::WINDOW_TURN.lock().unwrap_or_else(|e| e.into_inner());
        let url = format!("{}/scene.tif", slow.origin);
        app.run("ui.addUrl", json!({"url": url, "bbox": [10.25, 45.5, 10.5, 45.75], "name": "Left"})).unwrap();
        app.run("project.new", json!({})).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !app.ui.window_reads.is_empty() {
            assert!(std::time::Instant::now() < deadline, "its place wasn't given up");
            app.poll_data_downloads();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        drop(reading);
        assert_eq!(slow.held.load(Ordering::Relaxed), 0, "nothing is asked of the server");
        let said = |n: &rustgis_engine::Notice| n.text.contains("scene.tif") && n.text.contains("before its read began");
        assert!(app.session.notices.iter().any(said), "{:?}", app.session.notices);
    }

    /// The next window is read only once the frame has taken the last: a frame that stalls holds
    /// one window's values, not those of all that were asked for (#1330).
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn the_next_window_waits_for_the_last_to_be_taken() {
        let _turn = crate::fetching::turn();
        let slow = crate::fetching::slow_ranges(rustgis_engine::data::raster::cog::example(1024, 256));
        let mut app = RustGisApp::with_sample();
        let url = format!("{}/scene.tif", slow.origin);
        for name in ["First", "Second"] {
            app.run("ui.addUrl", json!({"url": url, "bbox": [10.25, 45.5, 10.5, 45.75], "name": name})).unwrap();
        }
        let landed =
            |app: &RustGisApp| app.ui.window_reads.iter().filter(|r| r.3.lock().unwrap_or_else(|e| e.into_inner()).is_some()).count();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while landed(&app) == 0 {
            assert!(std::time::Instant::now() < deadline, "no read landed");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // No frame takes it, so the other isn't read.
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert_eq!(landed(&app), 1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !app.ui.window_reads.is_empty() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting: {:?}", app.session.notices);
            app.poll_data_downloads();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(app.session.map().layer_by_name("First").is_some() && app.session.map().layer_by_name("Second").is_some());
    }

    /// A window that can't be read is refused at once, one read that fails says so once, and one
    /// whose project was closed meanwhile isn't added (#1330).
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_window_refused_failed_or_left_behind_is_said() {
        let until = |app: &mut RustGisApp| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while !app.ui.window_reads.is_empty() {
                assert!(std::time::Instant::now() < deadline, "timed out waiting");
                app.poll_data_downloads();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        let _turn = crate::fetching::turn();
        let mut app = RustGisApp::with_sample();
        let e = app.run("ui.addUrl", json!({"url": "https://x.org/a.tif", "bbox": "here"})).unwrap_err();
        assert!(e.contains("bbox is") && app.ui.window_reads.is_empty(), "{e}");
        // No probe first, either: a window is a GeoTIFF's or nothing.
        let e = app.run("ui.addUrl", json!({"url": "https://x.org/a.pmtiles", "bbox": [1, 2, 3, 4]})).unwrap_err();
        assert!(e.contains("names a .pmtiles file") && app.ui.cog_probes.is_empty(), "{e}");
        // Not a GeoTIFF: said once, without the address's keys.
        let slow = crate::fetching::slow_ranges(b"not a tiff at all".to_vec());
        let url = format!("{}/a.tif?token=hidden-1330", slow.origin);
        let mark = app.session.notice_mark();
        app.run("ui.addUrl", json!({"url": url, "bbox": [1, 2, 3, 4], "name": "Bad"})).unwrap();
        until(&mut app);
        let errors: Vec<String> = app.session.notices_since(mark).iter().filter(|n| n.error).map(|n| n.text.clone()).collect();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("/a.tif") && !errors[0].contains("hidden-1330"), "{errors:?}");
        let reading = app.session.notices_since(mark).iter().find(|n| n.text.starts_with("Reading")).map(|n| n.text.clone()).unwrap();
        assert!(reading.contains("/a.tif") && !reading.contains("hidden-1330"), "{reading}");
        assert!(app.session.map().layer_by_name("Bad").is_none());
        // Its project closed while it was read.
        let slow = crate::fetching::slow_ranges(rustgis_engine::data::raster::cog::example(1024, 256));
        let url = format!("{}/scene.tif", slow.origin);
        app.run("ui.addUrl", json!({"url": url, "bbox": [10.25, 45.5, 10.5, 45.75], "name": "Left"})).unwrap();
        app.run("project.new", json!({})).unwrap();
        until(&mut app);
        assert!(app.session.map().layer_by_name("Left").is_none());
        let said = |n: &rustgis_engine::Notice| n.text.contains("scene.tif") && n.text.contains("its project was closed");
        assert!(app.session.notices.iter().any(said), "{:?}", app.session.notices);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn an_anonymous_download_never_acquires_a_saved_login() {
        let _turn = crate::fetching::turn();
        use rustgis_engine::render::rustgis_tiles::{cache, logins};
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", server.local_addr().unwrap());
        let reply = std::thread::spawn(move || {
            let mut stream = crate::fetching::accept(&server).unwrap();
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
            }
            let body = br#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{}}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/geo+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(body).unwrap();
            String::from_utf8(request).unwrap()
        });
        let mut app = RustGisApp::with_sample();
        let id = app
            .run("connection.new", json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Ui-Key"}))
            .unwrap()["id"]
            .clone();
        app.run("connection.login", json!({"id":id,"apiKey":"ui-sentinel-757","allowInsecure":true})).unwrap();
        app.run("ui.addUrl", json!({"url":format!("{origin}/roads.geojson"),"name":"Anonymous download","service":true,"anonymous":true}))
            .unwrap();
        cache::wait(std::time::Duration::from_secs(5));
        app.poll_data_downloads();
        crate::serviceui::poll_pages(&mut app, &egui::Context::default());
        assert!(!reply.join().unwrap().to_ascii_lowercase().contains("x-ui-key:"));
        app.run("connection.login", json!({"id":id,"apiKey":"ui-sentinel-757","allowInsecure":true})).unwrap();
        let layer = app.session.map().layers.iter().find(|layer| layer.name == "Anonymous download").unwrap();
        assert!(layer.connection.is_empty());
        assert!(logins::apply(layer.dataset.source.as_deref().unwrap()).unwrap().is_none());
    }

    /// The add-service dialog's "Downloading… {layer}" line says how the download went when it
    /// lands (#1453): the layer added, or why not in the error line; a line no longer about that
    /// layer is left alone.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_landed_download_says_how_it_went_in_the_dialog() {
        let _turn = crate::fetching::turn();
        use rustgis_engine::render::rustgis_tiles::cache;
        use std::io::{Read, Write};
        // A server answering one request with `status` and a point's GeoJSON.
        let serve = |status: &'static str| {
            let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let origin = format!("http://{}", server.local_addr().unwrap());
            let reply = std::thread::spawn(move || {
                let mut stream = crate::fetching::accept(&server).unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = stream.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..count]);
                }
                let body = br#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},"properties":{}}]}"#;
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/geo+json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            });
            (origin, reply)
        };
        // Adds "Roads" from a server answering `status`, with `line` in the dialog meanwhile.
        let add = |status: &'static str, line: String| {
            let (origin, reply) = serve(status);
            let mut app = RustGisApp::with_sample();
            let url = format!("{origin}/roads.geojson?token=hidden-757");
            app.run("ui.addUrl", json!({"url": url, "name": "Roads", "service": true})).unwrap();
            app.dialogs.info = Some(line);
            cache::wait(std::time::Duration::from_secs(5));
            app.poll_data_downloads();
            crate::serviceui::poll_pages(&mut app, &egui::Context::default());
            reply.join().unwrap();
            app
        };
        let added = add("200 OK", crate::serviceui::downloading("Roads"));
        assert!(added.session.map().layers.iter().any(|l| l.name == "Roads"));
        assert_eq!(added.dialogs.info, Some(crate::serviceui::added("Roads")));
        assert_eq!(added.dialogs.error, None);
        let failed = add("404 Not Found", crate::serviceui::downloading("Roads"));
        assert!(!failed.session.map().layers.iter().any(|l| l.name == "Roads"));
        assert_eq!(failed.dialogs.info, None);
        // Why, redacted as the notice is: the address, not its token.
        let why = failed.dialogs.error.as_deref().unwrap_or("");
        assert!(why.contains("roads.geojson") && !why.contains("hidden-757"), "{why}");
        // The line has moved on to another layer (or another dialog): it stays as it is.
        let other = add("200 OK", crate::serviceui::downloading("Rivers"));
        assert_eq!(other.dialogs.info, Some(crate::serviceui::downloading("Rivers")));
        assert_eq!(other.dialogs.error, None);
        // Closing the originating project drops a late answer normally, with one informational notice.
        let mut late = RustGisApp::with_sample();
        let id = late.session.download_started();
        late.run("project.new", json!({})).unwrap();
        let mark = late.session.notice_mark();
        late.add_landed(
            crate::serviceui::Landed { id, url: "https://example.com/roads.geojson".into(), params: json!({"name":"Roads"}), origin: None },
            b"{}".to_vec(),
        );
        let notices = late.session.notices_since(mark);
        assert_eq!(notices.len(), 1, "a normal late drop must not also report a continuation error");
        assert!(!notices[0].error && notices[0].text.contains("project was closed"));
        assert!(late.session.map().layer_by_name("Roads").is_none());
    }

    /// A `ui.addUrl` the app runs that fails is said once, by running it: after a probe found a
    /// file to download whole, and for a request a command queued.
    #[test]
    fn a_failed_ui_add_url_the_app_runs_is_said_once() {
        #[cfg(not(target_arch = "wasm32"))]
        if crate::isolate_account_test("record_tests::a_failed_ui_add_url_the_app_runs_is_said_once") {
            return;
        }
        let mut app = RustGisApp::with_sample();
        let p = json!({"url": "https://x.org/a.csv", "connection": "no such connection"});
        let errors = |app: &RustGisApp, mark| app.session.notices_since(mark).iter().filter(|n| n.error).count();
        let mark = app.session.notice_mark();
        let probe: crate::Slot<Result<bool, String>> = Default::default();
        *probe.lock().unwrap() = Some(Ok(false));
        app.ui.cog_probes.push((p.clone(), None, probe));
        app.poll_data_downloads();
        assert_eq!(errors(&app, mark), 1, "{:?}", app.session.notices_since(mark));
        // A valid admitted request can still fail to start when all claim slots are occupied.
        let mut claims = Vec::new();
        for number in 0..64 {
            app.session.request_url(&json!({"url":format!("https://x.org/held-{number}.csv")})).unwrap();
            let request = app.session.take_url_requests().pop().unwrap();
            claims.push(app.session.download_started_for(&request).unwrap());
        }
        let mark = app.session.notice_mark();
        app.session.request_url(&json!({"url":"https://x.org/queued.csv"})).unwrap();
        let mut h = egui_kittest::Harness::builder().build_ui_state(
            |ui, app: &mut RustGisApp| {
                let ctx = ui.ctx().clone();
                app.logic(&ctx);
            },
            app,
        );
        h.step();
        let app = h.state();
        assert_eq!(errors(app, mark), 1, "{:?}", app.session.notices_since(mark));
        for claim in claims {
            assert!(h.state().session.download_is_pending(claim), "another request keeps its claim");
            h.state_mut().session.download_failed(claim);
        }
    }

    #[test]
    fn a_probe_dropped_by_another_project_says_so() {
        let mut app = RustGisApp::with_sample();
        let url = "https://example.com/elevation.tif?token=hidden-758";
        app.ui.cog_probes.push((json!({"url": url, "name": "Elevation"}), None, Default::default()));
        app.ui.cog_probes.push((json!({"url": "https://example.com/slope.tif", "name": "Slope"}), None, Default::default()));
        app.dialogs.info = Some(crate::serviceui::downloading("Elevation"));
        app.run("project.new", json!({})).unwrap();
        assert!(app.ui.cog_probes.is_empty());
        assert_eq!(app.dialogs.info, None);
        let why = app.dialogs.error.as_deref().unwrap_or("");
        // The reason stays, though the address's query names a secret.
        assert!(why.contains("elevation.tif") && why.contains("project was closed") && !why.contains("hidden-758"), "{why}");
        // The new project's notices: the one for each dropped probe comes after it opened.
        for file in ["elevation.tif", "slope.tif"] {
            let said = |n: &rustgis_engine::Notice| n.text.contains(file) && n.text.contains("project was closed");
            assert!(app.session.notices.iter().any(said), "{file}: {:?}", app.session.notices);
        }
        assert!(app.session.notices.iter().all(|n| !n.text.contains("hidden-758")));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn whitespace_downloads_keep_their_login_choice_and_read_options() {
        let _turn = crate::fetching::turn();
        use rustgis_engine::render::rustgis_tiles::{cache, logins};
        use std::io::{Read, Write};
        for anonymous in [false, true] {
            let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let origin = format!("http://{}", server.local_addr().unwrap());
            let reply = std::thread::spawn(move || {
                let mut stream = crate::fetching::accept(&server).unwrap();
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = stream.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..count]);
                }
                let body = b"name;east;north\nCentral Park;992000;220000\n";
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/csv\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                    .unwrap();
                stream.write_all(body).unwrap();
                String::from_utf8(request).unwrap()
            });
            let mut app = RustGisApp::with_sample();
            let id = app
                .run("connection.new", json!({"kind":"wms","address":format!("{origin}/wms"),"auth":"header","keyName":"X-Ui-Key"}))
                .unwrap()["id"]
                .clone();
            app.run("connection.login", json!({"id":id,"apiKey":"ui-whitespace-757","allowInsecure":true})).unwrap();
            let mut params = json!({
                "url":format!(" \t{origin}/parks.csv \r\n"), "name":"Downloaded parks",
                "anonymous":anonymous, "x":"east", "y":"north", "delimiter":";", "crs":"EPSG:2263", "zoom":false
            });
            if !anonymous {
                params["connection"] = id.clone();
            }
            app.run("ui.addUrl", params).unwrap();
            cache::wait(std::time::Duration::from_secs(5));
            app.poll_data_downloads();
            let request = reply.join().unwrap().to_ascii_lowercase();
            assert!(request.starts_with("get /parks.csv http/1.1\r\n"), "{request}");
            assert_eq!(request.contains("\r\nx-ui-key: ui-whitespace-757\r\n"), !anonymous, "{request}");
            // Reacquiring the same saved login must not authenticate an anonymous layer.
            app.run("connection.login", json!({"id":id,"apiKey":"ui-whitespace-757","allowInsecure":true})).unwrap();
            let layer = app.session.map().layer_by_name("Downloaded parks").unwrap();
            assert_eq!(layer.dataset.features.len(), 1);
            let extent = layer.extent();
            assert!((extent.min_x + 73.96).abs() < 0.05 && (extent.min_y - 40.78).abs() < 0.05, "{extent:?}");
            if anonymous {
                assert!(layer.anonymous);
                assert!(layer.connection.is_empty());
                assert!(layer.dataset.source.as_deref().is_none_or(|source| logins::apply(source).unwrap().is_none()));
            } else {
                assert_eq!(layer.connection, id.as_str().unwrap());
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod vertex_undo_tests {
    use super::RustGisApp;
    use rustgis_engine::data::VertexId;
    use serde_json::{Value, json};

    /// An undone vertex insertion leaves no vertex chosen: its index would name the next vertex,
    /// and Delete would remove that one (#428).
    #[test]
    fn undo_and_redo_clear_the_chosen_vertex() {
        let mut app = RustGisApp::with_sample();
        app.run("layer.template", json!({"template": "lines", "name": "Paths"})).unwrap();
        app.run("edit.enable", json!({"on": true})).unwrap();
        let geometry = json!({"type": "LineString", "coordinates": [[0, 0], [1, 0], [2, 0]]});
        let row = app.run("edit.addFeature", json!({"layer": "Paths", "geometry": geometry})).unwrap()["row"].as_u64().unwrap() as usize;
        let id = app.session.map().layer_by_name("Paths").unwrap().id;
        let insert = json!({"layer": id, "row": row, "action": "insert", "index": 0, "lon": 0.5, "lat": 0});
        app.run("edit.vertex", insert).unwrap();
        let chosen = Some((id, row, VertexId::new(0, 0, 1)));
        crate::vertexedit::choose(&mut app, chosen);
        assert_eq!(app.map.vertex, chosen, "chosen before undo");
        app.run("edit.undo", Value::Null).unwrap();
        assert_eq!(app.map.vertex, None, "undo");
        crate::vertexedit::choose(&mut app, chosen);
        assert_eq!(app.map.vertex, chosen, "chosen before redo");
        app.run("edit.redo", Value::Null).unwrap();
        assert_eq!(app.map.vertex, None, "redo");
    }
}

#[cfg(test)]
mod download_service_tests {
    use super::{RustGisApp, Services, complete_gp_file};
    use serde_json::json;

    /// The real asynchronous completion receiver owns the forwarding notice, even when the
    /// layer is added successfully. Input is already downloaded into the usual completion slot.
    #[test]
    fn tile_resolver_completion_reports_query_settings_it_withheld() {
        let mut missing_notices = Vec::new();
        for (query, destination, expected, count) in [
            ("collections=roads", "", "?collections=roads", 0),
            ("token=RGSYNTH_ui_20261103&collections=roads", "", "?collections=roads", 1),
            ("collections=roads", "?collections=z", "?collections=z", 1),
            ("collections=roads", "?collections=roads", "?collections=roads", 0),
        ] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let list = format!("https://h.invalid/svc/tiles?{query}");
            let capabilities = json!({"title":"roads", "links":[{"rel":"tilesets-map", "href":list}]}).to_string();
            let body = json!({"links":[{"rel":"item", "type":"image/png",
                "href":format!("https://elsewhere.invalid/{{tileMatrix}}/{{tileRow}}/{{tileCol}}{destination}")}]})
            .to_string()
            .into_bytes();
            let resolve = super::TilesResolve {
                params: json!({"url":"https://h.invalid/svc", "type":"ogcapi", "layer":"map", "text":capabilities}),
                list: list.clone(),
                doc: list,
                crossed_origin: false,
                read: 0,
                withheld_addresses: 0,
                vector: false,
                fallback: "https://h.invalid/svc/tiles/{z}/{y}/{x}".into(),
                slot: std::sync::Arc::new(std::sync::Mutex::new(Some(Ok(body)))),
            };
            app.ui.service_steps.push(super::ServiceStep::Tiles { resolve, dialog: true, origin: None, opened: app.session.opened });
            app.poll_service_steps();
            assert!(app.ui.service_steps.is_empty());
            let notices: Vec<_> = app.session.notices.iter().filter(|n| n.text.contains("address(es) the service listed")).collect();
            if notices.len() != count {
                missing_notices.push(format!("{query} -> {destination}: expected {count} notices, got {}", notices.len()));
            }
            if let Some(notice) = notices.first() {
                assert!(notice.text.starts_with("1 address(es)"), "{notices:?}");
            }
            assert_eq!(app.session.map().tiles.len(), 1);
            assert_eq!(app.session.map().tiles[0].url, format!("https://elsewhere.invalid/{{z}}/{{y}}/{{x}}{expected}"));
            assert!(app.session.map().tiles.iter().all(|t| !t.url.contains("RGSYNTH_")));
            assert!(app.dialogs.error.is_none(), "{:?}", app.dialogs.error);
        }
        // Refusing the advertised next document still reports the loss already counted.
        // This is the existing receiver slot: no fetch starts, and polling it again says nothing.
        for (query, count) in [("collections=roads", 0), ("token=RGSYNTH_ui_refused&collections=roads", 1)] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let list = format!("https://h.invalid/svc/tiles?{query}");
            let body = json!({"tilesets":[{"tileMatrixSetId":"WebMercatorQuad",
                "links":[{"rel":"self", "href":"http://127.0.0.1/tiles"}]}]})
            .to_string()
            .into_bytes();
            let resolve = super::TilesResolve {
                params: json!({"url":"https://h.invalid/svc", "type":"ogcapi", "layer":"map"}),
                list: list.clone(),
                doc: list,
                crossed_origin: false,
                read: 0,
                withheld_addresses: 0,
                vector: false,
                fallback: "https://h.invalid/svc/tiles/{z}/{y}/{x}".into(),
                slot: std::sync::Arc::new(std::sync::Mutex::new(Some(Ok(body)))),
            };
            app.ui.service_steps.push(super::ServiceStep::Tiles { resolve, dialog: false, origin: None, opened: app.session.opened });
            app.poll_service_steps();
            assert!(app.ui.service_steps.is_empty() && app.session.map().tiles.is_empty());
            let notices: Vec<_> = app.session.notices.iter().filter(|n| n.text.contains("address(es) the service listed")).collect();
            if notices.len() != count {
                missing_notices.push(format!("refused next for {query}: expected {count} notices, got {}", notices.len()));
            }
            assert_eq!(app.session.notices.iter().filter(|n| n.error).count(), 1);
            assert!(app.session.notices.iter().any(|n| n.error && n.text.contains("local network")));
            let before = app.session.notices.len();
            app.poll_service_steps();
            assert_eq!(app.session.notices.len(), before, "no repeated refusal or loss notice");
        }
        assert!(missing_notices.is_empty(), "{missing_notices:?}");
    }

    /// Real UI discovery never restores a login after a foreign hop, while a public saved
    /// connection on a same-origin chain can still gain its legitimate login later.
    #[test]
    fn tile_discovery_keeps_foreign_lineage_and_same_origin_connections() {
        use rustgis_engine::render::rustgis_tiles::{cache, logins};
        use std::sync::{Arc, Mutex};
        let _turn = crate::fetching::turn();
        let a = "https://ui-tile-lineage-a.invalid";
        let b = "https://ui-tile-lineage-b.invalid";
        for (foreign, returns, login) in [(true, true, true), (true, false, true), (false, false, true), (false, false, false)] {
            let middle = format!("{}/set", if foreign { b } else { a });
            let last = format!("{}/final", if foreign && !returns { b } else { a });
            let next = |url: &str| json!({"tilesets":[{"tileMatrixSetURI":"WebMercatorQuad", "links":[{"rel":"self","href":url}]}]});
            let documents = [
                (format!("{a}/list"), next(&middle).to_string().into_bytes()),
                (middle.clone(), next(&last).to_string().into_bytes()),
                (last.clone(), json!({"links":[{"rel":"item","type":"image/png", "href":format!("{a}/tiles/{{tileMatrix}}/{{tileRow}}/{{tileCol}}.png")}]}).to_string().into_bytes()),
            ];
            let requests = Arc::new(Mutex::new(Vec::new()));
            let seen = requests.clone();
            cache::set_fetcher(move |url, done| {
                let (url, _) = cache::split_download_limit(&url);
                let plain = logins::inner(url).split('?').next().unwrap();
                if ![a, b].iter().any(|origin| plain == *origin || plain.starts_with(&format!("{origin}/"))) {
                    done(Err("outside the lineage fixture".into()));
                    return;
                }
                seen.lock().unwrap().push((plain.to_string(), logins::apply(url).unwrap().is_some()));
                done(Ok(documents.iter().find(|(at, _)| at == plain).map(|(_, body)| body.clone()).unwrap_or_default()));
            });
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let mut connection = String::new();
            for (origin, authenticated) in [(a, login), (b, true)] {
                let c = app
                    .run(
                        "connection.new",
                        json!({"kind":"ogcapi","address":origin,
                    "auth":if authenticated {"header"} else {"none"}, "keyName":if authenticated {"X-Key"} else {""}}),
                    )
                    .unwrap();
                if authenticated {
                    app.run("connection.login", json!({"id":c["id"],"apiKey":"synthetic-ui-lineage"})).unwrap();
                }
                if origin == a {
                    connection = c["id"].as_str().unwrap().to_string();
                }
            }
            let text = json!({"title":"Lineage","links":[{"rel":"tilesets-map","href":format!("{a}/list")}]}).to_string();
            let source = crate::serviceui::ServiceSource { url: a.into(), kind: "ogcapi", text: text.clone(), documents: Vec::new() };
            let listing = app.session.run("service.layers", &json!({"url":a,"type":"ogcapi","text":text,"connection":connection})).unwrap();
            crate::serviceui::add_layer_from(&mut app, &source, &listing["layers"][0], "", None, false, None, Some(&connection)).unwrap();
            app.ws.recording = Some(Vec::new());
            for _ in 0..3 {
                app.poll_service_steps();
            }
            assert!(app.ui.service_steps.is_empty());
            assert_eq!(
                app.session.map().tiles.len(),
                1,
                "foreign={foreign}, returns={returns}, login={login}: {:?}",
                app.session.notices.iter().map(|n| &n.text).collect::<Vec<_>>()
            );
            let steps = app.ws.recording.take().unwrap();
            assert_eq!(steps.len(), usize::from(!foreign));
            assert_eq!(
                app.session.notices.iter().filter(|n| n.text.contains("tile discovery crossed origins")).count(),
                usize::from(foreign),
                "foreign={foreign}, returns={returns}, login={login}: {:?}",
                app.session.notices.iter().map(|n| &n.text).collect::<Vec<_>>()
            );
            if !foreign {
                assert_eq!(steps[0].command, "service.add");
                assert_eq!(steps[0].params["connection"], connection);
                assert!(!steps[0].params.to_string().contains(rustgis_engine::data::login_address::PREFIX));
            }
            assert_eq!(*requests.lock().unwrap(), [(format!("{a}/list"), login), (middle, login && !foreign), (last, login && !foreign),]);
            let tile = app.session.map().tiles.first().unwrap();
            assert_eq!(tile.anonymous, foreign);
            assert_eq!(tile.connection, if foreign { "" } else { &connection });
            assert_eq!(logins::apply(&tile.url).unwrap().is_some(), login && !foreign);
            app.run("connection.edit", json!({"id":connection,"auth":"header","keyName":"X-Key"})).unwrap();
            app.run("connection.login", json!({"id":connection,"apiKey":"synthetic-ui-lineage-later"})).unwrap();
            let (saved, _) = app.session.project.saved_text(None).unwrap();
            app.run("project.openText", json!({"text":saved})).unwrap();
            app.run("connection.login", json!({"id":connection,"apiKey":"synthetic-ui-lineage-reopened"})).unwrap();
            let tile = app.session.map().tiles.first().unwrap();
            assert_eq!(tile.anonymous, foreign);
            assert_eq!(tile.connection, if foreign { "" } else { &connection });
            assert_eq!(logins::apply(&tile.url).unwrap().is_some(), !foreign);
            let url = tile.url.replace("{z}", "0").replace("{y}", "0").replace("{x}", "0");
            cache::get(&url, cache::Want::Image);
            assert_eq!(
                requests.lock().unwrap().last(),
                Some(&(logins::inner(&url).to_string(), !foreign)),
                "render fetch preserves lineage after reload"
            );
        }
    }

    /// A failed step clears its own "still working" line beside the error; one that lands after a
    /// newer add leaves that add's line alone and is told as a notice.
    #[test]
    fn a_failed_service_step_clears_only_its_own_line() {
        for (newer, open) in [(false, true), (true, true), (false, false)] {
            let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
            let params = json!({"url":"https://h.invalid/wcs", "name":"Heights"});
            let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(Err("the server is down".to_string()))));
            let opened = app.session.opened;
            app.ui.service_steps.push(super::ServiceStep::Coverage { params, slot, dialog: true, origin: None, opened });
            app.dialogs.info = Some(if newer { "Reading the tileset of Roads" } else { "Describing Heights" }.into());
            app.dialogs.open = open.then_some("addService");
            app.poll_service_steps();
            if !open {
                // Closed, the dialog would keep the error unseen: it is told as a notice.
                assert!(app.dialogs.error.is_none(), "{:?}", app.dialogs.error);
                assert!(app.session.notices.iter().any(|n| n.error && n.text.contains("the server is down")));
            } else if newer {
                assert_eq!(app.dialogs.info.as_deref(), Some("Reading the tileset of Roads"), "the newer line stays");
                assert!(app.dialogs.error.is_none(), "{:?}", app.dialogs.error);
                assert!(app.session.notices.iter().any(|n| n.error && n.text.contains("the server is down")));
            } else {
                assert!(app.dialogs.info.is_none(), "the step's own line is cleared: {:?}", app.dialogs.info);
                assert!(app.dialogs.error.as_deref().is_some_and(|e| e.contains("the server is down")), "{:?}", app.dialogs.error);
            }
        }
    }

    #[test]
    fn retired_service_steps_clear_only_their_own_pending_dialog() {
        for coverage in [false, true] {
            for newer in 0..3 {
                let mut app = RustGisApp::new(rustgis_engine::Session::new(rustgis_engine::Project::default()));
                let opened = app.session.opened;
                let params = json!({"url":"https://retired.invalid/service", "name":"Old read"});
                let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
                let step = |opened, slot: std::sync::Arc<std::sync::Mutex<Option<Result<Vec<u8>, String>>>>| {
                    if coverage {
                        super::ServiceStep::Coverage { params: params.clone(), slot, dialog: true, origin: None, opened }
                    } else {
                        super::ServiceStep::Tiles {
                            resolve: super::TilesResolve {
                                params: params.clone(),
                                list: "https://retired.invalid/list".into(),
                                doc: "https://retired.invalid/list".into(),
                                crossed_origin: false,
                                read: 0,
                                withheld_addresses: 0,
                                vector: false,
                                fallback: "https://retired.invalid/{z}/{y}/{x}".into(),
                                slot: slot.clone(),
                            },
                            dialog: true,
                            origin: None,
                            opened,
                        }
                    }
                };
                app.ui.service_steps.push(step(opened, slot.clone()));
                app.session.run("project.new", &json!({})).unwrap();
                let pending = if coverage { "Describing Old read" } else { "Reading the tileset of Old read" };
                if newer == 2 {
                    app.ui.service_steps.push(step(app.session.opened, Default::default()));
                }
                app.dialogs.info = Some(if newer == 1 { "New operation" } else { pending }.into());
                *slot.lock().unwrap() = Some(Ok(b"late completion".to_vec()));
                app.poll_service_steps();
                assert_eq!(app.ui.service_steps.len(), usize::from(newer == 2));
                assert!(app.session.map().tiles.is_empty() && app.session.map().layers.is_empty());
                assert!(app.session.notices.iter().any(|n| n.text.contains("its project was closed while it was read")));
                assert_eq!(
                    app.dialogs.info.as_deref(),
                    match newer {
                        1 => Some("New operation"),
                        2 => Some(pending),
                        _ => None,
                    }
                );
            }
        }
    }

    #[test]
    fn rejected_download_does_not_announce_success_or_rename_project() {
        let mut app = RustGisApp::with_sample()
            .with_services(Services { download: Some(Box::new(|_, _| Err("browser rejected download".into()))), ..Default::default() });
        let name = app.session.project.name.clone();
        let saved_name = app.ui.web_file.clone();
        assert!(app.save_download("renamed.rgis").is_err());
        assert_eq!(app.session.project.name, name);
        assert_eq!(app.ui.web_file, saved_name);
        assert!(!app.session.notices.iter().any(|n| n.text.contains("Downloaded")));
        assert!(!app.download("archive.pmtiles", vec![1, 2, 3]));
        assert!(app.session.notices.iter().any(|n| n.error && n.text.contains("browser rejected download")));
        assert!(!app.session.notices.iter().any(|n| n.text.contains("Downloaded")));
    }

    /// The web app's Shapefile download says which field names it changed, once the browser takes
    /// the file; names that fit, other formats and a refused download say nothing of them.
    #[test]
    fn shapefile_download_says_which_field_names_it_changed() {
        use rustgis_engine::data::{Dataset, Field, FieldType, Geometry, GeometryType, Value};
        use rustgis_engine::geom::Coord;
        let changed = |app: &RustGisApp| app.session.notices.iter().filter(|n| n.text.contains("changed to fit a Shapefile")).count();
        let with = |accept: bool| {
            let download: Box<dyn Fn(&str, Vec<u8>) -> Result<(), String>> =
                if accept { Box::new(|_, _| Ok(())) } else { Box::new(|_, _| Err("browser rejected download".into())) };
            let mut app = RustGisApp::with_sample().with_services(Services { download: Some(download), ..Default::default() });
            let mut ds = Dataset::new(
                "Sites",
                GeometryType::Point,
                vec![
                    Field::new("population_2020", FieldType::Integer),
                    Field::new("notes", FieldType::Text),
                    Field::new("big", FieldType::Integer),
                ],
            );
            ds.push(
                Some(Geometry::Point(Coord::new(1.0, 2.0))),
                vec![Value::Int(1), Value::Text("x".repeat(300)), Value::Int(9_007_199_254_740_993)],
            );
            let id = app.session.map_mut().add_dataset(ds);
            app.session.active_layer = Some(id);
            app
        };
        let mut app = with(true);
        // The browser's export refuses a GeoPackage mode for another format, as the desktop's does.
        assert!(app.run("ui.exportLayer", json!({"format": "geojson", "mode": "replaceTable"})).is_err());
        assert_eq!(changed(&app), 0);
        // As the command reads them: no table name given is none.
        app.run("ui.exportLayer", json!({"format": "geojson", "mode": " newFile ", "tableName": ""})).unwrap();
        assert_eq!(changed(&app), 0, "GeoJSON keeps the names");
        app.run("ui.exportLayer", json!({"format": "shp"})).unwrap();
        assert_eq!(changed(&app), 1);
        assert!(app.session.notices.iter().any(|n| n.text == "Sites.zip: 1 text value was cut to at most 254 bytes, in 'notes'"));
        assert!(
            app.session
                .notices
                .iter()
                .any(|n| n.text == "Sites.zip: 1 value couldn't be written to its dBase field and was left empty, in 'big'")
        );
        assert!(app.session.notices.iter().any(|n| n.text
            == "Sites.zip: 1 field name was changed to fit a Shapefile (10 bytes at most, unique ignoring case): \
                'population_2020' → 'population'"));
        app.run("ui.exportLayer", json!({"format": "geojson"})).unwrap();
        assert_eq!(changed(&app), 1, "GeoJSON keeps the name");
        let mut app = with(false);
        let _ = app.run("ui.exportLayer", json!({"format": "shp"}));
        assert!(app.session.notices.iter().any(|n| n.error && n.text.contains("browser rejected download")));
        assert_eq!(changed(&app), 0, "nothing was downloaded");
        assert!(!app.session.notices.iter().any(|n| n.text.contains("254 bytes") || n.text.contains("left empty")));
        let mut app = with(true);
        let countries = app.session.map().layers.iter().find(|l| l.name == "Countries").map(|l| l.id);
        app.session.active_layer = countries;
        app.run("ui.exportLayer", json!({"format": "shp"})).unwrap();
        assert_eq!(changed(&app), 0, "the sample's names fit");
        assert!(!app.session.notices.iter().any(|n| n.text.contains("254 bytes") || n.text.contains("left empty")));
    }

    /// Export Features into an existing GeoPackage: the file is picked to open, not to save over,
    /// and the layer goes in as the table named in the dialog, beside the file's others.
    #[test]
    fn export_features_adds_a_table_to_a_picked_geopackage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("county.gpkg");
        let mut app = RustGisApp::with_sample();
        app.run("share.exportLayer", json!({"layer": "Cities", "path": path})).unwrap();
        app.session.active_layer = app.session.map().layer_by_name("Countries").map(|l| l.id);
        app.run("ui.dialog", json!({"name": "exportLayer"})).unwrap();
        let (id, instance) = app.dialogs.export_layer.unwrap();
        // Another layer selected while the dialog is open: the dialog's is the one exported.
        app.session.active_layer = app.session.map().layer_by_name("Cities").map(|l| l.id);
        crate::dialogs::PICKED.with(|p| *p.borrow_mut() = Some(path.display().to_string()));
        let p = json!({"format": "gpkg", "mode": "addTable", "tableName": "borders", "layer": id, "instance": instance});
        app.run("ui.exportLayer", p).unwrap();
        // Written on a thread of its own: waits for it to land.
        let end = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while crate::exportjob::land(&mut app) {
            assert!(std::time::Instant::now() < end, "the export never landed");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(rustgis_engine::data::source::layers(&path).unwrap(), ["Cities", "borders"]);
        let countries = app.session.map().layer_by_name("Countries").map(|l| l.dataset.features.len() as u64);
        assert_eq!(rustgis_engine::data::source::open_path(&path, Some("borders")).unwrap().info().count, countries);
        // An unknown mode is refused before a file is asked for (the pick isn't taken).
        crate::dialogs::PICKED.with(|p| *p.borrow_mut() = Some(path.display().to_string()));
        assert!(app.run("ui.exportLayer", json!({"format": "gpkg", "mode": "append"})).unwrap_err().contains("unknown mode"));
        assert!(crate::dialogs::PICKED.with(|p| p.borrow_mut().take()).is_some());
        // A layer by name is that layer; one not there is refused, not taken for the active one.
        let e = app.run("ui.exportLayer", json!({"format": "gpkg", "mode": "addTable", "layer": "Nowhere"})).unwrap_err();
        assert!(e.contains("Nowhere"), "{e}");
        // A layer or instance of the wrong kind is refused, not taken for the active or a reused layer.
        let e =
            app.run("ui.exportLayer", json!({"format": "gpkg", "mode": "replaceTable", "tableName": "Cities", "layer": true})).unwrap_err();
        assert!(e.contains("\"layer\""), "{e}");
        let e = app
            .run("ui.exportLayer", json!({"format": "gpkg", "mode": "replaceTable", "tableName": "Cities", "layer": id, "instance": "7"}))
            .unwrap_err();
        assert!(e.contains("\"instance\""), "{e}");
        // Cancelling the pick writes nothing.
        let before = std::fs::read(&path).unwrap();
        app.run("ui.exportLayer", json!({"format": "gpkg", "mode": "replaceTable", "tableName": "Cities"})).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn direct_file_result_reports_delivery_instead_of_pending() {
        let pending = json!({"pending": true, "download": 7, "file": "cities.pmtiles"});
        let completed = json!({"file": "cities.pmtiles", "delivered": true, "messages": ["Exported cities"]});
        assert_eq!(complete_gp_file(Ok(pending.clone()), 7, &Ok(completed.clone())), Ok(completed));
        assert_eq!(complete_gp_file(Ok(pending), 7, &Err("browser rejected download".into())), Err("browser rejected download".into()));
    }

    #[test]
    fn delivered_script_and_model_results_keep_their_wrappers_and_other_outputs() {
        let script = json!({
            "ran": [
                {"line": 1, "command": "model.run", "result": {
                    "model": "Export cities",
                    "ran": [
                        {"step": 1, "node": "export", "tool": "vectorToPmtiles",
                            "result": {"pending": true, "download": 7, "file": "cities.pmtiles"}},
                        {"step": 2, "node": "buffer", "tool": "buffer", "result": {"layer": 42}}
                    ],
                    "outputs": [{"node": "buffer", "layer": 42}]
                }},
                {"line": 2, "command": "map.zoom", "result": {"zoom": 5}}
            ]
        });
        let completed = json!({"file": "cities.pmtiles", "delivered": true, "messages": ["Exported cities"]});
        let mut expected = script.clone();
        expected["ran"][0]["result"]["ran"][0]["result"] = completed.clone();
        assert_eq!(complete_gp_file(Ok(script), 7, &Ok(completed)), Ok(expected));
    }

    #[test]
    fn any_matching_failed_export_fails_the_compound_command() {
        let script = json!({"ran": [
            {"line": 1, "command": "gp.run", "result": {"pending": true, "download": 7, "file": "cities.pmtiles"}},
            {"line": 2, "command": "gp.run", "result": {"pending": true, "download": 8, "file": "roads.pmtiles"}}
        ]});
        let cities = Ok(json!({"file": "cities.pmtiles", "delivered": true, "messages": []}));
        let after_first = complete_gp_file(Ok(script), 7, &cities);
        assert_eq!(complete_gp_file(after_first, 8, &Err("roads download rejected".into())), Err("roads download rejected".into()));
    }

    #[test]
    fn unrelated_delivery_failures_do_not_change_the_command_result() {
        let script = json!({"ran": [
            {"line": 1, "command": "gp.run", "result": {"pending": true, "download": 7, "file": "cities.pmtiles"}},
            {"line": 2, "command": "layer.color", "params": {"download": 8}, "result": {"layer": 42}}
        ]});
        assert_eq!(complete_gp_file(Ok(script.clone()), 8, &Err("unrelated download failed".into())), Ok(script));
        assert_eq!(
            complete_gp_file(Err("script command failed".into()), 7, &Err("download failed".into())),
            Err("script command failed".into())
        );
    }

    #[test]
    fn acknowledgment_errors_still_have_one_error_notice() {
        let mut app = RustGisApp::with_sample();
        assert_eq!(app.run("gp.fileDelivered", json!({"id": u64::MAX})), Err("no pending file with that id".into()));
        assert_eq!(app.session.notices.iter().filter(|n| n.error && n.text == "no pending file with that id").count(), 1);
        let mark = app.session.notice_mark();
        let error = app.acknowledge_gp_file(u64::MAX, None).unwrap_err();
        assert_eq!(error, "no pending file with that id");
        // A surrounding command uses the same reporter; the helper's notice must not be repeated.
        app.command_failed("gp.fileDelivered", &error, mark);
        assert_eq!(app.session.notices_since(mark).iter().filter(|n| n.error && n.text == error).count(), 1);
    }
}

#[cfg(test)]
mod peer_output_tests {
    use super::*;

    /// Drives the actual receiving wrapper, including the UI state a direct Session call misses.
    #[test]
    fn peer_project_uses_import_authority_and_resets_old_ui_work() {
        use rustgis_engine::render::rustgis_tiles::cache;
        use std::sync::{Arc, Mutex};
        let _turn = crate::fetching::turn();
        let mut app = RustGisApp::with_sample();
        app.session.project.maps[0].layers[0].dataset.source = Some("https://h.invalid/?token=RGSYNTH_4".into());
        let (text, _) = app.session.project.saved_text(None).unwrap();
        app.session.run("task.new", &json!({"name":"prior","steps":[]})).unwrap();
        app.assistant.queued = Some("old project action".into());
        app.ui.web_file = Some("prior.rgis".into());
        let generation = app.session.opened;
        app.ui.cog_probes.push((
            json!({"url":"https://peer-probe.invalid/elevation.tif?token=RGSYNTH_peer_probe","name":"Elevation"}),
            None,
            Default::default(),
        ));
        app.dialogs.info = Some(crate::serviceui::downloading("Elevation"));
        let probe_mark = app.session.notice_mark();
        app.open_peer_text(&text).unwrap();
        assert_ne!(app.session.opened, generation);
        assert!(app.ui.cog_probes.is_empty());
        assert!(app.dialogs.info.is_none(), "a retired peer probe cannot keep the reading message open");
        let retired = app
            .session
            .notices_since(probe_mark)
            .iter()
            .filter(|n| n.text.contains("peer-probe.invalid/elevation.tif") && n.text.contains("project was closed"))
            .count();
        assert_eq!(retired, 1, "the peer replacement must say which pending read was omitted");
        assert!(app.session.notices_since(probe_mark).iter().all(|n| !n.text.contains("RGSYNTH_peer_probe")));
        let clean_mark = app.session.notice_mark();
        app.open_peer_text(&text).unwrap();
        assert!(
            app.session.notices_since(clean_mark).iter().all(|n| !n.text.contains("project was closed")),
            "a clean peer replacement does not replay a retired read"
        );
        assert!(app.session.project.tasks.is_empty());
        assert!(app.ui.web_file.is_none() && app.assistant.queued.is_none());
        assert!(!serde_json::to_string(&app.session.project).unwrap().contains("RGSYNTH_4"));

        // Both replacement paths retire ready answers and reads still waiting for their document.
        for peer in [false, true] {
            for ready in [false, true] {
                let sent = Arc::new(Mutex::new(Vec::new()));
                let held = Arc::new(Mutex::new(Vec::<(String, cache::Done)>::new()));
                let (seen, callbacks) = (sent.clone(), held.clone());
                cache::set_fetcher(move |url, done| {
                    let plain = cache::split_download_limit(&url).0.to_string();
                    if !plain.starts_with("https://review29-service.invalid/") {
                        done(Err("outside the service fixture".into()));
                        return;
                    }
                    seen.lock().unwrap().push(plain.clone());
                    callbacks.lock().unwrap().push((plain, done));
                });
                let mut app = RustGisApp::with_sample();
                let (text, _) = app.session.project.saved_text(None).unwrap();
                let api = "https://review29-service.invalid/collections";
                let caps = json!({"collections":[
                    {"id":"next","links":[{"rel":"tilesets-map","href":format!("{api}/next/tiles")}]},
                    {"id":"ready","links":[{"rel":"tilesets-map","href":format!("{api}/ready/tiles")}]}
                ]})
                .to_string();
                let listing = app.session.run("service.layers", &json!({"url":api,"type":"ogcapi","text":caps})).unwrap();
                let source = crate::serviceui::ServiceSource { url: api.into(), kind: "ogcapi", text: caps, documents: Vec::new() };
                for layer in listing["layers"].as_array().unwrap() {
                    crate::serviceui::add_layer_from(&mut app, &source, layer, "", None, ready, None, None).unwrap();
                }
                let caps = r#"<wcs:Capabilities version="2.0.1" xmlns:wcs="http://www.opengis.net/wcs/2.0" xmlns:ows="http://www.opengis.net/ows/2.0">
                <wcs:Contents><wcs:CoverageSummary><wcs:CoverageId>soc</wcs:CoverageId><ows:Title>Old coverage</ows:Title>
                <ows:WGS84BoundingBox><ows:LowerCorner>5 49.2</ows:LowerCorner><ows:UpperCorner>5.8 50</ows:UpperCorner></ows:WGS84BoundingBox>
                </wcs:CoverageSummary></wcs:Contents></wcs:Capabilities>"#;
                let url = "https://review29-service.invalid/wcs";
                let listing = app.session.run("service.layers", &json!({"url":url,"type":"wcs","text":caps})).unwrap();
                let source = crate::serviceui::ServiceSource { url: url.into(), kind: "wcs", text: caps.into(), documents: Vec::new() };
                crate::serviceui::add_layer_from(&mut app, &source, &listing["layers"][0], "", None, ready, None, None).unwrap();
                assert_eq!(app.ui.service_steps.len(), 3);
                assert_eq!(sent.lock().unwrap().len(), 3);
                let finish = || {
                    for (url, done) in std::mem::take(&mut *held.lock().unwrap()) {
                        let body = if url.contains("/next/tiles") {
                            json!({"tilesets":[{"tileMatrixSetURI":"WebMercatorQuad","links":[{"rel":"self","href":"https://review29-service.invalid/next-document"}]}]}).to_string()
                        } else if url.contains("/ready/tiles") {
                            json!({"links":[{"rel":"item","type":"image/png","href":"https://review29-service.invalid/tiles/{tileMatrix}/{tileRow}/{tileCol}.png"}]}).to_string()
                        } else {
                            r#"<CoverageDescriptions><CoverageDescription><Envelope srsName="http://www.opengis.net/def/crs/EPSG/0/4326" axisLabels="Lat Long"><lowerCorner>49.2 5</lowerCorner><upperCorner>50 5.8</upperCorner></Envelope></CoverageDescription></CoverageDescriptions>"#.into()
                        };
                        done(Ok(body.into_bytes()));
                    }
                };
                if ready {
                    finish();
                }
                if peer {
                    app.open_peer_text(&text).unwrap();
                } else {
                    app.session.run("project.openText", &json!({"text":text})).unwrap();
                }
                let (layers, tiles) = (app.session.map().layers.len(), app.session.map().tiles.len());
                app.dialogs.info = Some("Current project info".into());
                app.dialogs.error = Some("Current project error".into());
                let mark = app.session.notice_mark();
                app.poll_service_steps();
                let drained = app.ui.service_steps.is_empty();
                if !ready {
                    finish();
                }
                app.poll_service_steps();
                // Drain any forbidden follow-up too, so a failing test leaves no in-flight fetch.
                for (_, done) in std::mem::take(&mut *held.lock().unwrap()) {
                    done(Err("stale service request".into()));
                }
                assert!(drained && app.ui.service_steps.is_empty(), "old steps must retire before inspecting their slots");
                assert_eq!(sent.lock().unwrap().len(), 3, "no old next-document or coverage request may start");
                assert_eq!((app.session.map().layers.len(), app.session.map().tiles.len()), (layers, tiles));
                assert!(app.ui.downloads.is_empty() && app.ui.cog_probes.is_empty());
                assert_eq!(app.dialogs.info.as_deref(), Some("Current project info"));
                assert_eq!(app.dialogs.error.as_deref(), Some("Current project error"));
                assert_eq!(app.session.notices_since(mark).iter().filter(|n| n.text.contains("project was closed")).count(), 3);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod look_tests {
    use super::RustGisApp;
    use crate::theme;

    /// App whose preferences answer only these keys, with a theme no folder ships.
    fn app_with(prefs: &'static [(&'static str, &'static str)]) -> RustGisApp {
        let mut app = RustGisApp::with_sample();
        app.themes.push(theme::Theme {
            name: "Mine".to_string(),
            dark: true,
            tokens: theme::Tokens::dark(),
            metrics: theme::Metrics::standard(),
            notes: Vec::new(),
        });
        app.services.load_pref = Some(Box::new(move |key: &str| prefs.iter().find(|(k, _)| *k == key).map(|(_, v)| (*v).to_string())));
        app
    }

    #[test]
    fn theme_startup_reads_after_the_saved_language_is_selected() {
        struct RestoreLanguage(crate::i18n::Lang);
        impl Drop for RestoreLanguage {
            /// Restores this test thread's caller language even if the startup assertion fails.
            fn drop(&mut self) {
                crate::i18n::set_current(self.0);
            }
        }
        let _restore = RestoreLanguage(crate::i18n::current());
        crate::i18n::set_current(crate::i18n::Lang::EN);
        let app = RustGisApp::with_sample();
        assert_eq!(app.themes_revision, 0, "construction must not read user themes or emit their notices");
        assert_eq!(app.themes.len(), 2, "built-in themes remain available before platform services");
        let services = super::Services {
            load_pref: Some(Box::new(|key| {
                if key == "language" {
                    return Some("de".to_string());
                }
                if key == "theme" {
                    assert_eq!(crate::i18n::current().code(), "de", "the theme folder was read in the chosen language");
                }
                None
            })),
            ..super::Services::default()
        };
        let app = app.with_services(services);
        assert_eq!(app.themes_revision, 1, "the folder is read once during preference restoration");
        assert_eq!(crate::i18n::current().code(), "de");
    }

    #[test]
    fn the_saved_theme_is_taken_at_start() {
        let mut app = app_with(&[("theme", "Mine"), ("ui_scale", "9")]);
        app.take_saved_look();
        assert_eq!(app.ui.theme_name, "Mine");
        assert!(app.ui.dark, "the theme brings its own brightness");
        assert!(!app.ui.dark_follows_system, "a chosen theme does not follow the system");
        assert_eq!(app.ui.interface_scale, 2.0, "the scale is held at 200 percent");
        assert_eq!(app.current_theme().name, "Mine");
    }

    #[test]
    fn a_platform_that_reports_no_theme_names_the_brightness() {
        crate::i18n::set_current(crate::i18n::Lang::EN);
        let mut app = app_with(&[]);
        app.take_saved_look();
        assert!(app.ui.dark_follows_system, "a fresh app follows the system");
        let (follows, name) = app.theme_shown(false);
        assert!(!follows, "a platform that reports no theme is not followed");
        assert_eq!(name, "Light");
        app.ui.dark = true;
        assert_eq!(app.theme_shown(false).1, "Dark");
        let (follows, name) = app.theme_shown(true);
        assert!(follows, "a reported theme is followed");
        assert_eq!(name, "Follow the System");
    }

    #[test]
    fn a_picked_theme_file_bigger_than_the_limit_is_refused_unread() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let path = dir.path().join("huge.json");
        std::fs::write(&path, "x".repeat(70 * 1024)).expect("the file");
        let mut app = app_with(&[]);
        let why = app.load_theme_file(&path).expect_err("a 70 KB file is refused");
        assert!(why.contains("64 KB"), "{why}");
    }

    #[test]
    fn the_menu_owns_the_interface_scale() {
        let mut app = app_with(&[]);
        app.take_saved_look();
        let log = saved_by(&mut app);
        let ctx = egui::Context::default();
        // A zoom factor lands at the end of the pass that sets it.
        let settle = |ctx: &egui::Context| {
            let mut pass = ctx.run_ui(egui::RawInput::default(), |_| {});
            pass.textures_delta.clear();
        };
        app.logic(&ctx);
        settle(&ctx);
        assert!(!ctx.options(|o| o.zoom_with_keyboard), "the zoom keys are off, so the scale cannot move behind the menu");
        // A scale from the menu is drawn, and the next frame does not undo it.
        app.run("ui.scale", serde_json::json!({ "factor": 1.25 })).expect("the command");
        app.logic(&ctx);
        settle(&ctx);
        app.logic(&ctx);
        settle(&ctx);
        assert_eq!(app.ui.interface_scale, 1.25, "the menu scale is kept, not read back as the old zoom");
        assert!((ctx.zoom_factor() - 1.25).abs() < 0.001, "and the interface is drawn at it");
        assert!(log.lock().expect("the log").iter().any(|(k, v)| k == "ui_scale" && v == "1.25"));
        app.run("ui.scale", serde_json::json!({ "factor": 1.234 })).expect("a fractional scale");
        let kept = log.lock().expect("the log").iter().rev().find(|(k, _)| k == "ui_scale").unwrap().1.clone();
        assert_eq!(kept.parse::<f32>().expect("saved scale"), app.ui.interface_scale, "restart retains full precision");
        let before = app.ui.interface_scale;
        assert!(app.run("ui.scale", serde_json::json!({"factr": 1.5})).is_err());
        assert_eq!(app.ui.interface_scale, before, "an unknown parameter does not reset the scale");
        // Past the limit the command holds the scale.
        app.run("ui.scale", serde_json::json!({ "factor": 3.0 })).expect("the command");
        app.logic(&ctx);
        settle(&ctx);
        app.logic(&ctx);
        settle(&ctx);
        assert_eq!(app.ui.interface_scale, 2.0, "the scale is held at 200 percent");
        assert!((ctx.zoom_factor() - 2.0).abs() < 0.001, "the context is held too");
        // A zoom from anywhere else is put back, so the menu and the drawing never disagree.
        ctx.set_zoom_factor(1.0);
        settle(&ctx);
        app.logic(&ctx);
        settle(&ctx);
        assert_eq!(app.ui.interface_scale, 2.0, "the menu does not follow a zoom it did not make");
        assert!((ctx.zoom_factor() - 2.0).abs() < 0.001, "and the context is put back to the menu's value");
    }

    /// A persisted out-of-range scale reports its adjustment; a clean setting stays quiet.
    #[test]
    fn a_saved_scale_adjustment_is_reported() {
        crate::i18n::set_current(crate::i18n::Lang::EN);
        let mut held = app_with(&[("ui_scale", "9")]);
        held.session.notices.clear();
        held.take_saved_look();
        assert_eq!(held.ui.interface_scale, 2.0);
        assert_eq!(held.session.notices.iter().filter(|n| n.text == "Interface scale held to 200%").count(), 1);
        let mut clean = app_with(&[("ui_scale", "1.25")]);
        clean.session.notices.clear();
        clean.take_saved_look();
        assert_eq!(clean.ui.interface_scale, 1.25);
        assert!(clean.session.notices.is_empty());
        let mut malformed = app_with(&[("ui_scale", "1,25")]);
        malformed.session.notices.clear();
        malformed.take_saved_look();
        assert_eq!(malformed.ui.interface_scale, 1.0);
        assert_eq!(malformed.session.notices.iter().filter(|n| n.text == "Interface scale held to 100%").count(), 1);
    }

    #[test]
    fn a_choice_outlives_a_file_that_stops_loading() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let path = dir.path().join("cobalt.json");
        let mut cobalt = builtin(true);
        cobalt.name = "Cobalt".to_string();
        let good = cobalt.to_json().expect("json");
        std::fs::write(&path, &good).expect("the file");
        let mut app = app_with(&[]);
        app.load_themes_from(Some(dir.path()));
        app.use_theme("Cobalt").expect("the file loads");
        assert_eq!(app.theme_shown(true).1, "Cobalt");
        // A save that does not parse: the folder drops the theme, the choice stays.
        std::fs::write(&path, "{\"schema\": 1, ").expect("the broken file");
        app.load_themes_from(Some(dir.path()));
        assert_eq!(app.ui.theme_name, "Cobalt", "the choice outlives the file");
        assert_eq!(app.current_theme().name, "Dark", "the nearest theme is drawn");
        assert_eq!(app.theme_shown(true).1, "Dark", "and the button names it");
        // The typo is fixed: the choice is drawn again without picking it again.
        std::fs::write(&path, &good).expect("the file");
        app.load_themes_from(Some(dir.path()));
        assert_eq!(app.current_theme().name, "Cobalt", "the choice is restored");
        assert_eq!(app.theme_shown(true).1, "Cobalt");
    }

    #[test]
    fn a_live_edit_that_changes_the_brightness_is_followed() {
        let dir = tempfile::tempdir().expect("a temporary folder");
        let path = dir.path().join("cobalt.json");
        let mut cobalt = builtin(true);
        cobalt.name = "Cobalt".to_string();
        std::fs::write(&path, cobalt.to_json().expect("json")).expect("the file");
        let mut app = app_with(&[]);
        let log = saved_by(&mut app);
        app.load_themes_from(Some(dir.path()));
        app.use_theme("Cobalt").expect("the file loads");
        let stamp = app.themes_stamp;
        let revision = app.theme_revision();
        app.load_themes_from(Some(dir.path()));
        assert_eq!(app.themes_stamp, stamp, "metadata remains unchanged");
        assert_ne!(app.theme_revision(), revision, "an explicit reload invalidates a caller's cached appearance");
        // The same file saved as a light theme: the interface takes its brightness with it.
        cobalt.dark = false;
        cobalt.tokens = theme::Tokens::light();
        std::fs::write(&path, cobalt.to_json().expect("json")).expect("the file");
        app.load_themes_from(Some(dir.path()));
        assert!(!app.ui.dark, "the interface takes the brightness of the theme it names");
        assert!(
            log.lock().expect("the log").iter().any(|(key, value)| key == "dark" && value == "false"),
            "restart retains the adopted brightness"
        );
        assert_eq!(app.current_theme().name, "Cobalt", "the named theme is drawn");
        assert_eq!(app.theme_shown(true).1, "Cobalt");
    }

    #[test]
    fn the_fallback_takes_the_brightness_asked_for() {
        let mut app = app_with(&[]);
        // Only a light theme loads and the interface asks for dark: the built-in dark is drawn.
        app.themes = vec![builtin(false)];
        app.ui.dark = true;
        app.ui.theme_name = "Gone".to_string();
        assert!(app.current_theme().dark, "the drawn theme is a dark one, as asked");
    }

    #[test]
    fn the_theme_command_lands_on_the_brightness_it_is_given() {
        let mut app = app_with(&[]);
        app.take_saved_look();
        let log = saved_by(&mut app);
        // A user file reworks Light and reports itself dark: the request, not the name, decides.
        let mut reworked = builtin(false);
        reworked.dark = true;
        app.themes = vec![reworked, builtin(true)];
        app.run("ui.theme", serde_json::json!({ "dark": false })).expect("the command");
        assert!(!app.ui.dark, "the brightness asked for is the one in force");
        assert!(log.lock().expect("the log").iter().any(|(k, v)| k == "dark" && v == "false"));
    }

    #[test]
    fn following_the_system_survives_a_restart() {
        let mut app = app_with(&[("theme", ""), ("dark", "auto")]);
        app.take_saved_look();
        assert!(app.ui.dark_follows_system, "the choice to follow is remembered, not read as a toggle");
        app.ui.dark = true;
        let log = saved_by(&mut app);
        app.run("ui.themeFile", serde_json::json!({})).expect("follow the system");
        assert!(log.lock().expect("the log").iter().any(|(key, value)| key == "dark_fallback" && value == "true"));
        let mut restarted = app_with(&[("theme", ""), ("dark", "auto"), ("dark_fallback", "true")]);
        restarted.take_saved_look();
        assert!(restarted.ui.dark_follows_system);
        assert!(restarted.ui.dark, "no reported system theme retains the saved fallback brightness");
        let mut app = app_with(&[("theme", ""), ("dark", "false")]);
        app.take_saved_look();
        assert!(!app.ui.dark_follows_system, "a saved brightness is a brightness, not a follow");
        assert!(!app.ui.dark);
    }

    #[test]
    fn a_parameter_of_the_wrong_type_is_refused() {
        let mut app = app_with(&[]);
        let log = saved_by(&mut app);
        app.ui.dark = false;
        app.ui.theme_name = "Custom".to_string();
        app.ui.dark_follows_system = true;
        for dark in [serde_json::json!("false"), serde_json::json!(0), serde_json::Value::Null] {
            let why = app.run("ui.theme", serde_json::json!({ "dark": dark })).expect_err("dark must be a boolean");
            assert!(why.contains("ui.theme"), "{why}");
            assert!(!app.ui.dark);
            assert_eq!(app.ui.theme_name, "Custom");
            assert!(app.ui.dark_follows_system);
            assert!(log.lock().expect("the log").is_empty(), "rejection saves no preferences");
        }
        for params in [serde_json::json!({"nmae":"Mine"}), serde_json::json!("Mine"), serde_json::json!({"name":"Mine", "extra":true})] {
            assert!(app.run("ui.themeFile", params).is_err());
            assert_eq!(app.ui.theme_name, "Custom");
            assert!(!app.ui.dark);
            assert!(app.ui.dark_follows_system);
            assert!(log.lock().expect("the log").is_empty(), "malformed parameters save no preferences");
        }
        for params in [serde_json::json!({"drak":false}), serde_json::json!("Dark")] {
            assert!(app.run("ui.theme", params).is_err());
            assert_eq!(app.ui.theme_name, "Custom");
            assert!(!app.ui.dark);
            assert!(log.lock().expect("the log").is_empty());
        }
        app.run("ui.theme", serde_json::json!({})).expect("omitting dark still toggles");
        assert!(app.ui.dark);
        let why = app.run("ui.themeFile", serde_json::json!({ "name": 5 })).expect_err("a name must be a string");
        assert!(why.contains("ui.themeFile"), "{why}");
        let why = app.run("ui.scale", serde_json::json!({ "factor": "big" })).expect_err("a factor must be a number");
        assert!(why.contains("ui.scale"), "{why}");
        app.run("ui.scale", serde_json::json!({ "factor": 1.25 })).expect("a number is accepted");
        assert_eq!(app.ui.interface_scale, 1.25);
    }

    #[test]
    fn a_theme_saved_as_the_other_brightness_takes_the_interface_with_it() {
        let mut app = app_with(&[]);
        let mut file = builtin(true);
        file.name = "Mine".to_string();
        app.themes = vec![file];
        app.ui.theme_name = "Mine".to_string();
        app.ui.dark = true;
        // The file is saved as a light theme while the app shows the dark one.
        let mut file = builtin(true);
        file.name = "Mine".to_string();
        file.dark = false;
        app.themes = vec![file];
        app.sync_named_brightness();
        assert!(!app.ui.dark, "the interface takes the brightness of the theme it names");
    }

    #[test]
    fn a_saved_theme_that_is_gone_leaves_the_saved_brightness() {
        let mut app = app_with(&[("theme", "Deleted"), ("dark", "true")]);
        app.take_saved_look();
        assert_eq!(app.ui.theme_name, "Deleted", "the choice waits for a file of that name");
        assert!(app.ui.dark, "Dark Mode was on when it was last closed");
        assert!(!app.ui.dark_follows_system);
        assert_eq!(app.theme_shown(true).1, "Dark", "the button names the theme drawn");
        assert_eq!(app.ui.interface_scale, 1.0, "no scale saved is the normal one");
        // The file appears later: the saved choice is drawn without picking it again.
        let dir = tempfile::tempdir().expect("a temporary folder");
        let mut saved = builtin(false);
        saved.name = "Deleted".to_string();
        std::fs::write(dir.path().join("deleted.json"), saved.to_json().expect("json")).expect("the file");
        app.load_themes_from(Some(dir.path()));
        assert_eq!(app.current_theme().name, "Deleted", "the saved choice is restored");
    }

    #[test]
    fn the_theme_command_names_the_theme_it_draws() {
        let mut app = app_with(&[]);
        app.take_saved_look();
        // The built-in Dark replaced by a light-flagged file, and a dark user theme beside it.
        let mut dark_named_light = builtin(false);
        dark_named_light.name = "Dark".to_string();
        let mut cobalt = builtin(true);
        cobalt.name = "Cobalt".to_string();
        app.themes = vec![builtin(false), dark_named_light, cobalt];
        app.run("ui.theme", serde_json::json!({ "dark": true })).expect("the command");
        assert_eq!(app.ui.theme_name, "Cobalt", "the name follows the theme drawn");
        assert_eq!(app.current_theme().name, "Cobalt");
        assert_eq!(app.theme_shown(true).1, "Cobalt");
    }

    #[test]
    fn a_refused_theme_file_is_said_once() {
        crate::i18n::set_current(crate::i18n::Lang::EN);
        let dir = tempfile::tempdir().expect("a temporary folder");
        std::fs::write(dir.path().join("half.json"), "{\"schema\": 1}").expect("the file");
        let mut app = app_with(&[]);
        let said = |app: &RustGisApp| app.session.notices.iter().filter(|n| n.text.contains("refused")).count();
        let before = said(&app);
        app.load_themes_from(Some(dir.path()));
        assert_eq!(said(&app) - before, 1, "the refusal is said");
        app.load_themes_from(Some(dir.path()));
        assert_eq!(said(&app) - before, 1, "the same refusal is not repeated");
    }

    #[test]
    fn a_held_size_is_said_once() {
        crate::i18n::set_current(crate::i18n::Lang::EN);
        let dir = tempfile::tempdir().expect("a temporary folder");
        let text = r#"{"schema": 1, "name": "Mine", "extends": "Light", "metrics": {"body": 1e9}}"#;
        std::fs::write(dir.path().join("mine.json"), text).expect("the file");
        std::fs::write(dir.path().join("other.json"), text.replace("Mine", "Other")).expect("another file");
        let mut app = app_with(&[]);
        let said = |app: &RustGisApp| app.session.notices.iter().filter(|n| n.text.contains("theme adjustment notice")).count();
        let before = said(&app);
        app.load_themes_from(Some(dir.path()));
        assert_eq!(said(&app) - before, 1, "the adjustments are reported together");
        assert!(app.session.notices.iter().any(|n| n.text.starts_with("2 theme adjustment notice")));
        app.load_themes_from(Some(dir.path()));
        assert_eq!(said(&app) - before, 1, "and not again on the next watch tick");
        let notes = app.themes.iter().find(|t| t.name == "Mine").expect("the lossy theme loaded").notes.clone();
        for expected in [2, 3] {
            app.announce_theme_notes(&notes);
            assert_eq!(said(&app) - before, expected, "an explicit read reports even previously held adjustments");
        }
        app.announce_theme_notes(&[]);
        app.load_themes_from(Some(dir.path()));
        assert_eq!(said(&app) - before, 3, "a clean read and unchanged folder refresh remain quiet");
    }

    fn builtin(dark: bool) -> theme::Theme {
        theme::Theme::builtin().into_iter().find(|t| t.dark == dark).expect("a built-in theme ships")
    }

    fn saved_by(app: &mut RustGisApp) -> std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = log.clone();
        app.services.save_pref =
            Some(Box::new(move |key: &str, value: &str| sink.lock().expect("the log").push((key.to_string(), value.to_string()))));
        log
    }

    #[test]
    fn choosing_a_theme_saves_the_choice() {
        let mut app = app_with(&[]);
        let log = saved_by(&mut app);
        app.use_theme("Mine").expect("the theme is in the list");
        assert_eq!(app.ui.theme_name, "Mine");
        assert!(app.ui.dark, "the theme brings its own brightness");
        assert!(!app.ui.dark_follows_system);
        let written = log.lock().expect("the log").clone();
        assert!(written.contains(&("theme".to_string(), "Mine".to_string())), "{written:?}");
        assert!(written.contains(&("dark".to_string(), "true".to_string())), "{written:?}");
        assert!(app.use_theme("Gone").is_err(), "a name the list does not hold");
    }
}
