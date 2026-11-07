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
        let data = Data::of(p.get("data_kind").and_then(Value::as_str).unwrap_or(""));
        let role = match p.get("io_role").and_then(Value::as_str) {
            Some("input") => Role::Input,
            Some("output") => Role::Output,
            _ => Role::Option,
        };
        let many = schema.and_then(|s| s.get("cardinality")).and_then(Value::as_str).is_some_and(|c| c != "single");
        let optional = !p.get("required").and_then(Value::as_bool).unwrap_or(false);
        let default = defaults.and_then(|d| d.get(pname)).filter(|v| optional && !v.is_null()).cloned();
        meta.push(MetaParam { name: pname.to_string(), role, data, default });
        // Outputs are written to a scratch folder and come back as layers: no form field.
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
        params.push(ParamSpec {
            name: leak(pname),
            label: leak(label_of(pname)),
            kind,
            required: p.get("required").and_then(Value::as_bool).unwrap_or(false),
            default: leak(default),
            help: leak(help),
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
    Some((spec, Meta { schema, category, params: meta }))
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

fn prepare_with(spec: &ToolSpec, meta: &Meta, inputs: &dyn Inputs, params: &Value) -> Result<Job, String> {
    let id = spec.id.strip_prefix(PREFIX).ok_or("not a GeoLibre tool")?;
    let value_of = |name: &str| params.get(name).filter(|v| !v.is_null() && v.as_str() != Some(""));
    let names = |v: &Value| -> Vec<String> {
        v.as_str().map(|s| s.split(';').map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).collect()).unwrap_or_default()
    };

    // Rasters set the coordinate system; otherwise the vector data's UTM zone.
    let mut raster_crs = Vec::new();
    let mut vector_extents = Vec::new();
    for p in meta.params.iter().filter(|p| p.role == Role::Input) {
        for n in value_of(&p.name).map(names).unwrap_or_default() {
            if let Some(r) = inputs.raster(&n) {
                raster_crs.push(r.raster.crs);
            } else if let Some(ds) = inputs.dataset(&n) {
                vector_extents.push(ds.extent());
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
    for p in &meta.params {
        match p.role {
            Role::Input => {
                let Some(v) = value_of(&p.name) else { continue };
                let files = names(v).iter().map(|n| stage(inputs, n, p.data, &tf, &mut job)).collect::<Result<Vec<_>, _>>()?;
                if !files.is_empty() {
                    job.args.push((p.name.clone(), Arg::Files(files)));
                }
            }
            Role::Output => {
                let file = format!("{}.{}", safe(&p.name), output_ext(p, meta));
                job.args.push((p.name.clone(), Arg::Files(vec![FileRef::Work(file.clone())])));
                let name =
                    if n_outputs > 1 { format!("{} {}", spec.name, label_of(&p.name).to_lowercase()) } else { spec.name.to_string() };
                job.outputs.push((file, name));
            }
            Role::Option => {
                let Some(v) = value_of(&p.name).map(|v| option_value(v, p.data)) else { continue };
                // A value left at the manifest's default isn't passed: the runner then uses its
                // own default, as when it runs alone. They can differ: `fill_depressions` lists
                // a `flat_increment` of 0.0001 but, without one, picks a far smaller step that
                // keeps flats draining (#237).
                if p.default.as_ref().is_some_and(|d| option_value(d, p.data) == v) {
                    continue;
                }
                job.args.push((p.name.clone(), Arg::Value(v)));
            }
        }
    }
    Ok(job)
}

/// Coordinates that can't be longitude/latitude (a tool kept the working coordinate system).
fn looks_projected(ds: &Dataset) -> bool {
    let e = ds.extent();
    !e.is_empty() && (e.min_x < -180.5 || e.max_x > 180.5 || e.min_y < -90.5 || e.max_y > 90.5)
}

/// Reads one output file back as a layer, table or message. `dir` is the native working folder,
/// for formats read with their sidecar files (shapefiles).
fn load(file_name: &str, bytes: &[u8], layer_name: &str, job: &Job, dir: Option<&Path>, messages: &mut Vec<String>) -> Option<OutputLayer> {
    let ext = Path::new(file_name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "tif" | "tiff" | "asc" => match rustgis_data::raster::read_bytes(file_name, bytes) {
            Ok(_) => Some(OutputLayer::Raster { name: layer_name.to_string(), file_name: file_name.to_string(), bytes: bytes.to_vec() }),
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
                Ok((mut ds, _)) => {
                    ds.name = layer_name.to_string();
                    if job.crs != Crs::LonLat
                        && looks_projected(&ds)
                        && let Ok(tf) = job.crs.transformer()
                    {
                        let lost = convert_features(&mut ds, |c| tf.to_lonlat(c));
                        if lost > 0 {
                            messages.push(format!(
                                "{layer_name}: {lost} features couldn't be converted to longitude/latitude and have no geometry"
                            ));
                        }
                    }
                    Some(if ds.features.iter().all(|f| f.geometry.is_none()) { OutputLayer::Table(ds) } else { OutputLayer::Vector(ds) })
                }
                // A JSON report rather than GeoJSON: show it.
                Err(_) if ext == "json" => {
                    text_message(bytes, layer_name, messages);
                    None
                }
                Err(e) => {
                    messages.push(format!("{layer_name}: {e}"));
                    None
                }
            }
        }
        "txt" | "html" | "htm" => {
            text_message(bytes, layer_name, messages);
            None
        }
        _ => {
            messages.push(format!("{layer_name}: wrote a .{ext} file, which RustGIS can't open as a layer"));
            None
        }
    }
}

/// The start of a text or HTML report as a message.
fn text_message(bytes: &[u8], layer_name: &str, messages: &mut Vec<String>) {
    let text = String::from_utf8_lossy(bytes);
    let mut plain = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }
    let lines: Vec<&str> = plain.lines().map(str::trim).filter(|l| !l.is_empty()).take(30).collect();
    if !lines.is_empty() {
        messages.push(format!("{layer_name}:\n{}", lines.join("\n")));
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
            && let Some(l) = load(f, bytes, name, job, dir, &mut messages)
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
    ToolResult { output: Output::Layers(layers), messages }
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
