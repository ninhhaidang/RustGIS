//! Analyze tab: geoprocessing environments, models, scripts, tool galleries, raster functions
//! and the workflows (suitability, visibility, lines of sight, interpolation, network routes,
//! neighbourhoods, simulation, data profiles, exploration, similarity and format conversion).

use std::collections::HashMap;

use rustgis_analysis::{network, rasterops};
use rustgis_data::raster::{Crs, Raster};
use rustgis_data::{Dataset, Field, FieldType, Geometry, GeometryType, Value as Attr};
use rustgis_geom::{Coord, Extent};
use serde_json::{Value, json};

use super::{b, f, ps};
use crate::project::{Model, ModelStep};
use crate::{CmdResult, CommandSpec as C, Session};

/// Registers workflow and raster commands with the parameter help used by command clients.
pub fn commands() -> Vec<C> {
    vec![
        C::new("gp.env", "Environments", "Analyze › Geoprocessing", environments).inputs(environment_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"extent"?: "view"|"full"|layer name|[minLon, minLat, maxLon, maxLat]|null, "crs"?: projected coordinate system ("EPSG:32616") rasters are made in|null (none: the layer's projected system, else degrees), "cellSize"?: in degrees, or in the units of the crs when one is set|null}"#,
        ),
        // A setting of the whole process, not of the project: no undo step, and `Reserved`, so the
        // assistant asks first and agents, plugins and imported items can't change it.
        C::new("gp.threads", "Threads", "Analyze › Geoprocessing", threads).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved)).view().params(
            r#"{"threads"?: the most threads one piece of work uses, 1–1024, for the whole process and not saved with the project (null: the default, the cores up to 16); without it, the setting is only said}"#,
        ),
        C::new("gp.gallery", "Tool Gallery", "Analyze › Portal", gallery).roles(&[]).effects(crate::policy::Effects::NONE).view().params(r#"{"kind": "ready"|"feature"|"raster"}"#),
        C::new("model.new", "ModelBuilder", "Analyze › Geoprocessing", new_model).roles(&[crate::ParamRole { key: "steps", role: crate::Role::ModelSteps }, crate::ParamRole { key: "graph", role: crate::Role::ModelGraph }]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved).with(crate::policy::Effect::Files)).params(
            r#"{"name"?: string, "graph"?: {"nodes": [{"id", "kind": "input"|"tool"|"output", "x", "y" (optional), an input's "layer", a tool's "tool" and "params", an output's "name"}], "edges": [{"from", "fromPort": "out", "to", "toPort": a tool's layer parameter | "in" (output)}]}, "steps"?: [{"tool": id, "params": {…, "%1" for step 1's output}}], "fromHistory"?: bool, "imported"?: bool (from a file: its tools may not touch files)}"#,
        ),
        C::new("model.save", "Save Model", "Analyze › Geoprocessing", save_model).roles(&[crate::ParamRole { key: "graph", role: crate::Role::ModelGraph }]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Reserved))
            .params(r#"{"name": model (made if new), "graph": graph (as model.new), "rename"?: new name, "imported"?: bool (from a file: its tools may not touch files; a model from a file stays one)}"#),
        C::new("model.run", "Run Model", "Analyze › Geoprocessing", run_model).roles(&[crate::ParamRole { key: "graph", role: crate::Role::ModelGraph }]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Automation))
            .params(r#"{"name"?: model (default: the last), "graph"?: run this graph instead (as model.new)}"#),
        C::new("model.validate", "Validate Model", "Analyze › Geoprocessing", |s, p| {
            let (_, g, _) = model_graph(s, p)?;
            note_passed_graph(s, p);
            let problems = crate::model::validate(s, &g);
            // What the model does that it may not mean, which doesn't keep it from running.
            let warnings = crate::model::warnings(&g);
            Ok(json!({"ok": problems.is_empty(), "problems": problems, "warnings": warnings}))
        }).roles(&[crate::ParamRole { key: "graph", role: crate::Role::ModelGraph }]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"name"?: model (default: the last), "graph"?: graph}"#),
        C::new("model.export", "Export Model", "Analyze › Geoprocessing", export_model).roles(&[crate::ParamRole { key: "graph", role: crate::Role::ModelGraph }]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"name"?: model (default: the last), "graph"?: export this graph instead, "path"?: "*.model.json" (else the file's text is returned)}"#),
        C::new("model.remove", "Delete Model", "Browser", |s, p| {
            let n = ps(p, "name").ok_or("name is required")?;
            let before = s.project.models.len();
            s.project.models.retain(|m| !m.name.eq_ignore_ascii_case(n));
            s.trusted_models.remove(&n.to_ascii_lowercase());
            if s.project.models.len() == before { Err(format!("no model '{n}'")) } else { Ok(Value::Null) }
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .params(r#"{"name": model}"#),
        C::new("script.run", "Run Script", "Analyze › Geoprocessing", run_script).roles(&[crate::ParamRole { key: "script", role: crate::Role::Script }]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Automation))
            .params(r#"{"script": "one command per line: id {json}; # comments"}"#),
        C::new("raster.function", "Raster Functions", "Analyze › Raster", raster_function).inputs(function_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layer": raster, "function": "slope"|"aspect"|"hillshade"|"ndvi"|"bandMath"|"remap"|"threshold"|"resample", "units"?: "degrees"|"percent" (slope only; default degrees; percent is 100 × tan(angle in radians)), "band"?: 1-based, "expression"?: "(b4-b3)/(b4+b3)", "ranges"?: [[lo, hi, value]] (lo inclusive, hi exclusive; [21, 22, 1] maps integer class 21 to 1), "value"?: number, "keep"?: bool (remap: keep unmatched cells), "nir"?, "red"?: band numbers (ndvi; default 4 and 3), "azimuth"?, "altitude"?, "zFactor"?, "target"?: raster (resample onto its grid), "name"?: output, "ran"?: (internal: a raster the app has worked out)}"#,
        ),
        C::new("raster.functionChain", "Function Editor", "Analyze › Raster", function_chain).inputs(chain_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"layer": raster, "steps": [{"function", … (as raster.function; slope units: "degrees" (default) or "percent")}], "name"?: output, "ran"?: (internal: a raster the app has worked out)}"#),
        C::new("analysis.suitability", "Suitability Modeler", "Analyze › Workflows", suitability).inputs(suitability_inputs).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"criteria": [{"layer": raster, "band"?: 1-based, "ranges": [[lo, hi, score]] (lo inclusive, hi exclusive; [21, 22, 1] scores integer class 21 as 1), "weight": number}], "name"?: output, "ran"?: (internal: a raster the app has worked out)}"#,
        ),
        C::new("analysis.viewshed", "Visibility Analysis", "Analyze › Workflows", viewshed).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"layer": elevation raster, "lon", "lat", "band"?: 1-based, "height"?: observer metres, 0 to 10,000 (default 1.7), "radius"?: metres (default: the whole raster), "heights"?: "nearest" (default) | "bilinear" (the ground along each ray), "name"?: output, "ran"?: (internal: a viewshed the app has worked out)}"#),
        C::new("analysis.lineOfSight", "Line Of Sight", "Analyze › Workflows", line_of_sight).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layer": elevation raster, "from": [lon, lat], "to": [lon, lat], "band"?: 1-based, "observerHeight"?: m, "targetHeight"?: m, "name"?: output}"#,
        ),
        C::new("analysis.interpolate", "Geostatistical Wizard", "Analyze › Workflows", interpolate).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layer": points, "field": numeric field, "crs"?: projected coordinate system ("EPSG:32616") to make it in (default the environment's, else the layer's projected system, else degrees; "EPSG:4326" for degrees), "cellSize"?: in degrees, or in the units of the crs, "power"?: 2, "neighbors"?: 12, "name"?: output, "ran"?: (internal: a surface the app has worked out)}"#,
        ),
        C::new("analysis.route", "Network Analysis", "Analyze › Workflows", route).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layer": line network, "from": [lon, lat], "to": [lon, lat], "costField"?: field, "snapDistance"?: metres, "name"?: output}"#,
        ),
        C::new("analysis.neighborhood", "Neighborhood Explorer", "Analyze › Workflows", neighborhood).roles(&[]).effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"lon", "lat", "radius": metres, "layers"?: [names], "select"?: bool}"#),
        C::new("analysis.simulate", "Simulation", "Analyze › Workflows", simulate).roles(&[]).effects(crate::policy::Effects::NONE).params(
            r#"{"layer": polygons, "count"?: points per polygon, "seed"?: number, "field"?: numeric field (count per polygon from it, scaled by "rate"), "rate"?: number, "name"?: output}"#,
        ),
        C::new("data.profile", "Data Engineering", "Analyze › Workflows", |s, p| {
            // All resident rows: completeness profiling is independent of map filters/selection.
            let ds = feature_ds(s, p)?;
            let rows: Vec<Value> = network::profile_fields(ds)
                .into_iter()
                .map(|f| json!({"field": f.name, "type": f.ty, "count": f.count, "nulls": f.nulls, "distinct": f.distinct, "truncated": f.truncated, "min": f.min, "max": f.max, "mean": f.mean, "top": f.top}))
                .collect();
            Ok(json!({"layer": ds.name, "features": ds.features.len(), "fields": rows}))
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .view()
        .params(r#"{"layer"?}"#),
        C::new("data.explore", "Exploratory Analysis", "Analyze › Workflows", explore).roles(&[]).effects(crate::policy::Effects::NONE)
            .view()
            .params(r#"{"layer"?, "fields"?: [numeric fields], "bins"?: number}"#),
        C::new("data.similar", "Embeddings", "Analyze › Workflows", super::similar::run).roles(&[]).effects(crate::policy::Effects::NONE)
            .params(r#"{"layer"?, "row"?: number (zero-based whole number; selected feature when omitted or null), "fields"?: [numeric fields], "count"?: number (whole number 1 to 1000; default 10), "select"?: bool}"#),
        C::new("data.convert", "Data Interop", "Analyze › Workflows", convert).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
            .view()
            .params(r#"{"layer"?, "format": "geojson"|"csv"|"kml"|"gpx"|"wkt", "path"?: file (the text is returned without one), "nameField"?: field}"#),
    ]
}

fn feature_ds<'a>(s: &'a Session, p: &Value) -> Result<&'a Dataset, String> {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    if l.is_raster() {
        return Err("this works on feature layers".into());
    }
    Ok(&l.dataset)
}

fn raster_of(s: &Session, name: &str) -> Result<std::sync::Arc<Raster>, String> {
    s.map()
        .layer_by_name(name)
        .and_then(|l| l.raster.as_ref())
        .and_then(|r| r.data.clone())
        .ok_or_else(|| format!("no raster layer '{name}'"))
}

fn band_of(p: &Value, r: &Raster) -> Result<usize, String> {
    let b = p.get("band").and_then(Value::as_u64).unwrap_or(1) as usize;
    if b == 0 || b > r.band_count() { Err(format!("{} has bands 1 to {}", r.name, r.band_count())) } else { Ok(b - 1) }
}

fn point(p: &Value, k: &str) -> Result<Coord, String> {
    let a = p.get(k).and_then(Value::as_array).ok_or_else(|| format!("{k} is [lon, lat]"))?;
    let c = Coord::new(a.first().and_then(Value::as_f64).unwrap_or(f64::NAN), a.get(1).and_then(Value::as_f64).unwrap_or(f64::NAN));
    if c.is_finite() && c.y.abs() <= 90.0 { Ok(c) } else { Err(format!("{k} is [lon, lat]")) }
}

/// A raster a command made, as the layer it adds (its GeoTIFF embedded, so it saves with the
/// project), what the command says of it besides its layer, and what else the command does once
/// it is added: made on any thread, added by [`add_made`].
pub struct Made {
    inputs: Vec<rustgis_data::provenance::Fingerprint>,
    name: String,
    layer: rustgis_map::RasterLayer,
    extra: Option<Value>,
    then: Option<Then>,
    /// Made on another thread: the command's name and the map it was started on.
    ran: Option<(&'static str, u64)>,
    pub(crate) continuation: Option<crate::policy::Continuation>,
    /// The file the work wrote, put in its place as the raster is added: a run cancelled or
    /// abandoned before then leaves the file there as it was.
    staged: Option<(rustgis_data::io::StagedWrite, Vec<String>)>,
}

impl Made {
    /// With `file` to commit as it is added; `said`, what the run had to say, goes with the
    /// error when it can't be (its `then` doesn't run).
    pub(crate) fn committing(mut self, file: rustgis_data::io::StagedWrite, said: Vec<String>) -> Made {
        self.staged = Some((file, said));
        self
    }
}

/// A workflow's work once its inputs are read: it reads nothing more of the session, so it runs on
/// any thread.
pub(crate) type Work = Box<dyn FnOnce() -> Result<Made, String> + Send>;

/// What a command does to the session once its raster is added (a trained model kept), and what it
/// adds to its answer.
pub(crate) type Then = Box<dyn FnOnce(&mut Session, &mut Value) + Send>;

/// A raster embedded as a GeoTIFF, as a layer of its own.
pub(crate) fn made(raster: Raster, extra: Option<Value>) -> Result<Made, String> {
    let bytes = rustgis_data::raster::geotiff::write_all(&raster).map_err(|e| e.to_string())?;
    let name = raster.name.clone();
    let layer = rustgis_map::RasterLayer::from_bytes(raster, format!("{name}.tif"), bytes);
    Ok(made_layer(name, layer, extra, None))
}

/// [`made`], with what is done once the layer is added (`then`).
pub(crate) fn made_then(raster: Raster, extra: Option<Value>, then: Option<Then>) -> Result<Made, String> {
    let mut m = made(raster, extra)?;
    m.then = then;
    Ok(m)
}

/// A layer made (styled, say), to be added as `name`.
pub(crate) fn made_layer(name: String, layer: rustgis_map::RasterLayer, extra: Option<Value>, then: Option<Then>) -> Made {
    Made { name, layer, extra, then, ran: None, continuation: None, inputs: Vec::new(), staged: None }
}

/// Adds a made raster as a layer: the command's answer is the layer's (its id, size and bands) and
/// what the command says besides (`extra`), and what it does then.
pub(crate) fn add_made(s: &mut Session, m: Made) -> CmdResult {
    let Made { name, layer, extra, then, inputs, staged, .. } = m;
    if let Some((file, said)) = staged {
        let path = file.destination().display().to_string();
        file.commit().map_err(|e| if said.is_empty() { format!("{path}: {e}") } else { format!("{path}: {e} ({})", said.join("; ")) })?;
    }
    rustgis_data::provenance::record(inputs);
    let mut v = super::raster::add_raster(s, &name, layer)?;
    if let (Some(Value::Object(more)), Some(o)) = (extra, v.as_object_mut()) {
        o.extend(more);
    }
    if let Some(then) = then {
        then(s, &mut v);
    }
    Ok(v)
}

/// What the remaps of a raster function, a chain or a suitability met: their ranges that match
/// nothing, with the raster each was given for, and the first raster a remap left without a
/// cell with a value.
#[derive(Default)]
struct Remaps {
    empty: Vec<(String, f64, f64)>,
    emptied: Option<String>,
}

impl Remaps {
    /// Their notices, one of each kind, said when the result is added (after a background run
    /// too).
    fn then(self) -> Then {
        let mut notes: Vec<String> = rasterops::empty_ranges_note(&self.empty).into_iter().collect();
        notes.extend(self.emptied.map(|name| format!("{name}: the remap left no cells with values")));
        notices(notes)
    }
}

/// Says `notes` when the result is added.
fn notices(notes: Vec<String>) -> Then {
    Box::new(move |s, _| {
        for note in notes {
            s.notify(note, false);
        }
    })
}

/// What a command that makes its raster on another thread reads of the session, and the work
/// that makes it.
type Start = fn(&Session, &Value) -> Result<Work, String>;

/// The commands that make a raster on another thread from the app ([`raster_start`]), each with
/// what it is called (in its progress, and a cancelled run's error) and what it reads of the
/// session: their work grows with the raster, and most of their passes spread it over the cores
/// from that thread. Run again with `{"ran": id}`, each adds the raster made (#1291 step 3).
pub(crate) const IN_BACKGROUND: &[(&str, &str, Start)] = &[
    ("analysis.viewshed", "Viewshed", viewshed_work),
    ("analysis.interpolate", "Interpolate (IDW)", interpolate_work),
    ("analysis.suitability", "Suitability Modeler", suitability_work),
    ("raster.function", "Raster Functions", function_work),
    ("raster.functionChain", "Function Editor", function_chain_work),
    ("raster.classify", "Classification Wizard", super::imagery::classify_work),
    ("raster.classifySupervised", "Classification Tools", super::imagery::supervised_work),
    ("raster.change", "Change Detection", super::imagery::change_work),
    ("raster.index", "Indices", super::imagery::index_work),
    ("stac.addBands", "Add STAC Bands", super::stac_bands::bands_work),
    ("stac.addIndex", "Add STAC Index", super::stac_bands::index_work),
    ("raster.process", "Process", super::imagery::process_work),
    ("raster.rasterize", "Polygons to Raster", super::imagery::rasterize_work),
    ("imagery.mosaic", "Build Mosaic", super::imagery::mosaic_work),
    ("dl.classify", "Deep Learning", super::deeplearning::classify_work),
    ("embeddings.similarity", "Satellite Embedding Similarity", super::embeddings::similarity_work),
    ("embeddings.change", "Satellite Embedding Change", super::embeddings::change_work),
];

/// `command` and what it is called, when it makes its raster on another thread from the app (see
/// [`raster_start`]).
pub fn runs_in_background(command: &str) -> Option<(&'static str, &'static str)> {
    IN_BACKGROUND.iter().find(|(c, ..)| *c == command).map(|(c, name, _)| (*c, *name))
}

/// `command`'s raster ([`IN_BACKGROUND`]) made here, or the one the app made on a thread
/// ([`raster_start`]) and handed over as `ran`: a raster another command made isn't taken.
pub(crate) fn made_or_ran(s: &mut Session, p: &Value, command: &str) -> CmdResult {
    let &(_, name, work) = IN_BACKGROUND.iter().find(|(c, ..)| *c == command).ok_or_else(|| format!("{command} makes no raster"))?;
    if let Some(ran) = p.get("ran").filter(|v| !v.is_null()) {
        let id = ran.as_u64().ok_or("ran is the number of a run the app worked out")?;
        let at = s.raster_results.iter().position(|(k, _)| *k == id).ok_or("that run is no longer kept: run it again")?;
        if s.raster_results[at].1.ran.is_some_and(|(made_by, _)| made_by != name) {
            return Err(format!("that run made another command's raster, not {name}'s"));
        }
        let c = s.raster_results[at].1.continuation.clone().ok_or("the raster has no initiating authority")?;
        s.continuation_owner(&c)?;
        let (_, m) = s.raster_results.remove(at);
        return s.resume(&c, |s| add_made(s, m))?;
    }
    let owner = s.continuation()?;
    owner.check()?;
    let made = work(s, p)?;
    // Where a person at a terminal can stop it with Ctrl-C: no work cap, as on the app's thread.
    let control = if s.stoppable { rustgis_analysis::control::Control::stoppable() } else { rustgis_analysis::control::Control::new() };
    let made = rustgis_analysis::run_under(name, &control, made)?;
    s.check_continuation(&owner)?;
    add_made(s, made)
}

#[cfg(not(target_arch = "wasm32"))]
/// A command that makes a raster ([`IN_BACKGROUND`]), started to make it on another thread
/// ([`RasterRun::run`]) while the session goes on: its inputs are read now (cheaply: the pixels
/// are shared), and [`raster_land`] hands the raster to the command, which adds it with undo and
/// history when run again with `ran` (#1291 step 3).
pub struct RasterRun {
    pub id: u64,
    /// What the command is called, for a cancelled run's error.
    pub name: &'static str,
    work: Work,
    capture_inputs: bool,
    /// The map it started on, the one its raster is added to.
    map: u64,
    continuation: crate::policy::Continuation,
}

#[cfg(not(target_arch = "wasm32"))]
impl RasterRun {
    /// Makes the raster, blocking until it's made; `control` stops it and counts its progress (see
    /// [`rustgis_analysis::control`]).
    pub fn run(self, control: &std::sync::Arc<rustgis_analysis::control::Control>) -> Result<Made, String> {
        let inputs = self.capture_inputs.then(rustgis_data::provenance::Capture::start);
        let RasterRun { name, work, map, continuation, .. } = self;
        continuation.check()?;
        let current = continuation.clone();
        let still = std::sync::Arc::new(move || current.check().is_ok()) as crate::render::rustgis_tiles::cache::Still;
        // A bug in the work, as one in a tool, must not take the app down.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::render::rustgis_tiles::cache::with_still(still, || rustgis_analysis::run_under(name, control, work))
        }))
        .unwrap_or_else(|_| Err(format!("{name} failed unexpectedly")))
        .map(|m| Made {
            ran: Some((name, map)),
            continuation: Some(continuation),
            inputs: inputs.as_ref().map_or_else(Vec::new, rustgis_data::provenance::Capture::inputs),
            ..m
        })
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Starts `command` (one of [`RasterRun`]'s) with `p` on the active map, as
/// it would run, to make its raster on another thread: its parameters checked and its inputs read
/// now. The run belongs to this project: [`raster_land`] refuses it once another project is open.
pub fn raster_start(s: &mut Session, command: &str, p: &Value) -> Result<RasterRun, String> {
    s.start_raster(command, p)
}

/// Raw preparation requires the engine's admission token and retains its initiating scope.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn raster_start_raw(s: &mut Session, op: crate::policy::Authorized<crate::policy::RasterOp>) -> Result<RasterRun, String> {
    let crate::policy::RasterOp { spec, params } = op.op();
    let (command, p) = (spec.id, params);
    let continuation = s.continuation()?;
    let &(_, name, start) =
        IN_BACKGROUND.iter().find(|(c, ..)| *c == command).ok_or_else(|| format!("{command} doesn't run in the background"))?;
    continuation.check()?;
    let current = continuation.clone();
    let still = std::sync::Arc::new(move || current.check().is_ok());
    let work = crate::render::rustgis_tiles::cache::with_still(still, || start(s, p))?;
    s.next_pending = s.next_pending.wrapping_add(1);
    let id = s.next_pending;
    // One runs at a time from the app; a few are kept, apart from the downloads' list, so no
    // number of downloads meanwhile pushes a run out.
    if s.raster_runs.len() >= 8 {
        s.raster_runs.remove(0);
    }
    s.raster_runs.push((id, continuation.clone()));
    Ok(RasterRun { id, name, work, map: s.map().instance_id(), continuation, capture_inputs: s.log.on() })
}

#[cfg(not(target_arch = "wasm32"))]
/// Whether a run from [`raster_start`] still waits for its raster: opening or creating a project
/// drops the runs of the previous one.
pub fn raster_waiting(s: &Session, id: u64) -> bool {
    s.raster_runs.iter().any(|(n, c)| *n == id && s.check_continuation(c).is_ok())
}

#[cfg(not(target_arch = "wasm32"))]
/// Forgets a run from [`raster_start`] that won't land (it failed, or its thread couldn't start).
pub fn raster_abandon(s: &mut Session, id: u64) {
    s.raster_runs.retain(|(r, _)| *r != id);
}

#[cfg(not(target_arch = "wasm32"))]
/// Hands over a raster [`RasterRun::run`] made, for its command run again with `{"ran": id}`:
/// `Ok(false)` when the run was dropped (another project is open), and an error when another map
/// is active than the one it started on, as for a tool run in the background. A run that failed
/// is forgotten with [`raster_abandon`].
pub fn raster_land(s: &mut Session, id: u64, m: Made) -> Result<bool, String> {
    // A missing run was dropped; a present run with revoked authority or another map must
    // reach its owner/current check and report the refusal, not masquerade as missing.
    if !s.raster_runs.iter().any(|(n, _)| *n == id) {
        return Ok(false);
    }
    let c = m.continuation.as_ref().ok_or("the raster has no initiating authority")?;
    s.check_delivery(c)?;
    let owner = s.raster_runs.iter().find(|(n, _)| *n == id).map(|(_, c)| c).ok_or("no pending raster with that id")?;
    if !owner.same_run(c) {
        return Err("this raster result belongs to another run".into());
    }
    raster_abandon(s, id);
    if let Some((name, map)) = m.ran
        && map != s.map().instance_id()
    {
        return Err(format!("{name} wasn't added: another map was opened while it ran"));
    }
    // The last two are kept (each a full raster and its GeoTIFF).
    if s.raster_results.len() >= 2 {
        s.raster_results.remove(0);
    }
    s.raster_results.push((id, m));
    Ok(true)
}

/// The output's name the parameters give, else `default`; one over 200 characters is refused (#1888).
fn out_name(p: &Value, default: String) -> Result<String, String> {
    Ok(super::name_given(p, "the output's name", 200)?.unwrap_or(default))
}

/// `gp.env`: sets what it's given of the geoprocessing environment (the extent, the projected
/// system rasters are made in, the cell size in that system's units or in degrees), and answers
/// the whole environment with this computer's memory limits. A system and cell size given together
/// are both checked before either is set.
fn environments(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(v) = p.get("extent") {
        let e = match v {
            Value::Null => None,
            Value::String(t) if t == "view" => {
                let m = s.map();
                let (w, h) = s.view_size;
                let e = m.camera.extent(w, h);
                let flat = m.flat();
                let mut ll = Extent::EMPTY;
                for c in
                    [Coord::new(e.min_x, e.min_y), Coord::new(e.max_x, e.max_y), Coord::new(e.min_x, e.max_y), Coord::new(e.max_x, e.min_y)]
                {
                    if let Some(x) = flat.inverse(c) {
                        ll.expand(x);
                    }
                }
                Some(ll)
            }
            Value::String(t) if t == "full" => Some(s.map().full_extent()),
            Value::String(t) => Some(s.map().layer_by_name(t).map(|l| l.extent()).ok_or_else(|| format!("no layer '{t}'"))?),
            Value::Array(a) if a.len() == 4 => {
                let n: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
                match n.as_slice() {
                    [a, b, c, d] if a < c && b < d => Some(Extent::new(*a, *b, *c, *d)),
                    _ => return Err("extent is [minLon, minLat, maxLon, maxLat]".into()),
                }
            }
            _ => return Err("extent is \"view\", \"full\", a layer name, four numbers or null".into()),
        };
        if e.as_ref().is_some_and(Extent::is_empty) {
            return Err("that extent is empty".into());
        }
        s.project.gp_env.extent = e;
    }
    // Both read before either is set, so a bad one changes nothing and says nothing; a cell size
    // given with a system is in its units.
    let mut note = None;
    // A cell size another system had, which a new one doesn't take (in that system's units).
    let mut dropped = None;
    let projected = match p.get("crs") {
        None => s.project.gp_env.projected,
        Some(Value::Null) => None,
        Some(Value::String(t)) if t.trim().is_empty() => None,
        Some(Value::String(t)) => {
            let (crs, compatibility) = rustgis_analysis::projected_crs(t.trim())?;
            note = compatibility;
            // The same system again keeps its cell size; another one starts without one.
            let cell_size = s.project.gp_env.projected.filter(|g| g.crs == crs).and_then(|g| g.cell_size);
            Some(crate::project::ProjectedCells { crs, cell_size })
        }
        Some(_) => return Err("crs is a projected coordinate system (such as \"EPSG:32616\"), or null".into()),
    };
    // Any change of system, to another or to none, drops the old one's cell size.
    if let Some(old) = s.project.gp_env.projected
        && let Some(cell) = old.cell_size
        && projected.is_none_or(|g| g.crs != old.crs)
    {
        dropped = Some((old.crs, cell, projected.is_none()));
    }
    let size = match p.get("cellSize") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(_) => Some(Some(f(p, "cellSize").filter(|c| c.is_finite() && *c > 0.0).ok_or("cellSize is a size above 0")?)),
    };
    if projected.is_none() && size.flatten().is_some_and(|c| c >= 10.0) {
        return Err("cellSize is degrees, above 0 and below 10, without a crs".into());
    }
    if let Some(note) = note {
        s.notify(format!("Environments: its coordinate system: {note}"), false);
    }
    // Said (#2639): always when the system is cleared; on a change of system, unless this call
    // gives the new one its own cell size.
    match (dropped, size) {
        (Some((crs, cell, true)), _) => s.notify(
            format!("Environments: the cell size {cell} in {}'s units was dropped with that system", crs.label()),
            false,
        ),
        // (`cellSize: null` gives the new one none, so it is said too.)
        (Some((crs, cell, false)), None | Some(None)) => s.notify(
            format!(
                "Environments: the cell size {cell} was in {}'s units, so it was dropped with that system: give cellSize for the new one, or each run picks its own",
                crs.label()
            ),
            false,
        ),
        _ => {}
    }
    s.project.gp_env.projected = projected;
    if let Some(size) = size {
        match s.project.gp_env.projected.as_mut() {
            Some(g) => g.cell_size = size,
            None => s.project.gp_env.cell_size = size,
        }
    }
    let env = &s.project.gp_env;
    // The memory limits on this computer (#1291 step 2), so a script or model can see why a run
    // that passes elsewhere is refused here.
    let limits = json!({
        "memory": rustgis_data::memory::physical(),
        "rasterSamples": rustgis_data::raster::max_samples(),
        "outputPixels": rasterops::max_pixels(),
        "pagedFeatures": rustgis_data::ogc::max_paged_features(),
        "densityCells": rustgis_analysis::max_density_cells(),
        "bufferBytes": rustgis_analysis::max_buffer_bytes(),
        "threadScratchBytes": rustgis_analysis::scratch_budget(),
        "undoBytes": s.undo_memory.limit,
        // What the limits follow: the computer, a container's limit, or RUSTGIS_MEMORY_BYTES.
        "memorySource": rustgis_data::memory::source(),
        // What the process holds (layers, tables, rasters, Undo history, copies being made), as of
        // the last command: past half the memory, the limits that follow it plan for less (#2431).
        "heldBytes": rustgis_data::memory::held(),
        // What the computer has free now (null: not known, or RUSTGIS_MEMORY_BYTES is set): the
        // limits plan for no more than this, less what is kept for drawing (#2431).
        "freeBytes": rustgis_data::memory::available(),
        "projectBytes": crate::project::max_bytes(),
        "fileBytes": rustgis_data::io::max_bytes(),
        // The threads one piece of work uses now, and the cores they come from (#1562).
        "threads": rustgis_data::threads::max(),
        "cores": rustgis_data::threads::cores(),
    });
    let projected = env.projected.map(|g| json!({"crs": g.crs.code(), "label": g.crs.label(), "cellSize": g.cell_size}));
    Ok(
        json!({"extent": env.extent.map(|e| [e.min_x, e.min_y, e.max_x, e.max_y]), "cellSize": env.cell_size, "projected": projected, "limits": limits}),
    )
}

/// `gp.threads`: sets the most threads one piece of work uses (#1562), for the whole process
/// (`null`: the default), and says the setting, the threads it gives now, the default's and the
/// cores. The browser runs on one thread, so there it can't be set.
fn threads(_: &mut Session, p: &Value) -> CmdResult {
    use rustgis_data::threads;
    if let Some(v) = p.get("threads") {
        if cfg!(target_arch = "wasm32") {
            return Err("the browser runs everything on one thread".into());
        }
        threads::set_max(threads::parse(v)?)?;
    }
    Ok(json!({"threads": threads::setting(), "max": threads::max(), "default": threads::default_max(), "cores": threads::cores()}))
}

/// Curated tool lists: everyday tools, feature analysis and raster analysis.
fn gallery(_: &mut Session, p: &Value) -> CmdResult {
    gallery_list(ps(p, "kind").unwrap_or("ready"))
}

/// A tool gallery's tools (no session needed, so menus can list them without running a command).
pub fn gallery_list(kind: &str) -> CmdResult {
    let ids: &[&str] = match kind {
        "ready" => &["buffer", "dissolve", "countPoints", "clip", "summaryStatistics", "selectByLocation"],
        "feature" => &[
            "buffer",
            "near",
            "clip",
            "erase",
            "intersect",
            "identity",
            "union",
            "symmetricalDifference",
            "update",
            "split",
            "dissolve",
            "convexHull",
            "featureToPoint",
            "simplify",
            "countPoints",
            "calculateGeometry",
        ],
        "raster" => &[],
        k => return Err(format!("unknown gallery '{k}'")),
    };
    let mut out: Vec<Value> = ids
        .iter()
        .filter_map(|id| rustgis_analysis::resolve_published(id).map(|r| r.spec()))
        .map(|t| json!({"id": t.id, "name": t.name, "summary": t.summary, "command": "gp.run"}))
        .collect();
    if kind == "raster" {
        for (id, name, summary) in [
            (
                "raster.function",
                "Raster Functions",
                "Slope (degrees or percent), aspect, hillshade, NDVI, band math, remap, threshold and resampling.",
            ),
            ("analysis.suitability", "Suitability Modeler", "Weighted overlay of scored rasters."),
            ("analysis.viewshed", "Viewshed", "The cells visible from an observer on a DEM."),
            ("analysis.interpolate", "Interpolate (IDW)", "A raster surface from point values."),
        ] {
            out.push(json!({"id": id, "name": name, "summary": summary, "command": id}));
        }
        out.extend(
            rustgis_analysis::published_tools()
                .filter(|t| t.toolbox.to_lowercase().contains("raster"))
                .take(40)
                .map(|t| json!({"id": t.id, "name": t.name, "summary": t.summary, "command": "gp.run"})),
        );
    }
    Ok(Value::Array(out))
}

/// Tells the user what a graph passed to a command lost or changed when it was read: one notice
/// per kind, each with a count.
fn notify_graph_dropped(s: &mut Session, dropped: crate::model::GraphDropped) {
    if dropped.nodes > 0 {
        s.notify(format!("model graph: {} node(s) left out: unreadable, without an id, or with the id of another", dropped.nodes), false);
    }
    if dropped.edges > 0 {
        s.notify(
            format!(
                "model graph: {} connection(s) left out: unreadable, to a missing node or itself, without an input port, or repeating another",
                dropped.edges
            ),
            false,
        );
    }
    if dropped.ids > 0 {
        s.notify(
            format!(
                "model graph: {} node id(s) changed: trimmed of spaces or cut to {} characters (connections naming the old id follow it)",
                dropped.ids,
                crate::model::MAX_NODE_ID
            ),
            false,
        );
    }
    if dropped.positions > 0 {
        s.notify(
            format!("model graph: {} node position(s) changed: not a number (moved to 0, 0) or out of range (clamped)", dropped.positions),
            false,
        );
    }
    if dropped.params > 0 {
        s.notify(format!("model graph: {} node(s) had parameters that were not an object; they were left out", dropped.params), false);
    }
}

/// Reads the `graph` a command was passed, if any, with what reading it left out or changed. The
/// command reports that once it succeeds, so a command that fails says nothing about it.
fn read_graph(p: &Value) -> Result<(Option<crate::project::ModelGraph>, crate::model::GraphDropped), String> {
    let Some(g) = p.get("graph").filter(|g| !g.is_null()) else { return Ok((None, Default::default())) };
    let (graph, dropped) = crate::model::graph_from_json_counted(g)?;
    Ok((Some(graph), dropped))
}

/// Says what the graph a command was passed (rather than a saved model's) lost when read.
fn note_passed_graph(s: &mut Session, p: &Value) {
    if let Some(Ok((_, dropped))) = p.get("graph").filter(|g| !g.is_null()).map(crate::model::graph_from_json_counted) {
        notify_graph_dropped(s, dropped);
    }
}

/// The most characters a model's name takes.
const MAX_MODEL_NAME: usize = 200;

/// Makes a model from a graph, steps or the run history. It stays on the UI thread (the Analyze
/// pane's "New Model From History" calls it there) because no tool runs here: each history run is
/// read once, so the work is linear in the history.
fn new_model(s: &mut Session, p: &Value) -> CmdResult {
    // A name given past its limit is refused, not cut.
    let base = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Model").to_string();
    let length = base.chars().count();
    if length > MAX_MODEL_NAME {
        return Err(format!("name is {length} characters: a model's name takes at most {MAX_MODEL_NAME}"));
    }
    // A graph, steps or the history is one way to make a model: given two, the other would be
    // dropped without a word, so that is refused.
    let given = |k: &str| p.get(k).is_some_and(|v| !v.is_null());
    let ways = usize::from(given("graph")) + usize::from(given("steps")) + usize::from(b(p, "fromHistory").unwrap_or(false));
    if ways > 1 {
        return Err("give a graph, steps or fromHistory, not more than one of them".into());
    }
    let (graph, dropped) = read_graph(p)?;
    // From the history: the successful runs left out for a tool that isn't available, and the
    // steps that read a layer one of them made.
    let (mut from_history, mut unavailable, mut unmade_at) = (false, 0usize, Vec::<usize>::new());
    // A null steps is none given, as the check for more than one way above takes it.
    let steps: Vec<ModelStep> = match p.get("steps").filter(|v| !v.is_null()) {
        _ if graph.is_some() => graph.as_ref().map(crate::model::graph_to_steps).unwrap_or_default(),
        // Steps given past a model's limit are refused, not cut.
        Some(v) => {
            let steps: Vec<ModelStep> = serde_json::from_value(v.clone()).map_err(|e| format!("steps: {e}"))?;
            if steps.len() > crate::model::MAX_NODES {
                return Err(format!("steps has {} entries: a model takes at most {}", steps.len(), crate::model::MAX_NODES));
            }
            steps
        }
        None if b(p, "fromHistory").unwrap_or(false) => {
            from_history = true;
            // Each successful run becomes a step; inputs naming an earlier run's output become
            // references to it, so the model re-runs as a chain.
            let mut steps: Vec<ModelStep> = Vec::new();
            // Each name holds the latest run that made it, with that run's step when it's replayed:
            // a run left out still made what it made, so a later step isn't wired to an older run's
            // output of the same name.
            let mut made: HashMap<&str, Option<usize>> = HashMap::new();
            for r in s.project.gp_history.iter().filter(|r| r.ok) {
                let Some(tool) = super::gp::tool_id(&r.tool) else {
                    unavailable += 1;
                    for name in r.outputs.iter() {
                        made.insert(name.as_str(), None);
                    }
                    continue;
                };
                let mut params = r.params.clone();
                // Only layer parameters can name an earlier output.
                let spec = rustgis_analysis::resolve_published(tool).map(|r| r.spec());
                let is_layer = |k: &str| {
                    spec.and_then(|t| t.params.iter().find(|p| p.name == k)).is_some_and(|p| {
                        matches!(
                            p.kind,
                            rustgis_analysis::ParamKind::Layer
                                | rustgis_analysis::ParamKind::PolygonLayer
                                | rustgis_analysis::ParamKind::PointLayer
                                | rustgis_analysis::ParamKind::RasterLayer
                                | rustgis_analysis::ParamKind::VectorLayer
                        )
                    })
                };
                let mut reads_unmade = false;
                if let Value::Object(o) = &mut params {
                    for (key, v) in o.iter_mut() {
                        if let Some(t) = v.as_str().filter(|_| is_layer(key))
                            // The latest run that made that name (names can be reused).
                            && let Some(&made_by) = made.get(t)
                        {
                            match made_by {
                                Some(k) => *v = json!(format!("%{}", k + 1)),
                                // Made by a run left out: the step reads the layer of that name,
                                // as the project has it, and that is said.
                                None => reads_unmade = true,
                            }
                        }
                    }
                }
                if reads_unmade {
                    unmade_at.push(steps.len());
                }
                for name in r.outputs.iter() {
                    made.insert(name.as_str(), Some(steps.len()));
                }
                steps.push(ModelStep { tool: tool.to_string(), params });
            }
            steps
        }
        None => Vec::new(),
    };
    // A history longer than a model takes keeps its first runs; that is said below. A graph's
    // steps are within the limit already (read_graph refuses a larger graph).
    let replayable = steps.len();
    let steps: Vec<ModelStep> = steps.into_iter().take(crate::model::MAX_NODES).collect();
    // A graph may name tools not installed here (its problems say so); steps may not.
    if graph.is_none() {
        for st in &steps {
            if super::gp::tool_id(&st.tool).is_none() {
                return Err(format!("unknown tool '{}'", st.tool));
            }
        }
    }
    // Steps stay steps (ModelBuilder shows them as a graph, and saves one).
    let mut name = base.clone();
    let mut i = 1;
    while s.project.models.iter().any(|m| m.name.eq_ignore_ascii_case(&name)) {
        i += 1;
        // The suffix that tells it apart fits within the name's limit: the name given gives way
        // to it, and that is said. A cut that lands on a space keeps no space before the suffix.
        let suffix = format!(" {i}");
        let head: String = base.chars().take(MAX_MODEL_NAME - suffix.chars().count()).collect();
        name = format!("{}{suffix}", head.trim_end());
    }
    let n = steps.len();
    // Of the steps the model keeps.
    let unmade = unmade_at.iter().filter(|&&i| i < n).count();
    s.project.models.push(Model { name: name.clone(), steps, graph });
    notify_graph_dropped(s, dropped);
    let kept = name.chars().count() - if i > 1 { format!(" {i}").chars().count() } else { 0 };
    // Said here, and in the result (`"cut"`), so a caller doesn't say it again.
    let cut = kept < length;
    if cut {
        s.notify(
            format!("ModelBuilder: a model has that name already, so the name given is cut to {kept} characters to fit \" {i}\""),
            false,
        );
    }
    // From the history: what it can't replay, and where it cuts, each said with its count.
    if unavailable > 0 {
        let said = if unavailable == 1 {
            "1 successful run in the history uses a tool that isn't available, so it's left out".to_string()
        } else {
            format!("{unavailable} successful runs in the history use a tool that isn't available, so they're left out")
        };
        s.notify(format!("{name}: {said}"), false);
    }
    if unmade > 0 {
        s.notify(
            format!("{name}: {unmade} step(s) read a layer made by a run that's left out, so they read the project's layer of that name"),
            false,
        );
    }
    if from_history && replayable > n {
        s.notify(
            format!("{name}: the model takes the first {n} of the {replayable} successful runs it can replay from the history"),
            false,
        );
    }
    let mut answer = json!({"model": name, "steps": n});
    if cut {
        answer["cut"] = json!(true);
    }
    Ok(answer)
}

/// The model a command names (or the last one), or the graph it passes: its name, graph and
/// whether it may touch files (a graph passed in is the caller's own).
pub(crate) fn model_graph(s: &Session, p: &Value) -> Result<(String, crate::project::ModelGraph, bool), String> {
    if let Some(g) = p.get("graph").filter(|g| !g.is_null()) {
        let name = super::within("name", "a model's name", ps(p, "name").unwrap_or("Model"), MAX_MODEL_NAME)?;
        return Ok((name, crate::model::graph_from_json(g)?, true));
    }
    let model = match ps(p, "name") {
        Some(n) => s.project.models.iter().find(|m| m.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no model '{n}'"))?,
        None => s.project.models.last().ok_or("the project has no models")?,
    };
    let g = match &model.graph {
        Some(graph) => graph.clone(),
        None => crate::model::checked_steps_to_graph(&model.steps)?,
    };
    Ok((model.name.clone(), g, s.item_trusted("model", &model.name)))
}

/// Runs a model's graph, or the graph passed: taking the tools the app ran ahead
/// ([`ahead_on_copy`]) whose inputs are still the same.
fn run_model(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(ran) = super::reads::take::<super::gp::RanAheads>(s, MODEL_AHEAD) {
        return super::gp::taking(s, ran, |s| run_model_here(s, p));
    }
    run_model_here(s, p)
}

#[cfg(not(target_arch = "wasm32"))]
/// The key of a model's tools run ahead ([`ahead_on_copy`]).
const MODEL_AHEAD: &str = "model.run";

#[cfg(not(target_arch = "wasm32"))]
/// The key of a script's tools run ahead ([`ahead_on_copy`]).
const SCRIPT_AHEAD: &str = "script.run";

#[cfg(not(target_arch = "wasm32"))]
/// `command` (`model.run` or `script.run`) run on a copy of the session, made on a thread of its
/// own: it records what each tool it runs read and made, for the command to take instead of
/// running them again, so what the run adds, says, logs and puts in Undo is the same as when it
/// runs here. It stops at a tool that writes files, a plugin's or GeoLibre's, and at a script's
/// line that may reach outside the session ([`runs_on_a_copy`]): from there the run here runs
/// them as it always has. What it says and how it ends are the run here's to say.
pub(crate) fn ahead_on_copy(s: &Session, command: &'static str, p: &Value) -> super::reads::ReadWork {
    let name = if command == SCRIPT_AHEAD { "Run Script" } else { "Run Model" };
    let (project, trusted) = (s.project.clone(), s.trusted_models.clone());
    let readers = s.undo_memory.readers();
    let retained = readers.retain(crate::undo_memory::allocations(&project));
    // The copy follows the extensions turned on or off here (#1752).
    let extensions = s.extensions().clone();
    let (view_size, active_layer, opened, p) = (s.view_size, s.active_layer, s.opened, p.clone());
    let selection_intent = s.selection_intent;
    let (selection_sequence, similar_selected) = (s.selection_sequence.clone(), s.similar_selected);
    // Reserve before the worker starts: its inner reads belong to this admission, even when
    // a newer UI request arrives while this script waits in the queue.
    let selection_admission = s.selection_admission.clone().unwrap_or_else(|| (s.selection_epoch(), Default::default()));
    // What its tools ask of servers and files goes in this session's Diagnostics, as when they
    // run here: the run here takes their results and asks nothing.
    let requests = s.diagnostics.capture();
    super::reads::ahead(name, command.into(), move || {
        let mut copy = Session::new(project);
        copy.requests_into = Some(requests);
        copy.trusted_models = trusted;
        copy.set_extensions(extensions);
        copy.view_size = view_size;
        copy.active_layer = active_layer;
        copy.opened = opened;
        copy.selection_intent = selection_intent;
        copy.selection_sequence = selection_sequence;
        copy.selection_admission = Some(selection_admission);
        copy.similar_selected = similar_selected;
        // The copy shares this session's rows, which this session already counts in what the
        // process holds (#2431): it neither measures nor adds them again.
        copy.undo_memory.memory = None;
        copy.undo_memory.use_readers(readers);
        copy.sync_policy_generation();
        copy.tools_ahead = Some(super::gp::Ahead::Recording(Vec::new()));
        copy.files_ahead = Some(super::gp::FilesAhead::Recording(Vec::new()));
        copy.grids_ahead = Some(crate::image_grid::Ahead::Recording(Vec::new()));
        copy.zones_ahead = Some(super::imagery::ZonesAhead::Recording(Vec::new()));
        let _ = copy.run(command, &p);
        let mut ran = super::gp::ran_ahead(&mut copy);
        ran.retained = Some(retained);
        ran
    })
}

#[cfg(not(target_arch = "wasm32"))]
/// Prepares a native script's replayable prefix. Grid queries and table writes must fit within it;
/// otherwise refuse before any line runs, so neither header reads nor table edits reach the UI thread.
/// Restricted callers keep their existing authority and dispatch those operations individually.
pub(crate) fn script_ahead(s: &Session, p: &Value, native: bool) -> Result<Option<super::reads::ReadWork>, String> {
    let Some(script) = ps(p, "script") else { return Ok(None) };
    let Ok(lines) = script_commands(script) else { return Ok(None) };
    let table = |id: &str, p: &Value| super::edit::table_edit_in_background(id).is_some() && p.get("table").is_some_and(|t| !t.is_null());
    if lines.iter().any(|(_, id, p)| table(id, p) || id == "raster.imageGrid") {
        if !native {
            return Err(
                "this script cannot prepare queries or table writes under its current permissions: run those commands directly".into()
            );
        }
        check_prepared_prefix(s, &lines)?;
    }
    let Some((_, id, params)) = lines.first() else { return Ok(None) };
    Ok((native && runs_on_a_copy(s, id, params)).then(|| ahead_on_copy(s, SCRIPT_AHEAD, p)))
}

#[cfg(not(target_arch = "wasm32"))]
/// Refuses an unrecordable prefix before live lines can precede a grid query or table write. Inspect
/// model tool metadata too: their command id alone does not reveal file-producing or plugin steps.
/// Recheck at landing, since a named model may have changed while the script worker ran.
fn check_prepared_prefix(s: &Session, lines: &[(usize, String, Value)]) -> Result<(), String> {
    let Some(last) = lines.iter().rposition(|(_, id, p)| {
        id == "raster.imageGrid" || (super::edit::table_edit_in_background(id).is_some() && p.get("table").is_some_and(|t| !t.is_null()))
    }) else {
        return Ok(());
    };
    let recordable_tool = |id: &str| {
        rustgis_analysis::resolve_published(id)
            .is_some_and(|tool| tool.source() != Some(rustgis_plugins::tools::SOURCE) && !super::gp::writes_files(tool.spec()))
    };
    for (n, id, p) in lines.iter().take(last + 1) {
        let nested = match id.as_str() {
            "model.run" => model_graph(s, p).is_ok_and(|(_, graph, _)| {
                graph
                    .nodes
                    .iter()
                    .filter(|node| node.kind == crate::project::ModelNodeKind::Tool)
                    .all(|node| node.tool.as_deref().is_some_and(recordable_tool))
            }),
            "gp.batch" => ps(p, "tool").is_some_and(recordable_tool),
            _ => true,
        };
        if !nested || !runs_on_a_copy(s, id, p) {
            return Err(format!(
                "line {n} ({id}) cannot be prepared before the queries or table writes: run the setup and prepared commands as separate scripts"
            ));
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
/// The commands a script's line may run on a copy of the session ([`runs_on_a_copy`]): those
/// whose results are taken (a tool, a model, a batch) and those that change only what the taking compares
/// by value (a selection, a layer's name or query, topology rules, the environment, the projection).
/// A line that makes rows of its own (a layer edit, a layer from a selection) isn't one. Table edits are the
/// exception: their rows are recorded and taken, so later tools retain their shared input.
pub(crate) const ON_A_COPY: &[&str] = &[
    "project.coordinateSystem",
    "edit.enable",
    "edit.setValue",
    "edit.addFeature",
    "edit.addField",
    "edit.deleteField",
    "edit.convertField",
    "edit.calculateField",
    "model.run",
    "gp.batch",
    "gp.run",
    "gp.spatialJoin",
    "gp.zonalStats",
    "gp.env",
    "data.similar",
    "topology.rules",
    "topology.validate",
    "select.all",
    "select.atPoint",
    "select.byAttributes",
    "select.byPolygon",
    "select.byRadius",
    "select.byRect",
    "select.clear",
    "select.rows",
    "select.switch",
    "layer.remove",
    "layer.rename",
    "layer.definitionQuery",
    "layer.select",
    "map.projection",
    "raster.spectral",
];

#[cfg(not(target_arch = "wasm32"))]
/// Whether a script's line runs on a copy of the session ([`ahead_on_copy`]): one of
/// [`ON_A_COPY`], naming no file: run twice, none changes anything outside the session. Any
/// other line stops the copy there, Add Data and Load All among them (they would read their
/// files and servers twice).
fn runs_on_a_copy(s: &Session, id: &str, params: &Value) -> bool {
    if id == "raster.imageGrid" {
        return true; // Header results are recorded on the worker and consumed once at landing.
    }
    if super::edit::table_edit_in_background(id).is_some() {
        return params.get("table").is_some_and(|t| !t.is_null()) && params.get("layer").is_none_or(Value::is_null);
    }
    // A model's own tools are checked as they run, and so are a batch's: a batch writes no file
    // (it runs tools that add layers), and the files it reads are read once, on the copy, for the
    // run here to take ([`super::gp::FilesAhead`]). Any other line by what its parameters name.
    ON_A_COPY.contains(&id) && (matches!(id, "model.run" | "gp.batch") || !super::items::touches_files(s, id, params))
}

fn run_model_here(s: &mut Session, p: &Value) -> CmdResult {
    // A model saved as steps runs as steps: `%n` may stand anywhere in them, and inputs may be files.
    if p.get("graph").is_none_or(Value::is_null) {
        let model = match ps(p, "name") {
            Some(n) => s.project.models.iter().find(|m| m.name.eq_ignore_ascii_case(n)),
            None => s.project.models.last(),
        };
        if let Some(m) = model.filter(|m| m.graph.is_none()).cloned() {
            let trusted = s.item_trusted("model", &m.name);
            if trusted {
                return run_steps(s, m);
            }
            let name = m.name.clone();
            return s.narrowed(crate::policy::Narrow::Item { kind: "model", name }, |s| run_steps(s, m))?;
        }
    }
    let (name, _, trusted) = model_graph(s, p)?;
    let op = s.authorize_model(p)?;
    let out = if trusted || p.get("graph").is_some_and(|g| !g.is_null()) {
        crate::model::run(s, op)?
    } else {
        s.narrowed(crate::policy::Narrow::Item { kind: "model", name }, |s| crate::model::run(s, op))??
    };
    // Said once the run went ahead, not for one that was refused.
    note_passed_graph(s, p);
    Ok(out)
}

/// The stored source and destination names used by graph saving and its provenance check. A name
/// over its limit is refused, not cut.
pub(crate) fn model_save_names(p: &Value) -> Result<(String, String), String> {
    let name = ps(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or("name is required")?;
    let name = super::within("name", "a model's name", name, MAX_MODEL_NAME)?;
    let new_name = match ps(p, "rename").map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => super::within("rename", "a model's name", n, MAX_MODEL_NAME)?,
        None => name.clone(),
    };
    Ok((name, new_name))
}

/// Saves a graph as a model, replacing the one of that name (or adding it).
fn save_model(s: &mut Session, p: &Value) -> CmdResult {
    let (name, new_name) = model_save_names(p)?;
    let (g, dropped) = read_graph(p)?;
    let g = g.ok_or("graph is required")?;
    let i = s.project.models.iter().position(|m| m.name.eq_ignore_ascii_case(&name));
    if !new_name.eq_ignore_ascii_case(&name) && s.project.models.iter().any(|m| m.name.eq_ignore_ascii_case(&new_name)) {
        return Err(format!("a model named '{new_name}' already exists"));
    }
    let model = Model { name: new_name.clone(), steps: crate::model::graph_to_steps(&g), graph: Some(g) };
    // A model that came in a file stays one when saved over (or renamed): its tools still may not
    // touch files. So does a graph saved as `imported`.
    match i.and_then(|i| s.project.models.get_mut(i)) {
        Some(m) => *m = model,
        None => s.project.models.push(model),
    }
    s.trusted_models.remove(&name.to_ascii_lowercase());
    notify_graph_dropped(s, dropped);
    Ok(json!({"model": new_name}))
}

/// A model as a `*.model.json` file: written to `path`, or its text returned.
fn export_model(s: &mut Session, p: &Value) -> CmdResult {
    let written = s.notice_mark();
    let (name, g, _) = model_graph(s, p)?;
    let (text, report) =
        crate::durable::output_owned(g, |g| serde_json::to_string_pretty(&crate::model::to_file(&name, g)).map_err(|e| e.to_string()))?;
    crate::durable::notify(s, &report);
    let file: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    match ps(p, "path") {
        Some(path) => {
            rustgis_data::io::write_atomic(std::path::Path::new(path), text.as_bytes()).map_err(|e| format!("{path}: {e}"))?;
            note_passed_graph(s, p);
            s.retain_written_notices(written);
            Ok(json!({"model": name, "path": path}))
        }
        None => {
            note_passed_graph(s, p);
            Ok(json!({"model": name, "fileName": format!("{file}.model.json"), "text": text}))
        }
    }
}

/// Runs a model saved as steps (no graph) in order, putting each step's output layer name in place
/// of `%n` anywhere in the parameters, as models always ran before ModelBuilder drew graphs.
fn run_steps(s: &mut Session, model: Model) -> CmdResult {
    if model.steps.is_empty() {
        return Err(format!("{} has no steps", model.name));
    }
    // A model that came in a file (an opened project, or an imported tool) may not touch files.
    // Each step is checked as the `gp.run` it is, tool and all, as ModelBuilder's graphs are.
    if !s.item_trusted("model", &model.name)
        && let Some((i, st)) = model
            .steps
            .iter()
            .enumerate()
            .find(|(_, st)| super::items::touches_files(s, "gp.run", &json!({"tool": st.tool, "params": st.params})))
    {
        return Err(format!(
            "step {} ({}) reads or writes files, uses the network or runs other software, which a model from a file can't do; make a new model with that step to run it",
            i + 1,
            st.tool
        ));
    }
    // The parameters the steps ignore are said once for the run, as a graph's run says them
    // (#2307). A failed run's error carries them, as its notices go back with its changes.
    let mut ignored = Vec::new();
    let res = run_step_list(s, &model, &mut ignored);
    let said = crate::model::ignored_summary(&ignored);
    match res {
        Err(e) if !said.is_empty() => Err(format!("{e}; {said}")),
        res => {
            if !said.is_empty() {
                s.warn(said);
            }
            res
        }
    }
}

/// Runs each of a model's steps in order ([`run_steps`]), collecting into `ignored` what each
/// step's tool ignores ([`super::gp::ignored_params`]).
fn run_step_list(s: &mut Session, model: &Model, ignored: &mut Vec<String>) -> CmdResult {
    let mut outputs: Vec<String> = Vec::new();
    let mut ran = Vec::new();
    for (i, st) in model.steps.iter().enumerate() {
        let mut params = st.params.clone();
        wire(&mut params, &outputs, i, 0)?;
        let before = s.project.gp_history.len();
        let r = super::gp::run_step(s, &st.tool, &params).map_err(|e| format!("step {} ({}): {e}", i + 1, st.tool))?;
        // Said once for the run; a failing step's error names its own keys already.
        if let Some(said) = rustgis_analysis::resolve_published(&st.tool).and_then(|r| super::gp::ignored_params(r.spec(), &params)) {
            ignored.push(said);
        }
        // A step that made no layer leaves nothing for a later %n to use.
        let out =
            (s.project.gp_history.len() > before).then(|| s.project.gp_history.last().and_then(|h| h.outputs.first().cloned())).flatten();
        outputs.push(out.unwrap_or_default());
        ran.push(json!({"step": i + 1, "tool": st.tool, "result": r}));
    }
    Ok(json!({"model": model.name, "ran": ran}))
}

/// Replaces `%n` (step n's output layer) anywhere in a step's parameters, nested values too.
fn wire(v: &mut Value, outputs: &[String], step: usize, depth: usize) -> Result<(), String> {
    match v {
        Value::String(t) => {
            if let Some(n) = t.strip_prefix('%').and_then(|n| n.parse::<usize>().ok()) {
                let out = outputs.get(n.wrapping_sub(1)).filter(|o| !o.is_empty());
                *v = json!(out.ok_or_else(|| format!("step {} uses %{n}, but step {n} made no output layer", step + 1))?);
            }
        }
        Value::Array(a) if depth < 8 => a.iter_mut().try_for_each(|x| wire(x, outputs, step, depth + 1))?,
        Value::Object(o) if depth < 8 => o.values_mut().try_for_each(|x| wire(x, outputs, step, depth + 1))?,
        _ => {}
    }
    Ok(())
}

/// Runs a script of commands, one per line (`id {json}`), stopping at the first error. Scripts
/// typed in the Script pane or passed on the command line are the user's own, so they may use
/// any command, files included; notebooks vet cells that came from files before running them.
/// The whole script is one undo step, none when no line changed what undo restores
/// ([`Session::shared_step`]): its lines only read the project, moved the view, wrote files
/// elsewhere, left a request, or said they changed nothing.
pub(crate) fn run_script(s: &mut Session, p: &Value) -> CmdResult {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(ran) = super::reads::take::<super::gp::RanAheads>(s, SCRIPT_AHEAD) {
        return super::gp::taking(s, ran, |s| run_script_here(s, p, true));
    }
    run_script_here(s, p, false)
}

/// Runs a script's lines in order ([`run_script`]); a prepared desktop run requires every table
/// write to consume its recorded worker result under the live line's ordinary authorization.
fn run_script_here(s: &mut Session, p: &Value, prepared: bool) -> CmdResult {
    let _ = prepared;
    let script = ps(p, "script").ok_or("script is required")?;
    let lines = script_commands(script)?;
    #[cfg(not(target_arch = "wasm32"))]
    if prepared {
        check_prepared_prefix(s, &lines)?;
    }
    let mut out = Vec::new();
    for (n, id, params) in lines {
        let spec = *s.registry.get(&id).ok_or_else(|| format!("line {n}: unknown command '{id}'"))?;
        let (mutates, label) = (spec.mutates, spec.label);
        // On a copy of the session ([`ahead_on_copy`]): from a line that may reach outside it,
        // the run here runs the rest.
        #[cfg(not(target_arch = "wasm32"))]
        if matches!(s.tools_ahead, Some(super::gp::Ahead::Recording(_))) && !runs_on_a_copy(s, &id, &params) {
            return Err(super::gp::NOT_AHEAD.into());
        }
        // As when typed directly, a line naming a layer is checked against that layer.
        if params.get("layer").is_some()
            && let Ok(lid) = s.layer_id(&params)
        {
            s.active_layer = Some(lid);
        }
        // And a line naming a standalone table, against that table.
        let (blocked, table) = s.check_conditions(&spec, &params);
        if let Some(why) = blocked.unwrap_or_else(|panic| std::panic::resume_unwind(panic)) {
            return Err(format!("line {n}: {id}: {why}"));
        }
        // As Session::run does: an edit to a layer read in place would be lost at its next read
        // (an edit of a table edits no layer).
        if let Some(e) = s.streamed_edit(&id, mutates, label, if table.is_some() { None } else { s.active_layer }) {
            return Err(format!("line {n}: {e}"));
        }
        let op = s.authorize(&id, &params).map_err(|e| format!("line {n} ({id}): {e}"))?;
        let invoke = |s: &mut Session| steps_of(s, |s| s.invoke(op, crate::policy::Txn::Shared));
        #[cfg(not(target_arch = "wasm32"))]
        let r = if super::edit::table_edit_in_background(&id).is_some() && params.get("table").is_some_and(|t| !t.is_null()) {
            super::edit::script_table(s, &id, &params, prepared, invoke)
        } else {
            invoke(s)
        };
        #[cfg(target_arch = "wasm32")]
        let r = invoke(s);
        let r = r.map_err(|e| format!("line {n}: {id}: {e}"))?;
        out.push(json!({"line": n, "command": id, "result": r}));
    }
    // An undo step only when a line edited ([`Session::shared_step`]); run inside a task, the
    // task's account, earlier steps included, decides.
    s.request_only = !s.shared_steps.edited;
    Ok(json!({"ran": out}))
}

/// `run` as a step of the command running now ([`Session::steps_running`] counts it, on every
/// path, a panic included, which is raised again).
pub(crate) fn steps_of<T>(s: &mut Session, run: impl FnOnce(&mut Session) -> T) -> T {
    s.steps_running += 1;
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(s)));
    s.steps_running = s.steps_running.saturating_sub(1);
    r.unwrap_or_else(|p| std::panic::resume_unwind(p))
}

/// Maximum script lines: execution refuses longer input before any command runs.
/// The Script pane colors at most this many lines.
pub const MAX_SCRIPT_LINES: usize = 10_000;

/// Whether command `id` can't run in a script: scripts and notebooks (which would run scripts), and
/// the interface's `ui.*` commands. The Script pane marks such a line with the same rule.
pub fn refused_in_script(id: &str) -> bool {
    id.starts_with("script.") || id.starts_with("ui.") || id == "notebook.run"
}

/// A script's commands, as `(line, id, params)` with 1-based lines: one `id {json}` per line,
/// blank lines and `#` comments skipped. A line that isn't JSON, or a command that can't run in
/// a script (scripts, notebooks, `ui.*`), is an error naming its line.
pub fn script_commands(script: &str) -> Result<Vec<(usize, String, Value)>, String> {
    let mut out = Vec::new();
    for (n, line) in script.lines().enumerate().take(MAX_SCRIPT_LINES + 1) {
        if n == MAX_SCRIPT_LINES {
            return Err(format!("the script has more than {MAX_SCRIPT_LINES} lines; split it before running"));
        }
        if let Some(parsed) = crate::script_line(line) {
            let (id, params) = parsed.map_err(|e| format!("line {}: {e}", n + 1))?;
            out.push((n + 1, id, params));
        }
    }
    Ok(out)
}

/// One raster function's output pixels on `r`'s grid, or a resample's on `target`'s (from
/// [`resample_target`]). A remap adds its ranges that match nothing to `remaps`, and whether it
/// left any cell with a value.
fn apply_function(r: &Raster, p: &Value, target: Option<&Raster>, remaps: &mut Remaps) -> Result<Vec<f32>, String> {
    let band = band_of(p, r)?;
    let zf = f(p, "zFactor").unwrap_or(1.0).clamp(0.0001, 100_000.0);
    match ps(p, "function").ok_or("function is required")? {
        "slope" => match p.get("units") {
            None => rasterops::slope(r, band, zf),
            Some(Value::String(units)) if units == "degrees" => rasterops::slope(r, band, zf),
            Some(Value::String(units)) if units == "percent" => rasterops::slope_percent(r, band, zf),
            _ => Err("slope units must be degrees or percent".into()),
        },
        "aspect" => rasterops::aspect(r, band),
        "hillshade" => {
            rasterops::hillshade(r, band, f(p, "azimuth").unwrap_or(315.0), f(p, "altitude").unwrap_or(45.0).clamp(0.0, 90.0), zf)
        }
        "ndvi" => {
            let (nir, red) = (p.get("nir").and_then(Value::as_u64).unwrap_or(4), p.get("red").and_then(Value::as_u64).unwrap_or(3));
            rasterops::band_math(r, &format!("(b{nir} - b{red}) / (b{nir} + b{red})"))
        }
        "bandMath" => rasterops::band_math(r, ps(p, "expression").ok_or("expression is required (b1, b2… with + - * /)")?),
        "remap" | "threshold" => {
            let ranges: Vec<(f64, f64, f64)> = if ps(p, "function") == Some("threshold") {
                let v = f(p, "value").ok_or("value is required")?;
                vec![(f64::NEG_INFINITY, v, 0.0), (v, f64::INFINITY, 1.0)]
            } else {
                p.get("ranges")
                    .and_then(Value::as_array)
                    .ok_or("ranges is [[lo, hi, value], …]")?
                    .iter()
                    .take(1000)
                    .map(|x| {
                        let n: Vec<f64> = x.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
                        match n.as_slice() {
                            [lo, hi, v] => Ok((*lo, *hi, *v)),
                            _ => Err("each range is [lo, hi, value]".to_string()),
                        }
                    })
                    .collect::<Result<_, _>>()?
            };
            let remap = rasterops::remap(r, band, &ranges, b(p, "keep").unwrap_or(false))?;
            remaps.empty.extend(remap.empty.into_iter().map(|(lo, hi)| (r.name.clone(), lo, hi)));
            if !rasterops::has_values(&remap.cells, r.width as usize)? {
                remaps.emptied.get_or_insert_with(|| r.name.clone());
            }
            Ok(remap.cells)
        }
        "resample" => rasterops::resample_to(r, band, target.ok_or("target (a raster to match) is required")?),
        other => Err(format!("unknown raster function '{other}'")),
    }
}

/// The raster a resample step (`p`) takes the grid of: none for another function.
fn resample_target(s: &Session, p: &Value) -> Result<Option<std::sync::Arc<Raster>>, String> {
    if ps(p, "function") != Some("resample") {
        return Ok(None);
    }
    let t = raster_of(s, resample_target_name(p)?.ok_or("target (a raster to match) is required")?)?;
    if t.width as usize * t.height as usize > rasterops::max_pixels() {
        return Err(format!("the target grid is larger than this computer's {} cells", rasterops::max_pixels()));
    }
    Ok(Some(t))
}

fn raster_function(s: &mut Session, p: &Value) -> CmdResult {
    made_or_ran(s, p, "raster.function")
}

/// What a raster function reads of the session, and the work that applies it.
fn function_work(s: &Session, p: &Value) -> Result<Work, String> {
    let src = raster_of(s, ps(p, "layer").ok_or("layer is required")?)?;
    let target = resample_target(s, p)?;
    let name = out_name(p, format!("{}_{}", src.name, ps(p, "function").unwrap_or("")))?;
    let defaults = s.raster_defaults.clone();
    let mut p = p.clone();
    // Read here, on the command's thread, where an unreadable value is said (#1600); only a remap
    // or threshold reads it.
    if matches!(ps(&p, "function"), Some("remap" | "threshold")) {
        super::settle_bool(&mut p, "keep");
    }
    Ok(Box::new(move || {
        let mut remaps = Remaps::default();
        let pixels = apply_function(&src, &p, target.as_deref(), &mut remaps)?;
        // Only a resample takes another raster's grid.
        let raster = derive_function(target.as_deref().unwrap_or(&src), &name, &p, pixels)?;
        if is_hillshade(&p) { made_hillshade(raster, &defaults) } else { made_then(raster, None, Some(remaps.then())) }
    }))
}

/// Wraps a function's pixels on its output grid, keeping a slope's unit in its band description
/// and a hillshade's byte storage. A later function starts with its own description.
fn derive_function(r: &Raster, name: &str, p: &Value, pixels: Vec<f32>) -> Result<Raster, String> {
    if is_hillshade(p) {
        return rasterops::derive_hillshade(r, name, pixels);
    }
    let mut out = rasterops::derive(r, name, vec![pixels])?;
    if ps(p, "function") == Some("slope")
        && let Some(band) = out.bands.first_mut()
    {
        band.name = if ps(p, "units") == Some("percent") { "Slope (percent)" } else { "Slope (degrees)" }.into();
    }
    Ok(out)
}

/// Whether raster function step `p` is a hillshade, whose pixels are bytes
/// ([`rasterops::derive_hillshade`]).
fn is_hillshade(p: &Value) -> bool {
    ps(p, "function") == Some("hillshade")
}

/// [`made`] for a hillshade, drawn as a stretch on the session's raster `defaults` however few
/// shades it holds: as whole numbers, 16 or fewer (a gentle slope) would otherwise be drawn as
/// classes (Unique Values).
fn made_hillshade(raster: Raster, defaults: &rustgis_map::RasterDefaults) -> Result<Made, String> {
    let mut m = made(raster, None)?;
    let style = &mut m.layer.style;
    style.renderer = rustgis_map::RasterRenderer::Stretch { band: 0, ramp: defaults.ramp.clone(), invert: defaults.invert };
    style.stretch = defaults.stretch;
    style.resampling = defaults.resampling;
    Ok(m)
}

/// Runs raster functions one after another, each on the previous one's output.
fn function_chain(s: &mut Session, p: &Value) -> CmdResult {
    made_or_ran(s, p, "raster.functionChain")
}

/// What a chain of raster functions reads of the session (its raster, and each resample's
/// target), and the work that applies them.
fn function_chain_work(s: &Session, p: &Value) -> Result<Work, String> {
    let src = raster_of(s, ps(p, "layer").ok_or("layer is required")?)?;
    let mut steps = p.get("steps").and_then(Value::as_array).ok_or("steps is required")?.clone();
    if steps.is_empty() || steps.len() > 20 {
        return Err("give 1 to 20 steps".into());
    }
    // Read here, on the command's thread, where an unreadable value is said (#1600).
    for st in steps.iter_mut().filter(|st| matches!(ps(st, "function"), Some("remap" | "threshold"))) {
        super::settle_bool(st, "keep");
    }
    let func = |st: &Value| st.get("function").and_then(Value::as_str).unwrap_or("?").to_string();
    let targets: Vec<_> = steps
        .iter()
        .enumerate()
        .map(|(i, st)| resample_target(s, st).map_err(|e| format!("step {} ({}): {e}", i + 1, func(st))))
        .collect::<Result<_, _>>()?;
    let named = Some(out_name(p, String::new())?).filter(|n| !n.is_empty());
    let defaults = s.raster_defaults.clone();
    Ok(Box::new(move || {
        let mut cur: Raster = (*src).clone();
        let mut names = Vec::new();
        let mut remaps = Remaps::default();
        for (i, (st, target)) in steps.iter().zip(&targets).enumerate() {
            let f = func(st);
            let pixels = apply_function(&cur, st, target.as_deref(), &mut remaps).map_err(|e| format!("step {} ({f}): {e}", i + 1))?;
            // Only a resample takes another raster's grid; otherwise the grid is the current one.
            cur = derive_function(target.as_deref().unwrap_or(&cur), &cur.name, st, pixels)?;
            names.push(f);
        }
        cur.name = named.unwrap_or_else(|| format!("{}_{}", cur.name, names.join("_")));
        // A chain that ends in a hillshade is drawn as one.
        let mut m = match steps.last() {
            Some(last) if is_hillshade(last) => made_hillshade(cur, &defaults)?,
            _ => made(cur, None)?,
        };
        m.then = Some(remaps.then());
        Ok(m)
    }))
}

fn suitability(s: &mut Session, p: &Value) -> CmdResult {
    made_or_ran(s, p, "analysis.suitability")
}

/// What a weighted overlay reads of the session (each criterion's raster, band, ranges and
/// weight), and the work that scores and weighs them.
fn suitability_work(s: &Session, p: &Value) -> Result<Work, String> {
    let crit = p.get("criteria").and_then(Value::as_array).ok_or("criteria is required")?;
    if crit.is_empty() || crit.len() > 20 {
        return Err("give 1 to 20 criteria".into());
    }
    let first = raster_of(s, criterion_layer(crit.first().ok_or("give 1 to 20 criteria")?)?)?;
    let mut criteria = Vec::new();
    for c in crit {
        let r = raster_of(s, criterion_layer(c)?)?;
        let band = band_of(c, &r)?;
        let ranges: Vec<(f64, f64, f64)> = c
            .get("ranges")
            .and_then(Value::as_array)
            .ok_or("each criterion has ranges [[lo, hi, score], …]")?
            .iter()
            .filter_map(|x| {
                let n: Vec<f64> = x.as_array()?.iter().filter_map(Value::as_f64).collect();
                if let [lo, hi, v] = n.as_slice() { Some((*lo, *hi, *v)) } else { None }
            })
            .collect();
        if ranges.is_empty() {
            return Err("each criterion has ranges [[lo, hi, score], …]".into());
        }
        criteria.push((r, band, ranges, c.get("weight").and_then(Value::as_f64).unwrap_or(1.0)));
    }
    let name = out_name(p, "Suitability".into())?;
    Ok(Box::new(move || {
        let mut scored = Vec::new();
        let mut empty = Vec::new();
        for (r, band, ranges, weight) in &criteria {
            let on_grid = rasterops::resample_to(r, *band, &first)?;
            let remap = rasterops::remap(&rasterops::derive(&first, &r.name, vec![on_grid])?, 0, ranges, false)?;
            empty.extend(remap.empty.into_iter().map(|(lo, hi)| (r.name.clone(), lo, hi)));
            scored.push((remap.cells, *weight));
        }
        let out = rasterops::weighted_overlay(&scored)?;
        // A cell is scored only where every criterion scores it.
        let none = !rasterops::has_values(&out, first.width as usize)?;
        let mut notes: Vec<String> = rasterops::empty_ranges_note(&empty).into_iter().collect();
        if none {
            notes.push(format!("{name}: no cell is scored by every criterion, so the result has no cells with values"));
        }
        made_then(rasterops::derive(&first, &name, vec![out])?, None, Some(notices(notes)))
    }))
}

fn viewshed(s: &mut Session, p: &Value) -> CmdResult {
    made_or_ran(s, p, "analysis.viewshed")
}

/// What a viewshed reads of the session, and the work that makes it.
fn viewshed_work(s: &Session, p: &Value) -> Result<Work, String> {
    let dem = raster_of(s, ps(p, "layer").ok_or("layer is required")?)?;
    let at = Coord::new(f(p, "lon").ok_or("lon is required")?, f(p, "lat").ok_or("lat is required")?);
    let band = band_of(p, &dem)?;
    let (cx, cy) = dem.cell_size_m();
    // The whole raster by default, however large: its diagonal reaches every cell from any
    // observer, and only the raster's cells are worked out. A radius given past the diagonal
    // covers the same cells, so it is taken as the diagonal: nothing is left out.
    let diagonal = (cx * dem.width as f64).hypot(cy * dem.height as f64).max(1.0);
    let radius = match p.get("radius") {
        None | Some(Value::Null) => diagonal,
        Some(v) => {
            f(p, "radius").filter(|r| *r > 0.0).ok_or_else(|| format!("radius must be a number of metres above 0, not {v}"))?.min(diagonal)
        }
    };
    // A raster around the whole Earth (in longitude/latitude) has its east and west edges side
    // by side, but rays don't cross from one to the other: said when the radius reaches an edge.
    // (Within half a cell of 360°: a cell size stored rounded falls a little short.)
    let pw = dem.transform.pixel_width.abs();
    let round_the_earth = dem.crs.is_geographic() && pw * f64::from(dem.width) >= 360.0 - pw / 2.0;
    let (oc, or) = dem.pixel_at(at).unwrap_or((0, 0));
    let (w, h) = (i64::from(dem.width), i64::from(dem.height));
    let (near, far) = (oc.min(w - 1 - oc), oc.max(w - 1 - oc));
    // Only when the way across the edge is shorter than the way through the raster: the cell
    // just across the near edge is `near + 1` cells away across it and `far` through the raster,
    // and with no shorter way (the observer within a cell of the middle column) every cell is
    // worked out as near as it is.
    let seam = round_the_earth && near + 1 < far && (near as f64 + 1.0) * cx <= radius;
    // The ground just across the edge is the far edge's column, reached only through the raster:
    // worked out the long way round in the rows where the radius reaches it (cell centres, as the
    // viewshed measures them). The rows with ground across the edge within the radius are those
    // where it reaches a cell one past the near edge. All of them are worked out when the radius
    // reaches the far column in the farthest of those rows, none when it doesn't reach the far
    // column even on the observer's row.
    let reaches = |dc: i64, dr: i64| (dc as f64 * cx).hypot(dr as f64 * cy) <= radius;
    // The rows are walked only on a raster round the Earth, at most its height.
    let last_row = if seam { (0..=or.max(h - 1 - or)).take_while(|dr| reaches(near + 1, *dr)).last().unwrap_or(0) } else { 0 };
    let across = if !reaches(far, 0) {
        "left out of the viewshed"
    } else if reaches(far, last_row) {
        "worked out the long way round"
    } else {
        "worked out the long way round near the observer's row and left out farther from it"
    };
    // Refused, naming it, when it isn't a height a viewshed takes: never another one used.
    let height = match p.get("height") {
        None | Some(Value::Null) => 1.7,
        Some(v) => f(p, "height")
            .filter(|h| (0.0..=10_000.0).contains(h))
            .ok_or_else(|| format!("height must be a number of metres from 0 to 10,000, not {v}"))?,
    };
    // How the ground along each ray is read: the cell a point falls in, or between cells.
    let heights = match p.get("heights") {
        None | Some(Value::Null) => rasterops::Heights::Nearest,
        Some(Value::String(h)) => {
            rasterops::Heights::named(h).ok_or_else(|| format!("heights must be \"nearest\" or \"bilinear\", not \"{h}\""))?
        }
        Some(other) => return Err(format!("heights must be \"nearest\" or \"bilinear\", not {other}")),
    };
    let name = out_name(p, format!("{}_viewshed", dem.name))?;
    let dem_name = dem.name.clone();
    Ok(Box::new(move || {
        let vs = rasterops::viewshed(&dem, band, at, height, radius, heights)?;
        let seen = vs.cells.iter().filter(|v| **v == 1.0).count();
        let (through_gaps, read_nearest) = (vs.through_gaps, vs.read_nearest);
        let answer = json!({"visibleCells": seen, "acrossNoData": through_gaps, "readNearest": read_nearest});
        let m = made(rasterops::derive(&dem, &name, vec![vs.cells])?, Some(answer))?;
        if through_gaps == 0 && read_nearest == 0 && !seam {
            return Ok(m);
        }
        // Ground with no data hides nothing, and bilinear heights read the nearest cell beside
        // it: said, with how many cells it touched. Rays that don't cross the raster's east–west
        // edge, said too.
        let then: Then = Box::new(move |s: &mut Session, _: &mut Value| {
            if seam {
                s.notify(
                    format!(
                        "{dem_name}: the raster goes round the Earth, but the viewshed's rays don't cross its east–west edge: ground just across it is {across}"
                    ),
                    false,
                );
            }
            if through_gaps > 0 {
                s.notify(
                    format!(
                        "{dem_name}: {through_gaps} cell(s) of the viewshed were seen across ground with no data, taken as hiding nothing"
                    ),
                    false,
                );
            }
            if read_nearest > 0 {
                s.notify(
                    format!("{dem_name}: {read_nearest} cell(s) were worked out with the nearest cell's height next to ground with no data, not between cells"),
                    false,
                );
            }
        });
        Ok(Made { then: Some(then), ..m })
    }))
}

fn line_of_sight(s: &mut Session, p: &Value) -> CmdResult {
    let dem = raster_of(s, ps(p, "layer").ok_or("layer is required")?)?;
    let name = out_name(p, "Line of Sight".into())?;
    let (a, b2) = (point(p, "from")?, point(p, "to")?);
    let band = band_of(p, &dem)?;
    let (seen, prof) =
        rasterops::line_of_sight(&dem, band, a, b2, f(p, "observerHeight").unwrap_or(1.7), f(p, "targetHeight").unwrap_or(0.0), 400)
            .ok_or("the observer is outside the raster or on no-data")?;
    // Visible and hidden stretches as separate lines.
    let mut ds = Dataset::new(name, GeometryType::Polyline, vec![Field::new("visible", FieldType::Boolean)]);
    let mut run: Vec<Coord> = Vec::new();
    let mut state: Option<bool> = None;
    for (_, c, _, vis) in &prof {
        if state.is_some_and(|st| st != *vis)
            && let Some(last) = run.last().copied()
        {
            if run.len() >= 2 {
                ds.push(Some(Geometry::Line(vec![std::mem::take(&mut run)])), vec![Attr::Bool(state.unwrap_or(false))]);
            }
            run = vec![last];
        }
        run.push(*c);
        state = Some(*vis);
    }
    if run.len() >= 2 {
        ds.push(Some(Geometry::Line(vec![run])), vec![Attr::Bool(state.unwrap_or(false))]);
    }
    let id = super::layer::add_dataset(s, ds);
    if let Some(l) = s.map_mut().layer_mut(id)
        && let Ok(r) = rustgis_map::Renderer::unique(&l.dataset, &[], "visible", rustgis_map::Ramp::default(), l.renderer.base_symbol())
    {
        l.renderer = r;
    }
    let profile: Vec<Value> = prof.iter().step_by(4).map(|(d, _, z, v)| json!({"distance": d, "elevation": z, "visible": v})).collect();
    Ok(json!({"layer": id, "visible": seen, "profile": profile}))
}

fn interpolate(s: &mut Session, p: &Value) -> CmdResult {
    made_or_ran(s, p, "analysis.interpolate")
}

/// What an IDW surface reads of the session, and the work that makes it.
fn interpolate_work(s: &Session, p: &Value) -> Result<Work, String> {
    let ds = feature_ds(s, p)?;
    let field = ps(p, "field").ok_or("field is required")?;
    let fi = ds.field_index(field).ok_or_else(|| format!("no field named '{field}'"))?;
    let name = out_name(p, format!("{}_{field}_idw", ds.name))?;
    // Features without a place or a number are left out, and said (by kind, with a count).
    let (mut no_point, mut no_value) = (0usize, 0usize);
    let mut pts: Vec<(Coord, f64)> = Vec::with_capacity(ds.features.len());
    // The run's system: the layer's own projected one it holds its coordinates in, as the
    // interpolation tools take it (`rustgis_analysis::works_in`), its places taken as they are; else
    // longitude/latitude, each place given there (one that can't be has no place). There a point
    // is converted alone; any other shape is converted first, so its label point (a polygon's
    // centroid) is the one it has in longitude/latitude. A shape with a place that can't be
    // converted gives its label point in its own system, converted, as long as that converts.
    let work = Some(ds.coord_crs()).filter(|c| rustgis_analysis::works_in(*c)).unwrap_or(Crs::LonLat);
    let to = if work == Crs::LonLat { Some(ds.lonlat_conversion()?) } else { None };
    let place = |g: &Geometry| match (&to, g) {
        (None, g) => g.label_point(),
        (Some(to), Geometry::Point(c)) => to.point(*c),
        (Some(to), g) => match Dataset::converted_to_lonlat(to, g) {
            Some(g) => g.label_point(),
            None => g.label_point().and_then(|c| to.point(c)),
        },
    };
    for (r, ft) in ds.features.iter().enumerate() {
        let Some(c) = ft.geometry.as_ref().and_then(place).filter(|c| c.is_finite()) else {
            no_point += 1;
            continue;
        };
        let Some(v) = ds.value(r, fi).as_f64().filter(|v| v.is_finite()) else {
            no_value += 1;
            continue;
        };
        pts.push((c, v));
    }
    let (layer_name, field_name) = (ds.name.clone(), field.to_string());
    // The system: the one given, else the environment's, else the projected one the layer holds its
    // coordinates in, else the one it was read in, else degrees.
    let env = &s.project.gp_env;
    // An empty `crs` is none given, as `null` is: the environment's system still applies.
    let given = p.get("crs").is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|t| !t.trim().is_empty()));
    let (crs, crs_note) = match env.projected {
        Some(g) if !given => (g.crs, None),
        _ => rustgis_analysis::raster_crs_in(ds, work, s.project.coordinate_system, p, "Geostatistical Wizard")?,
    };
    let crs_note = rustgis_analysis::in_its_units(p, crs, crs_note, &Extent::EMPTY, &[], &["cellSize"]);
    let env_extent = env.extent;
    let given_cell = f(p, "cellSize").or(env.cell_in(crs));
    // The environment's cell sizes this run doesn't use are said: the projected one when the run is
    // in another system, the degree one when it isn't in degrees. Run in the environment's own
    // system with its size, the degree size kept beside it (for older versions) goes unsaid.
    let own = env.projected.is_some_and(|g| g.crs == crs && g.cell_size.is_some());
    let unused: Vec<String> = [
        env.projected.filter(|g| g.crs != crs).and_then(|g| g.cell_size.map(|c| format!("{c}, in {}'s units", g.crs.label()))),
        env.cell_size.filter(|_| crs != Crs::LonLat && !own).map(|c| format!("{c}, in degrees")),
    ]
    .into_iter()
    .flatten()
    .collect();
    let cell_note = (f(p, "cellSize").is_none() && !unused.is_empty()).then(|| {
        let (what, verb) = if unused.len() > 1 { ("sizes", "weren't") } else { ("size", "wasn't") };
        let from_extent = if given_cell.is_none() { "; the cell size was set from the extent" } else { "" };
        format!(
            "Geostatistical Wizard: the environment's cell {what} ({}) {verb} used for a surface made in {}{from_extent}. Give cellSize, or set the environment's cell size in {}",
            unused.join("; "),
            crs.label(),
            if crs == Crs::LonLat { "degrees".to_string() } else { format!("{}'s units", crs.label()) }
        )
    });
    let power = f(p, "power").unwrap_or(2.0).clamp(0.5, 10.0);
    let neighbors = p.get("neighbors").and_then(Value::as_u64).unwrap_or(12).max(1) as usize;
    Ok(Box::new(move || {
        // Off the window's thread: the points, and the grid they set, in the system.
        let (pts, beyond_note) = rustgis_analysis::points_from(work, crs, pts)?;
        let e = match env_extent {
            Some(e) if crs == Crs::LonLat => e,
            Some(e) => {
                let [x0, y0, x1, y1] = rustgis_analysis::warp::bounds_in([e.min_x, e.min_y, e.max_x, e.max_y], crs)?;
                Extent::new(x0, y0, x1, y1)
            }
            None => {
                let mut e = Extent::EMPTY;
                pts.iter().for_each(|(c, _)| e.expand(*c));
                e.padded(0.05, rustgis_analysis::min_pad(crs))
            }
        };
        let cell = given_cell.unwrap_or((e.width().max(e.height()) / 300.0).max(1e-5));
        // A surface that can't be made says what was left out of it, so the input can be fixed.
        let left_out = |e: String| {
            let mut why = Vec::new();
            if no_point > 0 {
                why.push(format!("{no_point} features with no geometry or no usable point"));
            }
            if no_value > 0 {
                why.push(format!("{no_value} whose value isn't a number"));
            }
            let e = if why.is_empty() { e } else { format!("{e} ({} left out)", why.join(", ")) };
            // A size read in the layer's system's units, or one the environment holds that wasn't
            // used, can be why it failed.
            let notes: Vec<&str> = [crs_note.as_deref(), cell_note.as_deref()].into_iter().flatten().collect();
            let e = if notes.is_empty() { e } else { format!("{e} ({})", notes.join("; ")) };
            // Points the system can't hold can be why there are too few.
            match &beyond_note {
                Some(note) => format!("{e}; {note}"),
                None => e,
            }
        };
        let m = made(rasterops::idw(&pts, &e, cell, power, neighbors, &name, crs).map_err(left_out)?, None)?;
        if no_point == 0 && no_value == 0 && beyond_note.is_none() && crs_note.is_none() && cell_note.is_none() {
            return Ok(m);
        }
        let then: Then = Box::new(move |s: &mut Session, v: &mut Value| {
            let mut why = Vec::new();
            if no_point > 0 {
                why.push(format!("{no_point} with no geometry or no usable point"));
            }
            if no_value > 0 {
                why.push(format!("{no_value} whose {field_name} isn't a number"));
            }
            if !why.is_empty() {
                s.notify(format!("{layer_name}: features left out of the surface: {}", why.join(", ")), false);
                if let Some(o) = v.as_object_mut() {
                    o.insert("skipped".into(), json!({"noPoint": no_point, "noValue": no_value}));
                }
            }
            if let Some(note) = beyond_note {
                s.notify(format!("{layer_name}: {note}"), false);
            }
            // It names the layer itself.
            if let Some(note) = crs_note {
                s.notify(note, false);
            }
            if let Some(note) = cell_note {
                s.notify(note, false);
            }
        });
        Ok(Made { then: Some(then), ..m })
    }))
}

/// Routes over the query-visible resident network, independently of selection. Invalid
/// source segments and unreadable-query fallback are reported after a successful route.
fn route(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    if l.geometry_type() != GeometryType::Polyline {
        return Err("routes run over a line layer".into());
    }
    let name = out_name(p, format!("Route on {}", l.name))?;
    let cost = ps(p, "costField").map(|n| l.dataset.field_index(n).ok_or_else(|| format!("no field named '{n}'"))).transpose()?;
    let resolved = l.resolve_rows(&l.dataset, rustgis_map::rows::Policy::Query, None)?;
    let query_notice = resolved.query_error.map(|error| {
        crate::notice_text::NoticeText::format(
            "1 layer with an unreadable definition query; reading without that query: {layer}: {error}",
            [("layer", l.name.clone().into()), ("error", error.into())],
        )
    });
    let rows: Vec<usize> = match resolved.rows {
        rustgis_map::rows::Rows::All => (0..l.dataset.features.len()).collect(),
        rustgis_map::rows::Rows::Subset(rows) => rows,
    };
    // The network in longitude/latitude, as `from` and `to` are, whatever system the layer holds
    // its coordinates in: converted only when its graph is built, which is kept for the layer's
    // rows, so routes over a held network reuse it as they do over one in longitude/latitude.
    let system = (l.dataset.coord_crs(), l.dataset.crs);
    let r = network::shortest_route_held(
        &l.dataset,
        || l.dataset.in_lonlat(),
        &rows,
        point(p, "from")?,
        point(p, "to")?,
        cost,
        f(p, "snapDistance").unwrap_or(1000.0).clamp(0.0, 1e6),
    )?;
    let source_name = l.name.clone();
    let mut ds =
        Dataset::new(name, GeometryType::Polyline, vec![Field::new("length_km", FieldType::Double), Field::new("cost", FieldType::Double)]);
    ds.push(Some(Geometry::Line(vec![r.path.clone()])), vec![Attr::Float(r.length_m / 1000.0), Attr::Float(r.cost)]);
    hold_as(s, &mut ds, system)?;
    let nid = super::layer::add_dataset(s, ds);
    if let Some(notice) = query_notice {
        s.notify_text(notice, false);
    }
    if r.skipped_segments > 0 {
        s.notify(
            format!("{source_name}: {} segments with invalid coordinates were omitted from the route network", r.skipped_segments),
            false,
        );
    }
    if r.overflowing > 0 {
        s.notify(
            format!(
                "{source_name}: {} line(s) cost so much that a segment's cost is past the largest number, and were closed in the route network",
                r.overflowing
            ),
            false,
        );
    }
    Ok(json!({"layer": nid, "lengthKm": r.length_m / 1000.0, "cost": r.cost, "segments": r.rows.len()}))
}

/// Searches query-visible resident rows independently of selection, replacing selection only
/// when requested. Unreadable-query fallbacks are counted after a successful search.
fn neighborhood(s: &mut Session, p: &Value) -> CmdResult {
    let at = Coord::new(f(p, "lon").ok_or("lon is required")?, f(p, "lat").ok_or("lat is required")?);
    if !(at.x.is_finite() && at.y.abs() <= 90.0) {
        return Err("lon must be a finite number and lat within -90 to 90".into());
    }
    let radius = f(p, "radius").filter(|r| *r > 0.0).ok_or("radius is required (metres)")?.min(5e6);
    let only: Option<Vec<String>> =
        p.get("layers").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_ascii_lowercase)).collect());
    let mut out = Vec::new();
    let mut found: Vec<(u32, Vec<usize>)> = Vec::new();
    let mut bounded = Vec::new();
    let mut invalid = Vec::new();
    let mut query_errors = Vec::new();
    let mut query_error_count = 0;
    for l in s.map().layers.iter().filter(|l| s.map().shows(l) && !l.is_raster() && !l.graphics) {
        if only.as_ref().is_some_and(|o| !o.contains(&l.name.to_ascii_lowercase())) {
            continue;
        }
        let resolved = l.resolve_rows(&l.dataset, rustgis_map::rows::Policy::Query, None)?;
        if let Some(error) = resolved.query_error {
            query_error_count += 1;
            // NoticeText::list nests entries; keep its depth independent of the layer count.
            if query_errors.len() < 5 {
                query_errors.push(crate::notice_text::NoticeText::format(
                    "{layer}: {error}",
                    [("layer", l.name.clone().into()), ("error", error.into())],
                ));
            }
        }
        let rows: Vec<usize> = match resolved.rows {
            rustgis_map::rows::Rows::All => (0..l.dataset.features.len()).collect(),
            rustgis_map::rows::Rows::Subset(rows) => rows,
        };
        let report = network::within_report(&l.dataset, &rows, at, radius);
        let (near, n) = (report.rows, report.bounded);
        invalid.push((l.name.clone(), report.invalid_latitudes, report.missing_coordinates));
        if n > 0 {
            bounded.push((l.name.clone(), n));
        }
        if near.is_empty() {
            continue;
        }
        // Numeric fields summed over the neighbourhood.
        let sums: serde_json::Map<String, Value> = l
            .dataset
            .fields
            .iter()
            .enumerate()
            .filter(|(_, fd)| fd.ty.is_numeric())
            .map(|(fi, fd)| (fd.name.clone(), json!(near.iter().filter_map(|(r, _)| l.dataset.value(*r, fi).as_f64()).sum::<f64>())))
            .collect();
        out.push(json!({"layer": l.name, "count": near.len(), "nearestKm": near.first().map(|x| x.1 / 1000.0), "sums": sums}));
        found.push((l.id, near.into_iter().map(|x| x.0).collect()));
    }
    super::select::warn_bounded(s, &bounded);
    super::select::warn_radius_input(s, &invalid);
    if b(p, "select").unwrap_or(false) {
        s.map_mut().clear_selection();
        for (id, rows) in found {
            super::select::set(s, id, rows, super::select::Mode::Add);
        }
    }
    if query_error_count > 0 {
        use crate::notice_text::NoticeText;
        let key = if query_error_count == 1 {
            "{count} layer with an unreadable definition query; reading without that query: {layers}"
        } else {
            "{count} layers with unreadable definition queries; reading without those queries: {layers}"
        };
        let more = query_error_count - query_errors.len();
        let mut layers = NoticeText::list(query_errors);
        if more > 0 {
            layers = NoticeText::format("{layers}; {count} more layers", [("layers", layers), ("count", more.to_string().into())]);
        }
        s.notify_text(NoticeText::format(key, [("count", query_error_count.to_string().into()), ("layers", layers)]), false);
    }
    Ok(json!({"radiusKm": radius / 1000.0, "layers": out}))
}

/// Generate points in resident polygons kept by the selection and definition query. An
/// unreadable saved query leaves those polygons unfiltered and is reported once.
fn simulate(s: &mut Session, p: &Value) -> CmdResult {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).ok_or("no such layer")?;
    if l.geometry_type() != GeometryType::Polygon {
        return Err("points are simulated inside polygons".into());
    }
    let name = out_name(p, format!("{}_simulated", l.name))?;
    let resolved = l.resolve_rows(&l.dataset, rustgis_map::rows::Policy::SelectionAndQuery, None)?;
    let query_notice = resolved.query_error.map(|error| {
        crate::notice_text::NoticeText::format(
            "1 layer with an unreadable definition query; reading without that query: {layer}: {error}",
            [("layer", l.name.clone().into()), ("error", error.into())],
        )
    });
    let rows: Vec<usize> = match resolved.rows {
        rustgis_map::rows::Rows::All => (0..l.dataset.features.len()).collect(),
        rustgis_map::rows::Rows::Subset(rows) => rows,
    };
    // Placed in the polygons' own coordinates when their layer holds them in a projected system,
    // spread evenly there (#2032 C2e-5), as the analysis tools work in it
    // ([`rustgis_analysis::works_in`]); else in longitude/latitude, then held in the layer's
    // system, as the polygons are.
    let system = (l.dataset.coord_crs(), l.dataset.crs);
    let own = rustgis_analysis::works_in(system.0);
    let polygons = if own { std::borrow::Cow::Borrowed(&l.dataset) } else { l.dataset.in_lonlat()? };
    let seed = f(p, "seed").map_or(42, |v| v.max(0.0) as u64);
    let pts: Vec<(usize, Coord)> = match ps(p, "field") {
        Some(fname) => {
            // A count per polygon from a field (e.g. population × rate).
            let fi = l.dataset.field_index(fname).ok_or_else(|| format!("no field named '{fname}'"))?;
            let rate = f(p, "rate").unwrap_or(1.0).max(0.0);
            let mut out = Vec::new();
            for r in &rows {
                let n = (l.dataset.value(*r, fi).as_f64().unwrap_or(0.0) * rate).round().clamp(0.0, 100_000.0) as usize;
                out.extend(network::random_points(&polygons, &[*r], n, seed.wrapping_add(*r as u64)));
                if out.len() > 1_000_000 {
                    return Err("that would make more than a million points; lower the rate".into());
                }
            }
            out
        }
        None => {
            let per = f(p, "count").unwrap_or(100.0).clamp(1.0, 100_000.0) as usize;
            if rows.len().saturating_mul(per) > 1_000_000 {
                return Err("that would make more than a million points; lower the count".into());
            }
            network::random_points(&polygons, &rows, per, seed)
        }
    };
    let mut ds = Dataset::new(name, GeometryType::Point, vec![Field::new("polygon", FieldType::Integer)]);
    for (r, c) in &pts {
        ds.push(Some(Geometry::Point(*c)), vec![Attr::Int(*r as i64)]);
    }
    let n = ds.features.len();
    // A cancel stops the sampling with the points made so far: none of them is added.
    rustgis_data::progress::check().map_err(|e| e.to_string())?;
    if own {
        (ds.held_in, ds.crs) = system;
    } else {
        hold_as(s, &mut ds, system)?;
    }
    if let Some(notice) = query_notice {
        s.notify_text(notice, false);
    }
    let nid = super::layer::add_dataset(s, ds);
    Ok(json!({"layer": nid, "points": n}))
}

/// Holds `ds`, a layer made in longitude/latitude from another, as that layer is
/// ([`Dataset::hold_as`]; `system` is its [`Dataset::coord_crs`] and [`Dataset::crs`]), and says
/// so when it can't be: it stays in longitude/latitude then, as it was made. A cancel stops the
/// command, which adds nothing.
pub(crate) fn hold_as(
    s: &mut Session,
    ds: &mut Dataset,
    (held, crs): (rustgis_data::raster::Crs, Option<rustgis_data::raster::Crs>),
) -> Result<(), String> {
    match ds.hold_as(held, crs) {
        Err(e) if matches!(rustgis_data::DataError::of_conversion(e.clone()), rustgis_data::DataError::Cancelled) => Err(e),
        Err(e) => {
            s.notify(format!("{}: kept in longitude/latitude, as it was made, not in {}: {e}", ds.name, held.label()), false);
            Ok(())
        }
        Ok(()) => Ok(()),
    }
}

/// Numeric fields requested by name, or all numeric fields when omitted; invalid names fail.
pub(super) fn numeric_fields(ds: &Dataset, p: &Value) -> Result<Vec<usize>, String> {
    match p.get("fields").and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .map(|v| {
                let n = v.as_str().ok_or("fields are names")?;
                ds.field_index(n)
                    .filter(|i| ds.fields.get(*i).is_some_and(|f| f.ty.is_numeric()))
                    .ok_or_else(|| format!("no numeric field '{n}'"))
            })
            .collect(),
        None => Ok(ds.fields.iter().enumerate().filter(|(_, f)| f.ty.is_numeric()).map(|(i, _)| i).collect()),
    }
}

/// Histograms and correlations over All resident rows, independent of map filters/selection.
fn explore(s: &mut Session, p: &Value) -> CmdResult {
    let ds = feature_ds(s, p)?;
    let fields = numeric_fields(ds, p)?;
    if fields.is_empty() {
        return Err("the layer has no numeric fields".into());
    }
    let bins = p.get("bins").and_then(Value::as_u64).unwrap_or(10).clamp(2, 100) as usize;
    let cols: Vec<Vec<Option<f64>>> =
        fields.iter().map(|fi| (0..ds.features.len()).map(|r| ds.value(r, *fi).as_f64().filter(|v| v.is_finite())).collect()).collect();
    let hist: Vec<Value> = fields
        .iter()
        .zip(&cols)
        .map(|(fi, c)| {
            let v: Vec<f64> = c.iter().flatten().copied().collect();
            let (lo, hi) = v.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| (a.min(*x), b.max(*x)));
            let mut counts = vec![0usize; bins];
            for x in &v {
                let i = if hi > lo { (((x - lo) / (hi - lo)) * bins as f64) as usize } else { 0 };
                if let Some(c) = counts.get_mut(i.min(bins - 1)) {
                    *c += 1;
                }
            }
            json!({"field": ds.fields.get(*fi).map(|f| f.name.clone()), "min": lo, "max": hi, "counts": counts})
        })
        .collect();
    let corr = |a: &[Option<f64>], b: &[Option<f64>]| -> Option<f64> {
        let pairs: Vec<(f64, f64)> = a.iter().zip(b).filter_map(|(x, y)| Some(((*x)?, (*y)?))).collect();
        let n = pairs.len() as f64;
        if n < 3.0 {
            return None;
        }
        let (mx, my) = (pairs.iter().map(|p| p.0).sum::<f64>() / n, pairs.iter().map(|p| p.1).sum::<f64>() / n);
        let (sxy, sxx, syy) =
            pairs.iter().fold((0.0, 0.0, 0.0), |(a, b, c), (x, y)| (a + (x - mx) * (y - my), b + (x - mx).powi(2), c + (y - my).powi(2)));
        (sxx > 0.0 && syy > 0.0).then(|| sxy / (sxx * syy).sqrt())
    };
    let matrix: Vec<Vec<Option<f64>>> = cols.iter().map(|a| cols.iter().map(|b| corr(a, b)).collect()).collect();
    Ok(
        json!({"histograms": hist, "fields": fields.iter().filter_map(|fi| ds.fields.get(*fi).map(|f| f.name.clone())).collect::<Vec<_>>(), "correlation": matrix}),
    )
}

/// Converts text on the caller's thread for scripts and browser downloads. A streamed layer
/// needs a file path so its full source can be read off the desktop frame, rather than returning
/// only its loaded view. File conversions use the shared export lifecycle.
fn convert(s: &mut Session, p: &Value) -> CmdResult {
    use std::borrow::Cow;

    if ps(p, "path").is_some() {
        return super::exports::write_here(s, p, "data.convert");
    }
    let id = s.layer_id(p)?;
    let layer = s.map().layer(id).ok_or("no such layer")?;
    if layer.is_streamed() {
        return Err("a streamed layer needs a path to convert its whole source; text conversion would contain only the loaded view".into());
    }
    let construction_notes = s.constructions.export_notes(s.map().instance_id(), layer)?;
    let mut ds = Cow::Borrowed(feature_ds(s, p)?);
    let report = rustgis_data::export::filter_dataset(&mut ds, layer.definition_query.as_deref()).map_err(|e| e.to_string())?;
    let (text, notes) = converted(&ds, p)?;
    let null_numbers = rustgis_data::export::numbers_written_as_null(&ds.features, &ds.fields, conversion_format(p)?);
    let target = ds.name.clone();
    let query_note = super::share::query_export_note(&target, &layer.name, &report);
    drop(ds);
    for note in notes.into_iter().chain(construction_notes) {
        s.notify(note, false);
    }
    super::share::notify_null_numbers(s, &target, null_numbers);
    if let Some(note) = query_note {
        s.notify_text(note, false);
    }
    Ok(json!({"text": text}))
}

/// Captures a conversion's layer and parameters without reading its source on the UI thread.
/// GeoJSON and CSV use the chunk writer; KML, GPX and WKT read whole within the existing memory
/// limits, then check Cancel between features (one geometry's serialization is indivisible).
pub(crate) fn convert_work(s: &Session, p: &Value) -> Result<super::exports::Work, String> {
    use std::borrow::Cow;

    feature_ds(s, p)?;
    let id = s.layer_id(p)?;
    let layer = s.map().layer(id).ok_or("no such layer")?.clone();
    let path = ps(p, "path").ok_or("path is required")?.to_string();
    conversion_format(p)?;
    let params = p.clone();
    let (constructions, map) = (s.constructions.clone(), s.map().instance_id());
    Ok(Box::new(move || {
        let construction_notes = constructions.export_notes(map, &layer)?;
        let format = conversion_format(&params)?;
        let chunks = if layer.is_streamed() && matches!(format, "geojson" | "csv") { layer.chunk_export()? } else { None };
        let query = layer.definition_query.as_deref();
        let (mut notes, null_numbers, query_report) = if let Some((source, ds)) = chunks {
            let result = rustgis_data::export::write_source_as_filtered(
                source.as_ref(),
                &ds,
                std::path::Path::new(&path),
                rustgis_data::raster::Crs::LonLat,
                format,
                query,
            )
            .map_err(|e| e.to_string())?;
            let mut notes: rustgis_data::loss::Notes =
                rustgis_data::export_notes([(&ds, layer.extent())], &path, rustgis_data::raster::Crs::LonLat).into();
            if format == "csv" {
                notes.extend(result.csv.notes(&ds.fields, &path));
                notes.extend(result.csv.coordinates_note(&ds.fields, &path));
                notes.extend(result.text.notes(&ds.fields, &path));
            }
            if result.skipped > 0 {
                notes.push(format!("{path}: {} rows couldn't be read and were left out", result.skipped));
            }
            notes.extend(
                rustgis_data::source::unread_dates_note(&ds.fields, &result.unread_dates)
                    .map(|n| format!("{path}: {}: {n} (empty in the export)", ds.name)),
            );
            notes.extend(
                rustgis_data::source::rounded_dates_note(&ds.fields, &result.rounded_dates)
                    .map(|n| format!("{path}: {}: {n} when read for the export", ds.name)),
            );
            notes.extend(
                rustgis_data::source::null_text_note(&ds.fields, &result.null_text)
                    .map(|n| format!("{path}: {}: {n} (empty in the export)", ds.name)),
            );
            notes.extend(
                result.read_notes.iter().map(|n| n.clone().map_text(|t| format!("{path}: {}: {t} when read for the export", ds.name))),
            );
            notes.extend(
                rustgis_data::source::not_utf8_note(&ds.fields, &result.not_utf8, result.not_utf8_in)
                    .map(|n| format!("{path}: {}: {n}", ds.name)),
            );
            (notes, result.null_numbers, result.query)
        } else {
            let (mut ds, unreadable) = if layer.is_streamed() {
                let (ds, unreadable) = layer.load_all_counted()?;
                (Cow::Owned(ds), unreadable)
            } else {
                (Cow::Borrowed(&layer.dataset), rustgis_map::Unreadable::default())
            };
            let report = rustgis_data::export::filter_dataset(&mut ds, query).map_err(|e| e.to_string())?;
            rustgis_data::progress::begin(ds.features.len());
            let (text, notes) = converted(&ds, &params)?;
            let mut notes: rustgis_data::loss::Notes = notes.into();
            super::share::coalesce_read_zm(&mut notes, &path, &ds, &unreadable);
            rustgis_data::io::write_atomic(std::path::Path::new(&path), text.as_bytes()).map_err(|e| format!("{path}: {e}"))?;
            if unreadable.rows > 0 {
                notes.push(format!("{path}: {} rows couldn't be read and were left out", unreadable.rows));
            }
            notes.extend(super::share::read_notes(&path, &ds.name, &unreadable));
            (notes, rustgis_data::export::numbers_written_as_null(&ds.features, &ds.fields, format), report)
        };
        let bytes = std::fs::metadata(&path).map_err(|e| format!("{path}: {e}"))?.len();
        notes.extend(construction_notes);
        notes.push(format!("Exported {path}"));
        let target = path.clone();
        let query_note = super::share::query_export_note(&target, &layer.name, &query_report);
        let refused_notes = super::share::null_numbers_note(&target, null_numbers).iter().chain(&query_note).map(|n| n.english()).collect();
        let then = (null_numbers > 0 || query_note.is_some()).then(|| {
            Box::new(move |s: &mut Session, _: &mut Value| {
                super::share::notify_null_numbers(s, &target, null_numbers);
                super::share::notify_written_query(s, query_note);
                Ok(())
            }) as super::exports::Then
        });
        Ok(super::exports::Wrote {
            answer: json!({"path": path, "bytes": bytes}),
            notes,
            diagnostics: Vec::new(),
            refused_notes,
            written_path: Some(path),
            then,
            #[cfg(not(target_arch = "wasm32"))]
            owner: None,
            #[cfg(not(target_arch = "wasm32"))]
            crs_used: Vec::new(),
        })
    }))
}

/// Validates the existing case-sensitive format parameter before starting background work.
fn conversion_format(p: &Value) -> Result<&str, String> {
    match ps(p, "format").ok_or("format is required")? {
        f @ ("geojson" | "csv" | "kml" | "gpx" | "wkt") => Ok(f),
        f => Err(format!("unknown format '{f}'")),
    }
}

/// Serializes a complete dataset and collects its loss notices for delivery after success.
fn converted(ds: &Dataset, p: &Value) -> Result<(String, Vec<String>), String> {
    let name_field =
        ps(p, "nameField").and_then(|n| ds.field_index(n)).or_else(|| ds.fields.iter().position(|f| f.name.eq_ignore_ascii_case("name")));
    let format = conversion_format(p)?;
    // Every format here is longitude/latitude, whatever system the layer holds its coordinates
    // in; the notes below read the layer itself, converting as they need.
    let lonlat = ds.in_lonlat()?;
    let text = match format {
        "geojson" => rustgis_data::io::to_geojson_text(&lonlat).map_err(|e| e.to_string())?,
        "csv" => rustgis_data::io::to_csv(&lonlat).map_err(|e| e.to_string())?,
        "kml" => rustgis_data::interop::to_kml(&lonlat, name_field).map_err(|e| e.to_string())?,
        "gpx" => rustgis_data::interop::to_gpx(&lonlat, name_field).map_err(|e| e.to_string())?,
        _ => rustgis_data::interop::to_wkt_csv(&lonlat).map_err(|e| e.to_string())?,
    };
    let target = ps(p, "path").unwrap_or(&ds.name);
    let mut notes = rustgis_data::export_notes([(ds, ds.lonlat_extent())], target, rustgis_data::raster::Crs::LonLat);
    // CSV holds a point a row, GPX a name for each feature and no areas; reading text back, CSV reads blanks as null and KML and GPX trim it.
    notes.extend(rustgis_data::io::export_losses_notes(ds, format, name_field, target).map_err(|e| e.to_string())?);
    // KML and GPX hold features with geometry only.
    if let ext @ ("kml" | "gpx") = format {
        let without =
            if ext == "gpx" { rustgis_data::io::gpx_left_out(ds) } else { ds.features.iter().filter(|f| f.geometry.is_none()).count() };
        notes.extend(rustgis_data::io::left_out_note(target, ext, without));
    }
    Ok((text, notes))
}

/// Binds the extent layer, or all full-extent layers, without computing a view or loading pixels.
fn environment_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    match ps(p, "extent") {
        Some("full") => Ok(super::Inputs::layers(s.map().layers.iter().map(|l| (s.map(), l)))),
        Some(n) if n != "view" => super::Inputs::named(s.map(), [n]),
        _ => Ok(super::Inputs::layers(std::iter::empty())),
    }
}
/// Only resampling consumes a required target raster; all other functions return no target.
fn resample_target_name(p: &Value) -> Result<Option<&str>, String> {
    if ps(p, "function") == Some("resample") {
        Ok(Some(ps(p, "target").ok_or("target (a raster to match) is required")?))
    } else {
        Ok(None)
    }
}
/// Binds the actual primary raster and conditional resampling template.
fn function_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    super::Inputs::named(s.map(), [ps(p, "layer").ok_or("layer is required")?].into_iter().chain(resample_target_name(p)?))
}
/// Collects each function's actual target choice in step order.
fn chain_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let mut names = vec![ps(p, "layer").ok_or("layer is required")?];
    for step in p.get("steps").and_then(Value::as_array).ok_or("steps is required")? {
        names.extend(resample_target_name(step)?);
    }
    super::Inputs::named(s.map(), names)
}
/// Parses the literal raster selector used by each suitability criterion.
fn criterion_layer(p: &Value) -> Result<&str, String> {
    ps(p, "layer").ok_or_else(|| "each criterion names a raster layer".into())
}
/// Binds the same ordered criterion raster names used by the suitability executor.
fn suitability_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    let names = p
        .get("criteria")
        .and_then(Value::as_array)
        .ok_or("criteria is required")?
        .iter()
        .map(criterion_layer)
        .collect::<Result<Vec<_>, _>>()?;
    super::Inputs::named(s.map(), names)
}
