//! Analyze › Image Classification (Deep Learning Tools), Publish › Package (Deep Learning) and
//! Analyze › Imagery Sharing (Create Hosted Imagery): neural-network pixel classification with models kept
//! in the project and shared as packages, and rasters cut into web map tiles.

use std::collections::HashMap;
use std::path::Path;

use rustgis_analysis::neural::{self, MAX_SAMPLES_PER_CLASS, Mlp};
use rustgis_analysis::rasterops;
use rustgis_data::{Geometry, GeometryType};
use rustgis_geom::Coord;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::imagery::{bands_param, class_layer, name, raster};
use super::{f, ps};
use crate::{CmdResult, CommandSpec as C, Session};

/// Deep learning packages: zip files with this extension.
pub const PACKAGE_EXTENSION: &str = "rgisdlpk";
/// Most tiles a hosted imagery export writes.
const MAX_TILES: usize = 5_000;
/// Largest total size of a hosted imagery package's tiles.
const MAX_TILE_BYTES: usize = 256 << 20;
const TILE: u32 = 256;

/// A trained model kept in the project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DlModel {
    pub name: String,
    pub model: Mlp,
    /// Accuracy on held-back training pixels (0–1).
    #[serde(default)]
    pub accuracy: f64,
    /// Epochs the last training ran (not saved).
    #[serde(skip)]
    pub epochs: usize,
}

fn need_raster(s: &Session) -> Option<&'static str> {
    if s.map().layers.iter().any(|l| l.is_raster()) { None } else { Some("add a raster layer first") }
}

fn need_model(s: &Session) -> Option<&'static str> {
    if s.project.dl_models.is_empty() { Some("train a model first (Deep Learning Tools)") } else { None }
}

pub fn commands() -> Vec<C> {
    vec![
        C::new("dl.train", "Train Deep Learning Model", "Analyze › Image Classification", |s, p| {
            let (m, unread) = train(s, p)?;
            unread.say(s);
            let mut r = json!({"model": m.name, "accuracy": m.accuracy, "classes": m.model.classes, "epochs": m.epochs});
            r["replaced"] = json!(store(s, m));
            Ok(r)
        }).inputs(training_inputs).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_raster)
        .params(r#"{"layer": raster, "training": polygon layer, "classField": field naming each polygon's class, "bands"?: [1-based], "hidden"?: [units per hidden layer] (default [32, 16]), "epochs"?: 1..500 (default 60), "model"?: name}"#),
        C::new("dl.classify", "Deep Learning Tools", "Analyze › Image Classification", classify).inputs(classify_inputs).roles(&[]).effects(crate::policy::Effects::NONE).when(need_raster).params(
            r#"{"layer": raster, "model"?: a trained model's name, "training"?: polygon layer, "classField"?: field, "bands"?: [1-based], "hidden"?: [units], "epochs"?: 1..500 (without a model: train one first, as dl.train), "name"?: output, "ran"?: (internal: classes the app has worked out)}"#,
        ),
        C::new("dl.package", "Deep Learning Package", "Publish › Package", |s, p| {
            let path = ps(p, "path").ok_or("path is required")?;
            let bytes = package(s, p)?;
            rustgis_data::io::write_atomic(Path::new(path), &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(json!({"path": path, "bytes": bytes.len()}))
        }).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .view()
        .when(need_model)
        .params(r#"{"model"?: name (default: the last trained), "path": "*.rgisdlpk"}"#),
        C::new("dl.import", "Import Deep Learning Package", "Analyze › Image Classification", |s, p| {
            let bytes = read_bytes(p)?;
            // Only the model is inflated, whatever else the package holds.
            let entries = rustgis_data::zip::read_matching(&bytes, |n| n == "model.json")?;
            let json = entries.first().map(|e| &e.1).ok_or("not a deep learning package (no model.json)")?;
            let mut m: DlModel = serde_json::from_slice(json).map_err(|e| format!("not a deep learning package: {e}"))?;
            m.model.validate()?;
            for c in &mut m.model.classes {
                *c = c.chars().take(200).collect();
            }
            // An imported model never replaces one already here: it gets a name of its own.
            let base: String = m.name.trim().chars().take(200).collect();
            let base = if base.is_empty() { "Imported model".to_string() } else { base };
            let taken = |s: &Session, n: &str| s.project.dl_models.iter().any(|x| x.name.eq_ignore_ascii_case(n));
            m.name = base.clone();
            let mut k = 1;
            while taken(s, &m.name) {
                k += 1;
                m.name = format!("{base} {k}");
            }
            m.accuracy = if m.accuracy.is_finite() { m.accuracy.clamp(0.0, 1.0) } else { 0.0 };
            let name = m.name.clone();
            store(s, m);
            Ok(json!({"model": name}))
        }).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files))
        .params(r#"{"path"?: "*.rgisdlpk", "data"?: base64}"#),
        C::new("dl.remove", "Delete Model", "Analyze › Image Classification", |s, p| {
            let i = model_index(s, p)?;
            s.project.dl_models.remove(i);
            Ok(Value::Null)
        }).roles(&[]).effects(crate::policy::Effects::NONE)
        .when(need_model)
        .params(r#"{"model": name}"#),
        C::new("imagery.hostedImagery", "Create Hosted Imagery", "Analyze › Imagery Sharing", hosted).roles(&[]).effects(crate::policy::Effects::NONE.with(crate::policy::Effect::Files)).view().when(need_raster).params(
            r#"{"layer": raster, "path": "*.zip" (z/x/y.png tiles, tiles.json and a viewer page), "minZoom"?: 0..22, "maxZoom"?: 0..22 (default: the raster's resolution)}"#,
        ),
    ]
}

/// A deep learning package's bytes: `data` (base64) or the regular file at `path`, within 256 MiB.
fn read_bytes(p: &Value) -> Result<Vec<u8>, String> {
    use base64::Engine;
    const MAX: u64 = 256 << 20;
    if let Some(d) = ps(p, "data") {
        if d.len() as u64 > MAX / 3 * 4 {
            return Err("the file is too large".into());
        }
        return base64::engine::general_purpose::STANDARD.decode(d.trim()).map_err(|_| "data must be base64".to_string());
    }
    let path = ps(p, "path").ok_or("path or data is required")?;
    crate::project::read_file(std::path::Path::new(path), MAX)
}

/// Adds a model, or replaces the one of the same name (retraining): true when it replaced one.
fn store(s: &mut Session, m: DlModel) -> bool {
    match s.project.dl_models.iter_mut().find(|x| x.name.eq_ignore_ascii_case(&m.name)) {
        Some(slot) => {
            *slot = m;
            true
        }
        None => {
            s.project.dl_models.push(m);
            false
        }
    }
}

fn model_index(input: &impl super::InputSource, p: &Value) -> Result<usize, String> {
    let context = input.input_context();
    let s = &context;
    let ms = &s.project.dl_models;
    match ps(p, "model") {
        Some(n) => ms.iter().position(|m| m.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no model '{n}'")),
        None => ms.len().checked_sub(1).ok_or_else(|| "train a model first".to_string()),
    }
}

/// Trains a model from the pixels under training polygons, with the rows of the training layer's
/// source that couldn't be read. Where a person at a terminal can stop it with Ctrl-C
/// ([`Session::stoppable`](crate::Session::stoppable)), it takes no work cap, as from the pane.
/// It runs where it is called, reading of a layer read in place, training and all: the app's
/// Deep Learning Tools train through `dl.classify`, which runs on a thread of its own; this is
/// the scripts', command line's and assistant's way to train without classifying. Called on the
/// app's thread (by the assistant), it holds the window while it reads: a training layer read
/// from a database or a web address is read whole first, for as long as that takes.
fn train(s: &Session, p: &Value) -> Result<(DlModel, super::whole::Unread), String> {
    let t = training(s, p)?;
    if s.stoppable {
        rustgis_analysis::run_under("Train Deep Learning Model", &rustgis_analysis::control::Control::stoppable(), || t.fit())
    } else {
        t.fit()
    }
}

/// A model to train: the raster and the training layer, taken of the session, and how to train it
/// ([`Training::fit`], which reads nothing more of the session and runs on any thread: the
/// training layer is read whole and the pixels under its polygons sampled there, as the network
/// learns).
struct Training {
    raster: std::sync::Arc<rustgis_data::raster::Raster>,
    bands: Vec<usize>,
    /// The training layer's rows, read whole in [`Training::fit`].
    rows: super::whole::Whole,
    /// The class field's index.
    field: usize,
    hidden: Vec<usize>,
    epochs: usize,
    name: String,
    /// No name was given: the name's base, for a name of its own when the model is kept (a model
    /// kept meanwhile may have taken this one).
    auto: Option<String>,
}

impl Training {
    /// Reads the training layer whole and trains the model: the model, with the rows of the
    /// training layer's source that couldn't be read (named in an error, as they may be its
    /// cause).
    fn fit(self) -> Result<(DlModel, super::whole::Unread), String> {
        let Training { raster, bands, rows, field, hidden, epochs, name, .. } = self;
        let (ds, unread) = rows.read()?;
        let model = Self::learn(&ds, field, &raster, &bands, &hidden, epochs, name).map_err(|e| unread.explain(e))?;
        Ok((model, unread))
    }

    /// The model learnt from the polygons of `ds` (their classes in field `field`) over `raster`.
    fn learn(
        ds: &rustgis_data::Dataset,
        field: usize,
        raster: &rustgis_data::raster::Raster,
        bands: &[usize],
        hidden: &[usize],
        epochs: usize,
        name: String,
    ) -> Result<DlModel, String> {
        let (classes, polygons) = training_polygons(ds, field)?;
        let (samples, per_class) = neural::training_samples(raster, bands, &polygons, classes.len(), MAX_SAMPLES_PER_CLASS)?;
        // Every class needs pixels of its own (its polygons may miss the raster or cover no data).
        if let Some((c, _)) = classes.iter().zip(&per_class).find(|(_, n)| **n < 5) {
            return Err(format!("the class '{c}' has fewer than 5 pixels with data under its polygons"));
        }
        let t = neural::train(bands, classes, &samples, hidden, epochs)?;
        Ok(DlModel { name, model: t.model, accuracy: t.accuracy, epochs: t.epochs })
    }
}

/// The classes of a training layer's polygons, in the order they come, and each polygon's outline
/// with its class and its share of the class's samples. A polygon with no class value trains
/// nothing.
fn training_polygons(ds: &rustgis_data::Dataset, fi: usize) -> Result<(Vec<String>, Vec<(Vec<Vec<Coord>>, usize, usize)>), String> {
    let mut classes: Vec<String> = Vec::new();
    let mut parts: Vec<(Vec<Vec<Coord>>, usize, usize)> = Vec::new();
    // Polygons per class, so each polygon of a class gets a fair share of its samples.
    let mut polygons: HashMap<String, usize> = HashMap::new();
    for (row, ft) in ds.features.iter().enumerate() {
        if let Some(Geometry::Polygon(polys)) = &ft.geometry {
            *polygons.entry(ds.value(row, fi).to_string()).or_default() += polys.len();
        }
    }
    for (row, ft) in ds.features.iter().enumerate() {
        let Some(Geometry::Polygon(polys)) = &ft.geometry else { continue };
        let v = ds.value(row, fi);
        if matches!(v, rustgis_data::Value::Null) || v.to_string().trim().is_empty() {
            continue;
        }
        let key = v.to_string();
        let share = (MAX_SAMPLES_PER_CLASS / polygons.get(&key).copied().unwrap_or(1).max(1)).max(1);
        let ci = match classes.iter().position(|c| *c == key) {
            Some(i) => i,
            None if classes.len() < neural::MAX_CLASSES => {
                classes.push(key);
                classes.len() - 1
            }
            None => return Err(format!("training holds at most {} classes", neural::MAX_CLASSES)),
        };
        parts.extend(polys.iter().map(|rings| (rings.clone(), ci, share)));
    }
    Ok((classes, parts))
}

/// What training takes of the session: the raster, the training layer (read whole when the model
/// is fitted), and the model's name.
fn training(s: &Session, p: &Value) -> Result<Training, String> {
    let r = raster(s, p)?;
    r.require_coordinates()?;
    let bands = bands_param(p, &r)?;
    let tl = super::imagery::training_name(p)?;
    let l = s
        .map()
        .layer_by_name(tl)
        .filter(|l| l.geometry_type() == GeometryType::Polygon)
        .ok_or_else(|| format!("no polygon layer '{tl}'"))?;
    let field = l.dataset.field_index(ps(p, "classField").ok_or("classField is required")?).ok_or("no such class field")?;
    let rows = super::whole::Whole::of(l);
    let hidden: Vec<usize> = match p.get("hidden").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(Value::as_u64).map(|u| u as usize).take(neural::MAX_LAYERS).collect(),
        None => vec![32, 16],
    };
    let epochs = f(p, "epochs").unwrap_or(60.0).clamp(1.0, neural::MAX_EPOCHS as f64) as usize;
    // Without a name given, a new model never replaces one: it gets a name of its own.
    let (name, auto) = match ps(p, "model") {
        Some(_) => (name(&json!({"name": p.get("model")}), format!("{}_model", r.name)), None),
        None => {
            let base = format!("{}_model", r.name);
            (free_name(s, &base), Some(base))
        }
    };
    Ok(Training { raster: r, bands, rows, field, hidden, epochs, name, auto })
}

/// `base`, or `base 2`, `base 3`… : the first no model of the project has (whatever its case).
fn free_name(s: &Session, base: &str) -> String {
    let taken = |n: &str| s.project.dl_models.iter().any(|x| x.name.eq_ignore_ascii_case(n));
    let mut n = base.to_string();
    let mut k = 1;
    while taken(&n) {
        k += 1;
        n = format!("{base} {k}");
    }
    n
}

fn classify(s: &mut Session, p: &Value) -> CmdResult {
    super::workflows::made_or_ran(s, p, "dl.classify")
}

/// The model a classification uses: one of the project's, or one it trains first.
enum Model {
    Saved(DlModel),
    Train(Training),
}

/// What a deep learning classification reads of the session (the raster, and the model or what
/// trains one), and the work that trains the model if need be and classifies the pixels. A model
/// trained is kept in the project once the classes are added.
pub(crate) fn classify_work(s: &Session, p: &Value) -> Result<super::workflows::Work, String> {
    let r = raster(s, p)?;
    r.require_coordinates()?;
    let model = if p.get("training").is_some() {
        Model::Train(training(s, p)?)
    } else {
        let i = model_index(s, p)?;
        Model::Saved(s.project.dl_models.get(i).cloned().ok_or("no model")?)
    };
    let out = name(p, format!("{}_dl", r.name));
    Ok(Box::new(move || {
        let (m, trained) = match model {
            Model::Saved(m) => (m, None),
            Model::Train(t) => {
                let auto = t.auto.clone();
                let (m, unread) = t.fit()?;
                (m, Some((auto, unread)))
            }
        };
        let (class, prob) = neural::classify(&r, &m.model, rasterops::NODATA)?;
        let mut counts = vec![0usize; m.model.classes.len()];
        for v in class.iter().filter(|v| **v >= 1.0) {
            if let Some(c) = counts.get_mut(*v as usize - 1) {
                *c += 1;
            }
        }
        let layer = class_layer(rasterops::derive(&r, &out, vec![class, prob])?)?;
        let legend: Vec<Value> =
            m.model.classes.iter().zip(&counts).enumerate().map(|(i, (c, n))| json!({"value": i + 1, "class": c, "pixels": n})).collect();
        let extra = json!({"model": m.name, "accuracy": m.accuracy, "classes": legend});
        // Kept once its classes are added. One that was given no name is named then, so it never
        // replaces a model kept while it trained.
        let then: Option<super::workflows::Then> = trained.map(|(auto, unread)| {
            Box::new(move |s: &mut Session, v: &mut Value| {
                unread.say(s);
                let mut m = m;
                if let Some(base) = auto {
                    m.name = free_name(s, &base);
                    v["model"] = json!(m.name);
                }
                v["replaced"] = json!(store(s, m));
            }) as super::workflows::Then
        });
        Ok(super::workflows::made_layer(out, layer, Some(extra), then))
    }))
}

/// A deep learning package: the model and a description of its inputs and classes.
pub fn package(s: &Session, p: &Value) -> Result<Vec<u8>, String> {
    let i = model_index(s, p)?;
    let m = s.project.dl_models.get(i).ok_or("no model")?;
    let model = serde_json::to_vec_pretty(m).map_err(|e| e.to_string())?;
    let about = json!({
        "format": "RustGIS deep learning package",
        "version": 1,
        "modelType": "pixel classification (multilayer perceptron)",
        "name": m.name,
        "bands": m.model.bands.iter().map(|b| b + 1).collect::<Vec<_>>(),
        "classes": m.model.classes.iter().enumerate().map(|(i, c)| json!({"value": i + 1, "name": c})).collect::<Vec<_>>(),
        "hiddenLayers": m.model.layers.iter().take(m.model.layers.len().saturating_sub(1)).map(|l| l.out).collect::<Vec<_>>(),
        "accuracy": m.accuracy,
    });
    let about = serde_json::to_vec_pretty(&about).map_err(|e| e.to_string())?;
    rustgis_data::zip::write(&[("model.json".into(), model), ("package.json".into(), about)])
}

/// Web Mercator tile bounds (metres) of tile (z, x, y).
fn tile_bounds(z: u32, x: u32, y: u32) -> (f64, f64, f64, f64) {
    let world = 2.0 * std::f64::consts::PI * rustgis_geom::MERCATOR_R;
    let size = world / f64::from(1u32 << z.min(30));
    let min_x = -world / 2.0 + f64::from(x) * size;
    let max_y = world / 2.0 - f64::from(y) * size;
    (min_x, max_y - size, min_x + size, max_y)
}

/// Cuts a raster into XYZ web map tiles (PNG, transparent outside the raster).
fn hosted(s: &mut Session, p: &Value) -> CmdResult {
    let path = ps(p, "path").ok_or("path is required")?.to_string();
    let (bytes, mut info) = hosted_bytes(s, p)?;
    rustgis_data::io::write_atomic(Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
    info["path"] = json!(path);
    Ok(info)
}

/// The tiles of a raster as a zip, and what's in it.
pub fn hosted_bytes(s: &mut Session, p: &Value) -> Result<(Vec<u8>, Value), String> {
    let r = raster(s, p)?;
    let lname = ps(p, "layer").unwrap_or_default().to_string();
    let mut layer = s.map().layer_by_name(&lname).cloned().ok_or("no such layer")?;
    // Tiled even when hidden on the map.
    layer.visible = true;
    // The raster's extent in longitude/latitude, from its corners and edges.
    let mut e = rustgis_geom::Extent::EMPTY;
    let tr = r.transformer();
    let steps = 16;
    for i in 0..=steps {
        let t = f64::from(i) / f64::from(steps);
        for (c, rr) in [(t, 0.0), (t, 1.0), (0.0, t), (1.0, t)] {
            let w = r.transform.to_world(c * f64::from(r.width), rr * f64::from(r.height));
            if let Some(ll) = tr.to_lonlat(w) {
                e.expand(ll);
            }
        }
    }
    if e.is_empty() {
        return Err("the raster has no place on the map".into());
    }
    e.min_y = e.min_y.max(-85.0);
    e.max_y = e.max_y.min(85.0);
    // The zoom whose pixels best match the raster's, unless given.
    let world = 2.0 * std::f64::consts::PI * rustgis_geom::MERCATOR_R;
    let (a, b) = (rustgis_geom::lonlat_to_merc(Coord::new(e.min_x, e.min_y)), rustgis_geom::lonlat_to_merc(Coord::new(e.max_x, e.max_y)));
    let px = ((b.x - a.x) / f64::from(r.width.max(1))).max(1e-6);
    let native = (world / (f64::from(TILE) * px)).log2().round().clamp(0.0, 22.0) as u32;
    let max_z = f(p, "maxZoom").map_or(native, |z| z.clamp(0.0, 22.0) as u32);
    let min_z = f(p, "minZoom").map_or(max_z.saturating_sub(4), |z| z.clamp(0.0, 22.0) as u32).min(max_z);
    let tile_of = |z: u32, lon: f64, lat: f64| -> (u32, u32) {
        let m = rustgis_geom::lonlat_to_merc(Coord::new(lon, lat));
        let n = f64::from(1u32 << z);
        let x = ((m.x + world / 2.0) / world * n).floor().clamp(0.0, n - 1.0) as u32;
        let y = ((world / 2.0 - m.y) / world * n).floor().clamp(0.0, n - 1.0) as u32;
        (x, y)
    };
    let mut jobs = Vec::new();
    for z in min_z..=max_z {
        let (x0, y0) = tile_of(z, e.min_x, e.max_y);
        let (x1, y1) = tile_of(z, e.max_x, e.min_y);
        for x in x0..=x1 {
            for y in y0..=y1 {
                jobs.push((z, x, y));
                if jobs.len() > MAX_TILES {
                    return Err(format!("that's more than {MAX_TILES} tiles; lower maxZoom"));
                }
            }
        }
    }
    // A map holding only this raster, on a blank basemap, draws each tile.
    let mut map = rustgis_map::Map::new("tiles");
    map.basemap = rustgis_map::Basemap::Blank;
    map.graticule = false;
    map.layers = vec![layer];
    let mut entries = Vec::new();
    let mut held = 0usize;
    for (z, x, y) in &jobs {
        let (min_x, min_y, max_x, max_y) = tile_bounds(*z, *x, *y);
        let camera = rustgis_map::Camera {
            center: Coord::new((min_x + max_x) / 2.0, (min_y + max_y) / 2.0),
            res: (max_x - min_x) / f64::from(TILE),
            bearing: 0.0,
            pitch: 0.0,
        };
        let view = rustgis_render::View { width: f64::from(TILE), height: f64::from(TILE), pixels_per_point: 1.0, camera, sky: false };
        let Some(mut img) = rustgis_render::render(&mut map, &view).map(|r| r.pixmap) else { continue };
        // Transparent where the raster has no data.
        let mut any = false;
        for (k, px) in img.pixels_mut().iter_mut().enumerate() {
            let (col, row) = ((k as u32 % TILE) as f64 + 0.5, (k as u32 / TILE) as f64 + 0.5);
            let m = Coord::new(min_x + col * (max_x - min_x) / f64::from(TILE), max_y - row * (max_y - min_y) / f64::from(TILE));
            let ll = rustgis_geom::merc_to_lonlat(m);
            let covered = r.pixel_at(ll).is_some_and(|(c, rw)| (0..r.band_count()).any(|b| r.value(b, c, rw).is_some()));
            if covered {
                any = true;
            } else {
                *px = tiny_skia::PremultipliedColorU8::TRANSPARENT;
            }
        }
        if any {
            let png = img.encode_png().map_err(|e| e.to_string())?;
            held += png.len();
            if held > MAX_TILE_BYTES {
                return Err(format!("the tiles passed {} MB at zoom {z}; lower maxZoom", MAX_TILE_BYTES >> 20));
            }
            entries.push((format!("tiles/{z}/{x}/{y}.png"), png));
        }
    }
    let c = e.center();
    let tilejson = json!({
        "tilejson": "3.0.0",
        "name": lname,
        "tiles": ["tiles/{z}/{x}/{y}.png"],
        "minzoom": min_z,
        "maxzoom": max_z,
        "bounds": [e.min_x, e.min_y, e.max_x, e.max_y],
        "center": [c.x, c.y, min_z],
    });
    let n = entries.len();
    entries.push(("tiles.json".into(), serde_json::to_vec_pretty(&tilejson).map_err(|e| e.to_string())?));
    entries.push(("index.html".into(), viewer(&lname, min_z, max_z, c).into_bytes()));
    let bytes = rustgis_data::zip::write(&entries)?;
    Ok((bytes, json!({"tiles": n, "minZoom": min_z, "maxZoom": max_z})))
}

/// A small page showing the tiles (no libraries): drag to pan, wheel to zoom.
fn viewer(name: &str, min_z: u32, max_z: u32, c: Coord) -> String {
    let title = name.replace('&', "&amp;").replace('<', "&lt;");
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>{title}</title>
<style>html,body{{margin:0;height:100%;overflow:hidden;background:#e5e9ef;font-family:system-ui,sans-serif}}#m{{position:absolute;inset:0}}#m img{{position:absolute;width:256px;height:256px}}#z{{position:fixed;left:8px;bottom:8px;background:#fffc;padding:2px 6px;border-radius:4px;font-size:12px}}</style></head>
<body><div id="m"></div><div id="z"></div><script>
const minZ={min_z},maxZ={max_z},m=document.getElementById('m');let z=minZ,cx={cx},cy={cy};
function px(lon,lat,z){{const n=256*2**z;const s=Math.sin(lat*Math.PI/180);return [(lon+180)/360*n,(0.5-Math.log((1+s)/(1-s))/(4*Math.PI))*n]}}
let [x,y]=px(cx,cy,z);
function draw(){{m.innerHTML='';const w=innerWidth,h=innerHeight,n=2**z;const x0=Math.floor((x-w/2)/256),y0=Math.floor((y-h/2)/256);
for(let tx=x0;tx<=Math.floor((x+w/2)/256);tx++)for(let ty=y0;ty<=Math.floor((y+h/2)/256);ty++){{if(tx<0||ty<0||tx>=n||ty>=n)continue;const i=new Image();i.onerror=()=>i.remove();i.src=`tiles/${{z}}/${{tx}}/${{ty}}.png`;i.style.left=(tx*256-x+w/2)+'px';i.style.top=(ty*256-y+h/2)+'px';m.appendChild(i)}}
document.getElementById('z').textContent='zoom '+z}}
let drag=null;m.onmousedown=e=>drag=[e.clientX,e.clientY];onmouseup=()=>drag=null;
onmousemove=e=>{{if(drag){{x-=e.clientX-drag[0];y-=e.clientY-drag[1];drag=[e.clientX,e.clientY];draw()}}}};
onwheel=e=>{{const nz=Math.max(minZ,Math.min(maxZ,z+(e.deltaY<0?1:-1)));if(nz!==z){{const k=2**(nz-z);x*=k;y*=k;z=nz;draw()}}}};onresize=draw;draw();
</script></body></html>
"#,
        cx = c.x,
        cy = c.y
    )
}

/// Binds both raster and training polygons through their shared literal selectors.
fn training_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    let s = input;
    super::Inputs::named(s.map(), [super::imagery::raster_name(p)?, super::imagery::training_name(p)?])
}
/// Follows classification's training-or-saved-model branch without copying trained weights.
fn classify_inputs(input: &super::InputContext<'_>, p: &Value) -> Result<super::Inputs, String> {
    if p.get("training").is_some() {
        return training_inputs(input, p);
    }
    let s = input;
    let mut inputs = super::Inputs::named(s.map(), [super::imagery::raster_name(p)?])?;
    let i = model_index(input, p)?;
    inputs.state = json!({"model":s.project.dl_models.get(i).map(|m|&m.name),"index":i});
    Ok(inputs)
}
