//! L3: the GeoLibre tool suite (Whitebox's tools plus GeoLibre's own: 1,000+ raster, vector,
//! LiDAR, hydrology and terrain tools) as geoprocessing tools.
//!
//! RustGIS doesn't link the suite; a runner runs it. Natively that is the `geolibre` program, an
//! optional download run as a subprocess (see `runner`). In the browser it is geolibre-rust's
//! WASI runner, which the web app loads on demand and runs asynchronously
//! ([`set_manifests`], then `external_start` → `Services::geolibre_run` → `external_complete`
//! → `gp.finish` in the engine). [`register`] adds the tools to [`rustgis_analysis`], which
//! lists them next to the built-in tools under "GeoLibre ›" toolboxes; without a runner there
//! are none. Their forms come from the runner's manifests: each parameter says whether it is a
//! raster or vector input, an output, a choice, a number or a switch.
//!
//! The tools read and write files. Natively, raster layers and files already on disk are passed
//! by path; every other input is staged in a working folder (a temporary folder natively, an
//! in-memory `/work` folder in the browser): raster layers as their own file (or the embedded
//! copy), vector layers as GeoJSON. Vector data goes in a metric coordinate system (the first
//! raster input's, or the UTM zone of the data), so distances and areas are in metres; outputs
//! come back as new layers in longitude/latitude.
//!
//! Like Add Data, a tool reads any file named in its parameters (a layer name that isn't a
//! layer is taken as a path, and LiDAR inputs are paths), with the user's permissions. The
//! outputs a tool declares are written to the run's own scratch folder; the runner is trusted
//! like any program the user installs, so a tool could write elsewhere if asked to.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(not(target_arch = "wasm32"))]
pub mod install;
pub mod langpack;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub mod runner;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use rustgis_analysis::{Inputs, Output, OutputLayer, ParamKind, ParamSpec, ToolResult, ToolSpec};
use rustgis_data::raster::{Crs, Transformer};
use rustgis_data::{Coord, Dataset, Extent, Geometry};
use serde_json::{Value, json};

/// Tool ids get this prefix, so they never clash with the built-in tools.
pub const PREFIX: &str = "geolibre:";

/// The key the suite registers its tools under ([`rustgis_analysis::register_external`]).
pub const SOURCE: &str = "geolibre";

/// Adds the suite to the geoprocessing tools. The runner is found and its manifests read the
/// first time the tools are listed.
pub fn register() {
    rustgis_analysis::register_external(SOURCE, specs, run);
}

/// Uses `exe` as the runner (`None`: look in the usual places), and reloads the tool list.
/// Returns the runner in use and its version.
#[cfg(not(target_arch = "wasm32"))]
pub fn set_runner(exe: Option<std::path::PathBuf>) -> Result<Option<(std::path::PathBuf, String)>, String> {
    let found = runner::choose(exe)?;
    rustgis_analysis::reload_external_of(SOURCE);
    let _ = rustgis_analysis::external_tools_of(SOURCE);
    Ok(found)
}

/// Manifests given directly (the browser's runner lists them asynchronously), used instead of
/// asking the native runner.
static MANIFESTS: Mutex<Option<Vec<Value>>> = Mutex::new(None);

/// Uses `manifests` (the JSON array `geolibre manifests` prints) for the tool list, and reloads
/// it. This is how the browser build gets its tools.
pub fn set_manifests(json: &str) -> Result<usize, String> {
    let v: Vec<Value> = serde_json::from_str(json).map_err(|e| format!("unexpected manifests: {e}"))?;
    let n = v.len();
    if let Ok(mut m) = MANIFESTS.lock() {
        *m = Some(v);
    }
    rustgis_analysis::reload_external_of(SOURCE);
    let _ = rustgis_analysis::external_tools_of(SOURCE);
    Ok(n)
}

/// What a parameter is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Input,
    Output,
    Option,
}

/// The kind of data an input or output holds (the manifest's `data_kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Data {
    Raster,
    Vector,
    Table,
    Lidar,
    Text,
    Json,
    /// Unspecified: a file of any kind.
    File,
    Bool,
    Number,
    Other,
}

impl Data {
    fn of(s: &str) -> Data {
        match s {
            "raster" => Data::Raster,
            "vector" => Data::Vector,
            "table" => Data::Table,
            "lidar" => Data::Lidar,
            "text" => Data::Text,
            "json" => Data::Json,
            "file" => Data::File,
            "bool" => Data::Bool,
            "number" => Data::Number,
            _ => Data::Other,
        }
    }
}

#[derive(Clone, Debug)]
struct MetaParam {
    name: String,
    role: Role,
    data: Data,
    /// The manifest's default for an optional parameter, when it gives one.
    default: Option<Value>,
}

/// What a run needs to know about a tool beyond its [`ToolSpec`].
#[derive(Clone, Debug)]
struct Meta {
    schema: &'static [ParamSpec],
    category: String,
    params: Vec<MetaParam>,
    /// The form has the [`OUTPUT`] field naming the layers the outputs come back as.
    names_outputs: bool,
}

/// The parameter that names a tool's new layers, under the key the built-in tools use (#2224).
pub const OUTPUT: &str = "output";
/// The [`OUTPUT`] field's label, as on the built-in tools.
const OUTPUT_LABEL: &str = "Output Name";
/// The [`OUTPUT`] field's help: the same text on every tool, so it can be translated.
const OUTPUT_HELP: &str = "Name of the new layer. Empty: the tool's name. With several outputs, each adds its own name to it. A name already in use gets a number.";
/// The longest layer name [`OUTPUT`] keeps (the engine's longest name).
const MAX_OUTPUT_NAME: usize = 200;

/// The Raster Calculator's tool id: its inputs are bound to the names its expression quotes
/// ([`calculator_order`]).
const CALCULATOR: &str = "raster_calculator";
/// How the Raster Calculator's inputs meet its expression, added to the `inputs` field's help.
const CALCULATOR_INPUTS_HELP: &str = "Each name quoted in the expression is the name of one of these layers (or a file's path, file name or name without extension), and is read from that input whatever the order here. With only files listed and no quoted name matching any of them, the k-th quoted name is the k-th file.";

/// `p` is the [`OUTPUT`] field RustGIS adds, not one of the manifest's parameters.
pub fn is_output_name(p: &ParamSpec) -> bool {
    p.name == OUTPUT && p.label == OUTPUT_LABEL && p.kind == ParamKind::Text
}

static META: Mutex<Option<HashMap<String, Meta>>> = Mutex::new(None);

/// A `'static` copy of `s`, shared: reloading the tools (Locate…) reuses the strings it
/// already made instead of leaking another copy of every name and description.
fn leak(s: impl Into<String>) -> &'static str {
    static STRINGS: Mutex<Option<std::collections::HashSet<&'static str>>> = Mutex::new(None);
    let s = s.into();
    let Ok(mut set) = STRINGS.lock() else { return Box::leak(s.into_boxed_str()) };
    let set = set.get_or_insert_with(Default::default);
    if let Some(&have) = set.get(s.as_str()) {
        return have;
    }
    let new: &'static str = Box::leak(s.into_boxed_str());
    set.insert(new);
    new
}

/// `z_factor` → `Z factor`.
fn label_of(name: &str) -> String {
    let words = name.replace('_', " ");
    let mut c = words.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The manifests: given directly, else the native runner's (none without one).
fn manifests() -> Vec<Value> {
    if let Some(m) = MANIFESTS.lock().ok().and_then(|m| m.clone()) {
        return m;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        runner::manifests().unwrap_or_else(|e| {
            log::info!("GeoLibre tools: {e}");
            Vec::new()
        })
    }
    #[cfg(target_arch = "wasm32")]
    Vec::new()
}

/// Builds every tool's spec from the manifests (none without them).
fn specs() -> Vec<ToolSpec> {
    let manifests = manifests();
    let mut meta = HashMap::new();
    let mut out: Vec<ToolSpec> = manifests
        .iter()
        .filter_map(|m| {
            let (spec, info) = spec_of(m)?;
            meta.insert(spec.id.trim_start_matches(PREFIX).to_string(), info);
            Some(spec)
        })
        .collect();
    if let Ok(mut m) = META.lock() {
        *m = Some(meta);
    }
    out.sort_by(|a, b| a.toolbox.cmp(b.toolbox).then(a.name.cmp(b.name)));
    out
}

/// Where a tool sits: `GeoLibre › Terrain › Derivatives`, from its manifest's `taxonomy`
/// (geolibre-rust's menu grouping). A runner too old to list one gives the coarse `category`:
/// `GeoLibre › Raster`.
fn toolbox_of(m: &Value, category: &str) -> String {
    let taxonomy = m.get("taxonomy");
    let part = |k: &str| taxonomy.and_then(|t| t.get(k)).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    match (part("category"), part("subcategory")) {
        (Some(top), Some(sub)) => format!("GeoLibre › {top} › {sub}"),
        (Some(top), None) => format!("GeoLibre › {top}"),
        _ => format!("GeoLibre › {category}"),
    }
}

/// The runner has no field metadata: infer suggestions from the last name component.
/// Keep these editable in the form, since names can also describe new output columns.
fn field_kind(name: &str) -> Option<ParamKind> {
    match name.rsplit('_').next()? {
        last if last.eq_ignore_ascii_case("fields") => Some(ParamKind::FieldNames),
        last if last.eq_ignore_ascii_case("field") => {
            let numeric = name.split('_').any(|part| part.eq_ignore_ascii_case("weight") || part.eq_ignore_ascii_case("numeric"));
            Some(if numeric { ParamKind::NumericField } else { ParamKind::Field })
        }
        _ => None,
    }
}

/// Builds the form and staging metadata together, including legacy Composite Bands inputs.
fn spec_of(m: &Value) -> Option<(ToolSpec, Meta)> {
    let id = m.get("id")?.as_str()?;
    let name = m.get("display_name").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or(id);
    let category = m.get("category").and_then(Value::as_str).unwrap_or("Other").to_string();
    let summary = m.get("summary").and_then(Value::as_str).unwrap_or("");
    let defaults = m.get("defaults");
    let mut params = Vec::new();
    let mut meta = Vec::new();
    for p in m.get("params").and_then(Value::as_array).into_iter().flatten() {
        let Some(pname) = p.get("name").and_then(Value::as_str) else { continue };
        let schema = p.get("schema");
        // GeoLibre 1.8.0/1.8.1 describe this raster list as a string option (#1684).
        // Correct both the form and staging; leave explicit metadata from newer runners alone.
        let composite_inputs = id == "composite_bands"
            && pname == "inputs"
            && p.get("io_role").is_none()
            && p.get("data_kind").and_then(Value::as_str) == Some("string")
            && schema.and_then(|s| s.get("kind")).and_then(Value::as_str) == Some("string");
        // Raster Calculator takes several rasters, though GeoLibre 1.8.1 calls the input single.
        let calculator_inputs = id == CALCULATOR && pname == "inputs";
        let data = if composite_inputs { Data::Raster } else { Data::of(p.get("data_kind").and_then(Value::as_str).unwrap_or("")) };
        let role = match p.get("io_role").and_then(Value::as_str) {
            None if composite_inputs => Role::Input,
            Some("input") => Role::Input,
            Some("output") => Role::Output,
            _ => Role::Option,
        };
        let many = composite_inputs
            || (calculator_inputs && role == Role::Input && data == Data::Raster)
            || schema.and_then(|s| s.get("cardinality")).and_then(Value::as_str).is_some_and(|c| c != "single");
        let optional = !p.get("required").and_then(Value::as_bool).unwrap_or(false);
        let default = defaults.and_then(|d| d.get(pname)).filter(|v| optional && !v.is_null()).cloned();
        meta.push(MetaParam { name: pname.to_string(), role, data, default });
        // Outputs are written to a scratch folder and come back as layers: no field of their own
        // (the one [`OUTPUT`] field below names the layers).
        if role == Role::Output {
            continue;
        }
        let kind = match role {
            // Several layers, picked in order in the form (#270); files typed in go as text.
            Role::Input if many => match data {
                Data::Raster => ParamKind::RasterLayers,
                Data::Vector => ParamKind::VectorLayers,
                _ => ParamKind::Text,
            },
            Role::Input => match data {
                Data::Raster => ParamKind::RasterLayer,
                Data::Vector => ParamKind::VectorLayer,
                Data::Table => ParamKind::Layer,
                _ => ParamKind::Path,
            },
            _ => match schema.and_then(|s| s.get("kind")).and_then(Value::as_str) {
                Some("enum") => {
                    let options: Vec<&'static str> = schema
                        .and_then(|s| s.get("options"))
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|o| o.get("value").and_then(Value::as_str).map(leak))
                        .collect();
                    if options.is_empty() { ParamKind::Text } else { ParamKind::Choice(Box::leak(options.into_boxed_slice())) }
                }
                _ if data == Data::Bool => ParamKind::Bool,
                _ if data == Data::Number => ParamKind::Number,
                _ => field_kind(pname).unwrap_or(ParamKind::Text),
            },
        };
        // Example file names in the defaults ("dem.tif") are not defaults for a layer.
        // `ParamSpec::default` is JSON *text* (a string default is stored quoted, `"\"mean\""`,
        // as for the built-in tools): `analysis::run` and the form parse it back, so the tool and
        // the form both get `mean`. Hence `to_string`, not the bare string.
        let default = match role {
            Role::Input => String::new(),
            _ => defaults.and_then(|d| d.get(pname)).filter(|v| !v.is_null()).map(Value::to_string).unwrap_or_default(),
        };
        let mut help = p.get("description").and_then(Value::as_str).unwrap_or("").to_string();
        if many {
            help.push_str(" (layer names or file paths, separated by ;)");
        }
        if calculator_inputs && many {
            help.push(' ');
            help.push_str(CALCULATOR_INPUTS_HELP);
        }
        params.push(ParamSpec {
            name: leak(pname),
            label: leak(label_of(pname)),
            kind,
            required: p.get("required").and_then(Value::as_bool).unwrap_or(false),
            default: leak(default),
            help: leak(help),
        });
    }
    // One optional field names the new layers. A tool with an option or input of its own called
    // `output` keeps it, and its outputs keep the tool's name.
    let names_outputs = meta.iter().any(|p| p.role == Role::Output) && !params.iter().any(|p| p.name == OUTPUT);
    if names_outputs {
        params.push(ParamSpec {
            name: OUTPUT,
            label: OUTPUT_LABEL,
            kind: ParamKind::Text,
            required: false,
            default: "",
            help: OUTPUT_HELP,
        });
    }
    let spec = ToolSpec {
        id: leak(format!("{PREFIX}{id}")),
        name: leak(name),
        toolbox: leak(toolbox_of(m, &category)),
        summary: leak(summary),
        params: Box::leak(params.into_boxed_slice()),
        run: rustgis_analysis::external_placeholder,
    };
    let schema = spec.params;
    Some((spec, Meta { schema, category, params: meta, names_outputs }))
}

/// `g` with every point converted by `f`, or `None` when a point can't be (rather than
/// geometry with null coordinates).
fn convert(g: &Geometry, f: impl Fn(Coord) -> Option<Coord>) -> Option<Geometry> {
    let ok = std::cell::Cell::new(true);
    let out = g.map_coords(&|c| {
        f(c).filter(|m| m.is_finite()).unwrap_or_else(|| {
            ok.set(false);
            c
        })
    });
    ok.get().then_some(out)
}

/// Converts every feature's geometry, clearing those that can't be converted; returns how many.
fn convert_features(ds: &mut Dataset, f: impl Fn(Coord) -> Option<Coord>) -> usize {
    let mut lost = 0;
    for ft in &mut ds.features {
        if let Some(g) = &ft.geometry {
            ft.geometry = convert(g, &f);
            lost += usize::from(ft.geometry.is_none());
        }
    }
    lost
}

/// A file-name-safe version of a layer name.
fn safe(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).take(60).collect();
    if s.is_empty() { "layer".into() } else { s }
}

/// The coordinate system vector inputs are written in: the first raster input's, else the UTM
/// zone around the vector data (metres), else longitude/latitude for data spanning the world.
fn working_crs(rasters: &[Crs], vectors: &[Extent]) -> Crs {
    if let Some(c) = rasters.first() {
        return *c;
    }
    let e = vectors.iter().fold(Extent::EMPTY, |a, b| a.union(b));
    if e.is_empty() || e.width() > 40.0 {
        return Crs::LonLat;
    }
    let c = e.center();
    let zone = (((c.x + 180.0) / 6.0).floor() as i64 + 1).clamp(1, 60) as u32;
    Crs::from_epsg(if c.y >= 0.0 { 32600 + zone } else { 32700 + zone }).unwrap_or(Crs::LonLat)
}

/// A vector layer (its selection, if any) as GeoJSON in `crs`, and how many features' geometry
/// couldn't be converted (they're written without geometry).
fn vector_bytes(ds: &Dataset, rows: &[usize], crs: Crs, tf: &Transformer) -> Result<(Vec<u8>, usize), String> {
    let mut out = ds.clone();
    if !rows.is_empty() {
        out.features = rows.iter().filter_map(|&r| ds.features.get(r).cloned()).collect();
    }
    let lost = if crs != Crs::LonLat { convert_features(&mut out, |c| tf.from_lonlat(c)) } else { 0 };
    let mut json = rustgis_data::io::to_geojson(&out);
    if crs != Crs::LonLat
        && let Some(o) = json.as_object_mut()
    {
        // The legacy GeoJSON 2008 `crs` member, which the suite's reader honours.
        o.insert("crs".into(), json!({"type": "name", "properties": {"name": format!("urn:ogc:def:crs:EPSG::{}", crs.epsg())}}));
    }
    Ok((serde_json::to_vec(&json).map_err(|e| e.to_string())?, lost))
}

/// A file a tool argument refers to.
#[derive(Clone, Debug)]
enum FileRef {
    /// A staged input or an output, by file name in the run's working folder.
    Work(String),
    /// A file the user named, used where it is (native only).
    Path(String),
}

/// A tool argument.
#[derive(Clone, Debug)]
enum Arg {
    Files(Vec<FileRef>),
    Value(Value),
}

/// A run, prepared: the input files to stage, the arguments, and what to do with the outputs.
/// [`prepare`] makes it from the map's layers; a runner (the native `geolibre` program, or the
/// browser's WASI runner) stages the files in a working folder, runs the tool there, and hands
/// the new files and its printed lines to [`finish`].
#[derive(Clone, Debug)]
pub struct Job {
    /// The runner's tool id (without [`PREFIX`]).
    pub tool: String,
    /// The tool's display name.
    pub name: String,
    /// Input files to place in the working folder, by file name.
    pub files: Vec<(String, Vec<u8>)>,
    args: Vec<(String, Arg)>,
    /// (file name, layer name) for each declared output.
    outputs: Vec<(String, String)>,
    crs: Crs,
    /// Problems found while staging that don't stop the run.
    notes: Vec<String>,
}

impl Job {
    /// The arguments, with working-folder files under `dir` (a scratch folder natively, `/work`
    /// in the browser), as `(name, value)` pairs for `--name=value`.
    pub fn args(&self, dir: &str) -> Vec<(String, Value)> {
        let dir = dir.trim_end_matches(['/', '\\']);
        let sep = if dir.contains('\\') && !dir.contains('/') { '\\' } else { '/' };
        self.args
            .iter()
            .map(|(k, a)| {
                let v = match a {
                    Arg::Value(v) => v.clone(),
                    Arg::Files(files) => Value::String(
                        files
                            .iter()
                            .map(|f| match f {
                                FileRef::Work(n) => format!("{dir}{sep}{n}"),
                                FileRef::Path(p) => p.clone(),
                            })
                            .collect::<Vec<_>>()
                            .join(";"),
                    ),
                };
                (k.clone(), v)
            })
            .collect()
    }

    /// The arguments as `--name=value` strings (see [`Job::args`]).
    pub fn argv(&self, dir: &str) -> Vec<String> {
        self.args(dir).into_iter().map(|(k, v)| format!("--{k}={}", arg_text(&v))).collect()
    }

    fn stage(&mut self, name: &str, ext: &str, bytes: Vec<u8>) -> FileRef {
        // Numbered, because two layer names can map to the same safe name (`Roads 2020` and
        // `Roads_2020`, or any two non-ASCII names), and one file must never replace another.
        let file = format!("in{}-{}.{ext}", self.files.len(), safe(name));
        self.files.push((file.clone(), bytes));
        FileRef::Work(file)
    }
}

/// A parameter value as the runner's `--key=value` text.
pub fn arg_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Stages one input (a layer by name, or a file path) for `job`.
fn stage(inputs: &dyn Inputs, value: &str, data: Data, tf: &Transformer, job: &mut Job) -> Result<FileRef, String> {
    if let Some(r) = inputs.raster(value) {
        // Every tool places what it makes on the ground; a scan's place is only where it was laid.
        r.raster.require_coordinates().map_err(|e| format!("{value}: {e}"))?;
        if let Some(p) = r.path.filter(|p| on_disk(p)) {
            return Ok(FileRef::Path(p.to_string()));
        }
        // The parts of a large GeoTIFF the web app read are RustGIS's own: written out below instead.
        if let Some((file_name, bytes)) = r.embedded.filter(|(_, b)| !rustgis_data::raster::cog::Sparse::is_container(b)) {
            // The tools tell formats apart by extension.
            let ext = Path::new(file_name).extension().and_then(|e| e.to_str()).map(safe).unwrap_or_else(|| "tif".into());
            return Ok(job.stage(value, &ext, bytes.to_vec()));
        }
        let bytes = rustgis_data::raster::geotiff::write(r.raster, 0).map_err(|e| format!("{value}: {e}"))?;
        return Ok(job.stage(value, "tif", bytes));
    }
    if let Some(ds) = inputs.dataset(value) {
        if data == Data::Table || (ds.features.iter().all(|f| f.geometry.is_none()) && data != Data::Vector) {
            let text = rustgis_data::io::to_csv(ds).map_err(|e| format!("{value}: {e}"))?;
            return Ok(job.stage(value, "csv", text.into_bytes()));
        }
        let (bytes, lost) = vector_bytes(ds, inputs.selection(value), job.crs, tf)?;
        if lost > 0 {
            job.notes.push(format!("{value}: {lost} features lie outside the working coordinate system and were passed without geometry"));
        }
        return Ok(job.stage(value, "geojson", bytes));
    }
    if on_disk(value) {
        return Ok(FileRef::Path(value.to_string()));
    }
    Err(format!("there is no layer or file named '{value}'"))
}

/// The names a Raster Calculator expression quotes, each once, in the order they first appear,
/// read as the runner reads them: between `"` when the expression has one, else between `'`.
fn quoted_names(expression: &str) -> Vec<&str> {
    let delimiter = if expression.contains('"') { '"' } else { '\'' };
    let mut names: Vec<&str> = Vec::new();
    for name in expression.split(delimiter).skip(1).step_by(2).map(str::trim) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The Raster Calculator's `list` of inputs in the order its `expression` first quotes them,
/// and a notice when they keep their order instead. The runner binds the k-th quoted name to the
/// k-th input whatever that input is called, and RustGIS stages a layer under a file name of its
/// own, so without this a layer's name in the expression reads another layer's cells, silently.
/// A quoted name matches an input by layer name, or, for a file, by its path, file name or name
/// without extension; ASCII case aside, as layers are found by name, with an exact match first.
/// A name that matches none, or an input the expression doesn't use, is refused, unless only
/// files are listed and no name matches any: the runner's own contract (the k-th quoted name is
/// the k-th file) then holds as before, and the notice says which name reads which file.
fn calculator_order<'a>(expression: &str, list: Vec<&'a str>, inputs: &dyn Inputs) -> Result<(Vec<&'a str>, Option<String>), String> {
    let quoted = quoted_names(expression);
    if quoted.is_empty() {
        // The runner says an expression needs a quoted name.
        return Ok((list, None));
    }
    let layer: Vec<bool> = list.iter().map(|n| inputs.raster(n).is_some() || inputs.dataset(n).is_some()).collect();
    let file_parts = |n: &'a str| {
        let p = Path::new(n);
        [p.file_name(), p.file_stem()].into_iter().flatten().filter_map(|part| part.to_str())
    };
    // `whole`: the input's whole name (a layer's, or a file's path); else a file's name or stem.
    let matches = |q: &str, i: usize, same: fn(&str, &str) -> bool, whole: bool| {
        if whole { same(q, list[i]) } else { !layer[i] && file_parts(list[i]).any(|part| same(q, part)) }
    };
    let exact: fn(&str, &str) -> bool = |a, b| a == b;
    let any_case: fn(&str, &str) -> bool = |a, b| a.eq_ignore_ascii_case(b);
    // Exact before any case, and a whole name before a file's name, each pass over every name
    // before the next: a layer `A` isn't taken by `/x/A.tif` listed before it, nor `nir.tif` by
    // `'NIR'` before `'nir'` matches it exactly.
    let passes = [(exact, true), (exact, false), (any_case, true), (any_case, false)];
    // Each quoted name takes the first input it matches that no other name took, so one layer
    // listed twice can be quoted under two names (a path and a file name, say).
    let mut taken = vec![false; list.len()];
    let mut bound: Vec<Option<usize>> = vec![None; quoted.len()];
    for (same, whole) in passes {
        for (k, q) in quoted.iter().enumerate() {
            if bound[k].is_none()
                && let Some(i) = (0..list.len()).find(|&i| !taken[i] && matches(q, i, same, whole))
            {
                taken[i] = true;
                bound[k] = Some(i);
            }
        }
    }
    let order: Vec<&'a str> = bound.iter().flatten().map(|&i| list[i]).collect();
    let missing: Vec<String> = quoted.iter().zip(&bound).filter(|(_, b)| b.is_none()).map(|(q, _)| format!("\"{q}\"")).collect();
    if order.is_empty() && !layer.contains(&true) {
        // By position, one file per name: a file left over, or a name without a file, is refused
        // here, so the notice below names every pair.
        if quoted.len() != list.len() {
            return Err(format!(
                "the expression quotes {} name(s) but {} file(s) are listed: list one file per quoted name, in the order they first appear",
                quoted.len(),
                list.len()
            ));
        }
        let pairs: Vec<String> = quoted.iter().zip(&list).map(|(q, n)| format!("\"{q}\" from {n}")).collect();
        let note =
            format!("no quoted name matches an input file's path or name, so they are read in the order listed: {}", pairs.join(", "));
        return Ok((list, Some(note)));
    }
    if !missing.is_empty() {
        return Err(format!(
            "the expression quotes {} but the inputs are {}: quote each input layer's name, or a file's path or name, as it is listed",
            missing.join(", "),
            list.join("; ")
        ));
    }
    let unused: Vec<&str> = list.iter().zip(&taken).filter(|(_, t)| !**t).map(|(n, _)| *n).collect();
    if !unused.is_empty() {
        return Err(format!("the expression doesn't quote the input(s) {}: list only the layers it uses", unused.join("; ")));
    }
    Ok((order, None))
}

/// Whether `p` names something on disk (never in the browser, which has no file system).
fn on_disk(p: &str) -> bool {
    !cfg!(target_arch = "wasm32") && Path::new(p).exists()
}

/// The extension an output is written with.
fn output_ext(p: &MetaParam, meta: &Meta) -> &'static str {
    let has_raster_input = meta.params.iter().any(|q| q.role == Role::Input && q.data == Data::Raster);
    match p.data {
        Data::Raster => "tif",
        Data::Vector => "geojson",
        Data::Table => "csv",
        Data::Lidar => "las",
        Data::Text => "txt",
        Data::Json => "json",
        _ if p.name.contains("html") => "html",
        _ if p.name.contains("csv") => "csv",
        _ if p.name.contains("json") => "json",
        _ if matches!(meta.category.as_str(), "Raster" | "Terrain" | "Hydrology") || has_raster_input => "tif",
        _ => "geojson",
    }
}

/// A number as JSON, whole numbers as integers: the runner reads `--n=3.0` as a float, which a
/// tool expecting an integer (a filter size, a count) rejects.
fn number(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.0e15 { json!(n as i64) } else { json!(n) }
}

/// A form value as the JSON the tool expects (the form sends text).
fn option_value(v: &Value, data: Data) -> Value {
    match (v.as_str().map(str::trim), data) {
        (Some("true"), Data::Bool) => Value::Bool(true),
        (Some("false"), Data::Bool) => Value::Bool(false),
        (Some(s), Data::Number) => s.parse::<f64>().ok().filter(|n| n.is_finite()).map(number).unwrap_or_else(|| v.clone()),
        (None, Data::Number) => v.as_f64().filter(|n| n.is_finite()).map(number).unwrap_or_else(|| v.clone()),
        _ => v.clone(),
    }
}

/// Whether option value `v` is the manifest's default `d`. The form sends text, so a default that
/// isn't text on a field shown as text comes back as its JSON (`[255,255,255,255]`, `false`).
fn is_default(v: &Value, d: &Value, data: Data) -> bool {
    option_value(d, data) == *v || (!d.is_string() && json_text(v).as_ref() == Some(d))
}

/// The JSON a text value spells (`"[255,255,255,255]"` gives the list), if it is text and reads.
fn json_text(v: &Value) -> Option<Value> {
    v.as_str().and_then(|t| serde_json::from_str(t).ok())
}

/// Immutable manifest metadata paired with one published analysis schema.
#[derive(Clone)]
pub struct ResolvedTool {
    spec: &'static ToolSpec,
    meta: Meta,
}
impl ResolvedTool {
    /// Pairs a published analysis schema with metadata from that exact manifest load.
    pub fn capture(spec: &'static ToolSpec) -> Result<Self, String> {
        let id = spec.id.strip_prefix(PREFIX).ok_or("not a GeoLibre tool")?;
        let meta =
            META.lock().ok().and_then(|m| m.as_ref().and_then(|m| m.get(id)).cloned()).ok_or("the GeoLibre metadata is unavailable")?;
        if !std::ptr::eq(meta.schema, spec.params) {
            return Err("the GeoLibre metadata and analysis schema belong to different loads".into());
        }
        let pinned = Self { spec, meta };
        pinned.check()?;
        Ok(pinned)
    }
    /// Rejects a withdrawn or replaced schema before inputs or runner arguments are prepared.
    pub fn check(&self) -> Result<(), String> {
        if !rustgis_analysis::published_external_tools_of(SOURCE).iter().any(|spec| std::ptr::eq(spec, self.spec)) {
            return Err("the GeoLibre tool schema was reloaded".into());
        }
        Ok(())
    }
    /// Preparation uses captured metadata, never another lookup by tool name.
    /// Uses captured manifest metadata to prepare arguments; does not resolve the tool by name.
    pub fn prepare(&self, inputs: &dyn Inputs, params: &Value) -> Result<Job, String> {
        self.check()?;
        prepare_with(self.spec, &self.meta, inputs, params)
    }
}

/// Prepares a run of `spec` (a GeoLibre tool) on the map's layers, with `params` already
/// resolved (defaults filled, required ones present: see `rustgis_analysis::resolve_params`).
pub fn prepare(spec: &ToolSpec, inputs: &dyn Inputs, params: &Value) -> Result<Job, String> {
    let id = spec.id.strip_prefix(PREFIX).ok_or("not a GeoLibre tool")?;
    let meta =
        META.lock().ok().and_then(|m| m.as_ref().and_then(|m| m.get(id)).cloned()).ok_or_else(|| format!("{}: unknown tool", spec.name))?;
    prepare_with(spec, &meta, inputs, params)
}

/// Validates input name lists before choosing a CRS or staging files, preserving their order.
fn prepare_with(spec: &ToolSpec, meta: &Meta, inputs: &dyn Inputs, params: &Value) -> Result<Job, String> {
    let id = spec.id.strip_prefix(PREFIX).ok_or("not a GeoLibre tool")?;
    let value_of = |name: &str| params.get(name).filter(|v| !v.is_null() && v.as_str() != Some(""));
    let names = rustgis_analysis::layer_list;

    // Rasters set the coordinate system; otherwise the vector data's UTM zone.
    let mut raster_crs = Vec::new();
    let mut vector_extents = Vec::new();
    for p in meta.params.iter().filter(|p| p.role == Role::Input) {
        for n in value_of(&p.name).map(names).transpose()?.unwrap_or_default() {
            if let Some(r) = inputs.raster(n) {
                raster_crs.push(r.raster.crs);
            } else if let Some(ds) = inputs.dataset(n) {
                vector_extents.push(ds.lonlat_extent());
            }
        }
    }
    let crs = working_crs(&raster_crs, &vector_extents);
    let tf = crs.transformer()?;
    let mut job = Job {
        tool: id.to_string(),
        name: spec.name.to_string(),
        files: Vec::new(),
        args: Vec::new(),
        outputs: Vec::new(),
        crs,
        notes: Vec::new(),
    };

    let n_outputs = meta.params.iter().filter(|p| p.role == Role::Output).count();
    let given = if meta.names_outputs { output_name(spec, params, &mut job.notes)? } else { None };
    let base = given.as_deref().unwrap_or(spec.name);
    let (mut renamed, mut shortened) = (Vec::new(), false);
    for p in &meta.params {
        match p.role {
            Role::Input => {
                let Some(v) = value_of(&p.name) else { continue };
                let mut list = names(v)?;
                if id == CALCULATOR
                    && p.name == "inputs"
                    && let Some(expression) = params.get("expression").and_then(Value::as_str)
                {
                    let (order, note) = calculator_order(expression, list, inputs).map_err(|e| format!("{}: {e}", spec.name))?;
                    list = order;
                    job.notes.extend(note.map(|n| format!("{}: {n}", spec.name)));
                }
                let files = list.into_iter().map(|n| stage(inputs, n, p.data, &tf, &mut job)).collect::<Result<Vec<_>, _>>()?;
                if !files.is_empty() {
                    job.args.push((p.name.clone(), Arg::Files(files)));
                }
            }
            Role::Output => {
                let file = format!("{}.{}", safe(&p.name), output_ext(p, meta));
                job.args.push((p.name.clone(), Arg::Files(vec![FileRef::Work(file.clone())])));
                // Each output's own name after the one given, kept whole within the limit.
                let suffix = if n_outputs > 1 { format!(" {}", label_of(&p.name).to_lowercase()) } else { String::new() };
                let room = MAX_OUTPUT_NAME.saturating_sub(suffix.chars().count());
                shortened |= given.is_some() && base.chars().count() > room;
                let name: String = base.chars().take(room).chain(suffix.chars()).collect();
                // A name a layer, a table or an earlier output of this run has gets a number, so two
                // runs don't give two layers of one name. Said when it was the name given.
                let taken = |n: &str| {
                    inputs.dataset(n).is_some() || inputs.raster(n).is_some() || job.outputs.iter().any(|(_, o)| o.eq_ignore_ascii_case(n))
                };
                let free = numbered(base, &suffix, taken);
                if given.is_some() && free != name {
                    renamed.push(format!("'{name}' is now '{free}'"));
                }
                job.outputs.push((file, free));
            }
            Role::Option => {
                let Some(v) = value_of(&p.name).map(|v| option_value(v, p.data)) else { continue };
                // A value left at the manifest's default isn't passed: the runner then uses its
                // own default, as when it runs alone. They can differ: `fill_depressions` lists
                // a `flat_increment` of 0.0001 but, without one, picks a far smaller step that
                // keeps flats draining (#237).
                if p.default.as_ref().is_some_and(|d| is_default(&v, d, p.data)) {
                    continue;
                }
                job.args.push((p.name.clone(), Arg::Value(v)));
            }
        }
    }
    // One notice per kind, with a count.
    if !renamed.is_empty() {
        job.notes.push(format!("{}: {} output name(s) in use already, so numbered: {}", spec.name, renamed.len(), renamed.join(", ")));
    }
    if shortened {
        job.notes.push(format!("{}: the output name is cut to fit each output's own name within {MAX_OUTPUT_NAME} characters", spec.name));
    }
    Ok(job)
}

/// The layer name given in [`OUTPUT`], trimmed, or `None` for the tool's own name. Cut to
/// [`MAX_OUTPUT_NAME`] characters, which is said.
fn output_name(spec: &ToolSpec, params: &Value, notes: &mut Vec<String>) -> Result<Option<String>, String> {
    let name = match params.get(OUTPUT) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(s)) => s.trim(),
        Some(_) => return Err(format!("{}: '{OUTPUT}' is the new layer's name and must be text", spec.name)),
    };
    if name.is_empty() {
        return Ok(None);
    }
    let n = name.chars().count();
    if n > MAX_OUTPUT_NAME {
        notes.push(format!("{}: the output name is cut from {n} to {MAX_OUTPUT_NAME} characters", spec.name));
    }
    Ok(Some(name.chars().take(MAX_OUTPUT_NAME).collect()))
}

/// `base` with `suffix` (one output's own name) after it, or, while `taken` claims that, the
/// first with ` 2`, ` 3`… after the suffix that it doesn't. Each fits [`MAX_OUTPUT_NAME`]
/// characters by cutting `base`, never the suffix or the number.
fn numbered(base: &str, suffix: &str, taken: impl Fn(&str) -> bool) -> String {
    let fit = |tail: &str| -> String {
        let room = MAX_OUTPUT_NAME.saturating_sub(tail.chars().count());
        base.chars().take(room).chain(tail.chars()).collect()
    };
    let mut name = fit(suffix);
    let mut k = 1u64;
    while taken(&name) {
        k += 1;
        name = fit(&format!("{suffix} {k}"));
    }
    name
}

/// Coordinates that can't be longitude/latitude (a tool kept the working coordinate system).
fn looks_projected(ds: &Dataset) -> bool {
    ds.extent().beyond_lonlat()
}

/// `bytes` without a byte-order mark, when they start as a JSON object (after white space) rather
/// than with a TIFF's signature (which never starts with `{`).
fn json_not_tiff(bytes: &[u8]) -> Option<&[u8]> {
    let json = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(bytes);
    (json.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{')).then_some(json)
}

/// A JSON object (after white space; [`json_not_tiff`] has left out a byte-order mark) whose own
/// `type` ([`own_type`]) is one of GeoJSON's (a feature collection, a feature or a geometry),
/// TopoJSON's or CityJSON's, which RustGIS reads as layers. Whether the document parses doesn't
/// matter: GeoJSON cut short while it was written is read, so the reader's error is shown rather
/// than its text. Anything else is a report: a list, plain text, or an object with no `type`, with
/// a `type` that isn't text, or with another one.
fn is_geojson(json: &[u8]) -> bool {
    const TYPES: [&str; 11] = [
        "FeatureCollection",
        "Topology",
        "CityJSON",
        "Feature",
        "Point",
        "MultiPoint",
        "LineString",
        "MultiLineString",
        "Polygon",
        "MultiPolygon",
        "GeometryCollection",
    ];
    json.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{') && own_type(json).is_some_and(|t| TYPES.contains(&t.as_str()))
}

/// The `type` member of the JSON object `json` starts with, when its value is a string: among the
/// object's own members, wherever it comes (after a `crs`, after the features), not one nested in
/// them; names and values with their escapes decoded (`"\u0074ype"` is `"type"`). The first `type`
/// counts, should there be two. One pass, stopping at the `type` or at the object's end, and the
/// same whether the document is whole or cut short.
fn own_type(json: &[u8]) -> Option<String> {
    /// What the last of the object's own tokens was, as far as finding `"type": "…"` goes.
    #[derive(PartialEq)]
    enum Last {
        Other,
        TypeName,
        TypeColon,
    }
    let (mut depth, mut i, mut last) = (0usize, 0usize, Last::Other);
    while let Some(&b) = json.get(i) {
        match b {
            b'"' => {
                let (text, next) = json_string(json, i + 1, depth == 1);
                i = next;
                if depth == 1 {
                    if last == Last::TypeColon {
                        return text;
                    }
                    last = if text.as_deref() == Some("type") { Last::TypeName } else { Last::Other };
                }
                continue;
            }
            b'{' | b'[' => {
                depth += 1;
                if depth == 2 {
                    last = Last::Other;
                }
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return None;
                }
            }
            b':' if depth == 1 => last = if last == Last::TypeName { Last::TypeColon } else { Last::Other },
            _ if b.is_ascii_whitespace() => {}
            _ if depth == 1 => last = Last::Other,
            _ => {}
        }
        i += 1;
    }
    None
}

/// The JSON string whose text starts at `json[start]` (after its opening quote), its escapes
/// decoded when `decode` (else `None`, only skipped), and where after its closing quote the
/// document goes on (its end, when it is cut short inside the string).
fn json_string(json: &[u8], start: usize, decode: bool) -> (Option<String>, usize) {
    let mut out = Vec::new();
    let mut i = start;
    while let Some(&b) = json.get(i) {
        match b {
            b'"' => return (decode.then(|| String::from_utf8_lossy(&out).into_owned()), i + 1),
            b'\\' => {
                let Some(&e) = json.get(i + 1) else { break };
                i += 2;
                if !decode {
                    continue;
                }
                match e {
                    b'u' => {
                        let hex = |at: usize| {
                            json.get(at..at + 4)
                                .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
                                .and_then(|h| std::str::from_utf8(h).ok())
                                .and_then(|h| u32::from_str_radix(h, 16).ok())
                        };
                        let Some(mut code) = hex(i) else { continue };
                        i += 4;
                        // A pair of surrogates is one character.
                        if (0xD800..0xDC00).contains(&code)
                            && json.get(i..i + 2) == Some(b"\\u".as_slice())
                            && let Some(low) = hex(i + 2).filter(|l| (0xDC00..0xE000).contains(l))
                        {
                            code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            i += 6;
                        }
                        let c = char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER);
                        out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                    }
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    other => out.push(other),
                }
                continue;
            }
            _ => {
                if decode {
                    out.push(b);
                }
            }
        }
        i += 1;
    }
    (decode.then(|| String::from_utf8_lossy(&out).into_owned()), json.len())
}

/// Reads one output file back as a layer, table or message. `dir` is the native working folder,
/// for formats read with their sidecar files (shapefiles).
fn load(
    file_name: &str,
    bytes: &[u8],
    layer_name: &str,
    job: &Job,
    dir: Option<&Path>,
    messages: &mut Vec<String>,
    losses: &mut Vec<rustgis_data::loss::LossMessage>,
) -> Option<OutputLayer> {
    let mut ext = Path::new(file_name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    // A tool whose manifest calls its vector output a raster writes GeoJSON into the `.tif` it is
    // given (Longest Flowpath, #2762): read by what it holds, GeoJSON as `.json` and another JSON
    // document (a report) as text. A JSON object is taken without its byte-order mark, which the
    // JSON reader refuses; whether it is a layer ([`is_geojson`]) is worked out once, in a pass
    // over its members up to its `type`, and only where it decides something (not for a `.geojson`).
    let object = if matches!(ext.as_str(), "tif" | "tiff" | "json" | "geojson") { json_not_tiff(bytes) } else { None };
    let layer = matches!(ext.as_str(), "tif" | "tiff" | "json") && object.is_some_and(is_geojson);
    let renamed;
    let (file_name, bytes) = match object {
        Some(json) if matches!(ext.as_str(), "tif" | "tiff") => {
            ext = if layer { "json" } else { "txt" }.into();
            renamed = Path::new(file_name).with_extension(&ext).display().to_string();
            (renamed.as_str(), json)
        }
        // A `.json` object that isn't GeoJSON (the reader would take it for a feature without a
        // geometry) is shown as a report, as it is from a `.tif`.
        Some(json) if ext == "json" && !layer => {
            ext = "txt".into();
            (file_name, json)
        }
        Some(json) => (file_name, json),
        None if matches!(ext.as_str(), "json" | "geojson") => (file_name, bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(bytes)),
        None => (file_name, bytes),
    };
    match ext.as_str() {
        "tif" | "tiff" | "asc" => match rustgis_data::raster::read_bytes(file_name, bytes) {
            Ok(grey) => {
                let bytes = if ext == "asc" { bytes.to_vec() } else { colours_split(&grey, bytes, layer_name, messages) };
                Some(OutputLayer::Raster { name: layer_name.to_string(), file_name: file_name.to_string(), bytes })
            }
            Err(e) => {
                messages.push(format!("{layer_name}: {e}"));
                None
            }
        },
        "geojson" | "json" | "shp" | "csv" => {
            let read = match (ext.as_str(), dir) {
                ("shp", Some(d)) => rustgis_data::io::read_path(&d.join(file_name)),
                _ => rustgis_data::io::read_bytes(file_name, bytes),
            };
            match read {
                Ok((mut ds, report)) => {
                    ds.name = layer_name.to_string();
                    // What the reader left out or assumed (features without a geometry it reads, Z
                    // values, a `crs` it doesn't know) is said, as when the file is added.
                    for note in report.messages {
                        let note = note.prefixed(layer_name);
                        match note.loss {
                            Some(code) => losses.push(rustgis_data::loss::LossMessage { code, text: note.text }),
                            None => messages.push(note.text),
                        }
                    }
                    // An output that says its system (a .prj, a GeoJSON `crs`) is held in it as
                    // read; one in the working system that doesn't is relabelled to it.
                    if job.crs != Crs::LonLat && ds.coord_crs() == Crs::LonLat && looks_projected(&ds) {
                        match job.crs.to(Crs::LonLat) {
                            Ok(to) => {
                                ds.held_in = job.crs;
                                ds.crs = Some(job.crs);
                                // A feature with a place that can't be given in longitude/latitude
                                // is kept as read, and counted: it doesn't draw (#2032 C2f-4b). Each
                                // place is converted to be checked; the layer keeps its own.
                                let (mut lost, mut beyond) = (0usize, 0usize);
                                for g in ds.features.rows().filter_map(|r| r.geometry()) {
                                    match Dataset::converted_to_lonlat(&to, g) {
                                        Some(l) => beyond += usize::from(l.extent().beyond_lonlat()),
                                        None => lost += 1,
                                    }
                                }
                                if beyond > 0 {
                                    let have = if beyond == 1 { "feature has" } else { "features have" };
                                    messages.push(format!(
                                        "{layer_name}: {beyond} {have} coordinates that come out beyond longitude/latitude once converted from {}: it may not be their coordinate system, and they may draw out of place",
                                        job.crs.label()
                                    ));
                                }
                                if lost > 0 {
                                    let have = if lost == 1 { "feature has" } else { "features have" };
                                    messages.push(format!(
                                        "{layer_name}: {lost} {have} a place in {} that can't be given in longitude/latitude: kept as read, but not drawn",
                                        job.crs.label()
                                    ));
                                }
                            }
                            // Not held in a system it can't be converted from: kept as read, and said.
                            Err(e) => messages.push(format!(
                                "{layer_name}: its {} features are in {}, which can't be converted to longitude/latitude ({e}); they are kept as read and draw off the map",
                                ds.features.len(),
                                job.crs.label()
                            )),
                        }
                    }
                    Some(if ds.features.iter().all(|f| f.geometry.is_none()) { OutputLayer::Table(ds) } else { OutputLayer::Vector(ds) })
                }
                // A JSON report rather than GeoJSON: show it. GeoJSON that can't be read says why.
                Err(_) if ext == "json" && !layer => {
                    text_message(bytes, layer_name, false, messages);
                    None
                }
                Err(e) => {
                    messages.push(format!("{layer_name}: {e}"));
                    None
                }
            }
        }
        "txt" | "html" | "htm" => {
            text_message(bytes, layer_name, ext != "txt", messages);
            None
        }
        _ => {
            messages.push(format!("{layer_name}: wrote a .{ext} file, which RustGIS can't open as a layer"));
            None
        }
    }
}

/// A GeoTIFF output's bytes for its layer: one of packed colour (a single `UInt32` band of
/// `0xAABBGGRR`, as Panchromatic Sharpening writes by default) rewritten as red, green, blue and
/// alpha byte bands before anything holds it as `f32`, which drops the red byte (#2594), and said;
/// any other as the tool wrote it. `grey` is the file as read.
fn colours_split(grey: &rustgis_data::raster::Raster, bytes: &[u8], layer_name: &str, messages: &mut Vec<String>) -> Vec<u8> {
    use rustgis_data::raster::geotiff;
    match geotiff::unpack_rgba(grey, bytes).and_then(|r| r.map(|r| geotiff::write_all(&r)).transpose()) {
        Ok(Some(rgba)) => {
            messages.push(format!(
                "{layer_name}: its one band of packed colours (alpha, blue, green and red bytes) was split into red, green and blue bands, see-through where alpha is 0 or the pixel is no-data"
            ));
            // What reading the tool's file left out, which the rewritten file no longer shows.
            messages.extend(grey.lost.iter().map(|l| format!("{layer_name}: {l}")));
            rgba
        }
        Ok(None) => bytes.to_vec(),
        Err(e) => {
            messages.push(format!(
                "{layer_name}: its one band of packed colours couldn't be split into red, green and blue bands ({e}): it is kept as one band of 32-bit floats, which lose the red byte"
            ));
            bytes.to_vec()
        }
    }
}

/// `html` without its tags.
fn strip_tags(html: &str) -> String {
    let mut plain = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }
    plain
}

/// The most lines of a report shown.
const REPORT_LINES: usize = 30;
/// The most characters of a report's line shown (a JSON report may be one long line).
const REPORT_LINE_CHARS: usize = 500;

/// The start of a text, JSON or (`html`) HTML report as a message, its tags left out, saying how many
/// more lines it has.
fn text_message(bytes: &[u8], layer_name: &str, html: bool, messages: &mut Vec<String>) {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    let plain: std::borrow::Cow<str> = if html { strip_tags(text).into() } else { text.into() };
    // The first lines kept, the rest only counted, so a large report costs no more than its text.
    let mut rest = plain.lines().map(str::trim).filter(|l| !l.is_empty());
    let shown: Vec<&str> = rest.by_ref().take(REPORT_LINES).collect();
    let more = rest.count();
    let mut cut = 0;
    let lines: Vec<String> = shown
        .iter()
        .map(|l| match l.char_indices().nth(REPORT_LINE_CHARS) {
            Some((at, _)) => {
                cut += 1;
                format!("{}…", l.get(..at).unwrap_or(l))
            }
            None => l.to_string(),
        })
        .collect();
    if !lines.is_empty() {
        messages.push(format!("{layer_name}:\n{}", lines.join("\n")));
    }
    if more > 0 {
        messages.push(format!("{layer_name}: the report's other {more} non-empty lines aren't shown"));
    }
    if cut > 0 {
        messages.push(format!("{layer_name}: lines of the report cut at {REPORT_LINE_CHARS} characters: {cut}"));
    }
}

/// Turns a finished run into layers and messages. `files` are the new files in the working
/// folder (by file name), `lines` what the runner printed (its last line, if JSON, holds the
/// tool's structured results), `dir` the native working folder if there is one.
pub fn finish(job: &Job, files: &[(String, Vec<u8>)], lines: &[String], dir: Option<&Path>) -> ToolResult {
    let mut lines: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    let outputs = match lines.last().map(|l| serde_json::from_str::<Value>(l)) {
        Some(Ok(Value::Object(m))) => {
            lines.pop();
            m
        }
        _ => serde_json::Map::new(),
    };
    let mut messages = job.notes.clone();
    messages.extend(lines.into_iter().take(40).map(str::to_string));

    let mut layers = Vec::new();
    let mut losses = Vec::new();
    for (file, name) in &job.outputs {
        // A tool may pick its own extension (a shapefile for a vector output).
        let stem = Path::new(file).file_stem();
        let found = files.iter().find(|(f, _)| f == file).or_else(|| {
            files.iter().find(|(f, _)| {
                let p = Path::new(f);
                p.file_stem() == stem && p.extension().is_some_and(|e| e != "dbf" && e != "shx" && e != "prj")
            })
        });
        if let Some((f, bytes)) = found
            && let Some(l) = load(f, bytes, name, job, dir, &mut messages, &mut losses)
        {
            layers.push(l);
        }
    }
    // Statistics and other results the tool returns rather than writes (paths of the outputs,
    // which came back as layers, are left out).
    for (k, v) in &outputs {
        let text = match v {
            Value::String(s)
                if job.outputs.iter().any(|(f, _)| s == f || s.ends_with(&format!("/{f}")) || s.ends_with(&format!("\\{f}"))) =>
            {
                continue;
            }
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let text: String = text.chars().take(2000).collect();
        messages.push(format!("{}: {text}", label_of(k)));
    }
    if layers.is_empty() && messages.is_empty() {
        messages.push(format!("{} finished without output", job.name));
    }
    ToolResult { losses, output: Output::Layers(layers), messages }
}

/// Runs a tool to completion: natively through the `geolibre` program; in the browser the
/// runner is asynchronous, so the app uses [`prepare`] and [`finish`] around it instead.
fn run(spec: &ToolSpec, inputs: &dyn Inputs, params: &Value) -> Result<ToolResult, String> {
    let job = prepare(spec, inputs, params)?;
    #[cfg(not(target_arch = "wasm32"))]
    {
        native::run(&job).map_err(|e| format!("{}: {e}", spec.name))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = job;
        Err(format!("{}: in the browser, run GeoLibre tools from the Tools pane", spec.name))
    }
}

/// Runs the immutable native choice supplied by the engine. Development-process bytes remain
/// unsandboxed and unattested, exactly as for the ordinary runner.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_pinned(
    tool: &ResolvedTool,
    inputs: &dyn Inputs,
    params: &Value,
    pin: &runner::ResolvedRunner,
    live: &dyn Fn() -> Result<(), String>,
) -> Result<ToolResult, String> {
    pin.check()?;
    live()?;
    let job = tool.prepare(inputs, params)?;
    native::run_pinned(&job, pin, live)
}

#[cfg(test)]
mod tests;
