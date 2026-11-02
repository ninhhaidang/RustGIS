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
mod cswui;
mod cubesui;
mod dialogs;
mod docpanes;
mod earthaccessui;
mod editpanes;
mod embeddingsui;
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
mod pluginsui;
mod portalsui;
#[cfg(not(target_arch = "wasm32"))]
mod relayshot;
mod repairsource;
mod reportpane;
mod ribbon;
mod s3ui;
mod saveprompt;
mod scriptcolor;
mod scriptcomplete;
mod searchbox;
mod serviceui;
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

use rustgis_engine::render::rustgis_tiles::failure::Failure;
use rustgis_engine::{Project, Session};
use serde_json::{Value, json};

#[cfg(not(target_arch = "wasm32"))]
pub use autosave::FolderStore;
pub use autosave::{Recovery, RecoveryStore};
pub use chrome::{DoubleClick, TitlebarMetrics};
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

/// A result an asynchronous service fills in when it's done (the app polls it each frame).
pub type Slot<T> = std::sync::Arc<std::sync::Mutex<Option<T>>>;

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
    pub error: Option<String>,
    /// A capabilities request that failed, with what the server said (#509).
    pub failure: Option<Failure>,
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
    /// The tileset list, whose query the template keeps; the document being read; how many have
    /// been.
    pub list: String,
    pub doc: String,
    pub read: u8,
    pub vector: bool,
    /// The usual-path template, when the advertised one can't be read.
    pub fallback: String,
    pub slot: Slot<Result<Vec<u8>, String>>,
}

/// A service layer waiting on a document before it is added. `dialog` tells whether the add came
/// from the add-service dialog (its result shows there) or from the Connections tree (a notice);
/// `origin` is the connection a tree add is for, named in Recent once the layer is added.
pub enum ServiceStep {
    /// An OGC API layer whose advertised tile template is being read.
    Tiles { resolve: TilesResolve, dialog: bool, origin: Option<String> },
    /// A WCS coverage whose description is being read before it is asked for: `service.coverage`'s
    /// parameters and the description on its way.
    Coverage { params: Value, slot: Slot<Result<Vec<u8>, String>>, dialog: bool, origin: Option<String> },
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
    /// The Web Data service the Web Data pane shows (a `ui.dialog` name, see
    /// `panes::web_data_name`).
    pub web_data: &'static str,
    pub dark: bool,
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
    pub cog_probes: Vec<(Value, Slot<Result<bool, String>>)>,
    /// Service layers whose features are being read page by page, after their first page landed.
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
    /// The search box at the foot of the left pane.
    pub search: searchbox::State,
    /// The Data Sources pane's boxes, and the sources' generation last kept in the preferences.
    pub sources: sourcespane::State,
    pub sources_saved: u64,
    pub connections_saved: u64,
    /// The default raster style last kept in the preferences.
    pub raster_defaults_saved: rustgis_engine::map::RasterDefaults,
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
            web_data: "opendata",
            dark: false,
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
            earthaccess: Default::default(),
            arcgis: Default::default(),
            nldi: Default::default(),
            geocode: Default::default(),
            nav: Default::default(),
            bookmarks: Default::default(),
            plugin_panel: Default::default(),
            search: Default::default(),
            sources: Default::default(),
            sources_saved: 0,
            connections_saved: 0,
            raster_defaults_saved: Default::default(),
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
    applied_dark: Option<bool>,
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
        if s.active_layer.is_none() {
            s.active_layer = s.map().layers.first().map(|l| l.id);
        }
        let ui = UiState { keymap: keymap::Keymap::new(&s.registry), ..UiState::default() };
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
            mb: modelbuilder::ModelBuilder::default(),
            ws: workspacepanes::WorkspaceState::default(),
            time: timebar::TimeBarState::default(),
            plugins: pluginsui::PluginsState::default(),
            update: update::UpdateState::default(),
            reports: reportpane::ReportState::default(),
            docs: docpanes::DocState::default(),
            assistant: assistantpane::AssistantState::default(),
            fonts_frames: 0,
            applied_dark: None,
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
        // The GPU map backend: RUSTGIS_GPU forces it on, else the saved preference.
        self.map.gpu_enabled = std::env::var_os("RUSTGIS_GPU").is_some()
            || self.services.load_pref.as_ref().and_then(|load| load("ui.map.gpu")).as_deref() == Some("1");
        if let Some(m) = self.services.load_pref.as_ref().and_then(|load| load("ribbon_minimized")) {
            self.ui.ribbon_minimized = m == "true";
        }
        let list = |s: Option<String>| -> Vec<String> {
            s.map(|v| v.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()).unwrap_or_default()
        };
        self.ui.hidden_tabs = ribbon::current_hidden_tabs(list(self.services.load_pref.as_ref().and_then(|load| load("hidden_tabs"))));
        self.ui.quick_access = list(self.services.load_pref.as_ref().and_then(|load| load("quick_access")));
        // The browser's GeoLibre tools, loaded in an earlier visit: load them again (the runner
        // comes from the browser's cache).
        if self.services.load_pref.as_ref().and_then(|load| load("geolibre_web")).as_deref() == Some("on") {
            panes::start_geolibre_load(&mut self);
        }
        // Plugins the web app installed in earlier visits (the desktop loads its plugins folder).
        pluginsui::restore(&mut self);
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
        let (text, stripped) = match self.session.project.saved_text(None) {
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
        if stripped > 0 {
            self.session.notify(
                crate::i18n::fmt(
                    "{n} layer address(es) were saved without their keys, tokens or passwords: after RustGIS closes, add those layers again to give them",
                    &[("n", &stripped.to_string())],
                ),
                false,
            );
        }
        self.session.mark_saved();
        self.ui.web_file = Some(file);
        Ok(())
    }

    /// Offers bytes as a download (web).
    pub(crate) fn download_bytes(&mut self, name: &str, bytes: Vec<u8>) {
        self.download(name, bytes);
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

    /// Delivers exports from all command entry points, including scripts and the assistant.
    /// The engine records success only after the service submits the file to the host.
    fn deliver_gp_files(&mut self, mut requested: Result<Value, String>) -> Result<Value, String> {
        for (id, name, bytes) in self.session.take_gp_files() {
            let delivered = match &self.services.download {
                Some(download) => download(&name, bytes),
                None => Err(tl!("this build has no download service").into()),
            };
            let mut params = json!({"id": id});
            if let Err(error) = &delivered {
                params["error"] = json!(error);
            }
            let completed = self.session.run("gp.fileDelivered", &params);
            let result = delivered.and(completed);
            self.gp.last = Some(match &result {
                Ok(_) => (true, self.session.project.gp_history.last().map(|h| h.messages.clone()).unwrap_or_default()),
                Err(error) => (false, vec![error.clone()]),
            });
            // gp.fileDelivered reports delivery failures from the history record and its
            // own acknowledgment errors through Session::run; neither needs a second notice.
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
        self.run(id, params)
    }

    /// Runs a command by id: `ui.*` here, everything else in the engine.
    pub fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
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
        // A plugin's command that adds data by address doesn't freeze the app: the download is
        // left for the frame loop (`ui.addUrl`), as in the browser.
        let mut r = if id.starts_with("ui.") {
            self.run_ui(id, &params)
        } else if id.starts_with(rustgis_engine::plugins::PREFIX) {
            self.session.run_leaving_downloads(id, &params)
        } else {
            self.session.run(id, &params)
        };
        r = self.deliver_gp_files(r);
        if r.is_ok() && id == "tool.set" {
            self.map.cancel_sketch();
        }
        if let Err(e) = &r {
            log::info!("{id}: {e}");
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
            // its layer never lands in the new one.
            self.ui.cog_probes.clear();
        }
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
            self.map.vertex = None;
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
            self.record_step(id, &params, active);
        }
        self.note_last_project();
        // Sources added, saved or made favourites are kept with the preferences.
        sourcespane::save(self);
        sourcespane::save_connections(self);
        self.save_raster_defaults();
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

    /// Record Task: adds a command that ran from the app and changed the project as a step. Not
    /// panes and dialogs (`ui.*`), tasks themselves, or what only moves the view. A colour, outline
    /// or transparency set again on the same layer (dragged in its picker) updates the step it just
    /// made.
    /// `active` is the active layer's name from [`Self::layer_acted_on`], read before it ran.
    fn record_step(&mut self, id: &str, params: &Value, active: Option<(String, bool)>) {
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
            "ui.theme" => {
                self.ui.dark = p.get("dark").and_then(Value::as_bool).unwrap_or(!self.ui.dark);
                self.ui.dark_follows_system = false;
            }
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
            "ui.addUrl" => {
                if p.get("geolibreZarr").and_then(Value::as_bool) == Some(true) {
                    if let Err(e) = geolibreui::start(self, p) {
                        rustgis_engine::cmd::geolibre::discard_pending(&mut self.session, p);
                        return Err(e);
                    }
                    return Ok(Value::Null);
                }
                // The connection a Connections tree add is for (named in Recent once it is added).
                let origin = connectiontree::take_origin(self);
                let Some(url) = p.get("url").and_then(Value::as_str).map(str::trim).filter(|u| !u.is_empty()) else {
                    return self.run("ui.dialog", json!({"name": "addUrl"}));
                };
                if url.to_ascii_lowercase().split(['?', '#']).next().is_some_and(|u| u.ends_with(".pmtiles")) {
                    return connectiontree::run_named(self, "layer.addUrl", p.clone(), origin.as_deref());
                }
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
                if (remote || tiff || unnamed) && !scan && p.get("whole").and_then(Value::as_bool) != Some(true) {
                    let slot: Slot<Result<bool, String>> = Slot::default();
                    let land = slot.clone();
                    if remote {
                        rustgis_engine::data::source::open_url(
                            url,
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
                            url,
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
                    if let Some(o) = probe.as_object_mut() {
                        o.remove(connectiontree::ORIGIN);
                        if let Some(origin) = &origin {
                            o.insert(connectiontree::ORIGIN.into(), json!(origin));
                        }
                    }
                    self.ui.cog_probes.push((probe, slot));
                    return Ok(Value::Null);
                }
                let url = url.to_string();
                let slot: Slot<Result<Vec<u8>, String>> = Slot::default();
                let land = slot.clone();
                rustgis_engine::render::rustgis_tiles::cache::download_limited(
                    &url,
                    if p.get("geolibreProject").and_then(Value::as_bool) == Some(true) {
                        rustgis_engine::map::geolibre::MAX_TEXT
                    } else {
                        rustgis_engine::render::rustgis_tiles::cache::MAX_DOWNLOAD
                    },
                    Box::new(move |r| *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(r)),
                );
                let mut q = json!({});
                for k in ["name", "layer", "crs"] {
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
                if p.get("zoom").and_then(Value::as_bool) == Some(true) {
                    q["zoom"] = json!(true);
                }
                for k in ["service", "coverage"] {
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
                let shown = rustgis_engine::data::s3_address::inner(&url);
                self.session.notify(i18n::fmt("Downloading {url}…", &[("url", shown)]), false);
                let id = self.session.download_started();
                self.ui.downloads.push((id, url, q, slot));
            }
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
                // A catalogue: the dialog's search, run for the words given.
                if p.get("type").and_then(Value::as_str) == Some("csw") {
                    if self.dialogs.open != Some("addService") {
                        self.run("ui.dialog", json!({"name": "addService"}))?;
                    }
                    self.dialogs.mode = "csw";
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
                let caps = rustgis_engine::data::ogc::capabilities_url(url, (kind != "auto").then_some(kind));
                // A refusal is told with what the server said (#509).
                let slot = fetch_detailed_into(&caps);
                // The dialog shows the address connected to, however the connection was asked for.
                self.dialogs.text = url.to_string();
                self.ui.service = Some(ServiceConn {
                    url: url.to_string(),
                    kind,
                    slot,
                    text: None,
                    documents: Vec::new(),
                    pending: Some(caps),
                    listing: None,
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
                let png = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    rustgis_engine::cmd::share::map_png(session, None, None, Some(192.0))
                }))
                .unwrap_or_else(|_| Err("the capture failed unexpectedly".into()))?;
                let pm = rustgis_engine::render::decode_png(&png).ok_or("could not draw the map")?;
                let size = [pm.width() as usize, pm.height() as usize];
                self.ui.copy_image = Some(egui::ColorImage::from_rgba_premultiplied(size, pm.data()));
                self.session.notify(tl!("Copied the map to the clipboard"), false);
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
                let png = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    rustgis_engine::cmd::share::map_png(session, None, None, Some(192.0))
                }))
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
                let l = self
                    .session
                    .active_layer
                    .and_then(|id| self.session.map().layer(id))
                    .ok_or("select a layer in the Layers pane first")?;
                // Any format the dialog offers here (`dialogs::export_formats`): not GeoPackage or
                // FlatGeobuf, which the data crate refuses in the browser.
                let ext = p.get("format").and_then(Value::as_str).unwrap_or("geojson").to_ascii_lowercase();
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
                if left_out > 0 {
                    self.session.notify(format!("{name}: {left_out} features without drawable geometry were left out"), false);
                }
                if ext == "zip" && rustgis_engine::data::raster::wkt::to_wkt(crs).is_none() {
                    self.session.notify(format!("{name}: no .prj written, as {} has no WKT form", crs.label()), false);
                }
                if self.download(&name, bytes) {
                    for note in notes {
                        self.session.notify(note, false);
                    }
                }
            }
            "ui.exportRaster" if self.services.download.is_some() => {
                // The web app: encode once and download.
                match rustgis_engine::cmd::raster::geotiff_bytes(&self.session, &Value::Null) {
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
                let name = self
                    .session
                    .active_layer
                    .and_then(|id| self.session.map().layer(id))
                    .map(|l| format!("{}.tif", l.name))
                    .unwrap_or_default();
                if let Some(path) = dialogs::pick_save(&name, &["tif", "tiff"], tl!("GeoTIFF")) {
                    return self.run("share.exportRaster", json!({"path": path}));
                }
            }
            "ui.exportMap" => {
                let name = format!("{}.png", self.session.map().name);
                if let Some(path) = dialogs::pick_save(&name, &["png"], tl!("PNG image")) {
                    return self.run("share.exportMap", json!({"path": path, "dpi": 192}));
                }
            }
            "ui.exportLayer" => {
                let ext = p.get("format").and_then(Value::as_str).unwrap_or("geojson").to_ascii_lowercase();
                let label = dialogs::EXPORT_FORMATS
                    .iter()
                    .find(|(_, e)| *e == ext)
                    .map(|(l, _)| *l)
                    .ok_or_else(|| format!("unknown format '{ext}'"))?;
                let name = self
                    .session
                    .active_layer
                    .and_then(|id| self.session.map().layer(id))
                    .map(|l| format!("{}.{ext}", l.name))
                    .unwrap_or_default();
                if let Some(path) = dialogs::pick_save(&name, &[ext.as_str()], label) {
                    // The format chosen decides: its extension is added unless it was typed (a dot
                    // in the name, as in "U.S. Cities", isn't taken for one).
                    let typed = std::path::Path::new(&path).extension().is_some_and(|e| e.eq_ignore_ascii_case(&ext));
                    let path = if typed { path } else { format!("{path}.{ext}") };
                    return self.run("share.exportLayer", json!({"path": path}));
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
    fn poll_data_downloads(&mut self) {
        // GeoTIFFs, FlatGeobufs and GeoParquets whose headers are in: read in place, or
        // downloaded whole.
        let mut probed = Vec::new();
        self.ui.cog_probes.retain(|(p, slot)| match slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
            Some(r) => {
                probed.push((p.clone(), r));
                false
            }
            None => true,
        });
        for (mut p, r) in probed {
            let origin = connectiontree::take_origin_key(&mut p);
            let url = p.get("url").and_then(Value::as_str).unwrap_or("").to_string();
            match r {
                Ok(true) => {
                    // A GeoTIFF by its first bytes, whatever its address is named.
                    if !rustgis_engine::data::source::is_remote_name(&url) {
                        p["cog"] = json!(true);
                    }
                    let before = self.session.sources.generation;
                    if self.run("layer.addUrl", p).is_ok() {
                        connectiontree::name_recent(self, before, origin.as_deref());
                        self.map.invalidate();
                    }
                }
                Ok(false) => {
                    p["whole"] = json!(true);
                    let _ = connectiontree::run_add_url(self, p, origin.as_deref());
                }
                Err(e) => self.session.notify(if e.starts_with(&url) { e } else { format!("{url}: {e}") }, true),
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
                // A service's features: the pages after the first are read, off the frame, first.
                Ok(bytes) if p.get("service").and_then(Value::as_bool) == Some(true) => {
                    serviceui::read_pages(self, serviceui::Landed { id, url, params: p, origin }, bytes);
                }
                Ok(bytes) => self.add_landed(serviceui::Landed { id, url, params: p, origin }, bytes),
                Err(e) => {
                    self.session.download_failed(id);
                    rustgis_engine::cmd::geolibre::discard_pending(&mut self.session, &p);
                    self.session.notify(if e.starts_with(&url) { e } else { format!("{url}: {e}") }, true);
                }
            }
        }
    }

    /// Adds a download that landed (`layer.addDownload`), named in Recent after the connection it
    /// came from; one whose project was closed meanwhile is said and dropped.
    pub(crate) fn add_landed(&mut self, landed: serviceui::Landed, bytes: Vec<u8>) {
        let serviceui::Landed { id, url, params: mut p, origin } = landed;
        if !self.session.download_landed(id, bytes) {
            self.session.notify(i18n::fmt("{url}: downloaded after its project was closed, so it wasn't added", &[("url", &url)]), false);
            return;
        }
        p["id"] = json!(id);
        p["url"] = json!(url);
        let zoom = p.get("zoom").and_then(Value::as_bool) == Some(true);
        if zoom && self.ui.link_zoomed {
            p["zoom"] = json!(false);
        }
        let before = self.session.sources.generation;
        let pending = p.clone();
        match self.run("layer.addDownload", p) {
            Ok(r) => {
                connectiontree::name_recent(self, before, origin.as_deref());
                if r.get("zoomed").and_then(Value::as_bool) == Some(true) {
                    self.ui.link_zoomed = true;
                }
                self.map.invalidate();
            }
            Err(e) => {
                rustgis_engine::cmd::geolibre::discard_pending(&mut self.session, &pending);
                self.session.notify(if e.starts_with(&url) { e } else { format!("{url}: {e}") }, true);
            }
        }
    }

    /// Tells how a service layer added outside the add-service dialog (or in it) went: in the
    /// dialog's own line, or as a notice for the Connections tree.
    fn report_step(&mut self, dialog: bool, r: Result<String, String>) {
        match (dialog, r) {
            (true, Ok(m)) => self.dialogs.info = Some(m),
            (true, Err(e)) => self.dialogs.error = Some(e),
            (false, Ok(m)) => self.session.notify(m, false),
            (false, Err(e)) => self.session.notify(e, true),
        }
    }

    /// Moves the service layers waiting on a document along: an OGC API layer's tile template
    /// (read from its tileset list, then the tileset when the list doesn't carry it; the layer is
    /// added with the template read, or the usual path) and a WCS coverage's description (the
    /// coverage is then asked for the view and downloaded as From URL… does).
    fn poll_service_steps(&mut self) {
        use rustgis_engine::data::ogc::{TilesStep, resolve_tiles};
        for step in std::mem::take(&mut self.ui.service_steps) {
            match step {
                ServiceStep::Tiles { mut resolve, dialog, origin } => {
                    let Some(r) = resolve.slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
                        self.ui.service_steps.push(ServiceStep::Tiles { resolve, dialog, origin });
                        continue;
                    };
                    let next = r.ok().and_then(|body| resolve_tiles(&resolve.list, &resolve.doc, &body, resolve.vector));
                    let template = match next {
                        Some(TilesStep::Next(next)) if resolve.read < 2 => {
                            resolve.read += 1;
                            resolve.doc = next.clone();
                            resolve.slot = fetch_into(&next);
                            self.ui.service_steps.push(ServiceStep::Tiles { resolve, dialog, origin });
                            continue;
                        }
                        Some(TilesStep::Template(t)) => t,
                        _ => resolve.fallback.clone(),
                    };
                    let mut p = resolve.params;
                    p["tiles"] = json!(template);
                    let title = p["name"].as_str().unwrap_or("").to_string();
                    let r =
                        connectiontree::run_named(self, "service.add", p, origin.as_deref()).map(|_| format!("{} {title}", tl!("Added")));
                    self.report_step(dialog, r);
                    self.map.invalidate();
                }
                ServiceStep::Coverage { mut params, slot, dialog, origin } => {
                    let Some(r) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() else {
                        self.ui.service_steps.push(ServiceStep::Coverage { params, slot, dialog, origin });
                        continue;
                    };
                    let title = params["name"].as_str().unwrap_or("").to_string();
                    let r = r.and_then(|bytes| {
                        params["describe"] = json!(String::from_utf8_lossy(&bytes));
                        // Not `self.run`: an error belongs where the step reports, not the notices.
                        let c = self.session.run("service.coverage", &params)?;
                        let get = c.get("url").and_then(Value::as_str).unwrap_or("").to_string();
                        connectiontree::run_add_url(self, json!({"url": get, "name": title, "coverage": true}), origin.as_deref())
                    });
                    self.report_step(dialog, r.map(|_| format!("{} {title}", tl!("Downloading…"))));
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
                // Not `self.run`: an error belongs in the dialog, not the notices.
                match self.session.run("service.layers", &p) {
                    Ok(v) => {
                        conn.pending = v.get("next").and_then(Value::as_str).map(str::to_string);
                        if let Some(next) = &conn.pending {
                            conn.slot = fetch_detailed_into(next);
                        }
                        conn.listing = Some(v);
                    }
                    Err(e) => conn.error = Some(e),
                }
            }
            Err(f) => conn.failure = Some(f),
        }
    }

    /// Per-frame work that doesn't draw (theme, fonts, files that arrived).
    pub fn logic(&mut self, ctx: &egui::Context) {
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
                relay.poll(&mut self.session);
            }
            relayshot::drive(self, ctx);
            // A script an agent wrote to the file bridge.
            if let Some(bridge) = self.file_bridge.clone() {
                if !bridge.has_wake() {
                    let ctx = ctx.clone();
                    bridge.set_wake(Box::new(move || ctx.request_repaint()));
                }
                match bridge.poll(&mut self.session) {
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
        self.poll_downloads();
        let _ = self.deliver_gp_files(Ok(Value::Null));
        // A value picked under Dynamic Text lasts while the text tool does, however the tool
        // changes (Esc, another tool, a text placed without Keep Tool, a command).
        if self.session.tool != rustgis_engine::Tool::Graphic("text") {
            self.ui.dynamic_text = None;
        }
        // Addresses commands asked for where they couldn't download themselves (the browser):
        // fetched off the frame and added when they land, as Home › Add › Add Data › From URL does.
        for p in self.session.take_url_requests() {
            if let Err(e) = self.run("ui.addUrl", p) {
                self.session.notify(e, true);
            }
        }
        // Plugin commands that waited for `host.fetch` answers (the browser) run again once
        // they are in; their errors are notices already.
        let _ = self.session.poll_plugin_retries();
        // A frame may not come until the user moves: look again while commands wait.
        if self.session.plugin_retries_waiting() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
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
                .chain([ScriptFont::Arabic, ScriptFont::Bengali])
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
        if self.ui.dark_follows_system
            && let Some(system) = ctx.system_theme()
        {
            self.ui.dark = system == egui::Theme::Dark;
        }
        if self.applied_dark != Some(self.ui.dark) {
            let t = if self.ui.dark { theme::Tokens::dark() } else { theme::Tokens::light() };
            theme::apply(ctx, &t);
            self.applied_dark = Some(self.ui.dark);
            self.map.invalidate();
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
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
    }
}

/// The file name at the end of a project's web address, when it names a project or a project
/// package; the project is then opened as that file would be.
fn project_file_name(url: &str) -> Option<&str> {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let last = path.rsplit('/').next().unwrap_or("");
    (rustgis_engine::project::opens_as_project(last) || rustgis_engine::map::geolibre::is_project_file(last)).then_some(last)
}

/// Tests that fetch through the tile cache take turns: it has one fetcher for the whole process,
/// and a test that replaces it (`set_fetcher`) would answer another's downloads.
#[cfg(test)]
pub(crate) mod fetching {
    use std::sync::{Mutex, MutexGuard};

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

    /// Data added from a download isn't a step: its download is used up, so a task couldn't
    /// add it again.
    #[test]
    fn downloaded_data_isnt_recorded() {
        let mut app = RustGisApp::with_sample();
        app.ws.recording = Some(Vec::new());
        app.record_step("layer.addDownload", &json!({"id": 1, "url": "https://example.com/rivers.geojson"}), None);
        assert_eq!(app.ws.recording.as_ref().map(Vec::len), Some(0));
        assert!(app.session.notices.iter().any(|n| n.text.contains("session only")));
        app.record_step("layer.color", &json!({"layer": "Lakes", "color": "#123456"}), None);
        assert_eq!(app.ws.recording.as_ref().map(Vec::len), Some(1));
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
        app.map.vertex = Some((id, row, VertexId::new(0, 0, 1)));
        app.run("edit.undo", Value::Null).unwrap();
        assert_eq!(app.map.vertex, None, "undo");
        app.map.vertex = Some((id, row, VertexId::new(0, 0, 1)));
        app.run("edit.redo", Value::Null).unwrap();
        assert_eq!(app.map.vertex, None, "redo");
    }
}

#[cfg(test)]
mod download_service_tests {
    use super::{RustGisApp, Services, complete_gp_file};
    use serde_json::json;

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
    }
}
