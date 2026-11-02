//! Publish › Share As, Manage and Status: web maps, layers, styles, scenes and link charts as
//! self-contained open-format files (HTML pages that need no server or library, OGC API Features
//! static collections, MapLibre style JSON, glTF), locators for place search, geodata services,
//! and web tools. Each share is recorded as a shared item (Status › Jobs) that the Manage
//! commands update in place, written to a path or a registered data store.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use base64::Engine;
use rustgis_data::{Dataset, Geometry, GeometryType};
use rustgis_geom::Coord;
use rustgis_map::symbology::{MAX_LABEL_CLASSES, Renderer};
use rustgis_map::{Layer, Map, Rgba};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::ps;
use crate::{CmdResult, CommandSpec as C, Session};

/// The kinds of share: (kind, extension, label).
pub const KINDS: &[(&str, &str, &str)] = &[
    ("webMap", "html", "Web map"),
    ("webLayer", "zip", "Web layer"),
    ("webStyle", "json", "Web style"),
    ("locator", "rgisloc", "Locator"),
    ("geodataService", "zip", "Geodata service"),
    ("webTool", "rgistool", "Web tool"),
    ("webScene", "gltf", "Web scene"),
    ("linkChart", "html", "Web link chart"),
];

/// Most features a shared web map, scene or link chart carries.
const MAX_FEATURES: usize = 200_000;
/// Largest total size of the raster pictures embedded in a web map page.
const MAX_PAGE_PICTURES: usize = 64 << 20;
/// Largest glTF geometry buffer of a web scene.
const MAX_SCENE_BYTES: usize = 128 << 20;
/// Most nodes in a link chart.
const MAX_NODES: usize = 3000;
/// Most links in a link chart (its page lays them all out every frame).
const MAX_LINKS: usize = 20_000;
/// Most shared items and data stores kept.
const MAX_ITEMS: usize = 1000;
/// Largest file an upload copies.
const MAX_UPLOAD: u64 = 256 << 20;

/// A finished share: what it was, from what, where it went.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SharedItem {
    pub id: u32,
    pub kind: String,
    pub name: String,
    /// The map, layer, model or relationship it was made from.
    pub source: String,
    pub path: String,
    pub created: f64,
    pub updated: f64,
    pub bytes: usize,
    /// Written in this session. Items listed by an opened project carry paths from its file,
    /// so the Manage commands don't write to them until they're shared again.
    #[serde(skip)]
    pub this_session: bool,
}

/// A folder shares are written to when no path is given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DataStore {
    pub name: String,
    pub path: String,
    /// Registered in this session. A store listed by an opened project names a folder chosen by
    /// its file, so nothing is written there until it's registered again.
    #[serde(skip)]
    pub this_session: bool,
}

/// A data store shares may be written to: one registered in this session.
fn usable(store: &DataStore) -> Result<(), String> {
    if store.this_session {
        Ok(())
    } else {
        Err(format!(
            "the data store '{}' came with the opened project; register it again (Publish › Manage › Data Stores) to write to {}",
            store.name, store.path
        ))
    }
}

fn now() -> f64 {
    web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn safe(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_') { c } else { '_' }).take(100).collect();
    if s.is_empty() { "shared".into() } else { s }
}

fn need_items(s: &Session) -> Option<&'static str> {
    if s.project.shared.is_empty() { Some("share something first (Publish › Share As)") } else { None }
}

fn web_map(s: &mut Session, p: &Value) -> CmdResult {
    share(s, p, "webMap")
}
fn web_layer(s: &mut Session, p: &Value) -> CmdResult {
    share(s, p, "webLayer")
}
fn web_style_cmd(s: &mut Session, p: &Value) -> CmdResult {
    share(s, p, "webStyle")
}
fn locator_cmd(s: &mut Session, p: &Value) -> CmdResult {
    share(s, p, "locator")
}
fn geodata(s: &mut Session, p: &Value) -> CmdResult {
    share(s, p, "geodataService")
}
fn web_tool(s: &mut Session, p: &Value) -> CmdResult {
    share(s, p, "webTool")
}
fn web_scene(s: &mut Session, p: &Value) -> CmdResult {
    share(s, p, "webScene")
}
fn web_link_chart(s: &mut Session, p: &Value) -> CmdResult {
    share(s, p, "linkChart")
}
const MANAGE: &str = r#"{"item"?: id|name (default: the last of its kind), "map"?: re-share from another map}"#;

pub fn commands() -> Vec<C> {
    let share_as = |id: &'static str, label: &'static str, run: fn(&mut Session, &Value) -> CmdResult, params: &'static str| {
        C::new(id, label, "Publish › Share As", run).params(params)
    };
    vec![
        share_as("share.webMap", "Web Map", web_map, r#"{"map"?: name (default: the active map), "path"?: "*.html" (default: the first data store)}"#),
        share_as("share.webLayer", "Web Layer", web_layer, r#"{"layer"?: name, "path"?: "*.zip" (an OGC API Features collection with its style and a viewer)}"#),
        share_as("share.webStyle", "Web Style", web_style_cmd, r#"{"layer"?: name, "path"?: "*.json" (a MapLibre style)}"#),
        share_as("share.locator", "Locator", locator_cmd, r#"{"layer"?: name, "fields"?: [fields searched] (default: text fields), "path"?: "*.rgisloc"}"#),
        share_as("share.geodataService", "Geodata Service", geodata, r#"{"map"?: name, "path"?: "*.zip" (every layer and table as OGC API Features collections)}"#),
        share_as("share.webTool", "Web Tool", web_tool, r#"{"model"?: name (default: the last model), "path"?: "*.rgistool"}"#),
        share_as("share.webScene", "Web Scene", web_scene, r#"{"map"?: name, "path"?: "*.gltf" (polygons extruded by their layer's extrusion)}"#),
        share_as("share.linkChart", "Web Link Chart", web_link_chart, r#"{"relationship"?: name (default: every relationship), "path"?: "*.html"}"#),
        C::new("share.jobs", "Jobs", "Publish › Share As", |s, _| {
            let items: Vec<Value> = s.project.shared.iter().rev().map(|i| serde_json::to_value(i).unwrap_or(Value::Null)).collect();
            Ok(json!({"items": items, "stores": s.project.data_stores}))
        })
        .view(),
        C::new("share.saveWebMap", "Save Web Map", "Publish › Manage", |s, p| update_item(s, p, "webMap", ps(p, "map"))).when(need_items).params(MANAGE),
        C::new("share.replaceWebLayer", "Replace Web Layer", "Publish › Manage", |s, p| update_item(s, p, "webLayer", ps(p, "layer")))
            .when(need_items)
            .params(r#"{"item"?: id|name (default: the last web layer), "layer"?: the layer replacing it (default: the one it was made from)}"#),
        C::new("share.dataStore", "Data Stores", "Publish › Manage", data_store).params(r#"{"name"?, "path"?: folder, "remove"?: name (no parameters: list them)}"#),
        C::new("share.upload", "Upload File", "Publish › Manage", upload).params(r#"{"path": file, "store"?: name (default: the first data store)}"#),
        C::new("share.saveWebScene", "Save Web Scene", "Publish › Manage", |s, p| update_item(s, p, "webScene", ps(p, "map"))).when(need_items).params(MANAGE),
        C::new("share.saveWebLinkChart", "Save Web Link Chart", "Publish › Manage", |s, p| update_item(s, p, "linkChart", ps(p, "relationship")))
            .when(need_items)
            .params(r#"{"item"?: id|name (default: the last link chart), "relationship"?: re-share another relationship}"#),
        C::new("locator.find", "Find Place", "Home › Inspect", find).params(r#"{"locator"?: shared item name or *.rgisloc path (default: the last locator), "text": search text, "limit"?: 1..100, "zoom"?: bool (go to the best match)}"#),
        C::new("model.import", "Import Model", "Analyze › Geoprocessing", |s, p| {
            let text = super::items::read_text(p)?;
            let v: Value = serde_json::from_str(&text).map_err(|e| format!("not a RustGIS web tool: {e}"))?;
            let name = v["name"].as_str().unwrap_or("Tool").chars().take(200).collect::<String>();
            // A model file (`*.model.json`) holds a graph; a web tool its steps.
            if let Some(graph) = v.get("graph") {
                return s.run("model.new", &json!({"name": name, "graph": graph, "imported": true}));
            }
            let steps = v.get("steps").cloned().ok_or("not a RustGIS model or web tool (no graph or steps)")?;
            s.run("model.new", &json!({"name": name, "steps": steps, "imported": true}))
        })
        .params(r#"{"path"?: "*.model.json" | "*.rgistool", "text"?: file JSON, "data"?: base64 file}"#),
    ]
}

/// The shared item bytes of a kind: its name, bytes and source.
pub fn share_bytes(s: &mut Session, kind: &str, p: &Value) -> Result<(String, Vec<u8>, String), String> {
    match kind {
        "webMap" => {
            let mut m = map_of(s, p)?;
            m.layers.iter_mut().filter(|l| l.visible).try_for_each(load_whole)?;
            let html = web_map_html(&m, &m.name)?;
            Ok((m.name.clone(), html.into_bytes(), m.name))
        }
        "webLayer" => {
            let mut l = layer_of(s, p)?;
            load_whole(&mut l)?;
            Ok((l.name.clone(), web_layer_zip(s, &l)?, l.name.clone()))
        }
        "webStyle" => {
            let l = layer_of(s, p)?;
            let style = web_style(&l);
            Ok((l.name.clone(), serde_json::to_vec_pretty(&style).map_err(|e| e.to_string())?, l.name.clone()))
        }
        "locator" => {
            let l = layer_of(s, p)?;
            let fields: Option<Vec<String>> =
                p.get("fields").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect());
            let loc = locator(&l, fields.as_deref())?;
            Ok((l.name.clone(), serde_json::to_vec(&loc).map_err(|e| e.to_string())?, l.name.clone()))
        }
        "geodataService" => {
            let mut m = map_of(s, p)?;
            m.layers.iter_mut().filter(|l| !l.is_raster() && !l.graphics).try_for_each(load_whole)?;
            let mut layers: Vec<&Dataset> = m.layers.iter().filter(|l| !l.is_raster() && !l.graphics).map(|l| &l.dataset).collect();
            layers.extend(s.project.tables.iter().map(|t| &t.dataset));
            Ok((m.name.clone(), ogc_zip(&m.name, &layers, &[])?, m.name.clone()))
        }
        "webTool" => {
            let ms = &s.project.models;
            let m = match ps(p, "model") {
                Some(n) => ms.iter().find(|m| m.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no model '{n}'"))?,
                None => ms.last().ok_or("make a model first (ModelBuilder)")?,
            };
            let params: Vec<Value> = m
                .steps
                .iter()
                .enumerate()
                .flat_map(|(i, st)| {
                    st.params
                        .as_object()
                        .into_iter()
                        .flatten()
                        .map(move |(k, v)| json!({"step": i + 1, "tool": st.tool, "name": k, "default": v}))
                })
                .collect();
            let tool = json!({"format": "RustGIS web tool", "version": 1, "name": m.name, "steps": m.steps, "parameters": params});
            Ok((m.name.clone(), serde_json::to_vec_pretty(&tool).map_err(|e| e.to_string())?, m.name.clone()))
        }
        "webScene" => {
            let mut m = map_of(s, p)?;
            m.layers.iter_mut().filter(|l| l.visible && !l.is_raster()).try_for_each(load_whole)?;
            Ok((m.name.clone(), gltf(&m, s.view_size)?, m.name.clone()))
        }
        "linkChart" => {
            let rel = ps(p, "relationship").map(str::to_string);
            let html = link_chart(s, rel.as_deref())?;
            // Every relationship is recorded with an empty source (a relationship may be called
            // "Relationships").
            let name = rel.clone().unwrap_or_else(|| "Relationships".into());
            Ok((name, html.into_bytes(), rel.unwrap_or_default()))
        }
        k => Err(format!("unknown share '{k}'")),
    }
}

/// A map to share, its layer groups folded into their layers (the shared formats have none).
fn map_of(s: &Session, p: &Value) -> Result<Map, String> {
    let mut m = match ps(p, "map") {
        Some(n) => s.project.maps.iter().find(|m| m.name.eq_ignore_ascii_case(n)).cloned().ok_or_else(|| format!("no map '{n}'"))?,
        None => s.map().clone(),
    };
    m.bake_groups();
    Ok(m)
}

/// A streamed layer shares its whole source, not the features read for the current view.
fn load_whole(l: &mut Layer) -> Result<(), String> {
    if l.is_streamed() {
        l.dataset = l.load_all()?;
    }
    Ok(())
}

fn layer_of(s: &Session, p: &Value) -> Result<Layer, String> {
    let id = s.layer_id(p)?;
    let l = s.map().layer(id).cloned().ok_or("no such layer")?;
    if l.is_raster() {
        return Err("this shares feature layers, not rasters".into());
    }
    Ok(l)
}

/// Where a share goes: `path`, else the named or first data store.
fn target(s: &Session, p: &Value, name: &str, ext: &str) -> Result<String, String> {
    if let Some(path) = ps(p, "path") {
        return Ok(path.to_string());
    }
    let store = match ps(p, "store") {
        Some(n) => s.project.data_stores.iter().find(|d| d.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no data store '{n}'"))?,
        None => s.project.data_stores.first().ok_or("pass a path, or register a data store (Publish › Manage › Data Stores)")?,
    };
    usable(store)?;
    Ok(PathBuf::from(&store.path).join(format!("{}.{ext}", safe(name))).display().to_string())
}

fn share(s: &mut Session, p: &Value, kind: &str) -> CmdResult {
    let (name, bytes, source) = share_bytes(s, kind, p)?;
    let ext = KINDS.iter().find(|k| k.0 == kind).map_or("bin", |k| k.1);
    let path = target(s, p, &name, ext)?;
    // A share of another kind (or an uploaded file) at that path isn't replaced.
    if let Some(other) = s.project.shared.iter().find(|i| i.path == path && i.kind != kind) {
        return Err(format!("{path} already holds the shared {} '{}'; choose another name or path", other.kind, other.name));
    }
    rustgis_data::io::write_atomic(Path::new(&path), &bytes).map_err(|e| format!("{path}: {e}"))?;
    let t = now();
    let shared = &mut s.project.shared;
    let id = match shared.iter_mut().find(|i| i.path == path) {
        Some(i) => {
            (i.kind, i.name, i.source, i.updated, i.bytes, i.this_session) = (kind.to_string(), name.clone(), source, t, bytes.len(), true);
            i.id
        }
        None => {
            let id = shared.iter().map(|i| i.id).max().unwrap_or(0).saturating_add(1);
            if shared.len() >= MAX_ITEMS {
                shared.remove(0);
            }
            shared.push(SharedItem {
                id,
                kind: kind.to_string(),
                name: name.clone(),
                source,
                path: path.clone(),
                created: t,
                updated: t,
                bytes: bytes.len(),
                this_session: true,
            });
            id
        }
    };
    s.notify(format!("Shared {name} to {path}"), false);
    // After it, so the status bar, which shows the last notice, keeps saying it.
    note_label_classes(s, kind, p);
    note_export(s, kind, p, &path);
    Ok(json!({"item": id, "path": path, "bytes": bytes.len()}))
}

/// Re-shares an item in place, from its source or a new one.
fn update_item(s: &mut Session, p: &Value, kind: &str, new_source: Option<&str>) -> CmdResult {
    let item = match p.get("item") {
        Some(Value::Number(n)) => s.project.shared.iter().find(|i| Some(u64::from(i.id)) == n.as_u64() && i.kind == kind),
        Some(Value::String(n)) => s.project.shared.iter().rev().find(|i| i.kind == kind && i.name.eq_ignore_ascii_case(n)),
        _ => s.project.shared.iter().rev().find(|i| i.kind == kind),
    }
    .cloned()
    .ok_or_else(|| format!("no shared {} to update", KINDS.iter().find(|k| k.0 == kind).map_or(kind, |k| k.2).to_ascii_lowercase()))?;
    if !item.this_session {
        return Err(format!(
            "'{}' came with the opened project; share it again with Share As (choosing where it goes) before updating it",
            item.name
        ));
    }
    let source = new_source.unwrap_or(&item.source).to_string();
    let q = match kind {
        "webLayer" => json!({"layer": source, "path": item.path}),
        "linkChart" => json!({"relationship": if source.is_empty() { Value::Null } else { json!(source) }, "path": item.path}),
        _ => json!({"map": source, "path": item.path}),
    };
    share(s, &q, kind)
}

fn data_store(s: &mut Session, p: &Value) -> CmdResult {
    if let Some(n) = ps(p, "remove") {
        let before = s.project.data_stores.len();
        s.project.data_stores.retain(|d| !d.name.eq_ignore_ascii_case(n));
        if s.project.data_stores.len() == before {
            return Err(format!("no data store '{n}'"));
        }
    } else if let Some(path) = ps(p, "path") {
        if !Path::new(path).is_dir() {
            return Err(format!("{path} isn't a folder"));
        }
        let name: String = ps(p, "name")
            .map(str::to_string)
            .unwrap_or_else(|| Path::new(path).file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned()))
            .chars()
            .take(200)
            .collect();
        // Registering again a store that came with an opened project confirms it.
        s.project.data_stores.retain(|d| d.this_session || !d.name.eq_ignore_ascii_case(&name));
        if s.project.data_stores.iter().any(|d| d.name.eq_ignore_ascii_case(&name)) {
            return Err(format!("a data store is already called '{name}'"));
        }
        if s.project.data_stores.len() >= MAX_ITEMS {
            return Err("too many data stores".into());
        }
        s.project.data_stores.push(DataStore { name, path: path.to_string(), this_session: true });
    }
    Ok(json!({"stores": s.project.data_stores}))
}

fn upload(s: &mut Session, p: &Value) -> CmdResult {
    let src = ps(p, "path").ok_or("path is required")?;
    let file = Path::new(src).file_name().map(|n| n.to_string_lossy().into_owned()).ok_or("path must name a file")?;
    let store = match ps(p, "store") {
        Some(n) => s.project.data_stores.iter().find(|d| d.name.eq_ignore_ascii_case(n)).ok_or_else(|| format!("no data store '{n}'"))?,
        None => s.project.data_stores.first().ok_or("register a data store first (Publish › Manage › Data Stores)")?,
    }
    .clone();
    usable(&store)?;
    let dest = PathBuf::from(&store.path).join(&file);
    // A file of that name in the store (another upload, or a share) is never overwritten.
    if dest.exists() {
        return Err(format!("{} already has a file named {file}", store.name));
    }
    // create_new is the real guard (the check above only gives a clearer message): the file is
    // created new, never through an existing file or link. A copy that fails part way is removed,
    // so a retry can write it.
    let from = std::fs::File::open(src).map_err(|e| format!("{src}: {e}"))?;
    if !from.metadata().is_ok_and(|m| m.len() <= MAX_UPLOAD) {
        return Err(format!("{src}: uploads are at most {} MB", MAX_UPLOAD >> 20));
    }
    let mut from = std::io::Read::take(from, MAX_UPLOAD);
    let mut to = std::fs::OpenOptions::new().write(true).create_new(true).open(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let bytes = match std::io::copy(&mut from, &mut to) {
        Ok(n) => n,
        Err(e) => {
            drop(to);
            let _ = std::fs::remove_file(&dest);
            return Err(format!("{src}: {e}"));
        }
    };
    let t = now();
    let id = s.project.shared.iter().map(|i| i.id).max().unwrap_or(0).saturating_add(1);
    let path = dest.display().to_string();
    if s.project.shared.len() >= MAX_ITEMS {
        s.project.shared.remove(0);
    }
    s.project.shared.push(SharedItem {
        id,
        kind: "file".into(),
        name: file,
        source: src.to_string(),
        path: path.clone(),
        created: t,
        updated: t,
        bytes: bytes as usize,
        this_session: true,
    });
    Ok(json!({"item": id, "path": path, "bytes": bytes}))
}

fn html_escape(t: &str) -> String {
    t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// JSON for embedding in a `<script>`: `</` can't close the script.
fn script_json(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default().replace("</", "<\\/").replace("<!--", "<\\!--")
}

fn css(c: Rgba) -> String {
    format!("rgba({},{},{},{:.3})", c.r(), c.g(), c.b(), f64::from(c.a()) / 255.0)
}

/// A layer's visible features as GeoJSON, each with its drawn style.
fn styled_features(l: &Layer, budget: &mut usize) -> Vec<Value> {
    let rows = l.visible_rows();
    let opacity = 1.0 - f64::from(l.transparency.clamp(0.0, 100.0)) / 100.0;
    let mut out = Vec::new();
    // Its symbol by expression (no map scale: the page zooms on its own).
    let styler = l.styler(None);
    for (row, ft) in l.dataset.features.iter().enumerate() {
        if *budget == 0 {
            break;
        }
        if rows.get(row) != Some(&true) {
            continue;
        }
        let Some(g) = &ft.geometry else { continue };
        let sym = styler.symbol(row);
        let props: serde_json::Map<String, Value> =
            l.dataset.fields.iter().enumerate().map(|(i, f)| (f.name.clone(), json!(l.dataset.value(row, i).to_string()))).collect();
        out.push(json!({
            "type": "Feature",
            "geometry": rustgis_data::io::geometry_to_geojson(g),
            "properties": props,
            "style": {"fill": css(sym.fill), "stroke": css(sym.stroke), "width": sym.stroke_width, "size": sym.size, "opacity": opacity},
        }));
        *budget -= 1;
    }
    out
}

/// A self-contained web map page: the map's layers (rasters as images) drawn on a canvas in Web
/// Mercator, with pan, zoom, a layer list and pop-ups. No libraries or server.
fn web_map_html(m: &Map, title: &str) -> Result<String, String> {
    let mut budget = MAX_FEATURES;
    let mut pictures = 0usize;
    let mut layers = Vec::new();
    // Bottom-most first, as drawn.
    for l in m.layers.iter().rev().filter(|l| l.visible) {
        if l.is_raster() {
            let e = l.extent();
            if e.is_empty() {
                continue;
            }
            let mut only = Map::new("raster");
            only.basemap = rustgis_map::Basemap::Blank;
            only.graticule = false;
            only.layers = vec![l.clone()];
            let (a, b) = (
                rustgis_geom::lonlat_to_merc(Coord::new(e.min_x, e.min_y.max(-85.0))),
                rustgis_geom::lonlat_to_merc(Coord::new(e.max_x, e.max_y.min(85.0))),
            );
            let (w, h) = ((b.x - a.x).abs(), (b.y - a.y).abs());
            let px = 1600.0f64;
            let (vw, vh) = if w >= h { (px, (px * h / w.max(1e-9)).max(1.0)) } else { ((px * w / h.max(1e-9)).max(1.0), px) };
            let camera =
                rustgis_map::Camera { center: Coord::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0), res: w / vw, bearing: 0.0, pitch: 0.0 };
            let view = rustgis_render::View { width: vw, height: vh, pixels_per_point: 1.0, camera, sky: false };
            let png = rustgis_render::render_png(&mut only, &view)?;
            pictures += png.len();
            if pictures > MAX_PAGE_PICTURES {
                return Err("the map's rasters would make the page too large; hide some of them".into());
            }
            layers.push(
                json!({"name": l.name, "image": base64::engine::general_purpose::STANDARD.encode(png), "bounds": [a.x, a.y, b.x, b.y]}),
            );
        } else {
            layers.push(json!({"name": l.name, "features": styled_features(l, &mut budget)}));
        }
    }
    let c = m.center_lonlat();
    let bg = css(m.palette().0);
    Ok(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{title}</title>
<style>html,body{{margin:0;height:100%;overflow:hidden;font-family:system-ui,sans-serif}}canvas{{display:block}}
#panel{{position:fixed;top:8px;right:8px;background:#fffe;border-radius:6px;padding:6px 10px;font-size:13px;max-height:60vh;overflow:auto;box-shadow:0 1px 4px #0003}}
#pop{{position:fixed;display:none;background:#fff;border-radius:6px;padding:6px 8px;font-size:12px;box-shadow:0 1px 6px #0004;max-width:320px;max-height:40vh;overflow:auto}}
h1{{position:fixed;left:10px;top:4px;margin:0;font-size:16px;background:#fffc;padding:2px 8px;border-radius:4px}}</style></head>
<body><h1>{title}</h1><canvas id="c"></canvas><div id="panel"></div><div id="pop"></div><script>
const L={layers};const R=6378137,cv=document.getElementById('c'),g=cv.getContext('2d');
const merc=(lon,lat)=>{{lat=Math.max(-85,Math.min(85,lat));return [R*lon*Math.PI/180,R*Math.log(Math.tan(Math.PI/4+lat*Math.PI/360))]}};
let [cx,cy]=merc({lon},{lat}),res={res};const on=L.map(()=>true);const imgs=L.map(l=>{{if(!l.image)return null;const i=new Image();i.onload=draw;i.src='data:image/png;base64,'+l.image;return i}});
const esc=s=>String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;');const P=document.getElementById('panel');L.slice().reverse().forEach((l,ri)=>{{const k=L.length-1-ri;const d=document.createElement('label');d.style.display='block';d.innerHTML='<input type=checkbox checked> '+esc(l.name);d.firstChild.onchange=e=>{{on[k]=e.target.checked;draw()}};P.appendChild(d)}});
const sx=x=>(x-cx)/res+cv.width/2,sy=y=>(cy-y)/res+cv.height/2;
function path(ring){{ring.forEach((p,i)=>{{const [x,y]=merc(p[0],p[1]);i?g.lineTo(sx(x),sy(y)):g.moveTo(sx(x),sy(y))}})}}
function geom(G,st){{const t=G.type,c=G.coordinates;g.globalAlpha=st.opacity;g.fillStyle=st.fill;g.strokeStyle=st.stroke;g.lineWidth=st.width;
if(t==='Polygon'||t==='MultiPolygon'){{g.beginPath();(t==='Polygon'?[c]:c).forEach(p=>p.forEach(r=>{{path(r);g.closePath()}}));g.fill('evenodd');if(st.width>0)g.stroke()}}
else if(t==='LineString'||t==='MultiLineString'){{g.beginPath();(t==='LineString'?[c]:c).forEach(path);g.strokeStyle=st.fill;g.lineWidth=Math.max(1,st.width||1.5);g.stroke()}}
else if(t==='Point'||t==='MultiPoint'){{(t==='Point'?[c]:c).forEach(p=>{{const [x,y]=merc(p[0],p[1]);g.beginPath();g.arc(sx(x),sy(y),Math.max(2,st.size/2),0,7);g.fill();if(st.width>0)g.stroke()}})}}}}
function draw(){{cv.width=innerWidth;cv.height=innerHeight;g.fillStyle='{bg}';g.fillRect(0,0,cv.width,cv.height);
L.forEach((l,k)=>{{if(!on[k])return;if(l.image){{const i=imgs[k];if(i&&i.complete){{const b=l.bounds;g.globalAlpha=1;g.drawImage(i,sx(b[0]),sy(b[3]),(b[2]-b[0])/res,(b[3]-b[1])/res)}}return}}l.features.forEach(f=>geom(f.geometry,f.style))}});g.globalAlpha=1}}
let drag=null;cv.onmousedown=e=>drag=[e.clientX,e.clientY,false];onmouseup=e=>{{if(drag&&!drag[2])pick(e);drag=null}};
onmousemove=e=>{{if(!drag)return;const dx=e.clientX-drag[0],dy=e.clientY-drag[1];if(Math.abs(dx)+Math.abs(dy)>2)drag[2]=true;cx-=dx*res;cy+=dy*res;drag[0]=e.clientX;drag[1]=e.clientY;draw()}};
cv.onwheel=e=>{{e.preventDefault();const k=e.deltaY<0?0.8:1.25,mx=(e.clientX-cv.width/2)*res+cx,my=cy-(e.clientY-cv.height/2)*res;res*=k;cx=mx-(e.clientX-cv.width/2)*res;cy=my+(e.clientY-cv.height/2)*res;draw()}};
function inside(pt,r){{let c=false;for(let i=0,j=r.length-1;i<r.length;j=i++){{const a=merc(...r[i]),b=merc(...r[j]);if((a[1]>pt[1])!==(b[1]>pt[1])&&pt[0]<(b[0]-a[0])*(pt[1]-a[1])/(b[1]-a[1])+a[0])c=!c}}return c}}
function pick(e){{const pt=[(e.clientX-cv.width/2)*res+cx,cy-(e.clientY-cv.height/2)*res],pop=document.getElementById('pop');
for(let k=L.length-1;k>=0;k--){{if(!on[k]||!L[k].features)continue;for(const f of L[k].features){{const G=f.geometry,t=G.type,c=G.coordinates;let hit=false;
if(t==='Polygon')hit=inside(pt,c[0]);else if(t==='MultiPolygon')hit=c.some(p=>inside(pt,p[0]));else if(t==='Point'){{const q=merc(c[0],c[1]);hit=Math.hypot(q[0]-pt[0],q[1]-pt[1])<8*res}}
if(hit){{pop.innerHTML='<b>'+esc(L[k].name)+'</b><br>'+Object.entries(f.properties).map(([a,b])=>esc(a)+': '+esc(b)).join('<br>');pop.style.left=e.clientX+8+'px';pop.style.top=e.clientY+8+'px';pop.style.display='block';return}}}}}}pop.style.display='none'}}
onresize=draw;draw();
</script></body></html>
"#,
        title = html_escape(title),
        layers = script_json(&Value::Array(layers)),
        lon = if c.x.is_finite() { c.x } else { 0.0 },
        lat = if c.y.is_finite() { c.y } else { 0.0 },
        res = if m.camera.res.is_finite() && m.camera.res > 0.0 { m.camera.res } else { 40_000.0 },
        bg = bg
    ))
}

/// Says, when a shared web map, web layer or OGC API Features item has layers that lost Z or M
/// values on import, or whose positions are approximate or aren't longitude/latitude, what
/// `target` lacks: one notice per kind, counting them ([`rustgis_data::export_notes`]). Run once
/// the item is made, as [`note_label_classes`] is.
pub(crate) fn note_export(s: &mut Session, kind: &str, p: &Value, target: &str) {
    // Shared items are GeoJSON, longitude/latitude.
    let notes = match kind {
        "webLayer" => s
            .layer_id(p)
            .ok()
            .and_then(|id| s.map().layer(id))
            .map(|l| rustgis_data::export_notes([(&l.dataset, l.extent())], target, rustgis_data::raster::Crs::LonLat))
            .unwrap_or_default(),
        "webMap" | "geodataService" => map_of(s, p)
            .ok()
            .map(|m| {
                let shared = |l: &&Layer| if kind == "webMap" { l.visible } else { !l.is_raster() && !l.graphics };
                rustgis_data::export_notes(
                    m.layers.iter().filter(shared).map(|l| (&l.dataset, l.extent())),
                    target,
                    rustgis_data::raster::Crs::LonLat,
                )
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    for note in notes {
        s.notify(note, false);
    }
}

/// Says what a web layer or style (`kind`) leaves out of the labels: it labels every feature with
/// the Default class's field and text symbol, since no SQL clause has a MapLibre filter written
/// for it, and has no labels when Default is hidden (though the layer's labels are on). Run once
/// the share is made, so its own notice comes after the one saying it's made.
pub(crate) fn note_label_classes(s: &mut Session, kind: &str, p: &Value) {
    if !matches!(kind, "webLayer" | "webStyle") {
        return;
    }
    // Unique Values by expression: the style draws every feature with the other values' symbol.
    let classes = s.layer_id(p).ok().and_then(|id| s.map().layer(id)).filter(|l| l.renderer.expression().is_some()).map(|l| {
        let which = match &l.renderer {
            Renderer::Unique { other, .. } if other.draws(l.geometry_type()) => "its other values'",
            _ => "its first class's",
        };
        format!("{}'s style draws every feature with one symbol ({which}): its classes by expression aren't in it.", l.name)
    });
    if let Some(text) = classes {
        s.notify(text, false);
    }
    let Some(l) = s.layer_id(p).ok().and_then(|id| s.map().layer(id)).filter(|l| l.labels.enabled) else { return };
    let n = l.labels.classes.len().min(MAX_LABEL_CLASSES);
    let clause = l.labels.default.where_.as_deref().is_some_and(|w| !w.trim().is_empty());
    // A Default class without a field has no labels in the style: by a text expression alone, or
    // by nothing.
    let no_field = l.labels.default.field.is_empty();
    let text = if !l.labels.default.visible {
        Some(match n {
            0 => format!("{}'s style has no labels: its Default class is hidden.", l.name),
            _ => format!("{}'s style has no labels: its Default class is hidden, and its other label classes aren't in it.", l.name),
        })
    } else if no_field {
        Some(match l.labels.default.by.text {
            Some(_) => format!("{}'s style has no labels: its Default class's text is an expression, which isn't in it.", l.name),
            None => format!("{}'s style has no labels: its Default class has no label field.", l.name),
        })
    } else {
        (n > 0 || clause).then(|| {
            format!("{}'s style labels every feature with its Default class; its label classes' SQL clauses aren't in it.", l.name)
        })
    };
    // Its expressions have no MapLibre equivalent written for them.
    let by = (l.labels.default.visible && !no_field && !l.labels.default.by.is_empty()).then(|| {
        format!(
            "{}'s style labels with the Default class's field and text symbol ({}); its label expressions aren't in it.",
            l.name, l.labels.default.field
        )
    });
    for text in text.into_iter().chain(by) {
        s.notify(text, false);
    }
}

/// The one symbol a web style draws Unique Values by expression with: the rest's, or where that
/// draws nothing (a rule-based style without an else rule), the first class's. A line draws its
/// stroke; an area or point its fill, or an outline with a width.
fn one_symbol(r: &Renderer, g: GeometryType) -> rustgis_map::Symbol {
    match r {
        Renderer::Unique { other, .. } if other.draws(g) => *other,
        r => r.base_symbol(),
    }
}

/// A MapLibre (version 8) style for a layer, its symbology as expressions over a GeoJSON source.
/// Its labels are the Default class's, none when it is hidden (see `note_label_classes`).
pub fn web_style(l: &Layer) -> Value {
    let src = safe(&l.name);
    let gt = l.geometry_type();
    let prop = |field: &str| json!(["get", field]);
    // A colour (fill or stroke) for the layer's renderer, as a constant or an expression.
    let color = |pick: fn(&rustgis_map::Symbol) -> Rgba| -> Value {
        match &l.renderer {
            Renderer::Single(sym) => json!(pick(sym).hex()),
            // Classes by expression have no MapLibre expression written for them: one symbol, the
            // rest's, or the first class's where the rest isn't drawn (noted by
            // `note_label_classes`).
            Renderer::Unique { expression: Some(_), .. } => json!(pick(&one_symbol(&l.renderer, l.geometry_type())).hex()),
            Renderer::Unique { field, classes, other, .. } => {
                // MapLibre rejects repeated labels: the first class of a value wins.
                let mut e = vec![json!("match"), json!(["to-string", prop(field)])];
                let mut seen = std::collections::HashSet::new();
                for (v, sym) in classes {
                    if seen.insert(v.as_str()) {
                        e.push(json!(v));
                        e.push(json!(pick(sym).hex()));
                    }
                }
                e.push(json!(pick(other).hex()));
                if seen.is_empty() { json!(pick(other).hex()) } else { Value::Array(e) }
            }
            Renderer::Graduated { field, breaks, symbols, .. } => {
                let first = symbols.first().map_or_else(|| "#888888".to_string(), |s| pick(s).hex());
                let mut e = vec![json!("step"), json!(["to-number", prop(field)]), json!(first)];
                // Stops must ascend strictly.
                let mut last = f64::NEG_INFINITY;
                for (b, sym) in breaks.iter().zip(symbols.iter().skip(1)) {
                    if b.is_finite() && *b > last {
                        last = *b;
                        e.push(json!(b));
                        e.push(json!(pick(sym).hex()));
                    }
                }
                if e.len() == 3 { json!(first) } else { Value::Array(e) }
            }
            Renderer::Proportional { base, .. } => json!(pick(base).hex()),
        }
    };
    let opacity = 1.0 - f64::from(l.transparency.clamp(0.0, 100.0)) / 100.0;
    let width = match &l.renderer {
        Renderer::Single(s) | Renderer::Proportional { base: s, .. } => f64::from(s.stroke_width),
        Renderer::Unique { expression: Some(_), .. } => f64::from(one_symbol(&l.renderer, l.geometry_type()).stroke_width),
        Renderer::Unique { other, .. } => f64::from(other.stroke_width),
        Renderer::Graduated { symbols, .. } => symbols.first().map_or(1.0, |s| f64::from(s.stroke_width)),
    };
    let mut layers = Vec::new();
    match gt {
        GeometryType::Polygon => {
            layers.push(json!({"id": format!("{src}-fill"), "type": "fill", "source": src, "paint": {"fill-color": color(|s| s.fill), "fill-opacity": opacity}}));
            layers.push(json!({"id": format!("{src}-outline"), "type": "line", "source": src, "paint": {"line-color": color(|s| s.stroke), "line-width": width}}));
        }
        GeometryType::Polyline => {
            layers.push(json!({"id": format!("{src}-line"), "type": "line", "source": src, "paint": {"line-color": color(|s| s.stroke), "line-width": width.max(1.0), "line-opacity": opacity}}))
        }
        _ => {
            let radius = match &l.renderer {
                // A constant field (no range to interpolate over) gets one size.
                Renderer::Proportional { field, min_size, max_size, min, max, .. } if min.is_finite() && max.is_finite() && min < max => {
                    json!(["interpolate", ["linear"], ["to-number", prop(field)], min, f64::from(*min_size) / 2.0, max, f64::from(*max_size) / 2.0])
                }
                Renderer::Proportional { min_size, max_size, .. } => json!(f64::from(*min_size + *max_size) / 4.0),
                Renderer::Single(s) => json!(f64::from(s.size) / 2.0),
                Renderer::Unique { expression: Some(_), .. } => json!(f64::from(one_symbol(&l.renderer, l.geometry_type()).size) / 2.0),
                Renderer::Unique { other, .. } => json!(f64::from(other.size) / 2.0),
                Renderer::Graduated { symbols, .. } => json!(symbols.first().map_or(4.0, |s| f64::from(s.size) / 2.0)),
            };
            layers.push(json!({"id": format!("{src}-circle"), "type": "circle", "source": src, "paint": {
                "circle-color": color(|s| s.fill), "circle-radius": radius, "circle-stroke-color": color(|s| s.stroke), "circle-stroke-width": width, "circle-opacity": opacity}}));
        }
    }
    if l.labels.enabled && l.labels.default.visible && !l.labels.default.field.is_empty() {
        let c = &l.labels.default;
        // A halo of the class's own colour and width (points, as pixels: as the text's size), else
        // the one the map draws (light behind dark text, dark behind light) and thin.
        let halo_color = css(c.halo_color_for(c.color));
        let halo_width = if c.halo { c.halo_width.map_or(1.5, f64::from) } else { 0.0 };
        let mut layout = json!({"text-field": ["to-string", prop(&c.field)], "text-size": c.size});
        // Letter spacing in ems, as MapLibre has it.
        if c.spacing != 0.0 && c.size > 0.0 {
            layout["text-letter-spacing"] = json!(f64::from(c.spacing) / f64::from(c.size));
        }
        // Its offset in ems too, unless along a line, which the map doesn't move (a background
        // has no MapLibre equivalent: left out).
        if c.offset != [0.0; 2] && c.size > 0.0 && !(c.placement.along_line() && gt == GeometryType::Polyline) {
            layout["text-offset"] = json!(c.offset.map(|o| f64::from(o) / f64::from(c.size)));
        }
        layers.push(json!({"id": format!("{src}-labels"), "type": "symbol", "source": src, "layout": layout,
            "paint": {"text-color": c.color.hex(), "text-halo-color": halo_color, "text-halo-width": halo_width}}));
    }
    let (minzoom, maxzoom) = zoom_range(l);
    for layer in &mut layers {
        if let Some(z) = minzoom {
            layer["minzoom"] = json!(z);
        }
        if let Some(z) = maxzoom {
            layer["maxzoom"] = json!(z);
        }
    }
    json!({"version": 8, "name": l.name, "sources": {src.clone(): {"type": "geojson", "data": format!("{src}.geojson")}}, "layers": layers})
}

/// The MapLibre zooms (`minzoom`, `maxzoom`) matching a layer's scale limits. A scale is a ground
/// distance per pixel, so the zoom depends on latitude: the layer's centre is used.
fn zoom_range(l: &Layer) -> (Option<f32>, Option<f32>) {
    let e = l.extent();
    let lat = if e.is_empty() { 0.0 } else { e.center().y.clamp(-rustgis_geom::MAX_LAT, rustgis_geom::MAX_LAT) };
    let cos = lat.to_radians().cos().max(1e-6);
    let zoom = |scale: Option<f64>| {
        scale
            .filter(|s| s.is_finite() && *s > 0.0)
            .map(|s| crate::render::rustgis_tiles::template::style_zoom(s * rustgis_geom::PIXEL_M / cos))
    };
    (zoom(l.min_scale), zoom(l.max_scale))
}

/// Static OGC API – Features collections (landing page, conformance, collections and their
/// items), plus extra files.
fn ogc_zip(title: &str, layers: &[&Dataset], extra: &[(String, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut cols = Vec::new();
    let mut used: HashMap<String, usize> = HashMap::new();
    let mut taken = std::collections::HashSet::new();
    for ds in layers {
        let base = safe(&ds.name).to_ascii_lowercase();
        // The first free id: "a", then "a_2", "a_3", … skipping ids another layer already took.
        let n = used.entry(base.clone()).or_insert(0);
        let mut id = base.clone();
        while taken.contains(&id) {
            *n += 1;
            id = format!("{base}_{}", *n + 1);
        }
        taken.insert(id.clone());
        let e = ds.extent();
        let mut col = json!({
            "id": id, "title": ds.name, "itemType": "feature",
            "links": [{"href": format!("collections/{id}/items.json"), "rel": "items", "type": "application/geo+json"}],
        });
        if !e.is_empty() {
            col["extent"] =
                json!({"spatial": {"bbox": [[e.min_x, e.min_y, e.max_x, e.max_y]], "crs": "http://www.opengis.net/def/crs/OGC/1.3/CRS84"}});
        }
        entries.push((format!("collections/{id}.json"), serde_json::to_vec_pretty(&col).map_err(|e| e.to_string())?));
        let mut fc = rustgis_data::io::to_geojson(ds);
        fc["numberReturned"] = json!(ds.features.len());
        entries.push((format!("collections/{id}/items.json"), serde_json::to_vec(&fc).map_err(|e| e.to_string())?));
        cols.push(col);
    }
    let landing = json!({"title": title, "links": [
        {"href": "conformance.json", "rel": "conformance"}, {"href": "collections.json", "rel": "data"}]});
    let conformance = json!({"conformsTo": ["http://www.opengis.net/spec/ogcapi-features-1/1.0/conf/core", "http://www.opengis.net/spec/ogcapi-features-1/1.0/conf/geojson"]});
    entries.push(("index.json".into(), serde_json::to_vec_pretty(&landing).map_err(|e| e.to_string())?));
    entries.push(("conformance.json".into(), serde_json::to_vec_pretty(&conformance).map_err(|e| e.to_string())?));
    entries.push(("collections.json".into(), serde_json::to_vec_pretty(&json!({"collections": cols})).map_err(|e| e.to_string())?));
    entries.extend(extra.iter().cloned());
    rustgis_data::zip::write(&entries)
}

/// A web layer: the layer as an OGC API Features collection, its style, and a viewer page.
fn web_layer_zip(s: &Session, l: &Layer) -> Result<Vec<u8>, String> {
    let style = serde_json::to_vec_pretty(&web_style(l)).map_err(|e| e.to_string())?;
    let geojson = serde_json::to_vec(&rustgis_data::io::to_geojson(&l.dataset)).map_err(|e| e.to_string())?;
    let mut m = s.map().clone();
    m.layers = vec![l.clone()];
    let e = l.extent();
    if !e.is_empty() {
        // Centred on the layer, at the scale that shows all of it in a typical window.
        let (a, b) =
            (rustgis_geom::lonlat_to_merc(Coord::new(e.min_x, e.min_y)), rustgis_geom::lonlat_to_merc(Coord::new(e.max_x, e.max_y)));
        m.camera.center = Coord::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        m.camera.res = ((b.x - a.x).abs() / 1000.0).max((b.y - a.y).abs() / 650.0).max(1.0) * 1.1;
        m.camera.clamp();
    }
    let html = web_map_html(&m, &l.name)?;
    let src = safe(&l.name);
    ogc_zip(
        &l.name,
        &[&l.dataset],
        &[("style.json".into(), style), (format!("{src}.geojson"), geojson), ("index.html".into(), html.into_bytes())],
    )
}

/// A locator: searchable place names and where they are.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Locator {
    pub name: String,
    pub fields: Vec<String>,
    /// (text, longitude, latitude).
    pub entries: Vec<(String, f64, f64)>,
}

fn locator(l: &Layer, fields: Option<&[String]>) -> Result<Locator, String> {
    let ds = &l.dataset;
    let fis: Vec<usize> = match fields {
        Some(fs) => {
            fs.iter().map(|f| ds.field_index(f).ok_or_else(|| format!("'{}' has no field '{f}'", l.name))).collect::<Result<_, _>>()?
        }
        None => ds.fields.iter().enumerate().filter(|(_, f)| !f.ty.is_numeric()).map(|(i, _)| i).take(4).collect(),
    };
    if fis.is_empty() {
        return Err("pick the fields to search (the layer has no text fields)".into());
    }
    let mut entries = Vec::new();
    for (row, ft) in ds.features.iter().enumerate().take(MAX_FEATURES) {
        let Some(g) = &ft.geometry else { continue };
        let e = g.extent();
        if e.is_empty() {
            continue;
        }
        let c = match g {
            Geometry::Point(p) => *p,
            _ => e.center(),
        };
        for &fi in &fis {
            let t = match ds.value(row, fi) {
                rustgis_data::Value::Null => continue,
                v => v.to_string(),
            };
            if !t.trim().is_empty() {
                entries.push((t.trim().chars().take(500).collect(), c.x, c.y));
            }
        }
    }
    Ok(Locator { name: l.name.clone(), fields: fis.iter().filter_map(|i| ds.fields.get(*i)).map(|f| f.name.clone()).collect(), entries })
}

fn find(s: &mut Session, p: &Value) -> CmdResult {
    let text = ps(p, "text").map(str::trim).filter(|t| !t.is_empty()).ok_or("text is required")?.to_lowercase();
    let path = match ps(p, "locator") {
        Some(n) => s
            .project
            .shared
            .iter()
            .rev()
            .find(|i| i.kind == "locator" && i.name.eq_ignore_ascii_case(n))
            .map_or_else(|| n.to_string(), |i| i.path.clone()),
        None => s
            .project
            .shared
            .iter()
            .rev()
            .find(|i| i.kind == "locator")
            .map(|i| i.path.clone())
            .ok_or("share a locator first (Share As › Locator)")?,
    };
    let body = super::items::read_text(&json!({"path": path}))?;
    let loc: Locator = serde_json::from_str(&body).map_err(|e| format!("not a RustGIS locator: {e}"))?;
    let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(10).clamp(1, 100) as usize;
    // Exact, then prefix, then word prefix, then anywhere.
    let mut hits: Vec<(u8, &(String, f64, f64))> = loc
        .entries
        .iter()
        .filter_map(|e| {
            let t = e.0.to_lowercase();
            let score = if t == text {
                0
            } else if t.starts_with(&text) {
                1
            } else if t.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&text)) {
                2
            } else if t.contains(&text) {
                3
            } else {
                return None;
            };
            Some((score, e))
        })
        .collect();
    hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.0.len().cmp(&b.1.0.len())));
    hits.truncate(limit);
    let out: Vec<Value> =
        hits.iter().map(|(sc, e)| json!({"text": e.0, "lon": e.1, "lat": e.2, "score": 100 - u32::from(*sc) * 25})).collect();
    if super::b(p, "zoom").unwrap_or(false)
        && let Some((_, e)) = hits.first()
        && e.1.is_finite()
        && e.2.is_finite()
    {
        let (lon, lat) = (e.1, e.2);
        super::navigate::go_to_xy(s, &json!({"lon": lon, "lat": lat}))?;
    }
    Ok(json!({"matches": out}))
}

/// A glTF 2.0 scene (embedded buffer): polygon layers extruded by their extrusion setting (flat
/// otherwise), lines and points, coloured by their symbology. Metres east (x), up (y) and south
/// (z) of the map's centre, with a camera where the map view (`view` points across) looks
/// from: its bearing and pitch, as far off as MapLibre's camera would be.
fn gltf(m: &Map, view: (f64, f64)) -> Result<Vec<u8>, String> {
    let c = m.center_lonlat();
    let origin = rustgis_geom::lonlat_to_merc(c);
    let k = c.y.to_radians().cos().abs().max(0.01);
    let xy = |p: Coord| {
        let q = rustgis_geom::lonlat_to_merc(p);
        (((q.x - origin.x) * k) as f32, (-(q.y - origin.y) * k) as f32)
    };
    let mut buffer: Vec<u8> = Vec::new();
    let (mut views, mut accessors, mut meshes, mut nodes, mut materials) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut budget = MAX_FEATURES;
    for l in m.layers.iter().rev().filter(|l| l.visible && !l.is_raster()) {
        // Per colour, its positions and (for triangles) indices.
        // Ordered, so the same scene always writes the same file.
        let mut tri: std::collections::BTreeMap<[u8; 4], (Vec<[f32; 3]>, Vec<u32>)> = Default::default();
        let mut lines: std::collections::BTreeMap<[u8; 4], Vec<[f32; 3]>> = Default::default();
        let ex = l.extrusion.as_ref();
        let hf = ex.and_then(|e| e.field.as_deref()).and_then(|f| l.dataset.field_index(f));
        let (base_h, factor) = ex.map_or((0.0, 0.0), |e| (e.height, e.factor));
        let rows = l.visible_rows();
        let styler = l.styler(Some(m.scale()));
        // The bytes built so far: a feature only adds to its own colour, so each step adds what
        // the last feature's colour grew by.
        let size = |tri: &std::collections::BTreeMap<[u8; 4], (Vec<[f32; 3]>, Vec<u32>)>,
                    lines: &std::collections::BTreeMap<[u8; 4], Vec<[f32; 3]>>,
                    k: &[u8; 4]| {
            tri.get(k).map_or(0, |(p, i)| p.len() * 12 + i.len() * 4) + lines.get(k).map_or(0, |v| v.len() * 12)
        };
        let (mut built, mut last): (usize, Option<([u8; 4], usize)>) = (0, None);
        for (row, ft) in l.dataset.features.iter().enumerate() {
            if let Some((k, before)) = last.take() {
                built += size(&tri, &lines, &k).saturating_sub(before);
            }
            if budget == 0 || rows.get(row) != Some(&true) {
                continue;
            }
            let Some(g) = &ft.geometry else { continue };
            budget -= 1;
            // The geometry built so far, with the buffer already written, stays under the cap
            // (checked as it grows, so a few huge polygons can't run away first).
            if buffer.len() + built > MAX_SCENE_BYTES {
                return Err("the scene would be too large; share fewer features".into());
            }
            let sym = styler.symbol(row);
            let h = (hf.and_then(|fi| l.dataset.value(row, fi).as_f64()).filter(|v| v.is_finite()).unwrap_or(base_h) * factor)
                .clamp(0.0, 1e6) as f32;
            let key = [sym.fill.r(), sym.fill.g(), sym.fill.b(), sym.fill.a()];
            last = Some((key, size(&tri, &lines, &key)));
            match g {
                Geometry::Polygon(polys) => {
                    for rings in polys {
                        // Rings without their closing point (when they have one).
                        let open: Vec<Vec<(f64, f64)>> = rings
                            .iter()
                            .map(|r| {
                                let closed = r.len() > 1 && r.first() == r.last();
                                r.iter()
                                    .take(if closed { r.len() - 1 } else { r.len() })
                                    .map(|c| xy(*c))
                                    .map(|(x, z)| (f64::from(x), f64::from(z)))
                                    .collect::<Vec<_>>()
                            })
                            .collect();
                        // A polygon whose outer ring is degenerate is skipped (a hole never
                        // stands in for it); degenerate holes are dropped.
                        let Some(outer) = open.first().filter(|r| r.len() >= 3) else { continue };
                        let open: Vec<Vec<(f64, f64)>> =
                            std::iter::once(outer.clone()).chain(open.iter().skip(1).filter(|r| r.len() >= 3).cloned()).collect();
                        let Some(outer) = open.first() else { continue };
                        let Some(tris) = rustgis_geom::triangulate::triangulate(outer, open.get(1..).unwrap_or(&[])) else { continue };
                        let (pos, idx) = tri.entry(key).or_default();
                        let flat: Vec<(f64, f64)> = open.iter().flatten().copied().collect();
                        let base = pos.len() as u32;
                        for (x, z) in &flat {
                            pos.push([*x as f32, h, *z as f32]);
                        }
                        for t in &tris {
                            // Seen from above (+y), with z pointing south: reverse the winding.
                            idx.extend([base + t[0] as u32, base + t[2] as u32, base + t[1] as u32]);
                        }
                        if h > 0.0 {
                            let mut first = 0usize;
                            for r in &open {
                                let n = r.len();
                                for i in 0..n {
                                    let (a, b) = (flat.get(first + i).copied(), flat.get(first + (i + 1) % n).copied());
                                    let (Some(a), Some(b)) = (a, b) else { continue };
                                    let w = pos.len() as u32;
                                    for (p, y) in [(a, 0.0), (b, 0.0), (b, h), (a, h)] {
                                        pos.push([p.0 as f32, y, p.1 as f32]);
                                    }
                                    idx.extend([w, w + 1, w + 2, w, w + 2, w + 3]);
                                }
                                first += n;
                            }
                        }
                    }
                }
                Geometry::Line(parts) => {
                    let seg = lines.entry(key).or_default();
                    for part in parts {
                        for w in part.windows(2) {
                            let ((ax, az), (bx, bz)) = (xy(w[0]), xy(w[1]));
                            seg.push([ax, h, az]);
                            seg.push([bx, h, bz]);
                        }
                    }
                }
                Geometry::Point(p) => {
                    let (x, z) = xy(*p);
                    let seg = lines.entry(key).or_default();
                    // A vertical stick, as tall as the feature (or 50 m).
                    seg.push([x, 0.0, z]);
                    seg.push([x, h.max(50.0), z]);
                }
                Geometry::MultiPoint(ps_) => {
                    let seg = lines.entry(key).or_default();
                    for p in ps_ {
                        let (x, z) = xy(*p);
                        seg.push([x, 0.0, z]);
                        seg.push([x, h.max(50.0), z]);
                    }
                }
            }
        }
        let mut primitives = Vec::new();
        let material = |key: [u8; 4], materials: &mut Vec<Value>| -> usize {
            materials.push(json!({"pbrMetallicRoughness": {"baseColorFactor": key.map(|v| f64::from(v) / 255.0), "metallicFactor": 0.0, "roughnessFactor": 0.9}, "doubleSided": true}));
            materials.len() - 1
        };
        let push_positions = |pos: &[[f32; 3]], buffer: &mut Vec<u8>, views: &mut Vec<Value>, accessors: &mut Vec<Value>| -> usize {
            let off = buffer.len();
            let (mut mn, mut mx) = ([f32::MAX; 3], [f32::MIN; 3]);
            for p in pos {
                for i in 0..3 {
                    mn[i] = mn[i].min(p[i]);
                    mx[i] = mx[i].max(p[i]);
                    buffer.extend_from_slice(&p[i].to_le_bytes());
                }
            }
            views.push(json!({"buffer": 0, "byteOffset": off, "byteLength": pos.len() * 12, "target": 34962}));
            accessors.push(
                json!({"bufferView": views.len() - 1, "componentType": 5126, "count": pos.len(), "type": "VEC3", "min": mn, "max": mx}),
            );
            accessors.len() - 1
        };
        for (key, (pos, idx)) in tri {
            if pos.is_empty() || idx.is_empty() {
                continue;
            }
            let pa = push_positions(&pos, &mut buffer, &mut views, &mut accessors);
            let off = buffer.len();
            for i in &idx {
                buffer.extend_from_slice(&i.to_le_bytes());
            }
            views.push(json!({"buffer": 0, "byteOffset": off, "byteLength": idx.len() * 4, "target": 34963}));
            accessors.push(json!({"bufferView": views.len() - 1, "componentType": 5125, "count": idx.len(), "type": "SCALAR"}));
            let mat = material(key, &mut materials);
            primitives.push(json!({"attributes": {"POSITION": pa}, "indices": accessors.len() - 1, "material": mat, "mode": 4}));
        }
        for (key, seg) in lines {
            if seg.len() < 2 {
                continue;
            }
            let pa = push_positions(&seg, &mut buffer, &mut views, &mut accessors);
            let mat = material(key, &mut materials);
            primitives.push(json!({"attributes": {"POSITION": pa}, "material": mat, "mode": 1}));
        }
        if !primitives.is_empty() {
            meshes.push(json!({"name": l.name, "primitives": primitives}));
            nodes.push(json!({"name": l.name, "mesh": meshes.len() - 1}));
        }
        if buffer.len() > MAX_SCENE_BYTES {
            return Err("the scene would be too large; share fewer features".into());
        }
    }
    if nodes.is_empty() {
        return Err("the map has no features to put in a scene".into());
    }
    // As drawn: a camera set from outside a command may not be clamped yet.
    let mut at = m.camera;
    at.clamp();
    let (camera, node) = viewpoint(m, &at, view);
    nodes.push(node);
    let n = nodes.len();
    let doc = json!({
        "asset": {"version": "2.0", "generator": "RustGIS"},
        "scene": 0,
        "scenes": [{"name": m.name, "nodes": (0..n).collect::<Vec<_>>()}],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{"byteLength": buffer.len(), "uri": format!("data:application/octet-stream;base64,{}", base64::engine::general_purpose::STANDARD.encode(&buffer))}],
        "cameras": [camera],
        "extras": {"origin": [c.x, c.y], "units": "metres east, up and south of origin (longitude, latitude)",
            "bearing": at.bearing, "pitch": at.pitch},
    });
    serde_json::to_vec(&doc).map_err(|e| e.to_string())
}

/// The glTF camera of a scene and the node placing it: looking at the map's centre (the scene's
/// origin) from camera `at` (clamped), with MapLibre's field of view, at the distance that shows
/// the map's scale across a view `view` points in size.
fn viewpoint(m: &Map, at: &rustgis_map::Camera, view: (f64, f64)) -> (Value, Value) {
    let size = |v: f64| if v.is_finite() { v.clamp(1.0, 1e6) } else { 1.0 };
    let (w, h) = (size(view.0), size(view.1));
    let ground = at.res * m.ground_per_unit(at.center);
    let distance = h / 2.0 / rustgis_map::camera::TAN_HALF_FOV * ground;
    let distance = if distance.is_finite() { distance.clamp(1.0, 1e9) } else { 1e6 };
    let (bearing, pitch) = (at.bearing.to_radians(), at.pitch.to_radians());
    // Backed off from the centre against the heading, and raised.
    let (heading_x, heading_z) = (bearing.sin(), -bearing.cos());
    let position = [-heading_x * distance * pitch.sin(), distance * pitch.cos(), -heading_z * distance * pitch.sin()];
    // glTF cameras look down -z with +y up: tipped about x from level (looking north) to the
    // pitch, then turned about y to the bearing.
    let (tip, turn) = ((pitch - std::f64::consts::FRAC_PI_2) / 2.0, -bearing / 2.0);
    let (sx, cx, sy, cy) = (tip.sin(), tip.cos(), turn.sin(), turn.cos());
    let rotation = [cy * sx, cx * sy, -sx * sy, cy * cx];
    let camera = json!({"name": "View", "type": "perspective", "perspective": {
        "yfov": 2.0 * rustgis_map::camera::TAN_HALF_FOV.atan(),
        "aspectRatio": w / h,
        "znear": (distance / 1000.0).max(0.01),
    }});
    let node = json!({"name": "View", "camera": 0, "translation": position, "rotation": rotation});
    (camera, node)
}

/// A link chart: the features joined by the map's relationships, as a node-link diagram (a
/// self-contained page with a force layout; drag nodes, hover for names).
fn link_chart(s: &Session, rel: Option<&str>) -> Result<String, String> {
    let rels: Vec<&rustgis_map::design::Relationship> = s
        .project
        .maps
        .iter()
        .flat_map(|m| m.editing.relationships.iter())
        .filter(|r| rel.is_none_or(|n| r.name.eq_ignore_ascii_case(n)))
        .collect();
    if rels.is_empty() {
        return Err(match rel {
            Some(n) => format!("no relationship '{n}'"),
            None => "the project has no relationships (Data Design › Relationships)".into(),
        });
    }
    // A layer by name (the active map first) and the map it's in.
    let active = s.project.active_map;
    let find = |n: &str| {
        let in_map = |mi: usize| s.project.maps.get(mi).and_then(|m| m.layer_by_name(n)).map(|l| (mi, l));
        in_map(active).or_else(|| (0..s.project.maps.len()).find_map(in_map))
    };
    let mut nodes: Vec<Value> = Vec::new();
    // Nodes by map, layer id and row (same-named layers in different maps stay apart).
    let mut index: HashMap<(usize, u32, usize), usize> = HashMap::new();
    let mut links: Vec<Value> = Vec::new();
    let label = |l: &Layer, row: usize| -> String {
        let ds = &l.dataset;
        let fi = ds
            .fields
            .iter()
            .position(|f| f.name.eq_ignore_ascii_case("name"))
            .or_else(|| ds.fields.iter().position(|f| !f.ty.is_numeric()));
        fi.map(|i| ds.value(row, i).to_string()).unwrap_or_else(|| format!("{} {}", l.name, row + 1))
    };
    'rels: for (gi, r) in rels.iter().enumerate() {
        if nodes.len() >= MAX_NODES {
            break;
        }
        let (Some((om, o)), Some((dm, d))) = (find(&r.origin), find(&r.destination)) else { continue };
        let (Some(ok), Some(dk)) = (o.dataset.field_index(&r.origin_key), d.dataset.field_index(&r.destination_key)) else { continue };
        let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
        // Null or empty keys relate nothing.
        let key_of = |ds: &rustgis_data::Dataset, row: usize, f: usize| -> Option<String> {
            let v = ds.value(row, f);
            if matches!(v, rustgis_data::Value::Null) {
                return None;
            }
            Some(v.to_string()).filter(|k| !k.is_empty())
        };
        for row in 0..d.dataset.features.len() {
            if let Some(v) = key_of(&d.dataset, row, dk) {
                by_key.entry(v).or_default().push(row);
            }
        }
        for orow in 0..o.dataset.features.len() {
            let Some(key) = key_of(&o.dataset, orow, ok) else { continue };
            let Some(drows) = by_key.get(&key) else { continue };
            if nodes.len() >= MAX_NODES {
                break 'rels;
            }
            let mut node = |mi: usize, l: &Layer, row: usize, nodes: &mut Vec<Value>| -> usize {
                *index.entry((mi, l.id, row)).or_insert_with(|| {
                    nodes.push(json!({"name": label(l, row), "layer": l.name, "group": gi}));
                    nodes.len() - 1
                })
            };
            let a = node(om, o, orow, &mut nodes);
            for &drow in drows.iter().take(200) {
                if nodes.len() >= MAX_NODES || links.len() >= MAX_LINKS {
                    break 'rels;
                }
                let b = node(dm, d, drow, &mut nodes);
                links.push(json!({"source": a, "target": b, "relationship": r.name}));
            }
        }
    }
    if links.is_empty() {
        return Err("no features are related through those relationships".into());
    }
    let title = rel.unwrap_or("Relationships");
    let mut legend = String::new();
    let layers: Vec<String> = {
        let mut v: Vec<String> = nodes.iter().filter_map(|n| n["layer"].as_str().map(str::to_string)).collect();
        v.sort();
        v.dedup();
        v
    };
    for (i, l) in layers.iter().enumerate() {
        let _ = write!(legend, "<span><i style=\"background:hsl({},60%,50%)\"></i>{}</span>", i * 137 % 360, html_escape(l));
    }
    Ok(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>{title}</title>
<style>html,body{{margin:0;height:100%;font-family:system-ui,sans-serif;background:#fafbfc}}svg{{width:100%;height:100%}}line{{stroke:#9aa5b1;stroke-opacity:.6}}circle{{stroke:#fff;stroke-width:1.5;cursor:grab}}
#k{{position:fixed;left:8px;top:8px;background:#fffe;padding:6px 10px;border-radius:6px;font-size:13px;box-shadow:0 1px 4px #0002}}#k span{{margin-right:10px}}#k i{{display:inline-block;width:10px;height:10px;border-radius:5px;margin-right:4px}}</style></head>
<body><div id="k"><b>{title}</b><br>{legend}</div><svg id="s"></svg><script>
const N={nodes},E={links},LY={layers};const s=document.getElementById('s'),ns='http://www.w3.org/2000/svg';
N.forEach((n,i)=>{{const r=30+Math.sqrt(i)*12;n.x=innerWidth/2+Math.cos(i*2.4)*r;n.y=innerHeight/2+Math.sin(i*2.4)*r;n.vx=0;n.vy=0}});
const ln=E.map(e=>{{const l=document.createElementNS(ns,'line');s.appendChild(l);return l}});
const deg=N.map(()=>0);E.forEach(e=>{{deg[e.source]++;deg[e.target]++}});
const cs=N.map((n,i)=>{{const c=document.createElementNS(ns,'circle');c.setAttribute('r',4+Math.min(10,Math.sqrt(deg[i])*2));c.setAttribute('fill','hsl('+(LY.indexOf(n.layer)*137%360)+',60%,50%)');
const t=document.createElementNS(ns,'title');t.textContent=n.name+' ('+n.layer+')';c.appendChild(t);s.appendChild(c);c.onmousedown=()=>{{drag=i;wake()}};return c}});
let drag=null,running=false;function wake(){{if(!running){{running=true;requestAnimationFrame(tick)}}}}onmouseup=()=>drag=null;onmousemove=e=>{{if(drag!==null){{N[drag].x=e.clientX;N[drag].y=e.clientY;N[drag].vx=N[drag].vy=0}}}};
function tick(){{const n=N.length,cx=innerWidth/2,cy=innerHeight/2;const k=Math.min(1,600/n);
for(let i=0;i<n;i++)for(let j=i+1;j<Math.min(n,i+200);j++){{const a=N[i],b=N[j];let dx=a.x-b.x,dy=a.y-b.y,d2=dx*dx+dy*dy+.01;const f=400*k/d2;a.vx+=dx*f;a.vy+=dy*f;b.vx-=dx*f;b.vy-=dy*f}}
E.forEach(e=>{{const a=N[e.source],b=N[e.target];const dx=b.x-a.x,dy=b.y-a.y,d=Math.sqrt(dx*dx+dy*dy)+.01,f=(d-50)*.03;a.vx+=dx/d*f;a.vy+=dy/d*f;b.vx-=dx/d*f;b.vy-=dy/d*f}});
N.forEach((a,i)=>{{if(i===drag)return;a.vx+=(cx-a.x)*.002;a.vy+=(cy-a.y)*.002;a.vx=Math.max(-15,Math.min(15,a.vx*.8));a.vy=Math.max(-15,Math.min(15,a.vy*.8));a.x+=a.vx;a.y+=a.vy}});
E.forEach((e,i)=>{{const l=ln[i];l.setAttribute('x1',N[e.source].x);l.setAttribute('y1',N[e.source].y);l.setAttribute('x2',N[e.target].x);l.setAttribute('y2',N[e.target].y)}});
cs.forEach((c,i)=>{{c.setAttribute('cx',N[i].x);c.setAttribute('cy',N[i].y)}});
// Settled (and not dragged): stop until a node is dragged again.
const moving=N.reduce((m,a)=>Math.max(m,Math.abs(a.vx),Math.abs(a.vy)),0);if(moving>0.05||drag!==null)requestAnimationFrame(tick);else running=false}}wake();
</script></body></html>
"#,
        title = html_escape(title),
        legend = legend,
        nodes = script_json(&Value::Array(nodes)),
        links = script_json(&Value::Array(links)),
        layers = script_json(&json!(layers)),
    ))
}
