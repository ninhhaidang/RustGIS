//! L2: geoprocessing tools.
//!
//! Each tool is a [`ToolSpec`]: an id, a toolbox, parameters for the Tools pane, and a
//! pure `run` function over datasets. Inputs refer to layers by name through [`Inputs`]. Work is
//! done in Web Mercator metres, except buffers, which are geodesic: drawn in a projection centred
//! on each feature (see [`geodesic`]) so a 100 km buffer is 100 km on the ground.
//!
//! Tools from external suites (the GeoLibre/Whitebox tools in `rustgis-geolibre`, and plugins
//! in `rustgis-plugins`) join the built-in ones through [`register_external`]: they're listed by [`tools`], found by
//! [`tool`] and run by [`run`] like the rest.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod attribute_join;
pub mod control;
pub mod convert;
pub mod density;
pub mod dggs;
pub mod geodesic;
pub mod georef;
mod join;
pub mod kriging;
mod merge;
pub mod network;
pub mod neural;
mod parallel;
mod parameters;
pub mod rasterops;
pub mod selection;
pub mod sketch_edit;
mod stats;
pub mod tissot;
mod tools;
pub mod topology;
pub mod trend;
mod validity;
pub mod warp;

pub use attribute_join::{DuplicateStrategy, JoinFieldOptions, JoinFieldReport, join_field};
pub use density::{
    DENSITY_CELLS_FLOOR, KernelType, MAX_DENSITY_WORK, auto_cell_size, kernel_density, max_density_cells, points_extent,
    resolve_grid_extent, silverman_bandwidth,
};
pub use parallel::scratch_budget;
pub use parameters::StatisticList;
pub use tools::{TOOLS, find_field_case_insensitive, holds_coordinates, max_buffer_bytes, param_applies, simplify_geometry, zonal_bands};

use rustgis_data::Dataset;
use serde_json::Value;

/// Parameter kinds, which the Tools pane turns into widgets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
    /// Any layer.
    Layer,
    /// A polygon layer.
    PolygonLayer,
    /// A point layer.
    PointLayer,
    /// A field of the associated input (the longest `<input>_` name prefix, else the first layer).
    Field,
    /// Several field names entered as text, with suggestions from the associated input (tools
    /// whose runner takes the list as one string). The runner defines the list syntax;
    /// suggestions append comma-separated names.
    FieldNames,
    /// A numeric field of the associated input.
    NumericField,
    /// A field of the associated input that can hold coordinates: a numeric field, or a text field
    /// with numbers written in it (see [`holds_coordinates`]).
    CoordinateField,
    /// Ordered field names from the named input; accepts the existing string or array values.
    Fields {
        input: &'static str,
    },
    /// Ordered field/statistic objects from the named input, using the existing JSON keys.
    FieldStatistics {
        input: &'static str,
        statistics: StatisticList,
    },
    Number,
    Text,
    /// One of a fixed list of choices.
    Choice(&'static [&'static str]),
    /// A raster layer.
    RasterLayer,
    /// Any vector layer (no standalone tables).
    VectorLayer,
    /// Several raster layers, in order: their names (or file paths) separated by `;`.
    RasterLayers,
    /// Several vector layers, in order: their names (or file paths) separated by `;`.
    VectorLayers,
    /// On or off.
    Bool,
    /// A file path typed by the user (for data RustGIS doesn't open as a layer, such as LiDAR).
    Path,
    /// A file written by the tool, not an input to read. Native callers supply a destination;
    /// browser callers supply a download name (or use the tool's suggested name).
    SaveFile {
        ext: &'static str,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub name: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
    pub required: bool,
    /// Default value as JSON text (`"10"`, `"\"km\""`), or empty.
    pub default: &'static str,
    /// A longer description, shown on hover (may be empty).
    pub help: &'static str,
}

/// What a tool produced.
#[derive(Clone, Debug)]
pub enum Output {
    /// A new layer to add to the map.
    Layer(Dataset),
    /// Rows to select in an existing layer.
    Selection { layer: String, rows: Vec<usize> },
    /// An existing layer's dataset replaced (e.g. new fields).
    Update { layer: String, dataset: Dataset },
    /// A standalone table.
    Table(Dataset),
    /// Several new layers and tables (external tools can write more than one output).
    Layers(Vec<OutputLayer>),
    /// A downloadable file, never added to the map as a layer.
    File { file_name: String, bytes: Vec<u8> },
}

/// One output of [`Output::Layers`].
#[derive(Clone, Debug)]
pub enum OutputLayer {
    Vector(Dataset),
    /// A raster file, kept as its bytes so the project can embed it.
    Raster {
        name: String,
        file_name: String,
        bytes: Vec<u8>,
    },
    Table(Dataset),
}

/// A raster layer handed to a tool: its file (on disk or embedded), or failing that the pixels.
pub struct RasterInput<'a> {
    pub raster: &'a rustgis_data::raster::Raster,
    /// The file it was read from, when it is on disk.
    pub path: Option<&'a str>,
    /// The file's name and bytes, when it is embedded in the project.
    pub embedded: Option<(&'a str, &'a [u8])>,
}

#[derive(Clone, Debug)]
pub struct ToolResult {
    pub output: Output,
    pub messages: Vec<String>,
}

/// Access to the map's layers by name.
pub trait Inputs {
    fn dataset(&self, layer: &str) -> Option<&Dataset>;
    /// A standalone table by its id: an input given as a number, which no name is taken for.
    fn table(&self, _id: u32) -> Option<&Dataset> {
        None
    }
    /// Selected rows of a layer; tools honour a selection like desktop GIS does.
    fn selection(&self, layer: &str) -> &[usize];
    /// A raster layer by name.
    fn raster(&self, _layer: &str) -> Option<RasterInput<'_>> {
        None
    }
    /// The active map's flat projection, for tools that show what it does to the Earth (Tissot's
    /// indicatrix): its name. `None` when there's no map or it is a globe.
    fn projection_name(&self) -> Option<String> {
        None
    }
    /// A longitude/latitude point on the active map's projection plane, in metres. `None` where
    /// the projection doesn't reach, or with no flat map ([`Inputs::projection_name`]).
    fn project(&self, _c: rustgis_geom::Coord) -> Option<rustgis_geom::Coord> {
        None
    }
    /// The longitude/latitude extent the map's view shows, when there is a view.
    fn view_extent(&self) -> Option<rustgis_geom::Extent> {
        None
    }

    /// Builds a vector-tile archive without coupling the analysis crate to the tile crate.
    fn vector_to_pmtiles(
        &self,
        _dataset: &Dataset,
        _rows: &[usize],
        _min_zoom: u8,
        _max_zoom: u8,
        _source_layer: &str,
    ) -> Result<(Vec<u8>, Vec<String>), String> {
        Err("vector-to-PMTiles export is unavailable in this input provider".into())
    }

    /// Builds a raster tile archive with the layer's existing display style.
    fn raster_to_pmtiles(&self, _layer: &str, _min_zoom: u8, _max_zoom: u8) -> Result<Vec<u8>, String> {
        Err("raster-to-PMTiles export is unavailable in this input provider".into())
    }
}

pub struct ToolSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub toolbox: &'static str,
    pub summary: &'static str,
    pub params: &'static [ParamSpec],
    pub run: fn(&dyn Inputs, &Value) -> Result<ToolResult, String>,
}

/// Runs an external tool (the spec says which).
pub type ExternalRunner = fn(&ToolSpec, &dyn Inputs, &Value) -> Result<ToolResult, String>;

/// Where external tools come from: called the first time the tools are listed, and again after
/// [`reload_external`].
pub type ExternalProvider = fn() -> Vec<ToolSpec>;

/// One external tool suite: its tools are loaded lazily and leaked (each load lives as long as
/// the app, and reloads are rare: the user installs a suite or a plugin), so specs can be handed
/// out as `&'static`.
struct Source {
    key: &'static str,
    provider: ExternalProvider,
    runner: ExternalRunner,
    tools: Option<&'static [ToolSpec]>,
}

static SOURCES: std::sync::RwLock<Vec<Source>> = std::sync::RwLock::new(Vec::new());

/// Adds an external tool suite under `key` (the GeoLibre tools, plugins). Only the first
/// registration of a key counts; the provider runs lazily, so registering costs nothing until a
/// tool list is needed.
pub fn register_external(key: &'static str, provider: ExternalProvider, runner: ExternalRunner) {
    if let Ok(mut v) = SOURCES.write()
        && !v.iter().any(|s| s.key == key)
    {
        v.push(Source { key, provider, runner, tools: None });
    }
}

/// The tools of the suite registered as `key` (empty when there is none).
pub fn external_tools_of(key: &str) -> &'static [ToolSpec] {
    let provider = match SOURCES.read() {
        Ok(v) => match v.iter().find(|s| s.key == key) {
            Some(Source { tools: Some(t), .. }) => return t,
            Some(s) => s.provider,
            None => return &[],
        },
        Err(_) => return &[],
    };
    // One load at a time: a second caller waits for the first instead of loading again. The
    // provider runs without the sources' lock held, so a slow one never blocks the others.
    static LOADING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _loading = LOADING.lock();
    if let Ok(v) = SOURCES.read()
        && let Some(Source { tools: Some(t), .. }) = v.iter().find(|s| s.key == key)
    {
        return t;
    }
    let tools: &'static [ToolSpec] = Box::leak(
        std::panic::catch_unwind(provider)
            .unwrap_or_else(|_| {
                log::error!("the external tools '{key}' failed to load");
                Vec::new()
            })
            .into_boxed_slice(),
    );
    match SOURCES.write() {
        Ok(mut v) => match v.iter_mut().find(|s| s.key == key) {
            Some(s) => s.tools.get_or_insert(tools),
            None => tools,
        },
        Err(_) => tools,
    }
}

/// The external tools of every suite, in registration order (empty when none are registered).
pub fn external_tools() -> Vec<&'static ToolSpec> {
    let keys: Vec<&'static str> = SOURCES.read().map(|v| v.iter().map(|s| s.key).collect()).unwrap_or_default();
    keys.into_iter().flat_map(external_tools_of).collect()
}

/// Loads every suite's tools again the next time they're needed.
pub fn reload_external() {
    if let Ok(mut v) = SOURCES.write() {
        for s in v.iter_mut() {
            s.tools = None;
        }
    }
}

/// Loads the tools of the suite `key` again the next time they're needed (it was installed,
/// located or removed, or a plugin was).
pub fn reload_external_of(key: &str) {
    if let Ok(mut v) = SOURCES.write()
        && let Some(s) = v.iter_mut().find(|s| s.key == key)
    {
        s.tools = None;
    }
}

/// Every tool: the built-in ones, then the external ones.
pub fn tools() -> impl Iterator<Item = &'static ToolSpec> {
    TOOLS.iter().chain(external_tools())
}

/// The suite an external tool comes from, with its runner (`None` for a built-in tool).
fn source_of(spec: &ToolSpec) -> Option<(&'static str, ExternalRunner)> {
    // By id, not by address: a spec handed out before a reload (an open tool form) is still
    // the same tool.
    if TOOLS.iter().any(|t| std::ptr::eq(t, spec)) {
        return None;
    }
    let keys: Vec<(&'static str, ExternalRunner)> =
        SOURCES.read().map(|v| v.iter().map(|s| (s.key, s.runner)).collect()).unwrap_or_default();
    keys.into_iter().find(|(key, _)| external_tools_of(key).iter().any(|t| t.id == spec.id))
}

/// Whether a tool comes from an external suite.
pub fn is_external(spec: &ToolSpec) -> bool {
    source_of(spec).is_some()
}

/// The key of the suite a tool comes from (`None` for a built-in tool).
pub fn external_source(spec: &ToolSpec) -> Option<&'static str> {
    source_of(spec).map(|(k, _)| k)
}

pub fn tool(id: &str) -> Option<&'static ToolSpec> {
    let by_id = |t: &&ToolSpec| t.id.eq_ignore_ascii_case(id);
    let by_name = |t: &&ToolSpec| t.name.eq_ignore_ascii_case(id);
    // A built-in tool wins over an external one with the same name.
    tools().find(by_id).or_else(|| TOOLS.iter().find(by_name)).or_else(|| external_tools().into_iter().find(by_name))
}

/// The runner for external tools; never called (external specs carry the runner's id).
pub fn external_placeholder(_: &dyn Inputs, _: &Value) -> Result<ToolResult, String> {
    Err("this tool runs through the external suite".into())
}

/// A tool's parameters with defaults filled in, or an error naming a missing required one.
/// [`run`] does this first; a host that runs an external tool another way (the browser's
/// asynchronous runner) does it too, so both check parameters the same way.
pub fn resolve_params(spec: &ToolSpec, params: &Value) -> Result<Value, String> {
    let mut p = params.as_object().cloned().unwrap_or_default();
    // Not an external suite's tool (a copy of a built-in spec counts as built-in).
    let built_in = source_of(spec).is_none();
    if built_in {
        tools::normalize_params(spec, &mut p);
    }
    for ps in spec.params {
        if !p.contains_key(ps.name)
            && !ps.default.is_empty()
            && let Ok(v) = serde_json::from_str::<Value>(ps.default)
        {
            p.insert(ps.name.to_string(), v);
        }
        if ps.required && p.get(ps.name).is_none_or(|v| v.is_null() || v.as_str() == Some("")) {
            return Err(format!("{}: '{}' is required", spec.name, ps.label));
        }
        // The built-in tools' choices are the whole list. An external tool's (GeoLibre's, a
        // plugin's) can be looser, such as GeoLibre's comma-separated lists of options: its own
        // runner checks them.
        if let ParamKind::Choice(choices) = ps.kind
            && built_in
            && let Some(v) = p.get_mut(ps.name)
        {
            *v = choice(spec, ps, choices, v)?;
        }
        if ps.kind == ParamKind::Bool
            && built_in
            && let Some(v) = p.get_mut(ps.name)
        {
            *v = boolean(spec, ps, v)?;
        }
    }
    Ok(Value::Object(p))
}

/// A built-in tool's on-off parameter as `true` or `false` (text saying so counts). Anything else is
/// an error, never a guess at what `"yes"` or `1` meant (#406). A blank value, or null, is the
/// default.
fn boolean(spec: &ToolSpec, ps: &ParamSpec, v: &Value) -> Result<Value, String> {
    let default = || serde_json::from_str(ps.default).unwrap_or(Value::Null);
    match v {
        Value::Bool(_) => Ok(v.clone()),
        Value::Null => Ok(default()),
        Value::String(s) if s.trim().is_empty() => Ok(default()),
        Value::String(s) if s.trim().eq_ignore_ascii_case("true") => Ok(Value::Bool(true)),
        Value::String(s) if s.trim().eq_ignore_ascii_case("false") => Ok(Value::Bool(false)),
        _ => Err(format!("{}: '{}' must be true or false (not {})", spec.name, ps.label, shown(v))),
    }
}

/// A refused value as JSON for its message: up to 80 characters, then "…". A value can be huge,
/// and is written out only that far (serde_json stops when its writer does).
fn shown(v: &Value) -> String {
    struct Upto(String, usize);
    impl std::fmt::Write for Upto {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            for c in s.chars() {
                if self.1 == 0 {
                    return Err(std::fmt::Error);
                }
                self.0.push(c);
                self.1 -= 1;
            }
            Ok(())
        }
    }
    let mut out = Upto(String::new(), 80);
    if std::fmt::write(&mut out, format_args!("{v}")).is_err() {
        out.0.push('…');
    }
    out.0
}

/// A built-in tool's choice parameter as one of its choices, spelled as declared (`"meters"` is
/// `"Meters"`). Any other value is an error naming the choices: a tool never guesses what an
/// unknown one meant (#406). A blank value is the declared default.
fn choice(spec: &ToolSpec, ps: &ParamSpec, choices: &[&str], v: &Value) -> Result<Value, String> {
    let given: std::borrow::Cow<'_, str> = match v {
        Value::Null => return Ok(Value::Null),
        // Blank: the declared default, as when it's left out.
        Value::String(s) if s.trim().is_empty() => return Ok(serde_json::from_str(ps.default).unwrap_or_else(|_| v.clone())),
        Value::String(s) => s.trim().into(),
        Value::Number(_) | Value::Bool(_) => v.to_string().into(),
        _ => "".into(),
    };
    match choices.iter().find(|c| c.eq_ignore_ascii_case(&given)) {
        Some(c) => Ok(Value::String((*c).to_string())),
        None => Err(format!("{}: '{}' must be one of {} (not {})", spec.name, ps.label, choices.join(", "), shown(v))),
    }
}

/// A multi-layer parameter: semicolon-separated names or an array of names.
pub fn layer_list(value: &Value) -> Result<Vec<&str>, String> {
    let names: Vec<&str> = match value {
        Value::String(text) => text.split(';').map(str::trim).filter(|n| !n.is_empty()).collect(),
        Value::Array(values) => values
            .iter()
            .map(|v| v.as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or("input layers must be nonempty names"))
            .collect::<Result<_, _>>()?,
        _ => return Err("input layers must be names separated by ; or an array of names".into()),
    };
    if names.is_empty() {
        return Err("at least one input layer is required".into());
    }
    Ok(names)
}

/// Runs a tool with JSON parameters, filling defaults and checking required ones.
pub fn run(id: &str, inputs: &dyn Inputs, params: &Value) -> Result<ToolResult, String> {
    let spec = tool(id).ok_or_else(|| format!("no tool '{id}'"))?;
    let p = resolve_params(spec, params)?;
    // A bug in a tool must not take the app down.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match source_of(spec) {
        Some((_, runner)) => runner(spec, inputs, &p),
        None => (spec.run)(inputs, &p),
    }))
    .unwrap_or_else(|_| Err(format!("{} failed unexpectedly", spec.name)))
}

/// [`run`] under `control`: setting its flag stops the tool at its next check, and its count
/// follows the tool's progress (see [`control`]). A run cancelled before it returns is an error
/// saying how far it came, whatever the tool gave back: a tool may stop early however suits it,
/// and what it made by then is never taken for its result. One cancelled before it starts doesn't
/// start.
pub fn run_with(id: &str, inputs: &dyn Inputs, params: &Value, control: &std::sync::Arc<control::Control>) -> Result<ToolResult, String> {
    run_under(tool(id).map_or(id, |t| t.name), control, || run(id, inputs, params))
}

/// `work` on this thread under `control`, as [`run_with`] runs a tool, for work that isn't one (an
/// engine command's raster): setting `control`'s flag from another thread stops it at its next
/// check, its count follows the work's progress, and a cancelled run's error says how far `name`
/// came.
pub fn run_under<T>(name: &str, control: &std::sync::Arc<control::Control>, work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    if control.is_cancelled() {
        return Err(format!("{name} was cancelled before it started"));
    }
    let _current = control::install(Some(control.clone()));
    // A file the work writes or reads counts and stops with it too.
    let _data = rustgis_data::progress::install(Some(control.clone()));
    // No count until the work starts one: a control used before doesn't show that run's.
    control::begin(0);
    let res = work();
    if !control.is_cancelled() {
        // Finished: its last step is all done, however it counted. A failed one stays where it
        // stopped.
        if res.is_ok() {
            control::finish();
        }
        return res;
    }
    Err(match control.progress() {
        Some((done, total)) => {
            format!("{name} was cancelled {}% of the way through its current step", done.saturating_mul(100) / total.max(1))
        }
        None => format!("{name} was cancelled"),
    })
}
